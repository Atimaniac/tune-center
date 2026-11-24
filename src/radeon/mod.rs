//! Zakładka RADEON (karta AMD): odpowiedniki ustawień z AMD Software:
//! Adrenalin Edition, wyłącznie przez publiczne API sterownika AMD (ADLX):
//!
//! - FSR: FidelityFX Super Resolution 4 (podmiana FSR 3.1 w grach), podmiana
//!   generowania klatek FSR, Radeon Super Resolution, AMD Fluid Motion Frames,
//! - FREESYNC: AMD FreeSync, dokładność kolorów FreeSync Premium Pro i
//!   dynamiczna kontrola odświeżania - dla każdego ekranu,
//! - Ustawienia 3D: zakładka "Grafika" Adrenalina (Anti-Lag, Chill, Boost,
//!   wyostrzanie, Enhanced Sync, synchronizacja pionowa, limit klatek,
//!   wygładzanie, filtrowanie, teselacja, czyszczenie pamięci shaderów).
//!
//! ADLX zmienia ustawienia globalne (dla wszystkich gier), tak jak domyślny
//! profil w Adrenalinie. Zmiana działa od razu i zostaje po restarcie.

pub mod adlx;

use std::collections::HashMap;

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::theme::{self, accent, muted, warn, SwitchState};
use adlx::{describe, s3d, Adlx, IntRange, Obj};

/// Skąd wziąć interfejs ustawienia.
#[derive(Clone, Copy)]
enum Src {
    /// Usługa ustawień 3D: pozycja metody, czy bierze kartę, wymagana wersja usługi.
    S3d { slot: usize, gpu: bool, level: u8 },
    /// Usługa ekranów: pozycja metody (argument: ekran), wymagana wersja usługi.
    Display { slot: usize, level: u8 },
}

/// Element sterujący ustawienia (pozycje metod w interfejsie ustawienia).
enum Ctl {
    /// Włącz / wyłącz: `IsEnabled`, `SetEnabled`.
    Switch { get: usize, set: usize },
    /// Liczba w zakresie podanym przez sterownik.
    Int {
        pl: &'static str,
        en: &'static str,
        get: usize,
        set: usize,
        range: usize,
        unit: &'static str,
    },
    /// Wybór z listy wartości wyliczenia ADLX. `ext` - metoda nowszego
    /// interfejsu (patrz `Feature::ext`).
    Choice {
        pl: &'static str,
        en: &'static str,
        get: usize,
        set: usize,
        opts: &'static [(i32, &'static str, &'static str)],
        ext: bool,
    },
    /// Jednorazowe polecenie z potwierdzeniem.
    Action {
        pl: &'static str,
        en: &'static str,
        slot: usize,
    },
}

struct Feature {
    key: &'static str,
    name_pl: &'static str,
    name_en: &'static str,
    help_pl: &'static str,
    help_en: &'static str,
    src: Src,
    /// Nowszy interfejs tego samego ustawienia (QueryInterface) z dodatkowymi
    /// metodami, np. poziom Anti-Lag.
    ext: Option<&'static str>,
    /// Pozycja `IsSupported`.
    supported: usize,
    ctls: &'static [Ctl],
}

// ------------------------------- FSR -----------------------------------------

