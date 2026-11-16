//! Ekrany podłączone do karty Nvidia: tryb, kolor, HDR, Adaptive Sync
//! i rozdzielczości niestandardowe - wyłącznie przez publiczne NVAPI.
//!
//! Każda zmiana ma drogę powrotu: przed zapisem zapamiętujemy poprzedni stan,
//! a interfejs przez 15 s pyta, czy zachować zmianę (jak Windows). Brak
//! odpowiedzi = powrót do poprzedniego stanu.

use super::api::{self, check, need, Api, Handle};
use std::ffi::c_void;
use std::mem::size_of;

// ------------------------------ struktury ----------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingExt {
    flag: u32,
    rr: u16,
    rrx1k: u32,
    aspect: u32,
    rep: u16,
    status: u32,
    name: [u8; 40],
}

impl Default for TimingExt {
    fn default() -> Self {
        Self {
            flag: 0,
            rr: 0,
            rrx1k: 0,
            aspect: 0,
            rep: 0,
            status: 0,
            name: [0; 40],
        }
    }
}

/// `NV_TIMING`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Timing {
    pub h_visible: u16,
    pub h_border: u16,
    pub h_front_porch: u16,
    pub h_sync_width: u16,
    pub h_total: u16,
    pub h_sync_pol: u8,
    pub v_visible: u16,
    pub v_border: u16,
    pub v_front_porch: u16,
    pub v_sync_width: u16,
    pub v_total: u16,
    pub v_sync_pol: u8,
    pub interlaced: u16,
    /// Zegar pikseli w 10 kHz.
    pub pclk: u32,
    etc: TimingExt,
}

impl Timing {
    /// Odświeżanie w mHz policzone z taktowania.
    pub fn refresh_mhz(&self) -> u32 {
        if self.etc.rrx1k != 0 {
            return self.etc.rrx1k;
        }
        let total = self.h_total as u64 * self.v_total as u64;
        if total == 0 {
            return 0;
        }
        ((self.pclk as u64 * 10_000 * 1000) / total) as u32
    }
}

/// `NV_DISPLAYCONFIG_PATH_ADVANCED_TARGET_INFO_V1`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Advanced {
    version: u32,
    rotation: u32,
    scaling: u32,
    refresh_1k: u32,
    flags: u32,
    connector: u32,
    tv_format: u32,
    timing_override: u32,
    timing: Timing,
}

/// `NV_DISPLAYCONFIG_PATH_TARGET_INFO_V2`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct TargetInfo {
    display_id: u32,
    details: *mut Advanced,
    target_id: u32,
}

/// `NV_DISPLAYCONFIG_SOURCE_MODE_INFO_V1`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct SourceMode {
    width: u32,
    height: u32,
    color_depth: u32,
    color_format: u32,
    x: i32,
    y: i32,
    spanning: u32,
    flags: u32,
}

/// `NV_DISPLAYCONFIG_PATH_INFO_V2`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct PathInfo {
    version: u32,
    source_id: u32,
    target_count: u32,
    targets: *mut TargetInfo,
    source: *mut SourceMode,
    flags: u32,
    os_adapter: *mut c_void,
}

/// `NV_GPU_DISPLAYIDS`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GpuDisplayId {
    version: u32,
    connector: i32,
    display_id: u32,
    flags: u32,
}

/// `NV_COLOR_DATA_V5`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct ColorData {
    version: u32,
    size: u16,
    cmd: u8,
    data: ColorInner,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct ColorInner {
    format: u8,
    colorimetry: u8,
    dynamic_range: u8,
    bpc: u32,
    policy: u32,
    depth: u32,
}

/// `NV_HDR_COLOR_DATA_V2`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct HdrColor {
    version: u32,
    cmd: u32,
    mode: u32,
    static_metadata_id: u32,
    mastering: [u16; 12],
    format: u32,
    dynamic_range: u32,
    bpc: u32,
}

/// `NV_HDR_CAPABILITIES_V3`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HdrCaps {
    version: u32,
    flags: u32,
    static_metadata_id: u32,
    display: [u16; 11],
    dolby_bits: u32,
    dolby: [u16; 10],
    hdr10plus: u16,
}

/// `NV_GET_ADAPTIVE_SYNC_DATA_V1`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GetSync {
    version: u32,
    max_frame_interval: u32,
    flags: u32,
    last_flip_refresh_count: u32,
    last_flip_time: u64,
    flip_token: u32,
    reserved: [u32; 3],
}

/// `NV_SET_ADAPTIVE_SYNC_DATA_V1` (wersja 2).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SetSync {
    version: u32,
    max_frame_interval: u32,
    flags: u32,
    reserved1: u32,
    max_frame_interval_ns: u64,
    reserved: [u32; 4],
}

