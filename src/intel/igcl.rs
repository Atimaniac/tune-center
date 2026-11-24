//! Wiązanie do Intel Graphics Control Library (IGCL, `ControlLib.dll`,
//! instalowanej ze sterownikiem karty Intel) - publicznego API sterownika
//! Intela. Struktury i funkcje z nagłówka `include/igcl_api.h`
//! (github.com/intel/drivers.gpu.control-library, wersja API 1.1).
//!
//! Biblioteka leży w System32 albo - przy części sterowników DCH, np. dla
//! starszych grafik zintegrowanych - tylko w folderze sterownika w
//! DriverStore; wtedy szukamy jej obok sterownika trybu użytkownika karty
//! Intel (`UserModeDriverName` w kluczu karty).

use std::ffi::{c_char, c_void};
use std::ptr::null_mut;

pub type Handle = *mut c_void;

/// ctl_result_t
pub const CTL_RESULT_SUCCESS: u32 = 0;
const CTL_RESULT_SUCCESS_STILL_OPEN_BY_ANOTHER_CALLER: u32 = 1;
pub const CTL_RESULT_ERROR_UNSUPPORTED_FEATURE: u32 = 0x4000_000a;
const CTL_RESULT_ERROR_UNSUPPORTED_VERSION: u32 = 0x4000_0009;

/// CTL_MAKE_VERSION(1, 1)
const CTL_IMPL_VERSION: u32 = (1 << 16) | 1;

/// ctl_adapter_properties_flag_t
pub const ADAPTER_FLAG_INTEGRATED: u32 = 1;

#[repr(C)]
#[derive(Default)]
struct InitArgs {
    size: u32,
    version: u8,
    app_version: u32,
    flags: u32,
    supported_version: u32,
    /// ctl_application_id_t (GUID)
    uid: [u32; 4],
}

#[repr(C)]
struct FirmwareVersion {
    major: u64,
    minor: u64,
    build: u64,
}

/// ctl_device_adapter_properties_t
#[repr(C)]
struct DeviceProps {
    size: u32,
    version: u8,
    p_device_id: *mut c_void,
    device_id_size: u32,
    device_type: u32,
    supported_subfunction_flags: u32,
    driver_version: u64,
    firmware_version: FirmwareVersion,
    pci_vendor_id: u32,
    pci_device_id: u32,
    rev_id: u32,
    num_eus_per_sub_slice: u32,
    num_sub_slices_per_slice: u32,
    num_slices: u32,
    name: [c_char; 100],
    graphics_adapter_properties: u32,
    frequency: u32,
    pci_subsys_id: u16,
    pci_subsys_vendor_id: u16,
    adapter_bdf: [u8; 3],
    num_xe_cores: u32,
    reserved: [u8; 108],
}

/// ctl_property_t: unia { bool Enable; float/int32/uint32 Value } albo
/// { uint32 EnableType }. Pierwsze 4 bajty to `Enable` (bool w bajcie 0)
/// albo `EnableType`, kolejne - wartość liczby.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Property {
    pub head: u32,
    pub value: u32,
}

impl Property {
    pub fn enabled(&self) -> bool {
        self.head & 0xFF != 0
    }
    pub fn enum_type(&self) -> u32 {
        self.head
    }
    pub fn int(&self) -> i32 {
        self.value as i32
    }
    pub fn float(&self) -> f32 {
        f32::from_bits(self.value)
    }
}

/// ctl_3d_feature_getset_t
#[repr(C)]
struct FeatureGetSet {
    size: u32,
    version: u8,
    feature: u32,
    app_name: *mut c_char,
    app_name_len: i8,
    set: bool,
    value_type: u32,
    value: Property,
    custom_value_size: i32,
    custom_value: *mut c_void,
}

/// ctl_property_info_t: unia (największy wariant: bool + 4 x float = 20 B,
/// wariant enum: uint64 + uint32 = 16 B, wyrównanie 8).
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct PropertyInfo {
    pub raw: [u32; 6],
}

