//! Silnik Santa Monica Studio: shadery w `exec\wad\pc_le\*.shaderpack`.
//! Tylko odczyt plików gry.
//!
//! - God of War Ragnarök (DirectX 12): każdy plik to jedna ramka LZ4 z
//!   paczką "SHDRPACK" w wersji 11. Nagłówek (128 B, little-endian): magia,
//!   u64 wersja, u64 liczby: shadery, root signature, programy, przebiegi;
//!   u64 położenia: tablica shaderów, tablica root signature, programy,
//!   przebiegi, dane shaderów, dane root signature; +0x78 rozmiar danych.
//!   - shader (104 B): u64 skrót, u32 typ, u32 rozmiar, u32 położenie w
//!     danych shaderów, reszta (nazwa) nieczytana; kontener DXIL,
//!   - root signature (16 B): u64 skrót, u32 położenie w danych root
//!     signature, u32 rozmiar; kontener z samą częścią RTS0,
//!   - program (3744 B): u64 skrót, u64 skrót root signature, ..., +0x688
//!     sześć u64 skrótów shaderów: VS, PS, VS z teselacją, HS, DS, CS
//!     (0 albo same jedynki = brak),
//!   - przebieg (904 B): stan przebiegu, nieczytany.
//!
//!   Program może wskazać shader albo root signature z innej paczki (wspólne
//!   są w `root.shaderpack`), więc skróty rozwiązujemy po wszystkich.
//! - God of War (2018, DirectX 11): "SHDRPACK" w wersji 1, plik bez ramki.
//!   Nagłówek 0x60 B: magia, u64 wersja, u64 programy, u64, u64 liczba
//!   shaderów (+0x20), ...; dalej rekordy po 112 B: u64 skrót, u32 typ,
//!   u32 rozmiar spakowany, u32 rozmiar, u32 położenie od końca tablicy.
//!   Każdy shader to jeden blok LZ4 (bez ramki) z kontenerem DXBC.
//!
//! Ramka LZ4: magia 04 22 4D 18, FLG (wersja 01, bit 5 bloki niezależne,
//! bit 4 sumy bloków, bit 3 rozmiar treści, bit 2 suma treści, bit 0
//! słownik), BD (bity 6-4: rozmiar bloku, 4 = 64 KB ... 7 = 4 MB), u64
//! rozmiar treści, bajt sumy nagłówka; bloki: u32 rozmiar (bit 31 = blok
//! niespakowany, 0 = koniec), dane, u32 suma bloku. Czytamy tylko ramki z
//! niezależnymi blokami bez słownika - wtedy blok rozpakowuje się sam i można
//! czytać dowolny fragment. Sumy kontrolne są pomijane (nie sprawdzane).
//!
//! Pamięć: `index` rozpakowuje paczki po kolei, blok po bloku (kontenery w
//! kolejności położeń), i zostawia tylko opis shaderów (etap, sygnatury) oraz
//! tablicę bloków każdej paczki. Bajty shaderów to `Bytes::Lazy`: przy użyciu
//! rozpakowują tylko bloki, na które przypada kontener (zwykle 1-2 po
//! 64 KB), z małą pamięcią ostatnich bloków na paczkę wspólną dla wątków, i
//! sprawdzają, czy kontener się nie zmienił (aktualizacja gry). Same
//! kontenery mają w Ragnaröku ponad 1 GB - nie trzymamy ich w pamięci.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::container::{read_at, Container, Kind, SigElement};
use super::readers::{Bytes, Detected, Pipeline12, Set12};

const ENGINE: &str = "Santa Monica";
const PACK_DIR: [&str; 3] = ["exec", "wad", "pc_le"];
const ROOT_PACK: &str = "root.shaderpack";
const MAGIC: &[u8; 8] = b"SHDRPACK";

/// Bajtów czytanych przez `detect` (nagłówki i pierwsze bloki ramek).
const DETECT_BUDGET: u64 = 8 << 20;
const DETECT_FILES: usize = 16;
/// Najwięcej plików paczek i największy kontener.
const MAX_FILES: usize = 20_000;
const MAX_CONTAINER: u32 = 32 << 20;
/// Najwięcej rekordów tablic ze wszystkich paczek i w jednej tablicy.
const MAX_RECORDS: u64 = 1 << 22;
const MAX_COUNT: u64 = 1 << 20;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// Czy bajty zaczynają paczkę danej wersji (z pełnym nagłówkiem).
fn is_pack(head: &[u8], version: u64, header: usize) -> bool {
    head.len() >= header && head.starts_with(MAGIC) && le64(head, 8) == Some(version)
}

/// Pliki paczek gry (ścieżka, rozmiar), posortowane po ścieżce.
fn packs(game: &Path) -> Vec<(PathBuf, u64)> {
    let dir = PACK_DIR.iter().fold(game.to_path_buf(), |p, s| p.join(s));
    let mut out = Vec::new();
    collect(&dir, 0, &mut out);
    out.sort_by_key(|(p, _)| p.to_string_lossy().to_lowercase());
    out
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let Ok(t) = e.file_type() else { continue };
        let path = e.path();
        if t.is_dir() && depth < 3 {
            collect(&path, depth + 1, out);
        } else if t.is_file()
            && path.extension().is_some_and(|x| x.eq_ignore_ascii_case("shaderpack"))
        {
            out.push((path, e.metadata().map(|m| m.len()).unwrap_or(0)));
        }
    }
}

fn is_root(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(ROOT_PACK))
}

/// Kolejność dla `detect`: najpierw `root.shaderpack`, potem najmniejsze.
fn detect_order(files: &[(PathBuf, u64)]) -> Vec<&Path> {
    let mut v: Vec<&(PathBuf, u64)> = files.iter().collect();
    v.sort_by_key(|(p, len)| (!is_root(p), *len));
    v.into_iter().take(DETECT_FILES).map(|(p, _)| p.as_path()).collect()
}

