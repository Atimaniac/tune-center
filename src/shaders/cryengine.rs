//! CRYENGINE (Hunt: Showdown 1896 w DirectX 12): pamięć shaderów dostarczana
//! z grą. Tylko odczyt plików gry; nic nie jest odszyfrowywane.
//!
//! - `ShaderCache.pak` w folderze gry i w jego folderach pierwszego poziomu
//!   (np. `engine\shadercache.pak`) to zwykłe ZIP-y (wpisy zapisane albo
//!   DEFLATE). Paczka, która nie otwiera się jako ZIP (zaszyfrowana albo
//!   uszkodzona), jest liczona i pomijana.
//! - `shaders/cache/d3d12/cg?shaders/<efekt>@<wejście>` - plik CPCK (LE):
//!   "CPCK", i32 wersja, i32 liczba rekordów, u32 położenie katalogu; rekord
//!   to u32 CRC nazwy, u32 rozmiar (niższe 24 bity) z flagami, u32 położenie.
//!   Wpis: u32 rozmiar po rozpakowaniu i LZSS (albo kontener wprost); po
//!   rozpakowaniu 12 B nagłówka i jeden kontener DXBC/DXIL.
//! - `shaders/cache/d3d12/resourcelayoutbincache.txt` - układy zasobów,
//!   z których silnik buduje root signature; `pipelinestatebincache.txt` -
//!   zapisane potoki: którego układu używa który efekt.
//!
//! Shadery nie mają wbudowanej root signature (gdy mają - pule z RTS0).
//! Układ: slot = parametr pod swoim indeksem, w przestrzeni rejestrów równej
//! indeksowi; rejestr = miejsce wiązania w zestawie. Shader idzie pod każdy
//! układ potoków swojego efektu, do którego pasują jego wiązania (PSV0).
//! Układu, którego pamięć nie ma, użycza bliźniaczy potok (rekord równy poza
//! układem) albo układ najlepiej pasujący do shaderów. Statyczne samplery i
//! stany potoków wymagałyby zapisu z gry - samplery zestawów dostają własne
//! tablice, pary VS+PS łączy semantyka (pule).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{read_at, Container, Kind};
use super::readers::{Bytes, Detected, Pipeline12, Set12};

const ENGINE: &str = "CRYENGINE";
const PAK: &str = "shadercache.pak";
const LAYOUTS: &str = "shaders/cache/d3d12/resourcelayoutbincache.txt";
const PIPELINES: &str = "shaders/cache/d3d12/pipelinestatebincache.txt";
const BIN: &str = r"bin\win_x64";

/// Największy czytany plik CPCK (zmierzony największy: 34 MB).
const MAX_CPCK: u64 = 256 << 20;
/// Najwięcej rekordów katalogu jednego pliku CPCK.
const MAX_RECORDS: usize = 1 << 20;
/// Bajty przed kontenerem we wpisie, w których jest szukany (zmierzone: 12).
const MAX_HEADER: usize = 64;
/// Największy kontener.
const MAX_CONTAINER: usize = 64 << 20;
/// Najwięcej rekordów w całej grze (zmierzone: 52 873).
const MAX_INDEX_RECORDS: usize = 1 << 24;
/// ZIP: wpisy archiwum i rozmiar katalogu centralnego.
const MAX_ZIP_ENTRIES: usize = 1 << 20;
const MAX_DIRECTORY: u64 = 256 << 20;
/// Pliki układów (zmierzone: 188 KB) i potoków (zmierzone: 48 MB).
const MAX_LAYOUT_FILE: u64 = 64 << 20;
const MAX_PIPELINE_FILE: u64 = 256 << 20;
/// Najwięcej paczek i elementów układu.
const MAX_PAKS: usize = 64;
const MAX_LIST: usize = 255;
/// Układy z pamięci: tyle procent musi dać root signature, inaczej żaden.
const MIN_BUILT_PERCENT: usize = 95;
/// Dopasowanie układów: pary shader-układ zbierane i sprawdzenia razem.
const FIT_PAIRS: usize = 2_000_000;
const FIT_CHECKS: usize = 50_000_000;
/// `detect`: katalog centralny, wpisy CPCK, bajty i rekordy próbki.
const DETECT_DIRECTORY: u64 = 8 << 20;
const DETECT_FILES: usize = 8;
const DETECT_BYTES: u64 = 4 << 20;
const DETECT_TRIES: usize = 64;

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// Tekst dwujęzyczny.
fn both(pl: &str, en: &str) -> String {
    format!("{pl} / {en}")
}

// ------------------------------------ ZIP ------------------------------------

const EOCD: &[u8; 4] = b"PK\x05\x06";
const LOCATOR: &[u8; 4] = b"PK\x06\x07";
const EOCD64: &[u8; 4] = b"PK\x06\x06";
const CENTRAL: &[u8; 4] = b"PK\x01\x02";
const LOCAL: &[u8; 4] = b"PK\x03\x04";

/// Wpis archiwum z katalogu centralnego.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    /// Nazwa małymi literami, z `/`.
    name: String,
    flags: u16,
    method: u16,
    stored: u64,
    size: u64,
    /// Położenie nagłówka lokalnego.
    local: u64,
}

/// Koniec katalogu centralnego: (położenie, rozmiar); ZIP64, gdy jest
/// lokalizator przed nim.
fn end_record(file: &File, len: u64) -> Option<(u64, u64)> {
    let tail_len = len.min(22 + 0xFFFF + 20) as usize;
    if tail_len < 22 {
        return None;
    }
    let mut tail = vec![0u8; tail_len];
    read_at(file, len - tail_len as u64, &mut tail).ok()?;
    let at = (0..=tail_len - 22).rev().find(|&i| &tail[i..i + 4] == EOCD)?;
    let mut size = u64::from(le32(&tail, at + 12)?);
    let mut offset = u64::from(le32(&tail, at + 16)?);
    if at >= 20 && &tail[at - 20..at - 16] == LOCATOR {
        let z64 = le64(&tail, at - 12)?;
        let mut rec = [0u8; 56];
        if z64.checked_add(56).is_some_and(|end| end <= len) && read_at(file, z64, &mut rec).is_ok() && &rec[..4] == EOCD64 {
            size = le64(&rec, 40)?;
            offset = le64(&rec, 48)?;
        }
    }
    (offset.checked_add(size)? <= len).then_some((offset, size))
}

/// Rekordy katalogu centralnego, dopóki są całe i mają podpis.
fn parse_directory(b: &[u8], max: usize) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while let Some(head) = b.get(p..p + 46) {
        if &head[..4] != CENTRAL || out.len() >= max {
            break;
        }
        let field = |o: usize| le16(head, o).map(usize::from).unwrap_or(0);
        let (n, x, c) = (field(28), field(30), field(32));
        let end = p + 46 + n + x + c;
        let (Some(name), Some(extra)) = (b.get(p + 46..p + 46 + n), b.get(p + 46 + n..p + 46 + n + x)) else { break };
        if end > b.len() {
            break;
        }
        let mut e = Entry {
            name: String::from_utf8_lossy(name).replace('\\', "/").to_ascii_lowercase(),
            flags: le16(head, 8).unwrap_or(0),
            method: le16(head, 10).unwrap_or(0),
            stored: u64::from(le32(head, 20).unwrap_or(0)),
            size: u64::from(le32(head, 24).unwrap_or(0)),
            local: u64::from(le32(head, 42).unwrap_or(0)),
        };
        zip64_extra(extra, &mut e);
        out.push(e);
        p = end;
    }
    out
}

/// Pole dodatkowe ZIP64 (0x0001): rozmiar, rozmiar zapisany, położenie -
/// tylko te, które w rekordzie mają 0xFFFFFFFF.
fn zip64_extra(extra: &[u8], e: &mut Entry) {
    let mut p = 0usize;
    while let (Some(id), Some(n)) = (le16(extra, p), le16(extra, p + 2)) {
        let data = extra.get(p + 4..p + 4 + n as usize).unwrap_or(&[]);
        if id == 1 {
            let mut k = 0usize;
            for v in [&mut e.size, &mut e.stored, &mut e.local] {
                if *v == 0xFFFF_FFFF {
                    let Some(x) = le64(data, k) else { return };
                    *v = x;
                    k += 8;
                }
            }
            return;
        }
        p += 4 + n as usize;
    }
}

/// Początek danych wpisu (za nagłówkiem lokalnym).
fn data_start(file: &File, len: u64, e: &Entry) -> Option<u64> {
    let mut head = [0u8; 30];
    if e.local.checked_add(30)? > len {
        return None;
    }
    read_at(file, e.local, &mut head).ok()?;
    if &head[..4] != LOCAL {
        return None;
    }
    let data = e.local + 30 + u64::from(le16(&head, 26)?) + u64::from(le16(&head, 28)?);
    (data.checked_add(e.stored)? <= len).then_some(data)
}

/// Bajty wpisu (rozpakowane). `None`: zaszyfrowany, inna metoda, ponad
/// `max` albo dane uszkodzone.
fn read_entry(file: &File, len: u64, e: &Entry, max: u64) -> Option<Vec<u8>> {
    if e.flags & 1 != 0 || !(e.method == 0 || e.method == 8) || e.size > max || e.stored > max.saturating_mul(2) {
        return None;
    }
    let data = data_start(file, len, e)?;
    let mut stored = vec![0u8; e.stored as usize];
    read_at(file, data, &mut stored).ok()?;
    let out = if e.method == 0 { stored } else { super::inflate::inflate(&stored, e.size as usize)?.0 };
    (out.len() as u64 == e.size).then_some(out)
}

