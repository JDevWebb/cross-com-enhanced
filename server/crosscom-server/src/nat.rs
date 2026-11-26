//! NAT traversal (protocol 3): helping two players' games reach each other.
//!
//! A game that wants to reach other players asks the server to have each of them probe it:
//! RequestProbeInitiation (1) with their station URLs, or RequestProbeInitiationExt (3) with
//! the station to probe as well. The server calls InitiateProbe (2) on each of those players'
//! games, and their packets open the way through both players' routers.
//!
//! A probe makes another player's game send to an address, so the server only asks it of
//! players who joined a session with the caller, host a match a search may offer (a player
//! finds one and probes its host before joining), or host a room of the caller's squad (a
//! squadmate looking at its party, `clans.rs`), a few at a time, and only towards the caller:
//! the public address the server sees it at, or a private one when both players are behind
//! that same public address (on one network).

use std::net::IpAddr;
use std::sync::Arc;

use grfs_protocols::nat_traversal::nat_traversal_protocol::InitiateProbeRequest;
use grfs_protocols::nat_traversal::nat_traversal_protocol::NatTraversalProtocolMethod;
use grfs_protocols::nat_traversal::nat_traversal_protocol::NatTraversalProtocolServer;
use grfs_protocols::nat_traversal::nat_traversal_protocol::NatTraversalProtocolServerTrait;
use grfs_protocols::nat_traversal::nat_traversal_protocol::RequestProbeInitiationExtRequest;
use grfs_protocols::nat_traversal::nat_traversal_protocol::RequestProbeInitiationExtResponse;
use grfs_protocols::nat_traversal::nat_traversal_protocol::RequestProbeInitiationRequest;
use grfs_protocols::nat_traversal::nat_traversal_protocol::RequestProbeInitiationResponse;
use grfs_protocols::nat_traversal::nat_traversal_protocol::NAT_TRAVERSAL_PROTOCOL_ID;
use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::ToStream;
use quazal::rmc::types::StationURL;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::ClientInfo;
use quazal::Context;
use slog::info;
use slog::warn;
use slog::Logger;

use crate::sessions::Sessions;
use crate::ticket::player;

/// The most players one request may have probe the caller.
const MAX_TARGETS: usize = 8;
/// The most of the caller's own stations a plain request has probed.
const MAX_STATIONS: usize = 2;

struct NatTraversal {
    sessions: Arc<Sessions>,
    squads: Arc<crate::clans::Store>,
    rooms: Arc<crate::clans::Rooms>,
}

impl NatTraversal {
    /// Whether `host` hosts a room of `caller`'s squad (a squadmate looking at its party).
    fn hosts_squad_room_for(&self, host: u32, caller: u32) -> bool {
        self.rooms.hosted_by(host).is_some_and(|clan| self.squads.clan_of(caller).is_ok_and(|mine| mine == clan))
    }
}

/// The caller's stations a plain request has probed: those it registered at the address the
/// server sees it at (the game learns its public address and port for its peer-to-peer
/// socket), else the address and port the server sees.
fn own_stations(registered: &[StationURL], observed: &StationURL) -> Vec<StationURL> {
    let mut stations: Vec<StationURL> = Vec::new();
    for url in registered.iter().filter(|u| u.address == observed.address) {
        if stations.len() < MAX_STATIONS && !stations.iter().any(|s| s.address == url.address && s.port == url.port) {
            stations.push(url.clone());
        }
    }
    if stations.is_empty() {
        stations.push(observed.clone());
    }
    stations
}

/// Whether a target seen at `target` may be asked to probe `station` for a caller seen at
/// `caller`: the caller's public address, or a private one when both are behind it.
fn may_probe(station: &StationURL, caller: IpAddr, target: IpAddr) -> bool {
    match station.address.parse::<IpAddr>() {
        Ok(ip) if ip == caller => true,
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            let private = ip.is_private() || (a == 100 && (64..128).contains(&b));
            private && caller == target
        }
        _ => false,
    }
}