/// Początek pliku (najwyżej `n` bajtów).
fn head(path: &Path, n: usize) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut b = vec![0u8; (len.min(n as u64)) as usize];
    read_at(&file, 0, &mut b).ok()?;
    Some(b)
}

// --------------------------------- ramka LZ4 ---------------------------------

const FRAME_MAGIC: u32 = 0x184D_2204;
/// Największa treść ramki.
const MAX_FRAME: u64 = 16 << 30;
/// Bloków pamiętanych na ramkę (wspólnie dla wątków).
const CACHED_BLOCKS: usize = 4;
/// Najwięcej bajtów jednego odczytu z ramki.
const MAX_READ: usize = 64 << 20;

/// Nagłówek ramki: gdzie jest rozmiar pierwszego bloku, rozmiar bloku, czy
/// bloki mają sumy, rozmiar treści (jeśli podany).
struct FrameHead {
    first: usize,
    block: usize,
    checksums: bool,
    content: Option<u64>,
}

fn frame_head(b: &[u8]) -> Option<FrameHead> {
    if le32(b, 0)? != FRAME_MAGIC {
        return None;
    }
    let (flg, bd) = (*b.get(4)?, *b.get(5)?);
    // Wersja 01, bloki niezależne, bez słownika i bitu zarezerwowanego.
    if flg >> 6 != 1 || flg & 0x20 == 0 || flg & 0x03 != 0 || bd & 0x8F != 0 {
        return None;
    }
    let code = (bd >> 4) & 7;
    if code < 4 {
        return None;
    }
    let sized = flg & 0x08 != 0;
    Some(FrameHead {
        first: 6 + if sized { 8 } else { 0 } + 1,
        block: 1 << (2 * code as usize + 8),
        checksums: flg & 0x10 != 0,
        content: if sized { Some(le64(b, 6)?) } else { None },
    })
}

/// Blok ramki w pliku: położenie danych, rozmiar zapisany, czy niespakowany.
#[derive(Clone, Copy)]
struct Block {
    at: u64,
    size: u32,
    raw: bool,
}

/// Ramka LZ4 czytana w dowolnym miejscu (rozpakowuje tylko potrzebne bloki).
struct Frame {
    file: File,
    block: usize,
    blocks: Vec<Block>,
    len: u64,
    cache: Mutex<Vec<(usize, Arc<Vec<u8>>)>>,
}

impl Frame {
    fn open(path: &Path) -> Result<Frame, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let flen = file.metadata().map_err(|e| e.to_string())?.len();
        let mut h = [0u8; 19];
        let n = flen.min(h.len() as u64) as usize;
        read_at(&file, 0, &mut h[..n]).map_err(|e| e.to_string())?;
        let head = frame_head(&h[..n]).ok_or("to nie ramka LZ4 / not an LZ4 frame")?;
        let mut blocks = Vec::new();
        let mut pos = head.first as u64;
        loop {
            let mut w = [0u8; 4];
            read_at(&file, pos, &mut w).map_err(|_| "ramka ucięta / frame truncated")?;
            let v = u32::from_le_bytes(w);
            if v == 0 {
                break;
            }
            let size = v & 0x7FFF_FFFF;
            let end = pos + 4 + size as u64;
            if size == 0 || size as usize > head.block || end > flen {
                return Err("zły rozmiar bloku LZ4 / bad LZ4 block size".into());
            }
            if (blocks.len() as u64 + 1) * head.block as u64 > MAX_FRAME {
                return Err("ramka za duża / frame too large".into());
            }
            blocks.push(Block { at: pos + 4, size, raw: v >> 31 == 1 });
            pos = end + if head.checksums { 4 } else { 0 };
        }
        let Some(last) = blocks.len().checked_sub(1) else {
            return Err("pusta ramka LZ4 / empty LZ4 frame".into());
        };
        let mut f = Frame { file, block: head.block, blocks, len: 0, cache: Mutex::new(Vec::new()) };
        f.len = last as u64 * f.block as u64 + f.block_at(last)?.len() as u64;
        if head.content.is_some_and(|c| c != f.len) {
            return Err("rozmiar treści ramki się nie zgadza / frame content size mismatch".into());
        }
        Ok(f)
    }

    /// Rozpakowany blok `k`; każdy oprócz ostatniego ma pełny rozmiar.
    fn block_at(&self, k: usize) -> Result<Arc<Vec<u8>>, String> {
        if let Ok(c) = self.cache.lock() {
            if let Some((_, b)) = c.iter().find(|(i, _)| *i == k) {
                return Ok(b.clone());
            }
        }
        let b = *self.blocks.get(k).ok_or("blok poza ramką / block past the frame")?;
        let mut stored = vec![0u8; b.size as usize];
        read_at(&self.file, b.at, &mut stored).map_err(|_| "ramka ucięta / frame truncated")?;
        let data = if b.raw { stored } else { crate::lz4::decompress(&stored, self.block)? };
        if k + 1 < self.blocks.len() && data.len() != self.block {
            return Err("blok LZ4 niepełny / LZ4 block not full".into());
        }
        let data = Arc::new(data);
        if let Ok(mut c) = self.cache.lock() {
            c.insert(0, (k, data.clone()));
            c.truncate(CACHED_BLOCKS);
        }
        Ok(data)
    }

    /// `count` bajtów treści od `from`.
    fn read(&self, from: u64, count: usize) -> Result<Vec<u8>, String> {
        let end = from.checked_add(count as u64).filter(|&e| e <= self.len && count <= MAX_READ);
        let end = end.ok_or("odczyt poza ramką / read past the frame")?;
        let mut out = Vec::with_capacity(count);
        let mut pos = from;
        while pos < end {
            let k = (pos / self.block as u64) as usize;
            let data = self.block_at(k)?;
            let lo = (pos - k as u64 * self.block as u64) as usize;
            let n = ((data.len().saturating_sub(lo)) as u64).min(end - pos) as usize;
            if n == 0 {
                return Err("odczyt poza ramką / read past the frame".into());
            }
            out.extend_from_slice(&data[lo..lo + n]);
            pos += n as u64;
        }
        Ok(out)
    }
}

