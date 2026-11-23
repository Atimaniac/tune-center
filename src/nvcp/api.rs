//! Wiązanie do publicznego NVAPI (nvapi64.dll) dla zakładki NVIDIA.
//!
//! Tylko funkcje z oficjalnego NVAPI SDK (github.com/NVIDIA/nvapi, licencja
//! MIT): identyfikatory z `nvapi_interface.h`, struktury z `nvapi.h`. Funkcje
//! pobiera się przez `nvapi_QueryInterface` - dokładnie tak robi biblioteka
//! `nvapi64.lib` z SDK. Sterownik sprawdza pole `version` każdej struktury
//! (rozmiar + numer wersji), więc niezgodny układ kończy się błędem
//! `INCOMPATIBLE_STRUCT_VERSION`, a nie zapisem śmieci.

use std::ffi::c_void;
use std::mem::size_of;

pub type Status = i32;
pub type Handle = *mut c_void;
type QueryFn = unsafe extern "C" fn(u32) -> *const c_void;

pub const OK: Status = 0;
pub const END_ENUMERATION: Status = -7;
pub const SETTING_NOT_FOUND: Status = -160;
pub const PROFILE_NOT_FOUND: Status = -163;
pub const EXECUTABLE_NOT_FOUND: Status = -166;

/// `MAKE_NVAPI_VERSION(typ, wersja)` z nvapi.h.
pub const fn ver<T>(v: u32) -> u32 {
    (size_of::<T>() as u32) | (v << 16)
}

/// `NvAPI_UnicodeString` - 2048 znaków UTF-16 zakończonych zerem.
pub type WString = [u16; 2048];

pub fn to_wide(text: &str) -> Box<WString> {
    let mut out = Box::new([0u16; 2048]);
    for (slot, unit) in out.iter_mut().zip(text.encode_utf16().take(2047)) {
        *slot = unit;
    }
    out
}

pub fn from_wide(text: &[u16]) -> String {
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..len])
}

pub fn from_short(text: &[u8]) -> String {
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf8_lossy(&text[..len]).into_owned()
}

/// Opis błędu NVAPI w bieżącym języku, z kodem.
pub fn describe(status: Status) -> String {
    use crate::i18n::tr;
    let text = match status {
        -1 => tr("błąd ogólny", "generic error"),
        -3 => tr(
            "brak w tej wersji sterownika",
            "not implemented in this driver",
        ),
        -5 => tr("nieprawidłowy argument", "invalid argument"),
        -6 => tr("nie znaleziono karty Nvidia", "no Nvidia GPU found"),
        -9 => tr(
            "niezgodna wersja struktury",
            "incompatible structure version",
        ),
        -103 => tr("niedozwolone połączenie ustawień", "invalid combination"),
        -104 => tr(
            "nieobsługiwane przez kartę lub monitor",
            "not supported by the GPU or display",
        ),
        -137 => tr(
            "wymaga uprawnień administratora",
            "requires administrator rights",
        ),
        -150 => tr(
            "zmiana trybu ekranu nie powiodła się",
            "display mode change failed",
        ),
        -157 => tr("wymaga przeładowania sterownika", "driver reload required"),
        -160 => tr("nie znaleziono ustawienia", "setting not found"),
        -163 => tr("nie znaleziono profilu", "profile not found"),
        -164 => tr(
            "profil o tej nazwie już istnieje",
            "profile name already in use",
        ),
        -166 => tr(
            "program nie należy do profilu",
            "application not in profile",
        ),
        -167 => tr(
            "program jest już w innym profilu",
            "application already in another profile",
        ),
        -180 => tr(
            "niedozwolone w obecnym stanie",
            "not allowed in the current state",
        ),
        -187 => tr("nieprawidłowy ekran", "invalid display"),
        _ => tr("błąd NVAPI", "NVAPI error"),
    };
    format!("{text} ({status})")
}

pub fn check(status: Status) -> Result<(), String> {
    if status == OK {
        Ok(())
    } else {
        Err(describe(status))
    }
}

// ---------------------- identyfikatory (nvapi_interface.h) -----------------