const FSR: &[Feature] = &[
    Feature {
        key: "fsr4",
        name_pl: "AMD FidelityFX Super Resolution 4",
        name_en: "AMD FidelityFX Super Resolution 4",
        help_pl: "Podmienia w grach FSR 3.1 na FSR 4 (skalowanie oparte na uczeniu maszynowym, karty Radeon RX 9000) - odpowiednik podmiany DLSS. Działa w grach, które mają FSR 3.1, od następnego uruchomienia gry.",
        help_en: "Upgrades FSR 3.1 in games to FSR 4 (machine-learning upscaling, Radeon RX 9000 cards) - the counterpart of a DLSS override. Works in games with FSR 3.1, from the next time the game starts.",
        src: Src::S3d { slot: s3d::FIDELITYFX_SUPER_RESOLUTION, gpu: true, level: 3 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Switch { get: 4, set: 5 }],
    },
    Feature {
        key: "fsr_fg",
        name_pl: "Generowanie klatek FSR (podmiana)",
        name_en: "FSR Frame Generation (upgrade)",
        help_pl: "Podmienia w grach generowanie klatek FSR 3.1 na nowszą wersję ze sterownika. Działa od następnego uruchomienia gry.",
        help_en: "Upgrades FSR 3.1 frame generation in games to the newer version from the driver. Takes effect the next time the game starts.",
        src: Src::S3d { slot: s3d::FIDELITYFX_FRAME_GEN_UPGRADE, gpu: true, level: 3 },
        ext: None,
        supported: 5,
        ctls: &[Ctl::Switch { get: 6, set: 7 }],
    },
    Feature {
        key: "rsr",
        name_pl: "Radeon Super Resolution (RSR)",
        name_en: "Radeon Super Resolution (RSR)",
        help_pl: "Skalowanie FSR 1 przez sterownik w każdej grze uruchomionej na pełnym ekranie w niższej rozdzielczości niż ekran. Nie łączy się z FSR w grze.",
        help_en: "Driver FSR 1 upscaling in any game that runs full screen below the display resolution. Does not combine with in-game FSR.",
        src: Src::S3d { slot: s3d::RADEON_SUPER_RESOLUTION, gpu: false, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 5 },
            Ctl::Int { pl: "Wyostrzanie", en: "Sharpness", get: 7, set: 8, range: 6, unit: "%" },
        ],
    },
    Feature {
        key: "afmf",
        name_pl: "AMD Fluid Motion Frames (AFMF)",
        name_en: "AMD Fluid Motion Frames (AFMF)",
        help_pl: "Generowanie klatek przez sterownik w grach DirectX 11 i 12 (także bez FSR w grze). Wymaga, by gra działała na pełnym ekranie albo w oknie bez ramki.",
        help_en: "Driver frame generation in DirectX 11 and 12 games (also without in-game FSR). The game must run full screen or borderless.",
        src: Src::S3d { slot: s3d::AMD_FLUID_MOTION_FRAMES, gpu: false, level: 1 },
        ext: Some("IADLX3DAMDFluidMotionFrames1"),
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 5 },
            Ctl::Choice {
                pl: "Algorytm",
                en: "Algorithm",
                get: 6,
                set: 7,
                opts: &[(0, "Auto", "Auto"), (1, "Ulepszony", "Enhanced"), (2, "Standardowy", "Standard")],
                ext: true,
            },
            Ctl::Choice {
                pl: "Szukanie ruchu",
                en: "Search mode",
                get: 8,
                set: 9,
                opts: &[(0, "Auto", "Auto"), (1, "Standardowe", "Standard"), (2, "Wysokie", "High")],
                ext: true,
            },
            Ctl::Choice {
                pl: "Tryb",
                en: "Performance mode",
                get: 10,
                set: 11,
                opts: &[(0, "Auto", "Auto"), (1, "Jakość", "Quality"), (2, "Wydajność", "Performance")],
                ext: true,
            },
            Ctl::Choice {
                pl: "Przy szybkim ruchu",
                en: "Fast motion response",
                get: 12,
                set: 13,
                opts: &[(0, "Powtarzaj klatki", "Repeat frames"), (1, "Mieszaj klatki", "Blended frames")],
                ext: true,
            },
        ],
    },
];

// ----------------------------- FREESYNC --------------------------------------

const FREESYNC: &[Feature] = &[
    Feature {
        key: "freesync",
        name_pl: "AMD FreeSync",
        name_en: "AMD FreeSync",
        help_pl: "Zmienne odświeżanie ekranu dopasowane do liczby klatek gry - bez rwania obrazu i z mniejszym opóźnieniem niż synchronizacja pionowa.",
        help_en: "Variable refresh rate that follows the game's frame rate - no tearing and less latency than V-Sync.",
        src: Src::Display { slot: adlx::disp::FREESYNC, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Switch { get: 4, set: 5 }],
    },
    Feature {
        key: "freesync_color",
        name_pl: "Dokładność kolorów FreeSync Premium Pro",
        name_en: "FreeSync Premium Pro color accuracy",
        help_pl: "Na ekranach FreeSync Premium Pro z HDR: kolory i jasność według danych ekranu zamiast mapowania tonów.",
        help_en: "On FreeSync Premium Pro HDR displays: colors and brightness follow the display data instead of tone mapping.",
        src: Src::Display { slot: adlx::disp::FREESYNC_COLOR_ACCURACY, level: 3 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Switch { get: 4, set: 5 }],
    },
    Feature {
        key: "drrc",
        name_pl: "Dynamiczna kontrola odświeżania",
        name_en: "Dynamic Refresh Rate Control",
        help_pl: "Obniża odświeżanie ekranu na pulpicie, gdy obraz się nie zmienia (mniejsze zużycie energii, głównie laptopy).",
        help_en: "Lowers the refresh rate on the desktop when the image does not change (less power, mainly laptops).",
        src: Src::Display { slot: adlx::disp::DYNAMIC_REFRESH_RATE_CONTROL, level: 3 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Switch { get: 4, set: 5 }],
    },
];

