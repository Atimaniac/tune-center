//! NVIDIA > OLED fix: wartość `vrrFrameSplitBehavior` w kluczu sterownika
//! karty (klasa kart graficznych w `HKLM\SYSTEM\CurrentControlSet\Control\Class`).
//!
//! Sterownik przy VRR (G-SYNC) podwaja klatki (LFC), gdy FPS spada poniżej
//! progu - np. 60 FPS -> 120 Hz. Na monitorach OLED zmiana odświeżania daje
//! widoczne migotanie jasności. Bit 16 (0x80010055) przełącza wybór mnożnika
//! na „najniższy pasujący”: 1× aż do dolnej granicy zakresu VRR (48 FPS).
//! Domyślnie sterownik używa 0x80000055 (brak wpisu). Działa po restarcie.
//! Czysta instalacja sterownika usuwa wpis - program przywraca go przy starcie.

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::theme::{self, accent, muted, warn};
use crate::tune::reg;

const CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
const VALUE: &str = "vrrFrameSplitBehavior";
const STATE_FILE: &str = "oled-fix.json";

/// Najniższy pasujący mnożnik (bit 16) - opis z instrukcji.
pub const LOWEST_FIT: u32 = 0x8001_0055;
/// Domyślne zachowanie, ale „lepsze trzymanie” trybu (podwajanie < ~56,5 FPS przy 360 Hz).
pub const STICKY: u32 = 0x8000_0032;
/// Wartość, której sterownik używa bez wpisu.
pub const DRIVER_DEFAULT: u32 = 0x8000_0055;

/// Karta NVIDIA w kluczu klasy.
#[derive(Clone, Debug)]
pub struct Adapter {
    pub key: String,
    pub desc: String,
    /// Wersja w zapisie Windows, np. 32.0.16.2135.
    pub version: String,
    pub value: Option<u32>,
}

impl Adapter {
    fn path(&self) -> String {
        format!(r"{CLASS}\{}", self.key)
    }

    /// Wersja w zapisie NVIDIA (621.35).
    pub fn nv_version(&self) -> Option<String> {
        nv_version(&self.version)
    }
}

/// 32.0.16.2135 -> 621.35 (ostatnie 5 cyfr z dwóch ostatnich grup).
pub fn nv_version(windows: &str) -> Option<String> {
    let parts: Vec<&str> = windows.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let digits = format!("{}{:0>4}", parts[2], parts[3]);
    if digits.len() < 5 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let last = &digits[digits.len() - 5..];
    Some(format!("{}.{}", &last[..3], &last[3..]))
}

/// Karty NVIDIA (podklucze 0000, 0001...; `Properties` i `Configuration` pomijane).
pub fn adapters() -> Vec<Adapter> {
    reg::subkeys(reg::HKLM, CLASS)
        .into_iter()
        .filter(|k| k.len() == 4 && k.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|key| {
            let path = format!(r"{CLASS}\{key}");
            let id = reg::get_string(reg::HKLM, &path, "MatchingDeviceId")?;
            if !id.to_lowercase().contains("ven_10de") {
                return None;
            }
            Some(Adapter {
                desc: reg::get_string(reg::HKLM, &path, "DriverDesc").unwrap_or_default(),
                version: reg::get_string(reg::HKLM, &path, "DriverVersion").unwrap_or_default(),
                value: reg::get_dword(reg::HKLM, &path, VALUE),
                key,
            })
        })
        .collect()
}

/// Karty, których dotyczy zmiana: z wersją zgodną z działającym sterownikiem;
/// gdy żadna nie pasuje (albo wersja nieznana) - wszystkie karty NVIDIA.
fn targets(all: &[Adapter], driver: Option<&str>) -> Vec<Adapter> {
    let current = driver.map(|d| d.split(' ').next().unwrap_or(d).trim().to_owned());
    let matching: Vec<Adapter> = all
        .iter()
        .filter(|a| current.is_some() && a.nv_version() == current)
        .cloned()
        .collect();
    if matching.is_empty() {
        all.to_vec()
    } else {
        matching
    }
}

/// Zapamiętany wybór (przywracany po aktualizacji sterownika) i wartość sprzed
/// pierwszej zmiany (gdy była inna niż nasze).
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct Saved {
    /// 0 = wyłączone (domyślne sterownika).
    value: u32,
    #[serde(default)]
    original: Option<u32>,
}

fn state_path() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(STATE_FILE))
}

