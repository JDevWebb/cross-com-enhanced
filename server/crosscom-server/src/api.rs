//! The launcher's API, on the HTTP port next to the online config (behind Caddy, over HTTPS).
//! Always on.
//!
//! - `GET /api/info`: `{"name": "Cross-Com Enhanced", "game": "ghost-recon-future-soldier",
//!   "version": "0.1.0", "players_online": 3, "news": [...], "udp_ports": [21030, 21031]}`, the news as
//!   the game shows it (see `news.rs`), each `{"title", "body", "date": "YYYY-MM-DD", "shows"}`
//!   with `shows` null or `"multiplayer"`, `"campaign"` or `"guerrilla"`.
//! - `POST /api/key-login` with `{"global_id": ..., "time": ..., "host": ..., "signature": ...}`:
//!   the player's identity signing in (see `key_login`): `200 {"pid": ..., "name": ..., "token":
//!   ...}` with a new game token the game signs in with (see `accounts.rs`), `404` when the
//!   identity has no account here, else `401 {"error": ...}`. Failures count against the same limits as the game's
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
pub const KEY_LOGIN_PATH: &str = "/api/key-login";
/// The biggest request body taken: an identity, a time and a signature fit many times over.
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
    /// The server's public address, when it has one: a server hosted at home, set up on its
    /// players' own network, says so, and their clients forward their game's port as for one
    /// on the internet (`here`).
    #[serde(skip_serializing_if = "Option::is_none")]
    public_address: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyLogin {
    global_id: String,
    time: i64,
    host: String,
    signature: String,
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
            public_address: crate::here::public().map(|ip| ip.to_string()),
        };
        serde_json::to_string(&info).unwrap_or_default()
    }

    /// Answers a key sign-in from `address` with `body`, made to the server as `reached` (the
    /// request's Host): the identity's account and a game token for the game, or why not. The
    /// account's name is returned for the log when it signed in.
    ///
    /// The signature must be the identity's over `crosscom_identity::login_message` for this
    /// server's name as the request reached it, at a time within a few minutes of now and later
    /// than the account's last key sign-in: a signature made for another server, or seen once,
    /// signs no one in. Only the identity's holder learns whether it has an account here.
    pub fn key_login(&self, address: IpAddr, reached: Option<&str>, body: &[u8]) -> (Answer, Option<String>) {
        let refused = || (error("401 Unauthorized", "signing in with this identity didn't work"), None);
        let Ok(login) = serde_json::from_slice::<KeyLogin>(body) else {
            return (error("400 Bad Request", "expected {\"global_id\", \"time\", \"host\", \"signature\"}"), None);
        };
        // Not an identity: refused before it's counted, so it can't fill the limits.
        if !crosscom_identity::is_global_id(&login.global_id) {
            return refused();
        }
        let Some(attempt) = self.failed.try_begin(address, &login.global_id, Instant::now()) else {
            return (error("429 Too Many Requests", "too many failed sign-ins; try again later"), None);
        };
        if !crosscom_identity::fresh(login.time, crosscom_identity::now()) {
            return (error("400 Bad Request", "the signature's time is off: check this PC's clock"), None);
        }
        if reached.is_none_or(|host| crosscom_identity::host_key(host) != crosscom_identity::host_key(&login.host)) {
            return refused();
        }
        let message = crosscom_identity::login_message(&login.host, login.time);
        if !crosscom_identity::verify(&login.global_id, &message, &login.signature) {
            return refused();
        }
        let account = match self.accounts.by_identity(&login.global_id) {
            Ok(Some(account)) => account,
            // Not a failed sign-in: a player new here, who signs up next.
            Ok(None) => {
                self.failed.succeeded(attempt);
                return (error("404 Not Found", "this identity has no account here"), None);
            }
            Err(_) => return (error("500 Internal Server Error", "couldn't check the account"), None),
        };
        let (pid, name) = account;
        match self.accounts.use_key_login(pid, login.time) {
            Ok(true) => {}
            Ok(false) => return refused(),
            Err(_) => return (error("500 Internal Server Error", "couldn't check the account"), None),
        }
        self.failed.succeeded(attempt);
        // The game's own sign-in uses this token (see `accounts`).
        match self.accounts.issue_token(pid) {
            Ok(token) => (("200 OK", json!({ "pid": pid, "name": name, "token": token }).to_string()), Some(name)),
            Err(_) => (error("500 Internal Server Error", "couldn't make a game token"), None),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn api() -> (Api, Arc<Online>) {
        let accounts = Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap());
        accounts.add("Kozak", "hunter2").unwrap();
        accounts.add_identity("Ghost", &ghost().global_id()).unwrap();
        let online = Arc::new(Online::default());
        let api = Api::new(accounts, Arc::default(), "/nonexistent/news.toml".into(), Arc::clone(&online));
        (api, online)
    }

    /// The identity whose account "Ghost" is, in [`api`].
    pub(crate) fn ghost() -> crosscom_identity::Identity {
        crosscom_identity::Identity::from_secret(&[7; 32])
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

    /// A key sign-in for `id` at `host`, signed at `time`.
    pub(crate) fn key_login_body(id: &crosscom_identity::Identity, host: &str, time: i64) -> Vec<u8> {
        json!({ "global_id": id.global_id(), "time": time, "host": host, "signature": id.sign_login(host, time) })
            .to_string()
            .into_bytes()
    }

    #[test]
    fn an_identity_signs_in_to_its_account_once_per_signature() {
        let (api, _) = api();
        let here: IpAddr = "203.0.113.7".parse().unwrap();
        let id = crosscom_identity::Identity::generate();
        let now = crosscom_identity::now();
        let host = "play.example.org";
        // No account yet: only the holder learns that.
        assert_eq!(api.key_login(here, Some(host), &key_login_body(&id, host, now)).0 .0, "404 Not Found");
        let pid = api.accounts.add_identity("Pepper", &id.global_id()).unwrap();
        let ((status, json), name) = api.key_login(here, Some(host), &key_login_body(&id, host, now + 1));
        assert_eq!((status, name.as_deref()), ("200 OK", Some("Pepper")));
        let answer: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(answer["pid"], pid);
        assert!(crate::accounts::is_token(answer["token"].as_str().unwrap()));
        // The same signature again (seen on the wire, or replayed by a server it was sent to).
        assert_eq!(api.key_login(here, Some(host), &key_login_body(&id, host, now + 1)).0 .0, "401 Unauthorized");
        // Signed for another server, or sent to this one by another name.
        assert_eq!(api.key_login(here, Some(host), &key_login_body(&id, "evil.example.org", now + 2)).0 .0, "401 Unauthorized");
        assert_eq!(api.key_login(here, Some("other.example.org"), &key_login_body(&id, host, now + 3)).0 .0, "401 Unauthorized");
        assert_eq!(api.key_login(here, None, &key_login_body(&id, host, now + 4)).0 .0, "401 Unauthorized");
        // Port and case don't matter.
        assert_eq!(api.key_login(here, Some("Play.Example.org:443"), &key_login_body(&id, host, now + 5)).0 .0, "200 OK");
        // An old time, someone else's key, nonsense.
        assert_eq!(api.key_login(here, Some(host), &key_login_body(&id, host, now - 3600)).0 .0, "400 Bad Request");
        let other = crosscom_identity::Identity::generate();
        let mut forged: serde_json::Value = serde_json::from_slice(&key_login_body(&other, host, now + 6)).unwrap();
        forged["global_id"] = id.global_id().into();
        assert_eq!(api.key_login(here, Some(host), forged.to_string().as_bytes()).0 .0, "401 Unauthorized");
        assert_eq!(api.key_login(here, Some(host), b"{oops").0 .0, "400 Bad Request");
    }

    #[test]
    fn key_logins_stop_guessing() {
        let (api, _) = api();
        let guesser: IpAddr = "198.51.100.1".parse().unwrap();
        let id = crosscom_identity::Identity::generate();
        api.accounts.add_identity("Pepper", &id.global_id()).unwrap();
        let other = crosscom_identity::Identity::generate();
        let now = crosscom_identity::now();
        let mut tries = 0;
        loop {
            let mut forged: serde_json::Value = serde_json::from_slice(&key_login_body(&other, "h", now + tries)).unwrap();
            forged["global_id"] = id.global_id().into();
            if api.key_login(guesser, Some("h"), forged.to_string().as_bytes()).0 .0 != "401 Unauthorized" {
                break;
            }
            tries += 1;
            assert!(tries < 100, "never stopped");
        }
        assert_eq!(tries, 10);
        assert_eq!(
            api.key_login(guesser, Some("h"), &key_login_body(&id, "h", now + 50)).0 .0,
            "429 Too Many Requests",
            "even the right one, for now"
        );
    }
}
