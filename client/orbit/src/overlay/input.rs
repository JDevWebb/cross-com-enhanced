//! Keeps the mouse and keyboard from the game while the overlay's panel is open.
//!
//! hudhook holds back the game window's input messages (`ImguiRenderLoop::message_filter`), but
//! the game also reads the mouse and keyboard through DirectInput, and fences the cursor in with
//! `ClipCursor`. So while the panel is open:
//!
//! - the game's DirectInput reads come back empty (no camera turning, no keys);
//! - its `ClipCursor` calls are held back and the cursor is free within the game's window (the
//!   overlay draws its own cursor, as the game hides the system's); when the panel closes, the
//!   clip the game last asked for is put back;
//! - the key that closed the panel (the overlay key or Esc) stays held back until it's let go, so
//!   the game doesn't see it pressed as the panel closes.
//!
//! Should the game's window get no mouse messages (DirectInput can take the mouse for itself),
//! the mouse's DirectInput movement and buttons move the overlay's cursor instead.

use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicIsize;
use std::sync::atomic::AtomicU16;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use log::info;
use log::warn;
use retour::static_detour;
use windows::core::Interface as _;
use windows::core::HRESULT;
use windows::core::PCSTR;
use windows::Win32::Devices::HumanInterfaceDevice::DirectInput8Create;
use windows::Win32::Devices::HumanInterfaceDevice::GUID_SysMouse;
use windows::Win32::Devices::HumanInterfaceDevice::IDirectInput8A;
use windows::Win32::Devices::HumanInterfaceDevice::IDirectInput8W;
use windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8A;
use windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8W;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::System::LibraryLoader::GetProcAddress;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

/// DirectInput 8, as the game uses it.
const DIRECTINPUT_VERSION: u32 = 0x0800;

type GetDeviceStateFn = unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT;
type GetDeviceDataFn = unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut u32, u32) -> HRESULT;
type ClipCursorFn = unsafe extern "system" fn(*const RECT) -> BOOL;

static_detour! {
    static ClipCursorHook: unsafe extern "system" fn(*const RECT) -> BOOL;
    static StateW: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT;
    static DataW: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut u32, u32) -> HRESULT;
    static StateA: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT;
    static DataA: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut u32, u32) -> HRESULT;
}

/// The panel is open.
static OPEN: AtomicBool = AtomicBool::new(false);
/// A key (virtual-key code) held back from the game until it's let go; 0 for none.
static HOLD_KEY: AtomicU16 = AtomicU16::new(0);
/// The clip the game last asked for (None: none), put back when the panel closes.
static GAME_CLIP: Mutex<Option<RECT>> = Mutex::new(None);
/// The game's window, once hudhook has passed on one of its messages.
static GAME_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// The mouse's DirectInput movement while the panel is open, not yet given to the overlay.
static MOUSE_DX: AtomicI32 = AtomicI32::new(0);
static MOUSE_DY: AtomicI32 = AtomicI32::new(0);
static MOUSE_WHEEL: AtomicI32 = AtomicI32::new(0);
/// Its buttons (bit n: button n down), as last read.
static MOUSE_BUTTONS: AtomicU8 = AtomicU8::new(0);

/// Hooks `ClipCursor` and DirectInput's reads. Once, before the overlay is drawn.
pub(super) fn install() {
    // SAFETY: user32 is loaded in every Windows program; the address found is ClipCursor's,
    // whose type `ClipCursorFn` is.
    unsafe {
        match GetModuleHandleA(windows::core::s!("user32.dll"))
            .ok()
            .and_then(|m| GetProcAddress(m, windows::core::s!("ClipCursor")))
        {
            Some(clip) => {
                let clip = std::mem::transmute::<unsafe extern "system" fn() -> isize, ClipCursorFn>(clip);
                match ClipCursorHook.initialize(clip, clip_cursor).and_then(|h| h.enable()) {
                    Ok(()) => info!("Overlay: ClipCursor hooked"),
                    Err(e) => warn!("Overlay: couldn't hook ClipCursor: {e}"),
                }
            }
            None => warn!("Overlay: ClipCursor not found"),
        }
    }
    if !super::library_loads(c"dinput8.dll") {
        warn!("Overlay: dinput8.dll couldn't be loaded; DirectInput isn't held back while the panel is open");
    } else if let Err(e) = hook_direct_input() {
        warn!("Overlay: DirectInput isn't held back while the panel is open: {e}");
    }
}

