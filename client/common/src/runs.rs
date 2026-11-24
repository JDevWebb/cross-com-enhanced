//! Each run of the game gets its own log, and finished runs are sent to the server.
//!
//! The orbit DLL writes `crosscom.log` (see `logging`); the uplay DLL writes nothing. As the
//! orbit DLL starts, before it opens the log, the last run's `crosscom.log` (if it has anything
//! in it) moves to `logs/crosscom-<yyyymmdd-hhmmss>.log` beside the exe, named by the time (UTC)
//! that run started, and the new run starts a new `crosscom.log`. The launcher moves it the same
//! way once a game it started has ended. The newest [`MAX_KEPT`] runs are kept.
//!
//! A run not yet sent is pending. Once the server has taken it, it's renamed
//! `crosscom-<...>.sent.log`, so it isn't sent again and the player still has it.
//!
//! What's sent is a copy with the private parts hidden (see [`redact`]): the Windows user name
//! (wherever it appears, and the name in any home folder) and the account's password (in any
//! case, and escaped as JSON or a URL would have it). The account name stays: the server knows
//! it. A long log is cut to [`MAX_UPLOAD`] (see [`cap`]), only ever between lines, so no password
//! is left in part at a cut. The files on disk are never changed.
//!
//! Each run's log starts with the server and account it played with ([`begin`]), and a run is
//! only sent to that server, with that account set up (so the password hidden is the one it
//! signed in with): one played on another server, or with another account, or by a client from
//! before runs said so, waits (and is in time deleted, as all runs are).
//!
//! `POST /api/logs` takes the log as its body (`text/plain`), with what's known about it in
//! `X-Crosscom-*` headers (see [`headers`]). The server answers `201 {"id": "<report id>"}`.
//! It only takes logs from an address its account has recently played from (`403` otherwise).

use std::fs;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::logging;

/// The folder beside the exe that holds the runs.
pub const DIR: &str = "logs";
/// Runs kept, sent or not; older ones are deleted.
pub const MAX_KEPT: usize = 20;
/// Runs sent at a time, newest first; the rest wait for the next time.
pub const MAX_PER_PASS: usize = 5;
/// The server's page.
pub const PATH: &str = "/api/logs";
/// The most of one run that's sent.
pub const MAX_UPLOAD: usize = 2 * 1024 * 1024;
/// How much of the start of a longer run is sent; the rest is its end.
const HEAD_KEPT: usize = 256 * 1024;
/// Room left for the line that says what was cut.
const MARKER_ROOM: usize = 64;
const PREFIX: &str = "crosscom-";
const PENDING: &str = ".log";
const SENT: &str = ".sent.log";
/// Shortest Windows user name or password hidden wherever it appears: hiding one or two letters
/// everywhere would leave the log unreadable.
const MIN_HIDDEN: usize = 3;
/// The longest header value sent.
const MAX_HEADER: usize = 128;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The civil date (UTC) and time of day of a Unix time: `(year, month, day, hour, minute, second)`.
#[must_use]
pub fn utc(secs: u64) -> (u64, u32, u32, u32, u32, u32) {
    // Howard Hinnant's civil_from_days.
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as u64;
    (year, month, day, (rest / 3600) as u32, (rest / 60 % 60) as u32, (rest % 60) as u32)
}

