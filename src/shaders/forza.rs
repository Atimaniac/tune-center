//! ForzaTech (Forza Horizon 5/6 w DirectX 12): shadery w archiwach ZIP w
//! folderze `media`. Tylko odczyt plików gry.
//!
//! - `media\ShadersDXIL.zip` - shadery silnika: `*.vshader`, `*.pshader`,
//!   `*.cshader`, `*.hshader`, `*.dshader`, `*.gshader`, `*.rtshader`,
//! - archiwa materiałów (m.in. pod `media\_library`) - dla każdego scenariusza
//!   oświetlenia `<materiał><Scenariusz>.pcdxil.vso` / `.pso` / `.lso` (VS, PS,
//!   biblioteka ray tracingu).
//!
//! Każdy wpis to jeden kontener DXBC (DXIL w środku), zapisany (metoda 0)
//! albo skompresowany DEFLATE (metoda 8). Shadery rastrowe i compute niosą
//! root signature (część RTS0), więc potoki powstają bez zapisu z gry.
//! Biblioteki ray tracingu jej nie mają - są pomijane. Wpisy zaszyfrowane
//! (metoda 22: dźwięk i konfiguracja, nigdy shadery) i w innych metodach też.
//!
//! Potoki: `.vso` i `.pso` o tej samej nazwie w jednym archiwum - jeden potok;
//! każdy compute - jeden; reszta archiwum - jedna pula.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{self, read_at, Container, Kind};
use super::readers::{Bytes, Detected, Pipeline12, Set12};

const ENGINE: &str = "ForzaTech";
const FORMAT: &str = "pcdxil";

/// Najwięcej wpisów jednego archiwum (więcej - archiwum pominięte).
const MAX_ZIP_ENTRIES: usize = 1 << 20;
/// Najwięcej wpisów shaderów czytanych w całej grze.
const MAX_SHADER_ENTRIES: usize = 1 << 21;
/// Największy katalog centralny (1 << 20 wpisów z długimi nazwami).
const MAX_DIRECTORY: u64 = 256 << 20;
/// Największy kontener (rozmiar po rozpakowaniu) i wpis przed rozpakowaniem.
const MAX_CONTAINER: u64 = 16 << 20;
const MAX_STORED: u64 = 2 * MAX_CONTAINER;
/// Najmniejszy sensowny kontener (nagłówek z jedną częścią).
const MIN_CONTAINER: u64 = 40;
/// Najwięcej archiwów w `media` i głębokość folderów.
const MAX_ZIPS: usize = 100_000;
const MAX_DEPTH: usize = 8;

/// `detect`: część katalogu centralnego, liczba wpisów i bajtów próbki.
const DETECT_DIRECTORY: u64 = 2 << 20;
const DETECT_SAMPLE: usize = 16;
const DETECT_BYTES: u64 = 2 << 20;

fn engine_zip(game: &Path) -> PathBuf {
    game.join("media").join("ShadersDXIL.zip")
}

fn library(game: &Path) -> PathBuf {
    game.join("media").join("_library")
}

// ------------------------------------ ZIP ------------------------------------

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

const EOCD: &[u8; 4] = b"PK\x05\x06";
const LOCATOR: &[u8; 4] = b"PK\x06\x07";
const EOCD64: &[u8; 4] = b"PK\x06\x06";
const CENTRAL: &[u8; 4] = b"PK\x01\x02";
const LOCAL: &[u8; 4] = b"PK\x03\x04";

/// Wpis archiwum z katalogu centralnego.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    name: String,
    flags: u16,
    method: u16,
    /// Rozmiar zapisany i po rozpakowaniu.
    stored: u64,
    size: u64,
    /// Położenie nagłówka lokalnego.
    local: u64,
}

#[derive(Debug, PartialEq)]
enum ZipError {
    /// Nie archiwum albo uszkodzone.
    Broken,
    /// Za dużo wpisów albo za duży katalog.
    Huge,
}

/// Koniec katalogu centralnego: (położenie, rozmiar). Ostatni podpis, po
/// którym mieści się cały rekord; ZIP64, gdy jest lokalizator przed nim.
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

