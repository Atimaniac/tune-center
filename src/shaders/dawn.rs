//! Dawn (Eidos-Montréal; Marvel's Guardians of the Galaxy): magazyny potoków
//! DirectX 12 w `bin\`. Tylko odczyt.
//!
//! - Każdy plik `.store2`: u64 a, u64 b (rozmiar danych = długość pliku - 24),
//!   dane, na końcu znów u64 a. Dane zaczyna u32 n.
//! - `rawshader.store2` (shadery) i `rawroot.store2` (root signature jako
//!   kontenery z samą częścią RTS0): n wpisów u32 id, u32 rozmiar, u32 0 i
//!   kontener; wpisy wypełniają dane dokładnie.
//! - `rawpso.store2`: n rekordów po 680 B, każdy to jeden potok: +8 u32 rozmiar
//!   rekordu (664), +16 id root signature, +24 id VS albo CS, +40 id PS (0 -
//!   brak), dalej stan potoku. Pod +56, +72, +88, +104 leżałyby GS, HS, DS -
//!   takie rekordy są pomijane (gra ich nie ma).
//!
//! Rekord z id, którego nie ma w magazynie, albo wpisy niewypełniające danych
//! - inny układ.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{read_at, Container, Kind};
use super::readers::{Bytes, Detected, Pipeline12, Set12};

const SHADERS: &str = "rawshader.store2";
const ROOTS: &str = "rawroot.store2";
const PSOS: &str = "rawpso.store2";
const EXE: &str = "gotg.exe";

const HEADER: u64 = 16;
/// Nagłówek i stopka pliku.
const FRAMING: u64 = HEADER + 8;
/// Początek wpisów (po u32 n).
const FIRST: u64 = HEADER + 4;
const RECORD: u64 = 680;
const RECORD_PAYLOAD: u32 = 0x298;
const OTHER_STAGES: [usize; 4] = [56, 72, 88, 104];
/// Największy plik rekordów (gra: 8,3 MB).
const MAX_PSO_FILE: u64 = 256 << 20;
const MAX_ENTRIES: u32 = 1 << 21;
const MAX_CONTAINER: u32 = 16 << 20;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

fn store_path(game: &Path, name: &str) -> PathBuf {
    game.join("bin").join(name)
}

// --------------------------------- magazyny -----------------------------------

/// Liczba n z początku danych, gdy ramka pliku się zgadza.
fn frame(file: &std::fs::File, len: u64) -> Option<u32> {
    if len < FRAMING + 4 {
        return None;
    }
    let (mut head, mut tail) = ([0u8; FIRST as usize], [0u8; 8]);
    read_at(file, 0, &mut head).ok()?;
    read_at(file, len - 8, &mut tail).ok()?;
    if le64(&head, 8)? != len - FRAMING || le64(&tail, 0)? != le64(&head, 0)? {
        return None;
    }
    le32(&head, HEADER as usize)
}

fn framed(path: &Path) -> Option<(std::fs::File, u64, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let n = frame(&file, len)?;
    Some((file, len, n))
}

/// Wpis magazynu: położenie kontenera i zapisany rozmiar.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    at: u64,
    size: u32,
}

/// Wpisy magazynu po id; `None`, gdy nie wypełniają danych dokładnie.
fn entries(path: &Path) -> Option<HashMap<u32, Entry>> {
    let (file, len, n) = framed(path)?;
    if n > MAX_ENTRIES {
        return None;
    }
    let end = len - 8;
    let mut r = BufReader::with_capacity(1 << 16, file);
    std::io::Seek::seek(&mut r, std::io::SeekFrom::Start(FIRST)).ok()?;
    let mut out = HashMap::with_capacity(n as usize);
    let mut at = FIRST;
    let mut head = [0u8; 12];
    for _ in 0..n {
        if at + 12 > end {
            return None;
        }
        r.read_exact(&mut head).ok()?;
        let (id, size, zero) = (le32(&head, 0)?, le32(&head, 4)?, le32(&head, 8)?);
        at += 12;
        if zero != 0 || u64::from(size) > end - at || size > MAX_CONTAINER || out.insert(id, Entry { at, size }).is_some() {
            return None;
        }
        r.seek_relative(i64::from(size)).ok()?;
        at += u64::from(size);
    }
    (at == end).then_some(out)
}

/// Rekord potoku: id root signature, VS albo CS, PS (0 - brak) i czy
/// nazywa inny etap (GS, HS, DS).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Record {
    root: u32,
    first: u32,
    ps: u32,
    other: bool,
}

