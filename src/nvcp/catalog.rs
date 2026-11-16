//! Ustawienia 3D pokazywane tak jak w panelu sterowania Nvidia.
//!
//! Identyfikatory i wartości pochodzą z oficjalnego `NvApiDriverSettings.h`
//! (NVAPI SDK). Jedna opcja z panelu bywa kilkoma ustawieniami sterownika
//! naraz (np. V-Sync "Adaptacyjna" = tryb V-Sync + kontrola rozrywania), więc
//! każda opcja to lista par (ustawienie, wartość). Bieżąca opcja to pierwsza,
//! której wszystkie pary zgadzają się z odczytem.

// ----------------------- identyfikatory ustawień ---------------------------

pub const VSYNCMODE: u32 = 0x00A8_79CF;
pub const VSYNCTEARCONTROL: u32 = 0x005A_375C;
pub const PRERENDERLIMIT: u32 = 0x007B_A09E;
pub const FRL_FPS: u32 = 0x1083_5002;
pub const VRR_APP_OVERRIDE: u32 = 0x10A8_79CF;
pub const VRR_MODE: u32 = 0x1194_F158;
pub const VRROVERLAYINDICATOR: u32 = 0x1095_F16F;
pub const REFRESH_RATE_OVERRIDE: u32 = 0x0064_B541;
pub const OGL_TRIPLE_BUFFER: u32 = 0x20FD_D1F9;
pub const OGL_CPL_PREFER_DXPRESENT: u32 = 0x20D6_90F8;
pub const OGL_CPL_GDI_COMPATIBILITY: u32 = 0x2072_C5A3;
pub const OGL_THREAD_CONTROL: u32 = 0x20C1_221E;
pub const PREFERRED_PSTATE: u32 = 0x1057_EB71;
pub const QUALITY_ENHANCEMENTS: u32 = 0x00CE_2691;
pub const PS_TEXFILTER_ANISO_OPTS2: u32 = 0x00E7_3211;
pub const PS_TEXFILTER_NO_NEG_LODBIAS: u32 = 0x0019_BB68;
pub const ANISO_MODE_SELECTOR: u32 = 0x10D2_BB16;
pub const ANISO_MODE_LEVEL: u32 = 0x101E_61A9;
pub const FXAA_ENABLE: u32 = 0x1074_C972;
pub const AA_MODE_SELECTOR: u32 = 0x107E_FC5B;
pub const AA_MODE_GAMMACORRECTION: u32 = 0x107D_639D;
pub const AO_MODE: u32 = 0x0066_7329;
pub const NGX_DLSS_SR_OVERRIDE: u32 = 0x10E4_1E01;
pub const NGX_DLSS_SR_PRESET: u32 = 0x10E4_1DF3;
pub const NGX_DLSS_RR_OVERRIDE: u32 = 0x10E4_1E02;
pub const NGX_DLSS_FG_OVERRIDE: u32 = 0x10E4_1E03;
pub const NGX_DLSSG_MULTI_FRAME_COUNT: u32 = 0x104D_6667;
pub const NGX_DLAA_OVERRIDE: u32 = 0x10E4_1DF4;
pub const VRPRERENDERLIMIT: u32 = 0x1011_1133;
/// Maksymalny rozmiar pamięci podręcznej shaderów na dysku, w MiB
/// (0 = wyłączona, 0xFFFFFFFF = bez limitu; domyślnie 0x4000).
pub const PS_SHADERDISKCACHE_MAX_SIZE: u32 = 0x00AC_8497;

// Wartości (nazwy jak w NvApiDriverSettings.h).
const VSYNC_PASSIVE: u32 = 0x6092_5292;
const VSYNC_FORCEOFF: u32 = 0x0841_6747;
const VSYNC_FORCEON: u32 = 0x4781_4940;
const VSYNC_FLIPINTERVAL2: u32 = 0x3261_0244;
const VSYNC_VIRTUAL: u32 = 0x1888_8888;
const TEAR_DISABLE: u32 = 0x9686_1077;
const TEAR_ENABLE: u32 = 0x9994_1284;
const QUALITY_HIGHQUALITY: u32 = 0xFFFF_FFF6;

/// Jedna opcja do wyboru.
pub struct Opt {
    pub pl: &'static str,
    pub en: &'static str,
    pub writes: &'static [(u32, u32)],
}

