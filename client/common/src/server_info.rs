//! What the server says about itself at `GET /api/info`, for the overlay: its version, how many
//! players are online and its news; and its leaderboard (`GET /api/leaderboard?mode=...`). Also
//! the post that sends the logs (`runs`) and the client's router reports (`net`).
//!
//! Over HTTPS (see `http`). Every field is optional; a server without the page (404) is still a
//! server that answered.

use std::collections::HashSet;
use std::time::Duration;

use serde::Deserialize;

pub use crate::http::address;
pub use crate::http::Unreachable;
pub use crate::http::HTTP_PORT;

/// The page.
pub const PATH: &str = "/api/info";
/// The game a Cross-Com server for Future Soldier says it's for. A machine may host another
/// game's server behind the same Caddy, and an address alone reaches that one's page.
pub const GAME: &str = "ghost-recon-future-soldier";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    pub name: Option<String>,
    pub game: Option<String>,
    pub version: Option<String>,
    pub players_online: Option<u32>,
    /// The UDP ports the game signs in on (auth, then secure), which answer the address echo.
    pub udp_ports: Vec<u16>,
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

/// Asks `server` for its info, giving up after `timeout` at each step.
pub fn fetch(server: &str, timeout: Duration) -> Result<Reply, Unreachable> {
    let (status, body) = get(server, PATH, timeout)?;
    Ok(reply(status, &body))
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
    let (status, body) = get(server, &format!("{LEADERBOARD_PATH}?mode={mode}"), timeout)?;
    Ok((status == 200).then(|| serde_json::from_slice(&body).ok()).flatten())
}

/// Asks `server` for `path`: the answer's status and body.
fn get(server: &str, path: &str, timeout: Duration) -> Result<(u16, Vec<u8>), Unreachable> {
    let response = crate::http::request(server, "GET", path, &[("Accept", "application/json".into())], &[], timeout)?;
    Ok((response.status, response.body))
}

/// Posts `body` with `headers` to `path` on `server` (the logs, see `runs`): the answer's status
/// and body. Header values are put on one line.
pub fn post(server: &str, path: &str, headers: &[(&str, String)], body: &[u8], timeout: Duration) -> Result<(u16, Vec<u8>), Unreachable> {
    let response = crate::http::request(server, "POST", path, headers, body, timeout)?;
    Ok((response.status, response.body))
}

/// Reads the info page's answer.
#[must_use]
pub fn reply(status: u16, body: &[u8]) -> Reply {
    if status != 200 {
        return Reply::NoInfo(format!("HTTP {status}"));
    }
    match serde_json::from_slice::<ServerInfo>(body) {
        Ok(info) if info.game.as_deref() != Some(GAME) => Reply::OtherGame(info.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "another game's server".into())),
        Ok(info) => Reply::Info(info),
        Err(e) => Reply::NoInfo(format!("its info isn't as expected: {e}")),
    }
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

    fn ok(body: &str) -> Reply {
        reply(200, body.as_bytes())
    }

    #[test]
    fn reads_the_info() {
        let body = r#"{"game":"ghost-recon-future-soldier","version":"0.3.0","players_online":4,"udp_ports":[21030,21031],"news":[{"title":"Welcome","body":"Hi","date":"2026-11-15","shows":null},{"title":"Campaign","body":"Co-op","date":"2026-11-14","shows":"campaign"}],"later":1}"#;
        let Reply::Info(info) = ok(body) else { panic!() };
        assert_eq!(info.version.as_deref(), Some("0.3.0"));
        assert_eq!(info.players_online, Some(4));
        assert_eq!(info.udp_ports, [21030, 21031]);
        assert_eq!(info.news.len(), 2);
        assert_eq!(info.news[1].shows.as_deref(), Some("campaign"));
        assert_eq!(info.news[0].date.as_deref(), Some("2026-11-15"));
    }

    #[test]
    fn every_field_but_the_game_is_optional() {
        let Reply::Info(info) = ok(r#"{"game":"ghost-recon-future-soldier"}"#) else { panic!() };
        assert_eq!(info.version, None);
        let Reply::Info(info) = ok(r#"{"game":"ghost-recon-future-soldier","news":[{"title":"Only a title"}]}"#) else {
            panic!()
        };
        assert_eq!(info.news[0].body, "");
    }

    #[test]
    fn knows_another_games_server() {
        assert_eq!(ok(r#"{"name":"5th Echelon Enhanced","version":"0.5.0"}"#), Reply::OtherGame("5th Echelon Enhanced".into()));
        assert_eq!(ok("{}"), Reply::OtherGame("another game's server".into()));
    }

    #[test]
    fn a_server_without_the_page_still_answered() {
        assert_eq!(reply(404, b""), Reply::NoInfo("HTTP 404".into()));
        assert!(matches!(ok("not json"), Reply::NoInfo(_)));
    }

    #[test]
    fn reads_the_leaderboard() {
        let body = r#"{"mode":"siege","modes":["all"],"rows":[{"rank":1,"name":"Kozak","wins":3,"losses":1,"kills":9,"deaths":2,"objectives":1,"seconds":1140}]}"#;
        let board: Leaderboard = serde_json::from_str(body).unwrap();
        assert_eq!(board.mode, "siege");
        assert_eq!((board.rows[0].name.as_str(), board.rows[0].wins, board.rows[0].seconds), ("Kozak", 3, 1140));
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
