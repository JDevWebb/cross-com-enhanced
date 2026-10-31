//! Talking to a Cross-Com server over HTTP, as the game does first: checking it answers with
//! its online config, and making an account when the server allows sign-up.

use std::time::Duration;

use serde_json::json;

const ONLINE_CONFIG: &str = "/OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc";
const TIMEOUT: Duration = Duration::from_secs(8);

/// The server as typed: a host name or address, perhaps with `http://` or a trailing `/`.
/// Port 80 is the game's, so none other is taken.
#[must_use]
pub fn host(typed: &str) -> String {
    let t = typed.trim();
    let t = t.strip_prefix("http://").unwrap_or(t);
    t.trim_end_matches('/').to_owned()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).http_status_as_error(false).build().into()
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

/// Makes an account; its ID, or the server's reason for refusing.
pub fn sign_up(server: &str, name: &str, password: &str, code: &str) -> Result<u32, String> {
    let url = format!("http://{}/accounts", host(server));
    let code = Some(code.trim()).filter(|c| !c.is_empty());
    let body = json!({ "name": name.trim(), "password": password, "code": code }).to_string();
    let mut response = agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|e| format!("couldn't reach {}: {e}", host(server)))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    answer(status, &text)
}

fn answer(status: u16, body: &str) -> Result<u32, String> {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    match status {
        201 => json["pid"]
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
    }
}
