//! Diagnostyka zgodności: sprawdza, co na danym komputerze działa, a co nie,
//! i zapisuje raport, z którego da się poprawić program bez dostępu do
//! sprzętu (OPTIONS > „Diagnostyka zgodności” albo `tunecenter.exe --diagnose`).
//!
//! Zasady:
//! - wyłącznie odczyty - nic nie jest zapisywane do sprzętu, rejestru ani
//!   ustawień,
//! - raport jest po angielsku (do zgłoszenia na GitHubie) i zawiera surowe
//!   dane: kody błędów, nazwy funkcji, wartości rejestrów,
//! - bez danych osobowych: bez numerów seryjnych i adresów MAC; nazwa
//!   użytkownika i komputera są zastępowane w całym raporcie.

use std::path::PathBuf;
use std::time::Instant;

use crate::tune::reg;

pub const FILE: &str = "tunecenter-diagnostics.txt";
const ISSUES_NEW: &str = "https://github.com/Atimaniac/tune-center/issues/new";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Ok,
    Warn,
    Fail,
    Info,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "[OK]  ",
            Level::Warn => "[WARN]",
            Level::Fail => "[FAIL]",
            Level::Info => "[INFO]",
        }
    }
}

#[derive(Clone, Debug)]
struct Line {
    level: Level,
    text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    sections: Vec<(String, Vec<Line>)>,
    pub when: String,
    pub secs: u64,
    /// Krótki opis sprzętu (procesor, karta, płyta) do tytułu zgłoszenia.
    pub hardware: String,
}

impl Report {
    fn section(&mut self, name: &str) {
        self.sections.push((name.to_owned(), Vec::new()));
    }

    fn push(&mut self, level: Level, text: impl Into<String>) {
        if self.sections.is_empty() {
            self.section("General");
        }
        let text = text.into();
        if let Some(s) = self.sections.last_mut() {
            s.1.push(Line { level, text });
        }
    }

    fn ok(&mut self, t: impl Into<String>) {
        self.push(Level::Ok, t)
    }
    fn warn(&mut self, t: impl Into<String>) {
        self.push(Level::Warn, t)
    }
    fn fail(&mut self, t: impl Into<String>) {
        self.push(Level::Fail, t)
    }
    fn info(&mut self, t: impl Into<String>) {
        self.push(Level::Info, t)
    }

    /// Wynik: OK albo FAIL z tekstem.
    fn result<T, E: std::fmt::Display>(
        &mut self,
        what: &str,
        r: &Result<T, E>,
        ok: impl Fn(&T) -> String,
    ) {
        match r {
            Ok(v) => self.ok(format!("{what}: {}", ok(v))),
            Err(e) => self.fail(format!("{what}: {e}")),
        }
    }

    pub fn count(&self, level: Level) -> usize {
        self.sections
            .iter()
            .flat_map(|s| &s.1)
            .filter(|l| l.level == level)
            .count()
    }

    pub fn text(&self) -> String {
        let mut out = format!(
            "Tune Center compatibility report\nversion {}, generated {}, took {} s\nresult: {} FAIL, {} WARN, {} OK\nhardware: {}\n",
            env!("CARGO_PKG_VERSION"),
            self.when,
            self.secs,
            self.count(Level::Fail),
            self.count(Level::Warn),
            self.count(Level::Ok),
            self.hardware,
        );
        for (name, lines) in &self.sections {
            out.push_str(&format!("\n== {name} ==\n"));
            for l in lines {
                for (i, part) in l.text.lines().enumerate() {
                    if i == 0 {
                        out.push_str(&format!("{} {part}\n", l.level.tag()));
                    } else {
                        out.push_str(&format!("       {part}\n"));
                    }
                }
            }
        }
        anonymize(&out)
    }

    /// Same błędy i ostrzeżenia (do pola „Podsumowanie” zgłoszenia).
    pub fn problems(&self) -> String {
        let mut out = String::new();
        for (name, lines) in &self.sections {
            for l in lines
                .iter()
                .filter(|l| matches!(l.level, Level::Fail | Level::Warn))
            {
                let first = l.text.lines().next().unwrap_or_default();
                out.push_str(&format!("{} {name}: {first}\n", l.level.tag().trim()));
            }
        }
        if out.is_empty() {
            out.push_str("no FAIL / WARN lines\n");
        }
        anonymize(&out)
    }
}

