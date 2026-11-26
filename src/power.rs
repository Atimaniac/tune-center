//! Pobór mocy procesora i kart graficznych oraz licznik energii sesji.
//! Moduł nie zależy od interfejsu ani od procesu czujników - tylko liczy.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Jeden odczyt mocy z czujnika.
pub struct Reading<'a> {
    pub name: &'a str,
    pub hw: &'a str,
    /// Typ podzespołu wg LibreHardwareMonitor: Cpu, GpuNvidia, GpuAmd, GpuIntel...
    pub kind: &'a str,
    pub watts: Option<f32>,
}

/// Pobór mocy karty graficznej odczytany wprost ze sterownika karty
/// (NVML dla Nvidii, ADL dla AMD).
#[derive(Clone, Debug, PartialEq)]
pub struct GpuPower {
    pub name: String,
    /// Typ w konwencji czujników: "GpuNvidia" albo "GpuAmd".
    pub kind: &'static str,
    pub watts: f32,
}

/// Odczyt ze sterownika starszy niż tyle jest pomijany (okno schowane w
/// zasobniku nie odświeża odczytów) - wtedy obowiązują czujniki.
pub const DRIVER_MAX_AGE: Duration = Duration::from_secs(3);

/// Miejsce, w którym interfejs zostawia ostatni odczyt mocy karty dla
/// wątku liczącego energię.
pub type GpuSlot = std::sync::Arc<std::sync::Mutex<Option<(Instant, GpuPower)>>>;

/// Licznik energii współdzielony między wątkiem czujników a interfejsem.
pub type SharedMeter = std::sync::Arc<std::sync::Mutex<Meter>>;

/// Świeży odczyt ze sterownika karty albo `None`.
pub fn fresh(slot: &GpuSlot, now: Instant) -> Option<GpuPower> {
    let guard = slot.lock().ok()?;
    let (at, power) = guard.as_ref()?;
    (now.saturating_duration_since(*at) <= DRIVER_MAX_AGE).then(|| power.clone())
}

fn same_card(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
}

/// Zastępuje odczyty mocy karty graficznej z czujników odczytem wprost ze
/// sterownika. Zastępowana jest karta o pasującej nazwie, a gdy czujniki
/// znają tylko jedną kartę - właśnie ona. Wiersz zachowuje nazwę z czujników,
/// żeby licznik energii nie zaczynał od zera przy zmianie źródła.
pub fn with_driver_gpu<'a>(
    mut readings: Vec<Reading<'a>>,
    gpu: Option<&'a GpuPower>,
) -> Vec<Reading<'a>> {
    let Some(gpu) = gpu else { return readings };

    let mut cards: Vec<&'a str> = Vec::new();
    for r in &readings {
        if is_gpu(r.kind) && r.watts.is_some() && !cards.contains(&r.hw) {
            cards.push(r.hw);
        }
    }
    let replaced = cards
        .iter()
        .copied()
        .find(|hw| same_card(hw, &gpu.name))
        .or_else(|| (cards.len() == 1).then(|| cards[0]));

    if let Some(hw) = replaced {
        readings.retain(|r| !(is_gpu(r.kind) && r.hw == hw));
    }
    readings.push(Reading {
        name: "GPU Power",
        hw: replaced.unwrap_or(&gpu.name),
        kind: gpu.kind,
        watts: Some(gpu.watts),
    });
    readings
}

/// Wiersz zakładki Power: jeden podzespół.
#[derive(Clone, Debug)]
pub struct Row {
    /// "Procesor", "Karta graficzna", "Zasilacz" albo "Bateria".
    pub title: &'static str,
    pub hw: String,
    /// Pomiar całego komputera (zasilacz, bateria) - poza sumą podzespołów.
    pub system: bool,
    /// Aktualny pobór mocy w watach.
    pub watts: f32,
    /// Energia zużyta od początku sesji w watogodzinach.
    pub wh: f64,
    /// Średni pobór mocy w sesji.
    pub avg_w: f32,
    /// Największy pobór mocy w sesji.
    pub max_w: f32,
}

/// Suma wszystkich podzespołów.
#[derive(Clone, Debug, Default)]
pub struct Total {
    pub watts: f32,
    pub wh: f64,
    pub avg_w: f32,
    /// Największa chwilowa suma (nie suma osobnych maksimów).
    pub max_w: f32,
}

