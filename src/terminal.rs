use std::collections::VecDeque;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui::{
    self, text::LayoutJob, Color32, CursorIcon, Event, EventFilter, FontId, Key, PointerButton,
    Pos2, Rect, Stroke, TextFormat,
};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

const MAX_SCROLLBACK_LINES: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CellPos {
    row: u16,
    col: u16,
}

#[derive(Clone)]
struct SavedCell {
    text: String,
    fg: vt100::Color,
    bg: vt100::Color,
    inverse: bool,
    italic: bool,
    underline: bool,
}

struct TerminalState {
    parser: vt100::Parser,
    history: VecDeque<Vec<SavedCell>>,
}

impl TerminalState {
    fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
            history: VecDeque::with_capacity(256),
        }
    }

    fn process_bytes(&mut self, bytes: &[u8]) {
        for chunk in bytes.split_inclusive(|&b| b == b'\n') {
            if chunk.ends_with(b"\n") {
                let screen = self.parser.screen();
                let (rows, _) = screen.size();
                let (cur_row, _) = screen.cursor_position();

                if !screen.alternate_screen() && rows > 0 && cur_row + 1 >= rows {
                    let top_row = capture_screen_row(screen, 0);
                    if self.history.len() >= MAX_SCROLLBACK_LINES {
                        self.history.pop_front();
                    }
                    self.history.push_back(top_row);
                }
            }
            self.parser.process(chunk);
        }
    }

    fn cell_at(&self, visual_row: u16, col: u16, scrollback_offset: usize) -> Option<SavedCell> {
        let offset = scrollback_offset.min(self.history.len());
        let r = visual_row as usize;

        if r < offset {
            let hist_idx = self.history.len() - offset + r;
            self.history
                .get(hist_idx)
                .and_then(|row| row.get(col as usize))
                .cloned()
        } else {
            let screen_row = (r - offset) as u16;
            self.parser.screen().cell(screen_row, col).map(SavedCell::from_vt_cell)
        }
    }
}

impl SavedCell {
    fn from_vt_cell(cell: &vt100::Cell) -> Self {
        Self {
            text: cell.contents(),
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            inverse: cell.inverse(),
            italic: cell.italic(),
            underline: cell.underline(),
        }
    }
}