/// Pierwszy blok ramki w pliku, rozpakowany, jeśli mieści się w `budget`;
/// `read` rośnie o przeczytane bajty.
fn first_block(path: &Path, budget: u64, read: &mut u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let flen = file.metadata().ok()?.len();
    let mut h = [0u8; 23];
    let n = flen.min(h.len() as u64) as usize;
    read_at(&file, 0, &mut h[..n]).ok()?;
    *read += n as u64;
    let head = frame_head(&h[..n])?;
    let v = le32(&h[..n], head.first)?;
    let size = v & 0x7FFF_FFFF;
    let at = head.first as u64 + 4;
    if v == 0 || size as usize > head.block || size as u64 > budget.saturating_sub(n as u64) || at + size as u64 > flen {
        return None;
    }
    let mut stored = vec![0u8; size as usize];
    read_at(&file, at, &mut stored).ok()?;
    *read += size as u64;
    if v >> 31 == 1 {
        Some(stored)
    } else {
        crate::lz4::decompress(&stored, head.block).ok()
    }
}

// ------------------------------ SHDRPACK 11 ----------------------------------

const V11: u64 = 11;
const HEADER11: usize = 128;
const SHADER_RECORD: usize = 104;
const ROOT_RECORD: usize = 16;
const PROGRAM_RECORD: usize = 3744;
const PASS_RECORD: usize = 904;
const SLOTS_AT: usize = 0x688;
/// Bajtów tablicy czytanych naraz.
const CHUNK: usize = 1 << 20;

/// Wpis tablicy: skrót, położenie w treści ramki, rozmiar.
#[derive(Clone, Copy)]
struct Entry {
    hash: u64,
    at: u64,
    size: u32,
}

/// Program: skrót root signature i shadery gniazd (VS, PS, VS z teselacją,
/// HS, DS, CS).
#[derive(Clone, Copy)]
struct Program {
    rs: u64,
    slots: [u64; 6],
}

struct Pack11 {
    shaders: Vec<Entry>,
    roots: Vec<Entry>,
    programs: Vec<Program>,
}

/// Dlaczego paczka nie została przeczytana.
enum Skip {
    NotPack,
    OverCap,
    Broken,
}

/// Rekordy tablicy czytane kawałkami (nagłówek obiecujący więcej rekordów,
/// niż ma ramka, kończy się błędem przed przydziałem całości).
fn records(f: &Frame, at: u64, n: u64, size: usize, each: &mut dyn FnMut(&[u8]) -> Option<()>) -> Result<(), Skip> {
    let per = (CHUNK / size).max(1) as u64;
    let mut i = 0;
    while i < n {
        let k = per.min(n - i);
        let b = f.read(at + i * size as u64, k as usize * size).map_err(|_| Skip::Broken)?;
        for r in b.chunks_exact(size) {
            each(r).ok_or(Skip::Broken)?;
        }
        i += k;
    }
    Ok(())
}

fn read_pack11(f: &Frame, budget: &mut u64) -> Result<Pack11, Skip> {
    if f.len < HEADER11 as u64 {
        return Err(Skip::NotPack);
    }
    let h = f.read(0, HEADER11).map_err(|_| Skip::Broken)?;
    if !is_pack(&h, V11, HEADER11) {
        return Err(Skip::NotPack);
    }
    let u = |at: usize| le64(&h, at).unwrap_or(u64::MAX);
    let (n_shaders, n_roots, n_programs, n_passes) = (u(0x10), u(0x18), u(0x20), u(0x28));
    let (shader_table, root_table, program_table, pass_table) = (u(0x30), u(0x38), u(0x40), u(0x48));
    let (data, root_data) = (u(0x50), u(0x58));
    let len = f.len;
    let fits = |at: u64, n: u64, size: usize| {
        n <= MAX_COUNT && at >= HEADER11 as u64 && at <= len && n * size as u64 <= len - at
    };
    if !fits(shader_table, n_shaders, SHADER_RECORD)
        || !fits(root_table, n_roots, ROOT_RECORD)
        || !fits(program_table, n_programs, PROGRAM_RECORD)
        || !fits(pass_table, n_passes, PASS_RECORD)
        || data > len
        || root_data > len
    {
        return Err(Skip::Broken);
    }
    let total = n_shaders + n_roots + n_programs;
    if total > *budget {
        return Err(Skip::OverCap);
    }
    *budget -= total;
    // Wpis kontenera: rozmiar w granicach i w treści ramki.
    let entry = |r: &[u8], base: u64, at_off: usize, size_off: usize| -> Option<Entry> {
        let size = le32(r, size_off)?;
        let at = base.checked_add(le32(r, at_off)? as u64)?;
        ((32..=MAX_CONTAINER).contains(&size) && at + size as u64 <= len).then_some(Entry { hash: le64(r, 0)?, at, size })
    };
    let mut shaders = Vec::with_capacity(n_shaders as usize);
    records(f, shader_table, n_shaders, SHADER_RECORD, &mut |r| {
        shaders.push(entry(r, data, 16, 12)?);
        Some(())
    })?;
    let mut roots = Vec::with_capacity(n_roots as usize);
    records(f, root_table, n_roots, ROOT_RECORD, &mut |r| {
        roots.push(entry(r, root_data, 8, 12)?);
        Some(())
    })?;
    let mut programs = Vec::with_capacity(n_programs as usize);
    records(f, program_table, n_programs, PROGRAM_RECORD, &mut |r| {
        let mut slots = [0u64; 6];
        for (k, s) in slots.iter_mut().enumerate() {
            *s = le64(r, SLOTS_AT + 8 * k)?;
        }
        programs.push(Program { rs: le64(r, 8)?, slots });
        Some(())
    })?;
    Ok(Pack11 { shaders, roots, programs })
}

