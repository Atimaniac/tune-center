//! Unreal Engine: klasyczne paczki `.pak` (wersje 10-11, indeks z
//! „path hash” i pełnym indeksem katalogów) oraz archiwum shaderów
//! `.ushaderbytecode` w wersji 2 (FSerializedShaderArchive).
//!
//! Układ (wg open-source CUE4Parse, kod własny):
//! - Stopka (v11, 221 B od końca): klucz GUID 16, indeks szyfrowany 1,
//!   magia 0x5A6F12E1, wersja u32, przesunięcie indeksu i64, rozmiar i64,
//!   skrót 20, 5 nazw metod kompresji po 32 B.
//! - Indeks: punkt montowania (FString), liczba wpisów i32, ziarno u64,
//!   [bool u32 + przesunięcie, rozmiar, skrót] dla indeksu skrótów ścieżek i
//!   dla pełnego indeksu katalogów, zakodowane wpisy (tablica bajtów), wpisy
//!   niezakodowane (FPakEntry).
//! - Pełny indeks katalogów: katalogi (nazwa, pliki: nazwa + położenie wpisu;
//!   ≥0 = przesunięcie w zakodowanych, <0 = -(i+1) w niezakodowanych).
//! - Zakodowany wpis: słowo bitowe (rozmiar bloku >> 11 w 6 b, liczba bloków
//!   16 b, szyfrowanie 1 b, metoda 6 b, trzy flagi „mieści się w 32 b”),
//!   potem przesunięcie, rozmiar nieskompr., [rozmiar skompr.], rozmiary bloków.
//! - Dane pliku zaczynają się za nagłówkiem FPakEntry (53 B + 4 + 16 × bloki).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAGIC: [u8; 4] = [0xE1, 0x12, 0x6F, 0x5A];

/// Plik w paczce.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    offset: u64,
    pub size: u64,
    compressed: u64,
    method: u32,
    encrypted: bool,
    block_size: u32,
    /// Rozmiary skompresowanych bloków (puste = bez kompresji).
    blocks: Vec<u32>,
}

pub struct Pak {
    pub path: PathBuf,
    #[allow(dead_code)] // diagnostyka (test unreal_pak_scan)
    pub version: u32,
    pub encrypted_index: bool,
    /// Nazwy metod; metoda 0 = brak, i → `methods[i-1]`.
    pub methods: Vec<String>,
    pub files: Vec<Entry>,
}

struct Cur<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.o + n > self.b.len() {
            return Err("indeks paczki: za krótki".into());
        }
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// FString: długość i32 (z zerem; ujemna = UTF-16).
    fn string(&mut self) -> Result<String, String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n > 0 {
            let s = self.take(n as usize)?;
            Ok(String::from_utf8_lossy(&s[..s.len() - 1]).into_owned())
        } else {
            let s = self.take((-n) as usize * 2)?;
            let w: Vec<u16> = s
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Ok(String::from_utf16_lossy(&w[..w.len() - 1]))
        }
    }
}

fn read_at(f: &mut File, pos: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; len];
    f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Zakodowany wpis (FPakEntry::EncodeTo).
fn decode(c: &mut Cur) -> Result<Entry, String> {
    let bits = c.u32()?;
    let mut block_size = (bits & 0x3F) << 11;
    let block_count = (bits >> 6) & 0xFFFF;
    let encrypted = bits & (1 << 22) != 0;
    let method = (bits >> 23) & 0x3F;
    if bits & 0x3F == 0x3F {
        block_size = c.u32()?;
    }
    let offset = if bits & (1 << 31) != 0 {
        c.u32()? as u64
    } else {
        c.u64()?
    };
    let size = if bits & (1 << 30) != 0 {
        c.u32()? as u64
    } else {
        c.u64()?
    };
    let compressed = if method != 0 {
        if bits & (1 << 29) != 0 {
            c.u32()? as u64
        } else {
            c.u64()?
        }
    } else {
        size
    };
    let mut blocks = Vec::new();
    if block_count > 0 {
        if block_count == 1 && !encrypted {
            blocks.push(compressed as u32);
        } else {
            for _ in 0..block_count {
                blocks.push(c.u32()?);
            }
        }
    }
    if method != 0 && block_size == 0 {
        block_size = size as u32;
    }
    Ok(Entry {
        path: String::new(),
        offset,
        size,
        compressed,
        method,
        encrypted,
        block_size,
        blocks,
    })
}

