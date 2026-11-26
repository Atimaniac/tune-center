//! Bateria laptopa jak w LibreHardwareMonitor (`Battery`): moc ładowania /
//! rozładowania i temperatura z interfejsu baterii Windows
//! (SetupDi + IOCTL_BATTERY_QUERY_TAG / _QUERY_STATUS / _QUERY_INFORMATION).
//! Bez PawnIO - wystarczają uprawnienia zwykłego użytkownika.

use super::{valid_temp, Readings};

const KIND: &str = "Battery";

/// Identyfikator LHM: `Uri.EscapeDataString` (zostają tylko znaki
/// niezastrzeżone A-Z a-z 0-9 - _ . ~).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Nazwa czujnika mocy wg kierunku przepływu (jak LHM).
fn rate_name(mw: i32) -> &'static str {
    match mw {
        r if r > 0 => "Charge Rate",
        r if r < 0 => "Discharge Rate",
        _ => "Charge/Discharge Rate",
    }
}

/// Stan baterii z jednego odczytu.
struct Status {
    /// mW, dodatnie = ładowanie.
    rate: Option<i32>,
    temp: Option<f32>,
}

#[cfg(windows)]
mod sys {
    //! Interfejs baterii Windows (struktury z poclass.h).
    use std::ffi::c_void;

    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
        SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    };
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    use super::Status;

    const GUID_DEVICE_BATTERY: GUID = GUID::from_u128(0x72631e54_78a4_11d0_bcf7_00aa00b7b32a);
    const IOCTL_BATTERY_QUERY_TAG: u32 = 0x0029_4040;
    const IOCTL_BATTERY_QUERY_INFORMATION: u32 = 0x0029_4044;
    const IOCTL_BATTERY_QUERY_STATUS: u32 = 0x0029_404C;
    const BATTERY_SYSTEM_BATTERY: u32 = 0x8000_0000;
    const BATTERY_UNKNOWN_RATE: u32 = 0x8000_0000;
    const GENERIC_READ_WRITE: u32 = 0xC000_0000;
    // BATTERY_QUERY_INFORMATION_LEVEL
    const INFO: i32 = 0;
    const TEMPERATURE: i32 = 2;
    const DEVICE_NAME: i32 = 4;

    #[repr(C)]
    #[derive(Default)]
    struct QueryInformation {
        tag: u32,
        level: i32,
        at_rate: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Information {
        capabilities: u32,
        technology: u8,
        reserved: [u8; 3],
        chemistry: [u8; 4],
        designed_capacity: u32,
        full_charged_capacity: u32,
        default_alert1: u32,
        default_alert2: u32,
        critical_bias: u32,
        cycle_count: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct WaitStatus {
        tag: u32,
        timeout: u32,
        power_state: u32,
        low_capacity: u32,
        high_capacity: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct BatteryStatus {
        power_state: u32,
        capacity: u32,
        voltage: u32,
        rate: i32,
    }

    /// Wywołanie IOCTL ze strukturami o stałym rozmiarze.
    fn ioctl<I, O>(handle: HANDLE, code: u32, input: &I, output: &mut O) -> bool {
        let mut returned = 0u32;
        // SAFETY: bufory to zmienne o podanych rozmiarach, żyjące przez całe wywołanie.
        unsafe {
            DeviceIoControl(
                handle,
                code,
                Some(input as *const I as *const c_void),
                size_of::<I>() as u32,
                Some(output as *mut O as *mut c_void),
                size_of::<O>() as u32,
                Some(&mut returned),
                None,
            )
            .is_ok()
        }
    }

    /// Otwarta bateria.
    pub struct Device {
        path: Vec<u16>,
        /// Uchwyt jako liczba (HANDLE nie jest Send); 0 = zamknięty.
        handle: isize,
        pub tag: u32,
        pub name: String,
    }

    // SAFETY: uchwyt pliku można używać z dowolnego wątku.
    unsafe impl Send for Device {}

    impl Device {
        fn handle(&self) -> HANDLE {
            HANDLE(self.handle as *mut c_void)
        }

        fn open_handle(path: &[u16]) -> isize {
            // SAFETY: ścieżka zakończona zerem.
            unsafe {
                CreateFileW(
                    PCWSTR(path.as_ptr()),
                    GENERIC_READ_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }
            .map_or(0, |h| h.0 as isize)
        }

        fn close(&mut self) {
            if self.handle != 0 {
                // SAFETY: uchwyt z CreateFileW, zamykany raz.
                unsafe {
                    let _ = CloseHandle(self.handle());
                }
                self.handle = 0;
            }
        }

        fn query_tag(&self) -> Option<u32> {
            let wait = 0u32;
            let mut tag = 0u32;
            (self.handle != 0 && ioctl(self.handle(), IOCTL_BATTERY_QUERY_TAG, &wait, &mut tag) && tag != 0)
                .then_some(tag)
        }

        fn query<O>(&self, level: i32, mut out: O) -> Option<O> {
            let q = QueryInformation { tag: self.tag, level, at_rate: 0 };
            ioctl(self.handle(), IOCTL_BATTERY_QUERY_INFORMATION, &q, &mut out).then_some(out)
        }

        /// Bateria systemowa pod daną ścieżką (inne urządzenia, np. UPS bez
        /// flagi BATTERY_SYSTEM_BATTERY, są pomijane - jak w LHM).
        fn open(path: Vec<u16>) -> Option<Self> {
            let handle = Self::open_handle(&path);
            if handle == 0 {
                return None;
            }
            let mut dev = Self { path, handle, tag: 0, name: String::new() };
            dev.tag = dev.query_tag()?;
            let info = dev.query(INFO, Information::default())?;
            if info.capabilities & BATTERY_SYSTEM_BATTERY == 0 {
                return None;
            }
            let raw = dev.query(DEVICE_NAME, [0u16; 100]).unwrap_or([0; 100]);
            let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            dev.name = String::from_utf16_lossy(&raw[..end]);
            Some(dev)
        }

        fn try_status(&self) -> Option<Status> {
            if self.handle == 0 {
                return None;
            }
            let wait = WaitStatus { tag: self.tag, ..Default::default() };
            let mut st = BatteryStatus::default();
            if !ioctl(self.handle(), IOCTL_BATTERY_QUERY_STATUS, &wait, &mut st) {
                return None;
            }
            let rate = (st.rate as u32 != BATTERY_UNKNOWN_RATE).then_some(st.rate);
            // Dziesiąte części kelwina.
            let temp = self.query(TEMPERATURE, 0u32).map(|t| t as f32 / 10.0 - 273.15);
            Some(Status { rate, temp })
        }

        /// Stan baterii; przy nieważnym znaczniku (zmiana baterii) - nowy
        /// znacznik i jedna ponowna próba, jak w LHM.
        pub fn status(&mut self) -> Option<Status> {
            if let Some(s) = self.try_status() {
                return Some(s);
            }
            if self.handle == 0 {
                self.handle = Self::open_handle(&self.path);
            }
            match self.query_tag() {
                Some(tag) => self.tag = tag,
                None => {
                    self.close();
                    return None;
                }
            }
            self.try_status()
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            self.close();
        }
    }

    /// Wszystkie baterie systemowe.
    pub fn enumerate() -> Vec<Device> {
        let mut out = Vec::new();
        // SAFETY: lista urządzeń zwalniana na końcu; bufory szczegółów mają
        // rozmiar zwrócony przez SetupDiGetDeviceInterfaceDetailW.
        unsafe {
            let Ok(set) = SetupDiGetClassDevsW(
                Some(&GUID_DEVICE_BATTERY),
                PCWSTR::null(),
                None,
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            ) else {
                return out;
            };
            for i in 0..32u32 {
                let mut data = SP_DEVICE_INTERFACE_DATA {
                    cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                    ..Default::default()
                };
                if SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVICE_BATTERY, i, &mut data).is_err() {
                    break;
                }
                let mut needed = 0u32;
                let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut needed), None);
                if (needed as usize) < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
                    continue;
                }
                // Bufor wyrównany do 8 bajtów.
                let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
                let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
                (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                if SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), needed, None, None).is_err() {
                    continue;
                }
                // Ścieżka zaczyna się zaraz po polu cbSize.
                let start = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
                let max = (needed as usize).saturating_sub(4) / 2;
                let chars = std::slice::from_raw_parts(start, max);
                let end = chars.iter().position(|&c| c == 0).unwrap_or(max);
                let path: Vec<u16> = chars[..end].iter().copied().chain([0]).collect();
                if let Some(dev) = Device::open(path) {
                    out.push(dev);
                }
            }
            let _ = SetupDiDestroyDeviceInfoList(set);
        }
        out
    }
}

