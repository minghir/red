use std::path::PathBuf;
use eframe::egui;
use rfd::FileDialog;

use crate::ai_chat::{ActiveFileContext, AiAction, AiChatPanel};
use crate::editor::CodeEditor;
use crate::explorer::{ExplorerAction, FileExplorer};
use crate::file_ops;
use crate::syntax::EditorTheme;
use crate::terminal::TerminalPanel;

pub struct Tab {
    pub id: usize,
    pub editor: CodeEditor,
    pub file_path: Option<PathBuf>,
    pub is_dirty: bool,
}

impl Tab {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            editor: CodeEditor::default(),
            file_path: None,
            is_dirty: false,
        }
    }

    pub fn title(&self) -> String {
        let name = self
            .file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled");

        if self.is_dirty {
            format!("{name} *")
        } else {
            name.to_string()
        }
    }
}

pub struct RedApp {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub next_tab_id: usize,
    pub explorer: FileExplorer,
    pub terminal: TerminalPanel,
    pub ai_chat: AiChatPanel,
    pub show_explorer: bool,
    pub show_terminal: bool,
    pub show_ai_chat: bool,
    pub show_line_numbers: bool,
    pub theme: EditorTheme,
    applied_theme: Option<EditorTheme>,
    pub status_message: String,
}

impl Default for RedApp {
    fn default() -> Self {
        let mut app = Self {
            tabs: vec![Tab::new(0)],
            active_tab: 0,
            next_tab_id: 1,
            explorer: FileExplorer::default(),
            terminal: TerminalPanel::default(),
            ai_chat: AiChatPanel::default(),
            show_explorer: true,
            show_terminal: true,
            show_ai_chat: true,
            show_line_numbers: true,
            theme: EditorTheme::default(),
            applied_theme: None,
            status_message: String::new(),
        };

        let cwd = std::env::current_dir().ok();

        if let Some(arg) = std::env::args().nth(1) {
            let raw_path = PathBuf::from(arg);
            let path = if raw_path.is_absolute() {
                raw_path
            } else if let Some(dir) = &cwd {
                dir.join(raw_path)
            } else {
                raw_path
            };

            if path.is_dir() {
                app.status_message = format!("Proiect deschis: {}", path.display());
                app.terminal.set_working_dir(path.clone());
                app.explorer.set_root(path);
            } else if path.is_file() {
                if let Some(parent) = path.parent() {
                    let parent_buf = parent.to_path_buf();
                    app.terminal.set_working_dir(parent_buf.clone());
                    app.explorer.set_root(parent_buf);
                }
                app.open_file_path(path);
            }
        } else if let Some(dir) = cwd {
            app.status_message = format!("Proiect deschis: {}", dir.display());
            app.terminal.set_working_dir(dir.clone());
            app.explorer.set_root(dir);
        }

        app
    }
}

impl RedApp {
    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.applied_theme == Some(self.theme) {
            return;
        }

        crate::syntax::set_theme(self.theme);

        let mut visuals = if self.theme.is_dark() {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        if self.theme == EditorTheme::GitHubDark {
            visuals.extreme_bg_color = egui::Color32::from_rgb(13, 17, 23);
        }

        ctx.set_visuals(visuals);
        self.applied_theme = Some(self.theme);
    }

    fn toggle_terminal(&mut self) {
        self.show_terminal = !self.show_terminal;
        if self.show_terminal {
            self.terminal.request_focus();
        }
    }

    fn toggle_ai_chat(&mut self) {
        self.show_ai_chat = !self.show_ai_chat;
        if self.show_ai_chat {
            self.ai_chat.request_focus();
        }
    }

    fn new_tab(&mut self) {
        let tab = Tab::new(self.next_tab_id);
        self.next_tab_id += 1;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.status_message = "Tab nou creat.".to_string();
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = FileDialog::new().pick_file() {
            self.open_file_path(path);
        }
    }

