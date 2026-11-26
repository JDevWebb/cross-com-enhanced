//! Windows Firewall rules that let the game be reached by the players in its matches.
//!
//! The game hosts matches itself, so the first time it plays online Windows asks whether to allow
//! it; that prompt takes the focus from the fullscreen game, which can stay minimised and not draw
//! again. So setup adds inbound rules allowing the game's two exes UDP (all the game takes in:
//! its peer-to-peer traffic and the server's answers), asking Windows for permission (one UAC
//! prompt) only when they aren't there yet. A rule an earlier launcher added for every protocol
//! counts as not there, so it's replaced. A player who once dismissed Windows' prompt got rules
//! blocking the game, which win over any allowing; those, for exactly the game's exes, are taken
//! out too.
//!
//! The launcher gets a rule too, for UDP only: its connection test has the server send a packet
//! to a port the router forwards, which Windows would otherwise drop before the launcher saw it.
//! It's for the launcher where it is now, under one name, so a launcher that moved replaces it;
//! the test asks for it again then (`ensure_launcher`). Nothing else is changed.
//!
//! The rules are added by a PowerShell script run as administrator, with the paths written into
//! it in single quotes. PowerShell takes the typographic single quotes (U+2018 to U+201B) as
//! quotes too, so a path with one of those, a backtick or a control character is refused before
//! Windows is asked (see [`script`]), and every kind of single quote is doubled anyway.
//! PowerShell and `netsh` are started from Windows' system folder, never by their bare names.

use std::path::Path;
use std::path::PathBuf;

use crate::game::EXE_DX11;
use crate::game::EXE_DX9;

/// The rules' name, one per exe: `<RULE> (<exe>)`.
pub const RULE: &str = "Ghost Recon Future Soldier (Cross-Com)";
/// The launcher's rule, wherever it is.
pub const LAUNCHER_RULE: &str = "Cross-Com launcher (connection test)";

/// A rule to add: its name, the program it lets in to, and what it lets in (`UDP`, or `TCP`
/// for a hosted server's web side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub program: PathBuf,
    pub protocol: &'static str,
}

/// The hosted server's rules' name: `<SERVER_RULE> (<protocol>)`.
pub const SERVER_RULE: &str = "Cross-Com server (hosted from the launcher)";

/// The rules for a server the launcher hosts, run from `program`: its web side (TCP: the online
/// config, HTTPS) and the game's sign-in and services (UDP).
#[must_use]
pub fn server_rules(program: &Path) -> Vec<Rule> {
    ["TCP", "UDP"]
        .into_iter()
        .map(|protocol| Rule {
            name: format!("{SERVER_RULE} ({protocol})"),
            program: program.to_path_buf(),
            protocol,
        })
        .collect()
}

