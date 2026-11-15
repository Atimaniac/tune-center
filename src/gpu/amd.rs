//! Karty AMD Radeon przez ADL (atiadlxx.dll), interfejs Overdrive 8
//! (seria RX 5000 i nowsze).
//!
//! Układy struktur, identyfikatory ustawień i kolejność wywołań pochodzą
//! z oficjalnego ADL SDK (GPUOpen-LibrariesAndSDKs/display-library:
//! include/adl_defines.h, include/adl_structures.h, Sample/Overdrive8).
//!
//! Różnica względem Nvidii: sterownik AMD nie przyjmuje offsetów, tylko
//! bezwzględne wartości maksymalnego zegara rdzenia i pamięci (w MHz).
//! Krzywa V/F nie jest dostępna.

use super::backend::*;
use std::alloc::Layout;
use std::ffi::{c_char, c_void};
use std::mem::{size_of, zeroed};

type Ctx = *mut c_void;
type AllocCb = unsafe extern "system" fn(i32) -> *mut c_void;
type FnCreate = unsafe extern "C" fn(AllocCb, i32, *mut Ctx) -> i32;
type FnDestroy = unsafe extern "C" fn(Ctx) -> i32;
type FnNumAdapters = unsafe extern "C" fn(Ctx, *mut i32) -> i32;
type FnAdapterInfo = unsafe extern "C" fn(Ctx, *mut AdapterInfo, i32) -> i32;
type FnCaps = unsafe extern "C" fn(Ctx, i32, *mut i32, *mut i32, *mut i32) -> i32;
type FnInit = unsafe extern "C" fn(Ctx, i32, *mut InitSetting) -> i32;
type FnInitX2 = unsafe extern "C" fn(Ctx, i32, *mut i32, *mut i32, *mut *mut SingleInit) -> i32;
type FnCurrent = unsafe extern "C" fn(Ctx, i32, *mut CurrentSetting) -> i32;
type FnCurrentX2 = unsafe extern "C" fn(Ctx, i32, *mut i32, *mut *mut i32) -> i32;
type FnSet = unsafe extern "C" fn(Ctx, i32, *mut SetSetting, *mut CurrentSetting) -> i32;
type FnPmLog = unsafe extern "C" fn(Ctx, i32, *mut PmLogOutput) -> i32;
/// ADL2_Overdrive5_FanSpeed_Get / _Set: (kontekst, adapter, kontroler termiczny, wartość).
type FnFanSpeed = unsafe extern "C" fn(Ctx, i32, i32, *mut FanSpeedValue) -> i32;
/// ADL2_Overdrive5_FanSpeedToDefault_Set: (kontekst, adapter, kontroler termiczny).
type FnFanDefault = unsafe extern "C" fn(Ctx, i32, i32) -> i32;

/// ADLFanSpeedValue.
#[repr(C)]
struct FanSpeedValue {
    size: i32,
    speed_type: i32,
    speed: i32,
    flags: i32,
}

const FANCTRL_SPEED_TYPE_PERCENT: i32 = 1;
const FANCTRL_SPEED_TYPE_RPM: i32 = 2;
const FANCTRL_FLAG_USER_DEFINED_SPEED: i32 = 1;
type FnRegSet = unsafe extern "C" fn(Ctx, i32, i32, *const c_char, *const c_char, i32) -> i32;

const ADL_MAX_PATH: usize = 256;
const AMD_VENDOR_ID: i32 = 1002;

// ADLOD8SettingId (adl_defines.h)
const OD8_GFXCLK_FMAX: usize = 0;
const OD8_GFXCLK_FMIN: usize = 1;
const OD8_GFXCLK_FREQ3: usize = 6;
const OD8_UCLK_FMAX: usize = 8;
const OD8_UCLK_FMIN: usize = 34;
const OD8_OPTIMZED_POWER_MODE: usize = 36;
/// Liczba ustawień w bieżącym SDK.
const OD8_COUNT: usize = 77;
/// Tabele w naszych strukturach są większe niż w SDK: jeśli nowszy sterownik
/// zna więcej ustawień, nie wyjdzie przy zapisie poza bufor.
const TABLE: usize = 256;

