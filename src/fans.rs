//! Zakładka "Wentylatory": profile i karty wentylatorów z animacją.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2,
};

use crate::bridge::{Backend, Fan, Snapshot, Temp};
use crate::config::{Config, FanConfig, Mode, MIN_PCT};
use crate::curve;
use crate::sensors::key_temps;
use crate::theme::{self, accent, bg, card_edge, muted, track, warn, well};

const TEMP_MIN: f32 = 20.0;
const TEMP_MAX: f32 = 100.0;

pub struct FansTab {
    pub config: Config,
    /// Ustawienia zmienione w tej klatce - trzeba je przekazać do sterowania.
    dirty: bool,
    /// Ustawienia czekające na zapis do pliku.
    unsaved: bool,
    last_save: Instant,
    angles: HashMap<String, f32>,
    new_profile: String,
    /// Obserwator procesów gier (profile wentylatorów dla gier).
    watcher: Option<crate::game_watch::Watcher>,
    /// Gra, której profil jest teraz zastosowany.
    game_active: Option<crate::game_watch::Active>,
    /// Gry wykryte w launcherach (nazwa, folder); `None` = jeszcze szukamy.
    games: std::sync::Arc<std::sync::Mutex<Option<Vec<(String, String)>>>>,
    games_started: bool,
    /// Karta "Profile gier" rozwinięta (domyślnie zwinięta, żeby nie zasłaniać wentylatorów).
    games_open: bool,
    /// Pokazuj także złącza, na których nic się nie kręci.
    show_idle: bool,
}

/// Złącze wygląda na puste: brak odczytu obrotów albo zero. Wentylatory kart
/// graficznych stoją przy niskiej temperaturze, więc ich to nie dotyczy.
fn looks_unused(fan: &Fan) -> bool {
    !fan.kind.starts_with("Gpu") && fan.rpm.is_none_or(|rpm| rpm < 1.0)
}

impl FansTab {
    pub fn new() -> Self {
        Self {
            config: Config::load(),
            dirty: false,
            unsaved: false,
            last_save: Instant::now(),
            angles: HashMap::new(),
            new_profile: String::new(),
            watcher: None,
            game_active: None,
            games: Default::default(),
            games_started: false,
            games_open: false,
            show_idle: false,
        }
    }

    /// Reguły dla obserwatora: gry z przypisanym istniejącym profilem.
    fn rules(&self) -> Vec<crate::game_watch::Rule> {
        self.config
            .game_profiles
            .iter()
            .filter(|g| self.config.profiles.contains_key(&g.profile))
            .map(|g| crate::game_watch::Rule {
                path: g.path.clone(),
                name: g.name.clone(),
                profile: g.profile.clone(),
            })
            .collect()
    }

    /// Uruchamia obserwator gier (raz, przy starcie programu).
    pub fn start_games(&mut self, wake: impl Fn() + Send + 'static) {
        self.watcher = Some(crate::game_watch::Watcher::start(self.rules(), wake));
    }

    /// Przełącza profil, gdy gra z przypisanym profilem startuje albo się kończy.
    fn apply_game(&mut self) {
        let Some(w) = &self.watcher else { return };
        let now = w.active();
        if now == self.game_active {
            return;
        }
        let profile = match &now {
            Some(a) if self.config.profiles.contains_key(&a.profile) => a.profile.clone(),
            _ => self.config.global_profile.clone(),
        };
        if self.config.active != profile {
            self.config.active = profile;
            self.dirty = true;
        }
        self.game_active = now;
    }

    /// Ustawia profil gry (`None` = profil globalny) i odświeża reguły.
    fn set_game_profile(&mut self, path: &str, name: &str, profile: Option<String>) {
        let list = &mut self.config.game_profiles;
        match profile {
            Some(p) => match list.iter_mut().find(|g| g.path == path) {
                Some(g) => g.profile = p,
                None => list.push(crate::config::GameProfile {
                    path: path.to_owned(),
                    name: name.to_owned(),
                    profile: p,
                }),
            },
            // Gry dodane ręcznie (.exe) zostają na liście z profilem globalnym.
            None => match list.iter_mut().find(|g| g.path == path) {
                Some(g) if g.path.to_lowercase().ends_with(".exe") => g.profile = String::new(),
                _ => list.retain(|g| g.path != path),
            },
        }
        if let Some(w) = &self.watcher {
            w.set_rules(self.rules());
        }
        self.unsaved = true;
        // Zmiana dotyczy gry, która właśnie działa - przeliczymy profil.
        self.game_active = None;
    }

