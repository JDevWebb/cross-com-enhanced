//! What both client DLLs share: the settings file (`crosscom.toml`) and the log (`crosscom.log`),
//! both in the game's folder, the game's saves (`saves`) and what the server says about itself
//! (`server_info`).

pub mod config;
pub mod logging;
pub mod quick_match;
pub mod saves;
pub mod server_info;

use std::path::PathBuf;

pub use config::Config;

/// The folder of the running game's exe.
#[must_use]
pub fn game_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}
