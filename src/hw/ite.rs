//! Układy ITE IT86xx/IT87xx (Gigabyte, starsze ASUS/MSI/ASRock).
//!
//! Tryb konfiguracji: 0x87 0x01 0x55 0x55 (na 0x4E ostatni bajt 0xAA),
//! wyjście: 0x02 do rejestru 0x02. Kontroler środowiska (logiczne urządzenie 4)
//! ma okno rejestrów pod adresem bazowym: indeks `base + 5`, dane `base + 6`.
//! Rejestry, przeliczenia i kolejność zapisów jak w LibreHardwareMonitor
//! (`IT87XX.cs`, `LpcIO.cs`), sprawdzone z linuksowym `it87`.
//!
//! Cały dostęp do portów (także `probe`) wymaga blokady ISA trzymanej przez
//! wołającego (`Lpc::lock`).

use std::time::Duration;

use crate::bridge::{Fan, Speed};

use super::boards::{self, BoardNames, Counts};
use super::superio::{Board, Chip, Lpc};
use super::{valid_temp, Readings};

/// Nazwy domyślne LHM: "Temperature #n", "Fan #n" (obroty i sterowanie).
pub fn default_names(temps: usize, fans: usize, controls: usize) -> BoardNames {
    let numbered = |count: usize, prefix: &str| (0..count).map(|i| (i, format!("{prefix}{}", i + 1))).collect();
    BoardNames {
        temps: numbered(temps, "Temperature #"),
        fans: numbered(fans, "Fan #"),
        controls: numbered(controls, "Fan #"),
    }
}

/// Odczyty według list nazw (jak w LHM): temperatury, wentylatory ze
/// sterowaniem (nazwa i obroty z listy obrotów o tym samym indeksie) i obroty.
pub fn names_readings(
    names: &BoardNames,
    id: &str,
    hw: &str,
    temps: &[Option<f32>],
    fans: &[Option<f32>],
    controls: &[Option<f32>],
    out: &mut Readings,
) {
    for (i, name) in &names.temps {
        if let Some(t) = temps.get(*i) {
            out.temp(format!("{id}/temperature/{i}"), name, hw, "SuperIO", *t);
        }
    }
    let fan = |i: usize| names.fans.iter().find(|(f, _)| *f == i && i < fans.len()).map(|(_, n)| (n.as_str(), fans[i]));
    let control =
        |i: usize| names.controls.iter().any(|(c, _)| *c == i).then(|| controls.get(i).copied().flatten()).flatten();
    for (i, name) in &names.controls {
        let Some(percent) = controls.get(*i) else { continue };
        let (name, rpm) = fan(*i).unwrap_or((name.as_str(), None));
        out.fans.push(Fan {
            id: format!("{id}/control/{i}"),
            name: name.into(),
            hw: hw.into(),
            kind: "SuperIO".into(),
            rpm,
            percent: *percent,
        });
    }
    for (i, _) in &names.fans {
        if let Some((name, rpm)) = fan(*i) {
            out.speeds.push(Speed { name: name.into(), hw: hw.into(), rpm, percent: control(*i) });
        }
    }
}

/// Dostęp do portów Super I/O: `Lpc` w programie, symulowane rejestry w testach.
/// Wszystkie wywołania pod blokadą ISA trzymaną przez wołającego.
pub trait SioIo {
    /// Port rejestru konfiguracji wybranego gniazda (0x2E albo 0x4E).
    fn config_port(&self) -> u16;
    fn inb(&self, port: u16) -> Result<u8, String>;
    fn outb(&self, port: u16, value: u8) -> Result<(), String>;
    fn sio_inb(&self, reg: u8) -> Result<u8, String>;
    /// `reg` starszy bajt, `reg + 1` młodszy (jak `ioctl_superio_inw`).
    fn sio_inw(&self, reg: u8) -> Result<u16, String> {
        let hi = self.sio_inb(reg)?;
        let lo = self.sio_inb(reg.wrapping_add(1))?;
        Ok(u16::from(hi) << 8 | u16::from(lo))
    }
    fn sio_outb(&self, reg: u8, value: u8) -> Result<(), String>;
    /// Odblokowuje zakresy portów układu (tylko w trybie konfiguracji).
    fn find_bars(&self) -> Result<(), String>;
}

impl SioIo for Lpc {
    fn config_port(&self) -> u16 {
        self.port
    }
    fn inb(&self, port: u16) -> Result<u8, String> {
        Lpc::inb(self, port)
    }
    fn outb(&self, port: u16, value: u8) -> Result<(), String> {
        Lpc::outb(self, port, value)
    }
    fn sio_inb(&self, reg: u8) -> Result<u8, String> {
        Lpc::sio_inb(self, reg)
    }
    fn sio_inw(&self, reg: u8) -> Result<u16, String> {
        Lpc::sio_inw(self, reg)
    }
    fn sio_outb(&self, reg: u8, value: u8) -> Result<(), String> {
        Lpc::sio_outb(self, reg, value)
    }
    fn find_bars(&self) -> Result<(), String> {
        Lpc::find_bars(self)
    }
}

/// Port konfiguracji gniazda (jak `Lpc::select_slot`).
pub fn slot_port(slot: u8) -> u16 {
    if slot == 0 {
        0x2E
    } else {
        0x4E
    }
}

/// Poprawny adres bazowy okna rejestrów: stały odczyt, poza zakresem
/// zastrzeżonym, wyrównany do 8 portów.
pub fn valid_base(address: u16, verify: u16) -> bool {
    address == verify && address >= 0x100 && address & 0xF007 == 0
}

// Rejestry konfiguracji.
const CHIP_ID: u8 = 0x20;
const CHIP_VERSION: u8 = 0x22;
const CONFIG_CONTROL: u8 = 0x02;
const LDN_SELECT: u8 = 0x07;
const BASE_ADDRESS: u8 = 0x60;
const ENVIRONMENT_LDN: u8 = 0x04;

