//! Players' logs: `POST /api/logs`, each run's client log, sent by the launcher after a game it
//! started ends, or by the client DLL for a game started without it. They help the host see what
//! went wrong in a player's game. See docs/hosting.md, "Players' logs".
//!
//! The body is the log (`text/plain`, the client has already taken the Windows user name and the
//! password out), at most 4 MiB. Headers say what's known about it, all optional:
//! `X-Crosscom-Account`, `-Client` (version), `-Launcher` (version), `-Exe` (`DX11` or `DX9`),
//! `-Os`, `-Run` (the run's file name) and `-Exit` (the game's exit code).
//!
//! A log is only taken from an address the account it names signed in to the game from in the
//! last 24 hours (`Online::played_from`): the game's own requests go over plain HTTP and can't
//! carry a password safely, so where the player plays from stands in for one. Then at most 20 an
//! hour from an address, and 4 at once.
//!
//! - `201 {"id": "20261115-123456-ab12cd"}`: kept, as that report;
//! - `403`: logs turned off (`accept_logs = false`), or not signed in from this address lately;
//! - `411` without a Content-Length, `413` too big, `429` too many, `503` too many at once.
//!
//! Each is kept as `<logs_dir>/<YYYY-MM-DD>/<id>-<account>.log` with a `.json` beside it (the
//! headers, the client's address, when it came). Days older than `logs_keep_days` and, past
//! `logs_max_mb` in all, the oldest reports are deleted as new ones come. Logs are never served
//! back over HTTP: `crosscom-server logs list` and `logs show ID` read them.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde::Serialize;

use crate::accounts::Accounts;
use crate::online::Online;
use crate::rate_limit::address_key;
use crate::signup::error;
use crate::signup::Answer;

pub const PATH: &str = "/api/logs";
/// The biggest log taken (the client sends at most 2 MiB).
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// How long one upload may take in all.
pub const DEADLINE: Duration = Duration::from_secs(60);
/// Uploads an address may make in [`WINDOW`].
const MAX_PER_ADDRESS: u32 = 20;
const WINDOW: Duration = Duration::from_secs(60 * 60);
/// Uploads read at once; each is held in memory until it's written.
const MAX_AT_ONCE: usize = 4;
/// Addresses (and runs, for repeats) remembered.
const MAX_ENTRIES: usize = 10_000;
/// What one account may send in [`ACCOUNT_WINDOW`], so one can't push everyone else's logs out
/// of the space kept (`logs_max_mb`).
const MAX_PER_ACCOUNT: u32 = 48;
const MAX_BYTES_PER_ACCOUNT: u64 = 64 * 1024 * 1024;
const ACCOUNT_WINDOW: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
/// The longest header value kept.
const MAX_VALUE: usize = 128;
/// The longest account name in a file name.
const MAX_NAME: usize = 32;

/// The settings (`accept_logs`, `logs_dir`, `logs_max_mb`, `logs_keep_days`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub accept: bool,
    pub dir: PathBuf,
    pub max_bytes: u64,
    pub keep_days: u32,
}

/// What's kept beside each log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub id: String,
    /// When it came (Unix time) and as UTC.
    pub received: u64,
    pub received_utc: String,
    /// The client's address (behind a trusted proxy, the one it gives).
    pub address: String,
    pub account: String,
    pub client: Option<String>,
    pub launcher: Option<String>,
    pub exe: Option<String>,
    pub os: Option<String>,
    pub run: Option<String>,
    pub exit: Option<i64>,
    pub bytes: u64,
}

pub struct Logs {
    settings: Settings,
    accounts: Arc<Accounts>,
    online: Arc<Online>,
    /// Uploads per address (as `address_key` counts it): when its window began, and how many.
    uploads: Mutex<HashMap<IpAddr, (Instant, u32)>>,
    /// Per account: since when, how many logs and how many bytes.
    by_account: Mutex<HashMap<u32, (Instant, u32, u64)>>,
    /// Runs already taken (account, run name) and their reports: the same run sent again (by the
    /// launcher and the DLL at once) gets the same report.
    taken: Mutex<HashMap<(String, String), String>>,
    at_once: Arc<AtomicUsize>,
    /// Writing and pruning, one at a time.
    store: Mutex<()>,
}

