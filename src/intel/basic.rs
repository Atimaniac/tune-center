//! Podstawowy panel dla kart Intel, których nie obsługuje API Intela (IGCL):
//! grafiki sprzed 11. generacji procesorów (np. HD 530, UHD 620/630) działają
//! na starszym sterowniku Intela bez `ControlLib.dll`. Wszystko tu idzie przez
//! publiczne interfejsy Windows, które działają z każdą kartą:
//!
//! - karta: nazwa, wersja sterownika i zajęta pamięć (DXGI),
//! - ekran: rozdzielczość i odświeżanie (z potwierdzeniem i powrotem po 15 s),
//!   skalowanie obrazu, HDR (Windows Display Config),
//! - grafika Windows: HAGS, optymalizacje gier w oknie, zmienne odświeżanie
//!   (VRR) i wybór karty dla gry (te same wartości co Ustawienia Windows >
//!   Ekran > Grafika).

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::theme::{accent, muted, warn};

/// Klucz ustawień grafiki Windows (HKCU).
const GPU_PREFS: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
const GLOBAL: &str = "DirectXUserGlobalSettings";
const KEEP_SECONDS: u64 = 15;

// --------------------------- ustawienia DirectX ------------------------------

/// Wartość `klucz=wartość;` z tekstu ustawień DirectX (np.
/// "SwapEffectUpgradeEnable=1;VRROptimizeEnable=0;").
pub fn setting(text: &str, key: &str) -> Option<String> {
    text.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_owned())
    })
}

/// Tekst ustawień z `key` ustawionym na `value` (pozostałe bez zmian).
pub fn with_setting(text: &str, key: &str, value: &str) -> String {
    let mut out: Vec<String> = text
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter(|p| !p.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key)))
        .map(str::to_owned)
        .collect();
    out.push(format!("{key}={value}"));
    out.iter().map(|p| format!("{p};")).collect()
}

/// Tryb ekranu: szerokość, wysokość, odświeżanie (Hz).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

/// Tryby bez powtórzeń, od największej rozdzielczości i odświeżania.
pub fn sorted_modes(mut modes: Vec<Mode>) -> Vec<Mode> {
    modes.sort_by(|a, b| b.cmp(a));
    modes.dedup();
    modes
}

// ------------------------------- Windows -------------------------------------

/// Karta Intel widziana przez DXGI.
#[derive(Clone, Debug, Default)]
pub struct Adapter {
    pub name: String,
    pub driver: String,
    /// LUID karty (do dopasowania ekranów).
    pub luid: (u32, i32),
    /// Zajęta pamięć: (lokalna, współdzielona) w bajtach.
    pub used: (u64, u64),
}

/// Ekran podłączony do karty.
#[derive(Clone, Debug, Default)]
pub struct Display {
    pub name: String,
    /// Nazwa urządzenia GDI (np. \\.\DISPLAY1).
    pub gdi: String,
    pub adapter: (u32, i32),
    pub target: u32,
    pub current: Option<Mode>,
    pub modes: Vec<Mode>,
    /// DISPLAYCONFIG_SCALING (1 bez skalowania, 2 wyśrodkowany, 3 rozciągnięty,
    /// 4 z zachowaniem proporcji, 128 domyślne).
    pub scaling: i32,
    /// HDR: (obsługiwany, włączony).
    pub hdr: (bool, bool),
}