/// Czy gniazdo programu wskazuje shader.
fn has(slot: u64) -> bool {
    slot != 0 && slot != u64::MAX
}

/// Etap oczekiwany w gnieździe i jego miejsce w `Pipeline12::stages`.
const SLOTS: [(Kind, usize); 6] = [
    (Kind::Vertex, 0),
    (Kind::Pixel, 1),
    (Kind::Vertex, 0),
    (Kind::Hull, 3),
    (Kind::Domain, 4),
    (Kind::Compute, 5),
];

/// Potok: VS (z PS albo sam - przebieg głębi), VS z teselacją + HS + DS (z
/// PS albo bez) albo sam CS.
fn is_pipeline(s: &[u64; 6]) -> bool {
    let [vs, ps, ls, hs, ds, cs] = s.map(has);
    if cs {
        !(vs || ps || ls || hs || ds)
    } else if vs {
        !(ls || hs || ds)
    } else {
        ls && hs && ds
    }
}

/// Część RTS0, którą przyjmie `CreateRootSignature`: wersja 1.0-1.2, tablice
/// parametrów i samplerów w granicach.
fn rts0_valid(b: &[u8]) -> bool {
    let (Some(version), Some(params), Some(params_at), Some(samplers), Some(samplers_at)) =
        (le32(b, 0), le32(b, 4), le32(b, 8), le32(b, 12), le32(b, 16))
    else {
        return false;
    };
    let len = b.len() as u64;
    (1..=3).contains(&version)
        && params <= 64
        && samplers <= 2032
        && params_at as u64 + params as u64 * 12 <= len
        && samplers_at as u64 + samplers as u64 * 52 <= len
}

/// Opis shadera potrzebny do łączenia etapów.
struct Meta {
    kind: Kind,
    dxil: bool,
    inputs: Vec<SigElement>,
    outputs: Vec<SigElement>,
}

/// Shader z paczki: gdzie jest, tożsamość kontenera i opis.
struct Shader {
    frame: Arc<Frame>,
    at: u64,
    size: u32,
    id: [u8; 16],
    meta: Meta,
}

/// Wartości systemowe pisane przez poprzedni etap (clip/cull distance,
/// indeks celu i widoku).
const FROM_PREV: [u32; 4] = [2, 3, 4, 5];

/// Elementy bez wartości systemowych: (nazwa, indeks, rejestr, typ), posortowane.
fn plain(e: &[SigElement]) -> Vec<(&str, u32, u32, u32)> {
    let mut v: Vec<_> = e.iter().filter(|e| e.sysval == 0).map(|e| (e.name.as_str(), e.index, e.register, e.component)).collect();
    v.sort();
    v
}

/// Czy etapy się łączą (VS, HS, DS, PS kolejno): każde wejście czytane z
/// poprzedniego etapu jest zapisane (maska nadzbiorem), w tym samym rejestrze
/// i (bez wartości systemowych) tego samego typu; wyjścia HS to wejścia DS;
/// bez mieszania DXBC i DXIL. `None` - etap pisze jedno wyjście dwa razy.
fn chain(stages: &[&Meta]) -> Option<bool> {
    let mut order: Vec<&Meta> = stages.iter().copied().filter(|m| m.kind != Kind::Compute).collect();
    order.sort_by_key(|m| match m.kind {
        Kind::Vertex => 0,
        Kind::Hull => 1,
        Kind::Domain => 2,
        _ => 3,
    });
    for w in order.windows(2) {
        let (src, dst) = (w[0], w[1]);
        let mut outs: HashMap<(&str, u32), &SigElement> = HashMap::new();
        for o in &src.outputs {
            if outs.insert((o.name.as_str(), o.index), o).is_some() {
                return None;
            }
        }
        if src.dxil != dst.dxil {
            return Some(false);
        }
        if dst.kind == Kind::Domain && plain(&src.outputs) != plain(&dst.inputs) {
            return Some(false);
        }
        let control = matches!(dst.kind, Kind::Hull | Kind::Domain);
        for i in &dst.inputs {
            let o = outs.get(&(i.name.as_str(), i.index));
            if let Some(o) = o {
                if o.register != i.register || (i.sysval == 0 && o.component != i.component) {
                    return Some(false);
                }
            }
            if i.sysval != 0 && !FROM_PREV.contains(&i.sysval) && !control {
                continue;
            }
            match o {
                Some(o) if i.mask & !o.mask == 0 => {}
                _ => return Some(false),
            }
        }
    }
    Some(true)
}

/// Bajty shadera na żądanie: rozpakowuje tylko jego bloki i sprawdza, czy
/// kontener jest ten sam co przy indeksowaniu.
fn lazy(s: &Shader) -> Bytes {
    let (frame, at, size, id) = (s.frame.clone(), s.at, s.size, s.id);
    Bytes::Lazy(Arc::new(move || {
        let b = frame.read(at, size as usize).ok()?;
        let c = Container::parse(&b)?;
        (c.identity() == id).then(|| c.bytes.to_vec())
    }))
}

/// Gra z paczkami SHDRPACK 11 (God of War Ragnarök).
pub fn detect(game: &Path) -> Option<Detected> {
    let files = packs(game);
    let mut read = 0u64;
    let mut found = false;
    for p in detect_order(&files) {
        if read >= DETECT_BUDGET {
            break;
        }
        if first_block(p, DETECT_BUDGET - read, &mut read).is_some_and(|b| is_pack(&b, V11, HEADER11)) {
            found = true;
            break;
        }
    }
    found.then(|| Detected {
        engine: ENGINE,
        version: Some("SHDRPACK 11".into()),
        api: "DirectX 12",
        files,
        exe: Some("GoWR.exe".into()),
    })
}

