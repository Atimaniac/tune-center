//! Gigabyte RGB Fusion 2 na USB (układ ITE, 048D:8297, 8950, 5702, 5711).
//!
//! Raporty "feature" o 64 bajtach z numerem raportu 0xCC (strona użycia
//! 0xFF89):
//! - 0xCC 0x60 → potem odczyt raportu 0xCC: opis kontrolera (nazwa od
//!   bajtu 12, 28 znaków),
//! - 0xCC 0x31 0x00: wyłączenie efektu muzyki,
//! - efekt strefy: [0xCC, nagłówek, maska stref (u32 LE), maska 2 (u32),
//!   0, typ efektu, jasność maks., jasność min., kolor (u32 LE 0x00RRGGBB),
//!   …] - nagłówek 0x20 i maska 0xFF (0x07FF dla 5711) = wszystkie strefy,
//! - 0xCC 0x28 0xFF 0x00 (5711: 0x07): zastosowanie.
//!
//! Typ 0 = brak efektu (wyłączone), 1 = stały kolor. Nic nie jest zapisywane
//! w pamięci kontrolera. Nietestowane na sprzęcie.

use super::hid::{self, Hid, HidDevice};

const VID: u16 = 0x048D;
const PIDS: [u16; 4] = [0x8297, 0x8950, 0x5702, 0x5711];
const USAGE_PAGE: u16 = 0xFF89;
const REPORT: u8 = 0xCC;
const LEN: usize = 64;

struct Fusion {
    dev: HidDevice,
    pid: u16,
    id: String,
    name: String,
}

impl Fusion {
    fn send(&self, b: &[u8; LEN]) -> Result<(), String> {
        self.dev
            .send_feature_report(b)
            .map_err(|e| hid::err(&self.name, e))
    }

    fn short(&self, a: u8, b: u8, c: u8) -> Result<(), String> {
        let mut p = [0u8; LEN];
        p[0] = REPORT;
        p[1] = a;
        p[2] = b;
        p[3] = c;
        self.send(&p)
    }
}

/// Pakiet efektu dla wszystkich stref.
/// Efekty Fusion 2: 1 stały, 2 pulsowanie, 3 miganie, 4 cykl kolorów, 6 fala.
const EFFECTS: &[super::Effect] = &super::Effect::ALL;

/// Typ efektu i czasy (ms): narastanie, opadanie, przytrzymanie; parametr 0.
fn effect_timing(effect: super::Effect, speed: super::Speed) -> (u8, [u16; 3], u8) {
    use super::Effect;
    let t = speed.period_ms() / 4; // 375-2000 ms
    match effect {
        Effect::Static => (1, [0, 0, 0], 0),
        Effect::Breathing => (2, [t, t, 200], 0),
        Effect::Flashing => (3, [100, 100, t], 0),
        Effect::ColorCycle => (4, [t, t.saturating_sub(200), 0], 7),
        Effect::Rainbow => (6, [t / 4, 0, 0], 7),
    }
}

fn effect_packet(pid: u16, effect: u8, rgb: [u8; 3]) -> [u8; LEN] {
    effect_packet_timed(pid, effect, rgb, [0, 0, 0], 0)
}

fn effect_packet_timed(
    pid: u16,
    effect: u8,
    rgb: [u8; 3],
    periods: [u16; 3],
    param0: u8,
) -> [u8; LEN] {
    let mut p = [0u8; LEN];
    p[0] = REPORT;
    p[1] = 0x20;
    let zones: u32 = if pid == 0x5711 { 0x07FF } else { 0xFF };
    p[2..6].copy_from_slice(&zones.to_le_bytes());
    p[11] = effect;
    p[12] = 0xFF; // jasność maksymalna
    p[13] = 0x00; // jasność minimalna
    let color = u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]);
    p[14..18].copy_from_slice(&color.to_le_bytes());
    // Czasy efektu (u16 LE od bajtu 22) i parametr 0 (bajt 30).
    for (i, t) in periods.iter().enumerate() {
        p[22 + 2 * i..24 + 2 * i].copy_from_slice(&t.to_le_bytes());
    }
    if effect == 6 {
        p[31] = 1; // fala: parametr 1
    }
    p[30] = param0;
    p
}

impl super::Device for Fusion {
    fn info(&self) -> super::Info {
        super::Info {
            id: self.id.clone(),
            name: self.name.clone(),
            family: super::Family::GigabyteFusion,
            detail: format!("USB {VID:04X}:{:04X}", self.pid),
            restores: false,
            effects: EFFECTS.to_vec(),
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        let packet = if s.mode == super::Mode::Color {
            let (kind, periods, param0) = effect_timing(s.effect_for(EFFECTS), s.speed);
            effect_packet_timed(self.pid, kind, s.rgb(), periods, param0)
        } else {
            effect_packet(self.pid, 0, [0, 0, 0])
        };
        self.short(0x31, 0x00, 0x00)?;
        self.send(&packet)?;
        let apply = if self.pid == 0x5711 { 0x07 } else { 0x00 };
        self.short(0x28, 0xFF, apply)
    }

    fn release(&mut self) -> Result<(), String> {
        Ok(())
    }
}

pub fn discover(hid: &Hid) -> Result<Vec<Box<dyn super::Device>>, String> {
    let mut out: Vec<Box<dyn super::Device>> = Vec::new();
    for iface in hid
        .list(VID)
        .iter()
        .filter(|i| PIDS.contains(&i.pid) && i.usage_page == USAGE_PAGE)
    {
        let Ok(dev) = hid.open(iface) else {
            continue;
        };
        // Opis kontrolera (odczyt) - przy okazji sprawdzenie, że odpowiada.
        let mut req = [0u8; LEN];
        req[0] = REPORT;
        req[1] = 0x60;
        if dev.send_feature_report(&req).is_err() {
            continue;
        }
        let mut r = [0u8; LEN];
        r[0] = REPORT;
        let product = match dev.get_feature_report(&mut r) {
            Ok(n) if n >= 40 => String::from_utf8_lossy(&r[12..40])
                .trim_matches(char::from(0))
                .trim()
                .to_owned(),
            _ => continue,
        };
        let name = if product.is_empty() {
            "Gigabyte RGB Fusion 2".to_owned()
        } else {
            format!("Gigabyte RGB Fusion 2 ({product})")
        };
        out.push(Box::new(Fusion {
            dev,
            pid: iface.pid,
            id: hid::id("fusion2", iface, 0),
            name,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn effect_layout() {
        let p = super::effect_packet(0x8297, 1, [0x11, 0x22, 0x33]);
        assert_eq!(&p[..6], &[0xCC, 0x20, 0xFF, 0, 0, 0]);
        assert_eq!(p[11], 1);
        assert_eq!(&p[14..18], &[0x33, 0x22, 0x11, 0x00]);
    }
}
