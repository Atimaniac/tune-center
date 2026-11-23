//! Komunikacja z procesem pomocniczym `sensor-bridge` (C# + LibreHardwareMonitor)
//! oraz sterowanie wentylatorami.
//!
//! Bridge co sekundę wypisuje jedną linię JSON ze stanem czujników, a polecenia
//! przyjmuje na stdin: `set <id> <procent>`, `auto <id>`, `quit`.
//! Sterowanie działa w wątku czytającym, niezależnie od interfejsu, więc krzywe
//! są stosowane także wtedy, gdy okno jest zminimalizowane.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
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
    /// Proces czujników (podmieniany przy ponownym uruchomieniu po awarii).
    child: Child,
    /// Program się zamyka - zakończenie procesu nie jest awarią.
    quitting: bool,
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
    /// Ostatnie wiersze stderr procesu czujników (np. stos wyjątku .NET).
    stderr: StderrLines,
    /// Raport ostatniej awarii procesu czujników i czy użytkownik go jeszcze nie widział.
    crash: Option<String>,
    crash_unseen: bool,
}

/// Ostatnie wiersze wyjścia błędów procesu czujników.
type StderrLines = Arc<Mutex<std::collections::VecDeque<String>>>;
const STDERR_LINES: usize = 60;

/// Raport awarii procesu czujników: kod wyjścia i ostatnie wiersze stderr;
/// dopisywany też do `sensor-bridge-crash.log` obok programu.
fn crash_report(code: Option<i32>, stderr: &StderrLines) -> String {
    let code_text = match code {
        Some(c) => {
            let u = c as u32;
            format!("0x{u:08X} ({c}) - {}", crate::crash::describe(u))
        }
        None => "?".into(),
    };
    let lines: Vec<String> = stderr
        .lock()
        .map(|l| l.iter().cloned().collect())
        .unwrap_or_default();
    let mut text = format!(
        "{} - {BRIDGE} {}\n{} {code_text}\n",
        crate::bench::now_text(),
        env!("CARGO_PKG_VERSION"),
        crate::i18n::tr("Kod wyjścia:", "Exit code:"),
    );
    if lines.is_empty() {
        text.push_str(crate::i18n::tr(
            "(proces nie wypisał komunikatu błędu)",
            "(the process printed no error message)",
        ));
    } else {
        text.push_str(&lines.join("\n"));
    }
    if text.contains("NvidiaML") || text.to_lowercase().contains("nvml") {
        text.push_str(crate::i18n::tr(
            "\n\nBłąd w bibliotece sterownika NVIDIA (NVML) - zwykle przy instalacji lub przeładowaniu sterownika karty.",
            "\n\nError in the NVIDIA driver library (NVML) - usually while the graphics driver is being installed or reloaded.",
        ));
    }
    if let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("sensor-bridge-crash.log")))
    {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{text}\n----------------------------------------");
        }
    }
    text
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
    /// Odczyty "brak danych" (65535 obr./min) jako brak odczytu.
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
    }
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
            if self.skip_kinds.contains(&fan.kind.as_str()) || read_only_chip(&fan.hw) {
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
    inner: Arc<Mutex<Inner>>,
}

/// Ile razy z rzędu ponawiamy uruchomienie procesu czujników po awarii.
const MAX_RESTARTS: u32 = 5;
/// Proces, który przepracował tyle czasu, zeruje licznik ponowień.
const STABLE_AFTER: Duration = Duration::from_secs(120);

/// Wersja .NET, której wymaga sensor-bridge (TargetFramework w csproj).
const DOTNET_MAJOR: &str = "11.";

/// Czy jest runtime .NET w wersji wymaganej przez sensor-bridge
/// (`%ProgramFiles%\dotnet\shared\Microsoft.NETCore.App\11.*`).
pub fn dotnet_runtime_installed() -> bool {
    let base = std::env::var_os("ProgramFiles")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Program Files"));
    std::fs::read_dir(base.join(r"dotnet\shared\Microsoft.NETCore.App"))
        .map(|list| {
            list.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(DOTNET_MAJOR))
        })
        .unwrap_or(false)
}

/// Uruchamia proces czujników bez okna konsoli; jego stderr (komunikaty
/// błędów, stos wyjątku .NET) trafia do `stderr`.
fn start_child(
    path: &std::path::Path,
    stderr: &StderrLines,
) -> Result<(Child, ChildStdin, ChildStdout), String> {
    let mut cmd = Command::new(path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    if let Some(err) = child.stderr.take() {
        let buf = stderr.clone();
        if let Ok(mut b) = buf.lock() {
            b.clear();
        }
        crate::spawn("bridge-stderr", move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if let Ok(mut b) = buf.lock() {
                    if b.len() >= STDERR_LINES {
                        b.pop_front();
                    }
                    b.push_back(line);
                }
            }
        });
    }
    Ok((child, stdin, stdout))
}

impl Backend {
    /// `meter`: licznik energii sesji (liczy także przy schowanym oknie);
    /// `gpu`: ostatni odczyt mocy karty wprost ze sterownika.
    /// `board`: tablica, na którą trafiają temperatury z każdego odczytu;
    /// `skip_kinds`: typy kart, których wentylatorami steruje sterownik karty.
    ///
    /// Gdy proces czujników zakończy się sam (np. awaria biblioteki
    /// sterownika), jest uruchamiany ponownie (do `MAX_RESTARTS` razy z rzędu),
    /// a wentylatory dostają ponownie swoje ustawienia.
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

