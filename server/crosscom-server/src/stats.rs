//! Player statistics (protocol 108): rank, experience and the rest of a player's numbers, read
//! by the game as its menus load.
//!
//! The game's own protocol, not among its declarations. Its methods are, in order:
//! SendPlayerStats{Int32,Int64,Float,String,Blob} (1-5) and ReadPlayerStats{Int32,Int64,Float,
//! String,Blob} (6-10). A read asks, per stats board, for some players' columns:
//!
//! ```text
//! request: list<{ board: u8, keys: list<u32>, columns: list<u8> }>, players: list<u32>
//! reply:   list<{ player: u32, boards: list<{ board: u8, keys: list<u32>, values: list<{ column: u8, value }> }> }>
//! ```
//!
//! The keys are `-1` or empty in what the game sends (16 boards when its menus load); they're
//! echoed back.
//!
//! The five reads differ only in their values: an i32, an i64, an f32, a string (u16 length,
//! then the bytes) or a blob (u16 length, then the bytes).
//!
//! A send writes the caller's own numbers, a row per board and keys:
//!
//! ```text
//! request: list<{ board: u8, keys: list<u32>, values: list<{ column: u8, value, op: u8 }> }>
//! ```
//!
//! The game sends its whole profile this way (350 rows of board 3 from the menus, keyed by pairs
//! from (0, 0) up), every op 2, with values such as 255 for "none": so op 2 sets the value. Op 0
//! adds: what a match earned (after a ranked match, +1000 and +750 to a character's board-15
//! columns, +205 to board 14, its class set alongside with op 2); a string or blob is set. Other
//! ops are logged and also set, until a game shows what they mean.
//!
//! Values are kept per player, type, board, keys and column. A read's key `-1` matches any key,
//! so a board read with `-1` keys answers every stored row of it, each with its own keys; a board
//! with nothing stored answers one row of zeros, its keys echoed.
//!
//! Except the career boards (1 to 3), which hold a player's Career characters:
//! - board 1, the header: one row, no keys; column 1 the number of character slots open (3 at
//!   first, one more for each character that reaches level 50, up to 6), 2 the selected slot,
//!   3 flags. The game takes it only from exactly one row (keys aside) and locks every slot from
//!   the count on, so a row of zeros locked them all: a new player couldn't make a Career
//!   character, and loading a ranked match crashed for want of one. So it's always answered, as
//!   the game's own defaults when nothing is stored, and never with fewer than 3 slots open
//!   (which also mends players who were sent 0 and kept it).
//! - board 2, a row per character keyed by its slot (column 1 its experience, 3 its class, 4 its
//!   level), and board 3, its loadout keyed by slot and item: only what's stored, no filler
//!   (a filler row would be an empty character in slot 0).
//!
//! Board 18 (the numbers Multiplayer Challenges are made from) gets no filler either: a friend
//! with nothing stored would be a challenge to beat 0.

use std::io::Cursor;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

use byteorder::LittleEndian;
use byteorder::ReadBytesExt;
use byteorder::WriteBytesExt;
use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::ToStream;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::rmc::Request;
use quazal::ClientInfo;
use quazal::Context;
use rusqlite::params;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use slog::debug;
use slog::info;
use slog::warn;
use slog::Logger;

/// A read's method, by the type of its values.
const READ_INT32: u32 = 6;

/// The career boards (see the module's notes): the header, the characters, their loadouts.
const CAREER_HEADER: u8 = 1;
const CAREER_BOARDS: [u8; 3] = [1, 2, 3];
/// The numbers challenges are made from: a friend with none stored would be "beat 0".
const CHALLENGE_BOARD: u8 = 18;
/// The header's columns, and the game's defaults for them.
const SLOTS_OPEN: u8 = 1;
const CAREER_DEFAULTS: [(u8, i32); 3] = [(SLOTS_OPEN, 3), (2, 0), (3, 8)];
/// Slots open: at first, and at most.
const MIN_SLOTS: i32 = 3;
const MAX_SLOTS: i32 = 6;

