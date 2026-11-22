//! Zakładka BENCH: test wydajności procesora (jeden wątek / wszystkie wątki,
//! w stylu CPU-Z) i stress test z kontrolą poprawności obliczeń.
//!
//! Obciążenie (jednostka pracy) to własna mieszanka: liczby całkowite
//! (generator xorshift), sortowanie (rozgałęzienia, pamięć L1/L2), skrót FNV
//! i obliczenia zmiennoprzecinkowe (pierwiastek, sinus). Wynik = jednostki na
//! sekundę × stała skali. Skala jest własna - liczby nie są porównywalne
//! z CPU-Z ani innymi programami, służą do porównań na tym samym komputerze
//! (np. przed i po undervoltingu).
//!
//! Stress test: wszystkie wątki, jednostka + blok FMA (AVX2, gdy procesor ma)
//! na danych w L1 - najwięcej ciepła. Każdy wynik jest porównywany z wartością
//! policzoną na starcie: różnica = błąd obliczeń (niestabilne ustawienia).
//! Test kończy się sam po czasie albo po przekroczeniu limitu temperatury.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Czas każdej części testu wydajności (bez rozgrzewki).
pub const BENCH_TIME: Duration = Duration::from_secs(10);
const WARMUP: Duration = Duration::from_secs(1);
/// Skala wyniku: jednostki/s × SCALE.
const SCALE: f64 = 0.02;
/// Liczba różnych ziaren (wyniki wzorcowe liczone na starcie stress testu).
const SEEDS: u64 = 64;

/// Jednostka pracy testu wydajności (deterministyczna dla ziarna).
#[inline(never)]
pub fn unit(seed: u64) -> u64 {
    let mut buf = [0u32; 2048];
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for v in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *v = (x >> 16) as u32;
    }
    buf.sort_unstable();
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &v in buf.iter() {
        h = (h ^ v as u64).wrapping_mul(0x100_0000_01B3);
    }
    let mut f = [0f64; 256];
    let mut acc = 0.0f64;
    for (i, a) in f.iter_mut().enumerate() {
        *a = ((i as f64 + (seed & 0xFF) as f64) * 0.37).sqrt();
    }
    for _ in 0..4 {
        for a in f.iter_mut() {
            *a = (*a * 1.000_001 + 0.25).sin() * 2.0 + 3.0;
            acc += *a;
        }
    }
    std::hint::black_box(h ^ acc.to_bits())
}

/// Blok FMA na danych w L1 (najwięcej ciepła).
#[inline(always)]
fn fma_body(seed: u64) -> u64 {
    let mut a = [0f64; 512];
    for (i, v) in a.iter_mut().enumerate() {
        *v = 1.0 + ((i as u64 ^ seed) & 0xFF) as f64 * 1e-3;
    }
    let (m, c) = (0.999_999_9f64, 1e-7f64);
    for _ in 0..400 {
        for v in a.iter_mut() {
            *v = v.mul_add(m, c);
        }
    }
    a.iter().fold(0u64, |h, v| h.rotate_left(5) ^ v.to_bits())
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn fma_avx2(seed: u64) -> u64 {
    fma_body(seed)
}

fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    false
}

/// Jednostka stress testu: zwykła jednostka + blok FMA.
fn stress_unit(seed: u64, avx2: bool) -> u64 {
    let fma = if avx2 {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: wołane tylko, gdy procesor ma AVX2 i FMA.
            unsafe { fma_avx2(seed) }
        }
        #[cfg(not(target_arch = "x86_64"))]
        fma_body(seed)
    } else {
        fma_body(seed)
    };
    std::hint::black_box(unit(seed) ^ fma)
}

