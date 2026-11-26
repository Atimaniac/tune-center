//! Snowdrop (Massive; Star Wars Outlaws, kryptonim "helix"): shadery z
//! archiwów `<kryptonim>\sdf\pc\data`. Tylko odczyt plików gry.
//!
//! - `sdf.sdftoc` (little endian): "WEST", u32 wersja (0x29, 0x2A), 3 x u32,
//!   u32 liczba slotów, u32 liczba DDS, u32 liczba ustawień, 16 B, 48 B
//!   znacznik ("massive\0", 32 B, "ubisoft\0"), u8 podpis, u8 szyfrowanie,
//!   320 B podpisu (gdy jest), ustawienia po 8 B, 10 języków po 6 B, potem
//!   dla slotów: u32 rozmiary, 48 B znaczniki, u64 skróty. Dalsza tabela
//!   plików jest szyfrowana - nie jest czytana.
//! - Slot NNNN to plik `sdf-<część>-NNNN[-<język>].sdfdata`: "BERG", u32
//!   wersja, potem plastry jeden za drugim. Plaster skompresowany to strony
//!   64 KB, każda blok Oodle z nagłówkiem podającym długość (0x8C, dekoder
//!   6/10/12 z bitem sum kontrolnych, 3 B nagłówka kwantu: 18 bitów rozmiar -
//!   1, 3 B sumy gdy bit jest); plaster zapisany wprost to surowe bajty -
//!   przechodzone szukaniem następnej strony.
//! - Shader to zasób E3 02 BC DA, u32 1 i jeden kontener DXBC (DXBC SM5 albo
//!   DXIL), w stronach albo wprost. Strona nie podaje rozmiaru po
//!   rozpakowaniu - daje go rozmiar kontenera; dekodowane są tylko strony
//!   zaczynające zasób (pierwsze 8 B bloku LZ leży jawnie za nagłówkiem
//!   kawałka).
//!
//! Kontenery nie mają RTS0: root signature buduje silnik (wersja 1.1, stała
//! budowa: graficzna i compute, 32 statyczne samplery w przestrzeni 1 z
//! anizotropią z ustawień gracza, domyślnie 16x). Pary VS/PS nie są zapisane
//! (leżą w różnych plikach) - pule: graficzna (VS, PS) i compute. Pierwsze
//! przejście czyta wszystkie pliki danych (dziesiątki GB); pliki bez shaderów
//! są zapamiętane (nazwa, rozmiar, skrót z TOC) i potem pomijane.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{read_at, Container, Kind};
use super::readers::{Bytes, Detected, Set12};

const TOC: &str = r"sdf\pc\data\sdf.sdftoc";
/// Kryptonim Star Wars Outlaws - tylko jego root signature są znane.
const HELIX: &str = "helix";
const EXES: [&str; 2] = ["Outlaws.exe", "Outlaws_Plus.exe"];

const PAGE: usize = 1 << 16;
/// Zasób shadera (kontener + 8 B) i okno odczytu (zasób zapisany wprost się mieści).
const MAX_ASSET: usize = 8 << 20;
const WINDOW_MAX: usize = MAX_ASSET + (1 << 20);
/// Kawałek czytany naraz przy przechodzeniu pliku.
const CHUNK: usize = 1 << 20;
/// Strony znalezione szukaniem, które jeden plik może sprawdzać.
const MAX_RESYNCS: usize = 1 << 18;
const MAX_SLOTS: u32 = 1 << 16;
const MAX_SETTINGS: u32 = 1 << 12;
const MAX_DIRS: usize = 512;
const MAX_FILES: usize = 8192;
const MAX_SHADERS: usize = 400_000;
const MAX_RECORD: u64 = 16 << 20;
const RECORD_HEADER: &str = "snowdrop-walk 1";

const PREFIX: [u8; 12] = [0xE3, 0x02, 0xBC, 0xDA, 1, 0, 0, 0, b'D', b'X', b'B', b'C'];

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

// ----------------------------------- TOC --------------------------------------

/// Nagłówek TOC: 98 B. Wynik: (sloty, ustawienia, podpisany).
fn toc_header(path: &Path) -> Option<(u32, u32, bool)> {
    let file = File::open(path).ok()?;
    let mut h = [0u8; 0x30 + 48 + 2];
    read_at(&file, 0, &mut h).ok()?;
    let (version, slots, settings) = (le32(&h, 4)?, le32(&h, 0x14)?, le32(&h, 0x1C)?);
    let ok = &h[..4] == b"WEST"
        && matches!(version, 0x29 | 0x2A)
        && (1..=MAX_SLOTS).contains(&slots)
        && settings <= MAX_SETTINGS
        && h[0x30..].starts_with(b"massive\0");
    ok.then_some((slots, settings, h[0x30 + 48] != 0))
}

/// Skróty slotów z TOC (nie jest czytana zaszyfrowana tabela plików).
fn toc_hashes(path: &Path) -> Result<Vec<u64>, String> {
    let (slots, settings, signed) = toc_header(path)
        .ok_or_else(|| format!("{}: nie jest TOC Snowdrop / isn't a Snowdrop TOC", path.display()))?;
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let slots = slots as u64;
    let table = 98 + if signed { 0x140 } else { 0 } + 8 * settings as u64 + 60;
    if len < table + 60 * slots {
        return Err(format!("{}: krótszy niż podaje nagłówek / shorter than its header says", path.display()));
    }
    let mut b = vec![0u8; 8 * slots as usize];
    read_at(&file, table + 52 * slots, &mut b).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(b.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect())
}

/// TOC każdego folderu kryptonimu (gra i dodatki): (kryptonim, ścieżka). Też
/// sam wskazany folder, gdy to folder kryptonimu.
fn find_tocs(game: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if game.join(TOC).is_file() {
        out.push((name(game), game.join(TOC)));
    }
    let Ok(list) = std::fs::read_dir(game) else { return out };
    let mut dirs: Vec<PathBuf> = list
        .flatten()
        .take(MAX_DIRS * 8)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort_by_key(|d| name(d).to_lowercase());
    for d in dirs.into_iter().take(MAX_DIRS) {
        let toc = d.join(TOC);
        if toc.is_file() {
            out.push((name(&d), toc));
        }
    }
    out
}

