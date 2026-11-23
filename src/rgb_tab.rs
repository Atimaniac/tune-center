//! Zakładka RGB: oświetlenie wszystkich obsługiwanych urządzeń w jednym
//! miejscu - wspólny kolor albo osobno dla każdego urządzenia.

use eframe::egui::{self, RichText};

use crate::config::Config;
use crate::i18n::tr;
use crate::icons::Icon;
use crate::rgb::{self, Mode, Setting};
use crate::theme::{self, accent, muted, warn};

pub struct RgbTab {
    manager: rgb::Manager,
}

impl RgbTab {
    pub fn start(config: &rgb::Config, elevated: bool, ctx: &egui::Context) -> Self {
        let ctx = ctx.clone();
        Self {
            manager: rgb::Manager::start(config.clone(), elevated, move || ctx.request_repaint()),
        }
    }

    /// Praca w tle (także przy ukrytym oknie). Zwraca `true`, gdy trzeba
    /// zapisać konfigurację.
    pub fn logic(&mut self, config: &mut rgb::Config) -> bool {
        match self.manager.take_ram_saved() {
            Some(saved) => {
                config.ram_saved = saved;
                true
            }
            None => false,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, config: &mut Config, elevated: bool) {
        let snap = self.manager.snapshot();
        let mut changed = false;

        egui::ScrollArea::vertical().show(ui, |ui| {
            theme::heading(ui, Icon::Rgb, tr("Oświetlenie RGB", "RGB lighting"));

            // Wspólne ustawienie i wyszukiwanie.
            let width = theme::grid_card_width(ui.available_width(), 380.0, 14.0, 1, 18.0);
            theme::card(14, 18).show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal_wrapped(|ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.rgb.sync,
                            tr(
                                "To samo ustawienie na wszystkich urządzeniach",
                                "The same setting on all devices",
                            ),
                        )
                        .changed();
                    ui.add_space(16.0);
                    let label = if snap.scanning {
                        tr("Wyszukiwanie...", "Searching...")
                    } else {
                        tr("Szukaj urządzeń ponownie", "Search for devices again")
                    };
                    if theme::primary_button(ui, Some(Icon::Refresh), label, !snap.scanning).clicked() {
                        self.manager.rescan();
                    }
                });
                if config.rgb.sync {
                    ui.add_space(6.0);
                    changed |= setting_ui(ui, "rgb-all", &mut config.rgb.all);
                }
                ui.add_space(6.0);
                theme::info(
                    ui,
                    tr(
                        "Program nie zapisuje niczego w pamięci urządzeń: po wyłączeniu zasilania wracają do swoich efektów, a Tune Center ustawia kolor ponownie przy każdym starcie (także z Windows). „Bez zmian” oddaje sterowanie urządzeniu. Aplikacje producentów (G HUB, Armoury Crate, iCUE, MSI Center, RGB Fusion, SkyDimo) i OpenRGB mogą nadpisywać ustawienia - najlepiej, żeby nie działały jednocześnie.",
                        "The program saves nothing in device memory: after a power cycle they return to their own effects, and Tune Center sets the color again at every start (also with Windows). \"Unchanged\" returns control to the device. Vendor apps (G HUB, Armoury Crate, iCUE, MSI Center, RGB Fusion, SkyDimo) and OpenRGB may override the settings - preferably do not run them at the same time.",
                    ),
                );
            });
            ui.add_space(14.0);

            if snap.devices.is_empty() && !snap.scanning {
                theme::info(
                    ui,
                    tr(
                        "Nie znaleziono urządzeń z obsługiwanym podświetleniem. Obsługiwane: pamięć RAM z kontrolerem ENE (np. G.Skill Trident Z RGB), taśmy Skydimo, urządzenia Logitech G (HID++), karty NVIDIA ze strefami oświetlenia w sterowniku, ASUS Aura (USB), Gigabyte RGB Fusion 2 (USB), Corsair Lighting Node / Commander Pro i MSI Mystic Light.",
                        "No devices with supported lighting were found. Supported: RAM with an ENE controller (e.g. G.Skill Trident Z RGB), Skydimo strips, Logitech G devices (HID++), NVIDIA cards with lighting zones in the driver, ASUS Aura (USB), Gigabyte RGB Fusion 2 (USB), Corsair Lighting Node / Commander Pro and MSI Mystic Light.",
                    ),
                );
                ui.add_space(10.0);
            }

