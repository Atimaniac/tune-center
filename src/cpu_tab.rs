//! Zakładka "Procesor". Zawartość zależy od wykrytego procesora: dla Intela
//! limity mocy (PL1, PL2, okno czasowe turbo), offsety napięcia rdzeni P,
//! rdzeni E i cache, Core IccMax oraz podgląd mnożników; dla AMD Ryzen
//! Curve Optimizer i limity PBO (moduł `amd_tab`). Opcje drugiego producenta
//! nie są w ogóle pokazywane.
//!
//! Zmiany trafiają do procesora dopiero po kliknięciu "Zastosuj" w danej
//! sekcji i znikają po restarcie komputera - program niczego nie zapisuje
//! na stałe ani nie stosuje automatycznie przy starcie.

use eframe::egui::{self, Color32, CornerRadius, RichText, Vec2};

use crate::amd_tab::AmdTab;
use crate::cpu::intel::{self, State, OFFSET_MAX_MV, OFFSET_MIN_MV};
use crate::cpu::pawnio::PawnIo;
use crate::cpu::{cpu_info, CpuInfo};
use crate::theme::{self, accent, hot, muted, warn};

const GAP: f32 = 14.0;
const MARGIN: f32 = 16.0;

/// Kolory kafelków jak w Intel XTU: rdzenie P niebieskie, rdzenie E zielone.
const P_CORE: Color32 = Color32::from_rgb(0, 104, 181);
const E_CORE: Color32 = Color32::from_rgb(92, 122, 58);

enum Action {
    Reload,
    Limits,
    Voltages,
    IccMax,
}

/// Zakładka dopasowana do producenta procesora.
pub enum CpuTab {
    Intel(IntelTab),
    Amd(AmdTab),
}

impl CpuTab {
    pub fn new() -> Self {
        let info = cpu_info();
        if info.amd {
            Self::Amd(AmdTab::new(info))
        } else {
            Self::Intel(IntelTab::new(info))
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        match self {
            Self::Intel(tab) => tab.ui(ui),
            Self::Amd(tab) => tab.ui(ui),
        }
    }
}

pub struct IntelTab {
    info: CpuInfo,
    driver: Result<PawnIo, String>,
    state: Option<State>,
    // wartości w edycji
    pl1_w: f64,
    pl2_w: f64,
    tau_s: f64,
    /// Offsety w mV, w kolejności `state.voltages`.
    offsets_mv: Vec<i32>,
    iccmax_a: f32,
    armed: bool,
    status: String,
}

fn module_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("modules").join("IntelMSR.bin"))
}

impl IntelTab {
    pub fn new(info: CpuInfo) -> Self {
        let driver = if !info.intel {
            Err(crate::i18n::tr(
                "Strojenie jest dostępne tylko dla procesorów Intel i AMD Ryzen.",
                "Tuning is available only for Intel and AMD Ryzen processors.",
            )
            .to_string())
        } else {
            match module_path() {
                Some(path) => PawnIo::open(&path),
                None => Err(crate::i18n::tr(
                    "Nie można ustalić katalogu programu.",
                    "Cannot determine the program folder.",
                )
                .to_string()),
            }
        };
        let mut tab = Self {
            info,
            driver,
            state: None,
            pl1_w: 0.0,
            pl2_w: 0.0,
            tau_s: 0.0,
            offsets_mv: Vec::new(),
            iccmax_a: 0.0,
            armed: false,
            status: String::new(),
        };
        tab.reload();
        tab
    }

    /// Odczytuje stan z procesora i ustawia pola edycji na odczytane wartości.
    fn reload(&mut self) {
        let Ok(driver) = &mut self.driver else { return };
        let state = intel::read_state(driver, self.info.hybrid);
        if let Ok(limits) = &state.limits {
            self.pl1_w = limits.pl1_w;
            self.pl2_w = limits.pl2_w;
            self.tau_s = limits.tau_s;
        }
        self.offsets_mv = state
            .voltages
            .iter()
            .map(|(_, v)| v.as_ref().map(|v| v.offset_mv).unwrap_or(0))
            .collect();
        if let Some(Ok(amps)) = &state.iccmax_a {
            self.iccmax_a = *amps;
        }
        self.state = Some(state);
    }

