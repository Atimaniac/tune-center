#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Tune Center: karta graficzna, procesor, wentylatory, czujniki i pobór mocy w jednym oknie.
//!
//! Parametry: `--minimized` (start zminimalizowany - przy włączonym zasobniku
//!            od razu do ikony przy zegarze; używany przez autostart),
//!            `--demo` (symulowana karta graficzna).

mod amd_tab;
mod autostart;
mod bench;
mod bench_tab;
mod bridge;
mod config;
mod cpu;
mod cpu_tab;
mod crash;
mod curve;
mod disks;
mod elevation;
mod fans;
mod game_watch;
mod gpu;
mod gpu_bench;
mod gpu_fans;
mod hags;
mod history;
mod i18n;
mod icons;
mod leds;
#[cfg(test)]
mod logo_tool;
mod nvcp;
mod nvdriver;
mod oled_fix;
mod power;
mod power_tab;
mod rebar;
mod sensors;
mod shaders;
mod skydimo;
mod theme;
mod tray;
mod tune;
mod update;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, RichText, Sense, Stroke};

use bridge::{Backend, Status};
use fans::FansTab;
use gpu::GpuTab;
use icons::Icon;
use theme::{accent, muted, warn};

/// Rozmiar logo w nagłówku i ABOUT (plik `assets/logo-header.rgba`, okrągłe
/// logo z makiety; ikona okna i zasobnika zostaje przy `icon-128.rgba`).
const LOGO_SIZE: usize = 160;

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
    Tune,
    Bench,
    Options,
    About,
}

/// Zakładki w kolejności przycisków w nagłówku.
const TABS: [(Tab, &str, Icon); 7] = [
    (Tab::Nvidia, "NVIDIA", Icon::Gpu),
    (Tab::Cpu, "CPU", Icon::Cpu),
    (Tab::Fans, "FAN", Icon::Fan),
    (Tab::Sensors, "SENSORS", Icon::Sensors),
    (Tab::Power, "POWER", Icon::Power),
    (Tab::Tune, "TUNE", Icon::Gauge),
    (Tab::Bench, "BENCH", Icon::Bench),
];

/// Pochylenie boków przycisku zakładki (piksele).
const TAB_SKEW: f32 = 10.0;

