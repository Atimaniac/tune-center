//! Root signature potoków Dagor Engine (DirectX 12), zbudowana tak jak robi
//! to sterownik DX12 silnika: kolejność parametrów, widoczność etapów,
//! zakresy deskryptorów i flagi muszą być identyczne, żeby skompilowany
//! z góry potok trafił w ten sam wpis pamięci podręcznej sterownika.
//!
//! Odwzorowanie `decode_graphics_root_signature` / `decode_compute_root_signature`
//! (dxil/utility.h) i `RootSignatureGeneratorBase` (dxil/root_signature_generator.h)
//! z otwartego DagorEngine (BSD-3) - napisane od nowa w Rust.

// Etap 2 w budowie: generator nie jest jeszcze podpięty do kompilacji.
#![cfg_attr(not(test), allow(dead_code))]

use super::dagor::Usage;

// Stałe z dxil/compiled_shader_header.h.
const MAX_B_REGISTERS: u32 = 14;
const MAX_T_REGISTERS: u32 = 32;
const MAX_S_REGISTERS: u32 = MAX_T_REGISTERS;
const MAX_U_REGISTERS: u32 = 13;
const ROOT_CONSTANT_BUFFER_REGISTER_INDEX: u32 = 8;
const ROOT_CONSTANT_BUFFER_REGISTER_SPACE_OFFSET: u32 = 1;
const SPECIAL_CONSTANTS_REGISTER_INDEX: u32 = 7;
const DRAW_ID_REGISTER_SPACE: u32 = 1;
const REGULAR_RESOURCES_SPACE_INDEX: u32 = 0;
const BINDLESS_REGISTER_INDEX: u32 = 0;
const BINDLESS_SAMPLERS_SPACE_COUNT: u32 = 2;
const BINDLESS_SAMPLERS_SPACE_OFFSET: u32 = 1;
const BINDLESS_RESOURCES_SPACE_COUNT: u32 = 30;
const BINDLESS_RESOURCES_SPACE_OFFSET: u32 = 1;
const BINDLESS_SAMPLERS_SPACE_BITS_SHIFT: u32 = 0;
const BINDLESS_RESOURCES_SPACE_BITS_SHIFT: u32 =
    BINDLESS_SAMPLERS_SPACE_BITS_SHIFT + BINDLESS_SAMPLERS_SPACE_COUNT;
const BINDLESS_SAMPLERS_SPACE_BITS_MASK: u32 =
    ((1 << BINDLESS_SAMPLERS_SPACE_COUNT) - 1) << BINDLESS_SAMPLERS_SPACE_BITS_SHIFT;
const BINDLESS_RESOURCES_SPACE_BITS_MASK: u32 =
    ((1 << BINDLESS_RESOURCES_SPACE_COUNT) - 1) << BINDLESS_RESOURCES_SPACE_BITS_SHIFT;
const SC_DRAW_ID: u8 = 1 << 0;
const SC_NVIDIA_EXTENSION: u8 = 1 << 1;
const SC_AMD_EXTENSION: u8 = 1 << 2;
const NVIDIA_EXTENSION_SPACE: u32 = 99;
const AMD_EXTENSION_SPACE: u32 = 0x7FFF_0ADE;

/// Wymagania shadera oznaczające indeksowanie sterty deskryptorów (SM 6.6).
pub const SHADER_REQUIRES_RESOURCE_DESCRIPTOR_HEAP_INDEXING: u64 = 0x0200_0000;
pub const SHADER_REQUIRES_SAMPLER_DESCRIPTOR_HEAP_INDEXING: u64 = 0x0400_0000;

// Flagi D3D12_ROOT_SIGNATURE_FLAGS.
pub const FLAG_ALLOW_INPUT_LAYOUT: u32 = 0x1;
pub const FLAG_DENY_VERTEX: u32 = 0x2;
pub const FLAG_DENY_HULL: u32 = 0x4;
pub const FLAG_DENY_DOMAIN: u32 = 0x8;
pub const FLAG_DENY_GEOMETRY: u32 = 0x10;
pub const FLAG_DENY_PIXEL: u32 = 0x20;
pub const FLAG_RESOURCE_HEAP_INDEXED: u32 = 0x400;
pub const FLAG_SAMPLER_HEAP_INDEXED: u32 = 0x800;

