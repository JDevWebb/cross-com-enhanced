//! What both client DLLs share: the settings file (`crosscom.toml`) and the log (`crosscom.log`),
//! both in the game's folder, each run's log and sending it to the server (`runs`), the game's
//! saves (`saves`), the DLC packs installed (`dlc`) and what the server says about itself
//! (`server_info`).

pub mod config;
pub mod dlc;
pub mod lobby_size;
pub mod logging;
pub mod quick_match;
pub mod runs;
pub mod saves;
pub mod server_info;
pub mod system;

use std::path::PathBuf;

pub use config::Config;

/// The folder of the running game's exe.
#[must_use]
pub fn game_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}