    /// Przekazuje zmiany do sterowania i zapisuje je (z opóźnieniem) do pliku.
    /// Wywoływane w każdej klatce, niezależnie od otwartej zakładki.
    pub fn tick(&mut self, backend: Option<&Backend>, gpu_fans: Option<&crate::gpu_fans::GpuFans>) {
        self.apply_game();
        if self.dirty {
            if let Some(backend) = backend {
                backend.set_fans(self.config.fans().clone());
            }
            if let Some(gpu_fans) = gpu_fans {
                gpu_fans.set_config(self.config.fans().clone());
            }
            self.dirty = false;
            self.unsaved = true;
        }
        if self.unsaved && self.last_save.elapsed() >= Duration::from_millis(500) {
            self.config.save();
            self.unsaved = false;
            self.last_save = Instant::now();
        }
    }

    /// Profil globalny i informacja, który profil działa teraz.
    fn global_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("Profil globalny:", "Global profile:"))
                    .color(muted()),
            );
            let names: Vec<String> = self.config.profiles.keys().cloned().collect();
            let mut global = self.config.global_profile.clone();
            egui::ComboBox::from_id_salt("global_profile")
                .width(170.0)
                .selected_text(global.clone())
                .show_ui(ui, |ui| {
                    for name in &names {
                        ui.selectable_value(&mut global, name.clone(), name.as_str());
                    }
                });
            if global != self.config.global_profile {
                self.config.global_profile = global.clone();
                if self.game_active.is_none() {
                    self.config.active = global;
                    self.dirty = true;
                }
                self.unsaved = true;
            }
            theme::vsep(ui, 20.0);
            ui.label(
                RichText::new(crate::i18n::tr("Aktywny teraz:", "Active now:")).color(muted()),
            );
            let text = match &self.game_active {
                Some(g) if g.profile == self.config.active => crate::trf!(
                    "{} - z gry {}",
                    "{} - from the game {}",
                    self.config.active,
                    g.name
                ),
                _ => self.config.active.clone(),
            };
            crate::history::badge(ui, &text, accent());
            if self.game_active.is_some() {
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "po zamknięciu gry wróci profil globalny",
                        "the global profile returns when the game closes",
                    ))
                    .size(12.0)
                    .color(muted()),
                );
            }
        });
    }

    /// Lista gier z wyborem profilu.
    fn games_card(&mut self, ui: &mut egui::Ui) {
        if !self.games_started {
            self.games_started = true;
            let slot = self.games.clone();
            std::thread::spawn(move || {
                let list: Vec<(String, String)> = crate::shaders::games::find_all()
                    .into_iter()
                    .map(|g| (g.name, g.dir.display().to_string()))
                    .collect();
                *slot.lock().unwrap() = Some(list);
            });
        }
        let found = self.games.lock().ok().and_then(|g| g.clone());
        // Wykryte gry + gry dodane ręcznie (pliki .exe).
        let mut rows: Vec<(String, String)> = found.clone().unwrap_or_default();
        for g in &self.config.game_profiles {
            if !rows.iter().any(|(_, p)| p == &g.path) {
                rows.push((g.name.clone(), g.path.clone()));
            }
        }
        let names: Vec<String> = self.config.profiles.keys().cloned().collect();
        let global = crate::i18n::tr("globalny", "global").to_owned();
        let running = self.game_active.as_ref().map(|a| a.name.clone());
        let mut change: Option<(String, String, Option<String>)> = None;
        let mut remove: Option<String> = None;

        let assigned = self
            .config
            .game_profiles
            .iter()
            .filter(|g| !g.profile.is_empty())
            .count();
        let mut open = self.games_open;
        let width = ui.available_width() - 2.0 * 14.0 - 4.0;
        theme::card(10, 14).show(ui, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                theme::subheading(ui, crate::icons::Icon::Fan, crate::i18n::tr("Profile gier", "Game profiles"));
                let text = match &found {
                    None => crate::i18n::tr("szukanie gier...", "looking for games...").to_owned(),
                    Some(l) => crate::trf!(
                        "wykryte gry: {} (Steam, Epic, GOG, Ubisoft, EA, Xbox), z własnym profilem: {}",
                        "detected games: {} (Steam, Epic, GOG, Ubisoft, EA, Xbox), with their own profile: {}",
                        l.len(),
                        assigned
                    ),
                };
                ui.label(RichText::new(text).size(12.0).color(muted()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if open {
                        crate::i18n::tr("Zwiń", "Collapse")
                    } else {
                        crate::i18n::tr("Rozwiń", "Expand")
                    };
                    if ui.button(label).clicked() {
                        open = !open;
                    }
                });
            });
            if !open {
                return;
            }
            theme::info(
                ui,
                crate::i18n::tr(
                    "Gdy gra działa, wentylatory przechodzą na jej profil (gra rozpoznawana po folderze, sprawdzane co 2 s). Po zamknięciu gry wraca profil globalny. Profile tworzysz poniżej: \"Zapisz jako nowy\".",
                    "While a game runs, the fans switch to its profile (the game is recognized by its folder, checked every 2 s). When the game closes, the global profile returns. Create profiles below with \"Save as new\".",
                ),
            );
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("game_profiles")
                .max_height(220.0)
                .show(ui, |ui| {
                    egui::Grid::new("games").num_columns(4).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
                        for (name, path) in &rows {
                            ui.label(RichText::new(name).color(theme::text()));
                            ui.allocate_ui_with_layout(egui::vec2(420.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.add(egui::Label::new(RichText::new(path).size(11.5).color(muted())).truncate()).on_hover_text(path); });
                            if running.as_deref() == Some(name.as_str()) {
                                crate::history::badge(ui, crate::i18n::tr("URUCHOMIONA", "RUNNING"), theme::ok());
                            } else {
                                ui.allocate_exact_size(egui::vec2(96.0, 18.0), Sense::hover());
                            }
                            let current = self
                                .config
                                .game_profiles
                                .iter()
                                .find(|g| &g.path == path)
                                .map(|g| g.profile.clone())
                                .filter(|p| !p.is_empty());
                            let mut chosen = current.clone();
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt(("game", path))
                                    .width(150.0)
                                    .selected_text(chosen.clone().unwrap_or_else(|| global.clone()))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut chosen, None, global.as_str());
                                        for n in &names {
                                            ui.selectable_value(&mut chosen, Some(n.clone()), n.as_str());
                                        }
                                    });
                                if path.to_lowercase().ends_with(".exe")
                                    && ui.small_button(crate::i18n::tr("Usuń", "Remove")).clicked()
                                {
                                    remove = Some(path.clone());
                                }
                            });
                            if chosen != current {
                                change = Some((path.clone(), name.clone(), chosen));
                            }
                            ui.end_row();
                        }
                    });
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, None, crate::i18n::tr("+ Dodaj grę (.exe)", "+ Add game (.exe)"), true).clicked() {
                    if let Some(file) = rfd::FileDialog::new().add_filter("exe", &["exe"]).pick_file() {
                        let name = file
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let first = names.first().cloned();
                        change = Some((file.display().to_string(), name, first));
                    }
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "dla gier spoza wykrytych launcherów",
                        "for games outside the detected launchers",
                    ))
                    .size(12.0)
                    .color(muted()),
                );
            });
        });
        self.games_open = open;
        if let Some((path, name, profile)) = change {
            self.set_game_profile(&path, &name, profile);
        }
        if let Some(path) = remove {
            self.config.game_profiles.retain(|g| g.path != path);
            if let Some(w) = &self.watcher {
                w.set_rules(self.rules());
            }
            self.unsaved = true;
            self.game_active = None;
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("Edytowany profil:", "Edited profile:"))
                    .color(muted()),
            );

            let names: Vec<String> = self.config.profiles.keys().cloned().collect();
            let mut selected = self.config.active.clone();
            egui::ComboBox::from_id_salt("profile")
                .width(170.0)
                .selected_text(selected.clone())
                .show_ui(ui, |ui| {
                    for name in &names {
                        ui.selectable_value(&mut selected, name.clone(), name.as_str());
                    }
                });
            if selected != self.config.active {
                self.config.active = selected;
                self.dirty = true;
            }

            ui.add(
                egui::TextEdit::singleline(&mut self.new_profile)
                    .hint_text(crate::i18n::tr("Nazwa nowego profilu", "New profile name"))
                    .desired_width(170.0),
            );
            let name = self.new_profile.trim().to_owned();
            if ui
                .add_enabled(
                    !name.is_empty(),
                    egui::Button::new(crate::i18n::tr("Zapisz jako nowy", "Save as new")),
                )
                .clicked()
            {
                self.config.save_as(&name);
                self.new_profile.clear();
                self.dirty = true;
            }
            if ui
                .add_enabled(
                    self.config.profiles.len() > 1,
                    egui::Button::new(crate::i18n::tr("Usuń profil", "Delete profile")),
                )
                .clicked()
            {
                self.config.delete_active();
                self.dirty = true;
            }
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, snap: Option<&Snapshot>, dt: f32) {
        self.global_bar(ui);
        ui.add_space(8.0);
        self.games_card(ui);
        ui.add_space(10.0);
        self.toolbar(ui);
        ui.add_space(12.0);

        let Some(snap) = snap else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "Brak danych z czujników.",
                        "No sensor data.",
                    ))
                    .color(muted())
                    .size(16.0),
                );
            });
            return;
        };

        if !snap.admin {
            ui.label(
                RichText::new(
                    crate::i18n::tr("Program nie działa jako administrator - sterowanie wentylatorami płyty głównej będzie niedostępne.", "The program is not running as administrator - motherboard fan control will be unavailable."),
                )
                .color(warn()),
            );
            ui.add_space(8.0);
        }

        if snap.fans.is_empty() {
            ui.label(
                RichText::new(
                    crate::i18n::tr("Nie wykryto sterowalnych wentylatorów. Uruchom program jako administrator i sprawdź w README sekcję o sterowniku.", "No controllable fans detected. Run the program as administrator and check the driver section in the README."),
                )
                .color(muted()),
            );
            return;
        }

        // Złącza z zerowym odczytem są ukrywane, ale tylko w trybie Auto:
        // wentylator zatrzymany własnym ustawieniem musi zostać widoczny,
        // żeby dało się go z powrotem rozkręcić.
        let hidden = snap
            .fans
            .iter()
            .filter(|fan| {
                looks_unused(fan)
                    && self
                        .config
                        .fans()
                        .get(&fan.id)
                        .is_none_or(|cfg| cfg.mode == Mode::Auto)
            })
            .count();
        if hidden > 0 {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(
                    &mut self.show_idle,
                    crate::i18n::tr("Pokaż złącza bez wentylatora", "Show headers without a fan"),
                );
                if !self.show_idle {
                    ui.label(
                        RichText::new(crate::trf!("(ukryte: {hidden})", "(hidden: {hidden})"))
                            .color(muted()),
                    );
                }
            });
            ui.add_space(8.0);
        }
        let show_idle = self.show_idle;

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(14.0, 14.0);
                for fan in &snap.fans {
                    let auto = self
                        .config
                        .fans()
                        .get(&fan.id)
                        .is_none_or(|cfg| cfg.mode == Mode::Auto);
                    if !show_idle && auto && looks_unused(fan) {
                        continue;
                    }

                    // Kąt animacji: obroty spowolnione, żeby łopatki były czytelne.
                    let speed = match fan.rpm {
                        Some(rpm) => rpm / 60.0 * 0.12,
                        None => fan.percent.unwrap_or(0.0) / 100.0 * 3.0,
                    };
                    let angle = self.angles.entry(fan.id.clone()).or_insert(0.0);
                    *angle = (*angle + speed * TAU * dt) % TAU;
                    let angle = *angle;

                    let cfg = self.config.fans_mut().entry(fan.id.clone()).or_default();
                    if fan_card(ui, fan, cfg, &snap.temps, angle) {
                        self.dirty = true;
                    }
                }
            });
        });
    }
}

