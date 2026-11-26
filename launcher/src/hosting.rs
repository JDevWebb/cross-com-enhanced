//! A Cross-Com server hosted from the launcher, for friends: the server built into the launcher
//! (the `embed-server` feature), run while the launcher is open.
//!
//! It lives in the launcher's folder, in `Server` (`%APPDATA%\Cross-Com\Server`): the program,
//! its settings (written afresh at each start), its database, its HTTPS certificate (made the
//! first time and kept, so its join codes stay the same) and its log. It runs hidden, tied to the
//! launcher (a Windows job that ends it when the launcher goes, however it goes).
//!
//! Players reach it by its address with its certificate's fingerprint (a join code, see
//! `server::make_join_code`): its HTTPS is its own, which no certificate authority vouches for.
//! The host's own game plays on it at this machine's address (127.0.0.1), with the same
//! fingerprint.

use std::io::Read;
use std::io::Write;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::time::Duration;
use std::time::Instant;

use serde::Serialize;

/// The server's folder, in the launcher's.
pub const DIR: &str = "Server";
/// The program's name there.
pub const EXE: &str = "crosscom-server.exe";
/// Its settings, database, certificate and log there.
pub const CONFIG: &str = "crosscom-server.toml";
pub const DATABASE: &str = "crosscom.db";
pub const CERTIFICATE: &str = "https-certificate.der";
pub const LOG: &str = "server.log";
/// The ports it serves: the web side (TCP: the online config, and HTTPS, on one port), the game's
/// sign-in and services (UDP).
pub const HTTP_PORT: u16 = 80;
pub const AUTH_PORT: u16 = 21030;
pub const SECURE_PORT: u16 = 21031;
/// How long it may take to answer once started.
const START_WAIT: Duration = Duration::from_secs(15);

/// The server built into this launcher, or `None` in a build without it.
#[must_use]
pub fn built_in() -> Option<&'static [u8]> {
    #[cfg(feature = "embed-server")]
    {
        Some(include_bytes!(concat!(env!("OUT_DIR"), "/crosscom-server.exe")))
    }
    #[cfg(not(feature = "embed-server"))]
    {
        None
    }
}

/// The server's program in `dir`, put there from the launcher (when it changed), or one beside
/// the launcher for a build without it.
pub fn install(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}", dir.display()))?;
    let exe = dir.join(EXE);
    match built_in() {
        Some(bytes) => {
            if std::fs::read(&exe).ok().as_deref() != Some(bytes) {
                std::fs::write(&exe, bytes).map_err(|e| format!("Couldn't put the server in {}: {e}", exe.display()))?;
            }
            Ok(exe)
        }
        None => {
            let beside = std::env::current_exe().ok().and_then(|l| l.parent().map(|d| d.join(EXE))).filter(|p| p.is_file());
            beside.ok_or_else(|| "This launcher was built without the server: download a release.".into())
        }
    }
}

/// The server's settings, as written for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Settings {
    public_address: String,
    http_port: u16,
    https_port: u16,
    answer_as_reached: bool,
    auth_port: u16,
    secure_port: u16,
    database: PathBuf,
    logs_dir: PathBuf,
    news: PathBuf,
    signup: bool,
    log_level: &'static str,
}

/// Writes the server's settings in `dir`, for players to connect to `public` (its public
/// address; this machine's on the network when it has none): the file. `as_reached`: each
/// player on this machine, a LAN or a VPN is given the server's address there (hosting for this
/// PC's networks alone).
pub fn write_settings(dir: &Path, public: IpAddr, as_reached: bool) -> Result<PathBuf, String> {
    let settings = Settings {
        public_address: public.to_string(),
        http_port: HTTP_PORT,
        // HTTPS on the web port itself: one port for the router to forward.
        https_port: HTTP_PORT,
        answer_as_reached: as_reached,
        auth_port: AUTH_PORT,
        secure_port: SECURE_PORT,
        database: dir.join(DATABASE),
        logs_dir: dir.join("logs"),
        news: dir.join("news.toml"),
        // Friends make their accounts with their join code: whoever has it may play.
        signup: true,
        log_level: "info",
    };
    let text = toml::to_string(&settings).map_err(|e| e.to_string())?;
    let path = dir.join(CONFIG);
    std::fs::write(&path, format!("# Written by the Cross-Com launcher at each start (Host).\n{text}")).map_err(|e| format!("Couldn't write {}: {e}", path.display()))?;
    Ok(path)
}

