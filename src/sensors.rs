//! Zakładka SENSORS: najważniejsze temperatury, po jednej na podzespół.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, RichText, Sense, Vec2};

use crate::history::{self, History};

use crate::bridge::{Snapshot, Temp};
use crate::disks::Disk;
use crate::theme::{self, muted};

const CPU_PREFS: &[&str] = &["package", "tctl", "tdie", "core max", "core average"];
const GPU_PREFS: &[&str] = &["gpu core", "core"];
const BOARD_PREFS: &[&str] = &["system", "motherboard", "mainboard"];
const DISK_PREFS: &[&str] = &["composite", "temperature"];
const NO_PREFS: &[&str] = &[];

/// Kolejność, tytuł kafelka i preferowane nazwy czujników dla typu podzespołu.
fn kind_info(kind: &str) -> Option<(u8, &'static str, &'static [&'static str])> {
    if kind == "Cpu" {
        Some((0, "Procesor", CPU_PREFS))
    } else if kind.starts_with("Gpu") {
        Some((1, "Karta graficzna", GPU_PREFS))
    } else if kind == "SuperIO" || kind == "Motherboard" {
        Some((2, "Płyta główna", BOARD_PREFS))
    } else if kind == "Storage" {
        Some((3, "Dysk", DISK_PREFS))
    } else if kind == "Memory" {
        Some((4, "Pamięć RAM", NO_PREFS))
    } else {
        None
    }
}

/// Po jednym, najważniejszym czujniku na każdy kluczowy podzespół.
pub fn key_temps<'a>(temps: &'a [Temp]) -> Vec<(&'static str, &'a Temp)> {
    struct Group<'a> {
        rank: u8,
        title: &'static str,
        prefs: &'static [&'static str],
        hw: &'a str,
        list: Vec<&'a Temp>,
    }

    let mut groups: Vec<Group<'a>> = Vec::new();
    for t in temps {
        if t.value.is_none() {
            continue;
        }
        let Some((rank, title, prefs)) = kind_info(&t.kind) else {
            continue;
        };
        match groups.iter_mut().find(|g| g.rank == rank && g.hw == t.hw) {
            Some(g) => g.list.push(t),
            None => groups.push(Group {
                rank,
                title,
                prefs,
                hw: &t.hw,
                list: vec![t],
            }),
        }
    }
    groups.sort_by_key(|g| g.rank);

    groups
        .into_iter()
        .map(|g| {
            let main = g
                .prefs
                .iter()
                .find_map(|p| {
                    g.list
                        .iter()
                        .copied()
                        .find(|t| t.name.to_lowercase().contains(*p))
                })
                .unwrap_or(g.list[0]);
            (g.title, main)
        })
        .collect()
}

/// Odstęp między kafelkami.
const GAP: f32 = 12.0;
/// Margines wewnętrzny kafelka.
const MARGIN: f32 = 14.0;
/// Największa szerokość kafelka razem z ramką.
const MAX_W: f32 = 400.0;

/// Liczba kolumn, przy której `count` kafelków mieści się w obszarze
/// `width` x `height` i kafelki są możliwie duże. Zwraca (kolumny, wysokość kafelka).
pub fn layout(count: usize, width: f32, height: f32) -> (usize, f32) {
    const MIN_W: f32 = 230.0;
    const MIN_H: f32 = 150.0;
    const MAX_H: f32 = 220.0;

    let count = count.max(1);
    let max_cols = (((width + GAP) / (MIN_W + GAP)).floor() as usize).clamp(1, count);
    // Kafelek szerszy niż MAX_W wygląda jak pusty pasek, więc zaczynamy od
    // liczby kolumn, przy której kafelki mają rozsądną szerokość.
    let min_cols = (((width + GAP) / (MAX_W + GAP)).ceil() as usize).clamp(1, max_cols);
    let mut best = (max_cols, MIN_H);
    // Pierwsza liczba kolumn, przy której wszystko mieści się na wysokość,
    // daje największe kafelki.
    for cols in min_cols..=max_cols {
        let rows = count.div_ceil(cols);
        let tile_h = (height - GAP * (rows as f32 - 1.0)) / rows as f32;
        if tile_h >= MIN_H {
            return (cols, tile_h.min(MAX_H));
        }
        best = (cols, MIN_H);
    }
    best
}

/// Klucz kafelka w historii odczytów.
pub fn tile_key(tile: &Tile) -> String {
    format!("{}|{}", tile.title, tile.hw)
}

/// Kafelek: tytuł, nazwa podzespołu i temperatura.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub title: &'static str,
    pub hw: String,
    pub value: f32,
}

/// Tytuł kafelka dysku (klucz tłumaczenia).
const DISK_TITLE: &str = "Dysk";