/// Niezakodowany FPakEntry (v8+): przesunięcie, rozmiar, rozmiar nieskompr.,
/// metoda u32, skrót 20, [bloki], flagi u8, rozmiar bloku u32.
fn serialized(c: &mut Cur) -> Result<Entry, String> {
    let offset = c.u64()?;
    let compressed = c.u64()?;
    let size = c.u64()?;
    let method = c.u32()?;
    c.take(20)?;
    let mut ranges = Vec::new();
    if method != 0 {
        let n = c.u32()?;
        for _ in 0..n {
            let (s, e) = (c.u64()?, c.u64()?);
            ranges.push((e - s) as u32);
        }
    }
    let flags = c.u8()?;
    let block_size = c.u32()?;
    Ok(Entry {
        path: String::new(),
        offset,
        size,
        compressed,
        method,
        encrypted: flags & 1 != 0,
        block_size,
        blocks: ranges,
    })
}

impl Pak {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = f.metadata().map_err(|e| e.to_string())?.len();
        let tail = len.min(512);
        let foot = read_at(&mut f, len - tail, tail as usize)?;
        let m = foot
            .windows(4)
            .rposition(|w| w == MAGIC)
            .ok_or("brak stopki paczki .pak")?;
        let encrypted_index = m > 0 && foot[m - 1] == 1;
        let mut c = Cur { b: &foot, o: m + 4 };
        let version = c.u32()?;
        if !(10..=11).contains(&version) {
            return Err(format!(
                "paczka .pak w wersji {version} (obsługiwane 10-11)"
            ));
        }
        let index_offset = c.u64()?;
        let index_size = c.u64()?;
        c.take(20)?;
        let methods: Vec<String> = (0..5)
            .filter_map(|_| c.take(32).ok())
            .map(|s| {
                let end = s.iter().position(|&x| x == 0).unwrap_or(32);
                String::from_utf8_lossy(&s[..end]).into_owned()
            })
            .filter(|s| !s.is_empty())
            .collect();
        let mut pak = Self {
            path: path.to_path_buf(),
            version,
            encrypted_index,
            methods,
            files: Vec::new(),
        };
        if encrypted_index {
            return Ok(pak);
        }

        let index = read_at(&mut f, index_offset, index_size as usize)?;
        let mut c = Cur { b: &index, o: 0 };
        let mount = c.string()?;
        let _count = c.i32()?;
        c.u64()?; // ziarno skrótów ścieżek
        if c.u32()? != 0 {
            c.take(8 + 8 + 20)?;
        }
        if c.u32()? == 0 {
            return Err("paczka bez pełnego indeksu katalogów".into());
        }
        let (dir_offset, dir_size) = (c.u64()?, c.u64()?);
        c.take(20)?;
        let n = c.u32()? as usize;
        let encoded = c.take(n)?;
        let n = c.u32()? as usize;
        let mut plain = Vec::with_capacity(n);
        for _ in 0..n {
            plain.push(serialized(&mut c)?);
        }

        let dirs = read_at(&mut f, dir_offset, dir_size as usize)?;
        let mut c = Cur { b: &dirs, o: 0 };
        let dir_count = c.u32()?;
        for _ in 0..dir_count {
            let dir = c.string()?;
            let files = c.u32()?;
            for _ in 0..files {
                let name = c.string()?;
                let at = c.i32()?;
                let mut e = if at >= 0 {
                    let mut ec = Cur {
                        b: encoded,
                        o: at as usize,
                    };
                    decode(&mut ec)?
                } else {
                    match plain.get((-at - 1) as usize) {
                        Some(e) => e.clone(),
                        None => continue,
                    }
                };
                e.path = format!("{mount}{dir}{name}");
                pak.files.push(e);
            }
        }
        Ok(pak)
    }

    fn method_name(&self, m: u32) -> Option<&str> {
        (m != 0).then(|| {
            self.methods
                .get(m as usize - 1)
                .map(|s| s.as_str())
                .unwrap_or("?")
        })
    }

    /// Zawartość pliku (rozpakowana).
    pub fn read(&self, e: &Entry, oodle: Option<&super::unreal::Oodle>) -> Result<Vec<u8>, String> {
        if e.encrypted {
            return Err(format!("{}: plik szyfrowany", e.path));
        }
        let mut f = File::open(&self.path).map_err(|e| e.to_string())?;
        let header = 53
            + if e.method != 0 {
                4 + 16 * e.blocks.len() as u64
            } else {
                0
            };
        let start = e.offset + header;
        let Some(method) = self.method_name(e.method) else {
            return read_at(&mut f, start, e.size as usize);
        };
        let raw = read_at(&mut f, start, e.compressed as usize)?;
        let mut out = Vec::with_capacity(e.size as usize);
        let mut pos = 0usize;
        for &b in &e.blocks {
            let want = (e.size as usize - out.len()).min(e.block_size as usize);
            let src = raw
                .get(pos..pos + b as usize)
                .ok_or("blok poza danymi pliku")?;
            out.extend(super::unreal::decompress(method, src, want, oodle)?);
            pos += b as usize;
        }
        if out.len() != e.size as usize {
            return Err(format!(
                "{}: rozpakowano {} z {} B",
                e.path,
                out.len(),
                e.size
            ));
        }
        Ok(out)
    }
}

