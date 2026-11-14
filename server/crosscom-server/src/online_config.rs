//! The HTTP server: the online config, the first thing the game asks (it answers with where to
//! sign in), the launcher's API (see `api`), and account sign-up for the launcher when it's
//! turned on (see `signup`).

use std::io::Read;
use std::io::Write;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use serde::Serialize;
use slog::info;
use slog::warn;
use slog::Logger;

use crate::api::Api;
use crate::signup::SignUp;

const PATH: &str = "/OnlineConfigService.svc/GetOnlineConfig";
/// How long one request may take in all, however slowly it arrives.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// Requests answered at once; more are turned away. The game asks once as it starts, the
/// launcher as it opens its Play screen.
const MAX_CONNECTIONS: usize = 64;

#[derive(Serialize)]
struct Item {
    #[serde(rename = "Name")]
    name: &'static str,
    #[serde(rename = "Values")]
    values: [String; 1],
}

/// The answer: a JSON list of named values. The game signs in at `SandboxUrl`; the Uplay
/// addresses are asked for too, and point back here.
#[must_use]
pub fn content(public_ip: IpAddr, auth_port: u16) -> String {
    let item = |name, value: String| Item { name, values: [value] };
    let items = [
        item("SandboxUrl", format!("prudp:/address={public_ip};port={auth_port}")),
        item("SandboxUrlWS", format!("{public_ip}:{auth_port}")),
        item(
            "uplay_DownloadServiceUrl",
            format!("{public_ip}/UplayServices/UplayFacade/DownloadServicesRESTXML.svc/REST/XML/?url="),
        ),
        item("uplay_DynContentBaseUrl", format!("{public_ip}/u/Uplay/")),
        item("uplay_DynContentSecureBaseUrl", format!("{public_ip}/")),
        item("uplay_LinkappBaseUrl", format!("{public_ip}/u/Uplay/Packages/linkapp/1.1/")),
        item("uplay_PackageBaseUrl", format!("{public_ip}/u/Uplay/Packages/1.0.1/")),
        item(
            "uplay_WebServiceBaseUrl",
            format!("{public_ip}/UplayServices/UplayFacade/ProfileServicesFacadeRESTXML.svc/REST/"),
        ),
    ];
    serde_json::to_string(&items).unwrap_or_default()
}

/// What the HTTP server answers.
pub struct Http {
    /// The online config (see [`content`]).
    pub content: String,
    /// Sign-up, when the server's settings turn it on.
    pub signup: Option<SignUp>,
    pub api: Api,
    /// Proxies whose `X-Forwarded-For` and `X-Forwarded-Proto` are believed (see `proxy`).
    pub proxies: crate::proxy::Trusted,
}

/// Serves the online config, the launcher's API and sign-up if on, until the process ends.
pub fn serve(logger: &Logger, listen: SocketAddr, http: Http) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen)?;
    info!(logger, "Online config on http://{listen}{PATH}");
    info!(logger, "Launcher API on http://{listen}{}", crate::api::INFO_PATH);
    if http.signup.is_some() {
        info!(logger, "Sign-up on http://{listen}{}", crate::signup::PATH);
    }
    let http = Arc::new(http);
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::AcqRel);
            continue; // dropping the stream closes it
        }
        let logger = logger.clone();
        let open = Arc::clone(&open);
        let http = Arc::clone(&http);
        std::thread::spawn(move || {
            if let Err(e) = answer(&logger, stream, &http) {
                warn!(logger, "HTTP request failed: {e}");
            }
            open.fetch_sub(1, Ordering::AcqRel);
        });
    }
    Ok(())
}

