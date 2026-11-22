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
const ID_THERMAL_GET_SENSORS: u32 = 0x65FE_3AAD;
const ID_FAN_COOLERS_GET_STATUS: u32 = 0x35AE_D5E8;
const ID_FAN_COOLERS_GET_CONTROL: u32 = 0x814B_209F;
const ID_FAN_COOLERS_SET_CONTROL: u32 = 0xA589_71A5;
const ID_GET_ALL_CLOCK_FREQUENCIES: u32 = 0xDCB6_16C3;
const ID_GET_DYNAMIC_PSTATES_INFO_EX: u32 = 0x60DE_D2ED;
const ID_GET_CURRENT_VOLTAGE: u32 = 0x465F_9BCF;
const ID_GET_PSTATES20: u32 = 0x6FF8_1213;
const ID_SET_PSTATES20: u32 = 0x0F4D_AE6B;
const ID_GET_CLOCK_BOOST_MASK: u32 = 0x507B_4B59;
const ID_GET_CLOCK_BOOST_TABLE: u32 = 0x23F1_B133;
const ID_SET_CLOCK_BOOST_TABLE: u32 = 0x0733_E009;
const ID_GET_VFP_CURVE: u32 = 0x2153_7AD4;
const ID_POWER_TOPOLOGY_GET_STATUS: u32 = 0xEDCF_624E;

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

/// Rozszerzone czujniki temperatury (NvAPI_GPU_ThermalGetSensors).
/// Temperatury są w 1/256 °C; indeks 1 to hotspot.
#[repr(C)]
struct ThermalSensors {
    version: u32,
    mask: u32,
    reserved: [i32; 8],
    temperatures: [i32; 32],
}

/// Stan jednego wentylatora (NvAPI_GPU_ClientFanCoolersGetStatus).
#[repr(C)]
#[derive(Clone, Copy)]
struct FanStatusItem {
    cooler_id: u32,
    rpm: u32,
    min_level: u32,
    max_level: u32,
    level: u32,
    reserved: [u32; 8],
}

#[repr(C)]
struct FanCoolersStatus {
    version: u32,
    count: u32,
    reserved: [u64; 4],
    items: [FanStatusItem; 32],
}

/// Sterowanie jednym wentylatorem (NvAPI_GPU_ClientFanCoolersGet/SetControl).
#[repr(C)]
#[derive(Clone, Copy)]
struct FanControlItem {
    cooler_id: u32,
    /// Wysterowanie w procentach (tryb ręczny).
    level: u32,
    mode: u32,
    reserved: [u32; 8],
}

#[repr(C)]
struct FanCoolerControl {
    version: u32,
    reserved: u32,
    count: u32,
    reserved2: [u32; 8],
    items: [FanControlItem; 32],
}

const FAN_MODE_AUTO: u32 = 0;
const FAN_MODE_MANUAL: u32 = 1;

// Rozmiar struktury jest częścią numeru wersji, który sprawdza sterownik.
const _: () = assert!(size_of::<FanCoolersStatus>() == 1704);
const _: () = assert!(size_of::<FanCoolerControl>() == 1452);
const _: () = assert!(size_of::<ThermalSensors>() == 168);

/// Bit maski i indeks czujnika hotspot.
const SENSOR_HOTSPOT: usize = 1;
/// Indeks czujnika temperatury pamięci (memory junction), jak w LibreHardwareMonitor.
const SENSOR_MEMORY: usize = 9;
/// Temperatura pamięci na kartach RTX 50 (indeks 0 pusty).
const SENSOR_MEMORY_BLACKWELL: usize = 2;

/// Jeden kanał topologii mocy (NvAPI_GPU_ClientPowerTopologyGetStatus,
/// nieudokumentowane - układ z LibreHardwareMonitor). `power` to udział w
/// limicie mocy w tysięcznych procenta; domena 0 = układ GPU, 1 = cała karta.
#[repr(C)]
#[derive(Clone, Copy)]
struct PowerTopologyEntry {
    domain: u32,
    reserved: u32,
    power: u32,
    reserved2: u32,
}

#[repr(C)]
struct PowerTopology {
    version: u32,
    count: u32,
    entries: [PowerTopologyEntry; 4],
}

