//! Unreal Engine 5, DirectX 12: potoki z pamięci potoków gry
//! (`*.upipelinecache`) - dostarczanej w paczce gry (`*.stable.upipelinecache`)
//! albo zapisanej przez grę w `Saved`. Shadery SM6 bierzemy z archiwów
//! shaderów gry, root signature budujemy tak jak RHI D3D12 silnika.
//!
//! Formaty na podstawie publicznych opisów (CUE4Parse, Apache-2.0; opisy
//! faktów z SCSKiller) - kod napisany od zera. Gry są tylko czytane.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::warm::{Blend, GraphicsState, InputElement};

pub type Hash = [u8; 20];

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

// ------------------------- spakowany shader UE ------------------------------

/// Liczniki zasobów shadera (dane dodatkowe 'p') i cechy z nich wynikające.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub flags: u8,
    pub samplers: u8,
    pub srvs: u8,
    pub cbs: u8,
    pub uavs: u8,
    /// Rozszerzenia AMD (AGS) - dodatkowy główny UAV.
    pub amd_extension: bool,
    /// Cechy z danych 'x' (FShaderCodeFeatures).
    pub features: u16,
}

/// Shader UE: [tablica zasobów UE][kontener DXBC/DXIL][dane dodatkowe].
pub struct Packed<'a> {
    pub container: &'a [u8],
    pub counts: Counts,
}

pub fn parse_packed(b: &[u8]) -> Option<Packed<'_>> {
    // Kontener: pierwsza magia "DXBC", której rozmiar mieści się w danych.
    let mut from = 0;
    let container = loop {
        let at = from + b.get(from..)?.windows(4).position(|w| w == b"DXBC")?;
        let size = le32(b, at + 24)? as usize;
        if size >= 32 && at + size <= b.len() {
            break &b[at..at + size];
        }
        from = at + 4;
    };
    let mut counts = Counts::default();
    let total = le32(b, b.len().checked_sub(4)?)? as usize;
    if (5..=b.len()).contains(&total) {
        let end = b.len() - 4;
        let mut at = b.len() - total;
        while at + 5 <= end {
            let key = b[at];
            let n = le32(b, at + 1)? as usize;
            let data = b.get(at + 5..at + 5 + n)?;
            match key {
                b'p' if n >= 5 => {
                    counts.flags = data[0];
                    counts.samplers = data[1];
                    counts.srvs = data[2];
                    counts.cbs = data[3];
                    counts.uavs = data[4];
                }
                b'x' if n >= 1 => {
                    counts.features = data[0] as u16 | (data.get(1).copied().unwrap_or(0) as u16) << 8;
                    if n == 1 {
                        counts.features &= 0xFF;
                    }
                }
                b'v' if n >= 4 => {
                    let count = le32(data, 0)? as usize;
                    counts.amd_extension = (0..count)
                        .any(|i| le32(data, 4 + i * 15) == Some(0x1002));
                }
                _ => {}
            }
            at += 5 + n;
        }
    }
    Some(Packed { container, counts })
}

// ----------------------------- root signature --------------------------------

/// Etap shadera w potoku UE (z widocznością D3D12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Pixel,
    Vertex,
    Geometry,
    Mesh,
    Amplification,
    Compute,
}

