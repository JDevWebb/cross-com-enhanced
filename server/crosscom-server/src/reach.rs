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

pub struct Reach {
    sessions: Arc<Sessions>,
    /// Calls per address in the current minute: (minute's start, forwards, reaches).
    counts: Mutex<HashMap<IpAddr, (Instant, u32, u32)>>,
}

/// The words a report may use for how the router maps ports, and how it forwards.
const NAT_KINDS: [&str; 3] = ["consistent", "symmetric", "unknown"];
const HOW: [&str; 3] = ["UPnP", "PCP", "NAT-PMP"];

impl Reach {
    pub fn new(sessions: Arc<Sessions>) -> Self {
        Self {
            sessions,
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
    pub fn forward(&self, client: IpAddr, body: &[u8]) -> (Answer, Option<u32>) {
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
        let found = self.sessions.peer_at(client, local);
        if let Some((pid, _)) = found {
            self.sessions.set_reachability(
                pid,
                Reachability {
                    forward,
                    nat,
                    seen: client,
                    at: Instant::now(),
                },
            );
        }
        let port = found.and_then(|(_, port)| port);
        (("200 OK", json!({ "address": client.to_string(), "port": port }).to_string()), found.map(|(pid, _)| pid))
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

    fn reach() -> Reach {
        let sessions = Arc::new(Sessions::default());
        let url = |a: &str, port: u16| format!("udp:/address={a};port={port};PID=1;RVCID=2").parse::<StationURL>().unwrap();
        sessions.set_urls(
            7,
            "prudp:/address=198.51.100.7;port=2347".parse().unwrap(),
            vec![url("198.51.100.7", 56877), url("192.168.1.20", 2346)],
        );
        Reach::new(sessions)
    }

    fn json_of(answer: &Answer) -> serde_json::Value {
        serde_json::from_str(&answer.1).unwrap()
    }

    #[test]
    fn a_report_is_matched_to_the_game_that_registered_its_socket() {
        let r = reach();
        let home: IpAddr = "198.51.100.7".parse().unwrap();
        let (answer, pid) = r.forward(home, br#"{"local":"192.168.1.20:2346","forward":null,"nat":"symmetric"}"#);
        assert_eq!((answer.0, pid), ("200 OK", Some(7)));
        assert_eq!(json_of(&answer)["port"], 56877);
        assert_eq!(r.sessions.reachability(7).unwrap().nat, "symmetric");
        let (answer, pid) = r.forward(home, br#"{"local":"192.168.1.20:2346","forward":{"port":40000,"how":"UPnP"},"nat":"consistent"}"#);
        assert_eq!((answer.0, pid), ("200 OK", Some(7)));
        assert_eq!(r.sessions.forward_of(7), Some(40000));
        // Another player at that address, whose game hasn't registered: nothing is noted.
        let (answer, pid) = r.forward(home, br#"{"local":"192.168.1.21:2346","forward":{"port":40001,"how":"UPnP"},"nat":"consistent"}"#);
        assert_eq!((answer.0, pid), ("200 OK", None));
        assert_eq!(json_of(&answer)["port"], serde_json::Value::Null);
        assert_eq!(json_of(&answer)["address"], "198.51.100.7");
        // From elsewhere, the same socket is no one's.
        let (_, pid) = r.forward(
            "203.0.113.9".parse().unwrap(),
            br#"{"local":"192.168.1.20:2346","forward":{"port":1,"how":"UPnP"},"nat":"x"}"#,
        );
        assert_eq!(pid, None);
        assert_eq!(r.sessions.forward_of(7), Some(40000));
    }

    #[test]
    fn reports_that_dont_read_are_refused() {
        let r = reach();
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
        let r = reach();
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
