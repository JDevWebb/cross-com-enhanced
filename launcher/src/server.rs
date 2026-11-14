//! Talking to a Cross-Com server over HTTP, as the game does first: checking it answers with
//! its online config, asking how it is (its version, players online and news), signing in to an
//! account and making one when the server allows sign-up.
//!
//! Passwords go over HTTPS. Only a server on this machine or a private network (a LAN, or a VPN
//! such as Tailscale) may take them over plain HTTP, when it has no working HTTPS; a server on the
//! internet without it is refused. What carries no password (the online config, the info) stays
//! on plain HTTP, as the game itself only speaks that.

use std::net::IpAddr;
use std::net::ToSocketAddrs;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

const ONLINE_CONFIG: &str = "/OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc";
const TIMEOUT: Duration = Duration::from_secs(8);
/// How long to wait for a connection: a server without HTTPS should be found out quickly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Where a server's host learns to set up HTTPS.
pub const HTTPS_HELP: &str = "https://github.com/JDevWebb/cross-com-enhanced/blob/main/docs/hosting.md#https-with-caddy";

/// The server as typed: a host name or address, perhaps with `http://` or `https://` or a
/// trailing `/`. Port 80 is the game's, so none other is taken.
#[must_use]
pub fn host(typed: &str) -> String {
    let t = typed.trim();
    let t = t.strip_prefix("http://").or_else(|| t.strip_prefix("https://")).unwrap_or(t);
    t.trim_end_matches('/').to_owned()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .http_status_as_error(false)
        // A redirect would send the password on somewhere else.
        .max_redirects(0)
        .build()
        .into()
}

/// How a password went to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// Over HTTPS.
    Encrypted,
    /// Over plain HTTP, to a server on this machine or a private network without HTTPS.
    Unencrypted,
}

/// Whether an address is this machine's or on a private network: a LAN (RFC 1918, IPv6 unique
/// local), link-local, or the shared range VPNs such as Tailscale use (100.64.0.0/10).
#[must_use]
pub fn is_private(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Whether passwords may go to `host` over plain HTTP: every address it has is private (see
/// [`is_private`]). A name that can't be looked up isn't.
fn may_send_unencrypted(host: &str) -> bool {
    let name = host.trim_start_matches('[').trim_end_matches(']');
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = name.parse::<IpAddr>() {
        return is_private(ip);
    }
    let addresses: Vec<IpAddr> = (name, 80).to_socket_addrs().map(|a| a.map(|a| a.ip()).collect()).unwrap_or_default();
    all_private(&addresses)
}

fn all_private(addresses: &[IpAddr]) -> bool {
    !addresses.is_empty() && addresses.iter().all(|a| is_private(*a))
}

/// Why a password can't go to a server on the internet whose HTTPS didn't work.
fn needs_https(host: &str, error: &str) -> String {
    format!(
        "{host} didn't answer over HTTPS ({error}). Passwords only go to a server on the internet \
         encrypted, so it needs HTTPS; whoever runs it can set it up: {HTTPS_HELP}"
    )
}

/// Where the server says to sign in, from its online config (the address the game will use).
pub fn check(server: &str) -> Result<String, String> {
    let url = format!("http://{}{ONLINE_CONFIG}", host(server));
    let mut response = agent().get(&url).call().map_err(|e| format!("couldn't reach {}: {e}", host(server)))?;
    if response.status() != 200 {
        return Err(format!("{} answered {}: is it a Cross-Com server?", host(server), response.status()));
    }
    let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    sign_in_address(&body).ok_or_else(|| format!("{} didn't say where to sign in: is it a Cross-Com server?", host(server)))
}

/// The address and port in the online config's `SandboxUrl`.
fn sign_in_address(online_config: &str) -> Option<String> {
    let items: serde_json::Value = serde_json::from_str(online_config).ok()?;
    let url = items.as_array()?.iter().find(|i| i["Name"] == "SandboxUrl")?["Values"][0].as_str()?.to_owned();
    let rest = url.strip_prefix("prudp:/")?;
    let field = |name: &str| rest.split(';').find_map(|kv| kv.strip_prefix(name)).map(str::to_owned);
    Some(format!("{}:{}", field("address=")?, field("port=")?))
}

/// How the server is, from `GET /api/info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub version: String,
    pub players_online: u32,
    pub news: Vec<News>,
}

/// A news item, as the game shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct News {
    pub title: String,
    pub body: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The Ghost Feed view it's shown in: `multiplayer`, `campaign`, `guerrilla`, or none.
    #[serde(default)]
    pub shows: Option<String>,
}