const _: () = assert!(size_of::<PowerTopology>() == 72);

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
    _lib: libloading::Library,
    handles: Vec<Handle>,
    names: Vec<String>,
    pstates_version: u32,
    unload: Option<FnVoid>,
    get_thermal: Option<FnThermal>,
    get_sensors: Option<FnStruct<ThermalSensors>>,
    fan_status: Option<FnStruct<FanCoolersStatus>>,
    fan_get_control: Option<FnStruct<FanCoolerControl>>,
    fan_set_control: Option<FnStruct<FanCoolerControl>>,
    /// Oficjalna biblioteka monitorowania Nvidii: pobór mocy i wentylator.
    nvml: Option<super::nvml::Nvml>,
    /// Szczegółowe odczyty (PCIe TX/RX z NVML, topologia mocy) - patrz `set_detail`.
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
    get_topology: Option<FnStruct<PowerTopology>>,
    /// Maska czujników ThermalGetSensors obsługiwanych przez kartę (ustalana raz).
    sensor_mask: Option<u32>,
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
            let lib = libloading::Library::new("nvapi64.dll").map_err(|e| {
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
                get_sensors: q(query, ID_THERMAL_GET_SENSORS),
                fan_status: q(query, ID_FAN_COOLERS_GET_STATUS),
                fan_get_control: q(query, ID_FAN_COOLERS_GET_CONTROL),
                fan_set_control: q(query, ID_FAN_COOLERS_SET_CONTROL),
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
                get_topology: q(query, ID_POWER_TOPOLOGY_GET_STATUS),
                sensor_mask: None,
                _lib: lib,
            })
        }
    }

    fn handle(&self, gpu: usize) -> Result<Handle> {
        self.handles.get(gpu).copied().ok_or_else(|| {
            crate::i18n::tr("Nieprawidłowy indeks karty", "Invalid card index").to_string()
        })
    }

    fn read_fan_status(&self, h: Handle) -> Option<Box<FanCoolersStatus>> {
        let mut s: Box<FanCoolersStatus> = Box::new(unsafe { zeroed() });
        s.version = ver::<FanCoolersStatus>(1);
        call(
            "NvAPI_GPU_ClientFanCoolersGetStatus",
            self.fan_status,
            h,
            &mut *s,
        )
        .ok()?;
        Some(s)
    }

    fn read_fan_control(&self, h: Handle) -> Result<Box<FanCoolerControl>> {
        let mut c: Box<FanCoolerControl> = Box::new(unsafe { zeroed() });
        c.version = ver::<FanCoolerControl>(1);
        call(
            "NvAPI_GPU_ClientFanCoolersGetControl",
            self.fan_get_control,
            h,
            &mut *c,
        )?;
        Ok(c)
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

        // Hotspot: karta bez tego czujnika (np. seria RTX 50) odrzuca zapytanie
        // albo zwraca zero - wtedy wartości po prostu nie ma.
        if let Some(f) = self.get_sensors {
            let read = |mask: u32| {
                let mut s: ThermalSensors = unsafe { zeroed() };
                s.version = ver::<ThermalSensors>(2);
                s.mask = mask;
                (unsafe { f(h, &mut s) } == 0).then_some(s)
            };
            // Maska obsługiwanych czujników: najdłuższa ciągła, którą karta
            // przyjmuje (tak samo robi LibreHardwareMonitor).
            let mask = *self.sensor_mask.get_or_insert_with(|| {
                let mut best = 1 << SENSOR_HOTSPOT;
                for bits in 2..32 {
                    let mask = (1u32 << bits) - 1;
                    if read(mask).is_none() {
                        break;
                    }
                    best = mask;
                }
                best
            });
            if let Some(s) = read(mask) {
                // 255 °C (0xFF00) oznacza czujnik bez odczytu.
                let temp = |i: usize| {
                    let raw = s.temperatures[i];
                    (mask >> i & 1 == 1 && raw > 0 && raw < 255 * 256).then(|| raw as f32 / 256.0)
                };
                if temp(0).is_some() {
                    // Ampere/Ada: 0 = rdzeń, 1 = hotspot, 9 = pamięć.
                    m.hotspot_c = temp(SENSOR_HOTSPOT);
                    m.details.mem_temp_c = temp(SENSOR_MEMORY);
                } else {
                    // Blackwell (RTX 50, zmierzone na RTX 5070 pod obciążeniem):
                    // 0 i 9 puste, 1 = dokładna temperatura rdzenia (podąża za
                    // rdzeniem także pod obciążeniem - to nie hotspot), 2 = pamięć
                    // (całe stopnie, rośnie z opóźnieniem). Hotspotu sterownik RTX 50
                    // nie udostępnia ani w NVAPI, ani w NVML.
                    m.details.core_precise_c = temp(SENSOR_HOTSPOT);
                    m.details.mem_temp_c = temp(SENSOR_MEMORY_BLACKWELL);
                    m.details.hotspot_hidden = true;
                }
            }
        }

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

        // Moc samego układu GPU: udział domeny GPU względem całej karty z
        // topologii mocy, przeliczony na waty poboru całej karty.
        // Topologia mocy kosztuje ok. 11 ms CPU - tylko w widoku Informacje.
        if let (Some(f), Some(board_w), true) = (self.get_topology, m.power_w, self.detail) {
            let mut t: PowerTopology = unsafe { zeroed() };
            t.version = ver::<PowerTopology>(1);
            if unsafe { f(h, &mut t) } == 0 {
                let entries = &t.entries[..(t.count as usize).min(4)];
                let share = |d: u32| entries.iter().find(|e| e.domain == d).map(|e| e.power);
                if let (Some(chip), Some(board)) = (share(0), share(1).filter(|&b| b > 0)) {
                    m.details.chip_power_w = Some(board_w * chip as f32 / board as f32);
                }
            }
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

    fn fan(&mut self, gpu: usize) -> Option<FanStatus> {
        let h = self.handle(gpu).ok()?;
        let s = self.read_fan_status(h)?;
        let items = &s.items[..(s.count as usize).min(s.items.len())];
        let first = items.first()?;
        Some(FanStatus {
            // Karta ma zwykle kilka wentylatorów na tym samym wysterowaniu;
            // pokazujemy najszybszy.
            rpm: items.iter().map(|i| i.rpm).max(),
            pct: Some(first.level),
        })
    }

    fn fan_controllable(&mut self, gpu: usize) -> bool {
        let Ok(h) = self.handle(gpu) else {
            return false;
        };
        self.fan_set_control.is_some()
            && self.read_fan_status(h).is_some_and(|s| s.count > 0)
            && self.read_fan_control(h).is_ok_and(|c| c.count > 0)
    }

    fn set_fan(&mut self, gpu: usize, pct: Option<u32>) -> Result<()> {
        let h = self.handle(gpu)?;
        // Odczyt-zmiana-zapis: zmieniamy tylko tryb i wysterowanie, reszta
        // struktury wraca do sterownika taka, jaką podał.
        let mut c = self.read_fan_control(h)?;
        let count = (c.count as usize).min(c.items.len());
        if count == 0 {
            return Err(crate::i18n::tr(
                "sterownik nie zgłosił żadnego wentylatora",
                "the driver reported no fans",
            )
            .to_string());
        }
        for item in &mut c.items[..count] {
            match pct {
                Some(p) => {
                    item.mode = FAN_MODE_MANUAL;
                    item.level = p.min(100);
                }
                None => {
                    item.mode = FAN_MODE_AUTO;
                    item.level = 0;
                }
            }
        }
        call(
            "NvAPI_GPU_ClientFanCoolersSetControl",
            self.fan_set_control,
            h,
            &mut *c,
        )
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
        if let (Some(f), Ok(h)) = (api.get_sensors, api.handle(0)) {
            let mut s: ThermalSensors = unsafe { zeroed() };
            s.version = ver::<ThermalSensors>(2);
            s.mask = 0x1ffff;
            let st = unsafe { f(h, &mut s) };
            let t: Vec<String> = s.temperatures[..17]
                .iter()
                .map(|r| format!("{:.1}", *r as f32 / 256.0))
                .collect();
            println!("sensors {st}: {}", t.join(" "));
        }
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
                "load GPU {:?} MC {:?} VE {:?} BUS {:?} ENC {:?} DEC {:?} | power {:?} W chip {:?} W {:?} % TDP {:?} W | P{:?} limit {:?} | maska {:#x}",
                m.usage_pct, d.mem_ctrl_pct, d.video_pct, d.bus_pct, d.encoder_pct, d.decoder_pct,
                m.power_w, d.chip_power_w, d.power_pct_tdp, d.tdp_w, d.pstate, d.limit,
                api.sensor_mask.unwrap_or(0)
            );
        }
    }
}

