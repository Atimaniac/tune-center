//! Wspólne kolory i elementy wyglądu, w dwóch motywach: ciemnym (granat z
//! niebieską poświatą) i jasnym. Tu są też wspólne elementy interfejsu:
//! tło okna, karty z poświatą, nagłówki sekcji z ikoną, wyróżnione przyciski,
//! przyciski podzakładek i notatki z ikoną "i".

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

use eframe::egui::{
    self, pos2, vec2, Color32, CornerRadius, Mesh, Painter, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind,
};

use crate::icons::{self, Icon};

/// Motyw wyglądu programu.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Style {
    /// Granat z niebieską poświatą (jasny wariant w opcji „Jasny motyw”).
    #[default]
    Tune,
    /// Czerń z jaskrawą zielenią i ukośnymi smugami.
    NeonGreen,
    /// Fiolet, cyjan i magenta, miasto nocą w nagłówku.
    NeonCity,
    /// Czerń z czerwienią i ukośnymi smugami.
    Crimson,
}

impl Style {
    pub const ALL: [Style; 4] = [
        Style::Tune,
        Style::NeonGreen,
        Style::NeonCity,
        Style::Crimson,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Style::Tune => "Tune",
            Style::NeonGreen => "Neon Green",
            Style::NeonCity => "Neon City",
            Style::Crimson => "Crimson",
        }
    }

    fn index(self) -> u8 {
        self as u8
    }

    fn from_index(i: u8) -> Self {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

/// Aktualny motyw; kolory poniżej czytają go przy każdym rysowaniu.
static LIGHT: AtomicBool = AtomicBool::new(false);
static STYLE: AtomicU8 = AtomicU8::new(0);

pub fn style() -> Style {
    Style::from_index(STYLE.load(Ordering::Relaxed))
}

/// Jasny wariant - tylko w motywie Tune.
pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed) && style() == Style::Tune
}

fn pick(dark: Color32, light: Color32) -> Color32 {
    if is_light() {
        light
    } else {
        dark
    }
}

/// Kolory jednego motywu.
struct Palette {
    bg: Color32,
    card_fill: Color32,
    card_edge: Color32,
    well: Color32,
    track: Color32,
    accent: Color32,
    glow: Color32,
    /// Drugi kolor (napis "CENTER", wybrana podzakładka w Neon City).
    accent2: Color32,
    muted: Color32,
    wordmark: Color32,
    text: Color32,
    window: Color32,
    faint: Color32,
    /// Przyciski i pola: zwykłe, pod kursorem, wciśnięte, obwódka.
    widget: [Color32; 4],
    /// Wyróżniony przycisk: wyłączony, pod kursorem, zwykły.
    primary: [Color32; 3],
    /// Podzakładka: wybrana, pod kursorem.
    sub_tab: [Color32; 2],
    /// Zakładka: tło wybranej, pod kursorem, zwykłej; obwódka pod kursorem,
    /// zwykła; tekst zwykłej.
    tab: [Color32; 6],
    /// Poświaty tła (dwie barwy).
    bg_glow: [Color32; 2],
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        (r as u16 * a as u16 / 255) as u8,
        (g as u16 * a as u16 / 255) as u8,
        (b as u16 * a as u16 / 255) as u8,
        a,
    )
}

const TUNE_DARK: Palette = Palette {
    bg: rgb(5, 10, 20),
    card_fill: rgba(9, 19, 36, 232),
    card_edge: rgb(30, 66, 112),
    well: rgb(5, 12, 24),
    track: rgb(22, 40, 66),
    accent: rgb(56, 182, 255),
    glow: rgb(64, 190, 255),
    accent2: rgb(64, 190, 255),
    muted: rgb(180, 194, 216),
    wordmark: rgb(214, 232, 252),
    text: Color32::WHITE,
    window: rgb(8, 17, 32),
    faint: rgb(10, 22, 40),
    widget: [
        rgb(10, 24, 46),
        rgb(16, 40, 74),
        rgb(20, 52, 96),
        rgb(36, 82, 138),
    ],
    primary: [rgb(10, 20, 36), rgb(16, 48, 92), rgb(10, 30, 60)],
    sub_tab: [rgb(12, 36, 70), rgb(10, 24, 46)],
    tab: [
        rgb(12, 44, 88),
        rgb(12, 30, 56),
        rgb(8, 18, 34),
        rgb(52, 110, 180),
        rgb(30, 62, 104),
        rgb(214, 226, 242),
    ],
    bg_glow: [rgb(20, 110, 230), rgb(20, 110, 230)],
};

const TUNE_LIGHT: Palette = Palette {
    bg: rgb(244, 247, 252),
    card_fill: rgba(255, 255, 255, 240),
    card_edge: rgb(168, 198, 232),
    well: rgb(233, 240, 249),
    track: rgb(206, 218, 232),
    accent: rgb(0, 108, 196),
    glow: rgb(30, 140, 230),
    accent2: rgb(30, 140, 230),
    muted: rgb(78, 90, 110),
    wordmark: rgb(20, 60, 110),
    text: Color32::BLACK,
    window: Color32::WHITE,
    faint: rgb(238, 243, 250),
    widget: [
        rgb(236, 243, 252),
        rgb(220, 234, 250),
        rgb(204, 224, 248),
        rgb(150, 186, 226),
    ],
    primary: [rgb(236, 240, 246), rgb(214, 232, 252), rgb(232, 242, 254)],
    sub_tab: [rgb(220, 236, 252), rgb(234, 242, 252)],
    tab: [
        rgb(214, 234, 252),
        rgb(232, 242, 252),
        Color32::WHITE,
        rgb(120, 170, 220),
        rgb(170, 198, 230),
        Color32::BLACK,
    ],
    bg_glow: [rgb(20, 110, 230), rgb(20, 110, 230)],
};

