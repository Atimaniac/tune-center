//! Wyszukiwanie zainstalowanych gier w sklepach: Steam, Epic Games, GOG,
//! Ubisoft Connect, EA app, Xbox (PC), Battle.net i Riot Games. Tylko odczyt
//! manifestów sklepów i rejestru - niczego nie zmienia.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Store {
    Steam,
    Epic,
    Gog,
    Ubisoft,
    Ea,
    Xbox,
    BattleNet,
    Riot,
}

impl Store {
    pub fn name(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
            Store::Gog => "GOG",
            Store::Ubisoft => "Ubisoft Connect",
            Store::Ea => "EA app",
            Store::Xbox => "Xbox",
            Store::BattleNet => "Battle.net",
            Store::Riot => "Riot Games",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    /// Identyfikator sklepu, np. "steam:899770".
    pub id: String,
    pub name: String,
    pub store: Store,
    pub dir: PathBuf,
}

/// Wszystkie znalezione gry, posortowane po nazwie, bez duplikatów folderów.
pub fn find_all() -> Vec<Game> {
    let mut games = Vec::new();
    games.extend(steam());
    games.extend(epic());
    #[cfg(windows)]
    {
        games.extend(gog());
        games.extend(ubisoft());
        games.extend(ea());
        games.extend(uninstall_games());
    }
    games.extend(riot_metadata());
    games.extend(xbox());
    games.retain(|g| g.dir.is_dir());
    // Ta sama gra może przyjść z dwóch źródeł (np. rejestr i metadane Riot):
    // duplikaty po folderze, potem kolejność po nazwie.
    games.sort_by_key(|g| path_key(&g.dir));
    games.dedup_by(|a, b| same_path(&a.dir, &b.dir));
    games.sort_by_key(|g| g.name.to_lowercase());
    games
}

/// Ścieżka do porównań: małe litery, ukośniki odwrotne, bez końcowego "\\".
pub fn path_key(p: &Path) -> String {
    p.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

// --------------------------------- Steam -----------------------------------

/// Programy Steam, które nie są grami.
const STEAM_TOOLS: &[&str] = &[
    "228980", "1070560", "1391110", "1628350", "1493710", "2180100",
];

/// Wartość pary `"klucz" "wartość"` z pliku VDF/ACF (pierwsze wystąpienie).
pub fn vdf_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(&needle) else {
            continue;
        };
        let rest = rest.trim();
        if let Some(inner) = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            return Some(inner.replace("\\\\", "\\"));
        }
    }
    None
}

/// Wszystkie wartości `"path"` z libraryfolders.vdf.
pub fn vdf_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("\"path\"")?.trim();
            let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
            Some(PathBuf::from(inner.replace("\\\\", "\\")))
        })
        .collect()
}

/// Gra z manifestu appmanifest_*.acf.
pub fn steam_game(acf: &str, library: &Path) -> Option<Game> {
    let id = vdf_value(acf, "appid")?;
    if STEAM_TOOLS.contains(&id.as_str()) {
        return None;
    }
    let name = vdf_value(acf, "name")?;
    let lower = name.to_lowercase();
    if lower.contains("redistributable")
        || lower.contains("proton")
        || lower.contains("steam linux runtime")
    {
        return None;
    }
    let dir = library
        .join("steamapps")
        .join("common")
        .join(vdf_value(acf, "installdir")?);
    Some(Game {
        id: format!("steam:{id}"),
        name,
        store: Store::Steam,
        dir,
    })
}

fn steam_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(p) = super::registry::Key::open(super::registry::HKCU, r"Software\Valve\Steam")
            .and_then(|k| k.string("SteamPath"))
        {
            return Some(PathBuf::from(p.replace('/', "\\")));
        }
    }
    let default = PathBuf::from(r"C:\Program Files (x86)\Steam");
    default.is_dir().then_some(default)
}

