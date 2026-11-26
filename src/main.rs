#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// Tune Center
// Copyright (C) 2026 Atimaniac
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

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
mod diagnose;
mod disks;
mod dll;
mod elevation;
mod fans;
mod file_dialog;
mod game_watch;
mod gpu;
mod gpu_bench;
mod gpu_fans;
mod hags;
mod history;
mod hw;
mod hw_names;
mod i18n;
mod icons;
mod intel;
#[cfg(test)]
mod logo_tool;
mod lz4;
mod nvcp;
mod nvdriver;
mod oled_fix;
mod power;
mod power_tab;
mod radeon;
mod rebar;
mod rgb;
mod rgb_tab;
mod sensors;
mod shaders;
mod theme;
mod tray;
mod tune;
mod update;
mod zstd;

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

/// Rozmiar grafiki nagłówka motywu Neon City (`assets/theme-neon-city.rgba`).
const THEME_ART_SIZE: [usize; 2] = [190, 126];
/// Rozmiar grafiki dolnej krawędzi okna (`assets/theme-neon-city-bottom.rgba`).
const THEME_ART_BOTTOM_SIZE: [usize; 2] = [1060, 96];

/// Logo w kolorze aktualnego motywu (niebieskie logo przebarwione).
fn load_logo(ctx: &egui::Context) -> egui::TextureHandle {
    let rgba = theme::tint_logo(include_bytes!("../assets/logo-header.rgba"));
    ctx.load_texture(
        "logo",
        egui::ColorImage::from_rgba_unmultiplied([LOGO_SIZE, LOGO_SIZE], &rgba),
        egui::TextureOptions::LINEAR,
    )
}

/// Rozmiar grafiki napisu "TUNE CENTER" (`assets/wordmark.rgba`).
const WORDMARK_SIZE: [usize; 2] = [629, 60];
/// Wysokość napisu w nagłówku (punkty).
const WORDMARK_HEIGHT: f32 = 22.0;

/// Napis "TUNE CENTER" w kolorze aktualnego motywu (srebrny zostaje,
/// niebieski jest przebarwiany jak logo).
fn load_wordmark(ctx: &egui::Context) -> egui::TextureHandle {
    let rgba = theme::tint_wordmark(include_bytes!("../assets/wordmark.rgba"));
    ctx.load_texture(
        "wordmark",
        egui::ColorImage::from_rgba_unmultiplied(WORDMARK_SIZE, &rgba),
        egui::TextureOptions::LINEAR,
    )
}

/// Odstęp między klatkami animacji (20 klatek/s): kręcące się wentylatory,
/// pulsujące numery profili GPU.
pub const ANIMATION_FRAME: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Nvidia,
    Cpu,
    Fans,
    Sensors,
    Power,
    Tune,
    Rgb,
    Bench,
    Options,
    About,
}

/// Rozmiar okna w punktach interfejsu (na ekranie razy `UI_ZOOM`); okno ma
/// stały rozmiar albo jest zmaksymalizowane.
const WINDOW_SIZE: [f32; 2] = [1280.0, 869.0];

/// Rozmiar jednego przycisku okna (minimalizuj, maksymalizuj, zamknij).
const WINDOW_BUTTON: egui::Vec2 = egui::vec2(46.0, 26.0);

#[derive(Clone, Copy, PartialEq)]
enum WindowButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Przycisk okna rysowany w prawym górnym rogu nagłówka.
fn window_button(ui: &mut egui::Ui, rect: egui::Rect, kind: WindowButton, hint: &str) -> bool {
    let response = ui
        .interact(rect, egui::Id::unique(("window_button", kind as u8)), Sense::click())
        .on_hover_text(hint);
    let p = ui.painter();
    let hovered = response.hovered();
    if hovered {
        let fill = if kind == WindowButton::Close {
            Color32::from_rgb(196, 43, 28)
        } else {
            theme::accent().gamma_multiply(0.25)
        };
        p.rect_filled(rect, 0.0, fill);
    }
    let color = if hovered && kind == WindowButton::Close {
        Color32::WHITE
    } else if hovered {
        theme::text()
    } else {
        muted()
    };
    let stroke = Stroke::new(1.2, color);
    let c = rect.center();
    let h = 5.0;
    let square = |c: egui::Pos2, h: f32| {
        let (l, r, t, b) = (c.x - h, c.x + h, c.y - h, c.y + h);
        p.line_segment([egui::pos2(l, t), egui::pos2(r, t)], stroke);
        p.line_segment([egui::pos2(r, t), egui::pos2(r, b)], stroke);
        p.line_segment([egui::pos2(r, b), egui::pos2(l, b)], stroke);
        p.line_segment([egui::pos2(l, b), egui::pos2(l, t)], stroke);
    };
    match kind {
        WindowButton::Minimize => {
            p.line_segment([egui::pos2(c.x - h, c.y), egui::pos2(c.x + h, c.y)], stroke);
        }
        WindowButton::Maximize => square(c, h),
        WindowButton::Restore => {
            // Dwa nachodzące na siebie kwadraty: okno wróci do stałego rozmiaru.
            square(egui::pos2(c.x - 1.5, c.y + 1.5), h - 1.5);
            let (l, r, t, b) = (c.x - h + 3.0, c.x + h, c.y - h, c.y + h - 3.0);
            p.line_segment([egui::pos2(l, t), egui::pos2(r, t)], stroke);
            p.line_segment([egui::pos2(r, t), egui::pos2(r, b)], stroke);
        }
        WindowButton::Close => {
            p.line_segment([egui::pos2(c.x - h, c.y - h), egui::pos2(c.x + h, c.y + h)], stroke);
            p.line_segment([egui::pos2(c.x - h, c.y + h), egui::pos2(c.x + h, c.y - h)], stroke);
        }
    }
    response.clicked()
}

