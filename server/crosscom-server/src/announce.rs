//! Whether players' games can be reached where they announce themselves.
//!
//! Other players are given a game's public address first (or the port its router forwards, see
//! `Sessions::urls_seen_from`). Whether that address takes packets from anyone decides who can
//! join the matches it hosts: if it does, everyone; if not, only players whose games can punch
//! through their routers (not those behind a symmetric NAT, see `reach`).
//!
//! A game answers a NAT probe from anyone, on its peer-to-peer socket: 14 bytes, `a5 00`, the
//! sender's connection ID (u32, little-endian), a token of 4 bytes and 4 zero bytes; the answer is
//! `a5 01`, the game's own connection ID (its `RVCID`), the token, and 4 zero bytes. So when a game
//! registers its addresses, or its client reports a forward, the server sends a probe to the
//! address other players get first, from a socket of its own that the player's router has never
//! seen, and waits [`WAIT`] for the answer. The right game answering means the address takes
//! packets from anyone. (A router that lets in whatever comes from an address its player already
//! sent to, whatever the port, counts as open too: the server's address is one it has.)
//!
//! The result is logged, noted for the player (`open`), given to the client with its router report,
//! and put in the connection records (`connections`).

use std::collections::HashMap;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use slog::info;
use slog::warn;
use slog::Logger;

use crate::sessions::Sessions;

/// How long an answer is waited for.
pub const WAIT: Duration = Duration::from_millis(1500);
/// Copies of each probe sent, a moment apart: one lost would call a reachable game unreachable.
const COPIES: usize = 2;
/// How soon the same address is checked again for a player (it registers its URLs with each
/// session it makes; a new sign-in forgets what was found, see [`forget`]).
const AGAIN_AFTER: Duration = Duration::from_secs(2 * 60);
/// Probes waiting for an answer at most.
const MAX_PENDING: usize = 4096;
/// The connection ID the server's probes give as their sender's.
const SERVER_CID: u32 = 0x4343_4f4d;

/// A probe of `token`, as a game sends one.
#[must_use]
pub fn probe(token: [u8; 4]) -> Vec<u8> {
    let mut p = vec![0xa5, 0x00];
    p.extend_from_slice(&SERVER_CID.to_le_bytes());
    p.extend_from_slice(&token);
    p.extend_from_slice(&[0; 4]);
    p
}

/// A probe's answer: the token it answers and the connection ID of the game answering.
#[must_use]
pub fn answer(data: &[u8]) -> Option<([u8; 4], u32)> {
    if data.len() < 10 || data[0] != 0xa5 || data[1] != 0x01 {
        return None;
    }
    let cid = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    Some(([data[6], data[7], data[8], data[9]], cid))
}

/// What a check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    /// The address checked.
    pub to: SocketAddrV4,
    /// Whether the player's game answered there.
    pub open: bool,
    pub at: Instant,
}

struct Pending {
    pid: u32,
    to: SocketAddrV4,
    rvcid: Option<u32>,
    sent: Instant,
}

#[derive(Default)]
struct State {
    pending: HashMap<[u8; 4], Pending>,
    results: HashMap<u32, Checked>,
}

pub struct Announce {
    socket: UdpSocket,
    state: Mutex<State>,
}

/// The address other players are given first for `pid` (one elsewhere, not at its house), with
/// the game's connection ID there: what's checked.
#[must_use]
pub fn announced(sessions: &Sessions, pid: u32) -> Option<(SocketAddrV4, Option<u32>)> {
    let IpAddr::V4(seen) = sessions.seen(pid)? else {
        return None;
    };
    let elsewhere = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    // Its peer-to-peer address: not its connection to the server, registered at sign-in.
    let url = sessions
        .urls_seen_from(pid, elsewhere)
        .into_iter()
        .find(|u| u.scheme == "udp" && u.address == seen.to_string())?;
    let rvcid = url.params.get("RVCID").and_then(|c| c.parse().ok());
    Some((SocketAddrV4::new(seen, url.port), rvcid))
}

