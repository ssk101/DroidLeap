//! System tray icon with status and the main settings.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::spawn;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::spawn;

use crate::{geometry::Side, log};

const SIDES: [(Side, &str); 4] = [
    (Side::Left, "Left"),
    (Side::Right, "Right"),
    (Side::Top, "Top"),
    (Side::Bottom, "Bottom"),
];

const MENU_TITLE_SIDE: &str = "Device is beyond the screen's";

const APP_TITLE: &str = concat!("DroidLeap ", env!("CARGO_PKG_VERSION"));

/// The app icon as 8-bit RGBA, with its width and height.
fn app_icon_rgba() -> Result<(Vec<u8>, u32, u32), String> {
    decode_png_rgba(include_bytes!("../../../icon.png"))
}

fn decode_png_rgba(png_bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut data = vec![0; reader.output_buffer_size().ok_or("icon too large")?];
    let info = reader.next_frame(&mut data).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "expected 8-bit RGBA, got {:?} {:?}",
            info.color_type, info.bit_depth
        ));
    }
    data.truncate(info.buffer_size());
    Ok((data, info.width, info.height))
}

/// Opens a file with `program`, without waiting for it.
fn open_with(program: &str, path: &std::path::Path) {
    match std::process::Command::new(program).arg(path).spawn() {
        // Reap it in the background so it doesn't linger as a zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => log(format!("cannot open {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_decodes() {
        let (data, width, height) = app_icon_rgba().unwrap();
        assert_eq!((width, height), (256, 256));
        assert_eq!(data.len(), 256 * 256 * 4);
    }
}
