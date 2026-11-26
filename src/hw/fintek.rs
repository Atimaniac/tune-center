//! Układy Fintek F718xx (starsze płyty EVGA, MSI, ASRock).
//!
//! Tryb konfiguracji: 0x87 0x87 (jak Nuvoton), wyjście: 0xAA. Monitor sprzętu
//! (logiczne urządzenie 4, w F71858 urządzenie 2) ma okno rejestrów pod
//! adresem bazowym: indeks `base + 5`, dane `base + 6`. Rejestry, przeliczenia
//! i kolejność zapisów jak w LibreHardwareMonitor (`F718XX.cs`, `LpcIO.cs`),
//! sprawdzone z linuksowym `f71882fg`.
//!
//! Cały dostęp do portów (także `probe`) wymaga blokady ISA trzymanej przez
//! wołającego (`Lpc::lock`).

use std::time::Duration;

use super::boards::{self, BoardNames, Counts};
use super::ite::{default_names, names_readings, slot_port, valid_base, SioIo};
use super::superio::{Board, Chip, Lpc};
use super::{valid_temp, Readings};

// Rejestry konfiguracji.
const CHIP_ID: u8 = 0x20;
const CHIP_REVISION: u8 = 0x21;
const VENDOR_ID: u8 = 0x23;
const LDN_SELECT: u8 = 0x07;
const BASE_ADDRESS: u8 = 0x60;
const FINTEK_VENDOR: u16 = 0x1934;

// Rejestry monitora sprzętu.
const TEMP_CONFIG: u8 = 0x69;
const TEMP_BASE: u8 = 0x70;
/// Dwa bity na wentylator: 2n = wypełnienie (1) / obroty (0), 2n+1 = ręcznie (1) / automat (0).
const FAN_PWM_ENABLE: u8 = 0x96;
/// Bit 7 przełącza rejestry wentylatorów na drugi bank.
const FAN_FAULT_T: u8 = 0x9F;
const FAN_PROG_SEL: u8 = 0x80;
const FAN_TACH: [u8; 4] = [0xA0, 0xB0, 0xC0, 0xD0];
/// Cel obrotów (słowo); młodszy bajt to rejestr wypełnienia PWM.
const FAN_TARGET: [u8; 4] = [0xA2, 0xB2, 0xC2, 0xD2];
const FAN_PWM: [u8; 4] = [0xA3, 0xB3, 0xC3, 0xD3];
const FAN_FULL_SPEED: [u8; 4] = [0xA4, 0xB4, 0xC4, 0xD4];

const MAX_FANS: usize = 4;

/// Model układu: ID i wersja, nazwy jak w LibreHardwareMonitor, liczby czujników.
#[derive(Debug)]
struct Model {
    id: u8,
    revision: u8,
    /// Nazwa w identyfikatorze (`Chip` w LHM), np. "F71882".
    key: &'static str,
    /// Nazwa wyświetlana bez "Fintek ".
    name: &'static str,
    ldn: u8,
    temps: usize,
    fans: usize,
    /// LHM steruje tylko F71882 i układami z rejestrem trybu 0x96.
    controls: usize,
    /// Sterowanie przez tryb w 0x96 (F71878AD, F71889AD, F71889ED).
    pwm_enable: bool,
}

const fn model(id: u8, revision: u8, key: &'static str) -> Model {
    Model { id, revision, key, name: key, ldn: 0x04, temps: 3, fans: 3, controls: 0, pwm_enable: false }
}

static MODELS: [Model; 10] = [
    Model { ldn: 0x02, fans: 4, ..model(0x05, 0x07, "F71858") },
    Model { fans: 4, controls: 4, ..model(0x05, 0x41, "F71882") },
    model(0x06, 0x01, "F71862"),
    model(0x07, 0x23, "F71889F"),
    model(0x08, 0x14, "F71869"),
    Model { temps: 2, ..model(0x09, 0x01, "F71808E") },
    Model { controls: 3, pwm_enable: true, ..model(0x09, 0x09, "F71889ED") },
    Model { controls: 3, pwm_enable: true, ..model(0x10, 0x05, "F71889AD") },
    Model { name: "F71869A/F71811", ..model(0x10, 0x07, "F71869A") },
    Model { controls: 3, pwm_enable: true, ..model(0x11, 0x06, "F71878AD") },
];