// ADLOD8FeatureControl
const CAP_GFXCLK_LIMITS: i32 = 1 << 0;
const CAP_GFXCLK_CURVE: i32 = 1 << 1;
const CAP_UCLK_MAX: i32 = 1 << 2;
const CAP_OPTIMIZED_POWER_MODE: i32 = 1 << 16;

// ADL_PMLOG_SENSORS
const PMLOG_CLK_GFXCLK: usize = 1;
const PMLOG_CLK_MEMCLK: usize = 2;
const PMLOG_TEMPERATURE_EDGE: usize = 8;
const PMLOG_FAN_RPM: usize = 14;
const PMLOG_FAN_PERCENTAGE: usize = 15;
const PMLOG_INFO_ACTIVITY_GFX: usize = 19;
const PMLOG_GFX_VOLTAGE: usize = 21;
const PMLOG_ASIC_POWER: usize = 23;
const PMLOG_TEMPERATURE_HOTSPOT: usize = 27;
const PMLOG_TEMPERATURE_GFX: usize = 28;

// ----------------------------- struktury -----------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct AdapterInfo {
    size: i32,
    adapter_index: i32,
    udid: [u8; ADL_MAX_PATH],
    bus_number: i32,
    device_number: i32,
    function_number: i32,
    vendor_id: i32,
    adapter_name: [u8; ADL_MAX_PATH],
    display_name: [u8; ADL_MAX_PATH],
    present: i32,
    exist: i32,
    driver_path: [u8; ADL_MAX_PATH],
    driver_path_ext: [u8; ADL_MAX_PATH],
    pnp_string: [u8; ADL_MAX_PATH],
    os_display_index: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SingleInit {
    feature_id: i32,
    min: i32,
    max: i32,
    default: i32,
}

#[repr(C)]
struct InitSetting {
    count: i32,
    caps: i32,
    table: [SingleInit; TABLE],
}

#[repr(C)]
struct CurrentSetting {
    count: i32,
    table: [i32; TABLE],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SingleSet {
    value: i32,
    requested: i32,
    reset: i32,
}

#[repr(C)]
struct SetSetting {
    count: i32,
    table: [SingleSet; TABLE],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SensorData {
    supported: i32,
    value: i32,
}

#[repr(C)]
struct PmLogOutput {
    size: i32,
    sensors: [SensorData; 256],
}

// Rozmiary zgodne z nagłówkami SDK (Windows).
const _: () = assert!(size_of::<AdapterInfo>() == 1572);
const _: () = assert!(size_of::<SingleInit>() == 16);
const _: () = assert!(size_of::<SingleSet>() == 12);
const _: () = assert!(size_of::<PmLogOutput>() == 2052);

// ------------------------- pamięć dla ADL ----------------------------------
// ADL prosi o pamięć przez wywołanie zwrotne, a zwalnia ją wywołujący.
// Rozmiar bloku trzymamy w nagłówku przed zwracanym wskaźnikiem.

const HEADER: usize = 16;

unsafe extern "system" fn adl_alloc(size: i32) -> *mut c_void {
    if size <= 0 {
        return std::ptr::null_mut();
    }
    let total = size as usize + HEADER;
    let Ok(layout) = Layout::from_size_align(total, 16) else {
        return std::ptr::null_mut();
    };
    let base = std::alloc::alloc_zeroed(layout);
    if base.is_null() {
        return std::ptr::null_mut();
    }
    (base as *mut usize).write(total);
    base.add(HEADER) as *mut c_void
}

unsafe fn adl_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let base = (ptr as *mut u8).sub(HEADER);
    let total = (base as *const usize).read();
    std::alloc::dealloc(base, Layout::from_size_align_unchecked(total, 16));
}

// ------------------------------- API ---------------------------------------

struct Od8 {
    caps: i32,
    init: [SingleInit; OD8_COUNT],
    current: [i32; OD8_COUNT],
}

pub struct Amd {
    _lib: libloading::Library,
    ctx: Ctx,
    /// Indeksy adapterów ADL, po jednym na fizyczną kartę.
    adapters: Vec<i32>,
    names: Vec<String>,
    destroy: Option<FnDestroy>,
    caps: Option<FnCaps>,
    init: Option<FnInit>,
    init_x2: Option<FnInitX2>,
    current: Option<FnCurrent>,
    current_x2: Option<FnCurrentX2>,
    set: Option<FnSet>,
    pmlog: Option<FnPmLog>,
    reg_set: Option<FnRegSet>,
    fan_get: Option<FnFanSpeed>,
    fan_set: Option<FnFanSpeed>,
    fan_default: Option<FnFanDefault>,
}

unsafe fn sym<T: Copy>(lib: &libloading::Library, name: &[u8]) -> Option<T> {
    lib.get::<T>(name).ok().map(|s| *s)
}

fn c_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}