    fn limits_changed(&self) -> bool {
        match self.state.as_ref().map(|s| &s.limits) {
            Some(Ok(l)) => {
                (l.pl1_w - self.pl1_w).abs() > 0.05
                    || (l.pl2_w - self.pl2_w).abs() > 0.05
                    || (l.tau_s - self.tau_s).abs() > 0.0005
            }
            _ => false,
        }
    }

    fn voltages_changed(&self) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        state
            .voltages
            .iter()
            .zip(&self.offsets_mv)
            .any(|((_, v), new)| matches!(v, Ok(v) if v.offset_mv != *new))
    }

    fn iccmax_changed(&self) -> bool {
        match self.state.as_ref().and_then(|s| s.iccmax_a.as_ref()) {
            Some(Ok(amps)) => (amps - self.iccmax_a).abs() > 0.1,
            _ => false,
        }
    }

    fn apply_limits(&mut self) {
        let Ok(driver) = &mut self.driver else { return };
        self.status = match intel::set_power_limits(driver, self.pl1_w, self.pl2_w, self.tau_s) {
            Ok(l) => crate::trf!("Zastosowano limity mocy: PL1 {:.0} W, PL2 {:.0} W, okno {:.1} s (odczyt po zapisie).", "Power limits applied: PL1 {:.0} W, PL2 {:.0} W, window {:.1} s (read back after writing).",
                l.pl1_w, l.pl2_w, l.tau_s
            ),
            Err(e) => crate::trf!("Limity mocy: {e}.", "Power limits: {e}."),
        };
        self.reload();
    }

    fn apply_voltages(&mut self) {
        let Ok(driver) = &mut self.driver else { return };
        let Some(state) = &self.state else { return };
        let mut done = Vec::new();
        let mut error = None;
        for ((domain, current), new) in state.voltages.iter().zip(&self.offsets_mv) {
            let Ok(current) = current else { continue };
            if current.offset_mv == *new {
                continue;
            }
            match intel::set_voltage_offset(driver, *domain, *new) {
                Ok(mv) => done.push(format!("{} {mv} mV", domain.label())),
                Err(e) => {
                    // Po pierwszym błędzie nie próbujemy kolejnych domen.
                    error = Some(e);
                    break;
                }
            }
        }
        self.status = match (done.is_empty(), error) {
            (_, Some(e)) if done.is_empty() => crate::trf!("Napięcie: {e}.", "Voltage: {e}."),
            (_, Some(e)) => crate::trf!(
                "Zastosowano: {}. Potem błąd: {e}.",
                "Applied: {}. Then an error: {e}.",
                done.join(", ")
            ),
            (true, None) => crate::i18n::tr(
                "Brak zmian napięcia do zastosowania.",
                "No voltage changes to apply.",
            )
            .to_string(),
            (false, None) => crate::trf!(
                "Zastosowano offsety: {} (odczyt po zapisie).",
                "Offsets applied: {} (read back after writing).",
                done.join(", ")
            ),
        };
        self.reload();
    }

    fn apply_iccmax(&mut self) {
        let Ok(driver) = &mut self.driver else { return };
        self.status = match intel::set_iccmax(driver, self.iccmax_a) {
            Ok(amps) => crate::trf!(
                "Zastosowano Core IccMax: {amps:.2} A (odczyt po zapisie).",
                "Core IccMax applied: {amps:.2} A (read back after writing)."
            ),
            Err(e) => format!("Core IccMax: {e}."),
        };
        self.reload();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut action = None;

        ui.horizontal_wrapped(|ui| {
            let name = if self.info.name.is_empty() {
                crate::i18n::tr("Procesor", "Processor")
            } else {
                self.info.name.as_str()
            };
            ui.label(RichText::new(name).size(17.0).strong().color(theme::text()));
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
            let avail = ui.available_width();
            let card_w = theme::grid_card_width(avail, 340.0, GAP, 3, MARGIN);

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(GAP);
                if self.limits_card(ui, &state, card_w) {
                    action = Some(Action::Limits);
                }
                if self.voltage_card(ui, &state, card_w) {
                    action = Some(Action::Voltages);
                }
                if self.iccmax_card(ui, &state, card_w) {
                    action = Some(Action::IccMax);
                }
            });

            ui.add_space(18.0);
            ratio_section(ui, &state);

            ui.add_space(12.0);
            ui.label(
                RichText::new(
                    crate::i18n::tr("Ustawienia działają do restartu komputera i nie są stosowane automatycznie. Zamknij Intel XTU i ThrottleStop - korzystają z tych samych rejestrów. Zmieniaj napięcie po 5-10 mV i testuj stabilność po każdej zmianie.", "Settings last until the computer restarts and are not applied automatically. Close Intel XTU and ThrottleStop - they use the same registers. Change voltage 5-10 mV at a time and test stability after each change."),
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
            Some(Action::Limits) => self.apply_limits(),
            Some(Action::Voltages) => self.apply_voltages(),
            Some(Action::IccMax) => self.apply_iccmax(),
            None => {}
        }
    }

    /// Karta limitów mocy. Zwraca true po kliknięciu "Zastosuj".
    fn limits_card(&mut self, ui: &mut egui::Ui, state: &State, width: f32) -> bool {
        let mut apply = false;
        theme::card(14, MARGIN as i8).show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(|ui| {
                title(ui, crate::i18n::tr("Limity mocy", "Power limits"));
                match &state.limits {
                    Err(e) => error(ui, e),
                    Ok(limits) => {
                        egui::Grid::new("cpu_limits").num_columns(3).spacing([14.0, 8.0]).show(ui, |ui| {
                            header_row(ui);

                            ui.label("Turbo Boost Power Max (PL1)");
                            ui.label(format!("{:.0} W", limits.pl1_w));
                            ui.add(
                                egui::DragValue::new(&mut self.pl1_w)
                                    .range(intel::POWER_MIN_W..=intel::POWER_MAX_W)
                                    .speed(1.0)
                                    .max_decimals(0)
                                    .suffix(" W"),
                            );
                            ui.end_row();

                            ui.label("Turbo Boost Short Power Max (PL2)");
                            ui.label(format!("{:.0} W", limits.pl2_w));
                            ui.add(
                                egui::DragValue::new(&mut self.pl2_w)
                                    .range(intel::POWER_MIN_W..=intel::POWER_MAX_W)
                                    .speed(1.0)
                                    .max_decimals(0)
                                    .suffix(" W"),
                            );
                            ui.end_row();

                            ui.label("Turbo Boost Power Time Window");
                            ui.label(format!("{:.1} s", limits.tau_s));
                            ui.add(
                                egui::DragValue::new(&mut self.tau_s)
                                    .range(intel::TAU_MIN_S..=intel::TAU_MAX_S)
                                    .speed(1.0)
                                    .max_decimals(1)
                                    .suffix(" s"),
                            );
                            ui.end_row();
                        });

                        ui.add_space(6.0);
                        if limits.locked {
                            ui.label(RichText::new(crate::i18n::tr("Zablokowane przez BIOS do restartu.", "Locked by the BIOS until restart.")).color(warn()));
                        }
                        let can = self.armed && !limits.locked && self.limits_changed();
                        if ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Zastosuj limity mocy", "Apply power limits"))).clicked() {
                            apply = true;
                        }
                        note(
                            ui,
                            crate::i18n::tr("Okno czasowe jest zaokrąglane do najbliższej wartości, którą przyjmuje procesor. Jeśli płyta ma własny, niższy limit, obowiązuje niższy z nich.", "The time window is rounded to the nearest value the CPU accepts. If the board has its own, lower limit, the lower of the two applies."),
                        );
                    }
                }
            });
        });
        apply
    }

    /// Karta offsetów napięcia. Zwraca true po kliknięciu "Zastosuj".
    fn voltage_card(&mut self, ui: &mut egui::Ui, state: &State, width: f32) -> bool {
        let mut apply = false;
        theme::card(14, MARGIN as i8).show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(|ui| {
                title(ui, crate::i18n::tr("Offset napięcia (Core Voltage Offset)", "Core Voltage Offset"));
                egui::Grid::new("cpu_voltage").num_columns(3).spacing([14.0, 8.0]).show(ui, |ui| {
                    header_row(ui);
                    for (i, (domain, voltage)) in state.voltages.iter().enumerate() {
                        ui.label(domain.label());
                        match voltage {
                            Ok(v) => {
                                let mode = if v.override_mode { crate::i18n::tr("stałe", "override") } else { crate::i18n::tr("adaptacyjne", "adaptive") };
                                ui.label(format!("{} mV, {mode}", v.offset_mv));
                                if let Some(new) = self.offsets_mv.get_mut(i) {
                                    ui.add(
                                        egui::DragValue::new(new)
                                            .range(OFFSET_MIN_MV..=OFFSET_MAX_MV)
                                            .speed(1.0)
                                            .suffix(" mV"),
                                    );
                                }
                            }
                            Err(e) => {
                                ui.label(RichText::new(crate::i18n::tr("niedostępne", "unavailable")).color(hot())).on_hover_text(e);
                                ui.label("");
                            }
                        }
                        ui.end_row();
                    }
                });

                ui.add_space(6.0);
                let can = self.armed && self.voltages_changed();
                if ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Zastosuj napięcia", "Apply voltages"))).clicked() {
                    apply = true;
                }
                note(
                    ui,
                    &crate::trf!("Zakres {OFFSET_MIN_MV}..{OFFSET_MAX_MV} mV. Ujemny offset obniża napięcie (undervolt); zbyt duży zawiesza komputer. Tryb i mnożnik domeny zostają bez zmian.", "Range {OFFSET_MIN_MV}..{OFFSET_MAX_MV} mV. A negative offset lowers the voltage (undervolt); too much freezes the computer. The domain's mode and ratio are left unchanged."
                    ),
                );
            });
        });
        apply
    }

    /// Karta Core IccMax. Zwraca true po kliknięciu "Zastosuj".
    fn iccmax_card(&mut self, ui: &mut egui::Ui, state: &State, width: f32) -> bool {
        let mut apply = false;
        theme::card(14, MARGIN as i8).show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(|ui| {
                title(ui, "Core IccMax");
                match &state.iccmax_a {
                    None => note(
                        ui,
                        crate::i18n::tr("Zmiana IccMax jest obsługiwana tylko dla procesorów Alder Lake i Raptor Lake (12., 13. i 14. generacja).", "Changing IccMax is supported only for Alder Lake and Raptor Lake processors (12th, 13th and 14th generation)."),
                    ),
                    Some(Err(e)) => error(ui, e),
                    Some(Ok(amps)) => {
                        egui::Grid::new("cpu_iccmax").num_columns(3).spacing([14.0, 8.0]).show(ui, |ui| {
                            header_row(ui);
                            ui.label(crate::i18n::tr("Limit prądu rdzeni", "Core current limit"));
                            ui.label(format!("{amps:.2} A"));
                            ui.add(
                                egui::DragValue::new(&mut self.iccmax_a)
                                    .range(intel::ICCMAX_MIN_A..=intel::ICCMAX_MAX_A)
                                    .speed(1.0)
                                    .max_decimals(2)
                                    .suffix(" A"),
                            );
                            ui.end_row();
                        });
                        ui.add_space(6.0);
                        let can = self.armed && self.iccmax_changed();
                        if ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Zastosuj IccMax", "Apply IccMax"))).clicked() {
                            apply = true;
                        }
                        note(
                            ui,
                            crate::i18n::tr("Krok 0,25 A. Wartość maksymalna (511,75 A) oznacza brak limitu. Zbyt niski limit obniża wydajność pod obciążeniem.", "Step 0.25 A. The maximum value (511.75 A) means no limit. Too low a limit reduces performance under load."),
                        );
                    }
                }
            });
        });
        apply
    }
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(16.0).strong().color(theme::text()));
    ui.add_space(6.0);
}

