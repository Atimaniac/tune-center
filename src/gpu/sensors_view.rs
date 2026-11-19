//! Widok czujników karty wzorowany na zakładce "Sensors" programu GPU-Z:
//! kafelek = nazwa, wartość i wykres ostatnich dwóch minut (jak SENSORS i POWER).
//!
//! Źródła: NVML (oficjalne API monitorowania Nvidii), NVAPI (temperatury,
//! zegary, obciążenie silników, napięcie, topologia mocy), Windows (pamięć
//! systemu) i proces czujników (temperatura procesora).

use std::collections::VecDeque;

use eframe::egui::{self, Color32, RichText, Vec2};

use super::backend::Monitor;
use super::sampler::{Samples, HISTORY};
use crate::bridge::Snapshot;
use crate::history;
use crate::i18n::tr;
use crate::theme;

/// Co pokazuje kolumna wartości (jak lista rozwijana w GPU-Z).
#[derive(Clone, Copy, PartialEq, Default)]
enum Show {
    #[default]
    Current,
    Min,
    Max,
    Avg,
}

#[derive(Default)]
pub struct SensorsView {
    show: Show,
    /// Odczyty spoza karty, zbierane przy każdym nowym odczycie karty.
    sys_mem_mb: VecDeque<Option<f32>>,
    seen: u64,
}

/// Jeden wiersz: nazwa, opis źródła, jednostka, wartość z odczytu.
struct Row {
    name: &'static str,
    source: &'static str,
    unit: &'static str,
    decimals: usize,
    /// Stała górna granica wykresu (np. 100 %); `None` = największa wartość.
    max: Option<f32>,
    value: fn(&Monitor) -> Option<f32>,
}

fn rows() -> Vec<Row> {
    macro_rules! row {
        ($name:expr, $src:expr, $unit:expr, $dec:expr, $max:expr, $f:expr) => {
            Row {
                name: $name,
                source: $src,
                unit: $unit,
                decimals: $dec,
                max: $max,
                value: $f,
            }
        };
    }
    let pct = Some(100.0);
    vec![
        row!(tr("Zegar GPU", "GPU Clock"), "NVAPI", "MHz", 0, None, |m| m
            .core_mhz),
        row!(
            tr("Zegar pamięci", "Memory Clock"),
            "NVAPI",
            "MHz",
            0,
            None,
            |m| m.mem_mhz
        ),
        row!(
            tr("Zegar wideo", "Video Clock"),
            "NVML",
            "MHz",
            0,
            None,
            |m| m.details.video_mhz.map(|v| v as f32)
        ),
        row!(
            tr("Temperatura GPU", "GPU Temperature"),
            "NVAPI",
            "°C",
            1,
            None,
            |m| m.details.core_precise_c.or(m.temp_c.map(|v| v as f32))
        ),
        row!("Hot Spot", "NVAPI", "°C", 1, None, |m| m.hotspot_c),
        row!(
            tr("Temperatura pamięci", "Memory Temperature"),
            "NVAPI / NVML",
            "°C",
            1,
            None,
            |m| m.details.mem_temp_c
        ),
        row!(
            tr("Pamięć zajęta", "Memory Used"),
            "NVML",
            "MB",
            0,
            None,
            |m| m.details.mem_used_mb.map(|v| v as f32)
        ),
        row!("PCIe TX", "NVML", "MB/s", 1, None, |m| m
            .details
            .pcie_tx_mbs),
        row!("PCIe RX", "NVML", "MB/s", 1, None, |m| m
            .details
            .pcie_rx_mbs),
        row!(
            tr("Obciążenie GPU", "GPU Load"),
            "NVAPI",
            "%",
            0,
            pct,
            |m| m.usage_pct.map(|v| v as f32)
        ),
        row!(
            tr("Obciążenie kontrolera pamięci", "Memory Controller Load"),
            "NVAPI / NVML",
            "%",
            0,
            pct,
            |m| m.details.mem_ctrl_pct.map(|v| v as f32)
        ),
        row!(
            tr("Obciążenie silnika wideo", "Video Engine Load"),
            "NVAPI",
            "%",
            0,
            pct,
            |m| m.details.video_pct.map(|v| v as f32)
        ),
        row!(
            tr("Obciążenie magistrali", "Bus Interface Load"),
            "NVAPI",
            "%",
            0,
            pct,
            |m| m.details.bus_pct.map(|v| v as f32)
        ),
        row!(
            tr("Enkoder wideo", "Video Encoder"),
            "NVML",
            "%",
            0,
            pct,
            |m| m.details.encoder_pct.map(|v| v as f32)
        ),
        row!(
            tr("Dekoder wideo", "Video Decoder"),
            "NVML",
            "%",
            0,
            pct,
            |m| m.details.decoder_pct.map(|v| v as f32)
        ),
        row!(
            tr("Pobór mocy karty", "Board Power Draw"),
            "NVML",
            "W",
            1,
            None,
            |m| m.power_w
        ),
        row!(
            tr("Pobór mocy układu GPU", "GPU Chip Power Draw"),
            "NVAPI + NVML",
            "W",
            1,
            None,
            |m| m.details.chip_power_w
        ),
        row!(
            tr("Pobór mocy (% TDP)", "Power Consumption (% TDP)"),
            "NVML",
            "%",
            1,
            None,
            |m| m.details.power_pct_tdp
        ),
        row!(
            tr("Napięcie GPU", "GPU Voltage"),
            "NVAPI",
            "V",
            4,
            None,
            |m| m.voltage_mv.map(|v| v / 1000.0)
        ),
    ]
}