fn check(name: &str, status: i32) -> Result<()> {
    match status {
        0 => Ok(()),
        -8 => Err(crate::trf!(
            "{name}: funkcja nieobsługiwana przez tę kartę (kod -8)",
            "{name}: function not supported by this card (code -8)"
        )),
        s => Err(crate::trf!(
            "{name}: błąd ADL (kod {s})",
            "{name}: ADL error (code {s})"
        )),
    }
}

impl Amd {
    pub fn load() -> Result<Self> {
        unsafe {
            let lib = libloading::Library::new("atiadlxx.dll").map_err(|e| {
                crate::trf!(
                    "Nie można załadować atiadlxx.dll (brak sterownika AMD?): {e}",
                    "Cannot load atiadlxx.dll (AMD driver missing?): {e}"
                )
            })?;

            let create: FnCreate = sym(&lib, b"ADL2_Main_Control_Create\0").ok_or_else(|| {
                crate::i18n::tr(
                    "Brak ADL2_Main_Control_Create",
                    "ADL2_Main_Control_Create is missing",
                )
                .to_string()
            })?;
            let num_adapters: FnNumAdapters = sym(&lib, b"ADL2_Adapter_NumberOfAdapters_Get\0")
                .ok_or_else(|| {
                    crate::i18n::tr(
                        "Brak ADL2_Adapter_NumberOfAdapters_Get",
                        "ADL2_Adapter_NumberOfAdapters_Get is missing",
                    )
                    .to_string()
                })?;
            let adapter_info: FnAdapterInfo = sym(&lib, b"ADL2_Adapter_AdapterInfo_Get\0")
                .ok_or_else(|| {
                    crate::i18n::tr(
                        "Brak ADL2_Adapter_AdapterInfo_Get",
                        "ADL2_Adapter_AdapterInfo_Get is missing",
                    )
                    .to_string()
                })?;
            let destroy: Option<FnDestroy> = sym(&lib, b"ADL2_Main_Control_Destroy\0");

            let mut ctx: Ctx = std::ptr::null_mut();
            check("ADL2_Main_Control_Create", create(adl_alloc, 1, &mut ctx))?;

            let fail = |msg: String| -> Result<Self> {
                if let Some(f) = destroy {
                    f(ctx);
                }
                Err(msg)
            };

            let mut count = 0i32;
            if let Err(e) = check(
                "ADL2_Adapter_NumberOfAdapters_Get",
                num_adapters(ctx, &mut count),
            ) {
                return fail(e);
            }
            if count <= 0 {
                return fail(
                    crate::i18n::tr("Nie znaleziono żadnej karty AMD", "No AMD card found")
                        .to_string(),
                );
            }

            let mut infos: Vec<AdapterInfo> = vec![zeroed(); count as usize];
            let bytes = (size_of::<AdapterInfo>() * infos.len()) as i32;
            if let Err(e) = check(
                "ADL2_Adapter_AdapterInfo_Get",
                adapter_info(ctx, infos.as_mut_ptr(), bytes),
            ) {
                return fail(e);
            }

            // ADL zwraca osobny wpis dla każdego wyjścia obrazu; zostawiamy po
            // jednym na fizyczną kartę (ta sama szyna, urządzenie i funkcja).
            let mut adapters = Vec::new();
            let mut names = Vec::new();
            let mut seen: Vec<(i32, i32, i32)> = Vec::new();
            for info in &infos {
                if info.vendor_id != AMD_VENDOR_ID || info.bus_number < 0 {
                    continue;
                }
                let key = (info.bus_number, info.device_number, info.function_number);
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                adapters.push(info.adapter_index);
                let name = c_string(&info.adapter_name);
                names.push(if name.is_empty() {
                    format!("AMD GPU {}", adapters.len())
                } else {
                    name
                });
            }
            if adapters.is_empty() {
                return fail(
                    crate::i18n::tr("Nie znaleziono żadnej karty AMD", "No AMD card found")
                        .to_string(),
                );
            }

            Ok(Self {
                ctx,
                adapters,
                names,
                destroy,
                caps: sym(&lib, b"ADL2_Overdrive_Caps\0"),
                init: sym(&lib, b"ADL2_Overdrive8_Init_Setting_Get\0"),
                init_x2: sym(&lib, b"ADL2_Overdrive8_Init_SettingX2_Get\0"),
                current: sym(&lib, b"ADL2_Overdrive8_Current_Setting_Get\0"),
                current_x2: sym(&lib, b"ADL2_Overdrive8_Current_SettingX2_Get\0"),
                set: sym(&lib, b"ADL2_Overdrive8_Setting_Set\0"),
                pmlog: sym(&lib, b"ADL2_New_QueryPMLogData_Get\0"),
                reg_set: sym(&lib, b"ADL2_Adapter_RegValueInt_Set\0"),
                fan_get: sym(&lib, b"ADL2_Overdrive5_FanSpeed_Get\0"),
                fan_set: sym(&lib, b"ADL2_Overdrive5_FanSpeed_Set\0"),
                fan_default: sym(&lib, b"ADL2_Overdrive5_FanSpeedToDefault_Set\0"),
                _lib: lib,
            })
        }
    }

