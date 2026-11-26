//! Northlight (Remedy; Control w DirectX 12): pliki efektów w
//! `data\shaders\build\pc_dxil` (`*.obj`, bez kompresji). Tylko odczyt.
//!
//! - Plik efektu: magia "RFX ", potem shadery; każdy to u32 rozmiar, kontener
//!   DXBC (SM 5.1: VS, PS, CS) albo DXIL (biblioteki ray tracingu), u32
//!   długość i nazwa punktu wejścia oraz 8 zerowych bajtów. Shadery jednego
//!   potoku (rekordu) dzieli tylko 4 bajty zer więcej; inne bajty między
//!   shaderami to nagłówek przebiegu albo permutacji - nowy rekord.
//! - Każdy rekord VS (+ HS, DS) + PS albo sam CS to jeden potok; biblioteki
//!   ray tracingu są pomijane, rekordy niebędące potokiem idą do pul.
//!
//! Shadery nie mają wbudowanej root signature: silnik buduje w kodzie jedną
//! graficzną i jedną compute (wersja 1.0, zakresy kolejno - OFFSET_APPEND,
//! bez statycznych samplerów). Potoków z GS (ani MS, AS) nie ma.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{read_at, Container, Kind, SigElement};
use super::readers::{Bytes, Detected, Pipeline12, Set12};

pub const DIR: &str = r"data\shaders\build\pc_dxil";
const MAGIC: &[u8; 4] = b"RFX ";
const EXE: &str = "Control_DX12.exe";

/// Największy czytany plik efektu (Control: największy ma 53 MB).
const MAX_FILE: u64 = 512 << 20;
const MAX_FILES: usize = 20_000;
const MAX_SHADERS_PER_FILE: usize = 1 << 18;
const MAX_RECORDS: usize = 1 << 20;
/// Rozpoznanie: najmniejsze pliki efektów, każdy i razem ograniczone.
const DETECT_FILES: usize = 8;
const DETECT_FILE_MAX: u64 = 8 << 20;
const DETECT_BUDGET: u64 = 24 << 20;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

// ------------------------------ root signature -------------------------------

/// Wiersz opisu: tablica `[0, widoczność, (typ zakresu, liczba, rejestr,
/// przestrzeń, flagi)...]` albo główny CBV `[2, widoczność, rejestr,
/// przestrzeń, flagi]`. Wersja 1.0: flagi pomijane, każdy zakres pod
/// OFFSET_APPEND (jeden za drugim).
type Row = &'static [u32];

/// Graficzna: na etap (VS 1, PS 5, HS 2, DS 3) tablica 5 CBV (b4 dla VS i PS,
/// b0 dla HS i DS), 36 SRV, 8 UAV; potem tablice 4 samplerów; główne CBV
/// b0-b3 dla VS i PS; tablica 64 samplerów (przestrzeń 1) i 244000 SRV
/// (przestrzeń 1) dla PS.
const GRAPHICS: [Row; 18] = [
    &[0, 1, 2, 5, 4, 0, 0, 0, 36, 0, 0, 0, 1, 8, 0, 0, 0],
    &[0, 5, 2, 5, 4, 0, 0, 0, 36, 0, 0, 0, 1, 8, 0, 0, 0],
    &[0, 2, 2, 5, 0, 0, 0, 0, 36, 0, 0, 0, 1, 8, 0, 0, 0],
    &[0, 3, 2, 5, 0, 0, 0, 0, 36, 0, 0, 0, 1, 8, 0, 0, 0],
    &[0, 1, 3, 4, 0, 0, 0],
    &[0, 5, 3, 4, 0, 0, 0],
    &[0, 2, 3, 4, 0, 0, 0],
    &[0, 3, 3, 4, 0, 0, 0],
    &[2, 1, 0, 0, 0],
    &[2, 1, 1, 0, 0],
    &[2, 1, 2, 0, 0],
    &[2, 1, 3, 0, 0],
    &[2, 5, 0, 0, 0],
    &[2, 5, 1, 0, 0],
    &[2, 5, 2, 0, 0],
    &[2, 5, 3, 0, 0],
    &[0, 0, 3, 64, 0, 1, 0],
    &[0, 5, 0, 244000, 0, 1, 0],
];
/// Flagi graficznej: układ wejścia (0x1), bez GS (0x10).
const GRAPHICS_FLAGS: u32 = 17;

