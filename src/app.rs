use eframe::egui;

#[derive(Default)]
pub struct RedApp {
    pub text: String,
}

impl eframe::App for RedApp {
    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("RED - Rust EDitor");

            ui.add(
                egui::TextEdit::multiline(&mut self.text)
                    .desired_rows(30)
            );
        });
    }
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "RED",
        options,
        Box::new(|_| Ok(Box::new(RedApp::default()))),
    )
}