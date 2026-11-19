//! Signs in to a Cross-Com server the way Future Soldier does, and checks each step:
//! the online config, LoginEx and a ticket on the auth server, the secure connection with that
//! ticket, RegisterEx, and the privileges list. A wrong password must be refused.
//!
//! ```text
//! crosscom-testbot SERVER_IP NAME PASSWORD [HTTP_PORT] [--signup [CODE]] [--host SECONDS | --join | --friend NAME SECONDS | --answer SECONDS]
//! ```
//!
//! With `--signup`, it first makes the account over HTTP, as the launcher does (with the server's
//! sign-up code, if it has one).
//!
//! With `--host`, it then hosts a quick match as the game does and waits that long for a player
//! to come in: someone's probe request, passed on by the server. With `--join`, it searches for
//! a quick match as the game does, joins the first one found, asks for its host's addresses and
//! for the host to probe it, and leaves. Two bots, or a bot and the game, test matchmaking
//! with a single PC.
//!
//! With `--friend NAME SECONDS` it asks NAME to be friends, waits for the answer, then invites
//! NAME into a party; with `--answer SECONDS` it waits for a friend request, accepts it, waits
//! for the invitation and joins the party.

// Shared with other tests; not every helper is used here.
#[allow(dead_code)]
mod conn;

use std::net::IpAddr;
use std::net::SocketAddr;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use grfs_protocols::authentication_foundation::ticket_granting_protocol as tg;
use grfs_protocols::friends_service::friends_protocol as friends;
use grfs_protocols::game_session_service::game_session_protocol as gs;
use grfs_protocols::game_session_service::types::GameSession;
use grfs_protocols::game_session_service::types::GameSessionInvitation;
use grfs_protocols::game_session_service::types::GameSessionKey;
use grfs_protocols::game_session_service::types::GameSessionQuery;
use grfs_protocols::nat_traversal::nat_traversal_protocol as nat;
use grfs_protocols::privileges_service::privileges_protocol as privileges;
use grfs_protocols::protocol_foundation::types::NotificationEvent;
use grfs_protocols::secure_connection_service::secure_connection_protocol as secure;
use grfs_protocols::ubi_authentication::types::UbiAuthenticationLoginCustomData;
use md5::Digest;
use md5::Md5;
use quazal::prudp::packet::crypt_key;
use quazal::rmc::basic::FromStream;
use quazal::rmc::basic::ReadStream;
use quazal::rmc::basic::ToStream;
use quazal::rmc::types::Any;
use quazal::rmc::types::Property;
use quazal::rmc::types::QList;
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
    let request =
        "GET /OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc HTTP/1.1\r\nHost: onlineconfigservice.ubi.com\r\nConnection: close\r\n\r\n";
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

/// Makes an account over HTTP, as the launcher does: the status line and the body.
async fn sign_up(server: IpAddr, port: u16, name: &str, password: &str, code: Option<&str>) -> Result<(String, String)> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    let body = serde_json::json!({ "name": name, "password": password, "code": code }).to_string();
    let mut stream = tokio::net::TcpStream::connect((server, port)).await?;
    let request = format!(
        "POST /accounts HTTP/1.1\r\nHost: {server}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or_else(|| eyre!("no HTTP response"))?;
    Ok((head.lines().next().unwrap_or_default().to_owned(), body.to_owned()))
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
        bail!("usage: crosscom-testbot SERVER_IP NAME PASSWORD [HTTP_PORT] [--signup [CODE]] [--host SECONDS | --join | --friend NAME SECONDS | --answer SECONDS]");
    };
    let server: IpAddr = server.parse()?;
    let mut rest = rest.iter().map(String::as_str).peekable();
    let http_port = match rest.peek() {
        Some(p) if !p.starts_with("--") => rest.next().unwrap_or_default().parse()?,
        _ => 80,
    };
    let mut signup = None;
    if rest.peek() == Some(&"--signup") {
        rest.next();
        signup = Some(match rest.peek() {
            Some(code) if !code.starts_with("--") => rest.next().map(str::to_owned),
            _ => None,
        });
    }
    let mode = match (rest.next(), rest.next()) {
        (None, _) => Mode::Check,
        (Some("--host"), Some(secs)) => Mode::Host(secs.parse()?),
        (Some("--join"), None) => Mode::Join,
        (Some("--friend"), Some(other)) => Mode::Friend(other.to_owned(), rest.next().unwrap_or("30").parse()?),
        (Some("--answer"), Some(secs)) => Mode::Answer(secs.parse()?),
        _ => bail!("usage: crosscom-testbot SERVER_IP NAME PASSWORD [HTTP_PORT] [--signup [CODE]] [--host SECONDS | --join | --friend NAME SECONDS | --answer SECONDS]"),
    };

    let config = online_config(server, http_port).await?;
    let auth_port = config
        .split("prudp:/address=")
        .nth(1)
        .and_then(|s| s.split("port=").nth(1))
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|p| p.parse::<u16>().ok())
        .ok_or_else(|| eyre!("no SandboxUrl in the online config"))?;
    println!("ok   online config: sign in on port {auth_port}");
    if let Some(code) = signup {
        let (status, body) = sign_up(server, http_port, name, password, code.as_deref()).await?;
        if !status.starts_with("HTTP/1.1 201") {
            bail!("sign-up refused: {status} {body}");
        }
        println!("ok   signed up as {name}: {body}");
    }
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

    match mode {
        Mode::Check => {}
        Mode::Host(secs) => host(&mut conn, pid, &registered, secs).await?,
        Mode::Join => join(&mut conn, pid).await?,
        Mode::Friend(other, secs) => befriend(&mut conn, &other, secs).await?,
        Mode::Answer(secs) => answer(&mut conn, secs).await?,
    }

    conn.disconnect().await?;
    println!("all good");
    Ok(())
}

