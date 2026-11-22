//! Zakładka "Karta graficzna": zegary kart Nvidia i AMD, krzywa V/F kart Nvidia.
//! Podzakładki wzorowane na LACT.

mod amd;
pub mod backend;
mod curve_editor;
mod nvapi;
mod nvml;
mod sampler;
mod sensors_view;

use crate::bridge::Snapshot;
use crate::theme;
use backend::{Backend, Demo, Domain, Monitor, Multi, Offsets, Vendor};
use curve_editor::CurveEditor;
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Ładuje sterowniki kart obecnych w komputerze (Nvidia i AMD); `None`,
/// gdy nie ma żadnej. Każdy wątek korzystający ze sterownika ładuje własny
/// egzemplarz.
pub fn load_drivers() -> Option<Box<dyn Backend>> {
    let mut parts: Vec<Box<dyn Backend>> = Vec::new();
    if let Ok(api) = nvapi::NvApi::load() {
        parts.push(Box::new(api));
    }
    if let Ok(api) = amd::Amd::load() {
        parts.push(Box::new(api));
    }
    (!parts.is_empty()).then(|| Box::new(Multi::new(parts)) as Box<dyn Backend>)
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Info,
    Clocks,
    Curve,
}

/// Znacznik formatu w pliku eksportu.
const EXPORT_FORMAT: &str = "tune-center-gpu-profile";

/// Plik eksportu: profil z opisem, z jakiej karty pochodzi.
#[derive(Serialize, Deserialize)]
struct ExportFile {
    format: String,
    version: u32,
    #[serde(default)]
    gpu: String,
    profile: Profile,
}

/// Liczba slotów profili karty graficznej (przyciski 1-5).
const SLOTS: usize = 5;

#[derive(Serialize, Deserialize, Default, Clone)]
struct Profile {
    /// "nvidia" (offsety) albo "amd" (bezwzględne maksymalne zegary).
    /// Brak pola oznacza profil ze starszej wersji, czyli Nvidię.
    #[serde(default)]
    vendor: String,
    core_offset_mhz: i32,
    mem_offset_mhz: i32,
    /// (indeks punktu, offset w MHz)
    curve: Vec<(usize, i32)>,
    /// Punkt, od którego krzywa jest wyrównana w prawo (limit napięcia).
    #[serde(default)]
    flat_from: Option<usize>,
}

pub struct GpuTab {
    backend: Box<dyn Backend>,
    demo: bool,
    gpus: Vec<String>,
    gpu: usize,
    tab: Tab,
    monitor: Monitor,
    /// Odczyt czujników w osobnym wątku (z historią do wykresów).
    sampler: sampler::Sampler,
    sensors: sensors_view::SensorsView,
    offsets: Offsets,
    offsets_err: Option<String>,
    core_new_mhz: i32,
    mem_new_mhz: i32,
    curve: CurveEditor,
    curve_err: Option<String>,
    /// Automatyczne stosowanie profilu przy starcie i ostatnio użyty profil.
    settings: GpuSettings,
    status: String,
    /// Zapisane profile; `None` oznacza pusty slot.
    slots: Vec<Option<Profile>>,
    /// Po kliknięciu "Zapisz": czekamy na wybór numeru slotu.
    save_mode: bool,
    /// Ostatnio zapisany lub wczytany slot.
    active_slot: Option<usize>,
    /// Ustawienie HAGS w Windows; `None`, gdy nie ma go w rejestrze.
    hags: Option<bool>,
    /// HAGS zmieniono w tej sesji - zmiana czeka na restart komputera.
    hags_pending: bool,
    /// Wykrywanie Resizable BAR (działa w tle od startu programu).
    rebar: crate::rebar::Probe,
}

/// Okno pamięci większe niż 256 MB oznacza włączony Resizable BAR.
const REBAR_OFF_MB: u64 = 256;

/// Ustawienia zakładki pamiętane między uruchomieniami (`gpu-settings.json`).
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
struct GpuSettings {
    /// Stosuj ostatnio użyty profil przy każdym uruchomieniu programu.
    #[serde(default)]
    auto_apply: bool,
    /// Ostatnio zapisany lub wczytany profil (numer slotu od 0).
    #[serde(default)]
    last_slot: Option<usize>,
}

fn load_settings() -> GpuSettings {
    data_file("gpu-settings.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &GpuSettings) {
    if let (Some(path), Ok(json)) = (
        data_file("gpu-settings.json"),
        serde_json::to_string_pretty(settings),
    ) {
        let _ = std::fs::write(path, json);
    }
}

/// Znacznik "profil zastosowany automatycznie, czekamy, czy komputer to
/// wytrzyma". Jest usuwany po minucie pracy albo przy zwykłym zamknięciu
/// programu. Jeśli przy starcie nadal istnieje, poprzednie uruchomienie
/// skończyło się zawieszeniem lub restartem zaraz po zastosowaniu profilu.
fn pending_marker() -> Option<PathBuf> {
    data_file("gpu-autoapply.pending")
}

/// Ile sekund program musi przepracować po automatycznym zastosowaniu
/// profilu, żeby uznać profil za stabilny.
const AUTO_APPLY_GRACE_SECS: u64 = 60;

fn data_file(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(name))
}

