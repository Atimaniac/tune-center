//! Minimalne wiązanie do NVAPI (nvapi64.dll), łącznie z funkcjami
//! nieudokumentowanymi (krzywa V/F, zapis P-states).
//!
//! Identyfikatory funkcji i układy struktur pochodzą z publicznego nvapi.h
//! oraz z otwartych projektów, które odtworzyły część prywatną (nvapi-rs,
//! ccminer). Sterownik sprawdza pole `version` (rozmiar struktury + numer
//! wersji), więc niezgodna struktura kończy się błędem, a nie zapisem śmieci.

use super::backend::*;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};

type Status = i32;
type Handle = *mut c_void;
type QueryFn = unsafe extern "C" fn(u32) -> *const c_void;
type FnVoid = unsafe extern "C" fn() -> Status;
type FnEnum = unsafe extern "C" fn(*mut Handle, *mut u32) -> Status;
type FnName = unsafe extern "C" fn(Handle, *mut u8) -> Status;
type FnThermal = unsafe extern "C" fn(Handle, u32, *mut Thermal) -> Status;
type FnStruct<T> = unsafe extern "C" fn(Handle, *mut T) -> Status;

const ID_INITIALIZE: u32 = 0x0150_E828;
const ID_UNLOAD: u32 = 0xD22B_DD7E;
const ID_ENUM_PHYSICAL_GPUS: u32 = 0xE5AC_921F;
const ID_GET_FULL_NAME: u32 = 0xCEEE_8E9F;
const ID_GET_THERMAL_SETTINGS: u32 = 0xE364_0A56;
const ID_GET_ALL_CLOCK_FREQUENCIES: u32 = 0xDCB6_16C3;
const ID_GET_DYNAMIC_PSTATES_INFO_EX: u32 = 0x60DE_D2ED;
const ID_GET_CURRENT_VOLTAGE: u32 = 0x465F_9BCF;
const ID_GET_PSTATES20: u32 = 0x6FF8_1213;
const ID_SET_PSTATES20: u32 = 0x0F4D_AE6B;
const ID_GET_CLOCK_BOOST_MASK: u32 = 0x507B_4B59;
const ID_GET_CLOCK_BOOST_TABLE: u32 = 0x23F1_B133;
const ID_SET_CLOCK_BOOST_TABLE: u32 = 0x0733_E009;
const ID_GET_VFP_CURVE: u32 = 0x2153_7AD4;

const CLOCK_DOMAIN_GRAPHICS: u32 = 0;
const CLOCK_DOMAIN_MEMORY: u32 = 4;
const THERMAL_TARGET_ALL: u32 = 15;

const fn ver<T>(v: u32) -> u32 {
    (size_of::<T>() as u32) | (v << 16)
}

// ----------------------------- struktury -----------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct ThermalSensor {
    controller: i32,
    default_min: i32,
    default_max: i32,
    current: i32,
    target: i32,
}

