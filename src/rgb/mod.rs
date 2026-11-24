//! Oświetlenie RGB: wspólny interfejs urządzeń i wątek, który nimi steruje.
//!
//! Każde urządzenie umie trzy rzeczy: ustawić jeden kolor (z jasnością),
//! wyłączyć podświetlenie i oddać sterowanie własnemu efektowi urządzenia.
//! Protokoły zostały napisane od zera na podstawie publicznie znanych faktów
//! o urządzeniach (formaty ramek, numery rejestrów); żaden kod nie jest
//! kopiowany z innych programów.
//!
//! Zasady bezpieczeństwa:
//! - nic nie jest zapisywane w pamięci flash urządzeń - po odłączeniu
//!   zasilania wracają do swoich efektów, a program ustawia kolor ponownie
//!   przy każdym starcie,
//! - wykrywanie urządzeń to wyłącznie odczyty (lista USB, zapytania o
//!   konfigurację); pierwszy zapis następuje dopiero po wyborze trybu,
//! - urządzenia, których nie dało się sprawdzić na prawdziwym sprzęcie, są
//!   oznaczone w interfejsie jako „nietestowane”.

pub mod aura;
pub mod corsair;
pub mod corsair_ram;
pub mod ene_ram;
pub mod fusion2;
mod hid;
pub mod logitech;
pub mod mystic;
pub mod nv_illum;
mod serial;
pub mod skydimo;

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Tryby kontrolerów RAM sprzed pierwszej zmiany (wspólne z konfiguracją).
pub type RamSaved = Arc<Mutex<Vec<(u8, u8, u8)>>>;

/// Co program robi z podświetleniem urządzenia.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Program nie zmienia podświetlenia (efekt urządzenia / innej aplikacji).
    #[default]
    Untouched,
    /// Jeden kolor z jasnością.
    Color,
    /// Podświetlenie wyłączone.
    Off,
}

/// Efekt podświetlenia - wykonywany przez samo urządzenie (jego oprogramowanie),
/// program tylko go wybiera. Każde urządzenie ma swój podzbiór.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Effect {
    /// Stały kolor.
    #[default]
    Static,
    /// Kolor płynnie rozjaśnia się i gaśnie.
    Breathing,
    /// Kolor miga.
    Flashing,
    /// Płynne przechodzenie przez wszystkie kolory.
    ColorCycle,
    /// Tęczowa fala przesuwająca się po diodach.
    Rainbow,
}

impl Effect {
    pub const ALL: [Effect; 5] = [
        Effect::Static,
        Effect::Breathing,
        Effect::Flashing,
        Effect::ColorCycle,
        Effect::Rainbow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Effect::Static => crate::i18n::tr("Stały kolor", "Static color"),
            Effect::Breathing => crate::i18n::tr("Oddychanie", "Breathing"),
            Effect::Flashing => crate::i18n::tr("Miganie", "Flashing"),
            Effect::ColorCycle => crate::i18n::tr("Cykl kolorów", "Color cycle"),
            Effect::Rainbow => crate::i18n::tr("Tęcza (fala)", "Rainbow (wave)"),
        }
    }

    /// Czy efekt korzysta z wybranego koloru (cykl i tęcza mają własne).
    pub fn uses_color(self) -> bool {
        matches!(self, Effect::Static | Effect::Breathing | Effect::Flashing)
    }
}

/// Szybkość efektu.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Speed {
    Slow,
    #[default]
    Normal,
    Fast,
}

impl Speed {
    pub const ALL: [Speed; 3] = [Speed::Slow, Speed::Normal, Speed::Fast];