/// Maska procesorów logicznych rdzeni wydajnych (P) - test jednego wątku
/// działa na nich (Windows wybiera najszybszy rdzeń). 0 = bez ograniczenia.
fn performance_mask() -> usize {
    let topo = crate::cpu::monitor::topology();
    let best = topo.iter().map(|t| t.class).max().unwrap_or(0);
    let mask = topo
        .iter()
        .filter(|t| t.class == best)
        .fold(0usize, |m, t| m | t.mask);
    if topo.iter().any(|t| t.class != best) {
        mask
    } else {
        0
    }
}

#[cfg(windows)]
fn pin(mask: usize, n: usize) {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadAffinityMask, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    };
    // SAFETY: zmiana powinowactwa i priorytetu bieżącego wątku.
    unsafe {
        if mask != 0 {
            SetThreadAffinityMask(GetCurrentThread(), mask);
        }
        // Wyższy priorytet tylko dla testu jednego wątku: przy wszystkich wątkach
        // zagłodziłby okno programu (pasek postępu stałby w miejscu).
        if n == 1 {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        }
    }
}

#[cfg(not(windows))]
fn pin(_mask: usize, _n: usize) {}

pub fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Wyniki testu wydajności.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scores {
    pub single: Option<f64>,
    pub multi: Option<f64>,
}

/// Punkt wykresu stress testu (co sekundę).
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub temp: Option<f32>,
    pub watts: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    /// Zatrzymany przez użytkownika.
    User,
    /// Minął ustawiony czas.
    Time,
    /// Temperatura procesora osiągnęła limit.
    Temp(f32),
}

#[derive(Clone, Debug, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// Test wydajności: część (`multi`), postęp 0..1.
    Bench {
        multi: bool,
        progress: f32,
    },
    BenchDone,
    Stress,
    StressDone(Stop),
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub phase: Phase,
    pub scores: Scores,
    /// Stress test: początek, wątki, wykonane jednostki, błędy obliczeń.
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
    pub threads: usize,
    pub units: u64,
    pub errors: u64,
    pub avx2: bool,
    pub max_temp: Option<f32>,
    pub max_watts: Option<f32>,
    pub samples: Vec<Sample>,
}

impl State {
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Bench { .. } | Phase::Stress)
    }

    pub fn elapsed(&self) -> Duration {
        match self.started {
            Some(s) => self
                .ended
                .unwrap_or_else(Instant::now)
                .saturating_duration_since(s),
            None => Duration::ZERO,
        }
    }
}

/// Uruchamianie testów w tle.
#[derive(Default)]
pub struct Bench {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    stress_limit: Option<Duration>,
    temp_limit: f32,
    last_sample: Option<Instant>,
}

impl Bench {
    pub fn new() -> Self {
        Self {
            state: Arc::default(),
            stop: Arc::default(),
            stress_limit: None,
            temp_limit: 95.0,
            last_sample: None,
        }
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
    }

    /// Test wydajności: 10 s jeden wątek, potem 10 s wszystkie wątki.
    pub fn start_bench(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        if self.state().running() {
            return;
        }
        self.stop.store(false, Relaxed);
        let (state, stop) = (self.state.clone(), self.stop.clone());
        if let Ok(mut s) = state.lock() {
            *s = State {
                phase: Phase::Bench {
                    multi: false,
                    progress: 0.0,
                },
                threads: threads(),
                ..Default::default()
            };
        }
        std::thread::spawn(move || {
            let wake = Arc::new(wake);
            let single = measure(1, performance_mask(), &state, &stop, false, &wake);
            if let Ok(mut s) = state.lock() {
                s.scores.single = single;
            }
            let multi = if single.is_some() {
                measure(threads(), 0, &state, &stop, true, &wake)
            } else {
                None
            };
            if let Ok(mut s) = state.lock() {
                s.scores.multi = multi;
                s.phase = if stop.load(Relaxed) {
                    Phase::Idle
                } else {
                    Phase::BenchDone
                };
            }
            wake();
        });
    }

