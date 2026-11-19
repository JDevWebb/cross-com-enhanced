//! Signing in (`TicketGrantingProtocol`, on the auth server).
//!
//! Players sign in with `LoginEx`, sending their account name, CD key and password in a
//! `UbiAuthenticationLoginCustomData`. The game's built-in tracking principal signs in with plain
//! `Login`. Both get a Kerberos ticket for the secure server, encrypted with a key derived from a
//! password the game knows: `UbiDummyPwd` for players, `JaDe!` for tracking.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use grfs_protocols::authentication_foundation::ticket_granting_protocol::*;
use grfs_protocols::authentication_foundation::types::RVConnectionData;
use grfs_protocols::ubi_authentication::types::UbiAuthenticationLoginCustomData;
use quazal::kerberos::KerberosTicket;
use quazal::kerberos::KerberosTicketInternal;
use quazal::kerberos::SESSION_KEY_SIZE;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::ClassRegistry;
use quazal::rmc::types::QResult;
use quazal::rmc::types::StationURL;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use rand::TryRngCore;
use slog::info;
use slog::warn;
use slog::Logger;

use crate::accounts::Accounts;
use crate::rate_limit::FailedSignIns;

/// The secure server's principal ID.
pub const SERVER_PID: u32 = 0x1000;
/// The game's built-in tracking principal.
pub const TRACKING_NAME: &str = "Tracking";
pub const TRACKING_PID: u32 = 0x1111;
const TRACKING_PASSWORD: &str = "JaDe!";
const TICKET_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

struct TicketGranting {
    accounts: Arc<Accounts>,
    failed: Arc<FailedSignIns>,
    packs: Arc<crate::packs::Packs>,
    sessions: Arc<crate::sessions::Sessions>,
}

/// The signed-in player. The tracking principal, which anyone can sign in as (its password is
/// built into the game), isn't a player and is refused.
pub fn player<CI>(ci: &ClientInfo<CI>) -> Result<u32, Error> {
    match ci.user_id {
        Some(pid) if pid != TRACKING_PID => Ok(pid),
        _ => Err(Error::AccessDenied),
    }
}

/// The password a principal's tickets are encrypted for; `None` is the game's default
/// (`UbiDummyPwd`), which players use.
fn ticket_password(pid: u32) -> Option<&'static str> {
    (pid == TRACKING_PID).then_some(TRACKING_PASSWORD)
}

fn ticket(ctx: &Context, ci_address: &SocketAddr, user_pid: u32, server_pid: u32) -> Vec<u8> {
    let mut session_key = [0u8; SESSION_KEY_SIZE];
    rand::rngs::OsRng.try_fill_bytes(&mut session_key).expect("the system's random number generator");
    let valid_until = (SystemTime::now() + TICKET_LIFETIME).duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(u64::MAX);
    let ticket = KerberosTicket {
        session_key,
        pid: server_pid,
        internal: KerberosTicketInternal {
            principle_id: user_pid,
            valid_until,
            session_key,
            issued_to: [0; 16],
        }
        .for_address(ci_address.ip()),
    };
    ticket.as_bytes(user_pid, ticket_password(user_pid), &ctx.ticket_key)
}

/// Where the secure server is, for the game to connect to next.
fn connection_data(ctx: &Context) -> Result<RVConnectionData, Error> {
    let secure = ctx.secure_server_addr.ok_or(Error::InternalError)?;
    Ok(RVConnectionData {
        url_regular_protocols: format!("prudps:/address={};port={};CID=1;PID={SERVER_PID};sid=1;stream=3;type=2", secure.ip(), secure.port())
            .parse()
            .map_err(|_| Error::InternalError)?,
        lst_special_protocols: vec![],
        url_special_protocols: StationURL::default(),
    })
}

