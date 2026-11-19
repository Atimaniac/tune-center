//! Historia odczytów z ostatnich 120 s (co sekundę) i wspólne elementy
//! kafelków z wykresem w zakładkach SENSORS i POWER: wykres liniowy z
//! wypełnieniem, plakietka stanu i zmiana z ostatnich 5 s.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, Color32, CornerRadius, RichText, Sense, Stroke, Vec2};

use crate::theme::{self, muted, track};

/// Liczba zapamiętanych odczytów (co sekundę, czyli 120 s).
pub const LEN: usize = 120;

/// Serie odczytów pod kluczami (np. "Procesor|Intel Core i5").
#[derive(Default)]
pub struct History {
    last: Option<Instant>,
    series: HashMap<String, VecDeque<f32>>,
}

impl History {
    /// Czy minęła sekunda od ostatniego zapisu.
    pub fn due(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1))
    }

    /// Dopisuje odczyty; klucze, których tym razem nie ma, wypadają.
    pub fn record(&mut self, values: &[(String, f32)], now: Instant) {
        self.last = Some(now);
        for (key, value) in values {
            let s = self.series.entry(key.clone()).or_default();
            s.push_back(*value);
            while s.len() > LEN {
                s.pop_front();
            }
        }
        self.series
            .retain(|k, _| values.iter().any(|(key, _)| key == k));
    }

    pub fn get(&self, key: &str) -> Option<&VecDeque<f32>> {
        self.series.get(key)
    }
}

/// Minimum, maksimum i zmiana z ostatnich 5 s.
pub fn stats(series: &VecDeque<f32>) -> (f32, f32, f32) {
    let min = series.iter().copied().fold(f32::INFINITY, f32::min);
    let max = series.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let n = series.len();
    let delta = if n > 5 {
        series[n - 1] - series[n - 6]
    } else {
        0.0
    };
    (min, max, delta)
}

/// Plakietka w kształcie pigułki (np. "NORMALNA").
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let font = egui::FontId::proportional(11.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size() + Vec2::new(16.0, 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(255), color.gamma_multiply(0.18));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

/// Zmiana z ostatnich 5 s: trójkąt (rysowany - domyślna czcionka nie ma
/// znaków ▲▼) i wartość. Wzrost pomarańczowy, spadek zielony.
pub fn delta(ui: &mut egui::Ui, delta: f32, unit: &str, decimals: usize) {
    if delta.abs() < 0.5 * 10f32.powi(-(decimals as i32)) {
        return;
    }
    let up = delta > 0.0;
    let c = if up { theme::warn() } else { theme::ok() };
    let (r, _) = ui.allocate_exact_size(Vec2::new(9.0, 9.0), Sense::hover());
    let pts = if up {
        vec![
            pos2(r.center().x, r.top()),
            pos2(r.right(), r.bottom()),
            pos2(r.left(), r.bottom()),
        ]
    } else {
        vec![
            pos2(r.left(), r.top()),
            pos2(r.right(), r.top()),
            pos2(r.center().x, r.bottom()),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, c, Stroke::NONE));
    ui.label(
        RichText::new(format!("{:.decimals$}{unit}", delta.abs()))
            .size(11.5)
            .color(c),
    );
}

/// Podpisy osi czasu pod wykresem.
pub fn time_axis(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let small = |t: &str| RichText::new(t).size(11.0).color(muted());
        ui.label(small(crate::i18n::tr("120 s temu", "120 s ago")));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(small(crate::i18n::tr("teraz", "now")));
        });
    });
}

/// Zakres osi pionowej wykresu.
#[derive(Clone, Copy)]
pub enum Scale {
    /// Wokół min-maks serii, co najmniej `min_span` wysokości (np. temperatury).
    Around(f32),
    /// Od zera do maksimum serii z zapasem, co najmniej do podanej wartości (np. moc).
    FromZero(f32),
    /// Stały zakres (np. 0-100 %).
    Fixed(f32, f32),
}

/// Seria z historii jako lista z lukami (wspólny format wykresów).
pub fn as_points(series: Option<&VecDeque<f32>>) -> Vec<Option<f32>> {
    series
        .map(|s| s.iter().map(|v| Some(*v)).collect())
        .unwrap_or_default()
}

/// Minimum, maksimum i zmiana z ostatnich 5 s dla serii z lukami.
pub fn stats_points(series: &[Option<f32>]) -> Option<(f32, f32, f32)> {
    let values: Vec<f32> = series.iter().flatten().copied().collect();
    let min = values.iter().copied().reduce(f32::min)?;
    let max = values.iter().copied().reduce(f32::max)?;
    let n = series.len();
    let delta = match (
        series.last().copied().flatten(),
        n.checked_sub(6).and_then(|i| series[i]),
    ) {
        (Some(now), Some(before)) => now - before,
        _ => 0.0,
    };
    Some((min, max, delta))
}