/// Zastępuje nazwę użytkownika, folder profilu i nazwę komputera.
fn anonymize(text: &str) -> String {
    let mut out = text.to_owned();
    let mut replace = |needle: Option<String>, with: &str, word: bool| {
        let Some(n) = needle.filter(|n| n.len() >= 2) else {
            return;
        };
        // Bez rozróżniania wielkości liter (ścieżki Windows).
        let lower = out.to_lowercase();
        let nl = n.to_lowercase();
        let mut result = String::with_capacity(out.len());
        let mut last = 0;
        for (i, _) in lower.match_indices(&nl) {
            if i < last || !out.is_char_boundary(i) || !out.is_char_boundary(i + n.len()) {
                continue;
            }
            // Nazwy (użytkownik, komputer) tylko jako całe słowa - "ATI" nie
            // może zmieniać "compatibility"; ścieżka profilu zawsze.
            if word {
                let before = out[..i].chars().next_back();
                let after = out[i + n.len()..].chars().next();
                if before.is_some_and(char::is_alphanumeric)
                    || after.is_some_and(char::is_alphanumeric)
                {
                    continue;
                }
            }
            result.push_str(&out[last..i]);
            result.push_str(with);
            last = i + n.len();
        }
        result.push_str(&out[last..]);
        out = result;
    };
    let env = |k: &str| std::env::var(k).ok();
    replace(env("USERPROFILE"), "%USERPROFILE%", false);
    replace(env("USERNAME"), "%USERNAME%", true);
    replace(env("COMPUTERNAME"), "%COMPUTERNAME%", true);
    out
}

/// Dane, które działający program ma już w pamięci (żeby nie uruchamiać
/// drugiej kopii procesu czujników ani nie przeszkadzać wątkowi RGB).
#[derive(Default)]
pub struct Live {
    pub bridge: Option<(Option<crate::bridge::Snapshot>, String)>,
    pub rgb: Option<crate::rgb::Snapshot>,
    pub elevated: bool,
}

fn program_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// Ścieżka raportu: obok programu, a gdy tam nie wolno pisać - w %TEMP%.
pub fn save(report: &Report) -> Result<PathBuf, String> {
    let text = report.text();
    let mut tried = Vec::new();
    for dir in [program_dir(), Some(std::env::temp_dir())]
        .into_iter()
        .flatten()
    {
        let path = dir.join(FILE);
        match std::fs::write(&path, &text) {
            Ok(()) => return Ok(path),
            Err(e) => tried.push(format!("{}: {e}", path.display())),
        }
    }
    Err(tried.join("; "))
}