/// Wczytuje sloty z `gpu-profiles.json`. Pojedynczy profil ze starszej
/// wersji programu (`gpu-profile.json`) trafia do slotu 1.
fn load_slots() -> Vec<Option<Profile>> {
    let mut slots: Vec<Option<Profile>> = data_file("gpu-profiles.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if slots.is_empty() {
        let old: Option<Profile> = data_file("gpu-profile.json")
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok());
        slots.push(old);
    }
    slots.resize_with(SLOTS, || None);
    slots
}

fn save_slots(slots: &[Option<Profile>]) -> Result<(), String> {
    let path = data_file("gpu-profiles.json").ok_or(crate::i18n::tr(
        "nie można ustalić ścieżki",
        "cannot determine the path",
    ))?;
    let json = serde_json::to_string_pretty(slots).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

fn fmt<T: std::fmt::Display>(v: Option<T>, unit: &str) -> String {
    match v {
        Some(v) => format!("{v:.0} {unit}"),
        None => crate::i18n::tr("brak danych", "no data").to_string(),
    }
}

impl GpuTab {
    /// Podzakładka Overclock z argumentu `--page` (info, clocks, curve).
    pub fn open_page(&mut self, name: &str) {
        match name {
            "clocks" => self.tab = Tab::Clocks,
            "curve" => self.tab = Tab::Curve,
            "info" => self.tab = Tab::Info,
            _ => {}
        }
    }

    /// `demo_flag`: wymusza symulowaną kartę (parametr `--demo`).
    pub fn new(demo_flag: bool) -> Self {
        let (backend, demo, status): (Box<dyn Backend>, bool, String) = if demo_flag {
            (
                Box::new(Demo::new()),
                true,
                crate::i18n::tr(
                    "Tryb demo: dane są symulowane, nic nie trafia do karty.",
                    "Demo mode: data is simulated, nothing is sent to the card.",
                )
                .to_string(),
            )
        } else {
            // Próbujemy obu sterowników; w komputerze mogą być karty obu firm.
            let mut parts: Vec<Box<dyn Backend>> = Vec::new();
            let mut found = Vec::new();
            let mut errors = Vec::new();
            match nvapi::NvApi::load() {
                Ok(api) => {
                    parts.push(Box::new(api));
                    found.push("Nvidia (NVAPI)");
                }
                Err(e) => errors.push(e),
            }
            match amd::Amd::load() {
                Ok(api) => {
                    parts.push(Box::new(api));
                    found.push("AMD (ADL)");
                }
                Err(e) => errors.push(e),
            }
            if parts.is_empty() {
                (
                    Box::new(Demo::new()),
                    true,
                    crate::trf!(
                        "Nie znaleziono karty Nvidia ani AMD ({}). Uruchomiono tryb demo.",
                        "No Nvidia or AMD card found ({}). Demo mode started.",
                        errors.join("; ")
                    ),
                )
            } else {
                (
                    Box::new(Multi::new(parts)),
                    false,
                    crate::trf!("Połączono: {}.", "Connected: {}.", found.join(", ")),
                )
            }
        };

        let gpus = backend.gpus();
        let mut tab = Self {
            backend,
            demo,
            gpus,
            gpu: 0,
            tab: Tab::Info,
            monitor: Monitor::default(),
            sampler: sampler::Sampler::start(demo),
            sensors: Default::default(),
            offsets: Offsets::default(),
            offsets_err: None,
            core_new_mhz: 0,
            mem_new_mhz: 0,
            curve: CurveEditor::default(),
            curve_err: None,
            settings: load_settings(),
            status,
            slots: load_slots(),
            save_mode: false,
            active_slot: None,
            hags: crate::hags::read(),
            hags_pending: false,
            rebar: crate::rebar::Probe::start(),
        };
        tab.active_slot = tab.settings.last_slot.filter(|s| *s < SLOTS);
        tab.reload();
        tab.auto_apply_on_start();
        tab
    }

    /// Zapamiętuje ostatnio użyty profil (także na kolejne uruchomienia).
    fn remember_slot(&mut self, slot: usize) {
        self.active_slot = Some(slot);
        if self.settings.last_slot != Some(slot) {
            self.settings.last_slot = Some(slot);
            save_settings(&self.settings);
        }
    }

    /// Włącza lub wyłącza stosowanie ostatniego profilu przy starcie programu.
    fn set_auto_apply(&mut self, enabled: bool) {
        if enabled {
            let slot = self
                .settings
                .last_slot
                .filter(|s| self.slots.get(*s).is_some_and(|p| p.is_some()));
            let Some(slot) = slot else {
                self.status =
                    crate::i18n::tr("Najpierw zapisz albo wczytaj jeden z profili 1-5 - to on będzie uruchamiany.", "First save or load one of profiles 1-5 - that is the one that will be applied.").to_string();
                return;
            };
            self.status = crate::trf!("Profil {} będzie stosowany przy każdym uruchomieniu programu. Aby działo się to przy starcie Windows, włącz „Uruchamiaj z Windows” w zakładce OPTIONS.", "Profile {} will be applied every time the program starts. To make that happen at Windows startup, turn on “Start with Windows” in the OPTIONS tab.",
                slot + 1
            );
        } else {
            self.status = crate::i18n::tr(
                "Wyłączono automatyczne stosowanie profilu.",
                "Automatic profile apply turned off.",
            )
            .to_string();
        }
        self.settings.auto_apply = enabled;
        save_settings(&self.settings);
    }

    /// Przy starcie programu stosuje ostatnio użyty profil, jeśli użytkownik
    /// to włączył. Zabezpieczenie: jeśli poprzednie automatyczne zastosowanie
    /// skończyło się zawieszeniem, opcja wyłącza się sama, żeby komputer nie
    /// zawieszał się przy każdym starcie.
    fn auto_apply_on_start(&mut self) {
        if self.demo || !self.settings.auto_apply {
            return;
        }
        let Some(marker) = pending_marker() else {
            return;
        };

        if marker.exists() {
            let _ = std::fs::remove_file(&marker);
            self.settings.auto_apply = false;
            save_settings(&self.settings);
            self.status = crate::i18n::tr("Automatyczne stosowanie profilu zostało wyłączone: po poprzednim zastosowaniu program nie przepracował minuty (zawieszenie albo restart). Karta działa na ustawieniach fabrycznych.", "Automatic profile apply has been turned off: after the previous apply the program did not stay up for a minute (freeze or restart). The card is running stock settings.")
                .to_string();
            return;
        }

        let Some(slot) = self.settings.last_slot.filter(|s| *s < SLOTS) else {
            return;
        };
        let Some(profile) = self.slots[slot].clone() else {
            self.status = crate::trf!(
                "Profil {} jest pusty - nie ma czego zastosować przy starcie.",
                "Profile {} is empty - nothing to apply at startup.",
                slot + 1
            );
            return;
        };
        if let Err(e) = self.use_profile(profile) {
            self.status = crate::trf!(
                "Profil {} nie został zastosowany przy starcie: {e}.",
                "Profile {} was not applied at startup: {e}.",
                slot + 1
            );
            return;
        }

        let _ = std::fs::write(&marker, b"");
        if !self.apply() {
            let _ = std::fs::remove_file(&marker);
            self.status = crate::trf!(
                "Profil {} przy starcie: {}",
                "Profile {} at startup: {}",
                slot + 1,
                self.status
            );
            return;
        }
        self.active_slot = Some(slot);
        self.status = crate::trf!(
            "Profil {} zastosowany automatycznie przy starcie. {}",
            "Profile {} applied automatically at startup. {}",
            slot + 1,
            self.status
        );

        // Po minucie stabilnej pracy uznajemy profil za bezpieczny.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(AUTO_APPLY_GRACE_SECS));
            let _ = std::fs::remove_file(marker);
        });
    }

    fn vendor(&self) -> Vendor {
        self.backend.vendor(self.gpu)
    }

    fn vendor_tag(&self) -> &'static str {
        match self.vendor() {
            Vendor::Nvidia => "nvidia",
            Vendor::Amd => "amd",
        }
    }

    /// Wczytuje z karty aktualne offsety i krzywą (porzuca niezapisaną edycję).
    fn reload(&mut self) {
        match self.backend.offsets(self.gpu) {
            Ok(o) => {
                self.core_new_mhz = o.core.map(|c| c.value_khz / 1000).unwrap_or(0);
                self.mem_new_mhz = o.mem.map(|c| c.value_khz / 1000).unwrap_or(0);
                self.offsets = o;
                self.offsets_err = None;
            }
            Err(e) => {
                self.offsets = Offsets::default();
                self.offsets_err = Some(e);
            }
        }
        match self.backend.curve(self.gpu) {
            Ok(points) => {
                self.curve.load(&points);
                self.curve_err = None;
            }
            Err(e) => {
                self.curve.load(&[]);
                self.curve_err = Some(e);
            }
        }
        // Karty AMD nie mają krzywej V/F - nie zostajemy na niedostępnej zakładce.
        if self.vendor() == Vendor::Amd && self.tab == Tab::Curve {
            self.tab = Tab::Info;
        }
        self.sampler.set_gpu(self.gpu);
    }

    fn core_dirty(&self) -> bool {
        self.offsets
            .core
            .is_some_and(|c| c.value_khz / 1000 != self.core_new_mhz)
    }

    fn mem_dirty(&self) -> bool {
        self.offsets
            .mem
            .is_some_and(|c| c.value_khz / 1000 != self.mem_new_mhz)
    }

    fn dirty(&self) -> bool {
        self.core_dirty() || self.mem_dirty() || self.curve.dirty()
    }

    /// Wysyła zmiany do karty. Zwraca false, jeśli karta odrzuciła zapis.
    fn apply(&mut self) -> bool {
        let mut errors = Vec::new();
        if self.core_dirty() {
            if let Err(e) =
                self.backend
                    .set_offset(self.gpu, Domain::Core, self.core_new_mhz * 1000)
            {
                errors.push(e);
            }
        }
        if self.mem_dirty() {
            if let Err(e) =
                self.backend
                    .set_offset(self.gpu, Domain::Memory, self.mem_new_mhz * 1000)
            {
                errors.push(e);
            }
        }
        if self.curve.dirty() {
            if let Err(e) = self.backend.set_curve(self.gpu, &self.curve.deltas()) {
                errors.push(e);
            }
        }
        self.reload();

        // Krzywa wyrównana w prawo: sprawdzamy na odczycie z karty, czy żaden
        // punkt o wyższym napięciu nie ma wyższego zegara niż punkt graniczny
        // (sterownik zaokrągla zegary), i w razie potrzeby poprawiamy zapis.
        let mut lowered = 0;
        let mut still_above = false;
        if errors.is_empty() && self.curve.flat_from.is_some() {
            for round in 0..4 {
                let n = self.curve.enforce_flat(round);
                if n == 0 {
                    break;
                }
                if round == 3 {
                    // Trzy poprawki nie wystarczyły - nie zapisujemy czwartej.
                    self.curve.revert_keep_flat();
                    still_above = true;
                    break;
                }
                lowered = lowered.max(n);
                if let Err(e) = self.backend.set_curve(self.gpu, &self.curve.deltas()) {
                    errors.push(e);
                    break;
                }
                self.reload();
            }
        }

        let ok = errors.is_empty();
        self.status = if !errors.is_empty() {
            crate::trf!("Błąd zapisu: {}", "Write error: {}", errors.join("; "))
        } else if still_above {
            crate::i18n::tr("Zastosowano, ale sterownik nadal ustawia wyższy zegar w części punktów za limitem napięcia. Obniż zaznaczony punkt o 15 MHz i wyrównaj ponownie.", "Applied, but the driver still sets a higher clock at some points past the voltage limit. Lower the selected point by 15 MHz and flatten again.")
                .to_string()
        } else if let Some(mv) = self.curve.flat_voltage_mv() {
            if lowered > 0 {
                crate::trf!("Zastosowano. Limit napięcia {mv:.0} mV: obniżono {lowered} pkt, które sterownik zaokrąglił w górę.", "Applied. Voltage limit {mv:.0} mV: lowered {lowered} points that the driver had rounded up."
                )
            } else {
                crate::trf!(
                    "Zastosowano. Limit napięcia {mv:.0} mV: krzywa po zapisie jest płaska.",
                    "Applied. Voltage limit {mv:.0} mV: the curve is flat after writing."
                )
            }
        } else {
            crate::i18n::tr(
                "Zastosowano. Wartości poniżej są odczytane z karty po zapisie.",
                "Applied. The values below were read back from the card after writing.",
            )
            .to_string()
        };
        ok
    }

    fn reset_defaults(&mut self) {
        // Nvidia: offset 0. AMD: fabryczna wartość maksymalnego zegara.
        self.core_new_mhz = self.offsets.core.map(|c| c.default_khz / 1000).unwrap_or(0);
        self.mem_new_mhz = self.offsets.mem.map(|c| c.default_khz / 1000).unwrap_or(0);
        self.curve.zero_all();
        if self.apply() {
            self.status = crate::i18n::tr(
                "Przywrócono ustawienia fabryczne.",
                "Stock settings restored.",
            )
            .to_string();
        }
    }

    /// Bieżące ustawienia z edycji (także jeszcze niezastosowane) jako profil.
    fn current_profile(&self) -> Profile {
        Profile {
            vendor: self.vendor_tag().to_string(),
            core_offset_mhz: self.core_new_mhz,
            mem_offset_mhz: self.mem_new_mhz,
            curve: self
                .curve
                .deltas()
                .into_iter()
                .map(|(i, khz)| (i, khz / 1000))
                .collect(),
            flat_from: self.curve.flat_from,
        }
    }

    /// Wstawia profil do edycji; do karty trafia dopiero po "Zastosuj".
    fn use_profile(&mut self, p: Profile) -> Result<(), String> {
        // Profil Nvidii zawiera offsety, a AMD wartości bezwzględne -
        // wczytanie cudzego profilu ustawiłoby bezsensowne zegary.
        let saved = if p.vendor.is_empty() {
            "nvidia"
        } else {
            p.vendor.as_str()
        };
        if saved != self.vendor_tag() {
            return Err(crate::i18n::tr(
                "został zapisany dla karty innego producenta",
                "was saved for a card from another vendor",
            )
            .to_string());
        }
        if let Some(c) = self.offsets.core {
            self.core_new_mhz = p.core_offset_mhz.clamp(c.min_khz / 1000, c.max_khz / 1000);
        }
        if let Some(c) = self.offsets.mem {
            self.mem_new_mhz = p.mem_offset_mhz.clamp(c.min_khz / 1000, c.max_khz / 1000);
        }
        for (index, mhz) in p.curve {
            self.curve
                .set_delta_by_index(index, mhz.saturating_mul(1000));
        }
        self.curve.flat_from = p.flat_from;
        Ok(())
    }

    /// Zapisuje bieżące ustawienia do slotu.
    fn save_profile(&mut self, slot: usize) {
        let profile = self.current_profile();
        let previous = self.slots[slot].replace(profile);
        match save_slots(&self.slots) {
            Ok(()) => {
                self.remember_slot(slot);
                self.status = crate::trf!("Zapisano profil {}.", "Profile {} saved.", slot + 1);
            }
            Err(e) => {
                self.slots[slot] = previous;
                self.status =
                    crate::trf!("Błąd zapisu profilu: {e}", "Error saving the profile: {e}");
            }
        }
    }

    /// Wczytuje slot do edycji.
    fn load_profile(&mut self, slot: usize) {
        let Some(p) = self.slots[slot].clone() else {
            self.status = crate::trf!("Profil {} jest pusty.", "Profile {} is empty.", slot + 1);
            return;
        };
        match self.use_profile(p) {
            Ok(()) => {
                self.remember_slot(slot);
                self.status = crate::trf!(
                    "Wczytano profil {}. Kliknij „Zastosuj”, aby wysłać go do karty.",
                    "Profile {} loaded. Click “Apply” to send it to the card.",
                    slot + 1
                );
            }
            Err(e) => self.status = crate::trf!("Profil {} {e}.", "Profile {} {e}.", slot + 1),
        }
    }

    /// Eksport bieżących ustawień do wybranego pliku - kopia, która przetrwa
    /// ponowną instalację programu lub systemu.
    fn export_profile(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title(crate::i18n::tr(
                "Eksportuj profil karty graficznej",
                "Export graphics card profile",
            ))
            .add_filter(
                crate::i18n::tr("Profil Tune Center", "Tune Center profile"),
                &["json"],
            )
            .set_file_name("tune-center-profil-gpu.json")
            .save_file()
        else {
            return; // anulowano
        };
        let file = ExportFile {
            format: EXPORT_FORMAT.to_string(),
            version: 1,
            gpu: self.gpus.get(self.gpu).cloned().unwrap_or_default(),
            profile: self.current_profile(),
        };
        let result = serde_json::to_string_pretty(&file)
            .map_err(|e| e.to_string())
            .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
        self.status = match result {
            Ok(()) => crate::trf!(
                "Wyeksportowano profil do {}",
                "Profile exported to {}",
                path.display()
            ),
            Err(e) => crate::trf!("Błąd eksportu profilu: {e}", "Profile export error: {e}"),
        };
    }

    /// Import profilu z pliku do edycji. Przyjmuje też pojedynczy profil
    /// zapisany przez starsze wersje programu (`gpu-profile.json`).
    fn import_profile(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title(crate::i18n::tr(
                "Importuj profil karty graficznej",
                "Import graphics card profile",
            ))
            .add_filter(
                crate::i18n::tr("Profil Tune Center", "Tune Center profile"),
                &["json"],
            )
            .pick_file()
        else {
            return; // anulowano
        };
        let profile = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| match serde_json::from_str::<ExportFile>(&text) {
                Ok(file) if file.format == EXPORT_FORMAT => Ok(file.profile),
                Ok(_) => Err(crate::i18n::tr(
                    "to nie jest plik profilu Tune Center",
                    "this is not a Tune Center profile file",
                )
                .to_string()),
                Err(_) => serde_json::from_str::<Profile>(&text).map_err(|_| {
                    crate::i18n::tr(
                        "to nie jest plik profilu Tune Center",
                        "this is not a Tune Center profile file",
                    )
                    .to_string()
                }),
            });
        self.status = match profile.and_then(|p| {
            self.use_profile(p)
                .map_err(|e| crate::trf!("profil {e}", "the profile {e}"))
        }) {
            Ok(()) => {
                self.active_slot = None;
                crate::i18n::tr("Zaimportowano profil. Kliknij „Zastosuj”, aby wysłać go do karty, albo „Zapisz” i numer, aby zachować go w programie.", "Profile imported. Click “Apply” to send it to the card, or “Save” and a number to keep it in the program.")
                    .to_string()
            }
            Err(e) => crate::trf!("Błąd importu profilu: {e}", "Profile import error: {e}"),
        };
    }

    /// Przyciski 1-5 i "Zapisz", jak w MSI Afterburner: po kliknięciu
    /// "Zapisz" numery pulsują i kliknięcie numeru zapisuje profil w tym
    /// slocie. Poza trybem zapisu kliknięcie numeru wczytuje profil.
    fn profile_bar(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::i18n::tr("Profil:", "Profile:"));

        let pulse = if self.save_mode {
            // Pulsowanie wymaga ciągłego odświeżania (~30 klatek/s wystarcza).
            ui.ctx().request_repaint_after(crate::ANIMATION_FRAME);
            let t = ui.input(|i| i.time);
            0.5 + 0.5 * (t * 6.0).sin() as f32
        } else {
            0.0
        };

        for slot in 0..SLOTS {
            let filled = self.slots[slot].is_some();
            let label = RichText::new(format!("{}", slot + 1)).strong();
            let mut button = egui::Button::new(label).min_size(egui::vec2(30.0, 24.0));
            if self.save_mode {
                button = button.fill(theme::accent().linear_multiply(0.35 + 0.5 * pulse));
            } else {
                button = button.selected(self.active_slot == Some(slot));
            }

            let hint = if self.save_mode {
                crate::trf!(
                    "Zapisz bieżące ustawienia jako profil {}",
                    "Save current settings as profile {}",
                    slot + 1
                )
            } else if filled {
                crate::trf!("Wczytaj profil {}", "Load profile {}", slot + 1)
            } else {
                crate::trf!("Profil {} jest pusty", "Profile {} is empty", slot + 1)
            };

            let response = ui
                .add_enabled(self.save_mode || filled, button)
                .on_hover_text(hint.clone())
                .on_disabled_hover_text(hint);
            if response.clicked() {
                if self.save_mode {
                    self.save_profile(slot);
                    self.save_mode = false;
                } else {
                    self.load_profile(slot);
                }
            }
        }

        let save = egui::Button::new(if self.save_mode {
            crate::i18n::tr("Anuluj", "Cancel")
        } else {
            crate::i18n::tr("Zapisz", "Save")
        });
        if ui
            .add(save)
            .on_hover_text(crate::i18n::tr(
                "Kliknij, a potem wybierz numer od 1 do 5",
                "Click, then choose a number from 1 to 5",
            ))
            .clicked()
        {
            self.save_mode = !self.save_mode;
            self.status = if self.save_mode {
                crate::i18n::tr(
                    "Wybierz numer od 1 do 5, pod którym zapisać profil.",
                    "Choose a number from 1 to 5 to save the profile under.",
                )
                .to_string()
            } else {
                crate::i18n::tr("Anulowano zapis profilu.", "Profile save cancelled.").to_string()
            };
        }

        if ui
            .button(crate::i18n::tr("Eksportuj", "Export"))
            .on_hover_text(crate::i18n::tr(
                "Zapisuje bieżące ustawienia do wybranego pliku",
                "Saves the current settings to a file you choose",
            ))
            .clicked()
        {
            self.export_profile();
        }
        if ui
            .button(crate::i18n::tr("Importuj", "Import"))
            .on_hover_text(crate::i18n::tr(
                "Wczytuje ustawienia z pliku wyeksportowanego wcześniej",
                "Loads settings from a previously exported file",
            ))
            .clicked()
        {
            self.import_profile();
        }
    }

    /// Nazwa i temperatura wybranej karty odczytane wprost ze sterownika
    /// (NVAPI / ADL); `None` w trybie demo albo gdy sterownik nie podał wartości.
    pub fn reading(&self) -> Option<(&str, f32)> {
        if self.demo {
            return None;
        }
        let name = self.gpus.get(self.gpu)?;
        Some((name.as_str(), self.monitor.temp_c? as f32))
    }

    /// Domyślny limit mocy karty (TDP) ze sterownika, np. dla plakietki
    /// obciążenia w zakładce POWER.
    /// Nazwa wybranej karty NVIDIA (do wyszukania sterownika); `None` dla AMD i demo.
    pub fn nvidia_name(&self) -> Option<String> {
        if self.demo || self.vendor() != Vendor::Nvidia {
            return None;
        }
        self.gpus.get(self.gpu).cloned()
    }

    pub fn power_limit_w(&self) -> Option<f32> {
        if self.demo {
            return None;
        }
        self.monitor.details.tdp_w.filter(|w| *w > 0.0)
    }

    /// Pobór mocy wybranej karty odczytany wprost ze sterownika.
    pub fn power(&self) -> Option<crate::power::GpuPower> {
        if self.demo {
            return None;
        }
        Some(crate::power::GpuPower {
            name: self.gpus.get(self.gpu)?.clone(),
            kind: match self.vendor() {
                Vendor::Nvidia => "GpuNvidia",
                Vendor::Amd => "GpuAmd",
            },
            watts: self.monitor.power_w?,
        })
    }

    /// Bieżące odczyty wybranej karty (zegary, temperatura, moc).
    pub fn monitor(&self) -> &backend::Monitor {
        &self.monitor
    }

    /// Najnowszy odczyt karty z wątku czujników.
    pub fn poll(&mut self) {
        if let Some(m) = self.sampler.latest() {
            self.monitor = m;
        }
    }

    // ------------------------------ zakładki -------------------------------

    /// Włącza albo wyłącza HAGS w ustawieniach Windows.
    fn set_hags(&mut self, enabled: bool) {
        match crate::hags::set(enabled) {
            Ok(()) => {
                self.hags = crate::hags::read();
                self.hags_pending = true;
                self.status = if enabled {
                    crate::i18n::tr(
                        "HAGS włączone. Zmiana zadziała po restarcie komputera.",
                        "HAGS turned on. The change takes effect after the computer restarts.",
                    )
                } else {
                    crate::i18n::tr(
                        "HAGS wyłączone. Zmiana zadziała po restarcie komputera.",
                        "HAGS turned off. The change takes effect after the computer restarts.",
                    )
                }
                .to_string();
            }
            Err(e) => self.status = crate::trf!("HAGS: {e}.", "HAGS: {e}."),
        }
    }

    /// Ustawienia systemu dotyczące karty: HAGS i Resizable BAR.
    fn system_info(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        ui.heading("System");
        ui.add_space(4.0);
        let mut toggle = None;
        egui::Grid::new("system")
            .num_columns(2)
            .spacing([40.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label("HAGS").on_hover_text(crate::i18n::tr(
                    "Sprzętowo przyspieszane planowanie GPU (Hardware-accelerated GPU scheduling).",
                    "Hardware-accelerated GPU scheduling.",
                ));
                ui.horizontal(|ui| {
                    let (text, color) = match self.hags {
                        Some(true) => (crate::i18n::tr("włączone", "on"), theme::accent()),
                        Some(false) => (crate::i18n::tr("wyłączone", "off"), theme::text()),
                        None => (
                            crate::i18n::tr("nieustawione (wyłączone)", "not set (off)"),
                            theme::muted(),
                        ),
                    };
                    ui.label(RichText::new(text).color(color));
                    let enabled = self.hags == Some(true);
                    let button = if enabled {
                        crate::i18n::tr("Wyłącz", "Turn off")
                    } else {
                        crate::i18n::tr("Włącz", "Turn on")
                    };
                    if ui.small_button(button).clicked() {
                        toggle = Some(!enabled);
                    }
                    if self.hags_pending {
                        ui.label(
                            RichText::new(crate::i18n::tr(
                                "wymaga restartu komputera",
                                "restart required",
                            ))
                            .color(theme::warn()),
                        );
                    }
                });
                ui.end_row();

                ui.label("Resizable BAR");
                match self.rebar.state() {
                    crate::rebar::State::Checking => {
                        ui.label(
                            RichText::new(crate::i18n::tr("sprawdzanie...", "checking..."))
                                .color(theme::muted()),
                        );
                    }
                    crate::rebar::State::Known(mb) if mb > REBAR_OFF_MB => {
                        ui.label(
                            RichText::new(crate::trf!(
                                "włączony (okno pamięci {mb} MB)",
                                "on (memory window {mb} MB)"
                            ))
                            .color(theme::accent()),
                        );
                    }
                    crate::rebar::State::Known(mb) => {
                        ui.label(crate::trf!(
                            "wyłączony (okno pamięci {mb} MB)",
                            "off (memory window {mb} MB)"
                        ));
                    }
                    crate::rebar::State::Unknown => {
                        ui.label(
                            RichText::new(crate::i18n::tr("nieznane", "unknown"))
                                .color(theme::muted()),
                        );
                    }
                }
                ui.end_row();
            });
        if let Some(enabled) = toggle {
            self.set_hags(enabled);
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(crate::i18n::tr(
                "HAGS to ustawienie Windows (Ekran > Grafika); pokazany stan to ustawienie, a zmiana działa po restarcie. Jeśli karta lub sterownik nie obsługuje HAGS, włączenie nic nie zmienia. Resizable BAR włącza się w BIOS-ie płyty głównej.",
                "HAGS is a Windows setting (Display > Graphics); the state shown is the setting, and a change takes effect after a restart. If the card or driver does not support HAGS, turning it on changes nothing. Resizable BAR is enabled in the motherboard BIOS.",
            ))
            .size(11.5)
            .color(theme::muted()),
        );
    }

    fn tab_info(&mut self, ui: &mut egui::Ui, snap: Option<&Snapshot>) {
        ui.heading(self.gpus.get(self.gpu).cloned().unwrap_or_default());
        ui.add_space(4.0);
        {
            let samples = self.sampler.samples.lock().unwrap();
            self.sensors.ui(ui, &samples, snap);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(1000));

        ui.add_space(16.0);
        ui.heading(crate::i18n::tr("Aktywne ustawienia", "Active settings"));
        ui.add_space(4.0);
        egui::Grid::new("active")
            .num_columns(2)
            .spacing([40.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                let amd = self.vendor() == Vendor::Amd;
                ui.label(if amd {
                    crate::i18n::tr("Maks. zegar rdzenia", "Max core clock")
                } else {
                    crate::i18n::tr("Offset rdzenia", "Core offset")
                });
                ui.label(fmt(self.offsets.core.map(|c| c.value_khz / 1000), "MHz"));
                ui.end_row();
                ui.label(if amd {
                    crate::i18n::tr("Maks. zegar pamięci", "Max memory clock")
                } else {
                    crate::i18n::tr("Offset pamięci", "Memory offset")
                });
                ui.label(fmt(self.offsets.mem.map(|c| c.value_khz / 1000), "MHz"));
                ui.end_row();
                ui.label(crate::i18n::tr("Punkty krzywej V/F", "V/F curve points"));
                ui.label(if amd {
                    crate::i18n::tr("niedostępna dla kart AMD", "not available for AMD cards")
                        .to_string()
                } else if self.curve.points.is_empty() {
                    crate::i18n::tr("niedostępna", "unavailable").to_string()
                } else {
                    let changed = self
                        .curve
                        .points
                        .iter()
                        .filter(|p| p.applied_khz != 0)
                        .count();
                    crate::trf!(
                        "{} (zmienionych: {changed})",
                        "{} (changed: {changed})",
                        self.curve.points.len()
                    )
                });
                ui.end_row();
            });

        self.system_info(ui);
    }

    fn tab_clocks(&mut self, ui: &mut egui::Ui) {
        let amd = self.vendor() == Vendor::Amd;
        ui.heading(if amd {
            crate::i18n::tr("Maksymalne zegary", "Maximum clocks")
        } else {
            crate::i18n::tr("Offsety zegarów", "Clock offsets")
        });
        ui.add_space(4.0);
        if let Some(e) = &self.offsets_err {
            ui.colored_label(theme::hot(), e);
            return;
        }
        ui.spacing_mut().slider_width = (ui.available_width() - 160.0).max(120.0);

        ui.label(if amd {
            crate::i18n::tr("Rdzeń (maksymalny zegar)", "Core (maximum clock)")
        } else {
            crate::i18n::tr(
                "Rdzeń (przesuwa całą krzywą V/F)",
                "Core (shifts the whole V/F curve)",
            )
        });
        match self.offsets.core {
            Some(c) => {
                ui.add(
                    egui::Slider::new(&mut self.core_new_mhz, c.min_khz / 1000..=c.max_khz / 1000)
                        .suffix(" MHz"),
                );
            }
            None => {
                ui.weak(crate::i18n::tr(
                    "Niedostępne dla tej karty.",
                    "Not available for this card.",
                ));
            }
        }
        ui.add_space(12.0);
        ui.label(if amd {
            crate::i18n::tr("Pamięć (maksymalny zegar)", "Memory (maximum clock)")
        } else {
            crate::i18n::tr("Pamięć", "Memory")
        });
        match self.offsets.mem {
            Some(c) => {
                ui.add(
                    egui::Slider::new(&mut self.mem_new_mhz, c.min_khz / 1000..=c.max_khz / 1000)
                        .suffix(" MHz"),
                );
            }
            None => {
                ui.weak(crate::i18n::tr(
                    "Niedostępne dla tej karty.",
                    "Not available for this card.",
                ));
            }
        }
        ui.add_space(16.0);
        ui.weak(if amd {
            crate::i18n::tr("Zakresy suwaków podaje sterownik. Dla kart AMD ustawia się bezwzględną wartość maksymalnego zegara, a nie offset. Krzywa V/F nie jest dostępna.", "Slider ranges come from the driver. For AMD cards you set the absolute maximum clock, not an offset. The V/F curve is not available.")
        } else {
            crate::i18n::tr("Zakresy suwaków podaje sterownik. Ustawienia nie są zapisywane w karcie: znikają po restarcie systemu lub sterownika.", "Slider ranges come from the driver. Settings are not stored in the card: they are lost when the system or driver restarts.")
        });
    }

    fn tab_curve(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.curve_err {
            ui.heading(crate::i18n::tr("Krzywa V/F", "V/F curve"));
            ui.colored_label(theme::hot(), e);
            ui.weak(crate::i18n::tr(
                "Krzywa V/F jest dostępna od serii GTX 10 wzwyż.",
                "The V/F curve is available from the GTX 10 series onward.",
            ));
            return;
        }
        let live = match (self.monitor.voltage_mv, self.monitor.core_mhz) {
            (Some(mv), Some(mhz)) => Some((mv, mhz)),
            _ => None,
        };
        self.curve.ui(ui, live);
    }
}

