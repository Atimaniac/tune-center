//! Strona Fast Load w zakładce TUNE: lista gier z rozmiarem, dyskiem,
//! gotowością do DirectStorage, profilem odczytów i wczytywaniem do pamięci RAM.

use eframe::egui::{self, RichText};

use crate::fastload::{budget, readiness, Bypass, DriveKind, Entry, FastLoad, Load, Readiness, MIN_RAM};
use crate::i18n::tr;
use crate::icons::Icon;
use crate::theme;

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

/// Opis stanu BypassIO dla dysku gry.
fn bypass_text(bypass: &Bypass) -> String {
    match bypass {
        Bypass::Ready => tr("BypassIO: dostępne.", "BypassIO: available.").to_owned(),
        Bypass::Blocked { driver, reason } => crate::trf!(
            "BypassIO: zablokowane przez sterownik \"{driver}\" ({reason}).",
            "BypassIO: blocked by the \"{driver}\" driver ({reason})."
        ),
        Bypass::Unsupported => tr(
            "BypassIO: niedostępne (wymaga Windows 11 i dysku NVMe z systemem NTFS).",
            "BypassIO: not available (needs Windows 11 and an NVMe drive with NTFS).",
        )
        .to_owned(),
    }
}

/// Komórka DirectStorage: czy gra go używa i czy dysk na to pozwala.
fn directstorage_cell(ui: &mut egui::Ui, g: &Entry) {
    if g.size.is_none() {
        ui.label(RichText::new("...").color(theme::muted()));
        return;
    }
    let version = g.dstorage.as_deref().unwrap_or("");
    let (text, color, hover) = match readiness(g.dstorage.is_some(), &g.drive) {
        Readiness::NotUsed => (
            "-".to_owned(),
            theme::muted(),
            tr(
                "Gra nie używa DirectStorage (brak dstorage.dll). Skrócić wczytywanie może Fast Load.",
                "The game does not use DirectStorage (no dstorage.dll). Fast Load can shorten loading.",
            )
            .to_owned(),
        ),
        Readiness::Ready => (
            crate::trf!("GOTOWA  {version}", "READY  {version}"),
            theme::ok(),
            tr(
                "Gra używa DirectStorage, dysk to NVMe, a BypassIO jest dostępne - pełna szybkość. Takie gry czytają pliki z pominięciem pamięci podręcznej Windows, więc Fast Load im nie pomaga.",
                "The game uses DirectStorage, the drive is NVMe and BypassIO is available - full speed. Such games read files bypassing the Windows cache, so Fast Load does not help them.",
            )
            .to_owned(),
        ),
        Readiness::NoBypass => (
            crate::trf!("BEZ BYPASSIO  {version}", "NO BYPASSIO  {version}"),
            theme::warn(),
            format!(
                "{} {}",
                tr(
                    "Gra używa DirectStorage na dysku NVMe, ale bez BypassIO odczyt idzie zwykłą, wolniejszą ścieżką.",
                    "The game uses DirectStorage on an NVMe drive, but without BypassIO reads take the regular, slower path.",
                ),
                bypass_text(&g.drive.bypass)
            ),
        ),
        Readiness::SlowDrive => (
            crate::trf!("WOLNY DYSK  {version}", "SLOW DRIVE  {version}"),
            theme::warn(),
            tr(
                "Gra używa DirectStorage, ale nie leży na dysku NVMe - działa bez korzyści. Przenieś ją na NVMe.",
                "The game uses DirectStorage but is not on an NVMe drive - it works without the benefit. Move it to NVMe.",
            )
            .to_owned(),
        ),
    };
    ui.label(RichText::new(text.trim_end()).color(color)).on_hover_text(hover);
}