const NEON_GREEN: Palette = Palette {
    bg: rgb(5, 8, 5),
    card_fill: rgba(9, 14, 8, 235),
    card_edge: rgb(44, 78, 20),
    well: rgb(4, 7, 3),
    track: rgb(28, 46, 16),
    accent: rgb(140, 255, 30),
    glow: rgb(128, 250, 36),
    accent2: rgb(160, 255, 50),
    muted: rgb(182, 200, 172),
    wordmark: rgb(236, 246, 230),
    text: Color32::WHITE,
    window: rgb(9, 15, 8),
    faint: rgb(12, 20, 9),
    widget: [
        rgb(12, 20, 8),
        rgb(22, 38, 12),
        rgb(30, 54, 14),
        rgb(66, 118, 26),
    ],
    primary: [rgb(10, 16, 8), rgb(30, 60, 10), rgb(16, 32, 8)],
    sub_tab: [rgb(22, 46, 10), rgb(13, 22, 9)],
    tab: [
        rgb(26, 58, 8),
        rgb(16, 30, 10),
        rgb(9, 14, 7),
        rgb(90, 160, 34),
        rgb(44, 78, 20),
        rgb(222, 236, 212),
    ],
    bg_glow: [rgb(90, 210, 20), rgb(60, 160, 10)],
};

const NEON_CITY: Palette = Palette {
    bg: rgb(9, 6, 24),
    card_fill: rgba(16, 11, 40, 228),
    card_edge: rgb(72, 46, 124),
    well: rgb(8, 5, 22),
    track: rgb(40, 28, 78),
    accent: rgb(34, 227, 255),
    glow: rgb(34, 227, 255),
    accent2: rgb(255, 63, 184),
    muted: rgb(194, 184, 224),
    wordmark: rgb(242, 238, 255),
    text: Color32::WHITE,
    window: rgb(14, 10, 34),
    faint: rgb(18, 12, 42),
    widget: [
        rgb(20, 14, 48),
        rgb(34, 22, 74),
        rgb(48, 28, 96),
        rgb(96, 62, 156),
    ],
    primary: [rgb(16, 11, 38), rgb(62, 20, 84), rgb(36, 18, 66)],
    sub_tab: [rgb(70, 18, 72), rgb(22, 15, 48)],
    tab: [
        rgb(40, 30, 92),
        rgb(28, 20, 62),
        rgb(16, 11, 36),
        rgb(124, 84, 206),
        rgb(72, 46, 124),
        rgb(228, 222, 246),
    ],
    bg_glow: [rgb(255, 63, 184), rgb(34, 180, 255)],
};

const CRIMSON: Palette = Palette {
    bg: rgb(9, 4, 4),
    card_fill: rgba(18, 8, 8, 235),
    card_edge: rgb(86, 26, 22),
    well: rgb(7, 3, 3),
    track: rgb(52, 20, 18),
    accent: rgb(255, 64, 44),
    glow: rgb(255, 52, 34),
    accent2: rgb(255, 64, 44),
    muted: rgb(212, 184, 180),
    wordmark: rgb(246, 236, 234),
    text: Color32::WHITE,
    window: rgb(16, 8, 8),
    faint: rgb(20, 10, 9),
    widget: [
        rgb(24, 10, 9),
        rgb(42, 16, 14),
        rgb(60, 20, 16),
        rgb(124, 38, 30),
    ],
    primary: [rgb(18, 8, 8), rgb(72, 16, 12), rgb(40, 12, 10)],
    sub_tab: [rgb(64, 16, 12), rgb(24, 10, 9)],
    tab: [
        rgb(74, 14, 10),
        rgb(38, 12, 10),
        rgb(16, 8, 7),
        rgb(176, 52, 38),
        rgb(86, 26, 22),
        rgb(238, 224, 222),
    ],
    bg_glow: [rgb(220, 30, 20), rgb(160, 20, 14)],
};

fn pal() -> &'static Palette {
    match style() {
        Style::Tune if is_light() => &TUNE_LIGHT,
        Style::Tune => &TUNE_DARK,
        Style::NeonGreen => &NEON_GREEN,
        Style::NeonCity => &NEON_CITY,
        Style::Crimson => &CRIMSON,
    }
}

/// Tło okna.
pub fn bg() -> Color32 {
    pal().bg
}

/// Główny kolor tekstu: biały na ciemnym tle, czarny na jasnym.
pub fn text() -> Color32 {
    pal().text
}

pub fn card_fill() -> Color32 {
    pal().card_fill
}

pub fn card_edge() -> Color32 {
    pal().card_edge
}

/// Tło wykresów, pól edycji i wnętrza wentylatora.
pub fn well() -> Color32 {
    pal().well
}

/// Linie siatki i tła pasków.
pub fn track() -> Color32 {
    pal().track
}

pub fn accent() -> Color32 {
    pal().accent
}

/// Kolor poświaty (obramowania aktywnych elementów).
pub fn glow() -> Color32 {
    pal().glow
}

/// Drugi kolor motywu (w Tune taki sam jak poświata).
pub fn accent2() -> Color32 {
    pal().accent2
}

/// Tekst pomocniczy.
pub fn muted() -> Color32 {
    pal().muted
}

pub fn warn() -> Color32 {
    pick(
        Color32::from_rgb(255, 176, 64),
        Color32::from_rgb(176, 98, 0),
    )
}

pub fn hot() -> Color32 {
    pick(
        Color32::from_rgb(255, 96, 96),
        Color32::from_rgb(196, 32, 32),
    )
}

pub fn ok() -> Color32 {
    pick(
        Color32::from_rgb(64, 220, 120),
        Color32::from_rgb(24, 150, 70),
    )
}

/// Kolor napisu "TUNE" w nagłówku.
pub fn wordmark() -> Color32 {
    pal().wordmark
}

