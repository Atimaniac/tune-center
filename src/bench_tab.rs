//! Zakładka BENCH: wynik jednego i wszystkich wątków (paski jak w CPU-Z),
//! stress test z wykresem temperatury i mocy oraz historia wyników.

use std::time::Duration;

use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense};

use crate::bench::{self, Phase, Record, Stop};
use crate::i18n::tr;
use crate::icons::Icon;
use crate::theme::{self, accent, muted, warn};

/// Czas stress testu do wyboru (minuty, 0 = do zatrzymania).
const DURATIONS: [u64; 5] = [5, 15, 30, 60, 0];
/// Limity temperatury procesora (°C).
const TEMP_LIMITS: [f32; 3] = [85.0, 90.0, 95.0];
/// Ile wyników trzyma historia.
const HISTORY: usize = 10;

pub struct BenchTab {
    bench: bench::Bench,
    duration: usize,
    temp_limit: usize,
    /// Wynik już zapisany do historii (żeby nie dopisywać co klatkę).
    saved: bool,
    cpu: crate::cpu::CpuInfo,
}

impl BenchTab {
    pub fn new() -> Self {
        Self {
            bench: bench::Bench::new(),
            duration: 1,
            temp_limit: 2,
            saved: true,
            cpu: crate::cpu::cpu_info(),
        }
    }

    /// Co sekundę z `App::logic`: odczyty do limitu temperatury i wykresu.
    pub fn observe(&mut self, temp: Option<f32>, watts: Option<f32>) {
        self.bench.observe(temp, watts);
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        history: &mut Vec<Record>,
        temp_now: Option<f32>,
    ) -> bool {
        let state = self.bench.state();
        let mut changed = false;
        if state.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        // Zakończony test wydajności trafia do historii (raz).
        if let (Phase::BenchDone, Some(s), Some(m), false) = (
            &state.phase,
            state.scores.single,
            state.scores.multi,
            self.saved,
        ) {
            history.insert(
                0,
                Record {
                    when: bench::now_text(),
                    single: s,
                    multi: m,
                    threads: state.threads,
                },
            );
            history.truncate(HISTORY);
            self.saved = true;
            changed = true;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    theme::heading(ui, Icon::Bench, "BENCH");
                    theme::vsep(ui, 28.0);
                    ui.label(
                        RichText::new(&self.cpu.name)
                            .size(16.0)
                            .color(theme::text()),
                    );
                    ui.label(
                        RichText::new(crate::trf!(
                            "{} wątków{}",
                            "{} threads{}",
                            bench::threads(),
                            if self.cpu.hybrid {
                                tr(", rdzenie P + E", ", P + E cores")
                            } else {
                                ""
                            }
                        ))
                        .color(muted()),
                    );
                });
                ui.add_space(10.0);