    pub fn label(self) -> &'static str {
        match self {
            Speed::Slow => crate::i18n::tr("Wolno", "Slow"),
            Speed::Normal => crate::i18n::tr("Średnio", "Normal"),
            Speed::Fast => crate::i18n::tr("Szybko", "Fast"),
        }
    }

    /// 0 = wolno, 1 = średnio, 2 = szybko.
    pub fn level(self) -> u8 {
        match self {
            Speed::Slow => 0,
            Speed::Normal => 1,
            Speed::Fast => 2,
        }
    }

    /// Okres efektu w milisekundach (urządzenia, które przyjmują czas).
    pub fn period_ms(self) -> u16 {
        match self {
            Speed::Slow => 8000,
            Speed::Normal => 4000,
            Speed::Fast => 1500,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Setting {
    pub mode: Mode,
    pub color: [u8; 3],
    /// 0-100 %.
    pub brightness: u8,
    #[serde(default)]
    pub effect: Effect,
    #[serde(default)]
    pub speed: Speed,
}

impl Default for Setting {
    fn default() -> Self {
        Self {
            mode: Mode::Untouched,
            color: [255, 255, 255],
            brightness: 100,
            effect: Effect::Static,
            speed: Speed::Normal,
        }
    }
}

impl Setting {
    /// Kolor przeskalowany jasnością (czarny przy wyłączeniu).
    pub fn rgb(&self) -> [u8; 3] {
        match self.mode {
            Mode::Color => scaled(self.color, self.brightness),
            _ => [0, 0, 0],
        }
    }

    /// Efekt do wykonania na urządzeniu, które obsługuje `supported` (inaczej
    /// stały kolor). Wyłączenie zawsze jako stały czarny / tryb "wyłączone".
    pub fn effect_for(&self, supported: &[Effect]) -> Effect {
        if self.mode == Mode::Color && supported.contains(&self.effect) {
            self.effect
        } else {
            Effect::Static
        }
    }
}

/// Kolor przeskalowany jasnością 0-100 %.
pub fn scaled(color: [u8; 3], brightness: u8) -> [u8; 3] {
    let b = brightness.min(100) as u32;
    color.map(|c| ((c as u32 * b + 50) / 100) as u8)
}

/// Ustawienia oświetlenia zapisywane w konfiguracji programu.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Ten sam tryb i kolor na wszystkich urządzeniach.
    #[serde(default)]
    pub sync: bool,
    #[serde(default)]
    pub all: Setting,
    /// Ustawienia poszczególnych urządzeń (klucz = `Info::id`).
    #[serde(default)]
    pub devices: BTreeMap<String, Setting>,
    /// Tryby kontrolerów RAM sprzed pierwszej zmiany: (adres, direct, tryb).
    #[serde(default)]
    pub ram_saved: Vec<(u8, u8, u8)>,
    /// Ustawienia z OPTIONS (do 0.27) przeniesione do zakładki RGB.
    #[serde(default)]
    pub migrated: bool,
}

impl Config {
    /// Ustawienie obowiązujące dla urządzenia.
    pub fn effective(&self, id: &str) -> Setting {
        if self.sync {
            self.all
        } else {
            self.devices.get(id).copied().unwrap_or_default()
        }
    }
}

/// Rodzina urządzeń (opis i informacja, czy była sprawdzona na sprzęcie).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    EneRam,
    CorsairRam,
    Skydimo,
    Logitech,
    NvidiaGpu,
    AsusAura,
    GigabyteFusion,
    Corsair,
    MsiMystic,
}

impl Family {
    /// Sprawdzone na prawdziwym sprzęcie.
    pub fn tested(self) -> bool {
        matches!(
            self,
            Family::EneRam | Family::CorsairRam | Family::Skydimo | Family::Logitech
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Family::EneRam => crate::i18n::tr("Pamięć RAM (ENE, SMBus)", "RAM (ENE, SMBus)"),
            Family::CorsairRam => {
                crate::i18n::tr("Pamięć RAM Corsair (SMBus)", "Corsair RAM (SMBus)")
            }
            Family::Skydimo => crate::i18n::tr("Taśma Skydimo (USB)", "Skydimo strip (USB)"),
            Family::Logitech => "Logitech G (HID++)",
            Family::NvidiaGpu => crate::i18n::tr("Karta NVIDIA (NVAPI)", "NVIDIA card (NVAPI)"),
            Family::AsusAura => "ASUS Aura (USB)",
            Family::GigabyteFusion => "Gigabyte RGB Fusion 2 (USB)",
            Family::Corsair => "Corsair Lighting Node / Commander Pro",
            Family::MsiMystic => "MSI Mystic Light (USB)",
        }
    }
}

