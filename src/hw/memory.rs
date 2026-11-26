//! Temperatury modułów RAM jak w LibreHardwareMonitor (`DimmMemory`, dane z
//! RAMSPDToolkit): czujnik JEDEC TSOD modułów DDR4 (adresy 0x18..0x1F) i
//! czujnik huba SPD5118 modułów DDR5 (adresy 0x50..0x57).
//!
//! Magistrala SMBus przez moduł PawnIO `SmbusI801.bin` (Intel) albo
//! `SmbusPIIX4.bin` (AMD), zawsze pod blokadą "Global\Access_SMBUS.HTP.Method".
//! Okresowo wykonujemy wyłącznie odczyty. Jedyne zapisy to wybór strony SPD
//! przy otwarciu (raz, żeby odczytać producenta i numer części - jak LHM):
//! DDR4 - polecenie SPA (adres 0x36/0x37, bez danych w pamięci), DDR5 - rejestr
//! strony MR11 huba (pominięte, gdy chipset blokuje zapis SPD). Po odczycie
//! strona wraca do poprzedniej.

use std::path::Path;
use std::time::Duration;

use crate::cpu::pawnio::{NamedLock, PawnIo};

use super::{valid_temp, Readings};

const KIND: &str = "Memory";
const SMBUS_LOCK: &str = r"Global\Access_SMBUS.HTP.Method";
const BUS_MODULES: [&str; 2] = ["SmbusI801.bin", "SmbusPIIX4.bin"];

// Protokoły SMBus modułów PawnIO (jak w Linuksie).
const READ: u64 = 1;
const WRITE: u64 = 0;
const PROTO_BYTE: u64 = 1;
const PROTO_BYTE_DATA: u64 = 2;
const PROTO_WORD_DATA: u64 = 3;

const SPD_BEGIN: u8 = 0x50;
const SPD_END: u8 = 0x57;
/// DDR4: adres wyboru strony 0 (strona 1 = 0x37).
const DDR4_PAGE_ADDR: u8 = 0x36;
/// DDR5: rejestr strony pamięci huba (MR11).
const DDR5_MR_PAGE: u8 = 0x0B;

/// Magistrala SMBus przez PawnIO.
struct Bus {
    io: PawnIo,
}

impl Bus {
    fn open(dir: &Path) -> Option<Self> {
        BUS_MODULES.iter().find_map(|m| PawnIo::open(&dir.join(m)).ok().map(|io| Self { io }))
    }

    fn xfer(&self, addr: u8, rw: u64, cmd: u8, proto: u64, data: Option<u64>) -> Option<u64> {
        let mut input = vec![addr as u64, rw, cmd as u64, proto];
        input.extend(data);
        let mut out = [0u64; 1];
        self.io.call(b"ioctl_smbus_xfer\0", &input, &mut out).ok()?;
        Some(out[0])
    }

