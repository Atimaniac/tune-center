//! Kontenery shaderów DirectX ("DXBC"): DXBC (SM4/SM5) i DXIL (SM6) -
//! części, etap programu, sygnatury wejścia/wyjścia i root signature
//! wbudowana w shader (część RTS0). Do tego wycinanie kontenerów z dowolnych
//! plików gry i dobieranie shaderów wierzchołków do shaderów pikseli tak, jak
//! łączy je potok D3D12 (po semantykach).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Etap programu (typ z nagłówka DXIL albo tokenu wersji DXBC).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Compute,
    /// Biblioteka DXIL (ray tracing), mesh, amplification i inne.
    Other,
}

impl Kind {
    fn from_type(t: u32) -> Kind {
        match t {
            0 => Kind::Pixel,
            1 => Kind::Vertex,
            2 => Kind::Geometry,
            3 => Kind::Hull,
            4 => Kind::Domain,
            5 => Kind::Compute,
            _ => Kind::Other,
        }
    }
}

/// Element sygnatury: semantyka, indeks, wartość systemowa, typ składowych,
/// rejestr i maska.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SigElement {
    /// Nazwa semantyki małymi literami.
    pub name: String,
    pub index: u32,
    pub sysval: u32,
    /// 1 = uint, 2 = sint, 3 = float (D3D_REGISTER_COMPONENT_TYPE).
    pub component: u32,
    pub register: u32,
    pub mask: u8,
}

/// Wartości systemowe (D3D_NAME) dla sygnatur.
pub const SV_POSITION: u32 = 1;
pub const SV_TARGET: u32 = 64;
const SV_DEPTH: [u32; 3] = [65, 66, 67];
/// Wejścia shadera pikseli, które tworzy rasteryzator, nie shader wierzchołków.
const PS_GENERATED: [u32; 4] = [7, 9, 10, 23]; // PrimitiveID, IsFrontFace, SampleIndex, Barycentrics

/// Przeanalizowany kontener (bajty pożyczone z pliku albo bufora).
#[derive(Clone, Debug)]
pub struct Container<'a> {
    pub bytes: &'a [u8],
    parts: Vec<([u8; 4], usize, usize)>,
}

impl<'a> Container<'a> {
    /// Kontener zaczynający się na początku `b` (może być dłuższe).
    pub fn parse(b: &'a [u8]) -> Option<Container<'a>> {
        if b.get(..4)? != b"DXBC" || le32(b, 20)? != 1 {
            return None;
        }
        let total = le32(b, 24)? as usize;
        let count = le32(b, 28)? as usize;
        if total < 32 + 8 || total > b.len() || !(1..=64).contains(&count) || 32 + 4 * count > total {
            return None;
        }
        let bytes = &b[..total];
        let mut parts = Vec::with_capacity(count);
        for k in 0..count {
            let at = le32(bytes, 32 + 4 * k)? as usize;
            let size = le32(bytes, at + 4)? as usize;
            if at < 32 + 4 * count || at + 8 + size > total {
                return None;
            }
            parts.push((bytes[at..at + 4].try_into().ok()?, at + 8, size));
        }
        Some(Container { bytes, parts })
    }

    pub fn part(&self, fourcc: &[u8; 4]) -> Option<&'a [u8]> {
        self.parts
            .iter()
            .find(|(f, ..)| f == fourcc)
            .map(|&(_, at, size)| &self.bytes[at..at + size])
    }

    /// Tożsamość kontenera: skrót z nagłówka, a gdy jest zerowy (kontener
    /// bez walidacji) - skrót całej treści.
    pub fn identity(&self) -> [u8; 16] {
        let digest: [u8; 16] = self.bytes[4..20].try_into().unwrap();
        if digest != [0; 16] {
            return digest;
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.bytes.hash(&mut h);
        let a = h.finish();
        self.bytes.len().hash(&mut h);
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&a.to_le_bytes());
        out[8..].copy_from_slice(&h.finish().to_le_bytes());
        out
    }

