//! Has the game name the Cross-Com server in its online config request.
//!
//! The game asks `http://onlineconfigservice.ubi.com/OnlineConfigService.svc/GetOnlineConfig`,
//! and `net` sends the lookup of that name to the server, so the request reaches it; but the
//! game adds its own `Host: onlineconfigservice.ubi.com` header, a string in its exe pushed by
//! one instruction. A server behind a proxy that tells sites apart by name (one machine hosting
//! another game's server too) wouldn't know the request was its own. So that instruction is
//! pointed at `Host: <the server's name>` instead, any length.
//!
//! The string and the instruction are found by their contents, in the exe as loaded (DX11 or
//! DX9), and only when each is there exactly once; otherwise nothing is changed and the game
//! names Ubisoft's host as before, which a server answering every name still takes.
#![cfg_attr(not(all(windows, target_arch = "x86")), allow(dead_code))]

/// The header the game adds, as it's kept in its exe.
const HEADER: &[u8] = b"Host: onlineconfigservice.ubi.com\0";
/// `push imm32`.
const PUSH: u8 = 0x68;

/// Where `pattern` is in `data`, if it's there exactly once.
fn find_once(data: &[u8], pattern: &[u8]) -> Option<usize> {
    let mut found = data.windows(pattern.len()).enumerate().filter(|(_, w)| *w == pattern).map(|(i, _)| i);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// A section of the exe as loaded: its address, whether it holds code, and its bytes.
pub(crate) struct Section {
    pub at: u32,
    pub code: bool,
    pub bytes: Vec<u8>,
}

/// The address of the operand of the one `push` of the header's address (the header in a data
/// section once, pushed once in the code), or why there's none.
fn operand(sections: &[Section]) -> Result<u32, &'static str> {
    let mut header = None;
    for s in sections.iter().filter(|s| !s.code) {
        if let Some(i) = find_once(&s.bytes, HEADER) {
            if header.is_some() {
                return Err("its Host header is there twice");
            }
            header = Some(s.at + u32::try_from(i).map_err(|_| "a section too big")?);
        }
    }
    let header = header.ok_or("no Host header found")?;
    let mut push = [PUSH; 5];
    push[1..].copy_from_slice(&header.to_le_bytes());
    let mut found = None;
    for s in sections.iter().filter(|s| s.code) {
        if let Some(i) = find_once(&s.bytes, &push) {
            if found.is_some() {
                return Err("the Host header is pushed in more than one place");
            }
            found = Some(s.at + u32::try_from(i).map_err(|_| "a section too big")? + 1);
        }
    }
    found.ok_or("the Host header isn't pushed where expected")
}

/// `Host: NAME`, for the server `server` as set up (`host`, `host:port` or a URL).
fn header_for(server: &str) -> Option<std::ffi::CString> {
    let (host, _) = crosscom_client_common::http::address(server);
    if host.is_empty() || host.contains(['\r', '\n', ' ']) {
        return None;
    }
    std::ffi::CString::new(format!("Host: {host}")).ok()
}

/// Points the game's Host header at the server's name. Called once at start, before the game
/// asks for its online config.
#[cfg(all(windows, target_arch = "x86"))]
pub(crate) fn install(server: &str) {
    use log::info;
    use log::warn;

    let Some(header) = header_for(server) else {
        warn!("{server:?} can't name the server in the game's request: it names Ubisoft's host");
        return;
    };
    let Some(sections) = crate::memory::exe_sections() else {
        warn!("The game's exe can't be read: its online config request names Ubisoft's host");
        return;
    };
    let sections: Vec<Section> = sections
        .into_iter()
        .filter_map(|(at, size, code)| {
            Some(Section {
                at: u32::try_from(at).ok()?,
                code,
                bytes: crate::memory::read(at, size)?,
            })
        })
        .collect();
    match operand(&sections) {
        Ok(at) => {
            // The header must outlive the game's every request: kept for the process.
            let header: &'static std::ffi::CStr = Box::leak(header.into_boxed_c_str());
            let Ok(address) = u32::try_from(header.as_ptr() as usize) else { return };
            if crate::memory::patch(at as usize, &address.to_le_bytes()) {
                info!("The game names {} in its online config request", header.to_string_lossy().trim_start_matches("Host: "));
            } else {
                warn!("Couldn't change the game's Host header: its online config request names Ubisoft's host");
            }
        }
        Err(e) => warn!("The game's exe: {e}; its online config request names Ubisoft's host"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(header_at: u32, pushes: &[u32]) -> Vec<Section> {
        let mut data = vec![0u8; 64];
        data[16..16 + HEADER.len()].copy_from_slice(HEADER);
        let mut code = vec![0x90u8; 64];
        for (n, &to) in pushes.iter().enumerate() {
            code[8 + n * 10] = PUSH;
            code[9 + n * 10..13 + n * 10].copy_from_slice(&to.to_le_bytes());
        }
        vec![
            Section {
                at: 0x40_1000,
                code: true,
                bytes: code,
            },
            Section {
                at: header_at - 16,
                code: false,
                bytes: data,
            },
        ]
    }

    #[test]
    fn finds_the_one_push_of_the_header() {
        assert_eq!(operand(&exe(0xd2_0268, &[0xd2_0268])), Ok(0x40_1000 + 9));
    }

    #[test]
    fn changes_nothing_unless_found_once() {
        assert!(operand(&exe(0xd2_0268, &[])).is_err(), "not pushed");
        assert!(operand(&exe(0xd2_0268, &[0xd2_0268, 0xd2_0268])).is_err(), "pushed twice");
        assert!(operand(&exe(0xd2_0268, &[0xd2_0269])).is_err(), "pushing something else");
        let mut twice = exe(0xd2_0268, &[0xd2_0268]);
        twice.push(Section {
            at: 0x50_0000,
            code: false,
            bytes: HEADER.to_vec(),
        });
        assert!(operand(&twice).is_err(), "the header twice");
        let none = vec![Section {
            at: 0x40_1000,
            code: true,
            bytes: vec![0x90; 16],
        }];
        assert!(operand(&none).is_err());
    }

    #[test]
    fn names_the_servers_host() {
        assert_eq!(header_for("grfs-test.jdevwebb.net").unwrap().to_str().unwrap(), "Host: grfs-test.jdevwebb.net");
        assert_eq!(header_for("http://203.0.113.10:8080/").unwrap().to_str().unwrap(), "Host: 203.0.113.10");
        assert!(header_for("").is_none());
        assert!(header_for("evil\r\nX: 1").is_none(), "no header injected");
    }
}