    fn adapter(&self, gpu: usize) -> Result<i32> {
        self.adapters.get(gpu).copied().ok_or_else(|| {
            crate::i18n::tr("Nieprawidłowy indeks karty", "Invalid card index").to_string()
        })
    }

    /// Obroty albo wysterowanie wentylatora przez Overdrive 5.
    fn od5_fan(&self, adapter: i32, speed_type: i32) -> Option<u32> {
        let f = self.fan_get?;
        let mut value = FanSpeedValue {
            size: size_of::<FanSpeedValue>() as i32,
            speed_type,
            speed: 0,
            flags: 0,
        };
        (unsafe { f(self.ctx, adapter, 0, &mut value) } == 0).then_some(value.speed.max(0) as u32)
    }

    /// Zakresy, wartości domyślne i bieżące ustawienia Overdrive 8.
    fn read_od8(&self, adapter: i32) -> Result<Od8> {
        let caps_fn = self.caps.ok_or_else(|| {
            crate::i18n::tr(
                "ADL2_Overdrive_Caps: funkcja niedostępna w tym sterowniku",
                "ADL2_Overdrive_Caps: function not available in this driver",
            )
            .to_string()
        })?;
        let (mut supported, mut enabled, mut version) = (0i32, 0i32, 0i32);
        check("ADL2_Overdrive_Caps", unsafe {
            caps_fn(
                self.ctx,
                adapter,
                &mut supported,
                &mut enabled,
                &mut version,
            )
        })?;
        if version != 8 {
            return Err(crate::trf!("Ta karta używa interfejsu Overdrive w wersji {version}. Strojenie zegarów jest obsługiwane tylko dla Overdrive 8 (seria RX 5000 i nowsze).", "This card uses Overdrive interface version {version}. Clock tuning is supported only for Overdrive 8 (RX 5000 series and newer)."
            ));
        }

        let mut od = Od8 {
            caps: 0,
            init: [SingleInit::default(); OD8_COUNT],
            current: [0; OD8_COUNT],
        };

        // --- zakresy ---
        if let Some(f) = self.init_x2 {
            let mut n = OD8_COUNT as i32;
            let mut list: *mut SingleInit = std::ptr::null_mut();
            let status = unsafe { f(self.ctx, adapter, &mut od.caps, &mut n, &mut list) };
            if status == 0 && !list.is_null() {
                let n = (n.max(0) as usize).min(OD8_COUNT);
                for (i, slot) in od.init.iter_mut().enumerate().take(n) {
                    *slot = unsafe { *list.add(i) };
                }
            }
            unsafe { adl_free(list as *mut c_void) };
            check("ADL2_Overdrive8_Init_SettingX2_Get", status)?;
        } else {
            let f = self.init.ok_or_else(|| {
                crate::i18n::tr(
                    "ADL2_Overdrive8_Init_Setting_Get: funkcja niedostępna w tym sterowniku",
                    "ADL2_Overdrive8_Init_Setting_Get: function not available in this driver",
                )
                .to_string()
            })?;
            let mut s: Box<InitSetting> = Box::new(unsafe { zeroed() });
            s.count = OD8_COUNT as i32;
            check("ADL2_Overdrive8_Init_Setting_Get", unsafe {
                f(self.ctx, adapter, &mut *s)
            })?;
            od.caps = s.caps;
            od.init.copy_from_slice(&s.table[..OD8_COUNT]);
        }

        // --- bieżące wartości ---
        if let Some(f) = self.current_x2 {
            let mut n = OD8_COUNT as i32;
            let mut list: *mut i32 = std::ptr::null_mut();
            let status = unsafe { f(self.ctx, adapter, &mut n, &mut list) };
            if status == 0 && !list.is_null() {
                let n = (n.max(0) as usize).min(OD8_COUNT);
                for (i, slot) in od.current.iter_mut().enumerate().take(n) {
                    *slot = unsafe { *list.add(i) };
                }
            }
            unsafe { adl_free(list as *mut c_void) };
            check("ADL2_Overdrive8_Current_SettingX2_Get", status)?;
        } else {
            let f = self.current.ok_or_else(|| {
                crate::i18n::tr(
                    "ADL2_Overdrive8_Current_Setting_Get: funkcja niedostępna w tym sterowniku",
                    "ADL2_Overdrive8_Current_Setting_Get: function not available in this driver",
                )
                .to_string()
            })?;
            let mut s: Box<CurrentSetting> = Box::new(unsafe { zeroed() });
            s.count = OD8_COUNT as i32;
            check("ADL2_Overdrive8_Current_Setting_Get", unsafe {
                f(self.ctx, adapter, &mut *s)
            })?;
            od.current.copy_from_slice(&s.table[..OD8_COUNT]);
        }

        Ok(od)
    }

