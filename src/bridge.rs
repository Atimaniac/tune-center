//! Czujniki i sterowanie wentylatorami płyty: usługa w procesie programu
//! (`hw::service`, sterownik PawnIO i własny kod) odczytuje stan co sekundę w
//! osobnym wątku, a krzywe są stosowane także wtedy, gdy okno jest
//! zminimalizowane. Typy odczytów (identyfikatory, nazwy, rodzaje
//! podzespołów) są zgodne z dawnym procesem LibreHardwareMonitor, więc zapisane
//! ustawienia wentylatorów działają bez zmian.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::config::{FanConfig, Fans, Mode, MIN_PCT};
use crate::curve;
use crate::hw::service::Service;
use crate::power::{self, GpuSlot, Reading, SharedMeter};

/// Odczyt temperatury.
#[derive(Clone, Debug)]
pub struct Temp {
    pub id: String,
    /// Nazwa czujnika, np. "CPU Package".
    pub name: String,
    /// Nazwa podzespołu, np. "Intel Core i5-13600K".
    pub hw: String,
    /// Typ podzespołu: Cpu, GpuNvidia, SuperIO, Memory, Battery...
    pub kind: String,
    pub value: Option<f32>,
}

impl Temp {
    pub fn label(&self) -> String {
        format!("{} - {}", self.hw, self.name)
    }
}

/// Wentylator, którym można sterować.
#[derive(Clone, Debug)]
pub struct Fan {
    pub id: String,
    pub name: String,
    pub hw: String,
    /// Typ podzespołu, do którego należy wentylator (np. SuperIO, GpuAmd).
    pub kind: String,
    pub rpm: Option<f32>,
    pub percent: Option<f32>,
}

/// Czujnik obrotów (także bez sterowania).
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Speed {
    pub name: String,
    pub hw: String,
    pub rpm: Option<f32>,
    pub percent: Option<f32>,
}

