//! Game sessions (`GameSessionProtocol`): what the game makes when a player opens Multiplayer,
//! and finds, joins and leaves. The sessions themselves are in `sessions`.

use std::net::IpAddr;
use std::sync::Arc;

use grfs_protocols::game_session_service::game_session_protocol::*;
use grfs_protocols::game_session_service::types::GameSessionInvitationReceived;
use grfs_protocols::game_session_service::types::GameSessionInvitationSent;
use grfs_protocols::game_session_service::types::GameSessionKey;
use grfs_protocols::game_session_service::types::GameSessionParticipant;
use grfs_protocols::game_session_service::types::GameSessionSearchResult;
use grfs_protocols::game_session_service::types::GameSessionSearchWithParticipantsResult;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::DateTime;
use quazal::rmc::types::Property;
use quazal::rmc::types::QList;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::info;
use slog::Logger;

use crate::accounts::Accounts;
use crate::connections;
use crate::friends::Friends;
use crate::invitations::Invitations;
use crate::notify;
use crate::packs::Packs;
use crate::sessions;
use crate::sessions::Sessions;
use crate::ticket::player;

/// How long a host's game has been silent when another player may take over hosting. A game asks
/// about 10 s after losing its host, and pings every 10 s: so a live host may now and then have
/// been quiet this long too. That's accepted, and bounded: only a player in the match may, once
/// a minute (`Sessions::take_over`). What it gains is the session's listing here (its settings and
/// who is listed in it), not the match itself, which the players' games run between them.
const GONE_AFTER: std::time::Duration = std::time::Duration::from_secs(8);

/// Whether `pid`'s game has gone (not heard from for [`GONE_AFTER`], or never).
fn gone(pid: u32) -> bool {
    quazal::last_heard(pid).is_none_or(|t| t.elapsed() > GONE_AFTER)
}

/// How long a host's game may have been silent and its lobby still be offered. Longer than
/// [`GONE_AFTER`]: a search mustn't miss a live host between two pings. A host whose game has gone
/// (it crashed, or its PC lost the connection) keeps its listing for host migration, but nobody
/// new is sent to it.
const OFFERED_WHILE_HEARD: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether `pid`'s game was heard from lately enough for its lobby to be offered.
fn heard_lately(pid: u32) -> bool {
    quazal::last_heard(pid).is_some_and(|t| t.elapsed() < OFFERED_WHILE_HEARD)
}

/// Results a search returns at most, and players one invitation may go to.
const MAX_RESULTS: usize = 100;
const MAX_RECIPIENTS: usize = 16;

struct GameSessions {
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
    friends: Arc<Friends>,
    packs: Arc<Packs>,
    invitations: Invitations,
}

/// The lobbies a search offers (at most [`MAX_RESULTS`]), and how many were held back because the
/// player can't punch through (`strict`) and their host can't be reached (`open`, by host).
/// `open` is asked after the sessions are looked at, not while: it looks at them itself.
fn offered(
    sessions: &Sessions,
    type_id: u32,
    parameters: &[(u32, u32)],
    strict: bool,
    playable: impl Fn(&sessions::Session) -> bool,
    open: impl Fn(u32) -> Option<bool>,
) -> (Vec<(sessions::Key, sessions::Session)>, usize) {
    let limit = if strict { MAX_RESULTS * 4 } else { MAX_RESULTS };
    let candidates = sessions.find_where(type_id, parameters, limit, playable);
    if !strict {
        return (candidates, 0);
    }
    let (mut found, held): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|(_, s)| offered_to_strict(open(s.host)));
    found.truncate(MAX_RESULTS);
    (found, held.len())
}

/// Whether a lobby is offered to a player who can't punch through, by what its host's announced
/// address answered: not when it was checked and didn't answer; when it did, or it wasn't
/// checked (a server on a private network, a check under way), it is.
fn offered_to_strict(host_open: Option<bool>) -> bool {
    host_open != Some(false)
}

fn key(k: &GameSessionKey) -> sessions::Key {
    (k.type_id, k.session_id)
}

fn properties(list: &QList<Property>) -> Vec<(u32, u32)> {
    list.0.iter().map(|p| (p.id, p.value)).collect()
}

