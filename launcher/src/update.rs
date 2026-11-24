//! The launcher keeping itself up to date, from this project's GitHub releases.
//!
//! A release's files are listed with their SHA-256 in `SHA256SUMS`, which carries the release
//! key's signature (`SHA256SUMS.sig`, made offline by `scripts/sign-release.sh`; what's signed is
//! what `scripts/verify-release.sh` checks: a purpose line, the version, then the file). A new
//! launcher is used only when that signature is good for the release's own version and its
//! checksum matches, so someone who can change a release on GitHub still can't ship an update,
//! and an old release published again under a new tag isn't installed either. Only a release
//! newer than this launcher is installed.
//!
//! The launcher replaces itself by renaming: Windows lets a running exe be renamed, not
//! overwritten, so the new one takes its name and starts, and the old one (kept as `<name>.old`)
//! is deleted on the next start ([`clean_up`]). The new one is read back from the disk and
//! checked again just before and after it takes the name, and held open so it can't be changed
//! until it has started, so another user who can write to the launcher's folder can't swap in a
//! file of their own after the download was checked.
//!
//! The release key's public half comes from `scripts/verify-release.sh` when the launcher is
//! built (see `build.rs`); a launcher built before the key exists doesn't update itself, nor does
//! a development build (a release like `0.2.0-dev`).

use std::io::Read as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use serde::Deserialize;

/// Where releases are listed: GitHub's API for this repository (another, for a test build).
const LATEST: &str = env!("CROSSCOM_UPDATE_FROM");
/// The release key's public half (Ed25519, its SubjectPublicKeyInfo in base64), or "".
const RELEASE_KEY: &str = env!("CROSSCOM_RELEASE_KEY");
pub const RELEASES_PAGE: &str = "https://github.com/JDevWebb/cross-com-enhanced/releases";
/// What `SHA256SUMS.sig` signs, before the version.
const PURPOSE: &str = "cross-com/release/v1";
/// The launcher in a release.
pub const LAUNCHER_ASSET: &str = "crosscom-launcher.exe";
const SUMS_ASSET: &str = "SHA256SUMS";
const SIG_ASSET: &str = "SHA256SUMS.sig";
/// The largest download: the launcher, with the client in it.
const MAX_DOWNLOAD: u64 = 128 * 1024 * 1024;
/// The largest release listing, checksum file or signature.
const MAX_SMALL: u64 = 1024 * 1024;

/// A release number: `0.2.0`, or `0.2.0-dev` (a pre-release).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub numbers: (u64, u64, u64),
    pub pre: Option<String>,
}

impl Release {
    /// `0.2.0`, `v0.2.0` or `0.2.0-rc.1`; anything else is none.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
        let (numbers, pre) = match text.split_once('-') {
            Some((n, p)) => (n, Some(p)),
            None => (text, None),
        };
        let pre_ok = pre.is_none_or(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));
        let mut parts = numbers
            .split('.')
            .map(|n| (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| n.parse::<u64>().ok()).flatten());
        let (Some(Some(a)), Some(Some(b)), Some(Some(c)), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return None;
        };
        pre_ok.then(|| Release {
            numbers: (a, b, c),
            pre: pre.map(str::to_owned),
        })
    }

    /// Whether this release comes after `other`: higher numbers, or the same with `other` a
    /// pre-release and this not.
    #[must_use]
    pub fn newer_than(&self, other: &Release) -> bool {
        match self.numbers.cmp(&other.numbers) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => self.pre.is_none() && other.pre.is_some(),
        }
    }
}

/// Whether this launcher looks for updates: a release build, with the release key built in.
#[must_use]
pub fn enabled(current: &str) -> bool {
    !RELEASE_KEY.is_empty() && Release::parse(current).is_some_and(|r| r.pre.is_none())
}