/// Odczyt mocy w watach.
#[derive(Clone, Debug)]
pub struct Power {
    pub name: String,
    pub hw: String,
    pub kind: String,
    pub value: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub temps: Vec<Temp>,
    /// Wentylatory, którymi można sterować.
    pub fans: Vec<Fan>,
    /// Wszystkie czujniki obrotów (na razie nieużywane w interfejsie).
    #[allow(dead_code)]
    pub speeds: Vec<Speed>,
    /// Czujniki mocy (W).
    pub powers: Vec<Power>,
    pub admin: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Status {
    Starting,
    Running,
    Error(String),
}

/// Wysterowanie wynikające z ustawień wentylatora: `Some(procent)` albo
/// `None`, gdy sterować ma płyta główna lub karta (tryb Auto, brak ustawień,
/// krzywa bez odczytu temperatury - wtedy bezpieczniej oddać sterowanie).
pub fn target(cfg: Option<&FanConfig>, temps: &[Temp]) -> Option<f32> {
    let cfg = cfg?;
    match cfg.mode {
        Mode::Auto => None,
        Mode::Manual => Some(cfg.manual),
        Mode::Curve => cfg
            .source
            .as_ref()
            .and_then(|src| temps.iter().find(|t| &t.id == src))
            .and_then(|t| t.value)
            .map(|t| curve::eval(&cfg.points, t)),
    }
}

/// Wartość obrotów, która oznacza brak odczytu (licznik 0xFFFF), a nie
/// prawdziwą prędkość - np. układ NCT6683D na płytach ASRock.
const RPM_INVALID: f32 = 65_000.0;

/// Układy Super I/O, którymi nie sterujemy - tylko odczyt. NCT6683D ma na
/// płytach (np. ASRock) własny firmware producenta; zapisy z systemu są
/// uznawane za ryzykowne (sterownik Linuksa też je blokuje).
pub fn read_only_chip(hw: &str) -> bool {
    hw.to_ascii_uppercase().contains("NCT6683")
}

impl Snapshot {
    /// Odczyty "brak danych" (65535 obr./min) jako brak odczytu i nazwa
    /// próbki inżynierskiej procesora zamieniona na właściwą.
    pub fn sanitize(&mut self) {
        let fix = |rpm: &mut Option<f32>| {
            if rpm.is_some_and(|r| r >= RPM_INVALID) {
                *rpm = None;
            }
        };
        for f in &mut self.fans {
            fix(&mut f.rpm);
        }
        for s in &mut self.speeds {
            fix(&mut s.rpm);
        }
        let cpu = |hw: &mut String| {
            if crate::cpu::is_engineering_name(hw) && !crate::cpu::is_engineering_name(crate::cpu::name()) {
                *hw = crate::cpu::name().to_owned();
            }
        };
        self.temps.iter_mut().for_each(|t| cpu(&mut t.hw));
        self.fans.iter_mut().for_each(|f| cpu(&mut f.hw));
        self.speeds.iter_mut().for_each(|s| cpu(&mut s.hw));
        self.powers.iter_mut().for_each(|p| cpu(&mut p.hw));
    }
}

struct Inner {
    service: Arc<Mutex<Service>>,
    /// Program się zamyka - wątek odczytów kończy pracę.
    quitting: bool,
    snapshot: Option<Snapshot>,
    status: Status,
    /// Konfiguracja aktywnego profilu.
    fans: Fans,
    /// Ostatnio wysłane wysterowanie: Some(p) = wymuszone p %, None = oddane BIOS-owi.
    applied: HashMap<String, Option<f32>>,
    /// Typy podzespołów, których wentylatorami steruje sterownik karty
    /// graficznej (moduł `gpu_fans`), a nie ta usługa.
    skip_kinds: Vec<&'static str>,
}

impl Inner {
    /// Przelicza tryby na wysterowanie i ustawia tylko zmiany.
    fn apply(&mut self) {
        // Program się zamyka: wentylatory zostały już oddane płycie i nie
        // wolno ustawić ich ponownie (proces skończy się bez przywracania).
        if self.quitting {
            return;
        }
        // Awaryjne oddanie wentylatorów płycie (błąd w innym wątku) skasowało
        // nasze ustawienia w układzie - zapominamy je, żeby ustawić od nowa.
        if self.applied.values().any(Option::is_some) && self.service.lock().is_ok_and(|s| !s.touched()) {
            self.applied.clear();
        }
        let Some(snap) = &self.snapshot else { return };
        let mut changes = Vec::new();

        for fan in &snap.fans {
            if self.skip_kinds.contains(&fan.kind.as_str()) || read_only_chip(&fan.hw) {
                continue;
            }
            let target = target(self.fans.get(&fan.id), &snap.temps)
                .map(|p| p.clamp(MIN_PCT, 100.0).round());

            let prev = self.applied.get(&fan.id).copied();
            match (target, prev) {
                (Some(p), Some(Some(q))) if (p - q).abs() < 1.0 => {}
                (Some(p), _) => changes.push((fan.id.clone(), Some(p))),
                (None, Some(Some(_))) => changes.push((fan.id.clone(), None)),
                (None, _) => {}
            }
        }

        if changes.is_empty() {
            return;
        }
        let Ok(mut service) = self.service.lock() else { return };
        for (id, value) in changes {
            if service.set(&id, value).is_ok() {
                self.applied.insert(id, value);
            }
        }
    }
}

/// Jeden odczyt: licznik energii, tablica temperatur dla wentylatorów kart,
/// stan i sterowanie wentylatorami.
fn handle_snapshot(
    inner: &Arc<Mutex<Inner>>,
    mut snap: Snapshot,
    meter: &SharedMeter,
    gpu: &GpuSlot,
    board: &crate::gpu_fans::TempBoard,
) {
    snap.sanitize();
    let mut s = inner.lock().unwrap();
    if let Some(err) = &snap.error {
        s.status = Status::Error(err.clone());
        return;
    }
    s.status = Status::Running;
    let readings: Vec<Reading> = snap
        .powers
        .iter()
        .map(|p| Reading {
            name: &p.name,
            hw: &p.hw,
            kind: &p.kind,
            watts: p.value,
        })
        .collect();
    // Moc karty graficznej: odczyt ze sterownika, jeśli świeży.
    let now = Instant::now();
    let driver = power::fresh(gpu, now);
    let readings = power::with_driver_gpu(readings, driver.as_ref());
    if let Ok(mut meter) = meter.lock() {
        meter.update(&readings, now);
    }
    drop(readings);
    if let Ok(mut b) = board.lock() {
        b.clone_from(&snap.temps);
    }
    s.snapshot = Some(snap);
    s.apply();
}

pub struct Backend {
    inner: Arc<Mutex<Inner>>,
}

impl Backend {
    /// Uruchamia usługę czujników i wątek odczytów co sekundę.
    /// `meter`: licznik energii sesji (liczy także przy schowanym oknie);
    /// `gpu`: ostatni odczyt mocy karty wprost ze sterownika;
    /// `board`: tablica, na którą trafiają temperatury z każdego odczytu;
    /// `skip_kinds`: typy kart, których wentylatorami steruje sterownik karty.
    pub fn spawn(
        ctx: egui::Context,
        fans: Fans,
        meter: SharedMeter,
        gpu: GpuSlot,
        board: crate::gpu_fans::TempBoard,
        skip_kinds: Vec<&'static str>,
    ) -> Self {
        let service = Service::open();
        let inner = Arc::new(Mutex::new(Inner {
            service: service.clone(),
            quitting: false,
            snapshot: None,
            status: Status::Starting,
            fans,
            applied: HashMap::new(),
            skip_kinds,
        }));
        let thread_inner = inner.clone();
        crate::spawn("sensors", move || {
            let run = std::panic::AssertUnwindSafe(|| loop {
                let started = Instant::now();
                if thread_inner.lock().map(|s| s.quitting).unwrap_or(true) {
                    break;
                }
                let snap = match service.lock() {
                    Ok(mut s) => s.read(),
                    Err(_) => break,
                };
                handle_snapshot(&thread_inner, snap, &meter, &gpu, &board);
                ctx.request_repaint();
                std::thread::sleep(Duration::from_secs(1).saturating_sub(started.elapsed()));
            });
            // Błąd w odczycie albo sterowaniu: wentylatory wracają do płyty
            // (także przez "zatrutą" blokadę), a interfejs pokazuje błąd
            // zamiast zamrożonych odczytów.
            if std::panic::catch_unwind(run).is_err() {
                service.lock().unwrap_or_else(|e| e.into_inner()).restore_all();
                let mut s = thread_inner.lock().unwrap_or_else(|e| e.into_inner());
                s.applied.clear();
                s.status = Status::Error(
                    crate::i18n::tr(
                        "Czujniki zatrzymane po błędzie - wentylatory oddane płycie głównej. Uruchom program ponownie.",
                        "Sensors stopped after an error - fans handed back to the motherboard. Restart the program.",
                    )
                    .to_owned(),
                );
                drop(s);
                ctx.request_repaint();
            }
        });
        Self { inner }
    }

