//! The launcher's work, without its window (`launcher-slint` is the window): finding the game,
//! installing Cross-Com's client into it (and taking it out again), which DLC packs it has, its
//! settings, talking to a server, and sending it the game's logs.

pub mod client;
pub mod dlc;
pub mod firewall;
pub mod game;
pub mod logs;
pub mod server;
pub mod settings;
