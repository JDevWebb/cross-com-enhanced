//! Multiplayer Challenges ("dares"): a player challenges a friend to beat one of their numbers.
//!
//! The challenges themselves are the game's (23 kinds of number, built in). A dare's value is
//! packed in a u32: byte 0 how often it has been passed back, byte 1 which number (0-22), the top
//! 16 bits the value to beat. The game sends one as an event on protocol 5001 (method 1): the
//! event's name, then named string parameters,
//!
//! ```text
//! request: name: string, list<{ key: string, value: Variant }>
//! Variant: u8 type, then 0 nothing, 1 i64, 2 f64, 3 bool (u8), 4 string, 5 date-time (u64), 6 u64
//! ```
//!
//! a dare being "GDE" with `targetPID`, `dareId` (the sender's own PID) and `score` (the packed
//! value). "MPP" sends strings; numbers may come as any of the number types, so each value is
//! read as text. Other events (such as "MPP" after a ranked match) are only answered.
//!
//! Protocol 5008 hands them out: RequestDareInfo (1) answers the server's time and the caller's
//! dares, each `{ sender: u32, u32, sent: date-time, value: u32, id: u32, u32 }` (the game reads
//! these four); DeleteDare (2) takes one dare's id, which the game does once a dare is a week
//! old or its sender is no longer a friend.
//!
//! Dares are kept only between friends, one per sender and friend (a newer one replaces it), at
//! most [`MAX_PER_PLAYER`] per player, for [`KEPT_FOR`].

use std::io::Cursor;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use byteorder::LittleEndian;
use byteorder::ReadBytesExt;
use quazal::prudp::ClientRegistry;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::rmc::Request;
use quazal::ClientInfo;
use quazal::Context;
use rusqlite::params;
use rusqlite::Connection;
use slog::info;
use slog::Logger;

use crate::fixed::packed_date_time;
use crate::friends::Friends;

/// How long a dare is kept: the game drops older ones itself.
const KEPT_FOR: u64 = 7 * 24 * 3600;
/// The most dares one player is kept (the oldest go first).
const MAX_PER_PLAYER: i64 = 32;
/// The longest event name or parameter read, and the most parameters.
const MAX_STRING: usize = 256;
const MAX_PARAMETERS: u32 = 32;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// One dare, as kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dare {
    pub id: u32,
    pub sender: u32,
    /// When it was sent, in Unix seconds.
    pub sent: u64,
    pub value: u32,
}