fn header_row(ui: &mut egui::Ui) {
    ui.label("");
    ui.label(
        RichText::new(crate::i18n::tr("Obecnie", "Current"))
            .size(11.5)
            .color(muted()),
    );
    ui.label(
        RichText::new(crate::i18n::tr("Nowa wartość", "New value"))
            .size(11.5)
            .color(muted()),
    );
    ui.end_row();
}

fn error(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(crate::trf!("Niedostępne: {text}.", "Unavailable: {text}.")).color(hot()),
    );
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).size(11.5).color(muted()));
}

/// Podgląd limitów mnożnika turbo w formie kafelków (tylko odczyt).
fn ratio_section(ui: &mut egui::Ui, state: &State) {
    ui.label(
        RichText::new(crate::i18n::tr(
            "Mnożniki turbo (tylko odczyt)",
            "Turbo ratios (read-only)",
        ))
        .size(17.0)
        .strong()
        .color(theme::text()),
    );
    ui.add_space(6.0);

    // Podgląd jest dodatkiem: gdy moduł PawnIO nie pozwala odczytać rejestru
    // (starsze wydania modułu nie znają rejestru rdzeni E, 0x650), pokazujemy
    // spokojną informację zamiast czerwonego błędu.
    match &state.p_turbo {
        Ok(ratios) => ratio_tiles(ui, crate::i18n::tr("Rdzenie P", "P-cores"), ratios, P_CORE),
        Err(e) => unavailable(ui, crate::i18n::tr("Rdzenie P", "P-cores"), e),
    }
    if let Some(e_turbo) = &state.e_turbo {
        ui.add_space(8.0);
        match e_turbo {
            Ok(ratios) => ratio_tiles(ui, crate::i18n::tr("Rdzenie E", "E-cores"), ratios, E_CORE),
            Err(e) => unavailable(ui, crate::i18n::tr("Rdzenie E", "E-cores"), e),
        }
    }

    ui.add_space(6.0);
    ui.label(
        RichText::new(
            crate::i18n::tr("Kafelek pokazuje limit mnożnika, gdy aktywna jest dana grupa rdzeni (od pierwszej do ósmej). Ustawianie mnożnika osobno dla każdego rdzenia nie jest dostępne - powód opisuje README.", "Each tile shows the ratio limit when the given core group is active (first to eighth). Setting the ratio per core is not available - the README explains why."),
        )
        .size(11.5)
        .color(accent()),
    );
}

/// Informacja o niedostępnym podglądzie; szczegóły błędu są w podpowiedzi.
fn unavailable(ui: &mut egui::Ui, label: &str, details: &str) {
    ui.label(RichText::new(label).color(muted()));
    ui.label(
        RichText::new(
            crate::i18n::tr("Podgląd niedostępny: zainstalowana wersja modułu PawnIO nie pozwala odczytać tego rejestru. Nie wpływa to na pozostałe ustawienia.", "Preview unavailable: the installed PawnIO module version does not allow reading this register. Other settings are not affected."),
        )
        .size(11.5)
        .color(muted()),
    )
    .on_hover_text(details);
}

fn ratio_tiles(ui: &mut egui::Ui, label: &str, ratios: &[u8; 8], color: Color32) {
    ui.label(RichText::new(label).color(muted()));
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(6.0);
        for (i, ratio) in ratios.iter().enumerate() {
            if *ratio == 0 {
                continue;
            }
            egui::Frame::NONE
                .fill(color)
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(64.0);
                        ui.label(
                            RichText::new(crate::trf!("Grupa {}", "Group {}", i + 1))
                                .size(11.5)
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new(format!("{ratio}x"))
                                .size(20.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                    });
                });
        }
    });
}
