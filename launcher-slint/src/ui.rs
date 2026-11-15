//! The window (ui/app.slint) and its callbacks: each screen filled in from the launcher's state
//! (core.rs) when it opens, and the work it asks for done on a thread of its own, the window
//! updated when it's done.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crosscom_launcher::server;
use crosscom_launcher::settings::OVERLAY_KEYS;
use slint::ComponentHandle;
use slint::ModelRc;
use slint::SharedString;
use slint::VecModel;

use crate::core;
use crate::core::ClientState;
use crate::core::Launcher;
use crate::AppWindow;
use crate::InfoView;
use crate::NewsRow;

/// How long a note stays in the corner.
const TOAST: Duration = Duration::from_secs(5);

thread_local! {
    /// The server Setup last checked, so an answer for one since changed is let go.
    static CHECKED: RefCell<String> = const { RefCell::new(String::new()) };
}

fn s(text: impl AsRef<str>) -> SharedString {
    SharedString::from(text.as_ref())
}

/// Runs `job` on a thread of its own, then `done` with what it gave on the window's thread.
fn background<T: Send + 'static>(w: &AppWindow, job: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&AppWindow, T) + Send + 'static) {
    let weak = w.as_weak();
    std::thread::spawn(move || {
        let out = job();
        let _ = weak.upgrade_in_event_loop(move |w| done(&w, out));
    });
}

/// The launcher's state as the screens show it.
pub fn info_view(info: &core::Info) -> InfoView {
    InfoView {
        game_dir: s(info.game_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default()),
        client: s(match info.client {
            ClientState::NoGame => "none",
            ClientState::No => "no",
            ClientState::Yes => "yes",
            ClientState::Update => "update",
        }),
        carries_client: info.carries_client,
        server: s(info.server.as_deref().unwrap_or_default()),
        community: info.server.as_deref().is_some_and(server::is_community),
        account: s(info.account.as_deref().unwrap_or_default()),
        skip_intro: info.skip_intro,
        dx9: info.dx9,
        overlay: info.overlay,
        overlay_key: key_index(&info.overlay_key),
        send_logs: info.send_logs,
        ready: info.ready(),
    }
}

fn key_index(key: &str) -> i32 {
    OVERLAY_KEYS.iter().position(|k| *k == key).and_then(|i| i32::try_from(i).ok()).unwrap_or(4)
}

/// A news item as Play lists it.
pub fn news_row(item: &server::News) -> NewsRow {
    let tag = match item.shows.as_deref() {
        Some("multiplayer") => "Multiplayer",
        Some("campaign") => "Campaign",
        Some("guerrilla") => "Guerrilla",
        Some(other) => other,
        None => "",
    };
    NewsRow {
        date: s(&item.date),
        title: s(&item.title),
        body: s(&item.body),
        tag: s(tag),
    }
}

/// The server card's line once the server answered: its players and version.
pub fn server_detail(info: Option<&server::Info>) -> String {
    match info {
        None => "An older server: no player count or news.".into(),
        Some(i) => {
            let players = if i.players_online == 1 {
                "1 player".to_owned()
            } else {
                format!("{} players", i.players_online)
            };
            format!("{players} online · server {}", i.version)
        }
    }
}

/// The last logs sent, as Settings says it.
pub fn logs_last(status: &core::LogsStatus) -> String {
    match &status.last {
        Some((id, at)) => format!("Last logs sent: {id} at {}", when(*at)),
        None => "No logs sent yet.".into(),
    }
}

/// A Unix time as `YYYY-MM-DD HH:MM UTC`.
pub fn when(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", rest / 3600, rest % 3600 / 60)
}

/// Shows the launcher's state everywhere; what it is.
fn refresh_info(w: &AppWindow, l: &Launcher) -> core::Info {
    let info = l.info();
    w.set_info(info_view(&info));
    info
}