const ID_INITIALIZE: u32 = 0x0150_e828;
const ID_SYS_GET_DRIVER_AND_BRANCH_VERSION: u32 = 0x2926_aaad;
const ID_ENUM_PHYSICAL_GPUS: u32 = 0xe5ac_921f;
const ID_GPU_GET_CONNECTED_DISPLAY_IDS: u32 = 0x0078_dba2;
const ID_ENUM_NVIDIA_DISPLAY_HANDLE: u32 = 0x9abd_d40d;
const ID_GET_ASSOCIATED_NVIDIA_DISPLAY_NAME: u32 = 0x22a7_8b05;
const ID_DISP_GET_DISPLAY_ID_BY_DISPLAY_NAME: u32 = 0xae45_7190;
const ID_DISP_GET_GDI_PRIMARY_DISPLAY_ID: u32 = 0x1e9d_8a31;
const ID_DISP_GET_EDID_DATA: u32 = 0x436c_ed76;
const ID_DISP_GET_DISPLAY_CONFIG: u32 = 0x11ab_ccf8;
const ID_DISP_SET_DISPLAY_CONFIG: u32 = 0x5d8c_f8de;
const ID_DISP_COLOR_CONTROL: u32 = 0x92f9_d80d;
const ID_DISP_HDR_COLOR_CONTROL: u32 = 0x351d_a224;
const ID_DISP_GET_HDR_CAPABILITIES: u32 = 0x84f2_a8df;
const ID_DISP_GET_ADAPTIVE_SYNC_DATA: u32 = 0xb73d_1ee9;
const ID_DISP_SET_ADAPTIVE_SYNC_DATA: u32 = 0x3eeb_ba1d;
const ID_DISP_GET_VRR_INFO: u32 = 0xdf8f_da57;
const ID_DISP_ENUM_CUSTOM_DISPLAY: u32 = 0xa207_2d59;
const ID_DISP_TRY_CUSTOM_DISPLAY: u32 = 0x1f7d_b630;
const ID_DISP_SAVE_CUSTOM_DISPLAY: u32 = 0x4988_2876;
const ID_DISP_REVERT_CUSTOM_DISPLAY_TRIAL: u32 = 0xcbbd_40f0;
const ID_DISP_DELETE_CUSTOM_DISPLAY: u32 = 0x552e_5b9b;
const ID_DISP_GET_TIMING: u32 = 0x1751_67e9;

const ID_DRS_CREATE_SESSION: u32 = 0x0694_d52e;
const ID_DRS_DESTROY_SESSION: u32 = 0xdad9_cff8;
const ID_DRS_LOAD_SETTINGS: u32 = 0x375d_bd6b;
const ID_DRS_SAVE_SETTINGS: u32 = 0xfcbc_7e14;
const ID_DRS_LOAD_SETTINGS_FROM_FILE: u32 = 0xd3ed_e889;
const ID_DRS_SAVE_SETTINGS_TO_FILE: u32 = 0x2be2_5df8;
const ID_DRS_CREATE_PROFILE: u32 = 0xcc17_6068;
const ID_DRS_DELETE_PROFILE: u32 = 0x1709_3206;
const ID_DRS_GET_CURRENT_GLOBAL_PROFILE: u32 = 0x617b_ff9f;
const ID_DRS_GET_PROFILE_INFO: u32 = 0x61cd_6fd6;
const ID_DRS_FIND_PROFILE_BY_NAME: u32 = 0x7e4a_9a0b;
const ID_DRS_ENUM_PROFILES: u32 = 0xbc37_1ee0;
const ID_DRS_GET_NUM_PROFILES: u32 = 0x1dae_4fbc;
const ID_DRS_CREATE_APPLICATION: u32 = 0x4347_a9de;
const ID_DRS_ENUM_APPLICATIONS: u32 = 0x7fa2_173a;
const ID_DRS_FIND_APPLICATION_BY_NAME: u32 = 0xeee5_66b2;
const ID_DRS_SET_SETTING: u32 = 0x577d_d202;
const ID_DRS_GET_SETTING: u32 = 0x73bf_8338;
const ID_DRS_ENUM_SETTINGS: u32 = 0xae30_39da;
const ID_DRS_ENUM_AVAILABLE_SETTING_IDS: u32 = 0xf020_614a;
const ID_DRS_ENUM_AVAILABLE_SETTING_VALUES: u32 = 0x2ec3_9f90;
const ID_DRS_GET_SETTING_NAME_FROM_ID: u32 = 0xd61c_be6e;
const ID_DRS_DELETE_PROFILE_SETTING: u32 = 0xe4a2_6362;
const ID_DRS_RESTORE_ALL_DEFAULTS: u32 = 0x5927_b094;
const ID_DRS_RESTORE_PROFILE_DEFAULT: u32 = 0xfa5f_6134;
const ID_DRS_RESTORE_PROFILE_DEFAULT_SETTING: u32 = 0x53f0_381e;
const ID_GPU_CLIENT_ILLUM_ZONES_GET_INFO: u32 = 0x4b81_241b;
const ID_GPU_CLIENT_ILLUM_ZONES_GET_CONTROL: u32 = 0x3dbf_5764;
const ID_GPU_CLIENT_ILLUM_ZONES_SET_CONTROL: u32 = 0x197d_065e;

