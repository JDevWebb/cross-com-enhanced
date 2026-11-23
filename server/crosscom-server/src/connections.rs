//! The connection records: how players' joins went, to see how often players can't reach each
//! other, and whose routers that happens with.
//!
//! The game doesn't tell the server when it can't reach a match's players: it gives up, leaves
//! the session and says "Connection to server lost" (a joiner couldn't reach its host, left 33 s
//! in). So a join is noted when it's made (the player joins, or a host adds them), with the NAT
//! probes the player asks for meanwhile, and recorded when it ends: how long the player stayed,
//! and how it ended. A join that ends within [`SHORT`] is likely a failed connection, though a
//! player backing out of a lobby looks the same; one that ends because its host had gone first
//! is recorded as that. Each record has what both players' clients said about their routers
//! (`reach`): whether they forward the game's port and how they map ports.
//!
//! A search's join can fail before it starts: the player picks a lobby and asks to reach its
//! host, can't, and gives up without the host ever adding them (the game then hosts a lobby of
//! its own). So that choice is noted too (`Sessions::choose`), and recorded as `unreached` when
//! the host hasn't added the player within [`UNREACHED_AFTER`].
//!
//! Kept in the database for [`KEPT_FOR`], with no addresses: players by account, and whether the
//! two were on one network. `crosscom-server connections [DAYS]` sums them up.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use rusqlite::params;
use rusqlite::Connection;
use slog::info;
use slog::Logger;

use crate::sessions::Key;
use crate::sessions::Sessions;

/// A join that ends sooner than this is counted as ending early.
pub const SHORT: Duration = Duration::from_secs(90);
/// A player who chose a lobby and hasn't been added to it this long after never got in.
pub const UNREACHED_AFTER: Duration = Duration::from_secs(60);
/// How long records are kept.
const KEPT_FOR: u64 = 90 * 24 * 3600;
/// Joins followed at once; an open join older than [`FORGET_AFTER`] is recorded as stayed.
const MAX_OPEN: usize = 10_000;
const FORGET_AFTER: Duration = Duration::from_secs(6 * 3600);

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// How a join ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The player left (or abandoned) the session.
    Left,
    /// The host took them out.
    Removed,
    /// Their game's connection to the server went, or it signed in afresh.
    Dropped,
}

#[derive(Debug, Clone)]
struct Open {
    at: Instant,
    host: u32,
    probes: u32,
}

pub struct Connections {
    db: Mutex<Connection>,
    open: Mutex<HashMap<(u32, Key), Open>>,
    /// Lobbies players chose from a search and haven't been added to yet: when, and the host.
    attempts: Mutex<HashMap<(u32, Key), Open>>,
    /// Each player's latest probe requests: a joiner asks to reach a match's host just before
    /// the host adds it, so they count towards the join that follows.
    probes: Mutex<HashMap<u32, Vec<Instant>>>,
}

/// How long before a join a probe request still counts towards it, and the most kept a player.
const PROBES_BEFORE: Duration = Duration::from_secs(60);
const MAX_PROBES_KEPT: usize = 16;

/// One join as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub at: u64,
    pub player: u32,
    pub host: u32,
    pub seconds: u64,
    pub probes: u32,
    /// `stayed`, `left`, `removed`, `dropped`, `host_gone` (its host had gone first), or
    /// `unreached` (it chose the lobby but the host never added it).
    pub outcome: &'static str,
    pub same_network: bool,
    pub forwarded: bool,
    pub host_forwarded: bool,
    /// What each client said about how its router maps ports, or `none` with no report.
    pub nat: String,
    pub host_nat: String,
}

impl Record {
    #[must_use]
    pub fn early(&self) -> bool {
        matches!(self.outcome, "left" | "removed" | "dropped" | "unreached")
    }
}

