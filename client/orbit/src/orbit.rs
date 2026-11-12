//! The Orbit client's C++ methods the game calls. `build.rs` exports each under its MSVC name.
//!
//! They're `thiscall` member functions of `mg::orbitclient::OrbitClient`, `SavegameReader` and
//! `SavegameWriter`. The game allocates the `OrbitClient` itself, and this library keeps no state
//! in it. Readers and writers are this library's own (`Reader`, `Writer`), handed to the game
//! through its listeners and freed when it closes them.
//!
//! Results go back through listener objects the game passes in: each listener interface has one
//! virtual method, `thiscall`, at the start of its vtable.

#![allow(non_snake_case)]

use std::ffi::c_char;
use std::ffi::c_void;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use crosscom_client_common::saves::Saves;
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

// Saves: kept on this PC (`crosscom_client_common::saves`), per account. The game asks for a
// reader or writer, waits up to 2 s for its listener to be called, then reads or writes through
// it. A listener left uncalled makes the game give up on saving until it restarts, so every
// request is answered, straight away.

/// What the game's save listeners are called with: `(request id, error, reader or writer)` and
/// `(request id, bytes read or written)`. Each interface's one method is first in its vtable.
type OnObject = unsafe extern "thiscall" fn(*mut c_void, u32, u32, *mut c_void);
type OnBytes = unsafe extern "thiscall" fn(*mut c_void, u32, u32);

/// The error the game is given when there's no save, or it couldn't be read or written: any
/// value but 0 is a failure to the game.
const SAVE_FAILED: u32 = 1;

/// Calls a listener's first virtual method. `listener` points at its vtable pointer.
unsafe fn first_method<F: Copy>(listener: *mut c_void) -> Option<F> {
    if listener.is_null() {
        return None;
    }
    let vtable = unsafe { *listener.cast::<*const F>() };
    if vtable.is_null() {
        None
    } else {
        Some(unsafe { *vtable })
    }
}

fn saves() -> Option<Saves> {
    Saves::for_account(&crate::config().username)
}

/// An open save, handed to the game as a `SavegameReader`.
struct Reader {
    data: Vec<u8>,
}

/// A save being written, handed to the game as a `SavegameWriter`.
struct Writer {
    saves: Saves,
    product: u32,
    savegame: u32,
    data: Vec<u8>,
}

#[unsafe(no_mangle)]
pub unsafe extern "thiscall" fn OrbitClient_GetSavegameReader(_this: *mut c_void, request_id: u32, listener: *mut c_void, product_id: u32, savegame_id: u32) {
    let Some(on_reader) = (unsafe { first_method::<OnObject>(listener) }) else {
        warn!("GetSavegameReader({request_id}) without a listener");
        return;
    };
    let found = match saves().map(|s| s.read(product_id, savegame_id)) {
        Some(Ok(Some(data))) => Some(data),
        Some(Ok(None)) => {
            info!("No save {product_id}-{savegame_id} yet");
            None
        }
        Some(Err(e)) => {
            warn!("Couldn't read save {product_id}-{savegame_id}: {e}");
            None
        }
        None => {
            warn!("No APPDATA folder: saves aren't kept");
            None
        }
    };
    match found {
        Some(data) => {
            debug!("Reading save {product_id}-{savegame_id} ({} bytes)", data.len());
            let reader = Box::into_raw(Box::new(Reader { data }));
            unsafe { on_reader(listener, request_id, 0, reader.cast()) };
        }
        None => unsafe { on_reader(listener, request_id, SAVE_FAILED, std::ptr::null_mut()) },
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "thiscall" fn OrbitClient_GetSavegameWriter(_this: *mut c_void, request_id: u32, listener: *mut c_void, product_id: u32, savegame_id: u32, _open: bool) {
    let Some(on_writer) = (unsafe { first_method::<OnObject>(listener) }) else {
        warn!("GetSavegameWriter({request_id}) without a listener");
        return;
    };
    let Some(saves) = saves() else {
        warn!("No APPDATA folder: saves aren't kept");
        unsafe { on_writer(listener, request_id, SAVE_FAILED, std::ptr::null_mut()) };
        return;
    };
    let writer = Box::into_raw(Box::new(Writer {
        saves,
        product: product_id,
        savegame: savegame_id,
        data: Vec::new(),
    }));
    unsafe { on_writer(listener, request_id, 0, writer.cast()) };
}

/// Copies the save from `offset` into `buffer` (up to `size` bytes) and tells the listener how
/// many; 0 is the end.
#[unsafe(no_mangle)]
pub unsafe extern "thiscall" fn SavegameReader_Read(this: *mut c_void, request_id: u32, listener: *mut c_void, offset: u32, buffer: *mut c_void, size: u32) {
    let Some(on_read) = (unsafe { first_method::<OnBytes>(listener) }) else {
        warn!("SavegameReader::Read({request_id}) without a listener");
        return;
    };
    let mut n = 0;
    if !this.is_null() && !buffer.is_null() {
        let reader = unsafe { &*this.cast::<Reader>() };
        let rest = reader.data.get(offset as usize..).unwrap_or_default();
        let count = rest.len().min(size as usize);
        unsafe { std::ptr::copy_nonoverlapping(rest.as_ptr(), buffer.cast::<u8>(), count) };
        n = u32::try_from(count).unwrap_or(0);
    }
    unsafe { on_read(listener, request_id, n) };
}

#[unsafe(no_mangle)]
pub unsafe extern "thiscall" fn SavegameReader_Close(this: *mut c_void) {
    if !this.is_null() {
        drop(unsafe { Box::from_raw(this.cast::<Reader>()) });
    }
}

#[unsafe(no_mangle)]
pub extern "thiscall" fn SavegameWriter_SetName(_this: *mut c_void, _name: *mut u16) -> bool {
    true
}

/// Takes `size` bytes for the save and tells the listener it took them all.
#[unsafe(no_mangle)]
pub unsafe extern "thiscall" fn SavegameWriter_Write(this: *mut c_void, request_id: u32, listener: *mut c_void, buffer: *mut c_void, size: u32) {
    let Some(on_write) = (unsafe { first_method::<OnBytes>(listener) }) else {
        warn!("SavegameWriter::Write({request_id}) without a listener");
        return;
    };
    let mut n = 0;
    if !this.is_null() && !buffer.is_null() {
        let writer = unsafe { &mut *this.cast::<Writer>() };
        if writer.data.len() + size as usize <= crosscom_client_common::saves::MAX_SIZE {
            writer.data.extend_from_slice(unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) });
            n = size;
        }
    }
    unsafe { on_write(listener, request_id, n) };
}

/// Keeps what was written when `commit` is set, and frees the writer.
#[unsafe(no_mangle)]
pub unsafe extern "thiscall" fn SavegameWriter_Close(this: *mut c_void, commit: bool) {
    if this.is_null() {
        return;
    }
    let writer = unsafe { Box::from_raw(this.cast::<Writer>()) };
    if !commit {
        return;
    }
    match writer.saves.write(writer.product, writer.savegame, &writer.data) {
        Ok(()) => debug!("Saved {}-{} ({} bytes)", writer.product, writer.savegame, writer.data.len()),
        Err(e) => warn!("Couldn't keep save {}-{}: {e}", writer.product, writer.savegame),
    }
}
