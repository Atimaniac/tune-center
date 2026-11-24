//! Pamięć potoków Dagor Engine DirectX 12: `<gra>\cache\dx12.cache` (CX12,
//! wersja 34). Gra zapisuje w niej każdy potok graficzny, którego użyła:
//! skróty shaderów wierzchołków i pikseli (SHA-1 kontenerów ze zrzutu
//! shaderów) oraz topologię, układ wejścia, stan renderowania i układ
//! celów renderowania. Plik jest gry, nie sterownika - zostaje po
//! aktualizacji sterownika, więc z niego odtwarzamy potoki.
//!
//! Format na podstawie otwartego DagorEngine (BSD-3, `pipeline_cache.cpp`) -
//! kod napisany od zera.

use std::path::Path;

const MAGIC: &[u8; 4] = b"CX12";
pub const VERSION: u32 = 34;
const HEADER: usize = 152;

pub type Hash = [u8; 20];

/// Wariant potoku graficznego (indeksy do tablic w tym samym pliku).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Variant {
    pub topology: u32,
    pub framebuffer: u32,
    pub input_layout: u32,
    pub wireframe: bool,
    pub static_state: u32,
}

#[derive(Clone, Debug)]
pub struct Graphics {
    pub vs: Hash,
    pub ps: Hash,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Cache {
    pub vendor: u32,
    pub device: u32,
    pub flags: u32,
    pub graphics: Vec<Graphics>,
    pub compute: Vec<Hash>,
    pub static_states: Vec<[u8; 32]>,
    pub input_layouts: Vec<[u8; 64]>,
    pub framebuffers: Vec<[u8; 14]>,
}

struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or("dx12.cache: ucięte dane / truncated data")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn hash(&mut self) -> Result<Hash, String> {
        Ok(self.take(20)?.try_into().unwrap())
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    /// Blob sterownika (u32 długość + bajty) - pomijany.
    fn blob(&mut self) -> Result<(), String> {
        let n = self.u32()? as usize;
        self.take(n).map(|_| ())
    }
}

fn header_u32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

impl Cache {
    pub fn open(path: &Path) -> Result<Cache, String> {
        Self::parse(&std::fs::read(path).map_err(|e| e.to_string())?)
    }

