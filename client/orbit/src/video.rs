//! Skips the splash videos the game plays as it starts, when `skip_intro_videos` is set.
//!
//! The game plays its videos with Bink. Its `BinkOpen` is hooked: for the splash videos it answers
//! as for a missing file (no movie), and the game goes on to its menus.

use std::ffi::c_char;
use std::ffi::c_void;
use std::ffi::CStr;

use log::error;
use log::info;
use retour::static_detour;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;

type BinkOpenFn = unsafe extern "stdcall" fn(*const c_char, u32) -> *mut c_void;

/// `BinkOpen`'s flag for "the name is an open file handle".
const BINK_FILE_HANDLE: u32 = 0x0080_0000;

static_detour! {
    static BinkOpen: unsafe extern "stdcall" fn(*const c_char, u32) -> *mut c_void;
}

/// Hooks binkw32's `BinkOpen`. The game imports binkw32, so it's loaded before this library.
pub(crate) fn install() {
    let target = unsafe {
        let module = GetModuleHandleA(c"binkw32.dll".as_ptr().cast());
        if module.is_null() {
            error!("binkw32.dll isn't loaded; the intro videos will play");
            return;
        }
        let Some(proc) = GetProcAddress(module, c"_BinkOpen@8".as_ptr().cast()) else {
            error!("binkw32.dll has no BinkOpen; the intro videos will play");
            return;
        };
        std::mem::transmute::<unsafe extern "system" fn() -> isize, BinkOpenFn>(proc)
    };
    let result = unsafe { BinkOpen.initialize(target, |name, flags| open(name, flags)).and_then(|hook| hook.enable()) };
    match result {
        Ok(()) => info!("Skipping the intro videos"),
        Err(e) => error!("Couldn't hook BinkOpen; the intro videos will play: {e}"),
    }
}

fn open(name: *const c_char, flags: u32) -> *mut c_void {
    if !name.is_null() && flags & BINK_FILE_HANDLE == 0 {
        let path = unsafe { CStr::from_ptr(name) }.to_string_lossy();
        if crosscom_client_common::config::is_intro_video(&path) {
            info!("Skipped {path}");
            return std::ptr::null_mut();
        }
    }
    unsafe { BinkOpen.call(name, flags) }
}