fn toast(w: &AppWindow, said: &str) {
    w.set_toast(s(said));
    let weak = w.as_weak();
    let shown = said.to_owned();
    slint::Timer::single_shot(TOAST, move || {
        if let Some(w) = weak.upgrade() {
            // Not a later one.
            if w.get_toast() == shown.as_str() {
                w.set_toast(s(""));
            }
        }
    });
}

/// Opens a screen ("play", "setup" or "settings"); Play is the setup until everything's set.
fn open(w: &AppWindow, l: &Arc<Launcher>, screen: &str) {
    w.set_screen(s(screen));
    let info = refresh_info(w, l);
    match screen {
        "play" if info.ready() => load_news(w, l),
        "play" | "setup" => open_setup(w, &info, None),
        "settings" => open_settings(w, l, &info),
        _ => {}
    }
}

/// Play: asks the server how it is, and for its news.
fn load_news(w: &AppWindow, l: &Arc<Launcher>) {
    if w.get_news_loading() {
        return;
    }
    w.set_news_loading(true);
    if w.get_server_state() != "up" {
        w.set_server_state(s("asking"));
        w.set_server_detail(s("Asking the server…"));
        w.set_news_note(s("Asking the server…"));
    }
    let l = l.clone();
    background(
        w,
        move || l.server_info(),
        |w, answer| {
            w.set_news_loading(false);
            let (state, detail, news, note) = match answer {
                Ok(Some(info)) => ("up", server_detail(Some(&info)), info.news.iter().map(news_row).collect(), "No news from the server."),
                Ok(None) => ("up", server_detail(None), Vec::new(), "This server doesn't share its news with the launcher."),
                Err(e) => ("down", e, Vec::new(), "The server didn't answer, so there's no news."),
            };
            w.set_server_state(s(state));
            w.set_server_detail(s(detail));
            w.set_news(ModelRc::new(VecModel::from(news)));
            w.set_news_note(s(note));
        },
    );
}

/// The server Setup has chosen: the community test network, or the one typed.
fn setup_host(w: &AppWindow) -> String {
    if w.get_setup_community() {
        server::COMMUNITY.into()
    } else {
        server::host(&w.get_setup_server_typed())
    }
}

/// Setup, filled in from what's set up now. `community`: the server choice to start from
/// (Settings' Change server and its way back to the community network); otherwise the community
/// test network unless another server is set up.
fn open_setup(w: &AppWindow, info: &core::Info, community: Option<bool>) {
    let first = !info.ready();
    let set_up = info.server.as_deref().filter(|h| !server::is_community(h));
    let community = community.unwrap_or(set_up.is_none());
    w.set_setup_game_dir(s(info.game_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default()));
    w.set_setup_game_error(s(""));
    w.set_setup_community(community);
    w.set_setup_server_typed(s(set_up.unwrap_or_default()));
    w.set_setup_server_ok(s(""));
    w.set_setup_server_error(s(""));
    w.set_setup_new_account(info.account.is_none());
    w.set_setup_name(s(info.account.as_deref().unwrap_or_default()));
    w.set_setup_password(s(""));
    w.set_setup_again(s(""));
    w.set_setup_code(s(""));
    w.set_setup_skip_intro(first || info.skip_intro);
    w.set_setup_dx9(info.dx9);
    w.set_setup_busy(false);
    w.set_setup_error(s(""));
    CHECKED.with(|c| c.borrow_mut().clear());
    w.set_setup_server_checking(false);
    setup_changed(w);
    if info.game_dir.is_none() && !w.get_setup_game_finding() {
        w.set_setup_game_finding(true);
        background(w, core::find_game, |w, found| {
            w.set_setup_game_finding(false);
            if let Some(dir) = found {
                if w.get_setup_game_dir().is_empty() {
                    w.set_setup_game_dir(s(dir.display().to_string()));
                }
            }
            setup_changed(w);
        });
    }
    if !setup_host(w).is_empty() {
        check_server(w);
    }
}