    /// Odczyt z kilkoma próbami (magistrala bywa chwilowo zajęta).
    fn retry(&self, tries: usize, f: impl Fn() -> Option<u64>) -> Option<u64> {
        for i in 0..tries.max(1) {
            if let Some(v) = f() {
                return Some(v);
            }
            if i + 1 < tries {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        None
    }

    fn read_byte(&self, addr: u8) -> Option<u8> {
        self.xfer(addr, READ, 0, PROTO_BYTE, None).map(|v| v as u8)
    }

    fn byte(&self, addr: u8, cmd: u8, tries: usize) -> Option<u8> {
        self.retry(tries, || self.xfer(addr, READ, cmd, PROTO_BYTE_DATA, None)).map(|v| v as u8)
    }

    /// Słowo (pierwszy bajt z magistrali = młodszy).
    fn word(&self, addr: u8, cmd: u8, tries: usize) -> Option<u16> {
        self.retry(tries, || self.xfer(addr, READ, cmd, PROTO_WORD_DATA, None)).map(|v| v as u16)
    }

    /// Wybór strony SPD (DDR4: adres SPA, DDR5: rejestr MR11 huba).
    fn select_page(&self, addr: u8, cmd: u8, value: u8) -> bool {
        debug_assert!(addr == DDR4_PAGE_ADDR || addr == DDR4_PAGE_ADDR + 1 || cmd == DDR5_MR_PAGE);
        let ok = self.xfer(addr, WRITE, cmd, PROTO_BYTE_DATA, Some(value as u64)).is_some();
        std::thread::sleep(Duration::from_millis(1));
        ok
    }

    /// Czy chipset Intela blokuje zapis pod adresy SPD (bit SPD_WD).
    fn spd_write_protected(&self) -> bool {
        let mut out = [0u64; 1];
        self.io.call(b"ioctl_write_protection\0", &[], &mut out).map_or(false, |_| out[0] != 0)
    }
}

/// Temperatura w formacie czujników JEDEC (bit 12 = znak, 0,0625 °C), jak
/// `SPDTemperatureConverter` w RAMSPDToolkit.
fn convert(raw: u16) -> f32 {
    if raw & 0x1000 != 0 {
        (raw & !0x1000) as f32 * 0.0625 - 256.0
    } else {
        raw as f32 * 0.0625
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Ddr4,
    Ddr5,
}

/// Jeden moduł z czujnikiem temperatury.
struct Dimm {
    kind: Kind,
    /// Numer gniazda (adres SPD - 0x50).
    index: u8,
    hw: String,
    /// Stałe czujniki LHM (rozdzielczość, progi) odczytane przy otwarciu.
    fixed: Vec<(&'static str, Option<f32>)>,
    /// DDR5: czujnik wyłączony (MR26) - bez odczytu, jak w LHM.
    enabled: bool,
    last: Option<f32>,
}

impl Dimm {
    fn addr(&self) -> u8 {
        SPD_BEGIN + self.index
    }

    fn sensor_addr(&self) -> u8 {
        match self.kind {
            Kind::Ddr4 => 0x18 | (self.index & 0x07),
            Kind::Ddr5 => self.addr(),
        }
    }

    /// Bieżąca temperatura (pod blokadą SMBus).
    fn update(&mut self, bus: &Bus) {
        if !self.enabled {
            return;
        }
        let t = match self.kind {
            // TSOD: rejestr 5, bajty w kolejności MSB, LSB; bity 11:0 + znak.
            Kind::Ddr4 => bus.word(self.sensor_addr(), 0x05, 3).map(|w| convert(w.swap_bytes() & 0xFFF)),
            // SPD5118: MR49 (młodszy) i MR50 (starszy).
            Kind::Ddr5 => bus.word(self.addr(), 0x31, 3).map(convert),
        };
        if let Some(t) = t.and_then(valid_temp) {
            self.last = Some(t);
        }
    }
}

/// Producent modułu wg kodu JEDEC JEP106 (bank = kody kontynuacji + 1).
/// Wybór producentów pamięci z tabeli RAMSPDToolkit (nazwy jak w LHM).
fn manufacturer(continuation: u8, id: u8) -> Option<&'static str> {
    const TABLE: &[(u8, u8, &str)] = &[
        (1, 0x2C, "Micron Technology"),
        (1, 0xAD, "SK Hynix"),
        (1, 0xC1, "Infineon (Siemens)"),
        (1, 0xC8, "Apple Computer"),
        (1, 0xCE, "Samsung"),
        (1, 0x54, "Hewlett-Packard"),
        (1, 0xDA, "Winbond Electronic"),
        (2, 0x94, "Smart Modular"),
        (2, 0x98, "Kingston"),
        (2, 0x32, "Mushkin Enhanced Memory"),
        (2, 0xBA, "PNY Technologies Inc"),
        (2, 0x4F, "Transcend Information"),
        (2, 0x7A, "Apacer Technology"),
        (3, 0x9E, "Corsair"),
        (3, 0xFE, "Elpida"),
        (4, 0x0B, "Nanya Technology"),
        (4, 0x13, "Golden Empire"),
        (4, 0x94, "Mushkin"),
        (4, 0x25, "Kingmax Semiconductor"),
        (5, 0x0D, "Patriot Scientific"),
        (5, 0xB0, "OCZ"),
        (5, 0x43, "Ramaxel Technology"),
        (5, 0xCB, "A-DATA Technology"),
        (5, 0xCD, "G Skill Intl"),
        (5, 0xEF, "Team Group Inc"),
        (5, 0xF1, "Toshiba Corporation"),
        (6, 0x02, "Patriot Memory (PDP Systems)"),
        (6, 0x9B, "Crucial Technology"),
        (6, 0x51, "Qimonda"),
        (7, 0x16, "Avexir Technologies Corporation"),
        (7, 0x34, "Super Talent"),
        (7, 0xD3, "Silicon Power Computer & Communications"),
        (7, 0x5E, "Lexar Media"),
        (7, 0x6D, "V-Color Technology Inc"),
        (8, 0x1C, "Ramsta"),
        (8, 0x46, "Gloway International (HK)"),
        (8, 0x5D, "Wilk Elektronik S.A."),
        (8, 0xEA, "Transcend Technology Co Ltd"),
        (8, 0x70, "Panram International Corporation"),
        (9, 0x13, "Gloway International Co Ltd"),
        (9, 0x98, "Essencore Limited"),
        (9, 0xC8, "Lenovo"),
        (9, 0xF1, "Asgard"),
        (9, 0x75, "JUHOR"),
        (10, 0xA1, "Teclast"),
        (10, 0xAD, "Neo Forza"),
        (10, 0xCD, "Shenzhen Longsys Electronics Co Ltd"),
        (10, 0x68, "Kimtigo Semiconductor (HK) Limited"),
        (10, 0xEC, "Colorful Technology Ltd"),
        (10, 0xF2, "GIGA-BYTE Technology Co Ltd"),
        (10, 0xF7, "Netac Technology Co Ltd"),
        (11, 0x02, "KingSpec"),
        (11, 0x91, "CXMT"),
        (11, 0x31, "Biwin Semiconductor (HK) Co Ltd"),
        (11, 0xC2, "Thermaltake Technology Co Ltd"),
        (11, 0xD6, "Shenzhen Longsys Electronics Co Ltd"),
        (11, 0x6B, "Acer"),
        (11, 0x76, "Lexar Co Limited"),
        (12, 0x92, "Kingbank Technology Co Ltd"),
        (12, 0xDC, "ASUS"),
        (13, 0xAB, "Biwin Storage Technology Co Ltd"),
        (13, 0x5D, "OLOy Technology"),
        (13, 0xFD, "Dell Technologies"),
        (14, 0xEC, "CFD Sales Inc"),
        (17, 0x8F, "Essencore"),
    ];
    let bank = (continuation & 0x7F).checked_add(1)?;
    TABLE.iter().find(|(b, i, _)| *b == bank && *i == id).map(|(_, _, n)| *n)
}

/// Numer części: bez spacji i zer na końcach (jak RAMSPDToolkit).
fn part_number(bytes: &[u8]) -> String {
    let s: String = bytes.iter().filter(|&&b| b != 0x20).map(|&b| b as char).collect();
    s.trim_matches('\0').to_owned()
}

/// Nazwa jak w LHM: "<producent> - <numer części> (#<gniazdo>)".
fn dimm_name(index: u8, mfr: Option<&str>, part: &str) -> String {
    format!("{} - {} (#{index})", mfr.unwrap_or(""), part)
}

/// DDR4: bieżąca strona SPD (ACK pod 0x36 = strona 0).
fn ddr4_page0(bus: &Bus, pages: bool) -> bool {
    bus.read_byte(DDR4_PAGE_ADDR).is_some() || (pages && bus.select_page(DDR4_PAGE_ADDR, 0, 0xFF))
}

/// Hub DDR5 (MR0 = 0x51, MR1 = 0x18).
fn is_ddr5_hub(bus: &Bus, addr: u8) -> bool {
    bus.byte(addr, 0x00, 5) == Some(0x51) && bus.byte(addr, 0x01, 5) == Some(0x18)
}

fn probe_ddr4(bus: &Bus, addr: u8, pages: bool) -> Option<Dimm> {
    let index = addr - SPD_BEGIN;
    // Typ pamięci (bajt 2): DDR4, DDR4E, LPDDR4, LPDDR4X.
    if !matches!(bus.byte(addr, 0x02, 5)?, 0x0C | 0x0E | 0x10 | 0x11) {
        return None;
    }
    let ts = 0x18 | (index & 0x07);
    // Czujnik: bajt 14 bit 7, albo odpowiedź rejestru możliwości czujnika.
    let has_sensor = bus.byte(addr, 0x0E, 5)? & 0x80 != 0 || bus.word(ts, 0x00, 1).is_some();
    if !has_sensor {
        return None;
    }
    let reg = |r: u8| bus.word(ts, r, 50).map(u16::swap_bytes);
    let resolution = reg(0x00).map(|c| 0.5 / (1u32 << ((c & 0x18) >> 3)) as f32);
    let limit = |r: u8| reg(r).map(|w| convert(w & 0xFFF));
    let fixed = vec![
        ("Temperature Sensor Resolution", resolution),
        ("Thermal Sensor Low Limit", limit(0x03)),
        ("Thermal Sensor High Limit", limit(0x02)),
        ("Thermal Sensor Critical Limit", limit(0x04)),
    ];

    // Producent i numer części: strona 1 (bajty 0x140.. = 0x40.. na stronie).
    let mut hw = format!("DIMM #{index}");
    if pages && bus.select_page(DDR4_PAGE_ADDR + 1, 0, 0xFF) {
        let cont = bus.byte(addr, 0x40, 5);
        let id = bus.byte(addr, 0x41, 5);
        let part: Option<Vec<u8>> = (0x49u8..0x5C).map(|o| bus.byte(addr, o, 5)).collect();
        if let (Some(cont), Some(id), Some(part)) = (cont, id, part) {
            hw = dimm_name(index, manufacturer(cont, id), &part_number(&part));
        }
        bus.select_page(DDR4_PAGE_ADDR, 0, 0xFF);
    }
    Some(Dimm { kind: Kind::Ddr4, index, hw, fixed, enabled: true, last: None })
}

fn probe_ddr5(bus: &Bus, addr: u8, pages: bool) -> Option<Dimm> {
    let index = addr - SPD_BEGIN;
    // MR5 bit 1 = hub ma czujnik temperatury.
    if bus.byte(addr, 0x05, 50)? & 0x02 == 0 {
        return None;
    }
    let limit = |r: u8| bus.word(addr, r, 50).map(convert);
    let fixed = vec![
        ("Temperature Sensor Resolution", Some(0.25)),
        ("Thermal Sensor Low Limit", limit(0x1E)),
        ("Thermal Sensor High Limit", limit(0x1C)),
        ("Thermal Sensor Critical Low Limit", limit(0x22)),
        ("Thermal Sensor Critical High Limit", limit(0x20)),
    ];
    // MR26 = 0: czujnik włączony.
    let enabled = bus.byte(addr, 0x1A, 50) == Some(0);

    // Producent i numer części: strona 4 (bajty 0x200.. = 0x80.. w oknie).
    let mut hw = format!("DIMM #{index}");
    if pages {
        if let Some(page) = bus.byte(addr, DDR5_MR_PAGE, 5) {
            if bus.select_page(addr, DDR5_MR_PAGE, 4) {
                let cont = bus.byte(addr, 0x80, 5);
                let id = bus.byte(addr, 0x81, 5);
                let part: Option<Vec<u8>> = (0x89u8..=0xA6).map(|o| bus.byte(addr, o, 5)).collect();
                if let (Some(cont), Some(id), Some(part)) = (cont, id, part) {
                    hw = dimm_name(index, manufacturer(cont, id), &part_number(&part));
                }
                bus.select_page(addr, DDR5_MR_PAGE, page & 0x07);
            }
        }
    }
    Some(Dimm { kind: Kind::Ddr5, index, hw, fixed, enabled, last: None })
}

/// Temperatury modułów pamięci RAM.
pub struct Memory {
    bus: Bus,
    dimms: Vec<Dimm>,
}

impl Memory {
    pub fn open() -> Option<Self> {
        let dir = super::module_path("SmbusI801.bin")?.parent()?.to_path_buf();
        // Bez zmiany strony SPD (zapisu na SMBus): producent i numer części
        // modułów z tablicy SMBIOS Windows.
        let mut memory = Self::open_in(&dir, false)?;
        let parts = smbios_modules();
        for (d, (mfr, part)) in memory.dimms.iter_mut().zip(parts) {
            d.hw = dimm_name(d.index, Some(&mfr), &part_number(part.as_bytes()));
        }
        Some(memory)
    }

    /// `pages`: wolno wybrać stronę SPD (nazwa modułu); bez tego same odczyty.
    fn open_in(dir: &Path, pages: bool) -> Option<Self> {
        let bus = Bus::open(dir)?;
        let mut dimms = Vec::new();
        // LHM ponawia wykrywanie kilka razy, gdy magistrala jest zajęta.
        for _ in 0..3 {
            if let Ok(_lock) = NamedLock::acquire(SMBUS_LOCK, 2000) {
                let protected = bus.spd_write_protected();
                for addr in SPD_BEGIN..=SPD_END {
                    // Pusty slot nie odpowiada.
                    if bus.byte(addr, 0x00, 2).is_none() {
                        continue;
                    }
                    let dimm = if is_ddr5_hub(&bus, addr) {
                        probe_ddr5(&bus, addr, pages && !protected)
                    } else if ddr4_page0(&bus, pages) {
                        probe_ddr4(&bus, addr, pages)
                    } else {
                        None
                    };
                    dimms.extend(dimm);
                }
                for d in &mut dimms {
                    d.update(&bus);
                }
            }
            if !dimms.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        (!dimms.is_empty()).then_some(Self { bus, dimms })
    }

    pub fn read(&mut self, out: &mut Readings) {
        // Zajęta magistrala: zostają poprzednie wartości.
        if let Ok(_lock) = NamedLock::acquire(SMBUS_LOCK, 200) {
            for d in &mut self.dimms {
                d.update(&self.bus);
            }
        }
        for d in &self.dimms {
            let id = |i: usize| format!("/memory/dimm/{}/temperature/{i}", d.index);
            out.temp(id(0), &format!("DIMM #{}", d.index), &d.hw, KIND, d.last);
            for (i, (name, v)) in d.fixed.iter().enumerate() {
                out.temp(id(i + 1), name, &d.hw, KIND, *v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion() {
        assert_eq!(convert(0x0228), 34.5);
        assert_eq!(convert(0x1FF0), -1.0);
        assert_eq!(manufacturer(0x04, 0xCD), Some("G Skill Intl"));
        assert_eq!(manufacturer(0x01, 0x98), Some("Kingston"));
        assert_eq!(part_number(b"F4-4000C18-32GTZR  \0"), "F4-4000C18-32GTZR");
        assert_eq!(dimm_name(2, Some("G Skill Intl"), "F4-4000C18-32GTZR"), "G Skill Intl - F4-4000C18-32GTZR (#2)");
    }
}

#[cfg(test)]
mod hardware {
    //! Tylko odczyt (bez wyboru strony SPD, więc nazwa "DIMM #n"), administrator i PawnIO:
    //! `cargo test --release hw_memory -- --ignored --nocapture`
    use super::*;

    #[test]
    #[ignore]
    fn hw_memory() {
        let mut m = Memory::open_in(Path::new("D:/tune-center-main/dist/modules"), false).expect("Memory::open");
        for _ in 0..2 {
            let mut r = Readings::default();
            m.read(&mut r);
            for t in &r.temps {
                println!("{} | {} | {} | {} | {:?}", t.id, t.name, t.hw, t.kind, t.value);
            }
            println!("---");
            std::thread::sleep(Duration::from_millis(1000));
        }
    }
}

/// Producent z SMBIOS: nazwa albo kod JEDEC zapisany szesnastkowo ("04CD" =
/// 4 kody kontynuacji i identyfikator 0xCD) - wtedy nazwa z tabeli JEDEC.
fn smbios_manufacturer(text: &str) -> String {
    let t = text.trim();
    if t.len() == 4 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        if let (Ok(cont), Ok(id)) = (u8::from_str_radix(&t[..2], 16), u8::from_str_radix(&t[2..], 16)) {
            if let Some(name) = manufacturer(cont, id) {
                return name.to_owned();
            }
        }
    }
    t.to_owned()
}

/// Zainstalowane moduły pamięci z tablicy SMBIOS (struktury typu 17 z
/// niezerowym rozmiarem, w kolejności tablicy): (producent, numer części).
pub fn smbios_modules() -> Vec<(String, String)> {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};
        // SAFETY: najpierw zapytanie o rozmiar, potem bufor tego rozmiaru.
        let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) } as usize;
        if size < 8 {
            return Vec::new();
        }
        let mut buf = vec![0u8; size];
        let got = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut buf)) } as usize;
        parse_smbios_modules(&buf[..got.min(size)])
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// `RawSMBIOSData`: 8 bajtów nagłówka, potem struktury SMBIOS.
fn parse_smbios_modules(raw: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(len) = raw.get(4..8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) else {
        return out;
    };
    let table = &raw[8..(8 + len).min(raw.len())];
    let mut pos = 0;
    while pos + 4 <= table.len() {
        let (kind, flen) = (table[pos], table[pos + 1] as usize);
        if flen < 4 || pos + flen > table.len() {
            break;
        }
        let formatted = &table[pos..pos + flen];
        // Teksty: po części sformatowanej, zakończone podwójnym zerem.
        let mut end = pos + flen;
        while end + 1 < table.len() && !(table[end] == 0 && table[end + 1] == 0) {
            end += 1;
        }
        let strings: Vec<&[u8]> = table[pos + flen..end].split(|&b| b == 0).collect();
        let text = |i: Option<&u8>| -> String {
            match i.copied() {
                Some(n) if n > 0 => strings.get(n as usize - 1).map(|s| String::from_utf8_lossy(s).trim().to_owned()).unwrap_or_default(),
                _ => String::new(),
            }
        };
        if kind == 127 {
            break;
        }
        if kind == 17 && flen >= 0x1B {
            let size = u16::from_le_bytes([formatted[0x0C], formatted[0x0D]]);
            if size != 0 && size != 0xFFFF {
                out.push((smbios_manufacturer(&text(formatted.get(0x17))), text(formatted.get(0x1A))));
            }
        }
        pos = end + 2;
    }
    out
}

#[cfg(test)]
mod smbios_tests {
    use super::*;

    #[test]
    fn reads_memory_devices_from_smbios() {
        // Nagłówek RawSMBIOSData, jedna struktura typu 17 (rozmiar 16 GB), koniec (127).
        let mut t17 = vec![0u8; 0x22];
        t17[0] = 17;
        t17[1] = 0x22;
        t17[0x0C] = 0x00;
        t17[0x0D] = 0x40;
        t17[0x17] = 1; // producent
        t17[0x1A] = 2; // numer części
        let mut table = t17;
        table.extend_from_slice(b"04CD\0F4-4000C18-32GTZR  \0\0");
        let mut empty = vec![0u8; 0x22];
        empty[0] = 17;
        empty[1] = 0x22;
        table.extend_from_slice(&empty);
        table.extend_from_slice(&[0, 0]);
        table.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        let mut raw = vec![0, 3, 4, 0];
        raw.extend_from_slice(&(table.len() as u32).to_le_bytes());
        raw.extend_from_slice(&table);
        let mods = parse_smbios_modules(&raw);
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].0, "G Skill Intl");
        assert_eq!(part_number(mods[0].1.as_bytes()), "F4-4000C18-32GTZR");
    }

    /// Moduły z tego komputera: `cargo test smbios_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn smbios_live() {
        println!("{:?}", smbios_modules());
    }
}
