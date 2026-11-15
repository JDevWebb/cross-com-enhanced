//! What the page asks of the launcher: how things stand, and the setup and play, done by the
//! launcher library. Anything that waits on the network or the disk runs off the window's thread.
//!
//! The game's logs go to the server once a game the launcher started ends (on a thread that
//! waits for it), and as the launcher starts (see `crosscom_launcher::logs`).

use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use crosscom_client_common::config::Config;
use crosscom_launcher::client;
use crosscom_launcher::game;
use crosscom_launcher::game::Installed;
use crosscom_launcher::logs;
use crosscom_launcher::server;
use crosscom_launcher::settings;
use crosscom_launcher::settings::Launcher as Prefs;
use serde::Deserialize;
use serde::Serialize;
use tauri::Manager;

/// What the game is given as its CD key when none was set: the server doesn't check it.
const NO_CD_KEY: &str = "CROSSCOM";

pub struct Launcher {
    prefs_path: PathBuf,
    prefs: Mutex<Prefs>,
    /// What the last sending of logs said, and whether logs are being sent now.
    logs: Mutex<(Option<String>, bool)>,
}

impl Launcher {
    pub fn new() -> Self {
        let base = std::env::var_os("APPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
        let prefs_path = base.join("Cross-Com").join("launcher.toml");
        let mut prefs = Prefs::load(&prefs_path);
        if prefs.game_dir.as_deref().is_none_or(|d| !game::is_game_dir(d)) {
            prefs.game_dir = game::find(&game::candidates());
        }
        Self {
            prefs_path,
            prefs: Mutex::new(prefs),
            logs: Mutex::new((None, false)),
        }
    }

    fn prefs(&self) -> Prefs {
        self.prefs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn save(&self, prefs: Prefs) -> Result<(), String> {
        prefs.save(&self.prefs_path)?;
        *self.prefs.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = prefs;
        Ok(())
    }

    fn logs(&self) -> std::sync::MutexGuard<'_, (Option<String>, bool)> {
        self.logs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends the game's pending logs (after moving the last run's aside, when no game has it
    /// open), unless they're being sent already; what came of it. Waits on the network: call it
    /// off the window's thread.
    pub fn send_logs(&self, exit: Option<i32>) -> String {
        let Some(dir) = self.prefs().game_dir.filter(|d| game::is_game_dir(d)) else {
            return "No game set up.".into();
        };
        if std::mem::replace(&mut self.logs().1, true) {
            return "Logs are being sent.".into();
        }
        let ended = logs::finish_run(&dir);
        let outcome = logs::send(&dir, env!("CARGO_PKG_VERSION"), ended.zip(exit));
        let said = logs::said(&outcome);
        if let Some(latest) = outcome.sent.first() {
            let mut prefs = self.prefs();
            prefs.last_logs = Some(settings::LastLogs {
                id: latest.id.clone(),
                at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            });
            let _ = self.save(prefs);
        }
        *self.logs() = (Some(said.clone()), false);
        said
    }
}

/// How things stand, for every screen.
#[derive(Serialize)]
pub struct Info {
    product: &'static str,
    release: &'static str,
    game_dir: Option<String>,
    /// Cross-Com's client in the game: "none" (no game), "no", "yes" or "update".
    client: &'static str,
    /// Whether this launcher carries the client (a release build does).
    carries_client: bool,
    server: Option<String>,
    account: Option<String>,
    skip_intro: bool,
    dx9: bool,
    /// The in-game overlay, and the key that opens it (`F1` to `F12`).
    overlay: bool,
    overlay_key: String,
    /// Send the game's logs to the server after each game.
    send_logs: bool,
    /// Everything's set: Play can go.
    ready: bool,
}

fn client_state(dir: Option<&Path>) -> &'static str {
    match (dir, client::files()) {
        (None, _) => "none",
        (Some(dir), Some(files)) => match game::installed(dir, &files) {
            Installed::No => "no",
            Installed::Yes => "yes",
            Installed::Other => "update",
        },
        (Some(dir), None) if game::CLIENT_DLLS.iter().all(|d| dir.join(format!("{d}.orig")).is_file()) => "yes",
        (Some(_), None) => "no",
    }
}

#[tauri::command]
pub fn info(state: tauri::State<'_, Launcher>) -> Info {
    let prefs = state.prefs();
    let dir = prefs.game_dir.as_deref().filter(|d| game::is_game_dir(d));
    let config = dir.map(settings::client).unwrap_or_default();
    let overlay = dir.map(settings::overlay).unwrap_or_default();
    let client = client_state(dir);
    let server = config.server.clone().filter(|s| !s.is_empty());
    let account = Some(config.username.clone()).filter(|n| !n.is_empty());
    Info {
        product: "Cross-Com Enhanced",
        release: env!("CARGO_PKG_VERSION"),
        game_dir: dir.map(|d| d.display().to_string()),
        ready: dir.is_some() && server.is_some() && account.is_some() && client != "no",
        client,
        carries_client: client::files().is_some(),
        server,
        account,
        skip_intro: config.skip_intro_videos,
        dx9: prefs.dx9,
        overlay: overlay.on,
        overlay_key: overlay.key,
        send_logs: config.send_logs,
    }
}

/// The game's folder, found on its own, if it can be.
#[tauri::command]
pub async fn find_game() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| game::find(&game::candidates()).map(|d| d.display().to_string()))
        .await
        .ok()
        .flatten()
}

