//! `POST /api/check-host` with `{"web_port": 80, "udp_ports": [21030, 21031]}`: whether a
//! Cross-Com server hosted at the asker's own address answers from outside. A host's launcher
//! asks a server elsewhere (the community's) once its router has forwarded the ports, to know
//! friends on the internet will reach it.
//!
//! The web port is asked for `/api/info` over TCP (a Cross-Com server says its game there); each
//! UDP port gets the address echo every Cross-Com server answers on its sign-in ports (a PRUDP
//! user packet with a nonce; see `address_echo`). Only the asker's own address is checked, never
//! another, and only a public one; a few times a minute at most. Answers
//! `{"address": ..., "web": "ok" | "no answer" | "another server", "udp": [{"port": ..., "answers": bool}]}`.

use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::TcpStream;
use std::net::UdpSocket;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use crate::signup::error;
use crate::signup::Answer;

pub const PATH: &str = "/api/check-host";
/// The biggest request body taken.
pub const MAX_BODY: usize = 256;
/// The most UDP ports checked at once.
const MAX_UDP: usize = 4;
/// Checks per address per minute.
const PER_MINUTE: u32 = 3;
/// Addresses counted at once.
const MAX_COUNTED: usize = 10_000;
/// How long the web port has to answer, and each UDP port.
const WEB_WAIT: Duration = Duration::from_secs(5);
const UDP_WAIT: Duration = Duration::from_secs(3);
/// The most of the web port's answer read.
const MAX_ANSWER: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    web_port: u16,
    #[serde(default)]
    udp_ports: Vec<u16>,
}

#[derive(Default)]
pub struct CheckHost {
    counted: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl CheckHost {
    /// Whether `client` may have another check now.
    fn allow(&self, client: IpAddr) -> bool {
        let mut counted = self.counted.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if counted.len() >= MAX_COUNTED {
            counted.retain(|_, (at, _)| now.duration_since(*at) < Duration::from_secs(60));
        }
        let entry = counted.entry(client).or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= PER_MINUTE
    }

    /// Checks the server at `client` (the asker's own address) as `body` asks.
    pub fn handle(&self, client: IpAddr, body: &[u8]) -> Answer {
        let Ok(request) = serde_json::from_slice::<Request>(body) else {
            return error("400 Bad Request", "expected {\"web_port\", \"udp_ports\"}");
        };
        if request.web_port == 0 || request.udp_ports.len() > MAX_UDP || request.udp_ports.contains(&0) {
            return error("400 Bad Request", "a web port and up to 4 UDP ports, none 0");
        }
        let client = client.to_canonical();
        if !client.is_ipv4() || !crate::here::is_public(client) {
            return error("400 Bad Request", "only a public IPv4 address can be checked from outside");
        }
        if !self.allow(client) {
            return error("429 Too Many Requests", "too many checks from here; try again in a minute");
        }
        let web = web(SocketAddr::new(client, request.web_port));
        let udp: Vec<_> = request
            .udp_ports
            .iter()
            .map(|&port| json!({ "port": port, "answers": udp(SocketAddr::new(client, port)) }))
            .collect();
        ("200 OK", json!({ "address": client.to_string(), "web": web, "udp": udp }).to_string())
    }
}

/// What the web port at `to` said to `GET /api/info`.
fn web(to: SocketAddr) -> &'static str {
    let Ok(mut stream) = TcpStream::connect_timeout(&to, WEB_WAIT) else {
        return "no answer";
    };
    let _ = stream.set_read_timeout(Some(WEB_WAIT));
    let _ = stream.set_write_timeout(Some(WEB_WAIT));
    if stream.write_all(b"GET /api/info HTTP/1.1\r\nHost: check\r\nConnection: close\r\n\r\n").is_err() {
        return "no answer";
    }
    let mut answer = Vec::new();
    let _ = stream.take(MAX_ANSWER).read_to_end(&mut answer);
    if answer.is_empty() {
        "no answer"
    } else if String::from_utf8_lossy(&answer).contains(&format!("\"game\":\"{}\"", crate::api::GAME)) {
        "ok"
    } else {
        "another server"
    }
}

