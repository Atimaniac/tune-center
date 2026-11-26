//! Zouna (Asobo; A Plague Tale: Requiem): gra rozpoznawana po plikach
//! `All.psc` i `LangDef.tsc` w folderze instalacji. Tylko odczyt.
//!
//! - Shadery to surowe kontenery DXBC/DXIL w plikach gry, wycinane jak w
//!   `warm12` (pliki nazwane jak shadery w całości, pozostałe najpierw
//!   próbkami). Plik, w którym kontenery leżą rekordami (shadery jednego
//!   potoku tuż po sobie, do 32 B przerwy, każdy rekord to VS (+ PS, GS, HS,
//!   DS) albo sam CS), daje potoki dokładne; inny plik - pule (graficzna i
//!   compute osobno).
//! - Shadery nie niosą użytecznej root signature: silnik ma jedną compute i
//!   jedną graficzną (wersja 1.1, bez statycznych samplerów) bez względu na
//!   etapy potoku.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{self, read_at, Container, Kind};
use super::readers::{Bytes, Detected, Pipeline12, Set12};

const MARKERS: [&str; 2] = ["All.psc", "LangDef.tsc"];
const EXE: &str = "APlagueTaleRequiem_x64.exe";

/// Bajtów próbkowanych z plików bez nazwy shaderów (jak `warm12`).
const SAMPLE_BUDGET: u64 = 4 << 30;
const SAMPLE_WINDOW: u64 = 4 << 20;
const MAX_SHADERS: usize = 1 << 21;
/// Największa przerwa między shaderami jednego rekordu.
const MAX_RECORD_GAP: u64 = 32;
/// Plików na liście rozpoznania, gdy żaden nie ma nazwy shaderów.
const DETECT_FILES: usize = 32;

// ------------------------------ root signature -------------------------------

/// Wiersz: tablica z jednym zakresem `[0, widoczność, typ zakresu, liczba,
/// rejestr, przestrzeń, flagi zakresu, przesunięcie]` (przesunięcie
/// 0xFFFFFFFF = OFFSET_APPEND).
type Row = [u32; 8];
const APPEND: u32 = u32::MAX;

/// Compute: tablice 64 SRV, 14 CBV, 16 samplerów, 8 UAV i trzy po 256 SRV
/// (przestrzenie 42-44), widoczne dla wszystkich etapów.
const COMPUTE: [Row; 7] = [
    [0, 0, 0, 64, 0, 0, 5, APPEND],
    [0, 0, 2, 14, 0, 0, 5, APPEND],
    [0, 0, 3, 16, 0, 0, 1, APPEND],
    [0, 0, 1, 8, 0, 0, 3, APPEND],
    [0, 0, 0, 256, 0, 42, 5, APPEND],
    [0, 0, 0, 256, 0, 43, 5, APPEND],
    [0, 0, 0, 256, 0, 44, 5, APPEND],
];
const COMPUTE_FLAGS: u32 = 0;

/// Graficzna: na etap (PS 5, VS 1, GS 4, HS 2, DS 3) tablice 64 SRV, 14 CBV
/// i 16 samplerów; potem 8 UAV i trzy po 256 SRV dla wszystkich.
const GRAPHICS: [Row; 19] = [
    [0, 5, 0, 64, 0, 0, 5, APPEND],
    [0, 5, 2, 14, 0, 0, 5, APPEND],
    [0, 5, 3, 16, 0, 0, 1, APPEND],
    [0, 1, 0, 64, 0, 0, 5, APPEND],
    [0, 1, 2, 14, 0, 0, 5, APPEND],
    [0, 1, 3, 16, 0, 0, 1, APPEND],
    [0, 4, 0, 64, 0, 0, 5, APPEND],
    [0, 4, 2, 14, 0, 0, 5, APPEND],
    [0, 4, 3, 16, 0, 0, 1, APPEND],
    [0, 2, 0, 64, 0, 0, 5, APPEND],
    [0, 2, 2, 14, 0, 0, 5, APPEND],
    [0, 2, 3, 16, 0, 0, 1, APPEND],
    [0, 3, 0, 64, 0, 0, 5, APPEND],
    [0, 3, 2, 14, 0, 0, 5, APPEND],
    [0, 3, 3, 16, 0, 0, 1, APPEND],
    [0, 0, 1, 8, 0, 0, 3, APPEND],
    [0, 0, 0, 256, 0, 42, 5, APPEND],
    [0, 0, 0, 256, 0, 43, 5, APPEND],
    [0, 0, 0, 256, 0, 44, 5, APPEND],
];
/// Flagi graficznej: układ wejścia.
const GRAPHICS_FLAGS: u32 = 1;