fn load() -> Saved {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store(s: &Saved) {
    if let (Some(p), Ok(json)) = (state_path(), serde_json::to_string_pretty(s)) {
        let _ = std::fs::write(p, json);
    }
}

fn error_text(e: String) -> String {
    if e.ends_with("błąd 5") || e.ends_with("error 5") {
        tr(
            "Brak uprawnień - uruchom program jako administrator (przycisk w pasku stanu).",
            "Access denied - run the program as administrator (button in the status bar).",
        )
        .to_owned()
    } else {
        e
    }
}

/// Zapisuje wartość (`None` = usunięcie wpisu) na kartach, sprawdza odczytem,
/// a przy niezgodności przywraca poprzednią. Zwraca liczbę zmienionych kart.
fn write(targets: &[Adapter], value: Option<u32>) -> Result<usize, String> {
    let mut done = 0;
    for a in targets {
        let path = a.path();
        let old = reg::get_dword(reg::HKLM, &path, VALUE);
        if old == value {
            continue;
        }
        reg::set_dword(reg::HKLM, &path, VALUE, value).map_err(error_text)?;
        let now = reg::get_dword(reg::HKLM, &path, VALUE);
        if now != value {
            let _ = reg::set_dword(reg::HKLM, &path, VALUE, old);
            return Err(crate::trf!(
                "{}: odczyt po zapisie się nie zgadza - przywrócono poprzednią wartość",
                "{}: read-back after the write does not match - the previous value was restored",
                a.desc
            ));
        }
        done += 1;
    }
    Ok(done)
}

/// Przy starcie: jeśli poprawka była włączona, a sterownik (np. po czystej
/// instalacji) jej nie ma - zapisuje ją ponownie. Zwraca komunikat dla paska stanu.
pub fn reapply() -> Option<String> {
    let mut saved = load();
    let all = adapters();
    if saved.value == 0 {
        // Wartość ustawiona ręcznie (poza programem) - zapamiętujemy ją, żeby
        // przetrwała kolejne instalacje sterownika.
        if let Some(v) = all
            .iter()
            .find_map(|a| a.value.filter(|v| [LOWEST_FIT, STICKY].contains(v)))
        {
            saved.value = v;
            store(&saved);
        }
        return None;
    }
    let missing: Vec<Adapter> = all
        .iter()
        .filter(|a| a.value != Some(saved.value))
        .cloned()
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(match write(&missing, Some(saved.value)) {
        Ok(0) => return None,
        Ok(_) => tr(
            "OLED fix: sterownik NVIDIA usunął ustawienie (aktualizacja) - przywrócono, działa po restarcie komputera.",
            "OLED fix: the NVIDIA driver removed the setting (update) - restored, takes effect after a reboot.",
        )
        .to_owned(),
        Err(e) => crate::trf!("OLED fix: {e}", "OLED fix: {e}"),
    })
}

/// Opis wartości.
pub fn describe(v: Option<u32>) -> String {
    match v {
        None => tr(
            "brak wpisu - domyślne sterownika (0x80000055)",
            "no value - driver default (0x80000055)",
        )
        .to_owned(),
        Some(LOWEST_FIT) => tr(
            "0x80010055 - najniższy mnożnik (poprawka włączona)",
            "0x80010055 - lowest multiplier (fix enabled)",
        )
        .to_owned(),
        Some(STICKY) => tr(
            "0x80000032 - domyślne, bardziej „lepkie”",
            "0x80000032 - default, stickier",
        )
        .to_owned(),
        Some(DRIVER_DEFAULT) => tr(
            "0x80000055 - domyślne sterownika",
            "0x80000055 - driver default",
        )
        .to_owned(),
        Some(v) => crate::trf!("0x{v:08X} - własna wartość", "0x{v:08X} - custom value"),
    }
}

/// Podzakładka NVIDIA > OLED fix.
pub struct Panel {
    adapters: Option<Vec<Adapter>>,
    /// Wybrany wariant do włączenia.
    choice: u32,
    status: Option<(String, bool)>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            adapters: None,
            choice: LOWEST_FIT,
            status: None,
        }
    }
}

