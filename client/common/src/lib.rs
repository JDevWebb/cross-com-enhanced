//! What both client DLLs share: the settings file (`crosscom.toml`) and the log (`crosscom.log`),
//! both in the game's folder, and the game's saves (`saves`).

pub mod config;
pub mod logging;
pub mod quick_match;
pub mod saves;

use std::path::PathBuf;

pub use config::Config;

/// The folder of the running game's exe.
#[must_use]
pub fn game_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}
