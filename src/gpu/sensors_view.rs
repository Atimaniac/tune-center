//! Widok czujników karty wzorowany na zakładce "Sensors" programu GPU-Z:
//! wiersz = nazwa, wartość i wykres ostatnich dwóch minut na całą szerokość
//! (wykres jak w SENSORS i POWER). Mniej istotne odczyty są pod "Ukryte".
//!
//! Źródła: NVML (oficjalne API monitorowania Nvidii), NVAPI (temperatury,
//! zegary, obciążenie silników, napięcie, topologia mocy), Windows (pamięć
//! systemu) i usługa czujników (temperatura procesora).

use std::collections::VecDeque;

use eframe::egui::{self, Color32, RichText, Vec2};

use super::backend::Monitor;
use super::sampler::{Samples, HISTORY};
use crate::bridge::Snapshot;
use crate::history;
use crate::i18n::tr;
use crate::icons::{self, Icon};
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
    /// Czy rozwinięte są mniej istotne odczyty ("Ukryte"); po starcie zwinięte.
    show_hidden: bool,
    seen: u64,
    /// Wersja sterownika z zakładki NVIDIA/RADEON/INTEL: (nazwa, krótka
    /// wersja, pełna wersja do podpowiedzi), pokazywana obok limitu mocy.
    pub driver: Option<(String, String, String)>,
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
        row!("Hot Spot", "ADLX", "°C", 1, None, |m| m.hotspot_c),
        row!(
            tr("Temperatura pamięci", "Memory Temperature"),
            "NVML / ADLX",
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

/// Dane jednego wiersza.
struct Line {
    name: String,
    source: String,
    unit: &'static str,
    decimals: usize,
    max: Option<f32>,
    series: Vec<Option<f32>>,
}

impl Line {
    fn fmt(&self, v: f32) -> String {
        format(v, self.decimals, self.unit)
    }

    /// Ikona wiersza: według jednostki, a dla zegarów i obciążeń według nazwy.
    fn icon(&self) -> Icon {
        let name = self.name.as_str();
        match self.unit {
            "°C" => Icon::Thermo,
            "MB" => Icon::Database,
            "W" | "V" => Icon::Power,
            "MB/s" => Icon::Swap,
            "MHz" if name == tr("Zegar pamięci", "Memory Clock") => Icon::Memory,
            "MHz" if name == tr("Zegar wideo", "Video Clock") => Icon::Monitor,
            "MHz" => Icon::Chip,
            _ if name == tr("Obciążenie GPU", "GPU Load") => Icon::Gpu,
            _ if name == tr("Obciążenie magistrali", "Bus Interface Load") => Icon::Bus,
            _ if name == tr("Obciążenie kontrolera pamięci", "Memory Controller Load") => {
                Icon::Memory
            }
            _ => Icon::Gauge,
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
            (None, "MB") => history::Scale::Around(64.0),
            _ => history::Scale::FromZero(1.0),
        }
    }
}

/// Mniej istotne odczyty, schowane pod przyciskiem "Ukryte" (wybór użytkownika).
fn is_hidden(name: &str) -> bool {
    [
        "PCIe TX",
        "PCIe RX",
        tr("Obciążenie kontrolera pamięci", "Memory Controller Load"),
        tr("Pobór mocy karty", "Board Power Draw"),
        tr("Pobór mocy układu GPU", "GPU Chip Power Draw"),
        tr("Pobór mocy (% TDP)", "Power Consumption (% TDP)"),
        tr("Pamięć systemu zajęta", "System Memory Used"),
    ]
    .contains(&name)
}

/// Wysokość wiersza (jak w GPU-Z: nazwa, wartość, wykres na całą długość).
const ROW_H: f32 = 27.0;

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
        ui.add_space(3.0);

        let history = &samples.history;
        let last = history.back();

        // Odczyty bez wykresu: pasek "nazwa: wartość" nad wierszami.
        let mut chips: Vec<(Icon, String, String, String)> = Vec::new();
        if let Some(m) = last {
            if let Some(limit) = &m.details.limit {
                chips.push((
                    Icon::Power,
                    tr("Ograniczenie", "PerfCap").into(),
                    limit.clone(),
                    "NVML".into(),
                ));
            }
            if let Some(p) = m.details.pstate {
                chips.push((
                    Icon::Sensors,
                    tr("Stan wydajności", "Performance State").into(),
                    format!("P{p}"),
                    "NVML".into(),
                ));
            }
            if let (Some(g), Some(w)) = (m.details.pcie_gen, m.details.pcie_width) {
                chips.push((
                    Icon::Chip,
                    tr("Łącze PCIe", "PCIe Link").into(),
                    format!("Gen {g} x{w}"),
                    "NVML".into(),
                ));
            }
            if let Some(total) = m.details.mem_total_mb {
                chips.push((
                    Icon::Memory,
                    tr("Pamięć karty", "Memory Size").into(),
                    format!("{total} MB"),
                    "NVML".into(),
                ));
            }
            if let Some(tdp) = m.details.tdp_w {
                chips.push((
                    Icon::Power,
                    tr("Limit mocy (TDP)", "Power Limit (TDP)").into(),
                    format!("{tdp:.0} W"),
                    "NVML".into(),
                ));
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            for (i, (icon, name, value, source)) in chips.iter().enumerate() {
                if i > 0 {
                    theme::vsep(ui, 16.0);
                }
                icons::show(ui, *icon, 15.0, theme::accent());
                ui.label(
                    RichText::new(format!("{name}: {value}"))
                        .size(12.5)
                        .color(theme::text()),
                )
                .on_hover_text(crate::trf!("Źródło: {source}", "Source: {source}"));
            }
            if let Some((name, short, full)) = &self.driver {
                if !chips.is_empty() {
                    theme::vsep(ui, 16.0);
                }
                icons::show(ui, Icon::Driver, 15.0, theme::accent());
                ui.label(
                    RichText::new(format!("{name}: {short}"))
                        .size(12.5)
                        .color(theme::text()),
                )
                .on_hover_text(full.as_str());
            }
        });
        ui.add_space(5.0);

        // Wiersze z wykresem - tylko czujniki, które karta udostępnia.
        let mut lines: Vec<Line> = rows()
            .into_iter()
            .filter_map(|row| {
                let series: Vec<Option<f32>> = history.iter().map(row.value).collect();
                series.iter().any(Option::is_some).then(|| Line {
                    name: row.name.to_owned(),
                    source: row.source.to_owned(),
                    unit: row.unit,
                    decimals: row.decimals,
                    max: row.max,
                    series,
                })
            })
            .collect();
        let ram: Vec<Option<f32>> = self.sys_mem_mb.iter().copied().collect();
        if ram.iter().any(Option::is_some) {
            lines.push(Line {
                name: tr("Pamięć systemu zajęta", "System Memory Used").to_owned(),
                source: "Windows".into(),
                unit: "MB",
                decimals: 0,
                max: None,
                series: ram,
            });
        }
        let (hidden, shown): (Vec<Line>, Vec<Line>) =
            lines.into_iter().partition(|l| is_hidden(&l.name));

        let show = self.show;
        for line in &shown {
            line_ui(ui, line, show);
        }
        if !hidden.is_empty() {
            ui.add_space(4.0);
            let label = if self.show_hidden {
                crate::trf!("Zwiń ukryte ({})", "Collapse hidden ({})", hidden.len())
            } else {
                crate::trf!("Ukryte ({})", "Hidden ({})", hidden.len())
            };
            let icon = if self.show_hidden {
                crate::icons::Icon::Check
            } else {
                crate::icons::Icon::Download
            };
            if theme::primary_button(ui, Some(icon), &label, true)
                .on_hover_text(tr(
                    "PCIe TX/RX, kontroler pamięci, pobór mocy i pamięć systemu",
                    "PCIe TX/RX, memory controller, power draw and system memory",
                ))
                .clicked()
            {
                self.show_hidden = !self.show_hidden;
            }
            ui.add_space(4.0);
            if self.show_hidden {
                for line in &hidden {
                    line_ui(ui, line, show);
                }
            }
        }
    }
}

