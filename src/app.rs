use eframe::egui;
use rfd::FileDialog;

#[derive(Default)]
pub struct RedApp {
    pub text: String,
	pub current_file: Option<String>,
}

impl eframe::App for RedApp {
    fn update(
		&mut self,
		ctx: &egui::Context,
		_frame: &mut eframe::Frame,
	) {
		egui::TopBottomPanel::top("menu").show(ctx, |ui| {
			ui.horizontal(|ui| {

				if ui.button("Open").clicked() {
					if let Some(path) = FileDialog::new().pick_file() {
						if let Ok(content) =
							crate::file_ops::open_file(
								path.to_str().unwrap(),
							)
						{
							self.text = content;
							self.current_file =
								Some(path.display().to_string());
						}
					}
				}

				if ui.button("Save").clicked() {
					if let Some(path) = &self.current_file {
						let _ = crate::file_ops::save_file(
							path,
							&self.text,
						);
					}
				}
			});
		});

		egui::CentralPanel::default().show(ctx, |ui| {

			ui.heading("RED - Rust EDitor");

			if let Some(path) = &self.current_file {
				ui.label(path);
			} else {
				ui.label("Untitled");
			}

			ui.add(
				egui::TextEdit::multiline(&mut self.text)
					.desired_rows(30),
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