/// `yyyymmdd-hhmmss` (UTC).
#[must_use]
pub fn stamp(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// The time a run file's name gives, for people: `2026-11-15 12:34:56 UTC`.
#[must_use]
pub fn run_time(name: &str) -> Option<String> {
    let stamp = name.strip_prefix(PREFIX)?.get(..15)?;
    let digits = |r: std::ops::Range<usize>| stamp.get(r).filter(|s| s.bytes().all(|b| b.is_ascii_digit()));
    if stamp.as_bytes()[8] != b'-' {
        return None;
    }
    Some(format!(
        "{}-{}-{} {}:{}:{} UTC",
        digits(0..4)?,
        digits(4..6)?,
        digits(6..8)?,
        digits(9..11)?,
        digits(11..13)?,
        digits(13..15)?
    ))
}

/// When a log's run started: the Unix time its first line begins with.
fn first_line_time(path: &Path) -> Option<u64> {
    let mut start = [0u8; 32];
    let n = fs::File::open(path).and_then(|mut f| f.read(&mut start)).ok()?;
    let digits: String = start[..n].iter().take_while(|b| b.is_ascii_digit()).map(|&b| char::from(b)).collect();
    digits.parse().ok()
}

/// Moves the last run's `crosscom.log` in `game_dir` (if it has anything in it) into the runs
/// folder; where it went. `Ok(None)` when there was nothing to move, or another process moved it
/// first.
pub fn rotate(game_dir: &Path) -> io::Result<Option<PathBuf>> {
    let log = game_dir.join(logging::FILE_NAME);
    let meta = match fs::metadata(&log) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if meta.len() == 0 {
        return Ok(None);
    }
    let modified = || meta.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
    let started = first_line_time(&log).or_else(modified).unwrap_or_else(now);
    let dir = game_dir.join(DIR);
    fs::create_dir_all(&dir)?;
    let stamp = stamp(started);
    let taken = |name: &str| dir.join(format!("{name}{PENDING}")).exists() || dir.join(format!("{name}{SENT}")).exists();
    let mut name = format!("{PREFIX}{stamp}");
    let mut n = 2;
    while taken(&name) {
        name = format!("{PREFIX}{stamp}-{n}");
        n += 1;
    }
    let target = dir.join(format!("{name}{PENDING}"));
    match fs::rename(&log, &target) {
        Ok(()) => Ok(Some(target)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// How soon after a log began a game loading this library is still the same launch: the game
/// starts itself again through `Future Soldier.exe` a moment after it's first started, and that
/// second process's log carries on the first's.
pub const SAME_LAUNCH_SECS: u64 = 30;

/// As the game loads this library: [`rotate`], unless `crosscom.log` began less than
/// [`SAME_LAUNCH_SECS`] ago (the same launch, starting itself again), which carries on in it.
pub fn rotate_at_start(game_dir: &Path) -> io::Result<Option<PathBuf>> {
    rotate_at_start_at(game_dir, now())
}

fn rotate_at_start_at(game_dir: &Path, now: u64) -> io::Result<Option<PathBuf>> {
    let log = game_dir.join(logging::FILE_NAME);
    if first_line_time(&log).is_some_and(|began| now.saturating_sub(began) < SAME_LAUNCH_SECS) {
        return Ok(None);
    }
    rotate(game_dir)
}

/// A run's log in the runs folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub path: PathBuf,
    /// Its file name.
    pub name: String,
    pub sent: bool,
}

/// The runs in `game_dir`'s runs folder, newest first.
#[must_use]
pub fn runs(game_dir: &Path) -> Vec<Run> {
    let Ok(entries) = fs::read_dir(game_dir.join(DIR)) else {
        return Vec::new();
    };
    let mut runs: Vec<Run> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !name.starts_with(PREFIX) || !name.ends_with(PENDING) || !e.file_type().ok()?.is_file() {
                return None;
            }
            Some(Run {
                path: e.path(),
                sent: name.ends_with(SENT),
                name,
            })
        })
        .collect();
    // The names start with the time, so they sort by it.
    runs.sort_by(|a, b| b.name.cmp(&a.name));
    runs
}

/// Deletes all but the newest [`MAX_KEPT`] runs.
pub fn prune(game_dir: &Path) {
    for run in runs(game_dir).into_iter().skip(MAX_KEPT) {
        let _ = fs::remove_file(run.path);
    }
}

/// The runs not sent yet, newest first, at most [`MAX_PER_PASS`].
#[must_use]
pub fn pending(game_dir: &Path) -> Vec<Run> {
    runs(game_dir).into_iter().filter(|r| !r.sent).take(MAX_PER_PASS).collect()
}

/// The runs not sent yet that were played on `server` as `account` (see [`begin`]), newest
/// first, at most [`MAX_PER_PASS`].
#[must_use]
pub fn pending_for(game_dir: &Path, server: &str, account: &str) -> Vec<Run> {
    runs(game_dir)
        .into_iter()
        .filter(|r| !r.sent && played_in(&r.path).is_some_and(|p| p.is(server, account)))
        .take(MAX_PER_PASS)
        .collect()
}

/// What a run says at its start it was played with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    /// The server's host (as [`host`] gives it).
    pub server: String,
    pub account: String,
}

impl Played {
    /// Whether this is `server` (as typed or as a host) and `account`.
    #[must_use]
    pub fn is(&self, server: &str, account: &str) -> bool {
        !self.server.is_empty() && self.server == host(server) && self.account.trim().eq_ignore_ascii_case(account.trim())
    }
}

/// What a run's log starts with to say what it played with: `<time> INFO  orbit Run: {...}`.
const PLAYED: &str = " Run: ";
/// How far into a log [`begin`]'s line is looked for.
const PLAYED_WITHIN: usize = 16 * 1024;

/// A server as typed, as a host: no `http(s)://`, no trailing `/`, lower case.
fn host(server: &str) -> String {
    let s = server.trim();
    let s = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")).unwrap_or(s);
    s.trim_end_matches('/').to_ascii_lowercase()
}

/// Starts this run's `crosscom.log` in `game_dir` (or adds to it, for a run that carries on in
/// it) with the server and account it plays with, so its log is only sent there. Written to the
/// file itself, whatever the log level.
pub fn begin(game_dir: &Path, server: &str, account: &str) -> io::Result<()> {
    use std::io::Write;
    let record = serde_json::json!({ "server": host(server), "account": account.trim() });
    let line = format!("{}.000 INFO  orbit{PLAYED}{record}\n", now());
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(game_dir.join(logging::FILE_NAME))?
        .write_all(line.as_bytes())
}

/// What `text` (a log's start) says it played with: the first line [`begin`] wrote.
fn played(text: &str) -> Option<Played> {
    let line = text.lines().take(50).find(|l| l.contains(" INFO  orbit Run: {"))?;
    let json: serde_json::Value = serde_json::from_str(line.split_once(PLAYED)?.1).ok()?;
    Some(Played {
        server: json["server"].as_str()?.to_owned(),
        account: json["account"].as_str()?.to_owned(),
    })
}

/// What the run at `path` says it played with.
fn played_in(path: &Path) -> Option<Played> {
    let mut start = Vec::new();
    fs::File::open(path).ok()?.take(PLAYED_WITHIN as u64).read_to_end(&mut start).ok()?;
    played(&String::from_utf8_lossy(&start))
}

/// Marks a run as sent (renames it `*.sent.log`).
pub fn mark_sent(run: &Run) -> io::Result<PathBuf> {
    let stem = run.name.strip_suffix(PENDING).unwrap_or(&run.name);
    let target = run.path.with_file_name(format!("{stem}{SENT}"));
    fs::rename(&run.path, &target)?;
    Ok(target)
}

