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
    signed_in: SignIns,
}

/// Sign-ins that succeed, per address and per account, as (burst, then a second): each makes a
/// ticket, and the telemetry account (whose password is public) or a player's own could
/// otherwise be signed in as over and over. Per address, a burst for a household or a LAN party
/// signing in together (two each: the player and the telemetry account); per account (not the
/// shared telemetry one), a game restarted a few times in a row.
const SIGN_INS_PER_ADDRESS: (f64, f64) = (60.0, 1.0);
const SIGN_INS_PER_ACCOUNT: (f64, f64) = (10.0, 1.0 / 30.0);
/// Addresses and accounts counted at once; the one counted longest ago makes room.
const MAX_SIGN_IN_SOURCES: usize = 20_000;

/// A token bucket: the tokens, and when it was last filled.
type Bucket = (f64, Instant);

#[derive(Default)]
struct SignIns(std::sync::Mutex<SignInTables>);

#[derive(Default)]
struct SignInTables {
    /// Players' sign-ins, by address.
    by_address: std::collections::HashMap<std::net::IpAddr, Bucket>,
    /// The shared telemetry account's, by address: apart, so anyone behind a player's address
    /// (it needs no secret) can't use up the players' own.
    tracking_by_address: std::collections::HashMap<std::net::IpAddr, Bucket>,
    by_account: std::collections::HashMap<u32, Bucket>,
}

/// `key`'s bucket, refilled until `now` (a new key's starts full). A full table first forgets its
/// oldest bucket.
fn bucket<K: std::hash::Hash + Eq + Copy>(table: &mut std::collections::HashMap<K, Bucket>, key: K, (burst, rate): (f64, f64), now: Instant) -> &mut Bucket {
    if table.len() >= MAX_SIGN_IN_SOURCES && !table.contains_key(&key) {
        if let Some(oldest) = table.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| *k) {
            table.remove(&oldest);
        }
    }
    let bucket = table.entry(key).or_insert((burst, now));
    bucket.0 = (bucket.0 + now.saturating_duration_since(bucket.1).as_secs_f64() * rate).min(burst);
    bucket.1 = bucket.1.max(now);
    bucket
}

impl SignIns {
    fn tables(&self) -> std::sync::MutexGuard<'_, SignInTables> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether `from` may sign in now (asked before the password is, which costs to check).
    fn address_may(&self, from: std::net::IpAddr, now: Instant) -> bool {
        per_address_lifted() || bucket(&mut self.tables().by_address, crate::rate_limit::address_key(from), SIGN_INS_PER_ADDRESS, now).0 >= 1.0
    }

    /// Counts a sign-in as `pid` from `from` that succeeded; false (and nothing counted) when the
    /// address or the account is over its rate. The shared telemetry account is counted by
    /// address only: every game signs in as it.
    fn succeeded(&self, from: std::net::IpAddr, pid: u32, now: Instant) -> bool {
        let mut tables = self.tables();
        let tables = &mut *tables;
        let lifted = per_address_lifted();
        let by_address = if pid == TRACKING_PID { &mut tables.tracking_by_address } else { &mut tables.by_address };
        if !lifted && bucket(by_address, crate::rate_limit::address_key(from), SIGN_INS_PER_ADDRESS, now).0 < 1.0 {
            return false;
        }
        if pid != TRACKING_PID {
            let account = bucket(&mut tables.by_account, pid, SIGN_INS_PER_ACCOUNT, now);
            if account.0 < 1.0 {
                return false;
            }
            account.0 -= 1.0;
        }
        if !lifted {
            let by_address = if pid == TRACKING_PID { &mut tables.tracking_by_address } else { &mut tables.by_address };
            bucket(by_address, crate::rate_limit::address_key(from), SIGN_INS_PER_ADDRESS, now).0 -= 1.0;
        }
        true
    }
}

/// Whether the per-address limits are lifted (`FE_MAX_CONNECTIONS_PER_IP`: the load test's
/// players all sign in from one address).
fn per_address_lifted() -> bool {
    quazal::prudp::many_per_ip().is_some()
}

