//! The game's saves, kept on this PC: what Orbit kept in Ubisoft's cloud.
//!
//! Future Soldier keeps one small save through Orbit (its session's settings, under the PC's
//! name). Each account has its own folder: `%APPDATA%\Cross-Com\Saves\<account>\<product>-<savegame>.sav`.
//! The game's profile itself (the name it asks for on the first start, its options and campaign)
//! is in the Documents folder: see `profiles`.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

/// The folder under `%APPDATA%`.
pub const DIR_NAME: &str = "Cross-Com";

/// The largest save kept. The game's is 812 bytes; this only stops a runaway writer.
pub const MAX_SIZE: usize = 1 << 20;

/// Where one account's saves live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saves {
    dir: PathBuf,
}

impl Saves {
    /// `%APPDATA%\Cross-Com\Saves\<account>`, or `None` without `APPDATA`.
    #[must_use]
    pub fn for_account(account: &str) -> Option<Self> {
        let app_data = std::env::var_os("APPDATA")?;
        Some(Self::in_dir(Path::new(&app_data).join(DIR_NAME).join("Saves"), account))
    }

    /// The saves of `account` under `root`.
    #[must_use]
    pub fn in_dir(root: PathBuf, account: &str) -> Self {
        Self {
            dir: root.join(folder_name(account)),
        }
    }

    #[must_use]
    pub fn path(&self, product: u32, savegame: u32) -> PathBuf {
        self.dir.join(format!("{product}-{savegame}.sav"))
    }

    /// The save, or `None` when there's none yet.
    ///
    /// # Errors
    /// Reading it failed, or it's larger than [`MAX_SIZE`].
    pub fn read(&self, product: u32, savegame: u32) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(product, savegame)) {
            Ok(data) if data.len() > MAX_SIZE => Err(io::Error::new(io::ErrorKind::InvalidData, "the save is too large")),
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Replaces the save: written beside it first, then moved over it, so a crash mid-write
    /// leaves the old one.
    ///
    /// # Errors
    /// Writing failed, or `data` is larger than [`MAX_SIZE`].
    pub fn write(&self, product: u32, savegame: u32, data: &[u8]) -> io::Result<()> {
        if data.len() > MAX_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the save is too large"));
        }
        fs::create_dir_all(&self.dir)?;
        let path = self.path(product, savegame);
        let new = path.with_extension("sav.new");
        fs::write(&new, data)?;
        fs::rename(&new, &path)
    }
}

/// A folder name for an account: the name's letters, digits, `-`, `_` and `.` (as account names
/// are), anything else as `_`, and never empty, `.` or `..`.
fn folder_name(account: &str) -> String {
    let name: String = account
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        "_".repeat(name.len().max(1))
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crosscom-saves-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_save_is_none_and_a_written_one_reads_back() {
        let root = temp_root("roundtrip");
        let saves = Saves::in_dir(root.clone(), "Kozak");
        assert_eq!(saves.read(53, 1).unwrap(), None);
        saves.write(53, 1, b"profile").unwrap();
        assert_eq!(saves.read(53, 1).unwrap().as_deref(), Some(&b"profile"[..]));
        saves.write(53, 1, b"newer").unwrap();
        assert_eq!(saves.read(53, 1).unwrap().as_deref(), Some(&b"newer"[..]));
        assert!(!saves.path(53, 1).with_extension("sav.new").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn accounts_have_their_own_folders() {
        let root = temp_root("accounts");
        let a = Saves::in_dir(root.clone(), "alpha");
        let b = Saves::in_dir(root.clone(), "bravo");
        a.write(53, 1, b"a").unwrap();
        assert_eq!(b.read(53, 1).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_names_stay_inside_the_saves_folder() {
        assert_eq!(folder_name("Kozak.1_x-y"), "Kozak.1_x-y");
        assert_eq!(folder_name("../evil"), ".._evil");
        assert_eq!(folder_name(".."), "__");
        assert_eq!(folder_name("."), "_");
        assert_eq!(folder_name(""), "_");
        assert_eq!(folder_name("a\\b:c"), "a_b_c");
    }

    #[test]
    fn oversized_saves_are_refused() {
        let root = temp_root("big");
        let saves = Saves::in_dir(root.clone(), "big");
        assert!(saves.write(53, 1, &vec![0; MAX_SIZE + 1]).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
