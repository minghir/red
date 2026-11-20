use std::fs;
use std::io;
use std::path::Path;

/// Citește conținutul unui fișier de pe disc ca String UTF-8.
pub fn open_file<P: AsRef<Path>>(path: P) -> io::Result<String> {
    fs::read_to_string(path)
}

/// Scrie conținutul text într-un fișier pe disc (creează fișierul sau îl suprascrie).
pub fn save_file<P: AsRef<Path>>(path: P, content: &str) -> io::Result<()> {
    fs::write(path, content)
}

/// Extrage extensia fișierului (ex: "rs", "toml", "md") pentru syntax highlighting.
pub fn get_extension(path: &Path) -> Option<&str> {
    path.extension().and_then(|ext| ext.to_str())
}