/// Otwarta paczka.
struct Pak {
    path: Arc<PathBuf>,
    file: File,
    len: u64,
    entries: Vec<Entry>,
}

impl Pak {
    /// `None` - nie ZIP (zaszyfrowany albo uszkodzony). `partial`: czyta
    /// najwyżej `max_dir` bajtów katalogu; inaczej większy to `None`.
    fn open(path: &Path, max_dir: u64, partial: bool) -> Option<Pak> {
        let file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        let (offset, size) = end_record(&file, len)?;
        if size > max_dir && !partial {
            return None;
        }
        let mut b = vec![0u8; size.min(max_dir) as usize];
        read_at(&file, offset, &mut b).ok()?;
        let entries = parse_directory(&b, MAX_ZIP_ENTRIES);
        if entries.is_empty() && size > 0 {
            return None;
        }
        Some(Pak { path: Arc::new(path.to_path_buf()), file, len, entries })
    }

    fn read(&self, e: &Entry, max: u64) -> Option<Vec<u8>> {
        read_entry(&self.file, self.len, e, max)
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Pliki CPCK DirectX 12: (indeks wpisu, efekt małymi literami). Nazwa
    /// obecna dwa razy jest pomijana.
    fn cg_files(&self) -> Vec<(usize, String)> {
        let mut count: HashMap<&str, usize> = HashMap::new();
        for e in &self.entries {
            *count.entry(e.name.as_str()).or_default() += 1;
        }
        let mut out: Vec<(usize, String)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| count.get(e.name.as_str()) == Some(&1))
            .filter_map(|(i, e)| cg_file(&e.name).map(|f| (i, f.split('@').next().unwrap_or(f).to_owned())))
            .collect();
        out.sort_by(|a, b| self.entries[a.0].name.cmp(&self.entries[b.0].name));
        out
    }
}

/// Nazwa pliku z `.../shaders/cache/d3d12/cg?shaders/<plik>` (? = v, p, g,
/// d, h, c); nazwa już małymi literami.
fn cg_file(name: &str) -> Option<&str> {
    let at = name.find("shaders/cache/d3d12/cg").filter(|&i| i == 0 || name.as_bytes()[i - 1] == b'/')?;
    let rest = &name[at + "shaders/cache/d3d12/cg".len()..];
    let stage = *rest.as_bytes().first()?;
    let file = rest.get(1..)?.strip_prefix("shaders/")?;
    (b"vpgdhc".contains(&stage) && !file.is_empty() && !file.contains('/')).then_some(file)
}

// ------------------------------------ CPCK -----------------------------------

/// Rekordy katalogu z danymi (położenie, rozmiar); `None` - nie CPCK albo
/// katalog poza plikiem.
fn records(f: &[u8]) -> Option<Vec<(u32, u32)>> {
    if f.len() < 16 || &f[..4] != b"CPCK" {
        return None;
    }
    let n = le32(f, 8)? as i32;
    let dir = le32(f, 12)? as usize;
    // Punkt wejścia bez kombinacji: katalog pod -1.
    if n == 0 {
        return Some(Vec::new());
    }
    let n = usize::try_from(n).ok().filter(|&n| n <= MAX_RECORDS)?;
    if dir < 16 || dir.checked_add(12 * n)? > f.len() {
        return None;
    }
    Some(
        (0..n)
            .filter_map(|i| {
                let at = dir + 12 * i;
                let size = le32(f, at + 4)? & 0xFF_FFFF;
                (size > 0).then_some((le32(f, at + 8)?, size))
            })
            .collect(),
    )
}

/// LZSS (Okumura) w wersji CRYENGINE: pierścień 4 KB spacji od 4078; bity
/// bajtu flag od najniższego: 1 - literał, 0 - dopasowanie z dwóch bajtów
/// (12 bitów położenia, górna połowa drugiego bajtu na górze; długość - 3 w
/// dolnej). Zwraca liczbę zapisanych bajtów (najwyżej `dst.len()`).
fn lzss(src: &[u8], dst: &mut [u8]) -> usize {
    let mut ring = [b' '; 4096];
    let (mut r, mut p, mut o) = (4096 - 18, 0usize, 0usize);
    let mut flags = 0u32;
    while o < dst.len() {
        flags >>= 1;
        if flags & 0x100 == 0 {
            let Some(&f) = src.get(p) else { break };
            p += 1;
            flags = u32::from(f) | 0xFF00;
        }
        if flags & 1 != 0 {
            let Some(&c) = src.get(p) else { break };
            p += 1;
            dst[o] = c;
            ring[r] = c;
            o += 1;
            r = (r + 1) & 4095;
            continue;
        }
        let (Some(&a), Some(&b)) = (src.get(p), src.get(p + 1)) else { break };
        p += 2;
        let i = usize::from(a) | (usize::from(b & 0xF0) << 4);
        let n = usize::from(b & 0x0F) + 3;
        for k in 0..n {
            if o >= dst.len() {
                break;
            }
            let c = ring[(i + k) & 4095];
            dst[o] = c;
            ring[r] = c;
            o += 1;
            r = (r + 1) & 4095;
        }
    }
    o
}

/// Kontener zaczynający się co 4 B w pierwszych `MAX_HEADER` bajtach.
fn find_container(b: &[u8]) -> Option<&[u8]> {
    (0..=MAX_HEADER)
        .step_by(4)
        .take_while(|&k| k + 32 <= b.len())
        .find_map(|k| Container::parse(&b[k..]).map(|c| c.bytes))
}

/// Kontener wpisu CPCK: zapisany wprost albo u32 rozmiar i LZSS.
fn decode(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 4 {
        return None;
    }
    if let Some(c) = find_container(data) {
        return Some(c.to_vec());
    }
    let n = le32(data, 0)? as usize;
    // Bajt flag i 8 dopasowań (17 B) daje najwyżej 144 B.
    if !(32..=MAX_CONTAINER + MAX_HEADER).contains(&n) || n > (data.len() - 4).saturating_mul(9) {
        return None;
    }
    let mut out = vec![0u8; n];
    (lzss(&data[4..], &mut out) == n).then_some(())?;
    find_container(&out).map(<[u8]>::to_vec)
}

/// Wiązania shadera z PSV0: (typ, przestrzeń, pierwszy, ostatni rejestr);
/// typ 1 sampler, 2 CBV, 3-5 SRV, 6-9 UAV; ostatni 0xFFFFFFFF - bez
/// ograniczenia. `None` - brak PSV0 albo wartość poza zakresem.
fn psv_bindings(c: &Container) -> Option<Vec<[u32; 4]>> {
    let psv = c.part(b"PSV0")?;
    let info = le32(psv, 0)? as usize;
    if info < 24 {
        return None;
    }
    let p = 4usize.checked_add(info)?;
    let n = le32(psv, p)? as usize;
    if n == 0 {
        return Some(Vec::new());
    }
    let size = le32(psv, p + 4)? as usize;
    let start = p + 8;
    if !(size == 16 || size == 24) || n > psv.len().saturating_sub(start) / size {
        return None;
    }
    (0..n)
        .map(|i| {
            let e = start + i * size;
            let b = [le32(psv, e)?, le32(psv, e + 4)?, le32(psv, e + 8)?, le32(psv, e + 12)?];
            ((1..=9).contains(&b[0]) && b[1] < 0xFFFF_FFF0 && b[3] >= b[2]).then_some(b)
        })
        .collect()
}

// ----------------------------------- układy ----------------------------------

/// Znacznik obiektu między polami.
const MARKER: [u8; 4] = [0x7F, 0xC1, 0xA4, 0xB1];

/// Nagłówek rekordu: ASCII "vvvv <16 hex klucz> tttt <4 hex długość>|".
/// (klucz, długość ciała); nagłówek ma 32 B.
fn record_header(b: &[u8]) -> Option<(u64, usize)> {
    let h = b.get(..32)?;
    if h[4] != b' ' || h[21] != b' ' || h[26] != b' ' || h[31] != b'|' {
        return None;
    }
    let hex = |s: &[u8]| std::str::from_utf8(s).ok().and_then(|t| u64::from_str_radix(t, 16).ok());
    Some((hex(&h[5..21])?, hex(&h[27..31])? as usize))
}

/// Ciała rekordów pliku, dopóki ramki się zgadzają: (klucz, ciało). Drugie:
/// czy plik przeczytano do końca.
fn record_bodies(file: &[u8]) -> (Vec<(u64, &[u8])>, bool) {
    let mut out = Vec::new();
    let mut p = 0usize;
    while let Some((key, len)) = file.get(p..).and_then(record_header) {
        let Some(body) = file.get(p + 32..p + 32 + len) else { break };
        out.push((key, body));
        p += 32 + len;
        if file.get(p) == Some(&b'\n') {
            p += 1;
        }
    }
    (out, p >= file.len())
}

/// Pola: u16 id, u8 długość (0xFE: dalej u16 długość) i wartość. `markers`:
/// znaczniki obiektu między polami są pomijane. `None` - pole poza końcem.
fn walk(b: &[u8], markers: bool) -> Option<Vec<(u16, &[u8])>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < b.len() {
        if markers && b[p..].starts_with(&MARKER) {
            p += 4;
            continue;
        }
        let id = le16(b, p)?;
        let mut n = usize::from(*b.get(p + 2)?);
        p += 3;
        if n == 0xFE {
            n = usize::from(le16(b, p)?);
            p += 2;
        }
        out.push((id, b.get(p..p + n)?));
        p += n;
    }
    Some(out)
}