// ---------------------------- Ustawienia 3D ----------------------------------

const SETTINGS_3D: &[Feature] = &[
    Feature {
        key: "antilag",
        name_pl: "Radeon Anti-Lag",
        name_en: "Radeon Anti-Lag",
        help_pl: "Zmniejsza opóźnienie między ruchem myszy a obrazem, gdy karta jest w pełni obciążona.",
        help_en: "Reduces input latency when the graphics card is fully loaded.",
        src: Src::S3d { slot: s3d::ANTI_LAG, gpu: true, level: 0 },
        ext: Some("IADLX3DAntiLag1"),
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 5 },
            Ctl::Choice {
                pl: "Rodzaj",
                en: "Type",
                get: 6,
                set: 7,
                opts: &[(0, "Anti-Lag", "Anti-Lag"), (1, "Anti-Lag 2", "Anti-Lag 2")],
                ext: true,
            },
        ],
    },
    Feature {
        key: "chill",
        name_pl: "Radeon Chill",
        name_en: "Radeon Chill",
        help_pl: "Dopasowuje liczbę klatek do ruchu w grze w podanym zakresie - mniej energii i ciepła, gdy na ekranie mało się dzieje.",
        help_en: "Adjusts the frame rate to in-game movement within the given range - less power and heat when little is happening.",
        src: Src::S3d { slot: s3d::CHILL, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 8 },
            Ctl::Int { pl: "Minimum", en: "Minimum", get: 6, set: 9, range: 5, unit: " FPS" },
            Ctl::Int { pl: "Maksimum", en: "Maximum", get: 7, set: 10, range: 5, unit: " FPS" },
        ],
    },
    Feature {
        key: "boost",
        name_pl: "Radeon Boost",
        name_en: "Radeon Boost",
        help_pl: "Obniża rozdzielczość w trakcie szybkiego ruchu kamery, żeby utrzymać więcej klatek (obsługiwane gry).",
        help_en: "Lowers the resolution during fast camera movement to keep more frames (supported games).",
        src: Src::S3d { slot: s3d::BOOST, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 7 },
            Ctl::Int { pl: "Minimalna rozdzielczość", en: "Minimum resolution", get: 6, set: 8, range: 5, unit: "%" },
        ],
    },
    Feature {
        key: "ris",
        name_pl: "Radeon Image Sharpening",
        name_en: "Radeon Image Sharpening",
        help_pl: "Wyostrza obraz gier (przydatne przy skalowaniu i wygładzaniu TAA).",
        help_en: "Sharpens game images (useful with upscaling and TAA).",
        src: Src::S3d { slot: s3d::IMAGE_SHARPENING, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 7 },
            Ctl::Int { pl: "Wyostrzanie", en: "Sharpness", get: 6, set: 8, range: 5, unit: "%" },
        ],
    },
    Feature {
        key: "ris_desktop",
        name_pl: "Wyostrzanie także na pulpicie",
        name_en: "Image Sharpening on the desktop",
        help_pl: "Radeon Image Sharpening działa także poza grami (pulpit, przeglądarka, wideo).",
        help_en: "Radeon Image Sharpening also applies outside games (desktop, browser, video).",
        src: Src::S3d { slot: s3d::IMAGE_SHARPEN_DESKTOP, gpu: true, level: 2 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Switch { get: 4, set: 5 }],
    },
    Feature {
        key: "enhanced_sync",
        name_pl: "Enhanced Sync",
        name_en: "Enhanced Sync",
        help_pl: "Bez rwania obrazu przy liczbie klatek powyżej odświeżania, z mniejszym opóźnieniem niż zwykła synchronizacja pionowa.",
        help_en: "No tearing above the refresh rate, with less latency than regular V-Sync.",
        src: Src::S3d { slot: s3d::ENHANCED_SYNC, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Switch { get: 4, set: 5 }],
    },
    Feature {
        key: "wfvr",
        name_pl: "Czekaj na odświeżenie pionowe",
        name_en: "Wait for Vertical Refresh",
        help_pl: "Synchronizacja pionowa (V-Sync) wymuszona przez sterownik albo zostawiona grze.",
        help_en: "Vertical sync (V-Sync) forced by the driver or left to the game.",
        src: Src::S3d { slot: s3d::WAIT_FOR_VERTICAL_REFRESH, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Choice {
            pl: "Tryb",
            en: "Mode",
            get: 5,
            set: 6,
            opts: &[
                (0, "Zawsze wyłączone", "Always off"),
                (1, "Wyłączone, chyba że gra chce", "Off, unless application specifies"),
                (2, "Włączone, chyba że gra nie chce", "On, unless application specifies"),
                (3, "Zawsze włączone", "Always on"),
            ],
            ext: false,
        }],
    },
    Feature {
        key: "frtc",
        name_pl: "Kontrola docelowej liczby klatek",
        name_en: "Frame Rate Target Control",
        help_pl: "Limit klatek na sekundę ustawiony w sterowniku (mniej ciepła i hałasu).",
        help_en: "A frames-per-second cap set in the driver (less heat and noise).",
        src: Src::S3d { slot: s3d::FRAME_RATE_TARGET_CONTROL, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 7 },
            Ctl::Int { pl: "Limit", en: "Target", get: 6, set: 8, range: 5, unit: " FPS" },
        ],
    },
    Feature {
        key: "aa",
        name_pl: "Wygładzanie krawędzi",
        name_en: "Anti-Aliasing",
        help_pl: "Wygładzanie wymuszone przez sterownik (głównie starsze gry DirectX 9-11). Poziom i metoda działają przy trybie innym niż \"Ustawienia gry\".",
        help_en: "Driver-forced anti-aliasing (mainly older DirectX 9-11 games). Level and method apply when the mode is not \"Use application settings\".",
        src: Src::S3d { slot: s3d::ANTI_ALIASING, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Choice {
                pl: "Tryb",
                en: "Mode",
                get: 4,
                set: 7,
                opts: &[
                    (0, "Ustawienia gry", "Use application settings"),
                    (1, "Wzmocnij ustawienia gry", "Enhance application settings"),
                    (2, "Zastąp ustawienia gry", "Override application settings"),
                ],
                ext: false,
            },
            Ctl::Choice {
                pl: "Poziom",
                en: "Level",
                get: 5,
                set: 8,
                opts: &[
                    (2, "2x", "2x"),
                    (3, "2x EQ", "2x EQ"),
                    (4, "4x", "4x"),
                    (5, "4x EQ", "4x EQ"),
                    (8, "8x", "8x"),
                    (9, "8x EQ", "8x EQ"),
                ],
                ext: false,
            },
            Ctl::Choice {
                pl: "Metoda",
                en: "Method",
                get: 6,
                set: 9,
                opts: &[
                    (0, "Multisampling", "Multisampling"),
                    (1, "Adaptacyjny multisampling", "Adaptive multisampling"),
                    (2, "Supersampling", "Supersampling"),
                ],
                ext: false,
            },
        ],
    },
    Feature {
        key: "mlaa",
        name_pl: "Morfologiczne wygładzanie (MLAA)",
        name_en: "Morphological Anti-Aliasing",
        help_pl: "Wygładzanie krawędzi jako filtr na gotowym obrazie - działa w każdej grze, lekko rozmywa obraz.",
        help_en: "Edge smoothing as a filter on the finished image - works in any game, slightly blurs the image.",
        src: Src::S3d { slot: s3d::MORPHOLOGICAL_ANTI_ALIASING, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Switch { get: 4, set: 5 }],
    },
    Feature {
        key: "af",
        name_pl: "Filtrowanie anizotropowe",
        name_en: "Anisotropic Filtering",
        help_pl: "Ostrzejsze tekstury oglądane pod kątem (podłoga, droga), wymuszone przez sterownik.",
        help_en: "Sharper textures seen at an angle (floors, roads), forced by the driver.",
        src: Src::S3d { slot: s3d::ANISOTROPIC_FILTERING, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Switch { get: 4, set: 6 },
            Ctl::Choice {
                pl: "Poziom",
                en: "Level",
                get: 5,
                set: 7,
                opts: &[(2, "2x", "2x"), (4, "4x", "4x"), (8, "8x", "8x"), (16, "16x", "16x")],
                ext: false,
            },
        ],
    },
    Feature {
        key: "tessellation",
        name_pl: "Teselacja",
        name_en: "Tessellation",
        help_pl: "Ograniczenie poziomu teselacji przez sterownik (mniej obciążenia w grach z przesadną teselacją).",
        help_en: "Driver limit on the tessellation level (less load in games with excessive tessellation).",
        src: Src::S3d { slot: s3d::TESSELLATION, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[
            Ctl::Choice {
                pl: "Tryb",
                en: "Mode",
                get: 4,
                set: 6,
                opts: &[
                    (0, "Optymalizacja AMD", "AMD optimized"),
                    (1, "Ustawienia gry", "Use application settings"),
                    (2, "Zastąp ustawienia gry", "Override application settings"),
                ],
                ext: false,
            },
            Ctl::Choice {
                pl: "Maksymalny poziom",
                en: "Maximum level",
                get: 5,
                set: 7,
                opts: &[
                    (1, "Wyłączona", "Off"),
                    (2, "2x", "2x"),
                    (4, "4x", "4x"),
                    (6, "6x", "6x"),
                    (8, "8x", "8x"),
                    (16, "16x", "16x"),
                    (32, "32x", "32x"),
                    (64, "64x", "64x"),
                ],
                ext: false,
            },
        ],
    },
    Feature {
        key: "reset_shader_cache",
        name_pl: "Pamięć podręczna shaderów",
        name_en: "Shader cache",
        help_pl: "Usuwa wszystkie skompilowane shadery sterownika AMD (wszystkie gry). Gry skompilują je ponownie - albo użyj zakładki Shader compilation.",
        help_en: "Deletes every compiled shader of the AMD driver (all games). Games will compile them again - or use the Shader compilation tab.",
        src: Src::S3d { slot: s3d::RESET_SHADER_CACHE, gpu: true, level: 0 },
        ext: None,
        supported: 3,
        ctls: &[Ctl::Action {
            pl: "Wyczyść pamięć podręczną shaderów",
            en: "Reset shader cache",
            slot: 4,
        }],
    },
];

