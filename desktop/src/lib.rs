//! tablet-edge: move the mouse off a screen edge to use it, and the keyboard,
//! on an Android tablet connected over USB.

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
