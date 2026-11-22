//! Zakładka BENCH: wydajność procesora (paski jak w CPU-Z), stress test
//! procesora, test stabilności karty graficznej (2 min, okno z wynikiem
//! i wykresami) oraz historia wyników.
//!
//! Układ: dwa wiersze po dwie karty o równej szerokości i wysokości -
//! procesor (wydajność | stress test), karta (stabilność | historia).
//! Przy wąskim oknie karty idą jedna pod drugą.

use std::time::Duration;

use eframe::egui::{self, vec2, Color32, CornerRadius, RichText, Sense};

use crate::bench::{self, Phase, Record, Stop};
use crate::gpu_bench::{self, GpuBench};
use crate::i18n::tr;
use crate::icons::Icon;
use crate::theme::{self, accent, muted, warn};

/// Czas stress testu do wyboru (minuty, 0 = do zatrzymania).
const DURATIONS: [u64; 5] = [5, 15, 30, 60, 0];
/// Limity temperatury procesora (°C).
const TEMP_LIMITS: [f32; 3] = [85.0, 90.0, 95.0];
/// Ile wyników trzyma historia.
const HISTORY: usize = 10;
/// Wysokość kart w wierszu (równe karty obok siebie).
const ROW1_H: f32 = 300.0;
const ROW2_H: f32 = 330.0;
/// Odstęp między kartami i wewnętrzny margines karty.
const GAP: f32 = 18.0;
const PAD: f32 = 16.0;

pub struct BenchTab {
    bench: bench::Bench,
    gpu: GpuBench,
    duration: usize,
    temp_limit: usize,
    /// Wynik już zapisany do historii (żeby nie dopisywać co klatkę).
    saved: bool,
    gpu_saved: bool,
    /// Okno z wynikiem testu karty (stan z chwili zakończenia).
    popup: Option<gpu_bench::State>,
    /// Wynik ostatniego testu jest pierwszy w historii - porównania z najlepszym
    /// pomijają go (inaczej test porównywałby się sam ze sobą).
    just_saved: bool,
    cpu: crate::cpu::CpuInfo,
}

pub struct Histories<'a> {
    pub cpu: &'a mut Vec<Record>,
    pub gpu: &'a mut Vec<gpu_bench::Record>,
}

impl BenchTab {
    pub fn new() -> Self {
        Self {
            bench: bench::Bench::new(),
            gpu: GpuBench::new(),
            duration: 1,
            temp_limit: 2,
            saved: true,
            gpu_saved: true,
            popup: None,
            just_saved: false,
            cpu: crate::cpu::cpu_info(),
        }
    }

    /// Co sekundę z `App::logic`: odczyty procesora i karty.
    pub fn observe(
        &mut self,
        temp: Option<f32>,
        watts: Option<f32>,
        gpu: gpu_bench::Sample,
        gpu_limit: Option<&str>,
    ) {
        self.bench.observe(temp, watts);
        self.gpu.observe(gpu, gpu_limit);
    }

    /// Start testu karty (argument `--bench-gpu`).
    pub fn start_gpu(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        self.gpu_saved = false;
        self.popup = None;
        self.just_saved = false;
        self.gpu.start(wake);
    }

    /// Zwraca `true`, gdy zmieniła się historia (do zapisania ustawień).
    pub fn ui(&mut self, ui: &mut egui::Ui, mut h: Histories, temp_now: Option<f32>) -> bool {
        let state = self.bench.state();
        let gstate = self.gpu.state();
        let mut changed = false;
        if state.running() || gstate.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        // Zakończony test procesora trafia do historii (raz).
        if let (Phase::BenchDone, Some(s), Some(m), false) = (
            &state.phase,
            state.scores.single,
            state.scores.multi,
            self.saved,
        ) {
            h.cpu.insert(
                0,
                Record {
                    when: bench::now_text(),
                    single: s,
                    multi: m,
                    threads: state.threads,
                },
            );
            h.cpu.truncate(HISTORY);
            self.saved = true;
            changed = true;
        }
        // Zakończony test karty: historia (tylko pełne 2 min) i okno z wynikiem.
        // Przy resecie sterownika (TDR) okno też się pokazuje - to wynik testu.
        if !self.gpu_saved
            && matches!(
                gstate.phase,
                gpu_bench::Phase::Done(_) | gpu_bench::Phase::Failed(_)
            )
        {
            self.gpu_saved = true;
            if gstate.phase == gpu_bench::Phase::Done(true) {
                if let Some(r) = gpu_record(&gstate) {
                    h.gpu.insert(0, r);
                    self.just_saved = true;
                    h.gpu.truncate(HISTORY);
                    changed = true;
                }
            }
            self.popup = Some(gstate.clone());
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
                    if !gstate.adapter.is_empty() {
                        theme::vsep(ui, 28.0);
                        ui.label(
                            RichText::new(&gstate.adapter)
                                .size(16.0)
                                .color(theme::text()),
                        );
                    }
                });
                ui.add_space(10.0);