/// Pola bez powtórzeń.
fn fields(b: &[u8]) -> Result<HashMap<u16, &[u8]>, &'static str> {
    let mut out = HashMap::new();
    for (id, v) in walk(b, false).ok_or("pole poza końcem / a field past the end")? {
        if out.insert(id, v).is_some() {
            return Err("pole dwa razy / a field twice");
        }
    }
    Ok(out)
}

fn get<'a>(f: &HashMap<u16, &'a [u8]>, id: u16) -> Result<&'a [u8], &'static str> {
    f.get(&id).copied().ok_or("brak pola / a field missing")
}

/// Liczba bez znaku o dokładnie `width` bajtach.
fn int(f: &HashMap<u16, &[u8]>, id: u16, width: usize) -> Result<u64, &'static str> {
    let v = get(f, id)?;
    if v.len() != width {
        return Err("pole innej szerokości / a field of another width");
    }
    let mut w = [0u8; 8];
    w[..width].copy_from_slice(v);
    Ok(u64::from_le_bytes(w))
}

/// Lista: u8 liczba, potem tyle pól (wartości).
fn list(b: &[u8]) -> Result<Vec<&[u8]>, &'static str> {
    let (&n, rest) = b.split_first().ok_or("lista bez liczby / a list without a count")?;
    let items = walk(rest, false).ok_or("pole poza końcem / a field past the end")?;
    if items.len() != usize::from(n) || items.len() > MAX_LIST {
        return Err("lista innej długości / a list of another length");
    }
    Ok(items.into_iter().map(|(_, v)| v).collect())
}

/// Wiązanie slotu: etapy (bity: 1 VS, 2 PS, 4 GS, 8 DS, 16 HS, 32 CS), rejestr,
/// typ (0 CBV, 1 SRV, 2 sampler, 3 UAV), flagi i zasób (bajt 0: rodzaj, 9 =
/// stałe; bajt 4: ich liczba).
#[derive(Clone, Debug, PartialEq)]
struct Binding {
    stages: u16,
    ty: u8,
    flags: u8,
    resource: u64,
}

/// Slot układu: indeks parametru, rodzaj (0 CBV, 1 SRV, 2 zestaw, 3 stałe).
#[derive(Clone, Debug, PartialEq)]
struct Slot {
    index: u8,
    kind: u8,
    bindings: Vec<Binding>,
}

#[derive(Clone, Debug, PartialEq)]
struct Layout {
    slots: Vec<Slot>,
}

impl Layout {
    fn size(&self) -> usize {
        self.slots.iter().map(|s| s.bindings.len()).sum()
    }
}

/// Ciało układu: 3C2F lista slotów; slot: 2ED1 {A35D rodzaj, 4138 indeks},
/// 7768 lista wiązań; wiązanie: 2ED1 {8587 etapy, CDCF rejestr, A35D typ,
/// CBFC = 0, F95D flagi}, 7768 {C532 zasób}.
fn parse_layout(body: &[u8]) -> Result<Layout, &'static str> {
    let body = body.strip_prefix(&MARKER[..]).unwrap_or(body);
    let rf = fields(body)?;
    let mut slots = Vec::new();
    for sv in list(get(&rf, 0x3C2F)?)? {
        let sf = fields(sv)?;
        let head = fields(get(&sf, 0x2ED1)?)?;
        let mut bindings = Vec::new();
        for bv in list(get(&sf, 0x7768)?)? {
            let bf = fields(bv)?;
            let point = fields(get(&bf, 0x2ED1)?)?;
            if int(&point, 0xFCCB, 4)? != 0 {
                return Err("pole CBFC różne od 0 / field CBFC isn't 0");
            }
            int(&point, 0xCFCD, 1)?;
            bindings.push(Binding {
                stages: int(&point, 0x8587, 2)? as u16,
                ty: int(&point, 0xA35D, 1)? as u8,
                flags: int(&point, 0x5DF9, 1)? as u8,
                resource: int(&fields(get(&bf, 0x7768)?)?, 0x32C5, 8)?,
            });
        }
        slots.push(Slot { index: int(&head, 0x4138, 1)? as u8, kind: int(&head, 0xA35D, 1)? as u8, bindings });
    }
    Ok(Layout { slots })
}

/// Układy z pliku: klucz -> układ; nieczytelne liczone w `bad`.
fn parse_layouts(file: &[u8], bad: &mut usize) -> BTreeMap<u64, Layout> {
    let mut out = BTreeMap::new();
    for (key, body) in record_bodies(file).0 {
        match parse_layout(body) {
            Ok(l) if !out.contains_key(&key) => {
                out.insert(key, l);
            }
            _ => *bad += 1,
        }
    }
    out
}

/// Zapisane potoki: efekt -> (układ, compute); grupy rekordów graficznych
/// równych poza układem (do bliźniaków); odrzucone rekordy według powodu.
#[derive(Default)]
struct Pipelines {
    used: HashMap<String, BTreeSet<(u64, bool)>>,
    /// Skrót opisu -> (układy spoza pamięci, układy z pamięci).
    groups: HashMap<(u64, usize), (BTreeSet<u64>, BTreeSet<u64>)>,
    rejected: BTreeMap<&'static str, usize>,
}

/// Rekord potoku: u32 przed pierwszym znacznikiem, potem pola i znaczniki.
/// Układ graficzny w 6CDB, compute w 1097, nazwa efektu w 602D; 2F2F to
/// flaga układu spoza pamięci.
fn parse_pipelines(file: &[u8], shipped: &BTreeMap<u64, Layout>) -> Pipelines {
    use std::hash::{Hash, Hasher};
    let mut out = Pipelines::default();
    let (bodies, whole) = record_bodies(file);
    for (_, body) in bodies {
        let start = body.get(..8.min(body.len())).and_then(|h| h.windows(4).position(|w| w == MARKER)).unwrap_or(0);
        let Some(top) = walk(&body[start..], true) else {
            *out.rejected.entry("ramka / framing").or_default() += 1;
            continue;
        };
        let layouts: Vec<&(u16, &[u8])> = top.iter().filter(|(id, _)| *id == 0x1097 || *id == 0x6CDB).collect();
        let names: Vec<&(u16, &[u8])> = top.iter().filter(|(id, _)| *id == 0x602D).collect();
        let why = match (layouts.as_slice(), names.as_slice()) {
            ([], _) => Some("brak układu / no layout"),
            ([_, _, ..], _) => Some("dwa układy / two layouts"),
            ([(_, v)], _) if v.len() != 8 => Some("układ nie 8 B / a layout not 8 bytes"),
            (_, [_]) => None,
            _ => Some("brak jednej nazwy efektu / not one effect name"),
        };
        if let Some(w) = why {
            *out.rejected.entry(w).or_default() += 1;
            continue;
        }
        let (field, value) = *layouts[0];
        let Some(id) = le64(value, 0) else { continue };
        let effect = String::from_utf8_lossy(names[0].1).trim_end_matches('\0').to_ascii_lowercase();
        out.used.entry(effect).or_default().insert((id, field == 0x1097));
        if field == 0x6CDB {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            let mut n = 0usize;
            for (fid, v) in top.iter().filter(|(fid, _)| *fid != 0x6CDB && *fid != 0x2F2F) {
                (fid, v).hash(&mut h);
                n += v.len() + 1;
            }
            let g = out.groups.entry((h.finish(), n)).or_default();
            let side = if shipped.contains_key(&id) { &mut g.1 } else { &mut g.0 };
            side.insert(id);
        }
    }
    if !whole {
        *out.rejected.entry("ucięty plik / truncated file").or_default() += 1;
    }
    out
}

// ------------------------------ root signature -------------------------------

