//! Windows Firewall rules that let the game be reached by the players in its matches.
//!
//! The game hosts matches itself, so the first time it plays online Windows asks whether to allow
//! it; that prompt takes the focus from the fullscreen game, which can stay minimised and not draw
//! again. So setup adds inbound rules allowing the game's two exes, asking Windows for permission
//! (one UAC prompt) only when they aren't there yet. A player who once dismissed Windows' prompt
//! got rules blocking the game, which win over any allowing; those, for exactly the game's exes,
//! are taken out too.
//!
//! The launcher gets a rule too, for UDP only: its connection test has the server send a packet
//! to a port the router forwards, which Windows would otherwise drop before the launcher saw it.
//! It's for the launcher where it is now, under one name, so a launcher that moved replaces it;
//! the test asks for it again then (`ensure_launcher`). Nothing else is changed.

use std::path::Path;
use std::path::PathBuf;

use crate::game::EXE_DX11;
use crate::game::EXE_DX9;

/// The rules' name, one per exe: `<RULE> (<exe>)`.
pub const RULE: &str = "Ghost Recon Future Soldier (Cross-Com)";
/// The launcher's rule, wherever it is.
pub const LAUNCHER_RULE: &str = "Cross-Com launcher (connection test)";

/// A rule to add: its name, the program, and whether it's for UDP only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub program: PathBuf,
    pub udp_only: bool,
}

/// The rules for the game's exes in `dir`.
#[must_use]
pub fn game_rules(dir: &Path) -> Vec<Rule> {
    programs(dir)
        .into_iter()
        .map(|program| Rule {
            name: rule_name(&program),
            program,
            udp_only: false,
        })
        .collect()
}

/// The launcher's rule, for the launcher where it is now.
#[must_use]
pub fn launcher_rule() -> Option<Rule> {
    let program = std::env::current_exe().ok()?;
    Some(Rule {
        name: LAUNCHER_RULE.into(),
        program,
        udp_only: true,
    })
}

/// The game's exes in `dir` that need a rule.
#[must_use]
pub fn programs(dir: &Path) -> Vec<PathBuf> {
    [EXE_DX11, EXE_DX9].iter().map(|exe| dir.join(exe)).filter(|p| p.is_file()).collect()
}

fn rule_name(program: &Path) -> String {
    let path = program.to_string_lossy();
    let exe = path.rsplit(['\\', '/']).next().unwrap_or_default();
    format!("{RULE} ({exe})")
}

/// The PowerShell that adds `rules` (replacing ours by the same name, and Windows' blocking rules
/// for exactly these programs).
#[must_use]
pub fn script(rules: &[Rule]) -> String {
    // Single-quoted PowerShell strings: only ' needs doubling (the game's folder has one).
    let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut lines = vec!["$ErrorActionPreference = 'Stop'".to_owned()];
    for rule in rules {
        let path = quote(&rule.program.to_string_lossy());
        let name = quote(&rule.name);
        let protocol = if rule.udp_only { " -Protocol UDP" } else { "" };
        lines.push(format!("Get-NetFirewallRule -DisplayName {name} -ErrorAction SilentlyContinue | Remove-NetFirewallRule"));
        lines.push(format!(
            "Get-NetFirewallApplicationFilter -Program {path} -ErrorAction SilentlyContinue | Get-NetFirewallRule | Where-Object {{ $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }} | Remove-NetFirewallRule"
        ));
        lines.push(format!(
            "New-NetFirewallRule -DisplayName {name} -Direction Inbound -Program {path}{protocol} -Action Allow -Profile Any | Out-Null"
        ));
    }
    lines.join("\n")
}

/// The script as PowerShell's `-EncodedCommand` takes it: UTF-16LE, base64.
#[cfg(any(windows, test))]
fn encoded(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

#[cfg(any(windows, test))]
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What became of the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Already there for this folder.
    Present,
    Added,
    /// The player said no to Windows' permission prompt.
    Declined,
    Failed(String),
}

/// Whether each rule is there, for that very program (`netsh` needs no permission).
#[cfg(windows)]
fn present(rules: &[Rule]) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    rules.iter().all(|rule| {
        let shown = std::process::Command::new("netsh")
            .args(["advfirewall", "firewall", "show", "rule"])
            .arg(format!("name={}", rule.name))
            .arg("verbose")
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let Ok(shown) = shown else { return false };
        let text = String::from_utf8_lossy(&shown.stdout).to_lowercase();
        shown.status.success() && text.contains(&rule.program.to_string_lossy().to_lowercase())
    })
}

/// Adds those of `rules` that aren't there, asking Windows for permission (one prompt) if any.
#[cfg(windows)]
fn ensure_rules(rules: &[Rule]) -> Outcome {
    let missing: Vec<Rule> = rules.iter().filter(|r| !present(std::slice::from_ref(r))).cloned().collect();
    if missing.is_empty() {
        return Outcome::Present;
    }
    let args = format!("-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}", encoded(&script(&missing)));
    match elevated("powershell.exe", &args) {
        Ok(0) if present(&missing) => Outcome::Added,
        Ok(code) => Outcome::Failed(format!("PowerShell ended with {code}")),
        Err(e) if e == "declined" => Outcome::Declined,
        Err(e) => Outcome::Failed(e),
    }
}