    fn open_folder_dialog(&mut self) {
        if let Some(folder) = FileDialog::new().pick_folder() {
            self.explorer.set_root(folder.clone());
            self.terminal.set_working_dir(folder.clone());
            self.show_explorer = true;
            self.status_message = format!("Proiect deschis: {}", folder.display());
        }
    }

    fn open_file_path(&mut self, path: PathBuf) {
        if let Some(existing_idx) = self
            .tabs
            .iter()
            .position(|t| t.file_path.as_ref() == Some(&path))
        {
            self.active_tab = existing_idx;
            self.status_message = "Fișierul este deja deschis.".to_string();
            return;
        }

        match file_ops::open_file(&path) {
            Ok(content) => {
                let reuse_initial = self.tabs.len() == 1
                    && self.tabs[0].file_path.is_none()
                    && !self.tabs[0].is_dirty
                    && self.tabs[0].editor.content.is_empty();

                if reuse_initial {
                    self.tabs[0].editor.content = content;
                    self.tabs[0].file_path = Some(path);
                    self.tabs[0].is_dirty = false;
                    self.active_tab = 0;
                } else {
                    let mut tab = Tab::new(self.next_tab_id);
                    self.next_tab_id += 1;
                    tab.editor.content = content;
                    tab.file_path = Some(path);
                    self.tabs.push(tab);
                    self.active_tab = self.tabs.len() - 1;
                }
                self.status_message = "Fișier încărcat.".to_string();
            }
            Err(err) => {
                self.status_message = format!("Eroare la deschidere: {err}");
            }
        }
    }

    fn save_current_with_ctx(&mut self, ctx: Option<&egui::Context>) {
        let has_path = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.file_path.as_ref())
            .is_some();

