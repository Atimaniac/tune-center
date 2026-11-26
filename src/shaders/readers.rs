//! Wspólny interfejs czytników silników, które dają shadery z samych plików
//! gry (bez kluczy i bez zapisu z gry). Każdy czytnik to osobny moduł:
//!
//! - `detect(game) -> Option<Detected>` - tani test przy skanowaniu gier,
//! - DirectX 12: `index(game, say) -> Result<Set12, String>`,
//! - DirectX 11: `for_each_shader(game, progress, each) -> Result<usize, String>`
//!   (`each` dostaje bajty zawierające kontenery DXBC; zwraca liczbę wpisów).
//!
//! Gry są tylko czytane.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Bajty shadera wczytywane na żądanie (wiele wątków).
#[derive(Clone)]
pub enum Bytes {
    Lazy(Arc<dyn Fn() -> Option<Vec<u8>> + Send + Sync>),
}

impl Bytes {
    pub fn get(&self) -> Option<Vec<u8>> {
        match self {
            Bytes::Lazy(f) => f(),
        }
    }
}

/// Potok DirectX 12 znany z plików gry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pipeline12 {
    /// Root signature: indeks w `Set12::signatures`; `None` - wbudowana w
    /// shader (część RTS0 pierwszego etapu, który ją ma).
    pub rs: Option<usize>,
    /// Etapy VS, PS, GS, HS, DS, CS: indeksy w `Set12::shaders`.
    pub stages: [Option<usize>; 6],
}

/// Shadery DirectX 12 gry.
#[derive(Default)]
pub struct Set12 {
    pub shaders: Vec<Bytes>,
    /// Zserializowane root signature (jak z `D3D12SerializeRootSignature`
    /// albo część RTS0).
    pub signatures: Vec<Vec<u8>>,
    /// Potoki dokładnie takie, jakie tworzy gra.
    pub pipelines: Vec<Pipeline12>,
    /// Pule shaderów bez znanych par: shadery wierzchołków i pikseli łączone
    /// po semantykach sygnatur, compute osobno. Druga wartość - root
    /// signature puli (`None` - wbudowana w shadery).
    pub pools: Vec<(Vec<usize>, Option<usize>)>,
    /// Uwagi do wyświetlenia (liczby, pominięte pliki).
    pub notes: Vec<String>,
}

/// Wynik rozpoznania gry.
#[derive(Clone, Debug, PartialEq)]
pub struct Detected {
    /// Nazwa silnika do wyświetlenia (np. "RAGE gen9").
    pub engine: &'static str,
    pub version: Option<String>,
    /// "DirectX 12" albo "DirectX 11".
    pub api: &'static str,
    /// Pliki z shaderami (do listy w interfejsie): ścieżka i rozmiar.
    pub files: Vec<(PathBuf, u64)>,
    /// Plik .exe gry (nazwa pliku), jeśli czytnik go zna; inaczej wybór ogólny.
    pub exe: Option<String>,
}

/// Czytniki w kolejności sprawdzania.
pub const KINDS: [&str; 15] = [
    "rage", "forza", "santamonica", "northlight", "luminous", "gow1", "cryengine", "snowdrop", "nixxes", "stingray", "tank",
    "zouna", "dawn", "frostbite", "unreal12",
];