/// `NV_GET_VRR_INFO_V1`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VrrInfo {
    version: u32,
    flags: u32,
    reserved: [u32; 4],
}

/// `NV_VIEWPORTF`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Viewport {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// `NV_CUSTOM_DISPLAY`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct CustomDisplay {
    version: u32,
    pub width: u32,
    pub height: u32,
    depth: u32,
    color_format: u32,
    src_partition: Viewport,
    x_ratio: f32,
    y_ratio: f32,
    pub timing: Timing,
    hw_mode_set_only: u32,
}

/// `NV_TIMING_FLAG`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TimingFlag {
    interlaced: u32,
    tv_format: u32,
    scaling: u32,
}

/// `NV_TIMING_INPUT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TimingInput {
    version: u32,
    width: u32,
    height: u32,
    rr: f32,
    flag: TimingFlag,
    kind: u32,
}

/// `NV_EDID_DATA_V2`.
#[repr(C)]
struct EdidData {
    version: u32,
    data: *mut u8,
    size: u32,
    reserved: [u32; 8],
}

// Rozmiary z nvapi.h - wchodzą w numer wersji struktury.
const _: () = assert!(size_of::<TimingExt>() == 64);
const _: () = assert!(size_of::<Timing>() == 96);
const _: () = assert!(size_of::<Advanced>() == 128);
const _: () = assert!(size_of::<TargetInfo>() == 24);
const _: () = assert!(size_of::<SourceMode>() == 32);
const _: () = assert!(size_of::<PathInfo>() == 48);
const _: () = assert!(size_of::<GpuDisplayId>() == 16);
const _: () = assert!(size_of::<ColorData>() == 24);
const _: () = assert!(size_of::<HdrColor>() == 52);
const _: () = assert!(size_of::<HdrCaps>() == 64);
const _: () = assert!(size_of::<GetSync>() == 40);
const _: () = assert!(size_of::<SetSync>() == 40);
const _: () = assert!(size_of::<VrrInfo>() == 24);
const _: () = assert!(size_of::<CustomDisplay>() == 144);
const _: () = assert!(size_of::<TimingInput>() == 32);
const _: () = assert!(size_of::<EdidData>() == 56);

const PATH_INFO_VER: u32 = api::ver::<PathInfo>(2);
const ADVANCED_VER: u32 = api::ver::<Advanced>(1);
const DISPLAY_IDS_VER: u32 = api::ver::<GpuDisplayId>(3);
const COLOR_VER: u32 = api::ver::<ColorData>(5);
const HDR_COLOR_VER: u32 = api::ver::<HdrColor>(2);
const HDR_CAPS_VER: u32 = api::ver::<HdrCaps>(3);
const GET_SYNC_VER: u32 = api::ver::<GetSync>(1);
const SET_SYNC_VER: u32 = api::ver::<SetSync>(2);
const VRR_VER: u32 = api::ver::<VrrInfo>(1);
const CUSTOM_VER: u32 = api::ver::<CustomDisplay>(1);
const TIMING_INPUT_VER: u32 = api::ver::<TimingInput>(1);
const EDID_VER: u32 = api::ver::<EdidData>(2);

/// Flagi `NvAPI_DISP_SetDisplayConfig`.
pub const VALIDATE_ONLY: u32 = 0x1;
pub const SAVE_TO_PERSISTENCE: u32 = 0x2;

/// `NV_TIMING_OVERRIDE_AUTO` / `_CVT_RB`.
const TIMING_OVERRIDE_AUTO: u32 = 1;
pub const TIMING_CVT_RB: u32 = 6;

// ------------------------------- ekrany ------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub id: u32,
    /// Nazwa Windows, np. "\\.\DISPLAY1".
    pub gdi_name: String,
    /// Nazwa monitora z EDID, jeśli jest.
    pub monitor: Option<String>,
    pub connector: &'static str,
    pub active: bool,
    pub primary: bool,
    /// Natywna rozdzielczość z EDID (pierwszy opis taktowania).
    pub native: Option<(u32, u32)>,
    /// Maksymalne odświeżanie w Hz z deskryptora zakresów EDID (0xFD).
    pub max_refresh_hz: Option<u32>,
}

impl Display {
    pub fn label(&self) -> String {
        let name = self
            .monitor
            .clone()
            .unwrap_or_else(|| self.connector.to_owned());
        let gdi = self.gdi_name.trim_start_matches("\\\\.\\");
        if gdi.is_empty() {
            name
        } else {
            format!("{name} ({gdi})")
        }
    }
}

