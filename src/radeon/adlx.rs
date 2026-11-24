//! Wiązanie do AMD ADLX (`amdadlx64.dll`, instalowany ze sterownikiem
//! Radeon) - publicznego API sterownika, z którego korzysta AMD Software:
//! Adrenalin Edition. Układ interfejsów z nagłówków C ADLX SDK
//! (github.com/GPUOpen-LibrariesAndSDKs/ADLX, `SDK/Include`, wersja 2.0).
//!
//! Interfejsy ADLX to obiekty z tablicą metod (jak COM): pierwsze pole obiektu
//! wskazuje tablicę wskaźników na funkcje (`__stdcall`, na x64 zwykła
//! konwencja Windows). Każdy interfejs poza `IADLXSystem` zaczyna się od
//! `Acquire`, `Release`, `QueryInterface` (pozycje 0-2) i trzeba go zwolnić.
//! Numery pozycji metod są poniżej przy każdym interfejsie.

use std::ffi::{c_char, c_void, CStr};
use std::ptr::null_mut;

/// ADLX_RESULT
pub const ADLX_OK: i32 = 0;
const ADLX_ALREADY_ENABLED: i32 = 1;
/// ADLX zainicjowany wcześniej w tym procesie - według `ADLX_SUCCEEDED`
/// z ADLXDefines.h to sukces.
const ADLX_ALREADY_INITIALIZED: i32 = 2;
pub const ADLX_NOT_SUPPORTED: i32 = 12;

/// Wersja nagłówków, z którymi zgodne są poniższe tablice metod
/// (ADLX_MAKE_FULL_VER(2, 0, 0, 125)).
const ADLX_FULL_VERSION: u64 = (2u64 << 48) | 125;

type Vtbl = *const *const c_void;

/// Wskaźnik na obiekt ADLX; zwalniany (`Release`) przy końcu życia.
pub struct Obj {
    ptr: *mut Vtbl,
    release: bool,
}

// Obiekty ADLX są używane tylko z wątku interfejsu; przeniesienie wskaźnika
// między wątkami nie zmienia obiektu.
unsafe impl Send for Obj {}

impl Drop for Obj {
    fn drop(&mut self) {
        if self.release && !self.ptr.is_null() {
            // SAFETY: pozycja 1 każdego interfejsu pochodnego IADLXInterface.
            unsafe {
                let f: extern "system" fn(*mut Vtbl) -> i32 = std::mem::transmute(self.slot(1));
                f(self.ptr);
            }
        }
    }
}

impl Obj {
    fn new(ptr: *mut c_void) -> Option<Obj> {
        (!ptr.is_null()).then_some(Obj {
            ptr: ptr as *mut Vtbl,
            release: true,
        })
    }

    /// Wskaźnik na funkcję z pozycji `i` tablicy metod.
    unsafe fn slot(&self, i: usize) -> *const c_void {
        *(*self.ptr).add(i)
    }

    pub fn raw(&self) -> *mut c_void {
        self.ptr as *mut c_void
    }

