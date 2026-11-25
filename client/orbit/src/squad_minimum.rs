//! Squad matches with fewer than four a side (`quick_match_min_players`: half of it a side).
//!
//! The game's menus are told "Squad room min players" (4 in its data) once, as they start, and
//! their Squad Ready Room won't look for a match with fewer in the party: "Failed to join the
//! requested match. Waiting for players... [N]", with no word to the server. The value comes from
//! a read in the AI library, which the game loads (and unloads, and loads again) during its start;
//! a thread watching for the library can be too late for the menus. So the library's loading is
//! hooked: as `LoadLibrary` returns it, and before the game runs any of its code, its reads of the
//! minimum are changed to the setting (`crosscom_client_common::quick_match::plan_squad`).
//!
//! Only on the known game build: the patterns must each be found as expected, or nothing changes.

use std::ffi::c_void;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use crosscom_client_common::quick_match;
use log::error;
use log::info;
use log::warn;
use retour::static_detour;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;

use crate::memory::layout;
use crate::memory::read;

type LoadA = unsafe extern "system" fn(*const u8) -> *mut c_void;
type LoadW = unsafe extern "system" fn(*const u16) -> *mut c_void;
type LoadExA = unsafe extern "system" fn(*const u8, *mut c_void, u32) -> *mut c_void;
type LoadExW = unsafe extern "system" fn(*const u16, *mut c_void, u32) -> *mut c_void;

static_detour! {
    static LoadLibraryA: unsafe extern "system" fn(*const u8) -> *mut c_void;
    static LoadLibraryW: unsafe extern "system" fn(*const u16) -> *mut c_void;
    static LoadLibraryExA: unsafe extern "system" fn(*const u8, *mut c_void, u32) -> *mut c_void;
    static LoadLibraryExW: unsafe extern "system" fn(*const u16, *mut c_void, u32) -> *mut c_void;
}

/// Players a side, as set.
static PER_SIDE: AtomicU8 = AtomicU8::new(0);

/// Hooks kernel32's `LoadLibrary` (all four forms), so the AI library is changed as it loads.
pub(crate) fn install(players: u8) {
    let per_side = quick_match::squad_players(players);
    PER_SIDE.store(per_side, Ordering::Relaxed);
    let kernel32 = unsafe { GetModuleHandleA(c"kernel32.dll".as_ptr().cast()) };
    if kernel32.is_null() {
        error!("kernel32.dll isn't loaded; squad matches keep the game's minimum");
        return;
    }
    let find = |name: &core::ffi::CStr| unsafe { GetProcAddress(kernel32, name.as_ptr().cast()) };
    let hooked = unsafe {
        let a = find(c"LoadLibraryA").map(|p| std::mem::transmute::<_, LoadA>(p));
        let w = find(c"LoadLibraryW").map(|p| std::mem::transmute::<_, LoadW>(p));
        let ex_a = find(c"LoadLibraryExA").map(|p| std::mem::transmute::<_, LoadExA>(p));
        let ex_w = find(c"LoadLibraryExW").map(|p| std::mem::transmute::<_, LoadExW>(p));
        [
            a.map(|t| LoadLibraryA.initialize(t, |name| changed(LoadLibraryA.call(name))).and_then(|h| h.enable())),
            w.map(|t| LoadLibraryW.initialize(t, |name| changed(LoadLibraryW.call(name))).and_then(|h| h.enable())),
            ex_a.map(|t| {
                LoadLibraryExA
                    .initialize(t, |name, file, flags| changed(LoadLibraryExA.call(name, file, flags)))
                    .and_then(|h| h.enable())
            }),
            ex_w.map(|t| {
                LoadLibraryExW
                    .initialize(t, |name, file, flags| changed(LoadLibraryExW.call(name, file, flags)))
                    .and_then(|h| h.enable())
            }),
        ]
    };
    let failed = hooked.iter().filter(|h| !matches!(h, Some(Ok(())))).count();
    if failed == 4 {
        warn!("Couldn't hook LoadLibrary; squad matches keep the game's minimum");
        return;
    }
    info!("Squad matches start with {per_side} a side or more");
    // Already loaded (the client started late): changed now.
    if let Some(base) = crate::memory::module(quick_match::MODULE) {
        change(base);
    }
}

/// After a library is loaded: when it's the AI library, its reads of the minimum are changed.
fn changed(module: *mut c_void) -> *mut c_void {
    if !module.is_null() && crate::memory::module(quick_match::MODULE) == Some(module as usize) {
        change(module as usize);
    }
    module
}

/// Changes the AI library at `base`. A load changed already no longer has the reads the patterns
/// look for, so it's left as it is.
fn change(base: usize) {
    let per_side = PER_SIDE.load(Ordering::Relaxed);
    let Some((sections, _)) = layout(base) else {
        warn!("The AI library's headers can't be read; squad matches keep the game's minimum");
        return;
    };
    for (start, size) in sections {
        let Some(code) = read(base + start, size) else { continue };
        match quick_match::plan_squad(&code, per_side) {
            Ok(patches) => {
                if !patches.iter().all(|p| crate::memory::patch(base + start + p.offset, &p.bytes)) {
                    warn!("Couldn't change the squad minimum's code");
                }
                return;
            }
            Err(e) if e.contains("not found") => {}
            Err(e) => {
                warn!("Squad matches keep the game's minimum (a game build not known: {e})");
                return;
            }
        }
    }
}
