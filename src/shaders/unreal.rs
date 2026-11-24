//! Unreal Engine 5: kontenery IoStore (`*.utoc` + `*.ucas`) i archiwum
//! shaderów (`FIoStoreShaderCodeArchive`).
//!
//! Format na podstawie open-source CUE4Parse (tylko odniesienie, kod własny):
//! - `.utoc`: nagłówek 144 B (magia „-==--==--==--==-”), potem tablice:
//!   identyfikatory porcji (12 B), przesunięcie+długość (2 × 40 bitów BE),
//!   ziarna haszu doskonałego, porcje bez haszu, bloki kompresji (12 B),
//!   nazwy metod kompresji (stała szerokość z nagłówka).
//! - Porcje są w przestrzeni nieskompresowanej, podzielone na bloki
//!   `CompressionBlockSize`; blok na dysku wyrównany do 16 B (AES).
//! - Biblioteka shaderów = porcja typu 8 (ShaderCodeLibrary): wersja 1,
//!   skróty map, skróty shaderów, identyfikatory porcji grup, wpisy map,
//!   wpisy shaderów (u64: częstotliwość 4 b, grupa 30 b, przesunięcie 30 b),
//!   wpisy grup (przesunięcie indeksów, liczba, rozmiar nieskompr., skompr.),
//!   indeksy. Grupa = osobna porcja; gdy rozmiary się różnią, grupa jest
//!   dodatkowo skompresowana (domyślnie Oodle).
//! - Oodle nie jest open source: ładujemy `oo2core_*_win64.dll` z folderu gry
//!   (nie dołączamy go do programu). Gra z Oodle wkompilowanym w exe bez
//!   tej biblioteki nie jest obsługiwana.
//! - Paczki szyfrowane (AES) - nieobsługiwane (klucz jest w exe gry).

// IoStore: na razie tylko wykrywanie szyfrowania; reszta czeka na jawną grę UE5 z .utoc.
#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 16] = b"-==--==--==--==-";
const HEADER: usize = 144;

const FLAG_COMPRESSED: u8 = 0x01;
const FLAG_ENCRYPTED: u8 = 0x02;
const FLAG_SIGNED: u8 = 0x04;
const FLAG_INDEXED: u8 = 0x08;

/// Typy porcji UE5 (bajt 11 identyfikatora).
pub const CHUNK_SHADER_LIBRARY: u8 = 8;
pub const CHUNK_SHADER_CODE: u8 = 9;

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn be40(b: &[u8]) -> u64 {
    b.iter().take(5).fold(0u64, |a, &x| (a << 8) | x as u64)
}

#[derive(Clone, Copy, Debug)]
struct Block {
    offset: u64,
    compressed: u32,
    uncompressed: u32,
    method: u8,
}

/// Kontener IoStore.
pub struct Toc {
    pub path: PathBuf,
    pub version: u8,
    pub flags: u8,
    block_size: u64,
    partition_size: u64,
    pub ids: Vec<[u8; 12]>,
    spans: Vec<(u64, u64)>,
    blocks: Vec<Block>,
    /// Nazwy metod; indeks 0 = brak kompresji, nazwa i → metoda i+1.
    pub methods: Vec<String>,
}

impl Toc {
    pub fn open(path: &Path) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if data.len() < HEADER || &data[..16] != MAGIC {
            return Err(format!("{}: to nie jest plik .utoc", path.display()));
        }
        let version = data[16];
        let entries = u32_at(&data, 0x18) as usize;
        let block_count = u32_at(&data, 0x1C) as usize;
        let block_entry = u32_at(&data, 0x20) as usize;
        let name_count = u32_at(&data, 0x24) as usize;
        let name_len = u32_at(&data, 0x28) as usize;
        let block_size = u32_at(&data, 0x2C) as u64;
        let flags = data[0x50];
        let seeds = u32_at(&data, 0x54) as usize;
        let partition_size = if version < 3 {
            u64::MAX
        } else {
            u64_at(&data, 0x58)
        };
        let overflow = if version >= 5 {
            u32_at(&data, 0x60) as usize
        } else {
            0
        };
        if block_entry != 12 {
            return Err(format!("nieznany rozmiar wpisu bloku: {block_entry}"));
        }

