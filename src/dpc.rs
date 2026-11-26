//! Analizator DPC: czas wykonania procedur DPC i obsługi przerwań (ISR) dla
//! każdego sterownika, liczba twardych błędów stron i opóźnienie wybudzenia
//! wątku - prosty odpowiednik LatencyMon bez własnego sterownika.
//!
//! Dane pochodzą z systemowego śledzenia ETW (rejestrator jądra: DPC,
//! przerwania, twarde błędy stron) - sama obserwacja, wymaga administratora.
//! Zdarzenie DPC/ISR niesie czas wejścia do procedury i jej adres; czas
//! trwania to różnica do znacznika zdarzenia, a adres wskazuje sterownik.
//! Opóźnienie wybudzenia: jeden wątek co 1 ms ustawia zdarzenie, drugi (o
//! najwyższym priorytecie) mierzy, po jakim czasie dostał procesor -
//! przybliżenie wskaźnika "interrupt to process latency".

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Rodzaj procedury.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Dpc,
    Isr,
}

/// Liczniki jednej procedury albo sterownika (czasy w mikrosekundach).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Times {
    pub count: u64,
    pub max_us: f64,
    pub total_us: f64,
}

impl Times {
    fn add(&mut self, us: f64) {
        self.count += 1;
        self.total_us += us;
        self.max_us = self.max_us.max(us);
    }

    fn merge(&mut self, o: &Times) {
        self.count += o.count;
        self.total_us += o.total_us;
        self.max_us = self.max_us.max(o.max_us);
    }

    pub fn avg_us(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_us / self.count as f64
        }
    }
}

/// Zbiera zdarzenia w trakcie pomiaru (klucz: rodzaj i adres procedury).
#[derive(Default)]
pub struct Collector {
    routines: HashMap<(Kind, u64), Times>,
    pub hard_faults: u64,
    /// Procedury dłuższe niż 1 ms.
    pub over_1ms: u64,
    /// Liczba wywołań procedury na danym procesorze logicznym.
    cpus: HashMap<(u64, u16), u32>,
}

impl Collector {
    /// Czasy dłuższe niż sekunda to błąd odczytu (zawinięty licznik) - pomijane.
    pub fn add(&mut self, kind: Kind, routine: u64, us: f64) {
        if !(0.0..1_000_000.0).contains(&us) {
            return;
        }
        if us >= 1000.0 {
            self.over_1ms += 1;
        }
        self.routines.entry((kind, routine)).or_default().add(us);
    }

    pub fn add_cpu(&mut self, routine: u64, cpu: u16) {
        *self.cpus.entry((routine, cpu)).or_default() += 1;
    }
}

/// Sterowniki kart graficznych (małymi literami).
const GPU_DRIVERS: [&str; 7] =
    ["nvlddmkm.sys", "amdkmdag.sys", "atikmdag.sys", "atikmpag.sys", "igdkmd64.sys", "igdkmdn64.sys", "igdkmdnd64.sys"];

/// Procesor logiczny, na którym wykonuje się najwięcej DPC sterownika karty
/// graficznej (przy kilku kartach - tej z największą liczbą DPC).
pub fn gpu_cpu(c: &Collector, modules: &[(u64, String)]) -> Option<u16> {
    let mut by: HashMap<(String, u16), u64> = HashMap::new();
    for ((routine, cpu), n) in &c.cpus {
        let driver = driver_of(modules, *routine).to_lowercase();
        if GPU_DRIVERS.contains(&driver.as_str()) {
            *by.entry((driver, *cpu)).or_default() += *n as u64;
        }
    }
    let mut totals: HashMap<&str, u64> = HashMap::new();
    for ((driver, _), n) in &by {
        *totals.entry(driver.as_str()).or_default() += n;
    }
    let top = totals.into_iter().max_by_key(|(d, n)| (*n, d.to_string()))?.0.to_owned();
    by.into_iter().filter(|((d, _), _)| *d == top).max_by_key(|((_, cpu), n)| (*n, u16::MAX - *cpu)).map(|((_, cpu), _)| cpu)
}

/// Wiersz wyniku: sterownik z czasami DPC i ISR.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub driver: String,
    pub dpc: Times,
    pub isr: Times,
}

