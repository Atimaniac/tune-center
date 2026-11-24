//! Czytnik zrzutów shaderów Dagor Engine (`compiledShaders\*.shdump.bin`,
//! wersja 11.3).
//!
//! Format na podstawie otwartego kodu DagorEngine (Gaijin Entertainment,
//! licencja BSD-3): `shader_layout.h` i `dag_bindump.h`. Kod jest napisany od
//! nowa w Rust; niczego nie kopiujemy.
//!
//! Plik: 64-bajtowy nagłówek (`VSPS`, `dump`, wersja), potem
//! `CompressedLayout` = rozmiar po rozpakowaniu (u32) i `VecHolder<u8>` ze
//! strumieniem zstd głównego układu `ScriptedShadersBinDump`. W układzie
//! każde pole listy (`VecHolder`, `List`, `Span`) to 8 bajtów: przesunięcie
//! i32 liczone od adresu samego pola i licznik u32. Shadery są osobnymi
//! ramkami zstd kompresowanymi wspólnym słownikiem z pola `dictionary`.

// Etap 2 w budowie: czytnik nie jest jeszcze podpięty do interfejsu.
#![cfg_attr(not(test), allow(dead_code))]

use std::path::Path;

pub const HEADER_SIZE: usize = 64;
const MAGIC1: &[u8; 4] = b"VSPS";
const MAGIC2: &[u8; 4] = b"dump";
pub const SUPPORTED_VERSION: &[u8; 4] = b"11.3";

/// Przesunięcia pól w `ScriptedShadersBinDump` (wszystkie listy 8 B, int 4 B).
mod field {
    pub const VPR_COUNT: usize = 16;
    pub const FSH_COUNT: usize = 20;
    pub const UNCOMPRESSED_SIZES: usize = 172;
    pub const SHADERS_METADATA: usize = 180;
    pub const SHADERS: usize = 188;
    pub const DICTIONARY: usize = 196;
    /// Najmniejszy rozmiar układu, w którym mieszczą się powyższe pola.
    pub const MIN_LAYOUT: usize = 204;
}

#[derive(Debug)]
pub struct Dump {
    layout: Vec<u8>,
    pub vpr_count: u32,
    pub fsh_count: u32,
}