pub enum Kind {
    /// Lista opcji.
    Choice(&'static [Opt]),
    /// Liczba z zakresu; `zero` to opis wartości 0 (np. "wyłączone").
    Number {
        id: u32,
        min: u32,
        max: u32,
        zero_pl: &'static str,
        zero_en: &'static str,
        unit: &'static str,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Sync,
    Performance,
    Quality,
    Dlss,
    Vr,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Sync,
        Section::Performance,
        Section::Quality,
        Section::Dlss,
        Section::Vr,
    ];

    pub fn title(self) -> &'static str {
        use crate::i18n::tr;
        match self {
            Section::Sync => tr("Synchronizacja i opóźnienia", "Sync and latency"),
            Section::Performance => tr("Wydajność", "Performance"),
            Section::Quality => tr("Jakość obrazu", "Image quality"),
            Section::Dlss => tr(
                "DLSS (nadpisanie przez sterownik)",
                "DLSS (driver override)",
            ),
            Section::Vr => tr("Rzeczywistość wirtualna", "Virtual reality"),
        }
    }
}

pub struct Item {
    pub section: Section,
    pub pl: &'static str,
    pub en: &'static str,
    pub help_pl: &'static str,
    pub help_en: &'static str,
    pub kind: Kind,
}

impl Item {
    pub fn name(&self) -> &'static str {
        crate::i18n::tr(self.pl, self.en)
    }

    pub fn help(&self) -> &'static str {
        crate::i18n::tr(self.help_pl, self.help_en)
    }

    /// Wszystkie ustawienia sterownika, które ta pozycja zmienia.
    pub fn ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = match &self.kind {
            Kind::Choice(opts) => opts
                .iter()
                .flat_map(|o| o.writes.iter().map(|w| w.0))
                .collect(),
            Kind::Number { id, .. } => vec![*id],
        };
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// Indeks bieżącej opcji: pierwsza, której wszystkie pary zgadzają się
/// z wartościami zwracanymi przez `value(id)`.
pub fn current_option(opts: &[Opt], value: impl Fn(u32) -> Option<u32>) -> Option<usize> {
    opts.iter()
        .position(|o| o.writes.iter().all(|&(id, v)| value(id) == Some(v)))
}

const fn o(pl: &'static str, en: &'static str, writes: &'static [(u32, u32)]) -> Opt {
    Opt { pl, en, writes }
}

