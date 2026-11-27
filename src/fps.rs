//! Pomiar klatek gry: FPS, 1% low i 0,1% low - bez wstrzykiwania czegokolwiek
//! do gry. Program słucha systemowych zdarzeń Windows (ETW) o wyświetlaniu
//! klatek: DXGI i Direct3D 9 (wywołanie Present w grze) oraz jądra grafiki
//! DxgKrnl (każda klatka, także Vulkan i OpenGL). Czas między kolejnymi
//! zdarzeniami tego samego procesu to czas klatki.
//!
//! 1% low to średni FPS z najwolniejszego 1 % klatek (0,1% low - z 0,1 %).
//! Przerwy dłuższe niż sekunda (ekran ładowania, przełączenie okna) nie są
//! liczone jako klatki. Wymaga uprawnień administratora.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Przerwa dłuższa niż tyle ms to nie klatka (ładowanie, przełączenie okna).
const GAP_MS: f32 = 1000.0;
/// Klatka dłuższa niż tyle ms liczy się jako zacięcie.
const HITCH_MS: f32 = 50.0;
/// Plik z historią pomiarów obok ustawień programu.
const FILE: &str = "tunecenter-fps.json";

/// Statystyki z czasów klatek (ms).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub frames: u64,
    pub avg: f32,
    /// Średni FPS z najwolniejszego 1 % i 0,1 % klatek.
    pub low1: f32,
    pub low01: f32,
    pub min: f32,
    pub max: f32,
    /// Klatki dłuższe niż 50 ms.
    pub hitches: u32,
}

/// Średni FPS z najwolniejszej części klatek (`share` = 0,01 dla 1% low).
/// `sorted` - czasy klatek rosnąco.
fn low(sorted: &[f32], share: f64) -> f32 {
    let n = ((sorted.len() as f64 * share).ceil() as usize).clamp(1, sorted.len());
    let worst = &sorted[sorted.len() - n..];
    let mean = worst.iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    if mean > 0.0 {
        (1000.0 / mean) as f32
    } else {
        0.0
    }
}

pub fn stats(frame_ms: &[f32]) -> Stats {
    let mut sorted: Vec<f32> = frame_ms.iter().copied().filter(|v| *v > 0.0 && *v < GAP_MS).collect();
    if sorted.is_empty() {
        return Stats::default();
    }
    sorted.sort_by(f32::total_cmp);
    let sum: f64 = sorted.iter().map(|v| *v as f64).sum();
    Stats {
        frames: sorted.len() as u64,
        avg: (1000.0 * sorted.len() as f64 / sum) as f32,
        low1: low(&sorted, 0.01),
        low01: low(&sorted, 0.001),
        min: 1000.0 / sorted[sorted.len() - 1],
        max: 1000.0 / sorted[0],
        hitches: sorted.iter().filter(|v| **v >= HITCH_MS).count() as u32,
    }
}

/// Skąd pochodzi zdarzenie klatki.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Wywołanie Present w grze (DXGI albo Direct3D 9).
    Api,
    /// Jądro grafiki - każda klatka niezależnie od API.
    Kernel,
}

/// Klatki jednego procesu z obu źródeł.
#[derive(Default)]
struct Proc {
    api: Vec<f32>,
    kernel: Vec<f32>,
    last_api: Option<i64>,
    last_kernel: Option<i64>,
}

impl Proc {
    /// Źródło do statystyk: wywołania Present gry, a gdy gra ich nie zgłasza
    /// (Vulkan, OpenGL) - zdarzenia jądra.
    fn frames(&self) -> &[f32] {
        // Jądro potrafi zgłosić więcej niż jedno zdarzenie na klatkę, więc
        // wywołania gry wygrywają, gdy jest ich co najmniej ćwierć tego.
        if self.api.len() * 4 >= self.kernel.len() && !self.api.is_empty() {
            &self.api
        } else {
            &self.kernel
        }
    }
}

/// Zbiera klatki wszystkich procesów w trakcie pomiaru.
#[derive(Default)]
pub struct Collector {
    procs: HashMap<u32, Proc>,
    ms_per_tick: f64,
}

impl Collector {
    pub fn new(ms_per_tick: f64) -> Self {
        Self { procs: HashMap::new(), ms_per_tick }
    }

