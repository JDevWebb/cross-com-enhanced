//! The launcher's API, on the HTTP port next to the online config (behind Caddy, over HTTPS).
//! Always on.
//!
//! - `GET /api/info`: `{"name": "Cross-Com Enhanced", "game": "ghost-recon-future-soldier",
//!   "version": "0.1.0", "players_online": 3, "news": [...], "udp_ports": [21030, 21031]}`, the news as
//!   the game shows it (see `news.rs`), each `{"title", "body", "date": "YYYY-MM-DD", "shows"}`
//!   with `shows` null or `"multiplayer"`, `"campaign"` or `"guerrilla"`.
//! - `POST /api/login` with `{"name": ..., "password": ...}`: `200 {"pid": ..., "token": ...}` when
//!   they match an account, with a new game token the game signs in with in place of the password
//!   (see `accounts.rs`); else `401 {"error": ...}`. Only the password is taken here, not a token. Failures count against the same limits as the game's
//!   own sign-in (`rate_limit.rs`), so the two together give no more guesses than either:
//!   `429` once over them.
//! - `GET /api/leaderboard?mode=...`: every player ranked (see `leaderboard.rs`).
//! - `POST /api/forward` and `POST /api/reach`: clients' reports of their routers, and their
//!   connection tests (see `reach.rs`).

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use slog::Logger;

use crate::accounts;
use crate::accounts::Accounts;
use crate::leaderboard;
use crate::news;
use crate::online::Online;
use crate::rate_limit::FailedSignIns;
use crate::signup::error;
use crate::signup::Answer;

pub const INFO_PATH: &str = "/api/info";
/// The game this server is for, as `/api/info` says: the launcher and the overlay check it, as
/// a machine may host another game's server too (behind one Caddy), and an address alone could
/// reach that one's API.
pub const GAME: &str = "ghost-recon-future-soldier";
pub const NAME: &str = "Cross-Com Enhanced";
pub const LOGIN_PATH: &str = "/api/login";
/// The biggest request body taken: a name and a password fit many times over.
pub const MAX_BODY: usize = 1024;

pub struct Api {
    accounts: Arc<Accounts>,
    failed: Arc<FailedSignIns>,
    news: PathBuf,
    online: Arc<Online>,
    stats: Option<Arc<crate::stats::Store>>,
    leaderboards: leaderboard::Cache,
    udp_ports: Vec<u16>,
}

#[derive(Serialize)]
struct Info {
    name: &'static str,
    game: &'static str,
    version: &'static str,
    players_online: usize,
    news: Vec<news::Shown>,
    udp_ports: Vec<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    name: String,
    password: String,
}

impl Api {
    pub fn new(accounts: Arc<Accounts>, failed: Arc<FailedSignIns>, news: PathBuf, online: Arc<Online>) -> Self {
        Self {
            accounts,
            failed,
            news,
            online,
            stats: None,
            leaderboards: leaderboard::Cache::default(),
            udp_ports: Vec::new(),
        }
    }

    /// The UDP ports the game signs in on, auth then secure: both answer the address echo,
    /// which clients' connection tests ask (see `reach`).
    #[must_use]
    pub fn with_udp_ports(mut self, ports: Vec<u16>) -> Self {
        self.udp_ports = ports;
        self
    }

    /// The statistics the leaderboard is made from.
    #[must_use]
    pub fn with_stats(mut self, stats: Arc<crate::stats::Store>) -> Self {
        self.stats = Some(stats);
        self
    }

    /// The `GET /api/leaderboard` answer for `query` (`mode=...`).
    pub fn leaderboard(&self, query: &str) -> Answer {
        let Some(mode) = leaderboard::mode_of(query) else {
            let modes: Vec<_> = leaderboard::MODES.iter().map(|(name, _)| *name).collect();
            return error("400 Bad Request", &format!("mode is one of {}", modes.join(", ")));
        };
        let Some(stats) = &self.stats else {
            return ("200 OK", leaderboard::answer(mode, Vec::new()));
        };
        let made = self.leaderboards.get(mode, || {
            let stored = stats.leaderboard_numbers().ok()?;
            let rows = leaderboard::table(&stored, leaderboard::MODES[mode].1, |pid| self.accounts.name(pid).ok().flatten());
            Some(leaderboard::answer(mode, rows))
        });
        match made {
            Some(json) => ("200 OK", json),
            None => error("500 Internal Server Error", "couldn't read the statistics"),
        }
    }

    /// The `GET /api/info` answer.
    pub fn info(&self, logger: &Logger) -> String {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let info = Info {
            name: NAME,
            game: GAME,
            version: env!("CROSSCOM_RELEASE"),
            players_online: self.online.count(),
            news: news::shown(&news::load(&self.news, logger), now),
            udp_ports: self.udp_ports.clone(),
        };
        serde_json::to_string(&info).unwrap_or_default()
    }