/// Numer slotu z nazwy `sdf-<litera>-NNNN[-xx-xx].sdfdata`.
fn data_slot(name: &str) -> Option<usize> {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("sdf-")?.strip_suffix(".sdfdata")?.as_bytes();
    let (head, tail) = (rest.get(..6)?, &rest[6..]);
    if !head[0].is_ascii_lowercase() || head[1] != b'-' || !head[2..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let locale = |t: &[u8]| t.len() == 6 && t[0] == b'-' && t[3] == b'-' && [1, 2, 4, 5].iter().all(|&i| t[i].is_ascii_lowercase());
    if !tail.is_empty() && !locale(tail) {
        return None;
    }
    std::str::from_utf8(&head[2..]).ok()?.parse().ok()
}

/// Pliki danych folderu TOC: (ścieżka, rozmiar, slot), po nazwie.
fn data_files(toc: &Path) -> Vec<(PathBuf, u64, usize)> {
    let Some(dir) = toc.parent() else { return Vec::new() };
    let Ok(list) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(PathBuf, u64, usize)> = list
        .flatten()
        .take(MAX_FILES * 4)
        .filter_map(|e| {
            let slot = data_slot(&e.file_name().to_string_lossy())?;
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            Some((e.path(), meta.len(), slot))
        })
        .collect();
    out.sort_by_key(|(p, ..)| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    out.truncate(MAX_FILES);
    out
}

fn find_exe(game: &Path) -> Option<String> {
    EXES.iter().find(|e| game.join(e).is_file()).map(|e| e.to_string())
}

/// Gra Snowdrop z TOC folderu "helix" (sprawdzany tylko nagłówek).
pub fn detect(game: &Path) -> Option<Detected> {
    let tocs = find_tocs(game);
    let (_, base) = tocs.iter().find(|(c, _)| c.eq_ignore_ascii_case(HELIX))?;
    toc_header(base)?;
    let mut files = Vec::new();
    for (_, toc) in &tocs {
        let Ok(meta) = std::fs::metadata(toc) else { continue };
        if toc != base && toc_header(toc).is_none() {
            continue;
        }
        files.push((toc.clone(), meta.len()));
        files.extend(data_files(toc).into_iter().map(|(p, len, _)| (p, len)));
    }
    Some(Detected {
        engine: "Snowdrop",
        version: Some(format!("SDF {HELIX}")),
        api: "DirectX 12",
        files,
        exe: find_exe(game),
    })
}

// ---------------------------------- odczyt -------------------------------------

/// Okno odczytu pliku: bajty od położenia, najwyżej `WINDOW_MAX`, mniej na
/// końcu. Błąd odczytu: puste wyniki i `failed`.
struct Window {
    file: File,
    len: u64,
    start: u64,
    buf: Vec<u8>,
    chunk: usize,
    failed: bool,
}

impl Window {
    fn new(file: File, chunk: usize) -> Option<Window> {
        let len = file.metadata().ok()?.len();
        Some(Window { file, len, start: 0, buf: Vec::new(), chunk, failed: false })
    }

    fn at(&mut self, pos: u64, n: usize) -> &[u8] {
        if self.failed || pos >= self.len {
            return &[];
        }
        let end = pos.saturating_add(n.min(WINDOW_MAX) as u64).min(self.len);
        if pos < self.start || end > self.start + self.buf.len() as u64 {
            let size = (n.min(WINDOW_MAX).max(self.chunk) as u64).min(self.len - pos) as usize;
            self.buf.resize(size, 0);
            self.start = pos;
            if read_at(&self.file, pos, &mut self.buf).is_err() {
                self.failed = true;
                self.buf.clear();
                return &[];
            }
        }
        let at = (pos - self.start) as usize;
        &self.buf[at..at + (end - pos) as usize]
    }
}

// ----------------------------------- Oodle -------------------------------------

/// Długość strony Oodle z jej nagłówka (Kraken, Mermaid/Selkie, Leviathan;
/// nowy strumień), inaczej 0.
fn page_len(h: &[u8]) -> usize {
    if h.len() < 5 || h[0] != 0x8C || !matches!(h[1] & 0x7F, 6 | 10 | 12) {
        return 0;
    }
    let size = (h[2] as usize & 3) << 16 | (h[3] as usize) << 8 | h[4] as usize;
    if size == 0x3FFFF {
        return 0;
    }
    5 + (h[1] >> 7) as usize * 3 + size + 1
}

/// Strona może zaczynać zasób shadera: pierwsze bajty zasobu za 3 B nagłówka
/// kawałka LZ albo od razu.
fn starts(h: &[u8]) -> bool {
    let Some(&dec) = h.get(1) else { return false };
    let at = 5 + (dec >> 7) as usize * 3;
    let head = &PREFIX[..8];
    h.get(at + 3..).is_some_and(|s| s.starts_with(head)) || h.get(at..).is_some_and(|s| s.starts_with(head))
}

/// Rozpakowuje stronę do `dst` (crate oozextract): liczba bajtów albo
/// `None`. Przy błędzie początek `dst` bywa już zapisany. Sumy kontrolne
/// (3 B za nagłówkiem kwantu) są usuwane - dekoder ich nie czyta. Panika
/// dekodera na złych danych jest łapana.
fn decode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    if src.len() < 6 {
        return None;
    }
    let stripped;
    let src = if src[1] & 0x80 != 0 {
        let rest = src.get(8..)?;
        let mut v = Vec::with_capacity(5 + rest.len());
        v.extend_from_slice(&[src[0], src[1] & 0x7F, src[2], src[3], src[4]]);
        v.extend_from_slice(rest);
        stripped = v;
        &stripped[..]
    } else {
        src
    };
    thread_local! {
        static OOZ: std::cell::RefCell<Option<oozextract::Extractor>> = const { std::cell::RefCell::new(None) };
    }
    OOZ.with(|x| {
        let mut slot = x.borrow_mut();
        let ex = slot.get_or_insert_with(oozextract::Extractor::new);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ex.read_from_slice(src, dst))) {
            Ok(Ok(n)) => Some(n),
            Ok(Err(_)) => None,
            Err(_) => {
                *slot = None; // stan po panice niepewny - nowy dekoder
                None
            }
        }
    })
}

// ------------------------------- kontenery ------------------------------------

/// Stałe MD5 (sinus), liczone raz.
fn md5_k() -> &'static [u32; 64] {
    static K: std::sync::OnceLock<[u32; 64]> = std::sync::OnceLock::new();
    K.get_or_init(|| std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32))
}

fn md5_block(st: &mut [u32; 4], block: &[u8]) {
    const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let k = md5_k();
    let m: [u32; 16] = std::array::from_fn(|i| le32(block, 4 * i).unwrap_or(0));
    let [mut a, mut b, mut c, mut d] = *st;
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(f.rotate_left(S[(i / 16) * 4 + i % 4]));
    }
    for (s, v) in st.iter_mut().zip([a, b, c, d]) {
        *s = s.wrapping_add(v);
    }
}

/// Skrót kontenera DXBC (MD5 z własnym dopełnieniem: liczba bitów na
/// początku ostatniego bloku, (bity >> 2) | 1 na końcu) - jak w nagłówku.
fn dxbc_digest(c: &[u8]) -> [u8; 16] {
    let src = c.get(20..).unwrap_or(&[]);
    let n = src.len();
    let mut st = [0x6745_2301u32, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];
    let full = n - n % 64;
    for b in src[..full].as_chunks::<64>().0 {
        md5_block(&mut st, b);
    }
    let left = &src[full..];
    let bits = (n as u32).wrapping_mul(8);
    let mut blk = [0u8; 64];
    if left.len() >= 56 {
        blk[..left.len()].copy_from_slice(left);
        blk[left.len()] = 0x80;
        md5_block(&mut st, &blk);
        blk = [0; 64];
        blk[..4].copy_from_slice(&bits.to_le_bytes());
    } else {
        blk[..4].copy_from_slice(&bits.to_le_bytes());
        blk[4..4 + left.len()].copy_from_slice(left);
        blk[4 + left.len()] = 0x80;
    }
    blk[60..].copy_from_slice(&((bits >> 2) | 1).to_le_bytes());
    md5_block(&mut st, &blk);
    let mut out = [0u8; 16];
    for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(st) {
        o.copy_from_slice(&s.to_le_bytes());
    }
    out
}