/// Tytuł kafelka karty graficznej (klucz tłumaczenia).
const GPU_TITLE: &str = "Karta graficzna";

/// Składa listę kafelków. Temperatura karty graficznej odczytana wprost ze
/// sterownika (`gpu`) zastępuje odczyt z procesu czujników, a gdy tamtego
/// nie ma - jest dodawana zaraz za procesorem.
pub fn tiles(temps: &[Temp], gpu: Option<(&str, f32)>, disks: &[Disk]) -> Vec<Tile> {
    let mut out: Vec<Tile> = key_temps(temps)
        .into_iter()
        .map(|(title, t)| Tile {
            title,
            hw: t.hw.clone(),
            value: t.value.unwrap_or(0.0),
        })
        .collect();

    if let Some((name, value)) = gpu {
        let tile = Tile {
            title: GPU_TITLE,
            hw: name.to_owned(),
            value,
        };
        match out.iter().position(|t| t.title == GPU_TITLE) {
            Some(i) => out[i] = tile,
            None => {
                let after_cpu = out.iter().take_while(|t| t.title == "Procesor").count();
                out.insert(after_cpu, tile);
            }
        }
    }

    // Dyski odczytane przez API Windows zastępują te same dyski z procesu
    // czujników; dyski, których Windows nie podaje, zostają.
    if !disks.is_empty() {
        let same = |a: &str, b: &str| {
            let (a, b) = (a.to_lowercase(), b.to_lowercase());
            !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
        };
        out.retain(|t| !(t.title == DISK_TITLE && disks.iter().any(|d| same(&d.name, &t.hw))));
        // Miejsce dysków: za ostatnim kafelkiem, który nie jest pamięcią RAM.
        let at = out
            .iter()
            .position(|t| t.title == "Pamięć RAM")
            .unwrap_or(out.len());
        for (i, d) in disks.iter().enumerate() {
            out.insert(
                at + i,
                Tile {
                    title: DISK_TITLE,
                    hw: d.name.clone(),
                    value: d.temp_c,
                },
            );
        }
    }
    out
}

pub fn ui(
    ui: &mut egui::Ui,
    snap: Option<&Snapshot>,
    gpu: Option<(&str, f32)>,
    disks: &[Disk],
    history: &History,
) {
    let key = tiles(snap.map(|s| s.temps.as_slice()).unwrap_or(&[]), gpu, disks);
    if key.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "Brak danych z czujników.",
                    "No sensor data.",
                ))
                .color(muted())
                .size(16.0),
            );
        });
        return;
    }

    theme::heading(
        ui,
        crate::icons::Icon::Thermo,
        crate::i18n::tr("Najważniejsze temperatury", "Key temperatures"),
    );
    ui.add_space(4.0);

    // Kafelki dzielą między siebie całe wolne miejsce, więc wszystkie są
    // widoczne bez przewijania; pasek przewijania pojawia się dopiero w
    // bardzo małym oknie.
    // Rozmiar liczymy przed ScrollArea: w jej wnętrzu wysokość jest nieograniczona.
    let width = ui.available_width();
    let height = ui.available_height() - 4.0;
    egui::ScrollArea::vertical().show(ui, |ui| {
        let (cols, tile_h) = layout(key.len(), width, height);
        let outer_w = ((width + GAP) / cols as f32 - GAP).min(MAX_W);
        // 2 px na obramowanie i 1 px zapasu na zaokrąglenia.
        let inner = Vec2::new(
            (outer_w - 2.0 * MARGIN - 3.0).max(120.0),
            (tile_h - 2.0 * MARGIN - 3.0).max(100.0),
        );

        ui.spacing_mut().item_spacing = Vec2::new(GAP, GAP);
        for row in key.chunks(cols) {
            ui.horizontal_top(|ui| {
                for tile in row {
                    temp_tile(ui, tile, history.get(&tile_key(tile)), inner);
                }
            });
        }
    });
}

/// Stan temperatury: napis plakietki i kolor.
fn state(value: f32) -> (&'static str, Color32) {
    if value >= 80.0 {
        (crate::i18n::tr("GORĄCO", "HOT"), theme::hot())
    } else if value >= 65.0 {
        (crate::i18n::tr("WYSOKA", "HIGH"), theme::warn())
    } else {
        (crate::i18n::tr("NORMALNA", "NORMAL"), theme::ok())
    }
}

