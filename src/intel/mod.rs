//! Zakładka karty Intel (Arc, Iris Xe, UHD): ustawienia z Intel Graphics
//! Software, wyłącznie przez publiczne API sterownika Intela (IGCL,
//! `ControlLib.dll`):
//!
//! - Ustawienia 3D: synchronizacja klatek, niskie opóźnienie, limit klatek,
//!   frame pacing, generowanie klatek, filtrowanie, wygładzanie, wyostrzanie,
//!   teselacja, gotowe shadery - globalnie albo dla wybranej gry,
//! - ARC SYNC: Intel Arc Sync / Adaptive Sync dla każdego ekranu, Adaptive
//!   Sync Plus, VRR w grach w oknie,
//! - Ekran: skalowanie, retro scaling, wyostrzanie ekranu, oszczędzanie
//!   energii ekranu.
//!
//! IGCL obsługuje grafiki Intel Xe (11. generacja procesorów i nowsze) oraz
//! karty Arc; starsze (np. HD 530) zgłaszają "platforma nieobsługiwana".

pub mod igcl;

use std::collections::HashMap;

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::theme::{self, accent, muted, warn, SwitchState};
use igcl::{arc_sync, describe, feature, power, scaling, value_type, FeatureDetails, Handle, Igcl, Property};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Settings3d,
    ArcSync,
    Display,
}

/// Opis ustawienia 3D: nazwa, pomoc i (dla wyliczeń) wartości.
struct Meta {
    id: u32,
    pl: &'static str,
    en: &'static str,
    help_pl: &'static str,
    help_en: &'static str,
    /// Wartości wyliczenia; `flags` - wartości to bity (synchronizacja klatek).
    opts: &'static [(u32, &'static str, &'static str)],
    flags: bool,
    unit: &'static str,
}

const OFF_ON: &[(u32, &str, &str)] = &[(0, "Wyłączone", "Off"), (1, "Włączone", "On")];

