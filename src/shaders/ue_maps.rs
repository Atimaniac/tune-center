//! Unreal Engine, DirectX 12 bez pamięci potoków: shadery SM6 z bibliotek
//! kodu shaderów gry (`ShaderArchive-*-PCD3D_SM6.ushaderbytecode` w paczkach
//! .pak albo biblioteki w kontenerach IoStore), pogrupowane w mapy shaderów
//! (shadery jednego materiału / typu, rysowane razem).
//!
//! - W mapie każdy shader pikseli dostaje shadery wierzchołków o tej samej
//!   sygnaturze łączenia (semantyka, indeks, wiersz, typ elementów bez
//!   wartości systemowych; maski pokryte), do `CANDIDATES` kandydatów;
//!   biblioteka globalna to jedna wspólna mapa. Compute osobno, shader
//!   wierzchołków bez pary osobno (przebieg samej głębi).
//! - Root signature jak RHI D3D12 danej wersji silnika (`Rule`): tabele SRV,
//!   samplerów i UAV na etap w kolejności priorytetu, potem główne CBV,
//!   dodatki 5.x (AGS, stałe, NVIDIA, bufor diagnostyczny), flagi i zakazy
//!   etapów bez powiązań. Jedna zserializowana root signature na różny opis.
//! - Mesh, amplification, GS/HS/DS i ray tracing - pomijane.
//!
//! Formaty jak w `unreal.rs` / `unreal_pak.rs` / `ue12.rs` (opisy publiczne,
//! kod własny). Gry są tylko czytane.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::Container;
use super::readers::{Bytes, Pipeline12, Set12};
use super::unreal::{Toc, CHUNK_SHADER_CODE, CHUNK_SHADER_LIBRARY};

/// Skrót shadera (FSHAHash; 8-bajtowe z UE 5.8 dopełnione zerami).
type Hash = [u8; 20];

/// Najwięcej potoków jednej gry.
const MAX_PIPELINES: usize = 250_000;
/// Najwięcej shaderów wierzchołków na shader pikseli.
const CANDIDATES: usize = 4;
/// `OffsetInDescriptorsFromTableStart`: zaraz za poprzednim zakresem.
const APPEND: u32 = u32::MAX;
/// Nieograniczony zakres deskryptorów.
const UNBOUNDED: u32 = u32::MAX;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

// ------------------------------ wersja silnika -------------------------------

/// Wersja silnika (główna, poboczna) z tekstu „++UE5+Release-5.N” w bajtach
/// (ASCII albo UTF-16).
fn version_in(b: &[u8]) -> Option<(u32, u32)> {
    for (tag, step) in [(&b"++UE"[..], 1usize), (&b"+\0+\0U\0E\0"[..], 2)] {
        let mut from = 0;
        while let Some(p) = find(b.get(from..)?, tag) {
            let at = from + p + tag.len();
            let rest: Vec<u8> = b[at..].iter().step_by(step).take(16).copied().collect();
            if let Some(v) = parse_release(&rest) {
                return Some(v);
            }
            from = at;
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut at = 0;
    while at + needle.len() <= hay.len() {
        let p = hay[at..hay.len() + 1 - needle.len()].iter().position(|&c| c == first)?;
        if hay[at + p..].starts_with(needle) {
            return Some(at + p);
        }
        at += p + 1;
    }
    None
}

/// „5+Release-5.1” → (5, 1).
fn parse_release(s: &[u8]) -> Option<(u32, u32)> {
    let major = *s.first()?;
    if !(b'4'..=b'6').contains(&major) {
        return None;
    }
    let rest = s.get(1..)?.strip_prefix(b"+Release-")?;
    let rest = rest.strip_prefix(&[major, b'.'])?;
    let digits: String = rest.iter().take(2).take_while(|c| c.is_ascii_digit()).map(|&c| c as char).collect();
    Some(((major - b'0') as u32, digits.parse().ok()?))
}

/// Wersja silnika z pliku .exe (czytanego kawałkami z zakładką).
fn exe_version(exe: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    const CHUNK: usize = 8 << 20;
    const OVERLAP: usize = 64;
    let mut f = std::fs::File::open(exe).ok()?;
    let mut buf = vec![0u8; CHUNK + OVERLAP];
    let mut keep = 0;
    loop {
        let mut n = keep;
        while n < buf.len() {
            match f.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(_) => return None,
            }
        }
        if let Some(v) = version_in(&buf[..n]) {
            return Some(v);
        }
        if n < buf.len() {
            return None;
        }
        buf.copy_within(n - OVERLAP..n, 0);
        keep = OVERLAP;
    }
}

/// Pliki .exe forków silnika ze zmienioną root signature (inny układ niż
/// wydanie, z którego wyszły) - bez zasady.
const FORKS: [&str; 3] = ["b1-win64-shipping.exe", "ff7rebirth_.exe", "client-win64-shipping.exe"];

/// Wersja silnika gry (z jej pliku `<Projekt>\Binaries\Win64\*-Shipping.exe`).
fn game_version(game: &Path) -> Option<(u32, u32)> {
    let exe = super::warm::unreal_game_exe(game)?;
    if FORKS.contains(&exe.to_lowercase().as_str()) {
        return None;
    }
    std::fs::read_dir(game)
        .ok()?
        .flatten()
        .map(|e| e.path().join("Binaries").join("Win64").join(&exe))
        .find(|p| p.is_file())
        .and_then(|p| exe_version(&p))
}

// ------------------------------- zasady RHI ----------------------------------

/// Etap potoku UE.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum St {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Mesh,
    Amplification,
    Compute,
}

impl St {
    /// D3D12_SHADER_VISIBILITY.
    fn vis(self) -> u32 {
        match self {
            St::Vertex => 1,
            St::Hull => 2,
            St::Domain => 3,
            St::Geometry => 4,
            St::Pixel => 5,
            St::Amplification => 6,
            St::Mesh => 7,
            St::Compute => 0,
        }
    }
    /// D3D12_ROOT_SIGNATURE_FLAG_DENY_*_SHADER_ROOT_ACCESS.
    fn deny(self) -> u32 {
        match self {
            St::Vertex => 0x2,
            St::Hull => 0x4,
            St::Domain => 0x8,
            St::Geometry => 0x10,
            St::Pixel => 0x20,
            St::Amplification => 0x100,
            St::Mesh => 0x200,
            St::Compute => 0,
        }
    }
}

/// Jak RHI D3D12 danej wersji UE buduje root signature (FD3D12RootSignatureDesc
/// przy wiązaniu zasobów tier 3).
#[derive(Debug)]
struct Rule {
    id: &'static str,
    /// Etapy graficzne w kolejności priorytetu (też: które można zakazać).
    stages: [St; 5],
    max_srvs: u32,
    max_samplers: u32,
    /// Etapy z tabelą UAV.
    uav_stages: &'static [St],
    /// Flaga układu wejścia tylko, gdy VS czyta wejście wierzchołków (do 4.21).
    vertex_input_layout: bool,
    /// MAX_SRVS większe, gdy shadery gry wiążą więcej (48, potem 64).
    grow_srvs: bool,
    /// Tabele tylko, gdy etap wiąże zasoby tego typu (od 5.1).
    tables_when_used: bool,
    /// Stałe główne b0 space 3 (4 wartości), od 5.4.
    root_constants: bool,
    /// UAV rozszerzeń AMD (AGS).
    ags: bool,
    /// UAV bufora diagnostycznego u0 space 999.
    diagnostic: bool,
    /// Flagi bezpośredniego indeksowania stert (bindless).
    heap_flags: bool,
    /// Tabela rozszerzeń NVIDIA u0 space 1001 (5.8).
    nv_extension: bool,
    /// Tabele nieograniczonych SRV w przestrzeniach 4..9 (fork 4.27).
    space_bindless: bool,
    /// Tabele nieograniczonych SRV w przestrzeniach 10g..10g+4 (fork 4.26).
    bindless: bool,
    /// 128 SRV, gdy shadery mają SRV w przestrzeniach 4..9.
    avalanche_srvs: bool,
    /// Statyczne samplery UE (rejestr pierwszego, przestrzeń); `None` - brak.
    samplers: Option<(u32, u32)>,
}

const UE4_STAGES: [St; 5] = [St::Pixel, St::Vertex, St::Geometry, St::Hull, St::Domain];
const UE5_STAGES: [St; 5] = [St::Pixel, St::Vertex, St::Geometry, St::Mesh, St::Amplification];

const UE420: Rule = Rule {
    id: "ue-4.20",
    stages: UE4_STAGES,
    max_srvs: 32,
    max_samplers: 16,
    uav_stages: &[St::Pixel, St::Compute],
    vertex_input_layout: true,
    grow_srvs: true,
    tables_when_used: false,
    root_constants: false,
    ags: false,
    diagnostic: false,
    heap_flags: false,
    nv_extension: false,
    space_bindless: true,
    bindless: true,
    avalanche_srvs: false,
    samplers: None,
};
const UE421: Rule = Rule { id: "ue-4.21", max_srvs: 48, ..UE420 };
const UE422: Rule = Rule { id: "ue-4.22", vertex_input_layout: false, ..UE421 };
const UE425: Rule = Rule { id: "ue-4.25", max_srvs: 64, grow_srvs: false, samplers: Some((1000, 0)), ..UE422 };
const UE426: Rule = Rule { id: "ue-4.26", avalanche_srvs: true, samplers: Some((0, 1000)), ..UE425 };
const UE50: Rule = Rule {
    id: "ue-5.0",
    stages: UE5_STAGES,
    max_srvs: 64,
    max_samplers: 16,
    uav_stages: &[St::Pixel, St::Compute],
    vertex_input_layout: false,
    grow_srvs: false,
    tables_when_used: false,
    root_constants: false,
    ags: true,
    diagnostic: true,
    heap_flags: true,
    nv_extension: false,
    space_bindless: false,
    bindless: false,
    avalanche_srvs: false,
    samplers: Some((0, 1000)),
};
const UE51: Rule = Rule { id: "ue-5.1", tables_when_used: true, ..UE50 };
const UE54: Rule = Rule { id: "ue-5.4", max_samplers: 32, root_constants: true, ..UE51 };
const UE55: Rule = Rule { id: "ue-5.5", uav_stages: &[St::Pixel, St::Compute, St::Vertex], ..UE54 };
const UE58: Rule = Rule {
    id: "ue-5.8",
    uav_stages: &[St::Pixel, St::Compute, St::Vertex, St::Mesh, St::Amplification],
    nv_extension: true,
    ..UE55
};

/// Zasada wersji silnika; `None` - wersja bez znanej zasady.
fn rule_for(major: u32, minor: u32) -> Option<&'static Rule> {
    Some(match (major, minor) {
        (4, 20) => &UE420,
        (4, 21) => &UE421,
        (4, 22..=24) => &UE422,
        (4, 25) => &UE425,
        (4, 26..=27) => &UE426,
        (5, 0) => &UE50,
        (5, 1..=3) => &UE51,
        (5, 4) => &UE54,
        (5, 5..=7) => &UE55,
        (5, 8..) | (6, 0) => &UE58,
        _ => return None,
    })
}