/// Indeksy w `Set12::signatures`.
const RS_COMPUTE: usize = 0;
const RS_GRAPHICS: usize = 1;

/// Serializuje opis w wersji 1.1 (same tablice, bez statycznych samplerów).
#[cfg(windows)]
fn serialize(flags: u32, rows: &[Row]) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let ranges: Vec<D3D12_DESCRIPTOR_RANGE1> = rows
        .iter()
        .map(|r| D3D12_DESCRIPTOR_RANGE1 {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(r[2] as i32),
            NumDescriptors: r[3],
            BaseShaderRegister: r[4],
            RegisterSpace: r[5],
            Flags: D3D12_DESCRIPTOR_RANGE_FLAGS(r[6] as i32),
            OffsetInDescriptorsFromTableStart: r[7],
        })
        .collect();
    let params: Vec<D3D12_ROOT_PARAMETER1> = rows
        .iter()
        .zip(&ranges)
        .map(|(r, range)| D3D12_ROOT_PARAMETER1 {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE(r[0] as i32),
            Anonymous: D3D12_ROOT_PARAMETER1_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE1 { NumDescriptorRanges: 1, pDescriptorRanges: range },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY(r[1] as i32),
        })
        .collect();
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: params.len() as u32,
                pParameters: if params.is_empty() { std::ptr::null() } else { params.as_ptr() },
                NumStaticSamplers: 0,
                pStaticSamplers: std::ptr::null(),
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

/// Obie root signature silnika: [compute, graficzna].
#[cfg(windows)]
fn signatures() -> Result<Vec<Vec<u8>>, String> {
    Ok(vec![serialize(COMPUTE_FLAGS, &COMPUTE)?, serialize(GRAPHICS_FLAGS, &GRAPHICS)?])
}

#[cfg(not(windows))]
fn signatures() -> Result<Vec<Vec<u8>>, String> {
    Err("root signature tylko w Windows / root signatures need Windows".into())
}

// --------------------------------- rozpoznanie --------------------------------

fn is_zouna(game: &Path) -> bool {
    MARKERS.iter().all(|m| game.join(m).is_file())
}

/// Gra Zouna: oba pliki-znaczniki. Lista plików: nazwane jak shadery, a bez
/// nich największe kandydaty do wycinania.
pub fn detect(game: &Path) -> Option<Detected> {
    if !is_zouna(game) {
        return None;
    }
    let all = container::candidate_files(game);
    let named: Vec<(PathBuf, u64)> = all.iter().take_while(|(p, _)| container::named(p)).cloned().collect();
    let files = if named.is_empty() { all.into_iter().take(DETECT_FILES).collect() } else { named };
    Some(Detected {
        engine: "Zouna",
        version: None,
        api: "DirectX 12",
        files,
        exe: game.join(EXE).is_file().then(|| EXE.to_string()),
    })
}

// ----------------------------------- rekordy -----------------------------------

/// Kontener wycięty z pliku: położenie, rozmiar, etap (`None` - bez kodu) i
/// indeks w `Set12::shaders` (`None` - pominięty).
#[derive(Clone, Copy, Debug)]
struct Found {
    at: u64,
    size: u64,
    kind: Option<Kind>,
    id: Option<usize>,
}

/// Gniazdo etapu w `Pipeline12::stages`.
fn slot(k: Kind) -> Option<usize> {
    match k {
        Kind::Vertex => Some(0),
        Kind::Pixel => Some(1),
        Kind::Geometry => Some(2),
        Kind::Hull => Some(3),
        Kind::Domain => Some(4),
        Kind::Compute => Some(5),
        Kind::Other => None,
    }
}