/// Why this launcher doesn't update itself, when it doesn't.
#[must_use]
pub fn disabled_why(current: &str) -> Option<&'static str> {
    if Release::parse(current).is_none_or(|r| r.pre.is_some()) {
        Some("This is a development build: it doesn't update itself. Get a new one from whoever sent it, or from the releases page.")
    } else if RELEASE_KEY.is_empty() {
        Some("This launcher was built without the release key, so it can't check an update: get new releases from the releases page.")
    } else {
        None
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: String,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// The latest release.
#[derive(Debug, Clone)]
pub struct Latest {
    pub version: String,
    /// Its page, for the release notes (only this project's own pages).
    pub page: String,
    /// Its notes, as published (not signed: shown, never acted on).
    pub notes: String,
    assets: Vec<GhAsset>,
}

impl Latest {
    /// Whether it's newer than `current`, this launcher's release.
    #[must_use]
    pub fn newer_than(&self, current: &str) -> bool {
        match (Release::parse(&self.version), Release::parse(current)) {
            (Some(latest), Some(current)) => latest.newer_than(&current),
            _ => false,
        }
    }

    fn url(&self, name: &str) -> Result<&str, String> {
        let url = self
            .assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.as_str())
            .ok_or_else(|| format!("release {} has no {name}", self.version))?;
        // What's downloaded is checked against the signed checksums anyway; this keeps the
        // asking itself to where releases are (the listing isn't signed).
        if !from_releases(url) {
            return Err(format!("release {}'s {name} isn't where releases are", self.version));
        }
        Ok(url)
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(10)))
        // GitHub sends downloads on to its storage.
        .max_redirects(5)
        .user_agent(concat!("crosscom-launcher/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Downloads `url`, at most `max` bytes, within `timeout` in all.
fn get(url: &str, timeout: Duration, max: u64) -> Result<Vec<u8>, String> {
    let mut response = agent(timeout)
        .get(url)
        .header("Accept", "application/vnd.github+json, application/octet-stream;q=0.9, */*;q=0.8")
        .call()
        .map_err(|e| format!("couldn't download {url}: {e}"))?;
    let mut data = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(max + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("the download from {url} stopped: {e}"))?;
    if data.len() as u64 > max {
        return Err(format!("{url} is larger than expected"));
    }
    Ok(data)
}

/// Whether `url` is HTTPS on GitHub, where releases and their files are (or where a test build
/// was told to look).
fn from_releases(url: &str) -> bool {
    let host_of = |u: &str| {
        let rest = u.strip_prefix("https://")?;
        let host = rest.split(['/', '?', '#']).next()?;
        (!host.contains(['@', ':'])).then_some(host.to_owned())
    };
    let Some(host) = host_of(url) else {
        return false;
    };
    matches!(host.as_str(), "github.com" | "objects.githubusercontent.com" | "release-assets.githubusercontent.com")
        || (!LATEST.starts_with("https://api.github.com/") && host_of(LATEST).is_some_and(|test| test == host))
}

/// Asks GitHub for the latest release.
pub fn latest() -> Result<Latest, String> {
    let body = get(LATEST, Duration::from_secs(15), MAX_SMALL)?;
    let r: GhRelease = serde_json::from_slice(&body).map_err(|e| format!("GitHub's answer made no sense: {e}"))?;
    let version = r.tag_name.trim_start_matches('v').to_owned();
    Ok(Latest {
        page: if r.html_url.starts_with(&format!("{RELEASES_PAGE}/")) {
            r.html_url
        } else {
            RELEASES_PAGE.to_owned()
        },
        notes: r.body,
        version,
        assets: r.assets,
    })
}

/// Whether `signature` (base64) is `key`'s (an Ed25519 SubjectPublicKeyInfo, base64) over
/// release `version`'s `sums`, as `scripts/verify-release.sh` checks it.
fn signed(key: &str, version: &str, sums: &[u8], signature: &str) -> bool {
    let b64 = base64::engine::general_purpose::STANDARD;
    let (Ok(spki), Ok(signature)) = (b64.decode(key.trim()), b64.decode(signature.split_whitespace().collect::<String>())) else {
        return false;
    };
    // An Ed25519 SubjectPublicKeyInfo is 12 bytes of header, then the key's 32.
    const HEADER: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
    if spki.len() != 44 || spki[..12] != HEADER {
        return false;
    }
    let mut message = format!("{PURPOSE}\n{version}\n").into_bytes();
    message.extend_from_slice(sums);
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &spki[12..])
        .verify(&message, &signature)
        .is_ok()
}

/// `name`'s SHA-256 as `sums` lists it (`sha256sum`'s lines: the hash, two spaces or a space and
/// a star, the name).
fn checksum_for(sums: &str, name: &str) -> Option<[u8; 32]> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(' ')?;
        let file = file.trim_start_matches([' ', '*']).trim_end();
        if file != name || hash.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(hash.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(out)
    })
}

