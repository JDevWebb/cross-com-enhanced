//! What the screens ask of the launcher: how things stand, and the setup and play, done by the
//! launcher library. Anything here may wait on the network or the disk: the window calls it off
//! its own thread (ui.rs).
//!
//! The game's logs go to the server once a game the launcher started ends (on a thread that
//! waits for it), and as the launcher starts (see `crosscom_launcher::logs`).

use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::MutexGuard;

use crosscom_client_common::config::Config;
use crosscom_launcher::client;
use crosscom_launcher::game;
use crosscom_launcher::game::Installed;
use crosscom_launcher::logs;
use crosscom_launcher::server;
use crosscom_launcher::settings;
use crosscom_launcher::settings::Launcher as Prefs;

/// What the game is given as its CD key when none was set: the server doesn't check it.
const NO_CD_KEY: &str = "CROSSCOM";
/// The launcher's release.
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");

/// Where About's links go, by their button's id; `None` for an id About doesn't have.
#[must_use]
pub fn about_link(id: &str) -> Option<&'static str> {
    Some(match id {
        "source" => "https://github.com/JDevWebb/cross-com-enhanced",
        "releases" => "https://github.com/JDevWebb/cross-com-enhanced/releases",
        "issues" => "https://github.com/JDevWebb/cross-com-enhanced/issues",
        "profile" => "https://github.com/JDevWebb",
        "sister" => "https://github.com/JDevWebb/5th-echelon-enhanced",
        "upstream" => "https://github.com/unixoide/5th-echelon",
        "phoenix" => "https://github.com/Phoenix-Network-Ltd/Ghost-Recon-Future-Soldier-Research",
        "grobackend" => "https://github.com/zeroKilo/GROBackendWV",
        "prudp" => "https://github.com/Kinnay/NintendoClients/wiki/PRUDP-Protocol",
        "kofi" => "https://ko-fi.com/jdevwebb",
        _ => return None,
    })
}

/// Opens a web address in the default browser. Only a plain `https` address: nothing a program
/// handed it could take as its own options (explorer.exe reads commas as switches).
pub fn open_url(url: &str) -> Result<(), String> {
    let plain = url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && !rest.split('/').next().unwrap_or_default().contains('@')
            && !rest.chars().any(|c| c.is_whitespace() || matches!(c, ',' | '"' | '\\' | '^' | '&' | '|' | '<' | '>'))
    });
    if !plain {
        return Err("Not a link the launcher opens.".into());
    }
    // Windows' own handler for web addresses (the default browser), not explorer.exe's command line.
    #[cfg(windows)]
    let opened = std::process::Command::new("rundll32").arg("url.dll,FileProtocolHandler").arg(url).spawn();
    #[cfg(not(windows))]
    let opened = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(url).spawn();
    opened.map(|_| ()).map_err(|e| format!("Couldn't open {url}: {e}"))
}

pub struct Launcher {
    prefs_path: PathBuf,
    prefs: Mutex<Prefs>,
    /// What the last sending of logs said, and whether logs are being sent now.
    logs: Mutex<(Option<String>, bool)>,
}

/// Cross-Com's client in the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// No game set up.
    NoGame,
    No,
    Yes,
    /// This launcher carries another version: installed when Play is pressed.
    Update,
}

/// How things stand, for every screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub game_dir: Option<PathBuf>,
    pub client: ClientState,
    /// Whether this launcher carries the client (a release build does).
    pub carries_client: bool,
    pub server: Option<String>,
    pub account: Option<String>,
    pub skip_intro: bool,
    pub dx9: bool,
    /// The in-game overlay, and the key that opens it (`F1` to `F12`).
    pub overlay: bool,
    pub overlay_key: String,
    /// Send the game's logs to the server after each game.
    pub send_logs: bool,
}

impl Info {
    /// Everything's set: Play can go.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.game_dir.is_some() && self.server.is_some() && self.account.is_some() && self.client != ClientState::No
    }
}