/// A player's stored career header (board 1, int32 values) as the game must get it: one row with
/// no keys, the defaults for what isn't stored, and the slots open never fewer than 3.
fn career_header(stored: Stored) -> Stored {
    let mut row: Stored = stored.into_iter().filter(|(keys, ..)| keys.is_empty()).collect();
    for (column, default) in CAREER_DEFAULTS {
        if !row.iter().any(|(_, c, _)| *c == column) {
            row.push((Vec::new(), column, default.to_le_bytes().to_vec()));
        }
    }
    for (_, column, value) in &mut row {
        if *column == SLOTS_OPEN {
            let open = value.get(..4).and_then(|v| v.try_into().ok()).map_or(0, i32::from_le_bytes);
            if !(MIN_SLOTS..=MAX_SLOTS).contains(&open) {
                *value = open.clamp(MIN_SLOTS, MAX_SLOTS).to_le_bytes().to_vec();
            }
        }
    }
    row.sort_by_key(|(_, c, _)| *c);
    row
}
const READ_BLOB: u32 = 10;

/// A new player's value, encoded, for a read method.
fn zero_value(method: u32) -> Option<Vec<u8>> {
    match method {
        READ_INT32 => Some(vec![0; 4]),
        7 => Some(vec![0; 8]),                // Int64
        8 => Some(0f32.to_le_bytes().into()), // Float
        9 => Some(String::new().to_bytes()),  // String
        READ_BLOB => Some(vec![0, 0]),        // Blob: an empty u16-length buffer
        _ => None,
    }
}
/// Why a send wasn't kept.
#[derive(Debug)]
enum WriteError {
    /// More than a player may keep.
    TooMuch,
    Database(rusqlite::Error),
}

fn db_error(e: rusqlite::Error) -> WriteError {
    WriteError::Database(e)
}

/// A send's method, by the type of its values; a read's is 5 more.
const SEND_INT32: u32 = 1;
const SEND_BLOB: u32 = 5;
/// The ops seen: add to the value (numbers), and set it.
const OP_ADD: u8 = 0;
const OP_SET: u8 = 2;

/// `stored` (if any) plus `added`, both encoded for `send_method`; for a string or blob, `added`.
fn add_value(send_method: u32, stored: Option<&[u8]>, added: &[u8]) -> Vec<u8> {
    let Some(stored) = stored.filter(|s| s.len() == added.len()) else {
        return added.to_vec();
    };
    match send_method {
        1 => {
            let (a, b) = (
                i32::from_le_bytes(stored.try_into().unwrap_or_default()),
                i32::from_le_bytes(added.try_into().unwrap_or_default()),
            );
            a.saturating_add(b).to_le_bytes().to_vec()
        }
        2 => {
            let (a, b) = (
                i64::from_le_bytes(stored.try_into().unwrap_or_default()),
                i64::from_le_bytes(added.try_into().unwrap_or_default()),
            );
            a.saturating_add(b).to_le_bytes().to_vec()
        }
        3 => {
            let (a, b) = (
                f32::from_le_bytes(stored.try_into().unwrap_or_default()),
                f32::from_le_bytes(added.try_into().unwrap_or_default()),
            );
            (a + b).to_le_bytes().to_vec()
        }
        _ => added.to_vec(),
    }
}
/// The longest string or blob kept, the most rows one send may write, and the most a player may
/// keep: values, and bytes of keys and values.
const MAX_VALUE_LEN: usize = 1024;
const MAX_ROWS: u32 = 2048;
const MAX_STORED: i64 = 65_536;
/// As much as one reply carries, so all of it can be read back (a whole profile is about 25 KB).
#[allow(clippy::cast_possible_wrap)]
const MAX_STORED_BYTES: i64 = MAX_REPLY as i64;

