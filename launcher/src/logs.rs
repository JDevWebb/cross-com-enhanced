//! Sending the game's logs to the server (see `crosscom_client_common::runs`): the run that
//! just ended once a game the launcher started exits, and any others still pending, also as the
//! launcher starts. Unless `send_logs = false` in `crosscom.toml`.
//!
//! The server only takes them from an address the account has played from lately, so a player
//! who hasn't played online from this PC yet has them wait (`Refused::NotSignedIn`).

use std::path::Path;
use std::time::Duration;

use crosscom_client_common::logging;
use crosscom_client_common::runs;
use crosscom_client_common::runs::Refused;
use crosscom_client_common::runs::Sent;
use crosscom_client_common::server_info;
use crosscom_client_common::system;

use crate::server;
use crate::settings;

/// How long one upload may take in all.
const TIMEOUT: Duration = Duration::from_secs(60);

/// What came of sending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The runs the server took, newest first.
    pub sent: Vec<Sent>,
    /// Why the rest weren't, if they weren't.
    pub refused: Option<Refused>,
    /// Nothing was tried: `send_logs = false`, or no server or account set up.
    pub off: bool,
}

/// Whether the game has `path` open: Windows won't let it be opened without sharing then.
#[cfg(windows)]
fn in_use(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    match std::fs::OpenOptions::new().read(true).share_mode(0).open(path) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(ERROR_SHARING_VIOLATION),
    }
}

#[cfg(not(windows))]
fn in_use(_path: &Path) -> bool {
    false
}

/// After the game the launcher started has exited, waits for the game itself to end: the exe
/// the launcher starts starts the game again through `Future Soldier.exe` a moment later and
/// exits, so the game is running for as long as something has `crosscom.log` open. Whether the
/// game was still running (the exit code the launcher saw is then the first exe's, not the
/// game's).
pub fn wait_for_game(game_dir: &Path) -> bool {
    const SETTLE: std::time::Duration = std::time::Duration::from_secs(5);
    const EVERY: std::time::Duration = std::time::Duration::from_secs(3);
    const AT_MOST: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
    let log = game_dir.join(logging::FILE_NAME);
    // The second exe opens the log a moment after the first one exits.
    std::thread::sleep(SETTLE);
    let started = std::time::Instant::now();
    let mut waited = false;
    while in_use(&log) && started.elapsed() < AT_MOST {
        waited = true;
        std::thread::sleep(EVERY);
    }
    waited
}

/// Moves the last run's `crosscom.log` in `game_dir` aside (see `runs::rotate`) unless a game
/// still has it open; the run's file name.
#[must_use]
pub fn finish_run(game_dir: &Path) -> Option<String> {
    if in_use(&game_dir.join(logging::FILE_NAME)) {
        return None;
    }
    let moved = runs::rotate(game_dir).ok()??;
    runs::prune(game_dir);
    moved.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Sends the pending runs in `game_dir` that were played on its server, as its account, to that
/// server; `exit` is the game's exit code for the run it belongs to. `launcher` is this
/// launcher's version.
#[must_use]
pub fn send(game_dir: &Path, launcher: &str, exit: Option<(String, i32)>) -> Outcome {
    let config = settings::client(game_dir);
    let Some(host) = config.server.as_deref().map(server::host).filter(|h| !h.is_empty()) else {
        return Outcome { off: true, ..Outcome::default() };
    };
    if !config.send_logs || config.username.is_empty() {
        return Outcome { off: true, ..Outcome::default() };
    }
    let private = runs::Private::of_this_pc(&config.username, &config.password);
    let meta = runs::Meta {
        account: config.username.clone(),
        client: launcher.to_owned(),
        launcher: Some(launcher.to_owned()),
        os: system::os_version(),
        exit,
    };
    // Only the runs played on this server, as this account: the password hidden is theirs.
    let (sent, refused) = runs::send_pending(game_dir, &host, &private, &meta, |body, headers| post(&host, headers, body));
    Outcome { sent, refused, off: false }
}

/// Posts a log to `host`: over HTTPS, or plain HTTP to a server set up by a private address
/// without HTTPS (`crosscom_client_common::http`).
fn post(host: &str, headers: &[(&'static str, String)], body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    server_info::post(host, runs::PATH, headers, body, TIMEOUT).map_err(|e| e.0)
}

/// What the launcher says about an outcome.
#[must_use]
pub fn said(outcome: &Outcome) -> String {
    if outcome.off {
        return "Logs aren't sent: turned off, or no server and account set up.".into();
    }
    let sent = match outcome.sent.len() {
        0 => String::new(),
        1 => format!("Logs sent: report {}.", outcome.sent[0].id),
        n => format!("Logs of {n} games sent: report {} the latest.", outcome.sent[0].id),
    };
    let rest = match &outcome.refused {
        None if outcome.sent.is_empty() => "No logs waiting to be sent.".to_owned(),
        None => String::new(),
        Some(Refused::NotSignedIn) => "Logs wait until you've played online from this PC.".to_owned(),
        Some(why) => format!("Logs not sent: {why}. They'll go later."),
    };
    [sent, rest].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_happened() {
        let sent = |id: &str| Sent {
            run: "crosscom-20261115-123456.log".into(),
            id: id.into(),
        };
        let outcome = Outcome {
            sent: vec![sent("20261115-123456-ab12cd")],
            ..Outcome::default()
        };
        assert_eq!(said(&outcome), "Logs sent: report 20261115-123456-ab12cd.");
        let waiting = Outcome {
            refused: Some(Refused::NotSignedIn),
            ..Outcome::default()
        };
        assert_eq!(said(&waiting), "Logs wait until you've played online from this PC.");
        assert_eq!(said(&Outcome::default()), "No logs waiting to be sent.");
    }

    #[test]
    fn finishes_the_run_once() {
        let game = tempfile::tempdir().unwrap();
        assert_eq!(finish_run(game.path()), None);
        std::fs::write(game.path().join(logging::FILE_NAME), "1794746096.000 INFO  orbit Cross-Com 0.1.0 loaded\n").unwrap();
        assert_eq!(finish_run(game.path()).as_deref(), Some("crosscom-20261115-123456.log"));
        assert_eq!(finish_run(game.path()), None);
    }

    #[test]
    fn nothing_is_sent_when_turned_off() {
        let game = tempfile::tempdir().unwrap();
        assert!(send(game.path(), "0.1.0", None).off, "no settings");
        std::fs::write(game.path().join("crosscom.toml"), "server = \"192.0.2.1\"\nusername = \"Kozak\"\nsend_logs = false\n").unwrap();
        assert!(send(game.path(), "0.1.0", None).off);
    }
}