                let full = ui.available_width();
                let two = full >= 980.0;
                let w = if two { (full - 18.0) / 2.0 } else { full } - 2.0 * 16.0 - 4.0;
                if two {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 18.0;
                        ui.vertical(|ui| self.bench_card(ui, &state, history, w));
                        ui.vertical(|ui| self.stress_card(ui, &state, temp_now, w));
                    });
                } else {
                    self.bench_card(ui, &state, history, w);
                    ui.add_space(12.0);
                    self.stress_card(ui, &state, temp_now, w);
                }
                ui.add_space(12.0);
                if history_card(ui, history, full - 2.0 * 16.0 - 4.0) {
                    changed = true;
                }
            });
        changed
    }

    fn bench_card(&mut self, ui: &mut egui::Ui, state: &bench::State, history: &[Record], w: f32) {
        theme::card(10, 16).show(ui, |ui| {
            ui.set_width(w);
            theme::subheading(ui, Icon::Cpu, tr("Wydajność procesora", "CPU performance"));
            ui.label(
                RichText::new(tr(
                    "Po 10 s na jednym wątku (rdzenie P) i na wszystkich wątkach. Skala własna - porównuj wyniki na tym komputerze, np. przed i po zmianie ustawień w CPU.",
                    "10 s on one thread (P-cores) and on all threads. Own scale - compare results on this PC, e.g. before and after changing CPU settings.",
                ))
                .size(12.0)
                .color(muted()),
            );
            ui.add_space(10.0);
            let best_single = history.iter().map(|r| r.single).fold(0.0, f64::max);
            let best_multi = history.iter().map(|r| r.multi).fold(0.0, f64::max);
            let (p_single, p_multi) = match state.phase {
                Phase::Bench { multi: false, progress } => (Some(progress), None),
                Phase::Bench { multi: true, progress } => (None, Some(progress)),
                _ => (None, None),
            };
            score_row(ui, tr("Jeden wątek", "Single thread"), state.scores.single, best_single, p_single, w);
            ui.add_space(8.0);
            score_row(ui, tr("Wszystkie wątki", "Multi thread"), state.scores.multi, best_multi, p_multi, w);
            if let (Some(s), Some(m)) = (state.scores.single, state.scores.multi) {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(crate::trf!(
                        "Wielowątkowo {:.1}× szybciej niż jeden wątek",
                        "Multi thread {:.1}× faster than single thread",
                        m / s
                    ))
                    .size(12.0)
                    .color(muted()),
                );
            }
            ui.add_space(10.0);
            let bench_running = matches!(state.phase, Phase::Bench { .. });
            ui.horizontal(|ui| {
                if bench_running {
                    if theme::primary_button(ui, None, tr("Zatrzymaj", "Stop"), true).clicked() {
                        self.bench.stop();
                    }
                } else if theme::primary_button(
                    ui,
                    Some(Icon::Bench),
                    tr("Uruchom test", "Run benchmark"),
                    !state.running(),
                )
                .clicked()
                {
                    self.saved = false;
                    let ctx = ui.ctx().clone();
                    self.bench.start_bench(move || ctx.request_repaint());
                }
                if bench_running {
                    ui.label(RichText::new(tr("Nie używaj komputera w trakcie testu.", "Don't use the PC during the test.")).size(12.0).color(warn()));
                }
            });
        });
    }

    fn stress_card(
        &mut self,
        ui: &mut egui::Ui,
        state: &bench::State,
        temp_now: Option<f32>,
        w: f32,
    ) {
        theme::card(10, 16).show(ui, |ui| {
            ui.set_width(w);
            theme::subheading(ui, Icon::Thermo, "Stress test");
            ui.label(
                RichText::new(crate::trf!(
                    "Pełne obciążenie wszystkich wątków{}. Każdy wynik jest sprawdzany - błąd obliczeń oznacza niestabilne ustawienia (np. za mocny undervolting).",
                    "Full load on all threads{}. Every result is verified - a calculation error means unstable settings (e.g. too much undervolting).",
                    if state.avx2 || is_avx2() { " (AVX2 + FMA)" } else { "" }
                ))
                .size(12.0)
                .color(muted()),
            );
            ui.add_space(8.0);
            let stress = matches!(state.phase, Phase::Stress);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Czas:", "Duration:")).color(muted()));
                let label = |m: u64| {
                    if m == 0 {
                        tr("do zatrzymania", "until stopped").to_owned()
                    } else {
                        crate::trf!("{m} min", "{m} min")
                    }
                };
                ui.add_enabled_ui(!stress, |ui| {
                    egui::ComboBox::from_id_salt("stress_time")
                        .width(130.0)
                        .selected_text(label(DURATIONS[self.duration]))
                        .show_ui(ui, |ui| {
                            for (i, &m) in DURATIONS.iter().enumerate() {
                                ui.selectable_value(&mut self.duration, i, label(m));
                            }
                        });
                    ui.label(RichText::new(tr("Stop przy:", "Stop at:")).color(muted()));
                    egui::ComboBox::from_id_salt("stress_temp")
                        .width(80.0)
                        .selected_text(format!("{:.0} °C", TEMP_LIMITS[self.temp_limit]))
                        .show_ui(ui, |ui| {
                            for (i, &t) in TEMP_LIMITS.iter().enumerate() {
                                ui.selectable_value(&mut self.temp_limit, i, format!("{t:.0} °C"));
                            }
                        });
                });
            });
            ui.add_space(8.0);

            // Odczyty.
            let elapsed = state.elapsed().as_secs();
            let now_w = state.samples.last().and_then(|s| s.watts);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 22.0;
                stat(ui, tr("Czas", "Time"), format!("{:02}:{:02}", elapsed / 60, elapsed % 60), theme::text());
                stat(
                    ui,
                    tr("Temperatura", "Temperature"),
                    temp_now.map_or("-".into(), |t| format!("{t:.0} °C")),
                    temp_color(temp_now, TEMP_LIMITS[self.temp_limit]),
                );
                stat(ui, tr("Maks.", "Max"), state.max_temp.map_or("-".into(), |t| format!("{t:.0} °C")), theme::text());
                stat(ui, tr("Moc CPU", "CPU power"), now_w.map_or("-".into(), |w| format!("{w:.0} W")), theme::text());
                stat(ui, tr("Maks. moc", "Max power"), state.max_watts.map_or("-".into(), |w| format!("{w:.0} W")), theme::text());
                let (text, color) = if state.started.is_none() {
                    ("-".to_owned(), theme::text())
                } else if state.errors == 0 {
                    ("0".to_owned(), theme::ok())
                } else {
                    (state.errors.to_string(), theme::hot())
                };
                stat(ui, tr("Błędy obliczeń", "Calculation errors"), text, color);
            });
            ui.add_space(6.0);
            let temps: Vec<Option<f32>> = state.samples.iter().map(|s| s.temp).collect();
            let watts: Vec<Option<f32>> = state.samples.iter().map(|s| s.watts).collect();
            let cw = (w - 12.0) / 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                crate::history::chart(ui, vec2(cw, 70.0), &temps, theme::hot(), crate::history::Scale::Around(10.0), &|v| format!("{v:.0} °C"));
                crate::history::chart(ui, vec2(cw, 70.0), &watts, accent(), crate::history::Scale::FromZero(50.0), &|v| format!("{v:.0} W"));
            });
            ui.add_space(8.0);

            if let Phase::StressDone(stop) = &state.phase {
                let (text, color) = match stop {
                    Stop::Temp(t) => (
                        crate::trf!("Zatrzymano: temperatura {t:.0} °C osiągnęła limit.", "Stopped: temperature {t:.0} °C reached the limit."),
                        warn(),
                    ),
                    Stop::Time => (tr("Zakończono po ustawionym czasie.", "Finished after the set time.").to_owned(), accent()),
                    Stop::User => (tr("Zatrzymano.", "Stopped.").to_owned(), muted()),
                };
                let verdict = if state.errors > 0 {
                    crate::trf!(" Błędy obliczeń: {} - ustawienia są niestabilne.", " Calculation errors: {} - the settings are unstable.", state.errors)
                } else {
                    tr(" Bez błędów obliczeń.", " No calculation errors.").to_owned()
                };
                ui.label(RichText::new(text + &verdict).color(if state.errors > 0 { theme::hot() } else { color }));
                ui.add_space(4.0);
            }
            if temp_now.is_none() {
                ui.label(
                    RichText::new(tr(
                        "Brak odczytu temperatury procesora (potrzebny proces czujników i administrator) - limit temperatury nie zadziała.",
                        "No CPU temperature reading (needs the sensor process and administrator rights) - the temperature limit will not work.",
                    ))
                    .size(12.0)
                    .color(warn()),
                );
            }
            ui.horizontal(|ui| {
                if stress {
                    if theme::primary_button(ui, None, tr("Zatrzymaj", "Stop"), true).clicked() {
                        self.bench.stop();
                    }
                } else if theme::primary_button(ui, Some(Icon::Thermo), tr("Start", "Start"), !state.running()).clicked() {
                    let minutes = DURATIONS[self.duration];
                    let limit = (minutes > 0).then(|| Duration::from_secs(minutes * 60));
                    let ctx = ui.ctx().clone();
                    self.bench.start_stress(limit, TEMP_LIMITS[self.temp_limit], move || ctx.request_repaint());
                }
            });
        });
    }
}