const SETTINGS_3D: &[Meta] = &[
    Meta {
        id: feature::GAMING_FLIP_MODES,
        pl: "Synchronizacja klatek",
        en: "Frame synchronization",
        help_pl: "Jak gra oddaje klatki do ekranu: V-Sync, Smooth Sync (mniej widoczne rwanie), Speed Sync (bez rwania, bez czekania).",
        help_en: "How the game hands frames to the display: V-Sync, Smooth Sync (less visible tearing), Speed Sync (no tearing, no waiting).",
        opts: &[
            (1, "Ustawienie gry", "Application choice"),
            (2, "V-Sync wyłączony", "V-Sync off"),
            (4, "V-Sync włączony", "V-Sync on"),
            (8, "Smooth Sync", "Smooth Sync"),
            (16, "Speed Sync", "Speed Sync"),
            (32, "Limit klatek", "Capped FPS"),
        ],
        flags: true,
        unit: "",
    },
    Meta {
        id: feature::LOW_LATENCY,
        pl: "Tryb niskiego opóźnienia",
        en: "Low latency mode",
        help_pl: "Mniej klatek w kolejce - szybsza reakcja obrazu na mysz. Boost dodatkowo podnosi zegary karty.",
        help_en: "Fewer queued frames - faster response to the mouse. Boost also raises the card's clocks.",
        opts: &[(0, "Wyłączony", "Off"), (1, "Włączony", "On"), (2, "Włączony + Boost", "On + Boost")],
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::FRAME_LIMIT,
        pl: "Limit klatek",
        en: "Frame rate limiter",
        help_pl: "Maksymalna liczba klatek na sekundę ustawiona w sterowniku.",
        help_en: "Maximum frames per second set in the driver.",
        opts: &[],
        flags: false,
        unit: " FPS",
    },
    Meta {
        id: feature::FRAME_PACING,
        pl: "Frame pacing",
        en: "Frame pacing",
        help_pl: "Równe odstępy między klatkami (płynniejszy obraz kosztem odrobiny opóźnienia).",
        help_en: "Even spacing between frames (smoother picture at a little latency).",
        opts: &[
            (0, "Wyłączony", "Off"),
            (1, "Bez wygładzania", "No smoothing"),
            (2, "Maksymalne wygładzanie", "Maximum smoothing"),
            (3, "Gry rywalizacyjne", "Competitive gaming"),
        ],
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::FRAME_GENERATION,
        pl: "Generowanie klatek (XeSS FG)",
        en: "Frame generation (XeSS FG)",
        help_pl: "Nadpisanie generowania klatek XeSS przez sterownik - odpowiednik nadpisań DLSS i FSR.",
        help_en: "Driver override of XeSS frame generation - the counterpart of DLSS and FSR overrides.",
        opts: &[(0, "Ustawienie gry", "Application choice"), (1, "2x", "2x"), (2, "3x", "3x"), (3, "4x", "4x")],
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::ANISOTROPIC,
        pl: "Filtrowanie anizotropowe",
        en: "Anisotropic filtering",
        help_pl: "Ostrzejsze tekstury oglądane pod kątem.",
        help_en: "Sharper textures seen at an angle.",
        opts: &[(0, "Ustawienie gry", "Application choice"), (2, "2x", "2x"), (4, "4x", "4x"), (8, "8x", "8x"), (16, "16x", "16x")],
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::CMAA,
        pl: "Wygładzanie CMAA",
        en: "CMAA anti-aliasing",
        help_pl: "Conservative Morphological Anti-Aliasing - wygładzanie krawędzi jako filtr obrazu.",
        help_en: "Conservative Morphological Anti-Aliasing - edge smoothing as an image filter.",
        opts: &[(0, "Wyłączone", "Off"), (1, "Zamiast MSAA", "Override MSAA"), (2, "Wzmocnij grę", "Enhance application")],
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::MSAA,
        pl: "MSAA",
        en: "MSAA",
        help_pl: "Wygładzanie wielopróbkowe wymuszone przez sterownik.",
        help_en: "Multisample anti-aliasing forced by the driver.",
        opts: &[(0, "Ustawienie gry", "Application choice"), (1, "Wyłączone", "Disabled"), (2, "2x", "2x"), (4, "4x", "4x"), (8, "8x", "8x"), (16, "16x", "16x")],
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::TEXTURE_FILTERING_QUALITY,
        pl: "Jakość filtrowania tekstur",
        en: "Texture filtering quality",
        help_pl: "Wydajność albo jakość próbkowania tekstur.",
        help_en: "Performance or quality of texture sampling.",
        opts: &[(0, "Wydajność", "Performance"), (1, "Zrównoważona", "Balanced"), (2, "Jakość", "Quality")],
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::ADAPTIVE_TESSELLATION,
        pl: "Adaptacyjna teselacja",
        en: "Adaptive tessellation",
        help_pl: "Sterownik zmniejsza poziom teselacji dalekich obiektów.",
        help_en: "The driver lowers tessellation for distant objects.",
        opts: OFF_ON,
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::SHARPENING_FILTER,
        pl: "Filtr wyostrzania",
        en: "Sharpening filter",
        help_pl: "Wyostrza obraz gier (przy XeSS, FSR i TAA).",
        help_en: "Sharpens game images (with XeSS, FSR and TAA).",
        opts: OFF_ON,
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::PREBUILT_SHADER_DOWNLOAD,
        pl: "Pobieranie gotowych shaderów",
        en: "Prebuilt shader download",
        help_pl: "Sterownik pobiera od Intela skompilowane shadery popularnych gier.",
        help_en: "The driver downloads compiled shaders of popular games from Intel.",
        opts: OFF_ON,
        flags: false,
        unit: "",
    },
];

const SYNC_3D: &[Meta] = &[
    Meta {
        id: feature::ADAPTIVE_SYNC_PLUS,
        pl: "Adaptive Sync Plus",
        en: "Adaptive Sync Plus",
        help_pl: "Gdy gra przekracza odświeżanie ekranu - bez rwania i z niskim opóźnieniem.",
        help_en: "When the game exceeds the refresh rate - no tearing and low latency.",
        opts: OFF_ON,
        flags: false,
        unit: "",
    },
    Meta {
        id: feature::VRR_WINDOWED_BLT,
        pl: "VRR w grach w oknie",
        en: "VRR in windowed games",
        help_pl: "Zmienne odświeżanie także w oknie i w oknie bez ramki.",
        help_en: "Variable refresh rate also in windowed and borderless games.",
        opts: &[(0, "Auto", "Auto"), (1, "Włączone", "On"), (2, "Wyłączone", "Off")],
        flags: false,
        unit: "",
    },
];

const ARC_PROFILES: &[(u32, &str, &str)] = &[
    (arc_sync::RECOMMENDED, "Zalecany", "Recommended"),
    (arc_sync::EXCELLENT, "Doskonały", "Excellent"),
    (arc_sync::GOOD, "Dobry", "Good"),
    (arc_sync::COMPATIBLE, "Zgodny", "Compatible"),
    (arc_sync::VESA, "VESA", "VESA"),
    (arc_sync::CUSTOM, "Własny", "Custom"),
    (arc_sync::OFF, "Wyłączony", "Off"),
];