/// Cały kontener z kodem programu i zgodnym skrótem (zerowy - bez walidacji).
fn valid(c: &[u8]) -> bool {
    Container::parse(c).is_some_and(|x| x.bytes.len() == c.len() && x.kind().is_some())
        && (c[4..20] == [0u8; 16] || c[4..20] == dxbc_digest(c))
}

/// Zasób shadera pod `p`: kontener i położenie bajtów za nim.
fn asset(w: &mut Window, p: u64, page: &mut [u8]) -> Option<(Vec<u8>, u64)> {
    let h = w.at(p, 40).to_vec();
    let len = page_len(&h);
    if len == 0 {
        if h.len() < 40 || !h.starts_with(&PREFIX) {
            return None;
        }
        let raw = le32(&h, 32)? as usize + 8;
        if !(40..=MAX_ASSET).contains(&raw) {
            return None;
        }
        let stored = w.at(p, raw);
        if stored.len() < raw || !valid(&stored[8..]) {
            return None;
        }
        return Some((stored[8..].to_vec(), p + raw as u64));
    }
    let first = w.at(p, len).to_vec();
    if first.len() < len || page.len() < PAGE {
        return None;
    }
    let page = &mut page[..PAGE];
    page[..40].fill(0);
    let n = decode(&first, page);
    if !page.starts_with(&PREFIX) {
        return None;
    }
    let total = le32(page, 32)? as usize + 8;
    if !(40..=MAX_ASSET).contains(&total) {
        return None;
    }
    let next = p + len as u64;
    if total <= PAGE {
        // Strona kończy się na zasobie: dokładny rozmiar; dalej coś jeszcze: cała strona.
        let mut exact = vec![0u8; total];
        if decode(&first, &mut exact) == Some(total) && valid(&exact[8..]) {
            return Some((exact.split_off(8), next));
        }
        return (n == Some(PAGE) && valid(&page[8..total])).then(|| (page[8..total].to_vec(), next));
    }
    if n != Some(PAGE) {
        return None;
    }
    let mut out = page.to_vec();
    out.reserve(total - PAGE);
    let mut q = next;
    while out.len() < total {
        let want = (total - out.len()).min(PAGE);
        let done = out.len();
        out.resize(done + want, 0);
        let size = page_len(w.at(q, 8));
        if size > 0 {
            let src = w.at(q, size).to_vec();
            if src.len() == size && decode(&src, &mut out[done..]) == Some(want) {
                q += size as u64;
                continue;
            }
        }
        // Strona, która się nie skompresowała, leży wprost.
        let stored = w.at(q, want);
        if stored.len() < want {
            return None;
        }
        out[done..].copy_from_slice(stored);
        q += want as u64;
    }
    valid(&out[8..]).then(|| (out.split_off(8), q))
}

/// Strona znaleziona szukaniem jest stroną: rozpakowuje się do 64 KB,
/// zaczyna shader albo kończy się tam, gdzie następna strona lub plik.
fn resumes(w: &mut Window, p: u64, len: usize, page: &mut [u8]) -> bool {
    let src = w.at(p, len).to_vec();
    if src.len() < len || page.len() < PAGE {
        return false;
    }
    let page = &mut page[..PAGE];
    page[..PREFIX.len()].fill(0);
    if decode(&src, page) == Some(PAGE) || page.starts_with(&PREFIX) {
        return true;
    }
    let end = p + len as u64;
    end == w.len || page_len(w.at(end, 8)) > 0
}

/// Liczniki przejścia plików danych.
#[derive(Default)]
struct WalkCounts {
    broken: usize,
    cut: usize,
    not_data: usize,
}

/// Przechodzi plik danych: `found` z położeniem i kontenerem każdego zasobu
/// shadera, w kolejności pliku. `false` - plik ucięty limitem resynchronizacji.
fn walk(w: &mut Window, page: &mut [u8], n: &mut WalkCounts, found: &mut dyn FnMut(u64, Vec<u8>)) -> bool {
    if w.at(0, 4) != b"BERG" {
        n.not_data += 1;
        return true;
    }
    let mut p = 8u64;
    let mut resyncs = 0usize;
    let mut after_stored = false;
    while p < w.len {
        let mut len = page_len(w.at(p, 8));
        // Za zasobem zapisanym wprost mogą być dalsze surowe bajty: nagłówek
        // sprawdzany jak znaleziony szukaniem.
        if len > 0 && after_stored && !resumes(w, p, len, page) {
            len = 0;
        }
        after_stored = false;
        let h = w.at(p, 24).to_vec();
        if len > 0 && !starts(&h) {
            p += len as u64; // strona, nie początek shadera
            continue;
        }
        if len > 0 || h.starts_with(&PREFIX) {
            if let Some((c, next)) = asset(w, p, page) {
                found(p, c);
                after_stored = len == 0;
                p = next;
                continue;
            }
            n.broken += 1;
            if len > 0 {
                p += len as u64;
                continue;
            }
        }
        // Surowe bajty: następna strona, która się rozpakowuje, albo zasób wprost.
        p += 1;
        loop {
            let s = w.at(p, CHUNK);
            if s.len() < 8 {
                return true;
            }
            let Some(i) = s.iter().position(|&b| b == 0x8C || b == 0xE3) else {
                p += (s.len() - 7) as u64;
                continue;
            };
            p += i as u64;
            if w.at(p, PREFIX.len()).starts_with(&PREFIX) {
                break;
            }
            let k = page_len(w.at(p, 8));
            if k > 0 {
                resyncs += 1;
                if resyncs > MAX_RESYNCS {
                    n.cut += 1;
                    return false;
                }
                if resumes(w, p, k, page) {
                    break;
                }
            }
            p += 1;
        }
    }
    true
}

/// Bajty shadera czytane na żądanie: jeden zasób (jego strony) od nowa,
/// sprawdzany tożsamością kontenera.
fn lazy(path: Arc<PathBuf>, at: u64, size: usize, id: [u8; 16]) -> Bytes {
    Bytes::Lazy(Arc::new(move || {
        let mut w = Window::new(File::open(path.as_ref()).ok()?, 0)?;
        let mut page = vec![0u8; PAGE];
        let (c, _) = asset(&mut w, at, &mut page)?;
        let ok = c.len() == size && Container::parse(&c).is_some_and(|x| x.identity() == id);
        ok.then_some(c)
    }))
}

// ----------------------------- zapis przejścia ---------------------------------

/// Plik zapisu przejścia gry w danych programu (`Cache\snowdrop`).
fn record_path(game: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    super::games::path_key(game).hash(&mut h);
    Some(super::store::app_data_dir()?.join("Cache").join("snowdrop").join(format!("{:016x}.txt", h.finish())))
}