/// Odczytane ustawienie: interfejs i bieżące wartości.
struct Loaded {
    obj: Option<Obj>,
    /// Interfejs ma metody nowszej wersji (`Feature::ext`).
    has_ext: bool,
    supported: bool,
    error: Option<String>,
    values: HashMap<usize, Result<i32, i32>>,
    ranges: HashMap<usize, IntRange>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Fsr,
    FreeSync,
    Settings3d,
}

/// Panele RADEON (FSR, FREESYNC, ustawienia 3D) i dane ADLX.
pub struct Radeon {
    loaded: HashMap<(&'static str, usize), Loaded>,
    displays: Option<Vec<(String, Obj)>>,
    confirm: Option<&'static str>,
    status: Option<(String, bool)>,
    /// Wspólna sesja ADLX programu (`Adlx::shared`).
    adlx: Option<std::sync::Arc<Adlx>>,
    /// Ostatni błąd połączenia z ADLX.
    adlx_error: Option<String>,
}

impl Default for Radeon {
    fn default() -> Self {
        Self::new()
    }
}

impl Radeon {
    pub fn new() -> Self {
        Self {
            loaded: HashMap::new(),
            displays: None,
            confirm: None,
            status: None,
            adlx: None,
            adlx_error: None,
        }
    }

    fn adlx(&mut self) -> Result<&Adlx, String> {
        if self.adlx.is_none() && self.adlx_error.is_none() {
            match Adlx::shared() {
                Ok(a) => self.adlx = Some(a),
                Err(e) => self.adlx_error = Some(e),
            }
        }
        match (&self.adlx, &self.adlx_error) {
            (Some(a), _) => Ok(a),
            (None, Some(e)) => Err(e.clone()),
            (None, None) => Err(String::new()),
        }
    }