fn find_model(id: u8, revision: u8) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id && m.revision == revision)
}

/// Rozpoznanie układu Fintek w wybranym gnieździe (blokada ISA po stronie wołającego).
pub fn probe(lpc: &mut Lpc, slot: u8, index: usize) -> Option<Box<dyn Chip>> {
    let mut chip = probe_io(lpc, slot)?;
    chip.apply_board(&Board::from_registry(), index);
    Some(Box::new(chip))
}

fn probe_io<P: SioIo + ?Sized>(io: &P, slot: u8) -> Option<Fintek> {
    let port = io.config_port();
    if slot > 1 || port != slot_port(slot) {
        return None;
    }
    // Klucz 0x87 0x87 otwiera też Nuvoton/Winbond - 0xAA zamyka każdy z nich;
    // układ ITE (klucz 0x87 0x01 0x55 ..) traktuje te bajty jak zapis indeksu.
    let entered = io.outb(port, 0x87).and_then(|_| io.outb(port, 0x87));
    let found = entered.ok().and_then(|_| configure(io));
    let _ = io.outb(port, 0xAA);
    let (model, address, verify, vendor) = found?;
    if address != verify || vendor != FINTEK_VENDOR {
        return None;
    }
    // Niektóre układy podają adres z dodanym już przesunięciem 5.
    let base = if address & 0x07 == 0x05 { address & 0xFFF8 } else { address };
    if !valid_base(base, base) {
        return None;
    }
    Some(Fintek::new(model, base))
}

/// W trybie konfiguracji: ID, BAR-y, adres bazowy (dwa odczyty) i ID producenta.
fn configure<P: SioIo + ?Sized>(io: &P) -> Option<(&'static Model, u16, u16, u16)> {
    let id = io.sio_inb(CHIP_ID).ok()?;
    let revision = io.sio_inb(CHIP_REVISION).ok()?;
    let model = find_model(id, revision)?;
    io.find_bars().ok()?;
    io.sio_outb(LDN_SELECT, model.ldn).ok()?;
    let address = io.sio_inw(BASE_ADDRESS).ok()?;
    std::thread::sleep(Duration::from_millis(1));
    let verify = io.sio_inw(BASE_ADDRESS).ok()?;
    let vendor = io.sio_inw(VENDOR_ID).ok()?;
    Some((model, address, verify, vendor))
}

/// Stan sterowania sprzed pierwszej zmiany.
#[derive(Clone, Copy, Debug)]
enum Saved {
    /// F71882: sam rejestr PWM.
    Pwm(u8),
    /// Bity trybu w 0x96 i słowo celu (obejmuje rejestr PWM).
    Mode { enable: u8, target: u16 },
}

pub struct Fintek {
    model: &'static Model,
    base: u16,
    temps: Vec<Option<f32>>,
    fans: Vec<Option<f32>>,
    controls: Vec<Option<f32>>,
    saved: [Option<Saved>; MAX_FANS],
    /// Oryginalny 0x9F, gdy bank wentylatorów został przełączony na 0.
    fault_t: Option<u8>,
    /// Czujniki pokazywane w LHM (domyślne albo dla płyty).
    names: BoardNames,
}

impl Fintek {
    fn new(model: &'static Model, base: u16) -> Self {
        Self {
            model,
            base,
            temps: vec![None; model.temps],
            fans: vec![None; model.fans],
            controls: vec![None; model.controls],
            saved: [None; MAX_FANS],
            fault_t: None,
            names: default_names(model.temps, model.fans, model.controls),
        }
    }

    /// Nazwy czujników dla płyty (`boards.rs`); bez listy - zostają domyślne.
    fn apply_board(&mut self, board: &Board, index: usize) {
        let counts = Counts { temps: self.model.temps, fans: self.model.fans, controls: self.model.controls };
        if let Some(names) = boards::names(self.model.key, &board.manufacturer, &board.product, index, counts) {
            self.names = names;
        }
    }

    fn read<P: SioIo + ?Sized>(&self, io: &P, reg: u8) -> Result<u8, String> {
        io.outb(self.base + 5, reg)?;
        io.inb(self.base + 6)
    }

    fn write<P: SioIo + ?Sized>(&self, io: &P, reg: u8, value: u8) -> Result<(), String> {
        io.outb(self.base + 5, reg)?;
        io.outb(self.base + 6, value)
    }

