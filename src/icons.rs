//! Proste ikony rysowane wektorowo (bez czcionki z ikonami i bez plików):
//! zakładki, nagłówki sekcji, przyciski i pasek stanu.

use std::f32::consts::{PI, TAU};

use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Rect, Sense, Shape, Stroke};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Icon {
    Gpu,
    Cpu,
    Fan,
    Sensors,
    Power,
    Options,
    About,
    Refresh,
    Download,
    Thermo,
    Target,
    Check,
    Swap,
    Gauge,
    Chip,
}

/// Rysuje ikonę wpisaną w kwadrat `rect`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    // Punkt we współrzędnych ikony: (-0.5..0.5) względem środka.
    let p = |x: f32, y: f32| pos2(c.x + x * s, c.y + y * s);
    let w = (s * 0.085).max(1.3);
    let stroke = Stroke::new(w, color);

    match icon {
        Icon::Cpu | Icon::Chip => {
            let body = Rect::from_center_size(c, vec2(s * 0.56, s * 0.56));
            painter.rect_stroke(
                body,
                egui::CornerRadius::same((s * 0.08) as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            if icon == Icon::Cpu {
                let inner = Rect::from_center_size(c, vec2(s * 0.22, s * 0.22));
                painter.rect_filled(inner, egui::CornerRadius::same(1), color);
            }
            for i in [-0.16, 0.0, 0.16] {
                painter.line_segment([p(i, -0.28), p(i, -0.44)], stroke);
                painter.line_segment([p(i, 0.28), p(i, 0.44)], stroke);
                painter.line_segment([p(-0.28, i), p(-0.44, i)], stroke);
                painter.line_segment([p(0.28, i), p(0.44, i)], stroke);
            }
        }
        Icon::Gpu => {
            let body = Rect::from_min_max(p(-0.44, -0.24), p(0.44, 0.2));
            painter.rect_stroke(
                body,
                egui::CornerRadius::same((s * 0.06) as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.circle_stroke(p(-0.12, -0.02), s * 0.13, stroke);
            painter.circle_stroke(p(0.2, -0.02), s * 0.09, stroke);
            // Złącze PCIe pod kartą.
            painter.line_segment([p(-0.3, 0.32), p(0.18, 0.32)], stroke);
        }
        Icon::Fan => {
            // Obudowa i trzy pełne łopatki (koła wokół piasty).
            painter.circle_stroke(c, s * 0.46, Stroke::new(w * 0.9, color));
            for k in 0..3 {
                let a = k as f32 * TAU / 3.0 - PI / 2.0;
                painter.circle_filled(c + vec2(a.cos(), a.sin()) * s * 0.21, s * 0.15, color);
            }
            painter.circle_filled(c, s * 0.1, color);
        }
        Icon::Sensors => {
            for (i, h) in [0.35, 0.6, 0.85, 0.5].iter().enumerate() {
                let x = -0.3 + i as f32 * 0.2;
                painter.line_segment(
                    [p(x, 0.42), p(x, 0.42 - h * 0.8)],
                    Stroke::new(w * 1.3, color),
                );
            }
        }
        Icon::Power => {
            painter.add(Shape::convex_polygon(
                vec![p(0.12, -0.48), p(-0.28, 0.08), p(0.04, 0.08)],
                color,
                Stroke::NONE,
            ));
            painter.add(Shape::convex_polygon(
                vec![p(-0.04, -0.08), p(0.28, -0.08), p(-0.12, 0.48)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Options => {
            for k in 0..8 {
                let a = k as f32 * TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment(
                    [c + d * s * 0.26, c + d * s * 0.44],
                    Stroke::new(w * 1.6, color),
                );
            }
            painter.circle_stroke(c, s * 0.28, stroke);
            painter.circle_stroke(c, s * 0.1, stroke);
        }
        Icon::About => {
            painter.circle_stroke(c, s * 0.42, stroke);
            painter.circle_filled(p(0.0, -0.2), w * 0.8, color);
            painter.line_segment([p(0.0, -0.06), p(0.0, 0.24)], stroke);
        }
        Icon::Refresh => {
            arc(painter, c, s * 0.32, 0.35 * PI, 1.9 * PI, stroke);
            let a = 1.9 * PI;
            let tip = c + vec2(a.cos(), a.sin()) * s * 0.32;
            painter.add(Shape::convex_polygon(
                vec![
                    tip + vec2(-s * 0.16, -s * 0.02),
                    tip + vec2(s * 0.04, -s * 0.16),
                    tip + vec2(s * 0.06, s * 0.06),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Download => {
            painter.line_segment([p(0.0, -0.42), p(0.0, 0.14)], stroke);
            painter.line(vec![p(-0.2, -0.06), p(0.0, 0.16), p(0.2, -0.06)], stroke);
            painter.line(
                vec![p(-0.38, 0.12), p(-0.38, 0.38), p(0.38, 0.38), p(0.38, 0.12)],
                stroke,
            );
        }
        Icon::Thermo => {
            painter.circle_stroke(p(0.0, 0.26), s * 0.15, stroke);
            painter.circle_filled(p(0.0, 0.26), s * 0.08, color);
            painter.line_segment([p(-0.08, 0.13), p(-0.08, -0.36)], stroke);
            painter.line_segment([p(0.08, 0.13), p(0.08, -0.36)], stroke);
            arc(painter, p(0.0, -0.36), s * 0.08, PI, TAU, stroke);
            painter.line_segment([p(0.0, 0.2), p(0.0, -0.18)], stroke);
        }
        Icon::Target => {
            painter.circle_stroke(c, s * 0.3, stroke);
            painter.circle_filled(c, s * 0.07, color);
            for (a, b) in [
                ((0.0, -0.48), (0.0, -0.16)),
                ((0.0, 0.16), (0.0, 0.48)),
                ((-0.48, 0.0), (-0.16, 0.0)),
                ((0.16, 0.0), (0.48, 0.0)),
            ] {
                painter.line_segment([p(a.0, a.1), p(b.0, b.1)], stroke);
            }
        }
        Icon::Check => {
            painter.line(
                vec![p(-0.34, 0.0), p(-0.1, 0.24), p(0.36, -0.24)],
                Stroke::new(w * 1.2, color),
            );
        }
        Icon::Swap => {
            painter.line_segment([p(-0.36, -0.14), p(0.36, -0.14)], stroke);
            painter.line(vec![p(0.18, -0.3), p(0.36, -0.14), p(0.18, 0.02)], stroke);
            painter.line_segment([p(0.36, 0.16), p(-0.36, 0.16)], stroke);
            painter.line(vec![p(-0.18, 0.0), p(-0.36, 0.16), p(-0.18, 0.32)], stroke);
        }
        Icon::Gauge => {
            arc(painter, p(0.0, 0.12), s * 0.4, PI, TAU, stroke);
            painter.line_segment([p(0.0, 0.12), p(0.22, -0.14)], stroke);
            painter.circle_filled(p(0.0, 0.12), w, color);
            painter.line_segment([p(-0.4, 0.12), p(-0.3, 0.12)], stroke);
            painter.line_segment([p(0.3, 0.12), p(0.4, 0.12)], stroke);
        }
    }
}

fn arc(painter: &Painter, c: Pos2, r: f32, from: f32, to: f32, stroke: Stroke) {
    let n = 24;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = from + (to - from) * i as f32 / n as f32;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    painter.line(pts, stroke);
}

/// Ikona jako osobny element układu.
pub fn show(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect, icon, color);
    }
    response
}
