//! HTTPS of the server's own (`https_port`), for a server with no proxy giving it HTTPS: one
//! hosted from the launcher, at home. On a port of its own, or on the HTTP port itself
//! (`https_port` = `http_port`): there, a connection starting with TLS's handshake (its first
//! byte, 0x16, never starts an HTTP request) is HTTPS, any other plain HTTP. One port is all a
//! router then has to forward, whatever number it gives it.
//!
//! Its certificate is its own, made the first time and kept beside the database: no domain or
//! certificate authority needed. Players' launchers know it by its fingerprint (the SHA-256 of
//! the certificate), which the host's join code carries, so they talk to this server and no one
//! else (`crosscom_client_common::http`).
//!
//! It works as a proxy in front of the HTTP side would: each request, once its TLS is done, is
//! passed on to the HTTP side on this machine with `X-Forwarded-For` (the client's address) and
//! `X-Forwarded-Proto: https`, from [`FRONT`], an address of this machine that the server trusts
//! as a proxy (`Config::trusted_proxies`). The HTTP side then counts each client and takes
//! sign-ins as it does through Caddy. Requests from this machine's usual address (127.0.0.1, the
//! host's own launcher) aren't proxied ones: their headers aren't believed.

use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use rustls::pki_types::CertificateDer;
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::PrivatePkcs8KeyDer;
use sha2::Digest;
use slog::info;
use slog::warn;
use slog::Logger;

/// The address the HTTPS side passes requests on from: trusted as a proxy, and used by nothing
/// else.
pub const FRONT: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
/// How long a client may take for its handshake or between bytes, and for the answer to go.
const IDLE: Duration = Duration::from_secs(30);
/// The most of a request's headers read, and of its body passed on (a player's logs: 4 MiB).
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: u64 = 5 * 1024 * 1024;
/// Connections being served at once, in all and per client address.
const MAX_CONNECTIONS: usize = 256;
const PER_CLIENT: usize = 16;

/// The server's certificate: made with its key the first time, then read from `certificate`
/// and `key`.
pub fn certificate(certificate: &Path, key: &Path) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), String> {
    if let (Ok(cert), Ok(key)) = (std::fs::read(certificate), std::fs::read(key)) {
        return Ok((CertificateDer::from(cert), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key))));
    }
    let made = rcgen::generate_simple_self_signed(vec!["cross-com.invalid".to_owned()]).map_err(|e| format!("couldn't make a certificate: {e}"))?;
    let (cert, key_der) = (made.cert.der().to_vec(), made.key_pair.serialize_der());
    write_private(key, &key_der).map_err(|e| format!("couldn't keep the certificate's key in {}: {e}", key.display()))?;
    std::fs::write(certificate, &cert).map_err(|e| format!("couldn't keep the certificate in {}: {e}", certificate.display()))?;
    Ok((CertificateDer::from(cert), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der))))
}

/// Writes `data` to `path`, readable by this user alone where the system has such a thing.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data)
}

/// The certificate's fingerprint, as join codes carry it: its SHA-256, in lowercase hex.
#[must_use]
pub fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    sha2::Sha256::digest(certificate.as_ref()).iter().map(|b| format!("{b:02x}")).collect()
}

/// TLS's first byte: a handshake record.
pub const HANDSHAKE: u8 = 0x16;

/// The server's HTTPS: its certificate, and the HTTP side requests go on to.
pub struct Front {
    config: Arc<rustls::ServerConfig>,
    http: SocketAddr,
    pub fingerprint: String,
}

impl Front {
    /// HTTPS with `certificate`, passing requests on to the HTTP side at `http`.
    pub fn new(http: SocketAddr, certificate: (CertificateDer<'static>, PrivateKeyDer<'static>)) -> Result<Self, String> {
        let fingerprint = fingerprint(&certificate.0);
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(vec![certificate.0], certificate.1)
            .map_err(|e| format!("the certificate won't do: {e}"))?;
        Ok(Self {
            config: Arc::new(config),
            http,
            fingerprint,
        })
    }

