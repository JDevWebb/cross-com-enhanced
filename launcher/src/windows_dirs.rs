//! Where Windows' own programs are, so the launcher starts them by their full path: a program
//! started by its bare name is looked for in the current folder and the launcher's own folder
//! first, where a file put beside a downloaded launcher would be run in its place.
//!
//! The folders come from Windows itself (`GetSystemDirectoryW`, `GetSystemWindowsDirectoryW`),
//! not from environment variables.

use std::path::Path;
use std::path::PathBuf;

/// `name` in Windows' system folder (`C:\Windows\System32`), such as `netsh.exe`.
pub fn system_program(name: &str) -> Result<PathBuf, String> {
    under(system_dir(), name)
}

/// `name` in the Windows folder (`C:\Windows`), such as `explorer.exe`.
pub fn windows_program(name: &str) -> Result<PathBuf, String> {
    under(windows_dir(), name)
}

/// `name` in `dir`, when `dir` is a full path and `name` a plain file name (or a path below it).
fn under(dir: Option<PathBuf>, name: &str) -> Result<PathBuf, String> {
    let dir = dir.filter(|d| is_full(d)).ok_or_else(|| format!("couldn't find Windows' folder for {name}"))?;
    let plain = !name.is_empty() && !name.contains(':') && !name.starts_with(['\\', '/']) && !name.split(['\\', '/']).any(|p| p.is_empty() || p == "..");
    if !plain {
        return Err(format!("{name} isn't a program name"));
    }
    Ok(dir.join(name))
}

/// Whether `dir` is a full Windows path (`C:\...`) or, off Windows, an absolute one.
fn is_full(dir: &Path) -> bool {
    let text = dir.to_string_lossy();
    let b = text.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')) || (!cfg!(windows) && dir.is_absolute())
}

#[cfg(windows)]
fn from_windows(get: unsafe extern "system" fn(*mut u16, u32) -> u32) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let mut buf = vec![0u16; 260];
    loop {
        let size = u32::try_from(buf.len()).ok()?;
        // SAFETY: the buffer is `size` wide characters long.
        let n = unsafe { get(buf.as_mut_ptr(), size) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return Some(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])));
        }
        // Too small: `n` is the size needed.
        buf.resize(n + 1, 0);
    }
}

#[cfg(windows)]
fn system_dir() -> Option<PathBuf> {
    from_windows(windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW)
}

#[cfg(windows)]
fn windows_dir() -> Option<PathBuf> {
    // The shared Windows folder, not a Terminal Services user's own.
    from_windows(windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW)
}

#[cfg(not(windows))]
fn system_dir() -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
fn windows_dir() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_go_in_windows_folders_only() {
        let dir = || Some(PathBuf::from(r"C:\Windows\System32"));
        assert_eq!(under(dir(), "netsh.exe"), Ok(PathBuf::from(r"C:\Windows\System32").join("netsh.exe")));
        assert!(under(dir(), r"WindowsPowerShell\v1.0\powershell.exe").is_ok());
        for bad in ["", r"..\evil.exe", r"C:\evil.exe", r"\\server\share\x.exe", "/x", r"a\\b"] {
            assert!(under(dir(), bad).is_err(), "{bad:?}");
        }
        assert!(under(None, "netsh.exe").is_err(), "Windows didn't say");
        assert!(under(Some(PathBuf::from(r"System32")), "netsh.exe").is_err(), "not a full path");
    }
}
