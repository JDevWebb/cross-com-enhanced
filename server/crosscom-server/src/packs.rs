//! Which DLC packs players have and lobbies use, so a search offers a player only the lobbies
//! their game can load.
//!
//! - A lobby's packs come from its host: the game asks for the map list with the packs its lobby
//!   uses (see `maps`: 0x8000 Arctic Strike, 0x10000 Khyber Strike).
//! - A player's packs come from Cross-Com's client: the game itself doesn't say which it has,
//!   but the client hands it its sign-in details, and adds the packs it found in the game's
//!   folder to the CD key as `;packs=` and the pack numbers (`;packs=13` for Arctic and Khyber
//!   Strike, `;packs=` for none). The server checks no CD keys, and the field comes with the
//!   sign-in, so it's the signed-in player's own. A client that doesn't say (an older one) isn't
//!   held back: the game's own check, as it joins, still refuses a lobby it can't load.
//! - A lobby in Stockade needs Arctic Strike, and one in Takeover Khyber Strike, on any map: the
//!   game refuses those modes without them.

use std::collections::HashMap;
use std::sync::Mutex;

/// The packs' bits, as the game's map-list category has them.
pub const ARCTIC: u32 = 0x8000;
pub const KHYBER: u32 = 0x1_0000;
const PACK_BITS: u32 = ARCTIC | KHYBER;

/// Session property 116: the mode, in the game's own numbering, as its worlds record it (see
/// `maps::mode`): seen in game (2026-11-18), Stockade lobbies are 1 and Takeover lobbies 6, the
/// map list asking for their categories (0x100, 0x200) at the same time.
pub const MODE: u32 = 116;
const STOCKADE: u32 = crate::maps::mode::STOCKADE;
const TAKEOVER: u32 = crate::maps::mode::TAKEOVER;

/// How many players and lobbies are remembered: more than a server has at once (each player has
/// one entry, replaced at each sign-in).
const MAX_ENTRIES: usize = 100_000;

/// What the client adds to the CD key.
const MARK: &str = ";packs=";

/// The packs a client says its game has, from the CD key it signed in with, as category bits;
/// `None` if it doesn't say. Raven Strike (2) has no bit: it's co-op only.
#[must_use]
pub fn from_online_key(key: &str) -> Option<u32> {
    let (_, packs) = key.rsplit_once(MARK)?;
    if packs.len() > 3 || !packs.chars().all(|c| matches!(c, '1'..='3')) {
        return None;
    }
    Some(packs.chars().fold(0, |bits, c| match c {
        '1' => bits | ARCTIC,
        '3' => bits | KHYBER,
        _ => bits,
    }))
}

/// The packs a lobby needs: those its map list uses, and its mode's.
#[must_use]
pub fn needed(lobby: Option<u32>, mode: Option<u32>) -> u32 {
    let mode_pack = match mode {
        Some(STOCKADE) => ARCTIC,
        Some(TAKEOVER) => KHYBER,
        _ => 0,
    };
    lobby.unwrap_or(0) & PACK_BITS | mode_pack
}

/// Whether a player with `player`'s packs (`None`: unknown) may be offered a lobby needing
/// `needed`.
#[must_use]
pub fn can_join(player: Option<u32>, needed: u32) -> bool {
    player.is_none_or(|has| needed & !has == 0)
}

#[derive(Default)]
pub struct Packs {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// A signed-in player's packs, by PID.
    players: HashMap<u32, u32>,
    /// The packs the lobby a player hosts uses, by the host's PID (from its last map list).
    lobbies: HashMap<u32, u32>,
}

impl Packs {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A player signed in, saying which packs it has (or not).
    pub fn signed_in(&self, pid: u32, packs: Option<u32>) {
        let mut inner = self.lock();
        match packs {
            Some(bits) => {
                if inner.players.len() >= MAX_ENTRIES && !inner.players.contains_key(&pid) {
                    inner.players.clear();
                }
                inner.players.insert(pid, bits);
            }
            None => {
                inner.players.remove(&pid);
            }
        }
        // A new sign-in starts with no lobby.
        inner.lobbies.remove(&pid);
    }

