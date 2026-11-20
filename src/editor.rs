use eframe::egui::{self, Align2, Color32, FontId, Rect, Stroke, Vec2};

pub struct CodeEditor {
    pub content: String,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

impl Default for CodeEditor {
    fn default() -> Self {
        Self {
            content: String::new(),
            cursor_line: 1,
            cursor_col: 1,
        }
    }
}

struct GutterPalette {
    bg: Color32,
    border: Color32,
    active_line_bg: Color32,
    active_text: Color32,
    inactive_text: Color32,
}

impl GutterPalette {
    fn from_visuals(dark_mode: bool) -> Self {
        if dark_mode {
            Self {
                bg: Color32::from_gray(26),
                border: Color32::from_gray(55),
                active_line_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 14),
                active_text: Color32::from_gray(235),
                inactive_text: Color32::from_gray(115),
            }
        } else {
            Self {
                bg: Color32::from_gray(238),
                border: Color32::from_gray(205),
                active_line_bg: Color32::from_rgba_unmultiplied(0, 0, 0, 14),
                active_text: Color32::from_gray(30),
                inactive_text: Color32::from_gray(135),
            }
        }
    }
}

impl CodeEditor {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        extension: Option<&str>,
        show_line_numbers: bool,
    ) -> egui::Response {
        let font_id = FontId::monospace(14.0);
        let (row_height, char_width) =
            ui.fonts_mut(|f| (f.row_height(&font_id), f.glyph_width(&font_id, '0')));

        let total_lines = self.content.split('\n').count();
        let digits = total_lines.to_string().len().max(3);
        let gutter_width = (digits as f32) * char_width + 16.0;
        let palette = GutterPalette::from_visuals(ui.visuals().dark_mode);

        let viewport_size = ui.available_size();
        let viewport_width = viewport_size.x;
        let viewport_height = viewport_size.y;

        // Dezactivăm line-wrapping-ul ca fiecare linie logică să corespundă 1:1 cu numărul ei
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
            let mut layout_job = crate::syntax::highlight(text.as_str(), extension);
            layout_job.wrap.max_width = f32::INFINITY;
            ui.fonts_mut(|f| f.layout_job(layout_job))
        };

        let scroll_id = ui.make_persistent_id("editor_scroll");

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            // 1. Coloana fixă din stânga pentru numerele de linie (întreaga înălțime a panoului)
            let gutter = if show_line_numbers {
                Some(ui.allocate_exact_size(
                    Vec2::new(gutter_width, viewport_height),
                    egui::Sense::hover(),
                ))
            } else {
                None
            };

            let editor_width = (viewport_width - if show_line_numbers { gutter_width } else { 0.0 })
                .max(40.0);

            // 2. Editorul cu scroll bidirecțional ancorat pe 100% din panou
            let output = egui::ScrollArea::both()
                .id_salt("editor_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let min_rows = (viewport_height / row_height).ceil().max(1.0) as usize;
                    egui::TextEdit::multiline(&mut self.content)
                        .code_editor()
                        .frame(false)
                        .lock_focus(true)
                        .desired_width(editor_width)
                        .desired_rows(min_rows)
                        .layouter(&mut layouter)
                        .show(ui)
                })
                .inner;

            // 3. Calculăm linia și coloana curentă a cursorului
            if let Some(range) = output.cursor_range {
                let mut line = 1;
                let mut col = 1;
                for ch in self.content.chars().take(range.primary.index) {
                    if ch == '\n' {
                        line += 1;
                        col = 1;
                    } else {
                        col += 1;
                    }
                }
                self.cursor_line = line;
                self.cursor_col = col;
            }

            // 4. Desenăm numerele liniilor sincronizate cu poziția reală a textului (galley_pos)
            if let Some((gutter_rect, gutter_resp)) = gutter {
                if gutter_resp.hovered() {
                    let delta = ui.input(|i| i.smooth_scroll_delta);
                    if delta != Vec2::ZERO {
                        if let Some(mut state) = egui::scroll_area::State::load(ui.ctx(), scroll_id)
                        {
                            state.offset.y = (state.offset.y - delta.y).max(0.0);
                            state.store(ui.ctx(), scroll_id);
                        }
                    }
                }

                let painter = ui.painter_at(gutter_rect);
                let galley_top = output.galley_pos.y;

                painter.rect_filled(gutter_rect, 0.0, palette.bg);
                painter.vline(
                    gutter_rect.right() - 0.5,
                    gutter_rect.y_range(),
                    Stroke::new(1.0_f32, palette.border),
                );

                for line_idx in 0..total_lines {
                    let line_num = line_idx + 1;
                    let (y_top, y_bottom, y_center) =
                        if let Some(row) = output.galley.rows.get(line_idx) {
                            let r = row.rect();
                            (
                                galley_top + r.top(),
                                galley_top + r.bottom(),
                                galley_top + r.center().y,
                            )
                        } else {
                            let top = galley_top + (line_idx as f32) * row_height;
                            (top, top + row_height, top + 0.5 * row_height)
                        };

                    // Randăm doar liniile vizibile în panou
                    if y_bottom < gutter_rect.top() || y_top > gutter_rect.bottom() {
                        continue;
                    }

                    let is_active_line = line_num == self.cursor_line;
                    if is_active_line {
                        let highlight_rect = Rect::from_min_max(
                            egui::pos2(gutter_rect.left(), y_top),
                            egui::pos2(gutter_rect.right() - 1.0, y_bottom),
                        );
                        painter.rect_filled(highlight_rect, 0.0, palette.active_line_bg);
                    }

                    let text_color = if is_active_line {
                        palette.active_text
                    } else {
                        palette.inactive_text
                    };

                    painter.text(
                        egui::pos2(gutter_rect.right() - 8.0, y_center),
                        Align2::RIGHT_CENTER,
                        line_num.to_string(),
                        font_id.clone(),
                        text_color,
                    );
                }
            }

            output.response
        })
        .inner
    }

    pub fn insert_at_cursor(&mut self, snippet: &str) {
        let mut current_line = 1;
        let mut current_col = 1;
        let mut byte_idx = self.content.len();

        for (idx, ch) in self.content.char_indices() {
            if current_line == self.cursor_line && current_col == self.cursor_col {
                byte_idx = idx;
                break;
            }
            if ch == '\n' {
                if current_line == self.cursor_line {
                    byte_idx = idx;
                    break;
                }
                current_line += 1;
                current_col = 1;
            } else {
                current_col += 1;
            }
        }

        self.content.insert_str(byte_idx, snippet);
    }
}