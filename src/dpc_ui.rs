//! Strona DPC w zakładce TUNE: pomiar czasu DPC i ISR sterowników.

use eframe::egui::{self, RichText};

use crate::dpc::{Dpc, Report, Verdict};
use crate::i18n::tr;
use crate::icons::Icon;
use crate::theme;

/// Kolor czasu procedury: do 0,5 ms dobrze, do 1 ms uwaga, powyżej źle.
fn time_color(us: f64) -> egui::Color32 {
    if us >= 1000.0 {
        theme::hot()
    } else if us >= 500.0 {
        theme::warn()
    } else {
        theme::ok()
    }
}

fn us(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.2} ms", v / 1000.0)
    } else {
        format!("{v:.0} µs")
    }
}

/// Kafelek z jedną liczbą i opisem; `previous` - wartość z poprzedniego pomiaru.
fn tile(ui: &mut egui::Ui, title: &str, value: String, color: egui::Color32, previous: Option<String>, hover: &str) {
    theme::card(8, 10)
        .show(ui, |ui| {
            // Kafelki stoją w wierszu, więc ich treść wymaga własnej kolumny.
            ui.vertical(|ui| {
                ui.set_min_width(170.0);
                ui.label(RichText::new(title).size(12.0).color(theme::muted()));
                ui.label(RichText::new(value).size(22.0).strong().color(color));
                let prev = previous.map(|p| crate::trf!("poprzednio: {p}", "before: {p}")).unwrap_or_default();
                ui.label(RichText::new(prev).size(11.5).color(theme::muted()));
            });
        })
        .response
        .on_hover_text(hover);
}

fn summary(ui: &mut egui::Ui, r: &Report, prev: Option<&Report>) {
    let (text, color) = match r.verdict() {
        Verdict::Good => (
            tr(
                "Dobrze: żadna procedura nie blokowała procesora dłużej niż 0,5 ms.",
                "Good: no routine held a processor for longer than 0.5 ms.",
            ),
            theme::ok(),
        ),
        Verdict::Fair => (
            tr(
                "Uwaga: są procedury trwające ponad 0,5 ms - możliwe pojedyncze przycięcia dźwięku lub obrazu.",
                "Caution: some routines took over 0.5 ms - occasional audio or frame hitches are possible.",
            ),
            theme::warn(),
        ),
        Verdict::Bad => (
            tr(
                "Źle: procedury trwają ponad 1 ms - sterownik z góry tabeli powoduje przycięcia.",
                "Bad: routines take over 1 ms - the driver at the top of the table causes stutter.",
            ),
            theme::hot(),
        ),
    };
    ui.label(RichText::new(text).strong().color(color));
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        tile(
            ui,
            tr("Najdłuższe DPC", "Longest DPC"),
            us(r.dpc_max_us),
            time_color(r.dpc_max_us),
            prev.map(|p| us(p.dpc_max_us)),
            tr(
                "Najdłuższa pojedyncza procedura DPC. W tym czasie rdzeń nie obsługuje dźwięku ani gry.",
                "The longest single DPC routine. During it the core serves neither audio nor the game.",
            ),
        );
        tile(
            ui,
            tr("Najdłuższe ISR", "Longest ISR"),
            us(r.isr_max_us),
            time_color(r.isr_max_us),
            prev.map(|p| us(p.isr_max_us)),
            tr("Najdłuższa obsługa przerwania sprzętowego.", "The longest hardware interrupt handler."),
        );
        tile(
            ui,
            tr("Opóźnienie wybudzenia", "Wake-up latency"),
            us(r.wake_max_us),
            time_color(r.wake_max_us / 2.0),
            prev.map(|p| us(p.wake_max_us)),
            &crate::trf!(
                "Największy czas od sygnału do uruchomienia wątku o najwyższym priorytecie (średnio {}). Przybliżenie wskaźnika \"interrupt to process latency\" z LatencyMon.",
                "The longest time from a signal to a top-priority thread running (average {}). An approximation of LatencyMon's \"interrupt to process latency\".",
                us(r.wake_avg_us)
            ),
        );
        tile(
            ui,
            tr("Twarde błędy stron", "Hard page faults"),
            r.hard_faults.to_string(),
            theme::text(),
            prev.map(|p| p.hard_faults.to_string()),
            tr(
                "Odczyty z dysku brakujących stron pamięci w całym systemie. Dużo w trakcie gry oznacza brak pamięci RAM.",
                "Disk reads of missing memory pages across the system. Many during a game means too little RAM.",
            ),
        );
        tile(
            ui,
            tr("Czas w DPC i ISR", "Time in DPC and ISR"),
            format!("{:.2} %", r.busy_pct),
            theme::text(),
            prev.map(|p| format!("{:.2} %", p.busy_pct)),
            &crate::trf!(
                "Udział czasu wszystkich rdzeni spędzony w DPC i ISR. Procedur ponad 1 ms: {}.",
                "Share of all cores' time spent in DPC and ISR. Routines over 1 ms: {}.",
                r.over_1ms
            ),
        );
    });
}