enum Mode {
    Check,
    Host(u64),
    Join,
    Friend(String, u64),
    Answer(u64),
}

/// What the game's quick match searches with, and the match it makes when it finds none: a Conflict
/// match by a game with every DLC pack (115 = 3, Arctic and Khyber Strike), as a player's game
/// with them searches, so the bot finds their lobbies and they find its.
const QUICK_MATCH_SEARCH: [(u32, u32); 8] = [(3, 1), (100, 1), (101, 2), (116, 2), (105, 16), (103, 0), (115, 3), (114, 12)];
const QUICK_MATCH: [(u32, u32); 14] = [
    (7, 0),
    (3, 12),
    (4, 0),
    (100, 1),
    (101, 2),
    (116, 2),
    (105, 16),
    (115, 3),
    (103, 0),
    (113, 10),
    (114, 12),
    (112, 3),
    (117, 0),
    (102, 3),
];

fn properties(list: &[(u32, u32)]) -> QList<Property> {
    QList(list.iter().map(|&(id, value)| Property { id, value }).collect())
}

/// Registers this bot's peer-to-peer station as the game does: at its public address, with its
/// connection's RVCID, so others can have it probe them.
async fn register_urls(conn: &mut Conn, pid: u32, registered: &secure::RegisterExResponse) -> Result<()> {
    let port = conn.local_addr()?.port();
    let url = format!(
        "udp:/address={};port={port};PID={pid};RVCID={}",
        registered.url_public.address, registered.pid_connection_id
    );
    let station: StationURL = url.parse().map_err(|e| eyre!("{e:?}"))?;
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::RegisterUrLs as u32,
        gs::RegisterUrLsRequest {
            station_urls: QList(vec![station]),
        }
        .to_bytes(),
    )
    .await?;
    println!("ok   registered {url}");
    Ok(())
}

async fn host(conn: &mut Conn, pid: u32, registered: &secure::RegisterExResponse, secs: u64) -> Result<()> {
    register_urls(conn, pid, registered).await?;
    let created: gs::CreateSessionResponse = decode(
        &conn
            .call(
                gs::GAME_SESSION_PROTOCOL_ID,
                gs::GameSessionProtocolMethod::CreateSession as u32,
                gs::CreateSessionRequest {
                    game_session: GameSession {
                        type_id: 1,
                        attributes: properties(&QUICK_MATCH),
                    },
                }
                .to_bytes(),
            )
            .await?,
    )?;
    let key = created.game_session_key;
    println!("ok   hosting quick match {}; waiting {secs} s for a player", key.session_id);
    let probe = conn
        .wait_request(
            nat::NAT_TRAVERSAL_PROTOCOL_ID,
            nat::NatTraversalProtocolMethod::InitiateProbe as u32,
            std::time::Duration::from_secs(secs),
        )
        .await?;
    match probe {
        Some(request) => {
            let probe: nat::InitiateProbeRequest = decode(&request.parameters)?;
            println!("ok   asked to probe {}: a player is on the way in", probe.url_station_to_probe);
        }
        None => println!("--   no player came in {secs} s"),
    }
    let count: gs::GetParticipantCountResponse = decode(
        &conn
            .call(
                gs::GAME_SESSION_PROTOCOL_ID,
                gs::GameSessionProtocolMethod::GetParticipantCount as u32,
                gs::GetParticipantCountRequest {
                    game_session_key: GameSessionKey { ..key },
                }
                .to_bytes(),
            )
            .await?,
    )?;
    println!("ok   {} players in the match", count.count);
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::DeleteSession as u32,
        gs::DeleteSessionRequest { game_session_key: key }.to_bytes(),
    )
    .await?;
    println!("ok   match ended");
    Ok(())
}

