//! Wyłączanie podświetlenia LED pamięci RAM (kontrolery ENE na SMBus).
//!
//! Moduły RAM z podświetleniem RGB (m.in. G.Skill Trident Z RGB, wiele
//! modułów ASUS/ENE) mają kontroler ENE na magistrali SMBus chipsetu. Dostęp
//! do SMBus daje oficjalny moduł PawnIO `SmbusI801.bin` (chipsety Intel), pod
//! blokadą "Global\Access_SMBUS.HTP.Method", tak jak w LibreHardwareMonitor,
//! HWiNFO i OpenRGB. Wymaga uprawnień administratora i sterownika PawnIO.
//!
//! Protokół ENE (fakty z publicznej dokumentacji OpenRGB, kod napisany od
//! zera): rejestr 16-bitowy wybiera się zapisem słowa (bajty zamienione) do
//! komendy 0x00, wartość czyta się bajtem z komendy 0x81, a zapisuje bajtem
//! do komendy 0x01. Podpis kontrolera: rejestry SMBus 0xA0..0xAF zwracają
//! 0x00..0x0F.
//!
//! Bezpieczeństwo:
//! - piszemy wyłącznie pod adresy 0x70..0x77 i tylko do kontrolera, który
//!   przeszedł test podpisu (sam odczyt); pamięć SPD modułów (0x50..0x57)
//!   nie jest nigdy adresowana,
//! - nie przemapowujemy adresów: po włączeniu komputera wszystkie moduły
//!   odpowiadają wspólnie pod 0x77 i jeden zapis wyłącza je wszystkie,
//! - nie zapisujemy niczego w pamięci flash kontrolera (bez "Save") - po
//!   restarcie moduły wracają do swojego trybu, a program wyłącza je
//!   ponownie przy starcie,
//! - poprzedni tryb jest zapamiętywany, żeby można go było przywrócić.

use std::path::Path;

use crate::cpu::pawnio::{NamedLock, PawnIo};

const SMBUS_LOCK: &str = "Global\\Access_SMBUS.HTP.Method";

/// Adresy kontrolerów ENE pamięci RAM (0x77 = adres po włączeniu zasilania).
const ENE_ADDRESSES: std::ops::RangeInclusive<u8> = 0x70..=0x77;

const REG_DEVICE_NAME: u16 = 0x1000;
const REG_MICRON_CHECK: u16 = 0x1030;
const REG_DIRECT: u16 = 0x8020;
const REG_MODE: u16 = 0x8021;
const REG_APPLY: u16 = 0x80A0;
const APPLY: u8 = 0x01;
const MODE_OFF: u8 = 0;

// Protokoły SMBus modułu SmbusI801 (jak w Linuksie).
const READ: u64 = 1;
const WRITE: u64 = 0;
const PROTO_BYTE: u64 = 1;
const PROTO_BYTE_DATA: u64 = 2;
const PROTO_WORD_DATA: u64 = 3;

/// Magistrala SMBus przez PawnIO.
pub struct Smbus {
    io: PawnIo,
}

impl Smbus {
    pub fn open(module: &Path) -> Result<Self, String> {
        Ok(Self {
            io: PawnIo::open(module)?,
        })
    }

    fn xfer(
        &self,
        addr: u8,
        rw: u64,
        cmd: u8,
        proto: u64,
        data: Option<u64>,
    ) -> Result<u64, String> {
        let mut input = vec![addr as u64, rw, cmd as u64, proto];
        if let Some(d) = data {
            input.push(d);
        }
        let mut out = [0u64; 1];
        self.io.call(b"ioctl_smbus_xfer\0", &input, &mut out)?;
        Ok(out[0])
    }

    fn read_byte(&self, addr: u8) -> Result<u8, String> {
        self.xfer(addr, READ, 0, PROTO_BYTE, None).map(|v| v as u8)
    }

    fn read_byte_data(&self, addr: u8, cmd: u8) -> Result<u8, String> {
        self.xfer(addr, READ, cmd, PROTO_BYTE_DATA, None)
            .map(|v| v as u8)
    }

    fn write_byte_data(&self, addr: u8, cmd: u8, value: u8) -> Result<(), String> {
        self.xfer(addr, WRITE, cmd, PROTO_BYTE_DATA, Some(value as u64))
            .map(|_| ())
    }