/// Compute: te same tablice i główne CBV raz, widoczne dla wszystkich etapów.
const COMPUTE: [Row; 8] = [
    &[0, 0, 2, 5, 4, 0, 0, 0, 36, 0, 0, 0, 1, 8, 0, 0, 0],
    &[0, 0, 3, 4, 0, 0, 0],
    &[2, 0, 0, 0, 0],
    &[2, 0, 1, 0, 0],
    &[2, 0, 2, 0, 0],
    &[2, 0, 3, 0, 0],
    &[0, 0, 3, 64, 0, 1, 0],
    &[0, 0, 0, 244000, 0, 1, 0],
];
const COMPUTE_FLAGS: u32 = 0;

/// Indeksy w `Set12::signatures`.
const RS_GRAPHICS: usize = 0;
const RS_COMPUTE: usize = 1;

/// Serializuje opis w wersji 1.0 (bez statycznych samplerów).
#[cfg(windows)]
fn serialize(flags: u32, rows: &[Row]) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let mut ranges: Vec<Vec<D3D12_DESCRIPTOR_RANGE>> = Vec::with_capacity(rows.len());
    for r in rows {
        let mut list = Vec::new();
        if r.first() == Some(&0) {
            for c in r.get(2..).unwrap_or(&[]).chunks(5) {
                if c.len() < 5 {
                    return Err("wiersz tablicy: niepełny zakres / table row: incomplete range".into());
                }
                list.push(D3D12_DESCRIPTOR_RANGE {
                    RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(c[0] as i32),
                    NumDescriptors: c[1],
                    BaseShaderRegister: c[2],
                    RegisterSpace: c[3],
                    OffsetInDescriptorsFromTableStart: u32::MAX,
                });
            }
        } else if r.len() < 4 {
            return Err("wiersz: za krótki / row: too short".into());
        }
        ranges.push(list);
    }
    let params: Vec<D3D12_ROOT_PARAMETER> = rows
        .iter()
        .zip(&ranges)
        .map(|(r, range)| D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE(r[0] as i32),
            Anonymous: if r[0] == 0 {
                D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: range.len() as u32,
                        pDescriptorRanges: range.as_ptr(),
                    },
                }
            } else {
                D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: r[2], RegisterSpace: r[3] } }
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY(r[1] as i32),
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

/// Obie root signature silnika: [graficzna, compute].
#[cfg(windows)]
fn signatures() -> Result<Vec<Vec<u8>>, String> {
    Ok(vec![serialize(GRAPHICS_FLAGS, &GRAPHICS)?, serialize(COMPUTE_FLAGS, &COMPUTE)?])
}

#[cfg(not(windows))]
fn signatures() -> Result<Vec<Vec<u8>>, String> {
    Err("root signature tylko w Windows / root signatures need Windows".into())
}

// --------------------------------- rekordy ------------------------------------

/// Shader w pliku efektu: położenie kontenera, rozmiar i etap (`None` - bez
/// kodu programu).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shader {
    at: usize,
    size: usize,
    kind: Option<Kind>,
}

/// Bajty między dwoma shaderami jednego rekordu: u32 długość, nazwa
/// (litery, cyfry, `_`), 12 zer, u32 rozmiar następnego.
fn entry_point(gap: &[u8]) -> bool {
    let Some(n) = le32(gap, 0) else { return false };
    let n = n as usize;
    if n < 1 || gap.len() < 21 || gap.len() != n.saturating_add(20) {
        return false;
    }
    gap[4..4 + n].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && gap[4 + n..16 + n].iter().all(|&c| c == 0)
}

