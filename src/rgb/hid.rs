//! Wspólny dostęp do urządzeń USB HID (biblioteka hidapi).

use std::ffi::CString;

pub use hidapi::HidDevice;

pub struct Hid {
    api: hidapi::HidApi,
}

/// Interfejs HID urządzenia (jedno urządzenie USB może mieć ich kilka).
#[derive(Clone, Debug)]
pub struct Iface {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub interface: i32,
    pub path: CString,
    pub product: String,
}

impl Hid {
    pub fn new() -> Result<Self, String> {
        hidapi::HidApi::new()
            .map(|api| Self { api })
            .map_err(|e| format!("HID: {e}"))
    }

    /// Wszystkie interfejsy HID (diagnostyka).
    pub fn list_all(&self) -> Vec<Iface> {
        let mut v: Vec<u16> = self.api.device_list().map(|d| d.vendor_id()).collect();
        v.sort();
        v.dedup();
        v.into_iter().flat_map(|vid| self.list(vid)).collect()
    }

    /// Interfejsy urządzeń danego producenta.
    pub fn list(&self, vid: u16) -> Vec<Iface> {
        self.api
            .device_list()
            .filter(|d| d.vendor_id() == vid)
            .map(|d| Iface {
                vid: d.vendor_id(),
                pid: d.product_id(),
                usage_page: d.usage_page(),
                usage: d.usage(),
                interface: d.interface_number(),
                path: d.path().to_owned(),
                product: d.product_string().unwrap_or_default().trim().to_owned(),
            })
            .collect()
    }

    pub fn open(&self, iface: &Iface) -> Result<HidDevice, String> {
        self.api
            .open_path(&iface.path)
            .map_err(|e| format!("{:04X}:{:04X}: {e}", iface.vid, iface.pid))
    }
}

/// Identyfikator urządzenia w konfiguracji: rodzina + VID:PID (+ numer).
pub fn id(prefix: &str, iface: &Iface, n: usize) -> String {
    if n == 0 {
        format!("{prefix}:{:04X}:{:04X}", iface.vid, iface.pid)
    } else {
        format!("{prefix}:{:04X}:{:04X}:{n}", iface.vid, iface.pid)
    }
}

/// Błąd zapisu HID z nazwą urządzenia.
pub fn err(name: &str, e: hidapi::HidError) -> String {
    format!("{name}: {e}")
}