/// What Setup still needs, after any change to it.
fn setup_changed(w: &AppWindow) {
    let host = setup_host(w);
    // A server changed since it was checked: checked again before it's trusted.
    if CHECKED.with(|c| *c.borrow() != host) {
        w.set_setup_server_ok(s(""));
        w.set_setup_server_error(s(""));
    }
    let account = core::account_missing(&w.get_setup_name(), &w.get_setup_password(), &w.get_setup_again(), w.get_setup_new_account());
    w.set_setup_account_done(account.is_none());
    let missing = if w.get_setup_game_dir().is_empty() {
        "Choose the game's folder."
    } else if host.is_empty() {
        "Enter the server's address."
    } else {
        account.unwrap_or("")
    };
    w.set_setup_missing(s(missing));
}

/// Checks the server Setup has chosen: where the game will sign in, or why not.
fn check_server(w: &AppWindow) {
    let host = setup_host(w);
    if host.is_empty() {
        return;
    }
    CHECKED.with(|c| *c.borrow_mut() = host.clone());
    w.set_setup_server_checking(true);
    w.set_setup_server_ok(s(""));
    w.set_setup_server_error(s(""));
    let checking = host.clone();
    background(
        w,
        move || server::check(&checking),
        move |w, answer| {
            // A later check, or a server since changed: this answer is let go.
            if CHECKED.with(|c| *c.borrow() != host) {
                return;
            }
            w.set_setup_server_checking(false);
            if setup_host(w) != host {
                return;
            }
            match answer {
                Ok(at) => w.set_setup_server_ok(s(at)),
                Err(e) => w.set_setup_server_error(s(e)),
            }
        },
    );
}

/// Asks the player for the game's folder (Windows' own picker).
fn choose_game(w: &AppWindow) {
    #[cfg(windows)]
    background(
        w,
        || rfd::FileDialog::new().set_title("The folder with \"Future Soldier DX11.exe\"").pick_folder(),
        |w, dir: Option<PathBuf>| {
            let Some(dir) = dir else { return };
            match core::check_game(&dir) {
                Ok(()) => {
                    w.set_setup_game_dir(s(dir.display().to_string()));
                    w.set_setup_game_error(s(""));
                }
                Err(e) => w.set_setup_game_error(s(e)),
            }
            setup_changed(w);
        },
    );
    #[cfg(not(windows))]
    w.set_setup_game_error(s("Choosing a folder needs Windows."));
}

fn run_setup(w: &AppWindow, l: &Arc<Launcher>) {
    let setup = core::Setup {
        game_dir: PathBuf::from(w.get_setup_game_dir().as_str()),
        server: setup_host(w),
        name: w.get_setup_name().into(),
        password: w.get_setup_password().into(),
        new_account: w.get_setup_new_account(),
        code: w.get_setup_code().into(),
        skip_intro: w.get_setup_skip_intro(),
        dx9: w.get_setup_dx9(),
    };
    w.set_setup_busy(true);
    w.set_setup_error(s(""));
    let worker = l.clone();
    let l = l.clone();
    background(
        w,
        move || worker.setup(setup),
        move |w, done| {
            w.set_setup_busy(false);
            match done {
                Ok(said) => {
                    w.set_setup_password(s(""));
                    w.set_setup_again(s(""));
                    // The server may be another: its card asks afresh.
                    w.set_server_state(s("asking"));
                    w.set_news(ModelRc::default());
                    open(w, &l, "play");
                    toast(w, &said);
                }
                Err(e) => w.set_setup_error(s(e)),
            }
        },
    );
}

/// Settings, filled in from what's set up now.
fn open_settings(w: &AppWindow, l: &Arc<Launcher>, info: &core::Info) {
    let view = info_view(info);
    w.set_opt_skip_intro(view.skip_intro);
    w.set_opt_dx9(view.dx9);
    w.set_opt_overlay(view.overlay);
    w.set_opt_overlay_key(view.overlay_key);
    w.set_opt_send_logs(view.send_logs);
    w.set_options_said(s(""));
    w.set_acc_new(false);
    w.set_acc_name(s(""));
    w.set_acc_password(s(""));
    w.set_acc_again(s(""));
    w.set_acc_code(s(""));
    w.set_acc_ready(false);
    w.set_acc_said(s(""));
    w.set_restore_confirming(false);
    w.set_restore_said(s(""));
    show_logs(w, l);
}

