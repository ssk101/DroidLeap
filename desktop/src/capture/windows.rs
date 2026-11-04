//! Windows capture: low-level hooks notice the pointer reaching the edge and
//! swallow input while the tablet has it. Pointer motion comes from raw
//! input, which keeps reporting while the cursor itself stands still.
//!
//! Hook callbacks must return quickly or Windows silently removes the hook,
//! so they only queue events; the engine does the slow work.

use std::{
    ffi::c_void,
    mem, ptr,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    thread::JoinHandle,
};

use tokio::sync::mpsc;
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Input::{
                GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE,
                RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE,
                RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP,
                LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, RegisterClassW,
                SetCursorPos, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
                WM_DISPLAYCHANGE, WM_ENDSESSION, WM_INPUT, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
                WM_QUERYENDSESSION, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
                WM_XBUTTONUP, WNDCLASSW, WS_OVERLAPPED, XBUTTON1,
            },
        },
    },
    core::BOOL,
};

use super::{CaptureError, Input};
use crate::{
    config::Paths,
    geometry::{EdgeBarrier, Rect, Side, outer_barriers, touched_barrier},
    keymap::evdev_from_windows,
};

const VK_PAUSE: u32 = 0x13;

type Inputs = mpsc::UnboundedSender<Result<Input, CaptureError>>;

/// State the hook callbacks read. Hooks have no user data pointer, hence a static.
struct Shared {
    inputs: Mutex<Option<Inputs>>,
    side: Mutex<Option<Side>>,
    barriers: Mutex<Vec<EdgeBarrier>>,
    active: AtomicBool,
    holding: AtomicBool,
}

static SHARED: Shared = Shared {
    inputs: Mutex::new(None),
    side: Mutex::new(None),
    barriers: Mutex::new(Vec::new()),
    active: AtomicBool::new(false),
    holding: AtomicBool::new(false),
};

pub struct Capture {
    inputs: mpsc::UnboundedReceiver<Result<Input, CaptureError>>,
    /// 0 if the hook thread failed to start.
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn open(_paths: &Paths) -> Capture {
        let (sender, inputs) = mpsc::unbounded_channel();
        *lock(&SHARED.inputs) = Some(sender);
        SHARED.active.store(false, Ordering::SeqCst);
        SHARED.holding.store(false, Ordering::SeqCst);
        let (started, thread_id) = std_mpsc::channel();
        let thread = std::thread::spawn(move || hook_thread(started));
        let thread_id = thread_id.recv().unwrap_or(0);
        Capture {
            inputs,
            thread_id,
            thread: Some(thread),
        }
    }

    pub fn set_edge(&self, side: Side) {
        *lock(&SHARED.side) = Some(side);
        send(Ok(Input::Watching(refresh_barriers())));
    }

    pub fn set_active(&self, active: bool) {
        SHARED.active.store(active, Ordering::SeqCst);
    }

    pub fn release(&self, to: (f64, f64)) {
        SHARED.holding.store(false, Ordering::SeqCst);
        unsafe { SetCursorPos(to.0.round() as i32, to.1.round() as i32) };
    }

    pub async fn next(&mut self) -> Result<Input, CaptureError> {
        match self.inputs.recv().await {
            Some(input) => input,
            None => Err(CaptureError::Failed("input hooks stopped".into())),
        }
    }

    pub async fn close(mut self) {
        SHARED.active.store(false, Ordering::SeqCst);
        SHARED.holding.store(false, Ordering::SeqCst);
        if self.thread_id != 0 {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
        *lock(&SHARED.inputs) = None;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn send(input: Result<Input, CaptureError>) {
    if let Some(inputs) = lock(&SHARED.inputs).as_ref() {
        let _ = inputs.send(input); // only fails while the engine is shutting down
    }
}

/// Recomputes the barriers for the watched side. Returns how many.
fn refresh_barriers() -> usize {
    let Some(side) = *lock(&SHARED.side) else {
        return 0;
    };
    let barriers = outer_barriers(&monitors(), side);
    let count = barriers.len();
    *lock(&SHARED.barriers) = barriers;
    count
}

fn monitors() -> Vec<Rect> {
    unsafe extern "system" fn add(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let monitors = unsafe { &mut *(data as *mut Vec<Rect>) };
        let mut info: MONITORINFO = unsafe { mem::zeroed() };
        info.cbSize = mem::size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
            let r = info.rcMonitor;
            monitors.push(Rect {
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
            });
        }
        1
    }
    let mut monitors: Vec<Rect> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(add),
            &mut monitors as *mut _ as LPARAM,
        )
    };
    monitors
}

/// Runs the hooks and a hidden window (raw input, display and logout messages)
/// until `WM_QUIT`. Reports its thread id through `started`, or 0 if setup failed.
fn hook_thread(started: std_mpsc::Sender<u32>) {
    let setup = unsafe { set_up() };
    let (window, hooks) = match setup {
        Ok(handles) => handles,
        Err(e) => {
            send(Err(CaptureError::Failed(e)));
            let _ = started.send(0);
            return;
        }
    };
    let _ = started.send(unsafe { GetCurrentThreadId() });

    let mut message: MSG = unsafe { mem::zeroed() };
    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        unsafe { DispatchMessageW(&message) };
    }

    unsafe {
        for hook in hooks {
            UnhookWindowsHookEx(hook);
        }
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: ptr::null_mut(),
        };
        RegisterRawInputDevices(&device, 1, mem::size_of::<RAWINPUTDEVICE>() as u32);
        DestroyWindow(window);
    }
    send(Err(CaptureError::Failed("input hooks stopped".into())));
}

