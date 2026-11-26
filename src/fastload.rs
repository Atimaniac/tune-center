//! Fast Load: wczytanie plików gry do pamięci podręcznej Windows przed jej
//! uruchomieniem oraz sprawdzenie gotowości do DirectStorage.
//!
//! Wczytanie to zwykły, buforowany odczyt plików gry: Windows zostawia
//! przeczytane dane w wolnej pamięci RAM (lista standby), więc gra dostaje je
//! potem z pamięci zamiast z dysku. Pliki gry nie są zmieniane, nic nie jest
//! wstrzykiwane do gry. Dane zostają w pamięci tak długo, jak Windows nie
//! potrzebuje jej na coś innego - stąd wymaganie dużej ilości RAM.
//!
//! DirectStorage działa tylko w grach, które same go używają (mają
//! `dstorage.dll`). Dla nich program pokazuje, czy dysk to NVMe i czy system
//! plików pozwala na BypassIO (pytanie `FSCTL_MANAGE_BYPASS_IO`, tylko odczyt).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::fastload_trace::{Seg, Store};

const GIB: u64 = 1 << 30;
/// Od tylu GB pamięci RAM funkcja ma sens (zestaw 32 GB zgłasza ok. 31,9 GB).
pub const MIN_RAM: u64 = 30 * GIB;

/// Rodzaj dysku, na którym leży gra.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Nvme,
    Ssd,
    Hdd,
    Usb,
    Unknown,
}

impl DriveKind {
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Nvme => "NVMe",
            DriveKind::Ssd => "SATA SSD",
            DriveKind::Hdd => "HDD",
            DriveKind::Usb => "USB",
            DriveKind::Unknown => "?",
        }
    }
}

/// Odpowiedź systemu plików na pytanie o BypassIO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bypass {
    /// BypassIO można włączyć dla plików na tym woluminie.
    Ready,
    /// Blokuje je sterownik (nazwa) z podanego powodu.
    Blocked { driver: String, reason: String },
    /// System nie zna tego pytania (Windows 10) albo wolumin go nie obsługuje.
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct Drive {
    /// Litera z dwukropkiem, np. "D:"; pusta dla ścieżek sieciowych.
    pub letter: String,
    pub kind: DriveKind,
    pub bypass: Bypass,
}

/// Gotowość gry do DirectStorage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// Gra nie używa DirectStorage.
    NotUsed,
    /// NVMe i BypassIO - pełna ścieżka.
    Ready,
    /// NVMe, ale BypassIO zablokowane albo niedostępne.
    NoBypass,
    /// Dysk inny niż NVMe - działa, ale bez korzyści.
    SlowDrive,
}

pub fn readiness(dstorage: bool, drive: &Drive) -> Readiness {
    if !dstorage {
        return Readiness::NotUsed;
    }
    match (drive.kind, &drive.bypass) {
        (DriveKind::Nvme, Bypass::Ready) => Readiness::Ready,
        (DriveKind::Nvme, _) => Readiness::NoBypass,
        _ => Readiness::SlowDrive,
    }
}

/// Stan wczytywania jednej gry.
#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    Idle,
    Queued,
    Running { done: u64, total: u64 },
    /// Wczytane bajty, czas, czy zmieściło się wszystko, czy dane są trzymane
    /// w pamięci i czy wczytano tylko fragmenty z profilu gry.
    Done { bytes: u64, secs: f64, whole: bool, pinned: bool, profiled: bool },
    Stopped,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub store: &'static str,
    pub dir: PathBuf,
    /// Bajty do wczytania (bez filmów i instalatorów); `None` - jeszcze liczone.
    pub size: Option<u64>,
    pub drive: Drive,
    /// Wersja `dstorage.dll` gry ("" gdy nie da się jej odczytać); `None` - gra
    /// nie używa DirectStorage albo folder nie został jeszcze przejrzany.
    pub dstorage: Option<String>,
    pub load: Load,
    /// Profil odczytów gry: rozmiar zapamiętanych fragmentów i liczba sesji.
    pub profile: Option<(u64, u32)>,
    /// Trwa zapisywanie odczytów gry: liczba zebranych fragmentów (MiB).
    pub learning: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub games: Vec<Entry>,
    pub scanning: bool,
    pub scanned: bool,
    /// Pamięć RAM: cała i dostępna (wolna + pamięć podręczna), w bajtach.
    pub ram_total: u64,
    pub ram_avail: u64,
    /// Błąd ostatniej operacji (np. brak uprawnień do śledzenia odczytów).
    pub message: Option<String>,
}

/// Ile bajtów można wczytać, żeby zostawić zapas dla systemu i gry.
pub fn budget(total: u64, avail: u64) -> u64 {
    let reserve = (total / 8).max(4 * GIB);
    avail.saturating_sub(reserve)
}

/// Pliki, których gra nie czyta przy wczytywaniu poziomów: filmy, instalatory,
/// symbole i zrzuty.
pub fn skip_file(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "bk2" | "bik" | "mp4" | "webm" | "usm" | "wmv" | "avi" | "mkv" | "pdb" | "dmp" | "log" | "msi" | "cab"
    )
}

/// Foldery z instalatorami i dodatkami, nie z danymi gry.
pub fn skip_dir(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "_commonredist" | "redist" | "redistributables" | "__installer" | "directx" | "vcredist" | "support"
            | "easyanticheat" | "battleye" | "crashreports" | "logs" | "screenshots"
    )
}