impl Connections {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS connection_joins (
                at INTEGER NOT NULL,
                player INTEGER NOT NULL,
                host INTEGER NOT NULL,
                seconds INTEGER NOT NULL,
                probes INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                same_network INTEGER NOT NULL,
                forwarded INTEGER NOT NULL,
                host_forwarded INTEGER NOT NULL,
                nat TEXT NOT NULL,
                host_nat TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS connection_joins_at ON connection_joins (at);",
        )?;
        Ok(Self {
            db: Mutex::new(db),
            open: Mutex::default(),
            attempts: Mutex::default(),
            probes: Mutex::default(),
        })
    }

    fn open_joins(&self) -> std::sync::MutexGuard<'_, HashMap<(u32, Key), Open>> {
        self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `pid` chose `key`, hosted by `host`, from a search, and asks to reach it.
    pub fn attempted(&self, logger: &Logger, sessions: &Sessions, pid: u32, key: Key, host: u32) {
        self.sweep(logger, sessions);
        // Already in it (a player that joins before it asks to reach the host).
        if self.open_joins().contains_key(&(pid, key)) || sessions.get(key).is_some_and(|s| s.has(pid)) {
            return;
        }
        let probes = self.recent_probes(pid);
        let mut attempts = self.attempts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if attempts.len() < MAX_OPEN {
            attempts.entry((pid, key)).or_insert(Open { at: Instant::now(), host, probes });
        }
    }

    /// Records the attempts that never got in.
    pub fn sweep(&self, logger: &Logger, sessions: &Sessions) {
        let expired: Vec<((u32, Key), Open)> = {
            let mut attempts = self.attempts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let keys: Vec<(u32, Key)> = attempts.iter().filter(|(_, o)| o.at.elapsed() >= UNREACHED_AFTER).map(|(k, _)| *k).collect();
            keys.into_iter().filter_map(|k| attempts.remove(&k).map(|v| (k, v))).collect()
        };
        for ((pid, key), attempt) in expired {
            self.record(logger, sessions, pid, key, &attempt, "unreached");
        }
    }

    /// `pid` joined `key`, hosted by `host`. A join already followed keeps its start.
    pub fn joined(&self, logger: &Logger, sessions: &Sessions, pid: u32, key: Key, host: u32) {
        if pid == host {
            return;
        }
        self.attempts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(pid, key));
        let mut open = self.open_joins();
        if open.len() >= MAX_OPEN {
            let old: Vec<(u32, Key)> = open.iter().filter(|(_, o)| o.at.elapsed() >= FORGET_AFTER).map(|(k, _)| *k).collect();
            for (player, k) in old {
                if let Some(o) = open.remove(&(player, k)) {
                    self.record(logger, sessions, player, k, &o, "stayed");
                }
            }
            if open.len() >= MAX_OPEN {
                return;
            }
        }
        let before = self.recent_probes(pid);
        open.entry((pid, key)).or_insert(Open {
            at: Instant::now(),
            host,
            probes: before,
        });
    }

    /// How many probes `pid` asked for in the last [`PROBES_BEFORE`].
    fn recent_probes(&self, pid: u32) -> u32 {
        let probes = self.probes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = probes.get(&pid).map_or(0, |p| p.iter().filter(|t| t.elapsed() < PROBES_BEFORE).count());
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// `pid` asked for NAT probes: counted on its joins still within [`SHORT`], and kept a while
    /// for a join that follows.
    pub fn probed(&self, pid: u32) {
        {
            let mut probes = self.probes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if probes.len() >= MAX_OPEN && !probes.contains_key(&pid) {
                probes.retain(|_, p| p.iter().any(|t| t.elapsed() < PROBES_BEFORE));
            }
            if probes.len() < MAX_OPEN || probes.contains_key(&pid) {
                let mine = probes.entry(pid).or_default();
                mine.retain(|t| t.elapsed() < PROBES_BEFORE);
                if mine.len() < MAX_PROBES_KEPT {
                    mine.push(Instant::now());
                }
            }
        }
        for ((player, _), o) in self.open_joins().iter_mut() {
            if *player == pid && o.at.elapsed() < SHORT {
                o.probes = o.probes.saturating_add(1);
            }
        }
        for ((player, _), o) in self.attempts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter_mut() {
            if *player == pid {
                o.probes = o.probes.saturating_add(1);
            }
        }
    }

    /// `pid`'s join of `key` ends, `how`. Called before the session forgets them.
    pub fn ended(&self, logger: &Logger, sessions: &Sessions, pid: u32, key: Key, how: End) {
        let Some(o) = self.open_joins().remove(&(pid, key)) else {
            return;
        };
        let host_there = sessions.get(key).is_some_and(|s| s.has(o.host) && s.host == o.host);
        let outcome = if o.at.elapsed() >= SHORT {
            "stayed"
        } else if !host_there {
            "host_gone"
        } else {
            match how {
                End::Left => "left",
                End::Removed => "removed",
                End::Dropped => "dropped",
            }
        };
        self.record(logger, sessions, pid, key, &o, outcome);
    }

    /// `pid`'s game is gone: each of its joins ends.
    pub fn gone(&self, logger: &Logger, sessions: &Sessions, pid: u32) {
        let keys: Vec<Key> = self.open_joins().keys().filter(|(p, _)| *p == pid).map(|(_, k)| *k).collect();
        for key in keys {
            self.ended(logger, sessions, pid, key, End::Dropped);
        }
    }

    fn record(&self, logger: &Logger, sessions: &Sessions, pid: u32, key: Key, o: &Open, outcome: &'static str) {
        let nat_of = |p: u32| sessions.reachability(p).map_or_else(|| "none".to_owned(), |r| r.nat);
        let record = Record {
            at: now(),
            player: pid,
            host: o.host,
            seconds: o.at.elapsed().as_secs(),
            probes: o.probes,
            outcome,
            same_network: sessions.seen(pid).is_some() && sessions.seen(pid) == sessions.seen(o.host),
            forwarded: sessions.forward_of(pid).is_some(),
            host_forwarded: sessions.forward_of(o.host).is_some(),
            nat: nat_of(pid),
            host_nat: nat_of(o.host),
        };
        if record.early() {
            info!(
                logger,
                "{pid:#x}'s join of {key:?} ended after {} s: {outcome}", record.seconds;
                "host" => format!("{:#x}", o.host), "probes" => o.probes, "same_network" => record.same_network,
                "forwarded" => record.forwarded, "host_forwarded" => record.host_forwarded,
                "nat" => &record.nat, "host_nat" => &record.host_nat
            );
        }
        if let Err(e) = self.store(&record) {
            slog::warn!(logger, "couldn't keep a connection record: {e}");
        }
    }

    fn store(&self, r: &Record) -> rusqlite::Result<()> {
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        db.execute("DELETE FROM connection_joins WHERE at < ?1", params![r.at.saturating_sub(KEPT_FOR)])?;
        db.execute(
            "INSERT INTO connection_joins (at, player, host, seconds, probes, outcome, same_network, forwarded, host_forwarded, nat, host_nat)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                r.at,
                r.player,
                r.host,
                r.seconds,
                r.probes,
                r.outcome,
                r.same_network,
                r.forwarded,
                r.host_forwarded,
                r.nat,
                r.host_nat
            ],
        )?;
        Ok(())
    }

    /// The records since `since` (Unix time), oldest first.
    pub fn since(&self, since: u64) -> rusqlite::Result<Vec<Record>> {
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut statement = db.prepare(
            "SELECT at, player, host, seconds, probes, outcome, same_network, forwarded, host_forwarded, nat, host_nat
             FROM connection_joins WHERE at >= ?1 ORDER BY at",
        )?;
        let rows = statement.query_map(params![since], |row| {
            let outcome: String = row.get(5)?;
            Ok(Record {
                at: row.get(0)?,
                player: row.get(1)?,
                host: row.get(2)?,
                seconds: row.get(3)?,
                probes: row.get(4)?,
                outcome: ["stayed", "left", "removed", "dropped", "host_gone", "unreached"]
                    .into_iter()
                    .find(|o| *o == outcome)
                    .unwrap_or("stayed"),
                same_network: row.get(6)?,
                forwarded: row.get(7)?,
                host_forwarded: row.get(8)?,
                nat: row.get(9)?,
                host_nat: row.get(10)?,
            })
        })?;
        rows.collect()
    }
}