    /// A host asked for its lobby's map list with this category.
    pub fn hosting(&self, host: u32, category: u32) {
        let mut inner = self.lock();
        if inner.lobbies.len() >= MAX_ENTRIES && !inner.lobbies.contains_key(&host) {
            inner.lobbies.clear();
        }
        inner.lobbies.insert(host, category & PACK_BITS);
    }

    #[must_use]
    pub fn player(&self, pid: u32) -> Option<u32> {
        self.lock().players.get(&pid).copied()
    }

    #[must_use]
    pub fn lobby(&self, host: u32) -> Option<u32> {
        self.lock().lobbies.get(&host).copied()
    }

    /// Whether `pid` may be offered the lobby `host` hosts, in `mode` (property 116).
    #[must_use]
    pub fn offer(&self, pid: u32, host: u32, mode: Option<u32>) -> bool {
        can_join(self.player(pid), needed(self.lobby(host), mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_as_the_game_sends_them() {
        // Property 116 as seen from the game (2026-11-18): a Takeover lobby is 6, a Stockade one 1.
        assert_eq!(needed(None, Some(6)), KHYBER, "Takeover needs Khyber Strike");
        assert_eq!(needed(None, Some(1)), ARCTIC, "Stockade needs Arctic Strike");
        assert_eq!(needed(None, Some(2)), 0, "Conflict needs nothing");
        assert_eq!(needed(None, Some(5)), 0, "Siege needs nothing");
    }

    #[test]
    fn reads_the_packs_the_client_adds_to_the_cd_key() {
        assert_eq!(from_online_key("ABCD-EFGH;packs=123"), Some(ARCTIC | KHYBER));
        assert_eq!(from_online_key("ABCD-EFGH;packs=13"), Some(ARCTIC | KHYBER));
        assert_eq!(from_online_key("ABCD-EFGH;packs=1"), Some(ARCTIC));
        assert_eq!(from_online_key("ABCD-EFGH;packs=3"), Some(KHYBER));
        assert_eq!(from_online_key("ABCD-EFGH;packs=2"), Some(0), "Raven Strike is co-op only");
        assert_eq!(from_online_key("ABCD-EFGH;packs="), Some(0), "none");
        assert_eq!(from_online_key(";packs=1"), Some(ARCTIC), "with no CD key");
        assert_eq!(from_online_key("ABCD-EFGH"), None, "an older client says nothing");
        assert_eq!(from_online_key("ABCD;packs=14"), None);
        assert_eq!(from_online_key("ABCD;packs=1111"), None);
    }

    /// Every combination of the lobby's packs, its mode and the player's packs.
    #[test]
    fn a_player_is_offered_only_lobbies_it_can_load() {
        let combos = [0, ARCTIC, KHYBER, ARCTIC | KHYBER];
        for lobby in combos {
            for mode in [None, Some(2), Some(STOCKADE), Some(TAKEOVER)] {
                let need = needed(Some(lobby), mode);
                for has in combos {
                    let can = can_join(Some(has), need);
                    let arctic = lobby & ARCTIC != 0 || mode == Some(STOCKADE);
                    let khyber = lobby & KHYBER != 0 || mode == Some(TAKEOVER);
                    let want = (!arctic || has & ARCTIC != 0) && (!khyber || has & KHYBER != 0);
                    assert_eq!(can, want, "lobby {lobby:#x} mode {mode:?} player {has:#x}");
                }
                assert!(can_join(None, need), "a player who doesn't say is offered everything");
            }
        }
    }

    #[test]
    fn the_base_game_bit_is_not_a_pack() {
        assert_eq!(needed(Some(0x4000 | 0x10 | 2), Some(2)), 0);
        assert_eq!(needed(Some(0x1_c012), Some(2)), ARCTIC | KHYBER);
    }

    #[test]
    fn remembers_players_and_their_lobbies() {
        let packs = Packs::default();
        packs.signed_in(1, Some(0));
        packs.signed_in(2, Some(ARCTIC | KHYBER));
        packs.signed_in(3, None);
        packs.hosting(2, 0x1_c012);
        assert!(!packs.offer(1, 2, Some(2)), "no packs, and the lobby uses both");
        assert!(packs.offer(3, 2, Some(2)), "unknown packs: offered");
        packs.hosting(1, 0x4012);
        assert!(packs.offer(2, 1, Some(2)), "a base-game lobby is for everyone");
        assert!(packs.offer(2, 4, Some(TAKEOVER)), "a host not heard from yet: only the mode counts");
        assert!(!packs.offer(1, 4, Some(TAKEOVER)));
        packs.signed_in(2, Some(ARCTIC));
        assert_eq!(packs.lobby(2), None, "signing in again starts with no lobby");
        assert_eq!(packs.player(2), Some(ARCTIC));
    }

    /// A search as the game makes it, against lobbies with every mix of packs: a player is offered
    /// only those it can load, and lobbies it can't load don't take the others' places.
    #[test]
    fn a_search_offers_only_lobbies_the_player_can_load() {
        use crate::sessions::Sessions;
        let sessions = Sessions::default();
        let packs = Packs::default();
        let lobby = |host: u32, category: u32, mode: u32| {
            packs.signed_in(host, Some(category & PACK_BITS));
            packs.hosting(host, category);
            sessions.create(host, 1, &[(7, 0), (3, 12), (4, 0), (MODE, mode)]).unwrap()
        };
        let both = lobby(10, 0x1_c012, 2);
        let base = lobby(11, 0x4012, 2);
        let arctic = lobby(12, 0x8012, 2);
        let khyber = lobby(13, 0x1_0012, 2);
        let search = |pid: u32, limit: usize| {
            let mut found: Vec<_> = sessions
                .find_where(1, &[(3, 1), (MODE, 2)], limit, |s| s.host != pid && packs.offer(pid, s.host, s.attribute(MODE)))
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            found.sort_unstable();
            found
        };
        let sorted = |mut v: Vec<_>| {
            v.sort_unstable();
            v
        };
        packs.signed_in(1, Some(0));
        packs.signed_in(2, Some(ARCTIC));
        packs.signed_in(3, Some(KHYBER));
        packs.signed_in(4, Some(ARCTIC | KHYBER));
        packs.signed_in(5, None);
        assert_eq!(search(1, 10), vec![base]);
        assert_eq!(search(2, 10), sorted(vec![base, arctic]));
        assert_eq!(search(3, 10), sorted(vec![base, khyber]));
        assert_eq!(search(4, 10), sorted(vec![both, base, arctic, khyber]));
        assert_eq!(search(5, 10), sorted(vec![both, base, arctic, khyber]), "an older client: as before");
        assert_eq!(search(1, 1), vec![base], "a limit of one still finds the one it can load");
        // Modes: Stockade needs Arctic Strike and Takeover Khyber Strike, whatever the map list.
        let stockade = lobby(14, 0x4102, STOCKADE);
        let takeover = lobby(15, 0x4202, TAKEOVER);
        let modes = |pid: u32, mode: u32| sessions.find_where(1, &[(3, 1), (MODE, mode)], 10, |s| packs.offer(pid, s.host, s.attribute(MODE))).len();
        assert_eq!((modes(1, STOCKADE), modes(1, TAKEOVER)), (0, 0));
        assert_eq!((modes(2, STOCKADE), modes(2, TAKEOVER)), (1, 0));
        assert_eq!((modes(3, STOCKADE), modes(3, TAKEOVER)), (0, 1));
        let _ = (stockade, takeover);
    }
}
