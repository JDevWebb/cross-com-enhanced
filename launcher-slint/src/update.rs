//! The launcher keeping itself up to date (`crosscom_launcher::update`): a newer release looked
//! for at start and every 4 hours (not in a development build), offered in the bar under the
//! title, and installed when the player says: the launcher replaces itself and restarts, the new
//! one bringing the client to match. Never while the game runs, so the launcher is there to send
//! its log when it ends. About's Check for updates looks at once.

use std::sync::Mutex;
use std::time::Duration;

use crosscom_launcher::update;
use slint::ComponentHandle;

use crate::core;
use crate::AppWindow;

/// How often a newer release is looked for.
const RECHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Default)]
struct State {
    latest: Option<update::Latest>,
    /// The version the player said Later to: the bar comes back for a newer one.
    later: Option<String>,
    updating: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    latest: None,
    later: None,
    updating: false,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// At start: the launcher an update left behind goes, and a newer release is looked for now and
/// every few hours.
pub fn start(w: &AppWindow) {
    std::thread::spawn(update::clean_up);
    if !update::enabled(core::RELEASE) {
        return;
    }
    let weak = w.as_weak();
    std::thread::spawn(move || loop {
        // Not answering is tried again next time; About's Check for updates says why.
        if let Ok(latest) = update::latest() {
            state().latest = Some(latest);
            if weak.upgrade_in_event_loop(|w| show(&w)).is_err() {
                return;
            }
        }
        std::thread::sleep(RECHECK_EVERY);
    });
}

/// Shows the bar for a newer release, unless the player said Later to it.
fn show(w: &AppWindow) {
    let st = state();
    let offer = st
        .latest
        .as_ref()
        .filter(|l| l.newer_than(core::RELEASE) && st.later.as_deref() != Some(l.version.as_str()));
    match offer {
        Some(l) => {
            w.set_update_version(l.version.as_str().into());
            if !st.updating && w.get_update_state() != "failed" {
                w.set_update_state("offer".into());
            }
        }
        None => w.set_update_version("".into()),
    }
}

/// The bar's Update: installs the newer release and restarts into it.
pub fn now(w: &AppWindow) {
    if w.get_playing() {
        return;
    }
    let latest = {
        let mut st = state();
        if st.updating {
            return;
        }
        let Some(latest) = st.latest.clone().filter(|l| l.newer_than(core::RELEASE)) else {
            return;
        };
        st.updating = true;
        latest
    };
    w.set_update_state("updating".into());
    let weak = w.as_weak();
    std::thread::spawn(move || {
        let result = update::update_self(&latest, core::RELEASE);
        state().updating = false;
        match result {
            // The new launcher has started: this one goes.
            Ok(()) => std::process::exit(0),
            Err(e) => {
                let _ = weak.upgrade_in_event_loop(move |w| {
                    w.set_update_state("failed".into());
                    w.set_update_said(e.into());
                });
            }
        }
    });
}

/// The bar's What's new: the release's page.
pub fn notes() {
    let page = state().latest.as_ref().map_or_else(|| update::RELEASES_PAGE.to_owned(), |l| l.page.clone());
    let _ = core::open_url(&page);
}

/// The bar's Later: hidden until a newer release than this one.
pub fn later(w: &AppWindow) {
    {
        let mut st = state();
        st.later = st.latest.as_ref().map(|l| l.version.clone());
    }
    w.set_update_state("offer".into());
    show(w);
}

/// About's Check for updates.
pub fn check(w: &AppWindow) {
    if let Some(why) = update::disabled_why(core::RELEASE) {
        w.set_update_checked(why.into());
        return;
    }
    w.set_update_checking(true);
    let weak = w.as_weak();
    std::thread::spawn(move || {
        let found = update::latest();
        let said = match &found {
            Ok(l) if l.newer_than(core::RELEASE) => format!("Version {} is available: Update, in the bar above.", l.version),
            Ok(l) => format!("Up to date: {} is the latest release.", l.version),
            Err(e) => format!("Couldn't look for a newer launcher: {e}"),
        };
        if let Ok(latest) = found {
            let mut st = state();
            // Asked for: shown even if Later was said to it.
            st.later = None;
            st.latest = Some(latest);
        }
        let _ = weak.upgrade_in_event_loop(move |w| {
            w.set_update_checking(false);
            w.set_update_checked(said.into());
            show(&w);
        });
    });
}
