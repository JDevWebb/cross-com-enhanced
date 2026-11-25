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
//!
//! The game writes these numbers itself (setting them, op 2), so a modified game could write
//! anything. They're kept sane as they're written ([`sane_write`]: never negative, and growing by
//! at most a few matches' worth at once), and a player's numbers for a mode that couldn't be
//! (more wins than matches, more kills or time than that many matches could bring) are left out
//! of the ranking ([`plausible`]), not refused: the game expects its writes to succeed.

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

/// The most one match could bring a player, generously: kills, deaths, objectives and seconds.
/// A player's numbers for a mode beyond this many times the matches they played (and one more:
/// time and kills in a match they left early count, the match doesn't) aren't ranked.
const MAX_KILLS_PER_MATCH: u32 = 200;
const MAX_DEATHS_PER_MATCH: u32 = 200;
const MAX_OBJECTIVES_PER_MATCH: u32 = 100;
const MAX_SECONDS_PER_MATCH: u32 = 3 * 3600;
/// Matches one write may add, at most: the game saves after each match, so this is a game that
/// couldn't save for a while catching up.
const MAX_MATCHES_PER_WRITE: i32 = 20;

/// Whether a stored number is one of the leaderboard's: on its board, keyed by a mode, in one of
/// its columns.
#[must_use]
pub fn ranked(board: u8, keys: &[u32], column: u8) -> bool {
    board == BOARD && matches!(keys, [key] if *key < FIRST_NOT_A_MODE) && [TIME, PLAYED, WON, KILLS, DEATHS, OBJECTIVES].contains(&column)
}

/// One match's worth of a leaderboard column.
#[allow(clippy::cast_possible_wrap)] // the per-match limits are small
fn per_match(column: u8) -> i32 {
    (match column {
        KILLS => MAX_KILLS_PER_MATCH,
        DEATHS => MAX_DEATHS_PER_MATCH,
        OBJECTIVES => MAX_OBJECTIVES_PER_MATCH,
        TIME => MAX_SECONDS_PER_MATCH,
        _ => 1,
    }) as i32
}

/// The most a column may grow in one go: [`MAX_MATCHES_PER_WRITE`] matches' worth.
fn max_growth(column: u8) -> i32 {
    MAX_MATCHES_PER_WRITE.saturating_mul(per_match(column))
}

/// How often a game can finish a match: one match's worth of a column comes back to a player's
/// allowance this often.
const MATCH_EVERY: Duration = Duration::from_secs(120);
/// Allowances kept before the stale ones are dropped (a dropped one starts full again).
const MAX_ALLOWANCES: usize = 200_000;

/// How much each player's leaderboard columns may still grow: a bucket per player, mode and
/// column holding up to [`max_growth`], refilling a match's worth every [`MATCH_EVERY`]. A game
/// saves after each match, minutes apart; writing over and over can't add more than real
/// matches could. Kept in memory: a restart fills them again, which is no more than a game
/// catching up.
#[derive(Default)]
pub struct Growth {
    allowances: Mutex<HashMap<AllowanceKey, Allowance>>,
}

/// Whose allowance: the player, the stored row's keys (its mode) and the column.
type AllowanceKey = (u32, Vec<u8>, u8);
/// What's left of it, and when it was last refilled.
type Allowance = (i64, Instant);

impl Growth {
    /// Grants up to `wanted` growth of `column` for `pid` under `keys` (its mode), from what's
    /// left of its allowance at `now`.
    pub fn take(&self, pid: u32, keys: &[u8], column: u8, wanted: i32, now: Instant) -> i32 {
        let cap = i64::from(max_growth(column));
        let mut all = self.allowances.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if all.len() >= MAX_ALLOWANCES {
            all.retain(|_, (_, at)| now.saturating_duration_since(*at) < MATCH_EVERY * 20);
            if all.len() >= MAX_ALLOWANCES {
                all.clear();
            }
        }
        let (left, at) = all.entry((pid, keys.to_vec(), column)).or_insert((cap, now));
        let matches = i64::try_from(now.saturating_duration_since(*at).as_secs() / MATCH_EVERY.as_secs()).unwrap_or(i64::MAX);
        if matches > 0 {
            *left = left.saturating_add(matches.saturating_mul(i64::from(per_match(column)))).min(cap);
            *at = now;
        }
        let granted = i64::from(wanted.max(0)).min(*left);
        *left -= granted;
        i32::try_from(granted).unwrap_or(0)
    }
}

/// A leaderboard number (see [`ranked`]) as it's kept when the game writes `new` (a little-endian
/// i32) over `stored`: never negative, and growing only by what `grant` allows of the growth
/// asked for (see [`Growth`]). Nothing stored counts as zero: a new player's first write can't
/// put them anywhere on the board.
#[must_use]
pub fn sane_write(stored: Option<&[u8]>, new: &[u8], grant: impl FnOnce(i32) -> i32) -> Vec<u8> {
    let Ok(bytes) = <[u8; 4]>::try_from(new) else {
        return new.to_vec();
    };
    let value = i32::from_le_bytes(bytes).max(0);
    let old = stored.and_then(|s| <[u8; 4]>::try_from(s).ok()).map_or(0, |s| i32::from_le_bytes(s).max(0));
    let value = if value > old { old.saturating_add(grant(value - old).min(value - old)) } else { value };
    value.to_le_bytes().to_vec()
}

/// A player's numbers for one mode.
#[derive(Default)]
struct Sums {
    played: u32,
    won: u32,
    kills: u32,
    deaths: u32,
    objectives: u32,
    seconds: u32,
}

