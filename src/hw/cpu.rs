//! Czujniki procesora jak w LibreHardwareMonitor (`IntelCpu`, `Amd17Cpu`):
//! temperatury i moc, z tymi samymi identyfikatorami i nazwami.
//!
//! - Intel: moduł PawnIO `IntelMSR.bin` (tylko odczyt MSR). Temperatury rdzeni
//!   z IA32_THERM_STATUS (0x19C, odczyt na danym rdzeniu), pakietu z 0x1B1,
//!   TjMax z 0x1A2; moc z liczników energii RAPL (0x611 / 0x639 / 0x619 /
//!   0x64D, jednostka z 0x606) - z różnicy dwóch odczytów.
//! - AMD Zen (rodziny 17h, 19h, 1Ah): moduł `AMDFamily17.bin` - temperatura
//!   Tctl/Tdie i temperatury CCD z rejestrów SMN, moc pakietu i rdzeni z
//!   liczników energii MSR 0xC001029B / 0xC001029A; opcjonalnie `RyzenSMU.bin`
//!   - czujniki z tabeli PM (tylko znane wersje tabeli, jak w LHM).
//!
//! Kolejność rdzeni i ich nazwy (P-Core / E-Core / CPU Core #n) liczymy z
//! CPUID tak samo jak LHM, bo od nich zależą numery w identyfikatorach.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use crate::cpu::pawnio::{NamedLock, PawnIo};

use super::{valid_temp, Readings};

const KIND: &str = "Cpu";
/// Blokada magistrali PCI (SMN, SMU) - wspólna z LHM, HWiNFO, Ryzen Master.
const PCI_LOCK: &str = r"Global\Access_PCI";

// ---------------------------------------------------------------------------
// CPUID i topologia
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[allow(unused_unsafe)]
fn cpuid(leaf: u32) -> [u32; 4] {
    // SAFETY: instrukcja CPUID jest dostępna na każdym procesorze x86_64.
    let r = unsafe { std::arch::x86_64::__cpuid_count(leaf, 0) };
    [r.eax, r.ebx, r.ecx, r.edx]
}

#[cfg(not(target_arch = "x86_64"))]
fn cpuid(_leaf: u32) -> [u32; 4] {
    [0; 4]
}

/// Dane CPUID jednego procesora logicznego (to, czego używa LHM).
#[derive(Clone, Debug, Default)]
struct Thread {
    /// Maska powinowactwa (grupa 0).
    mask: usize,
    max_leaf: u32,
    max_ext: u32,
    leaf1: [u32; 4],
    leaf4_eax: u32,
    leaf6_eax: u32,
    leaf1a_eax: u32,
    ext8_ecx: u32,
    ext1e: [u32; 4],
}

impl Thread {
    fn read(mask: usize) -> Self {
        let max_leaf = cpuid(0)[0].min(1024);
        let max_ext = cpuid(0x8000_0000)[0];
        let leaf = |l: u32| if max_leaf >= l { cpuid(l) } else { [0; 4] };
        let ext = |l: u32| if max_ext >= l { cpuid(l) } else { [0; 4] };
        Thread {
            mask,
            max_leaf,
            max_ext,
            leaf1: leaf(1),
            leaf4_eax: leaf(4)[0],
            leaf6_eax: leaf(6)[0],
            leaf1a_eax: leaf(0x1A)[0],
            ext8_ecx: ext(0x8000_0008)[2],
            ext1e: ext(0x8000_001E),
        }
    }

    /// Rodzina i model jak w LHM (rozszerzone pola zawsze doliczane).
    fn family(&self) -> u32 {
        let eax = self.leaf1[0];
        ((eax & 0x0FF0_0000) >> 20) + ((eax & 0x0F00) >> 8)
    }

    fn model(&self) -> u32 {
        let eax = self.leaf1[0];
        ((eax & 0x0F_0000) >> 12) + ((eax & 0xF0) >> 4)
    }

    fn stepping(&self) -> u32 {
        self.leaf1[0] & 0xF
    }

    /// (ProcessorId, CoreId) wg wzorów LHM (`CpuId`).
    fn ids(&self, intel: bool) -> (u32, u32) {
        let apic = (self.leaf1[1] >> 24) & 0xFF;
        let (thread_bits, core_bits) = if intel {
            let max_core_thread = (self.leaf1[1] >> 16) & 0xFF;
            let max_core = if self.max_leaf >= 4 { ((self.leaf4_eax >> 26) & 0x3F) + 1 } else { 1 };
            (next_log2(max_core_thread / max_core), next_log2(max_core))
        } else {
            let per_pkg = if self.max_ext >= 0x8000_0008 { (self.ext8_ecx & 0xFF) + 1 } else { 1 };
            let mut core_bits = next_log2(per_pkg);
            if matches!(self.family(), 0x17 | 0x19 | 0x1A) {
                core_bits = match (self.ext8_ecx >> 12) & 0xF {
                    4 => 4,
                    5 => 5,
                    6 => 6,
                    _ => core_bits,
                };
            }
            (0, core_bits)
        };
        let shift = (core_bits + thread_bits).min(31);
        let processor = apic >> shift;
        let core = (apic >> thread_bits.min(31)).wrapping_sub(processor << core_bits.min(31));
        (processor, core)
    }
}