/// Kolory przycisku zakładki: (tło, obwódka, tekst).
pub fn tab_colors(selected: bool, hovered: bool) -> (Color32, Color32, Color32) {
    let p = pal();
    let selected_text = if is_light() {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    match (selected, hovered) {
        (true, _) => (p.tab[0], p.glow, selected_text),
        (false, true) => (p.tab[1], p.tab[3], selected_text),
        (false, false) => (p.tab[2], p.tab[4], p.tab[5]),
    }
}

/// Powiększenie przycisków, zakładek i nagłówka względem treści: treść jest
/// pisana czcionką w zwykłym rozmiarze, a elementy do klikania są większe.
pub const BUTTON_SCALE: f32 = 8.0 / 7.0;

/// Wymiar elementu do klikania (przycisk, zakładka) po powiększeniu.
pub fn b(x: f32) -> f32 {
    x * BUTTON_SCALE
}

/// Rodziny czcionki Inter poza zwykłym tekstem: półgruba (zakładki, przyciski
/// paska stanu) i pogrubiona kursywa (napis "TUNE CENTER").
const FONT_SEMIBOLD: &str = "inter-semibold";
const FONT_WORDMARK: &str = "inter-bold-italic";

/// Rejestruje czcionkę Inter: zwykła zastępuje domyślną czcionkę tekstu,
/// półgruba i pogrubiona kursywa są osobnymi rodzinami. Domyślne czcionki
/// egui zostają jako zapas dla znaków, których Inter nie ma.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let files: [(&str, &'static [u8]); 3] = [
        ("inter", include_bytes!("../assets/Inter-Regular.ttf")),
        (FONT_SEMIBOLD, include_bytes!("../assets/Inter-SemiBold.ttf")),
        (FONT_WORDMARK, include_bytes!("../assets/Inter-BoldItalic.ttf")),
    ];
    for (name, bytes) in files {
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
    }
    let text = fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    text.insert(0, "inter".to_owned());
    let fallback = text.clone();
    for name in [FONT_SEMIBOLD, FONT_WORDMARK] {
        let mut family = vec![name.to_owned()];
        family.extend(fallback.iter().cloned());
        fonts
            .families
            .insert(egui::FontFamily::Name(name.into()), family);
    }
    ctx.set_fonts(fonts);
}

/// Półgruba czcionka zakładek i przycisków paska stanu.
pub fn tab_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FONT_SEMIBOLD.into()))
}

/// Pogrubiona kursywa napisu "TUNE CENTER".
pub fn wordmark_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FONT_WORDMARK.into()))
}

/// Ustawia motyw całego programu.
pub fn apply(ctx: &egui::Context, light: bool, style: Style) {
    LIGHT.store(light, Ordering::Relaxed);
    STYLE.store(style.index(), Ordering::Relaxed);
    // Jasny wariant istnieje tylko w motywie Tune.
    let light = is_light();
    // Motyw egui nie idzie za ustawieniem Windows: przy jasnym trybie systemu
    // egui przełączało się na własny jasny styl, bez kolorów programu (szare
    // przyciski i wyszarzone napisy na ciemnym tle).
    ctx.set_theme(if light {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    });

    let mut visuals = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    visuals.panel_fill = bg();
    visuals.window_fill = pal().window;
    visuals.window_stroke = Stroke::new(1.0_f32, card_edge());
    visuals.extreme_bg_color = well();
    visuals.faint_bg_color = pal().faint;
    visuals.selection.bg_fill = accent().linear_multiply(0.45);
    visuals.selection.stroke = Stroke::new(1.0_f32, glow());
    visuals.hyperlink_color = accent();
    // W jasnym motywie cały tekst ma być czarny, także w przyciskach i listach.
    visuals.override_text_color = if light { Some(text()) } else { None };

    // Przyciski i pola: ciemny granat z niebieską obwódką, jaśniejszą pod kursorem.
    let w = &mut visuals.widgets;
    let radius = CornerRadius::same(8);
    let [base, hover, press, line] = pal().widget;
    w.inactive.bg_fill = base;
    w.inactive.weak_bg_fill = base;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, line);
    w.inactive.corner_radius = radius;
    w.hovered.bg_fill = hover;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, glow());
    w.hovered.corner_radius = radius;
    w.active.bg_fill = press;
    w.active.weak_bg_fill = press;
    w.active.bg_stroke = Stroke::new(1.5_f32, glow());
    w.active.corner_radius = radius;
    w.open.weak_bg_fill = hover;
    w.open.bg_stroke = Stroke::new(1.0_f32, glow());
    w.open.corner_radius = radius;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, card_edge());
    // Domyślny tekst etykiet i przycisków w ciemnym motywie egui jest dość
    // ciemnoszary i słabo czytelny na granacie - jaśniejszy odcień.
    if !light {
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(214, 222, 236));
        w.inactive.fg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(226, 232, 242));
        w.hovered.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
        w.active.fg_stroke = Stroke::new(2.0_f32, Color32::WHITE);
    }
    w.noninteractive.corner_radius = radius;
    ctx.set_visuals(visuals);

    ctx.global_style_mut(|style| {
        style.spacing.button_padding = vec2(9.0, 4.0);
        style.spacing.interact_size.y = 25.0;
        style
            .text_styles
            .insert(egui::TextStyle::Heading, tab_font(18.0));
        // Stałe paski przewijania zamiast „pływających”: animacja ich pojawiania
        // się rysowała okno ~18 razy/s, gdy kursor stał nad przewijaną listą
        // (pomiar: NVIDIA > Ustawienia 3D 15-29% rdzenia bez ruchu myszy).
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        // Szerszy suwak w kolorze tekstu: na ciemnym tle domyślny (kolor
        // przycisku) był prawie niewidoczny i nie było widać, że niżej coś jest.
        style.spacing.scroll.bar_width = 10.0;
        style.spacing.scroll.foreground_color = true;
    });
}

// ------------------------------------------------------------------ tło okna

/// Miękka okrągła poświata: kolor w środku, przezroczysto na brzegu.
pub fn radial_glow(painter: &Painter, center: Pos2, radius: Pos2, color: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    let n = 48u32;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(
            pos2(center.x + a.cos() * radius.x, center.y + a.sin() * radius.y),
            Color32::TRANSPARENT,
        );
    }
    for i in 1..=n {
        mesh.add_triangle(0, i, i + 1);
    }
    painter.add(mesh);
}

