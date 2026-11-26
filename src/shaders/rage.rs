//! RAGE gen9 (GTA V Enhanced, DirectX 12): biblioteki shaderów "SGD2" leżą
//! bez kompresji i bez szyfrowania w archiwach RPF folderu `update`. Spis
//! treści archiwum jest zaszyfrowany (klucze ma tylko .exe gry), więc
//! biblioteki są szukane po sygnaturze na granicach bloków 512 B (RPF7 wyrównuje
//! tak dane każdego wpisu). Tylko odczyt plików gry; klucze i .exe nie są czytane.
//!
//! Biblioteka: "SGD2", potem dla każdego gniazda etapu (VS, PS, GS, DS, HS, CS)
//! u32 liczba i shadery: u8 długość nazwy (z NUL), nazwa, u8, u32 rozmiar,
//! kontener DXBC (DXIL w środku), u64 skrót, 144 B, u32 n i n bajtów. Dalej
//! 8 bajtów zerowych, u32 liczba efektów i efekty: u8 długość nazwy (z NUL),
//! nazwa, u32, dla każdego gniazda u32 n i n indeksów u32 shaderów biblioteki,
//! treść do "proplst\0", u32 liczba właściwości (każda: u32 rozmiar i bajty)
//! i tablica tekstów (u32 rozmiar i bajty). W treści jest tablica przejść:
//! u32 liczba technik, dla każdej u32 (najstarszy bajt - liczba przejść), u32
//! suma przejść i po 24 B na przejście - pierwsze sześć to gniazda etapów
//! (indeks od 1 w listę efektu, 0 = brak). Rozmiar efektów nie jest zapisany:
//! czytane są coraz dłuższe kawałki, aż się przeczytają.
//!
//! Każdy shader niesie root signature (część RTS0), a każde przejście to
//! dokładny zestaw etapów potoku. Biblioteki wewnątrz zaszyfrowanych wpisów
//! nie są widoczne.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{read_at, Container, Kind, SigElement};
use super::readers::{Bytes, Detected, Pipeline12, Set12};

const MAGIC: &[u8; 4] = b"SGD2";
const UPDATE_DIR: &str = "update";
const EXE: &str = "GTA5_Enhanced.exe";
/// Wyrównanie danych wpisów RPF7.
const BLOCK: u64 = 512;
/// Bajtów przeszukiwanych przy rozpoznaniu (najmniejsze archiwa najpierw;
/// w GTA V Enhanced pierwsza biblioteka leży 3,5 MB od początku update2.rpf).
const DETECT_BUDGET: u64 = 16 << 20;
/// Granice odrzucające fałszywe trafienia (GTA V Enhanced: 7594 PS w jednej
/// bibliotece, największy shader 0,5 MB, 5,3 MB efektów).
const MAX_SHADERS: u32 = 1 << 16;
const MAX_SHADER_SIZE: u32 = 64 << 20;
const MAX_EFFECTS: u32 = 1 << 20;
const MAX_EFFECTS_BYTES: u64 = 256 << 20;
const FIRST_EFFECTS_READ: u64 = 16 << 20;
const MAX_PIPELINES: usize = 1 << 20;
/// Kawałki przeszukiwania i okno kursora.
const SCAN_CHUNK: usize = 8 << 20;
const DETECT_CHUNK: usize = 1 << 20;
const WINDOW: usize = 1 << 20;

/// Etap każdego gniazda biblioteki i jego miejsce w `Pipeline12::stages`
/// (tam kolejność VS, PS, GS, HS, DS, CS).
const SLOT_KIND: [Kind; 6] = [Kind::Vertex, Kind::Pixel, Kind::Geometry, Kind::Domain, Kind::Hull, Kind::Compute];
const SLOT_STAGE: [usize; 6] = [0, 1, 2, 4, 3, 5];

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

// ------------------------------- odczyt danych -------------------------------

/// Źródło bajtów: plik archiwum albo bufor (testy).
trait Source {
    fn size(&self) -> u64;
    fn read(&self, at: u64, buf: &mut [u8]) -> bool;
}

struct Disk {
    file: Arc<File>,
    len: u64,
}

impl Disk {
    fn open(path: &Path) -> Option<Disk> {
        let file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        Some(Disk { file: Arc::new(file), len })
    }
}

impl Source for Disk {
    fn size(&self) -> u64 {
        self.len
    }
    fn read(&self, at: u64, buf: &mut [u8]) -> bool {
        at.checked_add(buf.len() as u64).is_some_and(|e| e <= self.len) && read_at(&self.file, at, buf).is_ok()
    }
}