fn next_log2(x: u32) -> u32 {
    if x == 0 {
        return 0;
    }
    32 - (x - 1).leading_zeros()
}

/// Uruchamia `f` na procesorze logicznym z maski; `None`, gdy nie da się
/// przypiąć wątku.
#[cfg(windows)]
fn on_cpu<T>(mask: usize, f: impl FnOnce() -> T) -> Option<T> {
    use windows::Win32::System::Threading::{
        GetCurrentProcessorNumber, GetCurrentThread, SetThreadAffinityMask,
    };
    if mask == 0 {
        return None;
    }
    let target = mask.trailing_zeros();
    // SAFETY: zmiana powinowactwa bieżącego wątku, przywracana po wywołaniu.
    unsafe {
        let old = SetThreadAffinityMask(GetCurrentThread(), mask);
        if old == 0 {
            return None;
        }
        // Windows przenosi wątek przy najbliższym przydziale czasu.
        for _ in 0..20 {
            if GetCurrentProcessorNumber() == target {
                break;
            }
            std::thread::yield_now();
        }
        let r = f();
        SetThreadAffinityMask(GetCurrentThread(), old);
        Some(r)
    }
}

#[cfg(not(windows))]
fn on_cpu<T>(_mask: usize, f: impl FnOnce() -> T) -> Option<T> {
    Some(f())
}

/// Procesory logiczne grupy 0 (jak LHM: najwyżej 64).
fn threads() -> Vec<Thread> {
    #[cfg(windows)]
    let count = {
        // SAFETY: proste zapytanie bez wskaźników.
        unsafe { windows::Win32::System::Threading::GetActiveProcessorCount(0) }
    };
    #[cfg(not(windows))]
    let count = 1u32;
    (0..count.min(usize::BITS))
        .filter_map(|i| {
            let mask = 1usize << i;
            on_cpu(mask, || Thread::read(mask))
        })
        .collect()
}

/// Pakiety (wg ProcessorId) z rdzeniami (wg CoreId) - grupowanie LHM.
fn packages(all: Vec<Thread>, intel: bool) -> Vec<Vec<Vec<Thread>>> {
    let mut map: BTreeMap<u32, BTreeMap<u32, Vec<Thread>>> = BTreeMap::new();
    for t in all {
        let (p, c) = t.ids(intel);
        map.entry(p).or_default().entry(c).or_default().push(t);
    }
    map.into_values().map(|cores| cores.into_values().collect()).collect()
}

/// Ciąg marki z CPUID (bajty jak znaki Latin-1, jak w LHM).
fn brand() -> String {
    let mut s = String::new();
    if cpuid(0x8000_0000)[0] >= 0x8000_0004 {
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            for reg in cpuid(leaf) {
                s.extend(reg.to_le_bytes().iter().map(|&b| if b == 0 { ' ' } else { b as char }));
            }
        }
    }
    s.trim().to_owned()
}

/// Nazwa procesora jak w LHM (`CpuId.Name`): bez (R), (TM), "CPU", liczby
/// rdzeni i częstotliwości po "@".
fn lhm_name(brand: &str) -> String {
    let mut s = brand.to_owned();
    for pat in [
        "(R)",
        "(TM)",
        "(tm)",
        "CPU",
        "Dual-Core Processor",
        "Triple-Core Processor",
        "Quad-Core Processor",
        "Six-Core Processor",
        "Eight-Core Processor",
        "64-Core Processor",
        "32-Core Processor",
        "24-Core Processor",
        "16-Core Processor",
        "12-Core Processor",
        "8-Core Processor",
        "6-Core Processor",
    ] {
        s = s.replace(pat, "");
    }
    for _ in 0..10 {
        s = s.replace("  ", " ");
    }
    if let Some(at) = s.rfind('@') {
        s.truncate(at);
    }
    s.trim().to_owned()
}

/// Nazwa podzespołu: nazwa LHM, a dla próbki inżynierskiej ("Genuine Intel
/// 0000") - poprawiona nazwa programu, tak jak robił to most do LHM.
fn hw_name(lhm: &str) -> String {
    let fixed = crate::cpu::name();
    if crate::cpu::is_engineering_name(lhm) && !crate::cpu::is_engineering_name(fixed) {
        fixed.to_owned()
    } else {
        lhm.to_owned()
    }
}

