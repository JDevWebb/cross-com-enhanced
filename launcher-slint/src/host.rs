//! Host: a server for friends, started and stopped from the launcher (the server built in, see
//! `crosscom_launcher::hosting`), with what it takes to reach it.
//!
//! Who can join is the host's choice:
//! - **My network and VPN**: players on this PC's networks (a LAN, a VPN such as Tailscale). The
//!   router isn't asked for anything; each player is given the server's address on their network.
//! - **Anyone on the internet**: the router is asked to forward the server's web port (TCP: the
//!   online config, and HTTPS, on one port) and the game's two UDP ports (UPnP, PCP or NAT-PMP;
//!   renewed while it runs, taken away when it stops). Many routers won't forward a port under
//!   1024, so the web port goes out on whatever port the router gives it: the join code says it,
//!   and the client sends the game there. Once it runs, the community server checks it from
//!   outside, so the host knows friends will reach it.
//!
//! Either way, Windows Firewall lets the server in (Windows asks once), and friends join with a
//! join code: an address of the server's, with its web port, and its certificate's fingerprint.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crosscom_client_net::portmap;
use crosscom_client_net::portmap::Protocol;
use crosscom_launcher::firewall;
use crosscom_launcher::hosting;
use crosscom_launcher::server;

/// How long the router is asked to keep each forward, and how often they're renewed.
const LEASE: u32 = 60 * 60;
const RENEW: Duration = Duration::from_secs(20 * 60);
const DESCRIPTION: &str = "Cross-Com server";
/// Ports asked for on the router for the web side, in turn, when it won't give port 80 itself.
const WEB_PORTS: [u16; 3] = [hosting::HTTP_PORT, 8080, 28_080];
/// The game's UDP ports, forwarded on their own numbers (the online config gives them).
const UDP_PORTS: [(u16, &str); 2] = [(hosting::AUTH_PORT, "Sign-in (UDP 21030)"), (hosting::SECURE_PORT, "Game services (UDP 21031)")];
/// How long the server has to answer before it's checked from outside.
const READY_WAIT: Duration = Duration::from_secs(15);

/// A line of Host's checklist: what, how it stands ("good", "warn", "bad", "muted"), and more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub label: String,
    pub value: String,
    pub tone: &'static str,
    pub detail: String,
}

/// A join code, and where it works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub place: String,
    pub code: String,
}

/// How hosting stands, for the Host screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// "stopped", "starting", "running" or "failed".
    pub state: &'static str,
    /// What it's doing, or why it failed.
    pub said: String,
    pub players: u32,
    pub lines: Vec<Line>,
    pub codes: Vec<Code>,
    pub log: Vec<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            state: "stopped",
            said: String::new(),
            players: 0,
            lines: Vec::new(),
            codes: Vec::new(),
            log: Vec::new(),
        }
    }
}

/// Where friends on the internet reach the server: its public address, and the port the
/// router gives its web side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Outside {
    address: Ipv4Addr,
    web_port: u16,
}

/// What's set up while the server runs.
struct Running {
    hosted: hosting::Hosted,
    forwards: Arc<Mutex<Vec<portmap::Mapping>>>,
    /// The renewing thread stops when this is false.
    renewing: Arc<Mutex<bool>>,
    lines: Arc<Mutex<Vec<Line>>>,
    /// For friends on the internet, when they're let in.
    outside: Option<Outside>,
}

