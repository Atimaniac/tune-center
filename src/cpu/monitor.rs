//! Odczyty procesora Intel jak w Intel XTU (temperatury, moc, napięcie,
//! zegary, przyczyny ograniczeń) - przez PawnIO i moduł `IntelMSR.bin`.
//!
//! XTU (IntelOverclockingSDK, lista czujników `MonitorMetadata.xml`) czyta
//! te wartości własnym sterownikiem z rejestrów MSR opisanych w Intel SDM;
//! tu czytamy te same rejestry przez oficjalny moduł PawnIO:
//! - temperatura pakietu: 0x1B1 (IA32_PACKAGE_THERM_STATUS), TjMax z 0x1A2,
//! - temperatury rdzeni: 0x19C (IA32_THERM_STATUS) na każdym rdzeniu,
//! - moc pakietu / rdzeni / grafiki: liczniki energii RAPL 0x611 / 0x639 /
//!   0x641, jednostka z 0x606,
//! - napięcie rdzenia (VID): 0x198 bity 47:32 (1/8192 V),
//! - zegary rdzeni: APERF/MPERF (0xE8/0xE7) x zegar bazowy (0xCE),
//! - zegar cache (ring): 0x621 bity 6:0 x 100 MHz,
//! - ograniczenia (thermal, power PL1/PL2, current/EDP, VR): 0x64F.
//!
//! Odczyt na danym rdzeniu wymaga przypięcia wątku do tego rdzenia, więc
//! wszystko działa w osobnym wątku z własnym połączeniem PawnIO.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::intel::Msr;
use super::pawnio::PawnIo;

const IA32_MPERF: u64 = 0xE7;
const IA32_APERF: u64 = 0xE8;
const PLATFORM_INFO: u64 = 0xCE;
const PERF_STATUS: u64 = 0x198;
const THERM_STATUS: u64 = 0x19C;
const TEMPERATURE_TARGET: u64 = 0x1A2;
const PACKAGE_THERM_STATUS: u64 = 0x1B1;
const RAPL_POWER_UNIT: u64 = 0x606;
const PKG_ENERGY: u64 = 0x611;
const PP0_ENERGY: u64 = 0x639;
const PP1_ENERGY: u64 = 0x641;
const UNCORE_PERF_STATUS: u64 = 0x621;
const PERF_LIMIT_REASONS: u64 = 0x64F;

/// Jeden rdzeń fizyczny.
#[derive(Clone, Debug, Default)]
pub struct Core {
    /// "P0", "E3"...
    pub label: String,
    pub efficient: bool,
    pub temp_c: Option<f32>,
    pub mhz: Option<f32>,
    pub vid_v: Option<f32>,
}

/// Odczyty z jednej chwili.
#[derive(Clone, Debug, Default)]
pub struct Readings {
    pub tjmax_c: Option<f32>,
    pub package_c: Option<f32>,
    pub package_w: Option<f32>,
    pub cores_w: Option<f32>,
    pub graphics_w: Option<f32>,
    pub vid_v: Option<f32>,
    pub ring_ghz: Option<f32>,
    pub cores: Vec<Core>,
    /// Ograniczenia aktywne teraz (bity 0-15 rejestru 0x64F).
    pub limits: Vec<&'static str>,
    pub error: Option<String>,
}

impl Readings {
    pub fn max_core(&self, efficient: bool) -> Option<f32> {
        self.cores
            .iter()
            .filter(|c| c.efficient == efficient)
            .filter_map(|c| c.mhz)
            .reduce(f32::max)
    }
}

/// Nazwy bitów stanu 0x64F (Intel SDM, MSR_CORE_PERF_LIMIT_REASONS).
fn limit_names(v: u64) -> Vec<&'static str> {
    use crate::i18n::tr;
    let names: [(u32, &'static str); 9] = [
        (0, "PROCHOT"),
        (1, tr("temperatura", "thermal")),
        (5, tr("średnia temperatura", "avg thermal")),
        (6, tr("temperatura VR", "VR thermal")),
        (7, "VR TDC"),
        (8, tr("prąd / EDP", "current / EDP")),
        (10, "PL1"),
        (11, "PL2"),
        (12, tr("maks. turbo", "max turbo")),
    ];
    names
        .iter()
        .filter(|(bit, _)| v >> bit & 1 == 1)
        .map(|(_, n)| *n)
        .collect()
}

/// Rdzeń fizyczny: maska pierwszego procesora logicznego i klasa wydajności.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Topo {
    pub mask: usize,
    pub class: u8,
}

