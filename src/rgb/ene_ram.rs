//! Podświetlenie pamięci RAM (kontrolery ENE na SMBus): kolor, wyłączenie,
//! przywrócenie efektu.
//!
//! Moduły RAM z podświetleniem RGB (m.in. G.Skill Trident Z RGB, wiele
//! modułów ASUS/ENE) mają kontroler ENE na magistrali SMBus chipsetu. Dostęp
//! do SMBus daje oficjalny moduł PawnIO `SmbusI801.bin` (chipsety Intel), pod
//! blokadą "Global\Access_SMBUS.HTP.Method", tak jak w LibreHardwareMonitor,
//! HWiNFO i OpenRGB. Wymaga uprawnień administratora i sterownika PawnIO.
//!
//! Protokół ENE (publicznie znane fakty, kod napisany od zera): rejestr
//! 16-bitowy wybiera się zapisem słowa (bajty zamienione) do komendy 0x00,
//! wartość czyta się bajtem z komendy 0x81, a zapisuje bajtem do komendy
//! 0x01. Podpis kontrolera: rejestry SMBus 0xA0..0xAF zwracają 0x00..0x0F.
//! Tabela konfiguracji od 0x1C00 (bajt 0x02 = liczba diod), kolory efektów
//! od 0x8010 (nowsze kontrolery 0x8160), po 3 bajty na diodę w kolejności
//! R, B, G; tryb 0x8021 (0 = wyłączone, 1 = stały kolor, 5 = tęcza).
//!
//! Bezpieczeństwo:
//! - piszemy wyłącznie pod adresy 0x70..0x77 i tylko do kontrolera, który
//!   przeszedł test podpisu (sam odczyt); pamięć SPD modułów (0x50..0x57)
//!   nie jest nigdy adresowana,
//! - nie przemapowujemy adresów: po włączeniu komputera wszystkie moduły
//!   odpowiadają wspólnie pod 0x77 i jeden zapis zmienia je wszystkie,
//! - nie zapisujemy niczego w pamięci flash kontrolera (bez "Save") - po
//!   restarcie moduły wracają do swojego trybu, a program ustawia go
//!   ponownie przy starcie,
//! - poprzedni tryb jest zapamiętywany, żeby można go było przywrócić.

use std::path::Path;

use crate::cpu::pawnio::{NamedLock, PawnIo};

pub(crate) const SMBUS_LOCK: &str = "Global\\Access_SMBUS.HTP.Method";

/// Adresy kontrolerów ENE pamięci RAM (0x77 = adres po włączeniu zasilania).
const ENE_ADDRESSES: std::ops::RangeInclusive<u8> = 0x70..=0x77;

const REG_DEVICE_NAME: u16 = 0x1000;
const REG_MICRON_CHECK: u16 = 0x1030;
const REG_CONFIG_TABLE: u16 = 0x1C00;
const CONFIG_LED_COUNT: u16 = 0x02;
const REG_COLORS_EFFECT: u16 = 0x8010;
const REG_COLORS_EFFECT_V2: u16 = 0x8160;
const REG_DIRECT: u16 = 0x8020;
const REG_MODE: u16 = 0x8021;
const REG_SPEED: u16 = 0x8022;
const REG_APPLY: u16 = 0x80A0;
const APPLY: u8 = 0x01;
const MODE_OFF: u8 = 0;
const MODE_STATIC: u8 = 1;
const MODE_RAINBOW: u8 = 5;
/// Rejestry kolorów mieszczą najwyżej 10 diod.
const MAX_LEDS: u8 = 10;

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

    pub(crate) fn read_byte_data(&self, addr: u8, cmd: u8) -> Result<u8, String> {
        self.xfer(addr, READ, cmd, PROTO_BYTE_DATA, None)
            .map(|v| v as u8)
    }

    pub(crate) fn write_byte_data(&self, addr: u8, cmd: u8, value: u8) -> Result<(), String> {
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
pub struct Controller {
    pub addr: u8,
    pub name: String,
    pub mode: u8,
    pub direct: u8,
    /// Liczba diod (tabela konfiguracji, bajt 0x02).
    pub leds: u8,
}

/// Moduły PawnIO magistrali SMBus: chipsety Intel (I801) i AMD (PIIX4 /
/// FCH). Moduł bez swojego kontrolera odmawia wczytania.
const BUS_MODULES: [&str; 2] = ["SmbusI801.bin", "SmbusPIIX4.bin"];

fn module_dir() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("modules"))
}