pub struct Host {
    dir: PathBuf,
    running: Mutex<Option<Running>>,
    /// Starting (what it's doing), or why the last start failed.
    starting: Mutex<Option<String>>,
    failed: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Host {
    /// Hosting from the launcher's folder `launcher_dir` (the server's is in it).
    #[must_use]
    pub fn new(launcher_dir: &std::path::Path) -> Self {
        Self {
            dir: launcher_dir.join(hosting::DIR),
            running: Mutex::new(None),
            starting: Mutex::new(None),
            failed: Mutex::new(None),
        }
    }

    /// Whether the server is running (or starting).
    #[must_use]
    pub fn is_on(&self) -> bool {
        lock(&self.running).is_some() || lock(&self.starting).is_some()
    }

    /// Whether starting will ask Windows for permission (the firewall's rules for the server
    /// aren't there): the launcher says why first.
    #[must_use]
    pub fn firewall_would_ask(&self) -> bool {
        hosting::install(&self.dir).is_ok_and(|exe| firewall::server_would_ask(&exe))
    }

    /// The server's certificate's fingerprint, once it has started once.
    #[must_use]
    pub fn fingerprint(&self) -> Option<server::Fingerprint> {
        hosting::fingerprint(&self.dir)
    }

    /// Starts the server for this PC's networks, or (`internet`) for anyone on the internet with
    /// a join code too, with everything that takes. Takes a few seconds (the router, Windows'
    /// prompt, the check from outside): off the window's thread.
    pub fn start(&self, internet: bool) -> Result<(), String> {
        if self.is_on() {
            return Ok(());
        }
        *lock(&self.failed) = None;
        let say = |what: &str| *lock(&self.starting) = Some(what.to_owned());
        say("Starting…");
        let result = self.start_steps(internet, &say);
        *lock(&self.starting) = None;
        if let Err(e) = &result {
            *lock(&self.failed) = Some(e.clone());
        }
        result
    }

    fn start_steps(&self, internet: bool, say: &dyn Fn(&str)) -> Result<(), String> {
        say("Putting the server in place…");
        let exe = hosting::install(&self.dir)?;
        let mut lines = Vec::new();
        say("Letting it through Windows Firewall…");
        lines.push(match firewall::ensure_server(&exe) {
            firewall::Outcome::Present | firewall::Outcome::Added => line("Windows Firewall", "Server allowed", "good", "Windows lets players reach the server."),
            firewall::Outcome::Declined => line(
                "Windows Firewall",
                "Not allowed",
                "bad",
                "Windows wasn't allowed to let the server in: players on other PCs can't reach it. Stop and start again to be asked again.",
            ),
            firewall::Outcome::Failed(why) => line(
                "Windows Firewall",
                "Couldn't change it",
                "bad",
                &format!("{why}. Players on other PCs may not reach the server."),
            ),
        });
        let locals = hosting::local_addresses();
        let lan = locals.iter().copied().find(|ip| !is_vpn(*ip));

        let mut forwards = Vec::new();
        let mut outside = None;
        let mut public = None;
        if internet {
            say("Asking your router to forward the server's ports…");
            let (web, mut more) = forward_ports(lan, &mut forwards);
            lines.append(&mut more);
            public = web.map(|(address, _)| address).or_else(|| {
                say("Finding your address on the internet…");
                seen_from_outside()
            });
            outside = public.zip(web.map(|(_, port)| port)).map(|(address, web_port)| Outside { address, web_port });
            match (public, web) {
                (None, _) => lines.push(line(
                    "Internet",
                    "Not found",
                    "warn",
                    "Your address on the internet couldn't be found: friends on your network or VPN can still play.",
                )),
                // Forwarded by hand, the code is the host's to write: the address it needs.
                (Some(ip), None) => lines.push(line(
                    "Internet",
                    "No join code",
                    "warn",
                    &format!(
                        "Your address on the internet is {ip}. Your router didn't forward the web port, so there's no internet join code: forward a TCP port (8080, say) to this PC's port 80 in your router, then send friends {ip}:8080 followed by the # and fingerprint of any code below."
                    ),
                )),
                (Some(_), Some(_)) => {}
            }
        }

        say("Starting the server…");
        // Players on the internet are given the public address (forwarded by hand, too).
        let address = public.map(IpAddr::V4).or_else(|| lan.map(IpAddr::V4)).unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        // For this PC's networks alone, each player gets the server's address on their own.
        let config = hosting::write_settings(&self.dir, address, !internet)?;
        let hosted = hosting::Hosted::start(&self.dir, &exe, &config)?;
        let forwards = Arc::new(Mutex::new(forwards));
        let renewing = Arc::new(Mutex::new(true));
        spawn_renewal(Arc::clone(&forwards), Arc::clone(&renewing), lan);
        let lines = Arc::new(Mutex::new(lines));
        *lock(&self.running) = Some(Running {
            hosted,
            forwards,
            renewing,
            lines: Arc::clone(&lines),
            outside,
        });

        if internet {
            say("Checking it from outside…");
            let ready = std::time::Instant::now();
            while !self.answering() && ready.elapsed() < READY_WAIT {
                std::thread::sleep(Duration::from_millis(500));
            }
            let mut checked = match outside {
                Some(o) => check_from_outside(o),
                None => vec![line(
                    "From outside",
                    "Not checked",
                    "warn",
                    if public.is_some() {
                        "Without the web port forwarded, the community server can't check it: ask a friend to join to be sure."
                    } else {
                        "Without an address on the internet, friends there can't reach the server."
                    },
                )],
            };
            lock(&lines).append(&mut checked);
        }
        Ok(())
    }

    /// Whether the running server answers on this PC.
    fn answering(&self) -> bool {
        lock(&self.running).as_mut().is_some_and(|r| matches!(r.hosted.state(), hosting::State::Running { .. }))
    }

    /// Stops the server and takes its forwards away.
    pub fn stop(&self) {
        let Some(running) = lock(&self.running).take() else { return };
        *lock(&running.renewing) = false;
        running.hosted.stop();
        let forwards = std::mem::take(&mut *lock(&running.forwards));
        for mapping in forwards {
            mapping.remove();
        }
    }

    /// How hosting stands now.
    pub fn status(&self) -> Status {
        let log = hosting::log_tail(&self.dir, 80);
        if let Some(said) = lock(&self.starting).clone() {
            let lines = lock(&self.running).as_ref().map(|r| lock(&r.lines).clone()).unwrap_or_default();
            return Status {
                state: "starting",
                said,
                lines,
                log,
                ..Status::default()
            };
        }
        let mut running = lock(&self.running);
        let Some(r) = running.as_mut() else {
            let failed = lock(&self.failed).clone();
            return Status {
                state: if failed.is_some() { "failed" } else { "stopped" },
                said: failed.unwrap_or_default(),
                log,
                ..Status::default()
            };
        };
        let (state, said, players) = match r.hosted.state() {
            hosting::State::Running { players } => ("running", String::new(), players),
            hosting::State::Starting | hosting::State::Stopped => ("starting", "Starting the server…".into(), 0),
            hosting::State::Failed(why) => ("failed", why, 0),
        };
        let outside = r.outside;
        let lines = lock(&r.lines).clone();
        drop(running);
        let codes = if state == "running" { self.codes(outside) } else { Vec::new() };
        if state == "failed" {
            *lock(&self.failed) = Some(said.clone());
            self.stop();
        }
        Status {
            state,
            said,
            players,
            lines,
            codes,
            log,
        }
    }

    /// The join codes: the internet's (when it's let in), then each network's this PC is on.
    fn codes(&self, outside: Option<Outside>) -> Vec<Code> {
        let Some(fingerprint) = self.fingerprint() else { return Vec::new() };
        let mut codes = Vec::new();
        if let Some(o) = outside {
            codes.push(Code {
                place: "Friends on the internet".into(),
                code: server::make_join_code(&o.address.to_string(), o.web_port, &fingerprint),
            });
        }
        for local in hosting::local_addresses() {
            let place = if is_vpn(local) { "Friends on your VPN (Tailscale)" } else { "Friends on your network" };
            codes.push(Code {
                place: place.into(),
                code: server::make_join_code(&local.to_string(), hosting::HTTP_PORT, &fingerprint),
            });
        }
        codes
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Asks the router to forward the web port (on the first port of [`WEB_PORTS`] it gives, or one
/// it picks) and the game's UDP ports, keeping each forward in `forwards`: the public address
/// and the port the web side is reached on (when forwarded), and a line for each.
fn forward_ports(lan: Option<Ipv4Addr>, forwards: &mut Vec<portmap::Mapping>) -> (Option<(Ipv4Addr, u16)>, Vec<Line>) {
    let mut lines = Vec::new();
    let mut web = None;
    let mut refused = String::new();
    for external in WEB_PORTS {
        match portmap::map_with(Protocol::Tcp, hosting::HTTP_PORT, external, lan, LEASE, DESCRIPTION) {
            Ok(mapping) => {
                let (address, port) = (*mapping.public.ip(), mapping.public.port());
                let other = if port == hosting::HTTP_PORT {
                    String::new()
                } else {
                    format!(" Your router wouldn't give port 80 itself, so friends' games are sent to port {port} (the join code says it).")
                };
                lines.push(line(
                    "Web port (TCP)",
                    "Forwarded",
                    "good",
                    &format!("Your router forwards {address}:{port} to this PC ({}): the online config and HTTPS.{other}", mapping.how),
                ));
                web = Some((address, port));
                forwards.push(mapping);
                break;
            }
            Err(e) => refused = e,
        }
    }
    if web.is_none() {
        lines.push(line("Web port (TCP)", "Not forwarded", "bad", &short(&refused)));
    }
    for (port, label) in UDP_PORTS {
        match portmap::map_with(Protocol::Udp, port, port, lan, LEASE, DESCRIPTION) {
            Ok(mapping) if mapping.public.port() == port => {
                lines.push(line(
                    label,
                    "Forwarded",
                    "good",
                    &format!("Your router forwards {}:{port} to this PC ({}).", mapping.public.ip(), mapping.how),
                ));
                forwards.push(mapping);
            }
            Ok(mapping) => {
                lines.push(line(
                    label,
                    "Another port",
                    "bad",
                    &format!(
                        "Your router forwards {} instead: friends' games need port {port} itself. Forward it to this PC in your router's settings.",
                        mapping.public
                    ),
                ));
                forwards.push(mapping);
            }
            Err(e) => lines.push(line(label, "Not forwarded", "bad", &short(&e))),
        }
    }
    (web, lines)
}

/// The community server's check of the server from outside: whether its web port and the game's
/// ports answer at `o`.
fn check_from_outside(o: Outside) -> Vec<Line> {
    let body = serde_json::json!({ "web_port": o.web_port, "udp_ports": UDP_PORTS.map(|(p, _)| p) }).to_string();
    let headers = [("Content-Type", "application/json".to_owned())];
    let answer = crosscom_client_common::http::request(server::COMMUNITY, "POST", "/api/check-host", &headers, body.as_bytes(), Duration::from_secs(30));
    let answer = match answer {
        Ok(a) if a.status == 200 => a,
        Ok(a) if a.status == 404 => {
            return vec![line(
                "From outside",
                "Not checked",
                "muted",
                "The community server doesn't check hosted servers yet: ask a friend to join to be sure.",
            )];
        }
        Ok(a) => {
            let why = serde_json::from_slice::<serde_json::Value>(&a.body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("it answered {}", a.status));
            return vec![line("From outside", "Not checked", "warn", &format!("The community server couldn't check: {why}."))];
        }
        Err(e) => return vec![line("From outside", "Not checked", "warn", &format!("The community server couldn't be asked: {e}"))],
    };
    let v: serde_json::Value = serde_json::from_slice(&answer.body).unwrap_or_default();
    outside_lines(o, &v)
}

/// What the community server's answer `v` says about reaching the server at `o`.
fn outside_lines(o: Outside, v: &serde_json::Value) -> Vec<Line> {
    let seen = v["address"].as_str().unwrap_or_default().to_owned();
    let mut lines = Vec::new();
    if !seen.is_empty() && seen != o.address.to_string() {
        lines.push(line(
            "From outside",
            "Another address",
            "warn",
            &format!(
                "The internet sees this PC at {seen}, not {} as your router says: another router or your provider may be in front of yours.",
                o.address
            ),
        ));
    }
    let web_ok = v["web"] == "ok";
    let udp_ok: Vec<bool> = v["udp"].as_array().map(|a| a.iter().map(|p| p["answers"] == true).collect()).unwrap_or_default();
    let all = web_ok && udp_ok.len() == UDP_PORTS.len() && udp_ok.iter().all(|ok| *ok);
    if all {
        lines.push(line(
            "From outside",
            "Reachable",
            "good",
            &format!("The community server reached your server at {}: friends on the internet can join.", o.address),
        ));
        return lines;
    }
    let mut failing = Vec::new();
    if !web_ok {
        failing.push(format!("the web port (TCP {})", o.web_port));
    }
    for ((port, _), ok) in UDP_PORTS.iter().zip(udp_ok.iter().copied().chain(std::iter::repeat(false))) {
        if !ok {
            failing.push(format!("UDP {port}"));
        }
    }
    lines.push(line(
        "From outside",
        "Not reachable",
        "bad",
        &format!(
            "The community server couldn't reach {} at {}: friends on the internet can't join. Check your router forwards them to this PC, and that nothing else (another router, your provider) is in front of it.",
            failing.join(", "),
            o.address
        ),
    ));
    lines
}

/// Whether `ip` is a VPN's (Tailscale's and others' 100.64.0.0/10).
fn is_vpn(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

fn line(label: &str, value: &str, tone: &'static str, detail: &str) -> Line {
    Line {
        label: label.into(),
        value: value.into(),
        tone,
        detail: detail.into(),
    }
}

/// The router's reasons, shortened for a line.
fn short(e: &str) -> String {
    if e.contains("isn't a public one") {
        return "Your internet provider or another router is in front of yours (carrier-grade NAT): friends on the internet can't reach a server here. Friends on your network or a VPN such as Tailscale can.".into();
    }
    if e.matches("no answer").count() >= 3 {
        return "Your router didn't answer UPnP, PCP or NAT-PMP: turn UPnP on in its settings, or forward the port to this PC yourself.".into();
    }
    format!("Your router didn't forward it: {}", e.chars().take(200).collect::<String>())
}

/// Where the community server sees this PC: its address on the internet.
fn seen_from_outside() -> Option<Ipv4Addr> {
    let ports = crosscom_client_net::check::server_udp(server::COMMUNITY).ok()?;
    crosscom_client_net::check::mapping(&ports).0.map(|seen| *seen.ip())
}

/// Renews the forwards every [`RENEW`] while `renewing` holds.
fn spawn_renewal(forwards: Arc<Mutex<Vec<portmap::Mapping>>>, renewing: Arc<Mutex<bool>>, lan: Option<Ipv4Addr>) {
    let _ = std::thread::Builder::new().name("host forwards".into()).spawn(move || loop {
        std::thread::sleep(RENEW);
        if !*lock(&renewing) {
            return;
        }
        let mut kept = lock(&forwards);
        let mut renewed = Vec::new();
        for mapping in kept.drain(..) {
            let (protocol, local, public) = (mapping.protocol(), mapping.local_port(), mapping.public.port());
            match portmap::map_with(protocol, local, public, lan, LEASE, DESCRIPTION) {
                Ok(again) => renewed.push(again),
                Err(_) => renewed.push(mapping),
            }
        }
        *kept = renewed;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o() -> Outside {
        Outside {
            address: Ipv4Addr::new(203, 0, 114, 7),
            web_port: 8080,
        }
    }

    #[test]
    fn reads_the_check_from_outside() {
        let all = serde_json::json!({"address": "203.0.114.7", "web": "ok", "udp": [{"port": 21030, "answers": true}, {"port": 21031, "answers": true}]});
        let lines = outside_lines(o(), &all);
        assert_eq!((lines.len(), lines[0].value.as_str(), lines[0].tone), (1, "Reachable", "good"));

        let some = serde_json::json!({"address": "203.0.114.7", "web": "no answer", "udp": [{"port": 21030, "answers": true}, {"port": 21031, "answers": false}]});
        let lines = outside_lines(o(), &some);
        assert_eq!(lines[0].tone, "bad");
        assert!(lines[0].detail.contains("the web port (TCP 8080), UDP 21031 at 203.0.114.7"), "{}", lines[0].detail);

        let elsewhere = serde_json::json!({"address": "198.51.100.9", "web": "ok", "udp": [{"answers": true}, {"answers": true}]});
        let lines = outside_lines(o(), &elsewhere);
        assert_eq!((lines[0].value.as_str(), lines[1].value.as_str()), ("Another address", "Reachable"));
    }
}
