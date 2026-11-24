//! Source 2 (np. Counter-Strike 2): shadery w plikach `.vcs` w paczkach VPK
//! (`game\<mod>\shaders_pc_dir.vpk`). Gra jest tylko czytana.
//!
//! VPK (wersja 1 i 2): nagłówek, drzewo katalogu (rozszerzenie → ścieżka →
//! nazwa → wpis), dane w pliku `_dir` za drzewem albo w `<nazwa>_NNN.vpk`.
//! Plik `.vcs` dla DirectX 11 (`*_pc_50_*`) trzyma kod shaderów w kontenerach
//! DXBC - jawnie albo w ramkach Zstandard. Zamiast odtwarzać zmienny między
//! wersjami układ `.vcs`, wycinamy kontenery DXBC z pliku i z każdej ramki
//! zstd, która rozpakowuje się bez słownika.
//!
//! Format VPK na podstawie publicznych opisów (Valve Developer Community) -
//! kod napisany od zera.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::unity::{carve_dxbc, Dxbc};

const VPK_MAGIC: u32 = 0x55AA_1234;
const IN_DIR: u16 = 0x7FFF;
/// Największy wynik jednej ramki zstd w pliku `.vcs`.
const FRAME_MAX: usize = 256 << 20;

/// Plik w paczce VPK.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    preload: Vec<u8>,
    archive: u16,
    offset: u32,
    length: u32,
}

pub struct Vpk {
    dir: PathBuf,
    /// Początek danych w pliku `_dir` (za nagłówkiem i drzewem).
    data_start: u64,
    pub entries: Vec<Entry>,
}

fn cstr(b: &[u8], at: &mut usize) -> Result<String, String> {
    let rest = b.get(*at..).ok_or("VPK: koniec drzewa")?;
    let n = rest.iter().position(|&c| c == 0).ok_or("VPK: brak końca tekstu")?;
    let s = String::from_utf8_lossy(&rest[..n]).into_owned();
    *at += n + 1;
    Ok(s)
}

fn le<const N: usize>(b: &[u8], at: &mut usize) -> Result<[u8; N], String> {
    let s = b.get(*at..*at + N).ok_or("VPK: ucięty wpis")?;
    *at += N;
    Ok(s.try_into().unwrap())
}

impl Vpk {
    pub fn open(dir: &Path) -> Result<Vpk, String> {
        let mut f = std::fs::File::open(dir).map_err(|e| e.to_string())?;
        let mut head = [0u8; 28];
        f.read_exact(&mut head[..12]).map_err(|e| e.to_string())?;
        let word = |i: usize| u32::from_le_bytes(head[i..i + 4].try_into().unwrap());
        if word(0) != VPK_MAGIC {
            return Err("to nie VPK".into());
        }
        let (version, tree) = (word(4), word(8) as usize);
        let header = match version {
            1 => 12,
            2 => {
                f.read_exact(&mut head[12..28]).map_err(|e| e.to_string())?;
                28
            }
            v => return Err(format!("VPK: wersja {v} nieobsługiwana")),
        };
        if tree > 256 << 20 {
            return Err("VPK: za duże drzewo".into());
        }
        let mut b = vec![0u8; tree];
        f.read_exact(&mut b).map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        let mut at = 0;
        loop {
            let ext = cstr(&b, &mut at)?;
            if ext.is_empty() {
                break;
            }
            loop {
                let path = cstr(&b, &mut at)?;
                if path.is_empty() {
                    break;
                }
                loop {
                    let name = cstr(&b, &mut at)?;
                    if name.is_empty() {
                        break;
                    }
                    let _crc = le::<4>(&b, &mut at)?;
                    let preload = u16::from_le_bytes(le::<2>(&b, &mut at)?) as usize;
                    let archive = u16::from_le_bytes(le::<2>(&b, &mut at)?);
                    let offset = u32::from_le_bytes(le::<4>(&b, &mut at)?);
                    let length = u32::from_le_bytes(le::<4>(&b, &mut at)?);
                    let _end = le::<2>(&b, &mut at)?;
                    let preload = b.get(at..at + preload).ok_or("VPK: ucięty wpis")?.to_vec();
                    at += preload.len();
                    let full = if path == " " {
                        format!("{name}.{ext}")
                    } else {
                        format!("{path}/{name}.{ext}")
                    };
                    entries.push(Entry {
                        path: full,
                        preload,
                        archive,
                        offset,
                        length,
                    });
                }
            }
        }
        Ok(Vpk {
            dir: dir.to_path_buf(),
            data_start: (header + tree) as u64,
            entries,
        })
    }

