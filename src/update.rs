//! Aktualizacje z GitHuba.
//!
//! Program pyta o najnowsze wydanie repozytorium (`releases/latest`) i
//! porównuje jego znacznik (np. `v0.17.0`) z własną wersją. Gdy wydanie jest
//! nowsze i ma załącznik `*Setup.exe` z adresu wydań tego repozytorium,
//! przycisk "Zainstaluj X" pobiera instalator do katalogu tymczasowego,
//! uruchamia go w trybie cichym (`/SILENT`) i zamyka program; instalator po
//! aktualizacji uruchamia program ponownie. Załączniki spod innych adresów
//! są odrzucane (`trusted_installer`) - wtedy zostaje tylko odnośnik do strony
//! wydania.
//!
//! Zapytania i pobieranie wykonuje `curl.exe` dołączony do Windows 10 i 11,
//! w osobnym wątku, żeby nie blokować okna.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// Repozytorium, w którym publikowane są wydania.
pub const REPO: &str = "Atimaniac/tune-center";

/// Instalator mniejszy od tego rozmiaru na pewno nie jest prawdziwy
/// (np. strona błędu zamiast pliku).
const MIN_INSTALLER_BYTES: u64 = 100 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Jeszcze nie sprawdzano.
    Idle,
    Checking,
    UpToDate,
    Available {
        version: String,
        /// Strona wydania.
        url: String,
        /// Zaufany adres instalatora; `None`, gdy wydanie go nie ma.
        installer: Option<String>,
    },
    /// Trwa pobieranie instalatora.
    Downloading {
        version: String,
    },
    /// Instalator wystartował; program powinien się zamknąć.
    Launched,
    /// Pobranie albo uruchomienie instalatora nie powiodło się.
    InstallFailed {
        url: String,
        error: String,
    },
    Failed(String),
}

/// Najważniejsze dane wydania z odpowiedzi GitHuba.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub url: String,
    pub installer: Option<String>,
}

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
        self.state.lock().unwrap().clone()
    }

    /// Uruchamia sprawdzanie w tle. `wake` jest wołane po zakończeniu,
    /// żeby okno odświeżyło wynik.
    pub fn start(&self, wake: impl FnOnce() + Send + 'static) {
        {
            let mut state = self.state.lock().unwrap();
            if matches!(
                *state,
                State::Checking | State::Downloading { .. } | State::Launched
            ) {
                return;
            }
            *state = State::Checking;
        }
        let shared = self.state.clone();
        std::thread::spawn(move || {
            let result = match fetch_latest() {
                Ok(release) => {
                    if is_newer(&release.tag, env!("CARGO_PKG_VERSION")) {
                        State::Available {
                            version: release.tag.trim_start_matches(['v', 'V']).to_owned(),
                            url: release.url,
                            installer: release.installer,
                        }
                    } else {
                        State::UpToDate
                    }
                }
                Err(e) => State::Failed(e),
            };
            *shared.lock().unwrap() = result;
            wake();
        });
    }

    /// Pobiera instalator z bieżącego stanu `Available` i uruchamia go
    /// w trybie cichym. Po sukcesie stan zmienia się na `Launched`.
    /// `lang` to kod języka instalatora ("pl" / "en").
    pub fn install(&self, lang: &'static str, wake: impl FnOnce() + Send + 'static) {
        let (version, url, installer) = {
            let mut state = self.state.lock().unwrap();
            let State::Available {
                version,
                url,
                installer: Some(installer),
            } = state.clone()
            else {
                return;
            };
            // Drugie sprawdzenie na wypadek zmiany kodu wyżej: nigdy nie
            // pobieramy pliku spoza wydań tego repozytorium.
            if !trusted_installer(&installer) {
                return;
            }
            *state = State::Downloading {
                version: version.clone(),
            };
            (version, url, installer)
        };
        let shared = self.state.clone();
        std::thread::spawn(move || {
            let path = std::env::temp_dir().join("TuneCenter-Setup-update.exe");
            let result = download(&installer, &path).and_then(|()| launch(&path, lang));
            *shared.lock().unwrap() = match result {
                Ok(()) => State::Launched,
                Err(error) => State::InstallFailed {
                    url,
                    error: crate::trf!(
                        "nie udało się zainstalować {version}: {error}",
                        "could not install {version}: {error}"
                    ),
                },
            };
            wake();
        });
    }
}

/// Zamienia "v0.12.1" albo "0.12.1-beta" na [0, 12, 1].
pub fn parse_version(text: &str) -> Option<Vec<u32>> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let core = text.split(['-', '+', ' ']).next()?;
    let parts: Option<Vec<u32>> = core.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