/// Rdzenie fizyczne z Windows (GetLogicalProcessorInformationEx, grupa 0).
#[cfg(windows)]
pub(crate) fn topology() -> Vec<Topo> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore,
    };
    let mut len = 0u32;
    // SAFETY: najpierw zapytanie o rozmiar, potem bufor tej wielkości.
    unsafe {
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len);
        let mut buf = vec![0u8; len as usize];
        if GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr() as *mut _),
            &mut len,
        )
        .is_err()
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 48 <= len as usize {
            let size = u32::from_le_bytes(buf[at + 4..at + 8].try_into().unwrap()) as usize;
            let class = buf[at + 9];
            let mask = usize::from_le_bytes(buf[at + 32..at + 40].try_into().unwrap());
            let group = u16::from_le_bytes(buf[at + 40..at + 42].try_into().unwrap());
            if group == 0 && mask != 0 {
                // Pierwszy procesor logiczny rdzenia.
                out.push(Topo {
                    mask: mask.isolate_lowest_one(),
                    class,
                });
            }
            if size == 0 {
                break;
            }
            at += size;
        }
        out
    }
}

#[cfg(not(windows))]
pub(crate) fn topology() -> Vec<Topo> {
    Vec::new()
}

/// Uruchamia `f` na procesorze logicznym z maski (przypięcie wątku).
#[cfg(windows)]
fn on_cpu<T>(mask: usize, f: impl FnOnce() -> T) -> T {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
    // SAFETY: zmiana powinowactwa bieżącego wątku; przywracana po wywołaniu.
    unsafe {
        let old = SetThreadAffinityMask(GetCurrentThread(), mask);
        // Windows przenosi wątek przy następnym przydziale czasu.
        std::thread::yield_now();
        let r = f();
        if old != 0 {
            SetThreadAffinityMask(GetCurrentThread(), old);
        }
        r
    }
}

#[cfg(not(windows))]
fn on_cpu<T>(_mask: usize, f: impl FnOnce() -> T) -> T {
    f()
}

/// Liczniki z poprzedniego odczytu (moc i zegary liczymy z różnic).
#[derive(Default)]
struct Previous {
    at: Option<Instant>,
    pkg: Option<u64>,
    pp0: Option<u64>,
    pp1: Option<u64>,
    aperf_mperf: Vec<Option<(u64, u64)>>,
}

struct Reader {
    io: PawnIo,
    cores: Vec<Topo>,
    hybrid: bool,
    tjmax: Option<f32>,
    energy_unit: Option<f64>,
    base_mhz: Option<f32>,
    prev: Previous,
}

impl Reader {
    fn new(io: PawnIo) -> Self {
        let cores = topology();
        let hybrid = cores.iter().any(|c| c.class != cores[0].class);
        let mut r = Self {
            io,
            cores,
            hybrid,
            tjmax: None,
            energy_unit: None,
            base_mhz: None,
            prev: Previous::default(),
        };
        r.tjmax =
            r.io.read(TEMPERATURE_TARGET)
                .ok()
                .map(|v| ((v >> 16) & 0xFF) as f32)
                .filter(|t| *t > 0.0);
        r.energy_unit =
            r.io.read(RAPL_POWER_UNIT)
                .ok()
                .map(|v| 1.0 / (1u64 << ((v >> 8) & 0x1F)) as f64);
        r.base_mhz =
            r.io.read(PLATFORM_INFO)
                .ok()
                .map(|v| ((v >> 8) & 0xFF) as f32 * 100.0)
                .filter(|m| *m > 0.0);
        r
    }

