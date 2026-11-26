//! Usługa czujników w procesie programu: zbiera odczyty z procesora, pamięci,
//! baterii i układów Super I/O płyty, steruje wentylatorami płyty i oddaje je
//! płycie przy zamknięciu programu - także po awarii (`emergency_restore`).

use std::sync::{Arc, Mutex, Weak};

use crate::bridge::Snapshot;

use super::battery::Battery;
use super::cpu::Cpu;
use super::memory::Memory;
use super::superio::{self, Chip, Lpc};
use super::Readings;

pub struct Service {
    /// Układy Super I/O, każdy z własną instancją modułu LpcIO.
    chips: Vec<(Lpc, Box<dyn Chip>)>,
    cpu: Option<Cpu>,
    memory: Option<Memory>,
    battery: Option<Battery>,
    /// Strefy ACPI z Windows - gdy układu płyty nie da się odczytać.
    acpi: super::acpi::Acpi,
    /// Czy któryś wentylator płyty dostał ustawienie (do przywrócenia).
    touched: bool,
    /// Uwagi z uruchomienia (brak modułu, brak uprawnień) - do diagnostyki.
    pub notes: Vec<String>,
    /// Cykle do ponownego wykrycia układów płyty, gdy żadnego nie znaleziono.
    redetect_in: u32,
}

/// Uruchomiona usługa - dla przywracania wentylatorów po awarii programu.
static ACTIVE: Mutex<Option<Weak<Mutex<Service>>>> = Mutex::new(None);

impl Service {
    /// Otwiera wszystko, co się da; brak jednego źródła nie blokuje reszty.
    pub fn open() -> Arc<Mutex<Service>> {
        let mut notes = Vec::new();
        let chips = match super::module_path("LpcIO.bin").map(|p| superio::detect(&p)) {
            Some(Ok(c)) => {
                if c.is_empty() {
                    notes.push("Super I/O: nie rozpoznano układu / no chip recognized".into());
                }
                c
            }
            Some(Err(e)) => {
                notes.push(format!("LpcIO: {e}"));
                Vec::new()
            }
            None => Vec::new(),
        };
        let service = Arc::new(Mutex::new(Service {
            chips,
            cpu: Cpu::open(),
            memory: Memory::open(),
            battery: Battery::open(),
            acpi: Default::default(),
            touched: false,
            notes,
            redetect_in: 30,
        }));
        if let Ok(mut a) = ACTIVE.lock() {
            *a = Some(Arc::downgrade(&service));
        }
        service
    }

    /// Jeden cykl odczytów w formacie procesu czujników.
    pub fn read(&mut self) -> Snapshot {
        let mut r = Readings::default();
        // Układu płyty nie znaleziono (np. blokadę trzymał inny program) -
        // ponowna próba co 30 s.
        if self.chips.is_empty() {
            self.redetect_in = self.redetect_in.saturating_sub(1);
            if self.redetect_in == 0 {
                self.redetect_in = 30;
                if let Some(Ok(c)) = super::module_path("LpcIO.bin").map(|p| superio::detect(&p)) {
                    self.chips = c;
                }
            }
        }
        if let Some(c) = self.cpu.as_mut() {
            c.read(&mut r);
        }
        if let Some(m) = self.memory.as_mut() {
            m.read(&mut r);
        }
        if let Some(b) = self.battery.as_mut() {
            b.read(&mut r);
        }
        for (lpc, chip) in &mut self.chips {
            // Blokadę trzyma inny program albo odczyt się nie udał: zostają
            // poprzednie wartości (krzywe nie tracą na chwilę czujnika).
            if let Ok(_lock) = lpc.lock() {
                let _ = chip.update(lpc);
            }
            chip.readings(&mut r);
        }
        // Układ płyty nie dał żadnej temperatury (brak układu, firmware
        // producenta): temperatury stref termicznych ACPI z Windows.
        if !r.temps.iter().any(|t| t.kind == "SuperIO" && t.value.is_some()) {
            self.acpi.read(&mut r);
        }
        Snapshot {
            temps: r.temps,
            fans: r.fans,
            speeds: r.speeds,
            powers: r.powers,
            admin: crate::elevation::is_elevated(),
            error: None,
        }
    }