                let full = ui.available_width();
                let two = full >= 980.0;
                // Szerokość treści karty (bez marginesów i obwódki).
                let w = if two { (full - GAP) / 2.0 } else { full } - 2.0 * PAD - 4.0;
                let best = self.gpu_best(&h.gpu[usize::from(self.just_saved).min(h.gpu.len())..]);
                if two {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        ui.vertical(|ui| self.bench_card(ui, &state, h.cpu, w, ROW1_H));
                        ui.vertical(|ui| self.stress_card(ui, &state, temp_now, w, ROW1_H));
                    });
                    ui.add_space(GAP);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        ui.vertical(|ui| self.gpu_card(ui, &gstate, best, w, ROW2_H));
                        ui.vertical(|ui| {
                            if history_card(ui, &mut h, w, ROW2_H) {
                                changed = true;
                            }
                        });
                    });
                } else {
                    self.bench_card(ui, &state, h.cpu, w, 0.0);
                    ui.add_space(GAP);
                    self.stress_card(ui, &state, temp_now, w, 0.0);
                    ui.add_space(GAP);
                    self.gpu_card(ui, &gstate, best, w, 0.0);
                    ui.add_space(GAP);
                    if history_card(ui, &mut h, w, 0.0) {
                        changed = true;
                    }
                }
            });

        let skip = usize::from(self.just_saved).min(h.gpu.len());
        self.popup_ui(ui.ctx(), best_gpu(&h.gpu[skip..]));
        changed
    }

    fn gpu_best(&self, list: &[gpu_bench::Record]) -> f64 {
        list.iter().map(|r| r.gflops).fold(0.0, f64::max)
    }

    fn bench_card(
        &mut self,
        ui: &mut egui::Ui,
        state: &bench::State,
        history: &[Record],
        w: f32,
        h: f32,
    ) {
        theme::card(10, PAD as i8).show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            theme::subheading(ui, Icon::Cpu, tr("Wydajność procesora", "CPU performance"));
            note(
                ui,
                tr(
                    "Po 10 s na jednym wątku (rdzenie P) i na wszystkich wątkach. Skala własna - porównuj wyniki na tym komputerze, np. przed i po zmianie ustawień w CPU.",
                    "10 s on one thread (P-cores) and on all threads. Own scale - compare results on this PC, e.g. before and after changing CPU settings.",
                ),
            );
            ui.add_space(14.0);
            let best_single = history.iter().map(|r| r.single).fold(0.0, f64::max);
            let best_multi = history.iter().map(|r| r.multi).fold(0.0, f64::max);
            let (p_single, p_multi) = match state.phase {
                Phase::Bench { multi: false, progress } => (Some(progress), None),
                Phase::Bench { multi: true, progress } => (None, Some(progress)),
                _ => (None, None),
            };
            score_row(ui, tr("Jeden wątek", "Single thread"), state.scores.single, best_single, p_single, w);
            ui.add_space(10.0);
            score_row(ui, tr("Wszystkie wątki", "Multi thread"), state.scores.multi, best_multi, p_multi, w);
            ui.add_space(6.0);
            let ratio = match (state.scores.single, state.scores.multi) {
                (Some(s), Some(m)) => crate::trf!(
                    "Wielowątkowo {:.1}× szybciej niż jeden wątek",
                    "Multi thread {:.1}× faster than single thread",
                    m / s
                ),
                _ => String::new(),
            };
            note(ui, &ratio);
            ui.add_space(14.0);
            let bench_running = matches!(state.phase, Phase::Bench { .. });
            ui.horizontal(|ui| {
                if bench_running {
                    if theme::primary_button(ui, None, tr("Zatrzymaj", "Stop"), true).clicked() {
                        self.bench.stop();
                    }
                    ui.label(RichText::new(tr("Nie używaj komputera w trakcie testu.", "Don't use the PC during the test.")).size(12.0).color(warn()));
                } else if theme::primary_button(
                    ui,
                    Some(Icon::Bench),
                    tr("Uruchom test", "Run benchmark"),
                    !state.running() && !self.gpu.state().running(),
                )
                .clicked()
                {
                    self.saved = false;
                    let ctx = ui.ctx().clone();
                    self.bench.start_bench(move || ctx.request_repaint());
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
        h: f32,
    ) {
        theme::card(10, PAD as i8).show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            theme::subheading(ui, Icon::Thermo, tr("Stress test procesora", "CPU stress test"));
            note(
                ui,
                &crate::trf!(
                    "Pełne obciążenie wszystkich wątków{}. Każdy wynik jest sprawdzany - błąd obliczeń oznacza niestabilne ustawienia (np. za mocny undervolting).",
                    "Full load on all threads{}. Every result is verified - a calculation error means unstable settings (e.g. too much undervolting).",
                    if state.avx2 || is_avx2() { " (AVX2 + FMA)" } else { "" }
                ),
            );
            ui.add_space(8.0);
            let stress = matches!(state.phase, Phase::Stress);
            ui.horizontal(|ui| {
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
                    ui.add_space(10.0);
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

            let elapsed = state.elapsed().as_secs();
            let now_w = state.samples.last().and_then(|s| s.watts);
            let (err_text, err_color) = if state.started.is_none() {
                ("-".to_owned(), theme::text())
            } else if state.errors == 0 {
                ("0".to_owned(), theme::ok())
            } else {
                (state.errors.to_string(), theme::hot())
            };
            stats_row(
                ui,
                w,
                &[
                    (tr("Czas", "Time"), format!("{:02}:{:02}", elapsed / 60, elapsed % 60), theme::text()),
                    (
                        tr("Temperatura", "Temperature"),
                        temp_now.map_or("-".into(), |t| format!("{t:.0} °C")),
                        temp_color(temp_now, TEMP_LIMITS[self.temp_limit]),
                    ),
                    (tr("Maks.", "Max"), state.max_temp.map_or("-".into(), |t| format!("{t:.0} °C")), theme::text()),
                    (tr("Moc", "Power"), now_w.map_or("-".into(), |w| format!("{w:.0} W")), theme::text()),
                    (tr("Błędy", "Errors"), err_text, err_color),
                ],
            );
            ui.add_space(6.0);
            let temps: Vec<Option<f32>> = state.samples.iter().map(|s| s.temp).collect();
            let watts: Vec<Option<f32>> = state.samples.iter().map(|s| s.watts).collect();
            let cw = (w - 12.0) / 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                crate::history::chart(ui, vec2(cw, 60.0), &temps, theme::hot(), crate::history::Scale::Around(10.0), &|v| format!("{v:.0} °C"));
                crate::history::chart(ui, vec2(cw, 60.0), &watts, accent(), crate::history::Scale::FromZero(50.0), &|v| format!("{v:.0} W"));
            });
            ui.add_space(6.0);
            if let Phase::StressDone(stop) = &state.phase {
                let text = match stop {
                    Stop::Temp(t) => crate::trf!("Zatrzymano: {t:.0} °C osiągnęło limit.", "Stopped: {t:.0} °C reached the limit."),
                    Stop::Time => tr("Zakończono po ustawionym czasie.", "Finished after the set time.").to_owned(),
                    Stop::User => tr("Zatrzymano.", "Stopped.").to_owned(),
                };
                let verdict = if state.errors > 0 {
                    crate::trf!(" Błędy obliczeń: {} - ustawienia są niestabilne.", " Calculation errors: {} - the settings are unstable.", state.errors)
                } else {
                    tr(" Bez błędów obliczeń.", " No calculation errors.").to_owned()
                };
                let color = if state.errors > 0 { theme::hot() } else if matches!(stop, Stop::Temp(_)) { warn() } else { accent() };
                ui.label(RichText::new(text + &verdict).size(12.5).color(color));
            } else if temp_now.is_none() {
                ui.label(
                    RichText::new(tr(
                        "Brak odczytu temperatury procesora - limit temperatury nie zadziała.",
                        "No CPU temperature reading - the temperature limit will not work.",
                    ))
                    .size(12.0)
                    .color(warn()),
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if stress {
                    if theme::primary_button(ui, None, tr("Zatrzymaj", "Stop"), true).clicked() {
                        self.bench.stop();
                    }
                } else if theme::primary_button(
                    ui,
                    Some(Icon::Thermo),
                    tr("Start", "Start"),
                    !state.running() && !self.gpu.state().running(),
                )
                .clicked()
                {
                    let minutes = DURATIONS[self.duration];
                    let limit = (minutes > 0).then(|| Duration::from_secs(minutes * 60));
                    let ctx = ui.ctx().clone();
                    self.bench.start_stress(limit, TEMP_LIMITS[self.temp_limit], move || ctx.request_repaint());
                }
            });
        });
    }

    fn gpu_card(&mut self, ui: &mut egui::Ui, s: &gpu_bench::State, best: f64, w: f32, h: f32) {
        theme::card(10, PAD as i8).show(ui, |ui| {
            ui.set_width(w);
            ui.set_min_height(h);
            theme::subheading(ui, Icon::Gpu, tr("Stabilność karty graficznej", "Graphics card stability"));
            note(
                ui,
                tr(
                    "2 minuty pełnego obciążenia: obliczenia FP32 na całym rdzeniu i zapis/odczyt 512 MB pamięci. Każdy wynik jest sprawdzany - błąd oznacza niestabilne zegary (podkręcenie, undervolting). Po teście pokaże się okno z wynikiem i wykresami.",
                    "2 minutes of full load: FP32 math on the whole GPU and 512 MB memory write/read. Every result is verified - an error means unstable clocks (overclock, undervolt). A window with the result and charts opens after the test.",
                ),
            );
            ui.add_space(10.0);

            // Postęp.
            let progress = (s.elapsed().as_secs_f32() / gpu_bench::TEST_TIME.as_secs_f32()).min(1.0);
            let (rect, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(4), theme::track());
            if s.started.is_some() {
                let mut fill = rect;
                fill.set_width(rect.width() * progress);
                ui.painter().rect_filled(fill, CornerRadius::same(4), accent());
            }
            ui.add_space(4.0);
            let status = match &s.phase {
                gpu_bench::Phase::Idle => tr("Gotowy.", "Ready.").to_owned(),
                gpu_bench::Phase::Starting => tr("Przygotowanie (shadery, dobór obciążenia)...", "Preparing (shaders, load tuning)...").to_owned(),
                gpu_bench::Phase::Running => {
                    let left = gpu_bench::TEST_TIME.saturating_sub(s.elapsed()).as_secs();
                    crate::trf!("Test trwa - zostało {left} s", "Running - {left} s left")
                }
                gpu_bench::Phase::Done(true) => tr("Zakończony.", "Finished.").to_owned(),
                gpu_bench::Phase::Done(false) => tr("Zatrzymany przed czasem - wynik nie trafia do historii.", "Stopped early - the result is not saved.").to_owned(),
                gpu_bench::Phase::Failed(e) => crate::trf!("Błąd: {e}", "Error: {e}"),
            };
            let color = if matches!(s.phase, gpu_bench::Phase::Failed(_)) { theme::hot() } else { muted() };
            ui.label(RichText::new(status).size(12.0).color(color));
            ui.add_space(8.0);

            let last = s.samples.last().copied().unwrap_or_default();
            let fmt = |v: Option<f32>, unit: &str| v.map_or("-".to_owned(), |v| format!("{v:.0} {unit}"));
            let (err_text, err_color) = if s.started.is_none() {
                ("-".to_owned(), theme::text())
            } else if s.errors() == 0 {
                ("0".to_owned(), theme::ok())
            } else {
                (s.errors().to_string(), theme::hot())
            };
            stats_row(
                ui,
                w,
                &[
                    (tr("Rdzeń", "Core"), fmt(last.core_mhz, "MHz"), theme::text()),
                    (tr("Pamięć", "Memory"), fmt(last.mem_mhz, "MHz"), theme::text()),
                    (tr("Temperatura", "Temperature"), fmt(last.temp, "°C"), theme::text()),
                    (tr("Moc", "Power"), fmt(last.watts, "W"), theme::text()),
                    (tr("Błędy", "Errors"), err_text, err_color),
                ],
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Wynik", "Score")).color(muted()));
                let score = s.gflops.map_or("-".to_owned(), |g| format!("{g:.0}"));
                ui.label(RichText::new(score).size(26.0).strong().color(theme::text()));
                ui.label(RichText::new("GFLOPS").color(muted()));
                if let Some(bw) = s.bandwidth {
                    theme::vsep(ui, 22.0);
                    ui.label(RichText::new(format!("{bw:.0} GB/s")).size(16.0).color(theme::text()));
                }
                if best > 0.0 {
                    if let Some(g) = s.gflops {
                        theme::vsep(ui, 22.0);
                        let diff = (g / best - 1.0) * 100.0;
                        ui.label(
                            RichText::new(crate::trf!("{:+.1}% od najlepszego", "{:+.1}% vs best", diff))
                                .size(12.0)
                                .color(muted()),
                        );
                    }
                }
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if s.running() {
                    if theme::primary_button(ui, None, tr("Zatrzymaj", "Stop"), true).clicked() {
                        self.gpu.stop();
                    }
                } else {
                    let cpu_busy = self.bench.state().running();
                    if theme::primary_button(ui, Some(Icon::Gpu), tr("Start (2 min)", "Start (2 min)"), !cpu_busy).clicked() {
                        self.gpu_saved = false;
                        self.popup = None;
                        self.just_saved = false;
                        let ctx = ui.ctx().clone();
                        self.gpu.start(move || ctx.request_repaint());
                    }
                    if matches!(s.phase, gpu_bench::Phase::Done(_)) && ui.button(tr("Pokaż wynik", "Show result")).clicked() {
                        self.popup = Some(s.clone());
                    }
                }
            });
        });
    }

    /// Okno z wynikiem testu karty: wynik, zegary, temperatura, moc i wykresy z 2 minut.
    fn popup_ui(&mut self, ctx: &egui::Context, best: Option<gpu_bench::Record>) {
        let Some(s) = &self.popup else { return };
        let mut open = true;
        let mut close = false;
        egui::Window::new(tr("Wynik testu karty graficznej", "Graphics card test result"))
            .collapsible(false)
            .resizable(false)
            .default_width(640.0)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                let w = 620.0;
                ui.set_width(w);
                ui.label(RichText::new(&s.adapter).color(muted()));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let score = s.gflops.map_or("-".to_owned(), |g| format!("{g:.0}"));
                    ui.label(RichText::new(score).size(34.0).strong().color(theme::text()));
                    ui.label(RichText::new("GFLOPS FP32").color(muted()));
                    if let Some(bw) = s.bandwidth {
                        theme::vsep(ui, 26.0);
                        ui.label(RichText::new(format!("{bw:.0} GB/s")).size(20.0).color(theme::text()));
                        ui.label(RichText::new(tr("pamięć", "memory")).color(muted()));
                    }
                });
                let (verdict, color) = if let gpu_bench::Phase::Failed(e) = &s.phase {
                    (crate::trf!("PRZERWANY - {e}", "ABORTED - {e}"), theme::hot())
                } else if s.errors() == 0 {
                    (
                        tr("STABILNA - bez błędów obliczeń i pamięci", "STABLE - no calculation or memory errors").to_owned(),
                        theme::ok(),
                    )
                } else {
                    (
                        crate::trf!(
                            "NIESTABILNA - błędy obliczeń: {}, pamięci: {}. Zmniejsz podkręcenie.",
                            "UNSTABLE - calculation errors: {}, memory errors: {}. Reduce the overclock.",
                            s.alu_errors,
                            s.mem_errors
                        ),
                        theme::hot(),
                    )
                };
                crate::history::badge(ui, &verdict, color);
                if let (Some(b), Some(g)) = (&best, s.gflops) {
                    ui.label(
                        RichText::new(crate::trf!(
                            "Najlepszy wynik: {:.0} GFLOPS ({}) - ten test {:+.1}%",
                            "Best result: {:.0} GFLOPS ({}) - this test {:+.1}%",
                            b.gflops,
                            b.when,
                            (g / b.gflops - 1.0) * 100.0
                        ))
                        .size(12.0)
                        .color(muted()),
                    );
                }
                if matches!(s.phase, gpu_bench::Phase::Done(false)) {
                    ui.label(RichText::new(tr("Test zatrzymany przed czasem.", "Test stopped early.")).size(12.0).color(warn()));
                }
                ui.add_space(10.0);

                // Wiersze: nazwa | średnio / min / maks. | wykres z 2 minut.
                let rows: [(&str, &str, Color32, crate::history::Scale, fn(&gpu_bench::Sample) -> Option<f32>); 4] = [
                    (tr("Zegar rdzenia", "Core clock"), "MHz", accent(), crate::history::Scale::Around(100.0), |x| x.core_mhz),
                    (tr("Zegar pamięci", "Memory clock"), "MHz", Color32::from_rgb(170, 120, 255), crate::history::Scale::Around(100.0), |x| x.mem_mhz),
                    (tr("Temperatura", "Temperature"), "°C", theme::hot(), crate::history::Scale::Around(10.0), |x| x.temp),
                    (tr("Pobór mocy", "Power draw"), "W", warn(), crate::history::Scale::FromZero(50.0), |x| x.watts),
                ];
                for (name, unit, color, scale, get) in rows {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(170.0);
                            ui.label(RichText::new(name).color(theme::text()));
                            match s.stats(get) {
                                Some((avg, min, max)) => {
                                    ui.label(RichText::new(format!("{avg:.0} {unit}")).size(20.0).strong().color(color));
                                    ui.label(
                                        RichText::new(crate::trf!("min. {:.0} / maks. {:.0}", "min {:.0} / max {:.0}", min, max))
                                            .size(11.5)
                                            .color(muted()),
                                    );
                                }
                                None => {
                                    ui.label(RichText::new(tr("brak odczytu", "no reading")).color(muted()));
                                }
                            }
                        });
                        let series: Vec<Option<f32>> = s.samples.iter().map(get).collect();
                        let u = unit.to_owned();
                        crate::history::chart(ui, vec2(w - 182.0, 64.0), &series, color, scale, &move |v| format!("{v:.0} {u}"));
                    });
                    ui.add_space(6.0);
                }
                if let Some((avg, min, _)) = s.stats(|x| x.core_mhz) {
                    ui.label(
                        RichText::new(crate::trf!(
                            "Stałość zegara rdzenia: najniższy {:.1}% średniego.",
                            "Core clock consistency: lowest {:.1}% of the average.",
                            min / avg * 100.0
                        ))
                        .size(12.0)
                        .color(muted()),
                    );
                }
                if !s.limits.is_empty() {
                    ui.label(
                        RichText::new(crate::trf!(
                            "Ograniczenia zegarów w teście: {}",
                            "Clock limits during the test: {}",
                            s.limits.join(", ")
                        ))
                        .size(12.0)
                        .color(muted()),
                    );
                }
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if theme::primary_button(ui, None, tr("Zamknij", "Close"), true).clicked() {
                        close = true;
                    }
                });
            });
        if !open || close {
            self.popup = None;
        }
    }
}

