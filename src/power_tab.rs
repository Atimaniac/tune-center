//! Zakładka "Power": pobór mocy procesora i karty graficznej oraz suma -
//! kafelki z plakietką obciążenia, zmianą z 5 s, średnią, maksimum, energią
//! sesji i wykresem z ostatnich 120 s. Pod kafelkami pasek sesji: czas,
//! zużyta energia, koszt i zerowanie licznika.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Vec2};

use crate::history::{self, History};
use crate::power::{Row, SharedMeter};
use crate::theme::{self, accent, muted, warn};

const GAP: f32 = 12.0;
const MARGIN: f32 = 14.0;

/// Klucz sumy w historii mocy.
pub const TOTAL_KEY: &str = "Razem";

/// Klucz podzespołu w historii mocy.
pub fn row_key(row: &Row) -> String {
    format!("{}|{}", row.title, row.hw)
}

/// Limity mocy do plakietki obciążenia (`None` = nieznany, bez plakietki).
pub struct Limits {
    /// Procesor: PL2 (Intel) albo PPT (AMD).
    pub cpu_w: Option<f32>,
    /// Karta graficzna: domyślny limit (TDP) ze sterownika.
    pub gpu_w: Option<f32>,
}

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

/// Koszt energii; dwa miejsca po przecinku, a poniżej grosza trzy.
fn fmt_cost(wh: f64, price_kwh: f32, currency: &str) -> String {
    let cost = wh / 1000.0 * price_kwh as f64;
    if cost < 0.01 && cost > 0.0 {
        format!("{cost:.3} {currency}")
    } else {
        format!("{cost:.2} {currency}")
    }
}

/// Plakietka obciążenia: udział bieżącej mocy w limicie.
pub fn load_state(watts: f32, limit: Option<f32>) -> Option<(&'static str, Color32)> {
    let frac = watts / limit.filter(|l| *l > 0.0)?;
    Some(if frac >= 0.8 {
        (crate::i18n::tr("WYSOKI", "HIGH"), warn())
    } else if frac >= 0.35 {
        (crate::i18n::tr("ŚREDNI", "MEDIUM"), accent())
    } else {
        (crate::i18n::tr("NISKI", "LOW"), theme::ok())
    })
}

/// Dane jednego kafelka.
struct Tile {
    title: String,
    hw: String,
    watts: f32,
    avg_w: f32,
    max_w: f32,
    wh: f64,
    key: String,
    limit: Option<f32>,
}

pub fn ui(
    ui: &mut egui::Ui,
    meter: &SharedMeter,
    history: &History,
    limits: Limits,
    (price, currency): (f32, &str),
) {
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

    let limit_of = |title: &str| match title {
        "Procesor" => limits.cpu_w,
        "Karta graficzna" => limits.gpu_w,
        _ => None,
    };
    let mut tiles: Vec<Tile> = rows
        .iter()
        .map(|r| Tile {
            title: crate::i18n::tr_dyn(r.title).to_uppercase(),
            hw: r.hw.clone(),
            watts: r.watts,
            avg_w: r.avg_w,
            max_w: r.max_w,
            wh: r.wh,
            key: row_key(r),
            limit: limit_of(r.title),
        })
        .collect();
    // Limit sumy tylko wtedy, gdy znamy limity wszystkich podzespołów.
    let total_limit = rows.iter().map(|r| limit_of(r.title)).sum::<Option<f32>>();
    tiles.push(Tile {
        title: crate::i18n::tr("RAZEM", "TOTAL").to_owned(),
        hw: crate::i18n::tr("procesor + karta graficzna", "CPU + graphics card").to_owned(),
        watts: total.watts,
        avg_w: total.avg_w,
        max_w: total.max_w,
        wh: total.wh,
        key: TOTAL_KEY.to_owned(),
        limit: total_limit,
    });

    theme::heading(
        ui,
        crate::icons::Icon::Power,
        crate::i18n::tr("Pobór mocy", "Power draw"),
    );
    ui.add_space(4.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // Kafelki obok siebie (tyle kolumn, ile się mieści), wysokość z
        // wolnego miejsca nad paskiem sesji.
        let avail = ui.available_width();
        let cols = (((avail + GAP) / (240.0 + GAP)).floor().max(1.0) as usize).min(tiles.len());
        let rows_n = tiles.len().div_ceil(cols);
        let inner_w = (avail + GAP) / cols as f32 - GAP - 2.0 * MARGIN - 3.0;
        let free = ui.available_height() - 110.0;
        let tile_h = ((free - GAP * (rows_n as f32 - 1.0)) / rows_n as f32).clamp(230.0, 300.0);
        let inner_h = tile_h - 2.0 * MARGIN - 3.0;

        for chunk in tiles.chunks(cols) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(GAP);
                for tile in chunk {
                    power_tile(ui, tile, history, Vec2::new(inner_w, inner_h));
                }
            });
            ui.add_space(GAP);
        }

        session_bar(ui, meter, total.wh, elapsed, price, currency);
        ui.add_space(8.0);
        theme::info(
            ui,
            crate::i18n::tr("Wartości obejmują tylko procesor i kartę graficzną, a nie cały komputer. Licznik działa od uruchomienia programu, także gdy okno jest schowane. Plakietka pokazuje udział mocy w limicie: procesor - PL2 (Intel) lub PPT (AMD), karta - domyślny limit sterownika.", "Values cover only the CPU and graphics card, not the whole computer. The counter runs from program start, including while the window is hidden. The badge shows the share of the power limit: CPU - PL2 (Intel) or PPT (AMD), card - the driver's default limit."),
        );
        ui.add_space(8.0);
    });
}

