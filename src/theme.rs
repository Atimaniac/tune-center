//! Wspólne kolory i elementy wyglądu, w dwóch motywach: ciemnym (granat z
//! niebieską poświatą) i jasnym. Tu są też wspólne elementy interfejsu:
//! tło okna, karty z poświatą, nagłówki sekcji z ikoną, wyróżnione przyciski,
//! przyciski podzakładek i notatki z ikoną "i".

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    self, pos2, vec2, Color32, CornerRadius, Mesh, Painter, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind,
};

use crate::icons::{self, Icon};

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

/// Tło okna: bardzo ciemny granat albo jasny szaroniebieski.
pub fn bg() -> Color32 {
    pick(
        Color32::from_rgb(5, 10, 20),
        Color32::from_rgb(244, 247, 252),
    )
}

/// Główny kolor tekstu: biały na ciemnym tle, czarny na jasnym.
pub fn text() -> Color32 {
    pick(Color32::WHITE, Color32::BLACK)
}

fn card_fill() -> Color32 {
    pick(
        Color32::from_rgba_unmultiplied(9, 19, 36, 232),
        Color32::from_rgba_unmultiplied(255, 255, 255, 240),
    )
}

pub fn card_edge() -> Color32 {
    pick(
        Color32::from_rgb(30, 66, 112),
        Color32::from_rgb(168, 198, 232),
    )
}

/// Tło wykresów, pól edycji i wnętrza wentylatora.
pub fn well() -> Color32 {
    pick(
        Color32::from_rgb(5, 12, 24),
        Color32::from_rgb(233, 240, 249),
    )
}

/// Linie siatki i tła pasków.
pub fn track() -> Color32 {
    pick(
        Color32::from_rgb(22, 40, 66),
        Color32::from_rgb(206, 218, 232),
    )
}

pub fn accent() -> Color32 {
    pick(
        Color32::from_rgb(56, 182, 255),
        Color32::from_rgb(0, 108, 196),
    )
}

/// Jasny błękit poświaty (obramowania aktywnych elementów).
pub fn glow() -> Color32 {
    pick(
        Color32::from_rgb(64, 190, 255),
        Color32::from_rgb(30, 140, 230),
    )
}

/// Tekst pomocniczy.
pub fn muted() -> Color32 {
    pick(
        Color32::from_rgb(180, 194, 216),
        Color32::from_rgb(78, 90, 110),
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

pub fn ok() -> Color32 {
    pick(
        Color32::from_rgb(64, 220, 120),
        Color32::from_rgb(24, 150, 70),
    )
}

/// Kolor napisu "TUNE CENTER" w nagłówku.
pub fn wordmark() -> Color32 {
    pick(
        Color32::from_rgb(214, 232, 252),
        Color32::from_rgb(20, 60, 110),
    )
}

/// Nazwa rodziny czcionki przycisków zakładek.
const TAB_FONT: &str = "audiowide";

/// Rejestruje czcionkę Audiowide jako osobną rodzinę. Treść programu zostaje
/// przy czcionce domyślnej; Audiowide jest używana tylko na przyciskach zakładek
/// i w napisie "TUNE CENTER".
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
    visuals.window_fill = pick(Color32::from_rgb(8, 17, 32), Color32::WHITE);
    visuals.window_stroke = Stroke::new(1.0_f32, card_edge());
    visuals.extreme_bg_color = well();
    visuals.faint_bg_color = pick(
        Color32::from_rgb(10, 22, 40),
        Color32::from_rgb(238, 243, 250),
    );
    visuals.selection.bg_fill = accent().linear_multiply(0.45);
    visuals.selection.stroke = Stroke::new(1.0_f32, glow());
    visuals.hyperlink_color = accent();
    // W jasnym motywie cały tekst ma być czarny, także w przyciskach i listach.
    visuals.override_text_color = if light { Some(text()) } else { None };

    // Przyciski i pola: ciemny granat z niebieską obwódką, jaśniejszą pod kursorem.
    let w = &mut visuals.widgets;
    let radius = CornerRadius::same(6);
    let base = pick(
        Color32::from_rgb(10, 24, 46),
        Color32::from_rgb(236, 243, 252),
    );
    let hover = pick(
        Color32::from_rgb(16, 40, 74),
        Color32::from_rgb(220, 234, 250),
    );
    let press = pick(
        Color32::from_rgb(20, 52, 96),
        Color32::from_rgb(204, 224, 248),
    );
    let line = pick(
        Color32::from_rgb(36, 82, 138),
        Color32::from_rgb(150, 186, 226),
    );
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
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
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

/// Tło całego okna: granat z niebieskimi poświatami i ukośnymi liniami
/// w rogach (w jasnym motywie bardzo delikatne).
pub fn paint_background(painter: &Painter, rect: Rect) {
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
    let font = egui::FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, text());
    let icon_w = if icon.is_some() { 26.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 36.0, 34.0);
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
        let fill = match (enabled, hovered) {
            (false, _) => pick(
                Color32::from_rgb(10, 20, 36),
                Color32::from_rgb(236, 240, 246),
            ),
            (true, true) => pick(
                Color32::from_rgb(16, 48, 92),
                Color32::from_rgb(214, 232, 252),
            ),
            (true, false) => pick(
                Color32::from_rgb(10, 30, 60),
                Color32::from_rgb(232, 242, 254),
            ),
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
            let r = Rect::from_center_size(pos2(x + 9.0, rect.center().y), vec2(18.0, 18.0));
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

/// Przycisk podzakładki: napis z jasnym podkreśleniem, gdy wybrany.
pub fn sub_tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let font = egui::FontId::proportional(14.0);
    let color = if selected { text() } else { muted() };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let size = vec2(galley.size().x + 20.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let hovered = response.hovered();
        if selected || hovered {
            let fill = if selected {
                pick(
                    Color32::from_rgb(12, 36, 70),
                    Color32::from_rgb(220, 236, 252),
                )
            } else {
                pick(
                    Color32::from_rgb(10, 24, 46),
                    Color32::from_rgb(234, 242, 252),
                )
            };
            p.rect(
                rect,
                CornerRadius::same(6),
                fill,
                Stroke::new(1.0_f32, if selected { glow() } else { card_edge() }),
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
                glow(),
                1.0,
            );
        }
        let text_color = if hovered && !selected { text() } else { color };
        p.galley(rect.center() - galley.size() / 2.0, galley, text_color);
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
