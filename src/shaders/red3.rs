//! REDengine 3 (Wiedźmin 3 w DirectX 12): shadery z pamięci shaderów gry w
//! `content\content0`. Tylko odczyt plików gry.
//!
//! - `shaderdx12_0.cache` - shadery materiałów: od początku pliku kolejno
//!   u64 klucz, u8 kompresja (1 = zlib), u32 rozmiar po rozpakowaniu, u32
//!   rozmiar zapisany i dane; po rozpakowaniu: u32 rozmiar kontenera, u32,
//!   kontener DXBC (DXIL w środku) i refleksja silnika. Dalej techniki - każda
//!   to jeden potok i podaje klucze shaderów etapów (VS, PS, GS, HS, DS, CS).
//!   Stopka 48 B: u32 shadery, u32 techniki, u64, u64 położenie technik,
//!   u64 ich rozmiar, u64, "RDHS", u32 wersja (5).
//! - `staticshaderDx12_0.cache` - shadery silnika: u64, u64, u32 n i n bajtów
//!   (jak wyżej, bez kompresji). Stopka 28 B: u32 shadery, 16 B, "RDHS", wersja.
//!
//! Shadery nie mają wbudowanej root signature: silnik używa trzech stałych -
//! compute, potoki z teselacją albo shaderem geometrii, pozostałe potoki
//! graficzne.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::container::read_at;

const MAGIC: u32 = 0x5348_4452; // "RDHS"
const VERSION: u32 = 5;
const MAX_SHADERS: u32 = 200_000;
const MAX_TECHNIQUES: u32 = 2_000_000;
const MAX_STATIC: u32 = 4_000;
const MAX_PAYLOAD: u32 = 4 << 20;

pub const MATERIALS: &str = r"content\content0\shaderdx12_0.cache";
pub const STATIC: &str = r"content\content0\staticshaderDx12_0.cache";

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Położenie shadera w pliku.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub at: u64,
    pub stored: u32,
    pub zlib: bool,
    pub inflated: u32,
}

/// Klucze etapów techniki: VS, PS, GS, HS, DS, CS (0 = brak).
pub type Technique = [u64; 6];

pub struct Materials {
    pub shaders: HashMap<u64, Entry>,
    pub techniques: Vec<Technique>,
}

pub fn materials_path(game: &Path) -> PathBuf {
    game.join(MATERIALS)
}

pub fn static_path(game: &Path) -> PathBuf {
    game.join(STATIC)
}

/// Gra REDengine 3 z pamięcią shaderów DirectX 12 (po stopce pliku).
pub fn detect(game: &Path) -> bool {
    let path = materials_path(game);
    let Ok(file) = std::fs::File::open(&path) else { return false };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut foot = [0u8; 48];
    len >= 56 && read_at(&file, len - 48, &mut foot).is_ok() && u32_at(&foot, 40) == MAGIC && u32_at(&foot, 44) == VERSION
}

/// Odczyt całego pliku materiałów. `None`: inny plik, inna wersja albo dane
/// uszkodzone (każda liczba ograniczona bajtami, które zostały).
pub fn read_materials(bytes: &[u8]) -> Option<Materials> {
    let len = bytes.len();
    if len < 56 {
        return None;
    }
    let foot = &bytes[len - 48..];
    if u32_at(foot, 40) != MAGIC || u32_at(foot, 44) != VERSION {
        return None;
    }
    let (count, techniques) = (u32_at(foot, 0), u32_at(foot, 4));
    let (at, size) = (u64_at(foot, 16) as usize, u64_at(foot, 24) as usize);
    if count == 0 || count > MAX_SHADERS || techniques == 0 || techniques > MAX_TECHNIQUES {
        return None;
    }
    if at > len - 56 || size > len - 56 - at {
        return None;
    }
    let mut shaders = HashMap::with_capacity(count as usize);
    let mut pos = 0usize;
    for _ in 0..count {
        if pos + 17 > at {
            return None;
        }
        let key = u64_at(bytes, pos);
        let zlib = bytes[pos + 8] != 0;
        let inflated = u32_at(bytes, pos + 9);
        let stored = u32_at(bytes, pos + 13);
        pos += 17;
        if stored as usize > at - pos || inflated > MAX_PAYLOAD {
            return None;
        }
        shaders.insert(key, Entry { at: pos as u64, stored, zlib, inflated });
        pos += stored as usize;
    }
    if pos != at {
        return None;
    }
    let end = at + size;
    let mut list = Vec::with_capacity(techniques as usize);
    let mut cur = Cur { b: bytes, pos: at, end };
    for _ in 0..techniques {
        list.push(cur.technique()?);
    }
    (cur.pos == end).then_some(Materials { shaders, techniques: list })
}

struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
    end: usize,
}

impl Cur<'_> {
    fn skip(&mut self, n: usize) -> Option<()> {
        if n > self.end - self.pos {
            return None;
        }
        self.pos += n;
        Some(())
    }
    fn byte(&mut self) -> Option<u8> {
        let b = *self.b.get(self.pos).filter(|_| self.pos < self.end)?;
        self.pos += 1;
        Some(b)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = u32_at(self.b.get(..self.end)?.get(self.pos..self.pos + 4)?, 0);
        self.pos += 4;
        Some(v)
    }
    fn u64(&mut self) -> Option<u64> {
        let v = u64_at(self.b.get(..self.end)?.get(self.pos..self.pos + 8)?, 0);
        self.pos += 8;
        Some(v)
    }
    /// Tekst o zwartej długości: 6 bitów w pierwszym bajcie (bit 6 = dalej),
    /// potem po 7 bitów (bit 7 = dalej). Ścieżka: znaki UTF-16, chyba że bit 7
    /// pierwszego bajtu; nazwa: bajty.
    fn text(&mut self, name: bool) -> Option<()> {
        let first = self.byte()?;
        let mut n = (first & 0x3F) as usize;
        let mut more = first & 0x40 != 0;
        let mut shift = 6;
        while more {
            if shift > 27 {
                return None;
            }
            let x = self.byte()?;
            n |= ((x & 0x7F) as usize) << shift;
            more = x & 0x80 != 0;
            shift += 7;
        }
        self.skip(if name || first & 0x80 != 0 { n } else { 2 * n })
    }
    /// u64 x3, ścieżka, u32, klucze sześciu etapów, dwa klucze ray tracingu,
    /// u64, dwie listy rekordów po 32 B, dwie listy (nazwa, u8).
    fn technique(&mut self) -> Option<Technique> {
        let start = self.pos;
        self.skip(24)?;
        self.text(false)?;
        self.skip(4)?;
        let mut keys = [0u64; 6];
        for k in &mut keys {
            *k = self.u64()?;
        }
        self.skip(16 + 8)?;
        for _ in 0..2 {
            let n = self.u32()? as usize;
            self.skip(n.checked_mul(32)?)?;
        }
        for _ in 0..2 {
            let n = self.u32()? as usize;
            if n.checked_mul(2)? > self.end - self.pos {
                return None;
            }
            for _ in 0..n {
                self.text(true)?;
                self.skip(1)?;
            }
        }
        (self.pos > start).then_some(keys)
    }
}

/// Shadery silnika (plik statyczny).
pub fn read_static(bytes: &[u8]) -> Option<Vec<Entry>> {
    let len = bytes.len();
    if len < 28 {
        return None;
    }
    let foot = &bytes[len - 28..];
    if u32_at(foot, 20) != MAGIC || u32_at(foot, 24) != VERSION {
        return None;
    }
    let n = u32_at(foot, 0);
    if n > MAX_STATIC || n as usize * 28 > len {
        return None;
    }
    let mut out = Vec::with_capacity(n as usize);
    let mut pos = 0usize;
    for _ in 0..n {
        pos += 16;
        let size = u32_at(bytes.get(pos..pos + 4)?, 0);
        pos += 4;
        if !(8..=MAX_PAYLOAD).contains(&size) || size as usize > len - 28 - pos {
            return None;
        }
        out.push(Entry { at: pos as u64, stored: size, zlib: false, inflated: size });
        pos += size as usize;
    }
    (pos == len - 28).then_some(out)
}

/// Kontener DXBC z wpisu (rozpakowany); `None` - wpis pusty albo uszkodzony.
pub fn container(file: &std::fs::File, e: &Entry) -> Option<Vec<u8>> {
    if e.stored > MAX_PAYLOAD || e.inflated > MAX_PAYLOAD {
        return None;
    }
    let mut stored = vec![0u8; e.stored as usize];
    read_at(file, e.at, &mut stored).ok()?;
    let payload = if e.zlib {
        super::inflate::zlib(&stored, e.inflated as usize)?
    } else {
        stored
    };
    if payload.len() < 40 || &payload[8..12] != b"DXBC" {
        return None;
    }
    let size = u32_at(&payload, 0) as usize;
    (size >= 32 && size <= payload.len() - 8).then(|| payload[8..8 + size].to_vec())
}

