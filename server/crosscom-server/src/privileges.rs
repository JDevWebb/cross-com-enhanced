//! What a player may do and has unlocked (`PrivilegesProtocol`).
//!
//! The list is the one the official servers gave every player, as recorded by the Phoenix
//! Network Future Soldier backend: playing online, and the pre-order and promotion unlocks.

use std::collections::HashMap;

use grfs_protocols::privileges_service::privileges_protocol::*;
use grfs_protocols::privileges_service::types::Privilege;
use grfs_protocols::privileges_service::types::PrivilegeEx;
use quazal::prudp::ClientRegistry;
use quazal::rmc::types::QList;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::Logger;

/// (ID, description); every one is permanent.
const PRIVILEGES: &[(u32, &str)] = &[
    (0x0001, "Allow to play online"),
    (0x03E8, "Engineer Head Variant 1"),
    (0x03E9, "Engineer Head Variant 2"),
    (0x03EA, "Engineer Head Variant 3"),
    (0x03EB, "Engineer Head Variant 4"),
    (0x03EC, "Engineer Head Variant 5"),
    (0x03ED, "Engineer Head Variant 6"),
    (0x03EE, "Engineer Head Variant 7"),
    (0x03EF, "Engineer Head Variant 8"),
    (0x03F0, "Rifleman Head Variant 1"),
    (0x03F1, "Rifleman Head Variant 2"),
    (0x03F2, "Rifleman Head Variant 3"),
    (0x03F3, "Rifleman Head Variant 4"),
    (0x03F4, "Rifleman Head Variant 5"),
    (0x03F5, "Rifleman Head Variant 6"),
    (0x03F6, "Rifleman Head Variant 7"),
    (0x03F7, "Rifleman Head Variant 8"),
    (0x03F8, "Scout Head Variant 1"),
    (0x03F9, "Scout Head Variant 2"),
    (0x03FA, "Scout Head Variant 3"),
    (0x03FB, "Scout Head Variant 4"),
    (0x03FC, "Scout Head Variant 5"),
    (0x03FD, "Scout Head Variant 6"),
    (0x03FE, "Scout Head Variant 7"),
    (0x03FF, "Scout Head Variant 8"),
    (0x0401, "Weapon AK47"),
    (0x0404, "Weapon M40A5"),
    (0x0405, "Weapon MK14"),
    (0x0406, "Weapon MN913"),
    (0x0410, "Map MH01 Tramway"),
    (0x0422, "Tiger Stripe Camo"),
    (0x0453, "Winter Digital Camo"),
    (0x13BA, "Unlock an R6 weapon skin"),
    (0x13C4, "Unlock a Tom Clancy weapon skin"),
    (0x13CE, "Unlock a Splinter Cell weapon skin"),
    (0x13D6, "Unknown privilege"),
    // The DLC packs. The game mounts a pack only when it holds the pack's ownership privilege,
    // 9001-9003 by the pack's index; without one, a match on that pack's maps (and Stockade,
    // Arctic Strike's mode, on any map) gets its host kicked for content it hasn't downloaded.
    (9001, "DLC 1 owned: Arctic Strike"),
    (9002, "DLC 2 owned"),
    (9003, "DLC 3 owned"),
    // Each dlc.ini's TrialPrivilegeID: trials only, they don't make a pack owned.
    (3000, "DLC 1 trial: Arctic Strike"),
    (3010, "DLC 2 trial"),
    (3020, "DLC 3 trial"),
];

struct Privileges;

impl<CI> PrivilegesProtocolServerTrait<CI> for Privileges {
    fn get_privileges(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: GetPrivilegesRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetPrivilegesResponse, Error> {
        crate::ticket::player(ci)?;
        let privileges = PRIVILEGES.iter().map(|&(id, d)| (id, Privilege { id, description: d.into() })).collect::<HashMap<_, _>>();
        Ok(GetPrivilegesResponse { privileges })
    }

    fn get_expired_privileges(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: GetExpiredPrivilegesRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetExpiredPrivilegesResponse, Error> {
        crate::ticket::player(ci)?;
        Ok(GetExpiredPrivilegesResponse {
            expired_privileges: QList(vec![]),
        })
    }

    fn get_privileges_ex(
        &self,
        _logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        _request: GetPrivilegesExRequest,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<GetPrivilegesExResponse, Error> {
        crate::ticket::player(ci)?;
        let privileges_ex = PRIVILEGES
            .iter()
            .map(|&(id, d)| PrivilegeEx {
                id,
                description: d.into(),
                duration: -1,
            })
            .collect();
        Ok(GetPrivilegesExResponse {
            privileges_ex: QList(privileges_ex),
        })
    }
}

pub fn new_protocol<CI: 'static>() -> Box<dyn Protocol<CI>> {
    Box::new(PrivilegesProtocolServer::new(Privileges))
}
