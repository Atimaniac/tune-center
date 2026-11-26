//! Frostbite (EA: Battlefield, Mass Effect: Andromeda, Need for Speed, Dead
//! Space), ścieżka DirectX 11. Gra ma `Data\layout.toc`, a obok archiwa
//! `cas_NN.cas`. Shadery to kontenery DXBC w skompresowanych blokach archiwów.
//! Katalogów (cas.cat, .toc, .sb) nie czytamy - różnią się między wersjami
//! silnika i bywają zaciemnione; rozpakowujemy kolejno wszystkie bloki każdego
//! archiwum i podajemy strumień dalej (kontenery wycina wywołujący).
//!
//! Wpisy leżą jeden za drugim, każdy to ciąg bloków z nagłówkiem 8 B:
//! - u32 BE: rozmiar po rozpakowaniu w dolnych 24 bitach; niezerowy górny
//!   bajt - zstd ze słownikiem zapisanym w grze (pomijamy),
//! - bajt kodeka: 0x00 bez kompresji, 0x02 zlib, 0x09 LZ4 (blok), 0x0F zstd,
//!   0x11 / 0x15 / 0x19 Oodle; bit 7 - blok zaciemniony (pomijamy),
//! - bajt 0x7N (N - bity 16-19 rozmiaru zapisanego) i u16 BE - jego dolne bity.
//!
//! Gdzie nagłówek się nie zgadza (łatka delta, wpis szyfrowany), szukamy dalej
//! najbliższego bloku, który się rozpakuje. Gry są tylko czytane.

use std::fs::File;
use std::path::{Path, PathBuf};

use super::container::read_at;
use super::readers::Detected;

const LAYOUT: &str = r"Data\layout.toc";
/// Blok po rozpakowaniu (Frostbite pisze bloki po 64 KB).
const MAX_RAW: usize = 1 << 22;
/// Okno odczytu pliku (rozmiar zapisany ma 20 bitów, więc blok się mieści).
const WINDOW: usize = 4 << 20;
/// Co tyle nowych bajtów strumienia podajemy okno dalej.
const CHUNK: usize = 8 << 20;
/// Kontener większy nie jest shaderem (fałszywe "DXBC" trzymałoby tyle).
const MAX_CONTAINER: usize = 16 << 20;
const MAX_DEPTH: usize = 6;
const MAX_ARCHIVES: usize = 20_000;

/// Kodeki z nagłówka (bez bitu zaciemnienia).
fn known(codec: u8) -> bool {
    matches!(codec & 0x7F, 0x00 | 0x02 | 0x09 | 0x0F | 0x11 | 0x15 | 0x19)
}

/// Bajty kodeka i flag mogą zaczynać nagłówek.
fn candidate(codec: u8, flags: u8) -> bool {
    flags & 0xF0 == 0x70 && known(codec)
}

/// Nagłówek bloku.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Header {
    raw: usize,
    stored: usize,
    codec: u8,
    /// Górny bajt rozmiaru: słownik zstd z gry.
    dict: bool,
}

impl Header {
    fn parse(h: &[u8; 8]) -> Option<Header> {
        let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
        let raw = (size & 0xFF_FFFF) as usize;
        let stored = usize::from(h[5] & 0x0F) << 16 | usize::from(u16::from_be_bytes([h[6], h[7]]));
        let codec = h[4];
        let ok = candidate(codec, h[5])
            && (1..=MAX_RAW).contains(&raw)
            && stored > 0
            && (codec != 0 || stored == raw);
        ok.then_some(Header { raw, stored, codec, dict: size >> 24 != 0 })
    }