impl Source for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }
    fn read(&self, at: u64, buf: &mut [u8]) -> bool {
        let Some(src) = usize::try_from(at).ok().and_then(|a| self.get(a..a.checked_add(buf.len())?)) else {
            return false;
        };
        buf.copy_from_slice(src);
        true
    }
}

/// Odczyt kolejny przez okno 1 MB (małe pola bez osobnego odczytu z dysku).
struct Cursor<'a> {
    src: &'a dyn Source,
    pos: u64,
    win: Vec<u8>,
    win_at: u64,
    win_len: usize,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a dyn Source, pos: u64) -> Self {
        Cursor { src, pos, win: Vec::new(), win_at: 0, win_len: 0 }
    }
    fn read(&mut self, out: &mut [u8]) -> Option<()> {
        let mut done = 0;
        while done < out.len() {
            if self.pos < self.win_at || self.pos >= self.win_at + self.win_len as u64 {
                let n = (self.src.size().checked_sub(self.pos)?).min(WINDOW as u64) as usize;
                if n == 0 {
                    return None;
                }
                if self.win.len() < n {
                    self.win.resize(n, 0);
                }
                if !self.src.read(self.pos, &mut self.win[..n]) {
                    return None;
                }
                self.win_at = self.pos;
                self.win_len = n;
            }
            let from = (self.pos - self.win_at) as usize;
            let n = (out.len() - done).min(self.win_len - from);
            out[done..done + n].copy_from_slice(&self.win[from..from + n]);
            done += n;
            self.pos += n as u64;
        }
        Some(())
    }
    fn u8(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        self.read(&mut b)?;
        Some(b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(&mut b)?;
        Some(u32::from_le_bytes(b))
    }
    fn u64(&mut self) -> Option<u64> {
        let mut b = [0u8; 8];
        self.read(&mut b)?;
        Some(u64::from_le_bytes(b))
    }
    /// Przesuwa o `n` bajtów, nie dalej niż koniec źródła.
    fn skip(&mut self, n: u64) -> Option<()> {
        let to = self.pos.checked_add(n)?;
        (to <= self.src.size()).then(|| self.pos = to)
    }
}

// -------------------------------- biblioteka ---------------------------------

/// Efekt: indeksy shaderów biblioteki w każdym gnieździe i przejścia (dla
/// każdego gniazda indeks od 1 w `stages`, 0 = brak); `None` - tablicy
/// przejść nie znaleziono.
#[derive(Debug)]
struct Effect {
    stages: [Vec<u32>; 6],
    passes: Option<Vec<[u8; 6]>>,
}

/// Biblioteka: shadery każdego gniazda (wynik `add` - `None`, gdy kontener
/// jest wadliwy), efekty i koniec biblioteki w pliku.
struct Library {
    shaders: [Vec<Option<usize>>; 6],
    effects: Vec<Effect>,
    end: u64,
}

/// Biblioteka pod `at` albo `None`, gdy bajty tam nią nie są. `add` dostaje
/// położenie i bajty każdego kontenera, który się przeczytał.
fn read_library(src: &dyn Source, at: u64, add: &mut dyn FnMut(u64, &Container) -> Option<usize>) -> Option<Library> {
    let mut cur = Cursor::new(src, at);
    let mut magic = [0u8; 4];
    cur.read(&mut magic)?;
    if &magic != MAGIC {
        return None;
    }
    let mut shaders: [Vec<Option<usize>>; 6] = Default::default();
    for slot in shaders.iter_mut() {
        let count = cur.u32()?;
        if count > MAX_SHADERS {
            return None;
        }
        for _ in 0..count {
            let name = cur.u8()?;
            cur.skip(name as u64 + 1)?;
            let size = cur.u32()?;
            if !(32..=MAX_SHADER_SIZE).contains(&size) || cur.pos + size as u64 > src.size() {
                return None;
            }
            let off = cur.pos;
            let mut c = vec![0u8; size as usize];
            cur.read(&mut c)?;
            let id = Container::parse(&c).filter(|x| x.bytes.len() == c.len() && x.kind().is_some()).and_then(|x| add(off, &x));
            slot.push(id);
            cur.skip(8 + 144)?;
            let block = cur.u32()?;
            cur.skip(block as u64)?;
        }
    }
    if cur.u64()? != 0 || shaders.iter().all(Vec::is_empty) {
        return None;
    }
    let n = cur.u32()?;
    if n > MAX_EFFECTS {
        return None;
    }
    let start = cur.pos;
    let counts = shaders.each_ref().map(Vec::len);
    let mut want = FIRST_EFFECTS_READ;
    loop {
        let len = (src.size() - start).min(want).min(MAX_EFFECTS_BYTES);
        let mut tail = vec![0u8; len as usize];
        if !src.read(start, &mut tail) {
            return None;
        }
        if let Some((effects, used)) = parse_effects(&tail, n, &counts) {
            return Some(Library { shaders, effects, end: start + used as u64 });
        }
        if len < want || want >= MAX_EFFECTS_BYTES {
            return None;
        }
        want *= 4;
    }
}

