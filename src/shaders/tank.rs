//! Tank (Blizzard: Overwatch z instalacji Steam), ścieżka DirectX 11.
//! Gra trzyma dane w statycznym kontenerze TACT: `data\.build.config` (tekst
//! `klucz = wartość`) i pliki `data\data.CCC.AAA`, każdy to ciąg bloków BLTE
//! jeden za drugim. Manifestu root (szyfrowany co wersję) nie czytamy ani nie
//! odszyfrowujemy - przechodzimy kolejno wszystkie bloki każdego pliku.
//!
//! Blok BLTE: "BLTE", u32 BE rozmiar nagłówka (od początku bloku), bajt flag,
//! u24 BE liczba kawałków i na kawałek u32 BE rozmiar zapisany, u32 BE rozmiar
//! po rozpakowaniu, MD5 (24 B; 40 B przy fladze 0x10); potem kawałki: bajt
//! trybu i dane. Tryby: 'N' bez kompresji, 'Z' zlib; 'E' (szyfrowany) i inne
//! pomijamy i liczymy.
//!
//! Zasób ShaderCode: 8 bajtów 0xFF, nagłówek zasobu, potem człon gzip z
//! jednym kontenerem. Kontenery DXBC SM 5.0 (DirectX 11) podajemy dalej, DXIL
//! (DirectX 12) pomijamy - gra buduje root signature w locie, a bez zapisu z
//! gry (anti-cheat: tylko pliki na dysku) ich nie znamy. Gra jest tylko
//! czytana.

use std::fs::File;
use std::path::{Path, PathBuf};

use super::container::{carve, read_at};
use super::readers::Detected;

const DATA: &str = "data";
const BUILD_CONFIG: &str = r"data\.build.config";
const PRODUCT: &str = "Prometheus";
const EXE: &str = "Overwatch.exe";
const SETTINGS: &str = r"Overwatch\Settings\Settings_v0.ini";

/// Początek zasobu ShaderCode.
const MAGIC: [u8; 8] = [0xFF; 8];
const GZIP: [u8; 3] = [0x1F, 0x8B, 0x08];
/// Ile podejść do członu gzip w zasobie (nagłówek może udawać magię).
const GZIP_TRIES: usize = 4;

const MAX_CHUNKS: usize = 1 << 16;
/// Największy zasób po rozpakowaniu; zapisany może być trochę większy
/// (zlib na danych już skompresowanych i bajty trybu).
const MAX_ASSET: usize = 64 << 20;
const MAX_STORED: usize = MAX_ASSET + (1 << 20);
/// Tekst czytany w całości (build config, ustawienia gry).
const MAX_TEXT: u64 = 1 << 20;
const MAX_FILES: usize = 100_000;
/// Okno odczytu pliku danych: bloki leżą po kolei, większość mieści się w nim.
const WINDOW: usize = 4 << 20;
/// Tyle zapisanych bajtów pierwszego kawałka wystarczy na tablice Huffmana i
/// pierwsze bajty zasobu.
const PEEK: usize = 16 << 10;

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

// ------------------------------ rozpoznanie ----------------------------------

/// Tekst pliku z limitem rozmiaru; UTF-16 sprowadzony do ASCII (zera usunięte).
fn small_text(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > MAX_TEXT {
        return None;
    }
    let mut b = vec![0u8; len as usize];
    read_at(&file, 0, &mut b).ok()?;
    b.retain(|&c| c != 0);
    Some(String::from_utf8_lossy(&b).into_owned())
}

/// Pierwsza wartość klucza z tekstu `klucz = wartość` (`#` - komentarz).
fn config_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim())
}

/// API z linii `GraphicsAPI = "Dx12"` ustawień gry; bez niej gra wybiera sama
/// (DirectX 11 jest wtedy możliwy).
fn api_from(settings: &str) -> &'static str {
    let value = settings.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "GraphicsAPI").then(|| v.trim().trim_matches('"').trim().to_ascii_lowercase())
    });
    if value.as_deref() == Some("dx12") {
        "DirectX 12"
    } else {
        "DirectX 11"
    }
}

/// Plik ustawień w Dokumentach (także Dokumenty w OneDrive).
fn settings_file() -> Option<PathBuf> {
    ["USERPROFILE", "OneDrive"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|d| PathBuf::from(d).join("Documents").join(SETTINGS))
        .find(|p| p.is_file())
}

