//! The game's DLC packs: which are installed, and what a player without one can't play.
//!
//! Each pack is a folder under the game's `DLC` folder holding its archive (`DLC\dlc1\dlc1.big`)
//! and its `dlc.ini`. The game mounts a pack only when its archive is there (and the server grants
//! it, which Cross-Com's servers do for every pack), so the folder is what decides. A match that
//! uses a pack needs every player in it to have the pack. `DLC\dlc` is the game's own update,
//! not a pack.
//!
//! The launcher only says what's missing. It never offers the packs or says where to get them.

use std::path::Path;

/// One of the game's DLC packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pack {
    /// Its folder under `DLC`, and its archive's name there (`dlc1` and `dlc1.big`).
    pub folder: &'static str,
    pub name: &'static str,
    /// What it adds, as players would list it.
    pub adds: &'static str,
    /// What a player without it can't play online.
    pub without: &'static str,
}

/// The three packs, in the game's order.
pub const PACKS: [Pack; 3] = [
    Pack {
        folder: "dlc1",
        name: "Arctic Strike",
        adds: "Stockade, the Riot, Skyline and Evicted maps, Arctic Base (Guerrilla)",
        without: "Stockade, and the Riot, Skyline and Evicted maps",
    },
    Pack {
        folder: "dlc2",
        name: "Raven Strike",
        adds: "Campaign missions and Sawmill (Guerrilla): co-op only",
        without: "its co-op missions and the Sawmill Guerrilla map",
    },
    Pack {
        folder: "dlc3",
        name: "Khyber Strike",
        adds: "Takeover, the Switchback, Palace and Transit maps, Village (Guerrilla)",
        without: "Takeover, and the Switchback, Palace and Transit maps",
    },
];

/// Smaller than any real pack archive (the smallest is hundreds of megabytes): a file this size
/// is a download that stopped or a placeholder.
const MIN_ARCHIVE: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Installed,
    Missing,
    /// The folder is there but the pack can't load: what's wrong.
    Incomplete(String),
}

/// Each pack and how it stands in `game` (the game's folder).
#[must_use]
pub fn scan(game: &Path) -> Vec<(Pack, State)> {
    PACKS.iter().map(|&pack| (pack, state(game, pack))).collect()
}

fn state(game: &Path, pack: Pack) -> State {
    let dir = game.join("DLC").join(pack.folder);
    if !dir.is_dir() {
        return State::Missing;
    }
    let archive = dir.join(format!("{}.big", pack.folder));
    match std::fs::metadata(&archive) {
        Err(_) => State::Incomplete(format!("DLC\\{0}\\{0}.big is missing", pack.folder)),
        Ok(m) if m.len() < MIN_ARCHIVE => State::Incomplete(format!("DLC\\{0}\\{0}.big is only {1} bytes", pack.folder, m.len())),
        Ok(_) if !dir.join("dlc.ini").is_file() => State::Incomplete(format!("DLC\\{}\\dlc.ini is missing", pack.folder)),
        Ok(_) => State::Installed,
    }
}

/// The installed packs' numbers, as the client tells the server at sign-in ("13" for Arctic and
/// Khyber Strike, "" for none; see the server's `packs`).
#[must_use]
pub fn installed_numbers(game: &Path) -> String {
    scan(game)
        .iter()
        .filter(|(_, s)| *s == State::Installed)
        .map(|(p, _)| p.folder.trim_start_matches("dlc"))
        .collect()
}

/// What to tell the player about their packs, or `None` when every pack is installed: which are
/// missing or incomplete and what that keeps them out of.
#[must_use]
pub fn warning(scan: &[(Pack, State)]) -> Option<String> {
    let missing: Vec<&Pack> = scan.iter().filter(|(_, s)| *s == State::Missing).map(|(p, _)| p).collect();
    let broken: Vec<(&Pack, &str)> = scan
        .iter()
        .filter_map(|(p, s)| match s {
            State::Incomplete(why) => Some((p, why.as_str())),
            _ => None,
        })
        .collect();
    if missing.is_empty() && broken.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    if !missing.is_empty() {
        let names = list(&missing.iter().map(|p| p.name).collect::<Vec<_>>());
        let without = missing.iter().map(|p| p.without).collect::<Vec<_>>().join("; ");
        let (them, a_match) = if missing.len() == 1 {
            ("it", "a match that uses it")
        } else {
            ("them", "a match that uses one")
        };
        lines.push(format!(
            "Not installed: {names}. You can play everything else online, but not {without}: {a_match} needs every player in it to have {them}."
        ));
    }
    for (pack, why) in broken {
        lines.push(format!(
            "{} looks incomplete ({why}), so the game won't load it. Check the game's files in Ubisoft Connect or Steam.",
            pack.name
        ));
    }
    Some(lines.join("\n"))
}