const SCALING: &[(u32, &str, &str)] = &[
    (scaling::ASPECT_RATIO, "Zachowaj proporcje", "Maintain aspect ratio"),
    (scaling::STRETCHED, "Rozciągnij", "Stretch"),
    (scaling::CENTERED, "Wyśrodkuj", "Center"),
    (scaling::IDENTITY, "Ustawienie ekranu", "Display default"),
];

const POWER: &[(u32, &str, &str)] = &[
    (power::PSR, "Panel Self Refresh (PSR)", "Panel Self Refresh (PSR)"),
    (power::DPST, "Oszczędzanie energii wyświetlacza (DPST)", "Display power saving (DPST)"),
    (power::FBC, "Kompresja bufora ramki (FBC)", "Frame buffer compression (FBC)"),
    (power::LRR, "Niższe odświeżanie na baterii (LRR)", "Lower refresh rate on battery (LRR)"),
];

fn lang<'a>(pl: &'a str, en: &'a str) -> &'a str {
    if crate::i18n::english() {
        en
    } else {
        pl
    }
}

/// Wyjście ekranu z tym, co sterownik dla niego obsługuje.
struct Output {
    handle: Handle,
    arc: Option<igcl::ArcSyncMonitor>,
    scaling: Option<u32>,
    sharpness: Vec<igcl::SharpnessFilter>,
    power: Option<u32>,
}

pub struct Intel {
    igcl: Option<Result<Igcl, String>>,
    device: usize,
    caps: Option<Result<Vec<FeatureDetails>, String>>,
    /// Profil gry (nazwa .exe); pusty - ustawienia globalne.
    app: String,
    app_applied: String,
    values: HashMap<(u32, String), Result<Property, u32>>,
    outputs: Option<Vec<Output>>,
    status: Option<(String, bool)>,
    /// Edytowany własny profil Arc Sync, po wyjściu.
    custom: HashMap<usize, igcl::ArcSyncProfile>,
}

impl Default for Intel {
    fn default() -> Self {
        Self::new()
    }
}

impl Intel {
    pub fn new() -> Self {
        Self {
            igcl: None,
            device: 0,
            caps: None,
            app: String::new(),
            app_applied: String::new(),
            values: HashMap::new(),
            outputs: None,
            status: None,
            custom: HashMap::new(),
        }
    }

    fn igcl(&mut self) -> Result<&Igcl, String> {
        if self.igcl.is_none() {
            self.igcl = Some(Igcl::load());
        }
        match self.igcl.as_ref().unwrap() {
            Ok(i) => Ok(i),
            Err(e) => Err(e.clone()),
        }
    }

    /// Wersja sterownika Intela (do śledzenia zmian w Shader compilation).
    pub fn driver_version(&mut self) -> Option<String> {
        let i = self.igcl().ok()?;
        i.devices.first().map(|d| d.driver.clone())
    }

    fn reload(&mut self) {
        self.caps = None;
        self.values.clear();
        self.outputs = None;
        self.custom.clear();
    }

    fn handle(&self) -> Option<Handle> {
        let i = self.igcl.as_ref()?.as_ref().ok()?;
        i.devices.get(self.device).map(|d| d.handle)
    }

    fn report<T>(&mut self, name: &str, r: Result<T, u32>) {
        self.status = Some(match r {
            Ok(_) => (crate::trf!("{name}: zmieniono.", "{name}: changed."), false),
            Err(e) => (format!("{name}: {}", describe(e)), true),
        });
    }

