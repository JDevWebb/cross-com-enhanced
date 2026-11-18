//! The in-game overlay: a panel over the game (Server, Lobby, News, Leaderboards, Account, Help), opened and closed
//! with the overlay key (`overlay_key`, F5 unless set), and toasts in the top-right corner when
//! something changes (signed in, the server unreachable, news).
//!
//! It draws with Dear ImGui through hudhook, which hooks the game's DirectX `Present` (DirectX 11
//! or 9, by the exe's name). Hooking starts 10 s after the game loads this library: hooking at
//! once can stop a fullscreen game from starting. Anything that fails is logged and the game runs
//! on without the overlay.
//!
//! What the server says about itself (`/api/info`) is asked for on a thread of its own, every
//! minute, and its leaderboard on another when the Leaderboards tab asks, so the game's frames
//! never wait on the network.

mod input;
mod ui;

use std::ffi::CStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use crosscom_client_common::server_info;
use crosscom_client_common::server_info::Change;
use crosscom_client_common::server_info::Leaderboard;
use crosscom_client_common::server_info::Reply;
use crosscom_client_common::server_info::ServerInfo;
use crosscom_client_common::server_info::Watch;
use crosscom_client_common::Config;
use log::error;
use log::info;
use log::warn;
use windows::core::PCSTR;
use windows::Win32::System::LibraryLoader::LoadLibraryA;

/// How long after the library loads the overlay hooks the game's drawing.
const START_DELAY: Duration = Duration::from_secs(10);
/// How often the server is asked for its info.
const POLL_EVERY: Duration = Duration::from_secs(60);
/// How long one request may take, at each step.
const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// The DirectX the game draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Dx9,
    Dx11,
}

impl Engine {
    /// By the exe's name: `Future Soldier DX11.exe` or `Future Soldier DX9.exe`.
    fn detect() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        engine_for(exe.file_name()?.to_str()?)
    }
}

fn engine_for(exe: &str) -> Option<Engine> {
    let exe = exe.to_ascii_lowercase();
    if exe.contains("dx11") {
        Some(Engine::Dx11)
    } else if exe.contains("dx9") {
        Some(Engine::Dx9)
    } else {
        None
    }
}

/// Whether the server answered the last time it was asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Not asked yet.
    #[default]
    Checking,
    Online,
    Offline,
}

