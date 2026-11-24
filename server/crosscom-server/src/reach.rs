//! Reaching players' games: what their clients say about their routers, with no password;
//! through a proxy, over HTTPS only (`online_config`).
//!
//! - `POST /api/forward` with `{"local": "192.168.1.20:2346", "forward": {"port": 40000, "how":
//!   "UPnP"} | null, "nat": "consistent"}`: the client's report for the game's peer-to-peer
//!   socket on its network. It's matched to the player seen at the caller's address whose game
//!   registered that socket; a forward is then given to other players first (see
//!   `Sessions::urls_seen_from`), and both go in the connection records (`connections`). Answers
//!   `{"address": IP, "port": PORT | null}`: where the server sees the caller, and the socket's
//!   public port once the game has registered it, which the client asks its router to forward.
//! - `POST /api/reach` with `{"port": 40001, "nonce": "0123abcd..."}`: sends `crosscom-reach `
//!   and the nonce, once, to that port at the caller's address, from a socket that has sent
//!   nothing else, and answers `{"address": IP}`. The client's connection test forwards a port
//!   that has sent nothing, so the packet arrives only if the forward works. Only to the
//!   caller's own address, smaller than the request, and a few a minute from each address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use crate::sessions::Reachability;
use crate::sessions::Sessions;
use crate::signup::error;
use crate::signup::Answer;