/// Whether the UDP port at `to` answers the address echo.
fn udp(to: SocketAddr) -> bool {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else { return false };
    let nonce: [u8; 8] = rand::random();
    let request = echo_packet(nonce);
    let started = Instant::now();
    let mut buf = [0u8; 256];
    for _ in 0..3 {
        if socket.send_to(&request, to).is_err() {
            return false;
        }
        let round = Instant::now();
        while round.elapsed() < UDP_WAIT / 3 {
            let left = (UDP_WAIT / 3).saturating_sub(round.elapsed()).max(Duration::from_millis(10));
            if socket.set_read_timeout(Some(left)).is_err() {
                return false;
            }
            match socket.recv_from(&mut buf) {
                Ok((n, from)) if from == to && buf[..n].starts_with(&nonce) => return true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if started.elapsed() >= UDP_WAIT {
            break;
        }
    }
    false
}

/// The address echo's request (as the game and the client send it): a PRUDP v0 user packet from
/// virtual port 1 to virtual port 1, carrying `nonce`.
fn echo_packet(nonce: [u8; 8]) -> Vec<u8> {
    const VPORT: u8 = 0x71;
    const USER: u8 = 5;
    let mut data = vec![VPORT, VPORT, USER, 0, 0, 0, 0, 0, 0, 0];
    data.extend_from_slice(&nonce);
    let whole = data.len() - data.len() % 4;
    let sum = data[..whole]
        .chunks_exact(4)
        .fold(0u32, |acc, c| acc.wrapping_add(u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
    let checksum = sum.to_le_bytes().iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    data.push(data[whole..].iter().fold(checksum, |acc, b| acc.wrapping_add(*b)));
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same packet the client sends (`crosscom_client_net::echo`).
    #[test]
    fn the_echo_is_the_clients() {
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(echo_packet(nonce), crosscom_client_net::echo::packet(nonce));
    }

    #[test]
    fn only_a_public_address_and_a_few_times_a_minute() {
        let c = CheckHost::default();
        let body = br#"{"web_port":80,"udp_ports":[21030]}"#;
        assert_eq!(c.handle("192.168.1.7".parse().unwrap(), body).0, "400 Bad Request", "a private address");
        assert_eq!(c.handle("100.92.170.101".parse().unwrap(), body).0, "400 Bad Request", "a VPN's");
        assert_eq!(c.handle("203.0.114.7".parse().unwrap(), br#"{"web_port":0}"#).0, "400 Bad Request");
        assert_eq!(c.handle("203.0.114.7".parse().unwrap(), br#"{"web_port":80,"udp_ports":[1,2,3,4,5]}"#).0, "400 Bad Request");
        assert_eq!(c.handle("203.0.114.7".parse().unwrap(), b"{oops").0, "400 Bad Request");
        let here: IpAddr = "203.0.114.9".parse().unwrap();
        for _ in 0..PER_MINUTE {
            assert!(c.allow(here));
        }
        assert!(!c.allow(here), "past the limit");
    }

    /// A Cross-Com server's web port and echo, on this machine, answer as one hosted would.
    #[test]
    fn finds_a_server_answering() {
        let web_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let web_at = web_listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut s in web_listener.incoming().flatten() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let body = format!("{{\"game\":\"{}\"}}", crate::api::GAME);
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            }
        });
        assert_eq!(web(web_at), "ok");
        let echo = UdpSocket::bind("127.0.0.1:0").unwrap();
        let echo_at = echo.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            while let Ok((n, from)) = echo.recv_from(&mut buf) {
                // The payload (the nonce) back, as the server's echo does.
                let _ = echo.send_to(&buf[10..n - 1], from);
            }
        });
        assert!(udp(echo_at));
        let closed = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(!udp(closed));
        let nothing = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_eq!(web(nothing), "no answer");
    }
}
