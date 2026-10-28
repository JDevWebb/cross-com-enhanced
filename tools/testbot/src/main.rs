//! Signs in to a Cross-Com server the way Future Soldier does, and checks each step:
//! the online config, LoginEx and a ticket on the auth server, the secure connection with that
//! ticket, RegisterEx, and the privileges list. A wrong password must be refused.
//!
//! ```text
//! crosscom-testbot SERVER_IP NAME PASSWORD [HTTP_PORT]
//! ```

mod conn;

use std::net::IpAddr;
use std::net::SocketAddr;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use grfs_protocols::authentication_foundation::ticket_granting_protocol as tg;
use grfs_protocols::privileges_service::privileges_protocol as privileges;
use grfs_protocols::secure_connection_service::secure_connection_protocol as secure;
use grfs_protocols::ubi_authentication::types::UbiAuthenticationLoginCustomData;
use md5::Digest;
use md5::Md5;
use quazal::prudp::packet::crypt_key;
use quazal::rmc::basic::FromStream;
use quazal::rmc::basic::ReadStream;
use quazal::rmc::basic::ToStream;
use quazal::rmc::types::Any;
use quazal::rmc::types::StationURL;

use crate::conn::Conn;

/// The secure server's principal ID, which tickets are asked for.
const SERVER_PID: u32 = 0x1000;

fn decode<T: FromStream>(data: &[u8]) -> Result<T> {
    ReadStream::from_bytes(data).read().map_err(|e| eyre!("decoding answer: {e:?}"))
}

/// The key a ticket is encrypted with for principal `pid` (Quazal's Kerberos: MD5 of the
/// password, repeated 65000 + pid % 1024 times).
fn ticket_key(pid: u32, password: &str) -> Vec<u8> {
    let mut key = password.as_bytes().to_vec();
    for _ in 0..(65000 + pid % 1024) {
        key = Md5::digest(&key).to_vec();
    }
    key
}

/// The online config, over plain HTTP, as the game asks for it.
async fn online_config(server: IpAddr, port: u16) -> Result<String> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::net::TcpStream::connect((server, port)).await?;
    let request = "GET /OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc HTTP/1.1\r\nHost: onlineconfigservice.ubi.com\r\n\r\n";
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    let Some((head, body)) = response.split_once("\r\n\r\n") else {
        bail!("no HTTP response");
    };
    if !head.starts_with("HTTP/1.1 200") {
        bail!("online config: {}", head.lines().next().unwrap_or_default());
    }
    Ok(body.to_owned())
}

/// The auth server: LoginEx, then a ticket for the secure server.
async fn sign_in(auth: SocketAddr, name: &str, password: &str) -> Result<(u32, SocketAddr, Vec<u8>)> {
    let (mut conn, _) = Conn::connect(auth, vec![]).await?;
    let login = tg::LoginExRequest {
        str_user_name: name.into(),
        o_extra_data: Any::new(
            "UbiAuthenticationLoginCustomData".into(),
            UbiAuthenticationLoginCustomData {
                data: quazal::rmc::types::Data,
                user_name: name.into(),
                online_key: "AAAA-BBBB-CCCC-DDDD".into(),
                password: password.into(),
            }
            .to_bytes(),
        ),
    };
    let resp: tg::LoginExResponse = decode(
        &conn
            .call(tg::TICKET_GRANTING_PROTOCOL_ID, tg::TicketGrantingProtocolMethod::LoginEx as u32, login.to_bytes())
            .await?,
    )?;
    let pid = resp.pid_principal;
    let url = &resp.p_connection_data.url_regular_protocols;
    let secure = SocketAddr::new(url.address.parse()?, url.port);
    let ticket: tg::RequestTicketResponse = decode(
        &conn
            .call(
                tg::TICKET_GRANTING_PROTOCOL_ID,
                tg::TicketGrantingProtocolMethod::RequestTicket as u32,
                tg::RequestTicketRequest {
                    id_source: pid,
                    id_target: SERVER_PID,
                }
                .to_bytes(),
            )
            .await?,
    )?;
    conn.disconnect().await?;
    Ok((pid, secure, ticket.buf_response))
}

