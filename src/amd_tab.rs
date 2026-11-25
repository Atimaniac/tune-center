//! Zakładka "Procesor" dla AMD Ryzen: Curve Optimizer dla każdego rdzenia
//! i limity Precision Boost Overdrive (PPT, TDC, EDC, skalar).
//!
//! Zmiany trafiają do procesora po kliknięciu przycisku w danej sekcji
//! i znikają po restarcie albo uśpieniu komputera - program niczego nie
//! zapisuje w procesorze ani w BIOS-ie na stałe. Opcjonalnie (`cpu-amd.json`)
//! ostatnio zastosowany Curve Optimizer jest stosowany przy starcie programu,
//! z zabezpieczeniem przed zawieszaniem się przy każdym starcie.

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

use crate::cpu::amd::{self, Commands, Generation, Layout, Limit, Readback, Smu, State};
use crate::cpu::pawnio::AmdDriver;
use crate::cpu::CpuInfo;
use crate::cpu_tab::vendor_header;
use crate::icons::Icon;
use crate::theme::{self, accent, hot, muted, warn};

const MARGIN: f32 = 16.0;

enum Action {
    Reload,
    Curve,
    Ppt,
    Tdc,
    Edc,
    Scalar,
}

pub struct AmdTab {
    info: CpuInfo,
    generation: Option<Generation>,
    driver: Result<AmdDriver, String>,
    state: Option<State>,
    /// Marginesy w edycji, w kolejności rdzeni ze stanu.
    margins: Vec<i32>,
    /// Wartość do wpisania wszystkim rdzeniom naraz.
    all_cores: i32,
    ppt_w: u32,
    tdc_a: u32,
    edc_a: u32,
    scalar: u32,
    /// Położenie limitów w tabeli PM; ustalane przy pierwszym odczycie.
    layout: Option<Layout>,
    /// Czy odczyt PPT, TDC i EDC jest potwierdzony (znana wersja tabeli albo
    /// zapis, po którym tabela pokazała zadaną wartość).
    confirmed: [bool; 3],
    /// Pola limitów zostały już ustawione na wartości z procesora.
    limits_loaded: bool,
    armed: bool,
    status: String,
    settings: Settings,
}

fn limit_slot(limit: Limit) -> usize {
    match limit {
        Limit::Ppt => 0,
        Limit::Tdc => 1,
        Limit::Edc => 2,
    }
}

fn module(name: &str) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("modules").join(name))
}

fn data_file(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(name))
}

/// Ustawienia pamiętane między uruchomieniami (`cpu-amd.json`).
#[derive(Serialize, Deserialize, Default, Clone)]
struct Settings {
    /// Stosuj zapamiętany Curve Optimizer przy każdym uruchomieniu programu.
    #[serde(default)]
    auto_apply: bool,
    /// Ostatnio zastosowane marginesy: (numer rdzenia w systemie, margines).
    #[serde(default)]
    curve: Vec<(usize, i32)>,
}