/// What the overlay shows about the server.
#[derive(Debug, Clone, Default)]
pub(crate) struct Status {
    pub server: Option<String>,
    pub reach: Reach,
    /// The last info it gave.
    pub info: Option<ServerInfo>,
    /// Why it answered without its info, if it did.
    pub no_info: Option<String>,
    /// Why it couldn't be reached, if it couldn't.
    pub error: Option<String>,
    /// The address reaches another game's server (by its name), not this game's.
    pub other_game: Option<String>,
    pub checked: Option<Instant>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
/// Moves on each time [`STATUS`] changes, so a frame only copies it then.
static STATUS_VERSION: AtomicU32 = AtomicU32::new(0);
/// Toasts to show, oldest first.
static TOASTS: Mutex<Vec<Toast>> = Mutex::new(Vec::new());
/// The game has asked for its sign-in details.
static LOGIN_GIVEN: AtomicBool = AtomicBool::new(false);
/// Wakes the poller early (the game is signing in).
static WAKE: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();
/// The leaderboard last asked for, and asks for another (a mode's place in `LEADERBOARD_MODES`).
static BOARD: Mutex<Option<Board>> = Mutex::new(None);
static BOARD_VERSION: AtomicU32 = AtomicU32::new(0);
static BOARD_ASK: OnceLock<Mutex<mpsc::Sender<usize>>> = OnceLock::new();

/// What the Leaderboards tab shows.
#[derive(Debug, Clone, Default)]
pub(crate) struct Board {
    /// The mode asked for, by its place in `LEADERBOARD_MODES`.
    pub mode: usize,
    pub loading: bool,
    pub board: Option<Leaderboard>,
    /// The server answered without a leaderboard (an older server).
    pub missing: bool,
    pub error: Option<String>,
}

/// Asks for `mode`'s leaderboard (on the leaderboard thread).
pub(crate) fn ask_leaderboard(mode: usize) {
    set_board(|b| {
        b.mode = mode;
        b.loading = true;
    });
    if let Some(ask) = BOARD_ASK.get().and_then(|a| a.lock().ok()) {
        let _ = ask.send(mode);
    }
}

/// The leaderboard, if it changed since `seen` (and its new version).
pub(crate) fn board_since(seen: u32) -> Option<(u32, Board)> {
    let now = BOARD_VERSION.load(Ordering::Acquire);
    if now == seen {
        return None;
    }
    let board = BOARD.try_lock().ok()?.clone().unwrap_or_default();
    Some((now, board))
}

fn set_board(change: impl FnOnce(&mut Board)) {
    if let Ok(mut board) = BOARD.lock() {
        change(board.get_or_insert_with(Board::default));
    }
    BOARD_VERSION.fetch_add(1, Ordering::Release);
}

/// Fetches the leaderboards asked for, the latest ask first, until the game ends.
fn leaderboards(server: &str, asks: &mpsc::Receiver<usize>) {
    while let Ok(mut mode) = asks.recv() {
        while let Ok(later) = asks.try_recv() {
            mode = later;
        }
        let name = server_info::LEADERBOARD_MODES.get(mode).map_or("all", |(name, _)| name);
        let result = server_info::fetch_leaderboard(server, name, POLL_TIMEOUT);
        if let Err(e) = &result {
            warn!("Couldn't ask {server} for its leaderboard: {e}");
        }
        set_board(|b| {
            if b.mode != mode {
                return; // another was asked for meanwhile
            }
            b.loading = false;
            match result {
                Ok(Some(board)) => {
                    b.board = Some(board);
                    b.missing = false;
                    b.error = None;
                }
                Ok(None) => {
                    b.board = None;
                    b.missing = true;
                    b.error = None;
                }
                Err(e) => b.error = Some(e.0),
            }
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Toast {
    pub text: String,
    pub bad: bool,
}

fn toast(text: impl Into<String>, bad: bool) {
    let text = text.into();
    info!("Overlay toast: {text}");
    if let Ok(mut toasts) = TOASTS.lock() {
        toasts.push(Toast { text, bad });
    }
}

/// The toasts waiting to be shown.
pub(crate) fn take_toasts() -> Vec<Toast> {
    TOASTS.try_lock().map(|mut t| std::mem::take(&mut *t)).unwrap_or_default()
}

/// The server's status, if it changed since `seen` (and its new version).
pub(crate) fn status_since(seen: u32) -> Option<(u32, Status)> {
    let now = STATUS_VERSION.load(Ordering::Acquire);
    if now == seen {
        return None;
    }
    let status = STATUS.try_lock().ok()?.clone().unwrap_or_default();
    Some((now, status))
}

fn set_status(change: impl FnOnce(&mut Status)) {
    if let Ok(mut status) = STATUS.lock() {
        change(status.get_or_insert_with(Status::default));
    }
    STATUS_VERSION.fetch_add(1, Ordering::Release);
}

/// The game asked for its sign-in details: it's signing in.
pub(crate) fn login_given() {
    if !LOGIN_GIVEN.swap(true, Ordering::SeqCst) {
        if let Some(wake) = WAKE.get().and_then(|w| w.lock().ok()) {
            let _ = wake.send(());
        }
    }
}

pub(crate) fn login_was_given() -> bool {
    LOGIN_GIVEN.load(Ordering::SeqCst)
}

/// Starts the overlay, unless `overlay = false`. Called once, as the library loads: it only
/// starts threads, which run once the game's loader is done.
pub(crate) fn start(config: &Config) {
    if !config.overlay {
        info!("Overlay off (overlay = false)");
        return;
    }
    let server = config.server.clone().filter(|s| !s.trim().is_empty());
    set_status(|s| s.server.clone_from(&server));
    if let Some(server) = server {
        let (tx, rx) = mpsc::channel();
        let _ = WAKE.set(Mutex::new(tx));
        let username = config.username.clone();
        let polled = server.clone();
        let spawned = std::thread::Builder::new().name("crosscom-server-info".into()).spawn(move || poll(&polled, &username, &rx));
        if let Err(e) = spawned {
            error!("Overlay: couldn't start asking the server for its info: {e}");
        }
        let (tx, rx) = mpsc::channel();
        let _ = BOARD_ASK.set(Mutex::new(tx));
        let spawned = std::thread::Builder::new().name("crosscom-leaderboard".into()).spawn(move || leaderboards(&server, &rx));
        if let Err(e) = spawned {
            error!("Overlay: couldn't start the leaderboard's thread: {e}");
        }
    }
    let Some(engine) = Engine::detect() else {
        warn!("Overlay off: the exe's name doesn't say DX9 or DX11");
        return;
    };
    let (key, vk) = config.overlay_key();
    if !key.eq_ignore_ascii_case(config.overlay_key.trim()) {
        warn!("Overlay: overlay_key {:?} isn't F1 to F12; using {key}", config.overlay_key);
    }
    let spawned = std::thread::Builder::new().name("crosscom-overlay".into()).spawn(move || {
        std::thread::sleep(START_DELAY);
        // hudhook panics on some failures; here that only ends this thread.
        match std::panic::catch_unwind(|| hook(engine, key, vk)) {
            Ok(Ok(())) => info!("Overlay ready ({engine:?}); {key} opens it"),
            Ok(Err(e)) => error!("Overlay off: {e}"),
            Err(_) => error!("Overlay off: hooking {engine:?} failed"),
        }
    });
    if let Err(e) = spawned {
        error!("Overlay off: couldn't start its thread: {e}");
    }
}

/// Hooks the game's drawing.
fn hook(engine: Engine, key: &'static str, vk: u16) -> Result<(), String> {
    use hudhook::hooks::dx11::ImguiDx11Hooks;
    use hudhook::hooks::dx9::ImguiDx9Hooks;
    use hudhook::Hudhook;

    info!("Overlay: hooking {engine:?}");
    // They're delay-loaded (build.rs): loaded here first, so a missing one is an error here and
    // not a crash at its first call.
    let needed: &[&CStr] = match engine {
        Engine::Dx11 => &[c"d3d11.dll", c"d3dcompiler_47.dll"],
        Engine::Dx9 => &[c"d3d9.dll"],
    };
    if let Some(missing) = needed.iter().find(|dll| !library_loads(dll)) {
        return Err(format!("{} couldn't be loaded", missing.to_string_lossy()));
    }
    let overlay = ui::Overlay::new(key, vk);
    let builder = match engine {
        Engine::Dx11 => {
            // hudhook hooks through a dummy device and swap chain, which some systems won't make
            // straight away; without one it would panic.
            let mut tries = 0;
            while let Err(e) = hudhook::hooks::dx11::check() {
                tries += 1;
                if tries == 3 {
                    return Err(format!("DirectX 11 wouldn't make the device the overlay hooks through ({e})"));
                }
                warn!("Overlay: DirectX 11 wouldn't make the device the overlay hooks through ({e}); trying again in 5 s");
                std::thread::sleep(Duration::from_secs(5));
            }
            Hudhook::builder().with::<ImguiDx11Hooks>(overlay)
        }
        Engine::Dx9 => Hudhook::builder().with::<ImguiDx9Hooks>(overlay),
    };
    input::install();
    builder.build().apply().map_err(|e| format!("couldn't hook {engine:?}: {e:?}"))
}

/// Loads `dll` (and keeps it loaded), if it can be.
pub(super) fn library_loads(dll: &CStr) -> bool {
    // SAFETY: a NUL-terminated name; the module is never freed.
    unsafe { LoadLibraryA(PCSTR(dll.as_ptr().cast())) }.is_ok()
}

/// Asks the server for its info every minute (sooner when the game signs in), until the game ends.
fn poll(server: &str, username: &str, wake: &mpsc::Receiver<()>) {
    let mut watch = Watch::default();
    let mut signed_in_shown = false;
    let mut other_game_shown = false;
    loop {
        let result = server_info::fetch(server, POLL_TIMEOUT);
        for change in watch.update(&result) {
            match change {
                Change::Unreachable => toast("Server unreachable", true),
                Change::NewNews(1) => toast("1 new news", false),
                Change::NewNews(n) => toast(format!("{n} new news"), false),
            }
        }
        match &result {
            Ok(Reply::Info(_)) => {}
            Ok(Reply::NoInfo(why)) => info!("{server} answered without its info: {why}"),
            Ok(Reply::OtherGame(name)) => {
                warn!("{server} reaches {name}, another game's server: set this game's server up by its own domain");
                if !std::mem::replace(&mut other_game_shown, true) {
                    toast(format!("{server} is {name}, not this game's server"), true);
                }
            }
            Err(e) => warn!("Couldn't ask {server} for its info: {e}"),
        }
        let online = result.is_ok();
        set_status(|s| {
            s.reach = if online { Reach::Online } else { Reach::Offline };
            s.checked = Some(Instant::now());
            match result {
                Ok(Reply::Info(info)) => {
                    s.info = Some(info);
                    s.no_info = None;
                    s.error = None;
                    s.other_game = None;
                }
                Ok(Reply::NoInfo(why)) => {
                    s.no_info = Some(why);
                    s.error = None;
                    s.other_game = None;
                }
                Ok(Reply::OtherGame(name)) => {
                    s.info = None;
                    s.no_info = None;
                    s.error = None;
                    s.other_game = Some(name);
                }
                Err(e) => s.error = Some(e.0),
            }
        });
        if online && login_was_given() && !signed_in_shown && !username.is_empty() {
            signed_in_shown = true;
            toast(format!("Cross-Com: signed in as {username}"), false);
        }
        match wake.recv_timeout(POLL_EVERY) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(POLL_EVERY),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_engine_by_the_exe() {
        assert_eq!(engine_for("Future Soldier DX11.exe"), Some(Engine::Dx11));
        assert_eq!(engine_for("future soldier dx9.exe"), Some(Engine::Dx9));
        assert_eq!(engine_for("Future Soldier.exe"), None);
    }
}
