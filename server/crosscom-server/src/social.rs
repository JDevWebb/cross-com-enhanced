//! Friends (protocol 20), UbiFriends (47), SearchFriends (86) and FriendMessagesExtension (87):
//! the game's friends list, friend requests with their messages, blocking, and finding players
//! by name. All of them work on the same relationships (see `friends`).
//!
//! The game builds its list from Friends.GetRelationships, then UbiFriends.GetRelationshipsList,
//! then the blocked players (Friends.GetDetailedList(3)); the Ubisoft list is left empty so
//! nobody shows twice. A player asked to be friends, or whose request was accepted, is told at
//! once if they're connected.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use grfs_protocols::friend_messages_extension_service::friend_messages_extension_service_protocol as messages;
use grfs_protocols::friend_messages_extension_service::types::FriendInvitationMessage;
use grfs_protocols::friends_service::friends_protocol::*;
use grfs_protocols::friends_service::types::FriendData;
use grfs_protocols::friends_service::types::RelationshipData;
use grfs_protocols::search_friends_service::search_friends_protocol as search;
use grfs_protocols::search_friends_service::types::FoundFriend;
use grfs_protocols::ubi_friends_service::ubi_friends_protocol as ubi;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::QList;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::info;
use slog::Logger;

use crate::accounts::Accounts;
use crate::friends;
use crate::friends::Friends;
use crate::friends::Requested;
use crate::notify;
use crate::ticket::player;

/// The most players a name search finds, and the shortest search.
const MAX_FOUND: u32 = 20;
const MIN_SEARCH_LEN: usize = 2;

/// How often one player may tell another of a friend request, and how many players they may tell
/// in that time: asking, being declined and asking again mustn't be a way to pester someone.
const NOTIFY_EVERY: Duration = Duration::from_secs(10 * 60);
const MAX_TOLD: usize = 20;
/// Pairs remembered at once.
const MAX_PAIRS: usize = 50_000;

#[derive(Clone)]
struct Social {
    friends: Arc<Friends>,
    accounts: Arc<Accounts>,
    told: Arc<Mutex<HashMap<(u32, u32), Instant>>>,
}

/// Whether `from` may tell `to` of a request now (and notes it if so).
fn may_tell(told: &mut HashMap<(u32, u32), Instant>, from: u32, to: u32, now: Instant) -> bool {
    told.retain(|_, at| now.duration_since(*at) < NOTIFY_EVERY);
    if told.contains_key(&(from, to)) || told.keys().filter(|(f, _)| *f == from).count() >= MAX_TOLD || told.len() >= MAX_PAIRS {
        return false;
    }
    told.insert((from, to), now);
    true
}

fn error(e: friends::Error) -> Error {
    match e {
        friends::Error::Db => Error::InternalError,
        _ => Error::AccessDenied,
    }
}

impl Social {
    fn name(&self, pid: u32) -> String {
        self.accounts.name(pid).ok().flatten().unwrap_or_default()
    }

    /// A player's PID by name, if there's such a player.
    fn by_name(&self, name: &str) -> Result<u32, Error> {
        self.accounts.pid(name.trim()).map_err(|_| Error::InternalError)?.ok_or(Error::AccessDenied)
    }

    fn exists(&self, pid: u32) -> Result<(), Error> {
        self.accounts.name(pid).map_err(|_| Error::InternalError)?.map(|_| ()).ok_or(Error::AccessDenied)
    }