impl PropertyInfo {
    /// Wariant enum: maska obsługiwanych wartości (bity) i wartość domyślna.
    pub fn enum_info(&self) -> (u64, u32) {
        (
            self.raw[0] as u64 | (self.raw[1] as u64) << 32,
            self.raw[2],
        )
    }
    /// Wariant int: (min, max, krok, domyślna); `DefaultEnable` w bajcie 0.
    pub fn int_range(&self) -> (i32, i32, i32, i32) {
        (
            self.raw[1] as i32,
            self.raw[2] as i32,
            self.raw[3] as i32,
            self.raw[4] as i32,
        )
    }
    /// Wariant float: (min, max, krok, domyślna).
    pub fn float_range(&self) -> (f32, f32, f32, f32) {
        let f = |i: usize| f32::from_bits(self.raw[i]);
        (f(1), f(2), f(3), f(4))
    }
}

/// ctl_3d_feature_details_t
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FeatureDetails {
    pub feature: u32,
    pub value_type: u32,
    pub info: PropertyInfo,
    custom_value_size: i32,
    custom_value: *mut c_void,
    pub per_app: bool,
    conflicting: i64,
    pub misc: i16,
    reserved: [i16; 3],
}

/// ctl_3d_feature_caps_t
#[repr(C)]
struct FeatureCaps {
    size: u32,
    version: u8,
    count: u32,
    details: *mut FeatureDetails,
}

/// ctl_property_value_type_t
pub mod value_type {
    pub const BOOL: u32 = 0;
    pub const FLOAT: u32 = 1;
    pub const INT32: u32 = 2;
    pub const UINT32: u32 = 3;
    pub const ENUM: u32 = 4;
}

/// ctl_3d_feature_t
pub mod feature {
    pub const FRAME_PACING: u32 = 0;
    pub const FRAME_LIMIT: u32 = 2;
    pub const ANISOTROPIC: u32 = 3;
    pub const CMAA: u32 = 4;
    pub const TEXTURE_FILTERING_QUALITY: u32 = 5;
    pub const ADAPTIVE_TESSELLATION: u32 = 6;
    pub const SHARPENING_FILTER: u32 = 7;
    pub const MSAA: u32 = 8;
    pub const GAMING_FLIP_MODES: u32 = 9;
    pub const ADAPTIVE_SYNC_PLUS: u32 = 10;
    pub const VRR_WINDOWED_BLT: u32 = 14;
    pub const LOW_LATENCY: u32 = 16;
    pub const FRAME_GENERATION: u32 = 17;
    pub const PREBUILT_SHADER_DOWNLOAD: u32 = 18;
}

// ------------------------------- ekran ---------------------------------------

/// ctl_intel_arc_sync_monitor_params_t
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ArcSyncMonitor {
    size: u32,
    version: u8,
    pub supported: bool,
    pub min_hz: f32,
    pub max_hz: f32,
    pub max_increase_us: u32,
    pub max_decrease_us: u32,
}

/// ctl_intel_arc_sync_profile_params_t
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ArcSyncProfile {
    size: u32,
    version: u8,
    /// ctl_intel_arc_sync_profile_t (patrz `arc_sync`).
    pub profile: u32,
    pub max_hz: f32,
    pub min_hz: f32,
    pub max_increase_us: u32,
    pub max_decrease_us: u32,
}

/// ctl_intel_arc_sync_profile_t
pub mod arc_sync {
    pub const RECOMMENDED: u32 = 1;
    pub const EXCELLENT: u32 = 2;
    pub const GOOD: u32 = 3;
    pub const COMPATIBLE: u32 = 4;
    pub const OFF: u32 = 5;
    pub const VESA: u32 = 6;
    pub const CUSTOM: u32 = 7;
}

/// ctl_scaling_caps_t / ctl_retro_scaling_caps_t / ctl_power_optimization_caps_t
#[repr(C)]
#[derive(Default)]
struct FlagCaps {
    size: u32,
    version: u8,
    flags: u32,
}