/// Dane licznika jednego podzespołu.
#[derive(Default)]
struct Stats {
    wh: f64,
    max_w: f32,
}

/// Nazwy czujników oznaczających moc całego podzespołu, od najlepszej.
const PREFS: &[&str] = &["package", "total", "gpu power", "power"];

/// Dłuższa przerwa między odczytami (uśpienie komputera, zawieszenie)
/// nie jest doliczana do licznika.
const MAX_STEP: Duration = Duration::from_secs(5);

fn is_cpu(kind: &str) -> bool {
    kind == "Cpu"
}

fn is_gpu(kind: &str) -> bool {
    kind.starts_with("Gpu")
}

/// Zasilacz z USB (Corsair HXi/RMi, MSI Ai): moc całego komputera.
fn is_psu(kind: &str) -> bool {
    kind == "Psu"
}

/// Bateria laptopa: tempo rozładowania to moc całego komputera.
fn is_battery(kind: &str) -> bool {
    kind == "Battery"
}

/// Po jednym czujniku mocy na procesor, każdą kartę graficzną, zasilacz z
/// USB i baterię (tylko gdy się rozładowuje).
pub fn main_rows(readings: &[Reading]) -> Vec<Row> {
    let groups: [(&'static str, fn(&str) -> bool); 4] = [
        ("Procesor", is_cpu),
        ("Karta graficzna", is_gpu),
        ("Zasilacz", is_psu),
        ("Bateria", is_battery),
    ];

    let mut rows = Vec::new();
    for (title, matches) in groups {
        let mut hardware: Vec<&str> = Vec::new();
        for r in readings {
            if matches(r.kind) && r.watts.is_some() && !hardware.contains(&r.hw) {
                hardware.push(r.hw);
            }
        }
        for hw in hardware {
            let list: Vec<&Reading> = readings
                .iter()
                .filter(|r| r.hw == hw && matches(r.kind) && r.watts.is_some())
                .collect();
            let preferred = PREFS.iter().find_map(|pref| {
                list.iter()
                    .copied()
                    .find(|r| r.name.to_lowercase().contains(*pref))
            });
            // Bez czujnika "całości" bierzemy największy odczyt. Bateria: tylko
            // tempo rozładowania (przy ładowaniu nie mierzy poboru komputera).
            let pick = if title == "Bateria" {
                list.iter()
                    .copied()
                    .find(|r| r.name.trim().eq_ignore_ascii_case("discharge rate") && r.watts.unwrap_or(0.0) > 0.5)
            } else {
                preferred.or_else(|| {
                    list.iter()
                        .copied()
                        .max_by(|a, b| a.watts.unwrap_or(0.0).total_cmp(&b.watts.unwrap_or(0.0)))
                })
            };
            if let Some(r) = pick {
                rows.push(Row {
                    title,
                    hw: hw.to_string(),
                    system: title == "Zasilacz" || title == "Bateria",
                    watts: r.watts.unwrap_or(0.0).max(0.0),
                    wh: 0.0,
                    avg_w: 0.0,
                    max_w: 0.0,
                });
            }
        }
    }
    rows
}

/// Licznik energii: całkuje moc po czasie między kolejnymi odczytami
/// oraz pamięta średnią i maksimum.
pub struct Meter {
    stats: HashMap<String, Stats>,
    /// Łączny czas, z którego policzono energię (bez pominiętych przerw).
    measured_secs: f64,
    total_max_w: f32,
    last: Option<Instant>,
    started: Instant,
    rows: Vec<Row>,
}

impl Meter {
    pub fn new(now: Instant) -> Self {
        Self {
            stats: HashMap::new(),
            measured_secs: 0.0,
            total_max_w: 0.0,
            last: None,
            started: now,
            rows: Vec::new(),
        }
    }

    pub fn update(&mut self, readings: &[Reading], now: Instant) {
        let step = match self.last {
            Some(last) => now.saturating_duration_since(last).min(MAX_STEP),
            None => Duration::ZERO,
        };
        self.last = Some(now);
        self.measured_secs += step.as_secs_f64();

        let mut rows = main_rows(readings);
        for row in &mut rows {
            let stats = self
                .stats
                .entry(format!("{}|{}", row.title, row.hw))
                .or_default();
            stats.wh += row.watts as f64 * step.as_secs_f64() / 3600.0;
            stats.max_w = stats.max_w.max(row.watts);
            row.wh = stats.wh;
            row.max_w = stats.max_w;
            row.avg_w = average(stats.wh, self.measured_secs, row.watts);
        }
        if rows.iter().any(|r| !r.system) {
            let sum: f32 = rows.iter().filter(|r| !r.system).map(|r| r.watts).sum();
            self.total_max_w = self.total_max_w.max(sum);
        }
        self.rows = rows;
    }

    pub fn rows(&self) -> Vec<Row> {
        self.rows.clone()
    }

    pub fn total(&self) -> Total {
        // Suma podzespołów: zasilacz i bateria mierzą cały komputer (już z nimi).
        let watts: f32 = self.rows.iter().filter(|r| !r.system).map(|r| r.watts).sum();
        let wh: f64 = self.rows.iter().filter(|r| !r.system).map(|r| r.wh).sum();
        Total {
            watts,
            wh,
            avg_w: average(wh, self.measured_secs, watts),
            max_w: self.total_max_w,
        }
    }

    /// Czas od początku sesji albo od ostatniego wyzerowania.
    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    /// Zeruje energię, średnie i maksima.
    pub fn reset(&mut self, now: Instant) {
        self.stats.clear();
        self.measured_secs = 0.0;
        self.total_max_w = self.rows.iter().map(|r| r.watts).sum();
        self.started = now;
        self.last = Some(now);
        for row in &mut self.rows {
            row.wh = 0.0;
            row.avg_w = row.watts;
            row.max_w = row.watts;
        }
    }
}

/// Średnia moc z energii i czasu; zanim upłynie jakikolwiek czas - bieżąca.
fn average(wh: f64, secs: f64, current: f32) -> f32 {
    if secs > 0.0 {
        (wh * 3600.0 / secs) as f32
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_reading_replaces_sensor_reading() {
        let gpu = GpuPower {
            name: "NVIDIA GeForce RTX 5070".to_owned(),
            kind: "GpuNvidia",
            watts: 180.0,
        };
        // Jedna karta w czujnikach: zastąpiona, nazwa z czujników zostaje.
        let rows = main_rows(&with_driver_gpu(
            vec![
                r("CPU Package", "i5", "Cpu", 60.0),
                r("GPU Package", "RTX 5070", "GpuNvidia", 150.0),
            ],
            Some(&gpu),
        ));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].hw, "RTX 5070");
        assert_eq!(rows[1].watts, 180.0);

        // Brak czujników: sama karta ze sterownika.
        let rows = main_rows(&with_driver_gpu(Vec::new(), Some(&gpu)));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hw, "NVIDIA GeForce RTX 5070");

        // Dwie inne karty w czujnikach: nic nie jest zastępowane.
        let rows = main_rows(&with_driver_gpu(
            vec![
                r("GPU Power", "Radeon RX 6800", "GpuAmd", 90.0),
                r("GPU Power", "Intel Arc", "GpuIntel", 30.0),
            ],
            Some(&gpu),
        ));
        assert_eq!(rows.len(), 3);

        // Bez odczytu ze sterownika lista zostaje bez zmian.
        assert_eq!(
            main_rows(&with_driver_gpu(
                vec![r("GPU Package", "RTX", "GpuNvidia", 150.0)],
                None
            ))
            .len(),
            1
        );
    }

    #[test]
    fn stale_driver_reading_is_ignored() {
        let now = Instant::now();
        let gpu = GpuPower {
            name: "x".to_owned(),
            kind: "GpuAmd",
            watts: 1.0,
        };
        let slot: GpuSlot = Default::default();
        assert_eq!(fresh(&slot, now), None);
        *slot.lock().unwrap() = Some((now, gpu.clone()));
        assert_eq!(fresh(&slot, now + Duration::from_secs(1)), Some(gpu));
        assert_eq!(fresh(&slot, now + Duration::from_secs(10)), None);
    }

    #[test]
    fn psu_and_battery_are_whole_computer_outside_the_total() {
        let readings = vec![
            r("CPU Package", "i7", "Cpu", 60.0),
            r("GPU Package", "RTX", "GpuNvidia", 150.0),
            r("+12V", "Corsair HX1000i", "Psu", 200.0),
            r("Total watts", "Corsair HX1000i", "Psu", 260.0),
            r("Discharge Rate", "Battery", "Battery", 35.0),
            r("Charge Rate", "Battery", "Battery", 0.0),
        ];
        let rows = main_rows(&readings);
        let titles: Vec<(&str, f32, bool)> = rows.iter().map(|x| (x.title, x.watts, x.system)).collect();
        assert_eq!(
            titles,
            vec![("Procesor", 60.0, false), ("Karta graficzna", 150.0, false), ("Zasilacz", 260.0, true), ("Bateria", 35.0, true)]
        );
        let start = Instant::now();
        let mut m = Meter::new(start);
        m.update(&readings, start);
        assert_eq!(m.total().watts, 210.0);
        // Ładowanie: bez poboru komputera, bez wiersza baterii.
        let charging = vec![r("Discharge Rate", "Battery", "Battery", 0.0), r("Charge Rate", "Battery", "Battery", 45.0)];
        assert!(main_rows(&charging).is_empty());
    }

    fn r<'a>(name: &'a str, hw: &'a str, kind: &'a str, w: f32) -> Reading<'a> {
        Reading {
            name,
            hw,
            kind,
            watts: Some(w),
        }
    }

    #[test]
    fn picks_package_and_orders_cpu_first() {
        let readings = [
            r("GPU Package", "RTX 4070", "GpuNvidia", 150.0),
            r("CPU Cores", "i5-13600K", "Cpu", 40.0),
            r("CPU Package", "i5-13600K", "Cpu", 55.0),
            r("Fan", "Board", "SuperIO", 3.0),
        ];
        let rows = main_rows(&readings);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].title, rows[0].watts), ("Procesor", 55.0));
        assert_eq!((rows[1].title, rows[1].watts), ("Karta graficzna", 150.0));
    }

    #[test]
    fn falls_back_to_largest_reading() {
        let readings = [
            r("GPU Core", "RX 7800", "GpuAmd", 120.0),
            r("GPU SoC", "RX 7800", "GpuAmd", 30.0),
        ];
        assert_eq!(main_rows(&readings)[0].watts, 120.0);
    }

    #[test]
    fn integrates_energy_and_ignores_long_gaps() {
        let t0 = Instant::now();
        let readings = [r("CPU Package", "cpu", "Cpu", 3600.0)];
        let mut m = Meter::new(t0);
        m.update(&readings, t0);
        assert_eq!(m.rows()[0].wh, 0.0);
        // 3600 W przez 1 s = 1 Wh
        m.update(&readings, t0 + Duration::from_secs(1));
        assert!((m.rows()[0].wh - 1.0).abs() < 1e-9);
        // przerwa 1 h liczy się tylko jako 5 s
        m.update(&readings, t0 + Duration::from_secs(3601));
        assert!((m.rows()[0].wh - 6.0).abs() < 1e-9);
        m.reset(t0 + Duration::from_secs(3601));
        assert_eq!(m.rows()[0].wh, 0.0);
        m.update(&readings, t0 + Duration::from_secs(3602));
        assert!((m.rows()[0].wh - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tracks_average_and_maximum() {
        let t0 = Instant::now();
        let mut m = Meter::new(t0);
        let at = |cpu: f32, gpu: f32| {
            [
                Reading {
                    name: "CPU Package",
                    hw: "cpu",
                    kind: "Cpu",
                    watts: Some(cpu),
                },
                Reading {
                    name: "GPU Package",
                    hw: "gpu",
                    kind: "GpuNvidia",
                    watts: Some(gpu),
                },
            ]
        };
        m.update(&at(100.0, 50.0), t0);
        // przed upływem czasu średnia = bieżąca
        assert_eq!(m.rows()[0].avg_w, 100.0);
        m.update(&at(100.0, 200.0), t0 + Duration::from_secs(1));
        m.update(&at(20.0, 60.0), t0 + Duration::from_secs(2));
        let rows = m.rows();
        // każdy krok liczy się z mocą zmierzoną na jego końcu
        assert!((rows[0].avg_w - 60.0).abs() < 1e-3);
        assert_eq!(rows[0].max_w, 100.0);
        assert!((rows[1].avg_w - 130.0).abs() < 1e-3);
        assert_eq!(rows[1].max_w, 200.0);
        let total = m.total();
        assert_eq!(total.watts, 80.0);
        assert!((total.avg_w - 190.0).abs() < 1e-3);
        // maksimum sumy to największa chwilowa suma: 100 + 200
        assert_eq!(total.max_w, 300.0);
        m.reset(t0 + Duration::from_secs(2));
        assert_eq!(m.total().max_w, 80.0);
        assert_eq!(m.rows()[0].max_w, 20.0);
    }
}