/// Rozpoznanie gry przez czytniki: (rodzaj, wynik).
pub fn detect(game: &Path) -> Option<(&'static str, Detected)> {
    let tries: [(&'static str, fn(&Path) -> Option<Detected>); 14] = [
        ("rage", super::rage::detect),
        ("forza", super::forza::detect),
        ("santamonica", super::santamonica::detect),
        ("northlight", super::northlight::detect),
        ("luminous", super::luminous::detect),
        ("gow1", super::santamonica::detect_v1),
        ("cryengine", super::cryengine::detect),
        ("snowdrop", super::snowdrop::detect),
        ("nixxes", super::nixxes::detect),
        ("stingray", super::nixxes::detect_stingray),
        ("tank", super::tank::detect),
        ("zouna", super::zouna::detect),
        ("dawn", super::dawn::detect),
        ("frostbite", super::frostbite::detect),
    ];
    tries.iter().find_map(|(k, f)| f(game).map(|d| (*k, d)))
}

/// Czy rodzaj to DirectX 12 (inaczej DirectX 11).
pub fn is_dx12(kind: &str) -> bool {
    matches!(
        kind,
        "rage" | "forza" | "santamonica" | "northlight" | "cryengine" | "snowdrop" | "nixxes" | "zouna" | "dawn" | "unreal12"
    )
}

/// Shadery DirectX 12 rodzaju `kind`.
pub fn index12(kind: &str, game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    match kind {
        "rage" => super::rage::index(game, say),
        "forza" => super::forza::index(game, say),
        "santamonica" => super::santamonica::index(game, say),
        "northlight" => super::northlight::index(game, say),
        "cryengine" => super::cryengine::index(game, say),
        "snowdrop" => super::snowdrop::index(game, say),
        "nixxes" => super::nixxes::index(game, say),
        "zouna" => super::zouna::index(game, say),
        "dawn" => super::dawn::index(game, say),
        "unreal12" => super::ue_maps::index(game, say),
        other => Err(format!("nieznany rodzaj / unknown kind: {other}")),
    }
}

/// Shadery DirectX 11 rodzaju `kind`.
pub fn for_each11(
    kind: &str,
    game: &Path,
    progress: &mut dyn FnMut(usize, usize),
    each: &mut dyn FnMut(&[u8]),
) -> Result<usize, String> {
    match kind {
        "luminous" => super::luminous::for_each_shader(game, progress, each),
        "gow1" => super::santamonica::for_each_shader_v1(game, progress, each),
        "stingray" => super::nixxes::for_each_shader_stingray(game, progress, each),
        "tank" => super::tank::for_each_shader(game, progress, each),
        "frostbite" => super::frostbite::for_each_shader(game, progress, each),
        other => Err(format!("nieznany rodzaj / unknown kind: {other}")),
    }
}


// ------------------------------ pomocniki ------------------------------------

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Biblioteki importowane przez plik .exe (tabela importów i importów
/// opóźnionych), małymi literami. Czyta tylko nagłówki i tabele (kilka KB).
pub fn pe_imports(exe: &Path) -> Option<Vec<String>> {
    use super::container::read_at;
    let file = std::fs::File::open(exe).ok()?;
    let len = file.metadata().ok()?.len();
    let mut head = vec![0u8; 4096.min(len as usize)];
    read_at(&file, 0, &mut head).ok()?;
    if head.get(..2)? != b"MZ" {
        return None;
    }
    let pe = le32(&head, 0x3C)? as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = le16(&head, pe + 6)? as usize;
    let opt_size = le16(&head, pe + 20)? as usize;
    let opt = pe + 24;
    // Katalogi danych: PE32+ od 112, PE32 od 96.
    let dirs = opt + if le16(&head, opt)? == 0x20B { 112 } else { 96 };
    let table = opt + opt_size;
    // (adres wirtualny, rozmiar, adres w pliku) sekcji.
    let mut map = Vec::new();
    for s in 0..sections.min(96) {
        let at = table + 40 * s;
        map.push((le32(&head, at + 12)?, le32(&head, at + 8)?.max(le32(&head, at + 16)?), le32(&head, at + 20)?));
    }
    let offset = |rva: u32| -> Option<u64> {
        map.iter()
            .find(|(va, size, _)| rva >= *va && rva < va.saturating_add(*size))
            .map(|(va, _, raw)| u64::from(rva - va) + u64::from(*raw))
    };
    let read = |rva: u32, n: usize| -> Option<Vec<u8>> {
        let at = offset(rva)?;
        let n = n.min(len.saturating_sub(at) as usize);
        let mut b = vec![0u8; n];
        read_at(&file, at, &mut b).ok()?;
        Some(b)
    };
    let mut out = Vec::new();
    // (katalog, rozmiar deskryptora, przesunięcie nazwy w deskryptorze).
    for (dir, stride, name_at) in [(1usize, 20usize, 12usize), (13, 32, 4)] {
        let Some(rva) = le32(&head, dirs + 8 * dir).filter(|r| *r != 0) else { continue };
        let Some(desc) = read(rva, 20 * 1024) else { continue };
        for k in 0..desc.len() / stride {
            let Some(name) = le32(&desc, k * stride + name_at).filter(|n| *n != 0) else { break };
            if let Some(b) = read(name, 64) {
                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                out.push(String::from_utf8_lossy(&b[..end]).to_lowercase());
            }
        }
    }
    Some(out)
}

/// Czy plik wspomina "d3d11" i "d3d12" (ASCII albo UTF-16, bez względu na
/// wielkość liter) - także biblioteki ładowane dynamicznie, których nie ma w
/// tabeli importów. Czyta cały plik kawałkami.
pub fn mentions_d3d(exe: &Path) -> Option<(bool, bool)> {
    use super::container::read_at;
    let file = std::fs::File::open(exe).ok()?;
    let len = file.metadata().ok()?.len();
    const CHUNK: u64 = 8 << 20;
    let (mut d11, mut d12) = (false, false);
    let mut buf = Vec::new();
    let mut at = 0u64;
    while at < len && !(d11 && d12) {
        // Zakładka 16 B: tekst na granicy kawałka.
        let n = (len - at).min(CHUNK + 16) as usize;
        buf.resize(n, 0);
        read_at(&file, at, &mut buf).ok()?;
        let lower: Vec<u8> = buf.iter().map(u8::to_ascii_lowercase).collect();
        let has = |w: &[u8]| {
            let wide: Vec<u8> = w.iter().flat_map(|&c| [c, 0]).collect();
            lower.windows(w.len()).any(|x| x == w) || lower.windows(wide.len()).any(|x| x == wide.as_slice())
        };
        d11 |= has(b"d3d11");
        d12 |= has(b"d3d12");
        at += CHUNK;
    }
    Some((d11, d12))
}

/// Wynik kosztownego rozpoznania zapamiętany przy stemplu plików (ścieżka,
/// rozmiar, czas zmiany): `scan` działa tylko, gdy pliki się zmieniły albo
/// wyniku jeszcze nie ma. Zapis w danych programu (`Cache\detect`).
pub fn cached_detect(kind: &str, game: &Path, files: &[(PathBuf, u64)], scan: impl FnOnce() -> bool) -> bool {
    use std::hash::{Hash, Hasher};
    let mut stamp = String::new();
    for (path, len) in files {
        let time = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        stamp.push_str(&format!("{}|{len}|{time}\n", super::games::path_key(path)));
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind, super::games::path_key(game)).hash(&mut h);
    let file = super::store::app_data_dir().map(|d| d.join("Cache").join("detect").join(format!("{kind}-{:016x}.txt", h.finish())));
    if let Some(saved) = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
        if let Some(rest) = saved.strip_prefix(&stamp) {
            match rest.trim() {
                "1" => return true,
                "0" => return false,
                _ => {}
            }
        }
    }
    let found = scan();
    if let Some(f) = file {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, format!("{stamp}{}\n", u8::from(found)));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pe_imports_of_a_real_exe() {
        let exe = std::env::current_exe().unwrap();
        let libs = pe_imports(&exe).unwrap();
        assert!(libs.iter().any(|l| l == "kernel32.dll"), "{libs:?}");
        // Plik, który nie jest programem.
        let junk = std::env::temp_dir().join(format!("tc-pe-{}.exe", std::process::id()));
        std::fs::write(&junk, b"MZ not really").unwrap();
        assert!(pe_imports(&junk).is_none());
        let _ = std::fs::remove_file(junk);
        // Gra DirectX 12 z tego komputera, jeśli jest.
        let witcher = Path::new(r"D:\Steam\steamapps\common\The Witcher 3\bin\x64_dx12\witcher3.exe");
        if witcher.is_file() {
            // Ładuje d3d12 dynamicznie: nie ma go w importach, jest w tekstach.
            assert!(!pe_imports(witcher).unwrap().is_empty());
            assert_eq!(mentions_d3d(witcher), Some((false, true)));
        }
    }

    #[test]
    fn caches_detection_until_files_change() {
        let game = std::env::temp_dir().join(format!("tc-detect-{}", std::process::id()));
        std::fs::create_dir_all(&game).unwrap();
        let file = game.join("a.rpf");
        std::fs::write(&file, b"1234").unwrap();
        let files = vec![(file.clone(), 4u64)];
        let runs = std::cell::Cell::new(0);
        let scan = || {
            runs.set(runs.get() + 1);
            true
        };
        assert!(cached_detect("test", &game, &files, scan));
        assert!(cached_detect("test", &game, &files, scan));
        assert_eq!(runs.get(), 1);
        // Inny rozmiar - nowe przeszukanie.
        let changed = vec![(file, 5u64)];
        assert!(cached_detect("test", &game, &changed, scan));
        assert_eq!(runs.get(), 2);
        let _ = std::fs::remove_dir_all(&game);
        if let Some(dir) = super::super::store::app_data_dir() {
            for e in std::fs::read_dir(dir.join("Cache").join("detect")).into_iter().flatten().flatten() {
                if e.file_name().to_string_lossy().starts_with("test-") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}