    pub fn is_dxil(&self) -> bool {
        self.part(b"DXIL").is_some()
    }

    /// Etap programu; `None` - kontener bez kodu (np. sama root signature).
    pub fn kind(&self) -> Option<Kind> {
        let program = self
            .part(b"DXIL")
            .or_else(|| self.part(b"SHEX"))
            .or_else(|| self.part(b"SHDR"))?;
        Some(Kind::from_type(le32(program, 0)? >> 16))
    }

    /// Root signature wbudowana w shader (zserializowana, jak z
    /// `D3D12SerializeRootSignature`).
    pub fn root_signature(&self) -> Option<&'a [u8]> {
        self.part(b"RTS0").filter(|p| p.len() >= 24)
    }

    pub fn inputs(&self) -> Vec<SigElement> {
        self.signature(&[b"ISG1", b"ISGN"])
    }

    pub fn outputs(&self) -> Vec<SigElement> {
        self.signature(&[b"OSG1", b"OSG5", b"OSGN"])
    }

    fn signature(&self, names: &[&[u8; 4]]) -> Vec<SigElement> {
        let Some((fourcc, data)) = names.iter().find_map(|n| Some((*n, self.part(n)?))) else {
            return Vec::new();
        };
        // (rozmiar elementu, przesunięcie pól od nazwy): ISGN/OSGN 24 B,
        // OSG5 28 B (najpierw strumień), ISG1/OSG1 32 B (strumień, ..., precyzja).
        let (stride, skip) = match fourcc {
            b"ISGN" | b"OSGN" => (24, 0),
            b"OSG5" => (28, 4),
            _ => (32, 4),
        };
        let Some(count) = le32(data, 0) else { return Vec::new() };
        let mut out = Vec::new();
        for e in 0..count.min(128) as usize {
            let at = 8 + e * stride + skip;
            let (Some(name), Some(index), Some(sysval), Some(component), Some(register), Some(&mask)) = (
                le32(data, at),
                le32(data, at + 4),
                le32(data, at + 8),
                le32(data, at + 12),
                le32(data, at + 16),
                data.get(at + 20),
            ) else {
                break;
            };
            let name = data
                .get(name as usize..)
                .and_then(|rest| rest.split(|&c| c == 0).next())
                .map(|n| String::from_utf8_lossy(n).to_lowercase())
                .unwrap_or_default();
            out.push(SigElement { name, index, sysval, component, register, mask });
        }
        out
    }
}

// --------------------------------- łączenie ----------------------------------