// ------------------------------ root signature -------------------------------

/// Wiersz opisu: tablica `[0, widoczność, typ zakresu, liczba, rejestr,
/// przestrzeń, flagi]` albo główny deskryptor `[2 (CBV), widoczność, rejestr,
/// przestrzeń, flagi]`.
type Row = &'static [u32];

const COMMON: [Row; 23] = [
    &[2, 5, 0, 0, 2],
    &[2, 5, 1, 0, 2],
    &[2, 5, 2, 0, 2],
    &[2, 5, 3, 0, 2],
    &[2, 5, 4, 0, 2],
    &[2, 1, 0, 0, 2],
    &[2, 1, 1, 0, 2],
    &[2, 1, 2, 0, 2],
    &[2, 1, 3, 0, 2],
    &[2, 1, 4, 0, 2],
    &[0, 5, 2, 11, 5, 0, 1],
    &[0, 5, 0, 8, 0, 0, 1],
    &[0, 5, 0, 11, 8, 0, 1],
    &[0, 5, 0, 13, 19, 0, 1],
    &[0, 5, 0, 31, 32, 0, 1],
    &[0, 5, 3, 8, 0, 0, 0],
    &[0, 5, 3, 8, 8, 0, 0],
    &[0, 1, 2, 11, 5, 0, 1],
    &[0, 1, 0, 8, 0, 0, 1],
    &[0, 1, 0, 56, 8, 0, 1],
    &[0, 1, 3, 8, 0, 0, 0],
    &[0, 1, 3, 8, 8, 0, 0],
    &[0, 0, 1, 4, 0, 0, 1],
];

/// Dodatkowe tablice dla GS (4), HS (2) i DS (3).
const EXTRA_STAGES: [Row; 12] = [
    &[0, 4, 2, 32, 0, 0, 1],
    &[0, 4, 0, 64, 0, 0, 1],
    &[0, 4, 3, 8, 0, 0, 0],
    &[0, 4, 3, 8, 8, 0, 0],
    &[0, 2, 2, 32, 0, 0, 1],
    &[0, 2, 0, 64, 0, 0, 1],
    &[0, 2, 3, 8, 0, 0, 0],
    &[0, 2, 3, 8, 8, 0, 0],
    &[0, 3, 2, 32, 0, 0, 1],
    &[0, 3, 0, 64, 0, 0, 1],
    &[0, 3, 3, 8, 0, 0, 0],
    &[0, 3, 3, 8, 8, 0, 0],
];

const COMPUTE: [Row; 9] = [
    &[2, 0, 0, 0, 2],
    &[0, 0, 2, 15, 1, 0, 1],
    &[0, 0, 0, 8, 0, 0, 1],
    &[0, 0, 0, 11, 8, 0, 1],
    &[0, 0, 0, 13, 19, 0, 1],
    &[0, 0, 0, 31, 32, 0, 1],
    &[0, 0, 1, 16, 0, 0, 1],
    &[0, 0, 3, 8, 0, 0, 0],
    &[0, 0, 3, 8, 8, 0, 0],
];

/// Rodzaj root signature silnika.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Signature {
    Compute,
    /// Z GS, HS lub DS (flagi: układ wejścia, stream output).
    Extended,
    /// VS i PS (flagi: układ wejścia, bez HS, DS i GS).
    Graphics,
}

/// Opis root signature: (flagi, wiersze).
pub fn description(s: Signature) -> (u32, Vec<Row>) {
    match s {
        Signature::Compute => (0, COMPUTE.to_vec()),
        Signature::Extended => (65, COMMON.iter().chain(EXTRA_STAGES.iter()).copied().collect()),
        Signature::Graphics => (29, COMMON.to_vec()),
    }
}

