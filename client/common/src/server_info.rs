//! What the server says about itself at `GET /api/info`, on its HTTP port (80), for the overlay:
//! its version, how many players are online and its news; and its leaderboard
//! (`GET /api/leaderboard?mode=...`).
//!
//! A small HTTP/1.0 client over a plain `TcpStream`, as the server answers on plain HTTP and the
//! client DLL shouldn't carry a TLS stack. Every field is optional; a server without the page
//! (404) is still a server that answered.

use std::collections::HashSet;
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::time::Duration;

use serde::Deserialize;

/// The page.
pub const PATH: &str = "/api/info";
/// The game a Cross-Com server for Future Soldier says it's for. A machine may host another
/// game's server behind the same Caddy, and an address alone reaches that one's page.
pub const GAME: &str = "ghost-recon-future-soldier";
/// The server's HTTP port, unless `server` names another (`host:port`).
pub const HTTP_PORT: u16 = 80;
/// The most of an answer that's read.
const MAX_RESPONSE: usize = 512 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    pub name: Option<String>,
    pub game: Option<String>,
    pub version: Option<String>,
    pub players_online: Option<u32>,
    pub news: Vec<NewsItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(default)]
pub struct NewsItem {
    pub title: String,
    pub body: String,
    /// `YYYY-MM-DD`.
    pub date: Option<String>,
    /// Where the game shows it: `multiplayer`, `campaign` or `guerrilla`, or everywhere (none).
    pub shows: Option<String>,
}

/// What the server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Info(ServerInfo),
    /// It answered, but without its info (no such page, or not as expected).
    NoInfo(String),
    /// Another game's server answered (by its name): the address reaches the wrong one.
    OtherGame(String),
}

/// The server couldn't be asked: not found, not answering, or not speaking HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreachable(pub String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The host and port of `server` (`host`, `host:port`, `[v6]` or `[v6]:port`).
#[must_use]
pub fn address(server: &str) -> (String, u16) {
    let server = server.trim();
    if let Some(rest) = server.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(HTTP_PORT);
            return (host.to_owned(), port);
        }
    }
    match server.split_once(':') {
        Some((host, port)) if !port.contains(':') => (host.to_owned(), port.parse().unwrap_or(HTTP_PORT)),
        _ => (server.to_owned(), HTTP_PORT),
    }
}

/// Asks `server` for its info, giving up after `timeout` at each step.
pub fn fetch(server: &str, timeout: Duration) -> Result<Reply, Unreachable> {
    reply(&get(server, PATH, timeout)?)
}

/// The leaderboard's page.
pub const LEADERBOARD_PATH: &str = "/api/leaderboard";
/// The modes the leaderboard has, as the server names them, and as shown.
pub const LEADERBOARD_MODES: [(&str, &str); 7] = [
    ("all", "All modes"),
    ("conflict", "Conflict"),
    ("decoy", "Decoy"),
    ("saboteur", "Saboteur"),
    ("siege", "Siege"),
    ("stockade", "Stockade"),
    ("takeover", "Takeover"),
];

/// One player on the leaderboard.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Ranked {
    pub rank: u32,
    pub name: String,
    pub wins: u32,
    pub losses: u32,
    pub kills: u32,
    pub deaths: u32,
    pub objectives: u32,
    pub seconds: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Leaderboard {
    pub mode: String,
    pub rows: Vec<Ranked>,
}

/// Asks `server` for its leaderboard of `mode` (one of [`LEADERBOARD_MODES`]). `Ok(None)`: the
/// server answered without one (an older server).
pub fn fetch_leaderboard(server: &str, mode: &str, timeout: Duration) -> Result<Option<Leaderboard>, Unreachable> {
    let mode: String = mode.chars().filter(char::is_ascii_alphanumeric).collect();
    let response = get(server, &format!("{LEADERBOARD_PATH}?mode={mode}"), timeout)?;
    Ok(body(&response)?.ok().and_then(|body| serde_json::from_slice(&body).ok()))
}