fn err(text: &str) -> String {
    crate::trf!("zrzut shaderów Dagor: {text}", "Dagor shader dump: {text}")
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Lista bindump pod adresem `at`: (początek danych, liczba elementów).
/// Sprawdza, że dane mieszczą się w buforze dla elementów o rozmiarze `item`.
pub fn list_at(b: &[u8], at: usize, item: usize) -> Option<(usize, usize)> {
    let offset = i32_at(b, at)?;
    let count = u32_at(b, at + 4)? as usize;
    if count == 0 {
        return Some((at, 0));
    }
    if offset == 0 {
        return None;
    }
    let start = (at as i64).checked_add(offset as i64)?;
    if start < 0 {
        return None;
    }
    let start = start as usize;
    let end = start.checked_add(count.checked_mul(item)?)?;
    (end <= b.len()).then_some((start, count))
}

/// Sprawdza nagłówek i zwraca (rozmiar po rozpakowaniu, zakres danych zstd).
pub fn parse_header(file: &[u8]) -> Result<(usize, std::ops::Range<usize>), String> {
    if file.len() < HEADER_SIZE + 12 || &file[0..4] != MAGIC1 || &file[4..8] != MAGIC2 {
        return Err(err(crate::i18n::tr(
            "to nie jest zrzut shaderów",
            "not a shader dump",
        )));
    }
    if &file[8..12] != SUPPORTED_VERSION {
        let v = String::from_utf8_lossy(&file[8..12]).into_owned();
        return Err(err(&crate::trf!(
            "nieobsługiwana wersja {v} (obsługiwana 11.3)",
            "unsupported version {v} (11.3 is supported)"
        )));
    }
    let decompressed = u32_at(file, HEADER_SIZE).unwrap_or(0) as usize;
    let (start, count) = list_at(file, HEADER_SIZE + 4, 1)
        .ok_or_else(|| err(crate::i18n::tr("uszkodzony nagłówek", "damaged header")))?;
    Ok((decompressed, start..start + count))
}

impl Dump {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = std::fs::read(path).map_err(|e| err(&e.to_string()))?;
        Self::parse(&file)
    }

    pub fn parse(file: &[u8]) -> Result<Self, String> {
        let (size, data) = parse_header(file)?;
        let layout = crate::zstd::decompress(&file[data], size)
            .map_err(|e| err(&crate::trf!("rozpakowanie: {e}", "decompression: {e}")))?;
        if layout.len() != size || layout.len() < field::MIN_LAYOUT {
            return Err(err(crate::i18n::tr(
                "zły rozmiar układu",
                "wrong layout size",
            )));
        }
        Ok(Self {
            vpr_count: u32_at(&layout, field::VPR_COUNT).unwrap_or(0),
            fsh_count: u32_at(&layout, field::FSH_COUNT).unwrap_or(0),
            layout,
        })
    }

    /// Liczba skompresowanych shaderów.
    pub fn shader_count(&self) -> usize {
        list_at(&self.layout, field::SHADERS, 8).map_or(0, |l| l.1)
    }

    fn sizes(&self) -> Option<(usize, usize)> {
        list_at(&self.layout, field::UNCOMPRESSED_SIZES, 4)
    }

    fn dictionary(&self) -> &[u8] {
        match list_at(&self.layout, field::DICTIONARY, 1) {
            Some((start, count)) => &self.layout[start..start + count],
            None => &[],
        }
    }

    /// Skompresowane bajty i rozmiar po rozpakowaniu shadera `index`.
    fn compressed(&self, index: usize) -> Option<(&[u8], usize)> {
        let (shaders, count) = list_at(&self.layout, field::SHADERS, 8)?;
        if index >= count {
            return None;
        }
        let (start, len) = list_at(&self.layout, shaders + index * 8, 1)?;
        let (sizes, size_count) = self.sizes()?;
        if index >= size_count {
            return None;
        }
        let size = u32_at(&self.layout, sizes + index * 4)? as usize;
        Some((&self.layout[start..start + len], size))
    }

    /// Metadane shadera `index` (dla DX12: dxil::ShaderContainer).
    pub fn metadata(&self, index: usize) -> Option<&[u8]> {
        let (list, count) = list_at(&self.layout, field::SHADERS_METADATA, 8)?;
        if index >= count {
            return None;
        }
        let (start, len) = list_at(&self.layout, list + index * 8, 1)?;
        Some(&self.layout[start..start + len])
    }

    /// Rozpakowuje wszystkie shadery; `each` dostaje (indeks, bajty).
    /// Zwraca liczbę rozpakowanych albo pierwszy błąd.
    pub fn for_each_shader(
        &self,
        mut each: impl FnMut(usize, &[u8]) -> bool,
    ) -> Result<usize, String> {
        let dict = self.dictionary();
        let mut decompressor = if dict.is_empty() {
            Ok(crate::zstd::Decoder::new())
        } else {
            crate::zstd::Decoder::with_dictionary(dict)
        }
        .map_err(|e| err(&e))?;
        let mut done = 0;
        for index in 0..self.shader_count() {
            let (bytes, size) = self.compressed(index).ok_or_else(|| {
                err(&crate::trf!(
                    "brak shadera {index}",
                    "missing shader {index}"
                ))
            })?;
            if bytes.is_empty() {
                continue;
            }
            let data = decompressor
                .decompress(bytes, size)
                .map_err(|e| err(&crate::trf!("shader {index}: {e}", "shader {index}: {e}")))?;
            if data.len() != size {
                return Err(err(&crate::trf!(
                    "shader {index}: zły rozmiar po rozpakowaniu",
                    "shader {index}: wrong decompressed size"
                )));
            }
            done += 1;
            if !each(index, &data) {
                break;
            }
        }
        Ok(done)
    }
}

// --------------------------- metadane DX12 ---------------------------------
//
// `dxil::ShaderContainer`: typ (u16 + u16 z flagą stream output), skrót SHA-1
// (20 B), `VecHolder<u8> data` w 24. Dane to `dxil::Shader` (pojedynczy
// shader) albo `dxil::VertexShaderPipeline` (shader wierzchołków + wskaźniki
// na hull, domain, geometry). `dxil::Shader` = `ShaderHeader` (96 B),
// przesunięcie i rozmiar kodu w rozpakowanym bloku, skompresowane źródło.

/// Etap potoku (`dxil::ShaderStage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Pixel,
    Geometry,
    Domain,
    Hull,
    Compute,
    Mesh,
    Amplification,
    Other(u8),
}

