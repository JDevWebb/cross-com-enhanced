//! Account sign-up over HTTP, for the launcher: `POST /accounts` with
//! `{"name": ..., "password": ..., "code": ...}`.
//!
//! Off unless the server's settings turn it on (`signup = true`). A server can ask for a sign-up
//! code (`signup_code`) that it gives to its players, so strangers can't make accounts. Each
//! address may try a few times a day, and the server takes only so many attempts an hour in
//! all, so names can't be listed by trying them and the database can't be filled.
//!
//! Answers are JSON: `201 {"pid": ...}`, or an error status with `{"error": "..."}`.

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
/// The biggest request body taken: a name, a password and a code fit many times over.
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
    password: String,
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

    /// Answers a sign-up from `address` with `body`. The name is returned for the log when an
    /// account was made.
    pub fn handle(&self, address: IpAddr, body: &[u8]) -> (Answer, Option<String>) {
        let limits = || self.limits.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let too_many = || (error("429 Too Many Requests", "too many sign-ups; try again later"), None);
        if !limits().allow_address(address, Instant::now()) {
            return too_many();
        }
        let Ok(request) = serde_json::from_slice::<Request>(body) else {
            return (
                error("400 Bad Request", "expected {\"name\", \"password\"} (and \"code\" if the server asks for one)"),
                None,
            );
        };
        if let Some(code) = &self.code {
            if !request.code.as_deref().is_some_and(|c| same(c, code)) {
                return (error("403 Forbidden", "this server needs its sign-up code"), None);
            }
        }
        if !limits().allow_all(Instant::now()) {
            return too_many();
        }
        match self.accounts.add(&request.name, &request.password) {
            Ok(pid) => (("201 Created", json!({ "pid": pid }).to_string()), Some(request.name)),
            Err(accounts::Error::NameTaken) => (error("409 Conflict", "that name is taken"), None),
            Err(e @ (accounts::Error::InvalidName | accounts::Error::InvalidPassword)) => (error("400 Bad Request", &e.to_string()), None),
            Err(_) => (error("500 Internal Server Error", "couldn't make the account"), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(name: &str, password: &str, code: Option<&str>) -> Vec<u8> {
        json!({ "name": name, "password": password, "code": code }).to_string().into_bytes()
    }

    #[test]
    fn makes_accounts_that_can_sign_in() {
        let accounts = Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap());
        let signup = SignUp::new(Arc::clone(&accounts), None);
        let here: IpAddr = "203.0.113.7".parse().unwrap();
        let ((status, json), name) = signup.handle(here, &body("Kozak", "hunter2", None));
        assert_eq!(status, "201 Created", "{json}");
        assert_eq!(name.as_deref(), Some("Kozak"));
        assert!(accounts.verify("Kozak", "hunter2").unwrap().is_some());
        assert_eq!(signup.handle(here, &body("kozak", "x", None)).0 .0, "409 Conflict");
        assert_eq!(signup.handle(here, &body("no spaces", "x", None)).0 .0, "400 Bad Request");
        assert_eq!(signup.handle(here, b"{not json").0 .0, "400 Bad Request");
        assert_eq!(signup.handle(here, br#"{"name":"a","password":"b","admin":true}"#).0 .0, "400 Bad Request");
    }

    #[test]
    fn asks_for_the_code_when_the_server_has_one() {
        let accounts = Arc::new(Accounts::open(std::path::Path::new(":memory:")).unwrap());
        let signup = SignUp::new(accounts, Some("ghosts".into()));
        let here: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(signup.handle(here, &body("Kozak", "pw", None)).0 .0, "403 Forbidden");
        assert_eq!(signup.handle(here, &body("Kozak", "pw", Some("ghost"))).0 .0, "403 Forbidden");
        assert_eq!(signup.handle(here, &body("Kozak", "pw", Some("ghosts"))).0 .0, "201 Created");
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
            assert_eq!(signup.handle(stranger, &body("x", "pw", Some("guess"))).0 .0, "403 Forbidden");
        }
        let player: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(signup.handle(player, &body("Kozak", "pw", Some("ghosts"))).0 .0, "201 Created");
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
