//! Podświetlenie pamięci RAM Corsair (Vengeance RGB PRO, Dominator Platinum
//! RGB i podobne DDR4) na magistrali SMBus.
//!
//! Kontroler Corsair odpowiada pod adresami 0x58-0x5F. Rozpoznanie (sam
//! odczyt): rejestr 0x43 = 0x1A / 0x1B / 0x1C, rejestr 0x44 = 0x01 / 0x03 /
//! 0x04. Konfigurację efektu wysyła się jako 20 bajtów przez bufor
//! kontrolera:
//! - 0x0B ← 0 (wyczyszczenie bufora), 0x21 ← 0 (początek danych),
//! - 20 × 0x20 ← bajt danych,
//! - 0x42 → suma kontrolna CRC-8 (wielomian 0x07) bufora; zgodna z naszą →
//! - 0x82 ← 1 (zastosowanie konfiguracji efektu), oczekiwanie, aż bit 3
//!   rejestru stanu 0x30 zgaśnie.
//!
//! Efekt: [tryb, szybkość, kolory własne (1), kierunek, R, G, B, jasność,
//! R2, G2, B2, jasność2, 0…]; tryb 0x10 = stały kolor. Wyłączenie = stały
//! czarny. Sprawdzone na Corsair Vengeance RGB PRO (CMW16GX4M2C3200C16, płyta
//! MSI B450 z chipsetem AMD) - zgłoszenie #1.

use super::ene_ram::{open_bus, Smbus, SMBUS_LOCK};
use crate::cpu::pawnio::NamedLock;

const ADDRESSES: std::ops::RangeInclusive<u8> = 0x58..=0x5F;
const REG_RESET_BUFFER: u8 = 0x0B;
const REG_SET_DATA: u8 = 0x20;
const REG_DATA_START: u8 = 0x21;
const REG_STATUS: u8 = 0x30;
const REG_CHECKSUM: u8 = 0x42;
const REG_APPLY: u8 = 0x82;
const ID_EFFECT: u8 = 1;
const MODE_STATIC: u8 = 0x10;
const CUSTOM_COLORS: u8 = 0x01;

/// CRC-8, wielomian x^8 + x^2 + x + 1 (0x07), wartość początkowa 0.
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Konfiguracja efektu „stały kolor”.
fn effect(rgb: [u8; 3]) -> [u8; 20] {
    let mut e = [0u8; 20];
    e[0] = MODE_STATIC;
    e[2] = CUSTOM_COLORS;
    e[4..7].copy_from_slice(&rgb);
    e[7] = 0xFF;
    e[8..11].copy_from_slice(&rgb);
    e[11] = 0xFF;
    e
}

/// Rozpoznanie kontrolera - wyłącznie odczyty.
fn is_corsair(bus: &Smbus, addr: u8) -> bool {
    matches!(bus.read_byte_data(addr, 0x43), Ok(0x1A..=0x1C))
        && matches!(bus.read_byte_data(addr, 0x44), Ok(0x01 | 0x03 | 0x04))
}

fn find(bus: &Smbus) -> Vec<u8> {
    ADDRESSES.filter(|&a| is_corsair(bus, a)).collect()
}

fn send_effect(bus: &Smbus, addr: u8, data: &[u8; 20]) -> Result<(), String> {
    bus.write_byte_data(addr, REG_RESET_BUFFER, 0)?;
    bus.write_byte_data(addr, REG_DATA_START, 0)?;
    for &b in data {
        bus.write_byte_data(addr, REG_SET_DATA, b)?;
    }
    let device = bus.read_byte_data(addr, REG_CHECKSUM)?;
    let ours = crc8(data);
    if device != ours {
        return Err(crate::trf!(
            "0x{addr:02X}: suma kontrolna {device:02X} zamiast {ours:02X} - konfiguracja nie została zastosowana",
            "0x{addr:02X}: checksum {device:02X} instead of {ours:02X} - the configuration was not applied"
        ));
    }
    bus.write_byte_data(addr, REG_APPLY, ID_EFFECT)?;
    for _ in 0..20 {
        if bus
            .read_byte_data(addr, REG_STATUS)
            .is_ok_and(|s| s & 0x08 == 0)
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err(crate::trf!(
        "0x{addr:02X}: kontroler nie zgłosił gotowości",
        "0x{addr:02X}: the controller did not report ready"
    ))
}

struct CorsairRam {
    addresses: Vec<u8>,
}

impl super::Device for CorsairRam {
    fn info(&self) -> super::Info {
        let list: Vec<String> = self
            .addresses
            .iter()
            .map(|a| format!("0x{a:02X}"))
            .collect();
        super::Info {
            id: "ram-corsair".into(),
            name: crate::i18n::tr("Pamięć RAM Corsair", "Corsair RAM").into(),
            family: super::Family::CorsairRam,
            detail: crate::trf!("moduły: {}", "modules: {}", list.join(", ")),
            restores: false,
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        let bus = open_bus()?;
        let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
        let data = effect(s.rgb());
        let errors: Vec<String> = self
            .addresses
            .iter()
            .filter_map(|&a| send_effect(&bus, a, &data).err())
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn release(&mut self) -> Result<(), String> {
        // Poprzedniego efektu nie da się odczytać - wraca po wyłączeniu zasilania.
        Ok(())
    }
}

/// Wyszukiwanie (wymaga administratora i PawnIO; sam odczyt).
pub fn discover(elevated: bool) -> Result<Vec<Box<dyn super::Device>>, String> {
    if !elevated {
        return Ok(Vec::new());
    }
    // Błąd magistrali zgłasza już wyszukiwanie kontrolerów ENE.
    let Ok(bus) = open_bus() else {
        return Ok(Vec::new());
    };
    let _lock = NamedLock::acquire(SMBUS_LOCK, 2000)?;
    let addresses = find(&bus);
    if addresses.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![Box::new(CorsairRam { addresses })])
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_and_effect() {
        // CRC-8 (0x07) dla "123456789" = 0xF4.
        assert_eq!(super::crc8(b"123456789"), 0xF4);
        let e = super::effect([1, 2, 3]);
        assert_eq!(&e[..12], &[0x10, 0, 1, 0, 1, 2, 3, 0xFF, 1, 2, 3, 0xFF]);
        assert!(e[12..].iter().all(|&b| b == 0));
    }
}
