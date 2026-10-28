//! The library's entry point.

use std::ffi::c_void;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::HINSTANCE;
use windows_sys::Win32::Foundation::TRUE;
use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

#[unsafe(no_mangle)]
extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe { DisableThreadLibraryCalls(module) };
        crate::start();
    }
    TRUE
}