    /// `me` asks `other` to be friends, with `message`; tells `other`, or both on a match.
    #[allow(clippy::too_many_arguments)]
    fn request<CI>(&self, logger: &Logger, ctx: &Context, socket: &std::net::UdpSocket, registry: &ClientRegistry<CI>, me: u32, other: u32, message: &str) -> Result<(), Error> {
        self.exists(other)?;
        let outcome = self.friends.request(me, other).map_err(error)?;
        info!(logger, "{me:#x} asked {other:#x} to be friends: {outcome:?}");
        match outcome {
            Requested::Sent => {
                if !message.is_empty() {
                    self.friends.set_message(me, other, message).map_err(error)?;
                }
                let told = &mut self.told.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if may_tell(told, me, other, Instant::now()) {
                    notify::send(logger, ctx, socket, registry, other, &notify::friend_request(me, &self.name(me)));
                }
            }
            Requested::Dropped => {
                if !message.is_empty() {
                    self.friends.set_message(me, other, message).map_err(error)?;
                }
            }
            Requested::Friends => {
                notify::send(logger, ctx, socket, registry, other, &notify::now_friends(me, &self.name(me)));
            }
            Requested::Already => {}
        }
        Ok(())
    }

    fn accept<CI>(&self, logger: &Logger, ctx: &Context, socket: &std::net::UdpSocket, registry: &ClientRegistry<CI>, me: u32, other: u32) -> Result<bool, Error> {
        let accepted = self.friends.accept(me, other).map_err(error)?;
        if accepted {
            info!(logger, "{me:#x} and {other:#x} are now friends");
            notify::send(logger, ctx, socket, registry, other, &notify::now_friends(me, &self.name(me)));
        }
        Ok(accepted)
    }

    fn relationship<CI>(&self, registry: &ClientRegistry<CI>, pid: u32, relationship: u8) -> RelationshipData {
        RelationshipData {
            pid,
            str_name: self.name(pid),
            by_relationship: relationship,
            ui_details: 0,
            by_status: u8::from(relationship == friends::FRIEND && registry.client_by_user_id(pid).is_some()),
        }
    }
}

