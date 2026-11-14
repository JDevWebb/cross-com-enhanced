//! Skips the splash videos the game plays as it starts, when `skip_intro_videos` is set.
//!
//! The game plays its videos with Bink. Its `BinkOpen` is hooked: a splash video is opened as usual,
//! then moved to its last frame (`BinkGoto`), so the game sees it end at once and goes on to its
//! menus. The game can't do without them: answering "no movie" (or the files missing) leaves it
//! retrying the first video forever on a black screen.

use std::ffi::c_char;
use std::ffi::c_void;
use std::ffi::CStr;

use log::error;
use log::info;
use retour::static_detour;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;

type BinkOpenFn = unsafe extern "stdcall" fn(*const c_char, u32) -> *mut c_void;
type BinkGotoFn = unsafe extern "stdcall" fn(*mut c_void, u32, i32);

/// binkw32's `BinkGoto`, found when the hook is installed.
static BINK_GOTO: std::sync::OnceLock<BinkGotoFn> = std::sync::OnceLock::new();

/// Where an open movie's frame count is: the third `u32` of Bink's movie structure (after the
/// width and height).
const FRAMES_INDEX: usize = 2;

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
        let Some(goto) = GetProcAddress(module, c"_BinkGoto@12".as_ptr().cast()) else {
            error!("binkw32.dll has no BinkGoto; the intro videos will play");
            return;
        };
        let _ = BINK_GOTO.set(std::mem::transmute::<unsafe extern "system" fn() -> isize, BinkGotoFn>(goto));
        std::mem::transmute::<unsafe extern "system" fn() -> isize, BinkOpenFn>(proc)
    };
    let result = unsafe { BinkOpen.initialize(target, open).and_then(|hook| hook.enable()) };
    match result {
        Ok(()) => info!("Skipping the intro videos"),
        Err(e) => error!("Couldn't hook BinkOpen; the intro videos will play: {e}"),
    }
}

fn open(name: *const c_char, flags: u32) -> *mut c_void {
    let movie = unsafe { BinkOpen.call(name, flags) };
    if movie.is_null() || name.is_null() || flags & BINK_FILE_HANDLE != 0 {
        return movie;
    }
    let path = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if crosscom_client_common::config::is_intro_video(&path) {
        if let Some(goto) = BINK_GOTO.get() {
            let frames = unsafe { movie.cast::<u32>().add(FRAMES_INDEX).read() };
            unsafe { goto(movie, frames, 0) };
            info!("Skipped {path} (to frame {frames})");
        }
    }
    movie
}