fn is_data_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 12
        && b[..5].eq_ignore_ascii_case(b"data.")
        && b[8] == b'.'
        && b[5..8].iter().chain(&b[9..]).all(u8::is_ascii_digit)
}

/// Pliki danych `data\data.CCC.AAA` (posortowane): (ścieżka, rozmiar).
fn data_files(game: &Path) -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64)> = std::fs::read_dir(game.join(DATA))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_data_name(&e.file_name().to_string_lossy()))
        .filter_map(|e| Some((e.path(), e.metadata().ok().filter(|m| m.is_file())?.len())))
        .take(MAX_FILES)
        .collect();
    out.sort();
    out
}

/// Gra: build config produktu "Prometheus" (Overwatch) i pliki danych.
pub fn detect(game: &Path) -> Option<Detected> {
    let config = game.join(BUILD_CONFIG);
    if !config.is_file() {
        return None;
    }
    let text = small_text(&config)?;
    if config_value(&text, "build-product") != Some(PRODUCT) {
        return None;
    }
    let files = data_files(game);
    if files.is_empty() {
        return None;
    }
    let version = config_value(&text, "build-branch")
        .filter(|b| !b.is_empty())
        .map(|b| b.replace('_', "."))
        .or_else(|| config_value(&text, "build-number").map(str::to_string));
    let api = settings_file().and_then(|p| small_text(&p)).map_or("DirectX 11", |t| api_from(&t));
    Some(Detected {
        engine: "Tank",
        version,
        api,
        files,
        exe: game.join(EXE).is_file().then(|| EXE.to_string()),
    })
}

// ------------------------------ pliki danych ---------------------------------

/// Okno odczytu pliku: kolejne bloki czytane z pamięci, duże osobno.
struct Window<'a> {
    file: &'a File,
    len: u64,
    start: u64,
    buf: Vec<u8>,
}

impl<'a> Window<'a> {
    fn new(file: &'a File, len: u64) -> Self {
        Window { file, len, start: 0, buf: Vec::new() }
    }

    /// `n` bajtów od `pos`; `None` - poza plikiem albo błąd odczytu.
    fn get(&mut self, pos: u64, n: usize) -> Option<&[u8]> {
        let end = pos.checked_add(n as u64).filter(|&e| e <= self.len)?;
        let inside = pos >= self.start && end <= self.start + self.buf.len() as u64;
        if !inside {
            let take = (self.len - pos).min(WINDOW.max(n) as u64) as usize;
            if self.buf.capacity() > WINDOW && take <= WINDOW {
                self.buf = Vec::new(); // po dużym bloku oddajemy pamięć
            }
            self.buf.resize(take, 0);
            self.start = pos;
            if read_at(self.file, pos, &mut self.buf).is_err() {
                self.buf.clear();
                return None;
            }
        }
        let at = (pos - self.start) as usize;
        self.buf.get(at..at + n)
    }
}

/// Kawałek bloku: (rozmiar zapisany z bajtem trybu, rozmiar po rozpakowaniu).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Chunk {
    stored: usize,
    raw: usize,
}

/// Blok BLTE: położenie danych (za nagłówkiem), kawałki, cała długość.
#[derive(Debug, PartialEq)]
struct Blob {
    data: u64,
    chunks: Vec<Chunk>,
    len: u64,
}

/// Blok zaczynający się w `at`; `None` - nie ma tu bloku z tabelą kawałków
/// albo wychodzi poza plik.
fn blob_at(w: &mut Window, at: u64) -> Option<Blob> {
    let head: [u8; 12] = w.get(at, 12)?.try_into().ok()?;
    if &head[..4] != b"BLTE" {
        return None;
    }
    let size = be32(&head, 4)? as usize;
    let n = (be32(&head, 8)? & 0xFF_FFFF) as usize;
    let entry = if head[8] == 0x10 { 40 } else { 24 };
    if n == 0 || n > MAX_CHUNKS || size != 12 + n * entry {
        return None;
    }
    let room = w.len - at;
    if size as u64 > room {
        return None;
    }
    let table = w.get(at + 12, n * entry)?;
    let mut chunks = Vec::with_capacity(n);
    let mut total = size as u64;
    for e in table.chunks_exact(entry) {
        let (stored, raw) = (be32(e, 0)? as usize, be32(e, 4)? as usize);
        if stored == 0 || stored > i32::MAX as usize || raw > i32::MAX as usize {
            return None;
        }
        total += stored as u64;
        chunks.push(Chunk { stored, raw });
    }
    (total <= room).then_some(Blob { data: at + size as u64, chunks, len: total })
}