#[repr(C)]
struct Thermal {
    version: u32,
    count: u32,
    sensors: [ThermalSensor; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ClockDomain {
    present: u32,
    freq_khz: u32,
}

#[repr(C)]
struct ClockFreqs {
    version: u32,
    clock_type: u32, // 0 = bieżące zegary
    domains: [ClockDomain; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Utilization {
    present: u32,
    percent: u32,
}

#[repr(C)]
struct Usages {
    version: u32,
    flags: u32,
    utilization: [Utilization; 8],
}

#[repr(C)]
struct VoltageStatus {
    version: u32,
    flags: u32,
    zero: [u32; 8],
    value_uv: u32,
    unknown: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ParamDelta {
    value: i32,
    min: i32,
    max: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PstateClock {
    domain_id: u32,
    type_id: u32,
    editable: u32,
    freq_delta: ParamDelta,
    data: [u32; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PstateVoltage {
    domain_id: u32,
    editable: u32,
    volt_uv: u32,
    delta: ParamDelta,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Pstate {
    id: u32,
    editable: u32,
    clocks: [PstateClock; 8],
    voltages: [PstateVoltage; 4],
}

#[repr(C)]
struct Pstates20 {
    version: u32,
    flags: u32,
    num_pstates: u32,
    num_clocks: u32,
    num_base_voltages: u32,
    pstates: [Pstate; 16],
    ov_num: u32,
    ov: [PstateVoltage; 4],
}

/// Maska punktów krzywej; używamy tylko pola `mask`.
#[repr(C)]
struct ClockMasks {
    version: u32,
    mask: [u32; 4],
    rest: [u32; 1542],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ClockTableEntry {
    unknown0: [u32; 5],
    freq_delta_khz: i32,
    unknown1: [u32; 3],
}

#[repr(C)]
struct ClockTable {
    version: u32,
    mask: [u32; 4],
    unknown: [u32; 12],
    entries: [ClockTableEntry; 255],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VfpEntry {
    unknown0: u32,
    freq_khz: u32,
    volt_uv: u32,
    unknown1: [u32; 4],
}

#[repr(C)]
struct VfpCurve {
    version: u32,
    mask: [u32; 4],
    unknown: [u32; 12],
    entries: [VfpEntry; 255],
}

// Rozmiary muszą zgadzać się co do bajta z tym, czego oczekuje sterownik.
const _: () = assert!(size_of::<Thermal>() == 0x44);
const _: () = assert!(size_of::<ClockFreqs>() == 0x108);
const _: () = assert!(size_of::<Usages>() == 0x48);
const _: () = assert!(size_of::<VoltageStatus>() == 0x4C);
const _: () = assert!(size_of::<Pstates20>() == 0x1CF8);
const _: () = assert!(size_of::<ClockMasks>() == 0x182C);
const _: () = assert!(size_of::<ClockTable>() == 0x2420);
const _: () = assert!(size_of::<VfpCurve>() == 0x1C28);

const PSTATES_VERSIONS: [u32; 3] = [0x3_1CF8, 0x2_1CF8, 0x1_1C94];

// ------------------------------- API ---------------------------------------

pub struct NvApi {
    _lib: crate::dll::Library,
    handles: Vec<Handle>,
    names: Vec<String>,
    pstates_version: u32,
    unload: Option<FnVoid>,
    get_thermal: Option<FnThermal>,
    /// Oficjalna biblioteka monitorowania Nvidii: pobór mocy, wentylatory
    /// (odczyt i sterowanie) i temperatura pamięci.
    nvml: Option<super::nvml::Nvml>,
    /// Szczegółowe odczyty (PCIe TX/RX i moc układu z NVML) - patrz `set_detail`.
    detail: bool,
    get_clocks: Option<FnStruct<ClockFreqs>>,
    get_usages: Option<FnStruct<Usages>>,
    get_voltage: Option<FnStruct<VoltageStatus>>,
    get_pstates: Option<FnStruct<Pstates20>>,
    set_pstates: Option<FnStruct<Pstates20>>,
    get_mask: Option<FnStruct<ClockMasks>>,
    get_table: Option<FnStruct<ClockTable>>,
    set_table: Option<FnStruct<ClockTable>>,
    get_vfp: Option<FnStruct<VfpCurve>>,
}

unsafe fn q<T: Copy>(query: QueryFn, id: u32) -> Option<T> {
    let p = query(id);
    if p.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy::<*const c_void, T>(&p))
    }
}

fn status_text(s: Status) -> &'static str {
    match s {
        -1 => crate::i18n::tr("błąd ogólny", "general error"),
        -3 => crate::i18n::tr("funkcja niezaimplementowana", "function not implemented"),
        -4 => crate::i18n::tr("API niezainicjalizowane", "API not initialized"),
        -5 => crate::i18n::tr("nieprawidłowy argument", "invalid argument"),
        -6 => crate::i18n::tr("nie znaleziono karty Nvidia", "no Nvidia card found"),
        -9 => crate::i18n::tr(
            "niezgodna wersja struktury",
            "incompatible structure version",
        ),
        -104 => crate::i18n::tr(
            "funkcja nieobsługiwana przez tę kartę",
            "function not supported by this card",
        ),
        -137 => crate::i18n::tr(
            "brak uprawnień (uruchom jako administrator)",
            "insufficient privileges (run as administrator)",
        ),
        _ => crate::i18n::tr("nieznany błąd", "unknown error"),
    }
}

fn check(name: &str, s: Status) -> Result<()> {
    if s == 0 {
        Ok(())
    } else {
        Err(crate::trf!(
            "{name}: {} (kod {s})",
            "{name}: {} (code {s})",
            status_text(s)
        ))
    }
}

fn call<T>(name: &str, f: Option<FnStruct<T>>, h: Handle, data: &mut T) -> Result<()> {
    let f = f.ok_or_else(|| {
        crate::trf!(
            "{name}: funkcja niedostępna w tym sterowniku",
            "{name}: function not available in this driver"
        )
    })?;
    check(name, unsafe { f(h, data) })
}

fn mask_bit(mask: &[u32; 4], i: usize) -> bool {
    i < 128 && (mask[i / 32] >> (i % 32)) & 1 == 1
}

impl NvApi {
    pub fn load() -> Result<Self> {
        unsafe {
            let lib = crate::dll::Library::new("nvapi64.dll").map_err(|e| {
                crate::trf!(
                    "Nie można załadować nvapi64.dll (brak sterownika Nvidia?): {e}",
                    "Cannot load nvapi64.dll (Nvidia driver missing?): {e}"
                )
            })?;
            let query: QueryFn = *lib.get::<QueryFn>(b"nvapi_QueryInterface\0").map_err(|e| {
                crate::trf!(
                    "Brak nvapi_QueryInterface: {e}",
                    "nvapi_QueryInterface is missing: {e}"
                )
            })?;

            let init: FnVoid = q(query, ID_INITIALIZE).ok_or_else(|| {
                crate::i18n::tr("Brak NvAPI_Initialize", "NvAPI_Initialize is missing").to_string()
            })?;
            check("NvAPI_Initialize", init())?;

            let enum_gpus: FnEnum = q(query, ID_ENUM_PHYSICAL_GPUS).ok_or_else(|| {
                crate::i18n::tr(
                    "Brak NvAPI_EnumPhysicalGPUs",
                    "NvAPI_EnumPhysicalGPUs is missing",
                )
                .to_string()
            })?;
            let mut raw: [Handle; 64] = [std::ptr::null_mut(); 64];
            let mut count = 0u32;
            check(
                "NvAPI_EnumPhysicalGPUs",
                enum_gpus(raw.as_mut_ptr(), &mut count),
            )?;
            let handles: Vec<Handle> = raw[..(count as usize).min(64)].to_vec();
            if handles.is_empty() {
                return Err(crate::i18n::tr(
                    "Nie znaleziono żadnej karty Nvidia",
                    "No Nvidia card found",
                )
                .to_string());
            }

            let get_name: Option<FnName> = q(query, ID_GET_FULL_NAME);
            let names = handles
                .iter()
                .enumerate()
                .map(|(i, &h)| {
                    let mut buf = [0u8; 64];
                    match get_name {
                        Some(f) if f(h, buf.as_mut_ptr()) == 0 => {
                            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                            String::from_utf8_lossy(&buf[..end]).into_owned()
                        }
                        _ => format!("GPU {i}"),
                    }
                })
                .collect();

            Ok(Self {
                handles,
                names,
                pstates_version: 0,
                unload: q(query, ID_UNLOAD),
                get_thermal: q(query, ID_GET_THERMAL_SETTINGS),
                nvml: super::nvml::Nvml::load(),
                detail: true,
                get_clocks: q(query, ID_GET_ALL_CLOCK_FREQUENCIES),
                get_usages: q(query, ID_GET_DYNAMIC_PSTATES_INFO_EX),
                get_voltage: q(query, ID_GET_CURRENT_VOLTAGE),
                get_pstates: q(query, ID_GET_PSTATES20),
                set_pstates: q(query, ID_SET_PSTATES20),
                get_mask: q(query, ID_GET_CLOCK_BOOST_MASK),
                get_table: q(query, ID_GET_CLOCK_BOOST_TABLE),
                set_table: q(query, ID_SET_CLOCK_BOOST_TABLE),
                get_vfp: q(query, ID_GET_VFP_CURVE),
                _lib: lib,
            })
        }
    }

    fn handle(&self, gpu: usize) -> Result<Handle> {
        self.handles.get(gpu).copied().ok_or_else(|| {
            crate::i18n::tr("Nieprawidłowy indeks karty", "Invalid card index").to_string()
        })
    }

    fn read_pstates(&mut self, h: Handle) -> Result<Box<Pstates20>> {
        let versions: Vec<u32> = if self.pstates_version != 0 {
            vec![self.pstates_version]
        } else {
            PSTATES_VERSIONS.to_vec()
        };
        let mut last = String::new();
        for v in versions {
            let mut p: Box<Pstates20> = Box::new(unsafe { zeroed() });
            p.version = v;
            match call("NvAPI_GPU_GetPstates20", self.get_pstates, h, &mut *p) {
                Ok(()) => {
                    self.pstates_version = v;
                    return Ok(p);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn read_mask(&self, h: Handle) -> Result<[u32; 4]> {
        let mut m: Box<ClockMasks> = Box::new(unsafe { zeroed() });
        m.version = ver::<ClockMasks>(1);
        call("NvAPI_GPU_GetClockBoostMask", self.get_mask, h, &mut *m)?;
        Ok(m.mask)
    }

    fn read_table(&self, h: Handle, mask: [u32; 4]) -> Result<Box<ClockTable>> {
        let mut t: Box<ClockTable> = Box::new(unsafe { zeroed() });
        t.version = ver::<ClockTable>(1);
        t.mask = mask;
        call("NvAPI_GPU_GetClockBoostTable", self.get_table, h, &mut *t)?;
        Ok(t)
    }
}

impl Drop for NvApi {
    fn drop(&mut self) {
        if let Some(f) = self.unload {
            unsafe {
                f();
            }
        }
    }
}

impl Backend for NvApi {
    fn gpus(&self) -> Vec<String> {
        self.names.clone()
    }

    fn set_detail(&mut self, on: bool) {
        self.detail = on;
    }

    /// Tylko jeden odczyt NVAPI (GetThermalSettings) zamiast pełnego `monitor`.
    fn temperature(&mut self, gpu: usize) -> Option<i32> {
        let h = self.handle(gpu).ok()?;
        let f = self.get_thermal?;
        let mut t: Thermal = unsafe { zeroed() };
        t.version = ver::<Thermal>(2);
        (unsafe { f(h, THERMAL_TARGET_ALL, &mut t) } == 0 && t.count > 0)
            .then(|| t.sensors[0].current)
    }

    fn monitor(&mut self, gpu: usize) -> Monitor {
        let mut m = Monitor::default();
        let Ok(h) = self.handle(gpu) else { return m };

        if let Some(f) = self.get_thermal {
            let mut t: Thermal = unsafe { zeroed() };
            t.version = ver::<Thermal>(2);
            if unsafe { f(h, THERMAL_TARGET_ALL, &mut t) } == 0 && t.count > 0 {
                m.temp_c = Some(t.sensors[0].current);
            }
        }

        // Temperatura pamięci - NVML (hotspotu oficjalne API Nvidii nie podają).
        if let Some(nvml) = &self.nvml {
            let name = self.names.get(gpu).map(String::as_str).unwrap_or("");
            let r = nvml.read(gpu, name, self.detail);
            m.power_w = r.power_w;
            m.fan_pct = r.fan_pct;
            let d = &mut m.details;
            d.mem_temp_c = r.mem_temp_c.or(d.mem_temp_c);
            d.mem_used_mb = r.mem_used_mb;
            d.mem_total_mb = r.mem_total_mb;
            d.pcie_tx_mbs = r.pcie_tx_mbs;
            d.pcie_rx_mbs = r.pcie_rx_mbs;
            d.pcie_gen = r.pcie_gen;
            d.pcie_width = r.pcie_width;
            d.mem_ctrl_pct = r.mem_ctrl_pct;
            d.encoder_pct = r.encoder_pct;
            d.decoder_pct = r.decoder_pct;
            d.video_mhz = r.video_mhz;
            d.pstate = r.pstate;
            d.tdp_w = r.tdp_w;
            d.limit = r.limit_reasons.map(super::nvml::limit_text);
            if let (Some(w), Some(tdp)) = (r.power_w, r.tdp_w.filter(|&t| t > 0.0)) {
                d.power_pct_tdp = Some(w / tdp * 100.0);
            }
        }

        // Moc samego układu GPU (NVML, zakres GPU) - tylko w widoku Informacje.
        if let (Some(nvml), true) = (&self.nvml, self.detail) {
            let name = self.names.get(gpu).map(String::as_str).unwrap_or("");
            m.details.chip_power_w = nvml.chip_power(gpu, name);
        }
        if let Some(fan) = self.fan(gpu) {
            m.fan_rpm = fan.rpm;
            m.fan_pct = fan.pct.or(m.fan_pct);
        }

        let mut c: ClockFreqs = unsafe { zeroed() };
        c.version = ver::<ClockFreqs>(2);
        if call("clocks", self.get_clocks, h, &mut c).is_ok() {
            let read = |d: u32| {
                let e = c.domains[d as usize];
                (e.present & 1 == 1).then(|| e.freq_khz as f32 / 1000.0)
            };
            m.core_mhz = read(CLOCK_DOMAIN_GRAPHICS);
            m.mem_mhz = read(CLOCK_DOMAIN_MEMORY);
        }

        let mut v: VoltageStatus = unsafe { zeroed() };
        v.version = ver::<VoltageStatus>(1);
        if call("voltage", self.get_voltage, h, &mut v).is_ok() && v.value_uv > 0 {
            m.voltage_mv = Some(v.value_uv as f32 / 1000.0);
        }

        let mut u: Usages = unsafe { zeroed() };
        u.version = ver::<Usages>(1);
        if call("usages", self.get_usages, h, &mut u).is_ok() {
            // Domeny: 0 = GPU, 1 = pamięć (FB), 2 = wideo, 3 = magistrala.
            let usage = |i: usize| {
                let e = u.utilization[i];
                (e.present & 1 == 1).then_some(e.percent)
            };
            m.usage_pct = usage(0);
            m.details.mem_ctrl_pct = m.details.mem_ctrl_pct.or(usage(1));
            m.details.video_pct = usage(2);
            m.details.bus_pct = usage(3);
        }

        m
    }

    // Wentylatory przez oficjalne NVML (odczyt, ustawianie, tryb automatyczny).
    fn fan(&mut self, gpu: usize) -> Option<FanStatus> {
        let name = self.names.get(gpu)?;
        let (rpm, pct) = self.nvml.as_ref()?.fan_status(gpu, name)?;
        Some(FanStatus { rpm, pct })
    }

    fn fan_controllable(&mut self, gpu: usize) -> bool {
        let name = self.names.get(gpu).map(String::as_str).unwrap_or("");
        self.nvml.as_ref().is_some_and(|n| n.fan_controllable(gpu, name))
    }

    fn set_fan(&mut self, gpu: usize, pct: Option<u32>) -> Result<()> {
        let name = self.names.get(gpu).cloned().unwrap_or_default();
        match &self.nvml {
            Some(n) => n.set_fans(gpu, &name, pct),
            None => Err(crate::i18n::tr(
                "brak biblioteki NVML (nvml.dll) - sterowanie wentylatorami niedostępne",
                "NVML (nvml.dll) not available - fan control unavailable",
            )
            .to_string()),
        }
    }

    fn offsets(&mut self, gpu: usize) -> Result<Offsets> {
        let h = self.handle(gpu)?;
        let p = self.read_pstates(h)?;
        let mut out = Offsets::default();
        if p.num_pstates == 0 {
            return Ok(out);
        }
        let n = (p.num_clocks as usize).min(8);
        for clock in &p.pstates[0].clocks[..n] {
            if clock.editable & 1 == 0 {
                continue;
            }
            let info = OffsetInfo {
                value_khz: clock.freq_delta.value,
                min_khz: clock.freq_delta.min,
                max_khz: clock.freq_delta.max,
                default_khz: 0,
            };
            match clock.domain_id {
                CLOCK_DOMAIN_GRAPHICS => out.core = Some(info),
                CLOCK_DOMAIN_MEMORY => out.mem = Some(info),
                _ => {}
            }
        }
        Ok(out)
    }

    fn set_offset(&mut self, gpu: usize, domain: Domain, khz: i32) -> Result<()> {
        let h = self.handle(gpu)?;
        // Odczyt ustala wersję struktury akceptowaną przez sterownik.
        let _ = self.read_pstates(h)?;

        let mut p: Box<Pstates20> = Box::new(unsafe { zeroed() });
        p.version = self.pstates_version;
        p.num_pstates = 1;
        p.num_clocks = 1;
        p.pstates[0].id = 0;
        p.pstates[0].clocks[0].domain_id = match domain {
            Domain::Core => CLOCK_DOMAIN_GRAPHICS,
            Domain::Memory => CLOCK_DOMAIN_MEMORY,
        };
        p.pstates[0].clocks[0].freq_delta.value = khz;

        // Zapis Pstates20 zeruje krzywą V/F (zmierzone na RTX 5070: zmiana
        // offsetu pamięci wyzerowała wszystkie 63 punkty undervoltingu).
        // Krzywą odczytujemy przed zapisem i przywracamy, gdy sterownik ją zmienił.
        let saved = self
            .read_mask(h)
            .and_then(|mask| self.read_table(h, mask).map(|t| (mask, t)))
            .ok();
        call("NvAPI_GPU_SetPstates20", self.set_pstates, h, &mut *p)?;
        if let Some((mask, mut table)) = saved {
            let changed = self.read_table(h, mask).is_ok_and(|now| {
                (0..128).any(|i| {
                    mask_bit(&mask, i)
                        && now.entries[i].freq_delta_khz != table.entries[i].freq_delta_khz
                })
            });
            if changed {
                call(
                    "NvAPI_GPU_SetClockBoostTable",
                    self.set_table,
                    h,
                    &mut *table,
                )
                .map_err(|e| {
                    crate::trf!(
                        "offset zapisany, ale nie udało się przywrócić krzywej V/F: {}",
                        "offset written, but the V/F curve could not be restored: {}",
                        e
                    )
                })?;
            }
        }
        Ok(())
    }

    fn curve(&mut self, gpu: usize) -> Result<Vec<CurvePoint>> {
        let h = self.handle(gpu)?;
        let mask = self.read_mask(h)?;

        let mut vfp: Box<VfpCurve> = Box::new(unsafe { zeroed() });
        vfp.version = ver::<VfpCurve>(1);
        vfp.mask = mask;
        call("NvAPI_GPU_GetVFPCurve", self.get_vfp, h, &mut *vfp)?;

        let table = self.read_table(h, mask)?;

        // Punkty rdzenia mają rosnące napięcie. Wpisy pamięci (dalej w tej
        // samej tabeli) łamią tę monotoniczność, więc na nich kończymy.
        let mut points = Vec::new();
        let mut last_uv = 0u32;
        for i in 0..128 {
            if !mask_bit(&mask, i) {
                continue;
            }
            let e = vfp.entries[i];
            if e.volt_uv == 0 || e.freq_khz == 0 {
                continue;
            }
            if e.volt_uv <= last_uv {
                break;
            }
            last_uv = e.volt_uv;
            points.push(CurvePoint {
                index: i,
                volt_uv: e.volt_uv,
                freq_khz: e.freq_khz,
                delta_khz: table.entries[i].freq_delta_khz,
            });
        }
        if points.is_empty() {
            return Err(crate::i18n::tr(
                "Sterownik nie zwrócił punktów krzywej V/F",
                "The driver returned no V/F curve points",
            )
            .to_string());
        }
        Ok(points)
    }

    fn set_curve(&mut self, gpu: usize, deltas: &[(usize, i32)]) -> Result<()> {
        let h = self.handle(gpu)?;
        let mask = self.read_mask(h)?;
        let mut table = self.read_table(h, mask)?;
        for &(i, d) in deltas {
            if mask_bit(&mask, i) {
                table.entries[i].freq_delta_khz = d;
            }
        }
        call(
            "NvAPI_GPU_SetClockBoostTable",
            self.set_table,
            h,
            &mut *table,
        )
    }
}

#[cfg(test)]
mod hardware {
    //! Odczyt wszystkich czujników karty (tylko odczyt):
    //! `cargo test nvapi_monitor -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn nvapi_monitor() {
        let mut api = NvApi::load().expect("NVAPI");
        for _ in 0..2 {
            let t = std::time::Instant::now();
            let m = api.monitor(0);
            println!("czas odczytu {:?}", t.elapsed());
            if let Some(n) = &api.nvml {
                let t = std::time::Instant::now();
                n.read(0, "", true);
                println!("  w tym NVML {:?}", t.elapsed());
            }
            let d = &m.details;
            println!(
                "temp {:?} hotspot {:?} mem {:?} | core {:?} mem {:?} video {:?} MHz | {:?} mV",
                m.temp_c,
                m.hotspot_c,
                d.mem_temp_c,
                m.core_mhz,
                m.mem_mhz,
                d.video_mhz,
                m.voltage_mv
            );
            println!(
                "fan {:?} RPM {:?} % | VRAM {:?}/{:?} MB | PCIe TX {:?} RX {:?} MB/s gen {:?} x{:?}",
                m.fan_rpm, m.fan_pct, d.mem_used_mb, d.mem_total_mb, d.pcie_tx_mbs, d.pcie_rx_mbs,
                d.pcie_gen, d.pcie_width
            );
            println!(
                "load GPU {:?} MC {:?} VE {:?} BUS {:?} ENC {:?} DEC {:?} | power {:?} W chip {:?} W {:?} % TDP {:?} W | P{:?} limit {:?}",
                m.usage_pct, d.mem_ctrl_pct, d.video_pct, d.bus_pct, d.encoder_pct, d.decoder_pct,
                m.power_w, d.chip_power_w, d.power_pct_tdp, d.tdp_w, d.pstate, d.limit
            );
        }
    }
}

#[cfg(test)]
mod thermal_settings_probe {
    //! Wszystkie czujniki z publicznego NvAPI_GPU_GetThermalSettings:
    //! `cargo test nvapi_thermal_settings -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn nvapi_thermal_settings() {
        let api = NvApi::load().unwrap();
        let h = api.handle(0).unwrap();
        let f = api.get_thermal.unwrap();
        for index in [THERMAL_TARGET_ALL, 0, 1, 2] {
            let mut t: Thermal = unsafe { zeroed() };
            t.version = ver::<Thermal>(2);
            let st = unsafe { f(h, index, &mut t) };
            print!("indeks {index}: status {st}, liczba {}:", t.count);
            for s in &t.sensors[..(t.count as usize).min(3)] {
                print!(
                    "  [kontroler {} cel {} {} °C]",
                    s.controller, s.target, s.current
                );
            }
            println!();
        }
    }
}

#[cfg(test)]
mod interplay {
    //! Czy zapis pamięci zmienia krzywą i odwrotnie (zapis z przywróceniem):
    //! `cargo test nvapi_mem_curve_interplay --release -- --ignored --nocapture`
    use super::*;

    fn snapshot(api: &mut NvApi) -> (i32, i32, Vec<(usize, i32)>) {
        let o = api.offsets(0).expect("offsets");
        let c = api.curve(0).expect("curve");
        (
            o.core.map(|x| x.value_khz).unwrap_or(0),
            o.mem.map(|x| x.value_khz).unwrap_or(0),
            c.iter().map(|p| (p.index, p.delta_khz)).collect(),
        )
    }

    fn diff(a: &[(usize, i32)], b: &[(usize, i32)]) -> usize {
        a.iter().zip(b).filter(|(x, y)| x.1 != y.1).count()
    }

    #[test]
    #[ignore]
    fn nvapi_mem_curve_interplay() {
        let mut api = NvApi::load().expect("NVAPI");
        let wait = || std::thread::sleep(std::time::Duration::from_millis(500));
        let (core0, mem0, curve0) = snapshot(&mut api);
        println!(
            "start: rdzeń {} kHz, pamięć {} kHz, punkty z deltą≠0: {}",
            core0,
            mem0,
            curve0.iter().filter(|p| p.1 != 0).count()
        );

        // A: zmiana pamięci.
        api.set_offset(0, Domain::Memory, mem0 - 50_000).unwrap();
        wait();
        let (core1, mem1, curve1) = snapshot(&mut api);
        println!(
            "A pamięć -50 MHz: pamięć {mem1}, rdzeń {core1}, zmienione punkty krzywej: {}",
            diff(&curve0, &curve1)
        );
        assert!(
            curve0.iter().any(|p| p.1 != 0),
            "krzywa bez zmian - test nic nie wykaże"
        );
        assert_eq!(diff(&curve0, &curve1), 0, "zapis pamięci zmienił krzywą");
        api.set_offset(0, Domain::Memory, mem0).unwrap();
        wait();

        // B: zapis tej samej krzywej.
        api.set_curve(0, &curve0).unwrap();
        wait();
        let (core2, mem2, curve2) = snapshot(&mut api);
        println!(
            "B ta sama krzywa: pamięć {mem2}, rdzeń {core2}, zmienione punkty: {}",
            diff(&curve0, &curve2)
        );

        // C: zmiana jednego punktu krzywej (-15 MHz).
        let mut changed = curve0.clone();
        let k = changed.len() / 2;
        changed[k].1 -= 15_000;
        api.set_curve(0, &changed).unwrap();
        wait();
        let (core3, mem3, curve3) = snapshot(&mut api);
        println!(
            "C punkt {} -15 MHz: pamięć {mem3}, rdzeń {core3}, zmienione punkty: {}",
            changed[k].0,
            diff(&curve0, &curve3)
        );
        assert_eq!(mem3, mem0, "zapis krzywej zmienił pamięć");

        // Przywrócenie (najpierw pamięć, potem krzywa) i sprawdzenie.
        api.set_offset(0, Domain::Memory, mem0).unwrap();
        api.set_curve(0, &curve0).unwrap();
        wait();
        let (core9, mem9, curve9) = snapshot(&mut api);
        println!(
            "koniec: rdzeń {core9}, pamięć {mem9}, różne punkty od startu: {}",
            diff(&curve0, &curve9)
        );
        assert_eq!(mem9, mem0, "pamięć nie wróciła");
        assert_eq!(diff(&curve0, &curve9), 0, "krzywa nie wróciła");
    }
}

impl NvApi {
    /// Diagnostyka zgodności: funkcje NVAPI, których sterownik nie ma, i wersja
    /// struktury P-stanów (tylko odczyt).
    pub fn diagnostics(&self) -> Vec<String> {
        let mut out = vec![format!("pstates20 struct version 0x{:08X}", self.pstates_version)];
        let fns: [(&str, bool); 11] = [
            ("GetThermalSettings", self.get_thermal.is_some()),
            ("GetAllClockFrequencies", self.get_clocks.is_some()),
            ("GetDynamicPstatesInfoEx", self.get_usages.is_some()),
            ("GetCurrentVoltage", self.get_voltage.is_some()),
            ("GetPstates20", self.get_pstates.is_some()),
            ("SetPstates20", self.set_pstates.is_some()),
            (
                "ClockClientClkVfPointsGetInfo (mask)",
                self.get_mask.is_some(),
            ),
            ("ClockClientClkVfPointsGetControl", self.get_table.is_some()),
            ("ClockClientClkVfPointsSetControl", self.set_table.is_some()),
            ("ClockClientClkVfPointsGetStatus", self.get_vfp.is_some()),
            ("NVML (nvml.dll)", self.nvml.is_some()),
        ];
        let missing: Vec<&str> = fns.iter().filter(|f| !f.1).map(|f| f.0).collect();
        out.push(if missing.is_empty() {
            "all used NVAPI functions present".to_owned()
        } else {
            format!("missing: {}", missing.join(", "))
        });
        out
    }
}
