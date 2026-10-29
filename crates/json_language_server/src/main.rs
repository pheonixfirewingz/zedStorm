#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::io::Result<()> {
    json_language_server::run_server()
}