/// Komórka stanu wczytywania.
fn load_cell(ui: &mut egui::Ui, g: &Entry, limit: u64) {
    match &g.load {
        Load::Idle => match (g.profile, g.size) {
            // Bez profilu gra większa niż limit wczyta się tylko częściowo.
            (None, Some(size)) if size > limit && limit > 0 => ui.label(
                RichText::new(crate::trf!("zmieści się {} z {}", "{} of {} will fit", gb(limit), gb(size)))
                    .size(12.0)
                    .color(theme::muted()),
            ),
            _ => ui.label(""),
        },
        Load::Queued => ui.label(RichText::new(tr("w kolejce", "queued")).color(theme::muted())),
        Load::Running { done, total } => ui.add(
            egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32)
                .desired_width(170.0)
                .text(format!("{} / {}", gb(*done), gb(*total))),
        ),
        Load::Done { bytes, secs, whole, pinned, profiled } => {
            let part = match (*whole, *profiled) {
                (false, _) => tr(" (część)", " (part)"),
                (true, true) => tr(" (z profilu)", " (from profile)"),
                (true, false) => "",
            };
            let text = if *pinned {
                crate::trf!("trzymane w pamięci: {}{part}", "held in memory: {}{part}", gb(*bytes))
            } else {
                crate::trf!("w pamięci: {} w {:.0} s{part}", "in memory: {} in {:.0} s{part}", gb(*bytes), secs)
            };
            ui.label(RichText::new(text).color(theme::ok()))
        }
        Load::Stopped => ui.label(RichText::new(tr("przerwano", "stopped")).color(theme::muted())),
        Load::Failed(e) => ui.label(RichText::new(e).color(theme::warn())),
    };
}

impl FastLoad {
    /// Komórka profilu: co gra faktycznie czyta, nauka i usuwanie profilu.
    fn profile_cell(&self, ui: &mut egui::Ui, g: &Entry, elevated: bool, learning_other: bool) {
        ui.horizontal(|ui| {
            if let Some(chunks) = g.learning {
                ui.spinner();
                ui.label(RichText::new(crate::trf!("zapis: {} MB", "recording: {} MB", chunks)).color(theme::accent()));
                if ui
                    .small_button(tr("Zakończ", "Finish"))
                    .on_hover_text(tr(
                        "Kończy zapis i dolicza sesję do profilu. Zapis kończy się też sam po zamknięciu gry.",
                        "Ends the recording and adds the session to the profile. Recording also ends by itself when the game closes.",
                    ))
                    .clicked()
                {
                    self.finish_learn();
                }
                return;
            }
            match g.profile {
                Some((bytes, sessions)) => ui.label(format!("{} / {}", gb(bytes), sessions)).on_hover_text(crate::trf!(
                    "Profil z {} sesji: gra czytała {}. Wczytywane będą tylko te dane.",
                    "Profile from {} sessions: the game read {}. Only this data will be loaded.",
                    sessions,
                    gb(bytes)
                )),
                None => ui
                    .label(RichText::new(tr("cała gra", "whole game")).color(theme::muted()))
                    .on_hover_text(tr("Brak profilu - wczytywana jest cała gra.", "No profile - the whole game is loaded.")),
            };
            let hover = if elevated {
                tr(
                    "Zapisuje, które dane gra czyta: kliknij, uruchom grę, wczytaj poziom i zagraj chwilę, potem zamknij grę. Kolejne sesje uzupełniają profil. Najlepiej robić to przed wczytaniem gry do RAM.",
                    "Records which data the game reads: click, start the game, load a level and play a little, then close the game. Further sessions extend the profile. Best done before loading the game into RAM.",
                )
            } else {
                tr(
                    "Zapis odczytów gry wymaga uprawnień administratora (przycisk na dole okna).",
                    "Recording the game's reads needs administrator rights (button at the bottom of the window).",
                )
            };
            if ui
                .add_enabled(elevated && !learning_other, egui::Button::new(tr("Ucz", "Learn")).small())
                .on_hover_text(hover)
                .on_disabled_hover_text(hover)
                .clicked()
            {
                self.learn(&g.id);
            }
            if g.profile.is_some()
                && ui
                    .small_button(tr("Usuń", "Forget"))
                    .on_hover_text(tr(
                        "Usuwa profil - gra znów będzie wczytywana cała.",
                        "Deletes the profile - the whole game will be loaded again.",
                    ))
                    .clicked()
            {
                self.forget(&g.id);
            }
        });
    }