/// "A", "A and B", "A, B and C".
fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A game folder with the given packs installed (each a real-sized archive and its dlc.ini).
    fn game(packs: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DLC/dlc")).unwrap();
        for p in packs {
            let d = dir.path().join("DLC").join(p);
            std::fs::create_dir_all(&d).unwrap();
            let big = std::fs::File::create(d.join(format!("{p}.big"))).unwrap();
            big.set_len(MIN_ARCHIVE).unwrap();
            std::fs::write(d.join("dlc.ini"), "Name=x\n").unwrap();
        }
        dir
    }

    fn states(dir: &Path) -> Vec<State> {
        scan(dir).into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn every_combination_of_packs_is_seen_as_it_is() {
        let all = ["dlc1", "dlc2", "dlc3"];
        for mask in 0..8u8 {
            let have: Vec<&str> = all.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, p)| *p).collect();
            let dir = game(&have);
            let want: Vec<State> = all.iter().map(|p| if have.contains(p) { State::Installed } else { State::Missing }).collect();
            assert_eq!(states(dir.path()), want, "packs {have:?}");
            assert_eq!(warning(&scan(dir.path())).is_none(), have.len() == 3, "packs {have:?}");
        }
    }

    #[test]
    fn the_numbers_the_server_is_told() {
        assert_eq!(installed_numbers(game(&["dlc1", "dlc2", "dlc3"]).path()), "123");
        assert_eq!(installed_numbers(game(&["dlc1", "dlc3"]).path()), "13");
        assert_eq!(installed_numbers(game(&[]).path()), "");
        let incomplete = game(&["dlc1", "dlc3"]);
        std::fs::remove_file(incomplete.path().join("DLC/dlc3/dlc3.big")).unwrap();
        assert_eq!(installed_numbers(incomplete.path()), "1", "an incomplete pack can't load: not counted");
    }

    #[test]
    fn a_game_with_no_dlc_folder_has_no_packs() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(states(dir.path()), vec![State::Missing; 3]);
    }

    #[test]
    fn the_game_s_own_update_is_not_a_pack() {
        let dir = game(&[]);
        std::fs::write(dir.path().join("DLC/dlc/dlc.big"), vec![0; 16]).unwrap();
        assert_eq!(states(dir.path()), vec![State::Missing; 3]);
    }

    #[test]
    fn a_pack_that_cant_load_is_incomplete() {
        let dir = game(&["dlc1", "dlc2", "dlc3"]);
        std::fs::remove_file(dir.path().join("DLC/dlc1/dlc1.big")).unwrap();
        std::fs::File::create(dir.path().join("DLC/dlc2/dlc2.big")).unwrap().set_len(10).unwrap();
        std::fs::remove_file(dir.path().join("DLC/dlc3/dlc.ini")).unwrap();
        let s = states(dir.path());
        assert!(matches!(&s[0], State::Incomplete(w) if w.contains("dlc1.big is missing")));
        assert!(matches!(&s[1], State::Incomplete(w) if w.contains("only 10 bytes")));
        assert!(matches!(&s[2], State::Incomplete(w) if w.contains("dlc.ini is missing")));
        let w = warning(&scan(dir.path())).unwrap();
        assert_eq!(w.lines().count(), 3);
        assert!(!w.contains("Not installed"));
    }

    #[test]
    fn the_warning_says_what_a_missing_pack_keeps_you_out_of() {
        let w = warning(&scan(game(&["dlc2"]).path())).unwrap();
        assert!(w.starts_with("Not installed: Arctic Strike and Khyber Strike."), "{w}");
        assert!(w.contains("Stockade") && w.contains("Takeover"), "{w}");
        assert!(w.contains("have them"), "{w}");
        let w = warning(&scan(game(&["dlc1", "dlc2"]).path())).unwrap();
        assert!(w.starts_with("Not installed: Khyber Strike."), "{w}");
        assert!(w.contains("Takeover") && !w.contains("Stockade"), "{w}");
        assert!(w.contains("have it"), "{w}");
        let w = warning(&scan(game(&[]).path())).unwrap();
        assert!(w.starts_with("Not installed: Arctic Strike, Raven Strike and Khyber Strike."), "{w}");
    }

    #[test]
    fn the_warning_never_says_where_to_get_a_pack() {
        for packs in [&[][..], &["dlc1"], &["dlc3"]] {
            let w = warning(&scan(game(packs).path())).unwrap().to_lowercase();
            for word in ["download", "buy", "store", "http", "get it"] {
                assert!(!w.contains(word), "{word} in {w}");
            }
        }
    }
}