impl Panel {
    fn apply(&mut self, driver: Option<&str>, value: Option<u32>) {
        let all = adapters();
        let targets = targets(&all, driver);
        if targets.is_empty() {
            self.status = Some((
                tr(
                    "Nie znaleziono karty NVIDIA w rejestrze.",
                    "No NVIDIA card found in the registry.",
                )
                .to_owned(),
                true,
            ));
            return;
        }
        let mut saved = load();
        // Wartość sprzed pierwszej zmiany, jeśli była inna niż nasze warianty.
        if saved.value == 0 && saved.original.is_none() {
            saved.original = targets[0]
                .value
                .filter(|v| ![LOWEST_FIT, STICKY, DRIVER_DEFAULT].contains(v));
        }
        // Wyłączenie: wartość sprzed zmian, a bez niej - usunięcie wpisu (jak w instrukcji).
        let write_value = match value {
            Some(v) => Some(v),
            None => saved.original,
        };
        self.status = Some(match write(&targets, write_value) {
            Ok(n) => {
                saved.value = value.unwrap_or(0);
                if value.is_none() {
                    saved.original = None;
                }
                store(&saved);
                if n == 0 {
                    (
                        tr(
                            "Bez zmian - wartość już ustawiona.",
                            "No change - the value is already set.",
                        )
                        .to_owned(),
                        false,
                    )
                } else {
                    (
                        tr(
                            "Zapisano i sprawdzono odczytem. Uruchom ponownie komputer, żeby sterownik użył nowej wartości.",
                            "Written and verified. Reboot the PC so the driver picks up the new value.",
                        )
                        .to_owned(),
                        false,
                    )
                }
            }
            Err(e) => (e, true),
        });
        self.adapters = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, driver: Option<&str>) {
        let all = self.adapters.get_or_insert_with(adapters).clone();
        let targets = targets(&all, driver);
        let current = targets.first().and_then(|a| a.value);
        let enabled = current.is_some_and(|v| v != DRIVER_DEFAULT);

        theme::heading(
            ui,
            crate::icons::Icon::Gpu,
            tr("OLED fix - migotanie przy VRR", "OLED fix - VRR flicker"),
        );
        ui.add_space(4.0);
        ui.label(tr(
            "Przy G-SYNC sterownik podwaja klatki, gdy FPS spada poniżej progu (np. 60 FPS -> odświeżanie 120 Hz). Na monitorach OLED każda taka zmiana mnożnika jest widoczna jako migotanie jasności. Ukryta wartość vrrFrameSplitBehavior = 0x80010055 przełącza sterownik na najniższy mnożnik: 1× dopóki FPS mieści się w zakresie VRR, podwajanie dopiero poniżej 48 FPS.",
            "With G-SYNC the driver doubles frames when FPS drops below a threshold (e.g. 60 FPS -> 120 Hz refresh). On OLED monitors every such multiplier change shows up as brightness flicker. The hidden value vrrFrameSplitBehavior = 0x80010055 switches the driver to the lowest multiplier: 1× as long as FPS is inside the VRR range, doubling only below 48 FPS.",
        ));
        ui.add_space(8.0);

        let width = ui.available_width() - 2.0 * 14.0 - 4.0;
        theme::card(10, 14).show(ui, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Stan:", "Status:")).color(muted()));
                let (text, color) = if enabled {
                    (tr("WŁĄCZONY", "ON"), theme::ok())
                } else {
                    (tr("WYŁĄCZONY", "OFF"), muted())
                };
                crate::history::badge(ui, text, color);
                ui.label(describe(current));
            });
            ui.add_space(4.0);
            if all.is_empty() {
                ui.label(RichText::new(tr("Nie znaleziono karty NVIDIA w rejestrze.", "No NVIDIA card found in the registry.")).color(warn()));
            }
            for a in &all {
                let used = targets.iter().any(|t| t.key == a.key);
                let text = crate::trf!(
                    "{} - klucz {}, sterownik {} ({}), wartość: {}",
                    "{} - key {}, driver {} ({}), value: {}",
                    a.desc,
                    a.key,
                    a.nv_version().unwrap_or_default(),
                    a.version,
                    a.value.map(|v| format!("0x{v:08X}")).unwrap_or_else(|| "-".into())
                );
                ui.label(RichText::new(text).size(12.0).color(if used { theme::text() } else { muted() }));
            }

