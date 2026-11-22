//! Rozpoznanie gry: silnik, wersja, API grafiki, anty-cheat i pliki
//! shaderów, które da się skompilować z góry. Tylko odczyt plików gry.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Unreal,
    Unity,
    Dagor,
    Unknown,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Unreal => "Unreal Engine",
            Engine::Unity => "Unity",
            Engine::Dagor => "Dagor Engine",
            Engine::Unknown => crate::i18n::tr("nieznany", "unknown"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderKind {
    /// Zrzut shaderów Dagora dla DirectX 11.
    DagorDx11,
    /// Zrzut shaderów Dagora dla DirectX 12.
    DagorDx12,
    /// Zrzut shaderów Dagora dla Vulkana (SPIR-V).
    DagorVulkan,
    /// Pliki serializowane / paczki Unity (obiekty Shader w środku).
    UnityAssets,
    /// Archiwum shaderów Unreal (*.ushaderbytecode) albo kontenery IoStore/pak.
    UnrealArchive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShaderFile {
    pub path: PathBuf,
    pub kind: ShaderKind,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub engine: Engine,
    pub version: Option<String>,
    /// API grafiki, jeśli da się je ustalić (np. "DirectX 12").
    pub api: Option<String>,
    pub anti_cheat: Vec<&'static str>,
    pub shaders: Vec<ShaderFile>,
    /// Recorder SCSKiller (d3d12.dll + scskiller.cfg) w folderze gry.
    pub scskiller_recorder: bool,
    /// Unreal: paczki szyfrowane (shaderów nie da się odczytać).
    pub encrypted: bool,
}

impl Info {
    pub fn shader_bytes(&self) -> u64 {
        self.shaders.iter().map(|s| s.bytes).sum()
    }
}

/// Pliki i katalogi do `depth` poziomów w głąb (bez wchodzenia głębiej).
fn walk(root: &Path, depth: usize, out: &mut Vec<(PathBuf, bool, u64)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        out.push((path.clone(), meta.is_dir(), meta.len()));
        if meta.is_dir() && depth > 0 {
            walk(&path, depth - 1, out);
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn inspect(dir: &Path) -> Info {
    let mut entries = Vec::new();
    walk(dir, 3, &mut entries);

    let mut info = Info {
        engine: Engine::Unknown,
        version: None,
        api: None,
        anti_cheat: Vec::new(),
        shaders: Vec::new(),
        scskiller_recorder: dir.join("scskiller.cfg").is_file(),
        encrypted: false,
    };

    // Anty-cheat: tylko po nazwach plików i katalogów.
    for (path, _, _) in &entries {
        let n = name_of(path);
        let ac = if n.contains("easyanticheat") {
            Some("EasyAntiCheat")
        } else if n == "battleye" || n.ends_with("_be.exe") {
            Some("BattlEye")
        } else if n.starts_with("vgk") || n.contains("vanguard") {
            Some("Vanguard")
        } else if n.contains("xigncode") {
            Some("XIGNCODE3")
        } else {
            None
        };
        if let Some(ac) = ac {
            if !info.anti_cheat.contains(&ac) {
                info.anti_cheat.push(ac);
            }
        }
    }

    // Dagor: compiledShaders\*.shdump.bin.
    for (path, is_dir, len) in &entries {
        let n = name_of(path);
        if !is_dir && n.ends_with(".shdump.bin") {
            let kind = if n.contains("spirv") {
                ShaderKind::DagorVulkan
            } else if n.contains("dx12") {
                ShaderKind::DagorDx12
            } else {
                ShaderKind::DagorDx11
            };
            info.shaders.push(ShaderFile {
                path: path.clone(),
                kind,
                bytes: *len,
            });
        }
    }
    if !info.shaders.is_empty() {
        info.engine = Engine::Dagor;
        info.api = dagor_api(dir);
        return info;
    }

    // Unity: UnityPlayer.dll i folder *_Data.
    let unity_player = entries
        .iter()
        .find(|(p, d, _)| !d && name_of(p) == "unityplayer.dll")
        .map(|(p, _, _)| p.parent().unwrap_or(dir).to_path_buf());
    if let Some(root) = unity_player {
        info.engine = Engine::Unity;
        if let Some(data) = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir() && name_of(p).ends_with("_data"))
        {
            info.version = unity_version(&data);
            info.api = unity_api(&data);
            let mut files = Vec::new();
            walk(&data, 6, &mut files);
            for (path, is_dir, len) in files {
                let n = name_of(&path);
                let serialized = n == "globalgamemanagers"
                    || n.ends_with(".assets")
                    || n.ends_with(".bundle")
                    || n == "data.unity3d"
                    || n.starts_with("level") && !n.contains('.')
                    || n == "unity_builtin_extra";
                if !is_dir && serialized {
                    info.shaders.push(ShaderFile {
                        path,
                        kind: ShaderKind::UnityAssets,
                        bytes: len,
                    });
                }
            }
        }
        return info;
    }

    // Unreal: folder Engine i paczki w <Gra>\Content\Paks.
    let paks: Vec<&(PathBuf, bool, u64)> = entries
        .iter()
        .filter(|(p, d, _)| {
            let n = name_of(p);
            !d && (n.ends_with(".utoc") || n.ends_with(".pak") || n.ends_with(".ushaderbytecode"))
                && p.parent().is_some_and(|parent| {
                    name_of(parent) == "paks" || n.ends_with(".ushaderbytecode")
                })
        })
        .collect();
    if !paks.is_empty() && dir.join("Engine").is_dir() {
        info.engine = Engine::Unreal;
        // Szyfrowanie: pierwszy kontener gry (global.utoc jest zwykle jawny).
        info.encrypted = paks
            .iter()
            .map(|(p, _, _)| p)
            .find(|p| {
                let n = name_of(p);
                n.ends_with(".utoc") && n != "global.utoc"
            })
            .and_then(|p| super::unreal::Toc::open(p).ok())
            .is_some_and(|t| t.encrypted())
            || paks.iter().any(|(p, _, _)| {
                name_of(p).ends_with(".pak") && super::unreal::pak_index_encrypted(p)
            });
        // API: DefaultEngine.ini z największej paczki .pak (IoStore jeszcze nie).
        if !info.encrypted {
            info.api = super::unreal_pak::find_pak(dir)
                .and_then(|p| super::unreal_pak::default_api(&p))
                .map(str::to_owned);
        }
        for (path, _, len) in paks {
            info.shaders.push(ShaderFile {
                path: path.clone(),
                kind: ShaderKind::UnrealArchive,
                bytes: *len,
            });
        }
    }
    info
}

/// Wersja Unity z nagłówka pliku globalgamemanagers (tekst w rodzaju
/// "2022.3.62f2" w pierwszych bajtach).
fn unity_version(data: &Path) -> Option<String> {
    let path = ["globalgamemanagers", "data.unity3d", "mainData"]
        .iter()
        .map(|n| data.join(n))
        .find(|p| p.is_file())?;
    let mut head = vec![0u8; 4096];
    let n = {
        use std::io::Read;
        std::fs::File::open(path).ok()?.read(&mut head).ok()?
    };
    find_unity_version(&head[..n])
}

/// Szuka w bajtach tekstu wersji Unity: cyfry.cyfry.cyfry + litera + cyfry.
pub fn find_unity_version(bytes: &[u8]) -> Option<String> {
    let text: String = bytes
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { ' ' })
        .collect();
    text.split(' ').find_map(|word| {
        let mut parts = word.splitn(3, '.');
        let (a, b, c) = (parts.next()?, parts.next()?, parts.next()?);
        let ok = a.len() == 4
            && a.chars().all(|ch| ch.is_ascii_digit())
            && !b.is_empty()
            && b.chars().all(|ch| ch.is_ascii_digit())
            && c.chars().next().is_some_and(|ch| ch.is_ascii_digit())
            && c.chars().any(|ch| matches!(ch, 'a' | 'b' | 'f' | 'p'));
        ok.then(|| word.to_owned())
    })
}

/// API z ostatniego Player.log gry (LocalLow\<firma>\<produkt>\Player.log,
/// nazwy z *_Data\app.info).
fn unity_api(data: &Path) -> Option<String> {
    let info = std::fs::read_to_string(data.join("app.info")).ok()?;
    let mut lines = info.lines();
    let (company, product) = (lines.next()?.trim(), lines.next()?.trim());
    let log = dirs_local_low()?
        .join(company)
        .join(product)
        .join("Player.log");
    let text = std::fs::read_to_string(log).ok()?;
    api_from_log(&text)
}

pub fn api_from_log(text: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.to_lowercase();
        if l.contains("direct3d 12") || l.contains("d3d12") && l.contains("version") {
            return Some("DirectX 12".into());
        }
        if l.contains("direct3d 11") {
            return Some("DirectX 11".into());
        }
        if l.contains("vulkan") && l.contains("version") {
            return Some("Vulkan".into());
        }
    }
    None
}

fn dirs_local_low() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("USERPROFILE")?)
            .join("AppData")
            .join("LocalLow"),
    )
}