/// Rekordy pliku: ciągi kontenerów leżących tuż po sobie (kontenery bez kodu
/// nie są etapami). Plik jest listą rekordów, gdy każdy ciąg to jeden potok
/// (różne etapy graficzne z VS albo sam CS) i choć jeden ma dwa etapy;
/// inaczej `None` (paczka shaderów).
fn records(seq: &[Found]) -> Option<Vec<Vec<Found>>> {
    let mut runs: Vec<Vec<Found>> = Vec::new();
    let mut end: Option<u64> = None;
    for f in seq {
        if end.is_none_or(|e| f.at.saturating_sub(e) > MAX_RECORD_GAP) {
            runs.push(Vec::new());
        }
        end = Some(f.at.saturating_add(f.size));
        if f.kind.is_some() {
            runs.last_mut()?.push(*f);
        }
    }
    runs.retain(|r| !r.is_empty());
    let pipeline = |r: &Vec<Found>| {
        let kinds: HashSet<Kind> = r.iter().filter_map(|f| f.kind).collect();
        let graphics = kinds.iter().all(|k| matches!(k, Kind::Vertex | Kind::Pixel | Kind::Geometry | Kind::Hull | Kind::Domain));
        kinds.len() == r.len() && ((graphics && kinds.contains(&Kind::Vertex)) || (r.len() == 1 && kinds.contains(&Kind::Compute)))
    };
    (!runs.is_empty() && runs.iter().all(pipeline) && runs.iter().any(|r| r.len() > 1)).then_some(runs)
}

// ------------------------------------ indeks ------------------------------------

/// Bajty shadera czytane na żądanie (sprawdzane tożsamością kontenera).
fn lazy(path: Arc<PathBuf>, at: u64, size: usize, id: [u8; 16]) -> Bytes {
    Bytes::Lazy(Arc::new(move || {
        let file = std::fs::File::open(path.as_ref()).ok()?;
        let mut b = vec![0u8; size];
        read_at(&file, at, &mut b).ok()?;
        let ok = Container::parse(&b).is_some_and(|c| c.bytes.len() == size && c.identity() == id);
        ok.then_some(b)
    }))
}

#[derive(Default)]
struct Counts {
    files: usize,
    records: usize,
    pooled: usize,
    other_stages: usize,
    over_cap: usize,
    unreadable: usize,
}