fn answer(logger: &Logger, mut stream: TcpStream, http: &Http) -> std::io::Result<()> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    stream.set_write_timeout(Some(REQUEST_DEADLINE))?;
    let peer = stream.peer_addr()?;
    let mut buf = [0u8; 4096];
    let mut len = 0;
    // Read until the headers end and any body has come (or the buffer is full), within one
    // deadline for the whole request.
    let mut wanted = None;
    while len < buf.len() && wanted.is_none_or(|w| len < w) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "request too slow"));
        }
        stream.set_read_timeout(Some(left))?;
        let n = stream.read(&mut buf[len..])?;
        if n == 0 {
            break;
        }
        len += n;
        if wanted.is_none() {
            if let Some(end) = buf[..len].windows(4).position(|w| w == b"\r\n\r\n") {
                let body = content_length(&String::from_utf8_lossy(&buf[..end])).unwrap_or(0);
                wanted = Some(end + 4 + body.min(buf.len()));
            }
        }
    }
    let Some((status, body)) = respond(logger, http, peer, &buf[..len]) else {
        return Ok(());
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

/// What a request asks for.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    OnlineConfig,
    SignUp,
    Info,
    Login,
    Leaderboard,
    /// A connection that sends nothing, such as the container's health check.
    Nothing,
    Other,
}

fn route(request_line: &str) -> Route {
    let mut parts = request_line.split(' ');
    let (method, path) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    match (method, path.split('?').next().unwrap_or_default()) {
        ("", _) => Route::Nothing,
        ("GET", PATH) => Route::OnlineConfig,
        ("GET", crate::api::INFO_PATH) => Route::Info,
        ("GET", crate::leaderboard::PATH) => Route::Leaderboard,
        ("POST", crate::api::LOGIN_PATH) => Route::Login,
        ("POST", p) if p == crate::signup::PATH => Route::SignUp,
        _ => Route::Other,
    }
}

/// The answer to a whole request (head and body), as a status and a JSON body; `None` for one
/// that asks nothing.
fn respond(logger: &Logger, http: &Http, peer: SocketAddr, request: &[u8]) -> Option<(&'static str, String)> {
    let len = request.len();
    let head_end = request.windows(4).position(|w| w == b"\r\n\r\n");
    let head = String::from_utf8_lossy(&request[..head_end.unwrap_or(len)]);
    let line = head.lines().next().unwrap_or_default();
    let declared = content_length(&head).unwrap_or(0);
    let start = head_end.map_or(len, |e| e + 4);
    let body = &request[start.min(len)..];
    // The body of a POST, if it all came and isn't too big.
    let body = |max: usize| (declared <= max && body.len() >= declared).then(|| &body[..declared]);
    let too_big = || ("413 Content Too Large", "{\"error\":\"request too big\"}".to_owned());
    // Who asked: through a trusted proxy, the client it names.
    let client = crate::proxy::client(peer.ip(), &head, &http.proxies);
    let peer = client.address;
    let route = route(line);
    if matches!(route, Route::Login | Route::SignUp) && !client.may_send_passwords() {
        warn!(logger, "{peer}: password sent through the proxy without HTTPS, refused");
        return Some(crate::signup::error("403 Forbidden", "this server takes passwords over HTTPS only"));
    }
    Some(match route {
        Route::Nothing => return None,
        Route::OnlineConfig => {
            info!(logger, "Online config for {peer}");
            ("200 OK", http.content.clone())
        }
        Route::Info => ("200 OK", http.api.info(logger)),
        Route::Leaderboard => {
            let target = line.split(' ').nth(1).unwrap_or_default();
            http.api.leaderboard(target.split_once('?').map_or("", |(_, query)| query))
        }
        Route::Login => match body(crate::api::MAX_BODY) {
            None => too_big(),
            Some(body) => {
                let ((status, json), name) = http.api.login(peer, body);
                match name {
                    Some(name) => info!(logger, "{peer} signed in to the launcher as {name}"),
                    None => info!(logger, "{peer}: launcher sign-in refused, {status}"),
                }
                (status, json)
            }
        },
        Route::SignUp if http.signup.is_some() => match (body(crate::signup::MAX_BODY), http.signup.as_ref()) {
            (Some(body), Some(signup)) => {
                let ((status, json), made) = signup.handle(peer, body);
                match made {
                    Some(name) => info!(logger, "{peer} signed up as {name}"),
                    None => info!(logger, "{peer}: sign-up refused, {status}"),
                }
                (status, json)
            }
            _ => too_big(),
        },
        Route::SignUp | Route::Other => {
            warn!(logger, "{peer} asked for something else: {line:?}");
            ("404 Not Found", String::new())
        }
    })
}