/// One value of a send, as encoded (so a read returns it as it came).
fn read_value(rdr: &mut Cursor<&[u8]>, send_method: u32) -> std::io::Result<Vec<u8>> {
    let len = match send_method {
        1 | 3 => 4,
        2 => 8,
        4 | 5 => {
            let len = rdr.read_u16::<LittleEndian>()?;
            if usize::from(len) > MAX_VALUE_LEN {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            let mut value = len.to_le_bytes().to_vec();
            value.resize(2 + usize::from(len), 0);
            rdr.read_exact(&mut value[2..])?;
            return Ok(value);
        }
        _ => return Err(std::io::ErrorKind::InvalidData.into()),
    };
    let mut value = vec![0; len];
    rdr.read_exact(&mut value)?;
    Ok(value)
}

#[derive(Debug, PartialEq, Eq)]
struct Row {
    board: u8,
    keys: Vec<u32>,
    /// (column, value as encoded, op)
    values: Vec<(u8, Vec<u8>, u8)>,
}

fn parse_send(parameters: &[u8], method: u32) -> Result<Vec<Row>, Error> {
    let mut rdr = Cursor::new(parameters);
    read_list(&mut rdr, MAX_ROWS, |r| {
        let board = r.read_u8()?;
        let keys = read_list(r, MAX_KEYS, |r| r.read_u32::<LittleEndian>()).map_err(|_| std::io::ErrorKind::InvalidData)?;
        let values = read_list(r, MAX_COLUMNS, |r| {
            let column = r.read_u8()?;
            let value = read_value(r, method)?;
            Ok((column, value, r.read_u8()?))
        })
        .map_err(|_| std::io::ErrorKind::InvalidData)?;
        Ok(Row { board, keys, values })
    })
}

fn keys_blob(keys: &[u32]) -> Vec<u8> {
    keys.iter().flat_map(|k| k.to_le_bytes()).collect()
}

fn keys_from_blob(blob: &[u8]) -> Vec<u32> {
    blob.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Every player's statistics, in the server's database.
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
            "CREATE TABLE IF NOT EXISTS player_stats (
                pid INTEGER NOT NULL,
                type INTEGER NOT NULL,
                board INTEGER NOT NULL,
                keys BLOB NOT NULL,
                col INTEGER NOT NULL,
                value BLOB NOT NULL,
                PRIMARY KEY (pid, type, board, keys, col)
            ) WITHOUT ROWID;",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Writes a send's rows for `pid`, all or nothing. Refused if the player would keep more than
    /// `MAX_STORED` values or `MAX_STORED_BYTES`.
    fn write(&self, pid: u32, value_type: u32, rows: &[Row]) -> Result<(), WriteError> {
        let mut db = self.lock();
        let tx = db.transaction().map_err(db_error)?;
        {
            let mut set = tx
                .prepare_cached("INSERT OR REPLACE INTO player_stats (pid, type, board, keys, col, value) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
                .map_err(db_error)?;
            let mut get = tx
                .prepare_cached("SELECT value FROM player_stats WHERE pid = ?1 AND type = ?2 AND board = ?3 AND keys = ?4 AND col = ?5")
                .map_err(db_error)?;
            for row in rows {
                let keys = keys_blob(&row.keys);
                for (column, value, op) in &row.values {
                    let value = if *op == OP_ADD {
                        let stored: Option<Vec<u8>> = get
                            .query_row(params![pid, value_type, row.board, keys, column], |r| r.get(0))
                            .optional()
                            .map_err(db_error)?;
                        add_value(value_type, stored.as_deref(), value)
                    } else {
                        value.clone()
                    };
                    set.execute(params![pid, value_type, row.board, keys, column, value]).map_err(db_error)?;
                }
            }
        }
        let (stored, bytes): (i64, i64) = tx
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(length(keys) + length(value)), 0) FROM player_stats WHERE pid = ?1",
                [pid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_error)?;
        if stored > MAX_STORED || bytes > MAX_STORED_BYTES {
            return Err(WriteError::TooMuch);
        }
        tx.commit().map_err(db_error)
    }

    /// A player's stored rows of a board: (keys, column, value). Refused past `MAX_REPLY` bytes,
    /// which no reply could carry.
    fn rows(&self, pid: u32, value_type: u32, board: u8) -> Result<Stored, Error> {
        let db = self.lock();
        let mut query = db
            .prepare_cached("SELECT keys, col, value FROM player_stats WHERE pid = ?1 AND type = ?2 AND board = ?3 ORDER BY keys, col")
            .map_err(|_| Error::InternalError)?;
        let rows = query
            .query_map(params![pid, value_type, board], |r| {
                Ok((keys_from_blob(&r.get::<_, Vec<u8>>(0)?), r.get::<_, u8>(1)?, r.get::<_, Vec<u8>>(2)?))
            })
            .map_err(|_| Error::InternalError)?;
        let mut out = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let row = row.map_err(|_| Error::InternalError)?;
            bytes += 4 * row.0.len() + 1 + row.2.len();
            if bytes > MAX_REPLY {
                return Err(Error::ParsingError);
            }
            out.push(row);
        }
        Ok(out)
    }
}

