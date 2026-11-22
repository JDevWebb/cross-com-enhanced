//! Talking to a Cross-Com server's API: over HTTPS, as the launcher and the client DLL both do.
//!
//! Plain HTTP only for a server set up by a private address (a LAN, or a VPN such as Tailscale,
//! or this machine), and only when nothing answers on its HTTPS port at all: a server there may
//! well have no HTTPS. A server set up by name, or on the internet, is only spoken to over
//! HTTPS, and a failure after something answered (a certificate that isn't valid, a broken
//! handshake) never leads to plain HTTP: it may be someone in the way.
//!
//! The game itself asks for its online config over plain HTTP, on port 80; that one stays as the
//! game does it (`launcher::server::check`).

use std::net::IpAddr;
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

/// Why a server whose HTTPS failed with `error` wasn't asked.
fn needs_https(host: &str, error: &ureq::Error) -> String {
    if may_go_plain(host) {
        format!("{host}'s HTTPS didn't work ({error}). Whoever runs the server can check it: {HTTPS_HELP}")
    } else {
        format!(
            "{host} didn't answer over HTTPS ({error}). A server set up by name or on the internet needs HTTPS; \
             whoever runs it can set it up: {HTTPS_HELP}"
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
/// server at a private address where nothing answers on HTTPS. Gives up after `timeout`.
pub fn request(server: &str, method: &str, path: &str, headers: &[(&str, String)], body: &[u8], timeout: Duration) -> Result<Response, Unreachable> {
    let (host, port) = address(server);
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

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::io::Write;

    use super::*;

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
        assert!(e.0.contains("needs HTTPS"), "{e}");
    }
}