impl Row {
    pub fn worst_us(&self) -> f64 {
        self.dpc.max_us.max(self.isr.max_us)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub secs: f64,
    /// Sterowniki od najgorszego (najdłuższa pojedyncza procedura).
    pub rows: Vec<Row>,
    pub dpc_max_us: f64,
    pub isr_max_us: f64,
    pub over_1ms: u64,
    pub hard_faults: u64,
    /// Opóźnienie wybudzenia wątku: największe i średnie.
    pub wake_max_us: f64,
    pub wake_avg_us: f64,
    /// Udział czasu DPC i ISR w czasie wszystkich rdzeni (%).
    pub busy_pct: f64,
}

/// Ocena wyniku: najdłuższa procedura i opóźnienie wybudzenia.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Good,
    Fair,
    Bad,
}

impl Report {
    pub fn verdict(&self) -> Verdict {
        let worst = self.dpc_max_us.max(self.isr_max_us);
        if worst >= 1000.0 || self.wake_max_us >= 2000.0 {
            Verdict::Bad
        } else if worst >= 500.0 || self.wake_max_us >= 1000.0 {
            Verdict::Fair
        } else {
            Verdict::Good
        }
    }
}

/// Sterownik, do którego należy adres: moduły jako (adres bazowy, nazwa),
/// posortowane rosnąco - ostatni o bazie nie większej niż adres.
pub fn driver_of(modules: &[(u64, String)], address: u64) -> String {
    let i = modules.partition_point(|(base, _)| *base <= address);
    match i.checked_sub(1).and_then(|i| modules.get(i)) {
        Some((_, name)) => name.clone(),
        None => format!("0x{address:X}"),
    }
}

/// Składa raport: procedury łączone po sterowniku.
pub fn report(c: &Collector, modules: &[(u64, String)], secs: f64, cpus: usize, wake: (f64, f64)) -> Report {
    let mut by_driver: HashMap<String, Row> = HashMap::new();
    for ((kind, routine), times) in &c.routines {
        let name = driver_of(modules, *routine);
        let row = by_driver.entry(name.clone()).or_insert_with(|| Row { driver: name, ..Default::default() });
        match kind {
            Kind::Dpc => row.dpc.merge(times),
            Kind::Isr => row.isr.merge(times),
        }
    }
    let mut rows: Vec<Row> = by_driver.into_values().collect();
    rows.sort_by(|a, b| b.worst_us().total_cmp(&a.worst_us()).then(a.driver.cmp(&b.driver)));
    let total: f64 = rows.iter().map(|r| r.dpc.total_us + r.isr.total_us).sum();
    Report {
        secs,
        dpc_max_us: rows.iter().map(|r| r.dpc.max_us).fold(0.0, f64::max),
        isr_max_us: rows.iter().map(|r| r.isr.max_us).fold(0.0, f64::max),
        over_1ms: c.over_1ms,
        hard_faults: c.hard_faults,
        wake_max_us: wake.0,
        wake_avg_us: wake.1,
        busy_pct: if secs > 0.0 && cpus > 0 { total / (secs * 1e6 * cpus as f64) * 100.0 } else { 0.0 },
        rows,
    }
}

#[cfg(windows)]
mod sys {
    use super::{Collector, Kind};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use windows::core::{GUID, HSTRING, PWSTR};
    use windows::Win32::System::Diagnostics::Etw::{
        CloseTrace, ControlTraceW, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE, EVENT_RECORD,
        EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_FLAG, EVENT_TRACE_FLAG_DPC, EVENT_TRACE_FLAG_INTERRUPT,
        EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE,
        EVENT_TRACE_SYSTEM_LOGGER_MODE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_RAW_TIMESTAMP,
        PROCESS_TRACE_MODE_REAL_TIME, WNODE_FLAG_TRACED_GUID,
    };

    /// Klasy zdarzeń rejestratora jądra.
    const PERF_INFO: GUID = GUID::from_u128(0xCE1DBFB4_137E_4DA6_87B0_3F59AA102CBC);
    const PAGE_FAULT: GUID = GUID::from_u128(0x3D6FA8D3_FE05_11D0_9DDA_00C04FD7BA7C);
    /// Kody zdarzeń PerfInfo: DPC wątkowe, ISR, DPC, DPC zegara.
    const OP_THREAD_DPC: u8 = 66;
    const OP_ISR: u8 = 67;
    const OP_DPC: u8 = 68;
    const OP_TIMER_DPC: u8 = 69;
    const OP_HARD_FAULT: u8 = 32;
    /// Sesja śledzenia: nazwa i identyfikator. Osobna dla analizatora i dla
    /// wykrywania rdzenia karty, żeby jedno nie przerywało drugiego.
    #[derive(Clone, Copy)]
    pub struct Session {
        name: &'static str,
        guid: GUID,
    }
    pub const ANALYZER: Session =
        Session { name: "TuneCenterDpc", guid: GUID::from_u128(0x7D3C1B5A_41E2_4C8E_9A57_54756E654350) };
    pub const CORE: Session =
        Session { name: "TuneCenterDpcCore", guid: GUID::from_u128(0x7D3C1B5A_41E2_4C8E_9A57_54756E654352) };
    const ERROR_ALREADY_EXISTS: u32 = 183;

