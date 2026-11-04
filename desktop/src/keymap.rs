//! Linux evdev key codes (what libei reports) to USB HID keyboard usages
//! (HID Usage Tables, page 0x07).

/// HID usage for an evdev key code, or `None` for keys the HID keyboard
/// descriptor cannot express (media keys, volume, power, ...).
pub fn hid_usage(evdev: u32) -> Option<u8> {
    let usage = match evdev {
        1 => 0x29,                          // Esc
        2..=10 => evdev as u8 + 0x1C,       // 1..9
        11 => 0x27,                         // 0
        12 => 0x2D,                         // -
        13 => 0x2E,                         // =
        14 => 0x2A,                         // Backspace
        15 => 0x2B,                         // Tab
        16 => 0x14,                         // Q
        17 => 0x1A,                         // W
        18 => 0x08,                         // E
        19 => 0x15,                         // R
        20 => 0x17,                         // T
        21 => 0x1C,                         // Y
        22 => 0x18,                         // U
        23 => 0x0C,                         // I
        24 => 0x12,                         // O
        25 => 0x13,                         // P
        26 => 0x2F,                         // [
        27 => 0x30,                         // ]
        28 => 0x28,                         // Enter
        29 => 0xE0,                         // Left Ctrl
        30 => 0x04,                         // A
        31 => 0x16,                         // S
        32 => 0x07,                         // D
        33 => 0x09,                         // F
        34 => 0x0A,                         // G
        35 => 0x0B,                         // H
        36 => 0x0D,                         // J
        37 => 0x0E,                         // K
        38 => 0x0F,                         // L
        39 => 0x33,                         // ;
        40 => 0x34,                         // '
        41 => 0x35,                         // `
        42 => 0xE1,                         // Left Shift
        43 => 0x31,                         // backslash
        44 => 0x1D,                         // Z
        45 => 0x1B,                         // X
        46 => 0x06,                         // C
        47 => 0x19,                         // V
        48 => 0x05,                         // B
        49 => 0x11,                         // N
        50 => 0x10,                         // M
        51 => 0x36,                         // ,
        52 => 0x37,                         // .
        53 => 0x38,                         // /
        54 => 0xE5,                         // Right Shift
        55 => 0x55,                         // Keypad *
        56 => 0xE2,                         // Left Alt
        57 => 0x2C,                         // Space
        58 => 0x39,                         // Caps Lock
        59..=68 => evdev as u8 - 59 + 0x3A, // F1..F10
        69 => 0x53,                         // Num Lock
        70 => 0x47,                         // Scroll Lock
        71 => 0x5F,                         // Keypad 7
        72 => 0x60,                         // Keypad 8
        73 => 0x61,                         // Keypad 9
        74 => 0x56,                         // Keypad -
        75 => 0x5C,                         // Keypad 4
        76 => 0x5D,                         // Keypad 5
        77 => 0x5E,                         // Keypad 6
        78 => 0x57,                         // Keypad +
        79 => 0x59,                         // Keypad 1
        80 => 0x5A,                         // Keypad 2
        81 => 0x5B,                         // Keypad 3
        82 => 0x62,                         // Keypad 0
        83 => 0x63,                         // Keypad .
        86 => 0x64,                         // Non-US backslash
        87 => 0x44,                         // F11
        88 => 0x45,                         // F12
        96 => 0x58,                         // Keypad Enter
        97 => 0xE4,                         // Right Ctrl
        98 => 0x54,                         // Keypad /
        99 => 0x46,                         // Print Screen / SysRq
        100 => 0xE6,                        // Right Alt (AltGr)
        102 => 0x4A,                        // Home
        103 => 0x52,                        // Up
        104 => 0x4B,                        // Page Up
        105 => 0x50,                        // Left
        106 => 0x4F,                        // Right
        107 => 0x4D,                        // End
        108 => 0x51,                        // Down
        109 => 0x4E,                        // Page Down
        110 => 0x49,                        // Insert
        111 => 0x4C,                        // Delete
        119 => 0x48,                        // Pause
        125 => 0xE3,                        // Left Meta
        126 => 0xE7,                        // Right Meta
        127 => 0x65,                        // Compose / Menu (Application)
        _ => return None,
    };
    Some(usage)
}

