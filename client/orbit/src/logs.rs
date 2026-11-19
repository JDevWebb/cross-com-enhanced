//! Sends the logs of earlier runs to the server, for a game started without the launcher (the
//! launcher sends them itself as a game it started ends). See `crosscom_client_common::runs`.
//!
//! The server only takes logs from an address the account has played from lately, so they go
//! once this game has signed in: a while after it asks for its sign-in details, on a thread of
//! its own, over plain HTTP (this library has no TLS). Nothing waits on it. A run the server
//! doesn't take stays pending for the next start; a refusal for not being signed in is quiet.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use crosscom_client_common::runs;
use crosscom_client_common::runs::Refused;
use crosscom_client_common::server_info;
use crosscom_client_common::system;
use crosscom_client_common::Config;
use log::info;
use log::warn;

/// How long after the game asks for its sign-in details the logs go: by then it has signed in.
const AFTER_SIGN_IN: Duration = Duration::from_secs(30);
/// How long one upload may take, at each step.
const TIMEOUT: Duration = Duration::from_secs(30);

static SIGNED_IN: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();

/// The game asked for its sign-in details.
pub(crate) fn signing_in() {
    if let Some(tx) = SIGNED_IN.get().and_then(|t| t.lock().ok()) {
        let _ = tx.send(());
    }
}

/// Keeps the newest runs and, unless `send_logs = false`, starts the thread that sends the
/// pending ones once the game signs in.
pub(crate) fn start(dir: PathBuf, config: &Config) {
    let server = config.server.clone().filter(|s| !s.trim().is_empty());
    let send = config.send_logs && server.is_some() && !config.username.is_empty();
    if !config.send_logs {
        info!("Logs aren't sent (send_logs = false)");
    }
    let (tx, rx) = mpsc::channel();
    if send {
        let _ = SIGNED_IN.set(Mutex::new(tx));
    }
    let private = runs::Private::of_this_pc(&config.username, &config.password);
    let meta = runs::Meta {
        account: config.username.clone(),
        client: env!("CROSSCOM_RELEASE").into(),
        launcher: None,
        os: system::os_version(),
        exit: None,
    };
    let spawned = std::thread::Builder::new().name("crosscom-logs".into()).spawn(move || {
        runs::prune(&dir);
        let Some(server) = server.filter(|_| send) else {
            return;
        };
        if runs::pending(&dir).is_empty() || rx.recv().is_err() {
            return;
        }
        std::thread::sleep(AFTER_SIGN_IN);
        let (sent, refused) = runs::send_pending(&dir, &private, &meta, |body, headers| {
            server_info::post(&server, runs::PATH, headers, body, TIMEOUT).map_err(|e| e.0)
        });
        for run in sent {
            let when = runs::run_time(&run.run).unwrap_or(run.run);
            info!("Logs of the run of {when} sent: report {}", run.id);
        }
        match refused {
            None => {}
            Some(Refused::NotSignedIn) => info!("Logs not sent: the server takes them once this game has signed in; they'll go next time"),
            Some(why) => warn!("Logs not sent ({why}); they'll go next time"),
        }
    });
    if let Err(e) = spawned {
        warn!("Couldn't start sending the logs: {e}");
    }
}