    pub fn add(&mut self, pid: u32, source: Source, timestamp: i64) {
        let p = self.procs.entry(pid).or_default();
        let (last, list) = match source {
            Source::Api => (&mut p.last_api, &mut p.api),
            Source::Kernel => (&mut p.last_kernel, &mut p.kernel),
        };
        if let Some(prev) = *last {
            let ms = ((timestamp - prev) as f64 * self.ms_per_tick) as f32;
            // Do 8 godzin przy 300 klatkach/s; dalej tylko najnowsze.
            if ms > 0.0 && list.len() < 9_000_000 {
                list.push(ms);
            }
        }
        *last = Some(timestamp);
    }

    /// Liczba klatek każdego procesu.
    pub fn counts(&self) -> Vec<(u32, usize)> {
        self.procs.iter().map(|(pid, p)| (*pid, p.frames().len())).collect()
    }

    pub fn frames(&self, pid: u32) -> &[f32] {
        self.procs.get(&pid).map(|p| p.frames()).unwrap_or(&[])
    }

    /// FPS z ostatnich `ms` milisekund procesu (odczyt na żywo).
    pub fn recent_fps(&self, pid: u32, ms: f32) -> f32 {
        let frames = self.frames(pid);
        let (mut sum, mut n) = (0.0f32, 0u32);
        for v in frames.iter().rev() {
            if *v >= GAP_MS || sum + v > ms {
                break;
            }
            sum += v;
            n += 1;
        }
        if sum > 0.0 {
            1000.0 * n as f32 / sum
        } else {
            0.0
        }
    }
}

/// Procesy, które nie są grą: kompozytor pulpitu, powłoka i sam program.
pub fn is_system(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "dwm.exe" | "explorer.exe" | "tunecenter.exe" | "searchhost.exe" | "startmenuexperiencehost.exe" | "shellexperiencehost.exe"
            | "textinputhost.exe" | "applicationframehost.exe" | "systemsettings.exe" | "lockapp.exe" | "csrss.exe"
    )
}

/// Proces do pomiaru: działająca gra z wykrytych folderów (`game_pids`), a bez
/// niej - niesystemowy proces z największą liczbą klatek.
pub fn pick(counts: &[(u32, usize)], names: &HashMap<u32, String>, game_pids: &[u32]) -> Option<u32> {
    let best = |list: &mut dyn Iterator<Item = &(u32, usize)>| list.filter(|(_, n)| *n > 0).max_by_key(|(pid, n)| (*n, u32::MAX - *pid)).map(|(pid, _)| *pid);
    best(&mut counts.iter().filter(|(pid, _)| game_pids.contains(pid)))
        .or_else(|| best(&mut counts.iter().filter(|(pid, _)| names.get(pid).is_some_and(|n| !is_system(n)))))
}

/// Jeden zapisany pomiar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// Data i godzina zakończenia (tekst lokalny) i znacznik do sortowania.
    pub when: String,
    pub unix: u64,
    /// Nazwa gry (z listy gier) albo nazwa pliku procesu.
    pub game: String,
    pub exe: String,
    /// Czas pomiaru w sekundach (od Start do Stop).
    pub secs: f32,
    pub stats: Stats,
    /// Włączone w trakcie pomiaru opcje programu - do porównań.
    #[serde(default)]
    pub setup: String,
}

/// Historia pomiarów (najnowsze na początku).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History {
    pub runs: Vec<Run>,
}

impl History {
    fn path() -> Option<std::path::PathBuf> {
        Some(std::env::current_exe().ok()?.parent()?.join(FILE))
    }

    pub fn load() -> Self {
        Self::path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let (Some(p), Ok(text)) = (Self::path(), serde_json::to_string_pretty(self)) {
            let _ = std::fs::write(p, text);
        }
    }

    /// Dopisuje pomiar na początek; zostaje 200 najnowszych.
    pub fn push(&mut self, run: Run) {
        self.runs.insert(0, run);
        self.runs.truncate(200);
        self.save();
    }
}

