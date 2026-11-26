//! Talking to a Cross-Com server over HTTP, as the game does first: checking it answers with
//! its online config, asking how it is (its version, players online and news), and signing in to
//! the player's account there with their identity (`crate::identity`), or making one when the
//! server allows sign-up. Either way the server answers a game token, which the game signs in
//! with: players have no password.
//!
//! The API goes over HTTPS, the game token above all. Only a server set up by its private address (a
//! LAN, or a VPN such as Tailscale, or this machine) may be asked over plain HTTP, and only when
//! nothing answers on its HTTPS port at all; a server set up by name, or on the internet, without
//! HTTPS is refused (`crosscom_client_common::http`, which the client DLL shares). The online
//! config is asked over plain HTTP, as the game asks it.

use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

use crate::identity::Identity;

const ONLINE_CONFIG: &str = "/OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc";
const TIMEOUT: Duration = Duration::from_secs(8);
/// How long to wait for a connection: a server without HTTPS should be found out quickly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// The community test network: the server a new setup starts with, so a new player only signs in
/// or makes an account. Players can choose any other server instead, and come back to it.
pub const COMMUNITY: &str = "grfs-test.jdevwebb.net";
/// What the launcher calls it.
pub const COMMUNITY_NAME: &str = "Community test network";
/// A note shown beside its name while it isn't open yet; "" once it is.
pub const COMMUNITY_STATUS: &str = "";
/// Where a server's host learns to set up HTTPS.
pub use crosscom_client_common::http::HTTPS_HELP;

/// The server as typed: a host name or address, perhaps with `http://` or `https://` or a
/// trailing `/`, and perhaps a join code's fingerprint after `#` (left out here; see
/// [`join_code`]). Port 80 is the game's, so none other is taken.
#[must_use]
pub fn host(typed: &str) -> String {
    let t = typed.trim();
    let t = t.split_once('#').map_or(t, |(host, _)| host).trim();
    let t = t.strip_prefix("http://").or_else(|| t.strip_prefix("https://")).unwrap_or(t);
    t.trim_end_matches('/').to_owned()
}

/// A server as typed, or a host's join code (`HOST#FINGERPRINT`, its HTTPS certificate's): the
/// host, and the fingerprint when there's one.
pub fn join_code(typed: &str) -> Result<(String, Option<Fingerprint>), String> {
    let host = host(typed);
    match typed.split_once('#') {
        None => Ok((host, None)),
        Some((_, fingerprint)) => crosscom_client_common::http::parse_fingerprint(fingerprint.trim())
            .map(|f| (host, Some(f)))
            .ok_or_else(|| "That join code isn't whole: copy it again from the host's launcher (Host › Join code).".to_owned()),
    }
}

/// The join code friends set up the launcher with, for a server at `host` whose web side (HTTP
/// and HTTPS, as the router forwards it) is on `port`, and whose HTTPS certificate is
/// `fingerprint`: `HOST:PORT#FINGERPRINT`, without `:80`.
#[must_use]
pub fn make_join_code(host: &str, port: u16, fingerprint: &Fingerprint) -> String {
    let at = if port == crosscom_client_common::http::HTTP_PORT {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };
    format!("{at}#{}", crosscom_client_common::http::fingerprint_text(fingerprint))
}

/// The server typed, ready to be asked: its host, with HTTPS to it taking only the certificate
/// a join code names (or any a certificate authority vouches for, without one).
pub fn chosen(typed: &str) -> Result<String, String> {
    let (host, fingerprint) = join_code(typed)?;
    crosscom_client_common::http::pin(&host, fingerprint);
    Ok(host)
}

/// The server as Setup keeps it while it's being chosen: the host, and a join code's
/// fingerprint after `#`.
#[must_use]
pub fn typed(text: &str) -> String {
    match text.split_once('#') {
        Some((_, fingerprint)) => format!("{}#{}", host(text), fingerprint.trim()),
        None => host(text),
    }
}

pub use crosscom_client_common::http::Fingerprint;

