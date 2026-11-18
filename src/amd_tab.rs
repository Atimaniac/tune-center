//! ZakĹ‚adka "Procesor" dla AMD Ryzen: Curve Optimizer dla kaĹĽdego rdzenia
//! i limity Precision Boost Overdrive (PPT, TDC, EDC, skalar).
//!
//! Zmiany trafiajÄ… do procesora po klikniÄ™ciu przycisku w danej sekcji
//! i znikajÄ… po restarcie albo uĹ›pieniu komputera - program niczego nie
//! zapisuje na staĹ‚e ani nie stosuje automatycznie przy starcie.

use eframe::egui::{self, RichText};

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
    /// Marginesy w edycji, w kolejnoĹ›ci rdzeni ze stanu.
    margins: Vec<i32>,
    /// WartoĹ›Ä‡ do wpisania wszystkim rdzeniom naraz.
    all_cores: i32,
    ppt_w: u32,
    tdc_a: u32,
    edc_a: u32,
    scalar: u32,
    /// PoĹ‚oĹĽenie limitĂłw w tabeli PM; ustalane przy pierwszym odczycie.
    layout: Option<Layout>,
    /// Czy odczyt PPT, TDC i EDC jest potwierdzony (znana wersja tabeli albo
    /// zapis, po ktĂłrym tabela pokazaĹ‚a zadanÄ… wartoĹ›Ä‡).
    confirmed: [bool; 3],
    /// Pola limitĂłw zostaĹ‚y juĹĽ ustawione na wartoĹ›ci z procesora.
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
            (None, _, _) => Err(crate::trf!("Ten procesor AMD (rodzina 0x{:X}, model 0x{:X}) nie jest obsĹ‚ugiwany. ObsĹ‚ugiwane sÄ… Ryzen 5000, 7000 i 9000 do komputerĂłw stacjonarnych.", "This AMD processor (family 0x{:X}, model 0x{:X}) is not supported. Supported: desktop Ryzen 5000, 7000 and 9000.",
                info.family, info.model
            )),
            (Some(_), Some(smu), Some(smn)) => AmdDriver::open(&smu, &smn),
            _ => Err(crate::i18n::tr("Nie moĹĽna ustaliÄ‡ katalogu programu.", "Cannot determine the program folder.").to_string()),
        };
        let mut tab = Self {
            info,
            generation,
            driver,
            state: None,
            margins: Vec::new(),
            all_cores: 0,
            // ZastÄ™powane wartoĹ›ciami z procesora, gdy uda siÄ™ je odczytaÄ‡.
            ppt_w: 142,
            tdc_a: 110,
            edc_a: 170,
            scalar: 1,
            layout: None,
            confirmed: [false; 3],
            limits_loaded: false,
            // Zapis dostÄ™pny od razu (bez "odblokuj zapis"), jak w zakĹ‚adce karty.
            armed: true,
            status: String::new(),
        };
        tab.reload();
        tab
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

        // UkĹ‚ad tabeli PM zaleĹĽy od jej wersji; ustalamy go raz.
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
            // Pola startujÄ… od bieĹĽÄ…cych ustawieĹ„ procesora.
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
                    // Po pierwszym bĹ‚Ä™dzie nie ruszamy kolejnych rdzeni.
                    error = Some(e);
                    break;
                }
            }
        }
        self.status = match error {
            Some(e) if done == 0 => format!("Curve Optimizer: {e}."),
            Some(e) => crate::trf!(
                "Zmieniono {done} rdzeni, potem bĹ‚Ä…d: {e}.",
                "Changed {done} cores, then an error: {e}."
            ),
            None => crate::trf!(
                "Curve Optimizer: zmieniono {done} rdzeni (potwierdzone odczytem).",
                "Curve Optimizer: changed {done} cores (confirmed by read-back)."
            ),
        };
        self.reload();
    }

    /// WysyĹ‚a limit i sprawdza w tabeli PM, czy procesor go pokazuje.
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
                "WysĹ‚ano {name} {value} {unit}. SMU przyjÄ™Ĺ‚o polecenie.",
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
                crate::trf!("{name} ustawione na {value} {unit}. WartoĹ›Ä‡ znaleziono w innym polu tabeli niĹĽ zakĹ‚adane - odczyt {name} zostaĹ‚ poprawiony.", "{name} set to {value} {unit}. The value was found in a different table field than assumed - the {name} readout has been corrected."
                )
            }
            Ok(Readback::Unconfirmed) => {
                self.confirmed[limit_slot(limit)] = false;
                crate::trf!("SMU przyjÄ™Ĺ‚o {name} {value} {unit}, ale tabela procesora nie pokazuje tej wartoĹ›ci. PĹ‚yta mogĹ‚a ograniczyÄ‡ limit; odczyt {name} jest niepewny.", "The SMU accepted {name} {value} {unit}, but the CPU table does not show that value. The board may have capped the limit; the {name} readout is uncertain."
                )
            }
            Err(e) => crate::trf!(
                "SMU przyjÄ™Ĺ‚o {name} {value} {unit}; odczyt kontrolny nie powiĂłdĹ‚ siÄ™: {e}.",
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
            "AMD",
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
                crate::i18n::tr("Ustawienia dziaĹ‚ajÄ… do restartu lub uĹ›pienia komputera i nie sÄ… stosowane automatycznie. Zamknij Ryzen Master i inne programy do strojenia. Zmieniaj Curve Optimizer po kilka punktĂłw i testuj stabilnoĹ›Ä‡ po kaĹĽdej zmianie - niestabilny rdzeĹ„ czÄ™sto ujawnia siÄ™ dopiero przy maĹ‚ym obciÄ…ĹĽeniu.", "Settings last until the computer restarts or sleeps and are not applied automatically. Close Ryzen Master and other tuning programs. Change Curve Optimizer a few points at a time and test stability after each change - an unstable core often shows up only under light load."),
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

    /// Curve Optimizer: kafelek na rdzeĹ„. Zwraca true po klikniÄ™ciu "Zastosuj".
    fn curve_section(&mut self, ui: &mut egui::Ui, state: &State) -> bool {
        let mut apply = false;
        theme::heading(ui, Icon::Target, "Curve Optimizer");

        let Some(curve) = self.commands().and_then(|c| c.curve) else {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "Curve Optimizer nie jest obsĹ‚ugiwany dla tego procesora.",
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
                    RichText::new(crate::trf!("NiedostÄ™pne: {e}.", "Unavailable: {e}."))
                        .color(hot()),
                );
                return false;
            }
            Some(Ok(cores)) => cores,
        };
        let range = curve.min..=curve.max;

        // Wpisanie jednej wartoĹ›ci do wszystkich pĂłl (bez wysyĹ‚ania do procesora).
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
                "WypeĹ‚nia pola poniĹĽej; do procesora trafia dopiero po \"Zastosuj\"",
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
                        RichText::new(crate::trf!("RdzeĹ„ {}", "Core {}", core.index))
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
                                RichText::new(crate::i18n::tr("bĹ‚Ä…d odczytu", "read error"))
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
            &crate::trf!("Zakres: {} .. +{}. WartoĹ›Ä‡ ujemna obniĹĽa napiÄ™cie rdzenia (jeden punkt to okoĹ‚o 3-5 mV), dodatnia je podnosi.{}", "Range: {} .. +{}. A negative value lowers the core voltage (one point is about 3-5 mV), a positive one raises it.{}",
                curve.min,
                curve.max,
                if curve.set_mailbox.is_some() {
                    crate::i18n::tr(" Na Ryzen 5000 zapis idzie przez skrzynkÄ™ MP1, a odczyt kontrolny przez RSMU.", " On Ryzen 5000 writes go through the MP1 mailbox and the verification read through RSMU.")
                } else {
                    ""
                }
            ),
        );
        apply
    }

    /// Limity PBO. Zwraca akcjÄ™ klikniÄ™tego przycisku.
    fn pbo_section(&mut self, ui: &mut egui::Ui, state: &State) -> Option<Action> {
        let mut action = None;
        let width = (ui.available_width() - 2.0 * MARGIN - 4.0).max(300.0);
        theme::card(14, MARGIN as i8).show(ui, |ui| {
            ui.set_width(width);
            theme::heading(ui, Icon::Power, "Precision Boost Overdrive");
            ui.add_space(4.0);
            let limits = state.limits.as_ref().ok();
            let confirmed = self.confirmed;
            // BieĹĽÄ…ca wartoĹ›Ä‡ z tabeli procesora; "?" gdy odczyt nie jest potwierdzony.
            let current = |ui: &mut egui::Ui, value: Option<f32>, unit: &str, slot: usize| match value {
                Some(v) if confirmed[slot] => {
                    ui.label(RichText::new(format!("{v:.0} {unit}")).size(16.0).color(accent()));
                }
                Some(v) => {
                    ui.label(RichText::new(format!("{v:.0} {unit} ?")).size(16.0).color(warn())).on_hover_text(
                        crate::i18n::tr("UkĹ‚ad tabeli tej wersji nie jest potwierdzony. WartoĹ›Ä‡ zostanie sprawdzona przy pierwszym ustawieniu tego limitu.", "The table layout for this version is not confirmed. The value will be checked the first time you set this limit."),
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
                    ui.label(RichText::new(crate::i18n::tr("Nowa wartoĹ›Ä‡", "New value")).size(12.5).color(accent()));
                    ui.label("");
                    ui.end_row();

                    row_label(ui, crate::i18n::tr("Limit mocy pakietu (PPT)", "Package power limit (PPT)"));
                    current(ui, limits.and_then(|l| l.ppt_w), "W", 0);
                    ui.add_sized(field, egui::DragValue::new(&mut self.ppt_w).range(amd::PPT_MIN_W..=amd::PPT_MAX_W).suffix(" W"));
                    if theme::primary_button(ui, None, crate::i18n::tr("Ustaw PPT", "Set PPT"), self.armed).clicked() {
                        action = Some(Action::Ppt);
                    }
                    ui.end_row();

                    row_label(ui, crate::i18n::tr("Limit prÄ…du ciÄ…gĹ‚ego (TDC)", "Sustained current limit (TDC)"));
                    current(ui, limits.and_then(|l| l.tdc_a), "A", 1);
                    ui.add_sized(field, egui::DragValue::new(&mut self.tdc_a).range(amd::CURRENT_MIN_A..=amd::CURRENT_MAX_A).suffix(" A"));
                    if theme::primary_button(ui, None, crate::i18n::tr("Ustaw TDC", "Set TDC"), self.armed).clicked() {
                        action = Some(Action::Tdc);
                    }
                    ui.end_row();

                    row_label(ui, crate::i18n::tr("Limit prÄ…du szczytowego (EDC)", "Peak current limit (EDC)"));
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
                Ok(limits) => crate::trf!("BieĹĽÄ…ce limity pochodzÄ… z tabeli procesora (wersja 0x{:06X}). Po kaĹĽdym ustawieniu program sprawdza, czy tabela pokazuje zadanÄ… wartoĹ›Ä‡. Znak zapytania oznacza odczyt jeszcze niepotwierdzony zapisem.", "Current limits come from the CPU table (version 0x{:06X}). After each change the program checks that the table shows the requested value. A question mark means the readout has not yet been confirmed by a write.",
                    limits.version
                ),
                Err(e) => crate::trf!("BieĹĽÄ…cych limitĂłw nie udaĹ‚o siÄ™ odczytaÄ‡ ({e}). Limity moĹĽna ustawiaÄ‡, ale bez potwierdzenia odczytem.", "Current limits could not be read ({e}). Limits can still be set, but without read-back confirmation."
                ),
            };
            theme::info(ui, &note);
        });
        action
    }
}