fn connector_name(raw: i32) -> &'static str {
    match raw {
        1 => "VGA",
        2 => "Component",
        3 => "S-Video",
        4 => "HDMI",
        5 => "DVI",
        6 => "LVDS",
        7 => "DisplayPort",
        8 => "Composite",
        _ => "?",
    }
}

/// Monitory podłączone do kart Nvidia.
pub fn displays(api: &Api) -> Result<Vec<Display>, String> {
    let enum_gpus = need(api.enum_gpus)?;
    let connected = need(api.connected_display_ids)?;
    let mut gpus: [Handle; 64] = [std::ptr::null_mut(); 64];
    let mut gpu_count = 0u32;
    // SAFETY: tablica NVAPI_MAX_PHYSICAL_GPUS uchwytów.
    check(unsafe { enum_gpus(gpus.as_mut_ptr(), &mut gpu_count) })?;

    let names = gdi_names(api);
    let primary = api.primary_display_id.and_then(|f| {
        let mut id = 0u32;
        // SAFETY: wskaźnik na zmienną lokalną.
        (unsafe { f(&mut id) } == api::OK).then_some(id)
    });

    let mut out = Vec::new();
    for &gpu in gpus.iter().take(gpu_count as usize) {
        let mut count = 0u32;
        // SAFETY: najpierw pytamy tylko o liczbę ekranów.
        if unsafe { connected(gpu, std::ptr::null_mut(), &mut count, 0) } != api::OK || count == 0 {
            continue;
        }
        let mut ids = vec![
            GpuDisplayId {
                version: DISPLAY_IDS_VER,
                ..Default::default()
            };
            count as usize
        ];
        // SAFETY: tablica `count` struktur z poprawną wersją.
        if unsafe { connected(gpu, ids.as_mut_ptr() as *mut c_void, &mut count, 0) } != api::OK {
            continue;
        }
        for d in ids.iter().take(count as usize) {
            let edid = edid(api, d.display_id);
            out.push(Display {
                id: d.display_id,
                gdi_name: names
                    .iter()
                    .find(|(id, _)| *id == d.display_id)
                    .map(|(_, n)| n.clone())
                    .unwrap_or_default(),
                monitor: edid.as_deref().and_then(edid_name),
                connector: connector_name(d.connector),
                active: d.flags & (1 << 2) != 0,
                primary: primary == Some(d.display_id),
                native: edid.as_deref().and_then(edid_native),
                max_refresh_hz: edid.as_deref().and_then(edid_max_refresh),
            });
        }
    }
    // Aktywne ekrany najpierw, główny na początku.
    out.sort_by_key(|d| (!d.primary, !d.active, d.id));
    Ok(out)
}

