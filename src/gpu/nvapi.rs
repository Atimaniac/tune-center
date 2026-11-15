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
            let mut s: ThermalSensors = unsafe { zeroed() };
            s.version = ver::<ThermalSensors>(2);
            s.mask = 1 << SENSOR_HOTSPOT;
            if unsafe { f(h, &mut s) } == 0 {
                let raw = s.temperatures[SENSOR_HOTSPOT];
                if raw > 0 {
                    m.hotspot_c = Some(raw as f32 / 256.0);
                }
            }
        }

        if let Some(nvml) = &self.nvml {
            let name = self.names.get(gpu).map(String::as_str).unwrap_or("");
            let reading = nvml.read(gpu, name);
            m.power_w = reading.power_w;
            m.fan_pct = reading.fan_pct;
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
        if call("usages", self.get_usages, h, &mut u).is_ok() && u.utilization[0].present & 1 == 1 {
            m.usage_pct = Some(u.utilization[0].percent);
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
        call("NvAPI_GPU_SetPstates20", self.set_pstates, h, &mut *p)
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
