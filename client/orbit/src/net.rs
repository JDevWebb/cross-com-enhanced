//! Sends the game's lookup of Ubisoft's online config service to the Cross-Com server.
//!
//! The game finds its servers through `onlineconfigservice.ubi.com`. Hooking `getaddrinfo` in
//! ws2_32 itself, not only the game's import of it, also catches lookups made inside other
//! libraries the game uses for HTTP.

use std::ffi::c_char;
use std::ffi::c_void;
use std::ffi::CStr;
use std::ffi::CString;

use log::error;
use log::info;
use retour::static_detour;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;

type GetAddrInfoFn = unsafe extern "stdcall" fn(*const c_char, *const c_char, *const c_void, *mut *mut c_void) -> i32;

static_detour! {
    static GetAddrInfo: unsafe extern "stdcall" fn(*const c_char, *const c_char, *const c_void, *mut *mut c_void) -> i32;
}

/// Hooks ws2_32's `getaddrinfo`. The game imports ws2_32, so it's loaded before this library and
/// nothing needs loading here, under the loader lock.
pub(crate) fn install() {
    let target = unsafe {
        let module = GetModuleHandleA(c"ws2_32.dll".as_ptr().cast());
        if module.is_null() {
            error!("ws2_32.dll isn't loaded; the game's lookups can't be redirected");
            return;
        }
        let Some(proc) = GetProcAddress(module, c"getaddrinfo".as_ptr().cast()) else {
            error!("ws2_32.dll has no getaddrinfo; the game's lookups can't be redirected");
            return;
        };
        std::mem::transmute::<unsafe extern "system" fn() -> isize, GetAddrInfoFn>(proc)
    };
    let result = unsafe {
        GetAddrInfo
            .initialize(target, |node, service, hints, res| lookup(node, service, hints, res))
            .and_then(|hook| hook.enable())
    };
    match result {
        Ok(()) => info!("Redirecting lookups of {}", crosscom_client_common::config::ONLINE_CONFIG_HOST),
        Err(e) => error!("Couldn't hook getaddrinfo: {e}"),
    }
}

fn lookup(node: *const c_char, service: *const c_char, hints: *const c_void, res: *mut *mut c_void) -> i32 {
    if !node.is_null() {
        let host = unsafe { CStr::from_ptr(node) }.to_string_lossy();
        if let Some(server) = crate::config().redirect(&host) {
            if let Ok(server) = CString::new(server) {
                info!("Looking up {server:?} for {host}");
                return unsafe { GetAddrInfo.call(server.as_ptr(), service, hints, res) };
            }
        }
    }
    unsafe { GetAddrInfo.call(node, service, hints, res) }
}