fn capture_screen_row(screen: &vt100::Screen, row: u16) -> Vec<SavedCell> {
    let (_, cols) = screen.size();
    let mut cells = Vec::with_capacity(cols as usize);
    for c in 0..cols {
        if let Some(cell) = screen.cell(row, c) {
            cells.push(SavedCell::from_vt_cell(cell));
        } else {
            cells.push(SavedCell {
                text: String::new(),
                fg: vt100::Color::Default,
                bg: vt100::Color::Default,
                inverse: false,
                italic: false,
                underline: false,
            });
        }
    }
    cells
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellProfile {
    #[cfg(target_os = "windows")]
    Cmd,
    #[cfg(target_os = "windows")]
    PowerShell,
    #[cfg(target_os = "windows")]
    Wsl,
    #[cfg(not(target_os = "windows"))]
    DefaultShell,
}

impl Default for ShellProfile {
    fn default() -> Self {
        #[cfg(target_os = "windows")]
        {
            ShellProfile::Cmd
        }
        #[cfg(not(target_os = "windows"))]
        {
            ShellProfile::DefaultShell
        }
    }
}

impl ShellProfile {
    pub fn all() -> &'static [ShellProfile] {
        #[cfg(target_os = "windows")]
        {
            &[ShellProfile::Cmd, ShellProfile::PowerShell, ShellProfile::Wsl]
        }
        #[cfg(not(target_os = "windows"))]
        {
            &[ShellProfile::DefaultShell]
        }
    }

    pub fn tab_label(self) -> &'static str {
        match self {
            #[cfg(target_os = "windows")]
            ShellProfile::Cmd => "cmd",
            #[cfg(target_os = "windows")]
            ShellProfile::PowerShell => "powershell",
            #[cfg(target_os = "windows")]
            ShellProfile::Wsl => "wsl",
            #[cfg(not(target_os = "windows"))]
            ShellProfile::DefaultShell => "sh",
        }
    }

    pub fn menu_label(self) -> &'static str {
        match self {
            #[cfg(target_os = "windows")]
            ShellProfile::Cmd => "Command Prompt (cmd)",
            #[cfg(target_os = "windows")]
            ShellProfile::PowerShell => "PowerShell",
            #[cfg(target_os = "windows")]
            ShellProfile::Wsl => "WSL (Linux)",
            #[cfg(not(target_os = "windows"))]
            ShellProfile::DefaultShell => "Default Shell",
        }
    }

    fn program(self) -> String {
        match self {
            #[cfg(target_os = "windows")]
            ShellProfile::Cmd => {
                std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
            }
            #[cfg(target_os = "windows")]
            ShellProfile::PowerShell => "powershell.exe".to_string(),
            #[cfg(target_os = "windows")]
            ShellProfile::Wsl => "wsl.exe".to_string(),
            #[cfg(not(target_os = "windows"))]
            ShellProfile::DefaultShell => {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
            }
        }
    }

    fn cd_command(self, dir: &Path) -> String {
        match self {
            #[cfg(target_os = "windows")]
            ShellProfile::Cmd => format!("cd /d \"{}\"\r", dir.display()),
            #[cfg(target_os = "windows")]
            ShellProfile::PowerShell => format!("Set-Location -LiteralPath \"{}\"\r", dir.display()),
            #[cfg(target_os = "windows")]
            ShellProfile::Wsl => format!("cd \"$(wslpath '{}')\"\r", dir.display()),
            #[cfg(not(target_os = "windows"))]
            ShellProfile::DefaultShell => format!("cd \"{}\"\n", dir.display()),
        }
    }

    fn clear_bytes(self) -> &'static [u8] {
        match self {
            #[cfg(target_os = "windows")]
            ShellProfile::Cmd | ShellProfile::PowerShell => b"cls\r",
            #[cfg(target_os = "windows")]
            ShellProfile::Wsl => b"\x0c",
            #[cfg(not(target_os = "windows"))]
            ShellProfile::DefaultShell => b"\x0c",
        }
    }
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: BufWriter<Box<dyn Write + Send>>,
    state: Arc<Mutex<TerminalState>>,
    exited: Arc<Mutex<bool>>,
    rows: u16,
    cols: u16,
}

impl PtySession {
    fn spawn(
        profile: ShellProfile,
        working_dir: &Path,
        rows: u16,
        cols: u16,
        ctx: egui::Context,
    ) -> Result<Self, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Nu s-a putut deschide PTY: {e}"))?;

        let mut cmd = CommandBuilder::new(profile.program());
        if working_dir.is_dir() {
            cmd.cwd(working_dir);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Nu s-a putut porni {}: {e}", profile.tab_label()))?;

        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Eroare reader PTY: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Eroare writer PTY: {e}"))?;

        let state = Arc::new(Mutex::new(TerminalState::new(rows, cols)));
        let exited = Arc::new(Mutex::new(false));

        let state_clone = Arc::clone(&state);
        let ctx_read = ctx.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut st) = state_clone.lock() {
                            st.process_bytes(&buf[..n]);
                        }
                        ctx_read.request_repaint();
                    }
                }
            }
        });

        let exited_clone = Arc::clone(&exited);
        thread::spawn(move || {
            let _ = child.wait();
            if let Ok(mut flag) = exited_clone.lock() {
                *flag = true;
            }
            ctx.request_repaint();
        });

        Ok(Self {
            master: pair.master,
            writer: BufWriter::new(writer),
            state,
            exited,
            rows,
            cols,
        })
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        if self.writer.write_all(bytes).is_ok() {
            let _ = self.writer.flush();
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) -> bool {
        if self.rows == rows && self.cols == cols {
            return false;
        }
        self.rows = rows;
        self.cols = cols;

        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });

        if let Ok(mut st) = self.state.lock() {
            st.parser.set_size(rows, cols);
        }
        true
    }

    fn is_exited(&self) -> bool {
        self.exited.lock().map(|g| *g).unwrap_or(false)
    }
}

