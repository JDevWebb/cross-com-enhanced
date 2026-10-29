//! Protocols the game calls that aren't among its protocol declarations (so nothing is generated
//! for them), answered with fixed replies. A call to a method without an answer is logged with
//! its parameters, which is how the next ones are worked out. The IDs, methods and answers are the ones the
//! Phoenix Network backend found the game accepts; the method names follow Ghost Recon Online's
//! and Blacklist's versions of the same protocols.

use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::ToStream;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::rmc::Request;
use quazal::ClientInfo;
use quazal::Context;
use slog::debug;
use slog::info;
use slog::Logger;

/// An empty list: just its count.
const EMPTY_LIST: [u8; 4] = [0, 0, 0, 0];

pub struct Method {
    pub id: u32,
    pub name: &'static str,
    /// The reply's parameters, already encoded.
    pub answer: fn() -> Vec<u8>,
}

pub struct FixedProtocol {
    pub id: u16,
    pub name: &'static str,
    pub methods: &'static [Method],
}

impl<CI> Protocol<CI> for FixedProtocol {
    fn id(&self) -> u16 {
        self.id
    }

    fn name(&self) -> String {
        self.name.into()
    }

    fn num_methods(&self) -> u32 {
        self.methods.iter().map(|m| m.id).max().unwrap_or(0)
    }

    fn handle(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: &Request,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, Error> {
        crate::ticket::player(ci)?;
        let Some(method) = self.methods.iter().find(|m| m.id == request.method_id) else {
            // These protocols carry game data (statistics, maps, clans), never credentials.
            let shown = &request.parameters[..request.parameters.len().min(512)];
            info!(
                logger,
                "{}.{} has no answer yet; parameters ({} bytes): {}",
                self.name,
                request.method_id,
                request.parameters.len(),
                hex(shown)
            );
            return Err(Error::UnknownMethod);
        };
        debug!(logger, "{}.{}: fixed answer", self.name, method.name);
        Ok((method.answer)())
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        self.methods.iter().find(|m| m.id == method_id).map(|m| m.name.into())
    }
}

/// Maps offered, as the world keys of their mode-specific worlds (the ones the game's DLC
/// configuration names): an experiment. With no maps the game abandons the match it was going to
/// host; whether these are the IDs it wants is to be seen.
const MAPS: [u32; 26] = [
    0x222a_c79a,
    0x222a_c991,
    0x222a_c0d6,
    0x222a_cb1f,
    0x222a_cb44,
    0x222a_cb69,
    0x222a_cb7b,
    0x222a_cb8d,
    0x222a_c535,
    0x222a_cba6, // the ten maps, Stockade
    0x3220_f4de,
    0x3220_f542,
    0x3220_f55f,
    0x3220_f67e,
    0xa033_3e99, // Peshawar Streets: Conflict, Decoy, Saboteur, Siege, Stockade
    0x3220_f9db,
    0x3220_fa0f,
    0x3220_fa2c,
    0x3220_fa72,
    0xbc32_91bc, // Peshawar Rooftops
    0x3220_fc58,
    0x3220_fcb5,
    0x3220_fce5,
    0x3220_fd02,
    0x3220_efa1, // Peshawar Residential
    0x9f2e_1b5b, // Arctic Base, Guerrilla
];

fn map_list() -> Vec<u8> {
    let mut out = (MAPS.len() as u32).to_le_bytes().to_vec();
    for map in MAPS {
        out.extend_from_slice(&map.to_le_bytes());
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every protocol answered this way.
pub fn protocols<CI: 'static>() -> Vec<Box<dyn Protocol<CI>>> {
    const PROTOCOLS: &[FixedProtocol] = &[
        FixedProtocol {
            id: 109,
            name: "RichPresenceProtocol",
            methods: &[Method {
                id: 1,
                name: "StartPresence",
                answer: Vec::new,
            }],
        },
        FixedProtocol {
            id: 110,
            name: "ClansProtocol",
            // The player's clan: none (0). With 1 the game went on to ask for that clan's details.
            methods: &[Method {
                id: 13,
                name: "GetCurUserClan",
                answer: || 0u32.to_bytes(),
            }],
        },
        FixedProtocol {
            id: 111,
            name: "MiscProtocol",
            methods: &[
                Method {
                    id: 3,
                    name: "GetGenericSettings",
                    answer: || EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 4,
                    name: "Method4",
                    answer: Vec::new,
                },
            ],
        },
        FixedProtocol {
            id: 120,
            name: "UplayWinProtocol",
            methods: &[
                Method {
                    id: 1,
                    name: "GetActions",
                    answer: || EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 5,
                    name: "GetRewards",
                    answer: || EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 7,
                    name: "UplayWelcome",
                    answer: || EMPTY_LIST.to_vec(),
                },
            ],
        },
        FixedProtocol {
            id: 121,
            name: "MapListProtocol",
            // Both answer a list<u32> (worked out from the game's encoders): the map categories,
            // and a category's maps. Empty for now, so the game keeps its own map rotation; which
            // IDs it expects is still to work out.
            methods: &[
                Method {
                    id: 1,
                    name: "GetAvailableMapCategories",
                    answer: || EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 2,
                    name: "GetMapList",
                    answer: map_list,
                },
            ],
        },
        FixedProtocol {
            id: 125,
            name: "TrackingExtensionProtocol",
            methods: &[Method {
                id: 1,
                name: "GetIsVIP",
                answer: || true.to_bytes(),
            }],
        },
        FixedProtocol {
            id: 5003,
            name: "OverlordCoreProtocol",
            // The game's settings: none, so it uses its defaults.
            methods: &[Method {
                id: 1,
                name: "GetSettings",
                answer: || EMPTY_LIST.to_vec(),
            }],
        },
    ];
    PROTOCOLS.iter().map(|p| Box::new(FixedProtocol { ..*p }) as Box<dyn Protocol<CI>>).collect()
}
