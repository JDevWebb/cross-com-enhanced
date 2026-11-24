//! The HTTP server: the online config, the first thing the game asks (it answers with where to
//! sign in), the launcher's API (see `api`), account sign-up for the launcher when it's turned
//! on (see `signup`), and players' logs (see `logs`).

use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde::Serialize;
use slog::info;
use slog::warn;
use slog::Logger;

use crate::api::Api;
use crate::rate_limit::address_key;
use crate::signup::SignUp;

const PATH: &str = "/OnlineConfigService.svc/GetOnlineConfig";
/// How long one request may take in all, however slowly it arrives.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// Requests answered at once; more are turned away. The game asks once as it starts, the
/// launcher as it opens its Play screen.
const MAX_CONNECTIONS: usize = 64;
/// Of those, the last few are kept for the game's online config: a connection let in past
/// `MAX_CONNECTIONS - RESERVED` is answered only if it asks for that, so clients holding
/// connections open (sending slowly, or uploading logs) can't stop games starting.
const RESERVED: usize = 8;
/// Connections open at once from one client address (IPv6 by its /64, as `address_key`
/// counts): plenty for a household's launchers and games. Behind a trusted proxy every
/// connection comes from the proxy, so there the cap is on the client it names, once the
/// headers say which; the proxy itself may hold up to `MAX_CONNECTIONS`.
const PER_CLIENT: usize = 4;

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
    /// Players' logs.
    pub logs: crate::logs::Logs,
    /// Clients' reports of their routers, and their connection tests.
    pub reach: crate::reach::Reach,
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
    let gate = Arc::new(Gate::default());
    for stream in listener.incoming().flatten() {
        let Ok(peer) = stream.peer_addr() else {
            continue;
        };
        let Some((pass, reserve)) = gate.connect(peer.ip(), allowed(&http.proxies, peer.ip())) else {
            continue; // dropping the stream closes it
        };
        let logger = logger.clone();
        let http = Arc::clone(&http);
        let conn = Conn { gate: Arc::clone(&gate), reserve };
        std::thread::spawn(move || {
            if let Err(e) = answer(&logger, stream, &http, &conn) {
                warn!(logger, "HTTP request failed: {e}");
            }
            drop(pass);
        });
    }
    Ok(())
}

/// How many connections `peer` may have open at once: a trusted proxy carries everyone's.
fn allowed(proxies: &crate::proxy::Trusted, peer: IpAddr) -> usize {
    if proxies.trusts(peer) {
        MAX_CONNECTIONS
    } else {
        PER_CLIENT
    }
}

/// The connections open, in all and per client address.
#[derive(Default)]
struct Gate {
    open: Mutex<Open>,
}

#[derive(Default)]
struct Open {
    total: usize,
    /// Per client address (as `address_key` counts them): the connections' own, and through a
    /// proxy the clients it names.
    by_address: HashMap<IpAddr, usize>,
}

/// A connection let in by the [`Gate`], or a proxied client's place: given back when dropped.
struct Pass {
    gate: Arc<Gate>,
    address: IpAddr,
    /// Whether it counts in the total (a connection) or only for its address (a proxied client).
    connection: bool,
}

impl Gate {
    fn open(&self) -> std::sync::MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A new connection from `peer`, which may have `allowed` open at once: its pass, and
    /// whether it was let in on the reserve (see [`RESERVED`]); none when it's one too many.
    fn connect(self: &Arc<Self>, peer: IpAddr, allowed: usize) -> Option<(Pass, bool)> {
        let address = address_key(peer);
        let mut open = self.open();
        if open.total >= MAX_CONNECTIONS || open.by_address.get(&address).copied().unwrap_or(0) >= allowed {
            return None;
        }
        let reserve = open.total >= MAX_CONNECTIONS - RESERVED;
        open.total += 1;
        *open.by_address.entry(address).or_default() += 1;
        let pass = Pass {
            gate: Arc::clone(self),
            address,
            connection: true,
        };
        Some((pass, reserve))
    }

