//! The player's identity (see the `crosscom-identity` crate): made once, kept in the launcher's
//! own folder, and moved to another PC as text. It's what signs the player in: there's no
//! password.
//!
//! On Windows the key is encrypted for the Windows user (DPAPI); elsewhere, and under Wine, the
//! file is readable by the user only. Whoever has the key can sign in to every account it made,
//! so its exported text is a secret too. Ported from 5th Echelon Enhanced's launcher.

use std::path::Path;
use std::path::PathBuf;

pub use crosscom_identity::Identity;

/// The key's file, in the launcher's folder.
pub const FILE: &str = "identity.key";
/// What an exported identity starts with.
const EXPORT_PREFIX: &str = "cross-com-identity:v1:";
/// What an encrypted key file starts with.
const DPAPI_PREFIX: &str = "dpapi:";

/// Where the key is kept in the launcher's folder `dir`.
#[must_use]
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The player's identity, made the first time.
pub fn load_or_create(dir: &Path) -> Result<Identity, String> {
    if let Some(identity) = load(dir)? {
        return Ok(identity);
    }
    let identity = Identity::generate();
    save(&path(dir), &identity)?;
    Ok(identity)
}

/// The saved identity, if there is one.
pub fn load(dir: &Path) -> Result<Option<Identity>, String> {
    load_at(&path(dir))
}

fn load_at(path: &Path) -> Result<Option<Identity>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("couldn't read your identity ({}): {e}", path.display())),
    };
    let text = text.trim();
    let secret = match text.strip_prefix(DPAPI_PREFIX) {
        Some(blob) => protect::unprotect(blob).ok_or_else(|| {
            format!(
                "your identity ({}) is encrypted for another Windows user or PC: use it again from where you copied it (Settings › Server and account)",
                path.display()
            )
        })?,
        None => text.to_owned(),
    };
    parse_secret(&secret).map(Some)
}

fn save(path: &Path, identity: &Identity) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    }
    let secret = crosscom_identity::base32_encode(&identity.secret());
    let text = match protect::protect(&secret) {
        Some(blob) => format!("{DPAPI_PREFIX}{blob}\n"),
        None => format!("{secret}\n"),
    };
    write_private(path, text.as_bytes()).map_err(|e| format!("couldn't save your identity ({}): {e}", path.display()))
}

/// Saves `identity` as this PC's, first keeping a different one it replaces as
/// `identity.<its short id>.key` beside it (encrypted as it was), so using the wrong PC's
/// identity can be undone. That file, when one was kept.
pub fn replace(dir: &Path, identity: &Identity) -> Result<Option<PathBuf>, String> {
    let path = path(dir);
    let kept = keep_replaced(&path, identity)?;
    save(&path, identity)?;
    Ok(kept)
}

fn keep_replaced(path: &Path, identity: &Identity) -> Result<Option<PathBuf>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("couldn't read your identity ({}): {e}", path.display())),
    };
    // One this PC can't read (another Windows user's) is kept as well, under a plain name.
    let name = match load_at(path) {
        Ok(Some(old)) if old.global_id() == identity.global_id() => return Ok(None),
        Ok(Some(old)) => format!("identity.{}.key", crosscom_identity::short(&old.global_id())),
        _ => "identity.unreadable.key".to_owned(),
    };
    let kept = path.with_file_name(name);
    write_private(&kept, &bytes).map_err(|e| format!("couldn't keep your old identity ({}): {e}", kept.display()))?;
    Ok(Some(kept))
}

/// The identity as the launcher shows it: its global id's first characters (`ABCD-EFGH`).
#[must_use]
pub fn short(identity: &Identity) -> String {
    crosscom_identity::short(&identity.global_id())
}

/// The identity as text, to move it to another PC.
#[must_use]
pub fn export(identity: &Identity) -> String {
    format!("{EXPORT_PREFIX}{}", crosscom_identity::base32_encode(&identity.secret()))
}

/// An identity from [`export`]'s text.
pub fn import(text: &str) -> Result<Identity, String> {
    let secret = text
        .trim()
        .strip_prefix(EXPORT_PREFIX)
        .ok_or("That isn't an identity copied from the Cross-Com launcher.")?;
    parse_secret(secret)
}

fn parse_secret(text: &str) -> Result<Identity, String> {
    let damaged = || "The identity is damaged.".to_owned();
    let bytes = crosscom_identity::base32_decode(text.trim()).ok_or_else(damaged)?;
    let secret: [u8; 32] = bytes.try_into().map_err(|_| damaged())?;
    Ok(Identity::from_secret(&secret))
}

