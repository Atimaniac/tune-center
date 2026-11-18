//! Zakładka NVIDIA - odpowiednik panelu sterowania Nvidia.
//!
//! - Overclock: dawna zakładka GPU (`crate::gpu::GpuTab`) - zegary, krzywa
//!   V/F, profile; ma własne zaplecze (także karty AMD i funkcje NVAPI spoza
//!   publicznego SDK) i działa nawet wtedy, gdy panel poniżej jest niedostępny.
//!
//! Pozostałe podzakładki korzystają wyłącznie z publicznego NVAPI SDK:
//!
//! - Ustawienia 3D: profil globalny i profile programów (DRS),
//! - Ekran: rozdzielczość, odświeżanie, obrót, skalowanie,
//! - Kolor i HDR: format, głębia, zakres dynamiczny, HDR10,
//! - G-SYNC: tryb G-SYNC, wskaźnik, Adaptive Sync monitora,
//! - Rozdzielczości niestandardowe.
//!
//! Zmiany ekranu działają na próbę: przez 15 s można je zachować albo cofnąć,
//! a bez odpowiedzi wracają same. Zmiany ustawień 3D są trwałe; przed
//! pierwszą zmianą w danym uruchomieniu programu powstaje kopia zapasowa
//! całej bazy ustawień sterownika (katalog `nvidia-backup` obok programu).

pub mod api;
pub mod catalog;
pub mod display;
pub mod drs;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::theme::{accent, muted, warn};
use api::Api;
use catalog::{Item, Kind};
use drs::{Drs, Location, Target};

/// Czas na potwierdzenie zmiany ekranu.
const CONFIRM_TIME: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    /// Dawna zakładka GPU.
    Overclock,
    Settings3d,
    /// Nadpisania DLSS (jak w NVIDIA App).
    Dlss,
    Display,
    Color,
    Gsync,
    Custom,
    /// Kompilacja shaderów z góry (crate::shaders).
    Shaders,
}

/// Zmiana ekranu czekająca na potwierdzenie.
enum Undo {
    Mode(display::Config),
    Color { display: u32, old: display::Color },
    Hdr { display: u32, old: bool },
    Custom { display: u32 },
}

struct Pending {
    undo: Undo,
    deadline: Instant,
}

pub struct NvcpTab {
    api: Option<Result<Arc<Api>, String>>,
    driver: Option<String>,
    drs: Option<Result<Drs, String>>,
    page: Page,
    status: Option<(String, bool)>,

    // ------------------------------ 3D ------------------------------
    target: Target,
    profiles: Option<Vec<drs::ProfileInfo>>,
    filter: String,
    /// Odczyty ustawień bieżącego profilu (klucz: identyfikator).
    values: HashMap<u32, Option<drs::Read>>,
    defaults: HashMap<u32, Option<(u32, Vec<u32>)>>,
    numbers: HashMap<u32, u32>,
    apps: Vec<String>,
    advanced: bool,
    advanced_all: bool,
    advanced_rows: Option<Vec<(u32, String, Option<drs::Read>)>>,
    advanced_edit: HashMap<u32, u32>,
    confirm_restore_all: bool,
    backup_choice: usize,

    // ----------------------------- ekran -----------------------------
    displays: Option<Result<Vec<display::Display>, String>>,
    selected: usize,
    config: Option<Result<display::Config, String>>,
    edit_mode: Option<display::Mode>,
    resolutions: HashMap<u32, Vec<(u32, u32)>>,
    refreshes: HashMap<(u32, u32, u32), Vec<u32>>,
    color: Option<Result<display::Color, String>>,
    color_edit: Option<display::Color>,
    color_support: HashMap<(u32, u8, u32, u8), bool>,
    hdr: Option<Result<display::Hdr, String>>,
    sync: Option<Result<bool, String>>,
    vrr: Option<display::Vrr>,
    customs: Option<Vec<display::CustomDisplay>>,
    custom_w: u32,
    custom_h: u32,
    custom_hz: f32,
    pending: Option<Pending>,
    /// Podzakładka Shader compilation.
    shaders: crate::shaders::ShaderTab,
}

impl NvcpTab {
    pub fn new() -> Self {
        Self {
            api: None,
            driver: None,
            drs: None,
            page: Page::Overclock,
            status: None,
            target: Target::Global,
            profiles: None,
            filter: String::new(),
            values: HashMap::new(),
            defaults: HashMap::new(),
            numbers: HashMap::new(),
            apps: Vec::new(),
            advanced: false,
            advanced_all: false,
            advanced_rows: None,
            advanced_edit: HashMap::new(),
            confirm_restore_all: false,
            backup_choice: 0,
            displays: None,
            selected: 0,
            config: None,
            edit_mode: None,
            resolutions: HashMap::new(),
            refreshes: HashMap::new(),
            color: None,
            color_edit: None,
            color_support: HashMap::new(),
            hdr: None,
            sync: None,
            vrr: None,
            customs: None,
            custom_w: 1920,
            custom_h: 1080,
            custom_hz: 60.0,
            pending: None,
            shaders: crate::shaders::ShaderTab::new(),
        }
    }

    fn api(&mut self) -> Result<Arc<Api>, String> {
        if self.api.is_none() {
            let api = Api::load().map(Arc::new);
            if let Ok(a) = &api {
                self.driver = a.driver();
            }
            self.api = Some(api);
        }
        self.api.clone().unwrap()
    }

