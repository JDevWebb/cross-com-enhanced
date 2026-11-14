//! The server's leaderboard, for the overlay and the launcher: `GET /api/leaderboard?mode=...`.
//!
//! It's made from the numbers the game keeps for its own Leaderboards screen: after each match
//! the game saves board 10 with a row per mode it has played, keyed by the mode (1 Stockade,
//! 2 Conflict, 3 Decoy, 4 Saboteur, 5 Siege, 6 Takeover; rows keyed 12 or more aren't modes),
//! columns 1 time played (seconds), 2 matches played, 3 matches won, 7 kills, 8 deaths and
//! 9 objectives. The game's screen ranks only a player's friends; this ranks every player.
//!
//! The answer: `{"mode": "conflict", "modes": [...], "rows": [{"rank", "name", "wins", "losses",
//! "kills", "deaths", "objectives", "seconds"}]}`, best first (most wins, then kills, then
//! objectives), at most [`MAX_ROWS`]. `mode` is one of [`MODES`]; `all` adds the modes up.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde::Serialize;

pub const PATH: &str = "/api/leaderboard";
/// The board the game keeps its leaderboard numbers on.
pub const BOARD: u8 = 10;
/// The modes asked for by name, and the key each is kept under (`all`: every mode).
pub const MODES: [(&str, Option<u32>); 7] = [
    ("all", None),
    ("conflict", Some(2)),
    ("decoy", Some(3)),
    ("saboteur", Some(4)),
    ("siege", Some(5)),
    ("stockade", Some(1)),
    ("takeover", Some(6)),
];
/// Keys from here on aren't modes (the game skips them too).
const FIRST_NOT_A_MODE: u32 = 12;
/// The most players listed.
pub const MAX_ROWS: usize = 100;
/// How long one mode's table is kept before it's made again.
const KEEP_FOR: Duration = Duration::from_secs(30);

const TIME: u8 = 1;
const PLAYED: u8 = 2;
const WON: u8 = 3;
const KILLS: u8 = 7;
const DEATHS: u8 = 8;
const OBJECTIVES: u8 = 9;

/// One stored number: player, keys, column, value (as stored: a little-endian i32).
pub type Stored = (u32, Vec<u32>, u8, Vec<u8>);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub rank: usize,
    pub name: String,
    pub wins: u32,
    pub losses: u32,
    pub kills: u32,
    pub deaths: u32,
    pub objectives: u32,
    pub seconds: u32,
}

#[derive(Serialize)]
struct Answer<'a> {
    mode: &'a str,
    modes: Vec<&'static str>,
    rows: Vec<Entry>,
}

/// The mode a query (`mode=conflict`) asks for, by its place in [`MODES`]; `all` without one.
#[must_use]
pub fn mode_of(query: &str) -> Option<usize> {
    let asked = query.split('&').find_map(|p| p.strip_prefix("mode=")).unwrap_or("all");
    MODES.iter().position(|(name, _)| name.eq_ignore_ascii_case(asked))
}