/// Pliki gry do wczytania (posortowane po ścieżce), ich łączny rozmiar i
/// ścieżka `dstorage.dll`, jeśli gra ją ma.
pub fn walk(dir: &Path) -> (Vec<(PathBuf, u64)>, u64, Option<PathBuf>) {
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut dstorage = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            // Dowiązań nie śledzimy (pętle, pliki spoza gry).
            if ft.is_symlink() {
                continue;
            }
            let path = e.path();
            if ft.is_dir() {
                if !skip_dir(&e.file_name().to_string_lossy()) {
                    stack.push(path);
                }
                continue;
            }
            if e.file_name().to_string_lossy().eq_ignore_ascii_case("dstorage.dll") && dstorage.is_none() {
                dstorage = Some(path.clone());
            }
            if skip_file(&path) {
                continue;
            }
            let len = e.metadata().map(|m| m.len()).unwrap_or(0);
            if len > 0 {
                total += len;
                files.push((path, len));
            }
        }
    }
    files.sort();
    (files, total, dstorage)
}

/// Litera woluminu ścieżki ("D:"), jeśli ścieżka ją ma.
pub fn drive_letter(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let b = s.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| s[..2].to_ascii_uppercase())
}

#[cfg(windows)]
mod sys {
    use super::{Bypass, DriveKind};
    use std::ffi::c_void;
    use std::path::Path;
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, FILE_FLAGS_AND_ATTRIBUTES,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_NO_BUFFERING,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, VS_FIXEDFILEINFO,
    };
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::Win32::System::IO::DeviceIoControl;

    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const FSCTL_MANAGE_BYPASS_IO: u32 = 0x0009_0448;
    const FS_BPIO_OP_QUERY: u32 = 3;
    const STORAGE_DEVICE_PROPERTY: u32 = 0;
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Uchwyt bez praw odczytu danych - wystarcza do pytań o właściwości.
    fn open(path: &str) -> Option<Handle> {
        open_with(path, FILE_FLAG_BACKUP_SEMANTICS)
    }

    fn open_with(path: &str, flags: FILE_FLAGS_AND_ATTRIBUTES) -> Option<Handle> {
        let name = HSTRING::from(path);
        unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                flags,
                None,
            )
        }
        .ok()
        .map(Handle)
    }

    fn ioctl(h: &Handle, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                h.0,
                code,
                Some(input.as_ptr() as *const c_void),
                input.len() as u32,
                Some(out.as_mut_ptr() as *mut c_void),
                out.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .ok()
        .map(|_| returned as usize)
    }

    fn property(h: &Handle, id: u32, out: &mut [u8]) -> Option<usize> {
        // STORAGE_PROPERTY_QUERY: PropertyId, QueryType (0 = standard), dane.
        let mut query = [0u8; 12];
        query[..4].copy_from_slice(&id.to_le_bytes());
        ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, &query, out)
    }

    /// Rodzaj dysku woluminu `letter` ("D:").
    pub fn drive_kind(letter: &str) -> DriveKind {
        let Some(h) = open(&format!(r"\\.\{letter}")) else { return DriveKind::Unknown };
        let mut buf = [0u8; 1024];
        // STORAGE_DEVICE_DESCRIPTOR: BusType pod przesunięciem 28.
        let bus = match property(&h, STORAGE_DEVICE_PROPERTY, &mut buf) {
            Some(n) if n >= 32 => u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]),
            _ => return DriveKind::Unknown,
        };
        match bus {
            17 => return DriveKind::Nvme,
            7 => return DriveKind::Usb,
            _ => {}
        }
        // DEVICE_SEEK_PENALTY_DESCRIPTOR: IncursSeekPenalty pod przesunięciem 8.
        let mut seek = [0u8; 16];
        match property(&h, STORAGE_DEVICE_SEEK_PENALTY_PROPERTY, &mut seek) {
            Some(n) if n >= 9 => {
                if seek[8] != 0 {
                    DriveKind::Hdd
                } else {
                    DriveKind::Ssd
                }
            }
            _ => DriveKind::Unknown,
        }
    }

    /// Napis UTF-16 z pola o stałym rozmiarze `bytes`, do pierwszego zera.
    fn wide(buf: &[u8], at: usize, bytes: usize) -> String {
        let end = (at + bytes).min(buf.len());
        let units: Vec<u16> = buf[at.min(end)..end]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        String::from_utf16_lossy(&units)
    }

    /// Wynik FS_BPIO_OUTPUT dla operacji QUERY (nagłówek 24 bajty, potem
    /// FS_BPIO_RESULTS: status, nazwa sterownika, powód).
    pub fn parse_bypass(out: &[u8]) -> Bypass {
        if out.len() < 96 {
            return Bypass::Unsupported;
        }
        let status = i32::from_le_bytes([out[24], out[25], out[26], out[27]]);
        if status == 0 {
            return Bypass::Ready;
        }
        // Pola nazw mają stały rozmiar: 32 i 128 znaków.
        Bypass::Blocked { driver: wide(out, 30, 64), reason: wide(out, 96, 256) }
    }

    /// Czy system plików pozwala na BypassIO dla pliku `file` (samo pytanie).
    /// BypassIO dotyczy uchwytów bez buforowania, więc tak otwieramy plik.
    pub fn bypass(file: &Path) -> Bypass {
        let Some(h) = open_with(&file.to_string_lossy(), FILE_FLAG_NO_BUFFERING) else { return Bypass::Unsupported };
        // FS_BPIO_INPUT: Operation, InFlags, Reserved1, Reserved2.
        let mut input = [0u8; 24];
        input[..4].copy_from_slice(&FS_BPIO_OP_QUERY.to_le_bytes());
        let mut out = [0u8; 512];
        match ioctl(&h, FSCTL_MANAGE_BYPASS_IO, &input, &mut out) {
            Some(_) => parse_bypass(&out),
            None => Bypass::Unsupported,
        }
    }

    /// Pamięć RAM: (cała, dostępna).
    pub fn memory() -> (u64, u64) {
        let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() {
            (m.ullTotalPhys, m.ullAvailPhys)
        } else {
            (0, 0)
        }
    }

    /// Mapuje fragment pliku do odczytu i dotyka każdej jego strony, żeby
    /// trafiła do zbioru roboczego programu. Zwraca adres widoku.
    pub fn map(path: &Path, offset: u64, len: u64) -> Option<usize> {
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
        use windows::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, FILE_MAP_READ, PAGE_READONLY};
        const GRANULARITY: u64 = 64 << 10;
        const FILE_READ_DATA: u32 = 1;
        let name = HSTRING::from(path.to_string_lossy().as_ref());
        unsafe {
            let file = Handle(
                CreateFileW(
                    PCWSTR(name.as_ptr()),
                    FILE_READ_DATA,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
                .ok()?,
            );
            let mapping = Handle(CreateFileMappingW(file.0, None, PAGE_READONLY, 0, 0, PCWSTR::null()).ok()?);
            // Początek widoku musi być wielokrotnością 64 KiB.
            let start = offset / GRANULARITY * GRANULARITY;
            let size = (offset - start + len) as usize;
            let view = MapViewOfFile(mapping.0, FILE_MAP_READ, (start >> 32) as u32, start as u32, size);
            if view.Value.is_null() {
                return None;
            }
            let base = view.Value as *const u8;
            let mut at = 0usize;
            while at < size {
                std::ptr::read_volatile(base.add(at));
                at += 4096;
            }
            Some(view.Value as usize)
        }
    }

    pub fn unmap(view: usize) {
        use windows::Win32::System::Memory::{UnmapViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS};
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view as *mut c_void });
        }
    }

    /// Wersja pliku z zasobów ("1.2.2.0" -> "1.2.2").
    pub fn file_version(path: &Path) -> Option<String> {
        let name = HSTRING::from(path.to_string_lossy().as_ref());
        unsafe {
            let size = GetFileVersionInfoSizeW(PCWSTR(name.as_ptr()), None);
            if size == 0 {
                return None;
            }
            let mut data = vec![0u8; size as usize];
            GetFileVersionInfoW(PCWSTR(name.as_ptr()), None, size, data.as_mut_ptr() as *mut c_void).ok()?;
            let mut info: *mut c_void = std::ptr::null_mut();
            let mut len = 0u32;
            if !VerQueryValueW(data.as_ptr() as *const c_void, w!("\\"), &mut info, &mut len).as_bool()
                || info.is_null()
                || (len as usize) < size_of::<VS_FIXEDFILEINFO>()
            {
                return None;
            }
            let f = &*(info as *const VS_FIXEDFILEINFO);
            let (a, b, c) = (f.dwFileVersionMS >> 16, f.dwFileVersionMS & 0xFFFF, f.dwFileVersionLS >> 16);
            Some(format!("{a}.{b}.{c}"))
        }
    }
}

