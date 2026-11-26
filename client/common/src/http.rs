//! Talking to a Cross-Com server's API: over HTTPS, as the launcher and the client DLL both do.
//!
//! Plain HTTP only for a server set up by a private address (a LAN, or a VPN such as Tailscale,
//! or this machine), and only when nothing answers on its HTTPS port at all: a server there may
//! well have no HTTPS. A server set up by name, or on the internet, is only spoken to over
//! HTTPS, and a failure after something answered (a certificate that isn't valid, a broken
//! handshake) never leads to plain HTTP: it may be someone in the way.
//!
//! A server hosted from a player's launcher has HTTPS with a certificate of its own, which no
//! certificate authority vouches for: its host's join code names it by its fingerprint, and
//! HTTPS to it takes that certificate and no other ([`pin`]). Never plain HTTP then. Its HTTPS is
//! on its web port, the one the game asks for its online config on (80 unless the server names
//! another: `host:port`), as the server serves both there.
//!
//! The game itself asks for its online config over plain HTTP, on port 80; that one stays as the
//! game does it (`launcher::server::check`).

use std::io::Read;
use std::io::Write;
use std::net::IpAddr;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

/// The longest a connection may take: a server without HTTPS should be found out quickly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// The most of an answer that's read.
const MAX_RESPONSE: u64 = 512 * 1024;
/// The server's HTTP port, unless the server names another (`host:port`).
pub const HTTP_PORT: u16 = 80;

/// How a request went to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// Over HTTPS.
    Encrypted,
    /// Over plain HTTP, to a server at a private address without HTTPS.
    Unencrypted,
}

/// The server's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub sent: Sent,
}

/// The server couldn't be asked: not found, not answering, or not over HTTPS when it must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreachable(pub String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a server's host learns to set up HTTPS.
pub const HTTPS_HELP: &str = "https://github.com/JDevWebb/cross-com-enhanced/blob/main/docs/hosting.md#https-with-caddy";

/// The host and port of `server` (`host`, `host:port`, `[v6]` or `[v6]:port`, perhaps with
/// `http://` or `https://` and a trailing `/`). The port is the plain HTTP one.
#[must_use]
pub fn address(server: &str) -> (String, u16) {
    let server = server.trim();
    let server = server.strip_prefix("http://").or_else(|| server.strip_prefix("https://")).unwrap_or(server);
    let server = server.trim_end_matches('/');
    if let Some(rest) = server.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(HTTP_PORT);
            return (host.to_owned(), port);
        }
    }
    match server.split_once(':') {
        Some((host, port)) if !port.contains(':') => (host.to_owned(), port.parse().unwrap_or(HTTP_PORT)),
        _ => (server.to_owned(), HTTP_PORT),
    }
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

/// Whether `host` may be spoken to over plain HTTP when it has no HTTPS: only when it's given
/// as a private IP address (see [`is_private`]) or `localhost`. A name is never looked up for
/// this: its DNS could say private for the check and send the connection elsewhere.
#[must_use]
pub fn may_go_plain(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let ip = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    ip.parse::<IpAddr>().is_ok_and(is_private)
}

/// Whether an HTTPS attempt failed before reaching anything: refused, unreachable or timed out
/// while connecting. A failure after that may be someone in the way, and never leads to plain
/// HTTP.
#[must_use]
pub fn nothing_answered(error: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    match error {
        ureq::Error::ConnectionFailed | ureq::Error::Timeout(ureq::Timeout::Connect) => true,
        ureq::Error::Io(e) => matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable),
        _ => false,
    }
}

/// Whether `error` is the server's name not being found (no such name, or the DNS not answering
/// for now), rather than anything about the server itself.
#[must_use]
pub fn lookup_failed(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::HostNotFound => true,
        // Windows' WSAHOST_NOT_FOUND, WSATRY_AGAIN, WSANO_RECOVERY and WSANO_DATA.
        ureq::Error::Io(e) => e.raw_os_error().is_some_and(|code| (11001..=11004).contains(&code)) || e.to_string().contains("failed to lookup address"),
        _ => false,
    }
}

/// What to say when `host`'s name wasn't found.
#[must_use]
pub fn lookup_failed_said(host: &str, error: &ureq::Error) -> String {
    format!("Couldn't look up {host} ({error}). Check the address is right, or try again in a moment: your internet connection or its DNS may be down.")
}

