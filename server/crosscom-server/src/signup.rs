//! Account sign-up over HTTP, for the launcher: `POST /accounts` with `{"name": ..., "global_id":
//! ..., "time": ..., "host": ..., "signature": ..., "code": ...}`. There is no password: the
//! account belongs to the player's identity (see `accounts`), which signs its name for this
//! server (`crosscom_identity::signup_message`), and signs in with it afterwards.
//!
//! Off unless the server's settings turn it on (`signup = true`). A server can ask for a sign-up
//! code (`signup_code`) that it gives to its players, so strangers can't make accounts. Each
//! address may try a few times a day, and the server takes only so many attempts an hour in
//! all, so names can't be listed by trying them and the database can't be filled.
//!
//! Answers are JSON: `201 {"pid": ..., "name": ..., "token": ...}` (the token the game signs in
//! with), or an error status with `{"error": "..."}`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use crate::accounts;
use crate::accounts::Accounts;

pub const PATH: &str = "/accounts";
/// The biggest request body taken: a name, an identity, a signature and a code fit many times over.
pub const MAX_BODY: usize = 1024;
/// Attempts per address in a day, and by everyone in an hour.
const PER_ADDRESS: u32 = 10;
const ADDRESS_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const IN_ALL: u32 = 200;
const ALL_WINDOW: Duration = Duration::from_secs(60 * 60);
/// Addresses remembered at once; when full, the count with the fewest attempts goes.
const MAX_ADDRESSES: usize = 50_000;
/// The shortest sign-up code a server may set: long enough that guessing it, a few tries a day
/// from each address, is hopeless.
pub const MIN_CODE_LEN: usize = 12;

/// The key an address is counted under: an IPv4 address as itself (also when seen as
/// `::ffff:a.b.c.d`), an IPv6 address by its /48, which one site usually has all of.
fn address_key(address: IpAddr) -> IpAddr {
    match address.to_canonical() {
        address @ IpAddr::V4(_) => address,
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[3..].fill(0);
            IpAddr::from(segments)
        }
    }
}

pub struct SignUp {
    accounts: Arc<Accounts>,
    code: Option<String>,
    limits: Mutex<Limits>,
}

#[derive(Default)]
struct Limits {
    by_address: HashMap<IpAddr, (Instant, u32)>,
    all: Option<(Instant, u32)>,
}

impl Limits {
    /// Counts an attempt from `address`; false when the address is over its limit.
    fn allow_address(&mut self, address: IpAddr, now: Instant) -> bool {
        let key = address_key(address);
        if self.by_address.len() >= MAX_ADDRESSES && !self.by_address.contains_key(&key) {
            self.by_address.retain(|_, (since, _)| now.duration_since(*since) < ADDRESS_WINDOW);
            if self.by_address.len() >= MAX_ADDRESSES {
                // Full of live counts: the one with the fewest attempts goes (the oldest of those),
                // rather than every new address waiting. An address at its limit only goes once
                // every other one is too, so filling the table doesn't give it fresh tries.
                if let Some(least) = self.by_address.iter().min_by_key(|(_, &(since, n))| (n, since)).map(|(k, _)| *k) {
                    self.by_address.remove(&least);
                }
            }
        }
        let mine = match self.by_address.get(&key) {
            Some(&(since, n)) if now.duration_since(since) < ADDRESS_WINDOW => (since, n),
            _ => (now, 0),
        };
        if mine.1 >= PER_ADDRESS {
            return false;
        }
        self.by_address.insert(key, (mine.0, mine.1 + 1));
        true
    }

    /// Counts an attempt against the server-wide limit; false when it's reached. Only attempts
    /// with the right code (or on a server without one) count, so strangers guessing the code
    /// can't use up the players' sign-ups.
    fn allow_all(&mut self, now: Instant) -> bool {
        let all = match self.all {
            Some((since, n)) if now.duration_since(since) < ALL_WINDOW => (since, n),
            _ => (now, 0),
        };
        if all.1 >= IN_ALL {
            return false;
        }
        self.all = Some((all.0, all.1 + 1));
        true
    }