// ------------------------------ typy funkcji -------------------------------

pub type FnDriverVersion = unsafe extern "C" fn(*mut u32, *mut u8) -> Status;
pub type FnEnumGpus = unsafe extern "C" fn(*mut Handle, *mut u32) -> Status;
/// Funkcja z uchwytem karty i strukturą parametrów (np. oświetlenie).
pub type FnGpuStruct = unsafe extern "C" fn(Handle, *mut c_void) -> Status;
pub type FnConnectedDisplayIds = unsafe extern "C" fn(Handle, *mut c_void, *mut u32, u32) -> Status;
pub type FnEnumDisplayHandle = unsafe extern "C" fn(u32, *mut Handle) -> Status;
pub type FnDisplayName = unsafe extern "C" fn(Handle, *mut u8) -> Status;
pub type FnIdByName = unsafe extern "C" fn(*const u8, *mut u32) -> Status;
pub type FnPrimaryId = unsafe extern "C" fn(*mut u32) -> Status;
pub type FnEdid = unsafe extern "C" fn(u32, *mut c_void, *mut u32) -> Status;
pub type FnGetDisplayConfig = unsafe extern "C" fn(*mut u32, *mut c_void) -> Status;
pub type FnSetDisplayConfig = unsafe extern "C" fn(u32, *mut c_void, u32) -> Status;
pub type FnDisplayStruct = unsafe extern "C" fn(u32, *mut c_void) -> Status;
pub type FnEnumCustom = unsafe extern "C" fn(u32, u32, *mut c_void) -> Status;
pub type FnTryCustom = unsafe extern "C" fn(*mut u32, u32, *mut c_void) -> Status;
pub type FnSaveCustom = unsafe extern "C" fn(*mut u32, u32, u32, u32) -> Status;
pub type FnRevertCustom = unsafe extern "C" fn(*mut u32, u32) -> Status;
pub type FnGetTiming = unsafe extern "C" fn(u32, *mut c_void, *mut c_void) -> Status;

pub type FnSession = unsafe extern "C" fn(Handle) -> Status;
pub type FnSessionOut = unsafe extern "C" fn(*mut Handle) -> Status;
pub type FnSessionFile = unsafe extern "C" fn(Handle, *const u16) -> Status;
pub type FnProfileOut = unsafe extern "C" fn(Handle, *mut Handle) -> Status;
pub type FnProfileInfo = unsafe extern "C" fn(Handle, Handle, *mut c_void) -> Status;
pub type FnCreateProfile = unsafe extern "C" fn(Handle, *mut c_void, *mut Handle) -> Status;
pub type FnProfile = unsafe extern "C" fn(Handle, Handle) -> Status;
pub type FnFindProfile = unsafe extern "C" fn(Handle, *const u16, *mut Handle) -> Status;
pub type FnEnumProfiles = unsafe extern "C" fn(Handle, u32, *mut Handle) -> Status;
pub type FnNumProfiles = unsafe extern "C" fn(Handle, *mut u32) -> Status;
pub type FnEnumApps = unsafe extern "C" fn(Handle, Handle, u32, *mut u32, *mut c_void) -> Status;
pub type FnFindApp = unsafe extern "C" fn(Handle, *const u16, *mut Handle, *mut c_void) -> Status;
pub type FnGetSetting = unsafe extern "C" fn(Handle, Handle, u32, *mut c_void) -> Status;
pub type FnEnumSettings =
    unsafe extern "C" fn(Handle, Handle, u32, *mut u32, *mut c_void) -> Status;
