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
type FnFanRpm = unsafe extern "C" fn(Device, *mut FanSpeedInfo) -> i32;
type FnSet = unsafe extern "C" fn(Device, u32, u32) -> i32;
type FnFanDefault = unsafe extern "C" fn(Device, u32) -> i32;

/// `nvmlFanSpeedInfo_t` (wersja 1): obroty wentylatora `fan`.
#[repr(C)]
#[derive(Default)]
struct FanSpeedInfo {
    version: u32,
    fan: u32,
    speed: u32,
}

/// `NVML_STRUCT_VERSION(FanSpeedInfo, 1)`: rozmiar i numer wersji w bicie 24.
const FAN_SPEED_INFO_V1: u32 = std::mem::size_of::<FanSpeedInfo>() as u32 | 1 << 24;

/// `NVML_FI_DEV_POWER_INSTANT` z zakresem `NVML_POWER_SCOPE_GPU` - chwilowa
/// moc samego układu GPU (mW).
const FIELD_POWER_INSTANT: u32 = 186;
const POWER_SCOPE_GPU: u32 = 0;

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
    _lib: crate::dll::Library,
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
    num_fans: Option<FnValue>,
    fan_v2: Option<FnArg>,
    fan_rpm: Option<FnFanRpm>,
    set_fan: Option<FnSet>,
    default_fan: Option<FnFanDefault>,
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

unsafe fn sym<T: Copy>(lib: &crate::dll::Library, name: &[u8]) -> Option<T> {
    lib.get::<T>(name).ok().map(|s| *s)
}