    /// Wersja AMD Software (do śledzenia zmian sterownika w Shader compilation).
    pub fn driver_version(&mut self) -> Option<String> {
        self.adlx().ok()?.software_version.clone()
    }


    /// Odczytuje ustawienie (dla `display` - na danym ekranie).
    fn load(&mut self, f: &'static Feature, display: usize) -> &Loaded {
        let key = (f.key, display);
        if !self.loaded.contains_key(&key) {
            let loaded = self.read(f, display);
            self.loaded.insert(key, loaded);
        }
        &self.loaded[&key]
    }

    fn read(&mut self, f: &'static Feature, display: usize) -> Loaded {
        let mut out = Loaded {
            obj: None,
            has_ext: false,
            supported: false,
            error: None,
            values: HashMap::new(),
            ranges: HashMap::new(),
        };
        let obj = match self.open(f, display) {
            Ok(o) => o,
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        };
        let (obj, has_ext) = match f.ext.and_then(|iid| obj.query(iid)) {
            Some(newer) => (newer, true),
            None => (obj, false),
        };
        out.supported = obj.flag(f.supported).unwrap_or(false);
        if out.supported {
            for c in f.ctls {
                match c {
                    Ctl::Switch { get, .. } => {
                        out.values.insert(*get, obj.flag(*get).map(i32::from));
                    }
                    Ctl::Int { get, range, .. } => {
                        out.values.insert(*get, obj.int(*get));
                        if let Ok(r) = obj.range(*range) {
                            out.ranges.insert(*range, r);
                        }
                    }
                    Ctl::Choice { get, ext, .. } if !*ext || has_ext => {
                        out.values.insert(*get, obj.int(*get));
                    }
                    _ => {}
                }
            }
        }
        out.obj = Some(obj);
        out.has_ext = has_ext;
        out
    }