/// Rekordy z `rawpso.store2`; `None` - inny układ albo id spoza magazynów.
fn pso_records(path: &Path, shaders: &HashMap<u32, Entry>, roots: &HashSet<u32>) -> Option<Vec<Record>> {
    let (file, len, n) = framed(path)?;
    if len > MAX_PSO_FILE || len - FRAMING - 4 != u64::from(n) * RECORD {
        return None;
    }
    let mut b = vec![0u8; len as usize];
    read_at(&file, 0, &mut b).ok()?;
    let mut out = Vec::with_capacity(n as usize);
    for k in 0..n as usize {
        let at = FIRST as usize + k * RECORD as usize;
        let (root, first, ps) = (le32(&b, at + 16)?, le32(&b, at + 24)?, le32(&b, at + 40)?);
        if le32(&b, at + 8)? != RECORD_PAYLOAD || !roots.contains(&root) || !shaders.contains_key(&first) || (ps != 0 && !shaders.contains_key(&ps)) {
            return None;
        }
        let mut other = false;
        for o in OTHER_STAGES {
            other |= shaders.contains_key(&le32(&b, at + o)?);
        }
        out.push(Record { root, first, ps, other });
    }
    Some(out)
}

// --------------------------------- rozpoznanie --------------------------------

/// Gra Dawn: trzy magazyny w `bin\` z poprawną ramką (czyta po 28 B z pliku).
pub fn detect(game: &Path) -> Option<Detected> {
    let mut files = Vec::new();
    for name in [PSOS, ROOTS, SHADERS] {
        let path = store_path(game, name);
        let (_, len, _) = framed(&path)?;
        files.push((path, len));
    }
    let exe = [game.join(EXE), game.join("bin").join(EXE)].iter().any(|p| p.is_file()).then(|| EXE.to_string());
    Some(Detected { engine: "Dawn", version: None, api: "DirectX 12", files, exe })
}

// ------------------------------------ indeks ------------------------------------

/// Nagłówek kontenera we wpisie: rozmiar (nie większy niż wpis) i skrót.
fn header(file: &std::fs::File, e: Entry) -> Option<(usize, [u8; 16])> {
    let mut h = [0u8; 32];
    if e.size < 32 {
        return None;
    }
    read_at(file, e.at, &mut h).ok()?;
    let total = le32(&h, 24)?;
    (h.get(..4)? == b"DXBC" && (40..=e.size).contains(&total)).then(|| (total as usize, h[4..20].try_into().unwrap_or([0; 16])))
}

/// Cały kontener ze wpisu.
fn whole(file: &std::fs::File, at: u64, size: usize) -> Option<Vec<u8>> {
    let mut b = vec![0u8; size];
    read_at(file, at, &mut b).ok()?;
    Container::parse(&b).is_some_and(|c| c.bytes.len() == size).then_some(b)
}

/// Bajty shadera czytane na żądanie (sprawdzane skrótem z nagłówka).
fn lazy(path: Arc<PathBuf>, at: u64, size: usize, digest: [u8; 16]) -> Bytes {
    Bytes::Lazy(Arc::new(move || {
        let file = std::fs::File::open(path.as_ref()).ok()?;
        whole(&file, at, size).filter(|b| b.get(4..20) == Some(&digest[..]))
    }))
}