/// ctl_scaling_settings_t
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Scaling {
    size: u32,
    version: u8,
    pub enable: bool,
    /// ctl_scaling_type_flag_t (patrz `scaling`).
    pub kind: u32,
    custom_x: u32,
    custom_y: u32,
    hardware_mode_set: bool,
    preferred: u32,
}

/// ctl_scaling_type_flag_t / ctl_retro_scaling_type_flag_t
pub mod scaling {
    pub const IDENTITY: u32 = 1;
    pub const CENTERED: u32 = 2;
    pub const STRETCHED: u32 = 4;
    pub const ASPECT_RATIO: u32 = 8;
    pub const RETRO_INTEGER: u32 = 1;
    pub const RETRO_NEAREST_NEIGHBOUR: u32 = 2;
}

/// ctl_retro_scaling_settings_t
#[repr(C)]
#[derive(Default)]
struct RetroScaling {
    size: u32,
    version: u8,
    get: bool,
    enable: bool,
    kind: u32,
}

/// ctl_sharpness_filter_properties_t
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SharpnessFilter {
    pub kind: u32,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub default: f32,
}

/// ctl_sharpness_caps_t
#[repr(C)]
struct SharpnessCaps {
    size: u32,
    version: u8,
    flags: u32,
    count: u8,
    filters: *mut SharpnessFilter,
}

/// ctl_sharpness_settings_t
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Sharpness {
    size: u32,
    version: u8,
    pub enable: bool,
    pub kind: u32,
    pub intensity: f32,
}

/// ctl_power_optimization_settings_t (unia danych funkcji: 20 B).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct PowerSetting {
    size: u32,
    version: u8,
    plan: u32,
    feature: u32,
    enable: bool,
    specific: [u32; 5],
    source: u32,
}

/// ctl_power_optimization_flag_t
pub mod power {
    pub const FBC: u32 = 1;
    pub const PSR: u32 = 2;
    pub const DPST: u32 = 4;
    pub const LRR: u32 = 8;
}

type DisplaysFn = unsafe extern "C" fn(Handle, *mut u32, *mut Handle) -> u32;
type ArcInfoFn = unsafe extern "C" fn(Handle, *mut ArcSyncMonitor) -> u32;
type ArcProfileFn = unsafe extern "C" fn(Handle, *mut ArcSyncProfile) -> u32;
type FlagCapsFn = unsafe extern "C" fn(Handle, *mut FlagCaps) -> u32;
type ScalingFn = unsafe extern "C" fn(Handle, *mut Scaling) -> u32;
type RetroFn = unsafe extern "C" fn(Handle, *mut RetroScaling) -> u32;
type SharpCapsFn = unsafe extern "C" fn(Handle, *mut SharpnessCaps) -> u32;
type SharpFn = unsafe extern "C" fn(Handle, *mut Sharpness) -> u32;
type PowerFn = unsafe extern "C" fn(Handle, *mut PowerSetting) -> u32;

type InitFn = unsafe extern "C" fn(*mut InitArgs, *mut Handle) -> u32;
type CloseFn = unsafe extern "C" fn(Handle) -> u32;
type EnumDevicesFn = unsafe extern "C" fn(Handle, *mut u32, *mut Handle) -> u32;
type DevicePropsFn = unsafe extern "C" fn(Handle, *mut DeviceProps) -> u32;
type CapsFn = unsafe extern "C" fn(Handle, *mut FeatureCaps) -> u32;
type GetSetFn = unsafe extern "C" fn(Handle, *mut FeatureGetSet) -> u32;

/// Karta Intel widziana przez IGCL.
pub struct Device {
    pub handle: Handle,
    pub name: String,
    pub pci_device_id: u32,
    pub integrated: bool,
    /// Wersja sterownika (ctl `driver_version`, np. 31.0.101.2111).
    pub driver: String,
}

/// Sesja IGCL: biblioteka, uchwyt API i karty Intel.
pub struct Igcl {
    lib: crate::dll::Library,
    api: Handle,
    pub devices: Vec<Device>,
}

// Uchwyty IGCL są używane z wątku interfejsu; biblioteka jest wielowątkowa.
unsafe impl Send for Igcl {}