/// Pary (identyfikator ekranu, nazwa Windows) dla aktywnych ekranów.
fn gdi_names(api: &Api) -> Vec<(u32, String)> {
    let (Some(enum_handle), Some(name_of), Some(id_of)) = (
        api.enum_display_handle,
        api.display_name,
        api.display_id_by_name,
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in 0..16 {
        let mut handle: Handle = std::ptr::null_mut();
        // SAFETY: wskaźnik na zmienną lokalną.
        if unsafe { enum_handle(index, &mut handle) } != api::OK {
            break;
        }
        let mut name = [0u8; 64];
        // SAFETY: bufor NvAPI_ShortString.
        if unsafe { name_of(handle, name.as_mut_ptr()) } != api::OK {
            continue;
        }
        let mut id = 0u32;
        // SAFETY: nazwa zakończona zerem z poprzedniego wywołania.
        if unsafe { id_of(name.as_ptr(), &mut id) } == api::OK {
            out.push((id, api::from_short(&name)));
        }
    }
    out
}

fn edid(api: &Api, display_id: u32) -> Option<Vec<u8>> {
    let f = api.edid_data?;
    let mut buffer = vec![0u8; 1024];
    let mut data = EdidData {
        version: EDID_VER,
        data: buffer.as_mut_ptr(),
        size: buffer.len() as u32,
        reserved: [0; 8],
    };
    let mut flag = 0u32; // NV_EDID_FLAG_DEFAULT
                         // SAFETY: bufor o rozmiarze podanym w `size`.
    let status = unsafe {
        f(
            display_id,
            &mut data as *mut EdidData as *mut c_void,
            &mut flag,
        )
    };
    if status != api::OK || data.size < 128 {
        return None;
    }
    buffer.truncate((data.size as usize).min(1024));
    Some(buffer)
}

/// Nazwa monitora z deskryptora 0xFC bloku podstawowego EDID.
pub fn edid_name(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 {
        return None;
    }
    for start in [54usize, 72, 90, 108] {
        let d = &edid[start..start + 18];
        if d[0] == 0 && d[1] == 0 && d[3] == 0xFC {
            let text: String = d[5..18]
                .iter()
                .take_while(|&&c| c != 0x0A && c != 0)
                .map(|&c| c as char)
                .collect();
            let text = text.trim().to_owned();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Rozdzielczość natywna z pierwszego opisu taktowania (DTD) EDID.
pub fn edid_max_refresh(edid: &[u8]) -> Option<u32> {
    if edid.len() < 128 {
        return None;
    }
    for start in [54usize, 72, 90, 108] {
        let d = &edid[start..start + 18];
        if d[0] == 0 && d[1] == 0 && d[3] == 0xFD {
            // Bajt 4, bit 1: do maksimum pionowego dodaje się 255 (EDID 1.4).
            let offset = if d[4] & 0x02 != 0 { 255 } else { 0 };
            let max = d[6] as u32 + offset;
            return (max > 0).then_some(max);
        }
    }
    None
}

/// Rozdzielczość natywna z pierwszego opisu taktowania (DTD) EDID.
pub fn edid_native(edid: &[u8]) -> Option<(u32, u32)> {
    if edid.len() < 72 {
        return None;
    }
    let d = &edid[54..72];
    if d[0] == 0 && d[1] == 0 {
        return None;
    }
    let w = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
    let h = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
    (w > 0 && h > 0).then_some((w, h))
}

// --------------------------- konfiguracja ekranów --------------------------

/// Konfiguracja ekranów z buforami należącymi do nas (NVAPI wypełnia
/// struktury przez wskaźniki, które tu trzymamy).
pub struct Config {
    paths: Vec<PathInfo>,
    sources: Vec<SourceMode>,
    targets: Vec<Vec<TargetInfo>>,
    details: Vec<Vec<Advanced>>,
}

/// Bieżący tryb jednego ekranu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Odświeżanie w mHz (59 940 = 59,94 Hz).
    pub refresh_mhz: u32,
    pub rotation: u32,
    pub scaling: u32,
}

impl Config {
    pub fn read(api: &Api) -> Result<Self, String> {
        let get = need(api.get_display_config)?;
        let mut count = 0u32;
        // SAFETY: pierwsze wywołanie podaje tylko liczbę ścieżek.
        check(unsafe { get(&mut count, std::ptr::null_mut()) })?;
        let empty = PathInfo {
            version: PATH_INFO_VER,
            source_id: 0,
            target_count: 0,
            targets: std::ptr::null_mut(),
            source: std::ptr::null_mut(),
            flags: 0,
            os_adapter: std::ptr::null_mut(),
        };
        let mut cfg = Config {
            paths: vec![empty; count as usize],
            sources: Vec::new(),
            targets: Vec::new(),
            details: Vec::new(),
        };
        // SAFETY: tablica `count` struktur z poprawną wersją; drugie
        // wywołanie podaje liczbę ekranów w każdej ścieżce.
        check(unsafe { get(&mut count, cfg.paths.as_mut_ptr() as *mut c_void) })?;
        cfg.paths.truncate(count as usize);
        for i in 0..cfg.paths.len() {
            let n = cfg.paths[i].target_count as usize;
            cfg.sources.push(SourceMode::default());
            cfg.details.push(
                (0..n)
                    .map(|_| Advanced {
                        version: ADVANCED_VER,
                        rotation: 0,
                        scaling: 0,
                        refresh_1k: 0,
                        flags: 0,
                        connector: 0,
                        tv_format: 0,
                        timing_override: 0,
                        timing: Timing::default(),
                    })
                    .collect(),
            );
            cfg.targets.push(
                (0..n)
                    .map(|_| TargetInfo {
                        display_id: 0,
                        details: std::ptr::null_mut(),
                        target_id: 0,
                    })
                    .collect(),
            );
        }
        cfg.link();
        // SAFETY: wszystkie wskaźniki prowadzą do buforów w `cfg`.
        check(unsafe { get(&mut count, cfg.paths.as_mut_ptr() as *mut c_void) })?;
        Ok(cfg)
    }

    /// Ustawia wskaźniki w strukturach na nasze bufory.
    fn link(&mut self) {
        for i in 0..self.paths.len() {
            for (t, d) in self.targets[i].iter_mut().zip(self.details[i].iter_mut()) {
                t.details = d;
            }
            self.paths[i].targets = self.targets[i].as_mut_ptr();
            self.paths[i].source = &mut self.sources[i];
        }
    }

    /// Głęboka kopia (do przywrócenia poprzedniego stanu).
    pub fn duplicate(&self) -> Self {
        let mut copy = Config {
            paths: self.paths.clone(),
            sources: self.sources.clone(),
            targets: self.targets.clone(),
            details: self.details.clone(),
        };
        copy.link();
        copy
    }

    fn locate(&self, display_id: u32) -> Option<(usize, usize)> {
        self.targets.iter().enumerate().find_map(|(i, ts)| {
            ts.iter()
                .position(|t| t.display_id == display_id)
                .map(|j| (i, j))
        })
    }

    pub fn mode(&self, display_id: u32) -> Option<Mode> {
        let (i, j) = self.locate(display_id)?;
        let s = &self.sources[i];
        let d = &self.details[i][j];
        Some(Mode {
            width: s.width,
            height: s.height,
            refresh_mhz: if d.refresh_1k != 0 {
                d.refresh_1k
            } else {
                d.timing.refresh_mhz()
            },
            rotation: d.rotation,
            scaling: d.scaling,
        })
    }

    /// Zmienia tryb jednego ekranu w kopii konfiguracji. Przy zmianie
    /// rozdzielczości lub odświeżania taktowanie wybiera sterownik.
    pub fn with_mode(&self, display_id: u32, mode: Mode) -> Option<Self> {
        let (i, j) = self.locate(display_id)?;
        let mut cfg = self.duplicate();
        let old = self.mode(display_id)?;
        let s = &mut cfg.sources[i];
        s.width = mode.width;
        s.height = mode.height;
        let d = &mut cfg.details[i][j];
        d.refresh_1k = mode.refresh_mhz;
        d.rotation = mode.rotation;
        d.scaling = mode.scaling;
        if (old.width, old.height, old.refresh_mhz) != (mode.width, mode.height, mode.refresh_mhz) {
            d.timing = Timing::default();
            d.timing_override = TIMING_OVERRIDE_AUTO;
        }
        cfg.link();
        Some(cfg)
    }

    pub fn apply(&mut self, api: &Api, flags: u32) -> Result<(), String> {
        let set = need(api.set_display_config)?;
        self.link();
        // SAFETY: wszystkie wskaźniki prowadzą do buforów w `self`.
        check(unsafe {
            set(
                self.paths.len() as u32,
                self.paths.as_mut_ptr() as *mut c_void,
                flags,
            )
        })
    }

    /// Czy sterownik przyjąłby tę konfigurację (nic nie zmienia na ekranie).
    pub fn validate(&mut self, api: &Api) -> bool {
        self.apply(api, VALIDATE_ONLY).is_ok()
    }
}

// --------------------------------- kolor -----------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    /// 0 RGB, 1 YCbCr422, 2 YCbCr444, 3 YCbCr420.
    pub format: u8,
    /// 0 pełny (VESA), 1 ograniczony (CEA), 0xFF auto.
    pub dynamic_range: u8,
    /// 1 = 6, 2 = 8, 3 = 10, 4 = 12, 5 = 16 bitów na kanał.
    pub bpc: u32,
    /// 0 = ustawienia Nvidia (użytkownika), 1 = domyślne.
    pub policy: u32,
}

const COLOR_GET: u8 = 1;
const COLOR_SET: u8 = 2;
const COLOR_IS_SUPPORTED: u8 = 3;

fn color_call(api: &Api, display_id: u32, cmd: u8, color: Option<Color>) -> Result<Color, String> {
    let f = need(api.color_control)?;
    let mut c = ColorData {
        version: COLOR_VER,
        size: size_of::<ColorData>() as u16,
        cmd,
        data: ColorInner {
            format: 0xFE,
            colorimetry: 0xFE,
            dynamic_range: 0xFF,
            bpc: 0,
            policy: 1,
            depth: 0,
        },
    };
    if let Some(color) = color {
        c.data.format = color.format;
        c.data.dynamic_range = color.dynamic_range;
        c.data.bpc = color.bpc;
        c.data.policy = color.policy;
        // Kolorymetria zgodna z formatem (RGB albo YCC709).
        c.data.colorimetry = if color.format == 0 { 0 } else { 2 };
        if color.policy == 1 {
            c.data.format = 0xFE;
            c.data.colorimetry = 0xFE;
            c.data.dynamic_range = 0xFF;
            c.data.bpc = 0;
        }
    }
    // SAFETY: struktura z poprawną wersją i rozmiarem.
    check(unsafe { f(display_id, &mut c as *mut ColorData as *mut c_void) })?;
    Ok(Color {
        format: c.data.format,
        dynamic_range: c.data.dynamic_range,
        bpc: c.data.bpc,
        policy: c.data.policy,
    })
}

pub fn color(api: &Api, display_id: u32) -> Result<Color, String> {
    color_call(api, display_id, COLOR_GET, None)
}

pub fn color_supported(api: &Api, display_id: u32, color: Color) -> bool {
    color.policy == 1 || color_call(api, display_id, COLOR_IS_SUPPORTED, Some(color)).is_ok()
}

pub fn set_color(api: &Api, display_id: u32, color: Color) -> Result<(), String> {
    color_call(api, display_id, COLOR_SET, Some(color)).map(|_| ())
}

// ---------------------------------- HDR ------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hdr {
    pub supported: bool,
    /// Bieżący tryb: 0 wyłączony, 2 HDR10 (UHDA), inne - opis w `hdr_mode_name`.
    pub mode: u32,
}

pub fn hdr(api: &Api, display_id: u32) -> Result<Hdr, String> {
    let supported = api.hdr_capabilities.is_some_and(|f| {
        let mut caps = HdrCaps {
            version: HDR_CAPS_VER,
            ..Default::default()
        };
        // SAFETY: struktura z poprawną wersją.
        let status = unsafe { f(display_id, &mut caps as *mut HdrCaps as *mut c_void) };
        status == api::OK && caps.flags & 1 != 0 // isST2084EotfSupported
    });
    let f = need(api.hdr_color_control)?;
    let mut data = HdrColor {
        version: HDR_COLOR_VER,
        cmd: 0, // GET
        ..Default::default()
    };
    // SAFETY: struktura z poprawną wersją.
    check(unsafe { f(display_id, &mut data as *mut HdrColor as *mut c_void) })?;
    Ok(Hdr {
        supported,
        mode: data.mode,
    })
}

pub fn set_hdr(api: &Api, display_id: u32, on: bool) -> Result<(), String> {
    let f = need(api.hdr_color_control)?;
    let mut data = HdrColor {
        version: HDR_COLOR_VER,
        cmd: 1, // SET
        mode: if on { 2 } else { 0 },
        // Domyślne parametry wyświetlacza (sterownik je uzupełnia).
        format: 0xFE,        // NV_COLOR_FORMAT_DEFAULT
        dynamic_range: 0xFF, // AUTO
        bpc: 0,              // DEFAULT
        ..Default::default()
    };
    // SAFETY: struktura z poprawną wersją.
    check(unsafe { f(display_id, &mut data as *mut HdrColor as *mut c_void) })
}

pub fn hdr_mode_name(mode: u32) -> &'static str {
    use crate::i18n::tr;
    match mode {
        0 => tr("wyłączony", "off"),
        2 => "HDR10",
        3 => "EDR",
        4 => "SDR",
        5 => tr("HDR10 (przekazywanie)", "HDR10 (passthrough)"),
        6 => "HDR10 (NB)",
        7 => "Dolby Vision",
        _ => "?",
    }
}