/// Whether `server` (as typed) is the community test network.
#[must_use]
pub fn is_community(server: &str) -> bool {
    host(server).eq_ignore_ascii_case(COMMUNITY)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .http_status_as_error(false)
        // A redirect would send the sign-in on somewhere else.
        .max_redirects(0)
        .build()
        .into()
}

pub use crosscom_client_common::http::is_private;
pub(crate) use crosscom_client_common::http::may_go_plain as may_send_unencrypted;
pub(crate) use crosscom_client_common::http::nothing_answered;

/// Whether a sign-in may go to `host` over plain HTTP after HTTPS failed with `error`.
fn falls_back_to_plain(host: &str, error: &ureq::Error) -> bool {
    may_send_unencrypted(host) && nothing_answered(error)
}

/// Why a sign-in didn't go to a server whose HTTPS failed with `error`.
fn needs_https(host: &str, error: &ureq::Error) -> String {
    if crosscom_client_common::http::lookup_failed(error) {
        return crosscom_client_common::http::lookup_failed_said(host, error);
    }
    if may_send_unencrypted(host) {
        // A private address whose HTTPS answered but failed: not a server without HTTPS.
        format!("{host}'s HTTPS didn't work ({error}), so nothing was sent. Whoever runs the server can check its HTTPS: {HTTPS_HELP}")
    } else {
        format!(
            "{host} didn't answer over HTTPS ({error}). It may be down for now; try again in a moment. If it \
             never answers, it may not have HTTPS: the launcher only signs in to a server set up by name or on \
             the internet encrypted, so it needs HTTPS; whoever runs it can set it up: {HTTPS_HELP}. A server \
             on your own network or VPN without HTTPS can be set up by its IP address."
        )
    }
}