    /// Rysuje stronę. `auto` - gry wczytywane po starcie programu, `pin` -
    /// opcja "Trzymaj w pamięci"; zwraca `true`, gdy któreś się zmieniło (do
    /// zapisania w ustawieniach). Nauka profilu wymaga administratora.
    pub fn ui(&mut self, ui: &mut egui::Ui, auto: &mut Vec<String>, pin: &mut bool, elevated: bool) -> bool {
        self.ensure_scanned();
        let snap = self.snapshot();
        let limit = budget(snap.ram_total, snap.ram_avail);
        let busy = snap.games.iter().any(|g| matches!(g.load, Load::Queued | Load::Running { .. }));
        let learning_other = snap.games.iter().any(|g| g.learning.is_some());
        let mut changed = false;

        theme::heading(ui, Icon::Memory, "Fast Load");
        theme::info(
            ui,
            tr(
                "Wczytuje pliki gry do wolnej pamięci RAM, zanim ją uruchomisz - gra dostaje je potem z pamięci zamiast z dysku, więc poziomy ładują się szybciej. Pliki gry nie są zmieniane i nic nie jest do niej wstrzykiwane. Największy zysk jest na dyskach SATA i HDD; na szybkim NVMe różnica jest mała. Przycisk Ucz zapisuje, które dane gra faktycznie czyta - potem wczytywane są tylko one. Kolumna DirectStorage pokazuje, czy gra używa tej technologii i czy dysk pozwala z niej skorzystać - DirectStorage nie da się dodać do gry z zewnątrz.",
                "Reads the game's files into free RAM before you start it - the game then gets them from memory instead of the disk, so levels load faster. The game's files are not changed and nothing is injected into it. The gain is largest on SATA drives and HDDs; on a fast NVMe drive the difference is small. The Learn button records which data the game actually reads - afterwards only that is loaded. The DirectStorage column shows whether the game uses that technology and whether the drive lets it benefit - DirectStorage cannot be added to a game from outside.",
            ),
        );
        ui.label(
            RichText::new(crate::trf!(
                "Pamięć RAM: {}, dostępne {} - jednorazowo do wczytania najwyżej {}.",
                "RAM: {}, available {} - at most {} can be loaded at once.",
                gb(snap.ram_total),
                gb(snap.ram_avail),
                gb(limit)
            ))
            .color(theme::muted()),
        );
        if snap.ram_total < MIN_RAM {
            ui.label(
                RichText::new(tr(
                    "Ten komputer ma mniej niż 32 GB RAM - wczytane dane szybko ustąpią miejsca grze, a automatyczne wczytywanie jest wyłączone.",
                    "This computer has less than 32 GB of RAM - loaded data will quickly make room for the game, and automatic loading is off.",
                ))
                .color(theme::warn()),
            );
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let refresh = tr("Odśwież listę gier", "Refresh game list");
            if theme::primary_button(ui, Some(Icon::Refresh), refresh, !snap.scanning && !busy).clicked() {
                self.rescan();
            }
            if theme::primary_button(ui, None, tr("Zatrzymaj wczytywanie", "Stop loading"), busy).clicked() {
                self.stop();
            }
            if ui
                .checkbox(pin, tr("Trzymaj w pamięci", "Keep in memory"))
                .on_hover_text(tr(
                    "Po wczytaniu Tune Center trzyma dane gry we własnej pamięci, więc Windows nie usuwa ich przy zwykłej pracy. Dane są zwalniane przyciskiem Zwolnij, po zamknięciu gry, przy zamknięciu Tune Center albo gdy systemowi zaczyna brakować pamięci. Trzymana gra nie zaktualizuje się, dopóki jej nie zwolnisz. Nie dotyczy wczytywania po starcie.",
                    "After loading, Tune Center holds the game's data in its own memory, so Windows does not drop it during normal work. The data is released with the Release button, when the game closes, when Tune Center exits or when the system starts running out of memory. A held game cannot update until you release it. Does not apply to loading at startup.",
                ))
                .changed()
            {
                changed = true;
            }
            if snap.scanning {
                ui.spinner();
                ui.label(
                    RichText::new(tr("Szukanie gier i liczenie rozmiarów...", "Finding games and measuring sizes..."))
                        .color(theme::muted()),
                );
            }
        });
        if let Some(msg) = &snap.message {
            ui.label(RichText::new(msg).color(theme::warn()));
        }
        ui.add_space(8.0);