/// What's hidden in a log before it's sent.
#[derive(Debug, Clone, Default)]
pub struct Private {
    /// The Windows user names (the account's, and its home folder's name when that differs).
    pub names: Vec<String>,
    /// The account's password.
    pub password: String,
    /// The account's name, kept where it appears whole even when it holds a hidden name.
    pub account: String,
}

impl Private {
    /// This PC's Windows user (`USERNAME`, and the last part of `USERPROFILE`), with the
    /// account's name and password.
    #[must_use]
    pub fn of_this_pc(account: &str, password: &str) -> Self {
        let profile = std::env::var("USERPROFILE").ok();
        let profile_name = profile
            .as_deref()
            .and_then(|p| p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next())
            .map(str::to_owned);
        let mut names: Vec<String> = std::env::var("USERNAME").ok().into_iter().chain(profile_name).map(|n| n.trim().to_owned()).collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        Self {
            names,
            password: password.to_owned(),
            account: account.to_owned(),
        }
    }
}

/// `text` with the private parts hidden:
/// - the password, wherever it appears, in any (ASCII) case and escaped in any of the usual ways
///   (see [`secret_at`]), as `<password>`;
/// - the Windows user names, wherever they appear (in any case, inside longer words too), as
///   `<user>`, except where the account's name appears whole;
/// - the user's folder name in home folders (`C:\Users\<name>`, `/home/<name>`, also with doubled
///   backslashes), as `<user>`, whoever's it is.
///
/// A password or name shorter than 3 characters is only hidden in home folders.
#[must_use]
pub fn redact(text: &str, private: &Private) -> String {
    // The password first, on its own: nothing kept (such as the account name inside it) can
    // leave part of it.
    let text = if private.password.chars().count() >= MIN_HIDDEN {
        hide_secret(text, &private.password, "<password>")
    } else {
        text.to_owned()
    };
    let text = text.as_str();
    let mut needles: Vec<(String, bool, &str)> = Vec::new();
    for name in &private.names {
        if name.chars().count() >= MIN_HIDDEN {
            needles.push((name.to_ascii_lowercase(), true, "<user>"));
        }
    }
    let account = private.account.trim();
    let keep: Vec<String> = (!account.is_empty()).then(|| account.to_ascii_lowercase()).into_iter().collect();
    hide_homes(&hide(text, &needles, &keep))
}

/// `text` with `secret` replaced by `with` wherever [`secret_at`] finds it.
fn hide_secret(text: &str, secret: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        if let Some(len) = secret_at(text.as_bytes(), i, secret) {
            out.push_str(with);
            i += len;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// How long `secret` is at byte `i` of `text`, if it's there, each of its characters as it is
/// (ASCII letters in either case) or written as:
/// - URL encoding: `%xx` for each of its UTF-8 bytes (either case), or `+` for a space;
/// - JSON's or Rust's escapes: `\"`, `\\`, `\/`, `\'`, `\n`, `\r`, `\t`, `\u` and four hex digits
///   (UTF-16, a pair for a character past U+FFFF) or `\u{41}`;
///
/// in any mix, as a log line built from a request might have it. The match ends where `secret`
/// does, always on a character boundary.
fn secret_at(text: &[u8], i: usize, secret: &str) -> Option<usize> {
    let mut chars = secret.chars();
    let Some(c) = chars.next() else { return Some(0) };
    // A `\`, `%` or `+` may be itself or start an escape: each way is tried.
    [raw_at(text, i, c), escaped_at(text, i, c)]
        .into_iter()
        .flatten()
        .find_map(|len| secret_at(text, i + len, chars.as_str()).map(|rest| len + rest))
}

/// How long `c` is at byte `at` of `text` as itself (an ASCII letter in either case).
fn raw_at(text: &[u8], at: usize, c: char) -> Option<usize> {
    let rest = text.get(at..)?;
    let mut utf8 = [0u8; 4];
    let raw = c.encode_utf8(&mut utf8).as_bytes();
    (rest.len() >= raw.len() && rest[..raw.len()].eq_ignore_ascii_case(raw)).then_some(raw.len())
}

/// How long `c` is at byte `at` of `text`, escaped in any of the ways [`secret_at`] takes.
fn escaped_at(text: &[u8], at: usize, c: char) -> Option<usize> {
    let rest = text.get(at..)?;
    let mut utf8 = [0u8; 4];
    let raw = c.encode_utf8(&mut utf8).as_bytes();
    let hex = |digits: &[u8]| -> Option<u32> {
        let digits = std::str::from_utf8(digits).ok().filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_hexdigit()))?;
        u32::from_str_radix(digits, 16).ok()
    };
    match rest.first()? {
        b'+' if c == ' ' => Some(1),
        b'%' => {
            // One %xx per UTF-8 byte.
            let mut n = 0;
            for byte in raw {
                let digits = rest.get(n..n + 3).filter(|d| d[0] == b'%')?;
                if hex(&digits[1..])? != u32::from(*byte) {
                    return None;
                }
                n += 3;
            }
            Some(n)
        }
        b'\\' => {
            let escaped = *rest.get(1)?;
            let simple = match escaped {
                b'n' => Some('\n'),
                b'r' => Some('\r'),
                b't' => Some('\t'),
                b'"' | b'\\' | b'/' | b'\'' => Some(char::from(escaped)),
                _ => None,
            };
            if simple == Some(c) {
                return Some(2);
            }
            if escaped != b'u' {
                return None;
            }
            if rest.get(2) == Some(&b'{') {
                // Rust's \u{41}.
                let end = rest.iter().take(12).position(|b| *b == b'}')?;
                return (hex(&rest[3..end])? == u32::from(c)).then_some(end + 1);
            }
            // JSON's \u and four hex digits, a surrogate pair past U+FFFF.
            let mut units = [0u16; 2];
            let mut n = 0;
            for unit in c.encode_utf16(&mut units).iter() {
                let digits = rest.get(n..n + 6).filter(|d| d.starts_with(b"\\u"))?;
                if hex(&digits[2..])? != u32::from(*unit) {
                    return None;
                }
                n += 6;
            }
            Some(n)
        }
        _ => None,
    }
}

