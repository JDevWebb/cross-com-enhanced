//! Applies `quick_match_min_players` (see `crosscom_client_common::quick_match`) once the game
//! has loaded its AI library, which it does by itself early in its start-up.

use std::ffi::c_void;
use std::time::Duration;
use std::time::Instant;

use crosscom_client_common::quick_match;
use log::info;
use log::warn;
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Memory::VirtualProtect;
use windows_sys::Win32::System::Memory::PAGE_EXECUTE_READWRITE;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// How long to wait for the library: it's loaded in the first seconds of the game's start.
const WAIT: Duration = Duration::from_secs(180);
/// A section that holds code.
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

/// The module's code sections as loaded: (offset from its base, size).
///
/// # Safety
/// `base` must be a loaded module's base address.
unsafe fn code_sections(base: *const u8) -> Vec<(usize, usize)> {
    let read32 = |at: usize| unsafe { base.add(at).cast::<u32>().read_unaligned() } as usize;
    let read16 = |at: usize| usize::from(unsafe { base.add(at).cast::<u16>().read_unaligned() });
    let nt = read32(0x3c);
    let sections = read16(nt + 6);
    let first = nt + 24 + read16(nt + 20);
    (0..sections.min(96))
        .map(|i| first + i * 40)
        .filter(|&h| read32(h + 36) as u32 & IMAGE_SCN_MEM_EXECUTE != 0)
        .map(|h| (read32(h + 12), read32(h + 8)))
        .collect()
}

/// The module's size in memory.
///
/// # Safety
/// `base` must be a loaded module's base address.
unsafe fn image_size(base: *const u8) -> usize {
    let nt = unsafe { base.add(0x3c).cast::<u32>().read_unaligned() } as usize;
    unsafe { base.add(nt + 24 + 56).cast::<u32>().read_unaligned() as usize }
}

/// Waits for the AI library on a thread of its own, then patches it.
pub(crate) fn install(players: u8) {
    std::thread::spawn(move || {
        let started = Instant::now();
        let name = format!("{}\0", quick_match::MODULE);
        let base = loop {
            let module = unsafe { GetModuleHandleA(name.as_ptr()) };
            if !module.is_null() {
                break module.cast::<u8>().cast_const();
            }
            if started.elapsed() > WAIT {
                warn!("{} never loaded; Quick Match keeps the game's minimum", quick_match::MODULE);
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        // The patterns are each inside one code section.
        let mut planned = Err(String::from("no code section"));
        for (start, size) in unsafe { code_sections(base) } {
            let code = unsafe { std::slice::from_raw_parts(base.add(start), size) };
            planned = quick_match::plan(code, players).map(|p| (start, p));
            if planned.is_ok() {
                break;
            }
        }
        // The minimums themselves, before the code that reads them is changed.
        let globals = (|| {
            for (start, size) in unsafe { code_sections(base) } {
                let code = unsafe { std::slice::from_raw_parts(base.add(start), size) };
                if let Ok(found) = quick_match::minimums(code) {
                    return found.to_vec();
                }
            }
            Vec::new()
        })();
        let (start, patches) = match planned {
            Ok(found) => found,
            Err(e) => {
                warn!("Couldn't set the Quick Match minimum (a game build not known: {e}); it stays the game's");
                return;
            }
        };
        // The join rule (the last two patches): as each player joins, the host sets the lobby's
        // minimum from it, which is what a lobby's start is measured against. The minimums
        // themselves are written below too.
        for patch in patches.iter().skip(patches.len().saturating_sub(2)) {
            let at = unsafe { base.add(start + patch.offset) }.cast_mut();
            let mut old = 0;
            if unsafe { VirtualProtect(at.cast::<c_void>(), patch.bytes.len(), PAGE_EXECUTE_READWRITE, &raw mut old) } == 0 {
                warn!("Couldn't change the Quick Match minimum's code");
                return;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(patch.bytes.as_ptr(), at, patch.bytes.len());
                VirtualProtect(at.cast::<c_void>(), patch.bytes.len(), old, &raw mut old);
                FlushInstructionCache(GetCurrentProcess(), at.cast::<c_void>(), patch.bytes.len());
            }
        }
        info!("Quick Match lobbies start with {players} players or more");
        let end = base as usize + unsafe { image_size(base) };
        let globals: Vec<*mut u32> = globals
            .into_iter()
            .map(|a| a as usize)
            .filter(|&a| a >= base as usize && a + 4 <= end)
            .map(|a| a as *mut u32)
            .collect();
        loop {
            for &g in &globals {
                unsafe {
                    if g.read_unaligned() != u32::from(players) {
                        g.write_unaligned(u32::from(players));
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}