fn best_gpu(list: &[gpu_bench::Record]) -> Option<gpu_bench::Record> {
    list.iter()
        .max_by(|a, b| a.gflops.total_cmp(&b.gflops))
        .cloned()
}

fn gpu_record(s: &gpu_bench::State) -> Option<gpu_bench::Record> {
    Some(gpu_bench::Record {
        when: bench::now_text(),
        gpu: s.adapter.clone(),
        gflops: s.gflops?,
        bandwidth: s.bandwidth.unwrap_or(0.0),
        errors: s.errors(),
        core_avg: s.stats(|x| x.core_mhz).map_or(0.0, |v| v.0),
        mem_avg: s.stats(|x| x.mem_mhz).map_or(0.0, |v| v.0),
        temp_max: s.stats(|x| x.temp).map_or(0.0, |v| v.2),
        watts_avg: s.stats(|x| x.watts).map_or(0.0, |v| v.0),
    })
}

fn is_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    false
}

fn note(ui: &mut egui::Ui, text: &str) {
    if !text.is_empty() {
        ui.label(RichText::new(text).size(12.0).color(muted()));
    }
}

fn temp_color(t: Option<f32>, limit: f32) -> Color32 {
    match t {
        Some(t) if t >= limit - 5.0 => theme::hot(),
        Some(t) if t >= 80.0 => warn(),
        _ => theme::text(),
    }
}

