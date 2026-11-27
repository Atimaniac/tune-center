//! Strona FPS w zakładce TUNE: pomiar klatek gry (średnia, 1% low, 0,1% low)
//! od przycisku Start do Stop i historia pomiarów.

use eframe::egui::{self, RichText};

use crate::fps::{Meter, Run};
use crate::i18n::tr;
use crate::icons::Icon;
use crate::theme;

fn fps(v: f32) -> String {
    if v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn duration(secs: f32) -> String {
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Kafelek z jedną liczbą.
fn tile(ui: &mut egui::Ui, title: &str, value: String, color: egui::Color32, hover: &str) {
    theme::card(8, 10)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_min_width(120.0);
                ui.label(RichText::new(title).size(12.0).color(theme::muted()));
                ui.label(RichText::new(value).size(22.0).strong().color(color));
            });
        })
        .response
        .on_hover_text(hover);
}

/// Poprzedni pomiar tej samej gry (starszy wpis na liście).
fn previous<'a>(runs: &'a [Run], index: usize) -> Option<&'a Run> {
    runs[index + 1..].iter().find(|r| r.game == runs[index].game)
}

impl Meter {
    /// `setup` - opis włączonych opcji programu, zapisywany przy pomiarze.
    pub fn ui(&mut self, ui: &mut egui::Ui, elevated: bool, setup: &str) {
        theme::heading(ui, Icon::Bench, "FPS");
        theme::info(
            ui,
            tr(
                "Mierzy klatki gry: średni FPS, 1% low i 0,1% low (średni FPS z najwolniejszego 1 % i 0,1 % klatek - to one decydują o płynności). Kliknij Start, zagraj w tym samym miejscu przez minutę lub dłużej, wróć i kliknij Stop. Zmień jedno ustawienie i zmierz ponownie - historia pokaże różnicę. Pomiar tylko słucha zdarzeń Windows o wyświetlaniu klatek; do gry nic nie jest wstrzykiwane. Mierzona jest uruchomiona gra, a gdy program jej nie rozpozna - program wyświetlający najwięcej klatek.",
                "Measures a game's frames: average FPS, 1% low and 0.1% low (the average FPS of the slowest 1 % and 0.1 % of frames - they decide how smooth it feels). Click Start, play in the same place for a minute or longer, come back and click Stop. Change one setting and measure again - the history shows the difference. The measurement only listens to Windows events about displayed frames; nothing is injected into the game. The running game is measured, and when the program does not recognise it, the program displaying the most frames.",
            ),
        );
        if !elevated {
            ui.label(
                RichText::new(tr(
                    "Pomiar wymaga uprawnień administratora - użyj przycisku \"Uruchom jako administrator\" na dole okna.",
                    "The measurement needs administrator rights - use the \"Run as administrator\" button at the bottom of the window.",
                ))
                .color(theme::warn()),
            );
        }
        ui.add_space(6.0);
        let running = self.running();
        ui.horizontal(|ui| {
            if theme::primary_button(ui, Some(Icon::Bench), "Start", elevated && !running).clicked() {
                self.start();
            }
            if theme::primary_button(ui, None, "Stop", running).clicked() {
                self.stop(setup);
            }
            if running {
                ui.spinner();
                ui.label(RichText::new(tr("Pomiar trwa - przełącz się do gry.", "Measuring - switch to the game.")).color(theme::accent()));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).color(theme::warn()));
            }
        });
        ui.add_space(8.0);

        if self.running() {
            let live = self.live();
            let s = live.stats;
            let name = if live.name.is_empty() { tr("czekam na klatki...", "waiting for frames...").to_owned() } else { live.name.clone() };
            ui.label(RichText::new(crate::trf!("Mierzony program: {name}", "Measured program: {name}")).strong());
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                tile(ui, tr("FPS teraz", "FPS now"), fps(live.fps), theme::accent(), tr("Średnia z ostatniej sekundy.", "Average of the last second."));
                tile(ui, tr("Średni FPS", "Average FPS"), fps(s.avg), theme::text(), tr("Średnia z całego pomiaru.", "Average of the whole measurement."));
                tile(ui, "1% low", fps(s.low1), theme::ok(), tr("Średni FPS z najwolniejszego 1 % klatek.", "Average FPS of the slowest 1 % of frames."));
                tile(ui, "0.1% low", fps(s.low01), theme::ok(), tr("Średni FPS z najwolniejszego 0,1 % klatek.", "Average FPS of the slowest 0.1 % of frames."));
                tile(
                    ui,
                    tr("Zacięcia", "Hitches"),
                    s.hitches.to_string(),
                    if s.hitches > 0 { theme::warn() } else { theme::text() },
                    tr("Klatki dłuższe niż 50 ms.", "Frames longer than 50 ms."),
                );
                tile(ui, tr("Czas", "Time"), duration(live.secs), theme::text(), tr("Czas od kliknięcia Start.", "Time since Start was clicked."));
            });
            ui.add_space(10.0);
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Historia pomiarów", "Measurement history")).strong().color(theme::accent()));
            if !self.history.runs.is_empty() && ui.small_button(tr("Wyczyść", "Clear")).clicked() {
                self.history.runs.clear();
                self.history.save();
            }
        });
        if self.history.runs.is_empty() {
            ui.label(RichText::new(tr("Brak pomiarów.", "No measurements yet.")).color(theme::muted()));
            return;
        }
        let mut remove: Option<usize> = None;
        egui::ScrollArea::both().id_salt("fps_history").auto_shrink([false, false]).show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            egui::Grid::new("fps_runs").striped(true).num_columns(11).spacing([18.0, 6.0]).show(ui, |ui| {
                for title in [
                    tr("Data", "Date"),
                    tr("Gra", "Game"),
                    tr("Czas", "Time"),
                    tr("Średni", "Average"),
                    "1% low",
                    tr("Zmiana 1% low", "1% low change"),
                    "0.1% low",
                    "Min",
                    tr("Zacięcia", "Hitches"),
                    tr("Ustawienia", "Setup"),
                    "",
                ] {
                    ui.label(RichText::new(title).strong().color(theme::accent()));
                }
                ui.end_row();
                let runs = &self.history.runs;
                for (i, r) in runs.iter().enumerate() {
                    ui.label(&r.when);
                    ui.label(RichText::new(&r.game).strong()).on_hover_text(&r.exe);
                    ui.label(duration(r.secs));
                    ui.label(fps(r.stats.avg));
                    ui.label(RichText::new(fps(r.stats.low1)).strong());
                    // Różnica 1% low wobec poprzedniego pomiaru tej samej gry.
                    match previous(runs, i).filter(|p| p.stats.low1 > 0.0) {
                        Some(p) => {
                            let pct = (r.stats.low1 / p.stats.low1 - 1.0) * 100.0;
                            let color = if pct >= 2.0 {
                                theme::ok()
                            } else if pct <= -2.0 {
                                theme::warn()
                            } else {
                                theme::muted()
                            };
                            ui.label(RichText::new(format!("{pct:+.1} %")).color(color)).on_hover_text(crate::trf!(
                                "Wobec pomiaru z {} ({} FPS). Różnice do około 2 % mieszczą się w rozrzucie między przebiegami.",
                                "Against the run from {} ({} FPS). Differences up to about 2 % are within run-to-run variation.",
                                p.when,
                                fps(p.stats.low1)
                            ));
                        }
                        None => {
                            ui.label(RichText::new("-").color(theme::muted()));
                        }
                    }
                    ui.label(fps(r.stats.low01));
                    ui.label(fps(r.stats.min));
                    ui.label(r.stats.hitches.to_string());
                    ui.label(RichText::new(&r.setup).size(12.0).color(theme::muted()));
                    if ui.small_button(tr("Usuń", "Delete")).clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(i) = remove {
            self.history.runs.remove(i);
            self.history.save();
        }
    }
}
