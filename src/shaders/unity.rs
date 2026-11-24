//! Odczyt shaderów DirectX 11 (kontenery DXBC) z plików gier Unity.
//!
//! Shadery leżą w obiektach `Shader` (klasa 48) i `ComputeShader` (klasa 72)
//! w plikach serializowanych Unity - luźnych (`level*`, `sharedassets*`,
//! `resources.assets`, `globalgamemanagers.assets`, `unity_builtin_extra`)
//! albo w paczkach UnityFS (`*.bundle`, bloki LZ4).
//!
//! `Shader` trzyma na końcu obiektu tablice: platformy, przesunięcia, długości
//! skompresowane i rozpakowane (od Unity 2019.3 tablice tablic - kilka
//! segmentów na platformę) oraz wspólny blob LZ4. Zamiast odtwarzać cały,
//! zmienny między wersjami układ `SerializedShader`, szukamy w obiekcie tego
//! zestawu tablic i sprawdzamy jego spójność; blob platformy D3D11 (4)
//! rozpakowujemy i wycinamy z niego kontenery DXBC. `ComputeShader` trzyma kod
//! bez kompresji, więc kontenery wycinamy wprost z obiektu.
//!
//! Format na podstawie publicznych opisów (UnityPy / AssetStudio, MIT) -
//! kod napisany od zera.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Platforma kompilatora shaderów Unity: Direct3D 11 (także dla DX12).
const PLATFORM_D3D11: u32 = 4;
const CLASS_SHADER: i32 = 48;
const CLASS_COMPUTE_SHADER: i32 = 72;

/// Etap shadera z tokenu wersji programu DXBC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Compute,
}

impl Stage {
    fn from_program_type(t: u32) -> Option<Stage> {
        Some(match t {
            0 => Stage::Pixel,
            1 => Stage::Vertex,
            2 => Stage::Geometry,
            3 => Stage::Hull,
            4 => Stage::Domain,
            5 => Stage::Compute,
            _ => return None,
        })
    }

    #[cfg(test)]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Jeden kontener DXBC (Shader Model 4/5).
#[derive(Clone, Debug)]
pub struct Dxbc<'a> {
    pub stage: Stage,
    pub code: &'a [u8],
}

impl Dxbc<'_> {
    /// Skrót z nagłówka kontenera (16 B) - do usuwania powtórzeń.
    pub fn digest(&self) -> [u8; 16] {
        self.code[4..20].try_into().unwrap()
    }
}

// ------------------------------ bajty ---------------------------------------

struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
    le: bool,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8], le: bool) -> Self {
        Cur { b, pos: 0, le }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or("koniec danych / unexpected end of data")?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        let a: [u8; 2] = self.take(2)?.try_into().unwrap();
        Ok(if self.le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    }
    fn u32(&mut self) -> Result<u32, String> {
        let a: [u8; 4] = self.take(4)?.try_into().unwrap();
        Ok(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }
    fn i32(&mut self) -> Result<i32, String> {
        self.u32().map(|v| v as i32)
    }
    fn u64(&mut self) -> Result<u64, String> {
        let a: [u8; 8] = self.take(8)?.try_into().unwrap();
        Ok(if self.le {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        })
    }
    fn cstr(&mut self) -> Result<&'a str, String> {
        let rest = &self.b[self.pos..];
        let n = rest
            .iter()
            .position(|&c| c == 0)
            .ok_or("brak końca tekstu")?;
        let s = std::str::from_utf8(&rest[..n]).map_err(|_| "tekst nie UTF-8")?;
        self.pos += n + 1;
        Ok(s)
    }
    fn align(&mut self, a: usize) {
        self.pos = self.pos.div_ceil(a) * a;
    }
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

// --------------------------- dostęp swobodny --------------------------------

/// Źródło bajtów z dostępem swobodnym: plik albo węzeł paczki.
trait Source {
    fn len(&self) -> u64;
    fn read_at(&mut self, off: u64, out: &mut [u8]) -> Result<(), String>;
    fn read_vec(&mut self, off: u64, len: usize) -> Result<Vec<u8>, String> {
        if off.checked_add(len as u64).is_none_or(|e| e > self.len()) {
            return Err("odczyt poza plikiem / read past end".into());
        }
        let mut v = vec![0; len];
        self.read_at(off, &mut v)?;
        Ok(v)
    }
}

struct FileSource {
    file: File,
    len: u64,
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, off: u64, out: &mut [u8]) -> Result<(), String> {
        self.file
            .seek(SeekFrom::Start(off))
            .map_err(|e| e.to_string())?;
        self.file.read_exact(out).map_err(|e| e.to_string())
    }
}

