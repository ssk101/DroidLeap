//! Android Open Accessory 2.0 HID: the tablet sees us as a USB mouse and
//! keyboard, with no app or USB debugging needed on the tablet.
//! <https://source.android.com/docs/core/interaction/accessories/aoa2#hid-support>
//!
//! On Windows, control transfers must go through a claimed interface with the
//! WinUSB driver; the ADB interface is the one that usually has it.

use std::time::Duration;

use nusb::{
    DeviceInfo,
    transfer::{ControlIn, ControlOut, ControlType, Recipient},
};

const GET_PROTOCOL: u8 = 51;
const REGISTER_HID: u8 = 54;
const UNREGISTER_HID: u8 = 55;
const SET_HID_REPORT_DESC: u8 = 56;
const SEND_HID_EVENT: u8 = 57;
const TIMEOUT: Duration = Duration::from_secs(1);

/// ADB's USB interface (vendor class 0xFF, subclass 0x42, protocol 1).
const ADB_INTERFACE: (u8, u8, u8) = (0xFF, 0x42, 0x01);

/// What control transfers go through.
#[cfg(windows)]
type ControlPipe = nusb::Interface;
/// What control transfers go through. Not an interface: claiming ADB's would cut off adb.
#[cfg(not(windows))]
type ControlPipe = nusb::Device;

/// An Android device with HID devices registered over AOA.
pub struct Tablet {
    control: ControlPipe,
    id: nusb::DeviceId,
    name: String,
    registered: Vec<u16>,
}

impl Tablet {
    /// Opens the device with the given USB serial number. Without a serial,
    /// opens the only connected device that exposes ADB.
    pub async fn open(serial: Option<&str>) -> Result<Tablet, String> {
        let info = find(serial).await?;
        let name = describe(&info);
        let device = info
            .open()
            .await
            .map_err(|e| format!("cannot open {name}: {e} (USB permissions?)"))?;
        let tablet = Tablet {
            control: control_pipe(&info, device, &name).await?,
            id: info.id(),
            name,
            registered: Vec::new(),
        };
        let version = tablet.protocol_version().await?;
        if version < 2 {
            return Err(format!(
                "{} supports AOA version {version}; HID input needs version 2 (Android 4.1+)",
                tablet.name
            ));
        }
        Ok(tablet)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Identifies the device in hotplug events.
    pub fn id(&self) -> nusb::DeviceId {
        self.id
    }

    /// Registers a HID device and uploads its report descriptor.
    pub async fn register(&mut self, id: u16, descriptor: &[u8]) -> Result<(), String> {
        let len = u16::try_from(descriptor.len()).map_err(|_| "HID descriptor too long")?;
        self.control_out(REGISTER_HID, id, len, &[]).await?;
        // Track it before uploading the descriptor so close() also cleans up a half-registered device.
        self.registered.push(id);
        self.control_out(SET_HID_REPORT_DESC, id, 0, descriptor)
            .await
    }

    pub async fn send(&self, id: u16, report: &[u8]) -> Result<(), String> {
        self.control_out(SEND_HID_EVENT, id, 0, report).await
    }

    /// Unregisters every HID device, so the tablet stops seeing a mouse and keyboard.
    pub async fn close(self) -> Result<(), String> {
        let mut first_error = None;
        for &id in self.registered.iter().rev() {
            if let Err(e) = self.control_out(UNREGISTER_HID, id, 0, &[]).await {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn protocol_version(&self) -> Result<u16, String> {
        let data = self
            .control
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: GET_PROTOCOL,
                    value: 0,
                    index: 0,
                    length: 2,
                },
                TIMEOUT,
            )
            .await
            .map_err(|e| format!("{} did not answer the AOA protocol query: {e}", self.name))?;
        match data[..] {
            [lo, hi] => Ok(u16::from_le_bytes([lo, hi])),
            _ => Err(format!(
                "{} sent a malformed AOA protocol version: {data:?}",
                self.name
            )),
        }
    }

    async fn control_out(
        &self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), String> {
        self.control
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    data,
                },
                TIMEOUT,
            )
            .await
            .map_err(|e| {
                format!(
                    "AOA request {request} (HID id {value}) to {}: {e}",
                    self.name
                )
            })
    }
}

async fn find(serial: Option<&str>) -> Result<DeviceInfo, String> {
    let devices: Vec<DeviceInfo> = nusb::list_devices()
        .await
        .map_err(|e| format!("cannot list USB devices: {e}"))?
        .collect();

    if let Some(serial) = serial {
        return devices
            .into_iter()
            .find(|d| d.serial_number() == Some(serial))
            .ok_or_else(|| format!("no USB device with serial {serial}"));
    }

    let mut android: Vec<DeviceInfo> = devices
        .into_iter()
        .filter(|d| {
            d.interfaces()
                .any(|i| (i.class(), i.subclass(), i.protocol()) == ADB_INTERFACE)
        })
        .collect();
    match android.len() {
        1 => Ok(android.remove(0)),
        0 => Err(
            "no Android device with USB debugging found; plug in the tablet, or give its \
                  USB serial number (see `lsusb -v`)"
                .into(),
        ),
        _ => Err(format!(
            "several Android devices found, give a serial number: {}",
            android.iter().map(describe).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn describe(d: &DeviceInfo) -> String {
    format!(
        "{} {} ({:04x}:{:04x}, serial {})",
        d.manufacturer_string().unwrap_or("?"),
        d.product_string().unwrap_or("?"),
        d.vendor_id(),
        d.product_id(),
        d.serial_number().unwrap_or("none"),
    )
}

#[cfg(windows)]
async fn control_pipe(
    info: &DeviceInfo,
    device: nusb::Device,
    name: &str,
) -> Result<ControlPipe, String> {
    let number = info
        .interfaces()
        .find(|i| (i.class(), i.subclass(), i.protocol()) == ADB_INTERFACE)
        .map(|i| i.interface_number())
        .ok_or_else(|| {
            format!("{name} shows no ADB interface: turn on USB debugging on the tablet")
        })?;
    device.claim_interface(number).await.map_err(|e| {
        format!(
            "cannot use the ADB interface of {name}: {e}. Close adb and Android Studio if they run; \
             otherwise its driver isn't WinUSB-based"
        )
    })
}

#[cfg(not(windows))]
async fn control_pipe(
    _info: &DeviceInfo,
    device: nusb::Device,
    _name: &str,
) -> Result<ControlPipe, String> {
    Ok(device)
}