/// Zakładki w kolejności przycisków w nagłówku.
const TABS: [(Tab, &str, Icon); 8] = [
    (Tab::Nvidia, "NVIDIA", Icon::Gpu),
    (Tab::Cpu, "CPU", Icon::Cpu),
    (Tab::Fans, "FAN", Icon::Fan),
    (Tab::Sensors, "SENSORS", Icon::Sensors),
    (Tab::Power, "POWER", Icon::Power),
    (Tab::Tune, "TUNE", Icon::Gauge),
    (Tab::Rgb, "RGB", Icon::Rgb),
    (Tab::Bench, "BENCH", Icon::Bench),
];

/// Pochylenie boków przycisku zakładki (piksele).
const TAB_SKEW: f32 = 9.0;

/// Powiększenie całego interfejsu. Treść ma zwykły rozmiar czcionki;
/// większe są tylko przyciski, zakładki i nagłówek (`theme::b`).
const UI_ZOOM: f32 = 1.0;

/// Szerokość napisu zakładki.
fn tab_text_width(ui: &egui::Ui, label: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            label.to_owned(),
            theme::tab_font(theme::b(12.5)),
            Color32::WHITE,
        )
        .size()
        .x
}

/// Szerokość całej zakładki o napisie szerokości `text_width`.
fn tab_width(text_width: f32) -> f32 {
    text_width + theme::b(34.0) + 2.0 * TAB_SKEW
}

