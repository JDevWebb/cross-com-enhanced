//! The server's settings: `crosscom-server.toml`.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::ToSocketAddrs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// This server's address as players reach it: a host name or an IP address. The game is told
    /// to connect here.
    pub public_address: String,
    /// The address to listen on; all of the machine's by default.
    pub listen_ip: IpAddr,
    /// The online config service, over HTTP. The game always asks on port 80.
    pub http_port: u16,
    /// The sign-in (authentication) server, over UDP.
    pub auth_port: u16,
    /// The secure server, where the game's online services are, over UDP.
    pub secure_port: u16,
    /// The accounts database.
    pub database: PathBuf,
    /// `error`, `warning`, `info`, `debug` or `trace`.
    pub log_level: String,
    /// Whether players may make their own accounts over HTTP (`POST /accounts`, which the launcher
    /// uses). Off by default: accounts are made with `add-account`.
    pub signup: bool,
    /// A code players must give to sign up, if set; give it to the people you play with.
    pub signup_code: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            public_address: "127.0.0.1".into(),
            listen_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            http_port: 80,
            auth_port: 21030,
            secure_port: 21031,
            database: "crosscom.db".into(),
            log_level: "info".into(),
            signup: false,
            signup_code: None,
        }
    }
}

impl Config {
    /// Reads the settings; a missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{} isn't valid: {}", path.display(), e.message())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("couldn't read {}: {e}", path.display())),
        }
    }

    /// The settings with any given in the environment instead, as a container is set up:
    /// `CROSSCOM_PUBLIC_ADDRESS`, `CROSSCOM_LOG_LEVEL`, `CROSSCOM_SIGNUP` (`true` or `false`) and
    /// `CROSSCOM_SIGNUP_CODE`. `var` looks one up.
    pub fn with_env(mut self, var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        if let Some(address) = var("CROSSCOM_PUBLIC_ADDRESS") {
            self.public_address = address.trim().into();
        }
        if let Some(level) = var("CROSSCOM_LOG_LEVEL") {
            self.log_level = level.trim().into();
        }
        if let Some(signup) = var("CROSSCOM_SIGNUP") {
            self.signup = match signup.trim() {
                "true" | "1" | "yes" => true,
                "false" | "0" | "no" => false,
                other => return Err(format!("CROSSCOM_SIGNUP is true or false, not {other:?}")),
            };
        }
        if let Some(code) = var("CROSSCOM_SIGNUP_CODE") {
            self.signup_code = Some(code);
        }
        Ok(self)
    }

    /// The public address as an IP address, looking the name up if it is one. Station URLs need an
    /// address, not a name.
    pub fn public_ip(&self) -> Result<IpAddr, String> {
        if let Ok(ip) = self.public_address.parse() {
            return Ok(ip);
        }
        (self.public_address.as_str(), 0)
            .to_socket_addrs()
            .map_err(|e| format!("couldn't look up {}: {e}", self.public_address))?
            .map(|a| a.ip())
            .find(IpAddr::is_ipv4)
            .ok_or_else(|| format!("{} has no IPv4 address", self.public_address))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_in_what_is_missing() {
        let config: Config = toml::from_str("public_address = \"203.0.113.7\"").unwrap();
        assert_eq!(config.public_ip().unwrap(), "203.0.113.7".parse::<IpAddr>().unwrap());
        assert_eq!(config.auth_port, 21030);
        assert_eq!(config.http_port, 80);
    }

    #[test]
    fn the_environment_overrides_the_file() {
        let env = |name: &str| match name {
            "CROSSCOM_PUBLIC_ADDRESS" => Some(" 198.51.100.4 ".to_string()),
            "CROSSCOM_SIGNUP" => Some("true".to_string()),
            "CROSSCOM_SIGNUP_CODE" => Some("ghosts".to_string()),
            "CROSSCOM_LOG_LEVEL" => Some(String::new()),
            _ => None,
        };
        let config = Config::default().with_env(env).unwrap();
        assert_eq!(config.public_address, "198.51.100.4");
        assert!(config.signup);
        assert_eq!(config.signup_code.as_deref(), Some("ghosts"));
        assert_eq!(config.log_level, "info", "empty values are ignored");
        assert!(Config::default().with_env(|n| (n == "CROSSCOM_SIGNUP").then(|| "maybe".into())).is_err());
        assert!(!Config::default().signup, "off by default");
    }

    #[test]
    fn refuses_unknown_settings() {
        assert!(toml::from_str::<Config>("auth_prot = 1").is_err());
    }
}