    /// Answers a sign-in from `address` with `body`. The account's name is returned for the log
    /// when it matched.
    pub fn login(&self, address: IpAddr, body: &[u8]) -> (Answer, Option<String>) {
        let wrong = || (error("401 Unauthorized", "wrong name or password"), None);
        let Ok(login) = serde_json::from_slice::<Login>(body) else {
            return (error("400 Bad Request", "expected {\"name\", \"password\"}"), None);
        };
        // A name no account can have is refused before it's counted, so it can't fill the limits.
        if !accounts::valid_name(&login.name) {
            return wrong();
        }
        // Counted as failed from here unless it matches (see `rate_limit`), so requests at once
        // can't all try before any has failed.
        let Some(attempt) = self.failed.try_begin(address, &login.name, Instant::now()) else {
            return (error("429 Too Many Requests", "too many failed sign-ins; try again later"), None);
        };
        // The password only: a game token can't be turned into more of them.
        match self.accounts.verify_password(&login.name, &login.password) {
            Ok(Some(pid)) => {
                self.failed.succeeded(attempt);
                // The game's own sign-in uses this token, not the password (see `accounts`).
                match self.accounts.issue_token(pid) {
                    Ok(token) => (("200 OK", json!({ "pid": pid, "token": token }).to_string()), Some(login.name)),
                    Err(_) => (error("500 Internal Server Error", "couldn't make a game token"), None),
                }
            }
            Ok(None) => wrong(),
            Err(_) => (error("500 Internal Server Error", "couldn't check the account"), None),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn api() -> (Api, Arc<Online>) {
        let accounts = Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap());
        accounts.add("Kozak", "hunter2").unwrap();
        let online = Arc::new(Online::default());
        let api = Api::new(accounts, Arc::default(), "/nonexistent/news.toml".into(), Arc::clone(&online));
        (api, online)
    }

    fn login(name: &str, password: &str) -> Vec<u8> {
        json!({ "name": name, "password": password }).to_string().into_bytes()
    }

    #[test]
    fn info_gives_the_version_players_and_news() {
        let (api, online) = api();
        online.signed_in(0x10_0000, "192.0.2.1".parse().unwrap());
        let logger = Logger::root(slog::Discard, slog::o!());
        let json: serde_json::Value = serde_json::from_str(&api.info(&logger)).unwrap();
        assert_eq!(json["version"], env!("CROSSCOM_RELEASE"));
        assert_eq!(json["game"], "ghost-recon-future-soldier");
        assert_eq!(json["name"], "Cross-Com Enhanced");
        assert_eq!(json["players_online"], 1);
        let news = json["news"].as_array().unwrap();
        assert_eq!(news.len(), 1, "the welcome, without a news file");
        assert_eq!(news[0]["title"], "Welcome to Cross-Com");
        assert_eq!(news[0]["shows"], serde_json::Value::Null);
        let date = news[0]["date"].as_str().unwrap();
        assert!(date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-', "{date}");
    }

    #[test]
    fn leaderboard_answers_a_mode_or_says_which() {
        let (api, _) = api();
        let (status, json) = api.leaderboard("mode=conflict");
        assert_eq!(status, "200 OK");
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["mode"], "conflict");
        assert_eq!(json["rows"].as_array().unwrap().len(), 0);
        assert_eq!(api.leaderboard("").0, "200 OK");
        assert_eq!(api.leaderboard("mode=tdm").0, "400 Bad Request");
    }

    #[test]
    fn login_with_the_right_password_only() {
        let (api, _) = api();
        let here: IpAddr = "203.0.113.7".parse().unwrap();
        let ((status, json), name) = api.login(here, &login("kozak", "hunter2"));
        assert_eq!(status, "200 OK");
        let answer: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(answer["pid"], 1_048_576);
        let token = answer["token"].as_str().unwrap();
        assert!(accounts::is_token(token), "{token}");
        assert_eq!(name.as_deref(), Some("kozak"));
        let ((status, _), _) = api.login(here, &login("kozak", token));
        assert_eq!(status, "401 Unauthorized", "a token can't sign the launcher in");
        let ((status, json), name) = api.login(here, &login("Kozak", "wrong"));
        assert_eq!((status, json.as_str()), ("401 Unauthorized", r#"{"error":"wrong name or password"}"#));
        assert_eq!(name, None);
        assert_eq!(api.login(here, &login("Nobody", "hunter2")).0 .0, "401 Unauthorized");
        assert_eq!(api.login(here, &login("not a name", "x")).0 .0, "401 Unauthorized");
        assert_eq!(api.login(here, b"{oops").0 .0, "400 Bad Request");
        assert_eq!(api.login(here, br#"{"name":"Kozak","password":"hunter2","admin":true}"#).0 .0, "400 Bad Request");
    }

    #[test]
    fn logins_at_once_cant_pass_the_limit_together() {
        let (api, _) = api();
        let api = Arc::new(api);
        let barrier = Arc::new(std::sync::Barrier::new(30));
        let threads: Vec<_> = (0..30)
            .map(|i| {
                let (api, barrier) = (Arc::clone(&api), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    api.login("198.51.100.1".parse().unwrap(), &login("Kozak", &format!("guess{i}"))).0 .0
                })
            })
            .collect();
        let statuses: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(statuses.iter().filter(|s| **s == "401 Unauthorized").count(), 10, "{statuses:?}");
        assert_eq!(statuses.iter().filter(|s| **s == "429 Too Many Requests").count(), 20);
    }

    #[test]
    fn login_stops_guessing() {
        let (api, _) = api();
        let guesser: IpAddr = "198.51.100.1".parse().unwrap();
        let mut tries = 0;
        while api.login(guesser, &login("Kozak", &format!("guess{tries}"))).0 .0 == "401 Unauthorized" {
            tries += 1;
            assert!(tries < 100, "never stopped");
        }
        assert_eq!(tries, 10);
        assert_eq!(api.login(guesser, &login("Kozak", "hunter2")).0 .0, "429 Too Many Requests", "even the right one, for now");
        assert_eq!(api.login(guesser, &login("Pepper", "x")).0 .0, "429 Too Many Requests", "any name from that address");
    }
}