/// Rekordy katalogu centralnego kolejno, dopóki są całe i mają podpis.
/// Więcej niż `max` - `Huge`.
fn parse_directory(b: &[u8], max: usize) -> Result<Vec<Entry>, ZipError> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while let Some(head) = b.get(p..p + 46) {
        if &head[..4] != CENTRAL {
            break;
        }
        if out.len() >= max {
            return Err(ZipError::Huge);
        }
        let field = |o: usize| le16(head, o).map(usize::from).unwrap_or(0);
        let (n, x, c) = (field(28), field(30), field(32));
        let end = p + 46 + n + x + c;
        let (Some(name), Some(extra)) = (b.get(p + 46..p + 46 + n), b.get(p + 46 + n..p + 46 + n + x)) else {
            break;
        };
        if end > b.len() {
            break;
        }
        let mut e = Entry {
            name: String::from_utf8_lossy(name).replace('\\', "/"),
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
    Ok(out)
}

/// Pole dodatkowe ZIP64 (0x0001): kolejno rozmiar, rozmiar zapisany i
/// położenie - tylko te, które w rekordzie mają wartość 0xFFFFFFFF.
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

/// Katalog centralny archiwum. `partial` - czyta najwyżej `max_bytes` bajtów
/// katalogu (do próbki); inaczej większy katalog to `Huge`.
fn directory(file: &File, len: u64, max_bytes: u64, max_entries: usize, partial: bool) -> Result<Vec<Entry>, ZipError> {
    let (offset, size) = end_record(file, len).ok_or(ZipError::Broken)?;
    if size > max_bytes && !partial {
        return Err(ZipError::Huge);
    }
    let mut b = vec![0u8; size.min(max_bytes) as usize];
    read_at(file, offset, &mut b).map_err(|_| ZipError::Broken)?;
    let entries = parse_directory(&b, max_entries)?;
    if entries.is_empty() && size > 0 {
        return Err(ZipError::Broken);
    }
    Ok(entries)
}

/// Bajty wpisu (rozpakowane). `None`: zaszyfrowany, inna metoda, rozmiar
/// poza granicami kontenera albo dane uszkodzone.
fn read_entry(file: &File, len: u64, e: &Entry) -> Option<Vec<u8>> {
    if e.flags & 1 != 0 || !(e.method == 0 || e.method == 8) {
        return None;
    }
    if !(MIN_CONTAINER..=MAX_CONTAINER).contains(&e.size) || e.stored > MAX_STORED {
        return None;
    }
    let mut head = [0u8; 30];
    if e.local.checked_add(30)? > len {
        return None;
    }
    read_at(file, e.local, &mut head).ok()?;
    if &head[..4] != LOCAL {
        return None;
    }
    let data = e.local + 30 + u64::from(le16(&head, 26)?) + u64::from(le16(&head, 28)?);
    if data.checked_add(e.stored)? > len {
        return None;
    }
    let mut stored = vec![0u8; e.stored as usize];
    read_at(file, data, &mut stored).ok()?;
    let out = if e.method == 0 {
        stored
    } else {
        super::inflate::inflate(&stored, e.size as usize)?.0
    };
    (out.len() as u64 == e.size).then_some(out)
}

// --------------------------------- shadery -----------------------------------

/// Nazwa wpisu z shaderem: `.pcdxil.?so` albo `.?shader` / `.??shader`.
fn shader_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if let Some(stem) = n.strip_suffix("so") {
        let mut it = stem.chars().rev();
        if it.next().is_some_and(|c| c.is_ascii_lowercase()) && stem[..stem.len() - 1].ends_with(".pcdxil.") {
            return true;
        }
    }
    if let Some(stem) = n.strip_suffix("shader") {
        let letters = stem.rsplit('.').next().unwrap_or("");
        return stem.contains('.') && (1..=2).contains(&letters.len()) && letters.bytes().all(|c| c.is_ascii_lowercase());
    }
    false
}