    fn read_word<P: SioIo + ?Sized>(&self, io: &P, reg: u8) -> Result<u16, String> {
        Ok(u16::from(self.read(io, reg)?) << 8 | u16::from(self.read(io, reg.wrapping_add(1))?))
    }

    fn write_word<P: SioIo + ?Sized>(&self, io: &P, reg: u8, value: u16) -> Result<(), String> {
        self.write(io, reg, (value >> 8) as u8)?;
        self.write(io, reg.wrapping_add(1), value as u8)
    }

    fn update_io<P: SioIo + ?Sized>(&mut self, io: &P) -> Result<(), String> {
        let mut temps = std::mem::take(&mut self.temps);
        let result = self.read_temps(io, &mut temps);
        self.temps = temps;
        result?;
        for i in 0..self.fans.len() {
            let Some(&reg) = FAN_TACH.get(i) else { continue };
            let count = self.read_word(io, reg)?;
            self.fans[i] = fan_rpm(count);
        }
        let enable = if self.model.pwm_enable { self.read(io, FAN_PWM_ENABLE)? } else { 0 };
        for i in 0..self.controls.len() {
            let Some(&pwm) = FAN_PWM.get(i) else { continue };
            let duty = |v: u8| f32::from(v) * 100.0 / 255.0;
            self.controls[i] = Some(if !self.model.pwm_enable || enable & (1 << (2 * i)) != 0 {
                duty(self.read(io, pwm)?)
            } else {
                // Tryb obrotów: oba rejestry to liczniki (1,5 MHz / obroty),
                // wypełnienie = pełne obroty / cel.
                let (Some(&t), Some(&f)) = (FAN_TARGET.get(i), FAN_FULL_SPEED.get(i)) else { continue };
                let target = self.read_word(io, t)?;
                let full = self.read_word(io, f)?;
                if target == 0 || full == 0 {
                    0.0
                } else {
                    (f32::from(full) * 100.0 / f32::from(target)).min(100.0)
                }
            });
        }
        Ok(())
    }

    fn read_temps<P: SioIo + ?Sized>(&self, io: &P, temps: &mut [Option<f32>]) -> Result<(), String> {
        if self.model.key == "F71858" {
            let mode = self.read(io, TEMP_CONFIG)? & 0x03;
            for (i, t) in temps.iter_mut().enumerate() {
                let reg = TEMP_BASE + 2 * i as u8;
                let high = self.read(io, reg)?;
                let low = self.read(io, reg + 1)?;
                *t = f71858_temp(mode, high, low);
            }
        } else {
            for (i, t) in temps.iter_mut().enumerate() {
                let c = self.read(io, TEMP_BASE + 2 * (i as u8 + 1))? as i8;
                *t = if c > 0 && c < i8::MAX { valid_temp(f32::from(c)) } else { None };
            }
        }
        Ok(())
    }

    fn set_io<P: SioIo + ?Sized>(&mut self, io: &P, index: usize, percent: Option<f32>) -> Result<(), String> {
        if index >= self.model.controls || index >= MAX_FANS {
            return Err(crate::trf!("nie ma sterowania {index}", "no control {index}"));
        }
        let Some(p) = percent else { return self.restore_io(io, index) };
        if !p.is_finite() {
            return Err(crate::i18n::tr("zła wartość wypełnienia", "bad duty value").to_owned());
        }
        let (pwm, target) = (FAN_PWM[index], FAN_TARGET[index]);
        let mask = 0x03u8 << (2 * index);
        // Jak LHM: procent * 2,55 obcięte do bajtu.
        let value = (p.clamp(0.0, 100.0) * 2.55) as u8;
        if self.saved[index].is_none() {
            self.saved[index] = Some(if self.model.pwm_enable {
                if self.fault_t.is_none() {
                    // Zapisy niżej muszą trafić do banku 0.
                    let fault = self.read(io, FAN_FAULT_T)?;
                    self.write(io, FAN_FAULT_T, fault & !FAN_PROG_SEL)?;
                    self.fault_t = Some(fault);
                }
                Saved::Mode { enable: self.read(io, FAN_PWM_ENABLE)? & mask, target: self.read_word(io, target)? }
            } else {
                Saved::Pwm(self.read(io, pwm)?)
            });
        }
        if self.model.pwm_enable {
            // Oba bity: ręczne wypełnienie z rejestru PWM.
            let enable = self.read(io, FAN_PWM_ENABLE)?;
            self.write(io, FAN_PWM_ENABLE, enable | mask)?;
        }
        self.write(io, pwm, value)
    }

