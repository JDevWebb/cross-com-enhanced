//! The client's settings: `crosscom.toml`, next to the game's exe.
//!
//! The launcher writes it; players can edit it by hand. Both client DLLs read it.

use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;

/// The settings file's name, in the game's folder.
pub const FILE_NAME: &str = "crosscom.toml";

/// The host the game asks for its online configuration. Its lookups are sent to the Cross-Com server.
pub const ONLINE_CONFIG_HOST: &str = "onlineconfigservice.ubi.com";

/// The longest username, password or CD key the game accepts: it copies each into an 80-byte buffer
/// with its terminating NUL, and a longer one stops the game.
pub const MAX_LOGIN_FIELD_LEN: usize = 79;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The Cross-Com server, a host name or an address. Without it the game talks to Ubisoft's
    /// (long gone) servers.
    pub server: Option<String>,
    /// The account to sign in with.
    pub username: String,
    pub password: String,
    /// The game's CD key, sent with the sign-in.
    pub cd_key: String,
    /// `error`, `warn`, `info` (the default), `debug` or `trace`.
    pub log_level: Option<String>,
    /// Skip the splash videos the game plays as it starts.
    pub skip_intro_videos: bool,
    /// The fewest players a Quick Match (ranked) lobby needs before it starts. The game's own
    /// data asks for 8 (6 in the smaller modes), which a small community rarely reaches; unset
    /// keeps the game's numbers.
    pub quick_match_min_players: Option<u8>,
    /// The in-game overlay (on unless set to false).
    pub overlay: bool,
    /// The key that opens the overlay's panel: `F1` to `F12`. An unknown name means F5.
    pub overlay_key: String,
    /// Send each run's log to the server after it (on unless set to false), with the Windows
    /// user name and the password taken out (see `runs`).
    pub send_logs: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: None,
            username: String::new(),
            password: String::new(),
            cd_key: String::new(),
            log_level: None,
            skip_intro_videos: false,
            quick_match_min_players: None,
            overlay: true,
            overlay_key: DEFAULT_OVERLAY_KEY.to_owned(),
            send_logs: true,
        }
    }
}

/// The overlay's key unless another is chosen.
pub const DEFAULT_OVERLAY_KEY: &str = "F5";

/// The keys the overlay can open with, by name, with their Windows virtual-key codes.
pub const OVERLAY_KEYS: [(&str, u16); 12] = [
    ("F1", 0x70),
    ("F2", 0x71),
    ("F3", 0x72),
    ("F4", 0x73),
    ("F5", 0x74),
    ("F6", 0x75),
    ("F7", 0x76),
    ("F8", 0x77),
    ("F9", 0x78),
    ("F10", 0x79),
    ("F11", 0x7A),
    ("F12", 0x7B),
];

/// The fewest players a match can be played with.
pub const MIN_QUICK_MATCH_PLAYERS: u8 = 2;

/// The splash videos the game plays as it starts, in its `Video` folder.
pub const INTRO_VIDEOS: [&str; 3] = ["SPL_ESRB_UBI_CLANCY.bik", "SPL_RSE_Logo_fade.bik", "SPL_Intro_Trailer.bik"];

/// Whether `path` is one of the splash videos (by file name, in any case, after `/` or `\\`).
#[must_use]
pub fn is_intro_video(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    INTRO_VIDEOS.iter().any(|v| v.eq_ignore_ascii_case(name))
}