/// An upload let through [`Logs::check`]; counts against [`MAX_AT_ONCE`] until dropped.
pub struct Slot {
    at_once: Arc<AtomicUsize>,
    /// The body's length.
    pub len: usize,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.at_once.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The value of header `name` in `head` (the request line and headers), if given.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .skip(1)
        .find_map(|l| l.split_once(':').filter(|(n, _)| n.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
}

/// A header value as kept: no control characters, at most [`MAX_VALUE`] characters.
fn text(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).take(MAX_VALUE).collect::<String>().trim().to_owned()
}

/// Part of a file name: only `A-Z a-z 0-9 . _ -`, not starting with a dot, at most `max`
/// characters; `unknown` when nothing's left.
pub fn file_part(value: &str, max: usize) -> String {
    let part: String = value.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).take(max).collect();
    let part = part.trim_start_matches('.');
    if part.is_empty() {
        "unknown".into()
    } else {
        part.to_owned()
    }
}

/// `(year, month, day, hour, minute, second)` of a Unix time, UTC.
fn utc(secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let (y, m, d) = crate::news::civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0));
    let rest = secs % 86_400;
    (y, m, d, rest / 3600, rest / 60 % 60, rest % 60)
}

/// A report ID: when it came (UTC) and six random hex digits, `20261115-123456-ab12cd`.
fn new_id(now: u64) -> String {
    let (y, mo, d, h, mi, s) = utc(now);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{:06x}", rand::random::<u32>() & 0xff_ffff)
}

/// Whether `id` is shaped as [`new_id`] makes them (and so safe in a path).
fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 22
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 15 => *c == b'-',
            16.. => c.is_ascii_hexdigit() && !c.is_ascii_uppercase(),
            _ => c.is_ascii_digit(),
        })
}

/// The day folder (`YYYY-MM-DD`) of a report ID.
fn day_of(id: &str) -> String {
    format!("{}-{}-{}", &id[..4], &id[4..6], &id[6..8])
}