/// Asks `server` for `path` over plain HTTP/1.0, and reads the whole answer.
fn get(server: &str, path: &str, timeout: Duration) -> Result<Vec<u8>, Unreachable> {
    let (host, port) = address(server);
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| Unreachable(format!("couldn't look up {host}: {e}")))?
        .next()
        .ok_or_else(|| Unreachable(format!("{host} has no address")))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| Unreachable(format!("couldn't connect to {addr}: {e}")))?;
    let io = |e: std::io::Error| Unreachable(format!("{addr}: {e}"));
    stream.set_read_timeout(Some(timeout)).map_err(io)?;
    stream.set_write_timeout(Some(timeout)).map_err(io)?;
    let host_header = if host.contains(':') { format!("[{host}]") } else { host.clone() };
    let request = format!("GET {path} HTTP/1.0\r\nHost: {host_header}\r\nAccept: application/json\r\nConnection: close\r\nUser-Agent: Cross-Com\r\n\r\n");
    stream.write_all(request.as_bytes()).map_err(io)?;
    let mut response = Vec::new();
    stream.take(MAX_RESPONSE as u64).read_to_end(&mut response).map_err(io)?;
    Ok(response)
}

/// Reads an HTTP response as the info page.
pub fn reply(response: &[u8]) -> Result<Reply, Unreachable> {
    Ok(match body(response)? {
        Ok(body) => match serde_json::from_slice::<ServerInfo>(&body) {
            Ok(info) if info.game.as_deref() != Some(GAME) => Reply::OtherGame(info.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "another game's server".into())),
            Ok(info) => Reply::Info(info),
            Err(e) => Reply::NoInfo(format!("its info isn't as expected: {e}")),
        },
        Err(status) => Reply::NoInfo(format!("HTTP {status}")),
    })
}

/// An HTTP response's body, or its status when that isn't 200.
fn body(response: &[u8]) -> Result<Result<Vec<u8>, u16>, Unreachable> {
    let Some(head_end) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err(Unreachable("the server's answer isn't HTTP".into()));
    };
    let head = String::from_utf8_lossy(&response[..head_end]);
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .filter(|l| l.starts_with("HTTP/"))
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Unreachable("the server's answer isn't HTTP".into()))?;
    if status != 200 {
        return Ok(Err(status));
    }
    let header = |name: &str| {
        head.lines()
            .skip(1)
            .find_map(|l| l.split_once(':').filter(|(n, _)| n.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_owned()))
    };
    let mut body = response[head_end + 4..].to_vec();
    if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        body = unchunk(&body);
    } else if let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.truncate(len);
    }
    Ok(Ok(body))
}

/// A chunked body's data (as much as came).
fn unchunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = body.windows(2).position(|w| w == b"\r\n") {
        let size = String::from_utf8_lossy(&body[..line_end]);
        let Ok(size) = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16) else {
            break;
        };
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(body.len());
        out.extend_from_slice(&body[..take]);
        body = body.get(take + 2..).unwrap_or_default();
    }
    out
}

/// What the overlay tells the player about, from one check to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The server stopped answering (or didn't from the start).
    Unreachable,
    /// News items not there at the last check.
    NewNews(usize),
}

/// Follows the checks, to say what changed.
#[derive(Debug, Default)]
pub struct Watch {
    /// Whether the last check failed.
    failing: bool,
    /// The news items seen so far, once the server has given its news.
    seen: Option<HashSet<NewsItem>>,
}

