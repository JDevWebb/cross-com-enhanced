//! The launcher's work, without its window (`launcher-slint` is the window): finding the game,
//! installing Cross-Com's client into it (and taking it out again), which DLC packs it has, its
//! settings, the player's identity, talking to a server, sending it the game's logs, and
//! hosting a server for friends.

pub mod client;
pub mod dlc;
pub mod firewall;
pub mod game;
pub mod hosting;
pub mod identity;
pub mod logs;
pub mod server;
pub mod settings;
pub mod update;
pub mod windows_dirs;