/// Karta jednego wentylatora. Zwraca true, jeśli użytkownik zmienił ustawienia.
fn fan_card(ui: &mut egui::Ui, fan: &Fan, cfg: &mut FanConfig, temps: &[Temp], angle: f32) -> bool {
    let mut changed = false;

    // Wentylatory kart AMD nie obsługują krzywej - zostaje Auto i Ręcznie.
    let curve_allowed = fan.kind != "GpuAmd";
    if !curve_allowed && cfg.mode == Mode::Curve {
        cfg.mode = Mode::Auto;
        changed = true;
    }

    theme::card(14, 16).show(ui, |ui| {
        ui.set_width(300.0);
        ui.push_id(&fan.id, |ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(&fan.name)
                        .size(17.0)
                        .strong()
                        .color(theme::text()),
                );
                ui.label(RichText::new(&fan.hw).size(11.5).color(muted()));
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    let (resp, painter) = ui.allocate_painter(Vec2::splat(104.0), Sense::hover());
                    draw_fan(&painter, resp.rect.center(), 50.0, angle);

                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.add_space(14.0);
                        let rpm = fan
                            .rpm
                            .map(|r| format!("{r:.0}"))
                            .unwrap_or_else(|| "--".into());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(rpm).size(30.0).strong().color(theme::text()));
                            ui.label(RichText::new("RPM").color(muted()));
                        });
                        let pct = fan
                            .percent
                            .map(|p| format!("{p:.0} %"))
                            .unwrap_or_else(|| "-- %".into());
                        ui.label(RichText::new(pct).size(16.0).color(accent()));
                    });
                });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    for (mode, label) in [
                        (Mode::Auto, "Auto (BIOS)"),
                        (Mode::Manual, crate::i18n::tr("Ręcznie", "Manual")),
                        (Mode::Curve, crate::i18n::tr("Krzywa", "Curve")),
                    ] {
                        let enabled = mode != Mode::Curve || curve_allowed;
                        let button = egui::Button::selectable(cfg.mode == mode, label);
                        if ui
                            .add_enabled(enabled, button)
                            .on_disabled_hover_text(crate::i18n::tr(
                                "Niedostępne dla wentylatorów kart AMD",
                                "Not available for AMD card fans",
                            ))
                            .clicked()
                            && cfg.mode != mode
                        {
                            cfg.mode = mode;
                            changed = true;
                        }
                    }
                });
                ui.add_space(8.0);

                match cfg.mode {
                    Mode::Auto => {
                        ui.label(
                            RichText::new(crate::i18n::tr(
                                "Wentylatorem steruje płyta główna.",
                                "The motherboard controls this fan.",
                            ))
                            .color(muted()),
                        );
                    }
                    Mode::Manual => {
                        ui.spacing_mut().slider_width = 220.0;
                        let slider = egui::Slider::new(&mut cfg.manual, MIN_PCT..=100.0)
                            .suffix(" %")
                            .fixed_decimals(0);
                        if ui.add(slider).changed() {
                            changed = true;
                        }
                    }
                    Mode::Curve => {
                        // Domyślnie temperatura procesora, a gdy jej brak - pierwszy czujnik.
                        if cfg.source.is_none() {
                            cfg.source = key_temps(temps)
                                .first()
                                .map(|(_, t)| t.id.clone())
                                .or_else(|| temps.first().map(|t| t.id.clone()));
                            if cfg.source.is_some() {
                                changed = true;
                            }
                        }

                        let current = cfg
                            .source
                            .as_ref()
                            .and_then(|s| temps.iter().find(|t| &t.id == s));
                        let selected = current.map(|t| t.label()).unwrap_or_else(|| {
                            crate::i18n::tr("Wybierz czujnik", "Choose a sensor").into()
                        });

                        ui.label(
                            RichText::new(crate::i18n::tr(
                                "Czujnik temperatury:",
                                "Temperature sensor:",
                            ))
                            .size(11.5)
                            .color(muted()),
                        );
                        egui::ComboBox::from_id_salt("source")
                            .width(290.0)
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for t in temps {
                                    let label = match t.value {
                                        Some(v) => format!("{}  ({v:.0} °C)", t.label()),
                                        None => t.label(),
                                    };
                                    if ui
                                        .selectable_label(
                                            cfg.source.as_deref() == Some(t.id.as_str()),
                                            label,
                                        )
                                        .clicked()
                                    {
                                        cfg.source = Some(t.id.clone());
                                        changed = true;
                                    }
                                }
                            });

                        ui.add_space(6.0);
                        let temp_now = current.and_then(|t| t.value);
                        if points_editor(ui, &mut cfg.points, temp_now) {
                            changed = true;
                        }
                    }
                }
            });
        });
    });

    changed
}