    fn write_word_data(&self, addr: u8, cmd: u8, value: u16) -> Result<(), String> {
        self.xfer(addr, WRITE, cmd, PROTO_WORD_DATA, Some(value as u64))
            .map(|_| ())
    }
}

/// Kontroler ENE pod danym adresem.
struct Ene<'a> {
    bus: &'a Smbus,
    addr: u8,
}

impl Ene<'_> {
    fn select(&self, reg: u16) -> Result<(), String> {
        // Rejestr podaje się z zamienionymi bajtami.
        self.bus.write_word_data(self.addr, 0x00, reg.swap_bytes())
    }

    fn read(&self, reg: u16) -> Result<u8, String> {
        self.select(reg)?;
        self.bus.read_byte_data(self.addr, 0x81)
    }

    fn write(&self, reg: u16, value: u8) -> Result<(), String> {
        debug_assert!(ENE_ADDRESSES.contains(&self.addr));
        self.select(reg)?;
        self.bus.write_byte_data(self.addr, 0x01, value)
    }

    fn text(&self, reg: u16) -> Result<String, String> {
        let mut bytes = Vec::with_capacity(16);
        for i in 0..16 {
            let b = self.read(reg + i)?;
            if b == 0 {
                break;
            }
            bytes.push(b);
        }
        Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
    }
}

/// Test podpisu kontrolera ENE - wyłącznie odczyty.
fn has_ene_signature(bus: &Smbus, addr: u8) -> bool {
    if bus.read_byte(addr).is_err() && bus.read_byte_data(addr, 0x00).is_err() {
        return false;
    }
    (0xA0u8..0xB0).all(|cmd| bus.read_byte_data(addr, cmd) == Ok(cmd - 0xA0))
}

/// Znaleziony kontroler podświetlenia.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub addr: u8,
    pub name: String,
    pub mode: u8,
    pub direct: u8,
}

/// Wynik operacji na wszystkich kontrolerach.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub devices: Vec<Device>,
    pub errors: Vec<String>,
}

/// Plik modułu SmbusI801 obok programu.
pub fn module_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("modules").join("SmbusI801.bin"))
}

fn open_bus() -> Result<Smbus, String> {
    let path = module_path().ok_or("brak ścieżki programu")?;
    Smbus::open(&path)
}

/// Kontrolery ENE pamięci RAM (tylko odczyt).
fn find(bus: &Smbus) -> Vec<Device> {
    let mut out = Vec::new();
    for addr in ENE_ADDRESSES {
        if !has_ene_signature(bus, addr) {
            continue;
        }
        let ene = Ene { bus, addr };
        // Moduły Micron mają podobny kontroler, ale inny protokół - pomijamy.
        if ene.text(REG_MICRON_CHECK).is_ok_and(|t| t == "Micron") {
            continue;
        }
        let name = ene.text(REG_DEVICE_NAME).unwrap_or_default();
        let (Ok(mode), Ok(direct)) = (ene.read(REG_MODE), ene.read(REG_DIRECT)) else {
            continue;
        };
        out.push(Device {
            addr,
            name,
            mode,
            direct,
        });
    }
    out
}

/// Wyszukuje kontrolery podświetlenia RAM (tylko odczyt).
#[cfg_attr(not(test), allow(dead_code))]
pub fn scan() -> Result<Report, String> {
    let bus = open_bus()?;
    let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
    Ok(Report {
        devices: find(&bus),
        errors: Vec::new(),
    })
}

/// Ustawia tryb i tryb bezpośredni, zatwierdza i sprawdza odczytem.
fn set_mode(bus: &Smbus, addr: u8, direct: u8, mode: u8) -> Result<(), String> {
    let ene = Ene { bus, addr };
    ene.write(REG_DIRECT, direct)?;
    ene.write(REG_MODE, mode)?;
    ene.write(REG_APPLY, APPLY)?;
    let read = (ene.read(REG_DIRECT)?, ene.read(REG_MODE)?);
    if read != (direct, mode) {
        return Err(crate::trf!(
            "0x{addr:02X}: po zapisie odczytano tryb {} / {} zamiast {direct} / {mode}",
            "0x{addr:02X}: read back mode {} / {} instead of {direct} / {mode}",
            read.0,
            read.1
        ));
    }
    Ok(())
}

