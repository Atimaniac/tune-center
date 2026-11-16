#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Tune Center: karta graficzna, procesor, wentylatory, czujniki i pobór mocy w jednym oknie.
//!
//! Parametry: `--minimized` (start zminimalizowany - przy włączonym zasobniku
//!            od razu do ikony przy zegarze; używany przez autostart),
//!            `--demo` (symulowana karta graficzna).

mod amd_tab;
mod autostart;
mod bridge;
mod config;
mod cpu;
mod cpu_tab;
mod curve;
mod disks;
mod fans;
mod gpu;
mod gpu_fans;
mod hags;
mod i18n;
mod nvcp;
mod power;
mod power_tab;
mod rebar;
mod sensors;
mod shaders;
mod theme;
mod tray;
mod update;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, RichText, Sense, Stroke, StrokeKind};

use bridge::{Backend, Status};
use fans::FansTab;
use gpu::GpuTab;
use theme::{accent, bg, muted, warn};

/// Rozmiar logo wczytywanego jako tekstura (plik `assets/icon-128.rgba`).
const LOGO_SIZE: usize = 128;

/// Odstęp między klatkami animacji (~30 klatek/s): kręcące się wentylatory,
/// pulsujące numery profili GPU.
pub const ANIMATION_FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Nvidia,
    Cpu,
    Fans,
    Sensors,
    Power,
    Options,
    About,
}

/// Zakładki w kolejności przycisków w nagłówku.
const TABS: [(Tab, &str); 7] = [
    (Tab::Nvidia, "NVIDIA"),
    (Tab::Cpu, "CPU"),
    (Tab::Fans, "FAN"),
    (Tab::Sensors, "SENSORS"),
    (Tab::Power, "POWER"),
    (Tab::Options, "OPTIONS"),
    (Tab::About, "ABOUT"),
];