/// Czy `latest` jest wersją nowszą niż `current`. Nieczytelny znacznik
/// nie jest traktowany jako aktualizacja.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (parse_version(latest), parse_version(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

/// Czy adres wskazuje instalator z wydań naszego repozytorium:
/// `https://github.com/<REPO>/releases/download/<znacznik>/<nazwa>Setup.exe`.
pub fn trusted_installer(url: &str) -> bool {
    let prefix = format!("https://github.com/{REPO}/releases/download/").to_ascii_lowercase();
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix(&prefix) else {
        return false;
    };
    let mut parts = rest.split('/');
    let (Some(tag), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let clean = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
    };
    clean(tag) && clean(name) && name.ends_with("setup.exe")
}

/// Wyciąga znacznik, adres strony i zaufany adres instalatora z odpowiedzi GitHuba.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|_| {
        crate::i18n::tr(
            "nieczytelna odpowiedź serwera",
            "unreadable server response",
        )
        .to_owned()
    })?;
    match value.get("tag_name").and_then(|t| t.as_str()) {
        Some(tag) => {
            let url = value
                .get("html_url")
                .and_then(|u| u.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/latest"));
            let installer = value
                .get("assets")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .filter(|a| {
                    a.get("name")
                        .and_then(|n| n.as_str())
                        .is_some_and(|n| n.to_ascii_lowercase().ends_with("setup.exe"))
                })
                .filter_map(|a| a.get("browser_download_url").and_then(|u| u.as_str()))
                .find(|u| trusted_installer(u))
                .map(str::to_owned);
            Ok(Release {
                tag: tag.to_owned(),
                url,
                installer,
            })
        }
        None => {
            let message = value.get("message").and_then(|m| m.as_str()).unwrap_or("");
            if message.eq_ignore_ascii_case("Not Found") {
                Err(crate::i18n::tr(
                    "w repozytorium nie ma jeszcze opublikowanego wydania",
                    "the repository has no published release yet",
                )
                .to_owned())
            } else if message.is_empty() {
                Err(crate::i18n::tr(
                    "nieczytelna odpowiedź serwera",
                    "unreadable server response",
                )
                .to_owned())
            } else {
                Err(message.to_owned())
            }
        }
    }
}

/// `curl.exe` bez okna konsoli.
fn curl() -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn curl_failed(e: std::io::Error) -> String {
    crate::trf!(
        "nie udało się uruchomić curl ({e})",
        "could not run curl ({e})"
    )
}

fn fetch_latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let output = curl()
        .args([
            "-sS",
            "-L",
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: TuneCenter",
            &url,
        ])
        .output()
        .map_err(curl_failed)?;
    if !output.status.success() {
        return Err(
            crate::i18n::tr("brak połączenia z GitHubem", "no connection to GitHub").to_owned(),
        );
    }
    parse_release(&String::from_utf8_lossy(&output.stdout))
}