    /// One more connection for `client`, named by a trusted proxy (whose own connection is
    /// counted already): none when it has [`PER_CLIENT`] open.
    fn client(self: &Arc<Self>, client: IpAddr) -> Option<Pass> {
        let address = address_key(client);
        let mut open = self.open();
        let count = open.by_address.entry(address).or_default();
        if *count >= PER_CLIENT {
            return None;
        }
        *count += 1;
        Some(Pass {
            gate: Arc::clone(self),
            address,
            connection: false,
        })
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        let mut open = self.gate.open();
        if self.connection {
            open.total = open.total.saturating_sub(1);
        }
        if let Some(count) = open.by_address.get_mut(&self.address) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.by_address.remove(&self.address);
            }
        }
    }
}

/// What [`answer`] knows of its connection.
struct Conn {
    gate: Arc<Gate>,
    /// Let in on the reserve: only the online config is answered.
    reserve: bool,
}

/// Reads what comes into `buf`, waiting until `deadline` at most, and `idle` without a byte.
fn read_by(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant, idle: Duration, what: &str) -> std::io::Result<usize> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{what} too slow")));
    }
    stream.set_read_timeout(Some(left.min(idle)))?;
    stream.read(buf)
}

fn answer(logger: &Logger, mut stream: TcpStream, http: &Http, conn: &Conn) -> std::io::Result<()> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    stream.set_write_timeout(Some(REQUEST_DEADLINE))?;
    let peer = stream.peer_addr()?;
    let mut buf = [0u8; 4096];
    let mut len = 0;
    // The headers first, within one deadline for the whole request.
    let head_end = loop {
        if let Some(end) = buf[..len].windows(4).position(|w| w == b"\r\n\r\n") {
            break Some(end);
        }
        if len == buf.len() {
            break None;
        }
        let n = read_by(&mut stream, &mut buf[len..], deadline, REQUEST_DEADLINE, "request")?;
        if n == 0 {
            break None;
        }
        len += n;
    };
    let head = String::from_utf8_lossy(&buf[..head_end.unwrap_or(len)]).into_owned();
    let route = route(head.lines().next().unwrap_or_default());
    if conn.reserve && !matches!(route, Route::OnlineConfig | Route::Nothing) {
        let (status, body) = crate::signup::error("503 Service Unavailable", "busy; try again in a moment");
        return send(&mut stream, status, &body);
    }
    // Through a trusted proxy, the client it names may only have so many open, as if it had
    // come straight here. The online config is answered at once, whoever asks.
    let client = crate::proxy::client(peer.ip(), &head, &http.proxies);
    let _client_pass = if client.proxied && client.address != peer.ip() && route != Route::OnlineConfig {
        let Some(pass) = conn.gate.client(client.address) else {
            info!(logger, "{}: too many connections at once, refused", client.address);
            let (status, body) = crate::signup::error("429 Too Many Requests", "too many requests at once from here");
            return send(&mut stream, status, &body);
        };
        Some(pass)
    } else {
        None
    };
    if let (Some(end), Route::Logs) = (head_end, &route) {
        // A log is bigger than the buffer, and slower: read on its own terms.
        return answer_logs(logger, stream, http, peer.ip(), &head, &buf[end + 4..len]);
    }
    // Then any body, as far as the buffer goes.
    if let Some(end) = head_end {
        let wanted = end + 4 + content_length(&head).unwrap_or(0).min(buf.len());
        while len < buf.len() && len < wanted {
            let n = read_by(&mut stream, &mut buf[len..], deadline, REQUEST_DEADLINE, "request")?;
            if n == 0 {
                break;
            }
            len += n;
        }
    }
    let Some((status, body)) = respond(logger, http, peer, &buf[..len]) else {
        return Ok(());
    };
    send(&mut stream, status, &body)
}