struct Block {
    /// Położenie skompresowanego bloku w pliku.
    file_off: u64,
    packed: u32,
    /// Początek bloku w rozpakowanym strumieniu.
    start: u64,
    size: u32,
    flags: u16,
}

/// Paczka UnityFS: rozpakowany strumień składany z bloków na żądanie.
struct Bundle {
    file: File,
    blocks: Vec<Block>,
    nodes: Vec<(u64, u64, String)>,
    cache: Option<(usize, Vec<u8>)>,
}

fn decompress(kind: u16, packed: &[u8], size: usize) -> Result<Vec<u8>, String> {
    match kind & 0x3F {
        0 => Ok(packed.to_vec()),
        2 | 3 => crate::lz4::decompress(packed, size).map_err(|e| format!("LZ4: {e}")),
        1 => Err("LZMA - nieobsługiwane / not supported".into()),
        k => Err(format!("kompresja {k} - nieobsługiwana / not supported")),
    }
}

impl Bundle {
    fn open(path: &Path) -> Result<Bundle, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let mut head = vec![0u8; 512.min(len as usize)];
        file.read_exact(&mut head).map_err(|e| e.to_string())?;
        let mut c = Cur::new(&head, false);
        if c.cstr()? != "UnityFS" {
            return Err("to nie UnityFS".into());
        }
        let version = c.u32()?;
        c.cstr()?;
        c.cstr()?;
        c.u64()?;
        let info_packed = c.u32()? as usize;
        let info_size = c.u32()? as usize;
        let flags = c.u32()?;
        if version >= 7 {
            c.align(16);
        }
        let mut data_start = c.pos as u64;
        let info_off = if flags & 0x80 != 0 {
            len.checked_sub(info_packed as u64).ok_or("zły rozmiar")?
        } else {
            data_start += info_packed as u64;
            c.pos as u64
        };
        let mut packed = vec![0u8; info_packed];
        file.seek(SeekFrom::Start(info_off))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut packed).map_err(|e| e.to_string())?;
        let info = decompress(flags as u16, &packed, info_size)?;
        if flags & 0x200 != 0 {
            data_start = data_start.div_ceil(16) * 16;
        }
        let mut c = Cur::new(&info, false);
        c.skip(16)?;
        let count = c.i32()?.max(0) as usize;
        let (mut blocks, mut file_off, mut start) = (Vec::with_capacity(count), data_start, 0u64);
        for _ in 0..count {
            let size = c.u32()?;
            let packed = c.u32()?;
            let flags = c.u16()?;
            blocks.push(Block {
                file_off,
                packed,
                start,
                size,
                flags,
            });
            file_off += packed as u64;
            start += size as u64;
        }
        let count = c.i32()?.max(0) as usize;
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            let off = c.u64()?;
            let size = c.u64()?;
            c.u32()?;
            nodes.push((off, size, c.cstr()?.to_owned()));
        }
        Ok(Bundle {
            file,
            blocks,
            nodes,
            cache: None,
        })
    }

    fn block(&mut self, i: usize) -> Result<&[u8], String> {
        if self.cache.as_ref().is_none_or(|(n, _)| *n != i) {
            let b = &self.blocks[i];
            let mut packed = vec![0u8; b.packed as usize];
            self.file
                .seek(SeekFrom::Start(b.file_off))
                .map_err(|e| e.to_string())?;
            self.file
                .read_exact(&mut packed)
                .map_err(|e| e.to_string())?;
            let data = decompress(b.flags, &packed, b.size as usize)?;
            self.cache = Some((i, data));
        }
        Ok(&self.cache.as_ref().unwrap().1)
    }

    fn read_at(&mut self, mut off: u64, out: &mut [u8]) -> Result<(), String> {
        let mut done = 0;
        while done < out.len() {
            let i = self
                .blocks
                .partition_point(|b| b.start + b.size as u64 <= off);
            if i >= self.blocks.len() {
                return Err("odczyt poza paczką / read past bundle".into());
            }
            let start = self.blocks[i].start;
            let data = self.block(i)?;
            let at = (off - start) as usize;
            let n = (data.len() - at).min(out.len() - done);
            out[done..done + n].copy_from_slice(&data[at..at + n]);
            done += n;
            off += n as u64;
        }
        Ok(())
    }
}