// Rejestry kontrolera środowiska.
const EC_CONFIG: u8 = 0x00;
const EC_BANK: u8 = 0x06;
const EC_FAN_DIVISOR: u8 = 0x0B;
const EC_FAN16_ENABLE_ALT: u8 = 0x0B;
const EC_FAN16_ENABLE: u8 = 0x0C;
const EC_FAN_MAIN_CTRL: u8 = 0x13;
const EC_TEMP_BASE: u8 = 0x29;
const EC_VENDOR_ID: u8 = 0x58;
const ITE_VENDOR_IDS: [u8; 2] = [0x90, 0x7F];

const FAN_TACH: [u8; 6] = [0x0D, 0x0E, 0x0F, 0x80, 0x82, 0x4C];
const FAN_TACH_EXT: [u8; 6] = [0x18, 0x19, 0x1A, 0x81, 0x83, 0x4D];
const FAN_TACH_ALT: [u8; 6] = [0x0D, 0x0E, 0x0F, 0x80, 0x82, 0x93];
const FAN_TACH_EXT_ALT: [u8; 6] = [0x18, 0x19, 0x1A, 0x81, 0x83, 0x94];
const PWM_EXT: [u8; 6] = [0x63, 0x6B, 0x73, 0x7B, 0xA3, 0xAB];
const PWM_DEFAULT: [u8; 6] = [0x15, 0x16, 0x17, 0x7F, 0xA7, 0xAF];
const PWM_IT8665E: [u8; 6] = [0x15, 0x16, 0x17, 0x1E, 0x1F, 0x92];
const PWM_IT8792E: [u8; 3] = [0x15, 0x16, 0x17];

/// Najwięcej wentylatorów i sterowań w tej rodzinie.
const MAX_FANS: usize = 6;

/// Model układu: ID, nazwy jak w LibreHardwareMonitor i różnice w rejestrach.
#[derive(Debug)]
struct Model {
    id: u16,
    /// Nazwa w identyfikatorze (`Chip` w LHM), np. "IT8792E".
    key: &'static str,
    /// Nazwa wyświetlana bez "ITE ", np. "IT8791E/IT8792E/IT8795E".
    name: &'static str,
    temps: usize,
    fans: usize,
    controls: usize,
    /// Rejestry sterowania PWM (bit 7 = tryb automatyczny).
    pwm: &'static [u8],
    /// Nowsze układy: 8-bitowe wypełnienie w osobnym rejestrze (`PWM_EXT`).
    ext: bool,
    /// Szósty wentylator w rejestrach 0x93/0x94 (IT8625E, IT8665E).
    alt_fan6: bool,
    /// Przed odczytem trzeba wybrać bank 0 (IT8655E, IT8665E).
    bank0: bool,
    /// Bit 0x10 rejestru konfiguracji może być zerem (IT8655E, IT8665E).
    no_config_bit: bool,
    /// IT8688E nie oddaje zapisanego indeksu przy odczycie portu indeksu.
    no_index_echo: bool,
    /// IT8689E: rejestr sterowania ustawiany na 0x7F zamiast starej wartości.
    pwm_7f: bool,
    /// Najniższa wersja z 16-bitowymi licznikami obrotów (IT8705F, IT8712F).
    fan16_from: u8,
    /// Nie wolno wychodzić z trybu konfiguracji (IT879x: zawieszenie systemu,
    /// utrata dostępu do drugiego układu - jak w linuksowym `it87`).
    no_exit: bool,
    /// Drugi układ na płytach Gigabyte - wentylatory prowadzi firmware.
    gigabyte_ec: bool,
}

const fn model(id: u16, key: &'static str) -> Model {
    Model {
        id,
        key,
        name: key,
        temps: 3,
        fans: 5,
        controls: 3,
        pwm: &PWM_DEFAULT,
        ext: false,
        alt_fan6: false,
        bank0: false,
        no_config_bit: false,
        no_index_echo: false,
        pwm_7f: false,
        fan16_from: 0,
        no_exit: false,
        gigabyte_ec: false,
    }
}

/// Układy obsługiwane przez LibreHardwareMonitor tą samą ścieżką kodu.
static MODELS: [Model; 25] = [
    Model { temps: 4, controls: 4, ext: true, ..model(0x8613, "IT8613E") },
    Model { controls: 5, ext: true, ..model(0x8620, "IT8620E") },
    Model { fans: 6, controls: 6, ext: true, pwm: &PWM_IT8665E, alt_fan6: true, ..model(0x8625, "IT8625E") },
    Model { temps: 6, fans: 6, controls: 6, ext: true, ..model(0x8628, "IT8628E") },
    Model { temps: 2, fans: 2, controls: 2, ext: true, ..model(0x8631, "IT8631E") },
    Model { temps: 2, fans: 2, controls: 2, ext: true, ..model(0x8638, "IT8638E") },
    Model { temps: 6, fans: 3, controls: 3, ext: true, bank0: true, no_config_bit: true, ..model(0x8655, "IT8655E") },
    Model {
        temps: 6,
        fans: 6,
        controls: 6,
        ext: true,
        pwm: &PWM_IT8665E,
        alt_fan6: true,
        bank0: true,
        no_config_bit: true,
        ..model(0x8665, "IT8665E")
    },
    Model { temps: 7, fans: 6, controls: 5, ext: true, ..model(0x8686, "IT8686E") },
    Model { temps: 6, fans: 6, controls: 5, ext: true, no_index_echo: true, ..model(0x8688, "IT8688E") },
    Model { temps: 6, fans: 6, controls: 6, ext: true, pwm_7f: true, ..model(0x8689, "IT8689E") },
    Model { temps: 6, fans: 6, controls: 6, ext: true, ..model(0x8696, "IT8696E") },
    Model { fans: 3, fan16_from: 3, ..model(0x8705, "IT8705F") },
    Model { fan16_from: 8, ..model(0x8712, "IT8712F") },
    model(0x8716, "IT8716F"),
    model(0x8718, "IT8718F"),
    model(0x8720, "IT8720F"),
    Model { ext: true, ..model(0x8721, "IT8721F") },
    model(0x8726, "IT8726F"),
    Model { ext: true, ..model(0x8728, "IT8728F") },
    Model { ext: true, ..model(0x8771, "IT8771E") },
    Model { ext: true, ..model(0x8772, "IT8772E") },
    Model { no_exit: true, gigabyte_ec: true, ..model(0x8790, "IT8790E") },
    Model {
        name: "IT8791E/IT8792E/IT8795E",
        fans: 3,
        pwm: &PWM_IT8792E,
        ext: true,
        no_exit: true,
        gigabyte_ec: true,
        ..model(0x8733, "IT8792E")
    },
    Model { fans: 3, ext: true, no_exit: true, gigabyte_ec: true, ..model(0x8695, "IT87952E") },
];

