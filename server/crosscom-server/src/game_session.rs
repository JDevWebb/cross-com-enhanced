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

    fn participant(&self, pid: u32) -> GameSessionParticipant {
        GameSessionParticipant {
            pid,
            name: self.accounts.name(pid).ok().flatten().unwrap_or_default(),
            station_urls: QList(self.sessions.urls(pid)),
        }
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
        player(ci)?;
        let session = self.sessions.get(key(&request.game_session_key)).ok_or(Error::AccessDenied)?;
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
        player(ci)?;
        let session = self.sessions.get(key(&request.game_session_key)).ok_or(Error::AccessDenied)?;
        let range = &request.result_range;
        let participants = session
            .participants
            .iter()
            .skip(range.offset as usize)
            .take(range.size as usize)
            .map(|&p| self.participant(p))
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
        self.sessions.set_urls(pid, request.station_urls.0);
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
        player(ci)?;
        let found = self.sessions.with_participants(request.game_session_type_id, &request.participant_ids.0);
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
        player(ci)?;
        let session = self.sessions.get(key(&request.game_session_key)).ok_or(Error::AccessDenied)?;
        let participants = request
            .participant_ids
            .0
            .iter()
            .filter(|p| session.participants.contains(p))
            .take(MAX_RESULTS)
            .map(|&p| self.participant(p))
            .collect();
        Ok(GetParticipantsUrLsResponse {
            participants: QList(participants),
        })
    }
}

pub fn new_protocol<CI: 'static>(sessions: Arc<Sessions>, accounts: Arc<Accounts>) -> Box<dyn Protocol<CI>> {
    Box::new(GameSessionProtocolServer::new(GameSessions { sessions, accounts }))
}