fn load_settings() -> Settings {
    data_file("cpu-amd.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = data_file("cpu-amd.json").ok_or("brak katalogu programu / no program folder")?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Znacznik "Curve Optimizer zastosowany przy starcie, czekamy, czy komputer
/// to wytrzyma". Usuwany po minucie pracy albo przy zwykłym zamknięciu
/// programu. Jeśli przy starcie nadal istnieje, poprzednie uruchomienie
/// skończyło się zawieszeniem albo restartem zaraz po zastosowaniu.
fn pending_marker() -> Option<PathBuf> {
    data_file("cpu-amd-autoapply.pending")
}

/// Ile sekund program musi przepracować po automatycznym zastosowaniu,
/// żeby uznać ustawienia za stabilne.
const AUTO_APPLY_GRACE_SECS: u64 = 60;

impl Drop for AmdTab {
    /// Zwykłe zamknięcie programu to nie zawieszenie: usuwamy znacznik.
    fn drop(&mut self) {
        if let Some(marker) = pending_marker() {
            let _ = std::fs::remove_file(marker);
        }
    }
}

impl AmdTab {
    pub fn new(info: CpuInfo) -> Self {
        let generation = Generation::detect(info.family, info.model);
        let driver = match (generation, module("RyzenSMU.bin"), module("AMDFamily17.bin")) {
            (None, _, _) => Err(crate::trf!("Ten procesor AMD (rodzina 0x{:X}, model 0x{:X}) nie jest obsługiwany. Obsługiwane są Ryzen 3000, 5000, 7000 i 9000 do komputerów stacjonarnych.", "This AMD processor (family 0x{:X}, model 0x{:X}) is not supported. Supported: desktop Ryzen 3000, 5000, 7000 and 9000.",
                info.family, info.model
            )),
            (Some(_), Some(smu), Some(smn)) => AmdDriver::open(&smu, &smn),
            _ => Err(crate::i18n::tr("Nie można ustalić katalogu programu.", "Cannot determine the program folder.").to_string()),
        };
        let mut tab = Self {
            info,
            generation,
            driver,
            state: None,
            margins: Vec::new(),
            all_cores: 0,
            // Zastępowane wartościami z procesora, gdy uda się je odczytać.
            ppt_w: 142,
            tdc_a: 110,
            edc_a: 170,
            scalar: 1,
            layout: None,
            confirmed: [false; 3],
            limits_loaded: false,
            // Zapis dostępny od razu (bez "odblokuj zapis"), jak w zakładce karty.
            armed: true,
            status: String::new(),
            settings: load_settings(),
        };
        tab.reload();
        tab.auto_apply_on_start();
        tab
    }

    /// Marginesy z ekranu jako pary (numer rdzenia, margines); `None`, gdy
    /// odczyt któregoś rdzenia się nie udał.
    fn current_curve(&self) -> Option<Vec<(usize, i32)>> {
        let Some(Some(Ok(cores))) = self.state.as_ref().map(|s| s.cores.as_ref()) else {
            return None;
        };
        cores
            .iter()
            .map(|(core, m)| m.as_ref().ok().map(|m| (core.index, *m)))
            .collect()
    }

    /// Włącza lub wyłącza stosowanie Curve Optimizera przy starcie programu.
    /// Przy włączeniu zapamiętuje wartości, które procesor ma teraz.
    fn set_auto_apply(&mut self, enabled: bool) {
        if enabled {
            let Some(curve) = self.current_curve() else {
                self.status = crate::i18n::tr(
                    "Nie udało się odczytać Curve Optimizera wszystkich rdzeni - nie ma czego zapamiętać.",
                    "Could not read Curve Optimizer of all cores - there is nothing to remember.",
                )
                .to_string();
                return;
            };
            if curve.iter().all(|(_, m)| *m == 0) {
                self.status = crate::i18n::tr(
                    "Najpierw ustaw i zastosuj Curve Optimizer - to te wartości będą stosowane przy starcie.",
                    "First set and apply Curve Optimizer - those values will be applied at startup.",
                )
                .to_string();
                return;
            }
            self.settings.curve = curve;
        }
        self.settings.auto_apply = enabled;
        self.status = match (save_settings(&self.settings), enabled) {
            (Err(e), _) => crate::trf!("Nie udało się zapisać ustawienia: {e}.", "Could not save the setting: {e}."),
            (Ok(()), true) => crate::i18n::tr(
                "Bieżący Curve Optimizer będzie stosowany przy każdym uruchomieniu programu. Aby działo się to przy starcie Windows, włącz „Uruchamiaj z Windows” w zakładce OPTIONS.",
                "The current Curve Optimizer will be applied every time the program starts. To make that happen at Windows startup, turn on “Start with Windows” in the OPTIONS tab.",
            )
            .to_string(),
            (Ok(()), false) => crate::i18n::tr(
                "Wyłączono stosowanie Curve Optimizera przy starcie.",
                "Curve Optimizer apply at startup turned off.",
            )
            .to_string(),
        };
    }

    /// Przy starcie programu stosuje zapamiętany Curve Optimizer, jeśli
    /// użytkownik to włączył. Jeśli poprzednie takie zastosowanie skończyło
    /// się zawieszeniem albo restartem, opcja wyłącza się sama.
    fn auto_apply_on_start(&mut self) {
        if !self.settings.auto_apply || self.driver.is_err() {
            return;
        }
        let Some(marker) = pending_marker() else {
            return;
        };
        if marker.exists() {
            let _ = std::fs::remove_file(&marker);
            self.settings.auto_apply = false;
            let _ = save_settings(&self.settings);
            self.status = crate::i18n::tr(
                "Stosowanie Curve Optimizera przy starcie zostało wyłączone: po poprzednim zastosowaniu program nie przepracował minuty (zawieszenie albo restart). Procesor działa na ustawieniach z BIOS-u.",
                "Curve Optimizer apply at startup has been turned off: after the previous apply the program did not stay up for a minute (freeze or restart). The CPU runs the BIOS settings.",
            )
            .to_string();
            return;
        }
        let Some(Some(Ok(cores))) = self.state.as_ref().map(|s| s.cores.as_ref()) else {
            return;
        };
        // Ten sam procesor: zapamiętane rdzenie muszą odpowiadać obecnym.
        let saved: std::collections::HashMap<usize, i32> = self.settings.curve.iter().copied().collect();
        if saved.len() != cores.len() || cores.iter().any(|(c, _)| !saved.contains_key(&c.index)) {
            self.status = crate::i18n::tr(
                "Zapamiętany Curve Optimizer nie pasuje do rdzeni tego procesora - nie zastosowano go przy starcie.",
                "The remembered Curve Optimizer does not match this processor's cores - it was not applied at startup.",
            )
            .to_string();
            return;
        }
        self.margins = cores.iter().map(|(c, _)| saved[&c.index]).collect();

        let _ = std::fs::write(&marker, b"");
        let ok = self.apply_curve();
        self.status = if ok {
            crate::trf!(
                "Curve Optimizer zastosowany automatycznie przy starcie. {}",
                "Curve Optimizer applied automatically at startup. {}",
                self.status
            )
        } else {
            let _ = std::fs::remove_file(&marker);
            crate::trf!("Curve Optimizer przy starcie: {}", "Curve Optimizer at startup: {}", self.status)
        };
        if ok {
            // Po minucie stabilnej pracy uznajemy ustawienia za bezpieczne.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(AUTO_APPLY_GRACE_SECS));
                let _ = std::fs::remove_file(marker);
            });
        }
    }

    /// Bieżący limit PPT z tabeli procesora.
    pub fn ppt_limit_w(&self) -> Option<f32> {
        self.state.as_ref()?.limits.as_ref().ok()?.ppt_w
    }

    fn commands(&self) -> Option<Commands> {
        self.generation.map(amd::commands)
    }

    fn reload(&mut self) {
        let (Some(cmds), Some(generation)) = (self.commands(), self.generation) else {
            return;
        };
        let Ok(driver) = &mut self.driver else { return };

        // Układ tabeli PM zależy od jej wersji; ustalamy go raz.
        let layout = match self.layout {
            Some(layout) => layout,
            None => {
                let version = driver.pm_table().map(|(version, _)| version).unwrap_or(0);
                let layout = amd::layout(generation, version);
                self.confirmed = [layout.verified; 3];
                self.layout = Some(layout);
                layout
            }
        };

        let state = amd::read_state(driver, cmds, layout, self.info.physical_cores);
        if let (Ok(limits), false) = (&state.limits, self.limits_loaded) {
            // Pola startują od bieżących ustawień procesora.
            if let Some(v) = limits.ppt_w {
                self.ppt_w = v.round() as u32;
            }
            if let Some(v) = limits.tdc_a {
                self.tdc_a = v.round() as u32;
            }
            if let Some(v) = limits.edc_a {
                self.edc_a = v.round() as u32;
            }
            self.limits_loaded = true;
        }
        if let Some(Ok(cores)) = &state.cores {
            self.margins = cores
                .iter()
                .map(|(_, m)| *m.as_ref().unwrap_or(&0))
                .collect();
        }
        if let Ok(scalar) = &state.scalar {
            self.scalar = (scalar.round() as u32).clamp(amd::SCALAR_MIN, amd::SCALAR_MAX);
        }
        self.state = Some(state);
    }

    fn curve_changed(&self) -> bool {
        match self.state.as_ref().and_then(|s| s.cores.as_ref()) {
            Some(Ok(cores)) => cores
                .iter()
                .zip(&self.margins)
                .any(|((_, current), new)| matches!(current, Ok(c) if c != new)),
            _ => false,
        }
    }

    /// Wysyła zmienione marginesy. Zwraca true, gdy wszystkie się udały.
    fn apply_curve(&mut self) -> bool {
        let Some(curve) = self.commands().and_then(|c| c.curve) else {
            return false;
        };
        let Ok(driver) = &mut self.driver else { return false };
        let Some(Some(Ok(cores))) = self.state.as_ref().map(|s| s.cores.as_ref()) else {
            return false;
        };

        let mut done = 0;
        let mut error = None;
        for ((core, current), new) in cores.iter().zip(&self.margins) {
            let Ok(current) = current else { continue };
            if current == new {
                continue;
            }
            match amd::set_margin(driver, curve, *core, *new) {
                Ok(_) => done += 1,
                Err(e) => {
                    // Po pierwszym błędzie nie ruszamy kolejnych rdzeni.
                    error = Some(e);
                    break;
                }
            }
        }
        let ok = error.is_none();
        self.status = match error {
            Some(e) if done == 0 => format!("Curve Optimizer: {e}."),
            Some(e) => crate::trf!(
                "Zmieniono {done} rdzeni, potem błąd: {e}.",
                "Changed {done} cores, then an error: {e}."
            ),
            None => crate::trf!(
                "Curve Optimizer: zmieniono {done} rdzeni (potwierdzone odczytem).",
                "Curve Optimizer: changed {done} cores (confirmed by read-back)."
            ),
        };
        self.reload();
        // Przy włączonym stosowaniu przy starcie zapamiętujemy nowe wartości.
        if ok && self.settings.auto_apply {
            if let Some(curve) = self.current_curve() {
                self.settings.curve = curve;
                let _ = save_settings(&self.settings);
            }
        }
        ok
    }

    /// Wysyła limit i sprawdza w tabeli PM, czy procesor go pokazuje.
    fn send_limit(&mut self, limit: Limit, name: &str, unit: &str, value: u32) -> String {
        let Some(cmds) = self.commands() else {
            return String::new();
        };
        let Ok(driver) = &mut self.driver else {
            return String::new();
        };
        let sent = match limit {
            Limit::Ppt => amd::set_ppt(driver, cmds, value),
            Limit::Tdc => amd::set_tdc(driver, cmds, value),
            Limit::Edc => amd::set_edc(driver, cmds, value),
        };
        if let Err(e) = sent {
            return format!("{name}: {e}.");
        }
        let Some(layout) = &mut self.layout else {
            return crate::trf!(
                "Wysłano {name} {value} {unit}. SMU przyjęło polecenie.",
                "Sent {name} {value} {unit}. The SMU accepted the command."
            );
        };
        match amd::confirm_limit(driver, layout, limit, value) {
            Ok(Readback::Confirmed) => {
                self.confirmed[limit_slot(limit)] = true;
                crate::trf!(
                    "{name} ustawione na {value} {unit} (potwierdzone odczytem z procesora).",
                    "{name} set to {value} {unit} (confirmed by reading back from the CPU)."
                )
            }
            Ok(Readback::Relocated(_)) => {
                self.confirmed[limit_slot(limit)] = true;
                crate::trf!("{name} ustawione na {value} {unit}. Wartość znaleziono w innym polu tabeli niż zakładane - odczyt {name} został poprawiony.", "{name} set to {value} {unit}. The value was found in a different table field than assumed - the {name} readout has been corrected."
                )
            }
            Ok(Readback::Unconfirmed) => {
                self.confirmed[limit_slot(limit)] = false;
                crate::trf!("SMU przyjęło {name} {value} {unit}, ale tabela procesora nie pokazuje tej wartości. Płyta mogła ograniczyć limit; odczyt {name} jest niepewny.", "The SMU accepted {name} {value} {unit}, but the CPU table does not show that value. The board may have capped the limit; the {name} readout is uncertain."
                )
            }
            Err(e) => crate::trf!(
                "SMU przyjęło {name} {value} {unit}; odczyt kontrolny nie powiódł się: {e}.",
                "The SMU accepted {name} {value} {unit}; the verification read failed: {e}."
            ),
        }
    }

    fn apply_limit(&mut self, action: &Action) {
        self.status = match action {
            Action::Ppt => self.send_limit(Limit::Ppt, "PPT", "W", self.ppt_w),
            Action::Tdc => self.send_limit(Limit::Tdc, "TDC", "A", self.tdc_a),
            Action::Edc => self.send_limit(Limit::Edc, "EDC", "A", self.edc_a),
            Action::Scalar => {
                let Some(cmds) = self.commands() else { return };
                let Ok(driver) = &mut self.driver else { return };
                match amd::set_scalar(driver, cmds, self.scalar) {
                    Ok(s) => crate::trf!(
                        "Skalar PBO: {s:.0}x (odczyt po zapisie).",
                        "PBO scalar: {s:.0}x (read back after writing)."
                    ),
                    Err(e) => crate::trf!("Skalar PBO: {e}.", "PBO scalar: {e}."),
                }
            }
            Action::Reload | Action::Curve => return,
        };
        self.reload();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut action = None;

        if vendor_header(
            ui,
            &self.info.name,
            self.generation.map(|g| g.name()),
        ) {
            action = Some(Action::Reload);
        }
        ui.add_space(10.0);

        if let Err(e) = &self.driver {
            ui.label(RichText::new(e).color(warn()));
            return;
        }
        let Some(state) = self.state.clone() else {
            return;
        };

        if !self.status.is_empty() {
            theme::info(ui, &self.status);
            ui.add_space(6.0);
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.curve_section(ui, &state) {
                action = Some(Action::Curve);
            }
            ui.add_space(18.0);
            if let Some(limit) = self.pbo_section(ui, &state) {
                action = Some(limit);
            }
            ui.add_space(12.0);
            theme::info(
                ui,
                crate::i18n::tr("Ustawienia działają do restartu lub uśpienia komputera. Curve Optimizer może być stosowany przy starcie programu (opcja obok „Zastosuj Curve Optimizer”); limity PBO nie są stosowane automatycznie. Zamknij Ryzen Master i inne programy do strojenia. Zmieniaj Curve Optimizer po kilka punktów i testuj stabilność po każdej zmianie - niestabilny rdzeń często ujawnia się dopiero przy małym obciążeniu.", "Settings last until the computer restarts or sleeps. Curve Optimizer can be applied when the program starts (option next to “Apply Curve Optimizer”); PBO limits are not applied automatically. Close Ryzen Master and other tuning programs. Change Curve Optimizer a few points at a time and test stability after each change - an unstable core often shows up only under light load."),
            );
            ui.add_space(12.0);
        });

        match action {
            Some(Action::Reload) => {
                self.reload();
                self.status = crate::i18n::tr(
                    "Odczytano ustawienia z procesora.",
                    "Settings read from the CPU.",
                )
                .to_string();
            }
            Some(Action::Curve) => {
                self.apply_curve();
            }
            Some(limit) => self.apply_limit(&limit),
            None => {}
        }
    }

    /// Curve Optimizer: kafelek na rdzeń. Zwraca true po kliknięciu "Zastosuj".
    fn curve_section(&mut self, ui: &mut egui::Ui, state: &State) -> bool {
        let mut apply = false;
        theme::heading(ui, Icon::Target, "Curve Optimizer");

        let Some(curve) = self.commands().and_then(|c| c.curve) else {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "Ten procesor nie ma Curve Optimizera - AMD wprowadziło go od Ryzen 5000 (Zen 3). Limity PBO poniżej działają.",
                    "This processor has no Curve Optimizer - AMD introduced it with Ryzen 5000 (Zen 3). The PBO limits below work.",
                ))
                .color(muted()),
            );
            return false;
        };
        let cores = match &state.cores {
            None => return false,
            Some(Err(e)) => {
                ui.label(
                    RichText::new(crate::trf!("Niedostępne: {e}.", "Unavailable: {e}."))
                        .color(hot()),
                );
                return false;
            }
            Some(Ok(cores)) => cores,
        };
        let range = curve.min..=curve.max;

        // Wpisanie jednej wartości do wszystkich pól (bez wysyłania do procesora).
        ui.horizontal_wrapped(|ui| {
            ui.add_space(42.0);
            ui.label(RichText::new(crate::i18n::tr("Wszystkie rdzenie:", "All cores:")).size(14.0));
            ui.add_sized(
                [56.0, 28.0],
                egui::DragValue::new(&mut self.all_cores)
                    .range(range.clone())
                    .speed(0.2),
            );
            theme::vsep(ui, 26.0);
            if theme::primary_button(
                ui,
                Some(Icon::Swap),
                crate::i18n::tr("Wpisz do wszystkich", "Fill all"),
                true,
            )
            .on_hover_text(crate::i18n::tr(
                "Wypełnia pola poniżej; do procesora trafia dopiero po \"Zastosuj\"",
                "Fills the fields below; nothing is sent to the CPU until \"Apply\"",
            ))
            .clicked()
            {
                self.margins.fill(self.all_cores);
            }
            let can = self.armed && self.curve_changed();
            if theme::primary_button(
                ui,
                Some(Icon::Check),
                crate::i18n::tr("Zastosuj Curve Optimizer", "Apply Curve Optimizer"),
                can,
            )
            .clicked()
            {
                apply = true;
            }
            theme::vsep(ui, 26.0);
            let mut auto = self.settings.auto_apply;
            if ui
                .checkbox(&mut auto, crate::i18n::tr("Stosuj przy starcie z Windows", "Apply at Windows startup"))
                .on_hover_text(crate::i18n::tr(
                    "Przy każdym uruchomieniu programu stosuje ostatnio zastosowany Curve Optimizer (przy włączeniu: wartości, które procesor ma teraz). Żeby działo się to przy starcie Windows, włącz też „Uruchamiaj z Windows” w OPTIONS. Jeśli po takim zastosowaniu komputer się zawiesi albo zrestartuje w ciągu minuty, opcja wyłączy się sama.",
                    "Every time the program starts it applies the last applied Curve Optimizer (when turned on: the values the CPU has now). To make that happen at Windows startup, also turn on “Start with Windows” in OPTIONS. If the computer freezes or restarts within a minute after such an apply, the option turns itself off.",
                ))
                .changed()
            {
                self.set_auto_apply(auto);
            }
        });
        ui.add_space(12.0);

        let margins = &mut self.margins;
        theme::card_grid(
            ui,
            cores,
            150.0,
            12.0,
            8,
            10,
            14,
            |ui, i, (core, current), tile_w| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(crate::trf!("Rdzeń {}", "Core {}", core.index))
                            .size(16.0)
                            .strong()
                            .color(theme::text()),
                    );
                    ui.label(
                        RichText::new(format!("CCD {}", core.ccd))
                            .size(12.0)
                            .color(muted()),
                    );
                    match current {
                        Ok(value) => {
                            ui.label(
                                RichText::new(crate::trf!(
                                    "obecnie: {value:+}",
                                    "current: {value:+}"
                                ))
                                .size(13.0)
                                .color(accent()),
                            );
                            ui.add_space(4.0);
                            if let Some(new) = margins.get_mut(i) {
                                ui.add_sized(
                                    [tile_w, 32.0],
                                    egui::DragValue::new(new).range(range.clone()).speed(0.2),
                                );
                            }
                        }
                        Err(e) => {
                            ui.label(
                                RichText::new(crate::i18n::tr("błąd odczytu", "read error"))
                                    .color(hot()),
                            )
                            .on_hover_text(e);
                        }
                    }
                });
            },
        );

        ui.add_space(8.0);
        theme::info(
            ui,
            &crate::trf!("Zakres: {} .. +{}. Wartość ujemna obniża napięcie rdzenia (jeden punkt to około 3-5 mV), dodatnia je podnosi.{}", "Range: {} .. +{}. A negative value lowers the core voltage (one point is about 3-5 mV), a positive one raises it.{}",
                curve.min,
                curve.max,
                if curve.set_mailbox.is_some() {
                    crate::i18n::tr(" Na Ryzen 5000 zapis idzie przez skrzynkę MP1, a odczyt kontrolny przez RSMU.", " On Ryzen 5000 writes go through the MP1 mailbox and the verification read through RSMU.")
                } else {
                    ""
                }
            ),
        );
        apply
    }

    /// Limity PBO. Zwraca akcję klikniętego przycisku.
    fn pbo_section(&mut self, ui: &mut egui::Ui, state: &State) -> Option<Action> {
        let mut action = None;
        let width = (ui.available_width() - 2.0 * MARGIN - 4.0).max(300.0);
        theme::card(14, MARGIN as i8).show(ui, |ui| {
            ui.set_width(width);
            theme::heading(ui, Icon::Power, "Precision Boost Overdrive");
            ui.add_space(4.0);
            let limits = state.limits.as_ref().ok();
            let confirmed = self.confirmed;
            // Bieżąca wartość z tabeli procesora; "?" gdy odczyt nie jest potwierdzony.
            let current = |ui: &mut egui::Ui, value: Option<f32>, unit: &str, slot: usize| match value {
                Some(v) if confirmed[slot] => {
                    ui.label(RichText::new(format!("{v:.0} {unit}")).size(16.0).color(accent()));
                }
                Some(v) => {
                    ui.label(RichText::new(format!("{v:.0} {unit} ?")).size(16.0).color(warn())).on_hover_text(
                        crate::i18n::tr("Układ tabeli tej wersji nie jest potwierdzony. Wartość zostanie sprawdzona przy pierwszym ustawieniu tego limitu.", "The table layout for this version is not confirmed. The value will be checked the first time you set this limit."),
                    );
                }
                None => {
                    ui.label(RichText::new(crate::i18n::tr("nieznane", "unknown")).color(muted()));
                }
            };
            let field = [170.0, 30.0];
            let row_label = |ui: &mut egui::Ui, text: &str| {
                ui.label(RichText::new(text).size(15.0).color(theme::text()));
            };

            ui.horizontal(|ui| {
                ui.add_space(20.0);
                egui::Grid::new("amd_pbo").num_columns(4).spacing([40.0, 10.0]).show(ui, |ui| {
                    ui.label("");
                    ui.label(RichText::new(crate::i18n::tr("Obecnie", "Current")).size(12.5).color(muted()));
                    ui.label(RichText::new(crate::i18n::tr("Nowa wartość", "New value")).size(12.5).color(accent()));
                    ui.label("");
                    ui.end_row();

                    row_label(ui, crate::i18n::tr("Limit mocy pakietu (PPT)", "Package power limit (PPT)"));
                    current(ui, limits.and_then(|l| l.ppt_w), "W", 0);
                    ui.add_sized(field, egui::DragValue::new(&mut self.ppt_w).range(amd::PPT_MIN_W..=amd::PPT_MAX_W).suffix(" W"));
                    if theme::primary_button(ui, None, crate::i18n::tr("Ustaw PPT", "Set PPT"), self.armed).clicked() {
                        action = Some(Action::Ppt);
                    }
                    ui.end_row();

                    row_label(ui, crate::i18n::tr("Limit prądu ciągłego (TDC)", "Sustained current limit (TDC)"));
                    current(ui, limits.and_then(|l| l.tdc_a), "A", 1);
                    ui.add_sized(field, egui::DragValue::new(&mut self.tdc_a).range(amd::CURRENT_MIN_A..=amd::CURRENT_MAX_A).suffix(" A"));
                    if theme::primary_button(ui, None, crate::i18n::tr("Ustaw TDC", "Set TDC"), self.armed).clicked() {
                        action = Some(Action::Tdc);
                    }
                    ui.end_row();

                    row_label(ui, crate::i18n::tr("Limit prądu szczytowego (EDC)", "Peak current limit (EDC)"));
                    current(ui, limits.and_then(|l| l.edc_a), "A", 2);
                    ui.add_sized(field, egui::DragValue::new(&mut self.edc_a).range(amd::CURRENT_MIN_A..=amd::CURRENT_MAX_A).suffix(" A"));
                    if theme::primary_button(ui, None, crate::i18n::tr("Ustaw EDC", "Set EDC"), self.armed).clicked() {
                        action = Some(Action::Edc);
                    }
                    ui.end_row();

                    row_label(ui, crate::i18n::tr("Skalar PBO", "PBO scalar"));
                    match &state.scalar {
                        Ok(s) => ui.label(RichText::new(format!("{s:.0}x")).size(16.0).color(accent())),
                        Err(_) => ui.label(RichText::new(crate::i18n::tr("nieznane", "unknown")).color(muted())),
                    };
                    ui.add_sized(field, egui::DragValue::new(&mut self.scalar).range(amd::SCALAR_MIN..=amd::SCALAR_MAX).suffix("x"));
                    if theme::primary_button(ui, None, crate::i18n::tr("Ustaw skalar", "Set scalar"), self.armed).clicked() {
                        action = Some(Action::Scalar);
                    }
                    ui.end_row();
                });
            });
            ui.add_space(8.0);
            let note = match &state.limits {
                Ok(limits) => crate::trf!("Bieżące limity pochodzą z tabeli procesora (wersja 0x{:06X}). Po każdym ustawieniu program sprawdza, czy tabela pokazuje zadaną wartość. Znak zapytania oznacza odczyt jeszcze niepotwierdzony zapisem.", "Current limits come from the CPU table (version 0x{:06X}). After each change the program checks that the table shows the requested value. A question mark means the readout has not yet been confirmed by a write.",
                    limits.version
                ),
                Err(e) => crate::trf!("Bieżących limitów nie udało się odczytać ({e}). Limity można ustawiać, ale bez potwierdzenia odczytem.", "Current limits could not be read ({e}). Limits can still be set, but without read-back confirmation."
                ),
            };
            theme::info(ui, &note);
        });
        action
    }
}