fn read_msr(io: &PawnIo, msr: u32) -> Option<u64> {
    let mut out = [0u64; 1];
    io.call(b"ioctl_read_msr\0", &[msr as u64], &mut out).ok()?;
    Some(out[0])
}

/// Licznik energii (32 bity, z zawinięciem) i chwila odczytu.
#[derive(Clone, Copy)]
struct Energy {
    last: u32,
    at: Instant,
}

impl Energy {
    /// Moc w W od poprzedniego odczytu (`unit` = J na jednostkę licznika).
    fn update(&mut self, raw: u32, unit: f64, now: Instant) -> Option<f32> {
        let dt = now.duration_since(self.at).as_secs_f64();
        if dt < 0.01 {
            return None;
        }
        let delta = raw.wrapping_sub(self.last);
        self.last = raw;
        self.at = now;
        let w = unit * delta as f64 / dt;
        w.is_finite().then_some(w as f32)
    }
}

/// Czujniki procesora.
pub struct Cpu {
    inner: Inner,
}

enum Inner {
    Intel(PawnIo, Vec<IntelPkg>),
    Amd(Box<Amd>),
}

impl Cpu {
    pub fn open() -> Option<Self> {
        let dir = super::module_path("IntelMSR.bin")?.parent()?.to_path_buf();
        Self::open_in(&dir)
    }

    /// Otwiera z modułami z podanego folderu.
    fn open_in(dir: &Path) -> Option<Self> {
        let info = crate::cpu::cpu_info();
        let inner = if info.intel {
            let io = PawnIo::open(&dir.join("IntelMSR.bin")).ok()?;
            let pkgs = IntelPkg::open_all(&io)?;
            Inner::Intel(io, pkgs)
        } else if info.amd && matches!(info.family, 0x17 | 0x19 | 0x1A) {
            Inner::Amd(Box::new(Amd::open(dir)?))
        } else {
            return None;
        };
        Some(Self { inner })
    }

    pub fn read(&mut self, out: &mut Readings) {
        match &mut self.inner {
            Inner::Intel(io, pkgs) => pkgs.iter_mut().for_each(|p| p.read(io, out)),
            Inner::Amd(amd) => amd.read(out),
        }
    }
}

// ---------------------------------------------------------------------------
// Intel
// ---------------------------------------------------------------------------

const IA32_THERM_STATUS: u32 = 0x19C;
const IA32_TEMPERATURE_TARGET: u32 = 0x1A2;
const IA32_PACKAGE_THERM_STATUS: u32 = 0x1B1;
const MSR_RAPL_POWER_UNIT: u32 = 0x606;
/// Liczniki energii RAPL z nazwami LHM (Graphics LHM pomija na Windows).
const RAPL: [(u32, &str); 5] = [
    (0x611, "CPU Package"),
    (0x639, "CPU Cores"),
    (0x641, "CPU Graphics"),
    (0x619, "CPU Memory"),
    (0x64D, "CPU Platform"),
];

/// Skąd TjMax i czy jest RAPL - wg mikroarchitektury z tabeli LHM.
#[derive(Clone, Copy, PartialEq)]
enum Arch {
    Unknown,
    /// Core 2 (65 nm / 45 nm), stały TjMax, bez RAPL.
    Core,
    /// Atom 45 nm (model 0x1C), stały TjMax, bez RAPL.
    Atom45,
    /// TjMax z MSR, bez RAPL (Nehalem, Atom 32 nm).
    MsrNoRapl,
    /// Silvermont / Airmont: RAPL w mikrodżulach.
    Silvermont,
    /// Pozostałe znane: TjMax z MSR i RAPL.
    Modern,
    /// Pentium 4: TjMax 100, bez RAPL.
    NetBurst,
}

fn intel_arch(family: u32, model: u32) -> Arch {
    match family {
        6 => match model {
            0x0F | 0x17 => Arch::Core,
            0x1C => Arch::Atom45,
            0x1A | 0x1E | 0x1F | 0x25 | 0x2C | 0x2E | 0x2F | 0x36 => Arch::MsrNoRapl,
            0x37 | 0x4A | 0x4D | 0x5A | 0x5D | 0x4C => Arch::Silvermont,
            0x2A | 0x2D | 0x3A | 0x3E | 0x3C | 0x3F | 0x45 | 0x46 | 0x3D | 0x47 | 0x4F | 0x56
            | 0x4E | 0x5E | 0x55 | 0x8E | 0x9E | 0x5C | 0x5F | 0x7A | 0x66 | 0x7D | 0x7E
            | 0x6A | 0x6C | 0xA5 | 0xA6 | 0x86 | 0x8C | 0x8D | 0x97 | 0x9A | 0xBE | 0xB7
            | 0xBA | 0xBF | 0xAC | 0xAA | 0x9C | 0xA7 | 0xB5 | 0xC5 | 0xC6 | 0xBD | 0x8F
            | 0x96 | 0xCC => Arch::Modern,
            _ => Arch::Unknown,
        },
        0x0F => match model {
            0x00..=0x04 | 0x06 => Arch::NetBurst,
            _ => Arch::Unknown,
        },
        _ => Arch::Unknown,
    }
}