impl Drop for Igcl {
    fn drop(&mut self) {
        // SAFETY: uchwyt z ctlInit, zamykany raz.
        unsafe {
            if let Ok(close) = self.lib.get::<CloseFn>(b"ctlClose\0") {
                close(self.api);
            }
        }
    }
}

fn check(r: u32) -> Result<(), u32> {
    if r == CTL_RESULT_SUCCESS || r == CTL_RESULT_SUCCESS_STILL_OPEN_BY_ANOTHER_CALLER {
        Ok(())
    } else {
        Err(r)
    }
}

/// Opis kodu ctl_result_t.
pub fn describe(code: u32) -> String {
    let text = match code {
        0x4000_0007 => "CTL_RESULT_ERROR_NOT_AVAILABLE",
        0x4000_0006 => "CTL_RESULT_ERROR_INSUFFICIENT_PERMISSIONS",
        0x4000_0009 => "CTL_RESULT_ERROR_UNSUPPORTED_VERSION",
        CTL_RESULT_ERROR_UNSUPPORTED_FEATURE => {
            return crate::i18n::tr(
                "niedostępne na tej karcie albo w tym sterowniku",
                "not available on this card or driver",
            )
            .into()
        }
        0x4000_000b => "CTL_RESULT_ERROR_INVALID_ARGUMENT",
        0x4000_0015 => "CTL_RESULT_ERROR_NOT_IMPLEMENTED",
        0x4000_0020 => "CTL_RESULT_ERROR_PLATFORM_NOT_SUPPORTED",
        0x4000_0026 => "CTL_RESULT_ERROR_LOAD",
        0x4000_0024 => "CTL_RESULT_ERROR_RESET_DEVICE_REQUIRED",
        0x4000_0025 => "CTL_RESULT_ERROR_FULL_REBOOT_REQUIRED",
        _ => "",
    };
    if text.is_empty() {
        format!("IGCL 0x{code:08X}")
    } else {
        text.to_owned()
    }
}

/// Folder sterownika trybu użytkownika karty Intel (DriverStore) z rejestru.
#[cfg(windows)]
fn driver_store_dirs() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let class = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    for i in 0..16 {
        let key = format!(r"{class}\{i:04}");
        let Some(id) = crate::tune::reg::get_string(crate::tune::reg::HKLM, &key, "MatchingDeviceId") else {
            continue;
        };
        if !id.to_ascii_lowercase().contains("ven_8086") {
            continue;
        }
        if let Some(umd) = crate::tune::reg::get_string(crate::tune::reg::HKLM, &key, "UserModeDriverName") {
            if let Some(dir) = umd
                .split(['\0', ';'])
                .find(|p| !p.trim().is_empty())
                .and_then(|p| std::path::Path::new(p.trim()).parent().map(|d| d.to_path_buf()))
            {
                out.push(dir);
            }
        }
    }
    out
}

fn load_library() -> Result<crate::dll::Library, String> {
    // SAFETY: biblioteka sterownika Intel.
    unsafe {
        if let Ok(lib) = crate::dll::Library::new("ControlLib.dll") {
            return Ok(lib);
        }
        #[cfg(windows)]
        for dir in driver_store_dirs() {
            let path = dir.join("ControlLib.dll");
            if path.is_file() {
                if let Ok(lib) = crate::dll::Library::new(&path) {
                    return Ok(lib);
                }
            }
        }
    }
    Err(crate::i18n::tr(
        "Nie można załadować ControlLib.dll (brak sterownika karty Intel?)",
        "Cannot load ControlLib.dll (no Intel graphics driver?)",
    )
    .into())
}