/// Liczniki przejścia (do komunikatu, gdy nic nie znaleziono).
#[derive(Default)]
struct Stats {
    blobs: usize,
    assets: usize,
    dx11: usize,
    dxil: usize,
    /// Kawałki szyfrowane ('E') i innych kodeków - bloki pominięte.
    encrypted: usize,
    other: usize,
    /// Zasoby, które się nie rozpakowały.
    broken: usize,
    unreadable: usize,
    stopped: Vec<String>,
}

impl Stats {
    fn mode(&mut self, m: u8) {
        if m == b'E' {
            self.encrypted += 1;
        } else {
            self.other += 1;
        }
    }

    fn describe(&self) -> String {
        let mut s = format!(
            "brak shaderów DirectX 11 / no DirectX 11 shaders: {} bloków / blobs, {} zasobów ShaderCode / ShaderCode assets, {} DXIL (DirectX 12, pominięte / skipped)",
            self.blobs, self.assets, self.dxil
        );
        if self.encrypted + self.other > 0 {
            s += &format!(
                "; pominięte bloki / skipped blobs: {} szyfrowanych / encrypted, {} innych kodeków / other codecs",
                self.encrypted, self.other
            );
        }
        if self.broken > 0 {
            s += &format!("; {} uszkodzonych / broken", self.broken);
        }
        if self.unreadable > 0 {
            s += &format!("; {} plików nieczytelnych / unreadable files", self.unreadable);
        }
        if !self.stopped.is_empty() {
            s += &format!("; odczyt przerwany / reading stopped: {}", self.stopped.join(", "));
        }
        s
    }
}

/// Pierwsze bajty bloku po rozpakowaniu (z początku pierwszego kawałka);
/// `Err(tryb)` - kawałek szyfrowany albo innego kodeka.
fn peek(w: &mut Window, b: &Blob, want: usize) -> Result<Option<Vec<u8>>, u8> {
    let first = b.chunks[0];
    let Some(raw) = w.get(b.data, first.stored.min(PEEK)) else { return Ok(None) };
    match raw[0] {
        b'N' => Ok(raw.get(1..1 + want).map(<[u8]>::to_vec)),
        // Dwa bajty nagłówka zlib, potem DEFLATE.
        b'Z' => Ok(raw.get(3..).and_then(|d| inflate_prefix(d, want))),
        m => Err(m),
    }
}

