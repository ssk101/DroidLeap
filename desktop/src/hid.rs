//! HID mouse and keyboard reports for Android's AOA2 HID support.
//!
//! The report descriptors and report layouts are taken from scrcpy
//! (app/src/hid/hid_mouse.c, hid_keyboard.c), Copyright (C) 2018 Genymobile,
//! Copyright (C) 2018-2025 Romain Vimont, licensed under the Apache License 2.0.

/// AOA accessory id for the keyboard (same as scrcpy).
pub const KEYBOARD_ID: u16 = 1;
/// AOA accessory id for the mouse (same as scrcpy).
pub const MOUSE_ID: u16 = 2;

/// Boot-style relative mouse: 5 buttons, X/Y, vertical wheel and horizontal pan,
/// each axis a signed byte.
pub const MOUSE_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xA1, 0x01, // Collection (Application)
    0x09, 0x01, //   Usage (Pointer)
    0xA1, 0x00, //   Collection (Physical)
    0x05, 0x09, //     Usage Page (Buttons)
    0x19, 0x01, //     Usage Minimum (1)
    0x29, 0x05, //     Usage Maximum (5)
    0x15, 0x00, //     Logical Minimum (0)
    0x25, 0x01, //     Logical Maximum (1)
    0x95, 0x05, //     Report Count (5)
    0x75, 0x01, //     Report Size (1)
    0x81, 0x02, //     Input (Data, Variable, Absolute)
    0x95, 0x01, //     Report Count (1)
    0x75, 0x03, //     Report Size (3)
    0x81, 0x01, //     Input (Constant): padding
    0x05, 0x01, //     Usage Page (Generic Desktop)
    0x09, 0x30, //     Usage (X)
    0x09, 0x31, //     Usage (Y)
    0x09, 0x38, //     Usage (Wheel)
    0x15, 0x81, //     Logical Minimum (-127)
    0x25, 0x7F, //     Logical Maximum (127)
    0x75, 0x08, //     Report Size (8)
    0x95, 0x03, //     Report Count (3)
    0x81, 0x06, //     Input (Data, Variable, Relative)
    0x05, 0x0C, //     Usage Page (Consumer)
    0x0A, 0x38, 0x02, // Usage (AC Pan)
    0x15, 0x81, //     Logical Minimum (-127)
    0x25, 0x7F, //     Logical Maximum (127)
    0x75, 0x08, //     Report Size (8)
    0x95, 0x01, //     Report Count (1)
    0x81, 0x06, //     Input (Data, Variable, Relative)
    0xC0, //       End Collection
    0xC0, //     End Collection
];

/// Boot-style keyboard: modifier byte, reserved byte, six key slots, LED output.
pub const KEYBOARD_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0xE0, //   Usage Minimum (224)
    0x29, 0xE7, //   Usage Maximum (231)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Variable, Absolute): modifier byte
    0x75, 0x08, //   Report Size (8)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x01, //   Input (Constant): reserved byte
    0x05, 0x08, //   Usage Page (LEDs)
    0x19, 0x01, //   Usage Minimum (1)
    0x29, 0x05, //   Usage Maximum (5)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x05, //   Report Count (5)
    0x91, 0x02, //   Output (Data, Variable, Absolute): LED report
    0x75, 0x03, //   Report Size (3)
    0x95, 0x01, //   Report Count (1)
    0x91, 0x01, //   Output (Constant): LED padding
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x65, //   Logical Maximum (101)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x06, //   Report Count (6)
    0x81, 0x00, //   Input (Data, Array): key slots
    0xC0, //     End Collection
];

pub type MouseReport = [u8; 5];
pub type KeyboardReport = [u8; 8];

/// libei reports smooth scrolling in logical pixels; this many make one wheel step.
/// A guess at a comfortable speed, not a standard.
const SCROLL_PX_PER_STEP: f64 = 15.0;
/// libei's discrete scroll unit: 120 per wheel notch.
const DISCRETE_PER_STEP: f64 = 120.0;

