//! The map list (protocol 121): which worlds a hosted match may vote between.
//!
//! Each adversarial map has one world per game mode, and the game loads the world it is given,
//! so a world key also picks the mode. The game asks GetMapList with a category: a bit per game
//! mode, a bit per DLC pack the lobby's players have, and the match size in the low bits (1 or 2).
//! The answer is every world of those modes on the base maps and those packs.
//!
//! Two numberings meet here. A world's mode is the game's mode list: 1 Stockade, 2 Conflict,
//! 3 Decoy, 4 Saboteur, 5 Siege, 6 Takeover, 7 Team Deathmatch; a session's property 116 uses
//! this one too (seen 2026-11-18: Stockade lobbies 1, Takeover 6). The category uses the menu's
//! numbering, `1 << (n + 2)`: 2 Conflict, 3 Decoy, 4 Saboteur,
//! 5 Siege, 6 Stockade, 7 Takeover, 8 Guerrilla. Seen from the game (2026-11-13): Conflict 0x10,
//! Decoy 0x20, Stockade 0x100, Takeover 0x200.
//!
//! Team Deathmatch worlds exist only for the two night maps and the game has no rules for the
//! mode: loading one hangs, so they are never offered.
//!
//! The packs, from the game itself (2026-11-16):
//! - Its pack bits: 0x4000 the base game (always there), 0x8000 Arctic Strike (`DLC\dlc1`),
//!   0x10000 Khyber Strike (`DLC\dlc3`). A player's bits are 0x4000, plus 0x8000 with pack 1
//!   mounted, plus 0x10000 with pack 3. Raven Strike (`DLC\dlc2`) has no bit: it adds co-op
//!   content only (campaign missions and the Sawmill Guerrilla map), nothing adversarial. The game
//!   drops 0x4000 from the category unless the lobby has both packs or neither, so a category
//!   with no pack bits at all also means the base game.
//! - Which pack each world is in: every world key is in exactly one of the game's archives.
//!   Stockade worlds (every map but the night maps) are Arctic Strike's; every Takeover world is
//!   Khyber Strike's; Riot, Skyline and Evicted are Arctic Strike's maps; Switchback, Palace and
//!   Transit Khyber Strike's; the two night maps are the base game's (their Takeover worlds
//!   Khyber Strike's).
//! - The game also refuses Stockade to a player without Arctic Strike and Takeover to one without
//!   Khyber Strike, on any map, so those modes need their pack even where the world is elsewhere.
//!   A world on a pack's map in another pack's mode (Takeover on Riot) needs both.

/// Game modes, as a world records them.
#[allow(dead_code)]
pub mod mode {
    pub const STOCKADE: u32 = 1;
    pub const CONFLICT: u32 = 2;
    pub const DECOY: u32 = 3;
    pub const SABOTEUR: u32 = 4;
    pub const SIEGE: u32 = 5;
    pub const TAKEOVER: u32 = 6;
    /// Not shipped: never offered (see the module's notes).
    pub const TEAM_DEATHMATCH: u32 = 7;
    /// Co-op against waves of enemies: one world per map (our own number; its worlds are only
    /// asked for by its category bit).
    pub const GUERRILLA: u32 = 8;
}

/// Map packs, as bits of the category (see the module's notes). The base maps are always offered.
const BASE: u32 = 0;
const ARCTIC: u32 = 0x8000;
const KHYBER: u32 = 0x1_0000;
/// Raven Strike has no category bit, so what needs it (only the Sawmill Guerrilla map) is never
/// offered: the game couldn't say its lobby has the pack.
const RAVEN: u32 = 0x8000_0000;
const PACKS: u32 = ARCTIC | KHYBER | RAVEN;

/// The category's mode bits and the world mode each asks for.
const MODE_BITS: &[(u32, u32)] = &[
    (1 << 4, mode::CONFLICT),
    (1 << 5, mode::DECOY),
    (1 << 6, mode::SABOTEUR),
    (1 << 7, mode::SIEGE),
    (1 << 8, mode::STOCKADE),
    (1 << 9, mode::TAKEOVER),
    (1 << 10, mode::GUERRILLA),
];
/// The bit that asks for a world mode.
fn mode_bit(world_mode: u32) -> u32 {
    MODE_BITS.iter().find(|&&(_, m)| m == world_mode).map_or(0, |&(bit, _)| bit)
}
/// The adversarial mode bits: asking for no mode means all of them.
const ADVERSARIAL: u32 = 0x3f0;
/// The pack a mode came with, needed on every map.
const fn mode_pack(world_mode: u32) -> u32 {
    match world_mode {
        mode::STOCKADE => ARCTIC,
        mode::TAKEOVER => KHYBER,
        _ => BASE,
    }
}

