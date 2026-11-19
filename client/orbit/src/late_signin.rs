//! Lets a game that signed in after its start (answering "sign in now?" after its connection to
//! the server was lost, or after the server couldn't be reached at its start) join friends.
//!
//! When the game signs in, its menus (in the AI library) go online. If the game isn't then in one
//! of the states it signs in from at its start, they also mark it as waiting for the platform to
//! confirm the sign-in, and they don't let it join anyone until that confirmation arrives. On PC
//! it never does, so accepting an invitation stopped at "Joining Session" until the game was
//! restarted. A game that signs in at its start never waits.
//!
//! So the library is changed to not wait (a jump taken always where it was taken only when the
//! game was in such a state); the library is watched, and changed again whenever it's loaded
//! again. If the confirmation did come, it would only clear what is now never set.
#![cfg_attr(not(all(windows, target_arch = "x86")), allow(dead_code))]

use std::time::Duration;

#[cfg(all(windows, target_arch = "x86"))]
use log::info;
#[cfg(all(windows, target_arch = "x86"))]
use log::warn;

/// The library with the menus' code.
const MODULE: &str = "AICLASS_PC_F.dll";
/// How often it's looked at: the game loads it again a few times as it starts.
const TICK: Duration = Duration::from_secs(1);

/// `test al, al; jnz +7; or byte [esi+0xcc8], 4; or byte [esi+0xcc8], 0x39`: after asking whether
/// the game is in a state it signs in from at its start, the wait is set unless it is.
const WAIT_SET: [u8; 18] = [0x84, 0xc0, 0x75, 0x07, 0x80, 0x8e, 0xc8, 0x0c, 0x00, 0x00, 0x04, 0x80, 0x8e, 0xc8, 0x0c, 0x00, 0x00, 0x39];
/// Where the jump is, in it.
const JUMP: usize = 2;
/// The jump taken always.
const ALWAYS: u8 = 0xeb;

/// Where `pattern` is in `code`, if it's there exactly once.
fn find_once(code: &[u8], pattern: &[u8]) -> Option<usize> {
    let mut found = code.windows(pattern.len()).enumerate().filter(|(_, w)| *w == pattern).map(|(i, _)| i);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Where the jump is in the library at `base` (whether or not it's changed yet), or why there's
/// none.
#[cfg(all(windows, target_arch = "x86"))]
fn find(base: usize) -> Result<usize, &'static str> {
    let sections = crate::memory::code_sections(base).ok_or("its headers can't be read")?;
    let mut changed = WAIT_SET;
    changed[JUMP] = ALWAYS;
    for (start, size) in sections {
        let code = crate::memory::read(base + start, size).ok_or("its code can't be read")?;
        if let Some(at) = find_once(&code, &WAIT_SET).or_else(|| find_once(&code, &changed)) {
            return Ok(base + start + at + JUMP);
        }
    }
    Err("a library not known")
}

/// Watches the library on a thread of its own, and changes it each time it's loaded.
#[cfg(all(windows, target_arch = "x86"))]
pub(crate) fn install() {
    std::thread::spawn(|| {
        // The library's base and the jump's place, once changed.
        let mut changed: Option<(usize, usize)> = None;
        let mut first = true;
        loop {
            let base = crate::memory::module(MODULE);
            let still = changed.is_some_and(|(b, at)| Some(b) == base && crate::memory::read(at, 1).is_some_and(|j| j[0] == ALWAYS));
            if let (Some(base), false) = (base, still) {
                match find(base) {
                    Ok(at) if crate::memory::patch(at, &[ALWAYS]) => {
                        if first {
                            info!("Games that sign in after their start can join friends");
                            first = false;
                        }
                        changed = Some((base, at));
                    }
                    Ok(_) => {
                        warn!("Couldn't change {MODULE}: a game that signs in again may need restarting to join friends");
                        return;
                    }
                    // Unloaded while it was being read: looked at again next time.
                    Err(_) if crate::memory::module(MODULE) != Some(base) => {}
                    Err(e) => {
                        warn!("{MODULE}: {e}; a game that signs in again may need restarting to join friends");
                        return;
                    }
                }
            }
            std::thread::sleep(TICK);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_is_found_once() {
        let mut code = vec![0x90; 40];
        code[10..28].copy_from_slice(&WAIT_SET);
        assert_eq!(find_once(&code, &WAIT_SET), Some(10));
        code[28..40].copy_from_slice(&WAIT_SET[..12]);
        assert_eq!(find_once(&code, &WAIT_SET), Some(10));
    }

    #[test]
    fn a_pattern_found_twice_is_not_used() {
        let mut code = vec![0x90; 60];
        code[0..18].copy_from_slice(&WAIT_SET);
        code[30..48].copy_from_slice(&WAIT_SET);
        assert_eq!(find_once(&code, &WAIT_SET), None);
    }

    #[test]
    fn only_the_jump_changes() {
        let mut changed = WAIT_SET;
        changed[JUMP] = ALWAYS;
        let differing: Vec<usize> = (0..WAIT_SET.len()).filter(|&i| WAIT_SET[i] != changed[i]).collect();
        assert_eq!(differing, vec![JUMP]);
        // A short jump over the 7-byte `or` that sets the wait.
        assert_eq!(changed[JUMP + 1], 7);
    }
}
