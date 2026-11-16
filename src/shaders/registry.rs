//! Minimalny odczyt rejestru Windows (advapi32) - do wyszukiwania gier
//! zainstalowanych przez sklepy, które zapisują się w rejestrze (GOG,
//! Ubisoft, EA, Steam).

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    type Hkey = isize;
    // Predefiniowane klucze są rozszerzane znakowo (HKEY = (LONG)0x8000000x).
    pub const HKLM: Hkey = 0x8000_0002u32 as i32 as isize;
    pub const HKCU: Hkey = 0x8000_0001u32 as i32 as isize;
    const KEY_READ: u32 = 0x2_0019;
    const KEY_WOW64_64KEY: u32 = 0x0100;
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const RRF_RT_REG_EXPAND_SZ: u32 = 0x0000_0004;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: Hkey, sub: *const u16, opts: u32, sam: u32, out: *mut Hkey) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
        fn RegEnumKeyExW(
            key: Hkey,
            index: u32,
            name: *mut u16,
            name_len: *mut u32,
            reserved: *mut u32,
            class: *mut u16,
            class_len: *mut u32,
            time: *mut c_void,
        ) -> i32;
        fn RegGetValueW(
            key: Hkey,
            sub: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            len: *mut u32,
        ) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub struct Key(Hkey);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: uchwyt otwarty w `open`.
            unsafe { RegCloseKey(self.0) };
        }
    }

    impl Key {
        pub fn open(root: Hkey, path: &str) -> Option<Key> {
            let mut out: Hkey = 0;
            let p = wide(path);
            // SAFETY: tekst zakończony zerem, wskaźnik na zmienną lokalną.
            let status =
                unsafe { RegOpenKeyExW(root, p.as_ptr(), 0, KEY_READ | KEY_WOW64_64KEY, &mut out) };
            (status == 0).then_some(Key(out))
        }

        /// Nazwy podkluczy.
        pub fn subkeys(&self) -> Vec<String> {
            let mut out = Vec::new();
            for index in 0.. {
                let mut name = [0u16; 512];
                let mut len = name.len() as u32;
                // SAFETY: bufor o długości `len`.
                let status = unsafe {
                    RegEnumKeyExW(
                        self.0,
                        index,
                        name.as_mut_ptr(),
                        &mut len,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if status != 0 {
                    break;
                }
                out.push(String::from_utf16_lossy(&name[..len as usize]));
            }
            out
        }

        pub fn string(&self, value: &str) -> Option<String> {
            let v = wide(value);
            let mut buffer = vec![0u16; 2048];
            let mut len = (buffer.len() * 2) as u32;
            // SAFETY: bufor o rozmiarze `len` bajtów.
            let status = unsafe {
                RegGetValueW(
                    self.0,
                    std::ptr::null(),
                    v.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                    std::ptr::null_mut(),
                    buffer.as_mut_ptr() as *mut c_void,
                    &mut len,
                )
            };
            if status != 0 {
                return None;
            }
            let chars = (len as usize / 2).min(buffer.len());
            let end = buffer[..chars]
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(chars);
            let text = String::from_utf16_lossy(&buffer[..end]);
            (!text.trim().is_empty()).then_some(text)
        }

        #[allow(dead_code)]
        pub fn dword(&self, value: &str) -> Option<u32> {
            let v = wide(value);
            let mut data = 0u32;
            let mut len = 4u32;
            // SAFETY: 4-bajtowy bufor.
            let status = unsafe {
                RegGetValueW(
                    self.0,
                    std::ptr::null(),
                    v.as_ptr(),
                    RRF_RT_REG_DWORD,
                    std::ptr::null_mut(),
                    &mut data as *mut u32 as *mut c_void,
                    &mut len,
                )
            };
            (status == 0).then_some(data)
        }
    }
}

#[cfg(windows)]
pub use imp::{Key, HKCU, HKLM};