/// What the setup screen sends.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub game_dir: PathBuf,
    pub server: String,
    pub name: String,
    pub password: String,
    /// Make the account first (with the server's sign-up code, if it has one).
    pub new_account: bool,
    pub code: String,
    pub skip_intro: bool,
    pub dx9: bool,
}

/// Another account on the server set up.
#[derive(Debug, Clone, Default)]
pub struct Account {
    pub name: String,
    pub password: String,
    /// Make the account (with the server's sign-up code, if it has one) rather than sign in.
    pub new_account: bool,
    pub code: String,
}

/// The game options, from Settings.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub skip_intro: bool,
    pub dx9: bool,
    pub overlay: bool,
    pub overlay_key: String,
    pub send_logs: bool,
}

/// How the logs stand, for Settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogsStatus {
    /// The last report the server took, and when (Unix time).
    pub last: Option<(String, u64)>,
    /// What the last sending said.
    pub said: Option<String>,
    pub sending: bool,
}

/// A name the server takes: letters, digits, `-`, `_` and `.`, up to 32.
#[must_use]
pub fn name_ok(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// What's still missing from an account form, or `None` when it can go.
#[must_use]
pub fn account_missing(name: &str, password: &str, again: &str, new: bool) -> Option<&'static str> {
    if !name_ok(name.trim()) {
        Some("Enter a name: letters, digits, \"-\", \"_\" or \".\".")
    } else if password.is_empty() {
        Some("Enter your password.")
    } else if new && password != again {
        Some("The two passwords differ.")
    } else {
        None
    }
}