fn day(secs: u64) -> String {
    let (y, m, d, ..) = utc(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

fn is_day(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Logs {
    pub fn new(settings: Settings, accounts: Arc<Accounts>, online: Arc<Online>) -> Self {
        Self {
            settings,
            accounts,
            online,
            uploads: Mutex::default(),
            by_account: Mutex::default(),
            taken: Mutex::default(),
            at_once: Arc::default(),
            store: Mutex::default(),
        }
    }

    /// Whether an upload with these headers, from `client`, is taken, before its body is read:
    /// a [`Slot`] for it, or the answer refusing it.
    pub fn check(&self, client: IpAddr, head: &str) -> Result<Slot, Answer> {
        self.check_at(client, head, Instant::now())
    }

    fn check_at(&self, client: IpAddr, head: &str, now: Instant) -> Result<Slot, Answer> {
        if !self.settings.accept {
            return Err(error("403 Forbidden", "this server doesn't take logs"));
        }
        let Some(len) = header(head, "content-length").and_then(|v| v.parse::<usize>().ok()) else {
            return Err(error("411 Length Required", "say the log's length (Content-Length)"));
        };
        if len > MAX_BODY {
            return Err(error("413 Content Too Large", "the log is too big: 4 MiB at most"));
        }
        // Who it is first: only addresses a player signed in from are counted below, so others
        // can't fill the counts' tables.
        let account = header(head, "x-crosscom-account").map(text).unwrap_or_default();
        let pid = self.accounts.pid(&account).ok().flatten().filter(|pid| self.online.played_from(*pid, client));
        let Some(pid) = pid else {
            return Err(error(
                "403 Forbidden",
                "sign in to the game first; logs are taken from players who have played from this address recently",
            ));
        };
        if !self.allow(client, now) {
            return Err(error("429 Too Many Requests", "too many logs from here; try again later"));
        }
        if !self.allow_account(pid, len as u64, now) {
            return Err(error("429 Too Many Requests", "too many logs from this account today; try again tomorrow"));
        }
        if self.at_once.fetch_add(1, Ordering::AcqRel) >= MAX_AT_ONCE {
            self.at_once.fetch_sub(1, Ordering::AcqRel);
            return Err(error("503 Service Unavailable", "busy; try again later"));
        }
        Ok(Slot {
            at_once: Arc::clone(&self.at_once),
            len,
        })
    }

    /// Counts an upload from `client`; whether it's within the limit.
    fn allow(&self, client: IpAddr, now: Instant) -> bool {
        let mut uploads = self.uploads.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = address_key(client);
        if uploads.len() >= MAX_ENTRIES && !uploads.contains_key(&key) {
            uploads.retain(|_, (since, _)| now.duration_since(*since) < WINDOW);
            // Still full: the oldest goes, rather than refusing every new address.
            if uploads.len() >= MAX_ENTRIES {
                if let Some(oldest) = uploads.iter().min_by_key(|(_, (since, _))| *since).map(|(k, _)| *k) {
                    uploads.remove(&oldest);
                }
            }
        }
        let (since, count) = uploads.entry(key).or_insert((now, 0));
        if now.duration_since(*since) >= WINDOW {
            *since = now;
            *count = 0;
        }
        *count += 1;
        *count <= MAX_PER_ADDRESS
    }

    /// Counts a log of `bytes` from account `pid`; whether it's within the account's share.
    fn allow_account(&self, pid: u32, bytes: u64, now: Instant) -> bool {
        let mut accounts = self.by_account.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if accounts.len() >= MAX_ENTRIES && !accounts.contains_key(&pid) {
            accounts.retain(|_, (since, ..)| now.duration_since(*since) < ACCOUNT_WINDOW);
            if accounts.len() >= MAX_ENTRIES {
                if let Some(oldest) = accounts.iter().min_by_key(|(_, (since, ..))| *since).map(|(k, _)| *k) {
                    accounts.remove(&oldest);
                }
            }
        }
        let (since, count, total) = accounts.entry(pid).or_insert((now, 0, 0));
        if now.duration_since(*since) >= ACCOUNT_WINDOW {
            (*since, *count, *total) = (now, 0, 0);
        }
        if *count >= MAX_PER_ACCOUNT || *total + bytes > MAX_BYTES_PER_ACCOUNT {
            return false;
        }
        *count += 1;
        *total += bytes;
        true
    }

    /// Keeps a log [`Self::check`] let through: its body and headers, from `client`.
    pub fn store(&self, client: IpAddr, head: &str, body: &[u8]) -> (Answer, Option<Report>) {
        self.store_at(client, head, body, now_secs())
    }

    fn store_at(&self, client: IpAddr, head: &str, body: &[u8], now: u64) -> (Answer, Option<Report>) {
        let value = |name: &str| header(head, name).map(text).filter(|v| !v.is_empty());
        let account = value("x-crosscom-account").unwrap_or_default();
        let run = value("x-crosscom-run");
        let created = |id: &str| ("201 Created", serde_json::json!({ "id": id }).to_string());
        let key = run.clone().map(|run| (account.to_lowercase(), run));
        if let Some(id) = key.as_ref().and_then(|k| self.taken.lock().ok()?.get(k).cloned()) {
            return (created(&id), None);
        }
        let id = new_id(now);
        let (y, mo, d, h, mi, s) = utc(now);
        let report = Report {
            id: id.clone(),
            received: now,
            received_utc: format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC"),
            address: client.to_canonical().to_string(),
            account: account.clone(),
            client: value("x-crosscom-client"),
            launcher: value("x-crosscom-launcher"),
            exe: value("x-crosscom-exe"),
            os: value("x-crosscom-os"),
            run,
            exit: value("x-crosscom-exit").and_then(|v| v.parse().ok()),
            bytes: body.len() as u64,
        };
        let _store = self.store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(e) = write(&self.settings.dir, &report, body) {
            return (error("500 Internal Server Error", &format!("couldn't keep the log: {e}")), None);
        }
        prune(&self.settings.dir, self.settings.keep_days, self.settings.max_bytes, now);
        if let Some(key) = key {
            let mut taken = self.taken.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if taken.len() >= MAX_ENTRIES {
                taken.clear();
            }
            taken.insert(key, id.clone());
        }
        (created(&id), Some(report))
    }
}

/// The file name (without extension) a report is kept under.
fn stem(report: &Report) -> String {
    format!("{}-{}", report.id, file_part(&report.account, MAX_NAME))
}

fn write(dir: &Path, report: &Report, body: &[u8]) -> std::io::Result<()> {
    let day = dir.join(day_of(&report.id));
    fs::create_dir_all(&day)?;
    let stem = stem(report);
    fs::write(day.join(format!("{stem}.log")), body)?;
    let mut json = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    json.push(b'\n');
    fs::File::create(day.join(format!("{stem}.json")))?.write_all(&json)
}

/// The day folders in `dir`, oldest first.
fn days(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut days: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().into_string().ok().filter(|n| is_day(n))?, e.path())))
        .collect();
    days.sort();
    days
}

