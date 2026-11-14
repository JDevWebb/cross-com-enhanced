//! The launcher's API, over HTTP next to the online config. Always on.
//!
//! - `GET /api/info`: `{"version": "0.1.0", "players_online": 3, "news": [...]}`, the news as
//!   the game shows it (see `news.rs`), each `{"title", "body", "date": "YYYY-MM-DD", "shows"}`
//!   with `shows` null or `"multiplayer"`, `"campaign"` or `"guerrilla"`.
//! - `POST /api/login` with `{"name": ..., "password": ...}`: `200 {"pid": ...}` when they match
//!   an account, else `401 {"error": ...}`. Failures count against the same limits as the game's
//!   own sign-in (`rate_limit.rs`), so the two together give no more guesses than either:
//!   `429` once over them.

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
use crate::news;
use crate::online::Online;
use crate::rate_limit::FailedSignIns;
use crate::signup::error;
use crate::signup::Answer;

pub const INFO_PATH: &str = "/api/info";
pub const LOGIN_PATH: &str = "/api/login";
/// The biggest request body taken: a name and a password fit many times over.
pub const MAX_BODY: usize = 1024;

pub struct Api {
    accounts: Arc<Accounts>,
    failed: Arc<FailedSignIns>,
    news: PathBuf,
    online: Arc<Online>,
}

#[derive(Serialize)]
struct Info {
    version: &'static str,
    players_online: usize,
    news: Vec<news::Shown>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    name: String,
    password: String,
}

impl Api {
    pub fn new(accounts: Arc<Accounts>, failed: Arc<FailedSignIns>, news: PathBuf, online: Arc<Online>) -> Self {
        Self { accounts, failed, news, online }
    }

    /// The `GET /api/info` answer.
    pub fn info(&self, logger: &Logger) -> String {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let info = Info {
            version: env!("CARGO_PKG_VERSION"),
            players_online: self.online.count(),
            news: news::shown(&news::load(&self.news, logger), now),
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
        if !self.failed.allowed(address, &login.name, Instant::now()) {
            return (error("429 Too Many Requests", "too many failed sign-ins; try again later"), None);
        }
        match self.accounts.verify(&login.name, &login.password) {
            Ok(Some(pid)) => {
                self.failed.succeeded(address, &login.name);
                (("200 OK", json!({ "pid": pid }).to_string()), Some(login.name))
            }
            Ok(None) => {
                self.failed.failed(address, &login.name, Instant::now());
                wrong()
            }
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
        online.signed_in(0x10_0000);
        let logger = Logger::root(slog::Discard, slog::o!());
        let json: serde_json::Value = serde_json::from_str(&api.info(&logger)).unwrap();
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["players_online"], 1);
        let news = json["news"].as_array().unwrap();
        assert_eq!(news.len(), 1, "the welcome, without a news file");
        assert_eq!(news[0]["title"], "Welcome to Cross-Com");
        assert_eq!(news[0]["shows"], serde_json::Value::Null);
        let date = news[0]["date"].as_str().unwrap();
        assert!(date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-', "{date}");
    }

    #[test]
    fn login_with_the_right_password_only() {
        let (api, _) = api();
        let here: IpAddr = "203.0.113.7".parse().unwrap();
        let ((status, json), name) = api.login(here, &login("kozak", "hunter2"));
        assert_eq!(status, "200 OK");
        assert_eq!(json, r#"{"pid":1048576}"#);
        assert_eq!(name.as_deref(), Some("kozak"));
        let ((status, json), name) = api.login(here, &login("Kozak", "wrong"));
        assert_eq!((status, json.as_str()), ("401 Unauthorized", r#"{"error":"wrong name or password"}"#));
        assert_eq!(name, None);
        assert_eq!(api.login(here, &login("Nobody", "hunter2")).0 .0, "401 Unauthorized");
        assert_eq!(api.login(here, &login("not a name", "x")).0 .0, "401 Unauthorized");
        assert_eq!(api.login(here, b"{oops").0 .0, "400 Bad Request");
        assert_eq!(api.login(here, br#"{"name":"Kozak","password":"hunter2","admin":true}"#).0 .0, "400 Bad Request");
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
