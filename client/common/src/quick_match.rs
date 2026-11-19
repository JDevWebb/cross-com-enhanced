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
