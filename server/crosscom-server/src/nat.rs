//! NAT traversal (protocol 3): helping two players' games reach each other.
//!
//! A game that wants to reach other players asks the server to have each of them probe it:
//! RequestProbeInitiation (1) with their station URLs, or RequestProbeInitiationExt (3) with
//! the station to probe as well. The server calls InitiateProbe (2) on each of those players'
//! games, and their packets open the way through both players' routers.
//!
//! A probe makes another player's game send to an address, so the server only asks it of
//! players who joined a session with the caller, or host a match a search may offer (a player
//! finds one and probes its host before joining), a few at a time, and only towards the caller:
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
use quazal::prudp::packet::PacketType;
use quazal::prudp::packet::QPacket;
use quazal::prudp::packet::StreamType;
use quazal::prudp::packet::VPort;
use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::ToStream;
use quazal::rmc::types::StationURL;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::rmc::Request;
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
}

/// The caller's stations a plain request has probed: those it registered at the address the
/// server sees it at (the game learns its public address and port for its peer-to-peer
/// socket), else the address and port the server sees.
fn own_stations(registered: &[StationURL], observed: &StationURL) -> Vec<StationURL> {
    let mut stations: Vec<StationURL> = Vec::new();
    for url in registered.iter().skip(1).filter(|u| u.address == observed.address) {
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
            if !self.sessions.share(caller, other) && !self.sessions.hosts_public(other) {
                warn!(logger, "Not probing {other:#x} for {caller:#x}: not in a session together, nor hosting a public one");
                continue;
            }
            let address = *target.address();
            for station in stations.iter().filter(|s| may_probe(s, seen, address.ip())) {
                let payload = Request {
                    protocol_id: NAT_TRAVERSAL_PROTOCOL_ID,
                    call_id: rand::random(),
                    method_id: NatTraversalProtocolMethod::InitiateProbe as u32,
                    parameters: InitiateProbeRequest {
                        url_station_to_probe: station.clone(),
                    }
                    .to_bytes(),
                }
                .to_bytes();
                let packet = QPacket {
                    source: VPort {
                        port: 1,
                        stream_type: StreamType::RVSec,
                    },
                    destination: VPort {
                        port: 15,
                        stream_type: StreamType::RVSec,
                    },
                    packet_type: PacketType::Data,
                    payload,
                    ..Default::default()
                };
                info!(logger, "{other:#x} probes {station} for {caller:#x}");
                if let Err(e) = quazal::prudp::send_request(logger, ctx, &address, socket, packet, &mut target) {
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
        let observed: StationURL = format!("prudp:/address={};port={}", ci.address().ip(), ci.address().port())
            .parse()
            .map_err(|_| Error::InternalError)?;
        let stations = own_stations(&self.sessions.urls(pid), &observed);
        self.probe(logger, ctx, pid, ci.address().ip(), &request.url_target_list.0, &stations, client_registry, socket)?;
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
        let seen: IpAddr = ci.address().ip();
        let station = request.url_station_to_probe;
        if !may_probe(&station, seen, seen) {
            warn!(logger, "{pid:#x} asked for probes to {station}, not an address of its own; refused");
            return Err(Error::AccessDenied);
        }
        self.probe(logger, ctx, pid, seen, &request.url_target_list.0, &[station], client_registry, socket)?;
        Ok(RequestProbeInitiationExtResponse)
    }
}

pub fn new_protocol<CI: 'static>(sessions: Arc<Sessions>) -> Box<dyn Protocol<CI>> {
    Box::new(NatTraversalProtocolServer::new(NatTraversal { sessions }))
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
        // As kept: the address seen first, then what the game registered that may be passed on.
        let registered = vec![
            observed.clone(),
            url("udp:/address=203.0.113.7;port=22014;PID=1048577;RVCID=1"),
            url("udp:/address=192.168.1.110;port=2346;PID=1048577;RVCID=1"),
            url("udp:/address=203.0.113.7;port=22014;PID=1048577;RVCID=1"),
        ];
        let stations = own_stations(&registered, &observed);
        assert_eq!(stations.len(), 1);
        assert_eq!((stations[0].address.as_str(), stations[0].port), ("203.0.113.7", 22014));
        let none = own_stations(std::slice::from_ref(&observed), &observed);
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