    /// Nagłówek: karta, wersja sterownika, ponowny odczyt; `false`, gdy IGCL
    /// jest niedostępny.
    fn header(&mut self, ui: &mut egui::Ui, intro_pl: &'static str, intro_en: &'static str) -> bool {
        match self.igcl() {
            Ok(i) => {
                let names: Vec<(String, String)> = i.devices.iter().map(|d| (d.name.clone(), d.driver.clone())).collect();
                if names.is_empty() {
                    ui.label(RichText::new(tr("API Intela nie widzi karty Intel.", "The Intel API sees no Intel card.")).color(warn()));
                    return false;
                }
                self.device = self.device.min(names.len() - 1);
                ui.horizontal_wrapped(|ui| {
                    if names.len() > 1 {
                        for (i, (n, _)) in names.iter().enumerate() {
                            if ui.selectable_label(self.device == i, n).clicked() && self.device != i {
                                self.device = i;
                                self.reload();
                            }
                        }
                    } else {
                        ui.label(RichText::new(&names[0].0).strong().color(accent()));
                    }
                    ui.label(RichText::new(crate::trf!("sterownik {}", "driver {}", names[self.device].1)).color(muted()));
                    if ui.button(tr("Odczytaj ponownie", "Read again")).clicked() {
                        self.reload();
                        self.status = None;
                    }
                });
                ui.label(RichText::new(tr(intro_pl, intro_en)).small().color(muted()));
                ui.add_space(8.0);
                true
            }
            Err(e) => {
                ui.label(
                    RichText::new(tr(
                        "Panel Intel niedostępny. API Intela (IGCL) obsługuje grafiki Intel Xe (procesory 11. generacji i nowsze) oraz karty Arc.",
                        "Intel panel unavailable. The Intel API (IGCL) supports Intel Xe graphics (11th-gen processors and newer) and Arc cards.",
                    ))
                    .strong(),
                );
                ui.label(RichText::new(e).color(warn()));
                if ui.button(tr("Spróbuj ponownie", "Try again")).clicked() {
                    self.igcl = None;
                }
                false
            }
        }
    }

    fn caps(&mut self) -> Vec<FeatureDetails> {
        if self.caps.is_none() {
            let caps = match (self.igcl.as_ref().and_then(|r| r.as_ref().ok()), self.handle()) {
                (Some(i), Some(h)) => i.features(h),
                _ => Err(String::new()),
            };
            self.caps = Some(caps);
        }
        self.caps.as_ref().and_then(|c| c.as_ref().ok()).cloned().unwrap_or_default()
    }

    fn value(&mut self, f: &FeatureDetails, app: &str) -> Result<Property, u32> {
        let key = (f.feature, app.to_owned());
        if !self.values.contains_key(&key) {
            let r = match (self.igcl.as_ref().and_then(|r| r.as_ref().ok()), self.handle()) {
                (Some(i), Some(h)) => i.feature(h, f.feature, f.value_type, (!app.is_empty()).then_some(app), None),
                _ => Err(igcl::CTL_RESULT_ERROR_UNSUPPORTED_FEATURE),
            };
            self.values.insert(key.clone(), r);
        }
        self.values[&key]
    }

    fn set_feature(&mut self, f: &FeatureDetails, app: &str, name: &str, p: Property) {
        let r = match (self.igcl.as_ref().and_then(|r| r.as_ref().ok()), self.handle()) {
            (Some(i), Some(h)) => i.feature(h, f.feature, f.value_type, (!app.is_empty()).then_some(app), Some(p)),
            _ => Err(igcl::CTL_RESULT_ERROR_UNSUPPORTED_FEATURE),
        };
        self.values.remove(&(f.feature, app.to_owned()));
        self.report(name, r);
    }

    /// Ustawienia 3D z listy `list`, które karta obsługuje.
    fn features_ui(&mut self, ui: &mut egui::Ui, list: &'static [Meta]) {
        let caps = self.caps();
        if let Some(Err(e)) = &self.caps {
            if !e.is_empty() {
                ui.label(RichText::new(crate::trf!("Ustawienia 3D: {e}", "3D settings: {e}")).color(warn()));
            }
        }
        let mut shown = 0;
        for meta in list {
            let Some(f) = caps.iter().find(|c| c.feature == meta.id).copied() else {
                continue;
            };
            shown += 1;
            // Profil gry tylko dla ustawień, które go obsługują.
            let app = if f.per_app { self.app_applied.clone() } else { String::new() };
            self.feature_ui(ui, meta, &f, &app);
            ui.add_space(8.0);
        }
        if shown == 0 && caps.is_empty() {
            ui.label(RichText::new(tr("Sterownik nie podał ustawień 3D dla tej karty.", "The driver reported no 3D settings for this card.")).color(muted()));
        }
    }