/// Adres nowego zgłoszenia z szablonem i wypełnionym tytułem oraz podsumowaniem.
pub fn issue_url(report: &Report) -> String {
    let title = format!("Compatibility: {}", report.hardware);
    let mut summary = format!(
        "Tune Center {} - {} FAIL, {} WARN\n{}",
        env!("CARGO_PKG_VERSION"),
        report.count(Level::Fail),
        report.count(Level::Warn),
        report.problems()
    );
    // Adres musi zostać krótki - pełny raport idzie przez schowek albo plik.
    if summary.len() > 1800 {
        let mut cut = 1800;
        while !summary.is_char_boundary(cut) {
            cut -= 1;
        }
        summary.truncate(cut);
        summary.push_str("\n...");
    }
    format!(
        "{ISSUES_NEW}?template=compatibility.yml&title={}&summary={}",
        url_encode(&title),
        url_encode(&summary)
    )
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hklm(path: &str, name: &str) -> Option<String> {
    reg::get_string(reg::HKLM, path, name).filter(|s| !s.trim().is_empty())
}

/// Uruchamia wszystkie sprawdzenia. `step` dostaje nazwę bieżącego etapu.
pub fn run(live: Live, step: impl Fn(&str)) -> Report {
    let started = Instant::now();
    let mut r = Report {
        when: crate::bench::now_text(),
        ..Default::default()
    };
    let mut hw = Vec::new();

    step("System");
    system(&mut r, &live, &mut hw);
    step("CPU");
    cpu(&mut r, &mut hw);
    step("GPU");
    gpu(&mut r, &mut hw);
    step("Sensors");
    sensors(&mut r, &live);
    step("Disks");
    disks(&mut r);
    step("RGB");
    rgb(&mut r, &live);
    step("TUNE");
    tune(&mut r);
    step("Shaders");
    shaders(&mut r);
    step("Crash logs");
    crash_logs(&mut r);

    r.hardware = hw.join(" / ");
    r.secs = started.elapsed().as_secs();
    r
}

fn system(r: &mut Report, live: &Live, hw: &mut Vec<String>) {
    r.section("System");
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = format!(
        "{} {} build {}.{}",
        hklm(NT, "ProductName").unwrap_or_default(),
        hklm(NT, "DisplayVersion").unwrap_or_default(),
        hklm(NT, "CurrentBuild").unwrap_or_default(),
        reg::get_dword(reg::HKLM, NT, "UBR").unwrap_or(0)
    );
    r.info(format!("Windows: {build}"));
    r.info(format!(
        "program folder: {}",
        program_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    ));
    r.info(format!(
        "language: {}, elevated: {}, UAC EnableLUA: {:?}",
        crate::i18n::code(),
        live.elevated,
        reg::get_dword(
            reg::HKLM,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
            "EnableLUA"
        )
    ));
    #[cfg(windows)]
    r.info(format!(
        "tray host (Shell_TrayWnd) present: {}",
        crate::tray::tray_host_present()
    ));
    if !live.elevated {
        r.warn("not running as administrator - CPU, CPU sensors, RAM RGB and most TUNE items cannot be read");
    }

    const BIOS: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
    let board = format!(
        "{} {}",
        hklm(BIOS, "BaseBoardManufacturer").unwrap_or_default(),
        hklm(BIOS, "BaseBoardProduct").unwrap_or_default()
    );
    r.info(format!(
        "system: {} {}; board: {board}; BIOS {} {} ({})",
        hklm(BIOS, "SystemManufacturer").unwrap_or_default(),
        hklm(BIOS, "SystemProductName").unwrap_or_default(),
        hklm(BIOS, "BIOSVendor").unwrap_or_default(),
        hklm(BIOS, "BIOSVersion").unwrap_or_default(),
        hklm(BIOS, "BIOSReleaseDate").unwrap_or_default(),
    ));
    if !board.trim().is_empty() {
        hw.push(board.trim().to_owned());
    }

    if crate::bridge::dotnet_runtime_installed() {
        r.ok(".NET 11 runtime installed");
    } else {
        r.fail(".NET 11 runtime not found (sensor process cannot start)");
    }
    let dir = program_dir().unwrap_or_default();
    for f in [
        "sensor-bridge.exe",
        "sensor-bridge.dll",
        "sensor-bridge.runtimeconfig.json",
        "sensor-bridge.deps.json",
        "LibreHardwareMonitorLib.dll",
    ] {
        if dir.join(f).exists() {
            r.ok(format!("{f} present"));
        } else {
            r.fail(format!("{f} missing next to the program"));
        }
    }
    let modules = dir.join("modules");
    let version = std::fs::read_to_string(modules.join("version.txt")).unwrap_or_default();
    for m in [
        "IntelMSR.bin",
        "RyzenSMU.bin",
        "AMDFamily17.bin",
        "SmbusI801.bin",
    ] {
        if modules.join(m).exists() {
            r.ok(format!("PawnIO module {m} present ({})", version.trim()));
        } else {
            r.warn(format!("PawnIO module {m} missing"));
        }
    }
    let pawnio = hklm(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO",
        "DisplayVersion",
    );
    match pawnio {
        Some(v) => r.ok(format!("PawnIO driver installed, version {v}")),
        None => r.warn("PawnIO not found in installed programs (optional; needed for CPU, CPU sensors, RAM RGB)"),
    }
}

fn cpu(r: &mut Report, hw: &mut Vec<String>) {
    use crate::cpu::{amd, intel, pawnio};
    r.section("CPU");
    let info = crate::cpu::cpu_info();
    r.info(format!(
        "{} | intel {} amd {} family 0x{:X} model 0x{:X} hybrid {} physical cores {} logical {}",
        info.name.trim(),
        info.intel,
        info.amd,
        info.family,
        info.model,
        info.hybrid,
        info.physical_cores,
        std::thread::available_parallelism().map_or(0, |n| n.get())
    ));
    hw.push(info.name.trim().to_owned());
    let module = |n: &str| program_dir().unwrap_or_default().join("modules").join(n);

    if info.intel {
        let mut io = match pawnio::PawnIo::open(&module("IntelMSR.bin")) {
            Ok(io) => {
                r.ok("PawnIO IntelMSR opened");
                io
            }
            Err(e) => {
                r.fail(format!("PawnIO IntelMSR: {e}"));
                return;
            }
        };
        // Surowe odczyty MSR używanych przez program.
        for (msr, what) in [
            (0x1A2u64, "TEMPERATURE_TARGET"),
            (0x198, "PERF_STATUS (VID)"),
            (0x1AD, "TURBO_RATIO_LIMIT (P)"),
            (0x650, "TURBO_RATIO_LIMIT (E)"),
            (0x601, "PL4 / VR_CURRENT_CONFIG"),
            (0x606, "RAPL_POWER_UNIT"),
            (0x610, "PKG_POWER_LIMIT"),
            (0x611, "PKG_ENERGY_STATUS"),
            (0x620, "RING_RATIO_LIMIT"),
            (0xCE, "PLATFORM_INFO"),
        ] {
            match intel::Msr::read(&mut io, msr) {
                Ok(v) => r.ok(format!("MSR 0x{msr:X} {what} = 0x{v:016X}")),
                Err(e) => r.fail(format!("MSR 0x{msr:X} {what}: {e}")),
            }
        }
        let state = intel::read_state(&mut io, info.hybrid);
        r.result("power limits", &state.limits, |v| format!("{v:?}"));
        for (domain, v) in &state.voltages {
            r.result(&format!("voltage offset {}", domain.label()), v, |v| {
                format!("{v:?}")
            });
        }
        match &state.iccmax_a {
            Some(v) => r.result("Core IccMax", v, |a| format!("{a} A")),
            None => r.info("Core IccMax: not supported on this model"),
        }
        r.result("P-core turbo ratios", &state.p_turbo, |v| format!("{v:?}"));
        if let Some(e) = &state.e_turbo {
            r.result("E-core turbo ratios", e, |v| format!("{v:?}"));
        }
    } else if info.amd {
        let generation = amd::Generation::detect(info.family, info.model);
        let Some(generation) = generation else {
            r.fail(format!(
                "AMD family 0x{:X} model 0x{:X} not in the generation table",
                info.family, info.model
            ));
            return;
        };
        r.ok(format!("generation {}", generation.name()));
        let mut driver =
            match pawnio::AmdDriver::open(&module("RyzenSMU.bin"), &module("AMDFamily17.bin")) {
                Ok(d) => {
                    r.ok("PawnIO RyzenSMU opened");
                    d
                }
                Err(e) => {
                    r.fail(format!("PawnIO RyzenSMU: {e}"));
                    return;
                }
            };
        let version = match amd::Smu::pm_table(&mut driver) {
            Ok((v, table)) => {
                r.ok(format!("PM table version 0x{v:X}, {} values", table.len()));
                v
            }
            Err(e) => {
                r.fail(format!("PM table: {e}"));
                0
            }
        };
        let layout = amd::layout(generation, version);
        r.info(format!("PM table layout {layout:?}"));
        let state = amd::read_state(
            &mut driver,
            amd::commands(generation),
            layout,
            info.physical_cores,
        );
        r.result("PBO scalar", &state.scalar, |v| format!("{v}"));
        r.result("PPT/TDC/EDC", &state.limits, |v| format!("{v:?}"));
        match &state.cores {
            None => r.info("Curve Optimizer: not available for this generation"),
            Some(Err(e)) => r.fail(format!("Curve Optimizer core map: {e}")),
            Some(Ok(cores)) => {
                let bad: Vec<String> = cores
                    .iter()
                    .filter_map(|(c, v)| v.as_ref().err().map(|e| format!("{c:?}: {e}")))
                    .collect();
                if bad.is_empty() {
                    r.ok(format!("Curve Optimizer: {} cores read", cores.len()));
                } else {
                    r.fail(format!("Curve Optimizer read errors:\n{}", bad.join("\n")));
                }
            }
        }
    } else {
        r.warn("unknown CPU vendor - CPU tab not available");
    }
}

fn gpu(r: &mut Report, hw: &mut Vec<String>) {
    r.section("Graphics card");
    for (dev, bars) in crate::rebar::display_windows() {
        let id: String = dev.split('\\').take(2).collect::<Vec<_>>().join("\\");
        let mb: Vec<String> = bars.iter().map(|b| format!("{} MB", b >> 20)).collect();
        r.info(format!("PCI {id}, memory windows: {}", mb.join(", ")));
    }
    r.info(format!(
        "HAGS (HwSchMode 2 = on): {:?}",
        crate::hags::read()
    ));
    for (ok, line) in crate::gpu::driver_diagnostics() {
        if ok {
            r.ok(line)
        } else {
            r.info(line)
        }
    }
    let Some(mut backend) = crate::gpu::load_drivers() else {
        r.fail("no NVIDIA or AMD driver library could be loaded");
        return;
    };
    let names = backend.gpus();
    if names.is_empty() {
        r.fail("driver loaded but no GPU listed");
    }
    for (i, name) in names.iter().enumerate() {
        hw.push(name.clone());
        r.info(format!("GPU {i}: {name} ({:?})", backend.vendor(i)));
        backend.set_detail(true);
        let m = backend.monitor(i);
        let d = &m.details;
        let line = format!(
            "monitor: temp {:?} hotspot {:?} (hidden {}) mem temp {:?} core {:?} MHz mem {:?} MHz {:?} mV usage {:?}% power {:?} W tdp {:?} W fan {:?} rpm {:?}% pstate {:?} pcie gen {:?} x{:?} limit {:?}",
            m.temp_c, m.hotspot_c, d.hotspot_hidden, d.mem_temp_c, m.core_mhz, m.mem_mhz, m.voltage_mv, m.usage_pct,
            m.power_w, d.tdp_w, m.fan_rpm, m.fan_pct, d.pstate, d.pcie_gen, d.pcie_width, d.limit
        );
        if m.temp_c.is_none() || m.power_w.is_none() {
            r.warn(line);
        } else {
            r.ok(line);
        }
        match backend.fan(i) {
            Some(f) => r.ok(format!(
                "fan status {f:?}, controllable {}",
                backend.fan_controllable(i)
            )),
            None => r.warn("fan status not available from the driver"),
        }
        match backend.offsets(i) {
            Ok(o) => {
                let f = |x: &Option<crate::gpu::backend::OffsetInfo>| {
                    x.as_ref().map_or("n/a".to_owned(), |o| {
                        format!("{} kHz [{}..{}]", o.value_khz, o.min_khz, o.max_khz)
                    })
                };
                r.ok(format!(
                    "clock offsets: core {} mem {}",
                    f(&o.core),
                    f(&o.mem)
                ));
            }
            Err(e) => r.fail(format!("clock offsets: {e}")),
        }
        match backend.curve(i) {
            Ok(c) if c.is_empty() => r.info("V/F curve: not available (AMD or unsupported)"),
            Ok(c) => {
                let p = |p: &crate::gpu::backend::CurvePoint| {
                    format!(
                        "#{} {} mV {} MHz ({:+} MHz)",
                        p.index,
                        p.volt_uv / 1000,
                        p.freq_khz / 1000,
                        p.delta_khz / 1000
                    )
                };
                r.ok(format!(
                    "V/F curve: {} points, first {}, last {}",
                    c.len(),
                    p(&c[0]),
                    p(&c[c.len() - 1])
                ));
            }
            Err(e) => r.fail(format!("V/F curve: {e}")),
        }
    }
}

fn sensors(r: &mut Report, live: &Live) {
    r.section("Sensors (sensor-bridge / LibreHardwareMonitor)");
    let (snap, status) = match &live.bridge {
        Some((snap, status)) => (snap.clone(), status.clone()),
        None => bridge_once(),
    };
    r.info(format!("sensor process: {status}"));
    let Some(s) = snap else {
        if status.starts_with("another sensor-bridge") {
            r.warn("no data from the sensor process");
        } else {
            r.fail("no data from the sensor process");
        }
        return;
    };
    if let Some(e) = &s.error {
        r.fail(format!("sensor process error: {e}"));
    }
    if !s.admin {
        r.warn("sensor process runs without administrator rights");
    }
    let mut kinds: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for t in &s.temps {
        kinds.entry(format!("{} [{}]", t.kind, t.hw)).or_default().0 += 1;
    }
    for f in &s.fans {
        kinds.entry(format!("{} [{}]", f.kind, f.hw)).or_default().1 += 1;
    }
    for (k, (t, f)) in &kinds {
        r.info(format!("{k}: {t} temperatures, {f} controllable fans"));
    }
    if !s.temps.iter().any(|t| t.kind == "Cpu" && t.value.is_some()) {
        r.warn("no CPU temperature");
    }
    if !kinds.keys().any(|k| k.starts_with("SuperIO")) {
        r.warn("no Super I/O chip detected - motherboard fans and temperatures unavailable");
    }
    for f in &s.fans {
        let line = format!(
            "fan {} ({} / {}): {:?} rpm, {:?} %",
            f.id, f.hw, f.name, f.rpm, f.percent
        );
        if f.rpm.is_some() || f.percent.is_some() {
            r.ok(line);
        } else {
            r.info(line);
        }
    }
    for t in s.temps.iter().filter(|t| t.value.is_none()) {
        r.info(format!("temperature without value: {}", t.label()));
    }
}

/// Jednorazowe uruchomienie procesu czujników (tryb `--diagnose`). Nie
/// uruchamiamy go, gdy działa już inny - mógłby oddać wentylatory BIOS-owi.
fn bridge_once() -> (Option<crate::bridge::Snapshot>, String) {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};
    if other_bridge_running() {
        return (None, "another sensor-bridge.exe is running - not started (run the diagnostics from OPTIONS instead)".into());
    }
    let Some(path) = program_dir().map(|d| d.join("sensor-bridge.exe")) else {
        return (None, "no program folder".into());
    };
    let mut cmd = Command::new(&path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (None, format!("start failed: {e}")),
    };
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(out) = stdout else { return };
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(s) = serde_json::from_str::<crate::bridge::Snapshot>(&line) {
                // Pierwszy pełny odczyt (z czujnikami) albo błąd.
                if !s.temps.is_empty() || s.error.is_some() {
                    let _ = tx.send(s);
                    return;
                }
            }
        }
    });
    let snap = rx.recv_timeout(std::time::Duration::from_secs(25)).ok();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"quit\n");
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = child.kill();
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }
    let status = match (&snap, stderr.trim()) {
        (Some(_), _) => "started for the diagnostics, first reading received".to_owned(),
        (None, "") => "no reading within 25 s".to_owned(),
        (None, e) => format!(
            "no reading; stderr:\n{}",
            e.lines().take(40).collect::<Vec<_>>().join("\n")
        ),
    };
    (snap, status)
}