/// Whether `dir` (chosen by the player) holds the game.
#[tauri::command]
pub fn check_game(dir: String) -> Result<String, String> {
    if game::is_game_dir(Path::new(&dir)) {
        Ok(dir)
    } else {
        Err("That folder doesn't hold the game: choose the one with \"Future Soldier DX11.exe\" in it.".into())
    }
}

/// Where the server says the game will sign in; an error if it doesn't answer as a Cross-Com
/// server.
#[tauri::command]
pub async fn check_server(server: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || server::check(&server)).await.map_err(|e| e.to_string())?
}

/// How the server set up is: its version, players online and news. `None` for a server that
/// answers but is older than the launcher's API; an error if it doesn't answer.
#[tauri::command]
pub async fn server_info(state: tauri::State<'_, Launcher>) -> Result<Option<server::Info>, String> {
    let dir = state.prefs().game_dir.filter(|d| game::is_game_dir(d)).ok_or("No game set up.")?;
    let host = settings::client(&dir).server.filter(|s| !s.is_empty()).ok_or("No server set up.")?;
    tauri::async_runtime::spawn_blocking(move || server::info(&host)).await.map_err(|e| e.to_string())?
}

/// Signs in to an account (`new` false) or makes one, then the account the game signs in with.
/// Whether the server checked it (an older one can't), and how the password went.
fn account(host: &str, name: &str, password: &str, new: bool, code: &str) -> Result<(bool, server::Sent), String> {
    if new {
        let (_, sent) = server::sign_up(host, name, password, code)?;
        Ok((true, sent))
    } else {
        let (pid, sent) = server::sign_in(host, name, password)?;
        Ok((pid.is_some(), sent))
    }
}

/// Said after an account's done, when its password went unencrypted.
fn unencrypted_note(sent: server::Sent) -> &'static str {
    match sent {
        server::Sent::Encrypted => "",
        server::Sent::Unencrypted => " This server on your network has no HTTPS, so your password went to it unencrypted.",
    }
}

#[derive(Deserialize)]
pub struct Setup {
    game_dir: String,
    server: String,
    name: String,
    password: String,
    /// Make the account first (with the server's sign-up code, if it has one).
    new_account: bool,
    #[serde(default)]
    code: String,
    skip_intro: bool,
    dx9: bool,
}