fn find_model(id: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

/// Rozpoznanie układu ITE w wybranym gnieździe (blokada ISA po stronie wołającego).
pub fn probe(lpc: &mut Lpc, slot: u8, index: usize) -> Option<Box<dyn Chip>> {
    let mut chip = probe_io(lpc, slot)?;
    chip.apply_board(&Board::from_registry(), index);
    Some(Box::new(chip))
}

fn enter<P: SioIo + ?Sized>(io: &P, port: u16) -> Result<(), String> {
    let last = if port == 0x4E { 0xAA } else { 0x55 };
    for b in [0x87, 0x01, 0x55, last] {
        io.outb(port, b)?;
    }
    Ok(())
}

fn exit<P: SioIo + ?Sized>(io: &P) {
    let _ = io.sio_outb(CONFIG_CONTROL, 0x02);
}

/// Wyjście po nieznanym ID. Klucz ITE nie otwiera układów Nuvoton/Fintek
/// (te czekają na 0x87 0x87), ale bajty 0x55 otwierają SMSC - te zamyka 0xAA.
fn leave_unknown<P: SioIo + ?Sized>(io: &P, port: u16, id: Option<u16>, entered: bool) {
    match id {
        // Nic nie odpowiada w trybie konfiguracji.
        Some(0) | Some(0xFFFF) => {}
        // Inny układ ITE: na 0x4E może to być IT879x, którego nie wolno zamykać.
        Some(id) if (0x86..=0x88).contains(&(id >> 8)) || id >> 8 == 0x85 => {
            if port == 0x2E {
                exit(io);
            }
        }
        // Inny producent otwarty naszym kluczem (SMSC) - 0xAA zamyka też Nuvoton/Fintek.
        Some(_) => {
            if entered {
                let _ = io.outb(port, 0xAA);
            }
        }
        // Błąd odczytu: zamykamy jak ITE tam, gdzie to bezpieczne.
        None => {
            if entered && port == 0x2E {
                exit(io);
            }
        }
    }
}

fn probe_io<P: SioIo + ?Sized>(io: &P, slot: u8) -> Option<Ite> {
    let port = io.config_port();
    if slot > 1 || port != slot_port(slot) {
        return None;
    }
    // Na 0x4E drugi układ (IT879x) bywa już w trybie konfiguracji; ponowne
    // wejście psuje jego ID (0x8883) - wtedy czytamy bez klucza.
    let pre = if port == 0x4E { io.sio_inw(CHIP_ID).ok().filter(|&id| id != 0xFFFF) } else { None };
    let entered = pre.is_none();
    let id = match pre {
        Some(id) => Some(id),
        None => {
            if enter(io, port).is_err() {
                return None;
            }
            io.sio_inw(CHIP_ID).ok()
        }
    };
    let Some(model) = id.and_then(find_model) else {
        leave_unknown(io, port, id, entered);
        return None;
    };
    let found = configure(io, model);
    if !model.no_exit {
        exit(io);
    }
    let (base, version) = found?;
    Ite::new(io, model, base, version, port)
}

/// W trybie konfiguracji: BAR-y, adres bazowy i wersja; adres GPIO tylko
/// sprawdzany (LHM odrzuca układ z błędnym).
fn configure<P: SioIo + ?Sized>(io: &P, model: &Model) -> Option<(u16, u8)> {
    io.find_bars().ok()?;
    io.sio_outb(LDN_SELECT, ENVIRONMENT_LDN).ok()?;
    let address = io.sio_inw(BASE_ADDRESS).ok()?;
    std::thread::sleep(Duration::from_millis(1));
    let verify = io.sio_inw(BASE_ADDRESS).ok()?;
    let version = io.sio_inb(CHIP_VERSION).ok()? & 0x0F;
    let (gpio_ldn, gpio_reg) = if model.key == "IT8705F" { (0x05, BASE_ADDRESS) } else { (0x07, BASE_ADDRESS + 2) };
    io.sio_outb(LDN_SELECT, gpio_ldn).ok()?;
    let gpio = io.sio_inw(gpio_reg).ok()?;
    std::thread::sleep(Duration::from_millis(1));
    let gpio_verify = io.sio_inw(gpio_reg).ok()?;
    (valid_base(address, verify) && valid_base(gpio, gpio_verify)).then_some((address, version))
}

/// Okno rejestrów kontrolera środowiska.
struct Ec {
    base: u16,
    echo: bool,
}

impl Ec {
    /// `None` - port indeksu nie oddał numeru rejestru (odczyt niepewny).
    fn read<P: SioIo + ?Sized>(&self, io: &P, reg: u8) -> Result<Option<u8>, String> {
        io.outb(self.base + 5, reg)?;
        let value = io.inb(self.base + 6)?;
        let ok = !self.echo || io.inb(self.base + 5)? == reg;
        Ok(ok.then_some(value))
    }

    /// Odczyt wymagany do zapisu: niepewny = błąd.
    fn read_sure<P: SioIo + ?Sized>(&self, io: &P, reg: u8) -> Result<u8, String> {
        self.read(io, reg)?
            .ok_or_else(|| crate::trf!("niepewny odczyt rejestru 0x{reg:02X}", "unreliable read of register 0x{reg:02X}"))
    }

    fn write<P: SioIo + ?Sized>(&self, io: &P, reg: u8, value: u8) -> Result<(), String> {
        io.outb(self.base + 5, reg)?;
        io.outb(self.base + 6, value)?;
        io.inb(self.base + 5).map(|_| ())
    }
}

/// Stan sterowania sprzed pierwszej zmiany.
#[derive(Clone, Copy, Debug)]
struct Saved {
    ctrl: u8,
    /// Bit wyjścia PWM w 0x13 (tylko wentylatory 0-2).
    main_on: Option<bool>,
    ext: Option<u8>,
}

pub struct Ite {
    model: &'static Model,
    ec: Ec,
    fan16: bool,
    fan_off: [bool; MAX_FANS],
    read_only: bool,
    temps: Vec<Option<f32>>,
    fans: Vec<Option<f32>>,
    controls: Vec<Option<f32>>,
    saved: [Option<Saved>; MAX_FANS],
    /// Czujniki pokazywane w LHM (domyślne albo dla płyty).
    names: BoardNames,
}

impl Ite {
    /// Po wyjściu z konfiguracji: identyfikator producenta i bit konfiguracji
    /// w oknie rejestrów, tryb liczników obrotów.
    fn new<P: SioIo + ?Sized>(io: &P, model: &'static Model, base: u16, version: u8, port: u16) -> Option<Self> {
        let ec = Ec { base, echo: !model.no_index_echo };
        let vendor = ec.read(io, EC_VENDOR_ID).ok()??;
        if !ITE_VENDOR_IDS.contains(&vendor) {
            return None;
        }
        let config = ec.read(io, EC_CONFIG).ok()??;
        if config & 0x10 == 0 && !model.no_config_bit {
            return None;
        }
        let fan16 = version >= model.fan16_from;
        let mut fan_off = [false; MAX_FANS];
        if fan16 {
            if let Ok(Some(modes)) = ec.read(io, EC_FAN16_ENABLE) {
                if model.fans >= 5 {
                    fan_off[3] = modes & 0x10 == 0;
                    fan_off[4] = modes & 0x20 == 0;
                }
                if model.fans >= 6 {
                    if model.key == "IT8665E" {
                        if let Ok(Some(alt)) = ec.read(io, EC_FAN16_ENABLE_ALT) {
                            fan_off[5] = alt & 0x08 == 0;
                        }
                    } else {
                        fan_off[5] = modes & 0x04 == 0;
                    }
                }
            }
        }
        Some(Self {
            model,
            ec,
            fan16,
            fan_off,
            // Drugi układ Gigabyte: wentylatorami steruje firmware płyty, a jej
            // wyłączenie wymaga MMIO mostka ISA albo portu ECIO spoza LpcIO.
            read_only: model.gigabyte_ec && port == 0x4E,
            temps: vec![None; model.temps],
            fans: vec![None; model.fans],
            controls: vec![None; model.controls],
            saved: [None; MAX_FANS],
            names: default_names(model.temps, model.fans, model.controls),
        })
    }

    /// Nazwy czujników dla płyty (`boards.rs`); bez listy - zostają domyślne.
    fn apply_board(&mut self, board: &Board, index: usize) {
        let counts = Counts { temps: self.model.temps, fans: self.model.fans, controls: self.model.controls };
        if let Some(names) = boards::names(self.model.key, &board.manufacturer, &board.product, index, counts) {
            self.names = names;
        }
    }

    fn update_io<P: SioIo + ?Sized>(&mut self, io: &P) -> Result<(), String> {
        let ec = &self.ec;
        // Bank 0 (po uśpieniu bywa inny); zapis tylko, gdy wybrany jest inny bank.
        if self.model.bank0 {
            if let Some(bank) = ec.read(io, EC_BANK)? {
                if bank & 0x60 != 0 {
                    ec.write(io, EC_BANK, bank & 0x9F)?;
                }
            }
        }
        for (i, t) in self.temps.iter_mut().enumerate() {
            if let Some(raw) = ec.read(io, EC_TEMP_BASE + i as u8)? {
                let c = raw as i8;
                *t = if c > 0 && c < i8::MAX { valid_temp(f32::from(c)) } else { None };
            }
        }
        for (i, f) in self.fans.iter_mut().enumerate() {
            if self.fan_off.get(i).copied().unwrap_or(true) {
                continue;
            }
            if self.fan16 {
                let (lo, hi) = if self.model.alt_fan6 { (FAN_TACH_ALT, FAN_TACH_EXT_ALT) } else { (FAN_TACH, FAN_TACH_EXT) };
                let (Some(&lo), Some(&hi)) = (lo.get(i), hi.get(i)) else { continue };
                let Some(lo) = ec.read(io, lo)? else { continue };
                let Some(hi) = ec.read(io, hi)? else { continue };
                *f = fan16_rpm(u16::from(hi) << 8 | u16::from(lo));
            } else {
                let Some(&reg) = FAN_TACH.get(i) else { continue };
                let Some(count) = ec.read(io, reg)? else { continue };
                let mut divisor = 2u32;
                if i < 2 {
                    let Some(d) = ec.read(io, EC_FAN_DIVISOR)? else { continue };
                    divisor = 1 << ((d >> (3 * i)) & 0x07);
                }
                *f = fan8_rpm(count, divisor);
            }
        }
        for (i, c) in self.controls.iter_mut().enumerate() {
            let Some(&reg) = self.model.pwm.get(i) else { continue };
            let Some(ctrl) = ec.read(io, reg)? else { continue };
            if ctrl & 0x80 != 0 {
                // Tryb automatyczny - wypełnienia nie da się odczytać.
                *c = None;
            } else if self.model.ext {
                let Some(&ext) = PWM_EXT.get(i) else { continue };
                if let Some(v) = ec.read(io, ext)? {
                    *c = Some((f32::from(v) * 100.0 / 255.0).round());
                }
            } else {
                *c = Some((f32::from(ctrl & 0x7F) * 100.0 / 127.0).round());
            }
        }
        Ok(())
    }

    fn set_io<P: SioIo + ?Sized>(&mut self, io: &P, index: usize, percent: Option<f32>) -> Result<(), String> {
        if index >= self.model.controls {
            return Err(crate::trf!("nie ma sterowania {index}", "no control {index}"));
        }
        let Some(p) = percent else { return self.restore_io(io, index) };
        if self.read_only {
            return Err(crate::i18n::tr("układ tylko do odczytu", "read-only chip").to_owned());
        }
        if !p.is_finite() {
            return Err(crate::i18n::tr("zła wartość wypełnienia", "bad duty value").to_owned());
        }
        let (Some(&reg), Some(&ext)) = (self.model.pwm.get(index), PWM_EXT.get(index)) else {
            return Err(crate::trf!("nie ma sterowania {index}", "no control {index}"));
        };
        // Jak LHM: procent * 2,55 obcięte do bajtu.
        let value = (p.clamp(0.0, 100.0) * 2.55) as u8;
        let ec = &self.ec;
        let saved = match self.saved.get(index).copied().flatten() {
            Some(s) => s,
            None => {
                let s = Saved {
                    ctrl: ec.read_sure(io, reg)?,
                    main_on: if index < 3 { Some(ec.read_sure(io, EC_FAN_MAIN_CTRL)? & (1 << index) != 0) } else { None },
                    ext: if self.model.ext { Some(ec.read_sure(io, ext)?) } else { None },
                };
                if let Some(slot) = self.saved.get_mut(index) {
                    *slot = Some(s);
                }
                s
            }
        };
        if saved.main_on == Some(false) {
            let main = ec.read_sure(io, EC_FAN_MAIN_CTRL)?;
            ec.write(io, EC_FAN_MAIN_CTRL, main | (1 << index))?;
        }
        if self.model.ext {
            let ctrl = if self.model.pwm_7f { 0x7F } else { saved.ctrl & 0x7F };
            ec.write(io, reg, ctrl)?;
            ec.write(io, ext, value)
        } else {
            ec.write(io, reg, value >> 1)
        }
    }

    /// Przywraca stan sprzed pierwszej zmiany (no-op, gdy nic nie zmieniono).
    fn restore_io<P: SioIo + ?Sized>(&mut self, io: &P, index: usize) -> Result<(), String> {
        let Some(saved) = self.saved.get(index).copied().flatten() else { return Ok(()) };
        let (Some(&reg), Some(&ext)) = (self.model.pwm.get(index), PWM_EXT.get(index)) else { return Ok(()) };
        let ec = &self.ec;
        ec.write(io, reg, saved.ctrl)?;
        if let Some(on) = saved.main_on {
            let main = ec.read_sure(io, EC_FAN_MAIN_CTRL)?;
            if (main & (1 << index) != 0) != on {
                ec.write(io, EC_FAN_MAIN_CTRL, main ^ (1 << index))?;
            }
        }
        if let Some(v) = saved.ext {
            ec.write(io, ext, v)?;
        }
        if let Some(slot) = self.saved.get_mut(index) {
            *slot = None;
        }
        Ok(())
    }
}

/// 16-bitowy licznik (dzielnik 2): 0xFFFF = brak impulsów, <= 0x3F = brak czujnika.
fn fan16_rpm(count: u16) -> Option<f32> {
    (count > 0x3F && count < 0xFFFF).then(|| 1.35e6 / (f32::from(count) * 2.0))
}

/// 8-bitowy licznik ze starszych układów: 0xFF = brak impulsów, 0 = brak czujnika.
fn fan8_rpm(count: u8, divisor: u32) -> Option<f32> {
    (count > 0 && count < 0xFF).then(|| 1.35e6 / (f32::from(count) * divisor as f32))
}

impl Chip for Ite {
    fn name(&self) -> String {
        format!("ITE {}", self.model.name)
    }

    fn id(&self) -> String {
        // Indeks w LHM liczony w grupie tego samego układu - zawsze 0.
        format!("/lpc/{}/0", self.model.key.to_ascii_lowercase())
    }

    fn read_only(&self) -> bool {
        self.read_only
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

/// Symulowany układ Super I/O do testów (ITE i Fintek/Nuvoton).
#[cfg(test)]
pub mod sim {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::SioIo;

    pub struct State {
        pub port: u16,
        /// Klucz wejścia do trybu konfiguracji.
        pub key: Vec<u8>,
        /// Wyjście: `true` = 0x02 do rejestru 0x02 (ITE), `false` = 0xAA (Nuvoton/Fintek).
        pub ite_exit: bool,
        pub entered: bool,
        pub recent: Vec<u8>,
        index: u8,
        pub ldn: u8,
        pub global: [u8; 0x30],
        pub ldn_regs: HashMap<(u8, u8), u8>,
        pub bars: bool,
        pub base: u16,
        pub ec: [u8; 256],
        ec_index: u8,
        /// Port indeksu oddaje zapisany indeks.
        pub echo: bool,
        /// Zapisy do okna rejestrów (rejestr, wartość).
        pub ec_writes: Vec<(u8, u8)>,
        /// Zapisy do rejestrów konfiguracji (rejestr, wartość).
        pub sio_writes: Vec<(u8, u8)>,
    }

    pub struct Sim(pub RefCell<State>);

    impl Sim {
        pub fn new(port: u16, key: &[u8], ite_exit: bool, id: u16) -> Self {
            let mut global = [0u8; 0x30];
            global[0x20] = (id >> 8) as u8;
            global[0x21] = id as u8;
            Sim(RefCell::new(State {
                port,
                key: key.to_vec(),
                ite_exit,
                entered: false,
                recent: Vec::new(),
                index: 0,
                ldn: 0,
                global,
                ldn_regs: HashMap::new(),
                bars: false,
                base: 0,
                ec: [0; 256],
                ec_index: 0,
                echo: true,
                ec_writes: Vec::new(),
                sio_writes: Vec::new(),
            }))
        }

        /// Adres bazowy w rejestrach 0x60/0x61 urządzenia `ldn`.
        pub fn set_base(&self, ldn: u8, reg: u8, base: u16) {
            let mut s = self.0.borrow_mut();
            s.ldn_regs.insert((ldn, reg), (base >> 8) as u8);
            s.ldn_regs.insert((ldn, reg + 1), base as u8);
        }

        fn sio_write(s: &mut State, reg: u8, value: u8) {
            s.sio_writes.push((reg, value));
            match reg {
                0x07 => s.ldn = value,
                0x02 if s.ite_exit && value & 0x02 != 0 => s.entered = false,
                r if r < 0x30 => s.global[r as usize] = value,
                r => {
                    s.ldn_regs.insert((s.ldn, r), value);
                }
            }
        }
    }

    impl SioIo for Sim {
        fn config_port(&self) -> u16 {
            self.0.borrow().port
        }

        fn inb(&self, port: u16) -> Result<u8, String> {
            let s = self.0.borrow();
            if !s.bars || port & 0xFFF8 != s.base {
                return Err(format!("odmowa dostępu do portu {port:X}"));
            }
            match port - s.base {
                5 => Ok(if s.echo { s.ec_index } else { s.ec_index ^ 0x80 }),
                6 => Ok(s.ec[s.ec_index as usize]),
                _ => Ok(0xFF),
            }
        }

        fn outb(&self, port: u16, value: u8) -> Result<(), String> {
            let mut s = self.0.borrow_mut();
            if port == s.port {
                if s.entered {
                    if !s.ite_exit && value == 0xAA {
                        s.entered = false;
                    } else {
                        s.index = value;
                    }
                } else {
                    s.recent.push(value);
                    let n = s.key.len();
                    if s.recent.len() >= n && s.recent[s.recent.len() - n..] == s.key[..] {
                        s.entered = true;
                        s.recent.clear();
                    }
                }
                return Ok(());
            }
            if port == s.port + 1 {
                if s.entered {
                    let reg = s.index;
                    Self::sio_write(&mut s, reg, value);
                }
                return Ok(());
            }
            if !s.bars || port & 0xFFF8 != s.base {
                return Err(format!("odmowa dostępu do portu {port:X}"));
            }
            match port - s.base {
                5 => s.ec_index = value,
                6 => {
                    let i = s.ec_index;
                    s.ec[i as usize] = value;
                    s.ec_writes.push((i, value));
                }
                _ => {}
            }
            Ok(())
        }

        fn sio_inb(&self, reg: u8) -> Result<u8, String> {
            let s = self.0.borrow();
            if !s.entered {
                return Ok(0xFF);
            }
            Ok(match reg {
                0x07 => s.ldn,
                r if r < 0x30 => s.global[r as usize],
                r => s.ldn_regs.get(&(s.ldn, r)).copied().unwrap_or(0),
            })
        }

        fn sio_outb(&self, reg: u8, value: u8) -> Result<(), String> {
            let mut s = self.0.borrow_mut();
            if s.entered {
                Self::sio_write(&mut s, reg, value);
            }
            Ok(())
        }

        fn find_bars(&self) -> Result<(), String> {
            let mut s = self.0.borrow_mut();
            if !s.entered {
                return Err("STATUS_NOT_FOUND".into());
            }
            // Moduł przegląda wszystkie urządzenia logiczne.
            s.ldn = 0xFE;
            s.bars = true;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sim::Sim;
    use super::*;

    const ITE_KEY: [u8; 4] = [0x87, 0x01, 0x55, 0x55];
    const ITE_KEY_4E: [u8; 4] = [0x87, 0x01, 0x55, 0xAA];

    /// Układ ITE z poprawnym oknem rejestrów pod 0x290.
    fn ite(port: u16, id: u16) -> Sim {
        let key = if port == 0x4E { ITE_KEY_4E } else { ITE_KEY };
        let sim = Sim::new(port, &key, true, id);
        sim.set_base(4, 0x60, 0x290);
        sim.set_base(7, 0x62, 0xA00);
        sim.set_base(5, 0x60, 0xA00);
        {
            let mut s = sim.0.borrow_mut();
            s.base = 0x290;
            s.global[0x22] = 0x0A;
            s.ec[0x58] = 0x90;
            s.ec[0x00] = 0x10;
            // Liczniki 16-bitowe wentylatorów 4-6 włączone.
            s.ec[0x0C] = 0x34;
        }
        sim
    }

    #[test]
    fn models_are_consistent() {
        for m in &MODELS {
            assert!(m.controls <= m.pwm.len() && m.controls <= MAX_FANS, "{}", m.key);
            assert!(m.fans <= MAX_FANS && m.temps <= 8, "{}", m.key);
            assert_eq!(MODELS.iter().filter(|x| x.id == m.id).count(), 1);
        }
    }

    #[test]
    fn detects_it8686e_and_exits() {
        let sim = ite(0x2E, 0x8686);
        let chip = probe_io(&sim, 0).expect("IT8686E");
        assert_eq!(chip.name(), "ITE IT8686E");
        assert_eq!(chip.id(), "/lpc/it8686e/0");
        assert!(!chip.read_only());
        let s = sim.0.borrow();
        assert!(!s.entered, "tryb konfiguracji musi być zamknięty");
        assert!(s.bars);
        assert!(s.ec_writes.is_empty(), "wykrywanie nie zapisuje rejestrów");
    }

    #[test]
    fn names_and_ids() {
        for (id, name, key) in [
            (0x8733, "ITE IT8791E/IT8792E/IT8795E", "/lpc/it8792e/0"),
            (0x8695, "ITE IT87952E", "/lpc/it87952e/0"),
            (0x8620, "ITE IT8620E", "/lpc/it8620e/0"),
            (0x8705, "ITE IT8705F", "/lpc/it8705f/0"),
        ] {
            let sim = ite(0x2E, id);
            let chip = probe_io(&sim, 0).unwrap();
            assert_eq!((chip.name().as_str(), chip.id().as_str()), (name, key));
        }
    }

    #[test]
    fn unknown_ite_id_exits_config() {
        let sim = ite(0x2E, 0x8799);
        assert!(probe_io(&sim, 0).is_none());
        assert!(!sim.0.borrow().entered);
        assert!(!sim.0.borrow().bars);
    }

    #[test]
    fn nuvoton_and_fintek_are_untouched() {
        // Nuvoton (klucz 0x87 0x87): klucz ITE go nie otwiera, nic nie zapisujemy.
        for id in [0xD42A, 0x0541] {
            let sim = Sim::new(0x2E, &[0x87, 0x87], false, id);
            assert!(probe_io(&sim, 0).is_none());
            let s = sim.0.borrow();
            assert!(!s.entered);
            assert!(s.sio_writes.is_empty());
        }
    }

    #[test]
    fn smsc_opened_by_key_is_closed() {
        let sim = Sim::new(0x2E, &[0x55], false, 0x7A01);
        assert!(probe_io(&sim, 0).is_none());
        assert!(!sim.0.borrow().entered);
    }

    #[test]
    fn it8792e_on_4e_stays_in_config_and_is_read_only() {
        let sim = ite(0x4E, 0x8733);
        let mut chip = probe_io(&sim, 1).expect("IT8792E");
        assert!(sim.0.borrow().entered, "IT879x nie wolno zamykać");
        assert!(chip.read_only());
        assert!(chip.set_io(&sim, 0, Some(50.0)).is_err());
        assert!(sim.0.borrow().ec_writes.is_empty());
    }

    #[test]
    fn preentered_4e_is_read_without_key() {
        let sim = ite(0x4E, 0x8733);
        sim.0.borrow_mut().entered = true;
        assert!(probe_io(&sim, 1).is_some());
        // Ani jednego bajtu klucza.
        assert!(sim.0.borrow().recent.is_empty());
    }

    #[test]
    fn bad_vendor_or_base_rejected() {
        let sim = ite(0x2E, 0x8686);
        sim.0.borrow_mut().ec[0x58] = 0x12;
        assert!(probe_io(&sim, 0).is_none());
        let sim = ite(0x2E, 0x8686);
        sim.set_base(4, 0x60, 0x0295);
        assert!(probe_io(&sim, 0).is_none());
        assert!(!sim.0.borrow().entered);
    }

    #[test]
    fn decodes_temps_fans_and_pwm() {
        let sim = ite(0x2E, 0x8686);
        let mut chip = probe_io(&sim, 0).unwrap();
        {
            let mut s = sim.0.borrow_mut();
            s.ec[0x29] = 45;
            s.ec[0x2A] = 0x80; // -128: brak czujnika
            s.ec[0x2B] = 0x7F; // 127: brak czujnika
            s.ec[0x2C] = 0;
            // Wentylator 1: 0x0200 -> 1318 obr./min.
            s.ec[0x0D] = 0x00;
            s.ec[0x18] = 0x02;
            // Wentylator 2: 0xFFFF - brak impulsów.
            s.ec[0x0E] = 0xFF;
            s.ec[0x19] = 0xFF;
            // Wentylator 3: 0x0030 - brak czujnika.
            s.ec[0x0F] = 0x30;
            // PWM 1 ręcznie 128/255, PWM 2 automatycznie.
            s.ec[0x15] = 0x00;
            s.ec[0x63] = 128;
            s.ec[0x16] = 0x80;
        }
        chip.update_io(&sim).unwrap();
        assert_eq!(chip.temps[0], Some(45.0));
        assert_eq!(chip.temps[1], None);
        assert_eq!(chip.temps[2], None);
        assert_eq!(chip.temps[3], None);
        assert!((chip.fans[0].unwrap() - 1318.36).abs() < 0.1);
        assert_eq!(chip.fans[1], None);
        assert_eq!(chip.fans[2], None);
        assert_eq!(chip.controls[0], Some(50.0));
        assert_eq!(chip.controls[1], None);

        let mut r = Readings::default();
        chip.readings(&mut r);
        assert_eq!(r.temps.len(), 7);
        assert_eq!(r.temps[0].id, "/lpc/it8686e/0/temperature/0");
        assert_eq!(r.temps[0].name, "Temperature #1");
        assert_eq!(r.temps[0].kind, "SuperIO");
        assert_eq!(r.temps[0].hw, "ITE IT8686E");
        assert_eq!(r.speeds.len(), 6);
        assert_eq!(r.fans.len(), 5);
        assert_eq!(r.fans[4].id, "/lpc/it8686e/0/control/4");
        assert_eq!(r.fans[0].name, "Fan #1");
        assert_eq!(r.fans[0].percent, Some(50.0));
        assert_eq!(r.speeds[5].percent, None);
    }

    #[test]
    fn board_names_replace_defaults() {
        let sim = ite(0x2E, 0x8688);
        let mut chip = probe_io(&sim, 0).unwrap();
        let gigabyte =
            Board { manufacturer: "Gigabyte Technology Co., Ltd.".into(), product: "X570 AORUS MASTER".into() };
        chip.apply_board(&gigabyte, 0);
        let mut r = Readings::default();
        chip.readings(&mut r);
        let temps: Vec<(&str, &str)> = r.temps.iter().map(|t| (t.id.as_str(), t.name.as_str())).collect();
        assert_eq!(temps[0], ("/lpc/it8688e/0/temperature/0", "System #1"));
        assert_eq!(temps[4], ("/lpc/it8688e/0/temperature/4", "VRM MOS"));
        assert_eq!(r.fans[3].id, "/lpc/it8688e/0/control/3");
        assert_eq!(r.fans[3].name, "PCH Fan");
        assert_eq!(r.speeds.len(), 5);
        assert_eq!(r.speeds[4].name, "CPU Optional Fan");

        // ASUS IT8665E: sterowania "Fan #n", obroty z nazwami płyty.
        let sim = ite(0x2E, 0x8665);
        let mut chip = probe_io(&sim, 0).unwrap();
        chip.apply_board(&Board { manufacturer: "ASUSTeK COMPUTER INC.".into(), product: "PRIME X370-PRO".into() }, 0);
        let mut r = Readings::default();
        chip.readings(&mut r);
        assert_eq!(r.temps[2].name, "VRM");
        assert_eq!(r.fans[5].name, "Water Pump");
        assert_eq!(r.fans[5].id, "/lpc/it8665e/0/control/5");

        // Nieznana płyta: nazwy domyślne.
        let sim = ite(0x2E, 0x8688);
        let mut chip = probe_io(&sim, 0).unwrap();
        chip.apply_board(&Board { manufacturer: "JGINYUE".into(), product: "B760I GAMING".into() }, 0);
        let mut r = Readings::default();
        chip.readings(&mut r);
        assert_eq!(r.temps.len(), 6);
        assert_eq!(r.temps[5].name, "Temperature #6");
        assert_eq!(r.fans[0].name, "Fan #1");
    }

    #[test]
    fn invalid_index_echo_keeps_last_value() {
        let sim = ite(0x2E, 0x8686);
        let mut chip = probe_io(&sim, 0).unwrap();
        sim.0.borrow_mut().ec[0x29] = 40;
        chip.update_io(&sim).unwrap();
        let mut s = sim.0.borrow_mut();
        s.echo = false;
        s.ec[0x29] = 90;
        drop(s);
        chip.update_io(&sim).unwrap();
        assert_eq!(chip.temps[0], Some(40.0));
    }

    #[test]
    fn it8688e_ignores_index_echo() {
        let sim = ite(0x2E, 0x8688);
        sim.0.borrow_mut().echo = false;
        let mut chip = probe_io(&sim, 0).expect("IT8688E");
        sim.0.borrow_mut().ec[0x29] = 33;
        chip.update_io(&sim).unwrap();
        assert_eq!(chip.temps[0], Some(33.0));
    }

    #[test]
    fn old_8bit_counters() {
        // IT8705F w wersji 2: liczniki 8-bitowe, dzielniki z 0x0B.
        let sim = ite(0x2E, 0x8705);
        sim.0.borrow_mut().global[0x22] = 0x02;
        let mut chip = probe_io(&sim, 0).unwrap();
        assert!(!chip.fan16);
        {
            let mut s = sim.0.borrow_mut();
            s.ec[0x0B] = 0b001_010; // wentylator 1: /4, wentylator 2: /2
            s.ec[0x0D] = 100;
            s.ec[0x0E] = 0xFF;
            s.ec[0x0F] = 0;
            s.ec[0x15] = 0x3F; // ręcznie, 63/127
        }
        chip.update_io(&sim).unwrap();
        assert!((chip.fans[0].unwrap() - 3375.0).abs() < 0.01);
        assert_eq!(chip.fans[1], None);
        assert_eq!(chip.fans[2], None);
        assert_eq!(chip.controls[0], Some(50.0));
    }

    #[test]
    fn pwm_write_and_restore_ext() {
        let sim = ite(0x2E, 0x8686);
        let mut chip = probe_io(&sim, 0).unwrap();
        {
            let mut s = sim.0.borrow_mut();
            s.ec[0x13] = 0x06; // wyjście PWM 1 wyłączone
            s.ec[0x15] = 0x81; // tryb automatyczny
            s.ec[0x63] = 0x40;
        }
        chip.set_io(&sim, 0, Some(100.0)).unwrap();
        chip.set_io(&sim, 0, Some(40.0)).unwrap();
        {
            let s = sim.0.borrow();
            assert_eq!(s.ec_writes, vec![(0x13, 0x07), (0x15, 0x01), (0x63, 255), (0x13, 0x07), (0x15, 0x01), (0x63, 102)]);
            assert_eq!(s.ec[0x63], 102);
        }
        sim.0.borrow_mut().ec_writes.clear();
        chip.set_io(&sim, 0, None).unwrap();
        let s = sim.0.borrow();
        assert_eq!(s.ec_writes, vec![(0x15, 0x81), (0x13, 0x06), (0x63, 0x40)]);
        assert_eq!((s.ec[0x13], s.ec[0x15], s.ec[0x63]), (0x06, 0x81, 0x40));
        drop(s);
        // Drugi powrót niczego nie zapisuje.
        sim.0.borrow_mut().ec_writes.clear();
        chip.set_io(&sim, 0, None).unwrap();
        assert!(sim.0.borrow().ec_writes.is_empty());
    }

    #[test]
    fn pwm_write_old_chip_and_it8689e() {
        let sim = ite(0x2E, 0x8716);
        let mut chip = probe_io(&sim, 0).unwrap();
        sim.0.borrow_mut().ec[0x13] = 0x07;
        sim.0.borrow_mut().ec[0x16] = 0x90;
        chip.set_io(&sim, 1, Some(50.0)).unwrap();
        assert_eq!(sim.0.borrow().ec_writes, vec![(0x16, 63)]);
        chip.set_io(&sim, 1, None).unwrap();
        assert_eq!(sim.0.borrow().ec[0x16], 0x90);

        let sim = ite(0x2E, 0x8689);
        let mut chip = probe_io(&sim, 0).unwrap();
        sim.0.borrow_mut().ec[0xA3] = 0x80;
        chip.set_io(&sim, 4, Some(20.0)).unwrap();
        assert_eq!(sim.0.borrow().ec_writes, vec![(0xA7, 0x7F), (0xA3, 51)]);
        chip.restore_sim(&sim);
        assert_eq!(sim.0.borrow().ec[0xA3], 0x80);
        assert!(chip.set_io(&sim, 6, Some(20.0)).is_err());
        assert!(chip.set_io(&sim, 0, Some(f32::NAN)).is_err());
    }

    impl Ite {
        /// `restore_all` na symulacji (trait `Chip` wymaga `Lpc`).
        fn restore_sim(&mut self, io: &Sim) {
            for i in 0..self.model.controls {
                let _ = self.restore_io(io, i);
            }
        }
    }

    #[test]
    fn bank_select_only_when_needed() {
        let sim = ite(0x2E, 0x8665);
        let mut chip = probe_io(&sim, 0).unwrap();
        sim.0.borrow_mut().ec[0x06] = 0x05;
        chip.update_io(&sim).unwrap();
        assert!(sim.0.borrow().ec_writes.is_empty());
        sim.0.borrow_mut().ec[0x06] = 0x45;
        chip.update_io(&sim).unwrap();
        assert_eq!(sim.0.borrow().ec_writes, vec![(0x06, 0x05)]);
    }
}