impl Launcher {
    /// The launcher's own settings in `prefs_path`; the game's folder found again when the one
    /// kept is gone.
    pub fn new(prefs_path: PathBuf) -> Self {
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

    /// Where the launcher keeps its own settings: `%APPDATA%\Cross-Com\launcher.toml`.
    #[must_use]
    pub fn default_prefs_path() -> PathBuf {
        let base = std::env::var_os("APPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
        base.join("Cross-Com").join("launcher.toml")
    }

    fn prefs(&self) -> Prefs {
        self.prefs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn save(&self, prefs: Prefs) -> Result<(), String> {
        prefs.save(&self.prefs_path)?;
        *self.prefs.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = prefs;
        Ok(())
    }

    fn logs(&self) -> MutexGuard<'_, (Option<String>, bool)> {
        self.logs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The game's folder, while it still holds the game.
    pub fn game_dir(&self) -> Option<PathBuf> {
        self.prefs().game_dir.filter(|d| game::is_game_dir(d))
    }

    pub fn info(&self) -> Info {
        let prefs = self.prefs();
        let dir = prefs.game_dir.filter(|d| game::is_game_dir(d));
        let config = dir.as_deref().map(settings::client).unwrap_or_default();
        let overlay = dir.as_deref().map(settings::overlay).unwrap_or_default();
        Info {
            client: client_state(dir.as_deref()),
            game_dir: dir,
            carries_client: client::files().is_some(),
            server: config.server.clone().filter(|s| !s.is_empty()),
            account: Some(config.username.clone()).filter(|n| !n.is_empty()),
            skip_intro: config.skip_intro_videos,
            dx9: prefs.dx9,
            overlay: overlay.on,
            overlay_key: overlay.key,
            send_logs: config.send_logs,
        }
    }

    /// Sends the game's pending logs (after moving the last run's aside, when no game has it
    /// open), unless they're being sent already; what came of it.
    pub fn send_logs(&self, exit: Option<i32>) -> String {
        let Some(dir) = self.game_dir() else {
            return "No game set up.".into();
        };
        if std::mem::replace(&mut self.logs().1, true) {
            return "Logs are being sent.".into();
        }
        let ended = logs::finish_run(&dir);
        let outcome = logs::send(&dir, RELEASE, ended.zip(exit));
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

    pub fn logs_status(&self) -> LogsStatus {
        let (said, sending) = self.logs().clone();
        LogsStatus {
            last: self.prefs().last_logs.map(|l| (l.id, l.at)),
            said,
            sending,
        }
    }

    /// How the server set up is: its version, players online and news. `None` for a server that
    /// answers but is older than the launcher's API; an error if it doesn't answer, or is
    /// another game's.
    pub fn server_info(&self) -> Result<Option<server::Info>, String> {
        let dir = self.game_dir().ok_or("No game set up.")?;
        let host = settings::client(&dir).server.filter(|s| !s.is_empty()).ok_or("No server set up.")?;
        server::info(&host)
    }

    /// Sets everything up in one go: checks the game and the server, makes the account if asked,
    /// installs the client and writes its settings. What to tell the player.
    pub fn setup(&self, setup: Setup) -> Result<String, String> {
        let Some(files) = client::files() else {
            return Err("This launcher was built without the client; download a release.".into());
        };
        let dir = setup.game_dir;
        if !game::is_game_dir(&dir) {
            return Err("That folder doesn't hold the game.".into());
        }
        let name = setup.name.trim().to_owned();
        if name.is_empty() || setup.password.is_empty() {
            return Err("Fill in your name and password.".into());
        }
        let host = server::host(&setup.server);
        if host.is_empty() {
            return Err("Enter the server's address.".into());
        }
        let old = settings::client(&dir);
        let overlay = settings::overlay(&dir);
        let config = Config {
            server: Some(host.clone()),
            username: name.clone(),
            password: setup.password,
            cd_key: if old.cd_key.is_empty() { NO_CD_KEY.into() } else { old.cd_key.clone() },
            skip_intro_videos: setup.skip_intro,
            ..old
        };
        server::check(&host)?;
        let (checked, sent) = account(&host, &config.username, &config.password, setup.new_account, &setup.code)?;
        game::install(&dir, &files)?;
        settings::save_client(&dir, &config, &overlay)?;
        let mut prefs = self.prefs();
        prefs.game_dir = Some(dir);
        prefs.dx9 = setup.dx9;
        self.save(prefs)?;
        let said = match (setup.new_account, checked) {
            (true, _) => format!("Account {name} made, and the game is set up."),
            (false, true) => format!("Signed in as {name}, and the game is set up."),
            (false, false) => "The game is set up. This server can't check passwords here; the game will.".into(),
        };
        Ok(said + unencrypted_note(sent))
    }

    /// Another account on the server set up: signs in to it (or makes it), then the game uses it.
    pub fn set_account(&self, account: Account) -> Result<String, String> {
        let dir = self.game_dir().ok_or("Set the game up first.")?;
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
        let (checked, sent) = self::account(&host, &config.username, &config.password, new, &account.code)?;
        settings::save_client(&dir, &config, &overlay)?;
        let said = match (new, checked) {
            (true, _) => format!("Account {name} made. The game signs in with it."),
            (false, true) => format!("Signed in as {name}. The game signs in with it."),
            (false, false) => format!("The game signs in as {name} now. This server can't check passwords here; the game will."),
        };
        Ok(said + unencrypted_note(sent))
    }

    /// The game options: written into the client's settings and the launcher's.
    pub fn set_options(&self, options: Options) -> Result<(), String> {
        let mut prefs = self.prefs();
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
        self.save(prefs)
    }

    /// Starts the game, bringing the client up to date first if this launcher carries a newer
    /// one: the game, and what to tell the player.
    pub fn play(&self) -> Result<(std::process::Child, String), String> {
        let prefs = self.prefs();
        let dir = prefs.game_dir.clone().filter(|d| game::is_game_dir(d)).ok_or("Set the game up first.")?;
        let exe = dir.join(if prefs.dx9 { game::EXE_DX9 } else { game::EXE_DX11 });
        let mut updated = false;
        if let Some(files) = client::files() {
            if game::installed(&dir, &files) != Installed::Yes {
                game::install(&dir, &files)?;
                updated = true;
            }
        }
        let child = std::process::Command::new(&exe)
            .current_dir(&dir)
            .spawn()
            .map_err(|e| format!("Couldn't start {}: {e}", exe.display()))?;
        let said = if updated {
            "Cross-Com's client was brought up to date. The game is starting."
        } else {
            "The game is starting."
        };
        Ok((child, said.into()))
    }

    /// Opens the folder the game's logs are kept in.
    pub fn open_logs(&self) -> Result<(), String> {
        let dir = self.game_dir().ok_or("No game set up.")?;
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
    pub fn restore(&self) -> Result<String, String> {
        let dir = self.game_dir().ok_or("No game set up.")?;
        game::restore(&dir)?;
        Ok("The game's own files are back. Set up again to play on Cross-Com.".into())
    }
}

fn client_state(dir: Option<&Path>) -> ClientState {
    match (dir, client::files()) {
        (None, _) => ClientState::NoGame,
        (Some(dir), Some(files)) => match game::installed(dir, &files) {
            Installed::No => ClientState::No,
            Installed::Yes => ClientState::Yes,
            Installed::Other => ClientState::Update,
        },
        (Some(dir), None) if game::CLIENT_DLLS.iter().all(|d| dir.join(format!("{d}.orig")).is_file()) => ClientState::Yes,
        (Some(_), None) => ClientState::No,
    }
}

/// The game's folder, found on its own, if it can be.
#[must_use]
pub fn find_game() -> Option<PathBuf> {
    game::find(&game::candidates())
}

/// Whether `dir` (chosen by the player) holds the game.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn check_game(dir: &Path) -> Result<(), String> {
    if game::is_game_dir(dir) {
        Ok(())
    } else {
        Err("That folder doesn't hold the game: choose the one with \"Future Soldier DX11.exe\" in it.".into())
    }
}

/// Signs in to an account (`new` false) or makes one. Whether the server checked it (an older
/// one can't), and how the password went.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_opens_only_its_own_plain_links() {
        for id in ["source", "releases", "issues", "profile", "sister", "upstream", "phoenix", "grobackend", "prudp", "kofi"] {
            let url = about_link(id).unwrap();
            assert!(url.starts_with("https://"), "{id}");
            assert!(!url.contains(',') && !url.contains(' '), "{id}");
        }
        assert_eq!(about_link("file"), None);
        for bad in [
            "http://example.org",
            "https://",
            "https://user@example.org",
            "https://a.org/x,y",
            "file:///c:/x",
            "https://a.org/\\x",
        ] {
            assert!(open_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn checks_the_account_form() {
        assert_eq!(account_missing("Kozak", "pw", "", false), None);
        assert_eq!(account_missing("Kozak", "pw", "pw", true), None);
        assert!(account_missing("Kozak", "pw", "pW", true).unwrap().contains("differ"));
        assert!(account_missing("Ko zak", "pw", "pw", true).unwrap().contains("name"));
        assert!(account_missing("", "pw", "pw", false).unwrap().contains("name"));
        assert!(account_missing("Kozak", "", "", false).unwrap().contains("password"));
        assert!(name_ok("a.b-c_9") && !name_ok(&"x".repeat(33)));
    }

    #[test]
    fn ready_once_everything_is_set() {
        let info = Info {
            game_dir: Some("C:/Games/GRFS".into()),
            client: ClientState::Yes,
            carries_client: true,
            server: Some(server::COMMUNITY.into()),
            account: Some("Kozak".into()),
            skip_intro: true,
            dx9: false,
            overlay: true,
            overlay_key: "F5".into(),
            send_logs: true,
        };
        assert!(info.ready());
        assert!(
            Info {
                client: ClientState::Update,
                ..info.clone()
            }
            .ready(),
            "Play brings it up to date"
        );
        assert!(!Info {
            client: ClientState::No,
            ..info.clone()
        }
        .ready());
        assert!(!Info { account: None, ..info }.ready());
    }
}