// ------------------------------ G-SYNC / VRR -------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Vrr {
    pub enabled: bool,
    pub possible: bool,
    pub requested: bool,
    pub active_now: bool,
}

pub fn vrr(api: &Api, display_id: u32) -> Option<Vrr> {
    let f = api.vrr_info?;
    let mut info = VrrInfo {
        version: VRR_VER,
        ..Default::default()
    };
    // SAFETY: struktura z poprawną wersją.
    let status = unsafe { f(display_id, &mut info as *mut VrrInfo as *mut c_void) };
    (status == api::OK).then_some(Vrr {
        enabled: info.flags & 1 != 0,
        possible: info.flags & 2 != 0,
        requested: info.flags & 4 != 0,
        active_now: info.flags & 16 != 0,
    })
}

/// Czy Adaptive Sync (G-SYNC Compatible) jest włączone dla ekranu.
pub fn adaptive_sync(api: &Api, display_id: u32) -> Result<bool, String> {
    let f = need(api.get_adaptive_sync)?;
    let mut data = GetSync {
        version: GET_SYNC_VER,
        ..Default::default()
    };
    // SAFETY: struktura z poprawną wersją.
    check(unsafe { f(display_id, &mut data as *mut GetSync as *mut c_void) })?;
    Ok(data.flags & 1 == 0) // bDisableAdaptiveSync
}

