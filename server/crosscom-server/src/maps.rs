//! The map list (protocol 121): which worlds a hosted match may vote between.
//!
//! Each adversarial map has one world per game mode, and the game loads the world it is given,
//! so a world key also picks the mode. The game asks GetMapList with a category: a bit per game
//! mode and a bit per map pack; bit 1 is always set and isn't a mode. The answer is every world
//! of those modes on the base maps and those packs.
//!
//! Two numberings meet here. A world's mode is the game's mode list: 1 Stockade, 2 Conflict,
//! 3 Decoy, 4 Saboteur, 5 Siege, 6 Takeover, 7 Team Deathmatch. The category uses the menu's
//! numbering (the session's property 116), `1 << (n + 2)`: 2 Conflict, 3 Decoy, 4 Saboteur,
//! 5 Siege, 6 Stockade, 7 Takeover, 8 Guerrilla. Seen from the game (2026-11-13): Conflict 0x10,
//! Decoy 0x20, Stockade 0x100, Takeover 0x200.
//!
//! Team Deathmatch worlds exist only for the two night maps and the game has no rules for the
//! mode: loading one hangs, so they are never offered.
//!
//! The downloadable modes need their pack even on the base maps: Stockade came with Arctic
//! Strike (its config lists every base map's Stockade world; offered without it, a host was
//! kicked from its own match for "content that you have not yet downloaded"), Takeover with the
//! second pack (Overpass's Takeover world first appears in that pack's world list, and the game
//! asks for Takeover with that pack's bit).

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

/// Map packs, as bits of the category. The base maps are always offered. Which pack a world
/// came with is read from the game's data: Arctic Strike's config lists its worlds, and each
/// pack's bigfile carries the world list as it stood with that pack (the night maps are in the
/// first pack's, Switchback, Palace and Transit only from the second's).
const BASE: u32 = 0;
const ARCTIC: u32 = 0x4000;
const RAVEN: u32 = 0x8000;
const KHYBER: u32 = 0x1_0000;
const PACKS: u32 = ARCTIC | RAVEN | KHYBER;

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
        mode::TAKEOVER => RAVEN,
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
    Map { name: "Switchback", pack: RAVEN, worlds: &[(1, 0x3220_f29c), (2, 0x3221_04af), (3, 0x3221_050c), (4, 0x3221_053c), (5, 0x3221_0559), (6, 0x3220_f094)] },
    Map { name: "Palace", pack: RAVEN, worlds: &[(1, 0x3220_f0e6), (2, 0x3221_0604), (3, 0x3221_0661), (4, 0x3221_0691), (5, 0x3221_06ae), (6, 0x3220_f0ab)] },
    Map { name: "Transit", pack: RAVEN, worlds: &[(1, 0xab31_4a2e), (2, 0xab31_491e), (3, 0xab31_4954), (4, 0xab31_4971), (5, 0xab31_4a08), (6, 0xab31_4a46)] },
    Map { name: "Market (Night)", pack: ARCTIC, worlds: &[(1, 0xa110_fe1f), (2, 0xa110_fd2e), (3, 0xa110_fd62), (4, 0xa110_fd7f), (5, 0xa110_fdc5), (6, 0xa110_fde8)] },
    Map { name: "Harbor (Night)", pack: ARCTIC, worlds: &[(1, 0xa112_0b05), (2, 0xa112_0a47), (3, 0xa112_0a77), (4, 0xa112_0a95), (5, 0xa112_0aeb), (6, 0xa112_0b1d)] },
    // Guerrilla: one world each (which pack Village and Sawmill came with is a guess).
    Map { name: "Shanty Town", pack: BASE, worlds: &[(8, 0x9f2a_ad81)] },
    Map { name: "Mansion", pack: BASE, worlds: &[(8, 0x9f2a_e9dd)] },
    Map { name: "Drilling Ship", pack: BASE, worlds: &[(8, 0x9f2b_a4e9)] },
    Map { name: "Arctic Base", pack: ARCTIC, worlds: &[(8, 0x9f2e_1b5b)] },
    Map { name: "Village", pack: RAVEN, worlds: &[(8, 0x9f2e_7cf4)] },
    Map { name: "Sawmill", pack: KHYBER, worlds: &[(8, 0x5d28_13a3)] },
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
    let category = parameters.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let worlds = worlds(category);
    let mut out = (worlds.len() as u32).to_le_bytes().to_vec();
    for key in worlds {
        out.extend_from_slice(&key.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_as_the_game_asks_gives_conflict_worlds() {
        let worlds = worlds(0x4012);
        // Pipeline's and Riot's Conflict worlds.
        assert!(worlds.contains(&0xa921_1863));
        assert!(worlds.contains(&0x3220_f4de));
        // No Stockade or Saboteur worlds, and no packs not asked for.
        assert!(!worlds.contains(&0x222a_c79a));
        assert!(!worlds.contains(&0xa920_bdb8));
        assert!(!worlds.contains(&0x3221_04af));
        // 9 base maps have a Conflict world (not Rig), Arctic Strike's 3 and the 2 night maps.
        assert_eq!(worlds.len(), 9 + 3 + 2);
        assert_eq!(super::worlds(0x4022).len(), 8 + 3 + 2, "Decoy: not on Rig or Sand Storm");
    }

    #[test]
    fn stockade_asks_for_the_stockade_worlds() {
        // Seen from the game: Stockade 0xc102 (Arctic and the second pack).
        let stockade = worlds(0xc102);
        assert!(stockade.contains(&0x222a_c79a), "Pipeline's");
        assert!(stockade.contains(&0x222a_cb69), "Rig's");
        assert!(stockade.contains(&0xab31_4a2e), "Transit's");
        assert!(!stockade.contains(&0x3220_cd6e), "not Pipeline's Takeover world");
        // 10 base maps, 3 Arctic Strike, 3 second-pack and the 2 night maps.
        assert_eq!(stockade.len(), 10 + 3 + 3 + 2);
        assert!(worlds(0x0102).is_empty(), "Stockade needs Arctic Strike");
        assert_eq!(worlds(0x4102).len(), 10 + 3 + 2);
    }

    #[test]
    fn takeover_asks_for_the_takeover_worlds_never_team_deathmatch() {
        // Seen from the game: Takeover 0xc202.
        let takeover = worlds(0xc202);
        assert!(takeover.contains(&0x3220_cd6e), "Pipeline's");
        assert!(takeover.contains(&0x3220_c22c), "Overpass's");
        assert!(takeover.contains(&0xa110_fde8), "Market (Night)'s");
        assert!(takeover.contains(&0xab31_4a46), "Transit's");
        assert!(!takeover.contains(&0xa110_fe01) && !takeover.contains(&0xa112_0b46), "no Team Deathmatch worlds");
        assert_eq!(takeover.len(), 10 + 3 + 3 + 2);
        assert!(worlds(0x4202).is_empty(), "Takeover needs the second pack");
        assert_eq!(worlds(0x8202).len(), 10 + 3);
    }

    #[test]
    fn guerrilla_has_its_own_worlds() {
        let guerrilla = mode_bit(mode::GUERRILLA);
        assert_eq!(worlds(guerrilla), vec![0x9f2a_ad81, 0x9f2a_e9dd, 0x9f2b_a4e9]);
        assert_eq!(worlds(guerrilla | ARCTIC).len(), 4);
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
