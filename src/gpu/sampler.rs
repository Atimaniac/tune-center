//! Odczyt czujników karty w osobnym wątku, raz na sekundę.
//!
//! Pełny odczyt (NVAPI + NVML, m.in. przepływ PCIe mierzony przez sterownik
//! w krótkim oknie) trwa ok. 100 ms - w wątku interfejsu powodowałby przycięcia.
//! Wątek ma własne połączenie ze sterownikiem (`load_drivers`) i trzyma
//! historię ostatnich odczytów do wykresów.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::backend::{Backend, Demo, Monitor};

/// Liczba pamiętanych odczytów (sekund) do wykresów.
pub const HISTORY: usize = 120;

#[derive(Default)]
pub struct Samples {
    /// Kolejne odczyty, najnowszy na końcu.
    pub history: VecDeque<Monitor>,
    /// Licznik odczytów (do wykrycia nowego).
    pub seq: u64,
}

impl Samples {
    pub fn latest(&self) -> Option<&Monitor> {
        self.history.back()
    }
}

pub struct Sampler {
    pub samples: Arc<Mutex<Samples>>,
    gpu: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    /// Ostatnie wyświetlenie widoku Informacje - szczegółowe (kosztowne)
    /// odczyty tylko, gdy był widoczny w ciągu ostatnich 3 s.
    detail: Arc<Mutex<Option<Instant>>>,
}

impl Sampler {
    /// `demo`: symulowana karta zamiast sterownika.
    pub fn start(demo: bool) -> Self {
        let samples = Arc::new(Mutex::new(Samples::default()));
        let gpu = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let detail: Arc<Mutex<Option<Instant>>> = Arc::default();
        let (s, g, st, d) = (samples.clone(), gpu.clone(), stop.clone(), detail.clone());
        std::thread::Builder::new()
            .name("gpu-sampler".into())
            .spawn(move || {
                let backend: Option<Box<dyn Backend>> = if demo {
                    Some(Box::new(Demo::new()))
                } else {
                    super::load_drivers()
                };
                let Some(mut backend) = backend else { return };
                let mut current = usize::MAX;
                while !st.load(Ordering::Relaxed) {
                    let index = g.load(Ordering::Relaxed);
                    let want = d
                        .lock()
                        .map(|t| t.is_some_and(|t| t.elapsed() < Duration::from_secs(3)))
                        .unwrap_or(true);
                    backend.set_detail(want);
                    let m = backend.monitor(index);
                    {
                        let mut s = s.lock().unwrap();
                        if index != current {
                            s.history.clear();
                            current = index;
                        }
                        if s.history.len() >= HISTORY {
                            s.history.pop_front();
                        }
                        s.history.push_back(m);
                        s.seq += 1;
                    }
                    std::thread::sleep(Duration::from_millis(1000));
                }
            })
            .ok();
        Self {
            samples,
            gpu,
            stop,
            detail,
        }
    }

    /// Widok Informacje jest na ekranie - włącza szczegółowe odczyty.
    pub fn want_detail(&self) {
        if let Ok(mut t) = self.detail.lock() {
            *t = Some(Instant::now());
        }
    }

    pub fn set_gpu(&self, index: usize) {
        self.gpu.store(index, Ordering::Relaxed);
    }

    pub fn latest(&self) -> Option<Monitor> {
        self.samples.lock().unwrap().latest().cloned()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