    /// Stress test na wszystkich wątkach; `limit` = czas (None = do zatrzymania).
    pub fn start_stress(
        &mut self,
        limit: Option<Duration>,
        temp_limit: f32,
        wake: impl Fn() + Send + Sync + 'static,
    ) {
        if self.state().running() {
            return;
        }
        self.stop.store(false, Relaxed);
        self.stress_limit = limit;
        self.temp_limit = temp_limit;
        self.last_sample = None;
        let avx2 = has_avx2();
        let n = threads();
        let (state, stop) = (self.state.clone(), self.stop.clone());
        if let Ok(mut s) = state.lock() {
            let scores = s.scores;
            *s = State {
                phase: Phase::Stress,
                scores,
                started: Some(Instant::now()),
                threads: n,
                avx2,
                ..Default::default()
            };
        }
        std::thread::spawn(move || {
            // Wyniki wzorcowe (jeden wątek, przed obciążeniem).
            let expected: Arc<Vec<u64>> =
                Arc::new((0..SEEDS).map(|s| stress_unit(s, avx2)).collect());
            let units = Arc::new(AtomicU64::new(0));
            let errors = Arc::new(AtomicU64::new(0));
            let workers: Vec<_> = (0..n)
                .map(|t| {
                    let (expected, units, errors, stop) = (
                        expected.clone(),
                        units.clone(),
                        errors.clone(),
                        stop.clone(),
                    );
                    std::thread::spawn(move || {
                        let mut seed = t as u64;
                        while !stop.load(Relaxed) {
                            let i = seed % SEEDS;
                            if stress_unit(i, avx2) != expected[i as usize] {
                                errors.fetch_add(1, Relaxed);
                            }
                            units.fetch_add(1, Relaxed);
                            seed = seed.wrapping_add(7);
                        }
                    })
                })
                .collect();
            while !stop.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                if let Ok(mut s) = state.lock() {
                    s.units = units.load(Relaxed);
                    s.errors = errors.load(Relaxed);
                }
                wake();
            }
            for w in workers {
                let _ = w.join();
            }
            if let Ok(mut s) = state.lock() {
                s.units = units.load(Relaxed);
                s.errors = errors.load(Relaxed);
                s.ended = Some(Instant::now());
                if !matches!(s.phase, Phase::StressDone(_)) {
                    s.phase = Phase::StressDone(Stop::User);
                }
            }
            wake();
        });
    }

    /// Co sekundę z `App::logic` (także przy ukrytym oknie): odczyty do
    /// wykresu, limit temperatury i czasu stress testu.
    pub fn observe(&mut self, temp: Option<f32>, watts: Option<f32>) {
        let now = Instant::now();
        if self
            .last_sample
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        let Ok(mut s) = self.state.lock() else { return };
        if !matches!(s.phase, Phase::Stress) {
            return;
        }
        self.last_sample = Some(now);
        s.samples.push(Sample { temp, watts });
        if s.samples.len() > 120 {
            s.samples.remove(0);
        }
        if let Some(t) = temp {
            s.max_temp = Some(s.max_temp.map_or(t, |m| m.max(t)));
        }
        if let Some(w) = watts {
            s.max_watts = Some(s.max_watts.map_or(w, |m| m.max(w)));
        }
        let stop = if temp.is_some_and(|t| t >= self.temp_limit) {
            Some(Stop::Temp(temp.unwrap()))
        } else if self.stress_limit.is_some_and(|l| s.elapsed() >= l) {
            Some(Stop::Time)
        } else {
            None
        };
        if let Some(reason) = stop {
            s.phase = Phase::StressDone(reason);
            self.stop.store(true, Relaxed);
        }
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

/// Pomiar: `n` wątków przez rozgrzewkę + BENCH_TIME; wynik w punktach.
fn measure(
    n: usize,
    mask: usize,
    state: &Arc<Mutex<State>>,
    stop: &Arc<AtomicBool>,
    multi: bool,
    wake: &Arc<impl Fn() + Send + Sync + 'static>,
) -> Option<f64> {
    let done = Arc::new(AtomicBool::new(false));
    let counts: Vec<Arc<AtomicU64>> = (0..n).map(|_| Arc::new(AtomicU64::new(0))).collect();
    let workers: Vec<_> = counts
        .iter()
        .enumerate()
        .map(|(t, c)| {
            let (c, done, stop) = (c.clone(), done.clone(), stop.clone());
            std::thread::spawn(move || {
                pin(mask, n);
                let mut seed = (t as u64) << 32;
                while !done.load(Relaxed) && !stop.load(Relaxed) {
                    unit(seed);
                    seed += 1;
                    c.fetch_add(1, Relaxed);
                }
            })
        })
        .collect();
    let total = |c: &[Arc<AtomicU64>]| c.iter().map(|x| x.load(Relaxed)).sum::<u64>();
    let start = Instant::now();
    let mut base: Option<(Instant, u64)> = None;
    let mut result = None;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if stop.load(Relaxed) {
            break;
        }
        let el = start.elapsed();
        if base.is_none() && el >= WARMUP {
            base = Some((Instant::now(), total(&counts)));
        }
        let progress = (el.as_secs_f32() / (WARMUP + BENCH_TIME).as_secs_f32()).min(1.0);
        if let Ok(mut s) = state.lock() {
            s.phase = Phase::Bench { multi, progress };
        }
        wake();
        if el >= WARMUP + BENCH_TIME {
            if let Some((t0, u0)) = base {
                let secs = t0.elapsed().as_secs_f64();
                result = Some((total(&counts) - u0) as f64 / secs * SCALE);
            }
            break;
        }
    }
    done.store(true, Relaxed);
    for w in workers {
        let _ = w.join();
    }
    result
}

