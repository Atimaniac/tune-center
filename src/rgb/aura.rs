//! ASUS Aura na USB: kontrolery płyt głównych (0B05:18F3, 1939, 19AF, 1AA6,
//! 1BED) i osobne kontrolery złączy ARGB (1867, 1872, 18A3, 18A5).
//!
//! Raporty mają 65 bajtów z numerem raportu 0xEC:
//! - 0xEC 0xB0 → tabela konfiguracji (odpowiedź 0xEC 0x30, tabela od bajtu 4;
//!   bajt 0x02 = liczba złączy ARGB, 0x1B = liczba diod płyty),
//! - 0xEC 0x82 → wersja oprogramowania (tekst od bajtu 2),
//! - płyta: 0xEC 0x35 [kanał efektu, 0, 0, tryb] i 0xEC 0x36 [maska diod
//!   (16 bitów, big-endian), 0, kolory R,G,B od bajtu 5 + 3 × pierwsza dioda],
//! - kontroler ARGB: 0xEC 0x3B [kanał, 0, tryb, R, G, B].
//!
//! Tryb 0 = wyłączone, 1 = stały kolor. Zatwierdzenia w pamięci (0x3F) nie
//! wysyłamy - po wyłączeniu zasilania wraca efekt ustawiony w urządzeniu.
//! Nietestowane na sprzęcie.

use super::hid::{self, Hid, HidDevice, Iface};

const VID: u16 = 0x0B05;
const MAINBOARD: [u16; 5] = [0x18F3, 0x1939, 0x19AF, 0x1AA6, 0x1BED];
const ADDRESSABLE: [u16; 4] = [0x1867, 0x1872, 0x18A3, 0x18A5];
const REPORT: u8 = 0xEC;
const LEN: usize = 65;
const MODE_OFF: u8 = 0;
const MODE_STATIC: u8 = 1;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Mainboard,
    Addressable,
}

struct Aura {
    dev: HidDevice,
    kind: Kind,
    id: String,
    name: String,
    version: String,
    /// Liczba diod płyty (sterowanych jako jeden kanał efektu).
    board_leds: u8,
    /// Liczba złączy ARGB.
    argb: u8,
}

fn packet(cmd: u8, data: &[u8]) -> [u8; LEN] {
    let mut b = [0u8; LEN];
    b[0] = REPORT;
    b[1] = cmd;
    b[2..2 + data.len()].copy_from_slice(data);
    b
}

fn query(dev: &HidDevice, cmd: u8, reply: u8) -> Option<[u8; LEN]> {
    dev.write(&packet(cmd, &[])).ok()?;
    let mut r = [0u8; LEN];
    for _ in 0..4 {
        let n = dev.read_timeout(&mut r, 300).ok()?;
        if n == 0 {
            return None;
        }
        if r[0] == REPORT && r[1] == reply {
            return Some(r);
        }
    }
    None
}

impl Aura {
    fn send(&self, b: &[u8; LEN]) -> Result<(), String> {
        self.dev
            .write(b)
            .map(|_| ())
            .map_err(|e| hid::err(&self.name, e))
    }
}

impl super::Device for Aura {
    fn info(&self) -> super::Info {
        super::Info {
            id: self.id.clone(),
            name: self.name.clone(),
            family: super::Family::AsusAura,
            detail: crate::trf!(
                "{}, diody płyty: {}, złącza ARGB: {}",
                "{}, board LEDs: {}, ARGB headers: {}",
                self.version,
                self.board_leds,
                self.argb
            ),
            restores: false,
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        let mode = if s.mode == super::Mode::Color {
            MODE_STATIC
        } else {
            MODE_OFF
        };
        let [r, g, b] = s.rgb();
        match self.kind {
            Kind::Addressable => {
                for ch in 0..self.argb {
                    self.send(&packet(0x3B, &[ch, 0, mode, r, g, b]))?;
                }
            }
            Kind::Mainboard => {
                // Kanały efektów: najpierw diody płyty, potem złącza ARGB
                // (w oprogramowaniu "AULA3-AR32" złącza ARGB mają kanały od 0).
                let argb_first = self.version.starts_with("AULA3-AR32");
                let mut channels: Vec<(u8, u8)> = Vec::new(); // (kanał efektu, liczba diod)
                if self.board_leds > 0 {
                    let ch = if argb_first { self.argb } else { 0 };
                    channels.push((ch, self.board_leds));
                }
                for i in 0..self.argb {
                    let ch = if argb_first {
                        i
                    } else {
                        i + u8::from(self.board_leds > 0)
                    };
                    channels.push((ch, 1));
                }
                let mut start = 0u8;
                for (ch, leds) in channels {
                    self.send(&packet(0x35, &[ch, 0, 0, mode]))?;
                    if start as u32 + leds as u32 <= 16 {
                        let mask: u16 = (((1u32 << leds) - 1) << start) as u16;
                        let mut data = vec![(mask >> 8) as u8, mask as u8, 0];
                        data.resize(3 + 3 * start as usize, 0);
                        for _ in 0..leds {
                            data.extend_from_slice(&[r, g, b]);
                        }
                        if data.len() <= LEN - 2 {
                            self.send(&packet(0x36, &data))?;
                        }
                    }
                    start = start.saturating_add(leds);
                }
            }
        }
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        // Efekt z pamięci urządzenia wraca po wyłączeniu zasilania.
        Ok(())
    }
}

pub fn discover(hid: &Hid) -> Result<Vec<Box<dyn super::Device>>, String> {
    let list = hid.list(VID);
    let mut out: Vec<Box<dyn super::Device>> = Vec::new();
    for pid in MAINBOARD.iter().chain(&ADDRESSABLE) {
        let ifaces: Vec<&Iface> = list.iter().filter(|i| i.pid == *pid).collect();
        // Interfejs Aura ma stronę użycia 0xFF72; gdy jej nie ma, bierzemy pierwszy.
        let Some(iface) = ifaces
            .iter()
            .find(|i| i.usage_page == 0xFF72)
            .or(ifaces.first())
        else {
            continue;
        };
        let Ok(dev) = hid.open(iface) else {
            continue;
        };
        let Some(table) = query(&dev, 0xB0, 0x30) else {
            continue;
        };
        let version = query(&dev, 0x82, 0x02)
            .map(|r| {
                String::from_utf8_lossy(&r[2..18])
                    .trim_matches(char::from(0))
                    .trim()
                    .to_owned()
            })
            .unwrap_or_default();
        let cfg = &table[4..];
        let kind = if MAINBOARD.contains(pid) {
            Kind::Mainboard
        } else {
            Kind::Addressable
        };
        let argb = cfg[0x02].min(8);
        let board_leds = if kind == Kind::Mainboard {
            cfg[0x1B].min(16)
        } else {
            0
        };
        let name = match kind {
            Kind::Mainboard => {
                crate::i18n::tr("ASUS Aura (płyta główna)", "ASUS Aura (motherboard)")
            }
            Kind::Addressable => crate::i18n::tr("ASUS Aura ARGB", "ASUS Aura ARGB"),
        };
        out.push(Box::new(Aura {
            dev,
            kind,
            id: hid::id("aura", iface, 0),
            name: name.to_owned(),
            version,
            board_leds,
            argb,
        }));
    }
    Ok(out)
}