type Hooks = [windows_sys::Win32::UI::WindowsAndMessaging::HHOOK; 2];

unsafe fn set_up() -> Result<(HWND, Hooks), String> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let class_name: Vec<u16> = "tablet-edge-capture\0".encode_utf16().collect();
    let mut class: WNDCLASSW = unsafe { mem::zeroed() };
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = instance;
    class.lpszClassName = class_name.as_ptr();
    // Fails harmlessly when the class exists from an earlier session.
    unsafe { RegisterClassW(&class) };
    // A hidden top-level window: it receives raw input and the logout messages.
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if window.is_null() {
        return Err("cannot create the input window".into());
    }
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: window,
    };
    if unsafe { RegisterRawInputDevices(&device, 1, mem::size_of::<RAWINPUTDEVICE>() as u32) } == 0
    {
        unsafe { DestroyWindow(window) };
        return Err("cannot register for raw mouse input".into());
    }
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0) };
    let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0) };
    if mouse.is_null() || keyboard.is_null() {
        unsafe {
            UnhookWindowsHookEx(mouse);
            UnhookWindowsHookEx(keyboard);
            DestroyWindow(window);
        }
        return Err("cannot install the mouse and keyboard hooks".into());
    }
    Ok((window, [mouse, keyboard]))
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_INPUT => {
            if SHARED.holding.load(Ordering::SeqCst)
                && let Some((dx, dy)) = unsafe { raw_motion(lparam as HRAWINPUT) }
            {
                send(Ok(Input::Motion { dx, dy }));
                send(Ok(Input::Frame));
            }
        }
        WM_DISPLAYCHANGE => {
            send(Ok(Input::MonitorsChanged));
            send(Ok(Input::Watching(refresh_barriers())));
            return 0;
        }
        WM_QUERYENDSESSION => return 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                send(Ok(Input::Quit));
            }
            return 0;
        }
        _ => {}
    }
    // Also needed after WM_INPUT, to free the raw input data.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Relative pointer motion in a raw input message; absolute devices (pens, remote desktop) give none.
unsafe fn raw_motion(handle: HRAWINPUT) -> Option<(f64, f64)> {
    let mut raw: RAWINPUT = unsafe { mem::zeroed() };
    let mut size = mem::size_of::<RAWINPUT>() as u32;
    let header_size = mem::size_of::<RAWINPUTHEADER>() as u32;
    let read = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            &mut raw as *mut _ as *mut c_void,
            &mut size,
            header_size,
        )
    };
    if read == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE {
        return None;
    }
    let mouse = unsafe { raw.data.mouse };
    if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 || (mouse.lLastX == 0 && mouse.lLastY == 0) {
        return None;
    }
    Some((mouse.lLastX as f64, mouse.lLastY as f64))
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 && on_mouse(wparam as u32, info) {
            return 1;
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// Returns whether to swallow the event.
fn on_mouse(message: u32, info: &MSLLHOOKSTRUCT) -> bool {
    if SHARED.holding.load(Ordering::SeqCst) {
        let wheel = (info.mouseData >> 16) as i16 as i32;
        let x_button = if (info.mouseData >> 16) as u16 == XBUTTON1 {
            0x113
        } else {
            0x114
        };
        let input = match message {
            WM_LBUTTONDOWN | WM_LBUTTONUP => Some(Input::Button {
                code: 0x110,
                pressed: message == WM_LBUTTONDOWN,
            }),
            WM_RBUTTONDOWN | WM_RBUTTONUP => Some(Input::Button {
                code: 0x111,
                pressed: message == WM_RBUTTONDOWN,
            }),
            WM_MBUTTONDOWN | WM_MBUTTONUP => Some(Input::Button {
                code: 0x112,
                pressed: message == WM_MBUTTONDOWN,
            }),
            WM_XBUTTONDOWN | WM_XBUTTONUP => Some(Input::Button {
                code: x_button,
                pressed: message == WM_XBUTTONDOWN,
            }),
            // Windows wheels count positive away from the user (up); libei counts positive down.
            WM_MOUSEWHEEL => Some(Input::ScrollSteps { dx: 0, dy: -wheel }),
            WM_MOUSEHWHEEL => Some(Input::ScrollSteps { dx: wheel, dy: 0 }),
            _ => None, // motion arrives through raw input
        };
        if let Some(input) = input {
            send(Ok(input));
            send(Ok(Input::Frame));
        }
        return true;
    }
    if message != WM_MOUSEMOVE || !SHARED.active.load(Ordering::SeqCst) {
        return false;
    }
    let Some(side) = *lock(&SHARED.side) else {
        return false;
    };
    let position = (info.pt.x, info.pt.y);
    let Some(monitor) = touched_barrier(&lock(&SHARED.barriers), side, position).map(|b| b.monitor)
    else {
        return false;
    };
    SHARED.holding.store(true, Ordering::SeqCst);
    send(Ok(Input::EdgeTouched {
        position: (position.0 as f64, position.1 as f64),
        monitor,
    }));
    true
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && SHARED.holding.load(Ordering::SeqCst) {
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if info.flags & LLKHF_INJECTED == 0 {
            let pressed = info.flags & LLKHF_UP == 0;
            if pressed && info.vkCode == VK_PAUSE {
                // Give the desktop its input back even if the engine is stuck.
                SHARED.holding.store(false, Ordering::SeqCst);
            }
            if let Some(key) =
                evdev_from_windows(info.scanCode, info.flags & LLKHF_EXTENDED != 0, info.vkCode)
            {
                send(Ok(Input::Key { code: key, pressed }));
            }
            return 1;
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}
