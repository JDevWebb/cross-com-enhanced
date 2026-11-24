//! The Cross-Com launcher: one native window (ui.rs, ui/*.slint) on the launcher library
//! (core.rs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod core;
mod shots;
mod ui;
mod update;

fn main() {
    // The README's screenshots, on made-up data (scripts/launcher-shots.sh).
    let args: Vec<String> = std::env::args().collect();
    if let Some(at) = args.iter().position(|a| a == "--shots") {
        let dir = args.get(at + 1).map_or_else(|| "shots".into(), std::path::PathBuf::from);
        if let Err(e) = shots::run(&dir) {
            eprintln!("shots: {e}");
            std::process::exit(1);
        }
        return;
    }
    // OpenGL where there's OpenGL 2 or later; Slint's software renderer where there isn't (a
    // virtual machine without its graphics driver), or with CROSSCOM_RENDERER=software.
    if std::env::var("CROSSCOM_RENDERER").as_deref() == Ok("software") && std::env::var_os("SLINT_BACKEND").is_none() {
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }
    if let Err(e) = ui::run() {
        let said = format!("The launcher couldn't open its window: {e}\n\nIf this is a virtual machine, turn on its 3D acceleration or install its graphics driver, or start the launcher with CROSSCOM_RENDERER=software.");
        #[cfg(windows)]
        rfd::MessageDialog::new()
            .set_title("Cross-Com Enhanced")
            .set_description(&said)
            .set_level(rfd::MessageLevel::Error)
            .show();
        eprintln!("{said}");
        std::process::exit(1);
    }
}
