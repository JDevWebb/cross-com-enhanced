//! The server's two calls for reaching players, over HTTPS (`crosscom_client_common::http`), with
//! no password:
//!
//! - `POST /api/forward` tells it the port this PC's router forwards to the game's peer-to-peer
//!   socket (or that there's none, and how the router maps ports), and answers where the server
//!   sees that socket, once the game has told it;
//! - `POST /api/reach` has it send one small packet to a port of the address it sees the call
//!   come from, which shows whether a forward the router made lets players in.

use std::net::Ipv4Addr;
use std::net::SocketAddrV4;
use std::time::Duration;

use crosscom_client_common::server_info;
use serde::Deserialize;
use serde::Serialize;

pub const FORWARD_PATH: &str = "/api/forward";
pub const REACH_PATH: &str = "/api/reach";
/// What the server's packet for `/api/reach` starts with, before the nonce.
pub const REACH_PREFIX: &[u8] = b"crosscom-reach ";
const TIMEOUT: Duration = Duration::from_secs(8);

/// A forward the router made: the port players outside reach the game on, and how it was asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forward {
    pub port: u16,
    pub how: String,
}

/// `POST /api/forward`'s body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardReport {
    /// The account the game signs in with: whose report it is (the server takes it only from
    /// where that player's game is).
    pub account: String,
    /// The game token the game signs in with (`crosscom_client_common::config::is_game_token`):
    /// proof the report is the player's own. Never a password; left out when there's no token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The game's peer-to-peer socket on this PC's network, `192.168.1.20:2346`.
    pub local: String,
    pub forward: Option<Forward>,
    /// How the router maps ports: `consistent`, `symmetric` or `unknown` (see `check::Nat`).
    pub nat: String,
}

/// `POST /api/forward`'s answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ForwardAnswer {
    /// The address the server sees this PC at.
    pub address: Option<Ipv4Addr>,
    /// The public port of the game's peer-to-peer socket, once the game has registered it.
    pub port: Option<u16>,
    /// Whether the server reached the game, from a socket of its own, at the address other
    /// players get first: anyone can join its matches (none until it has checked).
    pub open: Option<bool>,
}

fn post_json<T: Serialize>(server: &str, path: &str, body: &T) -> Result<Vec<u8>, String> {
    let body = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let headers = [("Content-Type", "application/json".to_owned())];
    let (status, answer) = server_info::post(server, path, &headers, &body, TIMEOUT).map_err(|e| e.0)?;
    if status != 200 {
        let text = String::from_utf8_lossy(&answer);
        return Err(format!("{path}: {status} {}", text.trim()));
    }
    Ok(answer)
}

/// Tells the server about the game's socket on this network, and its forward if any.
pub fn forward(server: &str, report: &ForwardReport) -> Result<ForwardAnswer, String> {
    let answer = post_json(server, FORWARD_PATH, report)?;
    serde_json::from_slice(&answer).map_err(|e| format!("{FORWARD_PATH}: {e}"))
}

/// Has the server send [`REACH_PREFIX`] and `nonce` to `port` at the address it sees this PC at,
/// which it answers.
pub fn reach(server: &str, port: u16, nonce: &str) -> Result<Option<Ipv4Addr>, String> {
    #[derive(Serialize)]
    struct Request<'a> {
        port: u16,
        nonce: &'a str,
    }
    #[derive(Deserialize)]
    struct Answer {
        address: Option<Ipv4Addr>,
    }
    let answer = post_json(server, REACH_PATH, &Request { port, nonce })?;
    Ok(serde_json::from_slice::<Answer>(&answer).map_err(|e| format!("{REACH_PATH}: {e}"))?.address)
}

/// Whether `datagram` is the server's packet for `nonce`.
#[must_use]
pub fn is_reach(datagram: &[u8], nonce: &str) -> bool {
    datagram.strip_prefix(REACH_PREFIX).is_some_and(|rest| rest == nonce.as_bytes())
}

/// The forward's address and port as players outside see them.
#[must_use]
pub fn public(address: Ipv4Addr, forward: &Forward) -> SocketAddrV4 {
    SocketAddrV4::new(address, forward.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_reads_as_the_server_expects() {
        let report = ForwardReport {
            account: "Kozak".into(),
            token: None,
            local: "192.168.1.20:2346".into(),
            forward: Some(Forward { port: 40000, how: "UPnP".into() }),
            nat: "consistent".into(),
        };
        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            r#"{"account":"Kozak","local":"192.168.1.20:2346","forward":{"port":40000,"how":"UPnP"},"nat":"consistent"}"#
        );
        let answer: ForwardAnswer = serde_json::from_str(r#"{"address":"203.0.113.7","port":null}"#).unwrap();
        assert_eq!(answer.address, Some(Ipv4Addr::new(203, 0, 113, 7)));
        assert_eq!(answer.port, None);
    }

    #[test]
    fn knows_the_servers_packet() {
        assert!(is_reach(b"crosscom-reach 0011aabb", "0011aabb"));
        assert!(!is_reach(b"crosscom-reach 0011aabc", "0011aabb"));
        assert!(!is_reach(b"0011aabb", "0011aabb"));
    }
}