/// Whether `data` is release `version`'s `name`: `sums` signed as that version by `key`, and
/// listing `data`'s checksum, which is given back.
fn checked(key: &str, version: &str, sums: &str, signature: &str, name: &str, data: &[u8]) -> Result<[u8; 32], String> {
    if Release::parse(version).is_none() || !signed(key, version, sums.as_bytes(), signature) {
        return Err(format!("release {version} isn't signed by the project's release key as {version}; not installed"));
    }
    let want = checksum_for(sums, name).ok_or_else(|| format!("{SUMS_ASSET} doesn't list {name}; not installed"))?;
    let got = ring::digest::digest(&ring::digest::SHA256, data);
    if got.as_ref() != want {
        return Err(format!("{name} doesn't match its published checksum; not installed"));
    }
    Ok(want)
}

/// Opens `path` for reading, and on Windows so that nothing else can write it, delete it or
/// rename it while the file is open, and checks its SHA-256 is `want`. The open file is given
/// back: holding it keeps the checked contents at that path.
fn open_checked(path: &Path, want: &[u8; 32]) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ alone: others may read it (and Windows run it), not change it.
        options.share_mode(0x1);
    }
    let mut file = options.open(path).map_err(|e| format!("couldn't read {} back: {e}", path.display()))?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("couldn't read {} back: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        context.update(&buf[..n]);
    }
    if context.finish().as_ref() != want {
        return Err(format!("{} changed after it was checked; not installed", path.display()));
    }
    Ok(file)
}

/// Downloads `name` from the release, checked, to `to` (through a temporary file beside it, so
/// a failed download leaves nothing behind); its SHA-256, as the signed checksums list it.
fn download(latest: &Latest, name: &str, to: &Path) -> Result<[u8; 32], String> {
    let sums = get(latest.url(SUMS_ASSET)?, Duration::from_secs(30), MAX_SMALL)?;
    let sums = String::from_utf8(sums).map_err(|_| format!("{SUMS_ASSET} isn't text"))?;
    let sig = latest.url(SIG_ASSET).map_err(|_| format!("release {} isn't signed yet; try again later", latest.version))?;
    let sig = String::from_utf8(get(sig, Duration::from_secs(30), MAX_SMALL)?).map_err(|_| format!("{SIG_ASSET} isn't text"))?;
    // The tag's version, signed with the files: checked before anything big is downloaded.
    if !signed(RELEASE_KEY, &latest.version, sums.as_bytes(), &sig) {
        return Err(format!(
            "release {} isn't signed by the project's release key as {}; not installed",
            latest.version, latest.version
        ));
    }
    let data = get(latest.url(name)?, Duration::from_secs(300), MAX_DOWNLOAD)?;
    let want = checked(RELEASE_KEY, &latest.version, &sums, &sig, name, &data)?;
    install(&data, to).map_err(|e| format!("couldn't write {}: {e}", to.display()))?;
    Ok(want)
}