/// DirectInput's device methods, from a mouse of our own: every device the game makes shares
/// them. The wide and narrow interfaces may have their own.
fn hook_direct_input() -> windows::core::Result<()> {
    let mut hooked: Vec<usize> = Vec::new();
    // SAFETY: DirectInput8Create writes an interface of the IID asked for, or fails; the vtable
    // entries are the methods of the types they're transmuted to (HRESULT is an i32).
    unsafe {
        let instance = GetModuleHandleA(PCSTR::null())?;
        let mut wide: Option<IDirectInput8W> = None;
        DirectInput8Create(instance, DIRECTINPUT_VERSION, &IDirectInput8W::IID, std::ptr::addr_of_mut!(wide).cast(), None)?;
        if let Some(wide) = wide {
            let mut device: Option<IDirectInputDevice8W> = None;
            wide.CreateDevice(&GUID_SysMouse, &mut device, None)?;
            if let Some(device) = device {
                let vtable = device.vtable();
                let state: GetDeviceStateFn = vtable.GetDeviceState;
                let data: GetDeviceDataFn = std::mem::transmute(vtable.GetDeviceData);
                if fresh(&mut hooked, state as usize) {
                    report(
                        "GetDeviceState (W)",
                        StateW.initialize(state, |d, n, b| device_state(&StateW, d, n, b)).and_then(|h| h.enable()),
                    );
                }
                if fresh(&mut hooked, data as usize) {
                    report(
                        "GetDeviceData (W)",
                        DataW.initialize(data, |d, s, b, c, f| device_data(&DataW, d, s, b, c, f)).and_then(|h| h.enable()),
                    );
                }
            }
        }
        let mut narrow: Option<IDirectInput8A> = None;
        DirectInput8Create(instance, DIRECTINPUT_VERSION, &IDirectInput8A::IID, std::ptr::addr_of_mut!(narrow).cast(), None)?;
        if let Some(narrow) = narrow {
            let mut device: Option<IDirectInputDevice8A> = None;
            narrow.CreateDevice(&GUID_SysMouse, &mut device, None)?;
            if let Some(device) = device {
                let vtable = device.vtable();
                let state: GetDeviceStateFn = vtable.GetDeviceState;
                let data: GetDeviceDataFn = std::mem::transmute(vtable.GetDeviceData);
                if fresh(&mut hooked, state as usize) {
                    report(
                        "GetDeviceState (A)",
                        StateA.initialize(state, |d, n, b| device_state(&StateA, d, n, b)).and_then(|h| h.enable()),
                    );
                }
                if fresh(&mut hooked, data as usize) {
                    report(
                        "GetDeviceData (A)",
                        DataA.initialize(data, |d, s, b, c, f| device_data(&DataA, d, s, b, c, f)).and_then(|h| h.enable()),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Whether `address` isn't hooked yet (the wide and narrow interfaces can share a method).
fn fresh(hooked: &mut Vec<usize>, address: usize) -> bool {
    if hooked.contains(&address) {
        return false;
    }
    hooked.push(address);
    true
}

fn report<T>(name: &str, result: retour::Result<T>) {
    match result {
        Ok(_) => info!("Overlay: {name} hooked"),
        Err(e) => warn!("Overlay: couldn't hook {name}: {e}"),
    }
}

/// The game's window, from a message hudhook passed on.
pub(super) fn seen_window(hwnd: HWND) {
    GAME_WINDOW.store(hwnd.0, Ordering::Relaxed);
}

/// Whether the game is in front: the overlay key is read from the whole keyboard, so it only
/// counts then. With no window in front at all (Wine without a window manager) it's taken to be.
pub(super) fn game_in_front() -> bool {
    // SAFETY: plain queries; `pid` is ours to write.
    unsafe {
        let window = GetForegroundWindow();
        if window.0 == 0 {
            return true;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        pid == std::process::id()
    }
}

/// Whether `vk` is held down now.
pub(super) fn key_down(vk: u16) -> bool {
    // SAFETY: a plain query.
    unsafe { GetAsyncKeyState(i32::from(vk)) }.cast_unsigned() & 0x8000 != 0
}

/// The game doesn't see `vk` until it's let go.
pub(super) fn hold_until_released(vk: u16) {
    HOLD_KEY.store(vk, Ordering::SeqCst);
}

/// Whether the game's input is held back: the panel is open, or the key that closed it is still down.
pub(super) fn held_back() -> bool {
    if OPEN.load(Ordering::SeqCst) {
        return true;
    }
    let vk = HOLD_KEY.load(Ordering::SeqCst);
    if vk == 0 {
        return false;
    }
    if key_down(vk) {
        return true;
    }
    HOLD_KEY.store(0, Ordering::SeqCst);
    false
}

/// The panel opened or closed (called every frame).
pub(super) fn set_open(open: bool) {
    if OPEN.swap(open, Ordering::SeqCst) == open {
        return;
    }
    if open {
        MOUSE_DX.store(0, Ordering::SeqCst);
        MOUSE_DY.store(0, Ordering::SeqCst);
        MOUSE_WHEEL.store(0, Ordering::SeqCst);
        // Free within the game's window, so a click can't land on another one.
        set_clip(game_window_rect().as_ref());
    } else {
        // The game's own clip again, but only while it's in front: another program's window
        // mustn't be fenced in.
        let clip = *GAME_CLIP.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        set_clip(if game_in_front() { clip.as_ref() } else { None });
    }
}

/// The game window's client area, on the screen.
fn game_window_rect() -> Option<RECT> {
    // SAFETY: plain queries on a window handle; the points and rect are ours to write.
    unsafe {
        let known = GAME_WINDOW.load(Ordering::Relaxed);
        let window = if known != 0 { HWND(known) } else { GetForegroundWindow() };
        let mut client = RECT::default();
        GetClientRect(window, &mut client).ok()?;
        let mut top_left = POINT { x: client.left, y: client.top };
        let mut bottom_right = POINT {
            x: client.right,
            y: client.bottom,
        };
        let _ = ClientToScreen(window, &mut top_left);
        let _ = ClientToScreen(window, &mut bottom_right);
        Some(RECT {
            left: top_left.x,
            top: top_left.y,
            right: bottom_right.x,
            bottom: bottom_right.y,
        })
    }
}

fn set_clip(rect: Option<&RECT>) {
    if ClipCursorHook.is_enabled() {
        // SAFETY: the real ClipCursor, with a rect or null.
        unsafe { ClipCursorHook.call(rect.map_or(std::ptr::null(), std::ptr::from_ref)) };
    }
}

fn clip_cursor(rect: *const RECT) -> BOOL {
    // SAFETY: the game passes a RECT or null, as ClipCursor takes.
    let wanted = unsafe { rect.as_ref().copied() };
    *GAME_CLIP.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = wanted;
    if OPEN.load(Ordering::SeqCst) {
        return BOOL(1);
    }
    // SAFETY: the real ClipCursor, with what the game passed.
    unsafe { ClipCursorHook.call(rect) }
}

/// `DIMOUSESTATE` and `DIMOUSESTATE2`: the movement (x, y, wheel) as `i32`s, then 4 or 8 buttons.
const MOUSE_STATE_SIZES: [u32; 2] = [16, 20];

/// The device's state, as if nothing moved or was pressed while input is held back.
fn device_state(detour: &retour::StaticDetour<GetDeviceStateFn>, device: *mut c_void, size: u32, buffer: *mut c_void) -> HRESULT {
    // SAFETY: the real method, with what the game passed.
    let result = unsafe { detour.call(device, size, buffer) };
    if result.is_ok() && !buffer.is_null() && held_back() {
        if OPEN.load(Ordering::SeqCst) && MOUSE_STATE_SIZES.contains(&size) {
            // SAFETY: DirectInput filled `size` bytes of mouse state at `buffer`.
            let state = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) };
            note_mouse(state);
        }
        // SAFETY: the caller gave a buffer of `size` bytes for the state.
        unsafe { std::ptr::write_bytes(buffer.cast::<u8>(), 0, size as usize) };
    }
    result
}

/// The device's buffered events: none while input is held back (they're read, so they don't
/// pile up for when the panel closes).
fn device_data(detour: &retour::StaticDetour<GetDeviceDataFn>, device: *mut c_void, size: u32, data: *mut c_void, count: *mut u32, flags: u32) -> HRESULT {
    // SAFETY: the real method, with what the game passed.
    let result = unsafe { detour.call(device, size, data, count, flags) };
    if result.is_ok() && !count.is_null() && held_back() {
        // SAFETY: `count` is the caller's in/out event count.
        unsafe { *count = 0 };
    }
    result
}

fn note_mouse(state: &[u8]) {
    let field = |at: usize| i32::from_le_bytes([state[at], state[at + 1], state[at + 2], state[at + 3]]);
    MOUSE_DX.fetch_add(field(0), Ordering::SeqCst);
    MOUSE_DY.fetch_add(field(4), Ordering::SeqCst);
    MOUSE_WHEEL.fetch_add(field(8), Ordering::SeqCst);
    let buttons = state[12..].iter().take(8).enumerate().fold(0u8, |b, (i, &v)| if v & 0x80 != 0 { b | 1 << i } else { b });
    MOUSE_BUTTONS.store(buttons, Ordering::SeqCst);
}

/// The mouse's DirectInput movement since the last call (x, y, wheel in notches of 120) and its buttons.
pub(super) fn take_mouse() -> (i32, i32, i32, u8) {
    (
        MOUSE_DX.swap(0, Ordering::SeqCst),
        MOUSE_DY.swap(0, Ordering::SeqCst),
        MOUSE_WHEEL.swap(0, Ordering::SeqCst),
        MOUSE_BUTTONS.load(Ordering::SeqCst),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_mouse_state() {
        let mut state = Vec::new();
        state.extend(5i32.to_le_bytes());
        state.extend((-3i32).to_le_bytes());
        state.extend(120i32.to_le_bytes());
        state.extend([0x80, 0, 0x80, 0]);
        let _ = take_mouse();
        note_mouse(&state);
        assert_eq!(take_mouse(), (5, -3, 120, 0b101));
        assert_eq!(take_mouse(), (0, 0, 0, 0b101), "the movement is taken once; the buttons stay as read");
    }
}