pub const FORWARD_PATH: &str = "/api/forward";
pub const REACH_PATH: &str = "/api/reach";
/// The biggest request body taken.
pub const MAX_BODY: usize = 512;
const REACH_PREFIX: &[u8] = b"crosscom-reach ";
/// Calls from one address per minute: a client renews its forward every 10 minutes, and its
/// connection test calls each once.
const FORWARDS_PER_MINUTE: u32 = 12;
const REACHES_PER_MINUTE: u32 = 4;
/// Addresses counted at once.
const MAX_COUNTED: usize = 10_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardRequest {
    /// Whose report it is (older clients don't say).
    #[serde(default)]
    account: String,
    /// The game token the player's game signs in with (see `accounts`): proof the report is the
    /// player's own, from a client that has one.
    #[serde(default)]
    token: Option<String>,
    local: String,
    forward: Option<Forward>,
    #[serde(default)]
    nat: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Forward {
    port: u16,
    how: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReachRequest {
    port: u16,
    nonce: String,
}

/// The player a router report was matched to, and whether its game announced the socket the
/// report names, and so it was kept (else the game announced another adapter's address, or the
/// report wasn't the player's own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Matched {
    pub pid: u32,
    pub announced: bool,
    /// The report brings a forward it didn't have before (or on another port).
    pub new_forward: bool,
}

pub struct Reach {
    sessions: Arc<Sessions>,
    /// The accounts: a report is the named player's, when their game is seen where it came from.
    accounts: Arc<crate::accounts::Accounts>,
    /// Calls per address in the current minute: (minute's start, forwards, reaches).
    counts: Mutex<HashMap<IpAddr, (Instant, u32, u32)>>,
}

/// The words a report may use for how the router maps ports, and how it forwards.
const NAT_KINDS: [&str; 3] = ["consistent", "symmetric", "unknown"];
const HOW: [&str; 3] = ["UPnP", "PCP", "NAT-PMP"];

impl Reach {
    pub fn new(sessions: Arc<Sessions>, accounts: Arc<crate::accounts::Accounts>) -> Self {
        Self {
            sessions,
            accounts,
            counts: Mutex::default(),
        }
    }

    /// Counts a call from `ip`: false when it's one too many this minute.
    fn allow(&self, ip: IpAddr, reach: bool) -> bool {
        let mut counts = self.counts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if counts.len() >= MAX_COUNTED {
            counts.retain(|_, (since, _, _)| now.duration_since(*since) < Duration::from_secs(60));
            if counts.len() >= MAX_COUNTED {
                return false;
            }
        }
        let entry = counts.entry(ip).or_insert((now, 0, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0, 0);
        }
        if reach {
            entry.2 += 1;
            entry.2 <= REACHES_PER_MINUTE
        } else {
            entry.1 += 1;
            entry.1 <= FORWARDS_PER_MINUTE
        }
    }

    /// `POST /api/forward` from `client` with `body`. The player it was matched to, if any, for
    /// the log.
    pub fn forward(&self, client: IpAddr, body: &[u8]) -> (Answer, Option<Matched>) {
        if !self.allow(client, false) {
            return (error("429 Too Many Requests", "too many reports from here; try again later"), None);
        }
        let Ok(request) = serde_json::from_slice::<ForwardRequest>(body) else {
            return (error("400 Bad Request", "expected {\"local\", \"forward\", \"nat\"}"), None);
        };
        let Ok(local) = request.local.parse::<SocketAddrV4>() else {
            return (error("400 Bad Request", "local is an IPv4 address and port"), None);
        };
        let nat = if NAT_KINDS.contains(&request.nat.as_str()) { request.nat } else { "unknown".to_owned() };
        let forward = match request.forward {
            Some(f) if f.port == 0 || !HOW.contains(&f.how.as_str()) => {
                return (error("400 Bad Request", "a forward is a port and how it was made (UPnP, PCP or NAT-PMP)"), None);
            }
            Some(f) => Some((f.port, f.how)),
            None => None,
        };
        // The player the report names, when their game is seen where it came from (and whether
        // their game announced the socket it names: else another adapter's address, see
        // `Reachability::local`); from an older client that names no one, the player whose game
        // announced that socket.
        let found = if request.account.is_empty() {
            self.sessions.peer_at(client, local).map(|f| (f, true))
        } else {
            self.accounts
                .pid(request.account.trim())
                .ok()
                .flatten()
                .and_then(|pid| self.sessions.socket_of(pid, client, local).map(|(port, announced)| ((pid, port), announced)))
        };
        // Kept only when the game announced the socket the report names. Anyone behind the same
        // public address can name the player (housemates, a guest on the Wi-Fi), and without a
        // password; a report naming any socket could send the player's joins to a forward of
        // their own, or point housemates' games at another machine on the network. The game
        // announcing it is the player's own word for it. (A game that announced another
        // adapter's address, a VPN's, gets the port to forward but nothing kept: its housemates
        // reach it as the game announced.)
        //
        // A report carrying the player's game token is the player's own whatever socket it names:
        // kept, so a game that announced another adapter's address still gets the right one put
        // first for its housemates (`Sessions::unannounced_local`).
        let found = found.map(|((pid, port), announced)| {
            let own = announced && self.announces(pid, local);
            let proven = request
                .token
                .as_deref()
                .is_some_and(|t| self.accounts.token_owner(request.account.trim(), t).ok().flatten() == Some(pid));
            ((pid, port), own, own || proven)
        });
        let mut new_forward = false;
        if let Some(((pid, _), _, true)) = found {
            let before = self.sessions.reachability(pid).and_then(|r| r.forward.map(|(port, _)| port));
            new_forward = forward.as_ref().is_some_and(|(port, _)| before != Some(*port));
            self.sessions.set_reachability(
                pid,
                Reachability {
                    forward,
                    nat,
                    seen: client,
                    local,
                    at: Instant::now(),
                },
            );
        }
        let port = found.and_then(|((_, port), _, _)| port);
        // A new forward is checked afresh once this is answered: what was found before it isn't
        // what's in front of the game now.
        let open = if new_forward {
            None
        } else {
            found.and_then(|((pid, _), _, _)| crate::announce::open(pid))
        };
        let answer = json!({ "address": client.to_string(), "port": port, "open": open }).to_string();
        (("200 OK", answer), found.map(|((pid, _), announced, _)| Matched { pid, announced, new_forward }))
    }

    /// Whether `pid`'s game announced `local` as one of its peer-to-peer sockets (a `udp` station
    /// URL; at sign-in it registers only its connection to the server, which says nothing).
    fn announces(&self, pid: u32, local: SocketAddrV4) -> bool {
        let address = local.ip().to_string();
        self.sessions
            .registered(pid)
            .iter()
            .any(|u| u.scheme == "udp" && u.address == address && u.port == local.port())
    }

    /// `POST /api/reach` from `client` with `body`.
    pub fn reach(&self, client: IpAddr, body: &[u8]) -> Answer {
        let Ok(request) = serde_json::from_slice::<ReachRequest>(body) else {
            return error("400 Bad Request", "expected {\"port\", \"nonce\"}");
        };
        let nonce_ok = (8..=32).contains(&request.nonce.len()) && request.nonce.bytes().all(|b| b.is_ascii_hexdigit());
        if !nonce_ok || request.port < 1024 {
            return error("400 Bad Request", "the port is 1024 or above, and the nonce 8 to 32 hex digits");
        }
        let IpAddr::V4(ip) = client else {
            return error("400 Bad Request", "IPv4 only, as the game");
        };
        if !self.allow(client, true) {
            return error("429 Too Many Requests", "too many tests from here; try again in a minute");
        }
        let mut packet = REACH_PREFIX.to_vec();
        packet.extend_from_slice(request.nonce.as_bytes());
        let sent = UdpSocket::bind("0.0.0.0:0").and_then(|s| s.send_to(&packet, SocketAddr::from((ip, request.port))));
        if sent.is_err() {
            return error("500 Internal Server Error", "couldn't send the packet");
        }
        ("200 OK", json!({ "address": client.to_string() }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use quazal::rmc::types::StationURL;

    use super::*;

    /// A server with Kozak's game registered at 198.51.100.7 (public port 56877, 192.168.1.20 on
    /// its network) and Housemate's account, whose game hasn't registered. Kozak's pid.
    fn reach() -> (Reach, u32) {
        let sessions = Arc::new(Sessions::default());
        let accounts = Arc::new(crate::accounts::Accounts::open(std::path::Path::new(":memory:")).unwrap());
        let kozak = accounts.add("Kozak", "hunter22").unwrap();
        accounts.add("Housemate", "hunter23").unwrap();
        let url = |a: &str, port: u16| format!("udp:/address={a};port={port};PID=1;RVCID=2").parse::<StationURL>().unwrap();
        sessions.set_urls(
            kozak,
            "prudp:/address=198.51.100.7;port=2347".parse().unwrap(),
            vec![url("198.51.100.7", 56877), url("192.168.1.20", 2346)],
        );
        (Reach::new(sessions, accounts), kozak)
    }

    fn matched(pid: u32, announced: bool) -> Option<Matched> {
        Some(Matched {
            pid,
            announced,
            new_forward: false,
        })
    }

    fn json_of(answer: &Answer) -> serde_json::Value {
        serde_json::from_str(&answer.1).unwrap()
    }

    #[test]
    fn a_report_is_the_named_players_where_their_game_is() {
        let (r, k) = reach();
        let home: IpAddr = "198.51.100.7".parse().unwrap();
        let (answer, pid) = r.forward(home, br#"{"account":"Kozak","local":"192.168.1.20:2346","forward":null,"nat":"symmetric"}"#);
        assert_eq!((answer.0, pid), ("200 OK", matched(k, true)));
        assert_eq!(json_of(&answer)["port"], 56877);
        assert_eq!(r.sessions.reachability(k).unwrap().nat, "symmetric");
        let (answer, pid) = r.forward(
            home,
            br#"{"account":"Kozak","local":"192.168.1.20:2346","forward":{"port":40000,"how":"UPnP"},"nat":"consistent"}"#,
        );
        let new = Matched {
            new_forward: true,
            ..matched(k, true).unwrap()
        };
        assert_eq!((answer.0, pid), ("200 OK", Some(new)), "a new forward: its player's address is checked afresh");
        assert_eq!(json_of(&answer)["open"], serde_json::Value::Null, "nothing said about it before the check");
        assert_eq!(r.sessions.forward_of(k), Some(40000));
        let (_, pid) = r.forward(
            home,
            br#"{"account":"Kozak","local":"192.168.1.20:2346","forward":{"port":40000,"how":"UPnP"},"nat":"consistent"}"#,
        );
        assert_eq!(pid, matched(k, true), "the same forward renewed: nothing new");
        // A housemate's client, its game not registered yet: never taken for Kozak's.
        let (answer, pid) = r.forward(
            home,
            br#"{"account":"Housemate","local":"192.168.1.21:2346","forward":{"port":40001,"how":"UPnP"},"nat":"consistent"}"#,
        );
        assert_eq!((answer.0, pid), ("200 OK", None));
        assert_eq!(json_of(&answer)["port"], serde_json::Value::Null);
        assert_eq!(json_of(&answer)["address"], "198.51.100.7");
        assert_eq!(r.sessions.forward_of(k), Some(40000), "Kozak's untouched");
        // Kozak's name from elsewhere: not where Kozak's game is.
        let (_, pid) = r.forward(
            "203.0.113.9".parse().unwrap(),
            br#"{"account":"Kozak","local":"192.168.1.20:2346","forward":{"port":1,"how":"UPnP"},"nat":"x"}"#,
        );
        assert_eq!(pid, None);
        assert_eq!(r.sessions.forward_of(k), Some(40000));
        // An older client, naming no one: the game that announced the socket.
        let (_, pid) = r.forward(home, br#"{"local":"192.168.1.20:2346","forward":null,"nat":"consistent"}"#);
        assert_eq!(pid, matched(k, true));
        let (_, pid) = r.forward(home, br#"{"local":"192.168.1.21:2346","forward":null,"nat":"consistent"}"#);
        assert_eq!(pid, None);
    }

    /// A report carrying the player's game token is theirs, whatever socket it names (the game
    /// announced a VPN adapter's address in its place): kept. Another account's token isn't proof.
    #[test]
    fn a_report_with_the_players_token_is_kept_wherever_it_points() {
        let (r, k) = reach();
        let home: IpAddr = "198.51.100.7".parse().unwrap();
        let token = r.accounts.issue_token(k).unwrap();
        let body = format!(r#"{{"account":"Kozak","token":"{token}","local":"10.8.0.5:2346","forward":null,"nat":"consistent"}}"#);
        let (answer, pid) = r.forward(home, body.as_bytes());
        assert_eq!((answer.0, pid), ("200 OK", matched(k, false)), "not the announced socket");
        assert_eq!(r.sessions.reachability(k).map(|r| r.local.to_string()), Some("10.8.0.5:2346".into()), "but kept");
        let housemate = r.accounts.pid("Housemate").unwrap().unwrap();
        let theirs = r.accounts.issue_token(housemate).unwrap();
        let body = format!(r#"{{"account":"Kozak","token":"{theirs}","local":"192.168.1.66:2346","forward":{{"port":40066,"how":"UPnP"}},"nat":"consistent"}}"#);
        let _ = r.forward(home, body.as_bytes());
        assert_eq!(r.sessions.forward_of(k), None, "another account's token proves nothing");
    }

    /// A report naming a socket the game didn't announce (a VPN adapter's address in the game's
    /// place, or someone else at the same public address naming the player): answered, so the
    /// client can forward the right port, but nothing kept.
    #[test]
    fn a_report_naming_a_socket_the_game_didnt_announce_isnt_kept() {
        let (r, k) = reach();
        let home: IpAddr = "198.51.100.7".parse().unwrap();
        let (answer, pid) = r.forward(home, br#"{"account":"Kozak","local":"10.8.0.5:2346","forward":null,"nat":"consistent"}"#);
        assert_eq!((answer.0, pid), ("200 OK", matched(k, false)));
        assert_eq!(json_of(&answer)["port"], 56877, "the client can forward the right port");
        assert_eq!(r.sessions.reachability(k), None);
        // A housemate naming Kozak, with a forward of their own: Kozak's joins aren't sent there.
        let (answer, pid) = r.forward(
            home,
            br#"{"account":"Kozak","local":"192.168.1.66:2346","forward":{"port":40066,"how":"UPnP"},"nat":"consistent"}"#,
        );
        assert_eq!((answer.0, pid), ("200 OK", matched(k, false)), "not a new forward: nothing to check");
        assert_eq!(r.sessions.forward_of(k), None);
        // Kozak's own report, naming the socket its game announced, is kept.
        r.forward(
            home,
            br#"{"account":"Kozak","local":"192.168.1.20:2346","forward":{"port":40000,"how":"UPnP"},"nat":"consistent"}"#,
        );
        assert_eq!(r.sessions.forward_of(k), Some(40000));
        let (_, pid) = r.forward(home, br#"{"account":"Kozak","local":"192.168.1.66:2346","forward":null,"nat":"symmetric"}"#);
        assert_eq!(pid, matched(k, false));
        assert_eq!(r.sessions.forward_of(k), Some(40000), "a spoofed report doesn't take it away either");
        assert_eq!(r.sessions.reachability(k).unwrap().nat, "consistent");
    }

    /// Signed in, not yet in Multiplayer: the game has announced no peer-to-peer socket, so a
    /// report can't be matched to one and isn't kept (the client waits for the port anyway).
    #[test]
    fn nothing_kept_before_the_game_announces_its_socket() {
        let (r, _) = reach();
        let pepper = r.accounts.add("Pepper", "hunter24").unwrap();
        r.sessions.set_urls(
            pepper,
            "prudp:/address=198.51.100.8;port=2347".parse().unwrap(),
            vec!["prudp:/address=198.51.100.8;port=2347;sid=15;type=3".parse().unwrap()],
        );
        let (answer, pid) = r.forward(
            "198.51.100.8".parse().unwrap(),
            br#"{"account":"Pepper","local":"192.168.1.30:2346","forward":{"port":40000,"how":"UPnP"},"nat":"consistent"}"#,
        );
        assert_eq!((answer.0, pid), ("200 OK", matched(pepper, false)));
        assert_eq!(json_of(&answer)["port"], serde_json::Value::Null);
        assert_eq!(r.sessions.reachability(pepper), None);
    }

    #[test]
    fn reports_that_dont_read_are_refused() {
        let (r, _) = reach();
        let home: IpAddr = "198.51.100.7".parse().unwrap();
        for body in [
            &b"{}"[..],
            br#"{"local":"nowhere","forward":null}"#,
            br#"{"local":"192.168.1.20:2346","forward":{"port":0,"how":"UPnP"}}"#,
            br#"{"local":"192.168.1.20:2346","forward":{"port":5,"how":"magic"}}"#,
        ] {
            assert_eq!(r.forward(home, body).0 .0, "400 Bad Request", "{}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn the_reach_packet_goes_to_the_caller_only_and_not_too_often() {
        let (r, _) = reach();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let port = socket.local_addr().unwrap().port();
        let me: IpAddr = "127.0.0.1".parse().unwrap();
        let body = format!(r#"{{"port":{port},"nonce":"0011aabbccdd"}}"#);
        let answer = r.reach(me, body.as_bytes());
        assert_eq!(answer.0, "200 OK");
        let mut buf = [0u8; 64];
        let n = socket.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"crosscom-reach 0011aabbccdd");
        assert!(n < body.len(), "smaller than the request");
        assert_eq!(r.reach(me, br#"{"port":80,"nonce":"0011aabbccdd"}"#).0, "400 Bad Request", "a low port");
        assert_eq!(r.reach(me, br#"{"port":40000,"nonce":"not hex!"}"#).0, "400 Bad Request");
        for _ in 1..REACHES_PER_MINUTE {
            assert_eq!(r.reach(me, body.as_bytes()).0, "200 OK");
        }
        assert_eq!(r.reach(me, body.as_bytes()).0, "429 Too Many Requests");
    }

    /// The client's connection test asks the address echo with a packet of its own making: the
    /// server reads it as the game's (a user packet, virtual port 1 to 1, the nonce its payload,
    /// a checksum it takes), and the client reads the answer it makes.
    #[test]
    fn the_clients_echo_is_read_as_the_games() {
        use quazal::prudp::packet::PacketType;
        use quazal::prudp::packet::QPacket;
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8];
        let data = crosscom_client_net::echo::packet(nonce);
        let ctx = quazal::Context::ghost_recon_future_soldier();
        let (read, len) = QPacket::from_bytes(&ctx, &data).unwrap();
        assert_eq!(len as usize, data.len());
        assert_eq!(read.packet_type, PacketType::User);
        assert_eq!((read.source.port, read.destination.port), (1, 1));
        assert_eq!(read.payload, nonce);
        read.validate(&ctx, &data).unwrap();
        let mut reply = read.payload.clone();
        reply.extend_from_slice(b"udp:/address=203.0.113.7;port=56877\0");
        assert_eq!(crosscom_client_net::echo::answer(&reply, &nonce), Some("203.0.113.7:56877".parse().unwrap()));
    }

    /// The client knows the server's reach packet for its nonce.
    #[test]
    fn the_client_knows_the_reach_packet() {
        let mut packet = REACH_PREFIX.to_vec();
        packet.extend_from_slice(b"0011aabbccdd");
        assert!(crosscom_client_net::api::is_reach(&packet, "0011aabbccdd"));
        assert_eq!(crosscom_client_net::api::REACH_PATH, REACH_PATH);
        assert_eq!(crosscom_client_net::api::FORWARD_PATH, FORWARD_PATH);
    }
}