        if has_path {
            let saved_info = if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if let Some(path) = &tab.file_path {
                    match file_ops::save_file(path, &tab.editor.content) {
                        Ok(_) => {
                            tab.is_dirty = false;
                            self.status_message = "Salvat.".to_string();
                            Some((path.clone(), tab.editor.content.clone()))
                        }
                        Err(err) => {
                            self.status_message = format!("Eroare la salvare: {err}");
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let (Some((path, content)), Some(egui_ctx)) = (saved_info, ctx) {
                self.ai_chat.notify_file_saved(
                    self.explorer
                        .root
                        .as_ref()
                        .map(|root| root.path.as_path()),
                    &path,
                    &content,
                    egui_ctx,
                );
            }
        } else {
            self.save_current_as_with_ctx(ctx);
        }
    }

    fn save_current(&mut self) {
        self.save_current_with_ctx(None);
    }

    fn save_current_as_with_ctx(&mut self, ctx: Option<&egui::Context>) {
        if let Some(path) = FileDialog::new().save_file() {
            let saved_info = if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                match file_ops::save_file(&path, &tab.editor.content) {
                    Ok(_) => {
                        tab.file_path = Some(path.clone());
                        tab.is_dirty = false;
                        self.status_message = "Salvat.".to_string();
                        self.explorer.refresh();
                        Some((path, tab.editor.content.clone()))
                    }
                    Err(err) => {
                        self.status_message = format!("Eroare la salvare: {err}");
                        None
                    }
                }
            } else {
                None
            };

            if let (Some((path, content)), Some(egui_ctx)) = (saved_info, ctx) {
                self.ai_chat.notify_file_saved(
                    self.explorer
                        .root
                        .as_ref()
                        .map(|root| root.path.as_path()),
                    &path,
                    &content,
                    egui_ctx,
                );
            }
        }
    }

    fn save_current_as(&mut self) {
        self.save_current_as_with_ctx(None);
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }

        self.tabs.remove(index);

        if self.tabs.is_empty() {
            let tab = Tab::new(self.next_tab_id);
            self.next_tab_id += 1;
            self.tabs.push(tab);
            self.active_tab = 0;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if index < self.active_tab {
            self.active_tab -= 1;
        }
    }

    fn close_all_tabs(&mut self) {
        self.tabs.clear();
        let tab = Tab::new(self.next_tab_id);
        self.next_tab_id += 1;
        self.tabs.push(tab);
        self.active_tab = 0;
        self.status_message = "Toate tab-urile au fost închise.".to_string();
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let new_sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::N);
        let open_sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
        let open_folder_sc = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::O,
        );
        let save_as_sc = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::S,
        );
        let save_sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        let close_sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::W);
        let toggle_sidebar_sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::B);
        let toggle_terminal_sc =
            egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Backtick);
        let toggle_ai_sc = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::I,
        );
        let next_tab_sc = egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Tab);

        let mut trigger_save = false;
        let mut trigger_save_as = false;

        ctx.input_mut(|i| {
            if i.consume_shortcut(&new_sc) {
                self.new_tab();
            } else if i.consume_shortcut(&open_folder_sc) {
                self.open_folder_dialog();
            } else if i.consume_shortcut(&open_sc) {
                self.open_file_dialog();
            } else if i.consume_shortcut(&save_as_sc) {
                trigger_save_as = true;
            } else if i.consume_shortcut(&save_sc) {
                trigger_save = true;
            } else if i.consume_shortcut(&close_sc) {
                self.close_tab(self.active_tab);
            } else if i.consume_shortcut(&toggle_sidebar_sc) {
                self.show_explorer = !self.show_explorer;
            } else if i.consume_shortcut(&toggle_terminal_sc) {
                self.toggle_terminal();
            } else if i.consume_shortcut(&toggle_ai_sc) {
                self.toggle_ai_chat();
            } else if i.consume_shortcut(&next_tab_sc) && !self.tabs.is_empty() {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
            }
        });

        if trigger_save {
            self.save_current_with_ctx(Some(ctx));
        } else if trigger_save_as {
            self.save_current_as_with_ctx(Some(ctx));
        }
    }

    fn render_menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("main_menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    ui.set_min_width(200.0);

                    if ui
                        .add(egui::Button::new("New File").shortcut_text("Ctrl+N"))
                        .clicked()
                    {
                        self.new_tab();
                    }
                    if ui
                        .add(egui::Button::new("Open File...").shortcut_text("Ctrl+O"))
                        .clicked()
                    {
                        self.open_file_dialog();
                    }
                    if ui
                        .add(egui::Button::new("Open Folder...").shortcut_text("Ctrl+Shift+O"))
                        .clicked()
                    {
                        self.open_folder_dialog();
                    }

                    ui.separator();

                    if ui
                        .add(egui::Button::new("Save").shortcut_text("Ctrl+S"))
                        .clicked()
                    {
                        self.save_current_with_ctx(Some(ctx));
                    }
                    if ui
                        .add(egui::Button::new("Save As...").shortcut_text("Ctrl+Shift+S"))
                        .clicked()
                    {
                        self.save_current_as_with_ctx(Some(ctx));
                    }

                    ui.separator();

                    if ui
                        .add(egui::Button::new("Close Tab").shortcut_text("Ctrl+W"))
                        .clicked()
                    {
                        self.close_tab(self.active_tab);
                    }
                    if ui.button("Close All").clicked() {
                        self.close_all_tabs();
                    }

                    ui.separator();

                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("Edit", |ui| {
                    ui.set_min_width(180.0);

                    if ui.button("Clear Document").clicked() {
                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                            if !tab.editor.content.is_empty() {
                                tab.editor.content.clear();
                                tab.is_dirty = true;
                            }
                        }
                    }

                    ui.separator();
                    ui.label("Undo / Redo (Ctrl+Z / Ctrl+Y)");
                });

                ui.menu_button("View", |ui| {
                    ui.set_min_width(200.0);

                    let explorer_label = if self.show_explorer {
                        "✓ Explorer"
                    } else {
                        "   Explorer"
                    };
                    if ui
                        .add(egui::Button::new(explorer_label).shortcut_text("Ctrl+B"))
                        .clicked()
                    {
                        self.show_explorer = !self.show_explorer;
                    }

                    let ai_label = if self.show_ai_chat {
                        "✓ AI Chat"
                    } else {
                        "   AI Chat"
                    };
                    if ui
                        .add(egui::Button::new(ai_label).shortcut_text("Ctrl+Shift+I"))
                        .clicked()
                    {
                        self.toggle_ai_chat();
                    }

                    let terminal_label = if self.show_terminal {
                        "✓ Terminal"
                    } else {
                        "   Terminal"
                    };
                    if ui
                        .add(egui::Button::new(terminal_label).shortcut_text("Ctrl+`"))
                        .clicked()
                    {
                        self.toggle_terminal();
                    }

                    let line_numbers_label = if self.show_line_numbers {
                        "✓ Line Numbers"
                    } else {
                        "   Line Numbers"
                    };
                    if ui.button(line_numbers_label).clicked() {
                        self.show_line_numbers = !self.show_line_numbers;
                    }

                    ui.separator();

                    ui.menu_button("Color Theme", |ui| {
                        ui.set_min_width(180.0);

                        let mut added_light_separator = false;
                        for &theme in EditorTheme::ALL {
                            if !theme.is_dark() && !added_light_separator {
                                ui.separator();
                                added_light_separator = true;
                            }

                            let label = if self.theme == theme {
                                format!("✓ {}", theme.label())
                            } else {
                                format!("   {}", theme.label())
                            };

                            if ui.button(label).clicked() {
                                self.theme = theme;
                                self.status_message = format!("Temă activă: {}", theme.label());
                            }
                        }
                    });
                });
            });
        });
    }

    fn render_status_bar(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tab) = self.tabs.get(self.active_tab) {
                    let full_path = tab
                        .file_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Untitled".to_string());
                    let ext = tab
                        .file_path
                        .as_deref()
                        .and_then(file_ops::get_extension)
                        .unwrap_or("plain text");
                    let lines = tab.editor.content.split('\n').count();
                    let chars = tab.editor.content.chars().count();

                    ui.label(format!(
                        "{full_path} | Ln {}, Col {} | Tip: {ext} | Temă: {} | Linii: {lines} | Caractere: {chars}",
                        tab.editor.cursor_line,
                        tab.editor.cursor_col,
                        self.theme.label()
                    ));
                    ui.separator();
                }
                ui.label(&self.status_message);
            });
        });
    }

    fn render_explorer_panel(&mut self, ctx: &egui::Context) {
        if !self.show_explorer {
            return;
        }

        let active_path = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.file_path.clone());

        let mut action = ExplorerAction::None;

        egui::SidePanel::left("file_explorer")
            .resizable(true)
            .default_width(220.0)
            .min_width(150.0)
            .show(ctx, |ui| {
                action = self.explorer.show(ui, active_path.as_deref());
            });

        match action {
            ExplorerAction::OpenFolderRequested => self.open_folder_dialog(),
            ExplorerAction::FileClicked(path) => self.open_file_path(path),
            ExplorerAction::None => {}
        }
    }

    fn render_ai_chat_panel(&mut self, ctx: &egui::Context) {
        if !self.show_ai_chat {
            return;
        }

        let project_root = self
            .explorer
            .root
            .as_ref()
            .map(|root| root.path.clone());

        let active_ctx = self.tabs.get(self.active_tab).map(|tab| {
            let file_name = tab
                .file_path
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("Untitled")
                .to_string();

            let relative_path = match (&project_root, &tab.file_path) {
                (Some(root), Some(fp)) => fp
                    .strip_prefix(root)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| file_name.clone()),
                (_, Some(fp)) => fp
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Untitled")
                    .to_string(),
                _ => file_name.clone(),
            };

            ActiveFileContext {
                file_name,
                relative_path,
                extension: tab
                    .file_path
                    .as_deref()
                    .and_then(file_ops::get_extension)
                    .map(str::to_owned),
                content: tab.editor.content.clone(),
            }
        });

        let mut close_requested = false;
        let mut ai_action = AiAction::None;

        egui::SidePanel::right("ai_chat_panel")
            .resizable(true)
            .default_width(340.0)
            .min_width(240.0)
            .max_width(580.0)
            .show(ctx, |ui| {
                let (close, act) =
                    self.ai_chat
                        .show(ui, ctx, active_ctx.as_ref(), project_root.as_deref());
                close_requested = close;
                ai_action = act;
            });

        if close_requested {
            self.show_ai_chat = false;
        }

        match ai_action {
            AiAction::InsertCode(code) => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.editor.insert_at_cursor(&code);
                    tab.is_dirty = true;
                    self.status_message = "Cod inserat în editor.".to_string();
                }
            }
            AiAction::ReplaceFile(code) => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.editor.content = code;
                    tab.is_dirty = true;
                    self.status_message = "Conținutul fișierului a fost actualizat.".to_string();
                }
            }
            AiAction::None => {}
        }
    }

    fn render_terminal_panel(&mut self, ctx: &egui::Context) {
        if !self.show_terminal {
            return;
        }

        let avail_h = ctx.available_rect().height();
        let height = self.terminal.resolve_height(avail_h);
        let mut close_requested = false;

        egui::TopBottomPanel::bottom("terminal_panel")
            .exact_height(height)
            .show(ctx, |ui| {
                close_requested = self.terminal.show(ui, ctx, avail_h);
            });

        if close_requested {
            self.show_terminal = false;
        }
    }

    fn render_tabs_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("file_tabs_bar").show(ctx, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("tabs_scroll")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let mut tab_to_close: Option<usize> = None;

                        for (idx, tab) in self.tabs.iter().enumerate() {
                            let is_active = idx == self.active_tab;

                            ui.group(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;

                                let mut label_resp = ui.selectable_label(is_active, tab.title());
                                if let Some(path) = &tab.file_path {
                                    label_resp =
                                        label_resp.on_hover_text(path.display().to_string());
                                }

                                if label_resp.clicked() {
                                    self.active_tab = idx;
                                }
                                if label_resp.middle_clicked() {
                                    tab_to_close = Some(idx);
                                }

                                if ui.small_button("×").clicked() {
                                    tab_to_close = Some(idx);
                                }
                            });
                        }

                        if let Some(idx) = tab_to_close {
                            self.close_tab(idx);
                        }

                        if ui.button("+").on_hover_text("Tab nou (Ctrl+N)").clicked() {
                            self.new_tab();
                        }
                    });
                });
        });
    }
}

