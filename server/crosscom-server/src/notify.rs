//! Events the server pushes to a player's game: ProcessNotificationEvent (protocol 14, method 1).
//!
//! An event's type is a category times 1000 plus a subtype; the game dispatches on the category
//! (worked out from its handler): 1 friend events, whose second parameter is the relationship
//! (2 a request for friendship, 1 now friends); 7 game session events, where 7005 is an
//! invitation received (the sender, then the session's type and ID, and the message).

use grfs_protocols::protocol_foundation::notification_protocol::NOTIFICATION_PROTOCOL_ID;
use grfs_protocols::protocol_foundation::types::NotificationEvent;
use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::ToStream;
use quazal::Context;
use slog::debug;
use slog::warn;
use slog::Logger;

const PROCESS_NOTIFICATION_EVENT: u32 = 1;
const FRIEND_EVENT: u32 = 1000;
const GAME_INVITATION_RECEIVED: u32 = 7005;

/// `from` asked `to` to be friends.
#[must_use]
pub fn friend_request(from: u32, name: &str) -> NotificationEvent {
    NotificationEvent {
        pid_source: from,
        ui_type: FRIEND_EVENT,
        ui_param_1: from,
        ui_param_2: u32::from(crate::friends::REQUEST),
        str_param: name.into(),
        ui_param_3: 0,
    }
}

/// `from` and the player told are now friends.
#[must_use]
pub fn now_friends(from: u32, name: &str) -> NotificationEvent {
    NotificationEvent {
        ui_param_2: u32::from(crate::friends::FRIEND),
        ..friend_request(from, name)
    }
}

/// `from` invites the player told into a session.
#[must_use]
pub fn game_invitation(from: u32, session_type: u32, session_id: u32, message: &str) -> NotificationEvent {
    NotificationEvent {
        pid_source: from,
        ui_type: GAME_INVITATION_RECEIVED,
        ui_param_1: session_type,
        ui_param_2: session_id,
        str_param: message.into(),
        ui_param_3: 0,
    }
}

/// Sends `event` to `to`'s game if they're connected; whether it went.
pub fn send<CI>(logger: &Logger, ctx: &Context, socket: &std::net::UdpSocket, registry: &ClientRegistry<CI>, to: u32, event: &NotificationEvent) -> bool {
    let Some(client) = registry.client_by_user_id(to) else {
        debug!(logger, "{to:#x} isn't connected; event {} not sent", event.ui_type);
        return false;
    };
    let Ok(mut client) = client.try_borrow_mut() else {
        return false;
    };
    match quazal::rmc::call_client(
        logger,
        ctx,
        socket,
        &mut client,
        NOTIFICATION_PROTOCOL_ID,
        PROCESS_NOTIFICATION_EVENT,
        event.to_bytes(),
        true,
    ) {
        Ok(_) => true,
        Err(e) => {
            warn!(logger, "Couldn't send event {} to {to:#x}: {e}", event.ui_type);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_carry_what_the_game_reads() {
        let request = friend_request(0x10_0001, "Kozak");
        assert_eq!((request.ui_type / 1000, request.ui_param_1, request.ui_param_2), (1, 0x10_0001, 2));
        assert_eq!(now_friends(5, "x").ui_param_2, 1);
        let invite = game_invitation(5, 1, 0xdead_beef, "join us");
        assert_eq!((invite.ui_type / 1000, invite.ui_type % 1000), (7, 5));
        assert_eq!((invite.pid_source, invite.ui_param_1, invite.ui_param_2), (5, 1, 0xdead_beef));
        // On the wire: the sender, then the type, as the game's event object holds them.
        let bytes = invite.to_bytes();
        assert_eq!(&bytes[..8], &[5, 0, 0, 0, 0x5d, 0x1b, 0, 0]);
    }
}