async fn join(conn: &mut Conn, pid: u32) -> Result<()> {
    let found: gs::SearchSessionsResponse = decode(
        &conn
            .call(
                gs::GAME_SESSION_PROTOCOL_ID,
                gs::GameSessionProtocolMethod::SearchSessions as u32,
                gs::SearchSessionsRequest {
                    game_session_query: GameSessionQuery {
                        type_id: 1,
                        query_id: 2,
                        parameters: properties(&QUICK_MATCH_SEARCH),
                    },
                }
                .to_bytes(),
            )
            .await?,
    )?;
    let Some(session) = found.search_results.0.into_iter().next() else {
        bail!("no quick match found");
    };
    let key = session.session_key;
    println!(
        "ok   found quick match {} hosted by {:#x} at {}",
        key.session_id,
        session.host_pid,
        session.host_urls.0.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    );
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::JoinSession as u32,
        gs::JoinSessionRequest {
            game_session_key: GameSessionKey { ..key },
        }
        .to_bytes(),
    )
    .await?;
    println!("ok   joined");
    let urls: gs::GetParticipantsUrLsResponse = decode(
        &conn
            .call(
                gs::GAME_SESSION_PROTOCOL_ID,
                gs::GameSessionProtocolMethod::GetParticipantsUrLs as u32,
                gs::GetParticipantsUrLsRequest {
                    game_session_key: GameSessionKey { ..key },
                    participant_ids: QList(vec![session.host_pid, pid]),
                }
                .to_bytes(),
            )
            .await?,
    )?;
    println!("ok   participants' addresses: {:?}", urls);
    conn.call(
        nat::NAT_TRAVERSAL_PROTOCOL_ID,
        nat::NatTraversalProtocolMethod::RequestProbeInitiation as u32,
        nat::RequestProbeInitiationRequest {
            url_target_list: QList(session.host_urls.0),
        }
        .to_bytes(),
    )
    .await?;
    println!("ok   asked the host to probe this bot");
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::AbandonSession as u32,
        gs::AbandonSessionRequest { game_session_key: key }.to_bytes(),
    )
    .await?;
    println!("ok   left the match");
    Ok(())
}

/// Waits for an event the server pushes (ProcessNotificationEvent) of a category.
async fn event(conn: &mut Conn, category: u32, secs: u64) -> Result<NotificationEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let Some(request) = conn.wait_request(14, 1, left).await? else {
            bail!("no event of category {category} in {secs} s");
        };
        let event: NotificationEvent = decode(&request.parameters)?;
        println!(
            "ok   event {} from {:#x} ({}, {}, {:?})",
            event.ui_type, event.pid_source, event.ui_param_1, event.ui_param_2, event.str_param
        );
        if event.ui_type / 1000 == category {
            return Ok(event);
        }
    }
}

async fn relationships(conn: &mut Conn) -> Result<()> {
    let list: friends::GetRelationshipsResponse = decode(
        &conn
            .call(
                friends::FRIENDS_PROTOCOL_ID,
                friends::FriendsProtocolMethod::GetRelationships as u32,
                friends::GetRelationshipsRequest {
                    result_range: quazal::rmc::types::ResultRange { offset: 0, size: 100 },
                }
                .to_bytes(),
            )
            .await?,
    )?;
    for r in &list.lst_relationships_list {
        println!("ok   relationship: {} ({:#x}) {} online {}", r.str_name, r.pid, r.by_relationship, r.by_status);
    }
    Ok(())
}