/// Mouse state accumulated over one libei frame, turned into reports by [`Mouse::frame`].
///
/// Inputs use libei's conventions: positive y is down, positive x is right.
#[derive(Default)]
pub struct Mouse {
    buttons: u8,
    buttons_changed: bool,
    /// Motion not yet sent; the fractional part carries over to the next frame.
    motion: (f64, f64),
    /// Scroll not yet sent, in wheel steps; fractions carry over.
    scroll: (f64, f64),
    frame_scroll_px: (f64, f64),
    frame_scroll_discrete: Option<(f64, f64)>,
}

impl Mouse {
    /// Sets an evdev button (BTN_LEFT..BTN_EXTRA). Returns false for buttons
    /// the HID mouse doesn't have.
    pub fn set_button(&mut self, evdev: u32, pressed: bool) -> bool {
        let bit = match evdev {
            0x110 => 0, // BTN_LEFT
            0x111 => 1, // BTN_RIGHT
            0x112 => 2, // BTN_MIDDLE
            0x113 => 3, // BTN_SIDE (back)
            0x114 => 4, // BTN_EXTRA (forward)
            _ => return false,
        };
        let buttons = if pressed {
            self.buttons | 1 << bit
        } else {
            self.buttons & !(1 << bit)
        };
        self.buttons_changed |= buttons != self.buttons;
        self.buttons = buttons;
        true
    }

    pub fn add_motion(&mut self, dx: f64, dy: f64) {
        self.motion.0 += dx;
        self.motion.1 += dy;
    }

    pub fn add_scroll_px(&mut self, dx: f64, dy: f64) {
        self.frame_scroll_px.0 += dx;
        self.frame_scroll_px.1 += dy;
    }

    /// Wheel scrolling in libei's discrete units (120 per notch).
    pub fn add_scroll_discrete(&mut self, dx: i32, dy: i32) {
        let (x, y) = self.frame_scroll_discrete.unwrap_or_default();
        self.frame_scroll_discrete = Some((x + dx as f64, y + dy as f64));
    }

    /// Ends a libei frame: the reports to send, in order. Empty when nothing changed.
    pub fn frame(&mut self) -> Vec<MouseReport> {
        // A wheel may be reported both as discrete and as pixel scrolling in
        // the same frame; prefer the discrete value so it isn't counted twice.
        let steps = match self.frame_scroll_discrete.take() {
            Some((x, y)) => (x / DISCRETE_PER_STEP, y / DISCRETE_PER_STEP),
            None => (
                self.frame_scroll_px.0 / SCROLL_PX_PER_STEP,
                self.frame_scroll_px.1 / SCROLL_PX_PER_STEP,
            ),
        };
        self.frame_scroll_px = (0.0, 0.0);
        self.scroll.0 += steps.0;
        self.scroll.1 += steps.1;

        let mut dx = take_whole(&mut self.motion.0);
        let mut dy = take_whole(&mut self.motion.1);
        // Like scrcpy: at most 127 steps per frame, the rest carries over.
        let hwheel = take_clamped(&mut self.scroll.0);
        // HID wheel is positive away from the user (scroll up); libei's y is positive down.
        let wheel = -take_clamped(&mut self.scroll.1);

        let mut reports = Vec::new();
        if self.buttons_changed || dx != 0 || dy != 0 || wheel != 0 || hwheel != 0 {
            // A fast flick can exceed one report's range: split it instead of losing distance.
            loop {
                let (cx, cy) = (dx.clamp(-127, 127), dy.clamp(-127, 127));
                let (w, h) = if reports.is_empty() {
                    (wheel, hwheel)
                } else {
                    (0, 0)
                };
                reports.push([
                    self.buttons,
                    cx as i8 as u8,
                    cy as i8 as u8,
                    w as i8 as u8,
                    h as i8 as u8,
                ]);
                dx -= cx;
                dy -= cy;
                if dx == 0 && dy == 0 {
                    break;
                }
            }
        }
        self.buttons_changed = false;
        reports
    }

