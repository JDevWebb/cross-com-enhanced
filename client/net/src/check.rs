//! The connection test: how well other players can reach this PC's game.
//!
//! - **UDP to the server:** the address echo on the server's two sign-in ports (`echo`). No
//!   answer means the game's traffic doesn't get through at all.
//! - **How the router maps ports:** both echoes from one socket. The same public port for both
//!   is a router other players can punch through to (consistent); a port per destination is a
//!   symmetric NAT, which they mostly can't.
//! - **Port forwarding:** the router asked to forward a fresh socket's port (UPnP, PCP or
//!   NAT-PMP, `portmap`), and the server asked to send a packet to it (`api::reach`). The socket
//!   has sent nothing, so only the forward can let that packet in.
//!
//! The launcher runs it from Settings; the client runs its parts in the game (`keep`).

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::ToSocketAddrs;
use std::net::UdpSocket;
use std::time::Duration;
use std::time::Instant;

use crosscom_client_common::server_info;

use crate::api;
use crate::echo;
use crate::portmap;

const ECHO_WAIT: Duration = Duration::from_secs(3);
const REACH_WAIT: Duration = Duration::from_secs(3);
/// How long the test's forward is asked for; it's taken away at the end anyway.
const TEST_LEASE: u32 = 120;

/// How the router maps this PC's ports to its public address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nat {
    /// One public port for a socket, whoever it sends to: players can punch through.
    Consistent,
    /// A public port per destination: players mostly can't punch through.
    Symmetric,
    /// Not found out (the server has one UDP port, or didn't answer).
    Unknown,
}

impl Nat {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Nat::Consistent => "consistent",
            Nat::Symmetric => "symmetric",
            Nat::Unknown => "unknown",
        }
    }
}

/// The router's forward: how it was asked, and the public address and port it gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Router {
    pub how: &'static str,
    pub public: SocketAddrV4,
}

/// What the test found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Where the server sees this PC's UDP, if it answered.
    pub seen: Option<SocketAddrV4>,
    pub nat: Nat,
    /// The router's forward, or why there's none.
    pub router: Result<Router, String>,
    /// Whether the server's packet came in through the forward.
    pub forward_works: Option<bool>,
    /// Why the server couldn't be tested, if it couldn't.
    pub problem: Option<String>,
}

/// The test's verdict, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Open,
    Moderate,
    Strict,
    Blocked,
}

impl Report {
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        if self.seen.is_none() {
            Verdict::Blocked
        } else if self.forward_works == Some(true) {
            Verdict::Open
        } else if self.nat == Nat::Symmetric {
            Verdict::Strict
        } else {
            Verdict::Moderate
        }
    }

    /// The verdict in a sentence.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.verdict() {
            Verdict::Open => "Open: other players can reach your game directly.".into(),
            Verdict::Moderate => "Moderate: you can play with most players. Two players whose routers are both strict may not reach each other.".into(),
            Verdict::Strict => "Strict: your router gives each connection a different port, so many players won't be able to reach you. \
                 Turning on UPnP on your router fixes this."
                .into(),
            Verdict::Blocked => match &self.problem {
                Some(p) => format!("Not tested: {p}"),
                None => "Blocked: the game's traffic to the server didn't get through. A firewall, or the network you're on, may be blocking UDP.".into(),
            },
        }
    }

    /// Each part of the test, in a line.
    #[must_use]
    pub fn details(&self) -> Vec<String> {
        let mut lines = Vec::new();
        match self.seen {
            Some(seen) => lines.push(format!("The server: reached, and it sees you at {}.", seen.ip())),
            None => lines.push("The server: no answer to the game's traffic.".into()),
        }
        lines.push(match self.nat {
            Nat::Consistent => "Your router: keeps one port per connection, so other players can reach you through it.".into(),
            Nat::Symmetric => "Your router: changes the port for each player (a strict NAT).".into(),
            Nat::Unknown => "Your router: couldn't tell how it handles ports.".into(),
        });
        match (&self.router, self.forward_works) {
            (Ok(r), Some(true)) => lines.push(format!("Port forwarding: works ({}).", r.how)),
            (Ok(r), _) => lines.push(format!(
                "Port forwarding: the router agreed ({}), but nothing came in through it. Another router may be in front of it.",
                r.how
            )),
            (Err(e), _) => lines.push(format!("Port forwarding: not available ({}).", short(e))),
        }
        if let (Some(seen), Ok(r)) = (self.seen, &self.router) {
            if seen.ip() != r.public.ip() {
                lines.push(format!(
                    "Your router's internet address ({}) isn't the one the server sees ({}): you may be behind a second router or a VPN.",
                    r.public.ip(),
                    seen.ip()
                ));
            }
        }
        lines
    }
}

