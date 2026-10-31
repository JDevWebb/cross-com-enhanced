//! The game's folder: finding it, and putting Cross-Com's client DLLs into it.
//!
//! The game loads `ubiorbitapi_r2_loader.dll` and `uplay_r1_loader.dll` from its folder.
//! Installing copies Cross-Com's over them, first keeping the game's own as `*.orig` (once: a
//! later install never overwrites the originals). Restoring puts the originals back.

use std::path::Path;
use std::path::PathBuf;

/// The DLLs Cross-Com replaces.
pub const CLIENT_DLLS: [&str; 2] = ["ubiorbitapi_r2_loader.dll", "uplay_r1_loader.dll"];
/// The game's executables: DirectX 11, and DirectX 9 for older cards.
pub const EXE_DX11: &str = "Future Soldier DX11.exe";
pub const EXE_DX9: &str = "Future Soldier DX9.exe";
/// Where the game keeps its originals of the DLLs.
const ORIGINAL_SUFFIX: &str = ".orig";

/// Whether `dir` holds the game.
#[must_use]
pub fn is_game_dir(dir: &Path) -> bool {
    dir.join(EXE_DX11).is_file() || dir.join(EXE_DX9).is_file()
}

/// Folders the game is often installed in, under each of `roots` (drive roots and Program Files
/// folders): Ubisoft Connect and Steam libraries.
#[must_use]
pub fn usual_places(roots: &[PathBuf]) -> Vec<PathBuf> {
    const UNDER: [&str; 6] = [
        "Ubisoft/Ubisoft Game Launcher/games/Tom Clancy's Ghost Recon Future Soldier",
        "Ubisoft Game Launcher/games/Tom Clancy's Ghost Recon Future Soldier",
        "Steam/steamapps/common/Ghost Recon Future Soldier",
        "Steam/steamapps/common/Tom Clancy's Ghost Recon Future Soldier",
        "SteamLibrary/steamapps/common/Ghost Recon Future Soldier",
        "Games/Tom Clancy's Ghost Recon Future Soldier",
    ];
    roots.iter().flat_map(|root| UNDER.iter().map(move |u| root.join(u))).collect()
}

/// The first of `candidates` that holds the game.
#[must_use]
pub fn find(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|d| is_game_dir(d)).cloned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// The game's own DLLs.
    No,
    /// Cross-Com's, as shipped with this launcher.
    Yes,
    /// Cross-Com's, but different from this launcher's (an older or newer version).
    Other,
}

/// Whether Cross-Com's client is in `game`, compared with the DLLs in `client`.
#[must_use]
pub fn installed(game: &Path, client: &Path) -> Installed {
    let backed_up = CLIENT_DLLS.iter().all(|d| original(game, d).is_file());
    if !backed_up {
        return Installed::No;
    }
    let same = CLIENT_DLLS.iter().all(|d| match (std::fs::read(game.join(d)), std::fs::read(client.join(d))) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    });
    if same {
        Installed::Yes
    } else {
        Installed::Other
    }
}

fn original(game: &Path, dll: &str) -> PathBuf {
    game.join(format!("{dll}{ORIGINAL_SUFFIX}"))
}

/// Copies Cross-Com's DLLs from `client` into `game`, keeping the game's own first.
pub fn install(game: &Path, client: &Path) -> Result<(), String> {
    if !is_game_dir(game) {
        return Err(format!("{} isn't the game's folder", game.display()));
    }
    for dll in CLIENT_DLLS {
        if !client.join(dll).is_file() {
            return Err(format!("{dll} is missing next to the launcher"));
        }
    }
    for dll in CLIENT_DLLS {
        let (current, kept) = (game.join(dll), original(game, dll));
        if current.is_file() && !kept.exists() {
            std::fs::copy(&current, &kept).map_err(|e| format!("couldn't keep the game's {dll}: {e}"))?;
        }
        std::fs::copy(client.join(dll), &current).map_err(|e| format!("couldn't install {dll}: {e} (is the game running?)"))?;
    }
    Ok(())
}

/// Puts the game's own DLLs back.
pub fn restore(game: &Path) -> Result<(), String> {
    for dll in CLIENT_DLLS {
        let kept = original(game, dll);
        if kept.is_file() {
            std::fs::copy(&kept, game.join(dll)).map_err(|e| format!("couldn't restore {dll}: {e} (is the game running?)"))?;
            std::fs::remove_file(&kept).map_err(|e| format!("couldn't tidy up {}: {e}", kept.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(EXE_DX11), b"exe").unwrap();
        for dll in CLIENT_DLLS {
            std::fs::write(dir.path().join(dll), b"ubisoft").unwrap();
        }
        dir
    }

    fn client(contents: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for dll in CLIENT_DLLS {
            std::fs::write(dir.path().join(dll), contents).unwrap();
        }
        dir
    }

    #[test]
    fn installs_keeping_the_originals_once_and_restores_them() {
        let (game, ours) = (game(), client(b"cross-com 1"));
        assert_eq!(installed(game.path(), ours.path()), Installed::No);
        install(game.path(), ours.path()).unwrap();
        assert_eq!(installed(game.path(), ours.path()), Installed::Yes);
        assert_eq!(std::fs::read(game.path().join(CLIENT_DLLS[0])).unwrap(), b"cross-com 1");

        // An update: the originals stay the game's.
        let newer = client(b"cross-com 2");
        assert_eq!(installed(game.path(), newer.path()), Installed::Other);
        install(game.path(), newer.path()).unwrap();
        assert_eq!(std::fs::read(original(game.path(), CLIENT_DLLS[1])).unwrap(), b"ubisoft");

        restore(game.path()).unwrap();
        assert_eq!(std::fs::read(game.path().join(CLIENT_DLLS[0])).unwrap(), b"ubisoft");
        assert_eq!(installed(game.path(), newer.path()), Installed::No);
    }

    #[test]
    fn refuses_what_isnt_the_game_or_a_client() {
        let not_game = tempfile::tempdir().unwrap();
        assert!(install(not_game.path(), client(b"x").path()).is_err());
        let empty = tempfile::tempdir().unwrap();
        let game = game();
        assert!(install(game.path(), empty.path()).is_err());
        assert_eq!(std::fs::read(game.path().join(CLIENT_DLLS[0])).unwrap(), b"ubisoft", "nothing touched");
    }

    #[test]
    fn finds_the_game_in_the_usual_places() {
        let root = tempfile::tempdir().unwrap();
        let places = usual_places(&[root.path().to_path_buf()]);
        assert!(find(&places).is_none());
        let dir = root.path().join("Steam/steamapps/common/Ghost Recon Future Soldier");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(EXE_DX9), b"exe").unwrap();
        assert_eq!(find(&places), Some(dir));
    }
}
