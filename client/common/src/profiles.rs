//! The game's own profiles, which hold its options, key bindings and campaign saves.
//!
//! The game keeps them under the player's Documents folder (wherever Windows has moved it, such
//! as OneDrive): `Documents\Ubisoft\Tom Clancy's Ghost Recon Future Soldier\profiles\`. It names
//! each profile's folder after the account it signs in with, and the first time it finds no
//! profile there it asks for a name ("Enter Name"). A profile is a folder per profile ID (a
//! number) holding `profile.dat`, the profile's name (at byte 4, up to 12 characters as the game
//! takes them, and a flag at byte 68), with its settings (`Settings.ini`, `controls.ini`,
//! `data0` to `data2`) and campaign saves (`SavedGame`) beside it, which the game makes itself.
//!
//! So that the game never asks, the client gives a new account a profile before the game looks
//! ([`ensure`]): a copy of the profile last played on this PC, given the account's name (the
//! player's options and campaign come along, from Ubisoft's own days too), or a new one named
//! after the account. The launcher shows them, backs them up and puts them back.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

/// Under the Documents folder.
pub const DIR: &str = r"Ubisoft\Tom Clancy's Ghost Recon Future Soldier\profiles";
/// The longest name the game takes for a profile.
pub const MAX_NAME: usize = 12;
/// The file that makes a folder a profile.
pub const PROFILE_FILE: &str = "profile.dat";
/// `profile.dat`'s size, its name's place and its flag's.
const SIZE: usize = 69;
const NAME_AT: usize = 4;
const NAME_SPACE: usize = 64;
const FLAG_AT: usize = 68;

/// The game's profiles folder for this Windows user, or `None` off Windows (or without a
/// Documents folder).
#[must_use]
pub fn root() -> Option<PathBuf> {
    documents().map(|d| d.join(DIR))
}

#[cfg(windows)]
fn documents() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::UI::Shell::SHGetFolderPathW;
    /// `CSIDL_PERSONAL`: the Documents folder, as the game asks for it.
    const CSIDL_PERSONAL: i32 = 5;
    let mut buf = [0u16; 260];
    // SAFETY: a buffer of MAX_PATH wide characters, as the call takes.
    let result = unsafe { SHGetFolderPathW(std::ptr::null_mut(), CSIDL_PERSONAL, std::ptr::null_mut(), 0, buf.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buf[..len])))
}

#[cfg(not(windows))]
fn documents() -> Option<PathBuf> {
    None
}

/// A profile `profile.dat` names `name` (cut to [`MAX_NAME`] characters).
#[must_use]
pub fn profile_dat(name: &str) -> Vec<u8> {
    let mut data = vec![0u8; SIZE];
    let name: Vec<u8> = name.bytes().filter(|b| b.is_ascii_graphic() || *b == b' ').take(MAX_NAME).collect();
    data[NAME_AT..NAME_AT + name.len()].copy_from_slice(&name);
    data[FLAG_AT] = 1;
    data
}

/// The name in a `profile.dat`, if it reads as one.
#[must_use]
pub fn name_in(data: &[u8]) -> Option<String> {
    let field = data.get(NAME_AT..NAME_AT + NAME_SPACE)?;
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    let name = std::str::from_utf8(&field[..end]).ok()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// A folder name the game would make for `account` (its name as it is), unless it can't be one.
fn folder_for(account: &str) -> Option<&str> {
    let ok =
        !account.is_empty() && account != "." && account != ".." && !account.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control());
    ok.then_some(account)
}

/// A profile, as found in the profiles folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Its folder's name: the account the game played it with.
    pub account: String,
    /// The name it was given (in the game's Profiles screen).
    pub name: Option<String>,
    /// When it was last written.
    pub last_played: Option<SystemTime>,
    /// Whether it holds campaign saves.
    pub campaign: bool,
}

/// The `profile.dat` files in a profile's folder (one per profile ID).
fn profile_files(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else { return Vec::new() };
    entries.flatten().map(|e| e.path().join(PROFILE_FILE)).filter(|p| p.is_file()).collect()
}