#[cfg(not(windows))]
mod sys {
    use super::{Bypass, DriveKind};
    use std::path::Path;
    pub fn drive_kind(_: &str) -> DriveKind {
        DriveKind::Unknown
    }
    pub fn bypass(_: &Path) -> Bypass {
        Bypass::Unsupported
    }
    pub fn memory() -> (u64, u64) {
        (0, 0)
    }
    pub fn file_version(_: &Path) -> Option<String> {
        None
    }
    pub fn map(_: &Path, _: u64, _: u64) -> Option<usize> {
        None
    }
    pub fn unmap(_: usize) {}
}

pub use sys::memory;

/// Dysk gry; wynik dla litery jest zapamiętywany w `cache`.
fn drive_of(dir: &Path, cache: &mut HashMap<String, Drive>) -> Drive {
    let Some(letter) = drive_letter(dir) else {
        return Drive { letter: String::new(), kind: DriveKind::Unknown, bypass: Bypass::Unsupported };
    };
    cache
        .entry(letter.clone())
        .or_insert_with(|| Drive {
            kind: sys::drive_kind(&letter),
            bypass: any_file(dir).map(|f| sys::bypass(&f)).unwrap_or(Bypass::Unsupported),
            letter,
        })
        .clone()
}

/// Dowolny plik z folderu gry (pierwszy znaleziony, najwyżej 3 poziomy w głąb).
fn any_file(dir: &Path) -> Option<PathBuf> {
    let mut level = vec![dir.to_path_buf()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for d in &level {
            // Folder bez prawa odczytu nie przerywa szukania w pozostałych.
            let Ok(entries) = std::fs::read_dir(d) else { continue };
            for e in entries.flatten() {
                match e.file_type() {
                    Ok(t) if t.is_file() => return Some(e.path()),
                    Ok(t) if t.is_dir() => next.push(e.path()),
                    _ => {}
                }
            }
        }
        level = next;
    }
    None
}

