//! Urządzenia Logitech G przez protokół HID++ 2.0 (klawiatury, myszy,
//! odbiorniki LIGHTSPEED).
//!
//! HID++ 2.0: długi raport 0x11 o 20 bajtach - [0x11, numer urządzenia,
//! indeks funkcji, (numer polecenia << 4) | identyfikator programu,
//! 16 bajtów parametrów]. Numer urządzenia 0xFF = urządzenie na kablu,
//! 1..6 = urządzenia sparowane z odbiornikiem. Indeks funkcji podaje funkcja
//! główna 0x0000 (polecenie 0: identyfikator funkcji → indeks). Błąd ma
//! indeks 0xFF i kod w 5. bajcie.
//!
//! Podświetlenie:
//! - 0x8071 (RGB Effects, nowsze urządzenia): polecenie 0 = informacje
//!   (grupy diod, efekty grupy i ich identyfikatory), 1 = efekt grupy,
//!   5 = sterowanie przez program,
//! - 0x8070 (Color LED Effects, starsze): 0 = liczba stref, 1 = strefa,
//!   2 = efekt strefy, 3 = ustaw efekt.
//!
//! Identyfikator efektu 0x0000 = wyłączone, 0x0001 = stały kolor. Efekty nie
//! są zapisywane w pamięci urządzenia (bez trwałości), więc po ponownym
//! podłączeniu urządzenie wraca do swojego efektu.

use std::time::{Duration, Instant};

use super::hid::{Hid, HidDevice, Iface};

const VID: u16 = 0x046D;
const LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
/// Identyfikator naszego programu w poleceniach (odróżnia odpowiedzi).
const SW_ID: u8 = 0x0B;
const TIMEOUT: Duration = Duration::from_millis(400);

const F_NAME: u16 = 0x0005;
const F_RGB1: u16 = 0x8070;
const F_RGB2: u16 = 0x8071;

const EFFECT_OFF: u16 = 0x0000;
const EFFECT_FIXED: u16 = 0x0001;
const EFFECT_CYCLE: u16 = 0x0003;
const EFFECT_WAVE: u16 = 0x0004;
const EFFECT_BREATHING: u16 = 0x000A;

/// Połączenie z jednym urządzeniem HID++ (kabel albo pozycja w odbiorniku).
struct Link {
    dev: HidDevice,
    index: u8,
}

impl Link {
    /// Polecenie i odpowiedź (16 bajtów parametrów).
    fn request(&self, feature: u8, function: u8, params: &[u8]) -> Result<[u8; 16], String> {
        let mut buf = [0u8; LONG_LEN];
        buf[0] = LONG;
        buf[1] = self.index;
        buf[2] = feature;
        buf[3] = (function << 4) | SW_ID;
        buf[4..4 + params.len().min(16)].copy_from_slice(&params[..params.len().min(16)]);
        // Stare odpowiedzi (np. powiadomienia) nie mogą pomylić się z naszą.
        let mut junk = [0u8; 64];
        while matches!(self.dev.read_timeout(&mut junk, 0), Ok(n) if n > 0) {}
        self.dev
            .write(&buf)
            .map_err(|e| format!("HID++ {:02X}: {e}", self.index))?;
        let deadline = Instant::now() + TIMEOUT;
        let mut r = [0u8; 64];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(crate::i18n::tr(
                    "urządzenie nie odpowiada",
                    "the device does not respond",
                )
                .into());
            }
            let n = self
                .dev
                .read_timeout(&mut r, left.as_millis() as i32)
                .map_err(|e| format!("HID++: {e}"))?;
            if n < 7 || r[1] != self.index {
                continue;
            }
            // Błąd HID++ 2.0: [.., 0xFF, indeks, polecenie, kod].
            if r[2] == 0xFF && r[3] == feature && r[4] == buf[3] {
                return Err(crate::trf!("błąd HID++ {}", "HID++ error {}", r[5]));
            }
            // Błąd HID++ 1.0 (np. brak urządzenia na tej pozycji odbiornika).
            if r[2] == 0x8F {
                return Err(crate::i18n::tr("brak urządzenia", "no device").into());
            }
            if r[0] == LONG && n >= LONG_LEN && r[2] == feature && r[3] == buf[3] {
                let mut out = [0u8; 16];
                out.copy_from_slice(&r[4..20]);
                return Ok(out);
            }
        }
    }

    /// Indeks funkcji (None = urządzenie jej nie ma).
    fn feature(&self, id: u16) -> Result<Option<u8>, String> {
        let r = self.request(0x00, 0, &[(id >> 8) as u8, id as u8])?;
        Ok((r[0] != 0).then_some(r[0]))
    }

    /// Nazwa urządzenia z funkcji 0x0005.
    fn name(&self) -> Option<String> {
        let f = self.feature(F_NAME).ok()??;
        let len = self.request(f, 0, &[]).ok()?[0] as usize;
        let mut bytes = Vec::new();
        while bytes.len() < len.min(64) {
            let part = self.request(f, 1, &[bytes.len() as u8]).ok()?;
            let take = (len - bytes.len()).min(16);
            bytes.extend_from_slice(&part[..take]);
        }
        let name = String::from_utf8_lossy(&bytes)
            .trim_matches(char::from(0))
            .trim()
            .to_owned();
        (!name.is_empty()).then_some(name)
    }
}

