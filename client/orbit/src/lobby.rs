//! The lobby you host: how many players it needs before its countdown starts, set from the
//! overlay's Lobby tab while it waits.
//!
//! The game keeps the lobby in a record in its network mode's object: its kind, the players
//! needed to start (a Quick Match or a squad room starts its countdown at that many; the others
//! are launched by their host) and the most players. The host's record is the one that counts: the
//! host runs the start test. Only the retail DX11 exe is known; nothing is read or written
//! otherwise.
//!
//! The game sets the minimum again as players join a Quick Match (to the client's
//! `quick_match_min_players`), so a choice made here is kept: written again each frame while the
//! lobby waits, and forgotten when its match starts or this game stops hosting it. Everything is
//! read and written through `ReadProcessMemory`/`WriteProcessMemory`, which fail rather than fault.
//!
//! The record's "most players" isn't what limits a lobby (an invitation still joined one set to
//! 1); it's only read, as the minimum's ceiling.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::Mutex;

/// The network mode's object (the online mode's while in a lobby).
const MODE_OBJECT: usize = 0x00fe_9a88;
/// The online mode's class (its vtable): the only one with a lobby record.
const ONLINE_MODE: u32 = 0x00dd_cdc4;
/// In the mode object: whether this game hosts the lobby, whether its match has started, the
/// record and the record's size.
const IS_HOST: usize = 0x5;
const IN_MATCH: usize = 0x4;
const RECORD: usize = 0x18c1;
const RECORD_SIZE: usize = 0x1941;
/// A lobby's record is this big.
const LOBBY_RECORD: u8 = 0x68;
/// In the record.
const STARTED: usize = 0x23;
const KIND: usize = 0x27;
const MIN: usize = 0x28;
const MAX: usize = 0x29;
/// The lobby record's getter binding, as the known exe has it: `mov ecx, [MODE_OBJECT]; mov eax,
/// [ecx]; mov edx, [eax+0x8c]` (the record's size), checked before anything is read.
const KNOWN_AT: usize = 0x00b6_2400;
const KNOWN: [u8; 14] = [0x8b, 0x0d, 0x88, 0x9a, 0xfe, 0x00, 0x8b, 0x01, 0x8b, 0x90, 0x8c, 0x00, 0x00, 0x00];

/// What a lobby is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    QuickMatch,
    /// Custom and private matches, Guerrilla and co-op: the host launches them.
    Custom,
    SquadRoom,
}

impl Kind {
    fn from_record(kind: u8) -> Option<Self> {
        match kind {
            1 => Some(Self::QuickMatch),
            2 | 3 => Some(Self::Custom),
            5 => Some(Self::SquadRoom),
            _ => None,
        }
    }

    /// Whether the game starts it by itself at the minimum (the others are launched by the host).
    pub(crate) fn starts_at_minimum(self) -> bool {
        matches!(self, Self::QuickMatch | Self::SquadRoom)
    }
}

/// The lobby this game is in, as its record has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Lobby {
    pub(crate) kind: Kind,
    pub(crate) host: bool,
    pub(crate) min: u8,
    pub(crate) max: u8,
}

/// The host's choice for the lobby it hosts: the mode object it's in (a new lobby is in a new one,
/// or after a match) and the minimum chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chosen {
    object: usize,
    min: Option<u8>,
}

static CHOSEN: Mutex<Option<Chosen>> = Mutex::new(None);

/// The range the minimum may be set in: from 1 up to the most players.
pub(crate) fn min_range(max: u8) -> (u8, u8) {
    (1, max.max(1))
}

#[cfg(windows)]
mod game {
    use std::sync::OnceLock;

    use super::*;
    use crate::memory;

    /// Whether the exe is the one these places are known in (looked at once).
    fn known() -> bool {
        static KNOWN_EXE: OnceLock<bool> = OnceLock::new();
        *KNOWN_EXE.get_or_init(|| memory::read(KNOWN_AT, KNOWN.len()).as_deref() == Some(&KNOWN[..]))
    }

    /// The mode object, its flags and the lobby record, if it's the online mode's with a lobby
    /// whose match hasn't started.
    fn waiting_lobby() -> Option<(usize, Vec<u8>, Vec<u8>)> {
        if !known() {
            return None;
        }
        let object = memory::read_u32(MODE_OBJECT)? as usize;
        if object == 0 || memory::read_u32(object)? != ONLINE_MODE {
            return None;
        }
        if memory::read(object + RECORD_SIZE, 1)?[0] != LOBBY_RECORD {
            return None;
        }
        let flags = memory::read(object, 8)?;
        let record = memory::read(object + RECORD, usize::from(LOBBY_RECORD))?;
        if flags[IN_MATCH] != 0 || record[STARTED] != 0 {
            return None;
        }
        Some((object, flags, record))
    }

    /// The lobby this game waits in (none once its match has started).
    pub(crate) fn current() -> Option<Lobby> {
        let (_, flags, record) = waiting_lobby()?;
        Some(Lobby {
            kind: Kind::from_record(record[KIND])?,
            host: flags[IS_HOST] != 0,
            min: record[MIN],
            max: record[MAX],
        })
    }

    /// Keeps the host's choice in its lobby; called every frame (on the overlay's thread).
    pub(crate) fn tick() {
        let mut chosen = CHOSEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let hosted = waiting_lobby().filter(|(_, flags, record)| flags[IS_HOST] != 0 && Kind::from_record(record[KIND]).is_some_and(Kind::starts_at_minimum));
        let Some((object, _, record)) = hosted else {
            *chosen = None;
            return;
        };
        let c = chosen.get_or_insert(Chosen { object, min: None });
        if c.object != object {
            *c = Chosen { object, min: None };
        }
        if let Some(min) = c.min.filter(|&m| m != record[MIN]) {
            memory::write(object + RECORD + MIN, &[min]);
        }
    }

    /// Sets the hosted lobby's minimum (kept until its match starts).
    pub(crate) fn choose_min(min: u8) {
        if let Some(c) = CHOSEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
            c.min = Some(min);
        }
        tick();
    }
}

#[cfg(windows)]
pub(crate) use game::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_as_the_record_has_them() {
        assert_eq!(Kind::from_record(1), Some(Kind::QuickMatch));
        assert_eq!(Kind::from_record(2), Some(Kind::Custom));
        assert_eq!(Kind::from_record(3), Some(Kind::Custom));
        assert_eq!(Kind::from_record(5), Some(Kind::SquadRoom));
        assert_eq!(Kind::from_record(0), None);
        assert_eq!(Kind::from_record(192), None);
        assert!(Kind::QuickMatch.starts_at_minimum());
        assert!(Kind::SquadRoom.starts_at_minimum());
        assert!(!Kind::Custom.starts_at_minimum());
    }

    #[test]
    fn the_minimum_goes_up_to_the_most_players() {
        assert_eq!(min_range(12), (1, 12));
        assert_eq!(min_range(4), (1, 4));
        assert_eq!(min_range(0), (1, 1));
    }
}