/// Writes `data` to `path` readable by the user only (elsewhere than Windows): written to a new
/// file beside it first, then moved over it.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        std::io::Write::write_all(&mut file, data)?;
    }
    std::fs::rename(&tmp, path)
}

/// Encrypting the key for the Windows user (DPAPI), as hex.
mod protect {
    /// `secret` encrypted and in hex; none where DPAPI isn't there (or is Wine's).
    pub fn protect(secret: &str) -> Option<String> {
        #[cfg(windows)]
        {
            if running_under_wine() {
                return None;
            }
            run(secret.as_bytes(), true).map(|b| hex(&b))
        }
        #[cfg(not(windows))]
        {
            let _ = secret;
            None
        }
    }

    /// [`protect`]'s output decrypted; none if this user can't.
    pub fn unprotect(blob: &str) -> Option<String> {
        #[cfg(windows)]
        {
            run(&unhex(blob)?, false).and_then(|b| String::from_utf8(b).ok())
        }
        #[cfg(not(windows))]
        {
            let _ = blob;
            None
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn unhex(text: &str) -> Option<Vec<u8>> {
        let text = text.trim();
        if !text.len().is_multiple_of(2) {
            return None;
        }
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
    }

    /// Whether the launcher runs under Wine or Proton (its ntdll has `wine_get_version`).
    #[cfg(windows)]
    fn running_under_wine() -> bool {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
        use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
        // SAFETY: both take NUL-terminated names; ntdll is loaded in every process.
        unsafe {
            let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
            !ntdll.is_null() && GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast()).is_some()
        }
    }

    /// Encrypts (`protect`) or decrypts `data` for the Windows user.
    #[cfg(windows)]
    fn run(data: &[u8], protect: bool) -> Option<Vec<u8>> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Cryptography::CryptProtectData;
        use windows_sys::Win32::Security::Cryptography::CryptUnprotectData;
        use windows_sys::Win32::Security::Cryptography::CRYPTPROTECT_UI_FORBIDDEN;
        use windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB;

        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).ok()?,
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        use std::ptr::null;
        // SAFETY: the input points at `data`, which outlives the call, and isn't written; the
        // output is Windows' allocation, copied and then freed with LocalFree.
        unsafe {
            let done = if protect {
                CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            } else {
                CryptUnprotectData(&input, std::ptr::null_mut(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            };
            if done == 0 || output.pbData.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(output.pbData.cast());
            Some(bytes)
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn hex_round_trips() {
            let bytes = [0u8, 1, 0xab, 0xff];
            assert_eq!(super::unhex(&super::hex(&bytes)).unwrap(), bytes);
            assert!(super::unhex("abc").is_none());
            assert!(super::unhex("zz").is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn made_once_then_kept() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
        let first = load_or_create(dir.path()).unwrap();
        let again = load_or_create(dir.path()).unwrap();
        assert_eq!(first.global_id(), again.global_id());
        assert_eq!(load(dir.path()).unwrap().unwrap().global_id(), first.global_id());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path(dir.path())).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "readable by the user only");
        }
    }

    #[test]
    fn a_replaced_identity_is_kept_beside() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(replace(dir.path(), &Identity::generate()).unwrap(), None, "nothing to keep yet");
        let old = load(dir.path()).unwrap().unwrap();
        assert_eq!(replace(dir.path(), &old).unwrap(), None, "the same identity again");
        let new = Identity::generate();
        let kept = replace(dir.path(), &new).unwrap().expect("a different one keeps the old");
        assert_eq!(load_at(&kept).unwrap().unwrap().global_id(), old.global_id());
        assert_eq!(load(dir.path()).unwrap().unwrap().global_id(), new.global_id());
    }

    #[test]
    fn a_damaged_file_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "not a key\n").unwrap();
        assert!(load(dir.path()).is_err());
        assert!(load_or_create(dir.path()).is_err(), "never made again over one it can't read");
        let kept = replace(dir.path(), &Identity::generate()).unwrap().unwrap();
        assert!(kept.ends_with("identity.unreadable.key"));
    }

    #[test]
    fn export_and_import() {
        let me = Identity::generate();
        let text = export(&me);
        assert!(text.starts_with("cross-com-identity:v1:"));
        assert_eq!(import(&format!("  {text}\n")).unwrap().global_id(), me.global_id());
        assert!(import("hello").is_err());
        assert!(import("cross-com-identity:v1:AAAA").is_err());
    }
}
