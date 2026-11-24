//! In the game: keeps the game's peer-to-peer port forwarded on the router while it runs, and
//! tells the server.
//!
//! The server introduces players to each other and has their games punch through their
//! routers, which works for most. A forward makes this PC reachable whatever the other player's
//! router does. On a thread of its own:
//!
//! 1. how the router maps ports (`check::mapping`), for the server's connection records;
//! 2. waits until the game has registered its peer-to-peer socket, which the server then knows
//!    the public port of (`api::forward` answers it);
//! 3. asks the router to forward that public port to the socket (the port the router already
//!    gives it, so a forward never takes another PC's), and checks once that a forward lets the
//!    server's packet in (`check::forwarding`): a router behind another one agrees to forwards
//!    that are no use;
//! 4. tells the server the forward, which it gives other players, and renews both while the
//!    game runs. The forward's lease ends half an hour after the game closes.
//!
//! Nothing is done for a server on a private network, nor when `port_forwarding = false`.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::thread;
use std::time::Duration;

use log::info;
use log::warn;

use crate::api;
use crate::check;
use crate::portmap;

/// The port the game binds for its peer-to-peer traffic.
pub const GAME_PEER_PORT: u16 = 2346;
/// How long the router is asked to keep the forward, and how often it's renewed.
const LEASE: u32 = 30 * 60;
const RENEW: Duration = Duration::from_secs(10 * 60);
/// How often the server is asked whether the game has registered yet.
const WAIT_FOR_GAME: Duration = Duration::from_secs(20);
/// After the router refused, or a forward let nothing in: when to try again.
const RETRY: Duration = Duration::from_secs(30 * 60);
const DESCRIPTION: &str = "Cross-Com (Future Soldier)";

/// Starts keeping the forward for the game's connection to `server`.
pub fn spawn(server: String, account: String) {
    let started = thread::Builder::new().name("port forward".into()).spawn(move || run(&server, &account));
    if let Err(e) = started {
        warn!("port forward: couldn't start: {e}");
    }
}

/// What [`spawn`]'s thread does, until the process ends (the test bot runs it too).
pub fn run(server: &str, account: &str) {
    let ports = match check::server_udp(server) {
        Ok(p) => p,
        Err(e) => {
            info!("port forward: not used: {e}");
            return;
        }
    };
    let Some(server_ip) = ports.first().map(SocketAddr::ip) else {
        return;
    };
    if check::private(server_ip) {
        info!("port forward: not needed for a server on this network ({server_ip})");
        return;
    }
    let IpAddr::V4(server_v4) = server_ip else {
        return;
    };
    let Some(local) = portmap::local_ip_towards(server_v4) else {
        warn!("port forward: no local address towards {server_ip}");
        return;
    };
    let (_, nat) = check::mapping(&ports);
    info!("port forward: the router maps ports {}", nat.as_str());
    let report = |forward: Option<api::Forward>| api::ForwardReport {
        account: account.to_owned(),
        local: format!("{local}:{GAME_PEER_PORT}"),
        forward,
        nat: nat.as_str().into(),
    };

    // The game's socket's public port, once the game has registered it.
    let seen = loop {
        match api::forward(server, &report(None)) {
            Ok(api::ForwardAnswer { port: Some(port), .. }) => break port,
            Ok(_) => thread::sleep(WAIT_FOR_GAME),
            Err(e) => {
                warn!("port forward: {e}");
                thread::sleep(RETRY);
            }
        }
    };
    let mut ask = portmap::port_to_ask(GAME_PEER_PORT, Some(seen));
    let mut checked = false;
    // What the server last said about reaching the game from outside, logged as it changes.
    let mut open = None;
    let mut said = |answer: Result<api::ForwardAnswer, String>| match answer {
        Ok(a) => {
            if a.open.is_some() && a.open != open {
                open = a.open;
                if a.open == Some(true) {
                    info!("port forward: the server reached the game from outside: anyone can join its matches");
                } else {
                    info!("port forward: the server couldn't reach the game from outside: only players who can punch through can join its matches");
                }
            }
        }
        Err(e) => warn!("port forward: couldn't tell the server: {e}"),
    };
    loop {
        match portmap::map_as(GAME_PEER_PORT, ask, Some(local), LEASE, DESCRIPTION) {
            Ok(mapping) => {
                if !checked {
                    let (_, works) = check::forwarding(server, Some(local));
                    if works != Some(true) {
                        info!("port forward: the router agreed ({}), but a forward let nothing in; not used", mapping.how);
                        mapping.remove();
                        thread::sleep(RETRY);
                        continue;
                    }
                    checked = true;
                    info!("port forward: {} forwards {} to {local}:{GAME_PEER_PORT}", mapping.how, mapping.public);
                }
                // Renewed as it is: the same port each time.
                ask = mapping.public.port();
                let forward = api::Forward {
                    port: mapping.public.port(),
                    how: mapping.how.into(),
                };
                said(api::forward(server, &report(Some(forward))));
                // Kept until its lease ends: dropping it doesn't take it away.
                drop(mapping);
                thread::sleep(RENEW);
            }
            Err(e) => {
                info!("port forward: the router didn't forward the game's port: {e}");
                said(api::forward(server, &report(None)));
                thread::sleep(RETRY);
            }
        }
    }
}

/// Whether the client keeps a forward for `server`: anything but an address on this network.
#[must_use]
pub fn wanted(server: &str) -> bool {
    let host = crosscom_client_common::server_info::address(server).0;
    !host.parse::<Ipv4Addr>().is_ok_and(|ip| check::private(IpAddr::V4(ip)))
}