fn polar(center: Pos2, radius: f32, angle: f32) -> Pos2 {
    center + Vec2::new(angle.cos(), angle.sin()) * radius
}

/// Rysuje wentylator: obudowa, siedem łopatek obróconych o `angle`, piasta.
fn draw_fan(painter: &egui::Painter, center: Pos2, radius: f32, angle: f32) {
    painter.circle_filled(center, radius, well());
    painter.circle_stroke(center, radius, Stroke::new(2.0_f32, card_edge()));

    const BLADES: usize = 7;
    let inner = radius * 0.24;
    let outer = radius * 0.90;
    let blade = accent().linear_multiply(0.85);

    for k in 0..BLADES {
        let a = angle + k as f32 * TAU / BLADES as f32;
        let points = vec![
            polar(center, inner, a),
            polar(center, outer, a + 0.22),
            polar(center, outer, a + 0.50),
            polar(center, outer * 0.96, a + 0.72),
            polar(center, inner, a + 0.48),
        ];
        painter.add(Shape::convex_polygon(points, blade, Stroke::NONE));
    }

    painter.circle_filled(center, radius * 0.27, track());
    painter.circle_stroke(center, radius * 0.27, Stroke::new(1.5_f32, accent()));
    painter.circle_filled(center, radius * 0.07, accent());
}