struct TerminalTab {
    id: usize,
    profile: ShellProfile,
    session: Option<PtySession>,
    spawn_error: Option<String>,
    scrollback_offset: usize,
    selection: Option<(CellPos, CellPos)>,
}

impl TerminalTab {
    fn spawn(
        id: usize,
        profile: ShellProfile,
        working_dir: &Path,
        rows: u16,
        cols: u16,
        ctx: &egui::Context,
    ) -> Self {
        match PtySession::spawn(profile, working_dir, rows, cols, ctx.clone()) {
            Ok(session) => Self {
                id,
                profile,
                session: Some(session),
                spawn_error: None,
                scrollback_offset: 0,
                selection: None,
            },
            Err(err) => Self {
                id,
                profile,
                session: None,
                spawn_error: Some(err),
                scrollback_offset: 0,
                selection: None,
            },
        }
    }

    fn copy_selection_to_clipboard(&mut self, ctx: &egui::Context) -> bool {
        let Some((start, end)) = self.selection else {
            return false;
        };
        let Some(session) = &self.session else {
            return false;
        };

        let text = if let Ok(st) = session.state.lock() {
            extract_selected_text(&st, self.scrollback_offset, start, end)
        } else {
            return false;
        };

        if !text.is_empty() {
            ctx.copy_text(text);
            self.selection = None;
            true
        } else {
            false
        }
    }
}

pub struct TerminalPanel {
    pub working_dir: PathBuf,
    height: Option<f32>,
    tabs: Vec<TerminalTab>,
    active_tab: usize,
    next_tab_id: usize,
    focus_next_frame: bool,
}

impl Default for TerminalPanel {
    fn default() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            working_dir,
            height: None,
            tabs: Vec::new(),
            active_tab: 0,
            next_tab_id: 1,
            focus_next_frame: true,
        }
    }
}

