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
    /// The news the game shows in its menus (see `news.rs` for the format); read at each sign-in,
    /// so edits need no restart. Without the file, a welcome.
    pub news: PathBuf,
    /// Reverse proxies in front of the HTTP side (such as Caddy, for HTTPS), by address or
    /// network (`172.31.80.0/24`). A request from one of them is taken to be from the client its
    /// `X-Forwarded-For` names, and passwords through it only over HTTPS (see `proxy.rs`). None by
    /// default: the headers are ignored.
    pub trusted_proxies: Vec<String>,
    /// Whether players' clients may send their logs (`POST /api/logs`, see `logs.rs`). On by
    /// default; they're only taken from addresses their account played from lately.
    pub accept_logs: bool,
    /// Where the logs are kept, in a folder a day.
    pub logs_dir: PathBuf,
    /// The most the logs may take in all, in MiB; past it the oldest go.
    pub logs_max_mb: u64,
    /// Days logs are kept.
    pub logs_keep_days: u32,
    /// HTTPS of the server's own on this port (http_port's own serves both), for a server with no proxy giving it HTTPS (one
    /// hosted from the launcher, at home): its own certificate, made once and kept beside the
    /// database, which players' launchers know by its fingerprint (the host's join code; see
    /// `tls.rs`). Off by default.
    pub https_port: Option<u16>,
    /// Gives each player on this machine, a LAN or a VPN the server's address there, whatever
    /// `public_address` is: for a server hosted from the launcher for its network and VPN. Off by
    /// default: a server in a container would give its container's own address.
    pub answer_as_reached: bool,
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
            news: "news.toml".into(),
            trusted_proxies: Vec::new(),
            accept_logs: true,
            logs_dir: "logs".into(),
            logs_max_mb: 2048,
            logs_keep_days: 30,
            https_port: None,
            answer_as_reached: false,
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
    /// `CROSSCOM_SIGNUP_CODE` and `CROSSCOM_TRUSTED_PROXIES` (separated by commas or spaces),
    /// `CROSSCOM_ACCEPT_LOGS` (`true` or `false`), `CROSSCOM_LOGS_MAX_MB` and
    /// `CROSSCOM_LOGS_KEEP_DAYS`, `CROSSCOM_HTTPS_PORT`. `var` looks one up.
    pub fn with_env(mut self, var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        if let Some(address) = var("CROSSCOM_PUBLIC_ADDRESS") {
            self.public_address = address.trim().into();
        }
        if let Some(level) = var("CROSSCOM_LOG_LEVEL") {
            self.log_level = level.trim().into();
        }
        if let Some(signup) = var("CROSSCOM_SIGNUP") {
            self.signup = flag("CROSSCOM_SIGNUP", &signup)?;
        }
        if let Some(accept) = var("CROSSCOM_ACCEPT_LOGS") {
            self.accept_logs = flag("CROSSCOM_ACCEPT_LOGS", &accept)?;
        }
        if let Some(mb) = var("CROSSCOM_LOGS_MAX_MB") {
            self.logs_max_mb = mb.trim().parse().map_err(|_| format!("CROSSCOM_LOGS_MAX_MB is a number of MiB, not {mb:?}"))?;
        }
        if let Some(days) = var("CROSSCOM_LOGS_KEEP_DAYS") {
            self.logs_keep_days = days.trim().parse().map_err(|_| format!("CROSSCOM_LOGS_KEEP_DAYS is a number of days, not {days:?}"))?;
        }
        if let Some(code) = var("CROSSCOM_SIGNUP_CODE") {
            self.signup_code = Some(code);
        }
        if let Some(port) = var("CROSSCOM_HTTPS_PORT") {
            self.https_port = Some(port.trim().parse().map_err(|_| format!("CROSSCOM_HTTPS_PORT is a port, not {port:?}"))?);
        }
        if let Some(proxies) = var("CROSSCOM_TRUSTED_PROXIES") {
            self.trusted_proxies = proxies.split([',', ' ']).filter(|p| !p.is_empty()).map(str::to_owned).collect();
        }
        self.check()?;
        Ok(self)
    }

    /// The logs' settings.
    pub fn logs(&self) -> crate::logs::Settings {
        crate::logs::Settings {
            accept: self.accept_logs,
            dir: self.logs_dir.clone(),
            max_bytes: self.logs_max_mb.saturating_mul(1024 * 1024),
            keep_days: self.logs_keep_days,
        }
    }

    /// The proxies in `trusted_proxies`, and the server's own HTTPS when it has it
    /// (`tls::FRONT`).
    pub fn trusted_proxies(&self) -> Result<crate::proxy::Trusted, String> {
        let mut proxies = self.trusted_proxies.clone();
        if self.https_port.is_some() {
            proxies.push(crate::tls::FRONT.to_string());
        }
        crate::proxy::Trusted::parse(&proxies)
    }

    /// Where the server's own HTTPS certificate and its key are kept: beside the database.
    pub fn tls_files(&self) -> (PathBuf, PathBuf) {
        let dir = self.database.parent().unwrap_or_else(|| Path::new(""));
        (dir.join("https-certificate.der"), dir.join("https-key.der"))
    }

    /// Refuses a sign-up code short enough to guess, and proxies that aren't addresses.
    fn check(&self) -> Result<(), String> {
        self.trusted_proxies()?;
        if self.https_port.is_some() && !(self.listen_ip.is_unspecified() || self.listen_ip.is_loopback()) {
            return Err("https_port needs listen_ip to be all addresses (0.0.0.0) or this machine's: the server's HTTPS passes requests to its HTTP side on 127.0.0.1".into());
        }
        if self.https_port == Some(0) {
            return Err("https_port is a port, not 0 (the same as http_port serves both there)".into());
        }
        match &self.signup_code {
            Some(code) if !code.is_empty() && code.chars().count() < crate::signup::MIN_CODE_LEN => Err(format!(
                "signup_code is too short to keep strangers out: use at least {} random characters",
                crate::signup::MIN_CODE_LEN
            )),
            _ => Ok(()),
        }
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

/// A `true` or `false` setting from the environment.
fn flag(name: &str, value: &str) -> Result<bool, String> {
    match value.trim() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        other => Err(format!("{name} is true or false, not {other:?}")),
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
            "CROSSCOM_SIGNUP_CODE" => Some("ghosts-in-the-machine".to_string()),
            "CROSSCOM_LOG_LEVEL" => Some(String::new()),
            _ => None,
        };
        let config = Config::default().with_env(env).unwrap();
        assert_eq!(config.public_address, "198.51.100.4");
        assert!(config.signup);
        assert_eq!(config.signup_code.as_deref(), Some("ghosts-in-the-machine"));
        assert!(Config::default().with_env(|n| (n == "CROSSCOM_SIGNUP_CODE").then(|| "ghosts".into())).is_err(), "too short");
        assert_eq!(config.log_level, "info", "empty values are ignored");
        assert!(Config::default().with_env(|n| (n == "CROSSCOM_SIGNUP").then(|| "maybe".into())).is_err());
        assert!(!Config::default().signup, "off by default");
    }

    #[test]
    fn trusted_proxies_from_the_file_or_the_environment() {
        let config: Config = toml::from_str("trusted_proxies = [\"172.31.80.10\", \"fd00::/8\"]").unwrap();
        let trusted = config.trusted_proxies().unwrap();
        assert!(trusted.trusts("172.31.80.10".parse().unwrap()));
        assert!(trusted.trusts("fd00::1".parse().unwrap()));
        assert!(!trusted.trusts("172.31.80.11".parse().unwrap()));
        let config = Config::default()
            .with_env(|n| (n == "CROSSCOM_TRUSTED_PROXIES").then(|| "172.31.80.10, 10.0.0.0/8".into()))
            .unwrap();
        assert_eq!(config.trusted_proxies, ["172.31.80.10", "10.0.0.0/8"]);
        assert!(Config::default().with_env(|n| (n == "CROSSCOM_TRUSTED_PROXIES").then(|| "caddy".into())).is_err());
        assert_eq!(Config::default().trusted_proxies().unwrap(), crate::proxy::Trusted::default(), "none by default");
    }

    #[test]
    fn logs_are_taken_unless_turned_off() {
        let config = Config::default();
        assert!(config.accept_logs);
        assert_eq!(config.logs().max_bytes, 2048 * 1024 * 1024);
        assert_eq!(config.logs().dir, PathBuf::from("logs"));
        let env = |name: &str| match name {
            "CROSSCOM_ACCEPT_LOGS" => Some("false".to_string()),
            "CROSSCOM_LOGS_MAX_MB" => Some("100".to_string()),
            "CROSSCOM_LOGS_KEEP_DAYS" => Some("7".to_string()),
            _ => None,
        };
        let config = Config::default().with_env(env).unwrap();
        assert!(!config.accept_logs);
        assert_eq!((config.logs_max_mb, config.logs_keep_days), (100, 7));
        assert!(Config::default().with_env(|n| (n == "CROSSCOM_LOGS_MAX_MB").then(|| "lots".into())).is_err());
        let config: Config = toml::from_str("accept_logs = false\nlogs_dir = \"/var/crosscom/logs\"").unwrap();
        assert!(!config.logs().accept);
    }

    #[test]
    fn refuses_unknown_settings() {
        assert!(toml::from_str::<Config>("auth_prot = 1").is_err());
    }
}