/// Kontener zajmujący cały wpis (z kodem programu).
fn whole_container(b: &[u8]) -> Option<Container<'_>> {
    Container::parse(b).filter(|c| c.bytes.len() == b.len() && c.kind().is_some())
}

/// Plik .exe gry w jej folderze: najpierw `ForzaHorizon*`, potem `Forza*`
/// (instalacji z aplikacji Xbox może się nie dać odczytać - wtedy brak).
fn exe_name(game: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(game)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".exe") && l.starts_with("forza") && !l.contains("crash") && !l.contains("launcher")
        })
        .collect();
    names.sort_by_key(|n| (!n.to_ascii_lowercase().starts_with("forzahorizon"), n.len(), n.clone()));
    names.into_iter().next()
}

/// Gra ForzaTech: `media\ShadersDXIL.zip` i folder `media\_library`. Próbka
/// kilku shaderów silnika (do kilku MB) mówi, czy niosą root signature.
pub fn detect(game: &Path) -> Option<Detected> {
    let zip = engine_zip(game);
    let len = std::fs::metadata(&zip).ok().filter(|m| m.is_file())?.len();
    if !library(game).is_dir() {
        return None;
    }
    let (programs, rts0) = sample(&zip, len);
    let version = if programs > 0 && rts0 * 10 >= programs * 9 { format!("{FORMAT}+RTS0") } else { FORMAT.to_owned() };
    Some(Detected {
        engine: ENGINE,
        version: Some(version),
        api: "DirectX 12",
        files: vec![(zip, len)],
        exe: exe_name(game),
    })
}

/// (programy, programy z RTS0) w pierwszych wpisach shaderów archiwum.
fn sample(zip: &Path, len: u64) -> (usize, usize) {
    let Ok(file) = File::open(zip) else { return (0, 0) };
    let Ok(entries) = directory(&file, len, DETECT_DIRECTORY, MAX_ZIP_ENTRIES, true) else { return (0, 0) };
    let (mut programs, mut rts0, mut read) = (0, 0, 0u64);
    for e in entries.iter().filter(|e| shader_name(&e.name)).take(DETECT_SAMPLE) {
        // Wpis ponad budżet - pominięty (próbka czyta najwyżej DETECT_BYTES).
        if e.stored > DETECT_BYTES - read {
            continue;
        }
        read += e.stored;
        let Some(b) = read_entry(&file, len, e) else { continue };
        let Some(c) = whole_container(&b) else { continue };
        if c.kind() != Some(Kind::Other) {
            programs += 1;
            rts0 += usize::from(c.root_signature().is_some());
        }
    }
    (programs, rts0)
}

/// Archiwa w `media` (rekurencyjnie), w stałej kolejności.
fn zips(media: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(media.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(t) = e.file_type() else { continue };
            let path = e.path();
            if t.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if t.is_file()
                && path.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip"))
                && out.len() < MAX_ZIPS
            {
                out.push(path);
            }
        }
    }
    out.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    out
}

/// Pominięte i policzone przy indeksowaniu.
#[derive(Default)]
struct Counts {
    zips: usize,
    entries: usize,
    pairs: usize,
    unlinked: usize,
    libraries: usize,
    without_rs: usize,
    broken: usize,
    first_broken: Option<String>,
    huge: usize,
    unreadable: usize,
    not_one: usize,
    capped: bool,
}

/// Wpis z shaderem do potoku po nazwie: VS (wyjścia) albo PS (wymagania).
type Gives = (usize, Vec<(String, u32, u32)>, bool);
type Needs = (usize, Vec<(String, u32, u32)>);