    fn ok(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), false));
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), true));
    }

    /// Wołane w każdej klatce (także przy oknie w zasobniku): cofa zmianę
    /// ekranu, której nie potwierdzono na czas.
    pub fn tick(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.deadline)
        {
            self.revert();
            self.ok(tr(
                "Brak potwierdzenia - przywrócono poprzednie ustawienia ekranu.",
                "No confirmation - previous display settings restored.",
            ));
        }
    }

    /// `gpu` i `snap` służą podzakładce Overclock (dawna zakładka GPU).
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        gpu: &mut crate::gpu::GpuTab,
        snap: Option<&crate::bridge::Snapshot>,
    ) {
        // Brak panelu NVAPI (np. karta AMD) nie blokuje podzakładki Overclock.
        let api = self.api().ok();

        egui::Panel::top("nvcp_header")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (page, label) in [
                        (Page::Overclock, "Overclock"),
                        (Page::Settings3d, tr("Ustawienia 3D", "3D settings")),
                        (Page::Dlss, "DLSS"),
                        (Page::Display, tr("Ekran", "Display")),
                        (Page::Color, tr("Kolor i HDR", "Color and HDR")),
                        (Page::Gsync, "G-SYNC"),
                        (
                            Page::Custom,
                            tr("Rozdzielczości niestandardowe", "Custom resolutions"),
                        ),
                        (Page::Shaders, "Shader compilation"),
                    ] {
                        if crate::theme::sub_tab(ui, label, self.page == page).clicked() {
                            self.page = page;
                            self.status = None;
                        }
                    }
                    if let Some(driver) = &self.driver {
                        // Sam numer wersji (bez gałęzi), żeby mieścił się w wierszu
                        // podzakładek; pełna nazwa w podpowiedzi.
                        let short = driver.split(" (").next().unwrap_or(driver);
                        ui.separator();
                        ui.label(
                            RichText::new(crate::trf!("Sterownik {short}", "Driver {short}"))
                                .color(muted()),
                        )
                        .on_hover_text(driver.as_str());
                    }
                });
                ui.separator();
                if let Some(api) = &api {
                    self.pending_banner(ui, api);
                }
            });

        if self.page == Page::Overclock {
            // Dawna zakładka GPU ma własne panele i przewijanie.
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| gpu.ui(ui, snap));
        } else {
            let api = match api {
                Some(a) => a,
                None => {
                    let error = match &self.api {
                        Some(Err(e)) => e.clone(),
                        _ => String::new(),
                    };
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(tr(
                            "Panel Nvidia niedostępny (karta Nvidia jest potrzebna do tej części; Overclock działa dalej).",
                            "Nvidia panel unavailable (this part needs an Nvidia card; Overclock still works).",
                        ))
                        .strong(),
                    );
                    ui.label(RichText::new(error).color(warn()));
                    return;
                }
            };

            egui::Panel::bottom("nvcp_status")
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    if let Some((text, error)) = &self.status {
                        ui.add_space(4.0);
                        ui.label(RichText::new(text).color(if *error { warn() } else { accent() }));
                    }
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match self.page {
                            Page::Overclock => {}
                            Page::Settings3d => self.settings_ui(ui, &api),
                            Page::Dlss => self.dlss_ui(ui, &api),
                            Page::Display => self.display_ui(ui, &api),
                            Page::Color => self.color_ui(ui, &api),
                            Page::Gsync => self.gsync_ui(ui, &api),
                            Page::Custom => self.custom_ui(ui, &api),
                            Page::Shaders => self.shaders_ui(ui, &api),
                        });
                });
        }

        if self.pending.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }

    // ----------------------------- 3D: profile -----------------------------

    fn drs(&mut self, api: &Arc<Api>) -> Result<&mut Drs, String> {
        if self.drs.is_none() {
            self.drs = Some(Drs::open(api.clone()));
        }
        match self.drs.as_mut().unwrap() {
            Ok(d) => Ok(d),
            Err(e) => Err(e.clone()),
        }
    }

    fn invalidate_3d(&mut self) {
        self.values.clear();
        self.numbers.clear();
        self.advanced_rows = None;
        self.advanced_edit.clear();
        self.apps.clear();
    }

    /// Wartość, która obowiązuje dla ustawienia w bieżącym profilu.
    fn effective(&mut self, api: &Arc<Api>, id: u32) -> Option<u32> {
        if !self.values.contains_key(&id) {
            let target = self.target.clone();
            let read = self
                .drs(api)
                .ok()
                .and_then(|d| d.get(&target, id).ok().flatten());
            self.values.insert(id, read);
        }
        if let Some(Some(r)) = self.values.get(&id) {
            return r.value;
        }
        self.default_of(api, id)
    }

    fn default_of(&mut self, api: &Arc<Api>, id: u32) -> Option<u32> {
        if !self.defaults.contains_key(&id) {
            let d = self.drs(api).ok().and_then(|d| d.defaults(id));
            self.defaults.insert(id, d);
        }
        self.defaults.get(&id).cloned().flatten().map(|d| d.0)
    }

    /// Czy ustawienie jest zmienione w tym profilu (a nie dziedziczone).
    fn is_own(&self, ids: &[u32]) -> bool {
        ids.iter().any(
            |id| matches!(self.values.get(id), Some(Some(r)) if r.location == Location::Current),
        )
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        if !self.scope_ui(ui, api) {
            return;
        }
        self.sections_ui(ui, api, catalog::ITEMS, &catalog::Section::ALL);
        ui.add_space(10.0);
        self.restore_row(ui, api);
        ui.add_space(6.0);
        self.advanced_ui(ui, api);
    }

    /// Podzakładka DLSS: nadpisania DLSS jak w NVIDIA App (globalnie albo
    /// dla wybranej gry), zapisywane w profilach sterownika (DRS).
    fn dlss_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        ui.label(
            RichText::new(tr(
                "Nadpisania DLSS zapisywane w profilach sterownika - tak jak w NVIDIA App. Ustawienia globalne działają we wszystkich grach z DLSS; ustawienia programu tylko w wybranej grze (mają pierwszeństwo). Zmiany działają od następnego uruchomienia gry. Każda grupa działa dopiero po włączeniu jej \"Nadpisania przez sterownik\".",
                "DLSS overrides stored in driver profiles - like in NVIDIA App. Global settings apply to every DLSS game; program settings only to the chosen game (and take precedence). Changes take effect the next time the game starts. Each group works only with its \"Driver override\" turned on.",
            ))
            .small()
            .color(muted()),
        );
        ui.add_space(6.0);
        if !self.scope_ui(ui, api) {
            return;
        }
        self.sections_ui(ui, api, catalog::DLSS_ITEMS, &catalog::Section::DLSS);
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let label = if self.target == Target::Global {
                tr(
                    "Przywróć domyślne DLSS (globalne)",
                    "Restore DLSS defaults (global)",
                )
            } else {
                tr(
                    "Usuń nadpisania DLSS tej gry",
                    "Remove this game's DLSS overrides",
                )
            };
            if ui.button(label).clicked() {
                let mut ids: Vec<u32> = catalog::DLSS_ITEMS.iter().flat_map(|i| i.ids()).collect();
                ids.sort_unstable();
                ids.dedup();
                self.run(api, Action::Restore(ids));
            }
            if ui.button(tr("Odśwież", "Refresh")).clicked() {
                let r = self.drs(api).and_then(|d| d.reload());
                self.invalidate_3d();
                self.profiles = None;
                if let Err(e) = r {
                    self.fail(e);
                }
            }
        });
    }

    /// Sekcje z pozycjami katalogu dla bieżącego profilu.
    fn sections_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &Arc<Api>,
        items: &'static [Item],
        sections: &[catalog::Section],
    ) {
        let global = self.target == Target::Global;
        let mut actions: Vec<Action> = Vec::new();
        for &section in sections {
            let list: Vec<&'static Item> = items.iter().filter(|i| i.section == section).collect();
            if list.is_empty() {
                continue;
            }
            ui.add_space(8.0);
            ui.label(RichText::new(section.title()).strong().color(accent()));
            egui::Grid::new(("nvcp_items", section as u8))
                .num_columns(2)
                .min_col_width(360.0)
                .spacing([16.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    for item in list {
                        self.item_row(ui, api, item, global, &mut actions);
                        ui.end_row();
                    }
                });
        }
        for action in actions {
            self.run(api, action);
        }
    }

    /// Wybór profilu (globalny albo program); `false`, gdy nie ma czego pokazać.
    fn scope_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) -> bool {
        if let Err(e) = self.drs(api) {
            ui.label(RichText::new(e).color(warn()));
            return false;
        }

        // Wybór profilu: globalny albo program.
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(
                    self.target == Target::Global,
                    tr("Ustawienia globalne", "Global settings"),
                )
                .clicked()
                && self.target != Target::Global
            {
                self.target = Target::Global;
                self.invalidate_3d();
            }
            let program = matches!(self.target, Target::Profile(_));
            if ui
                .selectable_label(program, tr("Ustawienia programu", "Program settings"))
                .clicked()
                && !program
            {
                self.target = Target::Profile(String::new());
                self.invalidate_3d();
            }
        });
        ui.add_space(4.0);

        if let Target::Profile(name) = self.target.clone() {
            if !self.program_picker(ui, api, &name) {
                return false;
            }
        }
        true
    }

    /// Lista programów; zwraca `false`, gdy żaden nie jest jeszcze wybrany.
    fn program_picker(&mut self, ui: &mut egui::Ui, api: &Arc<Api>, current: &str) -> bool {
        if self.profiles.is_none() {
            let list = self.drs(api).and_then(|d| d.program_profiles());
            match list {
                Ok(l) => self.profiles = Some(l),
                Err(e) => {
                    ui.label(RichText::new(e).color(warn()));
                    return false;
                }
            }
        }
        let mut chosen: Option<String> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Program:", "Program:"));
            let shown = if current.is_empty() {
                tr("wybierz...", "choose...").to_owned()
            } else {
                current.to_owned()
            };
            egui::ComboBox::from_id_salt("nvcp_program")
                .width(320.0)
                .selected_text(shown)
                .height(400.0)
                .show_ui(ui, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text(tr("Szukaj...", "Search..."))
                            .desired_width(300.0),
                    );
                    let filter = self.filter.to_lowercase();
                    let mut shown = 0;
                    for p in self.profiles.as_ref().unwrap() {
                        if !filter.is_empty() && !p.name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        shown += 1;
                        if shown > 300 {
                            ui.label(RichText::new(tr("... zawęź wyszukiwanie", "... narrow the search")).color(muted()));
                            break;
                        }
                        let label = if p.predefined {
                            p.name.clone()
                        } else {
                            format!("{} ★", p.name)
                        };
                        if ui.selectable_label(p.name == current, label).clicked() {
                            chosen = Some(p.name.clone());
                        }
                    }
                });
            if ui
                .button(tr("Dodaj program...", "Add program..."))
                .on_hover_text(tr(
                    "Wybierz plik .exe gry lub programu. Jeśli ma już profil Nvidii, zostanie wybrany; inaczej powstanie nowy profil.",
                    "Pick the game's or program's .exe. If it already has an Nvidia profile it is selected; otherwise a new profile is created.",
                ))
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("exe", &["exe"])
                    .pick_file()
                {
                    let exe = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    match self.drs(api).and_then(|d| d.add_program(&exe)) {
                        Ok(name) => {
                            self.profiles = None;
                            chosen = Some(name.clone());
                            self.ok(crate::trf!("Profil programu: {name}", "Program profile: {name}"));
                        }
                        Err(e) => self.fail(e),
                    }
                }
            }
            let own = self
                .profiles
                .as_ref()
                .and_then(|l| l.iter().find(|p| p.name == current))
                .is_some_and(|p| !p.predefined);
            if own && ui.button(tr("Usuń profil", "Delete profile")).clicked() {
                match self.drs(api).and_then(|d| d.delete_profile(current)) {
                    Ok(()) => {
                        self.profiles = None;
                        self.target = Target::Profile(String::new());
                        self.invalidate_3d();
                        self.ok(tr("Profil usunięty.", "Profile deleted."));
                    }
                    Err(e) => self.fail(e),
                }
            }
        });
        if let Some(name) = chosen {
            self.target = Target::Profile(name);
            self.invalidate_3d();
        }
        let Target::Profile(name) = &self.target else {
            return true;
        };
        if name.is_empty() {
            ui.label(
                RichText::new(tr(
                    "Wybierz program z listy (★ = profil utworzony przez Ciebie) albo dodaj nowy.",
                    "Choose a program from the list (★ = profile you created) or add a new one.",
                ))
                .color(muted()),
            );
            return false;
        }
        if self.apps.is_empty() {
            let target = self.target.clone();
            self.apps = self
                .drs(api)
                .and_then(|d| d.apps(&target))
                .unwrap_or_default();
        }
        if !self.apps.is_empty() {
            ui.label(
                RichText::new(crate::trf!("Pliki: {}", "Files: {}", self.apps.join(", ")))
                    .small()
                    .color(muted()),
            );
        }
        true
    }

    fn item_row(
        &mut self,
        ui: &mut egui::Ui,
        api: &Arc<Api>,
        item: &'static Item,
        global: bool,
        actions: &mut Vec<Action>,
    ) {
        let ids = item.ids();
        for &id in &ids {
            self.effective(api, id);
        }
        let own = self.is_own(&ids);
        let name = RichText::new(item.name());
        ui.label(if own && !global { name.strong() } else { name })
            .on_hover_text(item.help());

        // Kontrolka i przycisk przywracania w jednej komórce siatki.
        ui.horizontal(|ui| {
            match &item.kind {
                Kind::Choice(opts) => {
                    let values: HashMap<u32, Option<u32>> = ids
                        .iter()
                        .map(|&id| (id, self.effective(api, id)))
                        .collect();
                    let current =
                        catalog::current_option(opts, |id| values.get(&id).copied().flatten());
                    let mut text = match current {
                        Some(i) => tr(opts[i].pl, opts[i].en).to_owned(),
                        None => {
                            let raw: Vec<String> = ids
                                .iter()
                                .map(|id| format!("0x{:X}", values[id].unwrap_or(0)))
                                .collect();
                            crate::trf!("inne ({})", "other ({})", raw.join(", "))
                        }
                    };
                    if !global && !own {
                        text = crate::trf!("{text} (globalne)", "{text} (global)");
                    }
                    egui::ComboBox::from_id_salt(("nvcp_item", item.section as u8, item.en))
                        .width(280.0)
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            if !global
                                && ui
                                    .selectable_label(
                                        !own,
                                        tr("Użyj ustawienia globalnego", "Use global setting"),
                                    )
                                    .clicked()
                            {
                                actions.push(Action::Restore(ids.clone()));
                            }
                            for (i, opt) in opts.iter().enumerate() {
                                if ui
                                    .selectable_label(
                                        current == Some(i) && (own || global),
                                        tr(opt.pl, opt.en),
                                    )
                                    .clicked()
                                {
                                    actions.push(Action::Set(opt.writes.to_vec()));
                                }
                            }
                        });
                }
                Kind::Number {
                    id,
                    min,
                    max,
                    zero_pl,
                    zero_en,
                    unit,
                } => {
                    let value = self.effective(api, *id).unwrap_or(0);
                    let edit = self.numbers.entry(*id).or_insert(value.clamp(*min, *max));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(edit).range(*min..=*max).suffix(
                            if unit.is_empty() {
                                String::new()
                            } else {
                                format!(" {unit}")
                            },
                        ));
                        if *edit == 0 {
                            ui.label(RichText::new(tr(zero_pl, zero_en)).color(muted()));
                        }
                        if *edit != value && ui.button(tr("Zastosuj", "Apply")).clicked() {
                            actions.push(Action::Set(vec![(*id, *edit)]));
                        }
                        if !global && !own {
                            ui.label(RichText::new(tr("(globalne)", "(global)")).color(muted()));
                        }
                    });
                }
            }

            if own {
                let (label, hint) = if global {
                    (
                        tr("domyślne", "default"),
                        tr("Przywróć domyślne sterownika", "Restore driver default"),
                    )
                } else {
                    (
                        tr("globalne", "global"),
                        tr(
                            "Wróć do ustawienia globalnego",
                            "Back to the global setting",
                        ),
                    )
                };
                if ui.small_button(label).on_hover_text(hint).clicked() {
                    actions.push(Action::Restore(ids.clone()));
                }
            }
        });
    }

    fn run(&mut self, api: &Arc<Api>, action: Action) {
        let target = self.target.clone();
        let result = match &action {
            Action::Set(pairs) => self.drs(api).and_then(|d| d.set_many(&target, pairs)),
            Action::Restore(ids) => self.drs(api).and_then(|d| d.restore(&target, ids)),
        };
        self.invalidate_3d();
        match result {
            Ok(()) => {
                let backup = self
                    .drs(api)
                    .ok()
                    .and_then(|d| d.backup_path().cloned())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.ok(crate::trf!(
                    "Zapisano w sterowniku. Kopia zapasowa: {backup}",
                    "Saved in the driver. Backup: {backup}"
                ));
            }
            Err(e) => self.fail(e),
        }
    }

    fn restore_row(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        ui.horizontal_wrapped(|ui| {
            let profile_label = if self.target == Target::Global {
                tr("Przywróć domyślne (globalne)", "Restore defaults (global)")
            } else {
                tr("Przywróć domyślne profilu", "Restore profile defaults")
            };
            if ui.button(profile_label).clicked() {
                let target = self.target.clone();
                let r = self.drs(api).and_then(|d| d.restore_profile(&target));
                self.invalidate_3d();
                match r {
                    Ok(()) => self.ok(tr("Przywrócono ustawienia domyślne profilu.", "Profile defaults restored.")),
                    Err(e) => self.fail(e),
                }
            }
            let label = if self.confirm_restore_all {
                tr("Na pewno? Kliknij ponownie", "Sure? Click again")
            } else {
                tr("Przywróć wszystkie profile", "Restore all profiles")
            };
            if ui
                .button(RichText::new(label).color(if self.confirm_restore_all { warn() } else { ui.visuals().text_color() }))
                .on_hover_text(tr(
                    "Ustawienia fabryczne wszystkich profili sterownika (jak \"Przywróć\" w panelu Nvidia).",
                    "Factory settings for every driver profile (like \"Restore\" in the Nvidia panel).",
                ))
                .clicked()
            {
                if self.confirm_restore_all {
                    self.confirm_restore_all = false;
                    let r = self.drs(api).and_then(|d| d.restore_all());
                    self.invalidate_3d();
                    self.profiles = None;
                    match r {
                        Ok(()) => self.ok(tr("Przywrócono ustawienia fabryczne wszystkich profili.", "All profiles restored to factory settings.")),
                        Err(e) => self.fail(e),
                    }
                } else {
                    self.confirm_restore_all = true;
                }
            }
            if ui.button(tr("Odśwież", "Refresh")).on_hover_text(tr("Wczytaj ustawienia sterownika ponownie (np. po zmianach w panelu Nvidia).", "Reload driver settings (e.g. after changes in the Nvidia panel).")).clicked() {
                let r = self.drs(api).and_then(|d| d.reload());
                self.invalidate_3d();
                self.profiles = None;
                if let Err(e) = r {
                    self.fail(e);
                }
            }
        });

        let backups = drs::backups();
        if !backups.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Kopie zapasowe:", "Backups:")).color(muted()));
                self.backup_choice = self.backup_choice.min(backups.len() - 1);
                let name = |p: &std::path::PathBuf| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                egui::ComboBox::from_id_salt("nvcp_backup")
                    .width(220.0)
                    .selected_text(name(&backups[self.backup_choice]))
                    .show_ui(ui, |ui| {
                        for (i, b) in backups.iter().enumerate() {
                            ui.selectable_value(&mut self.backup_choice, i, name(b));
                        }
                    });
                if ui
                    .button(tr("Przywróć z kopii", "Restore from backup"))
                    .clicked()
                {
                    let path = backups[self.backup_choice].clone();
                    let r = self.drs(api).and_then(|d| d.restore_backup(&path));
                    self.invalidate_3d();
                    self.profiles = None;
                    match r {
                        Ok(()) => self.ok(tr(
                            "Przywrócono ustawienia z kopii zapasowej.",
                            "Settings restored from backup.",
                        )),
                        Err(e) => self.fail(e),
                    }
                }
            });
        }
    }

    fn advanced_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        let open = egui::CollapsingHeader::new(tr(
            "Zaawansowane - wszystkie ustawienia sterownika",
            "Advanced - all driver settings",
        ))
        .default_open(self.advanced)
        .show(ui, |ui| {
            ui.label(RichText::new(tr(
                "Surowe wartości ze sterownika (nazwy po angielsku, jak w NVAPI). Zmieniaj tylko, jeśli wiesz, co robi dane ustawienie.",
                "Raw driver values (English names, as in NVAPI). Change only if you know what the setting does.",
            ))
            .color(muted()));
            if ui.checkbox(&mut self.advanced_all, tr("Pokaż wszystkie znane sterownikowi (nie tylko zmienione w profilu)", "Show all settings known to the driver (not only those changed in the profile)")).changed() {
                self.advanced_rows = None;
            }
            if self.advanced_rows.is_none() {
                let target = self.target.clone();
                let all = self.advanced_all;
                let rows = self.drs(api).map(|d| {
                    if all {
                        d.all_ids()
                            .into_iter()
                            .map(|id| {
                                let name = d.name_of(id).unwrap_or_default();
                                (id, name, d.read_any(&target, id).ok().flatten())
                            })
                            .collect::<Vec<_>>()
                    } else {
                        d.profile_settings(&target)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|r| {
                                let name = if r.name.is_empty() { d.name_of(r.id).unwrap_or_default() } else { r.name.clone() };
                                (r.id, name, Some(r))
                            })
                            .collect()
                    }
                });
                self.advanced_rows = Some(rows.unwrap_or_default());
            }
            let rows = self.advanced_rows.clone().unwrap_or_default();
            let mut actions = Vec::new();
            egui::Grid::new("nvcp_adv")
                .num_columns(4)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new(tr("Ustawienie", "Setting")).strong());
                    ui.label(RichText::new("ID").strong());
                    ui.label(RichText::new(tr("Wartość", "Value")).strong());
                    ui.label("");
                    ui.end_row();
                    for (id, name, read) in rows {
                        ui.label(if name.is_empty() { "?".to_owned() } else { name });
                        ui.label(RichText::new(format!("0x{id:08X}")).monospace());
                        match read.as_ref().and_then(|r| r.value) {
                            Some(value) => {
                                let edit = self.advanced_edit.entry(id).or_insert(value);
                                ui.horizontal(|ui| {
                                    ui.add(egui::DragValue::new(edit).hexadecimal(8, false, true).prefix("0x"));
                                    let place = match read.as_ref().map(|r| r.location) {
                                        Some(Location::Current) => tr("w profilu", "in profile"),
                                        Some(Location::Global) => tr("globalne", "global"),
                                        Some(Location::Base) => tr("bazowe", "base"),
                                        _ => tr("domyślne", "default"),
                                    };
                                    ui.label(RichText::new(place).small().color(muted()));
                                });
                                ui.horizontal(|ui| {
                                    if *edit != value && ui.small_button(tr("Zapisz", "Save")).clicked() {
                                        actions.push(Action::Set(vec![(id, *edit)]));
                                    }
                                    if read.as_ref().is_some_and(|r| r.location == Location::Current)
                                        && ui.small_button(tr("przywróć", "reset")).clicked()
                                    {
                                        actions.push(Action::Restore(vec![id]));
                                    }
                                });
                            }
                            None => {
                                ui.label(RichText::new(if read.is_some() { tr("(nie DWORD - tylko podgląd)", "(not DWORD - view only)") } else { tr("(nieustawione)", "(not set)") }).color(muted()));
                                ui.label("");
                            }
                        }
                        ui.end_row();
                    }
                });
            for a in actions {
                self.run(api, a);
            }
        });
        self.advanced = open.fully_open();
    }

    // ------------------------------- ekrany --------------------------------

    fn load_displays(&mut self, api: &Arc<Api>) {
        if self.displays.is_none() {
            self.displays = Some(display::displays(api));
            self.selected = 0;
        }
    }

    fn current_display(&self) -> Option<display::Display> {
        match &self.displays {
            Some(Ok(list)) => list.get(self.selected).cloned(),
            _ => None,
        }
    }

    fn invalidate_display(&mut self) {
        self.config = None;
        self.edit_mode = None;
        self.color = None;
        self.color_edit = None;
        self.hdr = None;
        self.sync = None;
        self.vrr = None;
        self.customs = None;
    }

    /// Wybór ekranu; zwraca wybrany albo `None` (z komunikatem).
    fn display_picker(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) -> Option<display::Display> {
        self.load_displays(api);
        let list = match &self.displays {
            Some(Ok(l)) if !l.is_empty() => l.clone(),
            Some(Err(e)) => {
                ui.label(RichText::new(e.clone()).color(warn()));
                return None;
            }
            _ => {
                ui.label(tr(
                    "Nie znaleziono ekranów podłączonych do karty Nvidia.",
                    "No displays connected to an Nvidia GPU.",
                ));
                return None;
            }
        };
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Ekran:", "Display:"));
            egui::ComboBox::from_id_salt("nvcp_display")
                .width(320.0)
                .selected_text(list[self.selected.min(list.len() - 1)].label())
                .show_ui(ui, |ui| {
                    for (i, d) in list.iter().enumerate() {
                        let mut label = d.label();
                        if d.primary {
                            label.push_str(tr(" - główny", " - primary"));
                        }
                        if ui.selectable_label(self.selected == i, label).clicked() {
                            self.selected = i;
                            changed = true;
                        }
                    }
                });
            if ui.button(tr("Odśwież", "Refresh")).clicked() {
                self.displays = None;
                self.resolutions.clear();
                self.refreshes.clear();
                self.color_support.clear();
                changed = true;
            }
        });
        if changed {
            self.invalidate_display();
            self.load_displays(api);
        }
        self.current_display()
    }

    fn config(&mut self, api: &Arc<Api>) -> Result<&display::Config, String> {
        if self.config.is_none() {
            self.config = Some(display::Config::read(api));
        }
        match self.config.as_ref().unwrap() {
            Ok(c) => Ok(c),
            Err(e) => Err(e.clone()),
        }
    }

    /// Rozdzielczości, które sterownik przyjmuje dla ekranu (sprawdzane
    /// przez `VALIDATE_ONLY` przy bieżącym odświeżaniu albo 60 Hz).
    fn resolutions(
        &mut self,
        api: &Arc<Api>,
        d: &display::Display,
        current: display::Mode,
    ) -> Vec<(u32, u32)> {
        if let Some(list) = self.resolutions.get(&d.id) {
            return list.clone();
        }
        let mut candidates: Vec<(u32, u32)> = display::COMMON_RESOLUTIONS.to_vec();
        candidates.push((current.width, current.height));
        if let Some(n) = d.native {
            candidates.push(n);
        }
        let (max_w, max_h) = d.native.unwrap_or((u32::MAX, u32::MAX));
        candidates.retain(|&(w, h)| w <= max_w && h <= max_h);
        candidates.sort_unstable_by(|a, b| b.cmp(a));
        candidates.dedup();
        let portrait = current.rotation % 2 == 1;
        let mut out = Vec::new();
        if let Ok(cfg) = self.config(api) {
            for (w, h) in candidates {
                let (sw, sh) = if portrait { (h, w) } else { (w, h) };
                let ok = (sw, sh) == (current.width, current.height)
                    || [current.refresh_mhz, 60_000].iter().any(|&r| {
                        cfg.with_mode(
                            d.id,
                            display::Mode {
                                width: sw,
                                height: sh,
                                refresh_mhz: r,
                                ..current
                            },
                        )
                        .is_some_and(|mut c| c.validate(api))
                    });
                if ok {
                    out.push((w, h));
                }
            }
        }
        self.resolutions.insert(d.id, out.clone());
        out
    }

    fn refresh_rates(
        &mut self,
        api: &Arc<Api>,
        d: &display::Display,
        mode: display::Mode,
    ) -> Vec<u32> {
        let key = (d.id, mode.width, mode.height);
        if let Some(list) = self.refreshes.get(&key) {
            return list.clone();
        }
        let mut out = Vec::new();
        if let Ok(cfg) = self.config(api) {
            let current = cfg.mode(d.id);
            let mut candidates = display::COMMON_REFRESH_MHZ.to_vec();
            if let Some(c) = current {
                candidates.push(c.refresh_mhz);
            }
            // Sterownik w teście (VALIDATE_ONLY) przyjmuje też odświeżanie,
            // którego monitor nie obsługuje - górną granicę bierzemy z EDID,
            // a bez niej z bieżącego trybu.
            let limit_mhz = d
                .max_refresh_hz
                .map(|hz| hz * 1000 + 500)
                .or(current.map(|c| c.refresh_mhz + 500))
                .unwrap_or(u32::MAX);
            candidates.retain(|&r| r <= limit_mhz);
            candidates.sort_unstable();
            candidates.dedup();
            for r in candidates {
                let same = current.is_some_and(|c| {
                    (c.width, c.height, c.refresh_mhz) == (mode.width, mode.height, r)
                });
                if same
                    || cfg
                        .with_mode(
                            d.id,
                            display::Mode {
                                refresh_mhz: r,
                                ..mode
                            },
                        )
                        .is_some_and(|mut c| c.validate(api))
                {
                    out.push(r);
                }
            }
        }
        self.refreshes.insert(key, out.clone());
        out
    }

    fn display_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        let Some(d) = self.display_picker(ui, api) else {
            return;
        };
        let current = match self.config(api) {
            Ok(cfg) => cfg.mode(d.id),
            Err(e) => {
                ui.label(RichText::new(e).color(warn()));
                return;
            }
        };
        let Some(current) = current else {
            ui.label(
                RichText::new(tr(
                    "Ten ekran nie jest aktywny w konfiguracji Windows.",
                    "This display is not active in the Windows configuration.",
                ))
                .color(muted()),
            );
            return;
        };
        ui.add_space(6.0);
        ui.label(
            RichText::new(crate::trf!(
                "Teraz: {} x {} @ {:.2} Hz, obrót {}, skalowanie: {}",
                "Now: {} x {} @ {:.2} Hz, rotation {}, scaling: {}",
                current.width,
                current.height,
                current.refresh_mhz as f32 / 1000.0,
                display::rotation_name(current.rotation),
                display::scaling_name(current.scaling)
            ))
            .color(muted()),
        );
        if let Some((w, h)) = d.native {
            ui.label(
                RichText::new(crate::trf!(
                    "Natywna rozdzielczość monitora: {w} x {h}",
                    "Native monitor resolution: {w} x {h}"
                ))
                .small()
                .color(muted()),
            );
        }
        ui.add_space(8.0);

        let mut edit = self.edit_mode.unwrap_or(current);
        let portrait = edit.rotation % 2 == 1;
        let (lw, lh) = if portrait {
            (edit.height, edit.width)
        } else {
            (edit.width, edit.height)
        };
        let resolutions = self.resolutions(api, &d, current);
        egui::Grid::new("nvcp_mode")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label(tr("Rozdzielczość", "Resolution"));
                egui::ComboBox::from_id_salt("nvcp_res")
                    .width(200.0)
                    .selected_text(format!("{lw} x {lh}"))
                    .show_ui(ui, |ui| {
                        for &(w, h) in &resolutions {
                            let native = d.native == Some((w, h));
                            let label = if native {
                                format!("{w} x {h} ({})", tr("natywna", "native"))
                            } else {
                                format!("{w} x {h}")
                            };
                            if ui.selectable_label((w, h) == (lw, lh), label).clicked() {
                                let (sw, sh) = if portrait { (h, w) } else { (w, h) };
                                edit.width = sw;
                                edit.height = sh;
                            }
                        }
                    });
                ui.end_row();

                ui.label(tr("Częstotliwość odświeżania", "Refresh rate"));
                let rates = self.refresh_rates(api, &d, edit);
                if !rates.contains(&edit.refresh_mhz) {
                    if let Some(&best) = rates.iter().max() {
                        edit.refresh_mhz = best;
                    }
                }
                egui::ComboBox::from_id_salt("nvcp_hz")
                    .width(200.0)
                    .selected_text(format!("{:.2} Hz", edit.refresh_mhz as f32 / 1000.0))
                    .show_ui(ui, |ui| {
                        for &r in rates.iter().rev() {
                            ui.selectable_value(
                                &mut edit.refresh_mhz,
                                r,
                                format!("{:.2} Hz", r as f32 / 1000.0),
                            );
                        }
                    });
                ui.end_row();

                ui.label(tr("Obrót", "Rotation"));
                ui.horizontal(|ui| {
                    for r in 0..4u32 {
                        if ui
                            .selectable_label(edit.rotation == r, display::rotation_name(r))
                            .clicked()
                            && edit.rotation != r
                        {
                            if (edit.rotation % 2) != (r % 2) {
                                std::mem::swap(&mut edit.width, &mut edit.height);
                            }
                            edit.rotation = r;
                        }
                    }
                });
                ui.end_row();

                ui.label(tr("Skalowanie", "Scaling"));
                egui::ComboBox::from_id_salt("nvcp_scaling")
                    .width(260.0)
                    .selected_text(display::scaling_name(edit.scaling))
                    .show_ui(ui, |ui| {
                        for &s in display::SCALINGS {
                            ui.selectable_value(&mut edit.scaling, s, display::scaling_name(s));
                        }
                    });
                ui.end_row();
            });
        self.edit_mode = Some(edit);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let changed = edit != current;
            if ui
                .add_enabled(
                    changed && self.pending.is_none(),
                    egui::Button::new(tr("Zastosuj", "Apply")),
                )
                .clicked()
            {
                self.apply_mode(api, d.id, edit);
            }
            if changed && ui.button(tr("Anuluj", "Cancel")).clicked() {
                self.edit_mode = None;
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new(tr(
            "Lista zawiera tryby, które sterownik zaakceptował w teście (bez zmiany obrazu). Po zastosowaniu masz 15 s na potwierdzenie - bez niego ekran wróci do poprzedniego trybu.",
            "The list shows modes the driver accepted in a dry run (no picture change). After applying you have 15 s to confirm - otherwise the display returns to the previous mode.",
        ))
        .small()
        .color(muted()));
    }

    fn apply_mode(&mut self, api: &Arc<Api>, display_id: u32, mode: display::Mode) {
        let old = match self.config(api) {
            Ok(c) => c.duplicate(),
            Err(e) => return self.fail(e),
        };
        let Some(mut new) = old.with_mode(display_id, mode) else {
            return self.fail(tr(
                "Nie znaleziono ekranu w konfiguracji.",
                "Display not found in the configuration.",
            ));
        };
        if !new.validate(api) {
            return self.fail(tr(
                "Sterownik nie przyjmuje tego trybu.",
                "The driver does not accept this mode.",
            ));
        }
        match new.apply(api, display::SAVE_TO_PERSISTENCE) {
            Ok(()) => {
                self.pending = Some(Pending {
                    undo: Undo::Mode(old),
                    deadline: Instant::now() + CONFIRM_TIME,
                });
                self.config = None;
                self.edit_mode = None;
                self.status = None;
            }
            Err(e) => self.fail(e),
        }
    }

    // ------------------------------ kolor / HDR ----------------------------

    fn color_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        let Some(d) = self.display_picker(ui, api) else {
            return;
        };
        if self.color.is_none() {
            self.color = Some(display::color(api, d.id));
        }
        let current = match self.color.clone().unwrap() {
            Ok(c) => c,
            Err(e) => {
                ui.label(RichText::new(e).color(warn()));
                return;
            }
        };
        let mut edit = self.color_edit.unwrap_or(current);
        ui.add_space(8.0);
        ui.label(
            RichText::new(tr("Kolor wyjścia", "Output color"))
                .strong()
                .color(accent()),
        );
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut edit.policy,
                1,
                tr("Domyślne ustawienia koloru", "Default color settings"),
            );
            ui.radio_value(
                &mut edit.policy,
                0,
                tr("Ustawienia koloru Nvidia", "Nvidia color settings"),
            );
        });
        let user = edit.policy == 0;
        let formats: [(u8, &str); 4] = [
            (0, "RGB"),
            (2, "YCbCr444"),
            (1, "YCbCr422"),
            (3, "YCbCr420"),
        ];
        let depths: [(u32, &str); 4] = [
            (2, tr("8 bitów", "8 bpc")),
            (3, tr("10 bitów", "10 bpc")),
            (4, tr("12 bitów", "12 bpc")),
            (1, tr("6 bitów", "6 bpc")),
        ];
        let ranges: [(u8, &str); 2] = [(0, tr("Pełny", "Full")), (1, tr("Ograniczony", "Limited"))];

        ui.add_enabled_ui(user, |ui| {
            egui::Grid::new("nvcp_color")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label(tr("Format koloru", "Output color format"));
                    ui.horizontal(|ui| {
                        for (f, name) in formats {
                            let ok =
                                self.supported(api, d.id, display::Color { format: f, ..edit });
                            if ui
                                .add_enabled(ok, egui::Button::selectable(edit.format == f, name))
                                .clicked()
                            {
                                edit.format = f;
                            }
                        }
                    });
                    ui.end_row();
                    ui.label(tr("Głębia koloru", "Output color depth"));
                    ui.horizontal(|ui| {
                        for (b, name) in depths {
                            let ok = self.supported(api, d.id, display::Color { bpc: b, ..edit });
                            if ui
                                .add_enabled(ok, egui::Button::selectable(edit.bpc == b, name))
                                .clicked()
                            {
                                edit.bpc = b;
                            }
                        }
                    });
                    ui.end_row();
                    ui.label(tr("Zakres dynamiczny", "Output dynamic range"));
                    ui.horizontal(|ui| {
                        for (r, name) in ranges {
                            let ok = self.supported(
                                api,
                                d.id,
                                display::Color {
                                    dynamic_range: r,
                                    ..edit
                                },
                            );
                            if ui
                                .add_enabled(
                                    ok,
                                    egui::Button::selectable(edit.dynamic_range == r, name),
                                )
                                .clicked()
                            {
                                edit.dynamic_range = r;
                            }
                        }
                    });
                    ui.end_row();
                });
        });
        self.color_edit = Some(edit);
        ui.horizontal(|ui| {
            let changed = edit != current;
            if ui
                .add_enabled(
                    changed && self.pending.is_none(),
                    egui::Button::new(tr("Zastosuj", "Apply")),
                )
                .clicked()
            {
                if !display::color_supported(api, d.id, edit) {
                    self.fail(tr(
                        "Monitor nie obsługuje tego połączenia ustawień.",
                        "The display does not support this combination.",
                    ));
                } else {
                    match display::set_color(api, d.id, edit) {
                        Ok(()) => {
                            self.pending = Some(Pending {
                                undo: Undo::Color {
                                    display: d.id,
                                    old: current,
                                },
                                deadline: Instant::now() + CONFIRM_TIME,
                            });
                            self.color = None;
                            self.color_edit = None;
                            self.status = None;
                        }
                        Err(e) => self.fail(e),
                    }
                }
            }
            if changed && ui.button(tr("Anuluj", "Cancel")).clicked() {
                self.color_edit = None;
            }
        });

        ui.add_space(14.0);
        ui.label(RichText::new("HDR").strong().color(accent()));
        if self.hdr.is_none() {
            self.hdr = Some(display::hdr(api, d.id));
        }
        match self.hdr.clone().unwrap() {
            Ok(h) => {
                ui.label(crate::trf!(
                    "Tryb HDR: {}",
                    "HDR mode: {}",
                    display::hdr_mode_name(h.mode)
                ));
                if !h.supported {
                    ui.label(
                        RichText::new(tr(
                            "Monitor nie zgłasza obsługi HDR10 (ST2084).",
                            "The display does not report HDR10 (ST2084) support.",
                        ))
                        .color(muted()),
                    );
                } else {
                    let on = h.mode != 0;
                    let label = if on {
                        tr("Wyłącz HDR", "Turn HDR off")
                    } else {
                        tr("Włącz HDR (HDR10)", "Turn HDR on (HDR10)")
                    };
                    if ui
                        .add_enabled(self.pending.is_none(), egui::Button::new(label))
                        .clicked()
                    {
                        match display::set_hdr(api, d.id, !on) {
                            Ok(()) => {
                                self.pending = Some(Pending {
                                    undo: Undo::Hdr {
                                        display: d.id,
                                        old: on,
                                    },
                                    deadline: Instant::now() + CONFIRM_TIME,
                                });
                                self.hdr = None;
                                self.status = None;
                            }
                            Err(e) => self.fail(e),
                        }
                    }
                }
                ui.label(
                    RichText::new(tr(
                        "To ten sam przełącznik co \"Użyj HDR\" w Ustawieniach Windows.",
                        "This is the same switch as \"Use HDR\" in Windows Settings.",
                    ))
                    .small()
                    .color(muted()),
                );
            }
            Err(e) => {
                ui.label(RichText::new(e).color(muted()));
            }
        }
    }

    fn supported(&mut self, api: &Arc<Api>, display_id: u32, c: display::Color) -> bool {
        let key = (display_id, c.format, c.bpc, c.dynamic_range);
        *self.color_support.entry(key).or_insert_with(|| {
            display::color_supported(api, display_id, display::Color { policy: 0, ..c })
        })
    }

    // -------------------------------- G-SYNC --------------------------------

    /// Siatka ustawień profilu globalnego (niezależnie od profilu wybranego
    /// w Ustawieniach 3D).
    fn global_items(
        &mut self,
        ui: &mut egui::Ui,
        api: &Arc<Api>,
        items: &'static [Item],
        grid: &str,
    ) {
        if let Err(e) = self.drs(api) {
            ui.label(RichText::new(e).color(warn()));
            return;
        }
        let saved = std::mem::replace(&mut self.target, Target::Global);
        if saved != Target::Global {
            self.invalidate_3d();
        }
        let mut actions = Vec::new();
        egui::Grid::new(grid)
            .num_columns(2)
            .min_col_width(360.0)
            .spacing([16.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for item in items {
                    self.item_row(ui, api, item, true, &mut actions);
                    ui.end_row();
                }
            });
        for a in actions {
            self.run(api, a);
        }
        if saved != Target::Global {
            self.target = saved;
            self.invalidate_3d();
        }
    }

    // --------------------------- Shader compilation -------------------------

    fn shaders_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        ui.label(
            RichText::new(tr(
                "Kompilacja shaderów z góry: Tune Center czyta shadery z plików gry i kompiluje je do pamięci podręcznej sterownika, zanim gra musi to zrobić w trakcie rozgrywki (przycięcia \"shader stutter\").",
                "Shader precompilation: Tune Center reads the shaders from the game files and compiles them into the driver's cache before the game has to do it mid-play (\"shader stutter\").",
            ))
            .color(muted()),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(tr("Pamięć podręczna sterownika", "Driver shader cache"))
                .strong()
                .color(accent()),
        );
        self.global_items(ui, api, catalog::SHADER_ITEMS, "nvcp_shader_cache");
        ui.horizontal_wrapped(|ui| {
            if let Some(dir) = crate::shaders::nvidia_cache_dir() {
                let size = self
                    .shaders
                    .cache_bytes()
                    .map(crate::shaders::engine::human)
                    .unwrap_or_else(|| "?".into());
                ui.label(
                    RichText::new(crate::trf!(
                        "Folder: {} - zajęte: {size}",
                        "Folder: {} - used: {size}",
                        dir.display()
                    ))
                    .color(muted()),
                );
                if ui.button(tr("Otwórz folder", "Open folder")).clicked() {
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                }
            }
        });
        ui.add_space(12.0);
        self.shaders.games_ui(ui);
    }

    fn gsync_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        ui.label(
            RichText::new(tr("Ustawienia globalne G-SYNC", "Global G-SYNC settings"))
                .strong()
                .color(accent()),
        );
        self.global_items(ui, api, catalog::GSYNC_ITEMS, "nvcp_gsync");

        ui.add_space(14.0);
        ui.label(
            RichText::new(tr("Monitor", "Display"))
                .strong()
                .color(accent()),
        );
        let Some(d) = self.display_picker(ui, api) else {
            return;
        };
        if self.vrr.is_none() {
            self.vrr = display::vrr(api, d.id);
        }
        if let Some(v) = self.vrr {
            let yes_no = |b: bool| if b { tr("tak", "yes") } else { tr("nie", "no") };
            // Flagi NV_GET_VRR_INFO opisane dosłownie: "włączone" dotyczy
            // bieżącej aplikacji pełnoekranowej, "w trybie VRR" - ekranu.
            ui.label(crate::trf!(
                "VRR możliwe: {}, ekran w trybie VRR: {}, włączone dla bieżącej aplikacji: {}, zażądane przez aplikację: {}",
                "VRR possible: {}, display in VRR mode: {}, enabled for the current application: {}, requested by the application: {}",
                yes_no(v.possible),
                yes_no(v.active_now),
                yes_no(v.enabled),
                yes_no(v.requested)
            ));
        }
        if self.sync.is_none() {
            self.sync = Some(display::adaptive_sync(api, d.id));
        }
        match self.sync.clone().unwrap() {
            Ok(mut on) => {
                if ui
                    .checkbox(
                        &mut on,
                        tr(
                            "Włącz G-SYNC / G-SYNC Compatible dla tego monitora",
                            "Enable G-SYNC / G-SYNC Compatible for this display",
                        ),
                    )
                    .changed()
                {
                    match display::set_adaptive_sync(api, d.id, on) {
                        Ok(()) => self.ok(tr("Zapisano.", "Saved.")),
                        Err(e) => self.fail(e),
                    }
                    self.sync = None;
                    self.vrr = None;
                }
            }
            Err(e) => {
                ui.label(RichText::new(e).color(muted()));
            }
        }
    }

    // ------------------------ rozdzielczości niestandardowe -------------------

    fn custom_ui(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        let Some(d) = self.display_picker(ui, api) else {
            return;
        };
        if self.customs.is_none() {
            self.customs = Some(display::custom_list(api, d.id));
        }
        let list = self.customs.clone().unwrap_or_default();
        ui.add_space(8.0);
        if list.is_empty() {
            ui.label(
                RichText::new(tr(
                    "Brak rozdzielczości niestandardowych dla tego ekranu.",
                    "No custom resolutions for this display.",
                ))
                .color(muted()),
            );
        } else {
            egui::Grid::new("nvcp_custom_list")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for c in &list {
                        ui.label(format!(
                            "{} x {} @ {:.2} Hz",
                            c.width,
                            c.height,
                            c.timing.refresh_mhz() as f32 / 1000.0
                        ));
                        if ui.small_button(tr("Usuń", "Delete")).clicked() {
                            match display::custom_delete(api, d.id, c) {
                                Ok(()) => self.ok(tr("Usunięto.", "Deleted.")),
                                Err(e) => self.fail(e),
                            }
                            self.customs = None;
                        }
                        ui.end_row();
                    }
                });
        }
        ui.add_space(12.0);
        ui.label(
            RichText::new(tr("Nowa rozdzielczość", "New resolution"))
                .strong()
                .color(accent()),
        );
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.custom_w)
                    .range(640..=7680)
                    .suffix(" px"),
            );
            ui.label("x");
            ui.add(
                egui::DragValue::new(&mut self.custom_h)
                    .range(480..=4320)
                    .suffix(" px"),
            );
            ui.label("@");
            ui.add(
                egui::DragValue::new(&mut self.custom_hz)
                    .range(23.0..=540.0)
                    .speed(0.1)
                    .suffix(" Hz"),
            );
            if ui
                .add_enabled(
                    self.pending.is_none(),
                    egui::Button::new(tr("Testuj", "Test")),
                )
                .clicked()
            {
                match display::custom_try(api, d.id, self.custom_w, self.custom_h, self.custom_hz) {
                    Ok(_) => {
                        self.pending = Some(Pending {
                            undo: Undo::Custom { display: d.id },
                            deadline: Instant::now() + CONFIRM_TIME,
                        });
                        self.status = None;
                        self.config = None;
                    }
                    Err(e) => self.fail(e),
                }
            }
        });
        ui.label(RichText::new(tr(
            "Taktowanie liczy sterownik (standard CVT z ograniczonym wygaszaniem). Rozdzielczość działa najpierw na próbę - zapisz ją przyciskiem \"Zachowaj\" w ciągu 15 s. Zbyt wysokie odświeżanie może dać czarny ekran; wtedy po prostu poczekaj na powrót.",
            "The driver computes the timing (CVT reduced blanking). The resolution is applied as a trial first - save it with \"Keep\" within 15 s. A too-high refresh rate may give a black screen; then just wait for the revert.",
        ))
        .small()
        .color(muted()));
    }

    // ------------------------- potwierdzanie zmian ---------------------------

    fn pending_banner(&mut self, ui: &mut egui::Ui, api: &Arc<Api>) {
        let Some(p) = &self.pending else { return };
        let left = p
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            + 1;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::trf!(
                    "Zachować nowe ustawienia ekranu? Powrót za {left} s.",
                    "Keep the new display settings? Reverting in {left} s."
                ))
                .strong()
                .color(warn()),
            );
            if ui.button(tr("Zachowaj", "Keep")).clicked() {
                self.keep(api);
            }
            if ui.button(tr("Cofnij", "Revert")).clicked() {
                self.revert();
                self.ok(tr(
                    "Przywrócono poprzednie ustawienia ekranu.",
                    "Previous display settings restored.",
                ));
            }
        });
        ui.separator();
    }

    fn keep(&mut self, api: &Arc<Api>) {
        let Some(p) = self.pending.take() else { return };
        if let Undo::Custom { display } = p.undo {
            match display::custom_keep(api, display) {
                Ok(()) => self.ok(tr("Rozdzielczość zapisana.", "Resolution saved.")),
                Err(e) => self.fail(e),
            }
            self.customs = None;
        } else {
            self.ok(tr("Zachowano.", "Kept."));
        }
        self.invalidate_display();
        self.resolutions.clear();
        self.refreshes.clear();
    }

    fn revert(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let Some(Ok(api)) = self.api.clone() else {
            return;
        };
        let result = match p.undo {
            Undo::Mode(mut old) => old.apply(&api, display::SAVE_TO_PERSISTENCE),
            Undo::Color { display, old } => display::set_color(&api, display, old),
            Undo::Hdr { display, old } => display::set_hdr(&api, display, old),
            Undo::Custom { display } => display::custom_revert(&api, display),
        };
        if let Err(e) = result {
            self.fail(crate::trf!(
                "Nie udało się cofnąć zmiany: {e}",
                "Could not revert the change: {e}"
            ));
        }
        self.invalidate_display();
        self.resolutions.clear();
        self.refreshes.clear();
    }
}

