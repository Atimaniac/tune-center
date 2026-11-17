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
    Ratios,
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
    /// Mnożniki turbo w edycji (rdzenie P i E, 8 grup).
    p_ratios: [u8; 8],
    e_ratios: [u8; 8],
    armed: bool,
    status: String,
    /// Odczyty jak w XTU (osobny wątek).
    monitor: Option<crate::cpu::monitor::Monitor>,
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
            p_ratios: [0; 8],
            e_ratios: [0; 8],
            // Zapis dostępny od razu (bez "odblokuj zapis"), jak w zakładce karty.
            armed: true,
            status: String::new(),
            monitor: None,
        };
        if tab.driver.is_ok() {
            tab.monitor = module_path().map(crate::cpu::monitor::Monitor::start);
        }
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
        if let Ok(r) = &state.p_turbo {
            self.p_ratios = *r;
        }
        if let Some(Ok(r)) = &state.e_turbo {
            self.e_ratios = *r;
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

        let readings = self.monitor.as_ref().and_then(|m| m.get());
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(1000));
        egui::ScrollArea::vertical().show(ui, |ui| {
            let avail = ui.available_width();
            let card_w = theme::grid_card_width(avail, 340.0, GAP, 3, MARGIN);
            monitor_section(ui, readings.as_ref());
            ui.add_space(18.0);

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
            if self.ratio_section(ui, &state) {
                action = Some(Action::Ratios);
            }

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
            Some(Action::Ratios) => self.apply_ratios(),
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

/// Panel odczytów jak w Intel XTU (wątek `cpu::monitor`).
fn monitor_section(ui: &mut egui::Ui, readings: Option<&crate::cpu::monitor::Readings>) {
    use crate::i18n::tr;
    ui.label(
        RichText::new(tr("Monitor (jak w Intel XTU)", "Monitor (like Intel XTU)"))
            .size(17.0)
            .strong()
            .color(theme::text()),
    );
    ui.add_space(6.0);
    let Some(r) = readings else {
        ui.label(
            RichText::new(tr("Uruchamianie odczytów...", "Starting readings...")).color(muted()),
        );
        return;
    };
    if let Some(e) = &r.error {
        error(ui, e);
        return;
    }
    let fmt = |v: Option<f32>, unit: &str, dec: usize| match v {
        Some(v) => format!("{v:.dec$} {unit}"),
        None => "—".to_owned(),
    };
    let tiles: Vec<(&str, String)> = vec![
        (
            tr("Temperatura pakietu", "Package temperature"),
            fmt(r.package_c, "°C", 0),
        ),
        (tr("Moc pakietu", "Package power"), fmt(r.package_w, "W", 1)),
        (tr("Moc rdzeni", "Core power"), fmt(r.cores_w, "W", 1)),
        (
            tr("Moc grafiki", "Graphics power"),
            fmt(r.graphics_w, "W", 1),
        ),
        (
            tr("Napięcie rdzenia (VID)", "Core voltage (VID)"),
            fmt(r.vid_v, "V", 3),
        ),
        (
            tr("Zegar cache (ring)", "Cache (ring) clock"),
            fmt(r.ring_ghz, "GHz", 1),
        ),
        (
            tr("Maks. zegar rdzeni P", "Max P-core clock"),
            fmt(r.max_core(false).map(|m| m / 1000.0), "GHz", 2),
        ),
        (
            tr("Maks. zegar rdzeni E", "Max E-core clock"),
            fmt(r.max_core(true).map(|m| m / 1000.0), "GHz", 2),
        ),
        ("TjMax", fmt(r.tjmax_c, "°C", 0)),
        (
            tr("Ograniczenia teraz", "Limits now"),
            if r.limits.is_empty() {
                tr("brak", "none").to_owned()
            } else {
                r.limits.join(", ")
            },
        ),
    ];
    // Kafelki w wierszach - liczba kolumn z szerokości okna.
    let cols = ((ui.available_width() + 8.0) / (150.0 + 20.0 + 8.0))
        .floor()
        .max(1.0) as usize;
    for row in tiles.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(8.0);
            for (name, value) in row {
                theme::card(10, 10).show(ui, |ui| {
                    ui.set_width(150.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(*name).size(11.5).color(muted()));
                        ui.label(
                            RichText::new(value.as_str())
                                .size(17.0)
                                .strong()
                                .color(accent()),
                        );
                    });
                });
            }
        });
        ui.add_space(8.0);
    }
    ui.add_space(8.0);
    egui::Grid::new("cpu_cores")
        .num_columns(4)
        .spacing([28.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for h in [
                tr("Rdzeń", "Core"),
                tr("Temperatura", "Temperature"),
                tr("Zegar", "Clock"),
                "VID",
            ] {
                ui.label(RichText::new(h).size(11.5).color(muted()));
            }
            ui.end_row();
            for c in &r.cores {
                let color = if c.efficient { E_CORE } else { P_CORE };
                ui.label(RichText::new(&c.label).strong().color(color));
                let t = RichText::new(fmt(c.temp_c, "°C", 0));
                ui.label(match c.temp_c {
                    Some(v) => t.color(theme::temp_color(v)),
                    None => t,
                });
                ui.label(fmt(c.mhz, "MHz", 0));
                ui.label(fmt(c.vid_v, "V", 3));
                ui.end_row();
            }
        });
    ui.label(
        RichText::new(tr(
            "Te same rejestry procesora co Intel XTU (temperatury 0x19C/0x1B1, liczniki energii RAPL, VID 0x198, zegary APERF/MPERF, ograniczenia 0x64F), czytane co sekundę przez moduł PawnIO IntelMSR.",
            "The same CPU registers as Intel XTU (temperatures 0x19C/0x1B1, RAPL energy counters, VID 0x198, APERF/MPERF clocks, limits 0x64F), read every second through the PawnIO IntelMSR module.",
        ))
        .size(11.5)
        .color(muted()),
    );
}