pub static ITEMS: &[Item] = &[
    // ------------------------- synchronizacja --------------------------
    Item {
        section: Section::Sync,
        pl: "Synchronizacja pionowa (V-Sync)",
        en: "Vertical sync",
        help_pl: "Zgrywa klatki z odświeżaniem monitora. \"Szybka\" pozwala grze renderować bez limitu bez rozrywania obrazu.",
        help_en: "Syncs frames with the display refresh. \"Fast\" lets the game render uncapped without tearing.",
        kind: Kind::Choice(&[
            o("Ustawienie aplikacji", "Use the 3D application setting", &[(VSYNCMODE, VSYNC_PASSIVE)]),
            o("Wyłączona", "Off", &[(VSYNCMODE, VSYNC_FORCEOFF)]),
            o("Włączona", "On", &[(VSYNCMODE, VSYNC_FORCEON), (VSYNCTEARCONTROL, TEAR_DISABLE)]),
            o("Adaptacyjna", "Adaptive", &[(VSYNCMODE, VSYNC_FORCEON), (VSYNCTEARCONTROL, TEAR_ENABLE)]),
            o("Adaptacyjna (połowa odświeżania)", "Adaptive (half refresh rate)", &[(VSYNCMODE, VSYNC_FLIPINTERVAL2), (VSYNCTEARCONTROL, TEAR_ENABLE)]),
            o("Szybka", "Fast", &[(VSYNCMODE, VSYNC_VIRTUAL)]),
        ]),
    },
    Item {
        section: Section::Sync,
        pl: "Maks. klatek przygotowanych (niskie opóźnienia)",
        en: "Max pre-rendered frames (low latency)",
        help_pl: "Ile klatek procesor może przygotować z wyprzedzeniem. 1 = najniższe opóźnienie (jak tryb niskich opóźnień \"Wł.\").",
        help_en: "How many frames the CPU may prepare ahead. 1 = lowest latency (like low latency mode \"On\").",
        kind: Kind::Number { id: PRERENDERLIMIT, min: 0, max: 8, zero_pl: "ustawienie aplikacji", zero_en: "application setting", unit: "" },
    },
    Item {
        section: Section::Sync,
        pl: "Maks. liczba klatek na sekundę",
        en: "Max frame rate",
        help_pl: "Ogranicznik klatek w sterowniku. 0 = wyłączony.",
        help_en: "Driver frame rate limiter. 0 = off.",
        kind: Kind::Number { id: FRL_FPS, min: 0, max: 1023, zero_pl: "wyłączony", zero_en: "off", unit: "FPS" },
    },
    Item {
        section: Section::Sync,
        pl: "Technologia monitora",
        en: "Monitor technology",
        help_pl: "G-SYNC (zmienne odświeżanie) albo stałe odświeżanie dla aplikacji.",
        help_en: "G-SYNC (variable refresh) or fixed refresh for applications.",
        kind: Kind::Choice(&[
            o("G-SYNC / G-SYNC Compatible", "G-SYNC / G-SYNC Compatible", &[(VRR_APP_OVERRIDE, 0)]),
            o("Stałe odświeżanie", "Fixed refresh", &[(VRR_APP_OVERRIDE, 4)]),
            o("Wymuszone wyłączenie VRR", "VRR forced off", &[(VRR_APP_OVERRIDE, 1)]),
            o("Niedozwolone", "Disallowed", &[(VRR_APP_OVERRIDE, 2)]),
            o("ULMB", "ULMB", &[(VRR_APP_OVERRIDE, 3)]),
        ]),
    },
    Item {
        section: Section::Sync,
        pl: "Preferowana częstotliwość odświeżania",
        en: "Preferred refresh rate",
        help_pl: "Czy gry pełnoekranowe mają dostać najwyższe dostępne odświeżanie.",
        help_en: "Whether full-screen games get the highest available refresh rate.",
        kind: Kind::Choice(&[
            o("Ustawienie aplikacji", "Application-controlled", &[(REFRESH_RATE_OVERRIDE, 0)]),
            o("Najwyższa dostępna", "Highest available", &[(REFRESH_RATE_OVERRIDE, 1)]),
        ]),
    },
    Item {
        section: Section::Sync,
        pl: "Potrójne buforowanie (OpenGL)",
        en: "Triple buffering (OpenGL)",
        help_pl: "Działa tylko w OpenGL i tylko z włączoną synchronizacją pionową.",
        help_en: "OpenGL only, and only with vertical sync on.",
        kind: Kind::Choice(&[
            o("Wyłączone", "Off", &[(OGL_TRIPLE_BUFFER, 0)]),
            o("Włączone", "On", &[(OGL_TRIPLE_BUFFER, 1)]),
        ]),
    },
    Item {
        section: Section::Sync,
        pl: "Metoda prezentacji Vulkan/OpenGL",
        en: "Vulkan/OpenGL present method",
        help_pl: "\"Warstwa na DXGI\" pozwala na Auto HDR, nakładki i płynniejsze przełączanie okien.",
        help_en: "\"Layered on DXGI\" enables Auto HDR, overlays and smoother window switching.",
        kind: Kind::Choice(&[
            o("Automatyczna", "Auto", &[(OGL_CPL_PREFER_DXPRESENT, 2)]),
            o("Natywna", "Prefer native", &[(OGL_CPL_PREFER_DXPRESENT, 0)]),
            o("Warstwa na DXGI Swapchain", "Prefer layered on DXGI Swapchain", &[(OGL_CPL_PREFER_DXPRESENT, 1)]),
        ]),
    },
    // ---------------------------- wydajność ----------------------------
    Item {
        section: Section::Performance,
        pl: "Tryb zarządzania energią",
        en: "Power management mode",
        help_pl: "\"Maksymalna wydajność\" trzyma wysokie zegary karty także przy małym obciążeniu (większy pobór mocy).",
        help_en: "\"Prefer maximum performance\" keeps the GPU clocks high even under light load (more power).",
        kind: Kind::Choice(&[
            o("Optymalne zużycie energii", "Optimal power", &[(PREFERRED_PSTATE, 5)]),
            o("Normalny (adaptacyjny)", "Normal (adaptive)", &[(PREFERRED_PSTATE, 0)]),
            o("Maksymalna wydajność", "Prefer maximum performance", &[(PREFERRED_PSTATE, 1)]),
            o("Stała wydajność", "Prefer consistent performance", &[(PREFERRED_PSTATE, 3)]),
        ]),
    },
    Item {
        section: Section::Performance,
        pl: "Optymalizacja wątkowa (OpenGL)",
        en: "Threaded optimization (OpenGL)",
        help_pl: "Przenosi część pracy sterownika OpenGL na osobny wątek procesora.",
        help_en: "Moves part of the OpenGL driver work to a separate CPU thread.",
        kind: Kind::Choice(&[
            o("Automatycznie", "Auto", &[(OGL_THREAD_CONTROL, 0)]),
            o("Włączona", "On", &[(OGL_THREAD_CONTROL, 1)]),
            o("Wyłączona", "Off", &[(OGL_THREAD_CONTROL, 2)]),
        ]),
    },
    Item {
        section: Section::Performance,
        pl: "Zgodność OpenGL z GDI",
        en: "OpenGL GDI compatibility",
        help_pl: "Dla starszych programów OpenGL, które rysują też przez GDI.",
        help_en: "For older OpenGL programs that also draw through GDI.",
        kind: Kind::Choice(&[
            o("Automatycznie", "Auto", &[(OGL_CPL_GDI_COMPATIBILITY, 2)]),
            o("Preferuj wydajność", "Prefer performance", &[(OGL_CPL_GDI_COMPATIBILITY, 0)]),
            o("Preferuj zgodność", "Prefer compatibility", &[(OGL_CPL_GDI_COMPATIBILITY, 1)]),
        ]),
    },
    // ------------------------------ jakość -----------------------------
    Item {
        section: Section::Quality,
        pl: "Filtrowanie tekstur - jakość",
        en: "Texture filtering - Quality",
        help_pl: "Kompromis między jakością tekstur a wydajnością.",
        help_en: "Trade-off between texture quality and performance.",
        kind: Kind::Choice(&[
            o("Wysoka jakość", "High quality", &[(QUALITY_ENHANCEMENTS, QUALITY_HIGHQUALITY)]),
            o("Jakość", "Quality", &[(QUALITY_ENHANCEMENTS, 0)]),
            o("Wydajność", "Performance", &[(QUALITY_ENHANCEMENTS, 0x0A)]),
            o("Wysoka wydajność", "High performance", &[(QUALITY_ENHANCEMENTS, 0x14)]),
        ]),
    },
    Item {
        section: Section::Quality,
        pl: "Filtrowanie tekstur - optymalizacja próbek anizotropowych",
        en: "Texture filtering - Anisotropic sample optimization",
        help_pl: "Mniej próbek przy filtrowaniu anizotropowym - szybciej, nieco gorzej.",
        help_en: "Fewer samples for anisotropic filtering - faster, slightly worse.",
        kind: Kind::Choice(&[
            o("Wyłączona", "Off", &[(PS_TEXFILTER_ANISO_OPTS2, 0)]),
            o("Włączona", "On", &[(PS_TEXFILTER_ANISO_OPTS2, 1)]),
        ]),
    },
    Item {
        section: Section::Quality,
        pl: "Filtrowanie tekstur - ujemne przesunięcie LOD",
        en: "Texture filtering - Negative LOD bias",
        help_pl: "\"Ogranicz\" usuwa migotanie tekstur w grach z ujemnym LOD.",
        help_en: "\"Clamp\" removes texture shimmering in games with a negative LOD bias.",
        kind: Kind::Choice(&[
            o("Zezwalaj", "Allow", &[(PS_TEXFILTER_NO_NEG_LODBIAS, 0)]),
            o("Ogranicz", "Clamp", &[(PS_TEXFILTER_NO_NEG_LODBIAS, 1)]),
        ]),
    },
    Item {
        section: Section::Quality,
        pl: "Filtrowanie anizotropowe",
        en: "Anisotropic filtering",
        help_pl: "Ostrość tekstur oglądanych pod kątem.",
        help_en: "Sharpness of textures viewed at an angle.",
        kind: Kind::Choice(&[
            o("Ustawienie aplikacji", "Application-controlled", &[(ANISO_MODE_SELECTOR, 0)]),
            o("Wyłączone", "Off", &[(ANISO_MODE_SELECTOR, 1), (ANISO_MODE_LEVEL, 1)]),
            o("2x", "2x", &[(ANISO_MODE_SELECTOR, 1), (ANISO_MODE_LEVEL, 2)]),
            o("4x", "4x", &[(ANISO_MODE_SELECTOR, 1), (ANISO_MODE_LEVEL, 4)]),
            o("8x", "8x", &[(ANISO_MODE_SELECTOR, 1), (ANISO_MODE_LEVEL, 8)]),
            o("16x", "16x", &[(ANISO_MODE_SELECTOR, 1), (ANISO_MODE_LEVEL, 16)]),
        ]),
    },
    Item {
        section: Section::Quality,
        pl: "Antyaliasing - FXAA",
        en: "Antialiasing - FXAA",
        help_pl: "Szybkie wygładzanie krawędzi w sterowniku.",
        help_en: "Fast driver-side edge smoothing.",
        kind: Kind::Choice(&[
            o("Wyłączony", "Off", &[(FXAA_ENABLE, 0)]),
            o("Włączony", "On", &[(FXAA_ENABLE, 1)]),
        ]),
    },
    Item {
        section: Section::Quality,
        pl: "Antyaliasing - tryb",
        en: "Antialiasing - Mode",
        help_pl: "Czy sterownik ma nadpisywać lub wzmacniać antyaliasing gry.",
        help_en: "Whether the driver overrides or enhances the game's antialiasing.",
        kind: Kind::Choice(&[
            o("Ustawienie aplikacji", "Application-controlled", &[(AA_MODE_SELECTOR, 0)]),
            o("Nadpisz ustawienie aplikacji", "Override any application setting", &[(AA_MODE_SELECTOR, 1)]),
            o("Wzmocnij ustawienie aplikacji", "Enhance the application setting", &[(AA_MODE_SELECTOR, 2)]),
        ]),
    },
    Item {
        section: Section::Quality,
        pl: "Antyaliasing - korekcja gamma",
        en: "Antialiasing - Gamma correction",
        help_pl: "Korekcja gamma przy wygładzaniu krawędzi.",
        help_en: "Gamma correction when smoothing edges.",
        kind: Kind::Choice(&[
            o("Wyłączona", "Off", &[(AA_MODE_GAMMACORRECTION, 0)]),
            o("Włączona", "On", &[(AA_MODE_GAMMACORRECTION, 2)]),
        ]),
    },
    Item {
        section: Section::Quality,
        pl: "Okluzja otoczenia",
        en: "Ambient occlusion",
        help_pl: "Cieniowanie zagłębień dodawane przez sterownik (tylko obsługiwane gry).",
        help_en: "Driver-added contact shadows (supported games only).",
        kind: Kind::Choice(&[
            o("Wyłączona", "Off", &[(AO_MODE, 0)]),
            o("Niska", "Low", &[(AO_MODE, 1)]),
            o("Średnia", "Medium", &[(AO_MODE, 2)]),
            o("Wysoka", "High", &[(AO_MODE, 3)]),
        ]),
    },
    // ------------------------------- DLSS ------------------------------
    Item {
        section: Section::Dlss,
        pl: "DLSS Super Resolution - nadpisanie",
        en: "DLSS Super Resolution - override",
        help_pl: "Pozwala sterownikowi zmienić model DLSS w grach.",
        help_en: "Lets the driver change the DLSS model in games.",
        kind: Kind::Choice(&[
            o("Wyłączone", "Off", &[(NGX_DLSS_SR_OVERRIDE, 0)]),
            o("Włączone", "On", &[(NGX_DLSS_SR_OVERRIDE, 1)]),
        ]),
    },
    Item {
        section: Section::Dlss,
        pl: "DLSS Super Resolution - model (preset)",
        en: "DLSS Super Resolution - model (preset)",
        help_pl: "Działa przy włączonym nadpisaniu. \"Najnowszy\" = zawsze najnowszy model sterownika.",
        help_en: "Works with the override on. \"Latest\" = always the driver's newest model.",
        kind: Kind::Choice(&[
            o("Domyślny gry", "Game default", &[(NGX_DLSS_SR_PRESET, 0)]),
            o("Najnowszy", "Latest", &[(NGX_DLSS_SR_PRESET, 0x00FF_FFFF)]),
            o("Preset J", "Preset J", &[(NGX_DLSS_SR_PRESET, 10)]),
            o("Preset K", "Preset K", &[(NGX_DLSS_SR_PRESET, 11)]),
            o("Preset L", "Preset L", &[(NGX_DLSS_SR_PRESET, 12)]),
            o("Preset M", "Preset M", &[(NGX_DLSS_SR_PRESET, 13)]),
            o("Preset E", "Preset E", &[(NGX_DLSS_SR_PRESET, 5)]),
            o("Preset F", "Preset F", &[(NGX_DLSS_SR_PRESET, 6)]),
        ]),
    },
    Item {
        section: Section::Dlss,
        pl: "DLAA zamiast DLSS",
        en: "DLAA instead of DLSS",
        help_pl: "Renderowanie w natywnej rozdzielczości z antyaliasingiem DLSS.",
        help_en: "Native-resolution rendering with DLSS antialiasing.",
        kind: Kind::Choice(&[
            o("Domyślne", "Default", &[(NGX_DLAA_OVERRIDE, 0)]),
            o("Włączone", "On", &[(NGX_DLAA_OVERRIDE, 1)]),
        ]),
    },
    Item {
        section: Section::Dlss,
        pl: "DLSS Ray Reconstruction - nadpisanie",
        en: "DLSS Ray Reconstruction - override",
        help_pl: "Pozwala sterownikowi zmienić model Ray Reconstruction.",
        help_en: "Lets the driver change the Ray Reconstruction model.",
        kind: Kind::Choice(&[
            o("Wyłączone", "Off", &[(NGX_DLSS_RR_OVERRIDE, 0)]),
            o("Włączone", "On", &[(NGX_DLSS_RR_OVERRIDE, 1)]),
        ]),
    },
    Item {
        section: Section::Dlss,
        pl: "DLSS Frame Generation - nadpisanie",
        en: "DLSS Frame Generation - override",
        help_pl: "Pozwala sterownikowi zmienić generowanie klatek w grach.",
        help_en: "Lets the driver change frame generation in games.",
        kind: Kind::Choice(&[
            o("Wyłączone", "Off", &[(NGX_DLSS_FG_OVERRIDE, 0)]),
            o("Włączone", "On", &[(NGX_DLSS_FG_OVERRIDE, 1)]),
        ]),
    },
    Item {
        section: Section::Dlss,
        pl: "Generowanie wielu klatek - liczba klatek",
        en: "Multi frame generation - frame count",
        help_pl: "Ile klatek generować na każdą wyrenderowaną (karty RTX 50). 0 = wyłączone nadpisanie.",
        help_en: "Generated frames per rendered frame (RTX 50 cards). 0 = override off.",
        kind: Kind::Number { id: NGX_DLSSG_MULTI_FRAME_COUNT, min: 0, max: 15, zero_pl: "bez nadpisania", zero_en: "no override", unit: "" },
    },
    // -------------------------------- VR -------------------------------
    Item {
        section: Section::Vr,
        pl: "Klatki przygotowane z wyprzedzeniem (VR)",
        en: "Virtual reality pre-rendered frames",
        help_pl: "Jak wyżej, ale dla gogli VR. Mniej = mniejsze opóźnienie.",
        help_en: "Same as above, for VR headsets. Fewer = lower latency.",
        kind: Kind::Number { id: VRPRERENDERLIMIT, min: 0, max: 8, zero_pl: "ustawienie aplikacji", zero_en: "application setting", unit: "" },
    },
];

