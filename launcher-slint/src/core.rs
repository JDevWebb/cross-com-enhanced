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
use crosscom_client_common::profiles;
use crosscom_launcher::client;
use crosscom_launcher::dlc;
use crosscom_launcher::game;
use crosscom_launcher::game::Installed;
use crosscom_launcher::identity;
use crosscom_launcher::logs;
use crosscom_launcher::server;
use crosscom_launcher::settings;
use crosscom_launcher::settings::Launcher as Prefs;

/// What the game is given as its CD key when none was set: the server doesn't check it.
const NO_CD_KEY: &str = "CROSSCOM";
/// The launcher's release.
pub const RELEASE: &str = env!("CROSSCOM_RELEASE");

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
    // Windows' own handler for web addresses (the default browser), not explorer.exe's command line;
    // rundll32 from Windows' system folder, not whatever the bare name finds first.
    #[cfg(windows)]
    let opened = crosscom_launcher::windows_dirs::system_program("rundll32.exe")
        .map_err(std::io::Error::other)
        .and_then(|rundll32| std::process::Command::new(rundll32).arg("url.dll,FileProtocolHandler").arg(url).spawn());
    #[cfg(not(windows))]
    let opened = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(url).spawn();
    opened.map(|_| ()).map_err(|e| format!("Couldn't open {url}: {e}"))
}

pub struct Launcher {
    prefs_path: PathBuf,
    prefs: Mutex<Prefs>,
    /// What the last sending of logs said, and whether logs are being sent now.
    logs: Mutex<(Option<String>, bool)>,
    /// The server hosted from here, for friends (Host).
    pub host: crate::host::Host,
}

/// The address the host's own game plays on its own server at: this machine's.
pub const HOSTED_HERE: &str = "127.0.0.1";

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

/// The fewest players a Quick Match starts with unless the player chooses otherwise (the game's own
/// 8, or 6, is rarely reached on a small community's server): 2, and squad matches 1 a side.
pub const DEFAULT_MIN_PLAYERS: u8 = 2;

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
    /// Ask the router to forward the game's port while it runs.
    pub port_forwarding: bool,
    /// The fewest players a Quick Match starts with (2 to 12); a squad match, half that a side.
    pub min_players: u8,
    /// The DLC packs, and how each stands in the game's folder (empty with no game).
    pub dlc: Vec<(dlc::Pack, dlc::State)>,
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
    /// The name for a new account (with the server's sign-up code, if it has one), when the
    /// player's identity has none on the server yet.
    pub name: String,
    pub code: String,
    pub skip_intro: bool,
    pub dx9: bool,
    /// The in-game overlay, sending the game's logs, and asking the router to forward the game's
    /// port (each on unless the player turns it off).
    pub overlay: bool,
    pub send_logs: bool,
    pub port_forwarding: bool,
}

/// What using an identity from another PC came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Imported {
    /// It would replace this PC's own, a different one: the two (short), to ask first.
    Confirm { current: String, new: String },
    /// It's in use: what to tell the player.
    Done(String),
}

/// The game options, from Settings.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub skip_intro: bool,
    pub dx9: bool,
    pub overlay: bool,
    pub overlay_key: String,
    pub send_logs: bool,
    pub port_forwarding: bool,
    pub min_players: u8,
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

/// What's wrong with a new account's name, or `None` when the server will take it.
#[must_use]
pub fn name_missing(name: &str) -> Option<&'static str> {
    (!name_ok(name.trim())).then_some("Enter a name: letters, digits, \"-\", \"_\" or \".\".")
}

