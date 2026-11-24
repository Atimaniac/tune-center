//! Folder shaderów gier w danych aplikacji i dowiązania do pamięci podręcznej
//! sterownika.
//!
//! Sterownik karty trzyma skompilowane shadery wszystkich gier we wspólnym
//! folderze (NVIDIA: `%LOCALAPPDATA%\NVIDIA\DXCache`; AMD: `AMD\DxcCache` dla
//! DirectX 12 i `AMD\DxCache` dla DirectX 11) i tylko tam ich szuka. Tune
//! Center prowadzi obok własny folder - osobny podfolder na każdą grę - w
//! miejscu, które Windows przeznacza na dane aplikacji:
//! - program z Microsoft Store (pakiet MSIX):
//!   `%LOCALAPPDATA%\Packages\<rodzina pakietu>\LocalCache\Local\TuneCenter\Shaders`,
//! - program z instalatora: `%LOCALAPPDATA%\TuneCenter\Shaders`.
//!
//! Pliki gry w tym folderze są twardymi dowiązaniami (hard link) do plików w
//! `DXCache`: to te same dane pod dwiema nazwami, bez kopii na dysku. Twarde
//! dowiązanie, a nie skrót czy dowiązanie symboliczne, bo:
//! - sterownik widzi zwykły plik (skrótu `.lnk` by nie otworzył),
//! - nie wymaga uprawnień administratora ani trybu dewelopera,
//! - po odinstalowaniu programu shadery w `DXCache` zostają całe (nie ma
//!   „wiszących” odnośników).
//!
//! Gdy pliki znikną z `DXCache` (czyszczenie dysku, inne narzędzie), a
//! sterownik się nie zmienił, `restore` przywraca je z folderu gry bez
//! ponownej kompilacji.
//!
//! Zapis po obu stronach wykonuje proces uruchomiony poza pakietem (kopia
//! programu pod nazwą gry, patrz `warm`), więc pakiet nie potrzebuje żadnych
//! uprawnień zastrzeżonych do zapisu w danych użytkownika.

use std::path::{Path, PathBuf};

use super::warm::in_family;

/// Nazwa rodziny pakietu, gdy program działa jako pakiet (Microsoft Store).
#[cfg(windows)]
pub fn package_family() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentPackageFamilyName(len: *mut u32, name: *mut u16) -> i32;
    }
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: bufor i jego długość są poprawne przez całe wywołanie.
    let rc = unsafe { GetCurrentPackageFamilyName(&mut len, buf.as_mut_ptr()) };
    // 0 = sukces; 15700 (APPMODEL_ERROR_NO_PACKAGE) = program bez pakietu.
    (rc == 0 && len > 1).then(|| String::from_utf16_lossy(&buf[..len as usize - 1]))
}

#[cfg(not(windows))]
pub fn package_family() -> Option<String> {
    None
}

/// Folder danych Tune Center w `%LOCALAPPDATA%` - pełna, niewirtualizowana
/// ścieżka, taka sama dla procesu w pakiecie i poza nim.
pub fn app_data_dir() -> Option<PathBuf> {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let base = match package_family() {
        Some(family) => local
            .join("Packages")
            .join(family)
            .join("LocalCache")
            .join("Local"),
        None => local,
    };
    Some(base.join("TuneCenter"))
}

/// Folder shaderów gry o pliku `exe` (np. `aces.exe`).
pub fn game_dir(exe: &str) -> Option<PathBuf> {
    let name: String = exe
        .to_lowercase()
        .chars()
        .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    if name.is_empty() {
        return None;
    }
    Some(app_data_dir()?.join("Shaders").join(name))
}