    /// Dlaczego blok pomijamy (None - da się rozpakować).
    fn skip(&self) -> Option<Skip> {
        if self.codec & 0x80 != 0 {
            Some(Skip::Obfuscated)
        } else if self.dict {
            Some(Skip::Dictionary)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
enum Skip {
    Dictionary,
    Obfuscated,
    Failed,
}

/// Liczniki do komunikatu.
#[derive(Default, Debug)]
struct Stats {
    blocks: u64,
    dictionary: u64,
    obfuscated: u64,
    failed: u64,
    breaks: u64,
    unreadable: u64,
}

impl Stats {
    fn count(&mut self, s: Skip) {
        match s {
            Skip::Dictionary => self.dictionary += 1,
            Skip::Obfuscated => self.obfuscated += 1,
            Skip::Failed => self.failed += 1,
        }
    }

    fn describe(&self) -> String {
        format!(
            "brak kontenerów DXBC w archiwach (bloki: {} rozpakowanych, {} ze słownikiem zstd z gry, \
             {} zaciemnionych, {} nieudanych; przerwy: {}; nieczytelne pliki: {}) / \
             no DXBC containers in the archives (blocks: {} decoded, {} with the game's zstd dictionary, \
             {} obfuscated, {} failed; breaks: {}; unreadable files: {})",
            self.blocks, self.dictionary, self.obfuscated, self.failed, self.breaks, self.unreadable,
            self.blocks, self.dictionary, self.obfuscated, self.failed, self.breaks, self.unreadable,
        )
    }
}

/// Okno odczytu pliku: bloki czytane po kolei bez wczytywania całości.
struct Reader<'a> {
    file: &'a File,
    len: u64,
    buf: Vec<u8>,
    at: u64,
    filled: usize,
    failed: bool,
}

impl<'a> Reader<'a> {
    fn new(file: &'a File, len: u64) -> Self {
        Reader { file, len, buf: vec![0; WINDOW], at: 0, filled: 0, failed: false }
    }

    /// `n` bajtów od `pos`; None - poza plikiem albo błąd odczytu.
    fn get(&mut self, pos: u64, n: usize) -> Option<&[u8]> {
        let end = pos.checked_add(n as u64)?;
        if pos >= self.at && end <= self.at + self.filled as u64 {
            let o = (pos - self.at) as usize;
            return self.buf.get(o..o + n);
        }
        if end > self.len || n > WINDOW || self.failed {
            return None;
        }
        let want = (self.len - pos).min(WINDOW as u64) as usize;
        self.filled = 0;
        if read_at(self.file, pos, &mut self.buf[..want]).is_err() {
            self.failed = true;
            return None;
        }
        (self.at, self.filled) = (pos, want);
        self.buf.get(..n)
    }

    /// Najbliższe miejsce od `p`, gdzie bajty kodeka i flag pasują do nagłówka.
    fn next(&mut self, mut p: u64) -> u64 {
        let len = self.len;
        while p + 8 <= len {
            let n = (len - p).min(WINDOW as u64) as usize;
            let Some(s) = self.get(p, n) else { return len };
            if let Some(i) = s.windows(8).position(|w| candidate(w[4], w[5])) {
                return p + i as u64;
            }
            p += (n - 7) as u64;
        }
        len
    }
}

/// Dekodery bloków (Oodle z otwartego crate oozextract; bufory po kilka MB,
/// więc jeden na cały odczyt).
struct Decoders {
    ooz: oozextract::Extractor,
}

impl Decoders {
    fn new() -> Self {
        Decoders { ooz: oozextract::Extractor::new() }
    }

    /// Rozpakowuje blok do `out`; false - dane uszkodzone albo inny rozmiar.
    fn decode(&mut self, codec: u8, src: &[u8], raw: usize, out: &mut Vec<u8>) -> bool {
        out.clear();
        let got = match codec {
            0x00 => {
                out.extend_from_slice(src);
                return out.len() == raw;
            }
            0x02 => super::inflate::zlib(src, raw),
            0x09 => crate::lz4::decompress(src, raw).ok(),
            0x0F => crate::zstd::decompress(src, raw).ok(),
            0x11 | 0x15 | 0x19 => {
                out.resize(raw, 0);
                let n = self.ooz.read_from_slice(src, out);
                return matches!(n, Ok(n) if n == raw);
            }
            _ => None,
        };
        match got {
            Some(v) if v.len() == raw => {
                *out = v;
                true
            }
            _ => false,
        }
    }
}

/// Przechodzi bloki archiwum od początku. `sink(Some(bajty))` - kolejny
/// rozpakowany blok, `sink(None)` - przerwa w strumieniu.
fn walk(file: &File, len: u64, stats: &mut Stats, sink: &mut dyn FnMut(Option<&[u8]>)) {
    let mut win = Reader::new(file, len);
    let mut dec = Decoders::new();
    let mut out = Vec::new();
    let mut pos = 0u64;
    // Nagłówek osiągnięty po łańcuchu bloków (a nie znaleziony przeszukaniem).
    let mut chained = true;
    while pos + 8 <= len && !win.failed {
        let mut h = [0u8; 8];
        match win.get(pos, 8) {
            Some(b) => h.copy_from_slice(b),
            None => break,
        }
        if let Some(head) = Header::parse(&h) {
            let total = 8 + head.stored as u64;
            if let Some(data) = win.get(pos + 8, head.stored) {
                let skip = match head.skip() {
                    None if dec.decode(head.codec, data, head.raw, &mut out) => None,
                    None => Some(Skip::Failed),
                    s => s,
                };
                match skip {
                    None => {
                        stats.blocks += 1;
                        sink(Some(&out));
                        (pos, chained) = (pos + total, true);
                        continue;
                    }
                    // Nagłówek z łańcucha jest prawdziwy: sam blok pomijamy.
                    Some(s) if chained => {
                        stats.count(s);
                        sink(None);
                        pos += total;
                        continue;
                    }
                    Some(_) => {}
                }
            }
        }
        if chained {
            stats.breaks += 1;
        }
        sink(None);
        chained = false;
        pos = win.next(pos + 1);
    }
    if win.failed {
        stats.unreadable += 1;
    }
}

fn le32(b: &[u8], at: usize) -> Option<usize> {
    b.get(at..at.checked_add(4)?)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// Pierwsze "DXBC" od `from`.
fn find_magic(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 4 <= b.len() {
        let p = b[i..b.len() - 3].iter().position(|&x| x == b'D')?;
        if &b[i + p..i + p + 4] == b"DXBC" {
            return Some(i + p);
        }
        i += p + 1;
    }
    None
}

/// Czy w bajtach jest "DXBC" i od którego miejsca trzeba je zachować
/// (kontener jeszcze niepełny albo ucięty nagłówek).
fn scan(b: &[u8]) -> (bool, Option<usize>) {
    let mut any = false;
    let mut i = 0;
    while let Some(at) = find_magic(b, i) {
        any = true;
        match (le32(b, at + 20), le32(b, at + 24)) {
            (Some(1), Some(t)) if (32..=MAX_CONTAINER).contains(&t) && at + t > b.len() => {
                return (true, Some(at));
            }
            (Some(_), Some(_)) => {}
            _ => return (true, Some(at)),
        }
        i = at + 4;
    }
    (any, None)
}

/// Przesuwane okno rozpakowanego strumienia: co `chunk` nowych bajtów podaje
/// całość dalej i zostawia ogon z niepełnym kontenerem (do następnego okna),
/// więc kontener na granicy bloków albo okien dochodzi w całości.
struct Stream {
    buf: Vec<u8>,
    chunk: usize,
    /// Przy jakim rozmiarze podać okno.
    due: usize,
    passed: usize,
}

impl Stream {
    fn new(chunk: usize) -> Self {
        Stream { buf: Vec::new(), chunk, due: chunk, passed: 0 }
    }

    fn push(&mut self, b: &[u8], each: &mut dyn FnMut(&[u8])) {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= self.due {
            self.emit(false, each);
        }
    }

    /// Przerwa albo koniec archiwum: reszta wychodzi, bufor od nowa.
    fn flush(&mut self, each: &mut dyn FnMut(&[u8])) {
        self.emit(true, each);
    }

    fn emit(&mut self, end: bool, each: &mut dyn FnMut(&[u8])) {
        let (any, keep) = scan(&self.buf);
        if any {
            each(&self.buf);
            self.passed += 1;
        }
        if end {
            self.buf.clear();
        } else {
            // Bez niepełnego kontenera: 3 B na początek "DXBC".
            let k = keep.unwrap_or(self.buf.len().saturating_sub(3));
            self.buf.drain(..k);
        }
        self.due = self.buf.len() + self.chunk;
    }
}

fn is_archive(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("cas_") && n.ends_with(".cas")
}

/// Archiwa `cas_*.cas` w folderze gry (do 6 poziomów): (ścieżka, rozmiar).
fn archives(game: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![(game.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((e.path(), depth + 1));
                }
            } else if kind.is_file() && is_archive(&e.file_name().to_string_lossy()) {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                if len >= 8 && out.len() < MAX_ARCHIVES {
                    out.push((e.path(), len));
                }
            }
        }
    }
    out.sort();
    out
}

pub fn detect(game: &Path) -> Option<Detected> {
    if !game.join(LAYOUT).is_file() {
        return None;
    }
    let files = archives(game);
    if files.is_empty() {
        return None;
    }
    // Archiwa wyglądają tak samo w DX11 i DX12: API po pliku .exe gry. Gra
    // wyłącznie DirectX 12 nie wspomina d3d11 ani w importach, ani w tekstach
    // (biblioteki ładowane dynamicznie); przeszukanie całego .exe zapamiętane
    // do jego zmiany.
    let exe = main_exe(game);
    let dx12_only = exe.as_ref().is_some_and(|p| {
        let imports = super::readers::pe_imports(p).unwrap_or_default();
        if imports.iter().any(|l| l == "d3d11.dll") {
            return false;
        }
        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        super::readers::cached_detect("frostbite-dx12", game, &[(p.clone(), len)], || {
            super::readers::mentions_d3d(p).is_some_and(|(d11, d12)| d12 && !d11)
        })
    });
    Some(Detected {
        engine: "Frostbite",
        version: Some("CAS".into()),
        api: if dx12_only { "DirectX 12" } else { "DirectX 11" },
        files,
        exe: exe.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
    })
}

/// Plik .exe gry: największy w głównym folderze, bez instalatorów i
/// programów uruchamiających.
fn main_exe(game: &Path) -> Option<PathBuf> {
    const SKIP: [&str; 8] = ["unins", "setup", "launcher", "crash", "redist", "installer", "touchup", "cleanup"];
    std::fs::read_dir(game)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let len = e.metadata().ok().filter(|m| m.is_file())?.len();
            (name.ends_with(".exe") && !SKIP.iter().any(|s| name.contains(s))).then(|| (e.path(), len))
        })
        .max_by_key(|(_, len)| *len)
        .map(|(p, _)| p)
}

