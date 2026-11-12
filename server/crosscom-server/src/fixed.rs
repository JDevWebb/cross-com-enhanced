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
    /// The reply's parameters, encoded, from the request's.
    pub answer: fn(&[u8]) -> Vec<u8>,
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
        let answer = (method.answer)(&request.parameters);
        // Small requests and answers in full (a map category and its worlds, a dare request):
        // what the game asked for is how the answers are checked.
        if request.parameters.len() <= 64 && answer.len() <= 512 {
            debug!(logger, "{}.{}: fixed answer to {} is {}", self.name, method.name, hex(&request.parameters), hex(&answer));
        } else {
            debug!(logger, "{}.{}: fixed answer ({} bytes)", self.name, method.name, answer.len());
        }
        Ok(answer)
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        self.methods.iter().find(|m| m.id == method_id).map(|m| m.name.into())
    }
}

/// The time now, in Quazal's packed date-time: seconds in bits 0-5, minutes 6-11, hours 12-16,
/// day 17-21, month 22-25 and the year from bit 26. Misc.1 answers it (an 8-byte value, worked
/// out from the game's encoder); the game asks right after a match's map list.
pub(crate) fn server_time() -> u64 {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    packed_date_time(secs)
}

fn packed_date_time(unix: u64) -> u64 {
    let (days, rest) = (unix / 86_400, unix % 86_400);
    let (hour, minute, second) = (rest / 3600, rest / 60 % 60, rest % 60);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u64;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u64;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as u64;
    second | minute << 6 | hour << 12 | day << 17 | month << 22 | year << 26
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
                answer: |_| Vec::new(),
            }],
        },
        FixedProtocol {
            id: 110,
            name: "ClansProtocol",
            // The player's clan: none (0). With 1 the game went on to ask for that clan's details.
            // Method 28 (asked with a 0 as the menus load) answers a list and a u32 (from the
            // game's codec): none.
            methods: &[
                Method {
                    id: 13,
                    name: "GetCurUserClan",
                    answer: |_| 0u32.to_bytes(),
                },
                Method {
                    id: 28,
                    name: "Method28",
                    answer: |_| [EMPTY_LIST, [0; 4]].concat(),
                },
            ],
        },
        FixedProtocol {
            id: 111,
            name: "MiscProtocol",
            methods: &[
                Method {
                    id: 1,
                    name: "GetServerTime",
                    answer: |_| server_time().to_le_bytes().to_vec(),
                },
                Method {
                    id: 3,
                    name: "GetGenericSettings",
                    answer: |_| EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 4,
                    name: "Method4",
                    answer: |_| Vec::new(),
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
                    answer: |_| EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 5,
                    name: "GetRewards",
                    answer: |_| EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 7,
                    name: "UplayWelcome",
                    answer: |_| EMPTY_LIST.to_vec(),
                },
            ],
        },
        FixedProtocol {
            id: 121,
            name: "MapListProtocol",
            // Both answer a list<u32> (worked out from the game's encoders): the map categories,
            // and a category's maps (see `maps`).
            methods: &[
                Method {
                    id: 1,
                    name: "GetAvailableMapCategories",
                    answer: |_| EMPTY_LIST.to_vec(),
                },
                Method {
                    id: 2,
                    name: "GetMapList",
                    answer: crate::maps::map_list,
                },
            ],
        },
        FixedProtocol {
            id: 125,
            name: "TrackingExtensionProtocol",
            methods: &[Method {
                id: 1,
                name: "GetIsVIP",
                answer: |_| true.to_bytes(),
            }],
        },
        FixedProtocol {
            id: 5002,
            name: "OverlordNewsProtocol",
            // GetNews(predicate, result range) answers one list (from the game's call): no news.
            methods: &[Method {
                id: 1,
                name: "GetNews",
                answer: |_| EMPTY_LIST.to_vec(),
            }],
        },
        FixedProtocol {
            id: 5003,
            name: "OverlordCoreProtocol",
            // The game's settings: none, so it uses its defaults.
            methods: &[Method {
                id: 1,
                name: "GetSettings",
                answer: |_| EMPTY_LIST.to_vec(),
            }],
        },
        FixedProtocol {
            id: 5008,
            name: "OverlordDareProtocol",
            // RequestDareInfo(u32) answers a date-time and a list of dare infos, each
            // { u32, u32, date-time, u32, u32, u32 } (from the game's decoder): the server's time
            // and no dares. Left unanswered, a game with a saved profile waits on its title screen.
            methods: &[Method {
                id: 1,
                name: "RequestDareInfo",
                answer: |_| {
                    let mut reply = server_time().to_le_bytes().to_vec();
                    reply.extend_from_slice(&EMPTY_LIST);
                    reply
                },
            }],
        },
    ];
    PROTOCOLS.iter().map(|p| Box::new(FixedProtocol { ..*p }) as Box<dyn Protocol<CI>>).collect()
}

#[cfg(test)]
mod tests {
    use super::packed_date_time;

    #[test]
    fn packs_a_date_the_quazal_way() {
        // 2026-10-29 13:45:30 UTC
        let t = packed_date_time(1_793_281_530);
        assert_eq!((t >> 26, t >> 22 & 0xf, t >> 17 & 0x1f), (2026, 10, 29));
        assert_eq!((t >> 12 & 0x1f, t >> 6 & 0x3f, t & 0x3f), (13, 45, 30));
        let leap = packed_date_time(951_782_400); // 2000-02-29 00:00:00
        assert_eq!((leap >> 26, leap >> 22 & 0xf, leap >> 17 & 0x1f), (2000, 2, 29));
    }
}