/// Przycisk zakładki: zaokrąglony prostokąt z napisem. Nieaktywny jest
/// wypełniony ciemnym błękitem, aktywny ma tło okna i morskie obramowanie.
fn tab_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let font = theme::tab_font(15.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let size = egui::vec2((text_width + 28.0).max(92.0), 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let (fill, stroke, text) = if selected {
            (
                bg(),
                Stroke::new(2.0_f32, theme::tab_active()),
                theme::tab_active(),
            )
        } else if response.hovered() {
            (theme::tab_fill_hovered(), Stroke::NONE, Color32::WHITE)
        } else {
            (theme::tab_fill(), Stroke::NONE, Color32::WHITE)
        };
        // Obramowanie rysujemy do środka, żeby aktywny przycisk nie był większy.
        ui.painter().rect(
            rect,
            CornerRadius::same(9),
            fill,
            stroke,
            StrokeKind::Inside,
        );
        ui.painter()
            .text(rect.center(), Align2::CENTER_CENTER, label, font, text);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

struct App {
    tab: Tab,
    gpu: GpuTab,
    /// Panel sterowania Nvidia (publiczne NVAPI).
    nvcp: nvcp::NvcpTab,
    cpu: cpu_tab::CpuTab,
    fans: FansTab,
    /// Proces pomocniczy czujników i sterowania wentylatorami.
    bridge: Result<Backend, String>,
    minimize: bool,
    /// Czy istnieje zadanie uruchamiające program przy logowaniu do Windows.
    autostart: bool,
    /// Komunikat o nieudanej zmianie autostartu lub braku ikony w zasobniku.
    notice: Option<String>,
    /// Ikona w zasobniku systemowym; `None`, gdy nie udało się jej utworzyć.
    tray: Option<tray::Tray>,
    /// Logo programu pokazywane w nagłówku i w zakładce ABOUT.
    logo: egui::TextureHandle,
    /// Wentylatory kart graficznych sterowane wprost przez sterownik karty;
    /// `None`, gdy opcja jest wyłączona albo sterownik tego nie udostępnia.
    gpu_fans: Option<gpu_fans::GpuFans>,
    /// Temperatury dysków odczytywane przez API Windows.
    disks: disks::Disks,
    /// Licznik energii zakładki POWER.
    meter: power::SharedMeter,
    /// Ostatni odczyt mocy karty graficznej ze sterownika, dla licznika.
    gpu_power: power::GpuSlot,
    /// Kiedy interfejs sam ostatnio zaktualizował licznik (tylko gdy proces
    /// czujników nie działa).
    meter_tick: Instant,
    /// Sprawdzanie, czy jest nowsza wersja programu.
    updates: update::Checker,
    /// Ostatnie sprawdzenie uruchomił użytkownik przyciskiem - pokazujemy
    /// wtedy także wynik "masz najnowszą wersję" i błędy.
    update_asked: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let args: Vec<String> = std::env::args().collect();
        // Wczytanie ustawień ustala też język interfejsu.
        let fans = FansTab::new();
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, fans.config.light_theme);
        autostart::migrate();

        let icon = window_icon();
        let (tray, notice) =
            match tray::Tray::new(fans.config.tray, icon.rgba, icon.width, icon.height) {
                Ok(tray) => (Some(tray), None),
                Err(e) if cfg!(windows) => (
                    None,
                    Some(crate::trf!(
                        "Ikona w zasobniku niedostępna: {e}",
                        "Tray icon unavailable: {e}"
                    )),
                ),
                Err(_) => (None, None),
            };
        let meter: power::SharedMeter = Arc::new(Mutex::new(power::Meter::new(Instant::now())));
        let gpu_power: power::GpuSlot = Arc::default();
        let demo = args.iter().any(|a| a == "--demo");

        // Wentylatory kart graficznych: najpierw sterownik karty. Proces
        // czujników dowiaduje się, których kart ma nie dotykać.
        let board: gpu_fans::TempBoard = Arc::default();
        let gpu_fans = if fans.config.gpu_fans_direct && !demo {
            let ctx = cc.egui_ctx.clone();
            gpu_fans::GpuFans::start(fans.config.fans().clone(), board.clone(), move || {
                ctx.request_repaint()
            })
        } else {
            None
        };
        let skip_kinds = gpu_fans
            .as_ref()
            .map(|g| g.kinds().to_vec())
            .unwrap_or_default();

        let bridge = Backend::spawn(
            cc.egui_ctx.clone(),
            fans.config.fans().clone(),
            meter.clone(),
            gpu_power.clone(),
            board,
            skip_kinds,
        );

        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied(
                [LOGO_SIZE, LOGO_SIZE],
                include_bytes!("../assets/icon-128.rgba"),
            ),
            egui::TextureOptions::LINEAR,
        );

        let updates = update::Checker::new();
        if fans.config.check_updates {
            let ctx = cc.egui_ctx.clone();
            updates.start(move || ctx.request_repaint());
        }

        Self {
            tab: Tab::Nvidia,
            gpu: GpuTab::new(demo),
            nvcp: nvcp::NvcpTab::new(),
            cpu: cpu_tab::CpuTab::new(),
            fans,
            bridge,
            minimize: args.iter().any(|a| a == "--minimized"),
            autostart: autostart::is_enabled(),
            notice,
            tray,
            logo,
            gpu_fans,
            disks: disks::Disks::start(),
            meter,
            gpu_power,
            meter_tick: Instant::now(),
            updates,
            update_asked: false,
        }
    }

    /// Zakładka OPTIONS: ustawienia całego programu.
    fn options_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(
            RichText::new(crate::i18n::tr("Opcje", "Options"))
                .size(17.0)
                .strong()
                .color(theme::text()),
        );
        ui.add_space(6.0);

        let width = theme::grid_card_width(ui.available_width(), 380.0, 14.0, 1, 18.0).min(640.0);
        theme::card(14, 18).show(ui, |ui| {
            ui.set_width(width);

            // Język: zmiana działa od razu; napisy już wyświetlone w pasku
            // stanu i menu ikony w zasobniku zmienią się po ponownym uruchomieniu.
            ui.horizontal_wrapped(|ui| {
                ui.label("Język / Language:");
                let mut english = i18n::english();
                let changed = ui.radio_value(&mut english, false, "Polski").changed()
                    | ui.radio_value(&mut english, true, "English").changed();
                if changed {
                    i18n::set_english(english);
                    self.fans.config.language = Some(i18n::code().to_owned());
                    self.fans.config.save();
                }
            });
            option_note(
                ui,
                crate::i18n::tr("Menu ikony w zasobniku zmieni język po ponownym uruchomieniu programu.", "The tray icon menu changes language after the program is restarted."),
            );

            let mut enabled = self.autostart;
            if ui.checkbox(&mut enabled, crate::i18n::tr("Uruchamiaj z Windows", "Start with Windows")).changed() {
                match autostart::set(enabled) {
                    Ok(()) => {
                        self.autostart = enabled;
                        self.notice = None;
                    }
                    Err(e) => self.notice = Some(e),
                }
            }
            option_note(
                ui,
                crate::i18n::tr("Program startuje przy logowaniu, zminimalizowany, i od razu stosuje profil wentylatorów.", "The program starts at sign-in, minimized, and applies the fan profile right away."),
            );

            let mut to_tray = self.fans.config.tray;
            if ui
                .add_enabled(
                    self.tray.is_some(),
                    egui::Checkbox::new(&mut to_tray, crate::i18n::tr("Minimalizuj do zasobnika", "Minimize to tray")),
                )
                .changed()
            {
                self.fans.config.tray = to_tray;
                self.fans.config.save();
                if let Some(tray) = &self.tray {
                    tray.set_enabled(to_tray);
                }
            }
            option_note(
                ui,
                crate::i18n::tr("Po zminimalizowaniu okno znika z paska zadań i zostaje ikona przy zegarze. Lewy klik na ikonie przywraca okno.", "When minimized, the window leaves the taskbar and an icon stays next to the clock. Left-click the icon to bring the window back."),
            );

            let mut light = theme::is_light();
            if ui.checkbox(&mut light, crate::i18n::tr("Jasny motyw", "Light theme")).changed() {
                theme::apply(ctx, light);
                self.fans.config.light_theme = light;
                self.fans.config.save();
            }
            option_note(ui, crate::i18n::tr("Białe tło i czarny tekst zamiast ciemnego motywu.", "White background and black text instead of the dark theme."));

            let mut direct = self.fans.config.gpu_fans_direct;
            if ui
                .checkbox(&mut direct, crate::i18n::tr("Wentylatory karty graficznej przez sterownik karty", "Graphics card fans through the card driver"))
                .changed()
            {
                self.fans.config.gpu_fans_direct = direct;
                self.fans.config.save();
            }
            option_note(
                ui,
                crate::i18n::tr(
                    "Program steruje wentylatorami karty wprost przez sterownik Nvidii lub AMD. Po wyłączeniu robi to proces czujników (LibreHardwareMonitor). Zmiana działa po ponownym uruchomieniu programu; ustawienia wentylatora karty trzeba wtedy wybrać ponownie.",
                    "The program controls the card's fans directly through the Nvidia or AMD driver. When turned off, the sensor process (LibreHardwareMonitor) does it. The change takes effect after restarting the program; the card fan settings must then be chosen again.",
                ),
            );

            let mut check = self.fans.config.check_updates;
            if ui
                .checkbox(&mut check, crate::i18n::tr("Sprawdzaj aktualizacje przy starcie", "Check for updates at startup"))
                .changed()
            {
                self.fans.config.check_updates = check;
                self.fans.config.save();
            }
            option_note(
                ui,
                crate::i18n::tr(
                    "Przy starcie program pyta GitHub o najnowsze wydanie i pokazuje informację na dole okna, gdy jest nowsza wersja. Niczego nie pobiera sam.",
                    "At startup the program asks GitHub for the latest release and shows a note at the bottom of the window when a newer version exists. It downloads nothing on its own.",
                ),
            );
        });
    }

    /// Zakładka ABOUT: wersja, opis i użyte składniki.
    fn about_ui(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                    self.logo.id(),
                    egui::vec2(96.0, 96.0),
                )));
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new("TUNE CENTER")
                            .size(26.0)
                            .strong()
                            .color(theme::wordmark()),
                    );
                    ui.label(
                        RichText::new(crate::trf!("Wersja {}", "Version {}", env!("CARGO_PKG_VERSION"))).color(muted()),
                    );
                });
            });
            ui.add_space(12.0);

            let width = theme::grid_card_width(ui.available_width(), 380.0, 14.0, 1, 18.0).min(760.0);

            about_card(
                ui,
                width,
                crate::i18n::tr("O programie", "About"),
                &[
                    crate::i18n::tr("Strojenie i monitorowanie komputera w jednym oknie.", "PC tuning and monitoring in one window."),
                    crate::i18n::tr("GPU - zegary kart Nvidia i AMD, krzywa napięcie/częstotliwość kart Nvidia, profile, HAGS i Resizable BAR.", "GPU - Nvidia and AMD card clocks, voltage/frequency curve for Nvidia cards, profiles, HAGS and Resizable BAR."),
                    crate::i18n::tr("CPU - Intel: limity mocy, offsety napięcia, IccMax. AMD Ryzen: Curve Optimizer i limity PBO.", "CPU - Intel: power limits, voltage offsets, IccMax. AMD Ryzen: Curve Optimizer and PBO limits."),
                    crate::i18n::tr("FAN - sterowanie wentylatorami: ręcznie albo krzywą zależną od temperatury.", "FAN - fan control: manual or a temperature-based curve."),
                    crate::i18n::tr("SENSORS - najważniejsze temperatury podzespołów.", "SENSORS - key component temperatures."),
                    crate::i18n::tr("POWER - pobór mocy procesora i karty graficznej z licznikiem energii.", "POWER - CPU and graphics card power draw with an energy counter."),
                ],
            );
            about_card(
                ui,
                width,
                crate::i18n::tr("Uwaga", "Warning"),
                &[
                    crate::i18n::tr("Program zmienia ustawienia procesora, karty graficznej i wentylatorów. Zbyt agresywne wartości mogą zawiesić komputer albo pogorszyć chłodzenie.", "The program changes CPU, graphics card and fan settings. Overly aggressive values can freeze the computer or reduce cooling."),
                    crate::i18n::tr("Ustawienia procesora i karty graficznej działają do restartu komputera.", "CPU and graphics card settings last until the computer restarts."),
                    crate::i18n::tr("Używasz programu na własną odpowiedzialność.", "You use the program at your own risk."),
                ],
            );
            about_card(
                ui,
                width,
                crate::i18n::tr("Użyte składniki", "Components used"),
                &[
                    crate::i18n::tr("PawnIO i PawnIO.Modules (namazso) - dostęp do sprzętu.", "PawnIO and PawnIO.Modules (namazso) - hardware access."),
                    crate::i18n::tr("LibreHardwareMonitor - odczyt czujników i sterowanie wentylatorami.", "LibreHardwareMonitor - sensor readings and fan control."),
                    crate::i18n::tr("egui / eframe - interfejs.", "egui / eframe - user interface."),
                    crate::i18n::tr("Pełna lista i licencje: plik THIRD-PARTY.md obok programu.", "Full list and licenses: the THIRD-PARTY.md file next to the program."),
                ],
            );
            about_card(
                ui,
                width,
                crate::i18n::tr("Sterownik PawnIO jest opcjonalny", "The PawnIO driver is optional"),
                &[
                    crate::i18n::tr("Bez PawnIO działają: cała zakładka GPU, temperatura, wentylatory i pobór mocy karty graficznej oraz temperatury dysków.", "Without PawnIO these work: the whole GPU tab, graphics card temperature, fans and power draw, and disk temperatures."),
                    crate::i18n::tr("PawnIO jest potrzebny do: temperatury i mocy procesora, wentylatorów płyty głównej oraz zakładki CPU. Tych danych Windows nie udostępnia bez sterownika.", "PawnIO is needed for: CPU temperature and power, motherboard fans and the CPU tab. Windows does not expose that data without a driver."),
                ],
            );
        });
    }
}