    fn write(
        &self,
        adapter: i32,
        set: &mut SetSetting,
        current: &mut CurrentSetting,
    ) -> Result<()> {
        let f = self.set.ok_or_else(|| {
            crate::i18n::tr(
                "ADL2_Overdrive8_Setting_Set: funkcja niedostępna w tym sterowniku",
                "ADL2_Overdrive8_Setting_Set: function not available in this driver",
            )
            .to_string()
        })?;
        check("ADL2_Overdrive8_Setting_Set", unsafe {
            f(self.ctx, adapter, set, current)
        })
    }

    fn info(od: &Od8, id: usize) -> Option<OffsetInfo> {
        let r = od.init[id];
        (r.max > r.min).then(|| OffsetInfo {
            value_khz: od.current[id].saturating_mul(1000),
            min_khz: r.min.saturating_mul(1000),
            max_khz: r.max.saturating_mul(1000),
            default_khz: r.default.saturating_mul(1000),
        })
    }
}

impl Drop for Amd {
    fn drop(&mut self) {
        if let Some(f) = self.destroy {
            unsafe {
                f(self.ctx);
            }
        }
    }
}

impl Backend for Amd {
    fn gpus(&self) -> Vec<String> {
        self.names.clone()
    }

    fn vendor(&self, _gpu: usize) -> Vendor {
        Vendor::Amd
    }

