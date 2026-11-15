//! NVML (NVIDIA Management Library): oficjalne, udokumentowane API Nvidii do
//! monitorowania kart. Biblioteka `nvml.dll` jest instalowana ze sterownikiem.
//! Używamy jej tylko do odczytu: pobór mocy i wysterowanie wentylatora.
//!
//! Każde niepowodzenie (brak biblioteki, brak funkcji, błąd odczytu) oznacza
//! po prostu brak danej wartości - reszta programu działa bez zmian.

use std::ffi::{c_char, c_void, CStr};

type Device = *mut c_void;
type FnVoid = unsafe extern "C" fn() -> i32;
type FnCount = unsafe extern "C" fn(*mut u32) -> i32;
type FnHandle = unsafe extern "C" fn(u32, *mut Device) -> i32;
type FnName = unsafe extern "C" fn(Device, *mut c_char, u32) -> i32;
type FnValue = unsafe extern "C" fn(Device, *mut u32) -> i32;

/// Odczyt z jednej karty.
#[derive(Default, Clone, Copy)]
pub struct Reading {
    pub power_w: Option<f32>,
    pub fan_pct: Option<u32>,
}

pub struct Nvml {
    _lib: libloading::Library,
    /// Karty w kolejności NVML: uchwyt i nazwa.
    devices: Vec<(Device, String)>,
    shutdown: Option<FnVoid>,
    power: Option<FnValue>,
    fan: Option<FnValue>,
}

unsafe fn sym<T: Copy>(lib: &libloading::Library, name: &[u8]) -> Option<T> {
    lib.get::<T>(name).ok().map(|s| *s)
}

impl Nvml {
    pub fn load() -> Option<Self> {
        // SAFETY: nvml.dll to biblioteka sterownika Nvidii; sygnatury funkcji
        // są zgodne z dokumentacją NVML (nvml.h).
        unsafe {
            let lib = libloading::Library::new("nvml.dll").ok()?;
            let init: FnVoid = sym(&lib, b"nvmlInit_v2\0")?;
            let count: FnCount = sym(&lib, b"nvmlDeviceGetCount_v2\0")?;
            let handle: FnHandle = sym(&lib, b"nvmlDeviceGetHandleByIndex_v2\0")?;
            let name: Option<FnName> = sym(&lib, b"nvmlDeviceGetName\0");
            let shutdown: Option<FnVoid> = sym(&lib, b"nvmlShutdown\0");
            if init() != 0 {
                return None;
            }

            let mut n = 0u32;
            let mut devices = Vec::new();
            if count(&mut n) == 0 {
                for i in 0..n.min(16) {
                    let mut dev: Device = std::ptr::null_mut();
                    if handle(i, &mut dev) != 0 || dev.is_null() {
                        continue;
                    }
                    let mut buf = [0 as c_char; 96];
                    let label = match name {
                        Some(f) if f(dev, buf.as_mut_ptr(), buf.len() as u32) == 0 => {
                            // Bufor jest zakończony zerem przez NVML; na wszelki wypadek wymuszamy.
                            buf[buf.len() - 1] = 0;
                            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
                        }
                        _ => String::new(),
                    };
                    devices.push((dev, label));
                }
            }
            if devices.is_empty() {
                if let Some(f) = shutdown {
                    f();
                }
                return None;
            }

            Some(Self {
                power: sym(&lib, b"nvmlDeviceGetPowerUsage\0"),
                fan: sym(&lib, b"nvmlDeviceGetFanSpeed\0"),
                shutdown,
                devices,
                _lib: lib,
            })
        }
    }

    /// Karta NVML odpowiadająca karcie NVAPI o indeksie `index` i nazwie `name`.
    fn device(&self, index: usize, name: &str) -> Option<Device> {
        let names: Vec<&str> = self.devices.iter().map(|(_, n)| n.as_str()).collect();
        pick(&names, index, name).map(|i| self.devices[i].0)
    }

    pub fn read(&self, index: usize, name: &str) -> Reading {
        let Some(dev) = self.device(index, name) else {
            return Reading::default();
        };
        let value = |f: Option<FnValue>| {
            let f = f?;
            let mut v = 0u32;
            // SAFETY: uchwyt pochodzi z NVML, `v` jest poprawnym wskaźnikiem.
            (unsafe { f(dev, &mut v) } == 0).then_some(v)
        };
        Reading {
            // NVML podaje moc w miliwatach.
            power_w: value(self.power).map(|mw| mw as f32 / 1000.0),
            fan_pct: value(self.fan),
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        if let Some(f) = self.shutdown {
            // SAFETY: biblioteka jest wciąż załadowana.
            unsafe { f() };
        }
    }
}

/// Wybiera kartę z listy NVML: przy jednej karcie - ją; przy kilku - tę o
/// pasującej nazwie, a gdy nazwy nie rozstrzygają, tę o tym samym indeksie.
pub fn pick(names: &[&str], index: usize, name: &str) -> Option<usize> {
    if names.len() == 1 {
        return Some(0);
    }
    let wanted = name.to_lowercase();
    let matches: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            let n = n.to_lowercase();
            !n.is_empty() && (wanted.contains(&n) || n.contains(&wanted))
        })
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [only] => Some(*only),
        _ => (index < names.len()).then_some(index),
    }
}

#[cfg(test)]
mod tests {
    use super::pick;

    #[test]
    fn picks_device() {
        assert_eq!(pick(&["NVIDIA GeForce RTX 5070"], 0, "GPU 0"), Some(0));
        let two = ["NVIDIA GeForce RTX 3060", "NVIDIA GeForce RTX 5070"];
        assert_eq!(pick(&two, 0, "NVIDIA GeForce RTX 5070"), Some(1));
        // Dwie takie same karty: decyduje indeks.
        let same = ["NVIDIA GeForce RTX 5070", "NVIDIA GeForce RTX 5070"];
        assert_eq!(pick(&same, 1, "NVIDIA GeForce RTX 5070"), Some(1));
        assert_eq!(pick(&two, 5, "inna"), None);
        assert_eq!(pick(&[], 0, "x"), None);
    }
}