/// Mała statystyka: opis nad wartością.
fn stat(ui: &mut egui::Ui, label: &str, value: String) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(RichText::new(label).size(11.0).color(muted()));
        ui.label(
            RichText::new(value)
                .size(13.5)
                .strong()
                .color(theme::text()),
        );
    });
}

fn power_tile(ui: &mut egui::Ui, tile: &Tile, history: &History, inner: Vec2) {
    let series = history.get(&tile.key);
    let delta = series.map(|s| history::stats(s).2).unwrap_or(0.0);
    theme::card(12, MARGIN as i8).show(ui, |ui| {
        ui.set_width(inner.x);
        ui.set_height(inner.y);
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 2.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&tile.title)
                        .size(13.0)
                        .strong()
                        .color(theme::text()),
                );
                if let Some((text, color)) = load_state(tile.watts, tile.limit) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        history::badge(ui, text, color).on_hover_text(crate::trf!(
                            "{:.0}% limitu {:.0} W",
                            "{:.0}% of the {:.0} W limit",
                            tile.watts / tile.limit.unwrap_or(1.0) * 100.0,
                            tile.limit.unwrap_or(0.0)
                        ));
                    });
                }
            });
            ui.add(egui::Label::new(RichText::new(&tile.hw).size(11.5).color(muted())).truncate());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(fmt_watts(tile.watts))
                        .size(26.0)
                        .strong()
                        .color(accent()),
                );
                history::delta(ui, delta, " W", 1);
            });
            ui.add_space(4.0);
            ui.columns(3, |cols| {
                stat(
                    &mut cols[0],
                    crate::i18n::tr("Średnio", "Average"),
                    fmt_watts(tile.avg_w),
                );
                stat(
                    &mut cols[1],
                    crate::i18n::tr("Maks.", "Max"),
                    fmt_watts(tile.max_w),
                );
                stat(
                    &mut cols[2],
                    crate::i18n::tr("W sesji", "Session"),
                    fmt_energy(tile.wh),
                );
            });
            ui.add_space(6.0);
            let chart_h = (ui.available_height() - 18.0).max(24.0);
            history::chart(
                ui,
                Vec2::new(inner.x, chart_h),
                &history::as_points(series),
                accent(),
                history::Scale::FromZero(10.0),
                &|v| fmt_watts(v),
            );
            history::time_axis(ui);
        });
    });
}

/// Pasek sesji: czas, energia, koszt i zerowanie licznika.
fn session_bar(
    ui: &mut egui::Ui,
    meter: &SharedMeter,
    wh: f64,
    elapsed: Duration,
    price: f32,
    currency: &str,
) {
    let width = ui.available_width() - 2.0 * MARGIN - 3.0;
    theme::card(10, MARGIN as i8).show(ui, |ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let pair = |ui: &mut egui::Ui, label: &str, value: String| {
                ui.label(RichText::new(label).size(12.5).color(muted()));
                ui.label(
                    RichText::new(value)
                        .size(14.0)
                        .strong()
                        .color(theme::text()),
                );
            };
            pair(
                ui,
                crate::i18n::tr("Czas sesji", "Session time"),
                fmt_time(elapsed),
            );
            theme::vsep(ui, 20.0);
            pair(
                ui,
                crate::i18n::tr("Zużyta energia", "Energy used"),
                fmt_energy(wh),
            );
            if price > 0.0 {
                theme::vsep(ui, 20.0);
                pair(
                    ui,
                    &crate::trf!(
                        "Koszt ({price:.2} {currency}/kWh)",
                        "Cost ({price:.2} {currency}/kWh)"
                    ),
                    fmt_cost(wh, price, currency),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::primary_button(
                    ui,
                    Some(crate::icons::Icon::Refresh),
                    crate::i18n::tr("Wyzeruj licznik", "Reset counter"),
                    true,
                )
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
    });
}

#[cfg(test)]
mod tests {
    use super::{fmt_cost, load_state};

    #[test]
    fn cost_and_load() {
        assert_eq!(fmt_cost(1000.0, 0.9, "zł"), "0.90 zł");
        assert_eq!(fmt_cost(10.0, 0.9, "zł"), "0.009 zł");
        assert_eq!(fmt_cost(0.0, 0.9, "zł"), "0.00 zł");
        assert!(load_state(50.0, None).is_none());
        assert_eq!(load_state(10.0, Some(100.0)).unwrap().0, "NISKI");
        assert_eq!(load_state(50.0, Some(100.0)).unwrap().0, "ŚREDNI");
        assert_eq!(load_state(90.0, Some(100.0)).unwrap().0, "WYSOKI");
    }
}
