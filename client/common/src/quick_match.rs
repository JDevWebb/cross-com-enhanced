//! The fewest players a Quick Match lobby starts with (`quick_match_min_players`).
//!
//! The game's AI library (`AICLASS_PC_F.dll`) takes the minimum from its data (8 players, or 6
//! in the smaller modes), so a small community never starts a ranked match. What a lobby's start
//! is measured against is set by the host as each player joins (the data's minimum, raised to
//! twice the biggest party less one; each game numbers its own parties, so two solo players can
//! look like one party). The client makes that rule give the setting, and keeps the data's
//! minimums at the setting too (found as the operands of the code that reads them). Places are
//! found by their surrounding code (the library may load elsewhere than its preferred address),
//! only when each is found exactly once and both read the same value; otherwise nothing changes.
//! Tested in game: a 2-player Quick Match lobby starts.

/// A pattern: hex bytes, `??` for any byte (an address the loader may have moved).
pub(crate) type Pattern = &'static str;

pub(crate) fn bytes(pattern: Pattern) -> Vec<Option<u8>> {
    pattern.split_whitespace().map(|b| u8::from_str_radix(b, 16).ok()).collect()
}

/// Where a new lobby's minimum is chosen: three reads (one per lobby size) of the form
/// `mov al, [minimum]` (A0 + address) at +0x36, +0x3e and +0x46.
const CHOOSER: Pattern = "83 7C 24 04 02 75 51 83 F8 03 74 47 83 F8 02 74 42 8B C1 83 E0 03 3C 03 75 34 8B 0D ?? ?? ?? ?? 3B 0D ?? ?? ?? ?? 75 1E B8 2F 00 00 00 E8 ?? ?? ?? ?? 84 C0 75 08 A0 ?? ?? ?? ?? C2 08 00 A0 ?? ?? ?? ?? C2 08 00 A0 ?? ?? ?? ?? C2 08 00";
const CHOOSER_READS: [usize; 3] = [0x36, 0x3e, 0x46];

/// Where the host raises a lobby's minimum as players join: `mov cl, [minimum]` (8A 0D +
/// address) at +0x19, then twice the biggest party less one (`add al, al; dec al`) at +0x1f.
/// Solo players can look like one party there (each game numbers its own parties), which made
/// a 2-player lobby need 3; the party part is dropped, so the minimum is the setting.
const JOIN_RULE: Pattern = "80 7C 24 37 01 75 4B 0F B6 4C 24 39 39 0D ?? ?? ?? ?? 75 2E E8 ?? ?? ?? ?? 8A 0D ?? ?? ?? ?? 02 C0 FE C8 3A C1 77 02 8A C1";
const JOIN_READ: usize = 0x19;
const JOIN_PARTY: usize = 0x1f;

/// Where the game reads "Squad room min players" (4 in its data: four a side). Each read is
/// patched to the setting as the library loads, so no lobby or menu ever gets the data's value:
/// the menus are told it once, as they start (`setAIDLLMinSquadPartyPlayers`), and their Squad
/// Ready Room won't search for a match with fewer in the party ("Failed to join the requested
/// match. Waiting for players... [N]"); a squad room lobby's minimum is twice it.
///
/// - The menus: `mov edx, [minimum]` (8B 15 + address) at +0, followed by the binding call.
const SQUAD_MENU: Pattern = "8B 15 ?? ?? ?? ?? A1 ?? ?? ?? ?? 8B 88 34 13 00 00 83 C4 0C 53 52 FF D1";
/// - A squad room lobby, made by the host: `cmp eax, 5; jnz; mov al, [minimum]; add al, al`, the
///   read (A0 + address) at +11.
const SQUAD_ROOM: Pattern = "89 5E 0C 88 5E 23 83 F8 05 75 09 A0 ?? ?? ?? ?? 02 C0";
const SQUAD_ROOM_READ: usize = 11;
/// - A squad room lobby, made on joining: `cmp al, 5; jnz; mov cl, [minimum]; add cl, cl`, the read
///   (8A 0D + address) at +4. Twice, one per lobby size.
const SQUAD_JOIN: Pattern = "3C 05 75 0A 8A 0D ?? ?? ?? ?? 02 C9 EB 06 8A 0D";
const SQUAD_JOIN_READ: usize = 4;

/// The library's file name, as the game loads it.
pub const MODULE: &str = "AICLASS_PC_F.dll";

/// A change to make: `bytes` at `offset` into the module's image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub offset: usize,
    pub bytes: Vec<u8>,
}