pub struct Map {
    /// For reading the table; the game shows its own names.
    #[allow(dead_code)]
    pub name: &'static str,
    pub pack: u32,
    /// (mode, world key)
    pub worlds: &'static [(u32, u32)],
}

/// The adversarial maps and their worlds, from the game's world list.
#[rustfmt::skip]
pub const MAPS: &[Map] = &[
    Map { name: "Pipeline", pack: BASE, worlds: &[(1, 0x222a_c79a), (2, 0xa921_1863), (3, 0xa920_bdb3), (4, 0xa920_bdb8), (5, 0xa920_bdbd), (6, 0x3220_cd6e)] },
    Map { name: "Market", pack: BASE, worlds: &[(1, 0x222a_c991), (2, 0x222a_8978), (3, 0xa920_c26e), (4, 0xa920_c273), (5, 0xa920_c278), (6, 0x3220_cd8d)] },
    Map { name: "Overpass", pack: BASE, worlds: &[(1, 0x222a_c0d6), (2, 0xa921_18e3), (3, 0xa920_c018), (4, 0xa920_c01d), (5, 0xa920_c022), (6, 0x3220_c22c)] },
    Map { name: "Harbor", pack: BASE, worlds: &[(1, 0x222a_cb1f), (2, 0xa921_192e), (3, 0xa920_be1d), (4, 0xa920_be22), (5, 0xa920_be27), (6, 0x3220_cd93)] },
    Map { name: "Cargo", pack: BASE, worlds: &[(1, 0x222a_cb44), (2, 0xa920_c9f0), (3, 0xa920_bec6), (4, 0xa920_becb), (5, 0xa920_bed0), (6, 0x3220_cd99)] },
    Map { name: "Rig", pack: BASE, worlds: &[(1, 0x222a_cb69), (6, 0x3220_cd9f)] },
    Map { name: "Sand Storm", pack: BASE, worlds: &[(1, 0x222a_cb7b), (2, 0xbf0d_0b78), (4, 0x3220_c522), (5, 0xa926_5905), (6, 0x3220_cdab)] },
    Map { name: "Underground", pack: BASE, worlds: &[(1, 0x222a_cb8d), (2, 0xa926_0cfe), (3, 0xa926_5910), (4, 0x9d23_54b4), (5, 0xa926_5919), (6, 0x3220_cdb3)] },
    Map { name: "Mill", pack: BASE, worlds: &[(1, 0x222a_c535), (2, 0x0c28_3abe), (3, 0xa925_f973), (4, 0xa925_f977), (5, 0xa925_f97b), (6, 0x3220_cdb7)] },
    Map { name: "Alpha", pack: BASE, worlds: &[(1, 0x222a_cba6), (2, 0xa921_1903), (3, 0xa920_bf6f), (4, 0xa920_bf74), (5, 0xa920_bf79), (6, 0x3220_cdbf)] },
    Map { name: "Riot", pack: ARCTIC, worlds: &[(1, 0xa033_3e99), (2, 0x3220_f4de), (3, 0x3220_f542), (4, 0x3220_f55f), (5, 0x3220_f67e), (6, 0x3220_f064)] },
    Map { name: "Skyline", pack: ARCTIC, worlds: &[(1, 0xbc32_91bc), (2, 0x3220_f9db), (3, 0x3220_fa0f), (4, 0x3220_fa2c), (5, 0x3220_fa72), (6, 0xbc32_91d3)] },
    Map { name: "Evicted", pack: ARCTIC, worlds: &[(1, 0x3220_efa1), (2, 0x3220_fc58), (3, 0x3220_fcb5), (4, 0x3220_fce5), (5, 0x3220_fd02), (6, 0x3220_f07b)] },
    Map { name: "Switchback", pack: KHYBER, worlds: &[(1, 0x3220_f29c), (2, 0x3221_04af), (3, 0x3221_050c), (4, 0x3221_053c), (5, 0x3221_0559), (6, 0x3220_f094)] },
    Map { name: "Palace", pack: KHYBER, worlds: &[(1, 0x3220_f0e6), (2, 0x3221_0604), (3, 0x3221_0661), (4, 0x3221_0691), (5, 0x3221_06ae), (6, 0x3220_f0ab)] },
    Map { name: "Transit", pack: KHYBER, worlds: &[(1, 0xab31_4a2e), (2, 0xab31_491e), (3, 0xab31_4954), (4, 0xab31_4971), (5, 0xab31_4a08), (6, 0xab31_4a46)] },
    Map { name: "Market (Night)", pack: BASE, worlds: &[(1, 0xa110_fe1f), (2, 0xa110_fd2e), (3, 0xa110_fd62), (4, 0xa110_fd7f), (5, 0xa110_fdc5), (6, 0xa110_fde8)] },
    Map { name: "Harbor (Night)", pack: BASE, worlds: &[(1, 0xa112_0b05), (2, 0xa112_0a47), (3, 0xa112_0a77), (4, 0xa112_0a95), (5, 0xa112_0aeb), (6, 0xa112_0b1d)] },
    // Guerrilla: one world each.
    Map { name: "Shanty Town", pack: BASE, worlds: &[(8, 0x9f2a_ad81)] },
    Map { name: "Mansion", pack: BASE, worlds: &[(8, 0x9f2a_e9dd)] },
    Map { name: "Drilling Ship", pack: BASE, worlds: &[(8, 0x9f2b_a4e9)] },
    Map { name: "Arctic Base", pack: ARCTIC, worlds: &[(8, 0x9f2e_1b5b)] },
    Map { name: "Village", pack: KHYBER, worlds: &[(8, 0x9f2e_7cf4)] },
    Map { name: "Sawmill", pack: RAVEN, worlds: &[(8, 0x5d28_13a3)] },
];

