//! Applies `quick_match_min_players` (see `crosscom_client_common::quick_match`) once the game
//! has loaded its AI library, which it does by itself early in its start-up.
//!
//! The game may unload the library and load it again (it does during its start), so the library
//! is watched: when it's gone the minimums wait for it, and a library loaded again is patched
//! again. Its memory is only read and written through `ReadProcessMemory` / `WriteProcessMemory`,
//! which fail (rather than crash the game) if the library goes away between a check and a write.

use std::time::Duration;
use std::time::Instant;

use crosscom_client_common::quick_match;
use crosscom_client_common::quick_match::Patch;
use log::info;
use log::warn;

/// How long to wait for the library the first time: it's loaded in the first seconds of the
/// game's start.
const WAIT: Duration = Duration::from_secs(180);
/// How often the library and the minimums are looked at.
const TICK: Duration = Duration::from_secs(1);

use crate::memory::layout;
use crate::memory::read;
use crate::memory::write;

/// The library's base address, if it's loaded.
fn module() -> Option<usize> {
    crate::memory::module(quick_match::MODULE)
}

/// What was found in one load of the library: the code changes and the minimums' addresses.
struct Plan {
    patches: Vec<Patch>,
    globals: Vec<usize>,
}

/// Finds the places in the library at `base` (from a copy of its code, so nothing is read in
/// place).
fn plan(base: usize, players: u8) -> Result<Plan, String> {
    let (sections, image_size) = layout(base).ok_or("its headers can't be read")?;
    let mut planned = Err(String::from("no code section"));
    let mut globals = Vec::new();
    for (start, size) in sections {
        let code = read(base + start, size).ok_or("its code can't be read")?;
        if planned.is_err() {
            planned = quick_match::plan(&code, players).map(|p| {
                p.into_iter()
                    .map(|patch| Patch {
                        offset: start + patch.offset,
                        ..patch
                    })
                    .collect::<Vec<_>>()
            });
        }
        if globals.is_empty() {
            if let Ok(found) = quick_match::minimums(&code) {
                globals = found.iter().map(|&a| a as usize).filter(|&a| a >= base && a + 4 <= base + image_size).collect();
            }
        }
    }
    // The join rule (the last two patches): as each player joins, the host sets the lobby's
    // minimum from it, which is what a lobby's start is measured against.
    let mut patches = planned?;
    patches.drain(..patches.len().saturating_sub(2));
    Ok(Plan { patches, globals })
}

/// Makes the code changes; false if any couldn't be made.
fn apply(base: usize, patches: &[Patch]) -> bool {
    patches.iter().all(|patch| crate::memory::patch(base + patch.offset, &patch.bytes))
}

/// Whether the library at `base` still has the code changes (false once it's gone, or loaded
/// again at the same place).
fn applied(base: usize, patches: &[Patch]) -> bool {
    patches.iter().all(|p| read(base + p.offset, p.bytes.len()).is_some_and(|b| b == p.bytes))
}

/// Waits for the AI library on a thread of its own, then patches it, and again whenever it's
/// loaded again.
pub(crate) fn install(players: u8) {
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut first = true;
        loop {
            let Some(base) = module() else {
                if first && started.elapsed() > WAIT {
                    warn!("{} never loaded; Quick Match keeps the game's minimum", quick_match::MODULE);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            let plan = match plan(base, players) {
                Ok(plan) => plan,
                Err(e) => {
                    if module() != Some(base) {
                        continue; // unloaded while it was being read
                    }
                    warn!("Couldn't set the Quick Match minimum (a game build not known: {e}); it stays the game's");
                    return;
                }
            };
            if !apply(base, &plan.patches) {
                if module() != Some(base) {
                    continue;
                }
                warn!("Couldn't change the Quick Match minimum's code");
                return;
            }
            if first {
                info!("Quick Match lobbies start with {players} players or more");
            } else {
                info!("{} loaded again; Quick Match minimum set again", quick_match::MODULE);
            }
            first = false;
            let value = u32::from(players).to_le_bytes();
            while module() == Some(base) && applied(base, &plan.patches) {
                for &g in &plan.globals {
                    if read(g, 4).is_some_and(|v| v != value) {
                        write(g, &value);
                    }
                }
                std::thread::sleep(TICK);
            }
        }
    });
}