// --------------------------- opis root signature -----------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Range {
    /// D3D12_DESCRIPTOR_RANGE_TYPE: 0 SRV, 1 UAV, 2 CBV, 3 sampler.
    kind: u32,
    count: u32,
    base: u32,
    space: u32,
    /// D3D12_DESCRIPTOR_RANGE_FLAGS.
    flags: u32,
    offset: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Param {
    Table { vis: u32, ranges: Vec<Range> },
    /// Główny deskryptor: typ parametru 2 CBV / 4 UAV.
    Root { kind: u32, vis: u32, register: u32, space: u32, flags: u32 },
    Constants { vis: u32, register: u32, space: u32, count: u32 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Desc {
    flags: u32,
    params: Vec<Param>,
}

fn table(vis: u32, kind: u32, count: u32, space: u32, flags: u32) -> Param {
    Param::Table { vis, ranges: vec![Range { kind, count, base: 0, space, flags, offset: APPEND }] }
}

/// Liczniki zasobów etapu ('p') i flagi UE5 (`F_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Counts {
    samplers: u8,
    srvs: u8,
    cbs: u8,
    uavs: u8,
    flags: u8,
}

const F_BINDLESS_RESOURCES: u8 = 1;
const F_BINDLESS_SAMPLERS: u8 = 2;
const F_ROOT_CONSTANTS: u8 = 4;
const F_DIAGNOSTIC: u8 = 8;
const F_AMD: u8 = 16;
const F_NV: u8 = 32;

/// Shader gry potrzebny do potoku.
#[derive(Clone, Debug, Default)]
struct Shader {
    stage: Option<St>,
    counts: Counts,
    /// Sygnatura łączenia (wyjścia VS / wejścia PS bez wartości systemowych).
    key: u32,
    /// Pełna lista elementów (wyjścia VS / wejścia PS): indeks w `elems`.
    elems: u32,
    /// VS czyta wejście wierzchołków (element bez wartości systemowej).
    vertex_input: bool,
    /// VS zapisuje SV_Position (albo sygnatura nieznana).
    position: bool,
    /// Najwyższa przestrzeń 4..9 nieograniczonych SRV (0 - brak).
    space_top: u8,
    /// Grupy g przestrzeni 10g.. nieograniczonych SRV (bity).
    groups: u32,
    /// Wiąże u0 space 1001 (rozszerzenia NVIDIA).
    nv_u0: bool,
}

/// Ustawienia root signature całej gry.
#[derive(Clone, Copy, Debug, Default)]
struct Ctx {
    /// RHI na poziomie SM6 z kartą obsługującą mesh shadery: zakazy AS/MS.
    mesh_tier: bool,
    /// MAX_SRVS gry, gdy inne niż w zasadzie (0 - z zasady).
    max_srvs: u32,
    /// Tabela NVIDIA w każdej root signature (shadery wiążą u0 space 1001
    /// bez flagi rozszerzenia).
    nv_everywhere: bool,
}

/// Root signature potoku z etapów (compute sam; graficzne w dowolnej kolejności).
fn build(rule: &Rule, stages: &[(St, &Shader)], ctx: &Ctx) -> Desc {
    let order: Vec<(St, &Shader)> = match stages.iter().find(|(s, _)| *s == St::Compute) {
        Some(cs) => vec![*cs],
        None => rule.stages.iter().filter_map(|s| stages.iter().find(|(x, _)| x == s).copied()).collect(),
    };
    let max_srvs = if ctx.max_srvs > 0 { ctx.max_srvs } else { rule.max_srvs };
    // InitShaderRegisterCounts: (SRV, sampler, UAV, CB).
    let quantize = |s: St, c: &Counts| -> [u32; 4] {
        let uav_stage = rule.uav_stages.contains(&s);
        let cb = (c.cbs as u32).min(16);
        if rule.tables_when_used {
            [
                if c.srvs > 0 { max_srvs } else { 0 },
                if c.samplers > 0 { rule.max_samplers } else { 0 },
                if c.uavs > 0 && uav_stage { 16 } else { 0 },
                cb,
            ]
        } else {
            [max_srvs, rule.max_samplers, if uav_stage { 16 } else { 0 }, cb]
        }
    };
    let q: Vec<[u32; 4]> = order.iter().map(|(s, sh)| quantize(*s, &sh.counts)).collect();
    let mut d = Desc::default();
    for ((s, sh), q) in order.iter().zip(&q) {
        let vis = s.vis();
        if q[0] > 0 {
            d.params.push(table(vis, 0, q[0], 0, 5));
        }
        if rule.space_bindless {
            for space in 4..=sh.space_top as u32 {
                d.params.push(table(vis, 0, UNBOUNDED, space, 5));
            }
        }
        if q[1] > 0 {
            d.params.push(table(vis, 3, q[1], 0, 1));
        }
        if q[2] > 0 {
            d.params.push(table(vis, 1, q[2], 0, 3));
        }
        if rule.bindless {
            for g in (0..32u32).filter(|g| sh.groups & (1 << g) != 0) {
                let ranges = (0..5)
                    .map(|k| Range { kind: 0, count: UNBOUNDED, base: 0, space: 10 * g + k, flags: 5, offset: 0 })
                    .collect();
                d.params.push(Param::Table { vis, ranges });
            }
        }
    }
    for ((s, _), q) in order.iter().zip(&q) {
        for register in 0..q[3] {
            d.params.push(Param::Root { kind: 2, vis: s.vis(), register, space: 0, flags: 8 });
        }
    }
    let used = order.iter().fold(0u8, |f, (_, sh)| f | sh.counts.flags);
    if let Some((_, vs)) = order.iter().find(|(s, _)| *s == St::Vertex) {
        if !rule.vertex_input_layout || vs.vertex_input {
            d.flags |= 0x1; // ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
        }
    }
    if rule.ags && used & F_AMD != 0 {
        d.params.push(Param::Root { kind: 4, vis: 0, register: 0, space: 0x7FFF_0ADE, flags: 2 });
    }
    if rule.root_constants && used & F_ROOT_CONSTANTS != 0 {
        d.params.push(Param::Constants { vis: 0, register: 0, space: 3, count: 4 });
    }
    if (rule.nv_extension && used & F_NV != 0) || ctx.nv_everywhere {
        d.params.push(Param::Table {
            vis: 0,
            ranges: vec![Range { kind: 1, count: 1, base: 0, space: 1001, flags: 0, offset: 0 }],
        });
    }
    if rule.diagnostic && used & F_DIAGNOSTIC != 0 {
        d.params.push(Param::Root { kind: 4, vis: 0, register: 0, space: 999, flags: 2 });
    }
    if rule.heap_flags && used & F_BINDLESS_RESOURCES != 0 {
        d.flags |= 0x400;
    }
    if rule.heap_flags && used & F_BINDLESS_SAMPLERS != 0 {
        d.flags |= 0x800;
    }
    // Zakaz dla etapów nieobecnych albo bez powiązań (compute: wszystkie).
    for s in rule.stages {
        let empty = order.iter().zip(&q).find(|((x, _), _)| *x == s).is_none_or(|(_, q)| *q == [0; 4]);
        if empty && (ctx.mesh_tier || !matches!(s, St::Mesh | St::Amplification)) {
            d.flags |= s.deny();
        }
    }
    d
}

/// Serializuje opis w wersji 1.1 ze statycznymi samplerami UE (punktowy,
/// dwuliniowy, trójliniowy × zawijanie, obcinanie).
#[cfg(windows)]
fn serialize(d: &Desc, samplers: Option<(u32, u32)>) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let ranges: Vec<Vec<D3D12_DESCRIPTOR_RANGE1>> = d
        .params
        .iter()
        .map(|p| match p {
            Param::Table { ranges, .. } => ranges
                .iter()
                .map(|r| D3D12_DESCRIPTOR_RANGE1 {
                    RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(r.kind as i32),
                    NumDescriptors: r.count,
                    BaseShaderRegister: r.base,
                    RegisterSpace: r.space,
                    Flags: D3D12_DESCRIPTOR_RANGE_FLAGS(r.flags as i32),
                    OffsetInDescriptorsFromTableStart: r.offset,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let params: Vec<D3D12_ROOT_PARAMETER1> = d
        .params
        .iter()
        .zip(&ranges)
        .map(|(p, r)| match p {
            Param::Table { vis, .. } => D3D12_ROOT_PARAMETER1 {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER1_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE1 {
                        NumDescriptorRanges: r.len() as u32,
                        pDescriptorRanges: r.as_ptr(),
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
            Param::Root { kind, vis, register, space, flags } => D3D12_ROOT_PARAMETER1 {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE(*kind as i32),
                Anonymous: D3D12_ROOT_PARAMETER1_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                        ShaderRegister: *register,
                        RegisterSpace: *space,
                        Flags: D3D12_ROOT_DESCRIPTOR_FLAGS(*flags as i32),
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
            Param::Constants { vis, register, space, count } => D3D12_ROOT_PARAMETER1 {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER1_0 {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: *register,
                        RegisterSpace: *space,
                        Num32BitValues: *count,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
        })
        .collect();
    let statics: Vec<D3D12_STATIC_SAMPLER_DESC> = match samplers {
        None => Vec::new(),
        Some((register, space)) => [(0, 1), (0, 3), (0x14, 1), (0x14, 3), (0x15, 1), (0x15, 3)]
            .iter()
            .enumerate()
            .map(|(i, &(filter, address))| D3D12_STATIC_SAMPLER_DESC {
                Filter: D3D12_FILTER(filter),
                AddressU: D3D12_TEXTURE_ADDRESS_MODE(address),
                AddressV: D3D12_TEXTURE_ADDRESS_MODE(address),
                AddressW: D3D12_TEXTURE_ADDRESS_MODE(address),
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
                BorderColor: D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
                ShaderRegister: register + i as u32,
                RegisterSpace: space,
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            })
            .collect(),
    };
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: params.len() as u32,
                pParameters: if params.is_empty() { std::ptr::null() } else { params.as_ptr() },
                NumStaticSamplers: statics.len() as u32,
                pStaticSamplers: if statics.is_empty() { std::ptr::null() } else { statics.as_ptr() },
                Flags: D3D12_ROOT_SIGNATURE_FLAGS(d.flags as i32),
            },
        },
    };
    let mut blob: Option<ID3DBlob> = None;
    let mut error: Option<ID3DBlob> = None;
    // SAFETY: wskaźniki w `desc` prowadzą do żyjących wektorów (`ranges`,
    // `params`, `statics`), niezmienianych do końca wywołania.
    let result = unsafe { D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut error)) };
    let bytes = |b: &ID3DBlob| -> Vec<u8> {
        // SAFETY: bufor bloba D3D12 o podanym rozmiarze.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec() }
    };
    match (result, blob) {
        (Ok(()), Some(b)) => Ok(bytes(&b)),
        (r, _) => Err(error
            .map(|e| String::from_utf8_lossy(&bytes(&e)).trim_end_matches('\0').to_owned())
            .unwrap_or_else(|| format!("{r:?}"))),
    }
}

#[cfg(not(windows))]
fn serialize(_d: &Desc, _samplers: Option<(u32, u32)>) -> Result<Vec<u8>, String> {
    Err("root signature tylko w Windows / root signatures need Windows".into())
}

// ----------------------------- spakowany shader ------------------------------

/// Kontener DXBC/DXIL: pierwsza magia „DXBC”, której rozmiar mieści się w danych.
fn container_of(b: &[u8]) -> Option<&[u8]> {
    let mut from = 0;
    loop {
        let at = from + find(b.get(from..)?, b"DXBC")?;
        let size = le32(b, at + 24)? as usize;
        if size >= 32 && at.checked_add(size).is_some_and(|e| e <= b.len()) {
            return Some(&b[at..at + size]);
        }
        from = at + 4;
    }
}

/// Dane dodatkowe UE za kontenerem: [klucz u8][rozmiar u32][dane]..., na końcu
/// u32 długość całości.
fn trailer(b: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let Some(total) = b.len().checked_sub(4).and_then(|e| le32(b, e)).map(|t| t as usize) else {
        return out;
    };
    if total < 4 || total > b.len() {
        return out;
    }
    let end = b.len() - 4;
    let mut at = b.len() - total;
    while at + 5 <= end {
        let Some(n) = le32(b, at + 1).map(|n| n as usize) else { break };
        let Some(data) = at.checked_add(5 + n).filter(|&e| e <= end).map(|e| &b[at + 5..e]) else { break };
        out.push((b[at], data));
        at += 5 + n;
    }
    out
}

/// Liczniki zasobów ('p': flagi użycia, samplery, SRV, CB, UAV) i flagi UE5
/// (flagi użycia od 5.1, cechy 'x' od 5.0, rozszerzenia producentów 'v').
/// UE4: bajt 0 to co innego - bez flag.
fn counts(b: &[u8], ue5: bool, ue58: bool) -> Counts {
    let t = trailer(b);
    let get = |k: u8| t.iter().find(|(key, _)| *key == k).map(|(_, d)| *d);
    let Some(p) = get(b'p').filter(|p| p.len() >= 5) else { return Counts::default() };
    let mut c = Counts { samplers: p[1], srvs: p[2], cbs: p[3], uavs: p[4], flags: 0 };
    if !ue5 {
        return c;
    }
    let usage = p[0];
    let x = get(b'x').map_or(0u16, |x| x[0] as u16 | (x.get(1).copied().unwrap_or(0) as u16) << 8);
    if usage & 2 != 0 || x & 0x20 != 0 {
        c.flags |= F_BINDLESS_RESOURCES;
    }
    if usage & 4 != 0 || x & 0x40 != 0 {
        c.flags |= F_BINDLESS_SAMPLERS;
    }
    if usage & 8 != 0 {
        c.flags |= F_ROOT_CONSTANTS;
    }
    if usage & 0x40 != 0 || x & 0x10 != 0 {
        c.flags |= F_DIAGNOSTIC;
    }
    // 'v': liczba i32, potem wpisy po 15 B zaczynające się od producenta u32.
    if let Some(v) = get(b'v') {
        let n = le32(v, 0).unwrap_or(0) as usize;
        for i in 0..n.min(v.len() / 15) {
            match le32(v, 4 + 15 * i) {
                Some(0x1002) => c.flags |= F_AMD,
                Some(0x10DE) if ue58 => c.flags |= F_NV,
                _ => {}
            }
        }
    }
    c
}

/// Element sygnatury: nazwa (małe litery), indeks, wartość systemowa, typ
/// składowych, wiersz, maska i składowe użyte (wejście: czytane, wyjście:
/// zapisywane).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Elem {
    name: String,
    index: u32,
    sysval: u32,
    comp: u32,
    register: u32,
    mask: u8,
    used: u8,
}

fn signature(c: &Container, names: &[&[u8; 4]], output: bool) -> Vec<Elem> {
    let Some((fourcc, d)) = names.iter().find_map(|n| Some((*n, c.part(n)?))) else {
        return Vec::new();
    };
    // ISGN/OSGN 24 B; OSG5 28 B i ISG1/OSG1 32 B zaczynają się od strumienia.
    let (size, lead) = match fourcc {
        b"ISGN" | b"OSGN" => (24, 0),
        b"OSG5" => (28, 4),
        _ => (32, 4),
    };
    let (Some(count), Some(table)) = (le32(d, 0), le32(d, 4)) else { return Vec::new() };
    let mut out = Vec::new();
    for e in 0..(count as usize).min(1024) {
        let o = table as usize + e * size + lead;
        let (Some(name), Some(index), Some(sysval), Some(comp), Some(register), Some(&mask), Some(&rw)) = (
            le32(d, o),
            le32(d, o + 4),
            le32(d, o + 8),
            le32(d, o + 12),
            le32(d, o + 16),
            d.get(o + 20),
            d.get(o + 21),
        ) else {
            break;
        };
        let name = d
            .get(name as usize..)
            .and_then(|rest| rest.split(|&c| c == 0).next())
            .map(|n| String::from_utf8_lossy(&n[..n.len().min(256)]).to_lowercase())
            .unwrap_or_default();
        let used = if output { mask & !rw } else { rw };
        out.push(Elem { name, index, sysval, comp, register, mask, used });
    }
    out
}

/// Część PSV0: zakres szerokości fali i powiązania (klasa, przestrzeń,
/// dolny rejestr, górny; `u32::MAX` = nieograniczony).
struct Psv {
    lanes: Option<(u32, u32)>,
    bindings: Vec<(u32, u32, u32, u32)>,
}

fn psv(c: &Container) -> Psv {
    let mut out = Psv { lanes: None, bindings: Vec::new() };
    let Some(p) = c.part(b"PSV0") else { return out };
    let Some(info) = le32(p, 0).filter(|&s| s >= 24) else { return out };
    if let (Some(min), Some(max)) = (le32(p, 20), le32(p, 24)) {
        if !(min == 0 && max == u32::MAX) {
            out.lanes = Some((min, max));
        }
    }
    let at = 4 + info as usize;
    let n = le32(p, at).unwrap_or(0) as usize;
    let stride = if n > 0 { le32(p, at + 4).unwrap_or(0) as usize } else { 0 };
    if stride < 16 {
        return out;
    }
    for i in 0..n.min(4096) {
        let r = at + 8 + i * stride;
        let (Some(kind), Some(space), Some(lo), Some(hi)) = (le32(p, r), le32(p, r + 4), le32(p, r + 8), le32(p, r + 12)) else {
            break;
        };
        out.bindings.push((kind, space, lo, hi));
    }
    out
}

/// Dlaczego shader nie trafia do potoków.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skip {
    /// Bez kontenera albo uszkodzony.
    Broken,
    /// DXBC (SM5) - nie ta biblioteka.
    Dxbc,
    /// GS, HS, DS.
    Stage,
    Mesh,
    RayTracing,
}

/// Shader po odczycie (przed wspólnymi tablicami sygnatur).
struct Parsed {
    stage: St,
    counts: Counts,
    elems: Vec<Elem>,
    vertex_input: bool,
    position: bool,
    space_top: u8,
    groups: u32,
    nv_u0: bool,
    ags: bool,
    lanes: Option<(u32, u32)>,
}

fn parse_shader(code: &[u8], ue5: bool, ue58: bool) -> Result<Parsed, Skip> {
    let bytes = container_of(code).ok_or(Skip::Broken)?;
    let c = Container::parse(bytes).ok_or(Skip::Broken)?;
    let Some(dxil) = c.part(b"DXIL") else {
        return Err(if c.part(b"SHEX").or_else(|| c.part(b"SHDR")).is_some() { Skip::Dxbc } else { Skip::Broken });
    };
    let stage = match le32(dxil, 0).ok_or(Skip::Broken)? >> 16 {
        0 => St::Pixel,
        1 => St::Vertex,
        5 => St::Compute,
        2..=4 => return Err(Skip::Stage),
        13 | 14 => return Err(Skip::Mesh),
        6..=12 => return Err(Skip::RayTracing),
        _ => return Err(Skip::Broken),
    };
    let (elems, vertex_input, position) = match stage {
        St::Vertex => {
            let outputs = signature(&c, &[b"OSG1", b"OSG5", b"OSGN"], true);
            let inputs = signature(&c, &[b"ISG1", b"ISGN"], false);
            let position = outputs.is_empty() || outputs.iter().any(|e| e.sysval == 1);
            (outputs, inputs.iter().any(|e| e.sysval == 0), position)
        }
        St::Pixel => (signature(&c, &[b"ISG1", b"ISGN"], false), false, false),
        _ => (Vec::new(), false, false),
    };
    let psv = psv(&c);
    let mut p = Parsed {
        stage,
        counts: counts(code, ue5, ue58),
        elems,
        vertex_input,
        position,
        space_top: 0,
        groups: 0,
        nv_u0: false,
        ags: false,
        lanes: psv.lanes,
    };
    for &(kind, space, lo, hi) in &psv.bindings {
        let srv = (3..=5).contains(&kind);
        let uav = (6..=9).contains(&kind);
        if srv && hi == u32::MAX && (4..10).contains(&space) {
            p.space_top = p.space_top.max(space as u8);
        }
        if srv && hi == u32::MAX && space >= 10 && space / 10 < 32 {
            p.groups |= 1 << (space / 10);
        }
        p.nv_u0 |= uav && space == 1001 && lo == 0;
        p.ags |= space == 0x7FFF_0ADE;
    }
    Ok(p)
}

/// Czy wyjścia VS zasilają wejścia PS: każde wejście (bez wartości
/// generowanych przez rasteryzator) zapisane z pełną maską.
fn links(vs: &[Elem], ps: &[Elem]) -> bool {
    ps.iter().all(|i| {
        // Wartości systemowe z poprzedniego etapu: odległości obcinania,
        // indeksy tablicy celów i rzutni.
        if i.sysval != 0 && !(2..=5).contains(&i.sysval) {
            return true;
        }
        vs.iter()
            .rev()
            .find(|o| o.name == i.name && o.index == i.index)
            .is_some_and(|o| i.mask & !o.mask == 0 && i.used & !o.used == 0)
    })
}

// --------------------------------- biblioteki --------------------------------

struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len())?;
        let s = &self.b[self.at..end];
        self.at = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    /// Liczba wpisów, które mieszczą się w pozostałych danych.
    fn count(&mut self, item: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.checked_mul(item)? <= self.b.len() - self.at).then_some(n)
    }
    fn hashes(&mut self, width: usize) -> Option<Vec<Hash>> {
        let n = self.count(width)?;
        (0..n)
            .map(|_| {
                let mut h = [0u8; 20];
                h[..width].copy_from_slice(self.take(width)?);
                Some(h)
            })
            .collect()
    }
}