/// Jedno archiwum przez okno strumienia.
fn read_archive(path: &Path, stream: &mut Stream, stats: &mut Stats, each: &mut dyn FnMut(&[u8])) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => {
            stats.unreadable += 1;
            return;
        }
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    walk(&file, len, stats, &mut |b| match b {
        Some(b) => stream.push(b, &mut *each),
        None => stream.flush(&mut *each),
    });
    stream.flush(each);
}

pub fn for_each_shader(
    game: &Path,
    progress: &mut dyn FnMut(usize, usize),
    each: &mut dyn FnMut(&[u8]),
) -> Result<usize, String> {
    let files = archives(game);
    if files.is_empty() {
        return Err("brak archiwów cas_NN.cas / no cas_NN.cas archives".into());
    }
    let mut stats = Stats::default();
    let mut stream = Stream::new(CHUNK);
    progress(0, files.len());
    for (i, (path, _)) in files.iter().enumerate() {
        read_archive(path, &mut stream, &mut stats, each);
        progress(i + 1, files.len());
    }
    if stream.passed == 0 {
        return Err(stats.describe());
    }
    Ok(stream.passed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::container::tests::build;
    use crate::shaders::unity::carve_dxbc;

    /// Kontener DXBC z programem SM5 (shader wierzchołków).
    fn shader(seed: u8) -> Vec<u8> {
        let mut shex = ((1u32 << 16) | 0x50).to_le_bytes().to_vec();
        shex.extend_from_slice(&100u32.to_le_bytes());
        shex.extend((0..392u32).map(|i| (i as u8).wrapping_mul(seed)));
        build(&[(b"SHEX", shex), (b"STAT", vec![seed; 64])])
    }

    fn block(codec: u8, raw: usize, data: &[u8], top: u8) -> Vec<u8> {
        let mut b = ((raw as u32) | u32::from(top) << 24).to_be_bytes().to_vec();
        b.push(codec);
        b.push(0x70 | ((data.len() >> 16) & 0x0F) as u8);
        b.extend_from_slice(&(data.len() as u16).to_be_bytes());
        b.extend_from_slice(data);
        b
    }

    /// Strumień zlib z jednym blokiem DEFLATE bez kompresji.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let n = data.len() as u16;
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(data);
        let (mut a, mut s) = (1u32, 0u32);
        for &x in data {
            a = (a + u32::from(x)) % 65521;
            s = (s + a) % 65521;
        }
        z.extend_from_slice(&(s << 16 | a).to_be_bytes());
        z
    }

    /// Archiwum: kontener A rozcięty na trzy bloki (bez kompresji, LZ4,
    /// zlib), blok zstd ze słownikiem gry, śmieci i kontener B w bloku.
    fn archive() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (a, b) = (shader(3), shader(7));
        let mut s = vec![0x55u8; 100];
        s.extend_from_slice(&a);
        s.extend_from_slice(&[0xAA; 100]);
        let (p1, p2) = (150, 350);
        let mut f = block(0x00, p1, &s[..p1], 0);
        f.extend(block(0x09, p2 - p1, &crate::lz4::compress_literals(&s[p1..p2]), 0));
        f.extend(block(0x02, s.len() - p2, &zlib_stored(&s[p2..]), 0));
        f.extend(block(0x0F, 4096, &[0x11; 300], 1));
        f.extend_from_slice(&[0u8; 37]);
        let mut tail = vec![0x33u8; 20];
        tail.extend_from_slice(&b);
        f.extend(block(0x00, tail.len(), &tail, 0));
        (f, a, b)
    }

    fn found(windows: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for w in windows {
            carve_dxbc(w, |d| out.push(d.code.to_vec()));
        }
        out
    }

    #[test]
    fn header_fields() {
        let h = Header::parse(&[0, 1, 0, 0, 0x09, 0x71, 0x00, 0x10]).unwrap();
        assert_eq!(h, Header { raw: 0x10000, stored: 0x10010, codec: 0x09, dict: false });
        // Bez kompresji: rozmiary muszą być równe; zły bajt flag.
        assert!(Header::parse(&[0, 0, 0, 16, 0x00, 0x70, 0, 15]).is_none());
        assert!(Header::parse(&[0, 0, 0, 16, 0x02, 0x60, 0, 15]).is_none());
        assert!(Header::parse(&[0, 0, 0, 16, 0x8F, 0x70, 0, 15]).unwrap().skip().is_some());
    }

    #[test]
    fn reads_synthetic_game() {
        let dir = std::env::temp_dir().join(format!("tc_frostbite_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(r"Data\Win32")).unwrap();
        assert!(detect(&dir).is_none());
        std::fs::write(dir.join(LAYOUT), b"toc").unwrap();
        assert!(detect(&dir).is_none());
        let (cas, a, b) = archive();
        std::fs::write(dir.join(r"Data\Win32\cas_01.cas"), &cas).unwrap();
        let d = detect(&dir).unwrap();
        assert_eq!((d.engine, d.api, d.version.as_deref()), ("Frostbite", "DirectX 11", Some("CAS")));
        assert_eq!(d.files.len(), 1);

        let mut windows = Vec::new();
        let mut steps = Vec::new();
        let n = for_each_shader(&dir, &mut |i, t| steps.push((i, t)), &mut |w| windows.push(w.to_vec())).unwrap();
        assert_eq!(n, windows.len());
        assert_eq!(steps, vec![(0, 1), (1, 1)]);
        let got = found(&windows);
        assert!(got.contains(&a) && got.contains(&b));

        // Liczniki: 4 bloki rozpakowane, 1 ze słownikiem, przerwa przed B.
        let mut stats = Stats::default();
        let mut stream = Stream::new(CHUNK);
        read_archive(&dir.join(r"Data\Win32\cas_01.cas"), &mut stream, &mut stats, &mut |_| {});
        assert_eq!((stats.blocks, stats.dictionary, stats.breaks), (4, 1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn small_windows_keep_split_container() {
        let a = shader(5);
        let mut s = vec![0x11u8; 70];
        s.extend_from_slice(&a);
        s.extend_from_slice(&[0x22; 90]);
        let mut windows = Vec::new();
        let mut stream = Stream::new(64);
        for piece in s.chunks(50) {
            stream.push(piece, &mut |w| windows.push(w.to_vec()));
        }
        stream.flush(&mut |w| windows.push(w.to_vec()));
        assert!(windows.iter().all(|w| w.len() < 64 + a.len() + 64));
        assert!(found(&windows).contains(&a));
    }

    #[test]
    fn garbage_never_panics() {
        let mut g: Vec<u8> = (0..20_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for i in (0..g.len() - 8).step_by(97) {
            g[i + 4] = [0x02, 0x09, 0x0F, 0x11][i % 4];
            g[i + 5] = 0x70;
        }
        let path = std::env::temp_dir().join(format!("tc_frostbite_garbage_{}.cas", std::process::id()));
        std::fs::write(&path, &g).unwrap();
        let mut stats = Stats::default();
        let mut stream = Stream::new(CHUNK);
        read_archive(&path, &mut stream, &mut stats, &mut |_| {});
        assert_eq!(stream.passed, 0);
        let _ = std::fs::remove_file(&path);
    }
}