enum Action {
    Set(Vec<(u32, u32)>),
    Restore(Vec<u32>),
}

impl Drop for NvcpTab {
    /// Zamknięcie programu w trakcie próby = cofnięcie zmiany ekranu.
    fn drop(&mut self) {
        if self.pending.is_some() {
            self.revert();
        }
    }
}

#[cfg(test)]
mod hardware {
    //! Testy na prawdziwej karcie - tylko odczyt i próby `VALIDATE_ONLY`,
    //! niczego nie zmieniają. Uruchomienie:
    //! `cargo test nvcp_hardware -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn nvcp_hardware_read_only() {
        let api = Arc::new(Api::load().expect("NVAPI"));
        println!("sterownik: {:?}", api.driver());

        let drs = Drs::open(api.clone()).expect("DRS");
        let global = drs.info(&Target::Global).expect("profil globalny");
        println!("profil globalny: {global:?}");
        for item in catalog::ITEMS.iter().chain(catalog::GSYNC_ITEMS) {
            let values: Vec<String> = item
                .ids()
                .iter()
                .map(|&id| match drs.get(&Target::Global, id) {
                    Ok(Some(r)) => format!("0x{id:08X}={:?} ({:?})", r.value, r.location),
                    Ok(None) => format!("0x{id:08X}=domyślne {:?}", drs.defaults(id).map(|d| d.0)),
                    Err(e) => format!("0x{id:08X}: {e}"),
                })
                .collect();
            println!("  {}: {}", item.en, values.join(", "));
        }
        let t = Instant::now();
        let programs = drs.program_profiles().expect("profile");
        println!("profili programów: {} ({:?})", programs.len(), t.elapsed());
        println!("ustawień znanych sterownikowi: {}", drs.all_ids().len());