#[cfg(windows)]
mod sys {
    use super::{Adapter, Display, Mode};
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::Devices::Display::*;
    use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Graphics::Gdi::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn text(w: &[u16]) -> String {
        let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..end])
    }

    /// Karty Intel (DXGI, VendorId 0x8086).
    pub fn adapters() -> Vec<Adapter> {
        let mut out = Vec::new();
        // SAFETY: wywołania DXGI z poprawnymi argumentami; struktury lokalne.
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
            for i in 0.. {
                let Ok(adapter) = factory.EnumAdapters1(i) else { break };
                let Ok(desc) = adapter.GetDesc1() else { continue };
                if desc.VendorId != 0x8086 {
                    continue;
                }
                let driver = adapter
                    .CheckInterfaceSupport(&IDXGIDevice::IID)
                    .map(|v| {
                        let v = v as u64;
                        format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF, v & 0xFFFF)
                    })
                    .unwrap_or_default();
                let mut used = (0, 0);
                if let Ok(a3) = adapter.cast::<IDXGIAdapter3>() {
                    let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                    if a3.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info).is_ok() {
                        used.0 = info.CurrentUsage;
                    }
                    if a3.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut info).is_ok() {
                        used.1 = info.CurrentUsage;
                    }
                }
                out.push(Adapter {
                    name: text(&desc.Description),
                    driver,
                    luid: (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
                    used,
                });
            }
        }
        out
    }

    fn query(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), String> {
        // SAFETY: bufory o rozmiarach zwróconych przez GetDisplayConfigBufferSizes.
        unsafe {
            for _ in 0..4 {
                let (mut np, mut nm) = (0u32, 0u32);
                let r = GetDisplayConfigBufferSizes(flags, &mut np, &mut nm);
                if r != ERROR_SUCCESS {
                    return Err(format!("GetDisplayConfigBufferSizes: {}", r.0));
                }
                let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
                let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
                let r = QueryDisplayConfig(flags, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None);
                // 122 = ERROR_INSUFFICIENT_BUFFER: ekrany zmieniły się w międzyczasie.
                if r.0 == 122 {
                    continue;
                }
                if r != ERROR_SUCCESS {
                    return Err(format!("QueryDisplayConfig: {}", r.0));
                }
                paths.truncate(np as usize);
                modes.truncate(nm as usize);
                return Ok((paths, modes));
            }
            Err("QueryDisplayConfig: ekrany ciągle się zmieniają / displays keep changing".into())
        }
    }

    fn header(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, size: usize, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
        DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id }
    }

    fn modes_of(gdi: &[u16]) -> (Option<Mode>, Vec<Mode>) {
        let mode = |dm: &DEVMODEW| Mode { width: dm.dmPelsWidth, height: dm.dmPelsHeight, hz: dm.dmDisplayFrequency };
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        // SAFETY: DEVMODEW z ustawionym rozmiarem; nazwa zakończona zerem.
        let current = unsafe { EnumDisplaySettingsW(PCWSTR(gdi.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }
            .as_bool()
            .then(|| mode(&dm));
        let mut list = Vec::new();
        for i in 0..4096u32 {
            let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
            // SAFETY: jak wyżej.
            if !unsafe { EnumDisplaySettingsW(PCWSTR(gdi.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }.as_bool() {
                break;
            }
            // Tylko tryby 32-bitowe (pozostałe to te same rozdzielczości).
            if dm.dmBitsPerPel == 32 && dm.dmDisplayFrequency > 1 {
                list.push(mode(&dm));
            }
        }
        (current, super::sorted_modes(list))
    }

    /// Aktywne ekrany ze skalowaniem, HDR i trybami.
    pub fn displays() -> Result<Vec<Display>, String> {
        let (paths, _) = query(QDC_ONLY_ACTIVE_PATHS)?;
        let mut out = Vec::new();
        for p in &paths {
            let t = &p.targetInfo;
            let s = &p.sourceInfo;
            let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(), t.adapterId, t.id),
                ..Default::default()
            };
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), s.adapterId, s.id),
                ..Default::default()
            };
            let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(), t.adapterId, t.id),
                ..Default::default()
            };
            // SAFETY: struktury z nagłówkiem o właściwym typie i rozmiarze.
            let (name_ok, source_ok, color_ok) = unsafe {
                (
                    DisplayConfigGetDeviceInfo(&mut name.header) == 0,
                    DisplayConfigGetDeviceInfo(&mut source.header) == 0,
                    DisplayConfigGetDeviceInfo(&mut color.header) == 0,
                )
            };
            if !source_ok {
                continue;
            }
            // SAFETY: unia z polem bitowym odczytywana jako u32.
            let bits = if color_ok { unsafe { color.Anonymous.value } } else { 0 };
            let gdi = source.viewGdiDeviceName;
            let (current, modes) = modes_of(&gdi);
            let friendly = if name_ok { text(&name.monitorFriendlyDeviceName) } else { String::new() };
            out.push(Display {
                name: if friendly.is_empty() { text(&gdi) } else { friendly },
                gdi: text(&gdi),
                adapter: (t.adapterId.LowPart, t.adapterId.HighPart),
                target: t.id,
                current,
                modes,
                scaling: t.scaling.0,
                hdr: (bits & 1 != 0, bits & 2 != 0),
            });
        }
        Ok(out)
    }

    /// Ustawia tryb ekranu (najpierw próba, potem zapis w rejestrze).
    pub fn set_mode(gdi: &str, m: Mode) -> Result<(), String> {
        let name = wide(gdi);
        let dm = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            dmFields: DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
            dmPelsWidth: m.width,
            dmPelsHeight: m.height,
            dmDisplayFrequency: m.hz,
            ..Default::default()
        };
        // SAFETY: nazwa zakończona zerem, DEVMODEW żyje do końca wywołań.
        unsafe {
            let r = ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm), None, CDS_TEST, None);
            if r != DISP_CHANGE_SUCCESSFUL {
                return Err(format!("ChangeDisplaySettingsEx (test): {}", r.0));
            }
            let r = ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm), None, CDS_UPDATEREGISTRY, None);
            if r != DISP_CHANGE_SUCCESSFUL {
                return Err(format!("ChangeDisplaySettingsEx: {}", r.0));
            }
        }
        Ok(())
    }

    /// Skalowanie obrazu ekranu (adapter, cel).
    pub fn set_scaling(adapter: (u32, i32), target: u32, scaling: i32) -> Result<(), String> {
        let (mut paths, modes) = query(QDC_ONLY_ACTIVE_PATHS)?;
        let path = paths
            .iter_mut()
            .find(|p| p.targetInfo.adapterId.LowPart == adapter.0 && p.targetInfo.adapterId.HighPart == adapter.1 && p.targetInfo.id == target)
            .ok_or("nie ma już tego ekranu / that display is gone")?;
        path.targetInfo.scaling = DISPLAYCONFIG_SCALING(scaling);
        // SAFETY: tablice z QueryDisplayConfig, zmienione tylko w polu skalowania.
        let r = unsafe {
            SetDisplayConfig(Some(&paths), Some(&modes), SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE | SDC_ALLOW_CHANGES)
        };
        if r != 0 {
            return Err(format!("SetDisplayConfig: {r}"));
        }
        Ok(())
    }

    /// Włącza albo wyłącza HDR ekranu.
    pub fn set_hdr(adapter: (u32, i32), target: u32, on: bool) -> Result<(), String> {
        let mut state = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                std::mem::size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(),
                LUID { LowPart: adapter.0, HighPart: adapter.1 },
                target,
            ),
            ..Default::default()
        };
        state.Anonymous.value = u32::from(on);
        // SAFETY: nagłówek o właściwym typie i rozmiarze.
        let r = unsafe { DisplayConfigSetDeviceInfo(&state.header) };
        if r != 0 {
            return Err(format!("DisplayConfigSetDeviceInfo: {r}"));
        }
        Ok(())
    }

    // ------------------------------- rejestr ---------------------------------

    use windows::Win32::System::Registry::*;

    pub fn read_string(sub: &str, value: &str) -> Option<String> {
        let (sub, value) = (wide(sub), wide(value));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: bufor o rozmiarze `len` bajtów.
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            )
        };
        (r == ERROR_SUCCESS).then(|| text(&buf[..(len as usize / 2).min(buf.len())]))
    }

    pub fn write_string(sub: &str, value: &str, data: &str) -> Result<(), String> {
        let (sub, value, data) = (wide(sub), wide(value), wide(data));
        // SAFETY: teksty zakończone zerem; rozmiar w bajtach z zerem.
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                PCWSTR(value.as_ptr()),
                REG_SZ.0,
                Some(data.as_ptr() as *const _),
                (data.len() * 2) as u32,
            )
        };
        (r == ERROR_SUCCESS).then_some(()).ok_or_else(|| format!("RegSetKeyValue: {}", r.0))
    }

    pub fn delete_value(sub: &str, value: &str) -> Result<(), String> {
        let (sub, value) = (wide(sub), wide(value));
        // SAFETY: teksty zakończone zerem.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), PCWSTR(value.as_ptr())) };
        // 2 = ERROR_FILE_NOT_FOUND: wartości już nie ma.
        (r == ERROR_SUCCESS || r.0 == 2).then_some(()).ok_or_else(|| format!("RegDeleteKeyValue: {}", r.0))
    }
}