    /// Interfejs ustawienia z usługi ADLX.
    fn open(&mut self, f: &Feature, display: usize) -> Result<Obj, String> {
        let not_here = || tr("niedostępne w tym sterowniku", "not available in this driver").to_string();
        match f.src {
            Src::S3d { slot, gpu, level } => {
                let a = self.adlx()?;
                let s = a.services_3d.as_ref().ok_or_else(not_here)?;
                if a.level_3d < level {
                    return Err(not_here());
                }
                let r = if gpu { s.get_for(slot, &a.gpu) } else { s.get(slot) };
                r.map_err(describe)
            }
            Src::Display { slot, level } => {
                if self.displays.is_none() {
                    let list = self.adlx()?.displays();
                    self.displays = Some(list);
                }
                let a = self.adlx.as_ref().ok_or_else(not_here)?;
                let s = a.displays.as_ref().ok_or_else(not_here)?;
                if a.level_display < level {
                    return Err(not_here());
                }
                let d = self
                    .displays
                    .as_ref()
                    .and_then(|l| l.get(display))
                    .ok_or_else(not_here)?;
                s.get_for(slot, &d.1).map_err(describe)
            }
        }
    }

    /// Zapis przez interfejs ustawienia, potem ponowny odczyt.
    fn apply(&mut self, f: &'static Feature, display: usize, what: impl FnOnce(&Obj) -> Result<(), i32>) {
        let name = if crate::i18n::english() { f.name_en } else { f.name_pl };
        let result = match self.loaded.get(&(f.key, display)).and_then(|l| l.obj.as_ref()) {
            Some(obj) => what(obj),
            None => Err(adlx::ADLX_NOT_SUPPORTED),
        };
        self.status = Some(match result {
            Ok(()) => (crate::trf!("{name}: zmieniono.", "{name}: changed."), false),
            Err(e) => (format!("{name}: {}", describe(e)), true),
        });
        self.loaded.remove(&(f.key, display));
    }

    /// Lista ustawień jako karty z przełącznikami.
    fn features_ui(&mut self, ui: &mut egui::Ui, list: &'static [Feature], display: usize) {
        for f in list {
            self.feature_ui(ui, f, display);
            ui.add_space(8.0);
        }
    }

