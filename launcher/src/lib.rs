//! The launcher's work, without its window (`launcher-tauri` is the window): finding the game,
//! installing Cross-Com's client into it (and taking it out again), its settings, and talking to
//! a server.

pub mod client;
pub mod game;
pub mod server;
pub mod settings;
