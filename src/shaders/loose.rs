//! Gry bez rozpoznanego silnika: kontenery DXBC (DirectX 11, SM4/SM5) leżące
//! jawnie w plikach shaderów gry (`*.cso`, `*.fxo`, pliki z „shader” w nazwie).
//! Spakowanych i szyfrowanych archiwów nie rozpakowujemy - z nich nic nie
//! wyjdzie, a gra jest tylko czytana.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::unity::{carve_dxbc, Dxbc};

/// Rozszerzenia plików ze skompilowanymi shaderami.
const SHADER_EXT: [&str; 9] = ["cso", "fxo", "fxc", "vso", "pso", "gso", "dxbc", "shd", "sb"];
/// Pliki z „shader” w nazwie, ale bez kodu (źródła, teksty, obrazy, strony).
const TEXT_EXT: [&str; 26] = [
    "hlsl", "hlsli", "fx", "fxh", "h", "txt", "json", "xml", "ini", "cfg", "jpg", "jpeg", "png",
    "dds", "webp", "gif", "bmp", "tga", "mp4", "webm", "bk2", "wav", "ogg", "html", "js", "css",
];
/// Katalogi, których nie przeszukujemy (redystrybucje, anty-cheat).
const SKIP_DIRS: [&str; 6] = [
    "_commonredist",
    "redist",
    "directx",
    "easyanticheat",
    "battleye",
    "__installer",
];
/// Kontener większy niż to nie jest shaderem.
const MAX_CONTAINER: usize = 64 << 20;
const CHUNK: usize = 32 << 20;

fn lower_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|x| x.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Czy plik może zawierać skompilowane shadery.
pub fn is_shader_file(p: &Path) -> bool {
    let (name, ext) = (lower_name(p), ext_of(p));
    // Pliki DirectX 12 / Vulkan (DXIL, SPIR-V) - nie dla DirectX 11.
    let other_api = ["dx12", "d3d12", "vulkan", "spirv"]
        .iter()
        .any(|s| name.contains(s));
    if TEXT_EXT.contains(&ext.as_str()) || other_api {
        return false;
    }
    SHADER_EXT.contains(&ext.as_str()) || name.contains("shader")
}