/// The secure server: the sealed ticket and an encrypted challenge, which it must answer.
async fn connect_secure(addr: SocketAddr, pid: u32, ticket: &[u8]) -> Result<Conn> {
    if ticket.len() < 16 + 4 + 16 {
        bail!("ticket too short");
    }
    // RC4 under the player's key, then an HMAC this doesn't check.
    let plain = crypt_key(&ticket_key(pid, "UbiDummyPwd"), &ticket[..ticket.len() - 16]);
    let session_key = plain[..16].to_vec();
    let sealed: Vec<u8> = decode(&plain[20..])?;
    let challenge: u32 = rand::random();
    let mut connect_data = pid.to_bytes();
    connect_data.extend(0u32.to_bytes());
    connect_data.extend(challenge.to_bytes());
    let mut payload = sealed.to_bytes();
    payload.extend(crypt_key(&session_key, &connect_data).to_bytes());
    let (conn, answer) = Conn::connect(addr, payload).await?;
    let answer: Vec<u8> = decode(&answer).map_err(|_| eyre!("the secure server rejected the ticket"))?;
    if decode::<u32>(&answer)? != challenge.wrapping_add(1) {
        bail!("the secure server answered the challenge wrongly");
    }
    Ok(conn)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [server, name, password, rest @ ..] = args.as_slice() else {
        bail!("usage: crosscom-testbot SERVER_IP NAME PASSWORD [HTTP_PORT]");
    };
    let server: IpAddr = server.parse()?;
    let http_port = rest.first().map(|p| p.parse()).transpose()?.unwrap_or(80);

    let config = online_config(server, http_port).await?;
    let auth_port = config
        .split("prudp:/address=")
        .nth(1)
        .and_then(|s| s.split("port=").nth(1))
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|p| p.parse::<u16>().ok())
        .ok_or_else(|| eyre!("no SandboxUrl in the online config"))?;
    println!("ok   online config: sign in on port {auth_port}");
    let auth = SocketAddr::new(server, auth_port);

    if sign_in(auth, name, "not-the-password").await.is_ok() {
        bail!("a wrong password was accepted");
    }
    println!("ok   a wrong password is refused");

    let (pid, secure_addr, ticket) = sign_in(auth, name, password).await?;
    println!("ok   signed in as {name} (PID {pid:#x}); secure server {secure_addr}");

    let mut conn = connect_secure(secure_addr, pid, &ticket).await?;
    println!("ok   secure server accepted the ticket");

    let urls = vec!["prudp:/address=192.168.1.20;port=3074;sid=15;type=3".parse::<StationURL>().map_err(|e| eyre!("{e:?}"))?];
    let registered: secure::RegisterExResponse = decode(
        &conn
            .call(
                secure::SECURE_CONNECTION_PROTOCOL_ID,
                secure::SecureConnectionProtocolMethod::RegisterEx as u32,
                secure::RegisterExRequest {
                    vec_my_urls: urls,
                    h_custom_data: Any::new("Data".into(), vec![]),
                }
                .to_bytes(),
            )
            .await?,
    )?;
    println!("ok   registered: connection {:#x}, public URL {}", registered.pid_connection_id, registered.url_public);

    let privileges: privileges::GetPrivilegesExResponse = decode(
        &conn
            .call(
                privileges::PRIVILEGES_PROTOCOL_ID,
                privileges::PrivilegesProtocolMethod::GetPrivilegesEx as u32,
                privileges::GetPrivilegesExRequest { locale_code: "en-US".into() }.to_bytes(),
            )
            .await?,
    )?;
    if !privileges.privileges_ex.0.iter().any(|p| p.id == 1) {
        bail!("no 'play online' privilege");
    }
    println!("ok   {} privileges, including playing online", privileges.privileges_ex.0.len());

    conn.disconnect().await?;
    println!("all good");
    Ok(())
}
