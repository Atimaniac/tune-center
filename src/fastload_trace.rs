//! Fast Load: profil odczytów gry. Program zapisuje, które fragmenty plików
//! gra faktycznie czyta (systemowe śledzenie ETW, dostawca
//! Microsoft-Windows-Kernel-File - tylko obserwacja, nic nie jest zmieniane
//! ani wstrzykiwane do gry), i następnym razem wczytuje do pamięci tylko je.
//!
//! Śledzenie wymaga uprawnień administratora. Zapisywane są wyłącznie
//! odczyty plików z folderu gry: ścieżka względna i numery fragmentów po
//! 1 MiB, z licznikiem sesji, w których fragment był czytany.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Rozmiar fragmentu pliku w profilu.
pub const CHUNK: u64 = 1 << 20;
/// Plik z profilami obok ustawień programu.
const FILE: &str = "tunecenter-fastload.json";

/// Odczyty jednej gry: plik (ścieżka względna, małe litery) -> numer
/// fragmentu -> liczba sesji, w których był czytany.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub sessions: u32,
    pub files: BTreeMap<String, BTreeMap<u32, u32>>,
}

/// Fragment pliku do wczytania.
#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub path: PathBuf,
    pub offset: u64,
    pub len: u64,
}

/// Odczyty jednej sesji: plik -> fragmenty.
pub type Session = BTreeMap<String, BTreeSet<u32>>;

impl Profile {
    /// Dolicza sesję; pusta sesja (gra nic nie czytała) nie jest liczona.
    pub fn merge(&mut self, session: &Session) {
        if session.values().all(|c| c.is_empty()) {
            return;
        }
        self.sessions += 1;
        for (file, chunks) in session {
            let f = self.files.entry(file.clone()).or_default();
            for c in chunks {
                *f.entry(*c).or_insert(0) += 1;
            }
        }
    }

    /// Łączny rozmiar fragmentów (górne oszacowanie - ostatni fragment pliku
    /// bywa krótszy).
    pub fn bytes(&self) -> u64 {
        self.files.values().map(|c| c.len() as u64).sum::<u64>() * CHUNK
    }

    /// Fragmenty do wczytania z folderu `dir`, najwyżej `limit` bajtów. Gdy
    /// profil się nie mieści, zostają fragmenty czytane w największej liczbie
    /// sesji. `size_of` podaje rozmiar pliku (`None` - pliku już nie ma).
    pub fn segments(&self, dir: &Path, limit: u64, size_of: &dyn Fn(&Path) -> Option<u64>) -> Vec<Seg> {
        // (plik, fragment, licznik) - przy braku miejsca odpadają najrzadsze.
        let mut all: Vec<(&str, u32, u32)> = self
            .files
            .iter()
            .flat_map(|(f, chunks)| chunks.iter().map(move |(c, n)| (f.as_str(), *c, *n)))
            .collect();
        let max = (limit / CHUNK) as usize;
        if all.len() > max {
            all.sort_by(|a, b| b.2.cmp(&a.2));
            all.truncate(max);
            all.sort();
        }
        let mut out: Vec<Seg> = Vec::new();
        let mut current: Option<(&str, PathBuf, u64)> = None;
        for (file, chunk, _) in all {
            if current.as_ref().map(|c| c.0) != Some(file) {
                let path = dir.join(file);
                current = size_of(&path).map(|size| (file, path, size));
                if current.is_none() {
                    // Pliku już nie ma (aktualizacja gry) - pomijamy jego fragmenty.
                    current = Some((file, PathBuf::new(), 0));
                }
            }
            let Some((_, path, size)) = &current else { continue };
            let offset = chunk as u64 * CHUNK;
            if offset >= *size {
                continue;
            }
            let len = CHUNK.min(size - offset);
            match out.last_mut() {
                Some(last) if &last.path == path && last.offset + last.len == offset => last.len += len,
                _ => out.push(Seg { path: path.clone(), offset, len }),
            }
        }
        out
    }
}

/// Fragmenty obejmujące odczyt `size` bajtów od `offset`.
pub fn chunks_of(offset: u64, size: u64) -> std::ops::RangeInclusive<u32> {
    let first = (offset / CHUNK) as u32;
    let last = ((offset + size.max(1) - 1) / CHUNK) as u32;
    first..=last
}

/// Profile wszystkich gier (identyfikator sklepu -> profil).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    pub games: BTreeMap<String, Profile>,
}

