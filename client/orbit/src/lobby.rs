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
//! The most players a lobby takes is chosen by the game as the lobby is made, and can't be
//! changed after (the record's "most players" limits nothing: an invitation still joined one set
//! to 1). So the host's choice of most players is the size of the lobbies it makes next
//! (`crosscom_client_common::lobby_size`), kept in the game's data by a thread of its own.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
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

/// The most players of the lobbies this game makes, as its host chose (0: the game's own sizes).
static MOST: AtomicU8 = AtomicU8::new(0);

/// The most players chosen for the lobbies this game makes (`None`: the game's own).
pub(crate) fn most() -> Option<u8> {
    Some(MOST.load(Ordering::Relaxed)).filter(|&m| m != 0)
}

/// Chooses the most players of the lobbies this game makes from now on (`None`: the game's own).
pub(crate) fn choose_most(most: Option<u8>) {
    MOST.store(most.unwrap_or(0), Ordering::Relaxed);
}

/// One of the sizes the game makes lobbies with: its own value, and what was last written there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Size {
    game: u8,
    written: Option<u8>,
}

impl Size {
    /// What to write, given the value there now and the choice; `self` is kept up to date. A
    /// value not written here is the game's (it writes its data as the library loads).
    fn next(&mut self, now: u8, most: Option<u8>) -> Option<u8> {
        if self.written != Some(now) {
            self.game = now;
            self.written = None;
        }
        let wanted = crosscom_client_common::lobby_size::chosen(self.game, most);
        if wanted == now {
            return None;
        }
        self.written = (wanted != self.game).then_some(wanted);
        Some(wanted)
    }
}

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

/// Keeps the sizes the game makes lobbies with at the host's choice, in each load of the AI
/// library, on a thread of its own.
#[cfg(windows)]
pub(crate) fn keep_sizes() {
    use crosscom_client_common::lobby_size;
    use log::info;
    use log::warn;

    std::thread::spawn(|| {
        // The library's base, the sizes' addresses in it, and what each holds.
        let mut load: Option<(usize, Vec<(usize, Size)>)> = None;
        let mut said = false;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let Some(base) = crate::memory::module(crosscom_client_common::quick_match::MODULE) else {
                load = None;
                continue;
            };
            if load.as_ref().is_none_or(|(b, _)| *b != base) {
                let found = crate::memory::code_sections(base).and_then(|sections| {
                    sections
                        .into_iter()
                        .find_map(|(start, size)| lobby_size::sizes(&crate::memory::read(base + start, size)?).ok())
                });
                let Some(addresses) = found else {
                    if crate::memory::module(crosscom_client_common::quick_match::MODULE) == Some(base) {
                        warn!("The most players of hosted lobbies can't be set: a game build not known");
                        return;
                    }
                    continue;
                };
                let sizes = addresses.iter().map(|&a| (a as usize, Size { game: 0, written: None })).collect();
                load = Some((base, sizes));
            }
            let most = most();
            if let Some((_, sizes)) = load.as_mut() {
                for (at, size) in sizes.iter_mut() {
                    let Some(now) = crate::memory::read(*at, 1).map(|b| b[0]) else { continue };
                    if let Some(value) = size.next(now, most) {
                        crate::memory::write(*at, &[value]);
                    }
                }
            }
            if most.is_some() && !said {
                info!("Lobbies this game makes take at most {} players", most.unwrap_or_default());
            }
            said = most.is_some();
        }
    });
}

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
    fn a_size_follows_the_choice_and_comes_back() {
        let mut size = Size { game: 0, written: None };
        // The game's 12, with no choice: left alone.
        assert_eq!(size.next(12, None), None);
        assert_eq!(size.next(12, Some(6)), Some(6));
        // Ours, kept.
        assert_eq!(size.next(6, Some(6)), None);
        // The game wrote its data again (the library loaded again): set again.
        assert_eq!(size.next(12, Some(6)), Some(6));
        // The choice taken back: the game's own.
        assert_eq!(size.next(6, None), Some(12));
        assert_eq!(size.next(12, None), None);
        // Never bigger than the game's.
        let mut guerrilla = Size { game: 0, written: None };
        assert_eq!(guerrilla.next(4, Some(6)), None);
    }

    #[test]
    fn the_minimum_goes_up_to_the_most_players() {
        assert_eq!(min_range(12), (1, 12));
        assert_eq!(min_range(4), (1, 4));
        assert_eq!(min_range(0), (1, 1));
    }
}
