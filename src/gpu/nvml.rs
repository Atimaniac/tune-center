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
type FnValue64 = unsafe extern "C" fn(Device, *mut u64) -> i32;
type FnArg = unsafe extern "C" fn(Device, u32, *mut u32) -> i32;
type FnPair = unsafe extern "C" fn(Device, *mut u32, *mut u32) -> i32;
type FnMemory = unsafe extern "C" fn(Device, *mut Memory) -> i32;
type FnFields = unsafe extern "C" fn(Device, i32, *mut FieldValue) -> i32;

/// `nvmlMemory_t` (bajty).
#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

/// `nvmlFieldValue_t`.
#[repr(C)]
#[derive(Default)]
struct FieldValue {
    field_id: u32,
    scope_id: u32,
    timestamp: i64,
    latency_us: i64,
    value_type: u32,
    status: u32,
    value: u64,
}

const _: () = assert!(std::mem::size_of::<FieldValue>() == 40);

/// `NVML_FI_DEV_MEMORY_TEMP` - temperatura pamięci (nie każda karta).
const FIELD_MEMORY_TEMP: u32 = 82;

/// Odczyt z jednej karty.
#[derive(Default, Clone, Copy)]
pub struct Reading {
    pub power_w: Option<f32>,
    pub fan_pct: Option<u32>,
    /// Domyślny limit mocy (TDP) w watach.
    pub tdp_w: Option<f32>,
    /// Pamięć karty: zajęta i całkowita (MB).
    pub mem_used_mb: Option<u32>,
    pub mem_total_mb: Option<u32>,
    pub mem_temp_c: Option<f32>,
    /// Przepływ PCIe w MB/s (wysyłanie przez kartę / odbieranie).
    pub pcie_tx_mbs: Option<f32>,
    pub pcie_rx_mbs: Option<f32>,
    pub pcie_gen: Option<u32>,
    pub pcie_width: Option<u32>,
    /// Obciążenie kontrolera pamięci (%).
    pub mem_ctrl_pct: Option<u32>,
    pub encoder_pct: Option<u32>,
    pub decoder_pct: Option<u32>,
    pub video_mhz: Option<u32>,
    /// Stan wydajności P0..P15.
    pub pstate: Option<u32>,
    /// Przyczyny ograniczania zegarów (`nvmlClocksEventReasons`).
    pub limit_reasons: Option<u64>,
}

pub struct Nvml {
    _lib: libloading::Library,
    /// Karty w kolejności NVML: uchwyt i nazwa.
    devices: Vec<(Device, String)>,
    shutdown: Option<FnVoid>,
    power: Option<FnValue>,
    fan: Option<FnValue>,
    default_limit: Option<FnValue>,
    memory: Option<FnMemory>,
    fields: Option<FnFields>,
    pcie: Option<FnArg>,
    pcie_gen: Option<FnValue>,
    pcie_width: Option<FnValue>,
    utilization: Option<FnPair>,
    encoder: Option<FnPair>,
    decoder: Option<FnPair>,
    clock: Option<FnArg>,
    pstate: Option<FnValue>,
    reasons: Option<FnValue64>,
}