/// Parametr root signature (wersja 1.0). Zakres tablicy: (typ, liczba,
/// rejestr, przestrzeń, przesunięcie w tablicy).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Param {
    Table { vis: u32, ranges: Vec<[u32; 5]> },
    Constants { vis: u32, space: u32, count: u32 },
    Cbv { vis: u32, space: u32 },
    Srv { vis: u32, space: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Desc {
    flags: u32,
    params: Vec<Param>,
}

const UNBOUNDED: u32 = u32::MAX;

/// D3D12_SHADER_VISIBILITY: jeden etap (VS 1, HS 2, DS 3, GS 4, PS 5), inaczej
/// wszystkie.
fn vis(stages: u16) -> u32 {
    match stages {
        1 => 1,
        2 => 5,
        4 => 4,
        8 => 3,
        16 => 2,
        _ => 0,
    }
}

/// Rejestr wiązania to jego miejsce w zestawie posortowanym po typie.
fn type_sorted(s: &Slot) -> bool {
    s.bindings.windows(2).all(|w| w[0].ty <= w[1].ty)
}

/// Flagi: układ wejścia, bez AS i MS, bez etapów, których żadne wiązanie nie
/// widzi (sterownik bierze je do klucza pamięci).
fn flags(l: &Layout) -> u32 {
    let stages = l.slots.iter().flat_map(|s| &s.bindings).fold(0u16, |m, b| m | b.stages);
    let mut f = 0x1 | 0x100 | 0x200;
    for (bit, deny) in [(1u16, 0x2u32), (16, 0x4), (8, 0x8), (4, 0x10), (2, 0x20)] {
        if stages & bit == 0 {
            f |= deny;
        }
    }
    f
}

/// Tablica z wiązań (wiązanie, rejestr) w przestrzeni `space`.
fn table(bs: &[(&Binding, u32)], space: u32) -> Result<Option<Param>, String> {
    if bs.is_empty() {
        return Ok(None);
    }
    let mut ranges = Vec::new();
    for (k, (b, reg)) in bs.iter().enumerate() {
        let count = match (b.flags, b.ty) {
            (0, _) => 1,
            (1 | 5, 1) => UNBOUNDED,
            _ => return Err(format!("flagi {} typu {} / flags {} on type {}", b.flags, b.ty, b.flags, b.ty)),
        };
        let ty = match b.ty {
            0 => 2,
            1 => 0,
            2 => 3,
            _ => 1,
        };
        ranges.push([ty, count, *reg, space, k as u32]);
    }
    Ok(Some(Param::Table { vis: vis(bs.iter().fold(0, |m, (b, _)| m | b.stages)), ranges }))
}

/// Root signature układu, jak tworzy ją renderer D3D12 silnika: slot pod
/// swoim indeksem i w swojej przestrzeni; główny CBV, SRV i stałe w
/// rejestrze 0; zestaw - tablica deskryptorów, a jego samplery osobna
/// tablica za ostatnim slotem.
fn build(l: &Layout) -> Result<Desc, String> {
    let mut slots: Vec<&Slot> = l.slots.iter().collect();
    slots.sort_by_key(|s| s.index);
    let mut params = Vec::new();
    let mut samplers = Vec::new();
    for (i, s) in slots.iter().enumerate() {
        let space = u32::from(s.index);
        if usize::from(s.index) != i {
            return Err(format!("luka w indeksach slotów przy {space} / slot indices gap at {space}"));
        }
        let Some(first) = s.bindings.first() else {
            return Err(format!("pusty slot {space} / empty slot {space}"));
        };
        if s.kind != 2 && s.bindings.len() != 1 {
            return Err(format!("slot {space}: kilka wiązań / several bindings"));
        }
        if !type_sorted(s) {
            return Err(format!("slot {space}: wiązania nie po typie / bindings out of type order"));
        }
        if (matches!(s.kind, 0 | 3) && first.ty != 0) || (s.kind == 1 && first.ty != 1) {
            return Err(format!("slot {space}: rodzaj {} z typem {} / kind {} with type {}", s.kind, first.ty, s.kind, first.ty));
        }
        if s.kind != 2 && first.flags != 2 {
            return Err(format!("slot {space}: flagi {} / flags {}", first.flags, first.flags));
        }
        let constants = ((first.resource >> 32) & 0xFF) as u32;
        if s.kind == 3 && (first.resource & 0xFF != 9 || !(1..=63).contains(&constants)) {
            return Err(format!("slot {space}: stałe / root constants {:x}", first.resource));
        }
        let v = vis(first.stages);
        match s.kind {
            0 => params.push(Param::Cbv { vis: v, space }),
            1 => params.push(Param::Srv { vis: v, space }),
            3 => params.push(Param::Constants { vis: v, space, count: constants }),
            2 => {
                let regs: Vec<(&Binding, u32)> = s.bindings.iter().zip(0u32..).collect();
                let res: Vec<_> = regs.iter().copied().filter(|(b, _)| b.ty != 2).collect();
                let smp: Vec<_> = regs.iter().copied().filter(|(b, _)| b.ty == 2).collect();
                match (table(&res, space)?, table(&smp, space)?) {
                    (Some(r), Some(t)) => {
                        params.push(r);
                        samplers.push(t);
                    }
                    (Some(p), None) | (None, Some(p)) => params.push(p),
                    (None, None) => {}
                }
            }
            k => return Err(format!("slot {space}: nieznany rodzaj {k} / unknown kind {k}")),
        }
    }
    params.extend(samplers);
    Ok(Desc { flags: flags(l), params })
}

/// Serializuje opis w wersji 1.0 (bez statycznych samplerów).
#[cfg(windows)]
fn serialize(d: &Desc) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let ranges: Vec<Vec<D3D12_DESCRIPTOR_RANGE>> = d
        .params
        .iter()
        .map(|p| match p {
            Param::Table { ranges, .. } => ranges
                .iter()
                .map(|r| D3D12_DESCRIPTOR_RANGE {
                    RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(r[0] as i32),
                    NumDescriptors: r[1],
                    BaseShaderRegister: r[2],
                    RegisterSpace: r[3],
                    OffsetInDescriptorsFromTableStart: r[4],
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let params: Vec<D3D12_ROOT_PARAMETER> = d
        .params
        .iter()
        .zip(&ranges)
        .map(|(p, list)| {
            let (ty, v, anon) = match *p {
                Param::Table { vis, .. } => (
                    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    vis,
                    D3D12_ROOT_PARAMETER_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                            NumDescriptorRanges: list.len() as u32,
                            pDescriptorRanges: list.as_ptr(),
                        },
                    },
                ),
                Param::Constants { vis, space, count } => (
                    D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                    vis,
                    D3D12_ROOT_PARAMETER_0 {
                        Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: space, Num32BitValues: count },
                    },
                ),
                Param::Cbv { vis, space } => (
                    D3D12_ROOT_PARAMETER_TYPE_CBV,
                    vis,
                    D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: space } },
                ),
                Param::Srv { vis, space } => (
                    D3D12_ROOT_PARAMETER_TYPE_SRV,
                    vis,
                    D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 0, RegisterSpace: space } },
                ),
            };
            D3D12_ROOT_PARAMETER { ParameterType: ty, Anonymous: anon, ShaderVisibility: D3D12_SHADER_VISIBILITY(v as i32) }
        })
        .collect();
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_0,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_0: D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: params.len() as u32,
                pParameters: if params.is_empty() { std::ptr::null() } else { params.as_ptr() },
                NumStaticSamplers: 0,
                pStaticSamplers: std::ptr::null(),
                Flags: D3D12_ROOT_SIGNATURE_FLAGS(d.flags as i32),
            },
        },
    };
    let mut blob: Option<ID3DBlob> = None;
    let mut error: Option<ID3DBlob> = None;
    // SAFETY: wskaźniki w `desc` prowadzą do żyjących wektorów `params` i `ranges`.
    let result = unsafe { D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut error)) };
    let bytes = |b: &ID3DBlob| -> Vec<u8> {
        // SAFETY: bufor bloba z D3D12, ważny, dopóki blob żyje.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec() }
    };
    match (result, blob) {
        (Ok(()), Some(b)) => Ok(bytes(&b)),
        _ => Err(error
            .map(|e| String::from_utf8_lossy(&bytes(&e)).trim_end_matches('\0').to_owned())
            .unwrap_or_else(|| "D3D12SerializeVersionedRootSignature".into())),
    }
}

#[cfg(not(windows))]
fn serialize(_: &Desc) -> Result<Vec<u8>, String> {
    Err(both("root signature tylko w Windows", "root signatures need Windows"))
}

/// Zserializowane root signature bez powtórzeń.
#[derive(Default)]
struct Signatures {
    blobs: Vec<Vec<u8>>,
    by_desc: HashMap<Desc, Result<usize, String>>,
    by_blob: HashMap<Vec<u8>, usize>,
}

impl Signatures {
    fn of(&mut self, l: &Layout) -> Result<usize, String> {
        let d = build(l)?;
        if let Some(r) = self.by_desc.get(&d) {
            return r.clone();
        }
        let r = serialize(&d).map(|b| {
            *self.by_blob.entry(b.clone()).or_insert_with(|| {
                self.blobs.push(b);
                self.blobs.len() - 1
            })
        });
        self.by_desc.insert(d, r.clone());
        r
    }
}

// -------------------------------- dopasowanie --------------------------------

/// Shader do planu: etap, RTS0, wiązania z PSV0 i czy ma wyjścia.
struct Meta {
    kind: Kind,
    rts0: bool,
    psv: Option<Vec<[u32; 4]>>,
    outputs: bool,
}

fn stage_bit(k: Kind) -> u16 {
    match k {
        Kind::Vertex => 1,
        Kind::Pixel => 2,
        Kind::Geometry => 4,
        Kind::Domain => 8,
        Kind::Hull => 16,
        Kind::Compute => 32,
        Kind::Other => 0,
    }
}

/// Czy wiązania shadera mieszczą się w układzie: przestrzeń = indeks slotu,
/// rejestr = miejsce w zestawie; typ zgodny i etap widoczny. Główny CBV i
/// stałe to b0, główny SRV t0; zakres bez ograniczenia - jedyne wiązanie
/// zestawu (bindless SRV z flagą 1 albo 5).
fn fits(kind: Kind, psv: &[[u32; 4]], l: &Layout) -> bool {
    let bit = stage_bit(kind);
    if bit == 0 {
        return false;
    }
    psv.iter().all(|&[rt, space, lo, hi]| {
        let ty = match rt {
            1 => 2u8,
            2 => 0,
            3..=5 => 1,
            _ => 3,
        };
        let Some(slot) = l.slots.iter().find(|s| u32::from(s.index) == space) else { return false };
        let bs = &slot.bindings;
        if bs.is_empty() || !type_sorted(slot) {
            return false;
        }
        if hi == u32::MAX {
            return slot.kind == 2
                && bs.len() == 1
                && bs[0].ty == 1
                && matches!(bs[0].flags, 1 | 5)
                && lo == 0
                && ty == 1
                && bs[0].stages & bit != 0;
        }
        if slot.kind != 2 {
            return lo == 0 && hi == 0 && ty == u8::from(slot.kind == 1) && bs[0].stages & bit != 0;
        }
        bs.get(lo as usize..=hi as usize).is_some_and(|r| r.iter().all(|b| b.ty == ty && b.stages & bit != 0))
    })
}