/// How the logs stand, in Settings.
fn show_logs(w: &AppWindow, l: &Launcher) {
    let status = l.logs_status();
    w.set_logs_last(s(logs_last(&status)));
    if let Some(said) = &status.said {
        w.set_logs_said(s(said));
    }
    w.set_logs_sending(status.sending);
}

/// Sends the logs waiting to go (as the launcher starts, after a game, or when asked).
fn send_logs(w: &AppWindow, l: &Arc<Launcher>, exit: Option<i32>) {
    w.set_logs_sending(true);
    let worker = l.clone();
    let l = l.clone();
    background(
        w,
        move || worker.send_logs(exit),
        move |w, said| {
            w.set_logs_said(s(said));
            show_logs(w, &l);
        },
    );
}

fn save_options(w: &AppWindow, l: &Arc<Launcher>) {
    let key = usize::try_from(w.get_opt_overlay_key()).ok().and_then(|i| OVERLAY_KEYS.get(i)).copied().unwrap_or("F5");
    let options = core::Options {
        skip_intro: w.get_opt_skip_intro(),
        dx9: w.get_opt_dx9(),
        overlay: w.get_opt_overlay(),
        overlay_key: key.into(),
        send_logs: w.get_opt_send_logs(),
    };
    match l.set_options(options) {
        Ok(()) => {
            w.set_options_said(s("Saved."));
            w.set_options_failed(false);
        }
        Err(e) => {
            w.set_options_said(s(e));
            w.set_options_failed(true);
        }
    }
    refresh_info(w, l);
}

fn account_changed(w: &AppWindow) {
    let ready = core::account_missing(&w.get_acc_name(), &w.get_acc_password(), &w.get_acc_again(), w.get_acc_new()).is_none();
    w.set_acc_ready(ready);
}

fn run_account(w: &AppWindow, l: &Arc<Launcher>) {
    let account = core::Account {
        name: w.get_acc_name().into(),
        password: w.get_acc_password().into(),
        new_account: w.get_acc_new(),
        code: w.get_acc_code().into(),
    };
    w.set_acc_busy(true);
    w.set_acc_said(s(""));
    let worker = l.clone();
    let l = l.clone();
    background(
        w,
        move || worker.set_account(account),
        move |w, done| {
            w.set_acc_busy(false);
            match done {
                Ok(said) => {
                    w.set_acc_said(s(said));
                    w.set_acc_failed(false);
                    w.set_acc_password(s(""));
                    w.set_acc_again(s(""));
                    account_changed(w);
                }
                Err(e) => {
                    w.set_acc_said(s(e));
                    w.set_acc_failed(true);
                }
            }
            refresh_info(w, &l);
        },
    );
}

/// Starts the game; once it ends, its logs go to the server.
fn play(w: &AppWindow, l: &Arc<Launcher>) {
    if w.get_starting() {
        return;
    }
    w.set_starting(true);
    w.set_play_said(s(""));
    w.set_play_failed(false);
    let worker = l.clone();
    let after = l.clone();
    let weak = w.as_weak();
    let l = l.clone();
    background(
        w,
        move || {
            let (mut child, said) = worker.play()?;
            // The game's own thread: it waits for the game, then sends its logs.
            std::thread::spawn(move || {
                let exit = child.wait().ok().and_then(|status| status.code());
                let _ = weak.upgrade_in_event_loop(move |w| send_logs(&w, &after, exit));
            });
            Ok::<_, String>(said)
        },
        move |w, started| {
            w.set_starting(false);
            match started {
                Ok(said) => w.set_play_said(s(said)),
                Err(e) => {
                    w.set_play_said(s(e));
                    w.set_play_failed(true);
                }
            }
            refresh_info(w, &l);
        },
    );
}