/// Wpis shadera w archiwum v2.
#[derive(Clone, Copy, Debug)]
pub struct Code {
    pub offset: u64,
    pub size: u32,
    pub uncompressed: u32,
    #[allow(dead_code)] // diagnostyka (test unreal_pak_scan)
    pub frequency: u8,
}

/// FSerializedShaderArchive (plik `.ushaderbytecode`, wersja 2).
pub struct Archive {
    pub shaders: Vec<Code>,
    /// Początek bajtów shaderów w pliku.
    pub code_start: usize,
}

impl Archive {
    pub fn parse(b: &[u8]) -> Result<Self, String> {
        let mut c = Cur { b, o: 0 };
        let version = c.u32()?;
        if version != 2 {
            return Err(format!(
                "archiwum shaderów w wersji {version} (obsługiwana 2)"
            ));
        }
        let count = |c: &mut Cur, item: usize| -> Result<usize, String> {
            let n = c.u32()? as usize;
            if n.saturating_mul(item) > b.len() {
                return Err(format!("archiwum shaderów: nierealna liczba wpisów {n}"));
            }
            Ok(n)
        };
        let n = count(&mut c, 20)?;
        c.take(n * 20)?; // skróty map
        let n = count(&mut c, 20)?;
        c.take(n * 20)?; // skróty shaderów
        let n = count(&mut c, 16)?;
        c.take(n * 16)?; // wpisy map
        let n = count(&mut c, 17)?;
        let mut shaders = Vec::with_capacity(n);
        for _ in 0..n {
            shaders.push(Code {
                offset: c.u64()?,
                size: c.u32()?,
                uncompressed: c.u32()?,
                frequency: c.u8()?,
            });
        }
        let n = count(&mut c, 16)?;
        c.take(n * 16)?; // wpisy wstępnego wczytania
        let n = count(&mut c, 4)?;
        c.take(n * 4)?; // indeksy
        Ok(Self {
            shaders,
            code_start: c.o,
        })
    }

    /// Bajty shadera `i` (rozpakowane z Oodle, gdy rozmiary się różnią).
    pub fn code(
        &self,
        b: &[u8],
        i: usize,
        oodle: Option<&super::unreal::Oodle>,
    ) -> Result<Vec<u8>, String> {
        let s = self.shaders[i];
        let start = self.code_start + s.offset as usize;
        let raw = b
            .get(start..start + s.size as usize)
            .ok_or("shader poza archiwum")?;
        if s.size == s.uncompressed {
            Ok(raw.to_vec())
        } else {
            super::unreal::decompress("oodle", raw, s.uncompressed as usize, oodle)
        }
    }
}

/// Paczka gry z archiwami shaderów (największy `.pak` w `Content\Paks`).
pub fn find_pak(game: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32, best: &mut Option<(u64, PathBuf)>) {
        let Ok(list) = std::fs::read_dir(dir) else {
            return;
        };
        for e in list.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth > 0 {
                    walk(&p, depth - 1, best);
                }
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pak")) {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                if best.as_ref().is_none_or(|(l, _)| len > *l) {
                    *best = Some((len, p));
                }
            }
        }
    }
    let mut best = None;
    walk(game, 4, &mut best);
    best.map(|(_, p)| p)
}