/// Rekordy pliku efektu. `None`: nie plik efektu, kontener bez rozmiaru
/// przed nim albo za dużo shaderów (inny układ).
fn records(b: &[u8]) -> Option<Vec<Vec<Shader>>> {
    if b.len() < 8 || &b[..4] != MAGIC {
        return None;
    }
    let mut recs: Vec<Vec<Shader>> = Vec::new();
    let mut end: Option<usize> = None;
    let mut n = 0usize;
    let mut i = 4;
    while i + 4 <= b.len() {
        let Some(p) = b[i..].windows(4).position(|w| w == b"DXBC") else { break };
        let at = i + p;
        let Some(c) = Container::parse(&b[at..]) else {
            i = at + 1;
            continue;
        };
        n += 1;
        if n > MAX_SHADERS_PER_FILE || le32(b, at - 4) != Some(c.bytes.len() as u32) {
            return None;
        }
        let shader = Shader { at, size: c.bytes.len(), kind: c.kind() };
        match end {
            Some(e) if at >= e && entry_point(&b[e..at]) => {
                if let Some(last) = recs.last_mut() {
                    last.push(shader);
                }
            }
            _ => recs.push(vec![shader]),
        }
        end = Some(at + shader.size);
        i = at + shader.size;
    }
    Some(recs)
}

fn graphics(k: Kind) -> bool {
    matches!(k, Kind::Pixel | Kind::Vertex | Kind::Geometry | Kind::Hull | Kind::Domain)
}

/// Rekord jest potokiem: różne etapy graficzne z VS albo sam CS.
fn is_pipeline(r: &[Shader]) -> bool {
    let kinds: Vec<Option<Kind>> = r.iter().map(|s| s.kind).collect();
    let distinct = kinds.iter().collect::<HashSet<_>>().len() == kinds.len();
    distinct
        && (kinds.iter().all(|k| k.is_some_and(graphics)) && kinds.contains(&Some(Kind::Vertex))
            || kinds == [Some(Kind::Compute)])
}

/// Wartości systemowe wejścia, które zapisuje poprzedni etap (clip/cull
/// distance, indeks celu i viewportu).
const FROM_PREV_STAGE: [u32; 4] = [2, 3, 4, 5];

/// Czy `src` zasila `dst`: każde wejście czytane z poprzedniego etapu jest
/// w jego wyjściach (maska się mieści) i to w tym samym rejestrze (DXBC
/// łączy po rejestrach). `None`: `src` zapisuje jedno wyjście dwa razy.
fn feeds(src: &Container, dst: &Container) -> Option<bool> {
    if src.is_dxil() != dst.is_dxil() {
        return Some(false); // potok nie miesza DXBC i DXIL
    }
    let mut outs: HashMap<(String, u32), SigElement> = HashMap::new();
    for o in src.outputs() {
        if outs.insert((o.name.clone(), o.index), o).is_some() {
            return None;
        }
    }
    for i in dst.inputs() {
        let o = outs.get(&(i.name.clone(), i.index));
        if let Some(o) = o {
            if o.register != i.register {
                return Some(false);
            }
        }
        if i.sysval != 0 && !FROM_PREV_STAGE.contains(&i.sysval) {
            continue;
        }
        match o {
            Some(o) if i.mask & !o.mask == 0 => {}
            _ => return Some(false),
        }
    }
    Some(true)
}

/// Etapy rekordu (VS, PS, GS, HS, DS, CS) jako indeksy shaderów rekordu.
fn slots(r: &[Shader]) -> [Option<usize>; 6] {
    let mut s = [None; 6];
    for (k, sh) in r.iter().enumerate() {
        let slot = match sh.kind {
            Some(Kind::Vertex) => 0,
            Some(Kind::Pixel) => 1,
            Some(Kind::Geometry) => 2,
            Some(Kind::Hull) => 3,
            Some(Kind::Domain) => 4,
            Some(Kind::Compute) => 5,
            _ => continue,
        };
        s[slot] = Some(k);
    }
    s
}