/// The server's records and its sessions, once it's serving (none in tests and commands).
static SERVING: OnceLock<(Connections, Arc<Sessions>)> = OnceLock::new();

/// Starts keeping records of the joins of `sessions`, and a thread recording the attempts that
/// never got in.
pub fn start(logger: &Logger, connections: Connections, sessions: Arc<Sessions>) {
    if SERVING.set((connections, sessions)).is_err() {
        return;
    }
    let sweeping = logger.clone();
    let swept = std::thread::Builder::new().name("connection records".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_secs(15));
        if let Some((c, s)) = SERVING.get() {
            c.sweep(&sweeping, s);
        }
    });
    if let Err(e) = swept {
        slog::warn!(logger, "couldn't start recording joins that never got in: {e}");
    }
}

/// [`Connections::attempted`], while serving.
pub fn attempted(logger: &Logger, pid: u32, key: Key, host: u32) {
    if let Some((c, s)) = SERVING.get() {
        c.attempted(logger, s, pid, key, host);
    }
}

/// [`Connections::joined`], while serving.
pub fn joined(logger: &Logger, pid: u32, key: Key, host: u32) {
    if let Some((c, s)) = SERVING.get() {
        c.joined(logger, s, pid, key, host);
    }
}

/// [`Connections::probed`], while serving.
pub fn probed(pid: u32) {
    if let Some((c, _)) = SERVING.get() {
        c.probed(pid);
    }
}