        if snap.scanned && snap.games.is_empty() {
            ui.label(
                RichText::new(tr("Nie znaleziono zainstalowanych gier.", "No installed games found."))
                    .color(theme::muted()),
            );
            return changed;
        }

        egui::ScrollArea::both().id_salt("fastload_scroll").auto_shrink([false, false]).show(ui, |ui| {
            // Nazwy gier w jednym wierszu - kolumna dopasowuje się do najdłuższej.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            egui::Grid::new("fastload_games").striped(true).num_columns(8).spacing([18.0, 8.0]).show(ui, |ui| {
                for title in [
                    tr("Gra", "Game"),
                    tr("Rozmiar", "Size"),
                    tr("Dysk", "Drive"),
                    "DirectStorage",
                    tr("Profil", "Profile"),
                    "Fast Load",
                    "",
                    tr("Po starcie", "At startup"),
                ] {
                    ui.label(RichText::new(title).strong().color(theme::accent()));
                }
                ui.end_row();

                for g in &snap.games {
                    // Gra i sklep.
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&g.name).strong());
                        ui.label(RichText::new(g.store).size(11.5).color(theme::muted()));
                    })
                    .response
                    .on_hover_text(g.dir.display().to_string());

                    // Rozmiar danych do wczytania.
                    match g.size {
                        Some(size) => ui.label(gb(size)),
                        None => ui.label(RichText::new("...").color(theme::muted())),
                    };

                    // Dysk i BypassIO.
                    let drive_color = match g.drive.kind {
                        DriveKind::Nvme => theme::ok(),
                        DriveKind::Ssd | DriveKind::Unknown => theme::text(),
                        DriveKind::Hdd | DriveKind::Usb => theme::warn(),
                    };
                    ui.label(RichText::new(format!("{} {}", g.drive.letter, g.drive.kind.label())).color(drive_color))
                        .on_hover_text(bypass_text(&g.drive.bypass));

                    directstorage_cell(ui, g);
                    self.profile_cell(ui, g, elevated, learning_other);
                    load_cell(ui, g, limit);

                    // Przycisk wczytania albo zwolnienia trzymanych danych.
                    if matches!(g.load, Load::Done { pinned: true, .. }) {
                        if ui
                            .button(tr("Zwolnij", "Release"))
                            .on_hover_text(tr(
                                "Zwalnia dane gry trzymane w pamięci (zostają w pamięci podręcznej Windows, dopóki jest miejsce).",
                                "Releases the game data held in memory (it stays in the Windows cache while there is room).",
                            ))
                            .clicked()
                        {
                            self.release(&g.id);
                        }
                    } else {
                        let can = g.size.is_some_and(|s| s > 0)
                            && limit > 0
                            && !matches!(g.load, Load::Queued | Load::Running { .. });
                        if ui
                            .add_enabled(can, egui::Button::new(tr("Wczytaj do RAM", "Load into RAM")))
                            .on_hover_text(tr(
                                "Czyta dane gry do pamięci podręcznej Windows: z profilu tylko to, co gra czytała, bez profilu całą grę. Uruchom grę po zakończeniu.",
                                "Reads the game's data into the Windows cache: with a profile only what the game read, without one the whole game. Start the game when it finishes.",
                            ))
                            .clicked()
                        {
                            self.load(&g.id, *pin);
                        }
                    }

                    // Automatycznie po starcie programu.
                    let mut on = auto.contains(&g.id);
                    if ui
                        .add_enabled(snap.ram_total >= MIN_RAM, egui::Checkbox::without_text(&mut on))
                        .on_hover_text(tr(
                            "Wczytuje tę grę do pamięci 2 minuty po starcie Tune Center (np. po włączeniu komputera).",
                            "Loads this game into memory 2 minutes after Tune Center starts (e.g. after turning the computer on).",
                        ))
                        .changed()
                    {
                        if on {
                            auto.push(g.id.clone());
                        } else {
                            auto.retain(|id| id != &g.id);
                        }
                        changed = true;
                    }
                    ui.end_row();
                }
            });
        });
        changed
    }
}