impl Stage {
    fn visibility(self) -> u32 {
        match self {
            Stage::Vertex => 1,
            Stage::Geometry => 4,
            Stage::Pixel => 5,
            Stage::Amplification => 6,
            Stage::Mesh => 7,
            Stage::Compute => 0,
        }
    }
    fn deny(self) -> u32 {
        match self {
            Stage::Vertex => 0x2,
            Stage::Geometry => 0x10,
            Stage::Pixel => 0x20,
            Stage::Amplification => 0x100,
            Stage::Mesh => 0x200,
            Stage::Compute => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    /// Tablica z jednym zakresem: (typ 0 SRV / 1 UAV / 3 sampler, liczba, flagi).
    Table { kind: u32, count: u32, flags: u32, vis: u32 },
    /// Główny deskryptor: (typ 2 CBV / 4 UAV, rejestr, przestrzeń, flagi).
    Root { kind: u32, register: u32, space: u32, flags: u32, vis: u32 },
    Constants { register: u32, space: u32, dwords: u32 },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RootSig {
    pub flags: u32,
    pub params: Vec<Param>,
}

/// Wersja silnika i karty - od nich zależą szczegóły root signature.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    /// Wersja poboczna UE 5.x.
    pub minor: u32,
    /// Karta obsługuje mesh shadery (wtedy UE zabrania etapów AS/MS).
    pub mesh_shaders: bool,
}

/// Root signature potoku (etapy w dowolnej kolejności; tier 3 wiązania).
pub fn root_signature(stages: &[(Stage, Counts)], t: Target) -> RootSig {
    let compute = stages.iter().any(|(s, _)| *s == Stage::Compute);
    let order: &[Stage] = if compute {
        &[Stage::Compute]
    } else {
        &[Stage::Pixel, Stage::Vertex, Stage::Geometry, Stage::Mesh, Stage::Amplification]
    };
    let present: Vec<(Stage, Counts)> = order
        .iter()
        .filter_map(|s| stages.iter().find(|(x, _)| x == s).copied())
        .collect();
    let sampler_count = if t.minor >= 4 { 32 } else { 16 };
    let mut sig = RootSig::default();
    for &(stage, c) in &present {
        let vis = stage.visibility();
        if c.srvs > 0 {
            sig.params.push(Param::Table { kind: 0, count: 64, flags: 5, vis });
        }
        if c.samplers > 0 {
            sig.params.push(Param::Table { kind: 3, count: sampler_count, flags: 1, vis });
        }
        let uav_stage = matches!(stage, Stage::Pixel | Stage::Compute)
            || (stage == Stage::Vertex && t.minor >= 5);
        if c.uavs > 0 && uav_stage {
            sig.params.push(Param::Table { kind: 1, count: 16, flags: 3, vis });
        }
    }
    for &(stage, c) in &present {
        for register in 0..(c.cbs as u32).min(16) {
            sig.params.push(Param::Root { kind: 2, register, space: 0, flags: 8, vis: stage.visibility() });
        }
    }
    let any = |f: &dyn Fn(&Counts) -> bool| present.iter().any(|(_, c)| f(c));
    if any(&|c| c.amd_extension) {
        sig.params.push(Param::Root { kind: 4, register: 0, space: 0x7FFF_0ADE, flags: 2, vis: 0 });
    }
    if t.minor >= 4 && any(&|c| c.flags & 0x08 != 0) {
        sig.params.push(Param::Constants { register: 0, space: 3, dwords: 4 });
    }
    let diagnostic = |c: &Counts| {
        if t.minor >= 5 {
            c.flags & 0x40 != 0
        } else {
            c.features & 0x10 != 0
        }
    };
    if any(&diagnostic) {
        sig.params.push(Param::Root { kind: 4, register: 0, space: 999, flags: 2, vis: 0 });
    }
    if present.iter().any(|(s, _)| *s == Stage::Vertex) {
        sig.flags |= 0x1;
    }
    if any(&|c| c.flags & 0x02 != 0 || c.features & 0x20 != 0) {
        sig.flags |= 0x400;
    }
    if any(&|c| c.flags & 0x04 != 0 || c.features & 0x40 != 0) {
        sig.flags |= 0x800;
    }
    // Zakaz dostępu dla etapów bez powiązań (nieobecnych albo pustych).
    let graphics = [Stage::Vertex, Stage::Geometry, Stage::Pixel, Stage::Amplification, Stage::Mesh];
    for s in graphics {
        if matches!(s, Stage::Amplification | Stage::Mesh) && !t.mesh_shaders {
            continue;
        }
        let used = present.iter().any(|(x, c)| {
            *x == s && (c.srvs | c.samplers | c.uavs | c.cbs) != 0
        });
        if !used {
            sig.flags |= s.deny();
        }
    }
    sig
}

/// Serializuje root signature w wersji 1.1 (6 statycznych samplerów UE).
#[cfg(windows)]
pub fn serialize(sig: &RootSig) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let ranges: Vec<D3D12_DESCRIPTOR_RANGE1> = sig
        .params
        .iter()
        .map(|p| match p {
            Param::Table { kind, count, flags, .. } => D3D12_DESCRIPTOR_RANGE1 {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(*kind as i32),
                NumDescriptors: *count,
                BaseShaderRegister: 0,
                RegisterSpace: 0,
                Flags: D3D12_DESCRIPTOR_RANGE_FLAGS(*flags as i32),
                OffsetInDescriptorsFromTableStart: u32::MAX,
            },
            _ => D3D12_DESCRIPTOR_RANGE1::default(),
        })
        .collect();
    let params: Vec<D3D12_ROOT_PARAMETER1> = sig
        .params
        .iter()
        .zip(&ranges)
        .map(|(p, range)| match p {
            Param::Table { vis, .. } => D3D12_ROOT_PARAMETER1 {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER1_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE1 {
                        NumDescriptorRanges: 1,
                        pDescriptorRanges: range,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
            Param::Root { kind, register, space, flags, vis } => D3D12_ROOT_PARAMETER1 {
                ParameterType: if *kind == 2 {
                    D3D12_ROOT_PARAMETER_TYPE_CBV
                } else {
                    D3D12_ROOT_PARAMETER_TYPE_UAV
                },
                Anonymous: D3D12_ROOT_PARAMETER1_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                        ShaderRegister: *register,
                        RegisterSpace: *space,
                        Flags: D3D12_ROOT_DESCRIPTOR_FLAGS(*flags as i32),
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
            Param::Constants { register, space, dwords } => D3D12_ROOT_PARAMETER1 {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER1_0 {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: *register,
                        RegisterSpace: *space,
                        Num32BitValues: *dwords,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        })
        .collect();
    // PointWrap, PointClamp, BilinearWrap, BilinearClamp, TrilinearWrap, TrilinearClamp.
    let samplers: Vec<D3D12_STATIC_SAMPLER_DESC> = [(0, 1), (0, 3), (0x14, 1), (0x14, 3), (0x15, 1), (0x15, 3)]
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
            ShaderRegister: i as u32,
            RegisterSpace: 1000,
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        })
        .collect();
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: params.len() as u32,
                pParameters: if params.is_empty() { std::ptr::null() } else { params.as_ptr() },
                NumStaticSamplers: samplers.len() as u32,
                pStaticSamplers: samplers.as_ptr(),
                Flags: D3D12_ROOT_SIGNATURE_FLAGS(sig.flags as i32),
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
        (r, _) => Err(error
            .map(|e| String::from_utf8_lossy(&bytes(&e)).trim_end_matches('\0').to_owned())
            .unwrap_or_else(|| format!("{r:?}"))),
    }
}

// -------------------------- pamięć potoków ----------------------------------

const MAGIC: u64 = 0x5049_5045_4341_4348;
const TOC_MAGIC: u64 = 0x544F_4353_5441_5232;
const TOC_MAGIC_OLD: u64 = 0x544F_4353_5441_5254;

/// Potok graficzny z pamięci potoków (wartości jak w UE).
#[derive(Clone, Debug, PartialEq)]
pub struct GraphicsPso {
    pub vs: Hash,
    pub ps: Hash,
    pub gs: Hash,
    pub ms: Hash,
    pub amp: Hash,
    /// (strumień, przesunięcie, typ, atrybut, na instancję).
    pub vertex: Vec<(u8, u8, u8, u8, bool)>,
    /// Na cel: (op koloru, źródło, cel, op alfy, źródło, cel, maska).
    pub blend: [[u8; 7]; 8],
    pub independent_blend: bool,
    pub alpha_to_coverage: bool,
    pub depth_bias: f32,
    pub slope_bias: f32,
    pub fill: u8,
    pub cull: u8,
    pub depth_clip: u8,
    pub depth_write: bool,
    pub depth_test: u8,
    pub stencil: bool,
    /// Przód: test, porażka, porażka głębi, sukces.
    pub stencil_front: [u8; 4],
    pub stencil_read: u8,
    pub stencil_write: u8,
    /// (EPixelFormat, flagi tekstury) celów renderowania.
    pub targets: [(u32, u64); 8],
    pub targets_active: u32,
    pub samples: u32,
    pub primitive: u32,
    pub depth_format: u32,
}

#[derive(Debug, Default)]
pub struct PsoCache {
    pub version: u32,
    pub platform: u8,
    pub graphics: Vec<GraphicsPso>,
    pub compute: Vec<Hash>,
    /// Wpisy pominięte (ray tracing, nieznany układ).
    pub skipped: usize,
}

struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or("upipelinecache: ucięte dane / truncated data")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn boolean(&mut self) -> Result<bool, String> {
        Ok(self.u32()? != 0)
    }
    fn hash(&mut self) -> Result<Hash, String> {
        Ok(self.take(20)?.try_into().unwrap())
    }
}

impl PsoCache {
    pub fn parse(b: &[u8]) -> Result<PsoCache, String> {
        let mut c = Cur { b, at: 0 };
        if c.u64()? != MAGIC {
            return Err("to nie plik .upipelinecache / not a pipeline cache".into());
        }
        let version = c.u32()?;
        if !(25..=28).contains(&version) && version != 30 {
            return Err(format!(
                "upipelinecache: wersja {version} nieobsługiwana (obsługiwane 25-28, 30) / unsupported version"
            ));
        }
        c.u32()?; // wersja gry
        let platform = c.u8()?;
        c.take(16)?;
        let toc = c.u64()? as usize;
        let mut out = PsoCache { version, platform, ..Default::default() };
        let mut t = Cur { b, at: toc };
        let magic = t.u64()?;
        if magic != TOC_MAGIC && magic != TOC_MAGIC_OLD {
            return Err("upipelinecache: brak spisu / missing TOC".into());
        }
        if t.u8()? != 0 {
            t.take(16)?;
        }
        t.u32()?; // kolejność
        let count = t.u32()? as usize;
        if count > 10_000_000 {
            return Err("upipelinecache: nierealna liczba wpisów".into());
        }
        let mut bodies = Vec::with_capacity(count);
        for _ in 0..count {
            t.u32()?; // klucz
            let offset = t.u64()? as usize;
            let size = t.u64()? as usize;
            t.take(16)?; // GUID
            t.take(36)?; // statystyki
            let n = t.u32()? as usize;
            t.take(n.checked_mul(20).ok_or("upipelinecache: zła lista")?)?;
            t.u64()?; // maska użycia
            t.u16()?; // flagi silnika
            t.u64()?; // czas użycia
            bodies.push((offset, size));
        }
        for (offset, size) in bodies {
            let Some(body) = b.get(offset..offset.saturating_add(size)) else {
                out.skipped += 1;
                continue;
            };
            match parse_pso(body, version) {
                Ok(Some(Pso::Graphics(g))) => out.graphics.push(g),
                Ok(Some(Pso::Compute(h))) => out.compute.push(h),
                _ => out.skipped += 1,
            }
        }
        Ok(out)
    }
}

enum Pso {
    Graphics(GraphicsPso),
    Compute(Hash),
}

fn parse_pso(b: &[u8], version: u32) -> Result<Option<Pso>, String> {
    let mut c = Cur { b, at: 0 };
    match c.u32()? {
        0 => return Ok(Some(Pso::Compute(c.hash()?))),
        1 => {}
        _ => return Ok(None),
    }
    if version >= 30 {
        c.take(4)?;
    }
    let (vs, ps, gs, ms, amp) = (c.hash()?, c.hash()?, c.hash()?, c.hash()?, c.hash()?);
    let n = c.u32()? as usize;
    if n > 64 {
        return Err("upipelinecache: za dużo elementów wierzchołka".into());
    }
    let mut vertex = Vec::with_capacity(n);
    for _ in 0..n {
        let (stream, offset, kind, attribute) = (c.u8()?, c.u8()?, c.u8()?, c.u8()?);
        c.u16()?; // krok
        let instance = c.u16()? != 0;
        vertex.push((stream, offset, kind, attribute, instance));
    }
    let mut blend = [[0u8; 7]; 8];
    for rt in &mut blend {
        rt.copy_from_slice(c.take(7)?);
    }
    let independent_blend = c.boolean()?;
    let alpha_to_coverage = c.boolean()?;
    let depth_bias = c.f32()?;
    let slope_bias = c.f32()?;
    let fill = c.u8()?;
    let cull = c.u8()?;
    let depth_clip = c.u8()?;
    c.boolean()?; // MSAA
    if version < 27 {
        c.boolean()?; // linie AA
    }
    let depth_write = c.boolean()?;
    let depth_test = c.u8()?;
    let front = c.boolean()?;
    let stencil_front: [u8; 4] = c.take(4)?.try_into().unwrap();
    let back = c.boolean()?;
    c.take(4)?;
    let stencil_read = c.u8()?;
    let stencil_write = c.u8()?;
    let mut targets = [(0u32, 0u64); 8];
    for t in &mut targets {
        let format = c.u32()?;
        let flags = c.u64()?;
        c.take(2)?;
        *t = (format, flags);
    }
    let targets_active = c.u32()?;
    let samples = c.u32()?;
    let primitive = c.u32()?;
    let depth_format = c.u32()?;
    Ok(Some(Pso::Graphics(GraphicsPso {
        vs,
        ps,
        gs,
        ms,
        amp,
        vertex,
        blend,
        independent_blend,
        alpha_to_coverage,
        depth_bias,
        slope_bias,
        fill,
        cull,
        depth_clip,
        depth_write,
        depth_test,
        stencil: front || back,
        stencil_front,
        stencil_read,
        stencil_write,
        targets,
        targets_active,
        samples,
        primitive,
        depth_format,
    })))
}

// ----------------------------- stan → D3D12 ---------------------------------

/// EPixelFormat → DXGI_FORMAT widoku celu renderowania (sRGB z flagi).
pub fn target_format(format: u32, srgb: bool) -> u32 {
    match format {
        1 => 2,
        2 => if srgb { 91 } else { 87 },
        3 | 71 => 61,
        4 => 56,
        9 | 26 => 26,
        10 => 10,
        13 => 41,
        14 => 35,
        15 | 16 => 34,
        17 => 16,
        18 => 24,
        19 => 11,
        21 | 22 => 54,
        27 => 65,
        28 => 42,
        29 => 43,
        32 => 57,
        33 => 59,
        34 => 12,
        35 => 14,
        37 => if srgb { 29 } else { 28 },
        40 => 49,
        48 => 3,
        49 => 36,
        57 => 62,
        60 => 30,
        61 => 31,
        62 => 11,
        63 => 13,
        68 | 84 => 17,
        78 => 37,
        79 => 50,
        92 => 38,
        _ => 0,
    }
}

/// EPixelFormat bufora głębi → DXGI_FORMAT widoku głębi.
pub fn depth_format(format: u32) -> u32 {
    match format {
        11 => 20,
        12 => 55,
        20 => 45,
        _ => 0,
    }
}

/// EVertexElementType → DXGI_FORMAT.
fn vertex_format(kind: u8) -> u32 {
    const T: [u32; 21] = [0, 41, 16, 6, 2, 31, 30, 28, 87, 38, 14, 37, 34, 10, 13, 36, 12, 35, 11, 24, 42];
    T.get(kind as usize).copied().unwrap_or(0)
}

fn blend_factor(f: u8) -> u32 {
    match f {
        0..=9 => f as u32 + 1,
        10 => 14,
        11 => 15,
        12..=15 => f as u32 + 4,
        _ => 2,
    }
}

fn blend_op(o: u8) -> u32 {
    [1, 2, 4, 5, 3].get(o as usize).copied().unwrap_or(1)
}

fn compare(f: u8) -> u32 {
    [2, 4, 5, 7, 3, 6, 1, 8].get(f as usize).copied().unwrap_or(8)
}

impl GraphicsPso {
    pub fn state(&self) -> GraphicsState {
        let mut s = GraphicsState {
            inputs: self
                .vertex
                .iter()
                .map(|&(stream, offset, kind, attribute, instance)| InputElement {
                    semantic: "ATTRIBUTE".into(),
                    index: attribute as u32,
                    format: vertex_format(kind),
                    slot: stream as u32,
                    offset: offset as u32,
                    instance_rate: instance as u32,
                })
                .collect(),
            samples: self.samples.max(1),
            depth_format: depth_format(self.depth_format),
            topology: match self.primitive {
                2 => 2,
                4 => 1,
                0..=5 => 3,
                _ => 4,
            },
            wireframe: self.fill == 1,
            cull: match self.cull {
                1 => 3,
                2 => 2,
                _ => 1,
            },
            front_ccw: true,
            depth_bias: (self.depth_bias * (1u32 << 24) as f32).floor() as i32,
            slope_bias: self.slope_bias,
            depth_clip: self.depth_clip == 0,
            depth_write: self.depth_write,
            depth_enable: self.depth_test != 7 || self.depth_write,
            depth_func: compare(self.depth_test),
            stencil_enable: self.stencil,
            stencil_read: self.stencil_read,
            stencil_write: self.stencil_write,
            stencil_func: compare(self.stencil_front[0]),
            stencil_fail: self.stencil_front[1] as u32 + 1,
            stencil_depth_fail: self.stencil_front[2] as u32 + 1,
            stencil_pass: self.stencil_front[3] as u32 + 1,
            alpha_to_coverage: self.alpha_to_coverage,
            independent_blend: self.independent_blend,
            ..Default::default()
        };
        let active = (self.targets_active as usize).min(8);
        s.render_targets = self.targets[..active]
            .iter()
            .map(|&(f, flags)| target_format(f, flags & (1 << 4) != 0))
            .collect();
        for (rt, b) in s.blend.iter_mut().zip(&self.blend) {
            let off = b[0] == 0 && b[1] == 1 && b[2] == 0 && b[3] == 0 && b[4] == 1 && b[5] == 0;
            *rt = Blend {
                enable: !off,
                op: blend_op(b[0]),
                src: blend_factor(b[1]),
                dst: blend_factor(b[2]),
                op_alpha: blend_op(b[3]),
                src_alpha: blend_factor(b[4]),
                dst_alpha: blend_factor(b[5]),
                mask: b[6] & 0xF,
            };
        }
        s
    }
}

// ------------------------------ pliki gry ------------------------------------

/// Wersja poboczna UE 5.x z pliku .exe gry (tekst „++UE5+Release-5.N”).
pub fn engine_minor(exe: &Path) -> Option<u32> {
    let bytes = std::fs::read(exe).ok()?;
    let tag = "++UE5+Release-5.";
    // Tekst bywa ASCII albo UTF-16.
    let wide: Vec<u8> = tag.bytes().flat_map(|b| [b, 0]).collect();
    for (pattern, step) in [(tag.as_bytes().to_vec(), 1), (wide, 2)] {
        let Some(at) = bytes.windows(pattern.len()).position(|w| w == pattern.as_slice()) else {
            continue;
        };
        let digits: String = bytes[at + pattern.len()..]
            .iter()
            .step_by(step)
            .take(3)
            .take_while(|b| b.is_ascii_digit())
            .map(|&b| b as char)
            .collect();
        if let Ok(n) = digits.parse() {
            return Some(n);
        }
    }
    None
}

/// Wersja poboczna UE 5.x gry (z jej pliku *-Shipping.exe).
pub fn game_minor(game: &Path) -> Option<u32> {
    let exe = super::warm::unreal_game_exe(game)?;
    std::fs::read_dir(game)
        .ok()?
        .flatten()
        .map(|e| e.path().join("Binaries").join("Win64").join(&exe))
        .find(|p| p.is_file())
        .and_then(|p| engine_minor(&p))
}

/// Wszystkie jawne paczki `.pak` gry.
pub fn paks(game: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(game.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 5 {
                    stack.push((p, depth + 1));
                }
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pak")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Czy gra ma pamięć potoków SM6 (same nazwy plików - szybko).
pub fn has_pso_cache(game: &Path) -> bool {
    let sm6 = |name: &str| {
        let l = name.to_lowercase();
        l.ends_with(".upipelinecache") && l.contains("sm6")
    };
    let mut projects = HashSet::new();
    for pak in paks(game) {
        if let Some(project) = pak.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            projects.insert(project.file_name().unwrap_or_default().to_string_lossy().into_owned());
        }
        if super::unreal_pak::Pak::open(&pak)
            .is_ok_and(|p| !p.encrypted_index && p.files.iter().any(|e| sm6(&e.path)))
        {
            return true;
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    projects.iter().any(|project| {
        let mut dirs = vec![game.join(project).join("Saved")];
        if let Some(l) = &local {
            dirs.push(l.join(project).join("Saved"));
        }
        dirs.iter().any(|d| {
            std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| sm6(&e.file_name().to_string_lossy()))
        })
    })
}

/// Pamięci potoków SM6 gry: z paczek (`*.upipelinecache`) i z folderów
/// `Saved` (w folderze gry i w `%LOCALAPPDATA%\<Projekt>`). (nazwa, bajty).
pub fn pso_caches(game: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut projects = HashSet::new();
    for pak in paks(game) {
        if let Some(project) = pak
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
        {
            projects.insert(project.to_string_lossy().into_owned());
        }
        let Ok(p) = super::unreal_pak::Pak::open(&pak) else { continue };
        if p.encrypted_index {
            continue;
        }
        for e in p.files.iter().filter(|e| {
            let l = e.path.to_lowercase();
            l.ends_with(".upipelinecache") && l.contains("sm6")
        }) {
            if let Ok(bytes) = p.read(e, None) {
                out.push((e.path.clone(), bytes));
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    for project in projects.iter().filter(|p| p.as_str() != "Engine") {
        let mut dirs = vec![game.join(project).join("Saved")];
        if let Some(l) = &local {
            dirs.push(l.join(project).join("Saved"));
        }
        for dir in dirs {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let l = name.to_lowercase();
                if l.ends_with(".upipelinecache") && l.contains("sm6") {
                    if let Ok(bytes) = std::fs::read(e.path()) {
                        out.push((e.path().display().to_string(), bytes));
                    }
                }
            }
        }
    }
    out
}

/// Kod shaderów SM6 gry po skrócie: archiwa `.ushaderbytecode` z paczek i
/// biblioteki IoStore. Wczytuje tylko shadery z `wanted`.
pub fn load_shaders(game: &Path, wanted: &HashSet<Hash>, notes: &mut Vec<String>) -> HashMap<Hash, Vec<u8>> {
    let mut out = HashMap::new();
    for pak in paks(game) {
        let Ok(p) = super::unreal_pak::Pak::open(&pak) else { continue };
        if p.encrypted_index {
            continue;
        }
        for e in p.files.iter().filter(|e| {
            let l = e.path.to_lowercase();
            l.ends_with(".ushaderbytecode") && l.contains("sm6")
        }) {
            let bytes = match p.read(e, None) {
                Ok(b) => b,
                Err(err) => {
                    notes.push(err);
                    continue;
                }
            };
            let arch = match super::unreal_pak::Archive::parse(&bytes) {
                Ok(a) => a,
                Err(err) => {
                    notes.push(format!("{}: {err}", e.path));
                    continue;
                }
            };
            for (i, h) in arch.hashes.iter().enumerate() {
                if wanted.contains(h) && !out.contains_key(h) {
                    if let Ok(code) = arch.code(&bytes, i, None) {
                        out.insert(*h, code);
                    }
                }
            }
        }
    }
    let io = super::unreal::IoShaders::open(game);
    for (l, (_, lib)) in io.libs.iter().enumerate() {
        // Grupy z choć jednym potrzebnym shaderem.
        let mut groups: HashSet<usize> = HashSet::new();
        for (i, h) in lib.shader_hashes.iter().enumerate() {
            if wanted.contains(h) && !out.contains_key(h) {
                if let Some(s) = lib.shaders.get(i) {
                    groups.insert(s.group as usize);
                }
            }
        }
        for g in groups {
            let Ok(data) = io.group(l, g) else { continue };
            for (i, code) in lib.split(g, &data) {
                if let Some(h) = lib.shader_hashes.get(i as usize) {
                    if wanted.contains(h) {
                        out.entry(*h).or_insert_with(|| code.to_vec());
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_packed_shader_trailer() {
        let mut b = vec![1, 2, 3];
        let mut dxbc = b"DXBC".to_vec();
        dxbc.resize(32, 0);
        dxbc[24..28].copy_from_slice(&32u32.to_le_bytes());
        b.extend(&dxbc);
        let start = b.len();
        b.push(b'p');
        b.extend(5u32.to_le_bytes());
        b.extend([0, 4, 20, 4, 1]);
        b.push(b'x');
        b.extend(1u32.to_le_bytes());
        b.push(0x10);
        let total = (b.len() - start + 4) as u32;
        b.extend(total.to_le_bytes());
        let p = parse_packed(&b).unwrap();
        assert_eq!(p.container.len(), 32);
        assert_eq!((p.counts.samplers, p.counts.srvs, p.counts.cbs, p.counts.uavs), (4, 20, 4, 1));
        assert_eq!(p.counts.features, 0x10);
    }

    #[test]
    fn root_signature_order_and_flags() {
        let ps = Counts { samplers: 2, srvs: 3, cbs: 2, uavs: 1, ..Default::default() };
        let vs = Counts { srvs: 1, cbs: 1, uavs: 1, ..Default::default() };
        let t = Target { minor: 3, mesh_shaders: true };
        let sig = root_signature(&[(Stage::Vertex, vs), (Stage::Pixel, ps)], t);
        assert_eq!(
            sig.params,
            vec![
                Param::Table { kind: 0, count: 64, flags: 5, vis: 5 },
                Param::Table { kind: 3, count: 16, flags: 1, vis: 5 },
                Param::Table { kind: 1, count: 16, flags: 3, vis: 5 },
                Param::Table { kind: 0, count: 64, flags: 5, vis: 1 },
                Param::Root { kind: 2, register: 0, space: 0, flags: 8, vis: 5 },
                Param::Root { kind: 2, register: 1, space: 0, flags: 8, vis: 5 },
                Param::Root { kind: 2, register: 0, space: 0, flags: 8, vis: 1 },
            ]
        );
        // Wejście + zakazy GS, AS, MS.
        assert_eq!(sig.flags, 0x1 | 0x10 | 0x100 | 0x200);
        let cs = root_signature(&[(Stage::Compute, ps)], Target { minor: 4, mesh_shaders: false });
        assert_eq!(cs.flags, 0x2 | 0x10 | 0x20);
        assert!(matches!(cs.params[1], Param::Table { kind: 3, count: 32, .. }));
    }
}

#[cfg(test)]
mod synth {
    //! Testowa pamięć potoków (wersja 25) z shaderami wierzchołków gry, do
    //! sprawdzenia całej ścieżki: `TC_UNREAL_GAME=<folder gry>
    //! TC_PSO_OUT=<plik> cargo test ue12_write_synthetic --release -- --ignored --nocapture`
    use super::*;

    fn pso_body(vs: &Hash, inputs: &[(u32, u32)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(1u32.to_le_bytes());
        b.extend(vs);
        b.extend([0u8; 80]); // PS, GS, MS, AS
        b.extend((inputs.len() as u32).to_le_bytes());
        for (i, &(attribute, comps)) in inputs.iter().enumerate() {
            // Strumień na atrybut, przesunięcie 0, FloatN.
            b.extend([i as u8, 0, comps as u8, attribute as u8]);
            b.extend((comps as u16 * 4).to_le_bytes());
            b.extend(0u16.to_le_bytes());
        }
        for _ in 0..8 {
            b.extend([0, 1, 0, 0, 1, 0, 15]);
        }
        b.extend([0u8; 8]); // niezależne mieszanie, A2C
        b.extend(0f32.to_le_bytes());
        b.extend(0f32.to_le_bytes());
        b.extend([2, 1, 0]); // solid, CW, depth clip
        b.extend([0u8; 8]); // MSAA, linie AA
        b.extend(1u32.to_le_bytes()); // zapis głębi
        b.push(3); // GreaterEqual
        b.extend([0u8; 4]);
        b.extend([7, 0, 0, 0]);
        b.extend([0u8; 4]);
        b.extend([7, 0, 0, 0]);
        b.extend([0xFF, 0xFF]);
        for _ in 0..8 {
            b.extend(0u32.to_le_bytes());
            b.extend(0u64.to_le_bytes());
            b.extend([0, 0]);
        }
        b.extend(0u32.to_le_bytes()); // bez celów koloru
        b.extend(1u32.to_le_bytes());
        b.extend(0u32.to_le_bytes()); // lista trójkątów
        b.extend(11u32.to_le_bytes()); // DepthStencil
        b.extend(0u64.to_le_bytes());
        b.extend([0u8; 7]);
        b.extend([0u8; 4]);
        b
    }

    #[test]
    #[ignore]
    fn ue12_write_synthetic() {
        let game = PathBuf::from(std::env::var("TC_UNREAL_GAME").expect("TC_UNREAL_GAME"));
        let out = PathBuf::from(std::env::var("TC_PSO_OUT").expect("TC_PSO_OUT"));
        let pak = paks(&game)
            .into_iter()
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .unwrap();
        let p = super::super::unreal_pak::Pak::open(&pak).unwrap();
        let e = p
            .files
            .iter()
            .find(|e| {
                let l = e.path.to_lowercase();
                l.ends_with(".ushaderbytecode") && l.contains("sm6") && !l.contains("global")
            })
            .unwrap();
        let bytes = p.read(e, None).unwrap();
        let arch = super::super::unreal_pak::Archive::parse(&bytes).unwrap();
        let mut bodies = Vec::new();
        for i in (0..arch.shaders.len()).filter(|&i| arch.shaders[i].frequency == 0) {
            let code = arch.code(&bytes, i, None).unwrap();
            let Some(packed) = parse_packed(&code) else { continue };
            let inputs: Vec<(u32, u32)> = super::super::unity::input_signature(packed.container)
                .iter()
                .map(|el| (el.index, el.count))
                .collect();
            bodies.push((arch.hashes[i], pso_body(&arch.hashes[i], &inputs)));
            if bodies.len() == 200 {
                break;
            }
        }
        let mut f = Vec::new();
        f.extend(MAGIC.to_le_bytes());
        f.extend(25u32.to_le_bytes());
        f.extend(1u32.to_le_bytes());
        f.push(49);
        f.extend([0u8; 16]);
        f.extend(0u64.to_le_bytes()); // spis - niżej
        f.extend(0i64.to_le_bytes());
        let mut toc_entries = Vec::new();
        for (hash, body) in &bodies {
            toc_entries.push((f.len() as u64, body.len() as u64, *hash));
            f.extend(body);
        }
        let toc = f.len() as u64;
        f[33..41].copy_from_slice(&toc.to_le_bytes());
        f.extend(TOC_MAGIC.to_le_bytes());
        f.push(0);
        f.extend(0u32.to_le_bytes());
        f.extend((toc_entries.len() as u32).to_le_bytes());
        for (i, (offset, size, hash)) in toc_entries.iter().enumerate() {
            f.extend((i as u32).to_le_bytes());
            f.extend(offset.to_le_bytes());
            f.extend(size.to_le_bytes());
            f.extend([0u8; 16 + 36]);
            f.extend(1u32.to_le_bytes());
            f.extend(hash);
            f.extend(0u64.to_le_bytes());
            f.extend(0u16.to_le_bytes());
            f.extend(0i64.to_le_bytes());
        }
        f.extend(0x454F_462D_4D41_524Bu64.to_le_bytes());
        let parsed = PsoCache::parse(&f).unwrap();
        assert_eq!(parsed.graphics.len(), bodies.len());
        assert_eq!(parsed.graphics[0].vs, bodies[0].0);
        std::fs::write(&out, &f).unwrap();
        println!("zapisano {} potoków do {}", bodies.len(), out.display());
    }
}

#[cfg(test)]
mod hardware {
    //! Sprawdzenie root signature na shaderach compute gry UE5 (tworzy
    //! potoki w procesie testu): `TC_UNREAL_GAME=<folder gry>
    //! cargo test ue12_compute_check --release -- --ignored --nocapture`
    use super::*;

    #[test]
    #[ignore]
    fn ue12_compute_check() {
        let game = PathBuf::from(std::env::var("TC_UNREAL_GAME").expect("TC_UNREAL_GAME"));
        let exe = super::super::warm::unreal_game_exe(&game);
        let minor = exe
            .as_ref()
            .and_then(|n| {
                paks(&game).first().and_then(|p| {
                    let project = p.parent()?.parent()?.parent()?;
                    engine_minor(&project.join("Binaries").join("Win64").join(n))
                })
            });
        println!("exe {exe:?}, UE 5.{minor:?}");
        for (name, bytes) in pso_caches(&game) {
            match PsoCache::parse(&bytes) {
                Ok(c) => println!(
                    "{name}: v{} platforma {} graficzne {} compute {} pominięte {}",
                    c.version,
                    c.platform,
                    c.graphics.len(),
                    c.compute.len(),
                    c.skipped
                ),
                Err(e) => println!("{name}: {e}"),
            }
        }
        // Compute z archiwum SM6: root signature UE + utworzenie potoku.
        let pak = paks(&game).into_iter().max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).unwrap();
        let p = super::super::unreal_pak::Pak::open(&pak).unwrap();
        let e = p
            .files
            .iter()
            .find(|e| {
                let l = e.path.to_lowercase();
                l.ends_with(".ushaderbytecode") && l.contains("sm6") && !l.contains("global")
            })
            .unwrap();
        let bytes = p.read(e, None).unwrap();
        let arch = super::super::unreal_pak::Archive::parse(&bytes).unwrap();
        let device = super::super::warm::test_device().unwrap();
        let t = Target { minor: minor.unwrap_or(1), mesh_shaders: true };
        let (mut ok, mut bad, mut first) = (0, 0, Vec::new());
        for i in (0..arch.shaders.len()).filter(|&i| arch.shaders[i].frequency == 5).take(300) {
            let code = arch.code(&bytes, i, None).unwrap();
            let Some(packed) = parse_packed(&code) else { continue };
            let sig = root_signature(&[(Stage::Compute, packed.counts)], t);
            match serialize(&sig).and_then(|b| device.compute_with(&b, packed.container)) {
                Ok(()) => ok += 1,
                Err(err) => {
                    bad += 1;
                    if first.len() < 5 {
                        first.push(format!("#{i} {:?}: {err}", packed.counts));
                    }
                }
            }
        }
        println!("compute: udane {ok}, odrzucone {bad} {first:#?}");
    }
}
