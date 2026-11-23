//! MSI Mystic Light (płyty główne MSI, kontroler USB 1462:7Bxx-7Exx).
//!
//! Stan wszystkich stref to jeden raport "feature" 0x52 o długości 112, 162
//! albo 185 bajtów (zależnie od płyty). Strefa zajmuje 10 bajtów: efekt,
//! kolor R,G,B, szybkość i jasność (bity 2-6, 10 = 100 %), drugi kolor,
//! flagi koloru (bit 7 = własny kolor), wypełnienie. Ostatni bajt raportu
//! = zapis do pamięci (zawsze 0).
//!
//! Program czyta raport, zmienia efekt i kolor stref, i zapisuje go z
//! powrotem - pozostałe ustawienia zostają. Oryginalny raport jest
//! zapamiętywany, więc oddanie sterowania przywraca poprzedni efekt.
//! Efekt 0 = wyłączone, 1 = stały kolor. Nietestowane na sprzęcie.

use super::hid::{self, Hid, HidDevice};

const VID: u16 = 0x1462;
const REPORT: u8 = 0x52;
/// Efekty: 1 stały, 2 oddychanie, 3 miganie, 22 przesuwanie kolorów, 26 fala tęczy.
const EFFECTS: &[super::Effect] = &super::Effect::ALL;

/// Początki stref (pierwszy bajt = efekt) dla danej długości raportu.
fn zones(len: usize) -> Option<(Vec<usize>, Vec<usize>)> {
    // (strefy z jasnością i flagami, strefy tylko z efektem i kolorem)
    match len {
        185 => {
            let mut full = vec![1, 11, 21, 31, 42];
            full.extend((0..12).map(|k| 64 + 10 * k));
            Some((full, vec![53]))
        }
        162 => Some(((0..16).map(|k| 1 + 10 * k).collect(), Vec::new())),
        112 => Some(((0..11).map(|k| 1 + 10 * k).collect(), Vec::new())),
        _ => None,
    }
}

/// Raport ze zmienionym efektem i kolorem wszystkich stref.
#[cfg_attr(not(test), allow(dead_code))]
fn modified(original: &[u8], effect: u8, rgb: [u8; 3]) -> Vec<u8> {
    modified_speed(original, effect, rgb, None)
}

/// Jak `modified`, z szybkością efektu (bity 0-1: 0 wolno … 2 szybko).
fn modified_speed(original: &[u8], effect: u8, rgb: [u8; 3], speed: Option<u8>) -> Vec<u8> {
    let mut p = original.to_vec();
    let Some((full, simple)) = zones(p.len()) else {
        return p;
    };
    for &z in full.iter().chain(&simple) {
        p[z] = effect;
        p[z + 1..z + 4].copy_from_slice(&rgb);
    }
    for &z in &full {
        // Jasność 100 % (poziom 10); szybkość podana albo bez zmian.
        p[z + 4] = (p[z + 4] & !0x7C) | (10 << 2);
        if let Some(sp) = speed {
            p[z + 4] = (p[z + 4] & !0x03) | (sp & 0x03);
        }
        p[z + 8] |= 0x80;
    }
    let last = p.len() - 1;
    p[last] = 0;
    p
}

struct Mystic {
    dev: HidDevice,
    pid: u16,
    id: String,
    original: Vec<u8>,
    changed: bool,
}

impl Mystic {
    fn read(&self) -> Result<Vec<u8>, String> {
        read_report(&self.dev).ok_or_else(|| "MSI Mystic Light: brak odczytu".to_owned())
    }

    fn write(&self, p: &[u8]) -> Result<(), String> {
        self.dev
            .send_feature_report(p)
            .map_err(|e| hid::err("MSI Mystic Light", e))
    }
}

fn read_report(dev: &HidDevice) -> Option<Vec<u8>> {
    let mut buf = [0u8; 186];
    buf[0] = REPORT;
    let n = dev.get_feature_report(&mut buf).ok()?;
    // Niektóre sterowniki zwracają o bajt więcej.
    let len = [185, 162, 112]
        .into_iter()
        .find(|&l| n == l || n == l + 1)?;
    Some(buf[..len].to_vec())
}

impl super::Device for Mystic {
    fn info(&self) -> super::Info {
        super::Info {
            id: self.id.clone(),
            name: "MSI Mystic Light".into(),
            family: super::Family::MsiMystic,
            detail: crate::trf!(
                "USB {:04X}:{:04X}, raport {} B",
                "USB {:04X}:{:04X}, report {} B",
                VID,
                self.pid,
                self.original.len()
            ),
            restores: true,
            effects: EFFECTS.to_vec(),
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        if !self.changed {
            // Stan sprzed pierwszej zmiany (mógł się zmienić od wyszukania).
            self.original = self.read()?;
        }
        use super::Effect;
        let effect = match (s.mode, s.effect_for(EFFECTS)) {
            (super::Mode::Color, Effect::Static) => 1,
            (super::Mode::Color, Effect::Breathing) => 2,
            (super::Mode::Color, Effect::Flashing) => 3,
            (super::Mode::Color, Effect::ColorCycle) => 22,
            (super::Mode::Color, Effect::Rainbow) => 26,
            _ => 0,
        };
        self.write(&modified_speed(
            &self.original,
            effect,
            s.rgb(),
            Some(s.speed.level()),
        ))?;
        self.changed = true;
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        if self.changed {
            let mut p = self.original.clone();
            if let Some(last) = p.last_mut() {
                *last = 0;
            }
            self.write(&p)?;
            self.changed = false;
        }
        Ok(())
    }
}

pub fn discover(hid: &Hid) -> Result<Vec<Box<dyn super::Device>>, String> {
    let mut out: Vec<Box<dyn super::Device>> = Vec::new();
    for iface in hid
        .list(VID)
        .iter()
        .filter(|i| (0x7B00..=0x7EFF).contains(&i.pid) || i.pid == 0x1720)
    {
        // Kontroler Mystic Light ma stronę użycia 0x0001 (gdy system ją podaje).
        if iface.usage_page != 0 && iface.usage_page != 0x0001 {
            continue;
        }
        let Ok(dev) = hid.open(iface) else {
            continue;
        };
        let Some(original) = read_report(&dev) else {
            continue;
        };
        out.push(Box::new(Mystic {
            dev,
            pid: iface.pid,
            id: hid::id("mystic", iface, 0),
            original,
            changed: false,
        }));
        // Jeden kontroler na płytę.
        break;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn zone_rewrite() {
        let mut report = vec![0u8; 185];
        report[0] = super::REPORT;
        report[184] = 1;
        let p = super::modified(&report, 1, [10, 20, 30]);
        assert_eq!(p.len(), 185);
        assert_eq!(&p[1..5], &[1, 10, 20, 30]);
        assert_eq!(p[5], 10 << 2);
        assert_eq!(p[9], 0x80);
        assert_eq!(&p[53..57], &[1, 10, 20, 30]);
        assert_eq!(p[57], 0); // strefa Corsair: bez jasności
        assert_eq!(&p[174..178], &[1, 10, 20, 30]);
        assert_eq!(p[184], 0);
        assert_eq!(p[0], super::REPORT);
    }
}
