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
//! then the bytes) or a blob (u16 length, then the bytes). Nothing is stored yet, so every value
//! read is zero or empty, as for a new player.

use std::io::Cursor;

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
use slog::debug;
use slog::info;
use slog::Logger;

/// A read's method, by the type of its values.
const READ_INT32: u32 = 6;
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

/// The reply to a read: every asked-for column of every board, for every asked-for player, set to
/// `value`. Refused when it would be too big: the reply grows with players × boards × columns.
fn read_reply(request: &ReadRequest, value: &[u8]) -> Result<Vec<u8>, Error> {
    if reply_size(request, value.len()) > MAX_REPLY {
        return Err(Error::ParsingError);
    }
    let mut out = Vec::new();
    #[allow(clippy::cast_possible_truncation)] // bounded by MAX_LIST
    let len = |n: usize| n as u32;
    out.write_u32::<LittleEndian>(len(request.players.len())).ok();
    for &pid in &request.players {
        out.write_u32::<LittleEndian>(pid).ok();
        out.write_u32::<LittleEndian>(len(request.boards.len())).ok();
        for query in &request.boards {
            out.write_u8(query.board).ok();
            out.write_u32::<LittleEndian>(len(query.keys.len())).ok();
            for &key in &query.keys {
                out.write_u32::<LittleEndian>(key).ok();
            }
            out.write_u32::<LittleEndian>(len(query.columns.len())).ok();
            for &column in &query.columns {
                out.write_u8(column).ok();
                out.extend_from_slice(value);
            }
        }
    }
    Ok(out)
}

pub struct PlayerStatistics;

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
        crate::ticket::player(ci)?;
        let Some(value) = zero_value(request.method_id) else {
            let shown = &request.parameters[..request.parameters.len().min(512)];
            info!(
                logger,
                "PlayerStatistics.{} has no answer yet; parameters: {}",
                request.method_id,
                shown.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            return Err(Error::UnknownMethod);
        };
        let read = parse_read(&request.parameters)?;
        debug!(logger, "PlayerStatistics.{} {read:?}", request.method_id);
        read_reply(&read, &value)
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
                board: 3,
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
            3,                                  // board 3
            1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, // its keys, echoed
            2, 0, 0, 0,                         // two values
            1, 0, 0, 0, 0,                      // column 1 = 0
            2, 0, 0, 0, 0,                      // column 2 = 0
        ];
        assert_eq!(read_reply(&read, &zero_value(READ_INT32).unwrap()).unwrap(), expected);
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
            req.extend(std::iter::repeat(1).take(MAX_COLUMNS as usize));
        }
        req.write_u32::<LittleEndian>(MAX_PLAYERS).unwrap();
        req.extend(std::iter::repeat(7).take(4 * MAX_PLAYERS as usize));
        let read = parse_read(&req).unwrap();
        assert!(read_reply(&read, &zero_value(7).unwrap()).is_err());
    }

    #[test]
    fn the_reply_size_is_exact_for_every_value_type() {
        let read = parse_read(&bytes(FROM_THE_GAME)).unwrap();
        for method in READ_INT32..=READ_BLOB {
            let value = zero_value(method).unwrap();
            assert_eq!(reply_size(&read, value.len()), read_reply(&read, &value).unwrap().len(), "method {method}");
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
}