async fn befriend(conn: &mut Conn, other: &str, secs: u64) -> Result<()> {
    let added: friends::AddFriendByNameResponse = decode(
        &conn
            .call(
                friends::FRIENDS_PROTOCOL_ID,
                friends::FriendsProtocolMethod::AddFriendByName as u32,
                friends::AddFriendByNameRequest {
                    str_player_name: other.into(),
                    ui_details: 0,
                    str_message: "a bot would like to be friends".into(),
                }
                .to_bytes(),
            )
            .await?,
    )?;
    println!("ok   asked {other} to be friends: {}", added.return_value);
    let accepted = event(conn, 1, secs).await?;
    if accepted.ui_param_2 != 1 {
        bail!("expected 'now friends', got relationship {}", accepted.ui_param_2);
    }
    relationships(conn).await?;
    let created: gs::CreateSessionResponse = decode(
        &conn
            .call(
                gs::GAME_SESSION_PROTOCOL_ID,
                gs::GameSessionProtocolMethod::CreateSession as u32,
                gs::CreateSessionRequest {
                    game_session: GameSession {
                        type_id: 1,
                        attributes: properties(&[(7, 1), (3, 6), (4, 0), (102, 0), (100, 1)]),
                    },
                }
                .to_bytes(),
            )
            .await?,
    )?;
    let key = created.game_session_key;
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::SendInvitation as u32,
        gs::SendInvitationRequest {
            invitation: GameSessionInvitation {
                session_key: GameSessionKey { ..key },
                recipient_pids: QList(vec![accepted.pid_source]),
                message: "join my party".into(),
            },
        }
        .to_bytes(),
    )
    .await?;
    println!("ok   invited {other} into party {}", key.session_id);
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let count: gs::GetParticipantCountResponse = decode(
        &conn
            .call(
                gs::GAME_SESSION_PROTOCOL_ID,
                gs::GameSessionProtocolMethod::GetParticipantCount as u32,
                gs::GetParticipantCountRequest {
                    game_session_key: GameSessionKey { ..key },
                }
                .to_bytes(),
            )
            .await?,
    )?;
    println!("ok   {} players in the party", count.count);
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::DeleteSession as u32,
        gs::DeleteSessionRequest { game_session_key: key }.to_bytes(),
    )
    .await?;
    Ok(())
}

async fn answer(conn: &mut Conn, secs: u64) -> Result<()> {
    let request = event(conn, 1, secs).await?;
    if request.ui_param_2 != 2 {
        bail!("expected a friend request, got relationship {}", request.ui_param_2);
    }
    relationships(conn).await?;
    let accepted: friends::AcceptFriendshipResponse = decode(
        &conn
            .call(
                friends::FRIENDS_PROTOCOL_ID,
                friends::FriendsProtocolMethod::AcceptFriendship as u32,
                friends::AcceptFriendshipRequest { ui_player: request.pid_source }.to_bytes(),
            )
            .await?,
    )?;
    println!("ok   accepted: {}", accepted.return_value);
    let invite = event(conn, 7, secs).await?;
    let received: gs::GetInvitationsReceivedResponse = decode(
        &conn
            .call(
                gs::GAME_SESSION_PROTOCOL_ID,
                gs::GameSessionProtocolMethod::GetInvitationsReceived as u32,
                gs::GetInvitationsReceivedRequest {
                    game_session_type_id: invite.ui_param_1,
                    result_range: quazal::rmc::types::ResultRange { offset: 0, size: 10 },
                }
                .to_bytes(),
            )
            .await?,
    )?;
    let Some(invitation) = received.invitations.0.into_iter().next() else {
        bail!("told of an invitation, but none is listed");
    };
    println!(
        "ok   invitation into {} from {:#x}: {:?}",
        invitation.session_key.session_id, invitation.sender_pid, invitation.message
    );
    let key = GameSessionKey { ..invitation.session_key };
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::AcceptInvitation as u32,
        gs::AcceptInvitationRequest {
            game_session_invitation: invitation,
        }
        .to_bytes(),
    )
    .await?;
    conn.call(
        gs::GAME_SESSION_PROTOCOL_ID,
        gs::GameSessionProtocolMethod::JoinSession as u32,
        gs::JoinSessionRequest { game_session_key: key }.to_bytes(),
    )
    .await?;
    println!("ok   accepted the invitation and joined the party");
    Ok(())
}