/// Całe pliki jako fragmenty do wczytania.
pub fn whole(files: &[(PathBuf, u64)]) -> Vec<Seg> {
    files.iter().map(|(p, len)| Seg { path: p.clone(), offset: 0, len: *len }).collect()
}

/// Odczyt fragmentów plików do pamięci podręcznej systemu. `progress` dostaje
/// liczbę przeczytanych bajtów; `false` z `go_on` przerywa. Zwraca
/// przeczytane bajty.
pub fn preload(segs: &[Seg], limit: u64, progress: &mut dyn FnMut(u64), go_on: &dyn Fn() -> bool) -> u64 {
    use std::io::{Read, Seek, SeekFrom};
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    let mut reported = Instant::now();
    let mut open: Option<(&Path, std::fs::File)> = None;
    for seg in segs {
        // Kolejne fragmenty tego samego pliku idą jednym uchwytem.
        if open.as_ref().map(|o| o.0) != Some(seg.path.as_path()) {
            let mut opts = std::fs::OpenOptions::new();
            opts.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                // Odczyt sekwencyjny; pełne współdzielenie, żeby nie blokować
                // gry ani jej aktualizacji.
                opts.custom_flags(0x0800_0000).share_mode(7);
            }
            // Plik zajęty albo usunięty w międzyczasie - pomijamy.
            open = opts.open(&seg.path).ok().map(|f| (seg.path.as_path(), f));
        }
        let Some((_, f)) = open.as_mut() else { continue };
        if f.seek(SeekFrom::Start(seg.offset)).is_err() {
            continue;
        }
        let mut left = seg.len;
        while left > 0 {
            if done >= limit || !go_on() {
                progress(done);
                return done;
            }
            let want = left.min(buf.len() as u64) as usize;
            match f.read(&mut buf[..want]) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    done += n as u64;
                    left -= n as u64;
                }
            }
            if reported.elapsed() >= Duration::from_millis(200) {
                reported = Instant::now();
                progress(done);
            }
        }
    }
    progress(done);
    done
}

/// Dane gry trzymane w pamięci: zmapowane widoki plików. Dopóki istnieją,
/// strony są w zbiorze roboczym programu i Windows nie usuwa ich z pamięci
/// przy zwykłej pracy (zabiera je dopiero przy braku pamięci).
#[derive(Default)]
pub struct Pin {
    views: Vec<usize>,
    pub bytes: u64,
}

// Adresy widoków są używane tylko do ich zwolnienia.
unsafe impl Send for Pin {}

impl Pin {
    /// Mapuje fragmenty i dotyka każdej strony (dane są już w pamięci
    /// podręcznej po `preload`, więc to szybkie). `go_on` przerywa.
    pub fn hold(segs: &[Seg], limit: u64, go_on: &dyn Fn() -> bool) -> Pin {
        // Kawałki po 64 MiB: między nimi sprawdzamy Stop i wolną pamięć, więc
        // nawet jeden wielogigabajtowy plik da się przerwać.
        const STEP: u64 = 64 << 20;
        let mut pin = Pin::default();
        'segments: for seg in segs {
            let mut offset = seg.offset;
            let mut left = seg.len;
            while left > 0 {
                if pin.bytes >= limit || !go_on() {
                    break 'segments;
                }
                let part = left.min(STEP).min(limit - pin.bytes);
                if let Some(view) = sys::map(&seg.path, offset, part) {
                    pin.views.push(view);
                    pin.bytes += part;
                }
                offset += part;
                left -= part;
            }
        }
        pin
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        for v in self.views.drain(..) {
            sys::unmap(v);
        }
    }
}

/// Czy działa proces uruchomiony z folderu gry.
fn running(dir: &Path) -> bool {
    let dir = dir.to_string_lossy();
    crate::game_watch::process_paths().iter().any(|p| crate::game_watch::matches(&dir, p))
}

enum Cmd {
    Scan,
    /// Wczytanie gry; `pin` - trzymać potem w pamięci.
    Load(String, bool),
    Release(String),
    Learn(String),
    FinishLearn,
    Forget(String),
}

pub struct FastLoad {
    tx: Sender<Cmd>,
    shared: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    started: bool,
}