    /// Przywraca stan sprzed pierwszej zmiany (no-op, gdy nic nie zmieniono).
    fn restore_io<P: SioIo + ?Sized>(&mut self, io: &P, index: usize) -> Result<(), String> {
        let Some(saved) = self.saved.get(index).copied().flatten() else { return Ok(()) };
        let (Some(&pwm), Some(&target)) = (FAN_PWM.get(index), FAN_TARGET.get(index)) else { return Ok(()) };
        match saved {
            Saved::Pwm(v) => self.write(io, pwm, v)?,
            Saved::Mode { enable: old, target: t } => {
                let mask = 0x03u8 << (2 * index);
                let enable = self.read(io, FAN_PWM_ENABLE)?;
                self.write(io, FAN_PWM_ENABLE, (enable & !mask) | old)?;
                self.write_word(io, target, t)?;
            }
        }
        self.saved[index] = None;
        if let Some(fault) = self.fault_t {
            if self.saved.iter().all(Option::is_none) {
                self.write(io, FAN_FAULT_T, fault)?;
                self.fault_t = None;
            }
        }
        Ok(())
    }
}

/// F71858: 11-bitowa temperatura w dwóch bajtach, znak zależny od trybu tablicy.
/// 0xBB / 0xCC w starszym bajcie = brak czujnika.
fn f71858_temp(mode: u8, high: u8, low: u8) -> Option<f32> {
    if high == 0xBB || high == 0xCC {
        return None;
    }
    let mut bits: u32 = match mode {
        2 => u32::from(high & 0x80) << 8,
        3 => u32::from(low & 0x01) << 15,
        _ => 0,
    };
    bits |= u32::from(high) << 7;
    bits |= u32::from(low & 0xE0) >> 1;
    let raw = (bits & 0xFFF0) as u16 as i16;
    valid_temp(f32::from(raw) / 128.0)
}

/// Licznik okresu (1,5 MHz): >= 0x0FFF = brak impulsów, 0 = brak czujnika.
fn fan_rpm(count: u16) -> Option<f32> {
    (count > 0 && count < 0x0FFF).then(|| 1.5e6 / f32::from(count))
}

impl Chip for Fintek {
    fn name(&self) -> String {
        format!("Fintek {}", self.model.name)
    }

    fn id(&self) -> String {
        // Indeks w LHM liczony w grupie tego samego układu - zawsze 0.
        format!("/lpc/{}/0", self.model.key.to_ascii_lowercase())
    }

    fn update(&mut self, lpc: &Lpc) -> Result<(), String> {
        self.update_io(lpc)
    }

    fn readings(&self, out: &mut Readings) {
        let (id, hw) = (self.id(), self.name());
        names_readings(&self.names, &id, &hw, &self.temps, &self.fans, &self.controls, out);
    }

    fn set_control(&mut self, lpc: &Lpc, index: usize, percent: Option<f32>) -> Result<(), String> {
        self.set_io(lpc, index, percent)
    }

