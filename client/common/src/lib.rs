//! What both client DLLs share: the settings file (`crosscom.toml`) and the log (`crosscom.log`),
//! both in the game's folder.

pub mod config;
pub mod logging;

use std::path::PathBuf;

pub use config::Config;

/// The folder of the running game's exe.
#[must_use]
pub fn game_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}