/// Zakresy map w tablicy indeksów: poprawne indeksy shaderów albo `None`.
fn map_lists(maps: &[(u32, u32)], indices: &[u32], shaders: usize) -> Option<Vec<Vec<u32>>> {
    maps.iter()
        .map(|&(off, num)| {
            let list = indices.get(off as usize..(off as usize).checked_add(num as usize)?)?;
            list.iter().all(|&i| (i as usize) < shaders).then(|| list.to_vec())
        })
        .collect()
}

/// Archiwum `.ushaderbytecode` w wersji 2 (paczki .pak): mapy i shadery.
struct PakArchive {
    hashes: Vec<Hash>,
    /// (przesunięcie od `code_start`, rozmiar, rozmiar po rozpakowaniu).
    shaders: Vec<(u64, u32, u32)>,
    maps: Vec<Vec<u32>>,
    code_start: usize,
}

impl PakArchive {
    fn parse(b: &[u8]) -> Option<PakArchive> {
        let mut c = Cur { b, at: 0 };
        if c.u32()? != 2 {
            return None;
        }
        let n = c.count(20)?;
        c.take(n * 20)?; // skróty map
        let hashes = c.hashes(20)?;
        let n = c.count(16)?;
        let maps: Vec<(u32, u32)> = (0..n)
            .map(|_| {
                let m = (c.u32()?, c.u32()?);
                c.take(8)?; // wstępne wczytanie
                Some(m)
            })
            .collect::<Option<_>>()?;
        let n = c.count(17)?;
        let shaders: Vec<(u64, u32, u32)> = (0..n)
            .map(|_| {
                let s = (c.u64()?, c.u32()?, c.u32()?);
                c.take(1)?; // częstotliwość
                Some(s)
            })
            .collect::<Option<_>>()?;
        let n = c.count(16)?;
        c.take(n * 16)?; // wpisy wstępnego wczytania
        let n = c.count(4)?;
        let indices: Vec<u32> = (0..n).map(|_| c.u32()).collect::<Option<_>>()?;
        if hashes.len() != shaders.len() {
            return None;
        }
        let maps = map_lists(&maps, &indices, shaders.len())?;
        Some(PakArchive { hashes, shaders, maps, code_start: c.at })
    }

