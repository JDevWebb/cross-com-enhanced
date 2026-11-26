//! The Windows the client runs on, for the logs: its edition, release and build, and Wine's
//! version under Wine.

/// `Windows 11 Pro 23H2 (build 22631)`, with ` under Wine 9.0` under Wine. `None` off Windows.
#[cfg(windows)]
#[must_use]
pub fn os_version() -> Option<String> {
    const KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let product = registry_string(KEY, "ProductName");
    let release = registry_string(KEY, "DisplayVersion").or_else(|| registry_string(KEY, "ReleaseId"));
    let build = registry_string(KEY, "CurrentBuildNumber").or_else(|| registry_string(KEY, "CurrentBuild"));
    let mut text = describe(product.as_deref(), release.as_deref(), build.as_deref())?;
    if let Some(wine) = wine_version() {
        text.push_str(&format!(" under Wine {wine}"));
    }
    Some(text)
}

#[cfg(not(windows))]
#[must_use]
pub fn os_version() -> Option<String> {
    None
}

/// The version from the registry's values. Windows 11 still calls itself Windows 10 there; its
/// builds start at 22000.
#[cfg_attr(not(windows), allow(dead_code))]
fn describe(product: Option<&str>, release: Option<&str>, build: Option<&str>) -> Option<String> {
    let mut product = product?.trim().to_owned();
    if build.and_then(|b| b.trim().parse::<u32>().ok()).is_some_and(|b| b >= 22_000) {
        product = product.replacen("Windows 10", "Windows 11", 1);
    }
    let mut text = product;
    if let Some(release) = release.map(str::trim).filter(|r| !r.is_empty()) {
        text.push_str(&format!(" {release}"));
    }
    if let Some(build) = build.map(str::trim).filter(|b| !b.is_empty()) {
        text.push_str(&format!(" (build {build})"));
    }
    Some(text)
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A string value under `HKEY_LOCAL_MACHINE`, from the registry's 64-bit view. The game is a
/// 32-bit program, so its plain reads go to the 32-bit view (`WOW6432Node`), whose `ProductName`
/// Windows 10 leaves at its install image's ("Windows 10 Enterprise" on a Pro machine). 32-bit
/// Windows has only the one view, and a plain read is the fallback for anything refusing the flag.
#[cfg(windows)]
fn registry_string(key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::RRF_SUBKEY_WOW6464KEY;

    let (key, value) = (wide(key), wide(value));
    registry_read(&key, &value, RRF_SUBKEY_WOW6464KEY).or_else(|| registry_read(&key, &value, 0))
}

/// One `RegGetValueW` of a string, with `view` added to its flags.
#[cfg(windows)]
fn registry_read(key: &[u16], value: &[u16], view: u32) -> Option<String> {
    use windows_sys::Win32::System::Registry::RegGetValueW;
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows_sys::Win32::System::Registry::RRF_RT_REG_SZ;

    let mut buf = [0u16; 256];
    let mut size = u32::try_from(std::mem::size_of_val(&buf)).ok()?;
    // SAFETY: NUL-terminated names, and the buffer with its size in bytes; the call writes at most that.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | view,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let len = (size as usize / 2).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..len]);
    Some(text.trim_end_matches('\0').to_owned()).filter(|t| !t.is_empty())
}

/// Wine's version, when running under Wine (its `ntdll` has `wine_get_version`).
#[cfg(windows)]
fn wine_version() -> Option<String> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;

    let ntdll = wide("ntdll.dll");
    // SAFETY: a NUL-terminated name; ntdll is always loaded and never freed.
    let module = unsafe { GetModuleHandleW(ntdll.as_ptr()) };
    if module.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated name.
    let proc = unsafe { GetProcAddress(module, c"wine_get_version".as_ptr().cast()) }?;
    // SAFETY: Wine's `const char *wine_get_version(void)`, a static string.
    let get: unsafe extern "C" fn() -> *const std::ffi::c_char = unsafe { std::mem::transmute(proc) };
    let version = unsafe { get() };
    if version.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated static string.
    Some(unsafe { std::ffi::CStr::from_ptr(version) }.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_11_is_known_by_its_build() {
        assert_eq!(
            describe(Some("Windows 10 Pro"), Some("23H2"), Some("22631")).as_deref(),
            Some("Windows 11 Pro 23H2 (build 22631)")
        );
        assert_eq!(
            describe(Some("Windows 10 Home"), Some("22H2"), Some("19045")).as_deref(),
            Some("Windows 10 Home 22H2 (build 19045)")
        );
        assert_eq!(describe(Some("Windows 7 Ultimate"), None, Some("7601")).as_deref(), Some("Windows 7 Ultimate (build 7601)"));
        assert_eq!(describe(None, None, None), None);
    }
}