// ---------------------------------- pliki -------------------------------------

fn effects_dir(game: &Path) -> PathBuf {
    game.join(DIR)
}

/// Pliki efektów (`*.obj` w samym folderze), po nazwie bez wielkości liter.
fn effects(game: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(dir) = std::fs::read_dir(effects_dir(game)) else { return Vec::new() };
    let mut out: Vec<(PathBuf, u64)> = dir
        .flatten()
        .take(MAX_FILES * 4)
        .filter_map(|e| {
            let p = e.path();
            let obj = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("obj"));
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            obj.then_some((p, meta.len()))
        })
        .take(MAX_FILES)
        .collect();
    out.sort_by_key(|(p, _)| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    out
}

/// Northlight 2 (archiwa `.rmdtoc`: Alan Wake 2) buduje root signature
/// inaczej - nie ten czytnik. Szukane do 3 poziomów, ograniczone.
fn is_northlight2(game: &Path) -> bool {
    let mut stack = vec![(game.to_path_buf(), 0)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(list) = std::fs::read_dir(&dir) else { continue };
        for e in list.flatten() {
            seen += 1;
            if seen > 50_000 {
                return false;
            }
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() && depth < 3 {
                stack.push((p, depth + 1));
            } else if t.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rmdtoc")) {
                return true;
            }
        }
    }
    false
}

/// Cały plik, gdy zaczyna się magią efektu i ma najwyżej `max` bajtów.
fn read_effect(path: &Path, max: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < 8 || len > max {
        return None;
    }
    let mut magic = [0u8; 4];
    read_at(&file, 0, &mut magic).ok()?;
    if &magic != MAGIC {
        return None;
    }
    let mut b = vec![0u8; len as usize];
    read_at(&file, 0, &mut b).ok()?;
    Some(b)
}

/// Gra Northlight z efektami DirectX 12: jeden z najmniejszych plików ma
/// rekord VS+PS.
pub fn detect(game: &Path) -> Option<Detected> {
    let files = effects(game);
    if files.is_empty() || is_northlight2(game) {
        return None;
    }
    let mut small = files.clone();
    small.sort_by_key(|f| f.1);
    let mut budget = DETECT_BUDGET;
    let found = small.iter().take(DETECT_FILES).any(|(p, len)| {
        if *len > DETECT_FILE_MAX || *len > budget {
            return false;
        }
        budget -= len;
        let Some(b) = read_effect(p, DETECT_FILE_MAX) else { return false };
        records(&b).is_some_and(|recs| {
            recs.iter().any(|r| {
                let mut k: Vec<Option<Kind>> = r.iter().map(|s| s.kind).collect();
                k.sort();
                k == [Some(Kind::Pixel), Some(Kind::Vertex)]
            })
        })
    });
    found.then(|| Detected {
        engine: "Northlight",
        version: None,
        api: "DirectX 12",
        files,
        exe: game.join(EXE).is_file().then(|| EXE.to_string()),
    })
}

// --------------------------------- indeks -------------------------------------

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
    records: usize,
    libraries: usize,
    pooled: usize,
    unlinked: usize,
    malformed: usize,
    no_signature: usize,
    over_cap: usize,
    skipped_files: usize,
}