/// Wyłącza podświetlenie wszystkich znalezionych kontrolerów. Zwraca stan
/// sprzed zmiany (do przywrócenia).
pub fn turn_off() -> Result<Report, String> {
    let bus = open_bus()?;
    let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
    let devices = find(&bus);
    let mut report = Report {
        devices: devices.clone(),
        errors: Vec::new(),
    };
    for d in &devices {
        if let Err(e) = set_mode(&bus, d.addr, 0, MODE_OFF) {
            report.errors.push(e);
        }
    }
    Ok(report)
}

/// Przywraca zapamiętane tryby (adres, tryb bezpośredni, tryb).
pub fn restore(saved: &[(u8, u8, u8)]) -> Result<Report, String> {
    let bus = open_bus()?;
    let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
    let devices = find(&bus);
    let mut report = Report {
        devices: devices.clone(),
        errors: Vec::new(),
    };
    for d in &devices {
        let Some(&(_, direct, mode)) = saved.iter().find(|s| s.0 == d.addr) else {
            continue;
        };
        if let Err(e) = set_mode(&bus, d.addr, direct, mode) {
            report.errors.push(e);
        }
    }
    Ok(report)
}

/// Wynik operacji wykonanej w tle.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// Wyłączono; zawiera stan sprzed wyłączenia.
    Off(Report),
    Restored(Report),
    Failed(String),
}

/// Operacje na LED w osobnym wątku (SMBus i ładowanie modułu trwają chwilę).
#[derive(Default)]
pub struct Control {
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    result: std::sync::Arc<std::sync::Mutex<Option<Outcome>>>,
}

impl Control {
    fn spawn(
        &self,
        job: impl FnOnce() -> Outcome + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) {
        if self.busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let (busy, result) = (self.busy.clone(), self.result.clone());
        std::thread::spawn(move || {
            let outcome = job();
            *result.lock().unwrap() = Some(outcome);
            busy.store(false, std::sync::atomic::Ordering::SeqCst);
            wake();
        });
    }

    pub fn turn_off(&self, wake: impl Fn() + Send + 'static) {
        self.spawn(
            || turn_off().map_or_else(Outcome::Failed, Outcome::Off),
            wake,
        );
    }

    pub fn restore(&self, saved: Vec<(u8, u8, u8)>, wake: impl Fn() + Send + 'static) {
        self.spawn(
            move || restore(&saved).map_or_else(Outcome::Failed, Outcome::Restored),
            wake,
        );
    }

    pub fn busy(&self) -> bool {
        self.busy.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Wynik ostatniej operacji (raz).
    pub fn take(&self) -> Option<Outcome> {
        self.result.lock().unwrap().take()
    }
}

#[cfg(test)]
mod hardware {
    //! Odczyt kontrolerów LED pamięci RAM (bez zapisu):
    //! `cargo test leds_scan -- --ignored --nocapture` (jako administrator,
    //! moduł w target\release\modules\SmbusI801.bin).
    #[test]
    #[ignore]
    fn leds_scan() {
        match super::scan() {
            Ok(r) => {
                for d in &r.devices {
                    println!(
                        "0x{:02X}: \"{}\" tryb {} direct {}",
                        d.addr, d.name, d.mode, d.direct
                    );
                }
                println!("kontrolerów: {}", r.devices.len());
            }
            Err(e) => println!("błąd: {e}"),
        }
    }
}

#[cfg(test)]
mod hardware_write {
    //! Wyłączenie i przywrócenie podświetlenia RAM (zapis!):
    //! `cargo test leds_off_on -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn leds_off_on() {
        let before = super::turn_off().expect("wyłączenie");
        println!("przed: {:?}, błędy: {:?}", before.devices, before.errors);
        let after = super::scan().unwrap();
        println!("po wyłączeniu: {:?}", after.devices);
        std::thread::sleep(std::time::Duration::from_secs(5));
        let saved: Vec<(u8, u8, u8)> = before
            .devices
            .iter()
            .map(|d| (d.addr, d.direct, d.mode))
            .collect();
        let r = super::restore(&saved).expect("przywrócenie");
        println!("przywrócono, błędy: {:?}", r.errors);
        println!("teraz: {:?}", super::scan().unwrap().devices);
    }
}
