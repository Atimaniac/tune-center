//! Zakładka "Procesor" dla AMD Ryzen: Curve Optimizer dla każdego rdzenia
//! i limity Precision Boost Overdrive (PPT, TDC, EDC, skalar).
//!
//! Zmiany trafiają do procesora po kliknięciu przycisku w danej sekcji
//! i znikają po restarcie albo uśpieniu komputera - program niczego nie
//! zapisuje na stałe ani nie stosuje automatycznie przy starcie.

use eframe::egui::{self, RichText, Vec2};

use crate::cpu::amd::{self, Commands, Generation, Layout, Limit, Readback, Smu, State};
use crate::cpu::pawnio::AmdDriver;
use crate::cpu::CpuInfo;
use crate::theme::{self, accent, hot, muted, warn};

const GAP: f32 = 14.0;
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

impl AmdTab {
    pub fn new(info: CpuInfo) -> Self {
        let generation = Generation::detect(info.family, info.model);
        let driver = match (generation, module("RyzenSMU.bin"), module("AMDFamily17.bin")) {
            (None, _, _) => Err(crate::trf!("Ten procesor AMD (rodzina 0x{:X}, model 0x{:X}) nie jest obsługiwany. Obsługiwane są Ryzen 5000, 7000 i 9000 do komputerów stacjonarnych.", "This AMD processor (family 0x{:X}, model 0x{:X}) is not supported. Supported: desktop Ryzen 5000, 7000 and 9000.",
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
            armed: false,
            status: String::new(),
        };
        tab.reload();
        tab
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

    fn apply_curve(&mut self) {
        let Some(curve) = self.commands().and_then(|c| c.curve) else {
            return;
        };
        let Ok(driver) = &mut self.driver else { return };
        let Some(Some(Ok(cores))) = self.state.as_ref().map(|s| s.cores.as_ref()) else {
            return;
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

        ui.horizontal_wrapped(|ui| {
            let name = if self.info.name.is_empty() {
                crate::i18n::tr("Procesor AMD", "AMD processor")
            } else {
                self.info.name.as_str()
            };
            ui.label(RichText::new(name).size(17.0).strong().color(theme::text()));
            if let Some(generation) = self.generation {
                ui.label(RichText::new(generation.name()).color(muted()));
            }
            ui.separator();
            if ui
                .button(crate::i18n::tr("Odczytaj z procesora", "Read from CPU"))
                .clicked()
            {
                action = Some(Action::Reload);
            }
            if self.driver.is_ok() {
                ui.checkbox(
                    &mut self.armed,
                    crate::i18n::tr(
                        "Rozumiem ryzyko, odblokuj zapis",
                        "I understand the risk, unlock writing",
                    ),
                );
            }
        });
        ui.add_space(6.0);

        if let Err(e) = &self.driver {
            ui.label(RichText::new(e).color(warn()));
            return;
        }
        let Some(state) = self.state.clone() else {
            return;
        };

        if !self.status.is_empty() {
            ui.label(RichText::new(&self.status).color(muted()));
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
            ui.label(
                RichText::new(
                    crate::i18n::tr("Ustawienia działają do restartu lub uśpienia komputera i nie są stosowane automatycznie. Zamknij Ryzen Master i inne programy do strojenia. Zmieniaj Curve Optimizer po kilka punktów i testuj stabilność po każdej zmianie - niestabilny rdzeń często ujawnia się dopiero przy małym obciążeniu.", "Settings last until the computer restarts or sleeps and are not applied automatically. Close Ryzen Master and other tuning programs. Change Curve Optimizer a few points at a time and test stability after each change - an unstable core often shows up only under light load."),
                )
                .size(11.5)
                .color(muted()),
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
            Some(Action::Curve) => self.apply_curve(),
            Some(limit) => self.apply_limit(&limit),
            None => {}
        }
    }

    /// Curve Optimizer: kafelek na rdzeń. Zwraca true po kliknięciu "Zastosuj".
    fn curve_section(&mut self, ui: &mut egui::Ui, state: &State) -> bool {
        let mut apply = false;
        ui.label(
            RichText::new("Curve Optimizer")
                .size(17.0)
                .strong()
                .color(theme::text()),
        );
        ui.add_space(6.0);

        let Some(curve) = self.commands().and_then(|c| c.curve) else {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "Curve Optimizer nie jest obsługiwany dla tego procesora.",
                    "Curve Optimizer is not supported for this processor.",
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
            ui.label(crate::i18n::tr("Wszystkie rdzenie:", "All cores:"));
            ui.add(
                egui::DragValue::new(&mut self.all_cores)
                    .range(range.clone())
                    .speed(0.2),
            );
            if ui
                .button(crate::i18n::tr("Wpisz do wszystkich", "Fill all"))
                .on_hover_text(crate::i18n::tr(
                    "Wypełnia pola poniżej; do procesora trafia dopiero po \"Zastosuj\"",
                    "Fills the fields below; nothing is sent to the CPU until \"Apply\"",
                ))
                .clicked()
            {
                self.margins.fill(self.all_cores);
            }
            let can = self.armed && self.curve_changed();
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(crate::i18n::tr(
                        "Zastosuj Curve Optimizer",
                        "Apply Curve Optimizer",
                    )),
                )
                .clicked()
            {
                apply = true;
            }
        });
        ui.add_space(8.0);

        let tile_w = theme::grid_card_width(ui.available_width(), 150.0, 10.0, 8, 12.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(10.0);
            for (i, (core, current)) in cores.iter().enumerate() {
                theme::card(10, 12).show(ui, |ui| {
                    ui.set_width(tile_w);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(crate::trf!("Rdzeń {}", "Core {}", core.index))
                                .strong()
                                .color(theme::text()),
                        );
                        ui.label(
                            RichText::new(format!("CCD {}", core.ccd))
                                .size(11.5)
                                .color(muted()),
                        );
                        match current {
                            Ok(value) => {
                                ui.label(
                                    RichText::new(crate::trf!(
                                        "obecnie {value:+}",
                                        "current {value:+}"
                                    ))
                                    .size(11.5)
                                    .color(accent()),
                                );
                                if let Some(new) = self.margins.get_mut(i) {
                                    ui.add(
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
                });
            }
        });

        ui.add_space(6.0);
        ui.label(
            RichText::new(crate::trf!("Zakres {}..+{}. Wartość ujemna obniża napięcie rdzenia (jeden punkt to około 3-5 mV), dodatnia je podnosi.{}", "Range {}..+{}. A negative value lowers the core voltage (one point is about 3-5 mV), a positive one raises it.{}",
                curve.min,
                curve.max,
                if curve.set_mailbox.is_some() {
                    crate::i18n::tr(" Na Ryzen 5000 zapis idzie przez skrzynkę MP1, a odczyt kontrolny przez RSMU.", " On Ryzen 5000 writes go through the MP1 mailbox and the verification read through RSMU.")
                } else {
                    ""
                }
            ))
            .size(11.5)
            .color(muted()),
        );
        apply
    }

    /// Limity PBO. Zwraca akcję klikniętego przycisku.
    fn pbo_section(&mut self, ui: &mut egui::Ui, state: &State) -> Option<Action> {
        let mut action = None;
        ui.label(
            RichText::new("Precision Boost Overdrive")
                .size(17.0)
                .strong()
                .color(theme::text()),
        );
        ui.add_space(6.0);

        let width = theme::grid_card_width(ui.available_width(), 380.0, GAP, 1, MARGIN).min(560.0);
        theme::card(14, MARGIN as i8).show(ui, |ui| {
            ui.set_width(width);
            let limits = state.limits.as_ref().ok();
            let confirmed = self.confirmed;
            // Bieżąca wartość z tabeli procesora; "?" gdy odczyt nie jest potwierdzony.
            let current = |ui: &mut egui::Ui, value: Option<f32>, unit: &str, slot: usize| match value {
                Some(v) if confirmed[slot] => {
                    ui.label(RichText::new(format!("{v:.0} {unit}")).color(accent()));
                }
                Some(v) => {
                    ui.label(RichText::new(format!("{v:.0} {unit} ?")).color(warn())).on_hover_text(
                        crate::i18n::tr("Układ tabeli tej wersji nie jest potwierdzony. Wartość zostanie sprawdzona przy pierwszym ustawieniu tego limitu.", "The table layout for this version is not confirmed. The value will be checked the first time you set this limit."),
                    );
                }
                None => {
                    ui.label(RichText::new(crate::i18n::tr("nieznane", "unknown")).color(muted()));
                }
            };

            egui::Grid::new("amd_pbo").num_columns(4).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label("");
                ui.label(RichText::new(crate::i18n::tr("Obecnie", "Current")).size(11.5).color(muted()));
                ui.label(RichText::new(crate::i18n::tr("Nowa wartość", "New value")).size(11.5).color(muted()));
                ui.label("");
                ui.end_row();

                ui.label(crate::i18n::tr("Limit mocy pakietu (PPT)", "Package power limit (PPT)"));
                current(ui, limits.and_then(|l| l.ppt_w), "W", 0);
                ui.add(
                    egui::DragValue::new(&mut self.ppt_w)
                        .range(amd::PPT_MIN_W..=amd::PPT_MAX_W)
                        .suffix(" W"),
                );
                if ui.add_enabled(self.armed, egui::Button::new(crate::i18n::tr("Ustaw PPT", "Set PPT"))).clicked() {
                    action = Some(Action::Ppt);
                }
                ui.end_row();

                ui.label(crate::i18n::tr("Limit prądu ciągłego (TDC)", "Sustained current limit (TDC)"));
                current(ui, limits.and_then(|l| l.tdc_a), "A", 1);
                ui.add(
                    egui::DragValue::new(&mut self.tdc_a)
                        .range(amd::CURRENT_MIN_A..=amd::CURRENT_MAX_A)
                        .suffix(" A"),
                );
                if ui.add_enabled(self.armed, egui::Button::new(crate::i18n::tr("Ustaw TDC", "Set TDC"))).clicked() {
                    action = Some(Action::Tdc);
                }
                ui.end_row();

                ui.label(crate::i18n::tr("Limit prądu szczytowego (EDC)", "Peak current limit (EDC)"));
                current(ui, limits.and_then(|l| l.edc_a), "A", 2);
                ui.add(
                    egui::DragValue::new(&mut self.edc_a)
                        .range(amd::CURRENT_MIN_A..=amd::CURRENT_MAX_A)
                        .suffix(" A"),
                );
                if ui.add_enabled(self.armed, egui::Button::new(crate::i18n::tr("Ustaw EDC", "Set EDC"))).clicked() {
                    action = Some(Action::Edc);
                }
                ui.end_row();

                ui.label(crate::i18n::tr("Skalar PBO", "PBO scalar"));
                match &state.scalar {
                    Ok(s) => ui.label(RichText::new(format!("{s:.0}x")).color(accent())),
                    Err(_) => ui.label(RichText::new(crate::i18n::tr("nieznane", "unknown")).color(muted())),
                };
                ui.add(
                    egui::DragValue::new(&mut self.scalar)
                        .range(amd::SCALAR_MIN..=amd::SCALAR_MAX)
                        .suffix("x"),
                );
                if ui.add_enabled(self.armed, egui::Button::new(crate::i18n::tr("Ustaw skalar", "Set scalar"))).clicked() {
                    action = Some(Action::Scalar);
                }
                ui.end_row();
            });
            ui.add_space(6.0);
            let note = match &state.limits {
                Ok(limits) => crate::trf!("Bieżące limity pochodzą z tabeli procesora (wersja 0x{:06X}). Po każdym ustawieniu program sprawdza, czy tabela pokazuje zadaną wartość. Znak zapytania oznacza odczyt jeszcze niepotwierdzony zapisem.", "Current limits come from the CPU table (version 0x{:06X}). After each change the program checks that the table shows the requested value. A question mark means the readout has not yet been confirmed by a write.",
                    limits.version
                ),
                Err(e) => crate::trf!("Bieżących limitów nie udało się odczytać ({e}). Limity można ustawiać, ale bez potwierdzenia odczytem.", "Current limits could not be read ({e}). Limits can still be set, but without read-back confirmation."
                ),
            };
            ui.label(RichText::new(note).size(11.5).color(muted()));
        });
        action
    }
}
