//! Aktualizacja sterownika NVIDIA: sprawdzanie najnowszego sterownika Game
//! Ready dla karty na serwerze NVIDIA i cicha instalacja samego sterownika.
//!
//! Wyszukiwanie jak na stronie nvidia.com/drivers: identyfikator produktu z
//! `Download/API/lookupValueSearch.aspx?TypeID=3` (po nazwie karty), potem
//! `AjaxDriverService.php?func=DriverManualLookup` (Windows 10/11 64-bit,
//! DCH, WHQL, Game Ready). Paczka to archiwum 7-Zip SFX: rozpakowujemy tylko
//! sterownik grafiki, audio HDMI, PhysX i instalator (bez NVIDIA App i
//! telemetrii - jak NVCleanstall / TinyNvidiaUpdateChecker), usuwamy z
//! `setup.cfg` odwołania do pominiętych plików EULA i uruchamiamy
//! `setup.exe -s -noreboot -noeula` (instalacja cicha, ekran mignie).
//! Pobierane są tylko pliki z serwerów download.nvidia.com.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Najnowszy sterownik dla karty.
#[derive(Clone, Debug, PartialEq)]
pub struct Latest {
    pub version: String,
    pub url: String,
    /// Strona sterownika (opis zmian).
    pub details: String,
    /// Rozmiar podany przez NVIDIA, np. "1.04 GB".
    pub size: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate {
        installed: String,
    },
    Available {
        installed: String,
        latest: Latest,
    },
    Downloading {
        version: String,
        mb: u64,
    },
    Extracting {
        version: String,
    },
    Installing {
        version: String,
    },
    /// Zainstalowano; Windows może potrzebować restartu.
    Done {
        version: String,
    },
    Failed(String),
}

/// Porównanie wersji "621.14" < "621.24".
pub fn newer(latest: &str, installed: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(latest) > parse(installed)
}

/// Pliki spod innych adresów niż serwery NVIDIA są odrzucane.
pub fn trusted(url: &str) -> bool {
    [
        "https://us.download.nvidia.com/",
        "https://international.download.nvidia.com/",
    ]
    .iter()
    .any(|p| url.starts_with(p))
}

/// Identyfikator produktu z listy NVIDIA (`<Name>GeForce RTX 5070</Name><Value>1070</Value>`).
pub fn product_id(xml: &str, gpu: &str) -> Option<u32> {
    // Nowsze karty są na liście z przedrostkiem "NVIDIA ", starsze bez niego.
    let name = gpu.trim().trim_start_matches("NVIDIA ").trim();
    [format!("NVIDIA {name}"), name.to_owned()]
        .iter()
        .find_map(|n| {
            // Między znacznikami bywają znaki końca wiersza (CR).
            let needle = format!("<Name>{n}</Name>");
            let after = &xml[xml.find(&needle)? + needle.len()..];
            let value = &after[after.find("<Value>")? + "<Value>".len()..];
            value[..value.find('<')?].trim().parse().ok()
        })
}

/// Odpowiedź `DriverManualLookup`.
pub fn parse_lookup(json: &str) -> Result<Latest, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let info = &v["IDS"][0]["downloadInfo"];
    let field = |k: &str| info[k].as_str().unwrap_or("").to_owned();
    let latest = Latest {
        version: field("Version"),
        url: field("DownloadURL"),
        details: field("DetailsURL"),
        size: field("DownloadURLFileSize"),
    };
    if latest.version.is_empty() || latest.url.is_empty() {
        return Err(crate::i18n::tr(
            "serwer NVIDIA nie zwrócił sterownika dla tej karty",
            "the NVIDIA server returned no driver for this card",
        )
        .to_owned());
    }
    Ok(latest)
}