    pub fn parse(file: &[u8]) -> Result<Cache, String> {
        if file.len() < HEADER || &file[..4] != MAGIC {
            return Err("dx12.cache: to nie plik CX12 / not a CX12 file".into());
        }
        let version = header_u32(file, 4);
        if version != VERSION {
            return Err(format!(
                "dx12.cache: wersja {version} nieobsługiwana (obsługiwana {VERSION}) / unsupported version"
            ));
        }
        let h = |i| header_u32(file, i) as usize;
        let (compute, graphics) = (h(36), h(40));
        let (compute_sig, graphics_sig, mesh_sig) = (h(44), h(48), h(52));
        let (states, layouts, framebuffers) = (h(56), h(60), h(64));
        let (compressed, uncompressed) = (h(72), h(76));
        let payload: Vec<u8> = if compressed > 0 {
            let src = file
                .get(HEADER..HEADER + compressed)
                .ok_or("dx12.cache: ucięty plik / truncated file")?;
            crate::zstd::decompress(src, uncompressed)?
        } else {
            file[HEADER..].to_vec()
        };
        let mut c = Cur { b: &payload, at: 0 };
        let mut out = Cache {
            vendor: header_u32(file, 12),
            device: header_u32(file, 16),
            flags: header_u32(file, 28),
            ..Default::default()
        };
        for _ in 0..compute {
            out.compute.push(c.hash()?);
            c.blob()?;
        }
        for _ in 0..graphics {
            let (vs, ps) = (c.hash()?, c.hash()?);
            let count = c.u32()? as usize;
            if count > 1 << 20 {
                return Err("dx12.cache: nierealna liczba wariantów".into());
            }
            let mut variants = Vec::with_capacity(count);
            for _ in 0..count {
                variants.push(Variant {
                    topology: c.u32()?,
                    framebuffer: c.u32()?,
                    input_layout: c.u32()?,
                    wireframe: c.u32()? != 0,
                    static_state: c.u32()?,
                });
                c.blob()?;
            }
            out.graphics.push(Graphics { vs, ps, variants });
        }
        for _ in 0..compute_sig {
            c.take(32)?;
            c.blob()?;
        }
        for _ in 0..graphics_sig + mesh_sig {
            c.take(132)?;
            c.blob()?;
        }
        for _ in 0..states {
            out.static_states.push(c.array()?);
        }
        for _ in 0..layouts {
            out.input_layouts.push(c.array()?);
        }
        for _ in 0..framebuffers {
            out.framebuffers.push(c.array()?);
        }
        Ok(out)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn variant_count(&self) -> usize {
        self.graphics.iter().map(|g| g.variants.len()).sum()
    }
}

// ------------------------ stan Dagora → D3D12 ------------------------------

use super::warm::{Blend, GraphicsState, InputElement};

/// TEXFMT (indeks `TEXFMT_* >> 24`) → (DXGI liniowy, DXGI sRGB).
const FORMATS: [(u32, u32); 43] = [
    (87, 91), (24, 24), (24, 24), (11, 11), (10, 10), (2, 2), (35, 35), (34, 34), (16, 16),
    (54, 54), (41, 41), (71, 72), (74, 75), (77, 78), (17, 17), (84, 84), (56, 56), (65, 65),
    (61, 61), (86, 86), (87, 87), (85, 85), (0, 0), (13, 13), (12, 12), (3, 3), (80, 80),
    (83, 83), (28, 29), (42, 42), (43, 43), (26, 26), (67, 67), (49, 49), (51, 51), (95, 95),
    (98, 99), (62, 62), (57, 57), (45, 45), (55, 55), (40, 40), (20, 20),
];

/// Bajt FormatStore → DXGI_FORMAT celu renderowania / głębi.
pub fn dxgi_format(store: u8) -> u32 {
    let (linear, srgb) = FORMATS.get((store & 0x3F) as usize).copied().unwrap_or((0, 0));
    if store & 0x40 != 0 {
        srgb
    } else {
        linear
    }
}

/// Semantyki wejść Dagora (indeks lokalizacji → nazwa, indeks).
const SEMANTICS: [(&str, u32); 25] = [
    ("POSITION", 0), ("BLENDWEIGHT", 0), ("BLENDINDICES", 0), ("NORMAL", 0), ("PSIZE", 0),
    ("COLOR", 0), ("COLOR", 1), ("TEXCOORD", 0), ("TEXCOORD", 1), ("TEXCOORD", 2),
    ("TEXCOORD", 3), ("TEXCOORD", 4), ("TEXCOORD", 5), ("TEXCOORD", 6), ("TEXCOORD", 7),
    ("POSITION", 1), ("NORMAL", 1), ("TEXCOORD", 15), ("TEXCOORD", 8), ("TEXCOORD", 9),
    ("TEXCOORD", 10), ("TEXCOORD", 11), ("TEXCOORD", 12), ("TEXCOORD", 13), ("TEXCOORD", 14),
];

/// VSDT (`VSDT_* >> 16`) → DXGI_FORMAT (0 = brak).
const VERTEX_FORMATS: [u32; 25] = [
    41, 16, 6, 2, 87, 30, 38, 14, 0, 37, 13, 35, 11, 25, 24, 34, 10, 43, 18, 8, 4, 42, 17, 7, 3,
];

fn u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn bits(v: u32, from: u32, n: u32) -> u32 {
    (v >> from) & ((1 << n) - 1)
}

/// Układ wejścia (64 B).
pub fn input_layout(l: &[u8; 64]) -> Vec<InputElement> {
    let streams = u64::from_le_bytes(l[0..8].try_into().unwrap());
    let step_rate = l[59];
    let mask = u32_le(l, 60);
    let mut out = Vec::new();
    for i in 0..25 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let word = u16::from_le_bytes([l[8 + 2 * i], l[9 + 2 * i]]);
        let slot = ((streams >> (2 * i)) & 3) as u32;
        let format = VERTEX_FORMATS.get((word >> 11) as usize).copied().unwrap_or(0);
        let (name, index) = SEMANTICS[i];
        out.push(InputElement {
            semantic: name.into(),
            index,
            format,
            slot,
            offset: (word & 0x7FF) as u32 * 4,
            instance_rate: (step_rate >> slot & 1) as u32,
        });
    }
    out
}

impl Cache {
    /// Pełny stan potoku dla wariantu (bez shaderów i root signature).
    pub fn state(&self, v: &Variant) -> GraphicsState {
        let mut s = GraphicsState {
            topology: v.topology,
            wireframe: v.wireframe,
            ..Default::default()
        };
        if let Some(l) = self.input_layouts.get(v.input_layout as usize) {
            s.inputs = input_layout(l);
        }
        // Układ celów renderowania (14 B).
        let mut write_masks = [0u8; 8];
        if let Some(fb) = self.framebuffers.get(v.framebuffer as usize) {
            let targets = fb[9];
            let msaa = u16::from_le_bytes([fb[10], fb[11]]);
            let count = 8 - targets.leading_zeros() as usize;
            s.render_targets = (0..count)
                .map(|i| if targets & (1 << i) != 0 { dxgi_format(fb[i]) } else { 0 })
                .collect();
            for (i, m) in write_masks.iter_mut().enumerate() {
                *m = if targets & (1 << i) != 0 { 0xF } else { 0 };
            }
            let level = if targets != 0 {
                (msaa & 3) as u32
            } else {
                fb[13] as u32
            };
            s.samples = 1 << level;
            if fb[12] != 0 {
                s.depth_format = dxgi_format(fb[8]);
            }
        }
        let Some(st) = self.static_states.get(v.static_state as usize) else {
            return s;
        };
        // Stan stały (32 B): pola bitowe od najmłodszego bitu.
        let (a, b) = (u32_le(st, 0), u32_le(st, 4));
        s.depth_enable = a & 1 != 0;
        s.depth_write = a & 2 != 0;
        s.depth_clip = a & 4 != 0;
        s.stencil_enable = a & 0x10 != 0;
        let independent = a & 0x20 != 0;
        s.alpha_to_coverage = a & 0x40 != 0;
        s.depth_func = 1 + bits(a, 7, 3);
        s.forced_samples = (1u32 << bits(a, 10, 3)) >> 1;
        s.conservative = a & (1 << 13) != 0;
        s.cull = match bits(a, 16, 15) {
            0 | 1 => 1,
            3 => 3,
            _ => 2,
        };
        let dual_source = a & (1 << 31) != 0;
        s.stencil_read = bits(b, 0, 8) as u8;
        s.stencil_write = bits(b, 8, 8) as u8;
        s.stencil_func = 1 + bits(b, 16, 3);
        s.stencil_fail = 1 + bits(b, 19, 3);
        s.stencil_depth_fail = 1 + bits(b, 22, 3);
        s.stencil_pass = 1 + bits(b, 25, 7);
        let color_mask = u32_le(st, 8);
        s.depth_bias = (f32::from_le_bytes(st[12..16].try_into().unwrap()) / 2e-5) as i32;
        s.slope_bias = f32::from_le_bytes(st[16..20].try_into().unwrap());
        // Mieszanie: 4 × 3 B (albo jeden rozszerzony zapis przy dwóch źródłach).
        let params: Vec<Blend> = if dual_source {
            let p = &st[20..25];
            vec![Blend {
                enable: p[4] & 0x40 != 0,
                src: 1 + p[0] as u32,
                dst: 1 + p[1] as u32,
                src_alpha: 1 + p[2] as u32,
                dst_alpha: 1 + p[3] as u32,
                op: 1 + (p[4] & 7) as u32,
                op_alpha: 1 + (p[4] >> 3 & 7) as u32,
                mask: 0xF,
            }]
        } else {
            st[20..32]
                .chunks(3)
                .map(|p| Blend {
                    enable: p[2] & 0x40 != 0,
                    src: 1 + (p[0] & 0xF) as u32,
                    dst: 1 + (p[0] >> 4) as u32,
                    src_alpha: 1 + (p[1] & 0xF) as u32,
                    dst_alpha: 1 + (p[1] >> 4) as u32,
                    op: 1 + (p[2] & 7) as u32,
                    op_alpha: 1 + (p[2] >> 3 & 7) as u32,
                    mask: 0xF,
                })
                .collect()
        };
        for (i, rt) in s.blend.iter_mut().enumerate() {
            let p = if independent { params[i.min(params.len() - 1)] } else { params[0] };
            let mask = (color_mask >> (4 * i)) as u8 & 0xF & write_masks[i];
            *rt = Blend {
                enable: p.enable && mask != 0,
                mask,
                ..p
            };
        }
        let masks: Vec<u8> = s
            .blend
            .iter()
            .take(s.render_targets.len().max(1))
            .map(|b| b.mask)
            .collect();
        s.independent_blend =
            !dual_source && (independent || masks.iter().any(|m| *m != masks[0]));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_formats_and_input_layout() {
        assert_eq!(dxgi_format(0x00), 87);
        assert_eq!(dxgi_format(0x40), 91); // sRGB
        assert_eq!(dxgi_format(0x27), 45); // D24S8
        assert_eq!(dxgi_format(0x3F), 0);
        let mut l = [0u8; 64];
        // POSITION0 (lokalizacja 0): FLOAT3, przesunięcie 0; TEXCOORD0 (7): HALF2, 3 dword.
        l[8..10].copy_from_slice(&(2u16 << 11).to_le_bytes());
        l[22..24].copy_from_slice(&((15u16 << 11) | 3).to_le_bytes());
        l[60..64].copy_from_slice(&((1u32 << 0) | (1 << 7)).to_le_bytes());
        let e = input_layout(&l);
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].semantic.as_str(), e[0].format, e[0].offset), ("POSITION", 6, 0));
        assert_eq!((e[1].semantic.as_str(), e[1].index, e[1].format, e[1].offset), ("TEXCOORD", 0, 34, 12));
    }
}