    fn monitor(&mut self, gpu: usize) -> Monitor {
        let mut m = Monitor::default();
        let (Ok(adapter), Some(f)) = (self.adapter(gpu), self.pmlog) else {
            return m;
        };
        let mut log: Box<PmLogOutput> = Box::new(unsafe { zeroed() });
        log.size = size_of::<PmLogOutput>() as i32;
        if unsafe { f(self.ctx, adapter, &mut *log) } != 0 {
            return m;
        }
        let read = |id: usize| {
            let s = log.sensors[id];
            (s.supported != 0).then_some(s.value)
        };
        m.temp_c = read(PMLOG_TEMPERATURE_EDGE).or_else(|| read(PMLOG_TEMPERATURE_GFX));
        m.core_mhz = read(PMLOG_CLK_GFXCLK).map(|v| v as f32);
        m.mem_mhz = read(PMLOG_CLK_MEMCLK).map(|v| v as f32);
        m.voltage_mv = read(PMLOG_GFX_VOLTAGE).filter(|v| *v > 0).map(|v| v as f32);
        m.usage_pct = read(PMLOG_INFO_ACTIVITY_GFX).map(|v| v.max(0) as u32);
        m.hotspot_c = read(PMLOG_TEMPERATURE_HOTSPOT)
            .filter(|v| *v > 0)
            .map(|v| v as f32);
        m.fan_rpm = read(PMLOG_FAN_RPM).map(|v| v.max(0) as u32);
        m.fan_pct = read(PMLOG_FAN_PERCENTAGE).map(|v| v.clamp(0, 100) as u32);
        m.power_w = read(PMLOG_ASIC_POWER).filter(|v| *v > 0).map(|v| v as f32);
        m
    }

    fn fan(&mut self, gpu: usize) -> Option<FanStatus> {
        let adapter = self.adapter(gpu).ok()?;
        let status = FanStatus {
            rpm: self.od5_fan(adapter, FANCTRL_SPEED_TYPE_RPM),
            pct: self.od5_fan(adapter, FANCTRL_SPEED_TYPE_PERCENT),
        };
        if status != FanStatus::default() {
            return Some(status);
        }
        // Nowsze karty podają wentylator tylko w danych PMLog.
        let m = self.monitor(gpu);
        (m.fan_rpm.is_some() || m.fan_pct.is_some()).then_some(FanStatus {
            rpm: m.fan_rpm,
            pct: m.fan_pct,
        })
    }

    fn fan_controllable(&mut self, gpu: usize) -> bool {
        let Ok(adapter) = self.adapter(gpu) else {
            return false;
        };
        // Sterowanie idzie przez Overdrive 5; skoro sterownik odpowiada na
        // odczyt wysterowania tą drogą, przyjmie też zapis.
        self.fan_set.is_some()
            && self.fan_default.is_some()
            && self.od5_fan(adapter, FANCTRL_SPEED_TYPE_PERCENT).is_some()
    }

    fn set_fan(&mut self, gpu: usize, pct: Option<u32>) -> Result<()> {
        let adapter = self.adapter(gpu)?;
        match pct {
            Some(p) => {
                let f = self.fan_set.ok_or_else(|| {
                    crate::i18n::tr(
                        "ADL2_Overdrive5_FanSpeed_Set: funkcja niedostępna w tym sterowniku",
                        "ADL2_Overdrive5_FanSpeed_Set: function not available in this driver",
                    )
                    .to_string()
                })?;
                let mut value = FanSpeedValue {
                    size: size_of::<FanSpeedValue>() as i32,
                    speed_type: FANCTRL_SPEED_TYPE_PERCENT,
                    speed: p.min(100) as i32,
                    flags: FANCTRL_FLAG_USER_DEFINED_SPEED,
                };
                check("ADL2_Overdrive5_FanSpeed_Set", unsafe {
                    f(self.ctx, adapter, 0, &mut value)
                })
            }
            None => {
                let f = self.fan_default.ok_or_else(|| {
                    crate::i18n::tr(
                        "ADL2_Overdrive5_FanSpeedToDefault_Set: funkcja niedostępna w tym sterowniku",
                        "ADL2_Overdrive5_FanSpeedToDefault_Set: function not available in this driver",
                    )
                    .to_string()
                })?;
                check("ADL2_Overdrive5_FanSpeedToDefault_Set", unsafe {
                    f(self.ctx, adapter, 0)
                })
            }
        }
    }

    /// Dla AMD "offsety" to bezwzględne maksymalne zegary (wartości w kHz).
    fn offsets(&mut self, gpu: usize) -> Result<Offsets> {
        let od = self.read_od8(self.adapter(gpu)?)?;
        let mut out = Offsets::default();
        if od.caps & CAP_GFXCLK_LIMITS != 0 {
            out.core = Self::info(&od, OD8_GFXCLK_FMAX);
        }
        if od.caps & CAP_UCLK_MAX != 0 {
            out.mem = Self::info(&od, OD8_UCLK_FMAX);
        }
        Ok(out)
    }