impl Announce {
    /// A socket of its own, for probes and their answers.
    pub fn new() -> std::io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        Ok(Self { socket, state: Mutex::default() })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Checks `pid`'s announced address, unless it was just checked (or is being).
    ///
    /// `afresh`: whatever the last check found (a forward was just made: the address may be the
    /// same, but what's in front of it isn't).
    pub fn check(&self, logger: &Logger, sessions: &Sessions, pid: u32, afresh: bool) {
        let Some((to, rvcid)) = announced(sessions, pid) else {
            return;
        };
        let token: [u8; 4] = rand::random();
        {
            let mut state = self.state();
            let recent = !afresh && state.results.get(&pid).is_some_and(|c| c.to == to && c.at.elapsed() < AGAIN_AFTER);
            let waiting = state.pending.values().any(|p| p.pid == pid && p.to == to);
            if recent || waiting || state.pending.len() >= MAX_PENDING {
                return;
            }
            state.pending.insert(
                token,
                Pending {
                    pid,
                    to,
                    rvcid,
                    sent: Instant::now(),
                },
            );
        }
        let probe = probe(token);
        for _ in 0..COPIES {
            if let Err(e) = self.socket.send_to(&probe, to) {
                warn!(logger, "couldn't send {pid:#x}'s game a probe at {to}: {e}");
            }
        }
        slog::debug!(logger, "probing {pid:#x}'s game at {to}");
    }

    /// Takes in what came to the socket for a while, and settles the checks whose wait is over.
    fn work(&self, logger: &Logger) {
        let mut buf = [0u8; 64];
        if let Ok((n, SocketAddr::V4(from))) = self.socket.recv_from(&mut buf) {
            self.answered(logger, &buf[..n], from);
        }
        self.settle(logger);
    }

    fn answered(&self, logger: &Logger, data: &[u8], from: SocketAddrV4) {
        let Some((token, cid)) = answer(data) else {
            return;
        };
        let mut state = self.state();
        let Some(p) = state.pending.get(&token) else {
            return;
        };
        if from.ip() != p.to.ip() {
            return;
        }
        let Some(p) = state.pending.remove(&token) else {
            return;
        };
        let right = p.rvcid.is_none_or(|r| r == cid);
        state.results.insert(
            p.pid,
            Checked {
                to: p.to,
                open: right,
                at: Instant::now(),
            },
        );
        drop(state);
        if right {
            info!(logger, "{:#x}'s game answers at {} from anywhere: any player can join its matches", p.pid, p.to);
        } else {
            warn!(logger, "{:#x}'s announced address {} answered as another game ({cid}, not {:?})", p.pid, p.to, p.rvcid);
        }
    }

    fn settle(&self, logger: &Logger) {
        let mut state = self.state();
        let over: Vec<[u8; 4]> = state.pending.iter().filter(|(_, p)| p.sent.elapsed() >= WAIT).map(|(t, _)| *t).collect();
        for token in over {
            let Some(p) = state.pending.remove(&token) else {
                continue;
            };
            state.results.insert(
                p.pid,
                Checked {
                    to: p.to,
                    open: false,
                    at: Instant::now(),
                },
            );
            info!(
                logger,
                "{:#x}'s game didn't answer at {} from outside: only players who can punch through can join its matches", p.pid, p.to
            );
        }
    }

    /// Forgets what was found for `pid`: its game started afresh, and what's in front of it may
    /// have changed (a forward gone with the last game).
    pub fn forget(&self, pid: u32) {
        self.state().results.remove(&pid);
    }

    /// What the last check of `pid` found (none while it's being checked again).
    #[must_use]
    pub fn checked(&self, pid: u32) -> Option<Checked> {
        let state = self.state();
        if state.pending.values().any(|p| p.pid == pid) {
            return None;
        }
        state.results.get(&pid).copied()
    }
}

/// The server's checks, once it's serving.
static SERVING: OnceLock<(Announce, Arc<Sessions>)> = OnceLock::new();

/// Starts checking the addresses `sessions`' players announce.
pub fn start(logger: &Logger, sessions: Arc<Sessions>) -> std::io::Result<()> {
    let announce = Announce::new()?;
    if SERVING.set((announce, sessions)).is_err() {
        return Ok(());
    }
    let logger = logger.clone();
    std::thread::Builder::new().name("announced addresses".into()).spawn(move || loop {
        if let Some((a, _)) = SERVING.get() {
            a.work(&logger);
        }
    })?;
    Ok(())
}

/// [`Announce::check`], while serving.
pub fn check(logger: &Logger, pid: u32, afresh: bool) {
    if let Some((a, s)) = SERVING.get() {
        a.check(logger, s, pid, afresh);
    }
}

/// [`Announce::forget`], while serving.
pub fn forget(pid: u32) {
    if let Some((a, _)) = SERVING.get() {
        a.forget(pid);
    }
}