fn fetch(url: &str) -> Result<String, String> {
    let out = crate::update::curl()
        .args([
            "-sS",
            "-L",
            "--fail",
            "--max-time",
            "20",
            "-A",
            "TuneCenter",
            url,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(crate::i18n::tr(
            "brak połączenia z serwerem NVIDIA",
            "no connection to the NVIDIA server",
        )
        .to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Najnowszy sterownik Game Ready (WHQL, DCH, Windows 10/11 64-bit) dla karty.
pub fn lookup(gpu: &str) -> Result<Latest, String> {
    let xml = fetch("https://www.nvidia.com/Download/API/lookupValueSearch.aspx?TypeID=3")?;
    let pfid = product_id(&xml, gpu).ok_or_else(|| {
        crate::trf!(
            "karty \"{gpu}\" nie ma na liście NVIDIA",
            "the card \"{gpu}\" is not on the NVIDIA list"
        )
    })?;
    let url = format!(
        "https://gfwsl.geforce.com/services_toolkit/services/com/nvidia/services/AjaxDriverService.php?func=DriverManualLookup&pfid={pfid}&osID=57&languageCode=1033&isWHQL=1&dch=1&upCRD=0&qnf=0&sort1=1&numberOfResults=1"
    );
    parse_lookup(&fetch(&url)?)
}

fn work_dir() -> PathBuf {
    std::env::temp_dir().join("TuneCenter").join("nvdriver")
}

/// Pobiera paczkę (z podglądem postępu w MB).
fn download(latest: &Latest, set: &dyn Fn(State)) -> Result<PathBuf, String> {
    if !trusted(&latest.url) {
        return Err(crate::trf!(
            "niezaufany adres sterownika: {}",
            "untrusted driver address: {}",
            latest.url
        ));
    }
    let dir = work_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.exe", latest.version));
    let part = dir.join(format!("{}.part", latest.version));
    if path.exists() {
        return Ok(path);
    }
    let _ = std::fs::remove_file(&part);
    let mut child = crate::update::curl()
        .args(["-sS", "-L", "--fail", "-A", "TuneCenter", "-o"])
        .arg(&part)
        .arg(&latest.url)
        .spawn()
        .map_err(|e| e.to_string())?;
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => break,
            Some(_) => {
                let _ = std::fs::remove_file(&part);
                return Err(crate::i18n::tr(
                    "pobieranie sterownika nie powiodło się",
                    "downloading the driver failed",
                )
                .to_owned());
            }
            None => {
                let mb = std::fs::metadata(&part).map(|m| m.len() >> 20).unwrap_or(0);
                set(State::Downloading {
                    version: latest.version.clone(),
                    mb,
                });
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    // Paczka sterownika ma ok. 1 GB; mniejszy plik to na pewno nie ona.
    let len = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if len < 100 << 20 {
        let _ = std::fs::remove_file(&part);
        return Err(crate::i18n::tr(
            "pobrany plik nie jest paczką sterownika",
            "the downloaded file is not a driver package",
        )
        .to_owned());
    }
    std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Składniki paczki instalowane przez program (sam sterownik).
const PARTS: &[&str] = &[
    "Display.Driver",
    "HDAudio",
    "PhysX",
    "NVI2",
    "EULA.txt",
    "ListDevices.kml",
    "setup.cfg",
    "setup.exe",
];

/// Rozpakowuje wybrane składniki: `tar.exe` z Windows (libarchive czyta 7z),
/// a gdy go brak albo zawiedzie - 7-Zip, jeśli jest zainstalowany.
fn extract(package: &Path, dir: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let run = |mut cmd: std::process::Command| -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd.status().map(|s| s.success()).unwrap_or(false)
    };
    // Pełna ścieżka: w PATH bywa inny tar (np. z Git), który nie zna 7z.
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:Windows".to_owned());
    let mut tar =
        std::process::Command::new(PathBuf::from(system).join("System32").join("tar.exe"));
    tar.arg("-xf").arg(package).arg("-C").arg(dir).args(PARTS);
    let ok = run(tar) && dir.join("setup.exe").exists();
    if !ok {
        let seven = PathBuf::from(r"C:\Program Files\7-Zip\7z.exe");
        if !seven.exists() {
            return Err(crate::i18n::tr(
                "nie udało się rozpakować paczki (tar.exe) i nie ma 7-Zip",
                "could not unpack the package (tar.exe) and 7-Zip is not installed",
            )
            .to_owned());
        }
        let mut cmd = std::process::Command::new(seven);
        cmd.arg("x")
            .arg("-y")
            .arg(package)
            .arg(format!("-o{}", dir.display()))
            .args(PARTS);
        if !run(cmd) || !dir.join("setup.exe").exists() {
            return Err(crate::i18n::tr(
                "nie udało się rozpakować paczki sterownika",
                "could not unpack the driver package",
            )
            .to_owned());
        }
    }
    // Bez pominiętych plików EULA i zgód instalator odmawia pracy - usuwamy
    // odwołania do nich z setup.cfg.
    let cfg = dir.join("setup.cfg");
    let text = std::fs::read_to_string(&cfg).map_err(|e| e.to_string())?;
    let cleaned: String = text
        .lines()
        .filter(|l| {
            !(l.contains("EulaHtmlFile")
                || l.contains("FunctionalConsentFile")
                || l.contains("PrivacyPolicyFile"))
        })
        .map(|l| format!("{l}\r\n"))
        .collect();
    std::fs::write(&cfg, cleaned).map_err(|e| e.to_string())
}

/// Cicha instalacja z rozpakowanego katalogu.
fn install(dir: &Path) -> Result<(), String> {
    let status = std::process::Command::new(dir.join("setup.exe"))
        .args(["-s", "-noreboot", "-noeula", "-nofinish", "-nosplash"])
        .current_dir(dir)
        .status()
        .map_err(|e| e.to_string())?;
    match status.code() {
        Some(0) => Ok(()),
        c => Err(crate::trf!(
            "instalator NVIDIA zakończył się kodem {c:?}",
            "the NVIDIA installer exited with code {c:?}"
        )),
    }
}

/// Sprawdzanie i instalacja w osobnym wątku.
pub struct Checker {
    state: Arc<Mutex<State>>,
}

impl Checker {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::Idle)),
        }
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or(State::Idle)
    }

    fn busy(&self) -> bool {
        matches!(
            self.state(),
            State::Checking
                | State::Downloading { .. }
                | State::Extracting { .. }
                | State::Installing { .. }
        )
    }

    /// Sprawdza najnowszy sterownik dla karty `gpu` (nazwa z NVAPI) wobec
    /// zainstalowanej wersji `installed` (np. "621.14").
    pub fn check(&self, gpu: String, wake: impl Fn() + Send + 'static) {
        if self.busy() {
            return;
        }
        let state = self.state.clone();
        *state.lock().unwrap() = State::Checking;
        std::thread::spawn(move || {
            // Wersja zainstalowanego sterownika z NVAPI ("621.14 (r621_02)").
            let installed = crate::nvcp::api::Api::load()
                .ok()
                .and_then(|a| a.driver())
                .unwrap_or_default();
            let installed = installed.split(' ').next().unwrap_or("").to_owned();
            let result = match lookup(&gpu) {
                Ok(latest) if newer(&latest.version, &installed) => {
                    State::Available { installed, latest }
                }
                Ok(_) => State::UpToDate { installed },
                Err(e) => State::Failed(e),
            };
            *state.lock().unwrap() = result;
            wake();
        });
    }

    /// Pobiera, rozpakowuje i instaluje dostępny sterownik.
    pub fn install(&self, wake: impl Fn() + Send + Sync + 'static) {
        let State::Available { latest, .. } = self.state() else {
            return;
        };
        let state = self.state.clone();
        std::thread::spawn(move || {
            let set = |s: State| {
                *state.lock().unwrap() = s;
                wake();
            };
            let result = (|| -> Result<(), String> {
                let package = download(&latest, &set)?;
                set(State::Extracting {
                    version: latest.version.clone(),
                });
                let dir = work_dir().join(format!("{}-files", latest.version));
                extract(&package, &dir)?;
                set(State::Installing {
                    version: latest.version.clone(),
                });
                install(&dir)?;
                // Sprzątanie: paczka i rozpakowane pliki zajmują ok. 2 GB.
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&package);
                Ok(())
            })();
            set(match result {
                Ok(()) => State::Done {
                    version: latest.version.clone(),
                },
                Err(e) => State::Failed(e),
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{newer, parse_lookup, product_id, trusted};

    #[test]
    fn versions_and_ids() {
        assert!(newer("621.24", "621.14"));
        assert!(!newer("621.14", "621.14"));
        assert!(newer("622.01", "621.99"));
        assert!(!newer("591.86", "621.14"));
        let xml = r#"<LookupValue ParentID="1"><Name>GeForce RTX 5070 Ti</Name><Value>1068</Value></LookupValue><LookupValue ParentID="131"><Name>NVIDIA GeForce RTX 5070</Name><Value>1070</Value></LookupValue><Name>GeForce GTX 1080</Name><Value>815</Value>"#;
        assert_eq!(product_id(xml, "NVIDIA GeForce GTX 1080"), Some(815));
        assert_eq!(product_id(xml, "NVIDIA GeForce RTX 5070"), Some(1070));
        assert_eq!(product_id(xml, "NVIDIA GeForce RTX 5070 Ti"), Some(1068));
        assert_eq!(product_id(xml, "NVIDIA GeForce RTX 9999"), None);
        assert!(trusted(
            "https://us.download.nvidia.com/Windows/621.24/x.exe"
        ));
        assert!(!trusted("https://example.com/x.exe"));
        let json = r#"{ "Success" : "1", "IDS" : [{ "downloadInfo": { "Version" : "621.24", "DetailsURL" : "https://www.nvidia.com/en-us/drivers/details/285320/", "DownloadURL" : "https://us.download.nvidia.com/Windows/621.24/621.24-desktop-win10-win11-64bit-international-dch-whql.exe", "DownloadURLFileSize" : "1.04 GB" } }] }"#;
        let l = parse_lookup(json).unwrap();
        assert_eq!((l.version.as_str(), l.size.as_str()), ("621.24", "1.04 GB"));
    }

    /// Zapytanie do serwera NVIDIA: `cargo test nvdriver_lookup -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn nvdriver_lookup() {
        let xml =
            super::fetch("https://www.nvidia.com/Download/API/lookupValueSearch.aspx?TypeID=3")
                .unwrap();
        println!(
            "xml {} B, id {:?}",
            xml.len(),
            super::product_id(&xml, "NVIDIA GeForce RTX 5070")
        );
        println!("{:?}", super::lookup("NVIDIA GeForce RTX 5070"));
    }

    /// Rozpakowanie pobranej paczki (TC_NV_PACKAGE=<plik.exe>), bez instalacji.
    #[test]
    #[ignore]
    fn nvdriver_extract() {
        let pkg = std::env::var("TC_NV_PACKAGE").expect("TC_NV_PACKAGE");
        let dir = std::env::temp_dir()
            .join("TuneCenter")
            .join("nvdriver")
            .join("extract-test");
        let t = std::time::Instant::now();
        super::extract(std::path::Path::new(&pkg), &dir).unwrap();
        println!("rozpakowano w {:?}", t.elapsed());
        for e in std::fs::read_dir(&dir).unwrap() {
            println!("  {}", e.unwrap().file_name().to_string_lossy());
        }
        let cfg = std::fs::read_to_string(dir.join("setup.cfg")).unwrap();
        assert!(!cfg.contains("EulaHtmlFile"));
    }
}