/// Przyczyny ograniczania zegarów (jak "PerfCap Reason" w GPU-Z).
pub fn limit_text(reasons: u64) -> String {
    const NAMES: [(u64, &str); 8] = [
        (0x1, "Idle"),
        (0x2, "AppClk"),
        (0x4, "Pwr"),
        (0x8, "HwSlow"),
        (0x20, "Thrm"),
        (0x40, "HwThrm"),
        (0x80, "PwrBrake"),
        (0x100, "DispClk"),
    ];
    let names: Vec<&str> = NAMES
        .iter()
        .filter(|(bit, _)| reasons & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    if names.is_empty() {
        crate::i18n::tr("brak", "none").to_string()
    } else {
        names.join(", ")
    }
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
                default_limit: sym(&lib, b"nvmlDeviceGetPowerManagementDefaultLimit\0"),
                memory: sym(&lib, b"nvmlDeviceGetMemoryInfo\0"),
                fields: sym(&lib, b"nvmlDeviceGetFieldValues\0"),
                pcie: sym(&lib, b"nvmlDeviceGetPcieThroughput\0"),
                pcie_gen: sym(&lib, b"nvmlDeviceGetCurrPcieLinkGeneration\0"),
                pcie_width: sym(&lib, b"nvmlDeviceGetCurrPcieLinkWidth\0"),
                utilization: sym(&lib, b"nvmlDeviceGetUtilizationRates\0"),
                encoder: sym(&lib, b"nvmlDeviceGetEncoderUtilization\0"),
                decoder: sym(&lib, b"nvmlDeviceGetDecoderUtilization\0"),
                clock: sym(&lib, b"nvmlDeviceGetClockInfo\0"),
                pstate: sym(&lib, b"nvmlDeviceGetPerformanceState\0"),
                reasons: sym::<FnValue64>(&lib, b"nvmlDeviceGetCurrentClocksEventReasons\0")
                    .or_else(|| sym(&lib, b"nvmlDeviceGetCurrentClocksThrottleReasons\0")),
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
        // SAFETY (dla wszystkich wywołań poniżej): uchwyt z NVML, wskaźniki na
        // lokalne zmienne o typach z nvml.h.
        let pair = |f: Option<FnPair>| {
            let f = f?;
            let (mut a, mut b) = (0u32, 0u32);
            (unsafe { f(dev, &mut a, &mut b) } == 0).then_some((a, b))
        };
        let arg = |f: Option<FnArg>, x: u32| {
            let f = f?;
            let mut v = 0u32;
            (unsafe { f(dev, x, &mut v) } == 0).then_some(v)
        };
        let memory = self.memory.and_then(|f| {
            let mut m = Memory::default();
            (unsafe { f(dev, &mut m) } == 0).then_some(m)
        });
        let mem_temp_c = self.fields.and_then(|f| {
            let mut v = FieldValue {
                field_id: FIELD_MEMORY_TEMP,
                ..Default::default()
            };
            // Typ wartości: 1 = u32, 2 = u64, 3 = u64 (unsigned long), 5 = i64.
            (unsafe { f(dev, 1, &mut v) } == 0 && v.status == 0 && v.value as u32 > 0)
                .then_some(v.value as u32 as f32)
        });
        let reasons = self.reasons.and_then(|f| {
            let mut v = 0u64;
            (unsafe { f(dev, &mut v) } == 0).then_some(v)
        });
        let utilization = pair(self.utilization);
        Reading {
            // NVML podaje moc w miliwatach.
            power_w: value(self.power).map(|mw| mw as f32 / 1000.0),
            fan_pct: value(self.fan),
            tdp_w: value(self.default_limit).map(|mw| mw as f32 / 1000.0),
            mem_used_mb: memory.as_ref().map(|m| (m.used >> 20) as u32),
            mem_total_mb: memory.as_ref().map(|m| (m.total >> 20) as u32),
            mem_temp_c,
            // KB/s; licznik 0 = TX, 1 = RX.
            pcie_tx_mbs: arg(self.pcie, 0).map(|kb| kb as f32 / 1000.0),
            pcie_rx_mbs: arg(self.pcie, 1).map(|kb| kb as f32 / 1000.0),
            pcie_gen: value(self.pcie_gen),
            pcie_width: value(self.pcie_width),
            mem_ctrl_pct: utilization.map(|(_, mem)| mem),
            encoder_pct: pair(self.encoder).map(|(u, _)| u),
            decoder_pct: pair(self.decoder).map(|(u, _)| u),
            // NVML_CLOCK_VIDEO = 3.
            video_mhz: arg(self.clock, 3),
            pstate: value(self.pstate).filter(|&p| p < 32),
            limit_reasons: reasons,
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