/// Makes sure the game's exes in `dir`, and this launcher, are allowed, asking Windows for
/// permission if they aren't.
#[cfg(windows)]
#[must_use]
pub fn ensure(dir: &Path) -> Outcome {
    let mut rules = game_rules(dir);
    if rules.is_empty() {
        return Outcome::Present;
    }
    rules.extend(launcher_rule());
    ensure_rules(&rules)
}

/// Whether [`ensure`] would ask Windows for permission: a rule for the game's exes in `dir`, or
/// for this launcher, isn't there. The launcher explains the prompt before it comes.
#[cfg(windows)]
#[must_use]
pub fn ensure_would_ask(dir: &Path) -> bool {
    let mut rules = game_rules(dir);
    if rules.is_empty() {
        return false;
    }
    rules.extend(launcher_rule());
    !present(&rules)
}

/// Whether [`ensure_launcher`] would ask Windows for permission.
#[cfg(windows)]
#[must_use]
pub fn ensure_launcher_would_ask() -> bool {
    !present(&launcher_rule().into_iter().collect::<Vec<_>>())
}

/// Makes sure this launcher is allowed (for the connection test), asking Windows if it isn't.
#[cfg(windows)]
#[must_use]
pub fn ensure_launcher() -> Outcome {
    ensure_rules(&launcher_rule().into_iter().collect::<Vec<_>>())
}

#[cfg(not(windows))]
#[must_use]
pub fn ensure(_dir: &Path) -> Outcome {
    Outcome::Present
}

#[cfg(not(windows))]
#[must_use]
pub fn ensure_launcher() -> Outcome {
    Outcome::Present
}

#[cfg(not(windows))]
#[must_use]
pub fn ensure_would_ask(_dir: &Path) -> bool {
    false
}

#[cfg(not(windows))]
#[must_use]
pub fn ensure_launcher_would_ask() -> bool {
    false
}

/// Runs `file` with `args` as administrator (Windows' permission prompt), hidden, and waits for it.
#[cfg(windows)]
fn elevated(file: &str, args: &str) -> Result<u32, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Foundation::ERROR_CANCELLED;
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::System::Threading::INFINITE;
    use windows_sys::Win32::UI::Shell::ShellExecuteExW;
    use windows_sys::Win32::UI::Shell::SEE_MASK_NOCLOSEPROCESS;
    use windows_sys::Win32::UI::Shell::SHELLEXECUTEINFOW;

    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (verb, file, args) = (wide("runas"), wide(file), wide(args));
    // SAFETY: a zeroed SHELLEXECUTEINFOW is valid (no handles, null strings).
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0);
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = args.as_ptr();
    info.nShow = 0; // SW_HIDE
                    // SAFETY: the strings outlive the call; the process handle is closed below.
    if unsafe { ShellExecuteExW(&raw mut info) } == 0 {
        let error = unsafe { GetLastError() };
        return Err(if error == ERROR_CANCELLED {
            "declined".into()
        } else {
            format!("couldn't ask Windows (error {error})")
        });
    }
    if info.hProcess.is_null() {
        return Err("Windows gave no process to wait for".into());
    }
    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &raw mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_quotes_the_games_folder() {
        let program = PathBuf::from(r"C:\Games\Tom Clancy's Ghost Recon Future Soldier\Future Soldier DX11.exe");
        let rule = Rule {
            name: rule_name(&program),
            program,
            udp_only: false,
        };
        let script = script(std::slice::from_ref(&rule));
        assert!(script.contains(r"-Program 'C:\Games\Tom Clancy''s Ghost Recon Future Soldier\Future Soldier DX11.exe'"));
        assert!(script.contains("-DisplayName 'Ghost Recon Future Soldier (Cross-Com) (Future Soldier DX11.exe)'"));
        assert!(script.contains("-Direction Inbound"));
        // Only blocking rules for exactly this program are taken out.
        assert!(script.contains("$_.Action -eq 'Block'"));
        assert!(!script.contains("-Protocol"), "the game: every protocol");
    }

    #[test]
    fn the_launchers_rule_is_for_udp_under_one_name() {
        let rule = Rule {
            name: LAUNCHER_RULE.into(),
            program: PathBuf::from(r"C:\Users\Kozak\Downloads\crosscom-launcher.exe"),
            udp_only: true,
        };
        let script = script(std::slice::from_ref(&rule));
        assert!(
            script.contains("Get-NetFirewallRule -DisplayName 'Cross-Com launcher (connection test)' -ErrorAction SilentlyContinue | Remove-NetFirewallRule"),
            "{script}"
        );
        assert!(
            script.contains(r"-Program 'C:\Users\Kozak\Downloads\crosscom-launcher.exe' -Protocol UDP -Action Allow"),
            "{script}"
        );
    }

    #[test]
    fn base64_as_powershell_reads_it() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // "a" in UTF-16LE.
        assert_eq!(encoded("a"), "YQA=");
    }

    #[test]
    fn only_the_exes_that_are_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(EXE_DX11), b"").unwrap();
        assert_eq!(programs(dir.path()), vec![dir.path().join(EXE_DX11)]);
    }
}