/// Odczyty w równych kolumnach na całą szerokość karty.
fn stats_row(ui: &mut egui::Ui, w: f32, items: &[(&str, String, Color32)]) {
    let col = w / items.len() as f32;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (title, value, color) in items {
            ui.allocate_ui_with_layout(
                vec2(col, 44.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(col);
                    ui.label(RichText::new(*title).size(11.5).color(muted()));
                    ui.label(RichText::new(value).size(19.0).strong().color(*color));
                },
            );
        }
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
            [120.0, 24.0],
            egui::Label::new(RichText::new(title).color(theme::text())),
        );
        let bar_w = (w - 120.0 - 100.0 - 16.0).max(80.0);
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
            [100.0, 24.0],
            egui::Label::new(RichText::new(text).size(20.0).strong().color(theme::text())),
        );
    });
}

/// Historia wyników procesora i karty; zwraca `true`, gdy wyczyszczono.
fn history_card(ui: &mut egui::Ui, h: &mut Histories, w: f32, min_h: f32) -> bool {
    let mut cleared = false;
    theme::card(10, PAD as i8).show(ui, |ui| {
        ui.set_width(w);
        ui.set_min_height(min_h);
        ui.horizontal(|ui| {
            theme::subheading(ui, Icon::Sensors, tr("Historia wyników", "Result history"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if (!h.cpu.is_empty() || !h.gpu.is_empty())
                    && ui.button(tr("Wyczyść", "Clear")).clicked()
                {
                    h.cpu.clear();
                    h.gpu.clear();
                    cleared = true;
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("bench_history")
            .max_height((min_h - 60.0).max(160.0))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(tr("Procesor", "Processor"))
                        .strong()
                        .color(accent()),
                );
                if h.cpu.is_empty() {
                    note(ui, tr("Brak wyników.", "No results yet."));
                } else {
                    let best_s = h.cpu.iter().map(|r| r.single).fold(0.0, f64::max);
                    let best_m = h.cpu.iter().map(|r| r.multi).fold(0.0, f64::max);
                    egui::Grid::new("bench_cpu_history")
                        .num_columns(3)
                        .spacing([24.0, 3.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for t in [
                                tr("Data", "Date"),
                                tr("Jeden wątek", "Single"),
                                tr("Wszystkie", "Multi"),
                            ] {
                                ui.label(RichText::new(t).size(11.5).color(muted()));
                            }
                            ui.end_row();
                            for r in h.cpu.iter() {
                                ui.label(RichText::new(&r.when).size(12.5));
                                ui.label(best_mark(r.single, best_s));
                                ui.label(best_mark(r.multi, best_m));
                                ui.end_row();
                            }
                        });
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("Karta graficzna", "Graphics card"))
                        .strong()
                        .color(accent()),
                );
                if h.gpu.is_empty() {
                    note(ui, tr("Brak wyników.", "No results yet."));
                } else {
                    let best = h.gpu.iter().map(|r| r.gflops).fold(0.0, f64::max);
                    egui::Grid::new("bench_gpu_history")
                        .num_columns(6)
                        .spacing([18.0, 3.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for t in [
                                tr("Data", "Date"),
                                "GFLOPS",
                                "GB/s",
                                tr("Rdzeń", "Core"),
                                "°C / W",
                                tr("Błędy", "Errors"),
                            ] {
                                ui.label(RichText::new(t).size(11.5).color(muted()));
                            }
                            ui.end_row();
                            for r in h.gpu.iter() {
                                ui.label(RichText::new(&r.when).size(12.5));
                                ui.label(best_mark(r.gflops, best));
                                ui.label(RichText::new(format!("{:.0}", r.bandwidth)).size(12.5));
                                ui.label(
                                    RichText::new(format!("{:.0} MHz", r.core_avg)).size(12.5),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "{:.0} / {:.0}",
                                        r.temp_max, r.watts_avg
                                    ))
                                    .size(12.5),
                                );
                                let color = if r.errors == 0 {
                                    theme::ok()
                                } else {
                                    theme::hot()
                                };
                                ui.label(
                                    RichText::new(r.errors.to_string()).size(12.5).color(color),
                                );
                                ui.end_row();
                            }
                        });
                }
            });
    });
    cleared
}

fn best_mark(v: f64, best: f64) -> RichText {
    RichText::new(format!("{v:.0}"))
        .size(12.5)
        .color(if v >= best {
            theme::ok()
        } else {
            theme::text()
        })
}
