//! Keeps the tablet connected over USB: finds it, registers the HID keyboard
//! and mouse, and follows unplugging and replugging.

use std::time::Duration;

use futures_util::StreamExt;
use nusb::{
    DeviceId,
    hotplug::{HotplugEvent, HotplugWatch},
};
use tokio::time::Instant;

use crate::{
    aoa::Tablet,
    hid::{KEYBOARD_DESCRIPTOR, KEYBOARD_ID, MOUSE_DESCRIPTOR, MOUSE_ID},
    log,
};

/// After a USB device appears, udev may need a moment to grant us access.
const SETTLE_DELAY: Duration = Duration::from_secs(1);
const ATTEMPTS_AFTER_PLUG: u32 = 3;
/// Without hotplug events, look for the tablet this often.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What woke [`UsbTablet::wait`]; pass it to [`UsbTablet::handle`].
pub enum Wake {
    Retry,
    Plugged,
    Unplugged(DeviceId),
    HotplugEnded,
}

pub enum Change {
    Connected(String),
    Lost(String),
}

pub struct UsbTablet {
    tablet: Option<Tablet>,
    serial: Option<String>,
    hotplug: Option<HotplugWatch>,
    retry_at: Option<Instant>,
    attempts_left: u32,
    last_error: Option<String>,
}

impl UsbTablet {
    pub fn new(serial: Option<String>) -> UsbTablet {
        let hotplug = nusb::watch_devices()
            .inspect_err(|e| log(format!("USB hotplug unavailable, polling instead: {e}")))
            .ok();
        UsbTablet {
            tablet: None,
            serial,
            hotplug,
            retry_at: Some(Instant::now()),
            attempts_left: ATTEMPTS_AFTER_PLUG,
            last_error: None,
        }
    }

    pub fn get(&self) -> Option<&Tablet> {
        self.tablet.as_ref()
    }

    /// Waits for something that may change the connection. Cancel-safe.
    pub async fn wait(&mut self) -> Wake {
        let retry_at = self.retry_at;
        let retry = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        let hotplug = async {
            match self.hotplug.as_mut() {
                Some(watch) => watch.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = retry => Wake::Retry,
            event = hotplug => match event {
                Some(HotplugEvent::Connected(_)) => Wake::Plugged,
                Some(HotplugEvent::Disconnected(id)) => Wake::Unplugged(id),
                None => Wake::HotplugEnded,
            },
        }
    }

    pub async fn handle(&mut self, wake: Wake) -> Option<Change> {
        match wake {
            Wake::Retry => {
                self.retry_at = None;
                self.connect().await
            }
            Wake::Plugged if self.tablet.is_none() => {
                self.attempts_left = ATTEMPTS_AFTER_PLUG;
                self.retry_at = Some(Instant::now() + SETTLE_DELAY);
                None
            }
            Wake::Unplugged(id) if self.tablet.as_ref().is_some_and(|t| t.id() == id) => {
                Some(self.lost("unplugged".into()))
            }
            Wake::Plugged | Wake::Unplugged(_) => None,
            Wake::HotplugEnded => {
                log("USB hotplug events stopped, polling instead");
                self.hotplug = None;
                if self.tablet.is_none() {
                    self.retry_at = Some(Instant::now() + POLL_INTERVAL);
                }
                None
            }
        }
    }

    /// Forgets the tablet after it stopped answering, and looks for it again.
    pub fn lost(&mut self, reason: String) -> Change {
        // Not closed: it's gone or broken, so unregistering would fail too.
        self.tablet = None;
        self.attempts_left = ATTEMPTS_AFTER_PLUG;
        self.retry_at = Some(Instant::now() + SETTLE_DELAY);
        self.last_error = None;
        Change::Lost(reason)
    }

    /// Switches to another tablet. Returns the change if the current one was dropped.
    pub async fn set_serial(&mut self, serial: Option<String>) -> Option<Change> {
        if serial == self.serial {
            return None;
        }
        self.serial = serial;
        self.last_error = None;
        self.retry_at = Some(Instant::now());
        self.close()
            .await
            .then(|| Change::Lost("a different Android device was selected".into()))
    }

    /// Unregisters the HID devices. Returns whether a tablet was connected.
    pub async fn close(&mut self) -> bool {
        let Some(tablet) = self.tablet.take() else {
            return false;
        };
        if let Err(e) = tablet.close().await {
            log(format!("could not unregister the HID devices: {e}"));
        }
        true
    }

    async fn connect(&mut self) -> Option<Change> {
        if self.tablet.is_some() {
            return None;
        }
        match open(self.serial.as_deref()).await {
            Ok(tablet) => {
                let name = tablet.name().to_string();
                self.tablet = Some(tablet);
                self.last_error = None;
                Some(Change::Connected(name))
            }
            Err(e) => {
                if self.last_error.as_ref() != Some(&e) {
                    log(format!("Android device: {e}"));
                    self.last_error = Some(e);
                }
                if self.hotplug.is_none() {
                    self.retry_at = Some(Instant::now() + POLL_INTERVAL);
                } else if self.attempts_left > 0 {
                    self.attempts_left -= 1;
                    self.retry_at = Some(Instant::now() + SETTLE_DELAY * 2);
                }
                None
            }
        }
    }
}

async fn open(serial: Option<&str>) -> Result<Tablet, String> {
    let mut tablet = Tablet::open(serial).await?;
    let registered = async {
        tablet.register(KEYBOARD_ID, KEYBOARD_DESCRIPTOR).await?;
        tablet.register(MOUSE_ID, MOUSE_DESCRIPTOR).await
    }
    .await;
    match registered {
        Ok(()) => Ok(tablet),
        Err(e) => {
            if let Err(close) = tablet.close().await {
                log(format!("could not unregister the HID devices: {close}"));
            }
            Err(e)
        }
    }
}