        let need = |o: usize, n: usize| -> Result<(), String> {
            if o + n > data.len() {
                Err("uszkodzony plik .utoc (za krótki)".into())
            } else {
                Ok(())
            }
        };
        let mut o = HEADER;
        need(o, entries * 22)?;
        let ids: Vec<[u8; 12]> = (0..entries)
            .map(|i| data[o + i * 12..o + i * 12 + 12].try_into().unwrap())
            .collect();
        o += entries * 12;
        let spans: Vec<(u64, u64)> = (0..entries)
            .map(|i| {
                let e = &data[o + i * 10..o + i * 10 + 10];
                (be40(&e[..5]), be40(&e[5..]))
            })
            .collect();
        o += entries * 10;
        if version >= 4 {
            o += seeds * 4;
        }
        o += overflow * 4;
        need(o, block_count * 12)?;
        let blocks: Vec<Block> = (0..block_count)
            .map(|i| {
                let e = &data[o + i * 12..o + i * 12 + 12];
                let a = u64_at(e, 0);
                let b = u32_at(e, 8);
                Block {
                    offset: a & 0xFF_FFFF_FFFF,
                    compressed: (a >> 40) as u32,
                    uncompressed: b & 0xFF_FFFF,
                    method: (b >> 24) as u8,
                }
            })
            .collect();
        o += block_count * 12;
        // Wektory IV (AES-CTR) występują tylko przy szyfrowaniu - wtedy i tak kończymy.
        need(o, name_count * name_len)?;
        let methods = (0..name_count)
            .map(|i| {
                let raw = &data[o + i * name_len..o + (i + 1) * name_len];
                let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                String::from_utf8_lossy(&raw[..end]).into_owned()
            })
            .collect();
        let _ = (FLAG_SIGNED, FLAG_INDEXED, FLAG_COMPRESSED);
        Ok(Self {
            path: path.to_path_buf(),
            version,
            flags,
            block_size,
            partition_size,
            ids,
            spans,
            blocks,
            methods,
        })
    }

    /// Szyfrowanie: flaga kontenera, zaszyfrowany indeks sąsiedniego `.pak`
    /// albo tablice bez sensu (gry szyfrujące TOC bez ustawiania flagi,
    /// np. The Finals - bloki daleko poza rozmiarem plików `.ucas`).
    pub fn encrypted(&self) -> bool {
        self.flags & FLAG_ENCRYPTED != 0
            || pak_index_encrypted(&self.path.with_extension("pak"))
            || self.scrambled()
    }

    fn scrambled(&self) -> bool {
        let size: u64 = (0..8)
            .map(|i| self.partition(i))
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        self.blocks.iter().take(64).any(|b| {
            b.offset > size
                || b.uncompressed as u64 > self.block_size
                || (b.method as usize) > self.methods.len()
        })
    }

    pub fn chunk_type(&self, i: usize) -> u8 {
        self.ids[i][11]
    }

    pub fn find(&self, id: &[u8; 12]) -> Option<usize> {
        self.ids.iter().position(|x| x == id)
    }

    fn partition(&self, index: u64) -> PathBuf {
        let stem = self.path.with_extension("");
        if index == 0 {
            stem.with_extension("ucas")
        } else {
            PathBuf::from(format!("{}_s{index}.ucas", stem.display()))
        }
    }

    /// Nazwa metody kompresji bloku (`None` = bez kompresji).
    fn method_name(&self, m: u8) -> Option<&str> {
        (m != 0).then(|| {
            self.methods
                .get(m as usize - 1)
                .map(|s| s.as_str())
                .unwrap_or("?")
        })
    }

    /// Zawartość porcji (rozpakowana).
    pub fn read(&self, index: usize, oodle: Option<&Oodle>) -> Result<Vec<u8>, String> {
        if self.encrypted() {
            return Err("kontener szyfrowany (AES) - nieobsługiwany".into());
        }
        let (offset, length) = self.spans[index];
        let mut out = Vec::with_capacity(length as usize);
        if length == 0 {
            return Ok(out);
        }
        let first = offset / self.block_size;
        let last = (offset + length - 1) / self.block_size;
        let mut skip = (offset % self.block_size) as usize;
        let mut files: Vec<(u64, File)> = Vec::new();
        for bi in first..=last {
            let b = *self
                .blocks
                .get(bi as usize)
                .ok_or("blok poza tablicą bloków")?;
            let part = b.offset / self.partition_size.max(1);
            let pos = b.offset % self.partition_size.max(1);
            let file = match files.iter_mut().find(|(p, _)| *p == part) {
                Some((_, f)) => f,
                None => {
                    let p = self.partition(part);
                    let f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                    files.push((part, f));
                    &mut files.last_mut().unwrap().1
                }
            };
            let raw_size = (b.compressed as usize + 15) & !15;
            let mut raw = vec![0u8; raw_size];
            file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
            // Ostatni blok pliku może nie mieć pełnego wyrównania.
            let got = read_up_to(file, &mut raw).map_err(|e| e.to_string())?;
            if got < b.compressed as usize {
                return Err("blok ucinany na końcu pliku .ucas".into());
            }
            let plain = match self.method_name(b.method) {
                None => raw[..b.uncompressed as usize].to_vec(),
                Some(m) => decompress(
                    m,
                    &raw[..b.compressed as usize],
                    b.uncompressed as usize,
                    oodle,
                )?,
            };
            let remaining = length as usize - out.len();
            let take = (plain.len() - skip).min(remaining);
            out.extend_from_slice(&plain[skip..skip + take]);
            skip = 0;
        }
        Ok(out)
    }
}