/// How the server is; `None` when it answers as a Cross-Com server but is older than its API.
pub fn info(server: &str) -> Result<Option<Info>, String> {
    let url = format!("http://{}/api/info", host(server));
    let mut response = agent().get(&url).call().map_err(|e| format!("couldn't reach {}: {e}", host(server)))?;
    match response.status().as_u16() {
        200 => {
            let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
            serde_json::from_str(&body).map(Some).map_err(|_| format!("{}'s answer made no sense", host(server)))
        }
        404 => check(server).map(|_| None),
        status => Err(format!("{} answered {status}", host(server))),
    }
}

/// Checks a name and password with the server: the account's ID, `None` when the server is
/// older than this check (the game's own sign-in will tell), or why it refused; and how the
/// password went.
pub fn sign_in(server: &str, name: &str, password: &str) -> Result<(Option<u32>, Sent), String> {
    let body = json!({ "name": name.trim(), "password": password }).to_string();
    let (status, text, sent) = post_password(server, "/api/login", &body)?;
    if status == 404 {
        return Ok((None, sent));
    }
    answer(status, &text).map(|pid| (Some(pid), sent))
}

/// Makes an account; its ID and how the password went, or the server's reason for refusing.
pub fn sign_up(server: &str, name: &str, password: &str, code: &str) -> Result<(u32, Sent), String> {
    let code = Some(code.trim()).filter(|c| !c.is_empty());
    let body = json!({ "name": name.trim(), "password": password, "code": code }).to_string();
    let (status, text, sent) = post_password(server, "/accounts", &body)?;
    answer(status, &text).map(|pid| (pid, sent))
}

/// Posts JSON carrying a password: over HTTPS, or plain HTTP to a private server whose HTTPS
/// didn't work. The status, the answer's text and how it went.
fn post_password(server: &str, path: &str, body: &str) -> Result<(u16, String, Sent), String> {
    let host = host(server);
    let https = match post(&format!("https://{host}{path}"), body) {
        Ok((status, text)) => return Ok((status, text, Sent::Encrypted)),
        Err(e) => e,
    };
    if !may_send_unencrypted(&host) {
        return Err(needs_https(&host, &https));
    }
    let (status, text) = post(&format!("http://{host}{path}"), body).map_err(|e| format!("couldn't reach {host}: {e}"))?;
    if (300..400).contains(&status) {
        // A proxy sending passwords to its HTTPS, which didn't work from here.
        return Err(format!("{host} takes passwords over HTTPS only, and its HTTPS didn't work: {https}"));
    }
    Ok((status, text, Sent::Unencrypted))
}

/// Posts JSON; the status and the answer's text.
fn post(url: &str, body: &str) -> Result<(u16, String), String> {
    let mut response = agent().post(url).header("Content-Type", "application/json").send(body).map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    Ok((status, response.body_mut().read_to_string().unwrap_or_default()))
}