#[cfg(test)]
mod hotspot_load {
    //! Który czujnik ThermalGetSensors to hotspot, a który pamięć - pomiar
    //! pod obciążeniem obliczeniowym (hotspot rośnie razem z rdzeniem):
    //! `TC_BURN_CSO=<burn.cso> cargo test nvapi_hotspot_load -- --ignored --nocapture`.
    use super::*;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;

    #[test]
    #[ignore]
    fn nvapi_hotspot_load() {
        let cso = std::fs::read(std::env::var("TC_BURN_CSO").expect("TC_BURN_CSO")).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let burner = std::thread::spawn(move || unsafe {
            let mut dev = None;
            let mut ctx = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )
            .unwrap();
            let (dev, ctx) = (dev.unwrap(), ctx.unwrap());
            let mut cs = None;
            dev.CreateComputeShader(&cso, None, Some(&mut cs)).unwrap();
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: 4096,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
                MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
                StructureByteStride: 4,
                ..Default::default()
            };
            let mut buf = None;
            dev.CreateBuffer(&desc, None, Some(&mut buf)).unwrap();
            let mut uav = None;
            dev.CreateUnorderedAccessView(&buf.unwrap(), None, Some(&mut uav))
                .unwrap();
            ctx.CSSetShader(cs.as_ref(), None);
            ctx.CSSetUnorderedAccessViews(0, 1, Some(&Some(uav.unwrap())), None);
            let qd = D3D11_QUERY_DESC {
                Query: D3D11_QUERY_EVENT,
                MiscFlags: 0,
            };
            let mut q = None;
            dev.CreateQuery(&qd, Some(&mut q)).unwrap();
            let q = q.unwrap();
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                ctx.Dispatch(2048, 1, 1);
                ctx.End(&q);
                let mut done = 0u32;
                while ctx
                    .GetData(&q, Some(&mut done as *mut u32 as *mut _), 4, 0)
                    .is_err()
                    || done == 0
                {
                    std::thread::yield_now();
                }
            }
        });
        let mut api = NvApi::load().unwrap();
        let h = api.handle(0).unwrap();
        for step in 0..30 {
            let mut s: ThermalSensors = unsafe { zeroed() };
            s.version = ver::<ThermalSensors>(2);
            s.mask = 0x1ffff;
            unsafe { api.get_sensors.unwrap()(h, &mut s) };
            let m = api.monitor(0);
            println!(
                "{:>3} s  core {:?}  [1] {:.2}  [2] {:.2}  f308 {:.2}  moc {:?} W  load {:?} %",
                step * 2,
                m.temp_c,
                s.temperatures[1] as f32 / 256.0,
                s.temperatures[2] as f32 / 256.0,
                api.nvml
                    .as_ref()
                    .and_then(|n| n.field_f64(0, "", 308))
                    .unwrap_or(-1.0),
                m.power_w.map(|w| w.round()),
                m.usage_pct
            );
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        burner.join().unwrap();
        for step in 0..5 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let mut s: ThermalSensors = unsafe { zeroed() };
            s.version = ver::<ThermalSensors>(2);
            s.mask = 0x1ffff;
            unsafe { api.get_sensors.unwrap()(h, &mut s) };
            let m = api.monitor(0);
            println!(
                "po {:>2} s  core {:?}  [1] {:.2}  [2] {:.2}",
                (step + 1) * 2,
                m.temp_c,
                s.temperatures[1] as f32 / 256.0,
                s.temperatures[2] as f32 / 256.0
            );
        }
    }
}

