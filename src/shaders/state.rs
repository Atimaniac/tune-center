//! Stan kompilacji shaderów gier: co, kiedy i dla jakiego sterownika zostało
//! skompilowane (plik `shader-state.json` obok programu).
//!
//! Gra wymaga ponownej kompilacji, gdy:
//! - zmieniła się wersja sterownika NVIDIA (instalacja sterownika czyści jego
//!   pamięć podręczną shaderów),
//! - zmieniły się pliki shaderów gry (aktualizacja gry) - „odcisk” to liczba
//!   plików i ich łączny rozmiar.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::engine::Info;

const FILE: &str = "shader-state.json";

/// Wynik ostatniej udanej kompilacji gry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    /// Data i godzina, np. "2026-11-22 23:05".
    pub when: String,
    /// Wersja sterownika NVIDIA w chwili kompilacji, np. "621.35".
    pub driver: String,
    /// "DX11" albo "DX12".
    pub api: String,
    pub ok: usize,
    pub failed: usize,
    pub secs: u64,
    /// Rozmiar pamięci podręcznej sterownika dla gry po kompilacji.
    #[serde(default)]
    pub cache_bytes: Option<u64>,
    /// Odcisk plików shaderów gry (patrz `stamp`).
    pub stamp: String,
    /// Plik .exe gry - nazwa jej folderu shaderów (patrz `store`).
    #[serde(default)]
    pub exe: String,
    /// Ile plików pamięci podręcznej dowiązano do folderu shaderów gry.
    #[serde(default)]
    pub linked: Option<usize>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Saved {
    #[serde(default)]
    pub games: BTreeMap<String, Record>,
    /// Kompiluj w tle gry, które po instalacji nowego sterownika tego wymagają.
    #[serde(default)]
    pub auto_after_driver: bool,
    /// Wersja sterownika, dla której automatyczna kompilacja już ruszyła.
    #[serde(default)]
    pub auto_driver: String,
}

/// Program z instalatora: plik obok programu. Program z Microsoft Store: w
/// folderze danych aplikacji, bo folder pakietu jest tylko do odczytu.
fn path() -> Option<PathBuf> {
    if super::store::package_family().is_some() {
        let dir = super::store::app_data_dir()?;
        let _ = std::fs::create_dir_all(&dir);
        return Some(dir.join(FILE));
    }
    Some(std::env::current_exe().ok()?.parent()?.join(FILE))
}

pub fn load() -> Saved {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(s: &Saved) {
    if let (Some(p), Ok(json)) = (path(), serde_json::to_string_pretty(s)) {
        let _ = std::fs::write(p, json);
    }
}

/// Odcisk plików shaderów gry: liczba plików i łączny rozmiar.
pub fn stamp(info: &Info) -> String {
    format!("{}:{}", info.shaders.len(), info.shader_bytes())
}

/// Sam numer wersji sterownika ("621.35 (r620_00-...)" → "621.35").
pub fn short_driver(driver: &str) -> String {
    driver
        .split([' ', '('])
        .next()
        .unwrap_or(driver)
        .trim()
        .to_owned()
}

/// Stan gry na liście.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Nigdy nie kompilowana w Tune Center.
    Never,
    /// Skompilowana dla bieżącego sterownika i bieżących plików gry.
    Fresh,
    /// Od kompilacji zainstalowano inny sterownik (pamięć podręczna wyczyszczona).
    NewDriver { was: String },
    /// Pliki shaderów gry się zmieniły (aktualizacja gry).
    GameUpdated,
}

impl Status {
    pub fn needs_compile(&self) -> bool {
        *self != Status::Fresh
    }
}

pub fn status(record: Option<&Record>, info: &Info, driver: Option<&str>) -> Status {
    let Some(r) = record else {
        return Status::Never;
    };
    if let Some(d) = driver {
        if !r.driver.is_empty() && short_driver(d) != r.driver {
            return Status::NewDriver {
                was: r.driver.clone(),
            };
        }
    }
    if r.stamp != stamp(info) {
        return Status::GameUpdated;
    }
    Status::Fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(driver: &str, stamp: &str) -> Record {
        Record {
            when: String::new(),
            driver: driver.into(),
            api: "DX11".into(),
            ok: 1,
            failed: 0,
            secs: 1,
            cache_bytes: None,
            stamp: stamp.into(),
            exe: String::new(),
            linked: None,
        }
    }

    #[test]
    fn status_rules() {
        let info = Info {
            engine: super::super::engine::Engine::Unknown,
            version: None,
            api: None,
            anti_cheat: Vec::new(),
            shaders: Vec::new(),
            scskiller_recorder: false,
            encrypted: false,
            ue_pso_cache: false,
        };
        let s = stamp(&info);
        assert_eq!(status(None, &info, Some("621.35")), Status::Never);
        assert_eq!(
            status(Some(&record("621.35", &s)), &info, Some("621.35 (r620)")),
            Status::Fresh
        );
        assert_eq!(
            status(Some(&record("620.10", &s)), &info, Some("621.35")),
            Status::NewDriver {
                was: "620.10".into()
            }
        );
        assert_eq!(
            status(Some(&record("621.35", "9:9")), &info, Some("621.35")),
            Status::GameUpdated
        );
        // Bez znanej wersji sterownika liczą się tylko pliki gry.
        assert_eq!(
            status(Some(&record("620.10", &s)), &info, None),
            Status::Fresh
        );
        assert_eq!(short_driver("621.35 (r620_00-123)"), "621.35");
        assert_eq!(short_date("2026-11-22 23:03"), "22.11");
    }
}

/// Krótka data do listy: "2026-11-22 23:03" → "22.11".
pub fn short_date(when: &str) -> String {
    match when.split([' ', '-']).collect::<Vec<_>>().as_slice() {
        [_, m, d, ..] => format!("{d}.{m}"),
        _ => when.to_owned(),
    }
}