    /// Surowe bajty shadera `i` w archiwum.
    fn raw<'a>(&self, b: &'a [u8], i: usize) -> Option<&'a [u8]> {
        let &(off, size, _) = self.shaders.get(i)?;
        let start = self.code_start.checked_add(usize::try_from(off).ok()?)?;
        b.get(start..start.checked_add(size as usize)?)
    }
}

/// Rozpakowany kod shadera (Oodle, gdy rozmiary się różnią).
fn unpack(raw: &[u8], uncompressed: u32) -> Option<std::borrow::Cow<'_, [u8]>> {
    if raw.len() == uncompressed as usize {
        Some(std::borrow::Cow::Borrowed(raw))
    } else {
        super::unreal::decompress("oodle", raw, uncompressed as usize, None).ok().map(std::borrow::Cow::Owned)
    }
}

#[derive(Clone, Copy, Debug)]
struct IoGroup {
    chunk: [u8; 12],
    first: u32,
    count: u32,
    uncompressed: u32,
    compressed: u32,
}

/// Biblioteka shaderów IoStore (wersja 1): mapy, shadery (grupa,
/// przesunięcie), grupy.
struct IoLib {
    hashes: Vec<Hash>,
    shaders: Vec<(u32, u32)>,
    groups: Vec<IoGroup>,
    indices: Vec<u32>,
    maps: Vec<Vec<u32>>,
}

impl IoLib {
    /// `width`: bajty skrótu (20; 8 od UE 5.8).
    fn parse(b: &[u8], width: usize) -> Option<IoLib> {
        let mut c = Cur { b, at: 0 };
        if c.u32()? != 1 {
            return None;
        }
        c.hashes(width)?; // skróty map
        let hashes = c.hashes(width)?;
        let n = c.count(12)?;
        let chunks: Vec<[u8; 12]> = (0..n).map(|_| c.take(12)?.try_into().ok()).collect::<Option<_>>()?;
        let n = c.count(8)?;
        let maps: Vec<(u32, u32)> = (0..n).map(|_| Some((c.u32()?, c.u32()?))).collect::<Option<_>>()?;
        let n = c.count(8)?;
        let shaders: Vec<(u32, u32)> = (0..n)
            .map(|_| {
                let p = c.u64()?;
                Some((((p >> 4) & 0x3FFF_FFFF) as u32, ((p >> 34) & 0x3FFF_FFFF) as u32))
            })
            .collect::<Option<_>>()?;
        let n = c.count(16)?;
        let raw: Vec<[u32; 4]> = (0..n).map(|_| Some([c.u32()?, c.u32()?, c.u32()?, c.u32()?])).collect::<Option<_>>()?;
        let n = c.count(4)?;
        let indices: Vec<u32> = (0..n).map(|_| c.u32()).collect::<Option<_>>()?;
        if raw.len() != chunks.len() || hashes.len() != shaders.len() || shaders.iter().any(|&(g, _)| g as usize >= raw.len()) {
            return None;
        }
        let groups: Vec<IoGroup> = raw
            .iter()
            .zip(chunks)
            .map(|(&[first, count, uncompressed, compressed], chunk)| IoGroup { chunk, first, count, uncompressed, compressed })
            .collect();
        map_lists(&groups.iter().map(|g| (g.first, g.count)).collect::<Vec<_>>(), &indices, shaders.len())?;
        let maps = map_lists(&maps, &indices, shaders.len())?;
        Some(IoLib { hashes, shaders, groups, indices, maps })
    }

    fn parse_any(b: &[u8], short_first: bool) -> Option<IoLib> {
        let widths = if short_first { [8, 20] } else { [20, 8] };
        widths.iter().find_map(|&w| IoLib::parse(b, w))
    }

    /// Shadery grupy: (indeks shadera, początek, koniec) - do następnego
    /// przesunięcia w grupie albo do końca grupy.
    fn parts(&self, g: usize) -> Vec<(usize, usize, usize)> {
        let Some(grp) = self.groups.get(g) else { return Vec::new() };
        let first = grp.first as usize;
        let ids = self.indices.get(first..first.saturating_add(grp.count as usize)).unwrap_or(&[]);
        let mut list: Vec<(usize, usize)> = ids
            .iter()
            .filter_map(|&s| self.shaders.get(s as usize).map(|&(_, off)| (s as usize, off as usize)))
            .collect();
        list.sort_by_key(|&(_, o)| o);
        let len = grp.uncompressed as usize;
        (0..list.len())
            .filter_map(|i| {
                let (s, start) = list[i];
                let end = list.get(i + 1).map_or(len, |&(_, e)| e).min(len);
                (start < end).then_some((s, start, end))
            })
            .collect()
    }
}

/// Jawne kontenery IoStore gry i gdzie leżą porcje grup shaderów.
struct IoSrc {
    id: u64,
    tocs: Vec<(String, Toc)>,
    chunks: HashMap<[u8; 12], (usize, usize)>,
}

impl IoSrc {
    fn open(game: &Path, notes: &mut Vec<String>) -> Option<IoSrc> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let mut src = IoSrc {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            tocs: Vec::new(),
            chunks: HashMap::new(),
        };
        for path in super::unreal::tocs(game) {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            match Toc::open(&path) {
                Ok(t) if !t.encrypted() => {
                    for (i, id) in t.ids.iter().enumerate() {
                        if t.chunk_type(i) == CHUNK_SHADER_CODE {
                            src.chunks.entry(*id).or_insert((src.tocs.len(), i));
                        }
                    }
                    src.tocs.push((name, t));
                }
                Ok(_) => notes.push(format!("{name}: szyfrowany / encrypted")),
                Err(e) => notes.push(e),
            }
        }
        (!src.tocs.is_empty()).then_some(src)
    }

    /// Biblioteki: (kontener, porcja).
    fn libraries(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (t, (_, toc)) in self.tocs.iter().enumerate() {
            for i in (0..toc.ids.len()).filter(|&i| toc.chunk_type(i) == CHUNK_SHADER_LIBRARY) {
                out.push((t, i));
            }
        }
        out
    }

    /// Rozpakowane bajty grupy.
    fn group(&self, g: &IoGroup) -> Option<Vec<u8>> {
        let &(t, i) = self.chunks.get(&g.chunk)?;
        let raw = self.tocs.get(t)?.1.read(i, None).ok()?;
        if g.compressed == g.uncompressed {
            return Some(raw);
        }
        let packed = raw.get(..g.compressed as usize)?;
        super::unreal::decompress("oodle", packed, g.uncompressed as usize, None).ok()
    }

    /// Grupa przez pamięć wątku (ostatnie kilka): potoki jednej mapy biorą
    /// shadery z tych samych grup.
    fn cached_group(&self, g: &IoGroup) -> Option<Arc<Vec<u8>>> {
        type Cache = std::cell::RefCell<Vec<(u64, [u8; 12], Arc<Vec<u8>>)>>;
        thread_local! {
            static GROUPS: Cache = const { std::cell::RefCell::new(Vec::new()) };
        }
        let hit = GROUPS.with(|c| {
            c.borrow().iter().find(|(id, ch, _)| *id == self.id && *ch == g.chunk).map(|(.., d)| d.clone())
        });
        if hit.is_some() {
            return hit;
        }
        let data = Arc::new(self.group(g)?);
        GROUPS.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() >= 4 {
                c.remove(0);
            }
            c.push((self.id, g.chunk, data.clone()));
        });
        Some(data)
    }

    /// Czy biblioteka ma kod DXIL: pierwsza grupa z kontenerami decyduje.
    fn is_dxil(&self, lib: &IoLib) -> bool {
        for (g, grp) in lib.groups.iter().enumerate().take(8) {
            let Some(data) = self.group(grp) else { continue };
            let mut any = false;
            for (_, start, end) in lib.parts(g) {
                let Some(c) = data.get(start..end).and_then(container_of).and_then(Container::parse) else { continue };
                if c.is_dxil() {
                    return true;
                }
                any = true;
            }
            if any {
                return false;
            }
        }
        false
    }
}

/// Archiwum shaderów SM6 w paczce .pak (po nazwie).
fn is_sm6_archive(path: &str) -> bool {
    let l = path.to_lowercase();
    l.ends_with(".ushaderbytecode") && l.contains("sm6")
}

/// Paczki gry: łatki (`*_P.pak`) najpierw - ich archiwa zastępują te same
/// ścieżki z paczek podstawowych.
fn paks(game: &Path) -> Vec<PathBuf> {
    let mut list = super::ue12::paks(game);
    list.sort_by_key(|p| {
        !p.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase().ends_with("_p"))
    });
    list
}