impl<CI> FriendsProtocolServerTrait<CI> for Social {
    fn add_friend(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AddFriendRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<AddFriendResponse, Error> {
        let me = player(ci)?;
        self.request(logger, ctx, socket, client_registry, me, request.ui_player, &request.str_message)?;
        Ok(AddFriendResponse { return_value: true })
    }

    fn add_friend_by_name(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AddFriendByNameRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<AddFriendByNameResponse, Error> {
        let me = player(ci)?;
        let other = self.by_name(&request.str_player_name)?;
        self.request(logger, ctx, socket, client_registry, me, other, &request.str_message)?;
        Ok(AddFriendByNameResponse { return_value: true })
    }

    fn add_friend_with_details(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AddFriendWithDetailsRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<AddFriendWithDetailsResponse, Error> {
        let me = player(ci)?;
        self.request(logger, ctx, socket, client_registry, me, request.ui_player, &request.str_message)?;
        let relationship = if self.friends.are_friends(me, request.ui_player) {
            friends::FRIEND
        } else {
            friends::REQUEST
        };
        Ok(AddFriendWithDetailsResponse {
            relationship_data: self.relationship(client_registry, request.ui_player, relationship),
        })
    }

    fn add_friend_by_name_with_details(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AddFriendByNameWithDetailsRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<AddFriendByNameWithDetailsResponse, Error> {
        let me = player(ci)?;
        let other = self.by_name(&request.str_player_name)?;
        self.request(logger, ctx, socket, client_registry, me, other, &request.str_message)?;
        let relationship = if self.friends.are_friends(me, other) { friends::FRIEND } else { friends::REQUEST };
        Ok(AddFriendByNameWithDetailsResponse {
            relationship_data: self.relationship(client_registry, other, relationship),
        })
    }

    fn accept_friendship(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: AcceptFriendshipRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<AcceptFriendshipResponse, Error> {
        let me = player(ci)?;
        Ok(AcceptFriendshipResponse {
            return_value: self.accept(logger, ctx, socket, client_registry, me, request.ui_player)?,
        })
    }

    fn decline_friendship(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: DeclineFriendshipRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<DeclineFriendshipResponse, Error> {
        let me = player(ci)?;
        Ok(DeclineFriendshipResponse {
            return_value: self.friends.decline(me, request.ui_player).map_err(error)?,
        })
    }

    fn black_list(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: BlackListRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<BlackListResponse, Error> {
        let me = player(ci)?;
        self.exists(request.ui_player)?;
        self.friends.block(me, request.ui_player).map_err(error)?;
        Ok(BlackListResponse { return_value: true })
    }

    fn black_list_by_name(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: BlackListByNameRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<BlackListByNameResponse, Error> {
        let me = player(ci)?;
        let other = self.by_name(&request.str_player_name)?;
        self.friends.block(me, other).map_err(error)?;
        Ok(BlackListByNameResponse { return_value: true })
    }

    fn clear_relationship(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ClearRelationshipRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ClearRelationshipResponse, Error> {
        let me = player(ci)?;
        Ok(ClearRelationshipResponse {
            return_value: self.friends.clear(me, request.ui_player).map_err(error)?,
        })
    }

    fn update_details(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: UpdateDetailsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<UpdateDetailsResponse, Error> {
        player(ci)?;
        Ok(UpdateDetailsResponse { return_value: true })
    }

    fn get_list(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetListRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetListResponse, Error> {
        let me = player(ci)?;
        Ok(GetListResponse {
            lst_friends_list: self.friends.list(me, request.by_relationship, request.b_reversed).map_err(error)?,
        })
    }

    fn get_detailed_list(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetDetailedListRequest,
        client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetDetailedListResponse, Error> {
        let me = player(ci)?;
        let pids = self.friends.list(me, request.by_relationship, request.b_reversed).map_err(error)?;
        Ok(GetDetailedListResponse {
            lst_friends_list: pids
                .into_iter()
                .map(|pid| {
                    let r = self.relationship(client_registry, pid, request.by_relationship);
                    FriendData {
                        pid,
                        str_name: r.str_name,
                        by_relationship: r.by_relationship,
                        ui_details: 0,
                        str_status: String::new(),
                    }
                })
                .collect(),
        })
    }

    fn get_relationships(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: GetRelationshipsRequest,
        client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetRelationshipsResponse, Error> {
        let me = player(ci)?;
        let all = self.friends.relationships(me).map_err(error)?;
        let range = request.result_range;
        let page = all
            .iter()
            .skip(range.offset as usize)
            .take(range.size as usize)
            .map(|&(pid, relationship)| self.relationship(client_registry, pid, relationship))
            .collect();
        Ok(GetRelationshipsResponse {
            ui_total_count: u32::try_from(all.len()).unwrap_or(u32::MAX),
            lst_relationships_list: page,
        })
    }
}

impl<CI> ubi::UbiFriendsProtocolServerTrait<CI> for Social {
    fn request_friendship(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ubi::RequestFriendshipRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<ubi::RequestFriendshipResponse, Error> {
        let me = player(ci)?;
        self.request(logger, ctx, socket, client_registry, me, request.to.principal_id, "")?;
        Ok(ubi::RequestFriendshipResponse)
    }

    fn accept_friendship(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ubi::AcceptFriendshipRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<ubi::AcceptFriendshipResponse, Error> {
        let me = player(ci)?;
        self.accept(logger, ctx, socket, client_registry, me, request.to.principal_id)?;
        Ok(ubi::AcceptFriendshipResponse)
    }

    fn clear_relationship(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ubi::ClearRelationshipRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ubi::ClearRelationshipResponse, Error> {
        let me = player(ci)?;
        self.friends.clear(me, request.to.principal_id).map_err(error)?;
        Ok(ubi::ClearRelationshipResponse)
    }

    fn decline_friendship(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ubi::DeclineFriendshipRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ubi::DeclineFriendshipResponse, Error> {
        let me = player(ci)?;
        self.friends.decline(me, request.to.principal_id).map_err(error)?;
        Ok(ubi::DeclineFriendshipResponse)
    }

    fn add_to_blacklist(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ubi::AddToBlacklistRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ubi::AddToBlacklistResponse, Error> {
        let me = player(ci)?;
        self.exists(request.to.principal_id)?;
        self.friends.block(me, request.to.principal_id).map_err(error)?;
        Ok(ubi::AddToBlacklistResponse)
    }

    fn remove_from_blacklist(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: ubi::RemoveFromBlacklistRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ubi::RemoveFromBlacklistResponse, Error> {
        let me = player(ci)?;
        self.friends.unblock(me, request.to.principal_id).map_err(error)?;
        Ok(ubi::RemoveFromBlacklistResponse)
    }

    /// Empty: the game adds this list to Friends.GetRelationships', which already has everyone.
    fn get_relationships_list(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: ubi::GetRelationshipsListRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<ubi::GetRelationshipsListResponse, Error> {
        player(ci)?;
        Ok(ubi::GetRelationshipsListResponse {
            relationships_list: QList(vec![]),
        })
    }
}

impl<CI> search::SearchFriendsProtocolServerTrait<CI> for Social {
    fn search_friends(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: search::SearchFriendsRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<search::SearchFriendsResponse, Error> {
        let me = player(ci)?;
        let part = request.part_of_name.trim();
        if part.chars().count() < MIN_SEARCH_LEN {
            return Ok(search::SearchFriendsResponse { found_list: vec![] });
        }
        let found = self.accounts.search(part, MAX_FOUND + 1).map_err(|_| Error::InternalError)?;
        Ok(search::SearchFriendsResponse {
            found_list: found
                .into_iter()
                .filter(|&(pid, _)| pid != me)
                .take(MAX_FOUND as usize)
                .map(|(pid, str_name)| FoundFriend { pid, str_name })
                .collect(),
        })
    }
}

impl<CI> messages::FriendMessagesExtensionServiceProtocolServerTrait<CI> for Social {
    fn send_friend_invitation_message(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: messages::SendFriendInvitationMessageRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<messages::SendFriendInvitationMessageResponse, Error> {
        let me = player(ci)?;
        // Kept only with an open request from the sender; otherwise quietly dropped, so a
        // block isn't told.
        self.friends.set_message(me, request.receipt_pid, &request.message_body).map_err(error)?;
        Ok(messages::SendFriendInvitationMessageResponse)
    }

    fn receive_friend_invitation_messages(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: messages::ReceiveFriendInvitationMessagesRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<messages::ReceiveFriendInvitationMessagesResponse, Error> {
        let me = player(ci)?;
        Ok(messages::ReceiveFriendInvitationMessagesResponse {
            messages: self
                .friends
                .messages(me)
                .map_err(error)?
                .into_iter()
                .map(|(sender_pid, message)| FriendInvitationMessage { sender_pid, message })
                .collect(),
        })
    }

    fn remove_friend_invitation_message(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: messages::RemoveFriendInvitationMessageRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<messages::RemoveFriendInvitationMessageResponse, Error> {
        let me = player(ci)?;
        self.friends.remove_message(me, request.sender_pid).map_err(error)?;
        Ok(messages::RemoveFriendInvitationMessageResponse)
    }
}

pub fn protocols<CI: 'static>(friends: Arc<Friends>, accounts: Arc<Accounts>) -> Vec<Box<dyn Protocol<CI>>> {
    let social = Social {
        friends,
        accounts,
        told: Arc::default(),
    };
    vec![
        Box::new(FriendsProtocolServer::new(social.clone())),
        Box::new(ubi::UbiFriendsProtocolServer::new(social.clone())),
        Box::new(search::SearchFriendsProtocolServer::new(social.clone())),
        Box::new(messages::FriendMessagesExtensionServiceProtocolServer::new(social)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_is_told_of_a_request_once_in_a_while() {
        let mut told = HashMap::new();
        let now = Instant::now();
        assert!(may_tell(&mut told, 1, 2, now));
        assert!(!may_tell(&mut told, 1, 2, now + Duration::from_secs(60)), "asked again soon after a decline");
        assert!(may_tell(&mut told, 1, 2, now + NOTIFY_EVERY));
        for to in 10..10 + MAX_TOLD as u32 {
            may_tell(&mut told, 5, to, now);
        }
        assert!(!may_tell(&mut told, 5, 999, now), "too many players at once");
    }
}