pub fn set_adaptive_sync(api: &Api, display_id: u32, enabled: bool) -> Result<(), String> {
    let f = need(api.set_adaptive_sync)?;
    let mut data = SetSync {
        version: SET_SYNC_VER,
        flags: if enabled { 0 } else { 1 },
        ..Default::default()
    };
    // SAFETY: struktura z poprawną wersją.
    check(unsafe { f(display_id, &mut data as *mut SetSync as *mut c_void) })
}

// ------------------------ rozdzielczości niestandardowe ---------------------

pub fn custom_list(api: &Api, display_id: u32) -> Vec<CustomDisplay> {
    let Some(f) = api.enum_custom else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in 0..32 {
        let mut c = CustomDisplay {
            version: CUSTOM_VER,
            ..Default::default()
        };
        // SAFETY: struktura z poprawną wersją.
        if unsafe {
            f(
                display_id,
                index,
                &mut c as *mut CustomDisplay as *mut c_void,
            )
        } != api::OK
        {
            break;
        }
        out.push(c);
    }
    out
}

/// Liczy taktowanie (standard CVT z ograniczonym wygaszaniem, jak w
/// przykładzie Nvidii) i włącza rozdzielczość na próbę.
pub fn custom_try(
    api: &Api,
    display_id: u32,
    width: u32,
    height: u32,
    hz: f32,
) -> Result<CustomDisplay, String> {
    let get_timing = need(api.get_timing)?;
    let try_custom = need(api.try_custom)?;
    let mut input = TimingInput {
        version: TIMING_INPUT_VER,
        width,
        height,
        rr: hz,
        flag: TimingFlag {
            scaling: 1,
            ..Default::default()
        },
        kind: TIMING_CVT_RB,
    };
    let mut c = CustomDisplay {
        version: CUSTOM_VER,
        width,
        height,
        depth: 32,
        color_format: 0, // NV_FORMAT_UNKNOWN -> bieżący
        src_partition: Viewport {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        },
        x_ratio: 1.0,
        y_ratio: 1.0,
        ..Default::default()
    };
    // SAFETY: struktury z poprawnymi wersjami.
    check(unsafe {
        get_timing(
            display_id,
            &mut input as *mut TimingInput as *mut c_void,
            &mut c.timing as *mut Timing as *mut c_void,
        )
    })?;
    let mut ids = [display_id];
    // SAFETY: jeden ekran, jedna struktura.
    check(unsafe {
        try_custom(
            ids.as_mut_ptr(),
            1,
            &mut c as *mut CustomDisplay as *mut c_void,
        )
    })?;
    Ok(c)
}