    #[cfg(test)]
    fn allow(&mut self, address: IpAddr, now: Instant) -> bool {
        self.allow_address(address, now) && self.allow_all(now)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    name: String,
    global_id: String,
    time: i64,
    host: String,
    signature: String,
    #[serde(default)]
    code: Option<String>,
}

/// An HTTP status and a JSON body.
pub type Answer = (&'static str, String);

pub(crate) fn error(status: &'static str, message: &str) -> Answer {
    (status, json!({ "error": message }).to_string())
}

/// Compares in time that doesn't depend on where the two differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl SignUp {
    pub fn new(accounts: Arc<Accounts>, code: Option<String>) -> Self {
        Self {
            accounts,
            code: code.filter(|c| !c.is_empty()),
            limits: Mutex::default(),
        }
    }

    /// Answers a sign-up from `address` with `body`, made to the server as `reached` (the
    /// request's Host). The name is returned for the log when an account was made.
    pub fn handle(&self, address: IpAddr, reached: Option<&str>, body: &[u8]) -> (Answer, Option<String>) {
        let limits = || self.limits.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let too_many = || (error("429 Too Many Requests", "too many sign-ups; try again later"), None);
        if !limits().allow_address(address, Instant::now()) {
            return too_many();
        }
        let Ok(request) = serde_json::from_slice::<Request>(body) else {
            return (
                error(
                    "400 Bad Request",
                    "expected {\"name\", \"global_id\", \"time\", \"host\", \"signature\"} (and \"code\" if the server asks for one)",
                ),
                None,
            );
        };
        if let Some(code) = &self.code {
            if !request.code.as_deref().is_some_and(|c| same(c, code)) {
                return (error("403 Forbidden", "this server needs its sign-up code"), None);
            }
        }
        // The identity's own word for it, for this server, now.
        if !crosscom_identity::fresh(request.time, crosscom_identity::now()) {
            return (error("400 Bad Request", "the signature's time is off: check this PC's clock"), None);
        }
        let signed = crosscom_identity::is_global_id(&request.global_id)
            && reached.is_some_and(|host| crosscom_identity::host_key(host) == crosscom_identity::host_key(&request.host))
            && crosscom_identity::verify(
                &request.global_id,
                &crosscom_identity::signup_message(&request.host, &request.name, request.time),
                &request.signature,
            );
        if !signed {
            return (error("401 Unauthorized", "the identity's signature doesn't match"), None);
        }
        if !limits().allow_all(Instant::now()) {
            return too_many();
        }
        let pid = match self.accounts.add_identity(&request.name, &request.global_id) {
            Ok(pid) => pid,
            Err(accounts::Error::NameTaken) => return (error("409 Conflict", "that name is taken"), None),
            // The identity's holder learns its account's name: it's theirs.
            Err(e @ accounts::Error::IdentityTaken(_)) => return (error("409 Conflict", &e.to_string()), None),
            Err(e @ accounts::Error::InvalidName) => return (error("400 Bad Request", &e.to_string()), None),
            Err(_) => return (error("500 Internal Server Error", "couldn't make the account"), None),
        };
        // The game's own sign-in uses this token (see `accounts`).
        match self.accounts.issue_token(pid) {
            Ok(token) => (("201 Created", json!({ "pid": pid, "name": request.name, "token": token }).to_string()), Some(request.name)),
            Err(_) => (error("500 Internal Server Error", "couldn't make a game token"), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "play.example.org";

    /// A sign-up for `name` by a new identity.
    fn body(name: &str, code: Option<&str>) -> Vec<u8> {
        body_by(&crosscom_identity::Identity::generate(), name, code)
    }

    fn body_by(id: &crosscom_identity::Identity, name: &str, code: Option<&str>) -> Vec<u8> {
        let time = crosscom_identity::now();
        json!({ "name": name, "global_id": id.global_id(), "time": time, "host": HOST, "signature": id.sign_signup(HOST, name, time), "code": code })
            .to_string()
            .into_bytes()
    }

    #[test]
    fn makes_accounts_that_can_sign_in() {
        let accounts = Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap());
        let signup = SignUp::new(Arc::clone(&accounts), None);
        let here: IpAddr = "203.0.113.7".parse().unwrap();
        let id = crosscom_identity::Identity::generate();
        let ((status, json), name) = signup.handle(here, Some(HOST), &body_by(&id, "Kozak", None));
        assert_eq!(status, "201 Created", "{json}");
        assert_eq!(name.as_deref(), Some("Kozak"));
        let answer: serde_json::Value = serde_json::from_str(&json).unwrap();
        let token = answer["token"].as_str().unwrap();
        assert_eq!(accounts.verify("Kozak", token).unwrap(), accounts.pid("Kozak").unwrap(), "the game signs in with the token");
        assert_eq!(accounts.by_identity(&id.global_id()).unwrap().map(|(_, n)| n).as_deref(), Some("Kozak"));
        assert_eq!(signup.handle(here, Some(HOST), &body("kozak", None)).0 .0, "409 Conflict", "the name is taken");
        let again = signup.handle(here, Some(HOST), &body_by(&id, "Pepper", None)).0;
        assert_eq!(again.0, "409 Conflict", "one account per identity");
        assert!(again.1.contains("Kozak"), "{}", again.1);
        assert_eq!(signup.handle(here, Some(HOST), &body("no spaces", None)).0 .0, "400 Bad Request", "not a name");
        assert_eq!(
            signup.handle(here, Some("other.example.org"), &body("Pepper", None)).0 .0,
            "401 Unauthorized",
            "signed for another server"
        );
        assert_eq!(signup.handle(here, Some(HOST), b"{not json").0 .0, "400 Bad Request");
        let mut extra: serde_json::Value = serde_json::from_slice(&body("Pepper", None)).unwrap();
        extra["admin"] = true.into();
        assert_eq!(signup.handle(here, Some(HOST), extra.to_string().as_bytes()).0 .0, "400 Bad Request");
        // A signature for one name doesn't make another.
        let mut renamed: serde_json::Value = serde_json::from_slice(&body("Pepper", None)).unwrap();
        renamed["name"] = "Mallory".into();
        assert_eq!(signup.handle(here, Some(HOST), renamed.to_string().as_bytes()).0 .0, "401 Unauthorized");
    }

    #[test]
    fn asks_for_the_code_when_the_server_has_one() {
        let accounts = Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap());
        let signup = SignUp::new(accounts, Some("ghosts".into()));
        let here: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(signup.handle(here, Some(HOST), &body("Kozak", None)).0 .0, "403 Forbidden");
        assert_eq!(signup.handle(here, Some(HOST), &body("Kozak", Some("ghost"))).0 .0, "403 Forbidden");
        assert_eq!(signup.handle(here, Some(HOST), &body("Kozak", Some("ghosts"))).0 .0, "201 Created");
        assert!(SignUp::new(Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap()), Some(String::new()))
            .code
            .is_none());
    }

    #[test]
    fn limits_attempts_per_address_and_in_all() {
        let mut limits = Limits::default();
        let now = Instant::now();
        let a: IpAddr = "203.0.113.7".parse().unwrap();
        for _ in 0..PER_ADDRESS {
            assert!(limits.allow(a, now));
        }
        assert!(!limits.allow(a, now));
        assert!(!limits.allow("::ffff:203.0.113.7".parse().unwrap(), now), "the same address over IPv6");
        assert!(limits.allow(a, now + ADDRESS_WINDOW), "a day later");
        let mut limits = Limits::default();
        for i in 0..IN_ALL {
            assert!(limits.allow(IpAddr::from([10, 0, (i / 256) as u8, (i % 256) as u8]), now));
        }
        assert!(!limits.allow("198.51.100.1".parse().unwrap(), now));
        assert!(limits.allow("198.51.100.1".parse().unwrap(), now + ALL_WINDOW));
    }

    #[test]
    fn wrong_codes_dont_use_up_the_servers_sign_ups() {
        let accounts = Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap());
        let signup = SignUp::new(accounts, Some("ghosts".into()));
        for i in 0..IN_ALL + 50 {
            let stranger = IpAddr::from([10, 1, (i / 256) as u8, (i % 256) as u8]);
            assert_eq!(signup.handle(stranger, Some(HOST), &body("x", Some("guess"))).0 .0, "403 Forbidden");
        }
        let player: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(signup.handle(player, Some(HOST), &body("Kozak", Some("ghosts"))).0 .0, "201 Created");
    }

    #[test]
    fn a_full_table_makes_room_and_ipv6_counts_by_site() {
        let mut limits = Limits::default();
        let now = Instant::now();
        for i in 0..MAX_ADDRESSES as u32 {
            assert!(limits.allow_address(IpAddr::from(i.to_be_bytes()), now + Duration::from_millis(u64::from(i))));
        }
        assert!(
            limits.allow_address("203.0.113.7".parse().unwrap(), now + Duration::from_secs(60)),
            "a new address still gets in"
        );
        assert_eq!(limits.by_address.len(), MAX_ADDRESSES);

        // An address at its limit isn't pushed out by others that tried once.
        let mut limits = Limits::default();
        let guesser: IpAddr = "198.51.100.1".parse().unwrap();
        while limits.allow_address(guesser, now) {}
        for i in 0..MAX_ADDRESSES as u32 + 10 {
            limits.allow_address(IpAddr::from((i | 0x0a00_0000).to_be_bytes()), now);
        }
        assert!(!limits.allow_address(guesser, now), "still over its limit");
        assert_eq!(address_key("2001:db8:1:2::5".parse().unwrap()), address_key("2001:db8:1:ffff::9".parse().unwrap()));
    }
}
