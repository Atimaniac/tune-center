//! Ładowanie bibliotek DLL sterowników (NVAPI, NVML, ADL, ADLX, IGCL, PawnIO,
//! Oodle) i odczyt ich funkcji przez Windows API (`LoadLibraryExW`,
//! `GetProcAddress`, `FreeLibrary`).

use std::ffi::OsStr;
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::Diagnostics::Debug::{SetThreadErrorMode, SEM_FAILCRITICALERRORS, THREAD_ERROR_MODE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_FLAGS};

/// Błąd ładowania biblioteki albo funkcji.
#[derive(Debug, Clone)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Załadowana biblioteka; zwalniana (`FreeLibrary`) przy końcu życia.
pub struct Library(HMODULE);

// Uchwyt modułu jest ważny w całym procesie.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: moduł załadowany przez `new`, zwalniany raz.
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

/// Funkcja z biblioteki; żyje nie dłużej niż biblioteka.
pub struct Symbol<'a, T> {
    value: T,
    _lib: PhantomData<&'a Library>,
}

impl<T> std::ops::Deref for Symbol<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl Library {
    /// Ładuje bibliotekę (nazwę szuka Windows w zwykłej kolejności, ścieżkę
    /// bierze wprost). Bez okienek błędu systemu, gdy pliku brak.
    ///
    /// # Safety
    /// Ładowanie wykonuje kod inicjujący biblioteki (DllMain).
    pub unsafe fn new(name: impl AsRef<OsStr>) -> Result<Library, Error> {
        let wide: Vec<u16> = name.as_ref().encode_wide().chain(Some(0)).collect();
        let mut old = THREAD_ERROR_MODE(0);
        let _ = SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old));
        let result = LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_FLAGS(0));
        let _ = SetThreadErrorMode(old, None);
        result
            .map(Library)
            .map_err(|e| Error(format!("LoadLibraryExW: {}", e.message())))
    }

    /// Funkcja o nazwie `name` (zakończonej bajtem 0) jako typ `T`
    /// (wskaźnik na funkcję).
    ///
    /// # Safety
    /// `T` musi być wskaźnikiem na funkcję o sygnaturze zgodnej z biblioteką.
    pub unsafe fn get<T: Copy>(&self, name: &[u8]) -> Result<Symbol<'_, T>, Error> {
        debug_assert_eq!(name.last(), Some(&0), "nazwa funkcji bez bajtu 0");
        debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<usize>());
        let label = || String::from_utf8_lossy(name.strip_suffix(&[0]).unwrap_or(name)).into_owned();
        let p = GetProcAddress(self.0, PCSTR(name.as_ptr()))
            .ok_or_else(|| Error(format!("GetProcAddress {}: brak funkcji", label())))?;
        Ok(Symbol {
            value: std::mem::transmute_copy(&p),
            _lib: PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_system_function() {
        type GetTickCount = unsafe extern "system" fn() -> u32;
        // SAFETY: kernel32 i funkcja bez argumentów.
        unsafe {
            let lib = Library::new("kernel32.dll").unwrap();
            let f = lib.get::<GetTickCount>(b"GetTickCount\0").unwrap();
            assert!(f() > 0);
            assert!(lib.get::<GetTickCount>(b"NoSuchFunctionHere\0").is_err());
            assert!(Library::new("no-such-library-tc.dll").is_err());
        }
    }
}