/// `n` efektów od początku `b`: lista i liczba zużytych bajtów.
fn parse_effects(b: &[u8], n: u32, counts: &[usize; 6]) -> Option<(Vec<Effect>, usize)> {
    let mut list = Vec::new();
    let mut p = 0;
    for _ in 0..n {
        let e = parse_effect(b, &mut p)?;
        if e.stages.iter().zip(counts).any(|(ids, &c)| ids.iter().any(|&i| i as usize >= c)) {
            return None;
        }
        list.push(e);
    }
    Some((list, p))
}

fn parse_effect(b: &[u8], p: &mut usize) -> Option<Effect> {
    let len = *b.get(*p)? as usize;
    if len < 2 || *b.get(*p + len)? != 0 {
        return None;
    }
    *p += 1 + len;
    le32(b, *p)?;
    *p += 4;
    let mut stages: [Vec<u32>; 6] = Default::default();
    for slot in stages.iter_mut() {
        let n = le32(b, *p)?;
        if n > MAX_SHADERS || (n as usize) * 4 > b.len() - *p - 4 {
            return None;
        }
        *slot = (0..n as usize).map(|i| le32(b, *p + 4 + 4 * i).unwrap_or(0)).collect();
        *p += 4 + 4 * n as usize;
    }
    let marker = b[*p..].windows(8).position(|w| w == b"proplst\0")?;
    let passes = pass_table(&b[*p..*p + marker], &stages);
    *p += marker + 8;
    let props = le32(b, *p)?;
    *p += 4;
    // Właściwości i na końcu tablica tekstów: u32 rozmiar i bajty.
    for _ in 0..=props {
        let size = le32(b, *p)? as usize;
        if size > b.len() - *p - 4 {
            return None;
        }
        *p += 4 + size;
    }
    Some(Effect { stages, passes })
}

/// Tablica przejść w treści efektu: jedyne miejsce wyrównane do 4 B, gdzie
/// liczby się zgadzają i każdy indeks gniazda mieści się w liście efektu;
/// `None`, gdy nie ma żadnego albo jest więcej niż jedno.
fn pass_table(body: &[u8], stages: &[Vec<u32>; 6]) -> Option<Vec<[u8; 6]>> {
    let mut found = None;
    let mut p = 8;
    while p + 8 <= body.len() {
        let at = p;
        p += 4;
        let t = le32(body, at).unwrap_or(0) as usize;
        if !(1..=512).contains(&t) || at + 8 + 4 * t > body.len() {
            continue;
        }
        let counts: Vec<u8> = (0..t).map(|i| body[at + 4 + 4 * i + 3]).collect();
        if counts.contains(&0) {
            continue;
        }
        let sum: usize = counts.iter().map(|&c| c as usize).sum();
        let q = at + 4 + 4 * t;
        if le32(body, q) != Some(sum as u32) || q + 4 + 24 * sum > body.len() {
            continue;
        }
        let passes: Vec<[u8; 6]> = (0..sum)
            .map(|i| body[q + 4 + 24 * i..q + 10 + 24 * i].try_into().unwrap_or([0; 6]))
            .collect();
        let ok = passes
            .iter()
            .all(|r| (r[0] != 0 || r[5] != 0) && r.iter().zip(stages).all(|(&x, s)| x as usize <= s.len()));
        if !ok {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(passes);
    }
    found
}

/// Czy przejście to potok D3D12: VS (z HS i DS albo bez obu) albo sam CS.
fn is_pipeline(pass: &[u8; 6]) -> bool {
    if pass[5] != 0 {
        pass[..5].iter().all(|&x| x == 0)
    } else {
        pass[0] != 0 && (pass[3] == 0) == (pass[4] == 0)
    }
}

/// Przeszukuje źródło na granicach bloków: `library(at)` dla każdego bloku
/// zaczynającego się od "SGD2" zwraca koniec biblioteki albo `None`.
/// Kończy po `budget` bajtach; `true`, gdy była choć jedna biblioteka.
fn scan(src: &dyn Source, budget: u64, chunk: usize, read: &mut u64, library: &mut dyn FnMut(u64) -> Option<u64>) -> bool {
    let mut buf = vec![0u8; chunk.min(src.size() as usize).max(4)];
    let (mut at, mut scanned, mut any) = (0u64, 0u64, false);
    while at < src.size() && scanned < budget {
        let n = ((src.size() - at) as usize).min(buf.len());
        if !src.read(at, &mut buf[..n]) {
            break;
        }
        scanned += n as u64;
        *read += n as u64;
        let mut next = at + (n as u64 - n as u64 % BLOCK);
        let mut o = 0;
        while o + 4 <= n {
            if &buf[o..o + 4] == MAGIC {
                if let Some(end) = library(at + o as u64) {
                    any = true;
                    next = end.div_ceil(BLOCK) * BLOCK;
                    break;
                }
            }
            o += BLOCK as usize;
        }
        at = if next <= at { at + BLOCK } else { next };
    }
    any
}

// --------------------------------- pliki gry ---------------------------------

/// Wpis katalogu o nazwie `name` bez względu na wielkość liter.
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// Archiwa folderu `update` (bez podfolderów - DLC nie mają bibliotek).
fn archives(game: &Path) -> Vec<(PathBuf, u64)> {
    let Some(dir) = find(game, UPDATE_DIR).filter(|d| d.is_dir()) else { return Vec::new() };
    let mut out: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("rpf")))
        .filter_map(|e| Some((e.path(), e.metadata().ok().filter(|m| m.is_file())?.len())))
        .collect();
    out.sort_by_key(|(p, _)| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    out
}