/// Where the server says to sign in, from its online config (the address the game will use).
pub fn check(server: &str) -> Result<String, String> {
    let url = format!("http://{}{ONLINE_CONFIG}", host(server));
    let mut response = agent().get(&url).call().map_err(|e| {
        if crosscom_client_common::http::lookup_failed(&e) {
            crosscom_client_common::http::lookup_failed_said(&host(server), &e)
        } else {
            format!("couldn't reach {}: {e}", host(server))
        }
    })?;
    if response.status() != 200 {
        return Err(format!("{} answered {}: is it a Cross-Com server?", host(server), response.status()));
    }
    let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let address = sign_in_address(&body).ok_or_else(|| format!("{} didn't say where to sign in: is it a Cross-Com server?", host(server)))?;
    if let Some(other) = api_is_other_game(server) {
        return Err(wrong_game(server, &other));
    }
    Ok(address)
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

/// The game a Cross-Com server for Future Soldier says it's for, in `/api/info`.
pub const GAME: &str = "ghost-recon-future-soldier";

/// How the server is; `None` when it answers as a Cross-Com server but is older than its API.
/// An error for another game's server: a machine may host two behind one Caddy, and an address
/// reaches the other's API (see `other_game`).
pub fn info(server: &str) -> Result<Option<Info>, String> {
    let response = api_get(server, "/api/info")?;
    match response.status {
        200 => {
            let value: serde_json::Value = serde_json::from_slice(&response.body).map_err(|_| format!("{}'s answer made no sense", host(server)))?;
            if let Some(other) = other_game(&value) {
                return Err(wrong_game(server, &other));
            }
            serde_json::from_value(value).map(Some).map_err(|_| format!("{}'s answer made no sense", host(server)))
        }
        404 => check(server).map(|_| None),
        status => Err(format!("{} answered {status}", host(server))),
    }
}

/// The name of the other game's server `/api/info` came from, when it isn't this game's: one
/// saying another `game`, or none at all (a server for another game doesn't say this one's).
fn other_game(info: &serde_json::Value) -> Option<String> {
    if info["game"].as_str() == Some(GAME) {
        return None;
    }
    Some(info["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or("another game's server").to_owned())
}

fn wrong_game(server: &str, other: &str) -> String {
    format!(
        "{} reaches {other}, not a Future Soldier server. The machine may host both: set this server up by its own domain.",
        host(server)
    )
}

/// After the online config answered, whether the same address's API is another game's: what
/// `check` refuses. Anything else (no API, no answer) is left to the game to find out.
fn api_is_other_game(server: &str) -> Option<String> {
    let response = api_get(server, "/api/info").ok()?;
    if response.status != 200 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&response.body).ok()?;
    other_game(&value)
}

/// Asks the server's API for `path`, over HTTPS (see `crosscom_client_common::http`).
fn api_get(server: &str, path: &str) -> Result<crosscom_client_common::http::Response, String> {
    crosscom_client_common::http::request(&host(server), "GET", path, &[("Accept", "application/json".into())], &[], TIMEOUT).map_err(|e| e.0)
}

/// An account on a server, as making it or signing in to it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The account's ID.
    pub pid: u32,
    /// Its name, which other players see.
    pub name: String,
    /// The game token the game signs in with (written as its `password`).
    pub token: String,
}

/// Makes the account `name` on the server, owned by `identity` (with the server's sign-up code,
/// if it has one); or the server's reason for refusing.
pub fn sign_up(server: &str, identity: &Identity, name: &str, code: &str) -> Result<Account, String> {
    let body = signup_body(&host(server), identity, name, code, crosscom_identity::now());
    let (status, text) = post_json(server, "/accounts", &body.to_string())?;
    match status {
        404 => Err("this server doesn't let players make accounts; ask whoever runs it".into()),
        _ => account(status, &text),
    }
}

/// Signs `identity` in to its account on the server; `None` when it has none there.
pub fn key_login(server: &str, identity: &Identity) -> Result<Option<Account>, String> {
    let body = login_body(&host(server), identity, crosscom_identity::now());
    let (status, text) = post_json(server, "/api/key-login", &body.to_string())?;
    match status {
        404 => Ok(None),
        _ => account(status, &text).map(Some),
    }
}

/// What `POST /accounts` is sent: the name, and the identity's signature for it on `host` at
/// `time`.
fn signup_body(host: &str, identity: &Identity, name: &str, code: &str, time: i64) -> serde_json::Value {
    let name = name.trim();
    json!({
        "name": name,
        "global_id": identity.global_id(),
        "time": time,
        "host": host,
        "signature": identity.sign_signup(host, name, time),
        "code": Some(code.trim()).filter(|c| !c.is_empty()),
    })
}

/// What `POST /api/key-login` is sent: the identity's signature for `host` at `time`.
fn login_body(host: &str, identity: &Identity, time: i64) -> serde_json::Value {
    json!({
        "global_id": identity.global_id(),
        "time": time,
        "host": host,
        "signature": identity.sign_login(host, time),
    })
}

/// Posts JSON to the server's API: over HTTPS, or plain HTTP to a server at a private address
/// where nothing answers on the HTTPS port. The status and the answer's text.
fn post_json(server: &str, path: &str, body: &str) -> Result<(u16, String), String> {
    let host = host(server);
    // A server hosted from a player's launcher: its own certificate, which `http` checks.
    if crosscom_client_common::http::is_pinned(&host) {
        let headers = [("Content-Type", "application/json".to_owned())];
        let answer = crosscom_client_common::http::request(&host, "POST", path, &headers, body.as_bytes(), TIMEOUT).map_err(|e| e.0)?;
        return Ok((answer.status, String::from_utf8_lossy(&answer.body).into_owned()));
    }
    let https = match post(&format!("https://{host}{path}"), body) {
        Ok(answer) => return Ok(answer),
        Err(e) => e,
    };
    if !falls_back_to_plain(&host, &https) {
        return Err(needs_https(&host, &https));
    }
    let (status, text) = post(&format!("http://{host}{path}"), body).map_err(|e| format!("couldn't reach {host}: {e}"))?;
    if (300..400).contains(&status) {
        // A proxy sending the API to its HTTPS, which didn't work from here.
        return Err(format!("{host} takes sign-ins over HTTPS only, and its HTTPS didn't work: {https}"));
    }
    Ok((status, text))
}

/// Posts JSON; the status and the answer's text. No `Origin`: the server refuses posts from web
/// pages.
fn post(url: &str, body: &str) -> Result<(u16, String), ureq::Error> {
    let mut response = agent().post(url).header("Content-Type", "application/json").send(body)?;
    let status = response.status().as_u16();
    Ok((status, response.body_mut().read_to_string().unwrap_or_default()))
}

/// The account in a server's answer, or why it refused.
fn account(status: u16, body: &str) -> Result<Account, String> {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    if !matches!(status, 200 | 201) {
        return Err(match json["error"].as_str() {
            Some(error) => error.to_owned(),
            None if status == 429 => "the server has had too many sign-ins from here; try again in a minute".into(),
            None => format!("the server refused ({status})"),
        });
    }
    let read = || {
        Some(Account {
            pid: u32::try_from(json["pid"].as_u64()?).ok()?,
            name: json["name"].as_str().filter(|n| !n.is_empty())?.to_owned(),
            token: json["token"].as_str().filter(|t| crosscom_client_common::config::is_game_token(t))?.to_owned(),
        })
    };
    read().ok_or_else(|| "the server's answer made no sense".into())
}

#[cfg(test)]
mod join_code_tests {
    use super::*;

    #[test]
    fn join_codes() {
        let f = [0xabu8; 32];
        let code = make_join_code("203.0.113.7", 80, &f);
        assert_eq!(code, format!("203.0.113.7#{}", "ab".repeat(32)));
        let moved = make_join_code("203.0.113.7", 8080, &f);
        assert_eq!(join_code(&moved), Ok(("203.0.113.7:8080".into(), Some(f))), "the web port, kept with the host");
        assert_eq!(join_code(&code), Ok(("203.0.113.7".into(), Some(f))));
        assert_eq!(join_code(&format!(" https://{code} ")), Ok(("203.0.113.7".into(), Some(f))), "pasted with extra bits");
        assert_eq!(join_code("play.example.org"), Ok(("play.example.org".into(), None)));
        assert!(join_code("203.0.113.7#abc").is_err(), "cut short");
        assert_eq!(host(&code), "203.0.113.7");
        assert_eq!(typed(&format!("  {code}")), code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_another_games_server_by_its_info() {
        let ours = serde_json::json!({"name": "Cross-Com Enhanced", "game": "ghost-recon-future-soldier", "version": "0.1.0"});
        assert_eq!(other_game(&ours), None);
        let other = serde_json::json!({"name": "5th Echelon Enhanced", "version": "0.5.0", "features": []});
        assert_eq!(other_game(&other).as_deref(), Some("5th Echelon Enhanced"));
        assert_eq!(other_game(&serde_json::json!({"game": "something-else"})).as_deref(), Some("another game's server"));
        assert!(wrong_game("203.0.113.10", "5th Echelon Enhanced").contains("own domain"));
    }

    #[test]
    fn takes_the_server_as_people_type_it() {
        assert_eq!(host(" http://203.0.113.10/ "), "203.0.113.10");
        assert_eq!(host("play.example.org"), "play.example.org");
        assert_eq!(host("https://play.example.org/"), "play.example.org");
    }

    #[test]
    fn knows_the_community_network_as_typed() {
        assert!(is_community(COMMUNITY));
        assert!(is_community(" https://GRFS-test.jdevwebb.net/ "));
        assert!(!is_community("play.example.org"));
        assert!(!is_community(""));
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
    fn plain_http_only_to_a_private_address() {
        assert!(may_send_unencrypted("192.168.1.20"));
        assert!(may_send_unencrypted("100.100.1.2"), "a Tailscale address");
        assert!(may_send_unencrypted("localhost"));
        assert!(may_send_unencrypted("[fd00::5]"));
        assert!(may_send_unencrypted("fd00::5"));
        assert!(!may_send_unencrypted("203.0.113.10"));
        assert!(!may_send_unencrypted("2001:db8::1"));
        // Names are never looked up for this, whatever they point at.
        assert!(!may_send_unencrypted("play.example.org"));
        assert!(!may_send_unencrypted("lan-box.local"));
        assert!(!may_send_unencrypted("[play.example.org]"));
    }

    #[test]
    fn plain_http_only_when_nothing_answers_on_https() {
        use std::io::Error as Io;
        use std::io::ErrorKind;
        let refused = || ureq::Error::Io(Io::from(ErrorKind::ConnectionRefused));
        let lan = "192.168.1.20";
        assert!(falls_back_to_plain(lan, &refused()));
        assert!(falls_back_to_plain(lan, &ureq::Error::ConnectionFailed));
        assert!(falls_back_to_plain(lan, &ureq::Error::Timeout(ureq::Timeout::Connect)));
        assert!(falls_back_to_plain(lan, &ureq::Error::Io(Io::from(ErrorKind::HostUnreachable))));
        // Something answered and the encryption failed: never plain HTTP.
        assert!(!falls_back_to_plain(lan, &ureq::Error::Tls("certificate not valid")));
        assert!(!falls_back_to_plain(lan, &ureq::Error::Io(Io::from(ErrorKind::ConnectionReset))));
        assert!(!falls_back_to_plain(lan, &ureq::Error::Timeout(ureq::Timeout::RecvResponse)));
        assert!(!falls_back_to_plain(lan, &ureq::Error::Timeout(ureq::Timeout::Global)));
        // Not a private address: never plain HTTP.
        assert!(!falls_back_to_plain("203.0.113.10", &refused()));
        assert!(!falls_back_to_plain("play.example.org", &refused()));

        let why = needs_https("play.example.org", &refused());
        assert!(why.contains("play.example.org") && why.contains(HTTPS_HELP) && why.contains("IP address"), "{why}");
        let why = needs_https(lan, &ureq::Error::Tls("certificate not valid"));
        assert!(why.contains("nothing was sent") && why.contains("certificate not valid"), "{why}");
    }

    #[test]
    fn reads_where_to_sign_in() {
        let config = r#"[{"Name":"SandboxUrl","Values":["prudp:/address=203.0.113.10;port=21030"]},{"Name":"SandboxUrlWS","Values":["203.0.113.10:21030"]}]"#;
        assert_eq!(sign_in_address(config).as_deref(), Some("203.0.113.10:21030"));
        assert_eq!(sign_in_address("<html>"), None);
    }

    #[test]
    fn explains_the_servers_answers() {
        let token = format!("cct_{}", "a1".repeat(14));
        let made = account(201, &format!(r#"{{"pid":1048577,"name":"Kozak","token":"{token}"}}"#)).unwrap();
        assert_eq!(
            made,
            Account {
                pid: 1_048_577,
                name: "Kozak".into(),
                token
            }
        );
        assert_eq!(account(409, r#"{"error":"that name is taken"}"#), Err("that name is taken".into()));
        assert_eq!(account(502, "gateway"), Err("the server refused (502)".into()));
        assert!(account(429, "").unwrap_err().contains("try again"));
        assert_eq!(account(401, r#"{"error":"bad signature"}"#), Err("bad signature".into()));
        assert!(account(200, r#"{"pid":1048576,"name":"Kozak"}"#).unwrap_err().contains("no sense"), "no token");
        assert!(account(200, r#"{"pid":1048576,"name":"Kozak","token":"hunter2"}"#).is_err(), "not a game token");
    }

    #[test]
    fn signs_for_the_server_as_set_up() {
        let me = Identity::generate();
        let id = me.global_id();
        let up = signup_body("play.example.org", &me, " Kozak ", "", 1_000);
        assert_eq!(
            (up["name"].as_str(), up["host"].as_str(), up["time"].as_i64()),
            (Some("Kozak"), Some("play.example.org"), Some(1_000))
        );
        assert_eq!((up["global_id"].as_str(), &up["code"]), (Some(id.as_str()), &serde_json::Value::Null));
        let message = crosscom_identity::signup_message("play.example.org", "Kozak", 1_000);
        assert!(crosscom_identity::verify(&id, &message, up["signature"].as_str().unwrap()));
        assert_eq!(signup_body("play.example.org", &me, "Kozak", " ABC ", 1_000)["code"], "ABC");

        let login = login_body("play.example.org", &me, 1_000);
        let message = crosscom_identity::login_message("play.example.org", 1_000);
        assert!(crosscom_identity::verify(&id, &message, login["signature"].as_str().unwrap()));
        assert_eq!(login["host"], "play.example.org");
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