/// Pozioma linia o kolorze przechodzącym od `left` do `right`.
pub fn gradient_line(
    painter: &Painter,
    y: f32,
    x0: f32,
    x1: f32,
    width: f32,
    left: Color32,
    right: Color32,
) {
    let mut mesh = Mesh::default();
    let h = width / 2.0;
    mesh.colored_vertex(pos2(x0, y - h), left);
    mesh.colored_vertex(pos2(x1, y - h), right);
    mesh.colored_vertex(pos2(x1, y + h), right);
    mesh.colored_vertex(pos2(x0, y + h), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

/// Świecąca linia: kilka warstw o malejącej jasności.
pub fn glow_line(painter: &Painter, points: &[Pos2], color: Color32, strength: f32) {
    for (width, alpha) in [(9.0, 0.05), (5.0, 0.12), (2.5, 0.35), (1.2, 1.0)] {
        painter.line(
            points.to_vec(),
            Stroke::new(width, color.gamma_multiply(alpha * strength)),
        );
    }
}

/// Tło całego okna w aktualnym motywie. `art` - grafika nagłówka motywu
/// Neon City (postać w prawym górnym rogu).
/// Grafiki motywu Neon City: postać w nagłówku i ulica na dole okna.
pub struct CityArt<'a> {
    pub top: &'a egui::TextureHandle,
    pub bottom: &'a egui::TextureHandle,
}

pub fn paint_background(painter: &Painter, rect: Rect, art: Option<CityArt>) {
    match style() {
        Style::Tune => paint_tune(painter, rect),
        Style::NeonGreen | Style::Crimson => paint_streaks(painter, rect),
        Style::NeonCity => paint_city(painter, rect, art),
    }
}

/// Kolor z przezroczystością 0..1.
fn alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// Neon Green / Crimson: czerń, poświaty w kolorze motywu i ostre ukośne
/// smugi światła w prawym górnym i lewym dolnym rogu.
fn paint_streaks(painter: &Painter, rect: Rect) {
    let p = pal();
    painter.rect_filled(rect, CornerRadius::ZERO, p.bg);
    radial_glow(
        painter,
        pos2(rect.right() - 60.0, rect.top() + 120.0),
        pos2(420.0, 300.0),
        alpha(p.bg_glow[0], 0.13),
    );
    radial_glow(
        painter,
        pos2(rect.left() + 80.0, rect.bottom() - 60.0),
        pos2(360.0, 220.0),
        alpha(p.bg_glow[1], 0.10),
    );
    radial_glow(
        painter,
        pos2(rect.left() + 110.0, rect.top() + 60.0),
        pos2(240.0, 140.0),
        alpha(p.bg_glow[0], 0.10),
    );
    // Smugi: szerokie przygaszone pasy i cienkie jasne linie.
    let c = p.glow;
    for (i, (width, a)) in [
        (26.0, 0.05),
        (2.0, 0.55),
        (10.0, 0.08),
        (1.5, 0.35),
        (4.0, 0.18),
    ]
    .into_iter()
    .enumerate()
    {
        let o = i as f32 * 58.0;
        painter.line_segment(
            [
                pos2(rect.right() - 520.0 + o, rect.bottom() + 20.0),
                pos2(rect.right() - 40.0 + o, rect.top() - 20.0),
            ],
            Stroke::new(width, alpha(c, a * 0.8)),
        );
    }
    for (i, (width, a)) in [(18.0, 0.05), (2.0, 0.45), (6.0, 0.10)]
        .into_iter()
        .enumerate()
    {
        let o = i as f32 * 46.0;
        painter.line_segment(
            [
                pos2(rect.left() + 80.0 + o, rect.bottom() + 10.0),
                pos2(rect.left() + 360.0 + o, rect.bottom() - 260.0),
            ],
            Stroke::new(width, alpha(c, a * 0.7)),
        );
    }
}

/// Neon City: fiolet z poświatami magenty i cyjanu, w nagłówku panorama
/// miasta z oświetlonymi oknami i postać po prawej.
fn paint_city(painter: &Painter, rect: Rect, art: Option<CityArt>) {
    let p = pal();
    painter.rect_filled(rect, CornerRadius::ZERO, p.bg);
    radial_glow(
        painter,
        pos2(rect.right() - 120.0, rect.top() + 30.0),
        pos2(420.0, 220.0),
        alpha(p.bg_glow[0], 0.16),
    );
    radial_glow(
        painter,
        pos2(rect.left() + 100.0, rect.top() + 60.0),
        pos2(260.0, 150.0),
        alpha(p.bg_glow[1], 0.12),
    );
    radial_glow(
        painter,
        pos2(rect.center().x, rect.bottom() + 30.0),
        pos2(rect.width() * 0.6, 170.0),
        alpha(p.bg_glow[0], 0.10),
    );
    radial_glow(
        painter,
        pos2(rect.right() - 60.0, rect.bottom() - 80.0),
        pos2(300.0, 240.0),
        alpha(p.bg_glow[1], 0.08),
    );

    // Panorama miasta w pasie nagłówka (stałe "losowe" wymiary budynków).
    let base = rect.top() + 112.0;
    let art_w = 180.0;
    let x_end = rect.right() - art_w + 20.0;
    let mut x = (rect.right() - 720.0).max(rect.left() + 300.0);
    let mut seed: u32 = 0x9E37_79B9;
    let mut next = |n: u32| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 16) % n
    };
    let building = alpha(rgb(26, 18, 62), 0.85);
    while x < x_end {
        let w = 14.0 + next(22) as f32;
        let h = 30.0 + next(76) as f32;
        let top = base - h;
        painter.rect_filled(
            Rect::from_min_max(pos2(x, top), pos2(x + w, base)),
            CornerRadius::ZERO,
            building,
        );
        let mut wy = top + 5.0;
        while wy < base - 5.0 {
            let mut wx = x + 3.0;
            while wx < x + w - 3.0 {
                if next(4) == 0 {
                    let c = if next(2) == 0 { p.glow } else { p.accent2 };
                    painter.rect_filled(
                        Rect::from_min_size(pos2(wx, wy), vec2(2.0, 2.0)),
                        CornerRadius::ZERO,
                        alpha(c, 0.65),
                    );
                }
                wx += 6.0;
            }
            wy += 7.0;
        }
        x += w + 1.0 + next(4) as f32;
    }

    // Postać (grafika motywu) w prawym górnym rogu, na wysokość nagłówka.
    if let Some(art) = art {
        // Ulica z odbiciami neonów na dole okna (wyśrodkowana, krawędzie
        // wygaszone w samej grafice).
        let size = art.bottom.size_vec2();
        let w = size.x.max(rect.width());
        let h = size.y * w / size.x;
        let r = Rect::from_min_size(
            pos2(rect.center().x - w / 2.0, rect.bottom() - h),
            vec2(w, h),
        );
        painter.image(
            art.bottom.id(),
            r,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        let tex = art.top;
        let size = tex.size_vec2();
        let h = 120.0;
        let w = size.x * h / size.y;
        let r = Rect::from_min_size(pos2(rect.right() - w, rect.top()), vec2(w, h));
        painter.image(
            tex.id(),
            r,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// Tune: granat z niebieskimi poświatami i ukośnymi liniami w rogach (w
/// jasnym wariancie bardzo delikatne).
fn paint_tune(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, bg());
    let k = if is_light() { 0.35 } else { 1.0 };
    let blue = |a: f32| Color32::from_rgba_unmultiplied(20, 110, 230, (a * k * 255.0) as u8);

    radial_glow(
        painter,
        pos2(rect.left() + 110.0, rect.top() + 70.0),
        pos2(260.0, 160.0),
        blue(0.22),
    );
    radial_glow(
        painter,
        pos2(rect.right() - 80.0, rect.center().y),
        pos2(320.0, 380.0),
        blue(0.14),
    );
    radial_glow(
        painter,
        pos2(rect.center().x, rect.bottom() + 40.0),
        pos2(rect.width() * 0.6, 160.0),
        blue(0.12),
    );
    radial_glow(
        painter,
        pos2(rect.left(), rect.bottom() - 120.0),
        pos2(260.0, 300.0),
        blue(0.10),
    );

    // Ukośne smugi światła w prawym górnym i lewym dolnym rogu.
    let streak = Color32::from_rgba_unmultiplied(60, 160, 255, (26.0 * k) as u8);
    for i in 0..5 {
        let o = i as f32 * 36.0;
        painter.line_segment(
            [
                pos2(rect.right() - 420.0 + o, rect.top()),
                pos2(rect.right() + 40.0 + o, rect.top() + 460.0),
            ],
            Stroke::new(1.0_f32, streak),
        );
        painter.line_segment(
            [
                pos2(rect.left() - 60.0 + o, rect.bottom() - 380.0),
                pos2(rect.left() + 320.0 + o, rect.bottom()),
            ],
            Stroke::new(1.0_f32, streak),
        );
    }
}

// ----------------------------------------------------------------- karty

/// Karta z niebieską obwódką, poświatą na zewnątrz i jaśniejszą górną
/// krawędzią. Użycie jak `egui::Frame`: `theme::card(14, 16).show(ui, |ui| ...)`.
pub struct Card {
    frame: egui::Frame,
    radius: u8,
}

/// Ramka kafelka. Promień i margines w pikselach (egui 0.37 przyjmuje liczby całkowite).
pub fn card(radius: u8, margin: i8) -> Card {
    Card {
        frame: egui::Frame::NONE
            .fill(card_fill())
            .stroke(Stroke::new(1.0_f32, card_edge()))
            .corner_radius(CornerRadius::same(radius))
            .inner_margin(egui::Margin::same(margin)),
        radius,
    }
}

impl Card {
    pub fn show<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        // Miejsce na poświatę pod kartą rezerwujemy przed treścią, żeby była pod spodem.
        let under = ui.painter().add(egui::Shape::Noop);
        let inner = self.frame.show(ui, add_contents);
        let rect = inner.response.rect;
        if ui.is_rect_visible(rect) {
            let r = self.radius;
            let mut shapes = Vec::new();
            for (grow, alpha) in [(6.0, 0.035), (3.5, 0.07), (1.5, 0.14)] {
                shapes.push(egui::Shape::rect_stroke(
                    rect.expand(grow),
                    CornerRadius::same(r.saturating_add(grow as u8)),
                    Stroke::new(2.0_f32, glow().gamma_multiply(alpha)),
                    StrokeKind::Middle,
                ));
            }
            ui.painter().set(under, egui::Shape::Vec(shapes));
            // Jasna górna krawędź, wygaszana w prawo.
            let x0 = rect.left() + r as f32;
            let x1 = rect.right() - r as f32;
            let mid = x0 + (x1 - x0) * 0.55;
            let painter = ui.painter();
            gradient_line(
                painter,
                rect.top() + 0.5,
                x0,
                mid,
                2.0,
                glow(),
                glow().gamma_multiply(0.35),
            );
            gradient_line(
                painter,
                rect.top() + 0.5,
                mid,
                x1,
                2.0,
                glow().gamma_multiply(0.35),
                Color32::TRANSPARENT,
            );
        }
        inner
    }
}

// ------------------------------------------------- ramki i kształty okna

/// Ramka treści zakładki: jedna duża karta z zaokrąglonymi rogami.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::NONE
        .outer_margin(egui::Margin {
            left: 10,
            right: 10,
            top: 2,
            bottom: 0,
        })
        .inner_margin(egui::Margin::symmetric(12, 8))
        .fill(card_fill())
        .stroke(Stroke::new(1.0_f32, card_edge()))
        .corner_radius(CornerRadius::same(12))
}