impl eframe::App for RedApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        self.handle_shortcuts(ctx);

        self.render_menu_bar(ctx);
        self.render_status_bar(ctx);
        self.render_explorer_panel(ctx);
        self.render_ai_chat_panel(ctx);
        self.render_terminal_panel(ctx);
        self.render_tabs_bar(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                let extension = tab
                    .file_path
                    .as_deref()
                    .and_then(file_ops::get_extension)
                    .map(str::to_owned);

                ui.push_id(tab.id, |ui| {
                    let response =
                        tab.editor
                            .show(ui, extension.as_deref(), self.show_line_numbers);
                    if response.changed() {
                        tab.is_dirty = true;
                    }
                });
            }
        });
    }
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_icon(generate_red_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "RED - Rust EDitor",
        options,
        Box::new(|_| Ok(Box::new(RedApp::default()))),
    )
}

fn generate_red_icon() -> egui::IconData {
    const SIZE: usize = 64;
    const GLYPH: [&str; 7] = [
        "11110", "10001", "10001", "11110", "10100", "10010", "10001",
    ];

    let mut rgba = vec![0; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let glyph_x = x.saturating_sub(12) / 8;
            let glyph_y = y.saturating_sub(4) / 8;
            let is_letter = x >= 12
                && x < 52
                && y >= 4
                && y < 60
                && GLYPH[glyph_y].as_bytes()[glyph_x] == b'1';
            let color = if is_letter {
                [255, 255, 255, 255]
            } else {
                [198, 40, 40, 255]
            };
            let offset = (y * SIZE + x) * 4;
            rgba[offset..offset + 4].copy_from_slice(&color);
        }
    }

    egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}