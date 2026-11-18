//! Historia odczytów z ostatnich 120 s (co sekundę) i wspólne elementy
//! kafelków z wykresem w zakładkach SENSORS i POWER: wykres liniowy z
//! wypełnieniem, plakietka stanu i zmiana z ostatnich 5 s.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, pos2, Color32, CornerRadius, Painter, Rect, RichText, Sense, Stroke, Vec2,
};

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
pub fn delta(ui: &mut egui::Ui, delta: f32, unit: &str) {
    if delta.abs() < 0.1 {
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
        RichText::new(format!("{:.1}{unit}", delta.abs()))
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

/// Wykres liniowy z wypełnieniem i przerywaną siatką. Oś pozioma: 120 s
/// (najnowszy odczyt przy prawej krawędzi). Oś pionowa: od zera
/// (`from_zero`, np. moc) albo wokół min-maks serii z zapasem `min_span`.
pub fn chart(
    painter: &Painter,
    rect: Rect,
    series: Option<&VecDeque<f32>>,
    color: Color32,
    from_zero: bool,
    min_span: f32,
) {
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
    let Some(s) = series.filter(|s| s.len() >= 2) else {
        return;
    };
    let (min, max, _) = stats(s);
    let (lo, hi) = if from_zero {
        (0.0, (max * 1.15).max(min_span))
    } else {
        let mid = (min + max) / 2.0;
        let half = ((max - min) / 2.0 + min_span / 4.0).max(min_span / 2.0);
        (mid - half, mid + half)
    };
    let step = rect.width() / (LEN - 1) as f32;
    let start = LEN - s.len();
    let points: Vec<egui::Pos2> = s
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = rect.left() + (start + i) as f32 * step;
            let y = rect.bottom() - 3.0 - (v - lo) / (hi - lo) * (rect.height() - 8.0);
            pos2(x, y.clamp(rect.top(), rect.bottom()))
        })
        .collect();

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
    let last = *points.last().unwrap();
    painter.add(egui::Shape::line(points, Stroke::new(1.8_f32, color)));
    painter.circle_filled(last, 3.0, color);
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