#[cfg(test)]
mod hardware {
    //! Odczyt pamięci potoków gry (tylko odczyt):
    //! `TC_DAGOR_GAME=<folder gry> cargo test dagor_cache_read --release -- --ignored --nocapture`
    use super::*;

    #[test]
    #[ignore]
    fn dagor_cache_read() {
        let dir = std::path::PathBuf::from(std::env::var("TC_DAGOR_GAME").expect("TC_DAGOR_GAME"));
        let file = std::fs::read(dir.join("cache").join("dx12.cache")).unwrap();
        let c = Cache::parse(&file).unwrap();
        println!(
            "karta {:04x}:{:04x} flagi {} potoki VS+PS {} wariantów {} compute {} stany {} układy {} fb {}",
            c.vendor,
            c.device,
            c.flags,
            c.graphics.len(),
            c.variant_count(),
            c.compute.len(),
            c.static_states.len(),
            c.input_layouts.len(),
            c.framebuffers.len()
        );
        let bad = c
            .graphics
            .iter()
            .flat_map(|g| &g.variants)
            .filter(|v| {
                v.static_state as usize >= c.static_states.len()
                    || v.input_layout as usize >= c.input_layouts.len().max(1)
                    || v.framebuffer as usize >= c.framebuffers.len()
            })
            .count();
        println!("warianty z indeksem poza tablicą: {bad}");
        for g in c.graphics.iter().take(3) {
            println!("{:02x?}.. {:02x?}.. {:?}", &g.vs[..4], &g.ps[..4], g.variants.first());
        }
        // Dopasowanie skrótów do zrzutu shaderów DX12.
        let dump = super::super::dagor::Dump::open(
            &dir.join("compiledShaders").join("gameDX12.ps50.shdump.bin"),
        )
        .unwrap();
        let mut kinds = std::collections::HashMap::new();
        for i in 0..dump.shader_count() {
            if let Some(m) = dump.metadata(i) {
                let h: Hash = m[4..24].try_into().unwrap();
                kinds.insert(h, (m[0], i));
            }
        }
        let vs: std::collections::HashSet<_> = c.graphics.iter().map(|g| g.vs).collect();
        let ps: std::collections::HashSet<_> = c.graphics.iter().map(|g| g.ps).collect();
        let hit = |s: &std::collections::HashSet<Hash>| {
            s.iter().filter(|h| kinds.contains_key(*h)).count()
        };
        println!(
            "VS unikalne {} (w zrzucie {}), PS unikalne {} (w zrzucie {}), zrzut {} skrótów",
            vs.len(),
            hit(&vs),
            ps.len(),
            hit(&ps),
            kinds.len()
        );
        let vs_as: std::collections::HashMap<u8, usize> =
            vs.iter().filter_map(|h| kinds.get(h)).fold(Default::default(), |mut m, (k, _)| {
                *m.entry(*k).or_default() += 1;
                m
            });
        println!("typy kontenerów trafionych VS: {vs_as:?}");
        let zero_ps = c.graphics.iter().filter(|g| g.ps == [0; 20]).count();
        println!("PS zerowe: {zero_ps}");
        if let Some(fb) = c.framebuffers.first() {
            println!("fb[0] {fb:02x?}  il[0] {:02x?}", c.input_layouts.first());
        }
    }
}