/// The worlds a category covers: those of its modes (every adversarial mode when it names none)
/// on the base maps and the packs it names.
pub fn worlds(category: u32) -> Vec<u32> {
    let asked = |m: u32| {
        let bit = mode_bit(m);
        bit != 0
            && if category & (ADVERSARIAL | mode_bit(mode::GUERRILLA)) == 0 {
                ADVERSARIAL & bit != 0
            } else {
                category & bit != 0
            }
    };
    let has = |packs: u32| category & PACKS & packs == packs & PACKS;
    MAPS.iter()
        .flat_map(|m| m.worlds.iter().map(move |w| (m.pack, w)))
        .filter(|&(pack, &(m, _))| asked(m) && has(pack | mode_pack(m)))
        .map(|(_, &(_, key))| key)
        .collect()
}

/// GetMapList(u32 category) -> list<u32>.
pub fn map_list(parameters: &[u8]) -> Vec<u8> {
    let worlds = worlds(category(parameters));
    let mut out = (worlds.len() as u32).to_le_bytes().to_vec();
    for key in worlds {
        out.extend_from_slice(&key.to_le_bytes());
    }
    out
}

/// The map list protocol (121): its answers, and the packs each host's lobby uses, which
/// searches go by (see `packs`).
pub struct MapList {
    packs: std::sync::Arc<crate::packs::Packs>,
}

impl MapList {
    pub fn new(packs: std::sync::Arc<crate::packs::Packs>) -> Self {
        Self { packs }
    }
}

impl<CI> quazal::rmc::Protocol<CI> for MapList {
    fn id(&self) -> u16 {
        121
    }

    fn name(&self) -> String {
        "MapListProtocol".into()
    }

    fn num_methods(&self) -> u32 {
        2
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        match method_id {
            1 => Some("GetAvailableMapCategories".into()),
            2 => Some("GetMapList".into()),
            _ => None,
        }
    }