fn other_bridge_running() -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq sensor-bridge.exe", "/NH"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("sensor-bridge.exe")
        })
        .unwrap_or(false)
}

fn disks(r: &mut Report) {
    r.section("Disks (Windows storage API)");
    let list = crate::disks::read_now();
    if list.is_empty() {
        r.warn("no disk reports a temperature through the Windows API (SATA disks use the sensor process)");
    }
    for d in list {
        r.ok(format!("{}: {:.0} °C", d.name, d.temp_c));
    }
}

fn rgb(r: &mut Report, live: &Live) {
    use crate::rgb;
    r.section("RGB");
    let (devices, problems) = match &live.rgb {
        Some(s) => (
            s.devices
                .iter()
                .map(|e| (e.info.clone(), e.status.clone()))
                .collect::<Vec<_>>(),
            s.problems
                .iter()
                .map(|p| (p.family, p.text.clone()))
                .collect::<Vec<_>>(),
        ),
        None => {
            let saved: rgb::RamSaved = Default::default();
            let (found, problems) = rgb::discover(live.elevated, &saved);
            (
                found.iter().map(|d| (d.info(), None)).collect(),
                problems.into_iter().map(|p| (p.family, p.text)).collect(),
            )
        }
    };
    for (info, status) in devices {
        let st = match status {
            Some(Ok(t)) => format!(" - {t}"),
            Some(Err(e)) => format!(" - ERROR {e}"),
            None => String::new(),
        };
        let line = format!(
            "{} [{}{}] {} ({}){st}",
            info.name,
            info.family.label(),
            if info.family.tested() {
                ""
            } else {
                ", untested"
            },
            info.detail,
            info.id
        );
        if st.contains("ERROR") {
            r.fail(line);
        } else {
            r.ok(line);
        }
    }
    for (family, text) in problems {
        r.warn(format!("{}: {text}", family.label()));
    }
    r.info(format!(
        "USB HID devices:\n{}",
        rgb::hid_inventory().join("\n")
    ));
}

