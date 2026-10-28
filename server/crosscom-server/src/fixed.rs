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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every protocol answered this way.
pub fn protocols<CI: 'static>() -> Vec<Box<dyn Protocol<CI>>> {
    const PROTOCOLS: &[FixedProtocol] = &[
        FixedProtocol {
            id: 108,
            name: "PlayerStatisticsProtocol",
            methods: &[],
        },
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
            // The player's clan; the game carried on with 1.
            methods: &[Method {
                id: 13,
                name: "GetCurUserClan",
                answer: || 1u32.to_bytes(),
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
            methods: &[],
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
