//! The HTTP server: the online config, the first thing the game asks (it answers with where to
//! sign in), and account sign-up for the launcher when it's turned on (see `signup`).

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

use crate::signup::SignUp;

const PATH: &str = "/OnlineConfigService.svc/GetOnlineConfig";
/// How long one request may take in all, however slowly it arrives.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// Requests answered at once; more are turned away. The game asks once as it starts.
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

/// Serves the online config, and sign-up if given, until the process ends.
pub fn serve(logger: &Logger, listen: SocketAddr, content: String, signup: Option<SignUp>) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen)?;
    info!(logger, "Online config on http://{listen}{PATH}");
    if signup.is_some() {
        info!(logger, "Sign-up on http://{listen}{}", crate::signup::PATH);
    }
    let signup = signup.map(Arc::new);
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::AcqRel);
            continue; // dropping the stream closes it
        }
        let logger = logger.clone();
        let content = content.clone();
        let open = Arc::clone(&open);
        let signup = signup.clone();
        std::thread::spawn(move || {
            if let Err(e) = answer(&logger, stream, &content, signup.as_deref()) {
                warn!(logger, "online config request failed: {e}");
            }
            open.fetch_sub(1, Ordering::AcqRel);
        });
    }
    Ok(())
}

fn answer(logger: &Logger, mut stream: TcpStream, content: &str, signup: Option<&SignUp>) -> std::io::Result<()> {
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
    let head_end = buf[..len].windows(4).position(|w| w == b"\r\n\r\n");
    let request = String::from_utf8_lossy(&buf[..head_end.unwrap_or(len)]);
    let line = request.lines().next().unwrap_or_default();
    let declared = content_length(&request).unwrap_or(0);
    let answered: String;
    let (status, body) = if is_online_config_request(line) {
        info!(logger, "Online config for {peer}");
        ("200 OK", content)
    } else if let (Some(signup), true) = (signup, is_signup_request(line)) {
        let start = head_end.map_or(len, |e| e + 4);
        let body = &buf[start.min(len)..len];
        if declared > crate::signup::MAX_BODY || body.len() < declared {
            ("413 Content Too Large", "{\"error\":\"request too big\"}")
        } else {
            let ((status, json), made) = signup.handle(peer.ip(), &body[..declared]);
            match made {
                Some(name) => info!(logger, "{peer} signed up as {name}"),
                None => info!(logger, "{peer}: sign-up refused, {status}"),
            }
            answered = json;
            (status, answered.as_str())
        }
    } else if line.is_empty() {
        // A connection that sends nothing, such as the container's health check.
        return Ok(());
    } else {
        warn!(logger, "{peer} asked for something else: {line:?}");
        ("404 Not Found", "")
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

/// The request's Content-Length, if it gives one.
fn content_length(head: &str) -> Option<usize> {
    head.lines().skip(1).find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
    })
}

fn is_signup_request(request_line: &str) -> bool {
    let mut parts = request_line.split(' ');
    parts.next() == Some("POST") && parts.next() == Some(crate::signup::PATH)
}

fn is_online_config_request(request_line: &str) -> bool {
    let mut parts = request_line.split(' ');
    parts.next() == Some("GET") && parts.next().is_some_and(|target| target.split('?').next() == Some(PATH))
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
    fn recognises_the_request() {
        assert!(is_online_config_request(
            "GET /OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc HTTP/1.1"
        ));
        assert!(!is_online_config_request("GET /other HTTP/1.1"));
        assert!(!is_online_config_request("POST /OnlineConfigService.svc/GetOnlineConfig HTTP/1.1"));
        assert!(is_signup_request("POST /accounts HTTP/1.1"));
        assert!(!is_signup_request("GET /accounts HTTP/1.1"));
        assert_eq!(content_length("POST /accounts HTTP/1.1\r\nHost: x\r\ncontent-length: 42"), Some(42));
        assert_eq!(content_length("GET / HTTP/1.1\r\nHost: x"), None);
    }
}