/// [`Connections::ended`], while serving.
pub fn ended(logger: &Logger, pid: u32, key: Key, how: End) {
    if let Some((c, s)) = SERVING.get() {
        c.ended(logger, s, pid, key, how);
    }
}

/// [`Connections::gone`], while serving.
pub fn gone(logger: &Logger, pid: u32) {
    if let Some((c, s)) = SERVING.get() {
        c.gone(logger, s, pid);
    }
}

/// The router a record's player had, as a row of the summary.
fn router(forwarded: bool, nat: &str) -> &'static str {
    match (forwarded, nat) {
        (true, _) => "port forwarded",
        (false, "consistent") => "no forward, consistent NAT",
        (false, "symmetric") => "no forward, symmetric NAT",
        (false, "none") => "no report (older client, or forwarding off)",
        _ => "no forward, NAT unknown",
    }
}

/// The two players' routers together, as a row of the summary.
fn pair(r: &Record) -> &'static str {
    if r.same_network {
        return "on one network";
    }
    if r.forwarded || r.host_forwarded {
        return "either forwarded";
    }
    match (r.nat == "symmetric", r.host_nat == "symmetric") {
        (true, true) => "both symmetric",
        (true, false) | (false, true) => "one symmetric",
        _ if r.nat == "none" || r.host_nat == "none" => "a report missing",
        _ => "neither symmetric",
    }
}