/// `text` with each needle (`(text, ignoring ASCII case, replacement)`) replaced wherever it
/// appears, the longest where several start at one place, except where a `keep` word (lower case)
/// appears whole at least as long.
fn hide(text: &str, needles: &[(String, bool, &str)], keep: &[String]) -> String {
    if needles.is_empty() {
        return text.to_owned();
    }
    // ASCII case only: every byte stays where it was, so offsets match `text`.
    let lower = text.to_ascii_lowercase();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let whole_at =
        |i: usize, word: &str| lower[i..].starts_with(word) && !text[..i].chars().next_back().is_some_and(is_word) && !text[i + word.len()..].chars().next().is_some_and(is_word);
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let kept = keep.iter().filter(|k| whole_at(i, k)).map(String::len).max();
        let hidden = needles
            .iter()
            .filter(|(n, ci, _)| if *ci { lower[i..].starts_with(n.as_str()) } else { text[i..].starts_with(n.as_str()) })
            .max_by_key(|(n, ..)| n.len());
        match (kept, hidden) {
            (Some(keep), hide) if hide.is_none_or(|(n, ..)| keep >= n.len()) => {
                out.push_str(&text[i..i + keep]);
                i += keep;
            }
            (_, Some((n, _, with))) => {
                out.push_str(with);
                i += n.len();
            }
            _ => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// `text` with the user's folder in home folders as `<user>`: what follows `\Users\`, `/Users/`,
/// `\\Users\\`, `/home/` or `\home\` (in any case) up to the next separator, quote or line end.
/// The shared folders (`Public`, `Default`, `All Users`) stay.
fn hide_homes(text: &str) -> String {
    const HOMES: [&str; 6] = ["\\users\\", "/users/", "\\\\users\\\\", "/home/", "\\home\\", "\\\\home\\\\"];
    const SHARED: [&str; 4] = ["public", "default", "all users", "<user>"];
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let Some(home) = HOMES.iter().filter(|h| lower[i..].starts_with(*h)).max_by_key(|h| h.len()) else {
            out.push(c);
            i += c.len_utf8();
            continue;
        };
        let start = i + home.len();
        let end = text[start..]
            .find(['\\', '/', '"', '\'', '\r', '\n', '\t', '<', '>', '|', ',', ';', ')'])
            .map_or(text.len(), |n| start + n);
        out.push_str(&text[i..start]);
        let name = &lower[start..end];
        if name.is_empty() || SHARED.contains(&name) {
            out.push_str(&text[start..end]);
        } else {
            out.push_str("<user>");
        }
        i = end;
    }
    out
}

/// The largest char boundary at or before `i`.
fn floor_boundary(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `text` cut to at most [`MAX_UPLOAD`] bytes: when longer, its first 256 KiB and its end, at line
/// ends where it can, with a line between saying how much was cut.
#[must_use]
pub fn cap(text: String) -> String {
    if text.len() <= MAX_UPLOAD {
        return text;
    }
    let head = floor_boundary(&text, HEAD_KEPT);
    let head = text[..head].rfind('\n').map_or(head, |n| n + 1);
    let tail_len = MAX_UPLOAD - HEAD_KEPT - MARKER_ROOM;
    let mut tail = floor_boundary(&text, text.len() - tail_len);
    while tail < text.len() && !text.is_char_boundary(tail) {
        tail += 1;
    }
    if let Some(n) = text[tail..].find('\n') {
        tail += n + 1;
    }
    let cut = tail - head;
    format!("{}[... {cut} bytes cut ...]\n{}", &text[..head], &text[tail..])
}

/// A log's text, as much of it as could be sent: a file far over [`MAX_UPLOAD`] is read only at
/// its start and its end, so a huge log isn't read whole. Those are cut between lines (a line cut
/// across goes whole), so what's read is whole lines, and a password in one is hidden whole.
fn read_capped(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let limit = 2 * MAX_UPLOAD as u64;
    if len <= limit {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let mut head = vec![0u8; HEAD_KEPT];
    file.read_exact(&mut head)?;
    // Up to the end of the head's last whole line.
    head.truncate(head.iter().rposition(|b| *b == b'\n').map_or(0, |n| n + 1));
    let mut tail = Vec::new();
    file.seek(SeekFrom::End(-(MAX_UPLOAD as i64)))?;
    file.read_to_end(&mut tail)?;
    // From the start of the tail's first whole line.
    tail.drain(..tail.iter().position(|b| *b == b'\n').map_or(tail.len(), |n| n + 1));
    let cut = len - head.len() as u64 - tail.len() as u64;
    Ok(format!("{}[... {cut} bytes cut ...]\n{}", String::from_utf8_lossy(&head), String::from_utf8_lossy(&tail)))
}

/// A run, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    /// The run's file name.
    pub run: String,
    /// The log, private parts hidden and capped.
    pub body: String,
    /// `DX11` or `DX9`, from the log's first lines.
    pub exe: Option<&'static str>,
    /// The client version that wrote it, from its first line.
    pub client: Option<String>,
}

/// Reads a run and readies it to send.
pub fn prepare(run: &Run, private: &Private) -> io::Result<Upload> {
    let text = read_capped(&run.path)?;
    let (exe, client) = describe(&text);
    Ok(Upload {
        run: run.name.clone(),
        body: cap(redact(&text, private)),
        exe,
        client,
    })
}

/// The exe (`DX11` or `DX9`) and client version a log's first line names (`Cross-Com 0.1.0
/// loaded in Future Soldier DX11.exe from ...`).
fn describe(text: &str) -> (Option<&'static str>, Option<String>) {
    let Some(line) = text.lines().take(5).find(|l| l.contains("Cross-Com ") && l.contains(" loaded")) else {
        return (None, None);
    };
    let upper = line.to_ascii_uppercase();
    let exe = if upper.contains("DX11") {
        Some("DX11")
    } else if upper.contains("DX9") {
        Some("DX9")
    } else {
        None
    };
    let version = line.split("Cross-Com ").nth(1).and_then(|r| r.split(' ').next()).map(clean);
    (exe, version.filter(|v| !v.is_empty()))
}

/// A header value: printable, on one line, at most [`MAX_HEADER`] characters.
fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).take(MAX_HEADER).collect::<String>().trim().to_owned()
}