    /// Serves one connection from `peer`: its TLS, its request passed on, its answer back.
    pub fn relay(&self, stream: TcpStream, peer: SocketAddr) -> std::io::Result<()> {
        relay(stream, peer, self.http, Arc::clone(&self.config))
    }
}

/// Serves HTTPS on a port of its own (`listen`).
pub fn serve(logger: &Logger, listen: SocketAddr, front: Arc<Front>) -> Result<(), String> {
    let listener = TcpListener::bind(listen).map_err(|e| format!("couldn't listen on {listen}: {e}"))?;
    info!(logger, "HTTPS on {listen}, certificate {}", front.fingerprint);
    let gate = Arc::new(Gate::default());
    for stream in listener.incoming().flatten() {
        let Ok(peer) = stream.peer_addr() else { continue };
        let Some(pass) = gate.enter(peer.ip()) else { continue };
        let (logger, front) = (logger.clone(), Arc::clone(&front));
        std::thread::spawn(move || {
            if let Err(e) = front.relay(stream, peer) {
                slog::debug!(logger, "HTTPS request from {peer} failed: {e}");
            }
            drop(pass);
        });
    }
    warn!(logger, "HTTPS stopped listening");
    Ok(())
}

/// Connections being served: in all, and per address.
#[derive(Default)]
struct Gate(Mutex<(usize, HashMap<IpAddr, usize>)>);

/// A place in the [`Gate`], given back when dropped.
struct Pass(Arc<Gate>, IpAddr);

impl Gate {
    fn enter(self: &Arc<Self>, address: IpAddr) -> Option<Pass> {
        let address = address.to_canonical();
        let mut open = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let here = open.1.get(&address).copied().unwrap_or(0);
        if open.0 >= MAX_CONNECTIONS || here >= PER_CLIENT {
            return None;
        }
        open.0 += 1;
        open.1.insert(address, here + 1);
        Some(Pass(Arc::clone(self), address))
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        let mut open = self.0 .0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        open.0 = open.0.saturating_sub(1);
        if let Some(count) = open.1.get_mut(&self.1) {
            *count -= 1;
            if *count == 0 {
                open.1.remove(&self.1);
            }
        }
    }
}

/// One request from `peer`: its TLS, then passed on to the HTTP side and its answer back.
fn relay(stream: TcpStream, peer: SocketAddr, http: SocketAddr, config: Arc<rustls::ServerConfig>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_write_timeout(Some(IDLE))?;
    let connection = rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(connection, stream);
    // The request's headers.
    let mut request = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        if request.len() > MAX_HEAD {
            return answer(&mut tls, "431 Request Header Fields Too Large");
        }
        let n = tls.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
    if header(&head, "transfer-encoding").is_some() {
        return answer(&mut tls, "411 Length Required");
    }
    let length: u64 = match header(&head, "content-length").map(|v| v.parse()) {
        None => 0,
        Some(Ok(n)) if n <= MAX_BODY => n,
        Some(Ok(_)) => return answer(&mut tls, "413 Content Too Large"),
        Some(Err(_)) => return answer(&mut tls, "400 Bad Request"),
    };
    // On to the HTTP side, from the address it trusts as this proxy.
    let mut backend = connect_from(FRONT, http)?;
    backend.set_read_timeout(Some(IDLE))?;
    backend.set_write_timeout(Some(IDLE))?;
    backend.write_all(forwarded(&head, peer.ip()).as_bytes())?;
    let came = &request[head_end + 4..];
    let came = &came[..came.len().min(usize::try_from(length).unwrap_or(usize::MAX))];
    backend.write_all(came)?;
    let rest = length - came.len() as u64;
    std::io::copy(&mut (&mut tls).take(rest), &mut backend)?;
    // Its answer, back to the client (the HTTP side closes when it has answered).
    std::io::copy(&mut backend, &mut tls)?;
    tls.conn.send_close_notify();
    tls.flush()
}