    /// `QueryInterface` (pozycja 2) - nowszy interfejs tego samego obiektu.
    pub fn query(&self, iid: &str) -> Option<Obj> {
        let wide: Vec<u16> = iid.encode_utf16().chain(Some(0)).collect();
        let mut out: *mut c_void = null_mut();
        // SAFETY: (this, const wchar_t*, void**) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, *const u16, *mut *mut c_void) -> i32 =
                std::mem::transmute(self.slot(2));
            f(self.ptr, wide.as_ptr(), &mut out)
        };
        if r == ADLX_OK {
            Obj::new(out)
        } else {
            None
        }
    }

    /// Metoda `(this, T**)`: zwraca nowy obiekt.
    pub fn get(&self, slot: usize) -> Result<Obj, i32> {
        let mut out: *mut c_void = null_mut();
        // SAFETY: metoda o sygnaturze (this, void**) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, *mut *mut c_void) -> i32 =
                std::mem::transmute(self.slot(slot));
            f(self.ptr, &mut out)
        };
        check(r)?;
        Obj::new(out).ok_or(ADLX_NOT_SUPPORTED)
    }

    /// Metoda `(this, arg*, T**)`, np. usługa ustawień dla karty albo ekranu.
    pub fn get_for(&self, slot: usize, arg: &Obj) -> Result<Obj, i32> {
        let mut out: *mut c_void = null_mut();
        // SAFETY: metoda o sygnaturze (this, IADLX*, void**) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, *mut c_void, *mut *mut c_void) -> i32 =
                std::mem::transmute(self.slot(slot));
            f(self.ptr, arg.raw(), &mut out)
        };
        check(r)?;
        Obj::new(out).ok_or(ADLX_NOT_SUPPORTED)
    }

    /// Metoda `(this, adlx_uint, T**)` - element listy.
    pub fn at(&self, slot: usize, index: u32) -> Result<Obj, i32> {
        let mut out: *mut c_void = null_mut();
        // SAFETY: metoda o sygnaturze (this, adlx_uint, void**) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, u32, *mut *mut c_void) -> i32 =
                std::mem::transmute(self.slot(slot));
            f(self.ptr, index, &mut out)
        };
        check(r)?;
        Obj::new(out).ok_or(ADLX_NOT_SUPPORTED)
    }

    /// Metoda `(this) -> adlx_uint`, np. `Size` listy.
    pub fn count(&self, slot: usize) -> u32 {
        // SAFETY: metoda o sygnaturze (this) -> adlx_uint.
        unsafe {
            let f: extern "system" fn(*mut Vtbl) -> u32 = std::mem::transmute(self.slot(slot));
            f(self.ptr)
        }
    }

    /// Metoda `(this, adlx_bool*)`, np. `IsSupported`, `IsEnabled`.
    pub fn flag(&self, slot: usize) -> Result<bool, i32> {
        let mut v = 0u8;
        // SAFETY: metoda o sygnaturze (this, adlx_bool*) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, *mut u8) -> i32 =
                std::mem::transmute(self.slot(slot));
            f(self.ptr, &mut v)
        };
        check(r).map(|_| v != 0)
    }

    /// Metoda `(this, adlx_bool)`, np. `SetEnabled`.
    pub fn set_flag(&self, slot: usize, on: bool) -> Result<(), i32> {
        // SAFETY: metoda o sygnaturze (this, adlx_bool) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, u8) -> i32 = std::mem::transmute(self.slot(slot));
            f(self.ptr, on as u8)
        };
        check(r)
    }

    /// Metoda `(this, adlx_int*)` albo `(this, ENUM*)` (wyliczenia ADLX mają
    /// rozmiar `int`).
    pub fn int(&self, slot: usize) -> Result<i32, i32> {
        let mut v = 0i32;
        // SAFETY: metoda o sygnaturze (this, int*) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, *mut i32) -> i32 =
                std::mem::transmute(self.slot(slot));
            f(self.ptr, &mut v)
        };
        check(r).map(|_| v)
    }

    /// Metoda `(this, adlx_int)` albo `(this, ENUM)`.
    pub fn set_int(&self, slot: usize, value: i32) -> Result<(), i32> {
        // SAFETY: metoda o sygnaturze (this, int) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, i32) -> i32 = std::mem::transmute(self.slot(slot));
            f(self.ptr, value)
        };
        check(r)
    }

    /// Metoda `(this, ADLX_IntRange*)`.
    pub fn range(&self, slot: usize) -> Result<IntRange, i32> {
        let mut v = IntRange::default();
        // SAFETY: metoda o sygnaturze (this, ADLX_IntRange*) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, *mut IntRange) -> i32 =
                std::mem::transmute(self.slot(slot));
            f(self.ptr, &mut v)
        };
        check(r).map(|_| v)
    }

    /// Metoda `(this)` bez wyniku poza kodem, np. `ResetShaderCache`.
    pub fn call(&self, slot: usize) -> Result<(), i32> {
        // SAFETY: metoda o sygnaturze (this) -> ADLX_RESULT.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl) -> i32 = std::mem::transmute(self.slot(slot));
            f(self.ptr)
        };
        check(r)
    }

    /// Metoda `(this, const char**)`, np. nazwa karty albo wersja sterownika.
    pub fn text(&self, slot: usize) -> Result<String, i32> {
        let mut p: *const c_char = std::ptr::null();
        // SAFETY: metoda o sygnaturze (this, const char**) -> ADLX_RESULT;
        // tekst należy do obiektu i jest kopiowany od razu.
        let r = unsafe {
            let f: extern "system" fn(*mut Vtbl, *mut *const c_char) -> i32 =
                std::mem::transmute(self.slot(slot));
            f(self.ptr, &mut p)
        };
        check(r)?;
        if p.is_null() {
            return Err(ADLX_NOT_SUPPORTED);
        }
        // SAFETY: zakończony zerem tekst ADLX.
        Ok(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// ADLX_IntRange
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IntRange {
    pub min: i32,
    pub max: i32,
    pub step: i32,
}

fn check(r: i32) -> Result<(), i32> {
    // "Już włączone" przy włączaniu to nie błąd.
    if r == ADLX_OK || r == ADLX_ALREADY_ENABLED {
        Ok(())
    } else {
        Err(r)
    }
}

/// Opis kodu ADLX_RESULT dla użytkownika.
pub fn describe(code: i32) -> String {
    let name = match code {
        3 => "ADLX_FAIL",
        4 => "ADLX_INVALID_ARGS",
        5 => "ADLX_BAD_VER",
        6 => "ADLX_UNKNOWN_INTERFACE",
        8 => "ADLX_ADL_INIT_ERROR",
        9 => "ADLX_NOT_FOUND",
        10 => "ADLX_INVALID_OBJECT",
        12 => {
            return crate::i18n::tr(
                "niedostępne na tej karcie albo w tym sterowniku",
                "not available on this card or driver",
            )
            .into()
        }
        14 => "ADLX_GPU_INACTIVE",
        15 => "ADLX_GPU_IN_USE",
        16 => "ADLX_TIMEOUT_OPERATION",
        17 => "ADLX_NOT_ACTIVE",
        18 => "ADLX_RESET_NEEDED",
        _ => "",
    };
    if name.is_empty() {
        format!("ADLX {code}")
    } else {
        name.to_owned()
    }
}

// ------------------------------ pozycje metod -------------------------------

/// IADLXSystem (bez Acquire/Release - obiekt należy do biblioteki).
pub mod system {
    pub const GET_GPUS: usize = 1;
    pub const GET_DISPLAYS_SERVICES: usize = 3;
    pub const GET_3D_SETTINGS_SERVICES: usize = 7;
}

/// IADLXGPUList / IADLXDisplayList (IADLXList + `At_*List`).
pub mod list {
    pub const SIZE: usize = 3;
    pub const AT_TYPED: usize = 11;
}

/// IADLXGPU, IADLXGPU2.
pub mod gpu {
    pub const VENDOR_ID: usize = 3;
    pub const TYPE: usize = 5;
    pub const NAME: usize = 7;
    /// IADLXGPU2
    pub const AMD_SOFTWARE_VERSION: usize = 31;
    pub const DRIVER_VERSION: usize = 32;
    /// ADLX_GPU_TYPE
    pub const TYPE_DISCRETE: i32 = 2;
}

/// IADLX3DSettingsServices (0-16), ...1 (17), ...2 (18), ...3 (19, 20).
pub mod s3d {
    pub const ANTI_LAG: usize = 3;
    pub const CHILL: usize = 4;
    pub const BOOST: usize = 5;
    pub const IMAGE_SHARPENING: usize = 6;
    pub const ENHANCED_SYNC: usize = 7;
    pub const WAIT_FOR_VERTICAL_REFRESH: usize = 8;
    pub const FRAME_RATE_TARGET_CONTROL: usize = 9;
    pub const ANTI_ALIASING: usize = 10;
    pub const MORPHOLOGICAL_ANTI_ALIASING: usize = 11;
    pub const ANISOTROPIC_FILTERING: usize = 12;
    pub const TESSELLATION: usize = 13;
    /// Bez argumentu karty.
    pub const RADEON_SUPER_RESOLUTION: usize = 14;
    pub const RESET_SHADER_CACHE: usize = 15;
    /// IADLX3DSettingsServices1, bez argumentu karty.
    pub const AMD_FLUID_MOTION_FRAMES: usize = 17;
    /// IADLX3DSettingsServices2
    pub const IMAGE_SHARPEN_DESKTOP: usize = 18;
    /// IADLX3DSettingsServices3
    pub const FIDELITYFX_SUPER_RESOLUTION: usize = 19;
    pub const FIDELITYFX_FRAME_GEN_UPGRADE: usize = 20;
}

/// IADLXDisplayServices (0-19), ...1 (20), ...2 (21), ...3 (22, 23).
pub mod disp {
    pub const GET_DISPLAYS: usize = 4;
    pub const FREESYNC: usize = 9;
    /// IADLXDisplayServices3
    pub const DYNAMIC_REFRESH_RATE_CONTROL: usize = 22;
    pub const FREESYNC_COLOR_ACCURACY: usize = 23;
    /// IADLXDisplay
    pub const NAME: usize = 6;
}

// --------------------------------- sesja ------------------------------------

/// Połączenie z ADLX: biblioteka, system i wybrana karta AMD.
///
/// ADLX ma jedno połączenie na proces (drugie `ADLXInitialize` zwraca
/// `ADLX_ALREADY_INITIALIZED`, a `ADLXTerminate` kończy je dla wszystkich),
/// dlatego program używa jednej wspólnej sesji (`shared`), otwartej do końca
/// działania programu.
pub struct Adlx {
    pub gpu: Obj,
    pub services_3d: Option<Obj>,
    /// Najwyższa dostępna wersja usługi 3D (0-3).
    pub level_3d: u8,
    pub displays: Option<Obj>,
    pub level_display: u8,
    /// Obiekt systemu ADLX (nie ma `Release`; żyje razem z biblioteką).
    _system: Obj,
    _lib: libloading::Library,
    pub name: String,
    /// Wersja AMD Software (np. "25.11.1"), gdy sterownik ją podaje.
    pub software_version: Option<String>,
}

// Sesja jest wspólna dla wątków programu; ADLX obsługuje wywołania z wielu
// wątków (obiekty z licznikiem odwołań), a stan programu chroni Mutex.
unsafe impl Sync for Adlx {}

impl Adlx {
    /// Wspólna sesja ADLX programu. Nieudane połączenie (np. sterownik
    /// jeszcze się ładuje) jest ponawiane przy następnym wywołaniu.
    pub fn shared() -> Result<std::sync::Arc<Adlx>, String> {
        static SESSION: std::sync::Mutex<Option<std::sync::Arc<Adlx>>> =
            std::sync::Mutex::new(None);
        let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = session.as_ref() {
            return Ok(s.clone());
        }
        let s = std::sync::Arc::new(Adlx::load()?);
        *session = Some(s.clone());
        Ok(s)
    }

    fn load() -> Result<Adlx, String> {
        type Init = unsafe extern "C" fn(u64, *mut *mut c_void) -> i32;
        // SAFETY: biblioteka sterownika AMD; funkcje o sygnaturach z ADLX.h.
        unsafe {
            let lib = libloading::Library::new("amdadlx64.dll").map_err(|_| {
                crate::i18n::tr(
                    "Nie można załadować amdadlx64.dll (brak sterownika AMD Radeon?)",
                    "Cannot load amdadlx64.dll (no AMD Radeon driver?)",
                )
                .to_string()
            })?;
            let init: Init = *lib
                .get::<Init>(b"ADLXInitialize\0")
                .map_err(|e| format!("ADLXInitialize: {e}"))?;
            let mut sys: *mut c_void = null_mut();
            let r = init(ADLX_FULL_VERSION, &mut sys);
            if !(r == ADLX_OK || r == ADLX_ALREADY_INITIALIZED) || sys.is_null() {
                return Err(format!("ADLXInitialize: {}", describe(r)));
            }
            let system = Obj {
                ptr: sys as *mut Vtbl,
                release: false,
            };
            let gpus = system
                .get(system::GET_GPUS)
                .map_err(|e| format!("GetGPUs: {}", describe(e)))?;
            let gpu = match pick_gpu(&gpus) {
                Some(g) => g,
                None => {
                    return Err(crate::i18n::tr(
                        "ADLX nie widzi karty AMD.",
                        "ADLX sees no AMD card.",
                    )
                    .into());
                }
            };
            drop(gpus);
            let name = gpu.text(gpu::NAME).unwrap_or_else(|_| "AMD Radeon".into());
            let gpu2 = gpu.query("IADLXGPU2");
            let software_version = gpu2.as_ref().and_then(|g| {
                g.text(gpu::AMD_SOFTWARE_VERSION)
                    .or_else(|_| g.text(gpu::DRIVER_VERSION))
                    .ok()
            });
            drop(gpu2);

            let (services_3d, level_3d) = match system.get(system::GET_3D_SETTINGS_SERVICES) {
                Ok(base) => upgrade(
                    base,
                    &[
                        "IADLX3DSettingsServices1",
                        "IADLX3DSettingsServices2",
                        "IADLX3DSettingsServices3",
                    ],
                ),
                Err(_) => (None, 0),
            };
            let (displays, level_display) = match system.get(system::GET_DISPLAYS_SERVICES) {
                Ok(base) => upgrade(
                    base,
                    &[
                        "IADLXDisplayServices1",
                        "IADLXDisplayServices2",
                        "IADLXDisplayServices3",
                    ],
                ),
                Err(_) => (None, 0),
            };
            Ok(Adlx {
                gpu,
                services_3d,
                level_3d,
                displays,
                level_display,
                _system: system,
                _lib: lib,
                name,
                software_version,
            })
        }
    }

    /// Ekrany podłączone do kart AMD: (nazwa, obiekt IADLXDisplay).
    pub fn displays(&self) -> Vec<(String, Obj)> {
        let Some(s) = &self.displays else {
            return Vec::new();
        };
        let Ok(list) = s.get(disp::GET_DISPLAYS) else {
            return Vec::new();
        };
        (0..list.count(list::SIZE))
            .filter_map(|i| list.at(list::AT_TYPED, i).ok())
            .map(|d| {
                let name = d
                    .text(disp::NAME)
                    .ok()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| crate::i18n::tr("Ekran", "Display").into());
                (name, d)
            })
            .collect()
    }

}

/// Pierwsza karta AMD (identyfikator producenta 1002), najlepiej osobna
/// (nie zintegrowana w procesorze).
fn pick_gpu(gpus: &Obj) -> Option<Obj> {
    let all: Vec<Obj> = (0..gpus.count(list::SIZE))
        .filter_map(|i| gpus.at(list::AT_TYPED, i).ok())
        .filter(|g| {
            g.text(gpu::VENDOR_ID)
                .map(|v| v.trim().eq_ignore_ascii_case("1002"))
                .unwrap_or(true)
        })
        .collect();
    let discrete = all
        .iter()
        .position(|g| g.int(gpu::TYPE) == Ok(gpu::TYPE_DISCRETE));
    let index = discrete.unwrap_or(0);
    all.into_iter().nth(index)
}

/// Najnowsza wersja interfejsu usługi, jaką zna sterownik.
fn upgrade(base: Obj, newer: &[&str]) -> (Option<Obj>, u8) {
    let mut best = base;
    let mut level = 0;
    for (i, iid) in newer.iter().enumerate() {
        match best.query(iid) {
            Some(n) => {
                best = n;
                level = i as u8 + 1;
            }
            None => break,
        }
    }
    (Some(best), level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_headers() {
        // ADLX_MAKE_FULL_VER(2, 0, 0, 125)
        assert_eq!(ADLX_FULL_VERSION, 0x0002_0000_0000_007D);
    }

    #[test]
    fn range_layout() {
        assert_eq!(std::mem::size_of::<IntRange>(), 12);
    }

    /// Na komputerze z kartą AMD: `cargo test adlx_probe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn adlx_probe() {
        let a = Adlx::shared().expect("ADLX");
        println!(
            "{} | AMD Software {:?} | 3D lvl {} | display lvl {}",
            a.name, a.software_version, a.level_3d, a.level_display
        );
        for (name, _) in a.displays() {
            println!("ekran: {name}");
        }
    }
}