fn steam() -> Vec<Game> {
    let Some(root) = steam_root() else {
        return Vec::new();
    };
    let mut libraries = vec![root.clone()];
    if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        libraries.extend(vdf_paths(&text));
    }
    libraries.sort_by_key(|p| path_key(p));
    libraries.dedup_by(|a, b| same_path(a, b));
    let mut out = Vec::new();
    for library in libraries {
        let Ok(entries) = std::fs::read_dir(library.join("steamapps")) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            if let Some(game) = std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|acf| steam_game(&acf, &library))
            {
                out.push(game);
            }
        }
    }
    out
}

// --------------------------------- Epic ------------------------------------

/// Gra z manifestu Epic (*.item, JSON).
pub fn epic_game(json: &str) -> Option<Game> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v.get("bIsIncompleteInstall").and_then(|b| b.as_bool()) == Some(true) {
        return None;
    }
    let name = v.get("DisplayName")?.as_str()?.to_owned();
    let dir = PathBuf::from(v.get("InstallLocation")?.as_str()?);
    let app = v.get("AppName").and_then(|a| a.as_str()).unwrap_or(&name);
    Some(Game {
        id: format!("epic:{app}"),
        name,
        store: Store::Epic,
        dir,
    })
}

fn epic() -> Vec<Game> {
    let dir = Path::new(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests");
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "item"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|json| epic_game(&json))
        .collect()
}

// ------------------------- GOG / Ubisoft / EA (rejestr) ---------------------

#[cfg(windows)]
fn gog() -> Vec<Game> {
    use super::registry::{Key, HKLM};
    let Some(root) = Key::open(HKLM, r"SOFTWARE\WOW6432Node\GOG.com\Games") else {
        return Vec::new();
    };
    root.subkeys()
        .into_iter()
        .filter_map(|sub| {
            let k = Key::open(HKLM, &format!(r"SOFTWARE\WOW6432Node\GOG.com\Games\{sub}"))?;
            Some(Game {
                id: format!("gog:{sub}"),
                name: k.string("gameName")?,
                store: Store::Gog,
                dir: PathBuf::from(k.string("path")?),
            })
        })
        .collect()
}

#[cfg(windows)]
fn ubisoft() -> Vec<Game> {
    use super::registry::{Key, HKLM};
    let base = r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs";
    let Some(root) = Key::open(HKLM, base) else {
        return Vec::new();
    };
    root.subkeys()
        .into_iter()
        .filter_map(|sub| {
            let k = Key::open(HKLM, &format!(r"{base}\{sub}"))?;
            let dir = PathBuf::from(k.string("InstallDir")?.replace('/', "\\"));
            let name = dir.file_name()?.to_string_lossy().into_owned();
            Some(Game {
                id: format!("ubisoft:{sub}"),
                name,
                store: Store::Ubisoft,
                dir,
            })
        })
        .collect()
}

#[cfg(windows)]
fn ea() -> Vec<Game> {
    use super::registry::{Key, HKLM};
    let mut out = Vec::new();
    for base in [
        r"SOFTWARE\WOW6432Node\EA Games",
        r"SOFTWARE\WOW6432Node\Electronic Arts",
    ] {
        let Some(root) = Key::open(HKLM, base) else {
            continue;
        };
        for sub in root.subkeys() {
            let Some(k) = Key::open(HKLM, &format!(r"{base}\{sub}")) else {
                continue;
            };
            if let Some(dir) = k.string("Install Dir").or_else(|| k.string("InstallDir")) {
                out.push(Game {
                    id: format!("ea:{sub}"),
                    name: k.string("DisplayName").unwrap_or_else(|| sub.clone()),
                    store: Store::Ea,
                    dir: PathBuf::from(dir),
                });
            }
        }
    }
    out
}

// ------------------- Battle.net / Riot (rejestr deinstalacji) ----------------