/// The newest time anything in `dir` was written, looking a few folders down.
fn newest(dir: &Path, depth: u32) -> Option<SystemTime> {
    let mut newest = fs::metadata(dir).and_then(|m| m.modified()).ok();
    if depth == 0 {
        return newest;
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let time = if entry.path().is_dir() {
            newest_in(&entry.path(), depth - 1)
        } else {
            entry.metadata().and_then(|m| m.modified()).ok()
        };
        newest = newest.max(time);
    }
    newest
}

fn newest_in(dir: &Path, depth: u32) -> Option<SystemTime> {
    newest(dir, depth)
}

/// Whether a profile's folder holds campaign saves (a `SavedGame` folder with something in it).
fn has_campaign(folder: &Path) -> bool {
    fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| fs::read_dir(e.path().join("SavedGame")).is_ok_and(|mut d| d.next().is_some()))
}

/// The profiles in `root`, the most recently played first.
#[must_use]
pub fn list(root: &Path) -> Vec<Profile> {
    let mut found: Vec<Profile> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let files = profile_files(&e.path());
            let first = files.first()?;
            Some(Profile {
                account: e.file_name().to_string_lossy().into_owned(),
                name: fs::read(first).ok().as_deref().and_then(name_in),
                last_played: newest(&e.path(), 3),
                campaign: has_campaign(&e.path()),
            })
        })
        .collect();
    found.sort_by(|a, b| b.last_played.cmp(&a.last_played));
    found
}

/// What [`ensure`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ensured {
    /// The account has a profile already.
    Present,
    /// It was given a copy of the profile of `from` (the one last played on this PC).
    Copied { from: String },
    /// It was given a new profile.
    Made,
}

/// Makes sure `account` has a profile in `root` before the game looks, so it doesn't ask for a
/// name: a copy of the profile last played here, or a new one, named after the account.
///
/// # Errors
/// The account's name can't be a folder's, or the profile couldn't be written.
pub fn ensure(root: &Path, account: &str) -> io::Result<Ensured> {
    let folder = folder_for(account).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the account's name can't be a folder's"))?;
    let target = root.join(folder);
    if !profile_files(&target).is_empty() {
        return Ok(Ensured::Present);
    }
    fs::create_dir_all(root)?;
    if let Some(from) = list(root).into_iter().find(|p| p.account != folder) {
        copy_dir(&root.join(&from.account), &target)?;
        rename(&target, account)?;
        return Ok(Ensured::Copied { from: from.account });
    }
    let id = new_id();
    fs::create_dir_all(target.join(&id))?;
    fs::write(target.join(id).join(PROFILE_FILE), profile_dat(account))?;
    Ok(Ensured::Made)
}

/// Puts the profile of `from` in place of `account`'s (its own moved to `backups` first, in a
/// folder named after it and the time), named after the account.
///
/// # Errors
/// Either can't be a folder's name, or copying failed.
pub fn use_for(root: &Path, from: &str, account: &str, backups: &Path) -> io::Result<()> {
    let (Some(from), Some(folder)) = (folder_for(from), folder_for(account)) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a profile's name"));
    };
    if from == folder {
        return Ok(());
    }
    let target = root.join(folder);
    if target.exists() {
        fs::create_dir_all(backups)?;
        let kept = backups.join(format!("{folder} before {}", stamp()));
        copy_dir(&target, &kept)?;
        fs::remove_dir_all(&target)?;
    }
    copy_dir(&root.join(from), &target)?;
    rename(&target, account)
}

/// Gives every profile in `folder` the name `account`.
fn rename(folder: &Path, account: &str) -> io::Result<()> {
    for file in profile_files(folder) {
        let mut data = fs::read(&file)?;
        if data.len() < SIZE {
            data = profile_dat(account);
        } else {
            data[NAME_AT..NAME_AT + NAME_SPACE].fill(0);
            let fresh = profile_dat(account);
            data[NAME_AT..NAME_AT + MAX_NAME].copy_from_slice(&fresh[NAME_AT..NAME_AT + MAX_NAME]);
        }
        fs::write(file, data)?;
    }
    Ok(())
}