        let stderr: StderrLines = Arc::default();
        let (child, stdin, stdout) = start_child(&path, &stderr)?;
        let inner = Arc::new(Mutex::new(Inner {
            stdin,
            child,
            quitting: false,
            snapshot: None,
            status: Status::Starting,
            fans,
            applied: HashMap::new(),
            skip_kinds,
            stderr: stderr.clone(),
            crash: None,
            crash_unseen: false,
        }));

        let thread_inner = inner.clone();
        crate::spawn("bridge", move || {
            let mut stdout = stdout;
            let mut restarts = 0u32;
            loop {
                let started = Instant::now();
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(mut snap) = serde_json::from_str::<Snapshot>(&line) {
                        snap.sanitize();
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

                // Proces się zakończył. Przy zamykaniu programu - koniec;
                // inaczej (awaria) uruchamiamy go ponownie.
                {
                    let mut s = thread_inner.lock().unwrap();
                    if s.quitting {
                        break;
                    }
                    let exit = s.child.wait().ok();
                    // Awaria (kod różny od 0): raport z kodem i komunikatem błędu,
                    // okno w programie pokazuje go jeden raz.
                    if let Some(code) = exit.and_then(|e| e.code()).filter(|c| *c != 0) {
                        // Chwila na odczyt ostatnich wierszy stderr (wątek czytający).
                        std::thread::sleep(Duration::from_millis(300));
                        s.crash = Some(crash_report(Some(code), &s.stderr));
                        s.crash_unseen = true;
                    }
                    if started.elapsed() > STABLE_AFTER {
                        restarts = 0;
                    }
                    if restarts >= MAX_RESTARTS {
                        if !matches!(s.status, Status::Error(_)) {
                            // Najczęstsza przyczyna po aktualizacji: brak runtime .NET 11
                            // (np. winget w instalatorze nie zadziałał).
                            let text = if dotnet_runtime_installed() {
                                crate::i18n::tr(
                                    "Proces sensor-bridge kilka razy z rzędu zakończył działanie.",
                                    "The sensor-bridge process exited several times in a row.",
                                )
                            } else {
                                crate::i18n::tr(
                                    "Brak .NET 11 Runtime - czujniki i wentylatory nie działają. Zainstaluj go: winget install Microsoft.DotNet.Runtime.11 (albo dotnet.microsoft.com/download/dotnet/11.0).",
                                    "The .NET 11 Runtime is missing - sensors and fans do not work. Install it: winget install Microsoft.DotNet.Runtime.11 (or dotnet.microsoft.com/download/dotnet/11.0).",
                                )
                            };
                            s.status = Status::Error(text.into());
                        }
                        break;
                    }
                    s.status = Status::Starting;
                }
                ctx.request_repaint();
                restarts += 1;
                std::thread::sleep(Duration::from_secs(2));

                match start_child(&path, &stderr) {
                    Ok((child, stdin, out)) => {
                        let mut s = thread_inner.lock().unwrap();
                        if s.quitting {
                            // Program zamyka się w trakcie ponowienia.
                            s.child = child;
                            s.stdin = stdin;
                            s.send("quit");
                            break;
                        }
                        s.child = child;
                        s.stdin = stdin;
                        // Nowy proces nie zna wysterowania - wyślemy je od nowa.
                        s.applied.clear();
                        stdout = out;
                    }
                    Err(e) => {
                        thread_inner.lock().unwrap().status = Status::Error(e);
                        break;
                    }
                }
            }
            ctx.request_repaint();
        });

        Ok(Self { inner })
    }

    /// Raport ostatniej awarii procesu czujników i czy jest jeszcze niepokazany.
    pub fn crash_report(&self) -> Option<(String, bool)> {
        let s = self.inner.lock().ok()?;
        s.crash.clone().map(|c| (c, s.crash_unseen))
    }

    /// Okno z raportem zamknięte - następne pokaże dopiero nowa awaria.
    pub fn dismiss_crash(&self) {
        if let Ok(mut s) = self.inner.lock() {
            s.crash_unseen = false;
        }
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
            s.quitting = true;
            s.send("quit");
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let Ok(mut s) = self.inner.lock() else { break };
            match s.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if Instant::now() > deadline => {
                    let _ = s.child.kill();
                    break;
                }
                Ok(None) => {
                    drop(s);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

#[cfg(test)]
mod invalid_rpm_tests {
    use super::*;

    #[test]
    fn rpm_sentinel_and_read_only_chip() {
        let json = r#"{"temps":[],"fans":[
            {"id":"a","name":"Fan #1","hw":"Nuvoton NCT6683D","kind":"SuperIO","rpm":65535.0,"percent":100.0},
            {"id":"b","name":"Fan #2","hw":"Nuvoton NCT6796D-R","kind":"SuperIO","rpm":1200.0,"percent":40.0}],
            "speeds":[{"name":"x","hw":"h","rpm":65535.0,"percent":null}]}"#;
        let mut s: Snapshot = serde_json::from_str(json).unwrap();
        s.sanitize();
        assert_eq!(s.fans[0].rpm, None);
        assert_eq!(s.fans[1].rpm, Some(1200.0));
        assert_eq!(s.speeds[0].rpm, None);
        assert!(read_only_chip("Nuvoton NCT6683D"));
        assert!(!read_only_chip("Nuvoton NCT6796D-R"));
        assert!(!read_only_chip("Nuvoton NCT6701D"));
    }
}