/// Węzeł (plik) w paczce.
struct Node<'a> {
    bundle: &'a mut Bundle,
    base: u64,
    len: u64,
}

impl Source for Node<'_> {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, off: u64, out: &mut [u8]) -> Result<(), String> {
        self.bundle.read_at(self.base + off, out)
    }
}

// -------------------------- plik serializowany ------------------------------

struct Object {
    class: i32,
    start: u64,
    size: u32,
}

struct Serialized {
    unity: String,
    objects: Vec<Object>,
}

fn parse_serialized(src: &mut dyn Source) -> Result<Serialized, String> {
    let len = src.len();
    if len < 48 {
        return Err("za krótki".into());
    }
    let head = src.read_vec(0, 48)?;
    let mut c = Cur::new(&head, false);
    let mut meta_size = c.u32()? as u64;
    let mut file_size = c.u32()? as u64;
    let version = c.u32()?;
    let mut data_off = c.u32()? as u64;
    let mut header_end = 16u64;
    let mut le = true;
    if !(9..=60).contains(&version) {
        return Err("to nie plik serializowany".into());
    }
    if version >= 9 {
        let e = c.u8()?;
        if e > 1 {
            return Err("zła kolejność bajtów".into());
        }
        le = e == 0;
        c.skip(3)?;
        header_end = 20;
    }
    if version >= 22 {
        meta_size = c.u32()? as u64;
        file_size = c.u64()?;
        data_off = c.u64()?;
        c.u64()?;
        header_end = 48;
    }
    if file_size > len || data_off > file_size || meta_size > file_size || meta_size == 0 {
        return Err("niespójny nagłówek".into());
    }
    let meta = src.read_vec(header_end, meta_size as usize)?;
    let mut c = Cur::new(&meta, le);
    let unity = if version >= 7 {
        c.cstr()?.to_owned()
    } else {
        String::new()
    };
    if version >= 8 {
        c.i32()?;
    }
    let type_tree = version >= 13 && c.u8()? != 0;
    let type_count = c.i32()?;
    if !(0..=100_000).contains(&type_count) {
        return Err("zła liczba typów".into());
    }
    let mut classes = Vec::with_capacity(type_count as usize);
    for _ in 0..type_count {
        let class = c.i32()?;
        if version >= 16 {
            c.u8()?;
        }
        if version >= 17 {
            c.u16()?;
        }
        if version >= 13 {
            if (version < 16 && class < 0) || (version >= 16 && class == 114) {
                c.skip(16)?;
            }
            c.skip(16)?;
        }
        if type_tree {
            if version < 12 && version != 10 {
                return Err("stary format drzewa typów - nieobsługiwany".into());
            }
            let nodes = c.i32()?.max(0) as usize;
            let strings = c.i32()?.max(0) as usize;
            c.skip(nodes * if version >= 19 { 32 } else { 24 } + strings)?;
            if version >= 21 {
                let deps = c.i32()?.max(0) as usize;
                c.skip(deps * 4)?;
            }
        }
        classes.push(class);
    }
    let big_ids = (7..14).contains(&version) && c.i32()? != 0;
    let count = c.i32()?;
    if !(0..=10_000_000).contains(&count) {
        return Err("zła liczba obiektów".into());
    }
    let mut objects = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if big_ids {
            c.u64()?;
        } else if version < 14 {
            c.u32()?;
        } else {
            c.align(4);
            c.u64()?;
        }
        let start = if version >= 22 {
            c.u64()?
        } else {
            c.u32()? as u64
        } + data_off;
        let size = c.u32()?;
        let type_id = c.i32()?;
        let class = if version < 16 {
            c.u16()? as i32
        } else {
            *classes.get(type_id as usize).ok_or("zły typ obiektu")?
        };
        if version < 11 {
            c.u16()?;
        }
        if (11..17).contains(&version) {
            c.u16()?;
        }
        if version == 15 || version == 16 {
            c.u8()?;
        }
        if start + size as u64 <= len {
            objects.push(Object { class, start, size });
        }
    }
    Ok(Serialized { unity, objects })
}