fn is_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    false
}

fn temp_color(t: Option<f32>, limit: f32) -> Color32 {
    match t {
        Some(t) if t >= limit - 5.0 => theme::hot(),
        Some(t) if t >= 80.0 => warn(),
        _ => theme::text(),
    }
}

fn stat(ui: &mut egui::Ui, title: &str, value: String, color: Color32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(title).size(11.5).color(muted()));
        ui.label(RichText::new(value).size(20.0).strong().color(color));
    });
}

/// Wiersz wyniku: nazwa, pasek (wynik względem najlepszego w historii albo postęp), liczba.
fn score_row(
    ui: &mut egui::Ui,
    title: &str,
    score: Option<f64>,
    best: f64,
    progress: Option<f32>,
    w: f32,
) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [120.0, 22.0],
            egui::Label::new(RichText::new(title).color(theme::text())),
        );
        let bar_w = (w - 120.0 - 110.0).max(80.0);
        let (rect, _) = ui.allocate_exact_size(vec2(bar_w, 18.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(4), theme::track());
        let (fraction, color) = match (progress, score) {
            (Some(pr), _) => (pr, theme::glow().gamma_multiply(0.6)),
            (None, Some(s)) => ((s / best.max(s)) as f32, accent()),
            _ => (0.0, accent()),
        };
        if fraction > 0.0 {
            let mut fill = rect;
            fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
            p.rect_filled(fill, CornerRadius::same(4), color);
        }
        let text = match (progress, score) {
            (Some(pr), _) => format!("{:.0} %", pr * 100.0),
            (None, Some(s)) => format!("{s:.0}"),
            _ => "-".into(),
        };
        ui.add_sized(
            [100.0, 22.0],
            egui::Label::new(RichText::new(text).size(20.0).strong().color(theme::text())),
        );
    });
}