    fn feature_ui(&mut self, ui: &mut egui::Ui, meta: &'static Meta, f: &FeatureDetails, app: &str) {
        let name = lang(meta.pl, meta.en);
        let current = self.value(f, app);
        let mut set: Option<Property> = None;
        let width = (ui.available_width() - 30.0).max(300.0);
        theme::card(10, 12).show(ui, |ui| {
            ui.set_width(width);
            ui.label(RichText::new(name).strong().size(15.0).color(theme::text()));
            ui.label(RichText::new(lang(meta.help_pl, meta.help_en)).small().color(muted()));
            if !app.is_empty() {
                ui.label(RichText::new(crate::trf!("Profil gry: {app}", "Game profile: {app}")).small().color(accent()));
            }
            let p = match current {
                Ok(p) => p,
                Err(e) => {
                    ui.label(RichText::new(describe(e)).color(muted()));
                    return;
                }
            };
            ui.add_space(4.0);
            match f.value_type {
                value_type::ENUM => {
                    let (mask, _) = f.info.enum_info();
                    let supported = |v: u32| {
                        if meta.flags {
                            mask == 0 || mask & v as u64 != 0
                        } else {
                            mask == 0 || v >= 64 || (mask >> v) & 1 != 0
                        }
                    };
                    ui.horizontal_wrapped(|ui| {
                        for (v, pl, en) in meta.opts.iter().filter(|o| supported(o.0)) {
                            let selected = p.enum_type() == *v;
                            if ui.selectable_label(selected, lang(pl, en)).clicked() && !selected {
                                set = Some(Property { head: *v, value: 0 });
                            }
                        }
                    });
                }
                value_type::BOOL => {
                    let on = p.enabled();
                    ui.horizontal(|ui| {
                        if theme::switch(ui, &format!("intel_{}", meta.id), if on { SwitchState::On } else { SwitchState::Off }, true).clicked() {
                            set = Some(Property { head: !on as u32, value: 0 });
                        }
                        ui.label(if on { tr("Włączone", "On") } else { tr("Wyłączone", "Off") });
                    });
                }
                value_type::INT32 | value_type::UINT32 | value_type::FLOAT => {
                    let on = p.enabled();
                    let float = f.value_type == value_type::FLOAT;
                    let (min, max, step) = if float {
                        let (a, b, s, _) = f.info.float_range();
                        (a as i32, b as i32, s.max(1.0) as i32)
                    } else {
                        let (a, b, s, _) = f.info.int_range();
                        (a, b, s.max(1))
                    };
                    let (min, max) = if max > min { (min, max) } else { (20, 300) };
                    let current = if float { p.float() as i32 } else { p.int() };
                    let id = ui.scope_id().with(("intel_value", meta.id));
                    let mut value = ui.data(|d| d.get_temp::<i32>(id)).unwrap_or(current).clamp(min, max);
                    ui.horizontal(|ui| {
                        if theme::switch(ui, &format!("intel_{}", meta.id), if on { SwitchState::On } else { SwitchState::Off }, true).clicked() {
                            let v = if float { (value as f32).to_bits() } else { value as u32 };
                            set = Some(Property { head: !on as u32, value: v });
                        }
                        let slider = ui.add_enabled(
                            on,
                            egui::Slider::new(&mut value, min..=max).step_by(step as f64).suffix(meta.unit),
                        );
                        ui.data_mut(|d| d.insert_temp(id, value));
                        if (slider.drag_stopped() || (slider.changed() && !slider.dragged())) && value != current {
                            let v = if float { (value as f32).to_bits() } else { value as u32 };
                            set = Some(Property { head: 1, value: v });
                        }
                    });
                }
                _ => {
                    ui.label(RichText::new(tr("Ustawienie dostępne w Intel Graphics Software.", "Setting available in Intel Graphics Software.")).color(muted()));
                }
            }
        });
        if let Some(p) = set {
            self.set_feature(f, app, name, p);
        }
    }

    fn outputs(&mut self) {
        if self.outputs.is_some() {
            return;
        }
        let list = match (self.igcl.as_ref().and_then(|r| r.as_ref().ok()), self.handle()) {
            (Some(i), Some(h)) => i
                .displays(h)
                .into_iter()
                .map(|d| Output {
                    handle: d,
                    arc: i.arc_sync_info(d).ok(),
                    scaling: i.scaling_caps(d).ok().filter(|c| *c != 0),
                    sharpness: i.sharpness_caps(d).unwrap_or_default(),
                    power: i.power_caps(d).ok().filter(|c| *c != 0),
                })
                // Niepodłączone wyjścia nie odpowiadają na nic.
                .filter(|o| o.arc.is_some() || o.scaling.is_some() || !o.sharpness.is_empty() || o.power.is_some())
                .collect(),
            _ => Vec::new(),
        };
        self.outputs = Some(list);
    }

