//! The Cross-Com launcher: a window that sets the game up and starts it. Windows only.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;

#[cfg(windows)]
fn main() -> eframe::Result {
    app::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The Cross-Com launcher runs on Windows, where the game does.");
}