/// Sets everything up in one go: checks the game and the server, makes the account if asked,
/// installs the client and writes its settings.
#[tauri::command]
pub async fn setup(state: tauri::State<'_, Launcher>, setup: Setup) -> Result<String, String> {
    let Some(files) = client::files() else {
        return Err("This launcher was built without the client; download a release.".into());
    };
    let dir = PathBuf::from(&setup.game_dir);
    if !game::is_game_dir(&dir) {
        return Err("That folder doesn't hold the game.".into());
    }
    let name = setup.name.trim().to_owned();
    if name.is_empty() || setup.password.is_empty() {
        return Err("Fill in your name and password.".into());
    }
    let host = server::host(&setup.server);
    let old = settings::client(&dir);
    let overlay = settings::overlay(&dir);
    let config = Config {
        server: Some(host.clone()),
        username: name.clone(),
        password: setup.password.clone(),
        cd_key: if old.cd_key.is_empty() { NO_CD_KEY.into() } else { old.cd_key.clone() },
        skip_intro_videos: setup.skip_intro,
        ..old
    };
    let (new, code, work_dir) = (setup.new_account, setup.code, dir.clone());
    let (checked, sent) = tauri::async_runtime::spawn_blocking(move || -> Result<(bool, server::Sent), String> {
        server::check(&host)?;
        let checked = account(&host, &config.username, &config.password, new, &code)?;
        game::install(&work_dir, &files)?;
        settings::save_client(&work_dir, &config, &overlay)?;
        Ok(checked)
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut prefs = state.prefs();
    prefs.game_dir = Some(dir);
    prefs.dx9 = setup.dx9;
    state.save(prefs)?;
    let said = match (setup.new_account, checked) {
        (true, _) => format!("Account {name} made, and the game is set up."),
        (false, true) => format!("Signed in as {name}, and the game is set up."),
        (false, false) => "The game is set up. This server can't check passwords here; the game will.".into(),
    };
    Ok(said + unencrypted_note(sent))
}

#[derive(Deserialize)]
pub struct Account {
    name: String,
    password: String,
    /// Make the account (with the server's sign-up code, if it has one) rather than sign in.
    new_account: bool,
    #[serde(default)]
    code: String,
}

/// Another account on the server set up: signs in to it (or makes it), then the game uses it.
#[tauri::command]
pub async fn set_account(state: tauri::State<'_, Launcher>, account: Account) -> Result<String, String> {
    let dir = state.prefs().game_dir.filter(|d| game::is_game_dir(d)).ok_or("Set the game up first.")?;
    let name = account.name.trim().to_owned();
    if name.is_empty() || account.password.is_empty() {
        return Err("Fill in your name and password.".into());
    }
    let config = settings::client(&dir);
    let host = config.server.clone().filter(|s| !s.is_empty()).ok_or("Set a server up first.")?;
    let overlay = settings::overlay(&dir);
    let config = Config {
        username: name.clone(),
        password: account.password,
        ..config
    };
    let new = account.new_account;
    let (checked, sent) = tauri::async_runtime::spawn_blocking(move || -> Result<(bool, server::Sent), String> {
        let checked = self::account(&host, &config.username, &config.password, new, &account.code)?;
        settings::save_client(&dir, &config, &overlay)?;
        Ok(checked)
    })
    .await
    .map_err(|e| e.to_string())??;
    let said = match (new, checked) {
        (true, _) => format!("Account {name} made. The game signs in with it."),
        (false, true) => format!("Signed in as {name}. The game signs in with it."),
        (false, false) => format!("The game signs in as {name} now. This server can't check passwords here; the game will."),
    };
    Ok(said + unencrypted_note(sent))
}

#[derive(Deserialize)]
pub struct Options {
    skip_intro: bool,
    dx9: bool,
    overlay: bool,
    overlay_key: String,
    send_logs: bool,
}

/// The game options: written into the client's settings and the launcher's.
#[tauri::command]
pub fn set_options(state: tauri::State<'_, Launcher>, options: Options) -> Result<(), String> {
    let mut prefs = state.prefs();
    if let Some(dir) = prefs.game_dir.clone().filter(|d| game::is_game_dir(d)) {
        let mut config = settings::client(&dir);
        if !config.username.is_empty() {
            config.skip_intro_videos = options.skip_intro;
            config.send_logs = options.send_logs;
            let overlay = settings::Overlay {
                on: options.overlay,
                key: options.overlay_key,
            };
            settings::save_client(&dir, &config, &overlay)?;
        }
    }
    prefs.dx9 = options.dx9;
    state.save(prefs)
}

/// Starts the game, bringing the client up to date first if this launcher carries a newer one.
/// Once it ends, its logs go to the server (on a thread of their own).
#[tauri::command]
pub async fn play(app: tauri::AppHandle, state: tauri::State<'_, Launcher>) -> Result<String, String> {
    let prefs = state.prefs();
    let dir = prefs.game_dir.clone().filter(|d| game::is_game_dir(d)).ok_or("Set the game up first.")?;
    let exe = dir.join(if prefs.dx9 { game::EXE_DX9 } else { game::EXE_DX11 });
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let mut updated = false;
        if let Some(files) = client::files() {
            if game::installed(&dir, &files) != Installed::Yes {
                game::install(&dir, &files)?;
                updated = true;
            }
        }
        let mut child = std::process::Command::new(&exe)
            .current_dir(&dir)
            .spawn()
            .map_err(|e| format!("Couldn't start {}: {e}", exe.display()))?;
        std::thread::spawn(move || {
            let exit = child.wait().ok().and_then(|status| status.code());
            app.state::<Launcher>().send_logs(exit);
        });
        Ok(if updated {
            "Cross-Com's client was brought up to date. The game is starting.".into()
        } else {
            "The game is starting.".into()
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// How the logs stand, for Settings.
#[derive(Serialize)]
pub struct LogsStatus {
    /// The last report the server took, and when (Unix time).
    last_id: Option<String>,
    last_at: Option<u64>,
    /// What the last sending said.
    said: Option<String>,
    sending: bool,
}

#[tauri::command]
pub fn logs_status(state: tauri::State<'_, Launcher>) -> LogsStatus {
    let last = state.prefs().last_logs;
    let (said, sending) = state.logs().clone();
    LogsStatus {
        last_id: last.as_ref().map(|l| l.id.clone()),
        last_at: last.map(|l| l.at),
        said,
        sending,
    }
}

/// Sends the logs waiting to go now.
#[tauri::command]
pub async fn send_logs(app: tauri::AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<Launcher>().send_logs(None))
        .await
        .map_err(|e| e.to_string())
}

/// Opens the folder the game's logs are kept in.
#[tauri::command]
pub fn open_logs(state: tauri::State<'_, Launcher>) -> Result<(), String> {
    let dir = state.prefs().game_dir.filter(|d| game::is_game_dir(d)).ok_or("No game set up.")?;
    let logs = dir.join(crosscom_client_common::runs::DIR);
    std::fs::create_dir_all(&logs).map_err(|e| format!("Couldn't make {}: {e}", logs.display()))?;
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(opener)
        .arg(&logs)
        .spawn()
        .map_err(|e| format!("Couldn't open {}: {e}", logs.display()))?;
    Ok(())
}

/// Puts the game's own files back (Cross-Com's client comes out of it).
#[tauri::command]
pub fn restore(state: tauri::State<'_, Launcher>) -> Result<String, String> {
    let dir = state.prefs().game_dir.filter(|d| game::is_game_dir(d)).ok_or("No game set up.")?;
    game::restore(&dir)?;
    Ok("The game's own files are back. Set up again to play on Cross-Com.".into())
}
