//! HAGS (Hardware-accelerated GPU scheduling): odczyt i zmiana ustawienia Windows.
//!
//! Ustawienie to wartość `HwSchMode` w kluczu
//! `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers`: 2 = włączone,
//! 1 = wyłączone. To ta sama wartość, którą zmienia przełącznik w
//! Ustawieniach Windows (Ekran > Grafika). Zmiana działa po restarcie komputera.

/// Stan ustawienia: `Some(true)` włączone, `Some(false)` wyłączone,
/// `None` - wartości nie ma w rejestrze (system lub sterownik bez HAGS
/// albo ustawienie nigdy nie było zmieniane).
pub fn read() -> Option<bool> {
    imp::read().map(|mode| mode == 2)
}

pub fn set(enabled: bool) -> Result<(), String> {
    imp::write(if enabled { 2 } else { 1 })
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    const REG_DWORD: u32 = 4;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> i32;
        fn RegSetKeyValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            kind: u32,
            data: *const c_void,
            size: u32,
        ) -> i32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
    const VALUE: &str = "HwSchMode";

    pub fn read() -> Option<u32> {
        let subkey = wide(SUBKEY);
        let value = wide(VALUE);
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: wskaźniki wskazują na żywe bufory o podanych rozmiarach.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut data as *mut u32).cast(),
                &mut size,
            )
        };
        (status == 0).then_some(data)
    }

    pub fn write(mode: u32) -> Result<(), String> {
        let subkey = wide(SUBKEY);
        let value = wide(VALUE);
        // SAFETY: jak wyżej; zapisujemy 4 bajty wartości DWORD.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                REG_DWORD,
                (&mode as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(crate::trf!(
                "zapis do rejestru nie powiódł się (kod {status}); program musi działać jako administrator",
                "writing to the registry failed (code {status}); the program must run as administrator"
            ))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn read() -> Option<u32> {
        None
    }

    pub fn write(_mode: u32) -> Result<(), String> {
        Err(crate::i18n::tr(
            "HAGS jest ustawieniem systemu Windows.",
            "HAGS is a Windows setting.",
        )
        .to_owned())
    }
}