pub fn custom_keep(api: &Api, display_id: u32) -> Result<(), String> {
    let f = need(api.save_custom)?;
    let mut ids = [display_id];
    // SAFETY: jeden ekran; zapis tylko dla tego wyjścia i tego monitora.
    check(unsafe { f(ids.as_mut_ptr(), 1, 1, 1) })
}

pub fn custom_revert(api: &Api, display_id: u32) -> Result<(), String> {
    let f = need(api.revert_custom)?;
    let mut ids = [display_id];
    // SAFETY: jeden ekran.
    check(unsafe { f(ids.as_mut_ptr(), 1) })
}

pub fn custom_delete(api: &Api, display_id: u32, custom: &CustomDisplay) -> Result<(), String> {
    let f = need(api.delete_custom)?;
    let mut ids = [display_id];
    let mut c = *custom;
    c.version = CUSTOM_VER;
    // SAFETY: jeden ekran, struktura z poprawną wersją.
    check(unsafe {
        f(
            ids.as_mut_ptr(),
            1,
            &mut c as *mut CustomDisplay as *mut c_void,
        )
    })
}

// ------------------------------- listy trybów -------------------------------

/// Typowe rozdzielczości (16:9, 16:10, 21:9, 4:3) - kandydaci sprawdzani
/// przez sterownik (`VALIDATE_ONLY`).
pub const COMMON_RESOLUTIONS: &[(u32, u32)] = &[
    (7680, 4320),
    (5120, 2880),
    (5120, 2160),
    (5120, 1440),
    (3840, 2400),
    (3840, 2160),
    (3840, 1600),
    (3440, 1440),
    (2560, 1600),
    (2560, 1440),
    (2560, 1080),
    (2048, 1536),
    (1920, 1200),
    (1920, 1080),
    (1680, 1050),
    (1600, 1200),
    (1600, 900),
    (1440, 900),
    (1366, 768),
    (1280, 1024),
    (1280, 800),
    (1280, 720),
    (1024, 768),
    (800, 600),
];

/// Typowe częstotliwości odświeżania w mHz.
pub const COMMON_REFRESH_MHZ: &[u32] = &[
    23_976, 24_000, 25_000, 29_970, 30_000, 50_000, 59_940, 60_000, 75_000, 100_000, 119_880,
    120_000, 143_980, 144_000, 165_000, 170_000, 180_000, 200_000, 240_000, 280_000, 300_000,
    360_000, 480_000, 500_000,
];

pub fn rotation_name(r: u32) -> &'static str {
    match r {
        0 => "0°",
        1 => "90°",
        2 => "180°",
        3 => "270°",
        _ => "?",
    }
}

