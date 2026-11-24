//! Zakładka TUNE: ustawienia Windows wpływające na opóźnienia i DPC.
//!
//! Każda pozycja: odczyt stanu z systemu, przełączenie na wartość zalecaną
//! albo powrót do oryginału, sprawdzenie odczytem po zapisie. Przed pierwszą
//! zmianą danej pozycji oryginalna wartość trafia do `tune-backup.json` obok
//! programu; "Przywróć ustawienia Windows" cofa wszystko z tej kopii.
//!
//! Grupa A (udokumentowane przez Microsoft / producentów): Memory Integrity,
//! Virtual Machine Platform, karta sieciowa (Interrupt Moderation, Energy
//! Efficient Ethernet), plan zasilania (USB Selective Suspend, PCIe Link State
//! Power Management, Ultimate Performance), tryb gry. Grupa B (do sprawdzenia
//! na własnym sprzęcie): tryb MSI przerwań urządzeń, globalna rozdzielczość
//! timera, Win32PrioritySeparation, SystemResponsiveness.
//! Celowo pominięte: `bcdedit useplatformclock/disabledynamictick` (sprzeczne
//! wyniki, problemy z myszą) i `NetworkThrottlingIndex` (więcej przerwań).

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ rejestr

#[cfg(windows)]
pub(crate) mod reg {
    pub type Hkey = isize;
    pub const HKLM: Hkey = 0x8000_0002u32 as i32 as isize;
    pub const HKCU: Hkey = 0x8000_0001u32 as i32 as isize;
    const KEY_READ: u32 = 0x20019;
    const KEY_ALL: u32 = 0xF003F;
    const KEY_WOW64_64KEY: u32 = 0x0100;
    const REG_DWORD: u32 = 4;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: Hkey, sub: *const u16, opts: u32, sam: u32, out: *mut Hkey) -> i32;
        fn RegCreateKeyExW(
            key: Hkey,
            sub: *const u16,
            reserved: u32,
            class: *const u16,
            opts: u32,
            sam: u32,
            sec: *const u8,
            out: *mut Hkey,
            disp: *mut u32,
        ) -> i32;
        fn RegQueryValueExW(
            key: Hkey,
            name: *const u16,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            len: *mut u32,
        ) -> i32;
        fn RegSetValueExW(
            key: Hkey,
            name: *const u16,
            reserved: u32,
            kind: u32,
            data: *const u8,
            len: u32,
        ) -> i32;
        fn RegDeleteValueW(key: Hkey, name: *const u16) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
        fn RegEnumKeyExW(
            key: Hkey,
            index: u32,
            name: *mut u16,
            name_len: *mut u32,
            reserved: *mut u32,
            class: *mut u16,
            class_len: *mut u32,
            time: *mut core::ffi::c_void,
        ) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Key(Hkey);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn open(root: Hkey, path: &str, write: bool) -> Option<Key> {
        let mut out = 0;
        let sam = if write { KEY_ALL } else { KEY_READ } | KEY_WOW64_64KEY;
        let p = wide(path);
        (unsafe { RegOpenKeyExW(root, p.as_ptr(), 0, sam, &mut out) } == 0).then_some(Key(out))
    }

    /// Wartość DWORD; `None`, gdy brak klucza albo wartości.
    pub fn get_dword(root: Hkey, path: &str, name: &str) -> Option<u32> {
        let key = open(root, path, false)?;
        let n = wide(name);
        let mut kind = 0u32;
        let mut data = 0u32;
        let mut len = 4u32;
        let r = unsafe {
            RegQueryValueExW(
                key.0,
                n.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                &mut data as *mut u32 as *mut u8,
                &mut len,
            )
        };
        (r == 0 && kind == REG_DWORD).then_some(data)
    }

    /// Zapisuje DWORD (`Some`) albo usuwa wartość (`None`); tworzy brakujący klucz.
    pub fn set_dword(root: Hkey, path: &str, name: &str, value: Option<u32>) -> Result<(), String> {
        let p = wide(path);
        let n = wide(name);
        let mut key = 0;
        let mut disp = 0;
        let r = unsafe {
            RegCreateKeyExW(
                root,
                p.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_ALL | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut key,
                &mut disp,
            )
        };
        if r != 0 {
            return Err(crate::trf!(
                "rejestr {path}: błąd {r}",
                "registry {path}: error {r}"
            ));
        }
        let key = Key(key);
        let r = match value {
            Some(v) => unsafe {
                RegSetValueExW(
                    key.0,
                    n.as_ptr(),
                    0,
                    REG_DWORD,
                    &v as *const u32 as *const u8,
                    4,
                )
            },
            None => match unsafe { RegDeleteValueW(key.0, n.as_ptr()) } {
                2 => 0, // już brak wartości
                r => r,
            },
        };
        if r != 0 {
            return Err(crate::trf!(
                "rejestr {path}\\{name}: błąd {r}",
                "registry {path}\\{name}: error {r}"
            ));
        }
        Ok(())
    }

    /// Zapisuje tekst REG_SZ (`Some`) albo usuwa wartość (`None`); tworzy brakujący klucz.
    pub fn set_string(
        root: Hkey,
        path: &str,
        name: &str,
        value: Option<&str>,
    ) -> Result<(), String> {
        let Some(text) = value else {
            return set_dword(root, path, name, None);
        };
        let p = wide(path);
        let n = wide(name);
        let mut key = 0;
        let mut disp = 0;
        let r = unsafe {
            RegCreateKeyExW(
                root,
                p.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_ALL | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut key,
                &mut disp,
            )
        };
        if r != 0 {
            return Err(crate::trf!(
                "rejestr {path}: błąd {r}",
                "registry {path}: error {r}"
            ));
        }
        let key = Key(key);
        let data = wide(text);
        // REG_SZ = 1; długość w bajtach razem z kończącym zerem.
        let r = unsafe {
            RegSetValueExW(
                key.0,
                n.as_ptr(),
                0,
                1,
                data.as_ptr() as *const u8,
                (data.len() * 2) as u32,
            )
        };
        if r != 0 {
            return Err(crate::trf!(
                "rejestr {path}\\{name}: błąd {r}",
                "registry {path}\\{name}: error {r}"
            ));
        }
        Ok(())
    }

    pub fn exists(root: Hkey, path: &str) -> bool {
        open(root, path, false).is_some()
    }

    pub fn subkeys(root: Hkey, path: &str) -> Vec<String> {
        let Some(key) = open(root, path, false) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let r = unsafe {
                RegEnumKeyExW(
                    key.0,
                    i,
                    buf.as_mut_ptr(),
                    &mut len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != 0 {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        out
    }

    pub fn get_string(root: Hkey, path: &str, name: &str) -> Option<String> {
        let key = open(root, path, false)?;
        let n = wide(name);
        let mut kind = 0u32;
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegQueryValueExW(
                key.0,
                n.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buf.as_mut_ptr() as *mut u8,
                &mut len,
            )
        };
        if r != 0 || !(kind == 1 || kind == 2) {
            return None;
        }
        let s = String::from_utf16_lossy(&buf[..(len as usize / 2)]);
        Some(s.trim_end_matches('\0').to_owned())
    }
}

// ------------------------------------------------------- plan zasilania

#[cfg(windows)]
mod power {
    #[repr(C)]
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct Guid {
        d1: u32,
        d2: u16,
        d3: u16,
        d4: [u8; 8],
    }

    impl Guid {
        pub fn parse(s: &str) -> Guid {
            let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            let b = |i: usize, n: usize| u64::from_str_radix(&h[i..i + n], 16).unwrap_or(0);
            let mut d4 = [0u8; 8];
            for (k, v) in d4.iter_mut().enumerate() {
                *v = b(16 + k * 2, 2) as u8;
            }
            Guid {
                d1: b(0, 8) as u32,
                d2: b(8, 4) as u16,
                d3: b(12, 4) as u16,
                d4,
            }
        }

        pub fn text(&self) -> String {
            format!(
                "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}",
                self.d1,
                self.d2,
                self.d3,
                self.d4[0],
                self.d4[1],
                self.d4[2..]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        }
    }

    #[link(name = "powrprof")]
    extern "system" {
        fn PowerGetActiveScheme(root: isize, active: *mut *mut Guid) -> u32;
        fn PowerSetActiveScheme(root: isize, scheme: *const Guid) -> u32;
        fn PowerReadACValueIndex(
            root: isize,
            scheme: *const Guid,
            sub: *const Guid,
            setting: *const Guid,
            value: *mut u32,
        ) -> u32;
        fn PowerWriteACValueIndex(
            root: isize,
            scheme: *const Guid,
            sub: *const Guid,
            setting: *const Guid,
            value: u32,
        ) -> u32;
        fn PowerDuplicateScheme(root: isize, source: *const Guid, dest: *mut *mut Guid) -> u32;
        fn PowerReadFriendlyName(
            root: isize,
            scheme: *const Guid,
            sub: *const Guid,
            setting: *const Guid,
            buf: *mut u8,
            len: *mut u32,
        ) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    pub fn active() -> Option<Guid> {
        let mut p: *mut Guid = std::ptr::null_mut();
        if unsafe { PowerGetActiveScheme(0, &mut p) } != 0 || p.is_null() {
            return None;
        }
        let g = unsafe { *p };
        unsafe { LocalFree(p as *mut _) };
        Some(g)
    }

    pub fn set_active(g: &Guid) -> Result<(), String> {
        match unsafe { PowerSetActiveScheme(0, g) } {
            0 => Ok(()),
            e => Err(format!("PowerSetActiveScheme: {e}")),
        }
    }

    pub fn read_ac(sub: &Guid, setting: &Guid) -> Option<u32> {
        let scheme = active()?;
        let mut v = 0u32;
        (unsafe { PowerReadACValueIndex(0, &scheme, sub, setting, &mut v) } == 0).then_some(v)
    }

    /// Zapisuje wartość w aktywnym planie i aktywuje go ponownie (żeby zadziałała).
    pub fn write_ac(sub: &Guid, setting: &Guid, value: u32) -> Result<(), String> {
        let scheme = active().ok_or("PowerGetActiveScheme")?;
        match unsafe { PowerWriteACValueIndex(0, &scheme, sub, setting, value) } {
            0 => set_active(&scheme),
            e => Err(format!("PowerWriteACValueIndex: {e}")),
        }
    }

    pub fn duplicate(source: &Guid) -> Result<Guid, String> {
        let mut p: *mut Guid = std::ptr::null_mut();
        match unsafe { PowerDuplicateScheme(0, source, &mut p) } {
            0 if !p.is_null() => {
                let g = unsafe { *p };
                unsafe { LocalFree(p as *mut _) };
                Ok(g)
            }
            e => Err(format!("PowerDuplicateScheme: {e}")),
        }
    }

    /// Nazwa planu (do rozpoznania istniejącego planu "Ultimate Performance").
    pub fn name(scheme: &Guid) -> String {
        let mut len = 0u32;
        unsafe {
            PowerReadFriendlyName(
                0,
                scheme,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if len == 0 {
            return String::new();
        }
        let mut buf = vec![0u8; len as usize];
        if unsafe {
            PowerReadFriendlyName(
                0,
                scheme,
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                &mut len,
            )
        } != 0
        {
            return String::new();
        }
        let w: Vec<u16> = buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&w)
            .trim_end_matches('\0')
            .to_owned()
    }
}

// ------------------------------------------------------------- pozycje

/// Grupa pozycji.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Group {
    /// Udokumentowane przez Microsoft albo producentów.
    Proven,
    /// Do sprawdzenia na własnym sprzęcie.
    Test,
}

/// Stan pozycji odczytany z systemu.
#[derive(Clone, PartialEq, Debug)]
pub enum State {
    /// Zalecana wartość jest ustawiona (z opisem bieżącej).
    Optimized(String),
    /// Wartość domyślna / inna niż zalecana (z opisem bieżącej).
    Default(String),
    /// Pozycja nie dotyczy tego komputera (z powodem).
    Unsupported(String),
    /// Nie udało się odczytać.
    Unknown(String),
}

/// Jedna pozycja w zakładce.
#[derive(Clone, Debug)]
pub struct Item {
    pub key: String,
    pub name: String,
    pub desc: &'static str,
    pub group: Group,
    pub restart: bool,
    pub state: State,
    /// Ostrzeżenie (np. procesory X3D przy planie Ultimate Performance).
    pub warning: Option<String>,
}

/// Kopia oryginalnych wartości (klucz pozycji → wartość).
#[derive(Serialize, Deserialize, Default)]
struct Backup {
    #[serde(default)]
    values: BTreeMap<String, serde_json::Value>,
}

fn backup_path() -> Option<std::path::PathBuf> {
    // TC_TUNE_BACKUP: inny plik kopii (np. zmiany zrobione testem dla
    // zainstalowanego programu).
    if let Ok(p) = std::env::var("TC_TUNE_BACKUP") {
        return Some(p.into());
    }
    Some(
        std::env::current_exe()
            .ok()?
            .parent()?
            .join("tune-backup.json"),
    )
}

fn load_backup() -> Backup {
    backup_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_backup(b: &Backup) {
    if let (Some(p), Ok(s)) = (backup_path(), serde_json::to_string_pretty(b)) {
        let _ = std::fs::write(p, s);
    }
}

/// Zapamiętuje oryginał tylko przy pierwszej zmianie pozycji.
fn remember(key: &str, value: serde_json::Value) {
    let mut b = load_backup();
    b.values.entry(key.to_owned()).or_insert(value);
    save_backup(&b);
}

fn forget(key: &str) {
    let mut b = load_backup();
    if b.values.remove(key).is_some() {
        save_backup(&b);
    }
}

fn original(key: &str) -> Option<serde_json::Value> {
    load_backup().values.get(key).cloned()
}

// ---- pozycje oparte na wartości DWORD w rejestrze

#[cfg(windows)]
struct RegTweak {
    key: &'static str,
    root: reg::Hkey,
    path: &'static str,
    value: &'static str,
    optimized: u32,
    /// Wartość, gdy w rejestrze jej brak (zachowanie Windows).
    absent: Option<u32>,
}

#[cfg(windows)]
const REG_TWEAKS: &[RegTweak] = &[
    RegTweak {
        key: "hvci",
        root: reg::HKLM,
        path: r"SYSTEM\CurrentControlSet\Control\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity",
        value: "Enabled",
        optimized: 0,
        absent: Some(0),
    },
    RegTweak {
        key: "game_mode",
        root: reg::HKCU,
        path: r"Software\Microsoft\GameBar",
        value: "AutoGameModeEnabled",
        optimized: 1,
        absent: Some(1),
    },
    // Game Bar: nagrywanie gier (dwie wartości - druga w `game_dvr_capture`).
    RegTweak {
        key: "game_dvr",
        root: reg::HKCU,
        path: r"System\GameConfigStore",
        value: "GameDVR_Enabled",
        optimized: 0,
        absent: Some(1),
    },
    RegTweak {
        key: "game_dvr_capture",
        root: reg::HKCU,
        path: r"SOFTWARE\Microsoft\Windows\CurrentVersion\GameDVR",
        value: "AppCaptureEnabled",
        optimized: 0,
        absent: Some(1),
    },
    // Odstęp danych z myszy dla okien w tle (ms): domyślnie 8, najmniej 3.
    RegTweak {
        key: "mouse_background",
        root: reg::HKCU,
        path: r"Control Panel\Mouse",
        value: "RawMouseThrottleDuration",
        optimized: 3,
        absent: Some(8),
    },
    RegTweak {
        key: "fth",
        root: reg::HKLM,
        path: r"SOFTWARE\Microsoft\FTH",
        value: "Enabled",
        optimized: 0,
        absent: Some(1),
    },
    RegTweak {
        key: "fast_startup",
        root: reg::HKLM,
        path: r"SYSTEM\CurrentControlSet\Control\Session Manager\Power",
        value: "HiberbootEnabled",
        optimized: 0,
        absent: Some(1),
    },
    // Dynamic Lighting (Windows 11): sterowanie podświetleniem przez system.
    RegTweak {
        key: "dynamic_lighting",
        root: reg::HKCU,
        path: r"Software\Microsoft\Lighting",
        value: "AmbientLightingEnabled",
        optimized: 0,
        absent: Some(1),
    },
    RegTweak {
        key: "priority",
        root: reg::HKLM,
        path: r"SYSTEM\CurrentControlSet\Control\PriorityControl",
        value: "Win32PrioritySeparation",
        optimized: 0x26,
        absent: Some(2),
    },
    RegTweak {
        key: "responsiveness",
        root: reg::HKLM,
        path: r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile",
        value: "SystemResponsiveness",
        optimized: 10,
        absent: Some(20),
    },
    RegTweak {
        // Wyłączenie Multi-Plane Overlay (wartość z poprawki NVIDIA mpo_disable).
        key: "mpo",
        root: reg::HKLM,
        path: r"SOFTWARE\Microsoft\Windows\Dwm",
        value: "OverlayTestMode",
        optimized: 5,
        absent: None,
    },
];

/// Opis bieżącej wartości pozycji z rejestru, np. "Win32PrioritySeparation = 0x26
/// (38)" albo "brak wpisu - Windows używa 20".
#[cfg(windows)]
fn reg_value_text(t: &RegTweak, raw: Option<u32>) -> String {
    let num = |v: u32| {
        if t.key == "priority" {
            format!("0x{v:X} ({v})")
        } else {
            v.to_string()
        }
    };
    // Znaczenie wartości tam, gdzie to włącz / wyłącz.
    let meaning = |v: u32| match (t.key, v) {
        ("hvci", 0) => crate::i18n::tr(" - wyłączona", " - off"),
        ("hvci", _) => crate::i18n::tr(" - włączona", " - on"),
        ("game_mode", 0) => crate::i18n::tr(" - wyłączony", " - off"),
        ("game_mode", _) => crate::i18n::tr(" - włączony", " - on"),
        ("game_dvr" | "game_dvr_capture" | "fth" | "fast_startup" | "dynamic_lighting", 0) => {
            crate::i18n::tr(" - wyłączone", " - off")
        }
        ("game_dvr" | "game_dvr_capture" | "fth" | "fast_startup" | "dynamic_lighting", _) => {
            crate::i18n::tr(" - włączone", " - on")
        }
        ("mouse_background", _) => crate::i18n::tr(" ms", " ms"),
        ("mpo", 5) => crate::i18n::tr(" - MPO wyłączone", " - MPO off"),
        _ => "",
    };
    match (raw, t.absent) {
        (Some(v), _) => format!("{} = {}{}", t.value, num(v), meaning(v)),
        (None, Some(d)) => crate::trf!(
            "brak wpisu {} - Windows używa {}{}",
            "no {} entry - Windows uses {}{}",
            t.value,
            num(d),
            meaning(d)
        ),
        (None, None) if t.key == "mpo" => crate::i18n::tr(
            "brak wpisu OverlayTestMode - MPO włączone (domyślnie)",
            "no OverlayTestMode entry - MPO on (default)",
        )
        .to_owned(),
        (None, None) => crate::trf!("brak wpisu {}", "no {} entry", t.value),
    }
}

#[cfg(windows)]
fn reg_state(t: &RegTweak, path: &str) -> State {
    let raw = reg::get_dword(t.root, path, t.value);
    let text = reg_value_text(t, raw);
    match raw.or(t.absent) {
        Some(v) if v == t.optimized => State::Optimized(text),
        _ => State::Default(text),
    }
}

/// Wartość ustawienia planu zasilania po ludzku.
fn power_text(key: &str, v: u32) -> String {
    if key.starts_with("cpu_parking") {
        return if v >= 100 {
            crate::i18n::tr(
                "100% rdzeni zawsze aktywnych - bez parkowania",
                "100% of cores always active - no parking",
            )
            .to_owned()
        } else {
            crate::trf!(
                "co najmniej {v}% rdzeni aktywnych - reszta może być parkowana",
                "at least {v}% of cores active - the rest may be parked"
            )
        };
    }
    let meaning = match (key, v) {
        (_, 0) => crate::i18n::tr("wyłączone", "off"),
        ("pcie_aspm", 1) => crate::i18n::tr("umiarkowane oszczędzanie", "moderate power savings"),
        ("pcie_aspm", 2) => crate::i18n::tr("maksymalne oszczędzanie", "maximum power savings"),
        _ => crate::i18n::tr("włączone", "on"),
    };
    crate::trf!("{meaning} (indeks {v})", "{meaning} (index {v})")
}

/// Wartość słowa kluczowego karty sieciowej po ludzku (0 / 1).
fn on_off(v: &str) -> String {
    match v {
        "0" => crate::trf!("{v} - wyłączone", "{v} - off"),
        "1" => crate::trf!("{v} - włączone", "{v} - on"),
        _ => v.to_owned(),
    }
}

#[cfg(windows)]
fn reg_set(
    key: &str,
    root: reg::Hkey,
    path: &str,
    value: &str,
    optimized: u32,
    on: bool,
) -> Result<(), String> {
    if on {
        let current = reg::get_dword(root, path, value);
        remember(key, serde_json::json!(current));
        reg::set_dword(root, path, value, Some(optimized))?;
        // Sprawdzenie odczytem.
        if reg::get_dword(root, path, value) != Some(optimized) {
            return Err(crate::trf!(
                "{value}: zapis nie został potwierdzony odczytem",
                "{value}: the write was not confirmed by reading back"
            ));
        }
    } else {
        let orig: Option<u32> = original(key)
            .and_then(|v| serde_json::from_value(v).ok())
            .flatten();
        // Bez kopii (zmienione poza programem) - usuwamy wartość, Windows wraca do domyślnej.
        reg::set_dword(root, path, value, orig)?;
        if reg::get_dword(root, path, value) != orig {
            return Err(crate::trf!(
                "{value}: przywrócenie nie zostało potwierdzone odczytem",
                "{value}: the restore was not confirmed by reading back"
            ));
        }
        forget(key);
    }
    Ok(())
}

// ---- usługi Windows przez menedżera usług (nie przez rejestr: menedżer
// trzyma konfigurację w pamięci i potrafi nadpisać wpis zmieniony w rejestrze)

#[cfg(windows)]
mod service {
    use std::ffi::c_void;

    type Handle = *mut c_void;
    const SC_MANAGER_CONNECT: u32 = 0x0001;
    const SERVICE_QUERY_CONFIG: u32 = 0x0001;
    const SERVICE_CHANGE_CONFIG: u32 = 0x0002;
    const SERVICE_QUERY_STATUS: u32 = 0x0004;
    const SERVICE_START: u32 = 0x0010;
    const SERVICE_STOP: u32 = 0x0020;
    const SERVICE_NO_CHANGE: u32 = 0xFFFF_FFFF;
    const SERVICE_CONTROL_STOP: u32 = 1;
    pub const AUTO: u32 = 2;
    pub const DISABLED: u32 = 4;
    const RUNNING: u32 = 4;
    const STOPPED: u32 = 1;

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenSCManagerW(machine: *const u16, db: *const u16, access: u32) -> Handle;
        fn OpenServiceW(scm: Handle, name: *const u16, access: u32) -> Handle;
        fn CloseServiceHandle(h: Handle) -> i32;
        fn QueryServiceConfigW(h: Handle, cfg: *mut u8, size: u32, needed: *mut u32) -> i32;
        fn QueryServiceStatus(h: Handle, status: *mut [u32; 7]) -> i32;
        fn ChangeServiceConfigW(
            h: Handle,
            kind: u32,
            start: u32,
            error: u32,
            path: *const u16,
            group: *const u16,
            tag: *mut u32,
            deps: *const u16,
            user: *const u16,
            password: *const u16,
            display: *const u16,
        ) -> i32;
        fn ControlService(h: Handle, control: u32, status: *mut [u32; 7]) -> i32;
        fn StartServiceW(h: Handle, argc: u32, argv: *const *const u16) -> i32;
    }

    struct H(Handle);
    impl Drop for H {
        fn drop(&mut self) {
            // SAFETY: uchwyt z OpenSCManagerW / OpenServiceW, zamykany raz.
            unsafe { CloseServiceHandle(self.0) };
        }
    }

    fn last_error(what: &str) -> String {
        format!("{what}: {}", std::io::Error::last_os_error())
    }

    fn open(name: &str, access: u32) -> Result<(H, H), String> {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: zwykłe wywołania API; uchwyty zamyka `H`.
        let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if scm.is_null() {
            return Err(last_error("OpenSCManager"));
        }
        let scm = H(scm);
        let svc = unsafe { OpenServiceW(scm.0, wide.as_ptr(), access) };
        if svc.is_null() {
            return Err(last_error(name));
        }
        Ok((scm, H(svc)))
    }

    /// Typ uruchamiania (2 = automatycznie, 3 = ręcznie, 4 = wyłączona) i czy działa.
    pub fn state(name: &str) -> Result<(u32, bool), String> {
        let (_scm, svc) = open(name, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
        let mut buf = vec![0u8; 8192];
        let mut needed = 0u32;
        // SAFETY: bufor ma podany rozmiar; QUERY_SERVICE_CONFIGW zaczyna się od
        // dwServiceType i dwStartType (u32).
        if unsafe { QueryServiceConfigW(svc.0, buf.as_mut_ptr(), buf.len() as u32, &mut needed) }
            == 0
        {
            return Err(last_error(name));
        }
        let start = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let mut status = [0u32; 7];
        // SAFETY: SERVICE_STATUS = 7 × u32; drugie pole to stan.
        let running =
            unsafe { QueryServiceStatus(svc.0, &mut status) } != 0 && status[1] == RUNNING;
        Ok((start, running))
    }

    /// Ustawia typ uruchamiania; przy wyłączeniu zatrzymuje usługę, przy
    /// automatycznym - uruchamia ją.
    pub fn set(name: &str, start: u32) -> Result<(), String> {
        let (_scm, svc) = open(
            name,
            SERVICE_CHANGE_CONFIG | SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP,
        )?;
        let null = std::ptr::null();
        // SAFETY: zmieniamy tylko typ uruchamiania (reszta SERVICE_NO_CHANGE / null).
        let ok = unsafe {
            ChangeServiceConfigW(
                svc.0,
                SERVICE_NO_CHANGE,
                start,
                SERVICE_NO_CHANGE,
                null,
                null,
                std::ptr::null_mut(),
                null,
                null,
                null,
                null,
            )
        };
        if ok == 0 {
            return Err(last_error(name));
        }
        let mut status = [0u32; 7];
        // SAFETY: jak wyżej.
        unsafe { QueryServiceStatus(svc.0, &mut status) };
        if start == DISABLED && status[1] != STOPPED {
            // Zatrzymanie od razu (błąd nie jest krytyczny - typ już zmieniony).
            unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut status) };
        } else if start == AUTO && status[1] == STOPPED {
            unsafe { StartServiceW(svc.0, 0, std::ptr::null()) };
        }
        Ok(())
    }
}

#[cfg(windows)]
fn service_text(start: u32, running: bool) -> String {
    let kind = match start {
        2 => crate::i18n::tr("uruchamiana automatycznie", "starts automatically"),
        3 => crate::i18n::tr("uruchamiana ręcznie", "manual start"),
        4 => crate::i18n::tr("wyłączona", "disabled"),
        _ => "?",
    };
    let now = if running {
        crate::i18n::tr("działa", "running")
    } else {
        crate::i18n::tr("zatrzymana", "stopped")
    };
    crate::trf!(
        "usługa {kind}, teraz {now} (typ {start})",
        "service {kind}, now {now} (type {start})"
    )
}

/// SysMain: włączone = usługa wyłączona. Bez kopii powrót do automatycznej.
#[cfg(windows)]
fn sysmain_set(on: bool) -> Result<(), String> {
    const NAME: &str = "SysMain";
    let key = "sysmain";
    let (current, _) = service::state(NAME)?;
    let target = if on {
        remember(key, serde_json::json!(current));
        service::DISABLED
    } else {
        original(key)
            .and_then(|v| serde_json::from_value::<Option<u32>>(v).ok().flatten())
            .filter(|t| (2..=4).contains(t))
            .unwrap_or(service::AUTO)
    };
    service::set(NAME, target)?;
    if service::state(NAME)?.0 != target {
        return Err(crate::i18n::tr(
            "Zmiana usługi SysMain nie została potwierdzona odczytem.",
            "The SysMain change was not confirmed by reading back.",
        )
        .into());
    }
    if !on {
        forget(key);
    }
    Ok(())
}

// ---- pozycje spoza pojedynczych wartości DWORD

/// Numer kompilacji Windows (np. 26100); 0, gdy nie da się odczytać.
#[cfg(windows)]
fn windows_build() -> u32 {
    reg::get_string(
        reg::HKLM,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuild",
    )
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

/// Ustawienia DirectX dla wszystkich gier: tekst "Klucz=wartość;Klucz2=wartość;".
mod dx_settings {
    pub const PATH: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
    pub const VALUE: &str = "DirectXUserGlobalSettings";
    /// Optymalizacje dla gier w oknie (nowoczesny model prezentacji).
    pub const KEY: &str = "SwapEffectUpgradeEnable";

    pub fn get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
        text.split(';')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
            .map(|(_, v)| v.trim())
    }

    /// Tekst z ustawionym (`Some`) albo usuniętym (`None`) kluczem; inne bez zmian.
    pub fn with(text: &str, key: &str, value: Option<&str>) -> String {
        let mut out = String::new();
        let mut done = false;
        for part in text.split(';').filter(|p| !p.trim().is_empty()) {
            let k = part.split_once('=').map_or(part, |(k, _)| k).trim();
            if k.eq_ignore_ascii_case(key) {
                if let (Some(v), false) = (value, done) {
                    out.push_str(&format!("{key}={v};"));
                    done = true;
                }
            } else {
                out.push_str(part.trim());
                out.push(';');
            }
        }
        if let (Some(v), false) = (value, done) {
            out.push_str(&format!("{key}={v};"));
        }
        out
    }
}

#[cfg(windows)]
fn windowed_state() -> State {
    let text = reg::get_string(reg::HKCU, dx_settings::PATH, dx_settings::VALUE);
    let on = text
        .as_deref()
        .and_then(|t| dx_settings::get(t, dx_settings::KEY))
        == Some("1");
    let desc = if on {
        crate::i18n::tr("włączone", "on")
    } else {
        crate::i18n::tr("wyłączone", "off")
    };
    let raw = text.map_or_else(
        || crate::i18n::tr("brak ustawień DirectX", "no DirectX settings").to_owned(),
        |t| format!("{} = \"{t}\"", dx_settings::VALUE),
    );
    let text = format!("{desc} ({raw})");
    if on {
        State::Optimized(text)
    } else {
        State::Default(text)
    }
}

#[cfg(windows)]
fn windowed_set(on: bool) -> Result<(), String> {
    let key = "windowed_games";
    let current = reg::get_string(reg::HKCU, dx_settings::PATH, dx_settings::VALUE);
    let target = if on {
        remember(key, serde_json::json!(current));
        Some(dx_settings::with(
            current.as_deref().unwrap_or(""),
            dx_settings::KEY,
            Some("1"),
        ))
    } else {
        match original(key) {
            // Dokładnie poprzedni tekst (albo brak wartości).
            Some(v) => serde_json::from_value::<Option<String>>(v).ok().flatten(),
            // Bez kopii: usuwamy tylko nasz klucz.
            None => Some(dx_settings::with(
                current.as_deref().unwrap_or(""),
                dx_settings::KEY,
                None,
            ))
            .filter(|t| !t.is_empty()),
        }
    };
    reg::set_string(
        reg::HKCU,
        dx_settings::PATH,
        dx_settings::VALUE,
        target.as_deref(),
    )?;
    if reg::get_string(reg::HKCU, dx_settings::PATH, dx_settings::VALUE) != target {
        return Err(crate::i18n::tr(
            "Zapis ustawień DirectX nie został potwierdzony odczytem.",
            "The DirectX settings write was not confirmed by reading back.",
        )
        .into());
    }
    if !on {
        forget(key);
    }
    Ok(())
}

/// Przyspieszanie wskaźnika myszy („Popraw precyzję wskaźnika”): progi 1 i 2
/// oraz poziom przyspieszenia; 0 = ruch liniowy.
#[cfg(windows)]
mod mouse {
    #[link(name = "user32")]
    extern "system" {
        fn SystemParametersInfoW(
            action: u32,
            param: u32,
            pv: *mut core::ffi::c_void,
            ini: u32,
        ) -> i32;
    }
    const SPI_GETMOUSE: u32 = 0x0003;
    const SPI_SETMOUSE: u32 = 0x0004;
    const SPIF_UPDATEINIFILE: u32 = 0x01;
    const SPIF_SENDCHANGE: u32 = 0x02;
    /// Wartości domyślne Windows.
    pub const DEFAULT: [i32; 3] = [6, 10, 1];

    pub fn get() -> Option<[i32; 3]> {
        let mut v = [0i32; 3];
        // SAFETY: SPI_GETMOUSE zapisuje dokładnie trzy liczby int.
        let ok = unsafe { SystemParametersInfoW(SPI_GETMOUSE, 0, v.as_mut_ptr().cast(), 0) };
        (ok != 0).then_some(v)
    }

    /// Ustawia od razu i zapisuje w profilu użytkownika.
    pub fn set(mut v: [i32; 3]) -> Result<(), String> {
        // SAFETY: SPI_SETMOUSE czyta trzy liczby int.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_SETMOUSE,
                0,
                v.as_mut_ptr().cast(),
                SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn mouse_state() -> State {
    match mouse::get() {
        Some(v) if v[2] == 0 => State::Optimized(crate::trf!(
            "przyspieszanie wyłączone (progi {} / {}, poziom {})",
            "acceleration off (thresholds {} / {}, level {})",
            v[0],
            v[1],
            v[2]
        )),
        Some(v) => State::Default(crate::trf!(
            "przyspieszanie włączone (progi {} / {}, poziom {})",
            "acceleration on (thresholds {} / {}, level {})",
            v[0],
            v[1],
            v[2]
        )),
        None => State::Unknown(crate::i18n::tr("brak odczytu", "cannot read").into()),
    }
}

#[cfg(windows)]
fn mouse_set(on: bool) -> Result<(), String> {
    let key = "mouse_accel";
    let current = mouse::get().ok_or("SPI_GETMOUSE")?;
    let target = if on {
        remember(key, serde_json::json!(current));
        [0, 0, 0]
    } else {
        original(key)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(mouse::DEFAULT)
    };
    mouse::set(target)?;
    if mouse::get() != Some(target) {
        return Err(crate::i18n::tr(
            "Ustawienie myszy nie zostało potwierdzone odczytem.",
            "The mouse setting was not confirmed by reading back.",
        )
        .into());
    }
    if !on {
        forget(key);
    }
    Ok(())
}

/// Kompresja pamięci i obecność dysków HDD - jedno wywołanie PowerShell.
#[cfg(windows)]
fn memory_and_disks() -> Result<(bool, usize), String> {
    let out = powershell(
        "$m=(Get-MMAgent).MemoryCompression; $h=@(Get-PhysicalDisk | Where-Object MediaType -eq 'HDD').Count; \"$m;$h\"",
    )?;
    let (m, h) = out
        .trim()
        .split_once(';')
        .ok_or_else(|| out.trim().to_owned())?;
    Ok((m.eq_ignore_ascii_case("true"), h.parse().unwrap_or(0)))
}

#[cfg(windows)]
fn memory_compression_set(on: bool) -> Result<(), String> {
    // `on` = zalecane, czyli kompresja WYŁĄCZONA.
    let key = "memory_compression";
    let (current, _) = memory_and_disks()?;
    let target = if on {
        remember(key, serde_json::json!(current));
        false
    } else {
        original(key)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(true)
    };
    let cmd = if target {
        "Enable-MMAgent -MemoryCompression"
    } else {
        "Disable-MMAgent -MemoryCompression"
    };
    powershell(cmd)?;
    if memory_and_disks()?.0 != target {
        return Err(crate::i18n::tr(
            "Zmiana kompresji pamięci nie została potwierdzona odczytem.",
            "The memory compression change was not confirmed by reading back.",
        )
        .into());
    }
    if !on {
        forget(key);
    }
    Ok(())
}

// ---- tryb MSI przerwań

#[cfg(windows)]
mod devices {
    use super::reg;

    #[link(name = "cfgmgr32")]
    extern "system" {
        fn CM_Locate_DevNodeW(dev: *mut u32, id: *const u16, flags: u32) -> u32;
    }

    /// Urządzenie PCI z możliwą obsługą MSI.
    pub struct Device {
        pub id: String,
        pub name: String,
        pub class: String,
    }

    pub fn msi_path(id: &str) -> String {
        format!(
            r"SYSTEM\CurrentControlSet\Enum\{id}\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties"
        )
    }

    fn present(id: &str) -> bool {
        let w: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        let mut dev = 0u32;
        unsafe { CM_Locate_DevNodeW(&mut dev, w.as_ptr(), 0) == 0 }
    }

    /// Obecne urządzenia PCI: karty graficzne, sieciowe, kontrolery audio i USB.
    pub fn list() -> Vec<Device> {
        let mut out = Vec::new();
        let base = r"SYSTEM\CurrentControlSet\Enum\PCI";
        for dev in reg::subkeys(reg::HKLM, base) {
            for inst in reg::subkeys(reg::HKLM, &format!(r"{base}\{dev}")) {
                let id = format!(r"PCI\{dev}\{inst}");
                let path = format!(r"SYSTEM\CurrentControlSet\Enum\{id}");
                // Windows 11 nie zapisuje już nazwy klasy, tylko jej GUID.
                let guid = reg::get_string(reg::HKLM, &path, "ClassGUID")
                    .unwrap_or_default()
                    .to_lowercase();
                let class = match guid.as_str() {
                    "{4d36e968-e325-11ce-bfc1-08002be10318}" => "Display",
                    "{4d36e972-e325-11ce-bfc1-08002be10318}" => "Net",
                    "{4d36e96c-e325-11ce-bfc1-08002be10318}" => "Media",
                    "{36fc9e60-c465-11cf-8056-444553540000}" => "USB",
                    _ => "",
                }
                .to_owned();
                let name = reg::get_string(reg::HKLM, &path, "FriendlyName")
                    .or_else(|| reg::get_string(reg::HKLM, &path, "DeviceDesc"))
                    .map(|n| n.rsplit(';').next().unwrap_or(&n).to_owned())
                    .unwrap_or_default();
                let audio = name.to_lowercase().contains("audio");
                let wanted = !class.is_empty() || audio;
                // Producent z identyfikatora - odróżnia np. dwa kontrolery audio.
                let vendor = match dev.get(4..8) {
                    Some("10DE") => "NVIDIA ",
                    Some("8086") => "Intel ",
                    Some("1002") => "AMD ",
                    Some("1022") => "AMD ",
                    Some("10EC") => "Realtek ",
                    _ => "",
                };
                // Opis kontrolera USB bywa szablonem z parametrami ("(Intel(R),3.20,...)").
                let name = if name.starts_with('(') || name.contains('%') {
                    match class.as_str() {
                        "USB" => format!("{vendor}USB (xHCI)"),
                        _ => format!("{vendor}{dev}"),
                    }
                } else {
                    name
                };
                let name = if name.starts_with(vendor.trim()) {
                    name
                } else {
                    format!("{vendor}{name}")
                };
                if !wanted || !present(&id) {
                    continue;
                }
                // Tylko urządzenia, których sterownik w ogóle zarządza przerwaniami MSI.
                if !reg::exists(
                    reg::HKLM,
                    &format!(r"{path}\Device Parameters\Interrupt Management"),
                ) {
                    continue;
                }
                out.push(Device { id, name, class });
            }
        }
        out.sort_by(|a, b| a.class.cmp(&b.class).then(a.name.cmp(&b.name)));
        out
    }
}

// ---- karta sieciowa (PowerShell NetAdapter)

#[cfg(windows)]
fn powershell(script: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(300)
            .collect());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Deserialize, Serialize, Clone, Debug)]
struct NicValue {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Value")]
    value: String,
}

/// Wartości słowa kluczowego (np. `*InterruptModeration`) na fizycznych kartach.
#[cfg(windows)]
fn nic_values(keyword: &str) -> Result<Vec<NicValue>, String> {
    let script = format!(
        "$r = @(Get-NetAdapter -Physical | Get-NetAdapterAdvancedProperty -RegistryKeyword '{keyword}' -ErrorAction SilentlyContinue | ForEach-Object {{ [pscustomobject]@{{ Name = $_.Name; Value = ($_.RegistryValue -join '') }} }}); ConvertTo-Json -InputObject $r -Compress"
    );
    let out = powershell(&script)?;
    let out = out.trim();
    if out.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(out).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn nic_set(name: &str, keyword: &str, value: &str) -> Result<(), String> {
    let name = name.replace('\'', "''");
    powershell(&format!(
        "Set-NetAdapterAdvancedProperty -Name '{name}' -RegistryKeyword '{keyword}' -RegistryValue '{value}' -ErrorAction Stop"
    ))
    .map(|_| ())
}

#[cfg(windows)]
const NIC_TWEAKS: &[(&str, &str, &str)] = &[
    // (klucz pozycji, słowo kluczowe, wartość zalecana)
    ("nic_moderation", "*InterruptModeration", "0"),
    ("nic_eee", "*EEE", "0"),
];

// ---- karta sieciowa: oszczędzanie energii (ustawienia producenta + NDIS)

/// Słowa kluczowe oszczędzania energii w sterownikach kart sieciowych; karta
/// ma zwykle tylko część z nich. "0" wyłącza każde z nich.
#[cfg(windows)]
const NIC_POWER_KEYWORDS: &[(&str, &str)] = &[
    ("EnableGreenEthernet", "Green Ethernet"),
    ("GigaLite", "Gigabit Lite"),
    ("AdvancedEEE", "Advanced EEE"),
    ("PowerSavingMode", "Power Saving Mode"),
    ("AutoDisableGigabit", "Auto Disable Gigabit"),
    ("ULPMode", "Ultra Low Power Mode"),
    ("*SelectiveSuspend", "Selective Suspend"),
];

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
struct NicKeyword {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Keyword")]
    keyword: String,
    #[serde(rename = "Value")]
    value: String,
}

/// Zarządzanie energią karty w Windows (Get-NetAdapterPowerManagement):
/// „pozwól komputerowi wyłączać urządzenie” i selektywne wstrzymywanie NDIS.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
struct NicPm {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Allow")]
    allow: String,
    #[serde(rename = "Suspend")]
    suspend: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
struct NicPower {
    #[serde(default)]
    adv: Vec<NicKeyword>,
    #[serde(default)]
    pm: Vec<NicPm>,
}

impl NicPower {
    /// Wszystko, co karta ma, jest wyłączone ("Unsupported"/"Inactive" się nie liczy).
    fn off(&self) -> bool {
        self.adv.iter().all(|k| k.value == "0")
            && self
                .pm
                .iter()
                .all(|p| p.allow != "Enabled" && p.suspend != "Enabled")
    }

    fn empty(&self) -> bool {
        self.adv.is_empty()
            && self
                .pm
                .iter()
                .all(|p| p.allow == "Unsupported" && p.suspend == "Unsupported")
    }

    /// Opis bieżących ustawień, np. "Ethernet: Selective Suspend włączone, ...".
    fn text(&self) -> String {
        let state = |on: bool| {
            if on {
                crate::i18n::tr("włączone", "on")
            } else {
                crate::i18n::tr("wyłączone", "off")
            }
        };
        let mut parts = Vec::new();
        for p in &self.pm {
            let mut list = Vec::new();
            if p.allow != "Unsupported" {
                list.push(crate::trf!(
                    "wyłączanie przez system {}",
                    "turn off by the system {}",
                    state(p.allow == "Enabled")
                ));
            }
            if p.suspend != "Unsupported" && p.suspend != "Inactive" {
                list.push(format!(
                    "Selective Suspend {}",
                    state(p.suspend == "Enabled")
                ));
            }
            for k in self.adv.iter().filter(|k| k.name == p.name) {
                let label = NIC_POWER_KEYWORDS
                    .iter()
                    .find(|(kw, _)| *kw == k.keyword)
                    .map_or(k.keyword.as_str(), |(_, l)| l);
                list.push(format!("{label} {}", state(k.value != "0")));
            }
            if !list.is_empty() {
                parts.push(format!("{}: {}", p.name, list.join(", ")));
            }
        }
        parts.join("; ")
    }
}

#[cfg(windows)]
fn nic_power() -> Result<NicPower, String> {
    let keywords = NIC_POWER_KEYWORDS
        .iter()
        .map(|(k, _)| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "$k = @({keywords}); $a = @(Get-NetAdapter -Physical); \
         $adv = @($a | ForEach-Object {{ $n = $_.Name; Get-NetAdapterAdvancedProperty -Name $n -AllProperties -ErrorAction SilentlyContinue | Where-Object {{ $k -contains $_.RegistryKeyword }} | ForEach-Object {{ [pscustomobject]@{{ Name = $n; Keyword = $_.RegistryKeyword; Value = ($_.RegistryValue -join '') }} }} }}); \
         $pm = @($a | Get-NetAdapterPowerManagement -ErrorAction SilentlyContinue | ForEach-Object {{ [pscustomobject]@{{ Name = $_.Name; Allow = [string]$_.AllowComputerToTurnOffDevice; Suspend = [string]$_.SelectiveSuspend }} }}); \
         ConvertTo-Json -InputObject @{{ adv = $adv; pm = $pm }} -Compress -Depth 4"
    );
    let out = powershell(&script)?;
    serde_json::from_str(out.trim()).map_err(|e| e.to_string())
}

/// Zapisuje stan `target` (słowa kluczowe i zarządzanie energią) na kartach.
#[cfg(windows)]
fn nic_power_write(target: &NicPower) -> Result<(), String> {
    let q = |s: &str| s.replace('\'', "''");
    let mut script = String::new();
    for k in &target.adv {
        script.push_str(&format!(
            "Set-NetAdapterAdvancedProperty -Name '{}' -AllProperties -RegistryKeyword '{}' -RegistryValue '{}' -ErrorAction Stop; ",
            q(&k.name),
            q(&k.keyword),
            q(&k.value)
        ));
    }
    for p in &target.pm {
        let mut args = String::new();
        if matches!(p.allow.as_str(), "Enabled" | "Disabled") {
            args.push_str(&format!(" -AllowComputerToTurnOffDevice {}", p.allow));
        }
        if matches!(p.suspend.as_str(), "Enabled" | "Disabled") {
            args.push_str(&format!(" -SelectiveSuspend {}", p.suspend));
        }
        if !args.is_empty() {
            script.push_str(&format!(
                "Set-NetAdapterPowerManagement -Name '{}'{args} -ErrorAction Stop; ",
                q(&p.name)
            ));
        }
    }
    if script.is_empty() {
        return Ok(());
    }
    powershell(&script).map(|_| ())
}

// ---- funkcja Windows: Virtual Machine Platform (DISM)

#[cfg(windows)]
fn dism(args: &[&str]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("dism.exe")
        .args(["/English", "/Online"])
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // 3010 = wymagany restart (sukces).
    match out.status.code() {
        Some(0) | Some(3010) => Ok(text),
        c => Err(format!(
            "DISM {c:?}: {}",
            text.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
        )),
    }
}

#[cfg(windows)]
fn vmp_enabled() -> Result<Option<bool>, String> {
    let out = dism(&["/Get-FeatureInfo", "/FeatureName:VirtualMachinePlatform"])?;
    let state = out
        .lines()
        .find(|l| l.trim_start().starts_with("State :"))
        .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_lowercase());
    Ok(state.map(|s| s.starts_with("enable")))
}

// ---- sterownik NVIDIA (profil globalny DRS, publiczne NVAPI)

/// Pozycja sterownika: klucz, pary (ustawienie, wartość zalecana); pierwsza
/// para decyduje o stanie. `0` w wartości FPS = wyliczana z odświeżania.
#[cfg(windows)]
const NV_TWEAKS: &[(&str, &[(u32, u32)])] = &[
    // V-Sync: włączona (VSYNCMODE = FORCEON, bez adaptacyjnej - TEAR_DISABLE).
    (
        "nv_vsync",
        &[(0x00A8_79CF, 0x4781_4940), (0x005A_375C, 0x9686_1077)],
    ),
    // Limit klatek (FRL_FPS) poniżej odświeżania monitora.
    ("nv_fps_cap", &[(0x1083_5002, 0)]),
    // Low Latency Mode "On" = maks. 1 klatka przygotowana (PRERENDERLIMIT).
    ("nv_low_latency", &[(0x007B_A09E, 1)]),
    // Tryb zarządzania energią: maksymalna wydajność (PREFERRED_PSTATE).
    ("nv_power", &[(0x1057_EB71, 1)]),
];

/// Odświeżanie głównego monitora (Hz).
#[cfg(windows)]
fn refresh_hz() -> Option<u32> {
    #[link(name = "user32")]
    extern "system" {
        fn EnumDisplaySettingsW(device: *const u16, mode: u32, devmode: *mut u8) -> i32;
    }
    // DEVMODEW: 220 B, dmSize na 68, dmDisplayFrequency na 184.
    let mut dm = [0u8; 220];
    dm[68..70].copy_from_slice(&220u16.to_le_bytes());
    let ok = unsafe { EnumDisplaySettingsW(std::ptr::null(), 0xFFFF_FFFF, dm.as_mut_ptr()) };
    let hz = u32::from_le_bytes(dm[184..188].try_into().ok()?);
    (ok != 0 && hz > 1).then_some(hz)
}

/// Limit FPS dla G-SYNC: ok. 3,5% poniżej odświeżania (co najmniej 3 FPS),
/// żeby liczba klatek nie dobijała do odświeżania (wtedy G-SYNC przestaje działać).
pub fn fps_cap(hz: u32) -> u32 {
    (hz * 965 / 1000).min(hz.saturating_sub(3))
}

#[cfg(windows)]
fn nv_pairs(key: &str) -> Option<Vec<(u32, u32)>> {
    let (_, pairs) = NV_TWEAKS.iter().find(|(k, _)| *k == key)?;
    Some(
        pairs
            .iter()
            .map(|&(id, v)| {
                if key == "nv_fps_cap" {
                    (id, refresh_hz().map(fps_cap).unwrap_or(0))
                } else {
                    (id, v)
                }
            })
            .collect(),
    )
}

#[cfg(windows)]
fn nv_drs() -> Result<crate::nvcp::drs::Drs, String> {
    let api = crate::nvcp::api::Api::load()?;
    crate::nvcp::drs::Drs::open(Arc::new(api))
}

/// Opis wartości ustawienia sterownika.
fn nv_text(key: &str, v: Option<u32>) -> String {
    let Some(v) = v else {
        return text("domyślne sterownika", "driver default");
    };
    match (key, v) {
        ("nv_vsync", 0x4781_4940) => text("włączona", "on"),
        ("nv_vsync", 0x0841_6747) => text("wyłączona", "off"),
        ("nv_vsync", 0x6092_5292) => text("ustawienie aplikacji", "application setting"),
        ("nv_vsync", _) => text("adaptacyjna / szybka", "adaptive / fast"),
        ("nv_fps_cap", 0) => text("wyłączony", "off"),
        ("nv_fps_cap", v) => format!("{v} FPS"),
        ("nv_low_latency", 0) => text("ustawienie aplikacji (Off)", "application setting (Off)"),
        ("nv_low_latency", 1) => text("On - 1 klatka", "On - 1 frame"),
        ("nv_low_latency", v) => crate::trf!("{v} klatki", "{v} frames"),
        ("nv_power", 0) => text("normalny (adaptacyjny)", "normal (adaptive)"),
        ("nv_power", 1) => text("maksymalna wydajność", "prefer maximum performance"),
        ("nv_power", 3) => text("stała wydajność", "prefer consistent performance"),
        ("nv_power", 5) => text("optymalne zużycie energii", "optimal power"),
        (_, v) => format!("0x{v:X}"),
    }
}

#[cfg(windows)]
fn nvidia_items() -> Vec<Item> {
    let drs = match nv_drs() {
        Ok(d) => d,
        Err(_) => return Vec::new(), // bez karty / sterownika NVIDIA pozycji nie pokazujemy
    };
    let hz = refresh_hz();
    let mut out = Vec::new();
    for (key, _) in NV_TWEAKS {
        let pairs = nv_pairs(key).unwrap_or_default();
        let Some(&(id, want)) = pairs.first() else {
            continue;
        };
        let current = drs
            .read_any(&crate::nvcp::drs::Target::Global, id)
            .ok()
            .flatten()
            .and_then(|r| r.value)
            // Brak wpisu w profilu globalnym = wartość domyślna sterownika.
            .or_else(|| drs.defaults(id).map(|(d, _)| d));
        let shown = nv_text(key, current);
        let state = if *key == "nv_fps_cap" && want == 0 {
            State::Unsupported(text(
                "nie udało się odczytać odświeżania monitora",
                "could not read the monitor refresh rate",
            ))
        } else if current == Some(want) {
            State::Optimized(shown)
        } else {
            State::Default(shown)
        };
        let (name, desc, group) = match *key {
            "nv_vsync" => (
                text("NVIDIA: V-Sync włączona (z G-SYNC)", "NVIDIA: V-Sync on (with G-SYNC)"),
                crate::i18n::tr(
                    "Z G-SYNC / FreeSync: V-Sync w sterowniku włączona, w grze wyłączona - bez rwania obrazu przy skokach FPS (zalecenie Blur Busters / NVIDIA). Profil globalny.",
                    "With G-SYNC / FreeSync: V-Sync on in the driver and off in the game - no tearing on FPS spikes (Blur Busters / NVIDIA recommendation). Global profile.",
                ),
                Group::Proven,
            ),
            "nv_fps_cap" => (
                crate::trf!(
                    "NVIDIA: limit klatek {} FPS (odświeżanie {} Hz)",
                    "NVIDIA: frame cap {} FPS ({} Hz refresh)",
                    want,
                    hz.unwrap_or(0)
                ),
                crate::i18n::tr(
                    "Limit nieco poniżej odświeżania trzyma FPS w zakresie G-SYNC - bez kolejki klatek V-Sync i jej opóźnienia. Gry z Reflex ustawiają podobny limit same.",
                    "A cap slightly below the refresh rate keeps FPS inside the G-SYNC range - no V-Sync frame queue and its latency. Games with Reflex set a similar cap themselves.",
                ),
                Group::Proven,
            ),
            "nv_low_latency" => (
                text("NVIDIA: Low Latency Mode On", "NVIDIA: Low Latency Mode On"),
                crate::i18n::tr(
                    "Procesor przygotowuje najwyżej 1 klatkę naprzód - mniejsze opóźnienie wejścia w grach bez Reflex (w grach z Reflex włącz Reflex).",
                    "The CPU prepares at most 1 frame ahead - lower input latency in games without Reflex (in games with Reflex, turn Reflex on).",
                ),
                Group::Proven,
            ),
            _ => (
                text("NVIDIA: tryb energii - maksymalna wydajność", "NVIDIA: power mode - prefer maximum performance"),
                crate::i18n::tr(
                    "Karta trzyma wysokie zegary - bez krótkich spadków przy zmianie stanu P. Kosztem jest wyższy pobór na pulpicie (profil globalny).",
                    "The card keeps high clocks - no short dips on P-state changes. The cost is higher power draw on the desktop (global profile).",
                ),
                Group::Test,
            ),
        };
        out.push(Item {
            key: (*key).into(),
            name,
            desc,
            group,
            restart: false,
            state,
            warning: (*key == "nv_power").then(|| {
                text(
                    "Wyższy pobór mocy karty w spoczynku.",
                    "Higher idle power draw of the card.",
                )
            }),
        });
    }
    out
}

#[cfg(windows)]
fn nv_set(key: &str, on: bool) -> Result<(), String> {
    use crate::nvcp::drs::Target;
    let pairs = nv_pairs(key).ok_or("?")?;
    let mut drs = nv_drs()?;
    if on {
        if pairs.iter().any(|&(_, v)| v == 0) && key == "nv_fps_cap" {
            return Err(text(
                "nie udało się odczytać odświeżania monitora",
                "could not read the monitor refresh rate",
            ));
        }
        let current: Vec<(u32, Option<u32>)> = pairs
            .iter()
            .map(|&(id, _)| {
                (
                    id,
                    drs.read_any(&Target::Global, id)
                        .ok()
                        .flatten()
                        .and_then(|r| r.value),
                )
            })
            .collect();
        remember(key, serde_json::to_value(&current).unwrap_or_default());
        // set_many zapisuje kopię bazy DRS przed pierwszą zmianą i sprawdza odczytem.
        drs.set_many(&Target::Global, &pairs)
    } else {
        let orig: Vec<(u32, Option<u32>)> = original(key)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_else(|| pairs.iter().map(|&(id, _)| (id, None)).collect());
        for (id, value) in orig {
            match value {
                Some(v) => drs.set_many(&Target::Global, &[(id, v)])?,
                None => drs.restore(&Target::Global, &[id])?,
            }
        }
        forget(key);
        Ok(())
    }
}

// ---- plan zasilania

#[cfg(windows)]
const POWER_TWEAKS: &[(&str, &str, &str, u32, u32)] = &[
    // (klucz, podgrupa, ustawienie, wartość zalecana, domyślna Windows gdy brak kopii)
    (
        "usb_suspend",
        "2a737441-1930-4402-8d77-b2bebba308a3",
        "48e6b7a6-50f5-4782-a5d4-53bb8f07e226",
        0,
        1,
    ),
    (
        "pcie_aspm",
        "501a4d13-42af-4429-9fd1-a8139f2c5c96",
        "ee12f906-d277-404b-b6da-e5fa1a576df5",
        0,
        1,
    ),
    // Parkowanie rdzeni: minimalny % rdzeni bez parkowania (100 = bez parkowania).
    // CPMINCORES - rdzenie wydajne (P), CPMINCORES1 - drugi typ (rdzenie E).
    (
        "cpu_parking_p",
        "54533251-82be-4824-96c1-47b60b740d00",
        "0cc5b647-c1df-4637-891a-dec35c318583",
        100,
        10,
    ),
    (
        "cpu_parking_e",
        "54533251-82be-4824-96c1-47b60b740d00",
        "0cc5b647-c1df-4637-891a-dec35c318584",
        100,
        0,
    ),
];

/// Plan "Ultimate Performance" (ukryty wzorzec Windows).
#[cfg(windows)]
const ULTIMATE: &str = "e9a42b02-d5df-448d-aa00-03f14749eb61";

#[cfg(windows)]
fn is_ultimate(g: &power::Guid) -> bool {
    if g.text() == ULTIMATE {
        return true;
    }
    if let Some(v) = original("ultimate_created") {
        if v.as_str() == Some(g.text().as_str()) {
            return true;
        }
    }
    let n = power::name(g).to_lowercase();
    n.contains("ultimate")
        || n.contains("najwyższa wydajność")
        || n.contains("maksymalna wydajność")
}

// ------------------------------------------------------- odczyt wszystkiego

fn text(pl: &'static str, en: &'static str) -> String {
    crate::i18n::tr(pl, en).to_owned()
}

#[cfg(windows)]
pub(crate) fn read_all(cpu_x3d: bool) -> Vec<Item> {
    let mut items = Vec::new();
    let reg_item = |key: &str| REG_TWEAKS.iter().find(|t| t.key == key).unwrap();

    // --- grupa A
    let t = reg_item("hvci");
    items.push(Item {
        key: t.key.into(),
        name: text("Memory Integrity (integralność pamięci) wyłączona", "Memory Integrity turned off"),
        desc: crate::i18n::tr(
            "Microsoft: wyłączenie podnosi wydajność w grach o kilka % kosztem ochrony przed wstrzykiwaniem kodu do jądra. Zalecane wyłączać na czas grania.",
            "Microsoft: turning it off gains a few % in games at the cost of protection against code injection into the kernel. Recommended to turn off while gaming.",
        ),
        group: Group::Proven,
        restart: true,
        state: reg_state(t, t.path),
        warning: Some(text("Obniża bezpieczeństwo systemu.", "Lowers system security.")),
    });
    items.push(Item {
        key: "vmp".into(),
        name: text("Virtual Machine Platform wyłączona", "Virtual Machine Platform turned off"),
        desc: crate::i18n::tr(
            "Microsoft: wyłączenie zwalnia zasoby hiperwizora. Przestają działać WSL, maszyny wirtualne i aplikacje Android.",
            "Microsoft: turning it off frees hypervisor resources. WSL, virtual machines and Android apps stop working.",
        ),
        group: Group::Proven,
        restart: true,
        state: match vmp_enabled() {
            Ok(Some(true)) => State::Default(text("włączona", "enabled")),
            Ok(Some(false)) => State::Optimized(text("wyłączona", "disabled")),
            Ok(None) => State::Unsupported(text("funkcja niedostępna w tym systemie", "feature not available on this system")),
            Err(e) => State::Unknown(e),
        },
        warning: None,
    });
    for (key, keyword, optimized) in NIC_TWEAKS {
        let (name, desc) = if *key == "nic_moderation" {
            (
                text("Karta sieciowa: Interrupt Moderation wyłączone", "Network adapter: Interrupt Moderation off"),
                crate::i18n::tr(
                    "Intel / Microsoft: pakiety obsługiwane od razu, bez grupowania przerwań - mniejsze opóźnienie kosztem większego obciążenia CPU. Karta restartuje się na 2-3 s.",
                    "Intel / Microsoft: packets are handled immediately without interrupt batching - lower latency at the cost of more CPU load. The adapter restarts for 2-3 s.",
                ),
            )
        } else {
            (
                text("Karta sieciowa: Energy Efficient Ethernet wyłączone", "Network adapter: Energy Efficient Ethernet off"),
                crate::i18n::tr(
                    "Karta nie przechodzi w stan oszczędzania energii przy braku ruchu (wybudzanie dodaje opóźnienie). Karta restartuje się na 2-3 s.",
                    "The adapter does not enter power saving when idle (waking up adds latency). The adapter restarts for 2-3 s.",
                ),
            )
        };
        let state = match nic_values(keyword) {
            Ok(v) if v.is_empty() => State::Unsupported(text(
                "karta sieciowa nie ma tego ustawienia",
                "the network adapter has no such setting",
            )),
            Ok(v) if v.iter().all(|n| n.value == *optimized) => State::Optimized(
                v.iter()
                    .map(|n| format!("{}: {}", n.name, on_off(&n.value)))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Ok(v) => State::Default(
                v.iter()
                    .map(|n| format!("{}: {}", n.name, on_off(&n.value)))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Err(e) => State::Unknown(e),
        };
        items.push(Item {
            key: (*key).into(),
            name,
            desc,
            group: Group::Proven,
            restart: false,
            state,
            warning: None,
        });
    }
    items.push(Item {
        key: "nic_power".into(),
        name: text(
            "Karta sieciowa: oszczędzanie energii wyłączone",
            "Network adapter: power saving off",
        ),
        desc: crate::i18n::tr(
            "Wyłącza oszczędzanie energii, które ma karta: Green Ethernet, Gigabit Lite, Advanced EEE, Power Saving Mode, Auto Disable Gigabit (Realtek), Ultra Low Power Mode (Intel), Selective Suspend oraz wyłączanie karty przez Windows. Wybudzanie karty dodaje opóźnienie do pakietów. Karta restartuje się na 2-3 s.",
            "Turns off the power saving the adapter has: Green Ethernet, Gigabit Lite, Advanced EEE, Power Saving Mode, Auto Disable Gigabit (Realtek), Ultra Low Power Mode (Intel), Selective Suspend and turning the adapter off by Windows. Waking the adapter adds latency to packets. The adapter restarts for 2-3 s.",
        ),
        group: Group::Proven,
        restart: false,
        state: match nic_power() {
            Ok(p) if p.empty() => State::Unsupported(text(
                "karta sieciowa nie ma ustawień oszczędzania energii",
                "the network adapter has no power saving settings",
            )),
            Ok(p) if p.off() => State::Optimized(p.text()),
            Ok(p) => State::Default(p.text()),
            Err(e) => State::Unknown(e),
        },
        warning: None,
    });
    for (key, sub, setting, optimized, _) in POWER_TWEAKS {
        let (name, desc) = match *key {
            "usb_suspend" => (
                text("Zasilanie: USB Selective Suspend wyłączone", "Power: USB Selective Suspend off"),
                crate::i18n::tr(
                    "Porty USB nie usypiają urządzeń (mysz, klawiatura, interfejs audio) - bez opóźnień przy wybudzaniu. Dotyczy aktywnego planu zasilania (zasilanie sieciowe).",
                    "USB ports do not suspend devices (mouse, keyboard, audio interface) - no wake-up delays. Applies to the active power plan (AC power).",
                ),
            ),
            "pcie_aspm" => (
                text("Zasilanie: PCI Express Link State Power Management wyłączone", "Power: PCI Express Link State Power Management off"),
                crate::i18n::tr(
                    "Łącza PCIe (karta graficzna, NVMe, sieć) nie przechodzą w stany oszczędzania - bez opóźnień przy powrocie do pracy. Dotyczy aktywnego planu zasilania.",
                    "PCIe links (graphics card, NVMe, network) do not enter power saving states - no delays when resuming. Applies to the active power plan.",
                ),
            ),
            "cpu_parking_p" => (
                text("Procesor: parkowanie rdzeni P wyłączone", "CPU: P-core parking off"),
                crate::i18n::tr(
                    "Windows nie usypia (nie parkuje) rdzeni wydajnych - przy nagłym obciążeniu nie trzeba ich wybudzać, co wygładza czasy klatek. Kilka W więcej w spoczynku. Dotyczy aktywnego planu zasilania.",
                    "Windows does not park the performance cores - they do not have to wake up on a sudden load, which smooths frame times. A few W more at idle. Applies to the active power plan.",
                ),
            ),
            _ => (
                text("Procesor: parkowanie rdzeni E wyłączone", "CPU: E-core parking off"),
                crate::i18n::tr(
                    "To samo dla rdzeni energooszczędnych (procesory hybrydowe Intel 12.-14. gen.). Na procesorach bez rdzeni E ustawienie nic nie zmienia.",
                    "The same for the efficient cores (Intel 12th-14th gen hybrid CPUs). On CPUs without E-cores the setting changes nothing.",
                ),
            ),
        };
        let state = match power::read_ac(&power::Guid::parse(sub), &power::Guid::parse(setting)) {
            Some(v) if v == *optimized => State::Optimized(power_text(key, v)),
            Some(v) => State::Default(power_text(key, v)),
            // Część planów (np. po zmianach producenta płyty) nie ma tego ustawienia.
            None => State::Unsupported(text(
                "aktywny plan zasilania nie ma tego ustawienia",
                "the active power plan has no such setting",
            )),
        };
        items.push(Item {
            key: (*key).into(),
            name,
            desc,
            group: Group::Proven,
            restart: false,
            state,
            warning: None,
        });
    }
    items.push(Item {
        key: "ultimate".into(),
        name: text("Plan zasilania Ultimate Performance", "Ultimate Performance power plan"),
        desc: crate::i18n::tr(
            "Ukryty plan Windows bez oszczędzania energii (procesor bez głębokich stanów uśpienia). Program tworzy go i aktywuje; przywrócenie wraca do poprzedniego planu.",
            "Hidden Windows plan without power saving (no deep CPU sleep states). The program creates and activates it; restoring returns to the previous plan.",
        ),
        group: Group::Proven,
        restart: false,
        state: match power::active() {
            Some(g) if is_ultimate(&g) => State::Optimized(power::name(&g)),
            Some(g) => State::Default(power::name(&g)),
            None => State::Unknown(text("nie udało się odczytać planu zasilania", "could not read the power plan")),
        },
        warning: cpu_x3d.then(|| {
            text(
                "Ryzen X3D: Ultimate Performance może przeszkadzać w przydzielaniu gier do rdzeni z 3D V-Cache - zostań przy planie Zrównoważony.",
                "Ryzen X3D: Ultimate Performance can interfere with scheduling games on the 3D V-Cache cores - stay on Balanced.",
            )
        }),
    });
    let t = reg_item("game_mode");
    items.push(Item {
        key: t.key.into(),
        name: text("Tryb gry Windows włączony", "Windows Game Mode on"),
        desc: crate::i18n::tr(
            "Windows nadaje grze priorytet i wstrzymuje część zadań w tle (aktualizacje, powiadomienia).",
            "Windows prioritizes the game and pauses some background tasks (updates, notifications).",
        ),
        group: Group::Proven,
        restart: false,
        state: reg_state(t, t.path),
        warning: None,
    });

    let build = windows_build();
    items.push(Item {
        key: "windowed_games".into(),
        name: text(
            "Optymalizacje dla gier w oknie (Windows 11)",
            "Optimizations for windowed games (Windows 11)",
        ),
        desc: crate::i18n::tr(
            "Gry DirectX 10 i 11 w oknie i w oknie bez ramki dostają nowoczesny model wyświetlania: mniejsze opóźnienie klatek, działające VRR (G-SYNC / FreeSync) i Auto HDR. To samo co przełącznik w Ustawieniach > System > Ekran > Grafika. Działa po ponownym uruchomieniu gry.",
            "DirectX 10 and 11 games in windowed and borderless mode get the modern presentation model: lower frame latency, working VRR (G-SYNC / FreeSync) and Auto HDR. Same as the switch in Settings > System > Display > Graphics. Takes effect after restarting the game.",
        ),
        group: Group::Proven,
        restart: false,
        state: if build >= 22000 {
            windowed_state()
        } else {
            State::Unsupported(text("wymaga Windows 11", "requires Windows 11"))
        },
        warning: None,
    });

    let (dvr, cap) = (reg_item("game_dvr"), reg_item("game_dvr_capture"));
    let dvr_raw = reg::get_dword(dvr.root, dvr.path, dvr.value).or(dvr.absent);
    let cap_raw = reg::get_dword(cap.root, cap.path, cap.value).or(cap.absent);
    let dvr_text = format!(
        "{}, {}",
        reg_value_text(dvr, reg::get_dword(dvr.root, dvr.path, dvr.value)),
        reg_value_text(cap, reg::get_dword(cap.root, cap.path, cap.value))
    );
    items.push(Item {
        key: "game_dvr".into(),
        name: text(
            "Nagrywanie gier (Game Bar / Game DVR) wyłączone",
            "Game recording (Game Bar / Game DVR) off",
        ),
        desc: crate::i18n::tr(
            "Windows nie nagrywa ani nie buforuje obrazu gier w tle, co zdejmuje obciążenie z karty i procesora. Skrót Win+G nadal otwiera Game Bar. Działa po ponownym uruchomieniu gry.",
            "Windows does not record or buffer game video in the background, which takes load off the card and CPU. Win+G still opens Game Bar. Takes effect after restarting the game.",
        ),
        group: Group::Proven,
        restart: false,
        state: if dvr_raw == Some(0) && cap_raw == Some(0) {
            State::Optimized(dvr_text)
        } else {
            State::Default(dvr_text)
        },
        warning: None,
    });

    items.push(Item {
        key: "mouse_accel".into(),
        name: text(
            "Przyspieszanie wskaźnika myszy wyłączone",
            "Mouse pointer acceleration off",
        ),
        desc: crate::i18n::tr(
            "Wyłącza „Popraw precyzję wskaźnika”: kursor przesuwa się zawsze o tyle samo przy tym samym ruchu myszy, niezależnie od szybkości ruchu. Gry z surowym wejściem (raw input) tego nie używają, ale menu, gry bez raw input i pulpit tak. Działa od razu.",
            "Turns off \"Enhance pointer precision\": the cursor always moves the same distance for the same mouse movement, regardless of speed. Games with raw input ignore it, but menus, games without raw input and the desktop use it. Takes effect immediately.",
        ),
        group: Group::Proven,
        restart: false,
        state: mouse_state(),
        warning: None,
    });

    // Kompresja pamięci i dyski HDD (jedno wywołanie PowerShell, potrzebne niżej).
    let mem_disks = memory_and_disks();
    items.push(Item {
        key: "sysmain".into(),
        name: text(
            "SysMain (Superfetch / Prefetch) wyłączony",
            "SysMain (Superfetch / Prefetch) off",
        ),
        desc: crate::i18n::tr(
            "Usługa wstępnie wczytuje często używane programy do pamięci. Na dyskach SSD nie przyspiesza uruchamiania, a w tle czyta dysk i zajmuje pamięć. Microsoft zaleca jej wyłączenie w systemach czasu rzeczywistego. Tylko dla komputerów bez dysków HDD. Usługa jest zatrzymywana od razu.",
            "The service preloads frequently used programs into memory. On SSDs it does not speed up launching, while it reads the disk and uses memory in the background. Microsoft recommends turning it off for real-time systems. Only for PCs without HDDs. The service is stopped right away.",
        ),
        group: Group::Proven,
        restart: false,
        state: match &mem_disks {
            Ok((_, hdd)) if *hdd > 0 => State::Unsupported(crate::trf!(
                "w komputerze jest dysk HDD ({hdd}) - SysMain przyspiesza go",
                "the PC has a hard disk ({hdd}) - SysMain speeds it up"
            )),
            _ => match service::state("SysMain") {
                Ok((start, running)) if start == service::DISABLED => {
                    State::Optimized(service_text(start, running))
                }
                Ok((start, running)) => State::Default(service_text(start, running)),
                Err(e) => State::Unknown(e),
            },
        },
        warning: None,
    });
    items.extend(nvidia_items());

    // --- grupa B
    for d in devices::list() {
        let path = devices::msi_path(&d.id);
        let state = match reg::get_dword(reg::HKLM, &path, "MSISupported") {
            Some(1) => State::Optimized(text(
                "MSISupported = 1 - MSI włączone",
                "MSISupported = 1 - MSI on",
            )),
            Some(v) => State::Default(format!("MSISupported = {v}")),
            None => State::Default(text(
                "tryb przerwań wg sterownika",
                "interrupt mode chosen by the driver",
            )),
        };
        items.push(Item {
            key: format!("msi:{}", d.id),
            name: crate::trf!("Tryb MSI przerwań: {}", "MSI interrupt mode: {}", d.name),
            desc: crate::i18n::tr(
                "Message Signaled Interrupts zamiast współdzielonych linii IRQ - mniej opóźnień DPC, gdy urządzenia dzielą przerwanie. Sterownik Nvidii potrafi to cofnąć przy instalacji; zweryfikuj LatencyMon.",
                "Message Signaled Interrupts instead of shared IRQ lines - less DPC latency when devices share an interrupt. The Nvidia driver may undo it on install; verify with LatencyMon.",
            ),
            group: Group::Test,
            restart: true,
            state,
            warning: None,
        });
    }
    items.push(Item {
        key: "memory_compression".into(),
        name: text("Kompresja pamięci wyłączona", "Memory compression off"),
        desc: crate::i18n::tr(
            "Windows kompresuje rzadko używane strony pamięci zamiast zapisywać je na dysk; kosztuje to czas procesora przy dostępie do nich. Przy dużej ilości RAM (16 GB i więcej) kompresja rzadko jest potrzebna. Działa po restarcie.",
            "Windows compresses rarely used memory pages instead of writing them to disk; accessing them costs CPU time. With plenty of RAM (16 GB or more) compression is rarely needed. Takes effect after a restart.",
        ),
        group: Group::Test,
        restart: true,
        state: match &mem_disks {
            Ok((true, _)) => State::Default(text("włączona", "on")),
            Ok((false, _)) => State::Optimized(text("wyłączona", "off")),
            Err(e) => State::Unknown(e.clone()),
        },
        warning: Some(text(
            "Przy małej ilości RAM może zwiększyć użycie pliku stronicowania.",
            "With little RAM it may increase paging file use.",
        )),
    });
    let t = reg_item("mouse_background");
    items.push(Item {
        key: t.key.into(),
        name: text(
            "Dane myszy dla okien w tle co 3 ms (Windows 11)",
            "Mouse data for background windows every 3 ms (Windows 11)",
        ),
        desc: crate::i18n::tr(
            "Windows 11 ogranicza dane z myszy dla programów w tle do jednej paczki co 8 ms (125 Hz). 3 ms to najmniejsza wartość, jaką system przyjmuje - pomaga nakładkom, programom do nagrywania i drugiemu oknu gry. Działa od razu dla nowych programów.",
            "Windows 11 limits mouse data for background programs to one batch every 8 ms (125 Hz). 3 ms is the lowest value the system accepts - helps overlays, recording software and a second game window. Takes effect for newly started programs.",
        ),
        group: Group::Test,
        restart: false,
        state: if build >= 22621 {
            reg_state(t, t.path)
        } else {
            State::Unsupported(text("wymaga Windows 11 22H2", "requires Windows 11 22H2"))
        },
        warning: None,
    });
    let t = reg_item("fth");
    items.push(Item {
        key: t.key.into(),
        name: text("Fault Tolerant Heap wyłączony", "Fault Tolerant Heap off"),
        desc: crate::i18n::tr(
            "Po kilku awariach programu Windows na stałe włącza dla niego wolniejszy, „bezpieczniejszy” tryb przydzielania pamięci. Wyłączenie zapobiega spowolnieniu gier, które kiedyś się zawiesiły. Działa po restarcie.",
            "After a program crashes a few times, Windows permanently switches it to a slower, \"safer\" memory allocation mode. Turning it off prevents slowing down games that once crashed. Takes effect after a restart.",
        ),
        group: Group::Test,
        restart: true,
        state: reg_state(t, t.path),
        warning: None,
    });
    let t = reg_item("fast_startup");
    items.push(Item {
        key: t.key.into(),
        name: text("Szybkie uruchamianie wyłączone", "Fast startup off"),
        desc: crate::i18n::tr(
            "Zamknięcie systemu jest pełne zamiast częściowej hibernacji jądra: sterowniki i urządzenia uruchamiają się od nowa przy każdym starcie (mniej problemów po aktualizacjach sterowników, zmianach BIOS-u i podkręcaniu). Start trwa kilka sekund dłużej.",
            "Shutdown is complete instead of a partial kernel hibernation: drivers and devices start fresh at every boot (fewer problems after driver updates, BIOS changes and overclocking). Booting takes a few seconds longer.",
        ),
        group: Group::Test,
        restart: false,
        state: reg_state(t, t.path),
        warning: None,
    });
    let t = reg_item("dynamic_lighting");
    items.push(Item {
        key: t.key.into(),
        name: text(
            "Dynamic Lighting Windows wyłączone",
            "Windows Dynamic Lighting off",
        ),
        desc: crate::i18n::tr(
            "Windows 11 sam steruje podświetleniem zgodnych urządzeń (klawiatury, myszy). Wyłączenie zostawia podświetlenie zakładce RGB i aplikacjom producentów - bez nadpisywania kolorów przez system.",
            "Windows 11 controls the lighting of compatible devices (keyboards, mice) itself. Turning it off leaves lighting to the RGB tab and vendor apps - the system no longer overrides colors.",
        ),
        group: Group::Test,
        restart: false,
        state: if build >= 22621 {
            reg_state(t, t.path)
        } else {
            State::Unsupported(text("wymaga Windows 11", "requires Windows 11"))
        },
        warning: None,
    });
    let t = reg_item("priority");
    items.push(Item {
        key: t.key.into(),
        name: text("Priorytet okna na pierwszym planie (Win32PrioritySeparation = 0x26)", "Foreground priority (Win32PrioritySeparation = 0x26)"),
        desc: crate::i18n::tr(
            "Krótkie, zmienne kwanty czasu i 3x dłuższy kwant dla programu na pierwszym planie (gry). Domyślnie 2. Działa od razu.",
            "Short, variable time slices and a 3x longer slice for the foreground program (games). Default 2. Takes effect immediately.",
        ),
        group: Group::Test,
        restart: false,
        state: reg_state(t, t.path),
        warning: None,
    });
    let t = reg_item("responsiveness");
    items.push(Item {
        key: t.key.into(),
        name: text("SystemResponsiveness = 10", "SystemResponsiveness = 10"),
        desc: crate::i18n::tr(
            "Harmonogram multimediów (MMCSS) rezerwuje dla zadań w tle 10% CPU zamiast 20%. Mniejszych wartości Windows i tak nie przyjmuje.",
            "The multimedia scheduler (MMCSS) reserves 10% of CPU for background tasks instead of 20%. Windows does not accept lower values anyway.",
        ),
        group: Group::Test,
        restart: true,
        state: reg_state(t, t.path),
        warning: None,
    });
    let t = reg_item("mpo");
    items.push(Item {
        key: t.key.into(),
        name: text(
            "Multi-Plane Overlay (MPO) wyłączone",
            "Multi-Plane Overlay (MPO) off",
        ),
        desc: crate::i18n::tr(
            "Pulpit (DWM) składa obraz bez nakładek sprzętowych. NVIDIA zaleca to przy migotaniu, czarnych mignięciach i przycięciach w grach w oknie bez ramki oraz przy kilku monitorach o różnym odświeżaniu. Działa po restarcie.",
            "The desktop compositor (DWM) builds the image without hardware overlays. NVIDIA recommends this for flicker, black flashes and stutter in borderless games and with several monitors at different refresh rates. Takes effect after a restart.",
        ),
        group: Group::Test,
        restart: true,
        state: reg_state(t, t.path),
        warning: Some(text(
            "Nieoficjalne ustawienie Microsoftu - nowsze wersje Windows 11 mogą je ignorować. Bez problemów z obrazem lepiej zostawić MPO włączone (mniejsze obciążenie karty).",
            "Not an official Microsoft setting - newer Windows 11 builds may ignore it. Without display problems it is better to keep MPO on (less GPU load).",
        )),
    });
    items.retain(|i| !user_registry_item(&i.key));
    items
}

/// Pozycje zapisujące ustawienia użytkownika w HKEY_CURRENT_USER: Tryb gry,
/// nagrywanie Game Bar, dane myszy dla okien w tle, Dynamic Lighting
/// i optymalizacje gier w oknie.
///
/// Od 0.32.2 nie ma ich w zakładce TUNE: pakiet do Microsoft Store nie może
/// zapisywać do rejestru użytkownika (zapis jest wirtualizowany w pakiecie),
/// a zakładka ma wyglądać tak samo w obu wersjach.
/// `set` nadal je obsługuje, żeby "Przywróć ustawienia Windows" cofnęło
/// zmiany zrobione wcześniejszą wersją programu.
#[cfg(windows)]
fn user_registry_item(key: &str) -> bool {
    key == "windowed_games"
        || REG_TWEAKS
            .iter()
            .any(|t| t.key == key && t.root == reg::HKCU)
}

/// Zmienia pozycję: `on` = wartość zalecana, `off` = oryginał.
#[cfg(windows)]
fn set(key: &str, on: bool) -> Result<(), String> {
    // Usunięte pozycje można tylko przywrócić do oryginału.
    if on && user_registry_item(key) {
        return Err(text(
            "ta pozycja została usunięta z programu",
            "this item was removed from the program",
        ));
    }
    match key {
        // Dwie wartości: Game Bar (nagrywanie) i przechwytywanie aplikacji.
        "game_dvr" => {
            for k in ["game_dvr", "game_dvr_capture"] {
                let t = REG_TWEAKS.iter().find(|t| t.key == k).unwrap();
                reg_set(t.key, t.root, t.path, t.value, t.optimized, on)?;
            }
            return Ok(());
        }
        "windowed_games" => return windowed_set(on),
        "mouse_accel" => return mouse_set(on),
        "memory_compression" => return memory_compression_set(on),
        "sysmain" => return sysmain_set(on),
        _ => {}
    }
    if let Some(t) = REG_TWEAKS.iter().find(|t| t.key == key) {
        return reg_set(t.key, t.root, t.path, t.value, t.optimized, on);
    }
    if let Some(id) = key.strip_prefix("msi:") {
        return reg_set(
            key,
            reg::HKLM,
            &devices::msi_path(id),
            "MSISupported",
            1,
            on,
        );
    }
    if key == "nic_power" {
        if on {
            let current = nic_power()?;
            remember(key, serde_json::to_value(&current).unwrap_or_default());
            // Zapisujemy tylko to, co jest włączone.
            let target = NicPower {
                adv: current
                    .adv
                    .iter()
                    .filter(|k| k.value != "0")
                    .map(|k| NicKeyword {
                        value: "0".into(),
                        ..k.clone()
                    })
                    .collect(),
                pm: current
                    .pm
                    .iter()
                    .map(|p| NicPm {
                        name: p.name.clone(),
                        allow: if p.allow == "Enabled" {
                            "Disabled".into()
                        } else {
                            String::new()
                        },
                        suspend: if p.suspend == "Enabled" {
                            "Disabled".into()
                        } else {
                            String::new()
                        },
                    })
                    .collect(),
            };
            nic_power_write(&target)?;
            if !nic_power()?.off() {
                return Err(text(
                    "karta nie przyjęła wszystkich ustawień",
                    "the adapter did not accept every setting",
                ));
            }
        } else {
            let orig: NicPower = original(key)
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            nic_power_write(&orig)?;
            forget(key);
        }
        return Ok(());
    }
    if let Some((_, keyword, optimized)) = NIC_TWEAKS.iter().find(|(k, _, _)| *k == key) {
        let current = nic_values(keyword)?;
        if on {
            remember(key, serde_json::to_value(&current).unwrap_or_default());
            for n in &current {
                if n.value != *optimized {
                    nic_set(&n.name, keyword, optimized)?;
                }
            }
            if nic_values(keyword)?.iter().any(|n| n.value != *optimized) {
                return Err(text(
                    "karta nie przyjęła wartości",
                    "the adapter did not accept the value",
                ));
            }
        } else {
            let orig: Vec<NicValue> = original(key)
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            for o in &orig {
                nic_set(&o.name, keyword, &o.value)?;
            }
            forget(key);
        }
        return Ok(());
    }
    if let Some((_, sub, setting, optimized, default)) =
        POWER_TWEAKS.iter().find(|(k, ..)| *k == key)
    {
        let (sub, setting) = (power::Guid::parse(sub), power::Guid::parse(setting));
        if on {
            remember(key, serde_json::json!(power::read_ac(&sub, &setting)));
            if let Err(e) = power::write_ac(&sub, &setting, *optimized) {
                // Błąd 13: definicja ustawienia bez zakresu (ValueMin/ValueMax) - bywa
                // usunięty przez programy do "optymalizacji". Uzupełniamy standardowy
                // zakres 0-100 % i próbujemy ponownie.
                if !(e.ends_with(": 13") && key.starts_with("cpu_parking")) {
                    return Err(e);
                }
                let (_, sub_s, set_s, ..) = POWER_TWEAKS.iter().find(|(k, ..)| *k == key).unwrap();
                let def = format!(
                    r"SYSTEM\CurrentControlSet\Control\Power\PowerSettings\{sub_s}\{set_s}"
                );
                reg::set_dword(reg::HKLM, &def, "ValueMin", Some(0))?;
                reg::set_dword(reg::HKLM, &def, "ValueMax", Some(100))?;
                power::write_ac(&sub, &setting, *optimized)?;
            }
            if power::read_ac(&sub, &setting) != Some(*optimized) {
                return Err(text(
                    "plan zasilania nie przyjął wartości",
                    "the power plan did not accept the value",
                ));
            }
        } else {
            let orig: Option<u32> = original(key)
                .and_then(|v| serde_json::from_value(v).ok())
                .flatten();
            // Bez kopii: wartość domyślna Windows dla tego ustawienia.
            power::write_ac(&sub, &setting, orig.unwrap_or(*default))?;
            forget(key);
        }
        return Ok(());
    }
    if key.starts_with("nv_") {
        return nv_set(key, on);
    }
    match key {
        "vmp" => {
            if on {
                remember(key, serde_json::json!(vmp_enabled()?));
                dism(&[
                    "/Disable-Feature",
                    "/FeatureName:VirtualMachinePlatform",
                    "/NoRestart",
                ])?;
            } else {
                dism(&[
                    "/Enable-Feature",
                    "/FeatureName:VirtualMachinePlatform",
                    "/All",
                    "/NoRestart",
                ])?;
                forget(key);
            }
            Ok(())
        }
        "ultimate" => {
            if on {
                let prev = power::active().ok_or("PowerGetActiveScheme")?;
                remember(key, serde_json::json!(prev.text()));
                let plan = match original("ultimate_created")
                    .and_then(|v| v.as_str().map(power::Guid::parse))
                {
                    Some(g) if power::set_active(&g).is_ok() => g,
                    _ => {
                        let g = power::duplicate(&power::Guid::parse(ULTIMATE))?;
                        let mut b = load_backup();
                        b.values
                            .insert("ultimate_created".into(), serde_json::json!(g.text()));
                        save_backup(&b);
                        g
                    }
                };
                power::set_active(&plan)?;
                if power::active() != Some(plan) {
                    return Err(text(
                        "plan nie został aktywowany",
                        "the plan was not activated",
                    ));
                }
            } else {
                // Poprzedni plan albo "Zrównoważony", gdy kopii brak.
                let prev = original(key)
                    .and_then(|v| v.as_str().map(power::Guid::parse))
                    .unwrap_or_else(|| power::Guid::parse("381b4222-f694-41f0-9685-ff5bb260df2e"));
                power::set_active(&prev)?;
                forget(key);
            }
            Ok(())
        }
        _ => Err(format!("? {key}")),
    }
}

// ---------------------------------------------------------- wątek i stan

/// Stan widoczny dla interfejsu.
#[derive(Default, Clone)]
pub struct Snapshot {
    pub items: Vec<Item>,
    pub loading: bool,
    /// Pozycja w trakcie zmiany.
    pub busy: Option<String>,
    /// Ostatni wynik: (tekst, błąd?).
    pub message: Option<(String, bool)>,
    /// Czy w kopii są wartości do przywrócenia.
    pub has_backup: bool,
}

enum Cmd {
    Refresh,
    Set(String, bool),
    RestoreAll,
}

/// Wątek zakładki: odczyt i zmiany (DISM i PowerShell trwają kilka sekund).
pub struct Tune {
    tx: Sender<Cmd>,
    shared: Arc<Mutex<Snapshot>>,
    started: bool,
    /// Przewinąć listę na górę przy pierwszym wyświetleniu pozycji.
    scroll_top: bool,
}

impl Tune {
    pub fn new(cpu_x3d: bool, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let s = shared.clone();
        crate::spawn("tune", move || worker(rx, s, cpu_x3d, repaint));
        Self {
            tx,
            shared,
            started: false,
            scroll_top: true,
        }
    }

    /// Pierwszy odczyt dopiero przy otwarciu zakładki (PowerShell i DISM).
    pub fn ensure_loaded(&mut self) {
        if !self.started {
            self.started = true;
            let _ = self.tx.send(Cmd::Refresh);
        }
    }

    pub fn refresh(&self) {
        let _ = self.tx.send(Cmd::Refresh);
    }

    pub fn set(&self, key: &str, on: bool) {
        let _ = self.tx.send(Cmd::Set(key.to_owned(), on));
    }

    pub fn restore_all(&self) {
        let _ = self.tx.send(Cmd::RestoreAll);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

fn worker(rx: Receiver<Cmd>, shared: Arc<Mutex<Snapshot>>, cpu_x3d: bool, repaint: impl Fn()) {
    let update = |f: &dyn Fn(&mut Snapshot)| {
        if let Ok(mut s) = shared.lock() {
            f(&mut s);
        }
        repaint();
    };
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Refresh => {}
            Cmd::Set(key, on) => {
                update(&|s| s.busy = Some(key.clone()));
                #[cfg(windows)]
                let result = set(&key, on);
                #[cfg(not(windows))]
                let result: Result<(), String> = Err("Windows".into());
                let msg = match result {
                    Ok(()) => (
                        text(
                            "Zapisano i sprawdzono odczytem.",
                            "Saved and verified by reading back.",
                        ),
                        false,
                    ),
                    Err(e) => (e, true),
                };
                update(&|s| s.message = Some(msg.clone()));
            }
            Cmd::RestoreAll => {
                let keys: Vec<String> = load_backup()
                    .values
                    .keys()
                    .filter(|k| *k != "ultimate_created")
                    .cloned()
                    .collect();
                let mut errors = Vec::new();
                for key in keys {
                    update(&|s| s.busy = Some(key.clone()));
                    #[cfg(windows)]
                    if let Err(e) = set(&key, false) {
                        errors.push(format!("{key}: {e}"));
                    }
                }
                let msg = if errors.is_empty() {
                    (
                        text(
                            "Przywrócono ustawienia Windows sprzed zmian.",
                            "Windows settings restored to their state before the changes.",
                        ),
                        false,
                    )
                } else {
                    (errors.join("; "), true)
                };
                update(&|s| s.message = Some(msg.clone()));
            }
        }
        update(&|s| {
            s.busy = None;
            s.loading = true;
        });
        #[cfg(windows)]
        let items = read_all(cpu_x3d);
        #[cfg(not(windows))]
        let items = Vec::new();
        let has_backup = load_backup().values.keys().any(|k| k != "ultimate_created");
        update(&|s| {
            s.items = items.clone();
            s.loading = false;
            s.has_backup = has_backup;
        });
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::power::Guid;

    #[test]
    fn guid_roundtrip() {
        let s = "e9a42b02-d5df-448d-aa00-03f14749eb61";
        assert_eq!(Guid::parse(s).text(), s);
    }

    /// Odczyt wszystkich pozycji bez zmian: `cargo test tune_read -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn tune_read() {
        for i in super::read_all(false) {
            println!("{:?} {:<70} {:?}", i.group, i.name, i.state);
        }
    }
}

// ------------------------------------------------------------ interfejs

impl Tune {
    pub fn ui(&mut self, ui: &mut eframe::egui::Ui, elevated: bool) {
        use crate::icons::Icon;
        use crate::theme;
        use eframe::egui::{self, RichText};

        self.ensure_loaded();
        let snap = self.snapshot();
        let idle = snap.busy.is_none() && !snap.loading;

        theme::heading(
            ui,
            Icon::Gauge,
            crate::i18n::tr("Opóźnienia i DPC", "Latency and DPC"),
        );
        theme::info(
            ui,
            crate::i18n::tr(
                "Ustawienia Windows wpływające na opóźnienia wejścia i DPC. Przełącznik pokazuje stan odczytany z systemu: ON (zielony) = ustawiona wartość zalecana, OFF (czerwony) = wartość Windows; kliknięcie przełącza. Przed pierwszą zmianą program zapisuje oryginalne wartości (tune-backup.json) i sprawdza każdy zapis odczytem. Zmieniaj po jednej pozycji i porównuj LatencyMon przed i po.",
                "Windows settings that affect input latency and DPC. The switch shows the state read from the system: ON (green) = recommended value set, OFF (red) = Windows value; click to toggle. Before the first change the program saves the original values (tune-backup.json) and verifies every write by reading back. Change one item at a time and compare LatencyMon before and after.",
            ),
        );
        if !elevated {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "Większość pozycji wymaga uprawnień administratora - użyj przycisku \"Uruchom jako administrator\" na dole okna.",
                    "Most items require administrator rights - use the \"Run as administrator\" button at the bottom of the window.",
                ))
                .color(theme::warn()),
            );
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, Some(Icon::Refresh), crate::i18n::tr("Odczytaj ponownie", "Read again"), idle).clicked() {
                self.refresh();
            }
            if theme::primary_button(
                ui,
                Some(Icon::Swap),
                crate::i18n::tr("Przywróć ustawienia Windows", "Restore Windows settings"),
                idle && snap.has_backup,
            )
            .on_hover_text(crate::i18n::tr(
                "Cofa wszystkie zmiany zrobione w tej zakładce do wartości sprzed pierwszej zmiany.",
                "Reverts all changes made in this tab to the values before the first change.",
            ))
            .clicked()
            {
                self.restore_all();
            }
            if snap.loading {
                ui.spinner();
                ui.label(RichText::new(crate::i18n::tr("Odczyt...", "Reading...")).color(theme::muted()));
            }
            if let Some((msg, error)) = &snap.message {
                ui.label(RichText::new(msg).size(12.5).color(if *error { theme::warn() } else { theme::ok() }));
            }
        });
        ui.add_space(8.0);

        let width = ui.available_width() - 2.0 * 16.0 - 4.0;
        let mut change: Option<(String, bool)> = None;
        // Po pierwszym odczycie lista zaczyna się od góry.
        let mut scroll = egui::ScrollArea::vertical();
        if self.scroll_top && !snap.items.is_empty() {
            self.scroll_top = false;
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.id_salt("tune_scroll").show(ui, |ui| {
            for (group, title, icon) in [
                (
                    Group::Proven,
                    crate::i18n::tr(
                        "Sprawdzone (Microsoft, producenci sprzętu)",
                        "Proven (Microsoft, hardware vendors)",
                    ),
                    Icon::Check,
                ),
                (
                    Group::Test,
                    crate::i18n::tr(
                        "Do przetestowania na Twoim sprzęcie",
                        "To test on your hardware",
                    ),
                    Icon::Gauge,
                ),
            ] {
                let items: Vec<&Item> = snap.items.iter().filter(|i| i.group == group).collect();
                if items.is_empty() {
                    continue;
                }
                theme::card(12, 16).show(ui, |ui| {
                    ui.set_width(width);
                    theme::subheading(ui, icon, title);
                    let available = items
                        .iter()
                        .filter(|i| matches!(i.state, State::Optimized(_) | State::Default(_)))
                        .count();
                    let on = items
                        .iter()
                        .filter(|i| matches!(i.state, State::Optimized(_)))
                        .count();
                    ui.label(
                        RichText::new(crate::trf!(
                            "Włączone: {on} z {available} dostępnych",
                            "On: {on} of {available} available"
                        ))
                        .size(12.5)
                        .color(theme::muted()),
                    );
                    let mut msi_note = false;
                    for item in items {
                        // Opis trybu MSI raz, nad listą urządzeń.
                        if item.key.starts_with("msi:") && !msi_note {
                            msi_note = true;
                            ui.add_space(6.0);
                            theme::info(ui, item.desc);
                        }
                        ui.add_space(6.0);
                        if let Some(c) = item_ui(
                            ui,
                            item,
                            idle,
                            elevated || item.key == "game_mode",
                            snap.busy.as_deref(),
                        ) {
                            change = Some(c);
                        }
                    }
                });
                ui.add_space(12.0);
            }
        });
        if let Some((key, on)) = change {
            self.set(&key, on);
        }
    }
}

/// Wiersz pozycji; zwraca żądaną zmianę (klucz, włączyć zalecaną wartość?).
fn item_ui(
    ui: &mut eframe::egui::Ui,
    item: &Item,
    idle: bool,
    allowed: bool,
    busy: Option<&str>,
) -> Option<(String, bool)> {
    use crate::theme;
    use eframe::egui::{self, RichText};

    let mut change = None;
    let (switch, detail) = match &item.state {
        State::Optimized(v) => (theme::SwitchState::On, v.clone()),
        State::Default(v) => (theme::SwitchState::Off, v.clone()),
        State::Unsupported(v) => (theme::SwitchState::NotApplicable, v.clone()),
        // Nieznany stan: przełącznik nieaktywny, plakietka z błędem.
        State::Unknown(v) => (theme::SwitchState::NotApplicable, v.clone()),
    };
    let detail = Some(detail);
    let toggleable = matches!(item.state, State::Optimized(_) | State::Default(_));
    ui.horizontal(|ui| {
        let on = matches!(item.state, State::Optimized(_));
        let enabled = idle && allowed && toggleable;
        // Przełącznik pokazuje stan odczytany z Windows; kliknięcie zgłasza
        // zmianę, a gałka przesuwa się dopiero po zapisie i odczycie.
        if theme::switch(ui, &item.key, switch, enabled).clicked() {
            change = Some((item.key.clone(), !on));
        }
        ui.add_space(4.0);
        let name_color = if toggleable {
            theme::text()
        } else {
            theme::muted()
        };
        ui.label(RichText::new(&item.name).size(14.0).color(name_color));
        if busy == Some(item.key.as_str()) {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if matches!(item.state, State::Unknown(_)) {
                crate::history::badge(
                    ui,
                    crate::i18n::tr("BŁĄD ODCZYTU", "READ ERROR"),
                    theme::warn(),
                );
            }
            if item.restart {
                crate::history::badge(ui, crate::i18n::tr("RESTART", "RESTART"), theme::warn())
                    .on_hover_text(crate::i18n::tr(
                        "Działa po ponownym uruchomieniu komputera.",
                        "Takes effect after restarting the computer.",
                    ));
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(78.0);
        ui.vertical(|ui| {
            if !item.key.starts_with("msi:") {
                ui.add(
                    egui::Label::new(RichText::new(item.desc).size(12.0).color(theme::muted()))
                        .wrap(),
                );
            }
            if let Some(d) = detail {
                // Bieżąca wartość wyraźnie, przy każdej pozycji.
                let label = if toggleable {
                    crate::i18n::tr("Obecnie ustawione:", "Currently set:")
                } else {
                    crate::i18n::tr("Stan:", "Status:")
                };
                ui.label(
                    RichText::new(format!("{label} {d}"))
                        .size(12.5)
                        .color(theme::accent()),
                );
            }
            if let Some(w) = &item.warning {
                ui.label(RichText::new(w).size(12.0).color(theme::warn()));
            }
        });
    });
    change
}

#[cfg(all(test, windows))]
mod hardware {
    /// Zapis i przywrócenie nowych pozycji (ZMIENIA SYSTEM na chwilę, kopia w
    /// pliku tymczasowym): `cargo test tune_new_roundtrip -- --ignored --nocapture`.
    /// Pozycje, które są już włączone, najpierw wyłącza i potem przywraca.
    #[test]
    #[ignore]
    fn tune_new_roundtrip() {
        let backup = std::env::temp_dir().join("tc-tune-new-backup.json");
        let _ = std::fs::remove_file(&backup);
        std::env::set_var("TC_TUNE_BACKUP", &backup);
        let list = std::env::var("TC_ROUNDTRIP_KEYS").unwrap_or_else(|_| {
            "windowed_games,game_dvr,mouse_accel,mouse_background,dynamic_lighting".into()
        });
        let keys: Vec<&str> = list.split(',').collect();
        let state = |k: &str| {
            super::read_all(false)
                .into_iter()
                .find(|i| i.key == k)
                .map(|i| i.state)
        };
        for k in keys {
            let before = state(k);
            let was_on = matches!(before, Some(super::State::Optimized(_)));
            // Przełączamy w drugą stronę i z powrotem.
            let a = super::set(k, !was_on);
            let mid = state(k);
            let b = super::set(k, was_on);
            let after = state(k);
            println!(
                "{k}: przed {before:?}\n  zmiana {a:?} -> {mid:?}\n  powrót {b:?} -> {after:?}"
            );
            assert!(a.is_ok() && b.is_ok());
            assert_eq!(
                matches!(mid, Some(super::State::Optimized(_))),
                !was_on,
                "{k}: stan po zmianie"
            );
            assert_eq!(
                matches!(after, Some(super::State::Optimized(_))),
                was_on,
                "{k}: powrót"
            );
        }
        let _ = std::fs::remove_file(backup);
    }
}

#[cfg(all(test, windows))]
mod apply {
    /// Stosuje zalecane wartości wybranych pozycji (ZMIENIA SYSTEM):
    /// `TC_TUNE_KEYS=a,b TC_TUNE_BACKUP=<plik> cargo test tune_apply -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn tune_apply() {
        let keys = std::env::var("TC_TUNE_KEYS").expect("TC_TUNE_KEYS");
        for key in keys.split(',') {
            match super::set(key, true) {
                Ok(()) => println!("OK   {key}"),
                Err(e) => println!("BŁĄD {key}: {e}"),
            }
        }
        for i in super::read_all(false) {
            if keys.split(',').any(|k| k == i.key) {
                println!("{:<45} {:?}", i.key, i.state);
            }
        }
    }
}

#[cfg(test)]
mod mpo_test {
    /// Zapis MPO (OverlayTestMode = 5) i przywrócenie (usunięcie wpisu):
    /// `cargo test tune_mpo_roundtrip --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn tune_mpo_roundtrip() {
        let t = super::REG_TWEAKS.iter().find(|t| t.key == "mpo").unwrap();
        let before = super::reg::get_dword(t.root, t.path, t.value);
        let backup = std::env::temp_dir().join("tc-mpo-test-backup.json");
        std::env::set_var("TC_TUNE_BACKUP", &backup);
        super::set("mpo", true).unwrap();
        let on = super::reg::get_dword(t.root, t.path, t.value);
        super::set("mpo", false).unwrap();
        let after = super::reg::get_dword(t.root, t.path, t.value);
        let _ = std::fs::remove_file(backup);
        println!("przed {before:?}, po włączeniu {on:?}, po przywróceniu {after:?}");
        assert_eq!(on, Some(5));
        assert_eq!(after, before);
    }
}

#[cfg(test)]
mod dx_tests {
    use super::dx_settings::{get, with};

    #[test]
    fn dx_settings_text() {
        let t = "VRROptimizeEnable=0;";
        assert_eq!(get(t, "SwapEffectUpgradeEnable"), None);
        let on = with(t, "SwapEffectUpgradeEnable", Some("1"));
        assert_eq!(on, "VRROptimizeEnable=0;SwapEffectUpgradeEnable=1;");
        assert_eq!(get(&on, "swapeffectupgradeenable"), Some("1"));
        assert_eq!(
            with(&on, "SwapEffectUpgradeEnable", Some("0")),
            "VRROptimizeEnable=0;SwapEffectUpgradeEnable=0;"
        );
        assert_eq!(with(&on, "SwapEffectUpgradeEnable", None), t);
        assert_eq!(with("", "A", Some("1")), "A=1;");
    }
}
