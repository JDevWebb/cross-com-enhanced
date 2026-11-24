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
//!
//! What it finds ([`Connection`]: how the router maps ports, the forward, and whether the server
//! reached the game from outside) is kept for the overlay ([`connection`]) and written to
//! [`STATUS_FILE`] in the game's folder for the launcher.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use log::info;
use log::warn;
use serde::Deserialize;
use serde::Serialize;

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
/// How long after a report the server is asked again for what its check found: it probes the
/// game when the report comes, and waits up to 1.5 s for the answer.
const VERDICT_AFTER: Duration = Duration::from_secs(3);
/// The file in the game's folder the launcher reads the last game's connection from.
pub const STATUS_FILE: &str = "crosscom-connection.json";

/// What the client found about reaching this PC's game.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    /// How the router maps ports: `consistent`, `symmetric` or `unknown`.
    pub nat: String,
    /// The router's forward of the game's port: how, and the public address (`UPnP
    /// 203.0.113.7:2346`).
    pub forward: Option<String>,
    /// Whether the server reached the game from outside, at the address other players get first
    /// (none until it has checked).
    pub open: Option<bool>,
    /// When it was found (Unix time).
    pub at: u64,
}

/// How reachable this PC's game is, for the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachable {
    /// Any player can join its matches.
    Open,
    /// Most can: those who can punch through.
    Moderate,
    /// Few can, and it can join only hosts anyone can reach.
    Strict,
    /// Not known yet.
    Checking,
}

impl Connection {
    #[must_use]
    pub fn reachable(&self) -> Reachable {
        match self.open {
            Some(true) => Reachable::Open,
            Some(false) if self.nat == "symmetric" && self.forward.is_none() => Reachable::Strict,
            Some(false) => Reachable::Moderate,
            None => Reachable::Checking,
        }
    }
}

impl Reachable {
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Reachable::Open => "Open",
            Reachable::Moderate => "Moderate",
            Reachable::Strict => "Strict",
            Reachable::Checking => "Checking...",
        }
    }

    /// What it means, and what to do about it.
    #[must_use]
    pub fn advice(self) -> &'static str {
        match self {
            Reachable::Open => "Any player can join the matches you host.",
            Reachable::Moderate => "Most players can join the matches you host, but players behind strict routers can't (they aren't sent to your lobbies). Turning on UPnP in your router opens it to everyone.",
            Reachable::Strict => "Your router gives each connection a different port and doesn't forward the game's, so few players can join the matches you host, and Quick Match only sends you to hosts anyone can reach. Turn on UPnP in your router (often under Advanced or NAT).",
            Reachable::Checking => "The server checks once the game is in Multiplayer.",
        }
    }
}

static CONNECTION: Mutex<Option<Connection>> = Mutex::new(None);
static STATUS_PATH: OnceLock<PathBuf> = OnceLock::new();

/// What the client found so far (none before it has looked at the router).
#[must_use]
pub fn connection() -> Option<Connection> {
    CONNECTION.try_lock().ok().and_then(|c| c.clone())
}

/// Changes what's known, and writes it for the launcher.
fn update(change: impl FnOnce(&mut Connection)) {
    let now = {
        let Ok(mut c) = CONNECTION.lock() else { return };
        let c = c.get_or_insert_with(Connection::default);
        change(c);
        c.at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        c.clone()
    };
    if let Some(path) = STATUS_PATH.get() {
        if let Ok(json) = serde_json::to_vec_pretty(&now) {
            if let Err(e) = std::fs::write(path, json) {
                warn!("port forward: couldn't write {}: {e}", path.display());
            }
        }
    }
}

/// Starts keeping the forward for the game's connection to `server`, as `account`, writing what
/// it finds into `game_dir` (see [`STATUS_FILE`]).
pub fn spawn(server: String, account: String, token: Option<String>, game_dir: Option<PathBuf>) {
    if let Some(dir) = game_dir {
        let _ = STATUS_PATH.set(dir.join(STATUS_FILE));
    }
    let started = thread::Builder::new().name("port forward".into()).spawn(move || run(&server, &account, token.as_deref()));
    if let Err(e) = started {
        warn!("port forward: couldn't start: {e}");
    }
}

/// What [`spawn`]'s thread does, until the process ends (the test bot runs it too).
/// `token`: the game token, sent with each report as proof it's this player's (never a password).
pub fn run(server: &str, account: &str, token: Option<&str>) {
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
    update(|c| c.nat = nat.as_str().into());
    let report = |forward: Option<api::Forward>| api::ForwardReport {
        account: account.to_owned(),
        token: token.filter(|t| crosscom_client_common::config::is_game_token(t)).map(str::to_owned),
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
                update(|c| c.open = a.open);
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
                update(|c| c.forward = Some(format!("{} {}", mapping.how, mapping.public)));
                said(api::forward(server, &report(Some(forward.clone()))));
                // The server checks the game when the report comes: what it found, a moment on.
                thread::sleep(VERDICT_AFTER);
                said(api::forward(server, &report(Some(forward))));
                // Kept until its lease ends: dropping it doesn't take it away.
                drop(mapping);
                thread::sleep(RENEW);
            }
            Err(e) => {
                info!("port forward: the router didn't forward the game's port: {e}");
                update(|c| c.forward = None);
                said(api::forward(server, &report(None)));
                thread::sleep(VERDICT_AFTER);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(nat: &str, forward: bool, open: Option<bool>) -> Connection {
        Connection {
            nat: nat.into(),
            forward: forward.then(|| "UPnP 203.0.113.7:2346".into()),
            open,
            at: 0,
        }
    }

    #[test]
    fn how_reachable_the_game_is() {
        assert_eq!(connection("consistent", true, Some(true)).reachable(), Reachable::Open);
        assert_eq!(connection("symmetric", true, Some(true)).reachable(), Reachable::Open, "forwarded");
        assert_eq!(connection("consistent", false, Some(false)).reachable(), Reachable::Moderate);
        assert_eq!(connection("symmetric", false, Some(false)).reachable(), Reachable::Strict);
        assert_eq!(
            connection("symmetric", true, Some(false)).reachable(),
            Reachable::Moderate,
            "a forward that let nothing in yet"
        );
        assert_eq!(connection("consistent", false, None).reachable(), Reachable::Checking);
    }

    #[test]
    fn the_launcher_reads_what_the_client_writes() {
        let c = connection("symmetric", false, Some(false));
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Connection>(&json).unwrap(), c);
        assert_eq!(serde_json::from_str::<Connection>("{}").unwrap().reachable(), Reachable::Checking);
    }
}