/// Przycisk zakładki: równoległobok z ikoną i napisem. Nieaktywny ma ciemne
/// tło i ciemnoniebieską obwódkę, aktywny świeci jasnym błękitem i ma
/// poświatę pod spodem.
fn tab_button(
    ui: &mut egui::Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    height: f32,
) -> egui::Response {
    let font = theme::tab_font(12.5);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let size = egui::vec2(text_width + 22.0 + 2.0 * TAB_SKEW + 2.0, height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let shape = [
            egui::pos2(rect.left() + TAB_SKEW, rect.top()),
            egui::pos2(rect.right(), rect.top()),
            egui::pos2(rect.right() - TAB_SKEW, rect.bottom()),
            egui::pos2(rect.left(), rect.bottom()),
        ];
        let light = theme::is_light();
        let hovered = response.hovered();
        let (fill, edge, text) = match (selected, hovered, light) {
            (true, _, false) => (Color32::from_rgb(12, 44, 88), theme::glow(), Color32::WHITE),
            (true, _, true) => (
                Color32::from_rgb(214, 234, 252),
                theme::glow(),
                Color32::BLACK,
            ),
            (false, true, false) => (
                Color32::from_rgb(12, 30, 56),
                Color32::from_rgb(52, 110, 180),
                Color32::WHITE,
            ),
            (false, false, false) => (
                Color32::from_rgb(8, 18, 34),
                Color32::from_rgb(30, 62, 104),
                Color32::from_rgb(214, 226, 242),
            ),
            (false, true, true) => (
                Color32::from_rgb(232, 242, 252),
                Color32::from_rgb(120, 170, 220),
                Color32::BLACK,
            ),
            (false, false, true) => (
                Color32::WHITE,
                Color32::from_rgb(170, 198, 230),
                Color32::BLACK,
            ),
        };
        let mut closed = shape.to_vec();
        closed.push(shape[0]);
        if selected {
            theme::glow_line(p, &closed, edge, 0.9);
            // Poświata pod aktywną zakładką.
            theme::radial_glow(
                p,
                egui::pos2(rect.center().x, rect.bottom() + 2.0),
                egui::pos2(rect.width() * 0.5, 12.0),
                theme::glow().gamma_multiply(0.5),
            );
        }
        p.add(egui::Shape::convex_polygon(
            shape.to_vec(),
            fill,
            Stroke::new(if selected { 1.5 } else { 1.0 }, edge),
        ));
        if selected {
            theme::glow_line(
                p,
                &[
                    egui::pos2(rect.left() + 6.0, rect.bottom()),
                    egui::pos2(rect.right() - TAB_SKEW - 6.0, rect.bottom()),
                ],
                theme::glow(),
                1.0,
            );
        }
        let content = text_width + 22.0;
        let x = rect.center().x - content / 2.0;
        let icon_color = if selected { theme::glow() } else { text };
        icons::paint(
            p,
            egui::Rect::from_center_size(
                egui::pos2(x + 8.0, rect.center().y),
                egui::vec2(16.0, 16.0),
            ),
            icon,
            icon_color,
        );
        p.text(
            egui::pos2(x + 22.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            text,
        );
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
    /// Czy program działa jako administrator.
    elevated: bool,
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
    /// Wyłączanie podświetlenia LED (RAM) i wynik ostatniej operacji.
    leds: leds::Control,
    leds_status: Option<(String, bool)>,
    leds_started: bool,
    /// Taśma LED Skydimo (USB).
    skydimo: skydimo::Control,
    /// Zakładka TUNE (opóźnienia i DPC).
    tune: tune::Tune,
    /// Zakładka BENCH (test wydajności i stress test).
    bench: bench_tab::BenchTab,
    /// Temperatura procesora z kafelków SENSORS (co sekundę).
    cpu_temp: Option<f32>,
    /// Raport awarii z poprzedniego uruchomienia (okno przy starcie).
    crash_report: Option<String>,
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
    /// Ostatnie sprawdzenie aktualizacji (automatycznie co 15 minut).
    update_checked: Instant,
    /// Aktualizacja sterownika NVIDIA i czas ostatniego sprawdzenia.
    nvdriver: nvdriver::Checker,
    nvdriver_checked: Option<Instant>,
    /// Użytkownik sam kliknął "Sprawdź teraz" - pokazujemy też błędy.
    nvdriver_asked: bool,
    /// Start programu (opóźnienie pierwszego sprawdzenia sterownika).
    started: Instant,
    /// Historia temperatur zakładki SENSORS (120 s).
    sensor_hist: history::History,
    /// Historia mocy zakładki POWER (120 s).
    power_hist: history::History,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let args: Vec<String> = std::env::args().collect();
        // Wczytanie ustawień ustala też język interfejsu.
        let mut fans = FansTab::new();
        {
            // Profile gier: obserwator procesów przełącza profil wentylatorów.
            let ctx = cc.egui_ctx.clone();
            fans.start_games(move || ctx.request_repaint());
        }
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
        // Taśma Skydimo: kolor z ustawień od razu po starcie (także z autostartu).
        let skydimo = {
            let c = &fans.config;
            let ctx = cc.egui_ctx.clone();
            skydimo::Control::start(
                skydimo::Setting {
                    on: c.skydimo_on,
                    color: c.skydimo_color,
                    brightness: c.skydimo_brightness,
                },
                move || ctx.request_repaint(),
            )
        };
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
                include_bytes!("../assets/logo-header.rgba"),
            ),
            egui::TextureOptions::LINEAR,
        );

        let updates = update::Checker::new();
        if fans.config.check_updates {
            let ctx = cc.egui_ctx.clone();
            updates.start(move || ctx.request_repaint());
        }

        let mut app = Self {
            tab: Tab::Nvidia,
            gpu: GpuTab::new(demo),
            nvcp: nvcp::NvcpTab::new(),
            cpu: cpu_tab::CpuTab::new(),
            fans,
            bridge,
            minimize: args.iter().any(|a| a == "--minimized"),
            elevated: elevation::is_elevated(),
            autostart: autostart::is_enabled(),
            // OLED fix: przywrócenie wpisu usuniętego przez instalację sterownika.
            notice: notice.or_else(oled_fix::reapply),
            tray,
            logo,
            gpu_fans,
            disks: disks::Disks::start(),
            leds: leds::Control::default(),
            leds_status: None,
            leds_started: false,
            skydimo,
            tune: {
                let ctx = cc.egui_ctx.clone();
                tune::Tune::new(cpu::cpu_info().name.contains("X3D"), move || {
                    ctx.request_repaint()
                })
            },
            bench: bench_tab::BenchTab::new(),
            cpu_temp: None,
            crash_report: crash::previous(),
            meter,
            gpu_power,
            meter_tick: Instant::now(),
            updates,
            update_asked: false,
            update_checked: Instant::now(),
            nvdriver: nvdriver::Checker::new(),
            nvdriver_checked: None,
            nvdriver_asked: false,
            started: Instant::now(),
            sensor_hist: history::History::default(),
            power_hist: history::History::default(),
        };
        app.open_from_args();
        // Weryfikacja bez klikania: test karty od razu po starcie.
        if std::env::args().any(|a| a == "--bench-gpu") {
            app.tab = Tab::Bench;
            let ctx = cc.egui_ctx.clone();
            app.bench.start_gpu(move || ctx.request_repaint());
        }
        app
    }

    /// `--tab <nvidia|cpu|fan|sensors|power|tune|options|about>` i
    /// `--page <info|clocks|curve|3d|dlss|display|color|gsync|custom|shaders>`:
    /// program otwiera się na wskazanej zakładce (np. ze skrótu na pulpicie).
    /// Okno z raportem awarii: poprzednie uruchomienie programu albo
    /// awaria procesu czujników w tym uruchomieniu.
    fn crash_window(&mut self, ctx: &egui::Context) {
        let bridge_crash = self.bridge.as_ref().ok().and_then(|b| b.crash_report());
        let (report, from_bridge) = match (&self.crash_report, &bridge_crash) {
            (Some(r), _) => (r.clone(), false),
            (None, Some((r, true))) => (r.clone(), true),
            _ => return,
        };
        let mut open = true;
        let mut close = false;
        let title = if from_bridge {
            crate::i18n::tr("Awaria procesu czujników", "Sensor process crash")
        } else {
            crate::i18n::tr(
                "Poprzednio program zakończył się błędem",
                "The program crashed last time",
            )
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(if from_bridge {
                        crate::i18n::tr(
                            "Proces czujników (sensor-bridge) zakończył się błędem i został uruchomiony ponownie. Szczegóły:",
                            "The sensor process (sensor-bridge) crashed and was restarted. Details:",
                        )
                    } else {
                        crate::i18n::tr(
                            "Raport zapisany przy awarii (plik tunecenter-crash.log obok programu):",
                            "Report saved at the crash (tunecenter-crash.log next to the program):",
                        )
                    })
                    .color(muted()),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    let mut text = report.as_str();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(crate::i18n::tr("Kopiuj", "Copy")).clicked() {
                        ui.ctx().copy_text(report.clone());
                    }
                    if ui
                        .button(crate::i18n::tr("Otwórz folder", "Open folder"))
                        .clicked()
                    {
                        if let Some(dir) = crash::folder() {
                            let _ = std::process::Command::new("explorer").arg(dir).spawn();
                        }
                    }
                    if theme::primary_button(ui, None, crate::i18n::tr("Zamknij", "Close"), true)
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        if !open || close {
            if from_bridge {
                if let Ok(b) = &self.bridge {
                    b.dismiss_crash();
                }
            } else {
                crash::acknowledge();
                self.crash_report = None;
            }
        }
    }

    fn open_from_args(&mut self) {
        let args: Vec<String> = std::env::args().collect();
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .map(|v| v.to_lowercase())
        };
        if let Some(tab) = value("--tab") {
            self.tab = match tab.as_str() {
                "cpu" => Tab::Cpu,
                "fan" => Tab::Fans,
                "sensors" => Tab::Sensors,
                "power" => Tab::Power,
                "tune" => Tab::Tune,
                "bench" => Tab::Bench,
                "options" => Tab::Options,
                "about" => Tab::About,
                _ => Tab::Nvidia,
            };
        }
        if let Some(page) = value("--page") {
            self.nvcp.open_page(&page);
            self.gpu.open_page(&page);
        }
    }

    /// Sprawdzanie aktualizacji co 15 minut (gdy włączone w OPTIONS), także
    /// przy oknie w zasobniku. Wynik pokazuje pasek stanu.
    /// Pasek stanu: dostępny sterownik NVIDIA i postęp instalacji.
    fn nvdriver_status(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use nvdriver::State;
        let (text, color) = match self.nvdriver.state() {
            State::Available { installed, latest } => {
                theme::vsep(ui, 18.0);
                ui.label(
                    RichText::new(crate::trf!(
                        "Nowy sterownik NVIDIA {} (masz {installed})",
                        "New NVIDIA driver {} (you have {installed})",
                        latest.version
                    ))
                    .size(12.5)
                    .color(accent()),
                );
                let button = egui::Button::new(
                    RichText::new(crate::trf!("Zainstaluj {}", "Install {}", latest.version))
                        .size(12.0),
                );
                if ui
                    .add(button)
                    .on_hover_text(crate::trf!(
                        "Pobiera paczkę ({}) z serwera NVIDIA i instaluje sam sterownik (grafika, audio HDMI, PhysX - bez NVIDIA App) w trybie cichym, bez restartu. Ekran mignie kilka razy - zamknij wcześniej gry.",
                        "Downloads the package ({}) from the NVIDIA server and silently installs the driver only (graphics, HDMI audio, PhysX - no NVIDIA App), without a restart. The screen will flicker a few times - close games first.",
                        latest.size
                    ))
                    .clicked()
                {
                    let ctx = ctx.clone();
                    self.nvdriver.install(move || ctx.request_repaint());
                }
                ui.hyperlink_to(
                    RichText::new(crate::i18n::tr("co nowego", "what's new")).size(12.0),
                    latest.details,
                );
                return;
            }
            State::Downloading { version, mb } => (
                crate::trf!(
                    "Pobieranie sterownika {version}: {mb} MB",
                    "Downloading driver {version}: {mb} MB"
                ),
                muted(),
            ),
            State::Extracting { version } => (
                crate::trf!(
                    "Rozpakowywanie sterownika {version}...",
                    "Unpacking driver {version}..."
                ),
                muted(),
            ),
            State::Installing { version } => (
                crate::trf!(
                    "Instalacja sterownika {version} - ekran może mignąć...",
                    "Installing driver {version} - the screen may flicker..."
                ),
                warn(),
            ),
            State::Done { version } => (
                crate::trf!(
                    "Zainstalowano sterownik NVIDIA {version}.",
                    "NVIDIA driver {version} installed."
                ),
                accent(),
            ),
            State::Failed(e) if self.nvdriver_asked => (
                crate::trf!("Sterownik NVIDIA: {e}", "NVIDIA driver: {e}"),
                warn(),
            ),
            _ => return,
        };
        theme::vsep(ui, 18.0);
        ui.label(RichText::new(text).size(12.5).color(color));
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    /// OPTIONS: sprawdzanie sterownika NVIDIA.
    fn nvdriver_options(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut check = self.fans.config.nv_driver_check;
        ui.horizontal(|ui| {
            if ui
                .checkbox(
                    &mut check,
                    crate::i18n::tr(
                        "Sprawdzaj aktualizacje sterownika NVIDIA (przy starcie i co 6 godzin)",
                        "Check for NVIDIA driver updates (at startup and every 6 hours)",
                    ),
                )
                .changed()
            {
                self.fans.config.nv_driver_check = check;
                self.fans.config.save();
            }
            let name = self.gpu.nvidia_name();
            if ui
                .add_enabled(
                    name.is_some(),
                    egui::Button::new(crate::i18n::tr("Sprawdź teraz", "Check now")),
                )
                .clicked()
            {
                if let Some(name) = name {
                    self.nvdriver_asked = true;
                    let ctx = ctx.clone();
                    self.nvdriver.check(name, move || ctx.request_repaint());
                }
            }
        });
        let state = match self.nvdriver.state() {
            nvdriver::State::Checking => {
                crate::i18n::tr("Sprawdzanie...", "Checking...").to_owned()
            }
            nvdriver::State::UpToDate { installed } => crate::trf!(
                "Masz najnowszy sterownik Game Ready ({installed}).",
                "You have the latest Game Ready driver ({installed})."
            ),
            nvdriver::State::Available { latest, .. } => crate::trf!(
                "Dostępny sterownik {} - przycisk instalacji jest na dole okna.",
                "Driver {} is available - the install button is at the bottom of the window.",
                latest.version
            ),
            nvdriver::State::Failed(e) => e,
            _ => String::new(),
        };
        option_note(
            ui,
            crate::i18n::tr(
                "Program pyta serwer NVIDIA o najnowszy sterownik Game Ready (WHQL) dla Twojej karty. Instaluje się tylko po kliknięciu: sam sterownik z audio HDMI i PhysX, bez NVIDIA App i telemetrii, w trybie cichym.",
                "The program asks the NVIDIA server for the latest Game Ready (WHQL) driver for your card. It installs only when you click: the driver with HDMI audio and PhysX, without NVIDIA App and telemetry, silently.",
            ),
        );
        if !state.is_empty() {
            ui.label(RichText::new(state).size(12.0).color(accent()));
        }
    }

    /// Sterownik NVIDIA: pierwsze sprawdzenie 30 s po starcie, potem co 6 h.
    fn nvdriver_logic(&mut self, ctx: &egui::Context) {
        const EVERY: Duration = Duration::from_secs(6 * 3600);
        const FIRST: Duration = Duration::from_secs(30);
        if !self.fans.config.nv_driver_check {
            return;
        }
        let due = match self.nvdriver_checked {
            None => self.started.elapsed() >= FIRST,
            Some(t) => t.elapsed() >= EVERY,
        };
        if due {
            self.nvdriver_checked = Some(Instant::now());
            if let Some(name) = self.gpu.nvidia_name() {
                let idle = matches!(
                    self.nvdriver.state(),
                    nvdriver::State::Idle
                        | nvdriver::State::UpToDate { .. }
                        | nvdriver::State::Failed(_)
                );
                if idle {
                    let ctx = ctx.clone();
                    self.nvdriver.check(name, move || ctx.request_repaint());
                }
            }
        }
        let wait = match self.nvdriver_checked {
            None => FIRST.saturating_sub(self.started.elapsed()),
            Some(t) => EVERY.saturating_sub(t.elapsed()),
        };
        ctx.request_repaint_after(wait);
    }

    fn update_logic(&mut self, ctx: &egui::Context) {
        const EVERY: Duration = Duration::from_secs(15 * 60);
        if !self.fans.config.check_updates {
            return;
        }
        let elapsed = self.update_checked.elapsed();
        if elapsed >= EVERY {
            let idle = matches!(
                self.updates.state(),
                update::State::Idle | update::State::UpToDate | update::State::Failed(_)
            );
            if idle {
                let ctx = ctx.clone();
                self.updates.start(move || ctx.request_repaint());
            }
            self.update_checked = Instant::now();
        }
        ctx.request_repaint_after(EVERY.saturating_sub(self.update_checked.elapsed()));
    }

    /// Podświetlenie LED: wyłączenie przy starcie i wyniki operacji w tle.
    fn leds_logic(&mut self, ctx: &egui::Context) {
        if !self.leds_started {
            self.leds_started = true;
            if self.fans.config.leds_off && self.elevated {
                let ctx = ctx.clone();
                self.leds.turn_off(move || ctx.request_repaint());
            }
        }
        let Some(outcome) = self.leds.take() else {
            return;
        };
        let errors = |r: &leds::Report| {
            if r.errors.is_empty() {
                String::new()
            } else {
                format!(" ({})", r.errors.join("; "))
            }
        };
        self.leds_status = Some(match outcome {
            leds::Outcome::Off(r) if r.devices.is_empty() => (
                crate::i18n::tr(
                    "Nie znaleziono sterowalnego podświetlenia pamięci RAM.",
                    "No controllable RAM lighting found.",
                )
                .to_string(),
                true,
            ),
            leds::Outcome::Off(r) => {
                // Zapamiętujemy tryby sprzed pierwszego wyłączenia.
                if self.fans.config.leds_saved.is_empty() {
                    self.fans.config.leds_saved = r
                        .devices
                        .iter()
                        .filter(|d| d.mode != 0 || d.direct != 0)
                        .map(|d| (d.addr, d.direct, d.mode))
                        .collect();
                    self.fans.config.save();
                }
                let names: Vec<String> = r
                    .devices
                    .iter()
                    .map(|d| format!("{} (0x{:02X})", d.name, d.addr))
                    .collect();
                (
                    crate::trf!(
                        "Podświetlenie RAM wyłączone: {}{}",
                        "RAM lighting turned off: {}{}",
                        names.join(", "),
                        errors(&r)
                    ),
                    !r.errors.is_empty(),
                )
            }
            leds::Outcome::Restored(r) => {
                self.fans.config.leds_saved.clear();
                self.fans.config.save();
                (
                    crate::trf!(
                        "Podświetlenie RAM przywrócone{}",
                        "RAM lighting restored{}",
                        errors(&r)
                    ),
                    !r.errors.is_empty(),
                )
            }
            leds::Outcome::Failed(e) => (crate::trf!("LED: {e}", "LED: {e}"), true),
        });
    }

    /// Zakładka OPTIONS: ustawienia całego programu.
    fn options_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        theme::heading(ui, Icon::Options, crate::i18n::tr("Opcje", "Options"));
        ui.add_space(2.0);

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
                crate::i18n::tr("Po zminimalizowaniu okno znika z paska zadań i zostaje ikona przy zegarze. Lewy klik na ikonie przywraca okno, tak samo jak ponowne uruchomienie programu.", "When minimized, the window leaves the taskbar and an icon stays next to the clock. Left-click the icon to bring the window back - starting the program again does the same."),
            );
            #[cfg(windows)]
            if to_tray && !tray::tray_host_present() {
                option_note(
                    ui,
                    crate::i18n::tr("Obecna powłoka Windows nie ma obszaru powiadomień - okno minimalizuje się zwyczajnie na pasek zadań.", "The current Windows shell has no notification area - the window minimizes to the taskbar as usual."),
                );
            }

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

            let mut off = self.fans.config.leds_off;
            let changed = ui
                .add_enabled(
                    self.elevated && !self.leds.busy(),
                    egui::Checkbox::new(&mut off, crate::i18n::tr("Wyłącz podświetlenie LED (pamięć RAM)", "Turn off LED lighting (RAM)")),
                )
                .on_disabled_hover_text(crate::i18n::tr(
                    "Wymaga uprawnień administratora (przycisk w pasku stanu) i sterownika PawnIO.",
                    "Requires administrator rights (button in the status bar) and the PawnIO driver.",
                ))
                .changed();
            if changed {
                self.fans.config.leds_off = off;
                self.fans.config.save();
                let ctx = ctx.clone();
                if off {
                    self.leds.turn_off(move || ctx.request_repaint());
                } else {
                    let saved = self.fans.config.leds_saved.clone();
                    self.leds.restore(saved, move || ctx.request_repaint());
                }
            }
            option_note(
                ui,
                crate::i18n::tr(
                    "Wyłącza podświetlenie modułów RAM z kontrolerem ENE (np. G.Skill Trident Z RGB) przez magistralę SMBus (moduł PawnIO SmbusI801). Moduły pamiętają swój efekt, więc program wyłącza je przy każdym starcie; odznaczenie przywraca poprzedni efekt. Płyta główna i karta graficzna nie są obsługiwane.",
                    "Turns off the lighting of RAM modules with an ENE controller (e.g. G.Skill Trident Z RGB) over the SMBus (PawnIO SmbusI801 module). The modules keep their own effect, so the program turns them off at every start; unticking restores the previous effect. The motherboard and graphics card are not supported.",
                ),
            );
            if self.leds.busy() {
                ui.label(RichText::new(crate::i18n::tr("Trwa...", "Working...")).size(12.0).color(muted()));
            } else if let Some((text, error)) = &self.leds_status {
                ui.label(RichText::new(text).size(12.0).color(if *error { warn() } else { accent() }));
            }

            self.skydimo_ui(ui);
            self.nvdriver_options(ui, ctx);

            let mut check = self.fans.config.check_updates;
            if ui
                .checkbox(&mut check, crate::i18n::tr("Sprawdzaj aktualizacje (przy starcie i co 15 minut)", "Check for updates (at startup and every 15 minutes)"))
                .changed()
            {
                self.fans.config.check_updates = check;
                self.fans.config.save();
            }
            option_note(
                ui,
                crate::i18n::tr(
                    "Przy starcie i potem co 15 minut program pyta GitHub o najnowsze wydanie i pokazuje na dole okna informację, że nowsza wersja jest gotowa do zainstalowania. Niczego nie pobiera sam.",
                    "At startup and then every 15 minutes the program asks GitHub for the latest release and shows a note at the bottom of the window that a newer version is ready to install. It downloads nothing on its own.",
                ),
            );

            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr("Cena energii za kWh:", "Energy price per kWh:"));
                let mut price = self.fans.config.energy_price;
                let mut currency = self.fans.config.energy_currency.clone();
                let a = ui.add(
                    egui::DragValue::new(&mut price)
                        .range(0.0..=100.0)
                        .speed(0.01)
                        .max_decimals(2),
                );
                let b = ui.add(egui::TextEdit::singleline(&mut currency).desired_width(48.0).char_limit(6));
                if a.changed() || b.changed() {
                    self.fans.config.energy_price = price;
                    self.fans.config.energy_currency = currency;
                    self.fans.config.save();
                }
            });
            option_note(
                ui,
                crate::i18n::tr(
                    "Do kosztu energii zużytej w sesji (zakładka POWER). 0 ukrywa koszt.",
                    "Used for the cost of energy used in the session (POWER tab). 0 hides the cost.",
                ),
            );
        });
    }

    /// Ustawienie taśmy Skydimo z konfiguracji.
    fn skydimo_setting(&self) -> skydimo::Setting {
        let c = &self.fans.config;
        skydimo::Setting {
            on: c.skydimo_on,
            color: c.skydimo_color,
            brightness: c.skydimo_brightness,
        }
    }

    /// OPTIONS: taśma LED Skydimo - włączenie, kolor, jasność i stan połączenia.
    fn skydimo_ui(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let cfg = &mut self.fans.config;
        changed |= ui
            .checkbox(
                &mut cfg.skydimo_on,
                crate::i18n::tr(
                    "Podświetlenie Skydimo (taśma LED na USB)",
                    "Skydimo lighting (USB LED strip)",
                ),
            )
            .changed();
        ui.add_enabled_ui(cfg.skydimo_on, |ui| {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr("Kolor:", "Color:"));
                changed |= egui::color_picker::color_edit_button_srgb(ui, &mut cfg.skydimo_color)
                    .changed();
                ui.add_space(12.0);
                ui.label(crate::i18n::tr("Jasność:", "Brightness:"));
                changed |= ui
                    .add(egui::Slider::new(&mut cfg.skydimo_brightness, 0..=100).suffix(" %"))
                    .changed();
            });
        });
        if changed {
            self.fans.config.save();
            self.skydimo.set(self.skydimo_setting());
        }
        option_note(
            ui,
            crate::i18n::tr(
                "Jeden kolor na całej taśmie (protokół Adalight przez USB CH340, VID 1A86 PID 7523). Program wysyła kolor co sekundę - taśma bez danych gaśnie, więc świeci, dopóki Tune Center działa (także po starcie z Windows). Aplikacja SkyDimo musi być zamknięta, bo zajmuje port. Odznaczenie gasi taśmę i zwalnia port.",
                "One color on the whole strip (Adalight protocol over USB CH340, VID 1A86 PID 7523). The program sends the color every second - the strip turns off without data, so it stays lit while Tune Center runs (also when started with Windows). The SkyDimo app must be closed because it holds the port. Unticking turns the strip off and frees the port.",
            ),
        );
        let (text, error) = match self.skydimo.status() {
            skydimo::Status::Off => return,
            skydimo::Status::Searching => (
                crate::i18n::tr("Łączenie z taśmą...", "Connecting to the strip...").to_owned(),
                false,
            ),
            skydimo::Status::Connected { port, model, leds } => (
                crate::trf!(
                    "Połączono: {port}, model {model}, {leds} diod.",
                    "Connected: {port}, model {model}, {leds} LEDs."
                ),
                false,
            ),
            skydimo::Status::Error(e) => (e, true),
        };
        ui.label(
            RichText::new(text)
                .size(12.0)
                .color(if error { warn() } else { accent() }),
        );
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
                            .font(theme::tab_font(26.0))
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
        theme::subheading(ui, Icon::About, title);
        ui.add_space(2.0);
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
        self.leds_logic(ctx);
        self.update_logic(ctx);
        self.nvdriver_logic(ctx);
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

        // Historia temperatur zakładki SENSORS (co sekundę, także w tle).
        if self.sensor_hist.due(now) {
            let snap = self.bridge.as_ref().ok().and_then(|b| b.state().0);
            let key = sensors::tiles(
                snap.as_ref().map(|s| s.temps.as_slice()).unwrap_or(&[]),
                self.gpu.reading(),
                &self.disks.list(),
            );
            let values: Vec<(String, f32)> = key
                .iter()
                .map(|t| (sensors::tile_key(t), t.value))
                .collect();
            self.sensor_hist.record(&values, now);
            // BENCH: temperatura i moc procesora (limit i wykres stress testu).
            self.cpu_temp = key.iter().find(|t| t.title == "Procesor").map(|t| t.value);
            let cpu_w = self.meter.lock().ok().and_then(|m| {
                m.rows()
                    .iter()
                    .find(|r| r.title == "Procesor")
                    .map(|r| r.watts)
            });
            let m = self.gpu.monitor();
            let gpu = gpu_bench::Sample {
                core_mhz: m.core_mhz,
                mem_mhz: m.mem_mhz,
                temp: m.details.core_precise_c.or(m.temp_c.map(|t| t as f32)),
                watts: m.power_w,
            };
            let limit = m.details.limit.clone();
            self.bench
                .observe(self.cpu_temp, cpu_w, gpu, limit.as_deref());
            if let Ok(m) = self.meter.lock() {
                let mut values: Vec<(String, f32)> = m
                    .rows()
                    .iter()
                    .map(|r| (power_tab::row_key(r), r.watts))
                    .collect();
                values.push((power_tab::TOTAL_KEY.to_owned(), m.total().watts));
                // Przed pierwszym odczytem licznik daje same zera - nie zapisujemy
                // ich, żeby wykres nie zaczynał się od skoku z 0 W.
                if values.iter().any(|(_, w)| *w > 0.0) {
                    self.power_hist.record(&values, now);
                }
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

        // --- tło całego okna (granat z poświatą); panele są przezroczyste ---
        theme::paint_background(ui.painter(), ctx.content_rect());

        // --- nagłówek: logo, nazwa i przyciski zakładek ---
        let header = egui::Panel::top("header")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 18,
                right: 18,
                top: 10,
                bottom: 16,
            }))
            .show(ui, |ui| {
                // W wąskim oknie przyciski przechodzą do kolejnego wiersza.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(2.0, 8.0);
                    let (logo_rect, _) =
                        ui.allocate_exact_size(egui::vec2(76.0, 76.0), Sense::hover());
                    theme::radial_glow(
                        ui.painter(),
                        logo_rect.center(),
                        egui::pos2(52.0, 52.0),
                        theme::glow().gamma_multiply(0.15),
                    );
                    egui::Image::from_texture(egui::load::SizedTexture::new(
                        self.logo.id(),
                        egui::vec2(76.0, 76.0),
                    ))
                    .paint_at(ui, logo_rect);
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("TUNE CENTER")
                            .font(theme::tab_font(22.0))
                            .color(theme::wordmark()),
                    );
                    ui.add_space(10.0);
                    let tabs_x = ui.cursor().left();
                    for (tab, label, icon) in TABS {
                        if tab_button(ui, label, icon, self.tab == tab, 38.0).clicked() {
                            self.tab = tab;
                        }
                    }
                    tabs_x
                })
                .inner
            });
        // Świecąca linia pod nagłówkiem, z uskokiem pod zakładkami jak w panelu.
        {
            let r = header.response.rect;
            let y = r.bottom() - 2.0;
            let x_step = (header.inner + 4.0).min(r.right() - 120.0);
            let p = ui.painter();
            theme::gradient_line(
                p,
                y - 10.0,
                r.left(),
                x_step - 16.0,
                1.5,
                Color32::TRANSPARENT,
                theme::glow().gamma_multiply(0.7),
            );
            theme::glow_line(
                p,
                &[
                    egui::pos2(x_step - 16.0, y - 10.0),
                    egui::pos2(x_step - 6.0, y),
                    egui::pos2(r.right() - 80.0, y),
                ],
                theme::glow(),
                0.7,
            );
            theme::gradient_line(
                p,
                y,
                r.right() - 80.0,
                r.right(),
                1.5,
                theme::glow().gamma_multiply(0.6),
                Color32::TRANSPARENT,
            );
        }

        self.crash_window(&ctx);

        // --- pasek stanu na samym dole okna ---
        let status_panel = egui::Panel::bottom("status")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(18, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (text, color) = match &status {
                        Status::Starting => (
                            crate::i18n::tr("Czujniki: uruchamianie...", "Sensors: starting...")
                                .to_owned(),
                            warn(),
                        ),
                        Status::Running => (
                            crate::i18n::tr("Czujniki: połączono", "Sensors: connected").to_owned(),
                            theme::ok(),
                        ),
                        Status::Error(e) => (e.clone(), theme::hot()),
                    };
                    theme::dot(ui, color);
                    ui.label(RichText::new(text).size(13.0).color(theme::text()));
                    self.nvdriver_status(ui, &ctx);
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

                    // Bez administratora część funkcji jest niedostępna -
                    // przycisk uruchamia program ponownie z pytaniem UAC.
                    if !self.elevated {
                        ui.separator();
                        let button = egui::Button::new(
                            RichText::new(crate::i18n::tr(
                                "Uruchom jako administrator",
                                "Run as administrator",
                            ))
                            .size(12.0),
                        );
                        if ui
                            .add(button)
                            .on_hover_text(crate::i18n::tr(
                                "Program działa bez uprawnień administratora. Karta graficzna, panel Nvidia, kompilacja shaderów, dyski i moc karty działają normalnie. Administrator jest potrzebny do: zakładki CPU, temperatury i mocy procesora, wentylatorów płyty głównej, zmiany HAGS i autostartu.",
                                "The program runs without administrator rights. The graphics card, Nvidia panel, shader compilation, disks and card power work normally. Administrator rights are needed for: the CPU tab, CPU temperature and power, motherboard fans, changing HAGS and autostart.",
                            ))
                            .clicked()
                        {
                            match elevation::relaunch_as_admin() {
                                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(e) => self.notice = Some(e),
                            }
                        }
                    }

                    // Prawy dolny róg: przycisk i wynik sprawdzania aktualizacji.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Od prawej: ABOUT, OPTIONS, potem "Sprawdź aktualizację"
                        // (czytane od lewej: aktualizacja, OPTIONS, ABOUT).
                        for (tab, label, icon) in [
                            (Tab::About, "ABOUT", Icon::About),
                            (Tab::Options, "OPTIONS", Icon::Options),
                        ] {
                            if tab_button(ui, label, icon, self.tab == tab, 34.0).clicked() {
                                self.tab = tab;
                            }
                        }
                        let state = self.updates.state();
                        let checking = matches!(
                            state,
                            update::State::Checking
                                | update::State::Downloading { .. }
                                | update::State::Launched
                        );
                        // UPDATE: ten sam styl co zakładki; podświetlony w trakcie sprawdzania.
                        if tab_button(ui, "UPDATE", Icon::Download, checking, 34.0)
                            .on_hover_text(crate::i18n::tr("Sprawdź aktualizację", "Check for updates"))
                            .clicked()
                            && !checking
                        {
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
                                ui.label(
                                    RichText::new(crate::trf!(
                                        "Wersja {version} jest gotowa do zainstalowania.",
                                        "Version {version} is ready to install."
                                    ))
                                    .size(12.0)
                                    .color(accent()),
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

        // Linia nad paskiem stanu.
        {
            let r = status_panel.response.rect;
            theme::gradient_line(
                ui.painter(),
                r.top(),
                r.left(),
                r.center().x,
                1.0,
                Color32::TRANSPARENT,
                theme::card_edge(),
            );
            theme::gradient_line(
                ui.painter(),
                r.top(),
                r.center().x,
                r.right(),
                1.0,
                theme::card_edge(),
                Color32::TRANSPARENT,
            );
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(22, 12)))
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
                Tab::Sensors => sensors::ui(
                    ui,
                    snap.as_ref(),
                    self.gpu.reading(),
                    &self.disks.list(),
                    &self.sensor_hist,
                ),
                Tab::Tune => self.tune.ui(ui, self.elevated),
                Tab::Bench => {
                    if self.bench.ui(
                        ui,
                        bench_tab::Histories {
                            cpu: &mut self.fans.config.bench_history,
                            gpu: &mut self.fans.config.gpu_bench_history,
                        },
                        self.cpu_temp,
                    ) {
                        self.fans.config.save();
                    }
                }
                Tab::Power => power_tab::ui(
                    ui,
                    &self.meter,
                    &self.power_hist,
                    power_tab::Limits {
                        cpu_w: self.cpu.power_limit_w(),
                        gpu_w: self.gpu.power_limit_w(),
                    },
                    (
                        self.fans.config.energy_price,
                        &self.fans.config.energy_currency,
                    ),
                ),
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
    // Proces kompilujący shadery (uruchamiany z kopii nazwanej jak gra) -
    // bez okna, kończy się po kompilacji.
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG) {
        std::process::exit(shaders::warm::run_host(&args[2..]));
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG_D3D11) {
        std::process::exit(shaders::warm::run_host_d3d11(&args[2..]));
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG_PROBE) {
        std::process::exit(shaders::warm::run_cache_probe());
    }
    // Raport awarii (panika i awarie natywne, np. w nvml.dll): plik obok
    // programu i okno z opisem błędu. Tylko dla programu z oknem - procesy
    // pomocnicze (kompilacja shaderów) kończą się kodem błędu.
    crash::install();
    // Test raportu: --crash-test panic | native.
    match args
        .iter()
        .position(|a| a == "--crash-test")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
    {
        Some("panic") => panic!("test raportu awarii (--crash-test panic)"),
        // SAFETY: celowe naruszenie dostępu - sprawdzenie filtra wyjątków.
        Some("native") => unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u32>(), 1) },
        _ => {}
    }
    // Jedna kopia programu: ponowne uruchomienie pokazuje okno już działającej
    // (także schowane do zasobnika, gdy powłoka nie pokazuje ikony).
    #[cfg(windows)]
    {
        let minimized = args.iter().any(|a| a == "--minimized");
        let restart = args.iter().any(|a| a == "--restart");
        if tray::activate_other_instance(!minimized, restart) {
            return Ok(());
        }
    }

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