impl Stage {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Stage::Vertex,
            1 => Stage::Pixel,
            2 => Stage::Geometry,
            3 => Stage::Domain,
            4 => Stage::Hull,
            5 => Stage::Compute,
            6 => Stage::Mesh,
            7 => Stage::Amplification,
            n => Stage::Other(n),
        }
    }
}

/// `dxil::ShaderResourceUsageTable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Usage {
    pub t_mask: u32,
    pub s_mask: u32,
    pub bindless_mask: u32,
    pub b_mask: u16,
    pub u_mask: u16,
    pub root_constant_dwords: u8,
    pub special_constants_mask: u8,
}

/// `dxil::ShaderHeader` (96 B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub implicit_cbuf_reg_count: u32,
    pub bones_constants_used: u32,
    pub usage: Usage,
    pub s_compare_mask: u32,
    pub in_out_semantic_mask: u32,
    pub t_types: [u8; 32],
    pub u_types: [u8; 13],
    pub stage: Stage,
    pub input_primitive: u16,
    pub feature_flags_low: u32,
    pub feature_flags_high: u32,
    pub shader_model: u8,
    pub extension_vendor: u8,
    pub vendor_extension_mask: u16,
}

pub const SHADER_HEADER_SIZE: usize = 96;
const SHADER_SIZE: usize = SHADER_HEADER_SIZE + 4 + 4 + 12;
const CONTAINER_DATA: usize = 24;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

pub fn parse_shader_header(b: &[u8], at: usize) -> Option<Header> {
    let h = b.get(at..at + SHADER_HEADER_SIZE)?;
    Some(Header {
        implicit_cbuf_reg_count: u32_at(h, 0)?,
        bones_constants_used: u32_at(h, 4)?,
        usage: Usage {
            t_mask: u32_at(h, 8)?,
            s_mask: u32_at(h, 12)?,
            bindless_mask: u32_at(h, 16)?,
            b_mask: u16_at(h, 20)?,
            u_mask: u16_at(h, 22)?,
            root_constant_dwords: h[24],
            special_constants_mask: h[25],
        },
        s_compare_mask: u32_at(h, 28)?,
        in_out_semantic_mask: u32_at(h, 32)?,
        t_types: h[36..68].try_into().ok()?,
        u_types: h[68..81].try_into().ok()?,
        stage: Stage::from_raw(h[81]),
        input_primitive: u16_at(h, 82)?,
        feature_flags_low: u32_at(h, 84)?,
        feature_flags_high: u32_at(h, 88)?,
        shader_model: h[92],
        extension_vendor: h[93],
        vendor_extension_mask: u16_at(h, 94)?,
    })
}

/// Jeden etap: nagłówek i położenie kodu DXIL w rozpakowanym bloku shadera.
#[derive(Clone, Debug, PartialEq)]
pub struct StageCode {
    pub header: Header,
    pub bytecode: std::ops::Range<usize>,
}

fn parse_stage(b: &[u8], at: usize) -> Option<StageCode> {
    let header = parse_shader_header(b, at)?;
    let offset = u32_at(b, at + SHADER_HEADER_SIZE)? as usize;
    let size = u32_at(b, at + SHADER_HEADER_SIZE + 4)? as usize;
    Some(StageCode {
        header,
        bytecode: offset..offset.checked_add(size)?,
    })
}

