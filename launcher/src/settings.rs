//! What the launcher writes: the client's `crosscom.toml` in the game's folder (the server, the
//! account the game signs in with, the in-game overlay and whether the logs are sent), and its
//! own choices in the user's settings folder.

use std::path::Path;
use std::path::PathBuf;

use crosscom_client_common::config::Config;
use crosscom_client_common::config::FILE_NAME;
use serde::Deserialize;
use serde::Serialize;

/// The launcher's own choices, kept between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Launcher {
    pub game_dir: Option<PathBuf>,
    /// Start the DirectX 9 version instead of DirectX 11.
    pub dx9: bool,
    /// The last logs the server took.
    pub last_logs: Option<LastLogs>,
    /// Who can join the server hosted here: anyone on the internet with a join code too (the
    /// router asked to forward its ports), or only this PC's networks (a LAN, a VPN).
    pub host_internet: bool,
}

/// The last logs sent: the server's report ID, and when (Unix time).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastLogs {
    pub id: String,
    pub at: u64,
}

impl Launcher {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("couldn't save the launcher's settings: {e}"))
    }
}

/// The in-game overlay's settings, top-level keys of `crosscom.toml`: `overlay = true` and
/// `overlay_key = "F5"`. The client reads them; they're kept apart from [`Config`] here so the
/// launcher reads and writes them whether or not the client's settings know them yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub on: bool,
    /// `F1` to `F12`.
    pub key: String,
}

const OVERLAY: &str = "overlay";
const OVERLAY_KEY: &str = "overlay_key";
/// The keys the overlay can open on.
pub const OVERLAY_KEYS: [&str; 12] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];

impl Default for Overlay {
    fn default() -> Self {
        Self { on: true, key: "F5".into() }
    }
}

/// `crosscom.toml` in `game` as a table, if it reads.
fn table(game: &Path) -> Option<toml::Table> {
    toml::from_str(&std::fs::read_to_string(game.join(FILE_NAME)).ok()?).ok()
}

/// The client's settings in `game`, or the defaults when there are none (or they don't read).
#[must_use]
pub fn client(game: &Path) -> Config {
    let Some(mut table) = table(game) else {
        return Config::default();
    };
    table.remove(OVERLAY);
    table.remove(OVERLAY_KEY);
    toml::to_string(&table).ok().and_then(|text| Config::parse(&text).ok()).unwrap_or_default()
}

/// The overlay's settings in `game`; the defaults for any not set (or not valid).
#[must_use]
pub fn overlay(game: &Path) -> Overlay {
    let table = table(game).unwrap_or_default();
    let default = Overlay::default();
    Overlay {
        on: table.get(OVERLAY).and_then(toml::Value::as_bool).unwrap_or(default.on),
        key: table
            .get(OVERLAY_KEY)
            .and_then(toml::Value::as_str)
            .and_then(|k| OVERLAY_KEYS.iter().find(|o| o.eq_ignore_ascii_case(k)))
            .map_or(default.key, |k| (*k).to_owned()),
    }
}

/// Writes the client's settings into `game`, checked as the client will read them.
pub fn save_client(game: &Path, config: &Config, overlay: &Overlay) -> Result<(), String> {
    if !OVERLAY_KEYS.contains(&overlay.key.as_str()) {
        return Err(format!("the overlay opens with F1 to F12, not {}", overlay.key));
    }
    let own = toml::to_string(config).map_err(|e| e.to_string())?;
    Config::parse(&own).map_err(|e| e.to_string())?;
    let mut table: toml::Table = toml::from_str(&own).map_err(|e| e.to_string())?;
    table.insert(OVERLAY.into(), overlay.on.into());
    table.insert(OVERLAY_KEY.into(), overlay.key.clone().into());
    let text = format!(
        "# Written by the Cross-Com launcher; it rewrites this file when you change its settings.\n\
         # `password` holds a game token (cct_...) the launcher got from the server; players have no password.\n{}",
        toml::to_string(&table).map_err(|e| e.to_string())?
    );
    std::fs::write(game.join(FILE_NAME), text).map_err(|e| format!("couldn't write {FILE_NAME}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_reads_what_the_launcher_writes() {
        let game = tempfile::tempdir().unwrap();
        assert_eq!(client(game.path()), Config::default());
        let config = Config {
            server: Some("203.0.113.10".into()),
            username: "Kozak".into(),
            password: "pa\"ss = word".into(),
            cd_key: "AAAA-BBBB".into(),
            skip_intro_videos: true,
            ..Config::default()
        };
        save_client(game.path(), &config, &Overlay::default()).unwrap();
        assert_eq!(client(game.path()), config);
        let quiet = Config {
            send_logs: false,
            ..config.clone()
        };
        save_client(game.path(), &quiet, &Overlay::default()).unwrap();
        let text = std::fs::read_to_string(game.path().join(FILE_NAME)).unwrap();
        assert!(text.contains("\nsend_logs = false\n"), "{text}");
        assert_eq!(client(game.path()), quiet);
        let too_long = Config {
            password: "x".repeat(80),
            ..config
        };
        assert!(save_client(game.path(), &too_long, &Overlay::default()).is_err(), "the game can't take it");
    }

    #[test]
    fn writes_the_overlay_as_top_level_keys() {
        let game = tempfile::tempdir().unwrap();
        assert_eq!(overlay(game.path()), Overlay::default());
        let config = Config {
            server: Some("203.0.113.10".into()),
            username: "Kozak".into(),
            ..Config::default()
        };
        let off = Overlay { on: false, key: "F9".into() };
        save_client(game.path(), &config, &off).unwrap();
        let text = std::fs::read_to_string(game.path().join(FILE_NAME)).unwrap();
        assert!(text.contains("\noverlay = false\n") && text.contains("\noverlay_key = \"F9\"\n"), "{text}");
        assert_eq!(overlay(game.path()), off);
        assert_eq!(client(game.path()), config, "the rest still reads");
        let bad = Overlay { on: true, key: "F13".into() };
        assert!(save_client(game.path(), &config, &bad).is_err());

        std::fs::write(game.path().join(FILE_NAME), "username = \"Kozak\"\noverlay_key = \"f7\"\n").unwrap();
        assert_eq!(overlay(game.path()), Overlay { on: true, key: "F7".into() }, "by hand, in any case");
        std::fs::write(game.path().join(FILE_NAME), "overlay_key = \"Insert\"\n").unwrap();
        assert_eq!(overlay(game.path()).key, "F5");
    }

    #[test]
    fn keeps_its_own_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cross-Com/launcher.toml");
        assert_eq!(Launcher::load(&path), Launcher::default());
        let mine = Launcher {
            game_dir: Some("C:/Games/GRFS".into()),
            dx9: true,
            last_logs: Some(LastLogs {
                id: "20261115-123456-ab12cd".into(),
                at: 1_794_746_096,
            }),
            host_internet: true,
        };
        mine.save(&path).unwrap();
        assert_eq!(Launcher::load(&path), mine);
    }
}