/// Whether a player's numbers for a mode could be: no more wins than matches, and no more kills,
/// deaths, objectives or time than their matches (and one left early) could bring.
fn plausible(s: &Sums) -> bool {
    let matches = s.played.saturating_add(1);
    s.won <= s.played
        && s.kills <= matches.saturating_mul(MAX_KILLS_PER_MATCH)
        && s.deaths <= matches.saturating_mul(MAX_DEATHS_PER_MATCH)
        && s.objectives <= matches.saturating_mul(MAX_OBJECTIVES_PER_MATCH)
        && s.seconds <= matches.saturating_mul(MAX_SECONDS_PER_MATCH)
}

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
/// player with `name` (players without a name are left out). A player's numbers for a mode that
/// couldn't be (see [`plausible`]) aren't counted.
pub fn table(stored: &[Stored], mode: Option<u32>, name: impl Fn(u32) -> Option<String>) -> Vec<Entry> {
    let mut modes: HashMap<(u32, u32), Sums> = HashMap::new();
    for (pid, keys, column, value) in stored {
        let [key] = keys.as_slice() else { continue };
        if *key >= FIRST_NOT_A_MODE || mode.is_some_and(|m| m != *key) {
            continue;
        }
        let Ok(bytes) = <[u8; 4]>::try_from(value.as_slice()) else { continue };
        let value = u32::try_from(i32::from_le_bytes(bytes)).unwrap_or(0);
        let sums = modes.entry((*pid, *key)).or_default();
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
    let mut players: HashMap<u32, Sums> = HashMap::new();
    for ((pid, _), mode) in modes.into_iter().filter(|(_, s)| plausible(s)) {
        let sums = players.entry(pid).or_default();
        for (into, from) in [
            (&mut sums.played, mode.played),
            (&mut sums.won, mode.won),
            (&mut sums.kills, mode.kills),
            (&mut sums.deaths, mode.deaths),
            (&mut sums.objectives, mode.objectives),
            (&mut sums.seconds, mode.seconds),
        ] {
            *into = into.saturating_add(from);
        }
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
    /// `mode`'s answer, made with `make` when there's none fresh. The lock is held while it's
    /// made, so requests at once wait for that one instead of each reading the database.
    pub fn get(&self, mode: usize, make: impl FnOnce() -> Option<String>) -> Option<String> {
        let now = Instant::now();
        let mut cache = self.0.lock().ok()?;
        if let Some((at, json)) = cache.get(&mode) {
            if now.duration_since(*at) < KEEP_FOR {
                return Some(json.clone());
            }
        }
        let json = make()?;
        cache.insert(mode, (now, json.clone()));
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

    /// A mode's numbers that couldn't be are left out of the ranking (that mode's only).
    #[test]
    fn implausible_numbers_are_not_ranked() {
        let stored = vec![
            n(1, 2, PLAYED, 2),
            n(1, 2, WON, 5), // more wins than matches
            n(1, 3, PLAYED, 1),
            n(1, 3, WON, 1),
            n(2, 2, PLAYED, 1),
            n(2, 2, KILLS, 100_000), // more kills than a match could bring
            n(3, 2, PLAYED, 10),
            n(3, 2, WON, 4),
            n(3, 2, KILLS, 150),
            n(3, 2, TIME, 7200),
            n(4, 2, TIME, 600), // a match left early: time, no match
            n(5, 2, PLAYED, -3),
            n(5, 2, WON, -1),
        ];
        let name = |pid: u32| Some(format!("p{pid}"));
        let conflict = table(&stored, Some(2), name);
        assert_eq!(conflict.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["p3", "p4"]);
        let all = table(&stored, None, name);
        let p1 = all.iter().find(|e| e.name == "p1").unwrap();
        assert_eq!((p1.wins, p1.losses), (1, 0), "its other mode still counts");
    }

    /// As they're written: never negative, growing only by what the allowance grants, a first
    /// write counting from zero; and the allowance refills only with time.
    #[test]
    fn leaderboard_numbers_are_kept_sane_as_written() {
        let v = |n: i32| n.to_le_bytes().to_vec();
        let all = |g: i32| g;
        assert_eq!(sane_write(None, &v(-5), all), v(0));
        assert_eq!(sane_write(None, &v(300), |g| g.min(20)), v(20), "a first write counts from zero");
        assert_eq!(sane_write(Some(&v(300)), &v(301), all), v(301));
        assert_eq!(sane_write(Some(&v(10)), &v(3), |_| 0), v(3), "lower: as written");
        assert_eq!(sane_write(Some(&v(10)), &[1, 2], all), vec![1, 2], "not a 32-bit value: as it is");
        let growth = Growth::default();
        let start = Instant::now();
        let keys = [2u8, 0, 0, 0];
        assert_eq!(growth.take(1, &keys, WON, 1_000_000, start), MAX_MATCHES_PER_WRITE, "a full allowance");
        assert_eq!(growth.take(1, &keys, WON, 5, start), 0, "used up: writing again adds nothing");
        assert_eq!(growth.take(1, &keys, WON, 5, start + MATCH_EVERY * 3), 3, "a match's worth comes back per MATCH_EVERY");
        assert_eq!(growth.take(2, &keys, WON, 5, start), 5, "another player's own");
        assert!(ranked(BOARD, &[2], WON) && !ranked(BOARD, &[12], WON) && !ranked(BOARD, &[2], 4) && !ranked(3, &[2], WON));
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