/// Wymagania shadera pikseli wobec shadera wierzchołków: elementy, które
/// musi dostać z poprzedniego etapu (nazwa, indeks, typ).
pub fn ps_needs(ps: &Container) -> Vec<(String, u32, u32)> {
    let mut v: Vec<(String, u32, u32)> = ps
        .inputs()
        .into_iter()
        .filter(|e| !PS_GENERATED.contains(&e.sysval) && e.sysval != SV_POSITION)
        .map(|e| (e.name, e.index, e.component))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Wyjścia shadera wierzchołków (nazwa, indeks, typ) i czy daje SV_Position.
pub fn vs_gives(vs: &Container) -> (Vec<(String, u32, u32)>, bool) {
    let outputs = vs.outputs();
    let position = outputs.iter().any(|e| e.sysval == SV_POSITION);
    let mut v: Vec<(String, u32, u32)> = outputs
        .into_iter()
        .filter(|e| e.sysval != SV_POSITION)
        .map(|e| (e.name, e.index, e.component))
        .collect();
    v.sort();
    v.dedup();
    (v, position)
}

/// Czy wyjścia `gives` pokrywają wymagania `needs` (obie listy posortowane).
pub fn links(needs: &[(String, u32, u32)], gives: &[(String, u32, u32)]) -> bool {
    needs.iter().all(|n| gives.binary_search(n).is_ok())
}

/// Formaty celów renderowania z wyjść shadera pikseli (SV_Target):
/// DXGI_FORMAT dla każdego gniazda do najwyższego użytego (0 = brak celu),
/// i czy shader zapisuje głębię.
pub fn ps_targets(ps: &Container) -> (Vec<u32>, bool) {
    let outputs = ps.outputs();
    let depth = outputs.iter().any(|e| SV_DEPTH.contains(&e.sysval));
    let mut targets = Vec::new();
    for e in outputs.iter().filter(|e| e.sysval == SV_TARGET || e.name == "sv_target") {
        let slot = e.register.min(7) as usize;
        if targets.len() <= slot {
            targets.resize(slot + 1, 0);
        }
        targets[slot] = match e.component {
            1 => 3,  // R32G32B32A32_UINT
            2 => 4,  // R32G32B32A32_SINT
            _ => 10, // R16G16B16A16_FLOAT
        };
    }
    // Puste gniazdo między celami: format musi być podany.
    for t in targets.iter_mut().filter(|t| **t == 0) {
        *t = 10;
    }
    (targets, depth)
}

/// Układ wejścia shadera wierzchołków z jego sygnatury: jeden bufor,
/// elementy kolejno, formaty 32-bitowe o liczbie składowych z maski.
pub fn vs_layout(vs: &Container) -> Vec<super::warm::InputElement> {
    let mut elements: Vec<SigElement> = vs
        .inputs()
        .into_iter()
        .filter(|e| e.sysval == 0 && e.mask != 0 && (1..=3).contains(&e.component))
        .collect();
    elements.sort_by_key(|e| e.register);
    let mut offset = 0;
    elements
        .into_iter()
        .map(|e| {
            let count = (8 - (e.mask & 0xF).leading_zeros()).clamp(1, 4);
            // DXGI: R32G32B32A32 (2 float, 3 uint, 4 sint), R32G32B32 (6..8),
            // R32G32 (16..18), R32 (41 float, 42 uint, 43 sint).
            let format = match (count, e.component) {
                (4, 1) => 3,
                (4, 2) => 4,
                (4, _) => 2,
                (3, 1) => 7,
                (3, 2) => 8,
                (3, _) => 6,
                (2, 1) => 17,
                (2, 2) => 18,
                (2, _) => 16,
                (_, 1) => 42,
                (_, 2) => 43,
                _ => 41,
            };
            let element = super::warm::InputElement {
                semantic: e.name.to_uppercase(),
                index: e.index,
                format,
                slot: 0,
                offset,
                instance_rate: 0,
            };
            offset += 4 * count;
            element
        })
        .collect()
}

// --------------------------------- wycinanie ---------------------------------

/// Największy kontener brany pod uwagę.
const MAX_CONTAINER: usize = 16 << 20;
const CHUNK: usize = 32 << 20;

/// Wycina kontenery z bajtów, wołając `found(przesunięcie, kontener)`.
pub fn carve(bytes: &[u8], base: u64, found: &mut dyn FnMut(u64, &Container)) {
    let mut i = 0;
    while i + 40 <= bytes.len() {
        let Some(p) = bytes[i..].windows(4).position(|w| w == b"DXBC") else {
            return;
        };
        i += p;
        match Container::parse(&bytes[i..]) {
            Some(c) if c.kind().is_some() || c.root_signature().is_some() => {
                let size = c.bytes.len();
                found(base + i as u64, &c);
                i += size;
            }
            _ => i += 4,
        }
    }
}

/// Wycina kontenery z zakresu pliku: kawałkami z zakładką (kontener
/// zaczynający się w zakładce znajdzie następny kawałek), a kontener, który
/// zaczyna się wcześniej i przecina koniec kawałka, czyta osobno w całości.
pub fn carve_file(
    file: &std::fs::File,
    start: u64,
    len: u64,
    found: &mut dyn FnMut(u64, &Container),
) -> std::io::Result<()> {
    carve_chunked(file, start, len, CHUNK, found)
}

fn carve_chunked(
    file: &std::fs::File,
    start: u64,
    len: u64,
    chunk: usize,
    found: &mut dyn FnMut(u64, &Container),
) -> std::io::Result<()> {
    let overlap = (64 * 1024).min(chunk as u64 / 4);
    let end = start + len;
    let mut at = start;
    // Koniec ostatniego zgłoszonego kontenera - nic przed nim nie jest zgłaszane drugi raz.
    let mut reported = start;
    let mut buf = Vec::new();
    while at < end {
        let n = ((end - at) as usize).min(chunk);
        buf.resize(n, 0);
        read_at(file, at, &mut buf)?;
        let chunk_end = at + n as u64;
        let last = chunk_end >= end;
        let keep_from = if last { u64::MAX } else { chunk_end - overlap };
        carve(&buf, at, &mut |off, c| {
            if off >= reported && off < keep_from {
                reported = off + c.bytes.len() as u64;
                found(off, c);
            }
        });
        if last {
            break;
        }
        // Kontenery sprzed zakładki, które nie mieszczą się w kawałku.
        let mut p = (reported.max(at) - at) as usize;
        let stop = (keep_from - at) as usize;
        while p < stop {
            let Some(k) = buf[p..(stop + 3).min(n)].windows(4).position(|w| w == b"DXBC") else { break };
            p += k;
            let off = at + p as u64;
            let total = le32(&buf, p + 24).unwrap_or(0) as u64;
            if off >= reported && (40..=MAX_CONTAINER as u64).contains(&total) && off + total > chunk_end && off + total <= end {
                let mut whole = vec![0u8; total as usize];
                if read_at(file, off, &mut whole).is_ok() {
                    if let Some(c) = Container::parse(&whole).filter(|c| c.kind().is_some()) {
                        reported = off + total;
                        found(off, &c);
                    }
                }
            }
            p += 4;
        }
        at = keep_from.max(reported);
    }
    Ok(())
}

pub fn read_at(file: &std::fs::File, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], at + done as u64)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = file.try_clone()?;
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(buf)
    }
}

