//! Wentylatory kart graficznych sterowane wprost przez sterownik karty
//! (NVAPI dla Nvidii, ADL dla AMD), bez procesu czujników.
//!
//! Osobny wątek co sekundę czyta obroty i temperaturę karty, liczy
//! wysterowanie z ustawień (ręczne albo krzywa) i wysyła je do sterownika.
//! Działa także przy oknie schowanym w zasobniku.
//!
//! Zabezpieczenia:
//! - wentylator w trybie Auto nie jest w ogóle dotykany,
//! - gdy karta osiągnie `OVERHEAT_C`, sterowanie wraca do automatu karty,
//! - gdy nie ma odczytu temperatury dla krzywej, sterowanie wraca do automatu,
//! - przy zamykaniu programu wszystkie karty wracają do automatu.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::bridge::{self, Fan, Snapshot, Temp};
use crate::config::{FanConfig, Fans, MIN_PCT};
use crate::gpu::backend::Vendor;

/// Od tej temperatury karty (°C) program oddaje wentylator automatowi karty,
/// niezależnie od ustawień.
pub const OVERHEAT_C: f32 = 88.0;

/// Najnowsze temperatury z procesu czujników, dla krzywych opartych np. na
/// temperaturze procesora.
pub type TempBoard = Arc<Mutex<Vec<Temp>>>;

#[derive(Default)]
struct Shared {
    config: Fans,
    fans: Vec<Fan>,
    temps: Vec<Temp>,
    error: Option<String>,
}

pub struct GpuFans {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Typy kart obsługiwanych tą drogą ("GpuNvidia", "GpuAmd"); proces
    /// czujników nie steruje wtedy ich wentylatorami.
    kinds: Vec<&'static str>,
}

fn kind_of(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Nvidia => "GpuNvidia",
        Vendor::Amd => "GpuAmd",
    }
}

/// Wysterowanie do wysłania karcie: `Some(procent)` albo `None` (automat karty).
pub fn decide(cfg: Option<&FanConfig>, temps: &[Temp], card_temp: Option<f32>) -> Option<u32> {
    if card_temp.is_some_and(|t| t >= OVERHEAT_C) {
        return None;
    }
    bridge::target(cfg, temps).map(|p| p.clamp(MIN_PCT, 100.0).round() as u32)
}

impl GpuFans {
    /// Uruchamia sterowanie. Zwraca `None`, gdy żadna karta nie pozwala
    /// sterować wentylatorem przez sterownik - wtedy zostaje proces czujników.
    pub fn start(config: Fans, board: TempBoard, wake: impl Fn() + Send + 'static) -> Option<Self> {
        let shared = Arc::new(Mutex::new(Shared {
            config,
            ..Shared::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Vec<&'static str>>();

        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread = crate::spawn("gpu-fans", move || {
            run(thread_shared, thread_stop, board, ready_tx, wake);
        });

        // Sterowniki kart ładują się w wątku sterującym (uchwyty sterownika
        // zostają w jednym wątku); tu czekamy tylko na listę kart.
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(kinds) if !kinds.is_empty() => Some(Self {
                shared,
                stop,
                thread: Some(thread),
                kinds,
            }),
            _ => {
                stop.store(true, Ordering::Relaxed);
                None
            }
        }
    }

    pub fn kinds(&self) -> &[&'static str] {
        &self.kinds
    }

    /// Podmienia ustawienia wentylatorów (zmiana w zakładce FAN albo profilu).
    pub fn set_config(&self, config: Fans) {
        if let Ok(mut s) = self.shared.lock() {
            s.config = config;
        }
    }

    /// Ostatni błąd sterownika przy ustawianiu wentylatora.
    pub fn error(&self) -> Option<String> {
        self.shared.lock().ok()?.error.clone()
    }

    /// Dokłada wentylatory i temperatury kart do stanu z procesu czujników
    /// oraz usuwa z niego wentylatory tych samych kart, żeby każdy był na
    /// liście raz. Działa także bez procesu czujników.
    pub fn merge(&self, snap: Option<Snapshot>) -> Snapshot {
        let mut snap = snap.unwrap_or_else(|| Snapshot {
            admin: true,
            ..Snapshot::default()
        });
        snap.fans.retain(|f| !self.kinds.contains(&f.kind.as_str()));
        if let Ok(s) = self.shared.lock() {
            snap.fans.extend(s.fans.iter().cloned());
            snap.temps.extend(s.temps.iter().cloned());
        }
        snap
    }
}

impl Drop for GpuFans {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // Wątek oddaje karty automatowi i kończy; pętla sprawdza flagę co 100 ms.
            let _ = thread.join();
        }
    }
}

struct Card {
    index: usize,
    id: String,
    name: String,
    kind: &'static str,
}

