//! The window (ui/app.slint) and its callbacks: each screen filled in from the launcher's state
//! (core.rs) when it opens, and the work it asks for done on a thread of its own, the window
//! updated when it's done.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crosscom_client_net::check;
use crosscom_launcher::dlc;
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
use crate::CheckRow;
use crate::CodeRow;
use crate::DlcRow;
use crate::InfoView;
use crate::NewsRow;

/// The fewest-players list in Settings starts at this many.
const MIN_PLAYERS_FROM: u8 = crosscom_client_common::config::MIN_QUICK_MATCH_PLAYERS;

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
        initial: s(initial(info.account.as_deref())),
        skip_intro: info.skip_intro,
        dx9: info.dx9,
        overlay: info.overlay,
        overlay_key: key_index(&info.overlay_key),
        send_logs: info.send_logs,
        port_forwarding: info.port_forwarding,
        min_players: i32::from(info.min_players),
        dlc: dlc_rows(&info.dlc),
        dlc_note: s(dlc::warning(&info.dlc).unwrap_or_default()),
        ready: info.ready(),
    }
}

/// The account's first letter, in capitals, for the circle beside its name ("?" for none).
fn initial(account: Option<&str>) -> String {
    account.and_then(|a| a.chars().next()).map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

/// The packs as the screens list them.
pub fn dlc_rows(scan: &[(dlc::Pack, dlc::State)]) -> ModelRc<DlcRow> {
    let rows: Vec<DlcRow> = scan
        .iter()
        .map(|(pack, state)| DlcRow {
            name: s(pack.name),
            state: s(match state {
                dlc::State::Installed => "yes",
                dlc::State::Missing => "no",
                dlc::State::Incomplete(_) => "incomplete",
            }),
            adds: s(pack.adds),
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
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

/// Opens a screen ("play", "setup" or "settings"). Play is the setup until everything's set (the
/// first launch); "setup" is the setup run again, from Settings › About.
fn open(w: &AppWindow, l: &Arc<Launcher>, screen: &str) {
    w.set_screen(s(screen));
    let info = refresh_info(w, l);
    match screen {
        "play" if info.ready() => {
            show_identity(w, l);
            load_news(w, l);
        }
        "play" | "setup" => open_setup(w, l, &info),
        "settings" => open_settings(w, l, &info),
        "host" => show_host(w, l),
        _ => {}
    }
}

/// Before Host's first start: Windows' prompt for the server's firewall rules.
const HOST_FIREWALL_NOTICE: (&str, &str) = (
    "Windows will ask for permission",
    "Next, Windows asks whether to let Windows PowerShell make changes to your PC. That's the launcher adding \
     Windows Firewall rules, so your friends can reach the server: incoming connections to the server only (TCP for \
     its web side, UDP for the game's sign-in and services). It changes nothing else.\n\nClick Yes when Windows asks.",
);

/// Host's status, looked at off the window's thread (the server is asked how it is).
fn show_host(w: &AppWindow, l: &Arc<Launcher>) {
    let l = l.clone();
    background(
        w,
        move || (l.host.status(), l.plays_on_hosted()),
        |w, (status, here)| {
            w.set_host_state(s(status.state));
            w.set_host_said(s(&status.said));
            w.set_host_players(i32::try_from(status.players).unwrap_or(i32::MAX));
            let lines: Vec<CheckRow> = status
                .lines
                .iter()
                .map(|l| CheckRow {
                    label: s(&l.label),
                    value: s(&l.value),
                    tone: s(l.tone),
                    detail: s(&l.detail),
                    action: s(""),
                })
                .collect();
            w.set_host_lines(ModelRc::new(VecModel::from(lines)));
            let codes: Vec<CodeRow> = status
                .codes
                .iter()
                .map(|c| CodeRow {
                    place: s(&c.place),
                    code: s(&c.code),
                })
                .collect();
            w.set_host_codes(ModelRc::new(VecModel::from(codes)));
            let log: Vec<SharedString> = status.log.iter().rev().take(40).rev().map(s).collect();
            w.set_host_log(ModelRc::new(VecModel::from(log)));
            w.set_host_plays_here(here);
        },
    );
}

/// Host's Start: the notice first when Windows will ask for permission.
fn host_start(w: &AppWindow, l: &Arc<Launcher>) {
    let asking = l.clone();
    background(w, move || asking.host.firewall_would_ask(), {
        let l = l.clone();
        move |w, asks| {
            if asks {
                notice(w, HOST_FIREWALL_NOTICE, "host");
            } else {
                host_start_now(w, &l);
            }
        }
    });
}

/// Starts the server, off the window's thread, showing its steps as they go.
fn host_start_now(w: &AppWindow, l: &Arc<Launcher>) {
    w.set_host_state(s("starting"));
    w.set_host_said(s("Starting…"));
    let starting = l.clone();
    let internet = l.host_internet();
    background(w, move || starting.host.start(internet), {
        let l = l.clone();
        move |w, started: Result<(), String>| {
            if let Err(e) = started {
                toast(w, &e);
            }
            show_host(w, &l);
        }
    });
}

fn host_stop(w: &AppWindow, l: &Arc<Launcher>) {
    w.set_host_said(s("Stopping…"));
    let stopping = l.clone();
    background(w, move || stopping.host.stop(), {
        let l = l.clone();
        move |w, ()| show_host(w, &l)
    });
}

/// Host's Play on my server: the game set up to play on the server hosted here.
fn host_play_here(w: &AppWindow, l: &Arc<Launcher>) {
    w.set_host_play_busy(true);
    w.set_host_play_said(s(""));
    let playing = l.clone();
    background(w, move || playing.play_on_hosted(), {
        let l = l.clone();
        move |w, said: Result<String, String>| {
            w.set_host_play_busy(false);
            match said {
                Ok(said) => {
                    w.set_host_play_said(s(said));
                    w.set_host_play_failed(false);
                }
                Err(e) => {
                    w.set_host_play_said(s(e));
                    w.set_host_play_failed(true);
                }
            }
            refresh_info(w, &l);
            show_host(w, &l);
        }
    });
}

/// Play's checklist, looked at off the window's thread (Windows is asked about the firewall).
fn load_checks(w: &AppWindow, l: &Arc<Launcher>) {
    let l = l.clone();
    background(
        w,
        move || {
            let info = l.info();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            info.game_dir.map(|dir| core::checklist(&dir, info.port_forwarding, now)).unwrap_or_default()
        },
        |w, checks| show_checks(w, &checks),
    );
}

/// Shows Play's checklist.
pub fn show_checks(w: &AppWindow, checks: &[core::Check]) {
    let rows = checks
        .iter()
        .map(|c| CheckRow {
            label: s(c.label),
            value: s(&c.value),
            tone: s(c.tone),
            detail: s(&c.detail),
            action: s(c.action),
        })
        .collect::<Vec<_>>();
    w.set_checks(ModelRc::new(VecModel::from(rows)));
}

/// Play: asks the server how it is, and for its news.
fn load_news(w: &AppWindow, l: &Arc<Launcher>) {
    if w.get_news_loading() {
        return;
    }
    w.set_news_loading(true);
    load_checks(w, l);
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
        server::typed(&w.get_setup_server_typed())
    }
}

/// Setup's fields, filled in from what's set up now: the server set up (the community test
/// network when there's none, or it's that), nothing checked yet. Settings' Server section
/// switches server with them too.
fn prepare_setup(w: &AppWindow, info: &core::Info) {
    let first = !info.ready();
    let set_up = info.server.as_deref().filter(|h| !server::is_community(h));
    w.set_setup_game_dir(s(info.game_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default()));
    w.set_setup_game_error(s(""));
    w.set_setup_community(set_up.is_none());
    w.set_setup_server_typed(s(set_up.unwrap_or_default()));
    w.set_setup_server_ok(s(""));
    w.set_setup_server_error(s(""));
    w.set_setup_account(s(""));
    w.set_setup_name(s(""));
    w.set_setup_code(s(""));
    w.set_setup_skip_intro(first || info.skip_intro);
    w.set_setup_dx9(info.dx9);
    w.set_setup_overlay(info.overlay);
    w.set_setup_send_logs(info.send_logs);
    w.set_setup_port_forwarding(info.port_forwarding);
    w.set_setup_busy(false);
    w.set_setup_error(s(""));
    w.set_setup_step(0);
    w.set_setup_firewall_note(s(""));
    CHECKED.with(|c| c.borrow_mut().clear());
    w.set_setup_server_checking(false);
    setup_changed(w);
}

/// Setup, from the start: filled in from what's set up now, the game looked for when there's
/// none, and the server asked.
fn open_setup(w: &AppWindow, l: &Arc<Launcher>, info: &core::Info) {
    prepare_setup(w, info);
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
        check_server(w, l);
    }
}

/// What Setup still needs, after any change to it.
fn setup_changed(w: &AppWindow) {
    // The packs in the folder chosen, looked at again whenever it changes.
    let dir = w.get_setup_game_dir();
    let scan = if dir.is_empty() { Vec::new() } else { dlc::scan(std::path::Path::new(dir.as_str())) };
    w.set_setup_dlc(dlc_rows(&scan));
    w.set_setup_dlc_note(s(dlc::warning(&scan).unwrap_or_default()));
    let host = setup_host(w);
    // Compared with the server set up: by its host (a join code's fingerprint aside).
    w.set_setup_host(s(server::host(&host)));
    // A server changed since it was checked: checked again before it's trusted.
    if CHECKED.with(|c| *c.borrow() != host) {
        w.set_setup_server_ok(s(""));
        w.set_setup_server_error(s(""));
        w.set_setup_account(s(""));
    }
    // The identity's account there, found when the server was checked; or a new one's name.
    let account = if w.get_setup_account().is_empty() {
        core::name_missing(&w.get_setup_name())
    } else {
        None
    };
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

/// Checks the server Setup has chosen: where the game will sign in, and the account the player's
/// identity has there (if any); or why not.
fn check_server(w: &AppWindow, l: &Arc<Launcher>) {
    let host = setup_host(w);
    if host.is_empty() {
        return;
    }
    CHECKED.with(|c| *c.borrow_mut() = host.clone());
    w.set_setup_server_checking(true);
    w.set_setup_server_ok(s(""));
    w.set_setup_server_error(s(""));
    w.set_setup_account(s(""));
    let checking = host.clone();
    let l = l.clone();
    background(
        w,
        move || {
            let host = server::chosen(&checking)?;
            let at = server::check(&host)?;
            let account = l.find_account(&host).map_err(|e| format!("Couldn't sign in with your identity: {e}"))?;
            Ok::<_, String>((at, account))
        },
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
                Ok((at, account)) => {
                    w.set_setup_account(s(account.unwrap_or_default()));
                    w.set_setup_server_ok(s(at));
                }
                Err(e) => w.set_setup_server_error(s(e)),
            }
            setup_changed(w);
        },
    );
}

/// Asks the player for the game's folder (Windows' own picker), for Setup's first step.
fn choose_game(w: &AppWindow) {
    pick_game(w, |w, picked| {
        match picked {
            Ok(dir) => {
                w.set_setup_game_dir(s(dir.display().to_string()));
                w.set_setup_game_error(s(""));
            }
            Err(e) => w.set_setup_game_error(s(e)),
        }
        setup_changed(w);
    });
}

/// Settings' Change for the game's folder: the folder picked, then set up there as it is now
/// (the server and account set up, the options as they are): the client goes into the new
/// folder.
fn move_game(w: &AppWindow, l: &Arc<Launcher>) {
    let l = l.clone();
    w.set_moved_said(s(""));
    pick_game(w, move |w, picked| {
        let dir = match picked {
            Ok(dir) => dir,
            Err(e) => {
                w.set_moved_said(s(e));
                w.set_moved_failed(true);
                return;
            }
        };
        let info = l.info();
        let setup = core::Setup {
            game_dir: dir,
            server: info.server.clone().unwrap_or_default(),
            name: String::new(),
            code: String::new(),
            skip_intro: info.skip_intro,
            dx9: info.dx9,
            overlay: info.overlay,
            send_logs: info.send_logs,
            port_forwarding: info.port_forwarding,
        };
        run_setup_with(w, &l, setup, After::Moved);
    });
}

/// Windows' folder picker, then the folder checked for the game; `done` with it (`Err` for one
/// without the game). Nothing when the player cancels.
fn pick_game(w: &AppWindow, done: impl FnOnce(&AppWindow, Result<PathBuf, String>) + Send + 'static) {
    #[cfg(windows)]
    background(
        w,
        || rfd::FileDialog::new().set_title("The folder with \"Future Soldier DX11.exe\"").pick_folder(),
        move |w, dir: Option<PathBuf>| {
            let Some(dir) = dir else { return };
            let checked = core::check_game(&dir).map(|()| dir);
            done(w, checked);
        },
    );
    #[cfg(not(windows))]
    done(w, Err("Choosing a folder needs Windows.".into()));
}

/// On Setup's last step, before the press that brings Windows' permission prompt (the firewall
/// rules); and the notice before the connection test's.
pub const SETUP_FIREWALL: &str = "When you press Set up, Windows asks whether to let Windows PowerShell make changes to \
     your PC. That's the launcher adding Windows Firewall rules, so the players in your matches can reach your game. \
     It adds rules only for the game and for this launcher's connection test, and changes nothing else.\n\nClick Yes \
     when Windows asks. With No, Setup still finishes, but Windows asks again the first time you play online, which can \
     minimise the game.";
const TEST_NOTICE: (&str, &str) = (
    "Windows will ask for permission",
    "The connection test has the server send a packet to the port your router forwards, and Windows Firewall has \
     to let the launcher receive it. Next, Windows asks whether to let Windows PowerShell make changes to your PC: \
     that's the launcher adding a firewall rule for itself, for incoming UDP only.\n\nClick Yes when Windows asks. \
     With No, the test still runs, but it can't check your port forwarding.",
);

const FIREWALL_NOTICE: (&str, &str) = (
    "Windows will ask for permission",
    "Next, Windows asks whether to let Windows PowerShell make changes to your PC. That's the launcher adding \
     Windows Firewall rules, so the players in your matches can reach your game: incoming UDP only, for the game \
     and for this launcher's connection test. It changes nothing else.\n\nClick Yes when Windows asks.",
);

/// Play's Firewall card's Allow: the notice, then Windows' prompt (`allow_firewall_now`).
fn allow_firewall(w: &AppWindow) {
    notice(w, FIREWALL_NOTICE, "firewall");
}

/// Adds the firewall rules, off the window's thread; then says how it went and looks again.
fn allow_firewall_now(w: &AppWindow, l: &Arc<Launcher>) {
    let Some(dir) = l.game_dir() else { return };
    let after = l.clone();
    background(
        w,
        move || core::allow_firewall(&dir),
        move |w, said| {
            toast(w, &said);
            load_checks(w, &after);
        },
    );
}

/// Shows the notice `(title, body)`; Continue goes on with `then`.
fn notice(w: &AppWindow, (title, body): (&str, &str), then: &str) {
    w.set_notice_title(s(title));
    w.set_notice_body(s(body));
    w.set_notice_for(s(then));
}

fn close_notice(w: &AppWindow) {
    w.set_notice_title(s(""));
    w.set_notice_body(s(""));
    w.set_notice_for(s(""));
}

/// A step of Setup was shown: on the last, whether Windows will ask for permission, said there
/// before the press that brings the prompt.
fn setup_step_entered(w: &AppWindow, step: i32) {
    if step != 4 {
        return;
    }
    w.set_setup_firewall_note(s(""));
    let dir = PathBuf::from(w.get_setup_game_dir().as_str());
    background(
        w,
        move || core::firewall_would_ask(&dir),
        |w, asks| {
            if w.get_setup_step() == 4 {
                w.set_setup_firewall_note(s(if asks { SETUP_FIREWALL } else { "" }));
            }
        },
    );
}

/// What a setup was run for: the setup screen's, Settings' switch of server, or its move of the
/// game's folder (each says how it went in its own place).
#[derive(Clone, Copy)]
enum After {
    Setup,
    Switched,
    Moved,
}

/// What Setup's fields (and Settings' Server section, which uses them) set up.
fn setup_from_window(w: &AppWindow) -> core::Setup {
    core::Setup {
        game_dir: PathBuf::from(w.get_setup_game_dir().as_str()),
        server: setup_host(w),
        name: w.get_setup_name().into(),
        code: w.get_setup_code().into(),
        skip_intro: w.get_setup_skip_intro(),
        dx9: w.get_setup_dx9(),
        overlay: w.get_setup_overlay(),
        send_logs: w.get_setup_send_logs(),
        port_forwarding: w.get_setup_port_forwarding(),
    }
}

/// Sets it all up, from Setup's last step (which said whether Windows will ask, and why).
fn run_setup(w: &AppWindow, l: &Arc<Launcher>) {
    run_setup_with(w, l, setup_from_window(w), After::Setup);
}

/// Settings' Switch to this server: the setup again with the server chosen there, the game and
/// options as they are.
fn switch_server(w: &AppWindow, l: &Arc<Launcher>) {
    w.set_switch_said(s(""));
    run_setup_with(w, l, setup_from_window(w), After::Switched);
}

fn run_setup_with(w: &AppWindow, l: &Arc<Launcher>, setup: core::Setup, after: After) {
    w.set_setup_busy(true);
    w.set_setup_error(s(""));
    let worker = l.clone();
    let l = l.clone();
    background(
        w,
        move || worker.setup(setup),
        move |w, done| {
            w.set_setup_busy(false);
            if done.is_ok() {
                // The server may be another: Play asks afresh.
                w.set_server_state(s("asking"));
                w.set_news(ModelRc::default());
            }
            match (after, done) {
                (After::Setup, Ok(said)) => {
                    w.set_play_tab(s("status"));
                    open(w, &l, "play");
                    toast(w, &said);
                }
                (After::Setup, Err(e)) => w.set_setup_error(s(e)),
                (After::Switched | After::Moved, done) => {
                    let failed = done.is_err();
                    let said = done.unwrap_or_else(|e| e);
                    let info = refresh_info(w, &l);
                    if !failed {
                        // Settings shows what's set up now: the new server, nothing to switch to.
                        prepare_setup(w, &info);
                    }
                    if matches!(after, After::Switched) {
                        w.set_switch_said(s(said));
                        w.set_switch_failed(failed);
                    } else {
                        w.set_moved_said(s(said));
                        w.set_moved_failed(failed);
                    }
                }
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
    w.set_opt_port_forwarding(view.port_forwarding);
    w.set_opt_min_players(view.min_players - i32::from(MIN_PLAYERS_FROM));
    w.set_options_said(s(""));
    show_identity(w, l);
    w.set_id_paste(s(""));
    w.set_id_confirm(s(""));
    w.set_id_said(s(""));
    w.set_restore_confirming(false);
    w.set_restore_said(s(""));
    w.set_moved_said(s(""));
    w.set_switch_said(s(""));
    // The Server section switches server with Setup's own fields and check.
    if !w.get_setup_busy() {
        prepare_setup(w, info);
    }
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

/// The connection test's result, in Settings.
pub fn show_connection(w: &AppWindow, result: &Result<check::Report, String>) {
    let (summary, tone, details) = match result {
        Ok(report) => (
            report.summary(),
            match report.verdict() {
                check::Verdict::Open => "good",
                check::Verdict::Moderate => "warn",
                check::Verdict::Strict | check::Verdict::Blocked => "bad",
            },
            report.details(),
        ),
        Err(e) => (e.clone(), "bad", Vec::new()),
    };
    w.set_conn_summary(s(summary));
    w.set_conn_tone(s(tone));
    w.set_conn_details(ModelRc::new(VecModel::from(details.into_iter().map(s).collect::<Vec<_>>())));
}

/// The connection test, after the notice when Windows will ask for permission.
fn test_connection(w: &AppWindow, l: &Arc<Launcher>) {
    w.set_conn_testing(true);
    let l = l.clone();
    background(w, core::launcher_firewall_would_ask, move |w, asks| {
        if asks {
            w.set_conn_testing(false);
            notice(w, TEST_NOTICE, "test");
        } else {
            test_connection_now(w, &l);
        }
    });
}

/// Runs the connection test, off the window's thread.
fn test_connection_now(w: &AppWindow, l: &Arc<Launcher>) {
    w.set_conn_testing(true);
    w.set_conn_summary(s("Testing…"));
    w.set_conn_tone(s("warn"));
    w.set_conn_details(ModelRc::default());
    let worker = l.clone();
    background(
        w,
        move || worker.test_connection(),
        move |w, result| {
            w.set_conn_testing(false);
            show_connection(w, &result);
        },
    );
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
        port_forwarding: w.get_opt_port_forwarding(),
        min_players: u8::try_from(w.get_opt_min_players()).map_or(core::DEFAULT_MIN_PLAYERS, |i| i.saturating_add(MIN_PLAYERS_FROM)),
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

/// The player's identity, in Settings: its short form, or why it can't be read.
fn show_identity(w: &AppWindow, l: &Launcher) {
    let (short, error) = match l.identity_short() {
        Ok(short) => (short, String::new()),
        Err(e) => (String::new(), e),
    };
    w.set_id_short(s(short));
    w.set_id_error(s(error));
}

/// Settings' Copy: the identity as text, for the clipboard ("" when it can't be read).
fn identity_export(w: &AppWindow, l: &Launcher) -> SharedString {
    match l.identity_export() {
        Ok(text) => {
            w.set_id_said(s(IDENTITY_COPIED));
            w.set_id_failed(false);
            s(text)
        }
        Err(e) => {
            w.set_id_said(s(e));
            w.set_id_failed(true);
            s("")
        }
    }
}

/// Said once the identity is copied.
pub const IDENTITY_COPIED: &str = "Copied. On the other PC, paste it into \"Use an identity from another PC\". Whoever has it can sign in as you: keep it private.";

/// Uses the identity pasted from another PC: asks first when it would replace a different one,
/// then signs in to its account on the server set up (off the window's thread).
fn use_identity(w: &AppWindow, l: &Arc<Launcher>, confirmed: bool) {
    let text = w.get_id_paste().to_string();
    w.set_id_busy(true);
    w.set_id_said(s(""));
    let worker = l.clone();
    let l = l.clone();
    background(
        w,
        move || worker.use_identity(&text, confirmed),
        move |w, used| {
            w.set_id_busy(false);
            match used {
                Ok(core::Imported::Confirm { current, new }) => w.set_id_confirm(s(format!(
                    "This replaces this PC's identity {current} with {new}: the accounts {current} has won't sign in from this \
                     PC any more. The launcher keeps {current} in its folder, so it can be put back."
                ))),
                Ok(core::Imported::Done(said)) => {
                    w.set_id_paste(s(""));
                    w.set_id_said(s(said));
                    w.set_id_failed(false);
                }
                Err(e) => {
                    w.set_id_said(s(e));
                    w.set_id_failed(true);
                }
            }
            show_identity(w, &l);
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
    // The check before the game starts: the packs looked at again (they can change between
    // starts), and the warning shown if one is missing. It only warns: the game still starts.
    refresh_info(w, l);
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
                let mut exit = child.wait().ok().and_then(|status| status.code());
                // The game starts itself again and that first exe exits: wait for the game.
                if after.game_dir().is_some_and(|dir| crosscom_launcher::logs::wait_for_game(&dir)) {
                    exit = None;
                }
                let _ = weak.upgrade_in_event_loop(move |w| {
                    // "The game is starting." no longer holds once it has ended.
                    w.set_play_said(s(""));
                    w.set_play_failed(false);
                    w.set_playing(false);
                    // What the game found about its connection, on the checklist.
                    load_checks(&w, &after);
                    send_logs(&w, &after, exit);
                });
            });
            Ok::<_, String>(said)
        },
        move |w, started| {
            w.set_starting(false);
            match started {
                Ok(said) => {
                    w.set_play_said(s(said));
                    w.set_playing(true);
                }
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
    {
        let l = l.clone();
        w.on_close_window(move || {
            // The server hosted here stops with the launcher, its forwards taken away.
            l.host.stop();
            let _ = slint::quit_event_loop();
        });
    }
    w.on_host_start(on(host_start));
    w.on_host_stop(on(host_stop));
    w.on_host_play_here(on(host_play_here));
    {
        let (weak, l) = (weak.clone(), l.clone());
        w.on_host_access(move |internet| {
            if let (Err(e), Some(w)) = (l.set_host_internet(internet), weak.upgrade()) {
                toast(&w, &e);
            }
        });
    }
    {
        let weak = weak.clone();
        w.on_host_copied(move |place| {
            if let Some(w) = weak.upgrade() {
                toast(&w, &format!("Copied the join code for {}.", place.to_lowercase()));
            }
        });
    }
    {
        let (weak, l) = (weak.clone(), l.clone());
        w.on_go(move |screen| {
            if let Some(w) = weak.upgrade() {
                open(&w, &l, &screen);
            }
        });
    }
    w.on_play(on(play));
    w.on_update_now(on(|w, _| crate::update::now(w)));
    w.on_update_notes(on(|_, _| crate::update::notes()));
    w.on_update_later(on(|w, _| crate::update::later(w)));
    w.on_check_update(on(|w, _| crate::update::check(w)));
    w.on_refresh_news(on(load_news));
    {
        let weak = weak.clone();
        w.on_check_action(move |label| {
            if let (Some(w), "Firewall") = (weak.upgrade(), label.as_str()) {
                allow_firewall(&w);
            }
        });
    }
    w.on_setup_choose_game(on(|w, _| choose_game(w)));
    w.on_setup_check_server(on(check_server));
    w.on_setup_changed(on(|w, _| setup_changed(w)));
    w.on_setup_run(on(run_setup));
    {
        let weak = weak.clone();
        w.on_setup_step_entered(move |step| {
            if let Some(w) = weak.upgrade() {
                setup_step_entered(&w, step);
            }
        });
    }
    w.on_options_changed(on(save_options));
    {
        let (weak, l) = (weak.clone(), l.clone());
        w.on_identity_export(move || weak.upgrade().map(|w| identity_export(&w, &l)).unwrap_or_default());
    }
    {
        let (weak, l) = (weak.clone(), l.clone());
        w.on_identity_use(move |confirmed| {
            if let Some(w) = weak.upgrade() {
                use_identity(&w, &l, confirmed);
            }
        });
    }
    w.on_move_game(on(move_game));
    w.on_switch_server(on(switch_server));
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
    w.on_test_connection(on(test_connection));
    {
        let weak = weak.clone();
        let l = l.clone();
        w.on_notice_proceed(move |then| {
            let Some(w) = weak.upgrade() else { return };
            close_notice(&w);
            match then.as_str() {
                "test" => test_connection_now(&w, &l),
                "firewall" => allow_firewall_now(&w, &l),
                "host" => host_start_now(&w, &l),
                _ => {}
            }
        });
    }
    {
        let weak = weak.clone();
        w.on_notice_cancel(move |_| {
            if let Some(w) = weak.upgrade() {
                close_notice(&w);
            }
        });
    }
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
    w.set_community_status(s(server::COMMUNITY_STATUS));
    let l = Arc::new(Launcher::new(Launcher::default_prefs_path()));
    wire(&w, &l);
    w.set_host_carries_server(crosscom_launcher::hosting::built_in().is_some() || cfg!(debug_assertions));
    w.set_host_internet(l.host_internet());
    // Host's status, while it's shown or the server runs.
    let host_timer = slint::Timer::default();
    {
        let (weak, l) = (w.as_weak(), l.clone());
        host_timer.start(slint::TimerMode::Repeated, Duration::from_secs(2), move || {
            if let Some(w) = weak.upgrade() {
                if w.get_shown() == "host" || w.get_host_state() != "stopped" {
                    show_host(&w, &l);
                }
            }
        });
    }
    open(&w, &l, "play");
    w.set_loaded(true);
    crate::update::start(&w);
    // Logs left from earlier games (one that crashed, or was started without the launcher) go
    // as the launcher starts; with no game yet there are none.
    if l.game_dir().is_some() {
        send_logs(&w, &l, None);
    }
    w.show()?;
    // Once the event loop has made the window and Windows has placed it.
    let weak = w.as_weak();
    slint::Timer::single_shot(Duration::from_millis(50), move || {
        if let Some(w) = weak.upgrade() {
            fit_to_screen(&w);
        }
    });
    slint::run_event_loop()?;
    w.hide()
}

/// Room left under the window for the taskbar, and round it.
const TASKBAR: f64 = 64.0;
const EDGE: f64 = 16.0;

/// Fits the window to the screen it opened on: smaller when the screen is (a laptop at 150 %
/// scaling has 720 points of height), and moved up when Windows placed it running under the
/// taskbar; otherwise Play and the status bar are hidden there.
fn fit_to_screen(w: &AppWindow) {
    use slint::winit_030::WinitWindowAccessor as _;
    let fit = w.window().with_winit_window(|win| {
        let monitor = win.current_monitor()?;
        let scale = monitor.scale_factor();
        let screen = monitor.size().to_logical::<f64>(scale);
        let at = monitor.position().to_logical::<f64>(scale);
        let size = win.outer_size().to_logical::<f64>(scale);
        let now = win.outer_position().ok()?.to_logical::<f64>(scale);
        let resized = fitted((size.width, size.height), (screen.width, screen.height));
        let (width, height) = resized.unwrap_or((size.width, size.height));
        let below = now.y + height > at.y + screen.height - TASKBAR;
        let beside = now.x + width > at.x + screen.width;
        if resized.is_none() && !below && !beside {
            return None;
        }
        let x = at.x + ((screen.width - width) / 2.0).max(0.0);
        let y = at.y + ((screen.height - TASKBAR - height) / 2.0).max(0.0);
        Some((width, height, x, y))
    });
    if let Some(Some((width, height, x, y))) = fit {
        #[allow(clippy::cast_possible_truncation)]
        {
            w.window().set_size(slint::LogicalSize::new(width as f32, height as f32));
            w.window().set_position(slint::LogicalPosition::new(x as f32, y as f32));
        }
    }
}

/// The window's size fitted to a screen's (in points), keeping clear of the taskbar and the
/// edges; `None` when it fits already. Never below the window's least (940 by 600).
fn fitted((width, height): (f64, f64), (screen_width, screen_height): (f64, f64)) -> Option<(f64, f64)> {
    let room = (screen_width - 2.0 * EDGE, screen_height - TASKBAR - EDGE);
    if width <= room.0 && height <= room.1 {
        return None;
    }
    Some((width.min(room.0).max(940.0), height.min(room.1).max(600.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_the_window_to_a_small_screen() {
        // A 1920 by 1080 screen at 100 %: room to spare.
        assert_eq!(fitted((1180.0, 780.0), (1920.0, 1080.0)), None);
        // At 150 %, 1280 by 720 points: shorter, as wide.
        assert_eq!(fitted((1180.0, 780.0), (1280.0, 720.0)), Some((1180.0, 640.0)));
        // The Surface at 200 %, 1024 by 768: both, never below the least.
        assert_eq!(fitted((1180.0, 780.0), (1024.0, 768.0)), Some((992.0, 688.0)));
        assert_eq!(fitted((1180.0, 780.0), (800.0, 600.0)), Some((940.0, 600.0)));
    }

    #[test]
    fn takes_the_accounts_initial() {
        assert_eq!(initial(Some("kozak")), "K");
        assert_eq!(initial(Some("Ébène")), "É");
        assert_eq!(initial(None), "?");
    }

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