/// Writes `data` to `to` through a temporary file beside it.
fn install(data: &[u8], to: &Path) -> std::io::Result<()> {
    let tmp = with_suffix(to, ".download");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, to)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Replaces this launcher with `latest`'s, if it's newer than `current`, and starts it. On
/// success the caller exits: the new launcher is running.
pub fn update_self(latest: &Latest, current: &str) -> Result<(), String> {
    if let Some(why) = disabled_why(current) {
        return Err(why.into());
    }
    if !latest.newer_than(current) {
        return Err(format!("release {} isn't newer than this launcher ({current}); not installed", latest.version));
    }
    let me = std::env::current_exe().map_err(|e| format!("couldn't find this launcher: {e}"))?;
    let new = with_suffix(&me, ".new");
    let want = download(latest, LAUNCHER_ASSET, &new)?;
    // Held open until it has started: nothing can change it in between (see `swap_in_checked`).
    let _held = swap_in_checked(&me, &new, &want)?;
    std::process::Command::new(&me)
        .spawn()
        .map_err(|e| format!("the new launcher is in place, but didn't start ({e}): start it again"))?;
    Ok(())
}

/// Puts `new` in `exe`'s place as [`swap_in`] does, only if it's still what was checked (SHA-256
/// `want`): the folder may be one others can write to, so the file is read back from the disk
/// both before the swap and after it. The second time it's opened so that, on Windows, nothing
/// else can write, delete or rename it while it's held, and given back: the caller starts it
/// before letting go, so what starts is what was checked. A file that changed is deleted and
/// `exe` is left (or put back) as it was.
fn swap_in_checked(exe: &Path, new: &Path, want: &[u8; 32]) -> Result<std::fs::File, String> {
    if let Err(e) = open_checked(new, want) {
        let _ = std::fs::remove_file(new);
        return Err(e);
    }
    swap_in(exe, new).map_err(|e| format!("couldn't put the new launcher in place of {}: {e}", exe.display()))?;
    match open_checked(exe, want) {
        Ok(held) => Ok(held),
        Err(e) => {
            // Not the release's: out, and the running launcher back in its place.
            let _ = std::fs::remove_file(exe);
            let _ = std::fs::rename(with_suffix(exe, ".old"), exe);
            Err(e)
        }
    }
}

/// Puts `new` in `exe`'s place, keeping `exe` as `<exe>.old`; if that fails, `exe` is put back.
fn swap_in(exe: &Path, new: &Path) -> std::io::Result<()> {
    let old = with_suffix(exe, ".old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e);
    }
    Ok(())
}