/// Domyślne API gry z `<Projekt>/Config/DefaultEngine.ini` w paczce
/// (`DefaultGraphicsRHI=DefaultGraphicsRHI_DX11/DX12`).
pub fn default_api(pak: &Path) -> Option<&'static str> {
    let pak = Pak::open(pak).ok()?;
    let e = pak.files.iter().find(|e| {
        let l = e.path.to_lowercase();
        l.ends_with("/config/defaultengine.ini") && !l.contains("/engine/config/")
    })?;
    let text = String::from_utf8_lossy(&pak.read(e, None).ok()?).into_owned();
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("DefaultGraphicsRHI="))?;
    if line.ends_with("DX12") {
        Some("DirectX 12")
    } else if line.ends_with("DX11") {
        Some("DirectX 11")
    } else {
        None
    }
}

/// Archiwa shaderów SM5 (DXBC, DirectX 11) z paczki: wpisy i bajty.
pub fn sm5_archives(pak: &Path) -> Result<Vec<(Archive, Vec<u8>)>, String> {
    let pak = Pak::open(pak)?;
    if pak.encrypted_index {
        return Err("indeks paczki jest szyfrowany".into());
    }
    let mut out = Vec::new();
    for e in pak.files.iter().filter(|e| {
        let l = e.path.to_lowercase();
        l.ends_with(".ushaderbytecode") && l.contains("pcd3d_sm5")
    }) {
        let bytes = pak.read(e, None)?;
        let arch = Archive::parse(&bytes).map_err(|err| format!("{}: {err}", e.path))?;
        out.push((arch, bytes));
    }
    if out.is_empty() {
        return Err("w paczce nie ma archiwów shaderów DirectX 11 (PCD3D_SM5)".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TC_UNREAL_PAK=<plik .pak> cargo test unreal_pak_scan -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn unreal_pak_scan() {
        let p = PathBuf::from(std::env::var("TC_UNREAL_PAK").unwrap());
        let t = std::time::Instant::now();
        let pak = Pak::open(&p).unwrap();
        println!(
            "v{} indeks szyfrowany {} metody {:?} pliki {} ({:?})",
            pak.version,
            pak.encrypted_index,
            pak.methods,
            pak.files.len(),
            t.elapsed()
        );
        for e in pak.files.iter().filter(|e| {
            let l = e.path.to_lowercase();
            l.ends_with(".ushaderbytecode") || l.contains("pipelinecache")
        }) {
            println!(
                "  {} {} B (skompr. {} B, metoda {}, bloki {})",
                e.path,
                e.size,
                e.compressed,
                e.method,
                e.blocks.len()
            );
            if !e.path.to_lowercase().ends_with(".ushaderbytecode") {
                continue;
            }
            let t = std::time::Instant::now();
            let data = match pak.read(e, None) {
                Ok(d) => d,
                Err(err) => {
                    println!("    {err}");
                    continue;
                }
            };
            let arch = match Archive::parse(&data) {
                Ok(a) => a,
                Err(err) => {
                    println!("    {err}");
                    continue;
                }
            };
            let mut freq = [0usize; 16];
            for s in &arch.shaders {
                freq[(s.frequency & 15) as usize] += 1;
            }
            let packed = arch
                .shaders
                .iter()
                .filter(|s| s.size != s.uncompressed)
                .count();
            println!(
                "    shadery {} (skompr. {packed}) częstotliwości {:?} odczyt {:?}",
                arch.shaders.len(),
                &freq[..8],
                t.elapsed()
            );
            let mut dxbc = 0;
            let mut dxil = 0;
            let mut bad = 0;
            for i in 0..arch.shaders.len().min(200) {
                match arch.code(&data, i, None) {
                    Ok(code) => match super::super::unreal::container(&code) {
                        Some(cont) if cont.windows(4).any(|w| w == b"DXIL") => dxil += 1,
                        Some(_) => dxbc += 1,
                        None => {
                            if bad == 0 {
                                println!("    bez DXBC: {:02x?}", &code[..32.min(code.len())]);
                            }
                            bad += 1
                        }
                    },
                    Err(err) => {
                        if bad == 0 {
                            println!("    {err}");
                        }
                        bad += 1
                    }
                }
            }
            println!("    pierwsze 200: DXBC {dxbc}, DXIL {dxil}, błędy {bad}");
        }
    }
}