pub type FnSettingIds = unsafe extern "C" fn(*mut u32, *mut u32) -> Status;
pub type FnSettingValues = unsafe extern "C" fn(u32, *mut u32, *mut c_void) -> Status;
pub type FnSettingName = unsafe extern "C" fn(u32, *mut u16) -> Status;
pub type FnProfileSettingId = unsafe extern "C" fn(Handle, Handle, u32) -> Status;

/// Funkcje publicznego NVAPI używane przez zakładkę NVIDIA. Brak funkcji
/// (starszy sterownik) oznacza `None` i wyszarzoną część interfejsu.
pub struct Api {
    _lib: libloading::Library,
    pub driver_version: Option<FnDriverVersion>,
    pub enum_gpus: Option<FnEnumGpus>,
    pub connected_display_ids: Option<FnConnectedDisplayIds>,
    pub enum_display_handle: Option<FnEnumDisplayHandle>,
    pub display_name: Option<FnDisplayName>,
    pub display_id_by_name: Option<FnIdByName>,
    pub primary_display_id: Option<FnPrimaryId>,
    pub edid_data: Option<FnEdid>,
    pub get_display_config: Option<FnGetDisplayConfig>,
    pub set_display_config: Option<FnSetDisplayConfig>,
    pub color_control: Option<FnDisplayStruct>,
    pub hdr_color_control: Option<FnDisplayStruct>,
    pub hdr_capabilities: Option<FnDisplayStruct>,
    pub get_adaptive_sync: Option<FnDisplayStruct>,
    pub set_adaptive_sync: Option<FnDisplayStruct>,
    pub vrr_info: Option<FnDisplayStruct>,
    pub enum_custom: Option<FnEnumCustom>,
    pub try_custom: Option<FnTryCustom>,
    pub save_custom: Option<FnSaveCustom>,
    pub revert_custom: Option<FnRevertCustom>,
    pub delete_custom: Option<FnTryCustom>,
    pub get_timing: Option<FnGetTiming>,

    pub drs_create_session: Option<FnSessionOut>,
    pub drs_destroy_session: Option<FnSession>,
    pub drs_load: Option<FnSession>,
    pub drs_save: Option<FnSession>,
    pub drs_load_from_file: Option<FnSessionFile>,
    pub drs_save_to_file: Option<FnSessionFile>,
    pub drs_create_profile: Option<FnCreateProfile>,
    pub drs_delete_profile: Option<FnProfile>,
    pub drs_global_profile: Option<FnProfileOut>,
    pub drs_profile_info: Option<FnProfileInfo>,
    pub drs_find_profile: Option<FnFindProfile>,
    pub drs_enum_profiles: Option<FnEnumProfiles>,
    pub drs_num_profiles: Option<FnNumProfiles>,
    pub drs_create_app: Option<FnProfileInfo>,
    pub drs_enum_apps: Option<FnEnumApps>,
    pub drs_find_app: Option<FnFindApp>,
    pub drs_set_setting: Option<FnProfileInfo>,
    pub drs_get_setting: Option<FnGetSetting>,
    pub drs_enum_settings: Option<FnEnumSettings>,
    pub drs_setting_ids: Option<FnSettingIds>,
    pub drs_setting_values: Option<FnSettingValues>,
    pub drs_setting_name: Option<FnSettingName>,
    pub drs_delete_profile_setting: Option<FnProfileSettingId>,
    pub drs_restore_all: Option<FnSession>,
    pub drs_restore_profile: Option<FnProfile>,
    pub drs_restore_setting: Option<FnProfileSettingId>,