// ------------------------------- zbieranie -----------------------------------

/// Karta, na której gra będzie rysować.
#[derive(Clone, Copy, Debug)]
struct Gpu {
    mesh_shaders: bool,
    amd: bool,
    /// Zakres szerokości fali (WaveLaneCountMin/Max).
    lanes: (u32, u32),
}

impl Default for Gpu {
    fn default() -> Self {
        Gpu { mesh_shaders: false, amd: false, lanes: (32, 32) }
    }
}

#[cfg(windows)]
fn gpu() -> Gpu {
    use super::Vendor;
    match super::warm::d3d::gpu_device() {
        Ok(d) => Gpu {
            mesh_shaders: d.mesh_shaders(),
            amd: d.vendor == Vendor::Amd,
            lanes: match d.vendor {
                Vendor::Nvidia => (32, 32),
                Vendor::Amd => (32, 64),
                Vendor::Intel => (8, 32),
            },
        },
        Err(_) => Gpu::default(),
    }
}

#[cfg(not(windows))]
fn gpu() -> Gpu {
    Gpu::default()
}

/// Shadery i mapy zebrane z bibliotek gry.
#[derive(Default)]
struct Builder {
    ue5: bool,
    ue58: bool,
    gpu: Gpu,
    /// Skrót → indeks shadera (`None` - pominięty).
    by_hash: HashMap<Hash, Option<usize>>,
    shaders: Vec<Shader>,
    bytes: Vec<Bytes>,
    keys: HashMap<Vec<(String, u32, u32, u32)>, u32>,
    lists: HashMap<Vec<Elem>, u32>,
    elems: Vec<Vec<Elem>>,
    maps: Vec<Vec<usize>>,
    skipped: HashMap<&'static str, usize>,
    notes: Vec<String>,
    /// Bajty archiwów trzymane w pamięci.
    held: usize,
}

/// Wątki do odczytu (wynik w kolejności zadań).
fn parallel<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let threads = std::thread::available_parallelism().map_or(4, |t| t.get()).clamp(1, 16);
    if n < 64 || threads == 1 {
        return (0..n).map(f).collect();
    }
    let chunk = n.div_ceil(threads);
    let f = &f;
    std::thread::scope(|s| {
        let parts: Vec<_> = (0..n)
            .step_by(chunk)
            .map(|from| s.spawn(move || (from..(from + chunk).min(n)).map(f).collect::<Vec<T>>()))
            .collect();
        // Wynik każdego zadania albo żaden (błąd w wątku przechodzi dalej -
        // przesunięte wyniki przypisałyby dane złym shaderom).
        parts.into_iter().flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
    })
}

impl Builder {
    fn new(major: u32, minor: u32, gpu: Gpu) -> Builder {
        Builder { ue5: major >= 5, ue58: major > 5 || (major == 5 && minor >= 8), gpu, ..Default::default() }
    }

    fn skip(&mut self, why: &'static str) {
        *self.skipped.entry(why).or_default() += 1;
    }

    /// Dodaje odczytany shader; `None` - pominięty.
    fn register(&mut self, hash: Hash, parsed: Result<Parsed, Skip>, bytes: impl FnOnce() -> Bytes) -> Option<usize> {
        let id = match parsed {
            Err(why) => {
                self.skip(match why {
                    Skip::Broken => "broken",
                    Skip::Dxbc => "dxbc",
                    Skip::Stage => "gs/hs/ds",
                    Skip::Mesh => "mesh/amplification",
                    Skip::RayTracing => "ray tracing",
                });
                None
            }
            Ok(p) if p.ags && !self.gpu.amd => {
                self.skip("ags");
                None
            }
            Ok(p) if p.lanes.is_some_and(|(min, max)| min > self.gpu.lanes.1 || max < self.gpu.lanes.0) => {
                self.skip("wave");
                None
            }
            Ok(p) => {
                let mut key: Vec<(String, u32, u32, u32)> = p
                    .elems
                    .iter()
                    .filter(|e| e.sysval == 0)
                    .map(|e| (e.name.clone(), e.index, e.register, e.comp))
                    .collect();
                key.sort();
                let next = self.keys.len() as u32;
                let key = *self.keys.entry(key).or_insert(next);
                let elems = match self.lists.get(&p.elems) {
                    Some(&i) => i,
                    None => {
                        let i = self.elems.len() as u32;
                        self.lists.insert(p.elems.clone(), i);
                        self.elems.push(p.elems);
                        i
                    }
                };
                self.shaders.push(Shader {
                    stage: Some(p.stage),
                    counts: p.counts,
                    key,
                    elems,
                    vertex_input: p.vertex_input,
                    position: p.position,
                    space_top: p.space_top,
                    groups: p.groups,
                    nv_u0: p.nv_u0,
                });
                self.bytes.push(bytes());
                Some(self.shaders.len() - 1)
            }
        };
        self.by_hash.insert(hash, id);
        id
    }

    /// Mapy biblioteki (globalna - jedna wspólna).
    fn add_maps(&mut self, hashes: &[Hash], maps: &[Vec<u32>], pooled: bool) -> usize {
        let mut out: Vec<Vec<usize>> = Vec::new();
        for m in maps {
            let list: Vec<usize> = m
                .iter()
                .filter_map(|&i| hashes.get(i as usize).and_then(|h| self.by_hash.get(h).copied().flatten()))
                .collect();
            if list.is_empty() {
                continue;
            }
            match (pooled, out.first_mut()) {
                (true, Some(all)) => all.extend(list),
                _ => out.push(list),
            }
        }
        if let (true, Some(all)) = (pooled, out.first_mut()) {
            all.sort_unstable();
            all.dedup();
        }
        let n = out.len();
        self.maps.extend(out);
        n
    }

    /// Archiwum `.ushaderbytecode` z paczki.
    fn add_pak_archive(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), String> {
        let arch = PakArchive::parse(&bytes).ok_or(format!("{name}: nieznany układ archiwum / unknown archive layout"))?;
        let mut seen = HashSet::new();
        let new: Vec<usize> = (0..arch.hashes.len())
            .filter(|&i| !self.by_hash.contains_key(&arch.hashes[i]) && seen.insert(arch.hashes[i]))
            .collect();
        let (ue5, ue58) = (self.ue5, self.ue58);
        let parsed = parallel(new.len(), |k| {
            let i = new[k];
            arch.raw(&bytes, i)
                .and_then(|raw| unpack(raw, arch.shaders[i].2))
                .ok_or(Skip::Broken)
                .and_then(|code| parse_shader(&code, ue5, ue58))
        });
        // Do potoków trafią tylko shadery VS/PS/CS: gdy to mniej niż połowa
        // archiwum, ich surowe bajty idą do osobnego bufora.
        let kept: usize = new
            .iter()
            .zip(&parsed)
            .filter(|(_, p)| p.is_ok())
            .map(|(&i, _)| arch.shaders[i].1 as usize)
            .sum();
        let compact = kept * 2 < bytes.len();
        let mut places = Vec::with_capacity(new.len());
        let data = if compact {
            let mut buf = Vec::with_capacity(kept);
            for (&i, p) in new.iter().zip(&parsed) {
                let raw = arch.raw(&bytes, i).filter(|_| p.is_ok()).unwrap_or(&[]);
                places.push(buf.len());
                buf.extend_from_slice(raw);
            }
            drop(bytes);
            buf
        } else {
            for &i in &new {
                places.push(arch.code_start + arch.shaders[i].0 as usize);
            }
            bytes
        };
        self.held += data.len();
        let data = Arc::new(data);
        for ((&i, p), at) in new.iter().zip(parsed).zip(places) {
            let (_, size, uncompressed) = arch.shaders[i];
            let data = data.clone();
            self.register(arch.hashes[i], p, || {
                Bytes::Lazy(Arc::new(move || {
                    let raw = data.get(at..at.checked_add(size as usize)?)?;
                    let code = unpack(raw, uncompressed)?;
                    container_of(&code).map(<[u8]>::to_vec)
                }))
            });
        }
        let pooled = name.to_lowercase().contains("global");
        let maps = self.add_maps(&arch.hashes, &arch.maps, pooled);
        self.notes.push(format!(
            "{name}: map {maps}, shaderów {} / {maps} shader maps, {} shaders",
            arch.hashes.len(),
            arch.hashes.len()
        ));
        Ok(())
    }

    /// Archiwa SM6 z jawnych paczek .pak.
    fn add_paks(&mut self, list: &[PathBuf], say: &dyn Fn(String), total: usize) {
        let mut done = HashSet::new();
        let mut errors = Vec::new();
        for (k, pak) in list.iter().enumerate() {
            say(format!("scan {k} {total}"));
            let p = match super::unreal_pak::Pak::open(pak) {
                Ok(p) if !p.encrypted_index => p,
                Ok(_) => {
                    errors.push(format!("{}: indeks szyfrowany / encrypted index", pak.display()));
                    continue;
                }
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            for e in p.files.iter().filter(|e| is_sm6_archive(&e.path)) {
                if !done.insert(e.path.to_lowercase()) {
                    continue;
                }
                let name = e.path.rsplit('/').next().unwrap_or(&e.path).to_owned();
                match p.read(e, None).and_then(|b| self.add_pak_archive(&name, b)) {
                    Ok(()) => {}
                    Err(err) => errors.push(err),
                }
            }
        }
        // Błędy tylko pierwszych kilku paczek.
        self.notes.extend(errors.into_iter().take(3));
    }

    /// Biblioteki DXIL z jawnych kontenerów IoStore.
    fn add_io(&mut self, game: &Path, say: &dyn Fn(String), total: usize) {
        let mut notes = Vec::new();
        let Some(src) = IoSrc::open(game, &mut notes) else {
            self.notes.extend(notes.into_iter().take(3));
            return;
        };
        let src = Arc::new(src);
        say(format!("scan {} {total}", total.saturating_sub(1)));
        for (t, i) in src.libraries() {
            let toc_name = src.tocs[t].0.clone();
            let lib = match src.tocs[t].1.read(i, None).map(|d| IoLib::parse_any(&d, self.ue58)) {
                Ok(Some(l)) => l,
                Ok(None) => {
                    notes.push(format!("{toc_name}: nieznany układ biblioteki / unknown library layout"));
                    continue;
                }
                Err(e) => {
                    notes.push(format!("{toc_name}: {e}"));
                    continue;
                }
            };
            if !src.is_dxil(&lib) {
                continue; // SM5 albo ES3.1
            }
            let mut wanted = vec![false; lib.hashes.len()];
            let mut seen = HashSet::new();
            for (s, h) in lib.hashes.iter().enumerate() {
                wanted[s] = !self.by_hash.contains_key(h) && seen.insert(*h);
            }
            let groups: Vec<usize> = (0..lib.groups.len())
                .filter(|&g| lib.parts(g).iter().any(|&(s, ..)| wanted[s]))
                .collect();
            let (ue5, ue58) = (self.ue5, self.ue58);
            let found = parallel(groups.len(), |k| {
                let g = groups[k];
                let data = src.group(&lib.groups[g]);
                lib.parts(g)
                    .into_iter()
                    .filter(|&(s, ..)| wanted[s])
                    .map(|(s, start, end)| {
                        let parsed = data
                            .as_ref()
                            .and_then(|d| d.get(start..end))
                            .ok_or(Skip::Broken)
                            .and_then(|code| parse_shader(code, ue5, ue58));
                        (g, s, start, end, parsed)
                    })
                    .collect::<Vec<_>>()
            });
            for (g, s, start, end, parsed) in found.into_iter().flatten() {
                let grp = lib.groups[g];
                let src = src.clone();
                self.register(lib.hashes[s], parsed, || {
                    Bytes::Lazy(Arc::new(move || {
                        let data = src.cached_group(&grp)?;
                        container_of(data.get(start..end)?).map(<[u8]>::to_vec)
                    }))
                });
            }
            let pooled = toc_name.eq_ignore_ascii_case("global.utoc");
            let maps = self.add_maps(&lib.hashes, &lib.maps, pooled);
            self.notes.push(format!(
                "{toc_name}: map {maps}, shaderów {} / {maps} shader maps, {} shaders",
                lib.hashes.len(),
                lib.hashes.len()
            ));
        }
        self.notes.extend(notes.into_iter().take(3));
    }
}

// --------------------------------- potoki ------------------------------------

/// Potoki z map i ich root signature.
struct Planner<'a> {
    rule: &'static Rule,
    ctx: Ctx,
    shaders: &'a [Shader],
    descs: HashMap<Desc, Option<usize>>,
    signatures: Vec<Vec<u8>>,
    pipelines: Vec<Pipeline12>,
    seen: HashSet<[Option<usize>; 6]>,
    over_cap: usize,
    unserializable: usize,
    first_error: Option<String>,
}