/// What's said about the runs sent, beyond each run's own.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    /// The account the game signs in with (the server checks it played from this address).
    pub account: String,
    /// The sender's client version, for a run whose log doesn't say.
    pub client: String,
    /// The launcher's version, when the launcher sends.
    pub launcher: Option<String>,
    /// The Windows version (see `system::os_version`).
    pub os: Option<String>,
    /// The game's exit code, for the run it belongs to (by file name).
    pub exit: Option<(String, i32)>,
}

/// The headers sent with `upload`.
#[must_use]
pub fn headers(upload: &Upload, meta: &Meta) -> Vec<(&'static str, String)> {
    let mut headers = vec![("Content-Type", "text/plain; charset=utf-8".to_owned())];
    let mut add = |name: &'static str, value: Option<&str>| {
        if let Some(value) = value.map(clean).filter(|v| !v.is_empty()) {
            headers.push((name, value));
        }
    };
    add("X-Crosscom-Account", Some(&meta.account));
    add("X-Crosscom-Client", Some(upload.client.as_deref().unwrap_or(&meta.client)));
    add("X-Crosscom-Launcher", meta.launcher.as_deref());
    add("X-Crosscom-Exe", upload.exe);
    add("X-Crosscom-Os", meta.os.as_deref());
    add("X-Crosscom-Run", Some(&upload.run));
    let exit = meta.exit.as_ref().filter(|(run, _)| *run == upload.run).map(|(_, code)| code.to_string());
    add("X-Crosscom-Exit", exit.as_deref());
    headers
}

/// Why a run wasn't taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The account hasn't played from this address lately: try again after playing online.
    NotSignedIn,
    /// The server doesn't take logs (turned off, or a server from before them).
    Off,
    /// Anything else, as said.
    Other(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::NotSignedIn => f.write_str("the server takes logs once you've played online from this PC"),
            Refused::Off => f.write_str("the server doesn't take logs"),
            Refused::Other(why) => f.write_str(why),
        }
    }
}

/// The report ID from the server's answer, or why it didn't take the run.
pub fn report_id(status: u16, body: &[u8]) -> Result<String, Refused> {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let said = || json["error"].as_str().map(clean);
    match status {
        200 | 201 => json["id"]
            .as_str()
            .map(clean)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Refused::Other("the server's answer made no sense".into())),
        403 if json["error"].as_str().is_some_and(|e| e.contains("sign in")) => Err(Refused::NotSignedIn),
        403..=405 => Err(Refused::Off),
        413 => Err(Refused::Other("the log is too big for the server".into())),
        429 => Err(Refused::Other("too many logs sent from here lately; they'll go later".into())),
        _ => Err(Refused::Other(said().unwrap_or_else(|| format!("the server answered {status}")))),
    }
}

/// A run the server took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub run: String,
    pub id: String,
}