    pub illum_zones_get_info: Option<FnGpuStruct>,
    pub illum_zones_get_control: Option<FnGpuStruct>,
    pub illum_zones_set_control: Option<FnGpuStruct>,
}

// Wskaźniki do funkcji sterownika można wołać z dowolnego wątku.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

impl Api {
    pub fn load() -> Result<Self, String> {
        // SAFETY: biblioteka sterownika Nvidii; nie wykonuje kodu przy wczytaniu
        // poza standardową inicjalizacją DLL.
        let lib = unsafe { libloading::Library::new("nvapi64.dll") }.map_err(|_| {
            crate::i18n::tr(
                "brak nvapi64.dll - nie ma sterownika Nvidia",
                "nvapi64.dll not found - no Nvidia driver",
            )
            .to_owned()
        })?;
        // SAFETY: jedyna funkcja eksportowana przez nvapi64.dll.
        let query: QueryFn = unsafe {
            *lib.get::<QueryFn>(b"nvapi_QueryInterface\0").map_err(|_| {
                crate::i18n::tr(
                    "nvapi64.dll bez nvapi_QueryInterface",
                    "nvapi64.dll lacks nvapi_QueryInterface",
                )
                .to_owned()
            })?
        };
        let get = |id: u32| -> *const c_void { unsafe { query(id) } };
        macro_rules! f {
            ($id:expr) => {{
                let p = get($id);
                if p.is_null() {
                    None
                } else {
                    // SAFETY: typ funkcji odpowiada deklaracji z nvapi.h.
                    Some(unsafe { std::mem::transmute_copy::<*const c_void, _>(&p) })
                }
            }};
        }

        let init: Option<unsafe extern "C" fn() -> Status> = f!(ID_INITIALIZE);
        let init = init.ok_or_else(|| describe(-3))?;
        // SAFETY: NvAPI_Initialize nie przyjmuje argumentów; można ją wołać
        // wielokrotnie (licznik odwołań w sterowniku).
        check(unsafe { init() })?;

        Ok(Self {
            driver_version: f!(ID_SYS_GET_DRIVER_AND_BRANCH_VERSION),
            enum_gpus: f!(ID_ENUM_PHYSICAL_GPUS),
            connected_display_ids: f!(ID_GPU_GET_CONNECTED_DISPLAY_IDS),
            enum_display_handle: f!(ID_ENUM_NVIDIA_DISPLAY_HANDLE),
            display_name: f!(ID_GET_ASSOCIATED_NVIDIA_DISPLAY_NAME),
            display_id_by_name: f!(ID_DISP_GET_DISPLAY_ID_BY_DISPLAY_NAME),
            primary_display_id: f!(ID_DISP_GET_GDI_PRIMARY_DISPLAY_ID),
            edid_data: f!(ID_DISP_GET_EDID_DATA),
            get_display_config: f!(ID_DISP_GET_DISPLAY_CONFIG),
            set_display_config: f!(ID_DISP_SET_DISPLAY_CONFIG),
            color_control: f!(ID_DISP_COLOR_CONTROL),
            hdr_color_control: f!(ID_DISP_HDR_COLOR_CONTROL),
            hdr_capabilities: f!(ID_DISP_GET_HDR_CAPABILITIES),
            get_adaptive_sync: f!(ID_DISP_GET_ADAPTIVE_SYNC_DATA),
            set_adaptive_sync: f!(ID_DISP_SET_ADAPTIVE_SYNC_DATA),
            vrr_info: f!(ID_DISP_GET_VRR_INFO),
            enum_custom: f!(ID_DISP_ENUM_CUSTOM_DISPLAY),
            try_custom: f!(ID_DISP_TRY_CUSTOM_DISPLAY),
            save_custom: f!(ID_DISP_SAVE_CUSTOM_DISPLAY),
            revert_custom: f!(ID_DISP_REVERT_CUSTOM_DISPLAY_TRIAL),
            delete_custom: f!(ID_DISP_DELETE_CUSTOM_DISPLAY),
            get_timing: f!(ID_DISP_GET_TIMING),
            drs_create_session: f!(ID_DRS_CREATE_SESSION),
            drs_destroy_session: f!(ID_DRS_DESTROY_SESSION),
            drs_load: f!(ID_DRS_LOAD_SETTINGS),
            drs_save: f!(ID_DRS_SAVE_SETTINGS),
            drs_load_from_file: f!(ID_DRS_LOAD_SETTINGS_FROM_FILE),
            drs_save_to_file: f!(ID_DRS_SAVE_SETTINGS_TO_FILE),
            drs_create_profile: f!(ID_DRS_CREATE_PROFILE),
            drs_delete_profile: f!(ID_DRS_DELETE_PROFILE),
            drs_global_profile: f!(ID_DRS_GET_CURRENT_GLOBAL_PROFILE),
            drs_profile_info: f!(ID_DRS_GET_PROFILE_INFO),
            drs_find_profile: f!(ID_DRS_FIND_PROFILE_BY_NAME),
            drs_enum_profiles: f!(ID_DRS_ENUM_PROFILES),
            drs_num_profiles: f!(ID_DRS_GET_NUM_PROFILES),
            drs_create_app: f!(ID_DRS_CREATE_APPLICATION),
            drs_enum_apps: f!(ID_DRS_ENUM_APPLICATIONS),
            drs_find_app: f!(ID_DRS_FIND_APPLICATION_BY_NAME),
            drs_set_setting: f!(ID_DRS_SET_SETTING),
            drs_get_setting: f!(ID_DRS_GET_SETTING),
            drs_enum_settings: f!(ID_DRS_ENUM_SETTINGS),
            drs_setting_ids: f!(ID_DRS_ENUM_AVAILABLE_SETTING_IDS),
            drs_setting_values: f!(ID_DRS_ENUM_AVAILABLE_SETTING_VALUES),
            drs_setting_name: f!(ID_DRS_GET_SETTING_NAME_FROM_ID),
            drs_delete_profile_setting: f!(ID_DRS_DELETE_PROFILE_SETTING),
            drs_restore_all: f!(ID_DRS_RESTORE_ALL_DEFAULTS),
            drs_restore_profile: f!(ID_DRS_RESTORE_PROFILE_DEFAULT),
            drs_restore_setting: f!(ID_DRS_RESTORE_PROFILE_DEFAULT_SETTING),
            illum_zones_get_info: f!(ID_GPU_CLIENT_ILLUM_ZONES_GET_INFO),
            illum_zones_get_control: f!(ID_GPU_CLIENT_ILLUM_ZONES_GET_CONTROL),
            illum_zones_set_control: f!(ID_GPU_CLIENT_ILLUM_ZONES_SET_CONTROL),
            _lib: lib,
        })
    }