impl TerminalPanel {
    pub fn set_working_dir(&mut self, dir: PathBuf) {
        if !dir.is_dir() {
            return;
        }
        self.working_dir = dir.clone();

        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            let cmd = tab.profile.cd_command(&dir);
            if let Some(session) = &mut tab.session {
                session.write_bytes(cmd.as_bytes());
            }
        }
    }

    pub fn request_focus(&mut self) {
        self.focus_next_frame = true;
    }

    fn spawn_tab(&mut self, profile: ShellProfile, rows: u16, cols: u16, ctx: &egui::Context) {
        let tab = TerminalTab::spawn(
            self.next_tab_id,
            profile,
            &self.working_dir,
            rows,
            cols,
            ctx,
        );
        self.next_tab_id += 1;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.focus_next_frame = true;
    }

    fn restart_active(&mut self, rows: u16, cols: u16, ctx: &egui::Context) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            let id = tab.id;
            let profile = tab.profile;
            *tab = TerminalTab::spawn(id, profile, &self.working_dir, rows, cols, ctx);
            self.focus_next_frame = true;
        }
    }

    pub fn resolve_height(&mut self, avail_h: f32) -> f32 {
        let min_h = 90.0;
        let max_h = (avail_h * 0.75).max(140.0);
        let current = *self
            .height
            .get_or_insert_with(|| (avail_h * 0.30).clamp(min_h, max_h));
        let clamped = current.clamp(min_h, max_h);
        self.height = Some(clamped);
        clamped
    }

    /// Returnează `true` dacă utilizatorul a închis panoul de terminal.
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, avail_h: f32) -> bool {
        let mut close_panel = false;
        let dark_mode = ui.visuals().dark_mode;

        let (default_bg, default_fg, accent_color, selection_bg) = if dark_mode {
            (
                Color32::from_rgb(12, 12, 12),
                Color32::from_rgb(204, 204, 204),
                Color32::from_rgb(88, 166, 255),
                Color32::from_rgba_unmultiplied(56, 139, 253, 95),
            )
        } else {
            (
                Color32::from_rgb(250, 250, 250),
                Color32::from_rgb(30, 30, 30),
                Color32::from_rgb(9, 105, 218),
                Color32::from_rgba_unmultiplied(9, 105, 218, 65),
            )
        };

        // 1. Splitter bar sus pentru redimensionare manuală
        let (splitter_rect, splitter_resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 5.0), egui::Sense::drag());
        if splitter_resp.hovered() || splitter_resp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            ui.painter().hline(
                splitter_rect.x_range(),
                splitter_rect.center().y,
                Stroke::new(2.0_f32, accent_color),
            );
        }
        if splitter_resp.dragged() {
            let min_h = 90.0;
            let max_h = (avail_h * 0.75).max(140.0);
            let current = self.height.unwrap_or((avail_h * 0.30).clamp(min_h, max_h));
            self.height = Some((current - splitter_resp.drag_delta().y).clamp(min_h, max_h));
        }

        // 2. Bara de titlu + tab-uri de sesiuni + selector shell (+ / ⌄)
        let mut profile_to_spawn: Option<ShellProfile> = None;
        let mut tab_to_close: Option<usize> = None;
        let mut restart_clicked = false;
        let mut clear_clicked = false;
        let mut copy_clicked = false;
        let has_active_selection = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.selection.is_some());

        ui.horizontal(|ui| {
            ui.strong("TERMINAL");
            ui.separator();

            for (idx, tab) in self.tabs.iter().enumerate() {
                let is_active = idx == self.active_tab;
                let label = format!("{}: {}", idx + 1, tab.profile.tab_label());

                ui.group(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if ui.selectable_label(is_active, label).clicked() {
                        self.active_tab = idx;
                        self.focus_next_frame = true;
                    }
                    if ui.small_button("×").on_hover_text("Închide sesiunea").clicked() {
                        tab_to_close = Some(idx);
                    }
                });
            }

            if ui
                .small_button("+")
                .on_hover_text("Terminal nou (cmd)")
                .clicked()
            {
                profile_to_spawn = Some(ShellProfile::default());
            }

            #[cfg(target_os = "windows")]
            ui.menu_button("⌄", |ui| {
                ui.set_min_width(180.0);
                for &profile in ShellProfile::all() {
                    if ui.button(profile.menu_label()).clicked() {
                        profile_to_spawn = Some(profile);
                    }
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Ascunde terminalul (Ctrl+`)")
                    .clicked()
                {
                    close_panel = true;
                }
                if ui
                    .small_button("↻")
                    .on_hover_text("Repornește sesiunea activă")
                    .clicked()
                {
                    restart_clicked = true;
                }
                if ui
                    .small_button("Clear")
                    .on_hover_text("Curăță ecranul")
                    .clicked()
                {
                    clear_clicked = true;
                }
                if ui
                    .add_enabled(has_active_selection, egui::Button::new("Copy").small())
                    .on_hover_text("Copiază selecția (Ctrl+C)")
                    .clicked()
                {
                    copy_clicked = true;
                }
            });
        });

        ui.separator();

        // 3. Calculăm dimensiunea grilei (rânduri x coloane)
        let font_id = FontId::monospace(14.0);
        let (row_height, char_width) =
            ui.fonts_mut(|f| (f.row_height(&font_id), f.glyph_width(&font_id, 'M')));

        let padding = 6.0;
        let canvas_size = ui.available_size();
        let cols = ((canvas_size.x - padding * 2.0) / char_width)
            .floor()
            .max(20.0) as u16;
        let rows = ((canvas_size.y - padding * 2.0) / row_height)
            .floor()
            .max(3.0) as u16;

        if let Some(idx) = tab_to_close {
            if idx < self.tabs.len() {
                self.tabs.remove(idx);
                if self.tabs.is_empty() {
                    self.active_tab = 0;
                    return true;
                } else if self.active_tab >= self.tabs.len() {
                    self.active_tab = self.tabs.len() - 1;
                } else if idx < self.active_tab {
                    self.active_tab -= 1;
                }
                self.focus_next_frame = true;
            }
        }

        if let Some(profile) = profile_to_spawn {
            self.spawn_tab(profile, rows, cols, ctx);
        } else if self.tabs.is_empty() {
            self.spawn_tab(ShellProfile::default(), rows, cols, ctx);
        }

        if restart_clicked {
            self.restart_active(rows, cols, ctx);
        }

        if clear_clicked {
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.selection = None;
                tab.scrollback_offset = 0;
                let clear_seq = tab.profile.clear_bytes();
                if let Some(session) = &mut tab.session {
                    if let Ok(mut st) = session.state.lock() {
                        st.history.clear();
                    }
                    session.write_bytes(clear_seq);
                }
            }
        }

        // 4. Canvas-ul interactiv al sesiunii active
        let (canvas_rect, resp) =
            ui.allocate_exact_size(canvas_size, egui::Sense::click_and_drag());

        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }

        if resp.clicked() || self.focus_next_frame {
            resp.request_focus();
            self.focus_next_frame = false;
        }

        let has_focus = resp.has_focus();
        if has_focus {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
        }

        let origin = canvas_rect.min + egui::vec2(padding, padding);

        // Meniu contextual la click-dreapta (Copy / Select All / Clear)
        let mut select_all_clicked = false;
        let mut ctx_clear_clicked = false;
        resp.context_menu(|ui| {
            if ui
                .add_enabled(has_active_selection, egui::Button::new("Copy   Ctrl+C"))
                .clicked()
            {
                copy_clicked = true;
            }
            if ui.button("Select All").clicked() {
                select_all_clicked = true;
            }
            ui.separator();
            if ui.button("Clear").clicked() {
                ctx_clear_clicked = true;
            }
        });

        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return close_panel;
        };

        if ctx_clear_clicked {
            tab.selection = None;
            tab.scrollback_offset = 0;
            let clear_seq = tab.profile.clear_bytes();
            if let Some(session) = &mut tab.session {
                if let Ok(mut st) = session.state.lock() {
                    st.history.clear();
                }
                session.write_bytes(clear_seq);
            }
        }

        if select_all_clicked {
            tab.selection = Some((
                CellPos { row: 0, col: 0 },
                CellPos {
                    row: rows.saturating_sub(1),
                    col: cols.saturating_sub(1),
                },
            ));
        }

        // Selecție cu mouse-ul
        if resp.drag_started_by(PointerButton::Primary) {
            resp.request_focus();
            if let Some(pointer) = resp.interact_pointer_pos() {
                let cell = pos_to_cell(pointer, origin, char_width, row_height, rows, cols);
                tab.selection = Some((cell, cell));
            }
        } else if resp.dragged_by(PointerButton::Primary) {
            if let Some(pointer) = ctx.pointer_interact_pos() {
                let cell = pos_to_cell(pointer, origin, char_width, row_height, rows, cols);
                if let Some((start, _)) = tab.selection {
                    tab.selection = Some((start, cell));
                }
            }
        } else if resp.double_clicked_by(PointerButton::Primary) {
            if let Some(pointer) = resp.interact_pointer_pos() {
                let cell = pos_to_cell(pointer, origin, char_width, row_height, rows, cols);
                if let Some(session) = &tab.session {
                    if let Ok(st) = session.state.lock() {
                        tab.selection = select_word_at(&st, tab.scrollback_offset, cell);
                    }
                }
            }
        } else if resp.clicked_by(PointerButton::Primary) {
            tab.selection = None;
        }

        if copy_clicked {
            tab.copy_selection_to_clipboard(ctx);
        }

        // Scroll cu rotița mouse-ului (plafonat la numărul real de linii din istoric)
        if resp.hovered() {
            let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll_y.abs() > 1.0 {
                let max_offset = tab
                    .session
                    .as_ref()
                    .and_then(|s| s.state.lock().ok().map(|st| st.history.len()))
                    .unwrap_or(0);

                let delta_lines = (scroll_y / row_height).round() as isize;
                if delta_lines > 0 {
                    tab.scrollback_offset = tab
                        .scrollback_offset
                        .saturating_add(delta_lines as usize)
                        .min(max_offset);
                } else {
                    tab.scrollback_offset = tab
                        .scrollback_offset
                        .saturating_sub(delta_lines.unsigned_abs());
                }
            }
        }

        // 5. Tastatură & paste către sesiunea activă
        let mut restart_after_enter = false;
        let mut keyboard_copy = false;
        let selection_active = tab.selection.is_some();

        if let Some(session) = &mut tab.session {
            if session.resize(rows, cols) {
                tab.selection = None;
            }

            if has_focus {
                let exited = session.is_exited();
                let mut bytes_to_send = Vec::new();
                let mut reset_scroll = false;

                ctx.input(|i| {
                    for event in &i.events {
                        match event {
                            Event::Copy => {
                                keyboard_copy = true;
                            }
                            Event::Text(text) => {
                                bytes_to_send.extend_from_slice(text.as_bytes());
                                reset_scroll = true;
                            }
                            Event::Paste(text) => {
                                bytes_to_send.extend_from_slice(text.as_bytes());
                                reset_scroll = true;
                            }
                            Event::Key {
                                key,
                                pressed: true,
                                modifiers,
                                ..
                            } => {
                                if (modifiers.ctrl || modifiers.command) && !modifiers.alt {
                                    if *key == Key::C {
                                        if modifiers.shift || selection_active {
                                            keyboard_copy = true;
                                        } else {
                                            bytes_to_send.push(0x03);
                                            reset_scroll = true;
                                        }
                                    } else if *key == Key::D {
                                        bytes_to_send.push(0x04);
                                        reset_scroll = true;
                                    } else if *key == Key::Z {
                                        bytes_to_send.push(0x1A);
                                        reset_scroll = true;
                                    }
                                    continue;
                                }

                                let seq: Option<&[u8]> = match key {
                                    Key::Enter => Some(b"\r"),
                                    Key::Backspace => Some(b"\x7f"),
                                    Key::Tab => Some(b"\t"),
                                    Key::Escape => Some(b"\x1b"),
                                    Key::ArrowUp => Some(b"\x1b[A"),
                                    Key::ArrowDown => Some(b"\x1b[B"),
                                    Key::ArrowRight => Some(b"\x1b[C"),
                                    Key::ArrowLeft => Some(b"\x1b[D"),
                                    Key::Home => Some(b"\x1b[H"),
                                    Key::End => Some(b"\x1b[F"),
                                    Key::Delete => Some(b"\x1b[3~"),
                                    Key::PageUp => Some(b"\x1b[5~"),
                                    Key::PageDown => Some(b"\x1b[6~"),
                                    _ => None,
                                };

                                if let Some(bytes) = seq {
                                    bytes_to_send.extend_from_slice(bytes);
                                    reset_scroll = true;
                                }
                            }
                            _ => {}
                        }
                    }
                });

                if exited && bytes_to_send.contains(&b'\r') {
                    restart_after_enter = true;
                } else if !bytes_to_send.is_empty() {
                    if reset_scroll {
                        tab.scrollback_offset = 0;
                        tab.selection = None;
                    }
                    session.write_bytes(&bytes_to_send);
                }
            }
        }

        if keyboard_copy {
            tab.copy_selection_to_clipboard(ctx);
        }

        if restart_after_enter {
            self.restart_active(rows, cols, ctx);
            return close_panel;
        }

        // 6. Randarea celulelor ANSI + selecție + cursor
        let painter = ui.painter_at(canvas_rect);
        painter.rect_filled(canvas_rect, 0.0, default_bg);

        if let Some(err) = &tab.spawn_error {
            painter.text(
                origin,
                egui::Align2::LEFT_TOP,
                err,
                font_id,
                Color32::LIGHT_RED,
            );
            return close_panel;
        }

        let selection_bounds = tab.selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });

        if let Some(session) = &tab.session {
            let exited = session.is_exited();
            if let Ok(st) = session.state.lock() {
                tab.scrollback_offset = tab.scrollback_offset.min(st.history.len());

                let screen = st.parser.screen();
                let (screen_rows, screen_cols) = screen.size();
                let (cursor_row, cursor_col) = screen.cursor_position();
                let hide_cursor = screen.hide_cursor() || tab.scrollback_offset > 0;

                for r in 0..screen_rows {
                    let y = origin.y + (r as f32) * row_height;
                    let mut job = LayoutJob::default();
                    job.wrap.max_width = f32::INFINITY;

                    for c in 0..screen_cols {
                        let x = origin.x + (c as f32) * char_width;
                        let cell_rect =
                            Rect::from_min_size(egui::pos2(x, y), egui::vec2(char_width, row_height));

                        let is_selected = selection_bounds.is_some_and(|(min_p, max_p)| {
                            let pos = CellPos { row: r, col: c };
                            pos >= min_p && pos <= max_p
                        });

                        if let Some(cell) = st.cell_at(r, c, tab.scrollback_offset) {
                            let mut fg = vt_color_to_egui(cell.fg, default_fg, true);
                            let mut bg = vt_color_to_egui(cell.bg, default_bg, false);

                            if cell.inverse {
                                std::mem::swap(&mut fg, &mut bg);
                            }

                            if bg != default_bg {
                                painter.rect_filled(cell_rect, 0.0, bg);
                            }
                            if is_selected {
                                painter.rect_filled(cell_rect, 0.0, selection_bg);
                            }

                            let text: &str = if cell.text.is_empty() {
                                " "
                            } else {
                                &cell.text
                            };

                            let underline = if cell.underline {
                                Stroke::new(1.0_f32, fg)
                            } else {
                                Stroke::NONE
                            };

                            job.append(
                                text,
                                0.0,
                                TextFormat {
                                    font_id: font_id.clone(),
                                    color: fg,
                                    italics: cell.italic,
                                    underline,
                                    ..Default::default()
                                },
                            );
                        } else {
                            if is_selected {
                                painter.rect_filled(cell_rect, 0.0, selection_bg);
                            }
                            job.append(
                                " ",
                                0.0,
                                TextFormat {
                                    font_id: font_id.clone(),
                                    color: default_fg,
                                    ..Default::default()
                                },
                            );
                        }
                    }

                    let galley = ui.fonts_mut(|f| f.layout_job(job));
                    painter.galley(egui::pos2(origin.x, y), galley, default_fg);
                }

                if !hide_cursor && cursor_row < screen_rows && cursor_col < screen_cols {
                    let cursor_rect = Rect::from_min_size(
                        egui::pos2(
                            origin.x + (cursor_col as f32) * char_width,
                            origin.y + (cursor_row as f32) * row_height,
                        ),
                        egui::vec2(char_width, row_height),
                    );

                    if has_focus {
                        painter.rect_filled(
                            cursor_rect,
                            0.0,
                            Color32::from_rgba_unmultiplied(
                                default_fg.r(),
                                default_fg.g(),
                                default_fg.b(),
                                140,
                            ),
                        );
                    } else {
                        painter.rect_stroke(
                            cursor_rect,
                            0.0,
                            Stroke::new(1.0_f32, default_fg),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }

            if exited {
                let msg = format!(
                    "Sesiunea {} s-a încheiat. Apasă Enter sau ↻ pentru repornire.",
                    tab.profile.tab_label()
                );
                let banner_rect = Rect::from_min_size(
                    egui::pos2(canvas_rect.left(), canvas_rect.bottom() - row_height - 4.0),
                    egui::vec2(canvas_rect.width(), row_height + 4.0),
                );
                painter.rect_filled(banner_rect, 0.0, Color32::from_rgb(130, 40, 40));
                painter.text(
                    banner_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    msg,
                    font_id,
                    Color32::WHITE,
                );
            }
        }

        close_panel
    }
}

fn pos_to_cell(
    pointer: Pos2,
    origin: Pos2,
    char_width: f32,
    row_height: f32,
    screen_rows: u16,
    screen_cols: u16,
) -> CellPos {
    let rel_x = (pointer.x - origin.x).max(0.0);
    let rel_y = (pointer.y - origin.y).max(0.0);
    let col = ((rel_x / char_width).floor() as u16).min(screen_cols.saturating_sub(1));
    let row = ((rel_y / row_height).floor() as u16).min(screen_rows.saturating_sub(1));
    CellPos { row, col }
}

fn extract_selected_text(
    state: &TerminalState,
    scrollback_offset: usize,
    start: CellPos,
    end: CellPos,
) -> String {
    let (min_pos, max_pos) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };

    let (screen_rows, screen_cols) = state.parser.screen().size();
    let end_row = max_pos.row.min(screen_rows.saturating_sub(1));
    let mut lines = Vec::new();

    for r in min_pos.row..=end_row {
        let col_start = if r == min_pos.row { min_pos.col } else { 0 };
        let col_end = if r == max_pos.row {
            max_pos.col.min(screen_cols.saturating_sub(1))
        } else {
            screen_cols.saturating_sub(1)
        };

        let mut line = String::new();
        for c in col_start..=col_end {
            if let Some(cell) = state.cell_at(r, c, scrollback_offset) {
                if cell.text.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&cell.text);
                }
            }
        }
        lines.push(line.trim_end().to_string());
    }

    lines.join("\n")
}

fn select_word_at(
    state: &TerminalState,
    scrollback_offset: usize,
    pos: CellPos,
) -> Option<(CellPos, CellPos)> {
    let (screen_rows, screen_cols) = state.parser.screen().size();
    if pos.row >= screen_rows || pos.col >= screen_cols {
        return None;
    }

    let is_word_cell = |c: u16| -> bool {
        state
            .cell_at(pos.row, c, scrollback_offset)
            .map(|cell| !cell.text.is_empty() && !cell.text.chars().all(char::is_whitespace))
            .unwrap_or(false)
    };

    if !is_word_cell(pos.col) {
        return None;
    }

    let mut start_col = pos.col;
    while start_col > 0 && is_word_cell(start_col - 1) {
        start_col -= 1;
    }

    let mut end_col = pos.col;
    while end_col + 1 < screen_cols && is_word_cell(end_col + 1) {
        end_col += 1;
    }

    Some((
        CellPos {
            row: pos.row,
            col: start_col,
        },
        CellPos {
            row: pos.row,
            col: end_col,
        },
    ))
}

fn vt_color_to_egui(color: vt100::Color, default: Color32, is_fg: bool) -> Color32 {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(idx) => ansi_256_to_rgb(idx, is_fg),
    }
}

fn ansi_256_to_rgb(idx: u8, is_fg: bool) -> Color32 {
    const ANSI_16: [(u8, u8, u8); 16] = [
        (12, 12, 12),    // 0: Black
        (197, 15, 31),   // 1: Red
        (19, 161, 14),   // 2: Green
        (193, 156, 0),   // 3: Yellow
        (0, 55, 218),    // 4: Blue
        (136, 23, 152),  // 5: Magenta
        (58, 150, 221),  // 6: Cyan
        (204, 204, 204), // 7: White
        (118, 118, 118), // 8: Bright Black
        (231, 72, 86),   // 9: Bright Red
        (22, 198, 12),   // 10: Bright Green
        (249, 241, 165), // 11: Bright Yellow
        (59, 120, 255),  // 12: Bright Blue
        (180, 0, 158),   // 13: Bright Magenta
        (97, 214, 214),  // 14: Bright Cyan
        (242, 242, 242), // 15: Bright White
    ];

    if idx < 16 {
        let (r, g, b) = ANSI_16[idx as usize];
        return Color32::from_rgb(r, g, b);
    }

    if idx < 232 {
        let v = idx - 16;
        let r = v / 36;
        let g = (v % 36) / 6;
        let b = v % 6;
        let map = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
        return Color32::from_rgb(map(r), map(g), map(b));
    }

    let gray = 8 + (idx - 232) * 10;
    let _ = is_fg;
    Color32::from_gray(gray)
}