    fn set_offset(&mut self, gpu: usize, domain: Domain, khz: i32) -> Result<()> {
        let adapter = self.adapter(gpu)?;
        let od = self.read_od8(adapter)?;
        let mhz = khz / 1000;

        let (id, cap, label) = match domain {
            Domain::Core => (
                OD8_GFXCLK_FMAX,
                CAP_GFXCLK_LIMITS,
                crate::i18n::tr("zegar rdzenia", "core clock"),
            ),
            Domain::Memory => (
                OD8_UCLK_FMAX,
                CAP_UCLK_MAX,
                crate::i18n::tr("zegar pamięci", "memory clock"),
            ),
        };
        if od.caps & cap == 0 {
            return Err(crate::trf!(
                "Ta karta nie pozwala zmieniać ustawienia: {label}",
                "This card does not allow changing: {label}"
            ));
        }
        let range = od.init[id];
        if mhz < range.min || mhz > range.max {
            return Err(crate::trf!(
                "{label}: {mhz} MHz poza zakresem sterownika ({}-{} MHz)",
                "{label}: {mhz} MHz is outside the driver range ({}-{} MHz)",
                range.min,
                range.max
            ));
        }

        let mut current: Box<CurrentSetting> = Box::new(unsafe { zeroed() });
        current.count = OD8_COUNT as i32;
        current.table[..OD8_COUNT].copy_from_slice(&od.current);

        let mut set: Box<SetSetting> = Box::new(unsafe { zeroed() });
        set.count = OD8_COUNT as i32;

        // Kolejność jak w przykładzie AMD (Sample/Overdrive8: SetOD8Range /
        // SetOD8PlusRange). Karty z limitami zegara, ale bez krzywej
        // (RX 6000 i nowsze) wymagają trybu ręcznego i podania pary min/max.
        let plus = od.caps & CAP_GFXCLK_LIMITS != 0 && od.caps & CAP_GFXCLK_CURVE == 0;
        if plus {
            if od.caps & CAP_OPTIMIZED_POWER_MODE != 0 {
                let mut mode: Box<SetSetting> = Box::new(unsafe { zeroed() });
                mode.count = OD8_COUNT as i32;
                mode.table[OD8_OPTIMZED_POWER_MODE].requested = 1;
                mode.table[OD8_OPTIMZED_POWER_MODE].value = 3; // tryb ręczny
                self.write(adapter, &mut mode, &mut current)?;
            }
            let pair = match domain {
                Domain::Core => [OD8_GFXCLK_FMAX, OD8_GFXCLK_FMIN],
                Domain::Memory => [OD8_UCLK_FMAX, OD8_UCLK_FMIN],
            };
            for i in pair {
                set.table[i].requested = 1;
                set.table[i].value = od.current[i];
            }
        }

        set.table[id].requested = 1;
        set.table[id].value = mhz;
        if !plus && id == OD8_GFXCLK_FMAX {
            set.table[OD8_GFXCLK_FREQ3].value = mhz;
        }

        self.write(adapter, &mut set, &mut current)?;

        // Jak w przykładzie AMD: oznacza w sterowniku, że strojenie jest ręczne.
        if let Some(f) = self.reg_set {
            unsafe {
                f(
                    self.ctx,
                    adapter,
                    1,
                    std::ptr::null(),
                    c"IsAutoDefault".as_ptr(),
                    0,
                );
            }
        }
        Ok(())
    }

    fn curve(&mut self, _gpu: usize) -> Result<Vec<CurvePoint>> {
        Err(crate::i18n::tr(
            "Krzywa V/F nie jest dostępna dla kart AMD.",
            "The V/F curve is not available for AMD cards.",
        )
        .to_string())
    }

    fn set_curve(&mut self, _gpu: usize, _deltas: &[(usize, i32)]) -> Result<()> {
        Err(crate::i18n::tr(
            "Krzywa V/F nie jest dostępna dla kart AMD.",
            "The V/F curve is not available for AMD cards.",
        )
        .to_string())
    }
}