/// Stopka `.pak` (wersje 8-12): magia 0x5A6F12E1 w ostatnich bajtach, bajt
/// przed nią = indeks zaszyfrowany.
pub fn pak_index_encrypted(pak: &Path) -> bool {
    let Ok(mut f) = File::open(pak) else {
        return false;
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return false;
    };
    let tail = len.min(512);
    let mut buf = vec![0u8; tail as usize];
    if f.seek(SeekFrom::Start(len - tail)).is_err() || read_up_to(&mut f, &mut buf).is_err() {
        return false;
    }
    buf.windows(4)
        .rposition(|w| w == [0xE1, 0x12, 0x6F, 0x5A])
        .is_some_and(|i| i > 0 && buf[i - 1] == 1)
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Rozpakowanie bloku albo grupy shaderów.
pub fn decompress(
    method: &str,
    src: &[u8],
    size: usize,
    oodle: Option<&Oodle>,
) -> Result<Vec<u8>, String> {
    match method.to_ascii_lowercase().as_str() {
        "oodle" => match oodle {
            Some(o) => o.decompress(src, size),
            // Bez biblioteki z gry: otwarty dekompresor (crate oozextract).
            None => {
                // Jeden dekompresor na wątek (bufory po kilkaset KB - bez
                // przydzielania ich dla każdego z dziesiątek tysięcy shaderów).
                thread_local! {
                    static OOZ: std::cell::RefCell<oozextract::Extractor> =
                        std::cell::RefCell::new(oozextract::Extractor::new());
                }
                let mut out = vec![0u8; size];
                let n = OOZ
                    .with(|x| x.borrow_mut().read_from_slice(src, &mut out))
                    .map_err(|e| format!("Oodle: {e:?}"))?;
                if n != size {
                    return Err(format!("Oodle: rozpakowano {n} z {size} B"));
                }
                Ok(out)
            }
        },
        "lz4" => crate::lz4::decompress(src, size).map_err(|e| format!("LZ4: {e}")),
        "zstd" => crate::zstd::decompress(src, size),
        other => Err(format!("nieobsługiwana kompresja: {other}")),
    }
}

/// Biblioteka Oodle z folderu gry.
pub struct Oodle {
    _lib: crate::dll::Library,
    decompress: unsafe extern "C" fn(
        *const u8,
        isize,
        *mut u8,
        isize,
        i32,
        i32,
        i32,
        *mut u8,
        isize,
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        isize,
        i32,
    ) -> isize,
}

impl Oodle {
    /// Szuka `oo2core_*_win64.dll` w folderze gry (do 6 poziomów w głąb).
    pub fn find(game: &Path) -> Option<PathBuf> {
        fn walk(dir: &Path, depth: u32) -> Option<PathBuf> {
            let list = std::fs::read_dir(dir).ok()?;
            let mut dirs = Vec::new();
            for e in list.flatten() {
                let p = e.path();
                let n = e.file_name().to_string_lossy().to_lowercase();
                if p.is_dir() {
                    dirs.push(p);
                } else if n.starts_with("oo2core") && n.ends_with("win64.dll") {
                    return Some(p);
                }
            }
            if depth == 0 {
                return None;
            }
            dirs.into_iter().find_map(|d| walk(&d, depth - 1))
        }
        walk(game, 6)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        unsafe {
            let lib =
                crate::dll::Library::new(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let f = *lib
                .get(b"OodleLZ_Decompress\0")
                .map_err(|e| format!("OodleLZ_Decompress: {e}"))?;
            Ok(Self {
                _lib: lib,
                decompress: f,
            })
        }
    }

    pub fn decompress(&self, src: &[u8], size: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; size];
        let n = unsafe {
            (self.decompress)(
                src.as_ptr(),
                src.len() as isize,
                out.as_mut_ptr(),
                size as isize,
                1, // fuzz safe
                0,
                0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                3,
            )
        };
        if n as usize != size {
            return Err(format!("Oodle: rozpakowano {n} z {size} B"));
        }
        Ok(out)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ShaderEntry {
    pub frequency: u8,
    pub group: u32,
    pub offset: u32,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub chunk: [u8; 12],
    pub shaders: Vec<u32>,
    pub uncompressed: u32,
    pub compressed: u32,
}

/// Archiwum shaderów IoStore (porcja ShaderCodeLibrary).
#[derive(Debug, Default)]
pub struct Library {
    pub shader_hashes: Vec<[u8; 20]>,
    pub shaders: Vec<ShaderEntry>,
    pub groups: Vec<Group>,
}

struct Cur<'a> {
    b: &'a [u8],
    o: usize,
}

impl Cur<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.o + n > self.b.len() {
            return Err("archiwum shaderów: za krótkie".into());
        }
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn count(&mut self, item: usize) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n.saturating_mul(item) > self.b.len() {
            return Err(format!("archiwum shaderów: nierealna liczba wpisów {n}"));
        }
        Ok(n)
    }
}