/// An answer with no body.
fn answer(tls: &mut impl Write, status: &str) -> std::io::Result<()> {
    write!(tls, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
    tls.flush()
}

/// The first value of header `name` (lowercase) in `head`.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .find(|(n, _)| n.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

/// `head` as passed on: the client's own forwarding headers taken out, the client's address and
/// HTTPS put in, and the connection closed after the answer.
fn forwarded(head: &str, client: IpAddr) -> String {
    let mut lines = head.lines();
    let mut out = String::from(lines.next().unwrap_or_default());
    out.push_str("\r\n");
    for line in lines {
        let name = line.split_once(':').map_or("", |(n, _)| n.trim());
        let ours = ["x-forwarded-for", "x-forwarded-proto", "connection"].iter().any(|h| name.eq_ignore_ascii_case(h));
        if !ours {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out.push_str(&format!(
        "X-Forwarded-For: {}\r\nX-Forwarded-Proto: https\r\nConnection: close\r\n\r\n",
        client.to_canonical()
    ));
    out
}

/// A connection to `to` from `from` (this machine's address the HTTP side trusts).
fn connect_from(from: Ipv4Addr, to: SocketAddr) -> std::io::Result<TcpStream> {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    socket.bind(&SocketAddr::from((from, 0)).into())?;
    socket.connect_timeout(&to.into(), IDLE)?;
    Ok(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clients_own_forwarding_headers_are_replaced() {
        let head = "POST /api/key-login HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 1.2.3.4\r\nx-forwarded-proto: http\r\nConnection: keep-alive\r\nContent-Length: 2";
        let out = forwarded(head, "198.51.100.20".parse().unwrap());
        assert!(out.starts_with("POST /api/key-login HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n"), "{out}");
        assert!(
            out.ends_with("X-Forwarded-For: 198.51.100.20\r\nX-Forwarded-Proto: https\r\nConnection: close\r\n\r\n"),
            "{out}"
        );
        assert!(!out.contains("1.2.3.4") && !out.contains("keep-alive"));
        assert_eq!(header(head, "content-length"), Some("2"));
    }

    #[test]
    fn the_certificate_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = (dir.path().join("c.der"), dir.path().join("k.der"));
        let (first, _) = certificate(&cert, &key).unwrap();
        let (again, _) = certificate(&cert, &key).unwrap();
        assert_eq!(fingerprint(&first), fingerprint(&again));
        assert_eq!(fingerprint(&first).len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600, "the key is private");
        }
    }

    /// A request over HTTPS reaches the HTTP side from [`FRONT`] with the client's address, and
    /// its answer comes back.
    #[test]
    fn passes_requests_on_and_answers_back() {
        let backend = TcpListener::bind("127.0.0.1:0").unwrap();
        let http = backend.local_addr().unwrap();
        let seen = std::thread::spawn(move || {
            let (mut stream, from) = backend.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 1024];
            while !got.ends_with(b"hi") {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                got.extend_from_slice(&buf[..n]);
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
            (from.ip(), String::from_utf8(got).unwrap())
        });
        let dir = tempfile::tempdir().unwrap();
        let identity = certificate(&dir.path().join("c.der"), &dir.path().join("k.der")).unwrap();
        let pinned = identity.0.clone();
        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let listen = front.local_addr().unwrap();
        drop(front);
        let logger = Logger::root(slog::Discard, slog::o!());
        let front = Arc::new(Front::new(http, identity).unwrap());
        std::thread::spawn(move || serve(&logger, listen, front));
        std::thread::sleep(Duration::from_millis(200));

        // A client that trusts this certificate alone.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(pinned).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from("cross-com.invalid").unwrap();
        let connection = rustls::ClientConnection::new(Arc::new(client), name).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, TcpStream::connect(listen).unwrap());
        tls.write_all(b"POST /api/x HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 6.6.6.6\r\nContent-Length: 2\r\n\r\nhi")
            .unwrap();
        let mut answer = String::new();
        let _ = tls.read_to_string(&mut answer);
        assert!(answer.starts_with("HTTP/1.1 200 OK") && answer.ends_with("ok"), "{answer}");
        let (from, request) = seen.join().unwrap();
        assert_eq!(from, IpAddr::V4(FRONT), "from the address trusted as this proxy");
        assert!(request.contains("X-Forwarded-For: 127.0.0.1\r\nX-Forwarded-Proto: https\r\n"), "{request}");
        assert!(!request.contains("6.6.6.6"));
    }
}