/// Stały TjMax starszych procesorów (tabela LHM).
fn fixed_tjmax(model: u32, stepping: u32, cores: usize) -> f32 {
    match (model, stepping) {
        (0x0F, 0x06) => match cores {
            2 => 90.0,
            4 => 100.0,
            _ => 95.0,
        },
        (0x0F, 0x0B) => 100.0,
        (0x0F, _) => 95.0,
        (0x17, _) => 100.0,
        (0x1C, 0x0A) => 100.0,
        (0x1C, _) => 90.0,
        _ => 100.0,
    }
}

struct IntelCore {
    /// Maska pierwszego wątku rdzenia.
    mask: usize,
    name: String,
    tjmax: f32,
}

struct IntelPkg {
    index: usize,
    hw: String,
    mask: usize,
    cores: Vec<IntelCore>,
    /// Czujniki rdzeni (CPUID 6 EAX bit 0) i pakietu (bit 6).
    dts: bool,
    ptm: bool,
    tjmax_pkg: f32,
    unit: f64,
    /// Liczniki RAPL: indeks w `RAPL` i ostatni odczyt.
    energy: Vec<(usize, Energy)>,
}

impl IntelPkg {
    fn open_all(io: &PawnIo) -> Option<Vec<Self>> {
        let name = hw_name(&lhm_name(&brand()));
        let pkgs: Vec<Self> = packages(threads(), true)
            .into_iter()
            .enumerate()
            .filter_map(|(i, cores)| Self::open(io, i, cores, &name))
            .collect();
        (!pkgs.is_empty()).then_some(pkgs)
    }

    fn open(io: &PawnIo, index: usize, cores: Vec<Vec<Thread>>, hw: &str) -> Option<Self> {
        let first = cores.first()?.first()?.clone();
        let arch = intel_arch(first.family(), first.model());
        let known = arch != Arch::Unknown;
        let dts = known && first.max_leaf >= 6 && first.leaf6_eax & 1 != 0;
        let ptm = known && first.max_leaf >= 6 && first.leaf6_eax & 0x40 != 0;

        let count = cores.len();
        let (mut p, mut e) = (0, 0);
        let mut list = Vec::with_capacity(count);
        for (i, threads) in cores.iter().enumerate() {
            let Some(t) = threads.first() else { continue };
            let core_type = if t.max_leaf >= 0x1A { (t.leaf1a_eax >> 24) & 0xFF } else { 0 };
            let name = match core_type {
                0x40 => {
                    p += 1;
                    format!("P-Core #{p}")
                }
                0x20 => {
                    e += 1;
                    format!("E-Core #{e}")
                }
                _ if count == 1 => "CPU Core".to_owned(),
                _ => format!("CPU Core #{}", i + 1),
            };
            let tjmax = match arch {
                Arch::Core | Arch::Atom45 => fixed_tjmax(first.model(), first.stepping(), count),
                Arch::Unknown | Arch::NetBurst => 100.0,
                _ => on_cpu(t.mask, || read_msr(io, IA32_TEMPERATURE_TARGET))
                    .flatten()
                    .map_or(100.0, |v| ((v >> 16) & 0xFF) as f32),
            };
            list.push(IntelCore { mask: t.mask, name, tjmax });
        }

        // RAPL: jednostka energii i liczniki, które da się odczytać.
        let mut unit = 0.0;
        let mut energy = Vec::new();
        if matches!(arch, Arch::Modern | Arch::Silvermont) {
            let now = Instant::now();
            let raw = on_cpu(first.mask, || {
                let u = read_msr(io, MSR_RAPL_POWER_UNIT);
                let counters: Vec<Option<u64>> = RAPL.iter().map(|(m, _)| read_msr(io, *m)).collect();
                (u, counters)
            });
            if let Some((Some(u), counters)) = raw {
                let esu = ((u >> 8) & 0x1F) as u32;
                unit = if arch == Arch::Silvermont {
                    1.0e-6 * (1u64 << esu) as f64
                } else {
                    1.0 / (1u64 << esu) as f64
                };
                for (i, c) in counters.into_iter().enumerate() {
                    // "CPU Graphics" LHM pomija na Windows.
                    if i == 2 {
                        continue;
                    }
                    if let Some(c) = c {
                        energy.push((i, Energy { last: c as u32, at: now }));
                    }
                }
            }
        }

        Some(Self {
            index,
            hw: hw.to_owned(),
            mask: first.mask,
            tjmax_pkg: list.first().map_or(100.0, |c| c.tjmax),
            cores: list,
            dts,
            ptm,
            unit,
            energy,
        })
    }