fn send(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

/// `POST /api/logs`: refused before its body is read when it won't be taken (too big, too many,
/// not from a player's address); else its body is read, within [`crate::logs::DEADLINE`], and
/// kept. `start` is what of the body came with the headers.
fn answer_logs(logger: &Logger, mut stream: TcpStream, http: &Http, peer: IpAddr, head: &str, start: &[u8]) -> std::io::Result<()> {
    stream.set_write_timeout(Some(crate::logs::DEADLINE))?;
    let client = crate::proxy::client(peer, head, &http.proxies);
    if !client.may_send_passwords() {
        warn!(logger, "{}: a log sent through the proxy without HTTPS, refused", client.address);
        let (status, body) = crate::signup::error("403 Forbidden", "this server takes logs over HTTPS only");
        return send(&mut stream, status, &body);
    }
    let client = client.address;
    if let Some((status, body)) = from_a_web_page(&Route::Logs, head) {
        warn!(logger, "{client}: a log sent from a web page, refused");
        send(&mut stream, status, &body)?;
        return stream.shutdown(std::net::Shutdown::Write);
    }
    let slot = match http.logs.check(client, head) {
        Ok(slot) => slot,
        Err((status, body)) => {
            info!(logger, "{client}: logs refused, {status}");
            send(&mut stream, status, &body)?;
            // Without reading what's still coming: closing on it may cut the answer short, so
            // say no more is wanted first.
            return stream.shutdown(std::net::Shutdown::Write);
        }
    };
    // However slowly it comes, it holds its slot only so long (see `logs::deadline`).
    let deadline = Instant::now() + crate::logs::deadline(slot.len);
    let mut body = start[..start.len().min(slot.len)].to_vec();
    body.resize(slot.len, 0);
    let mut len = body.len().min(start.len());
    while len < slot.len {
        let n = read_by(&mut stream, &mut body[len..], deadline, crate::logs::IDLE, "log upload")?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "log upload cut short"));
        }
        len += n;
    }
    let ((status, json), report) = http.logs.store(client, head, &body);
    drop(slot);
    match report {
        Some(r) => info!(logger, "{client}: logs of {} kept as report {} ({} bytes)", r.account, r.id, r.bytes),
        None if status.starts_with("201") => info!(logger, "{client}: logs sent again, already kept"),
        None => warn!(logger, "{client}: logs not kept, {status}: {json}"),
    }
    send(&mut stream, status, &json)
}