fn error(e: sessions::Error) -> Error {
    match e {
        sessions::Error::NotFound => Error::AccessDenied,
        sessions::Error::NotHost => Error::AccessDenied,
        sessions::Error::Full => Error::InternalError,
    }
}

impl GameSessions {
    /// A session as `viewer` (the address the server sees the asking player at) is given it.
    fn result(&self, viewer: IpAddr, (k, session): (sessions::Key, sessions::Session)) -> GameSessionSearchResult {
        GameSessionSearchResult {
            session_key: GameSessionKey { type_id: k.0, session_id: k.1 },
            host_pid: session.host,
            host_urls: QList(self.sessions.urls_seen_from(session.host, viewer)),
            attributes: QList(session.properties().into_iter().map(|(id, value)| Property { id, value }).collect()),
        }
    }

    /// A participant as other members see them: name and addresses only for those who joined
    /// themselves.
    fn participant(&self, viewer: IpAddr, session: &sessions::Session, pid: u32) -> GameSessionParticipant {
        let joined = session.joined(pid);
        GameSessionParticipant {
            pid,
            name: if joined {
                self.accounts.name(pid).ok().flatten().unwrap_or_default()
            } else {
                String::new()
            },
            station_urls: QList(if joined { self.sessions.urls_seen_from(pid, viewer) } else { vec![] }),
        }
    }

    /// The session, if `pid` is in it: only members see who else is.
    fn member_of(&self, pid: u32, k: sessions::Key) -> Result<sessions::Session, Error> {
        self.sessions.get(k).filter(|s| s.has(pid)).ok_or(Error::AccessDenied)
    }
}

impl<CI> GameSessionProtocolServerTrait<CI> for GameSessions {
    fn create_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: CreateSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<CreateSessionResponse, Error> {
        let pid = player(ci)?;
        let attributes = properties(&request.game_session.attributes);
        let (type_id, session_id) = self.sessions.create(pid, request.game_session.type_id, &attributes).map_err(error)?;
        info!(logger, "{pid:#x} created session {type_id}/{session_id} {attributes:?}");
        Ok(CreateSessionResponse {
            game_session_key: GameSessionKey { type_id, session_id },
        })
    }