/// `crosscom-server connections`: the records of the last `days` days summed up, with players'
/// names from `name`.
#[must_use]
pub fn summary(records: &[Record], days: u64, name: impl Fn(u32) -> String) -> String {
    let mut out = String::new();
    let counted: Vec<&Record> = records.iter().filter(|r| r.outcome != "host_gone").collect();
    let early = counted.iter().filter(|r| r.early()).count();
    let pct = |n: usize, of: usize| if of == 0 { 0.0 } else { n as f64 * 100.0 / of as f64 };
    let _ = writeln!(out, "Joins in the last {days} days: {}", counted.len());
    let _ = writeln!(out, "  failed or ended within {} s: {early} ({:.0}%)", SHORT.as_secs(), pct(early, counted.len()));
    for (outcome, said) in [
        ("unreached", "never got in: the host never added them"),
        ("left", "the player left"),
        ("removed", "the host took them out"),
        ("dropped", "their game lost the server"),
    ] {
        let n = counted.iter().filter(|r| r.outcome == outcome).count();
        let _ = writeln!(out, "    {said}: {n}");
    }
    let host_gone = records.len() - counted.len();
    let _ = writeln!(out, "  (and {host_gone} ended because the host had gone, not counted)");
    let table = |out: &mut String, title: &str, key: &dyn Fn(&Record) -> &'static str| {
        let mut rows: Vec<(&'static str, usize, usize)> = Vec::new();
        for r in &counted {
            let k = key(r);
            match rows.iter_mut().find(|(name, _, _)| *name == k) {
                Some(row) => {
                    row.1 += 1;
                    row.2 += usize::from(r.early());
                }
                None => rows.push((k, 1, usize::from(r.early()))),
            }
        }
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        let _ = writeln!(out, "\n{title:<46} {:>6} {:>12}", "joins", "ended early");
        for (k, n, e) in rows {
            let _ = writeln!(out, "  {k:<44} {n:>6} {e:>6} ({:>3.0}%)", pct(e, n));
        }
    };
    table(&mut out, "By the joining player's router", &|r| router(r.forwarded, &r.nat));
    table(&mut out, "By both players' routers", &pair);
    let mut by_player: HashMap<u32, (usize, usize)> = HashMap::new();
    for r in &counted {
        let entry = by_player.entry(r.player).or_default();
        entry.0 += 1;
        entry.1 += usize::from(r.early());
    }
    let mut players: Vec<(u32, usize, usize)> = by_player.into_iter().filter(|(_, (_, e))| *e > 0).map(|(p, (n, e))| (p, n, e)).collect();
    players.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
    if !players.is_empty() {
        let _ = writeln!(out, "\nPlayers whose joins ended early most often:");
        for (pid, n, e) in players.into_iter().take(10) {
            let _ = writeln!(out, "  {:<24} {e} of {n}", name(pid));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use quazal::rmc::types::StationURL;

    use super::*;

    fn logger() -> Logger {
        Logger::root(slog::Discard, slog::o!())
    }

    fn setup() -> (tempfile::TempDir, Connections, Sessions, Key) {
        let dir = tempfile::tempdir().unwrap();
        let connections = Connections::open(&dir.path().join("db.sqlite")).unwrap();
        let sessions = Sessions::default();
        let url = |a: &str| format!("prudp:/address={a};port=2347").parse::<StationURL>().unwrap();
        sessions.set_urls(1, url("198.51.100.7"), vec![]);
        sessions.set_urls(2, url("203.0.113.9"), vec![]);
        let key = sessions.create(1, 1, &[]).unwrap();
        sessions.add(key, &[2], &[]).unwrap();
        (dir, connections, sessions, key)
    }

    #[test]
    fn a_join_that_ends_soon_is_recorded_as_early() {
        let (_dir, c, sessions, key) = setup();
        c.joined(&logger(), &sessions, 2, key, 1);
        c.probed(2);
        c.probed(2);
        c.ended(&logger(), &sessions, 2, key, End::Left);
        let records = c.since(0).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!((r.player, r.host, r.probes, r.outcome, r.same_network), (2, 1, 2, "left", false));
        assert_eq!((r.nat.as_str(), r.forwarded), ("none", false));
        assert!(r.early());
        c.ended(&logger(), &sessions, 2, key, End::Left);
        assert_eq!(c.since(0).unwrap().len(), 1, "once");
    }

    /// A joiner asks to reach the host just before the host adds it (as Quick Match does).
    #[test]
    fn probes_just_before_a_join_count_towards_it() {
        let (_dir, c, sessions, key) = setup();
        c.probed(2);
        c.joined(&logger(), &sessions, 2, key, 1);
        c.probed(2);
        c.ended(&logger(), &sessions, 2, key, End::Left);
        assert_eq!(c.since(0).unwrap()[0].probes, 2, "one just before the join, one while it was open");
    }

    /// A player who chose a lobby but was never added is recorded once the wait is over; one who
    /// was added isn't.
    #[test]
    fn an_attempt_that_never_got_in_is_recorded() {
        let (_dir, c, sessions, key) = setup();
        c.probed(3);
        c.attempted(&logger(), &sessions, 3, key, 1);
        // Player 2 is in the session already (setup added it): its probe isn't an attempt.
        c.attempted(&logger(), &sessions, 2, key, 1);
        c.sweep(&logger(), &sessions);
        assert!(c.since(0).unwrap().is_empty(), "not yet");
        c.probed(3);
        for v in c.attempts.lock().unwrap().values_mut() {
            v.at -= UNREACHED_AFTER;
        }
        c.sweep(&logger(), &sessions);
        let records = c.since(0).unwrap();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(
            (records[0].player, records[0].outcome, records[0].probes),
            (3, "unreached", 2),
            "one before, one while waiting"
        );
        assert!(records[0].early());
    }

    #[test]
    fn a_join_whose_host_went_first_isnt_counted_as_failing() {
        let (_dir, c, sessions, key) = setup();
        c.joined(&logger(), &sessions, 2, key, 1);
        sessions.remove(key, &[1]).unwrap();
        c.ended(&logger(), &sessions, 2, key, End::Left);
        assert_eq!(c.since(0).unwrap()[0].outcome, "host_gone");
    }

    #[test]
    fn a_game_that_goes_ends_its_joins() {
        let (_dir, c, sessions, key) = setup();
        c.joined(&logger(), &sessions, 2, key, 1);
        c.joined(&logger(), &sessions, 1, key, 1);
        c.gone(&logger(), &sessions, 2);
        let records = c.since(0).unwrap();
        assert_eq!(records.len(), 1, "the host's own session isn't a join");
        assert_eq!(records[0].outcome, "dropped");
    }

    #[test]
    fn the_summary_sums_by_router() {
        let record = |outcome: &'static str, forwarded: bool, nat: &str, host_nat: &str| Record {
            at: 0,
            player: 2,
            host: 1,
            seconds: 30,
            probes: 1,
            outcome,
            same_network: false,
            forwarded,
            host_forwarded: false,
            nat: nat.into(),
            host_nat: host_nat.into(),
        };
        let records = [
            record("left", false, "symmetric", "symmetric"),
            record("stayed", true, "symmetric", "consistent"),
            record("stayed", false, "consistent", "consistent"),
            record("host_gone", false, "consistent", "consistent"),
        ];
        let text = summary(&records, 30, |p| format!("player{p}"));
        assert!(text.contains("Joins in the last 30 days: 3"), "{text}");
        assert!(text.contains("failed or ended within 90 s: 1 (33%)"), "{text}");
        assert!(text.contains("(and 1 ended because the host had gone"), "{text}");
        assert!(text.contains("both symmetric"), "{text}");
        assert!(text.contains("player2"), "{text}");
    }
}