/// Tani test biblioteki: liczby gniazd do pierwszego shadera i ten shader -
/// cały kontener DXIL etapu swojego gniazda.
fn looks_like_library(src: &dyn Source, at: u64) -> bool {
    (|| {
        let mut cur = Cursor::new(src, at + 4);
        for kind in SLOT_KIND {
            let count = cur.u32()?;
            if count > MAX_SHADERS {
                return None;
            }
            if count == 0 {
                continue;
            }
            let name = cur.u8()?;
            cur.skip(name as u64 + 1)?;
            let size = cur.u32()?;
            if !(32..=4 << 20).contains(&size) {
                return None;
            }
            let mut c = vec![0u8; size as usize];
            cur.read(&mut c)?;
            let x = Container::parse(&c)?;
            return Some(x.bytes.len() == c.len() && x.is_dxil() && x.kind() == Some(kind));
        }
        Some(false)
    })()
    .unwrap_or(false)
}

/// GTA V Enhanced: biblioteka SGD2 w archiwach `update` (najmniejsze
/// najpierw, łącznie do `DETECT_BUDGET` bajtów).
pub fn detect(game: &Path) -> Option<Detected> {
    let files = archives(game);
    if files.is_empty() {
        return None;
    }
    // Spis treści archiwów jest szyfrowany, więc bibliotekę trzeba znaleźć
    // przeszukiwaniem (do DETECT_BUDGET) - wynik zapamiętany do zmiany archiwów.
    let found = super::readers::cached_detect("rage", game, &files, || {
        let mut by_size = files.clone();
        by_size.sort_by_key(|(_, len)| *len);
        let mut read = 0u64;
        by_size.iter().any(|(path, _)| {
            if read >= DETECT_BUDGET {
                return false;
            }
            let Some(disk) = Disk::open(path) else { return false };
            let budget = DETECT_BUDGET - read;
            scan(&disk, budget, DETECT_CHUNK, &mut read, &mut |at| looks_like_library(&disk, at).then_some(at + 4))
        })
    });
    if !found {
        return None;
    }
    let exe = find(game, EXE).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    Some(Detected { engine: "RAGE gen9", version: Some("SGD2".into()), api: "DirectX 12", files, exe })
}

// --------------------------------- indeks ------------------------------------

/// Shader w indeksie: etap, czy niesie RTS0, sygnatury do sprawdzenia łączenia.
struct Info {
    kind: Kind,
    rs: bool,
    outputs: Vec<SigElement>,
    inputs: Vec<SigElement>,
}

/// Czy VS zasila PS tak, jak sprawdza to D3D12: każde wejście PS (poza
/// wartościami systemowymi, których nie daje poprzedni etap) zapisane przez VS
/// z pełną maską i w tym samym rejestrze. `None`: VS zapisuje jedno wyjście
/// dwa razy.
fn feeds(vs: &Info, ps: &Info) -> Option<bool> {
    let mut outs: HashMap<(&str, u32), &SigElement> = HashMap::new();
    for o in &vs.outputs {
        if outs.insert((o.name.as_str(), o.index), o).is_some() {
            return None;
        }
    }
    Some(ps.inputs.iter().all(|i| {
        let o = outs.get(&(i.name.as_str(), i.index));
        if o.is_some_and(|o| o.register != i.register) {
            return false;
        }
        // Clip/cull distance, indeks celu i widoku: zapisuje poprzedni etap.
        if i.sysval != 0 && !(2..=5).contains(&i.sysval) {
            return true;
        }
        o.is_some_and(|o| i.mask & !o.mask & 0xF == 0)
    }))
}