/// Liczba shaderów w pliku danych z ostatniego przejścia, po kluczu (nazwa,
/// rozmiar, skrót z TOC). Pusty przy braku albo uszkodzeniu.
fn read_record(path: Option<&Path>) -> HashMap<String, usize> {
    let mut known = HashMap::new();
    let Some(path) = path else { return known };
    if std::fs::metadata(path).map_or(true, |m| m.len() > MAX_RECORD) {
        return known;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return known };
    let mut lines = text.lines();
    if lines.next() != Some(RECORD_HEADER) {
        return known;
    }
    for line in lines {
        if let Some((key, n)) = line.rsplit_once('\t') {
            if let Ok(n) = n.parse() {
                known.insert(key.to_string(), n);
            }
        }
    }
    known
}

fn write_record(path: Option<&Path>, walked: &HashMap<String, usize>) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut keys: Vec<(&String, &usize)> = walked.iter().collect();
    keys.sort();
    let mut text = format!("{RECORD_HEADER}\n");
    for (k, n) in keys {
        text.push_str(&format!("{k}\t{n}\n"));
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ----------------------------- root signature ----------------------------------

const SRV: u32 = 0;
const UAV: u32 = 1;
const CBV: u32 = 2;
const SAMPLER: u32 = 3;
const UNBOUNDED: u32 = u32::MAX;
/// D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_VOLATILE.
const VOLATILE: u32 = 1;
/// Przestrzeń rozszerzeń producenta (AGS_DX12_SHADER_INSTRINSICS_SPACE_ID), u0.
const EXTENSION_SPACE: u32 = 0x7FFF_0ADE;
/// Domyślna maksymalna anizotropia (ustawienie gry).
const DEFAULT_ANISOTROPY: u32 = 16;

/// Zakres tablicy: typ, liczba, rejestr, przestrzeń, flagi, przesunięcie od początku tablicy.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Range {
    kind: u32,
    count: u32,
    base: u32,
    space: u32,
    flags: u32,
    offset: u32,
}

/// Parametr (widoczny dla wszystkich etapów): główny CBV/SRV albo tablica.
#[derive(Clone, Debug, PartialEq)]
enum Param {
    Cbv { reg: u32, space: u32 },
    Srv { reg: u32, space: u32 },
    Table(Vec<Range>),
}

/// 32 samplery silnika s0-s31 (przestrzeń 1): filtr D3D12, adresowanie
/// U/V/W, czy bierze anizotropię z ustawień (inaczej 1), porównanie, kolor
/// brzegu.
const SAMPLERS: [(u32, u32, u32, u32, bool, u32, u32); 32] = [
    (0x55, 1, 1, 1, true, 1, 0),
    (0, 3, 3, 3, true, 1, 0),
    (0, 1, 1, 1, false, 1, 0),
    (0x14, 3, 3, 3, true, 1, 0),
    (0x55, 4, 4, 4, true, 1, 0),
    (0x15, 1, 3, 3, false, 1, 0),
    (0x15, 3, 3, 3, false, 1, 0),
    (0x94, 3, 3, 3, true, 5, 0),
    (0, 1, 1, 1, true, 1, 0),
    (0x15, 1, 1, 1, false, 1, 0),
    (0x55, 3, 3, 3, true, 1, 0),
    (0x14, 3, 3, 3, false, 1, 0),
    (0x14, 1, 1, 1, true, 1, 0),
    (0, 3, 3, 3, false, 1, 0),
    (0, 4, 4, 4, true, 1, 0),
    (0x80, 3, 3, 3, true, 5, 0),
    (0x55, 2, 2, 2, true, 1, 0),
    (0x55, 1, 3, 1, true, 1, 0),
    (0x55, 1, 3, 3, true, 1, 0),
    (0x55, 3, 3, 1, true, 1, 0),
    (0x55, 1, 1, 3, true, 1, 0),
    (0x55, 3, 3, 3, true, 1, 1),
    (0x55, 1, 4, 1, true, 1, 0),
    (0x55, 5, 5, 5, true, 1, 0),
    (1, 1, 1, 1, true, 1, 0),
    (0x55, 3, 1, 1, true, 1, 0),
    (0x55, 3, 1, 3, true, 1, 0),
    (0x14, 4, 3, 3, true, 1, 0),
    (0x55, 4, 4, 4, true, 1, 2),
    (0x14, 1, 3, 3, true, 1, 0),
    (0x14, 4, 4, 4, true, 1, 1),
    (0, 2, 2, 2, true, 1, 0),
];

/// Rozmiar tablicy zasobu MR_* o danym id (tekstury globalne 32-128).
fn texture_count(id: u32) -> u32 {
    match id {
        35 | 37 => 3,
        45 | 88 | 125 | 126 => 2,
        _ => 1,
    }
}

/// Root signature silnika (wersja 1.1, wszystko widoczne dla wszystkich
/// etapów; rejestr zasobu = id MR_* x 20): główny CBV b0 (przestrzeń 2);
/// tablica bindless (zakresy bez końca, po jednym na przestrzeń, i u0
/// rozszerzeń); CBV MR_ConstantBuffer; 12 globalnych CBV; 97 globalnych
/// tekstur; 13 globalnych buforów; 4 samplery; główne SRV (compute o jeden
/// więcej). Flagi: graficzna - układ wejścia, bez HS/DS/GS; compute - bez
/// etapów graficznych; obie bez AS i MS.
fn layout(compute: bool) -> (Vec<Param>, u32) {
    let v = |kind, count, base, space, offset| Range { kind, count, base, space, flags: VOLATILE, offset };
    let mut bindless = Vec::new();
    let spaces = |list: &[(u32, u32, u32)], out: &mut Vec<Range>| {
        for &(kind, first, n) in list {
            out.extend((0..n).map(|i| v(kind, UNBOUNDED, 0, first + i, 0)));
        }
    };
    spaces(&[(SRV, 600, 16), (SRV, 3380, 14), (SRV, 2900, 15), (UAV, 3340, 8), (UAV, 3360, 8)], &mut bindless);
    bindless.push(v(UAV, 1, 0, EXTENSION_SPACE, 0));
    spaces(&[(SRV, 3400, 64), (UAV, 3464, 64)], &mut bindless);
    let mut textures = Vec::new();
    let mut offset = 0;
    for id in 32..32 + 97 {
        let n = texture_count(id);
        textures.push(v(SRV, n, id * 20, 0, offset));
        offset += n;
    }
    let mut params = vec![
        Param::Cbv { reg: 0, space: 2 },
        Param::Table(bindless),
        Param::Table(vec![v(CBV, 4, 2600, 0, 0)]),
        Param::Table((0..12).map(|i| v(CBV, 1, (132 + i) * 20, 0, i)).collect()),
        Param::Table(textures),
        Param::Table((0..13).map(|i| v(SRV, 1, (153 + i) * 20, 0, i)).collect()),
        Param::Table(vec![Range { kind: SAMPLER, count: 4, base: 0, space: 0, flags: 0, offset: 0 }]),
    ];
    let roots: &[u32] = if compute { &[2940, 2960, 2980, 3000] } else { &[2940, 2960, 2980] };
    params.extend(roots.iter().map(|&reg| Param::Srv { reg, space: 0 }));
    let flags = if compute { 0x3E } else { 0x1D } | 0x300;
    (params, flags)
}

