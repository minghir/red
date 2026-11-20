mod ai_chat;
mod app;
mod editor;
mod explorer;
mod file_ops;
mod syntax;
mod terminal;

fn main() -> eframe::Result {
    app::run()
}