/// The files in a day folder, by name (oldest report first), with their sizes.
fn files(day: &Path) -> Vec<(String, PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(day) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(fs::Metadata::is_file)?;
            Some((e.file_name().into_string().ok()?, e.path(), meta.len()))
        })
        .collect();
    files.sort();
    files
}

/// Deletes the days older than `keep_days`, then the oldest reports until the rest come to at
/// most `max_bytes`.
fn prune(dir: &Path, keep_days: u32, max_bytes: u64, now: u64) {
    let oldest_kept = day(now.saturating_sub(u64::from(keep_days) * 86_400));
    let mut kept = Vec::new();
    for (name, path) in days(dir) {
        if name < oldest_kept {
            let _ = fs::remove_dir_all(&path);
        } else {
            kept.push(path);
        }
    }
    let all: Vec<(PathBuf, u64)> = kept.iter().flat_map(|d| files(d)).map(|(_, path, size)| (path, size)).collect();
    let mut total: u64 = all.iter().map(|(_, size)| size).sum();
    for (path, size) in all {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
    for day in kept {
        // Only succeeds when empty.
        let _ = fs::remove_dir(day);
    }
}

/// The reports kept in `dir`, newest first.
#[must_use]
pub fn list(dir: &Path) -> Vec<Report> {
    let mut reports: Vec<Report> = days(dir)
        .iter()
        .flat_map(|(_, day)| files(day))
        .filter(|(name, ..)| name.ends_with(".json"))
        .filter_map(|(_, path, _)| serde_json::from_slice(&fs::read(path).ok()?).ok())
        .filter(|r: &Report| valid_id(&r.id))
        .collect();
    reports.sort_by(|a, b| b.id.cmp(&a.id));
    reports
}

/// The log of report `id`, if it's kept.
#[must_use]
pub fn find(dir: &Path, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    let prefix = format!("{id}-");
    files(&dir.join(day_of(id)))
        .into_iter()
        .find(|(name, ..)| name.starts_with(&prefix) && name.ends_with(".log"))
        .map(|(_, path, _)| path)
}

/// `crosscom-server logs list`: the reports, newest first, one a line.
#[must_use]
pub fn listing(reports: &[Report]) -> String {
    let mut out = format!(
        "{:<22}  {:<23}  {:<16}  {:>9}  {:<4}  {:<8}  {}\n",
        "REPORT", "RECEIVED", "ACCOUNT", "BYTES", "EXE", "CLIENT", "EXIT"
    );
    for r in reports {
        // Values are kept printable (`text`); `{:?}`-free so they read as they are.
        out.push_str(&format!(
            "{:<22}  {:<23}  {:<16}  {:>9}  {:<4}  {:<8}  {}\n",
            r.id,
            r.received_utc,
            r.account,
            r.bytes,
            r.exe.as_deref().unwrap_or("-"),
            r.client.as_deref().unwrap_or("-"),
            r.exit.map_or_else(|| "-".to_owned(), |e| e.to_string())
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KOZAK: &str = "198.51.100.7";

    fn logs(dir: &Path, accept: bool) -> (Logs, Arc<Online>) {
        let accounts = Arc::new(Accounts::open(Path::new(":memory:")).unwrap());
        let pid = accounts.add("Kozak", "hunter2").unwrap();
        accounts.add("Pepper", "salt").unwrap();
        let online = Arc::new(Online::default());
        online.signed_in(pid, KOZAK.parse().unwrap());
        let settings = Settings {
            accept,
            dir: dir.to_owned(),
            max_bytes: 2048 * 1024 * 1024,
            keep_days: 30,
        };
        (Logs::new(settings, accounts, Arc::clone(&online)), online)
    }

    fn head(account: &str, len: usize) -> String {
        format!("POST /api/logs HTTP/1.1\r\nX-Crosscom-Account: {account}\r\nX-Crosscom-Run: crosscom-20261115-120000.log\r\nContent-Length: {len}")
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn strangers_arent_counted_and_an_account_has_a_daily_share() {
        let dir = tempfile::tempdir().unwrap();
        let (logs, _) = logs(dir.path(), true);
        let now = Instant::now();
        for i in 0..100u32 {
            let stranger = IpAddr::from([203, 0, 113, (i % 250) as u8]);
            assert!(logs.check_at(stranger, &head("Kozak", 10), now).is_err());
        }
        assert!(logs.uploads.lock().unwrap().is_empty(), "refused addresses aren't counted");
        let pid = logs.accounts.pid("Kozak").unwrap().unwrap();
        for _ in 0..MAX_PER_ACCOUNT {
            assert!(logs.allow_account(pid, 10, now));
        }
        assert!(!logs.allow_account(pid, 10, now), "the count");
        assert!(logs.allow_account(pid, 10, now + ACCOUNT_WINDOW), "a new day");
        assert!(!logs.allow_account(pid, MAX_BYTES_PER_ACCOUNT, now + ACCOUNT_WINDOW), "the bytes");
    }

    #[test]
    fn only_from_where_the_account_played() {
        let dir = tempfile::tempdir().unwrap();
        let (logs, online) = logs(dir.path(), true);
        assert!(logs.check(ip(KOZAK), &head("Kozak", 10)).is_ok());
        assert!(logs.check(ip(KOZAK), &head("kozak", 10)).is_ok(), "names in any case");
        let refused = |client: &str, account: &str| logs.check(ip(client), &head(account, 10)).err().map(|(status, json)| (status, json.contains("sign in")));
        assert_eq!(refused("198.51.100.8", "Kozak"), Some(("403 Forbidden", true)), "another address");
        assert_eq!(refused(KOZAK, "Pepper"), Some(("403 Forbidden", true)), "an account that didn't play from there");
        assert_eq!(refused(KOZAK, "Nobody"), Some(("403 Forbidden", true)), "no such account");
        assert_eq!(refused(KOZAK, ""), Some(("403 Forbidden", true)), "no account");

        let accounts = Arc::new(Accounts::open(Path::new(":memory:")).unwrap());
        let old = accounts.add("Old", "x").unwrap();
        online.signed_in_ago(old, ip("198.51.100.9"), crate::online::RECENT + Duration::from_secs(1));
        let logs = Logs::new(logs.settings.clone(), accounts, Arc::clone(&online));
        assert_eq!(logs.check(ip("198.51.100.9"), &head("Old", 10)).err().map(|a| a.0), Some("403 Forbidden"), "over a day ago");

        // The telemetry account, signing in from the same address, counts for nothing.
        let accounts = Arc::new(Accounts::open(Path::new(":memory:")).unwrap());
        let online = Arc::new(Online::default());
        online.signed_in(crate::ticket::TRACKING_PID, ip(KOZAK));
        let logs = Logs::new(logs.settings.clone(), accounts, online);
        assert_eq!(logs.check(ip(KOZAK), &head("Tracking", 10)).err().map(|a| a.0), Some("403 Forbidden"));
    }

    #[test]
    fn ipv6_by_the_64() {
        let dir = tempfile::tempdir().unwrap();
        let (logs, online) = logs(dir.path(), true);
        online.signed_in(logs.accounts.pid("Pepper").unwrap().unwrap(), ip("2001:db8:1:2::5"));
        assert!(logs.check(ip("2001:db8:1:2:aaaa::1"), &head("Pepper", 1)).is_ok());
        assert!(logs.check(ip("2001:db8:1:3::5"), &head("Pepper", 1)).is_err());
    }

    #[test]
    fn limits_size_rate_and_uploads_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (logs, _) = logs(dir.path(), true);
        let status = |r: Result<Slot, Answer>| r.err().map(|a| a.0);
        assert_eq!(status(logs.check(ip(KOZAK), &head("Kozak", MAX_BODY + 1))), Some("413 Content Too Large"));
        assert_eq!(
            status(logs.check(ip(KOZAK), "POST /api/logs HTTP/1.1\r\nX-Crosscom-Account: Kozak")),
            Some("411 Length Required")
        );
        let held: Vec<Slot> = (0..MAX_AT_ONCE).map(|_| logs.check(ip(KOZAK), &head("Kozak", 1)).ok().unwrap()).collect();
        assert_eq!(status(logs.check(ip(KOZAK), &head("Kozak", 1))), Some("503 Service Unavailable"));
        drop(held);
        let now = Instant::now();
        for _ in 0..MAX_PER_ADDRESS - MAX_AT_ONCE as u32 - 1 {
            assert!(logs.check_at(ip(KOZAK), &head("Kozak", 1), now).is_ok());
        }
        assert_eq!(status(logs.check_at(ip(KOZAK), &head("Kozak", 1), now)), Some("429 Too Many Requests"));
        assert!(logs.check_at(ip(KOZAK), &head("Kozak", 1), now + WINDOW).is_ok(), "an hour later");

        let (off, _) = self::logs(dir.path(), false);
        let (status, json) = off.check(ip(KOZAK), &head("Kozak", 1)).err().unwrap();
        assert_eq!(status, "403 Forbidden");
        assert!(json.contains("doesn't take logs"));
    }

    #[test]
    fn keeps_each_log_with_what_it_said() {
        let dir = tempfile::tempdir().unwrap();
        let (logs, _) = logs(dir.path(), true);
        let head = "POST /api/logs HTTP/1.1\r\nX-Crosscom-Account: ../../etc/Kozak\r\nX-Crosscom-Exe: DX11\r\nX-Crosscom-Os: Windows 11 Pro\u{1b}[31m\r\n\
                    X-Crosscom-Run: crosscom-20261115-120000.log\r\nX-Crosscom-Exit: -1073741819\r\nContent-Length: 8";
        let now = 1_794_746_096;
        let ((status, json), report) = logs.store_at(ip(KOZAK), head, b"the log\n", now);
        assert_eq!(status, "201 Created");
        let id = serde_json::from_str::<serde_json::Value>(&json).unwrap()["id"].as_str().unwrap().to_owned();
        assert!(valid_id(&id) && id.starts_with("20261115-123456-"), "{id}");
        let report = report.unwrap();
        assert_eq!(report.os.as_deref(), Some("Windows 11 Pro[31m"), "no control characters");
        assert_eq!(report.exit, Some(-1_073_741_819));
        let log = dir.path().join("2026-11-15").join(format!("{id}-etcKozak.log"));
        assert_eq!(fs::read(&log).unwrap(), b"the log\n", "no way out of its folder");
        assert!(log.with_extension("json").is_file());
        assert_eq!(find(dir.path(), &id), Some(log));
        assert_eq!(list(dir.path())[0].address, KOZAK);

        // The same run again (the launcher and the DLL at once): the same report.
        let ((_, again), report) = logs.store_at(ip(KOZAK), head, b"the log\n", now + 5);
        assert_eq!(again, json);
        assert!(report.is_none());
        assert_eq!(list(dir.path()).len(), 1);

        assert_eq!(find(dir.path(), "../../etc/passwd"), None);
        assert_eq!(find(dir.path(), "20261115-123456-zzzzzz"), None);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_part("Kozak", 32), "Kozak");
        assert_eq!(file_part("..", 32), "unknown");
        assert_eq!(file_part("../..\\x/y", 32), "xy");
        assert_eq!(file_part("", 32), "unknown");
        assert_eq!(file_part(&"a".repeat(100), 32).len(), 32);
        assert_eq!(file_part("Zoë Q", 32), "ZoQ");
        assert!(valid_id("20261115-123456-ab12cd"));
        assert!(!valid_id("20261115-123456-AB12CD"));
        assert!(!valid_id("2026111/-123456-ab12cd"));
        assert_eq!(new_id(1_794_746_096).len(), 22);
        assert!(valid_id(&new_id(0)));
    }

    #[test]
    fn prunes_old_days_then_the_oldest_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let day_secs = 86_400;
        let now = 1_794_746_096;
        let report = |at: u64, n: u8| Report {
            id: format!("{}-{n:06x}", &new_id(at)[..15]),
            account: "Kozak".into(),
            ..Report::default()
        };
        write(dir.path(), &report(now - 40 * day_secs, 1), &[b'x'; 100]).unwrap();
        for n in 0..5 {
            write(dir.path(), &report(now - day_secs, n), &[b'x'; 100]).unwrap();
        }
        write(dir.path(), &report(now, 9), &[b'x'; 100]).unwrap();
        fs::create_dir(dir.path().join("not-a-day")).unwrap();
        prune(dir.path(), 30, 10_000, now);
        assert_eq!(list(dir.path()).len(), 6, "the day 40 days ago went");
        assert!(dir.path().join("not-a-day").is_dir(), "only day folders are touched");
        // Each report is 100 bytes and its JSON; a cap that holds about three.
        let json = fs::metadata(dir.path().join(day(now)).join(format!("{}.json", stem(&report(now, 9))))).unwrap().len();
        prune(dir.path(), 30, 3 * (100 + json), now);
        let left: Vec<String> = list(dir.path()).into_iter().map(|r| r.id).collect();
        assert_eq!(left.len(), 3, "{left:?}");
        assert!(left[0].ends_with("09"), "the newest stays");
    }
}