/// Edytor krzywej temperatura -> obroty z przeciąganymi punktami.
/// Zwraca true po zmianie.
fn points_editor(ui: &mut egui::Ui, points: &mut [[f32; 2]], temp_now: Option<f32>) -> bool {
    let mut changed = false;
    let (resp, painter) = ui.allocate_painter(Vec2::new(296.0, 140.0), Sense::hover());
    let rect = resp.rect.shrink2(Vec2::new(8.0, 10.0));

    let to_screen = |t: f32, p: f32| -> Pos2 {
        Pos2::new(
            rect.left() + (t - TEMP_MIN) / (TEMP_MAX - TEMP_MIN) * rect.width(),
            rect.bottom() - p / 100.0 * rect.height(),
        )
    };

    painter.rect_filled(resp.rect, CornerRadius::same(8), well());

    // Siatka co 20 °C i co 25 %.
    let grid = Stroke::new(1.0_f32, track());
    let small = FontId::proportional(10.0);
    let mut t = TEMP_MIN;
    while t <= TEMP_MAX {
        painter.line_segment([to_screen(t, 0.0), to_screen(t, 100.0)], grid);
        painter.text(
            to_screen(t, 0.0) + Vec2::new(2.0, -2.0),
            Align2::LEFT_BOTTOM,
            format!("{t:.0}°"),
            small.clone(),
            muted(),
        );
        t += 20.0;
    }
    for p in [25.0, 50.0, 75.0, 100.0] {
        painter.line_segment([to_screen(TEMP_MIN, p), to_screen(TEMP_MAX, p)], grid);
    }

    // Przeciąganie punktów.
    let n = points.len();
    for i in 0..n {
        let pos = to_screen(points[i][0], points[i][1]);
        let hit = Rect::from_center_size(pos, Vec2::splat(22.0));
        let r = ui.interact(hit, ui.scope_id().with(("pt", i)), Sense::drag());
        if r.dragged() {
            if let Some(mouse) = r.interact_pointer_pos() {
                let lo = if i > 0 {
                    points[i - 1][0] + 1.0
                } else {
                    TEMP_MIN
                };
                let hi = if i + 1 < n {
                    points[i + 1][0] - 1.0
                } else {
                    TEMP_MAX
                };
                let t = TEMP_MIN + (mouse.x - rect.left()) / rect.width() * (TEMP_MAX - TEMP_MIN);
                let p = (rect.bottom() - mouse.y) / rect.height() * 100.0;
                points[i][0] = t.clamp(lo, hi.max(lo)).round();
                points[i][1] = p.clamp(MIN_PCT, 100.0).round();
                changed = true;
            }
        }
    }

    // Linia krzywej z płaskimi odcinkami na brzegach.
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        let mut line = vec![to_screen(TEMP_MIN, first[1])];
        line.extend(points.iter().map(|p| to_screen(p[0], p[1])));
        line.push(to_screen(TEMP_MAX, last[1]));
        painter.add(Shape::line(line, Stroke::new(2.0_f32, accent())));
    }

    // Aktualna temperatura i wynikające z niej wysterowanie.
    if let Some(t) = temp_now {
        let tc = t.clamp(TEMP_MIN, TEMP_MAX);
        let p = curve::eval(points, t).clamp(MIN_PCT, 100.0);
        painter.line_segment(
            [to_screen(tc, 0.0), to_screen(tc, 100.0)],
            Stroke::new(1.0_f32, warn().linear_multiply(0.6)),
        );
        painter.circle_filled(to_screen(tc, p), 3.5, warn());
        painter.text(
            Pos2::new(rect.left() + 2.0, rect.top()),
            Align2::LEFT_TOP,
            format!("{t:.0} °C -> {p:.0} %"),
            FontId::proportional(11.5),
            warn(),
        );
    }

    for p in points.iter() {
        let pos = to_screen(p[0], p[1]);
        painter.circle_filled(pos, 5.5, bg());
        painter.circle_stroke(pos, 5.5, Stroke::new(2.0_f32, accent()));
    }

    changed
}