/// Układ spoza pamięci: układy, które go zastępują (jeden albo dwa - wtedy
/// shader idzie pod oba), albo brak.
#[derive(Clone, Debug, PartialEq)]
enum Twin {
    Layouts { ids: Vec<u64>, by_fit: bool },
    None,
    TooMany,
}

/// Bliźniaki układów spoza pamięci: potok równy poza układem, którego układ
/// pamięć ma. Bez bliźniaka - układ (z tych, które dają root signature)
/// pasujący do największej liczby shaderów układu, najmniejszy; różne root
/// signature takich: jedna albo dwie.
fn resolve_twins(
    p: &Pipelines,
    layouts: &BTreeMap<u64, Layout>,
    built: &BTreeMap<u64, Result<usize, String>>,
    shaders: Option<&HashMap<u64, BTreeSet<usize>>>,
    metas: &[Meta],
) -> BTreeMap<u64, Twin> {
    let mut twins: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
    for (missing, shipped) in p.groups.values() {
        for m in missing {
            let t = twins.entry(*m).or_default();
            for s in shipped {
                if t.len() > 2 {
                    break;
                }
                t.insert(*s);
            }
        }
    }
    let buildable: Vec<(u64, &Layout, usize)> =
        layouts.iter().filter_map(|(id, l)| Some((*id, l, *built.get(id)?.as_ref().ok()?))).collect();
    let mut checks = 0usize;
    twins
        .into_iter()
        .map(|(id, t)| {
            let r = match t.len() {
                0 => {
                    let given = shaders.and_then(|s| s.get(&id)).filter(|s| !s.is_empty());
                    match given {
                        Some(sh) if checks.saturating_add(sh.len() * buildable.len()) <= FIT_CHECKS => {
                            checks += sh.len() * buildable.len();
                            best_fit(sh, &buildable, metas)
                        }
                        _ => Twin::None,
                    }
                }
                1 | 2 => Twin::Layouts { ids: t.into_iter().collect(), by_fit: false },
                _ => Twin::TooMany,
            };
            (id, r)
        })
        .collect()
}

fn best_fit(shaders: &BTreeSet<usize>, buildable: &[(u64, &Layout, usize)], metas: &[Meta]) -> Twin {
    let score: Vec<usize> = buildable
        .iter()
        .map(|(_, l, _)| {
            shaders.iter().filter(|&&s| metas[s].psv.as_ref().is_some_and(|b| fits(metas[s].kind, b, l))).count()
        })
        .collect();
    let top = score.iter().copied().max().unwrap_or(0);
    if top == 0 {
        return Twin::None;
    }
    let best: Vec<&(u64, &Layout, usize)> = buildable.iter().zip(&score).filter(|(_, n)| **n == top).map(|(b, _)| b).collect();
    let small = best.iter().map(|(_, l, _)| l.size()).min().unwrap_or(0);
    let mut sigs = HashSet::new();
    let ids: Vec<u64> = best.iter().filter(|(_, l, sig)| l.size() == small && sigs.insert(*sig)).map(|(id, ..)| *id).collect();
    if ids.len() > 2 {
        Twin::TooMany
    } else {
        Twin::Layouts { ids, by_fit: true }
    }
}

// --------------------------------- wykrywanie --------------------------------

/// Folder ochrony przed oszustwami - nic w nim nie jest czytane.
fn anti_cheat(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("anticheat") || n.contains("battleye")
}

/// `ShaderCache.pak` w folderze gry i w jego folderach pierwszego poziomu
/// (bez dowiązań i folderów ochrony przed oszustwami); najpierw `engine`.
fn find_paks(game: &Path) -> Vec<(PathBuf, u64)> {
    let mut dirs = vec![game.to_path_buf()];
    for e in std::fs::read_dir(game).into_iter().flatten().flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) && !anti_cheat(&e.file_name().to_string_lossy()) {
            dirs.push(e.path());
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(t) = e.file_type() else { continue };
            if !t.is_file() || !e.file_name().to_string_lossy().eq_ignore_ascii_case(PAK) {
                continue;
            }
            if let Some(len) = e.metadata().ok().map(|m| m.len()).filter(|&l| l >= 22) {
                out.push((e.path(), len));
            }
        }
    }
    let engine = |p: &Path| p.parent().and_then(Path::file_name).is_some_and(|n| n.eq_ignore_ascii_case("engine"));
    out.sort_by_key(|(p, _)| (!engine(p), p.to_string_lossy().to_lowercase()));
    out.truncate(MAX_PAKS);
    out
}

/// Plik .exe gry w `bin\win_x64`: `*Game.exe` (np. HuntGame.exe).
fn exe_name(game: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(game.join(BIN))
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with("game.exe") && !l.contains("crash") && !l.contains("launcher") && !l.contains("editor")
        })
        .collect();
    names.sort_by_key(|n| (n.len(), n.to_ascii_lowercase()));
    names.into_iter().next()
}

/// Czy paczki mają pliki CPCK DirectX 12 z shaderami: kilka najmniejszych
/// plików, łącznie do `DETECT_BYTES`.
fn sample(paks: &[(PathBuf, u64)]) -> bool {
    for (path, _) in paks {
        let Some(pak) = Pak::open(path, DETECT_DIRECTORY, true) else { continue };
        let mut files: Vec<&Entry> = pak.cg_files().into_iter().map(|(i, _)| &pak.entries[i]).collect();
        files.sort_by_key(|e| e.stored);
        let mut budget = DETECT_BYTES;
        for e in files.into_iter().take(DETECT_FILES) {
            if e.stored > budget {
                break;
            }
            budget -= e.stored;
            let Some(f) = pak.read(e, DETECT_BYTES) else { continue };
            let Some(dir) = records(&f) else { continue };
            for (offset, size) in dir.into_iter().take(DETECT_TRIES) {
                let Some(data) = f.get(offset as usize..(offset as usize).saturating_add(size as usize)) else { continue };
                if decode(data).is_some_and(|c| Container::parse(&c).is_some_and(|c| c.kind().is_some())) {
                    return true;
                }
            }
        }
    }
    false
}

/// Gra CRYENGINE z pamięcią shaderów DirectX 12: `ShaderCache.pak` z plikami
/// CPCK pod `d3d12` (wynik próbki zapamiętany przy stemplu paczek). Renderer
/// tylko D3D11 obok .exe - nie.
pub fn detect(game: &Path) -> Option<Detected> {
    let files = find_paks(game);
    if files.is_empty() {
        return None;
    }
    let bin = game.join(BIN);
    if bin.join("CryRenderD3D11.dll").is_file() && !bin.join("CryRenderD3D12.dll").is_file() {
        return None;
    }
    if !super::readers::cached_detect("cryengine", game, &files, || sample(&files)) {
        return None;
    }
    Some(Detected { engine: ENGINE, version: Some("CPCK".into()), api: "DirectX 12", files, exe: exe_name(game) })
}

// ----------------------------------- indeks ----------------------------------

/// Bajty shadera czytane na żądanie: wpis CPCK z paczki (zapisany - tylko
/// rekord; DEFLATE - cały plik), rozpakowany i sprawdzony tożsamością.
fn lazy(path: Arc<PathBuf>, e: Entry, rec: (u32, u32), id: [u8; 16], size: usize) -> Bytes {
    Bytes::Lazy(Arc::new(move || {
        let file = File::open(path.as_ref()).ok()?;
        let len = file.metadata().ok()?.len();
        let (at, n) = (u64::from(rec.0), u64::from(rec.1));
        if at.checked_add(n)? > e.size {
            return None;
        }
        let data = if e.method == 0 && e.flags & 1 == 0 {
            let mut b = vec![0u8; n as usize];
            read_at(&file, data_start(&file, len, &e)? + at, &mut b).ok()?;
            b
        } else {
            read_entry(&file, len, &e, MAX_CPCK)?.get(at as usize..(at + n) as usize)?.to_vec()
        };
        let c = decode(&data)?;
        let ok = Container::parse(&c).is_some_and(|x| x.bytes.len() == size && x.identity() == id);
        ok.then_some(c)
    }))
}

#[derive(Default)]
struct Counts {
    paks: usize,
    not_zip: usize,
    files: usize,
    entries: usize,
    unreadable: usize,
    not_cpck: usize,
    undecoded: usize,
    no_program: usize,
    capped: bool,
    bad_layouts: usize,
}

/// Plik z pierwszej paczki, która go ma.
fn find_entry(paks: &[Pak], name: &str, max: u64) -> Option<Vec<u8>> {
    paks.iter().find_map(|p| p.find(name).and_then(|e| p.read(e, max)))
}