/// Indeksy w `Set12::signatures`.
const RS_GRAPHICS: usize = 0;
const RS_COMPUTE: usize = 1;

/// Pliki ustawień gracza (Dokumenty, też w OneDrive).
fn settings_files() -> Vec<PathBuf> {
    let tail = Path::new("Documents").join("My Games").join("Outlaws").join("graphic settings.cfg");
    let mut out = Vec::new();
    for var in ["USERPROFILE", "OneDrive"] {
        if let Some(base) = std::env::var_os(var) {
            out.push(PathBuf::from(base).join(&tail));
        }
    }
    if let Some(base) = std::env::var_os("USERPROFILE") {
        out.push(PathBuf::from(base).join("OneDrive").join(&tail));
    }
    out
}

/// `["max anisotropy"] = "Nx"` z tekstu ustawień (1, 2, 4, 8, 16).
fn parse_anisotropy(text: &str) -> Option<u32> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower.get(from..)?.find("\"max anisotropy\"") {
        let at = from + i;
        from = at + 1;
        if !lower[..at].trim_end().ends_with('[') {
            continue;
        }
        let rest = lower[at + 16..].trim_start();
        let Some(rest) = rest.strip_prefix(']').map(str::trim_start) else { continue };
        let Some(rest) = rest.strip_prefix('=').map(str::trim_start) else { continue };
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() || digits.len() > 2 || !rest[digits.len()..].starts_with("x\"") {
            continue;
        }
        return digits.parse().ok().filter(|n| matches!(n, 1 | 2 | 4 | 8 | 16));
    }
    None
}

/// Anizotropia z ustawień gracza (pierwsze 64 KB pliku), inaczej 16x.
fn anisotropy() -> u32 {
    settings_files()
        .iter()
        .find_map(|p| {
            let file = File::open(p).ok()?;
            let len = file.metadata().ok()?.len().min(1 << 16) as usize;
            let mut b = vec![0u8; len];
            read_at(&file, 0, &mut b).ok()?;
            parse_anisotropy(&String::from_utf8_lossy(&b))
        })
        .unwrap_or(DEFAULT_ANISOTROPY)
}

#[cfg(windows)]
fn serialize(params: &[Param], flags: u32, anisotropy: u32) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let ranges: Vec<Vec<D3D12_DESCRIPTOR_RANGE1>> = params
        .iter()
        .map(|p| match p {
            Param::Table(list) => list
                .iter()
                .map(|r| D3D12_DESCRIPTOR_RANGE1 {
                    RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(r.kind as i32),
                    NumDescriptors: r.count,
                    BaseShaderRegister: r.base,
                    RegisterSpace: r.space,
                    Flags: D3D12_DESCRIPTOR_RANGE_FLAGS(r.flags as i32),
                    OffsetInDescriptorsFromTableStart: r.offset,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let root = |reg: u32, space: u32| D3D12_ROOT_PARAMETER1_0 {
        Descriptor: D3D12_ROOT_DESCRIPTOR1 { ShaderRegister: reg, RegisterSpace: space, Flags: D3D12_ROOT_DESCRIPTOR_FLAGS(0) },
    };
    let d3d_params: Vec<D3D12_ROOT_PARAMETER1> = params
        .iter()
        .zip(&ranges)
        .map(|(p, range)| {
            let (kind, anon) = match p {
                Param::Cbv { reg, space } => (D3D12_ROOT_PARAMETER_TYPE_CBV, root(*reg, *space)),
                Param::Srv { reg, space } => (D3D12_ROOT_PARAMETER_TYPE_SRV, root(*reg, *space)),
                Param::Table(_) => (
                    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    D3D12_ROOT_PARAMETER1_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE1 {
                            NumDescriptorRanges: range.len() as u32,
                            pDescriptorRanges: range.as_ptr(),
                        },
                    },
                ),
            };
            D3D12_ROOT_PARAMETER1 { ParameterType: kind, Anonymous: anon, ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL }
        })
        .collect();
    let samplers: Vec<D3D12_STATIC_SAMPLER_DESC> = SAMPLERS
        .iter()
        .enumerate()
        .map(|(i, &(filter, u, v, w, aniso, compare, border))| D3D12_STATIC_SAMPLER_DESC {
            Filter: D3D12_FILTER(filter as i32),
            AddressU: D3D12_TEXTURE_ADDRESS_MODE(u as i32),
            AddressV: D3D12_TEXTURE_ADDRESS_MODE(v as i32),
            AddressW: D3D12_TEXTURE_ADDRESS_MODE(w as i32),
            MipLODBias: 0.0,
            MaxAnisotropy: if aniso { anisotropy } else { 1 },
            ComparisonFunc: D3D12_COMPARISON_FUNC(compare as i32),
            BorderColor: D3D12_STATIC_BORDER_COLOR(border as i32),
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
            ShaderRegister: i as u32,
            RegisterSpace: 1,
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        })
        .collect();
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: d3d_params.len() as u32,
                pParameters: d3d_params.as_ptr(),
                NumStaticSamplers: samplers.len() as u32,
                pStaticSamplers: samplers.as_ptr(),
                Flags: D3D12_ROOT_SIGNATURE_FLAGS(flags as i32),
            },
        },
    };
    let mut blob: Option<ID3DBlob> = None;
    let mut error: Option<ID3DBlob> = None;
    // SAFETY: wskaźniki w `desc` prowadzą do żyjących wektorów.
    let result = unsafe { D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut error)) };
    let bytes = |b: &ID3DBlob| -> Vec<u8> {
        // SAFETY: blob z D3D12.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec() }
    };
    match (result, blob) {
        (Ok(()), Some(b)) => Ok(bytes(&b)),
        _ => Err(error
            .map(|e| String::from_utf8_lossy(&bytes(&e)).trim_end_matches('\0').to_owned())
            .unwrap_or_else(|| "D3D12SerializeVersionedRootSignature".into())),
    }
}

/// Obie root signature silnika: [graficzna, compute].
#[cfg(windows)]
fn signatures(anisotropy: u32) -> Result<Vec<Vec<u8>>, String> {
    [false, true]
        .iter()
        .map(|&compute| {
            let (params, flags) = layout(compute);
            serialize(&params, flags, anisotropy)
        })
        .collect()
}

#[cfg(not(windows))]
fn signatures(_anisotropy: u32) -> Result<Vec<Vec<u8>>, String> {
    Err("root signature tylko w Windows / root signatures need Windows".into())
}

// ----------------------------------- indeks -------------------------------------

/// Shadery DirectX 12 gry: pula graficzna (VS, PS) i compute z root
/// signature silnika.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    index_with(game, say, record_path(game).as_deref())
}