/// Pliki shaderów w folderze gry (do 8 poziomów w głąb): (ścieżka, rozmiar).
pub fn shader_files(game: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![(game.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = e.metadata() else { continue };
            let path = e.path();
            if meta.is_dir() {
                if depth < 8 && !SKIP_DIRS.contains(&lower_name(&path).as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if meta.len() >= 32 && is_shader_file(&path) {
                out.push((path, meta.len()));
            }
        }
    }
    out.sort();
    out
}

/// Plik .exe gry: największy w folderze gry (do 3 poziomów) bez programów
/// instalacyjnych, uruchamiających i narzędzi.
pub fn game_exe(game: &Path) -> Option<String> {
    const SKIP: [&str; 17] = [
        "crash", "unins", "setup", "redist", "vc_", "vcredist", "dxsetup", "launcher", "helper",
        "report", "anticheat", "_be", "prereq", "dotnet", "cefprocess", "steam", "installer",
    ];
    let mut best: Option<(u64, String)> = None;
    let mut stack = vec![(game.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = e.metadata() else { continue };
            let path = e.path();
            let name = lower_name(&path);
            if meta.is_dir() {
                if depth < 3 && !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if name.ends_with(".exe")
                && !SKIP.iter().any(|s| name.contains(s))
                && best.as_ref().is_none_or(|(len, _)| meta.len() > *len)
            {
                best = Some((meta.len(), e.file_name().to_string_lossy().into_owned()));
            }
        }
    }
    best.map(|(_, n)| n)
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

/// Przegląda plik kawałkami (pliki pamięci shaderów mają nawet kilka GB) i
/// podaje każdy kontener DXBC; kontener na granicy kawałków przechodzi do
/// następnego.
pub fn scan_file(path: &Path, found: &mut dyn FnMut(Dxbc)) -> Result<(), String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf: Vec<u8> = Vec::new();
    let mut eof = false;
    while !eof {
        let start = buf.len();
        buf.resize(start + CHUNK, 0);
        let mut filled = start;
        while filled < buf.len() {
            let n = f.read(&mut buf[filled..]).map_err(|e| e.to_string())?;
            if n == 0 {
                eof = true;
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        // Gdzie zaczyna się niedokończony kontener (zostaje na następny kawałek).
        let mut keep_from = None;
        let mut i = 0;
        while let Some(p) = buf[i..].windows(4).position(|w| w == b"DXBC") {
            let at = i + p;
            let total = le32(&buf, at + 24).map(|t| t as usize);
            let valid = le32(&buf, at + 20) == Some(1)
                && total.is_some_and(|t| (32..=MAX_CONTAINER).contains(&t));
            match total {
                None if !eof => {
                    keep_from = Some(at);
                    break;
                }
                Some(t) if valid && at + t > buf.len() && !eof => {
                    keep_from = Some(at);
                    break;
                }
                Some(t) if valid && at + t <= buf.len() => {
                    carve_dxbc(&buf[at..at + t], &mut *found);
                    i = at + t;
                }
                _ => i = at + 4,
            }
        }
        if eof {
            break;
        }
        // Bez niedokończonego kontenera zostaje ogon, w którym może zaczynać
        // się słowo "DXBC" (ale nie to, co już przejrzane).
        let keep = keep_from.unwrap_or_else(|| i.max(buf.len().saturating_sub(3)));
        buf.drain(..keep.min(buf.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dxbc(program_type: u32, fill: u8) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"DXBC");
        b.extend_from_slice(&[fill; 16]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&36u32.to_le_bytes());
        b.extend_from_slice(b"SHEX");
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&((program_type << 16) | 0x50).to_le_bytes());
        let n = b.len() as u32;
        b[24..28].copy_from_slice(&n.to_le_bytes());
        b
    }

    #[test]
    fn picks_shader_files() {
        assert!(is_shader_file(Path::new("x/ShaderCache.bin")));
        assert!(is_shader_file(Path::new("x/water_ps.cso")));
        assert!(!is_shader_file(Path::new("x/shaders/common.hlsl")));
        assert!(!is_shader_file(Path::new("x/texture.dds")));
        assert!(!is_shader_file(Path::new("content0/shaderdx12_0.cache")));
        assert!(!is_shader_file(Path::new("img/mesh_shader_ft_380x160.jpg")));
    }

    #[test]
    fn scans_file_with_container_across_chunks() {
        let dir = std::env::temp_dir().join("tc_loose_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shaders.cache");
        // Drugi kontener zaczyna się 10 B przed granicą pierwszego kawałka.
        let mut bytes = dxbc(0, 1);
        bytes.resize(CHUNK - 10, 0xEE);
        bytes.extend(dxbc(1, 2));
        bytes.extend_from_slice(b"DXBC");
        std::fs::write(&path, &bytes).unwrap();
        let mut stages = Vec::new();
        scan_file(&path, &mut |d| stages.push(d.stage)).unwrap();
        use super::super::unity::Stage;
        assert_eq!(stages, vec![Stage::Pixel, Stage::Vertex]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Pliki shaderów i kontenery DXBC w zainstalowanej grze (tylko odczyt):
    /// `TC_LOOSE_GAME=<folder gry> cargo test loose_scan --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn loose_scan() {
        let dir = PathBuf::from(std::env::var("TC_LOOSE_GAME").expect("TC_LOOSE_GAME"));
        println!("exe: {:?}", game_exe(&dir));
        let mut unique = std::collections::HashSet::new();
        for (f, len) in shader_files(&dir) {
            let before = unique.len();
            let r = scan_file(&f, &mut |d| {
                unique.insert(d.digest());
            });
            println!("{} ({len} B): +{} {:?}", f.display(), unique.len() - before, r.err());
        }
        println!("unikalne DXBC: {}", unique.len());
    }
}
