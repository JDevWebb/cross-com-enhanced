//! Logs an access violation in the game: where it happened, the registers, and the code addresses
//! on the stack (each as module+offset), so a crash report says which of the game's calls it was
//! in. It only logs: the exception goes on to the game's own handlers as before.

use std::ffi::c_void;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use log::error;
use windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION;
use windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler;
use windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS;
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameA;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleExA;
use windows_sys::Win32::System::LibraryLoader::GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS;
use windows_sys::Win32::System::LibraryLoader::GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
use windows_sys::Win32::System::Memory::VirtualQuery;
use windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION;
use windows_sys::Win32::System::Memory::MEM_COMMIT;
use windows_sys::Win32::System::Memory::PAGE_EXECUTE;
use windows_sys::Win32::System::Memory::PAGE_EXECUTE_READ;
use windows_sys::Win32::System::Memory::PAGE_EXECUTE_READWRITE;
use windows_sys::Win32::System::Memory::PAGE_EXECUTE_WRITECOPY;

/// Reports logged so far: the game may fault on purpose now and then, so only the first few.
static REPORTS: AtomicU32 = AtomicU32::new(0);
const MAX_REPORTS: u32 = 8;
/// How many stack words are looked at.
const STACK_WORDS: usize = 512;
/// How many code addresses are listed.
const MAX_FRAMES: usize = 40;
/// "Not handled here".
const CONTINUE_SEARCH: i32 = 0;

pub(crate) fn install() {
    // Called first: the game's own vectored handlers (if any) still see the exception after it.
    if unsafe { AddVectoredExceptionHandler(1, Some(handler)) }.is_null() {
        error!("Couldn't install the crash report");
    }
}

/// The region `at` is in, if it's committed.
fn region(at: usize) -> Option<MEMORY_BASIC_INFORMATION> {
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(at as *const c_void, &raw mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
    (n != 0 && info.State == MEM_COMMIT).then_some(info)
}

fn is_code(at: usize) -> bool {
    region(at).is_some_and(|r| r.Protect & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY) != 0)
}

/// Whether the code just before `at` is a call (so `at` is a return address, not a leftover):
/// `call rel32` (E8, 5 bytes back) or an indirect `call` (FF /2, 2 to 7 bytes back).
fn after_call(at: usize) -> bool {
    if at < 7 || !is_code(at - 7) {
        return false;
    }
    let before = unsafe { std::slice::from_raw_parts((at - 7) as *const u8, 7) };
    before[2] == 0xE8 || (1..=6).any(|back| before[7 - back - 1] == 0xFF && (before[7 - back] >> 3) & 7 == 2)
}

/// `at` as module+offset (the module's file name only), or the bare address.
fn describe(at: usize) -> String {
    let mut module = std::ptr::null_mut();
    let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    if unsafe { GetModuleHandleExA(flags, at as *const u8, &raw mut module) } == 0 || module.is_null() {
        return format!("{at:#010x}");
    }
    let mut name = [0u8; 260];
    let len = unsafe { GetModuleFileNameA(module, name.as_mut_ptr(), name.len() as u32) } as usize;
    let path = String::from_utf8_lossy(&name[..len.min(name.len())]);
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
    format!("{at:#010x} ({file}+{:#x})", at - module as usize)
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION || REPORTS.fetch_add(1, Ordering::Relaxed) >= MAX_REPORTS {
        return CONTINUE_SEARCH;
    }
    let (kind, address) = if record.NumberParameters >= 2 {
        let kind = match record.ExceptionInformation[0] {
            0 => "reading",
            1 => "writing",
            _ => "running",
        };
        (kind, record.ExceptionInformation[1])
    } else {
        ("at", 0)
    };
    error!(
        "Access violation {kind} {address:#010x} at {}: eax={:08x} ebx={:08x} ecx={:08x} edx={:08x} esi={:08x} edi={:08x} ebp={:08x} esp={:08x}",
        describe(record.ExceptionAddress as usize),
        context.Eax,
        context.Ebx,
        context.Ecx,
        context.Edx,
        context.Esi,
        context.Edi,
        context.Ebp,
        context.Esp
    );
    // Words on the stack that point just after a call: the return addresses (and a few leftovers).
    let esp = context.Esp as usize;
    let end = region(esp).map_or(esp, |r| r.BaseAddress as usize + r.RegionSize);
    let words = (end.saturating_sub(esp) / 4).min(STACK_WORDS);
    let frames: Vec<String> = (0..words)
        .filter_map(|i| {
            let word = unsafe { ((esp + i * 4) as *const u32).read_unaligned() } as usize;
            (word > 0x10000 && is_code(word) && after_call(word)).then(|| format!("[esp+{:#x}] {}", i * 4, describe(word)))
        })
        .take(MAX_FRAMES)
        .collect();
    error!("Stack: {}", frames.join(", "));
    CONTINUE_SEARCH
}