/// Shadery DirectX 12 gry: każdy różny rekord-potok z root signature silnika.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let files = effects(game);
    if files.is_empty() {
        return Err(format!("{DIR}: brak plików efektów / no effect files"));
    }
    let mut set = Set12 { signatures: signatures()?, ..Set12::default() };
    let mut ids: HashMap<([u8; 16], usize), usize> = HashMap::new();
    let mut seen: HashSet<[Option<usize>; 6]> = HashSet::new();
    let mut n = Counts::default();
    let mut visited = 0usize;
    for (k, (path, _)) in files.iter().enumerate() {
        if k % 64 == 0 {
            say(format!("pliki efektów / effect files: {k}/{}", files.len()));
        }
        let Some(b) = read_effect(path, MAX_FILE) else {
            n.skipped_files += 1;
            continue;
        };
        let Some(recs) = records(&b) else {
            n.skipped_files += 1;
            continue;
        };
        let shared = Arc::new(path.clone());
        let mut pool_gfx = Vec::new();
        let mut pool_cs = Vec::new();
        for r in &recs {
            visited += 1;
            if visited > MAX_RECORDS {
                n.over_cap += 1;
                continue;
            }
            let parsed: Vec<Container> = r.iter().filter_map(|s| Container::parse(&b[s.at..s.at + s.size])).collect();
            if parsed.len() != r.len() {
                continue;
            }
            // Biblioteka ray tracingu (DXIL, nie etap potoku) - pomijana.
            if r.iter().all(|s| s.kind == Some(Kind::Other)) && parsed.iter().all(|c| c.is_dxil()) {
                n.libraries += 1;
                continue;
            }
            let mut add = |s: &Shader, c: &Container| -> usize {
                *ids.entry((c.identity(), s.size)).or_insert_with(|| {
                    set.shaders.push(lazy(shared.clone(), s.at as u64, s.size, c.identity()));
                    set.shaders.len() - 1
                })
            };
            if !is_pipeline(r) {
                for (s, c) in r.iter().zip(&parsed) {
                    match s.kind {
                        Some(Kind::Vertex | Kind::Pixel) => pool_gfx.push(add(s, c)),
                        Some(Kind::Compute) => pool_cs.push(add(s, c)),
                        _ => continue,
                    }
                    n.pooled += 1;
                }
                continue;
            }
            let at = slots(r);
            let [vs, ps, gs, hs, ds, cs] = at;
            // Root signature silnika nie ma potoków z GS; HS tylko z DS.
            if gs.is_some() || hs.is_some() != ds.is_some() {
                n.no_signature += 1;
                continue;
            }
            if let (Some(p), Some(src)) = (ps, ds.or(vs)) {
                match feeds(&parsed[src], &parsed[p]) {
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
            n.records += 1;
            let mut stages = [None; 6];
            for (slot, idx) in at.iter().enumerate() {
                if let Some(i) = idx {
                    stages[slot] = Some(add(&r[*i], &parsed[*i]));
                }
            }
            if seen.insert(stages) {
                let rs = if cs.is_some() { RS_COMPUTE } else { RS_GRAPHICS };
                set.pipelines.push(Pipeline12 { rs: Some(rs), stages });
            }
        }
        for (mut pool, rs) in [(pool_gfx, RS_GRAPHICS), (pool_cs, RS_COMPUTE)] {
            pool.sort_unstable();
            pool.dedup();
            if !pool.is_empty() {
                set.pools.push((pool, Some(rs)));
            }
        }
    }
    if set.pipelines.is_empty() {
        return Err(format!("{DIR}: brak rekordów potoków / no pipeline records"));
    }
    let notes = &mut set.notes;
    notes.push(format!(
        "{} shaderów, {} rekordów potoków -> {} różnych potoków, {} bibliotek ray tracingu pominiętych / \
         {} shaders, {} pipeline records -> {} distinct pipelines, {} ray tracing libraries skipped",
        set.shaders.len(),
        n.records,
        set.pipelines.len(),
        n.libraries,
        set.shaders.len(),
        n.records,
        set.pipelines.len(),
        n.libraries
    ));
    let extra = [
        (n.unlinked, "VS nie zasila PS", "VS doesn't feed PS"),
        (n.malformed, "VS zapisuje wyjście dwa razy", "a VS writes one output twice"),
        (n.no_signature, "GS albo HS bez DS (brak root signature)", "GS or HS without DS (no root signature)"),
        (n.over_cap, "ponad limit rekordów", "over the record cap"),
    ];
    for (count, pl, en) in extra {
        if count > 0 {
            notes.push(format!("{count} rekordów pominiętych: {pl} / {count} records left out: {en}"));
        }
    }
    if n.pooled > 0 {
        notes.push(format!(
            "{} shaderów z rekordów niebędących potokiem - w pulach / {} shaders of non-pipeline records pooled",
            n.pooled, n.pooled
        ));
    }
    if n.skipped_files > 0 {
        notes.push(format!(
            "{} plików pominiętych (za duże albo inny układ) / {} files skipped (too large or another layout)",
            n.skipped_files, n.skipped_files
        ));
    }
    for line in &set.notes {
        say(line.clone());
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::{build, sig1};
    use super::*;

    /// Kod DXBC SM 5.1 danego typu (reszta nieczytana).
    fn shex(kind: u32, salt: u8) -> Vec<u8> {
        let mut b = ((kind << 16) | 0x51).to_le_bytes().to_vec();
        b.extend_from_slice(&[salt; 12]);
        b
    }

    fn vs(salt: u8, tex_reg: u32) -> Vec<u8> {
        build(&[
            (b"ISG1", sig1(&[("POSITION", 0, 0, 3, 0, 0x7)])),
            (b"OSG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, tex_reg, 0x3)])),
            (b"SHEX", shex(1, salt)),
        ])
    }

    fn ps(salt: u8) -> Vec<u8> {
        build(&[
            (b"ISG1", sig1(&[("SV_Position", 0, 1, 3, 0, 0xF), ("TEXCOORD", 0, 0, 3, 1, 0x3)])),
            (b"OSG1", sig1(&[("SV_Target", 0, 64, 3, 0, 0xF)])),
            (b"SHEX", shex(0, salt)),
        ])
    }

    fn cs(salt: u8) -> Vec<u8> {
        build(&[(b"SHEX", shex(5, salt))])
    }

    /// Plik efektu: każdy rekord po nagłówku przebiegu, shadery z punktami wejścia.
    fn effect(records: &[Vec<Vec<u8>>]) -> Vec<u8> {
        let mut f = b"RFX ".to_vec();
        f.extend_from_slice(&[1, 0, 0, 0]);
        for r in records {
            f.extend_from_slice(b"pass header\x07\x00");
            for (k, c) in r.iter().enumerate() {
                if k > 0 {
                    f.extend_from_slice(&[0u8; 4]);
                }
                f.extend_from_slice(&(c.len() as u32).to_le_bytes());
                f.extend_from_slice(c);
                let name = format!("main_{k}");
                f.extend_from_slice(&(name.len() as u32).to_le_bytes());
                f.extend_from_slice(name.as_bytes());
                f.extend_from_slice(&[0u8; 8]);
            }
        }
        f
    }

    #[test]
    fn splits_records_by_entry_points() {
        let f = effect(&[vec![vs(1, 1), ps(2)], vec![cs(3)], vec![ps(4)]]);
        let recs = records(&f).unwrap();
        let kinds: Vec<Vec<Option<Kind>>> = recs.iter().map(|r| r.iter().map(|s| s.kind).collect()).collect();
        assert_eq!(
            kinds,
            vec![vec![Some(Kind::Vertex), Some(Kind::Pixel)], vec![Some(Kind::Compute)], vec![Some(Kind::Pixel)]]
        );
        assert!(is_pipeline(&recs[0]) && is_pipeline(&recs[1]) && !is_pipeline(&recs[2]));
        // Kontener bez rozmiaru przed nim - inny układ.
        let mut bad = b"RFX \0\0\0\0".to_vec();
        bad.extend_from_slice(&cs(1));
        assert!(records(&bad).is_none());
        assert!(records(b"XXXX1234").is_none());
        // Ucięty plik nie panikuje.
        for cut in (0..f.len()).step_by(7) {
            let _ = records(&f[..cut]);
        }
    }

    #[test]
    fn checks_vs_feeds_ps_by_register() {
        let (good, bad, p) = (vs(1, 1), vs(2, 2), ps(3));
        let p = Container::parse(&p).unwrap();
        assert_eq!(feeds(&Container::parse(&good).unwrap(), &p), Some(true));
        assert_eq!(feeds(&Container::parse(&bad).unwrap(), &p), Some(false));
        let twice = build(&[
            (b"OSG1", sig1(&[("TEXCOORD", 0, 0, 3, 1, 0x3), ("TEXCOORD", 0, 0, 3, 2, 0x3)])),
            (b"SHEX", shex(1, 9)),
        ]);
        assert_eq!(feeds(&Container::parse(&twice).unwrap(), &p), None);
    }

    #[cfg(windows)]
    #[test]
    fn serializes_both_root_signatures() {
        let s = signatures().unwrap();
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|b| b.len() > 24));
        // Kontener z częścią RTS0: wersja 1.0, liczba parametrów, bez samplerów, flagi.
        for (blob, params, flags) in [(&s[RS_GRAPHICS], GRAPHICS.len(), GRAPHICS_FLAGS), (&s[RS_COMPUTE], COMPUTE.len(), COMPUTE_FLAGS)] {
            let rts0 = Container::parse(blob).and_then(|c| c.root_signature()).unwrap();
            assert_eq!(le32(rts0, 0), Some(1));
            assert_eq!(le32(rts0, 4), Some(params as u32));
            assert_eq!(le32(rts0, 12), Some(0));
            assert_eq!(le32(rts0, 20), Some(flags));
        }
    }

    #[cfg(windows)]
    #[test]
    fn indexes_synthetic_game() {
        let game = std::env::temp_dir().join(format!("tc-northlight-{}", std::process::id()));
        let dir = game.join(DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let a = effect(&[vec![vs(1, 1), ps(2)], vec![cs(3)], vec![vs(1, 1), ps(2)], vec![vs(4, 2), ps(2)], vec![ps(5)]]);
        std::fs::write(dir.join("a.obj"), &a).unwrap();
        std::fs::write(dir.join("b.obj"), effect(&[vec![cs(3)], vec![vs(6, 1), ps(7)]])).unwrap();
        std::fs::write(dir.join("readme.txt"), b"x").unwrap();
        std::fs::write(game.join(EXE), b"MZ").unwrap();

        let d = detect(&game).unwrap();
        assert_eq!((d.engine, d.api, d.exe.as_deref()), ("Northlight", "DirectX 12", Some(EXE)));
        assert_eq!(d.files.len(), 2);

        let set = index(&game, &|_| {}).unwrap();
        assert_eq!(set.signatures.len(), 2);
        // VS+PS (raz mimo powtórzenia), CS (raz z dwóch plików), drugi VS+PS z b.obj.
        assert_eq!(set.pipelines.len(), 3);
        let gfx: Vec<&Pipeline12> = set.pipelines.iter().filter(|p| p.rs == Some(RS_GRAPHICS)).collect();
        let comp: Vec<&Pipeline12> = set.pipelines.iter().filter(|p| p.rs == Some(RS_COMPUTE)).collect();
        assert_eq!((gfx.len(), comp.len()), (2, 1));
        assert!(comp[0].stages[5].is_some() && comp[0].stages[..5].iter().all(|s| s.is_none()));
        let vs_bytes = set.shaders[gfx[0].stages[0].unwrap()].get().unwrap();
        assert_eq!(vs_bytes, vs(1, 1));
        // Samotny PS w puli graficznej; rekord z VS w złym rejestrze pominięty.
        assert_eq!(set.pools, vec![(vec![set.pools[0].0[0]], Some(RS_GRAPHICS))]);
        assert!(set.notes.iter().any(|n| n.contains("VS doesn't feed PS")));
        let _ = std::fs::remove_dir_all(&game);

        assert!(detect(Path::new("Z:\\nie-ma-takiej-gry")).is_none());
    }
}
