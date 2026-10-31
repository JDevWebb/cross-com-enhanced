//! What the page asks of the launcher: how things stand, and the setup and play, done by the
//! launcher library. Anything that waits on the network or the disk runs off the window's thread.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use crosscom_client_common::config::Config;
use crosscom_launcher::client;
use crosscom_launcher::game;
use crosscom_launcher::game::Installed;
use crosscom_launcher::server;
use crosscom_launcher::settings;
use crosscom_launcher::settings::Launcher as Prefs;
use serde::Deserialize;
use serde::Serialize;

/// What the game is given as its CD key when none was set: the server doesn't check it.
const NO_CD_KEY: &str = "CROSSCOM";

pub struct Launcher {
    prefs_path: PathBuf,
    prefs: Mutex<Prefs>,
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
    let config = Config {
        server: Some(host.clone()),
        username: name.clone(),
        password: setup.password.clone(),
        cd_key: if old.cd_key.is_empty() { NO_CD_KEY.into() } else { old.cd_key },
        skip_intro_videos: setup.skip_intro,
        ..Config::default()
    };
    let (new, code, work_dir) = (setup.new_account, setup.code, dir.clone());
    let made = tauri::async_runtime::spawn_blocking(move || -> Result<bool, String> {
        server::check(&host)?;
        if new {
            server::sign_up(&host, &config.username, &config.password, &code)?;
        }
        game::install(&work_dir, &files)?;
        settings::save_client(&work_dir, &config)?;
        Ok(new)
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut prefs = state.prefs();
    prefs.game_dir = Some(dir);
    prefs.dx9 = setup.dx9;
    state.save(prefs)?;
    Ok(if made {
        format!("Account {name} made, and the game is set up.")
    } else {
        "The game is set up.".into()
    })
}

/// The game options: written into the client's settings and the launcher's.
#[tauri::command]
pub fn set_options(state: tauri::State<'_, Launcher>, skip_intro: bool, dx9: bool) -> Result<(), String> {
    let mut prefs = state.prefs();
    if let Some(dir) = prefs.game_dir.clone().filter(|d| game::is_game_dir(d)) {
        let mut config = settings::client(&dir);
        if !config.username.is_empty() {
            config.skip_intro_videos = skip_intro;
            settings::save_client(&dir, &config)?;
        }
    }
    prefs.dx9 = dx9;
    state.save(prefs)
}

/// Starts the game, bringing the client up to date first if this launcher carries a newer one.
#[tauri::command]
pub async fn play(state: tauri::State<'_, Launcher>) -> Result<String, String> {
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
        std::process::Command::new(&exe)
            .current_dir(&dir)
            .spawn()
            .map_err(|e| format!("Couldn't start {}: {e}", exe.display()))?;
        Ok(if updated {
            "Cross-Com's client was brought up to date. The game is starting.".into()
        } else {
            "The game is starting.".into()
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Puts the game's own files back (Cross-Com's client comes out of it).
#[tauri::command]
pub fn restore(state: tauri::State<'_, Launcher>) -> Result<String, String> {
    let dir = state.prefs().game_dir.filter(|d| game::is_game_dir(d)).ok_or("No game set up.")?;
    game::restore(&dir)?;
    Ok("The game's own files are back. Set up again to play on Cross-Com.".into())
}
