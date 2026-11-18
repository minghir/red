use std::fs;

pub fn open_file(path: &str) -> Result<String, std::io::Error> {
    fs::read_to_string(path)
}

pub fn save_file(path: &str, content: &str) -> Result<(), std::io::Error> {
    fs::write(path, content)
}