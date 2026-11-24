//! Reading and changing the game's own code and data, through `ReadProcessMemory` and
//! `WriteProcessMemory`, which fail (rather than crash the game) where nothing is mapped.

use std::ffi::c_void;

use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Memory::VirtualProtect;
use windows_sys::Win32::System::Memory::PAGE_EXECUTE_READWRITE;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// `len` bytes at `at`, or `None` if they can't be read.
pub(crate) fn read(at: usize, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut done = 0;
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), at as *const c_void, buf.as_mut_ptr().cast(), len, &raw mut done) };
    (ok != 0 && done == len).then_some(buf)
}

pub(crate) fn read_u32(at: usize) -> Option<u32> {
    read(at, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Writes `bytes` over data at `at`; false if it couldn't.
pub(crate) fn write(at: usize, bytes: &[u8]) -> bool {
    let mut done = 0;
    let ok = unsafe { WriteProcessMemory(GetCurrentProcess(), at as *const c_void, bytes.as_ptr().cast(), bytes.len(), &raw mut done) };
    ok != 0 && done == bytes.len()
}

/// The base address of the loaded library `name`, if it's loaded.
pub(crate) fn module(name: &str) -> Option<usize> {
    let name = format!("{name}\0");
    let module = unsafe { GetModuleHandleA(name.as_ptr()) };
    (!module.is_null()).then_some(module as usize)
}

/// A section that holds code.
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

/// The code sections of the module at `base` as loaded: (offset from its base, size); and its size
/// in memory.
pub(crate) fn layout(base: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let nt = read_u32(base + 0x3c)? as usize;
    let header = read(base + nt, 24 + 0x60)?;
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([header[at], header[at + 1]]));
    let image_size = u32::from_le_bytes(header[24 + 56..24 + 60].try_into().ok()?) as usize;
    let sections = u16_at(6).min(96);
    let table = read(base + nt + 24 + u16_at(20), sections * 40)?;
    let code = table
        .chunks_exact(40)
        .map(|h| move |at: usize| u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]) as usize)
        .filter(|field| field(36) as u32 & IMAGE_SCN_MEM_EXECUTE != 0)
        .map(|field| (field(12), field(8)))
        .collect();
    Some((code, image_size))
}

/// The sections of the game's exe as loaded: (address, size in memory, whether it holds code).
pub(crate) fn exe_sections() -> Option<Vec<(usize, usize, bool)>> {
    let base = unsafe { GetModuleHandleA(std::ptr::null()) } as usize;
    if base == 0 {
        return None;
    }
    let nt = read_u32(base + 0x3c)? as usize;
    let header = read(base + nt, 24)?;
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([header[at], header[at + 1]]));
    let sections = u16_at(6).min(96);
    let table = read(base + nt + 24 + u16_at(20), sections * 40)?;
    Some(
        table
            .chunks_exact(40)
            .map(|h| move |at: usize| u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]) as usize)
            .map(|field| (base + field(12), field(8), field(36) as u32 & IMAGE_SCN_MEM_EXECUTE != 0))
            .collect(),
    )
}

/// The code sections of the module at `base`: (offset from its base, size).
pub(crate) fn code_sections(base: usize) -> Option<Vec<(usize, usize)>> {
    layout(base).map(|(code, _)| code)
}

/// Writes `bytes` over code at `at`; false if it couldn't.
pub(crate) fn patch(at: usize, bytes: &[u8]) -> bool {
    let len = bytes.len();
    let mut old = 0;
    if unsafe { VirtualProtect(at as *const c_void, len, PAGE_EXECUTE_READWRITE, &raw mut old) } == 0 {
        return false;
    }
    let mut done = 0;
    let written = unsafe { WriteProcessMemory(GetCurrentProcess(), at as *const c_void, bytes.as_ptr().cast(), len, &raw mut done) } != 0 && done == len;
    unsafe {
        VirtualProtect(at as *const c_void, len, old, &raw mut old);
        FlushInstructionCache(GetCurrentProcess(), at as *const c_void, len);
    }
    written
}
