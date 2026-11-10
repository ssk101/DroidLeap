//! Small modal pop-ups: a message box on Windows; zenity or kdialog on Linux
//! when one is installed.

/// Asks a yes/no question. `None` if there is no way to show it.
pub async fn confirm(title: &str, message: &str) -> Option<bool> {
    let (title, message) = (title.to_string(), message.to_string());
    tokio::task::spawn_blocking(move || platform::confirm(&title, &message))
        .await
        .ok()
        .flatten()
}

/// Tells the user something. Returns whether a pop-up could be shown.
pub async fn show(title: &str, message: &str) -> bool {
    let (title, message) = (title.to_string(), message.to_string());
    tokio::task::spawn_blocking(move || platform::show(&title, &message))
        .await
        .unwrap_or(false)
}

#[cfg(windows)]
mod platform {
    use std::ptr;

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
        MessageBoxW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    // Topmost, so it can't hide behind other windows.
    fn message_box(title: &str, message: &str, style: u32) -> i32 {
        unsafe {
            MessageBoxW(
                ptr::null_mut(),
                wide(message).as_ptr(),
                wide(title).as_ptr(),
                style | MB_SETFOREGROUND | MB_TOPMOST,
            )
        }
    }

    pub fn confirm(title: &str, message: &str) -> Option<bool> {
        Some(message_box(title, message, MB_YESNO | MB_ICONQUESTION) == IDYES)
    }

    pub fn show(title: &str, message: &str) -> bool {
        message_box(title, message, MB_OK | MB_ICONINFORMATION) != 0
    }
}

#[cfg(not(windows))]
mod platform {
    use std::{io, process::Command};

    /// Runs the first dialog program that is installed. Returns its exit code.
    fn run(candidates: &[(&str, Vec<&str>)]) -> Option<i32> {
        for (program, args) in candidates {
            match Command::new(program).args(args).status() {
                Ok(status) => return status.code(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
        None
    }

    /// Zenity and kdialog both exit with 0 for yes and 1 for no.
    pub fn confirm(title: &str, message: &str) -> Option<bool> {
        match run(&[
            (
                "zenity",
                vec!["--question", "--title", title, "--text", message],
            ),
            ("kdialog", vec!["--title", title, "--yesno", message]),
        ])? {
            0 => Some(true),
            1 => Some(false),
            _ => None,
        }
    }

    pub fn show(title: &str, message: &str) -> bool {
        run(&[
            (
                "zenity",
                vec!["--info", "--title", title, "--text", message],
            ),
            ("kdialog", vec!["--title", title, "--msgbox", message]),
        ])
        .is_some()
    }
}