pub fn scaling_name(s: u32) -> &'static str {
    use crate::i18n::tr;
    match s {
        0 => tr("bez zmian", "no change"),
        1 => tr("monitor - pełny ekran", "display - full screen"),
        2 => tr("GPU - pełny ekran", "GPU - full screen"),
        3 => tr("GPU - bez skalowania", "GPU - no scaling"),
        5 => tr("GPU - zachowaj proporcje", "GPU - aspect ratio"),
        6 => tr("monitor - zachowaj proporcje", "display - aspect ratio"),
        7 => tr("monitor - bez skalowania", "display - no scaling"),
        8 => tr("GPU - skalowanie całkowite", "GPU - integer scaling"),
        _ => "?",
    }
}

pub const SCALINGS: &[u32] = &[1, 6, 7, 2, 5, 3, 8];

#[cfg(test)]
mod tests {
    use super::*;

    fn edid_with(name: &str, w: u32, h: u32) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[54] = 0x01; // zegar pikseli != 0 -> DTD
        e[56] = (w & 0xFF) as u8;
        e[58] = ((w >> 8) as u8) << 4;
        e[59] = (h & 0xFF) as u8;
        e[61] = ((h >> 8) as u8) << 4;
        let d = &mut e[72..90];
        d[3] = 0xFC;
        for (i, b) in name.bytes().enumerate() {
            d[5 + i] = b;
        }
        d[5 + name.len()] = 0x0A;
        e
    }

    #[test]
    fn reads_monitor_name_and_native_mode_from_edid() {
        let e = edid_with("LG ULTRAGEAR", 2560, 1440);
        assert_eq!(edid_name(&e).as_deref(), Some("LG ULTRAGEAR"));
        assert_eq!(edid_native(&e), Some((2560, 1440)));
        assert_eq!(edid_name(&e[..64]), None);
        assert_eq!(edid_native(&[0u8; 128]), None);
    }

    #[test]
    fn reads_refresh_limit_from_range_descriptor() {
        let mut e = vec![0u8; 128];
        let d = &mut e[90..108];
        d[3] = 0xFD;
        d[5] = 48;
        d[6] = 144;
        assert_eq!(edid_max_refresh(&e), Some(144));
        // Monitor 280 Hz: 25 + 255 (flaga przesunięcia w bajcie 4).
        e[90 + 4] = 0x02;
        e[90 + 6] = 25;
        assert_eq!(edid_max_refresh(&e), Some(280));
        assert_eq!(edid_max_refresh(&[0u8; 128]), None);
    }

    #[test]
    fn refresh_from_timing() {
        let t = Timing {
            h_total: 2720,
            v_total: 1481,
            pclk: 24_168, // 241,68 MHz
            ..Default::default()
        };
        // 241,68 MHz / (2720 * 1481) = 59,995 Hz
        assert!((t.refresh_mhz() as i32 - 59_995).abs() < 5);
    }

    #[test]
    fn config_duplicate_points_to_own_buffers() {
        let mut cfg = Config {
            paths: vec![PathInfo {
                version: PATH_INFO_VER,
                source_id: 0,
                target_count: 1,
                targets: std::ptr::null_mut(),
                source: std::ptr::null_mut(),
                flags: 0,
                os_adapter: std::ptr::null_mut(),
            }],
            sources: vec![SourceMode {
                width: 1920,
                height: 1080,
                ..Default::default()
            }],
            targets: vec![vec![TargetInfo {
                display_id: 7,
                details: std::ptr::null_mut(),
                target_id: 0,
            }]],
            details: vec![vec![Advanced {
                version: ADVANCED_VER,
                rotation: 0,
                scaling: 0,
                refresh_1k: 60_000,
                flags: 0,
                connector: 0,
                tv_format: 0,
                timing_override: 0,
                timing: Timing::default(),
            }]],
        };
        cfg.link();
        let new = cfg
            .with_mode(
                7,
                Mode {
                    width: 1280,
                    height: 720,
                    refresh_mhz: 60_000,
                    rotation: 0,
                    scaling: 2,
                },
            )
            .unwrap();
        // Oryginał bez zmian, kopia ze swoimi wskaźnikami.
        assert_eq!(cfg.mode(7).unwrap().width, 1920);
        assert_eq!(new.mode(7).unwrap().width, 1280);
        assert_ne!(new.paths[0].source, cfg.paths[0].source);
        let own: *const SourceMode = &new.sources[0];
        assert_eq!(new.paths[0].source as *const SourceMode, own);
        assert_eq!(new.details[0][0].timing_override, TIMING_OVERRIDE_AUTO);
        assert!(new.with_mode(99, new.mode(7).unwrap()).is_none());
    }
}
