//! Komunikacja z procesem pomocniczym `sensor-bridge` (C# + LibreHardwareMonitor)
//! oraz sterowanie wentylatorami.
//!
//! Bridge co sekundę wypisuje jedną linię JSON ze stanem czujników, a polecenia
//! przyjmuje na stdin: `set <id> <procent>`, `auto <id>`, `quit`.
//! Sterowanie działa w wątku czytającym, niezależnie od interfejsu, więc krzywe
//! są stosowane także wtedy, gdy okno jest zminimalizowane.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use serde::Deserialize;

use crate::config::{FanConfig, Fans, Mode, MIN_PCT};
use crate::curve;
use crate::power::{self, GpuSlot, Reading, SharedMeter};

const BRIDGE: &str = if cfg!(windows) {
    "sensor-bridge.exe"
} else {
    "sensor-bridge"
};

#[derive(Deserialize, Clone, Debug)]
pub struct Temp {
    pub id: String,
    /// Nazwa czujnika, np. "CPU Package".
    pub name: String,
    /// Nazwa podzespołu, np. "Intel Core i5-13600K".
    #[serde(default)]
    pub hw: String,
    /// Typ podzespołu wg LibreHardwareMonitor: Cpu, GpuNvidia, SuperIO, Storage...
    #[serde(default)]
    pub kind: String,
    pub value: Option<f32>,
}

impl Temp {
    pub fn label(&self) -> String {
        format!("{} - {}", self.hw, self.name)
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Fan {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub hw: String,
    /// Typ podzespołu, do którego należy wentylator (np. SuperIO, GpuAmd).
    #[serde(default)]
    pub kind: String,
    pub rpm: Option<f32>,
    pub percent: Option<f32>,
}

/// Odczyt obrotów dowolnego wentylatora (także takiego, którym nie da się sterować).
#[allow(dead_code)]
#[derive(Deserialize, Clone, Debug)]
pub struct Speed {
    pub name: String,
    #[serde(default)]
    pub hw: String,
    pub rpm: Option<f32>,
    pub percent: Option<f32>,
}

/// Odczyt mocy w watach.
#[derive(Deserialize, Clone, Debug)]
pub struct Power {
    pub name: String,
    #[serde(default)]
    pub hw: String,
    #[serde(default)]
    pub kind: String,
    pub value: Option<f32>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub temps: Vec<Temp>,
    /// Wentylatory, którymi można sterować.
    #[serde(default)]
    pub fans: Vec<Fan>,
    /// Wszystkie czujniki obrotów (część protokołu sensor-bridge, na razie nieużywane).
    #[serde(default)]
    #[allow(dead_code)]
    pub speeds: Vec<Speed>,
    /// Czujniki mocy (W).
    #[serde(default)]
    pub powers: Vec<Power>,
    #[serde(default = "yes")]
    pub admin: bool,
    #[serde(default)]
    pub error: Option<String>,
}

fn yes() -> bool {
    true
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

struct Inner {
    stdin: ChildStdin,
    snapshot: Option<Snapshot>,
    status: Status,
    /// Konfiguracja aktywnego profilu.
    fans: Fans,
    /// Ostatnio wysłane wysterowanie: Some(p) = wymuszone p %, None = oddane BIOS-owi.
    applied: HashMap<String, Option<f32>>,
    /// Typy podzespołów, których wentylatorami steruje sterownik karty
    /// graficznej (moduł `gpu_fans`), a nie ten proces.
    skip_kinds: Vec<&'static str>,
}

impl Inner {
    fn send(&mut self, line: &str) {
        let _ = writeln!(self.stdin, "{line}");
        let _ = self.stdin.flush();
    }

    /// Przelicza tryby na wysterowanie i wysyła do bridge'a tylko zmiany.
    fn apply(&mut self) {
        let Some(snap) = &self.snapshot else { return };
        let mut commands = Vec::new();

        for fan in &snap.fans {
            if self.skip_kinds.contains(&fan.kind.as_str()) {
                continue;
            }
            let target = target(self.fans.get(&fan.id), &snap.temps)
                .map(|p| p.clamp(MIN_PCT, 100.0).round());

            let prev = self.applied.get(&fan.id).copied();
            match (target, prev) {
                (Some(p), Some(Some(q))) if (p - q).abs() < 1.0 => {}
                (Some(p), _) => {
                    commands.push(format!("set {} {p:.0}", fan.id));
                    self.applied.insert(fan.id.clone(), Some(p));
                }
                (None, Some(Some(_))) => {
                    commands.push(format!("auto {}", fan.id));
                    self.applied.insert(fan.id.clone(), None);
                }
                (None, _) => {}
            }
        }

        for cmd in commands {
            self.send(&cmd);
        }
    }
}

pub struct Backend {
    child: Child,
    inner: Arc<Mutex<Inner>>,
}

impl Backend {
    /// `meter`: licznik energii sesji (liczy także przy schowanym oknie);
    /// `gpu`: ostatni odczyt mocy karty wprost ze sterownika.
    /// `board`: tablica, na którą trafiają temperatury z każdego odczytu;
    /// `skip_kinds`: typy kart, których wentylatorami steruje sterownik karty.
    pub fn spawn(
        ctx: egui::Context,
        fans: Fans,
        meter: SharedMeter,
        gpu: GpuSlot,
        board: crate::gpu_fans::TempBoard,
        skip_kinds: Vec<&'static str>,
    ) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = exe.parent().ok_or(crate::i18n::tr(
            "Nie można ustalić katalogu programu",
            "Cannot determine the program folder",
        ))?;
        let path = dir.join(BRIDGE);
        if !path.exists() {
            return Err(crate::trf!(
                "Nie znaleziono {BRIDGE} obok programu ({}).",
                "{BRIDGE} was not found next to the program ({}).",
                dir.display()
            ));
        }

        let mut cmd = Command::new(&path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn().map_err(|e| {
            crate::trf!(
                "Nie udało się uruchomić {BRIDGE}: {e}",
                "Could not start {BRIDGE}: {e}"
            )
        })?;
        let stdin = child.stdin.take().ok_or(crate::i18n::tr(
            "Brak stdin procesu pomocniczego",
            "The helper process has no stdin",
        ))?;
        let stdout = child.stdout.take().ok_or(crate::i18n::tr(
            "Brak stdout procesu pomocniczego",
            "The helper process has no stdout",
        ))?;

        let inner = Arc::new(Mutex::new(Inner {
            stdin,
            snapshot: None,
            status: Status::Starting,
            fans,
            applied: HashMap::new(),
            skip_kinds,
        }));

        let thread_inner = inner.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(snap) = serde_json::from_str::<Snapshot>(&line) {
                    let mut s = thread_inner.lock().unwrap();
                    if let Some(err) = &snap.error {
                        s.status = Status::Error(err.clone());
                    } else {
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
                        let driver = power::fresh(&gpu, now);
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
                }
                ctx.request_repaint();
            }
            let mut s = thread_inner.lock().unwrap();
            if !matches!(s.status, Status::Error(_)) {
                s.status = Status::Error(
                    crate::i18n::tr(
                        "Proces sensor-bridge zakończył działanie.",
                        "The sensor-bridge process has exited.",
                    )
                    .into(),
                );
            }
            drop(s);
            ctx.request_repaint();
        });

        Ok(Self { child, inner })
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
        // Bridge po `quit` przywraca sterowanie BIOS-u dla wszystkich wentylatorów.
        if let Ok(mut s) = self.inner.lock() {
            s.send("quit");
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if Instant::now() > deadline => {
                    let _ = self.child.kill();
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}