/// Kafelek: tytuł z plakietką stanu, podzespół, temperatura ze zmianą,
/// minimum i maksimum oraz wykres z ostatnich 120 s.
fn temp_tile(ui: &mut egui::Ui, tile: &Tile, series: Option<&VecDeque<f32>>, inner: Vec2) {
    let value = tile.value;
    let title = crate::i18n::tr_dyn(tile.title).to_uppercase();
    let color = theme::temp_color(value);
    let (state_text, state_color) = state(value);
    let (min, max, delta) = match series {
        Some(s) if !s.is_empty() => history::stats(s),
        _ => (value, value, 0.0),
    };
    // Wysoki kafelek dostaje większe cyfry.
    let digits = (inner.y * 0.16).clamp(22.0, 32.0);

    theme::card(12, MARGIN as i8).show(ui, |ui| {
        ui.set_width(inner.x);
        ui.set_height(inner.y);
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 2.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(title)
                        .size(13.0)
                        .strong()
                        .color(theme::text()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    history::badge(ui, state_text, state_color);
                });
            });
            ui.add(egui::Label::new(RichText::new(&tile.hw).size(11.5).color(muted())).truncate());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{value:.1}°C"))
                        .size(digits)
                        .strong()
                        .color(color),
                );
                history::delta(ui, delta, "°C");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("MIN {min:.1}° / MAX {max:.1}°"))
                            .size(11.5)
                            .color(muted()),
                    );
                });
            });
            ui.add_space(4.0);

            // Wykres zajmuje resztę kafelka; pod nim podpisy osi czasu.
            let chart_h = (ui.available_height() - 18.0).max(24.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(inner.x, chart_h), Sense::hover());
            history::chart(ui.painter(), rect, series, color, false, 4.0);
            history::time_axis(ui);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::{layout, tiles, Disk, Temp};

    fn temp(name: &str, hw: &str, kind: &str, value: f32) -> Temp {
        Temp {
            id: format!("{hw}/{name}"),
            name: name.to_owned(),
            hw: hw.to_owned(),
            kind: kind.to_owned(),
            value: Some(value),
        }
    }

    #[test]
    fn driver_gpu_temperature_wins() {
        let temps = vec![
            temp("CPU Package", "Intel Core i5", "Cpu", 55.0),
            temp("GPU Core", "RTX 5070", "GpuNvidia", 60.0),
            temp("Temperature", "SSD", "Storage", 40.0),
        ];
        let out = tiles(&temps, Some(("NVIDIA GeForce RTX 5070", 62.0)), &[]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].hw, "NVIDIA GeForce RTX 5070");
        assert_eq!(out[1].value, 62.0);

        // Bez procesu czujników zostaje sam odczyt ze sterownika karty.
        let out = tiles(&[], Some(("RTX 5070", 48.0)), &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Karta graficzna");

        // Karta bez odczytu w procesie czujników trafia za procesor.
        let out = tiles(
            &[temps[0].clone(), temps[2].clone()],
            Some(("RTX 5070", 48.0)),
            &[],
        );
        assert_eq!(out[1].title, "Karta graficzna");
        assert_eq!(tiles(&temps, None, &[]).len(), 3);
    }

    #[test]
    fn windows_disks_replace_sensor_disks() {
        let temps = vec![
            temp("CPU Package", "Intel Core i5", "Cpu", 55.0),
            temp("Temperature", "Samsung SSD 980 PRO 1TB", "Storage", 40.0),
            temp("Temperature", "WDC WD40EZRZ", "Storage", 33.0),
            temp("DIMM", "Kingston", "Memory", 38.0),
        ];
        let disks = [Disk {
            name: "Samsung SSD 980 PRO 1TB".to_owned(),
            temp_c: 43.0,
        }];
        let out = tiles(&temps, None, &disks);
        // Samsung z Windows, WDC z czujników, pamięć na końcu.
        let names: Vec<(&str, f32)> = out.iter().map(|t| (t.hw.as_str(), t.value)).collect();
        assert_eq!(
            names,
            vec![
                ("Intel Core i5", 55.0),
                ("WDC WD40EZRZ", 33.0),
                ("Samsung SSD 980 PRO 1TB", 43.0),
                ("Kingston", 38.0),
            ]
        );
        // Bez procesu czujników zostają same dyski z Windows.
        assert_eq!(tiles(&[], None, &disks).len(), 1);
    }

    #[test]
    fn tiles_fit_without_scrolling() {
        // Typowe okno 1080 x 600: pięć kafelków.
        assert_eq!(layout(5, 1080.0, 600.0), (3, 220.0));

        // Sześć kafelków w niższym oknie: 3 x 2 bez przewijania.
        let (cols, h) = layout(6, 1080.0, 340.0);
        assert_eq!(cols, 3);
        assert!(2.0 * h + 12.0 <= 340.01);

        // Jeden kafelek nie rozciąga się w nieskończoność.
        assert_eq!(layout(1, 1080.0, 900.0), (1, 220.0));

        // Wąskie okno: jedna kolumna, przewijanie zamiast ściskania.
        assert_eq!(layout(4, 300.0, 200.0), (1, 150.0));
    }
}