    fn arc_sync_ui(&mut self, ui: &mut egui::Ui) {
        self.outputs();
        let outputs: Vec<(usize, Handle, igcl::ArcSyncMonitor)> = self
            .outputs
            .iter()
            .flatten()
            .enumerate()
            .filter_map(|(i, o)| o.arc.map(|a| (i, o.handle, a)))
            .collect();
        if outputs.is_empty() {
            ui.label(RichText::new(tr(
                "Żaden ekran podłączony do karty Intel nie zgłasza Adaptive Sync / Intel Arc Sync.",
                "No display connected to the Intel card reports Adaptive Sync / Intel Arc Sync.",
            ))
            .color(muted()));
        }
        for (i, handle, info) in outputs {
            theme::heading(ui, crate::icons::Icon::Gpu, &format!("{} {}", tr("Ekran", "Display"), i + 1));
            let width = (ui.available_width() - 30.0).max(300.0);
            let profile = self.igcl.as_ref().and_then(|r| r.as_ref().ok()).and_then(|g| g.arc_sync_profile(handle).ok());
            let mut set: Option<igcl::ArcSyncProfile> = None;
            theme::card(10, 12).show(ui, |ui| {
                ui.set_width(width);
                ui.label(RichText::new("Intel Arc Sync / Adaptive Sync").strong().size(15.0).color(theme::text()));
                if !info.supported {
                    ui.label(RichText::new(tr("Ten ekran nie obsługuje zmiennego odświeżania.", "This display does not support variable refresh rate.")).color(muted()));
                    return;
                }
                ui.label(
                    RichText::new(crate::trf!(
                        "Zakres ekranu: {:.0}-{:.0} Hz. Odświeżanie podąża za liczbą klatek gry - bez rwania obrazu.",
                        "Display range: {:.0}-{:.0} Hz. The refresh rate follows the game's frame rate - no tearing.",
                        info.min_hz,
                        info.max_hz
                    ))
                    .small()
                    .color(muted()),
                );
                let Some(p) = profile else {
                    return;
                };
                ui.horizontal_wrapped(|ui| {
                    for (v, pl, en) in ARC_PROFILES {
                        let selected = p.profile == *v;
                        if ui.selectable_label(selected, lang(pl, en)).clicked() && !selected {
                            let mut n = p;
                            n.profile = *v;
                            if *v == arc_sync::CUSTOM && n.max_hz <= 0.0 {
                                n.min_hz = info.min_hz;
                                n.max_hz = info.max_hz;
                                n.max_increase_us = info.max_increase_us;
                                n.max_decrease_us = info.max_decrease_us;
                            }
                            set = Some(n);
                        }
                    }
                });
                if p.profile == arc_sync::CUSTOM {
                    let c = self.custom.entry(i).or_insert(p);
                    egui::Grid::new(("intel_arc_custom", i)).num_columns(2).show(ui, |ui| {
                        ui.label(tr("Min. odświeżanie", "Min refresh"));
                        ui.add(egui::Slider::new(&mut c.min_hz, info.min_hz.max(1.0)..=info.max_hz).suffix(" Hz"));
                        ui.end_row();
                        ui.label(tr("Max. odświeżanie", "Max refresh"));
                        ui.add(egui::Slider::new(&mut c.max_hz, info.min_hz.max(1.0)..=info.max_hz).suffix(" Hz"));
                        ui.end_row();
                        ui.label(tr("Zmiana w górę", "Frame time increase"));
                        ui.add(egui::Slider::new(&mut c.max_increase_us, 0..=info.max_increase_us.max(10_000)).suffix(" µs"));
                        ui.end_row();
                        ui.label(tr("Zmiana w dół", "Frame time decrease"));
                        ui.add(egui::Slider::new(&mut c.max_decrease_us, 0..=info.max_decrease_us.max(10_000)).suffix(" µs"));
                        ui.end_row();
                    });
                    if ui.button(tr("Zastosuj własny profil", "Apply custom profile")).clicked() {
                        let mut n = *c;
                        n.profile = arc_sync::CUSTOM;
                        n.min_hz = n.min_hz.min(n.max_hz);
                        set = Some(n);
                    }
                }
            });
            if let Some(p) = set {
                let r = match self.igcl.as_ref().and_then(|r| r.as_ref().ok()) {
                    Some(g) => g.set_arc_sync_profile(handle, p),
                    None => Err(igcl::CTL_RESULT_ERROR_UNSUPPORTED_FEATURE),
                };
                self.custom.remove(&i);
                self.report("Intel Arc Sync", r);
            }
            ui.add_space(10.0);
        }
        self.features_ui(ui, SYNC_3D);
    }

