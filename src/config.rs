//! Ustawienia: profile z konfiguracją każdego wentylatora, zapis do pliku JSON.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Dolna granica wysterowania w trybie ręcznym i na krzywej.
/// Chroni przed przypadkowym zatrzymaniem wentylatora CPU.
pub const MIN_PCT: f32 = 20.0;
pub const DEFAULT_PROFILE: &str = "Domyślny";

const FILE: &str = "tunecenter-config.json";
/// Nazwy pliku ustawień z czasów, gdy program nazywał się PCTune i FanCtl.
const OLD_FILES: &[&str] = &["pctune-config.json", "fanctl-config.json"];

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Auto,
    Manual,
    Curve,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FanConfig {
    pub mode: Mode,
    pub manual: f32,
    /// Identyfikator czujnika temperatury dla trybu krzywej.
    pub source: Option<String>,
    /// Punkty krzywej: [temperatura °C, procent].
    pub points: Vec<[f32; 2]>,
}

impl Default for FanConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            manual: 50.0,
            source: None,
            points: vec![[30.0, 25.0], [50.0, 40.0], [70.0, 70.0], [85.0, 100.0]],
        }
    }
}

pub type Fans = HashMap<String, FanConfig>;

/// Profil wentylatorów przypisany grze.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GameProfile {
    /// Folder gry (z launchera) albo plik .exe (gra dodana ręcznie).
    pub path: String,
    pub name: String,
    pub profile: String,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Profile {
    #[serde(default)]
    pub fans: Fans,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    /// Jasny motyw (białe tło, czarny tekst).
    #[serde(default)]
    pub light_theme: bool,
    /// Język interfejsu: "pl" albo "en". `None`: użytkownik jeszcze nie
    /// wybierał - obowiązuje język wybrany w instalatorze, a bez niego polski.
    #[serde(default)]
    pub language: Option<String>,
    /// Minimalizowanie do zasobnika systemowego zamiast do paska zadań.
    #[serde(default = "yes")]
    pub tray: bool,
    /// Wentylatory karty graficznej sterowane wprost przez sterownik karty
    /// (NVAPI / ADL) zamiast przez proces czujników.
    #[serde(default = "yes")]
    pub gpu_fans_direct: bool,
    /// Sprawdzanie przy starcie, czy jest nowsza wersja programu.
    #[serde(default = "yes")]
    pub check_updates: bool,
    /// Sprawdzanie nowego sterownika NVIDIA (przy starcie i co 6 h).
    #[serde(default = "yes")]
    pub nv_driver_check: bool,
    /// Wyłączone podświetlenie LED (pamięć RAM) - stosowane przy każdym starcie.
    #[serde(default)]
    pub leds_off: bool,
    /// Tryby LED sprzed wyłączenia: (adres SMBus, tryb bezpośredni, tryb).
    #[serde(default)]
    pub leds_saved: Vec<(u8, u8, u8)>,
    /// Taśma LED Skydimo (USB): sterowanie włączone, kolor i jasność (0-100 %).
    #[serde(default)]
    pub skydimo_on: bool,
    #[serde(default = "white")]
    pub skydimo_color: [u8; 3],
    #[serde(default = "full")]
    pub skydimo_brightness: u8,
    /// Cena energii za kWh do kosztu sesji w zakładce POWER (0 = nie pokazuj).
    #[serde(default = "default_price")]
    pub energy_price: f32,
    /// Waluta ceny energii (np. "zł").
    #[serde(default = "default_currency")]
    pub energy_currency: String,
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Profil wentylatorów poza grami (wraca po zamknięciu gry).
    #[serde(default)]
    pub global_profile: String,
    /// Profile przypisane grom (folder gry albo plik .exe).
    #[serde(default)]
    pub game_profiles: Vec<GameProfile>,
}

fn white() -> [u8; 3] {
    [255, 255, 255]
}

fn full() -> u8 {
    100
}

fn default_price() -> f32 {
    0.90
}

fn default_currency() -> String {
    "zł".to_owned()
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        let mut cfg = Self {
            light_theme: false,
            language: None,
            tray: true,
            check_updates: true,
            nv_driver_check: true,
            gpu_fans_direct: true,
            leds_off: false,
            leds_saved: Vec::new(),
            skydimo_on: false,
            skydimo_color: white(),
            skydimo_brightness: full(),
            energy_price: default_price(),
            energy_currency: default_currency(),
            active: String::new(),
            profiles: BTreeMap::new(),
            global_profile: String::new(),
            game_profiles: Vec::new(),
        };
        cfg.repair();
        cfg
    }
}

impl Config {
    fn dir() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.to_path_buf())
    }

    fn path() -> Option<PathBuf> {
        Some(Self::dir()?.join(FILE))
    }

    /// Plik ustawień; gdy go nie ma, sięga po plik z wcześniejszej nazwy programu.
    fn existing_path() -> Option<PathBuf> {
        let dir = Self::dir()?;
        std::iter::once(FILE)
            .chain(OLD_FILES.iter().copied())
            .map(|name| dir.join(name))
            .find(|p| p.exists())
    }

    pub fn load() -> Self {
        let mut cfg: Config = Self::existing_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        // Język trzeba ustalić przed naprawą ustawień: nazwa domyślnego
        // profilu zależy od języka.
        let english = match cfg.language.as_deref() {
            Some(code) => code == "en",
            None => Self::installer_language_is_english(),
        };
        crate::i18n::set_english(english);

        cfg.repair();
        // Po starcie obowiązuje profil globalny; profile gier włączy obserwator procesów.
        cfg.active = cfg.global_profile.clone();
        cfg
    }

    /// Instalator zapisuje wybrany język w pliku `default-language.txt`.
    fn installer_language_is_english() -> bool {
        Self::dir()
            .and_then(|dir| std::fs::read_to_string(dir.join("default-language.txt")).ok())
            .is_some_and(|text| text.trim().eq_ignore_ascii_case("en"))
    }

    pub fn save(&self) {
        if let (Some(path), Ok(json)) = (Self::path(), serde_json::to_string_pretty(self)) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Gwarantuje, że istnieje co najmniej jeden profil i że aktywny wskazuje na istniejący.
    fn repair(&mut self) {
        if self.profiles.is_empty() {
            let name = crate::i18n::tr(DEFAULT_PROFILE, "Default");
            self.profiles.insert(name.to_owned(), Profile::default());
        }
        if !self.profiles.contains_key(&self.active) {
            self.active = self.profiles.keys().next().cloned().unwrap_or_default();
        }
        // Profil globalny: przy pierwszym uruchomieniu (i po usunięciu profilu) =
        // bieżący; aktywny po starcie to zawsze globalny (gry przełączą go same).
        if !self.profiles.contains_key(&self.global_profile) {
            self.global_profile = self.active.clone();
        }
    }

    pub fn fans(&self) -> &Fans {
        &self.profiles[&self.active].fans
    }

    pub fn fans_mut(&mut self) -> &mut Fans {
        &mut self.profiles.entry(self.active.clone()).or_default().fans
    }

    /// Zapisuje bieżące ustawienia jako nowy profil i przełącza na niego.
    pub fn save_as(&mut self, name: &str) {
        let copy = self.profiles[&self.active].clone();
        self.profiles.insert(name.to_owned(), copy);
        self.active = name.to_owned();
    }

    pub fn delete_active(&mut self) {
        if self.profiles.len() > 1 {
            self.profiles.remove(&self.active);
            self.repair();
        }
    }
}