            ui.add_space(10.0);
            ui.label(RichText::new(tr("Wariant", "Variant")).strong().color(accent()));
            ui.radio_value(
                &mut self.choice,
                LOWEST_FIT,
                tr(
                    "0x80010055 - najniższy mnożnik (zalecane): 1× do 48 FPS",
                    "0x80010055 - lowest multiplier (recommended): 1× down to 48 FPS",
                ),
            );
            ui.radio_value(
                &mut self.choice,
                STICKY,
                tr(
                    "0x80000032 - zachowanie NVIDIA, ale rzadsze przełączanie (podwajanie poniżej ~56,5 FPS, koniec powyżej ~70,6 FPS przy 360 Hz)",
                    "0x80000032 - NVIDIA behavior but stickier (doubling below ~56.5 FPS, off above ~70.6 FPS at 360 Hz)",
                ),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if enabled && current == Some(self.choice) {
                    tr("Włączony", "Enabled")
                } else {
                    tr("Włącz", "Enable")
                };
                if theme::primary_button(ui, None, label, !all.is_empty()).clicked() {
                    self.apply(driver, Some(self.choice));
                }
                if ui
                    .add_enabled(enabled, egui::Button::new(tr("Wyłącz (domyślne sterownika)", "Disable (driver default)")))
                    .clicked()
                {
                    self.apply(driver, None);
                }
                if ui.button(tr("Odśwież", "Refresh")).clicked() {
                    self.adapters = None;
                }
            });
            if let Some((text, error)) = &self.status {
                ui.add_space(4.0);
                ui.label(RichText::new(text).color(if *error { warn() } else { accent() }));
            }
        });

        ui.add_space(10.0);
        theme::card(10, 14).show(ui, |ui| {
            ui.set_width(width);
            ui.label(RichText::new(tr("Przykład (monitor 360 Hz)", "Example (360 Hz monitor)")).strong().color(accent()));
            egui::Grid::new("oled_example").num_columns(3).spacing([28.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new("FPS").color(muted()));
                ui.label(RichText::new(tr("Domyślnie", "Default")).color(muted()));
                ui.label(RichText::new("0x80010055").color(muted()));
                ui.end_row();
                for (fps, def, fix) in [("60", "2× -> 120 Hz", "1× -> 60 Hz"), ("50", "2× -> 100 Hz", "1× -> 50 Hz"), ("47", "2× -> 94 Hz", "2× -> 94 Hz")] {
                    ui.label(fps);
                    ui.label(def);
                    ui.label(fix);
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            theme::info(
                ui,
                tr(
                    "Działa po restarcie komputera. Sprawdzenie: ogranicz grę do 55 FPS (np. RTSS) i patrz na licznik odświeżania monitora - ok. 55 Hz = działa, ok. 110 Hz = nie zastosowano.",
                    "Takes effect after a reboot. Check: cap a game at 55 FPS (e.g. RTSS) and watch the monitor's refresh counter - about 55 Hz = working, about 110 Hz = not applied.",
                ),
            );
            theme::info(
                ui,
                tr(
                    "Czysta instalacja sterownika usuwa ten wpis - Tune Center przywraca go przy starcie (potem potrzebny restart).",
                    "A clean driver install removes the value - Tune Center restores it at startup (a reboot is needed afterwards).",
                ),
            );
            theme::info(
                ui,
                tr(
                    "Wady: próg przesuwa się na 48 FPS, ale nie znika - gra wahająca się wokół 48 FPS nadal może migotać. Przy 48-50 FPS klatki są na granicy najdłuższego czasu panelu, więc spóźniona klatka może dać krótkie przycięcie; powyżej ~55 FPS zapas jest duży.",
                    "Trade-offs: the switch point moves to 48 FPS but does not disappear - a game wobbling around 48 FPS can still flicker. At 48-50 FPS frames sit at the panel's longest allowed frame time, so a late frame can cause a small hitch; above ~55 FPS there is plenty of headroom.",
                ),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zapis 0x80000032 na karcie, odczyt i powrót do poprzedniej wartości
    /// (bez restartu sterownik nie zauważa zmiany). Wymaga administratora.
    #[test]
    #[ignore]
    fn oled_fix_roundtrip() {
        let all = adapters();
        assert!(!all.is_empty(), "brak karty NVIDIA");
        let a = &all[..1];
        let old = a[0].value;
        println!(
            "{} {}: {:?}",
            a[0].desc,
            a[0].key,
            old.map(|v| format!("{v:#X}"))
        );
        assert_eq!(write(a, Some(STICKY)).unwrap(), 1);
        assert_eq!(adapters()[0].value, Some(STICKY));
        write(a, old).unwrap();
        assert_eq!(adapters()[0].value, old);
        println!("przywrócono {:?}", old.map(|v| format!("{v:#X}")));
    }

    #[test]
    fn driver_version() {
        assert_eq!(nv_version("32.0.16.2135").as_deref(), Some("621.35"));
        assert_eq!(nv_version("31.0.15.5222").as_deref(), Some("552.22"));
        assert_eq!(nv_version("32.0.16.0124").as_deref(), Some("601.24"));
        assert_eq!(nv_version("abc"), None);
    }
}