impl Store {
    fn path() -> Option<PathBuf> {
        Some(std::env::current_exe().ok()?.parent()?.join(FILE))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let (Some(p), Ok(text)) = (Self::path(), serde_json::to_string(self)) {
            let _ = std::fs::write(p, text);
        }
    }
}

/// Zbiera odczyty z folderu gry w trakcie śledzenia.
pub struct Recorder {
    /// Folder gry w postaci urządzenia, małymi literami, z "\" na końcu
    /// (np. "\device\harddiskvolume5\games\x\").
    root: String,
    /// Otwarte pliki gry: obiekt pliku w jądrze -> ścieżka względna.
    objects: HashMap<u64, String>,
    pub session: Session,
    own_pid: u32,
}

impl Recorder {
    pub fn new(root: &str, own_pid: u32) -> Self {
        let mut root = root.to_lowercase();
        if !root.ends_with('\\') {
            root.push('\\');
        }
        Self { root, objects: HashMap::new(), session: Session::new(), own_pid }
    }

    /// Otwarcie pliku: zapamiętujemy obiekty plików z folderu gry. Własne
    /// odczyty programu (wczytywanie do pamięci) są pomijane.
    pub fn on_create(&mut self, pid: u32, object: u64, name: &str) {
        let name = name.to_lowercase();
        match name.strip_prefix(&self.root) {
            Some(rel) if pid != self.own_pid && !rel.is_empty() => {
                self.objects.insert(object, rel.to_owned());
            }
            // Adres obiektu mógł zostać użyty ponownie dla innego pliku.
            _ => {
                self.objects.remove(&object);
            }
        }
    }

    pub fn on_read(&mut self, object: u64, offset: u64, size: u64) {
        if let Some(rel) = self.objects.get(&object) {
            self.session.entry(rel.clone()).or_default().extend(chunks_of(offset, size));
        }
    }

    /// Liczba zapisanych fragmentów (postęp w interfejsie).
    pub fn chunks(&self) -> usize {
        self.session.values().map(|c| c.len()).sum()
    }
}

#[cfg(windows)]
mod etw {
    use super::Recorder;
    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use windows::core::{GUID, HSTRING, PWSTR};
    use windows::Win32::Storage::FileSystem::QueryDosDeviceW;
    use windows::Win32::System::Diagnostics::Etw::{
        CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
        EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
        EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESS_TRACE_MODE_EVENT_RECORD,
        PROCESS_TRACE_MODE_REAL_TIME, WNODE_FLAG_TRACED_GUID,
    };

    /// Microsoft-Windows-Kernel-File.
    const KERNEL_FILE: GUID = GUID::from_u128(0xEDD08927_9CC4_4E65_B970_C2560FB5C289);
    /// Słowa kluczowe dostawcy: otwarcia plików i odczyty.
    const KEYWORD_CREATE: u64 = 0x80;
    const KEYWORD_READ: u64 = 0x100;
    const EVENT_CREATE: u16 = 12;
    const EVENT_READ: u16 = 15;
    const NAME: &str = "TuneCenterFastLoad";
    const ERROR_ALREADY_EXISTS: u32 = 183;

    /// Bufor EVENT_TRACE_PROPERTIES z miejscem na nazwę sesji.
    #[repr(C)]
    struct Props {
        p: EVENT_TRACE_PROPERTIES,
        name: [u16; 128],
    }

    fn props() -> Props {
        let mut props = Props { p: EVENT_TRACE_PROPERTIES::default(), name: [0; 128] };
        props.p.Wnode.BufferSize = size_of::<Props>() as u32;
        props.p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        // Znaczniki czasu z licznika wydajności.
        props.p.Wnode.ClientContext = 1;
        props.p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        props.p.BufferSize = 256;
        props.p.MinimumBuffers = 16;
        props.p.MaximumBuffers = 128;
        props.p.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        props
    }

    /// Folder w postaci urządzenia: "D:\Gry\X" -> "\Device\HarddiskVolume5\Gry\X".
    pub fn device_path(dir: &Path) -> Option<String> {
        let letter = crate::fastload::drive_letter(dir)?;
        let text = dir.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        let mut buf = [0u16; 512];
        let n = unsafe { QueryDosDeviceW(&HSTRING::from(letter.as_str()), Some(&mut buf)) } as usize;
        if n == 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(n);
        Some(format!("{}{}", String::from_utf16_lossy(&buf[..end]), &text[2..]))
    }

    fn u64_at(d: &[u8], at: usize) -> Option<u64> {
        Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
    }

    fn u32_at(d: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
    }