/// Ramka paska stanu: osobna zaokrąglona belka na dole okna.
pub fn bar_frame() -> egui::Frame {
    egui::Frame::NONE
        .outer_margin(egui::Margin {
            left: 10,
            right: 10,
            top: 6,
            bottom: 8,
        })
        .inner_margin(egui::Margin::symmetric(10, 4))
        .fill(card_fill())
        .stroke(Stroke::new(1.0_f32, card_edge()))
        .corner_radius(CornerRadius::same(10))
}

/// Obrys pochylonego prostokąta z zaokrąglonymi rogami (zakładki nagłówka).
pub fn slanted_rect(rect: Rect, skew: f32, radius: f32) -> Vec<Pos2> {
    let corners = [
        pos2(rect.left() + skew, rect.top()),
        pos2(rect.right(), rect.top()),
        pos2(rect.right() - skew, rect.bottom()),
        pos2(rect.left(), rect.bottom()),
    ];
    let mut out = Vec::with_capacity(20);
    for i in 0..4 {
        let p = corners[i];
        let a = p + (corners[(i + 3) % 4] - p).normalized() * radius;
        let b = p + (corners[(i + 1) % 4] - p).normalized() * radius;
        // Róg jako krzywa Béziera drugiego stopnia przez wierzchołek.
        for k in 0..=4 {
            let t = k as f32 / 4.0;
            let u = 1.0 - t;
            out.push(pos2(
                u * u * a.x + 2.0 * u * t * p.x + t * t * b.x,
                u * u * a.y + 2.0 * u * t * p.y + t * t * b.y,
            ));
        }
    }
    out
}