/// Zajęta pamięć systemu (MB) z Windows.
#[cfg(windows)]
fn system_memory_used_mb() -> Option<f32> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut s = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: struktura z poprawnym dwLength.
    unsafe { GlobalMemoryStatusEx(&mut s) }.ok()?;
    Some(((s.ullTotalPhys - s.ullAvailPhys) >> 20) as f32)
}

#[cfg(not(windows))]
fn system_memory_used_mb() -> Option<f32> {
    None
}

fn push(q: &mut VecDeque<Option<f32>>, v: Option<f32>) {
    if q.len() >= HISTORY {
        q.pop_front();
    }
    q.push_back(v);
}

fn format(v: f32, decimals: usize, unit: &str) -> String {
    if unit == "MB/s" && v >= 1000.0 {
        return format!("{:.2} GB/s", v / 1000.0);
    }
    format!("{v:.decimals$} {unit}")
}

/// Dane jednego kafelka.
struct Tile {
    name: String,
    source: String,
    unit: &'static str,
    decimals: usize,
    max: Option<f32>,
    series: Vec<Option<f32>>,
    /// Limit mocy (TDP) - do plakietki obciążenia kafelka mocy karty.
    tdp_w: Option<f32>,
}

impl Tile {
    fn fmt(&self, v: f32) -> String {
        format(v, self.decimals, self.unit)
    }

    /// Plakietka: temperatury - stan, moc karty - udział w TDP, % TDP - to samo.
    fn badge(&self, v: f32) -> Option<(&'static str, Color32)> {
        if self.unit == "°C" {
            Some(crate::sensors::temp_state(v))
        } else if self.unit == "W" && self.tdp_w.is_some() && self.name.contains("karty")
            || self.unit == "W" && self.tdp_w.is_some() && self.name.contains("Board")
        {
            crate::power_tab::load_state(v, self.tdp_w)
        } else if self.name.contains("TDP") {
            crate::power_tab::load_state(v, Some(100.0))
        } else {
            None
        }
    }

    fn color(&self, v: f32) -> Color32 {
        if self.unit == "°C" {
            theme::temp_color(v)
        } else {
            theme::accent()
        }
    }

    fn scale(&self) -> history::Scale {
        match (self.max, self.unit) {
            (Some(max), _) => history::Scale::Fixed(0.0, max),
            (None, "°C") => history::Scale::Around(4.0),
            (None, "V") => history::Scale::Around(0.1),
            _ => history::Scale::FromZero(1.0),
        }
    }
}

impl SensorsView {
    pub fn ui(&mut self, ui: &mut egui::Ui, samples: &Samples, _snap: Option<&Snapshot>) {
        if samples.seq != self.seen {
            self.seen = samples.seq;
            push(&mut self.sys_mem_mb, system_memory_used_mb());
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Pokaż:", "Show:")).color(theme::muted()));
            for (mode, text) in [
                (Show::Current, tr("bieżące", "current")),
                (Show::Min, tr("najmniejsze", "lowest")),
                (Show::Max, tr("największe", "highest")),
                (Show::Avg, tr("średnie", "average")),
            ] {
                if theme::sub_tab(ui, text, self.show == mode).clicked() {
                    self.show = mode;
                }
            }
            ui.label(
                RichText::new(crate::trf!(
                    "(ostatnie {} s, odczyt co 1 s; najedź na wykres, żeby zobaczyć wartość)",
                    "(last {} s, read every 1 s; hover a chart to see the value)",
                    HISTORY
                ))
                .size(11.5)
                .color(theme::muted()),
            );
        });
        ui.add_space(6.0);

        let history = &samples.history;
        let last = history.back();
        let tdp_w = last.and_then(|m| m.details.tdp_w);