        let displays = display::displays(&api).expect("ekrany");
        println!("ekrany: {displays:#?}");
        let cfg = display::Config::read(&api).expect("konfiguracja");
        for d in &displays {
            let Some(mode) = cfg.mode(d.id) else { continue };
            println!("tryb {}: {mode:?}", d.label());
            println!("  kolor: {:?}", display::color(&api, d.id));
            println!("  HDR: {:?}", display::hdr(&api, d.id));
            println!(
                "  VRR: {:?}, Adaptive Sync: {:?}",
                display::vrr(&api, d.id),
                display::adaptive_sync(&api, d.id)
            );
            println!(
                "  własne rozdzielczości: {}",
                display::custom_list(&api, d.id).len()
            );
            let mut same = cfg.with_mode(d.id, mode).unwrap();
            let t = Instant::now();
            println!(
                "  bieżący tryb przechodzi VALIDATE_ONLY: {} ({:?})",
                same.validate(&api),
                t.elapsed()
            );
            let t = Instant::now();
            let mut ok = Vec::new();
            for &(w, h) in display::COMMON_RESOLUTIONS {
                if let Some(mut c) = cfg.with_mode(
                    d.id,
                    display::Mode {
                        width: w,
                        height: h,
                        refresh_mhz: 60_000,
                        ..mode
                    },
                ) {
                    if c.validate(&api) {
                        ok.push(format!("{w}x{h}"));
                    }
                }
            }
            println!(
                "  rozdzielczości @60 Hz: {} ({:?})",
                ok.join(" "),
                t.elapsed()
            );
            let t = Instant::now();
            let mut rates = Vec::new();
            for &r in display::COMMON_REFRESH_MHZ {
                if let Some(mut c) = cfg.with_mode(
                    d.id,
                    display::Mode {
                        refresh_mhz: r,
                        ..mode
                    },
                ) {
                    if c.validate(&api) {
                        rates.push(r / 1000);
                    }
                }
            }
            println!(
                "  odświeżanie przy bieżącej rozdzielczości: {rates:?} ({:?})",
                t.elapsed()
            );
        }
    }

    /// Zapis DRS na tymczasowym profilu nieistniejącego programu: utworzenie,
    /// zapis, odczyt kontrolny, powrót, usunięcie profilu. Robi kopię bazy.
    /// `cargo test nvcp_hardware_drs_write -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn nvcp_hardware_drs_write_roundtrip() {
        let api = Arc::new(Api::load().expect("NVAPI"));
        let mut drs = Drs::open(api).expect("DRS");
        let exe = "tunecenter-selftest-nvcp.exe";
        let name = drs.add_program(exe).expect("utworzenie profilu");
        println!("profil: {name}, kopia: {:?}", drs.backup_path());
        let target = Target::Profile(name.clone());
        assert_eq!(drs.apps(&target).unwrap(), vec![exe.to_owned()]);
        assert_eq!(
            drs.profile_of_app(exe).unwrap().as_deref(),
            Some(name.as_str())
        );

        // Opcja złożona (V-Sync adaptacyjna) i liczba (limit FPS).
        let vsync = &catalog::ITEMS[0];
        let Kind::Choice(opts) = &vsync.kind else {
            panic!()
        };
        drs.set_many(&target, opts[3].writes).expect("zapis V-Sync");
        drs.set_many(&target, &[(catalog::FRL_FPS, 77)])
            .expect("zapis FPS");
        let fps = drs.get(&target, catalog::FRL_FPS).unwrap().unwrap();
        assert_eq!((fps.value, fps.location), (Some(77), Location::Current));
        let value = |id| drs.get(&target, id).unwrap().and_then(|r| r.value);
        assert_eq!(catalog::current_option(opts, value), Some(3));

        // Powrót do globalnego i sprzątanie.
        drs.restore(&target, &[catalog::FRL_FPS])
            .expect("przywrócenie");
        let fps = drs.get(&target, catalog::FRL_FPS).unwrap();
        assert_ne!(fps.map(|r| r.location), Some(Location::Current));
        drs.delete_profile(&name).expect("usunięcie profilu");
        assert_eq!(drs.profile_of_app(exe).unwrap(), None);
        println!("OK: zapis, odczyt, przywrócenie i usunięcie profilu działają");
    }

    /// Zapis wszystkich nadpisań DLSS na tymczasowym profilu:
    /// `cargo test nvcp_hardware_dlss -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn nvcp_hardware_dlss() {
        let api = Arc::new(Api::load().expect("NVAPI"));
        let mut drs = Drs::open(api).expect("DRS");
        let exe = "tunecenter-selftest-dlss.exe";
        let name = drs.add_program(exe).expect("utworzenie profilu");
        let target = Target::Profile(name.clone());
        let mut checked = 0;
        for item in catalog::DLSS_ITEMS {
            let writes: Vec<(u32, u32)> = match &item.kind {
                // Ostatnia opcja listy (np. Preset O, Własna skala, Dynamiczne).
                Kind::Choice(opts) => opts.last().unwrap().writes.to_vec(),
                Kind::Number { id, max, .. } => vec![(*id, (*max).min(75))],
            };
            let r = drs.set_many(&target, &writes);
            println!(
                "{:<45} {:?} -> {:?}",
                item.en,
                writes,
                r.as_ref().map(|_| "OK")
            );
            r.expect("zapis");
            for (id, v) in &writes {
                let read = drs.get(&target, *id).unwrap().unwrap();
                assert_eq!((read.value, read.location), (Some(*v), Location::Current));
            }
            checked += 1;
        }
        drs.delete_profile(&name).expect("usunięcie profilu");
        println!("OK: {checked} ustawień DLSS zapisanych i odczytanych, profil usunięty");
    }
}