fn index_with(game: &Path, say: &dyn Fn(String), record: Option<&Path>) -> Result<Set12, String> {
    let tocs = find_tocs(game);
    let (_, base) = tocs
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(HELIX))
        .ok_or_else(|| format!("{HELIX}\\{TOC}: brak / missing"))?;
    let base_hashes = toc_hashes(base)?;
    let aniso = anisotropy();
    let mut set = Set12 { signatures: signatures(aniso)?, ..Set12::default() };
    let mut n = WalkCounts::default();
    let (mut unreadable, mut other_stage, mut over_cap, mut skipped_tocs) = (0usize, 0usize, 0usize, 0usize);
    let mut first_unreadable: Option<String> = None;

    // Klucz pliku: (folder dodatku\)nazwa, rozmiar, skrót slotu z TOC.
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for (code, toc) in &tocs {
        let hashes = if toc == base {
            base_hashes.clone()
        } else {
            match toc_hashes(toc) {
                Ok(h) => h,
                Err(_) => {
                    skipped_tocs += 1;
                    continue;
                }
            }
        };
        let scope = if toc == base { String::new() } else { format!("{code}\\") };
        for (path, size, slot) in data_files(toc) {
            let name = path.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            let hash = hashes.get(slot).copied().unwrap_or(0);
            files.push((path, format!("{scope}{name}\t{size}\t{hash:016x}")));
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    files.truncate(MAX_FILES);
    let known = read_record(record);
    let todo: Vec<&(PathBuf, String)> = files.iter().filter(|(_, k)| known.get(k) != Some(&0)).collect();

    let mut ids: HashMap<([u8; 16], usize), usize> = HashMap::new();
    let (mut gfx, mut cs) = (Vec::new(), Vec::new());
    let mut walked: HashMap<String, usize> = HashMap::new();
    let mut page = vec![0u8; PAGE];
    let mut bytes = 0u64;
    let mut with_shaders = 0usize;
    for (i, (path, key)) in todo.iter().enumerate() {
        say(format!("pliki danych / data files: {i}/{}", todo.len()));
        let Some(mut w) = File::open(path).ok().and_then(|f| Window::new(f, CHUNK)) else {
            unreadable += 1;
            first_unreadable.get_or_insert_with(|| path.display().to_string());
            continue;
        };
        bytes += w.len;
        let shared = Arc::new(path.clone());
        let mut count = 0usize;
        let whole = walk(&mut w, &mut page, &mut n, &mut |at, c| {
            count += 1;
            let Some(parsed) = Container::parse(&c) else { return };
            let pool = match parsed.kind() {
                Some(Kind::Vertex | Kind::Pixel) => &mut gfx,
                Some(Kind::Compute) => &mut cs,
                _ => {
                    other_stage += 1;
                    return;
                }
            };
            let key = (parsed.identity(), c.len());
            if let Some(&id) = ids.get(&key) {
                pool.push(id);
                return;
            }
            if set.shaders.len() >= MAX_SHADERS {
                over_cap += 1;
                return;
            }
            set.shaders.push(lazy(shared.clone(), at, c.len(), key.0));
            ids.insert(key, set.shaders.len() - 1);
            pool.push(set.shaders.len() - 1);
        });
        if w.failed {
            unreadable += 1;
            first_unreadable.get_or_insert_with(|| path.display().to_string());
            continue; // nie zapisany: następnym razem od nowa
        }
        if whole {
            walked.insert(key.clone(), count);
        }
        if count > 0 {
            with_shaders += 1;
        }
    }
    say(format!("pliki danych / data files: {}/{}", todo.len(), todo.len()));
    for (_, key) in &files {
        if known.get(key) == Some(&0) {
            walked.entry(key.clone()).or_insert(0);
        }
    }
    if set.shaders.is_empty() {
        return Err(format!("{}: brak shaderów / no shaders", game.display()));
    }
    write_record(record, &walked);
    for (mut pool, rs) in [(gfx, RS_GRAPHICS), (cs, RS_COMPUTE)] {
        pool.sort_unstable();
        pool.dedup();
        if !pool.is_empty() {
            set.pools.push((pool, Some(rs)));
        }
    }

    let notes = &mut set.notes;
    let total = set.shaders.len();
    notes.push(format!(
        "{} plików danych, {} przejrzanych ({} MB), {with_shaders} z shaderami -> {total} shaderów; root signature silnika \
         (anizotropia {aniso}x) / {} data files, {} walked ({} MB), {with_shaders} with shaders -> {total} shaders; the engine's \
         root signatures (anisotropy {aniso}x)",
        files.len(),
        todo.len(),
        bytes >> 20,
        files.len(),
        todo.len(),
        bytes >> 20
    ));
    let extra = [
        (n.broken, "zasobów shadera nie rozpakowało się", "shader assets didn't decode"),
        (n.cut, "plików uciętych (za dużo surowych danych)", "files cut short (too much unpaged data)"),
        (n.not_data, "plików nie jest plikami danych", "files aren't data files"),
        (unreadable, "plików nie daje się czytać", "files don't read"),
        (other_stage, "shaderów innych etapów (GS, HS, DS, MS, ...) pominiętych", "shaders of other stages (GS, HS, DS, MS, ...) left out"),
        (over_cap, "shaderów ponad limit", "shaders over the cap"),
        (skipped_tocs, "TOC dodatków nieczytelnych", "add-on TOCs unreadable"),
    ];
    for (count, pl, en) in extra {
        if count > 0 {
            notes.push(format!("{count} {pl} / {count} {en}"));
        }
    }
    if let Some(f) = &first_unreadable {
        notes.push(format!("pierwszy nieczytelny / first unreadable: {f}"));
    }
    for line in &set.notes {
        say(line.clone());
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::{build, sig1};
    use super::*;

    fn shex(kind: u32, salt: u8) -> Vec<u8> {
        let mut b = ((kind << 16) | 0x51).to_le_bytes().to_vec();
        b.extend_from_slice(&[salt; 12]);
        b
    }

    fn vs(salt: u8) -> Vec<u8> {
        build(&[
            (b"ISG1", sig1(&[("POSITION", 0, 0, 3, 0, 0x7)])),
            (b"OSG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"SHEX", shex(1, salt)),
        ])
    }

    fn ps(salt: u8) -> Vec<u8> {
        build(&[
            (b"ISG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"OSG1", sig1(&[("SV_Target", 0, 64, 3, 0, 0xF)])),
            (b"SHEX", shex(0, salt)),
        ])
    }

    fn cs(salt: u8) -> Vec<u8> {
        build(&[(b"SHEX", shex(5, salt))])
    }

    /// Kontener z poprawnym skrótem w nagłówku.
    fn signed(mut c: Vec<u8>) -> Vec<u8> {
        let d = dxbc_digest(&c);
        c[4..20].copy_from_slice(&d);
        c
    }

    fn asset_bytes(c: &[u8]) -> Vec<u8> {
        let mut b = PREFIX[..8].to_vec();
        b.extend_from_slice(c);
        b
    }

    /// TOC: nagłówek, ustawienia, języki, rozmiary, znaczniki i skróty slotów.
    fn toc(version: u32, hashes: &[u64], settings: u32, signed: bool) -> Vec<u8> {
        let mut b = b"WEST".to_vec();
        for v in [version, 0, 0, 0, hashes.len() as u32, 0, settings] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0u8; 16]);
        b.extend_from_slice(b"massive\0");
        b.extend_from_slice(&[0u8; 32]);
        b.extend_from_slice(b"ubisoft\0");
        b.extend_from_slice(&[u8::from(signed), 1]);
        assert_eq!(b.len(), 98);
        if signed {
            b.extend_from_slice(&[0xAA; 0x140]);
        }
        b.extend(std::iter::repeat_n(0x11, 8 * settings as usize + 60));
        for _ in hashes {
            b.extend_from_slice(&123u32.to_le_bytes());
        }
        b.extend(std::iter::repeat_n(0x22, 48 * hashes.len()));
        for h in hashes {
            b.extend_from_slice(&h.to_le_bytes());
        }
        b.extend_from_slice(b"encrypted file table");
        b
    }

    /// Plik danych: "BERG", wersja, kawałki jeden za drugim.
    fn berg(parts: &[&[u8]]) -> Vec<u8> {
        let mut b = b"BERG".to_vec();
        b.extend_from_slice(&3u32.to_le_bytes());
        for p in parts {
            b.extend_from_slice(p);
        }
        b
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tc-snowdrop-{name}-{}", std::process::id()))
    }

    fn walk_bytes(data: &[u8], chunk: usize) -> (Vec<(u64, Vec<u8>)>, WalkCounts, bool) {
        let path = temp(&format!("walk-{chunk}-{}", data.len()));
        std::fs::write(&path, data).unwrap();
        let mut w = Window::new(File::open(&path).unwrap(), chunk).unwrap();
        let mut page = vec![0u8; PAGE];
        let mut n = WalkCounts::default();
        let mut found = Vec::new();
        let whole = walk(&mut w, &mut page, &mut n, &mut |at, c| found.push((at, c)));
        drop(w);
        let _ = std::fs::remove_file(&path);
        (found, n, whole)
    }

    #[test]
    fn reads_toc_header_and_hashes() {
        let dir = temp("toc");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sdf.sdftoc");
        for (settings, sig) in [(0, false), (3, true)] {
            std::fs::write(&path, toc(0x2A, &[7, 0xDEAD_BEEF_0000_0001], settings, sig)).unwrap();
            assert_eq!(toc_header(&path), Some((2, settings, sig)));
            assert_eq!(toc_hashes(&path).unwrap(), vec![7, 0xDEAD_BEEF_0000_0001]);
        }
        // Inna wersja, zły znacznik, ucięty plik - bez paniki.
        std::fs::write(&path, toc(0x30, &[1], 0, false)).unwrap();
        assert!(toc_header(&path).is_none() && toc_hashes(&path).is_err());
        let good = toc(0x29, &[1, 2, 3], 2, false);
        for cut in (0..good.len()).step_by(5) {
            std::fs::write(&path, &good[..cut]).unwrap();
            let _ = toc_hashes(&path);
        }
        let mut bad = good.clone();
        bad[0x30] = b'X';
        std::fs::write(&path, &bad).unwrap();
        assert!(toc_header(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_data_files() {
        assert_eq!(data_slot("sdf-a-0007.sdfdata"), Some(7));
        assert_eq!(data_slot("SDF-B-1234-en-us.SDFDATA"), Some(1234));
        for bad in ["sdf-a-007.sdfdata", "sdf-ab-0007.sdfdata", "sdf-a-0007-en.sdfdata", "sdf.sdftoc", "sdf-1-0007.sdfdata", "sdf-é"] {
            assert_eq!(data_slot(bad), None, "{bad}");
        }
    }

    #[test]
    fn reads_page_headers() {
        // Mermaid, 0x123 + 1 B spakowanych, bez i z sumami kontrolnymi.
        assert_eq!(page_len(&[0x8C, 10, 0x00, 0x01, 0x23]), 5 + 0x124);
        assert_eq!(page_len(&[0x8C, 0x8A, 0x03, 0x00, 0x00]), 8 + 0x30001);
        assert_eq!(page_len(&[0x8C, 10, 0x03, 0xFF, 0xFF]), 0);
        assert_eq!(page_len(&[0x8C, 5, 0, 0, 1]), 0);
        assert_eq!(page_len(&[0x8C, 10]), 0);
        let mut h = vec![0x8C, 10, 0, 0, 9, 0, 0, 0];
        h.extend_from_slice(&PREFIX[..8]);
        assert!(starts(&h));
        assert!(!starts(&h[..10]));
        // Śmieci zamiast strony: dekoder nie panikuje.
        let mut page = vec![0u8; PAGE];
        for junk in [&h[..], &[0x8C, 10, 0, 0, 3, 1, 2, 3, 4][..], &[0x8C, 0x86, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF][..]] {
            assert_ne!(decode(junk, &mut page), Some(PAGE));
        }
    }

    #[test]
    fn checks_dxbc_digest() {
        let c = signed(vs(1));
        assert!(valid(&c) && valid(&vs(1)));
        let mut bad = c.clone();
        bad[4] ^= 1;
        assert!(!valid(&bad));
        // Kontenery z bibliotek systemu (z prawdziwym skrótem), jeśli są.
        let mut checked = 0;
        for dll in ["d2d1.dll", "dwmcore.dll", "d3d10warp.dll"] {
            let Ok(b) = std::fs::read(Path::new(r"C:\Windows\System32").join(dll)) else { continue };
            let mut i = 0;
            while let Some(p) = b[i..].windows(4).position(|w| w == b"DXBC") {
                let at = i + p;
                i = at + 4;
                if let Some(c) = Container::parse(&b[at..]) {
                    if c.bytes[4..20] != [0u8; 16] {
                        assert_eq!(dxbc_digest(c.bytes), c.bytes[4..20], "{dll} @ {at}");
                        checked += 1;
                    }
                }
            }
        }
        eprintln!("sprawdzone skróty / digests checked: {checked}");
    }

    #[test]
    fn walks_stored_assets() {
        let (a, b, c) = (signed(vs(1)), ps(2), signed(cs(3)));
        let mut broken = signed(ps(4));
        broken[30] ^= 0x40; // skrót się nie zgadza
        let data = berg(&[
            b"raw bytes \xE3 \x8C\x0A\x00\x00\x02 xx",
            &asset_bytes(&a),
            &asset_bytes(&b),
            b"\x00\x01stored tail",
            &asset_bytes(&broken),
            &[0x8C, 0x0A, 0x00, 0x00, 0x04, 1, 2, 3, 4, 5, 0xE3, 0x02],
            &asset_bytes(&c),
            b"end",
        ]);
        for chunk in [0, 64, CHUNK] {
            let (found, n, whole) = walk_bytes(&data, chunk);
            assert!(whole);
            let got: Vec<&Vec<u8>> = found.iter().map(|(_, c)| c).collect();
            assert_eq!(got, vec![&a, &b, &c], "chunk {chunk}");
            assert_eq!(n.broken, 1);
            // Położenie wskazuje zasób, który czyta się ponownie.
            let path = temp(&format!("again-{chunk}"));
            std::fs::write(&path, &data).unwrap();
            let mut w = Window::new(File::open(&path).unwrap(), 0).unwrap();
            let mut page = vec![0u8; PAGE];
            for (at, c) in &found {
                assert_eq!(asset(&mut w, *at, &mut page).map(|x| x.0).as_ref(), Some(c));
            }
            drop(w);
            let _ = std::fs::remove_file(&path);
        }
        // Nie plik danych; ucięty plik nie panikuje.
        let (found, n, _) = walk_bytes(b"NOPE1234", 0);
        assert!(found.is_empty() && n.not_data == 1);
        for cut in (0..data.len()).step_by(11) {
            let _ = walk_bytes(&data[..cut], 0);
        }
    }

    #[test]
    fn parses_anisotropy_setting() {
        let cfg = "{\n  [\"Max Anisotropy\"] = \"8x\",\n  [\"other\"] = \"1\"\n}";
        assert_eq!(parse_anisotropy(cfg), Some(8));
        assert_eq!(parse_anisotropy("[ \"max anisotropy\" ]=\"16x\""), Some(16));
        assert_eq!(parse_anisotropy("[\"max anisotropy\"] = \"3x\""), None);
        assert_eq!(parse_anisotropy("\"max anisotropy\" = \"4x\""), None);
        assert_eq!(parse_anisotropy("[\"max anisotropy\"] = \"x\""), None);
        assert_eq!(parse_anisotropy("ąę [\"max anisotropy\"] = \"2x\""), Some(2));
    }

    #[test]
    fn lays_out_root_signatures() {
        let (gfx, gflags) = layout(false);
        let (comp, cflags) = layout(true);
        assert_eq!((gfx.len(), gflags), (10, 0x31D));
        assert_eq!((comp.len(), cflags), (11, 0x33E));
        let Param::Table(bindless) = &gfx[1] else { panic!() };
        assert_eq!(bindless.len(), 16 + 14 + 15 + 8 + 8 + 1 + 64 + 64);
        let Param::Table(textures) = &gfx[4] else { panic!() };
        assert_eq!(textures.len(), 97);
        let last = textures[96];
        assert_eq!((last.base, last.offset), (128 * 20, 96 + 2 + 2 + 1 + 1 + 1 + 1));
        assert_eq!(comp.last(), Some(&Param::Srv { reg: 3000, space: 0 }));
    }

    #[cfg(windows)]
    #[test]
    fn serializes_root_signatures() {
        let s = signatures(DEFAULT_ANISOTROPY).unwrap();
        assert_eq!(s.len(), 2);
        // Część RTS0: wersja 1.1, parametry, 32 statyczne samplery, flagi.
        for (blob, params, flags) in [(&s[RS_GRAPHICS], 10, 0x31D), (&s[RS_COMPUTE], 11, 0x33E)] {
            let rts0 = Container::parse(blob).and_then(|c| c.root_signature()).unwrap();
            assert_eq!(le32(rts0, 0), Some(2));
            assert_eq!(le32(rts0, 4), Some(params));
            assert_eq!(le32(rts0, 12), Some(32));
            assert_eq!(le32(rts0, 20), Some(flags));
        }
        assert_ne!(signatures(4).unwrap(), s);
    }

    #[cfg(windows)]
    #[test]
    fn indexes_synthetic_game() {
        let game = temp("game");
        let data = game.join("helix").join(r"sdf\pc\data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("sdf.sdftoc"), toc(0x2A, &[1, 2, 3], 1, false)).unwrap();
        let (v, p, c) = (signed(vs(1)), signed(ps(2)), signed(cs(3)));
        std::fs::write(data.join("sdf-a-0000.sdfdata"), berg(&[&asset_bytes(&v), b"..", &asset_bytes(&c)])).unwrap();
        std::fs::write(data.join("sdf-b-0001-en-us.sdfdata"), berg(&[b"xx", &asset_bytes(&p), &asset_bytes(&v)])).unwrap();
        std::fs::write(data.join("sdf-a-0002.sdfdata"), berg(&[b"no shaders here"])).unwrap();
        std::fs::write(data.join("notes.txt"), b"x").unwrap();
        // Dodatek z uszkodzonym TOC - pominięty.
        let dlc = game.join("dlc1").join(r"sdf\pc\data");
        std::fs::create_dir_all(&dlc).unwrap();
        std::fs::write(dlc.join("sdf.sdftoc"), b"WEST").unwrap();
        std::fs::write(dlc.join("sdf-a-0000.sdfdata"), berg(&[&asset_bytes(&c)])).unwrap();
        std::fs::write(game.join("Outlaws.exe"), b"MZ").unwrap();

        let d = detect(&game).unwrap();
        assert_eq!((d.engine, d.api, d.exe.as_deref()), ("Snowdrop", "DirectX 12", Some("Outlaws.exe")));
        assert_eq!(d.files.len(), 4);

        let record = game.join("record.txt");
        let set = index_with(&game, &|_| {}, Some(&record)).unwrap();
        assert_eq!(set.signatures.len(), 2);
        assert_eq!(set.shaders.len(), 3);
        assert!(set.pipelines.is_empty());
        assert_eq!(set.pools.len(), 2);
        let (gfx, rs) = &set.pools[0];
        assert_eq!((gfx.len(), *rs), (2, Some(RS_GRAPHICS)));
        assert_eq!(set.pools[1].1, Some(RS_COMPUTE));
        let mut got: Vec<Vec<u8>> = gfx.iter().map(|&i| set.shaders[i].get().unwrap()).collect();
        got.sort();
        let mut want = vec![v.clone(), p.clone()];
        want.sort();
        assert_eq!(got, want);
        assert_eq!(set.shaders[set.pools[1].0[0]].get().unwrap(), c);
        assert!(set.notes.iter().any(|n| n.contains("add-on TOCs unreadable")));

        // Zapis: plik bez shaderów pomijany następnym razem.
        let known = read_record(Some(&record));
        assert_eq!(known.len(), 3);
        assert_eq!(known.values().filter(|&&n| n == 0).count(), 1);
        let again = index_with(&game, &|_| {}, Some(&record)).unwrap();
        assert!(again.notes[0].contains("3 data files, 2 walked"), "{}", again.notes[0]);
        assert_eq!(read_record(Some(&record)), known);

        // Gra zmieniona po indeksie: bajty nie są zwracane.
        std::fs::write(data.join("sdf-a-0000.sdfdata"), berg(&[b"changed"])).unwrap();
        assert!(set.shaders.iter().any(|s| s.get().is_none()));
        let _ = std::fs::remove_dir_all(&game);

        assert!(detect(Path::new("Z:\\nie-ma-takiej-gry")).is_none());
        assert!(index(Path::new("Z:\\nie-ma-takiej-gry"), &|_| {}).is_err());
    }
}
