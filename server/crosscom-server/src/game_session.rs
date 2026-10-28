//! Game sessions (`GameSessionProtocol`): what the game makes when a player opens Multiplayer,
//! and finds, joins and leaves. The sessions themselves are in `sessions`.

use std::sync::Arc;

use grfs_protocols::game_session_service::game_session_protocol::*;
use grfs_protocols::game_session_service::types::GameSessionKey;
use grfs_protocols::game_session_service::types::GameSessionParticipant;
use grfs_protocols::game_session_service::types::GameSessionSearchResult;
use grfs_protocols::game_session_service::types::GameSessionSearchWithParticipantsResult;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::Property;
use quazal::rmc::types::QList;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::info;
use slog::Logger;

use crate::accounts::Accounts;
use crate::sessions;
use crate::sessions::Sessions;
use crate::ticket::player;

/// Results a search returns at most.
const MAX_RESULTS: usize = 100;

struct GameSessions {
    sessions: Arc<Sessions>,
    accounts: Arc<Accounts>,
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
    fn result(&self, (k, session): (sessions::Key, sessions::Session)) -> GameSessionSearchResult {
        GameSessionSearchResult {
            session_key: GameSessionKey { type_id: k.0, session_id: k.1 },
            host_pid: session.host,
            host_urls: QList(self.sessions.urls(session.host)),
            attributes: QList(session.attributes.iter().map(|&(id, value)| Property { id, value }).collect()),
        }
    }

    /// A participant as other members see them: name and addresses only for those who joined
    /// themselves.
    fn participant(&self, session: &sessions::Session, pid: u32) -> GameSessionParticipant {
        let joined = session.joined(pid);
        GameSessionParticipant {
            pid,
            name: if joined {
                self.accounts.name(pid).ok().flatten().unwrap_or_default()
            } else {
                String::new()
            },
            station_urls: QList(if joined { self.sessions.urls(pid) } else { vec![] }),
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
        self.sessions.update(pid, key(&update.session_key), &properties(&update.attributes)).map_err(error)?;
        Ok(UpdateSessionResponse)
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
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: MigrateSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<MigrateSessionResponse, Error> {
        // The session stays where it is; its new host takes it over.
        player(ci)?;
        Ok(MigrateSessionResponse {
            game_session_key_migrated: request.game_session_key,
        })
    }

    fn leave_session(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: LeaveSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<LeaveSessionResponse, Error> {
        let pid = player(ci)?;
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
            search_result: self.result((k, session)),
        })
    }

    fn search_sessions(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: SearchSessionsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<SearchSessionsResponse, Error> {
        let pid = player(ci)?;
        // Every session of the type but the player's own; the query's filters aren't applied yet.
        let found = self.sessions.search(request.game_session_query.type_id, MAX_RESULTS);
        Ok(SearchSessionsResponse {
            search_results: QList(found.into_iter().filter(|(_, s)| s.host != pid).map(|r| self.result(r)).collect()),
        })
    }

    fn add_participants(
        &self,
        _logger: &Logger,
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
        let pids: Vec<u32> = request.public_participant_ids.0.iter().chain(&request.private_participant_ids.0).copied().collect();
        self.sessions.add(k, &pids).map_err(error)?;
        Ok(AddParticipantsResponse)
    }

    fn remove_participants(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RemoveParticipantsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<RemoveParticipantsResponse, Error> {
        let pid = player(ci)?;
        let k = key(&request.game_session_key);
        let only_self = request.participant_ids.0.iter().all(|&p| p == pid);
        if !only_self && !self.sessions.is_host(pid, k) {
            return Err(Error::AccessDenied);
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
            .map(|&p| self.participant(&session, p))
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
        Ok(JoinSessionResponse)
    }

    fn abandon_session(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AbandonSessionRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<AbandonSessionResponse, Error> {
        let pid = player(ci)?;
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
                        game_session_search_result: self.result((k, s)),
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
            .filter_map(|k| self.sessions.get(key(k)).map(|s| self.result((key(k), s))))
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
            .map(|&p| self.participant(&session, p))
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
        Ok(Vec::new())
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        self.generated.method_name(method_id)
    }
}

pub fn new_protocol<CI: 'static>(sessions: Arc<Sessions>, accounts: Arc<Accounts>) -> Box<dyn Protocol<CI>> {
    Box::new(GameSessionProtocol {
        generated: Box::new(GameSessionProtocolServer::new(GameSessions {
            sessions: Arc::clone(&sessions),
            accounts,
        })),
        sessions,
    })
}
