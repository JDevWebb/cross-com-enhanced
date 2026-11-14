//! The Cross-Com launcher: the window, and the commands its page calls (commands.rs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

/// Where Microsoft offers the WebView2 runtime, for a PC without it.
#[cfg(target_os = "windows")]
const WEBVIEW2_PAGE: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

fn main() {
    // The window is a web view: on Windows it needs Microsoft's WebView2 runtime (part of Windows
    // 11 and of Edge; an older Windows 10 may lack it). Without it, open its download page rather
    // than fail to open.
    #[cfg(target_os = "windows")]
    if tauri::webview_version().is_err() {
        let _ = std::process::Command::new("explorer").arg(WEBVIEW2_PAGE).spawn();
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::Launcher::new())
        .invoke_handler(tauri::generate_handler![
            commands::info,
            commands::find_game,
            commands::check_game,
            commands::check_server,
            commands::server_info,
            commands::setup,
            commands::set_account,
            commands::set_options,
            commands::play,
            commands::restore,
        ])
        .run(tauri::generate_context!())
        .expect("the launcher's window");
}