/// Potoki DirectX 12 gry: każdy różny rekord z root signature, którą nazywa.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let layout = |name: &str| format!(r"bin\{name}: brak albo nieznany układ / missing or unknown layout");
    let shader_path = store_path(game, SHADERS);
    let root_path = store_path(game, ROOTS);
    say("magazyny / stores: 0/3".into());
    let shaders = entries(&shader_path).ok_or_else(|| layout(SHADERS))?;
    say("magazyny / stores: 1/3".into());
    let root_at = entries(&root_path).ok_or_else(|| layout(ROOTS))?;

    // Root signature: kontenery bez kodu z częścią RTS0.
    let mut set = Set12::default();
    let mut rs_of: HashMap<u32, usize> = HashMap::new();
    let root_file = std::fs::File::open(&root_path).map_err(|e| format!(r"bin\{ROOTS}: {e}"))?;
    let mut root_ids: Vec<u32> = root_at.keys().copied().collect();
    root_ids.sort_unstable();
    for id in root_ids {
        let e = root_at[&id];
        let blob = header(&root_file, e)
            .and_then(|(size, _)| whole(&root_file, e.at, size))
            .filter(|b| Container::parse(b).is_some_and(|c| c.kind().is_none() && c.root_signature().is_some()))
            .ok_or_else(|| layout(ROOTS))?;
        rs_of.insert(id, set.signatures.len());
        set.signatures.push(blob);
    }
    say("magazyny / stores: 2/3".into());
    let roots: HashSet<u32> = rs_of.keys().copied().collect();
    let records = pso_records(&store_path(game, PSOS), &shaders, &roots).ok_or_else(|| layout(PSOS))?;
    say("magazyny / stores: 3/3".into());

    let shader_file = std::fs::File::open(&shader_path).map_err(|e| format!(r"bin\{SHADERS}: {e}"))?;
    let shared = Arc::new(shader_path);
    // id magazynu -> (indeks w `set.shaders`, rozmiar kontenera); `None` - nieczytelny.
    let mut ids: HashMap<u32, Option<(usize, usize)>> = HashMap::new();
    let mut add = |id: u32, set: &mut Set12| -> Option<(usize, usize)> {
        *ids.entry(id).or_insert_with(|| {
            let e = *shaders.get(&id)?;
            let (size, digest) = header(&shader_file, e)?;
            set.shaders.push(lazy(shared.clone(), e.at, size, digest));
            Some((set.shaders.len() - 1, size))
        })
    };
    // Etap shadera bez PS (VS albo CS) - z całego kontenera.
    let mut kinds: HashMap<usize, Option<Kind>> = HashMap::new();
    let mut seen: HashSet<(usize, [Option<usize>; 6])> = HashSet::new();
    let (mut other, mut unreadable, mut unknown_stage) = (0usize, 0usize, 0usize);
    for r in &records {
        if r.other {
            other += 1;
            continue;
        }
        let rs = rs_of[&r.root];
        let Some((first, size)) = add(r.first, &mut set) else {
            unreadable += 1;
            continue;
        };
        let mut stages = [None; 6];
        if r.ps != 0 {
            let Some((ps, _)) = add(r.ps, &mut set) else {
                unreadable += 1;
                continue;
            };
            stages[0] = Some(first);
            stages[1] = Some(ps);
        } else {
            let at = shaders[&r.first].at;
            let kind = *kinds
                .entry(first)
                .or_insert_with(|| whole(&shader_file, at, size).and_then(|b| Container::parse(&b).and_then(|c| c.kind())));
            match kind {
                Some(Kind::Vertex) => stages[0] = Some(first),
                Some(Kind::Compute) => stages[5] = Some(first),
                _ => {
                    unknown_stage += 1;
                    continue;
                }
            }
        }
        if seen.insert((rs, stages)) {
            set.pipelines.push(Pipeline12 { rs: Some(rs), stages });
        }
    }
    if set.pipelines.is_empty() {
        return Err(format!(r"bin\{PSOS}: brak rekordów potoków / no pipeline records"));
    }
    let notes = &mut set.notes;
    notes.push(format!(
        "{} rekordów -> {} różnych potoków, {} shaderów, {} root signature / \
         {} records -> {} distinct pipelines, {} shaders, {} root signatures",
        records.len(),
        set.pipelines.len(),
        set.shaders.len(),
        set.signatures.len(),
        records.len(),
        set.pipelines.len(),
        set.shaders.len(),
        set.signatures.len()
    ));
    let extra = [
        (other, "z GS, HS albo DS", "with a GS, HS or DS"),
        (unreadable, "z nieczytelnym shaderem", "with an unreadable shader"),
        (unknown_stage, "bez PS z shaderem innym niż VS i CS", "without a PS whose shader is neither VS nor CS"),
    ];
    for (count, pl, en) in extra {
        if count > 0 {
            notes.push(format!("{count} rekordów pominiętych: {pl} / {count} records left out: {en}"));
        }
    }
    for line in &set.notes {
        say(line.clone());
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::{build, dxil};
    use super::*;

    fn shader(kind: u32, salt: u8) -> Vec<u8> {
        build(&[(b"DXIL", dxil(kind)), (b"SALT", vec![salt; 4])])
    }

    fn root(salt: u8) -> Vec<u8> {
        let mut rts0 = vec![2u8, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0];
        rts0.extend_from_slice(&[salt, 0, 0, 0]);
        build(&[(b"RTS0", rts0)])
    }

    /// Plik `.store2` z danymi `payload`.
    fn framed_file(payload: &[u8]) -> Vec<u8> {
        let a = payload.len() as u64 + 0x10007;
        let mut f = a.to_le_bytes().to_vec();
        f.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        f.extend_from_slice(payload);
        f.extend_from_slice(&a.to_le_bytes());
        f
    }

    fn store(list: &[(u32, Vec<u8>)]) -> Vec<u8> {
        framed_file(&entries_payload(list))
    }

    fn entries_payload(list: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut p = (list.len() as u32).to_le_bytes().to_vec();
        for (id, c) in list {
            p.extend_from_slice(&id.to_le_bytes());
            p.extend_from_slice(&(c.len() as u32).to_le_bytes());
            p.extend_from_slice(&0u32.to_le_bytes());
            p.extend_from_slice(c);
        }
        p
    }

    /// Rekordy (root, VS/CS, PS, id pod +72 - HS).
    fn psos(list: &[(u32, u32, u32, u32)]) -> Vec<u8> {
        let mut p = (list.len() as u32).to_le_bytes().to_vec();
        for (root, first, ps, hs) in list {
            let mut r = vec![0u8; RECORD as usize];
            r[8..12].copy_from_slice(&RECORD_PAYLOAD.to_le_bytes());
            r[16..20].copy_from_slice(&root.to_le_bytes());
            r[24..28].copy_from_slice(&first.to_le_bytes());
            r[40..44].copy_from_slice(&ps.to_le_bytes());
            r[72..76].copy_from_slice(&hs.to_le_bytes());
            p.extend_from_slice(&r);
        }
        framed_file(&p)
    }

    #[test]
    fn indexes_synthetic_stores() {
        let game = std::env::temp_dir().join(format!("tc-dawn-{}", std::process::id()));
        let bin = game.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert!(detect(&game).is_none());
        let (vs, ps, cs, vs2) = (shader(1, 1), shader(0, 2), shader(5, 3), shader(1, 4));
        std::fs::write(bin.join(SHADERS), store(&[(10, vs.clone()), (11, ps.clone()), (12, cs.clone()), (13, vs2.clone())])).unwrap();
        std::fs::write(bin.join(ROOTS), store(&[(1, root(1)), (2, root(2))])).unwrap();
        let records = [(1, 10, 11, 0), (2, 12, 0, 0), (1, 13, 0, 0), (1, 10, 11, 0), (1, 10, 11, 13), (2, 10, 11, 0)];
        std::fs::write(bin.join(PSOS), psos(&records)).unwrap();
        std::fs::write(game.join(EXE), b"MZ").unwrap();

        let d = detect(&game).unwrap();
        assert_eq!((d.engine, d.api, d.exe.as_deref()), ("Dawn", "DirectX 12", Some(EXE)));
        assert_eq!(d.files.len(), 3);

        let set = index(&game, &|_| {}).unwrap();
        assert_eq!(set.signatures, vec![root(1), root(2)]);
        // VS+PS (raz mimo powtórzenia), CS, sam VS, VS+PS z drugą root signature; rekord z HS pominięty.
        let want = [
            (0, [Some(0), Some(1), None, None, None, None]),
            (1, [None, None, None, None, None, Some(2)]),
            (0, [Some(3), None, None, None, None, None]),
            (1, [Some(0), Some(1), None, None, None, None]),
        ];
        let got: Vec<(usize, [Option<usize>; 6])> = set.pipelines.iter().map(|p| (p.rs.unwrap(), p.stages)).collect();
        assert_eq!(got, want);
        assert!(set.pools.is_empty());
        let bytes: Vec<Vec<u8>> = set.shaders.iter().map(|s| s.get().unwrap()).collect();
        assert_eq!(bytes, vec![vs.clone(), ps.clone(), cs.clone(), vs2.clone()]);
        assert!(set.notes.iter().any(|n| n.contains("with a GS, HS or DS")));

        // Rekord z id spoza magazynu - inny układ.
        std::fs::write(bin.join(PSOS), psos(&[(1, 99, 0, 0)])).unwrap();
        assert!(index(&game, &|_| {}).is_err());
        // Wpisy niewypełniające danych i ucięty plik.
        let mut bad = entries_payload(&[(10, vs.clone())]);
        bad.extend_from_slice(&[0u8; 4]);
        std::fs::write(bin.join(SHADERS), framed_file(&bad)).unwrap();
        assert!(framed(&bin.join(SHADERS)).is_some());
        assert!(entries(&bin.join(SHADERS)).is_none());
        let good = store(&[(10, vs.clone()), (11, ps.clone())]);
        for cut in (0..good.len()).step_by(5) {
            std::fs::write(bin.join(SHADERS), &good[..cut]).unwrap();
            assert!(entries(&bin.join(SHADERS)).is_none());
        }
        assert!(detect(&game).is_none());
        let _ = std::fs::remove_dir_all(&game);
    }
}