impl Watch {
    /// The changes since the last check. The first news the server gives is what's there, not new.
    pub fn update(&mut self, result: &Result<Reply, Unreachable>) -> Vec<Change> {
        let mut changes = Vec::new();
        let failing = result.is_err();
        if failing && !self.failing {
            changes.push(Change::Unreachable);
        }
        self.failing = failing;
        if let Ok(Reply::Info(info)) = result {
            let now: HashSet<NewsItem> = info.news.iter().cloned().collect();
            if let Some(seen) = &self.seen {
                let new = now.difference(seen).count();
                if new > 0 {
                    changes.push(Change::NewNews(new));
                }
            }
            self.seen = Some(now);
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn finds_the_port() {
        assert_eq!(address("play.example.net"), ("play.example.net".into(), 80));
        assert_eq!(address(" 10.0.0.5:8080 "), ("10.0.0.5".into(), 8080));
        assert_eq!(address("[::1]:81"), ("::1".into(), 81));
        assert_eq!(address("[::1]"), ("::1".into(), 80));
        assert_eq!(address("::1"), ("::1".into(), 80));
    }

    #[test]
    fn reads_the_info() {
        let body = r#"{"game":"ghost-recon-future-soldier","version":"0.3.0","players_online":4,"news":[{"title":"Welcome","body":"Hi","date":"2026-11-15","shows":null},{"title":"Campaign","body":"Co-op","date":"2026-11-14","shows":"campaign"}],"later":1}"#;
        let Ok(Reply::Info(info)) = reply(&ok(body)) else { panic!() };
        assert_eq!(info.version.as_deref(), Some("0.3.0"));
        assert_eq!(info.players_online, Some(4));
        assert_eq!(info.news.len(), 2);
        assert_eq!(info.news[1].shows.as_deref(), Some("campaign"));
        assert_eq!(info.news[0].date.as_deref(), Some("2026-11-15"));
    }

    #[test]
    fn every_field_but_the_game_is_optional() {
        let Ok(Reply::Info(info)) = reply(&ok(r#"{"game":"ghost-recon-future-soldier"}"#)) else {
            panic!()
        };
        assert_eq!(info.version, None);
        let Ok(Reply::Info(info)) = reply(&ok(r#"{"game":"ghost-recon-future-soldier","news":[{"title":"Only a title"}]}"#)) else {
            panic!()
        };
        assert_eq!(info.news[0].body, "");
    }

    #[test]
    fn knows_another_games_server() {
        assert_eq!(
            reply(&ok(r#"{"name":"5th Echelon Enhanced","version":"0.5.0"}"#)),
            Ok(Reply::OtherGame("5th Echelon Enhanced".into()))
        );
        assert_eq!(reply(&ok("{}")), Ok(Reply::OtherGame("another game's server".into())));
    }

    #[test]
    fn a_server_without_the_page_still_answered() {
        assert_eq!(reply(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"), Ok(Reply::NoInfo("HTTP 404".into())));
        assert!(matches!(reply(&ok("not json")), Ok(Reply::NoInfo(_))));
        assert!(reply(b"SSH-2.0-OpenSSH\r\n").is_err());
        assert!(reply(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn reads_the_leaderboard() {
        let body = r#"{"mode":"siege","modes":["all"],"rows":[{"rank":1,"name":"Kozak","wins":3,"losses":1,"kills":9,"deaths":2,"objectives":1,"seconds":1140}]}"#;
        let board: Leaderboard = serde_json::from_slice(&body_of(&ok(body))).unwrap();
        assert_eq!(board.mode, "siege");
        assert_eq!((board.rows[0].name.as_str(), board.rows[0].wins, board.rows[0].seconds), ("Kozak", 3, 1140));
    }

    fn body_of(response: &[u8]) -> Vec<u8> {
        body(response).unwrap().unwrap()
    }

    #[test]
    fn reads_a_chunked_body() {
        let json = br#"{"game":"ghost-recon-future-soldier","players_online":7}"#;
        let (a, b) = json.split_at(20);
        let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for part in [a, b] {
            response.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
            response.extend_from_slice(part);
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"0\r\n\r\n");
        let Ok(Reply::Info(info)) = reply(&response) else { panic!() };
        assert_eq!(info.players_online, Some(7));
    }

    #[test]
    fn says_when_the_server_goes_and_when_news_comes() {
        let news = |titles: &[&str]| {
            Ok(Reply::Info(ServerInfo {
                news: titles
                    .iter()
                    .map(|t| NewsItem {
                        title: (*t).into(),
                        ..NewsItem::default()
                    })
                    .collect(),
                ..ServerInfo::default()
            }))
        };
        let down = Err(Unreachable("no".into()));
        let mut watch = Watch::default();
        assert_eq!(watch.update(&news(&["a"])), vec![], "the first news is what's there");
        assert_eq!(watch.update(&news(&["a"])), vec![]);
        assert_eq!(watch.update(&news(&["b", "a", "c"])), vec![Change::NewNews(2)]);
        assert_eq!(watch.update(&down), vec![Change::Unreachable]);
        assert_eq!(watch.update(&down), vec![], "once, when it starts failing");
        assert_eq!(watch.update(&news(&["b", "a", "c"])), vec![], "back, with the same news");
        assert_eq!(watch.update(&Ok(Reply::NoInfo("HTTP 404".into()))), vec![], "answering without info isn't failing");

        let mut watch = Watch::default();
        assert_eq!(watch.update(&down), vec![Change::Unreachable], "failing from the start");
    }
}
