//! Dostęp do urządzeń USB HID przez Windows HID API (hid.dll i SetupAPI):
//! wyszukiwanie interfejsów, raporty wyjściowe i wejściowe, raporty
//! funkcyjne (feature).
//!
//! Zasady jak w Windows HID: bufor raportu zaczyna się od bajtu numeru
//! raportu (0, gdy urządzenie nie numeruje raportów) i ma pełną długość z
//! opisu urządzenia (`HidP_GetCaps`), więc krótsze dane dopełniamy zerami.
//! Odczyt raportu wejściowego bez numeracji zwracamy bez tego zera.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid, HidD_GetPreparsedData,
    HidD_GetProductString, HidD_SetFeature, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS,
    PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE,
    WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
use windows::Win32::System::IO::{CancelIo, DeviceIoControl, GetOverlappedResult, OVERLAPPED};

/// IOCTL_HID_GET_FEATURE = CTL_CODE(FILE_DEVICE_KEYBOARD, 100, METHOD_OUT_DIRECT, FILE_ANY_ACCESS).
const IOCTL_HID_GET_FEATURE: u32 = (0x0B << 16) | (100 << 2) | 2;
/// Czas na zapis raportu (zapis jest synchroniczny dla wywołującego).
const WRITE_TIMEOUT_MS: u32 = 1000;
/// HIDP_STATUS_SUCCESS
const HIDP_SUCCESS: i32 = 0x0011_0000;

pub struct Hid;