/// Sterowane urządzenie (żyje wyłącznie w wątku RGB).
pub trait Device {
    fn info(&self) -> Info;
    /// Ustawia kolor albo wyłącza podświetlenie (`Mode::Color` / `Mode::Off`).
    fn apply(&mut self, setting: &Setting) -> Result<(), String>;
    /// Oddaje sterowanie urządzeniu (jego własny efekt), jeśli się da.
    fn release(&mut self) -> Result<(), String>;
    /// Urządzenie gaśnie bez regularnych danych - `keepalive` co sekundę.
    fn needs_keepalive(&self) -> bool {
        false
    }
    /// Podtrzymanie ustawienia (domyślnie: ponowne `apply`).
    fn keepalive(&mut self, setting: &Setting) -> Result<(), String> {
        self.apply(setting)
    }
}

/// Opis urządzenia dla interfejsu.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub id: String,
    pub name: String,
    pub family: Family,
    /// Szczegóły (port, liczba diod, adresy) - jedna linijka.
    pub detail: String,
    /// Czy `release` przywraca efekt urządzenia od razu (inaczej po restarcie).
    pub restores: bool,
    /// Efekty, które urządzenie potrafi wykonać (zawsze z `Effect::Static`).
    pub effects: Vec<Effect>,
}

/// Stan urządzenia widoczny w interfejsie.
#[derive(Clone, Debug)]
pub struct Entry {
    pub info: Info,
    /// Wynik ostatniej operacji: Ok(opis) / Err(błąd).
    pub status: Option<Result<String, String>>,
}

/// Rodzina, której nie udało się sprawdzić (np. brak uprawnień).
#[derive(Clone, Debug)]
pub struct Problem {
    pub family: Family,
    pub text: String,
}

#[derive(Default, Clone)]
pub struct Snapshot {
    pub scanning: bool,
    pub devices: Vec<Entry>,
    pub problems: Vec<Problem>,
    /// Nowa kopia trybów RAM do zapisania w konfiguracji.
    pub ram_saved: Option<Vec<(u8, u8, u8)>>,
}

enum Command {
    Config(Config),
    Rescan,
}

/// Wątek sterujący oświetleniem.
pub struct Manager {
    tx: Sender<Command>,
    state: Arc<Mutex<Snapshot>>,
}

impl Manager {
    pub fn start(config: Config, elevated: bool, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let state = Arc::new(Mutex::new(Snapshot {
            scanning: true,
            ..Default::default()
        }));
        let st = state.clone();
        crate::spawn("rgb", move || worker(rx, config, elevated, st, repaint));
        Self { tx, state }
    }

    pub fn set(&self, config: &Config) {
        let _ = self.tx.send(Command::Config(config.clone()));
    }

    pub fn rescan(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.scanning = true;
        }
        let _ = self.tx.send(Command::Rescan);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Nowa kopia trybów RAM (raz) - do zapisania w konfiguracji.
    pub fn take_ram_saved(&self) -> Option<Vec<(u8, u8, u8)>> {
        self.state.lock().ok()?.ram_saved.take()
    }
}