    /// Plik z danymi archiwum `n` (`shaders_pc_dir.vpk` → `shaders_pc_003.vpk`).
    fn archive_path(&self, n: u16) -> Option<PathBuf> {
        let name = self.dir.file_name()?.to_string_lossy().into_owned();
        let stem = name.strip_suffix("_dir.vpk").or_else(|| name.strip_suffix("_DIR.vpk"))?;
        Some(self.dir.with_file_name(format!("{stem}_{n:03}.vpk")))
    }

    /// Cała zawartość pliku z paczki.
    pub fn read(&self, e: &Entry) -> Result<Vec<u8>, String> {
        let mut out = e.preload.clone();
        if e.length == 0 {
            return Ok(out);
        }
        let (path, at) = if e.archive == IN_DIR {
            (self.dir.clone(), self.data_start + e.offset as u64)
        } else {
            let p = self
                .archive_path(e.archive)
                .ok_or("VPK: nazwa paczki bez _dir")?;
            (p, e.offset as u64)
        };
        let mut f = std::fs::File::open(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        f.seek(SeekFrom::Start(at)).map_err(|err| err.to_string())?;
        let start = out.len();
        out.resize(start + e.length as usize, 0);
        f.read_exact(&mut out[start..]).map_err(|err| err.to_string())?;
        Ok(out)
    }
}

/// Czy plik `.vcs` jest dla DirectX (nie Vulkan / konsole).
pub fn is_dx_shader(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".vcs") && p.contains("_pc_") && !p.contains("vulkan")
}

/// Wycina kontenery DXBC z pliku `.vcs`: jawne i z ramek zstd.
/// Zwraca liczbę ramek, których nie dało się rozpakować (np. ze słownikiem).
pub fn carve_vcs(bytes: &[u8], found: &mut dyn FnMut(Dxbc)) -> usize {
    carve_dxbc(bytes, &mut *found);
    let decoder = crate::zstd::Decoder::new();
    let mut failed = 0;
    let mut i = 0;
    while let Some(p) = bytes[i..]
        .windows(4)
        .position(|w| w == [0x28, 0xB5, 0x2F, 0xFD])
    {
        let at = i + p;
        match decoder.decompress_frame(&bytes[at..], FRAME_MAX) {
            Ok((data, used)) => {
                carve_dxbc(&data, &mut *found);
                i = at + used.max(4);
            }
            Err(_) => {
                failed += 1;
                i = at + 4;
            }
        }
    }
    failed
}

/// Paczki shaderów gry Source 2: `game\*\shaders_pc_dir.vpk` (i podobne).
pub fn shader_vpks(game: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let root = game.join("game");
    for module in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(module.path()).into_iter().flatten().flatten() {
            let n = f.file_name().to_string_lossy().to_lowercase();
            if n.starts_with("shaders") && n.ends_with("_dir.vpk") && !n.contains("vulkan") {
                out.push(f.path());
            }
        }
    }
    out.sort();
    out
}