fn text(raw: &[c_char]) -> String {
    let bytes: Vec<u8> = raw.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

impl Igcl {
    pub fn load() -> Result<Igcl, String> {
        // Wersja API 1.1; starsze biblioteki (np. 1.0.93 ze sterownikiem
        // 31.0.101.2111) odrzucają ją i podają swoją wersję - wtedy próbujemy
        // jeszcze raz z nią, na świeżo załadowanej bibliotece (po odmowie
        // biblioteka nie przyjmuje kolejnego ctlInit).
        match Self::init(CTL_IMPL_VERSION)? {
            Ok(igcl) => Ok(igcl),
            Err((r, supported)) if r == CTL_RESULT_ERROR_UNSUPPORTED_VERSION && supported != 0 => {
                match Self::init(supported)? {
                    Ok(igcl) => Ok(igcl),
                    Err((r, _)) => Err(format!("ctlInit: {}", describe(r))),
                }
            }
            Err((r, _)) => Err(format!("ctlInit: {}", describe(r))),
        }
    }

    /// `Ok(Err((kod, wersja obsługiwana)))` - biblioteka odmówiła.
    #[allow(clippy::type_complexity)]
    fn init(version: u32) -> Result<Result<Igcl, (u32, u32)>, String> {
        let lib = load_library()?;
        // SAFETY: funkcje o sygnaturach z igcl_api.h; struktury zerowane i
        // z polem Size jak w przykładach Intela.
        unsafe {
            let init: InitFn = *lib.get(b"ctlInit\0").map_err(|e| format!("ctlInit: {e}"))?;
            let mut args = InitArgs {
                size: std::mem::size_of::<InitArgs>() as u32,
                app_version: version,
                ..Default::default()
            };
            let mut api: Handle = null_mut();
            let r = init(&mut args, &mut api);
            if r != CTL_RESULT_SUCCESS || api.is_null() {
                return Ok(Err((r, args.supported_version)));
            }
            let mut igcl = Igcl {
                lib,
                api,
                devices: Vec::new(),
            };
            igcl.devices = igcl.enumerate()?;
            Ok(Ok(igcl))
        }
    }

    fn enumerate(&self) -> Result<Vec<Device>, String> {
        // SAFETY: jak w `load`.
        unsafe {
            let list: EnumDevicesFn = *self
                .lib
                .get(b"ctlEnumerateDevices\0")
                .map_err(|e| format!("ctlEnumerateDevices: {e}"))?;
            let props: DevicePropsFn = *self
                .lib
                .get(b"ctlGetDeviceProperties\0")
                .map_err(|e| format!("ctlGetDeviceProperties: {e}"))?;
            let mut count = 0u32;
            let r = list(self.api, &mut count, null_mut());
            if r != CTL_RESULT_SUCCESS {
                return Err(format!("ctlEnumerateDevices: {}", describe(r)));
            }
            let mut handles: Vec<Handle> = vec![null_mut(); count as usize];
            let r = list(self.api, &mut count, handles.as_mut_ptr());
            if r != CTL_RESULT_SUCCESS {
                return Err(format!("ctlEnumerateDevices: {}", describe(r)));
            }
            let mut out = Vec::new();
            for h in handles.into_iter().take(count as usize).filter(|h| !h.is_null()) {
                let mut luid = [0u8; 8];
                let mut p: DeviceProps = std::mem::zeroed();
                p.size = std::mem::size_of::<DeviceProps>() as u32;
                p.version = 2;
                p.p_device_id = luid.as_mut_ptr() as *mut c_void;
                p.device_id_size = luid.len() as u32;
                if props(h, &mut p) != CTL_RESULT_SUCCESS {
                    continue;
                }
                // Wersja sterownika: 4 x 16 bitów (31.0.101.2111).
                let v = p.driver_version;
                let driver = format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF, v & 0xFFFF);
                out.push(Device {
                    handle: h,
                    name: text(&p.name),
                    pci_device_id: p.pci_device_id,
                    integrated: p.graphics_adapter_properties & ADAPTER_FLAG_INTEGRATED != 0,
                    driver,
                });
            }
            Ok(out)
        }
    }

    /// Ustawienia 3D obsługiwane przez kartę.
    pub fn features(&self, device: Handle) -> Result<Vec<FeatureDetails>, String> {
        // SAFETY: jak w `load`; tablica na tyle pozycji, ile podał sterownik.
        unsafe {
            let caps: CapsFn = *self
                .lib
                .get(b"ctlGetSupported3DCapabilities\0")
                .map_err(|e| format!("ctlGetSupported3DCapabilities: {e}"))?;
            let mut c: FeatureCaps = std::mem::zeroed();
            c.size = std::mem::size_of::<FeatureCaps>() as u32;
            let r = caps(device, &mut c);
            if r != CTL_RESULT_SUCCESS {
                return Err(describe(r));
            }
            let mut list: Vec<FeatureDetails> = vec![std::mem::zeroed(); c.count as usize];
            c.details = list.as_mut_ptr();
            let r = caps(device, &mut c);
            if r != CTL_RESULT_SUCCESS {
                return Err(describe(r));
            }
            list.truncate(c.count as usize);
            Ok(list)
        }
    }

    /// Odczyt (`set == None`) albo zapis ustawienia 3D. `app` - profil gry
    /// (nazwa pliku .exe); `None` - ustawienie globalne.
    pub fn feature(
        &self,
        device: Handle,
        feature: u32,
        value_type: u32,
        app: Option<&str>,
        set: Option<Property>,
    ) -> Result<Property, u32> {
        // SAFETY: jak w `load`; nazwa gry żyje do końca wywołania.
        unsafe {
            let getset: GetSetFn = *self
                .lib
                .get(b"ctlGetSet3DFeature\0")
                .map_err(|_| CTL_RESULT_ERROR_UNSUPPORTED_FEATURE)?;
            let mut name: Vec<u8> = app.map(|a| a.as_bytes().to_vec()).unwrap_or_default();
            let len = name.len().min(127) as i8;
            name.push(0);
            let mut g = FeatureGetSet {
                size: std::mem::size_of::<FeatureGetSet>() as u32,
                version: 0,
                feature,
                app_name: if app.is_some() { name.as_mut_ptr() as *mut c_char } else { null_mut() },
                app_name_len: if app.is_some() { len } else { 0 },
                set: set.is_some(),
                value_type,
                value: set.unwrap_or_default(),
                custom_value_size: 0,
                custom_value: null_mut(),
            };
            let r = getset(device, &mut g);
            if r == CTL_RESULT_SUCCESS || r == CTL_RESULT_SUCCESS_STILL_OPEN_BY_ANOTHER_CALLER {
                Ok(g.value)
            } else {
                Err(r)
            }
        }
    }

    /// Funkcja biblioteki o sygnaturze `F`.
    fn func<F: Copy>(&self, name: &[u8]) -> Result<F, u32> {
        // SAFETY: wywołujący podaje typ zgodny z igcl_api.h.
        unsafe { self.lib.get::<F>(name).map(|f| *f).map_err(|_| CTL_RESULT_ERROR_UNSUPPORTED_FEATURE) }
    }

    /// Wyjścia ekranu karty (także niepodłączone - te zgłaszają błędy).
    pub fn displays(&self, device: Handle) -> Vec<Handle> {
        let Ok(f) = self.func::<DisplaysFn>(b"ctlEnumerateDisplayOutputs\0") else {
            return Vec::new();
        };
        let mut count = 0u32;
        // SAFETY: najpierw liczba, potem tablica tej długości.
        unsafe {
            if f(device, &mut count, null_mut()) != CTL_RESULT_SUCCESS || count == 0 {
                return Vec::new();
            }
            let mut list = vec![null_mut(); count as usize];
            if f(device, &mut count, list.as_mut_ptr()) != CTL_RESULT_SUCCESS {
                return Vec::new();
            }
            list.truncate(count as usize);
            list.retain(|h| !h.is_null());
            list
        }
    }

    pub fn arc_sync_info(&self, display: Handle) -> Result<ArcSyncMonitor, u32> {
        let f = self.func::<ArcInfoFn>(b"ctlGetIntelArcSyncInfoForMonitor\0")?;
        let mut p = ArcSyncMonitor {
            size: std::mem::size_of::<ArcSyncMonitor>() as u32,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut p) }).map(|_| p)
    }

    pub fn arc_sync_profile(&self, display: Handle) -> Result<ArcSyncProfile, u32> {
        let f = self.func::<ArcProfileFn>(b"ctlGetIntelArcSyncProfile\0")?;
        let mut p = ArcSyncProfile {
            size: std::mem::size_of::<ArcSyncProfile>() as u32,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut p) }).map(|_| p)
    }

    /// Ustawia profil Arc Sync; dla `CUSTOM` także zakres odświeżania i
    /// szybkość zmian z `p`.
    pub fn set_arc_sync_profile(&self, display: Handle, p: ArcSyncProfile) -> Result<(), u32> {
        let f = self.func::<ArcProfileFn>(b"ctlSetIntelArcSyncProfile\0")?;
        let mut p = ArcSyncProfile {
            size: std::mem::size_of::<ArcSyncProfile>() as u32,
            version: 0,
            ..p
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut p) })
    }

    fn flag_caps(&self, name: &[u8], handle: Handle) -> Result<u32, u32> {
        let f = self.func::<FlagCapsFn>(name)?;
        let mut c = FlagCaps {
            size: std::mem::size_of::<FlagCaps>() as u32,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(handle, &mut c) }).map(|_| c.flags)
    }

    /// Obsługiwane rodzaje skalowania ekranu (maska `scaling::*`).
    pub fn scaling_caps(&self, display: Handle) -> Result<u32, u32> {
        self.flag_caps(b"ctlGetSupportedScalingCapability\0", display)
    }

    pub fn scaling(&self, display: Handle) -> Result<Scaling, u32> {
        let f = self.func::<ScalingFn>(b"ctlGetCurrentScaling\0")?;
        let mut s = Scaling {
            size: std::mem::size_of::<Scaling>() as u32,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut s) }).map(|_| s)
    }

    pub fn set_scaling(&self, display: Handle, kind: u32) -> Result<(), u32> {
        let f = self.func::<ScalingFn>(b"ctlSetCurrentScaling\0")?;
        let mut s = Scaling {
            size: std::mem::size_of::<Scaling>() as u32,
            enable: true,
            kind,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut s) })
    }

    /// Retro scaling karty: (obsługiwane rodzaje, włączone, rodzaj).
    pub fn retro_scaling(&self, device: Handle) -> Result<(u32, bool, u32), u32> {
        let caps = self.flag_caps(b"ctlGetSupportedRetroScalingCapability\0", device)?;
        let f = self.func::<RetroFn>(b"ctlGetSetRetroScaling\0")?;
        let mut s = RetroScaling {
            size: std::mem::size_of::<RetroScaling>() as u32,
            get: true,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(device, &mut s) }).map(|_| (caps, s.enable, s.kind))
    }

    pub fn set_retro_scaling(&self, device: Handle, enable: bool, kind: u32) -> Result<(), u32> {
        let f = self.func::<RetroFn>(b"ctlGetSetRetroScaling\0")?;
        let mut s = RetroScaling {
            size: std::mem::size_of::<RetroScaling>() as u32,
            get: false,
            enable,
            kind,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(device, &mut s) })
    }

    /// Filtry wyostrzania ekranu z zakresami.
    pub fn sharpness_caps(&self, display: Handle) -> Result<Vec<SharpnessFilter>, u32> {
        let f = self.func::<SharpCapsFn>(b"ctlGetSharpnessCaps\0")?;
        let mut c = SharpnessCaps {
            size: std::mem::size_of::<SharpnessCaps>() as u32,
            version: 0,
            flags: 0,
            count: 0,
            filters: null_mut(),
        };
        // SAFETY: najpierw liczba filtrów, potem tablica tej długości.
        unsafe {
            check(f(display, &mut c))?;
            let mut list = vec![SharpnessFilter::default(); c.count as usize];
            if list.is_empty() {
                return Ok(list);
            }
            c.filters = list.as_mut_ptr();
            check(f(display, &mut c))?;
            list.truncate(c.count as usize);
            Ok(list)
        }
    }

    pub fn sharpness(&self, display: Handle) -> Result<Sharpness, u32> {
        let f = self.func::<SharpFn>(b"ctlGetCurrentSharpness\0")?;
        let mut s = Sharpness {
            size: std::mem::size_of::<Sharpness>() as u32,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut s) }).map(|_| s)
    }

    pub fn set_sharpness(&self, display: Handle, enable: bool, kind: u32, intensity: f32) -> Result<(), u32> {
        let f = self.func::<SharpFn>(b"ctlSetCurrentSharpness\0")?;
        let mut s = Sharpness {
            size: std::mem::size_of::<Sharpness>() as u32,
            version: 0,
            enable,
            kind,
            intensity,
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut s) })
    }

    /// Obsługiwane funkcje oszczędzania energii ekranu (maska `power::*`).
    pub fn power_caps(&self, display: Handle) -> Result<u32, u32> {
        self.flag_caps(b"ctlGetPowerOptimizationCaps\0", display)
    }

    /// Stan funkcji oszczędzania energii (przy zasilaniu z sieci).
    pub fn power(&self, display: Handle, feature: u32) -> Result<bool, u32> {
        self.power_get(display, feature).map(|p| p.enable)
    }

    fn power_get(&self, display: Handle, feature: u32) -> Result<PowerSetting, u32> {
        let f = self.func::<PowerFn>(b"ctlGetPowerOptimizationSetting\0")?;
        let mut p = PowerSetting {
            size: std::mem::size_of::<PowerSetting>() as u32,
            feature,
            ..Default::default()
        };
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut p) }).map(|_| p)
    }

    /// Włącza / wyłącza funkcję, zachowując jej pozostałe dane (np. poziom DPST).
    pub fn set_power(&self, display: Handle, feature: u32, enable: bool) -> Result<(), u32> {
        let mut p = self.power_get(display, feature)?;
        let f = self.func::<PowerFn>(b"ctlSetPowerOptimizationSetting\0")?;
        p.size = std::mem::size_of::<PowerSetting>() as u32;
        p.feature = feature;
        p.enable = enable;
        // SAFETY: struktura z polem Size.
        check(unsafe { f(display, &mut p) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_header() {
        // Rozmiary z układu C (MSVC x64) struktur z igcl_api.h.
        assert_eq!(std::mem::size_of::<InitArgs>(), 36);
        assert_eq!(std::mem::size_of::<Property>(), 8);
        assert_eq!(std::mem::size_of::<PropertyInfo>(), 24);
        assert_eq!(std::mem::size_of::<FeatureGetSet>(), 56);
        assert_eq!(std::mem::size_of::<FeatureDetails>(), 72);
        assert_eq!(std::mem::size_of::<FeatureCaps>(), 24);
        assert_eq!(std::mem::size_of::<ArcSyncMonitor>(), 24);
        assert_eq!(std::mem::size_of::<ArcSyncProfile>(), 28);
        assert_eq!(std::mem::size_of::<FlagCaps>(), 12);
        assert_eq!(std::mem::size_of::<Scaling>(), 28);
        assert_eq!(std::mem::size_of::<RetroScaling>(), 12);
        assert_eq!(std::mem::size_of::<SharpnessFilter>(), 20);
        assert_eq!(std::mem::size_of::<SharpnessCaps>(), 24);
        assert_eq!(std::mem::size_of::<Sharpness>(), 16);
        assert_eq!(std::mem::size_of::<PowerSetting>(), 44);
    }

    /// Na komputerze z kartą Intel: `cargo test igcl_probe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn igcl_probe() {
        let igcl = Igcl::load().expect("IGCL");
        for d in &igcl.devices {
            println!(
                "{} | dev 0x{:04X} | zintegrowana {} | sterownik {}",
                d.name, d.pci_device_id, d.integrated, d.driver
            );
            match igcl.features(d.handle) {
                Ok(list) => {
                    for f in list {
                        let current = igcl.feature(d.handle, f.feature, f.value_type, None, None);
                        println!(
                            "  funkcja {:2} typ {} per-app {} misc 0x{:x} info {:?} -> {:?}",
                            f.feature, f.value_type, f.per_app, f.misc, f.info.raw, current
                        );
                    }
                }
                Err(e) => println!("  3D: {e}"),
            }
        }
    }
}