/// Nazwy plików w folderze (bez podfolderów).
fn file_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.metadata().is_ok_and(|m| m.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Wynik dowiązania plików gry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Linked {
    pub files: usize,
    pub bytes: u64,
    pub failed: usize,
}

/// Pary (folder pamięci podręcznej, jego folder w folderze gry): pliki
/// pierwszego (głównego) folderu leżą wprost w folderze gry, pozostałych -
/// w podfolderze o tej samej nazwie (np. `DxCache` dla DirectX 11 na AMD).
fn pairs(caches: &[PathBuf], store: &Path) -> Vec<(PathBuf, PathBuf)> {
    caches
        .iter()
        .enumerate()
        .map(|(i, cache)| {
            let dir = match (i, cache.file_name()) {
                (0, _) | (_, None) => store.to_path_buf(),
                (_, Some(name)) => store.join(name),
            };
            (cache.clone(), dir)
        })
        .collect()
}

/// Dowiązuje do folderu gry `store` wszystkie pliki z `caches` należące do
/// gry (klucze `keys`, patrz `warm::in_family`). Folder gry po tej operacji
/// odpowiada stanowi pamięci podręcznej: stare pliki, których w niej już nie
/// ma, są usuwane.
pub fn link_game(caches: &[PathBuf], store: &Path, keys: &[String]) -> std::io::Result<Linked> {
    let mut out = Linked::default();
    for (cache, dir) in pairs(caches, store) {
        let current: Vec<String> = file_names(&cache)
            .into_iter()
            .filter(|n| in_family(n, keys))
            .collect();
        if current.is_empty() && !dir.exists() {
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        for old in file_names(&dir) {
            if !current.contains(&old) {
                let _ = std::fs::remove_file(dir.join(old));
            }
        }
        for name in current {
            let (from, to) = (cache.join(&name), dir.join(&name));
            // Dowiązanie zakłada się od nowa: sterownik mógł zastąpić plik nowym.
            let _ = std::fs::remove_file(&to);
            match std::fs::hard_link(&from, &to) {
                Ok(()) => {
                    out.files += 1;
                    out.bytes += std::fs::metadata(&to).map(|m| m.len()).unwrap_or(0);
                }
                Err(_) => out.failed += 1,
            }
        }
    }
    Ok(out)
}

/// Stan folderu gry względem pamięci podręcznej sterownika.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Info {
    pub files: usize,
    pub bytes: u64,
    /// Pliki z folderu gry, których nie ma już w pamięci podręcznej.
    pub missing: usize,
}

pub fn info(caches: &[PathBuf], store: &Path) -> Info {
    let mut out = Info::default();
    for (cache, dir) in pairs(caches, store) {
        for name in file_names(&dir) {
            out.files += 1;
            out.bytes += std::fs::metadata(dir.join(&name))
                .map(|m| m.len())
                .unwrap_or(0);
            if !cache.join(&name).exists() {
                out.missing += 1;
            }
        }
    }
    out
}

/// Przywraca w pamięci podręcznej pliki, które są w folderze gry, a z niej
/// zniknęły. Zwraca (przywrócone, nieudane). Istniejących plików nie rusza.
pub fn restore(caches: &[PathBuf], store: &Path) -> std::io::Result<(usize, usize)> {
    let (mut restored, mut failed) = (0, 0);
    for (cache, dir) in pairs(caches, store) {
        let names = file_names(&dir);
        if names.is_empty() {
            continue;
        }
        std::fs::create_dir_all(&cache)?;
        for name in names {
            let to = cache.join(&name);
            if to.exists() {
                continue;
            }
            match std::fs::hard_link(dir.join(&name), &to) {
                Ok(()) => restored += 1,
                Err(_) => failed += 1,
            }
        }
    }
    Ok((restored, failed))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("TuneCenter-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn link_restore_cycle() {
        let root = temp("store");
        let (cache, store) = (root.join("DXCache"), root.join("Shaders").join("game.exe"));
        std::fs::create_dir_all(&cache).unwrap();
        // Dwa pliki gry (ten sam skrót i kolejny numer) i plik innej gry.
        std::fs::write(cache.join("1c68a91ac748b201.nvph"), b"abcd").unwrap();
        std::fs::write(cache.join("1c68a91ac748b202.bin"), b"ef").unwrap();
        std::fs::write(cache.join("00000000aaaaaaaa.nvph"), b"other").unwrap();
        let keys = vec!["nv:c748b201".to_string()];

        let linked = link_game(std::slice::from_ref(&cache), &store, &keys).unwrap();
        assert_eq!(
            linked,
            Linked {
                files: 2,
                bytes: 6,
                failed: 0
            }
        );
        assert!(!store.join("00000000aaaaaaaa.nvph").exists());
        assert_eq!(
            info(std::slice::from_ref(&cache), &store),
            Info {
                files: 2,
                bytes: 6,
                missing: 0
            }
        );

        // Te same dane pod dwiema nazwami: zapis sterownika widać w folderze gry.
        std::fs::write(cache.join("1c68a91ac748b202.bin"), b"efgh").unwrap();
        assert_eq!(
            std::fs::read(store.join("1c68a91ac748b202.bin")).unwrap(),
            b"efgh"
        );

        // Plik zniknął z pamięci podręcznej - dane zostają w folderze gry.
        std::fs::remove_file(cache.join("1c68a91ac748b201.nvph")).unwrap();
        assert_eq!(info(std::slice::from_ref(&cache), &store).missing, 1);
        assert_eq!(restore(std::slice::from_ref(&cache), &store).unwrap(), (1, 0));
        assert_eq!(
            std::fs::read(cache.join("1c68a91ac748b201.nvph")).unwrap(),
            b"abcd"
        );
        assert_eq!(info(std::slice::from_ref(&cache), &store).missing, 0);

        // Ponowne dowiązanie usuwa z folderu gry pliki, których już nie ma.
        std::fs::remove_file(cache.join("1c68a91ac748b202.bin")).unwrap();
        let linked = link_game(std::slice::from_ref(&cache), &store, &keys).unwrap();
        assert_eq!(linked.files, 1);
        assert!(!store.join("1c68a91ac748b202.bin").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn game_dir_name() {
        let dir = game_dir("Aces.EXE").unwrap();
        assert!(dir.ends_with("TuneCenter/Shaders/aces.exe"));
        assert!(game_dir("").is_none());
    }
}