    /// Ustawia wentylator o identyfikatorze `id` (`.../control/<n>`) na
    /// `percent` albo oddaje go płycie (`None`).
    pub fn set(&mut self, id: &str, percent: Option<f32>) -> Result<(), String> {
        let (prefix, index) = id.rsplit_once("/control/").ok_or("zły identyfikator / bad id")?;
        let index: usize = index.parse().map_err(|_| "zły identyfikator / bad id")?;
        let (lpc, chip) = self
            .chips
            .iter_mut()
            .find(|(_, c)| c.id() == prefix)
            .ok_or("nie ma takiego układu / no such chip")?;
        if chip.read_only() {
            return Err("układ tylko do odczytu / read-only chip".into());
        }
        let _lock = lpc.lock()?;
        chip.set_control(lpc, index, percent)?;
        if percent.is_some() {
            self.touched = true;
        }
        Ok(())
    }

    /// Oddaje płycie wszystkie wentylatory.
    pub fn restore_all(&mut self) {
        if !self.touched {
            return;
        }
        // Przy zamykaniu czekamy dłużej na blokadę niż przy odczycie.
        let lock = crate::cpu::pawnio::NamedLock::acquire(superio::ISA_LOCK, 2000);
        for (lpc, chip) in &mut self.chips {
            chip.restore_all(lpc);
        }
        drop(lock);
        self.touched = false;
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.restore_all();
    }
}

/// Awaria programu (panic, nieobsłużony wyjątek): oddaje wentylatory płycie,
/// jeśli usługa działa i nie jest akurat zablokowana przez wątek, który padł.
pub fn emergency_restore() {
    let Ok(active) = ACTIVE.try_lock() else { return };
    let Some(service) = active.as_ref().and_then(Weak::upgrade) else { return };
    let guard = service.try_lock();
    if let Ok(mut s) = guard {
        s.restore_all();
    }
}

#[cfg(test)]
mod live {
    //! Pełny odczyt usługi (bez zapisu do wentylatorów):
    //! `cargo test service_live -- --ignored --nocapture`. Moduły z
    //! `dist\modules` (test uruchamia się spoza folderu programu).
    use super::*;

    #[test]
    #[ignore]
    fn service_live() {
        std::env::set_var("TC_MODULES", r"D:\tune-center-main\dist\modules");
        let service = Service::open();
        let mut s = service.lock().unwrap();
        let _ = s.read();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let snap = s.read();
        println!("notes {:?}", s.notes);
        for t in &snap.temps {
            println!("T {} | {} | {} | {} | {:?}", t.id, t.name, t.hw, t.kind, t.value);
        }
        for f in &snap.fans {
            println!("F {} | {} | {} | {:?} rpm {:?} %", f.id, f.name, f.hw, f.rpm, f.percent);
        }
        for p in &snap.powers {
            println!("P {} | {} | {} | {:?}", p.name, p.hw, p.kind, p.value);
        }
    }
}

#[cfg(test)]
mod live_write {
    //! Zapis jednego wentylatora: 40 % na 3 s, potem stan płyty. Zmienia
    //! sterowanie wentylatorem! `cargo test service_fan_write -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn service_fan_write() {
        std::env::set_var("TC_MODULES", r"D:\tune-center-main\dist\modules");
        let id = std::env::var("TC_FAN").unwrap_or_else(|_| "/lpc/nct6796dr/0/control/2".into());
        let service = Service::open();
        let mut s = service.lock().unwrap();
        let pct = |s: &mut Service| s.read().fans.iter().find(|f| f.id == id).map(|f| (f.rpm, f.percent));
        println!("przed {:?}", pct(&mut s));
        println!("ustaw 40: {:?}", s.set(&id, Some(40.0)));
        std::thread::sleep(std::time::Duration::from_secs(3));
        println!("po 3 s {:?}", pct(&mut s));
        println!("oddaj: {:?}", s.set(&id, None));
        std::thread::sleep(std::time::Duration::from_secs(2));
        println!("po oddaniu {:?}", pct(&mut s));
    }
}