impl Planner<'_> {
    /// Potok z etapów [VS, PS, GS, HS, DS, CS] (bez powtórzeń i do limitu).
    fn add(&mut self, stages: [Option<usize>; 6]) {
        if self.seen.contains(&stages) {
            return;
        }
        if self.pipelines.len() >= MAX_PIPELINES {
            self.over_cap += 1;
            return;
        }
        let list: Vec<(St, &Shader)> = [(0, St::Vertex), (1, St::Pixel), (5, St::Compute)]
            .iter()
            .filter_map(|&(slot, st)| stages[slot].and_then(|i| self.shaders.get(i)).map(|s| (st, s)))
            .collect();
        let desc = build(self.rule, &list, &self.ctx);
        let rs = match self.descs.get(&desc) {
            Some(&rs) => rs,
            None => {
                let rs = match serialize(&desc, self.rule.samplers) {
                    Ok(blob) => {
                        self.signatures.push(blob);
                        Some(self.signatures.len() - 1)
                    }
                    Err(e) => {
                        self.first_error.get_or_insert(e);
                        None
                    }
                };
                self.descs.insert(desc, rs);
                rs
            }
        };
        self.seen.insert(stages);
        match rs {
            Some(rs) => self.pipelines.push(Pipeline12 { rs: Some(rs), stages }),
            None => self.unserializable += 1,
        }
    }
}

fn stages(vs: Option<usize>, ps: Option<usize>, cs: Option<usize>) -> [Option<usize>; 6] {
    [vs, ps, None, None, None, cs]
}


/// Root signature, potoki i uwagi planu.
struct Planned {
    signatures: Vec<Vec<u8>>,
    pipelines: Vec<Pipeline12>,
    notes: Vec<String>,
}

/// Potoki gry: najpierw każdy compute, każdy PS z pierwszym pasującym VS
/// mapy (wolny VS przed użytym) i VS bez pary osobno; potem pozostali
/// kandydaci - tak, żeby limit odciął najpierw powtórzenia.
fn plan(rule: &'static Rule, ctx: Ctx, shaders: &[Shader], elems: &[Vec<Elem>], maps: &[Vec<usize>]) -> Planned {
    let mut p = Planner {
        rule,
        ctx,
        shaders,
        descs: HashMap::new(),
        signatures: Vec::new(),
        pipelines: Vec::new(),
        seen: HashSet::new(),
        over_cap: 0,
        unserializable: 0,
        first_error: None,
    };
    let list = |i: usize| elems.get(shaders[i].elems as usize).map_or(&[][..], Vec::as_slice);
    let stage = |i: usize| shaders.get(i).and_then(|s| s.stage);
    let mut paired = vec![false; shaders.len()];
    let mut fed = vec![false; shaders.len()];
    let mut extra: Vec<(usize, usize)> = Vec::new();
    for map in maps {
        for &s in map.iter().filter(|&&s| stage(s) == Some(St::Compute)) {
            p.add(stages(None, None, Some(s)));
        }
        let mut by_key: HashMap<u32, Vec<usize>> = HashMap::new();
        for &s in map.iter().filter(|&&s| stage(s) == Some(St::Vertex) && shaders[s].position) {
            by_key.entry(shaders[s].key).or_default().push(s);
        }
        for &ps in map.iter().filter(|&&s| stage(s) == Some(St::Pixel)) {
            let Some(vss) = by_key.get(&shaders[ps].key) else { continue };
            let mut fits: Vec<usize> = vss.iter().copied().filter(|&v| links(list(v), list(ps))).collect();
            fits.sort_by_key(|&v| paired[v]);
            fits.truncate(CANDIDATES);
            let Some((&first, rest)) = fits.split_first() else { continue };
            paired[first] = true;
            fed[ps] = true;
            p.add(stages(Some(first), Some(ps), None));
            extra.extend(rest.iter().map(|&v| (v, ps)));
        }
    }
    let first_pass = p.pipelines.len();
    for v in (0..shaders.len()).filter(|&v| stage(v) == Some(St::Vertex) && shaders[v].position && !paired[v]) {
        p.add(stages(Some(v), None, None));
    }
    let lone = p.pipelines.len() - first_pass;
    for (v, ps) in extra {
        p.add(stages(Some(v), Some(ps), None));
    }
    let count = |st: St| (0..shaders.len()).filter(|&i| stage(i) == Some(st)).count();
    let unfed = (0..shaders.len()).filter(|&i| stage(i) == Some(St::Pixel) && !fed[i]).count();
    let mut notes = vec![format!(
        "VS {}, PS {}, CS {}; potoki {} (VS bez pary {lone}), root signature {}; PS bez pasującego VS w mapie {unfed} / pipelines {} (unpaired VS {lone}), root signatures {}; PS without a linking VS in their map {unfed}",
        count(St::Vertex),
        count(St::Pixel),
        count(St::Compute),
        p.pipelines.len(),
        p.signatures.len(),
        p.pipelines.len(),
        p.signatures.len(),
    )];
    if p.over_cap > 0 {
        notes.push(format!(
            "potoki ponad limit {MAX_PIPELINES}: {} pominięte / pipelines over the {MAX_PIPELINES} cap: {} left out",
            p.over_cap, p.over_cap
        ));
    }
    if p.unserializable > 0 {
        notes.push(format!(
            "potoki bez root signature: {} ({}) / pipelines without a root signature: {}",
            p.unserializable,
            p.first_error.unwrap_or_default(),
            p.unserializable
        ));
    }
    Planned { signatures: p.signatures, pipelines: p.pipelines, notes }
}

// --------------------------------- wejście -----------------------------------

/// Czy gra Unreal ma jawne biblioteki shaderów SM6 i wersję silnika ze znaną
/// zasadą root signature. Czyta .exe gry (wersja) i spisy paczek.
pub fn available(game: &Path) -> bool {
    let Some((major, minor)) = game_version(game) else { return false };
    if rule_for(major, minor).is_none() {
        return false;
    }
    let in_paks = paks(game).iter().any(|pak| {
        super::unreal_pak::Pak::open(pak)
            .is_ok_and(|p| !p.encrypted_index && p.files.iter().any(|e| is_sm6_archive(&e.path)))
    });
    in_paks || {
        let short = major > 5 || (major == 5 && minor >= 8);
        IoSrc::open(game, &mut Vec::new()).is_some_and(|src| {
            src.libraries().into_iter().any(|(t, i)| {
                src.tocs[t].1.read(i, None).ok().and_then(|d| IoLib::parse_any(&d, short)).is_some_and(|l| src.is_dxil(&l))
            })
        })
    }
}