/// Pamięć podręczna shaderów (zakładka Shader compilation).
pub static SHADER_ITEMS: &[Item] = &[Item {
    section: Section::Performance,
    pl: "Rozmiar pamięci podręcznej shaderów",
    en: "Shader cache size",
    help_pl: "Ile miejsca na dysku sterownik może zająć na skompilowane shadery. Za mały limit wyrzuca skompilowane shadery i gry kompilują je ponownie.",
    help_en: "How much disk space the driver may use for compiled shaders. Too small a limit evicts compiled shaders and games compile them again.",
    kind: Kind::Choice(&[
        o("Domyślny sterownika", "Driver default", &[(PS_SHADERDISKCACHE_MAX_SIZE, 0x4000)]),
        o("5 GB", "5 GB", &[(PS_SHADERDISKCACHE_MAX_SIZE, 5 * 1024)]),
        o("10 GB", "10 GB", &[(PS_SHADERDISKCACHE_MAX_SIZE, 10 * 1024)]),
        o("100 GB", "100 GB", &[(PS_SHADERDISKCACHE_MAX_SIZE, 100 * 1024)]),
        o("Bez limitu", "Unlimited", &[(PS_SHADERDISKCACHE_MAX_SIZE, 0xFFFF_FFFF)]),
        o("Wyłączona", "Disabled", &[(PS_SHADERDISKCACHE_MAX_SIZE, 0)]),
    ]),
}];