/// Magistrala SMBus pamięci RAM - pierwszy moduł, który pasuje do chipsetu.
/// Na płytach AMD moduły RAM są na porcie 0 (domyślnym) kontrolera FCH.
pub(crate) fn open_bus() -> Result<Smbus, String> {
    let dir = module_dir().ok_or("brak ścieżki programu")?;
    let mut errors = Vec::new();
    for m in BUS_MODULES {
        match Smbus::open(&dir.join(m)) {
            Ok(bus) => return Ok(bus),
            Err(e) => errors.push(e),
        }
    }
    Err(crate::trf!(
        "Nie znaleziono obsługiwanego kontrolera SMBus (chipset Intel lub AMD): {}",
        "No supported SMBus controller found (Intel or AMD chipset): {}",
        errors.join(" | ")
    ))
}

/// Kontrolery ENE pamięci RAM (tylko odczyt).
fn find(bus: &Smbus) -> Vec<Controller> {
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
        let leds = ene
            .read(REG_CONFIG_TABLE + CONFIG_LED_COUNT)
            .unwrap_or(0)
            .min(MAX_LEDS);
        out.push(Controller {
            addr,
            name,
            mode,
            direct,
            leds,
        });
    }
    out
}

/// Wyszukuje kontrolery podświetlenia RAM (tylko odczyt).
pub fn scan() -> Result<Vec<Controller>, String> {
    let bus = open_bus()?;
    let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
    Ok(find(&bus))
}