/// Interfejs HID urządzenia (jedno urządzenie USB może mieć ich kilka).
#[derive(Clone, Debug)]
pub struct Iface {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub interface: i32,
    /// Ścieżka urządzenia (\\?\hid#vid_...).
    pub path: String,
    pub product: String,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last_error(what: &str) -> String {
    // SAFETY: odczyt kodu błędu wątku.
    let e = unsafe { GetLastError() };
    format!("{what}: {}", windows::core::Error::from(e.to_hresult()).message())
}

/// Uchwyt pliku zamykany przy końcu życia.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: uchwyt otwarty przez nas.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn open_path(path: &str, rw: bool) -> Option<Owned> {
    let p = wide(path);
    let access = if rw { GENERIC_READ.0 | GENERIC_WRITE.0 } else { 0 };
    // SAFETY: ścieżka zakończona zerem.
    unsafe {
        CreateFileW(
            PCWSTR(p.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .ok()
    .map(Owned)
}

/// Opis interfejsu: (usage page, usage, długości raportów wejścia, wyjścia, funkcyjnego).
fn caps(h: HANDLE) -> Option<(u16, u16, usize, usize, usize)> {
    // SAFETY: uchwyt urządzenia HID; dane opisu zwalniane po odczycie.
    unsafe {
        let mut pre = PHIDP_PREPARSED_DATA::default();
        if !HidD_GetPreparsedData(h, &mut pre) {
            return None;
        }
        let mut c = HIDP_CAPS::default();
        let r = HidP_GetCaps(pre, &mut c);
        let _ = HidD_FreePreparsedData(pre);
        (r.0 == HIDP_SUCCESS).then_some((
            c.UsagePage,
            c.Usage,
            c.InputReportByteLength as usize,
            c.OutputReportByteLength as usize,
            c.FeatureReportByteLength as usize,
        ))
    }
}

/// Numer interfejsu USB z ścieżki ("...&mi_02#..." -> 2), -1 gdy brak.
fn interface_number(path: &str) -> i32 {
    let lower = path.to_ascii_lowercase();
    lower
        .find("&mi_")
        .and_then(|i| lower.get(i + 4..i + 6))
        .and_then(|h| i32::from_str_radix(h, 16).ok())
        .unwrap_or(-1)
}

/// Ścieżki wszystkich obecnych interfejsów HID.
fn paths() -> Vec<String> {
    let mut out = Vec::new();
    // SAFETY: wywołania SetupAPI z buforami o podanych rozmiarach; lista
    // zwalniana na końcu.
    unsafe {
        let guid: GUID = HidD_GetHidGuid();
        let Ok(set): Result<HDEVINFO, _> =
            SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
        else {
            return out;
        };
        let mut index = 0;
        loop {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set, None, &guid, index, &mut data).is_err() {
                break;
            }
            index += 1;
            let mut needed = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut needed), None);
            if needed == 0 {
                continue;
            }
            // Bufor wyrównany do 8 bajtów (struktura zaczyna się od cbSize).
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), needed, None, None).is_err() {
                continue;
            }
            let start = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let max = (needed as usize - 4) / 2;
            let len = (0..max).take_while(|&i| *start.add(i) != 0).count();
            out.push(String::from_utf16_lossy(std::slice::from_raw_parts(start, len)));
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Opis jednego interfejsu (otwartego bez praw zapisu - działa także dla
/// klawiatur i myszy, które Windows trzyma na wyłączność).
fn describe(path: &str) -> Option<Iface> {
    let h = open_path(path, false)?;
    // SAFETY: uchwyt urządzenia HID, bufory o podanych rozmiarach.
    unsafe {
        let mut a = HIDD_ATTRIBUTES {
            Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
            ..Default::default()
        };
        if !HidD_GetAttributes(h.0, &mut a) {
            return None;
        }
        let (usage_page, usage, ..) = caps(h.0).unwrap_or_default();
        let mut name = [0u16; 128];
        let product = if HidD_GetProductString(h.0, name.as_mut_ptr() as *mut c_void, (name.len() * 2) as u32) {
            let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            String::from_utf16_lossy(&name[..len]).trim().to_owned()
        } else {
            String::new()
        };
        Some(Iface {
            vid: a.VendorID,
            pid: a.ProductID,
            usage_page,
            usage,
            interface: interface_number(path),
            path: path.to_owned(),
            product,
        })
    }
}

impl Hid {
    pub fn new() -> Result<Self, String> {
        Ok(Self)
    }

    /// Wszystkie interfejsy HID (diagnostyka).
    pub fn list_all(&self) -> Vec<Iface> {
        let mut all: Vec<Iface> = paths().iter().filter_map(|p| describe(p)).collect();
        all.sort_by_key(|i| (i.vid, i.pid, i.interface));
        all
    }

    /// Interfejsy urządzeń danego producenta.
    pub fn list(&self, vid: u16) -> Vec<Iface> {
        self.list_all().into_iter().filter(|i| i.vid == vid).collect()
    }

    pub fn open(&self, iface: &Iface) -> Result<HidDevice, String> {
        let name = format!("{:04X}:{:04X}", iface.vid, iface.pid);
        let h = open_path(&iface.path, true).ok_or_else(|| last_error(&name))?;
        let (_, _, input, output, feature) =
            caps(h.0).ok_or_else(|| format!("{name}: HidP_GetCaps"))?;
        // SAFETY: zdarzenia ręcznie zerowane, zamykane w Drop urządzenia.
        let (read_event, write_event) = unsafe {
            (
                CreateEventW(None, true, false, None).map_err(|e| format!("{name}: {e}"))?,
                CreateEventW(None, true, false, None).map_err(|e| format!("{name}: {e}"))?,
            )
        };
        Ok(HidDevice {
            handle: h,
            input_len: input.max(1),
            output_len: output,
            feature_len: feature,
            read: Mutex::new(ReadState {
                ol: Box::new(OVERLAPPED {
                    hEvent: read_event,
                    ..Default::default()
                }),
                buf: vec![0; input.max(1)],
                pending: false,
            }),
            write: Mutex::new(Box::new(OVERLAPPED {
                hEvent: write_event,
                ..Default::default()
            })),
        })
    }
}

/// Odczyt w toku: struktura OVERLAPPED i bufor muszą żyć (pod stałym
/// adresem) aż system go zakończy - stąd Box i odczyt pozostawiany na
/// kolejne wywołanie, gdy dane jeszcze nie przyszły.
struct ReadState {
    ol: Box<OVERLAPPED>,
    buf: Vec<u8>,
    pending: bool,
}

/// Otwarty interfejs HID.
pub struct HidDevice {
    handle: Owned,
    input_len: usize,
    output_len: usize,
    feature_len: usize,
    read: Mutex<ReadState>,
    write: Mutex<Box<OVERLAPPED>>,
}

// Uchwyty Windows można używać z dowolnego wątku; stan odczytu i zapisu
// chronią Mutexy.
unsafe impl Send for HidDevice {}
unsafe impl Sync for HidDevice {}

impl Drop for HidDevice {
    fn drop(&mut self) {
        // SAFETY: anulujemy operacje przed zamknięciem uchwytów i zwolnieniem
        // buforów, na które wskazują.
        unsafe {
            let _ = CancelIo(self.handle.0);
            let state = self.read.get_mut().unwrap_or_else(|e| e.into_inner());
            if state.pending {
                let mut n = 0u32;
                let _ = GetOverlappedResult(self.handle.0, &*state.ol, &mut n, true);
            }
            let _ = CloseHandle(state.ol.hEvent);
            let w = self.write.get_mut().unwrap_or_else(|e| e.into_inner());
            let _ = CloseHandle(w.hEvent);
        }
    }
}

/// Dane dopełnione zerami do pełnej długości raportu.
fn padded(data: &[u8], len: usize) -> Vec<u8> {
    let mut b = data.to_vec();
    if b.len() < len {
        b.resize(len, 0);
    }
    b
}

impl HidDevice {
    /// Raport wyjściowy (pierwszy bajt: numer raportu). Zwraca liczbę
    /// zapisanych bajtów.
    pub fn write(&self, data: &[u8]) -> Result<usize, String> {
        if data.is_empty() {
            return Err("HID: pusty raport".into());
        }
        let buf = padded(data, self.output_len);
        let ol = self.write.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: bufor i OVERLAPPED żyją do końca operacji (czekamy na nią
        // albo ją anulujemy przed wyjściem).
        unsafe {
            let mut n = 0u32;
            let ol_ptr = &**ol as *const OVERLAPPED as *mut OVERLAPPED;
            if WriteFile(self.handle.0, Some(&buf), Some(&mut n), Some(ol_ptr)).is_err() {
                if GetLastError() != ERROR_IO_PENDING {
                    return Err(last_error("WriteFile"));
                }
                if WaitForSingleObject(ol.hEvent, WRITE_TIMEOUT_MS) != WAIT_OBJECT_0 {
                    let _ = CancelIo(self.handle.0);
                    let _ = GetOverlappedResult(self.handle.0, &**ol, &mut n, true);
                    return Err("WriteFile: przekroczony czas".into());
                }
                GetOverlappedResult(self.handle.0, &**ol, &mut n, false)
                    .map_err(|e| format!("WriteFile: {e}"))?;
            }
            Ok(n as usize)
        }
    }

    /// Raport wejściowy z czasem oczekiwania w ms (0 - bez czekania, -1 - do
    /// skutku). Zwraca 0, gdy w tym czasie nic nie przyszło; odczyt czeka
    /// dalej i jego wynik dostanie następne wywołanie.
    pub fn read_timeout(&self, out: &mut [u8], ms: i32) -> Result<usize, String> {
        let mut s = self.read.lock().unwrap_or_else(|e| e.into_inner());
        let s = &mut *s;
        // SAFETY: bufor i OVERLAPPED w stanie urządzenia żyją do zakończenia
        // odczytu (Drop czeka na niego).
        unsafe {
            let mut n = 0u32;
            if !s.pending {
                s.buf.iter_mut().for_each(|b| *b = 0);
                let _ = windows::Win32::System::Threading::ResetEvent(s.ol.hEvent);
                let ol_ptr = &mut *s.ol as *mut OVERLAPPED;
                match ReadFile(self.handle.0, Some(&mut s.buf[..self.input_len]), Some(&mut n), Some(ol_ptr)) {
                    Ok(()) => {}
                    Err(_) if GetLastError() == ERROR_IO_PENDING => s.pending = true,
                    Err(_) => return Err(last_error("ReadFile")),
                }
            }
            if s.pending {
                let wait = if ms < 0 { INFINITE } else { ms as u32 };
                if WaitForSingleObject(s.ol.hEvent, wait) != WAIT_OBJECT_0 {
                    return Ok(0);
                }
                s.pending = false;
                GetOverlappedResult(self.handle.0, &*s.ol, &mut n, false)
                    .map_err(|e| format!("ReadFile: {e}"))?;
            }
            let n = n as usize;
            if n == 0 {
                return Ok(0);
            }
            // Bez numeracji raportów Windows dokleja zero na początku.
            let data = if s.buf[0] == 0 { &s.buf[1..n] } else { &s.buf[..n] };
            let len = data.len().min(out.len());
            out[..len].copy_from_slice(&data[..len]);
            Ok(len)
        }
    }

    /// Raport funkcyjny do urządzenia (pierwszy bajt: numer raportu).
    pub fn send_feature_report(&self, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Err("HID: pusty raport".into());
        }
        let buf = padded(data, self.feature_len);
        // SAFETY: bufor o podanej długości.
        let ok = unsafe { HidD_SetFeature(self.handle.0, buf.as_ptr() as *const c_void, buf.len() as u32) };
        if ok {
            Ok(())
        } else {
            Err(last_error("HidD_SetFeature"))
        }
    }

    /// Raport funkcyjny z urządzenia; `buf[0]` - numer raportu. Zwraca liczbę
    /// bajtów razem z bajtem numeru raportu.
    pub fn get_feature_report(&self, buf: &mut [u8]) -> Result<usize, String> {
        if buf.is_empty() {
            return Err("HID: pusty bufor".into());
        }
        // SAFETY: bufor wejścia i wyjścia ten sam, o podanej długości; czekamy
        // na koniec operacji przed wyjściem.
        unsafe {
            // SAFETY: zdarzenie zamykane na końcu.
            let event = CreateEventW(None, true, false, None).map_err(|e| e.to_string())?;
            let ev = Owned(event);
            let ol = OVERLAPPED {
                hEvent: ev.0,
                ..Default::default()
            };
            let mut n = 0u32;
            let len = buf.len() as u32;
            let p = buf.as_mut_ptr() as *mut c_void;
            if DeviceIoControl(self.handle.0, IOCTL_HID_GET_FEATURE, Some(p), len, Some(p), len, Some(&mut n), Some(&ol as *const _ as *mut _)).is_err()
                && GetLastError() != ERROR_IO_PENDING
            {
                return Err(last_error("IOCTL_HID_GET_FEATURE"));
            }
            GetOverlappedResult(self.handle.0, &ol, &mut n, true)
                .map_err(|e| format!("IOCTL_HID_GET_FEATURE: {e}"))?;
            // Liczba bajtów nie obejmuje zerowego numeru raportu.
            if buf[0] == 0 {
                n += 1;
            }
            Ok(n as usize)
        }
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
pub fn err(name: &str, e: impl std::fmt::Display) -> String {
    format!("{name}: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_from_path() {
        assert_eq!(interface_number(r"\\?\hid#vid_046d&pid_c547&mi_02&col01#9&2"), 2);
        assert_eq!(interface_number(r"\\?\hid#vid_0b05&pid_19af&mi_0a#x"), 10);
        assert_eq!(interface_number(r"\\?\hid#vid_1234&pid_5678#x"), -1);
    }

    #[test]
    fn pads_reports() {
        assert_eq!(padded(&[1, 2], 4), vec![1, 2, 0, 0]);
        assert_eq!(padded(&[1, 2, 3], 2), vec![1, 2, 3]);
    }

    /// Zapis i odczyt na odbiorniku Logitech (HID++ 1.0: odczyt rejestru
    /// 0x02, bez zmian w urządzeniu):
    /// `cargo test hid_logitech_echo -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn hid_logitech_echo() {
        let hid = Hid::new().unwrap();
        let iface = hid
            .list(0x046D)
            .into_iter()
            .find(|i| i.usage_page == 0xFF00 && i.usage == 0x0001)
            .expect("odbiornik Logitech");
        let dev = hid.open(&iface).expect("otwarcie");
        // Nic nie czeka: odczyt bez czekania zwraca 0.
        let mut junk = [0u8; 20];
        while dev.read_timeout(&mut junk, 0).unwrap() > 0 {}
        let n = dev.write(&[0x10, 0xFF, 0x81, 0x02, 0, 0, 0]).expect("zapis");
        println!("zapisano {n} B");
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(1) {
            let mut r = [0u8; 20];
            let n = dev.read_timeout(&mut r, 200).expect("odczyt");
            if n > 0 {
                println!("odpowiedź {n} B: {:02X?}", &r[..n]);
                if r[0] == 0x10 && r[2] == 0x81 {
                    return;
                }
            }
        }
        panic!("brak odpowiedzi odbiornika");
    }

    /// Lista urządzeń HID tego komputera: `cargo test hid_list -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn hid_list() {
        for i in Hid::new().unwrap().list_all() {
            println!(
                "{:04X}:{:04X} if {} page {:04X} usage {:04X} {:?}",
                i.vid, i.pid, i.interface, i.usage_page, i.usage, i.product
            );
        }
    }
}