/// Serializuje opis w wersji 1.1 (bez statycznych samplerów).
#[cfg(windows)]
pub fn serialize(flags: u32, rows: &[Row]) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let ranges: Vec<D3D12_DESCRIPTOR_RANGE1> = rows
        .iter()
        .map(|r| match r[0] {
            0 => D3D12_DESCRIPTOR_RANGE1 {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(r[2] as i32),
                NumDescriptors: r[3],
                BaseShaderRegister: r[4],
                RegisterSpace: r[5],
                Flags: D3D12_DESCRIPTOR_RANGE_FLAGS(r[6] as i32),
                OffsetInDescriptorsFromTableStart: u32::MAX,
            },
            _ => D3D12_DESCRIPTOR_RANGE1::default(),
        })
        .collect();
    let params: Vec<D3D12_ROOT_PARAMETER1> = rows
        .iter()
        .zip(&ranges)
        .map(|(r, range)| D3D12_ROOT_PARAMETER1 {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE(r[0] as i32),
            Anonymous: if r[0] == 0 {
                D3D12_ROOT_PARAMETER1_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE1 { NumDescriptorRanges: 1, pDescriptorRanges: range },
                }
            } else {
                D3D12_ROOT_PARAMETER1_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                        ShaderRegister: r[2],
                        RegisterSpace: r[3],
                        Flags: D3D12_ROOT_DESCRIPTOR_FLAGS(r[4] as i32),
                    },
                }
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY(r[1] as i32),
        })
        .collect();
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: params.len() as u32,
                pParameters: params.as_ptr(),
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

/// Root signature potoku z etapami techniki (VS, PS, GS, HS, DS, CS).
pub fn signature_for(t: &Technique) -> Option<Signature> {
    let [vs, _ps, gs, hs, ds, cs] = *t;
    if cs != 0 && vs == 0 {
        return Some(Signature::Compute);
    }
    // D3D12: potok graficzny wymaga VS, a HS tylko razem z DS.
    if vs == 0 || (hs == 0) != (ds == 0) {
        return None;
    }
    Some(if gs != 0 || hs != 0 { Signature::Extended } else { Signature::Graphics })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn technique_bytes(keys: [u64; 8]) -> Vec<u8> {
        let mut b = vec![0u8; 24];
        // Ścieżka UTF-16 "ab" (2 znaki), potem u32.
        b.push(2);
        b.extend_from_slice(&[b'a', 0, b'b', 0]);
        b.extend_from_slice(&[0u8; 4]);
        for k in keys {
            b.extend_from_slice(&k.to_le_bytes());
        }
        b.extend_from_slice(&[0u8; 8]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 32]);
        b.extend_from_slice(&0u32.to_le_bytes());
        // Nazwy: jedna (bajty, długość 3) i bajt.
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[3, b'x', b'y', b'z', 0]);
        b.extend_from_slice(&0u32.to_le_bytes());
        b
    }

    #[test]
    fn reads_material_cache_layout() {
        let mut file = Vec::new();
        // Dwa shadery bez kompresji.
        for (key, data) in [(11u64, vec![1u8; 5]), (22, vec![2u8; 3])] {
            file.extend_from_slice(&key.to_le_bytes());
            file.push(0);
            file.extend_from_slice(&(data.len() as u32).to_le_bytes());
            file.extend_from_slice(&(data.len() as u32).to_le_bytes());
            file.extend_from_slice(&data);
        }
        let at = file.len() as u64;
        let t = technique_bytes([11, 22, 0, 0, 0, 0, 0, 0]);
        file.extend_from_slice(&t);
        let size = t.len() as u64;
        // Dwie krótkie listy przed stopką: n1 = 0, n2 = 0.
        file.extend_from_slice(&[0u8; 8]);
        let mut foot = Vec::new();
        foot.extend_from_slice(&2u32.to_le_bytes());
        foot.extend_from_slice(&1u32.to_le_bytes());
        foot.extend_from_slice(&0u64.to_le_bytes());
        foot.extend_from_slice(&at.to_le_bytes());
        foot.extend_from_slice(&size.to_le_bytes());
        foot.extend_from_slice(&0u64.to_le_bytes());
        foot.extend_from_slice(&MAGIC.to_le_bytes());
        foot.extend_from_slice(&VERSION.to_le_bytes());
        file.extend_from_slice(&foot);
        let m = read_materials(&file).unwrap();
        assert_eq!(m.shaders.len(), 2);
        assert_eq!(m.shaders[&22].at, 17 + 5 + 17);
        assert_eq!(m.techniques, vec![[11, 22, 0, 0, 0, 0]]);
        assert_eq!(signature_for(&m.techniques[0]), Some(Signature::Graphics));
        assert_eq!(signature_for(&[0, 0, 0, 0, 0, 5]), Some(Signature::Compute));
        assert_eq!(signature_for(&[1, 2, 0, 3, 4, 0]), Some(Signature::Extended));
        assert_eq!(signature_for(&[1, 2, 0, 3, 0, 0]), None);
    }
}