#[derive(Debug)]
pub enum Error {
    Read(std::io::Error),
    /// Only the message and the line: `toml`'s own error quotes the line, which may hold the
    /// password, and these errors go into the log players share.
    Parse {
        message: String,
        line: Option<usize>,
    },
    /// A username, password or CD key the game can't take.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read(e) => write!(f, "couldn't read {FILE_NAME}: {e}"),
            Error::Parse { message, line: Some(line) } => write!(f, "{FILE_NAME} isn't valid at line {line}: {message}"),
            Error::Parse { message, line: None } => write!(f, "{FILE_NAME} isn't valid: {message}"),
            Error::Invalid(e) => write!(f, "{FILE_NAME}: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Config {
    /// Reads `crosscom.toml` from the game's folder.
    pub fn load(game_dir: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(game_dir.join(FILE_NAME)).map_err(Error::Read)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        let config: Self = toml::from_str(text).map_err(|e| Error::Parse {
            message: safe_message(e.message()),
            line: e.span().map(|span| text[..span.start.min(text.len())].lines().count().max(1)),
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), Error> {
        if let Some(n) = self.quick_match_min_players {
            if !(MIN_QUICK_MATCH_PLAYERS..=12).contains(&n) {
                return Err(Error::Invalid(format!("quick_match_min_players is {MIN_QUICK_MATCH_PLAYERS} to 12, not {n}")));
            }
        }
        for (name, value) in [("username", &self.username), ("password", &self.password), ("cd_key", &self.cd_key)] {
            if value.len() > MAX_LOGIN_FIELD_LEN {
                return Err(Error::Invalid(format!("{name} is longer than {MAX_LOGIN_FIELD_LEN} characters")));
            }
            if value.contains('\0') {
                return Err(Error::Invalid(format!("{name} contains a NUL character")));
            }
        }
        Ok(())
    }

    /// The overlay's key: its name and virtual-key code. F5 when `overlay_key` isn't one of
    /// [`OVERLAY_KEYS`] (any case).
    #[must_use]
    pub fn overlay_key(&self) -> (&'static str, u16) {
        let find = |key: &str| OVERLAY_KEYS.iter().copied().find(|(name, _)| name.eq_ignore_ascii_case(key.trim()));
        find(&self.overlay_key).or_else(|| find(DEFAULT_OVERLAY_KEY)).unwrap_or(("F5", 0x74))
    }

    /// Where a lookup of `host` should go instead, if anywhere.
    #[must_use]
    pub fn redirect(&self, host: &str) -> Option<&str> {
        if host.eq_ignore_ascii_case(ONLINE_CONFIG_HOST) {
            self.server.as_deref().filter(|s| !s.is_empty())
        } else {
            None
        }
    }
}

/// `toml`'s message, when it can only name settings ("unknown field `sever`"); any other may quote
/// a value, such as a password written without quotes, so it's replaced.
fn safe_message(message: &str) -> String {
    if message.starts_with("unknown field") || message.starts_with("missing field") || message.starts_with("duplicate key") {
        message.to_owned()
    } else {
        "a setting isn't written correctly; text values need double quotes".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_file() {
        let config = Config::parse(
            r#"
            server = "play.example.net"
            username = "Kozak"
            password = "hunter2"
            cd_key = "ABCD-EFGH-IJKL-MNOP"
            log_level = "debug"
            "#,
        )
        .unwrap();
        assert_eq!(config.server.as_deref(), Some("play.example.net"));
        assert_eq!(config.username, "Kozak");
        assert_eq!(config.cd_key, "ABCD-EFGH-IJKL-MNOP");
        assert_eq!(config.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn everything_is_optional() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn refuses_unknown_settings() {
        assert!(matches!(Config::parse("sever = \"typo\""), Err(Error::Parse { .. })));
    }

    #[test]
    fn parse_errors_never_quote_the_file() {
        let err = Config::parse("username = \"Kozak\"\npassword = \"s3cret-pass\" oops\n").unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("s3cret-pass"), "{text}");
        assert!(text.contains("line 2"), "{text}");

        let err = Config::parse("password = 123456").unwrap_err().to_string();
        assert!(!err.contains("123456"), "{err}");
        let err = Config::parse("sever = \"x\"").unwrap_err().to_string();
        assert!(err.contains("unknown field `sever`"), "{err}");
    }

    #[test]
    fn refuses_fields_the_game_cant_take() {
        let long = "x".repeat(MAX_LOGIN_FIELD_LEN + 1);
        assert!(matches!(Config::parse(&format!("password = \"{long}\"")), Err(Error::Invalid(_))));
        let longest = "x".repeat(MAX_LOGIN_FIELD_LEN);
        assert!(Config::parse(&format!("password = \"{longest}\"")).is_ok());
    }

    #[test]
    fn knows_the_intro_videos_by_name() {
        assert!(is_intro_video("Video\\SPL_Intro_Trailer.bik"));
        assert!(is_intro_video("C:/Games/GRFS/video/spl_esrb_ubi_clancy.BIK"));
        assert!(is_intro_video("SPL_RSE_Logo_fade.bik"));
        assert!(!is_intro_video("Video\\MP_Intro.bik"));
        assert!(!is_intro_video("Video\\SPL_Intro_Trailer.bik.bak"));
        let config = Config::parse("skip_intro_videos = true").unwrap();
        assert!(config.skip_intro_videos);
        assert!(!Config::default().skip_intro_videos);
    }

    #[test]
    fn redirects_only_the_online_config_host() {
        let config = Config {
            server: Some("10.0.0.5".into()),
            ..Config::default()
        };
        assert_eq!(config.redirect("onlineconfigservice.ubi.com"), Some("10.0.0.5"));
        assert_eq!(config.redirect("OnlineConfigService.Ubi.com"), Some("10.0.0.5"));
        assert_eq!(config.redirect("www.ubi.com"), None);
        assert_eq!(Config::default().redirect(ONLINE_CONFIG_HOST), None);
    }

    #[test]
    fn the_overlay_is_on_with_f5_unless_set() {
        let config = Config::parse("").unwrap();
        assert!(config.overlay);
        assert_eq!(config.overlay_key(), ("F5", 0x74));
        let config = Config::parse("overlay = false\noverlay_key = \"F12\"").unwrap();
        assert!(!config.overlay);
        assert_eq!(config.overlay_key(), ("F12", 0x7B));
        assert_eq!(Config::parse("overlay_key = \"f1\"").unwrap().overlay_key(), ("F1", 0x70));
        // A key it doesn't know doesn't stop the game signing in: F5 instead.
        assert_eq!(Config::parse("overlay_key = \"Insert\"").unwrap().overlay_key(), ("F5", 0x74));
    }

    #[test]
    fn logs_are_sent_unless_turned_off() {
        assert!(Config::parse("").unwrap().send_logs);
        assert!(!Config::parse("send_logs = false").unwrap().send_logs);
    }

    #[test]
    fn the_quick_match_minimum_is_two_to_twelve() {
        assert_eq!(Config::parse("quick_match_min_players = 2").unwrap().quick_match_min_players, Some(2));
        assert!(Config::parse("quick_match_min_players = 1").is_err());
        assert!(Config::parse("quick_match_min_players = 13").is_err());
        assert_eq!(Config::parse("").unwrap().quick_match_min_players, None);
    }
}