/// Potoki DirectX 12: każdy program, którego etapy się łączą i którego root
/// signature jest w paczkach.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let files = packs(game);
    if files.is_empty() {
        return Err(r"brak plików exec\wad\pc_le\*.shaderpack / no exec\wad\pc_le\*.shaderpack files".into());
    }
    say(format!("Santa Monica: {0} paczek shaderów / {0} shader packs", files.len()));
    let mut shaders: Vec<Shader> = Vec::new();
    let mut by_id: HashMap<([u8; 16], usize), usize> = HashMap::new();
    let mut shader_of: HashMap<u64, usize> = HashMap::new();
    let mut set = Set12::default();
    let mut sig_ids: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut rs_of: HashMap<u64, usize> = HashMap::new();
    let mut programs: Vec<Program> = Vec::new();
    let (mut read_packs, mut unreadable, mut other, mut over_cap, mut bad, mut bad_rs) = (0, 0, 0, 0, 0, 0);
    let mut bytes = 0u64;
    let mut budget = MAX_RECORDS;
    for (path, _) in &files {
        let frame = match Frame::open(path) {
            Ok(f) => Arc::new(f),
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let pack = match read_pack11(&frame, &mut budget) {
            Ok(p) => p,
            Err(Skip::NotPack) => {
                other += 1;
                continue;
            }
            Err(Skip::OverCap) => {
                over_cap += 1;
                continue;
            }
            Err(Skip::Broken) => {
                unreadable += 1;
                continue;
            }
        };
        read_packs += 1;
        // Programy liczą się nawet wtedy, gdy kontenery paczki są uszkodzone.
        programs.extend_from_slice(&pack.programs);
        // Najpierw cała paczka, potem zapis: blok, który się nie rozpakuje,
        // nie zostawia połowy kontenerów.
        let mut order = pack.shaders.clone();
        order.sort_by_key(|e| e.at);
        let mut found: Vec<(u64, Shader)> = Vec::new();
        let mut roots: Vec<(u64, Vec<u8>)> = Vec::new();
        let (mut pack_bad, mut pack_bad_rs, mut pack_bytes) = (0, 0, 0u64);
        let whole = (|| -> Result<(), String> {
            for e in &order {
                let b = frame.read(e.at, e.size as usize)?;
                pack_bytes += b.len() as u64;
                let Some(c) = Container::parse(&b) else {
                    pack_bad += 1;
                    continue;
                };
                let Some(kind) = c.kind().filter(|k| *k != Kind::Other) else {
                    pack_bad += 1;
                    continue;
                };
                let meta = Meta { kind, dxil: c.is_dxil(), inputs: c.inputs(), outputs: c.outputs() };
                let s = Shader { frame: frame.clone(), at: e.at, size: e.size, id: c.identity(), meta };
                found.push((e.hash, s));
            }
            for e in &pack.roots {
                let b = frame.read(e.at, e.size as usize)?;
                match Container::parse(&b) {
                    Some(c) if c.kind().is_none() && c.root_signature().is_some_and(rts0_valid) => {
                        roots.push((e.hash, c.root_signature().unwrap_or_default().to_vec()));
                    }
                    _ => pack_bad_rs += 1,
                }
            }
            Ok(())
        })();
        if whole.is_err() {
            unreadable += 1;
            continue;
        }
        (bad, bad_rs, bytes) = (bad + pack_bad, bad_rs + pack_bad_rs, bytes + pack_bytes);
        for (hash, s) in found {
            let key = (s.id, s.size as usize);
            let i = *by_id.entry(key).or_insert_with(|| {
                shaders.push(s);
                shaders.len() - 1
            });
            shader_of.entry(hash).or_insert(i);
        }
        for (hash, rs) in roots {
            let next = set.signatures.len();
            let i = *sig_ids.entry(rs.clone()).or_insert(next);
            if i == next {
                set.signatures.push(rs);
            }
            rs_of.entry(hash).or_insert(i);
        }
    }
    if read_packs == 0 {
        return Err("żadna paczka SHDRPACK 11 / no SHDRPACK 11 shader pack".into());
    }

    // Potoki; shadery w `Set12` tylko te, których potoki używają.
    let mut out_of: HashMap<usize, usize> = HashMap::new();
    let mut seen: std::collections::HashSet<(usize, [Option<usize>; 6])> = std::collections::HashSet::new();
    let (mut no_rs, mut missing, mut mismatched, mut odd, mut unlinked, mut malformed) = (0, 0, 0, 0, 0, 0);
    for p in &programs {
        let Some(&rs) = rs_of.get(&p.rs) else {
            no_rs += 1;
            continue;
        };
        if !is_pipeline(&p.slots) {
            odd += 1;
            continue;
        }
        let mut stages: [Option<usize>; 6] = [None; 6];
        let mut ok = true;
        for (slot, &(kind, place)) in p.slots.iter().zip(&SLOTS) {
            if !has(*slot) {
                continue;
            }
            match shader_of.get(slot) {
                None => {
                    missing += 1;
                    ok = false;
                }
                Some(&i) if shaders[i].meta.kind != kind => {
                    mismatched += 1;
                    ok = false;
                }
                Some(&i) => stages[place] = Some(i),
            }
            if !ok {
                break;
            }
        }
        if !ok {
            continue;
        }
        let metas: Vec<&Meta> = stages.iter().flatten().map(|&i| &shaders[i].meta).collect();
        match chain(&metas) {
            None => {
                malformed += 1;
                continue;
            }
            Some(false) => {
                unlinked += 1;
                continue;
            }
            Some(true) => {}
        }
        if !seen.insert((rs, stages)) {
            continue;
        }
        let stages = stages.map(|s| {
            s.map(|i| {
                *out_of.entry(i).or_insert_with(|| {
                    set.shaders.push(lazy(&shaders[i]));
                    set.shaders.len() - 1
                })
            })
        });
        set.pipelines.push(Pipeline12 { rs: Some(rs), stages });
    }

    let mut note = format!(
        "{read_packs} paczek ({:.2} GB kontenerów): {} shaderów, {} root signature; {} programów -> {} potoków / \
         {read_packs} packs ({:.2} GB of containers): {} shaders, {} root signatures; {} programs -> {} pipelines",
        bytes as f64 / 1e9,
        shaders.len(),
        set.signatures.len(),
        programs.len(),
        set.pipelines.len(),
        bytes as f64 / 1e9,
        shaders.len(),
        set.signatures.len(),
        programs.len(),
        set.pipelines.len(),
    );
    let mut add = |n: usize, pl: &str, en: &str| {
        if n > 0 {
            note.push_str(&format!("; {n} {pl} / {n} {en}"));
        }
    };
    add(no_rs, "programów wskazuje root signature, której nie ma", "programs name a root signature no pack has");
    add(missing, "programów wskazuje shader, którego nie ma", "programs name a shader no pack has");
    add(mismatched, "programów wskazuje shader innego etapu", "programs name a shader of another stage");
    add(odd, "programów to nie potok", "programs aren't a pipeline");
    add(unlinked, "programów pominiętych: etapy się nie łączą", "programs left out: their stages don't link");
    add(malformed, "programów pominiętych: etap pisze wyjście dwa razy", "programs left out: a stage writes one output twice");
    add(bad_rs, "root signature odrzuconych", "root signatures rejected");
    add(bad, "kontenerów nieczytelnych", "unparseable containers");
    add(other, "paczek innej wersji", "packs of another version");
    add(over_cap, "paczek ponad limit rekordów", "packs over the record cap");
    add(unreadable, "paczek się nie rozpakowuje", "packs don't decompress");
    say(note.clone());
    set.notes.push(note);
    Ok(set)
}