    fn read(&mut self) -> Readings {
        let now = Instant::now();
        let dt = self.prev.at.map(|t| now.duration_since(t).as_secs_f64());
        let mut r = Readings {
            tjmax_c: self.tjmax,
            ..Default::default()
        };

        let tj = self.tjmax;
        let temp = |v: u64| -> Option<f32> {
            // Bit 31 = odczyt ważny; bity 22:16 = odległość od TjMax.
            (v >> 31 & 1 == 1)
                .then(|| tj.map(|t| t - ((v >> 16) & 0x7F) as f32))
                .flatten()
        };
        r.package_c = self.io.read(PACKAGE_THERM_STATUS).ok().and_then(temp);

        // Moc z liczników energii (32-bitowe, z zawinięciem).
        let mut power = |msr: u64, prev: &mut Option<u64>| -> Option<f32> {
            let v = self.io.read(msr).ok()? & 0xFFFF_FFFF;
            let last = prev.replace(v);
            let (Some(last), Some(dt), Some(unit)) = (last, dt, self.energy_unit) else {
                return None;
            };
            let delta = v.wrapping_sub(last) & 0xFFFF_FFFF;
            (dt > 0.05).then(|| (delta as f64 * unit / dt) as f32)
        };
        let mut pkg = self.prev.pkg;
        let mut pp0 = self.prev.pp0;
        let mut pp1 = self.prev.pp1;
        r.package_w = power(PKG_ENERGY, &mut pkg);
        r.cores_w = power(PP0_ENERGY, &mut pp0);
        r.graphics_w = power(PP1_ENERGY, &mut pp1);
        (self.prev.pkg, self.prev.pp0, self.prev.pp1) = (pkg, pp0, pp1);

        r.ring_ghz = self
            .io
            .read(UNCORE_PERF_STATUS)
            .ok()
            .map(|v| (v & 0x7F) as f32 / 10.0)
            .filter(|g| *g > 0.0);
        if let Ok(v) = self.io.read(PERF_LIMIT_REASONS) {
            r.limits = limit_names(v & 0xFFFF);
        }

        // Rdzenie: wątek przypinany kolejno do każdego rdzenia.
        if self.prev.aperf_mperf.len() != self.cores.len() {
            self.prev.aperf_mperf = vec![None; self.cores.len()];
        }
        let max_class = self.cores.iter().map(|c| c.class).max().unwrap_or(0);
        let (mut p_index, mut e_index) = (0, 0);
        let mut vid_max: Option<f32> = None;
        for (i, topo) in self.cores.clone().iter().enumerate() {
            let io = &mut self.io;
            let (therm, perf, aperf, mperf) = on_cpu(topo.mask, || {
                (
                    io.read(THERM_STATUS).ok(),
                    io.read(PERF_STATUS).ok(),
                    io.read(IA32_APERF).ok(),
                    io.read(IA32_MPERF).ok(),
                )
            });
            let efficient = self.hybrid && topo.class < max_class;
            let label = if efficient {
                e_index += 1;
                format!("E{}", e_index - 1)
            } else {
                p_index += 1;
                format!("P{}", p_index - 1)
            };
            let vid = perf
                .map(|v| ((v >> 32) & 0xFFFF) as f32 / 8192.0)
                .filter(|v| *v > 0.2 && *v < 2.0);
            vid_max = match (vid_max, vid) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            let mhz = match (aperf, mperf, self.prev.aperf_mperf[i], self.base_mhz) {
                (Some(a), Some(m), Some((pa, pm)), Some(base)) if m > pm => {
                    Some(base * (a.wrapping_sub(pa)) as f32 / (m - pm) as f32)
                }
                _ => None,
            };
            if let (Some(a), Some(m)) = (aperf, mperf) {
                self.prev.aperf_mperf[i] = Some((a, m));
            }
            r.cores.push(Core {
                label,
                efficient,
                temp_c: therm.and_then(temp),
                mhz: mhz.filter(|m| *m > 0.0 && *m < 10_000.0),
                vid_v: vid,
            });
        }
        r.vid_v = vid_max;
        self.prev.at = Some(now);
        r
    }
}

/// Odczyty co sekundę w osobnym wątku.
pub struct Monitor {
    pub latest: Arc<Mutex<Option<Readings>>>,
    stop: Arc<AtomicBool>,
}

impl Monitor {
    /// `module`: ścieżka do IntelMSR.bin.
    pub fn start(module: std::path::PathBuf) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s) = (latest.clone(), stop.clone());
        std::thread::Builder::new()
            .name("cpu-monitor".into())
            .spawn(move || {
                let io = match PawnIo::open(&module) {
                    Ok(io) => io,
                    Err(e) => {
                        *l.lock().unwrap() = Some(Readings {
                            error: Some(e),
                            ..Default::default()
                        });
                        return;
                    }
                };
                let mut reader = Reader::new(io);
                while !s.load(Ordering::Relaxed) {
                    let r = reader.read();
                    *l.lock().unwrap() = Some(r);
                    std::thread::sleep(Duration::from_millis(1000));
                }
            })
            .ok();
        Self { latest, stop }
    }

    pub fn get(&self) -> Option<Readings> {
        self.latest.lock().unwrap().clone()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod hardware {
    //! Odczyty jak w XTU na tym komputerze (tylko odczyt, administrator):
    //! `cargo test cpu_monitor -- --ignored --nocapture`
    //! (moduł w target\release\deps\modules\IntelMSR.bin).
    use super::*;

    #[test]
    #[ignore]
    fn cpu_monitor() {
        let exe = std::env::current_exe().unwrap();
        let module = exe.parent().unwrap().join("modules").join("IntelMSR.bin");
        let mut reader = Reader::new(PawnIo::open(&module).expect("PawnIO"));
        println!(
            "rdzeni: {}, hybryda: {}, TjMax {:?}, baza {:?} MHz",
            reader.cores.len(),
            reader.hybrid,
            reader.tjmax,
            reader.base_mhz
        );
        for _ in 0..3 {
            let r = reader.read();
            println!(
                "pakiet {:?} °C, moc pakiet {:?} W rdzenie {:?} W grafika {:?} W, VID {:?} V, ring {:?} GHz, ograniczenia {:?}",
                r.package_c, r.package_w, r.cores_w, r.graphics_w, r.vid_v, r.ring_ghz, r.limits
            );
            for c in &r.cores {
                print!(
                    "{} {:?}°C {:?}MHz  ",
                    c.label,
                    c.temp_c,
                    c.mhz.map(|m| m.round())
                );
            }
            println!();
            std::thread::sleep(Duration::from_millis(1000));
        }
    }
}