/// What a request asks for.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    OnlineConfig,
    SignUp,
    Info,
    Login,
    Leaderboard,
    Logs,
    Forward,
    Reach,
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
        ("POST", crate::logs::PATH) => Route::Logs,
        ("POST", crate::reach::FORWARD_PATH) => Route::Forward,
        ("POST", crate::reach::REACH_PATH) => Route::Reach,
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
    let connected_from = peer.ip().to_canonical();
    let peer = client.address;
    let route = route(line);
    if matches!(route, Route::Login | Route::SignUp) && !client.may_send_passwords() {
        warn!(logger, "{peer}: password sent through the proxy without HTTPS, refused");
        return Some(crate::signup::error("403 Forbidden", "this server takes passwords over HTTPS only"));
    }
    // What players' clients send about them (logs, their routers) goes over HTTPS too, where the
    // server has it: through the proxy, only over HTTPS.
    if matches!(route, Route::Logs | Route::Forward | Route::Reach) && !client.may_send_passwords() {
        warn!(logger, "{peer}: {line:?} through the proxy without HTTPS, refused");
        return Some(crate::signup::error("403 Forbidden", "this server takes this over HTTPS only"));
    }
    if let Some(refused) = from_a_web_page(&route, &head) {
        warn!(logger, "{peer}: {line:?} from a web page or not as JSON, refused ({})", refused.0);
        return Some(refused);
    }
    Some(match route {
        Route::Nothing => return None,
        Route::OnlineConfig => {
            info!(logger, "Online config for {peer}");
            ("200 OK", http.content.clone())
        }
        Route::Info => ("200 OK", http.api.info(logger)),
        // Read as it comes by `answer_logs`; here when the whole request is in hand (the tests).
        Route::Logs => match http.logs.check(peer, &head) {
            Err(answer) => answer,
            Ok(slot) => match body(slot.len) {
                None => too_big(),
                Some(body) => http.logs.store(peer, &head, body).0,
            },
        },
        Route::Leaderboard => {
            let target = line.split(' ').nth(1).unwrap_or_default();
            http.api.leaderboard(target.split_once('?').map_or("", |(_, query)| query))
        }
        Route::Forward => match body(crate::reach::MAX_BODY) {
            None => too_big(),
            Some(body) => {
                let (answer, matched) = http.reach.forward(peer, body);
                if let Some(m) = matched {
                    // As it came, quoted: it's the client's word, newlines and all.
                    let report = String::from_utf8_lossy(body);
                    if m.announced {
                        info!(logger, "{peer}: {:#x}'s client reported its router: {report:?}", m.pid);
                    } else {
                        warn!(
                            logger,
                            "{peer}: {:#x}'s client reported its router for a socket its game didn't announce (another adapter's address, or not its own report); not kept: {report:?}",
                            m.pid
                        );
                    }
                    // A new forward changes the address other players get first: checked afresh.
                    // A report renewing the same forward isn't a reason to look again: it comes
                    // every 10 minutes, also once the game has left Multiplayer and closed its
                    // peer-to-peer socket, where it would be found unreachable (and the player
                    // told so) after every match. The game's own registrations, made only in
                    // Multiplayer, check it again.
                    if m.new_forward {
                        crate::announce::check(logger, m.pid, true);
                    }
                }
                answer
            }
        },
        // Through the proxy without a client address it names, or naming one on its own network
        // (Docker's bridge, passing on a client that came over IPv6): the real client isn't
        // known, and the packet would go to the wrong machine.
        Route::Reach if client.proxied && (peer == connected_from || http.proxies.beside(peer)) => {
            warn!(logger, "{peer}: a connection test through the proxy from an address on its own network, refused");
            crate::signup::error(
                "403 Forbidden",
                "the server can't tell your address (behind its proxy, the client's address isn't passed on); ask its host",
            )
        }
        Route::Reach => match body(crate::reach::MAX_BODY) {
            None => too_big(),
            Some(body) => http.reach.reach(peer, body),
        },
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

/// The value of header `name` in `head` (the request line and headers), if given.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .skip(1)
        .find_map(|l| l.split_once(':').filter(|(n, _)| n.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
}

/// The request's Content-Length, if it gives one.
fn content_length(head: &str) -> Option<usize> {
    header(head, "content-length")?.parse().ok()
}

/// Why a POST is refused when a web page may have sent it, if it may have.
///
/// Any web page a player opens can have their browser send a POST here: a form, or a script's
/// "simple" request (`text/plain`, a form's types), which goes without asking the server first.
/// Such a page could guess at sign-ins in the player's name (and so lock their address out of
/// signing in to the game too, whose limit is shared) or send router reports from their address.
/// The clients (the launcher, the client DLL) send JSON as `application/json` and no `Origin`,
/// which a page can't send without the browser asking first (CORS), and this server never says
/// yes. So: a POST carrying an `Origin` (browsers add it to every POST, `null` from a sandboxed
/// page or a file) or a `Sec-Fetch-Site` other than `same-origin` or `none` is refused (`403`),
/// and one to the JSON routes that isn't `application/json` too (`415`). Logs are `text/plain`,
/// but need `X-Crosscom-Account`, a header a page can't add without asking either.
fn from_a_web_page(route: &Route, head: &str) -> Option<crate::signup::Answer> {
    if !matches!(route, Route::Login | Route::SignUp | Route::Forward | Route::Reach | Route::Logs) {
        return None;
    }
    let site = header(head, "sec-fetch-site").map(str::to_ascii_lowercase);
    if header(head, "origin").is_some() || site.is_some_and(|s| s != "same-origin" && s != "none") {
        return Some(crate::signup::error("403 Forbidden", "not taken from a web page"));
    }
    let json = header(head, "content-type")
        .and_then(|t| t.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"));
    if *route != Route::Logs && !json {
        return Some(crate::signup::error("415 Unsupported Media Type", "send JSON, as Content-Type: application/json"));
    }
    None
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
        assert_eq!(route("POST /api/forward HTTP/1.1"), Route::Forward);
        assert_eq!(route("POST /api/reach HTTP/1.1"), Route::Reach);
        assert_eq!(route("GET /api/reach HTTP/1.1"), Route::Other);
        assert_eq!(route("GET /api/info HTTP/1.1"), Route::Info);
        assert_eq!(route("GET /api/info?x=1 HTTP/1.1"), Route::Info);
        assert_eq!(route("POST /api/info HTTP/1.1"), Route::Other);
        assert_eq!(route("POST /api/login HTTP/1.1"), Route::Login);
        assert_eq!(route("GET /api/login HTTP/1.1"), Route::Other);
        assert_eq!(route("POST /api/logs HTTP/1.1"), Route::Logs);
        assert_eq!(route("GET /api/logs HTTP/1.1"), Route::Other, "never served back");
        assert_eq!(route("GET /api/logs/20261115-123456-ab12cd HTTP/1.1"), Route::Other);
        assert_eq!(route(""), Route::Nothing);
        assert_eq!(content_length("POST /accounts HTTP/1.1\r\nHost: x\r\ncontent-length: 42"), Some(42));
        assert_eq!(content_length("GET / HTTP/1.1\r\nHost: x"), None);
    }

    fn server(signup: bool) -> Http {
        server_in(signup, &std::env::temp_dir().join("crosscom-no-logs"), PLAYER)
    }

    fn server_in(signup: bool, logs: &std::path::Path, player: &str) -> Http {
        let (api, online) = crate::api::tests::api();
        let accounts = Arc::new(crate::accounts::Accounts::open(std::path::Path::new(":memory:")).unwrap());
        let pid = accounts.add("Kozak", "hunter2").unwrap();
        // Played from there earlier today, and gone since.
        online.signed_in(pid, player.parse().unwrap());
        online.gone(pid);
        let settings = crate::logs::Settings {
            accept: true,
            dir: logs.to_owned(),
            max_bytes: 1 << 30,
            keep_days: 30,
        };
        Http {
            content: "[]".into(),
            signup: signup.then(|| SignUp::new(Arc::clone(&accounts), None)),
            api,
            proxies: crate::proxy::Trusted::parse(&[PROXY]).unwrap(),
            logs: crate::logs::Logs::new(settings, Arc::clone(&accounts), online),
            reach: crate::reach::Reach::new(Arc::default(), Arc::clone(&accounts)),
        }
    }

    /// Where the test server's player played from.
    const PLAYER: &str = "198.51.100.1";

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
        format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
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
        assert_eq!((status, json.starts_with(r#"{"pid":1048576,"token":"cct_"#)), ("200 OK", true), "{json}");
        assert_eq!(ask(&http, &post("/api/login", r#"{"name":"Kozak","password":"nope"}"#)).unwrap().0, "401 Unauthorized");
        let big = format!(r#"{{"name":"Kozak","password":"{}"}}"#, "x".repeat(2000));
        assert_eq!(ask(&http, &post("/api/login", &big)).unwrap().0, "413 Content Too Large");
        let cut_short = "POST /api/login HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 50\r\n\r\n{}";
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
            "POST {path} HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: {client}\r\nX-Forwarded-Proto: {proto}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
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

    fn log_upload(account: &str, body: &str, forwarded_for: Option<&str>) -> String {
        log_upload_over(account, body, forwarded_for, "https")
    }

    fn log_upload_over(account: &str, body: &str, forwarded_for: Option<&str>, proto: &str) -> String {
        let forwarded = forwarded_for.map_or(String::new(), |f| format!("X-Forwarded-For: {f}\r\nX-Forwarded-Proto: {proto}\r\n"));
        format!(
            "POST /api/logs HTTP/1.1\r\nHost: x\r\n{forwarded}Content-Type: text/plain; charset=utf-8\r\nX-Crosscom-Account: {account}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn takes_logs_from_where_the_player_played() {
        let dir = tempfile::tempdir().unwrap();
        let http = server_in(false, dir.path(), PLAYER);
        let (status, json) = ask_from(&http, PLAYER, &log_upload("Kozak", "a log\n", None)).unwrap();
        assert_eq!(status, "201 Created", "{json}");
        let id = serde_json::from_str::<serde_json::Value>(&json).unwrap()["id"].as_str().unwrap().to_owned();
        assert_eq!(std::fs::read(crate::logs::find(dir.path(), &id).unwrap()).unwrap(), b"a log\n");
        assert_eq!(ask_from(&http, "203.0.113.9", &log_upload("Kozak", "a log\n", None)).unwrap().0, "403 Forbidden");
        // Behind the trusted proxy, the client it names.
        assert_eq!(ask_from(&http, PROXY, &log_upload("Kozak", "a log\n", Some(PLAYER))).unwrap().0, "201 Created");
        assert_eq!(ask_from(&http, PROXY, &log_upload("Kozak", "a log\n", Some("203.0.113.9"))).unwrap().0, "403 Forbidden");
        // Through the proxy over plain HTTP: refused, as a password would be.
        let (status, json) = ask_from(&http, PROXY, &log_upload_over("Kozak", "a log\n", Some(PLAYER), "http")).unwrap();
        assert_eq!(status, "403 Forbidden");
        assert!(json.contains("HTTPS"), "{json}");
        // Not from the proxy, the header is ignored.
        assert_eq!(ask_from(&http, "203.0.113.9", &log_upload("Kozak", "a log\n", Some(PLAYER))).unwrap().0, "403 Forbidden");
        let cut_short = "POST /api/logs HTTP/1.1\r\nX-Crosscom-Account: Kozak\r\nContent-Length: 50\r\n\r\n{}";
        assert_eq!(ask_from(&http, PLAYER, cut_short).unwrap().0, "413 Content Too Large");
    }

    /// Serves `http` on a port of 127.0.0.1 as `serve` does, a thread a connection; its address.
    fn serving(http: Http) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let http = Arc::new(http);
        let gate = Arc::new(Gate::default());
        std::thread::spawn(move || {
            let logger = Logger::root(slog::Discard, slog::o!());
            for stream in listener.incoming().flatten() {
                let peer = stream.peer_addr().unwrap().ip();
                let Some((pass, reserve)) = gate.connect(peer, allowed(&http.proxies, peer)) else {
                    continue;
                };
                let (logger, http) = (logger.clone(), Arc::clone(&http));
                let conn = Conn { gate: Arc::clone(&gate), reserve };
                std::thread::spawn(move || {
                    let _ = answer(&logger, stream, &http, &conn);
                    drop(pass);
                });
            }
        });
        addr
    }

    /// Sends `request` to `addr` and reads the answer to the end (empty when the connection was
    /// closed unanswered).
    fn talk(addr: SocketAddr, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let _ = stream.write_all(request);
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    }

    #[test]
    fn a_log_upload_over_a_real_connection() {
        let dir = tempfile::tempdir().unwrap();
        // The test's own connections come from 127.0.0.1: the player played from there.
        let addr = serving(server_in(false, dir.path(), "127.0.0.1"));
        let talk = |request: &[u8]| talk(addr, request);
        // Far more than the 4 KiB buffer the other requests are read into.
        let log = "a line of the log\n".repeat(100_000);
        let answer = talk(log_upload("Kozak", &log, None).as_bytes());
        assert!(answer.starts_with("HTTP/1.1 201 Created"), "{answer}");
        let reports = crate::logs::list(dir.path());
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].bytes as usize, log.len());
        // Too big: refused from its headers alone.
        let head = "POST /api/logs HTTP/1.1\r\nX-Crosscom-Account: Kozak\r\nContent-Length: 999999999\r\n\r\n";
        assert!(talk(head.as_bytes()).starts_with("HTTP/1.1 413"));
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
        // Clients' router reports and connection tests too.
        let report = r#"{"local":"192.168.1.20:2346","forward":null,"nat":"consistent"}"#;
        assert_eq!(via("/api/forward", report, "http").0, "403 Forbidden");
        assert_eq!(via("/api/forward", report, "https").0, "200 OK");
        assert_eq!(via("/api/reach", r#"{"port":40000,"nonce":"0011aabb"}"#, "http").0, "403 Forbidden");
        // What needs no password still goes over plain HTTP: the game's online config, the info.
        let get = |path: &str| ask_from(&http, PROXY, &format!("GET {path} HTTP/1.1\r\nX-Forwarded-Proto: http\r\n\r\n")).unwrap().0;
        assert_eq!(get("/OnlineConfigService.svc/GetOnlineConfig"), "200 OK");
        assert_eq!(get("/api/info"), "200 OK");
        // Straight to the server, as on a LAN without a proxy, as before.
        assert_eq!(ask(&http, &post("/api/login", login)).unwrap().0, "200 OK");
    }

    /// `body` POSTed to `path` with the extra `headers` (each ending `\r\n`).
    fn post_with(path: &str, headers: &str, body: &str) -> String {
        format!("POST {path} HTTP/1.1\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn web_pages_cant_post_in_a_players_name() {
        let http = server(true);
        let wrong = r#"{"name":"Kozak","password":"nope"}"#;
        let report = r#"{"local":"192.168.1.20:2346","forward":null,"nat":"consistent"}"#;
        let status = |path: &str, headers: &str, body: &str| ask(&http, &post_with(path, headers, body)).unwrap().0;
        // A page's "simple" requests: a form, or a script's text/plain. None counts as a failed
        // sign-in, so the player's address isn't locked out of the game's sign-in.
        for _ in 0..20 {
            assert_eq!(status("/api/login", "Content-Type: text/plain\r\n", wrong), "415 Unsupported Media Type");
            assert_eq!(
                status("/api/login", "Content-Type: application/x-www-form-urlencoded\r\n", wrong),
                "415 Unsupported Media Type"
            );
            assert_eq!(status("/api/login", "", wrong), "415 Unsupported Media Type", "no type at all");
        }
        assert_eq!(status("/api/login", "Content-Type: application/json\r\n", wrong), "401 Unauthorized", "not locked out");
        for path in ["/api/forward", "/api/reach", "/accounts"] {
            assert_eq!(status(path, "Content-Type: text/plain;charset=UTF-8\r\n", report), "415 Unsupported Media Type", "{path}");
        }
        // JSON from a page, whatever it claims, carries its Origin (or `null`), or the browser's
        // Sec-Fetch-Site.
        let json = "Content-Type: application/json\r\n";
        for from in [
            "Origin: https://evil.example\r\n",
            "Origin: null\r\n",
            "Sec-Fetch-Site: cross-site\r\n",
            "Sec-Fetch-Site: same-site\r\n",
        ] {
            let (status, json) = ask(&http, &post_with("/api/forward", &format!("{json}{from}"), report)).unwrap();
            assert_eq!(status, "403 Forbidden", "{from}");
            assert!(json.contains("web page"), "{json}");
        }
        assert_eq!(
            status("/api/forward", "Sec-Fetch-Site: none\r\nContent-Type: Application/JSON; charset=utf-8\r\n", report),
            "200 OK"
        );
        // Logs are text/plain from the clients: taken as before, but not from a page.
        let log = log_upload("Kozak", "a log\n", None);
        let from_page = log.replacen("Host: x\r\n", "Host: x\r\nOrigin: https://evil.example\r\n", 1);
        assert_eq!(ask_from(&http, PLAYER, &from_page).unwrap().0, "403 Forbidden");
        // GETs are left alone: the online config, the info.
        let get = "GET /api/info HTTP/1.1\r\nOrigin: https://a-fan-site.example\r\n\r\n";
        assert_eq!(ask(&http, get).unwrap().0, "200 OK");
    }

    /// The headers the real clients send, from their own code, are taken.
    #[test]
    fn the_clients_requests_are_taken() {
        let dir = tempfile::tempdir().unwrap();
        let http = server_in(true, dir.path(), PLAYER);
        // client/net (router reports, connection tests) and the launcher (sign-in, sign-up):
        // `Content-Type: application/json`; the logs (client/common's runs) `text/plain`.
        let report = r#"{"local":"192.168.1.20:2346","forward":null,"nat":"consistent"}"#;
        let json = "Content-Type: application/json\r\nUser-Agent: Cross-Com\r\n";
        assert_eq!(ask(&http, &post_with("/api/forward", json, report)).unwrap().0, "200 OK");
        assert_eq!(ask(&http, &post_with("/api/login", json, r#"{"name":"Kozak","password":"hunter2"}"#)).unwrap().0, "200 OK");
        assert_eq!(ask(&http, &post_with("/accounts", json, r#"{"name":"New","password":"x"}"#)).unwrap().0, "201 Created");
        assert_eq!(ask_from(&http, PLAYER, &log_upload("Kozak", "a log\n", None)).unwrap().0, "201 Created");
    }

    #[test]
    fn connection_tests_through_the_proxy_need_the_clients_real_address() {
        let http = server(false);
        let test = r#"{"port":40000,"nonce":"0011aabb"}"#;
        // Docker passing on a client that came over IPv6: Caddy names the network's gateway.
        let (status, json) = ask_from(&http, PROXY, &proxied("/api/reach", test, "172.31.80.1", "https")).unwrap();
        assert_eq!(status, "403 Forbidden");
        assert!(json.contains("can't tell your address"), "{json}");
        // No client named: the proxy's own address.
        let unnamed = format!(
            "POST /api/reach HTTP/1.1\r\nX-Forwarded-Proto: https\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{test}",
            test.len()
        );
        assert_eq!(ask_from(&http, PROXY, &unnamed).unwrap().0, "403 Forbidden");
        // A real client through the proxy: tested (sent to 198.51.100.1, a documentation
        // address, which goes nowhere; or not sent, where this machine has no route out).
        let status = ask_from(&http, PROXY, &proxied("/api/reach", test, "198.51.100.1", "https")).unwrap().0;
        assert!(status == "200 OK" || status.starts_with("500"), "{status}");
    }

    #[test]
    fn connections_are_capped_per_client_and_the_proxy_carries_everyones() {
        let gate = Arc::new(Gate::default());
        let player: IpAddr = "198.51.100.1".parse().unwrap();
        let proxies = crate::proxy::Trusted::parse(&[PROXY]).unwrap();
        let held: Vec<Pass> = (0..PER_CLIENT).map(|_| gate.connect(player, allowed(&proxies, player)).unwrap().0).collect();
        assert!(gate.connect(player, allowed(&proxies, player)).is_none(), "one too many from one address");
        let neighbour: IpAddr = "2001:db8:1:2::9".parse().unwrap();
        let _v6: Vec<(Pass, bool)> = (0..PER_CLIENT)
            .map(|i| gate.connect(format!("2001:db8:1:2::{}", i + 1).parse().unwrap(), PER_CLIENT).unwrap())
            .collect();
        assert!(gate.connect(neighbour, PER_CLIENT).is_none(), "IPv6 by the /64");
        drop(held);
        assert!(gate.connect(player, PER_CLIENT).is_some(), "given back");

        // The proxy may hold many; the clients it names are capped each, once their headers say.
        let gate = Arc::new(Gate::default());
        let proxy: IpAddr = PROXY.parse().unwrap();
        let through: Vec<Pass> = (0..MAX_CONNECTIONS - RESERVED).map(|_| gate.connect(proxy, allowed(&proxies, proxy)).unwrap().0).collect();
        let clients: Vec<Pass> = (0..PER_CLIENT).map(|_| gate.client(player).unwrap()).collect();
        assert!(gate.client(player).is_none());
        assert!(gate.client("198.51.100.2".parse().unwrap()).is_some(), "another client isn't held up");
        // Past the shared part, connections are let in on the reserve, for the online config.
        let (reserve, on_reserve) = gate.connect(proxy, MAX_CONNECTIONS).unwrap();
        assert!(on_reserve);
        drop((through, clients, reserve));
        assert_eq!(gate.open().total, 0);
        assert!(gate.open().by_address.is_empty());
    }

    /// Clients holding connections open slowly don't stop the game getting its online config,
    /// and one client's can't take the server.
    #[test]
    fn slow_clients_dont_keep_the_game_out() {
        let addr = serving(server(false));
        // Four connections from here that send half a request and wait.
        let slow: Vec<TcpStream> = (0..PER_CLIENT)
            .map(|_| {
                let mut s = TcpStream::connect(addr).unwrap();
                s.write_all(b"POST /api/login HTTP/1.1\r\n").unwrap();
                s
            })
            .collect();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            talk(addr, b"GET /OnlineConfigService.svc/GetOnlineConfig HTTP/1.1\r\n\r\n"),
            "",
            "a fifth from here is turned away"
        );
        drop(slow);
        std::thread::sleep(Duration::from_millis(200));
        let answer = talk(addr, b"GET /OnlineConfigService.svc/GetOnlineConfig HTTP/1.1\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    }

    #[test]
    fn on_the_reserve_only_the_online_config_is_answered() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let http = server(false);
        let gate = Arc::new(Gate::default());
        let logger = Logger::root(slog::Discard, slog::o!());
        let conn = Conn { gate, reserve: true };
        let ask = |request: &'static [u8]| {
            let client = std::thread::spawn(move || talk(addr, request));
            let (stream, _) = listener.accept().unwrap();
            answer(&logger, stream, &http, &conn).unwrap();
            client.join().unwrap()
        };
        assert!(ask(b"GET /api/info HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 503"));
        assert!(ask(b"GET /OnlineConfigService.svc/GetOnlineConfig HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 200 OK"));
    }
}