impl NatTraversal {
    /// Has each target probe each of `stations`, when the server may ask it of them.
    #[allow(clippy::too_many_arguments)]
    fn probe<CI>(
        &self,
        logger: &Logger,
        ctx: &Context,
        caller: u32,
        seen: IpAddr,
        targets: &[StationURL],
        stations: &[StationURL],
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<(), Error> {
        if targets.len() > MAX_TARGETS {
            return Err(Error::AccessDenied);
        }
        // Asking to reach a host whose session a search offered: the caller chose it. Noted for
        // every target first, as the game asks for the session's members and its host at once.
        for url in targets {
            let target = url
                .params
                .get("RVCID")
                .and_then(|c| c.parse::<quazal::ConnectionID>().ok())
                .and_then(|c| client_registry.client_by_connection_id(c));
            if let Some(other) = target.and_then(|t| t.try_borrow().ok().and_then(|t| t.user_id)).filter(|&p| p != caller) {
                if let Some(chosen) = self.sessions.choose(caller, other) {
                    crate::connections::attempted(logger, caller, chosen, other);
                }
            }
        }
        for url in targets {
            let Some(connection_id) = url.params.get("RVCID").and_then(|c| c.parse::<quazal::ConnectionID>().ok()) else {
                warn!(logger, "Not probing {url}: it has no RVCID");
                continue;
            };
            let Some(target) = client_registry.client_by_connection_id(connection_id) else {
                warn!(logger, "Not probing {url}: no one is connected with that RVCID");
                continue;
            };
            // The caller's own connection is borrowed already.
            let Ok(mut target) = target.try_borrow_mut() else {
                continue;
            };
            let Some(other) = target.user_id.filter(|&p| p != caller) else {
                continue;
            };
            if !self.sessions.share(caller, other) && !self.sessions.hosts_public(other) && !self.hosts_squad_room_for(other, caller) {
                warn!(
                    logger,
                    "Not probing {other:#x} for {caller:#x}: not in a session together, nor hosting a public one or a room of its squad"
                );
                continue;
            }
            let address = *target.address();
            let target_seen = crate::here::seen(address).ip();
            for station in stations.iter().filter(|s| may_probe(s, seen, target_seen)) {
                let parameters = InitiateProbeRequest {
                    url_station_to_probe: station.clone(),
                }
                .to_bytes();
                info!(logger, "{other:#x} probes {station} for {caller:#x}");
                // As the server's own call, numbered and signed as its answers are: a packet made
                // as a client's request is dropped by the game.
                let method = NatTraversalProtocolMethod::InitiateProbe as u32;
                if let Err(e) = quazal::rmc::call_client(logger, ctx, socket, &mut target, NAT_TRAVERSAL_PROTOCOL_ID, method, parameters, true) {
                    warn!(logger, "Sending a probe request to {address} failed: {e}");
                }
            }
        }
        Ok(())
    }
}

impl<CI> NatTraversalProtocolServerTrait<CI> for NatTraversal {
    fn request_probe_initiation(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RequestProbeInitiationRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<RequestProbeInitiationResponse, Error> {
        let pid = player(ci)?;
        crate::connections::probed(pid);
        let at = crate::here::seen(*ci.address());
        let observed: StationURL = format!("prudp:/address={};port={}", at.ip(), at.port()).parse().map_err(|_| Error::InternalError)?;
        let stations = own_stations(&self.sessions.registered(pid), &observed);
        self.probe(logger, ctx, pid, at.ip(), &request.url_target_list.0, &stations, client_registry, socket)?;
        Ok(RequestProbeInitiationResponse)
    }

    fn request_probe_initiation_ext(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: RequestProbeInitiationExtRequest,
        client_registry: &ClientRegistry<CI>,
        socket: &std::net::UdpSocket,
    ) -> Result<RequestProbeInitiationExtResponse, Error> {
        let pid = player(ci)?;
        crate::connections::probed(pid);
        let seen: IpAddr = crate::here::seen(*ci.address()).ip();
        let station = request.url_station_to_probe;
        if !may_probe(&station, seen, seen) {
            warn!(logger, "{pid:#x} asked for probes to {station}, not an address of its own; refused");
            return Err(Error::AccessDenied);
        }
        self.probe(logger, ctx, pid, seen, &request.url_target_list.0, &[station], client_registry, socket)?;
        Ok(RequestProbeInitiationExtResponse)
    }
}

pub fn new_protocol<CI: 'static>(sessions: Arc<Sessions>, squads: Arc<crate::clans::Store>, rooms: Arc<crate::clans::Rooms>) -> Box<dyn Protocol<CI>> {
    Box::new(NatTraversalProtocolServer::new(NatTraversal { sessions, squads, rooms }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> StationURL {
        s.parse().unwrap()
    }

    #[test]
    fn a_plain_request_has_the_callers_public_stations_probed() {
        let observed = url("prudp:/address=203.0.113.7;port=19259");
        // As kept: what the game registered that may be passed on.
        let registered = vec![
            url("udp:/address=203.0.113.7;port=22014;PID=1048577;RVCID=1"),
            url("udp:/address=192.168.1.110;port=2346;PID=1048577;RVCID=1"),
            url("udp:/address=203.0.113.7;port=22014;PID=1048577;RVCID=1"),
        ];
        let stations = own_stations(&registered, &observed);
        assert_eq!(stations.len(), 1);
        assert_eq!((stations[0].address.as_str(), stations[0].port), ("203.0.113.7", 22014));
        let none = own_stations(&[], &observed);
        assert_eq!(none.iter().map(ToString::to_string).collect::<Vec<_>>(), vec![observed.to_string()]);
    }
    #[test]
    fn probes_go_only_to_the_callers_public_address_or_its_shared_network() {
        let caller: IpAddr = "203.0.113.7".parse().unwrap();
        let elsewhere: IpAddr = "198.51.100.9".parse().unwrap();
        let public = url("udp:/address=203.0.113.7;port=22014");
        let lan = url("udp:/address=192.168.1.110;port=2346");
        assert!(may_probe(&public, caller, elsewhere));
        assert!(may_probe(&lan, caller, caller), "two players behind one address");
        assert!(!may_probe(&lan, caller, elsewhere), "not into another network");
        assert!(!may_probe(&url("udp:/address=198.51.100.200;port=1"), caller, elsewhere), "nor someone else's address");
        assert!(!may_probe(&url("udp:/address=127.0.0.1;port=1"), caller, caller));
    }
}