    /// Nazwa pliku ze zdarzenia Create: napis UTF-16 zakończony zerem po
    /// stałych polach (Irp, FileObject, wątek i opcje - 32 bajty).
    pub fn create_name(d: &[u8]) -> Option<String> {
        let units: Vec<u16> =
            d.get(32..)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
        Some(String::from_utf16_lossy(&units))
    }

    unsafe extern "system" fn callback(record: *mut EVENT_RECORD) {
        let r = unsafe { &*record };
        if r.EventHeader.ProviderId != KERNEL_FILE || r.UserContext.is_null() || r.UserData.is_null() {
            return;
        }
        let data = unsafe { std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize) };
        let rec = unsafe { &*(r.UserContext as *const Mutex<Recorder>) };
        match r.EventHeader.EventDescriptor.Id {
            // Create: Irp, FileObject, IssuingThreadId, CreateOptions,
            // CreateAttributes, ShareAccess, FileName.
            EVENT_CREATE => {
                if let (Some(object), Some(name)) = (u64_at(data, 8), create_name(data)) {
                    if let Ok(mut rec) = rec.lock() {
                        rec.on_create(r.EventHeader.ProcessId, object, &name);
                    }
                }
            }
            // Read: ByteOffset, Irp, FileObject, FileKey, IssuingThreadId, IOSize, IOFlags.
            EVENT_READ => {
                if let (Some(offset), Some(object), Some(size)) = (u64_at(data, 0), u64_at(data, 16), u32_at(data, 36)) {
                    if let Ok(mut rec) = rec.lock() {
                        rec.on_read(object, offset, size as u64);
                    }
                }
            }
            _ => {}
        }
    }

    /// Działające śledzenie odczytów jednej gry.
    pub struct Trace {
        handle: CONTROLTRACE_HANDLE,
        pub recorder: Arc<Mutex<Recorder>>,
        consumer: Option<std::thread::JoinHandle<()>>,
    }

    /// Zatrzymuje sesję programu, jeśli została (zamknięcie w trakcie nauki, awaria).
    pub fn stop_stale() {
        stop_session(CONTROLTRACE_HANDLE::default());
    }

    fn stop_session(handle: CONTROLTRACE_HANDLE) {
        let mut p = props();
        unsafe {
            let _ = ControlTraceW(handle, &HSTRING::from(NAME), &mut p.p, EVENT_TRACE_CONTROL_STOP);
        }
    }

    impl Trace {
        /// Zaczyna śledzić odczyty plików z folderu `dir`.
        pub fn start(dir: &Path) -> Result<Self, String> {
            let root = device_path(dir).ok_or("nieznany wolumin / unknown volume")?;
            let recorder = Arc::new(Mutex::new(Recorder::new(&root, std::process::id())));
            let name = HSTRING::from(NAME);
            let mut handle = CONTROLTRACE_HANDLE::default();
            let mut p = props();
            let mut code = unsafe { StartTraceW(&mut handle, &name, &mut p.p) }.0;
            if code == ERROR_ALREADY_EXISTS {
                // Sesja po poprzednim uruchomieniu programu (awaria) - zamykamy ją.
                stop_session(CONTROLTRACE_HANDLE::default());
                p = props();
                code = unsafe { StartTraceW(&mut handle, &name, &mut p.p) }.0;
            }
            match code {
                0 => {}
                5 => return Err("wymaga uprawnień administratora / needs administrator rights".into()),
                c => return Err(format!("StartTrace: {c}")),
            }
            let code = unsafe {
                EnableTraceEx2(
                    handle,
                    &KERNEL_FILE,
                    EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                    4,
                    KEYWORD_CREATE | KEYWORD_READ,
                    0,
                    0,
                    None,
                )
            }
            .0;
            if code != 0 {
                stop_session(handle);
                return Err(format!("EnableTrace: {code}"));
            }
            // Wątek odbierający zdarzenia; kończy się po zatrzymaniu sesji.
            let context = Arc::as_ptr(&recorder) as usize;
            let keep = recorder.clone();
            let consumer = crate::spawn("fastload-trace", move || {
                let _keep = keep;
                let mut name: Vec<u16> = NAME.encode_utf16().chain([0]).collect();
                let mut log = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(name.as_mut_ptr()), ..Default::default() };
                log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
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
            Ok(Self { handle, recorder, consumer: Some(consumer) })
        }

        /// Kończy śledzenie i oddaje zebrane odczyty.
        pub fn finish(mut self) -> super::Session {
            self.stop();
            self.recorder.lock().map(|r| r.session.clone()).unwrap_or_default()
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

    // Uchwyt sesji to liczba, a rejestrator jest za muteksem.
    unsafe impl Send for Trace {}
}

#[cfg(windows)]
pub use etw::{stop_stale, Trace};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_ranges() {
        assert_eq!(chunks_of(0, 1), 0..=0);
        assert_eq!(chunks_of(CHUNK - 1, 2), 0..=1);
        assert_eq!(chunks_of(3 * CHUNK, CHUNK), 3..=3);
        assert_eq!(chunks_of(5 * CHUNK, 0), 5..=5);
    }

    #[test]
    fn recorder_keeps_only_game_files() {
        let mut r = Recorder::new(r"\Device\HarddiskVolume5\Games\X", 7);
        r.on_create(100, 1, r"\Device\HarddiskVolume5\Games\X\Data\a.pak");
        r.on_create(100, 2, r"\Device\HarddiskVolume5\Windows\system32\x.dll");
        // Własny proces (wczytywanie do pamięci) nie jest zapisywany.
        r.on_create(7, 3, r"\Device\HarddiskVolume5\Games\X\data\b.pak");
        r.on_read(1, CHUNK / 2, CHUNK);
        r.on_read(2, 0, 100);
        r.on_read(3, 0, 100);
        r.on_read(9, 0, 100);
        assert_eq!(r.session.len(), 1);
        assert_eq!(r.session[r"data\a.pak"].iter().copied().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(r.chunks(), 2);
        // Ten sam adres obiektu dla pliku spoza gry - poprzedni wpis znika.
        r.on_create(100, 1, r"\Device\HarddiskVolume5\other.bin");
        r.on_read(1, 10 * CHUNK, 1);
        assert_eq!(r.chunks(), 2);
    }

    #[test]
    fn profile_merge_and_segments() {
        let mut p = Profile::default();
        p.merge(&Session::new());
        assert_eq!(p.sessions, 0);
        let s1: Session = [("a.pak".to_string(), [0u32, 1, 2, 7].into_iter().collect())].into_iter().collect();
        let s2: Session = [
            ("a.pak".to_string(), [1u32, 2].into_iter().collect()),
            ("gone.pak".to_string(), [0u32].into_iter().collect()),
        ]
        .into_iter()
        .collect();
        p.merge(&s1);
        p.merge(&s2);
        assert_eq!(p.sessions, 2);
        assert_eq!(p.files["a.pak"][&1], 2);
        assert_eq!(p.bytes(), 5 * CHUNK);

        let dir = Path::new(r"C:\g");
        // a.pak ma 7,5 MiB; gone.pak już nie istnieje.
        let size = |path: &Path| path.ends_with("a.pak").then_some(7 * CHUNK + CHUNK / 2);
        let segs = p.segments(dir, u64::MAX, &size);
        assert_eq!(
            segs,
            vec![
                Seg { path: dir.join("a.pak"), offset: 0, len: 3 * CHUNK },
                Seg { path: dir.join("a.pak"), offset: 7 * CHUNK, len: CHUNK / 2 },
            ]
        );
        // Miejsce na 2 fragmenty: zostają czytane w obu sesjach.
        let segs = p.segments(dir, 2 * CHUNK, &size);
        assert_eq!(segs, vec![Seg { path: dir.join("a.pak"), offset: CHUNK, len: 2 * CHUNK }]);
    }

    /// Śledzenie na tym komputerze (administrator): odczyt znanego miejsca w
    /// pliku musi trafić do sesji. `cargo test trace_live -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn trace_live() {
        use std::io::{Read, Seek, SeekFrom};
        let dir = std::env::temp_dir().join(format!("tc-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.bin");
        std::fs::write(&file, vec![5u8; 6 * CHUNK as usize]).unwrap();
        let trace = Trace::start(&dir).expect("trace");
        // Rejestrator pomija własny proces - na czas testu udajemy inny.
        trace.recorder.lock().unwrap().own_pid = 0;
        std::thread::sleep(std::time::Duration::from_millis(500));
        let mut f = std::fs::File::open(&file).unwrap();
        f.seek(SeekFrom::Start(4 * CHUNK + 100)).unwrap();
        let mut buf = vec![0u8; 1000];
        f.read_exact(&mut buf).unwrap();
        drop(f);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let session = trace.finish();
        eprintln!("session: {session:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(session.get("data.bin").is_some_and(|c| c.contains(&4)), "no read recorded");
    }
}