fn run(
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    board: TempBoard,
    ready: mpsc::Sender<Vec<&'static str>>,
    wake: impl Fn(),
) {
    let Some(mut backend) = crate::gpu::load_drivers() else {
        let _ = ready.send(Vec::new());
        return;
    };

    let mut cards = Vec::new();
    for (index, name) in backend.gpus().into_iter().enumerate() {
        if backend.fan_controllable(index) {
            cards.push(Card {
                index,
                id: format!("driver-gpu-{index}"),
                name,
                kind: kind_of(backend.vendor(index)),
            });
        }
    }
    let mut kinds: Vec<&'static str> = cards.iter().map(|c| c.kind).collect();
    kinds.sort_unstable();
    kinds.dedup();
    if ready.send(kinds).is_err() || cards.is_empty() {
        return;
    }

    // Ostatnio wysłane wysterowanie: Some(p) = wymuszone, brak wpisu = automat.
    let mut applied: HashMap<usize, u32> = HashMap::new();

    while !stop.load(Ordering::Relaxed) {
        let mut fans = Vec::new();
        let mut driver_temps = Vec::new();
        let mut card_temps = Vec::new();
        for card in &cards {
            let status = backend.fan(card.index).unwrap_or_default();
            let temp = backend.temperature(card.index).map(|t| t as f32);
            card_temps.push(temp);
            fans.push(Fan {
                id: card.id.clone(),
                name: "GPU Fan".to_owned(),
                hw: card.name.clone(),
                kind: card.kind.to_owned(),
                rpm: status.rpm.map(|r| r as f32),
                percent: status.pct.map(|p| p as f32),
            });
            driver_temps.push(Temp {
                id: format!("{}/temp", card.id),
                name: "GPU Core".to_owned(),
                hw: card.name.clone(),
                kind: card.kind.to_owned(),
                value: temp,
            });
        }

        // Krzywa może korzystać z temperatury karty albo z czujników procesu.
        let mut all_temps = driver_temps.clone();
        if let Ok(b) = board.lock() {
            all_temps.extend(b.iter().cloned());
        }
        let config = shared.lock().map(|s| s.config.clone()).unwrap_or_default();

        let mut error = None;
        for (card, card_temp) in cards.iter().zip(&card_temps) {
            let wanted = decide(config.get(&card.id), &all_temps, *card_temp);
            let current = applied.get(&card.index).copied();
            if wanted == current {
                continue;
            }
            match backend.set_fan(card.index, wanted) {
                Ok(()) => match wanted {
                    Some(p) => {
                        applied.insert(card.index, p);
                    }
                    None => {
                        applied.remove(&card.index);
                    }
                },
                Err(e) => error = Some(e),
            }
        }

        if let Ok(mut s) = shared.lock() {
            s.fans = fans;
            s.temps = driver_temps;
            s.error = error;
        }
        wake();

        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // Koniec pracy: każda karta, którą sterowaliśmy, wraca do automatu.
    for index in applied.keys() {
        let _ = backend.set_fan(*index, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;

    fn temp(id: &str, value: f32) -> Temp {
        Temp {
            id: id.to_owned(),
            name: "GPU Core".to_owned(),
            hw: "GPU".to_owned(),
            kind: "GpuNvidia".to_owned(),
            value: Some(value),
        }
    }

    fn cfg(mode: Mode) -> FanConfig {
        FanConfig {
            mode,
            manual: 60.0,
            source: Some("t".to_owned()),
            points: vec![[40.0, 30.0], [80.0, 100.0]],
        }
    }

    #[test]
    fn decides_fan_level() {
        let temps = [temp("t", 60.0)];
        // Auto i brak ustawień: karta steruje sama.
        assert_eq!(decide(None, &temps, Some(60.0)), None);
        assert_eq!(decide(Some(&cfg(Mode::Auto)), &temps, Some(60.0)), None);
        // Ręcznie i krzywa.
        assert_eq!(
            decide(Some(&cfg(Mode::Manual)), &temps, Some(60.0)),
            Some(60)
        );
        assert_eq!(
            decide(Some(&cfg(Mode::Curve)), &temps, Some(60.0)),
            Some(65)
        );
        // Dolna granica wysterowania.
        let mut low = cfg(Mode::Manual);
        low.manual = 5.0;
        assert_eq!(decide(Some(&low), &temps, Some(60.0)), Some(MIN_PCT as u32));
        // Krzywa bez odczytu temperatury: automat.
        assert_eq!(decide(Some(&cfg(Mode::Curve)), &[], Some(60.0)), None);
        // Przegrzanie: automat niezależnie od ustawień.
        assert_eq!(
            decide(Some(&cfg(Mode::Manual)), &temps, Some(OVERHEAT_C)),
            None
        );
        assert_eq!(decide(Some(&cfg(Mode::Manual)), &temps, None), Some(60));
    }
}
