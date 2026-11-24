//! `ubiorbitapi_r2_loader.dll`, Cross-Com's stand-in for Ubisoft's Orbit client library.
//!
//! The game gets its sign-in details from Orbit: the account name, password and CD key it sends
//! to the online service. This library gives it the ones in `crosscom.toml`. It also sends the
//! game's lookup of Ubisoft's online config service to the Cross-Com server (`net`), and draws
//! the in-game overlay (`overlay`).
//!
//! Orbit's cloud saves are kept on this PC instead (`crosscom_client_common::saves`).

#[cfg(all(windows, target_arch = "x86"))]
mod crash;
#[cfg(windows)]
mod dll;
#[cfg(any(test, windows))]
mod late_signin;
#[cfg(any(test, windows))]
mod lobby;
#[cfg(windows)]
mod logs;
#[cfg(windows)]
mod memory;
#[cfg(windows)]
mod net;
#[cfg(all(windows, target_arch = "x86"))]
mod orbit;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod quick_match;
#[cfg(windows)]
mod video;

use std::ffi::CString;
use std::sync::OnceLock;

use crosscom_client_common::Config;

/// The settings, read once when the game loads this library.
#[cfg(windows)]
static CONFIG: OnceLock<Config> = OnceLock::new();

/// The sign-in details, in the forms the game takes them: the password as a wide string, the
/// others as narrow ones, all NUL-terminated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub username: CString,
    pub password: Vec<u16>,
    pub cd_key: CString,
}

/// The longest each sign-in detail may be: the game copies each into an 80-byte buffer.
const MAX_DETAIL: usize = 79;

impl Login {
    /// `packs`: the numbers of the DLC packs installed (`dlc::installed_numbers`), which go to
    /// the server after the CD key as `;packs=` (the server checks no CD keys, and searches go by
    /// them), or `None` to say nothing.
    #[must_use]
    pub fn new(config: &Config, packs: Option<&str>) -> Self {
        let mut cd_key = config.cd_key.clone();
        if let Some(packs) = packs {
            let with = format!("{cd_key};packs={packs}");
            // Too long for the game's buffer: the CD key alone, and the server takes it the player
            // has every pack, as before.
            if with.len() <= MAX_DETAIL {
                cd_key = with;
            }
        }
        // The settings are checked for NULs when they're read, so these can't fail.
        Self {
            username: CString::new(config.username.as_str()).unwrap_or_default(),
            password: config.password.encode_utf16().chain(std::iter::once(0)).collect(),
            cd_key: CString::new(cd_key).unwrap_or_default(),
        }
    }
}

#[cfg(windows)]
fn config() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

/// Starts this run's log, reads the settings; called once, when the game loads the library.
#[cfg(windows)]
fn start() {
    use crosscom_client_common::game_dir;
    use crosscom_client_common::logging;
    use crosscom_client_common::runs;

    let Some(dir) = game_dir() else {
        return;
    };
    // The last run's log moves aside before this run's opens (unless it's this launch's: the game
    // starts itself again a moment after it's started). Only this library writes the log
    // (uplay_r1_loader.dll doesn't), so nothing else has it open.
    let rotated = runs::rotate_at_start(&dir);
    let loaded = Config::load(&dir);
    let level = loaded.as_ref().ok().and_then(|c| c.log_level.clone());
    logging::init(&dir, "orbit", level.as_deref());
    let exe = std::env::current_exe()
        .ok()
        .and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    log::info!("Cross-Com {} loaded in {exe} from {}", env!("CROSSCOM_RELEASE"), dir.display());
    if let Some(os) = crosscom_client_common::system::os_version() {
        log::info!("{os}");
    }
    match rotated {
        Ok(Some(last)) => log::info!("The last run's log is {}", last.display()),
        Ok(None) => {}
        Err(e) => log::warn!("Couldn't move the last run's log aside: {e}"),
    }
    match loaded {
        Ok(config) => {
            if config.server.is_none() {
                log::warn!("No server in {}: the game will look for Ubisoft's servers", crosscom_client_common::config::FILE_NAME);
            }
            let _ = CONFIG.set(config);
        }
        Err(e) => log::error!("{e}"),
    }
    #[cfg(target_arch = "x86")]
    crash::install();
    net::install();
    #[cfg(target_arch = "x86")]
    late_signin::install();
    lobby::keep_sizes();
    if CONFIG.get().is_some_and(|c| c.skip_intro_videos) {
        video::install();
    }
    if let Some(players) = CONFIG.get().and_then(|c| c.quick_match_min_players) {
        quick_match::install(players);
    }
    logs::start(dir, config());
    overlay::start(config());
    // The router's forward for the game's peer-to-peer port, on a thread of its own.
    if let Some(config) = CONFIG.get().filter(|c| c.port_forwarding) {
        if let Some(server) = config.server.clone().filter(|s| crosscom_client_net::keep::wanted(s)) {
            crosscom_client_net::keep::spawn(server, config.username.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_strings_are_nul_terminated() {
        let config = Config {
            username: "Kozak".into(),
            password: "pässword".into(),
            cd_key: "AAAA-BBBB".into(),
            ..Config::default()
        };
        let login = Login::new(&config, None);
        assert_eq!(login.username.as_bytes_with_nul(), b"Kozak\0");
        assert_eq!(login.cd_key.as_bytes_with_nul(), b"AAAA-BBBB\0");
        assert_eq!(String::from_utf16(&login.password[..login.password.len() - 1]).unwrap(), "pässword");
        assert_eq!(login.password.last(), Some(&0));
    }

    #[test]
    fn empty_settings_give_empty_strings() {
        let login = Login::new(&Config::default(), None);
        assert_eq!(login.username.as_bytes_with_nul(), b"\0");
        assert_eq!(login.password, vec![0]);
    }

    #[test]
    fn the_dlc_packs_go_after_the_cd_key() {
        let config = Config {
            cd_key: "AAAA-BBBB".into(),
            ..Config::default()
        };
        assert_eq!(Login::new(&config, Some("13")).cd_key.as_bytes(), b"AAAA-BBBB;packs=13");
        assert_eq!(Login::new(&config, Some("")).cd_key.as_bytes(), b"AAAA-BBBB;packs=");
        let long = Config {
            cd_key: "K".repeat(70),
            ..Config::default()
        };
        assert_eq!(Login::new(&long, Some("123")).cd_key.as_bytes(), "K".repeat(70).as_bytes(), "never past the game's buffer");
        assert!(
            Login::new(
                &Config {
                    cd_key: "K".repeat(69),
                    ..Config::default()
                },
                Some("123")
            )
            .cd_key
            .as_bytes()
            .len()
                <= MAX_DETAIL
        );
    }
}