/// Sends the pending runs in `game_dir` played on `server` as `meta.account` (see
/// [`pending_for`]), newest first, each with `send` (which posts the body and headers to
/// `server`'s [`PATH`] and gives the answer's status and body, or why it couldn't); `private` is
/// that account's. Each run taken is marked sent. Stops at the first that isn't; the rest stay
/// pending, as do runs played elsewhere.
pub fn send_pending(
    game_dir: &Path,
    server: &str,
    private: &Private,
    meta: &Meta,
    mut send: impl FnMut(&[u8], &[(&'static str, String)]) -> Result<(u16, Vec<u8>), String>,
) -> (Vec<Sent>, Option<Refused>) {
    let mut sent = Vec::new();
    for run in pending_for(game_dir, server, &meta.account) {
        let upload = match prepare(&run, private) {
            Ok(upload) => upload,
            Err(e) => return (sent, Some(Refused::Other(format!("couldn't read {}: {e}", run.name)))),
        };
        let answer = send(upload.body.as_bytes(), &headers(&upload, meta)).map_err(Refused::Other);
        match answer.and_then(|(status, body)| report_id(status, &body)) {
            Ok(id) => {
                // Another sender may have taken it meanwhile; it's sent either way.
                let _ = mark_sent(&run);
                sent.push(Sent { run: run.name, id });
            }
            Err(why) => return (sent, Some(why)),
        }
    }
    (sent, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_game_starting_itself_again_carries_on_in_its_log() {
        let game = tempdir();
        let log = game.join(logging::FILE_NAME);
        fs::write(&log, "1794708347.010 INFO  orbit Cross-Com loaded\n").unwrap();
        assert_eq!(rotate_at_start_at(&game, 1_794_708_347).unwrap(), None, "this launch's");
        assert!(log.exists());
        assert!(rotate_at_start_at(&game, 1_794_708_347 + SAME_LAUNCH_SECS).unwrap().is_some(), "an earlier run's");
    }

    #[test]
    fn a_password_holding_the_account_name_is_hidden_whole() {
        let private = Private {
            names: vec![],
            password: "kozak2024".into(),
            account: "Kozak".into(),
        };
        let out = redact("login kozak2024 for Kozak", &private);
        assert_eq!(out, "login <password> for Kozak");
    }

    fn private() -> Private {
        Private {
            names: vec!["jsmith".into(), "John Smith".into()],
            password: "hunter2!".into(),
            account: "Kozak".into(),
        }
    }

    #[test]
    fn hides_the_user_and_the_password() {
        let text = "1700000000.000 INFO  orbit Cross-Com 0.1.0 loaded in Future Soldier DX11.exe from C:\\Users\\John Smith\\Games\\GRFS\n\
                    saves in c:/users/jsmith/AppData and JSMITH-PC, password hunter2! here\n\
                    signing in as \"Kozak\"\n";
        let out = redact(text, &private());
        assert!(!out.to_ascii_lowercase().contains("smith"), "{out}");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("C:\\Users\\<user>\\Games\\GRFS"), "{out}");
        assert!(out.contains("c:/users/<user>/AppData"), "{out}");
        assert!(out.contains("<user>-PC"), "{out}");
        assert!(out.contains("password <password> here"), "{out}");
        assert!(out.contains("\"Kozak\""), "the account name stays: {out}");
    }

    #[test]
    fn hides_any_home_folder() {
        let p = Private::default();
        assert_eq!(redact("C:\\Users\\someone_else\\x", &p), "C:\\Users\\<user>\\x");
        assert_eq!(redact("\"C:\\\\Users\\\\jo\\\\x\"", &p), "\"C:\\\\Users\\\\<user>\\\\x\"");
        assert_eq!(redact("Z:\\home\\jo\\.wine", &p), "Z:\\home\\<user>\\.wine");
        assert_eq!(redact("/home/jo/games", &p), "/home/<user>/games");
        assert_eq!(redact("C:\\Users\\Public\\Documents", &p), "C:\\Users\\Public\\Documents");
        assert_eq!(redact("no path here", &p), "no path here");
    }

    #[test]
    fn the_account_stays_even_when_its_the_windows_name() {
        let p = Private {
            names: vec!["Kozak".into()],
            password: String::new(),
            account: "Kozak".into(),
        };
        let out = redact("as Kozak from C:\\Users\\Kozak\\Games, kozak1987", &p);
        assert_eq!(out, "as Kozak from C:\\Users\\<user>\\Games, <user>1987");
        // A short name or password is only hidden in home folders.
        let p = Private {
            names: vec!["jo".into()],
            password: "ab".into(),
            account: String::new(),
        };
        assert_eq!(redact("job ab C:\\Users\\jo\\x", &p), "job ab C:\\Users\\<user>\\x");
    }

    #[test]
    fn the_password_is_hidden_in_any_case() {
        let p = Private {
            password: "Secret".into(),
            ..Private::default()
        };
        assert_eq!(redact("Secret secret xSECRETx Secre", &p), "<password> <password> x<password>x Secre");
    }

    #[test]
    fn the_password_is_hidden_escaped_too() {
        let p = Private {
            password: "p@ss w\"d/\\é😀".into(),
            ..Private::default()
        };
        let bs = '\\';
        let hidden = |text: &str| {
            assert_eq!(redact(&format!("[{text}]"), &p), "[<password>]", "{text}");
        };
        hidden("p@ss w\"d/\\é😀");
        hidden("P@SS W\"D/\\é😀");
        // JSON, as serde_json writes it, and with every character past ASCII as \u escapes.
        hidden(serde_json::to_string("p@ss w\"d/\\é😀").unwrap().trim_matches('"'));
        hidden(&format!("p@ss w{bs}\"d{bs}/{bs}{bs}{bs}u00e9{bs}ud83d{bs}ude00"));
        // Rust's Debug.
        hidden(format!("{:?}", "p@ss w\"d/\\é😀").trim_matches('"'));
        hidden(&format!("p@ss w\"d/\\{bs}u{{e9}}{bs}u{{1f600}}"));
        // URL-encoded, either case, a space as + or %20.
        hidden("p%40ss+w%22d%2F%5C%C3%A9%F0%9F%98%80");
        hidden("p%40ss%20w%22d%2f%5c%c3%a9%f0%9f%98%80");
        // Not the password: left alone.
        assert_eq!(redact("p%40ss w%22d/", &p), "p%40ss w%22d/");
        assert_eq!(redact("%4", &p), "%4");
        assert_eq!(redact(&format!("x{bs}u{{"), &p), format!("x{bs}u{{"));
    }

    #[test]
    fn a_password_across_the_cut_of_a_huge_log_isnt_left_in_part() {
        let dir = tempdir();
        let path = dir.join("big.log");
        let p = Private {
            password: "hunter2!".into(),
            ..Private::default()
        };
        // A line holding the password, across where the head is cut and where the tail starts.
        let filler = |n: usize| "x".repeat(n);
        let tail_starts = 5 * MAX_UPLOAD - MAX_UPLOAD;
        let text = format!(
            "{}\nsigned in with hunter2! here\n{}\nagain hunter2! there\n{}\n",
            filler(HEAD_KEPT - 20),
            filler(tail_starts - HEAD_KEPT - 21),
            filler(MAX_UPLOAD - 12)
        );
        assert_eq!(text.len(), 5 * MAX_UPLOAD);
        assert_eq!(&text[HEAD_KEPT - 4..HEAD_KEPT], "hunt", "across the head's end");
        assert_eq!(&text[tail_starts..tail_starts + 4], "er2!", "across the tail's start");
        fs::write(&path, &text).unwrap();
        let read = read_capped(&path).unwrap();
        let out = redact(&read, &p);
        for part in ["hunt", "er2!"] {
            assert!(!out.contains(part), "{part}");
        }
        assert!(out.contains("[... "), "cut");
        let cut: usize = out.split("[... ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        assert_eq!(
            read.len() - read.lines().find(|l| l.starts_with("[... ")).unwrap().len() - 1 + cut,
            text.len(),
            "it says how much went"
        );
    }

    #[test]
    fn a_run_says_where_it_was_played() {
        let game = tempdir();
        begin(&game, "https://Play.Example.org/", " Kozak ").unwrap();
        let log = fs::read_to_string(game.join(logging::FILE_NAME)).unwrap();
        assert!(first_line_time(&game.join(logging::FILE_NAME)).is_some(), "it starts with the time: {log}");
        let played = played(&log).unwrap();
        assert_eq!(
            played,
            Played {
                server: "play.example.org".into(),
                account: "Kozak".into()
            }
        );
        assert!(played.is("play.example.org", "kozak"));
        assert!(played.is("http://PLAY.example.org", "Kozak"));
        assert!(!played.is("other.example.org", "Kozak"));
        assert!(!played.is("play.example.org", "Someone"));
        assert_eq!(super::played("1700000000.000 INFO  orbit Cross-Com 0.1.0 loaded\n"), None, "a client from before");
    }

    #[test]
    fn caps_long_logs_keeping_the_start_and_the_end() {
        let short = "a line\n".repeat(10);
        assert_eq!(cap(short.clone()), short);
        let line = "0123456789abcdefghijklmnopqrstuvwxyz é\n";
        let long = format!("FIRST\n{}LAST\n", line.repeat(3 * MAX_UPLOAD / line.len()));
        let out = cap(long.clone());
        assert!(out.len() <= MAX_UPLOAD, "{}", out.len());
        assert!(out.starts_with("FIRST\n") && out.ends_with("LAST\n"));
        let marker = out.lines().find(|l| l.starts_with("[... ")).unwrap();
        let cut: usize = marker.trim_start_matches("[... ").split(' ').next().unwrap().parse().unwrap();
        assert_eq!(out.len() - marker.len() - 1 + cut, long.len(), "it says how much went");
        let head = out.find("[... ").unwrap();
        assert!(head <= HEAD_KEPT && head > HEAD_KEPT - line.len(), "the first 256 KiB, to a line end");
        assert!(out[..head].ends_with('\n'));
    }

    #[test]
    fn reads_a_huge_log_only_at_its_ends() {
        let dir = tempdir();
        let path = dir.join("big.log");
        let line = "x".repeat(99) + "\n";
        fs::write(&path, format!("START\n{}END\n", line.repeat(5 * MAX_UPLOAD / 100))).unwrap();
        let text = read_capped(&path).unwrap();
        assert!(text.len() < MAX_UPLOAD + HEAD_KEPT + 100);
        assert!(text.starts_with("START\n") && text.ends_with("END\n"));
        assert!(cap(text).len() <= MAX_UPLOAD);
    }

    fn tempdir() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("crosscom-runs-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_times() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_794_746_096), "20261115-123456");
        assert_eq!(utc(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(run_time("crosscom-20261115-123456.log").as_deref(), Some("2026-11-15 12:34:56 UTC"));
        assert_eq!(run_time("crosscom-20261115-123456-2.sent.log").as_deref(), Some("2026-11-15 12:34:56 UTC"));
        assert_eq!(run_time("crosscom-x.log"), None);
        assert_eq!(run_time("other.log"), None);
    }

    #[test]
    fn each_run_gets_its_own_file() {
        let game = tempdir();
        assert_eq!(rotate(&game).unwrap(), None, "no log yet");
        fs::write(game.join(logging::FILE_NAME), "").unwrap();
        assert_eq!(rotate(&game).unwrap(), None, "an empty log stays");
        fs::write(game.join(logging::FILE_NAME), "1794746096.123 INFO  orbit Cross-Com 0.1.0 loaded\n").unwrap();
        let moved = rotate(&game).unwrap().unwrap();
        assert_eq!(moved, game.join("logs/crosscom-20261115-123456.log"));
        assert!(!game.join(logging::FILE_NAME).exists());
        // Another run that started in the same second.
        fs::write(game.join(logging::FILE_NAME), "1794746096.900 INFO  orbit again\n").unwrap();
        assert_eq!(rotate(&game).unwrap().unwrap(), game.join("logs/crosscom-20261115-123456-2.log"));

        for i in 0..MAX_KEPT + 3 {
            fs::write(game.join(logging::FILE_NAME), format!("{} INFO  orbit run {i}\n", 1_800_000_000 + i * 60)).unwrap();
            rotate(&game).unwrap();
        }
        fs::write(game.join("logs/notes.txt"), "the player's own").unwrap();
        prune(&game);
        let kept = runs(&game);
        assert_eq!(kept.len(), MAX_KEPT);
        assert!(kept[0].name > kept[1].name, "newest first");
        assert!(game.join("logs/notes.txt").exists(), "only runs are deleted");
        assert!(!kept.iter().any(|r| r.name.starts_with("crosscom-20261115")), "the oldest went");
    }

    #[test]
    fn sends_pending_runs_once() {
        let game = tempdir();
        let run = |time: u64, server: &str, account: &str, rest: &str| {
            let played = serde_json::json!({ "server": server, "account": account });
            fs::write(game.join(logging::FILE_NAME), format!("{time}.000 INFO  orbit Run: {played}\n{rest}")).unwrap();
            rotate(&game).unwrap().unwrap()
        };
        // Played elsewhere, as someone else, or by a client from before runs said where: not sent.
        let elsewhere = run(1_799_999_000, "other.example.org", "Kozak", "hunter2!\n");
        let someone_else = run(1_799_999_100, "play.example.org", "Someone", "hunter2!\n");
        fs::write(game.join(logging::FILE_NAME), "1799999200 INFO  orbit Cross-Com 0.1.0 loaded\nhunter2!\n").unwrap();
        let unknown = rotate(&game).unwrap().unwrap();
        for i in 0..3 {
            run(
                1_800_000_000 + i * 60,
                "play.example.org",
                "kozak",
                &format!(
                    "{} INFO  orbit Cross-Com 0.2.0 loaded in Future Soldier DX9.exe from C:\\Users\\jsmith\\GRFS\nhunter2! {i}\n",
                    1_800_000_000 + i * 60
                ),
            );
        }
        let meta = Meta {
            account: "Kozak".into(),
            client: "0.3.0".into(),
            launcher: Some("0.3.0".into()),
            os: Some("Windows 11 Pro\r\nX-Evil: 1".into()),
            exit: Some((runs(&game)[0].name.clone(), -1_073_741_819)),
        };
        let mut seen = Vec::new();
        let (sent, refused) = send_pending(&game, "https://play.example.org/", &private(), &meta, |body, headers| {
            seen.push((String::from_utf8(body.to_vec()).unwrap(), headers.to_vec()));
            Ok((201, format!(r#"{{"id":"20261115-123456-ab12c{}"}}"#, seen.len()).into_bytes()))
        });
        assert_eq!((sent.len(), refused), (3, None));
        assert_eq!(sent[0].id, "20261115-123456-ab12c1");
        let (body, headers) = &seen[0];
        assert!(body.contains("C:\\Users\\<user>\\GRFS") && body.contains("<password> 2"), "{body}");
        let header = |name: &str| headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str());
        assert_eq!(header("X-Crosscom-Exe"), Some("DX9"));
        assert_eq!(header("X-Crosscom-Client"), Some("0.2.0"), "the version that wrote it");
        assert_eq!(header("X-Crosscom-Account"), Some("Kozak"));
        assert_eq!(header("X-Crosscom-Os"), Some("Windows 11 ProX-Evil: 1"), "on one line");
        assert_eq!(header("X-Crosscom-Exit"), Some("-1073741819"));
        assert_eq!(seen[1].1.iter().find(|(n, _)| *n == "X-Crosscom-Exit"), None, "only for its own run");
        assert!(pending_for(&game, "play.example.org", "Kozak").is_empty());
        let still: Vec<PathBuf> = pending(&game).into_iter().map(|r| r.path).collect();
        assert_eq!(still, [unknown, someone_else, elsewhere], "only the runs played there, as this account");

        // Refused: stays pending.
        run(1_800_009_999, "play.example.org", "Kozak", "x\n");
        let (sent, refused) = send_pending(&game, "play.example.org", &private(), &meta, |_, _| {
            Ok((
                403,
                br#"{"error":"sign in to the game first; logs are taken from players who have played from this address recently"}"#.to_vec(),
            ))
        });
        assert_eq!((sent.len(), refused), (0, Some(Refused::NotSignedIn)));
        assert_eq!(pending_for(&game, "play.example.org", "Kozak").len(), 1);
    }

    #[test]
    fn reads_the_servers_answers() {
        assert_eq!(report_id(201, br#"{"id":"20261115-123456-ab12cd"}"#), Ok("20261115-123456-ab12cd".into()));
        assert_eq!(report_id(403, br#"{"error":"this server doesn't take logs"}"#), Err(Refused::Off));
        assert_eq!(report_id(404, b""), Err(Refused::Off));
        assert!(matches!(report_id(429, b""), Err(Refused::Other(_))));
        assert!(matches!(report_id(201, b"{}"), Err(Refused::Other(_))));
    }
}