/// Dagor: video { driver:t="dx12" } w config.blk albo <gra>.config.blk.
/// Bez wpisu silnik wybiera sam; `cache\dx12.cache` Dagor zapisuje po
/// sesji DirectX 12, więc jego obecność wskazuje to API.
fn dagor_api(dir: &Path) -> Option<String> {
    let config = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            let n = name_of(p);
            n == "config.blk" || n.ends_with(".config.blk")
        });
    if let Some(api) = config
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| dagor_driver(&t))
    {
        return Some(api);
    }
    dir.join("cache")
        .join("dx12.cache")
        .is_file()
        .then(|| "DirectX 12".into())
}

/// Tekst bloku najwyższego poziomu `name{...}` bez bloków zagnieżdżonych.
fn top_block(blk: &str, name: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut start = None;
    let mut out = String::new();
    for (i, ch) in blk.char_indices() {
        match ch {
            '{' => {
                if depth == 0 && start.is_none() {
                    let head = blk[..i].trim_end();
                    let word = head
                        .rsplit(|c: char| c.is_whitespace() || c == '}')
                        .next()
                        .unwrap_or("");
                    if word == name {
                        start = Some(i);
                    }
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && start.is_some() {
                    return Some(out);
                }
            }
            _ if depth == 1 && start.is_some() => out.push(ch),
            _ => {}
        }
    }
    start.map(|_| out)
}

pub fn dagor_driver(blk: &str) -> Option<String> {
    let block = top_block(blk, "video")?;
    let at = block.find("driver:t=\"")? + "driver:t=\"".len();
    let value = &block[at..];
    let value = &value[..value.find('"')?];
    Some(match value {
        "dx12" => "DirectX 12".into(),
        "dx11" => "DirectX 11".into(),
        "vulkan" => "Vulkan".into(),
        "auto" => crate::i18n::tr("automatycznie", "auto").into(),
        other => other.into(),
    })
}

/// Rozmiar w czytelnej postaci.
pub fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0)
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.0} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.0} KB", b / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_unity_version_in_header_bytes() {
        let mut bytes = vec![0u8; 64];
        bytes.extend_from_slice(b"2022.3.62f2\0\x05\0\0");
        assert_eq!(find_unity_version(&bytes).as_deref(), Some("2022.3.62f2"));
        assert_eq!(find_unity_version(b"version 1.2.3 nothing"), None);
    }

    #[test]
    fn reads_api_from_player_log() {
        let log = "Initialize engine version: 2022.3\nDirect3D:\n    Version:  Direct3D 12.0 [level 12.1]\n";
        assert_eq!(api_from_log(log).as_deref(), Some("DirectX 12"));
        let log11 = "Direct3D:\n    Version:  Direct3D 11.0 [level 11.1]\n";
        assert_eq!(api_from_log(log11).as_deref(), Some("DirectX 11"));
    }

    #[test]
    fn reads_dagor_driver_from_config() {
        let blk =
            "graphics{\n  x:i=1\n}\nvideo{\n  mode:t=\"fullscreen\"\n  driver:t=\"dx12\"\n}\n";
        assert_eq!(dagor_driver(blk).as_deref(), Some("DirectX 12"));
        assert_eq!(dagor_driver("graphics{}"), None);
        // Wpis po bloku zagnieżdżonym i blok bez wpisu.
        let nested =
            "video{\n  dynamicResolution{\n    enabled:b=no\n  }\n  driver:t=\"vulkan\"\n}\n";
        assert_eq!(dagor_driver(nested).as_deref(), Some("Vulkan"));
        let none =
            "video{\n  fpsLimit:i=120\n  dynamicResolution{\n    driver:t=\"dx11\"\n  }\n}\n";
        assert_eq!(dagor_driver(none), None);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(86_885_515), "83 MB");
        assert_eq!(human(36 * 1024 * 1024 * 1024), "36.0 GB");
    }
}