#[cfg(windows)]
mod sys {
    use super::{Collector, Source};
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};
    use windows::core::{GUID, HSTRING, PWSTR};
    use windows::Win32::System::Diagnostics::Etw::{
        CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
        EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
        EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESS_TRACE_MODE_EVENT_RECORD,
        PROCESS_TRACE_MODE_RAW_TIMESTAMP, PROCESS_TRACE_MODE_REAL_TIME, WNODE_FLAG_TRACED_GUID,
    };

    /// Microsoft-Windows-DXGI: Present (start) = zdarzenie 42.
    const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
    const DXGI_PRESENT: u16 = 42;
    /// Microsoft-Windows-D3D9: Present (start) = zdarzenie 1.
    const D3D9: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
    const D3D9_PRESENT: u16 = 1;
    /// Microsoft-Windows-DxgKrnl: Present (info) = zdarzenie 184; słowa
    /// kluczowe "Base" i "Present", żeby nie zbierać reszty zdarzeń jądra.
    const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);
    const DXGKRNL_PRESENT: u16 = 184;
    const DXGKRNL_KEYWORDS: u64 = 0x0800_0000 | 0x1;
    const NAME: &str = "TuneCenterFps";
    const SESSION: GUID = GUID::from_u128(0x7D3C1B5A_41E2_4C8E_9A57_54756E654360);
    const ERROR_ALREADY_EXISTS: u32 = 183;

    #[link(name = "kernel32")]
    extern "system" {
        fn QueryPerformanceFrequency(value: *mut i64) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Process32FirstW(snap: isize, entry: *mut ProcEntry) -> i32;
        fn Process32NextW(snap: isize, entry: *mut ProcEntry) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    #[repr(C)]
    struct ProcEntry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe: [u16; 260],
    }

    /// Nazwy plików działających procesów.
    pub fn process_names() -> HashMap<u32, String> {
        let mut out = HashMap::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(2, 0);
            if snap == -1 {
                return out;
            }
            let mut e: ProcEntry = std::mem::zeroed();
            e.size = size_of::<ProcEntry>() as u32;
            let mut ok = Process32FirstW(snap, &mut e);
            while ok != 0 {
                let n = e.exe.iter().position(|&c| c == 0).unwrap_or(260);
                out.insert(e.pid, String::from_utf16_lossy(&e.exe[..n]));
                ok = Process32NextW(snap, &mut e);
            }
            CloseHandle(snap);
        }
        out
    }

    #[repr(C)]
    struct Props {
        p: EVENT_TRACE_PROPERTIES,
        name: [u16; 128],
    }

    fn props() -> Props {
        let mut props = Props { p: EVENT_TRACE_PROPERTIES::default(), name: [0; 128] };
        props.p.Wnode.BufferSize = size_of::<Props>() as u32;
        props.p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        props.p.Wnode.Guid = SESSION;
        // Znaczniki czasu z licznika wydajności.
        props.p.Wnode.ClientContext = 1;
        props.p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        props.p.BufferSize = 256;
        props.p.MinimumBuffers = 32;
        props.p.MaximumBuffers = 256;
        props.p.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        props
    }

    unsafe extern "system" fn callback(record: *mut EVENT_RECORD) {
        let r = unsafe { &*record };
        if r.UserContext.is_null() {
            return;
        }
        let h = &r.EventHeader;
        let id = h.EventDescriptor.Id;
        let source = if (h.ProviderId == DXGI && id == DXGI_PRESENT) || (h.ProviderId == D3D9 && id == D3D9_PRESENT) {
            Source::Api
        } else if h.ProviderId == DXGKRNL && id == DXGKRNL_PRESENT {
            Source::Kernel
        } else {
            return;
        };
        let collector = unsafe { &*(r.UserContext as *const Mutex<Collector>) };
        if let Ok(mut c) = collector.lock() {
            c.add(h.ProcessId, source, h.TimeStamp);
        }
    }

    fn stop_session(handle: CONTROLTRACE_HANDLE) {
        let mut p = props();
        unsafe {
            let _ = ControlTraceW(handle, &HSTRING::from(NAME), &mut p.p, EVENT_TRACE_CONTROL_STOP);
        }
    }

    /// Zatrzymuje sesję programu, jeśli została (zamknięcie w trakcie pomiaru, awaria).
    pub fn stop_stale() {
        stop_session(CONTROLTRACE_HANDLE::default());
    }

    /// Działający pomiar klatek.
    pub struct Trace {
        handle: CONTROLTRACE_HANDLE,
        pub collector: Arc<Mutex<Collector>>,
        consumer: Option<std::thread::JoinHandle<()>>,
    }

    // Uchwyt sesji to liczba, a zbieracz jest za muteksem.
    unsafe impl Send for Trace {}

    impl Trace {
        pub fn start() -> Result<Self, String> {
            let mut freq = 0i64;
            unsafe { QueryPerformanceFrequency(&mut freq) };
            let collector = Arc::new(Mutex::new(Collector::new(1000.0 / freq.max(1) as f64)));
            let name = HSTRING::from(NAME);
            let mut handle = CONTROLTRACE_HANDLE::default();
            let mut p = props();
            let mut code = unsafe { StartTraceW(&mut handle, &name, &mut p.p) }.0;
            if code == ERROR_ALREADY_EXISTS {
                // Sesja po poprzednim uruchomieniu programu (awaria) - zamykamy ją.
                stop_stale();
                p = props();
                code = unsafe { StartTraceW(&mut handle, &name, &mut p.p) }.0;
            }
            match code {
                0 => {}
                5 => return Err(crate::i18n::tr("wymaga uprawnień administratora", "needs administrator rights").to_owned()),
                c => return Err(format!("StartTrace: {c}")),
            }
            for (provider, keywords) in [(DXGI, 0u64), (D3D9, 0), (DXGKRNL, DXGKRNL_KEYWORDS)] {
                let code =
                    unsafe { EnableTraceEx2(handle, &provider, EVENT_CONTROL_CODE_ENABLE_PROVIDER.0, 4, keywords, 0, 0, None) }.0;
                if code != 0 {
                    stop_session(handle);
                    return Err(format!("EnableTrace: {code}"));
                }
            }
            let context = Arc::as_ptr(&collector) as usize;
            let keep = collector.clone();
            let consumer = crate::spawn("fps-trace", move || {
                let _keep = keep;
                let mut name: Vec<u16> = NAME.encode_utf16().chain([0]).collect();
                let mut log = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(name.as_mut_ptr()), ..Default::default() };
                log.Anonymous1.ProcessTraceMode =
                    PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
                log.Anonymous2.EventRecordCallback = Some(callback);
                log.Context = context as *mut c_void;
                unsafe {
                    let h = OpenTraceW(&mut log);
                    if h.Value != u64::MAX {
                        let _ = ProcessTrace(&[h], None, None);
                        let _ = CloseTrace(h);
                    }
                }
            });
            Ok(Self { handle, collector, consumer: Some(consumer) })
        }

        fn stop(&mut self) {
            if let Some(consumer) = self.consumer.take() {
                stop_session(self.handle);
                let _ = consumer.join();
            }
        }
    }

    impl Drop for Trace {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(windows)]