/// Grupa diod (strefa) z indeksami efektów "stały kolor" i "wyłączone".
#[derive(Clone, Debug)]
struct Zone {
    index: u8,
    fixed: Option<u8>,
    off: Option<u8>,
    breathing: Option<u8>,
    cycle: Option<u8>,
    wave: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Api {
    /// 0x8071 z indeksem funkcji.
    Rgb2(u8),
    /// 0x8070 z indeksem funkcji.
    Rgb1(u8),
}

fn be16(a: u8, b: u8) -> u16 {
    ((a as u16) << 8) | b as u16
}

/// Strefy i ich efekty (tylko odczyty).
fn zones(link: &Link, api: Api) -> Result<Vec<Zone>, String> {
    let mut out = Vec::new();
    match api {
        Api::Rgb2(f) => {
            let info = link.request(f, 0, &[0xFF, 0xFF])?;
            let count = be16(info[1], info[2]).min(16) as u8;
            for z in 0..count {
                let zi = link.request(f, 0, &[z, 0xFF])?;
                let effects = zi[4].min(32);
                let mut zone = Zone {
                    index: z,
                    fixed: None,
                    off: None,
                    breathing: None,
                    cycle: None,
                    wave: None,
                };
                for e in 0..effects {
                    let ei = link.request(f, 0, &[z, e])?;
                    match be16(ei[2], ei[3]) {
                        EFFECT_FIXED => zone.fixed = zone.fixed.or(Some(e)),
                        EFFECT_CYCLE => zone.cycle = zone.cycle.or(Some(e)),
                        EFFECT_WAVE => zone.wave = zone.wave.or(Some(e)),
                        EFFECT_BREATHING => zone.breathing = zone.breathing.or(Some(e)),
                        EFFECT_OFF => zone.off = zone.off.or(Some(e)),
                        _ => {}
                    }
                }
                out.push(zone);
            }
        }
        Api::Rgb1(f) => {
            let count = link.request(f, 0, &[])?[0].min(16);
            for z in 0..count {
                let zi = link.request(f, 1, &[z])?;
                let effects = zi[3].min(32);
                let mut zone = Zone {
                    index: z,
                    fixed: None,
                    off: None,
                    breathing: None,
                    cycle: None,
                    wave: None,
                };
                for e in 0..effects {
                    let ei = link.request(f, 2, &[z, e])?;
                    match be16(ei[2], ei[3]) {
                        EFFECT_FIXED => zone.fixed = zone.fixed.or(Some(e)),
                        EFFECT_CYCLE => zone.cycle = zone.cycle.or(Some(e)),
                        EFFECT_WAVE => zone.wave = zone.wave.or(Some(e)),
                        EFFECT_BREATHING => zone.breathing = zone.breathing.or(Some(e)),
                        EFFECT_OFF => zone.off = zone.off.or(Some(e)),
                        _ => {}
                    }
                }
                out.push(zone);
            }
        }
    }
    Ok(out)
}

struct Logi {
    link: Link,
    api: Api,
    zones: Vec<Zone>,
    id: String,
    name: String,
    wired: bool,
    /// Program przejął sterowanie (0x8071, polecenie 5).
    owned: bool,
}

impl Logi {
    /// Efekty dostępne w pierwszej strefie (klawisze / główna strefa myszy).
    fn effects(&self) -> Vec<super::Effect> {
        use super::Effect;
        let mut out = vec![Effect::Static];
        if let Some(z) = self.zones.first() {
            if z.breathing.is_some() {
                out.push(Effect::Breathing);
            }
            if z.cycle.is_some() {
                out.push(Effect::ColorCycle);
            }
            if z.wave.is_some() {
                out.push(Effect::Rainbow);
            }
        }
        out
    }