/// The fingerprint of the server's HTTPS certificate in `dir` (made at its first start).
#[must_use]
pub fn fingerprint(dir: &Path) -> Option<crate::server::Fingerprint> {
    let cert = std::fs::read(dir.join(CERTIFICATE)).ok()?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &cert);
    digest.as_ref().try_into().ok()
}

/// How the hosted server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// Started, not answering yet.
    Starting,
    Running {
        players: u32,
    },
    /// It stopped by itself, or never answered: why, from its log.
    Failed(String),
}

/// The hosted server, while it runs.
pub struct Hosted {
    child: Child,
    dir: PathBuf,
    started: Instant,
    answered: bool,
}

impl Hosted {
    /// Starts the server in `dir` (its program `exe`, settings `config`), hidden and tied to this
    /// launcher, with its output in its log.
    pub fn start(dir: &Path, exe: &Path, config: &Path) -> Result<Self, String> {
        let log = std::fs::File::create(dir.join(LOG)).map_err(|e| format!("Couldn't open the server's log: {e}"))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let mut command = std::process::Command::new(exe);
        command
            .arg("--config")
            .arg(config)
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(err);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command.spawn().map_err(|e| format!("Couldn't start the server: {e}"))?;
        #[cfg(windows)]
        tie_to_launcher(&child);
        Ok(Self {
            child,
            dir: dir.to_path_buf(),
            started: Instant::now(),
            answered: false,
        })
    }

    /// How it is now: still running (and answering, with how many players are online), or why
    /// it stopped.
    pub fn state(&mut self) -> State {
        match self.child.try_wait() {
            Ok(Some(status)) => return State::Failed(failure(&self.dir, &format!("the server stopped ({status})"))),
            Err(e) => return State::Failed(format!("couldn't tell how the server is: {e}")),
            Ok(None) => {}
        }
        match players_online() {
            Some(players) => {
                self.answered = true;
                State::Running { players }
            }
            None if self.answered => State::Failed(failure(&self.dir, "the server stopped answering")),
            None if self.started.elapsed() > START_WAIT => {
                // Running, but its web side never answered: most likely port 80 is taken.
                State::Failed(failure(&self.dir, "the server didn't answer on port 80"))
            }
            None => State::Starting,
        }
    }

    /// Stops it.
    pub fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Why the server failed: `what`, and the first error its log gives.
fn failure(dir: &Path, what: &str) -> String {
    let log = std::fs::read_to_string(dir.join(LOG)).unwrap_or_default();
    let error = log
        .lines()
        .find(|l| l.contains(" ERRO ") || l.contains("stopped:") || l.contains("couldn't"))
        .map(clean_line);
    let taken = log.contains("os error 10048") || log.contains("Address already in use") || log.contains("Only one usage of each socket address");
    let mut said = what.to_owned();
    if taken {
        said += ": a port it needs (TCP 80, UDP 21030 or 21031) is in use by another program; close it and start again";
    } else if let Some(error) = error {
        said += &format!(": {error}");
    }
    said
}

/// The last `n` lines of the server's log in `dir`, as they read.
#[must_use]
pub fn log_tail(dir: &Path, n: usize) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(LOG)).unwrap_or_default();
    let lines: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(clean_line).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// A log line without its fields for the protocol's internals (`, call: ..., module: ...`).