            let sync = config.rgb.sync;
            theme::card_grid(ui, &snap.devices, 330.0, 14.0, 3, 14, 16, |ui, _, entry, _| {
                ui.vertical(|ui| device_card(ui, entry, sync, &mut config.rgb, &mut changed));
            });

            for p in &snap.problems {
                // Brak administratora pokazujemy tylko, gdy program nie ma uprawnień.
                if p.family == rgb::Family::EneRam && elevated {
                    continue;
                }
                theme::info(ui, &format!("{}: {}", p.family.label(), p.text));
                ui.add_space(4.0);
            }
        });

        if changed {
            config.save();
            self.manager.set(&config.rgb);
        }
    }
}

/// Karta jednego urządzenia.
fn device_card(
    ui: &mut egui::Ui,
    entry: &rgb::Entry,
    sync: bool,
    config: &mut rgb::Config,
    changed: &mut bool,
) {
    let info = &entry.info;
    ui.horizontal(|ui| {
        ui.label(RichText::new(&info.name).size(15.5).strong().color(theme::text()));
        if !info.family.tested() {
            ui.label(
                RichText::new(tr("nietestowane", "untested"))
                    .size(11.0)
                    .color(warn()),
            )
            .on_hover_text(tr(
                "Ta rodzina urządzeń nie była sprawdzona na prawdziwym sprzęcie. Program niczego nie zapisuje na stałe, ale efekt może być inny niż wybrany.",
                "This device family was not checked on real hardware. The program saves nothing permanently, but the result may differ from the choice.",
            ));
        }
    });
    ui.label(RichText::new(info.family.label()).size(12.0).color(muted()));
    if !info.detail.is_empty() {
        ui.add(egui::Label::new(RichText::new(&info.detail).size(11.5).color(muted())).wrap());
    }
    ui.add_space(6.0);

    if sync {
        ui.label(
            RichText::new(tr(
                "Ustawienie wspólne (u góry).",
                "Shared setting (above).",
            ))
            .size(12.0)
            .color(muted()),
        );
    } else {
        let mut s = config.devices.get(&info.id).copied().unwrap_or_default();
        if setting_ui(ui, &info.id, &mut s) {
            config.devices.insert(info.id.clone(), s);
            *changed = true;
        }
        if !info.restores && s.mode == Mode::Untouched {
            ui.label(
                RichText::new(tr(
                    "Po zmianie efekt urządzenia wróci dopiero po wyłączeniu zasilania.",
                    "After a change the device effect returns only after a power cycle.",
                ))
                .size(11.0)
                .color(muted()),
            );
        }
    }

    if let Some(status) = &entry.status {
        let (text, color) = match status {
            Ok(t) => (t.as_str(), accent()),
            Err(e) => (e.as_str(), warn()),
        };
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
    }
}

/// Tryb, kolor i jasność. Zwraca `true` po zmianie.
fn setting_ui(ui: &mut egui::Ui, id: &str, s: &mut Setting) -> bool {
    let mut changed = false;
    ui.push_id(id, |ui| {
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .radio_value(&mut s.mode, Mode::Untouched, tr("Bez zmian", "Unchanged"))
                .changed();
            changed |= ui
                .radio_value(&mut s.mode, Mode::Color, tr("Kolor", "Color"))
                .changed();
            changed |= ui
                .radio_value(&mut s.mode, Mode::Off, tr("Wyłączone", "Off"))
                .changed();
        });
        if s.mode == Mode::Color {
            ui.horizontal(|ui| {
                changed |= egui::color_picker::color_edit_button_srgb(ui, &mut s.color).changed();
                ui.add_space(10.0);
                ui.label(tr("Jasność:", "Brightness:"));
                changed |= ui
                    .add(egui::Slider::new(&mut s.brightness, 0..=100).suffix(" %"))
                    .changed();
            });
        }
    });
    changed
}

impl RgbTab {
    /// Stan urządzeń (diagnostyka zgodności).
    pub fn snapshot(&self) -> rgb::Snapshot {
        self.manager.snapshot()
    }
}
