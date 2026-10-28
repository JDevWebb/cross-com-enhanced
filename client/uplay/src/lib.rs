//! `uplay_r1_loader.dll`, Cross-Com's stand-in for Ubisoft's Uplay library.
//!
//! The game calls six Uplay functions. Without Uplay they all answer 0 ("no", "nothing"): no Uplay
//! session, no events and no Uplay friends. The game runs without them; its friends come from the
//! Cross-Com server instead. All are `cdecl`.

#[unsafe(no_mangle)]
pub extern "C" fn UPLAY_Init() -> u32 {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn UPLAY_Quit() -> u32 {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn UPLAY_Update() -> u32 {
    0
}

/// No events, ever.
#[unsafe(no_mangle)]
pub extern "C" fn UPLAY_GetNextEvent(_event: *mut core::ffi::c_void) -> u32 {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn UPLAY_FRIENDS_Init(_flags: u32) -> u32 {
    0
}

/// No Uplay friend list; the list the game passes in is left as it is.
#[unsafe(no_mangle)]
pub extern "C" fn UPLAY_FRIENDS_GetFriendList(_filter: u32, _list: *mut core::ffi::c_void) -> u32 {
    0
}