pub(crate) fn find_once(image: &[u8], pattern: Pattern) -> Result<usize, String> {
    let pattern = bytes(pattern);
    let mut found = image
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, w)| w.iter().zip(&pattern).all(|(b, p)| p.is_none_or(|p| *b == p)))
        .map(|(i, _)| i);
    match (found.next(), found.next()) {
        (Some(at), None) => Ok(at),
        (None, _) => Err("not found".into()),
        (Some(_), Some(_)) => Err("found more than once".into()),
    }
}

/// Every place `pattern` is found.
pub(crate) fn find_all(image: &[u8], pattern: Pattern) -> Vec<usize> {
    let pattern = bytes(pattern);
    image
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, w)| w.iter().zip(&pattern).all(|(b, p)| p.is_none_or(|p| *b == p)))
        .map(|(i, _)| i)
        .collect()
}

pub(crate) fn address_at(image: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([image[at], image[at + 1], image[at + 2], image[at + 3]])
}

/// Where the game keeps the three minimums it reads from its data (one per lobby size), as the
/// lobby code addresses them: absolute addresses, already relocated.
///
/// # Errors
/// As for [`plan`].
pub fn minimums(image: &[u8]) -> Result<[u32; 3], String> {
    let chooser = find_once(image, CHOOSER).map_err(|e| format!("lobby minimum: {e}"))?;
    Ok(CHOOSER_READS.map(|read| address_at(image, chooser + read + 1)))
}

/// The squad room minimum (players a side) for a Quick Match minimum of `players`: half, rounded
/// up, so a squad match starts with as few players in all.
#[must_use]
pub fn squad_players(players: u8) -> u8 {
    players.div_ceil(2).max(1)
}

/// The changes that make "Squad room min players" `per_side` wherever the library reads it.
///
/// # Errors
/// A place isn't found exactly once, or the three don't read the same value.
pub fn plan_squad(image: &[u8], per_side: u8) -> Result<Vec<Patch>, String> {
    let menu = find_once(image, SQUAD_MENU).map_err(|e| format!("squad minimum for the menus: {e}"))?;
    let room = find_once(image, SQUAD_ROOM).map_err(|e| format!("squad room minimum: {e}"))?;
    let joins = find_all(image, SQUAD_JOIN);
    if joins.len() != 2 {
        return Err(format!("squad room minimum on joining: found {} times, not twice", joins.len()));
    }
    let minimum = address_at(image, menu + 2);
    if address_at(image, room + SQUAD_ROOM_READ + 1) != minimum || joins.iter().any(|&j| address_at(image, j + SQUAD_JOIN_READ + 2) != minimum) {
        return Err("the squad minimum's reads read different values".into());
    }
    let mut menu_bytes = vec![0xBA]; // mov edx, per_side
    menu_bytes.extend(u32::from(per_side).to_le_bytes());
    menu_bytes.push(0x90);
    let mut patches = vec![
        Patch { offset: menu, bytes: menu_bytes },
        Patch {
            offset: room + SQUAD_ROOM_READ,
            bytes: vec![0xB0, per_side, 0x90, 0x90, 0x90], // mov al, per_side
        },
    ];
    patches.extend(joins.iter().map(|&join| Patch {
        offset: join + SQUAD_JOIN_READ,
        bytes: vec![0xB1, per_side, 0x90, 0x90, 0x90, 0x90], // mov cl, per_side
    }));
    Ok(patches)
}

