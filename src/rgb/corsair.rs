//! Corsair Lighting Node Core / Pro, Commander Pro i LS100 (1B1C:0C1A,
//! 0C0B, 0C10, 0C1E).
//!
//! Zapis: raport 0 + 64 bajty, odpowiedź 16 bajtów. Polecenia (bajt 1):
//! 0x37 reset kanału, 0x34 początek efektu, 0x38 stan kanału (1 = efekt
//! sprzętowy), 0x35 efekt [kanał, pierwsza dioda, liczba diod, tryb,
//! szybkość, kierunek, losowo, 0, kolor 1 RGB, kolor 2, kolor 3, …],
//! 0x39 jasność kanału (0-100), 0x33 0xFF zatwierdzenie.
//!
//! Tryb 4 = stały kolor; wyłączenie = stały czarny. Liczba diod na kanale
//! nie jest znana z urządzenia - ustawiamy 96 (więcej niż typowe zestawy).
//! Nietestowane na sprzęcie.

use super::hid::{self, Hid, HidDevice};

const VID: u16 = 0x1B1C;
/// (PID, liczba kanałów, nazwa)
const MODELS: [(u16, u8, &str); 4] = [
    (0x0C1A, 1, "Corsair Lighting Node Core"),
    (0x0C0B, 2, "Corsair Lighting Node Pro"),
    (0x0C10, 2, "Corsair Commander Pro"),
    (0x0C1E, 1, "Corsair LS100"),
];
const LEN: usize = 65;
const LEDS: u8 = 96;
const MODE_STATIC: u8 = 4;

struct Node {
    dev: HidDevice,
    channels: u8,
    id: String,
    name: String,
}

impl Node {
    fn cmd(&self, data: &[u8]) -> Result<(), String> {
        let mut b = [0u8; LEN];
        b[1..1 + data.len()].copy_from_slice(data);
        self.dev.write(&b).map_err(|e| hid::err(&self.name, e))?;
        let mut r = [0u8; 17];
        let _ = self.dev.read_timeout(&mut r, 50);
        Ok(())
    }
}

impl super::Device for Node {
    fn info(&self) -> super::Info {
        super::Info {
            id: self.id.clone(),
            name: self.name.clone(),
            family: super::Family::Corsair,
            detail: crate::trf!("kanały: {}", "channels: {}", self.channels),
            restores: false,
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        let [r, g, b] = s.rgb();
        for ch in 0..self.channels {
            self.cmd(&[0x37, ch])?;
            self.cmd(&[0x34, ch])?;
            self.cmd(&[0x38, ch, 0x01])?;
            self.cmd(&[0x39, ch, 100])?;
            self.cmd(&[
                0x35,
                ch,
                0,
                LEDS,
                MODE_STATIC,
                0,
                0,
                0,
                0,
                r,
                g,
                b,
                r,
                g,
                b,
                r,
                g,
                b,
            ])?;
        }
        self.cmd(&[0x33, 0xFF])
    }

    fn release(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn needs_keepalive(&self) -> bool {
        // Kontroler wraca do domyślnego efektu, gdy długo nie dostaje poleceń.
        true
    }

    fn keepalive(&mut self, _: &super::Setting) -> Result<(), String> {
        self.cmd(&[0x33, 0xFF])
    }
}

pub fn discover(hid: &Hid) -> Result<Vec<Box<dyn super::Device>>, String> {
    let mut out: Vec<Box<dyn super::Device>> = Vec::new();
    let list = hid.list(VID);
    for (pid, channels, name) in MODELS {
        for (n, iface) in list.iter().filter(|i| i.pid == pid).enumerate() {
            let Ok(dev) = hid.open(iface) else {
                continue;
            };
            out.push(Box::new(Node {
                dev,
                channels,
                id: hid::id("corsair", iface, n),
                name: name.to_owned(),
            }));
        }
    }
    Ok(out)
}