/// The request's Content-Length, if it gives one.
fn content_length(head: &str) -> Option<usize> {
    head.lines().skip(1).find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_the_game_at_the_auth_server() {
        let json: serde_json::Value = serde_json::from_str(&content("203.0.113.7".parse().unwrap(), 21030)).unwrap();
        let items = json.as_array().unwrap();
        assert_eq!(items.len(), 8);
        assert_eq!(items[0]["Name"], "SandboxUrl");
        assert_eq!(items[0]["Values"][0], "prudp:/address=203.0.113.7;port=21030");
        assert_eq!(items[1]["Values"][0], "203.0.113.7:21030");
    }

    #[test]
    fn routes_requests() {
        assert_eq!(
            route("GET /OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc HTTP/1.1"),
            Route::OnlineConfig
        );
        assert_eq!(route("GET /other HTTP/1.1"), Route::Other);
        assert_eq!(route("GET /api/leaderboard?mode=siege HTTP/1.1"), Route::Leaderboard);
        assert_eq!(route("POST /api/leaderboard HTTP/1.1"), Route::Other);
        assert_eq!(route("POST /OnlineConfigService.svc/GetOnlineConfig HTTP/1.1"), Route::Other);
        assert_eq!(route("POST /accounts HTTP/1.1"), Route::SignUp);
        assert_eq!(route("GET /accounts HTTP/1.1"), Route::Other);
        assert_eq!(route("GET /api/info HTTP/1.1"), Route::Info);
        assert_eq!(route("GET /api/info?x=1 HTTP/1.1"), Route::Info);
        assert_eq!(route("POST /api/info HTTP/1.1"), Route::Other);
        assert_eq!(route("POST /api/login HTTP/1.1"), Route::Login);
        assert_eq!(route("GET /api/login HTTP/1.1"), Route::Other);
        assert_eq!(route(""), Route::Nothing);
        assert_eq!(content_length("POST /accounts HTTP/1.1\r\nHost: x\r\ncontent-length: 42"), Some(42));
        assert_eq!(content_length("GET / HTTP/1.1\r\nHost: x"), None);
    }

    fn server(signup: bool) -> Http {
        let (api, _) = crate::api::tests::api();
        let accounts = Arc::new(crate::accounts::Accounts::open(std::path::Path::new(":memory:")).unwrap());
        Http {
            content: "[]".into(),
            signup: signup.then(|| SignUp::new(accounts, None)),
            api,
            proxies: crate::proxy::Trusted::parse(&[PROXY]).unwrap(),
        }
    }

    /// The proxy the test server trusts.
    const PROXY: &str = "172.31.80.10";

    fn ask(http: &Http, request: &str) -> Option<(&'static str, String)> {
        ask_from(http, "203.0.113.7", request)
    }

    fn ask_from(http: &Http, peer: &str, request: &str) -> Option<(&'static str, String)> {
        let logger = Logger::root(slog::Discard, slog::o!());
        let peer = std::net::SocketAddr::new(peer.parse().unwrap(), 50000);
        respond(&logger, http, peer, request.as_bytes())
    }

    fn post(path: &str, body: &str) -> String {
        format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn answers_the_launcher() {
        let http = server(false);
        let (status, json) = ask(&http, "GET /api/info HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(status, "200 OK");
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["players_online"], 0);
        assert!(json["news"].is_array());

        let (status, json) = ask(&http, &post("/api/login", r#"{"name":"Kozak","password":"hunter2"}"#)).unwrap();
        assert_eq!((status, json.as_str()), ("200 OK", r#"{"pid":1048576}"#));
        assert_eq!(ask(&http, &post("/api/login", r#"{"name":"Kozak","password":"nope"}"#)).unwrap().0, "401 Unauthorized");
        let big = format!(r#"{{"name":"Kozak","password":"{}"}}"#, "x".repeat(2000));
        assert_eq!(ask(&http, &post("/api/login", &big)).unwrap().0, "413 Content Too Large");
        let cut_short = "POST /api/login HTTP/1.1\r\nContent-Length: 50\r\n\r\n{}";
        assert_eq!(ask(&http, cut_short).unwrap().0, "413 Content Too Large");

        assert_eq!(
            ask(&http, &post("/accounts", r#"{"name":"New","password":"x"}"#)).unwrap().0,
            "404 Not Found",
            "sign-up off"
        );
        assert_eq!(ask(&server(true), &post("/accounts", r#"{"name":"New","password":"x"}"#)).unwrap().0, "201 Created");
        assert_eq!(
            ask(&http, "GET /OnlineConfigService.svc/GetOnlineConfig HTTP/1.1\r\n\r\n").unwrap(),
            ("200 OK", "[]".into())
        );
        assert_eq!(ask(&http, ""), None);
    }

    /// A request as the proxy passes it on: from `client`, over `proto`.
    fn proxied(path: &str, body: &str, client: &str, proto: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: {client}\r\nX-Forwarded-Proto: {proto}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn behind_the_proxy_limits_count_each_client() {
        let http = server(false);
        // Guessing at another account, so only the address limit comes into it.
        let wrong = r#"{"name":"Pepper","password":"nope"}"#;
        // Ten failures from one client through the proxy stop that client only.
        for _ in 0..10 {
            let status = ask_from(&http, PROXY, &proxied("/api/login", wrong, "198.51.100.1", "https")).unwrap().0;
            assert_eq!(status, "401 Unauthorized");
        }
        let guesser = ask_from(&http, PROXY, &proxied("/api/login", wrong, "198.51.100.1", "https")).unwrap().0;
        assert_eq!(guesser, "429 Too Many Requests");
        let right = r#"{"name":"Kozak","password":"hunter2"}"#;
        let other = ask_from(&http, PROXY, &proxied("/api/login", right, "198.51.100.2", "https")).unwrap().0;
        assert_eq!(other, "200 OK", "another client through the same proxy isn't held up");

        // A client that isn't the proxy can't name another address to get round its limit.
        let http = server(false);
        for i in 0..10 {
            let spoofed = proxied("/api/login", wrong, &format!("198.51.100.{i}"), "https");
            assert_eq!(ask_from(&http, "203.0.113.9", &spoofed).unwrap().0, "401 Unauthorized");
        }
        let spoofed = proxied("/api/login", right, "198.51.100.99", "https");
        assert_eq!(ask_from(&http, "203.0.113.9", &spoofed).unwrap().0, "429 Too Many Requests");
    }

    #[test]
    fn behind_the_proxy_passwords_need_https() {
        let http = server(true);
        let login = r#"{"name":"Kozak","password":"hunter2"}"#;
        let signup = r#"{"name":"New","password":"x"}"#;
        let via = |path, body, proto| ask_from(&http, PROXY, &proxied(path, body, "198.51.100.1", proto)).unwrap();
        let (status, json) = via("/api/login", login, "http");
        assert_eq!(status, "403 Forbidden");
        assert!(json.contains("HTTPS"), "{json}");
        assert_eq!(via("/accounts", signup, "http").0, "403 Forbidden");
        assert_eq!(via("/api/login", login, "https").0, "200 OK");
        assert_eq!(via("/accounts", signup, "https").0, "201 Created");
        let no_proto = format!("POST /api/login HTTP/1.1\r\nContent-Length: {}\r\n\r\n{login}", login.len());
        assert_eq!(ask_from(&http, PROXY, &no_proto).unwrap().0, "403 Forbidden");
        // What needs no password still goes over plain HTTP: the game's online config, the info.
        let get = |path: &str| ask_from(&http, PROXY, &format!("GET {path} HTTP/1.1\r\nX-Forwarded-Proto: http\r\n\r\n")).unwrap().0;
        assert_eq!(get("/OnlineConfigService.svc/GetOnlineConfig"), "200 OK");
        assert_eq!(get("/api/info"), "200 OK");
        // Straight to the server, as on a LAN without a proxy, as before.
        assert_eq!(ask(&http, &post("/api/login", login)).unwrap().0, "200 OK");
    }
}