pub use sys::{process_names, stop_stale, Trace};

/// Stan pomiaru na żywo.
#[derive(Clone, Debug, Default)]
pub struct Live {
    /// Proces, który teraz jest mierzony, i jego nazwa.
    pub pid: Option<u32>,
    pub name: String,
    pub exe: String,
    pub fps: f32,
    pub stats: Stats,
    pub secs: f32,
}

/// Pomiar klatek: start, stan na żywo, stop z zapisem do historii.
pub struct Meter {
    #[cfg(windows)]
    trace: Option<Trace>,
    started: Option<Instant>,
    /// Ostatni odczyt na żywo i kiedy go zrobiono.
    live: Live,
    ticked: Option<Instant>,
    /// Foldery gier (małymi literami) z nazwami - wczytywane w tle.
    games: Arc<Mutex<Vec<(String, String)>>>,
    pub history: History,
    pub error: Option<String>,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            trace: None,
            started: None,
            live: Live::default(),
            ticked: None,
            games: Arc::default(),
            history: History::load(),
            error: None,
        }
    }
}

/// Identyfikatory procesów uruchomionych z folderów gier i nazwa gry.
#[cfg(windows)]
fn game_pids(games: &[(String, String)]) -> (Vec<u32>, Option<String>) {
    let mut pids = Vec::new();
    let mut name = None;
    for (pid, path) in crate::game_watch::process_list() {
        if let Some((_, game)) = games.iter().find(|(dir, _)| crate::game_watch::matches(dir, &path)) {
            pids.push(pid);
            name.get_or_insert_with(|| game.clone());
        }
    }
    (pids, name)
}