fn clean_line(line: &str) -> String {
    let cut = [", call: ", ", service: ", ", module: ", ", session: "]
        .iter()
        .filter_map(|m| line.find(m))
        .min()
        .unwrap_or(line.len());
    line[..cut].to_owned()
}

/// How many players the server on this machine says are online, if it answers (its own API,
/// straight to its web side on this machine).
fn players_online() -> Option<u32> {
    let mut stream = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, HTTP_PORT)), Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    stream.write_all(b"GET /api/info HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").ok()?;
    let mut answer = Vec::new();
    let _ = stream.take(256 * 1024).read_to_end(&mut answer);
    let text = String::from_utf8_lossy(&answer);
    let body = text.split_once("\r\n\r\n")?.1;
    let info: serde_json::Value = serde_json::from_str(body).ok()?;
    (info["game"] == crate::server::GAME).then(|| info["players_online"].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0))
}

/// Ends the server with the launcher, however the launcher ends: a Windows job that kills what's
/// in it when its last handle (the launcher's, never closed) goes.
#[cfg(windows)]
fn tie_to_launcher(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;

    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
    use windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
    use windows_sys::Win32::System::JobObjects::SetInformationJobObject;
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
    use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    /// The job, as a number: made once, kept open until the launcher ends.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| {
        // SAFETY: no name, default security; the handle is kept for the process's life.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if !job.is_null() {
            // SAFETY: a zeroed structure is valid; only the limit flag is set.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0);
            // SAFETY: the job's own handle, and the structure with its size.
            unsafe { SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const limits).cast(), size) };
        }
        job as usize
    });
    if job != 0 {
        // SAFETY: the job, and the child's process handle (open while `child` lives).
        unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
    }
}

/// This machine's addresses players on its networks reach it at: the one towards the internet
/// (the LAN's), and a VPN's such as Tailscale's (100.64.0.0/10), when it's on one.
#[must_use]
pub fn local_addresses() -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    let towards = |to: Ipv4Addr| {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect((to, 9)).ok()?;
        match socket.local_addr().ok()?.ip() {
            IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
            _ => None,
        }
    };
    // The internet's way, then Tailscale's own (100.100.100.100 is its resolver).
    for to in [Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(100, 100, 100, 100)] {
        if let Some(ip) = towards(to).filter(|ip| !found.contains(ip)) {
            found.push(ip);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_settings_a_server_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_settings(dir.path(), "203.0.113.7".parse().unwrap(), true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(table["public_address"].as_str(), Some("203.0.113.7"));
        assert_eq!(table["https_port"].as_integer(), Some(80), "HTTPS on the web port");
        assert_eq!(table["answer_as_reached"].as_bool(), Some(true));
        assert_eq!(table["signup"].as_bool(), Some(true));
        assert_eq!(table["database"].as_str().map(PathBuf::from), Some(dir.path().join(DATABASE)));
    }

    #[test]
    fn log_lines_without_the_internals() {
        assert_eq!(
            clean_line("Nov 27 INFO \"Kozak\" signed in, call: 10, method_id: 2, module: x"),
            "Nov 27 INFO \"Kozak\" signed in"
        );
        assert_eq!(
            clean_line("Nov 27 INFO Listening on 0.0.0.0:21030, service: auth, module: quazal"),
            "Nov 27 INFO Listening on 0.0.0.0:21030"
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOG), "a\n\nb\nc, module: m\n").unwrap();
        assert_eq!(log_tail(dir.path(), 2), ["b", "c"]);
        std::fs::write(dir.path().join(LOG), "x ERRO stopped: Only one usage of each socket address (os error 10048)\n").unwrap();
        assert!(failure(dir.path(), "it stopped").contains("in use by another program"));
    }

    #[test]
    fn the_fingerprint_is_the_certificates_sha256() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(fingerprint(dir.path()), None);
        std::fs::write(dir.path().join(CERTIFICATE), b"abc").unwrap();
        let f = fingerprint(dir.path()).unwrap();
        assert_eq!(
            crosscom_client_common::http::fingerprint_text(&f),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