    #[link(name = "kernel32")]
    extern "system" {
        fn K32EnumDeviceDrivers(bases: *mut *mut c_void, bytes: u32, needed: *mut u32) -> i32;
        fn K32GetDeviceDriverBaseNameW(base: *mut c_void, name: *mut u16, len: u32) -> u32;
        fn QueryPerformanceCounter(value: *mut i64) -> i32;
        fn QueryPerformanceFrequency(value: *mut i64) -> i32;
        fn CreateEventW(attrs: *const c_void, manual: i32, initial: i32, name: *const u16) -> isize;
        fn SetEvent(event: isize) -> i32;
        fn WaitForSingleObject(handle: isize, ms: u32) -> u32;
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
        fn SetThreadAffinityMask(thread: isize, mask: usize) -> usize;
        fn CloseHandle(handle: isize) -> i32;
    }

    pub fn qpc() -> i64 {
        let mut v = 0i64;
        unsafe { QueryPerformanceCounter(&mut v) };
        v
    }

    pub fn qpc_freq() -> f64 {
        let mut v = 0i64;
        unsafe { QueryPerformanceFrequency(&mut v) };
        (v.max(1)) as f64
    }

    /// Załadowane sterowniki jądra: (adres bazowy, nazwa pliku), rosnąco.
    /// Bez uprawnień administratora Windows podaje same zera.
    pub fn modules() -> Vec<(u64, String)> {
        let mut bases = vec![std::ptr::null_mut::<c_void>(); 2048];
        let mut needed = 0u32;
        let bytes = (bases.len() * size_of::<*mut c_void>()) as u32;
        if unsafe { K32EnumDeviceDrivers(bases.as_mut_ptr(), bytes, &mut needed) } == 0 {
            return Vec::new();
        }
        bases.truncate(needed as usize / size_of::<*mut c_void>());
        let mut out: Vec<(u64, String)> = bases
            .into_iter()
            .filter(|b| !b.is_null())
            .map(|b| {
                let mut name = [0u16; 260];
                let n = unsafe { K32GetDeviceDriverBaseNameW(b, name.as_mut_ptr(), name.len() as u32) } as usize;
                (b as u64, String::from_utf16_lossy(&name[..n.min(name.len())]))
            })
            .collect();
        out.sort();
        out
    }

    #[repr(C)]
    struct Props {
        p: EVENT_TRACE_PROPERTIES,
        name: [u16; 128],
    }

    fn props(s: Session) -> Props {
        let mut props = Props { p: EVENT_TRACE_PROPERTIES::default(), name: [0; 128] };
        props.p.Wnode.BufferSize = size_of::<Props>() as u32;
        props.p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        props.p.Wnode.Guid = s.guid;
        // Znaczniki czasu z licznika wydajności - jak czas wejścia w zdarzeniach.
        props.p.Wnode.ClientContext = 1;
        // Własna sesja systemowa (Windows 8+), obok "NT Kernel Logger" innych programów.
        props.p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;
        props.p.EnableFlags = EVENT_TRACE_FLAG(
            EVENT_TRACE_FLAG_DPC.0 | EVENT_TRACE_FLAG_INTERRUPT.0 | EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS.0,
        );
        props.p.BufferSize = 512;
        props.p.MinimumBuffers = 32;
        props.p.MaximumBuffers = 256;
        props.p.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        props
    }

    struct Context {
        collector: Mutex<Collector>,
        us_per_tick: f64,
    }

