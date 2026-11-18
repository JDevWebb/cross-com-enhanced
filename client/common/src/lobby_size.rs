//! The most players a lobby takes, which the game chooses as the lobby is made.
//!
//! The game's AI library takes a new lobby's size from its data: 12 players for a Quick Match (8
//! in the smaller size), 4 for Guerrilla and co-op. That size is the session's slots for good:
//! the game reports them to the server (which offers a Quick Match only while it has free slots)
//! and stops offering invitations once they're taken. Changing a lobby's size after it's made
//! does neither, so a host's choice is made the size of the lobbies it makes next, by keeping
//! the data the size is chosen from at it (never above each mode's own size). The places are
//! found as the operands of the code that reads them (the library may load elsewhere than its
//! preferred address), only when that code is found exactly once.
//! Tested in game: a Guerrilla lobby made with 1 offered no invitation; a Quick Match made with 4
//! was a 4-player session.

use crate::quick_match::address_at;
use crate::quick_match::find_once;
use crate::quick_match::Pattern;

/// Where a new lobby's size is chosen: five reads of the form `mov bl, [size]` (8A 1D +
/// address): the Quick Match sizes at +0x20, +0x34 and +0x48, Guerrilla's at +0x6a and the other
/// custom matches' at +0x72.
const CHOOSER: Pattern = "8B 15 ?? ?? ?? ?? 3B 15 ?? ?? ?? ?? 75 3A B8 2F 00 00 00 E8 ?? ?? ?? ?? 8D 4C 24 0C 84 C0 75 14 8A 1D ?? ?? ?? ?? E8 ?? ?? ?? ?? 8A C3 5F 5E 5B 59 C2 0C 00 8A 1D ?? ?? ?? ?? E8 ?? ?? ?? ?? 8A C3 5F 5E 5B 59 C2 0C 00 8A 1D ?? ?? ?? ?? 8D 4C 24 0C E8 ?? ?? ?? ?? 8A C3 5F 5E 5B 59 C2 0C 00 F6 C3 01 EB A7 83 7C 24 1C 08 8A 1D ?? ?? ?? ?? 74 06 8A 1D ?? ?? ?? ??";
const READS: [usize; 5] = [0x20, 0x34, 0x48, 0x6a, 0x72];

/// The most players any lobby may be set to: the game's biggest.
pub const MOST: u8 = 12;

/// Where the game keeps the five sizes it makes lobbies with, as the code addresses them:
/// absolute addresses, already relocated.
///
/// # Errors
/// The code isn't found exactly once: a game build this pattern wasn't made for.
pub fn sizes(image: &[u8]) -> Result<[u32; 5], String> {
    let at = find_once(image, CHOOSER).map_err(|e| format!("lobby size: {e}"))?;
    Ok(READS.map(|read| address_at(image, at + read + 2)))
}

/// What a size the game made a lobby with becomes, given the host's choice (`None`: the game's).
#[must_use]
pub fn chosen(game: u8, most: Option<u8>) -> u8 {
    most.map_or(game, |most| game.min(most.clamp(1, MOST)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quick_match::bytes;

    fn image(addresses: [u32; 5]) -> Vec<u8> {
        let mut code: Vec<u8> = bytes(CHOOSER).iter().map(|b| b.unwrap_or(0x11)).collect();
        for (read, address) in READS.iter().zip(addresses) {
            code[read + 2..read + 6].copy_from_slice(&address.to_le_bytes());
        }
        [vec![0xCC; 32], code, vec![0xCC; 32]].concat()
    }

    #[test]
    fn finds_the_five_sizes() {
        let wanted = [0x1071_8f58, 0x1071_8f64, 0x1071_8f6c, 0x1071_8f80, 0x1071_8f78];
        assert_eq!(sizes(&image(wanted)).unwrap(), wanted);
    }

    #[test]
    fn finds_nothing_in_a_build_it_does_not_know() {
        assert!(sizes(&[0x90; 256]).is_err());
        let twice = [image([1; 5]), image([1; 5])].concat();
        assert!(sizes(&twice).is_err(), "found twice");
    }

    #[test]
    fn a_choice_never_makes_a_lobby_bigger() {
        assert_eq!(chosen(12, None), 12);
        assert_eq!(chosen(12, Some(6)), 6);
        assert_eq!(chosen(4, Some(6)), 4, "Guerrilla stays 4");
        assert_eq!(chosen(12, Some(0)), 1);
        assert_eq!(chosen(12, Some(40)), 12);
    }
}