fn tune(r: &mut Report) {
    use crate::tune::State;
    r.section("TUNE (read only)");
    for item in crate::tune::read_all(false) {
        match &item.state {
            State::Optimized(v) => r.ok(format!("{}: ON - {v}", item.key)),
            State::Default(v) => r.ok(format!("{}: OFF - {v}", item.key)),
            State::Unsupported(v) => r.info(format!("{}: n/a - {v}", item.key)),
            State::Unknown(v) => r.fail(format!("{}: read error - {v}", item.key)),
        }
    }
}

fn shaders(r: &mut Report) {
    use crate::shaders::{engine, games};
    r.section("Shader compilation (detected games)");
    let list = games::find_all();
    r.info(format!("{} games found", list.len()));
    for g in list.iter().take(60) {
        let i = engine::inspect(&g.dir);
        let line = format!(
            "{} [{}]: engine {} {} api {} shader files {} ({}) encrypted {} anti-cheat {:?}",
            g.name,
            g.store.name(),
            i.engine.name(),
            i.version.as_deref().unwrap_or("-"),
            i.api.as_deref().unwrap_or("-"),
            i.shaders.len(),
            engine::human(i.shader_bytes()),
            i.encrypted,
            i.anti_cheat
        );
        if matches!(i.engine, engine::Engine::Unknown) {
            r.info(line);
        } else {
            r.ok(line);
        }
    }
}