/// Wiersz jak w GPU-Z: nazwa, wartość (wg trybu) i wykres 120 s na całą
/// pozostałą szerokość, z podpowiedzią po najechaniu.
fn line_ui(ui: &mut egui::Ui, line: &Line, show: Show) {
    let values: Vec<f32> = line.series.iter().flatten().copied().collect();
    let shown = match show {
        Show::Current => line.series.last().copied().flatten(),
        Show::Min => values.iter().copied().reduce(f32::min),
        Show::Max => values.iter().copied().reduce(f32::max),
        Show::Avg => (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32),
    };
    let current = line.series.last().copied().flatten();
    let color = current.map(|v| line.color(v)).unwrap_or(theme::accent());
    let (min, max, _) = history::stats_points(&line.series).unwrap_or((0.0, 0.0, 0.0));

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
        let width = ui.available_width();
        let name_w = 230.0_f32.min(width * 0.35);
        let value_w = 110.0;

        let cell = |ui: &mut egui::Ui, w: f32| {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(w, ROW_H), egui::Sense::hover());
            ui.painter().rect(
                rect,
                egui::CornerRadius::same(6),
                theme::card_fill(),
                egui::Stroke::new(1.0_f32, theme::card_edge()),
                egui::StrokeKind::Inside,
            );
            (rect, response)
        };
        let (rect, response) = cell(ui, name_w);
        icons::paint(
            ui.painter(),
            egui::Rect::from_center_size(
                rect.left_center() + Vec2::new(19.0, 0.0),
                Vec2::splat(16.0),
            ),
            line.icon(),
            color,
        );
        ui.painter().text(
            rect.left_center() + Vec2::new(38.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &line.name,
            egui::FontId::proportional(13.0),
            theme::text(),
        );
        response.on_hover_text(crate::trf!("Źródło: {}", "Source: {}", line.source));

        let (rect, response) = cell(ui, value_w);
        ui.painter().text(
            rect.right_center() - Vec2::new(10.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            shown.map(|v| line.fmt(v)).unwrap_or_else(|| "—".into()),
            theme::tab_font(13.5),
            color,
        );
        response.on_hover_text(format!("MIN {} / MAX {}", line.fmt(min), line.fmt(max)));

        let chart_w = ui.available_width().max(80.0);
        history::chart(
            ui,
            Vec2::new(chart_w, ROW_H),
            &line.series,
            color,
            line.scale(),
            &|v| line.fmt(v),
        );
    });
    ui.add_space(3.0);
}