/// Przycisk paska stanu (UPDATE, OPTIONS, ABOUT): zaokrąglony prostokąt
/// z ikoną. `selected` - otwarta zakładka, `primary` - wypełniony kolorem.
pub fn bar_button(
    ui: &mut egui::Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    primary: bool,
) -> egui::Response {
    let font = tab_font(b(12.5));
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let size = vec2(text_width + b(46.0), b(27.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let pal = pal();
        let hovered = response.hovered();
        let (fill, edge) = if selected {
            (pal.tab[0], pal.glow)
        } else if primary {
            (if hovered { pal.widget[2] } else { pal.tab[0] }, pal.tab[3])
        } else if hovered {
            (pal.tab[1], pal.tab[3])
        } else {
            (pal.tab[2], pal.tab[4])
        };
        if selected {
            p.rect_stroke(
                rect.expand(2.0),
                CornerRadius::same(9),
                Stroke::new(3.0_f32, pal.glow.gamma_multiply(0.2)),
                StrokeKind::Middle,
            );
        }
        p.rect(
            rect,
            CornerRadius::same(7),
            fill,
            Stroke::new(if selected { 1.5_f32 } else { 1.0_f32 }, edge),
            StrokeKind::Inside,
        );
        let color = if selected || primary || hovered {
            text()
        } else {
            pal.tab[5]
        };
        let x = rect.center().x - (text_width + b(22.0)) / 2.0;
        icons::paint(
            p,
            Rect::from_center_size(pos2(x + b(8.0), rect.center().y), vec2(b(16.0), b(16.0))),
            icon,
            if selected { glow() } else { color },
        );
        p.text(
            pos2(x + b(22.0), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// --------------------------------------------------- nagłówki i przyciski

/// Nagłówek sekcji: niebieska ikona i duży napis.
pub fn heading(ui: &mut egui::Ui, icon: Icon, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        icons::show(ui, icon, 30.0, glow());
        ui.label(RichText::new(text).size(22.0).strong().color(self::text()));
    });
    ui.add_space(4.0);
}

/// Mniejszy nagłówek (np. tytuł karty).
pub fn subheading(ui: &mut egui::Ui, icon: Icon, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        icons::show(ui, icon, 22.0, glow());
        ui.label(RichText::new(text).size(17.0).strong().color(self::text()));
    });
    ui.add_space(4.0);
}

/// Notatka z ikoną "i" w kółku; tekst zawija się do szerokości.
pub fn info(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        icons::show(ui, Icon::About, 16.0, glow());
        ui.add(egui::Label::new(RichText::new(text).size(12.0).color(muted())).wrap());
    });
}

/// Wyróżniony przycisk: granatowy z jasną, świecącą obwódką i opcjonalną ikoną.
pub fn primary_button(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    enabled: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(b(14.0));
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, text());
    let icon_w = if icon.is_some() { b(26.0) } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + b(32.0), b(28.0));
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let hovered = enabled && response.hovered();
        let edge = if enabled { glow() } else { card_edge() };
        let [off, hot, normal] = pal().primary;
        let fill = match (enabled, hovered) {
            (false, _) => off,
            (true, true) => hot,
            (true, false) => normal,
        };
        if enabled {
            let strength = if hovered { 0.35 } else { 0.18 };
            p.rect_stroke(
                rect.expand(2.5),
                CornerRadius::same(9),
                Stroke::new(3.0_f32, edge.gamma_multiply(strength)),
                StrokeKind::Middle,
            );
        }
        p.rect(
            rect,
            CornerRadius::same(7),
            fill,
            Stroke::new(1.5_f32, edge),
            StrokeKind::Inside,
        );
        let color = if enabled { text() } else { muted() };
        let total = galley.size().x + icon_w;
        let mut x = rect.center().x - total / 2.0;
        if let Some(icon) = icon {
            let r = Rect::from_center_size(pos2(x + b(9.0), rect.center().y), vec2(b(18.0), b(18.0)));
            icons::paint(p, r, icon, if enabled { glow() } else { muted() });
            x += icon_w;
        }
        p.galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Tytuł zakładki: duży półgruby napis z ukośnikami w kolorze akcentu.
pub fn title(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(RichText::new(text).font(tab_font(19.0)).color(self::text()));
        let (rect, _) = ui.allocate_exact_size(vec2(26.0, 12.0), Sense::hover());
        for i in 0..3 {
            let x = rect.left() + i as f32 * 8.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    pos2(x + 5.0, rect.top()),
                    pos2(x + 9.0, rect.top()),
                    pos2(x + 4.0, rect.bottom()),
                    pos2(x, rect.bottom()),
                ],
                accent().gamma_multiply(1.0 - i as f32 * 0.3),
                Stroke::NONE,
            ));
        }
    });
}

/// Szerokość przycisku z ikoną o napisie szerokości `text_width`.
pub fn icon_button_width(text_width: f32) -> f32 {
    text_width + b(38.0)
}

/// Szerokość napisu przycisku z ikoną.
pub fn icon_button_text_width(ui: &egui::Ui, label: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            label.to_owned(),
            egui::FontId::proportional(b(12.5)),
            Color32::WHITE,
        )
        .size()
        .x
}