    /// Wersja sterownika, np. "591.44 (r591_37)".
    pub fn driver(&self) -> Option<String> {
        let f = self.driver_version?;
        let mut version = 0u32;
        let mut branch = [0u8; 64];
        // SAFETY: bufor ma NVAPI_SHORT_STRING_MAX bajtów.
        let status = unsafe { f(&mut version, branch.as_mut_ptr()) };
        (status == OK).then(|| {
            format!(
                "{}.{:02} ({})",
                version / 100,
                version % 100,
                from_short(&branch)
            )
        })
    }
}

/// Wywołanie funkcji, której sterownik może nie mieć.
pub fn need<T: Copy>(f: Option<T>) -> Result<T, String> {
    f.ok_or_else(|| describe(-3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_round_trip() {
        let w = to_wide("Zażółć.exe");
        assert_eq!(from_wide(&w[..]), "Zażółć.exe");
        assert_eq!(from_wide(&[0u16; 4]), "");
        let long = "x".repeat(5000);
        assert_eq!(from_wide(&to_wide(&long)[..]).len(), 2047);
    }

    #[test]
    fn short_strings_stop_at_nul() {
        assert_eq!(from_short(b"\\\\.\\DISPLAY1\0garbage"), "\\\\.\\DISPLAY1");
    }

    #[test]
    fn version_packs_size_and_number() {
        assert_eq!(ver::<[u8; 12]>(1), 0x0001_000c);
        assert_eq!(ver::<[u8; 48]>(2), 0x0002_0030);
    }
}