/// D3D12_SHADER_VISIBILITY.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vis {
    All = 0,
    Vertex = 1,
    Hull = 2,
    Domain = 3,
    Geometry = 4,
    Pixel = 5,
}

/// D3D12_DESCRIPTOR_RANGE_TYPE.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RangeKind {
    Srv = 0,
    Uav = 1,
    Cbv = 2,
    Sampler = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Range {
    pub kind: RangeKind,
    /// u32::MAX = bez ograniczenia (bindless).
    pub count: u32,
    pub base: u32,
    pub space: u32,
    pub offset: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Param {
    Constants {
        register: u32,
        space: u32,
        dwords: u32,
        vis: Vis,
    },
    Cbv {
        register: u32,
        space: u32,
        vis: Vis,
    },
    Table {
        ranges: Vec<Range>,
        vis: Vis,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct RootSignature {
    pub flags: u32,
    pub params: Vec<Param>,
}

/// Właściwości spoza nagłówków (GraphicsRootSignatureExtraProperties).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Properties {
    pub has_vertex_inputs: bool,
    pub resource_heap_indexing: bool,
    pub sampler_heap_indexing: bool,
}

/// Odpowiednik RootSignatureGeneratorBase - zbiera parametry w kolejności wywołań.
struct Generator {
    sig: RootSignature,
    vis: Vis,
    /// true: bufory stałe jako deskryptory główne (CBV); false: tabela zakresów.
    cbv_root_descriptors: bool,
    /// Zakresy bieżącej tabeli (CBV w trybie tabel, samplery, SRV, UAV).
    pending: Vec<Range>,
    bindless_samplers: Option<usize>,
    bindless_srv: Option<usize>,
    vendor_space: u32,
}

fn any_registers_used(u: &Usage) -> bool {
    (u.b_mask as u32
        | u.s_mask
        | u.t_mask
        | u.u_mask as u32
        | u.bindless_mask
        | u.root_constant_dwords as u32
        | u.special_constants_mask as u32)
        != 0
}

impl Generator {
    fn new(cbv_root_descriptors: bool) -> Self {
        Self {
            sig: RootSignature::default(),
            vis: Vis::All,
            cbv_root_descriptors,
            pending: Vec::new(),
            bindless_samplers: None,
            bindless_srv: None,
            vendor_space: 0,
        }
    }

    fn end(mut self) -> RootSignature {
        if self.vendor_space != 0 {
            self.sig.params.push(Param::Table {
                ranges: vec![Range {
                    kind: RangeKind::Uav,
                    count: 1,
                    base: 0,
                    space: self.vendor_space,
                    offset: 0,
                }],
                vis: Vis::All,
            });
        }
        self.sig
    }

    fn flush_table(&mut self) {
        let ranges = std::mem::take(&mut self.pending);
        self.sig.params.push(Param::Table {
            ranges,
            vis: self.vis,
        });
    }

    fn special_constants(&mut self, mask: u8) {
        if mask & SC_DRAW_ID != 0 {
            self.sig.params.push(Param::Constants {
                register: SPECIAL_CONSTANTS_REGISTER_INDEX,
                space: DRAW_ID_REGISTER_SPACE,
                dwords: 1,
                vis: Vis::All,
            });
        }
        if mask & SC_NVIDIA_EXTENSION != 0 {
            self.vendor_space = NVIDIA_EXTENSION_SPACE;
        }
        if mask & SC_AMD_EXTENSION != 0 {
            self.vendor_space = AMD_EXTENSION_SPACE;
        }
    }

    fn root_constants(&mut self, u: &Usage, vis: Vis) {
        if u.root_constant_dwords > 0 {
            self.vis = vis;
            self.sig.params.push(Param::Constants {
                register: ROOT_CONSTANT_BUFFER_REGISTER_INDEX,
                space: ROOT_CONSTANT_BUFFER_REGISTER_SPACE_OFFSET + u.root_constant_dwords as u32,
                dwords: u.root_constant_dwords as u32,
                vis,
            });
        }
    }

    fn constant_buffers(&mut self, u: &Usage, mut combined: u32, vis: Vis) {
        let mut mask = u.b_mask as u32;
        if mask == 0 {
            return;
        }
        self.vis = vis;
        self.pending.clear();
        let mut linear = 0;
        let mut i = 0;
        while i < MAX_B_REGISTERS && mask != 0 {
            if mask & 1 != 0 {
                if self.cbv_root_descriptors {
                    self.sig.params.push(Param::Cbv {
                        register: i,
                        space: REGULAR_RESOURCES_SPACE_INDEX,
                        vis,
                    });
                } else {
                    self.pending.push(Range {
                        kind: RangeKind::Cbv,
                        count: 1,
                        base: i,
                        space: REGULAR_RESOURCES_SPACE_INDEX,
                        offset: linear,
                    });
                }
            }
            linear += combined & 1;
            mask >>= 1;
            combined >>= 1;
            i += 1;
        }
        if !self.cbv_root_descriptors {
            self.flush_table();
        }
    }

    fn samplers(&mut self, u: &Usage, mut combined: u32, vis: Vis) {
        let mut mask = u.s_mask;
        if mask == 0 {
            return;
        }
        self.vis = vis;
        self.pending.clear();
        let mut linear = 0;
        let mut i = 0;
        while i < MAX_S_REGISTERS && mask != 0 {
            if mask & 1 != 0 {
                self.pending.push(Range {
                    kind: RangeKind::Sampler,
                    count: 1,
                    base: i,
                    space: REGULAR_RESOURCES_SPACE_INDEX,
                    offset: linear,
                });
            }
            linear += combined & 1;
            mask >>= 1;
            combined >>= 1;
            i += 1;
        }
        self.flush_table();
    }

    /// Wspólna część bindless: jeden parametr na wszystkie etapy; przy
    /// drugim etapie widoczność przechodzi na ALL.
    fn bindless(&mut self, slot: Option<usize>, vis: Vis) -> usize {
        match slot {
            None => {
                self.sig.params.push(Param::Table {
                    ranges: Vec::new(),
                    vis,
                });
                self.sig.params.len() - 1
            }
            Some(index) => {
                if let Param::Table { vis: v, .. } = &mut self.sig.params[index] {
                    *v = Vis::All;
                }
                index
            }
        }
    }

    fn bindless_add(&mut self, index: usize, kind: RangeKind, space: u32) {
        if let Param::Table { ranges, .. } = &mut self.sig.params[index] {
            if ranges
                .iter()
                .any(|r| r.base == BINDLESS_REGISTER_INDEX && r.space == space)
            {
                return;
            }
            ranges.push(Range {
                kind,
                count: u32::MAX,
                base: BINDLESS_REGISTER_INDEX,
                space,
                offset: 0,
            });
        }
    }

    fn bindless_samplers(&mut self, u: &Usage, vis: Vis) {
        let mut mask = (u.bindless_mask & BINDLESS_SAMPLERS_SPACE_BITS_MASK)
            >> BINDLESS_SAMPLERS_SPACE_BITS_SHIFT;
        if mask == 0 {
            return;
        }
        self.vis = vis;
        let index = self.bindless(self.bindless_samplers, vis);
        self.bindless_samplers = Some(index);
        let mut i = 0;
        while i < BINDLESS_SAMPLERS_SPACE_COUNT && mask != 0 {
            if mask & 1 != 0 {
                self.bindless_add(
                    index,
                    RangeKind::Sampler,
                    BINDLESS_SAMPLERS_SPACE_OFFSET + i,
                );
            }
            mask >>= 1;
            i += 1;
        }
    }

    fn bindless_srvs(&mut self, u: &Usage, vis: Vis) {
        let mut mask = (u.bindless_mask & BINDLESS_RESOURCES_SPACE_BITS_MASK)
            >> BINDLESS_RESOURCES_SPACE_BITS_SHIFT;
        if mask == 0 {
            return;
        }
        self.vis = vis;
        let index = self.bindless(self.bindless_srv, vis);
        self.bindless_srv = Some(index);
        let mut i = 0;
        while i < BINDLESS_RESOURCES_SPACE_COUNT && mask != 0 {
            if mask & 1 != 0 {
                self.bindless_add(index, RangeKind::Srv, BINDLESS_RESOURCES_SPACE_OFFSET + i);
            }
            mask >>= 1;
            i += 1;
        }
    }

    /// SRV i UAV: kolejne rejestry łączone w jeden zakres.
    fn views(&mut self, mask: u32, mut combined: u32, max: u32, kind: RangeKind, vis: Vis) {
        let mut mask = mask;
        if mask == 0 {
            return;
        }
        self.vis = vis;
        self.pending.clear();
        let (mut last_index, mut count, mut linear, mut last_linear) = (0, 0, 0, 0);
        let mut i = 0;
        // Uwaga: w oryginale pętla ma warunek `i <= MAX` (nie `<`).
        while i <= max && mask != 0 {
            if mask & 1 != 0 {
                if count == 0 {
                    last_index = i;
                    last_linear = linear;
                }
                count += 1;
                if mask & 0b10 == 0 {
                    self.pending.push(Range {
                        kind,
                        count,
                        base: last_index,
                        space: REGULAR_RESOURCES_SPACE_INDEX,
                        offset: last_linear,
                    });
                    count = 0;
                }
            }
            linear += combined & 1;
            mask >>= 1;
            combined >>= 1;
            i += 1;
        }
        self.flush_table();
    }
}

/// Root signature potoku graficznego (VS + PS, opcjonalnie HS/DS/GS).
pub fn graphics(
    props: Properties,
    vs: &Usage,
    ps: &Usage,
    hs: Option<&Usage>,
    ds: Option<&Usage>,
    gs: Option<&Usage>,
    cbv_root_descriptors: bool,
) -> RootSignature {
    let empty = Usage::default();
    let (hs, ds, gs) = (
        hs.unwrap_or(&empty),
        ds.unwrap_or(&empty),
        gs.unwrap_or(&empty),
    );
    let mut g = Generator::new(cbv_root_descriptors);
    let mut flags = 0;
    if props.has_vertex_inputs {
        flags |= FLAG_ALLOW_INPUT_LAYOUT;
    }
    if !any_registers_used(vs) {
        flags |= FLAG_DENY_VERTEX;
    }
    if !any_registers_used(ps) {
        flags |= FLAG_DENY_PIXEL;
    }
    if !any_registers_used(hs) {
        flags |= FLAG_DENY_HULL;
    }
    if !any_registers_used(ds) {
        flags |= FLAG_DENY_DOMAIN;
    }
    if !any_registers_used(gs) {
        flags |= FLAG_DENY_GEOMETRY;
    }
    if props.resource_heap_indexing {
        flags |= FLAG_RESOURCE_HEAP_INDEXED;
    }
    if props.sampler_heap_indexing {
        flags |= FLAG_SAMPLER_HEAP_INDEXED;
    }
    g.sig.flags = flags;

    let stages = [
        (ps, Vis::Pixel),
        (vs, Vis::Vertex),
        (hs, Vis::Hull),
        (ds, Vis::Domain),
        (gs, Vis::Geometry),
    ];
    g.special_constants(
        ps.special_constants_mask
            | vs.special_constants_mask
            | hs.special_constants_mask
            | ds.special_constants_mask
            | gs.special_constants_mask,
    );
    for (u, v) in stages {
        g.root_constants(u, v);
    }
    let combo = vs.b_mask as u32 | hs.b_mask as u32 | ds.b_mask as u32 | gs.b_mask as u32;
    for (u, v) in stages {
        let mask = if v == Vis::Pixel {
            ps.b_mask as u32
        } else {
            combo
        };
        g.constant_buffers(u, mask, v);
    }
    let combo = vs.s_mask | hs.s_mask | ds.s_mask | gs.s_mask;
    for (u, v) in stages {
        g.samplers(u, if v == Vis::Pixel { ps.s_mask } else { combo }, v);
    }
    for (u, v) in stages {
        g.bindless_samplers(u, v);
    }
    let combo = vs.t_mask | hs.t_mask | ds.t_mask | gs.t_mask;
    for (u, v) in stages {
        g.views(
            u.t_mask,
            if v == Vis::Pixel { ps.t_mask } else { combo },
            MAX_T_REGISTERS,
            RangeKind::Srv,
            v,
        );
    }
    for (u, v) in stages {
        g.bindless_srvs(u, v);
    }
    let combo = vs.u_mask as u32 | hs.u_mask as u32 | ds.u_mask as u32 | gs.u_mask as u32;
    for (u, v) in stages {
        let mask = if v == Vis::Pixel {
            ps.u_mask as u32
        } else {
            combo
        };
        g.views(u.u_mask as u32, mask, MAX_U_REGISTERS, RangeKind::Uav, v);
    }
    g.end()
}

/// Root signature potoku obliczeniowego.
pub fn compute(props: Properties, cs: &Usage, cbv_root_descriptors: bool) -> RootSignature {
    let mut g = Generator::new(cbv_root_descriptors);
    if props.resource_heap_indexing {
        g.sig.flags |= FLAG_RESOURCE_HEAP_INDEXED;
    }
    if props.sampler_heap_indexing {
        g.sig.flags |= FLAG_SAMPLER_HEAP_INDEXED;
    }
    g.special_constants(cs.special_constants_mask);
    g.root_constants(cs, Vis::All);
    g.constant_buffers(cs, cs.b_mask as u32, Vis::All);
    g.samplers(cs, cs.s_mask, Vis::All);
    g.bindless_samplers(cs, Vis::All);
    g.views(
        cs.t_mask,
        cs.t_mask,
        MAX_T_REGISTERS,
        RangeKind::Srv,
        Vis::All,
    );
    g.bindless_srvs(cs, Vis::All);
    g.views(
        cs.u_mask as u32,
        cs.u_mask as u32,
        MAX_U_REGISTERS,
        RangeKind::Uav,
        Vis::All,
    );
    g.end()
}

// --------------------------- serializacja D3D12 -----------------------------

/// Serializuje root signature jak silnik: `D3D12SerializeRootSignature`,
/// wersja 1.0. Zwraca bajty (blob) gotowe do `CreateRootSignature`.
#[cfg(windows)]
pub fn serialize(sig: &RootSignature) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;

    let range = |r: &Range| D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(r.kind as i32),
        NumDescriptors: r.count,
        BaseShaderRegister: r.base,
        RegisterSpace: r.space,
        OffsetInDescriptorsFromTableStart: r.offset,
    };
    // Zakresy muszą żyć do końca serializacji.
    let tables: Vec<Vec<D3D12_DESCRIPTOR_RANGE>> = sig
        .params
        .iter()
        .map(|p| match p {
            Param::Table { ranges, .. } => ranges.iter().map(range).collect(),
            _ => Vec::new(),
        })
        .collect();
    let params: Vec<D3D12_ROOT_PARAMETER> = sig
        .params
        .iter()
        .zip(&tables)
        .map(|(p, table)| match p {
            Param::Constants {
                register,
                space,
                dwords,
                vis,
            } => D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: *register,
                        RegisterSpace: *space,
                        Num32BitValues: *dwords,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
            Param::Cbv {
                register,
                space,
                vis,
            } => D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR {
                        ShaderRegister: *register,
                        RegisterSpace: *space,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
            Param::Table { vis, .. } => D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: table.len() as u32,
                        pDescriptorRanges: table.as_ptr(),
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY(*vis as i32),
            },
        })
        .collect();
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: params.len() as u32,
        pParameters: if params.is_empty() {
            std::ptr::null()
        } else {
            params.as_ptr()
        },
        NumStaticSamplers: 0,
        pStaticSamplers: std::ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAGS(sig.flags as i32),
    };
    let mut blob: Option<ID3DBlob> = None;
    let mut error: Option<ID3DBlob> = None;
    // SAFETY: wszystkie wskaźniki w `desc` prowadzą do żyjących wektorów.
    let result = unsafe {
        D3D12SerializeRootSignature(
            &desc,
            D3D_ROOT_SIGNATURE_VERSION_1,
            &mut blob,
            Some(&mut error),
        )
    };
    let bytes = |b: &ID3DBlob| -> Vec<u8> {
        // SAFETY: blob zwrócony przez D3D12; wskaźnik i rozmiar z jego metod.
        unsafe {
            std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())
                .to_vec()
        }
    };
    match (result, blob) {
        (Ok(()), Some(b)) => Ok(bytes(&b)),
        (r, _) => Err(error
            .map(|e| {
                String::from_utf8_lossy(&bytes(&e))
                    .trim_end_matches('\0')
                    .to_owned()
            })
            .unwrap_or_else(|| format!("{r:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(b: u16, t: u32, s: u32) -> Usage {
        Usage {
            b_mask: b,
            t_mask: t,
            s_mask: s,
            ..Default::default()
        }
    }

    #[test]
    fn empty_pipeline_denies_every_stage() {
        let e = Usage::default();
        let sig = graphics(Properties::default(), &e, &e, None, None, None, true);
        assert_eq!(
            sig.flags,
            FLAG_DENY_VERTEX
                | FLAG_DENY_PIXEL
                | FLAG_DENY_HULL
                | FLAG_DENY_DOMAIN
                | FLAG_DENY_GEOMETRY
        );
        assert!(sig.params.is_empty());
    }

    #[test]
    fn graphics_order_is_pixel_first_then_vertex() {
        let vs = usage(0b1, 0, 0);
        let ps = usage(0b10, 0b1011, 0b1);
        let props = Properties {
            has_vertex_inputs: true,
            ..Default::default()
        };
        let sig = graphics(props, &vs, &ps, None, None, None, true);
        assert_eq!(sig.flags & FLAG_ALLOW_INPUT_LAYOUT, FLAG_ALLOW_INPUT_LAYOUT);
        assert_eq!(sig.flags & (FLAG_DENY_VERTEX | FLAG_DENY_PIXEL), 0);
        // CBV PS (b1), CBV VS (b0), samplery PS, SRV PS: t0-t1 razem, t3 osobno.
        assert_eq!(
            sig.params[0],
            Param::Cbv {
                register: 1,
                space: 0,
                vis: Vis::Pixel
            }
        );
        assert_eq!(
            sig.params[1],
            Param::Cbv {
                register: 0,
                space: 0,
                vis: Vis::Vertex
            }
        );
        let Param::Table { ranges, vis } = &sig.params[3] else {
            panic!()
        };
        assert_eq!(*vis, Vis::Pixel);
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].base, ranges[0].count), (0, 2));
        assert_eq!(
            (ranges[1].base, ranges[1].count, ranges[1].offset),
            (3, 1, 2)
        );
    }

    #[test]
    fn cbv_ranges_mode_uses_tables() {
        let vs = usage(0b101, 0, 0);
        let e = Usage::default();
        let sig = graphics(Properties::default(), &vs, &e, None, None, None, false);
        let Param::Table { ranges, vis } = &sig.params[0] else {
            panic!()
        };
        assert_eq!(*vis, Vis::Vertex);
        assert_eq!(
            ranges
                .iter()
                .map(|r| (r.kind, r.base, r.offset))
                .collect::<Vec<_>>(),
            vec![(RangeKind::Cbv, 0, 0), (RangeKind::Cbv, 2, 1)]
        );
    }

    #[test]
    fn bindless_shared_between_stages_becomes_all() {
        let mut vs = Usage::default();
        vs.bindless_mask = 0b100; // przestrzeń SRV 1
        let mut ps = Usage::default();
        ps.bindless_mask = 0b1100; // przestrzenie SRV 1 i 2
        let sig = graphics(Properties::default(), &vs, &ps, None, None, None, true);
        let Param::Table { ranges, vis } = &sig.params[0] else {
            panic!()
        };
        assert_eq!(*vis, Vis::All);
        assert_eq!(ranges.len(), 2); // przestrzeń 1 bez duplikatu
        assert!(ranges
            .iter()
            .all(|r| r.count == u32::MAX && r.kind == RangeKind::Srv));
    }

    #[test]
    fn root_constants_and_nvidia_extension() {
        let mut cs = Usage::default();
        cs.root_constant_dwords = 4;
        cs.special_constants_mask = SC_NVIDIA_EXTENSION | SC_DRAW_ID;
        let sig = compute(Properties::default(), &cs, true);
        assert_eq!(
            sig.params[0],
            Param::Constants {
                register: 7,
                space: 1,
                dwords: 1,
                vis: Vis::All
            }
        );
        assert_eq!(
            sig.params[1],
            Param::Constants {
                register: 8,
                space: 5,
                dwords: 4,
                vis: Vis::All
            }
        );
        let Param::Table { ranges, .. } = sig.params.last().unwrap() else {
            panic!()
        };
        assert_eq!((ranges[0].kind, ranges[0].space), (RangeKind::Uav, 99));
    }

    #[cfg(windows)]
    #[test]
    fn serializes_with_d3d12() {
        let vs = usage(0b1, 0b11, 0b1);
        let ps = usage(0b1, 0b111, 0b11);
        let sig = graphics(
            Properties {
                has_vertex_inputs: true,
                ..Default::default()
            },
            &vs,
            &ps,
            None,
            None,
            None,
            true,
        );
        let blob = serialize(&sig).unwrap();
        assert_eq!(&blob[0..4], b"DXBC");
    }
}