#[cfg(windows)]
impl Meter {
    pub fn running(&self) -> bool {
        self.trace.is_some()
    }

    pub fn start(&mut self) {
        if self.trace.is_some() {
            return;
        }
        match Trace::start() {
            Ok(t) => {
                self.trace = Some(t);
                self.started = Some(Instant::now());
                self.error = None;
                self.live = Live::default();
                self.ticked = None;
                // Lista gier do rozpoznania procesu - w tle, żeby nie blokować okna.
                let games = self.games.clone();
                crate::spawn("fps-games", move || {
                    let list: Vec<(String, String)> = crate::shaders::games::find_all()
                        .into_iter()
                        .map(|g| (g.dir.to_string_lossy().to_lowercase(), g.name))
                        .collect();
                    if let Ok(mut g) = games.lock() {
                        *g = list;
                    }
                });
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Odczyt na żywo, odświeżany najwyżej dwa razy na sekundę: wybór procesu
    /// (działająca gra albo program z największą liczbą klatek) i statystyki.
    pub fn live(&mut self) -> Live {
        if self.trace.is_some() && self.ticked.is_none_or(|t| t.elapsed().as_millis() >= 500) {
            self.refresh();
        }
        self.live.clone()
    }

    fn refresh(&mut self) {
        let (Some(trace), Some(started)) = (&self.trace, self.started) else { return };
        self.ticked = Some(Instant::now());
        let names = process_names();
        let games = self.games.lock().map(|g| g.clone()).unwrap_or_default();
        let (pids, game) = game_pids(&games);
        let mut live = Live { secs: started.elapsed().as_secs_f32(), ..Default::default() };
        if let Ok(c) = trace.collector.lock() {
            if let Some(pid) = pick(&c.counts(), &names, &pids) {
                live.pid = Some(pid);
                live.exe = names.get(&pid).cloned().unwrap_or_default();
                live.name = match game {
                    Some(g) if pids.contains(&pid) => g,
                    _ => live.exe.clone(),
                };
                live.fps = c.recent_fps(pid, 1000.0);
                live.stats = stats(c.frames(pid));
            }
        }
        self.live = live;
    }

    /// Kończy pomiar i zapisuje go w historii (gdy zebrano klatki).
    /// `setup` - opis włączonych opcji programu, do porównań.
    pub fn stop(&mut self, setup: &str) -> Option<Run> {
        if self.trace.is_none() {
            return None;
        }
        self.refresh();
        self.trace = None;
        self.started = None;
        let live = self.live.clone();
        if live.pid.is_none() || live.stats.frames < 10 {
            self.error = Some(
                crate::i18n::tr("Nie zarejestrowano klatek żadnego programu.", "No frames from any program were recorded.").to_owned(),
            );
            return None;
        }
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let run = Run { when: local_time(), unix, game: live.name, exe: live.exe, secs: live.secs, stats: live.stats, setup: setup.to_owned() };
        self.history.push(run.clone());
        Some(run)
    }
}

#[cfg(windows)]
impl Drop for Meter {
    fn drop(&mut self) {
        // Zamknięcie programu w trakcie pomiaru: sesja śledzenia nie może zostać.
        self.trace = None;
        stop_stale();
    }
}

/// Lokalna data i godzina "RRRR-MM-DD GG:MM".
#[cfg(windows)]
fn local_time() -> String {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        weekday: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_math() {
        // 990 klatek po 10 ms i 10 po 40 ms: 1% low to te 10 najwolniejszych.
        let mut f = vec![10.0f32; 990];
        f.extend([40.0f32; 10]);
        let s = stats(&f);
        assert_eq!(s.frames, 1000);
        assert!((s.avg - 1000.0 * 1000.0 / (9900.0 + 400.0)).abs() < 0.01);
        assert!((s.low1 - 25.0).abs() < 0.01);
        // 0,1 % z 1000 klatek to jedna klatka.
        assert!((s.low01 - 25.0).abs() < 0.01);
        assert!((s.min - 25.0).abs() < 0.01 && (s.max - 100.0).abs() < 0.01);
        assert_eq!(s.hitches, 0);
        // Przerwa (ładowanie) nie jest klatką, zacięcie 60 ms jest.
        let s = stats(&[10.0, 10.0, 3000.0, 60.0]);
        assert_eq!((s.frames, s.hitches), (3, 1));
        assert_eq!(stats(&[]), Stats::default());
        assert_eq!(stats(&[5000.0]).frames, 0);
    }

    #[test]
    fn collector_prefers_game_present_calls() {
        let mut c = Collector::new(1.0);
        for t in 0..100 {
            c.add(7, Source::Api, t * 10);
            // Jądro zgłasza te same klatki i dodatkowe zdarzenia.
            c.add(7, Source::Kernel, t * 10);
            c.add(7, Source::Kernel, t * 10 + 1);
            // Gra bez wywołań DXGI (Vulkan): tylko jądro.
            c.add(9, Source::Kernel, t * 5);
        }
        assert_eq!(c.frames(7).len(), 99);
        assert!(c.frames(7).iter().all(|v| (*v - 10.0).abs() < 0.001));
        assert_eq!(c.frames(9).len(), 99);
        assert!((c.recent_fps(7, 500.0) - 100.0).abs() < 0.5);
        assert!(c.frames(1).is_empty());
        assert_eq!(c.recent_fps(1, 500.0), 0.0);
    }

    #[test]
    fn picks_the_game() {
        let names: HashMap<u32, String> =
            [(1, "dwm.exe"), (2, "game.exe"), (3, "browser.exe"), (4, "TuneCenter.exe")].into_iter().map(|(p, n)| (p, n.to_string())).collect();
        let counts = [(1, 9000), (2, 500), (3, 800), (4, 700)];
        // Wykryta gra wygrywa, choć inny program ma więcej klatek.
        assert_eq!(pick(&counts, &names, &[2]), Some(2));
        // Bez wykrytej gry: najwięcej klatek poza kompozytorem i samym programem.
        assert_eq!(pick(&counts, &names, &[]), Some(3));
        assert_eq!(pick(&[(1, 100)], &names, &[]), None);
        assert_eq!(pick(&[(2, 0)], &names, &[2]), None);
    }

    /// Klatki programów na tym komputerze przez 5 s (administrator):
    /// `cargo test fps_live -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn fps_live() {
        let trace = Trace::start().expect("trace");
        std::thread::sleep(std::time::Duration::from_secs(5));
        let names = process_names();
        let c = trace.collector.lock().unwrap();
        let mut counts = c.counts();
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        for (pid, n) in counts.iter().take(10) {
            let p = &c.procs[pid];
            let s = stats(c.frames(*pid));
            eprintln!(
                "{:<28} pid {:>6}: api {:>5} kernel {:>5} -> {} frames, avg {:.1} fps, 1% low {:.1}",
                names.get(pid).cloned().unwrap_or_default(), pid, p.api.len(), p.kernel.len(), n, s.avg, s.low1
            );
        }
        eprintln!("picked: {:?}", pick(&counts, &names, &[]).and_then(|p| names.get(&p).cloned()));
        assert!(!counts.is_empty(), "no present events");
    }
}

#[cfg(all(test, windows))]
mod meter_live {
    use super::*;

    /// Cały pomiar na tym komputerze (administrator): start, odczyt na żywo,
    /// stop z zapisem. `cargo test fps_meter_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn fps_meter_live() {
        let mut m = Meter::default();
        let before = m.history.runs.len();
        m.start();
        assert!(m.running(), "start: {:?}", m.error);
        std::thread::sleep(std::time::Duration::from_secs(8));
        let live = m.live();
        eprintln!("live: {} ({}) {:.1} fps, {:?}, {:.1} s", live.name, live.exe, live.fps, live.stats, live.secs);
        let run = m.stop("test");
        eprintln!("run: {run:?}; error: {:?}", m.error);
        assert!(!m.running());
        if let Some(r) = run {
            assert_eq!(m.history.runs.len(), before + 1);
            assert_eq!(m.history.runs[0], r);
            assert!(r.stats.frames >= 10 && r.stats.low1 <= r.stats.avg + 0.01 && r.stats.low01 <= r.stats.low1 + 0.01);
            // Test nie zostawia wpisu w historii.
            m.history.runs.remove(0);
            m.history.save();
        }
    }
}