/// The ranked table for `mode` (a key, or every mode) from the stored numbers, naming each
/// player with `name` (players without a name are left out).
pub fn table(stored: &[Stored], mode: Option<u32>, name: impl Fn(u32) -> Option<String>) -> Vec<Entry> {
    #[derive(Default)]
    struct Sums {
        played: u32,
        won: u32,
        kills: u32,
        deaths: u32,
        objectives: u32,
        seconds: u32,
    }
    let mut players: HashMap<u32, Sums> = HashMap::new();
    for (pid, keys, column, value) in stored {
        let [key] = keys.as_slice() else { continue };
        if *key >= FIRST_NOT_A_MODE || mode.is_some_and(|m| m != *key) {
            continue;
        }
        let Ok(bytes) = <[u8; 4]>::try_from(value.as_slice()) else { continue };
        let value = u32::try_from(i32::from_le_bytes(bytes)).unwrap_or(0);
        let sums = players.entry(*pid).or_default();
        let field = match *column {
            TIME => &mut sums.seconds,
            PLAYED => &mut sums.played,
            WON => &mut sums.won,
            KILLS => &mut sums.kills,
            DEATHS => &mut sums.deaths,
            OBJECTIVES => &mut sums.objectives,
            _ => continue,
        };
        *field = field.saturating_add(value);
    }
    let mut rows: Vec<Entry> = players
        .into_iter()
        .filter(|(_, s)| s.played > 0 || s.seconds > 0)
        .filter_map(|(pid, s)| {
            Some(Entry {
                rank: 0,
                name: name(pid)?,
                wins: s.won,
                losses: s.played.saturating_sub(s.won),
                kills: s.kills,
                deaths: s.deaths,
                objectives: s.objectives,
                seconds: s.seconds,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        (b.wins, b.kills, b.objectives)
            .cmp(&(a.wins, a.kills, a.objectives))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows.truncate(MAX_ROWS);
    for (i, row) in rows.iter_mut().enumerate() {
        row.rank = i + 1;
    }
    rows
}

/// The JSON answer for the mode at `mode` in [`MODES`].
#[must_use]
pub fn answer(mode: usize, rows: Vec<Entry>) -> String {
    let answer = Answer {
        mode: MODES[mode].0,
        modes: MODES.iter().map(|(name, _)| *name).collect(),
        rows,
    };
    serde_json::to_string(&answer).unwrap_or_default()
}

/// Answers kept for a while, so a burst of requests reads the database once.
#[derive(Default)]
pub struct Cache(Mutex<HashMap<usize, (Instant, String)>>);

impl Cache {
    /// `mode`'s answer, made with `make` when there's none fresh.
    pub fn get(&self, mode: usize, make: impl FnOnce() -> Option<String>) -> Option<String> {
        let now = Instant::now();
        if let Some((at, json)) = self.0.lock().ok()?.get(&mode) {
            if now.duration_since(*at) < KEEP_FOR {
                return Some(json.clone());
            }
        }
        let json = make()?;
        if let Ok(mut cache) = self.0.lock() {
            cache.insert(mode, (now, json.clone()));
        }
        Some(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(pid: u32, mode: u32, column: u8, value: i32) -> Stored {
        (pid, vec![mode], column, value.to_le_bytes().to_vec())
    }

    #[test]
    fn ranks_by_wins_then_kills_for_a_mode_or_all() {
        let stored = vec![
            n(1, 2, PLAYED, 3),
            n(1, 2, WON, 3),
            n(1, 2, TIME, 1140),
            n(2, 2, PLAYED, 4),
            n(2, 2, WON, 3),
            n(2, 2, KILLS, 9),
            n(2, 5, PLAYED, 2),
            n(2, 5, WON, 2),
            n(3, 2, PLAYED, 2),
            n(3, 2, WON, 1),
            n(3, 0xffff_ffff, PLAYED, 50), // not a mode
            n(4, 2, PLAYED, 1),            // no account name
            (5, vec![2, 1], PLAYED, 7i32.to_le_bytes().to_vec()),
        ];
        let name = |pid: u32| (pid != 4).then(|| format!("p{pid}"));
        let conflict = table(&stored, Some(2), name);
        assert_eq!(
            conflict.iter().map(|e| (e.rank, e.name.as_str(), e.wins, e.losses)).collect::<Vec<_>>(),
            vec![(1, "p2", 3, 1), (2, "p1", 3, 0), (3, "p3", 1, 1)]
        );
        assert_eq!(conflict[1].seconds, 1140);
        let all = table(&stored, None, name);
        assert_eq!((all[0].name.as_str(), all[0].wins, all[0].losses), ("p2", 5, 1));
        assert!(table(&stored, Some(6), name).is_empty());
    }

    #[test]
    fn reads_the_mode_asked() {
        assert_eq!(mode_of(""), Some(0));
        assert_eq!(mode_of("mode=Siege"), Some(4));
        assert_eq!(mode_of("x=1&mode=takeover"), Some(6));
        assert_eq!(mode_of("mode=guerrilla"), None);
        let json: serde_json::Value = serde_json::from_str(&answer(1, vec![Entry::default()])).unwrap();
        assert_eq!(json["mode"], "conflict");
        assert_eq!(json["modes"].as_array().unwrap().len(), MODES.len());
        assert_eq!(json["rows"][0]["wins"], 0);
    }

    #[test]
    fn keeps_an_answer_for_a_while() {
        let cache = Cache::default();
        assert_eq!(cache.get(1, || Some("a".into())), Some("a".into()));
        assert_eq!(cache.get(1, || Some("b".into())), Some("a".into()));
        assert_eq!(cache.get(2, || None), None);
    }
}