/// Potoki DirectX 12 z map shaderów gry (root signature na potok).
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let (major, minor) = game_version(game).ok_or("Unreal: nieznana wersja silnika / unknown engine version")?;
    let rule = rule_for(major, minor)
        .ok_or_else(|| format!("Unreal {major}.{minor}: brak zasady root signature / no root signature rule"))?;
    let gpu = gpu();
    let mut b = Builder::new(major, minor, gpu);
    b.notes.push(format!("Unreal {major}.{minor}: root signature wg zasady {0} / root signatures by rule {0}", rule.id));
    let list = paks(game);
    let total = list.len() + 1;
    b.add_paks(&list, say, total);
    b.add_io(game, say, total);
    say(format!("scan {total} {total}"));
    if b.shaders.is_empty() {
        let why = b.notes.iter().skip(1).take(3).cloned().collect::<Vec<_>>().join("; ");
        return Err(format!("brak czytelnych shaderów SM6 / no readable SM6 shaders{}", if why.is_empty() { String::new() } else { format!(": {why}") }));
    }
    let any_space = b.shaders.iter().any(|s| s.space_top > 0);
    let need = b.shaders.iter().map(|s| s.counts.srvs as u32).max().unwrap_or(0);
    let ctx = Ctx {
        mesh_tier: gpu.mesh_shaders,
        max_srvs: if rule.avalanche_srvs && any_space {
            128
        } else if rule.grow_srvs && need > rule.max_srvs {
            if need <= 48 { 48 } else { 64 }
        } else {
            0
        },
        nv_everywhere: !rule.nv_extension
            && b.shaders.iter().any(|s| s.nv_u0)
            && b.shaders.iter().all(|s| s.counts.flags & F_NV == 0),
    };
    if ctx.max_srvs > 0 {
        b.notes.push(format!("tabele SRV po {0} / SRV tables of {0}", ctx.max_srvs));
    }
    if ctx.nv_everywhere {
        b.notes.push("tabela NVIDIA u0 space 1001 w każdej root signature / NVIDIA extension table in every root signature".into());
    }
    let planned = plan(rule, ctx, &b.shaders, &b.elems, &b.maps);
    let mut notes = std::mem::take(&mut b.notes);
    notes.extend(planned.notes);
    if !b.skipped.is_empty() {
        let mut list: Vec<String> = b.skipped.iter().map(|(k, n)| format!("{k} {n}")).collect();
        list.sort();
        notes.push(format!("pominięte shadery / skipped shaders: {}", list.join(", ")));
    }
    notes.push(format!("archiwa w pamięci / archives held in memory: {} MB", b.held >> 20));
    Ok(Set12 { shaders: b.bytes, signatures: planned.signatures, pipelines: planned.pipelines, pools: Vec::new(), notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Element sygnatury do testów: (nazwa, indeks, wartość systemowa, wiersz, maska, użyte).
    type E<'a> = (&'a str, u32, u32, u32, u8, u8);

    fn sig_part(elems: &[E]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend((elems.len() as u32).to_le_bytes());
        b.extend(8u32.to_le_bytes());
        let names_at = 8 + 32 * elems.len();
        let mut names = Vec::new();
        for &(name, index, sysval, register, mask, used) in elems {
            b.extend(0u32.to_le_bytes()); // strumień
            b.extend(((names_at + names.len()) as u32).to_le_bytes());
            names.extend(name.as_bytes());
            names.push(0);
            for v in [index, sysval, 3, register] {
                b.extend(v.to_le_bytes());
            }
            b.extend([mask, used, 0, 0]);
            b.extend(0u32.to_le_bytes());
        }
        b.extend(names);
        b
    }

    /// Kontener DXIL: program `kind`, sygnatury, PSV0 z powiązaniami.
    fn dxil(kind: u32, inputs: &[E], outputs: &[E], bindings: &[(u32, u32, u32, u32)]) -> Vec<u8> {
        let mut program = ((kind << 16) | 0x60).to_le_bytes().to_vec();
        program.extend([0u8; 12]);
        let mut psv = 24u32.to_le_bytes().to_vec();
        psv.extend([0u8; 16]);
        psv.extend(0u32.to_le_bytes());
        psv.extend(u32::MAX.to_le_bytes());
        psv.extend((bindings.len() as u32).to_le_bytes());
        if !bindings.is_empty() {
            psv.extend(16u32.to_le_bytes());
            for &(k, s, lo, hi) in bindings {
                for v in [k, s, lo, hi] {
                    psv.extend(v.to_le_bytes());
                }
            }
        }
        let parts: Vec<(&[u8; 4], Vec<u8>)> =
            vec![(b"DXIL", program), (b"ISG1", sig_part(inputs)), (b"OSG1", sig_part(outputs)), (b"PSV0", psv)];
        let header = 32 + 4 * parts.len();
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        for (fourcc, data) in &parts {
            offsets.push((header + body.len()) as u32);
            body.extend(*fourcc);
            body.extend((data.len() as u32).to_le_bytes());
            body.extend(data);
        }
        let mut c = b"DXBC".to_vec();
        c.extend([7u8; 16]);
        c.extend(1u32.to_le_bytes());
        c.extend(((header + body.len()) as u32).to_le_bytes());
        c.extend((parts.len() as u32).to_le_bytes());
        for o in offsets {
            c.extend(o.to_le_bytes());
        }
        c.extend(body);
        c
    }

    /// Shader UE: nagłówek, kontener, dane 'p' (flagi, samplery, SRV, CB, UAV).
    fn packed(container: &[u8], p: [u8; 5], extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut b = b"UE-SRT".to_vec();
        b.extend(container);
        let start = b.len();
        for (k, d) in std::iter::once(&(b'p', p.to_vec())).chain(extra) {
            b.push(*k);
            b.extend((d.len() as u32).to_le_bytes());
            b.extend(d);
        }
        let total = (b.len() - start + 4) as u32;
        b.extend(total.to_le_bytes());
        b
    }

    const POS: E = ("sv_position", 0, 1, 0, 0xF, 0x0);
    const UV: E = ("texcoord", 0, 0, 1, 0x3, 0x0);

    fn vs(outputs: &[E]) -> Vec<u8> {
        packed(&dxil(1, &[("attribute", 0, 0, 0, 0x7, 0x7)], outputs, &[]), [0, 0, 1, 2, 0], &[])
    }

    /// Archiwum v2: jeden shader na wpis, mapy jako listy indeksów.
    fn archive(codes: &[Vec<u8>], maps: &[&[u32]]) -> Vec<u8> {
        let mut b = 2u32.to_le_bytes().to_vec();
        b.extend((maps.len() as u32).to_le_bytes());
        for m in 0..maps.len() {
            b.extend([m as u8 + 100; 20]);
        }
        b.extend((codes.len() as u32).to_le_bytes());
        for i in 0..codes.len() {
            b.extend([i as u8 + 1; 20]);
        }
        b.extend((maps.len() as u32).to_le_bytes());
        let mut indices: Vec<u32> = Vec::new();
        for m in maps {
            b.extend((indices.len() as u32).to_le_bytes());
            b.extend((m.len() as u32).to_le_bytes());
            b.extend([0u8; 8]);
            indices.extend(m.iter());
        }
        b.extend((codes.len() as u32).to_le_bytes());
        let mut at = 0u64;
        for c in codes {
            b.extend(at.to_le_bytes());
            b.extend((c.len() as u32).to_le_bytes());
            b.extend((c.len() as u32).to_le_bytes());
            b.push(0);
            at += c.len() as u64;
        }
        b.extend(0u32.to_le_bytes());
        b.extend((indices.len() as u32).to_le_bytes());
        for i in indices {
            b.extend(i.to_le_bytes());
        }
        for c in codes {
            b.extend(c);
        }
        b
    }

    fn shader(stage: St, samplers: u8, srvs: u8, cbs: u8, uavs: u8) -> Shader {
        Shader {
            stage: Some(stage),
            counts: Counts { samplers, srvs, cbs, uavs, flags: 0 },
            position: true,
            vertex_input: true,
            ..Default::default()
        }
    }

    #[test]
    fn ue_maps_reads_engine_version() {
        let mut exe = b"xx++UE5+Release-5.1-CL-23901901yy".to_vec();
        assert_eq!(version_in(&exe), Some((5, 1)));
        exe = "\0\0++UE4+Release-4.27\0".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        assert_eq!(version_in(&exe), Some((4, 27)));
        assert_eq!(version_in(b"++UE5+Release-4.1"), None);
        assert_eq!(rule_for(4, 23).map(|r| r.id), Some("ue-4.22"));
        assert_eq!(rule_for(4, 27).map(|r| r.id), Some("ue-4.26"));
        assert_eq!(rule_for(5, 3).map(|r| r.id), Some("ue-5.1"));
        assert_eq!(rule_for(5, 7).map(|r| r.id), Some("ue-5.5"));
        assert_eq!(rule_for(6, 0).map(|r| r.id), Some("ue-5.8"));
        assert!(rule_for(4, 19).is_none() && rule_for(6, 1).is_none());
    }

    #[test]
    fn ue_maps_parses_packed_shader() {
        let mut v = 1u32.to_le_bytes().to_vec();
        v.extend(0x1002u32.to_le_bytes());
        v.extend([0u8; 11]);
        let ps = packed(
            &dxil(
                0,
                &[POS, ("texcoord", 0, 0, 1, 0x3, 0x3)],
                &[("sv_target", 0, 64, 0, 0xF, 0)],
                &[(3, 5, 0, u32::MAX), (6, 1001, 0, 0)],
            ),
            [0x0A, 2, 3, 1, 1],
            &[(b'x', vec![0x10, 0]), (b'v', v)],
        );
        let p = parse_shader(&ps, true, false).ok().unwrap();
        assert_eq!(p.stage, St::Pixel);
        assert_eq!(
            p.counts,
            Counts { samplers: 2, srvs: 3, cbs: 1, uavs: 1, flags: F_BINDLESS_RESOURCES | F_ROOT_CONSTANTS | F_DIAGNOSTIC | F_AMD }
        );
        assert_eq!(p.elems.len(), 2);
        assert_eq!((p.space_top, p.nv_u0), (5, true));
        // UE4: bez flag.
        assert_eq!(parse_shader(&ps, false, false).ok().unwrap().counts.flags, 0);
        // Ucięte dane i nie-shader.
        assert!(parse_shader(&ps[..ps.len() / 2], true, false).is_err());
        assert!(parse_shader(b"DXBC", true, false).is_err());
        let gs = packed(&dxil(2, &[], &[], &[]), [0; 5], &[]);
        assert_eq!(parse_shader(&gs, true, false).err(), Some(Skip::Stage));
    }

    #[test]
    fn ue_maps_links_by_signature() {
        let out = [POS, ("texcoord", 0, 0, 1, 0x3, 0x0), ("sv_clipdistance", 0, 2, 2, 0x1, 0x0)];
        let ps = [POS, ("texcoord", 0, 0, 1, 0x3, 0x3)];
        let el = |l: &[E]| -> Vec<Elem> {
            l.iter()
                .map(|&(name, index, sysval, register, mask, used)| Elem { name: name.into(), index, sysval, comp: 3, register, mask, used: if used == 0 { mask } else { used } })
                .collect()
        };
        assert!(links(&el(&out), &el(&ps)));
        // PS czyta składową, której VS nie zapisuje.
        assert!(!links(&el(&out), &el(&[("texcoord", 0, 0, 1, 0x7, 0x7)])));
        // Odległość obcinania musi przyjść z VS.
        assert!(!links(&el(&[POS]), &el(&[("sv_clipdistance", 0, 2, 2, 0x1, 0x1)])));
        assert!(links(&el(&[POS]), &el(&[POS, ("sv_isfrontface", 0, 9, 1, 0x1, 0x1)])));
    }

    #[test]
    fn ue_maps_stock_root_signatures() {
        let ctx = Ctx::default();
        // 4.26: compute zakazuje wszystkich etapów graficznych (z HS/DS).
        let cs = shader(St::Compute, 1, 2, 2, 1);
        let d = build(&UE426, &[(St::Compute, &cs)], &ctx);
        assert_eq!(d.flags, 0x3E);
        assert_eq!(d.params.len(), 5);
        assert_eq!(d.params[0], table(0, 0, 64, 0, 5));
        assert_eq!(d.params[1], table(0, 3, 16, 0, 1));
        assert_eq!(d.params[2], table(0, 1, 16, 0, 3));
        // 5.0: tabele także bez powiązań; obecny etap nie jest zakazany.
        let (v, p) = (shader(St::Vertex, 0, 0, 1, 0), shader(St::Pixel, 0, 1, 0, 0));
        let d = build(&UE50, &[(St::Vertex, &v), (St::Pixel, &p)], &Ctx { mesh_tier: true, ..ctx });
        assert_eq!(d.params.len(), 3 + 2 + 1);
        assert_eq!(d.flags, 0x1 | 0x10 | 0x100 | 0x200);
        // 5.1: bez powiązań - bez tabel i z zakazem.
        let d = build(&UE51, &[(St::Vertex, &shader(St::Vertex, 0, 0, 0, 0)), (St::Pixel, &p)], &ctx);
        assert_eq!(d.params, vec![table(5, 0, 64, 0, 5)]);
        assert_eq!(d.flags, 0x1 | 0x2 | 0x10);
        // 4.20: układ wejścia tylko z wejściem wierzchołków; 32 SRV, bez UAV w VS.
        let mut bare = shader(St::Vertex, 0, 0, 0, 1);
        bare.vertex_input = false;
        let d = build(&UE420, &[(St::Vertex, &bare)], &ctx);
        assert_eq!(d.flags, 0x20 | 0x10 | 0x4 | 0x8);
        assert_eq!(d.params, vec![table(1, 0, 32, 0, 5), table(1, 3, 16, 0, 1)]);
        let wide = build(&UE420, &[(St::Vertex, &bare)], &Ctx { max_srvs: 48, ..ctx });
        assert_eq!(wide.params[0], table(1, 0, 48, 0, 5));
        // 5.8: tabela NVIDIA przy fladze rozszerzenia.
        let mut nv = shader(St::Compute, 0, 1, 0, 0);
        nv.counts.flags = F_NV | F_DIAGNOSTIC;
        let d = build(&UE58, &[(St::Compute, &nv)], &ctx);
        assert_eq!(
            d.params[1..],
            [
                Param::Table { vis: 0, ranges: vec![Range { kind: 1, count: 1, base: 0, space: 1001, flags: 0, offset: 0 }] },
                Param::Root { kind: 4, vis: 0, register: 0, space: 999, flags: 2 },
            ]
        );
        // Fork 4.27: tabele nieograniczonych SRV w przestrzeniach 4..top.
        let mut sb = shader(St::Pixel, 0, 1, 0, 0);
        sb.space_top = 5;
        let d = build(&UE426, &[(St::Pixel, &sb)], &ctx);
        assert_eq!(d.params[1..3], [table(5, 0, UNBOUNDED, 4, 5), table(5, 0, UNBOUNDED, 5, 5)]);
    }

    /// UE 5.1-5.7 bez przypadków brzegowych: to samo co `ue12::root_signature`.
    #[cfg(windows)]
    #[test]
    fn ue_maps_matches_ue12_builder() {
        use super::super::ue12;
        let ue = |c: &Shader| ue12::Counts {
            flags: if c.counts.flags & F_ROOT_CONSTANTS != 0 { 0x08 } else { 0 },
            samplers: c.counts.samplers,
            srvs: c.counts.srvs,
            cbs: c.counts.cbs,
            uavs: c.counts.uavs,
            ..Default::default()
        };
        let mut v = shader(St::Vertex, 1, 3, 2, 1);
        let p = shader(St::Pixel, 2, 5, 3, 1);
        let c = shader(St::Compute, 0, 2, 1, 4);
        for (rule, minor) in [(&UE51, 1), (&UE54, 4), (&UE55, 5)] {
            for mesh in [false, true] {
                v.counts.flags = if minor >= 4 { F_ROOT_CONSTANTS } else { 0 };
                let t = ue12::Target { minor, mesh_shaders: mesh };
                let ctx = Ctx { mesh_tier: mesh, ..Default::default() };
                let cases: [(Vec<(St, &Shader)>, Vec<(ue12::Stage, ue12::Counts)>); 3] = [
                    (vec![(St::Vertex, &v), (St::Pixel, &p)], vec![(ue12::Stage::Vertex, ue(&v)), (ue12::Stage::Pixel, ue(&p))]),
                    (vec![(St::Vertex, &v)], vec![(ue12::Stage::Vertex, ue(&v))]),
                    (vec![(St::Compute, &c)], vec![(ue12::Stage::Compute, ue(&c))]),
                ];
                for (mine, theirs) in cases {
                    let a = serialize(&build(rule, &mine, &ctx), rule.samplers).unwrap();
                    let b = ue12::serialize(&ue12::root_signature(&theirs, t)).unwrap();
                    assert_eq!(a, b, "{} {mine:?}", rule.id);
                }
            }
        }
    }

    #[test]
    fn ue_maps_reads_iostore_library() {
        // Wersja 1, skróty 8 B: 1 mapa, 2 shadery w jednej grupie.
        let mut b = 1u32.to_le_bytes().to_vec();
        b.extend(1u32.to_le_bytes());
        b.extend([9u8; 8]);
        b.extend(2u32.to_le_bytes());
        b.extend([1u8; 8]);
        b.extend([2u8; 8]);
        b.extend(1u32.to_le_bytes());
        b.extend([5u8; 12]);
        b.extend(1u32.to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(2u32.to_le_bytes());
        b.extend(2u32.to_le_bytes());
        for off in [0u64, 100] {
            b.extend((off << 34).to_le_bytes());
        }
        b.extend(1u32.to_le_bytes());
        for v in [0u32, 2, 160, 90] {
            b.extend(v.to_le_bytes());
        }
        b.extend(2u32.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(0u32.to_le_bytes());
        let lib = IoLib::parse_any(&b, false).unwrap();
        assert_eq!(lib.hashes[1][..8], [2u8; 8]);
        assert_eq!(lib.maps, vec![vec![1, 0]]);
        assert_eq!(lib.parts(0), vec![(0, 0, 100), (1, 100, 160)]);
        assert!(IoLib::parse(&b, 20).is_none());
        assert!(IoLib::parse(&b[..b.len() - 2], 8).is_none());
    }

    /// Cała ścieżka na syntetycznym archiwum: mapy, pary, root signature, bajty.
    #[cfg(windows)]
    #[test]
    fn ue_maps_pairs_shaders_within_maps() {
        let vs_uv = vs(&[POS, UV]);
        let vs_depth = vs(&[POS]);
        let ps_uv = packed(&dxil(0, &[POS, ("texcoord", 0, 0, 1, 0x3, 0x3)], &[], &[]), [0, 1, 1, 1, 0], &[]);
        let ps_other = packed(&dxil(0, &[("texcoord", 1, 0, 2, 0x3, 0x3)], &[], &[]), [0, 1, 1, 1, 0], &[]);
        let cs = packed(&dxil(5, &[], &[], &[]), [0, 0, 1, 1, 1], &[]);
        let gs = packed(&dxil(2, &[], &[], &[]), [0; 5], &[]);
        let codes = vec![vs_uv, vs_depth, ps_uv, ps_other, cs, gs];
        let bytes = archive(&codes, &[&[0, 1, 2, 4, 5], &[1, 3]]);
        let mut b = Builder::new(5, 1, Gpu::default());
        b.add_pak_archive("ShaderArchive-Game-PCD3D_SM6.ushaderbytecode", bytes.clone()).unwrap();
        assert_eq!(b.shaders.len(), 5); // GS pominięty
        assert_eq!(b.skipped.get("gs/hs/ds"), Some(&1));
        // Drugi raz te same skróty - bez nowych shaderów.
        b.add_pak_archive("copy", bytes).unwrap();
        assert_eq!(b.shaders.len(), 5);
        let planned = plan(&UE51, Ctx::default(), &b.shaders, &b.elems, &b.maps);
        let pipes: Vec<[Option<usize>; 6]> = planned.pipelines.iter().map(|p| p.stages).collect();
        // compute, VS+PS w mapie 0, VS głębi osobno; PS z innym wejściem bez pary.
        assert_eq!(pipes, vec![stages(None, None, Some(4)), stages(Some(0), Some(2), None), stages(Some(1), None, None)]);
        assert_eq!(planned.signatures.len(), 3);
        assert!(planned.pipelines.iter().all(|p| p.rs.is_some_and(|r| r < planned.signatures.len())));
        // Leniwe bajty: sam kontener.
        let c = b.bytes[2].get().unwrap();
        assert!(c.starts_with(b"DXBC") && Container::parse(&c).is_some_and(|c| c.is_dxil()));
        assert_eq!(c.len(), dxil(0, &[POS, ("texcoord", 0, 0, 1, 0x3, 0x3)], &[], &[]).len());
    }

    #[test]
    fn ue_maps_rejects_bad_archives() {
        assert!(PakArchive::parse(&[]).is_none());
        assert!(PakArchive::parse(&3u32.to_le_bytes()).is_none());
        let mut bad = archive(&[vec![1, 2, 3]], &[&[0]]);
        // Mapa wskazuje poza tablicę indeksów.
        bad[4 + 4 + 20 + 4 + 20 + 4] = 7;
        assert!(PakArchive::parse(&bad).is_none());
        assert!(trailer(&[0xFF, 0xFF, 0xFF, 0xFF]).is_empty());
        assert!(container_of(b"DXBC\0\0\0").is_none());
    }

    /// Palworld (UE 5.1, paczka .pak): `cargo test ue_maps -- --ignored --nocapture`.
    /// Tworzy próbkę potoków na karcie (root signature wobec shaderów).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn ue_maps_palworld() {
        use super::super::container;
        use super::super::warm::{GraphicsStages, GraphicsState};
        let game = Path::new(r"D:\Steam\steamapps\common\Palworld");
        if !game.is_dir() {
            println!("brak gry: {}", game.display());
            return;
        }
        let t = std::time::Instant::now();
        println!("wersja {:?}, available {} ({:?})", game_version(game), available(game), t.elapsed());
        let t = std::time::Instant::now();
        let set = index(game, &|line| {
            if !line.starts_with("scan") {
                println!("  {line}");
            }
        })
        .unwrap();
        println!(
            "index {:?}: shadery {}, root signature {}, potoki {}",
            t.elapsed(),
            set.shaders.len(),
            set.signatures.len(),
            set.pipelines.len()
        );
        for n in &set.notes {
            println!("  {n}");
        }
        let device = super::super::warm::test_device().unwrap();
        let (mut ok, mut bad, mut errors) = (0, 0, Vec::new());
        let step = (set.pipelines.len() / 600).max(1);
        for (k, p) in set.pipelines.iter().enumerate().step_by(step) {
            let load = |i: Option<usize>| i.and_then(|i| set.shaders[i].get()).unwrap_or_default();
            let Some(rs) = p.rs.and_then(|r| set.signatures.get(r)) else { continue };
            let result = device.root_signature(rs).and_then(|rs| {
                if p.stages[5].is_some() {
                    return device.compute(&rs, &load(p.stages[5]));
                }
                let stages = GraphicsStages { vs: load(p.stages[0]), ps: load(p.stages[1]), ..Default::default() };
                let mut state = GraphicsState { depth_format: 40, topology: 3, ..Default::default() };
                if let Some(c) = Container::parse(&stages.ps) {
                    state.render_targets = container::ps_targets(&c).0;
                }
                state.inputs = Container::parse(&stages.vs).map(|v| container::vs_layout(&v)).unwrap_or_default();
                device.graphics(&rs, &stages, &state)
            });
            match result {
                Ok(()) => ok += 1,
                Err(e) => {
                    bad += 1;
                    if errors.len() < 6 {
                        errors.push(format!("#{k} {:?}: {e}", p.stages));
                    }
                }
            }
        }
        println!("próbka na karcie {}: udane {ok}, odrzucone {bad} {errors:#?}", device.adapter);
    }
}