/// Cały blok po rozpakowaniu; `Err(tryb)` - kawałek szyfrowany albo innego
/// kodeka, `Ok(None)` - za duży albo uszkodzony.
fn decode(w: &mut Window, b: &Blob) -> Result<Option<Vec<u8>>, u8> {
    let raw: usize = b.chunks.iter().map(|c| c.raw).sum();
    let stored: usize = b.chunks.iter().map(|c| c.stored).sum();
    if raw == 0 || raw > MAX_ASSET || stored > MAX_STORED {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(raw);
    let mut pos = b.data;
    for c in &b.chunks {
        let Some(d) = w.get(pos, c.stored) else { return Ok(None) };
        match d[0] {
            b'N' if d.len() - 1 == c.raw => out.extend_from_slice(&d[1..]),
            b'N' => return Ok(None),
            b'Z' => match super::inflate::zlib(&d[1..], c.raw) {
                Some(x) => out.extend_from_slice(&x),
                None => return Ok(None),
            },
            m => return Err(m),
        }
        pos += c.stored as u64;
    }
    Ok(Some(out))
}

/// Początek danych DEFLATE członu gzip (RFC 1952); `None` - to nie nagłówek.
fn gzip_body(b: &[u8]) -> Option<usize> {
    if b.get(..3)? != GZIP {
        return None;
    }
    let flags = *b.get(3)?;
    if flags & 0xE0 != 0 {
        return None;
    }
    let mut p = 10;
    if flags & 0x04 != 0 {
        let extra = u16::from_le_bytes(b.get(p..p + 2)?.try_into().ok()?) as usize;
        p += 2 + extra;
    }
    // Nazwa i komentarz: teksty z zerem.
    for bit in [0x08, 0x10] {
        if flags & bit != 0 {
            p += b.get(p..)?.iter().position(|&c| c == 0)? + 1;
        }
    }
    if flags & 0x02 != 0 {
        p += 2;
    }
    (p <= b.len()).then_some(p)
}

/// Zawartość członu gzip zasobu ShaderCode: pierwszy człon (z kilku
/// podejść), który się rozpakuje; `None` - to nie zasób albo uszkodzony.
fn asset_payload(asset: &[u8]) -> Option<Vec<u8>> {
    if asset.len() < 16 || asset[..8] != MAGIC {
        return None;
    }
    let mut from = 8;
    for _ in 0..GZIP_TRIES {
        let k = from + asset.get(from..)?.windows(3).position(|w| w == GZIP)?;
        if let Some(body) = gzip_body(&asset[k..]).and_then(|s| asset.get(k + s..)) {
            // Rozmiar z końca członu (gdy kończy zasób), inaczej granica DEFLATE.
            let bound = body.len().saturating_mul(1032).saturating_add(1024).min(MAX_ASSET);
            let guess = le32(asset, asset.len() - 4).map_or(0, |n| n as usize);
            let tried = (guess > 0 && guess < bound).then(|| super::inflate::inflate(body, guess)).flatten();
            if let Some((out, _)) = tried.or_else(|| super::inflate::inflate(body, bound)) {
                return Some(out);
            }
        }
        from = k + 1;
    }
    None
}

/// Kontenery DXBC SM 5.0 z rozpakowanego zasobu (DXIL liczymy i pomijamy).
fn shaders(payload: &[u8], stats: &mut Stats, each: &mut dyn FnMut(&[u8])) {
    carve(payload, 0, &mut |_, c| {
        if c.is_dxil() {
            stats.dxil += 1;
        } else if c.kind().is_some() {
            stats.dx11 += 1;
            each(c.bytes);
        }
    });
}

/// Jeden plik danych: kolejno wszystkie bloki.
fn walk(file: &File, len: u64, name: &str, stats: &mut Stats, each: &mut dyn FnMut(&[u8])) {
    let mut w = Window::new(file, len);
    let mut at = 0u64;
    while at < len {
        let Some(blob) = blob_at(&mut w, at) else {
            // Bez tabeli kawałków nie znamy długości bloku: dalej nie idziemy.
            stats.stopped.push(format!("{name} @ {at}"));
            return;
        };
        stats.blobs += 1;
        let next = at + blob.len;
        match peek(&mut w, &blob, MAGIC.len()) {
            Ok(Some(head)) if head == MAGIC => {
                stats.assets += 1;
                match decode(&mut w, &blob) {
                    Ok(Some(asset)) => match asset_payload(&asset) {
                        Some(p) => shaders(&p, stats, each),
                        None => stats.broken += 1,
                    },
                    Ok(None) => stats.broken += 1,
                    Err(m) => stats.mode(m),
                }
            }
            Ok(_) => {}
            Err(m) => stats.mode(m),
        }
        at = next;
    }
}

/// Każdy shader DirectX 11 gry; postęp po każdym pliku danych.
pub fn for_each_shader(
    game: &Path,
    progress: &mut dyn FnMut(usize, usize),
    each: &mut dyn FnMut(&[u8]),
) -> Result<usize, String> {
    let files = data_files(game);
    if files.is_empty() {
        return Err(format!("brak plików {DATA}\\data.CCC.AAA / no {DATA}\\data.CCC.AAA files"));
    }
    let mut stats = Stats::default();
    progress(0, files.len());
    for (i, (path, _)) in files.iter().enumerate() {
        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        match File::open(path).and_then(|f| f.metadata().map(|m| (m.len(), f))) {
            Ok((len, file)) => walk(&file, len, &name, &mut stats, each),
            Err(_) => stats.unreadable += 1,
        }
        progress(i + 1, files.len());
    }
    if stats.dx11 == 0 {
        return Err(stats.describe());
    }
    Ok(stats.dx11)
}

// ------------------- początek strumienia DEFLATE -----------------------------

/// Bity strumienia DEFLATE od najmłodszego.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> Option<usize> {
        let mut v = 0usize;
        for i in 0..n {
            let byte = *self.b.get(self.pos >> 3)?;
            v |= usize::from((byte >> (self.pos & 7)) & 1) << i;
            self.pos += 1;
        }
        Some(v)
    }
}