/// The longest lists a request may have. The game asks for 16 boards of one player, each with
/// up to 2 keys and 12 columns; leaderboards ask for more players.
const MAX_BOARDS: u32 = 64;
const MAX_KEYS: u32 = 16;
const MAX_COLUMNS: u32 = 64;
const MAX_PLAYERS: u32 = 256;
/// The biggest reply a read may need: the reply grows with players × boards × columns, so a
/// small request could otherwise ask for a huge one.
const MAX_REPLY: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
struct BoardQuery {
    board: u8,
    keys: Vec<u32>,
    columns: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct ReadRequest {
    boards: Vec<BoardQuery>,
    players: Vec<u32>,
}

fn read_list<T>(rdr: &mut Cursor<&[u8]>, max: u32, mut item: impl FnMut(&mut Cursor<&[u8]>) -> std::io::Result<T>) -> Result<Vec<T>, Error> {
    let len = rdr.read_u32::<LittleEndian>().map_err(|_| Error::ParsingError)?;
    if len > max {
        return Err(Error::ParsingError);
    }
    (0..len).map(|_| item(rdr).map_err(|_| Error::ParsingError)).collect()
}

fn parse_read(parameters: &[u8]) -> Result<ReadRequest, Error> {
    let mut rdr = Cursor::new(parameters);
    let boards = read_list(&mut rdr, MAX_BOARDS, |r| {
        let board = r.read_u8()?;
        let keys = read_list(r, MAX_KEYS, |r| r.read_u32::<LittleEndian>()).map_err(|_| std::io::ErrorKind::InvalidData)?;
        let columns = read_list(r, MAX_COLUMNS, |r| r.read_u8()).map_err(|_| std::io::ErrorKind::InvalidData)?;
        Ok(BoardQuery { board, keys, columns })
    })?;
    let players = read_list(&mut rdr, MAX_PLAYERS, |r| r.read_u32::<LittleEndian>())?;
    Ok(ReadRequest { boards, players })
}

/// The size of the reply to `request`, in bytes, with values `value_len` bytes long.
fn reply_size(request: &ReadRequest, value_len: usize) -> usize {
    let per_player: usize = 4
        + 4
        + request
            .boards
            .iter()
            .map(|b| 1 + 4 + 4 * b.keys.len() + 4 + (1 + value_len) * b.columns.len())
            .sum::<usize>();
    4 + request.players.len() * per_player
}

/// What a player has stored of a board: (keys, column, value), in keys order.
pub(crate) type Stored = Vec<(Vec<u32>, u8, Vec<u8>)>;

/// A row of a read's reply: the board's query, the row's keys and its values, in the query's
/// column order.
type ReplyRow<'a> = (&'a BoardQuery, &'a [u32], Vec<&'a [u8]>);

fn matches(query: &[u32], keys: &[u32]) -> bool {
    query.len() == keys.len() && query.iter().zip(keys).all(|(&q, &k)| q == u32::MAX || q == k)
}

/// The reply to a read: for every asked-for player and board, a row per stored keys that match
/// (or one row of `zero`s, the keys echoed, when none do), with every asked-for column. Refused
/// when it would be too big: the reply grows with players × boards × rows × columns.
fn read_reply(request: &ReadRequest, zero: &[u8], mut stored: impl FnMut(u32, u8) -> Result<Stored, Error>) -> Result<Vec<u8>, Error> {
    if reply_size(request, zero.len()) > MAX_REPLY {
        return Err(Error::ParsingError);
    }
    #[allow(clippy::cast_possible_truncation)] // bounded by the caps and MAX_REPLY
    let len = |n: usize| n as u32;
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(len(request.players.len())).ok();
    for &pid in &request.players {
        let mut rows: Vec<ReplyRow> = Vec::new();
        // Each board once, however often it's asked for; and no more than a reply can carry.
        let mut loaded: Vec<(u8, Stored)> = Vec::new();
        let mut bytes = 0;
        for query in &request.boards {
            if !loaded.iter().any(|(b, _)| *b == query.board) {
                let found = stored(pid, query.board)?;
                bytes += found.iter().map(|(k, _, v)| 4 * k.len() + 1 + v.len()).sum::<usize>();
                if out.len() + bytes > MAX_REPLY {
                    return Err(Error::ParsingError);
                }
                loaded.push((query.board, found));
            }
        }
        for query in &request.boards {
            let found = &loaded.iter().find(|(b, _)| *b == query.board).expect("loaded above").1;
            let before = rows.len();
            let mut at = 0;
            while at < found.len() {
                let keys = &found[at].0;
                let end = at + found[at..].iter().take_while(|(k, ..)| k == keys).count();
                // The career header is one row the game takes whatever keys it asks with.
                if matches(&query.keys, keys) || query.board == CAREER_HEADER {
                    let values = query
                        .columns
                        .iter()
                        .map(|c| found[at..end].iter().find(|(_, col, _)| col == c).map_or(zero, |(.., v)| v.as_slice()))
                        .collect();
                    rows.push((query, keys, values));
                }
                at = end;
            }
            if rows.len() == before && !CAREER_BOARDS.contains(&query.board) && query.board != CHALLENGE_BOARD {
                rows.push((query, &query.keys, vec![zero; query.columns.len()]));
            }
        }
        out.write_u32::<LittleEndian>(pid).ok();
        out.write_u32::<LittleEndian>(len(rows.len())).ok();
        for (query, keys, values) in rows {
            out.write_u8(query.board).ok();
            out.write_u32::<LittleEndian>(len(keys.len())).ok();
            for &key in keys {
                out.write_u32::<LittleEndian>(key).ok();
            }
            out.write_u32::<LittleEndian>(len(values.len())).ok();
            for (&column, value) in query.columns.iter().zip(values) {
                out.write_u8(column).ok();
                out.extend_from_slice(value);
            }
            if out.len() > MAX_REPLY {
                return Err(Error::ParsingError);
            }
        }
    }
    Ok(out)
}