    fn restore_all(&mut self, lpc: &Lpc) {
        for i in 0..self.model.controls {
            let _ = self.restore_io(lpc, i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ite::sim::Sim;
    use super::*;

    const KEY: [u8; 2] = [0x87, 0x87];

    /// Układ Fintek z oknem rejestrów pod 0x295 (przesunięcie 5 już dodane).
    fn fintek(id: u8, revision: u8, ldn: u8) -> Sim {
        let sim = Sim::new(0x2E, &KEY, false, u16::from(id) << 8 | u16::from(revision));
        sim.set_base(ldn, 0x60, 0x295);
        let mut s = sim.0.borrow_mut();
        s.base = 0x290;
        s.global[0x23] = 0x19;
        s.global[0x24] = 0x34;
        drop(s);
        sim
    }

    #[test]
    fn detects_models_and_exits() {
        for (id, rev, ldn, name, key) in [
            (0x05, 0x41, 4, "Fintek F71882", "/lpc/f71882/0"),
            (0x05, 0x07, 2, "Fintek F71858", "/lpc/f71858/0"),
            (0x10, 0x07, 4, "Fintek F71869A/F71811", "/lpc/f71869a/0"),
            (0x09, 0x09, 4, "Fintek F71889ED", "/lpc/f71889ed/0"),
            (0x11, 0x06, 4, "Fintek F71878AD", "/lpc/f71878ad/0"),
        ] {
            let sim = fintek(id, rev, ldn);
            let chip = probe_io(&sim, 0).expect(name);
            assert_eq!((chip.name().as_str(), chip.id().as_str()), (name, key));
            assert_eq!(chip.base, 0x290);
            let s = sim.0.borrow();
            assert!(!s.entered && s.bars);
            assert!(s.ec_writes.is_empty());
        }
    }

    #[test]
    fn foreign_chips_return_none_and_exit() {
        // Nuvoton w tym samym kluczu: rozpoznany jako obcy, zamknięty 0xAA.
        let sim = Sim::new(0x2E, &KEY, false, 0xD42A);
        assert!(probe_io(&sim, 0).is_none());
        assert!(!sim.0.borrow().entered && !sim.0.borrow().bars);
        // ITE: klucz Fintek go nie otwiera, nic nie zapisujemy do rejestrów.
        let sim = Sim::new(0x2E, &[0x87, 0x01, 0x55, 0x55], true, 0x8686);
        assert!(probe_io(&sim, 0).is_none());
        assert!(!sim.0.borrow().entered);
        assert!(sim.0.borrow().sio_writes.is_empty());
        // F71811 (LHM go nie obsługuje) i zły producent.
        assert!(probe_io(&fintek(0x11, 0x18, 4), 0).is_none());
        let sim = fintek(0x05, 0x41, 4);
        sim.0.borrow_mut().global[0x24] = 0x00;
        assert!(probe_io(&sim, 0).is_none());
        assert!(!sim.0.borrow().entered);
    }

    #[test]
    fn decodes_temps_and_fans() {
        let sim = fintek(0x05, 0x41, 4);
        let mut chip = probe_io(&sim, 0).unwrap();
        {
            let mut s = sim.0.borrow_mut();
            s.ec[0x72] = 50;
            s.ec[0x74] = 0x80;
            s.ec[0x76] = 0;
            s.ec[0xA0] = 0x03; // 0x03E8 = 1000 -> 1500 obr./min
            s.ec[0xA1] = 0xE8;
            s.ec[0xB0] = 0x0F; // 0x0FFF - brak impulsów
            s.ec[0xB1] = 0xFF;
            s.ec[0xA3] = 0x80; // 128/255
        }
        chip.update_io(&sim).unwrap();
        assert_eq!(chip.temps, vec![Some(50.0), None, None]);
        assert_eq!(chip.fans[0], Some(1500.0));
        assert_eq!(chip.fans[1], None);
        assert_eq!(chip.fans[2], None);
        assert!((chip.controls[0].unwrap() - 50.196).abs() < 0.01);

        let mut r = Readings::default();
        chip.readings(&mut r);
        assert_eq!(r.temps[0].id, "/lpc/f71882/0/temperature/0");
        assert_eq!(r.temps[0].hw, "Fintek F71882");
        assert_eq!(r.fans.len(), 4);
        assert_eq!(r.speeds.len(), 4);
        assert_eq!(r.fans[3].id, "/lpc/f71882/0/control/3");
        assert_eq!(r.fans[0].name, "Fan #1");
        assert_eq!(r.fans[0].rpm, Some(1500.0));

        // EVGA X58 3X SLI (F71882): nazwy płyty, te same identyfikatory.
        chip.apply_board(&Board { manufacturer: "EVGA".into(), product: "132-BL-E758".into() }, 0);
        let mut r = Readings::default();
        chip.readings(&mut r);
        assert_eq!(r.temps[1].name, "VREG");
        assert_eq!(r.fans[3].id, "/lpc/f71882/0/control/3");
        assert_eq!(r.fans[3].name, "Chipset Fan");
        assert_eq!(r.fans[0].rpm, Some(1500.0));
    }

    #[test]
    fn msi_default_names_controls_differently() {
        // MSI bez listy płyty: LHM nazywa sterowania "Fan Control #n".
        let sim = fintek(0x10, 0x05, 4);
        let mut chip = probe_io(&sim, 0).unwrap();
        chip.apply_board(&Board { manufacturer: "MSI".into(), product: "Unknown".into() }, 0);
        let mut r = Readings::default();
        chip.readings(&mut r);
        assert_eq!(r.fans[0].name, "Fan #1");
        assert_eq!(chip.names.controls[0], (0, "Fan Control #1".to_string()));
        // Płyta spoza tablic: domyślne.
        let mut chip = probe_io(&sim, 0).unwrap();
        chip.apply_board(&Board { manufacturer: "JGINYUE".into(), product: "B760I GAMING".into() }, 0);
        assert_eq!(chip.names.controls[0], (0, "Fan #1".to_string()));
    }

    #[test]
    fn f71858_temperatures() {
        // Tryb 0: 0x32 << 7 | 0x60 >> 1 = 0x1930 -> 50,375 °C.
        assert_eq!(f71858_temp(0, 0x32, 0x60), Some(50.375));
        // Tryb 2: bit znaku ze starszego bajtu: 0xF800 -> -16 °C.
        assert_eq!(f71858_temp(2, 0xF0, 0x00), Some(-16.0));
        assert_eq!(f71858_temp(1, 0xBB, 0x00), None);
        let sim = fintek(0x05, 0x07, 2);
        let mut chip = probe_io(&sim, 0).unwrap();
        sim.0.borrow_mut().ec[0x70] = 0x28;
        chip.update_io(&sim).unwrap();
        assert_eq!(chip.temps[0], Some(40.0));
        assert!(chip.controls.is_empty());
        assert!(chip.set_io(&sim, 0, Some(50.0)).is_err());
    }

    #[test]
    fn pwm_write_and_restore_plain() {
        let sim = fintek(0x05, 0x41, 4);
        let mut chip = probe_io(&sim, 0).unwrap();
        sim.0.borrow_mut().ec[0xB3] = 0x44;
        chip.set_io(&sim, 1, Some(100.0)).unwrap();
        chip.set_io(&sim, 1, Some(0.0)).unwrap();
        assert_eq!(sim.0.borrow().ec_writes, vec![(0xB3, 255), (0xB3, 0)]);
        chip.set_io(&sim, 1, None).unwrap();
        assert_eq!(sim.0.borrow().ec[0xB3], 0x44);
        assert!(chip.saved.iter().all(Option::is_none));
    }

    #[test]
    fn pwm_write_and_restore_mode_bits() {
        let sim = fintek(0x09, 0x09, 4);
        let mut chip = probe_io(&sim, 0).unwrap();
        {
            let mut s = sim.0.borrow_mut();
            s.ec[0x9F] = 0x85; // drugi bank wybrany
            s.ec[0x96] = 0b00_00_00_00; // wszystkie automatycznie, tryb obrotów
            s.ec[0xB2] = 0x12;
            s.ec[0xB3] = 0x34;
            s.ec[0xB4] = 0x09; // pełne obroty 0x0900, cel 0x1234 -> ~49,5 %
            s.ec[0xB5] = 0x00;
        }
        chip.update_io(&sim).unwrap();
        assert!((chip.controls[1].unwrap() - 49.46).abs() < 0.1);
        chip.set_io(&sim, 1, Some(60.0)).unwrap();
        assert_eq!(sim.0.borrow().ec_writes, vec![(0x9F, 0x05), (0x96, 0b00_00_11_00), (0xB3, 153)]);
        chip.update_io(&sim).unwrap();
        assert!((chip.controls[1].unwrap() - 60.0).abs() < 0.01);
        sim.0.borrow_mut().ec_writes.clear();
        chip.restore_all_sim(&sim);
        let s = sim.0.borrow();
        assert_eq!(s.ec_writes, vec![(0x96, 0x00), (0xB2, 0x12), (0xB3, 0x34), (0x9F, 0x85)]);
        assert_eq!((s.ec[0x96], s.ec[0x9F], s.ec[0xB3]), (0x00, 0x85, 0x34));
        assert!(chip.fault_t.is_none());
    }

    impl Fintek {
        /// `restore_all` na symulacji (trait `Chip` wymaga `Lpc`).
        fn restore_all_sim(&mut self, io: &Sim) {
            for i in 0..self.model.controls {
                let _ = self.restore_io(io, i);
            }
        }
    }
}