/// Zwykły przycisk z ikoną. `accent` - wyróżniony jasną obwódką (główna
/// czynność w wierszu); wyłączony jest wyszarzony i nie reaguje na kliknięcie.
pub fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    enabled: bool,
    accent: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(b(12.5));
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let size = vec2(icon_button_width(text_width), b(25.0));
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let hovered = enabled && response.hovered();
        let [base, hover, press, line] = pal().widget;
        let fill = if !enabled {
            pal().primary[0]
        } else if response.is_pointer_button_down_on() {
            press
        } else if hovered {
            hover
        } else {
            base
        };
        let edge = if !enabled {
            card_edge()
        } else if accent || hovered {
            glow()
        } else {
            line
        };
        if enabled && accent {
            p.rect_stroke(
                rect.expand(2.0),
                CornerRadius::same(10),
                Stroke::new(3.0_f32, glow().gamma_multiply(0.18)),
                StrokeKind::Middle,
            );
        }
        p.rect(
            rect,
            CornerRadius::same(8),
            fill,
            Stroke::new(1.0_f32, edge),
            StrokeKind::Inside,
        );
        let color = if enabled {
            text()
        } else {
            muted().gamma_multiply(0.6)
        };
        let x = rect.center().x - (text_width + b(20.0)) / 2.0;
        icons::paint(
            p,
            Rect::from_center_size(pos2(x + b(7.0), rect.center().y), vec2(b(15.0), b(15.0))),
            icon,
            if enabled { glow() } else { color },
        );
        p.text(
            pos2(x + b(20.0), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            color,
        );
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Przycisk podzakładki: napis z jasnym podkreśleniem, gdy wybrany.
pub fn sub_tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    sub_tab_icon(ui, None, label, selected)
}

/// Przycisk podzakładki z opcjonalną ikoną przed napisem.
pub fn sub_tab_icon(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    selected: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(b(13.0));
    let color = if selected { text() } else { muted() };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let icon_w = if icon.is_some() { b(22.0) } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + b(18.0), b(27.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let hovered = response.hovered();
        if selected || hovered {
            let fill = if selected {
                pal().sub_tab[0]
            } else {
                pal().sub_tab[1]
            };
            p.rect(
                rect,
                CornerRadius::same(6),
                fill,
                Stroke::new(1.0_f32, if selected { accent2() } else { card_edge() }),
                StrokeKind::Inside,
            );
        }
        if selected {
            glow_line(
                p,
                &[
                    pos2(rect.left() + 8.0, rect.bottom() - 1.0),
                    pos2(rect.right() - 8.0, rect.bottom() - 1.0),
                ],
                accent2(),
                1.0,
            );
        }
        let text_color = if hovered && !selected { text() } else { color };
        let x = rect.center().x - (galley.size().x + icon_w) / 2.0;
        if let Some(icon) = icon {
            icons::paint(
                p,
                Rect::from_center_size(pos2(x + b(8.0), rect.center().y), vec2(b(16.0), b(16.0))),
                icon,
                if selected { glow() } else { text_color },
            );
        }
        p.galley(
            pos2(x + icon_w, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Pionowa kreska oddzielająca elementy w wierszu.
pub fn vsep(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().line_segment(
        [
            pos2(rect.center().x, rect.top()),
            pos2(rect.center().x, rect.bottom()),
        ],
        Stroke::new(1.0_f32, card_edge()),
    );
}

/// Kolorowa kropka stanu (np. "Czujniki: połączono").
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 6.0, color.gamma_multiply(0.25));
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

pub fn temp_color(value: f32) -> Color32 {
    if value >= 80.0 {
        hot()
    } else if value >= 65.0 {
        warn()
    } else {
        accent()
    }
}

/// Karty w wierszach o liczbie kolumn dobranej do szerokości okna.
/// (`horizontal_wrapped` nie zawija ramek, bo zna ich szerokość dopiero po
/// narysowaniu - dlatego wiersze składamy sami.) `add` dostaje szerokość wnętrza.
pub fn card_grid<T>(
    ui: &mut egui::Ui,
    items: &[T],
    min_outer: f32,
    gap: f32,
    max_cols: usize,
    radius: u8,
    margin: i8,
    mut add: impl FnMut(&mut egui::Ui, usize, &T, f32),
) {
    let avail = ui.available_width();
    let inner = grid_card_width(avail, min_outer, gap, max_cols, margin as f32);
    let cols = (((avail + gap) / (min_outer + gap)).floor().max(1.0) as usize).min(max_cols.max(1));
    for (row, chunk) in items.chunks(cols).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(gap, gap);
            for (j, item) in chunk.iter().enumerate() {
                card(radius, margin).show(ui, |ui| {
                    ui.set_width(inner);
                    add(ui, row * cols + j, item, inner);
                });
            }
        });
        ui.add_space(gap);
    }
}

/// Szerokość wnętrza kart ułożonych w siatce, która wypełnia dostępną
/// szerokość: liczba kolumn rośnie i maleje razem z oknem.
///
/// `min_outer` - najmniejsza sensowna szerokość całej karty, `gap` - odstęp
/// między kartami, `margin` - wewnętrzny margines karty (jak w [`card`]).
pub fn grid_card_width(avail: f32, min_outer: f32, gap: f32, max_cols: usize, margin: f32) -> f32 {
    let fit = ((avail + gap) / (min_outer + gap)).floor().max(1.0) as usize;
    let cols = fit.min(max_cols.max(1));
    let outer = (avail + gap) / cols as f32 - gap;
    // 2 px na obramowanie i 1 px zapasu, żeby zaokrąglenia nie zawijały wiersza.
    (outer - 2.0 * margin - 3.0).max(120.0)
}

/// Stan przełącznika ON/OFF.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SwitchState {
    On,
    Off,
    /// Ustawienie nie dotyczy tego komputera (szary, przerywany, bez gałki).
    NotApplicable,
}

