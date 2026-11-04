//! DroidLeap: move the mouse off a screen edge to use it, and the keyboard,
//! on an Android device connected over USB.
//!
//! "Tablet" in the code names that device's role, whether phone or tablet.

pub mod aoa;
pub mod capture;
pub mod config;
pub mod engine;
pub mod geometry;
pub mod hid;
pub mod keymap;
pub mod link;
pub mod logging;
pub mod tablet;
pub mod tray;

pub use logging::log;