#[derive(Default)]
struct Counts {
    libraries: usize,
    effects: usize,
    passes: usize,
    undecoded: usize,
    mismatched: usize,
    unlinked: usize,
    malformed: usize,
    without_rs: usize,
    bad: usize,
    over_cap: usize,
}

/// Shadery i potoki: każde przejście efektu, które jest potokiem D3D12, z
/// root signature wbudowaną w shadery (`rs: None`). Shadery efektów bez
/// tablicy przejść trafiają do puli swojej biblioteki.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let files = archives(game);
    if files.is_empty() {
        return Err(format!("{UPDATE_DIR}: brak archiwów RPF / no RPF archives"));
    }
    let mut set = Set12::default();
    let mut infos: Vec<Info> = Vec::new();
    let mut ids: HashMap<[u8; 16], usize> = HashMap::new();
    let mut seen: HashSet<[Option<usize>; 6]> = HashSet::new();
    let mut pools: Vec<Vec<usize>> = Vec::new();
    let mut n = Counts::default();
    let mut read = 0u64;
    let total = files.len();
    for (k, (path, _)) in files.iter().enumerate() {
        say(format!("scan {k} {total}"));
        let Some(disk) = Disk::open(path) else {
            say(format!("warning nie można otworzyć / cannot open: {}", path.display()));
            continue;
        };
        let file = disk.file.clone();
        let mut add = |off: u64, c: &Container| -> Option<usize> {
            let identity = c.identity();
            if let Some(&id) = ids.get(&identity) {
                return Some(id);
            }
            let kind = c.kind()?;
            let (file, size) = (file.clone(), c.bytes.len());
            let lazy = move || {
                let mut b = vec![0u8; size];
                read_at(&file, off, &mut b).ok()?;
                // Gra zaktualizowana od indeksu: inne bajty są pomijane.
                Container::parse(&b).filter(|x| x.identity() == identity && x.bytes.len() == size)?;
                Some(b)
            };
            set.shaders.push(Bytes::Lazy(Arc::new(lazy)));
            infos.push(Info {
                kind,
                rs: c.root_signature().is_some(),
                outputs: if kind == Kind::Vertex { c.outputs() } else { Vec::new() },
                inputs: if kind == Kind::Pixel { c.inputs() } else { Vec::new() },
            });
            ids.insert(identity, infos.len() - 1);
            Some(infos.len() - 1)
        };
        let mut libraries = Vec::new();
        scan(&disk, u64::MAX, SCAN_CHUNK, &mut read, &mut |at| {
            let lib = read_library(&disk, at, &mut add)?;
            let end = lib.end;
            libraries.push(lib);
            Some(end)
        });
        for lib in libraries {
            n.libraries += 1;
            n.bad += lib.shaders.iter().flatten().filter(|s| s.is_none()).count();
            plan_library(&lib, &infos, &mut set.pipelines, &mut seen, &mut pools, &mut n);
        }
    }
    say(format!("scan {total} {total}"));
    if n.libraries == 0 {
        return Err(format!(
            "{UPDATE_DIR}: w archiwach nie ma biblioteki shaderów SGD2 / no SGD2 shader library in the archives"
        ));
    }
    // Pule: tylko shadery, których żaden znany potok nie używa (bez powtórzeń
    // tej samej biblioteki w kilku archiwach).
    let used: HashSet<usize> = set.pipelines.iter().flat_map(|p| p.stages.iter().flatten().copied()).collect();
    for mut pool in pools {
        pool.retain(|s| !used.contains(s));
        pool.sort_unstable();
        pool.dedup();
        if !pool.is_empty() && !set.pools.iter().any(|(p, _)| *p == pool) {
            set.pools.push((pool, None));
        }
    }
    let count = |k: Kind| infos.iter().filter(|i| i.kind == k).count();
    set.notes.push(format!(
        "biblioteki SGD2 / SGD2 libraries: {} ({:.2} GB), shadery / shaders: {} (VS {}, PS {}, CS {}, inne / other {}), efekty / effects: {}, przejścia / passes: {} -> potoki / pipelines: {}",
        n.libraries,
        read as f64 / 1e9,
        infos.len(),
        count(Kind::Vertex),
        count(Kind::Pixel),
        count(Kind::Compute),
        infos.len() - count(Kind::Vertex) - count(Kind::Pixel) - count(Kind::Compute),
        n.effects,
        n.passes,
        set.pipelines.len()
    ));
    let extra = [
        (n.undecoded, "efekty bez tablicy przejść (shadery w puli) / effects without a pass table (shaders pooled)"),
        (n.mismatched, "przejścia z shaderem innego etapu / passes naming a shader of another stage"),
        (n.unlinked, "przejścia pominięte: VS nie zasila PS / passes left out: their VS doesn't feed their PS"),
        (n.malformed, "przejścia pominięte: VS zapisuje wyjście dwa razy / passes left out: a VS writes one output twice"),
        (n.without_rs, "przejścia bez root signature (RTS0) / passes without a root signature (RTS0)"),
        (n.bad, "wadliwe kontenery / malformed containers"),
        (n.over_cap, "potoki ponad limit / pipelines over the cap"),
    ];
    for (v, text) in extra {
        if v > 0 {
            set.notes.push(format!("{text}: {v}"));
        }
    }
    Ok(set)
}