/// Shadery gry ze wszystkich archiwów `media`.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let list = zips(&game.join("media"));
    let total = list.len();
    let mut set = Set12::default();
    let mut c = Counts::default();
    // Tożsamość kontenera -> indeks shadera (`None` - pominięty).
    let mut ids: HashMap<[u8; 16], Option<usize>> = HashMap::new();
    let mut kinds: Vec<Kind> = Vec::new();
    let mut computes: HashSet<usize> = HashSet::new();
    let mut pairs: HashSet<(usize, usize)> = HashSet::new();
    say(format!("scan 0 {total}"));
    'zips: for (n, path) in list.iter().enumerate() {
        say(format!("scan {n} {total}"));
        let Ok(file) = File::open(path) else { continue };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let entries = match directory(&file, len, MAX_DIRECTORY, MAX_ZIP_ENTRIES, false) {
            Ok(e) => e,
            Err(ZipError::Huge) => {
                c.huge += 1;
                continue;
            }
            Err(ZipError::Broken) => {
                c.broken += 1;
                c.first_broken.get_or_insert_with(|| path.file_name().unwrap_or_default().to_string_lossy().into_owned());
                continue;
            }
        };
        let shader_entries: Vec<&Entry> = entries.iter().filter(|e| shader_name(&e.name)).collect();
        if shader_entries.is_empty() {
            continue;
        }
        c.zips += 1;
        let zip = Arc::new(path.clone());
        let mut by_name: BTreeMap<String, (Option<Gives>, Option<Needs>)> = BTreeMap::new();
        let mut pool: Vec<usize> = Vec::new();
        for e in shader_entries {
            if c.entries >= MAX_SHADER_ENTRIES {
                c.capped = true;
                break;
            }
            let Some(bytes) = read_entry(&file, len, e) else {
                c.unreadable += 1;
                continue;
            };
            let Some(con) = whole_container(&bytes) else {
                c.not_one += 1;
                continue;
            };
            c.entries += 1;
            let id = con.identity();
            let kind = con.kind().unwrap_or(Kind::Other);
            let slot = match ids.get(&id) {
                Some(s) => *s,
                None => {
                    let s = if kind == Kind::Other {
                        c.libraries += 1;
                        None
                    } else if con.root_signature().is_none() {
                        c.without_rs += 1;
                        None
                    } else {
                        set.shaders.push(lazy(zip.clone(), e.clone(), id));
                        kinds.push(kind);
                        Some(set.shaders.len() - 1)
                    };
                    ids.insert(id, s);
                    s
                }
            };
            let Some(i) = slot else { continue };
            let lower = e.name.to_ascii_lowercase();
            match kinds[i] {
                Kind::Compute => {
                    if computes.insert(i) {
                        set.pipelines.push(Pipeline12 { rs: None, stages: [None, None, None, None, None, Some(i)] });
                    }
                }
                Kind::Vertex if lower.ends_with(".pcdxil.vso") => {
                    let (gives, position) = container::vs_gives(&con);
                    let slot = by_name.entry(lower[..lower.len() - 4].to_owned()).or_default();
                    if let Some(old) = slot.0.replace((i, gives, position)) {
                        pool.push(old.0);
                    }
                }
                Kind::Pixel if lower.ends_with(".pcdxil.pso") => {
                    let slot = by_name.entry(lower[..lower.len() - 4].to_owned()).or_default();
                    if let Some(old) = slot.1.replace((i, container::ps_needs(&con))) {
                        pool.push(old.0);
                    }
                }
                _ => pool.push(i),
            }
        }
        for (vs, ps) in by_name.into_values() {
            match (vs, ps) {
                (Some((v, gives, position)), Some((p, needs))) => {
                    if !position || !container::links(&needs, &gives) {
                        c.unlinked += 1;
                        pool.extend([v, p]);
                    } else if pairs.insert((v, p)) {
                        c.pairs += 1;
                        set.pipelines.push(Pipeline12 { rs: None, stages: [Some(v), Some(p), None, None, None, None] });
                    }
                }
                (Some((v, ..)), None) => pool.push(v),
                (None, Some((p, _))) => pool.push(p),
                (None, None) => {}
            }
        }
        let mut seen = HashSet::new();
        pool.retain(|i| seen.insert(*i));
        if !pool.is_empty() {
            set.pools.push((pool, None));
        }
        if c.capped {
            break 'zips;
        }
    }
    say(format!("scan {total} {total}"));
    let notes = summary(&c, &kinds);
    for line in &notes {
        say(line.clone());
    }
    set.notes = notes;
    if set.shaders.is_empty() {
        return Err(format!(
            "w archiwach folderu media nie ma shaderów z root signature / the zips in media hold no shaders with a root signature ({} / {total})",
            c.zips
        ));
    }
    Ok(set)
}