/// Wyszukuje wszystkie obsługiwane urządzenia.
pub(crate) fn discover(
    elevated: bool,
    ram_saved: &RamSaved,
) -> (Vec<Box<dyn Device>>, Vec<Problem>) {
    let mut devices: Vec<Box<dyn Device>> = Vec::new();
    let mut problems = Vec::new();
    let mut add = |family: Family, r: Result<Vec<Box<dyn Device>>, String>| match r {
        Ok(mut d) => devices.append(&mut d),
        Err(text) => problems.push(Problem { family, text }),
    };

    add(
        Family::EneRam,
        ene_ram::discover(elevated, ram_saved.clone()),
    );
    add(Family::CorsairRam, corsair_ram::discover(elevated));
    add(Family::Skydimo, skydimo::discover());
    add(Family::NvidiaGpu, nv_illum::discover());
    match hid::Hid::new() {
        Ok(hid) => {
            add(Family::Logitech, logitech::discover(&hid));
            add(Family::AsusAura, aura::discover(&hid));
            add(Family::GigabyteFusion, fusion2::discover(&hid));
            add(Family::Corsair, corsair::discover(&hid));
            add(Family::MsiMystic, mystic::discover(&hid));
        }
        Err(e) => problems.push(Problem {
            family: Family::Logitech,
            text: e,
        }),
    }
    (devices, problems)
}