/// Przełącznik ON/OFF: zielony z gałką po prawej (ON), czerwony z gałką po
/// lewej (OFF). Zmiana stanu przesuwa gałkę płynnie. Stan pokazuje to, co
/// podał wywołujący - kliknięcie tylko zgłasza żądanie zmiany.
pub fn switch(ui: &mut egui::Ui, id: &str, state: SwitchState, enabled: bool) -> egui::Response {
    let size = vec2(66.0, 28.0);
    let sense = if enabled && state != SwitchState::NotApplicable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let id = ui.scope_id().with(("tc-switch", id));
    let t = ui
        .ctx()
        .animate_bool_with_time(id, state == SwitchState::On, 0.2);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let radius = CornerRadius::same(14);
    let font = egui::FontId::proportional(12.0);

    if state == SwitchState::NotApplicable {
        // Przerywana obwódka z napisem "n/d".
        let color = muted().gamma_multiply(0.7);
        let r = rect.shrink(1.0);
        let mut pts = Vec::new();
        let steps = 48;
        for i in 0..=steps {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            // Zaokrąglony prostokąt przybliżony "stadionem".
            let (s, c) = a.sin_cos();
            let x = if c >= 0.0 {
                r.right() - 14.0
            } else {
                r.left() + 14.0
            } + c * 13.0;
            pts.push(pos2(x, r.center().y + s * 13.0));
        }
        for (i, w) in pts.windows(2).enumerate() {
            // Odcinek przeskakujący między łukami (góra/dół) rysują linie niżej.
            if i % 2 == 0 && (w[0].x - w[1].x).abs() < 6.0 {
                painter.line_segment([w[0], w[1]], Stroke::new(1.5, color));
            }
        }
        let mut x = r.left() + 16.0;
        while x < r.right() - 16.0 {
            let x2 = (x + 4.0).min(r.right() - 14.0);
            for y in [r.center().y - 13.0, r.center().y + 13.0] {
                painter.line_segment([pos2(x, y), pos2(x2, y)], Stroke::new(1.5, color));
            }
            x += 8.0;
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            crate::i18n::tr("n/d", "n/a"),
            font,
            color,
        );
        return response;
    }

    let lerp = |a: Color32, b: Color32| {
        let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
        Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
    };
    let (off_bg, off_edge, off_knob, off_text) = (
        pick(
            Color32::from_rgb(90, 26, 31),
            Color32::from_rgb(252, 222, 222),
        ),
        pick(
            Color32::from_rgb(224, 72, 72),
            Color32::from_rgb(200, 50, 50),
        ),
        pick(
            Color32::from_rgb(255, 138, 138),
            Color32::from_rgb(200, 50, 50),
        ),
        pick(
            Color32::from_rgb(255, 179, 179),
            Color32::from_rgb(170, 30, 30),
        ),
    );
    let (on_bg, on_edge, on_knob, on_text) = (
        pick(
            Color32::from_rgb(23, 128, 63),
            Color32::from_rgb(34, 160, 80),
        ),
        pick(
            Color32::from_rgb(64, 220, 120),
            Color32::from_rgb(24, 130, 60),
        ),
        Color32::WHITE,
        Color32::WHITE,
    );
    let mut bg = lerp(off_bg, on_bg);
    let edge = lerp(off_edge, on_edge);
    if !enabled {
        bg = bg.gamma_multiply(0.6);
    }
    painter.rect(rect, radius, bg, Stroke::new(1.5, edge), StrokeKind::Inside);
    if response.hovered() && enabled {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(16),
            Stroke::new(1.0, edge.gamma_multiply(0.5)),
            StrokeKind::Outside,
        );
    }
    // Napisy: ON po lewej, OFF po prawej - widoczny ten po stronie bez gałki.
    painter.text(
        pos2(rect.left() + 18.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        "ON",
        font.clone(),
        on_text.gamma_multiply(t),
    );
    painter.text(
        pos2(rect.right() - 20.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        "OFF",
        font,
        off_text.gamma_multiply(1.0 - t),
    );
    let x = rect.left() + 14.0 + t * (rect.width() - 28.0);
    painter.circle_filled(pos2(x, rect.center().y), 10.0, lerp(off_knob, on_knob));
    response.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::NotAllowed
    })
}

/// Napis "TUNE CENTER" w kolorze motywu. W jasnym motywie srebrne "TUNE"
/// jest przyciemniane do granatu, bo na białym tle było słabo widoczne.
pub fn tint_wordmark(rgba: &[u8]) -> Vec<u8> {
    let mut out = tint_logo(rgba);
    if is_light() {
        for px in out.chunks_exact_mut(4) {
            let max = px[0].max(px[1]).max(px[2]) as f32;
            let min = px[0].min(px[1]).min(px[2]) as f32;
            // Tylko szarości (srebro); niebieskie "CENTER" zostaje.
            if max > 0.0 && (max - min) / max < 0.18 {
                let k = max / 255.0;
                px[0] = (14.0 + 26.0 * k) as u8;
                px[1] = (30.0 + 40.0 * k) as u8;
                px[2] = (58.0 + 60.0 * k) as u8;
            }
        }
    }
    out
}

/// Logo (niebieskie RGBA) w kolorze aktualnego motywu: barwa kolorowych
/// pikseli jest przesuwana do barwy motywu, jasność, nasycenie i różnice
/// odcieni zostają.
pub fn tint_logo(rgba: &[u8]) -> Vec<u8> {
    let target = match style() {
        Style::Tune => return rgba.to_vec(),
        Style::NeonGreen => 92.0,
        Style::NeonCity => 188.0,
        Style::Crimson => 4.0,
    };
    // Barwa, nasycenie i jasność piksela (barwa w stopniach).
    let hsv = |px: &[u8]| {
        let (r, g, b) = (
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        );
        let max = r.max(g).max(b);
        let d = max - r.min(g).min(b);
        let s = if max > 0.0 { d / max } else { 0.0 };
        let h = if d <= 0.0 {
            0.0
        } else if max == r {
            60.0 * ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h, s, max)
    };
    // Średnia barwa kolorowej części grafiki. Barwa każdego piksela jest
    // przesuwana o tę samą wartość, więc przejścia odcieni zostają i grafika
    // nie wygląda na zamalowaną jednym kolorem.
    let (mut sum, mut weight) = (0.0_f32, 0.0_f32);
    for px in rgba.chunks_exact(4) {
        let (h, s, _) = hsv(px);
        if s >= 0.18 && px[3] > 0 {
            sum += h * s;
            weight += s;
        }
    }
    let mean = if weight > 0.0 { sum / weight } else { target };
    let mut out = rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        let (h, s, max) = hsv(px);
        // Szarości i biel (napis "TUNE") zostają bez zmian.
        if s < 0.18 {
            continue;
        }
        let [nr, ng, nb] = hsv_to_rgb((target + h - mean).rem_euclid(360.0), s, max);
        px[0] = (nr * 255.0) as u8;
        px[1] = (ng * 255.0) as u8;
        px[2] = (nb * 255.0) as u8;
    }
    out
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}