/// Refuses a sign-in on a connection that already signed in (as anyone): one connection is one
/// sign-in, so a connection can't make ticket after ticket, nor change who it is.
fn not_signed_in_yet<CI>(logger: &Logger, ci: &ClientInfo<CI>) -> Result<(), Error> {
    if let Some(pid) = ci.user_id {
        warn!(logger, "a second sign-in on {}'s connection (signed in as {pid:#x}) refused", ci.address());
        return Err(Error::AccessDenied);
    }
    Ok(())
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
        not_signed_in_yet(logger, ci)?;
        if !self.signed_in.succeeded(ci.address().ip(), TRACKING_PID, Instant::now()) {
            warn!(logger, "Login as {TRACKING_NAME} from {} refused: too many sign-ins from there", ci.address());
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
        not_signed_in_yet(logger, ci)?;
        if !self.signed_in.address_may(ci.address().ip(), Instant::now()) {
            warn!(logger, "sign-in from {} refused: too many sign-ins from there", ci.address());
            return Err(Error::AccessDenied);
        }
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
        if !self.signed_in.succeeded(from, pid, Instant::now()) {
            warn!(logger, "sign-in as {:?} from {} refused: signed in too often lately", login.user_name, ci.address());
            return Err(Error::AccessDenied);
        }
        // The DLC packs Cross-Com's client says the game has (see `packs`).
        let packs = crate::packs::from_online_key(&login.online_key);
        self.packs.signed_in(pid, packs);
        // A new sign-in is a game started afresh: it's in none of the sessions its last game was.
        crate::connections::gone(logger, pid);
        crate::announce::forget(pid);
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
        signed_in: SignIns::default(),
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

    /// Sign-ins that succeed: a burst then a rate per address, and per account (but not the
    /// shared telemetry account, which every game signs in as).
    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn successful_sign_ins_are_limited_per_address_and_account() {
        let limits = SignIns::default();
        let now = Instant::now();
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        let player = 0x0010_0000;
        let ok = (0..20).filter(|i| limits.succeeded(ip(&format!("192.0.2.{i}")), player, now)).count();
        assert_eq!(ok, SIGN_INS_PER_ACCOUNT.0 as usize, "one account, from anywhere");
        assert!(limits.succeeded(ip("192.0.2.99"), player, now + Duration::from_secs(31)), "one more after a while");
        let ok = (0..100).filter(|_| limits.succeeded(ip("198.51.100.1"), TRACKING_PID, now)).count();
        assert_eq!(ok, SIGN_INS_PER_ADDRESS.0 as usize, "the telemetry account: per address only");
        assert!(limits.address_may(ip("198.51.100.1"), now), "its own budget: players there still sign in");
        assert!(limits.succeeded(ip("198.51.100.1"), player + 1, now));
        // Players' sign-ins, many accounts from one address, use up that address's.
        let ok = (0..100).filter(|n| limits.succeeded(ip("203.0.113.5"), player + 10 + n, now)).count();
        assert_eq!(ok, SIGN_INS_PER_ADDRESS.0 as usize);
        assert!(!limits.address_may(ip("203.0.113.5"), now));
        assert!(limits.address_may(ip("203.0.113.6"), now));
        for n in 0..SIGN_INS_PER_ADDRESS.0 as u32 {
            assert!(limits.succeeded(ip("2001:db8::1"), player + 200 + n, now));
        }
        assert!(!limits.address_may(ip("2001:db8::2"), now), "IPv6 counts by its /64");
    }

    #[test]
    fn a_connection_signs_in_once() {
        let logger = Logger::root(slog::Discard, slog::o!());
        let mut ci = ClientInfo::<()>::new("192.0.2.1:3074".parse().unwrap());
        assert!(not_signed_in_yet(&logger, &ci).is_ok());
        ci.user_id = Some(TRACKING_PID);
        assert!(not_signed_in_yet(&logger, &ci).is_err());
    }

    #[test]
    fn only_tracking_has_its_own_ticket_password() {
        assert_eq!(ticket_password(TRACKING_PID), Some("JaDe!"));
        assert_eq!(ticket_password(0x0010_0000), None);
    }
}