    fn display_ui(&mut self, ui: &mut egui::Ui) {
        // Retro scaling - ustawienie karty.
        let retro = match (self.igcl.as_ref().and_then(|r| r.as_ref().ok()), self.handle()) {
            (Some(g), Some(h)) => g.retro_scaling(h).ok().filter(|(caps, ..)| *caps != 0),
            _ => None,
        };
        let width = (ui.available_width() - 30.0).max(300.0);
        if let Some((caps, on, kind)) = retro {
            let mut set: Option<(bool, u32)> = None;
            theme::card(10, 12).show(ui, |ui| {
                ui.set_width(width);
                ui.label(RichText::new("Retro scaling").strong().size(15.0).color(theme::text()));
                ui.label(RichText::new(tr("Ostre piksele w starych i pikselowych grach.", "Sharp pixels in old and pixel-art games.")).small().color(muted()));
                ui.horizontal_wrapped(|ui| {
                    if theme::switch(ui, "intel_retro", if on { SwitchState::On } else { SwitchState::Off }, true).clicked() {
                        let k = if kind & caps != 0 { kind } else { caps & caps.wrapping_neg() };
                        set = Some((!on, k));
                    }
                    for (v, pl, en) in [
                        (scaling::RETRO_INTEGER, "Całkowita wielokrotność", "Integer scaling"),
                        (scaling::RETRO_NEAREST_NEIGHBOUR, "Najbliższy sąsiad", "Nearest neighbour"),
                    ] {
                        if caps & v != 0 && ui.add_enabled(on, egui::Button::selectable(kind == v, lang(pl, en))).clicked() && kind != v {
                            set = Some((true, v));
                        }
                    }
                });
            });
            if let Some((on, kind)) = set {
                let r = match (self.igcl.as_ref().and_then(|r| r.as_ref().ok()), self.handle()) {
                    (Some(g), Some(h)) => g.set_retro_scaling(h, on, kind),
                    _ => Err(igcl::CTL_RESULT_ERROR_UNSUPPORTED_FEATURE),
                };
                self.report("Retro scaling", r);
            }
            ui.add_space(10.0);
        }

        self.outputs();
        let count = self.outputs.as_ref().map_or(0, |o| o.len());
        if count == 0 {
            ui.label(RichText::new(tr("Nie znaleziono ekranu podłączonego do karty Intel.", "No display connected to the Intel card was found.")).color(muted()));
        }
        for i in 0..count {
            let (handle, scaling_caps, filters, power_caps) = {
                let o = &self.outputs.as_ref().unwrap()[i];
                (o.handle, o.scaling, o.sharpness.clone(), o.power)
            };
            theme::heading(ui, crate::icons::Icon::Gpu, &format!("{} {}", tr("Ekran", "Display"), i + 1));
            let Some(g) = self.igcl.as_ref().and_then(|r| r.as_ref().ok()) else {
                return;
            };
            let current_scaling = scaling_caps.and_then(|_| g.scaling(handle).ok());
            let current_sharp = (!filters.is_empty()).then(|| g.sharpness(handle).ok()).flatten();
            let powers: Vec<(u32, bool)> = power_caps
                .map(|c| POWER.iter().filter(|p| c & p.0 != 0).filter_map(|p| g.power(handle, p.0).ok().map(|on| (p.0, on))).collect())
                .unwrap_or_default();
            let mut action: Option<Box<dyn FnOnce(&Igcl) -> (&'static str, Result<(), u32>)>> = None;
            theme::card(10, 12).show(ui, |ui| {
                ui.set_width(width);
                if let (Some(caps), Some(s)) = (scaling_caps, current_scaling) {
                    ui.label(RichText::new(tr("Skalowanie", "Scaling")).strong().color(theme::text()));
                    ui.horizontal_wrapped(|ui| {
                        for (v, pl, en) in SCALING.iter().filter(|o| caps & o.0 != 0) {
                            let selected = s.kind == *v;
                            if ui.selectable_label(selected, lang(pl, en)).clicked() && !selected {
                                let v = *v;
                                action = Some(Box::new(move |g: &Igcl| (tr("Skalowanie", "Scaling"), g.set_scaling(handle, v))));
                            }
                        }
                    });
                    ui.add_space(6.0);
                }
                if let Some(s) = current_sharp {
                    ui.label(RichText::new(tr("Wyostrzanie ekranu", "Display sharpness")).strong().color(theme::text()));
                    let filter = filters.iter().find(|f| f.kind == s.kind).or(filters.first()).copied().unwrap_or_default();
                    let id = ui.scope_id().with(("intel_sharp", i));
                    let mut value = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(s.intensity);
                    let (min, max) = if filter.max > filter.min { (filter.min, filter.max) } else { (0.0, 100.0) };
                    ui.horizontal(|ui| {
                        if theme::switch(ui, &format!("intel_sharp_{i}"), if s.enable { SwitchState::On } else { SwitchState::Off }, true).clicked() {
                            let (on, kind, v) = (!s.enable, filter.kind, value);
                            action = Some(Box::new(move |g: &Igcl| (tr("Wyostrzanie ekranu", "Display sharpness"), g.set_sharpness(handle, on, kind, v))));
                        }
                        let slider = ui.add_enabled(s.enable, egui::Slider::new(&mut value, min..=max));
                        ui.data_mut(|d| d.insert_temp(id, value));
                        if (slider.drag_stopped() || (slider.changed() && !slider.dragged())) && value != s.intensity {
                            let (kind, v) = (filter.kind, value);
                            action = Some(Box::new(move |g: &Igcl| (tr("Wyostrzanie ekranu", "Display sharpness"), g.set_sharpness(handle, true, kind, v))));
                        }
                    });
                    ui.add_space(6.0);
                }
                if !powers.is_empty() {
                    ui.label(RichText::new(tr("Oszczędzanie energii ekranu", "Display power saving")).strong().color(theme::text()));
                    for (flag, on) in &powers {
                        let (_, pl, en) = POWER.iter().find(|p| p.0 == *flag).copied().unwrap();
                        ui.horizontal(|ui| {
                            if theme::switch(ui, &format!("intel_power_{i}_{flag}"), if *on { SwitchState::On } else { SwitchState::Off }, true).clicked() {
                                let (flag, on) = (*flag, !*on);
                                action = Some(Box::new(move |g: &Igcl| (tr("Oszczędzanie energii", "Power saving"), g.set_power(handle, flag, on))));
                            }
                            ui.label(lang(pl, en));
                        });
                    }
                }
                if current_scaling.is_none() && current_sharp.is_none() && powers.is_empty() {
                    ui.label(RichText::new(tr("Sterownik nie udostępnia ustawień tego ekranu.", "The driver exposes no settings for this display.")).color(muted()));
                }
            });
            if let Some(a) = action {
                let (name, r) = a(g);
                self.report(name, r);
            }
            ui.add_space(10.0);
        }
    }

    fn status_ui(&self, ui: &mut egui::Ui) {
        if let Some((text, error)) = &self.status {
            ui.add_space(4.0);
            ui.label(RichText::new(text).color(if *error { warn() } else { accent() }));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, page: Page) {
        match page {
            Page::Settings3d => {
                if self.header(
                    ui,
                    "Ustawienia grafiki z Intel Graphics Software - globalne albo dla wybranej gry. Działają od następnego uruchomienia gry.",
                    "Graphics settings from Intel Graphics Software - global or for a chosen game. They take effect the next time the game starts.",
                ) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr("Profil gry (plik .exe, puste = globalne):", "Game profile (.exe file, empty = global):"));
                        ui.add(egui::TextEdit::singleline(&mut self.app).hint_text("Cyberpunk2077.exe").desired_width(220.0));
                        if ui.button(tr("Pokaż", "Show")).clicked() {
                            self.app_applied = self.app.trim().to_owned();
                        }
                        if !self.app_applied.is_empty() && ui.button(tr("Globalne", "Global")).clicked() {
                            self.app.clear();
                            self.app_applied.clear();
                        }
                    });
                    ui.add_space(6.0);
                    self.features_ui(ui, SETTINGS_3D);
                }
            }
            Page::ArcSync => {
                if self.header(
                    ui,
                    "Zmienne odświeżanie dla ekranów podłączonych do karty Intel - odpowiednik G-SYNC i FreeSync.",
                    "Variable refresh rate for displays connected to the Intel card - the counterpart of G-SYNC and FreeSync.",
                ) {
                    self.arc_sync_ui(ui);
                }
            }
            Page::Display => {
                if self.header(
                    ui,
                    "Skalowanie, wyostrzanie i oszczędzanie energii ekranu ze sterownika Intela.",
                    "Scaling, sharpness and display power saving from the Intel driver.",
                ) {
                    self.display_ui(ui);
                }
            }
        }
        self.status_ui(ui);
    }
}