/// Ustawienia G-SYNC profilu globalnego (zakładka G-SYNC).
pub static GSYNC_ITEMS: &[Item] = &[
    Item {
        section: Section::Sync,
        pl: "G-SYNC",
        en: "G-SYNC",
        help_pl: "Dla jakich okien sterownik włącza zmienne odświeżanie.",
        help_en: "Which windows the driver enables variable refresh for.",
        kind: Kind::Choice(&[
            o("Wyłączony", "Off", &[(VRR_MODE, 0)]),
            o("Tylko pełny ekran", "Full screen only", &[(VRR_MODE, 1)]),
            o(
                "Pełny ekran i okno",
                "Full screen and windowed",
                &[(VRR_MODE, 2)],
            ),
        ]),
    },
    Item {
        section: Section::Sync,
        pl: "Wskaźnik G-SYNC na ekranie",
        en: "G-SYNC indicator overlay",
        help_pl: "Napis w rogu ekranu, gdy gra działa ze zmiennym odświeżaniem.",
        help_en: "On-screen label when a game runs with variable refresh.",
        kind: Kind::Choice(&[
            o("Wyłączony", "Off", &[(VRROVERLAYINDICATOR, 0)]),
            o("Włączony", "On", &[(VRROVERLAYINDICATOR, 1)]),
        ]),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn every_item_has_options_and_texts() {
        for item in ITEMS.iter().chain(GSYNC_ITEMS).chain(SHADER_ITEMS) {
            assert!(!item.pl.is_empty() && !item.en.is_empty());
            assert!(!item.ids().is_empty(), "{}", item.pl);
            if let Kind::Choice(opts) = &item.kind {
                assert!(opts.len() >= 2, "{}", item.pl);
                for opt in opts.iter() {
                    assert!(!opt.writes.is_empty(), "{} / {}", item.pl, opt.pl);
                }
            }
            if let Kind::Number { min, max, .. } = &item.kind {
                assert!(min < max);
            }
        }
    }

    #[test]
    fn composite_vsync_options_are_distinguished() {
        let Kind::Choice(opts) = &ITEMS[0].kind else {
            panic!()
        };
        let read = |pairs: &[(u32, u32)]| {
            let map: HashMap<u32, u32> = pairs.iter().copied().collect();
            current_option(opts, |id| map.get(&id).copied())
        };
        assert_eq!(
            read(&[(VSYNCMODE, VSYNC_PASSIVE), (VSYNCTEARCONTROL, TEAR_DISABLE)]),
            Some(0)
        );
        assert_eq!(
            read(&[(VSYNCMODE, VSYNC_FORCEON), (VSYNCTEARCONTROL, TEAR_DISABLE)]),
            Some(2)
        );
        assert_eq!(
            read(&[(VSYNCMODE, VSYNC_FORCEON), (VSYNCTEARCONTROL, TEAR_ENABLE)]),
            Some(3)
        );
        assert_eq!(read(&[(VSYNCMODE, VSYNC_VIRTUAL)]), Some(5));
        // Nieznane połączenie nie udaje żadnej opcji.
        assert_eq!(read(&[(VSYNCMODE, 0x1234)]), None);
    }

    #[test]
    fn anisotropic_app_wins_over_level() {
        let aniso = ITEMS
            .iter()
            .find(|i| i.en == "Anisotropic filtering")
            .unwrap();
        let Kind::Choice(opts) = &aniso.kind else {
            panic!()
        };
        let map: HashMap<u32, u32> = [(ANISO_MODE_SELECTOR, 0), (ANISO_MODE_LEVEL, 16)].into();
        assert_eq!(current_option(opts, |id| map.get(&id).copied()), Some(0));
        let map: HashMap<u32, u32> = [(ANISO_MODE_SELECTOR, 1), (ANISO_MODE_LEVEL, 16)].into();
        assert_eq!(current_option(opts, |id| map.get(&id).copied()), Some(5));
    }

    #[test]
    fn ids_cover_all_written_settings() {
        let vsync = &ITEMS[0];
        assert_eq!(vsync.ids(), {
            let mut v = vec![VSYNCMODE, VSYNCTEARCONTROL];
            v.sort_unstable();
            v
        });
    }
}