// --------------------------------- panel -------------------------------------

/// Zmiana trybu czekająca na potwierdzenie.
struct Pending {
    gdi: String,
    old: Mode,
    until: Instant,
}

#[derive(Default)]
pub struct Basic {
    adapters: Option<Vec<Adapter>>,
    displays: Option<Result<Vec<Display>, String>>,
    global: Option<String>,
    pending: Option<Pending>,
    /// Wybrany tryb dla ekranu (indeks ekranu -> tryb).
    chosen: std::collections::HashMap<usize, Mode>,
    app: String,
    app_pref: Option<u32>,
    status: Option<(String, bool)>,
}

impl Basic {
    pub fn new() -> Self {
        Self::default()
    }

    fn reload(&mut self) {
        self.adapters = None;
        self.displays = None;
        self.global = None;
    }

    fn say(&mut self, name: &str, r: Result<(), String>) {
        self.status = Some(match r {
            Ok(()) => (crate::trf!("{name}: zmieniono.", "{name}: changed."), false),
            Err(e) => (format!("{name}: {e}"), true),
        });
    }

    fn adapters(&mut self) -> &[Adapter] {
        #[cfg(windows)]
        if self.adapters.is_none() {
            self.adapters = Some(sys::adapters());
        }
        self.adapters.get_or_insert_with(Vec::new)
    }

