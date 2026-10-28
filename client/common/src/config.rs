//! The client's settings: `crosscom.toml`, next to the game's exe.
//!
//! The launcher writes it; players can edit it by hand. Both client DLLs read it.

use std::fmt;
use std::path::Path;

use serde::Deserialize;

/// The settings file's name, in the game's folder.
pub const FILE_NAME: &str = "crosscom.toml";

/// The host the game asks for its online configuration. Its lookups are sent to the Cross-Com server.
pub const ONLINE_CONFIG_HOST: &str = "onlineconfigservice.ubi.com";

/// The longest username, password or CD key the game accepts: it copies each into an 80-byte buffer
/// with its terminating NUL, and a longer one stops the game.
pub const MAX_LOGIN_FIELD_LEN: usize = 79;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
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
}