impl IntelTab {
    fn ratios_changed(&self, state: &State) -> bool {
        matches!(&state.p_turbo, Ok(r) if *r != self.p_ratios)
            || matches!(&state.e_turbo, Some(Ok(r)) if *r != self.e_ratios)
    }

    /// Zapisuje zmienione mnożniki turbo (rdzenie P, potem E) z weryfikacją.
    fn apply_ratios(&mut self) {
        let Ok(driver) = &mut self.driver else { return };
        let Some(state) = &self.state else { return };
        let mut done = Vec::new();
        let mut error = None;
        let jobs = [
            (
                intel::CoreKind::Performance,
                state.p_turbo.as_ref().ok().copied(),
                self.p_ratios,
                crate::i18n::tr("rdzenie P", "P-cores"),
            ),
            (
                intel::CoreKind::Efficient,
                state
                    .e_turbo
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .copied(),
                self.e_ratios,
                crate::i18n::tr("rdzenie E", "E-cores"),
            ),
        ];
        for (kind, current, new, name) in jobs {
            if current.is_none_or(|c| c == new) {
                continue;
            }
            match intel::set_turbo_ratios(driver, kind, new) {
                Ok(r) => done.push(format!(
                    "{name} {:?}",
                    r.iter().filter(|x| **x != 0).collect::<Vec<_>>()
                )),
                Err(e) => {
                    error = Some(format!("{name}: {e}"));
                    break;
                }
            }
        }
        self.status = match (done.is_empty(), error) {
            (_, Some(e)) => crate::trf!("Mnożniki: {e}.", "Ratios: {e}."),
            (true, None) => {
                crate::i18n::tr("Brak zmian mnożników.", "No ratio changes.").to_string()
            }
            (false, None) => crate::trf!(
                "Zastosowano mnożniki: {} (odczyt po zapisie).",
                "Ratios applied: {} (read back after writing).",
                done.join("; ")
            ),
        };
        self.reload();
    }

