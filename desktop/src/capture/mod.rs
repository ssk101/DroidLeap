//! Per-OS input capture. Notices the pointer reaching the watched desktop
//! edge, holds the mouse and keyboard while they belong to the tablet, and
//! reports their input in one form: evdev codes, libei sign conventions.
//!
//! Each OS module provides a `Capture` with the same methods:
//! - `open(paths)` starts capturing in the background,
//! - `set_edge(side)` picks the edge to watch; answered by [`Input::Watching`],
//! - `set_active(bool)` arms or disarms the edge,
//! - `set_return_key(evdev)` names the key that ends a hold; backends that can
//!   end the hold themselves do, so a stuck engine can't keep the input,
//! - `release(to)` ends a hold and puts the pointer at `to`,
//! - `next()` waits for the next [`Input`],
//! - `close()` stops.
//!
//! Requests don't wait for the OS; failures surface through `next()`.

use crate::geometry::Rect;

#[derive(Debug)]
pub enum Input {
    /// How many monitors have the watched edge on the outside of the desktop.
    Watching(usize),
    /// The pointer reached the edge. Input is held from here until `release`.
    EdgeTouched {
        position: (f64, f64),
        monitor: Rect,
    },
    Motion {
        dx: f64,
        dy: f64,
    },
    /// `code` is an evdev button code (BTN_LEFT = 0x110, ...).
    Button {
        code: u32,
        pressed: bool,
    },
    /// Positive is down or right.
    ScrollPixels {
        dx: f64,
        dy: f64,
    },
    /// 120 per wheel notch; positive is down or right.
    ScrollSteps {
        dx: i32,
        dy: i32,
    },
    /// `code` is an evdev key code.
    Key {
        code: u32,
        pressed: bool,
    },
    /// Ends a group of input that happened together.
    Frame,
    /// The desktop ended the hold on its own.
    Released,
    /// Monitors were added, removed or moved; a [`Input::Watching`] follows.
    MonitorsChanged,
    /// The desktop is asking the user for permission; a [`Input::Watching`] follows if granted.
    AwaitingPermission,
    /// The user is logging out.
    Quit,
}

#[derive(Debug)]
pub enum CaptureError {
    /// The user refused to let the app capture input.
    Denied,
    Failed(String),
}

impl From<String> for CaptureError {
    fn from(e: String) -> Self {
        CaptureError::Failed(e)
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Capture;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::Capture;
