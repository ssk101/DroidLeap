//! The log window: a read-only text box that follows the in-memory log.
//!
//! Everything here runs on the tray thread, which owns the message loop.

use std::{
    ffi::c_void,
    mem, ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{
        CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DEFAULT_QUALITY, FF_MODERN, FIXED_PITCH,
        FW_NORMAL, OUT_DEFAULT_PRECIS,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_GETSEL, EM_LINEINDEX, EM_LINESCROLL,
            EM_REPLACESEL, EM_SETLIMITTEXT, EM_SETSEL,
        },
        HiDpi::GetDpiForSystem,
        Input::KeyboardAndMouse::SetFocus,
        WindowsAndMessaging::{
            CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, ES_AUTOVSCROLL, ES_MULTILINE,
            ES_READONLY, GetClientRect, GetScrollInfo, GetWindowTextLengthW, IDC_ARROW, IsIconic,
            KillTimer, LoadCursorW, MoveWindow, RegisterClassW, SB_BOTTOM, SB_VERT, SCROLLINFO,
            SIF_PAGE, SIF_POS, SIF_RANGE, SW_HIDE, SW_RESTORE, SW_SHOW, SendMessageW,
            SetForegroundWindow, SetTimer, ShowWindow, WM_CLOSE, WM_SETFOCUS, WM_SETFONT,
            WM_SHOWWINDOW, WM_SIZE, WM_TIMER, WM_VSCROLL, WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW,
            WS_VISIBLE, WS_VSCROLL,
        },
    },
};

use crate::logging;

const TITLE: &str = "DroidLeap log";
const REFRESH_TIMER: usize = 1;
const REFRESH_MS: u32 = 250;
/// Once the box holds more lines than this, the oldest [`TRIM_LINES`] go.
const MAX_LINES: isize = 4000;
const TRIM_LINES: usize = 2000;

/// The window and its text box, or null before the first [`show`].
static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static EDIT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// How much of the log the text box already has.
static SEEN: AtomicUsize = AtomicUsize::new(0);