/// Zapisany wynik (historia w ustawieniach).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub when: String,
    pub single: f64,
    pub multi: f64,
    pub threads: usize,
}

/// Bieżąca data i godzina (czas lokalny), np. "2026-11-22 18:05".
pub fn now_text() -> String {
    #[cfg(windows)]
    {
        // SAFETY: wypełnia strukturę czasu lokalnego.
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute
        )
    }
    #[cfg(not(windows))]
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_are_deterministic() {
        assert_eq!(unit(5), unit(5));
        assert_ne!(unit(5), unit(6));
        assert_eq!(stress_unit(3, false), stress_unit(3, false));
        if has_avx2() {
            // FMA sprzętowe = ten sam wynik co mul_add programowe (pojedyncze zaokrąglenie).
            assert_eq!(stress_unit(3, true), stress_unit(3, false));
        }
    }

    /// Kalibracja: `cargo test bench_calibrate --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_calibrate() {
        let t = Instant::now();
        let mut n = 0u64;
        while t.elapsed() < Duration::from_secs(3) {
            unit(n);
            n += 1;
        }
        println!(
            "jednostki/s (1 wątek, bez przypięcia): {:.0}",
            n as f64 / 3.0
        );
    }
}

#[cfg(test)]
mod run {
    use super::*;

    /// Pełny test wydajności i 15 s stress testu:
    /// `cargo test bench_run --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_run() {
        let mut b = Bench::new();
        b.start_bench(|| {});
        std::thread::sleep(Duration::from_millis(300));
        while b.state().running() {
            std::thread::sleep(Duration::from_millis(200));
        }
        let s = b.state();
        println!(
            "jeden wątek {:?}, wszystkie {:?} ({} wątków)",
            s.scores.single, s.scores.multi, s.threads
        );
        b.start_stress(None, 95.0, || {});
        std::thread::sleep(Duration::from_secs(15));
        b.stop();
        while b.state().running() {
            std::thread::sleep(Duration::from_millis(100));
        }
        let s = b.state();
        println!(
            "stress: avx2 {}, jednostki {}, błędy {}, {:?}",
            s.avx2, s.units, s.errors, s.phase
        );
        assert_eq!(s.errors, 0);
    }
}