/// Rejestr kolorów efektów: starsze kontrolery 0x8010, nowsze (AUDA0/AUMA0) 0x8160.
fn effect_colors_reg(name: &str) -> u16 {
    if name.starts_with("AUDA0") || name.starts_with("AUMA0") {
        REG_COLORS_EFFECT_V2
    } else {
        REG_COLORS_EFFECT
    }
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

/// Kolor wszystkich diod efektu (kolejność bajtów w kontrolerze: R, B, G).
fn set_color(bus: &Smbus, c: &Controller, rgb: [u8; 3]) -> Result<(), String> {
    let ene = Ene { bus, addr: c.addr };
    let base = effect_colors_reg(&c.name);
    for led in 0..c.leds as u16 {
        let reg = base + led * 3;
        ene.write(reg, rgb[0])?;
        ene.write(reg + 1, rgb[2])?;
        ene.write(reg + 2, rgb[1])?;
    }
    // Sprawdzenie odczytem pierwszej diody.
    if c.leds > 0 {
        let read = [ene.read(base)?, ene.read(base + 2)?, ene.read(base + 1)?];
        if read != rgb {
            return Err(crate::trf!(
                "0x{:02X}: po zapisie odczytano kolor {:?} zamiast {:?}",
                "0x{:02X}: read back color {:?} instead of {:?}",
                c.addr,
                read,
                rgb
            ));
        }
    }
    Ok(())
}

/// Pamięć RAM jako jedno urządzenie (wszystkie kontrolery naraz).
struct Ram {
    controllers: Vec<Controller>,
    /// Tryby sprzed pierwszej zmiany - wspólne z konfiguracją programu.
    saved: super::RamSaved,
}

impl Ram {
    /// Zapamiętuje tryby przed pierwszą zmianą (do przywrócenia efektu).
    fn remember(&self, now: &[Controller]) {
        let Ok(mut saved) = self.saved.lock() else {
            return;
        };
        if saved.is_empty() {
            // Tryby, które ustawia sam program (wyłączone, stały kolor), nie są
            // efektem urządzenia - wtedy przy oddaniu wraca tryb fabryczny.
            *saved = now
                .iter()
                .filter(|c| !matches!(c.mode, 0..=4) || c.direct != 0)
                .map(|c| (c.addr, c.direct, c.mode))
                .collect();
        }
    }
}

impl super::Device for Ram {
    fn info(&self) -> super::Info {
        let names: Vec<String> = self
            .controllers
            .iter()
            .map(|c| format!("{} (0x{:02X}, {} LED)", c.name, c.addr, c.leds))
            .collect();
        super::Info {
            id: "ram-ene".into(),
            name: crate::i18n::tr("Pamięć RAM", "RAM").into(),
            family: super::Family::EneRam,
            detail: names.join(", "),
            restores: true,
            effects: EFFECTS.to_vec(),
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        let bus = open_bus()?;
        let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
        let now = find(&bus);
        if now.is_empty() {
            return Err(crate::i18n::tr(
                "Kontrolery RAM przestały odpowiadać.",
                "The RAM controllers stopped responding.",
            )
            .into());
        }
        self.remember(&now);
        let mut errors = Vec::new();
        for c in &now {
            let r = match s.mode {
                super::Mode::Color => {
                    let (mode, speed) = effect_mode(s.effect_for(EFFECTS), s.speed);
                    set_color(&bus, c, s.rgb())
                        .and_then(|_| {
                            Ene {
                                bus: &bus,
                                addr: c.addr,
                            }
                            .write(REG_SPEED, speed)
                        })
                        .and_then(|_| set_mode(&bus, c.addr, 0, mode))
                }
                _ => set_mode(&bus, c.addr, 0, MODE_OFF),
            };
            if let Err(e) = r {
                errors.push(e);
            }
        }
        self.controllers = now;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn release(&mut self) -> Result<(), String> {
        let saved = self.saved.lock().map(|s| s.clone()).unwrap_or_default();
        let bus = open_bus()?;
        let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
        let mut errors = Vec::new();
        for c in find(&bus) {
            // Bez zapamiętanego trybu: tryb fabryczny "tęcza".
            let (direct, mode) = saved
                .iter()
                .find(|s| s.0 == c.addr)
                .map(|s| (s.1, s.2))
                .unwrap_or((0, MODE_RAINBOW));
            if let Err(e) = set_mode(&bus, c.addr, direct, mode) {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            if let Ok(mut s) = self.saved.lock() {
                s.clear();
            }
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

/// Wyszukiwanie przy starcie i na żądanie (wymaga administratora i PawnIO).
pub fn discover(
    elevated: bool,
    saved: super::RamSaved,
) -> Result<Vec<Box<dyn super::Device>>, String> {
    if !elevated {
        return Err(crate::i18n::tr(
            "Podświetlenie RAM wymaga uprawnień administratora (przycisk w pasku stanu) i sterownika PawnIO.",
            "RAM lighting requires administrator rights (button in the status bar) and the PawnIO driver.",
        )
        .into());
    }
    let controllers = scan()?;
    if controllers.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![Box::new(Ram { controllers, saved })])
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
                for c in &r {
                    println!(
                        "0x{:02X}: \"{}\" tryb {} direct {} diod {}",
                        c.addr, c.name, c.mode, c.direct, c.leds
                    );
                }
                println!("kontrolerów: {}", r.len());
            }
            Err(e) => println!("błąd: {e}"),
        }
    }
}

#[cfg(test)]
mod hardware_write {
    //! Kolor, wyłączenie i przywrócenie podświetlenia RAM (zapis!):
    //! `cargo test leds_color_off_on -- --ignored --nocapture`.
    use super::super::{Device, Mode, Setting};
    use std::sync::{Arc, Mutex};

    #[test]
    #[ignore]
    fn leds_color_off_on() {
        let before = super::scan().expect("odczyt");
        let saved = Arc::new(Mutex::new(Vec::new()));
        let mut d = super::discover(true, saved.clone())
            .expect("wyszukiwanie")
            .pop()
            .expect("brak kontrolerów");
        println!("{:?}", d.info());
        let color = Setting {
            mode: Mode::Color,
            color: [255, 0, 0],
            brightness: 60,
            ..Default::default()
        };
        println!("czerwony: {:?}", d.apply(&color));
        std::thread::sleep(std::time::Duration::from_secs(3));
        println!(
            "wyłączenie: {:?}",
            d.apply(&Setting {
                mode: Mode::Off,
                ..color
            })
        );
        std::thread::sleep(std::time::Duration::from_secs(3));
        println!("zapamiętane: {:?}", saved.lock().unwrap());
        println!("przywrócenie: {:?}", d.release());
        println!("teraz: {:?}", super::scan().unwrap());
        // Stan sprzed testu (np. wyłączone przez zainstalowany program).
        std::thread::sleep(std::time::Duration::from_secs(3));
        let bus = super::open_bus().unwrap();
        let _lock = super::NamedLock::acquire(super::SMBUS_LOCK, 2000).unwrap();
        for c in &before {
            println!(
                "stan sprzed testu 0x{:02X}: {:?}",
                c.addr,
                super::set_mode(&bus, c.addr, c.direct, c.mode)
            );
        }
    }
}

/// Efekty kontrolera ENE (tryby 1-5 rejestru 0x8021).
const EFFECTS: &[super::Effect] = &super::Effect::ALL;

/// Tryb i szybkość (rejestr 0x8022: 0 najszybciej … 4 najwolniej).
fn effect_mode(effect: super::Effect, speed: super::Speed) -> (u8, u8) {
    use super::Effect;
    let mode = match effect {
        Effect::Static => MODE_STATIC,
        Effect::Breathing => 2,
        Effect::Flashing => 3,
        Effect::ColorCycle => 4,
        Effect::Rainbow => MODE_RAINBOW,
    };
    (mode, 3 - speed.level())
}

/// Przywraca tryb kontrolera z odczytu (testy).
#[cfg(test)]
pub(crate) fn restore_mode(bus: &Smbus, c: &Controller) -> Result<(), String> {
    let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
    set_mode(bus, c.addr, c.direct, c.mode)
}