fn worker(
    rx: Receiver<Command>,
    mut config: Config,
    elevated: bool,
    state: Arc<Mutex<Snapshot>>,
    repaint: impl Fn(),
) {
    let ram_saved = Arc::new(Mutex::new(config.ram_saved.clone()));
    let mut devices: Vec<Box<dyn Device>> = Vec::new();
    // Ostatnio zastosowane ustawienie (Untouched = nic nie zmieniono).
    let mut applied: Vec<Setting> = Vec::new();
    let mut status: Vec<Option<Result<String, String>>> = Vec::new();
    let mut rescan = true;
    let mut last_keepalive = Instant::now();

    loop {
        if rescan {
            rescan = false;
            // Przed ponownym wyszukaniem oddajemy urządzenia (zamyka porty).
            for (d, a) in devices.iter_mut().zip(&applied) {
                if a.mode != Mode::Untouched {
                    let _ = d.release();
                }
            }
            let (found, problems) = discover(elevated, &ram_saved);
            devices = found;
            applied = vec![Setting::default(); devices.len()];
            status = vec![None; devices.len()];
            if let Ok(mut s) = state.lock() {
                s.scanning = false;
                s.problems = problems;
            }
        }

        // Zastosowanie zmian (i podtrzymanie dla taśm, które bez danych gasną).
        let keepalive = last_keepalive.elapsed() >= Duration::from_secs(1);
        if keepalive {
            last_keepalive = Instant::now();
        }
        for (i, d) in devices.iter_mut().enumerate() {
            let want = config.effective(&d.info().id);
            // Przy "Bez zmian" kolor i efekt nie mają znaczenia.
            let changed = want != applied[i]
                && !(want.mode == Mode::Untouched && applied[i].mode == Mode::Untouched);
            let repeat = keepalive && d.needs_keepalive() && want.mode != Mode::Untouched;
            if !changed && !repeat {
                continue;
            }
            let result = match want.mode {
                Mode::Untouched if applied[i].mode == Mode::Untouched => Ok(()),
                Mode::Untouched => d.release(),
                _ if !changed => d.keepalive(&want),
                _ => d.apply(&want),
            };
            if changed || result.is_err() {
                status[i] = Some(match &result {
                    Ok(()) => Ok(match want.mode {
                        Mode::Untouched => crate::i18n::tr(
                            "Sterowanie oddane urządzeniu.",
                            "Control returned to the device.",
                        )
                        .to_owned(),
                        Mode::Color => crate::i18n::tr("Kolor ustawiony.", "Color set.").to_owned(),
                        Mode::Off => crate::i18n::tr("Wyłączone.", "Turned off.").to_owned(),
                    }),
                    Err(e) => Err(e.clone()),
                });
            }
            // Po błędzie nie ponawiamy w kółko - następna próba przy zmianie
            // ustawień albo ponownym wyszukaniu urządzeń.
            applied[i] = want;
        }

        if let Ok(mut s) = state.lock() {
            let entries: Vec<Entry> = devices
                .iter()
                .zip(&status)
                .map(|(d, st)| Entry {
                    info: d.info(),
                    status: st.clone(),
                })
                .collect();
            let saved = ram_saved.lock().map(|r| r.clone()).unwrap_or_default();
            if saved != config.ram_saved {
                config.ram_saved = saved.clone();
                s.ram_saved = Some(saved);
            }
            let differs = entries.len() != s.devices.len()
                || entries
                    .iter()
                    .zip(&s.devices)
                    .any(|(a, b)| a.info != b.info || a.status != b.status);
            if differs || s.ram_saved.is_some() {
                s.devices = entries;
                repaint();
            }
        }

        match rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(cmd) => {
                let mut handle = |cmd| match cmd {
                    Command::Config(c) => {
                        let saved = config.ram_saved.clone();
                        config = c;
                        config.ram_saved = saved;
                    }
                    Command::Rescan => rescan = true,
                };
                handle(cmd);
                // Kilka zmian naraz (przeciąganie suwaka) - bierzemy ostatnią.
                while let Ok(cmd) = rx.try_recv() {
                    handle(cmd);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_and_effective() {
        assert_eq!(scaled([200, 100, 0], 50), [100, 50, 0]);
        assert_eq!(scaled([255, 255, 255], 100), [255, 255, 255]);
        let off = Setting {
            mode: Mode::Off,
            ..Default::default()
        };
        assert_eq!(off.rgb(), [0, 0, 0]);
        let mut c = Config::default();
        c.devices.insert("a".into(), off);
        assert_eq!(c.effective("a"), off);
        assert_eq!(c.effective("b").mode, Mode::Untouched);
        c.sync = true;
        assert_eq!(c.effective("a").mode, Mode::Untouched);
    }
}

/// Diagnostyka zgodności: wszystkie interfejsy USB HID (bez numerów
/// seryjnych) - z tego da się dopisać obsługę nieznanego kontrolera RGB.
pub fn hid_inventory() -> Vec<String> {
    let Ok(hid) = hid::Hid::new() else {
        return vec!["HID: init failed".into()];
    };
    let mut out: Vec<String> = hid
        .list_all()
        .iter()
        .map(|i| {
            format!(
                "{:04X}:{:04X} if {} page {:04X} usage {:04X} \"{}\"",
                i.vid, i.pid, i.interface, i.usage_page, i.usage, i.product
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod effects_hardware {
    use super::*;

    /// Pokaz efektów na znalezionych urządzeniach RAM ENE i Logitech, każdy
    /// ok. 4 s, potem powrót (ZMIENIA podświetlenie na chwilę):
    /// `cargo test rgb_effects_demo -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rgb_effects_demo() {
        let before = ene_ram::scan().unwrap_or_default();
        let saved: RamSaved = Default::default();
        let (mut devices, problems) = discover(true, &saved);
        println!("problemy: {problems:?}");
        devices.retain(|d| matches!(d.info().family, Family::EneRam | Family::Logitech));
        for d in &mut devices {
            let info = d.info();
            println!("== {} {:?}", info.name, info.effects);
            for e in info.effects.clone() {
                let s = Setting {
                    mode: Mode::Color,
                    color: [255, 0, 80],
                    brightness: 100,
                    effect: e,
                    speed: Speed::Fast,
                };
                println!("{:?}: {:?}", e, d.apply(&s));
                std::thread::sleep(std::time::Duration::from_secs(4));
            }
            if info.family == Family::Logitech {
                println!("oddanie: {:?}", d.release());
            }
        }
        // RAM wraca do trybu sprzed testu.
        if let Ok(bus) = ene_ram::open_bus() {
            for c in &before {
                println!(
                    "RAM 0x{:02X} → tryb {}: {:?}",
                    c.addr,
                    c.mode,
                    ene_ram::restore_mode(&bus, c)
                );
            }
        }
    }
}