    fn update_session(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: UpdateSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<UpdateSessionResponse, Error> {
        let pid = player(ci)?;
        let update = &request.game_session_update;
        match self.sessions.update(pid, key(&update.session_key), &properties(&update.attributes)) {
            // The game updates a match once more as it leaves it, after the session ended with
            // its last player: there's nothing left to update, and that's not an error.
            Ok(()) | Err(sessions::Error::NotFound) => Ok(UpdateSessionResponse),
            Err(e) => Err(error(e)),
        }
    }

    fn delete_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: DeleteSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<DeleteSessionResponse, Error> {
        let pid = player(ci)?;
        self.sessions.delete(pid, key(&request.game_session_key)).map_err(error)?;
        info!(logger, "{pid:#x} deleted session {:?}", key(&request.game_session_key));
        Ok(DeleteSessionResponse)
    }

    fn migrate_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: MigrateSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<MigrateSessionResponse, Error> {
        // The session keeps its key; the player asking hosts it from now on if its host has gone
        // (see `Sessions::take_over`). The other players ask too, after the first: they're told
        // yes, and the session stays with the first.
        let pid = player(ci)?;
        let k = key(&request.game_session_key);
        let before = self.sessions.get(k).map(|s| s.host);
        if self.sessions.take_over(k, pid, gone) {
            if before != Some(pid) {
                info!(logger, "{pid:#x} hosts session {k:?} now, after {:#x}", before.unwrap_or_default());
            }
        } else if self.sessions.is_member(pid, k) {
            info!(logger, "{pid:#x} asked to host session {k:?}; its host {:#x} stays", before.unwrap_or_default());
        } else {
            return Err(Error::AccessDenied);
        }
        Ok(MigrateSessionResponse {
            game_session_key_migrated: request.game_session_key,
        })
    }

    fn leave_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: LeaveSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<LeaveSessionResponse, Error> {
        let pid = player(ci)?;
        connections::ended(logger, pid, key(&request.game_session_key), connections::End::Left);
        self.sessions.remove(key(&request.game_session_key), &[pid]).map_err(error)?;
        Ok(LeaveSessionResponse)
    }

    fn get_session(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetSessionResponse, Error> {
        player(ci)?;
        let k = key(&request.game_session_key);
        let session = self.sessions.get(k).ok_or(Error::AccessDenied)?;
        Ok(GetSessionResponse {
            search_result: self.result(ci.address().ip(), (k, session)),
        })
    }

    fn search_sessions(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SearchSessionsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SearchSessionsResponse, Error> {
        let pid = player(ci)?;
        let query = &request.game_session_query;
        // Only lobbies the player's game can load: their packs and their mode's (see `packs`).
        let playable = |s: &crate::sessions::Session| s.host != pid && heard_lately(s.host) && self.packs.offer(pid, s.host, s.attribute(crate::packs::MODE));
        // A player who can't punch through (a symmetric NAT, no forward) only to hosts that can
        // be reached from anywhere: a lobby whose host can't be would end in a failed join, and
        // with none found the game hosts its own (see `announce`).
        let strict = self.sessions.punches_poorly(pid);
        let (found, held) = offered(&self.sessions, query.type_id, &properties(&query.parameters), strict, playable, crate::announce::open);
        if held > 0 {
            info!(
                logger,
                "{pid:#x} can't punch through (symmetric NAT, no forward): not offered {held} lobby(ies) whose host can't be reached from outside"
            );
        }
        // The game joins a session it found by reaching its host (a NAT probe), who then adds
        // it. Being offered a session gives nothing yet; reaching for its host is the choice
        // (see `Sessions::choose`).
        for (k, _) in &found {
            self.sessions.offer(*k, pid);
        }
        Ok(SearchSessionsResponse {
            search_results: QList(found.into_iter().map(|r| self.result(ci.address().ip(), r)).collect()),
        })
    }

    fn send_invitation(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SendInvitationRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<SendInvitationResponse, Error> {
        let pid = player(ci)?;
        let invitation = request.invitation;
        let k = key(&invitation.session_key);
        // Only into a session the sender is in, and only friends.
        let session = self.sessions.get(k).filter(|s| s.joined(pid)).ok_or(Error::AccessDenied)?;
        let now = std::time::Instant::now();
        for &recipient in invitation.recipient_pids.0.iter().take(MAX_RECIPIENTS) {
            if recipient == pid || session.joined(recipient) || !self.friends.are_friends(pid, recipient) {
                info!(logger, "{pid:#x} can't invite {recipient:#x} into {k:?}: not a friend, or already in");
                continue;
            }
            self.invitations
                .send(k, pid, recipient, &invitation.message, crate::fixed::server_time(), now)
                .map_err(|_| Error::AccessDenied)?;
            // The game reaches the members (a NAT probe) before it accepts.
            self.sessions.invite(k, pid, recipient);
            let told = notify::send(
                logger,
                ctx,
                socket,
                client_registry,
                recipient,
                &notify::game_invitation(pid, k.0, k.1, &invitation.message),
            );
            info!(logger, "{pid:#x} invited {recipient:#x} into {k:?} (told: {told})");
        }
        Ok(SendInvitationResponse)
    }

    fn get_invitation_received_count(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetInvitationReceivedCountRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetInvitationReceivedCountResponse, Error> {
        let pid = player(ci)?;
        let live = |k| self.sessions.get(k).is_some();
        let count = self.invitations.received(pid, request.game_session_type_id, live, std::time::Instant::now()).len();
        Ok(GetInvitationReceivedCountResponse {
            count: u32::try_from(count).unwrap_or(u32::MAX),
        })
    }

    fn get_invitations_received(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetInvitationsReceivedRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetInvitationsReceivedResponse, Error> {
        let pid = player(ci)?;
        let live = |k| self.sessions.get(k).is_some();
        let range = request.result_range;
        let found = self.invitations.received(pid, request.game_session_type_id, live, std::time::Instant::now());
        Ok(GetInvitationsReceivedResponse {
            invitations: QList(
                found
                    .into_iter()
                    .skip(range.offset as usize)
                    .take(range.size as usize)
                    .map(|i| GameSessionInvitationReceived {
                        session_key: GameSessionKey {
                            type_id: i.key.0,
                            session_id: i.key.1,
                        },
                        sender_pid: i.sender,
                        message: i.message,
                        creation_time: DateTime(i.created),
                    })
                    .collect(),
            ),
        })
    }

    fn get_invitation_sent_count(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetInvitationSentCountRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetInvitationSentCountResponse, Error> {
        let pid = player(ci)?;
        let live = |k| self.sessions.get(k).is_some();
        let count = self.invitations.sent(pid, request.game_session_type_id, live, std::time::Instant::now()).len();
        Ok(GetInvitationSentCountResponse {
            count: u32::try_from(count).unwrap_or(u32::MAX),
        })
    }

    fn get_invitations_sent(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetInvitationsSentRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetInvitationsSentResponse, Error> {
        let pid = player(ci)?;
        let live = |k| self.sessions.get(k).is_some();
        let range = request.result_range;
        let found = self.invitations.sent(pid, request.game_session_type_id, live, std::time::Instant::now());
        Ok(GetInvitationsSentResponse {
            invitations: QList(
                found
                    .into_iter()
                    .skip(range.offset as usize)
                    .take(range.size as usize)
                    .map(|i| GameSessionInvitationSent {
                        session_key: GameSessionKey {
                            type_id: i.key.0,
                            session_id: i.key.1,
                        },
                        recipient_pid: i.recipient,
                        message: i.message,
                        creation_time: DateTime(i.created),
                    })
                    .collect(),
            ),
        })
    }

    /// The game joins the session itself next; accepting only answers the invitation.
    fn accept_invitation(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AcceptInvitationRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<AcceptInvitationResponse, Error> {
        let pid = player(ci)?;
        let i = request.game_session_invitation;
        self.invitations.remove(key(&i.session_key), i.sender_pid, pid, std::time::Instant::now());
        self.sessions.answer(key(&i.session_key), pid, true);
        Ok(AcceptInvitationResponse)
    }

    fn decline_invitation(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: DeclineInvitationRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<DeclineInvitationResponse, Error> {
        let pid = player(ci)?;
        let i = request.game_session_invitation;
        self.invitations.remove(key(&i.session_key), i.sender_pid, pid, std::time::Instant::now());
        self.sessions.answer(key(&i.session_key), pid, false);
        Ok(DeclineInvitationResponse)
    }

    fn cancel_invitation(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: CancelInvitationRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<CancelInvitationResponse, Error> {
        let pid = player(ci)?;
        let i = request.game_session_invitation;
        self.invitations.remove(key(&i.session_key), pid, i.recipient_pid, std::time::Instant::now());
        self.sessions.uninvite(key(&i.session_key), pid, i.recipient_pid);
        Ok(CancelInvitationResponse)
    }

    fn add_participants(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AddParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<AddParticipantsResponse, Error> {
        let pid = player(ci)?;
        let k = key(&request.game_session_key);
        if !self.sessions.is_host(pid, k) {
            return Err(Error::AccessDenied);
        }
        self.sessions.add(k, &request.public_participant_ids.0, &request.private_participant_ids.0).map_err(error)?;
        for &added in request.public_participant_ids.0.iter().chain(&request.private_participant_ids.0) {
            connections::joined(logger, added, k, pid);
        }
        Ok(AddParticipantsResponse)
    }

    fn remove_participants(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RemoveParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RemoveParticipantsResponse, Error> {
        let pid = player(ci)?;
        let k = key(&request.game_session_key);
        // The host, or a player taking themself out. After a host drops, every player takes it
        // out, the new host first: for the others it's already out, which changes nothing.
        let ids = &request.participant_ids.0;
        let only_self = ids.iter().all(|&p| p == pid);
        let none_in = self.sessions.is_member(pid, k) && self.sessions.get(k).is_some_and(|s| ids.iter().all(|p| !s.has(*p)));
        if none_in {
            return Ok(RemoveParticipantsResponse);
        }
        if !only_self && !self.sessions.is_host(pid, k) {
            return Err(Error::AccessDenied);
        }
        let how = if only_self { connections::End::Left } else { connections::End::Removed };
        for &out in ids {
            connections::ended(logger, out, k, how);
        }
        self.sessions.remove(k, &request.participant_ids.0).map_err(error)?;
        Ok(RemoveParticipantsResponse)
    }

    fn get_participant_count(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetParticipantCountRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetParticipantCountResponse, Error> {
        let pid = player(ci)?;
        let session = self.member_of(pid, key(&request.game_session_key))?;
        #[allow(clippy::cast_possible_truncation)] // bounded by sessions::MAX_PARTICIPANTS
        Ok(GetParticipantCountResponse {
            count: session.participants.len() as u32,
        })
    }

    fn get_participants(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetParticipantsResponse, Error> {
        let pid = player(ci)?;
        let session = self.member_of(pid, key(&request.game_session_key))?;
        let range = &request.result_range;
        let participants = session
            .participants
            .iter()
            .skip(range.offset as usize)
            .take(range.size as usize)
            .map(|&p| self.participant(ci.address().ip(), &session, p))
            .collect();
        Ok(GetParticipantsResponse {
            participants: QList(participants),
        })
    }

    fn register_urls(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RegisterUrLsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RegisterUrLsResponse, Error> {
        let pid = player(ci)?;
        let observed = format!("prudp:/address={};port={}", ci.address().ip(), ci.address().port())
            .parse()
            .map_err(|_| Error::InternalError)?;
        self.sessions.set_urls(pid, observed, request.station_urls.0);
        Ok(RegisterUrLsResponse)
    }

    fn join_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: JoinSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<JoinSessionResponse, Error> {
        let pid = player(ci)?;
        let k = key(&request.game_session_key);
        self.sessions.join(pid, k).map_err(error)?;
        info!(logger, "{pid:#x} joined session {k:?}");
        if let Some(session) = self.sessions.get(k) {
            connections::joined(logger, pid, k, session.host);
        }
        Ok(JoinSessionResponse)
    }

    fn abandon_session(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AbandonSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<AbandonSessionResponse, Error> {
        let pid = player(ci)?;
        connections::ended(logger, pid, key(&request.game_session_key), connections::End::Left);
        self.sessions.remove(key(&request.game_session_key), &[pid]).map_err(error)?;
        Ok(AbandonSessionResponse)
    }

    fn search_sessions_with_participants(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SearchSessionsWithParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SearchSessionsWithParticipantsResponse, Error> {
        // Only the caller's own sessions: answering for any PID would hand out other players'
        // session keys, and with a key anyone can join and see the members. Friends' sessions
        // can follow once there are friends.
        let pid = player(ci)?;
        let asked: Vec<u32> = request.participant_ids.0.iter().copied().filter(|&p| p == pid).collect();
        let found = if asked.is_empty() {
            vec![]
        } else {
            self.sessions.with_participants(request.game_session_type_id, &asked)
        };
        Ok(SearchSessionsWithParticipantsResponse {
            search_results: QList(
                found
                    .into_iter()
                    .take(MAX_RESULTS)
                    .map(|(k, s)| GameSessionSearchWithParticipantsResult {
                        participant_ids: QList(s.participants.clone()),
                        game_session_search_result: self.result(ci.address().ip(), (k, s)),
                    })
                    .collect(),
            ),
        })
    }

    fn get_sessions(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetSessionsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetSessionsResponse, Error> {
        player(ci)?;
        let found = request
            .game_session_keys
            .0
            .iter()
            .take(MAX_RESULTS)
            .filter_map(|k| self.sessions.get(key(k)).map(|s| self.result(ci.address().ip(), (key(k), s))))
            .collect();
        Ok(GetSessionsResponse { search_results: QList(found) })
    }

    fn get_participants_urls(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetParticipantsUrLsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetParticipantsUrLsResponse, Error> {
        let pid = player(ci)?;
        let session = self.member_of(pid, key(&request.game_session_key))?;
        let participants = request
            .participant_ids
            .0
            .iter()
            .filter(|p| session.participants.contains(p))
            .take(MAX_RESULTS)
            .map(|&p| self.participant(ci.address().ip(), &session, p))
            .collect();
        Ok(GetParticipantsUrLsResponse {
            participants: QList(participants),
        })
    }
}

/// `GameSession.RegisterURLs`, read here rather than by the generated code: the game sends
/// entries that aren't station URLs as `quazal` reads them, and a parse failure made it abandon
/// the session it had just made. Each entry is logged; those that parse are kept.
const REGISTER_URLS: u32 = 21;
/// URLs read from one call at most.
const MAX_REGISTERED: u32 = 16;

struct GameSessionProtocol<CI> {
    generated: Box<dyn Protocol<CI>>,
    sessions: Arc<Sessions>,
}

impl<CI> Protocol<CI> for GameSessionProtocol<CI> {
    fn id(&self) -> u16 {
        self.generated.id()
    }

    fn name(&self) -> String {
        self.generated.name()
    }

    fn num_methods(&self) -> u32 {
        self.generated.num_methods()
    }

    fn handle(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: &quazal::rmc::Request,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, Error> {
        if request.method_id != REGISTER_URLS {
            return self.generated.handle(logger, ctx, ci, request, client_registry, socket);
        }
        let pid = player(ci)?;
        let mut stream = quazal::rmc::basic::ReadStream::from_bytes(&request.parameters);
        let count: u32 = stream.read().map_err(|_| Error::ParsingError)?;
        let mut urls = Vec::new();
        for _ in 0..count.min(MAX_REGISTERED) {
            let text: String = stream.read().map_err(|_| Error::ParsingError)?;
            match text.parse::<quazal::rmc::types::StationURL>() {
                Ok(url) => urls.push(url),
                Err(e) => info!(logger, "{pid:#x} registered a URL that isn't one ({e}): {text:?}"),
            }
        }
        info!(logger, "{pid:#x} registered URLs {urls:?}");
        let observed = format!("prudp:/address={};port={}", ci.address().ip(), ci.address().port())
            .parse()
            .map_err(|_| Error::InternalError)?;
        self.sessions.set_urls(pid, observed, urls);
        // Whether other players can reach it where it announces itself.
        crate::announce::check(logger, pid, false);
        Ok(Vec::new())
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        self.generated.method_name(method_id)
    }
}

pub fn new_protocol<CI: 'static>(sessions: Arc<Sessions>, accounts: Arc<Accounts>, friends: Arc<Friends>, packs: Arc<Packs>) -> Box<dyn Protocol<CI>> {
    Box::new(GameSessionProtocol {
        generated: Box::new(GameSessionProtocolServer::new(GameSessions {
            sessions: Arc::clone(&sessions),
            accounts,
            friends,
            packs,
            invitations: Invitations::default(),
        })),
        sessions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search by a player who can't punch through: lobbies whose host can't be reached are held
    /// back. What's asked about hosts looks at the sessions itself, as `announce::open` does:
    /// asked while the search held them, this would never end.
    #[test]
    fn a_strict_search_holds_back_unreachable_hosts_without_holding_the_sessions() {
        let sessions = Sessions::default();
        let a = sessions.create(10, 1, &[(3, 12)]).unwrap();
        let b = sessions.create(11, 1, &[(3, 12)]).unwrap();
        let open = |host: u32| {
            let _ = sessions.seen(host);
            Some(host == 11)
        };
        let (found, held) = offered(&sessions, 1, &[], true, |_| true, open);
        assert_eq!(found.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![b]);
        assert_eq!(held, 1);
        let (found, held) = offered(&sessions, 1, &[], false, |_| true, open);
        assert_eq!((found.len(), held), (2, 0), "everyone else: as before");
        assert!(found.iter().any(|(k, _)| *k == a));
    }

    #[test]
    fn a_player_who_cant_punch_through_isnt_sent_to_a_host_that_cant_be_reached() {
        assert!(!offered_to_strict(Some(false)));
        assert!(offered_to_strict(Some(true)));
        assert!(offered_to_strict(None), "not checked: offered");
    }
}