    /// Mnożniki turbo dla grup aktywnych rdzeni, edytowalne jak w Intel XTU.
    /// Zwraca true po kliknięciu "Zastosuj mnożniki".
    fn ratio_section(&mut self, ui: &mut egui::Ui, state: &State) -> bool {
        use crate::i18n::tr;
        let mut apply = false;
        ui.label(
            RichText::new(tr("Mnożniki turbo", "Turbo ratios"))
                .size(17.0)
                .strong()
                .color(theme::text()),
        );
        ui.add_space(6.0);
        match &state.p_turbo {
            Ok(current) => ratio_editor(
                ui,
                "p",
                tr("Rdzenie P", "P-cores"),
                current,
                &mut self.p_ratios,
                P_CORE,
            ),
            Err(e) => unavailable(ui, tr("Rdzenie P", "P-cores"), e),
        }
        if let Some(e_turbo) = &state.e_turbo {
            ui.add_space(8.0);
            match e_turbo {
                Ok(current) => ratio_editor(
                    ui,
                    "e",
                    tr("Rdzenie E", "E-cores"),
                    current,
                    &mut self.e_ratios,
                    E_CORE,
                ),
                Err(e) => unavailable(ui, tr("Rdzenie E", "E-cores"), e),
            }
        }
        ui.add_space(8.0);
        let changed = self.ratios_changed(state);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.armed && changed,
                    egui::Button::new(tr("Zastosuj mnożniki", "Apply ratios")),
                )
                .clicked()
            {
                apply = true;
            }
            if ui
                .add_enabled(
                    changed,
                    egui::Button::new(tr("Cofnij zmiany", "Undo changes")),
                )
                .clicked()
            {
                if let Ok(r) = &state.p_turbo {
                    self.p_ratios = *r;
                }
                if let Some(Ok(r)) = &state.e_turbo {
                    self.e_ratios = *r;
                }
            }
            if let Err(e) =
                intel::check_turbo_ratios(&self.p_ratios).and_then(|_| match &state.e_turbo {
                    Some(Ok(_)) => intel::check_turbo_ratios(&self.e_ratios),
                    _ => Ok(()),
                })
            {
                ui.label(RichText::new(e).size(11.5).color(warn()));
            }
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(tr(
                "Jak \"Active Core Ratio\" w Intel XTU: limit mnożnika (x100 MHz), gdy aktywnych jest tyle rdzeni, ile obejmuje grupa (grupa 1 = 1-2 rdzenie itd. - podział ustala procesor). Przy większej liczbie aktywnych rdzeni mnożnik nie może być wyższy. Działa do restartu; zablokowany procesor (bez K) nie przyjmie zmian - program przywróci wtedy poprzednie wartości. Podnoś po 1 i sprawdzaj stabilność.",
                "Like \"Active Core Ratio\" in Intel XTU: the ratio limit (x100 MHz) when as many cores are active as the group covers (group 1 = 1-2 cores etc. - the split is set by the CPU). With more active cores the ratio may not be higher. Lasts until restart; a locked CPU (non-K) will not accept changes - the program then restores the previous values. Raise by 1 and test stability.",
            ))
            .size(11.5)
            .color(muted()),
        );
        apply && changed
    }
}

/// Kafelki mnożników jednej grupy rdzeni z polem edycji (0 = grupa nieużywana).
fn ratio_editor(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    current: &[u8; 8],
    edit: &mut [u8; 8],
    color: Color32,
) {
    ui.label(RichText::new(label).color(muted()));
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(6.0);
        for i in 0..8 {
            if current[i] == 0 {
                continue;
            }
            let changed = edit[i] != current[i];
            egui::Frame::NONE
                .fill(color)
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(74.0);
                        ui.label(
                            RichText::new(crate::trf!("Grupa {}", "Group {}", i + 1))
                                .size(11.5)
                                .color(Color32::WHITE),
                        );
                        ui.push_id((id, i), |ui| {
                            ui.add(
                                egui::DragValue::new(&mut edit[i])
                                    .range(intel::RATIO_MIN..=intel::RATIO_MAX)
                                    .suffix("x")
                                    .speed(0.1),
                            );
                        });
                        let ghz =
                            RichText::new(format!("{:.1} GHz", edit[i] as f32 / 10.0)).size(11.5);
                        ui.label(if changed {
                            ghz.color(Color32::YELLOW).strong()
                        } else {
                            ghz.color(Color32::WHITE)
                        });
                    });
                });
        }
    });
}