/// Kod Huffmana kanoniczny: liczby kodów długości i symbole po kolei.
struct Code {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Code {
    fn new(lengths: &[u8]) -> Option<Code> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            *counts.get_mut(usize::from(l))? += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                *symbols.get_mut(usize::from(offs[usize::from(l)]))? = s as u16;
                offs[usize::from(l)] += 1;
            }
        }
        Some(Code { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Option<usize> {
        let (mut code, mut first, mut index) = (0usize, 0usize, 0usize);
        for len in 1..16 {
            code |= bits.take(1)?;
            let count = usize::from(self.counts[len]);
            if code < first + count {
                return self.symbols.get(index + code - first).map(|&s| usize::from(s));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Pierwsze `want` bajtów surowego DEFLATE (mniej, gdy strumień krótszy) -
/// bez rozpakowywania reszty bloku; wejście może być ucięte za nimi.
fn inflate_prefix(data: &[u8], want: usize) -> Option<Vec<u8>> {
    let mut bits = Bits { b: data, pos: 0 };
    let mut out = Vec::with_capacity(want + 258);
    while out.len() < want {
        let last = bits.take(1)?;
        let (lit, dist) = match bits.take(2)? {
            0 => {
                let p = bits.pos.div_ceil(8);
                let len = usize::from(u16::from_le_bytes(data.get(p..p + 2)?.try_into().ok()?));
                let nlen = usize::from(u16::from_le_bytes(data.get(p + 2..p + 4)?.try_into().ok()?));
                if len != !nlen & 0xFFFF {
                    return None;
                }
                let take = len.min(want - out.len());
                out.extend_from_slice(data.get(p + 4..p + 4 + take)?);
                bits.pos = (p + 4 + len) * 8;
                if last == 1 {
                    break;
                }
                continue;
            }
            1 => {
                let mut l = [8u8; 288];
                l[144..256].fill(9);
                l[256..280].fill(7);
                (Code::new(&l)?, Code::new(&[5u8; 30])?)
            }
            2 => {
                let hlit = bits.take(5)? + 257;
                let hdist = bits.take(5)? + 1;
                let hclen = bits.take(4)? + 4;
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in &ORDER[..hclen] {
                    cl[o] = bits.take(3)? as u8;
                }
                let code = Code::new(&cl)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < lengths.len() {
                    let (value, repeat) = match code.decode(&mut bits)? {
                        s @ 0..=15 => (s as u8, 1),
                        16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.take(2)?),
                        17 => (0, 3 + bits.take(3)?),
                        18 => (0, 11 + bits.take(7)?),
                        _ => return None,
                    };
                    lengths.get_mut(i..i + repeat)?.fill(value);
                    i += repeat;
                }
                (Code::new(&lengths[..hlit])?, Code::new(&lengths[hlit..])?)
            }
            _ => return None,
        };
        while out.len() < want {
            match lit.decode(&mut bits)? {
                s @ 0..=255 => out.push(s as u8),
                256 => break,
                s => {
                    let i = s - 257;
                    let len = usize::from(*LEN_BASE.get(i)?) + bits.take(u32::from(LEN_EXTRA[i]))?;
                    let d = dist.decode(&mut bits)?;
                    let back = usize::from(*DIST_BASE.get(d)?) + bits.take(u32::from(DIST_EXTRA[d]))?;
                    let from = out.len().checked_sub(back)?;
                    for k in 0..len {
                        out.push(out[from + k]);
                    }
                }
            }
        }
        if last == 1 {
            break;
        }
    }
    out.truncate(want);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::container::tests::{build, dxil};

    /// Kontener SM5 (program SHEX danego typu) z wypełnieniem.
    fn shader(kind: u32, fill: u8) -> Vec<u8> {
        build(&[(b"SHEX", ((kind << 16) | 0x50).to_le_bytes().to_vec()), (b"PAD_", vec![fill; 64])])
    }

    fn adler32(b: &[u8]) -> u32 {
        let (mut a, mut s) = (1u32, 0u32);
        for &x in b {
            a = (a + u32::from(x)) % 65_521;
            s = (s + a) % 65_521;
        }
        (s << 16) | a
    }

    /// DEFLATE z blokami bez kompresji.
    fn deflate_stored(data: &[u8]) -> Vec<u8> {
        let mut z = Vec::new();
        let blocks: Vec<&[u8]> = data.chunks(0xFFFF).collect();
        for (i, b) in blocks.iter().enumerate() {
            z.push(u8::from(i + 1 == blocks.len()));
            let n = b.len() as u16;
            z.extend_from_slice(&n.to_le_bytes());
            z.extend_from_slice(&(!n).to_le_bytes());
            z.extend_from_slice(b);
        }
        z
    }

    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut z = vec![0x78, 0x01];
        z.extend(deflate_stored(data));
        z.extend_from_slice(&adler32(data).to_be_bytes());
        z
    }

    /// Człon gzip z nazwą pliku (CRC32 nie jest sprawdzane - zera).
    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut g = vec![0x1F, 0x8B, 0x08, 0x08, 0, 0, 0, 0, 0, 0xFF];
        g.extend_from_slice(b"shader.bin\0");
        g.extend(deflate_stored(data));
        g.extend_from_slice(&0u32.to_le_bytes());
        g.extend_from_slice(&(data.len() as u32).to_le_bytes());
        g
    }

    /// Blok BLTE: kawałki (tryb, dane zapisane, rozmiar po rozpakowaniu).
    fn blte(flags: u8, chunks: &[(u8, Vec<u8>, usize)]) -> Vec<u8> {
        let entry = if flags == 0x10 { 40 } else { 24 };
        let mut b = b"BLTE".to_vec();
        b.extend_from_slice(&((12 + chunks.len() * entry) as u32).to_be_bytes());
        b.push(flags);
        b.extend_from_slice(&(chunks.len() as u32).to_be_bytes()[1..]);
        for (_, data, raw) in chunks {
            b.extend_from_slice(&((data.len() + 1) as u32).to_be_bytes());
            b.extend_from_slice(&(*raw as u32).to_be_bytes());
            b.extend_from_slice(&vec![0u8; entry - 8]);
        }
        for (mode, data, _) in chunks {
            b.push(*mode);
            b.extend_from_slice(data);
        }
        b
    }

    /// Zasób ShaderCode: magia, nagłówek z fałszywą magią gzip, człon gzip.
    fn asset(payload: &[u8]) -> Vec<u8> {
        let mut a = MAGIC.to_vec();
        a.extend_from_slice(&[0x1F, 0x8B, 0x08, 0xFF, 1, 2, 3, 4, 5, 6, 7, 8]);
        a.extend(gzip(payload));
        a
    }

    fn game_with(data: &[u8], tag: &str) -> PathBuf {
        let game = std::env::temp_dir().join(format!("tc-tank-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&game);
        std::fs::create_dir_all(game.join(DATA)).unwrap();
        std::fs::write(
            game.join(BUILD_CONFIG),
            "# Build Configuration\nbuild-product = Prometheus\nbuild-branch = 2_25_0_0\nbuild-number = 154088\n",
        )
        .unwrap();
        std::fs::write(game.join(r"data\data.000.000"), data).unwrap();
        game
    }

    #[test]
    fn reads_synthetic_static_container() {
        let vs = shader(1, 0x11);
        let ps = shader(0, 0x22);
        // Zasób DX11 (VS) i zasób DX12 (56 B nagłówka i DXIL - pomijany).
        let a1 = asset(&vs);
        let mut dx12 = vec![0u8; 56];
        dx12.extend(build(&[(b"DXIL", dxil(0))]));
        let a2 = asset(&dx12);
        let a3 = asset(&ps);
        let (head, tail) = a1.split_at(20);
        let mut file = Vec::new();
        // Zwykły blok, zasób w dwóch kawałkach (zlib + bez kompresji, flagi 0x10),
        // blok szyfrowany, zasób DX12, zasób bez kompresji, śmieci na końcu.
        file.extend(blte(0x0F, &[(b'N', vec![7u8; 100], 100)]));
        file.extend(blte(0x10, &[(b'Z', zlib_stored(head), head.len()), (b'N', tail.to_vec(), tail.len())]));
        file.extend(blte(0x0F, &[(b'E', vec![0xAB; 40], 30)]));
        file.extend(blte(0x0F, &[(b'Z', zlib_stored(&a2), a2.len())]));
        file.extend(blte(0x0F, &[(b'N', a3.clone(), a3.len())]));
        file.extend_from_slice(b"junk junk junk junk");
        let game = game_with(&file, "read");

        let d = detect(&game).expect("detect");
        assert_eq!((d.engine, d.version.as_deref()), ("Tank", Some("2.25.0.0")));
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.exe, None);
        std::fs::write(game.join(EXE), b"MZ").unwrap();
        assert_eq!(detect(&game).unwrap().exe.as_deref(), Some(EXE));

        let mut got = Vec::new();
        let mut steps = Vec::new();
        let n = for_each_shader(&game, &mut |a, b| steps.push((a, b)), &mut |b| got.push(b.to_vec())).unwrap();
        assert_eq!(n, 2);
        assert_eq!(got, vec![vs, ps]);
        assert_eq!(steps, vec![(0, 1), (1, 1)]);

        // Statystyki tego samego pliku.
        let f = File::open(game.join(r"data\data.000.000")).unwrap();
        let mut stats = Stats::default();
        walk(&f, file.len() as u64, "x", &mut stats, &mut |_| {});
        assert_eq!((stats.blobs, stats.assets, stats.dx11, stats.dxil, stats.encrypted), (5, 3, 2, 1, 1));
        assert_eq!(stats.stopped.len(), 1);
        drop(f);

        // Inny produkt - nie ta gra.
        std::fs::write(game.join(BUILD_CONFIG), "build-product = Fenris\n").unwrap();
        assert!(detect(&game).is_none());
        let _ = std::fs::remove_dir_all(&game);
    }

    #[test]
    fn settings_api_and_names() {
        assert_eq!(api_from("[Render.13]\nGraphicsAPI = \"Dx12\"\n"), "DirectX 12");
        assert_eq!(api_from("GraphicsAPI = \"Dx11\"\n"), "DirectX 11");
        assert_eq!(api_from("FullScreenMode = \"0\"\n"), "DirectX 11");
        assert!(is_data_name("data.012.345") && is_data_name("DATA.000.000"));
        assert!(!is_data_name("data.01.2345") && !is_data_name("data.000.00x") && !is_data_name(".build.config"));
    }

    #[test]
    fn prefix_of_fixed_and_stored_deflate() {
        // .NET DeflateStream("hello hello hello hello"): kod stały i dopasowanie.
        let fixed = [0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01];
        assert_eq!(inflate_prefix(&fixed, 8).unwrap(), b"hello he");
        assert_eq!(inflate_prefix(&fixed, 100).unwrap(), b"hello hello hello hello");
        let stored = deflate_stored(&[0xFF; 40]);
        assert_eq!(inflate_prefix(&stored[..20], 8).unwrap(), [0xFF; 8]);
        assert!(inflate_prefix(&[0xFF; 4], 8).is_none());
    }

    #[test]
    fn garbage_never_panics() {
        let a = asset(&shader(0, 1));
        let mut file = blte(0x0F, &[(b'Z', zlib_stored(&a), a.len())]);
        file.extend(blte(0x0F, &[(b'N', a.clone(), a.len())]));
        let game = game_with(&file, "junk");
        let path = game.join(r"data\data.000.000");
        let mut seed = 0x1234_5678u32;
        for round in 0..400 {
            let mut bad = file.clone();
            for _ in 0..1 + round % 4 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = seed as usize % bad.len();
                bad[at] ^= (seed >> 24) as u8 | 1;
            }
            bad.truncate(bad.len() - round % 7);
            std::fs::write(&path, &bad).unwrap();
            let f = File::open(&path).unwrap();
            walk(&f, bad.len() as u64, "x", &mut Stats::default(), &mut |_| {});
            let _ = asset_payload(&bad);
            let _ = inflate_prefix(&bad, 8);
        }
        let _ = std::fs::remove_dir_all(&game);
    }
}
