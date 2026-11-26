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
    /// Karta z rdzeniami obsługującymi kartę graficzną i dźwięk oraz
    /// przeniesieniem dźwięku na inny rdzeń. `backups` - kopie stanu sprzed
    /// zmian (z ustawień programu); zwraca `true`, gdy lista się zmieniła.
    fn irq_card(&mut self, ui: &mut egui::Ui, elevated: bool, backups: &mut Vec<crate::irq::Backup>) -> bool {
        use crate::irq::{gpu_core, pick_target, shared, Role};
        let ctx = ui.ctx().clone();
        self.irq.ensure_scanned(elevated, move || ctx.request_repaint());
        let measured = self.irq.measured();
        let gpu = gpu_core(&self.irq.devices, measured);
        let mut changed = false;
        let mut rescan = false;
        let mut pin: Option<(String, u16)> = None;
        let mut undo = false;

        theme::card(8, 10).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(tr("Rdzenie przerwań: karta graficzna i dźwięk", "Interrupt cores: graphics card and audio")).strong())
                    .on_hover_text(tr(
                        "Sterownik karty graficznej potrafi zająć rdzeń procesora na ułamek milisekundy. Jeśli ten sam rdzeń obsługuje przerwania dźwięku (kontroler HD Audio albo kontroler USB, do którego podłączony jest DAC), dźwięk czeka w kolejce i słychać trzaski. Na różnych rdzeniach sobie nie przeszkadzają.",
                        "The graphics driver can hold a processor core for a fraction of a millisecond. If the same core serves audio interrupts (the HD Audio controller or the USB controller your DAC is plugged into), audio waits in the queue and crackles. On different cores they do not disturb each other.",
                    ));
                if self.irq.devices.is_empty() {
                    ui.label(RichText::new(tr("Nie znaleziono urządzeń.", "No devices found.")).color(theme::muted()));
                    return;
                }
                ui.add_space(4.0);
                egui::Grid::new("irq_devices").num_columns(3).spacing([22.0, 5.0]).show(ui, |ui| {
                    for d in &self.irq.devices {
                        let kind = match d.role {
                            Role::Gpu => tr("Karta graficzna", "Graphics card"),
                            Role::Audio => tr("Dźwięk", "Audio"),
                        };
                        ui.label(RichText::new(kind).color(theme::accent()));
                        let via = if d.role == Role::Audio && d.controller != d.name {
                            crate::trf!("{} (przez: {})", "{} (through: {})", d.name, d.controller)
                        } else {
                            d.name.clone()
                        };
                        ui.label(via);
                        // Rdzeń: zmierzony (karta), przypięty albo z przydziału Windows.
                        let (text, hover) = match (d.role, measured, d.pinned, d.core()) {
                            (Role::Gpu, Some(m), ..) => (
                                crate::trf!("rdzeń {m} (zmierzony)", "core {m} (measured)"),
                                tr("Rdzeń, na którym przez 2 s wykonywały się DPC sterownika karty.", "The core on which the driver's DPCs ran during a 2 s trace.").to_owned(),
                            ),
                            (_, _, Some(p), _) => (
                                crate::trf!("rdzeń {p} (przypięty)", "core {p} (pinned)"),
                                tr("Rdzeń ustawiony wpisem w rejestrze urządzenia.", "The core set by an entry in the device's registry key.").to_owned(),
                            ),
                            (_, _, None, Some(c)) if d.exact() => (
                                crate::trf!("rdzeń {c}", "core {c}"),
                                tr("Rdzeń przydzielony przez Windows.", "The core assigned by Windows.").to_owned(),
                            ),
                            (_, _, None, Some(c)) => (
                                crate::trf!("rdzeń {c} (prawdopodobnie)", "core {c} (probably)"),
                                crate::trf!(
                                    "Windows przydzielił zbiór rdzeni {:?} i zwykle używa pierwszego. Pewny wynik daje pomiar (wymaga administratora).",
                                    "Windows assigned the core set {:?} and usually uses the first one. A measurement gives a sure result (needs administrator rights).",
                                    d.cores
                                ),
                            ),
                            _ => (tr("nieznany", "unknown").to_owned(), String::new()),
                        };
                        ui.label(RichText::new(text).strong()).on_hover_text(hover);
                        ui.end_row();
                    }
                });
                ui.add_space(4.0);

                let audio = self.irq.devices.iter().filter(|d| d.role == Role::Audio).count();
                let clash = shared(&self.irq.devices, gpu);
                if audio == 0 {
                    ui.label(RichText::new(tr("Nie znaleziono włączonego urządzenia dźwiękowego.", "No enabled audio device found.")).color(theme::muted()));
                } else if let Some(first) = clash.first() {
                    let core = gpu.unwrap_or(0);
                    ui.label(
                        RichText::new(crate::trf!(
                            "Dźwięk ({}) dzieli rdzeń {core} z kartą graficzną.",
                            "Audio ({}) shares core {core} with the graphics card.",
                            first.name
                        ))
                        .color(theme::warn()),
                    );
                    let used: Vec<u16> = self.irq.devices.iter().filter_map(|d| d.pinned).collect();
                    let cpus = std::thread::available_parallelism().map(|n| n.get() as u16).unwrap_or(1);
                    if let Some(target) = pick_target(gpu, &used, cpus) {
                        let label = crate::trf!("Przenieś dźwięk na rdzeń {target}", "Move audio to core {target}");
                        if theme::primary_button(ui, Some(Icon::Swap), &label, elevated)
                            .on_hover_text(tr(
                                "Zapisuje przypisanie w rejestrze urządzenia i restartuje kontroler dźwięku. Jeśli to kontroler USB, podłączone do niego urządzenia (także mysz i klawiatura) rozłączą się na kilka sekund. Poprzedni stan jest zapamiętany - przycisk Cofnij go przywraca. Wymaga administratora.",
                                "Writes the assignment to the device's registry key and restarts the audio controller. If it is a USB controller, devices plugged into it (mouse and keyboard included) disconnect for a few seconds. The previous state is remembered - the Undo button restores it. Needs administrator rights.",
                            ))
                            .clicked()
                        {
                            pin = Some((first.instance.clone(), target));
                        }
                    }
                } else {
                    let text = if self.irq.devices.iter().all(|d| d.role != Role::Gpu || measured.is_some() || d.exact()) {
                        tr("Karta graficzna i dźwięk są na różnych rdzeniach.", "The graphics card and audio are on different cores.")
                    } else {
                        tr(
                            "Karta graficzna i dźwięk są prawdopodobnie na różnych rdzeniach (pewność daje pomiar z uprawnieniami administratora).",
                            "The graphics card and audio are probably on different cores (a measurement with administrator rights makes it certain).",
                        )
                    };
                    ui.label(RichText::new(text).color(theme::ok()));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Sprawdź ponownie", "Check again")).clicked() {
                        rescan = true;
                    }
                    if !backups.is_empty()
                        && ui
                            .add_enabled(elevated, egui::Button::new(tr("Cofnij przypisanie dźwięku", "Undo the audio assignment")))
                            .on_hover_text(tr(
                                "Przywraca stan sprzed przeniesienia i restartuje kontroler dźwięku.",
                                "Restores the state before the move and restarts the audio controller.",
                            ))
                            .clicked()
                    {
                        undo = true;
                    }
                    if let Some((msg, error)) = &self.irq.message {
                        ui.label(RichText::new(msg).size(12.5).color(if *error { theme::warn() } else { theme::ok() }));
                    }
                });
            });
        });

        if let Some((instance, target)) = pin {
            match crate::irq::pin(&instance, target) {
                Ok(backup) => {
                    backups.push(backup);
                    self.irq.message = Some((crate::trf!("Dźwięk przeniesiony na rdzeń {target}.", "Audio moved to core {target}."), false));
                    changed = true;
                }
                Err((backup, e)) => {
                    if let Some(b) = backup {
                        backups.push(b);
                        changed = true;
                    }
                    self.irq.message = Some((e, true));
                }
            }
            rescan = true;
        }
        if undo {
            let mut failed = None;
            for b in backups.drain(..).rev() {
                if let Err(e) = crate::irq::restore(&b) {
                    failed = Some(e);
                }
            }
            self.irq.message = Some(match failed {
                Some(e) => (e, true),
                None => (tr("Przywrócono poprzednie przypisanie.", "The previous assignment is restored.").to_owned(), false),
            });
            changed = true;
            rescan = true;
        }
        if rescan {
            let ctx = ui.ctx().clone();
            self.irq.rescan(elevated, move || ctx.request_repaint());
        }
        changed
    }

    /// Strona DPC. Zwraca `true`, gdy zmieniły się `backups` (do zapisania).
    pub fn ui(&mut self, ui: &mut egui::Ui, elevated: bool, backups: &mut Vec<crate::irq::Backup>) -> bool {
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
        let changed = self.irq_card(ui, elevated, backups);
        ui.add_space(8.0);
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
        changed
    }
}