/// evdev code for a Windows key event, from its set-1 scancode, extended
/// (E0) flag and virtual-key code. Set-1 scancodes without E0 equal evdev codes.
pub fn evdev_from_windows(scan_code: u32, extended: bool, virtual_key: u32) -> Option<u32> {
    const VK_PAUSE: u32 = 0x13;
    // Pause sends E1 1D 45, which Windows reports with Num Lock's scancode.
    if virtual_key == VK_PAUSE {
        return Some(119);
    }
    if !extended {
        return (0x01..=0x58).contains(&scan_code).then_some(scan_code);
    }
    let evdev = match scan_code {
        0x1C => 96,  // Keypad Enter
        0x1D => 97,  // Right Ctrl
        0x35 => 98,  // Keypad /
        0x37 => 99,  // Print Screen
        0x38 => 100, // Right Alt
        0x45 => 69,  // Num Lock
        0x47 => 102, // Home
        0x48 => 103, // Up
        0x49 => 104, // Page Up
        0x4B => 105, // Left
        0x4D => 106, // Right
        0x4F => 107, // End
        0x50 => 108, // Down
        0x51 => 109, // Page Down
        0x52 => 110, // Insert
        0x53 => 111, // Delete
        0x5B => 125, // Left Windows
        0x5C => 126, // Right Windows
        0x5D => 127, // Menu
        _ => return None,
    };
    Some(evdev)
}

#[cfg(test)]
mod tests {
    use super::{evdev_from_windows, hid_usage};

    #[test]
    fn letter_rows() {
        let qwerty = [(16, "QWERTYUIOP"), (30, "ASDFGHJKL"), (44, "ZXCVBNM")];
        for (first, row) in qwerty {
            for (i, letter) in row.bytes().enumerate() {
                let expected = 0x04 + (letter - b'A');
                assert_eq!(
                    hid_usage(first + i as u32),
                    Some(expected),
                    "{}",
                    letter as char
                );
            }
        }
    }

    #[test]
    fn digits_and_function_keys() {
        let digits: Vec<_> = (2..=11).map(|k| hid_usage(k).unwrap()).collect();
        assert_eq!(digits, (0x1E..=0x27).collect::<Vec<u8>>());
        let f1_to_f12: Vec<_> = (59..=68)
            .chain([87, 88])
            .map(|k| hid_usage(k).unwrap())
            .collect();
        assert_eq!(f1_to_f12, (0x3A..=0x45).collect::<Vec<u8>>());
    }

    #[test]
    fn modifiers_map_to_e0_e7() {
        let mods = [29, 42, 56, 125, 97, 54, 100, 126];
        let usages: Vec<_> = mods.iter().map(|&k| hid_usage(k).unwrap()).collect();
        assert_eq!(usages, (0xE0..=0xE7).collect::<Vec<u8>>());
    }

    #[test]
    fn usages_fit_the_descriptor() {
        // The keyboard descriptor declares usages 0x00..=0x65 plus modifiers.
        for k in 0..1024 {
            if let Some(u) = hid_usage(k) {
                assert!(
                    u <= 0x65 || (0xE0..=0xE7).contains(&u),
                    "evdev {k} -> {u:#x}"
                );
            }
        }
    }

    #[test]
    fn unmapped_keys() {
        assert_eq!(hid_usage(0), None);
        assert_eq!(hid_usage(113), None); // KEY_MUTE
        assert_eq!(hid_usage(115), None); // KEY_VOLUMEUP
    }

    #[test]
    fn windows_scancodes() {
        assert_eq!(evdev_from_windows(0x1E, false, 0x41), Some(30)); // A
        assert_eq!(evdev_from_windows(0x1D, false, 0xA2), Some(29)); // Left Ctrl
        assert_eq!(evdev_from_windows(0x1D, true, 0xA3), Some(97)); // Right Ctrl
        assert_eq!(evdev_from_windows(0x48, false, 0x68), Some(72)); // Keypad 8
        assert_eq!(evdev_from_windows(0x48, true, 0x26), Some(103)); // Up arrow
        assert_eq!(evdev_from_windows(0x45, true, 0x90), Some(69)); // Num Lock
        assert_eq!(evdev_from_windows(0x45, false, 0x13), Some(119)); // Pause
        assert_eq!(evdev_from_windows(0x5B, true, 0x5B), Some(125)); // Windows key
        assert_eq!(evdev_from_windows(0x00, false, 0xB3), None); // media key
    }

    #[test]
    fn windows_keys_map_to_hid_usages() {
        for scan in [
            0x01, 0x0E, 0x1C, 0x2A, 0x39, 0x3B, 0x44, 0x45, 0x47, 0x53, 0x56, 0x57, 0x58,
        ] {
            let evdev = evdev_from_windows(scan, false, 0).unwrap();
            assert!(hid_usage(evdev).is_some(), "scancode {scan:#x}");
        }
        for scan in [
            0x1C, 0x1D, 0x35, 0x37, 0x38, 0x45, 0x47, 0x48, 0x49, 0x4B, 0x4D, 0x4F, 0x50, 0x51,
            0x52, 0x53, 0x5B, 0x5C, 0x5D,
        ] {
            let evdev = evdev_from_windows(scan, true, 0).unwrap();
            assert!(hid_usage(evdev).is_some(), "extended scancode {scan:#x}");
        }
    }
}