    unsafe extern "system" fn callback(record: *mut EVENT_RECORD) {
        let r = unsafe { &*record };
        if r.UserContext.is_null() {
            return;
        }
        let ctx = unsafe { &*(r.UserContext as *const Context) };
        let opcode = r.EventHeader.EventDescriptor.Opcode;
        if r.EventHeader.ProviderId == PAGE_FAULT {
            if opcode == OP_HARD_FAULT {
                if let Ok(mut c) = ctx.collector.lock() {
                    c.hard_faults += 1;
                }
            }
            return;
        }
        if r.EventHeader.ProviderId != PERF_INFO || r.UserData.is_null() || r.UserDataLength < 16 {
            return;
        }
        let kind = match opcode {
            OP_DPC | OP_TIMER_DPC | OP_THREAD_DPC => Kind::Dpc,
            OP_ISR => Kind::Isr,
            _ => return,
        };
        // DPC i ISR: InitialTime (czas wejścia), Routine (adres procedury).
        let data = unsafe { std::slice::from_raw_parts(r.UserData as *const u8, 16) };
        let initial = i64::from_le_bytes(data[..8].try_into().unwrap());
        let routine = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let us = (r.EventHeader.TimeStamp - initial) as f64 * ctx.us_per_tick;
        let cpu = unsafe { r.BufferContext.Anonymous.ProcessorIndex };
        if let Ok(mut c) = ctx.collector.lock() {
            c.add(kind, routine, us);
            if kind == Kind::Dpc {
                c.add_cpu(routine, cpu);
            }
        }
    }

    fn stop_session(handle: CONTROLTRACE_HANDLE, s: Session) {
        let mut p = props(s);
        unsafe {
            let _ = ControlTraceW(handle, &HSTRING::from(s.name), &mut p.p, EVENT_TRACE_CONTROL_STOP);
        }
    }

    /// Działający pomiar.
    pub struct Trace {
        handle: CONTROLTRACE_HANDLE,
        session: Session,
        context: Arc<Context>,
        consumer: Option<std::thread::JoinHandle<()>>,
    }

    impl Trace {
        pub fn start(session: Session) -> Result<Self, String> {
            let context = Arc::new(Context { collector: Mutex::new(Collector::default()), us_per_tick: 1e6 / qpc_freq() });
            let name = HSTRING::from(session.name);
            let mut handle = CONTROLTRACE_HANDLE::default();
            let mut p = props(session);
            let mut code = unsafe { StartTraceW(&mut handle, &name, &mut p.p) }.0;
            if code == ERROR_ALREADY_EXISTS {
                // Sesja po poprzednim uruchomieniu programu (awaria) - zamykamy ją.
                stop_session(CONTROLTRACE_HANDLE::default(), session);
                p = props(session);
                code = unsafe { StartTraceW(&mut handle, &name, &mut p.p) }.0;
            }
            match code {
                0 => {}
                5 => return Err("wymaga uprawnień administratora / needs administrator rights".into()),
                c => return Err(format!("StartTrace: {c}")),
            }
            let ptr = Arc::as_ptr(&context) as usize;
            let keep = context.clone();
            let consumer = crate::spawn("dpc-trace", move || {
                let _keep = keep;
                let mut name: Vec<u16> = session.name.encode_utf16().chain([0]).collect();
                let mut log = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(name.as_mut_ptr()), ..Default::default() };
                log.Anonymous1.ProcessTraceMode =
                    PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
                log.Anonymous2.EventRecordCallback = Some(callback);
                log.Context = ptr as *mut c_void;
                unsafe {
                    let h = OpenTraceW(&mut log);
                    if h.Value != u64::MAX {
                        let _ = ProcessTrace(&[h], None, None);
                        let _ = CloseTrace(h);
                    }
                }
            });
            Ok(Self { handle, session, context, consumer: Some(consumer) })
        }

        /// Kończy pomiar i oddaje zebrane liczniki.
        pub fn finish(mut self) -> Collector {
            self.stop();
            self.context.collector.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default()
        }