/// The rules for the game's exes in `dir`.
#[must_use]
pub fn game_rules(dir: &Path) -> Vec<Rule> {
    programs(dir)
        .into_iter()
        .map(|program| Rule {
            name: rule_name(&program),
            program,
            protocol: "UDP",
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
        protocol: "UDP",
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

/// The characters PowerShell takes as a single quote: `'` and the typographic ones.
const SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// Whether `text` can go in the script in single quotes: not with a typographic single quote, a
/// backtick or a control character (the plain `'`, as in the game's own folder name, is fine:
/// it's doubled).
fn quotable(text: &str) -> Result<(), String> {
    match text.chars().find(|c| SINGLE_QUOTES[1..].contains(c) || *c == '`' || c.is_control()) {
        Some(c) => Err(format!(
            "the firewall rules can't be added for {text:?}: it has a character ({c:?}) that isn't safe to hand Windows; put it in a folder without one"
        )),
        None => Ok(()),
    }
}

/// `text` as a single-quoted PowerShell string: every kind of single quote doubled.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        if SINGLE_QUOTES.contains(&c) {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// The PowerShell that adds `rules` (replacing ours by the same name, and Windows' blocking rules
/// for exactly these programs), each allowing UDP in. Refused when a path or name holds a
/// character that isn't safe in it (a typographic single quote, a backtick or a control
/// character).
pub fn script(rules: &[Rule]) -> Result<String, String> {
    let mut lines = vec!["$ErrorActionPreference = 'Stop'".to_owned()];
    for rule in rules {
        let program = rule.program.to_string_lossy();
        quotable(&program)?;
        quotable(&rule.name)?;
        let (path, name) = (quote(&program), quote(&rule.name));
        lines.push(format!("Get-NetFirewallRule -DisplayName {name} -ErrorAction SilentlyContinue | Remove-NetFirewallRule"));
        lines.push(format!(
            "Get-NetFirewallApplicationFilter -Program {path} -ErrorAction SilentlyContinue | Get-NetFirewallRule | Where-Object {{ $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }} | Remove-NetFirewallRule"
        ));
        lines.push(format!(
            "New-NetFirewallRule -DisplayName {name} -Direction Inbound -Program {path} -Protocol {} -Action Allow -Profile Any | Out-Null",
            rule.protocol
        ));
    }
    Ok(lines.join("\n"))
}

/// Whether `netsh`'s verbose listing of a rule (`shown`) is a rule for `program`, for
/// `protocol` (an earlier launcher's, for every protocol, isn't). The field names are in
/// Windows' language; the values (the path, `UDP`) aren't.
#[cfg_attr(not(windows), allow(dead_code))]
fn shows_rule(shown: &str, program: &Path, protocol: &str) -> bool {
    let shown = shown.to_lowercase();
    let protocol = protocol.to_lowercase();
    shown.contains(&program.to_string_lossy().to_lowercase()) && shown.lines().any(|line| line.split_whitespace().last().is_some_and(|value| value == protocol))
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
    let Ok(netsh) = crate::windows_dirs::system_program("netsh.exe") else {
        return false;
    };
    rules.iter().all(|rule| {
        let shown = std::process::Command::new(&netsh)
            .args(["advfirewall", "firewall", "show", "rule"])
            .arg(format!("name={}", rule.name))
            .arg("verbose")
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let Ok(shown) = shown else { return false };
        shown.status.success() && shows_rule(&String::from_utf8_lossy(&shown.stdout), &rule.program, rule.protocol)
    })
}

/// Adds those of `rules` that aren't there, asking Windows for permission (one prompt) if any.
#[cfg(windows)]
fn ensure_rules(rules: &[Rule]) -> Outcome {
    let missing: Vec<Rule> = rules.iter().filter(|r| !present(std::slice::from_ref(r))).cloned().collect();
    if missing.is_empty() {
        return Outcome::Present;
    }
    // Refused before Windows is asked: a path that isn't safe in the script.
    let script = match script(&missing) {
        Ok(script) => script,
        Err(e) => return Outcome::Failed(e),
    };
    let powershell = match crate::windows_dirs::system_program(r"WindowsPowerShell\v1.0\powershell.exe") {
        Ok(path) => path,
        Err(e) => return Outcome::Failed(e),
    };
    let args = format!("-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}", encoded(&script));
    match elevated(&powershell, &args) {
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

/// Whether Windows Firewall lets the game's exes in `dir` in (this launcher's own rule, for the
/// connection test, aside: it goes with where the launcher is).
#[cfg(windows)]
#[must_use]
pub fn game_allowed(dir: &Path) -> bool {
    present(&game_rules(dir))
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

/// Makes sure the server the launcher hosts, at `program`, is let in, asking Windows if it isn't.
#[cfg(windows)]
#[must_use]
pub fn ensure_server(program: &Path) -> Outcome {
    ensure_rules(&server_rules(program))
}

/// Whether [`ensure_server`] would ask Windows for permission.
#[cfg(windows)]
#[must_use]
pub fn server_would_ask(program: &Path) -> bool {
    !present(&server_rules(program))
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
pub fn ensure_server(_program: &Path) -> Outcome {
    Outcome::Present
}

#[cfg(not(windows))]
#[must_use]
pub fn server_would_ask(_program: &Path) -> bool {
    false
}

#[cfg(not(windows))]
#[must_use]
pub fn ensure_would_ask(_dir: &Path) -> bool {
    false
}

#[cfg(not(windows))]
#[must_use]
pub fn game_allowed(_dir: &Path) -> bool {
    true
}

#[cfg(not(windows))]
#[must_use]
pub fn ensure_launcher_would_ask() -> bool {
    false
}

/// Runs `file` with `args` as administrator (Windows' permission prompt), hidden, and waits for it.
#[cfg(windows)]
fn elevated(file: &Path, args: &str) -> Result<u32, String> {
    use std::os::windows::ffi::OsStrExt;

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
    let file: Vec<u16> = file.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (verb, args) = (wide("runas"), wide(args));
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
            protocol: "UDP",
        };
        let script = script(std::slice::from_ref(&rule)).unwrap();
        assert!(script.contains(r"-Program 'C:\Games\Tom Clancy''s Ghost Recon Future Soldier\Future Soldier DX11.exe'"));
        assert!(script.contains("-DisplayName 'Ghost Recon Future Soldier (Cross-Com) (Future Soldier DX11.exe)'"));
        assert!(script.contains("-Direction Inbound"));
        // Only blocking rules for exactly this program are taken out.
        assert!(script.contains("$_.Action -eq 'Block'"));
        assert!(script.contains("-Protocol UDP -Action Allow"), "the game: UDP only");
    }

    #[test]
    fn paths_powershell_could_read_as_code_are_refused_before_asking() {
        let rule = |path: &str| Rule {
            name: RULE.into(),
            program: PathBuf::from(path),
            protocol: "UDP",
        };
        // U+2018 to U+201B end a single-quoted string in PowerShell as ' does.
        for bad in [
            "C:\\Games\\x\u{2018}; calc; \u{2018}\\a.exe",
            "C:\\Games\\x\u{2019}; calc\\a.exe",
            "C:\\Games\\x\u{201A}\\a.exe",
            "C:\\Games\\x\u{201B}\\a.exe",
            "C:\\Games\\x`n\\a.exe",
            "C:\\Games\\x\n; calc\\a.exe",
            "C:\\Games\\x\r\\a.exe",
            "C:\\Games\\x\u{0}\\a.exe",
            "C:\\Games\\x\u{85}\\a.exe",
        ] {
            let e = script(&[rule(bad)]).expect_err(bad);
            assert!(e.contains("can't be added"), "{e}");
        }
        // The plain ' is fine (the game's own folder has one): doubled.
        assert!(script(&[rule(r"C:\Games\Tom Clancy's\a.exe")]).unwrap().contains(r"'C:\Games\Tom Clancy''s\a.exe'"));
        // Should a typographic one ever get through, it's doubled as well.
        assert_eq!(
            quote("a\u{2018}b\u{2019}c'd\u{201A}e\u{201B}"),
            "'a\u{2018}\u{2018}b\u{2019}\u{2019}c''d\u{201A}\u{201A}e\u{201B}\u{201B}'"
        );
    }

    #[test]
    fn only_a_udp_rule_for_that_program_is_there() {
        let program = PathBuf::from(r"C:\Games\GRFS\Future Soldier DX11.exe");
        let shown = |protocol: &str| {
            format!("Rule Name:                            x\r\nEnabled:                              Yes\r\nProtocol:                             {protocol}\r\nProgram:                              C:\\Games\\GRFS\\Future Soldier DX11.exe\r\n")
        };
        assert!(shows_rule(&shown("UDP"), &program, "UDP"));
        assert!(!shows_rule(&shown("Any"), &program, "UDP"), "an earlier launcher's rule: replaced");
        assert!(!shows_rule(&shown("UDP"), Path::new(r"C:\Other\Future Soldier DX11.exe"), "UDP"), "another folder's");
        // Windows in another language names the fields its own way.
        assert!(shows_rule("Protokoll:   UDP\nProgramm:  c:\\games\\grfs\\future soldier dx11.exe\n", &program, "UDP"));
    }

    #[test]
    fn the_launchers_rule_is_for_udp_under_one_name() {
        let rule = Rule {
            name: LAUNCHER_RULE.into(),
            program: PathBuf::from(r"C:\Users\Kozak\Downloads\crosscom-launcher.exe"),
            protocol: "UDP",
        };
        let script = script(std::slice::from_ref(&rule)).unwrap();
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
    fn the_hosted_server_gets_tcp_and_udp() {
        let program = Path::new(r"C:\Users\Kozak\AppData\Roaming\Cross-Com\Server\crosscom-server.exe");
        let rules = server_rules(program);
        assert_eq!(rules.iter().map(|r| r.protocol).collect::<Vec<_>>(), ["TCP", "UDP"]);
        let script = script(&rules).unwrap();
        assert!(script.contains("crosscom-server.exe' -Protocol TCP -Action Allow"), "{script}");
        assert!(script.contains("crosscom-server.exe' -Protocol UDP -Action Allow"), "{script}");
        assert!(shows_rule(
            "Protocol: TCP\nProgram: c:\\users\\kozak\\appdata\\roaming\\cross-com\\server\\crosscom-server.exe",
            program,
            "TCP"
        ));
        assert!(!shows_rule(
            "Protocol: UDP\nProgram: c:\\users\\kozak\\appdata\\roaming\\cross-com\\server\\crosscom-server.exe",
            program,
            "TCP"
        ));
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
