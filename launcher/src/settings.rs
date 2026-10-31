//! What the launcher writes: the client's `crosscom.toml` in the game's folder (the server and
//! the account the game signs in with), and its own choices in the user's settings folder.

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

/// The client's settings in `game`, or the defaults when there are none (or they don't read).
#[must_use]
pub fn client(game: &Path) -> Config {
    Config::load(game).unwrap_or_default()
}

/// Writes the client's settings into `game`, checked as the client will read them.
pub fn save_client(game: &Path, config: &Config) -> Result<(), String> {
    let text = format!(
        "# Written by the Cross-Com launcher; it rewrites this file when you change its settings.\n{}",
        toml::to_string(config).map_err(|e| e.to_string())?
    );
    Config::parse(&text).map_err(|e| e.to_string())?;
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
        save_client(game.path(), &config).unwrap();
        assert_eq!(client(game.path()), config);
        let too_long = Config {
            password: "x".repeat(80),
            ..config
        };
        assert!(save_client(game.path(), &too_long).is_err(), "the game can't take it");
    }

    #[test]
    fn keeps_its_own_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cross-Com/launcher.toml");
        assert_eq!(Launcher::load(&path), Launcher::default());
        let mine = Launcher {
            game_dir: Some("C:/Games/GRFS".into()),
            dx9: true,
        };
        mine.save(&path).unwrap();
        assert_eq!(Launcher::load(&path), mine);
    }
}