/// (główna, poboczna) z tekstu wersji Unity, np. "2019.4.41f1".
fn unity_version(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

// ------------------------------ obiekt Shader -------------------------------

/// Tablice opisujące blob (po jednym wpisie na platformę, każdy z segmentami).
struct BlobTables<'a> {
    platforms: Vec<u32>,
    /// (przesunięcie, długość skompresowana, długość rozpakowana) per segment.
    segments: Vec<Vec<(u32, u32, u32)>>,
    blob: &'a [u8],
}

fn read_tables(b: &[u8], at: usize, nested: bool) -> Option<BlobTables<'_>> {
    let mut c = Cur::new(b, true);
    c.pos = at;
    let n = c.u32().ok()? as usize;
    if !(1..=32).contains(&n) {
        return None;
    }
    let mut platforms = Vec::with_capacity(n);
    for _ in 0..n {
        let p = c.u32().ok()?;
        if p >= 64 {
            return None;
        }
        platforms.push(p);
    }
    let table = |c: &mut Cur| -> Option<Vec<Vec<u32>>> {
        if c.u32().ok()? as usize != n {
            return None;
        }
        (0..n)
            .map(|_| {
                let m = if nested { c.u32().ok()? as usize } else { 1 };
                if !(1..=256).contains(&m) {
                    return None;
                }
                (0..m).map(|_| c.u32().ok()).collect()
            })
            .collect()
    };
    let offsets = table(&mut c)?;
    let packed = table(&mut c)?;
    let sizes = table(&mut c)?;
    let blob_len = c.u32().ok()? as usize;
    let blob = c.take(blob_len).ok()?;
    let mut segments = Vec::with_capacity(n);
    let mut min_off = u32::MAX;
    for i in 0..n {
        if offsets[i].len() != packed[i].len() || offsets[i].len() != sizes[i].len() {
            return None;
        }
        let mut segs = Vec::new();
        for j in 0..offsets[i].len() {
            let (o, p, s) = (offsets[i][j], packed[i][j], sizes[i][j]);
            if (o as u64 + p as u64) > blob_len as u64 || s > (1 << 30) || (p > 0 && s == 0) {
                return None;
            }
            min_off = min_off.min(o);
            segs.push((o, p, s));
        }
        segments.push(segs);
    }
    (min_off == 0 && blob_len > 0).then_some(BlobTables {
        platforms,
        segments,
        blob,
    })
}

/// Rozpakowany kod platformy D3D11 z obiektu `Shader` (pusty, gdy brak).
fn shader_d3d11(obj: &[u8], nested_first: bool) -> Vec<u8> {
    for nested in [nested_first, !nested_first] {
        let mut at = 0;
        while at + 8 <= obj.len() {
            if let Some(t) = read_tables(obj, at, nested) {
                let mut out = Vec::new();
                let mut ok = true;
                for (i, &p) in t.platforms.iter().enumerate() {
                    if p != PLATFORM_D3D11 {
                        continue;
                    }
                    for &(o, n, size) in &t.segments[i] {
                        let packed = &t.blob[o as usize..(o + n) as usize];
                        match crate::lz4::decompress(packed, size as usize) {
                            Ok(d) => out.extend_from_slice(&d),
                            Err(_) => ok = false,
                        }
                    }
                }
                if ok {
                    return out;
                }
            }
            at += 4;
        }
    }
    Vec::new()
}

/// Wycina z bajtów kontenery DXBC z kodem SM4/SM5 (DXIL - SM6 - pomija).
pub fn carve_dxbc(bytes: &[u8], mut found: impl FnMut(Dxbc)) {
    let mut i = 0;
    while i + 32 <= bytes.len() {
        let Some(p) = bytes[i..].windows(4).position(|w| w == b"DXBC") else {
            return;
        };
        i += p;
        match container(&bytes[i..]) {
            Some((d, size)) => {
                if let Some(d) = d {
                    found(d);
                }
                i += size;
            }
            None => i += 4,
        }
    }
}