/// Shadery DirectX 12 gry: pule (efekt, root signature układu), compute
/// osobno; shadery z RTS0 w pulach paczki i efektu.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let paths = find_paks(game);
    if paths.is_empty() {
        return Err(both("brak ShaderCache.pak", "no ShaderCache.pak"));
    }
    let mut n = Counts::default();
    let mut paks = Vec::new();
    for (p, _) in &paths {
        match Pak::open(p, MAX_DIRECTORY, false) {
            Some(pak) => paks.push(pak),
            None => n.not_zip += 1,
        }
    }
    n.paks = paks.len();
    if paks.is_empty() {
        return Err(both(
            &format!("{} paczek shaderów nie otwiera się jako ZIP (zaszyfrowane albo uszkodzone)", n.not_zip),
            &format!("{} shader paks don't open as ZIPs (encrypted or damaged)", n.not_zip),
        ));
    }
    let files: Vec<(usize, usize, String)> =
        paks.iter().enumerate().flat_map(|(k, p)| p.cg_files().into_iter().map(move |(e, f)| (k, e, f))).collect();
    let mut set = Set12::default();
    let mut metas: Vec<Meta> = Vec::new();
    let mut ids: HashMap<([u8; 16], usize), usize> = HashMap::new();
    let mut pools: BTreeMap<(usize, String), BTreeSet<usize>> = BTreeMap::new();
    let mut versions = BTreeSet::new();
    for (k, (pi, ei, effect)) in files.iter().enumerate() {
        if k % 256 == 0 {
            say(format!("pliki CPCK / CPCK files: {k}/{}", files.len()));
        }
        if n.entries >= MAX_INDEX_RECORDS {
            n.capped = true;
            break;
        }
        n.files += 1;
        let pak = &paks[*pi];
        let e = &pak.entries[*ei];
        let Some(f) = pak.read(e, MAX_CPCK) else {
            n.unreadable += 1;
            continue;
        };
        let Some(dir) = records(&f) else {
            n.not_cpck += 1;
            continue;
        };
        versions.insert(le32(&f, 4).unwrap_or(0) as i32);
        let mut seen: HashMap<(u32, u32), Option<usize>> = HashMap::new();
        for rec in dir {
            n.entries += 1;
            let slot = match seen.get(&rec) {
                Some(s) => *s,
                None => {
                    let data = f.get(rec.0 as usize..(rec.0 as usize).saturating_add(rec.1 as usize));
                    let s = match data.and_then(decode) {
                        None => {
                            n.undecoded += 1;
                            None
                        }
                        Some(bytes) => match Container::parse(&bytes).and_then(|c| Some((c.kind()?, c))) {
                            None => {
                                n.no_program += 1;
                                None
                            }
                            Some((kind, c)) => {
                                let key = (c.identity(), c.bytes.len());
                                Some(*ids.entry(key).or_insert_with(|| {
                                    metas.push(Meta {
                                        kind,
                                        rts0: c.root_signature().is_some(),
                                        psv: psv_bindings(&c),
                                        outputs: !c.outputs().is_empty(),
                                    });
                                    set.shaders.push(lazy(pak.path.clone(), e.clone(), rec, key.0, key.1));
                                    set.shaders.len() - 1
                                }))
                            }
                        },
                    };
                    seen.insert(rec, s);
                    s
                }
            };
            if let Some(s) = slot {
                pools.entry((*pi, effect.clone())).or_default().insert(s);
            }
        }
    }
    if set.shaders.is_empty() {
        return Err(both("brak shaderów w paczkach shaderów", "no shader in the shader paks"));
    }

    // Układy zasobów: root signature, gdy prawie wszystkie się budują.
    say(both("układy zasobów", "resource layouts"));
    let layouts = find_entry(&paks, LAYOUTS, MAX_LAYOUT_FILE).map(|b| parse_layouts(&b, &mut n.bad_layouts)).unwrap_or_default();
    let mut sigs = Signatures::default();
    let built: BTreeMap<u64, Result<usize, String>> = layouts.iter().map(|(id, l)| (*id, sigs.of(l))).collect();
    let built_ok = built.values().filter(|r| r.is_ok()).count();
    let use_layouts = !layouts.is_empty() && built_ok * 100 >= MIN_BUILT_PERCENT * layouts.len();
    let first_error = built.values().find_map(|r| r.as_ref().err().cloned());
    let pipes = if use_layouts {
        find_entry(&paks, PIPELINES, MAX_PIPELINE_FILE).map(|b| parse_pipelines(&b, &layouts)).unwrap_or_default()
    } else {
        Pipelines::default()
    };
    let no_rs = |s: &usize| !metas[*s].rts0;

    // Shadery układów spoza pamięci (do dopasowania bez bliźniaka).
    let mut by_unshipped: Option<HashMap<u64, BTreeSet<usize>>> = Some(HashMap::new());
    let mut pairs = 0usize;
    'gather: for ((_, effect), members) in &pools {
        let Some(used) = pipes.used.get(effect) else { continue };
        for s in members.iter().filter(|s| no_rs(s) && metas[**s].psv.is_some()) {
            for (id, compute) in used {
                if *compute != (metas[*s].kind == Kind::Compute) || layouts.contains_key(id) {
                    continue;
                }
                pairs += 1;
                if pairs > FIT_PAIRS {
                    by_unshipped = None;
                    break 'gather;
                }
                if let Some(m) = by_unshipped.as_mut() {
                    m.entry(*id).or_default().insert(*s);
                }
            }
        }
    }
    let twins = resolve_twins(&pipes, &layouts, &built, by_unshipped.as_ref(), &metas);

    // Root signature i układy do dopasowania dla układu potoków.
    let roots = |id: u64| -> Option<(Vec<usize>, Vec<&Layout>)> {
        if let Some(l) = layouts.get(&id) {
            return Some((vec![*built.get(&id)?.as_ref().ok()?], vec![l]));
        }
        let Some(Twin::Layouts { ids, .. }) = twins.get(&id) else { return None };
        let mut rs = Vec::new();
        let mut ls = Vec::new();
        for t in ids {
            rs.push(*built.get(t)?.as_ref().ok()?);
            ls.push(layouts.get(t)?);
        }
        rs.sort_unstable();
        rs.dedup();
        Some((rs, ls))
    };

    let mut graphics: BTreeMap<(String, usize), BTreeSet<usize>> = BTreeMap::new();
    let mut compute: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut embedded: BTreeMap<(usize, String), Vec<usize>> = BTreeMap::new();
    let mut placed: HashSet<usize> = HashSet::new();
    let mut why: HashMap<usize, &'static str> = HashMap::new();
    for ((pi, effect), members) in &pools {
        let cands = pipes.used.get(effect);
        for &s in members {
            let m = &metas[s];
            if m.rts0 {
                embedded.entry((*pi, effect.clone())).or_default().push(s);
                placed.insert(s);
                continue;
            }
            if !use_layouts {
                why.insert(s, "brak układów zasobów - root signature tylko z zapisu gry / no resource layouts - root signature only from a recording");
                continue;
            }
            let Some(psv) = &m.psv else {
                why.insert(s, "brak wiązań PSV0 / no PSV0 bindings");
                continue;
            };
            if matches!(m.kind, Kind::Vertex | Kind::Domain | Kind::Geometry) && !m.outputs {
                why.insert(s, "etap wierzchołków bez wyjść / a vertex stage without outputs");
                continue;
            }
            let is_cs = m.kind == Kind::Compute;
            let mut reason = "brak zapisanego potoku efektu / no cached pipeline of its effect";
            for (id, _) in cands.into_iter().flatten().filter(|(_, c)| *c == is_cs) {
                let Some((rs, ls)) = roots(*id) else {
                    if !reason.starts_with("nie pasuje") {
                        reason = "układ bez root signature (spoza pamięci, bez bliźniaka) / layout without a root signature (not shipped, no twin)";
                    }
                    continue;
                };
                if !ls.iter().all(|l| fits(m.kind, psv, l)) {
                    reason = "nie pasuje do układu / doesn't fit the layout";
                    continue;
                }
                placed.insert(s);
                for r in rs {
                    if is_cs {
                        compute.insert((s, r));
                    } else {
                        graphics.entry((effect.clone(), r)).or_default().insert(s);
                    }
                }
            }
            if !placed.contains(&s) {
                why.insert(s, reason);
            }
        }
    }
    why.retain(|s, _| !placed.contains(s));

    if use_layouts {
        set.signatures = sigs.blobs;
    }
    for ((_, rs), members) in graphics {
        set.pools.push((members.into_iter().collect(), Some(rs)));
    }
    for (cs, rs) in compute {
        set.pipelines.push(Pipeline12 { rs: Some(rs), stages: [None, None, None, None, None, Some(cs)] });
    }
    for (_, members) in embedded {
        set.pools.push((members, None));
    }

    // Uwagi.
    let stages = {
        let mut by: BTreeMap<&str, usize> = BTreeMap::new();
        for m in &metas {
            let name = match m.kind {
                Kind::Vertex => "VS",
                Kind::Pixel => "PS",
                Kind::Compute => "CS",
                Kind::Geometry => "GS",
                Kind::Hull => "HS",
                Kind::Domain => "DS",
                Kind::Other => "inne / other",
            };
            *by.entry(name).or_default() += 1;
        }
        by.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
    };
    let version = versions.iter().map(i32::to_string).collect::<Vec<_>>().join("/");
    let notes = &mut set.notes;
    notes.push(format!(
        "{} paczek, {} plików CPCK (wersja {version}), {} wpisów -> {} shaderów ({stages}) / \
         {} paks, {} CPCK files (version {version}), {} entries -> {} shaders ({stages})",
        n.paks, n.files, n.entries, metas.len(), n.paks, n.files, n.entries, metas.len()
    ));
    let skipped = [
        (n.not_zip, "paczek nie otwiera się jako ZIP (zaszyfrowane albo uszkodzone)", "paks don't open as ZIPs (encrypted or damaged)"),
        (n.unreadable, "plików CPCK nieczytelnych (inna kompresja, za duże)", "CPCK files unreadable (another compression, too large)"),
        (n.not_cpck, "plików to nie CPCK", "files aren't CPCK"),
        (n.undecoded, "wpisów nie rozpakowuje się do kontenera", "entries don't decode to a container"),
        (n.no_program, "kontenerów bez programu", "containers without a program"),
        (n.bad_layouts, "układów zasobów nieczytelnych", "resource layouts unreadable"),
    ];
    for (count, pl, en) in skipped {
        if count > 0 {
            notes.push(format!("{count} {pl}, pominięte / {count} {en}, left out"));
        }
    }
    if n.capped {
        notes.push(both("za dużo rekordów - reszta pominięta", "too many records - the rest left out"));
    }
    if layouts.is_empty() {
        notes.push(both("brak układów zasobów D3D12 w paczkach", "no D3D12 resource layouts in the paks"));
    } else if !use_layouts {
        let e = first_error.unwrap_or_default();
        notes.push(format!(
            "układy zasobów: root signature z {built_ok} z {} - za mało, nieużyte ({e}) / \
             resource layouts: {built_ok} of {} build a root signature - too few, unused ({e})",
            layouts.len(),
            layouts.len()
        ));
    } else {
        let resolved: Vec<&Twin> = twins.values().filter(|t| matches!(t, Twin::Layouts { .. })).collect();
        let by_fit = resolved.iter().filter(|t| matches!(t, Twin::Layouts { by_fit: true, .. })).count();
        notes.push(format!(
            "{} układów zasobów, {} root signature; {} z {} układów spoza pamięci zastąpionych bliźniakiem ({by_fit} dopasowaniem shaderów); {} shaderów pod układami / \
             {} resource layouts, {} root signatures; {} of {} unshipped layouts resolved by twins ({by_fit} by shader fit); {} shaders placed under layouts",
            layouts.len(),
            set.signatures.len(),
            resolved.len(),
            twins.len(),
            placed.len(),
            layouts.len(),
            set.signatures.len(),
            resolved.len(),
            twins.len(),
            placed.len()
        ));
        for (w, count) in &pipes.rejected {
            let (pl, en) = w.split_once(" / ").unwrap_or((w, w));
            notes.push(format!("{count} rekordów potoków pominiętych: {pl} / {count} pipeline records left out: {en}"));
        }
    }
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for w in why.values() {
        *reasons.entry(w).or_default() += 1;
    }
    for (w, count) in reasons {
        let (pl, en) = w.split_once(" / ").unwrap_or((w, w));
        notes.push(format!("{count} shaderów pominiętych: {pl} / {count} shaders left out: {en}"));
    }
    if set.pools.is_empty() && set.pipelines.is_empty() {
        return Err(set.notes.join("; "));
    }
    say(format!("pliki CPCK / CPCK files: {}/{}", files.len(), files.len()));
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::{build as container, dxil, sig1};
    use super::*;

    /// LZSS z samych literałów.
    fn literal_lzss(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in b.chunks(8) {
            out.push(0xFF);
            out.extend_from_slice(chunk);
        }
        out
    }

    /// PSV0: informacje 24 B, potem wiązania (typ, przestrzeń, od, do).
    fn psv(bindings: &[[u32; 4]]) -> Vec<u8> {
        let mut b = 24u32.to_le_bytes().to_vec();
        b.extend_from_slice(&[0u8; 24]);
        b.extend_from_slice(&(bindings.len() as u32).to_le_bytes());
        b.extend_from_slice(&16u32.to_le_bytes());
        for x in bindings {
            for v in x {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    fn shader(kind: u32, bindings: &[[u32; 4]], salt: u8) -> Vec<u8> {
        let mut parts: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"DXIL", dxil(kind)), (b"PSV0", psv(bindings))];
        match kind {
            1 => {
                parts.push((b"ISG1", sig1(&[("POSITION", 0, 0, 3, 0, 0x7)])));
                parts.push((b"OSG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])));
            }
            0 => {
                parts.push((b"ISG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])));
                parts.push((b"OSG1", sig1(&[("SV_Target", 0, 64, 3, 0, 0xF)])));
            }
            _ => {}
        }
        let mut c = container(&parts);
        c[4] = salt;
        c
    }

    /// Plik CPCK: wpisy (12 B nagłówka + kontener, LZSS), katalog na końcu.
    fn cpck(containers: &[Vec<u8>]) -> Vec<u8> {
        let mut f = b"CPCK".to_vec();
        f.extend_from_slice(&10u32.to_le_bytes());
        f.extend_from_slice(&(containers.len() as u32).to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        let mut dir = Vec::new();
        for (k, c) in containers.iter().enumerate() {
            let mut raw = vec![0u8; 12];
            raw.extend_from_slice(c);
            let mut entry = (raw.len() as u32).to_le_bytes().to_vec();
            entry.extend_from_slice(&literal_lzss(&raw));
            dir.extend_from_slice(&(k as u32).to_le_bytes());
            dir.extend_from_slice(&(entry.len() as u32 | 0x0100_0000).to_le_bytes());
            dir.extend_from_slice(&(f.len() as u32).to_le_bytes());
            f.extend_from_slice(&entry);
        }
        let at = f.len() as u32;
        f[12..16].copy_from_slice(&at.to_le_bytes());
        f.extend_from_slice(&dir);
        f
    }

    /// ZIP z wpisami zapisanymi (metoda 0).
    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let local = out.len() as u32;
            out.extend_from_slice(LOCAL);
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(CENTRAL);
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0u8; 12]);
            central.extend_from_slice(&local.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(EOCD);
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn field(id: u16, v: &[u8]) -> Vec<u8> {
        let mut b = id.to_le_bytes().to_vec();
        if v.len() < 0xFE {
            b.push(v.len() as u8);
        } else {
            b.push(0xFE);
            b.extend_from_slice(&(v.len() as u16).to_le_bytes());
        }
        b.extend_from_slice(v);
        b
    }

    fn list_of(items: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![items.len() as u8];
        for i in items {
            b.extend(field(0, i));
        }
        b
    }

    /// Wiązanie: (etapy, typ, flagi, zasób).
    fn binding(stages: u16, ty: u8, flags: u8, resource: u64, reg: u8) -> Vec<u8> {
        let point = [
            field(0x8587, &stages.to_le_bytes()),
            field(0xCFCD, &[reg]),
            field(0xA35D, &[ty]),
            field(0xFCCB, &[0; 4]),
            field(0x5DF9, &[flags]),
        ]
        .concat();
        [field(0x2ED1, &point), field(0x7768, &field(0x32C5, &resource.to_le_bytes()))].concat()
    }

    fn slot(index: u8, kind: u8, bindings: &[Vec<u8>]) -> Vec<u8> {
        let head = [field(0xA35D, &[kind]), field(0x4138, &[index])].concat();
        [field(0x2ED1, &head), field(0x7768, &list_of(bindings))].concat()
    }

    fn record(key: u64, body: &[u8]) -> Vec<u8> {
        let mut b = format!("vvvv {key:016x} tttt {:04x}|", body.len()).into_bytes();
        b.extend_from_slice(body);
        b.push(b'\n');
        b
    }

    /// Układ: slot 0 - główny CBV (VS, PS, CS), slot 1 - zestaw t0, t1 (PS),
    /// sampler s2 (PS) i może u3 (CS), slot 2 - bindless SRV (VS, PS, CS).
    fn layout_body(extra_uav: bool) -> Vec<u8> {
        let mut set = vec![binding(2, 1, 0, 0, 0), binding(2, 1, 0, 0, 1), binding(2, 2, 0, 0, 2)];
        if extra_uav {
            set.push(binding(32, 3, 0, 0, 3));
        }
        let slots = [slot(0, 0, &[binding(35, 0, 2, 0, 0)]), slot(1, 2, &set), slot(2, 2, &[binding(35, 1, 5, 0, 0)])];
        [MARKER.to_vec(), field(0x3C2F, &list_of(&slots))].concat()
    }

    /// Rekord potoku: efekt, układ (compute albo graficzny), flaga, opis.
    fn pipeline(effect: &str, layout: u64, compute: bool, flag: u8, desc: u8) -> Vec<u8> {
        [
            vec![1, 0, 0, 0],
            MARKER.to_vec(),
            field(0x602D, format!("{effect}\0").as_bytes()),
            MARKER.to_vec(),
            field(if compute { 0x1097 } else { 0x6CDB }, &layout.to_le_bytes()),
            field(0x2F2F, &[flag]),
            field(0x1111, &[desc; 3]),
        ]
        .concat()
    }

    #[test]
    fn lzss_literals_and_matches() {
        // 'a', 'b', potem dopasowanie 4 B od 4078 (nachodzi na siebie).
        let src = [0x03, b'a', b'b', 0xEE, 0xF1];
        let mut out = [0u8; 6];
        assert_eq!(lzss(&src, &mut out), 6);
        assert_eq!(&out, b"ababab");
        // Dopasowanie w początkowych spacjach pierścienia.
        let mut out = [0u8; 3];
        assert_eq!(lzss(&[0x00, 0x00, 0x00], &mut out), 3);
        assert_eq!(&out, b"   ");
        // Ucięte dane - mniej bajtów, bez paniki.
        let mut out = [0u8; 10];
        assert_eq!(lzss(&[0xFF, 1, 2], &mut out), 2);
    }

    #[test]
    fn reads_cpck_records_and_decodes_containers() {
        let a = shader(1, &[[2, 0, 0, 0]], 1);
        let b = shader(5, &[], 2);
        let f = cpck(&[a.clone(), b.clone()]);
        let dir = records(&f).unwrap();
        assert_eq!(dir.len(), 2);
        let get = |(o, s): (u32, u32)| decode(&f[o as usize..(o + s) as usize]);
        assert_eq!(get(dir[0]).unwrap(), a);
        assert_eq!(get(dir[1]).unwrap(), b);
        // Kontener zapisany wprost.
        assert_eq!(decode(&a).unwrap(), a);
        // Uszkodzone: zły nagłówek, katalog poza plikiem, rozmiar za duży.
        assert!(records(b"CPCX\0\0\0\0\x01\0\0\0\x10\0\0\0").is_none());
        let mut broken = f.clone();
        broken[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(records(&broken).is_none());
        assert!(decode(&[0xFF, 0xFF, 0xFF, 0x7F, 1, 2, 3]).is_none());
        assert_eq!(records(b"CPCK\x0a\0\0\0\0\0\0\0\xff\xff\xff\xff"), Some(Vec::new()));
        let c = Container::parse(&a).unwrap();
        assert_eq!(psv_bindings(&c), Some(vec![[2, 0, 0, 0]]));
    }

    #[test]
    fn matches_cg_file_names() {
        assert_eq!(cg_file("shaders/cache/d3d12/cgpshaders/illum@ps"), Some("illum@ps"));
        assert_eq!(cg_file("x/shaders/cache/d3d12/cgcshaders/a@cs"), Some("a@cs"));
        assert_eq!(cg_file("shaders/cache/d3d11/cgpshaders/illum@ps"), None);
        assert_eq!(cg_file("shaders/cache/d3d12/cgxshaders/a"), None);
        assert_eq!(cg_file("shaders/cache/d3d12/cgvshaders/a/b"), None);
        assert_eq!(cg_file("myshaders/cache/d3d12/cgvshaders/a"), None);
    }

    #[test]
    fn builds_root_signatures_from_layouts_and_fits_shaders() {
        let file = [record(7, &layout_body(false)), record(8, b"garbage")].concat();
        let mut bad = 0;
        let layouts = parse_layouts(&file, &mut bad);
        assert_eq!((layouts.len(), bad), (1, 1));
        let l = &layouts[&7];
        let d = build(l).unwrap();
        // Bez GS, HS i DS (0x4 | 0x8 | 0x10); CS nie ma flagi.
        assert_eq!(d.flags, 0x1 | 0x100 | 0x200 | 0x4 | 0x8 | 0x10);
        assert_eq!(
            d.params,
            vec![
                Param::Cbv { vis: 0, space: 0 },
                Param::Table { vis: 5, ranges: vec![[0, 1, 0, 1, 0], [0, 1, 1, 1, 1]] },
                Param::Table { vis: 0, ranges: vec![[0, UNBOUNDED, 0, 2, 0]] },
                Param::Table { vis: 5, ranges: vec![[3, 1, 2, 1, 0]] },
            ]
        );
        // PS: b0, t0-t1 i s2 w przestrzeni 1, bindless t0 w przestrzeni 2.
        let ps = [[2, 0, 0, 0], [3, 1, 0, 1], [1, 1, 2, 2], [3, 2, 0, u32::MAX]];
        assert!(fits(Kind::Pixel, &ps, l));
        // VS nie widzi zestawu (tylko PS), rejestr poza zestawem, zły typ.
        assert!(!fits(Kind::Vertex, &ps, l));
        assert!(!fits(Kind::Pixel, &[[3, 1, 0, 3]], l));
        assert!(!fits(Kind::Pixel, &[[6, 1, 0, 0]], l));
        assert!(!fits(Kind::Pixel, &[[2, 9, 0, 0]], l));
        assert!(fits(Kind::Vertex, &[[2, 0, 0, 0]], l));
        // Luka w indeksach slotów - bez root signature.
        let gap = Layout { slots: vec![Slot { index: 1, kind: 0, bindings: vec![Binding { stages: 1, ty: 0, flags: 2, resource: 0 }] }] };
        assert!(build(&gap).is_err());
        #[cfg(windows)]
        {
            let blob = serialize(&d).unwrap();
            // Kontener z częścią RTS0 wersji 1.0 i 4 parametrami.
            let rts0 = Container::parse(&blob).and_then(|c| c.part(b"RTS0")).unwrap();
            assert_eq!((le32(rts0, 0), le32(rts0, 4)), (Some(1), Some(4)));
            assert!(serialize(&Desc { flags: 0, params: vec![Param::Constants { vis: 0, space: 0, count: 4 }] }).is_ok());
        }
    }

    #[test]
    fn reads_pipeline_records_and_resolves_twins() {
        let shipped: BTreeMap<u64, Layout> = [(7u64, Layout { slots: Vec::new() })].into();
        let file = [
            record(1, &pipeline("Illum", 7, false, 0, 1)),
            record(2, &pipeline("Illum", 9, false, 1, 1)),
            record(3, &pipeline("Post", 11, true, 0, 2)),
            record(4, &[1, 0, 0, 0, 0x2D, 0x60, 9]),
        ]
        .concat();
        let p = parse_pipelines(&file, &shipped);
        assert_eq!(p.used["illum"], BTreeSet::from([(7, false), (9, false)]));
        assert_eq!(p.used["post"], BTreeSet::from([(11, true)]));
        assert_eq!(p.rejected.values().sum::<usize>(), 1);
        let built: BTreeMap<u64, Result<usize, String>> = [(7u64, Ok(0))].into();
        let twins = resolve_twins(&p, &shipped, &built, None, &[]);
        assert_eq!(twins.get(&9), Some(&Twin::Layouts { ids: vec![7], by_fit: false }));
        assert_eq!(twins.len(), 1);
    }

    /// Gra: `engine\shadercache.pak` (shadery, układy, potoki) i paczka,
    /// która nie jest ZIP-em.
    fn game(dir: &Path) {
        let vs = shader(1, &[[2, 0, 0, 0]], 1);
        let ps = shader(0, &[[2, 0, 0, 0], [3, 1, 0, 1], [1, 1, 2, 2]], 2);
        let cs = shader(5, &[[2, 0, 0, 0], [7, 1, 3, 3]], 3);
        let odd = shader(0, &[[2, 5, 0, 0]], 4);
        let layouts = [record(7, &layout_body(false)), record(8, &layout_body(true))].concat();
        let pipes = [
            record(1, &pipeline("Illum", 7, false, 0, 1)),
            record(2, &pipeline("Illum", 9, false, 1, 1)),
            record(3, &pipeline("Post", 8, true, 0, 2)),
        ]
        .concat();
        let vs_file = cpck(&[vs.clone(), vs]);
        let ps_file = cpck(&[ps, odd]);
        let cs_file = cpck(&[cs]);
        let pak = zip(&[
            ("Shaders/Cache/D3D12/CGVShaders/Illum@VS", &vs_file),
            ("Shaders/Cache/D3D12/CGPShaders/Illum@PS", &ps_file),
            ("Shaders/Cache/D3D12/CGCShaders/Post@Main", &cs_file),
            ("Shaders/Cache/D3D12/ResourceLayoutBinCache.txt", &layouts),
            ("Shaders/Cache/D3D12/PipelineStateBinCache.txt", &pipes),
        ]);
        std::fs::create_dir_all(dir.join("engine")).unwrap();
        std::fs::create_dir_all(dir.join("hunt")).unwrap();
        std::fs::create_dir_all(dir.join(BIN)).unwrap();
        std::fs::write(dir.join("engine").join("ShaderCache.pak"), pak).unwrap();
        std::fs::write(dir.join("hunt").join("shadercache.pak"), vec![0x5Au8; 4096]).unwrap();
        std::fs::write(dir.join(BIN).join("HuntGame.exe"), b"MZ").unwrap();
    }

    #[test]
    fn finds_paks_samples_and_names_the_exe() {
        let dir = std::env::temp_dir().join(format!("tc-cry-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(detect(&dir).is_none());
        game(&dir);
        let paks = find_paks(&dir);
        assert_eq!(paks.len(), 2);
        assert!(paks[0].0.ends_with(Path::new("engine").join("ShaderCache.pak")));
        assert!(sample(&paks));
        assert!(!sample(&paks[1..]));
        assert_eq!(exe_name(&dir).as_deref(), Some("HuntGame.exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn indexes_synthetic_game() {
        let dir = std::env::temp_dir().join(format!("tc-cry-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        game(&dir);
        let set = index(&dir, &|_| {}).unwrap();
        // Kolejność plików: CS, PS, PS spoza układu, VS (dwa razy ten sam).
        assert_eq!(set.shaders.len(), 4);
        // Dwa układy, różne root signature.
        assert_eq!(set.signatures.len(), 2);
        // Illum: układ 7 i bliźniak 9 -> ta sama root signature, jedna pula.
        assert_eq!(set.pools.len(), 1);
        let (members, rs) = &set.pools[0];
        assert_eq!(members, &vec![1, 3]);
        assert_eq!(*rs, Some(0));
        // CS pod układem 8 (UAV u3 w zestawie).
        assert_eq!(set.pipelines, vec![Pipeline12 { rs: Some(1), stages: [None, None, None, None, None, Some(0)] }]);
        assert!(set.notes.iter().any(|n| n.contains("1 shaders left out")), "{:?}", set.notes);
        assert!(set.notes.iter().any(|n| n.contains("1 paks don't open as ZIPs")), "{:?}", set.notes);
        // Bajty na żądanie: ten sam kontener co w paczce.
        for s in &set.shaders {
            let b = s.get().unwrap();
            assert!(Container::parse(&b).is_some_and(|c| c.bytes.len() == b.len()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