// ------------------------------- SHDRPACK 1 ----------------------------------

const V1: u64 = 1;
const HEADER1: usize = 0x60;
const RECORD1: usize = 112;

/// Shader paczki w wersji 1: blok LZ4 w pliku i rozmiar po rozpakowaniu.
struct Rec1 {
    at: u64,
    packed: u32,
    size: u32,
}

fn read_pack1(file: &File) -> Option<Vec<Rec1>> {
    let len = file.metadata().ok()?.len();
    let mut h = [0u8; HEADER1];
    if len < HEADER1 as u64 {
        return None;
    }
    read_at(file, 0, &mut h).ok()?;
    if !is_pack(&h, V1, HEADER1) {
        return None;
    }
    let n = le64(&h, 0x20)?;
    let data = HEADER1 as u64 + n.checked_mul(RECORD1 as u64)?;
    if n > MAX_COUNT || data > len {
        return None;
    }
    let mut table = vec![0u8; n as usize * RECORD1];
    read_at(file, HEADER1 as u64, &mut table).ok()?;
    table
        .as_chunks::<RECORD1>()
        .0
        .iter()
        .map(|r| {
            let (packed, size) = (le32(r, 12)?, le32(r, 16)?);
            let at = data + le32(r, 20)? as u64;
            let ok = (1..=MAX_CONTAINER).contains(&packed) && (32..=MAX_CONTAINER).contains(&size) && at + packed as u64 <= len;
            ok.then_some(Rec1 { at, packed, size })
        })
        .collect()
}

/// Gra z paczkami SHDRPACK 1 (God of War z 2018 r.).
pub fn detect_v1(game: &Path) -> Option<Detected> {
    let files = packs(game);
    let found = detect_order(&files).into_iter().any(|p| head(p, HEADER1).is_some_and(|h| is_pack(&h, V1, HEADER1)));
    found.then(|| Detected {
        engine: ENGINE,
        version: Some("SHDRPACK 1".into()),
        api: "DirectX 11",
        files,
        exe: Some("GoW.exe".into()),
    })
}