impl Library {
    pub fn parse(b: &[u8]) -> Result<Self, String> {
        let mut c = Cur { b, o: 0 };
        let version = c.u32()?;
        if version != 1 {
            return Err(format!(
                "archiwum shaderów w wersji {version} (obsługiwana 1 - IoStore)"
            ));
        }
        let n = c.count(20)?;
        c.take(n * 20)?; // skróty map shaderów
        let n = c.count(20)?;
        let shader_hashes = (0..n)
            .map(|_| c.take(20).map(|s| s.try_into().unwrap()))
            .collect::<Result<Vec<[u8; 20]>, _>>()?;
        let n = c.count(12)?;
        let chunks = (0..n)
            .map(|_| c.take(12).map(|s| <[u8; 12]>::try_from(s).unwrap()))
            .collect::<Result<Vec<_>, _>>()?;
        let n = c.count(8)?;
        c.take(n * 8)?; // wpisy map shaderów
        let n = c.count(8)?;
        let shaders = (0..n)
            .map(|_| {
                let p = u64::from_le_bytes(c.take(8)?.try_into().unwrap());
                Ok(ShaderEntry {
                    frequency: (p & 0xF) as u8,
                    group: ((p >> 4) & 0x3FFF_FFFF) as u32,
                    offset: ((p >> 34) & 0x3FFF_FFFF) as u32,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let n = c.count(16)?;
        let raw_groups = (0..n)
            .map(|_| Ok((c.u32()?, c.u32()?, c.u32()?, c.u32()?)))
            .collect::<Result<Vec<_>, String>>()?;
        let n = c.count(4)?;
        let indices = (0..n).map(|_| c.u32()).collect::<Result<Vec<_>, _>>()?;
        if raw_groups.len() != chunks.len() {
            return Err(format!(
                "archiwum shaderów: {} grup, {} porcji grup",
                raw_groups.len(),
                chunks.len()
            ));
        }
        let groups = raw_groups
            .into_iter()
            .zip(chunks)
            .map(|((first, count, uncompressed, compressed), chunk)| Group {
                chunk,
                shaders: indices
                    .get(first as usize..(first + count) as usize)
                    .unwrap_or(&[])
                    .to_vec(),
                uncompressed,
                compressed,
            })
            .collect();
        Ok(Self {
            shader_hashes,
            shaders,
            groups,
        })
    }

    /// Shadery grupy: (indeks shadera, bajty) - rozmiar = do następnego
    /// przesunięcia w grupie albo do końca grupy.
    pub fn split<'a>(&self, group: usize, data: &'a [u8]) -> Vec<(u32, &'a [u8])> {
        let mut list: Vec<(u32, u32)> = self.groups[group]
            .shaders
            .iter()
            .filter_map(|&s| self.shaders.get(s as usize).map(|e| (s, e.offset)))
            .collect();
        list.sort_by_key(|&(_, o)| o);
        let mut out = Vec::new();
        for (i, &(s, o)) in list.iter().enumerate() {
            let end = list
                .get(i + 1)
                .map(|&(_, e)| e)
                .unwrap_or(data.len() as u32);
            if (o as usize) < data.len() && end >= o {
                out.push((s, &data[o as usize..(end as usize).min(data.len())]));
            }
        }
        out
    }
}

/// Kontener DXBC/DXIL w bajtach shadera UE (UE dokleja własne nagłówki i dane):
/// od magii „DXBC” na długość z nagłówka kontenera (u32 pod +24).
pub fn container(code: &[u8]) -> Option<&[u8]> {
    let start = code.windows(4).position(|w| w == b"DXBC")?;
    let size = u32::from_le_bytes(code.get(start + 24..start + 28)?.try_into().ok()?) as usize;
    code.get(start..start + size)
}

/// Pliki `.utoc` gry (Content\Paks i Engine\...\Paks).
pub fn tocs(game: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(list) = std::fs::read_dir(dir) else {
            return;
        };
        for e in list.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth > 0 {
                    walk(&p, depth - 1, out);
                }
            } else if p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("utoc"))
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(game, 5, &mut out);
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_length_and_entry() {
        assert_eq!(be40(&[0, 0, 0, 1, 2]), 0x102);
        let p: u64 = 3 | (7 << 4) | (100u64 << 34);
        let e = ShaderEntry {
            frequency: (p & 0xF) as u8,
            group: ((p >> 4) & 0x3FFF_FFFF) as u32,
            offset: ((p >> 34) & 0x3FFF_FFFF) as u32,
        };
        assert_eq!((e.frequency, e.group, e.offset), (3, 7, 100));
    }

    /// Przegląd gry UE5: `TC_UNREAL_GAME=<folder gry> cargo test unreal_scan -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn unreal_scan() {
        let game = PathBuf::from(std::env::var("TC_UNREAL_GAME").expect("TC_UNREAL_GAME"));
        let oodle_path = Oodle::find(&game);
        println!("oodle: {oodle_path:?}");
        let oodle = oodle_path.and_then(|p| Oodle::load(&p).map_err(|e| println!("{e}")).ok());
        for path in tocs(&game) {
            let toc = match Toc::open(&path) {
                Ok(t) => t,
                Err(e) => {
                    println!("{e}");
                    continue;
                }
            };
            if toc.encrypted() {
                println!(
                    "{}: szyfrowany - pomijam",
                    path.file_name().unwrap().to_string_lossy()
                );
                continue;
            }
            let libs: Vec<usize> = (0..toc.ids.len())
                .filter(|&i| toc.chunk_type(i) == CHUNK_SHADER_LIBRARY)
                .collect();
            let code = (0..toc.ids.len())
                .filter(|&i| toc.chunk_type(i) == CHUNK_SHADER_CODE)
                .count();
            println!(
                "{}: v{} flagi {:#x} porcje {} bloki {} metody {:?} biblioteki {} grupy-porcje {}",
                path.file_name().unwrap().to_string_lossy(),
                toc.version,
                toc.flags,
                toc.ids.len(),
                toc.blocks.len(),
                toc.methods,
                libs.len(),
                code
            );
            for &li in &libs {
                let data = match toc.read(li, oodle.as_ref()) {
                    Ok(d) => d,
                    Err(e) => {
                        println!("  biblioteka: {e}");
                        continue;
                    }
                };
                let lib = match Library::parse(&data) {
                    Ok(l) => l,
                    Err(e) => {
                        println!(
                            "  biblioteka ({} B): {e}; początek {:02x?}",
                            data.len(),
                            &data[..32.min(data.len())]
                        );
                        continue;
                    }
                };
                let mut freq = [0usize; 16];
                for s in &lib.shaders {
                    freq[s.frequency as usize] += 1;
                }
                let packed = lib
                    .groups
                    .iter()
                    .filter(|g| g.compressed != g.uncompressed)
                    .count();
                println!(
                    "  biblioteka: shadery {} grupy {} (skompresowane {packed}) częstotliwości {:?}",
                    lib.shaders.len(),
                    lib.groups.len(),
                    &freq[..8]
                );
                // Pierwsze grupy: rozpakowanie i kontenery DXBC.
                for g in 0..lib.groups.len().min(3) {
                    let Some(ci) = toc.find(&lib.groups[g].chunk) else {
                        println!("  grupa {g}: brak porcji w tym kontenerze");
                        continue;
                    };
                    let raw = match toc.read(ci, oodle.as_ref()) {
                        Ok(r) => r,
                        Err(e) => {
                            println!("  grupa {g}: {e}");
                            continue;
                        }
                    };
                    let grp = &lib.groups[g];
                    let data = if grp.compressed != grp.uncompressed {
                        match decompress(
                            "oodle",
                            &raw[..grp.compressed as usize],
                            grp.uncompressed as usize,
                            oodle.as_ref(),
                        ) {
                            Ok(d) => d,
                            Err(e) => {
                                println!("  grupa {g}: {e}");
                                continue;
                            }
                        }
                    } else {
                        raw
                    };
                    let parts = lib.split(g, &data);
                    let dx = parts.iter().filter(|(_, c)| container(c).is_some()).count();
                    println!(
                        "  grupa {g}: {} B, shadery {}, kontenery DXBC {dx}, początek {:02x?}",
                        data.len(),
                        parts.len(),
                        &data[..24.min(data.len())]
                    );
                }
            }
        }
    }
}
