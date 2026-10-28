//! Future Soldier's online protocols: request, response and type definitions generated with
//! `quazal-tools` from the protocol declarations the game carries, with the protocol IDs in
//! `data/protocol_ids.json`. Regenerate rather than edit the files in `protocols/`.
#[macro_use]
extern crate quazal_macros;
#[macro_use]
extern crate slog;

mod protocols;

pub use crate::protocols::*;