/// Plik .exe gry Source 2: `game\bin\win64\*.exe` bez narzędzi.
pub fn game_exe(game: &Path) -> Option<String> {
    let skip = ["crash", "vconsole", "helper", "report", "steam", "resourcecompiler"];
    std::fs::read_dir(game.join("game").join("bin").join("win64"))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            (lower.ends_with(".exe") && !skip.iter().any(|s| lower.contains(s)))
                .then(|| (e.metadata().map(|m| m.len()).unwrap_or(0), name))
        })
        .max()
        .map(|(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vpk_bytes(entries: &[(&str, &str, &str, &[u8])]) -> Vec<u8> {
        let mut tree = Vec::new();
        let mut data = Vec::new();
        for (ext, path, name, content) in entries {
            for s in [*ext, *path, *name] {
                tree.extend_from_slice(s.as_bytes());
                tree.push(0);
            }
            tree.extend_from_slice(&0u32.to_le_bytes());
            tree.extend_from_slice(&0u16.to_le_bytes());
            tree.extend_from_slice(&IN_DIR.to_le_bytes());
            tree.extend_from_slice(&(data.len() as u32).to_le_bytes());
            tree.extend_from_slice(&(content.len() as u32).to_le_bytes());
            tree.extend_from_slice(&0xFFFFu16.to_le_bytes());
            data.extend_from_slice(content);
            tree.extend_from_slice(&[0, 0, 0]); // koniec nazw, ścieżek, rozszerzeń
        }
        tree.push(0);
        let mut b = Vec::new();
        for v in [VPK_MAGIC, 1, tree.len() as u32] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend(tree);
        b.extend(data);
        b
    }

    #[test]
    fn reads_vpk_directory_and_data() {
        let dir = std::env::temp_dir().join("tc_vpk_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shaders_pc_dir.vpk");
        std::fs::write(
            &path,
            vpk_bytes(&[("vcs", "shaders/vfx", "x_pc_50_ps", b"hello")]),
        )
        .unwrap();
        let vpk = Vpk::open(&path).unwrap();
        assert_eq!(vpk.entries.len(), 1);
        assert_eq!(vpk.entries[0].path, "shaders/vfx/x_pc_50_ps.vcs");
        assert!(is_dx_shader(&vpk.entries[0].path));
        assert_eq!(vpk.read(&vpk.entries[0]).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn carves_dxbc_from_raw_zstd_frame() {
        // Kontener DXBC (VS) w ramce zstd z blokiem surowym.
        let mut code = Vec::new();
        code.extend_from_slice(b"DXBC");
        code.extend_from_slice(&[7; 16]);
        code.extend_from_slice(&1u32.to_le_bytes());
        code.extend_from_slice(&0u32.to_le_bytes());
        code.extend_from_slice(&1u32.to_le_bytes());
        code.extend_from_slice(&36u32.to_le_bytes());
        code.extend_from_slice(b"SHEX");
        code.extend_from_slice(&4u32.to_le_bytes());
        code.extend_from_slice(&((1u32 << 16) | 0x50).to_le_bytes());
        code.resize(300, 0); // ramka testowa wymaga co najmniej 256 B
        let n = code.len() as u32;
        code[24..28].copy_from_slice(&n.to_le_bytes());
        let mut file = b"vcs2 header".to_vec();
        file.extend(crate::zstd::raw_frame(&code));
        file.extend_from_slice(b"tail");
        // Surowy blok zstd trzyma kontener jawnie, więc widać go dwa razy
        // (powtórki usuwa skrót w procesie kompilującym).
        let mut stages = Vec::new();
        let failed = carve_vcs(&file, &mut |d| stages.push(d.stage));
        assert_eq!(failed, 0);
        assert_eq!(stages, vec![super::super::unity::Stage::Vertex; 2]);
    }

    /// Przegląd shaderów zainstalowanej gry Source 2 (tylko odczyt):
    /// `TC_SOURCE2_GAME=<folder gry> cargo test source2_scan -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn source2_scan() {
        let dir = PathBuf::from(std::env::var("TC_SOURCE2_GAME").expect("TC_SOURCE2_GAME"));
        println!("exe: {:?}", game_exe(&dir));
        let mut unique = std::collections::HashSet::new();
        for vpk in shader_vpks(&dir) {
            let p = Vpk::open(&vpk).expect("VPK");
            let files: Vec<_> = p.entries.iter().filter(|e| is_dx_shader(&e.path)).collect();
            let (mut failed, mut errors) = (0, 0);
            for e in &files {
                match p.read(e) {
                    Ok(b) => failed += carve_vcs(&b, &mut |d| {
                        unique.insert(d.digest());
                    }),
                    Err(_) => errors += 1,
                }
            }
            println!(
                "{}: plików {} (vcs DX {}), ramki zstd nieudane {failed}, błędy odczytu {errors}",
                vpk.display(),
                p.entries.len(),
                files.len()
            );
        }
        println!("unikalne DXBC: {}", unique.len());
    }
}