    fn set_zone(&self, zone: &Zone, s: &super::Setting) -> Result<(), String> {
        use super::Effect;
        let wanted = s.effect_for(&self.effects());
        let (effect, rgb) = match (s.mode, wanted) {
            (super::Mode::Color, Effect::Breathing) if zone.breathing.is_some() => {
                (zone.breathing, s.color)
            }
            (super::Mode::Color, Effect::ColorCycle) if zone.cycle.is_some() => {
                (zone.cycle, s.color)
            }
            (super::Mode::Color, Effect::Rainbow) if zone.wave.is_some() => (zone.wave, s.color),
            (super::Mode::Color, _) => (zone.fixed, s.rgb()),
            // Bez efektu "wyłączone" - stały czarny kolor.
            _ => (zone.off.or(zone.fixed), [0, 0, 0]),
        };
        let Some(effect) = effect else {
            return Ok(());
        };
        let mut p = [0u8; 16];
        p[0] = zone.index;
        p[1] = effect;
        p[2..5].copy_from_slice(&rgb);
        // Parametry efektu: okres w ms (big-endian) i jasność 0-100.
        let period = s.speed.period_ms().to_be_bytes();
        let bright = s.brightness.min(100);
        if Some(effect) == zone.breathing {
            p[5..7].copy_from_slice(&period);
            p[8] = bright;
        } else if Some(effect) == zone.cycle {
            p[7..9].copy_from_slice(&period);
            p[9] = bright;
        } else if Some(effect) == zone.wave {
            p[8] = period[1];
            p[9] = 0x01; // kierunek: poziomo
            p[10] = bright;
            p[11] = period[0];
        }
        match self.api {
            Api::Rgb2(f) => {
                if Some(effect) == zone.fixed {
                    p[5] = 0x02;
                }
                // Trwałość: tylko do odłączenia zasilania.
                p[12] = 0x01;
                self.link.request(f, 1, &p).map(|_| ())
            }
            Api::Rgb1(f) => self.link.request(f, 3, &p).map(|_| ()),
        }
    }
}

impl super::Device for Logi {
    fn info(&self) -> super::Info {
        let place = if self.wired {
            crate::i18n::tr("kabel USB", "USB cable").to_owned()
        } else {
            crate::trf!(
                "odbiornik, pozycja {}",
                "receiver, slot {}",
                self.link.index
            )
        };
        let api = match self.api {
            Api::Rgb2(_) => "0x8071",
            Api::Rgb1(_) => "0x8070",
        };
        super::Info {
            id: self.id.clone(),
            name: self.name.clone(),
            family: super::Family::Logitech,
            detail: crate::trf!(
                "{place}, HID++ {api}, stref: {}",
                "{place}, HID++ {api}, zones: {}",
                self.zones.len()
            ),
            restores: matches!(self.api, Api::Rgb2(_)),
            effects: self.effects(),
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        if let (Api::Rgb2(f), false) = (self.api, self.owned) {
            // Sterowanie przez program: kolory (bit 0) i zasilanie diod (bit 1).
            self.link.request(f, 5, &[1, 3, 5])?;
            self.owned = true;
        }
        let mut errors = Vec::new();
        for z in &self.zones {
            if let Err(e) = self.set_zone(z, s) {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("{}: {}", self.name, errors.join("; ")))
        }
    }

    fn release(&mut self) -> Result<(), String> {
        if let (Api::Rgb2(f), true) = (self.api, self.owned) {
            self.link.request(f, 5, &[1, 0, 0])?;
            self.owned = false;
        }
        Ok(())
    }
}

/// Czy interfejs przyjmuje długie raporty HID++.
fn is_hidpp_long(i: &Iface) -> bool {
    (i.usage_page == 0xFF43 && i.usage == 0x0602) || (i.usage_page == 0xFF00 && i.usage == 0x0002)
}

/// Urządzenie z podświetleniem pod danym numerem (None = brak / bez RGB).
fn probe(hid: &Hid, iface: &Iface, index: u8) -> Option<Logi> {
    let link = Link {
        dev: hid.open(iface).ok()?,
        index,
    };
    let api = if let Ok(Some(f)) = link.feature(F_RGB2) {
        Api::Rgb2(f)
    } else if let Ok(Some(f)) = link.feature(F_RGB1) {
        Api::Rgb1(f)
    } else {
        return None;
    };
    let zones = zones(&link, api).ok()?;
    if zones.iter().all(|z| z.fixed.is_none()) {
        return None;
    }
    let name = link.name().unwrap_or_else(|| iface.product.clone());
    // "G915 TKL LIGHTSPEED Wireless RGB Mechanical Gaming Keyboard" → "G915 TKL".
    let name = ["LIGHTSPEED", "Wireless", "Wired", "RGB"]
        .iter()
        .filter_map(|w| name.find(&format!(" {w}")))
        .min()
        .map_or(name.as_str(), |cut| &name[..cut])
        .to_owned();
    Some(Logi {
        api,
        zones,
        id: format!("logitech:{:04X}:{:04X}:{index:02X}", iface.vid, iface.pid),
        name: format!("Logitech {}", name.trim_start_matches("Logitech ").trim()),
        wired: index == 0xFF,
        owned: false,
        link,
    })
}

pub fn discover(hid: &Hid) -> Result<Vec<Box<dyn super::Device>>, String> {
    let mut out: Vec<Box<dyn super::Device>> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for iface in hid.list(VID).iter().filter(|i| is_hidpp_long(i)) {
        if !seen.insert(iface.path.clone()) {
            continue;
        }
        // Najpierw urządzenie na kablu, potem pozycje odbiornika.
        if let Some(d) = probe(hid, iface, 0xFF) {
            out.push(Box::new(d));
            continue;
        }
        for index in 1..=6 {
            if let Some(d) = probe(hid, iface, index) {
                out.push(Box::new(d));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod hardware {
    use super::super::{Device, Mode, Setting};

    /// Lista urządzeń Logitech z podświetleniem (tylko odczyt):
    /// `cargo test logitech_scan -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn logitech_scan() {
        let hid = super::Hid::new().unwrap();
        for i in hid.list(super::VID) {
            println!(
                "{:04X}:{:04X} if {} page {:04X} usage {:04X} \"{}\"",
                i.vid, i.pid, i.interface, i.usage_page, i.usage, i.product
            );
        }
        for d in super::discover(&hid).unwrap() {
            println!("{:?}", d.info());
        }
    }

    /// Kolor, wyłączenie i oddanie sterowania (zapis, bez pamięci flash):
    /// `cargo test logitech_color -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn logitech_color() {
        let hid = super::Hid::new().unwrap();
        for mut d in super::discover(&hid).unwrap() {
            println!("{:?}", d.info());
            let s = Setting {
                mode: Mode::Color,
                color: [255, 0, 0],
                brightness: 100,
                ..Default::default()
            };
            println!("czerwony: {:?}", d.apply(&s));
            std::thread::sleep(std::time::Duration::from_secs(3));
            println!(
                "zielony 30%: {:?}",
                d.apply(&Setting {
                    color: [0, 255, 0],
                    brightness: 30,
                    ..s
                })
            );
            std::thread::sleep(std::time::Duration::from_secs(3));
            println!(
                "wyłączone: {:?}",
                d.apply(&Setting {
                    mode: Mode::Off,
                    ..s
                })
            );
            std::thread::sleep(std::time::Duration::from_secs(3));
            println!("oddanie: {:?}", d.release());
        }
    }
}