    /// Nagłówek: dlaczego panel jest podstawowy, karta, sterownik, pamięć.
    pub fn header(&mut self, ui: &mut egui::Ui, reason: &str) {
        ui.label(
            RichText::new(tr(
                "API Intela (IGCL) nie obsługuje tej karty - grafiki sprzed 11. generacji działają na starszym sterowniku Intela. Poniżej ustawienia Windows, które działają z każdą kartą.",
                "The Intel API (IGCL) doesn't support this card - graphics older than 11th gen run on Intel's legacy driver. Below are Windows settings that work with any card.",
            ))
            .strong(),
        );
        ui.label(RichText::new(reason).small().color(muted()));
        let adapters = self.adapters().to_vec();
        ui.horizontal_wrapped(|ui| {
            for a in &adapters {
                ui.label(RichText::new(&a.name).strong().color(accent()));
                if !a.driver.is_empty() {
                    ui.label(RichText::new(crate::trf!("sterownik {}", "driver {}", a.driver)).color(muted()));
                }
                let mb = |b: u64| b / (1024 * 1024);
                ui.label(
                    RichText::new(crate::trf!(
                        "pamięć: {} MB dedykowanej, {} MB współdzielonej",
                        "memory: {} MB dedicated, {} MB shared",
                        mb(a.used.0),
                        mb(a.used.1)
                    ))
                    .color(muted()),
                );
            }
            if ui.button(tr("Odczytaj ponownie", "Read again")).clicked() {
                self.reload();
                self.status = None;
            }
        });
        ui.add_space(8.0);
    }

    fn global(&mut self) -> String {
        #[cfg(windows)]
        if self.global.is_none() {
            self.global = Some(sys::read_string(GPU_PREFS, GLOBAL).unwrap_or_default());
        }
        self.global.clone().unwrap_or_default()
    }

