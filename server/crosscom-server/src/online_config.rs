//! The online config service: the first thing the game asks, over HTTP. It answers with where
//! to sign in.

use std::io::Read;
use std::io::Write;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::time::Duration;

use serde::Serialize;
use slog::info;
use slog::warn;
use slog::Logger;

const PATH: &str = "/OnlineConfigService.svc/GetOnlineConfig";

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

/// Serves the online config until the process ends.
pub fn serve(logger: &Logger, listen: SocketAddr, content: String) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen)?;
    info!(logger, "Online config on http://{listen}{PATH}");
    for stream in listener.incoming().flatten() {
        let logger = logger.clone();
        let content = content.clone();
        std::thread::spawn(move || {
            if let Err(e) = answer(&logger, stream, &content) {
                warn!(logger, "online config request failed: {e}");
            }
        });
    }
    Ok(())
}

fn answer(logger: &Logger, mut stream: TcpStream, content: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let peer = stream.peer_addr()?;
    let mut buf = [0u8; 4096];
    let mut len = 0;
    // Only the request line matters; read until the headers end or the buffer is full.
    while len < buf.len() {
        let n = stream.read(&mut buf[len..])?;
        if n == 0 {
            break;
        }
        len += n;
        if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..len]);
    let line = request.lines().next().unwrap_or_default();
    let (status, body) = if is_online_config_request(line) {
        info!(logger, "Online config for {peer}");
        ("200 OK", content)
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
    }
}