    /// Releases all buttons and drops pending motion. Returns the report to send.
    pub fn release_all(&mut self) -> MouseReport {
        *self = Mouse::default();
        [0; 5]
    }
}

/// Removes and returns the whole part of `value`, rounding toward zero.
fn take_whole(value: &mut f64) -> i64 {
    let whole = value.trunc();
    *value -= whole;
    whole as i64
}

fn take_clamped(value: &mut f64) -> i64 {
    let whole = value.trunc().clamp(-127.0, 127.0);
    *value -= whole;
    whole as i64
}

const MAX_KEYS: usize = 6;
const ERROR_ROLL_OVER: u8 = 0x01;

/// Pressed keys, as a boot keyboard sees them.
#[derive(Default)]
pub struct Keyboard {
    modifiers: u8,
    /// Non-modifier usages in press order.
    pressed: Vec<u8>,
}

impl Keyboard {
    /// Applies a key change given as a HID usage. Returns the report to send,
    /// or `None` if the state didn't change (key repeat, release of an unknown key).
    pub fn key(&mut self, usage: u8, pressed: bool) -> Option<KeyboardReport> {
        let changed = if (0xE0..=0xE7).contains(&usage) {
            let bit = 1 << (usage - 0xE0);
            let modifiers = if pressed {
                self.modifiers | bit
            } else {
                self.modifiers & !bit
            };
            std::mem::replace(&mut self.modifiers, modifiers) != modifiers
        } else if pressed {
            let is_new = !self.pressed.contains(&usage);
            if is_new {
                self.pressed.push(usage);
            }
            is_new
        } else {
            let before = self.pressed.len();
            self.pressed.retain(|&u| u != usage);
            self.pressed.len() != before
        };
        changed.then(|| self.report())
    }

    /// Releases every key. Returns the report to send.
    pub fn release_all(&mut self) -> KeyboardReport {
        *self = Keyboard::default();
        self.report()
    }