        fn stop(&mut self) {
            if let Some(consumer) = self.consumer.take() {
                stop_session(self.handle, self.session);
                let _ = consumer.join();
            }
        }
    }

    impl Drop for Trace {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// Pusta pętla na procesorze `cpu` (pierwsze 64 procesory logiczne), o
    /// najniższym priorytecie, do ustawienia `stop`.
    pub fn spin(cpu: u16, stop: &AtomicBool) {
        const IDLE: i32 = -15;
        if cpu >= 64 {
            return;
        }
        unsafe {
            if SetThreadAffinityMask(GetCurrentThread(), 1usize << cpu) == 0 {
                return;
            }
            SetThreadPriority(GetCurrentThread(), IDLE);
        }
        while !stop.load(Ordering::Relaxed) {
            for _ in 0..4096 {
                std::hint::spin_loop();
            }
        }
    }

    /// Test wybudzania: ten wątek co 1 ms zapisuje czas i ustawia zdarzenie,
    /// a drugi wątek, o najwyższym priorytecie, czeka na nie i mierzy, po jakim
    /// czasie dostał procesor. Zwraca (max, średnia) w µs.
    pub fn wake_test(stop: &AtomicBool) -> (f64, f64) {
        use std::sync::atomic::AtomicI64;
        const TIME_CRITICAL: i32 = 15;
        let us_per_tick = 1e6 / qpc_freq();
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event == 0 {
            return (0.0, 0.0);
        }
        let sent = Arc::new(AtomicI64::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let (s, d) = (sent.clone(), done.clone());
        let waiter = crate::spawn("dpc-wake-wait", move || {
            let (mut max, mut sum, mut n) = (0.0f64, 0.0f64, 0u64);
            unsafe { SetThreadPriority(GetCurrentThread(), TIME_CRITICAL) };
            while !d.load(Ordering::Relaxed) {
                if unsafe { WaitForSingleObject(event, 200) } != 0 {
                    continue;
                }
                let late = ((qpc() - s.load(Ordering::Relaxed)) as f64 * us_per_tick).max(0.0);
                max = max.max(late);
                sum += late;
                n += 1;
            }
            (max, if n > 0 { sum / n as f64 } else { 0.0 })
        });
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            sent.store(qpc(), Ordering::Relaxed);
            unsafe { SetEvent(event) };
        }
        done.store(true, Ordering::Relaxed);
        let result = waiter.join().unwrap_or((0.0, 0.0));
        unsafe { CloseHandle(event) };
        result
    }
}

/// Procesor logiczny obsługujący DPC karty graficznej: krótkie śledzenie
/// (2 s, administrator) we własnej sesji.
#[cfg(windows)]
pub fn detect_gpu_cpu() -> Result<u16, String> {
    let modules = sys::modules();
    let trace = sys::Trace::start(sys::CORE)?;
    std::thread::sleep(Duration::from_secs(2));
    let collector = trace.finish();
    gpu_cpu(&collector, &modules).ok_or_else(|| "brak DPC sterownika karty / no GPU driver DPCs".to_owned())
}

/// Wątek, który nie pozwala zasnąć jednemu procesorowi logicznemu: pusta
/// pętla o najniższym priorytecie, przypięta do tego procesora. Gra i każdy
/// inny wątek mają przed nią pierwszeństwo, a DPC i tak ją przerywają.
#[cfg(windows)]
pub struct KeepAwake {
    stop: Arc<AtomicBool>,
    pub cpu: u16,
}

#[cfg(windows)]
impl KeepAwake {
    pub fn start(cpu: u16) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        crate::spawn("core-awake", move || sys::spin(cpu, &s));
        Self { stop, cpu }
    }
}

#[cfg(windows)]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Stan analizatora dla interfejsu.
#[derive(Clone, Default)]
pub struct State {
    /// Trwający pomiar: początek i zaplanowany czas (s).
    pub running: Option<(Instant, u64)>,
    pub report: Option<Report>,
    /// Poprzedni wynik - do porównania po zmianie ustawień.
    pub previous: Option<Report>,
    pub error: Option<String>,
}

pub struct Dpc {
    shared: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    /// Wybrany czas pomiaru (s).
    pub secs: u64,
    /// Rdzenie przerwań karty graficznej i dźwięku.
    pub irq: IrqPanel,
}

impl Dpc {
    pub fn new() -> Self {
        Self { shared: Arc::default(), stop: Arc::default(), secs: 30, irq: IrqPanel::default() }
    }