/// Deletes the previous launcher an update left behind.
pub fn clean_up() {
    let Ok(me) = std::env::current_exe() else { return };
    let old = with_suffix(&me, ".old");
    if old.exists() {
        // The old one may still be exiting.
        for _ in 0..20 {
            if std::fs::remove_file(&old).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use ring::signature::KeyPair as _;

    use super::*;

    /// A test key, and its public half as the launcher carries it.
    fn key() -> (ring::signature::Ed25519KeyPair, String) {
        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let mut spki = vec![0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        spki.extend_from_slice(pair.public_key().as_ref());
        (pair, base64::engine::general_purpose::STANDARD.encode(spki))
    }

    fn sign(pair: &ring::signature::Ed25519KeyPair, version: &str, sums: &str) -> String {
        let message = format!("{PURPOSE}\n{version}\n{sums}");
        base64::engine::general_purpose::STANDARD.encode(pair.sign(message.as_bytes()))
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn release_numbers() {
        let r = |s| Release::parse(s).unwrap();
        assert_eq!(r("v0.2.0"), Release { numbers: (0, 2, 0), pre: None });
        assert!(r("0.2.0").newer_than(&r("0.1.9")));
        assert!(r("0.10.0").newer_than(&r("0.9.0")), "numbers, not text");
        assert!(r("0.2.0").newer_than(&r("0.2.0-dev")));
        assert!(!r("0.2.0-rc.1").newer_than(&r("0.2.0")));
        assert!(!r("0.2.0").newer_than(&r("0.2.0")));
        for bad in ["", "0.2", "0.2.0.1", "0.2.x", "0.2.0-", "0.2.0-a;b", "1.0.0 ", "-1.0.0"] {
            assert_eq!(Release::parse(bad).filter(|_| bad.trim() == bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_development_build_doesnt_update_itself() {
        assert!(!enabled("0.2.0-dev"));
        assert!(disabled_why("0.2.0-dev").is_some_and(|w| w.contains("development build")));
    }

    #[test]
    fn checks_a_signature_as_verify_release_does() {
        // What sign-release.sh signs: "cross-com/release/v1\n", the version and "\n", then the
        // sums exactly.
        let (pair, public) = key();
        let sums = "00  crosscom-launcher.exe\n";
        let signature = sign(&pair, "0.1.0", sums);
        assert!(signed(&public, "0.1.0", sums.as_bytes(), &signature));
        assert!(
            signed(&public, "0.1.0", sums.as_bytes(), &format!("{}\n{}\n", &signature[..40], &signature[40..])),
            "wrapped"
        );
        assert!(!signed(&public, "0.1.1", sums.as_bytes(), &signature), "another version");
        assert!(!signed(&public, "0.1.0", b"11  crosscom-launcher.exe\n", &signature), "other sums");
        assert!(!signed("", "0.1.0", sums.as_bytes(), &signature), "no key built in");
        let other = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap();
        assert!(!signed(&public, "0.1.0", sums.as_bytes(), &sign(&other, "0.1.0", sums)), "another key");
    }

    #[test]
    fn checks_a_signature_openssl_made() {
        // A throwaway key: `openssl genpkey -algorithm ed25519`, then `openssl pkeyutl -sign
        // -rawin` over what sign-release.sh signs, as it signs it.
        let public = "MCowBQYDK2VwAyEArXhpvBxnvryjXI/7uIGKDzJG3wIvqxqJba3XneOgJV4=";
        let signature = "qKhv4zoEtPP60qfIx6nW/5EE1Y6Oc7TLinoS41CnG1Hu63v2z17UCt+eyZntaGhBMTuERLnHmr3vUe5fISEtCw==";
        assert!(signed(public, "0.1.0", b"00  crosscom-launcher.exe\n", signature));
        assert!(!signed(public, "0.1.1", b"00  crosscom-launcher.exe\n", signature));
    }

    #[test]
    fn a_download_is_installed_only_signed_as_its_release_and_matching_its_checksum() {
        let (pair, public) = key();
        let data = b"the new launcher";
        let sums = format!("{}  {LAUNCHER_ASSET}\n", hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref()));
        let sig = sign(&pair, "1.2.0", &sums);
        let digest = ring::digest::digest(&ring::digest::SHA256, data);
        assert_eq!(checked(&public, "1.2.0", &sums, &sig, LAUNCHER_ASSET, data).as_ref().map(|h| &h[..]), Ok(digest.as_ref()));
        let refused = |r: Result<[u8; 32], String>, why: &str| {
            let e = r.expect_err(why);
            assert!(e.contains("not installed"), "{why}: {e}");
        };
        refused(checked(&public, "1.2.0", &sums, &sig, LAUNCHER_ASSET, b"something else"), "another file");
        refused(checked(&public, "1.3.0", &sums, &sig, LAUNCHER_ASSET, data), "an old release under a new tag");
        refused(checked(&public, "1.2.0", &sums, &sig, "uplay_r1_loader.dll", data), "a file the sums don't list");
        refused(checked(&public, "1.2.0", &sums, "not a signature", LAUNCHER_ASSET, data), "no signature");
        refused(
            checked(&public, "1.2.0", &format!("{sums}{}  extra.dll\n", "0".repeat(64)), &sig, LAUNCHER_ASSET, data),
            "sums changed after signing",
        );
        let star = format!("{} *{LAUNCHER_ASSET}\n", hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref()));
        assert!(checksum_for(&star, LAUNCHER_ASSET).is_some(), "sha256sum's binary mark");
    }

    #[test]
    fn asks_only_where_releases_are() {
        assert!(from_releases("https://github.com/JDevWebb/cross-com-enhanced/releases/download/v0.2.0/SHA256SUMS"));
        assert!(from_releases("https://objects.githubusercontent.com/x"));
        assert!(!from_releases("http://github.com/x"), "not HTTPS");
        assert!(!from_releases("https://github.com.evil.example/x"));
        assert!(!from_releases("https://github.com@evil.example/x"));
        assert!(!from_releases("https://github.com:8443/x"));
    }

    /// Against a test release (`cargo test -- --ignored live_`, built with CROSSCOM_UPDATE_FROM
    /// and CROSSCOM_RELEASE_KEY naming it and its key): a release signed as its own version is
    /// downloaded and checked.
    #[test]
    #[ignore = "needs a test release: CROSSCOM_UPDATE_FROM and CROSSCOM_RELEASE_KEY at build time"]
    fn live_signed_release_is_downloaded() {
        let latest = latest().unwrap();
        assert!(latest.newer_than("0.1.0"), "{}", latest.version);
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("crosscom-launcher.exe.new");
        download(&latest, LAUNCHER_ASSET, &to).unwrap();
        assert!(std::fs::metadata(&to).unwrap().len() > 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "nothing else left behind");
    }

    /// The same, with the test release's files published again under a newer tag: refused.
    #[test]
    #[ignore = "needs a test release whose tag isn't the version its files were signed as"]
    fn live_retagged_release_is_refused() {
        let latest = latest().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("crosscom-launcher.exe.new");
        let e = download(&latest, LAUNCHER_ASSET, &to).unwrap_err();
        assert!(e.contains("isn't signed by the project's release key"), "{e}");
        assert!(!to.exists());
    }

    #[test]
    fn an_update_takes_the_launchers_place_and_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, new) = (dir.path().join("crosscom-launcher.exe"), dir.path().join("crosscom-launcher.exe.new"));
        std::fs::write(&exe, "old").unwrap();
        install(b"new", &new).unwrap();
        assert!(!dir.path().join("crosscom-launcher.exe.new.download").exists(), "nothing left beside it");
        swap_in(&exe, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(with_suffix(&exe, ".old")).unwrap(), "old", "kept, for clean_up");
        assert!(swap_in(&exe, &dir.path().join("missing")).is_err());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new", "put back as it was");
    }

    #[test]
    fn a_new_launcher_changed_after_its_check_isnt_put_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, new) = (dir.path().join("crosscom-launcher.exe"), dir.path().join("crosscom-launcher.exe.new"));
        let sha = |data: &[u8]| -> [u8; 32] { ring::digest::digest(&ring::digest::SHA256, data).as_ref().try_into().unwrap() };
        let want = sha(b"the release's launcher");
        std::fs::write(&exe, "old").unwrap();

        // Someone else put another file there after the download was checked.
        install(b"something else", &new).unwrap();
        let e = swap_in_checked(&exe, &new, &want).unwrap_err();
        assert!(e.contains("changed after it was checked"), "{e}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old", "the running launcher stays");
        assert!(!new.exists(), "the other file is deleted");
        assert!(!with_suffix(&exe, ".old").exists());

        // As checked: in place, held open.
        install(b"the release's launcher", &new).unwrap();
        let held = swap_in_checked(&exe, &new, &want).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "the release's launcher");
        assert_eq!(std::fs::read_to_string(with_suffix(&exe, ".old")).unwrap(), "old");
        drop(held);
        assert!(open_checked(&exe, &sha(b"old")).is_err());
    }
}