pub struct Store {
    db: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::with(Connection::open(path)?)
    }

    #[cfg(test)]
    fn in_memory() -> rusqlite::Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> rusqlite::Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS dares (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                target INTEGER NOT NULL,
                sender INTEGER NOT NULL,
                sent INTEGER NOT NULL,
                value INTEGER NOT NULL,
                UNIQUE (target, sender)
            );",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keeps `sender`'s dare to `target` (replacing an earlier one), sent at `at`.
    pub fn send(&self, sender: u32, target: u32, value: u32, at: u64) -> rusqlite::Result<()> {
        let db = self.lock();
        db.execute(
            "INSERT INTO dares (target, sender, sent, value) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (target, sender) DO UPDATE SET sent = excluded.sent, value = excluded.value, id = (SELECT COALESCE(MAX(id), 0) + 1 FROM dares)",
            params![target, sender, at, value],
        )?;
        db.execute(
            "DELETE FROM dares WHERE target = ?1 AND id NOT IN (SELECT id FROM dares WHERE target = ?1 ORDER BY sent DESC, id DESC LIMIT ?2)",
            params![target, MAX_PER_PLAYER],
        )?;
        Ok(())
    }

    /// `target`'s dares sent since `since` (Unix seconds); older ones are dropped.
    pub fn dares(&self, target: u32, since: u64) -> rusqlite::Result<Vec<Dare>> {
        let db = self.lock();
        db.execute("DELETE FROM dares WHERE sent < ?1", params![since])?;
        let mut query = db.prepare_cached("SELECT id, sender, sent, value FROM dares WHERE target = ?1 ORDER BY sent, id")?;
        let rows = query.query_map(params![target], |r| {
            Ok(Dare {
                id: r.get(0)?,
                sender: r.get(1)?,
                sent: r.get(2)?,
                value: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Drops one of `target`'s own dares.
    pub fn delete(&self, target: u32, id: u32) -> rusqlite::Result<bool> {
        Ok(self.lock().execute("DELETE FROM dares WHERE target = ?1 AND id = ?2", params![target, id])? > 0)
    }
}

fn read_string(r: &mut Cursor<&[u8]>) -> std::io::Result<String> {
    let len = usize::from(r.read_u16::<LittleEndian>()?);
    if len > MAX_STRING {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; len];
    r.read_exact(&mut bytes)?;
    Ok(String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(&bytes)).into_owned())
}

/// An event's name and its parameters, each value as text (a type not known ends the list read).
fn parse_event(parameters: &[u8]) -> std::io::Result<(String, Vec<(String, String)>)> {
    let mut r = Cursor::new(parameters);
    let name = read_string(&mut r)?;
    let count = r.read_u32::<LittleEndian>()?;
    let mut values = Vec::new();
    for _ in 0..count.min(MAX_PARAMETERS) {
        let key = read_string(&mut r)?;
        let value = match r.read_u8()? {
            0 => String::new(),
            1 => r.read_i64::<LittleEndian>()?.to_string(),
            2 => r.read_f64::<LittleEndian>()?.to_string(),
            3 => (r.read_u8()? != 0).to_string(),
            4 => read_string(&mut r)?,
            5 | 6 => r.read_u64::<LittleEndian>()?.to_string(),
            _ => break,
        };
        values.push((key, value));
    }
    Ok((name, values))
}

/// The start of a request's parameters in hex, for the log.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().take(256).fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// A dare event's target and packed value.
fn dare_of(values: &[(String, String)]) -> Option<(u32, u32)> {
    let get = |key: &str| values.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.trim().parse::<u32>().ok());
    Some((get("targetPID")?, get("score")?))
}

fn info_reply(dares: &[Dare], now: u64) -> Vec<u8> {
    let mut out = packed_date_time(now).to_le_bytes().to_vec();
    #[allow(clippy::cast_possible_truncation)] // at most MAX_PER_PLAYER
    out.extend_from_slice(&(dares.len() as u32).to_le_bytes());
    for d in dares {
        out.extend_from_slice(&d.sender.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&packed_date_time(d.sent).to_le_bytes());
        out.extend_from_slice(&d.value.to_le_bytes());
        out.extend_from_slice(&d.id.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    out
}

fn db_failed(logger: &Logger, e: &rusqlite::Error) -> Error {
    slog::error!(logger, "dares database: {e}");
    Error::InternalError
}

/// The game's event reports (protocol 5001), dares among them.
pub struct GameEvents {
    store: Arc<Store>,
    friends: Arc<Friends>,
}

impl GameEvents {
    pub fn new(store: Arc<Store>, friends: Arc<Friends>) -> Self {
        Self { store, friends }
    }
}

impl<CI> Protocol<CI> for GameEvents {
    fn id(&self) -> u16 {
        5001
    }

    fn name(&self) -> String {
        "GameEventProtocol".into()
    }

    fn num_methods(&self) -> u32 {
        1
    }

    fn handle(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: &Request,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, Error> {
        let pid = crate::ticket::player(ci)?;
        if request.method_id != 1 {
            return Err(Error::UnknownMethod);
        }
        let Ok((name, values)) = parse_event(&request.parameters) else {
            return Ok(Vec::new()); // an event we can't read is still received
        };
        if name == "GDE" {
            match dare_of(&values) {
                Some((target, value)) if target != pid && self.friends.are_friends(pid, target) => {
                    self.store.send(pid, target, value, now()).map_err(|e| db_failed(logger, &e))?;
                    info!(logger, "{pid:#x} challenged {target:#x} ({value:#010x})");
                }
                _ => info!(
                    logger,
                    "{pid:#x}: a challenge not kept (not a friend, or unreadable): {values:?} ({})",
                    hex(&request.parameters)
                ),
            }
        }
        Ok(Vec::new())
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        (method_id == 1).then(|| "ReportEvent".into())
    }
}

/// The dares a player was sent (protocol 5008).
pub struct OverlordDare {
    store: Arc<Store>,
}

impl OverlordDare {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

impl<CI> Protocol<CI> for OverlordDare {
    fn id(&self) -> u16 {
        5008
    }

    fn name(&self) -> String {
        "OverlordDareProtocol".into()
    }

    fn num_methods(&self) -> u32 {
        2
    }

    fn handle(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: &Request,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, Error> {
        let pid = crate::ticket::player(ci)?;
        let now = now();
        match request.method_id {
            1 => {
                let dares = self.store.dares(pid, now.saturating_sub(KEPT_FOR)).map_err(|e| db_failed(logger, &e))?;
                Ok(info_reply(&dares, now))
            }
            2 => {
                let id = Cursor::new(&request.parameters[..]).read_u32::<LittleEndian>().map_err(|_| Error::ParsingError)?;
                self.store.delete(pid, id).map_err(|e| db_failed(logger, &e))?;
                Ok(Vec::new())
            }
            _ => Err(Error::UnknownMethod),
        }
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        match method_id {
            1 => Some("RequestDareInfo".into()),
            2 => Some("DeleteDare".into()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(s: &str) -> Vec<u8> {
        let mut out = u16::try_from(s.len() + 1).unwrap().to_le_bytes().to_vec();
        out.extend_from_slice(s.as_bytes());
        out.push(0);
        out
    }

    fn event(name: &str, values: &[(&str, &str)]) -> Vec<u8> {
        let mut out = string(name);
        out.extend_from_slice(&u32::try_from(values.len()).unwrap().to_le_bytes());
        for (k, v) in values {
            out.extend(string(k));
            out.push(4);
            out.extend(string(v));
        }
        out
    }

    #[test]
    fn reads_events_as_the_game_sends_them() {
        // After a ranked match, as captured.
        let mpp = [
            4, 0, 77, 80, 80, 0, 3, 0, 0, 0, 3, 0, 112, 49, 0, 4, 6, 0, 83, 99, 111, 117, 116, 0, 3, 0, 112, 50, 0, 4, 2, 0, 49, 0, 3, 0, 112, 51, 0, 4, 6, 0, 83, 99, 111, 117,
            116, 0,
        ];
        let (name, values) = parse_event(&mpp).unwrap();
        assert_eq!(name, "MPP");
        assert_eq!(values[0], ("p1".into(), "Scout".into()));
        assert_eq!(values.len(), 3);
        let gde = event("GDE", &[("targetPID", "1048578"), ("dareId", "1048577"), ("score", "1234567")]);
        let (name, values) = parse_event(&gde).unwrap();
        assert_eq!(name, "GDE");
        assert_eq!(dare_of(&values), Some((1_048_578, 1_234_567)));
        assert_eq!(dare_of(&values[..1]), None);
        assert!(parse_event(&gde[..gde.len() - 2]).is_err());

        // Numbers as numbers: int64 and uint64.
        let mut gde = string("GDE");
        gde.extend_from_slice(&3u32.to_le_bytes());
        gde.extend(string("targetPID"));
        gde.push(1);
        gde.extend_from_slice(&1_048_578i64.to_le_bytes());
        gde.extend(string("dareId"));
        gde.push(6);
        gde.extend_from_slice(&1_048_577u64.to_le_bytes());
        gde.extend(string("score"));
        gde.push(1);
        gde.extend_from_slice(&1_234_567i64.to_le_bytes());
        let (_, values) = parse_event(&gde).unwrap();
        assert_eq!(dare_of(&values), Some((1_048_578, 1_234_567)));
    }

    #[test]
    fn dares_are_kept_per_friend_listed_and_deleted_by_their_target() {
        let store = Store::in_memory().unwrap();
        store.send(1, 2, 100, 1000).unwrap();
        store.send(3, 2, 300, 1001).unwrap();
        store.send(1, 2, 101, 1002).unwrap(); // replaces 1's earlier dare
        let dares = store.dares(2, 0).unwrap();
        assert_eq!(dares.iter().map(|d| (d.sender, d.value)).collect::<Vec<_>>(), vec![(3, 300), (1, 101)]);
        assert!(store.dares(1, 0).unwrap().is_empty());
        let id = dares[0].id;
        assert!(!store.delete(1, id).unwrap(), "only its target deletes it");
        assert!(store.delete(2, id).unwrap());
        assert_eq!(store.dares(2, 0).unwrap().len(), 1);
        assert!(store.dares(2, 1003).unwrap().is_empty(), "old ones go");
    }

    #[test]
    fn a_player_keeps_only_the_latest_dares() {
        let store = Store::in_memory().unwrap();
        for sender in 0..40u32 {
            store.send(sender + 10, 2, sender, u64::from(sender)).unwrap();
        }
        let dares = store.dares(2, 0).unwrap();
        assert_eq!(dares.len(), 32);
        assert_eq!(dares[0].value, 8, "the oldest went");
    }

    #[test]
    fn the_reply_lists_each_dare() {
        let reply = info_reply(
            &[Dare {
                id: 7,
                sender: 0x10_0001,
                sent: 0,
                value: 0x00c8_0100,
            }],
            0,
        );
        assert_eq!(reply.len(), 8 + 4 + 28);
        assert_eq!(&reply[8..12], &1u32.to_le_bytes());
        assert_eq!(&reply[12..16], &0x10_0001u32.to_le_bytes());
        assert_eq!(&reply[28..32], &0x00c8_0100u32.to_le_bytes());
        assert_eq!(&reply[32..36], &7u32.to_le_bytes());
    }
}