    fn feature_ui(&mut self, ui: &mut egui::Ui, f: &'static Feature, display: usize) {
        let english = crate::i18n::english();
        let (name, help) = if english { (f.name_en, f.help_en) } else { (f.name_pl, f.help_pl) };
        // Kopia odczytu do rysowania (zapisy idą po narysowaniu).
        let (supported, error, has_ext, values, ranges) = {
            let l = self.load(f, display);
            (l.supported, l.error.clone(), l.has_ext, l.values.clone(), l.ranges.clone())
        };
        let mut action: Option<Box<dyn FnOnce(&Obj) -> Result<(), i32>>> = None;
        let width = (ui.available_width() - 30.0).max(300.0);
        theme::card(10, 12).show(ui, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(name).strong().size(15.0).color(theme::text()));
                    ui.label(RichText::new(help).small().color(muted()));
                });
            });
            if !supported {
                let why = error.unwrap_or_else(|| {
                    tr("niedostępne na tej karcie", "not available on this card").to_string()
                });
                ui.label(RichText::new(why).color(muted()));
                return;
            }
            ui.add_space(4.0);
            let enabled = f.ctls.iter().find_map(|c| match c {
                Ctl::Switch { get, .. } => Some(values.get(get).and_then(|v| v.ok()) == Some(1)),
                _ => None,
            });
            for c in f.ctls {
                match c {
                    Ctl::Switch { get, set } => {
                        let on = values.get(get).and_then(|v| v.ok()) == Some(1);
                        ui.horizontal(|ui| {
                            let state = if on { SwitchState::On } else { SwitchState::Off };
                            if theme::switch(ui, &format!("radeon_{}_{display}", f.key), state, true).clicked() {
                                let (set, value) = (*set, !on);
                                action = Some(Box::new(move |o: &Obj| o.set_flag(set, value)));
                            }
                            ui.label(RichText::new(if on { tr("Włączone", "On") } else { tr("Wyłączone", "Off") }).color(if on { accent() } else { muted() }));
                        });
                    }
                    Ctl::Int { pl, en, get, set, range, unit } => {
                        let Some(Ok(current)) = values.get(get).copied() else { continue };
                        let r = ranges.get(range).copied().unwrap_or(IntRange { min: 0, max: 100, step: 1 });
                        let id = ui.scope_id().with(("radeon_int", f.key, *get, display));
                        let mut value = ui.data(|d| d.get_temp::<i32>(id)).unwrap_or(current);
                        ui.add_enabled_ui(enabled != Some(false), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(if english { *en } else { *pl });
                                let slider = ui.add(
                                    egui::Slider::new(&mut value, r.min..=r.max)
                                        .step_by(r.step.max(1) as f64)
                                        .suffix(*unit),
                                );
                                ui.data_mut(|d| d.insert_temp(id, value));
                                if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                                    if value != current {
                                        let set = *set;
                                        action = Some(Box::new(move |o: &Obj| o.set_int(set, value)));
                                    }
                                }
                            });
                        });
                    }
                    Ctl::Choice { pl, en, get, set, opts, ext } => {
                        if *ext && !has_ext {
                            continue;
                        }
                        let current = values.get(get).and_then(|v| v.ok());
                        let label_of = |v: i32| {
                            opts.iter()
                                .find(|o| o.0 == v)
                                .map(|o| if english { o.2 } else { o.1 }.to_owned())
                                .unwrap_or_else(|| format!("{v}"))
                        };
                        ui.add_enabled_ui(enabled != Some(false), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(if english { *en } else { *pl });
                                let shown = current.map(label_of).unwrap_or_else(|| "?".into());
                                egui::ComboBox::from_id_salt(("radeon_choice", f.key, *get, display))
                                    .selected_text(shown)
                                    .show_ui(ui, |ui| {
                                        for (v, opl, oen) in opts.iter() {
                                            let text = if english { *oen } else { *opl };
                                            if ui.selectable_label(current == Some(*v), text).clicked() && current != Some(*v) {
                                                let (set, v) = (*set, *v);
                                                action = Some(Box::new(move |o: &Obj| o.set_int(set, v)));
                                            }
                                        }
                                    });
                            });
                        });
                    }
                    Ctl::Action { pl, en, .. } => {
                        if ui.button(RichText::new(if english { *en } else { *pl }).color(warn())).clicked() {
                            self.confirm = Some(f.key);
                        }
                    }
                }
            }
        });
        if let Some(a) = action {
            self.apply(f, display, a);
        }
        // Potwierdzenie polecenia (czyszczenie pamięci shaderów).
        if self.confirm == Some(f.key) {
            let mut answer = None;
            egui::Modal::new(ui.scope_id().with(("radeon_confirm", f.key))).show(ui.ctx(), |ui| {
                ui.set_width(420.0);
                ui.label(RichText::new(tr("Czy na pewno?", "Are you sure?")).strong().size(16.0));
                ui.add_space(6.0);
                ui.label(help);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new(tr("Tak", "Yes")).color(warn())).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(tr("Anuluj", "Cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
            if let Some(yes) = answer {
                self.confirm = None;
                if yes {
                    if let Some(Ctl::Action { slot, .. }) = f.ctls.iter().find(|c| matches!(c, Ctl::Action { .. })) {
                        let slot = *slot;
                        self.apply(f, display, move |o| o.call(slot));
                    }
                }
            }
        }
    }

    /// Nagłówek z kartą i przyciskiem ponownego odczytu; `false`, gdy ADLX
    /// jest niedostępny.
    fn header(&mut self, ui: &mut egui::Ui, intro_pl: &'static str, intro_en: &'static str) -> bool {
        match self.adlx() {
            Ok(a) => {
                let (name, version) = (a.name.clone(), a.software_version.clone());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(name).strong().color(accent()));
                    if let Some(v) = version {
                        ui.label(RichText::new(format!("AMD Software {v}")).color(muted()));
                    }
                    if ui.button(tr("Odczytaj ponownie", "Read again")).clicked() {
                        self.loaded.clear();
                        self.displays = None;
                        self.status = None;
                    }
                });
                ui.label(RichText::new(tr(intro_pl, intro_en)).small().color(muted()));
                ui.add_space(8.0);
                true
            }
            Err(e) => {
                ui.label(
                    RichText::new(tr(
                        "Panel Radeon niedostępny (potrzebny sterownik AMD Radeon z ADLX; Overclock działa dalej).",
                        "Radeon panel unavailable (needs an AMD Radeon driver with ADLX; Overclock still works).",
                    ))
                    .strong(),
                );
                ui.label(RichText::new(e).color(warn()));
                if ui.button(tr("Spróbuj ponownie", "Try again")).clicked() {
                    self.adlx_error = None;
                }
                false
            }
        }
    }

    fn status_ui(&self, ui: &mut egui::Ui) {
        if let Some((text, error)) = &self.status {
            ui.add_space(4.0);
            ui.label(RichText::new(text).color(if *error { warn() } else { accent() }));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, page: Page) {
        match page {
            Page::Fsr => {
                if self.header(
                    ui,
                    "Technologie AMD FidelityFX i skalowania ze sterownika - odpowiednik nadpisań DLSS. Ustawienia globalne (wszystkie gry), działają od następnego uruchomienia gry.",
                    "AMD FidelityFX and driver upscaling technologies - the counterpart of DLSS overrides. Global settings (every game), effective from the next game start.",
                ) {
                    self.features_ui(ui, FSR, 0);
                }
            }
            Page::Settings3d => {
                if self.header(
                    ui,
                    "Ustawienia z zakładki Grafika w AMD Software: Adrenalin Edition, globalne dla wszystkich gier.",
                    "Settings from the Graphics tab of AMD Software: Adrenalin Edition, global for every game.",
                ) {
                    self.features_ui(ui, SETTINGS_3D, 0);
                }
            }
            Page::FreeSync => {
                if self.header(
                    ui,
                    "AMD FreeSync i ustawienia odświeżania dla każdego ekranu podłączonego do karty AMD.",
                    "AMD FreeSync and refresh settings for each display connected to the AMD card.",
                ) {
                    if self.displays.is_none() {
                        let list = self.adlx().map(|a| a.displays()).unwrap_or_default();
                        self.displays = Some(list);
                    }
                    let names: Vec<String> = self
                        .displays
                        .as_ref()
                        .map(|l| l.iter().map(|(n, _)| n.clone()).collect())
                        .unwrap_or_default();
                    if names.is_empty() {
                        ui.label(RichText::new(tr(
                            "Nie znaleziono ekranu podłączonego do karty AMD.",
                            "No display connected to the AMD card was found.",
                        ))
                        .color(muted()));
                    }
                    for (i, name) in names.iter().enumerate() {
                        theme::heading(ui, crate::icons::Icon::Gpu, &format!("{} {}", tr("Ekran", "Display"), i + 1));
                        ui.label(RichText::new(name).color(muted()));
                        ui.add_space(4.0);
                        self.features_ui(ui, FREESYNC, i);
                        ui.add_space(6.0);
                    }
                }
            }
        }
        self.status_ui(ui);
    }
}