/// Pobiera instalator do `path` i sprawdza, czy to plik wykonywalny Windows.
fn download(url: &str, path: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let status = curl()
        .args([
            "-sS",
            "-L",
            "--fail",
            // Także przekierowania (GitHub odsyła do swojego serwera plików)
            // muszą być po HTTPS.
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "600",
            "-H",
            "User-Agent: TuneCenter",
            "-o",
        ])
        .arg(path)
        .arg(url)
        .status()
        .map_err(curl_failed)?;
    if !status.success() {
        let _ = std::fs::remove_file(path);
        return Err(crate::i18n::tr(
            "pobieranie instalatora nie powiodło się",
            "downloading the installer failed",
        )
        .to_owned());
    }
    if let Err(e) = check_executable(path) {
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

/// Odrzuca pliki zbyt małe albo bez nagłówka "MZ" pliku .exe.
fn check_executable(path: &Path) -> Result<(), String> {
    use std::io::Read;
    let bad = || {
        crate::i18n::tr(
            "pobrany plik nie jest instalatorem",
            "the downloaded file is not an installer",
        )
        .to_owned()
    };
    let len = std::fs::metadata(path).map_err(|_| bad())?.len();
    let mut magic = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map_err(|_| bad())?;
    if len < MIN_INSTALLER_BYTES || &magic != b"MZ" {
        return Err(bad());
    }
    Ok(())
}

/// Argumenty cichej aktualizacji dla instalatora Inno Setup.
pub fn installer_args(lang: &str) -> Vec<String> {
    let lang = if lang == "en" { "en" } else { "pl" };
    vec![
        "/SILENT".to_owned(),
        "/SUPPRESSMSGBOXES".to_owned(),
        "/NORESTART".to_owned(),
        format!("/LANG={lang}"),
    ]
}

/// Uruchamia instalator i nie czeka na niego - program zaraz się zamyka,
/// a instalator po aktualizacji uruchamia go ponownie.
fn launch(path: &Path, lang: &str) -> Result<(), String> {
    std::process::Command::new(path)
        .args(installer_args(lang))
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            crate::trf!(
                "nie udało się uruchomić instalatora ({e})",
                "could not start the installer ({e})"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("v0.12.1"), Some(vec![0, 12, 1]));
        assert_eq!(parse_version("1.2"), Some(vec![1, 2]));
        assert_eq!(parse_version("0.13.0-beta"), Some(vec![0, 13, 0]));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.13.0", "0.12.0"));
        assert!(is_newer("0.12.1", "0.12.0"));
        assert!(is_newer("1.0", "0.99.9"));
        assert!(is_newer("0.12.0.1", "0.12.0"));
        assert!(!is_newer("0.12.0", "0.12.0"));
        assert!(!is_newer("v0.12", "0.12.0"));
        assert!(!is_newer("0.9.9", "0.12.0"));
        assert!(!is_newer("latest", "0.12.0"));
    }

    #[test]
    fn reads_release_json() {
        let json = r#"{"tag_name":"v0.13.0","html_url":"https://example.com/r"}"#;
        assert_eq!(
            parse_release(json),
            Ok(Release {
                tag: "v0.13.0".to_owned(),
                url: "https://example.com/r".to_owned(),
                installer: None,
            })
        );
        assert!(parse_release(r#"{"message":"Not Found"}"#).is_err());
        assert_eq!(
            parse_release(r#"{"message":"API rate limit exceeded"}"#),
            Err("API rate limit exceeded".to_owned())
        );
        assert!(parse_release("<html>").is_err());
    }

    #[test]
    fn picks_trusted_installer_asset() {
        let json = r#"{"tag_name":"v0.17.1","html_url":"https://example.com/r","assets":[
            {"name":"notes.txt","browser_download_url":"https://github.com/Atimaniac/tune-center/releases/download/v0.17.1/notes.txt"},
            {"name":"TuneCenter-Setup.exe","browser_download_url":"https://evil.example.com/TuneCenter-Setup.exe"},
            {"name":"TuneCenter-Setup.exe","browser_download_url":"https://github.com/Atimaniac/tune-center/releases/download/v0.17.1/TuneCenter-Setup.exe"}
        ]}"#;
        assert_eq!(
            parse_release(json).unwrap().installer.as_deref(),
            Some(
                "https://github.com/Atimaniac/tune-center/releases/download/v0.17.1/TuneCenter-Setup.exe"
            )
        );

        let only_untrusted = r#"{"tag_name":"v0.17.1","assets":[
            {"name":"TuneCenter-Setup.exe","browser_download_url":"https://evil.example.com/TuneCenter-Setup.exe"}
        ]}"#;
        assert_eq!(parse_release(only_untrusted).unwrap().installer, None);
    }

    #[test]
    fn trusts_only_our_release_downloads() {
        let ok = "https://github.com/Atimaniac/tune-center/releases/download/v0.17.0/TuneCenter-Setup.exe";
        assert!(trusted_installer(ok));
        assert!(trusted_installer(&ok.to_ascii_lowercase()));

        for bad in [
            "http://github.com/Atimaniac/tune-center/releases/download/v0.17.0/TuneCenter-Setup.exe",
            "https://github.com/Other/tune-center/releases/download/v0.17.0/TuneCenter-Setup.exe",
            "https://github.com/Atimaniac/tune-center-fork/releases/download/v0.17.0/TuneCenter-Setup.exe",
            "https://github.com.evil.com/Atimaniac/tune-center/releases/download/v0.17.0/TuneCenter-Setup.exe",
            "https://github.com/Atimaniac/tune-center/releases/download/v0.17.0/TuneCenter-Setup.zip",
            "https://github.com/Atimaniac/tune-center/releases/download/../x/TuneCenter-Setup.exe",
            "https://github.com/Atimaniac/tune-center/releases/download/v0.17.0/a/TuneCenter-Setup.exe",
            "https://github.com/Atimaniac/tune-center/releases/download/v0.17.0/TuneCenter-Setup.exe?x=1",
            "https://github.com/Atimaniac/tune-center/releases/download//TuneCenter-Setup.exe",
            "",
        ] {
            assert!(!trusted_installer(bad), "{bad}");
        }
    }

    #[test]
    fn builds_silent_installer_arguments() {
        assert_eq!(
            installer_args("en"),
            ["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/LANG=en"]
        );
        assert_eq!(installer_args("pl").last().unwrap(), "/LANG=pl");
        assert_eq!(installer_args("xx").last().unwrap(), "/LANG=pl");
    }
}
