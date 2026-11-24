//! Port szeregowy (COM) przez Windows API: wyszukanie portu urządzenia USB
//! po VID/PID i prosta transmisja 8N1 bez kontroli przepływu.

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::Communication::{
    GetCommState, PurgeComm, SetCommState, SetCommTimeouts, COMMTIMEOUTS, DCB, NOPARITY,
    ONESTOPBIT, PURGE_RXCLEAR,
};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey, DICS_FLAG_GLOBAL, DIGCF_PRESENT,
    DIREG_DEV, SPDRP_HARDWAREID, SP_DEVINFO_DATA,
};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE,
    OPEN_EXISTING,
};
use windows::Win32::System::Registry::{RegCloseKey, RegQueryValueExW, KEY_READ};

/// Klasa urządzeń "Porty (COM i LPT)".
const GUID_DEVCLASS_PORTS: GUID = GUID::from_u128(0x4d36e978_e325_11ce_bfc1_08002be10318);

/// Błąd otwarcia portu.
pub enum OpenError {
    /// Port zajęty przez inny program.
    Busy,
    Other(String),
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn os_error(what: &str) -> String {
    // SAFETY: odczyt kodu błędu wątku.
    let e = unsafe { GetLastError() };
    format!("{what}: {}", windows::core::Error::from(e.to_hresult()).message())
}

/// Nazwa portu (np. "COM5") pierwszego urządzenia USB o danym VID i PID.
pub fn find_port(vid: u16, pid: u16) -> Option<String> {
    let wanted = format!("VID_{vid:04X}&PID_{pid:04X}");
    // SAFETY: wywołania SetupAPI i rejestru z buforami o podanych rozmiarach;
    // lista i klucze zwalniane.
    unsafe {
        let set = SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_PORTS), PCWSTR::null(), None, DIGCF_PRESENT).ok()?;
        let mut found = None;
        let mut index = 0;
        loop {
            let mut info = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInfo(set, index, &mut info).is_err() {
                break;
            }
            index += 1;
            let mut ids = [0u8; 2048];
            if SetupDiGetDeviceRegistryPropertyW(set, &info, SPDRP_HARDWAREID, None, Some(&mut ids), None).is_err() {
                continue;
            }
            let ids: Vec<u16> = ids.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            if !String::from_utf16_lossy(&ids).to_ascii_uppercase().contains(&wanted) {
                continue;
            }
            let Ok(key) = SetupDiOpenDevRegKey(set, &info, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, KEY_READ.0) else {
                continue;
            };
            let name = wide("PortName");
            let mut buf = [0u16; 64];
            let mut len = (buf.len() * 2) as u32;
            let r = RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut len));
            let _ = RegCloseKey(key);
            if r.is_ok() {
                let chars = (len as usize / 2).min(buf.len());
                let port = String::from_utf16_lossy(&buf[..chars]).trim_end_matches('\0').to_owned();
                if !port.is_empty() {
                    found = Some(port);
                    break;
                }
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        found
    }
}

/// Otwarty port szeregowy.
pub struct Port {
    handle: HANDLE,
}

// Uchwyt pliku można używać z innego wątku.
unsafe impl Send for Port {}

impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: uchwyt otwarty przez nas.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Port {
    /// Otwiera port na wyłączność: `baud` bodów, 8 bitów danych, bez
    /// parzystości, 1 bit stopu, bez kontroli przepływu i bez DTR/RTS.
    /// `timeout_ms` - czas oczekiwania na odczyt i zapis.
    pub fn open(name: &str, baud: u32, timeout_ms: u32) -> Result<Port, OpenError> {
        let path = wide(&format!(r"\\.\{name}"));
        // SAFETY: ścieżka zakończona zerem; struktury zerowane i wypełniane
        // przez system.
        unsafe {
            let handle = CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .map_err(|e| {
                if GetLastError() == ERROR_ACCESS_DENIED || e.code() == ERROR_ACCESS_DENIED.to_hresult() {
                    OpenError::Busy
                } else {
                    OpenError::Other(format!("{name}: {}", e.message()))
                }
            })?;
            let port = Port { handle };
            let mut dcb = DCB {
                DCBlength: std::mem::size_of::<DCB>() as u32,
                ..Default::default()
            };
            GetCommState(handle, &mut dcb).map_err(|e| OpenError::Other(format!("{name}: {e}")))?;
            dcb.BaudRate = baud;
            dcb.ByteSize = 8;
            dcb.Parity = NOPARITY;
            dcb.StopBits = ONESTOPBIT;
            dcb.XonChar = 17;
            dcb.XoffChar = 19;
            dcb.ErrorChar = 0;
            dcb.EofChar = 26;
            // Pole bitowe DCB: tylko fBinary (bit 0); pozostałe (parzystość,
            // CTS/DSR, DTR i RTS wyłączone, XON/XOFF, fAbortOnError) zerowe.
            dcb._bitfield = 1;
            SetCommState(handle, &dcb).map_err(|e| OpenError::Other(format!("{name}: {e}")))?;
            // Odczyt: od razu, gdy są dane; inaczej do `timeout_ms`.
            let t = COMMTIMEOUTS {
                ReadIntervalTimeout: u32::MAX,
                ReadTotalTimeoutMultiplier: u32::MAX,
                ReadTotalTimeoutConstant: timeout_ms,
                WriteTotalTimeoutMultiplier: 0,
                WriteTotalTimeoutConstant: timeout_ms,
            };
            SetCommTimeouts(handle, &t).map_err(|e| OpenError::Other(format!("{name}: {e}")))?;
            Ok(port)
        }
    }

    /// Odrzuca dane czekające na odczyt.
    pub fn clear_input(&self) {
        // SAFETY: uchwyt portu.
        unsafe {
            let _ = PurgeComm(self.handle, PURGE_RXCLEAR);
        }
    }

    pub fn write_all(&self, data: &[u8]) -> Result<(), String> {
        let mut done = 0;
        while done < data.len() {
            let mut n = 0u32;
            // SAFETY: bufor żyje do końca wywołania (zapis synchroniczny).
            unsafe { WriteFile(self.handle, Some(&data[done..]), Some(&mut n), None) }
                .map_err(|_| os_error("WriteFile"))?;
            if n == 0 {
                return Err("WriteFile: przekroczony czas".into());
            }
            done += n as usize;
        }
        Ok(())
    }

    /// Odczyt; 0 - w czasie oczekiwania nic nie przyszło.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, String> {
        let mut n = 0u32;
        // SAFETY: bufor żyje do końca wywołania (odczyt synchroniczny).
        unsafe { ReadFile(self.handle, Some(buf), Some(&mut n), None) }.map_err(|_| os_error("ReadFile"))?;
        Ok(n as usize)
    }

    pub fn flush(&self) -> Result<(), String> {
        // SAFETY: uchwyt portu.
        unsafe { FlushFileBuffers(self.handle) }.map_err(|_| os_error("FlushFileBuffers"))
    }
}

#[cfg(test)]
mod tests {
    /// Port taśmy Skydimo, jeśli jest podłączona:
    /// `cargo test serial_find -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn serial_find() {
        println!("{:?}", super::find_port(0x1A86, 0x7523));
    }
}