/// Copies the folder `from` to `to`, everything in it.
///
/// # Errors
/// Reading or writing failed.
pub fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()))?;
        } else {
            fs::copy(&path, to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// A new profile ID: a seven-digit number, as the game's are.
fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() ^ (d.as_secs() as u32));
    (1_000_000 + nanos % 9_000_000).to_string()
}

/// The time now (UTC, as it says), for a folder's name: `2026-11-27 1130 UTC`.
#[must_use]
pub fn stamp() -> String {
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (hour, minute) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}{minute:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the game writes it (the Surface's own, its name aside).
    #[test]
    fn profile_dat_as_the_game_has_it() {
        let data = profile_dat("SurfaceTester");
        assert_eq!(data.len(), 69);
        assert_eq!(&data[..4], &[0, 0, 0, 0]);
        assert_eq!(&data[4..16], b"SurfaceTeste", "the game's 12 characters");
        assert!(data[16..68].iter().all(|b| *b == 0));
        assert_eq!(data[68], 1);
        assert_eq!(name_in(&data).as_deref(), Some("SurfaceTeste"));
        assert_eq!(name_in(&[0u8; 69]), None);
        assert_eq!(name_in(b"short"), None);
    }

    fn profile(root: &Path, account: &str, name: &str, campaign: bool) {
        let id = root.join(account).join("5505194");
        fs::create_dir_all(id.join("SavedGame")).unwrap();
        fs::write(id.join(PROFILE_FILE), profile_dat(name)).unwrap();
        fs::write(id.join("Settings.ini"), "[GRFS_GameOptions]\r\nSubtitles=1\r\n").unwrap();
        if campaign {
            fs::write(id.join("SavedGame").join("save0"), b"campaign").unwrap();
        }
    }

    #[test]
    fn a_new_account_gets_a_new_profile_named_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profiles");
        assert_eq!(ensure(&root, "Kozak").unwrap(), Ensured::Made);
        let found = list(&root);
        assert_eq!((found.len(), found[0].account.as_str(), found[0].name.as_deref()), (1, "Kozak", Some("Kozak")));
        assert_eq!(ensure(&root, "Kozak").unwrap(), Ensured::Present, "once");
    }

    #[test]
    fn a_new_account_gets_the_profile_last_played_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profiles");
        profile(&root, "UbisoftName", "oldname", true);
        assert_eq!(ensure(&root, "Kozak").unwrap(), Ensured::Copied { from: "UbisoftName".into() });
        let kozak = root.join("Kozak").join("5505194");
        assert_eq!(name_in(&fs::read(kozak.join(PROFILE_FILE)).unwrap()).as_deref(), Some("Kozak"));
        assert!(
            kozak.join("Settings.ini").is_file() && kozak.join("SavedGame").join("save0").is_file(),
            "options and campaign come along"
        );
        assert!(root.join("UbisoftName").join("5505194").join(PROFILE_FILE).is_file(), "the old one stays");
        let found = list(&root);
        assert!(found.iter().all(|p| p.campaign));
    }

    #[test]
    fn an_empty_folder_the_game_made_isnt_a_profile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profiles");
        fs::create_dir_all(root.join("Kozak")).unwrap();
        assert_eq!(ensure(&root, "Kozak").unwrap(), Ensured::Made);
    }

    #[test]
    fn using_another_profile_keeps_the_one_it_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profiles");
        let backups = dir.path().join("backups");
        profile(&root, "Kozak", "Kozak", false);
        profile(&root, "UbisoftName", "oldname", true);
        use_for(&root, "UbisoftName", "Kozak", &backups).unwrap();
        assert!(has_campaign(&root.join("Kozak")));
        let kept: Vec<_> = fs::read_dir(&backups).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(kept.len() == 1 && kept[0].starts_with("Kozak before "), "{kept:?}");
    }

    #[test]
    fn names_that_cant_be_folders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["", "..", "a/b", "a\\b", "c:x"] {
            assert!(ensure(dir.path(), bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn stamps_the_time() {
        assert_eq!(stamp().len(), "2026-11-27 1130 UTC".len());
        assert!(stamp().ends_with(" UTC"));
    }
}