    fn read(&mut self, io: &PawnIo, out: &mut Readings) {
        let id = |i: usize| format!("/intelcpu/{}/temperature/{i}", self.index);
        // Odległość od TjMax (bity 22:16), gdy odczyt ważny (bit 31).
        let delta = |v: u64| (v >> 31 & 1 == 1).then_some(((v >> 16) & 0x7F) as f32);

        let deltas: Vec<Option<f32>> = if self.dts {
            self.cores
                .iter()
                .map(|c| on_cpu(c.mask, || read_msr(io, IA32_THERM_STATUS)).flatten().and_then(delta))
                .collect()
        } else {
            Vec::new()
        };
        let temps: Vec<Option<f32>> =
            deltas.iter().zip(&self.cores).map(|(d, c)| d.map(|d| c.tjmax - d)).collect();

        let mut n = 0;
        if self.ptm && self.cores.len() > 1 {
            let valid: Vec<f32> = temps.iter().flatten().copied().collect();
            let max = valid.iter().copied().reduce(f32::max);
            // Jak w LHM: suma ważnych przez liczbę wszystkich rdzeni.
            let avg = max.map(|_| valid.iter().sum::<f32>() / temps.len().max(1) as f32);
            out.temp(id(0), "Core Max", &self.hw, KIND, max.and_then(valid_temp));
            out.temp(id(1), "Core Average", &self.hw, KIND, avg.and_then(valid_temp));
            n = 2;
        }
        for (c, t) in self.cores.iter().zip(&temps) {
            out.temp(id(n), &c.name, &self.hw, KIND, t.and_then(valid_temp));
            n += 1;
        }
        if self.ptm {
            let pkg = on_cpu(self.mask, || read_msr(io, IA32_PACKAGE_THERM_STATUS))
                .flatten()
                .and_then(delta)
                .map(|d| self.tjmax_pkg - d);
            out.temp(id(n), "CPU Package", &self.hw, KIND, pkg.and_then(valid_temp));
            n += 1;
        }
        for (c, d) in self.cores.iter().zip(&deltas) {
            let name = format!("{} Distance to TjMax", c.name);
            out.temp(id(n), &name, &self.hw, KIND, d.and_then(valid_temp));
            n += 1;
        }

        if !self.energy.is_empty() {
            let now = Instant::now();
            let msrs: Vec<u32> = self.energy.iter().map(|(i, _)| RAPL[*i].0).collect();
            let raw = on_cpu(self.mask, || msrs.iter().map(|m| read_msr(io, *m)).collect::<Vec<_>>())
                .unwrap_or_default();
            for (k, (i, e)) in self.energy.iter_mut().enumerate() {
                let w = raw.get(k).copied().flatten().and_then(|v| e.update(v as u32, self.unit, now));
                out.power(RAPL[*i].1, &self.hw, KIND, w);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AMD Zen
// ---------------------------------------------------------------------------

const MSR_PWR_UNIT: u32 = 0xC001_0299;
const MSR_CORE_ENERGY_STAT: u32 = 0xC001_029A;
const MSR_PKG_ENERGY_STAT: u32 = 0xC001_029B;
const THM_TCON_CUR_TMP: u32 = 0x0005_9800;
const F17H_M70H_CCD1_TEMP: u32 = 0x0005_9954;
const F17H_M61H_CCD1_TEMP: u32 = 0x0005_9B08;
const F1AH_M00H_CCD1_TEMP: u32 = 0x0005_99F0;
const CCD_TEMP_VALID: u32 = 0x800;
const TEMP_RANGE_SEL_MASK: u32 = 0x8_0000;
const TEMP_TJ_SEL_MASK: u32 = 0x3_0000;

/// Czujniki z tabeli PM (wersja tabeli -> (indeks, nazwa, temperatura?)),
/// w kolejności LHM (od niej zależą numery identyfikatorów).
fn pm_sensors(version: u32) -> &'static [(usize, &'static str, bool)] {
    match version {
        0x001E_0004 => &[
            (67, "SoC", false),
            (108, "Core #1", true),
            (109, "Core #2", true),
            (110, "Core #3", true),
            (111, "Core #4", true),
            (151, "GFX", true),
        ],
        0x0024_0903 => &[(115, "SoC", true)],
        0x0038_0805 => &[(127, "SoC", true)],
        0x0054_0004 => &[
            (3, "CPU PPT", false),
            (11, "Package", true),
            (20, "Core Power", false),
            (21, "SOC Power", false),
            (22, "Misc Power", false),
            (26, "Total Power", false),
            (211, "IOD Hotspot", true),
            (539, "L3 (CCD1)", true),
            (540, "L3 (CCD2)", true),
        ],
        0x0062_1202 => &[
            (0, "CPU PPT Limit", false),
            (1, "CPU PPT", false),
            (2, "PPT Fast Limit", false),
            (3, "PPT Slow", false),
            (26, "PPT Fast", false),
            (10, "CPU Temperature Limit", true),
            (11, "Package", true),
            (595, "L3 (CCD1)", true),
            (596, "L3 (CCD2)", true),
            (52, "VDDCR VDD", true),
            (57, "VDDCR SoC", true),
            (62, "VDD Misc", true),
            (21, "SoC Power", false),
        ],
        _ => &[],
    }
}

/// Indeks temperatur rdzeni w tabeli PM (tylko Granite Ridge 0x00621202).
fn pm_core_temp_base(version: u32) -> Option<usize> {
    (version == 0x0062_1202).then_some(333)
}

struct AmdCore {
    mask: usize,
    /// Numer rdzenia od 1 (jak LHM: kolejne CoreId z CPUID 8000001E).
    number: usize,
    temp_index: usize,
    energy: Option<Energy>,
}

/// Tabela PM przez `RyzenSMU.bin`.
struct Smu {
    io: PawnIo,
    version: u32,
}

impl Smu {
    fn open(module: &Path) -> Option<Self> {
        let io = PawnIo::open(module).ok()?;
        let _lock = NamedLock::acquire(PCI_LOCK, 2000).ok()?;
        let mut info = [0u64; 2];
        io.call(b"ioctl_resolve_pm_table\0", &[], &mut info).ok()?;
        Some(Self { io, version: info[0] as u32 })
    }

    /// Tabela PM (co najmniej `len` liczb) - dwie próby, jak w LHM.
    fn table(&self, len: usize) -> Option<Vec<f32>> {
        // Moduł oddaje najwyżej jedną stronę (512 komórek po 8 bajtów).
        let cells = len.div_ceil(2).clamp(1, 512);
        let mut buf = vec![0u64; cells];
        for _ in 0..2 {
            let _lock = NamedLock::acquire(PCI_LOCK, 50).ok()?;
            if self.io.call(b"ioctl_update_pm_table\0", &[], &mut []).is_err() {
                continue;
            }
            if self.io.call(b"ioctl_read_pm_table\0", &[], &mut buf).is_err() {
                continue;
            }
            let table: Vec<f32> = buf
                .iter()
                .flat_map(|c| [f32::from_bits(*c as u32), f32::from_bits((*c >> 32) as u32)])
                .collect();
            if table.first().is_some_and(|v| *v != 0.0) {
                return Some(table);
            }
        }
        None
    }
}

struct Amd {
    io: PawnIo,
    smu: Option<Smu>,
    hw: String,
    family: u32,
    model: u32,
    /// Przesunięcie Tdie względem Tctl (tabela k10temp, ujemne albo 0).
    offset: f32,
    mask: usize,
    cores: Vec<AmdCore>,
    pkg_energy: Option<Energy>,
    /// Czujniki tabeli PM: (indeks w tabeli, nazwa, numer temperatury albo None dla mocy).
    pm: Vec<(usize, &'static str, Option<usize>)>,
    pm_len: usize,
    /// Numery CCD nadane przy pierwszym wykryciu (jak w LHM).
    ccd_index: [Option<usize>; 16],
    ccd_max_index: Option<usize>,
    ccd_avg_index: Option<usize>,
    next_index: usize,
    /// Ostatnie odczyty SMN (gdy blokada PCI jest zajęta, zostają stare wartości).
    smn_temps: Vec<(usize, String, Option<f32>)>,
}

impl Amd {
    fn open(dir: &Path) -> Option<Self> {
        let io = PawnIo::open(&dir.join("AMDFamily17.bin")).ok()?;
        let pkgs = packages(threads(), false);
        // Rdzenie pierwszego pakietu wg CoreId z 8000001E EBX (sortowanie stabilne).
        let mut threads: Vec<Thread> = pkgs.into_iter().next()?.into_iter().flatten().collect();
        threads.sort_by_key(|t| t.ext1e[1] & 0xFF);
        let first = threads.first()?.clone();

        let smu = Smu::open(&dir.join("RyzenSMU.bin"));
        let version = smu.as_ref().map_or(0, |s| s.version);
        // Numery temperatur: 0..2 Tctl/Tdie, potem tabela PM, rdzenie, CCD.
        let mut next = 3;
        let mut pm = Vec::new();
        let mut pm_len = 0;
        for &(idx, name, temp) in pm_sensors(version) {
            let n = temp.then(|| {
                next += 1;
                next - 1
            });
            pm.push((idx, name, n));
            pm_len = pm_len.max(idx + 1);
        }

        let mut cores = Vec::new();
        let mut last_id = None;
        for t in &threads {
            let id = t.ext1e[1] & 0xFF;
            if last_id != Some(id) {
                cores.push(AmdCore { mask: t.mask, number: cores.len() + 1, temp_index: next, energy: None });
                next += 1;
            }
            last_id = Some(id);
        }
        if let Some(base) = pm_core_temp_base(version) {
            pm_len = pm_len.max(base + cores.len());
        }

        let name = lhm_name(&brand());
        let offset = if name.contains("1600X") || name.contains("1700X") || name.contains("1800X") {
            -20.0
        } else if name.contains("Threadripper 19") || name.contains("Threadripper 29") {
            -27.0
        } else if name.contains("2700X") {
            -10.0
        } else {
            0.0
        };

        let mut amd = Self {
            io,
            smu,
            hw: hw_name(&name),
            family: first.family(),
            model: first.model(),
            offset,
            mask: first.mask,
            cores,
            pkg_energy: None,
            pm,
            pm_len,
            ccd_index: [None; 16],
            ccd_max_index: None,
            ccd_avg_index: None,
            next_index: next,
            smn_temps: Vec::new(),
        };
        // Pierwszy odczyt liczników energii (moc liczona od następnego).
        amd.read(&mut Readings::default());
        Some(amd)
    }

    fn read_smn(&self, address: u32) -> Option<u32> {
        let mut out = [0u64; 1];
        self.io.call(b"ioctl_read_smn\0", &[address as u64], &mut out).ok()?;
        Some(out[0] as u32)
    }

    /// Temperatury z SMN (pod blokadą PCI trzymaną przez wołającego).
    fn smn(&mut self) -> Vec<(usize, String, Option<f32>)> {
        let mut out = Vec::new();
        if let Some(raw) = self.read_smn(THM_TCON_CUR_TMP) {
            let adjust = raw & TEMP_RANGE_SEL_MASK != 0 || raw & TEMP_TJ_SEL_MASK == TEMP_TJ_SEL_MASK;
            let mut t = ((raw >> 21) * 125) as f32 * 0.001;
            if adjust {
                t -= 49.0;
            }
            if self.offset < 0.0 {
                out.push((0, "Core (Tctl)".to_owned(), Some(t)));
                out.push((1, "Core (Tdie)".to_owned(), Some(t + self.offset)));
            } else {
                out.push((2, "Core (Tctl/Tdie)".to_owned(), Some(t)));
            }
        }

        let turin = self.family == 0x1A && self.model <= 0x2F;
        let per_ccd = turin || matches!(self.model, 0x31 | 0x71 | 0x21 | 0x61 | 0x44);
        if per_ccd {
            let (base, count) = if turin {
                (F1AH_M00H_CCD1_TEMP, 16)
            } else if matches!(self.model, 0x61 | 0x44) {
                (F17H_M61H_CCD1_TEMP, 8)
            } else {
                (F17H_M70H_CCD1_TEMP, 8)
            };
            let mut values = Vec::new();
            for i in 0..count {
                let Some(raw) = self.read_smn(base + i as u32 * 4) else { continue };
                let present = if turin { raw & CCD_TEMP_VALID != 0 } else { raw & 0xFFF > 0 };
                let t = ((raw & 0xFFF) as i64 * 125 - 305_000) as f32 * 0.001;
                // Wartość ujemna w LHM zawija się (uint) i jest odrzucana.
                if present && (0.0..125.0).contains(&t) {
                    let index = *self.ccd_index[i].get_or_insert_with(|| {
                        self.next_index += 1;
                        self.next_index - 1
                    });
                    out.push((index, format!("CCD{} (Tdie)", i + 1), Some(t)));
                    values.push(t);
                }
            }
            if self.ccd_index.iter().flatten().count() > 1 && !values.is_empty() {
                let max = values.iter().copied().fold(f32::MIN, f32::max);
                let avg = values.iter().sum::<f32>() / values.len() as f32;
                let next = &mut self.next_index;
                let mi = *self.ccd_max_index.get_or_insert_with(|| {
                    *next += 1;
                    *next - 1
                });
                let ai = *self.ccd_avg_index.get_or_insert_with(|| {
                    *next += 1;
                    *next - 1
                });
                out.push((mi, "CCDs Max (Tdie)".to_owned(), Some(max)));
                out.push((ai, "CCDs Average (Tdie)".to_owned(), Some(avg)));
            }
        }
        out
    }

    fn read(&mut self, out: &mut Readings) {
        if let Ok(_lock) = NamedLock::acquire(PCI_LOCK, 50) {
            self.smn_temps = self.smn();
        }
        let now = Instant::now();
        let io = &self.io;
        let pkg = on_cpu(self.mask, || (read_msr(io, MSR_PWR_UNIT), read_msr(io, MSR_PKG_ENERGY_STAT)));
        let unit = pkg.and_then(|p| p.0).map(|u| 0.5f64.powi(((u >> 8) & 0x1F) as i32));
        let table = match &self.smu {
            Some(smu) if self.pm_len > 0 => smu.table(self.pm_len),
            _ => None,
        };

        let mut temps: Vec<(usize, String, Option<f32>)> = self.smn_temps.clone();
        let mut powers: Vec<(String, Option<f32>)> = Vec::new();

        // Moc pakietu.
        let pkg_w = match (pkg.and_then(|p| p.1), unit) {
            (Some(raw), Some(unit)) => match self.pkg_energy.as_mut() {
                Some(e) => e.update(raw as u32, unit, now),
                None => {
                    self.pkg_energy = Some(Energy { last: raw as u32, at: now });
                    None
                }
            },
            _ => None,
        };
        powers.push(("Package".to_owned(), pkg_w));

        // Czujniki tabeli PM (LHM pokazuje je, gdy wartość różna od zera).
        if let Some(t) = &table {
            for &(idx, name, temp) in &self.pm {
                let Some(v) = t.get(idx).copied().filter(|v| *v != 0.0 && v.is_finite()) else { continue };
                match temp {
                    Some(n) => temps.push((n, name.to_owned(), Some(v))),
                    None => powers.push((name.to_owned(), Some(v))),
                }
            }
        }

        // Rdzenie: moc z licznika energii rdzenia, temperatura z tabeli PM.
        let base = self.smu.as_ref().and_then(|s| pm_core_temp_base(s.version));
        for c in &mut self.cores {
            let raw = on_cpu(c.mask, || read_msr(io, MSR_CORE_ENERGY_STAT)).flatten();
            let w = match (raw, unit) {
                (Some(raw), Some(unit)) => match c.energy.as_mut() {
                    Some(e) => e.update(raw as u32, unit, now),
                    None => {
                        c.energy = Some(Energy { last: raw as u32, at: now });
                        None
                    }
                },
                _ => None,
            };
            powers.push((format!("Core #{} (SMU)", c.number), w));
            if let (Some(base), Some(t)) = (base, &table) {
                if let Some(v) = t.get(base + c.number - 1).copied().filter(|v| *v > 0.0 && *v < 125.0) {
                    temps.push((c.temp_index, format!("Core #{}", c.number), Some(v)));
                }
            }
        }

        temps.sort_by_key(|t| t.0);
        for (i, name, v) in temps {
            out.temp(format!("/amdcpu/0/temperature/{i}"), &name, &self.hw, KIND, v.and_then(valid_temp));
        }
        for (name, v) in powers {
            out.power(&name, &self.hw, KIND, v.filter(|w| w.is_finite() && *w >= 0.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lhm_names() {
        assert_eq!(lhm_name("Intel(R) Core(TM) i7-13850HX"), "Intel Core i7-13850HX");
        assert_eq!(lhm_name("Genuine Intel(R) 0000"), "Genuine Intel 0000");
        assert_eq!(lhm_name("AMD Ryzen 7 5800X 8-Core Processor"), "AMD Ryzen 7 5800X");
        assert_eq!(lhm_name("Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz"), "Intel Core i7-8700K");
    }

    #[test]
    fn log2() {
        assert_eq!(next_log2(1), 0);
        assert_eq!(next_log2(2), 1);
        assert_eq!(next_log2(64), 6);
        assert_eq!(next_log2(65), 7);
    }

    #[test]
    fn energy_wraps() {
        let t0 = Instant::now();
        let mut e = Energy { last: u32::MAX - 9, at: t0 };
        let w = e.update(10, 1.0, t0 + std::time::Duration::from_secs(2)).unwrap();
        assert!((w - 10.0).abs() < 1e-3);
    }
}

#[cfg(test)]
mod hardware {
    //! Tylko odczyt, wymaga administratora i PawnIO:
    //! `cargo test --release hw_cpu -- --ignored --nocapture`
    use super::*;

    #[test]
    #[ignore]
    fn hw_cpu() {
        let mut cpu = Cpu::open_in(Path::new("D:/tune-center-main/dist/modules")).expect("Cpu::open");
        for _ in 0..3 {
            let mut r = Readings::default();
            cpu.read(&mut r);
            for t in &r.temps {
                println!("{} | {} | {} | {} | {:?}", t.id, t.name, t.hw, t.kind, t.value);
            }
            for p in &r.powers {
                println!("P {} | {} | {} | {:?}", p.name, p.hw, p.kind, p.value);
            }
            println!("---");
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
    }
}