/// The changes that make every Quick Match minimum `players`, in the module's image as loaded
/// (its sections in memory), before any of them is made.
///
/// # Errors
/// Either place isn't found exactly once, or they don't read the same value: a game build these
/// patterns weren't made for.
pub fn plan(image: &[u8], players: u8) -> Result<Vec<Patch>, String> {
    let players = players.clamp(crate::config::MIN_QUICK_MATCH_PLAYERS, 12);
    let chooser = find_once(image, CHOOSER).map_err(|e| format!("lobby minimum: {e}"))?;
    let join = find_once(image, JOIN_RULE).map_err(|e| format!("join rule: {e}"))?;
    let minimum = address_at(image, chooser + CHOOSER_READS[0] + 1);
    if address_at(image, join + JOIN_READ + 2) != minimum {
        return Err("the two places read different values".into());
    }
    let mut patches: Vec<Patch> = CHOOSER_READS
        .iter()
        .map(|&read| Patch {
            offset: chooser + read,
            bytes: vec![0xB0, players, 0x90, 0x90, 0x90], // mov al, players
        })
        .collect();
    patches.push(Patch {
        offset: join + JOIN_READ,
        bytes: vec![0xB1, players, 0x90, 0x90, 0x90, 0x90], // mov cl, players
    });
    patches.push(Patch {
        offset: join + JOIN_PARTY,
        bytes: vec![0x30, 0xC0, 0x90, 0x90], // xor al, al: no party size
    });
    Ok(patches)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up image holding both places, reading `minimum`, at some distance from each other.
    fn image(minimum: u32) -> Vec<u8> {
        let fill = |p: Pattern, slot: &[usize], value: u32| {
            let mut bytes: Vec<u8> = super::bytes(p).iter().map(|b| b.unwrap_or(0x11)).collect();
            for &at in slot {
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            bytes
        };
        let mut image = vec![0xCC; 64];
        image.extend(fill(CHOOSER, &[0x37, 0x3f, 0x47], minimum));
        image.extend(vec![0x90; 300]);
        image.extend(fill(JOIN_RULE, &[0x1b], minimum));
        image.extend(vec![0xCC; 64]);
        image
    }

    #[test]
    fn plans_four_reads_as_the_number() {
        let img = image(0x1071_8f5c);
        assert_eq!(minimums(&img).unwrap(), [0x1071_8f5c; 3]);
        let patches = plan(&img, 2).unwrap();
        assert_eq!(patches.len(), 5);
        assert_eq!(patches[4].bytes, vec![0x30, 0xC0, 0x90, 0x90]);
        assert_eq!(
            patches[0],
            Patch {
                offset: 64 + 0x36,
                bytes: vec![0xB0, 2, 0x90, 0x90, 0x90]
            }
        );
        assert_eq!(patches[3].bytes, vec![0xB1, 2, 0x90, 0x90, 0x90, 0x90]);
        assert_eq!(plan(&img, 0).unwrap()[0].bytes[1], 2, "at least 2");
    }

    #[test]
    fn patches_the_three_squad_minimum_reads() {
        let fill = |p: Pattern, at: usize, value: u32| {
            let mut bytes: Vec<u8> = super::bytes(p).iter().map(|b| b.unwrap_or(0x11)).collect();
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            bytes
        };
        let min = 0x1071_8f94;
        let mut img = vec![0xCC; 16];
        img.extend(fill(SQUAD_MENU, 2, min));
        img.extend(vec![0xCC; 16]);
        let room = img.len();
        img.extend(fill(SQUAD_ROOM, SQUAD_ROOM_READ + 1, min));
        img.extend(vec![0xCC; 16]);
        let join = img.len();
        img.extend(fill(SQUAD_JOIN, SQUAD_JOIN_READ + 2, min));
        img.extend(vec![0xCC; 16]);
        assert!(plan_squad(&img, 1).is_err(), "the join read once only");
        let join2 = img.len();
        img.extend(fill(SQUAD_JOIN, SQUAD_JOIN_READ + 2, min));
        img.extend(vec![0xCC; 16]);
        let patches = plan_squad(&img, 1).unwrap();
        assert_eq!(patches.len(), 4);
        assert_eq!(patches[3].offset, join2 + SQUAD_JOIN_READ);
        assert_eq!(
            patches[0],
            Patch {
                offset: 16,
                bytes: vec![0xBA, 1, 0, 0, 0, 0x90]
            }
        );
        assert_eq!(
            patches[1],
            Patch {
                offset: room + SQUAD_ROOM_READ,
                bytes: vec![0xB0, 1, 0x90, 0x90, 0x90]
            }
        );
        assert_eq!(
            patches[2],
            Patch {
                offset: join + SQUAD_JOIN_READ,
                bytes: vec![0xB1, 1, 0x90, 0x90, 0x90, 0x90]
            }
        );
        img[join + SQUAD_JOIN_READ + 2..join + SQUAD_JOIN_READ + 6].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        assert!(plan_squad(&img, 1).is_err(), "the reads disagree");
        assert_eq!((squad_players(2), squad_players(3), squad_players(8), squad_players(0)), (1, 2, 4, 1));
    }

    #[test]
    fn changes_nothing_on_a_build_it_does_not_know() {
        let mut img = image(0x1071_8f5c);
        img[64 + 0x47..64 + 0x4b].copy_from_slice(&0x1071_8f70u32.to_le_bytes()); // fine: another size's
        assert!(plan(&img, 2).is_ok());
        let join = img.len() - 64 - bytes(JOIN_RULE).len();
        img[join + 0x1b..join + 0x1f].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        assert!(plan(&img, 2).is_err(), "the two places disagree");
        let twice = [image(1), image(1)].concat();
        assert!(plan(&twice, 2).is_err(), "found twice");
        assert!(plan(&[0; 512], 2).is_err());
    }
}