impl Nvml {
    pub fn load() -> Option<Self> {
        // SAFETY: nvml.dll to biblioteka sterownika Nvidii; sygnatury funkcji
        // są zgodne z dokumentacją NVML (nvml.h).
        unsafe {
            let lib = crate::dll::Library::new("nvml.dll").ok()?;
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
                num_fans: sym(&lib, b"nvmlDeviceGetNumFans\0"),
                fan_v2: sym(&lib, b"nvmlDeviceGetFanSpeed_v2\0"),
                fan_rpm: sym(&lib, b"nvmlDeviceGetFanSpeedRPM\0"),
                set_fan: sym(&lib, b"nvmlDeviceSetFanSpeed_v2\0"),
                default_fan: sym(&lib, b"nvmlDeviceSetDefaultFanSpeed_v2\0"),
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

    /// `detail`: także przepływ PCIe TX/RX (NVML mierzy go przez ok. 20 ms na
    /// wywołanie) - tylko gdy widok Informacje jest otwarty.
    pub fn read(&self, index: usize, name: &str, detail: bool) -> Reading {
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
            pcie_tx_mbs: if detail {
                arg(self.pcie, 0).map(|kb| kb as f32 / 1000.0)
            } else {
                None
            },
            pcie_rx_mbs: if detail {
                arg(self.pcie, 1).map(|kb| kb as f32 / 1000.0)
            } else {
                None
            },
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

impl Nvml {
    /// Liczba wentylatorów karty (0 = brak albo karta bez sterowania).
    fn fans(&self, dev: Device) -> u32 {
        let Some(f) = self.num_fans else { return 0 };
        let mut n = 0u32;
        // SAFETY: uchwyt z NVML, wskaźnik na lokalną zmienną.
        if unsafe { f(dev, &mut n) } == 0 { n.min(16) } else { 0 }
    }

    /// Wentylatory: obroty najszybszego (RPM) i wysterowanie pierwszego (%).
    pub fn fan_status(&self, index: usize, name: &str) -> Option<(Option<u32>, Option<u32>)> {
        let dev = self.device(index, name)?;
        let n = self.fans(dev);
        if n == 0 {
            return None;
        }
        // SAFETY (oba odczyty): uchwyt z NVML, struktury i zmienne lokalne.
        let pct = self.fan_v2.and_then(|f| {
            let mut v = 0u32;
            (unsafe { f(dev, 0, &mut v) } == 0).then_some(v.min(100))
        });
        let rpm = self.fan_rpm.and_then(|f| {
            (0..n)
                .filter_map(|fan| {
                    let mut s = FanSpeedInfo { version: FAN_SPEED_INFO_V1, fan, speed: 0 };
                    (unsafe { f(dev, &mut s) } == 0).then_some(s.speed)
                })
                .max()
        });
        Some((rpm, pct))
    }

    /// Czy wentylatory karty da się ustawiać (są i funkcje zapisu istnieją).
    pub fn fan_controllable(&self, index: usize, name: &str) -> bool {
        self.set_fan.is_some()
            && self.default_fan.is_some()
            && self.device(index, name).is_some_and(|d| self.fans(d) > 0)
    }

    /// Ustawia wszystkie wentylatory na `pct` albo oddaje je sterownikowi
    /// (`None`, tryb automatyczny). Wymaga uprawnień administratora.
    pub fn set_fans(&self, index: usize, name: &str, pct: Option<u32>) -> Result<(), String> {
        let dev = self.device(index, name).ok_or_else(|| crate::i18n::tr("NVML nie widzi tej karty", "NVML doesn't see this card").to_string())?;
        let n = self.fans(dev);
        if n == 0 {
            return Err(crate::i18n::tr("sterownik nie zgłosił żadnego wentylatora", "the driver reported no fans").to_string());
        }
        for fan in 0..n {
            // SAFETY: uchwyt z NVML, numer wentylatora z nvmlDeviceGetNumFans.
            let r = match (pct, self.set_fan, self.default_fan) {
                (Some(p), Some(f), _) => unsafe { f(dev, fan, p.min(100)) },
                (None, _, Some(f)) => unsafe { f(dev, fan) },
                _ => return Err("NVML: brak funkcji / missing function".into()),
            };
            if r != 0 {
                return Err(crate::trf!("NVML: wentylator {fan}: błąd {r}", "NVML: fan {fan}: error {r}"));
            }
        }
        Ok(())
    }

    /// Chwilowa moc samego układu GPU (W), jeśli karta ją podaje.
    pub fn chip_power(&self, index: usize, name: &str) -> Option<f32> {
        let dev = self.device(index, name)?;
        let f = self.fields?;
        let mut v = FieldValue { field_id: FIELD_POWER_INSTANT, scope_id: POWER_SCOPE_GPU, ..Default::default() };
        // SAFETY: uchwyt z NVML, jedna struktura wyniku.
        (unsafe { f(dev, 1, &mut v) } == 0 && v.status == 0 && v.value as u32 > 0).then(|| v.value as u32 as f32 / 1000.0)
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

#[cfg(test)]
mod field_probe {
    //! Przegląd pól NVML (tylko odczyt):
    //! `cargo test nvml_field_probe -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn nvml_field_probe() {
        let n = Nvml::load().expect("NVML");
        let (dev, _) = n.devices[0];
        let f = n.fields.expect("nvmlDeviceGetFieldValues");
        for id in 1..400u32 {
            let mut v = FieldValue {
                field_id: id,
                ..Default::default()
            };
            if unsafe { f(dev, 1, &mut v) } == 0 && v.status == 0 {
                let raw = v.value;
                let as_f = f64::from_bits(raw);
                println!(
                    "pole {id}: typ {} wartość {} (f64 {:.2})",
                    v.value_type, raw as i64, as_f
                );
            }
        }
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// Czas (ściana) i CPU wątku dla każdego odczytu NVML:
    /// `cargo test nvml_timing --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn nvml_timing() {
        fn cpu() -> f64 {
            use windows::Win32::Foundation::FILETIME;
            use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
            let (mut a, mut b, mut k, mut u) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            unsafe {
                let _ = GetThreadTimes(GetCurrentThread(), &mut a, &mut b, &mut k, &mut u);
            }
            let t = |f: FILETIME| {
                ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e4
            };
            t(k) + t(u)
        }
        let n = Nvml::load().expect("NVML");
        let dev = n
            .device(0, "")
            .or_else(|| n.device(0, "NVIDIA GeForce RTX 5070"))
            .expect("karta");
        let value = |f: Option<FnValue>| {
            let mut v = 0u32;
            f.map(|f| unsafe { f(dev, &mut v) })
        };
        let pair = |f: Option<FnPair>| {
            let (mut a, mut b) = (0u32, 0u32);
            f.map(|f| unsafe { f(dev, &mut a, &mut b) })
        };
        let arg = |f: Option<FnArg>, x: u32| {
            let mut v = 0u32;
            f.map(|f| unsafe { f(dev, x, &mut v) })
        };
        let mut run = |name: &str, g: &mut dyn FnMut()| {
            let (t0, c0) = (std::time::Instant::now(), cpu());
            for _ in 0..10 {
                g();
            }
            println!(
                "{name:<12} {:7.2} ms ściana  {:7.2} ms CPU (na wywołanie)",
                t0.elapsed().as_secs_f64() * 100.0,
                (cpu() - c0) / 10.0
            );
        };
        run("power", &mut || {
            value(n.power);
        });
        run("limit", &mut || {
            value(n.default_limit);
        });
        run("memory", &mut || {
            if let Some(f) = n.memory {
                let mut m = Memory::default();
                unsafe { f(dev, &mut m) };
            }
        });
        run("mem temp", &mut || {
            if let Some(f) = n.fields {
                let mut v = FieldValue {
                    field_id: FIELD_MEMORY_TEMP,
                    ..Default::default()
                };
                unsafe { f(dev, 1, &mut v) };
            }
        });
        run("reasons", &mut || {
            if let Some(f) = n.reasons {
                let mut v = 0u64;
                unsafe { f(dev, &mut v) };
            }
        });
        run("util", &mut || {
            pair(n.utilization);
        });
        run("pcie tx", &mut || {
            arg(n.pcie, 0);
        });
        run("pcie rx", &mut || {
            arg(n.pcie, 1);
        });
        run("pcie gen", &mut || {
            value(n.pcie_gen);
            value(n.pcie_width);
        });
        run("encoder", &mut || {
            pair(n.encoder);
        });
        run("decoder", &mut || {
            pair(n.decoder);
        });
        run("clock", &mut || {
            arg(n.clock, 3);
        });
        run("pstate", &mut || {
            value(n.pstate);
        });
        run("fan", &mut || {
            value(n.fan);
        });
    }
}

#[cfg(test)]
mod fan_probe {
    //! Wentylatory i moc układu z NVML (tylko odczyt):
    //! `cargo test nvml_fan_probe -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn nvml_fan_probe() {
        let n = Nvml::load().expect("NVML");
        let (dev, name) = n.devices[0].clone();
        println!("karta {name}, wentylatorów {}", n.fans(dev));
        println!("stan {:?}", n.fan_status(0, &name));
        println!("sterowanie {}", n.fan_controllable(0, &name));
        println!("moc układu {:?} W, karty {:?} W", n.chip_power(0, &name), n.read(0, &name, false).power_w);
    }
}

#[cfg(test)]
mod fan_write {
    //! Zapis wentylatora przez NVML: 40 % na 6 s, potem tryb automatyczny.
    //! Zmienia pracę wentylatora karty! `cargo test nvml_fan_write -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn nvml_fan_write() {
        let n = Nvml::load().expect("NVML");
        let name = n.devices[0].1.clone();
        println!("ustaw 40%: {:?}", n.set_fans(0, &name, Some(40)));
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            println!("stan {:?}", n.fan_status(0, &name));
        }
        println!("auto: {:?}", n.set_fans(0, &name, None));
        std::thread::sleep(std::time::Duration::from_secs(3));
        println!("po auto {:?}", n.fan_status(0, &name));
    }
}