/// Sprawdza kontener zaczynający się od "DXBC"; zwraca (shader SM4/5, rozmiar).
fn container(b: &[u8]) -> Option<(Option<Dxbc<'_>>, usize)> {
    let total = le32(b, 24)? as usize;
    let chunks = le32(b, 28)? as usize;
    if le32(b, 20)? != 1 || total < 32 || total > b.len() || !(1..=64).contains(&chunks) {
        return None;
    }
    let code = &b[..total];
    let mut stage = None;
    for k in 0..chunks {
        let off = le32(code, 32 + 4 * k)? as usize;
        if off + 8 > total {
            return None;
        }
        match &code[off..off + 4] {
            b"SHEX" | b"SHDR" => {
                stage = Stage::from_program_type(le32(code, off + 8)? >> 16);
            }
            b"DXIL" => return Some((None, total)),
            _ => {}
        }
    }
    Some((stage.map(|stage| Dxbc { stage, code }), total))
}

// ------------------------------ przegląd gry --------------------------------

/// Pliki w folderze `*_Data` gry, które mogą zawierać shadery.
pub fn candidate_files(data_dir: &Path) -> Vec<PathBuf> {
    let skip_dirs = [
        "managed",
        "il2cpp_data",
        "plugins",
        "mono",
        "monobleedingedge",
    ];
    let skip_ext = [
        "ress", "resource", "dll", "json", "txt", "xml", "bank", "bmp", "png", "config", "info",
        "log", "bin", "hash", "manifest",
    ];
    let mut out = Vec::new();
    let mut stack = vec![data_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !skip_dirs.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if ft.is_file() {
                let ext = path
                    .extension()
                    .map(|x| x.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !skip_ext.contains(&ext.as_str()) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

/// Wynik przejrzenia jednego pliku.
#[derive(Default, Debug)]
pub struct FileStats {
    pub shaders: usize,
    pub compute: usize,
    pub containers: usize,
    /// Obiekty Shader bez kodu D3D11.
    pub empty: usize,
}

fn scan_serialized(
    src: &mut dyn Source,
    stats: &mut FileStats,
    found: &mut dyn FnMut(Dxbc),
) -> Result<(), String> {
    let file = parse_serialized(src)?;
    let nested = unity_version(&file.unity).is_none_or(|v| v >= (2019, 3));
    for o in &file.objects {
        if o.class != CLASS_SHADER && o.class != CLASS_COMPUTE_SHADER {
            continue;
        }
        let bytes = src.read_vec(o.start, o.size as usize)?;
        let before = stats.containers;
        let code = if o.class == CLASS_SHADER {
            stats.shaders += 1;
            shader_d3d11(&bytes, nested)
        } else {
            stats.compute += 1;
            bytes
        };
        carve_dxbc(&code, |d| {
            stats.containers += 1;
            found(d);
        });
        if o.class == CLASS_SHADER && stats.containers == before {
            stats.empty += 1;
        }
    }
    Ok(())
}

/// Przegląda plik (paczkę UnityFS albo plik serializowany) i podaje każdy
/// znaleziony kontener DXBC. Pliki innego rodzaju zwracają `Ok` bez wyników.
pub fn scan_file(path: &Path, found: &mut dyn FnMut(Dxbc)) -> Result<FileStats, String> {
    let mut stats = FileStats::default();
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut magic = [0u8; 8];
    if len < 8 || file.read_exact(&mut magic).is_err() {
        return Ok(stats);
    }
    if &magic == b"UnityFS\0" {
        drop(file);
        let mut bundle = Bundle::open(path)?;
        let nodes = bundle.nodes.clone();
        for (off, size, name) in nodes {
            let lower = name.to_lowercase();
            if lower.ends_with(".ress") || lower.ends_with(".resource") {
                continue;
            }
            let mut node = Node {
                bundle: &mut bundle,
                base: off,
                len: size,
            };
            // Węzły, które nie są plikami serializowanymi, pomijamy.
            let _ = scan_serialized(&mut node, &mut stats, found);
        }
        return Ok(stats);
    }
    let mut src = FileSource { file, len };
    match scan_serialized(&mut src, &mut stats, found) {
        Ok(()) => Ok(stats),
        // Plik innego rodzaju (np. dźwięk) - nie błąd.
        Err(_) if stats.shaders == 0 && stats.compute == 0 => Ok(stats),
        Err(e) => Err(e),
    }
}

/// Folder `<exe>_Data` gry Unity oraz nazwa pliku .exe gry.
pub fn game_layout(dir: &Path) -> Option<(PathBuf, String)> {
    let mut best: Option<(PathBuf, String)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix("_Data") {
            let exe = format!("{stem}.exe");
            if dir.join(&exe).is_file() {
                best = Some((e.path(), exe));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dxbc(program_type: u32) -> Vec<u8> {
        // Nagłówek 32 B + 1 przesunięcie + fragment SHEX z tokenem wersji.
        let mut b = Vec::new();
        b.extend_from_slice(b"DXBC");
        b.extend_from_slice(&[7; 16]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // rozmiar - niżej
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&36u32.to_le_bytes());
        b.extend_from_slice(b"SHEX");
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&((program_type << 16) | 0x50).to_le_bytes());
        let n = b.len() as u32;
        b[24..28].copy_from_slice(&n.to_le_bytes());
        b
    }

    #[test]
    fn carves_containers_and_stages() {
        let mut bytes = vec![0xAA; 13];
        bytes.extend(dxbc(1));
        bytes.extend_from_slice(b"DXBCxx");
        bytes.extend(dxbc(0));
        let mut stages = Vec::new();
        carve_dxbc(&bytes, |d| stages.push(d.stage));
        assert_eq!(stages, vec![Stage::Vertex, Stage::Pixel]);
    }

    #[test]
    fn finds_d3d11_blob_in_shader_object() {
        let code = dxbc(0);
        let packed = crate::lz4::compress_literals(&code);
        let mut obj = vec![0u8; 40]; // "SerializedShader"
        let w = |o: &mut Vec<u8>, v: u32| o.extend_from_slice(&v.to_le_bytes());
        // 2 platformy: Vulkan (18) bez danych i D3D11 (4).
        w(&mut obj, 2);
        w(&mut obj, 18);
        w(&mut obj, 4);
        for vals in [[0, 0], [0, packed.len() as u32], [0, code.len() as u32]] {
            w(&mut obj, 2);
            for v in vals {
                w(&mut obj, 1);
                w(&mut obj, v);
            }
        }
        w(&mut obj, packed.len() as u32);
        obj.extend_from_slice(&packed);
        obj.extend_from_slice(&[0, 0, 0, 0]);
        let out = shader_d3d11(&obj, true);
        assert_eq!(out, code);
    }

    /// Przegląd shaderów w zainstalowanej grze Unity:
    /// `TC_UNITY_GAME=<folder gry> cargo test unity_scan -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn unity_scan() {
        let dir = PathBuf::from(std::env::var("TC_UNITY_GAME").expect("TC_UNITY_GAME"));
        let (data, exe) = game_layout(&dir).expect("brak *_Data");
        let files = candidate_files(&data);
        println!("{exe}: {} plików", files.len());
        let started = std::time::Instant::now();
        let mut unique = std::collections::HashSet::new();
        let mut stages = [0usize; 6];
        let (mut shaders, mut compute, mut containers, mut errors, mut empty) = (0, 0, 0, 0, 0);
        for f in &files {
            match scan_file(f, &mut |d| {
                if unique.insert(d.digest()) {
                    stages[d.stage.index()] += 1;
                }
            }) {
                Ok(s) => {
                    shaders += s.shaders;
                    empty += s.empty;
                    compute += s.compute;
                    containers += s.containers;
                }
                Err(e) => {
                    errors += 1;
                    if errors < 5 {
                        println!("{}: {e}", f.display());
                    }
                }
            }
        }
        println!(
            "Shader {shaders}, ComputeShader {compute}, DXBC {containers}, unikalne {} \
             (PS/VS/GS/HS/DS/CS {stages:?}), błędy {errors}, Shader bez D3D11 {empty}, {:.1} s",
            unique.len(),
            started.elapsed().as_secs_f32()
        );
    }
}