fn table(ui: &mut egui::Ui, r: &Report) {
    egui::ScrollArea::vertical().id_salt("dpc_scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        egui::Grid::new("dpc_drivers").striped(true).num_columns(7).spacing([22.0, 6.0]).show(ui, |ui| {
            for title in [
                tr("Sterownik", "Driver"),
                tr("DPC najdłuższe", "DPC longest"),
                tr("DPC średnie", "DPC average"),
                tr("DPC liczba", "DPC count"),
                tr("ISR najdłuższe", "ISR longest"),
                tr("ISR liczba", "ISR count"),
                tr("Łącznie", "Total"),
            ] {
                ui.label(RichText::new(title).strong().color(theme::accent()));
            }
            ui.end_row();
            for row in r.rows.iter().take(40) {
                ui.label(RichText::new(&row.driver).strong());
                let cell = |ui: &mut egui::Ui, count: u64, value: f64, colored: bool| {
                    if count == 0 {
                        ui.label(RichText::new("-").color(theme::muted()));
                    } else if colored {
                        ui.label(RichText::new(us(value)).color(time_color(value)));
                    } else {
                        ui.label(us(value));
                    }
                };
                cell(ui, row.dpc.count, row.dpc.max_us, true);
                cell(ui, row.dpc.count, row.dpc.avg_us(), false);
                ui.label(row.dpc.count.to_string());
                cell(ui, row.isr.count, row.isr.max_us, true);
                ui.label(row.isr.count.to_string());
                ui.label(format!("{:.1} ms", (row.dpc.total_us + row.isr.total_us) / 1000.0));
                ui.end_row();
            }
        });
    });
}

impl Dpc {
    pub fn ui(&mut self, ui: &mut egui::Ui, elevated: bool) {
        let state = self.state();
        theme::heading(ui, Icon::Gauge, "DPC");
        theme::info(
            ui,
            tr(
                "Mierzy, jak długo sterowniki blokują procesor w procedurach DPC i obsłudze przerwań (ISR) - to one powodują trzaski dźwięku i przycięcia mimo wysokiego FPS. Pomiar tylko obserwuje system (śledzenie ETW Windows), niczego nie zmienia. Zmierz w trakcie typowego obciążenia (gra, muzyka), zmień jedno ustawienie na stronie System i zmierz ponownie - kafelki pokażą poprzedni wynik.",
                "Measures how long drivers hold a processor in DPC routines and interrupt handlers (ISR) - these cause audio crackle and stutter despite high FPS. The measurement only observes the system (Windows ETW tracing) and changes nothing. Measure under a typical load (game, music), change one setting on the System page and measure again - the tiles show the previous result.",
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
        ui.horizontal(|ui| {
            let running = state.running.is_some();
            if theme::primary_button(ui, Some(Icon::Gauge), tr("Zmierz", "Measure"), elevated && !running).clicked() {
                let ctx = ui.ctx().clone();
                self.start(move || ctx.request_repaint());
            }
            if theme::primary_button(ui, None, tr("Zakończ teraz", "Finish now"), running).clicked() {
                self.stop();
            }
            ui.label(tr("Czas:", "Duration:"));
            for secs in [15u64, 30, 60, 300] {
                let label = if secs >= 60 { format!("{} min", secs / 60) } else { format!("{secs} s") };
                if ui.add_enabled(!running, egui::Button::selectable(self.secs == secs, label)).clicked() {
                    self.secs = secs;
                }
            }
            if let Some((started, secs)) = state.running {
                let done = started.elapsed().as_secs_f32();
                ui.add(
                    egui::ProgressBar::new(done / secs.max(1) as f32)
                        .desired_width(220.0)
                        .text(format!("{:.0} / {secs} s", done.min(secs as f32))),
                );
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
        });
        if let Some(e) = &state.error {
            ui.label(RichText::new(e).color(theme::warn()));
        }
        ui.add_space(8.0);
        match &state.report {
            Some(r) => {
                summary(ui, r, state.previous.as_ref());
                ui.add_space(8.0);
                table(ui, r);
            }
            None if state.running.is_none() => {
                ui.label(RichText::new(tr("Brak pomiaru - kliknij Zmierz.", "No measurement yet - click Measure.")).color(theme::muted()));
            }
            None => {}
        }
    }
}