/// Brings the log window to the front, creating it on first use.
pub fn show() {
    unsafe {
        if WINDOW.load(Ordering::Relaxed).is_null()
            && let Err(e) = create()
        {
            crate::log(format!("cannot open the log window: {e}"));
            return;
        }
        let window = WINDOW.load(Ordering::Relaxed);
        refresh();
        ShowWindow(
            window,
            if IsIconic(window) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(window);
    }
}

unsafe fn create() -> Result<(), String> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let class_name = wide("droidleap-log");
    let mut class: WNDCLASSW = unsafe { mem::zeroed() };
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = instance;
    class.hCursor = unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) };
    class.lpszClassName = class_name.as_ptr();
    // Fails harmlessly when the class exists already.
    unsafe { RegisterClassW(&class) };

    let scale = |px: i32| (px as i64 * unsafe { GetDpiForSystem() } as i64 / 96) as i32;
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide(TITLE).as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            scale(900),
            scale(520),
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if window.is_null() {
        return Err("cannot create the window".into());
    }
    let edit = unsafe {
        CreateWindowExW(
            0,
            wide("EDIT").as_ptr(),
            ptr::null(),
            WS_CHILD
                | WS_VISIBLE
                | WS_VSCROLL
                | ES_MULTILINE as u32
                | ES_READONLY as u32
                | ES_AUTOVSCROLL as u32,
            0,
            0,
            0,
            0,
            window,
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    if edit.is_null() {
        return Err("cannot create the text box".into());
    }
    // The font lives as long as the process, like the window.
    let font = unsafe {
        CreateFontW(
            -scale(14),
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            DEFAULT_QUALITY as u32,
            (FIXED_PITCH | FF_MODERN) as u32,
            wide("Consolas").as_ptr(),
        )
    };
    unsafe {
        SendMessageW(edit, WM_SETFONT, font as WPARAM, 1);
        SendMessageW(edit, EM_SETLIMITTEXT, 0, 0); // 0 lifts the default 30000-character cap
    }
    WINDOW.store(window, Ordering::Relaxed);
    EDIT.store(edit, Ordering::Relaxed);
    unsafe { layout(window) };
    Ok(())
}

unsafe fn layout(window: HWND) {
    let edit = EDIT.load(Ordering::Relaxed);
    if edit.is_null() {
        return;
    }
    let mut area: RECT = unsafe { mem::zeroed() };
    unsafe {
        GetClientRect(window, &mut area);
        MoveWindow(edit, 0, 0, area.right, area.bottom, 1);
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SIZE => unsafe { layout(window) },
        WM_SETFOCUS => {
            unsafe { SetFocus(EDIT.load(Ordering::Relaxed)) };
        }
        // The timer runs only while the window is visible.
        WM_SHOWWINDOW => unsafe {
            if wparam != 0 {
                SetTimer(window, REFRESH_TIMER, REFRESH_MS, None);
            } else {
                KillTimer(window, REFRESH_TIMER);
            }
        },
        WM_TIMER => refresh(),
        // Hide instead of destroying, so reopening is instant and keeps the position.
        WM_CLOSE => {
            unsafe { ShowWindow(window, SW_HIDE) };
            return 0;
        }
        _ => {}
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Adds the log lines the text box hasn't got yet.
fn refresh() {
    let edit = EDIT.load(Ordering::Relaxed);
    if edit.is_null() {
        return;
    }
    let (lines, seen) = logging::lines_since(SEEN.load(Ordering::Relaxed));
    SEEN.store(seen, Ordering::Relaxed);
    if !lines.is_empty() {
        unsafe { append(edit, &lines) };
    }
}

/// Appends `lines`, leaving the selection and scroll position alone unless
/// the view was at the end, in which case it follows the new lines.
unsafe fn append(edit: HWND, lines: &[String]) {
    let send = |message, wparam, lparam| unsafe { SendMessageW(edit, message, wparam, lparam) };
    let following = unsafe { at_end(edit) };
    let first_visible = send(EM_GETFIRSTVISIBLELINE, 0, 0);
    let (mut start, mut end) = (0u32, 0u32);
    send(
        EM_GETSEL,
        &mut start as *mut u32 as WPARAM,
        &mut end as *mut u32 as LPARAM,
    );

    let text: Vec<u16> = lines
        .iter()
        .flat_map(|line| line.encode_utf16().chain([b'\r' as u16, b'\n' as u16]))
        .chain([0])
        .collect();
    let length = unsafe { GetWindowTextLengthW(edit) } as WPARAM;
    send(EM_SETSEL, length, length as LPARAM);
    send(EM_REPLACESEL, 0, text.as_ptr() as LPARAM);

    let mut first_visible = first_visible;
    if send(EM_GETLINECOUNT, 0, 0) > MAX_LINES {
        let cut = send(EM_LINEINDEX, TRIM_LINES, 0);
        send(EM_SETSEL, 0, cut);
        send(EM_REPLACESEL, 0, wide("").as_ptr() as LPARAM);
        let cut = cut as u32;
        start = start.saturating_sub(cut);
        end = end.saturating_sub(cut);
        first_visible = (first_visible - TRIM_LINES as isize).max(0);
    }

    send(EM_SETSEL, start as WPARAM, end as LPARAM);
    if following {
        send(WM_VSCROLL, SB_BOTTOM as WPARAM, 0);
    } else {
        send(
            EM_LINESCROLL,
            0,
            first_visible - send(EM_GETFIRSTVISIBLELINE, 0, 0),
        );
    }
}

unsafe fn at_end(edit: HWND) -> bool {
    let mut info: SCROLLINFO = unsafe { mem::zeroed() };
    info.cbSize = mem::size_of::<SCROLLINFO>() as u32;
    info.fMask = SIF_RANGE | SIF_PAGE | SIF_POS;
    unsafe { GetScrollInfo(edit, SB_VERT, &mut info) };
    info.nPos + info.nPage as i32 > info.nMax
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