fn crash_logs(r: &mut Report) {
    r.section("Crash logs");
    let dir = program_dir().unwrap_or_default();
    let mut any = false;
    for f in [
        "tunecenter-crash.log",
        "tunecenter-crash-previous.log",
        "sensor-bridge-crash.log",
    ] {
        if let Ok(text) = std::fs::read_to_string(dir.join(f)) {
            any = true;
            let tail: Vec<&str> = text
                .lines()
                .rev()
                .take(60)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            r.warn(format!("{f}:\n{}", tail.join("\n")));
        }
    }
    if !any {
        r.ok("no crash logs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_format() {
        let mut r = Report::default();
        r.section("A");
        r.ok("fine");
        r.fail("broken\nsecond line");
        r.warn("careful");
        let t = r.text();
        assert!(t.contains("== A =="));
        assert!(t.contains("[FAIL] broken\n       second line"));
        assert_eq!(r.count(Level::Fail), 1);
        assert!(r.problems().contains("[FAIL] A: broken"));
        assert!(issue_url(&r).starts_with(ISSUES_NEW));
        assert_eq!(url_encode("a b/ż"), "a%20b%2F%C5%BC");
    }

    #[test]
    fn anonymizes_user() {
        std::env::set_var("USERNAME", "Zbyszek");
        let t = anonymize(r"C:\Users\ZBYSZEK\AppData and zbyszek");
        assert!(!t.to_lowercase().contains("zbyszek"), "{t}");
    }

    /// Pełny raport na tym komputerze (tylko odczyty):
    /// `cargo test diagnose_full -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn diagnose_full() {
        let r = run(
            Live {
                elevated: crate::elevation::is_elevated(),
                ..Default::default()
            },
            |s| eprintln!("-- {s}"),
        );
        println!("{}", r.text());
    }
}