impl Launcher {
    /// The launcher's own settings in `prefs_path`; the game's folder found again when the one
    /// kept is gone.
    pub fn new(prefs_path: PathBuf) -> Self {
        let mut prefs = Prefs::load(&prefs_path);
        if prefs.game_dir.as_deref().is_none_or(|d| !game::is_game_dir(d)) {
            prefs.game_dir = game::find(&game::candidates());
        }
        let host = crate::host::Host::new(prefs_path.parent().unwrap_or_else(|| Path::new(".")));
        Self {
            prefs_path,
            prefs: Mutex::new(prefs),
            logs: Mutex::new((None, false)),
            host,
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

    /// The launcher's own folder: its settings, and the player's identity.
    fn dir(&self) -> PathBuf {
        self.prefs_path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    }

    /// The player's identity, made the first time it's needed.
    pub fn identity(&self) -> Result<identity::Identity, String> {
        identity::load_or_create(&self.dir())
    }

    /// The identity as Settings shows it (`ABCD-EFGH`), or why it can't be read.
    pub fn identity_short(&self) -> Result<String, String> {
        self.identity().map(|id| identity::short(&id))
    }

    /// The identity as text, to move it to another PC.
    pub fn identity_export(&self) -> Result<String, String> {
        self.identity().map(|id| identity::export(&id))
    }

    /// The name of the identity's account on `host`, if it has one there (Setup's account step).
    pub fn find_account(&self, host: &str) -> Result<Option<String>, String> {
        let identity = self.identity()?;
        Ok(server::key_login(host, &identity)?.map(|a| a.name))
    }

    /// Uses the identity pasted from another PC (`text`, as copied there). Replacing a different
    /// one asks first, unless `confirmed`. The one replaced is kept beside it; the account on the
    /// server set up is then the new identity's.
    pub fn use_identity(&self, text: &str, confirmed: bool) -> Result<Imported, String> {
        let new = identity::import(text)?;
        let dir = self.dir();
        let current = identity::load(&dir).ok().flatten().filter(|c| c.global_id() != new.global_id());
        if let (Some(current), false) = (&current, confirmed) {
            return Ok(Imported::Confirm {
                current: identity::short(current),
                new: identity::short(&new),
            });
        }
        let kept = identity::replace(&dir, &new)?;
        let mut said = format!("This PC uses the identity {} now.", identity::short(&new));
        if let Some(kept) = kept {
            said += &format!(" The one it replaced is kept in {}.", kept.display());
        }
        Ok(Imported::Done(said + &self.sign_in_again(&new)))
    }

    /// After the identity changed: the game's account on the server set up becomes the new
    /// identity's (none when it has none there). What to add to what the player is told.
    fn sign_in_again(&self, identity: &identity::Identity) -> String {
        let Some(dir) = self.game_dir() else { return String::new() };
        let config = settings::client(&dir);
        let Some(host) = config.server.clone().filter(|s| !s.is_empty()) else {
            return String::new();
        };
        let (account, said) = match server::key_login(&host, identity) {
            Ok(Some(account)) => (Some((account.name.clone(), account.token)), format!(" Signed in to {host} as {}.", account.name)),
            Ok(None) => (None, format!(" It has no account on {host} yet: Setup makes one.")),
            Err(e) => return format!(" Couldn't sign in to {host} with it ({e}); Setup tries again."),
        };
        let (username, password) = account.unwrap_or_default();
        match settings::save_client(&dir, &Config { username, password, ..config }, &settings::overlay(&dir)) {
            Ok(()) => said,
            Err(e) => format!(" {e}"),
        }
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
        let dlc = dir.as_deref().map(dlc::scan).unwrap_or_default();
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
            port_forwarding: config.port_forwarding,
            min_players: config.quick_match_min_players.unwrap_or(DEFAULT_MIN_PLAYERS),
            dlc,
        }
    }

    /// The connection test against the server set up (`crosscom_client_net::check`): how well
    /// other players can reach this PC's game. Takes a few seconds.
    pub fn test_connection(&self) -> Result<crosscom_client_net::check::Report, String> {
        let dir = self.game_dir().ok_or("No game set up.")?;
        let server = settings::client(&dir).server.filter(|s| !s.is_empty()).ok_or("No server set up.")?;
        // The test's packet comes in to the launcher: Windows Firewall has to let it (Setup adds
        // the rule; a launcher that moved asks again).
        use crosscom_launcher::firewall::Outcome;
        let unchecked = match crosscom_launcher::firewall::ensure_launcher() {
            Outcome::Present | Outcome::Added => None,
            Outcome::Declined => Some("Windows Firewall wasn't allowed to let the test in (the game checks it itself when you play)".to_owned()),
            Outcome::Failed(why) => Some(format!("the launcher couldn't be let through Windows Firewall ({why})")),
        };
        Ok(crosscom_client_net::check::run_with(&server, unchecked))
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

    /// Sets everything up in one go: checks the game and the server, signs in to the identity's
    /// account there (or makes it), installs the client and writes its settings. What to tell the
    /// player.
    pub fn setup(&self, setup: Setup) -> Result<String, String> {
        let Some(files) = client::files() else {
            return Err("This launcher was built without the client; download a release.".into());
        };
        let dir = setup.game_dir;
        if !game::is_game_dir(&dir) {
            return Err("That folder doesn't hold the game.".into());
        }
        // A host's join code pins the server's own HTTPS certificate.
        let (_, fingerprint) = server::join_code(&setup.server)?;
        let host = server::chosen(&setup.server)?;
        if host.is_empty() {
            return Err("Enter the server's address.".into());
        }
        server::check(&host)?;
        let identity = self.identity()?;
        let (account, made) = account(&host, &identity, &setup.name, &setup.code)?;
        let name = account.name.clone();
        let old = settings::client(&dir);
        let overlay = settings::Overlay {
            on: setup.overlay,
            ..settings::overlay(&dir)
        };
        let config = Config {
            server: Some(host),
            server_certificate: fingerprint.as_ref().map(crosscom_client_common::http::fingerprint_text),
            username: account.name,
            // The game signs in with the game token, as its password.
            password: account.token,
            cd_key: if old.cd_key.is_empty() { NO_CD_KEY.into() } else { old.cd_key.clone() },
            skip_intro_videos: setup.skip_intro,
            send_logs: setup.send_logs,
            port_forwarding: setup.port_forwarding,
            quick_match_min_players: Some(old.quick_match_min_players.unwrap_or(DEFAULT_MIN_PLAYERS)),
            ..old
        };
        game::install(&dir, &files)?;
        settings::save_client(&dir, &config, &overlay)?;
        // Before the game first hosts: Windows' own prompt takes the focus from the fullscreen game.
        let wall = firewall_note(crosscom_launcher::firewall::ensure(&dir));
        let mut prefs = self.prefs();
        prefs.game_dir = Some(dir);
        prefs.dx9 = setup.dx9;
        self.save(prefs)?;
        let said = if made {
            format!("Account {name} made, and the game is set up.")
        } else {
            format!("Signed in as {name}, and the game is set up.")
        };
        Ok(said + &wall)
    }

    /// The game options: written into the client's settings and the launcher's.
    pub fn set_options(&self, options: Options) -> Result<(), String> {
        let mut prefs = self.prefs();
        if let Some(dir) = prefs.game_dir.clone().filter(|d| game::is_game_dir(d)) {
            let mut config = settings::client(&dir);
            if !config.username.is_empty() {
                config.skip_intro_videos = options.skip_intro;
                config.send_logs = options.send_logs;
                config.port_forwarding = options.port_forwarding;
                config.quick_match_min_players = Some(options.min_players.clamp(crosscom_client_common::config::MIN_QUICK_MATCH_PLAYERS, 12));
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
        // A game set up by an older launcher has no minimum: it gets the default before it starts.
        let config = settings::client(&dir);
        if config.quick_match_min_players.is_none() && !config.username.is_empty() {
            let config = Config {
                quick_match_min_players: Some(DEFAULT_MIN_PLAYERS),
                ..config
            };
            settings::save_client(&dir, &config, &settings::overlay(&dir))?;
        }
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

    /// Whether the server hosted here lets anyone on the internet join (with a join code), or only
    /// this PC's networks.
    #[must_use]
    pub fn host_internet(&self) -> bool {
        self.prefs().host_internet
    }

    /// Chooses who can join the server hosted here (from its next start).
    pub fn set_host_internet(&self, internet: bool) -> Result<(), String> {
        let mut prefs = self.prefs();
        prefs.host_internet = internet;
        self.save(prefs)
    }

    /// Whether the game plays on the server hosted from here.
    #[must_use]
    pub fn plays_on_hosted(&self) -> bool {
        self.game_dir().is_some_and(|dir| settings::client(&dir).server.as_deref() == Some(HOSTED_HERE))
    }

    /// Has the game play on the server hosted from here (at this machine's address, with the
    /// server's own certificate), with the player's identity's account there, made with the
    /// player's name when it has none yet. What to tell the player.
    pub fn play_on_hosted(&self) -> Result<String, String> {
        let dir = self.game_dir().ok_or("Set the game up first: Play opens the setup.")?;
        let fingerprint = self.host.fingerprint().ok_or("Start the server first.")?;
        crosscom_client_common::http::pin(HOSTED_HERE, Some(fingerprint));
        let identity = self.identity()?;
        let old = settings::client(&dir);
        let name = if name_ok(&old.username) { old.username.clone() } else { "Host".to_owned() };
        let (account, made) = account(HOSTED_HERE, &identity, &name, "")?;
        let said = if made {
            format!("Your game plays on your server now, as {} (a new account there).", account.name)
        } else {
            format!("Your game plays on your server now, as {}.", account.name)
        };
        let config = Config {
            server: Some(HOSTED_HERE.into()),
            server_certificate: Some(crosscom_client_common::http::fingerprint_text(&fingerprint)),
            username: account.name,
            password: account.token,
            cd_key: if old.cd_key.is_empty() { NO_CD_KEY.into() } else { old.cd_key.clone() },
            ..old
        };
        settings::save_client(&dir, &config, &settings::overlay(&dir))?;
        Ok(said)
    }

    /// The game's profiles on this PC (`crosscom_client_common::profiles`), the most recently
    /// played first, with the one the game plays now marked.
    pub fn profiles(&self) -> Result<Vec<ProfileView>, String> {
        let root = profiles::root().ok_or("The game's profiles are in your Documents folder, which this needs Windows to find.")?;
        let current = self.game_dir().map(|dir| settings::client(&dir).username).unwrap_or_default();
        Ok(profiles::list(&root)
            .into_iter()
            .map(|p| ProfileView {
                current: p.account == current,
                played: p.last_played.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()),
                campaign: p.campaign,
                name: p.name.unwrap_or_else(|| p.account.clone()),
                account: p.account,
            })
            .collect())
    }

    /// Makes the profile played as `from` the one the game plays now, named after the account (the
    /// one it replaces kept in the launcher's folder). What to tell the player.
    pub fn use_profile(&self, from: &str) -> Result<String, String> {
        let root = profiles::root().ok_or("This needs Windows.")?;
        let dir = self.game_dir().ok_or("Set the game up first.")?;
        let account = settings::client(&dir).username;
        if account.is_empty() {
            return Err("Set the game up first: there's no account yet.".into());
        }
        profiles::use_for(&root, from, &account, &self.dir().join(PROFILE_BACKUPS)).map_err(|e| format!("Couldn't use that profile: {e}"))?;
        Ok(format!(
            "The game plays {from}'s profile now, as {account}. Its own was kept in the launcher's folder, in {PROFILE_BACKUPS}."
        ))
    }

    /// Copies the game's profiles and the client's saves into a new folder in `to`. What to tell
    /// the player.
    pub fn back_up_saves(&self, to: &Path) -> Result<String, String> {
        let root = profiles::root().ok_or("This needs Windows.")?;
        let backup = to.join(format!("Cross-Com saves {}", profiles::stamp()));
        copy_saves(&root, &self.dir().join(SAVES), &backup)?;
        Ok(format!("Backed up to {}.", backup.display()))
    }

    /// Puts back a backup made by [`Launcher::back_up_saves`] (the profiles and saves there now
    /// kept in the launcher's folder first). What to tell the player.
    pub fn restore_saves(&self, from: &Path) -> Result<String, String> {
        let root = profiles::root().ok_or("This needs Windows.")?;
        if !from.join(BACKUP_PROFILES).is_dir() {
            return Err(format!(
                "That isn't a Cross-Com saves backup: choose the folder named \"Cross-Com saves …\" (it has a {BACKUP_PROFILES} folder in it)."
            ));
        }
        let kept = self.dir().join(PROFILE_BACKUPS).join(format!("before restoring {}", profiles::stamp()));
        copy_saves(&root, &self.dir().join(SAVES), &kept)?;
        profiles::copy_dir(&from.join(BACKUP_PROFILES), &root).map_err(|e| format!("Couldn't put the profiles back: {e}"))?;
        if from.join(BACKUP_SAVES).is_dir() {
            profiles::copy_dir(&from.join(BACKUP_SAVES), &self.dir().join(SAVES)).map_err(|e| format!("Couldn't put the saves back: {e}"))?;
        }
        Ok(format!("Restored. What was there before is kept in the launcher's folder, in {PROFILE_BACKUPS}."))
    }

    /// Opens the game's profiles folder.
    pub fn open_profiles(&self) -> Result<(), String> {
        let root = profiles::root().ok_or("This needs Windows.")?;
        std::fs::create_dir_all(&root).map_err(|e| format!("Couldn't make {}: {e}", root.display()))?;
        open_folder(&root)
    }

    /// Opens the folder the game's logs are kept in.
    pub fn open_logs(&self) -> Result<(), String> {
        let dir = self.game_dir().ok_or("No game set up.")?;
        let logs = dir.join(crosscom_client_common::runs::DIR);
        std::fs::create_dir_all(&logs).map_err(|e| format!("Couldn't make {}: {e}", logs.display()))?;
        // explorer from the Windows folder, not whatever the bare name finds first.
        #[cfg(windows)]
        let opener = crosscom_launcher::windows_dirs::windows_program("explorer.exe")?;
        #[cfg(not(windows))]
        let opener = std::path::PathBuf::from("xdg-open");
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
        Ok("The game's own files are back. To play on Cross-Com again, go to Play: the setup shows there.".into())
    }
}

/// The client's saves (`crosscom_client_common::saves`), in the launcher's folder.
const SAVES: &str = "Saves";
/// Where profiles replaced or restored over are kept, in the launcher's folder.
pub const PROFILE_BACKUPS: &str = "Profile backups";
/// A backup's folders: the game's profiles, and the client's saves.
const BACKUP_PROFILES: &str = "profiles";
const BACKUP_SAVES: &str = "Cross-Com saves";

/// A game profile, as Settings › Saves lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileView {
    /// The account it was played with (its folder).
    pub account: String,
    /// Its name in the game.
    pub name: String,
    /// When it was last played (Unix time).
    pub played: Option<u64>,
    pub campaign: bool,
    /// The game plays it now.
    pub current: bool,
}

/// Copies the game's profiles in `profiles` and the client's saves in `saves` into `to`.
fn copy_saves(profiles: &Path, saves: &Path, to: &Path) -> Result<(), String> {
    if profiles.is_dir() {
        profiles::copy_dir(profiles, &to.join(BACKUP_PROFILES)).map_err(|e| format!("Couldn't copy the profiles: {e}"))?;
    }
    if saves.is_dir() {
        profiles::copy_dir(saves, &to.join(BACKUP_SAVES)).map_err(|e| format!("Couldn't copy the saves: {e}"))?;
    }
    Ok(())
}

/// Opens `dir` in File Explorer.
fn open_folder(dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let opener = crosscom_launcher::windows_dirs::windows_program("explorer.exe")?;
    #[cfg(not(windows))]
    let opener = std::path::PathBuf::from("xdg-open");
    std::process::Command::new(opener)
        .arg(dir)
        .spawn()
        .map_err(|e| format!("Couldn't open {}: {e}", dir.display()))?;
    Ok(())
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

/// Signs `identity` in to its account on `host`, or, when it has none there, makes the account
/// `name` (with the sign-up `code`, if the server has one): the account, with the game token the
/// game signs in with, and whether it was made.
fn account(host: &str, identity: &identity::Identity, name: &str, code: &str) -> Result<(server::Account, bool), String> {
    if let Some(account) = server::key_login(host, identity)? {
        return Ok((account, false));
    }
    if let Some(missing) = name_missing(name) {
        return Err(missing.into());
    }
    server::sign_up(host, identity, name, code).map(|account| (account, true))
}

/// One line of Play's checklist: what's checked, how it stands ("good", "warn", "bad" or
/// "muted"), and what it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub label: &'static str,
    pub value: String,
    pub tone: &'static str,
    pub detail: String,
    /// The button that fixes it ("" for none).
    pub action: &'static str,
}

/// Play's checklist for the game in `dir`: the firewall rules, the port forward, and how other
/// players reached the last game (what the client wrote, see `crosscom_client_net::keep`).
/// Asks Windows about the firewall rules: off the window's thread.
#[must_use]
pub fn checklist(dir: &Path, port_forwarding: bool, now: u64) -> Vec<Check> {
    let firewall = if !crosscom_launcher::firewall::game_allowed(dir) {
        Check {
            label: "Firewall",
            value: "Rules missing".into(),
            tone: "warn",
            detail: "Windows may stop other players reaching your game. Allow adds the rules (Windows asks first).".into(),
            action: "Allow",
        }
    } else {
        Check {
            label: "Firewall",
            value: "Game allowed".into(),
            tone: "good",
            detail: "Windows lets other players reach your game.".into(),
            action: "",
        }
    };
    let forwarding = if port_forwarding {
        Check {
            label: "Port forwarding",
            value: "On".into(),
            tone: "good",
            detail: "Asked of your router (UPnP, PCP or NAT-PMP) while the game runs.".into(),
            action: "",
        }
    } else {
        Check {
            label: "Port forwarding",
            value: "Off".into(),
            tone: "muted",
            detail: "Turned off in Settings › Network.".into(),
            action: "",
        }
    };
    let file = std::fs::read(dir.join(crosscom_client_net::keep::STATUS_FILE)).ok();
    let last = file.and_then(|f| serde_json::from_slice::<crosscom_client_net::keep::Connection>(&f).ok());
    vec![firewall, forwarding, connection_check(last.as_ref(), now)]
}

/// The checklist's line for the last game's connection.
fn connection_check(last: Option<&crosscom_client_net::keep::Connection>, now: u64) -> Check {
    use crosscom_client_net::keep::Reachable;
    let label = "Last game";
    let Some(c) = last.filter(|c| c.reachable() != Reachable::Checking) else {
        return Check {
            label,
            value: "Not checked yet".into(),
            tone: "muted",
            detail: "The server checks who can reach your game during your next game.".into(),
            action: "",
        };
    };
    let r = c.reachable();
    let tone = match r {
        Reachable::Open | Reachable::Forwarded => "good",
        Reachable::Moderate => "warn",
        _ => "bad",
    };
    Check {
        label,
        value: format!("{} · {}", r.title(), ago(now.saturating_sub(c.at))),
        tone,
        detail: r.advice().into(),
        action: "",
    }
}

/// "5 min ago".
fn ago(secs: u64) -> String {
    match secs {
        0..=119 => "just now".into(),
        120..=7199 => format!("{} min ago", secs / 60),
        7200..=172_799 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// Lets the game in `dir` (and this launcher's connection test) through Windows Firewall, asking
/// Windows for permission: what Play's Firewall card's Allow does. What to tell the player.
pub fn allow_firewall(dir: &Path) -> String {
    use crosscom_launcher::firewall::Outcome;
    match crosscom_launcher::firewall::ensure(dir) {
        Outcome::Present | Outcome::Added => "The game is let through Windows Firewall.".into(),
        Outcome::Declined => "Windows Firewall wasn't changed: Windows asks again the first time you play online.".into(),
        Outcome::Failed(why) => format!("Couldn't let the game through Windows Firewall: {why}"),
    }
}

/// Whether finishing Setup in `dir` will ask Windows for permission (the firewall rules aren't
/// all there), so the launcher says why first.
#[must_use]
pub fn firewall_would_ask(dir: &Path) -> bool {
    game::is_game_dir(dir) && crosscom_launcher::firewall::ensure_would_ask(dir)
}

/// Whether the connection test will ask Windows for permission (the launcher's rule isn't there).
#[must_use]
pub fn launcher_firewall_would_ask() -> bool {
    crosscom_launcher::firewall::ensure_launcher_would_ask()
}

/// What to add to setup's message about the firewall rules.
fn firewall_note(outcome: crosscom_launcher::firewall::Outcome) -> String {
    use crosscom_launcher::firewall::Outcome;
    match outcome {
        Outcome::Present | Outcome::Added => String::new(),
        Outcome::Declined => " The game wasn't let through Windows Firewall: when Windows asks, the first time you play online, allow it.".into(),
        Outcome::Failed(why) => format!(" The game couldn't be let through Windows Firewall ({why}): when Windows asks, the first time you play online, allow it."),
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
    fn checks_a_new_accounts_name() {
        assert_eq!(name_missing("Kozak"), None);
        assert_eq!(name_missing(" Kozak "), None, "trimmed, as it's sent");
        assert!(name_missing("Ko zak").unwrap().contains("name"));
        assert!(name_missing("").unwrap().contains("name"));
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
            min_players: DEFAULT_MIN_PLAYERS,
            send_logs: true,
            port_forwarding: true,
            dlc: Vec::new(),
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
        assert!(!Info { account: None, ..info.clone() }.ready());
        let no_packs = dlc::PACKS.iter().map(|&p| (p, dlc::State::Missing)).collect();
        assert!(Info { dlc: no_packs, ..info }.ready(), "missing packs only warn: Play still goes");
    }
}

#[cfg(test)]
mod checklist_tests {
    use crosscom_client_net::keep::Connection;

    use super::*;

    #[test]
    fn the_last_games_connection() {
        assert_eq!(connection_check(None, 1000).value, "Not checked yet");
        let open = Connection {
            nat: "consistent".into(),
            forward: Some("UPnP 203.0.113.7:2346".into()),
            open: Some(true),
            at: 1000,
            beside: false,
        };
        let c = connection_check(Some(&open), 1000 + 600);
        assert_eq!((c.value.as_str(), c.tone), ("Open · 10 min ago", "good"));
        let strict = Connection {
            nat: "symmetric".into(),
            forward: None,
            open: Some(false),
            at: 0,
            beside: false,
        };
        assert_eq!(connection_check(Some(&strict), 3 * 86400).tone, "bad");
        let unchecked = Connection { open: None, ..strict };
        assert_eq!(connection_check(Some(&unchecked), 10).value, "Not checked yet");
        // Hosting from the launcher: forwarded, which the server on this network can't check.
        let hosting = Connection {
            nat: "consistent".into(),
            forward: Some("UPnP 203.0.113.7:2346".into()),
            open: None,
            at: 1000,
            beside: true,
        };
        let c = connection_check(Some(&hosting), 1000 + 60);
        assert_eq!((c.value.as_str(), c.tone), ("Forwarded · just now", "good"));
        assert!(c.detail.contains("a friend joining is the test"), "{}", c.detail);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn another_pcs_identity_asks_before_replacing_this_ones() {
        let dir = std::env::temp_dir().join(format!("cc-identity-test-{}", std::process::id()));
        let l = Launcher::new(dir.join("launcher.toml"));
        let mine = l.identity_short().unwrap();
        assert_eq!(l.identity_short().unwrap(), mine, "made once");
        let same = l.identity_export().unwrap();
        assert!(matches!(l.use_identity(&same, false), Ok(Imported::Done(_))), "the same one: nothing to ask");

        let other = identity::Identity::generate();
        let text = identity::export(&other);
        let asked = l.use_identity(&text, false).unwrap();
        assert_eq!(
            asked,
            Imported::Confirm {
                current: mine.clone(),
                new: identity::short(&other)
            }
        );
        assert_eq!(l.identity_short().unwrap(), mine, "nothing changed before it's confirmed");
        let Imported::Done(said) = l.use_identity(&text, true).unwrap() else { panic!("used") };
        assert!(said.contains(&identity::short(&other)) && said.contains("kept"), "{said}");
        assert_eq!(l.identity_short().unwrap(), identity::short(&other));
        assert!(l.use_identity("my password", false).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