    fn set_global(&mut self, key: &str, on: bool, name: &str) {
        let text = with_setting(&self.global(), key, if on { "1" } else { "0" });
        #[cfg(windows)]
        let r = sys::write_string(GPU_PREFS, GLOBAL, &text);
        #[cfg(not(windows))]
        let r: Result<(), String> = Err(String::new());
        self.global = None;
        self.say(name, r);
    }

    fn switch(&mut self, ui: &mut egui::Ui, key: &str, pl: &'static str, en: &'static str, help_pl: &'static str, help_en: &'static str) {
        let mut on = setting(&self.global(), key).as_deref() == Some("1");
        if ui.checkbox(&mut on, tr(pl, en)).on_hover_text(tr(help_pl, help_en)).changed() {
            self.set_global(key, on, tr(pl, en));
        }
    }

    /// Ustawienia grafiki Windows (strona ustawień 3D).
    pub fn graphics_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(tr("Grafika Windows", "Windows graphics")).strong().color(accent()));
        let mut hags = crate::hags::read().unwrap_or(false);
        if ui
            .checkbox(&mut hags, tr("Sprzętowe planowanie GPU (HAGS)", "Hardware-accelerated GPU scheduling (HAGS)"))
            .on_hover_text(tr("Działa po ponownym uruchomieniu komputera. Wymaga uprawnień administratora.", "Takes effect after a restart. Needs administrator rights."))
            .changed()
        {
            let r = crate::hags::set(hags);
            self.say("HAGS", r);
        }
        self.switch(
            ui,
            "SwapEffectUpgradeEnable",
            "Optymalizacje gier w oknie",
            "Optimizations for windowed games",
            "Gry DirectX 10/11 w oknie dostają niższe opóźnienie i obsługę zmiennego odświeżania.",
            "DirectX 10/11 games in a window get lower latency and variable refresh support.",
        );
        self.switch(
            ui,
            "VRROptimizeEnable",
            "Zmienne odświeżanie (VRR) w Windows",
            "Variable refresh rate (VRR) in Windows",
            "Zmienne odświeżanie dla gier, które same go nie obsługują - tylko na ekranach z VRR i kartach, które je obsługują.",
            "Variable refresh for games that don't support it themselves - only on VRR displays and cards that support it.",
        );
        ui.add_space(8.0);
        ui.label(RichText::new(tr("Karta dla gry", "Card for a game")).strong().color(accent()));
        ui.label(
            RichText::new(tr(
                "W laptopie z dwiema kartami wybiera, na której działa gra (pełna ścieżka do pliku .exe).",
                "On a laptop with two cards, chooses which one runs the game (full path to the .exe).",
            ))
            .small()
            .color(muted()),
        );
        ui.horizontal_wrapped(|ui| {
            let edit = ui.add(egui::TextEdit::singleline(&mut self.app).hint_text(r"C:\Games\Game\game.exe").desired_width(320.0));
            if edit.changed() {
                self.app_pref = None;
            }
        });
        let app = self.app.trim().to_owned();
        if !app.is_empty() {
            #[cfg(windows)]
            if self.app_pref.is_none() {
                self.app_pref = Some(
                    sys::read_string(GPU_PREFS, &app)
                        .and_then(|t| setting(&t, "GpuPreference"))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                );
            }
            let current = self.app_pref.unwrap_or(0);
            ui.horizontal_wrapped(|ui| {
                for (value, pl, en) in [(0u32, "Windows decyduje", "Let Windows decide"), (1, "Oszczędzanie energii", "Power saving"), (2, "Wysoka wydajność", "High performance")] {
                    if ui.selectable_label(current == value, tr(pl, en)).clicked() && current != value {
                        #[cfg(windows)]
                        let r = if value == 0 {
                            sys::delete_value(GPU_PREFS, &app)
                        } else {
                            let old = sys::read_string(GPU_PREFS, &app).unwrap_or_default();
                            sys::write_string(GPU_PREFS, &app, &with_setting(&old, "GpuPreference", &value.to_string()))
                        };
                        #[cfg(not(windows))]
                        let r: Result<(), String> = Err(String::new());
                        self.app_pref = None;
                        self.say(tr("Karta dla gry", "Card for a game"), r);
                    }
                }
            });
        }
    }

    /// Strona Adaptive Sync: wyjaśnienie i VRR Windows.
    pub fn sync_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(tr(
                "Adaptive Sync w grafice Intela wymaga procesora 11. generacji lub nowszego. Na tej karcie zostaje zmienne odświeżanie Windows - działa tylko z ekranem, który je obsługuje.",
                "Adaptive Sync on Intel graphics needs an 11th-gen processor or newer. On this card Windows variable refresh remains - it works only with a display that supports it.",
            ))
            .color(muted()),
        );
        ui.add_space(6.0);
        self.switch(
            ui,
            "VRROptimizeEnable",
            "Zmienne odświeżanie (VRR) w Windows",
            "Variable refresh rate (VRR) in Windows",
            "Zmienne odświeżanie dla gier, które same go nie obsługują.",
            "Variable refresh for games that don't support it themselves.",
        );
    }

    /// Strona ekranu: tryb, skalowanie, HDR dla ekranów karty Intel.
    pub fn display_ui(&mut self, ui: &mut egui::Ui) {
        self.pending_ui(ui);
        #[cfg(windows)]
        if self.displays.is_none() {
            self.displays = Some(sys::displays());
        }
        let luids: Vec<(u32, i32)> = self.adapters().iter().map(|a| a.luid).collect();
        let displays = match self.displays.clone() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(warn()));
                return;
            }
            None => Vec::new(),
        };
        let mine: Vec<(usize, Display)> = displays.into_iter().enumerate().filter(|(_, d)| luids.contains(&d.adapter)).collect();
        if mine.is_empty() {
            ui.label(RichText::new(tr(
                "Do karty Intel nie jest podłączony żaden aktywny ekran (w laptopie z dwiema kartami ekran może obsługiwać druga karta).",
                "No active display is connected to the Intel card (on a laptop with two cards the other card may drive the display).",
            ))
            .color(muted()));
            return;
        }
        for (i, d) in mine {
            ui.label(RichText::new(format!("{} ({})", d.name, d.gdi)).strong().color(accent()));
            let current = d.current;
            let chosen = *self.chosen.get(&i).or(current.as_ref()).unwrap_or(&Mode { width: 0, height: 0, hz: 0 });
            let label = |m: &Mode| format!("{} × {} @ {} Hz", m.width, m.height, m.hz);
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Tryb:", "Mode:"));
                egui::ComboBox::from_id_salt(("intel-basic-mode", i)).selected_text(label(&chosen)).width(220.0).show_ui(ui, |ui| {
                    for m in &d.modes {
                        if ui.selectable_label(*m == chosen, label(m)).clicked() {
                            self.chosen.insert(i, *m);
                        }
                    }
                });
                let busy = self.pending.is_some();
                if ui.add_enabled(!busy && Some(chosen) != current && chosen.width > 0, egui::Button::new(tr("Zastosuj", "Apply"))).clicked() {
                    #[cfg(windows)]
                    {
                        let r = sys::set_mode(&d.gdi, chosen);
                        if r.is_ok() {
                            if let Some(old) = current {
                                self.pending = Some(Pending { gdi: d.gdi.clone(), old, until: Instant::now() + Duration::from_secs(KEEP_SECONDS) });
                            }
                        }
                        self.displays = None;
                        self.say(tr("Tryb ekranu", "Display mode"), r);
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Skalowanie:", "Scaling:"));
                for (value, pl, en) in [
                    (128, "Domyślne", "Default"),
                    (4, "Zachowaj proporcje", "Keep aspect ratio"),
                    (3, "Rozciągnij", "Stretch"),
                    (2, "Wyśrodkuj", "Center"),
                ] {
                    if ui.selectable_label(d.scaling == value, tr(pl, en)).clicked() && d.scaling != value {
                        #[cfg(windows)]
                        {
                            let r = sys::set_scaling(d.adapter, d.target, value);
                            self.displays = None;
                            self.say(tr("Skalowanie", "Scaling"), r);
                        }
                    }
                }
            });
            if d.hdr.0 {
                let mut on = d.hdr.1;
                if ui.checkbox(&mut on, "HDR").changed() {
                    #[cfg(windows)]
                    {
                        let r = sys::set_hdr(d.adapter, d.target, on);
                        self.displays = None;
                        self.say("HDR", r);
                    }
                }
            }
            ui.add_space(6.0);
        }
    }

    /// Zmiana trybu czekająca na potwierdzenie; bez niego powrót po 15 s.
    fn pending_ui(&mut self, ui: &mut egui::Ui) {
        let Some(p) = &self.pending else { return };
        let left = p.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let (gdi, old) = (p.gdi.clone(), p.old);
            self.pending = None;
            #[cfg(windows)]
            {
                let r = sys::set_mode(&gdi, old);
                self.displays = None;
                self.say(tr("Przywrócono poprzedni tryb", "Previous mode restored"), r);
            }
            #[cfg(not(windows))]
            let _ = (gdi, old);
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::trf!(
                    "Zachować nowy tryb? Powrót za {} s.",
                    "Keep the new mode? Reverting in {} s.",
                    left.as_secs() + 1
                ))
                .strong()
                .color(warn()),
            );
            if ui.button(tr("Zachowaj", "Keep")).clicked() {
                self.pending = None;
                self.status = Some((tr("Zachowano.", "Kept.").to_owned(), false));
            }
            if ui.button(tr("Przywróć", "Revert")).clicked() {
                if let Some(p) = &mut self.pending {
                    p.until = Instant::now();
                }
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        ui.add_space(6.0);
    }

    pub fn status_ui(&self, ui: &mut egui::Ui) {
        if let Some((text, error)) = &self.status {
            ui.add_space(4.0);
            ui.label(RichText::new(text).color(if *error { warn() } else { accent() }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_directx_settings_text() {
        let t = "SwapEffectUpgradeEnable=1;VRROptimizeEnable=0;";
        assert_eq!(setting(t, "vrroptimizeenable").as_deref(), Some("0"));
        assert_eq!(setting(t, "AutoHDREnable"), None);
        assert_eq!(with_setting(t, "VRROptimizeEnable", "1"), "SwapEffectUpgradeEnable=1;VRROptimizeEnable=1;");
        assert_eq!(with_setting("", "GpuPreference", "2"), "GpuPreference=2;");
        assert_eq!(with_setting("GpuPreference=1;", "GpuPreference", "2"), "GpuPreference=2;");
    }

    #[test]
    fn sorts_modes_without_duplicates() {
        let m = |w, h, hz| Mode { width: w, height: h, hz };
        let list = sorted_modes(vec![m(1280, 720, 60), m(1920, 1080, 60), m(1920, 1080, 144), m(1280, 720, 60)]);
        assert_eq!(list, vec![m(1920, 1080, 144), m(1920, 1080, 60), m(1280, 720, 60)]);
    }

    /// Odczyt kart i ekranów z tego komputera (tylko odczyt):
    /// `cargo test intel_basic_read -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn intel_basic_read() {
        for a in sys::adapters() {
            println!("karta {a:?}");
        }
        for d in sys::displays().unwrap() {
            println!(
                "ekran {} {} adapter {:?} cel {} tryb {:?} trybów {} skalowanie {} hdr {:?}",
                d.name,
                d.gdi,
                d.adapter,
                d.target,
                d.current,
                d.modes.len(),
                d.scaling,
                d.hdr
            );
        }
        println!("ustawienia {:?}", sys::read_string(GPU_PREFS, GLOBAL));
    }
}