impl FastLoad {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = channel::<Cmd>();
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (shared.clone(), stop.clone());
        crate::spawn("fastload", move || Worker::new(s, st, Box::new(repaint)).run(rx));
        Self { tx, shared, stop, started: false }
    }

    /// Pierwsze wyszukanie gier dopiero przy otwarciu strony.
    pub fn ensure_scanned(&mut self) {
        if !self.started {
            self.started = true;
            self.rescan();
        }
    }

    pub fn rescan(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.scanning = true;
        }
        let _ = self.tx.send(Cmd::Scan);
    }

    /// Dodaje grę do kolejki wczytywania; `pin` - trzymać potem w pamięci.
    pub fn load(&self, id: &str, pin: bool) {
        self.stop.store(false, Ordering::Relaxed);
        if let Ok(mut s) = self.shared.lock() {
            if let Some(g) = s.games.iter_mut().find(|g| g.id == id) {
                if matches!(g.load, Load::Queued | Load::Running { .. }) {
                    return;
                }
                g.load = Load::Queued;
            }
        }
        let _ = self.tx.send(Cmd::Load(id.to_owned(), pin));
    }

    /// Zwalnia dane gry trzymane w pamięci.
    pub fn release(&self, id: &str) {
        let _ = self.tx.send(Cmd::Release(id.to_owned()));
    }

    /// Zaczyna zapisywać, co gra czyta (do zamknięcia gry albo `finish_learn`).
    pub fn learn(&self, id: &str) {
        let _ = self.tx.send(Cmd::Learn(id.to_owned()));
    }

    pub fn finish_learn(&self) {
        let _ = self.tx.send(Cmd::FinishLearn);
    }

    /// Usuwa profil gry - znów będzie wczytywana cała.
    pub fn forget(&self, id: &str) {
        let _ = self.tx.send(Cmd::Forget(id.to_owned()));
    }

    /// Po starcie programu: wyszukanie gier i wczytanie wybranych, po `delay`
    /// (żeby nie spowalniać uruchamiania systemu). Bez trzymania w pamięci.
    pub fn auto(&mut self, ids: Vec<String>, delay: Duration) {
        if ids.is_empty() || memory().0 < MIN_RAM {
            return;
        }
        let tx = self.tx.clone();
        crate::spawn("fastload-auto", move || {
            std::thread::sleep(delay);
            let _ = tx.send(Cmd::Scan);
            for id in ids {
                let _ = tx.send(Cmd::Load(id, false));
            }
        });
    }

    /// Przerywa bieżące wczytywanie i czyści kolejkę.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut s = self.shared.lock().map(|s| s.clone()).unwrap_or_default();
        (s.ram_total, s.ram_avail) = memory();
        s
    }
}

/// Śledzenie odczytów jednej gry.
struct Learning {
    id: String,
    dir: PathBuf,
    #[cfg(windows)]
    trace: crate::fastload_trace::Trace,
    /// Gra była już widziana wśród procesów - jej zamknięcie kończy naukę.
    seen: bool,
}

/// Wątek roboczy: kolejka poleceń i co 2 s porządki (zamknięte gry, pamięć).
struct Worker {
    shared: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    repaint: Box<dyn Fn() + Send>,
    store: Store,
    /// Gry trzymane w pamięci i czy były już widziane wśród procesów.
    pins: HashMap<String, (Pin, PathBuf, bool)>,
    learning: Option<Learning>,
}

impl Worker {
    fn new(shared: Arc<Mutex<Snapshot>>, stop: Arc<AtomicBool>, repaint: Box<dyn Fn() + Send>) -> Self {
        Self { shared, stop, repaint, store: Store::load(), pins: HashMap::new(), learning: None }
    }