impl<CI> TicketGrantingProtocolServerTrait<CI> for TicketGranting {
    fn login(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: LoginRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<LoginResponse, Error> {
        // Players sign in with LoginEx; plain Login is only the game's tracking principal.
        if request.str_user_name != TRACKING_NAME {
            warn!(logger, "plain Login as {:?} from {} refused", request.str_user_name, ci.address());
            return Err(Error::AccessDenied);
        }
        ci.user_id = Some(TRACKING_PID);
        Ok(LoginResponse {
            return_value: QResult::Ok,
            pid_principal: TRACKING_PID,
            pbuf_response: ticket(ctx, ci.address(), TRACKING_PID, SERVER_PID),
            p_connection_data: connection_data(ctx)?,
            str_return_msg: String::new(),
        })
    }

    fn login_ex(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: LoginExRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<LoginExResponse, Error> {
        let mut classes = ClassRegistry::default();
        classes.register_class::<UbiAuthenticationLoginCustomData>("UbiAuthenticationLoginCustomData");
        let data = request.o_extra_data.into_inner(&classes)?;
        let Some(login) = data.as_any().downcast_ref::<UbiAuthenticationLoginCustomData>() else {
            warn!(logger, "LoginEx from {} without UbiAuthenticationLoginCustomData", ci.address());
            return Err(Error::ParsingError);
        };
        let from = ci.address().ip();
        // A name no account can have is refused before it's counted, so it can't fill the limits.
        if !crate::accounts::valid_name(&login.user_name) {
            warn!(logger, "sign-in from {} refused: not a valid name", ci.address());
            return Err(Error::AccessDenied);
        }
        let Some(attempt) = self.failed.try_begin(from, &login.user_name, Instant::now()) else {
            warn!(logger, "sign-in as {:?} from {} refused: too many failed sign-ins", login.user_name, ci.address());
            return Err(Error::AccessDenied);
        };
        let pid = match self.accounts.verify(&login.user_name, &login.password) {
            Ok(Some(pid)) => {
                self.failed.succeeded(attempt);
                pid
            }
            Ok(None) => {
                warn!(logger, "sign-in as {:?} from {} refused: wrong name or password", login.user_name, ci.address());
                return Err(Error::AccessDenied);
            }
            Err(e) => {
                warn!(logger, "sign-in as {:?} failed: {e}", login.user_name);
                return Err(Error::InternalError);
            }
        };
        // The DLC packs Cross-Com's client says the game has (see `packs`).
        let packs = crate::packs::from_online_key(&login.online_key);
        self.packs.signed_in(pid, packs);
        // A new sign-in is a game started afresh: it's in none of the sessions its last game was.
        let stale = self.sessions.signed_in_afresh(pid);
        if stale > 0 {
            info!(logger, "{pid:#x} signed in again: taken out of {stale} session(s) its last game was in");
        }
        // On one line: `{:#x?}` would pretty-print the Option over three.
        let packs_said = packs.map_or_else(|| "not reported (an older client)".to_owned(), |p| format!("{p:#x}"));
        info!(logger, "{:?} signed in from {} (PID {pid:#x}, DLC packs {packs_said})", login.user_name, ci.address());
        ci.user_id = Some(pid);
        Ok(LoginExResponse {
            return_value: QResult::Ok,
            pid_principal: pid,
            pbuf_response: ticket(ctx, ci.address(), pid, SERVER_PID),
            p_connection_data: connection_data(ctx)?,
            str_return_msg: String::new(),
        })
    }

    fn request_ticket(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RequestTicketRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RequestTicketResponse, Error> {
        if ci.user_id != Some(request.id_source) {
            warn!(logger, "ticket for {:#x} requested by {:?} from {}; refused", request.id_source, ci.user_id, ci.address());
            return Err(Error::AccessDenied);
        }
        Ok(RequestTicketResponse {
            return_value: QResult::Ok,
            buf_response: ticket(ctx, ci.address(), request.id_source, request.id_target),
        })
    }
}

/// The sign-in protocol. `failed` counts failed sign-ins, shared with the launcher's.
pub fn new_protocol<CI: 'static>(
    accounts: Arc<Accounts>,
    failed: Arc<FailedSignIns>,
    packs: Arc<crate::packs::Packs>,
    sessions: Arc<crate::sessions::Sessions>,
) -> Box<dyn Protocol<CI>> {
    Box::new(TicketGrantingProtocolServer::new(TicketGranting {
        accounts,
        failed,
        packs,
        sessions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_is_not_a_player() {
        let mut ci = ClientInfo::<()>::new("192.0.2.1:3074".parse().unwrap());
        assert!(player(&ci).is_err());
        ci.user_id = Some(TRACKING_PID);
        assert!(player(&ci).is_err());
        ci.user_id = Some(0x0010_0000);
        assert_eq!(player(&ci).unwrap(), 0x0010_0000);
    }

    #[test]
    fn only_tracking_has_its_own_ticket_password() {
        assert_eq!(ticket_password(TRACKING_PID), Some("JaDe!"));
        assert_eq!(ticket_password(0x0010_0000), None);
    }
}