/// Wynik diagnostyki w OPTIONS.
struct Outcome {
    report: Report,
    path: Result<PathBuf, String>,
}

/// Sekcja „Diagnostyka zgodności” w OPTIONS: uruchomienie w tle, wynik,
/// zgłoszenie na GitHubie.
#[derive(Default)]
pub struct Panel {
    step: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    result: std::sync::Arc<std::sync::Mutex<Option<Outcome>>>,
    outcome: Option<Outcome>,
    note: Option<String>,
}

impl Panel {
    fn running(&self) -> bool {
        self.step.lock().map(|s| s.is_some()).unwrap_or(false)
    }

    fn start(&mut self, live: Live, ctx: &eframe::egui::Context) {
        if self.running() {
            return;
        }
        self.outcome = None;
        self.note = None;
        if let Ok(mut s) = self.step.lock() {
            *s = Some(String::new());
        }
        let (step, result, ctx) = (self.step.clone(), self.result.clone(), ctx.clone());
        crate::spawn("diagnose", move || {
            let report = run(live, |name| {
                if let Ok(mut s) = step.lock() {
                    *s = Some(name.to_owned());
                }
                ctx.request_repaint();
            });
            let path = save(&report);
            if let Ok(mut r) = result.lock() {
                *r = Some(Outcome { report, path });
            }
            if let Ok(mut s) = step.lock() {
                *s = None;
            }
            ctx.request_repaint();
        });
    }