/// Wykres liniowy z wypełnieniem i przerywaną siatką, o rozmiarze `size`.
/// Oś pozioma: 120 s (najnowszy odczyt przy prawej krawędzi), luki w serii
/// (`None`) są pomijane. Po najechaniu kursorem: pionowa linia, punkt i dymek
/// z wartością (`fmt`) i czasem odczytu.
pub fn chart(
    ui: &mut egui::Ui,
    size: Vec2,
    series: &[Option<f32>],
    color: Color32,
    scale: Scale,
    fmt: &dyn Fn(f32) -> String,
) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect.expand(4.0));
    painter.rect_filled(rect, CornerRadius::same(4), theme::well());
    let grid = Stroke::new(1.0_f32, track());
    for f in [0.25, 0.5, 0.75] {
        let y = rect.top() + rect.height() * f;
        painter.extend(egui::Shape::dashed_line(
            &[pos2(rect.left(), y), pos2(rect.right(), y)],
            grid,
            4.0,
            4.0,
        ));
    }
    let Some((min, max, _)) = stats_points(series) else {
        return;
    };
    let (lo, hi) = match scale {
        Scale::FromZero(min_top) => (0.0, (max * 1.15).max(min_top)),
        Scale::Fixed(lo, hi) => (lo, hi),
        Scale::Around(min_span) => {
            let mid = (min + max) / 2.0;
            let half = ((max - min) / 2.0 + min_span / 4.0).max(min_span / 2.0);
            (mid - half, mid + half)
        }
    };
    let step = rect.width() / (LEN - 1) as f32;
    let start = LEN.saturating_sub(series.len());
    let x_of = |i: usize| rect.left() + (start + i) as f32 * step;
    let y_of = |v: f32| {
        (rect.bottom() - 3.0 - (v - lo) / (hi - lo) * (rect.height() - 8.0))
            .clamp(rect.top(), rect.bottom())
    };
    let points: Vec<egui::Pos2> = series
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| pos2(x_of(i), y_of(v))))
        .collect();

    if points.len() >= 2 {
        // Wypełnienie pod linią: pas trapezów do dolnej krawędzi.
        let mut mesh = egui::Mesh::default();
        let fill = color.gamma_multiply(0.14);
        for p in &points {
            mesh.colored_vertex(*p, fill);
            mesh.colored_vertex(pos2(p.x, rect.bottom()), fill);
        }
        for i in 0..points.len() as u32 - 1 {
            let a = i * 2;
            mesh.add_triangle(a, a + 1, a + 2);
            mesh.add_triangle(a + 1, a + 3, a + 2);
        }
        painter.add(mesh);
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(1.8_f32, color),
        ));
    }
    if let Some(last) = points.last() {
        painter.circle_filled(*last, 3.0, color);
    }

    // Wartość w miejscu wskazanym kursorem.
    if let Some(pos) = response.hover_pos() {
        let i = ((pos.x - rect.left()) / step).round() as isize - start as isize;
        if i >= 0 && (i as usize) < series.len() {
            let i = i as usize;
            let x = x_of(i);
            painter.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(1.0_f32, theme::text().gamma_multiply(0.6)),
            );
            let ago = series.len() - 1 - i;
            let text = match series[i] {
                Some(v) => {
                    painter.circle_filled(pos2(x, y_of(v)), 4.0, color);
                    painter.circle_stroke(
                        pos2(x, y_of(v)),
                        4.0,
                        Stroke::new(1.5_f32, theme::text()),
                    );
                    fmt(v)
                }
                None => "—".to_owned(),
            };
            response.on_hover_ui_at_pointer(|ui| {
                ui.label(RichText::new(text).strong().size(14.0));
                ui.label(
                    RichText::new(if ago == 0 {
                        crate::i18n::tr("teraz", "now").to_owned()
                    } else {
                        crate::trf!("{} s temu", "{} s ago", ago)
                    })
                    .size(11.5)
                    .color(muted()),
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{stats, History};
    use std::time::{Duration, Instant};

    #[test]
    fn keeps_120_seconds() {
        let mut h = History::default();
        let t0 = Instant::now();
        assert!(h.due(t0));
        for i in 0..130 {
            h.record(
                &[("cpu".to_owned(), 40.0 + (i % 10) as f32)],
                t0 + Duration::from_secs(i),
            );
        }
        assert!(!h.due(t0 + Duration::from_millis(129_500)));
        let s = h.get("cpu").unwrap();
        assert_eq!(s.len(), 120);
        let (min, max, delta) = stats(s);
        assert_eq!((min, max), (40.0, 49.0));
        // Ostatni odczyt 49 (i = 129), 5 s wcześniej 44.
        assert_eq!(delta, 5.0);
        // Klucz, którego już nie ma, wypada z historii.
        h.record(&[], t0 + Duration::from_secs(200));
        assert!(h.get("cpu").is_none());
    }
}