/// Opis pod polem wyboru w zakładce OPTIONS.
fn option_note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(muted()));
    ui.add_space(10.0);
}

/// Karta z tytułem i listą akapitów w zakładce ABOUT.
fn about_card(ui: &mut egui::Ui, width: f32, title: &str, lines: &[&str]) {
    theme::card(14, 18).show(ui, |ui| {
        ui.set_width(width);
        ui.label(
            RichText::new(title)
                .size(16.0)
                .strong()
                .color(theme::text()),
        );
        ui.add_space(6.0);
        for line in lines {
            ui.label(RichText::new(*line).color(theme::text()));
            ui.add_space(2.0);
        }
    });
    ui.add_space(12.0);
}

impl eframe::App for App {
    /// Praca niezwiązana z rysowaniem. eframe woła to przed każdym `ui`,
    /// a gdy okno jest ukryte w zasobniku - zamiast `ui`, po każdym
    /// `request_repaint` (proces czujników budzi okno co sekundę).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.minimize {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            self.minimize = false;
        }

        // Odczyty karty graficznej idą wprost ze sterownika (NVAPI / ADL),
        // więc odświeżamy je niezależnie od otwartej zakładki.
        self.gpu.poll();
        let now = Instant::now();
        if let Ok(mut slot) = self.gpu_power.lock() {
            *slot = self.gpu.power().map(|p| (now, p));
        }
        // Bez procesu czujników licznik energii prowadzi sam interfejs,
        // z samego odczytu karty graficznej.
        if self.bridge.is_err() && now.duration_since(self.meter_tick) >= Duration::from_secs(1) {
            self.meter_tick = now;
            let driver = power::fresh(&self.gpu_power, now);
            let readings = power::with_driver_gpu(Vec::new(), driver.as_ref());
            if let Ok(mut meter) = self.meter.lock() {
                meter.update(&readings, now);
            }
        }

        // Opóźniony zapis ustawień wentylatorów działa także przy ukrytym oknie.
        self.fans
            .tick(self.bridge.as_ref().ok(), self.gpu_fans.as_ref());
        // Niepotwierdzona zmiana ekranu wraca po 15 s, nawet gdy okno jest
        // schowane w zasobniku.
        self.nvcp.tick();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);

        let (snap, status) = match &self.bridge {
            Ok(b) => b.state(),
            Err(e) => (None, Status::Error(e.clone())),
        };

        // --- nagłówek: logo, nazwa i przyciski zakładek ---
        egui::Panel::top("header")
            .frame(
                egui::Frame::NONE
                    .fill(bg())
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show(ui, |ui| {
                // W wąskim oknie przyciski przechodzą do kolejnego wiersza.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(4.0, 6.0);
                    ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                        self.logo.id(),
                        egui::vec2(64.0, 64.0),
                    )));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("TUNE CENTER")
                            .size(26.0)
                            .strong()
                            .color(theme::wordmark()),
                    );
                    ui.add_space(14.0);
                    for (tab, label) in TABS {
                        if tab_button(ui, label, self.tab == tab).clicked() {
                            self.tab = tab;
                        }
                    }
                });
            });

        // --- pasek stanu na samym dole okna ---
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::NONE
                    .fill(bg())
                    .inner_margin(egui::Margin::symmetric(20, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (text, color) = match &status {
                        Status::Starting => (
                            crate::i18n::tr("Czujniki: uruchamianie...", "Sensors: starting...")
                                .to_owned(),
                            muted(),
                        ),
                        Status::Running => (
                            crate::i18n::tr("Czujniki: połączono", "Sensors: connected").to_owned(),
                            accent(),
                        ),
                        Status::Error(e) => (e.clone(), warn()),
                    };
                    ui.label(RichText::new(text).size(12.0).color(color));
                    if let Some(notice) = &self.notice {
                        ui.separator();
                        ui.label(RichText::new(notice).size(12.0).color(warn()));
                    }
                    if let Some(e) = self.gpu_fans.as_ref().and_then(|g| g.error()) {
                        ui.separator();
                        ui.label(
                            RichText::new(crate::trf!("Wentylator karty: {e}", "Card fan: {e}"))
                                .size(12.0)
                                .color(warn()),
                        );
                    }

                    // Prawy dolny róg: przycisk i wynik sprawdzania aktualizacji.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let state = self.updates.state();
                        let checking = matches!(
                            state,
                            update::State::Checking
                                | update::State::Downloading { .. }
                                | update::State::Launched
                        );
                        let button = egui::Button::new(
                            RichText::new(crate::i18n::tr(
                                "Sprawdź aktualizację",
                                "Check for updates",
                            ))
                            .size(12.0),
                        );
                        if ui.add_enabled(!checking, button).clicked() {
                            self.update_asked = true;
                            let ctx = ctx.clone();
                            self.updates.start(move || ctx.request_repaint());
                        }
                        match state {
                            update::State::Idle => {}
                            update::State::Checking => {
                                ui.label(
                                    RichText::new(crate::i18n::tr("Sprawdzanie...", "Checking..."))
                                        .size(12.0)
                                        .color(muted()),
                                );
                            }
                            update::State::Available {
                                version,
                                url,
                                installer: Some(_),
                            } => {
                                // Układ od prawej: przycisk instalacji, potem odnośnik.
                                let install = egui::Button::new(
                                    RichText::new(crate::trf!(
                                        "Zainstaluj {version}",
                                        "Install {version}"
                                    ))
                                    .size(12.0)
                                    .color(warn()),
                                );
                                if ui
                                    .add(install)
                                    .on_hover_text(crate::i18n::tr(
                                        "Pobiera instalator z GitHuba, zamyka program i aktualizuje go. Ustawienia i profile zostają; program uruchomi się ponownie.",
                                        "Downloads the installer from GitHub, closes the program and updates it. Settings and profiles are kept; the program restarts.",
                                    ))
                                    .clicked()
                                {
                                    let ctx = ctx.clone();
                                    self.updates
                                        .install(crate::i18n::code(), move || ctx.request_repaint());
                                }
                                ui.hyperlink_to(
                                    RichText::new(crate::i18n::tr("co nowego", "what's new"))
                                        .size(12.0),
                                    url,
                                );
                            }
                            update::State::Available {
                                version,
                                url,
                                installer: None,
                            } => {
                                ui.hyperlink_to(
                                    RichText::new(crate::trf!(
                                        "Dostępna wersja {version} - pobierz",
                                        "Version {version} available - download"
                                    ))
                                    .size(12.0)
                                    .color(warn()),
                                    url,
                                );
                            }
                            update::State::Downloading { version } => {
                                ui.label(
                                    RichText::new(crate::trf!(
                                        "Pobieranie wersji {version}...",
                                        "Downloading version {version}..."
                                    ))
                                    .size(12.0)
                                    .color(muted()),
                                );
                            }
                            update::State::Launched => {
                                ui.label(
                                    RichText::new(crate::i18n::tr(
                                        "Aktualizowanie - program zostanie zamknięty...",
                                        "Updating - the program will close...",
                                    ))
                                    .size(12.0)
                                    .color(accent()),
                                );
                                // Zwykłe zamknięcie: wentylatory i karta wracają
                                // do automatu, zanim instalator podmieni pliki.
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            update::State::InstallFailed { url, error } => {
                                ui.hyperlink_to(
                                    RichText::new(crate::i18n::tr(
                                        "pobierz ręcznie",
                                        "download manually",
                                    ))
                                    .size(12.0),
                                    url,
                                );
                                ui.label(RichText::new(error).size(12.0).color(warn()));
                            }
                            update::State::UpToDate if self.update_asked => {
                                ui.label(
                                    RichText::new(crate::trf!(
                                        "Masz najnowszą wersję ({})",
                                        "You have the latest version ({})",
                                        env!("CARGO_PKG_VERSION")
                                    ))
                                    .size(12.0)
                                    .color(accent()),
                                );
                            }
                            update::State::Failed(e) if self.update_asked => {
                                ui.label(
                                    RichText::new(crate::trf!("Aktualizacje: {e}", "Updates: {e}"))
                                        .size(12.0)
                                        .color(muted()),
                                );
                            }
                            update::State::UpToDate | update::State::Failed(_) => {}
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(bg())
                    .inner_margin(egui::Margin::symmetric(20, 10)),
            )
            .show(ui, |ui| match self.tab {
                Tab::Nvidia => self.nvcp.ui(ui, &mut self.gpu, snap.as_ref()),
                Tab::Cpu => self.cpu.ui(ui),
                Tab::Fans => {
                    // Lista z wentylatorami kart ze sterownika zamiast tych
                    // samych wentylatorów z procesu czujników.
                    let merged = match &self.gpu_fans {
                        Some(g) => Some(g.merge(snap.clone())),
                        None => snap.clone(),
                    };
                    self.fans.ui(ui, merged.as_ref(), dt)
                }
                Tab::Sensors => {
                    sensors::ui(ui, snap.as_ref(), self.gpu.reading(), &self.disks.list())
                }
                Tab::Power => power_tab::ui(ui, &self.meter),
                Tab::Options => self.options_ui(ui, &ctx),
                Tab::About => self.about_ui(ui),
            });

        self.fans
            .tick(self.bridge.as_ref().ok(), self.gpu_fans.as_ref());

        // Animacja wentylatorów wymaga ciągłego odświeżania, ale ~30 klatek/s
        // wystarcza (kąt liczony z `dt`); bez limitu okno rysowało tyle klatek,
        // ile pozwalał monitor (ponad 200/s i cały rdzeń procesora).
        // Pozostałym zakładkom wystarczy odświeżenie dwa razy na sekundę.
        if self.tab == Tab::Fans {
            ctx.request_repaint_after(ANIMATION_FRAME);
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.fans.config.save();
    }
}

/// Ikona okna (pasek tytułu, pasek zadań): 128x128 pikseli, surowe RGBA.
fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon-128.rgba").to_vec(),
        width: 128,
        height: 128,
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(tray::WINDOW_TITLE)
            .with_icon(window_icon())
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([420.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        tray::WINDOW_TITLE,
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