/// Why a server whose HTTPS failed with `error` wasn't asked.
fn needs_https(host: &str, error: &ureq::Error) -> String {
    if lookup_failed(error) {
        return lookup_failed_said(host, error);
    }
    if may_go_plain(host) {
        format!("{host}'s HTTPS didn't work ({error}). Whoever runs the server can check it: {HTTPS_HELP}")
    } else {
        format!(
            "{host} didn't answer over HTTPS ({error}). It may be down for now; try again in a moment. If it never \
             answers, it may not have HTTPS, which a server set up by name or on the internet needs; whoever runs it \
             can set it up: {HTTPS_HELP}"
        )
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
        .http_status_as_error(false)
        // A redirect would send the request on somewhere else.
        .max_redirects(0)
        .user_agent("Cross-Com")
        .build()
        .into()
}

/// The URL of `path` on `host`, at `port` when it isn't the scheme's own.
fn url(scheme: &str, host: &str, port: Option<u16>, path: &str) -> String {
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
    match port {
        Some(port) => format!("{scheme}://{host}:{port}{path}"),
        None => format!("{scheme}://{host}{path}"),
    }
}

fn send(url: &str, method: &str, headers: &[(&str, String)], body: &[u8], timeout: Duration) -> Result<(u16, Vec<u8>), ureq::Error> {
    let agent = agent(timeout);
    let mut response = if method == "GET" {
        let mut request = agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, &one_line(value));
        }
        request.call()?
    } else {
        let mut request = agent.post(url);
        for (name, value) in headers {
            request = request.header(*name, &one_line(value));
        }
        request.send(body)?
    };
    let status = response.status().as_u16();
    let body = response.body_mut().with_config().limit(MAX_RESPONSE).read_to_vec().unwrap_or_default();
    Ok((status, body))
}