impl Drop for GpuTab {
    /// Zwykłe zamknięcie programu to nie zawieszenie: usuwamy znacznik,
    /// żeby automatyczne stosowanie profilu nie wyłączyło się bez powodu.
    fn drop(&mut self) {
        if let Some(marker) = pending_marker() {
            let _ = std::fs::remove_file(marker);
        }
    }
}

impl GpuTab {
    /// Rysuje całą zakładkę wewnątrz przekazanego obszaru.
    pub fn ui(&mut self, ui: &mut egui::Ui, snap: Option<&Snapshot>) {
        self.poll();

        egui::Panel::top("gpu_header")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let before = self.gpu;
                    egui::ComboBox::from_id_salt("gpu")
                        .width((ui.available_width() - 16.0).clamp(120.0, 280.0))
                        .selected_text(self.gpus.get(self.gpu).cloned().unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, name) in self.gpus.iter().enumerate() {
                                ui.selectable_value(&mut self.gpu, i, name);
                            }
                        });
                    if self.gpu != before {
                        self.reload();
                    }
                    ui.separator();
                    for (tab, label) in [
                        (Tab::Info, crate::i18n::tr("Informacje", "Info")),
                        (Tab::Clocks, crate::i18n::tr("Zegary", "Clocks")),
                    ] {
                        if theme::sub_tab(ui, label, self.tab == tab).clicked() {
                            self.tab = tab;
                        }
                    }
                    // Karty AMD nie udostępniają krzywej V/F - opcja jest wyszarzona.
                    let has_curve = self.vendor() != Vendor::Amd;
                    if has_curve {
                        if theme::sub_tab(
                            ui,
                            crate::i18n::tr("Krzywa V/F", "V/F curve"),
                            self.tab == Tab::Curve,
                        )
                        .clicked()
                        {
                            self.tab = Tab::Curve;
                        }
                    } else {
                        ui.add_enabled(
                            false,
                            egui::Button::selectable(
                                false,
                                crate::i18n::tr("Krzywa V/F", "V/F curve"),
                            ),
                        )
                        .on_disabled_hover_text(crate::i18n::tr(
                            "Niedostępne dla kart AMD",
                            "Not available for AMD cards",
                        ));
                    }
                    if self.demo {
                        ui.separator();
                        ui.label(
                            RichText::new(crate::i18n::tr("TRYB DEMO", "DEMO MODE"))
                                .color(theme::warn())
                                .strong(),
                        );
                    }
                });
                ui.add_space(10.0);
            });

        egui::Panel::bottom("gpu_footer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(self.dirty(), egui::Button::new(crate::i18n::tr("Zastosuj", "Apply")))
                        .clicked()
                    {
                        self.apply();
                    }
                    if ui.button(crate::i18n::tr("Przywróć fabryczne", "Restore stock")).clicked() {
                        self.reset_defaults();
                    }
                    if ui.button(crate::i18n::tr("Odczytaj z karty", "Read from card")).clicked() {
                        self.reload();
                        self.status = crate::i18n::tr("Odczytano ustawienia z karty.", "Settings read from the card.").to_string();
                    }
                    ui.separator();
                    self.profile_bar(ui);
                    if !self.demo {
                        ui.separator();
                        let mut auto = self.settings.auto_apply;
                        let label = match self.settings.last_slot {
                            Some(slot) => crate::trf!("Uruchamiaj profil {} z Windows", "Apply profile {} with Windows", slot + 1),
                            None => crate::i18n::tr("Uruchamiaj profil z Windows", "Apply profile with Windows").to_string(),
                        };
                        if ui
                            .checkbox(&mut auto, label)
                            .on_hover_text(
                                crate::i18n::tr("Przy każdym uruchomieniu programu stosuje ostatnio zapisany lub wczytany profil. Jeśli po takim zastosowaniu komputer się zawiesi, opcja wyłączy się sama przy następnym starcie.", "Applies the last saved or loaded profile every time the program starts. If the computer freezes after such an apply, the option turns itself off at the next start."),
                            )
                            .changed()
                        {
                            self.set_auto_apply(auto);
                        }
                    }
                });
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    if self.dirty() {
                        ui.label(RichText::new(crate::i18n::tr("Niezastosowane zmiany.", "Unapplied changes.")).color(theme::warn()));
                    }
                    ui.weak(&self.status);
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| match self.tab {
                Tab::Info => {
                    egui::ScrollArea::vertical().show(ui, |ui| self.tab_info(ui, snap));
                }
                Tab::Clocks => {
                    egui::ScrollArea::vertical().show(ui, |ui| self.tab_clocks(ui));
                }
                Tab::Curve => self.tab_curve(ui),
            });
    }
}