/// Historia wyników; zwraca `true`, gdy wyczyszczono.
fn history_card(ui: &mut egui::Ui, history: &mut Vec<Record>, w: f32) -> bool {
    let mut cleared = false;
    theme::card(10, 16).show(ui, |ui| {
        ui.set_width(w);
        ui.horizontal(|ui| {
            theme::subheading(ui, Icon::Sensors, tr("Historia wyników", "Result history"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !history.is_empty() && ui.button(tr("Wyczyść", "Clear")).clicked() {
                    history.clear();
                    cleared = true;
                }
            });
        });
        if history.is_empty() {
            ui.label(
                RichText::new(tr(
                    "Brak wyników - uruchom test.",
                    "No results yet - run the benchmark.",
                ))
                .color(muted()),
            );
            return;
        }
        let best_s = history.iter().map(|r| r.single).fold(0.0, f64::max);
        let best_m = history.iter().map(|r| r.multi).fold(0.0, f64::max);
        egui::Grid::new("bench_history")
            .num_columns(4)
            .spacing([36.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                for h in [
                    tr("Data", "Date"),
                    tr("Jeden wątek", "Single thread"),
                    tr("Wszystkie wątki", "Multi thread"),
                    tr("Wątki", "Threads"),
                ] {
                    ui.label(RichText::new(h).size(12.0).color(muted()));
                }
                ui.end_row();
                for r in history.iter() {
                    ui.label(&r.when);
                    let mark = |v: f64, best: f64| {
                        RichText::new(format!("{v:.0}")).color(if v >= best {
                            theme::ok()
                        } else {
                            theme::text()
                        })
                    };
                    ui.label(mark(r.single, best_s));
                    ui.label(mark(r.multi, best_m));
                    ui.label(r.threads.to_string());
                    ui.end_row();
                }
            });
    });
    cleared
}