/// A header value on one line.
fn one_line(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

/// Sends a `GET` or `POST` (with `body`) of `path` to `server`: over HTTPS, or plain HTTP to a
/// server at a private address where nothing answers on HTTPS. Gives up after `timeout`. A
/// server with a [`pin`] is asked over HTTPS with its certificate alone.
pub fn request(server: &str, method: &str, path: &str, headers: &[(&str, String)], body: &[u8], timeout: Duration) -> Result<Response, Unreachable> {
    let (host, port) = address(server);
    if let Some(fingerprint) = pinned(&host) {
        return pinned_request(&host, port, fingerprint, method, path, headers, body, timeout);
    }
    let https = match send(&url("https", &host, None, path), method, headers, body, timeout) {
        Ok((status, body)) => {
            return Ok(Response {
                status,
                body,
                sent: Sent::Encrypted,
            })
        }
        Err(e) => e,
    };
    if !(may_go_plain(&host) && nothing_answered(&https)) {
        return Err(Unreachable(needs_https(&host, &https)));
    }
    let plain_port = (port != HTTP_PORT).then_some(port);
    let (status, body) = send(&url("http", &host, plain_port, path), method, headers, body, timeout).map_err(|e| Unreachable(format!("couldn't reach {host}: {e}")))?;
    Ok(Response {
        status,
        body,
        sent: Sent::Unencrypted,
    })
}

/// A certificate's fingerprint: its SHA-256.
pub type Fingerprint = [u8; 32];

/// The servers whose HTTPS is known by its certificate's fingerprint: host and fingerprint.
static PINS: Mutex<Vec<(String, Fingerprint)>> = Mutex::new(Vec::new());

/// A fingerprint as written (64 hex digits, perhaps in groups split by `:` or spaces).
#[must_use]
pub fn parse_fingerprint(text: &str) -> Option<Fingerprint> {
    let hex: String = text.chars().filter(|c| !matches!(c, ':' | ' ' | '-')).collect();
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// A fingerprint as written: 64 lowercase hex digits.
#[must_use]
pub fn fingerprint_text(fingerprint: &Fingerprint) -> String {
    fingerprint.iter().map(|b| format!("{b:02x}")).collect()
}

/// From now on, HTTPS to `server` takes the certificate whose fingerprint is `fingerprint`
/// alone (a server hosted from a player's launcher). `None` takes its pin away.
pub fn pin(server: &str, fingerprint: Option<Fingerprint>) {
    let host = address(server).0.to_ascii_lowercase();
    let mut pins = PINS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    pins.retain(|(h, _)| *h != host);
    if let Some(fingerprint) = fingerprint {
        pins.push((host, fingerprint));
    }
}

/// Whether `server` is known by its certificate's fingerprint ([`pin`]).
#[must_use]
pub fn is_pinned(server: &str) -> bool {
    pinned(&address(server).0).is_some()
}

fn pinned(host: &str) -> Option<Fingerprint> {
    let pins = PINS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    pins.iter().find(|(h, _)| h.eq_ignore_ascii_case(host)).map(|(_, f)| *f)
}

/// The name asked for in the handshake: the certificate is checked by its fingerprint, not by
/// a name.
const PINNED_NAME: &str = "cross-com.invalid";

/// Takes the certificate whose fingerprint is the pinned one, and checks the handshake's
/// signatures as any certificate's.
#[derive(Debug)]
struct Pinned {
    fingerprint: Fingerprint,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use sha2::Digest;
        if sha2::Sha256::digest(end_entity.as_ref()).as_slice() == self.fingerprint {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("not the certificate the join code names".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// [`request`] to a pinned server: HTTPS on its web port with its certificate alone, one
/// request on its connection.
#[allow(clippy::too_many_arguments)]
fn pinned_request(
    host: &str,
    port: u16,
    fingerprint: Fingerprint,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
    timeout: Duration,
) -> Result<Response, Unreachable> {
    let failed = |e: &dyn std::fmt::Display| {
        Unreachable(format!(
            "{host} didn't answer over HTTPS ({e}). It may be down for now, or its host's join code may have changed."
        ))
    };
    pinned_exchange(host, fingerprint, method, path, headers, body, timeout, port).map_err(|e| failed(&e))
}

#[allow(clippy::too_many_arguments)]
fn pinned_exchange(
    host: &str,
    fingerprint: Fingerprint,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
    timeout: Duration,
    port: u16,
) -> std::io::Result<Response> {
    let address = (host, port).to_socket_addrs()?.next().ok_or_else(|| std::io::Error::other("no address"))?;
    let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT.min(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Pinned {
        fingerprint,
        provider: Arc::clone(&provider),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(PINNED_NAME).map_err(std::io::Error::other)?;
    let connection = rustls::ClientConnection::new(Arc::new(config), name).map_err(std::io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(connection, stream);
    let named = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {named}:{port}\r\nUser-Agent: Cross-Com\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {}\r\n", one_line(value)));
    }
    if method != "GET" {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    tls.write_all(request.as_bytes())?;
    tls.write_all(body)?;
    tls.flush()?;
    // The answer, to the end (or as far as it says it goes).
    let mut answer = Vec::new();
    let limit = MAX_RESPONSE + 16 * 1024;
    match (&mut tls).take(limit).read_to_end(&mut answer) {
        Ok(_) => {}
        // Closed without TLS's goodbye: what came is kept if it's whole (Content-Length says).
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !answer.is_empty() => {}
        Err(e) => return Err(e),
    }
    parse_answer(&answer).ok_or_else(|| std::io::Error::other("its answer made no sense"))
}

/// A whole HTTP answer: its status and body (as far as its `Content-Length` goes).
fn parse_answer(answer: &[u8]) -> Option<Response> {
    let end = answer.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&answer[..end]).ok()?;
    let status: u16 = head.lines().next()?.split(' ').nth(1)?.parse().ok()?;
    let mut body = answer[end + 4..].to_vec();
    let length = head
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .find(|(n, _)| n.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());
    if let Some(length) = length {
        if body.len() < length {
            return None;
        }
        body.truncate(length);
    }
    body.truncate(usize::try_from(MAX_RESPONSE).unwrap_or(usize::MAX));
    Some(Response {
        status,
        body,
        sent: Sent::Encrypted,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::io::Write;

    use super::*;

    #[test]
    fn fingerprints_as_written() {
        let hex = "93812b3fbb13dcb60da0f4fbce81fb959a308a2b25b1b09b3be973da9b9d297e";
        let f = parse_fingerprint(hex).unwrap();
        assert_eq!(fingerprint_text(&f), hex);
        assert_eq!(parse_fingerprint(&hex.to_uppercase()), Some(f));
        let grouped: Vec<String> = hex.as_bytes().chunks(2).map(|c| String::from_utf8(c.to_vec()).unwrap()).collect();
        assert_eq!(parse_fingerprint(&grouped.join(":")), Some(f));
        assert_eq!(parse_fingerprint(&hex[1..]), None);
        assert_eq!(parse_fingerprint(&hex.replace('9', "g")), None);
    }

    #[test]
    fn reads_an_answer() {
        let r = parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokextra").unwrap();
        assert_eq!((r.status, r.body.as_slice(), r.sent), (200, &b"ok"[..], Sent::Encrypted));
        assert!(parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nok").is_none(), "cut short");
        assert!(parse_answer(b"nonsense").is_none());
    }

    /// A server with its own certificate: asked with that certificate alone, and refused with
    /// any other fingerprint.
    #[test]
    fn a_pinned_server_is_asked_with_its_own_certificate_alone() {
        let made = rcgen::generate_simple_self_signed(vec!["anything.example".to_owned()]).unwrap();
        let cert = rustls::pki_types::CertificateDer::from(made.cert.der().to_vec());
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(made.key_pair.serialize_der().into());
        use sha2::Digest;
        let fingerprint: Fingerprint = sha2::Sha256::digest(cert.as_ref()).into();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        let config = Arc::new(config);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let Ok(connection) = rustls::ServerConnection::new(Arc::clone(&config)) else { continue };
                let mut tls = rustls::StreamOwned::new(connection, stream);
                let mut got = Vec::new();
                let mut buf = [0u8; 1024];
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tls.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got.extend_from_slice(&buf[..n]),
                    }
                }
                if got.is_empty() {
                    continue;
                }
                let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n{\"a\"");
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });
        let ask = |f: Fingerprint| pinned_exchange("127.0.0.1", f, "GET", "/api/info", &[], &[], Duration::from_secs(5), port);
        let answer = ask(fingerprint).unwrap();
        assert_eq!((answer.status, answer.body.as_slice()), (200, &b"{\"a\""[..]));
        let mut other = fingerprint;
        other[0] ^= 1;
        let e = ask(other).unwrap_err();
        assert!(e.to_string().contains("not the certificate the join code names"), "{e}");
    }

    #[test]
    fn pins_by_host() {
        let f = [7u8; 32];
        pin("Pinned.Example:80", Some(f));
        assert_eq!(pinned("pinned.example"), Some(f));
        pin("pinned.example", None);
        assert_eq!(pinned("pinned.example"), None);
    }

    #[test]
    fn finds_the_host_and_port() {
        assert_eq!(address("play.example.net"), ("play.example.net".into(), 80));
        assert_eq!(address(" 10.0.0.5:8080 "), ("10.0.0.5".into(), 8080));
        assert_eq!(address("https://play.example.net/"), ("play.example.net".into(), 80));
        assert_eq!(address("[::1]:81"), ("::1".into(), 81));
        assert_eq!(address("[::1]"), ("::1".into(), 80));
        assert_eq!(address("::1"), ("::1".into(), 80));
    }

    #[test]
    fn only_private_addresses_may_go_plain() {
        for host in ["192.168.1.20", "10.0.0.5", "100.100.1.2", "127.0.0.1", "localhost", "[::1]", "fd00::1"] {
            assert!(may_go_plain(host), "{host}");
        }
        for host in ["203.0.113.7", "play.example.net", "8.8.8.8", "192.168.1.20.example.net"] {
            assert!(!may_go_plain(host), "{host}");
        }
    }

    #[test]
    fn a_name_not_found_isnt_said_to_need_https() {
        let windows = ureq::Error::Io(std::io::Error::from_raw_os_error(11002));
        assert!(lookup_failed(&windows));
        assert!(lookup_failed(&ureq::Error::HostNotFound));
        assert!(needs_https("play.example.org", &windows).starts_with("Couldn't look up play.example.org"));
        let refused = ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(!lookup_failed(&refused));
        assert!(needs_https("play.example.org", &refused).contains("may be down"));
    }

    #[test]
    fn urls() {
        assert_eq!(url("https", "play.example.net", None, "/api/info"), "https://play.example.net/api/info");
        assert_eq!(url("http", "127.0.0.1", Some(8080), "/api/info"), "http://127.0.0.1:8080/api/info");
        assert_eq!(url("https", "::1", None, "/x"), "https://[::1]/x");
    }

    /// A server on this machine without HTTPS (nothing on 443 here) is asked over plain HTTP,
    /// with the request's headers on one line each.
    #[test]
    fn a_private_server_without_https_is_asked_over_plain_http() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            while !request.ends_with(b"the log\n") {
                let n = stream.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
            }
            stream.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 10\r\n\r\n{\"id\":\"x\"}").unwrap();
            String::from_utf8(request).unwrap()
        });
        let headers = [("X-Crosscom-Run", "crosscom-1.log\r\nX-Evil: 1".to_owned())];
        let answer = request(&format!("127.0.0.1:{port}"), "POST", "/api/logs", &headers, b"the log\n", Duration::from_secs(5)).unwrap();
        assert_eq!((answer.status, answer.body.as_slice(), answer.sent), (201, &br#"{"id":"x"}"#[..], Sent::Unencrypted));
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("post /api/logs http/1.1\r\n"), "{request}");
        assert!(request.contains("\r\ncontent-length: 8\r\n"), "{request}");
        assert!(request.contains("\r\nx-crosscom-run: crosscom-1.logx-evil: 1\r\n"), "one line: {request}");
    }

    /// A server by a public address or a name is never asked over plain HTTP.
    #[test]
    fn a_public_server_is_only_asked_over_https() {
        // TEST-NET-3, routed nowhere: nothing answers.
        let e = request("203.0.113.7", "GET", "/api/info", &[], &[], Duration::from_millis(300)).unwrap_err();
        assert!(e.0.contains("needs; whoever runs it can set it up"), "{e}");
    }
}