#[cfg(test)]
mod thermal_probe {
    //! Sprawdza, jakie wersje/rozmiary ThermalGetSensors przyjmuje karta
    //! (tylko odczyt, bufor większy niż deklarowany rozmiar):
    //! `cargo test nvapi_thermal_probe -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn nvapi_thermal_probe() {
        let api = NvApi::load().unwrap();
        let h = api.handle(0).unwrap();
        let f: unsafe extern "C" fn(Handle, *mut u32) -> Status =
            unsafe { std::mem::transmute(api.get_sensors.unwrap()) };
        for v in 1..=4u32 {
            for temps in [32usize, 64, 128] {
                let size = 8 + 32 + temps * 4;
                let mut buf = vec![0u32; 2048];
                buf[0] = size as u32 | (v << 16);
                buf[1] = u32::MAX;
                let st = unsafe { f(h, buf.as_mut_ptr()) };
                if st == 0 {
                    let vals: Vec<String> = buf[10..10 + temps]
                        .iter()
                        .enumerate()
                        .filter(|(_, &r)| r != 0 && r != 0xFF00)
                        .map(|(i, &r)| format!("[{i}] {:.2}", r as i32 as f32 / 256.0))
                        .collect();
                    println!(
                        "v{v} rozmiar {size}: maska {:#x} {}",
                        buf[1],
                        vals.join(" ")
                    );
                } else {
                    println!("v{v} rozmiar {size}: status {st}");
                }
            }
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