/// The window's callbacks.
fn wire(w: &AppWindow, l: &Arc<Launcher>) {
    use slint::winit_030::WinitWindowAccessor as _;
    let weak = w.as_weak();
    // Each callback gets the window and the launcher.
    let on = |f: fn(&AppWindow, &Arc<Launcher>)| {
        let (weak, l) = (weak.clone(), l.clone());
        move || {
            if let Some(w) = weak.upgrade() {
                f(&w, &l);
            }
        }
    };

    w.on_drag(on(|w, _| {
        w.window().with_winit_window(|win| {
            let _ = win.drag_window();
        });
    }));
    w.on_minimise(on(|w, _| w.window().set_minimized(true)));
    w.on_maximise(on(|w, _| {
        let to = !w.window().is_maximized();
        w.window().set_maximized(to);
        w.set_maximised(to);
    }));
    w.on_close_window(|| {
        let _ = slint::quit_event_loop();
    });
    {
        let (weak, l) = (weak.clone(), l.clone());
        w.on_go(move |screen| {
            if let Some(w) = weak.upgrade() {
                open(&w, &l, &screen);
            }
        });
    }
    w.on_play(on(play));
    w.on_refresh_news(on(load_news));
    w.on_setup_choose_game(on(|w, _| choose_game(w)));
    w.on_setup_check_server(on(|w, _| check_server(w)));
    w.on_setup_changed(on(|w, _| setup_changed(w)));
    w.on_setup_run(on(run_setup));
    w.on_options_changed(on(save_options));
    w.on_account_changed(on(|w, _| account_changed(w)));
    w.on_account_run(on(run_account));
    {
        let (weak, l) = (weak.clone(), l.clone());
        w.on_change_server(move |community| {
            if let Some(w) = weak.upgrade() {
                w.set_screen(s("setup"));
                let info = refresh_info(&w, &l);
                open_setup(&w, &info, Some(community));
            }
        });
    }
    {
        let weak = weak.clone();
        w.on_open_link(move |id| {
            let opened = core::about_link(&id).ok_or_else(|| "Not a link the launcher opens.".to_owned()).and_then(core::open_url);
            if let (Err(e), Some(w)) = (opened, weak.upgrade()) {
                w.set_toast(s(e));
            }
        });
    }
    w.on_send_logs_now(on(|w, l| send_logs(w, l, None)));
    w.on_open_logs(on(|w, l| {
        if let Err(e) = l.open_logs() {
            w.set_logs_said(s(e));
        }
    }));
    w.on_restore(on(|w, l| {
        match l.restore() {
            Ok(said) => {
                w.set_restore_said(s(said));
                w.set_restore_failed(false);
            }
            Err(e) => {
                w.set_restore_said(s(e));
                w.set_restore_failed(true);
            }
        }
        refresh_info(w, l);
    }));
}

pub fn run() -> Result<(), slint::PlatformError> {
    let w = AppWindow::new()?;
    w.set_release(s(core::RELEASE));
    w.set_community_host(s(server::COMMUNITY));
    w.set_community_name(s(server::COMMUNITY_NAME));
    let l = Arc::new(Launcher::new(Launcher::default_prefs_path()));
    wire(&w, &l);
    open(&w, &l, "play");
    w.set_loaded(true);
    // Logs left from earlier games (one that crashed, or was started without the launcher) go
    // as the launcher starts.
    send_logs(&w, &l, None);
    w.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_when_in_utc() {
        assert_eq!(when(0), "1970-01-01 00:00 UTC");
        assert_eq!(when(1_794_746_096), "2026-11-15 12:34 UTC");
        assert_eq!(when(951_782_400), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn says_how_the_server_is() {
        let info = server::Info {
            version: "0.1.0".into(),
            players_online: 1,
            news: Vec::new(),
        };
        assert_eq!(server_detail(Some(&info)), "1 player online · server 0.1.0");
        assert!(server_detail(None).contains("older server"));
    }
}