/// Wartość argumentu `--nazwa=wartość` z wiersza polecenia deinstalatora.
fn cli_arg(cmd: &str, name: &str) -> Option<String> {
    let needle = format!("--{name}=");
    let rest = &cmd[cmd.find(&needle)? + needle.len()..];
    let rest = rest.trim_start_matches('"');
    let end = rest.find(['"', ' ']).unwrap_or(rest.len());
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Gra z wpisu w rejestrze deinstalacji Windows: Battle.net (deinstalator
/// Blizzard z `--uid=`) albo Riot Client (klucz `Riot Game <produkt>.<kanał>`).
pub fn uninstall_game(key: &str, name: &str, dir: &str, uninstall: &str) -> Option<Game> {
    if dir.trim().is_empty() || name.trim().is_empty() {
        return None;
    }
    let dir = PathBuf::from(dir.trim().trim_matches('"').replace('/', "\\"));
    let lower = uninstall.to_lowercase();
    if lower.contains("blizzard uninstaller") {
        let uid = cli_arg(uninstall, "uid")?;
        if uid.eq_ignore_ascii_case("battle.net") {
            return None;
        }
        return Some(Game {
            id: format!("battlenet:{uid}"),
            name: name.trim().to_owned(),
            store: Store::BattleNet,
            dir,
        });
    }
    let product = key.strip_prefix("Riot Game ")?;
    if product.to_lowercase().starts_with("riot_client") {
        return None;
    }
    Some(Game {
        id: format!("riot:{product}"),
        name: name.trim().to_owned(),
        store: Store::Riot,
        dir,
    })
}

#[cfg(windows)]
fn uninstall_games() -> Vec<Game> {
    use super::registry::{Key, HKCU, HKLM};
    let mut out = Vec::new();
    for (root, base) in [
        (HKLM, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKLM, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKCU, r"Software\Microsoft\Windows\CurrentVersion\Uninstall"),
    ] {
        let Some(parent) = Key::open(root, base) else {
            continue;
        };
        for sub in parent.subkeys() {
            let Some(k) = Key::open(root, &format!(r"{base}\{sub}")) else {
                continue;
            };
            let field = |v: &str| k.string(v).unwrap_or_default();
            if let Some(game) = uninstall_game(
                &sub,
                &field("DisplayName"),
                &field("InstallLocation"),
                &field("UninstallString"),
            ) {
                out.push(game);
            }
        }
    }
    out
}

/// Nazwa gry Riot po identyfikatorze produktu.
fn riot_name(product: &str) -> Option<&'static str> {
    Some(match product {
        "league_of_legends" => "League of Legends",
        "valorant" => "VALORANT",
        "bacon" => "Legends of Runeterra",
        "lion" => "2XKO",
        "teamfighttactics" => "Teamfight Tactics",
        _ => return None,
    })
}

/// Gra z pliku `<produkt>.<kanał>.product_settings.yaml` klienta Riot.
pub fn riot_game(file_stem: &str, yaml: &str) -> Option<Game> {
    let product_line = file_stem.strip_suffix(".product_settings")?;
    let product = product_line.split('.').next()?;
    if product.starts_with("riot_client") {
        return None;
    }
    let path = yaml.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("product_install_full_path:")?;
        Some(rest.trim().trim_matches(|c| c == '"' || c == '\'').to_owned())
    })?;
    if path.is_empty() {
        return None;
    }
    let dir = PathBuf::from(path.replace('/', "\\"));
    let name = match riot_name(product) {
        Some(n) => n.to_owned(),
        None => dir.file_name()?.to_string_lossy().into_owned(),
    };
    Some(Game {
        id: format!("riot:{product_line}"),
        name,
        store: Store::Riot,
        dir,
    })
}

/// Gry z metadanych klienta Riot (`C:\ProgramData\Riot Games\Metadata`).
fn riot_metadata() -> Vec<Game> {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Riot Games")
        .join("Metadata");
    let mut out = Vec::new();
    for product in std::fs::read_dir(base).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(product.path()).into_iter().flatten().flatten() {
            let path = file.path();
            if path.extension().is_none_or(|x| x != "yaml") {
                continue;
            }
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            if let Some(game) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|yaml| riot_game(&stem, &yaml))
            {
                out.push(game);
            }
        }
    }
    out
}

// --------------------------------- Xbox ------------------------------------