/// Potoki z przejść efektów jednej biblioteki.
fn plan_library(
    lib: &Library,
    infos: &[Info],
    pipelines: &mut Vec<Pipeline12>,
    seen: &mut HashSet<[Option<usize>; 6]>,
    pools: &mut Vec<Vec<usize>>,
    n: &mut Counts,
) {
    let mut pool = Vec::new();
    for e in &lib.effects {
        n.effects += 1;
        let Some(passes) = &e.passes else {
            if e.stages.iter().any(|s| !s.is_empty()) {
                n.undecoded += 1;
                for (slot, list) in e.stages.iter().enumerate() {
                    pool.extend(list.iter().filter_map(|&i| lib.shaders[slot].get(i as usize).copied().flatten()));
                }
            }
            continue;
        };
        for pass in passes {
            n.passes += 1;
            if !is_pipeline(pass) {
                continue;
            }
            let mut stages = [None; 6];
            let mut ok = true;
            for slot in 0..6 {
                if pass[slot] == 0 || !ok {
                    continue;
                }
                let id = e.stages[slot]
                    .get(pass[slot] as usize - 1)
                    .and_then(|&i| lib.shaders[slot].get(i as usize).copied().flatten());
                match id {
                    Some(id) if infos[id].kind == SLOT_KIND[slot] => stages[SLOT_STAGE[slot]] = Some(id),
                    Some(_) => {
                        n.mismatched += 1;
                        ok = false;
                    }
                    None => ok = false,
                }
            }
            if !ok {
                continue;
            }
            if !stages.iter().flatten().any(|&s| infos[s].rs) {
                n.without_rs += 1;
                continue;
            }
            // Z GS albo DS shader pikseli czyta tamten etap, nie VS.
            if let (Some(vs), Some(ps), None, None) = (stages[0], stages[1], stages[2], stages[4]) {
                match feeds(&infos[vs], &infos[ps]) {
                    None => {
                        n.malformed += 1;
                        continue;
                    }
                    Some(false) => {
                        n.unlinked += 1;
                        continue;
                    }
                    Some(true) => {}
                }
            }
            if !seen.insert(stages) {
                continue;
            }
            if pipelines.len() >= MAX_PIPELINES {
                n.over_cap += 1;
                continue;
            }
            pipelines.push(Pipeline12 { rs: None, stages });
        }
    }
    if !pool.is_empty() {
        pools.push(pool);
    }
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::{build, dxil, sig1};
    use super::*;

    fn rts0() -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[0] = 2;
        b[8] = 24;
        b[16] = 24;
        b
    }

    fn vs(tag: u8, out: &str) -> Vec<u8> {
        let mut c = build(&[
            (b"ISG1", sig1(&[("POSITION", 0, 0, 3, 0, 0x7)])),
            (b"OSG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), (out, 0, 0, 3, 1, 0x3)])),
            (b"DXIL", dxil(1)),
            (b"RTS0", rts0()),
        ]);
        c[4] = tag;
        c
    }

    fn ps(tag: u8) -> Vec<u8> {
        let mut c = build(&[
            (b"ISG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"OSG1", sig1(&[("SV_Target", 0, 64, 3, 0, 0xF)])),
            (b"DXIL", dxil(0)),
            (b"RTS0", rts0()),
        ]);
        c[4] = tag;
        c
    }

    fn cs(tag: u8) -> Vec<u8> {
        let mut c = build(&[(b"DXIL", dxil(5)), (b"RTS0", rts0())]);
        c[4] = tag;
        c
    }

    fn u32s(b: &mut Vec<u8>, v: &[u32]) {
        for x in v {
            b.extend_from_slice(&x.to_le_bytes());
        }
    }

    /// Efekt: nazwa, indeksy shaderów każdego gniazda, przejścia (techniki po
    /// jednym przejściu) albo brak tablicy.
    fn effect(name: &str, stages: [&[u32]; 6], passes: Option<&[[u8; 6]]>) -> Vec<u8> {
        let mut b = vec![name.len() as u8 + 1];
        b.extend_from_slice(name.as_bytes());
        b.push(0);
        u32s(&mut b, &[0]);
        for s in stages {
            u32s(&mut b, &[s.len() as u32]);
            u32s(&mut b, s);
        }
        b.extend_from_slice(&[0xEE; 8]);
        if let Some(passes) = passes {
            u32s(&mut b, &[passes.len() as u32]);
            for _ in passes {
                u32s(&mut b, &[1 << 24]);
            }
            u32s(&mut b, &[passes.len() as u32]);
            for p in passes {
                b.extend_from_slice(p);
                b.extend_from_slice(&[0u8; 18]);
            }
        }
        b.extend_from_slice(b"proplst\0");
        u32s(&mut b, &[1, 3]);
        b.extend_from_slice(b"abc");
        u32s(&mut b, &[2]);
        b.extend_from_slice(b"x\0");
        b
    }

    /// Biblioteka: shadery każdego gniazda i efekty.
    fn library(slots: [&[Vec<u8>]; 6], effects: &[Vec<u8>]) -> Vec<u8> {
        let mut b = MAGIC.to_vec();
        for list in slots {
            u32s(&mut b, &[list.len() as u32]);
            for c in list {
                b.extend_from_slice(&[3, b'v', b's', 0, 7]);
                u32s(&mut b, &[c.len() as u32]);
                b.extend_from_slice(c);
                b.extend_from_slice(&[0x11; 8 + 144]);
                u32s(&mut b, &[5]);
                b.extend_from_slice(&[0x22; 5]);
            }
        }
        b.extend_from_slice(&[0u8; 8]);
        u32s(&mut b, &[effects.len() as u32]);
        for e in effects {
            b.extend_from_slice(e);
        }
        b
    }

    /// Archiwum: szum, biblioteka na granicy bloku, szum.
    fn archive(lib: &[u8]) -> Vec<u8> {
        let mut b = b"RPF7".to_vec();
        b.resize(1024, 0x5A);
        b.extend_from_slice(lib);
        b.resize(b.len().div_ceil(512) * 512 + 700, 0x33);
        b
    }

    fn sample_library() -> Vec<u8> {
        let vss = [vs(1, "TEXCOORD"), vs(2, "COLOR")];
        let pss = [ps(3)];
        let css = [cs(4), cs(5)];
        let e1 = effect(
            "lit",
            [&[0, 1], &[0], &[], &[], &[], &[0]],
            Some(&[
                [1, 1, 0, 0, 0, 0], // VS+PS
                [2, 1, 0, 0, 0, 0], // VS nie zasila PS
                [1, 0, 0, 0, 0, 0], // sam VS (tylko głębia)
                [0, 0, 0, 0, 0, 1], // CS
                [1, 1, 0, 0, 0, 0], // powtórka
            ]),
        );
        let e2 = effect("misc", [&[], &[], &[], &[], &[], &[1]], None);
        library([&vss, &pss, &[], &[], &[], &css], &[e1, e2])
    }

    fn temp_game(name: &str, rpf: &[u8], exe: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-rage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Update")).unwrap();
        std::fs::write(dir.join("Update").join("update.rpf"), rpf).unwrap();
        std::fs::write(dir.join("Update").join("tiny.rpf"), b"RPF7 nothing here").unwrap();
        if exe {
            std::fs::write(dir.join(EXE), b"MZ").unwrap();
        }
        dir
    }

    #[test]
    fn parses_library_and_pass_table() {
        let lib = sample_library();
        let mut next = 0;
        let l = read_library(&&lib[..], 0, &mut |_, _| {
            next += 1;
            Some(next - 1)
        })
        .unwrap();
        assert_eq!(l.end, lib.len() as u64);
        assert_eq!(l.shaders.each_ref().map(Vec::len), [2, 1, 0, 0, 0, 2]);
        assert_eq!(l.effects.len(), 2);
        assert_eq!(l.effects[0].passes.as_ref().unwrap().len(), 5);
        assert!(l.effects[1].passes.is_none());
        assert!(is_pipeline(&[1, 1, 0, 0, 0, 0]) && is_pipeline(&[0, 0, 0, 0, 0, 1]));
        assert!(!is_pipeline(&[1, 1, 0, 0, 1, 0]) && !is_pipeline(&[1, 0, 0, 0, 0, 1]) && !is_pipeline(&[0, 1, 0, 0, 0, 0]));
        // Dwie pasujące tablice: niejednoznaczne.
        let mut body = vec![0u8; 8];
        for _ in 0..2 {
            u32s(&mut body, &[1, 1 << 24, 1]);
            body.extend_from_slice(&[1, 0, 0, 0, 0, 0]);
            body.extend_from_slice(&[0u8; 18]);
        }
        let stages: [Vec<u32>; 6] = [vec![0], vec![], vec![], vec![], vec![], vec![]];
        assert!(pass_table(&body, &stages).is_none());
        assert_eq!(pass_table(&body[..8 + 36], &stages), Some(vec![[1, 0, 0, 0, 0, 0]]));
    }

    #[test]
    fn rejects_damaged_libraries_without_panic() {
        let lib = sample_library();
        for cut in 0..lib.len() {
            let _ = read_library(&&lib[..cut], 0, &mut |_, _| Some(0));
            let _ = looks_like_library(&&lib[..cut], 0);
        }
        let mut wrong = lib.clone();
        wrong[0] = b'X';
        assert!(read_library(&&wrong[..], 0, &mut |_, _| Some(0)).is_none());
        // Liczba shaderów ponad limit.
        let mut huge = lib.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_library(&&huge[..], 0, &mut |_, _| Some(0)).is_none());
        assert!(!looks_like_library(&&huge[..], 0));
        let mut data = lib.clone();
        for i in (0..data.len()).step_by(7) {
            data[i] ^= 0xA5;
            let _ = read_library(&&data[..], 0, &mut |_, _| Some(0));
        }
    }

    #[test]
    fn detects_game_by_library_in_update_archive() {
        let game = temp_game("detect", &archive(&sample_library()), true);
        let d = detect(&game).unwrap();
        assert_eq!((d.engine, d.api, d.exe.as_deref()), ("RAGE gen9", "DirectX 12", Some(EXE)));
        assert_eq!(d.files.len(), 2);
        let _ = std::fs::remove_dir_all(&game);

        // Biblioteka poza granicą bloku i gra bez folderu update: nie.
        let mut off = vec![0u8; 100];
        off.extend_from_slice(&sample_library());
        let game = temp_game("detect-off", &off, false);
        assert!(detect(&game).is_none());
        let _ = std::fs::remove_dir_all(&game);
        assert!(detect(&std::env::temp_dir().join("tc-rage-none-xyz")).is_none());
    }

    #[test]
    fn indexes_passes_as_pipelines() {
        let mut rpf = archive(&sample_library());
        // Druga kopia tej samej biblioteki: shadery i potoki się nie powtarzają.
        rpf.resize(rpf.len().div_ceil(512) * 512, 0);
        rpf.extend_from_slice(&archive(&sample_library())[1024..]);
        let game = temp_game("index", &rpf, true);
        let lines = std::cell::RefCell::new(Vec::new());
        let set = index(&game, &|l| lines.borrow_mut().push(l)).unwrap();
        assert_eq!(set.shaders.len(), 5);
        let kinds: Vec<Kind> = set
            .shaders
            .iter()
            .map(|b| Container::parse(&b.get().unwrap()).unwrap().kind().unwrap())
            .collect();
        assert_eq!(kinds, [Kind::Vertex, Kind::Vertex, Kind::Pixel, Kind::Compute, Kind::Compute]);
        assert_eq!(set.shaders[2].get().unwrap(), ps(3));
        let p = |s: [Option<usize>; 6]| Pipeline12 { rs: None, stages: s };
        assert_eq!(
            set.pipelines,
            vec![
                p([Some(0), Some(2), None, None, None, None]),
                p([Some(0), None, None, None, None, None]),
                p([None, None, None, None, None, Some(3)]),
            ]
        );
        assert_eq!(set.pools, vec![(vec![4], None)]);
        assert!(set.signatures.is_empty());
        assert!(set.notes.iter().any(|n| n.contains("VS doesn't feed") && n.ends_with(": 2")));
        assert!(lines.borrow().iter().any(|l| l == "scan 2 2"));
        let _ = std::fs::remove_dir_all(&game);

        let game = temp_game("index-empty", b"RPF7", false);
        assert!(index(&game, &|_| {}).is_err());
        let _ = std::fs::remove_dir_all(&game);
    }
}