/// Wskaźnik bindump (i32 względem pola, 0 = brak).
fn ptr_at(b: &[u8], at: usize) -> Option<Option<usize>> {
    let offset = i32_at(b, at)?;
    if offset == 0 {
        return Some(None);
    }
    let target = at as i64 + offset as i64;
    (target >= 0 && (target as usize) < b.len()).then_some(Some(target as usize))
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // krótkotrwałe wartości, tylko do odczytu
pub enum Container {
    /// Pojedynczy shader (piksele albo compute).
    Single(StageCode),
    /// Shader wierzchołków z opcjonalnymi hull/domain/geometry.
    Vertex {
        vertex: StageCode,
        hull: Option<StageCode>,
        domain: Option<StageCode>,
        geometry: Option<StageCode>,
    },
    /// Stream output, mesh, biblioteki - nie kompilujemy.
    Unsupported(u16),
}

pub fn parse_container(meta: &[u8]) -> Option<Container> {
    let kind = u16_at(meta, 0)?;
    let stream_output = u16_at(meta, 2)? & 1 != 0;
    if stream_output {
        return Some(Container::Unsupported(kind | 0x8000));
    }
    let (data, len) = list_at(meta, CONTAINER_DATA, 1)?;
    match kind {
        0 if len >= SHADER_SIZE => Some(Container::Single(parse_stage(meta, data)?)),
        // VertexShaderPipeline: cztery wskaźniki i32 (wierzchołki, hull,
        // domain, geometry), 0 = brak etapu.
        1 if len >= 16 => {
            let stage = |at: usize| -> Option<Option<StageCode>> {
                match ptr_at(meta, at)? {
                    Some(p) => Some(Some(parse_stage(meta, p)?)),
                    None => Some(None),
                }
            };
            Some(Container::Vertex {
                vertex: stage(data)??,
                hull: stage(data + 4)?,
                domain: stage(data + 8)?,
                geometry: stage(data + 12)?,
            })
        }
        other => Some(Container::Unsupported(other)),
    }
}

impl Dump {
    /// Metadane shadera `index` rozpoznane jako kontener DX12.
    pub fn container(&self, index: usize) -> Option<Container> {
        parse_container(self.metadata(index)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_offsets_are_relative_to_the_field() {
        let mut b = vec![0u8; 32];
        // Pole w 4: dane 12 bajtów dalej (16), 3 elementy po 4 B.
        b[4..8].copy_from_slice(&12i32.to_le_bytes());
        b[8..12].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(list_at(&b, 4, 4), Some((16, 3)));
        // Poza buforem.
        b[8..12].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(list_at(&b, 4, 4), None);
        // Pusta lista.
        assert_eq!(list_at(&[0u8; 8], 0, 4), Some((0, 0)));
    }

    #[test]
    fn rejects_other_files_and_versions() {
        assert!(parse_header(b"not a dump at all, definitely not").is_err());
        let mut f = vec![0u8; 96];
        f[0..4].copy_from_slice(b"VSPS");
        f[4..8].copy_from_slice(b"dump");
        f[8..12].copy_from_slice(b"10.9");
        assert!(parse_header(&f).is_err());
    }

    #[test]
    fn reads_header_and_round_trips_layout() {
        let layout: Vec<u8> = (0..300u32).flat_map(|i| (i % 7).to_le_bytes()).collect();
        let compressed = crate::zstd::raw_frame(&layout);
        let mut f = vec![0u8; HEADER_SIZE];
        f[0..4].copy_from_slice(b"VSPS");
        f[4..8].copy_from_slice(b"dump");
        f[8..12].copy_from_slice(b"11.3");
        f.extend_from_slice(&(layout.len() as u32).to_le_bytes());
        // VecHolder<u8> w 68: dane zaraz za nim (przesunięcie 8 od pola).
        f.extend_from_slice(&8i32.to_le_bytes());
        f.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        f.extend_from_slice(&compressed);
        let (size, range) = parse_header(&f).unwrap();
        assert_eq!(size, layout.len());
        assert_eq!(crate::zstd::decompress(&f[range], size).unwrap(), layout);
    }
}

#[cfg(test)]
mod hardware {
    //! Odczyt prawdziwego zrzutu (tylko odczyt). Ścieżka w zmiennej
    //! TC_DAGOR_DUMP: `cargo test dagor_dump -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn dagor_dump_reads_all_shaders() {
        let path = std::env::var("TC_DAGOR_DUMP").expect("TC_DAGOR_DUMP");
        let t = std::time::Instant::now();
        let dump = Dump::open(Path::new(&path)).unwrap();
        println!(
            "układ: {} B, vpr={}, fsh={}, shaderów={}, rozmiarów={:?}, metadanych={:?}, słownik={} B ({:?})",
            dump.layout.len(),
            dump.vpr_count,
            dump.fsh_count,
            dump.shader_count(),
            dump.sizes().map(|s| s.1),
            list_at(&dump.layout, field::SHADERS_METADATA, 8).map(|l| l.1),
            dump.dictionary().len(),
            t.elapsed()
        );
        let mut total = 0usize;
        let mut first: Vec<String> = Vec::new();
        let n = dump
            .for_each_shader(|i, data| {
                total += data.len();
                if first.len() < 5 {
                    let meta = dump.metadata(i).map(|m| m.len());
                    let head: Vec<String> =
                        data.iter().take(8).map(|b| format!("{b:02X}")).collect();
                    first.push(format!(
                        "#{i}: {} B, metadane {meta:?}, początek {}",
                        data.len(),
                        head.join(" ")
                    ));
                }
                true
            })
            .unwrap();
        println!(
            "rozpakowano {n} shaderów, razem {} MB ({:?})",
            total / 1024 / 1024,
            t.elapsed()
        );
        for line in first {
            println!("  {line}");
        }
        assert_eq!(n, dump.shader_count());

        // Metadane: typy kontenerów, etapy i zakresy kodu DXIL.
        use std::collections::HashMap;
        let mut kinds: HashMap<String, usize> = HashMap::new();
        let mut stages: HashMap<String, usize> = HashMap::new();
        let mut bad = Vec::new();
        let mut bindless = 0;
        let mut root_constants = 0;
        let mut models: HashMap<u8, usize> = HashMap::new();
        dump.for_each_shader(|i, data| {
            let check = |s: &StageCode, bad: &mut Vec<String>| {
                let ok = s.bytecode.end <= data.len()
                    && data.get(s.bytecode.start..s.bytecode.start + 4) == Some(b"DXBC".as_slice());
                if !ok && bad.len() < 5 {
                    bad.push(format!(
                        "#{i} {:?} {:?} / {}",
                        s.header.stage,
                        s.bytecode,
                        data.len()
                    ));
                }
            };
            let mut note = |s: &StageCode| {
                *stages.entry(format!("{:?}", s.header.stage)).or_default() += 1;
                *models.entry(s.header.shader_model).or_default() += 1;
                if s.header.usage.bindless_mask != 0 {
                    bindless += 1;
                }
                if s.header.usage.root_constant_dwords != 0 {
                    root_constants += 1;
                }
            };
            match dump.container(i) {
                Some(Container::Single(s)) => {
                    *kinds.entry("single".into()).or_default() += 1;
                    check(&s, &mut bad);
                    note(&s);
                }
                Some(Container::Vertex {
                    vertex,
                    hull,
                    domain,
                    geometry,
                }) => {
                    *kinds.entry("vertex".into()).or_default() += 1;
                    for s in [Some(vertex), hull, domain, geometry].into_iter().flatten() {
                        check(&s, &mut bad);
                        note(&s);
                    }
                }
                Some(Container::Unsupported(k)) => {
                    *kinds.entry(format!("inne {k:#x}")).or_default() += 1
                }
                None => *kinds.entry("nieczytelne".into()).or_default() += 1,
            }
            true
        })
        .unwrap();
        println!("kontenery: {kinds:?}");
        println!("etapy: {stages:?}");
        println!(
            "shader model: {models:?}, bindless: {bindless}, root constants: {root_constants}"
        );
        println!("złe zakresy kodu: {bad:?}");
        assert!(bad.is_empty());
        assert_eq!(kinds.get("nieczytelne"), None);
    }
}

#[cfg(test)]
mod dx11_probe {
    //! Zrzut Dagor DX11: ile shaderów i kontenerów DXBC (tylko odczyt):
    //! `TC_DAGOR_DUMP=<game.ps50.shdump.bin> cargo test dagor_dx11_probe -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn dagor_dx11_probe() {
        let path = std::env::var("TC_DAGOR_DUMP").expect("TC_DAGOR_DUMP");
        let dump = Dump::open(Path::new(&path)).unwrap();
        let mut stages = std::collections::HashMap::new();
        let mut unique = std::collections::HashSet::new();
        let (mut shaders, mut containers) = (0, 0);
        let mut first = Vec::new();
        dump.for_each_shader(|_, data| {
            shaders += 1;
            if first.len() < 3 {
                first.push(
                    data.iter()
                        .take(16)
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            crate::shaders::unity::carve_dxbc(data, |d| {
                containers += 1;
                if unique.insert(d.digest()) {
                    *stages.entry(format!("{:?}", d.stage)).or_insert(0) += 1;
                }
            });
            true
        })
        .unwrap();
        println!(
            "shaderów {shaders}, DXBC {containers}, unikalne {} {stages:?}",
            unique.len()
        );
        for f in first {
            println!("  {f}");
        }
    }
}
