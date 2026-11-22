//! Temperatury dysków odczytywane wprost przez API Windows
//! (`IOCTL_STORAGE_QUERY_PROPERTY`, właściwość `StorageDeviceTemperatureProperty`).
//!
//! Nie wymaga sterownika PawnIO ani procesu czujników. Działa dla dysków,
//! których sterownik w Windows podaje temperaturę - przede wszystkim NVMe;
//! część dysków SATA jej tą drogą nie udostępnia i wtedy zostaje odczyt z
//! procesu czujników. Odczyt wykonuje osobny wątek co kilka sekund.

use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Disk {
    pub name: String,
    pub temp_c: f32,
}

pub struct Disks {
    list: Arc<Mutex<Vec<Disk>>>,
}

/// Odstęp między odczytami. Temperatura dysku zmienia się powoli.
const INTERVAL: Duration = Duration::from_secs(10);

impl Disks {
    pub fn start() -> Self {
        let list = Arc::new(Mutex::new(Vec::new()));
        let shared = list.clone();
        crate::spawn("disks", move || loop {
            let found = imp::read_all();
            if let Ok(mut guard) = shared.lock() {
                *guard = found;
            }
            std::thread::sleep(INTERVAL);
        });
        Self { list }
    }

    pub fn list(&self) -> Vec<Disk> {
        self.list.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

fn u16_at(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(buf.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(buf.get(at..at + 4)?.try_into().ok()?))
}

/// Najwyższa temperatura (°C) z odpowiedzi STORAGE_TEMPERATURE_DATA_DESCRIPTOR.
///
/// Układ: Version u32, Size u32, CriticalTemperature i16, WarningTemperature i16,
/// InfoCount u16, Reserved0 [u8; 2], Reserved1 [u32; 2], potem InfoCount wpisów
/// STORAGE_TEMPERATURE_INFO po 16 bajtów (Index u16, Temperature i16, ...).
pub fn parse_temperature(buf: &[u8]) -> Option<f32> {
    const HEADER: usize = 24;
    const ENTRY: usize = 16;
    let count = u16_at(buf, 12)? as usize;
    let mut best: Option<i16> = None;
    for i in 0..count.min(32) {
        let Some(raw) = u16_at(buf, HEADER + i * ENTRY + 2) else {
            break;
        };
        let t = raw as i16;
        // Zero i wartości spoza rozsądnego zakresu oznaczają brak odczytu.
        if t > 0 && t < 150 && best.is_none_or(|b| t > b) {
            best = Some(t);
        }
    }
    best.map(f32::from)
}

/// Nazwa dysku z odpowiedzi STORAGE_DEVICE_DESCRIPTOR: producent i model.
/// VendorIdOffset jest pod adresem 12, ProductIdOffset pod 16; zero = brak.
pub fn parse_name(buf: &[u8]) -> Option<String> {
    let text_at = |offset_at: usize| -> String {
        let offset = u32_at(buf, offset_at).unwrap_or(0) as usize;
        if offset == 0 || offset >= buf.len() {
            return String::new();
        }
        let bytes = &buf[offset..];
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
    };
    let vendor = text_at(12);
    let product = text_at(16);
    let name = match (vendor.is_empty(), product.is_empty()) {
        (_, true) => vendor,
        (true, false) => product,
        (false, false) if product.to_lowercase().starts_with(&vendor.to_lowercase()) => product,
        (false, false) => format!("{vendor} {product}"),
    };
    (!name.is_empty()).then_some(name)
}

#[cfg(windows)]
mod imp {
    use super::{parse_name, parse_temperature, Disk};
    use std::ffi::c_void;

    const FILE_SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE: isize = -1;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const STORAGE_DEVICE_PROPERTY: u32 = 0;
    const STORAGE_DEVICE_TEMPERATURE_PROPERTY: u32 = 52;
    /// Największy numer sprawdzanego dysku (\\.\PhysicalDriveN).
    const MAX_DRIVES: u32 = 24;

    /// STORAGE_PROPERTY_QUERY: PropertyId, QueryType (0 = standardowe), parametry.
    #[repr(C)]
    struct Query {
        property: u32,
        query_type: u32,
        extra: [u8; 4],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const c_void,
            disposition: u32,
            flags: u32,
            template: isize,
        ) -> isize;
        fn DeviceIoControl(
            device: isize,
            code: u32,
            input: *const c_void,
            input_size: u32,
            output: *mut c_void,
            output_size: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    /// Wykonuje zapytanie o właściwość; zwraca wypełnioną część bufora.
    fn query(handle: isize, property: u32) -> Option<Vec<u8>> {
        let request = Query {
            property,
            query_type: 0,
            extra: [0; 4],
        };
        let mut buf = vec![0u8; 1024];
        let mut returned = 0u32;
        // SAFETY: bufory żyją przez całe wywołanie, rozmiary są zgodne.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                (&request as *const Query).cast(),
                std::mem::size_of::<Query>() as u32,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        buf.truncate((returned as usize).min(buf.len()));
        Some(buf)
    }

    pub fn read_all() -> Vec<Disk> {
        let mut out = Vec::new();
        for n in 0..MAX_DRIVES {
            let path: Vec<u16> = format!(r"\\.\PhysicalDrive{n}")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // Dostęp 0: samo zapytanie o właściwości, bez czytania danych dysku.
            // SAFETY: `path` jest zakończone zerem.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    0,
                    FILE_SHARE_READ_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };
            if handle == INVALID_HANDLE || handle == 0 {
                continue;
            }
            let temp = query(handle, STORAGE_DEVICE_TEMPERATURE_PROPERTY)
                .and_then(|buf| parse_temperature(&buf));
            if let Some(temp_c) = temp {
                let name = query(handle, STORAGE_DEVICE_PROPERTY)
                    .and_then(|buf| parse_name(&buf))
                    .unwrap_or_else(|| format!("Disk {n}"));
                out.push(Disk { name, temp_c });
            }
            // SAFETY: uchwyt jest poprawny i zamykany dokładnie raz.
            unsafe { CloseHandle(handle) };
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Disk;

    pub fn read_all() -> Vec<Disk> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(temps: &[i16]) -> Vec<u8> {
        let mut buf = vec![0u8; 24 + 16 * temps.len()];
        buf[12..14].copy_from_slice(&(temps.len() as u16).to_le_bytes());
        for (i, t) in temps.iter().enumerate() {
            let at = 24 + 16 * i;
            buf[at..at + 2].copy_from_slice(&(i as u16).to_le_bytes());
            buf[at + 2..at + 4].copy_from_slice(&t.to_le_bytes());
        }
        buf
    }

    #[test]
    fn reads_temperature() {
        assert_eq!(parse_temperature(&descriptor(&[41])), Some(41.0));
        assert_eq!(parse_temperature(&descriptor(&[38, 52, 0])), Some(52.0));
        // Brak wpisów, zero, wartości bez sensu i ucięty bufor.
        assert_eq!(parse_temperature(&descriptor(&[])), None);
        assert_eq!(parse_temperature(&descriptor(&[0])), None);
        assert_eq!(parse_temperature(&descriptor(&[-40, 300])), None);
        assert_eq!(parse_temperature(&[0u8; 8]), None);
        let mut cut = descriptor(&[45, 50]);
        cut.truncate(24 + 16);
        assert_eq!(parse_temperature(&cut), Some(45.0));
    }

    #[test]
    fn reads_name() {
        let mut buf = vec![0u8; 40];
        buf[16..20].copy_from_slice(&40u32.to_le_bytes());
        buf.extend_from_slice(b"Samsung SSD 980 PRO 1TB \0");
        assert_eq!(parse_name(&buf).as_deref(), Some("Samsung SSD 980 PRO 1TB"));

        // Producent osobno od modelu.
        let mut buf = vec![0u8; 40];
        buf[12..16].copy_from_slice(&40u32.to_le_bytes());
        buf[16..20].copy_from_slice(&44u32.to_le_bytes());
        buf.extend_from_slice(b"WDC\0WD40EZRZ\0");
        assert_eq!(parse_name(&buf).as_deref(), Some("WDC WD40EZRZ"));

        assert_eq!(parse_name(&[0u8; 40]), None);
        assert_eq!(parse_name(&[0u8; 4]), None);
    }
}