    pub fn state(&self) -> State {
        self.shared.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Zaczyna pomiar na `self.secs` sekund (w tle).
    #[cfg(windows)]
    pub fn start(&self, repaint: impl Fn() + Send + 'static) {
        let secs = self.secs;
        {
            let Ok(mut s) = self.shared.lock() else { return };
            if s.running.is_some() {
                return;
            }
            s.running = Some((Instant::now(), secs));
            s.error = None;
        }
        self.stop.store(false, Ordering::Relaxed);
        let (shared, stop) = (self.shared.clone(), self.stop.clone());
        crate::spawn("dpc", move || {
            let result = measure(secs, &stop, &repaint);
            if let Ok(mut s) = shared.lock() {
                s.running = None;
                match result {
                    Ok(report) => {
                        s.previous = s.report.take();
                        s.report = Some(report);
                    }
                    Err(e) => s.error = Some(e),
                }
            }
            repaint();
        });
    }

    #[cfg(not(windows))]
    pub fn start(&self, _: impl Fn() + Send + 'static) {}
}

/// Karta "Rdzenie przerwań" na stronie DPC: urządzenia, zmierzony rdzeń karty
/// graficznej i komunikat ostatniej operacji.
#[derive(Default)]
pub struct IrqPanel {
    pub devices: Vec<crate::irq::Device>,
    scanned: bool,
    /// Rdzeń karty graficznej z pomiaru (pewniejszy niż wpis w rejestrze).
    measured: Arc<Mutex<Option<u16>>>,
    /// Komunikat i czy to błąd.
    pub message: Option<(String, bool)>,
}

impl IrqPanel {
    /// Odczyt urządzeń (z rejestru) i, z uprawnieniami administratora, pomiar
    /// rdzenia karty graficznej w tle (2 s).
    pub fn rescan(&mut self, elevated: bool, repaint: impl Fn() + Send + 'static) {
        self.scanned = true;
        self.devices = crate::irq::scan();
        #[cfg(windows)]
        if elevated {
            let measured = self.measured.clone();
            crate::spawn("irq-gpu-core", move || {
                if let Ok(cpu) = detect_gpu_cpu() {
                    if let Ok(mut m) = measured.lock() {
                        *m = Some(cpu);
                    }
                    repaint();
                }
            });
        }
        #[cfg(not(windows))]
        let _ = (elevated, repaint);
    }

    pub fn ensure_scanned(&mut self, elevated: bool, repaint: impl Fn() + Send + 'static) {
        if !self.scanned {
            self.rescan(elevated, repaint);
        }
    }

    pub fn measured(&self) -> Option<u16> {
        self.measured.lock().ok().and_then(|m| *m)
    }
}

/// Jeden pomiar: śledzenie i test wybudzania przez `secs` sekund albo do Stop.
#[cfg(windows)]
pub fn measure(secs: u64, stop: &Arc<AtomicBool>, tick: &dyn Fn()) -> Result<Report, String> {
    let modules = sys::modules();
    let trace = sys::Trace::start(sys::ANALYZER)?;
    let started = Instant::now();
    let wake_stop = Arc::new(AtomicBool::new(false));
    let ws = wake_stop.clone();
    let wake = crate::spawn("dpc-wake", move || sys::wake_test(&ws));
    while started.elapsed() < Duration::from_secs(secs) && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
        tick();
    }
    wake_stop.store(true, Ordering::Relaxed);
    let wake = wake.join().unwrap_or((0.0, 0.0));
    let elapsed = started.elapsed().as_secs_f64();
    let collector = trace.finish();
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    Ok(report(&collector, &modules, elapsed, cpus, wake))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modules() -> Vec<(u64, String)> {
        vec![(0x1000, "ntoskrnl.exe".into()), (0x5000, "nvlddmkm.sys".into()), (0x9000, "ndis.sys".into())]
    }

    #[test]
    fn address_to_driver() {
        let m = modules();
        assert_eq!(driver_of(&m, 0x1000), "ntoskrnl.exe");
        assert_eq!(driver_of(&m, 0x5FFF), "nvlddmkm.sys");
        assert_eq!(driver_of(&m, 0xFFFF_0000), "ndis.sys");
        assert_eq!(driver_of(&m, 0x10), "0x10");
        assert_eq!(driver_of(&[], 0xABC), "0xABC");
    }

    #[test]
    fn report_merges_by_driver_and_sorts() {
        let mut c = Collector::default();
        c.add(Kind::Dpc, 0x5100, 200.0);
        c.add(Kind::Dpc, 0x5200, 1200.0);
        c.add(Kind::Isr, 0x5300, 50.0);
        c.add(Kind::Dpc, 0x9100, 300.0);
        c.add(Kind::Dpc, 0x9100, 100.0);
        // Błędne czasy są pomijane.
        c.add(Kind::Dpc, 0x9100, -5.0);
        c.add(Kind::Dpc, 0x9100, 5e9);
        c.hard_faults = 3;
        let r = report(&c, &modules(), 10.0, 4, (800.0, 40.0));
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0].driver, "nvlddmkm.sys");
        assert_eq!(r.rows[0].dpc, Times { count: 2, max_us: 1200.0, total_us: 1400.0 });
        assert_eq!(r.rows[0].isr.count, 1);
        assert_eq!(r.rows[1].dpc.avg_us(), 200.0);
        assert_eq!((r.dpc_max_us, r.isr_max_us, r.over_1ms, r.hard_faults), (1200.0, 50.0, 1, 3));
        // 1850 µs z 40 s czasu rdzeni.
        assert!((r.busy_pct - 1850.0 / 40e6 * 100.0).abs() < 1e-9);
        assert_eq!(r.verdict(), Verdict::Bad);
    }

    #[test]
    fn gpu_cpu_is_where_its_driver_runs() {
        let mut c = Collector::default();
        for _ in 0..50 {
            c.add_cpu(0x5100, 1);
        }
        for _ in 0..5 {
            c.add_cpu(0x5200, 9);
        }
        // Inny sterownik, choć częstszy, nie jest brany pod uwagę.
        for _ in 0..900 {
            c.add_cpu(0x9100, 4);
        }
        assert_eq!(gpu_cpu(&c, &modules()), Some(1));
        assert_eq!(gpu_cpu(&Collector::default(), &modules()), None);
    }

    #[test]
    fn verdict_thresholds() {
        let r = |dpc: f64, wake: f64| Report { dpc_max_us: dpc, wake_max_us: wake, ..Default::default() };
        assert_eq!(r(120.0, 300.0).verdict(), Verdict::Good);
        assert_eq!(r(600.0, 300.0).verdict(), Verdict::Fair);
        assert_eq!(r(120.0, 1500.0).verdict(), Verdict::Fair);
        assert_eq!(r(120.0, 2500.0).verdict(), Verdict::Bad);
    }

    /// Pomiar 5 s na tym komputerze (administrator):
    /// `cargo test dpc_live -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn dpc_live() {
        let r = measure(5, &Arc::default(), &|| {}).expect("measure");
        eprintln!(
            "{:.1} s: DPC max {:.0} us, ISR max {:.0} us, >1ms {}, hard faults {}, wake max {:.0} avg {:.0} us, busy {:.3} %",
            r.secs, r.dpc_max_us, r.isr_max_us, r.over_1ms, r.hard_faults, r.wake_max_us, r.wake_avg_us, r.busy_pct
        );
        for row in r.rows.iter().take(12) {
            eprintln!(
                "{:<24} DPC {:>7} max {:>7.1} avg {:>6.1}   ISR {:>7} max {:>7.1}",
                row.driver, row.dpc.count, row.dpc.max_us, row.dpc.avg_us(), row.isr.count, row.isr.max_us
            );
        }
        assert!(r.rows.iter().map(|r| r.dpc.count).sum::<u64>() > 100, "no DPC events");
        assert!(r.rows.iter().any(|r| r.driver.to_lowercase().ends_with(".sys") || r.driver.to_lowercase().ends_with(".exe")));
    }
}

#[cfg(all(test, windows))]
mod awake_tests {
    use super::*;

    /// Wykrycie rdzenia karty i wpływ podtrzymywania na DPC jej sterownika
    /// (administrator): `cargo test core_awake_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn core_awake_live() {
        let cpu = detect_gpu_cpu().expect("gpu cpu");
        eprintln!("GPU DPC core: {cpu}");
        let gpu = |r: &Report| r.rows.iter().find(|x| GPU_DRIVERS.contains(&x.driver.to_lowercase().as_str())).cloned().unwrap_or_default();
        let before = gpu(&measure(15, &Arc::default(), &|| {}).unwrap());
        let keep = KeepAwake::start(cpu);
        std::thread::sleep(Duration::from_secs(1));
        let during = gpu(&measure(15, &Arc::default(), &|| {}).unwrap());
        drop(keep);
        eprintln!("without: {} max {:.0} us avg {:.0} us ({} DPC)", before.driver, before.dpc.max_us, before.dpc.avg_us(), before.dpc.count);
        eprintln!("with:    {} max {:.0} us avg {:.0} us ({} DPC)", during.driver, during.dpc.max_us, during.dpc.avg_us(), during.dpc.count);
        assert!(during.dpc.count > 100);
    }
}