/// Napis zakładki. Zakładki karty i procesora pokazują model sprzętu (np.
/// "RTX 5070", "9070 XT", "i7-12700KF"); bez modelu - NVIDIA / RADEON / CPU.
fn tab_label(tab: Tab, label: &'static str) -> &'static str {
    match tab {
        Tab::Nvidia => hw_names::gpu_label().unwrap_or(match shaders::vendor() {
            Some(shaders::Vendor::Amd) => "RADEON",
            Some(shaders::Vendor::Intel) => "INTEL",
            _ => label,
        }),
        Tab::Cpu => hw_names::cpu_label().unwrap_or(label),
        _ => label,
    }
}

/// Przycisk zakładki: pochylona "pigułka" z ikoną i napisem. Nieaktywny ma
/// ciemne tło i ciemną obwódkę, aktywny jest wypełniony i świeci.
fn tab_button(
    ui: &mut egui::Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    height: f32,
) -> egui::Response {
    let font = theme::tab_font(theme::b(12.5));
    let text_width = tab_text_width(ui, label);
    let size = egui::vec2(tab_width(text_width), height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let shape = theme::slanted_rect(rect, TAB_SKEW, 7.0);
        let (fill, edge, text) = theme::tab_colors(selected, response.hovered());
        if selected {
            let mut closed = shape.clone();
            closed.push(shape[0]);
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
            shape,
            fill,
            Stroke::new(if selected { 1.5 } else { 1.0 }, edge),
        ));
        let content = text_width + theme::b(22.0);
        let x = rect.center().x - content / 2.0;
        let icon_color = if selected { theme::glow() } else { text };
        icons::paint(
            p,
            egui::Rect::from_center_size(
                egui::pos2(x + theme::b(8.0), rect.center().y),
                egui::Vec2::splat(theme::b(16.0)),
            ),
            icon,
            icon_color,
        );
        p.text(
            egui::pos2(x + theme::b(22.0), rect.center().y),
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
    /// Wysokość nagłówka z poprzedniej klatki: obszar przeciągania okna.
    header_height: f32,
    /// Czy rozmiar okna został już dopasowany do ekranu.
    fitted: bool,
    /// Czy istnieje zadanie uruchamiające program przy logowaniu do Windows.
    autostart: bool,
    /// Komunikat o nieudanej zmianie autostartu lub braku ikony w zasobniku.
    notice: Option<String>,
    /// Ikona w zasobniku systemowym; `None`, gdy nie udało się jej utworzyć.
    tray: Option<tray::Tray>,
    /// Logo programu pokazywane w nagłówku i w zakładce ABOUT.
    logo: egui::TextureHandle,
    /// Grafika napisu "TUNE CENTER" w nagłówku.
    wordmark: egui::TextureHandle,
    /// Grafika nagłówka motywu Neon City.
    theme_art: egui::TextureHandle,
    /// Grafika dolnej krawędzi okna w motywie Neon City.
    theme_art_bottom: egui::TextureHandle,
    /// Wentylatory kart graficznych sterowane wprost przez sterownik karty;
    /// `None`, gdy opcja jest wyłączona albo sterownik tego nie udostępnia.
    gpu_fans: Option<gpu_fans::GpuFans>,
    /// Zakładka RGB (oświetlenie).
    rgb: rgb_tab::RgbTab,
    /// Diagnostyka zgodności (OPTIONS).
    diagnose: diagnose::Panel,
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
        cc.egui_ctx.set_zoom_factor(UI_ZOOM);
        theme::apply(
            &cc.egui_ctx,
            fans.config.light_theme,
            fans.config.theme_style,
        );
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
        // Oświetlenie RGB: ustawienia stosowane od razu po starcie (także z autostartu).
        let elevated = elevation::is_elevated();
        let rgb = rgb_tab::RgbTab::start(&fans.config.rgb, elevated, &cc.egui_ctx);
        // Wentylatorami kart steruje zawsze sterownik karty.
        let gpu_fans = if !demo {
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

        // Czujniki w procesie programu (PawnIO, bez .NET).
        let bridge: Result<Backend, String> = Ok(Backend::spawn(
            cc.egui_ctx.clone(),
            fans.config.fans().clone(),
            meter.clone(),
            gpu_power.clone(),
            board,
            skip_kinds,
        ));

        let logo = load_logo(&cc.egui_ctx);
        let theme_art = cc.egui_ctx.load_texture(
            "theme-neon-city",
            egui::ColorImage::from_rgba_unmultiplied(
                THEME_ART_SIZE,
                include_bytes!("../assets/theme-neon-city.rgba"),
            ),
            egui::TextureOptions::LINEAR,
        );
        let theme_art_bottom = cc.egui_ctx.load_texture(
            "theme-neon-city-bottom",
            egui::ColorImage::from_rgba_unmultiplied(
                THEME_ART_BOTTOM_SIZE,
                include_bytes!("../assets/theme-neon-city-bottom.rgba"),
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
            header_height: 96.0,
            fitted: false,
            elevated,
            autostart: autostart::is_enabled(),
            // OLED fix: przywrócenie wpisu usuniętego przez instalację sterownika.
            notice: notice.or_else(oled_fix::reapply),
            tray,
            logo,
            wordmark: load_wordmark(&cc.egui_ctx),
            theme_art,
            theme_art_bottom,
            gpu_fans,
            disks: disks::Disks::start(),
            rgb,
            diagnose: diagnose::Panel::default(),
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
    /// Okno z raportem awarii poprzedniego uruchomienia programu.
    fn crash_window(&mut self, ctx: &egui::Context) {
        let Some(report) = self.crash_report.clone() else { return };
        let mut open = true;
        let mut close = false;
        let title = crate::i18n::tr(
            "Poprzednio program zakończył się błędem",
            "The program crashed last time",
        );
        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "Raport zapisany przy awarii (plik tunecenter-crash.log obok programu):",
                        "Report saved at the crash (tunecenter-crash.log next to the program):",
                    ))
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
            crash::acknowledge();
            self.crash_report = None;
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
                "rgb" => Tab::Rgb,
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

    /// Zakładka OPTIONS: ustawienia całego programu.
    fn options_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        theme::heading(ui, Icon::Options, crate::i18n::tr("Opcje", "Options"));
        ui.add_space(2.0);

        // Lista opcji jest wyższa niż okno - przewijana, z suwakiem po prawej.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| self.options_list(ui, ctx));
    }

    fn options_list(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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

            // Motyw wyglądu.
            let mut style = self.fans.config.theme_style;
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr("Motyw:", "Theme:"));
                egui::ComboBox::from_id_salt("theme_style")
                    .selected_text(style.name())
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for s in theme::Style::ALL {
                            ui.selectable_value(&mut style, s, s.name());
                        }
                    });
            });
            if style != self.fans.config.theme_style {
                self.fans.config.theme_style = style;
                self.fans.config.save();
                theme::apply(ctx, self.fans.config.light_theme, style);
                self.logo = load_logo(ctx);
                self.wordmark = load_wordmark(ctx);
            }
            option_note(
                ui,
                crate::i18n::tr(
                    "Tune - granat z niebieską poświatą; Neon Green - czerń i zieleń; Neon City - fiolet, cyjan i magenta z miastem nocą; Crimson - czerń i czerwień.",
                    "Tune - navy with a blue glow; Neon Green - black and green; Neon City - violet, cyan and magenta with a night city; Crimson - black and red.",
                ),
            );

            let tune = style == theme::Style::Tune;
            let mut light = self.fans.config.light_theme && tune;
            if ui
                .add_enabled(tune, egui::Checkbox::new(&mut light, crate::i18n::tr("Jasny motyw", "Light theme")))
                .changed()
            {
                self.fans.config.light_theme = light;
                self.fans.config.save();
                theme::apply(ctx, light, style);
                self.wordmark = load_wordmark(ctx);
            }
            option_note(
                ui,
                if tune {
                    crate::i18n::tr("Białe tło i czarny tekst zamiast ciemnego motywu.", "White background and black text instead of the dark theme.")
                } else {
                    crate::i18n::tr("Dostępne tylko w motywie Tune - pozostałe motywy są zawsze ciemne.", "Available only in the Tune theme - the other themes are always dark.")
                },
            );

            self.nvdriver_options(ui, ctx);

            {
                let elevated = self.elevated;
                let bridge = self.bridge.as_ref().map_or_else(
                    |e| (None, e.clone(), Vec::new()),
                    |b| {
                        let (snap, status) = b.state();
                        (snap, format!("{status:?}"), b.notes())
                    },
                );
                let rgb = self.rgb.snapshot();
                self.diagnose.ui(ui, ctx, move || diagnose::Live {
                    bridge: Some(bridge),
                    rgb: Some(rgb),
                    elevated,
                });
                ui.add_space(10.0);
            }

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

    /// Zakładka ABOUT: wersja, opis, autor i licencja, użyte składniki.
    fn about_ui(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                    self.logo.id(),
                    egui::vec2(96.0, 96.0),
                )));
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.add_space(22.0);
                    let ratio = WORDMARK_SIZE[0] as f32 / WORDMARK_SIZE[1] as f32;
                    ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                        self.wordmark.id(),
                        egui::vec2(30.0 * ratio, 30.0),
                    )));
                    ui.add_space(4.0);
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
                    crate::i18n::tr("NVIDIA - Overclock (zegary kart Nvidia i AMD, krzywa napięcie/częstotliwość, profile, HAGS, Resizable BAR), ustawienia 3D i DLSS, ekran, kolor i HDR, G-SYNC, OLED fix i aktualizacja sterownika. Karty AMD: ustawienia 3D, FSR i FreeSync (RADEON). Karty Intel: ustawienia 3D, ARC SYNC i ekran (INTEL), a starsze grafiki Intel - podstawowy panel z ustawieniami Windows.", "NVIDIA - Overclock (Nvidia and AMD card clocks, voltage/frequency curve, profiles, HAGS, Resizable BAR), 3D and DLSS settings, display, color and HDR, G-SYNC, OLED fix and driver updates. AMD cards: 3D settings, FSR and FreeSync (RADEON). Intel cards: 3D settings, ARC SYNC and display (INTEL), and older Intel graphics a basic panel with Windows settings."),
                    crate::i18n::tr("Kompilacja shaderów gier z góry, z samych plików gry: Unity, Unreal Engine (DX11 i DX12), Dagor, Source 2, REDengine 3, RAGE, ForzaTech, Santa Monica, Northlight, Luminous, Frostbite, CRYENGINE, Snowdrop, Nixxes, Stingray, Tank, Zouna, Dawn oraz gry DX12 z shaderami niosącymi root signature. Gry ze Steam, Epic, GOG, Ubisoft, EA, Xbox, Battle.net i Riot Games.", "Ahead-of-time game shader compilation from the game files alone: Unity, Unreal Engine (DX11 and DX12), Dagor, Source 2, REDengine 3, RAGE, ForzaTech, Santa Monica, Northlight, Luminous, Frostbite, CRYENGINE, Snowdrop, Nixxes, Stingray, Tank, Zouna, Dawn and DX12 games whose shaders carry their root signature. Games from Steam, Epic, GOG, Ubisoft, EA, Xbox, Battle.net and Riot Games."),
                    crate::i18n::tr("CPU - Intel: limity mocy, offsety napięcia, IccMax, mnożniki turbo. AMD Ryzen: Curve Optimizer i limity PBO.", "CPU - Intel: power limits, voltage offsets, IccMax, turbo ratios. AMD Ryzen: Curve Optimizer and PBO limits."),
                    crate::i18n::tr("FAN - wentylatory płyty i karty: ręcznie albo krzywą zależną od temperatury, profile, także osobne dla gier.", "FAN - motherboard and card fans: manual or a temperature-based curve, profiles, also per game."),
                    crate::i18n::tr("SENSORS i POWER - najważniejsze temperatury oraz pobór mocy procesora i karty z licznikiem energii i kosztem; zasilacz z USB (Corsair HXi/RMi, MSI Ai) i bateria laptopa pokazują pobór całego komputera.", "SENSORS and POWER - key temperatures and CPU and card power draw with an energy counter and cost; a USB power supply (Corsair HXi/RMi, MSI Ai) and a laptop battery show the whole computer's power draw."),
                    crate::i18n::tr("TUNE - ustawienia Windows wpływające na opóźnienia (przełączniki ON/OFF z kopią oryginałów).", "TUNE - Windows settings that affect latency (ON/OFF switches with a backup of the originals)."),
                    crate::i18n::tr("RGB - podświetlenie RAM, taśm Skydimo, urządzeń Logitech G, kart NVIDIA, płyt ASUS, Gigabyte i MSI oraz kontrolerów Corsair: kolor z jasnością, efekty (oddychanie, miganie, cykl kolorów, tęcza - tyle, ile ma dane urządzenie) z szybkością albo wyłączenie.", "RGB - lighting of RAM, Skydimo strips, Logitech G devices, NVIDIA cards, ASUS, Gigabyte and MSI boards and Corsair controllers: color with brightness, effects (breathing, flashing, color cycle, rainbow - as many as the device has) with speed, or off."),
                    crate::i18n::tr("BENCH - test wydajności procesora, stress test i test stabilności karty graficznej.", "BENCH - CPU performance test, stress test and graphics card stability test."),
                    crate::i18n::tr("OPTIONS - język, autostart, zasobnik, motywy (Tune, Neon Green, Neon City, Crimson) i diagnostyka zgodności ze zgłaszaniem na GitHubie.", "OPTIONS - language, autostart, tray, themes (Tune, Neon Green, Neon City, Crimson) and a compatibility check with GitHub reporting."),
                ],
            );
            license_card(ui, width);
            about_card(
                ui,
                width,
                crate::i18n::tr("Uwaga", "Warning"),
                &[
                    crate::i18n::tr("Program zmienia ustawienia procesora, karty graficznej, wentylatorów, podświetlenia i Windows. Zbyt agresywne wartości mogą zawiesić komputer albo pogorszyć chłodzenie.", "The program changes CPU, graphics card, fan, lighting and Windows settings. Overly aggressive values can freeze the computer or reduce cooling."),
                    crate::i18n::tr("Ustawienia procesora i karty graficznej działają do restartu komputera. Podświetlenie nie jest zapisywane w pamięci urządzeń - program ustawia je przy każdym starcie.", "CPU and graphics card settings last until the computer restarts. Lighting is not saved in device memory - the program sets it at every start."),
                    crate::i18n::tr("Używasz programu na własną odpowiedzialność.", "You use the program at your own risk."),
                ],
            );
            about_card(
                ui,
                width,
                crate::i18n::tr("Użyte składniki", "Components used"),
                &[
                    crate::i18n::tr("PawnIO i PawnIO.Modules (namazso) - dostęp do sprzętu; czujniki i wentylatory płyty obsługuje własny kod programu.", "PawnIO and PawnIO.Modules (namazso) - hardware access; motherboard sensors and fans are handled by the program's own code."),
                    crate::i18n::tr("NVIDIA NVAPI i NVML, AMD ADLX, Intel Graphics Control Library - karty graficzne.", "NVIDIA NVAPI and NVML, AMD ADLX, Intel Graphics Control Library - graphics cards."),
                    crate::i18n::tr("egui / eframe - interfejs.", "egui / eframe - user interface."),
                    crate::i18n::tr("Pełna lista i licencje: plik THIRD-PARTY.md obok programu.", "Full list and licenses: the THIRD-PARTY.md file next to the program."),
                ],
            );
            about_card(
                ui,
                width,
                crate::i18n::tr("Sterownik PawnIO jest opcjonalny", "The PawnIO driver is optional"),
                &[
                    crate::i18n::tr("Bez PawnIO działają: zakładka NVIDIA, temperatura, wentylatory i pobór mocy karty graficznej, temperatury dysków, TUNE oraz podświetlenie na USB.", "Without PawnIO these work: the NVIDIA tab, graphics card temperature, fans and power draw, disk temperatures, TUNE and USB lighting."),
                    crate::i18n::tr("PawnIO jest potrzebny do: temperatury i mocy procesora, temperatur płyty głównej i pamięci RAM, wentylatorów płyty głównej, zakładki CPU oraz podświetlenia pamięci RAM. Tych danych Windows nie udostępnia bez sterownika.", "PawnIO is needed for: CPU temperature and power, motherboard and RAM temperatures, motherboard fans, the CPU tab and RAM lighting. Windows does not expose that data without a driver."),
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

/// Adres repozytorium z kodem źródłowym.
const REPO_URL: &str = "https://github.com/Atimaniac/tune-center";

/// Karta ABOUT z autorem, odnośnikiem do GitHuba i notą licencyjną
/// (GPL-3.0-or-later, punkt 5d licencji: nota w interfejsie programu).
fn license_card(ui: &mut egui::Ui, width: f32) {
    use crate::i18n::tr;
    theme::card(14, 18).show(ui, |ui| {
        ui.set_width(width);
        theme::subheading(ui, Icon::About, tr("Autor i licencja", "Author and license"));
        ui.add_space(2.0);
        ui.label(RichText::new(tr("Autor: Atimaniac", "Author: Atimaniac")).color(theme::text()));
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tr("Kod źródłowy i zgłoszenia:", "Source code and issues:")).color(theme::text()));
            ui.hyperlink_to("github.com/Atimaniac/tune-center", REPO_URL);
        });
        ui.add_space(6.0);
        for line in [
            "Copyright (C) 2026 Atimaniac",
            tr(
                "Licencja: GNU General Public License w wersji 3 lub (według uznania) dowolnej nowszej (GPL-3.0-or-later).",
                "License: GNU General Public License, version 3 or (at your option) any later version (GPL-3.0-or-later).",
            ),
            tr(
                "Tune Center jest wolnym oprogramowaniem: możesz go rozpowszechniać i modyfikować na warunkach tej licencji.",
                "Tune Center is free software: you can redistribute it and modify it under the terms of this license.",
            ),
            tr(
                "Program jest udostępniany BEZ ŻADNEJ GWARANCJI, w tym bez domniemanej gwarancji przydatności handlowej albo przydatności do określonego celu.",
                "The program comes with ABSOLUTELY NO WARRANTY, including the implied warranty of merchantability or fitness for a particular purpose.",
            ),
            tr(
                "Pełna treść licencji: plik LICENSE obok programu.",
                "Full license text: the LICENSE file next to the program.",
            ),
        ] {
            ui.label(RichText::new(line).color(theme::text()));
            ui.add_space(2.0);
        }
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to(tr("Licencja", "License"), format!("{REPO_URL}/blob/main/LICENSE"));
            ui.label(RichText::new("·").color(muted()));
            ui.hyperlink_to(
                tr("Polityka prywatności", "Privacy policy"),
                format!("{REPO_URL}/blob/main/{}", tr("PRIVACY.pl.md", "PRIVACY.md")),
            );
            ui.label(RichText::new("·").color(muted()));
            ui.hyperlink_to("gnu.org/licenses", "https://www.gnu.org/licenses/");
        });
    });
    ui.add_space(12.0);
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
    /// `request_repaint` (wątek czujników budzi okno co sekundę).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.rgb.logic(&mut self.fans.config.rgb) {
            self.fans.config.save();
        }
        self.update_logic(ctx);
        self.nvdriver_logic(ctx);
        if self.minimize {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            self.minimize = false;
        }

        // Odczyty karty graficznej idą wprost ze sterownika (NVAPI / ADLX),
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
        self.nvcp.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);

        let (snap, status) = match &self.bridge {
            Ok(b) => b.state(),
            Err(e) => (None, Status::Error(e.clone())),
        };

        // --- tło całego okna (granat z poświatą); panele są przezroczyste ---
        theme::paint_background(
            ui.painter(),
            ctx.content_rect(),
            Some(theme::CityArt {
                top: &self.theme_art,
                bottom: &self.theme_art_bottom,
            }),
        );

        // Ekran mniejszy niż okno (np. 1366x768 albo skalowanie 150%): okno
        // dostaje rozmiar ekranu bez paska zadań, a zakładki się przewijają.
        if !self.fitted {
            if let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) {
                self.fitted = true;
                let room = egui::vec2(monitor.x, monitor.y - 48.0);
                let size = egui::vec2(WINDOW_SIZE[0].min(room.x), WINDOW_SIZE[1].min(room.y));
                if size.x < WINDOW_SIZE[0] || size.y < WINDOW_SIZE[1] {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                        (room.x - size.x) / 2.0,
                        (room.y - size.y) / 2.0,
                    )));
                }
            }
        }

        // --- belka okna: nagłówek zastępuje belkę Windows ---
        // Obszar przeciągania jest zgłaszany przed nagłówkiem, więc zakładki
        // i przyciski okna leżą nad nim i dostają swoje kliknięcia.
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        {
            let window = ctx.content_rect();
            let bar = egui::Rect::from_min_size(
                window.min,
                egui::vec2(window.width(), self.header_height),
            );
            let drag = ui.interact(bar, egui::Id::unique("window_drag"), Sense::click_and_drag());
            if drag.double_clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            } else if drag.drag_started_by(egui::PointerButton::Primary) && !maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        }

        // --- nagłówek: logo, nazwa i przyciski zakładek ---
        // Brakująca szerokość wiersza zakładek (gdy zakładki nie mieszczą
        // się w jednym wierszu nawet przy najmniejszym napisie).
        let mut missing = 0.0_f32;
        let header = egui::Panel::top("header")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 18,
                right: 14,
                top: 16,
                bottom: 6,
            }))
            .show(ui, |ui| {
                // Zakładki są w jednym wierszu. Gdy się nie mieszczą, okno się
                // poszerza; zawijają się tylko, gdy ekran jest za wąski.
                ui.horizontal_wrapped(|ui| {
                    const ROW: f32 = 54.0;
                    ui.spacing_mut().item_spacing = egui::vec2(3.0, 8.0);
                    let (logo_rect, _) =
                        ui.allocate_exact_size(egui::vec2(ROW, ROW), Sense::hover());
                    theme::radial_glow(
                        ui.painter(),
                        logo_rect.center(),
                        egui::pos2(46.0, 46.0),
                        theme::glow().gamma_multiply(0.15),
                    );
                    egui::Image::from_texture(egui::load::SizedTexture::new(
                        self.logo.id(),
                        egui::vec2(ROW, ROW),
                    ))
                    .paint_at(ui, logo_rect);
                    ui.add_space(8.0);
                    // Napis "TUNE CENTER" jako grafika. Zakładki muszą zmieścić
                    // się obok niego w tym samym wierszu, więc przy długich
                    // nazwach sprzętu napis jest odpowiednio mniejszy.
                    const GAP: f32 = 22.0;
                    let spacing = ui.spacing().item_spacing.x;
                    let tabs: f32 = TABS
                        .iter()
                        .map(|(tab, label, _)| {
                            tab_width(tab_text_width(ui, tab_label(*tab, label))) + spacing
                        })
                        .sum();
                    let ratio = WORDMARK_SIZE[0] as f32 / WORDMARK_SIZE[1] as f32;
                    let room = ui.available_width() - tabs - GAP - 2.0;
                    const MIN_WORDMARK: f32 = 110.0;
                    missing = (MIN_WORDMARK - room).max(0.0);
                    let width = (WORDMARK_HEIGHT * ratio).min(room).max(MIN_WORDMARK);
                    let size = egui::vec2(width, width / ratio);
                    let (name, _) =
                        ui.allocate_exact_size(egui::vec2(width + GAP, ROW), Sense::hover());
                    egui::Image::from_texture(egui::load::SizedTexture::new(
                        self.wordmark.id(),
                        size,
                    ))
                    .paint_at(
                        ui,
                        egui::Rect::from_min_size(
                            egui::pos2(name.left(), name.center().y - size.y / 2.0),
                            size,
                        ),
                    );
                    for (tab, label, icon) in TABS {
                        let label = tab_label(tab, label);
                        if tab_button(ui, label, icon, self.tab == tab, 34.0).clicked() {
                            self.tab = tab;
                        }
                    }
                });
            });
        self.header_height = header.response.rect.height();
        // Zakładki by się zawinęły: poszerzamy okno (najwyżej do szerokości
        // ekranu), żeby zostały w jednym wierszu.
        if missing > 0.5 && !maximized {
            let (inner, monitor) =
                ctx.input(|i| (i.viewport().inner_rect, i.viewport().monitor_size));
            if let Some(inner) = inner {
                let limit = monitor.map_or(f32::INFINITY, |m| m.x);
                let width = (inner.width() + missing.ceil() + 4.0).min(limit);
                if width > inner.width() + 0.5 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                        width,
                        inner.height(),
                    )));
                    // Okno nie może wyjść poza prawą krawędź ekranu.
                    if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
                        let overflow = outer.left() + width + (outer.width() - inner.width()) - limit;
                        if overflow > 0.0 {
                            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                egui::pos2((outer.left() - overflow).max(0.0), outer.top()),
                            ));
                        }
                    }
                }
            }
        }
        // Przyciski okna w prawym górnym rogu, nad wierszem zakładek.
        {
            let window = ctx.content_rect();
            let mut rect = egui::Rect::from_min_size(
                egui::pos2(window.right() - WINDOW_BUTTON.x, window.top()),
                WINDOW_BUTTON,
            );
            if window_button(ui, rect, WindowButton::Close, crate::i18n::tr("Zamknij", "Close")) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            rect = rect.translate(egui::vec2(-WINDOW_BUTTON.x, 0.0));
            let (kind, hint) = if maximized {
                (WindowButton::Restore, crate::i18n::tr("Przywróć rozmiar", "Restore size"))
            } else {
                (WindowButton::Maximize, crate::i18n::tr("Pełny ekran", "Full screen"))
            };
            if window_button(ui, rect, kind, hint) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            rect = rect.translate(egui::vec2(-WINDOW_BUTTON.x, 0.0));
            if window_button(ui, rect, WindowButton::Minimize, crate::i18n::tr("Minimalizuj", "Minimize")) {
                self.minimize = true;
            }
        }
        self.crash_window(&ctx);

        // --- pasek stanu na samym dole okna ---
        egui::Panel::bottom("status")
            .frame(theme::bar_frame())
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
                    // Długi komunikat błędu jest ucinany (całość w podpowiedzi),
                    // żeby nie wchodził pod przyciski po prawej.
                    ui.scope(|ui| {
                        ui.set_max_width(400.0);
                        ui.add(
                            egui::Label::new(RichText::new(&text).size(13.0).color(theme::text()))
                                .truncate(),
                        )
                        .on_hover_text(&text);
                    });
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
                        if theme::icon_button(
                            ui,
                            Icon::Shield,
                            crate::i18n::tr("Uruchom jako administrator", "Run as administrator"),
                            true,
                            false,
                        )
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
                            if theme::bar_button(ui, label, icon, self.tab == tab, false).clicked() {
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
                        if theme::bar_button(ui, "UPDATE", Icon::Download, checking, true)
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

        // Obwódka okna (bez belki Windows okno nie ma własnej ramki).
        if !maximized {
            let r = ctx.content_rect().shrink(0.5);
            let stroke = Stroke::new(1.0, theme::card_edge());
            let p = ui.painter();
            p.line_segment([r.left_top(), r.right_top()], stroke);
            p.line_segment([r.right_top(), r.right_bottom()], stroke);
            p.line_segment([r.right_bottom(), r.left_bottom()], stroke);
            p.line_segment([r.left_bottom(), r.left_top()], stroke);
        }

        egui::CentralPanel::default()
            .frame(theme::panel_frame())
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
                Tab::Rgb => self.rgb.ui(ui, &mut self.fans.config, self.elevated),
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

        // Animacja wentylatorów wymaga ciągłego odświeżania, ale 20 klatek/s
        // wystarcza (kąt liczony z `dt`; pomiar: 30 klatek/s = 11% rdzenia).
        // Bez limitu okno rysowało tyle klatek, ile pozwalał monitor. Pozostałym
        // zakładkom wystarczy raz na sekundę - nowe odczyty i ruch myszy i tak
        // budzą okno.
        if self.tab == Tab::Fans {
            ctx.request_repaint_after(ANIMATION_FRAME);
        } else {
            ctx.request_repaint_after(Duration::from_secs(1));
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
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG_UE12) {
        std::process::exit(shaders::warm::run_host_ue12(&args[2..]));
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some(shaders::warm12::HOST_ARG) {
        std::process::exit(shaders::warm12::run_host(&args[2..]));
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG_PROBE) {
        std::process::exit(shaders::warm::run_cache_probe());
    }
    #[cfg(windows)]
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG_LINK) {
        std::process::exit(shaders::warm::run_cache_link(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG_RESTORE) {
        std::process::exit(shaders::warm::run_cache_restore(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some(shaders::warm::HOST_ARG_DELETE) {
        std::process::exit(shaders::warm::run_cache_delete(&args[2..]));
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
    // Raport źródeł shaderów gier (tylko odczyt), bez okna.
    if let Some(i) = args.iter().position(|a| a == shaders::probe::ARG) {
        match shaders::probe::run(args.get(i + 1).map(String::as_str)) {
            Ok(path) => {
                println!("{}", path.display());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // Diagnostyka zgodności bez okna: raport obok programu (albo w %TEMP%).
    if args.iter().any(|a| a == "--diagnose") {
        // Raport po angielsku (do zgłoszenia na GitHubie).
        i18n::set_english(true);
        let report = diagnose::run(
            diagnose::Live {
                elevated: elevation::is_elevated(),
                ..Default::default()
            },
            |step| eprintln!("diagnose: {step}"),
        );
        match diagnose::save(&report) {
            Ok(path) => {
                println!("{}", path.display());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
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
            .with_inner_size([WINDOW_SIZE[0] * UI_ZOOM, WINDOW_SIZE[1] * UI_ZOOM])
            // Własna belka w nagłówku (przyciski okna rysuje `window_button`);
            // rozmiaru nie zmienia się myszą - układ jest liczony na ten rozmiar
            // albo na okno zmaksymalizowane.
            .with_decorations(false)
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        tray::WINDOW_TITLE,
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

/// Wątek z nazwą (widoczną w narzędziach diagnostycznych i przy pomiarach CPU).
pub fn spawn<F, T>(name: &str, f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("nie udało się uruchomić wątku")
}