/// Whether `pid`'s game answered at its announced address (when it was checked, and it's still
/// the address announced).
#[must_use]
pub fn open(pid: u32) -> Option<bool> {
    let (a, s) = SERVING.get()?;
    let checked = a.checked(pid)?;
    let (now, _) = announced(s, pid)?;
    (checked.to == now).then_some(checked.open)
}

#[cfg(test)]
mod tests {
    use quazal::rmc::types::StationURL;

    use super::*;

    fn logger() -> Logger {
        Logger::root(slog::Discard, slog::o!())
    }

    #[test]
    fn probes_and_answers_as_the_game_has_them() {
        let p = probe([1, 2, 3, 4]);
        assert_eq!(p.len(), 14);
        assert_eq!(&p[..2], &[0xa5, 0x00]);
        assert_eq!(&p[6..10], &[1, 2, 3, 4]);
        // An answer a game gave, from the capture of 2026-11-24.
        let a = hex("a501aaaaaa3a8e67395900000000");
        assert_eq!(answer(&a), Some(([0x8e, 0x67, 0x39, 0x59], 984_263_338)));
        assert_eq!(answer(&p), None, "a probe isn't an answer");
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// A stand-in game on this machine: answers probes as the game does, with its connection ID.
    fn game(cid: u32) -> SocketAddrV4 {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(at) = socket.local_addr().unwrap() else { panic!() };
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            while let Ok((n, from)) = socket.recv_from(&mut buf) {
                if n == 14 && buf[0] == 0xa5 && buf[1] == 0 {
                    let mut reply = vec![0xa5, 0x01];
                    reply.extend_from_slice(&cid.to_le_bytes());
                    reply.extend_from_slice(&buf[6..10]);
                    reply.extend_from_slice(&[0; 4]);
                    let _ = socket.send_to(&reply, from);
                }
            }
        });
        at
    }

    fn registered(sessions: &Sessions, pid: u32, at: SocketAddrV4, rvcid: u32) {
        let url = format!("udp:/address={};port={};PID={pid};RVCID={rvcid}", at.ip(), at.port())
            .parse::<StationURL>()
            .unwrap();
        sessions.set_urls(pid, format!("prudp:/address={};port=2347", at.ip()).parse().unwrap(), vec![url]);
    }

    fn run_until_settled(a: &Announce, pid: u32) -> Option<Checked> {
        let until = Instant::now() + WAIT + Duration::from_secs(1);
        while Instant::now() < until {
            a.work(&logger());
            if a.state().pending.values().all(|p| p.pid != pid) {
                return a.checked(pid);
            }
        }
        a.checked(pid)
    }

    #[test]
    fn a_game_that_answers_is_open_one_that_doesnt_isnt() {
        let sessions = Sessions::default();
        let a = Announce::new().unwrap();
        let answering = game(77);
        registered(&sessions, 1, answering, 77);
        a.check(&logger(), &sessions, 1, false);
        let c = run_until_settled(&a, 1).unwrap();
        assert!(c.open, "{c:?}");
        assert_eq!(c.to, answering);

        // Nothing there.
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(quiet) = silent.local_addr().unwrap() else { panic!() };
        registered(&sessions, 2, quiet, 78);
        a.check(&logger(), &sessions, 2, false);
        assert!(!run_until_settled(&a, 2).unwrap().open);

        // Another game at that address: not this player's.
        let other = game(99);
        registered(&sessions, 3, other, 79);
        a.check(&logger(), &sessions, 3, false);
        assert!(!run_until_settled(&a, 3).unwrap().open, "another game answered");
    }

    #[test]
    fn the_same_address_isnt_checked_again_soon() {
        let sessions = Sessions::default();
        let a = Announce::new().unwrap();
        registered(&sessions, 1, game(5), 5);
        a.check(&logger(), &sessions, 1, false);
        a.check(&logger(), &sessions, 1, false);
        assert_eq!(a.state().pending.len(), 1, "one check at a time");
        run_until_settled(&a, 1);
        a.check(&logger(), &sessions, 1, false);
        assert!(a.state().pending.is_empty(), "checked a moment ago");
        assert!(a.checked(1).is_some());
        a.check(&logger(), &sessions, 1, true);
        assert_eq!(a.state().pending.len(), 1, "a forward was just made: checked again");
        assert_eq!(a.checked(1), None, "nothing said while it's checked again");
        run_until_settled(&a, 1);
        a.forget(1);
        assert_eq!(a.checked(1), None, "signed in afresh");
        a.check(&logger(), &sessions, 1, false);
        assert_eq!(a.state().pending.len(), 1, "checked again");
    }
}