/// Shader czytany na żądanie: wpis archiwum jeszcze raz, rozpakowany;
/// kontener zmieniony od indeksowania (łatka gry) jest pomijany.
fn lazy(zip: Arc<PathBuf>, e: Entry, id: [u8; 16]) -> Bytes {
    Bytes::Lazy(Arc::new(move || {
        let file = File::open(zip.as_path()).ok()?;
        let len = file.metadata().ok()?.len();
        let b = read_entry(&file, len, &e)?;
        (whole_container(&b)?.identity() == id).then_some(b)
    }))
}

/// Uwagi: liczby shaderów i potoków oraz to, co pominięto.
fn summary(c: &Counts, kinds: &[Kind]) -> Vec<String> {
    let count = |k: Kind| kinds.iter().filter(|x| **x == k).count();
    let mut out = vec![format!(
        "{} archiwów z shaderami, {} wpisów -> {} shaderów (VS {}, PS {}, CS {}, inne {}), {} par VS+PS po nazwie / {} zips with shaders, {} entries -> {} shaders (VS {}, PS {}, CS {}, other {}), {} VS+PS pairs by name",
        c.zips, c.entries, kinds.len(), count(Kind::Vertex), count(Kind::Pixel), count(Kind::Compute),
        kinds.len() - count(Kind::Vertex) - count(Kind::Pixel) - count(Kind::Compute), c.pairs,
        c.zips, c.entries, kinds.len(), count(Kind::Vertex), count(Kind::Pixel), count(Kind::Compute),
        kinds.len() - count(Kind::Vertex) - count(Kind::Pixel) - count(Kind::Compute), c.pairs,
    )];
    let mut warn = |n: usize, pl: &str, en: &str| {
        if n > 0 {
            out.push(format!("warning {pl} / {en}: {n}"));
        }
    };
    warn(c.libraries, "biblioteki ray tracingu i inne etapy bez potoku (pominięte - potrzebny zapis z gry)", "ray tracing libraries and other stages without a pipeline (skipped - they need a recording)");
    warn(c.without_rs, "shadery bez wbudowanej root signature (pominięte)", "shaders without an embedded root signature (skipped)");
    warn(c.unlinked, "pary po nazwie, w których VS nie zasila PS (do puli)", "pairs by name whose VS doesn't feed the PS (pooled)");
    warn(c.unreadable, "wpisy nieczytelne (zaszyfrowane, inna kompresja, uszkodzone)", "unreadable entries (encrypted, other compression, damaged)");
    warn(c.not_one, "wpisy, które nie są jednym kontenerem", "entries that aren't one container");
    warn(c.huge, "archiwa z za dużą liczbą wpisów (pominięte)", "zips with too many entries (skipped)");
    if c.broken > 0 {
        out.push(format!(
            "warning archiwa, które się nie otwierają (pominięte) / zips that don't open (skipped): {} ({})",
            c.broken,
            c.first_broken.as_deref().unwrap_or("")
        ));
    }
    if c.capped {
        out.push("warning za dużo wpisów shaderów, reszta pominięta / too many shader entries, the rest skipped".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::{build, dxil, sig1};
    use super::*;

    /// Wpis testowy: nazwa, dane, kompresja DEFLATE (bloki niekompresowane),
    /// zaszyfrowany.
    struct Item<'a> {
        name: &'a str,
        data: Vec<u8>,
        deflate: bool,
        encrypted: bool,
    }

    fn item(name: &str, data: Vec<u8>, deflate: bool) -> Item<'_> {
        Item { name, data, deflate, encrypted: false }
    }

    /// DEFLATE z samych bloków niekompresowanych.
    fn stored_deflate(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[]] } else { data.chunks(0xFFFF).collect() };
        for (k, ch) in chunks.iter().enumerate() {
            out.push(u8::from(k + 1 == chunks.len()));
            out.extend_from_slice(&(ch.len() as u16).to_le_bytes());
            out.extend_from_slice(&(!(ch.len() as u16)).to_le_bytes());
            out.extend_from_slice(ch);
        }
        out
    }

    /// Archiwum ZIP; `zip64` - dodatkowo rekord i lokalizator ZIP64 (a w
    /// zwykłym rekordzie wartości 0xFFFF / 0xFFFFFFFF).
    fn zip(items: &[Item], zip64: bool) -> Vec<u8> {
        let mut b = b"junk".to_vec();
        let mut central = Vec::new();
        for it in items {
            let local = b.len() as u32;
            let (method, body) = if it.deflate { (8u16, stored_deflate(&it.data)) } else { (0, it.data.clone()) };
            let flags = u16::from(it.encrypted);
            let head = |sig: &[u8; 4], central: bool| {
                let mut h = sig.to_vec();
                if central {
                    h.extend_from_slice(&20u16.to_le_bytes());
                }
                h.extend_from_slice(&20u16.to_le_bytes());
                h.extend_from_slice(&flags.to_le_bytes());
                h.extend_from_slice(&method.to_le_bytes());
                h.extend_from_slice(&[0u8; 8]); // czas, data, CRC
                h.extend_from_slice(&(body.len() as u32).to_le_bytes());
                h.extend_from_slice(&(it.data.len() as u32).to_le_bytes());
                h.extend_from_slice(&(it.name.len() as u16).to_le_bytes());
                h
            };
            let mut l = head(LOCAL, false);
            l.extend_from_slice(&3u16.to_le_bytes());
            l.extend_from_slice(it.name.as_bytes());
            l.extend_from_slice(b"abc");
            b.extend_from_slice(&l);
            b.extend_from_slice(&body);
            let mut c = head(CENTRAL, true);
            if zip64 {
                // Położenie w polu ZIP64.
                c.extend_from_slice(&12u16.to_le_bytes());
            } else {
                c.extend_from_slice(&0u16.to_le_bytes());
            }
            c.extend_from_slice(&0u16.to_le_bytes()); // komentarz
            c.extend_from_slice(&[0u8; 8]); // dysk, atrybuty
            c.extend_from_slice(&(if zip64 { u32::MAX } else { local }).to_le_bytes());
            c.extend_from_slice(it.name.as_bytes());
            if zip64 {
                c.extend_from_slice(&1u16.to_le_bytes());
                c.extend_from_slice(&8u16.to_le_bytes());
                c.extend_from_slice(&u64::from(local).to_le_bytes());
            }
            central.extend_from_slice(&c);
        }
        let at = b.len() as u64;
        b.extend_from_slice(&central);
        if zip64 {
            let rec = b.len() as u64;
            b.extend_from_slice(EOCD64);
            b.extend_from_slice(&44u64.to_le_bytes());
            b.extend_from_slice(&[0u8; 12]);
            b.extend_from_slice(&(items.len() as u64).to_le_bytes());
            b.extend_from_slice(&(items.len() as u64).to_le_bytes());
            b.extend_from_slice(&(central.len() as u64).to_le_bytes());
            b.extend_from_slice(&at.to_le_bytes());
            b.extend_from_slice(LOCATOR);
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&rec.to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
        }
        b.extend_from_slice(EOCD);
        b.extend_from_slice(&[0u8; 4]);
        let n = if zip64 { 0xFFFF } else { items.len() as u16 };
        b.extend_from_slice(&n.to_le_bytes());
        b.extend_from_slice(&n.to_le_bytes());
        b.extend_from_slice(&(central.len() as u32).to_le_bytes());
        b.extend_from_slice(&(if zip64 { u32::MAX } else { at as u32 }).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(b"hi");
        b
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tc-forza-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rs() -> Vec<u8> {
        vec![2u8, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0]
    }

    fn vs(tag: u8) -> Vec<u8> {
        build(&[
            (b"OSG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"DXIL", dxil(1)),
            (b"RTS0", rs()),
            (b"PRIV", vec![tag]),
        ])
    }

    fn ps(tag: u8, needs: &str) -> Vec<u8> {
        build(&[
            (b"ISG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), (needs, 0, 0, 3, 1, 0x3)])),
            (b"DXIL", dxil(0)),
            (b"RTS0", rs()),
            (b"PRIV", vec![tag]),
        ])
    }

    fn cs(tag: u8) -> Vec<u8> {
        build(&[(b"DXIL", dxil(5)), (b"RTS0", rs()), (b"PRIV", vec![tag])])
    }

    #[test]
    fn matches_shader_names() {
        for n in ["a/b.vshader", "x.CSHADER", "m/matDay.pcdxil.vso", "q.pcdxil.lso", "r.rtshader"] {
            assert!(shader_name(n), "{n}");
        }
        for n in ["shader", "a.xyzshader", "a.vso", "a.pcdxil.so", "a.pcdxil.v1so", "a.txt", "a.pcdxil.vsox"] {
            assert!(!shader_name(n), "{n}");
        }
    }

    #[test]
    fn reads_zip_stored_deflate_and_zip64() {
        let big: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let dir = temp_dir("zip");
        for zip64 in [false, true] {
            let items = [
                item("one.bin", big[..100].to_vec(), false),
                item("dir\\two.bin", big.clone(), true),
                Item { name: "secret.bin", data: big[..64].to_vec(), deflate: false, encrypted: true },
            ];
            let path = dir.join(format!("t{zip64}.zip"));
            std::fs::write(&path, zip(&items, zip64)).unwrap();
            let file = File::open(&path).unwrap();
            let len = file.metadata().unwrap().len();
            let entries = directory(&file, len, MAX_DIRECTORY, 10, false).unwrap();
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[1].name, "dir/two.bin");
            assert_eq!(read_entry(&file, len, &entries[0]).unwrap(), &big[..100]);
            assert_eq!(read_entry(&file, len, &entries[1]).unwrap(), big);
            assert!(read_entry(&file, len, &entries[2]).is_none());
            assert_eq!(directory(&file, len, MAX_DIRECTORY, 2, false), Err(ZipError::Huge));
            assert_eq!(directory(&file, len, 10, 10, false), Err(ZipError::Huge));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn damaged_zips_do_not_panic() {
        let items = [item("a.cshader", cs(1), true), item("b.pcdxil.vso", vs(1), false)];
        let good = zip(&items, true);
        let dir = temp_dir("bad");
        let path = dir.join("x.zip");
        let mut variants: Vec<Vec<u8>> = (0..good.len()).step_by(7).map(|n| good[..n].to_vec()).collect();
        for k in (0..good.len()).step_by(3) {
            let mut b = good.clone();
            b[k] ^= 0xA5;
            variants.push(b);
        }
        for b in variants {
            std::fs::write(&path, &b).unwrap();
            let file = File::open(&path).unwrap();
            let len = b.len() as u64;
            if let Ok(entries) = directory(&file, len, MAX_DIRECTORY, 100, false) {
                for e in &entries {
                    let _ = read_entry(&file, len, e);
                }
            }
        }
        assert!(parse_directory(b"PK\x01\x02", 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_and_indexes_game() {
        let game = temp_dir("game");
        let media = game.join("media");
        std::fs::create_dir_all(media.join("_library").join("mats")).unwrap();
        std::fs::write(game.join("ForzaHorizon6.exe"), b"MZ").unwrap();
        std::fs::write(game.join("ForzaCrashHandler.exe"), b"MZ").unwrap();
        let library = build(&[(b"DXIL", dxil(6)), (b"PRIV", vec![9])]);
        let engine = [
            item("shaders/a.cshader", cs(1), true),
            item("shaders/b.vshader", vs(2), false),
            item("shaders/rt.rtshader", library, false),
            Item { name: "shaders/x.cshader", data: cs(3), deflate: false, encrypted: true },
            item("config.txt", b"not a shader at all, just some text here".to_vec(), false),
        ];
        std::fs::write(media.join("ShadersDXIL.zip"), zip(&engine, false)).unwrap();
        let mats = [
            item("m/MatDay.pcdxil.vso", vs(10), true),
            item("m/MatDay.pcdxil.pso", ps(11, "TEXCOORD"), false),
            item("m/Odd.pcdxil.vso", vs(12), false),
            item("m/Odd.pcdxil.pso", ps(13, "COLOR"), false),
            item("m/Lone.pcdxil.pso", ps(14, "TEXCOORD"), true),
            item("m/dup.cshader", cs(1), false),
            item("m/broken.pcdxil.vso", b"DXBC but not really a container..........".to_vec(), false),
        ];
        std::fs::write(media.join("_library").join("mats").join("mat.zip"), zip(&mats, true)).unwrap();
        std::fs::write(media.join("junk.zip"), b"not a zip").unwrap();

        let d = detect(&game).unwrap();
        assert_eq!((d.engine, d.api), ("ForzaTech", "DirectX 12"));
        assert_eq!(d.version.as_deref(), Some("pcdxil+RTS0"));
        assert_eq!(d.exe.as_deref(), Some("ForzaHorizon6.exe"));
        assert_eq!(d.files.len(), 1);

        let lines = std::cell::RefCell::new(Vec::new());
        let set = index(&game, &|l| lines.borrow_mut().push(l)).unwrap();
        // CS 1 (raz, choć w dwóch archiwach), VS 2, VS 10, PS 11, VS 12, PS 13, PS 14.
        assert_eq!(set.shaders.len(), 7);
        assert!(set.signatures.is_empty());
        let got = |i: usize| set.shaders[i].get().unwrap();
        let mut pipes: Vec<[Option<Vec<u8>>; 6]> = set
            .pipelines
            .iter()
            .map(|p| {
                assert_eq!(p.rs, None);
                p.stages.map(|s| s.map(got))
            })
            .collect();
        pipes.sort();
        let mut want_pipes = vec![
            [None, None, None, None, None, Some(cs(1))],
            [Some(vs(10)), Some(ps(11, "TEXCOORD")), None, None, None, None],
        ];
        want_pipes.sort();
        assert_eq!(pipes, want_pipes);
        let mut pools: Vec<Vec<Vec<u8>>> = set
            .pools
            .iter()
            .map(|(p, rs)| {
                assert_eq!(*rs, None);
                let mut v: Vec<Vec<u8>> = p.iter().map(|&i| got(i)).collect();
                v.sort();
                v
            })
            .collect();
        pools.sort();
        let mut mat_pool = vec![vs(12), ps(13, "COLOR"), ps(14, "TEXCOORD")];
        mat_pool.sort();
        let mut want_pools = vec![vec![vs(2)], mat_pool];
        want_pools.sort();
        assert_eq!(pools, want_pools);
        let text = lines.borrow().join("\n");
        assert!(text.contains("scan 3 3"), "{text}");
        assert!(set.notes.iter().any(|n| n.contains("ray tracing") && n.ends_with(": 1")));
        assert!(set.notes.iter().any(|n| n.contains("don't open") && n.contains("junk.zip")));
        assert!(set.notes.iter().any(|n| n.contains("doesn't feed") && n.ends_with(": 1")));

        // Łatka gry: zmieniony wpis nie jest zwracany.
        let patched = zip(&[item("shaders/a.cshader", cs(7), true), item("m/dup.cshader", cs(8), false)], false);
        std::fs::write(media.join("ShadersDXIL.zip"), &patched).unwrap();
        std::fs::write(media.join("_library").join("mats").join("mat.zip"), &patched).unwrap();
        assert!(set.shaders.iter().all(|s| s.get().is_none()));

        // Bez folderu _library - nie ForzaTech.
        std::fs::remove_dir_all(media.join("_library")).unwrap();
        assert!(detect(&game).is_none());
        let _ = std::fs::remove_dir_all(game);
    }
}