    fn handle(
        &self,
        logger: &slog::Logger,
        _ctx: &quazal::Context,
        ci: &mut quazal::ClientInfo<CI>,
        request: &quazal::rmc::Request,
        _client_registry: &quazal::prudp::ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, quazal::rmc::Error> {
        let pid = crate::ticket::player(ci)?;
        let answer = match request.method_id {
            // GetAvailableMapCategories: none (the game doesn't need any).
            1 => vec![0, 0, 0, 0],
            2 => {
                let category = category(&request.parameters);
                // Only the host of a lobby asks.
                self.packs.hosting(pid, category);
                map_list(&request.parameters)
            }
            _ => return Err(quazal::rmc::Error::UnknownMethod),
        };
        // The category and its worlds in full: what the game asked for is how answers are checked.
        slog::debug!(
            logger,
            "MapListProtocol.{}: answer to {} is {}",
            if request.method_id == 1 { "GetAvailableMapCategories" } else { "GetMapList" },
            crate::fixed::hex(&request.parameters[..request.parameters.len().min(64)]),
            crate::fixed::hex(&answer)
        );
        Ok(answer)
    }
}

fn category(parameters: &[u8]) -> u32 {
    parameters.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The category bits a lobby asks with, as the game makes them from its players' packs.
    const NONE: u32 = 0x4000;
    const ONLY_ARCTIC: u32 = 0x8000;
    const ONLY_KHYBER: u32 = 0x1_0000;
    const BOTH: u32 = 0x1_c000;
    /// Every combination of the two adversarial packs (Raven Strike changes nothing here).
    const COMBINATIONS: [(u32, &str); 5] = [
        (NONE, "no packs"),
        (0, "no pack bits"),
        (ONLY_ARCTIC, "Arctic Strike"),
        (ONLY_KHYBER, "Khyber Strike"),
        (BOTH, "both"),
    ];
    const MODES: [(u32, &str); 6] = [
        (1 << 4, "Conflict"),
        (1 << 5, "Decoy"),
        (1 << 6, "Saboteur"),
        (1 << 7, "Siege"),
        (1 << 8, "Stockade"),
        (1 << 9, "Takeover"),
    ];
    /// Size bits as the game sends them: 2 for the 12-player match, 1 for 8.
    const SIZES: [u32; 2] = [1, 2];

    /// Where each world is, as the game's archives have it (checked 2026-11-16 by looking every
    /// key up in Yeti.big and DLC\dlc1-3), independently of the table above: the base game, or
    /// pack 1 (Arctic Strike), 2 (Raven Strike) or 3 (Khyber Strike).
    fn archive(world: u32) -> u8 {
        let map = MAPS.iter().find(|m| m.worlds.iter().any(|&(_, k)| k == world)).expect("a world in the table");
        let (mode, _) = *map.worlds.iter().find(|&&(_, k)| k == world).unwrap();
        let night = map.name.ends_with("(Night)");
        match (map.name, mode) {
            ("Arctic Base", _) => 1,
            ("Sawmill", _) => 2,
            ("Village", _) => 3,
            ("Switchback" | "Palace" | "Transit", _) => 3,
            (_, mode::TAKEOVER) => 3,
            (_, mode::STOCKADE) if !night => 1,
            ("Riot" | "Skyline" | "Evicted", _) => 1,
            _ => 0,
        }
    }

    /// The packs a category says the lobby's players have.
    fn has(category: u32) -> [bool; 4] {
        [true, category & ARCTIC != 0, false, category & KHYBER != 0]
    }

    /// The pack a map's own level is in (its Conflict world's archive): a world of the map in
    /// another pack's mode needs this one too.
    fn home(map: &str) -> u8 {
        match map {
            "Riot" | "Skyline" | "Evicted" => 1,
            "Switchback" | "Palace" | "Transit" => 3,
            _ => 0,
        }
    }

    /// Whether a player with these packs can load and play the world: its archive and its map's
    /// are there, and the game's own rule (Stockade needs Arctic Strike, Takeover Khyber Strike)
    /// holds.
    fn playable(world: u32, packs: [bool; 4]) -> bool {
        let map = MAPS.iter().find(|m| m.worlds.iter().any(|&(_, k)| k == world)).unwrap();
        let mode = map.worlds.iter().find(|&&(_, k)| k == world).unwrap().0;
        packs[archive(world) as usize] && packs[home(map.name) as usize] && (mode != mode::STOCKADE || packs[1]) && (mode != mode::TAKEOVER || packs[3])
    }

    #[test]
    fn every_world_offered_is_one_the_lobby_can_load_for_every_pack_combination() {
        for (packs, who) in COMBINATIONS {
            for (mode, name) in MODES {
                for size in SIZES {
                    for world in worlds(packs | mode | size) {
                        assert!(playable(world, has(packs)), "{name} for {who}: {world:#x} needs a pack the lobby hasn't");
                    }
                }
            }
        }
    }

    #[test]
    fn every_playable_world_is_offered_for_every_pack_combination() {
        for (packs, who) in COMBINATIONS {
            for (mode, name) in MODES {
                let offered = worlds(packs | mode | 2);
                let want = mode_of(mode);
                for world in MAPS.iter().flat_map(|m| m.worlds).filter(|&&(m, _)| m == want).map(|&(_, k)| k) {
                    if playable(world, has(packs)) {
                        assert!(offered.contains(&world), "{name} for {who}: {world:#x} is playable but not offered");
                    }
                }
            }
        }
    }

    fn mode_of(bit: u32) -> u32 {
        MODE_BITS.iter().find(|&&(b, _)| b == bit).unwrap().1
    }

    #[test]
    fn what_each_combination_gets() {
        let count = |category: u32| worlds(category).len();
        // Conflict: 9 base maps (not Rig) and the 2 night maps; Arctic Strike adds its 3 maps,
        // Khyber Strike its 3.
        assert_eq!(count(NONE | 0x10 | 2), 9 + 2);
        assert_eq!(count(0x10 | 2), 9 + 2, "no pack bits is the base game too");
        assert_eq!(count(ONLY_ARCTIC | 0x10 | 2), 9 + 2 + 3);
        assert_eq!(count(ONLY_KHYBER | 0x10 | 2), 9 + 2 + 3);
        assert_eq!(count(BOTH | 0x10 | 2), 9 + 2 + 3 + 3);
        // Stockade needs Arctic Strike: 10 base maps, the 2 night maps and Riot, Skyline, Evicted;
        // with Khyber Strike too, its 3 maps.
        assert_eq!(count(NONE | 0x100 | 2), 0);
        assert_eq!(count(ONLY_KHYBER | 0x100 | 2), 0);
        assert_eq!(count(ONLY_ARCTIC | 0x100 | 2), 10 + 2 + 3);
        assert_eq!(count(BOTH | 0x100 | 2), 10 + 2 + 3 + 3);
        // Takeover needs Khyber Strike: 10 base maps, the 2 night maps and its own 3; with Arctic
        // Strike too, Riot, Skyline and Evicted.
        assert_eq!(count(NONE | 0x200 | 2), 0);
        assert_eq!(count(ONLY_ARCTIC | 0x200 | 2), 0);
        assert_eq!(count(ONLY_KHYBER | 0x200 | 2), 10 + 2 + 3);
        assert_eq!(count(BOTH | 0x200 | 2), 10 + 2 + 3 + 3);
    }

    #[test]
    fn as_the_game_asked_with_every_pack() {
        // Seen from the game with all three packs: Conflict 0x1c012.
        let conflict = worlds(0x1_c012);
        assert!(conflict.contains(&0xa921_1863), "Pipeline's");
        assert!(conflict.contains(&0x3220_f4de), "Riot's");
        assert!(conflict.contains(&0x3221_04af), "Switchback's");
        assert!(!conflict.contains(&0x222a_c79a) && !conflict.contains(&0xa920_bdb8), "no Stockade or Saboteur worlds");
        // And with none (before the server granted them): Conflict 0x4012, base maps only.
        let base = worlds(0x4012);
        assert!(base.contains(&0xa921_1863) && base.contains(&0xa110_fd2e), "Pipeline's and Market (Night)'s");
        assert!(!base.contains(&0x3220_f4de) && !base.contains(&0x3221_04af), "no pack maps");
    }

    #[test]
    fn takeover_never_offers_team_deathmatch() {
        for (packs, _) in COMBINATIONS {
            let takeover = worlds(packs | 0x200 | 2);
            assert!(!takeover.contains(&0xa110_fe01) && !takeover.contains(&0xa112_0b46));
        }
    }

    #[test]
    fn guerrilla_has_its_own_worlds() {
        let guerrilla = mode_bit(mode::GUERRILLA);
        assert_eq!(worlds(guerrilla), vec![0x9f2a_ad81, 0x9f2a_e9dd, 0x9f2b_a4e9]);
        assert_eq!(worlds(guerrilla | ONLY_ARCTIC), vec![0x9f2a_ad81, 0x9f2a_e9dd, 0x9f2b_a4e9, 0x9f2e_1b5b]);
        assert_eq!(worlds(guerrilla | BOTH).len(), 5, "Village with Khyber Strike; never Sawmill (Raven Strike has no bit)");
        assert!(!worlds(0).contains(&0x9f2a_ad81), "not unless asked for");
    }

    #[test]
    fn no_modes_means_every_base_mode_and_the_answer_is_a_list() {
        let base: usize = MAPS
            .iter()
            .filter(|m| m.pack == BASE)
            .flat_map(|m| m.worlds)
            .filter(|(m, _)| (mode::CONFLICT..=mode::SIEGE).contains(m))
            .count();
        assert_eq!(worlds(0).len(), base);
        let reply = map_list(&0x2u32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(reply[..4].try_into().unwrap()) as usize * 4 + 4, reply.len());
        assert_eq!(map_list(&[]), map_list(&0u32.to_le_bytes()));
    }
}