pub struct PlayerStatistics {
    store: std::sync::Arc<Store>,
    /// Whether an op other than set was seen and logged.
    other_op_logged: std::sync::atomic::AtomicBool,
}

impl PlayerStatistics {
    pub fn new(store: std::sync::Arc<Store>) -> Self {
        Self {
            store,
            other_op_logged: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl<CI> Protocol<CI> for PlayerStatistics {
    fn id(&self) -> u16 {
        108
    }

    fn name(&self) -> String {
        "PlayerStatisticsProtocol".into()
    }

    fn num_methods(&self) -> u32 {
        10
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
        let method = request.method_id;
        if (SEND_INT32..=SEND_BLOB).contains(&method) {
            let rows = parse_send(&request.parameters, method)?;
            let values: usize = rows.iter().map(|r| r.values.len()).sum();
            if let Some(op) = rows.iter().flat_map(|r| &r.values).map(|v| v.2).find(|&op| op != OP_SET && op != OP_ADD) {
                if !self.other_op_logged.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    let shown = &request.parameters[..request.parameters.len().min(512)];
                    warn!(
                        logger,
                        "PlayerStatistics.{method}: op {op} (only 0, add, and 2, set, are known; kept as set); parameters: {}",
                        shown.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    );
                }
            }
            self.store.write(pid, method, &rows).map_err(|e| match e {
                WriteError::TooMuch => Error::AccessDenied,
                WriteError::Database(e) => {
                    slog::error!(logger, "PlayerStatistics.{method}: not kept, the database failed: {e}");
                    Error::InternalError
                }
            })?;
            info!(logger, "PlayerStatistics.{method}: kept {} rows, {values} values", rows.len());
            return Ok(Vec::new());
        }
        let Some(zero) = zero_value(method) else {
            return Err(Error::UnknownMethod);
        };
        let read = parse_read(&request.parameters)?;
        debug!(logger, "PlayerStatistics.{method} {read:?}");
        let reply = read_reply(&read, &zero, |player, board| {
            let rows = self.store.rows(player, method - 5, board)?;
            Ok(if method == READ_INT32 && board == CAREER_HEADER { career_header(rows) } else { rows })
        })?;
        debug!(logger, "PlayerStatistics.{method}: replied {} bytes", reply.len());
        Ok(reply)
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        const NAMES: [&str; 10] = [
            "SendPlayerStatsInt32",
            "SendPlayerStatsInt64",
            "SendPlayerStatsFloat",
            "SendPlayerStatsString",
            "SendPlayerStatsBlob",
            "ReadPlayerStatsInt32",
            "ReadPlayerStatsInt64",
            "ReadPlayerStatsFloat",
            "ReadPlayerStatsString",
            "ReadPlayerStatsBlob",
        ];
        NAMES.get((method_id as usize).checked_sub(1)?).map(|n| (*n).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the game sent as its menus loaded (two of its boards).
    const FROM_THE_GAME: &str = "020000000a01000000ffffffff060000000708090102030d0000000006000000201a1312101b0100000001001000";

    #[allow(clippy::unnecessary_wraps)]
    fn none(_: u32, _: u8) -> Result<Stored, Error> {
        Ok(Vec::new())
    }

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn reads_what_the_game_sends() {
        let read = parse_read(&bytes(FROM_THE_GAME)).unwrap();
        assert_eq!(read.players, vec![0x0010_0001]);
        assert_eq!(
            read.boards,
            vec![
                BoardQuery {
                    board: 10,
                    keys: vec![0xffff_ffff],
                    columns: vec![7, 8, 9, 1, 2, 3]
                },
                BoardQuery {
                    board: 13,
                    keys: vec![],
                    columns: vec![0x20, 0x1a, 0x13, 0x12, 0x10, 0x1b]
                },
            ]
        );
    }

    #[test]
    fn answers_every_column_asked_for_with_zero() {
        let read = ReadRequest {
            boards: vec![BoardQuery {
                board: 4,
                keys: vec![0xffff_ffff],
                columns: vec![1, 2],
            }],
            players: vec![0x0010_0001],
        };
        #[rustfmt::skip]
        let expected = [
            1, 0, 0, 0,                         // one player
            1, 0, 0x10, 0,                      // pid 0x100001
            1, 0, 0, 0,                         // one board
            4,                                  // board 4
            1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, // its keys, echoed
            2, 0, 0, 0,                         // two values
            1, 0, 0, 0, 0,                      // column 1 = 0
            2, 0, 0, 0, 0,                      // column 2 = 0
        ];
        assert_eq!(read_reply(&read, &zero_value(READ_INT32).unwrap(), none).unwrap(), expected);
    }

    fn one_board(board: u8, keys: Vec<u32>, columns: Vec<u8>) -> ReadRequest {
        ReadRequest {
            boards: vec![BoardQuery { board, keys, columns }],
            players: vec![0x0010_0001],
        }
    }

    /// A reply's row: its keys, and (column, value)s.
    type Row32 = (Vec<u32>, Vec<(u8, i32)>);

    /// The rows of a one-player reply.
    fn reply_rows(reply: &[u8]) -> Vec<Row32> {
        let mut r = Cursor::new(reply);
        let u32_ = |r: &mut Cursor<&[u8]>| r.read_u32::<LittleEndian>().unwrap();
        assert_eq!(u32_(&mut r), 1);
        u32_(&mut r);
        (0..u32_(&mut r))
            .map(|_| {
                r.read_u8().unwrap();
                let keys = (0..u32_(&mut r)).map(|_| u32_(&mut r)).collect();
                let values = (0..u32_(&mut r)).map(|_| (r.read_u8().unwrap(), r.read_i32::<LittleEndian>().unwrap())).collect();
                (keys, values)
            })
            .collect()
    }

    #[test]
    fn a_new_player_gets_three_career_slots_and_no_empty_characters() {
        let zero = zero_value(READ_INT32).unwrap();
        let header = |stored: Stored| move |_, board| Ok(if board == CAREER_HEADER { career_header(stored.clone()) } else { Vec::new() });
        for keys in [vec![], vec![u32::MAX]] {
            let reply = read_reply(&one_board(1, keys, vec![1, 2, 3]), &zero, header(Vec::new())).unwrap();
            assert_eq!(reply_rows(&reply), vec![(vec![], vec![(1, 3), (2, 0), (3, 8)])]);
        }
        for board in [2, 3] {
            let reply = read_reply(&one_board(board, vec![u32::MAX], vec![1, 3]), &zero, header(Vec::new())).unwrap();
            assert!(reply_rows(&reply).is_empty(), "board {board}: no filler character");
        }
        let reply = read_reply(&one_board(4, vec![u32::MAX], vec![1]), &zero, none).unwrap();
        assert_eq!(reply_rows(&reply).len(), 1, "other boards keep their row of zeros");
    }

    #[test]
    fn a_stored_career_header_is_kept_with_at_least_three_slots() {
        let v = |n: i32| n.to_le_bytes().to_vec();
        let stored = vec![(vec![], 1, v(5)), (vec![], 2, v(1))];
        let got: Vec<(u8, Vec<u8>)> = career_header(stored).into_iter().map(|(_, c, v)| (c, v)).collect();
        assert_eq!(got, vec![(1, v(5)), (2, v(1)), (3, v(8))]);
        for (sent, kept) in [(0, 3), (-1, 3), (7, 6), (3, 3), (6, 6)] {
            assert_eq!(career_header(vec![(vec![], 1, v(sent))])[0].2, v(kept), "{sent} slots");
        }
        assert_eq!(career_header(vec![(vec![7], 1, v(5))])[0].2, v(3), "a keyed row isn't the header");
    }

    #[test]
    fn refuses_requests_for_huge_replies() {
        // Within every list's cap, but players × boards × columns is too big.
        let mut req = Vec::new();
        req.write_u32::<LittleEndian>(MAX_BOARDS).unwrap();
        for _ in 0..MAX_BOARDS {
            req.write_u8(1).unwrap();
            req.write_u32::<LittleEndian>(0).unwrap();
            req.write_u32::<LittleEndian>(MAX_COLUMNS).unwrap();
            req.extend(std::iter::repeat_n(1, MAX_COLUMNS as usize));
        }
        req.write_u32::<LittleEndian>(MAX_PLAYERS).unwrap();
        req.extend(std::iter::repeat_n(7, 4 * MAX_PLAYERS as usize));
        let read = parse_read(&req).unwrap();
        assert!(read_reply(&read, &zero_value(7).unwrap(), none).is_err());
    }

    #[test]
    fn the_reply_size_is_exact_for_every_value_type() {
        let read = parse_read(&bytes(FROM_THE_GAME)).unwrap();
        for method in READ_INT32..=READ_BLOB {
            let value = zero_value(method).unwrap();
            assert_eq!(reply_size(&read, value.len()), read_reply(&read, &value, none).unwrap().len(), "method {method}");
        }
    }

    #[test]
    fn values_for_each_read() {
        assert_eq!(zero_value(7).unwrap(), vec![0; 8]);
        assert_eq!(
            zero_value(9).unwrap(),
            vec![0, 0],
            "an empty string, as quazal writes it (the game takes it: LoginEx's empty message is the same)"
        );
        assert_eq!(zero_value(READ_BLOB).unwrap(), vec![0, 0]);
        assert_eq!(zero_value(1), None, "sends aren't reads");
    }

    #[test]
    fn refuses_absurd_or_truncated_requests() {
        assert!(parse_read(&[0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(parse_read(&[1, 0, 0, 0]).is_err());
        assert!(parse_read(&bytes(&FROM_THE_GAME[..FROM_THE_GAME.len() - 8])).is_err());
    }

    /// The start of what the game sent from its menus: rows of board 3, keyed (0, 0), (0, 1)...
    fn profile_send() -> Vec<u8> {
        let mut out = Vec::new();
        out.write_u32::<LittleEndian>(2).unwrap();
        for second in 0..2u32 {
            out.write_u8(3).unwrap();
            out.write_u32::<LittleEndian>(2).unwrap();
            out.write_u32::<LittleEndian>(0).unwrap();
            out.write_u32::<LittleEndian>(second).unwrap();
            let values: [(u8, i32); 3] = [(1, 17), (8, 255), (11, 92 + second as i32)];
            out.write_u32::<LittleEndian>(values.len() as u32).unwrap();
            for (column, value) in values {
                out.write_u8(column).unwrap();
                out.write_i32::<LittleEndian>(value).unwrap();
                out.write_u8(OP_SET).unwrap();
            }
        }
        out
    }

    #[test]
    fn reads_a_send_as_the_game_writes_it() {
        // The first bytes the game sent, up to its first row's end.
        let sent = bytes("0100000003020000000000000000000000020000000111000000020b5c00000002");
        let rows = parse_send(&sent, SEND_INT32).unwrap();
        assert_eq!(
            rows,
            vec![Row {
                board: 3,
                keys: vec![0, 0],
                values: vec![(1, 17i32.to_le_bytes().to_vec(), 2), (0x0b, 92i32.to_le_bytes().to_vec(), 2)],
            }]
        );
        assert!(parse_send(&sent[..sent.len() - 1], SEND_INT32).is_err(), "truncated");
        let mut string = bytes("01000000050000000001000000010300616263");
        string.push(2);
        let rows = parse_send(&string, 4).unwrap();
        assert_eq!(rows[0].values, vec![(1, vec![3, 0, b'a', b'b', b'c'], 2)]);
    }

    #[test]
    fn a_send_is_read_back_row_by_row_and_kept_per_player() {
        let store = Store::in_memory().unwrap();
        store.write(0x0010_0001, SEND_INT32, &parse_send(&profile_send(), SEND_INT32).unwrap()).unwrap();
        let read = ReadRequest {
            boards: vec![BoardQuery {
                board: 3,
                keys: vec![u32::MAX, u32::MAX],
                columns: vec![11, 2],
            }],
            players: vec![0x0010_0001, 0x0010_0002],
        };
        let reply = read_reply(&read, &zero_value(READ_INT32).unwrap(), |pid, board| store.rows(pid, SEND_INT32, board)).unwrap();
        #[rustfmt::skip]
        let expected: Vec<u8> = [
            &[2, 0, 0, 0][..],
            &[1, 0, 0x10, 0], &[2, 0, 0, 0],                       // the first player: two rows
            &[3], &[2, 0, 0, 0], &[0; 4], &[0; 4],                 // board 3, keys (0, 0)
            &[2, 0, 0, 0], &[11, 92, 0, 0, 0], &[2, 0, 0, 0, 0],   // column 11 = 92, 2 never sent
            &[3], &[2, 0, 0, 0], &[0; 4], &[1, 0, 0, 0],           // keys (0, 1)
            &[2, 0, 0, 0], &[11, 93, 0, 0, 0], &[2, 0, 0, 0, 0],
            &[2, 0, 0x10, 0], &[0, 0, 0, 0],                       // the second player: nothing kept, no filler (a career board)
        ].concat();
        assert_eq!(reply, expected);
        // A read of other types, and of exact keys.
        assert!(store.rows(0x0010_0001, 2, 3).unwrap().is_empty());
        let exact = ReadRequest {
            boards: vec![BoardQuery {
                board: 3,
                keys: vec![0, 1],
                columns: vec![1],
            }],
            players: vec![0x0010_0001],
        };
        let reply = read_reply(&exact, &zero_value(READ_INT32).unwrap(), |pid, board| store.rows(pid, SEND_INT32, board)).unwrap();
        assert_eq!(&reply[reply.len() - 5..], &[1, 17, 0, 0, 0]);
        assert_eq!(u32::from_le_bytes(reply[8..12].try_into().unwrap()), 1, "one row");
    }

    #[test]
    fn op_0_adds_what_a_match_earned() {
        let store = Store::in_memory().unwrap();
        let v = |n: i32| n.to_le_bytes().to_vec();
        let send = |values: Vec<(u8, Vec<u8>, u8)>| vec![Row { board: 15, keys: vec![0], values }];
        store.write(1, SEND_INT32, &send(vec![(1, v(0x17), OP_SET), (14, v(1000), OP_ADD)])).unwrap();
        store.write(1, SEND_INT32, &send(vec![(1, v(0x17), OP_SET), (14, v(750), OP_ADD)])).unwrap();
        let got: Vec<(u8, Vec<u8>)> = store.rows(1, SEND_INT32, 15).unwrap().into_iter().map(|(_, c, v)| (c, v)).collect();
        assert_eq!(got, vec![(1, v(0x17)), (14, v(1750))]);
        assert_eq!(add_value(1, Some(&v(i32::MAX)), &v(5)), v(i32::MAX), "saturates");
        assert_eq!(add_value(3, Some(&1.5f32.to_le_bytes()), &2.0f32.to_le_bytes()), 3.5f32.to_le_bytes().to_vec());
        assert_eq!(add_value(4, Some(&[1, 0, b'a']), &[1, 0, b'b']), vec![1, 0, b'b'], "a string is set");
    }

    #[test]
    fn a_later_send_replaces_and_a_player_cant_keep_too_much() {
        let store = Store::in_memory().unwrap();
        let row = |value: i32| Row {
            board: 1,
            keys: vec![],
            values: vec![(1, value.to_le_bytes().to_vec(), OP_SET)],
        };
        store.write(5, SEND_INT32, &[row(1)]).unwrap();
        store.write(5, SEND_INT32, &[row(7)]).unwrap();
        assert_eq!(store.rows(5, SEND_INT32, 1).unwrap(), vec![(vec![], 1, 7i32.to_le_bytes().to_vec())]);
        let many: Vec<Row> = (0..=MAX_STORED as u32 / 64)
            .map(|k| Row {
                board: 2,
                keys: vec![k],
                values: (0..64).map(|c| (c, vec![0; 4], OP_SET)).collect(),
            })
            .collect();
        assert!(store.write(5, SEND_INT32, &many).is_err());
        assert!(store.rows(5, SEND_INT32, 2).unwrap().is_empty(), "nothing of a refused send is kept");
    }

    #[test]
    fn a_player_cant_keep_too_many_bytes() {
        let store = Store::in_memory().unwrap();
        let blob = |k: u32| Row {
            board: 1,
            keys: vec![k],
            values: (0..8).map(|c| (c, vec![0; 2 + MAX_VALUE_LEN], OP_SET)).collect(),
        };
        let fits: Vec<Row> = (0..30).map(blob).collect();
        store.write(5, SEND_BLOB, &fits).unwrap();
        let more: Vec<Row> = (30..40).map(blob).collect();
        assert!(store.write(5, SEND_BLOB, &more).is_err(), "over {MAX_STORED_BYTES} bytes");
        assert_eq!(store.rows(5, SEND_BLOB, 1).unwrap().len(), 30 * 8, "what was kept reads back whole");
    }
}
