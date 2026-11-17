//! Widok czujników karty wzorowany na zakładce "Sensors" programu GPU-Z:
//! wiersz = nazwa, wartość i wykres ostatnich dwóch minut.
//!
//! Źródła: NVML (oficjalne API monitorowania Nvidii), NVAPI (temperatury,
//! zegary, obciążenie silników, napięcie, topologia mocy), Windows (pamięć
//! systemu) i proces czujników (temperatura procesora).

use std::collections::VecDeque;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use super::backend::Monitor;
use super::sampler::{Samples, HISTORY};
use crate::bridge::Snapshot;
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
    cpu_temp: VecDeque<Option<f32>>,
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
            tr("Wentylator (%)", "Fan Speed (%)"),
            "NVAPI / NVML",
            "%",
            0,
            pct,
            |m| m.fan_pct.map(|v| v as f32)
        ),
        row!(
            tr("Wentylator (RPM)", "Fan Speed (RPM)"),
            "NVAPI",
            "RPM",
            0,
            None,
            |m| m.fan_rpm.map(|v| v as f32)
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

fn cpu_temp(snap: Option<&Snapshot>) -> Option<f32> {
    let temps = &snap?.temps;
    crate::sensors::key_temps(temps)
        .into_iter()
        .find(|(title, _)| *title == "Procesor")
        .and_then(|(_, t)| t.value)
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

impl SensorsView {
    pub fn ui(&mut self, ui: &mut egui::Ui, samples: &Samples, snap: Option<&Snapshot>) {
        if samples.seq != self.seen {
            self.seen = samples.seq;
            push(&mut self.cpu_temp, cpu_temp(snap));
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
                if ui.selectable_label(self.show == mode, text).clicked() {
                    self.show = mode;
                }
            }
            ui.label(
                RichText::new(crate::trf!(
                    "(ostatnie {} s, odczyt co 1 s)",
                    "(last {} s, read every 1 s)",
                    HISTORY
                ))
                .small()
                .color(theme::muted()),
            );
        });
        ui.add_space(6.0);

        let history = &samples.history;
        for row in rows() {
            let series: Vec<Option<f32>> = history.iter().map(row.value).collect();
            // Wiersze, których karta nie udostępnia (np. hotspot w RTX 50), pomijamy.
            if series.iter().all(Option::is_none) {
                // Hotspot RTX 50: wiersz zostaje, z wyjaśnieniem braku wartości.
                let hidden = history.back().is_some_and(|m| m.details.hotspot_hidden);
                if row.name == "Hot Spot" && hidden {
                    text_row(
                        ui,
                        row.name,
                        tr(
                            "NVAPI / NVML. Sterownik kart RTX 50 nie udostępnia hotspotu w swoim API; GPU-Z odczytuje go własnym sterownikiem jądra.",
                            "NVAPI / NVML. The RTX 50 driver does not expose the hotspot through its API; GPU-Z reads it with its own kernel driver.",
                        ),
                        tr("niedostępny na RTX 50 (najedź na nazwę)", "unavailable on RTX 50 (hover the name)"),
                    );
                }
                continue;
            }
            let fmt = |v: f32| format(v, row.decimals, row.unit);
            self.row(ui, row.name, row.source, &series, row.max, &fmt);
        }
        // Przyczyna ograniczenia i stan wydajności - tekst, bez wykresu.
        if let Some(m) = history.back() {
            if let Some(limit) = &m.details.limit {
                text_row(
                    ui,
                    tr("Przyczyna ograniczenia", "PerfCap Reason"),
                    "NVML",
                    limit,
                );
            }
            if let Some(p) = m.details.pstate {
                text_row(
                    ui,
                    tr("Stan wydajności", "Performance State"),
                    "NVML",
                    &format!("P{p}"),
                );
            }
            if let (Some(g), Some(w)) = (m.details.pcie_gen, m.details.pcie_width) {
                text_row(
                    ui,
                    tr("Łącze PCIe (bieżące)", "PCIe Link (current)"),
                    "NVML",
                    &format!("Gen {g} x{w}"),
                );
            }
            if let Some(total) = m.details.mem_total_mb {
                text_row(
                    ui,
                    tr("Pamięć karty", "Memory Size"),
                    "NVML",
                    &format!("{total} MB"),
                );
            }
            if let Some(tdp) = m.details.tdp_w {
                text_row(
                    ui,
                    tr("Domyślny limit mocy (TDP)", "Default Power Limit (TDP)"),
                    "NVML",
                    &format!("{tdp:.0} W"),
                );
            }
        }
        let cpu: Vec<Option<f32>> = self.cpu_temp.iter().copied().collect();
        if cpu.iter().any(Option::is_some) {
            self.row(
                ui,
                tr("Temperatura CPU", "CPU Temperature"),
                "LibreHardwareMonitor",
                &cpu,
                None,
                &|v| format!("{v:.1} °C"),
            );
        } else {
            text_row(
                ui,
                tr("Temperatura CPU", "CPU Temperature"),
                "LibreHardwareMonitor",
                match snap {
                    None => tr(
                        "niedostępna (proces czujników nie działa)",
                        "unavailable (the sensor process is not running)",
                    ),
                    Some(s) if !s.admin => tr(
                        "niedostępna (wymaga administratora)",
                        "unavailable (requires administrator)",
                    ),
                    Some(_) => tr("niedostępna", "unavailable"),
                },
            );
        }
        let ram: Vec<Option<f32>> = self.sys_mem_mb.iter().copied().collect();
        if ram.iter().any(Option::is_some) {
            self.row(
                ui,
                tr("Pamięć systemu zajęta", "System Memory Used"),
                "Windows",
                &ram,
                None,
                &|v| format!("{v:.0} MB"),
            );
        }
    }

    fn row(
        &self,
        ui: &mut egui::Ui,
        name: &str,
        source: &str,
        series: &[Option<f32>],
        max: Option<f32>,
        fmt: &dyn Fn(f32) -> String,
    ) {
        let values: Vec<f32> = series.iter().flatten().copied().collect();
        let shown = match self.show {
            Show::Current => series.last().copied().flatten(),
            Show::Min => values.iter().copied().reduce(f32::min),
            Show::Max => values.iter().copied().reduce(f32::max),
            Show::Avg => {
                (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
            }
        };
        let text = shown.map(fmt).unwrap_or_else(|| "—".into());
        let (label_rect, value_rect, graph_rect) = row_rects(ui);
        let p = ui.painter();
        cell(p, label_rect);
        cell(p, value_rect);
        cell(p, graph_rect);
        p.text(
            label_rect.left_center() + Vec2::new(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::proportional(13.0),
            theme::text(),
        );
        p.text(
            value_rect.right_center() - Vec2::new(6.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            text,
            egui::FontId::monospace(12.5),
            theme::text(),
        );
        // Wykres: słupki od prawej (najnowszy odczyt przy prawej krawędzi).
        let top = max
            .unwrap_or_else(|| values.iter().copied().fold(0.0, f32::max))
            .max(1e-3);
        let inner = graph_rect.shrink(2.0);
        let step = inner.width() / HISTORY as f32;
        let n = series.len();
        let color = theme::accent();
        for (i, v) in series.iter().enumerate() {
            let Some(v) = v else { continue };
            let x = inner.right() - (n - i) as f32 * step;
            let h = (v / top).clamp(0.0, 1.0) * inner.height();
            let bar = Rect::from_min_max(
                Pos2::new(x, inner.bottom() - h.max(1.0)),
                Pos2::new(x + step.max(1.0), inner.bottom()),
            );
            p.rect_filled(bar, 0.0, color);
        }
        // Wartość w miejscu wskazanym kursorem: pionowa linia i dymek.
        let graph = ui.interact(
            graph_rect,
            ui.scope_id().with((name, "graph")),
            Sense::hover(),
        );
        if let Some(pos) = graph.hover_pos() {
            let back = ((inner.right() - pos.x) / step).floor() as usize;
            if back < n {
                let i = n - 1 - back;
                let x = inner.right() - (back as f32 + 0.5) * step;
                p.line_segment(
                    [Pos2::new(x, inner.top()), Pos2::new(x, inner.bottom())],
                    Stroke::new(1.0, theme::text()),
                );
                let text = match series[i] {
                    Some(v) => fmt(v),
                    None => "—".into(),
                };
                let ago = crate::trf!("{} s temu", "{} s ago", back);
                graph.on_hover_ui_at_pointer(|ui| {
                    ui.label(RichText::new(text).strong());
                    ui.label(RichText::new(ago).small().color(theme::muted()));
                });
            }
        }
        ui.interact(label_rect, ui.scope_id().with(name), Sense::hover())
            .on_hover_text(crate::trf!("Źródło: {source}", "Source: {source}"));
    }
}

/// Prostokąty kolumn jednego wiersza (nazwa, wartość, wykres).
fn row_rects(ui: &mut egui::Ui) -> (Rect, Rect, Rect) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    let label_w = 230.0f32.min(width * 0.4);
    let value_w = 120.0;
    let gap = 4.0;
    let label = Rect::from_min_size(rect.min, Vec2::new(label_w, rect.height() - 3.0));
    let value = Rect::from_min_size(
        Pos2::new(label.right() + gap, rect.top()),
        Vec2::new(value_w, rect.height() - 3.0),
    );
    let graph = Rect::from_min_max(
        Pos2::new(value.right() + gap, rect.top()),
        Pos2::new(rect.right(), rect.bottom() - 3.0),
    );
    (label, value, graph)
}

fn cell(p: &egui::Painter, r: Rect) {
    p.rect(
        r,
        2.0,
        theme::well(),
        Stroke::new(1.0, Color32::from_gray(90).gamma_multiply(0.5)),
        egui::StrokeKind::Inside,
    );
}

fn text_row(ui: &mut egui::Ui, name: &str, source: &str, value: &str) {
    let (label_rect, value_rect, graph_rect) = row_rects(ui);
    let full = Rect::from_min_max(value_rect.min, graph_rect.max);
    let p = ui.painter();
    cell(p, label_rect);
    cell(p, full);
    p.text(
        label_rect.left_center() + Vec2::new(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        theme::text(),
    );
    p.text(
        full.left_center() + Vec2::new(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        value,
        egui::FontId::monospace(12.5),
        theme::text(),
    );
    ui.interact(label_rect, ui.scope_id().with(name), Sense::hover())
        .on_hover_text(crate::trf!("Źródło: {source}", "Source: {source}"));
}
