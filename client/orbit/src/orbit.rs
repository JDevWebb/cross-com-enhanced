//! The Orbit client's C++ methods the game calls. `build.rs` exports each under its MSVC name.
//!
//! They're `thiscall` member functions of `mg::orbitclient::OrbitClient`, `SavegameReader` and
//! `SavegameWriter`. The game allocates those objects itself and this library keeps no state in
//! them, so `this` is never read or written.
//!
//! Results go back through listener objects the game passes in: each listener interface has one
//! virtual method, `thiscall`, at the start of its vtable.

#![allow(non_snake_case)]

use std::ffi::c_char;
use std::ffi::c_void;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use log::debug;
use log::info;
use log::warn;

use crate::Login;

/// `IGetLoginDetailsListener`.
#[repr(C)]
pub struct LoginDetailsListener {
    vtable: *const LoginDetailsListenerVtable,
}

#[repr(C)]
struct LoginDetailsListenerVtable {
    /// `(request id, username, password, CD key)`. The game copies each string into a buffer of
    /// 80 bytes; the settings are checked against that when they're read.
    on_login_details: unsafe extern "thiscall" fn(*mut LoginDetailsListener, u32, *const c_char, *const u16, *const c_char),
}

static NEXT_REQUEST_ID: AtomicU32 = AtomicU32::new(1);
static EMPTY_WIDE: [u16; 1] = [0];

fn login() -> &'static Login {
    static LOGIN: OnceLock<Login> = OnceLock::new();
    LOGIN.get_or_init(|| Login::new(crate::config()))
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_new(this: *mut c_void) -> *mut c_void {
    debug!("OrbitClient()");
    this
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_drop(_this: *mut c_void) {
    debug!("~OrbitClient()");
}

/// No installation problem.
#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_GetInstallationErrorNum(_this: *mut c_void) -> u32 {
    0
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_GetInstallationErrorString(_this: *mut c_void, _language: *const c_char) -> *mut u16 {
    EMPTY_WIDE.as_ptr().cast_mut()
}

/// The game asks whether it must restart through the launcher: it needn't.
#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_StartLauncher(_this: *mut c_void, _a: u32, _b: u32, _c: *const c_char, _d: *const c_char) -> bool {
    false
}

/// Called every frame to deliver pending results; this library delivers them straight away.
#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_Update(_this: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_GetRequestUniqueId(_this: *mut c_void) -> u32 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// Gives the game the account it signs in with. With no settings the strings are empty, and the
/// server refuses the sign-in, which the game reports.
#[unsafe(no_mangle)]
pub unsafe extern "thiscall" fn OrbitClient_GetLoginDetails(_this: *mut c_void, request_id: u32, listener: *mut LoginDetailsListener) {
    if listener.is_null() || unsafe { (*listener).vtable.is_null() } {
        warn!("GetLoginDetails({request_id}) without a listener");
        return;
    }
    let login = login();
    info!("Signing in as {:?}", login.username);
    unsafe {
        ((*(*listener).vtable).on_login_details)(listener, request_id, login.username.as_ptr(), login.password.as_ptr(), login.cd_key.as_ptr());
    }
}

// Cloud saves: accepted and dropped. The listeners aren't called, and the game carries on.

#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_GetSavegameReader(_this: *mut c_void, request_id: u32, _listener: *mut c_void, product_id: u32, savegame_id: u32) {
    debug!("GetSavegameReader({request_id}, product {product_id}, savegame {savegame_id}): no cloud saves");
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn OrbitClient_GetSavegameWriter(_this: *mut c_void, request_id: u32, _listener: *mut c_void, product_id: u32, savegame_id: u32, _open: bool) {
    debug!("GetSavegameWriter({request_id}, product {product_id}, savegame {savegame_id}): no cloud saves");
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn SavegameReader_Read(_this: *mut c_void, request_id: u32, _listener: *mut c_void, _offset: u32, _buffer: *mut c_void, _size: u32) {
    debug!("SavegameReader::Read({request_id})");
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn SavegameReader_Close(_this: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "thiscall" fn SavegameWriter_SetName(_this: *mut c_void, _name: *mut u16) -> bool {
    true
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn SavegameWriter_Write(_this: *mut c_void, request_id: u32, _listener: *mut c_void, _buffer: *mut c_void, _size: u32) {
    debug!("SavegameWriter::Write({request_id})");
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn SavegameWriter_Close(_this: *mut c_void, _commit: bool) {}