    fn report(&self) -> KeyboardReport {
        let mut report = [0; 8];
        report[0] = self.modifiers;
        if self.pressed.len() > MAX_KEYS {
            report[2..].fill(ERROR_ROLL_OVER);
        } else {
            report[2..2 + self.pressed.len()].copy_from_slice(&self.pressed);
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BTN_LEFT: u32 = 0x110;
    const BTN_EXTRA: u32 = 0x114;

    #[test]
    fn descriptor_sizes_match_scrcpy() {
        assert_eq!(MOUSE_DESCRIPTOR.len(), 67);
        assert_eq!(KEYBOARD_DESCRIPTOR.len(), 63);
    }

    #[test]
    fn idle_frame_sends_nothing() {
        assert!(Mouse::default().frame().is_empty());
    }

    #[test]
    fn buttons_set_bits_and_send_once() {
        let mut m = Mouse::default();
        assert!(m.set_button(BTN_LEFT, true));
        assert!(m.set_button(BTN_EXTRA, true));
        assert_eq!(m.frame(), vec![[0b1_0001, 0, 0, 0, 0]]);
        assert!(m.frame().is_empty());
        m.set_button(BTN_LEFT, false);
        assert_eq!(m.frame(), vec![[0b1_0000, 0, 0, 0, 0]]);
    }

    #[test]
    fn unknown_button_is_rejected() {
        let mut m = Mouse::default();
        assert!(!m.set_button(0x115, true));
        assert!(m.frame().is_empty());
    }

    #[test]
    fn pressing_a_held_button_is_not_a_change() {
        let mut m = Mouse::default();
        m.set_button(BTN_LEFT, true);
        m.frame();
        m.set_button(BTN_LEFT, true);
        assert!(m.frame().is_empty());
    }

    #[test]
    fn fractional_motion_carries_over() {
        let mut m = Mouse::default();
        m.add_motion(0.6, -0.6);
        assert!(m.frame().is_empty());
        m.add_motion(0.6, -0.6);
        assert_eq!(m.frame(), vec![[0, 1, (-1i8) as u8, 0, 0]]);
    }

    #[test]
    fn large_motion_is_split_not_clamped() {
        let mut m = Mouse::default();
        m.add_motion(300.0, -10.0);
        let r = m.frame();
        assert_eq!(r.len(), 3);
        let sum_x: i32 = r.iter().map(|r| r[1] as i8 as i32).sum();
        let sum_y: i32 = r.iter().map(|r| r[2] as i8 as i32).sum();
        assert_eq!((sum_x, sum_y), (300, -10));
        assert!(r.iter().all(|r| r[1] as i8 != -128 && r[2] as i8 != -128));
    }

    #[test]
    fn wheel_down_is_negative_hid_wheel() {
        let mut m = Mouse::default();
        m.add_scroll_discrete(0, 120);
        assert_eq!(m.frame(), vec![[0, 0, 0, (-1i8) as u8, 0]]);
        m.add_scroll_discrete(-240, 0);
        assert_eq!(m.frame(), vec![[0, 0, 0, 0, (-2i8) as u8]]);
    }

    #[test]
    fn discrete_wins_over_pixels_in_the_same_frame() {
        let mut m = Mouse::default();
        m.add_scroll_px(0.0, 15.0);
        m.add_scroll_discrete(0, 120);
        assert_eq!(m.frame(), vec![[0, 0, 0, (-1i8) as u8, 0]]);
        assert!(
            m.frame().is_empty(),
            "pixel scroll must not leak into the next frame"
        );
    }

    #[test]
    fn pixel_scroll_accumulates_to_steps() {
        let mut m = Mouse::default();
        m.add_scroll_px(0.0, -10.0);
        assert!(m.frame().is_empty());
        m.add_scroll_px(0.0, -10.0);
        assert_eq!(m.frame(), vec![[0, 0, 0, 1, 0]]);
    }

    #[test]
    fn huge_scroll_is_clamped_and_carried() {
        let mut m = Mouse::default();
        m.add_scroll_discrete(0, -200 * 120);
        assert_eq!(m.frame()[0][3], 127);
        assert_eq!(m.frame()[0][3], 73);
    }

    #[test]
    fn mouse_release_all_clears_everything() {
        let mut m = Mouse::default();
        m.set_button(BTN_LEFT, true);
        m.add_motion(0.9, 0.0);
        assert_eq!(m.release_all(), [0; 5]);
        m.add_motion(0.5, 0.0);
        assert!(m.frame().is_empty(), "old fraction must be dropped");
    }

    #[test]
    fn keys_and_modifiers() {
        let mut k = Keyboard::default();
        assert_eq!(k.key(0xE1, true), Some([0x02, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(k.key(0x04, true), Some([0x02, 0, 0x04, 0, 0, 0, 0, 0]));
        assert_eq!(k.key(0x05, true), Some([0x02, 0, 0x04, 0x05, 0, 0, 0, 0]));
        assert_eq!(k.key(0x04, false), Some([0x02, 0, 0x05, 0, 0, 0, 0, 0]));
        assert_eq!(k.key(0xE1, false), Some([0, 0, 0x05, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn repeats_and_stray_releases_send_nothing() {
        let mut k = Keyboard::default();
        k.key(0x04, true);
        assert_eq!(k.key(0x04, true), None);
        assert_eq!(k.key(0x05, false), None);
        assert_eq!(k.key(0xE0, false), None);
    }

    #[test]
    fn seventh_key_reports_rollover_then_recovers() {
        let mut k = Keyboard::default();
        for u in 0x04..0x0A {
            k.key(u, true);
        }
        assert_eq!(k.key(0x0A, true), Some([0, 0, 1, 1, 1, 1, 1, 1]));
        assert_eq!(
            k.key(0x04, false),
            Some([0, 0, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A])
        );
    }

    #[test]
    fn keyboard_release_all() {
        let mut k = Keyboard::default();
        k.key(0xE0, true);
        k.key(0x04, true);
        assert_eq!(k.release_all(), [0; 8]);
        assert_eq!(k.key(0x04, false), None);
    }
}