/// Bateria laptopa (moc ładowania / rozładowania, temperatura).
pub struct Battery {
    #[cfg(windows)]
    devices: Vec<sys::Device>,
}

impl Battery {
    #[cfg(windows)]
    pub fn open() -> Option<Self> {
        let devices = sys::enumerate();
        (!devices.is_empty()).then_some(Self { devices })
    }

    #[cfg(not(windows))]
    pub fn open() -> Option<Self> {
        None
    }

    #[cfg(windows)]
    pub fn read(&mut self, out: &mut Readings) {
        for d in &mut self.devices {
            let status = d.status();
            let hw = d.name.clone();
            let rate = status.as_ref().and_then(|s| s.rate);
            out.power(rate_name(rate.unwrap_or(0)), &hw, KIND, rate.map(|r| (r as f32 / 1000.0).abs()));
            if let Some(t) = status.and_then(|s| s.temp).and_then(valid_temp) {
                let id = format!("/battery/{}_{}/temperature/0", escape(&d.name.replace(' ', "-")), d.tag);
                out.temp(id, "Battery Temperature", &hw, KIND, Some(t));
            }
        }
    }

    #[cfg(not(windows))]
    pub fn read(&mut self, _out: &mut Readings) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(escape("DELL-5XJ28"), "DELL-5XJ28");
        assert_eq!(escape("Bat/1 x"), "Bat%2F1%20x");
        assert_eq!(rate_name(-12000), "Discharge Rate");
        assert_eq!(rate_name(5), "Charge Rate");
        assert_eq!(rate_name(0), "Charge/Discharge Rate");
    }
}

#[cfg(test)]
mod hardware {
    //! `cargo test --release hw_battery -- --ignored --nocapture`
    use super::*;

    #[test]
    #[ignore]
    fn hw_battery() {
        match Battery::open() {
            None => println!("brak baterii / no battery"),
            Some(mut b) => {
                let mut r = Readings::default();
                b.read(&mut r);
                for p in &r.powers {
                    println!("P {} | {} | {} | {:?}", p.name, p.hw, p.kind, p.value);
                }
                for t in &r.temps {
                    println!("{} | {} | {} | {:?}", t.id, t.name, t.hw, t.value);
                }
            }
        }
    }
}
