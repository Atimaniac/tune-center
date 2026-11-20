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
mod reg {
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
            .chunks_exact(2)
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
    RegTweak {
        key: "timer",
        root: reg::HKLM,
        path: r"SYSTEM\CurrentControlSet\Control\Session Manager\kernel",
        value: "GlobalTimerResolutionRequests",
        optimized: 1,
        absent: Some(0),
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
        ("timer", 0) => crate::i18n::tr(" - wyłączona", " - off"),
        ("timer", _) => crate::i18n::tr(" - włączona", " - on"),
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

// ---- plan zasilania

#[cfg(windows)]
const POWER_TWEAKS: &[(&str, &str, &str)] = &[
    // (klucz, podgrupa, ustawienie); wartość zalecana 0 = wyłączone
    (
        "usb_suspend",
        "2a737441-1930-4402-8d77-b2bebba308a3",
        "48e6b7a6-50f5-4782-a5d4-53bb8f07e226",
    ),
    (
        "pcie_aspm",
        "501a4d13-42af-4429-9fd1-a8139f2c5c96",
        "ee12f906-d277-404b-b6da-e5fa1a576df5",
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
fn read_all(cpu_x3d: bool) -> Vec<Item> {
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
    for (key, sub, setting) in POWER_TWEAKS {
        let (name, desc) = if *key == "usb_suspend" {
            (
                text("Zasilanie: USB Selective Suspend wyłączone", "Power: USB Selective Suspend off"),
                crate::i18n::tr(
                    "Porty USB nie usypiają urządzeń (mysz, klawiatura, interfejs audio) - bez opóźnień przy wybudzaniu. Dotyczy aktywnego planu zasilania (zasilanie sieciowe).",
                    "USB ports do not suspend devices (mouse, keyboard, audio interface) - no wake-up delays. Applies to the active power plan (AC power).",
                ),
            )
        } else {
            (
                text("Zasilanie: PCI Express Link State Power Management wyłączone", "Power: PCI Express Link State Power Management off"),
                crate::i18n::tr(
                    "Łącza PCIe (karta graficzna, NVMe, sieć) nie przechodzą w stany oszczędzania - bez opóźnień przy powrocie do pracy. Dotyczy aktywnego planu zasilania.",
                    "PCIe links (graphics card, NVMe, network) do not enter power saving states - no delays when resuming. Applies to the active power plan.",
                ),
            )
        };
        let state = match power::read_ac(&power::Guid::parse(sub), &power::Guid::parse(setting)) {
            Some(0) => State::Optimized(power_text(key, 0)),
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
    let t = reg_item("timer");
    items.push(Item {
        key: t.key.into(),
        name: text("Globalna rozdzielczość timera (Windows 11)", "Global timer resolution (Windows 11)"),
        desc: crate::i18n::tr(
            "Od Windows 10 2004 dokładniejszy timer zamówiony przez jeden program nie działa dla pozostałych. Ta wartość przywraca działanie globalne - pomaga grom, które tego wymagają (np. ograniczenie do 64 FPS).",
            "Since Windows 10 2004 a finer timer requested by one program does not apply to others. This value restores the global behavior - helps games that need it (e.g. a 64 FPS cap).",
        ),
        group: Group::Test,
        restart: true,
        state: reg_state(t, t.path),
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
    items
}

/// Zmienia pozycję: `on` = wartość zalecana, `off` = oryginał.
#[cfg(windows)]
fn set(key: &str, on: bool) -> Result<(), String> {
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
    if let Some((_, sub, setting)) = POWER_TWEAKS.iter().find(|(k, _, _)| *k == key) {
        let (sub, setting) = (power::Guid::parse(sub), power::Guid::parse(setting));
        if on {
            remember(key, serde_json::json!(power::read_ac(&sub, &setting)));
            power::write_ac(&sub, &setting, 0)?;
            if power::read_ac(&sub, &setting) != Some(0) {
                return Err(text(
                    "plan zasilania nie przyjął wartości",
                    "the power plan did not accept the value",
                ));
            }
        } else {
            let orig: Option<u32> = original(key)
                .and_then(|v| serde_json::from_value(v).ok())
                .flatten();
            // Domyślnie Windows ma oba ustawienia włączone (1 = włączone / umiarkowane).
            power::write_ac(&sub, &setting, orig.unwrap_or(1))?;
            forget(key);
        }
        return Ok(());
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
        std::thread::spawn(move || worker(rx, s, cpu_x3d, repaint));
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
                "Ustawienia Windows wpływające na opóźnienia wejścia i DPC. Zaznaczenie ustawia wartość zalecaną, odznaczenie przywraca oryginał. Przed pierwszą zmianą program zapisuje oryginalne wartości (tune-backup.json) i sprawdza każdy zapis odczytem. Zmieniaj po jednej pozycji i porównuj LatencyMon przed i po.",
                "Windows settings that affect input latency and DPC. Ticking sets the recommended value, unticking restores the original. Before the first change the program saves the original values (tune-backup.json) and verifies every write by reading back. Change one item at a time and compare LatencyMon before and after.",
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
    let (badge, color, detail) = match &item.state {
        State::Optimized(v) => (
            crate::i18n::tr("ZALECANE", "RECOMMENDED"),
            theme::ok(),
            Some(v.clone()),
        ),
        State::Default(v) => (
            crate::i18n::tr("DOMYŚLNE", "DEFAULT"),
            theme::muted(),
            Some(v.clone()),
        ),
        State::Unsupported(v) => (
            crate::i18n::tr("NIE DOTYCZY", "N/A"),
            theme::muted(),
            Some(v.clone()),
        ),
        State::Unknown(v) => (
            crate::i18n::tr("BŁĄD ODCZYTU", "READ ERROR"),
            theme::warn(),
            Some(v.clone()),
        ),
    };
    let toggleable = matches!(item.state, State::Optimized(_) | State::Default(_));
    ui.horizontal(|ui| {
        let mut on = matches!(item.state, State::Optimized(_));
        let enabled = idle && allowed && toggleable;
        if ui
            .add_enabled(
                enabled,
                egui::Checkbox::new(
                    &mut on,
                    RichText::new(&item.name).size(14.0).color(theme::text()),
                ),
            )
            .clicked()
        {
            change = Some((item.key.clone(), on));
        }
        if busy == Some(item.key.as_str()) {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::history::badge(ui, badge, color);
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
        ui.add_space(26.0);
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
    /// Zapis i przywrócenie na pozycji, która działa dopiero po restarcie
    /// (GlobalTimerResolutionRequests): `cargo test tune_roundtrip -- --ignored`.
    #[test]
    #[ignore]
    fn tune_roundtrip() {
        let t = super::REG_TWEAKS.iter().find(|t| t.key == "timer").unwrap();
        let before = super::reg::get_dword(t.root, t.path, t.value);
        super::set("timer", true).unwrap();
        assert_eq!(super::reg::get_dword(t.root, t.path, t.value), Some(1));
        super::set("timer", false).unwrap();
        assert_eq!(super::reg::get_dword(t.root, t.path, t.value), before);
        println!(
            "przed {before:?}, po przywróceniu {:?}",
            super::reg::get_dword(t.root, t.path, t.value)
        );
    }
}
