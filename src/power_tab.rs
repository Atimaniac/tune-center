//! Zakładka "Power": pobór mocy procesora i karty graficznej (bieżący,
//! średni, maksymalny), suma oraz licznik energii bieżącej sesji.
//! Układ dopasowuje się do szerokości okna.

use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, RichText, Sense, Vec2};

use crate::power::{Row, SharedMeter, Total};
use crate::theme::{self, accent, muted, track, warn};

const GAP: f32 = 14.0;
const MARGIN: f32 = 18.0;

fn fmt_watts(w: f32) -> String {
    format!("{w:.1} W")
}

fn fmt_energy(wh: f64) -> String {
    if wh >= 1000.0 {
        format!("{:.3} kWh", wh / 1000.0)
    } else {
        format!("{wh:.2} Wh")
    }
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

pub fn ui(ui: &mut egui::Ui, meter: &SharedMeter) {
    let (rows, total, elapsed) = {
        let m = meter.lock().unwrap();
        (m.rows(), m.total(), m.elapsed(Instant::now()))
    };

    if rows.is_empty() {
        ui.label(
            RichText::new(
                crate::i18n::tr("Nie wykryto czujników mocy procesora ani karty graficznej. Odczyt mocy procesora wymaga uruchomienia jako administrator i sterownika PawnIO.", "No CPU or graphics card power sensors detected. Reading CPU power requires running as administrator and the PawnIO driver."),
            )
            .color(warn()),
        );
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        let avail = ui.available_width();

        // --- podzespoły: tyle kolumn, ile mieści się w oknie ---
        section(ui, crate::i18n::tr("Pobór mocy", "Power draw"));
        let card_w = theme::grid_card_width(avail, 270.0, GAP, rows.len(), MARGIN);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(GAP);
            for row in &rows {
                component_card(ui, row, card_w);
            }
        });

        // --- suma ---
        ui.add_space(18.0);
        section(ui, crate::i18n::tr("Razem", "Total"));
        let total_w = theme::grid_card_width(avail, 270.0, GAP, 1, MARGIN);
        total_card(ui, &total, elapsed, total_w, meter);

        ui.add_space(10.0);
        ui.label(
            RichText::new(
                crate::i18n::tr("Wartości obejmują tylko procesor i kartę graficzną, a nie cały komputer. Licznik działa od uruchomienia programu, także gdy okno jest schowane.", "Values cover only the CPU and graphics card, not the whole computer. The counter runs from program start, including while the window is hidden."),
            )
            .size(11.5)
            .color(muted()),
        );
        ui.add_space(12.0);
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .size(17.0)
            .strong()
            .color(theme::text()),
    );
    ui.add_space(6.0);
}

/// Mała statystyka: opis nad wartością.
fn stat(ui: &mut egui::Ui, label: &str, value: String) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(11.5).color(muted()));
        ui.label(
            RichText::new(value)
                .size(16.0)
                .strong()
                .color(theme::text()),
        );
    });
}

/// Pasek: bieżący pobór względem maksimum sesji.
fn load_bar(ui: &mut egui::Ui, watts: f32, max_w: f32) {
    let (resp, painter) = ui.allocate_painter(Vec2::new(ui.available_width(), 6.0), Sense::hover());
    let frac = if max_w > 0.0 {
        (watts / max_w).clamp(0.0, 1.0)
    } else {
        0.0
    };
    painter.rect_filled(resp.rect, CornerRadius::same(3), track());
    let mut fill = resp.rect;
    fill.set_width(resp.rect.width() * frac);
    painter.rect_filled(fill, CornerRadius::same(3), accent());
}

fn component_card(ui: &mut egui::Ui, row: &Row, width: f32) {
    theme::card(14, MARGIN as i8).show(ui, |ui| {
        ui.set_width(width);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr_dyn(row.title))
                    .size(17.0)
                    .strong()
                    .color(theme::text()),
            );
            ui.add(egui::Label::new(RichText::new(&row.hw).size(11.5).color(muted())).truncate());
            ui.add_space(8.0);
            ui.label(
                RichText::new(fmt_watts(row.watts))
                    .size(34.0)
                    .strong()
                    .color(accent()),
            );
            ui.add_space(4.0);
            load_bar(ui, row.watts, row.max_w);
            ui.add_space(10.0);
            ui.columns(3, |cols| {
                stat(
                    &mut cols[0],
                    crate::i18n::tr("Średnio", "Average"),
                    fmt_watts(row.avg_w),
                );
                stat(
                    &mut cols[1],
                    crate::i18n::tr("Maksymalnie", "Maximum"),
                    fmt_watts(row.max_w),
                );
                stat(
                    &mut cols[2],
                    crate::i18n::tr("W tej sesji", "This session"),
                    fmt_energy(row.wh),
                );
            });
        });
    });
}

fn total_card(
    ui: &mut egui::Ui,
    total: &Total,
    elapsed: Duration,
    width: f32,
    meter: &SharedMeter,
) {
    theme::card(14, MARGIN as i8).show(ui, |ui| {
        ui.set_width(width);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("Suma poboru mocy", "Total power draw"))
                    .color(muted()),
            );
            ui.label(
                RichText::new(fmt_watts(total.watts))
                    .size(40.0)
                    .strong()
                    .color(accent()),
            );
            ui.add_space(4.0);
            load_bar(ui, total.watts, total.max_w);
            ui.add_space(12.0);

            let stats: [(&str, String); 4] = [
                (
                    crate::i18n::tr("Średnio", "Average"),
                    fmt_watts(total.avg_w),
                ),
                (
                    crate::i18n::tr("Maksymalnie", "Maximum"),
                    fmt_watts(total.max_w),
                ),
                (
                    crate::i18n::tr("Zużyta energia w tej sesji", "Energy used this session"),
                    fmt_energy(total.wh),
                ),
                (
                    crate::i18n::tr("Czas sesji", "Session time"),
                    fmt_time(elapsed),
                ),
            ];
            // W wąskim oknie statystyki układają się w dwóch wierszach.
            let per_row = if width >= 560.0 { 4 } else { 2 };
            for chunk in stats.chunks(per_row) {
                ui.columns(per_row, |cols| {
                    for (col, (label, value)) in cols.iter_mut().zip(chunk) {
                        stat(col, label, value.clone());
                    }
                });
                ui.add_space(6.0);
            }

            ui.add_space(4.0);
            if ui
                .button(crate::i18n::tr("Wyzeruj licznik", "Reset counter"))
                .on_hover_text(crate::i18n::tr(
                    "Zeruje energię, średnie, maksima i czas sesji",
                    "Resets energy, averages, maximums and session time",
                ))
                .clicked()
            {
                meter.lock().unwrap().reset(Instant::now());
            }
        });
    });
}