/// The first reason of the ways tried, shortened for a line.
fn short(e: &str) -> String {
    if e.contains("isn't a public one") {
        return "your internet provider or another router is in front of yours".into();
    }
    if e.contains("no answer") && e.matches("no answer").count() >= 3 {
        return "the router didn't answer UPnP, PCP or NAT-PMP; it may be turned off in its settings".into();
    }
    e.chars().take(160).collect()
}

/// The server's UDP sign-in ports and its address, from its info.
pub fn server_udp(server: &str) -> Result<Vec<SocketAddr>, String> {
    let info = match server_info::fetch(server, Duration::from_secs(8)).map_err(|e| e.0)? {
        server_info::Reply::Info(info) => info,
        server_info::Reply::NoInfo(why) => return Err(format!("the server didn't say its ports ({why})")),
        server_info::Reply::OtherGame(game) => return Err(format!("that's a server for {game}")),
    };
    if info.udp_ports.is_empty() {
        return Err("the server is too old for the connection test".into());
    }
    let host = server_info::address(server).0;
    let ip = (host.as_str(), 0)
        .to_socket_addrs()
        .map_err(|e| format!("couldn't look up {host}: {e}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| format!("{host} has no IPv4 address"))?
        .ip();
    Ok(info.udp_ports.iter().take(2).map(|p| SocketAddr::new(ip, *p)).collect())
}

/// Where the server sees this PC's UDP, and how the router maps ports.
#[must_use]
pub fn mapping(ports: &[SocketAddr]) -> (Option<SocketAddrV4>, Nat) {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return (None, Nat::Unknown);
    };
    let first = ports.first().and_then(|to| echo::ask(&socket, *to, ECHO_WAIT).ok());
    let second = ports.get(1).and_then(|to| first.and_then(|_| echo::ask(&socket, *to, ECHO_WAIT).ok()));
    let nat = match (first, second) {
        (Some(a), Some(b)) if a == b => Nat::Consistent,
        (Some(_), Some(_)) => Nat::Symmetric,
        _ => Nat::Unknown,
    };
    (first, nat)
}

/// Asks the router to forward a fresh socket's port, and the server to send to it: the forward,
/// and whether the server's packet came in through it. The forward is taken away after.
pub fn forwarding(server: &str, local: Option<Ipv4Addr>) -> (Result<Router, String>, Option<bool>) {
    let socket = match UdpSocket::bind((local.unwrap_or(Ipv4Addr::UNSPECIFIED), 0)) {
        Ok(s) => s,
        Err(e) => return (Err(e.to_string()), None),
    };
    let Ok(SocketAddr::V4(bound)) = socket.local_addr() else {
        return (Err("no local port".into()), None);
    };
    let mapping = match portmap::map(bound.port(), local, TEST_LEASE, "Cross-Com connection test") {
        Ok(m) => m,
        Err(e) => return (Err(e), None),
    };
    let router = Router {
        how: mapping.how,
        public: mapping.public,
    };
    let nonce = format!("{:016x}", u64::from_le_bytes(echo::nonce()));
    let works = match api::reach(server, router.public.port(), &nonce) {
        Ok(_) => Some(came_in(&socket, &nonce)),
        Err(e) => {
            log::warn!("connection test: the server couldn't send to the forward: {e}");
            None
        }
    };
    mapping.remove();
    (Ok(router), works)
}

/// Whether the server's packet for `nonce` arrives at `socket` within [`REACH_WAIT`].
fn came_in(socket: &UdpSocket, nonce: &str) -> bool {
    let started = Instant::now();
    let mut buf = [0u8; 128];
    while started.elapsed() < REACH_WAIT {
        let left = REACH_WAIT.saturating_sub(started.elapsed()).max(Duration::from_millis(10));
        if socket.set_read_timeout(Some(left)).is_err() {
            return false;
        }
        match socket.recv_from(&mut buf) {
            Ok((n, _)) if api::is_reach(&buf[..n], nonce) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// Whether `ip` is on a private network (a LAN, or a VPN such as Tailscale's 100.64/10): a
/// server there is reached without the router, so there's nothing to forward or test.
#[must_use]
pub fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

/// The whole test against `server`.
#[must_use]
pub fn run(server: &str) -> Report {
    let ports = match server_udp(server) {
        Ok(p) => p,
        Err(e) => {
            return Report {
                seen: None,
                nat: Nat::Unknown,
                router: Err(e.clone()),
                forward_works: None,
                problem: Some(e),
            }
        }
    };
    if ports.first().is_some_and(|p| private(p.ip())) {
        let why = "the server is on your own network or VPN, so there's nothing to test".to_owned();
        return Report {
            seen: None,
            nat: Nat::Unknown,
            router: Err(why.clone()),
            forward_works: None,
            problem: Some(why),
        };
    }
    let (seen, nat) = mapping(&ports);
    let (router, forward_works) = forwarding(server, None);
    Report {
        seen,
        nat,
        router,
        forward_works,
        problem: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(nat: Nat, forward_works: Option<bool>) -> Report {
        Report {
            seen: Some("203.0.113.7:40000".parse().unwrap()),
            nat,
            router: Ok(Router {
                how: "UPnP",
                public: "203.0.113.7:40001".parse().unwrap(),
            }),
            forward_works,
            problem: None,
        }
    }

    #[test]
    fn the_verdicts() {
        assert_eq!(report(Nat::Symmetric, Some(true)).verdict(), Verdict::Open, "a forward beats a strict NAT");
        assert_eq!(report(Nat::Consistent, Some(false)).verdict(), Verdict::Moderate);
        assert_eq!(report(Nat::Unknown, None).verdict(), Verdict::Moderate);
        assert_eq!(report(Nat::Symmetric, None).verdict(), Verdict::Strict);
        let blocked = Report {
            seen: None,
            ..report(Nat::Unknown, None)
        };
        assert_eq!(blocked.verdict(), Verdict::Blocked);
        assert!(blocked.summary().starts_with("Blocked"));
    }

    #[test]
    fn says_when_another_router_is_in_front() {
        let mut r = report(Nat::Consistent, Some(false));
        r.router = Ok(Router {
            how: "UPnP",
            public: "198.51.100.9:40001".parse().unwrap(),
        });
        let details = r.details();
        assert!(details.iter().any(|l| l.contains("second router")), "{details:?}");
        let r = Report {
            router: Err("UPnP: x; PCP: y; NAT-PMP: the router's external address 100.64.0.9 isn't a public one (...)".into()),
            ..report(Nat::Consistent, None)
        };
        assert!(r.details().iter().any(|l| l.contains("internet provider")));
    }

    #[test]
    fn private_servers() {
        for ip in ["192.168.1.2", "10.0.0.1", "100.100.1.1", "127.0.0.1"] {
            assert!(private(ip.parse().unwrap()), "{ip}");
        }
        assert!(!private("203.0.113.7".parse().unwrap()));
    }
}