/// Każdy kontener DXBC paczek w wersji 1 raz (pule, bez programów).
pub fn for_each_shader_v1(
    game: &Path,
    progress: &mut dyn FnMut(usize, usize),
    each: &mut dyn FnMut(&[u8]),
) -> Result<usize, String> {
    let files = packs(game);
    if files.is_empty() {
        return Err(r"brak plików exec\wad\pc_le\*.shaderpack / no exec\wad\pc_le\*.shaderpack files".into());
    }
    let total = files.len();
    progress(0, total);
    let mut seen: std::collections::HashSet<([u8; 16], usize)> = std::collections::HashSet::new();
    let (mut count, mut read_packs) = (0usize, 0usize);
    for (done, (path, _)) in files.iter().enumerate() {
        if let Some((file, mut recs)) = File::open(path).ok().and_then(|f| read_pack1(&f).map(|r| (f, r))) {
            read_packs += 1;
            recs.sort_by_key(|r| r.at);
            for r in &recs {
                let mut raw = vec![0u8; r.packed as usize];
                if read_at(&file, r.at, &mut raw).is_err() {
                    continue;
                }
                let Ok(b) = crate::lz4::decompress(&raw, r.size as usize) else { continue };
                if b.len() != r.size as usize {
                    continue;
                }
                let Some(c) = Container::parse(&b) else { continue };
                if c.kind().is_some() && seen.insert((c.identity(), c.bytes.len())) {
                    each(c.bytes);
                    count += 1;
                }
            }
        }
        progress(done + 1, total);
    }
    if read_packs == 0 {
        return Err("żadna paczka SHDRPACK 1 / no SHDRPACK 1 shader pack".into());
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::container::tests::{build, dxil, sig1};

    /// Katalog gry na próbę (usuwany na końcu testu).
    struct Game(PathBuf);

    impl Game {
        fn new(name: &str) -> Game {
            let root = std::env::temp_dir().join(format!("tc-santamonica-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join(r"exec/wad/pc_le")).unwrap();
            Game(root)
        }
        fn put(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join("exec/wad/pc_le").join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }

    impl Drop for Game {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Ramka LZ4 z blokami 64 KB: pełne niespakowane, ostatni niepełny jako
    /// literały; rozmiar treści, sumy bloków i treści (wartości dowolne).
    fn frame(content: &[u8]) -> Vec<u8> {
        let mut f = FRAME_MAGIC.to_le_bytes().to_vec();
        f.push(0x40 | 0x20 | 0x10 | 0x08 | 0x04);
        f.push(4 << 4);
        f.extend_from_slice(&(content.len() as u64).to_le_bytes());
        f.push(0xAB);
        for chunk in content.chunks(64 << 10) {
            if chunk.len() == 64 << 10 {
                f.extend_from_slice(&(chunk.len() as u32 | 0x8000_0000).to_le_bytes());
                f.extend_from_slice(chunk);
            } else {
                let b = crate::lz4::compress_literals(chunk);
                f.extend_from_slice(&(b.len() as u32).to_le_bytes());
                f.extend_from_slice(&b);
            }
            f.extend_from_slice(&[1, 2, 3, 4]);
        }
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&[5, 6, 7, 8]);
        f
    }

    #[test]
    fn reads_lz4_frame_across_blocks() {
        let game = Game::new("frame");
        let content: Vec<u8> = (0..150_000u32).map(|i| (i * 31 % 253) as u8).collect();
        let path = game.put("a.shaderpack", &frame(&content));
        let f = Frame::open(&path).unwrap();
        assert_eq!(f.len, content.len() as u64);
        assert_eq!(f.blocks.len(), 3);
        assert_eq!(f.read(65_530, 70_000).unwrap(), &content[65_530..135_530]);
        assert_eq!(f.read(0, content.len()).unwrap(), content);
        assert!(f.read(149_990, 11).is_err());
        let mut read = 0;
        assert_eq!(first_block(&path, 1 << 20, &mut read).unwrap(), &content[..64 << 10]);
        assert!(first_block(&path, 1000, &mut 0).is_none());
        // Bloki zależne i ucięta ramka nie są czytane.
        let mut dependent = frame(&content);
        dependent[4] &= !0x20;
        assert!(Frame::open(&game.put("b.shaderpack", &dependent)).is_err());
        let whole = frame(&content);
        assert!(Frame::open(&game.put("c.shaderpack", &whole[..whole.len() - 9])).is_err());
    }

    fn rs_container() -> Vec<u8> {
        let rs = vec![2u8, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0];
        build(&[(b"RTS0", rs)])
    }

    /// Paczka w wersji 11: shadery, root signature, programy (rs, gniazda);
    /// dane shaderów za wypełnieniem, żeby przechodziły przez granicę bloków.
    fn pack11(shaders: &[(u64, Vec<u8>)], roots: &[(u64, Vec<u8>)], programs: &[(u64, [u64; 6])]) -> Vec<u8> {
        let shader_table = HEADER11;
        let root_table = shader_table + shaders.len() * SHADER_RECORD;
        let program_table = root_table + roots.len() * ROOT_RECORD;
        let pass_table = program_table + programs.len() * PROGRAM_RECORD;
        let data = (pass_table + 70_000).max(65_000);
        let mut shader_data = Vec::new();
        let mut b = vec![0u8; data];
        b[..8].copy_from_slice(MAGIC);
        let put = |b: &mut Vec<u8>, at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        for (k, v) in [V11, shaders.len() as u64, roots.len() as u64, programs.len() as u64, 0].iter().enumerate() {
            put(&mut b, 8 + 8 * k, *v);
        }
        for (k, (hash, c)) in shaders.iter().enumerate() {
            let r = shader_table + k * SHADER_RECORD;
            put(&mut b, r, *hash);
            b[r + 12..r + 16].copy_from_slice(&(c.len() as u32).to_le_bytes());
            b[r + 16..r + 20].copy_from_slice(&(shader_data.len() as u32).to_le_bytes());
            shader_data.extend_from_slice(c);
        }
        let root_data_at = data + shader_data.len();
        let mut root_data = Vec::new();
        for (k, (hash, c)) in roots.iter().enumerate() {
            let r = root_table + k * ROOT_RECORD;
            put(&mut b, r, *hash);
            b[r + 8..r + 12].copy_from_slice(&(root_data.len() as u32).to_le_bytes());
            b[r + 12..r + 16].copy_from_slice(&(c.len() as u32).to_le_bytes());
            root_data.extend_from_slice(c);
        }
        for (k, (rs, slots)) in programs.iter().enumerate() {
            let r = program_table + k * PROGRAM_RECORD;
            put(&mut b, r, 1000 + k as u64);
            put(&mut b, r + 8, *rs);
            for (s, v) in slots.iter().enumerate() {
                put(&mut b, r + SLOTS_AT + 8 * s, *v);
            }
        }
        for (k, v) in [shader_table, root_table, program_table, pass_table, data, root_data_at].iter().enumerate() {
            put(&mut b, 0x30 + 8 * k, *v as u64);
        }
        put(&mut b, 0x78, shader_data.len() as u64);
        b.extend_from_slice(&shader_data);
        b.extend_from_slice(&root_data);
        b
    }

    #[test]
    fn indexes_v11_programs_across_packs() {
        let game = Game::new("v11");
        let vs = build(&[
            (b"ISG1", sig1(&[("POSITION", 0, 0, 3, 0, 0x7)])),
            (b"OSG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"DXIL", dxil(1)),
        ]);
        let ps = build(&[
            (b"ISG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"OSG1", sig1(&[("SV_Target", 0, 64, 3, 0, 0xF)])),
            (b"DXIL", dxil(0)),
        ]);
        let ps_bad = build(&[
            (b"ISG1", sig1(&[("TEXCOORD", 1, 0, 3, 2, 0x3)])),
            (b"OSG1", sig1(&[("SV_Target", 0, 64, 3, 0, 0xF)])),
            (b"DXIL", dxil(0)),
        ]);
        let cs = build(&[(b"DXIL", dxil(5))]);
        let (r, none) = (0x5151, 0x9999);
        let a = pack11(
            &[(1, vs.clone()), (2, ps.clone()), (3, ps_bad), (4, cs.clone())],
            &[],
            &[
                (r, [1, 2, 0, 0, 0, 0]),
                (r, [1, 3, 0, 0, 0, 0]),
                (none, [1, 2, 0, 0, 0, 0]),
                (r, [0, 0, 0, 0, 0, 4]),
                (r, [1, 2, 0, 0, 0, u64::MAX]),
                (r, [2, 1, 0, 0, 0, 0]),
            ],
        );
        game.put("a.shaderpack", &frame(&a));
        game.put(ROOT_PACK, &frame(&pack11(&[], &[(r, rs_container())], &[])));
        game.put("junk.shaderpack", b"not a frame");

        let d = detect(&game.0).unwrap();
        assert_eq!((d.engine, d.version.as_deref(), d.api), (ENGINE, Some("SHDRPACK 11"), "DirectX 12"));
        assert_eq!(d.files.len(), 3);
        assert!(detect_v1(&game.0).is_none());

        let set = index(&game.0, &|_| {}).unwrap();
        let rsc = rs_container();
        assert_eq!(set.signatures, vec![Container::parse(&rsc).unwrap().root_signature().unwrap().to_vec()]);
        assert_eq!(set.pipelines.len(), 2, "{:?}", set.notes);
        assert_eq!(set.pipelines[0], Pipeline12 { rs: Some(0), stages: [Some(0), Some(1), None, None, None, None] });
        assert_eq!(set.pipelines[1], Pipeline12 { rs: Some(0), stages: [None, None, None, None, None, Some(2)] });
        assert_eq!(set.shaders.len(), 3);
        assert_eq!(set.shaders[0].get().unwrap(), vs);
        assert_eq!(set.shaders[1].get().unwrap(), ps);
        assert_eq!(set.shaders[2].get().unwrap(), cs);
        let note = &set.notes[0];
        assert!(note.contains("1 programs name a root signature no pack has"), "{note}");
        assert!(note.contains("1 programs left out: their stages don't link"), "{note}");
        assert!(note.contains("1 programs name a shader of another stage"), "{note}");
    }

    #[test]
    fn links_tessellation_and_rejects_mixes() {
        let el = |name: &str, sysval, reg, mask| SigElement { name: name.into(), index: 0, sysval, component: 3, register: reg, mask };
        let meta = |kind, inputs, outputs| Meta { kind, dxil: true, inputs, outputs };
        let cp = vec![el("p", 0, 0, 0xF)];
        let vs = meta(Kind::Vertex, vec![], cp.clone());
        let hs = meta(Kind::Hull, cp.clone(), cp.clone());
        let ds = meta(Kind::Domain, cp.clone(), vec![el("sv_position", 1, 0, 0xF)]);
        let ps = meta(Kind::Pixel, vec![el("sv_position", 1, 0, 0xF)], vec![]);
        assert_eq!(chain(&[&ps, &ds, &hs, &vs]), Some(true));
        let ds_other = meta(Kind::Domain, vec![el("q", 0, 0, 0xF)], vec![]);
        assert_eq!(chain(&[&vs, &hs, &ds_other]), Some(false));
        let twice = meta(Kind::Vertex, vec![], vec![el("p", 0, 0, 1), el("p", 0, 1, 1)]);
        assert_eq!(chain(&[&twice, &ps]), None);
        let dxbc = Meta { dxil: false, ..meta(Kind::Pixel, vec![], vec![]) };
        assert_eq!(chain(&[&vs, &dxbc]), Some(false));
        assert!(is_pipeline(&[0, 0, 1, 2, 3, 0]) && !is_pipeline(&[1, 0, 1, 2, 3, 0]) && !is_pipeline(&[0, 1, 0, 0, 0, 0]));
    }

    #[test]
    fn reads_v1_packs() {
        let game = Game::new("v1");
        let a = build(&[(b"SHEX", dxil(1))]);
        let b = build(&[(b"SHEX", dxil(0)), (b"ISGN", vec![0u8; 8])]);
        let blocks: Vec<Vec<u8>> = [&a, &b, &a].iter().map(|c| crate::lz4::compress_literals(c)).collect();
        let mut f = vec![0u8; HEADER1];
        f[..8].copy_from_slice(MAGIC);
        f[8..16].copy_from_slice(&V1.to_le_bytes());
        f[0x20..0x28].copy_from_slice(&4u64.to_le_bytes());
        let mut data = Vec::new();
        let sizes = [a.len(), b.len(), a.len()];
        for (k, blk) in blocks.iter().enumerate() {
            let mut r = vec![0u8; RECORD1];
            r[12..16].copy_from_slice(&(blk.len() as u32).to_le_bytes());
            r[16..20].copy_from_slice(&(sizes[k] as u32).to_le_bytes());
            r[20..24].copy_from_slice(&(data.len() as u32).to_le_bytes());
            f.extend_from_slice(&r);
            data.extend_from_slice(blk);
        }
        // Czwarty rekord: blok, który nie rozpakowuje się do rozmiaru.
        let mut r = vec![0u8; RECORD1];
        r[12..16].copy_from_slice(&(blocks[0].len() as u32).to_le_bytes());
        r[16..20].copy_from_slice(&(a.len() as u32 + 7).to_le_bytes());
        f.extend_from_slice(&r);
        f.extend_from_slice(&data);
        game.put("x.shaderpack", &f);
        game.put("y.shaderpack", b"SHDRPACK");

        let d = detect_v1(&game.0).unwrap();
        assert_eq!((d.version.as_deref(), d.api, d.exe.as_deref()), (Some("SHDRPACK 1"), "DirectX 11", Some("GoW.exe")));
        assert!(detect(&game.0).is_none());
        let (mut got, mut steps) = (Vec::new(), Vec::new());
        let n = for_each_shader_v1(&game.0, &mut |d, t| steps.push((d, t)), &mut |c| got.push(c.to_vec())).unwrap();
        assert_eq!(n, 2);
        assert_eq!(got, vec![a, b]);
        assert_eq!(steps, vec![(0, 2), (1, 2), (2, 2)]);
    }
}