// ----------------------------- pliki gry -------------------------------------

/// Rozszerzenia plików, w których shaderów nie ma (media, teksty, programy).
const SKIP_EXT: [&str; 44] = [
    "bk2", "usm", "webm", "mp4", "avi", "wmv", "mkv", "mov", "wem", "bnk", "pck", "ogg", "wav",
    "mp3", "flac", "opus", "fsb", "xwb", "w3speech", "dds", "png", "jpg", "jpeg", "tga", "bmp",
    "ktx", "ktx2", "txt", "json", "xml", "ini", "cfg", "log", "html", "htm", "js", "css", "md",
    "pdf", "ttf", "otf", "msi", "strings", "w3strings",
];
const SKIP_DIRS: [&str; 10] = [
    "_commonredist",
    "redist",
    "directx",
    "easyanticheat",
    "battleye",
    "__installer",
    "support",
    "movies",
    "videos",
    "soundtrack",
];

fn lower_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Plik, którego nazwa wskazuje na shadery albo potoki - czytany w całości.
pub fn named(p: &Path) -> bool {
    let n = lower_name(p);
    ["shader", "pso", "pipeline", "dxil", "effect", "material", "cso"]
        .iter()
        .any(|w| n.contains(w))
}

/// Pliki gry, które mogą zawierać kontenery (do 8 poziomów), najpierw
/// nazwane jak shadery, potem największe.
pub fn candidate_files(game: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![(game.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = e.metadata() else { continue };
            let path = e.path();
            if meta.is_dir() {
                if depth < 8 && !SKIP_DIRS.contains(&lower_name(&path).as_str()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let ext = path
                .extension()
                .map(|x| x.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if meta.len() >= 64 && !SKIP_EXT.contains(&ext.as_str()) {
                out.push((path, meta.len()));
            }
        }
    }
    out.sort_by(|a, b| named(&b.0).cmp(&named(&a.0)).then(b.1.cmp(&a.1)));
    out
}

/// Zakresy pliku do próbkowania: początek, środek i koniec po `window` bajtów.
pub fn samples(len: u64, window: u64) -> Vec<(u64, u64)> {
    if len <= 3 * window {
        return vec![(0, len)];
    }
    vec![(0, window), (len / 2 - window / 2, window), (len - window, window)]
}

/// Statystyka kontenerów DirectX 12 z wbudowaną root signature.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Census {
    pub graphics: usize,
    pub compute: usize,
    /// Kontenery DXIL bez root signature (do nich potrzebny jest zapis gry).
    pub without_rs: usize,
    /// Pliki z kontenerami z root signature.
    pub files: Vec<(PathBuf, u64)>,
}

/// Szybkie sprawdzenie gry (przy skanowaniu): pliki nazwane jak shadery w
/// całości, pozostałe próbkami, łącznie do `budget` bajtów.
pub fn census(game: &Path, budget: u64) -> Census {
    let mut out = Census::default();
    let mut read = 0u64;
    let mut seen = std::collections::HashSet::new();
    for (path, len) in candidate_files(game) {
        if read >= budget {
            break;
        }
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let ranges = if named(&path) {
            vec![(0, len.min(budget - read))]
        } else {
            samples(len, 1 << 20)
        };
        let mut hit = false;
        for (start, n) in ranges {
            read += n;
            let _ = carve_file(&file, start, n, &mut |_, c| {
                if !seen.insert(c.identity()) {
                    return;
                }
                match (c.kind(), c.root_signature().is_some(), c.is_dxil()) {
                    (Some(Kind::Compute), true, _) => {
                        out.compute += 1;
                        hit = true;
                    }
                    (Some(Kind::Vertex | Kind::Pixel), true, _) => {
                        out.graphics += 1;
                        hit = true;
                    }
                    (Some(_), false, true) => out.without_rs += 1,
                    _ => {}
                }
            });
        }
        if hit {
            out.files.push((path, len));
        }
    }
    out
}

// ------------------------------ plan potoków --------------------------------

/// Shader w pliku gry.
#[derive(Clone, Debug)]
pub struct Located {
    pub file: usize,
    pub at: u64,
    pub size: u32,
}

/// Potok do utworzenia z wyciętych shaderów.
#[derive(Clone, Debug)]
pub enum Pipeline {
    Compute { cs: usize },
    /// Shader wierzchołków (i pikseli) z root signature jednego z nich.
    Graphics { vs: usize, ps: Option<usize>, alternatives: Vec<usize> },
}

/// Shadery jednej gry: położenie, etap, root signature (indeks w `signatures`).
#[derive(Default)]
pub struct Index {
    pub shaders: Vec<Located>,
    pub kinds: Vec<Kind>,
    pub rs: Vec<usize>,
    pub signatures: Vec<Vec<u8>>,
    /// Pliki, w których znalezione shadery leżą (do łączenia w puli pliku).
    pub file_of: Vec<usize>,
    pub needs: Vec<Vec<(String, u32, u32)>>,
    pub gives: Vec<(Vec<(String, u32, u32)>, bool)>,
    pub without_rs: usize,
    pub other_stages: usize,
}

impl Index {
    /// Dodaje kontener z root signature; bez niej tylko liczy.
    pub fn add(&mut self, file: usize, at: u64, c: &Container, rs_ids: &mut HashMap<Vec<u8>, usize>) {
        let Some(kind) = c.kind() else { return };
        let Some(rs) = c.root_signature() else {
            if c.is_dxil() {
                self.without_rs += 1;
            }
            return;
        };
        if !matches!(kind, Kind::Vertex | Kind::Pixel | Kind::Compute) {
            self.other_stages += 1;
            return;
        }
        let id = *rs_ids.entry(rs.to_vec()).or_insert_with(|| {
            self.signatures.push(rs.to_vec());
            self.signatures.len() - 1
        });
        self.shaders.push(Located { file, at, size: c.bytes.len() as u32 });
        self.kinds.push(kind);
        self.rs.push(id);
        self.file_of.push(file);
        self.needs.push(if kind == Kind::Pixel { ps_needs(c) } else { Vec::new() });
        self.gives.push(if kind == Kind::Vertex { vs_gives(c) } else { (Vec::new(), false) });
    }

    /// Potoki: każdy compute osobno; każdy shader pikseli z pasującym
    /// shaderem wierzchołków z tego samego pliku i z tą samą root signature
    /// (do 4 kandydatów na wypadek odrzucenia); shadery wierzchołków bez pary
    /// - same (potok tylko z głębią).
    pub fn plan(&self) -> Vec<Pipeline> {
        let mut out = Vec::new();
        // Shadery wierzchołków pogrupowane: (plik, root signature) -> zestawy wyjść.
        let mut groups: HashMap<(usize, usize), Vec<(usize, &Vec<(String, u32, u32)>)>> = HashMap::new();
        for i in 0..self.shaders.len() {
            match self.kinds[i] {
                Kind::Compute => out.push(Pipeline::Compute { cs: i }),
                Kind::Vertex if self.gives[i].1 => {
                    groups.entry((self.file_of[i], self.rs[i])).or_default().push((i, &self.gives[i].0));
                }
                _ => {}
            }
        }
        let mut paired = vec![false; self.shaders.len()];
        for i in 0..self.shaders.len() {
            if self.kinds[i] != Kind::Pixel {
                continue;
            }
            let Some(list) = groups.get(&(self.file_of[i], self.rs[i])) else { continue };
            // Najpierw shader nieużyty jeszcze w parze, potem dowolny.
            let mut fits: Vec<usize> = list
                .iter()
                .filter(|(_, gives)| links(&self.needs[i], gives))
                .map(|(v, _)| *v)
                .collect();
            fits.sort_by_key(|v| paired[*v]);
            fits.truncate(4);
            if let Some((&vs, rest)) = fits.split_first() {
                paired[vs] = true;
                out.push(Pipeline::Graphics { vs, ps: Some(i), alternatives: rest.to_vec() });
            }
        }
        for i in 0..self.shaders.len() {
            if self.kinds[i] == Kind::Vertex && !paired[i] {
                out.push(Pipeline::Graphics { vs: i, ps: None, alternatives: Vec::new() });
            }
        }
        out
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Część kontenera: fourcc + dane.
    pub fn build(parts: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let head = 32 + 4 * parts.len();
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        for (fourcc, data) in parts {
            offsets.push((head + body.len()) as u32);
            body.extend_from_slice(*fourcc);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
        }
        let mut b = b"DXBC".to_vec();
        b.extend_from_slice(&[0u8; 16]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&((head + body.len()) as u32).to_le_bytes());
        b.extend_from_slice(&(parts.len() as u32).to_le_bytes());
        for o in offsets {
            b.extend_from_slice(&o.to_le_bytes());
        }
        b.extend_from_slice(&body);
        b
    }

    /// Sygnatura ISG1/OSG1: (nazwa, indeks, wartość systemowa, typ, rejestr, maska).
    pub fn sig1(elements: &[(&str, u32, u32, u32, u32, u8)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(elements.len() as u32).to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        let names_at = 8 + 32 * elements.len();
        let mut names = Vec::new();
        for (name, index, sysval, comp, reg, mask) in elements {
            b.extend_from_slice(&0u32.to_le_bytes()); // strumień
            b.extend_from_slice(&((names_at + names.len()) as u32).to_le_bytes());
            names.extend_from_slice(name.as_bytes());
            names.push(0);
            for v in [*index, *sysval, *comp, *reg] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&[*mask, 0, 0, 0]);
            b.extend_from_slice(&0u32.to_le_bytes()); // precyzja
        }
        b.extend_from_slice(&names);
        b
    }

    /// Nagłówek programu DXIL danego typu (reszta kodu nie jest czytana).
    pub fn dxil(kind: u32) -> Vec<u8> {
        let mut b = ((kind << 16) | 0x60).to_le_bytes().to_vec();
        b.extend_from_slice(&[0u8; 20]);
        b
    }

    #[test]
    fn parses_parts_signatures_and_links() {
        let rs = vec![2u8, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0];
        let vs = build(&[
            (b"ISG1", sig1(&[("POSITION", 0, 0, 3, 0, 0x7), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"OSG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"DXIL", dxil(1)),
            (b"RTS0", rs.clone()),
        ]);
        let ps = build(&[
            (b"ISG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3), ("SV_IsFrontFace", 0, 9, 1, 2, 1)])),
            (b"OSG1", sig1(&[("SV_Target", 0, 64, 3, 0, 0xF), ("SV_Target", 2, 64, 1, 2, 0xF)])),
            (b"DXIL", dxil(0)),
            (b"RTS0", rs.clone()),
        ]);
        let (v, p) = (Container::parse(&vs).unwrap(), Container::parse(&ps).unwrap());
        assert_eq!((v.kind(), p.kind()), (Some(Kind::Vertex), Some(Kind::Pixel)));
        assert!(v.is_dxil() && v.root_signature() == Some(&rs[..]));
        let needs = ps_needs(&p);
        assert_eq!(needs, vec![("texcoord".to_string(), 0, 3)]);
        let (gives, position) = vs_gives(&v);
        assert!(position && links(&needs, &gives));
        assert!(!links(&[("color".into(), 0, 3)], &gives));
        assert_eq!(ps_targets(&p), (vec![10, 10, 3], false));
        let layout = vs_layout(&v);
        assert_eq!(layout.len(), 2);
        assert_eq!((layout[0].semantic.as_str(), layout[0].format, layout[0].offset), ("POSITION", 6, 0));
        assert_eq!((layout[1].format, layout[1].offset), (16, 12));

        // Wycinanie z bajtów z szumem i plan: para VS+PS i compute.
        let cs = build(&[(b"DXIL", dxil(5)), (b"RTS0", rs.clone())]);
        let mut blob = vec![7u8; 100];
        for c in [&vs, &ps, &cs] {
            blob.extend_from_slice(c);
            blob.extend_from_slice(b"DXBC junk");
        }
        let mut index = Index::default();
        let mut ids = HashMap::new();
        carve(&blob, 0, &mut |at, c| index.add(0, at, c, &mut ids));
        assert_eq!(index.shaders.len(), 3);
        assert_eq!(index.signatures.len(), 1);
        assert_eq!(index.shaders[0].at, 100);
        let plan = index.plan();
        assert!(plan.iter().any(|p| matches!(p, Pipeline::Compute { cs: 2 })));
        assert!(plan.iter().any(|p| matches!(p, Pipeline::Graphics { vs: 0, ps: Some(1), .. })));
        assert_eq!(plan.len(), 2);
    }

    #[test]
    fn carves_files_across_chunk_edges() {
        let rs = vec![2u8; 24];
        let one = build(&[(b"DXIL", dxil(5)), (b"RTS0", rs.clone())]);
        let mut blob = Vec::new();
        let mut expected = Vec::new();
        // Kontenery w różnych miejscach względem granic kawałków po 256 B.
        for (k, gap) in [3usize, 120, 1, 77, 200, 9, 31, 250].iter().enumerate() {
            blob.extend(std::iter::repeat_n(k as u8, *gap));
            expected.push(blob.len() as u64);
            let mut c = one.clone();
            c[4] = k as u8; // inny skrót - inny kontener
            blob.extend_from_slice(&c);
        }
        blob.extend_from_slice(&[0u8; 40]);
        let path = std::env::temp_dir().join(format!("tc-carve-{}.bin", std::process::id()));
        std::fs::write(&path, &blob).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        for chunk in [256usize, 300, 1 << 20] {
            let mut got = Vec::new();
            carve_chunked(&file, 0, blob.len() as u64, chunk, &mut |at, c| {
                assert_eq!(c.kind(), Some(Kind::Compute));
                got.push(at);
            })
            .unwrap();
            assert_eq!(got, expected, "chunk {chunk}");
        }
        drop(file);
        let _ = std::fs::remove_file(path);
    }
}