/// Shadery DirectX 12 gry: rekordy jako potoki dokładne, reszta w pulach
/// pliku, z root signature silnika.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    if !is_zouna(game) {
        return Err("brak All.psc i LangDef.tsc - to nie gra Zouna / no All.psc and LangDef.tsc - not a Zouna game".into());
    }
    let mut set = Set12 { signatures: signatures()?, ..Set12::default() };
    let files = container::candidate_files(game);
    let mut ids: HashMap<([u8; 16], usize), usize> = HashMap::new();
    let mut seen: HashSet<(usize, [Option<usize>; 6])> = HashSet::new();
    let mut n = Counts::default();
    let mut sampled = 0u64;
    let total = files.len();
    for (k, (path, len)) in files.iter().enumerate() {
        if k % 20 == 0 {
            say(format!("pliki / files: {k}/{total}"));
        }
        let Ok(file) = std::fs::File::open(path) else {
            n.unreadable += 1;
            continue;
        };
        // Plik bez nazwy shaderów: najpierw próbki, w całości tylko z trafieniem.
        if !container::named(path) {
            if sampled >= SAMPLE_BUDGET {
                continue;
            }
            let mut hit = false;
            for (start, len) in container::samples(*len, SAMPLE_WINDOW) {
                sampled += len;
                let _ = container::carve_file(&file, start, len, &mut |_, c| hit |= c.kind().is_some());
            }
            if !hit {
                continue;
            }
        }
        let shared = Arc::new(path.clone());
        let mut seq: Vec<Found> = Vec::new();
        let shaders = &mut set.shaders;
        let _ = container::carve_file(&file, 0, *len, &mut |at, c| {
            let kind = c.kind();
            let size = c.bytes.len();
            let id = match kind.and_then(slot) {
                Some(_) => {
                    let key = (c.identity(), size);
                    match ids.get(&key) {
                        Some(&i) => Some(i),
                        None if shaders.len() < MAX_SHADERS => {
                            shaders.push(lazy(shared.clone(), at, size, key.0));
                            ids.insert(key, shaders.len() - 1);
                            Some(shaders.len() - 1)
                        }
                        None => {
                            n.over_cap += 1;
                            None
                        }
                    }
                }
                None => {
                    if kind.is_some() {
                        n.other_stages += 1;
                    }
                    None
                }
            };
            seq.push(Found { at, size: size as u64, kind, id });
        });
        if seq.iter().all(|f| f.id.is_none()) {
            continue;
        }
        n.files += 1;
        if let Some(runs) = records(&seq) {
            for r in runs {
                n.records += 1;
                let mut stages = [None; 6];
                let mut whole = true;
                for f in &r {
                    match (f.kind.and_then(slot), f.id) {
                        (Some(s), Some(id)) => stages[s] = Some(id),
                        _ => whole = false,
                    }
                }
                let rs = if stages[5].is_some() { RS_COMPUTE } else { RS_GRAPHICS };
                if whole && seen.insert((rs, stages)) {
                    set.pipelines.push(Pipeline12 { rs: Some(rs), stages });
                }
            }
            continue;
        }
        // Paczka: pula graficzna (VS i PS) i pula compute.
        let (mut gfx, mut cs) = (Vec::new(), Vec::new());
        for f in &seq {
            match (f.kind, f.id) {
                (Some(Kind::Vertex | Kind::Pixel), Some(id)) => gfx.push(id),
                (Some(Kind::Compute), Some(id)) => cs.push(id),
                _ => continue,
            }
            n.pooled += 1;
        }
        for (mut pool, rs) in [(gfx, RS_GRAPHICS), (cs, RS_COMPUTE)] {
            pool.sort_unstable();
            pool.dedup();
            if !pool.is_empty() {
                set.pools.push((pool, Some(rs)));
            }
        }
    }
    say(format!("pliki / files: {total}/{total}"));
    if set.pipelines.is_empty() && set.pools.is_empty() {
        return Err("w plikach gry nie ma shaderów DirectX 12 / the game files hold no DirectX 12 shaders".into());
    }
    let notes = &mut set.notes;
    notes.push(format!(
        "{} shaderów w {} plikach: {} rekordów -> {} różnych potoków, {} w pulach / \
         {} shaders in {} files: {} records -> {} distinct pipelines, {} pooled",
        set.shaders.len(),
        n.files,
        n.records,
        set.pipelines.len(),
        n.pooled,
        set.shaders.len(),
        n.files,
        n.records,
        set.pipelines.len(),
        n.pooled
    ));
    let extra = [
        (n.other_stages, "shaderów innych etapów (mesh, biblioteki) pominiętych", "shaders of other stages (mesh, libraries) skipped"),
        (n.over_cap, "shaderów ponad limit", "shaders over the cap"),
        (n.unreadable, "plików nieczytelnych", "unreadable files"),
    ];
    for (count, pl, en) in extra {
        if count > 0 {
            notes.push(format!("{count} {pl} / {count} {en}"));
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

    /// Kontener DXIL etapu `kind` (0 PS, 1 VS, 5 CS), różny dla każdego `salt`.
    fn shader(kind: u32, salt: u8) -> Vec<u8> {
        build(&[(b"DXIL", dxil(kind)), (b"SALT", vec![salt; 4])])
    }

    fn found(at: u64, size: u64, kind: Kind) -> Found {
        Found { at, size, kind: Some(kind), id: Some(at as usize) }
    }

    #[test]
    fn tells_records_from_packs() {
        // VS+PS tuż po sobie, CS dalej: lista rekordów.
        let recs = records(&[found(0, 100, Kind::Vertex), found(108, 50, Kind::Pixel), found(400, 60, Kind::Compute)]).unwrap();
        assert_eq!(recs.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 1]);
        // Dwa VS w jednym ciągu - paczka.
        assert!(records(&[found(0, 100, Kind::Vertex), found(100, 100, Kind::Vertex)]).is_none());
        // Same pojedyncze shadery - paczka.
        assert!(records(&[found(0, 100, Kind::Vertex), found(500, 100, Kind::Pixel)]).is_none());
        // Samotny PS albo CS z PS - paczka.
        assert!(records(&[found(0, 10, Kind::Vertex), found(10, 10, Kind::Pixel), found(99, 10, Kind::Pixel)]).is_none());
        assert!(records(&[found(0, 10, Kind::Compute), found(10, 10, Kind::Pixel)]).is_none());
        assert!(records(&[]).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn serializes_both_root_signatures() {
        let s = signatures().unwrap();
        assert_eq!(s.len(), 2);
        let le32 = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        // Kontener z częścią RTS0: wersja 1.1, liczba parametrów, bez samplerów, flagi.
        for (blob, params, flags) in [(&s[RS_COMPUTE], COMPUTE.len(), COMPUTE_FLAGS), (&s[RS_GRAPHICS], GRAPHICS.len(), GRAPHICS_FLAGS)] {
            assert!(blob.len() > 24);
            let rts0 = Container::parse(blob).and_then(|c| c.root_signature()).unwrap();
            assert_eq!(le32(rts0, 0), 2);
            assert_eq!(le32(rts0, 4), params as u32);
            assert_eq!(le32(rts0, 12), 0);
            assert_eq!(le32(rts0, 20), flags);
        }
    }

    #[cfg(windows)]
    #[test]
    fn indexes_synthetic_game() {
        let game = std::env::temp_dir().join(format!("tc-zouna-{}", std::process::id()));
        let data = game.join("DATAS");
        std::fs::create_dir_all(&data).unwrap();
        assert!(detect(&game).is_none());
        std::fs::write(game.join("All.psc"), [1u8; 80]).unwrap();
        std::fs::write(game.join("LangDef.tsc"), [2u8; 80]).unwrap();
        std::fs::write(game.join(EXE), b"MZ").unwrap();

        // Paczka (bez nazwy shaderów): VS, PS, CS daleko od siebie.
        let (vs, ps, cs) = (shader(1, 1), shader(0, 2), shader(5, 3));
        let mut pack = vec![7u8; 300];
        for c in [&vs, &ps, &cs] {
            pack.extend_from_slice(c);
            pack.extend_from_slice(&[9u8; 200]);
        }
        std::fs::write(data.join("world.dpc"), &pack).unwrap();
        // Rekordy potoków: VS2+PS2 (8 B przerwy), CS2, VS+PS z paczki, powtórzenie.
        let (vs2, ps2, cs2) = (shader(1, 4), shader(0, 5), shader(5, 6));
        let mut recs = vec![0u8; 64];
        for r in [vec![&vs2, &ps2], vec![&cs2], vec![&vs, &ps], vec![&vs2, &ps2]] {
            for (k, c) in r.iter().enumerate() {
                if k > 0 {
                    recs.extend_from_slice(&[0u8; 8]);
                }
                recs.extend_from_slice(c);
            }
            recs.extend_from_slice(&[0xEEu8; 100]);
        }
        std::fs::write(data.join("pipelines.pso"), &recs).unwrap();

        let d = detect(&game).unwrap();
        assert_eq!((d.engine, d.api, d.exe.as_deref()), ("Zouna", "DirectX 12", Some(EXE)));
        assert!(d.files.iter().any(|(p, _)| p.ends_with("pipelines.pso")));

        let set = index(&game, &|_| {}).unwrap();
        assert_eq!(set.signatures.len(), 2);
        assert_eq!(set.shaders.len(), 6);
        assert_eq!(set.pipelines.len(), 3);
        let gfx: Vec<&Pipeline12> = set.pipelines.iter().filter(|p| p.rs == Some(RS_GRAPHICS)).collect();
        let comp: Vec<&Pipeline12> = set.pipelines.iter().filter(|p| p.rs == Some(RS_COMPUTE)).collect();
        assert_eq!((gfx.len(), comp.len()), (2, 1));
        assert!(comp[0].stages[5].is_some() && comp[0].stages[..5].iter().all(Option::is_none));
        assert_eq!(set.shaders[gfx[0].stages[0].unwrap()].get().unwrap(), vs2);
        assert_eq!(set.shaders[gfx[0].stages[1].unwrap()].get().unwrap(), ps2);
        // Pule paczki: VS+PS z graficzną, CS z compute; te same shadery co w rekordzie.
        assert_eq!(set.pools.len(), 2);
        let pool_gfx = set.pools.iter().find(|p| p.1 == Some(RS_GRAPHICS)).unwrap();
        let pool_cs = set.pools.iter().find(|p| p.1 == Some(RS_COMPUTE)).unwrap();
        assert_eq!(pool_gfx.0.len(), 2);
        assert_eq!(set.shaders[pool_cs.0[0]].get().unwrap(), cs);
        assert!(pool_gfx.0.contains(&gfx[1].stages[0].unwrap()));
        let _ = std::fs::remove_dir_all(&game);

        assert!(detect(Path::new("Z:\\nie-ma-takiej-gry")).is_none());
        assert!(index(Path::new("Z:\\nie-ma-takiej-gry"), &|_| {}).is_err());
    }
}