/// Gry z aplikacji Xbox: `<dysk>:\XboxGames\<gra>\Content`.
fn xbox() -> Vec<Game> {
    let mut out = Vec::new();
    for letter in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\XboxGames", letter as char));
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let content = entry.path().join("Content");
            if content.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                out.push(Game {
                    id: format!("xbox:{name}"),
                    name,
                    store: Store::Xbox,
                    dir: content,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_steam_library_and_manifest() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\Steam\"\n\t}\n}";
        assert_eq!(
            vdf_paths(vdf),
            vec![
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\Steam")
            ]
        );
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"899770\"\n\t\"name\"\t\t\"Last Epoch\"\n\t\"installdir\"\t\t\"Last Epoch\"\n}";
        let game = steam_game(acf, Path::new(r"D:\Steam")).unwrap();
        assert_eq!(game.id, "steam:899770");
        assert_eq!(game.name, "Last Epoch");
        assert_eq!(
            game.dir,
            PathBuf::from(r"D:\Steam\steamapps\common\Last Epoch")
        );
    }

    #[test]
    fn skips_steam_tools() {
        let acf = "\"appid\" \"228980\"\n\"name\" \"Steamworks Common Redistributables\"\n\"installdir\" \"Steamworks Shared\"";
        assert!(steam_game(acf, Path::new(r"D:\Steam")).is_none());
    }

    #[test]
    fn reads_epic_manifest() {
        let json = r#"{"DisplayName":"Alan Wake 2","InstallLocation":"E:\\Epic\\AlanWake2","AppName":"Dodo","bIsIncompleteInstall":false}"#;
        let game = epic_game(json).unwrap();
        assert_eq!(
            (game.id.as_str(), game.name.as_str()),
            ("epic:Dodo", "Alan Wake 2")
        );
        let partial =
            r#"{"DisplayName":"X","InstallLocation":"E:\\X","bIsIncompleteInstall":true}"#;
        assert!(epic_game(partial).is_none());
    }
    #[test]
    fn reads_battle_net_and_riot() {
        let cod = uninstall_game(
            "Call of Duty",
            "Call of Duty",
            r"D:\Call of Duty",
            r#""C:\ProgramData\Battle.net\Agent\Blizzard Uninstaller.exe" --lang=plPL --uid=auks --displayname="Call of Duty""#,
        )
        .unwrap();
        assert_eq!((cod.id.as_str(), cod.store), ("battlenet:auks", Store::BattleNet));
        assert_eq!(cod.dir, PathBuf::from(r"D:\Call of Duty"));
        let launcher = uninstall_game(
            "Battle.net",
            "Battle.net",
            r"D:\Battle.net",
            r#""C:\ProgramData\Battle.net\Agent\Blizzard Uninstaller.exe" --lang=plPL --uid=battle.net --displayname="Battle.net""#,
        );
        assert!(launcher.is_none());

        let lol = uninstall_game(
            "Riot Game league_of_legends.live",
            "League of Legends",
            "C:/Riot Games/League of Legends",
            r#""C:\Riot Games\Riot Client\RiotClientServices.exe" --uninstall-product=league_of_legends --uninstall-patchline=live"#,
        )
        .unwrap();
        assert_eq!(lol.id, "riot:league_of_legends.live");
        assert_eq!(lol.dir, PathBuf::from(r"C:\Riot Games\League of Legends"));
        assert!(uninstall_game("Riot Game Riot_Client.", "Riot Client", r"C:\Riot Games\Riot Client", "x").is_none());
        assert!(uninstall_game("7-Zip", "7-Zip", r"C:\7-Zip", r"C:\7-Zip\Uninstall.exe").is_none());

        let yaml = "product_install_full_path: \"C:/Riot Games/League of Legends\"\nproduct_install_root: \"C:/Riot Games\"\n";
        let game = riot_game("league_of_legends.live.product_settings", yaml).unwrap();
        assert_eq!(game.name, "League of Legends");
        assert_eq!(game.id, "riot:league_of_legends.live");
        assert_eq!(game.dir, PathBuf::from(r"C:\Riot Games\League of Legends"));
        assert!(riot_game("riot_client.product_settings", yaml).is_none());
    }
}
