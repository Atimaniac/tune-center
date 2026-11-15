//! Wspólne kolory i elementy wyglądu, w dwóch motywach: ciemnym i jasnym.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, CornerRadius, Stroke};

/// Aktualny motyw; kolory poniżej czytają go przy każdym rysowaniu.
static LIGHT: AtomicBool = AtomicBool::new(false);

pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

fn pick(dark: Color32, light: Color32) -> Color32 {
    if is_light() {
        light
    } else {
        dark
    }
}

/// Tło okna: ciemny granat albo biel.
pub fn bg() -> Color32 {
    pick(Color32::from_rgb(16, 18, 24), Color32::WHITE)
}

/// Główny kolor tekstu: biały na ciemnym tle, czarny na białym.
pub fn text() -> Color32 {
    pick(Color32::WHITE, Color32::BLACK)
}

fn card_fill() -> Color32 {
    pick(
        Color32::from_rgb(26, 29, 38),
        Color32::from_rgb(244, 246, 249),
    )
}

pub fn card_edge() -> Color32 {
    pick(
        Color32::from_rgb(44, 49, 63),
        Color32::from_rgb(208, 213, 222),
    )
}

/// Tło wykresów i wnętrza wentylatora.
pub fn well() -> Color32 {
    pick(
        Color32::from_rgb(18, 20, 27),
        Color32::from_rgb(232, 235, 241),
    )
}

/// Linie siatki i tła pasków.
pub fn track() -> Color32 {
    pick(
        Color32::from_rgb(38, 42, 54),
        Color32::from_rgb(212, 217, 226),
    )
}

pub fn accent() -> Color32 {
    pick(
        Color32::from_rgb(64, 196, 255),
        Color32::from_rgb(0, 112, 192),
    )
}

/// Tekst pomocniczy.
pub fn muted() -> Color32 {
    pick(
        Color32::from_rgb(140, 148, 168),
        Color32::from_rgb(78, 86, 102),
    )
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

/// Wypełnienie nieaktywnego przycisku zakładki (ciemny, stalowy niebieski).
pub fn tab_fill() -> Color32 {
    Color32::from_rgb(52, 86, 122)
}

/// Wypełnienie przycisku zakładki pod kursorem.
pub fn tab_fill_hovered() -> Color32 {
    Color32::from_rgb(66, 104, 144)
}

/// Obramowanie i napis aktywnej zakładki (morski).
pub fn tab_active() -> Color32 {
    pick(
        Color32::from_rgb(64, 214, 198),
        Color32::from_rgb(34, 178, 164),
    )
}

/// Kolor napisu "TUNE CENTER" w nagłówku.
pub fn wordmark() -> Color32 {
    pick(
        Color32::from_rgb(196, 212, 232),
        Color32::from_rgb(47, 74, 107),
    )
}

/// Nazwa rodziny czcionki przycisków zakładek.
const TAB_FONT: &str = "audiowide";

/// Rejestruje czcionkę Audiowide jako osobną rodzinę. Treść programu zostaje
/// przy czcionce domyślnej; Audiowide jest używana tylko na przyciskach zakładek.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        TAB_FONT.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/Audiowide-Regular.ttf"
        ))),
    );
    fonts.families.insert(
        egui::FontFamily::Name(TAB_FONT.into()),
        vec![TAB_FONT.to_owned()],
    );
    ctx.set_fonts(fonts);
}

/// Czcionka przycisków zakładek (Audiowide).
pub fn tab_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(TAB_FONT.into()))
}

/// Ustawia motyw całego programu.
pub fn apply(ctx: &egui::Context, light: bool) {
    LIGHT.store(light, Ordering::Relaxed);

    let mut visuals = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    visuals.panel_fill = bg();
    visuals.window_fill = bg();
    visuals.extreme_bg_color = well();
    visuals.selection.bg_fill = accent().linear_multiply(0.45);
    // W jasnym motywie cały tekst ma być czarny, także w przyciskach i listach.
    visuals.override_text_color = if light { Some(text()) } else { None };
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    ctx.set_visuals(visuals);
}

/// Ramka kafelka. Promień i margines w pikselach (egui 0.37 przyjmuje liczby całkowite).
pub fn card(radius: u8, margin: i8) -> egui::Frame {
    egui::Frame::NONE
        .fill(card_fill())
        .stroke(Stroke::new(1.0_f32, card_edge()))
        .corner_radius(CornerRadius::same(radius))
        .inner_margin(egui::Margin::same(margin))
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