    fn run(mut self, rx: std::sync::mpsc::Receiver<Cmd>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Cmd::Scan) => self.scan(),
                Ok(Cmd::Load(id, pin)) => self.load(&id, pin),
                Ok(Cmd::Release(id)) => self.release(&id),
                Ok(Cmd::Learn(id)) => self.learn(&id),
                Ok(Cmd::FinishLearn) => self.finish_learn(),
                Ok(Cmd::Forget(id)) => {
                    self.store.games.remove(&id);
                    self.store.save();
                    self.with(&id, |g| g.profile = None);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => self.housekeeping(),
                Err(_) => break,
            }
        }
    }

    /// Zmienia wpis gry i odświeża okno.
    fn with(&self, id: &str, f: impl FnOnce(&mut Entry)) {
        if let Ok(mut s) = self.shared.lock() {
            if let Some(g) = s.games.iter_mut().find(|g| g.id == id) {
                f(g);
            }
        }
        (self.repaint)();
    }

    fn dir_of(&self, id: &str) -> Option<PathBuf> {
        self.shared.lock().ok().and_then(|s| s.games.iter().find(|g| g.id == id).map(|g| g.dir.clone()))
    }

    fn profile_info(&self, id: &str) -> Option<(u64, u32)> {
        self.store.games.get(id).filter(|p| !p.files.is_empty()).map(|p| (p.bytes(), p.sessions))
    }

    fn scan(&mut self) {
        let found = crate::shaders::games::find_all();
        let mut drives = HashMap::new();
        // Stan wczytywania i trwająca nauka przechodzą na nową listę.
        let old: HashMap<String, (Load, Option<usize>)> = self
            .shared
            .lock()
            .map(|s| s.games.iter().map(|g| (g.id.clone(), (g.load.clone(), g.learning))).collect())
            .unwrap_or_default();
        let games: Vec<Entry> = found
            .into_iter()
            .map(|g| Entry {
                load: old.get(&g.id).map(|o| o.0.clone()).unwrap_or(Load::Idle),
                learning: old.get(&g.id).and_then(|o| o.1),
                drive: drive_of(&g.dir, &mut drives),
                profile: self.profile_info(&g.id),
                id: g.id,
                name: g.name,
                store: g.store.name(),
                dir: g.dir,
                size: None,
                dstorage: None,
            })
            .collect();
        let dirs: Vec<(String, PathBuf)> = games.iter().map(|g| (g.id.clone(), g.dir.clone())).collect();
        if let Ok(mut s) = self.shared.lock() {
            s.games = games;
        }
        (self.repaint)();
        // Rozmiary i DirectStorage gra po grze - lista jest widoczna od razu.
        for (id, dir) in dirs {
            let (_, total, dll) = walk(&dir);
            let version = dll.map(|p| sys::file_version(&p).unwrap_or_default());
            self.with(&id, |g| {
                g.size = Some(total);
                g.dstorage = version;
            });
        }
        if let Ok(mut s) = self.shared.lock() {
            s.scanning = false;
            s.scanned = true;
        }
        (self.repaint)();
    }

    fn load(&mut self, id: &str, pin: bool) {
        let Some(dir) = self.dir_of(id) else { return };
        // Kolejka przerwana przyciskiem Stop.
        if self.stop.load(Ordering::Relaxed) {
            self.with(id, |g| g.load = Load::Stopped);
            return;
        }
        // Poprzednio trzymane dane tej gry wracają do puli przed liczeniem limitu.
        self.pins.remove(id);
        let (ram_total, ram_avail) = memory();
        let limit = budget(ram_total, ram_avail);
        if limit == 0 {
            let why = crate::i18n::tr("za mało wolnej pamięci RAM", "not enough free RAM");
            self.with(id, |g| g.load = Load::Failed(why.into()));
            return;
        }
        // Z profilu: tylko to, co gra czytała; bez profilu: cała gra.
        let profile = self.store.games.get(id).filter(|p| !p.files.is_empty());
        let (segs, fits, profiled) = match profile {
            Some(p) => {
                let size = |path: &Path| std::fs::metadata(path).ok().map(|m| m.len());
                (p.segments(&dir, limit, &size), p.bytes() <= limit, true)
            }
            None => {
                let (files, total, _) = walk(&dir);
                (whole(&files), total <= limit, false)
            }
        };
        let goal = segs.iter().map(|s| s.len).sum::<u64>().min(limit);
        self.with(id, |g| g.load = Load::Running { done: 0, total: goal });
        let started = Instant::now();
        let reserve = ram_avail - limit;
        let stop = self.stop.clone();
        // Przerwanie przyciskiem albo gdy programy zajęły pamięć z zapasu.
        let go_on = move || !stop.load(Ordering::Relaxed) && memory().1 >= reserve / 2;
        let done = preload(&segs, goal, &mut |done| self.with(id, |g| g.load = Load::Running { done, total: goal }), &go_on);
        let secs = started.elapsed().as_secs_f64().max(0.001);
        if self.stop.load(Ordering::Relaxed) {
            self.with(id, |g| g.load = Load::Stopped);
            return;
        }
        let mut pinned = false;
        // Dysk USB można odłączyć w trakcie - odczyt zmapowanej pamięci
        // zakończyłby wtedy program, więc takich gier nie trzymamy.
        let usb = self
            .shared
            .lock()
            .is_ok_and(|s| s.games.iter().any(|g| g.id == id && g.drive.kind == DriveKind::Usb));
        if pin && !usb {
            let held = Pin::hold(&segs, done, &go_on);
            // Stop w trakcie przypinania: dane zostają w pamięci podręcznej,
            // ale nic nie jest trzymane.
            if self.stop.load(Ordering::Relaxed) {
                drop(held);
                self.with(id, |g| g.load = Load::Stopped);
                return;
            }
            pinned = held.bytes > 0;
            if pinned {
                self.pins.insert(id.to_owned(), (held, dir, false));
            }
        }
        let whole = fits && done >= goal;
        self.with(id, |g| g.load = Load::Done { bytes: done, secs, whole, pinned, profiled });
    }

    fn release(&mut self, id: &str) {
        if self.pins.remove(id).is_some() {
            self.with(id, |g| {
                if let Load::Done { pinned, .. } = &mut g.load {
                    *pinned = false;
                }
            });
        }
    }

    fn learn(&mut self, id: &str) {
        self.finish_learn();
        let Some(dir) = self.dir_of(id) else { return };
        #[cfg(windows)]
        match crate::fastload_trace::Trace::start(&dir) {
            Ok(trace) => {
                self.learning = Some(Learning { id: id.to_owned(), dir, trace, seen: false });
                self.with(id, |g| g.learning = Some(0));
            }
            Err(e) => {
                if let Ok(mut s) = self.shared.lock() {
                    s.message = Some(e);
                }
                (self.repaint)();
            }
        }
        #[cfg(not(windows))]
        let _ = dir;
    }

    /// Kończy naukę: dolicza sesję do profilu gry i zapisuje go.
    fn finish_learn(&mut self) {
        let Some(l) = self.learning.take() else { return };
        #[cfg(windows)]
        {
            let session = l.trace.finish();
            if session.values().all(|c| c.is_empty()) {
                // Nic nie zapisano: gra nie była uruchomiona albo jej folder
                // leży na dysku, którego ścieżek nie da się dopasować (dysk
                // sieciowy, SUBST, dowiązanie do innego woluminu).
                if let Ok(mut s) = self.shared.lock() {
                    s.message = Some(
                        crate::i18n::tr(
                            "Nauka nie zapisała żadnych odczytów - uruchom grę w trakcie zapisu. Gry na dysku sieciowym albo za dowiązaniem do innego dysku nie są obsługiwane.",
                            "Learning recorded no reads - start the game while recording. Games on a network drive or behind a link to another drive are not supported.",
                        )
                        .to_owned(),
                    );
                }
            } else if let Ok(mut s) = self.shared.lock() {
                s.message = None;
            }
            self.store.games.entry(l.id.clone()).or_default().merge(&session);
            self.store.save();
        }
        let info = self.profile_info(&l.id);
        self.with(&l.id, |g| {
            g.learning = None;
            g.profile = info;
        });
    }

    fn housekeeping(&mut self) {
        // Nauka: licznik w oknie i koniec po zamknięciu gry.
        if let Some(l) = &mut self.learning {
            let now = running(&l.dir);
            let finished = l.seen && !now;
            l.seen |= now;
            #[cfg(windows)]
            let chunks = l.trace.recorder.lock().map(|r| r.chunks()).unwrap_or(0);
            #[cfg(not(windows))]
            let chunks = 0;
            let id = l.id.clone();
            if finished {
                self.finish_learn();
            } else {
                self.with(&id, |g| g.learning = Some(chunks));
            }
        }
        // Trzymane gry: zwolnienie po zamknięciu gry (żeby nie blokować jej
        // aktualizacji) albo gdy systemowi brakuje pamięci.
        if self.pins.is_empty() {
            return;
        }
        let low = memory().1 < 2 * GIB;
        let mut gone = Vec::new();
        for (id, (_, dir, seen)) in &mut self.pins {
            let now = running(dir);
            if low || (*seen && !now) {
                gone.push(id.clone());
            }
            *seen |= now;
        }
        for id in gone {
            self.release(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zapas na inne odczyty z folderu gry w czasie testu (MiB).
    const CHUNK_SLACK: u64 = 1 << 20;

    #[test]
    fn budget_keeps_reserve() {
        // 32 GB: zapas 4 GB; 64 GB: zapas 8 GB.
        assert_eq!(budget(32 * GIB, 20 * GIB), 16 * GIB);
        assert_eq!(budget(64 * GIB, 20 * GIB), 12 * GIB);
        assert_eq!(budget(16 * GIB, 3 * GIB), 0);
    }

    #[test]
    fn letters_and_skips() {
        assert_eq!(drive_letter(Path::new(r"d:\Games\X")), Some("D:".into()));
        assert_eq!(drive_letter(Path::new(r"\\?\C:\Games")), Some("C:".into()));
        assert_eq!(drive_letter(Path::new(r"\\server\share\x")), None);
        assert!(skip_file(Path::new(r"C:\g\movies\intro.BK2")));
        assert!(!skip_file(Path::new(r"C:\g\data\pakchunk0.pak")));
        assert!(skip_dir("_CommonRedist"));
        assert!(!skip_dir("Content"));
    }

    #[test]
    fn readiness_rules() {
        let d = |kind, bypass| Drive { letter: "C:".into(), kind, bypass };
        let blocked = Bypass::Blocked { driver: "bitlocker".into(), reason: "x".into() };
        assert_eq!(readiness(false, &d(DriveKind::Nvme, Bypass::Ready)), Readiness::NotUsed);
        assert_eq!(readiness(true, &d(DriveKind::Nvme, Bypass::Ready)), Readiness::Ready);
        assert_eq!(readiness(true, &d(DriveKind::Nvme, blocked)), Readiness::NoBypass);
        assert_eq!(readiness(true, &d(DriveKind::Ssd, Bypass::Ready)), Readiness::SlowDrive);
    }

    #[test]
    fn walk_and_preload() {
        let dir = std::env::temp_dir().join(format!("tc-fastload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::create_dir_all(dir.join("_CommonRedist")).unwrap();
        std::fs::write(dir.join("data").join("a.pak"), vec![1u8; 3000]).unwrap();
        std::fs::write(dir.join("data").join("b.pak"), vec![2u8; 2000]).unwrap();
        std::fs::write(dir.join("intro.mp4"), vec![0u8; 500]).unwrap();
        std::fs::write(dir.join("_CommonRedist").join("vc.exe"), vec![0u8; 500]).unwrap();
        std::fs::write(dir.join("dstorage.dll"), b"x").unwrap();
        let (files, total, dll) = walk(&dir);
        assert_eq!(total, 5001);
        assert_eq!(files.len(), 3);
        assert!(dll.is_some());
        let mut last = 0;
        assert_eq!(preload(&whole(&files), u64::MAX, &mut |d| last = d, &|| true), 5001);
        assert_eq!(last, 5001);
        // Przerwanie przed pierwszym odczytem.
        assert_eq!(preload(&whole(&files), u64::MAX, &mut |_| {}, &|| false), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn bypass_result_parsing() {
        let mut out = vec![0u8; 352];
        assert_eq!(sys::parse_bypass(&out), Bypass::Ready);
        out[24..28].copy_from_slice(&(-1073741637i32).to_le_bytes());
        let name: Vec<u8> = "bitlocker".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        out[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        out[30..30 + name.len()].copy_from_slice(&name);
        let why: Vec<u8> = "encrypted".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        out[94..96].copy_from_slice(&(why.len() as u16).to_le_bytes());
        out[96..96 + why.len()].copy_from_slice(&why);
        assert_eq!(sys::parse_bypass(&out), Bypass::Blocked { driver: "bitlocker".into(), reason: "encrypted".into() });
    }

    /// Dwa wczytania tej samej gry (`TC_GAME` = fragment nazwy): drugie powinno
    /// iść z pamięci. `cargo test fastload_twice -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn fastload_twice() {
        let want = std::env::var("TC_GAME").unwrap_or_default().to_lowercase();
        let game = crate::shaders::games::find_all()
            .into_iter()
            .find(|g| g.name.to_lowercase().contains(&want))
            .expect("game");
        let (files, total, _) = walk(&game.dir);
        for pass in 1..=2 {
            let t = Instant::now();
            let done = preload(&whole(&files), u64::MAX, &mut |_| {}, &|| true);
            let secs = t.elapsed().as_secs_f64();
            eprintln!("pass {pass}: {} of {} bytes in {:.2} s = {:.2} GB/s", done, total, secs, done as f64 / secs / GIB as f64);
            assert_eq!(done, total);
        }
    }

    /// Cały łańcuch na prawdziwej grze (`TC_GAME`, administrator): nauka
    /// profilu z odczytu innego procesu, wczytanie z profilu, trzymanie,
    /// zwolnienie i usunięcie profilu.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn fastload_chain_live() {
        let want = std::env::var("TC_GAME").unwrap_or_default().to_lowercase();
        let mut fl = FastLoad::new(|| {});
        fl.ensure_scanned();
        let wait = |fl: &FastLoad, what: &str, ok: &dyn Fn(&Entry) -> bool| -> Entry {
            for _ in 0..600 {
                let s = fl.snapshot();
                if let Some(g) = s.games.iter().find(|g| g.name.to_lowercase().contains(&want)) {
                    if s.scanned && ok(g) {
                        return g.clone();
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            panic!("timeout: {what}; message {:?}", fl.snapshot().message);
        };
        let g = wait(&fl, "scan", &|g| g.size.is_some());
        fl.forget(&g.id);
        fl.learn(&g.id);
        wait(&fl, "learning", &|g| g.learning.is_some());
        // Odczyt największego pliku gry z innego procesu (własne są pomijane).
        let (files, _, _) = walk(&g.dir);
        let big = files.iter().max_by_key(|f| f.1).unwrap();
        let status = std::process::Command::new("cmd").arg("/c").arg("type").arg(&big.0).stdout(std::process::Stdio::null()).status();
        assert!(status.is_ok());
        std::thread::sleep(Duration::from_secs(3));
        fl.finish_learn();
        let g = wait(&fl, "profile", &|g| g.learning.is_none() && g.profile.is_some());
        let (bytes, sessions) = g.profile.unwrap();
        eprintln!("profile: {:.2} GB in {sessions} session(s); file {} = {:.2} GB", bytes as f64 / GIB as f64, big.0.display(), big.1 as f64 / GIB as f64);
        assert!(bytes >= big.1 && bytes < big.1 + 64 * CHUNK_SLACK);
        fl.load(&g.id, true);
        let g = wait(&fl, "load", &|g| matches!(g.load, Load::Done { .. } | Load::Failed(_)));
        eprintln!("load: {:?}", g.load);
        assert!(matches!(g.load, Load::Done { pinned: true, profiled: true, whole: true, .. }));
        fl.release(&g.id);
        wait(&fl, "release", &|g| matches!(g.load, Load::Done { pinned: false, .. }));
        fl.forget(&g.id);
        wait(&fl, "forget", &|g| g.profile.is_none());
    }

    /// Trzymanie w pamięci 2 GiB gry (`TC_GAME`): dostępna pamięć spada o tyle,
    /// ile trzymamy, i wraca po zwolnieniu.
    #[test]
    #[ignore]
    fn fastload_pin_live() {
        let want = std::env::var("TC_GAME").unwrap_or_default().to_lowercase();
        let game = crate::shaders::games::find_all()
            .into_iter()
            .find(|g| g.name.to_lowercase().contains(&want))
            .expect("game");
        let (files, _, _) = walk(&game.dir);
        let segs = whole(&files);
        let limit = 2 * GIB;
        let done = preload(&segs, limit, &mut |_| {}, &|| true);
        let before = memory().1;
        let t = Instant::now();
        let pin = Pin::hold(&segs, done, &|| true);
        let held = memory().1;
        eprintln!(
            "read {:.2} GB, pinned {:.2} GB in {:.2} s, available {:.2} -> {:.2} GB",
            done as f64 / GIB as f64,
            pin.bytes as f64 / GIB as f64,
            t.elapsed().as_secs_f64(),
            before as f64 / GIB as f64,
            held as f64 / GIB as f64
        );
        assert!(pin.bytes >= limit - (8 << 20));
        assert!(before.saturating_sub(held) > GIB, "pages not in the working set");
        drop(pin);
        let after = memory().1;
        eprintln!("released: available {:.2} GB", after as f64 / GIB as f64);
        assert!(after > held + GIB);
    }

    /// Odczyt na tym komputerze: `cargo test fastload_live -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn fastload_live() {
        let (total, avail) = memory();
        eprintln!("RAM {:.1} / {:.1} GB, budget {:.1} GB", avail as f64 / GIB as f64, total as f64 / GIB as f64, budget(total, avail) as f64 / GIB as f64);
        let mut drives = HashMap::new();
        for g in crate::shaders::games::find_all() {
            let d = drive_of(&g.dir, &mut drives);
            let (_, size, dll) = walk(&g.dir);
            let ver = dll.as_deref().map(|p| sys::file_version(p).unwrap_or_default());
            eprintln!("{:<40} {:>7.1} GB  {} {:?} {:?}  dstorage {:?}", g.name, size as f64 / GIB as f64, d.letter, d.kind, d.bypass, ver);
        }
    }
}

#[cfg(windows)]
impl Drop for FastLoad {
    fn drop(&mut self) {
        // Zamknięcie programu w trakcie nauki: sesja śledzenia plików nie może zostać.
        self.stop();
        crate::fastload_trace::stop_stale();
    }
}