        // Odczyty bez wykresu: pasek "nazwa: wartość" nad kafelkami.
        let mut chips: Vec<(String, String, String)> = Vec::new();
        if let Some(m) = last {
            if let Some(limit) = &m.details.limit {
                chips.push((
                    tr("Ograniczenie", "PerfCap").into(),
                    limit.clone(),
                    "NVML".into(),
                ));
            }
            if let Some(p) = m.details.pstate {
                chips.push((
                    tr("Stan wydajności", "Performance State").into(),
                    format!("P{p}"),
                    "NVML".into(),
                ));
            }
            if let (Some(g), Some(w)) = (m.details.pcie_gen, m.details.pcie_width) {
                chips.push((
                    tr("Łącze PCIe", "PCIe Link").into(),
                    format!("Gen {g} x{w}"),
                    "NVML".into(),
                ));
            }
            if let Some(total) = m.details.mem_total_mb {
                chips.push((
                    tr("Pamięć karty", "Memory Size").into(),
                    format!("{total} MB"),
                    "NVML".into(),
                ));
            }
            if let Some(tdp) = tdp_w {
                chips.push((
                    tr("Limit mocy (TDP)", "Power Limit (TDP)").into(),
                    format!("{tdp:.0} W"),
                    "NVML".into(),
                ));
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            for (name, value, source) in &chips {
                ui.label(
                    RichText::new(format!("  {name}: {value}  "))
                        .size(12.5)
                        .color(theme::text())
                        .background_color(theme::well()),
                )
                .on_hover_text(crate::trf!("Źródło: {source}", "Source: {source}"));
            }
        });
        ui.add_space(8.0);

        // Kafelki z wykresem - tylko czujniki, które karta udostępnia.
        let mut tiles: Vec<Tile> = rows()
            .into_iter()
            .filter_map(|row| {
                let series: Vec<Option<f32>> = history.iter().map(row.value).collect();
                series.iter().any(Option::is_some).then(|| Tile {
                    name: row.name.to_owned(),
                    source: row.source.to_owned(),
                    unit: row.unit,
                    decimals: row.decimals,
                    max: row.max,
                    series,
                    tdp_w,
                })
            })
            .collect();
        let ram: Vec<Option<f32>> = self.sys_mem_mb.iter().copied().collect();
        if ram.iter().any(Option::is_some) {
            tiles.push(Tile {
                name: tr("Pamięć systemu zajęta", "System Memory Used").to_owned(),
                source: "Windows".into(),
                unit: "MB",
                decimals: 0,
                max: None,
                series: ram,
                tdp_w: None,
            });
        }

        let show = self.show;
        theme::card_grid(ui, &tiles, 250.0, 10.0, 6, 10, 12, |ui, _, tile, width| {
            tile_ui(ui, tile, width, show);
        });
    }
}

/// Kafelek czujnika: nazwa z plakietką, wartość (wg trybu) ze zmianą z 5 s,
/// minimum i maksimum oraz wykres 120 s z podpowiedzią po najechaniu.
fn tile_ui(ui: &mut egui::Ui, tile: &Tile, width: f32, show: Show) {
    let values: Vec<f32> = tile.series.iter().flatten().copied().collect();
    let shown = match show {
        Show::Current => tile.series.last().copied().flatten(),
        Show::Min => values.iter().copied().reduce(f32::min),
        Show::Max => values.iter().copied().reduce(f32::max),
        Show::Avg => (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32),
    };
    let (min, max, delta) = history::stats_points(&tile.series).unwrap_or((0.0, 0.0, 0.0));
    let current = tile.series.last().copied().flatten();
    let color = current.map(|v| tile.color(v)).unwrap_or(theme::accent());

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 2.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(tile.name.to_uppercase())
                        .size(12.0)
                        .strong()
                        .color(theme::text()),
                )
                .truncate(),
            )
            .on_hover_text(crate::trf!("Źródło: {}", "Source: {}", tile.source));
            if let Some((text, c)) = current.and_then(|v| tile.badge(v)) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    history::badge(ui, text, c);
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(shown.map(|v| tile.fmt(v)).unwrap_or_else(|| "—".into()))
                    .size(20.0)
                    .strong()
                    .color(color),
            );
            if show == Show::Current {
                let unit = format!(" {}", tile.unit);
                history::delta(ui, delta, &unit, tile.decimals);
            }
        });
        ui.label(
            RichText::new(format!("MIN {} / MAX {}", tile.fmt(min), tile.fmt(max)))
                .size(11.0)
                .color(theme::muted()),
        );
        ui.add_space(4.0);
        history::chart(
            ui,
            Vec2::new(width, 56.0),
            &tile.series,
            color,
            tile.scale(),
            &|v| tile.fmt(v),
        );
        history::time_axis(ui);
    });
}