    /// Uwagi usługi czujników (brak modułu, nierozpoznany układ) - diagnostyka.
    pub fn notes(&self) -> Vec<String> {
        let Ok(s) = self.inner.lock() else { return Vec::new() };
        let notes = s.service.lock().map(|x| x.notes.clone()).unwrap_or_default();
        notes
    }

    pub fn state(&self) -> (Option<Snapshot>, Status) {
        let s = self.inner.lock().unwrap();
        (s.snapshot.clone(), s.status.clone())
    }

    /// Podmienia konfigurację (zmiana ustawień lub profilu) i od razu ją stosuje.
    pub fn set_fans(&self, fans: Fans) {
        let mut s = self.inner.lock().unwrap();
        s.fans = fans;
        s.apply();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Oddajemy płycie wszystkie wentylatory i zatrzymujemy wątek odczytów.
        // Blokada "zatruta" po panice wątku też jest otwierana - wentylatory
        // muszą wrócić do płyty niezależnie od tego, co zawiodło wcześniej.
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.quitting = true;
        let service = s.service.clone();
        // Bez trzymania `inner`: wątek odczytów bierze blokady w kolejności
        // inner -> service i mógłby się z nami zakleszczyć.
        drop(s);
        service.lock().unwrap_or_else(|e| e.into_inner()).restore_all();
    }
}

#[cfg(test)]
mod invalid_rpm_tests {
    use super::*;

    fn fan(id: &str, hw: &str, rpm: f32) -> Fan {
        Fan { id: id.into(), name: "Fan".into(), hw: hw.into(), kind: "SuperIO".into(), rpm: Some(rpm), percent: Some(40.0) }
    }

    #[test]
    fn rpm_sentinel_and_read_only_chip() {
        let mut s = Snapshot {
            fans: vec![fan("a", "Nuvoton NCT6683D", 65535.0), fan("b", "Nuvoton NCT6796D-R", 1200.0)],
            speeds: vec![Speed { name: "x".into(), hw: "h".into(), rpm: Some(65535.0), percent: None }],
            ..Default::default()
        };
        s.sanitize();
        assert_eq!(s.fans[0].rpm, None);
        assert_eq!(s.fans[1].rpm, Some(1200.0));
        assert_eq!(s.speeds[0].rpm, None);
        assert!(read_only_chip("Nuvoton NCT6683D"));
        assert!(!read_only_chip("Nuvoton NCT6796D-R"));
        assert!(!read_only_chip("Nuvoton NCT6701D"));
    }
}