fn answer(status: u16, body: &str) -> Result<u32, String> {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    match status {
        200 | 201 => json["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| "the server's answer made no sense".into()),
        404 => Err("this server doesn't let players make accounts; ask whoever runs it".into()),
        _ => Err(json["error"].as_str().map_or_else(|| format!("the server refused ({status})"), str::to_owned)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_server_as_people_type_it() {
        assert_eq!(host(" http://203.0.113.10/ "), "203.0.113.10");
        assert_eq!(host("play.example.org"), "play.example.org");
        assert_eq!(host("https://play.example.org/"), "play.example.org");
    }

    #[test]
    fn knows_private_addresses() {
        let private = |s: &str| is_private(s.parse().unwrap());
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.20",
            "169.254.10.1",
            "100.64.0.1",
            "100.101.102.103",
            "100.127.255.255",
            "::1",
            "fd7a:115c:a1e0::1",
            "fc00::1",
            "fe80::1",
            "::ffff:192.168.1.20",
        ] {
            assert!(private(ip), "{ip}");
        }
        for ip in [
            "203.0.113.10",
            "198.51.100.1",
            "8.8.8.8",
            "172.32.0.1",
            "172.15.255.255",
            "100.63.255.255",
            "100.128.0.1",
            "192.169.0.1",
            "2001:db8::1",
            "2606:4700::1111",
            "::ffff:203.0.113.10",
        ] {
            assert!(!private(ip), "{ip}");
        }
    }

    #[test]
    fn plain_http_for_passwords_only_on_a_private_network() {
        assert!(may_send_unencrypted("192.168.1.20"));
        assert!(may_send_unencrypted("100.100.1.2"), "a Tailscale address");
        assert!(may_send_unencrypted("localhost"));
        assert!(may_send_unencrypted("[fd00::5]"));
        assert!(!may_send_unencrypted("203.0.113.10"));
        assert!(!may_send_unencrypted("2001:db8::1"));
        assert!(!may_send_unencrypted("no-such-host.invalid"), "a name that can't be looked up");
        // A name counts by where it points: every address private.
        let ips = |list: &[&str]| list.iter().map(|s| s.parse().unwrap()).collect::<Vec<IpAddr>>();
        assert!(all_private(&ips(&["192.168.1.20", "fd00::5"])));
        assert!(!all_private(&ips(&["192.168.1.20", "203.0.113.10"])));
        assert!(!all_private(&[]));
        let why = needs_https("play.example.org", "connection refused");
        assert!(why.contains("play.example.org") && why.contains("connection refused") && why.contains(HTTPS_HELP), "{why}");
    }

    #[test]
    fn reads_where_to_sign_in() {
        let config = r#"[{"Name":"SandboxUrl","Values":["prudp:/address=203.0.113.10;port=21030"]},{"Name":"SandboxUrlWS","Values":["203.0.113.10:21030"]}]"#;
        assert_eq!(sign_in_address(config).as_deref(), Some("203.0.113.10:21030"));
        assert_eq!(sign_in_address("<html>"), None);
    }

    #[test]
    fn explains_the_servers_answers() {
        assert_eq!(answer(201, r#"{"pid":1048577}"#), Ok(1_048_577));
        assert_eq!(answer(409, r#"{"error":"that name is taken"}"#), Err("that name is taken".into()));
        assert!(answer(404, "").unwrap_err().contains("doesn't let players"));
        assert_eq!(answer(502, "gateway"), Err("the server refused (502)".into()));
        assert_eq!(answer(200, r#"{"pid":1048576}"#), Ok(1_048_576));
        assert_eq!(answer(401, r#"{"error":"wrong name or password"}"#), Err("wrong name or password".into()));
    }

    #[test]
    fn reads_the_servers_info() {
        let info: Info = serde_json::from_str(
            r#"{"version":"0.1.0","players_online":3,"news":[{"title":"Hi","body":"x","date":"2026-11-15","shows":null},{"title":"XP","body":"y","date":"2026-11-20","shows":"multiplayer"}],"later":1}"#,
        )
        .unwrap();
        assert_eq!(info.players_online, 3);
        assert_eq!(info.news[0].shows, None);
        assert_eq!(info.news[1].shows.as_deref(), Some("multiplayer"));
    }
}