    pub fn ui(
        &mut self,
        ui: &mut eframe::egui::Ui,
        ctx: &eframe::egui::Context,
        live: impl FnOnce() -> Live,
    ) {
        use crate::i18n::tr;
        use crate::theme;
        use eframe::egui::{self, RichText};

        if let Some(o) = self.result.lock().ok().and_then(|mut r| r.take()) {
            self.outcome = Some(o);
        }
        let running = self.running();
        ui.horizontal_wrapped(|ui| {
            if theme::primary_button(
                ui,
                Some(crate::icons::Icon::Check),
                tr("Diagnostyka zgodności", "Compatibility check"),
                !running,
            )
            .clicked()
            {
                self.start(live(), ctx);
            }
            if running {
                ui.spinner();
                let step = self
                    .step
                    .lock()
                    .ok()
                    .and_then(|s| s.clone())
                    .unwrap_or_default();
                ui.label(
                    RichText::new(crate::trf!("Sprawdzanie: {step}...", "Checking: {step}..."))
                        .color(theme::muted()),
                );
            }
        });
        ui.label(
            RichText::new(tr(
                "Sprawdza, co na tym komputerze działa, a co nie (procesor, karta, czujniki, dyski, RGB, TUNE, gry) - wyłącznie odczyty, nic nie jest zmieniane. Raport (bez numerów seryjnych i nazwy użytkownika) można zgłosić na GitHubie, żeby autor mógł poprawić program bez dostępu do Twojego sprzętu. Trwa od kilkunastu sekund do minuty.",
                "Checks what works on this computer and what does not (CPU, card, sensors, disks, RGB, TUNE, games) - read-only, nothing is changed. The report (without serial numbers or user name) can be submitted on GitHub so the author can fix the program without access to your hardware. Takes from several seconds to a minute.",
            ))
            .size(11.5)
            .color(theme::muted()),
        );
        let Some(o) = &self.outcome else {
            return;
        };
        let (fails, warns) = (o.report.count(Level::Fail), o.report.count(Level::Warn));
        let color = if fails > 0 {
            theme::warn()
        } else {
            theme::ok()
        };
        ui.label(
            RichText::new(crate::trf!(
                "Wynik: błędy {fails}, ostrzeżenia {warns}, czas {} s.",
                "Result: {fails} errors, {warns} warnings, {} s.",
                o.report.secs
            ))
            .color(color),
        );
        match &o.path {
            Ok(p) => ui.label(
                RichText::new(p.display().to_string())
                    .size(11.5)
                    .color(theme::accent()),
            ),
            Err(e) => ui.label(RichText::new(e).size(11.5).color(theme::warn())),
        };
        let mut open_folder = None;
        ui.horizontal_wrapped(|ui| {
            if theme::primary_button(ui, Some(crate::icons::Icon::Download), tr("Zgłoś na GitHubie", "Report on GitHub"), true)
                .clicked()
            {
                // Pełny raport przez schowek - adres zgłoszenia ma limit długości.
                ctx.copy_text(o.report.text());
                ctx.open_url(egui::OpenUrl::new_tab(issue_url(&o.report)));
                self.note = Some(
                    tr(
                        "Raport jest w schowku: na stronie zgłoszenia kliknij pole „Raport” i wklej go (Ctrl+V), albo przeciągnij tam plik raportu. Do zgłoszenia potrzebne jest konto GitHub.",
                        "The report is in the clipboard: on the issue page click the \"Report\" field and paste it (Ctrl+V), or drag the report file there. A GitHub account is needed to submit.",
                    )
                    .to_owned(),
                );
            }
            if ui.button(tr("Kopiuj raport", "Copy report")).clicked() {
                ctx.copy_text(o.report.text());
                self.note = Some(tr("Skopiowano do schowka.", "Copied to the clipboard.").to_owned());
            }
            if let Ok(p) = &o.path {
                if ui.button(tr("Otwórz folder", "Open folder")).clicked() {
                    open_folder = Some(p.clone());
                }
            }
        });
        if let Some(p) = open_folder {
            let _ = std::process::Command::new("explorer")
                .arg(format!("/select,{}", p.display()))
                .spawn();
        }
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).size(12.0).color(theme::accent()));
        }
    }
}
