//! Układy Super I/O Nuvoton: NCT6771F/6776F, NCT6779D, NCT679xD, NCT6701D,
//! NCT5585D, NCT610xD (rejestry w bankach) i NCT6683D/6686D/6687D(-R)
//! (przestrzeń EC ze stronami - firmware producenta płyty).
//!
//! Wykrywanie, rejestry, odczyty i nazwy czujników jak w LibreHardwareMonitor
//! (`Nct677X.cs`, `LpcIO.cs`, `SuperIOHardware.cs` - konfiguracje domyślne;
//! listy dla konkretnych płyt w `boards.rs`).
//!
//! Zapisy do rejestrów monitora sprzętu tylko w `set_control`/`restore_all`.
//! Wyjątek przy rozpoznaniu (jak w LHM): zdjęcie blokady przestrzeni I/O
//! monitora (bit 4 rejestru konfiguracji 0x28) w NCT679xD/6701D/5585D - bez
//! tego odczyty dają śmieci.

use std::thread::sleep;
use std::time::{Duration, Instant};

use super::boards::{self, Counts};
use super::superio::{Board, Chip, Lpc};
use super::{valid_temp, Readings};
use crate::bridge::{Fan, Speed};
use crate::i18n::tr;

// ------------------------------------------------------------------ dostęp do portów

/// Porty I/O - `Lpc` albo symulacja rejestrów w testach.
trait Bus {
    fn inb(&self, port: u16) -> Result<u8, String>;
    fn outb(&self, port: u16, value: u8) -> Result<(), String>;
}

/// Port konfiguracji układu (tryb konfiguracji, logiczne urządzenia).
trait ConfigBus: Bus {
    fn config_port(&self) -> u16;
    fn sio_inb(&self, reg: u8) -> Result<u8, String>;
    fn sio_outb(&self, reg: u8, value: u8) -> Result<(), String>;
    fn find_bars(&self) -> Result<(), String>;

    fn sio_inw(&self, reg: u8) -> Result<u16, String> {
        Ok((self.sio_inb(reg)? as u16) << 8 | self.sio_inb(reg.wrapping_add(1))? as u16)
    }
}

impl Bus for Lpc {
    fn inb(&self, port: u16) -> Result<u8, String> {
        Lpc::inb(self, port)
    }
    fn outb(&self, port: u16, value: u8) -> Result<(), String> {
        Lpc::outb(self, port, value)
    }
}

impl ConfigBus for Lpc {
    fn config_port(&self) -> u16 {
        self.port
    }
    fn sio_inb(&self, reg: u8) -> Result<u8, String> {
        Lpc::sio_inb(self, reg)
    }
    fn sio_outb(&self, reg: u8, value: u8) -> Result<(), String> {
        Lpc::sio_outb(self, reg, value)
    }
    fn find_bars(&self) -> Result<(), String> {
        Lpc::find_bars(self)
    }
}

// ------------------------------------------------------------------ rejestry konfiguracji

const CHIP_ID: u8 = 0x20;
const CHIP_REVISION: u8 = 0x21;
const DEVICE_SELECT: u8 = 0x07;
const BASE_ADDRESS: u8 = 0x60;
const ALTERNATE_BASE_ADDRESS: u8 = 0x64;
/// Logiczne urządzenie monitora sprzętu.
const HWM_LDN: u8 = 0x0B;
/// Blokada przestrzeni I/O monitora (bit 4) w NCT679xD.
const IO_SPACE_LOCK: u8 = 0x28;

// Monitor w bankach: rejestr adresu i danych względem bazy, wybór banku.
const ADDRESS_OFFSET: u16 = 0x05;
const DATA_OFFSET: u16 = 0x06;
const BANK_SELECT: u8 = 0x4E;
// Przestrzeń EC (NCT668x): strona, indeks, dane; 0xFF = strona wolna.
const EC_PAGE_OFFSET: u16 = 0x04;
const EC_INDEX_OFFSET: u16 = 0x05;
const EC_DATA_OFFSET: u16 = 0x06;
const EC_PAGE_FREE: u8 = 0xFF;

const NUVOTON_VENDOR_ID: u16 = 0x5CA3;

// NCT668x: żądanie/zakończenie zmiany konfiguracji wentylatorów (0xA01).
const FAN_CFG_REQUEST: u8 = 0x80;
const FAN_CFG_COMPLETE: u8 = 0x40;
// NCT6687D-R: stan silnika wentylatorów i jego bity (jak sterownik nct6687d Linuksa).
const DR_ENGINE_STATUS: u16 = 0xCF8;
const DR_CFG_LOCK: u8 = 1 << 6;
const DR_CFG_PHASE: u8 = 1 << 3;
const DR_CFG_INVALID: u8 = 1 << 4;
const DR_CFG_CHECK_DONE: u8 = 1 << 5;
/// Brak rejestru (NCT6687D-R, nieużywane kanały).
const NO_REG: u16 = 0xFFF;

// ------------------------------------------------------------------ typy układów

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Nct610x,
    Nct6771F,
    Nct6776F,
    Nct6779D,
    Nct6791D,
    Nct6792D,
    Nct6792DA,
    Nct6793D,
    Nct6795D,
    Nct6796D,
    Nct6796DR,
    Nct6796DS,
    Nct6797D,
    Nct6798D,
    Nct6799D,
    Nct6701D,
    Nct5585D,
    Nct6683D,
    Nct6686D,
    Nct6687D,
    Nct6687DR,
}

use Kind::*;

impl Kind {
    /// Nazwa jak `ChipName` w LHM.
    fn name(self) -> &'static str {
        match self {
            Nct610x => "Nuvoton NCT6102D/NCT6104D/NCT6106D",
            Nct6771F => "Nuvoton NCT6771F",
            Nct6776F => "Nuvoton NCT6776F",
            Nct6779D => "Nuvoton NCT6779D",
            Nct6791D => "Nuvoton NCT6791D",
            Nct6792D => "Nuvoton NCT6792D",
            Nct6792DA => "Nuvoton NCT6792D-A",
            Nct6793D => "Nuvoton NCT6793D",
            Nct6795D => "Nuvoton NCT6795D",
            Nct6796D => "Nuvoton NCT6796D",
            Nct6796DR => "Nuvoton NCT6796D-R",
            Nct6796DS => "Nuvoton NCT6796D-S",
            Nct6797D => "Nuvoton NCT6797D",
            Nct6798D => "Nuvoton NCT6798D",
            Nct6799D => "Nuvoton NCT6799D",
            Nct6701D => "Nuvoton NCT6701D",
            Nct5585D => "Nuvoton NCT5585D",
            Nct6683D => "Nuvoton NCT6683D",
            Nct6686D => "Nuvoton NCT6686D",
            Nct6687D => "Nuvoton NCT6687D",
            Nct6687DR => "Nuvoton NCT6687D-R",
        }
    }

    /// Nazwa wartości wyliczenia `Chip` w LHM małymi literami (część identyfikatora).
    fn slug(self) -> &'static str {
        match self {
            Nct610x => "nct610xd",
            Nct6771F => "nct6771f",
            Nct6776F => "nct6776f",
            Nct6779D => "nct6779d",
            Nct6791D => "nct6791d",
            Nct6792D => "nct6792d",
            Nct6792DA => "nct6792da",
            Nct6793D => "nct6793d",
            Nct6795D => "nct6795d",
            Nct6796D => "nct6796d",
            Nct6796DR => "nct6796dr",
            Nct6796DS => "nct6796ds",
            Nct6797D => "nct6797d",
            Nct6798D => "nct6798d",
            Nct6799D => "nct6799d",
            Nct6701D => "nct6701d",
            Nct5585D => "nct5585d",
            Nct6683D => "nct6683d",
            Nct6686D => "nct6686d",
            Nct6687D => "nct6687d",
            Nct6687DR => "nct6687dr",
        }
    }

    /// Przestrzeń EC ze stronami zamiast banków.
    fn ec(self) -> bool {
        matches!(self, Nct6683D | Nct6686D | Nct6687D | Nct6687DR)
    }

    /// Układy z blokadą przestrzeni I/O monitora.
    fn has_io_lock(self) -> bool {
        matches!(
            self,
            Nct6791D
                | Nct6792D
                | Nct6792DA
                | Nct6793D
                | Nct6795D
                | Nct6796D
                | Nct6796DR
                | Nct6796DS
                | Nct6797D
                | Nct6798D
                | Nct6799D
                | Nct6701D
                | Nct5585D
        )
    }
}

/// Płyty, od których zależy typ układu.
impl Board {
    fn msi(&self) -> bool {
        let m = self.manufacturer.trim();
        m.to_ascii_lowercase().starts_with("micro-star international") || m.eq_ignore_ascii_case("msi")
    }

    /// Płyty MSI serii 800 (AM5/LGA1851) z NCT6687D-R.
    fn msi_800(&self) -> bool {
        let p = self.product.to_ascii_uppercase();
        self.msi() && ["B840", "B850", "B860", "X870", "Z890"].iter().any(|s| p.contains(s))
    }

    fn x870e_nova(&self) -> bool {
        self.product.trim().eq_ignore_ascii_case("X870E Nova WiFi")
    }

    fn b650m_hdv(&self) -> bool {
        self.product.trim().to_ascii_uppercase().starts_with("B650M-HDV/M.2")
    }
}

/// Układ z ID i rewizji (rejestry 0x20/0x21); `board` czytana tylko w razie potrzeby.
fn identify(id: u8, rev: u8, board: &dyn Fn() -> Board) -> Option<Kind> {
    Some(match (id, rev) {
        (0xB4, r) if r & 0xF0 == 0x70 => Nct6771F,
        (0xC3, r) if r & 0xF0 == 0x30 => Nct6776F,
        (0xC4, r) if r & 0xF0 == 0x50 => Nct610x,
        (0xC5, r) if r & 0xF0 == 0x60 => Nct6779D,
        (0xC7, 0x32) => Nct6683D,
        (0xC8, 0x03) => Nct6791D,
        (0xC9, 0x11) => Nct6792D,
        (0xC9, 0x13) => Nct6792DA,
        (0xD1, 0x21) => Nct6793D,
        (0xD3, 0x52) => Nct6795D,
        (0xD4, 0x23) => Nct6796D,
        (0xD4, 0x2A) => {
            if board().x870e_nova() {
                Nct5585D
            } else {
                Nct6796DR
            }
        }
        (0xD4, 0x51) => Nct6797D,
        (0xD4, 0x2B) => Nct6798D,
        (0xD4, 0x40 | 0x41) => Nct6686D,
        (0xD5, 0x92) => {
            if board().msi_800() {
                Nct6687DR
            } else {
                Nct6687D
            }
        }
        (0xD8, 0x02) => {
            let b = board();
            if b.x870e_nova() || b.b650m_hdv() {
                Nct6796DS
            } else {
                Nct6799D
            }
        }
        (0xD8, 0x06) => Nct6701D,
        _ => return None,
    })
}

// ------------------------------------------------------------------ rozpoznanie

/// Rozpoznaje układ Nuvoton w wybranym gnieździe (pod blokadą ISA wołającego).
/// Nieznane ID - od razu `None`, żeby sprawdziły je kolejne sondy.
pub fn probe(lpc: &mut Lpc, slot: u8, index: usize) -> Option<Box<dyn Chip>> {
    let _ = slot;
    probe_on(lpc, &Board::from_registry, index).map(|c| Box::new(c) as Box<dyn Chip>)
}

fn probe_on(bus: &dyn ConfigBus, board: &dyn Fn() -> Board, sio_index: usize) -> Option<Nuvoton> {
    let port = bus.config_port();
    if port == 0 {
        return None;
    }
    let exit = || {
        let _ = bus.outb(port, 0xAA);
    };
    // Wejście w tryb konfiguracji (Winbond/Nuvoton/Fintek).
    if bus.outb(port, 0x87).and_then(|_| bus.outb(port, 0x87)).is_err() {
        exit();
        return None;
    }
    let (id, rev) = match (bus.sio_inb(CHIP_ID), bus.sio_inb(CHIP_REVISION)) {
        (Ok(id), Ok(rev)) => (id, rev),
        _ => {
            exit();
            return None;
        }
    };
    let Some(kind) = identify(id, rev, board) else {
        // 0x00/0xFF: nic nie odpowiedziało, układ nie jest w trybie konfiguracji.
        if id != 0 && id != 0xFF {
            exit();
        }
        return None;
    };

    // W trybie konfiguracji: zakresy portów, adres monitora, blokada I/O.
    let configured = (|| -> Result<(u16, u16), String> {
        bus.find_bars()?;
        bus.sio_outb(DEVICE_SELECT, HWM_LDN)?;
        let mut address = bus.sio_inw(BASE_ADDRESS)?;
        sleep(Duration::from_millis(1));
        let mut verify = bus.sio_inw(BASE_ADDRESS)?;
        // Rodzina D8 bywa z adresem w parze 0x64/0x65.
        if kind == Nct6701D && address == verify && invalid_base(address) {
            address = bus.sio_inw(ALTERNATE_BASE_ADDRESS)?;
            sleep(Duration::from_millis(1));
            verify = bus.sio_inw(ALTERNATE_BASE_ADDRESS)?;
        }
        if address == verify && kind.has_io_lock() {
            let options = bus.sio_inb(IO_SPACE_LOCK)?;
            if options & 0x10 != 0 {
                bus.sio_outb(IO_SPACE_LOCK, options & !0x10)?;
            }
        }
        Ok((address, verify))
    })();
    exit();

    let (mut address, verify) = configured.ok()?;
    if address != verify {
        return None;
    }
    if address & 0x07 == 0x05 {
        address &= 0xFFF8;
    }
    if invalid_base(address) {
        return None;
    }
    let mut chip = Nuvoton::new(kind, address, &board(), sio_index);
    chip.vendor_ok = chip.check_vendor(bus);
    Some(chip)
}

fn invalid_base(address: u16) -> bool {
    address < 0x100 || address & 0xF007 != 0
}

// ------------------------------------------------------------------ tablice czujników

/// Źródło temperatury: numer źródła, rejestr, rejestr i bit połówki stopnia,
/// rejestr wybranego źródła, rejestr zapasowy.
#[derive(Clone, Copy, Debug)]
struct TempSrc {
    source: Option<u8>,
    reg: u16,
    half_reg: u16,
    half_bit: i8,
    source_reg: u16,
    alt_reg: Option<u16>,
}

const fn tf(source: u8, reg: u16, half_reg: u16, half_bit: i8, source_reg: u16, alt: u16) -> TempSrc {
    TempSrc {
        source: Some(source),
        reg,
        half_reg,
        half_bit,
        source_reg,
        alt_reg: if alt == 0 { None } else { Some(alt) },
    }
}

/// Źródło bez połówki stopnia.
const fn ts(source: u8, reg: u16, source_reg: u16, alt: u16) -> TempSrc {
    tf(source, reg, 0, -1, source_reg, alt)
}

/// Rejestr bez numeru źródła.
const fn tn(reg: u16, half_reg: u16, half_bit: i8) -> TempSrc {
    TempSrc { source: None, reg, half_reg, half_bit, source_reg: 0, alt_reg: None }
}

// Numery źródeł NCT67xxD.
const SYSTIN: u8 = 1;
const CPUTIN: u8 = 2;
const AUXTIN0: u8 = 3;
const AUXTIN1: u8 = 4;
const AUXTIN2: u8 = 5;
const AUXTIN3: u8 = 6;
const AUXTIN4: u8 = 7;
const SMBUSMASTER0: u8 = 8;
const SMBUSMASTER1: u8 = 9;
const TSENSOR: u8 = 10;
const PECI_0: u8 = 16;
const PECI_1: u8 = 17;
const PCH_CHIP_CPU_MAX: u8 = 18;
const PCH_CHIP: u8 = 19;
const PCH_CPU: u8 = 20;
const PCH_MCH: u8 = 21;
const AGENT0_DIMM0: u8 = 22;
const AGENT0_DIMM1: u8 = 23;
const AGENT1_DIMM0: u8 = 24;
const AGENT1_DIMM1: u8 = 25;
const BYTE_TEMP0: u8 = 26;
const BYTE_TEMP1: u8 = 27;
const PECI_0_CAL: u8 = 28;
const PECI_1_CAL: u8 = 29;
const VIRTUAL_TEMP: u8 = 31;
const SPARE_TEMP: u8 = 32;
const SPARE_TEMP2: u8 = 33;
const AUXTIN5: u8 = 34;

/// Wspólny koniec tablic NCT679xD: PECI/PCH/DIMM i pozostałe.
macro_rules! nct67_tail {
    () => {
        [
            ts(PECI_1, 0x672, 0xC27, 0),
            ts(PCH_CHIP_CPU_MAX, 0x674, 0xC28, 0x400),
            ts(PCH_CHIP, 0x676, 0xC29, 0x401),
            ts(PCH_CPU, 0x678, 0xC2A, 0x402),
            ts(PCH_MCH, 0x67A, 0xC2B, 0x404),
            ts(AGENT0_DIMM0, 0x405, 0, 0),
            ts(AGENT0_DIMM1, 0x406, 0, 0),
            ts(AGENT1_DIMM0, 0x407, 0, 0),
            ts(AGENT1_DIMM1, 0x408, 0, 0),
        ]
    };
}

const fn concat<const A: usize, const B: usize, const N: usize>(a: [TempSrc; A], b: [TempSrc; B]) -> [TempSrc; N] {
    let mut out = [tn(0, 0, -1); N];
    let mut i = 0;
    while i < A {
        out[i] = a[i];
        i += 1;
    }
    while i < N {
        out[i] = b[i - A];
        i += 1;
    }
    out
}

const CORE_7: [TempSrc; 7] = [
    tf(PECI_0, 0x073, 0x074, 7, 0x100, 0),
    tf(CPUTIN, 0x075, 0x076, 7, 0x200, 0x491),
    tf(SYSTIN, 0x077, 0x078, 7, 0x300, 0x490),
    tf(AUXTIN0, 0x079, 0x07A, 7, 0x800, 0x492),
    tf(AUXTIN1, 0x07B, 0x07C, 7, 0x900, 0x493),
    tf(AUXTIN2, 0x07D, 0x07E, 7, 0xA00, 0x494),
    tf(AUXTIN3, 0x4A0, 0x49E, 6, 0xB00, 0x495),
];

const REST_7: [TempSrc; 7] = [
    ts(BYTE_TEMP0, 0x419, 0, 0),
    ts(BYTE_TEMP1, 0x41A, 0, 0),
    ts(PECI_0_CAL, 0x4F4, 0, 0),
    ts(PECI_1_CAL, 0x4F5, 0, 0),
    ts(VIRTUAL_TEMP, 0, 0, 0),
    ts(SPARE_TEMP, 0, 0, 0),
    ts(SPARE_TEMP2, 0, 0, 0),
];

/// NCT6791D/6792D/6793D/6795D.
const T_679X: [TempSrc; 26] = concat::<23, 3, 26>(
    concat::<17, 6, 23>(
        concat::<8, 9, 17>(concat::<7, 1, 8>(CORE_7, [ts(AUXTIN4, 0x027, 0x621, 0)]), nct67_tail!()),
        [
            tf(SMBUSMASTER0, 0x150, 0x151, 7, 0x622, 0),
            ts(SMBUSMASTER1, 0x670, 0xC26, 0),
            REST_7[0],
            REST_7[1],
            REST_7[2],
            REST_7[3],
        ],
    ),
    [REST_7[4], REST_7[5], REST_7[6]],
);

const SMB_2: [TempSrc; 2] = [tf(SMBUSMASTER0, 0x150, 0x151, 7, 0x622, 0), ts(SMBUSMASTER1, 0x670, 0xC26, 0)];

/// NCT6796D/6796D-R/6797D.
const T_6796: [TempSrc; 26] = concat::<19, 7, 26>(
    concat::<10, 9, 19>(concat::<8, 2, 10>(concat::<7, 1, 8>(CORE_7, [ts(AUXTIN4, 0x027, 0x621, 0)]), SMB_2), nct67_tail!()),
    REST_7,
);

/// NCT6798D/6799D (z TSENSOR).
const T_6798: [TempSrc; 27] = concat::<20, 7, 27>(
    concat::<11, 9, 20>(
        concat::<9, 2, 11>(
            concat::<7, 2, 9>(CORE_7, [ts(AUXTIN4, 0x027, 0x621, 0), tf(TSENSOR, 0x4A2, 0x4A1, 7, 0xC00, 0x496)]),
            SMB_2,
        ),
        nct67_tail!(),
    ),
    REST_7,
);

/// NCT6796D-S.
const T_6796DS: [TempSrc; 11] = [
    tf(CPUTIN, 0x073, 0x074, 7, 0x100, 0x491),
    tf(SYSTIN, 0x075, 0x076, 7, 0x200, 0x490),
    tf(AUXTIN0, 0x077, 0x078, 7, 0x300, 0x492),
    tf(AUXTIN1, 0x079, 0x07A, 7, 0x800, 0x493),
    tf(AUXTIN2, 0x07B, 0x07C, 7, 0x900, 0x494),
    tf(AUXTIN3, 0x07D, 0x07E, 7, 0xA00, 0x495),
    tf(AUXTIN4, 0x027, 0, 4, 0xC16, 0x496),
    tf(AUXTIN5, 0x449, 0, 4, 0x100, 0x4A2),
    tf(SMBUSMASTER0, 0x150, 0x151, 7, 0x622, 0),
    ts(PECI_0, 0x0720, 0, 0),
    ts(VIRTUAL_TEMP, 0, 0, 0),
];

/// NCT5585D.
const T_5585: [TempSrc; 4] = [
    ts(PECI_0, 0x0720, 0x100, 0),
    tf(CPUTIN, 0x075, 0x076, 7, 0x000, 0x073),
    tf(AUXTIN1, 0x07B, 0x07C, 7, 0x900, 0x493),
    tf(AUXTIN3, 0x4A0, 0x49E, 6, 0xB00, 0x495),
];

/// NCT6779D/6792D-A.
const T_6779: [TempSrc; 7] = [
    ts(PECI_0, 0x027, 0x621, 0),
    tf(CPUTIN, 0x073, 0x074, 7, 0x100, 0x491),
    tf(SYSTIN, 0x075, 0x076, 7, 0x200, 0x490),
    tf(AUXTIN0, 0x077, 0x078, 7, 0x300, 0x492),
    tf(AUXTIN1, 0x079, 0x07A, 7, 0x800, 0x493),
    tf(AUXTIN2, 0x07B, 0x07C, 7, 0x900, 0x494),
    tf(AUXTIN3, 0x150, 0x151, 7, 0x622, 0x495),
];

/// NCT6701D (bez połówek stopnia, kodowanie `decode_6701`).
const T_6701: [TempSrc; 29] = [
    ts(PECI_0, 0x073, 0x100, 0),
    ts(CPUTIN, 0x491, 0, 0),
    ts(SYSTIN, 0x490, 0, 0),
    ts(AUXTIN0, 0x492, 0, 0),
    ts(AUXTIN1, 0x493, 0, 0),
    ts(AUXTIN2, 0x494, 0, 0),
    ts(AUXTIN3, 0x495, 0, 0),
    ts(AUXTIN4, 0x027, 0x621, 0),
    ts(PECI_1, 0x672, 0xC27, 0),
    ts(PCH_CHIP_CPU_MAX, 0x674, 0xC28, 0x400),
    ts(PCH_CHIP, 0x676, 0xC29, 0x401),
    ts(PCH_CPU, 0x678, 0xC2A, 0x402),
    ts(PCH_MCH, 0x67A, 0xC2B, 0x404),
    ts(AGENT0_DIMM0, 0x405, 0, 0),
    ts(AGENT0_DIMM1, 0x406, 0, 0),
    ts(AGENT1_DIMM0, 0x407, 0, 0),
    ts(AGENT1_DIMM1, 0x408, 0, 0),
    ts(SMBUSMASTER0, 0x150, 0x622, 0),
    ts(SMBUSMASTER1, 0x670, 0xC26, 0),
    ts(BYTE_TEMP0, 0x419, 0, 0),
    ts(BYTE_TEMP1, 0x41A, 0, 0),
    ts(PECI_0_CAL, 0x4F4, 0, 0),
    ts(PECI_1_CAL, 0x4F5, 0, 0),
    ts(VIRTUAL_TEMP, 0, 0, 0),
    ts(SPARE_TEMP, 0x07B, 0x900, 0),
    ts(SPARE_TEMP2, 0, 0, 0),
    tn(0x409, 0, -1),
    tn(0x4A2, 0, -1),
    tn(0xC2F, 0, -1),
];

/// NCT6771F/6776F (źródło PECI_0: 5 albo 12); czytane są 4 pierwsze pozycje.
const fn t_6771(peci: u8) -> [TempSrc; 9] {
    [
        ts(peci, 0x027, 0x621, 0),
        tf(2, 0x073, 0x074, 7, 0x100, 0),
        tf(3, 0x075, 0x076, 7, 0x200, 0),
        tf(1, 0x077, 0x078, 7, 0x300, 0),
        TempSrc { source: None, reg: 0x150, half_reg: 0x151, half_bit: 7, source_reg: 0x622, alt_reg: None },
        TempSrc { source: None, reg: 0x250, half_reg: 0x251, half_bit: 7, source_reg: 0x623, alt_reg: None },
        TempSrc { source: None, reg: 0x62B, half_reg: 0x62E, half_bit: 0, source_reg: 0x624, alt_reg: None },
        TempSrc { source: None, reg: 0x62C, half_reg: 0x62E, half_bit: 1, source_reg: 0x625, alt_reg: None },
        TempSrc { source: None, reg: 0x62D, half_reg: 0x62E, half_bit: 2, source_reg: 0x626, alt_reg: None },
    ]
}

/// NCT610xD.
const T_610X: [TempSrc; 7] = [
    ts(12, 0x06B, 0x621, 0),
    tf(2, 0x010, 0x016, 0, 0, 0),
    tf(1, 0x011, 0x01B, 1, 0, 0),
    tf(3, 0x012, 0x01B, 2, 0, 0),
    tf(4, 0x013, 0x016, 3, 0, 0),
    tf(5, 0x014, 0x01B, 4, 0, 0),
    tf(6, 0x015, 0x01B, 5, 0, 0),
];

/// Sposób odczytu temperatur (gałęzie `Nct677X.Update`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TempAlgo {
    /// NCT6771F/6776F/6779D/6791D-6795D: źródło zawsze z rejestru.
    Legacy,
    /// NCT6796D i nowsze: źródło (5 bitów), pierwsze trafienie wygrywa.
    Masked,
    /// NCT6701D.
    D6701,
    /// NCT610xD: bez źródeł.
    D610x,
    /// NCT668x: rejestr + bit 7 następnego.
    Ec,
}

/// Obroty: 13-bitowe liczniki, 16-bitowe obr./min z progiem, albo EC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FanAlgo {
    Count13,
    Rpm16 { min: u16 },
    Ec,
}

const EC_TEMP_REGS: [u16; 11] = [0x100, 0x102, 0x104, 0x106, 0x108, 0x10A, 0x10C, 0x10E, 0x110, 0x112, 0x114];
const EC_FAN_REGS: [u16; 17] = [
    0x140, 0x142, 0x144, 0x146, 0x148, 0x14A, 0x14C, 0x14E, 0x150, 0x152, 0x154, 0x156, 0x158, 0x15A, 0x15C, 0x15E, 0x852,
];
/// Licznik 13-bitowy w przestrzeni EC (SYS Fan 7 na części płyt MSI).
const EC_FAN_COUNT_REG: u16 = 0x852;
const DR_FAN_REGS: [u16; 16] = [
    0x140, 0x142, 0x144, 0x146, 0x148, 0x14A, 0x14C, 0x14E, 0x150, 0x152, 0x15E, 0x15C, 0x15A, 0x158, 0x156, 0x154,
];
const DR_PWM_OUT: [u16; 16] = [
    0x160, 0x161, 0x162, 0x163, 0x164, 0x165, 0x166, 0x167, NO_REG, 0xC93, 0xE05, 0xE04, 0xE03, 0xE02, 0xE01, 0xE00,
];
const DR_PWM_COMMAND: [u16; 16] = [
    0xA28, 0xA29, 0xA2A, 0xA2B, NO_REG, NO_REG, NO_REG, NO_REG, NO_REG, 0x8E9, 0x265, 0x264, 0x263, 0x262, 0x261, 0x260,
];
const DR_MODE: [u16; 16] = [
    0xA00, 0xA00, 0xA00, 0xA00, 0xA00, 0xA00, 0xA00, 0xA00, 0xA00, 0x80F, 0x80F, 0x80F, 0x80F, 0x80F, 0x80F, 0x80F,
];
/// Bit trybu ręcznego w rejestrze trybu (-1: kanał bez sterowania).
const DR_MODE_BIT: [i8; 16] = [0, 1, 2, 3, -1, -1, -1, -1, -1, 1, 7, 6, 5, 4, 3, 2];
const COUNT_REGS: [u16; 7] = [0x4B0, 0x4B2, 0x4B4, 0x4B6, 0x4B8, 0x4BA, 0x4CC];

/// Rejestry i tablice jednego typu układu.
#[derive(Clone, Debug)]
struct Layout {
    temps: Vec<TempSrc>,
    /// Liczba temperatur widocznych w LHM (`Temperatures.Length`).
    temp_count: usize,
    temp_algo: TempAlgo,
    fan_regs: Vec<u16>,
    fan_algo: FanAlgo,
    pwm_out: Vec<u16>,
    pwm_command: Vec<u16>,
    mode: Vec<u16>,
    /// Rejestry identyfikatora producenta (starszy, młodszy); `None` - EC.
    vendor_regs: Option<(u16, u16)>,
}

impl Layout {
    fn of(kind: Kind) -> Self {
        let bank_pwm_out = if matches!(kind, Nct6797D | Nct6798D | Nct6799D | Nct6796DS | Nct5585D) {
            vec![0x001, 0x003, 0x011, 0x013, 0x015, 0xA09, 0xB09]
        } else {
            vec![0x001, 0x003, 0x011, 0x013, 0x015, 0x017, 0x029]
        };
        let bank_command = vec![0x109, 0x209, 0x309, 0x809, 0x909, 0xA09, 0xB09];
        let bank_mode = vec![0x102, 0x202, 0x302, 0x802, 0x902, 0xA02, 0xB02];
        let bank_vendor = Some((0x804F, 0x004F));
        let fans_d = match kind {
            Nct6779D => 5,
            Nct6796DR | Nct6796DS | Nct6797D | Nct6798D | Nct6799D | Nct6701D | Nct5585D => 7,
            _ => 6,
        };
        let bank_d = |temps: &[TempSrc], algo: TempAlgo| Layout {
            temps: temps.to_vec(),
            temp_count: temps.len(),
            temp_algo: algo,
            fan_regs: COUNT_REGS[..fans_d].to_vec(),
            fan_algo: FanAlgo::Count13,
            pwm_out: bank_pwm_out[..fans_d].to_vec(),
            pwm_command: bank_command[..fans_d].to_vec(),
            mode: bank_mode[..fans_d].to_vec(),
            vendor_regs: bank_vendor,
        };
        match kind {
            Nct6771F | Nct6776F => {
                let (fans, min, peci) = if kind == Nct6771F {
                    (4, (1.35e6 / 65535.0) as u16, 5)
                } else {
                    (5, (1.35e6 / 8191.0) as u16, 12)
                };
                Layout {
                    temps: t_6771(peci).to_vec(),
                    temp_count: 4,
                    temp_algo: TempAlgo::Legacy,
                    fan_regs: (0..fans).map(|i| 0x656 + (i << 1)).collect(),
                    fan_algo: FanAlgo::Rpm16 { min },
                    pwm_out: bank_pwm_out[..3].to_vec(),
                    pwm_command: bank_command[..3].to_vec(),
                    mode: bank_mode[..3].to_vec(),
                    vendor_regs: bank_vendor,
                }
            }
            Nct610x => Layout {
                temps: T_610X.to_vec(),
                temp_count: 7,
                temp_algo: TempAlgo::D610x,
                fan_regs: vec![0x030, 0x032, 0x034],
                fan_algo: FanAlgo::Rpm16 { min: (1.35e6 / 8191.0) as u16 },
                pwm_out: vec![0x04A, 0x04B, 0x04C],
                pwm_command: vec![0x119, 0x129, 0x139],
                mode: vec![0x113, 0x123, 0x133],
                vendor_regs: Some((0x80FE, 0x00FE)),
            },
            Nct6779D | Nct6792DA => bank_d(&T_6779, TempAlgo::Legacy),
            Nct6791D | Nct6792D | Nct6793D | Nct6795D => bank_d(&T_679X, TempAlgo::Legacy),
            Nct6796D | Nct6796DR | Nct6797D => bank_d(&T_6796, TempAlgo::Masked),
            Nct6798D | Nct6799D => bank_d(&T_6798, TempAlgo::Masked),
            Nct6796DS => bank_d(&T_6796DS, TempAlgo::Masked),
            Nct5585D => bank_d(&T_5585, TempAlgo::Masked),
            Nct6701D => bank_d(&T_6701, TempAlgo::D6701),
            Nct6683D | Nct6686D | Nct6687D => Layout {
                temps: EC_TEMP_REGS.iter().map(|&r| tn(r, 0, -1)).collect(),
                temp_count: EC_TEMP_REGS.len(),
                temp_algo: TempAlgo::Ec,
                fan_regs: EC_FAN_REGS.to_vec(),
                fan_algo: FanAlgo::Ec,
                pwm_out: (0x160..0x168).collect(),
                pwm_command: (0xA28..0xA30).collect(),
                mode: vec![0xA00; 8],
                vendor_regs: None,
            },
            Nct6687DR => Layout {
                temps: EC_TEMP_REGS[..7].iter().map(|&r| tn(r, 0, -1)).collect(),
                temp_count: 7,
                temp_algo: TempAlgo::Ec,
                fan_regs: DR_FAN_REGS.to_vec(),
                fan_algo: FanAlgo::Ec,
                pwm_out: DR_PWM_OUT.to_vec(),
                pwm_command: DR_PWM_COMMAND.to_vec(),
                mode: DR_MODE.to_vec(),
                vendor_regs: None,
            },
        }
    }
}

// ------------------------------------------------------------------ nazwy czujników

/// Czujniki widoczne w LHM: (indeks, nazwa) w kolejności list z `SuperIOHardware`.
#[derive(Clone, Debug, Default)]
struct Names {
    temps: Vec<(usize, String)>,
    fans: Vec<(usize, String)>,
    controls: Vec<(usize, String)>,
}

fn list(items: &[(usize, &str)]) -> Vec<(usize, String)> {
    items.iter().map(|&(i, n)| (i, n.to_string())).collect()
}

fn numbered(count: usize) -> Vec<(usize, String)> {
    (0..count).map(|i| (i, format!("Fan #{}", i + 1))).collect()
}

impl Names {
    /// Lista dla płyty z `boards.rs`, a bez niej konfiguracja domyślna LHM.
    fn of(kind: Kind, layout: &Layout, board: &Board, sio_index: usize) -> Self {
        let counts = Counts { temps: layout.temp_count, fans: layout.fan_regs.len(), controls: layout.pwm_out.len() };
        let chip = kind.slug().to_ascii_uppercase();
        if let Some(b) = boards::names(&chip, &board.manufacturer, &board.product, sio_index, counts) {
            return Names { temps: b.temps, fans: b.fans, controls: b.controls };
        }
        let fans = numbered(layout.fan_regs.len());
        let controls = numbered(layout.pwm_out.len());
        let cpu_core_and_numbered = |n: usize| {
            let mut t = vec![(0, "CPU Core".to_string())];
            t.extend((1..=n).map(|i| (i, format!("Temperature #{i}"))));
            t
        };
        match kind {
            Nct6771F | Nct6776F => Names { temps: cpu_core_and_numbered(3), fans, controls },
            Nct610x => Names {
                temps: list(&[
                    (0, "CPU Core"),
                    (1, "Auxiliary"),
                    (2, "System0"),
                    (3, "System1"),
                    (4, "System2"),
                    (5, "System3"),
                ]),
                fans,
                controls,
            },
            // LHM nie ma dla NCT5585D konfiguracji domyślnej.
            Nct5585D => Names::default(),
            Nct6686D | Nct6687D => {
                let fan_names = [
                    (0, "CPU Fan"),
                    (1, "Pump Fan"),
                    (2, "System Fan #1"),
                    (3, "System Fan #2"),
                    (4, "System Fan #3"),
                    (5, "System Fan #4"),
                    (6, "System Fan #5"),
                    (7, "System Fan #6"),
                ];
                Names {
                    temps: list(&[
                        (0, "CPU"),
                        (1, "System"),
                        (2, "VRM MOS"),
                        (3, "PCH"),
                        (4, "CPU Socket"),
                        (5, "PCIe x1"),
                        (6, "M2 #1"),
                    ]),
                    fans: list(&fan_names),
                    controls: list(&fan_names),
                }
            }
            Nct6687DR => Names {
                temps: list(&[(0, "CPU Core"), (1, "System"), (2, "VRM MOS"), (3, "Chipset")]),
                fans: list(&[
                    (0, "CPU Fan"),
                    (1, "Pump Fan #1"),
                    (2, "Chipset Fan"),
                    (10, "System Fan #1"),
                    (11, "System Fan #2"),
                    (12, "System Fan #3"),
                    (13, "System Fan #4"),
                    (14, "System Fan #5"),
                    (15, "System Fan #6"),
                    (3, "EZ-Connect Fan"),
                ]),
                controls: list(&[
                    (0, "CPU Fan"),
                    (1, "Pump Fan"),
                    (2, "Chipset Fan"),
                    (10, "System Fan #1"),
                    (11, "System Fan #2"),
                    (12, "System Fan #3"),
                    (13, "System Fan #4"),
                    (14, "System Fan #5"),
                    (15, "System Fan #6"),
                    (3, "EZ-Connect Fan"),
                ]),
            },
            // NCT6779D-6799D, 6701D, 6683D (i 6796D-S na innych płytach).
            _ => Names { temps: cpu_core_and_numbered(6), fans, controls },
        }
    }
}

// ------------------------------------------------------------------ układ

/// Rozpoznany układ Nuvoton.
pub struct Nuvoton {
    kind: Kind,
    /// Adres bazowy monitora sprzętu.
    base: u16,
    /// Indeks wśród układów tego samego typu (identyfikator LHM).
    index: usize,
    /// Czy identyfikator producenta się zgadza - bez tego brak czujników i zapisów.
    vendor_ok: bool,
    layout: Layout,
    names: Names,
    /// Temperatury jak `Temperatures` w LHM (wartości zostają między odczytami).
    temps: Vec<Option<f32>>,
    fans: Vec<Option<f32>>,
    controls: Vec<Option<f32>>,
    /// Czujnik miał już wartość (LHM pokazuje dopiero wtedy).
    temp_seen: Vec<bool>,
    fan_seen: Vec<bool>,
    /// Stan sprzed pierwszej zmiany: (tryb, PWM) na wentylator.
    saved: Vec<Option<(u8, u8)>>,
}

impl Nuvoton {
    fn new(kind: Kind, base: u16, board: &Board, sio_index: usize) -> Self {
        let layout = Layout::of(kind);
        let names = Names::of(kind, &layout, board, sio_index);
        let (t, f, c) = (layout.temp_count, layout.fan_regs.len(), layout.pwm_out.len());
        Self {
            kind,
            base,
            index: 0,
            vendor_ok: true,
            layout,
            names,
            temps: vec![None; t],
            fans: vec![None; f],
            controls: vec![None; c],
            temp_seen: vec![false; t],
            fan_seen: vec![false; f],
            saved: vec![None; c],
        }
    }

    // ---- rejestry monitora

    fn read(&self, bus: &dyn Bus, addr: u16) -> Result<u8, String> {
        let [bank_or_page, reg] = addr.to_be_bytes();
        if !self.kind.ec() {
            bus.outb(self.base + ADDRESS_OFFSET, BANK_SELECT)?;
            bus.outb(self.base + DATA_OFFSET, bank_or_page)?;
            bus.outb(self.base + ADDRESS_OFFSET, reg)?;
            return bus.inb(self.base + DATA_OFFSET);
        }
        self.ec_acquire(bus)?;
        bus.outb(self.base + EC_PAGE_OFFSET, bank_or_page)?;
        bus.outb(self.base + EC_INDEX_OFFSET, reg)?;
        let value = bus.inb(self.base + EC_DATA_OFFSET);
        // Strona wolna dla innych programów także po błędzie odczytu.
        let free = bus.outb(self.base + EC_PAGE_OFFSET, EC_PAGE_FREE);
        let value = value?;
        free?;
        Ok(value)
    }

    fn write(&self, bus: &dyn Bus, addr: u16, value: u8) -> Result<(), String> {
        let [bank_or_page, reg] = addr.to_be_bytes();
        if !self.kind.ec() {
            bus.outb(self.base + ADDRESS_OFFSET, BANK_SELECT)?;
            bus.outb(self.base + DATA_OFFSET, bank_or_page)?;
            bus.outb(self.base + ADDRESS_OFFSET, reg)?;
            return bus.outb(self.base + DATA_OFFSET, value);
        }
        self.ec_acquire(bus)?;
        bus.outb(self.base + EC_PAGE_OFFSET, bank_or_page)?;
        bus.outb(self.base + EC_INDEX_OFFSET, reg)?;
        let res = bus.outb(self.base + EC_DATA_OFFSET, value);
        let free = bus.outb(self.base + EC_PAGE_OFFSET, EC_PAGE_FREE);
        res?;
        free
    }

    /// Czeka (do 500 ms) na wolną stronę EC; potem przejmuje ją siłą (jak LHM).
    fn ec_acquire(&self, bus: &dyn Bus) -> Result<(), String> {
        let start = Instant::now();
        loop {
            if bus.inb(self.base + EC_PAGE_OFFSET)? == EC_PAGE_FREE {
                return Ok(());
            }
            if start.elapsed() >= Duration::from_millis(500) {
                return bus.outb(self.base + EC_PAGE_OFFSET, EC_PAGE_FREE);
            }
            sleep(Duration::from_millis(1));
        }
    }

    /// Identyfikator producenta 0x5CA3 (NCT668x - zawsze, jak w LHM).
    fn check_vendor(&self, bus: &dyn Bus) -> bool {
        if self.kind.ec() || self.kind == Nct6701D {
            return true;
        }
        let Some((hi, lo)) = self.layout.vendor_regs else { return true };
        match (self.read(bus, hi), self.read(bus, lo)) {
            (Ok(h), Ok(l)) => u16::from_be_bytes([h, l]) == NUVOTON_VENDOR_ID,
            _ => false,
        }
    }

    // ---- odczyty

    fn update_on(&mut self, bus: &dyn Bus) -> Result<(), String> {
        if !self.vendor_ok {
            return Ok(());
        }
        self.update_temps(bus)?;
        self.update_fans(bus)?;
        for i in 0..self.controls.len() {
            let v = self.read(bus, self.layout.pwm_out[i])? as f32 / 2.55;
            self.controls[i] = Some(if self.kind.ec() { v.round() } else { v });
        }
        for (seen, v) in self.temp_seen.iter_mut().zip(&self.temps) {
            *seen |= v.is_some();
        }
        for (seen, v) in self.fan_seen.iter_mut().zip(&self.fans) {
            *seen |= v.is_some();
        }
        Ok(())
    }

    fn update_temps(&mut self, bus: &dyn Bus) -> Result<(), String> {
        let table = self.layout.temps.clone();
        let n = self.temps.len();
        let bit = |s: u8| 1u64 << (s & 63);
        let range = |t: f32| (-55.0..=125.0).contains(&t).then_some(t);
        // Wartość w połówkach stopnia: rejestr << 1 | bit połówki.
        let halves = |this: &Self, ts: &TempSrc| -> Result<i32, String> {
            let mut v = (this.read(bus, ts.reg)? as i8 as i32) << 1;
            if ts.half_bit > 0 {
                v |= ((this.read(bus, ts.half_reg)? >> ts.half_bit) & 1) as i32;
            }
            Ok(v)
        };
        let mut mask = 0u64;
        match self.layout.temp_algo {
            TempAlgo::Ec => {
                for (i, ts) in table.iter().enumerate().take(n) {
                    let v = self.read(bus, ts.reg)? as i8 as f32;
                    let half = (self.read(bus, ts.reg + 1)? >> 7) & 1;
                    self.temps[i] = Some(v + 0.5 * half as f32);
                }
                return Ok(());
            }
            TempAlgo::D610x => {
                for (i, ts) in table.iter().enumerate().take(n) {
                    let v = self.read(bus, ts.reg)? as i8 as f32;
                    let half = if ts.half_bit >= 0 { (self.read(bus, ts.half_reg)? >> ts.half_bit) & 1 } else { 0 };
                    self.temps[i] = Some(v + 0.5 * half as f32);
                }
                return Ok(());
            }
            TempAlgo::Legacy => {
                for ts in &table {
                    let v = halves(self, ts)?;
                    let source = self.read(bus, ts.source_reg)?;
                    mask |= bit(source);
                    let t = range(0.5 * v as f32);
                    for j in 0..n {
                        if table[j].source == Some(source) {
                            self.temps[j] = t;
                        }
                    }
                }
            }
            TempAlgo::Masked => {
                for ts in &table {
                    if ts.reg == 0 {
                        continue;
                    }
                    let v = halves(self, ts)?;
                    let source = if ts.source_reg > 0 {
                        self.read(bus, ts.source_reg)? & 0x1F
                    } else {
                        ts.source.unwrap_or(0)
                    };
                    // Późniejsze rejestry tego samego źródła są bez połówek.
                    if mask & bit(source) != 0 {
                        continue;
                    }
                    let t = range(0.5 * v as f32);
                    if t.is_some() {
                        mask |= bit(source);
                    }
                    for j in 0..n {
                        if table[j].source == Some(source) {
                            self.temps[j] = t;
                        }
                    }
                }
            }
            TempAlgo::D6701 => {
                // Najpierw rejestry o stałym źródle, potem wybieralne.
                let mut order: Vec<usize> = (0..table.len()).collect();
                order.sort_by_key(|&i| table[i].source_reg > 0);
                for i in order {
                    let ts = table[i];
                    let Some(mut source) = ts.source else {
                        if i < n {
                            self.temps[i] = if ts.reg == 0 { None } else { decode_6701(self.read(bus, ts.reg)?) };
                        }
                        continue;
                    };
                    if ts.source_reg > 0 {
                        source = self.read(bus, ts.source_reg)?;
                        if !table.iter().any(|t| t.source == Some(source)) {
                            continue;
                        }
                    }
                    if mask & bit(source) != 0 || ts.reg == 0 {
                        continue;
                    }
                    let Some(t) = decode_6701(self.read(bus, ts.reg)?) else { continue };
                    mask |= bit(source);
                    for j in 0..n {
                        if table[j].source == Some(source) {
                            self.temps[j] = Some(t);
                        }
                    }
                }
            }
        }
        // Rejestry zapasowe dla źródeł bez odczytu.
        for (i, ts) in table.iter().enumerate().take(n) {
            let Some(alt) = ts.alt_reg else { continue };
            if ts.source.is_some_and(|s| mask & bit(s) != 0) {
                continue;
            }
            let t = self.read(bus, alt)? as i8 as f32;
            self.temps[i] = (t > 0.0 && t <= 125.0).then_some(t);
        }
        Ok(())
    }

    fn update_fans(&mut self, bus: &dyn Bus) -> Result<(), String> {
        for i in 0..self.fans.len() {
            let reg = self.layout.fan_regs[i];
            let high = self.read(bus, reg)?;
            let low = self.read(bus, reg + 1)?;
            self.fans[i] = match self.layout.fan_algo {
                FanAlgo::Count13 => count13(high, low, false),
                FanAlgo::Rpm16 { min } => {
                    let v = u16::from_be_bytes([high, low]);
                    Some(if v > min { v as f32 } else { 0.0 })
                }
                FanAlgo::Ec if reg == EC_FAN_COUNT_REG && self.kind != Nct6687DR => {
                    count13(high, low, self.kind == Nct6687D)
                }
                FanAlgo::Ec => match u16::from_be_bytes([high, low]) {
                    0xFFFF => None,
                    v => Some(v as f32),
                },
            };
        }
        Ok(())
    }

    // ---- sterowanie

    fn control_count(&self) -> usize {
        self.controls.len()
    }

    fn dr_valid(&self, index: usize) -> bool {
        DR_PWM_COMMAND.get(index).is_some_and(|&r| r != NO_REG) && DR_MODE_BIT.get(index).is_some_and(|&b| b >= 0)
    }

    /// Bit trybu ręcznego wentylatora w NCT668x.
    fn mode_mask(&self, index: usize) -> u8 {
        if self.kind == Nct6687DR {
            1u8 << DR_MODE_BIT[index].clamp(0, 7)
        } else {
            1u8 << (index & 7)
        }
    }

    fn set_on(&mut self, bus: &dyn Bus, index: usize, percent: Option<f32>) -> Result<(), String> {
        if self.is_read_only() {
            return Err(tr("Układ Nuvoton tylko do odczytu", "Nuvoton chip is read-only").into());
        }
        if index >= self.control_count() || (self.kind == Nct6687DR && !self.dr_valid(index)) {
            return Err(crate::trf!("Brak wentylatora {} w układzie {}", "No fan {} on chip {}", index, self.kind.name()));
        }
        let Some(percent) = percent else { return self.restore_on(bus, index) };
        // Jak LHM: bajt = procent * 2,55 obcięty.
        let value = (percent.clamp(0.0, 100.0) * 2.55) as u8;
        self.save_on(bus, index)?;
        let (mode_reg, cmd_reg) = (self.layout.mode[index], self.layout.pwm_command[index]);
        match self.kind {
            k if !k.ec() => {
                self.write(bus, mode_reg, 0)?;
                self.write(bus, cmd_reg, value)
            }
            Nct6687DR => {
                let mask = self.mode_mask(index);
                self.dr_configure(bus, index, |this, bus| {
                    this.update_byte(bus, mode_reg, !mask, mask)?;
                    this.write(bus, cmd_reg, value)
                })
            }
            _ => {
                self.write(bus, FAN_REQUEST_REG, FAN_CFG_REQUEST)?;
                sleep(Duration::from_millis(50));
                let mode = self.read(bus, mode_reg)? | self.mode_mask(index);
                self.write(bus, mode_reg, mode)?;
                self.write(bus, cmd_reg, value)?;
                self.write(bus, FAN_REQUEST_REG, FAN_CFG_COMPLETE)?;
                sleep(Duration::from_millis(50));
                Ok(())
            }
        }
    }

    /// Zapamiętuje tryb i PWM przed pierwszą zmianą wentylatora.
    fn save_on(&mut self, bus: &dyn Bus, index: usize) -> Result<(), String> {
        if self.saved[index].is_some() {
            return Ok(());
        }
        let mode = self.read(bus, self.layout.mode[index])?;
        let mode = if self.kind.ec() { mode & self.mode_mask(index) } else { mode };
        let pwm = self.read(bus, self.layout.pwm_command[index])?;
        self.saved[index] = Some((mode, pwm));
        Ok(())
    }

    /// Przywraca dokładnie zapamiętany tryb i PWM.
    fn restore_on(&mut self, bus: &dyn Bus, index: usize) -> Result<(), String> {
        let Some((mode, pwm)) = self.saved.get(index).copied().flatten() else { return Ok(()) };
        let (mode_reg, cmd_reg) = (self.layout.mode[index], self.layout.pwm_command[index]);
        match self.kind {
            k if !k.ec() => {
                self.write(bus, mode_reg, mode)?;
                self.write(bus, cmd_reg, pwm)?;
            }
            Nct6687DR => {
                let mask = self.mode_mask(index);
                self.dr_configure(bus, index, |this, bus| {
                    this.update_byte(bus, mode_reg, !mask, mode & mask)?;
                    this.write(bus, cmd_reg, pwm)
                })?;
            }
            _ => {
                let mask = self.mode_mask(index);
                let current = self.read(bus, mode_reg)?;
                self.write(bus, mode_reg, (current & !mask) | (mode & mask))?;
                self.write(bus, FAN_REQUEST_REG, FAN_CFG_REQUEST)?;
                sleep(Duration::from_millis(50));
                self.write(bus, cmd_reg, pwm)?;
                self.write(bus, FAN_REQUEST_REG, FAN_CFG_COMPLETE)?;
                sleep(Duration::from_millis(50));
            }
        }
        self.saved[index] = None;
        Ok(())
    }

    fn update_byte(&self, bus: &dyn Bus, addr: u16, and: u8, or: u8) -> Result<(), String> {
        let v = self.read(bus, addr)?;
        self.write(bus, addr, (v & and) | or)
    }

    /// NCT6687D-R: zmiana w fazie konfiguracji silnika wentylatorów (do 3 prób,
    /// gdy EC odrzuci konfigurację).
    fn dr_configure(
        &self,
        bus: &dyn Bus,
        index: usize,
        change: impl Fn(&Self, &dyn Bus) -> Result<(), String>,
    ) -> Result<(), String> {
        for _ in 0..3 {
            if !self.dr_start(bus)? {
                break;
            }
            change(self, bus)?;
            if self.dr_complete(bus)? {
                return Ok(());
            }
        }
        Err(crate::trf!(
            "Układ {} nie przyjął ustawienia wentylatora {}",
            "Chip {} rejected the setting of fan {}",
            self.kind.name(),
            index
        ))
    }

    fn dr_start(&self, bus: &dyn Bus) -> Result<bool, String> {
        let open = |s: u8| s & DR_CFG_LOCK == 0 && s & DR_CFG_PHASE != 0;
        if open(self.read(bus, DR_ENGINE_STATUS)?) {
            return Ok(true);
        }
        // Koniec poprzedniej fazy i brak oczekującego żądania.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            if self.read(bus, DR_ENGINE_STATUS)? & DR_CFG_PHASE == 0
                && self.read(bus, FAN_REQUEST_REG)? & FAN_CFG_REQUEST == 0
            {
                break;
            }
            sleep(Duration::from_millis(1));
        }
        self.update_byte(bus, FAN_REQUEST_REG, 0x7F, FAN_CFG_REQUEST)?;
        sleep(Duration::from_millis(10));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            if open(self.read(bus, DR_ENGINE_STATUS)?) {
                return Ok(true);
            }
            sleep(Duration::from_millis(1));
        }
        Ok(false)
    }

    fn dr_complete(&self, bus: &dyn Bus) -> Result<bool, String> {
        let s = self.read(bus, DR_ENGINE_STATUS)?;
        if s & DR_CFG_LOCK != 0 || s & DR_CFG_PHASE == 0 {
            return Ok(false);
        }
        self.update_byte(bus, FAN_REQUEST_REG, 0xBF, FAN_CFG_COMPLETE)?;
        sleep(Duration::from_millis(10));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            if self.read(bus, DR_ENGINE_STATUS)? & DR_CFG_CHECK_DONE != 0 {
                break;
            }
            sleep(Duration::from_millis(1));
        }
        Ok(self.read(bus, DR_ENGINE_STATUS)? & DR_CFG_INVALID == 0)
    }

    fn restore_all_on(&mut self, bus: &dyn Bus) {
        for i in 0..self.saved.len() {
            let _ = self.restore_on(bus, i);
        }
    }

    fn is_read_only(&self) -> bool {
        // NCT6683D: firmware producenta płyty (program traktuje go jako tylko do odczytu).
        !self.vendor_ok || self.kind == Nct6683D
    }

    fn readings_into(&self, out: &mut Readings) {
        if !self.vendor_ok {
            return;
        }
        let (id, hw) = (self.id_prefix(), self.kind.name());
        for (i, name) in &self.names.temps {
            if self.temp_seen.get(*i).copied().unwrap_or(false) {
                let value = self.temps[*i].and_then(valid_temp);
                out.temp(format!("{id}/temperature/{i}"), name, hw, "SuperIO", value);
            }
        }
        let control = |i: usize| self.names.controls.iter().any(|(c, _)| *c == i).then(|| self.controls.get(i).copied().flatten()).flatten();
        // Czujnik obrotów pokazywany w LHM (miał już wartość).
        let fan = |i: usize| {
            self.names
                .fans
                .iter()
                .find(|(f, _)| *f == i && self.fan_seen.get(i).copied().unwrap_or(false))
                .map(|(_, n)| (n.as_str(), self.fans[i]))
        };
        for (i, name) in &self.names.controls {
            if *i >= self.controls.len() {
                continue;
            }
            let (fan_name, rpm) = fan(*i).map_or((name.as_str(), None), |(n, r)| (n, r));
            out.fans.push(Fan {
                id: format!("{id}/control/{i}"),
                name: fan_name.into(),
                hw: hw.into(),
                kind: "SuperIO".into(),
                rpm,
                percent: self.controls[*i],
            });
        }
        for (i, _) in &self.names.fans {
            if let Some((name, rpm)) = fan(*i) {
                out.speeds.push(Speed { name: name.into(), hw: hw.into(), rpm, percent: control(*i) });
            }
        }
    }

    fn id_prefix(&self) -> String {
        format!("/lpc/{}/{}", self.kind.slug(), self.index)
    }
}

/// Rejestr żądania zmiany konfiguracji wentylatorów NCT668x.
const FAN_REQUEST_REG: u16 = 0xA01;

/// Licznik 13-bitowy na obr./min: przepełnienie - 0 (stoi), za mały - brak odczytu.
fn count13(high: u8, low: u8, nct6687d: bool) -> Option<f32> {
    if nct6687d && high == 0xFF && low == 0xF8 {
        return Some(0.0);
    }
    let count = (high as u32) << 5 | (low as u32 & 0x1F);
    if count >= 0x1FFF {
        Some(0.0)
    } else if count >= 0x15 {
        Some(1.35e6 / count as f32)
    } else {
        None
    }
}

/// NCT6701D: całe stopnie; 0x00, 0xA0 i 0x7E-0x80 to brak czujnika.
fn decode_6701(raw: u8) -> Option<f32> {
    match raw {
        0x00 | 0xA0 | 0x7E..=0x80 => None,
        v => Some(v as i8 as f32),
    }
}

impl Chip for Nuvoton {
    fn name(&self) -> String {
        self.kind.name().into()
    }

    fn id(&self) -> String {
        self.id_prefix()
    }

    fn read_only(&self) -> bool {
        self.is_read_only()
    }

    fn update(&mut self, lpc: &Lpc) -> Result<(), String> {
        self.update_on(lpc)
    }

    fn readings(&self, out: &mut Readings) {
        self.readings_into(out)
    }

    /// Wymaga blokady ISA trzymanej przez wołającego (jak każdy dostęp do `Lpc`).
    fn set_control(&mut self, lpc: &Lpc, index: usize, percent: Option<f32>) -> Result<(), String> {
        self.set_on(lpc, index, percent)
    }

    /// Wymaga blokady ISA trzymanej przez wołającego.
    fn restore_all(&mut self, lpc: &Lpc) {
        if !self.is_read_only() {
            self.restore_all_on(lpc)
        }
    }
}

// ------------------------------------------------------------------ testy

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    const BASE: u16 = 0x290;

    /// Symulacja monitora: banki (NCT67xx) albo strony EC (NCT668x).
    #[derive(Default)]
    struct FakeHwm {
        ec: bool,
        regs: RefCell<HashMap<u16, u8>>,
        index: Cell<u8>,
        bank: Cell<u8>,
        page: Cell<u8>,
        writes: RefCell<Vec<(u16, u8)>>,
    }

    impl FakeHwm {
        fn new(ec: bool) -> Self {
            let f = Self { ec, ..Default::default() };
            f.page.set(EC_PAGE_FREE);
            f
        }
        fn set(&self, addr: u16, v: u8) {
            self.regs.borrow_mut().insert(addr, v);
        }
        fn get(&self, addr: u16) -> u8 {
            self.regs.borrow().get(&addr).copied().unwrap_or(0)
        }
        fn addr(&self) -> u16 {
            let hi = if self.ec { self.page.get() } else { self.bank.get() };
            (hi as u16) << 8 | self.index.get() as u16
        }
    }

    impl Bus for FakeHwm {
        fn inb(&self, port: u16) -> Result<u8, String> {
            match port.checked_sub(BASE) {
                Some(4) if self.ec => Ok(self.page.get()),
                Some(6) if !self.ec && self.index.get() == BANK_SELECT => Ok(self.bank.get()),
                Some(6) => Ok(self.get(self.addr())),
                _ => Err(format!("port {port:#x}")),
            }
        }
        fn outb(&self, port: u16, v: u8) -> Result<(), String> {
            match port.checked_sub(BASE) {
                Some(4) if self.ec => self.page.set(v),
                Some(5) => self.index.set(v),
                Some(6) if !self.ec && self.index.get() == BANK_SELECT => self.bank.set(v),
                Some(6) => {
                    let a = self.addr();
                    self.set(a, v);
                    self.writes.borrow_mut().push((a, v));
                }
                _ => return Err(format!("port {port:#x}")),
            }
            Ok(())
        }
    }

    fn chip(kind: Kind, hwm: &FakeHwm) -> Nuvoton {
        let mut c = Nuvoton::new(kind, BASE, &Board::default(), 0);
        c.vendor_ok = c.check_vendor(hwm);
        c
    }

    fn nct6796dr() -> (FakeHwm, Nuvoton) {
        let h = FakeHwm::new(false);
        h.set(0x804F, 0x5C);
        h.set(0x004F, 0xA3);
        // PECI_0: 56,5 °C ze źródła 16, CPUTIN 36,5 °C, AUXTIN3 -42 °C.
        h.set(0x073, 56);
        h.set(0x074, 0x80);
        h.set(0x100, PECI_0);
        h.set(0x075, 36);
        h.set(0x076, 0x80);
        h.set(0x200, CPUTIN);
        h.set(0x4A0, (-42i8) as u8);
        h.set(0xB00, AUXTIN3);
        // Wentylator 1: licznik 991 -> 1362 obr./min; pozostałe: przepełnienie -> 0.
        for reg in COUNT_REGS {
            h.set(reg, 0xFF);
            h.set(reg + 1, 0x1F);
        }
        h.set(0x4B0, (991 >> 5) as u8);
        h.set(0x4B1, (991 & 0x1F) as u8);
        h.set(0x001, 127);
        h.set(0x102, 0x40);
        h.set(0x109, 0x80);
        h.set(0x202, 0x05);
        h.set(0x209, 0x33);
        let c = chip(Nct6796DR, &h);
        (h, c)
    }

    #[test]
    fn nct6796dr_readings_like_lhm() {
        let (h, mut c) = nct6796dr();
        assert!(c.vendor_ok && !c.is_read_only());
        assert_eq!(c.id_prefix(), "/lpc/nct6796dr/0");
        c.update_on(&h).unwrap();
        assert!(h.writes.borrow().is_empty(), "odczyt bez zapisów");
        assert_eq!(c.temps[0], Some(56.5));
        assert_eq!(c.temps[1], Some(36.5));
        assert_eq!(c.temps[6], Some(-42.0));
        let mut r = Readings::default();
        c.readings_into(&mut r);
        assert_eq!(r.temps[0].id, "/lpc/nct6796dr/0/temperature/0");
        assert_eq!(r.temps[0].name, "CPU Core");
        assert_eq!(r.temps[0].hw, "Nuvoton NCT6796D-R");
        assert_eq!(r.temps[0].kind, "SuperIO");
        let t6 = r.temps.iter().find(|t| t.id.ends_with("/temperature/6")).unwrap();
        assert_eq!((t6.name.as_str(), t6.value), ("Temperature #6", None));
        assert_eq!(r.fans.len(), 7);
        assert_eq!(r.fans[0].id, "/lpc/nct6796dr/0/control/0");
        assert_eq!(r.fans[0].name, "Fan #1");
        assert!((r.fans[0].rpm.unwrap() - 1362.26).abs() < 0.1);
        assert!((r.fans[0].percent.unwrap() - 49.80392).abs() < 0.001);
        assert_eq!(r.fans[1].rpm, Some(0.0));
        assert_eq!(r.speeds.len(), 7);
        assert_eq!(r.speeds[6].name, "Fan #7");
    }

    #[test]
    fn nct6796dr_set_and_restore_exact() {
        let (h, mut c) = nct6796dr();
        c.set_on(&h, 0, Some(50.0)).unwrap();
        assert_eq!(h.get(0x102), 0);
        assert_eq!(h.get(0x109), 127);
        c.set_on(&h, 0, Some(30.0)).unwrap();
        assert_eq!(h.get(0x109), 76);
        c.set_on(&h, 1, Some(100.0)).unwrap();
        assert_eq!(h.get(0x209), 255);
        c.set_on(&h, 0, None).unwrap();
        assert_eq!((h.get(0x102), h.get(0x109)), (0x40, 0x80));
        c.restore_all_on(&h);
        assert_eq!((h.get(0x202), h.get(0x209)), (0x05, 0x33));
        // Po przywróceniu kolejne przywrócenie nic nie zapisuje.
        let n = h.writes.borrow().len();
        c.restore_all_on(&h);
        assert_eq!(h.writes.borrow().len(), n);
        assert!(c.set_on(&h, 7, Some(10.0)).is_err());
    }

    #[test]
    fn wrong_vendor_is_read_only_without_writes() {
        let h = FakeHwm::new(false);
        let mut c = chip(Nct6798D, &h);
        assert!(!c.vendor_ok && c.is_read_only());
        c.update_on(&h).unwrap();
        let mut r = Readings::default();
        c.readings_into(&mut r);
        assert!(r.temps.is_empty() && r.fans.is_empty());
        assert!(c.set_on(&h, 0, Some(50.0)).is_err());
        c.restore_all_on(&h);
        assert!(h.writes.borrow().is_empty());
    }

    #[test]
    fn nct6687d_request_complete_protocol() {
        let h = FakeHwm::new(true);
        h.set(0x100, 45);
        h.set(0x101, 0x80);
        h.set(0x140, 0x05);
        h.set(0x141, 0xDC);
        h.set(0x142, 0xFF);
        h.set(0x143, 0xFF);
        h.set(0x160, 128);
        h.set(0xA00, 0b0000_0100);
        h.set(0xA29, 0x66);
        let mut c = chip(Nct6687D, &h);
        c.update_on(&h).unwrap();
        assert!(h.writes.borrow().is_empty());
        assert_eq!(c.temps[0], Some(45.5));
        assert_eq!(c.fans[0], Some(1500.0));
        assert_eq!(c.fans[1], None);
        assert_eq!(c.controls[0], Some(50.0));
        let mut r = Readings::default();
        c.readings_into(&mut r);
        assert_eq!(r.temps[0].name, "CPU");
        assert_eq!(r.fans[1].name, "Pump Fan");
        assert_eq!(r.fans[1].id, "/lpc/nct6687d/0/control/1");

        c.set_on(&h, 1, Some(40.0)).unwrap();
        let w: Vec<_> = h.writes.borrow().clone();
        assert_eq!(w, vec![(0xA01, 0x80), (0xA00, 0b0000_0110), (0xA29, 102), (0xA01, 0x40)]);
        assert_eq!(h.page.get(), EC_PAGE_FREE);
        h.writes.borrow_mut().clear();
        c.set_on(&h, 1, None).unwrap();
        assert_eq!(h.get(0xA00), 0b0000_0100);
        assert_eq!(h.get(0xA29), 0x66);
    }

    #[test]
    fn nct6683d_is_read_only() {
        let h = FakeHwm::new(true);
        let mut c = chip(Nct6683D, &h);
        assert!(c.is_read_only());
        assert!(c.set_on(&h, 0, Some(50.0)).is_err());
        c.restore_all_on(&h);
        assert!(h.writes.borrow().is_empty());
    }

    #[test]
    fn nct6687dr_config_phase() {
        let h = FakeHwm::new(true);
        // Faza konfiguracji otwarta, sprawdzenie zakończone, bez błędu.
        h.set(DR_ENGINE_STATUS, DR_CFG_PHASE | DR_CFG_CHECK_DONE);
        h.set(0x80F, 0b1000_0000);
        h.set(0x265, 0x40);
        let mut c = chip(Nct6687DR, &h);
        c.set_on(&h, 10, Some(20.0)).unwrap();
        assert_eq!(h.get(0x80F), 0b1000_0000);
        assert_eq!(h.get(0x265), 51);
        c.set_on(&h, 10, None).unwrap();
        assert_eq!(h.get(0x80F), 0b1000_0000);
        assert_eq!(h.get(0x265), 0x40);
        assert!(c.set_on(&h, 5, Some(20.0)).is_err());
        // EC odrzuca konfigurację -> błąd.
        h.set(DR_ENGINE_STATUS, DR_CFG_PHASE | DR_CFG_CHECK_DONE | DR_CFG_INVALID);
        assert!(c.set_on(&h, 0, Some(20.0)).is_err());
    }

    /// Port konfiguracji: tryb konfiguracji po 0x87 0x87, wyjście 0xAA.
    struct FakeConfig {
        hwm: FakeHwm,
        id: u8,
        rev: u8,
        entered: Cell<u8>,
        config: Cell<bool>,
        cr: RefCell<HashMap<u8, u8>>,
        index: Cell<u8>,
    }

    impl FakeConfig {
        fn new(id: u8, rev: u8) -> Self {
            let cr = HashMap::from([(0x60, (BASE >> 8) as u8), (0x61, BASE as u8), (IO_SPACE_LOCK, 0x10)]);
            Self {
                hwm: FakeHwm::new(false),
                id,
                rev,
                entered: Cell::new(0),
                config: Cell::new(false),
                cr: RefCell::new(cr),
                index: Cell::new(0),
            }
        }
    }

    impl Bus for FakeConfig {
        fn inb(&self, port: u16) -> Result<u8, String> {
            if port == 0x2F {
                return self.sio_inb(self.index.get());
            }
            self.hwm.inb(port)
        }
        fn outb(&self, port: u16, v: u8) -> Result<(), String> {
            match (port, v) {
                (0x2E, 0x87) => {
                    self.entered.set(self.entered.get() + 1);
                    self.config.set(self.entered.get() >= 2);
                    Ok(())
                }
                (0x2E, 0xAA) => {
                    self.entered.set(0);
                    self.config.set(false);
                    Ok(())
                }
                (0x2E, _) => {
                    self.index.set(v);
                    Ok(())
                }
                _ => self.hwm.outb(port, v),
            }
        }
    }

    impl ConfigBus for FakeConfig {
        fn config_port(&self) -> u16 {
            0x2E
        }
        fn sio_inb(&self, reg: u8) -> Result<u8, String> {
            if !self.config.get() {
                return Ok(0xFF);
            }
            Ok(match reg {
                CHIP_ID => self.id,
                CHIP_REVISION => self.rev,
                r => self.cr.borrow().get(&r).copied().unwrap_or(0),
            })
        }
        fn sio_outb(&self, reg: u8, v: u8) -> Result<(), String> {
            if self.config.get() {
                self.cr.borrow_mut().insert(reg, v);
            }
            Ok(())
        }
        fn find_bars(&self) -> Result<(), String> {
            if self.config.get() {
                Ok(())
            } else {
                Err("not in config mode".into())
            }
        }
    }

    #[test]
    fn probe_detects_and_leaves_config_mode() {
        let f = FakeConfig::new(0xD4, 0x2A);
        f.hwm.set(0x804F, 0x5C);
        f.hwm.set(0x004F, 0xA3);
        let c = probe_on(&f, &Board::default, 0).expect("NCT6796D-R");
        assert_eq!(c.kind, Nct6796DR);
        assert_eq!(c.base, BASE);
        assert!(c.vendor_ok);
        assert!(!f.config.get(), "tryb konfiguracji opuszczony");
        assert_eq!(f.cr.borrow()[&IO_SPACE_LOCK], 0x00, "blokada I/O zdjęta");
        assert_eq!(f.cr.borrow()[&DEVICE_SELECT], HWM_LDN);

        let msi = || Board { manufacturer: "Micro-Star International Co., Ltd.".into(), product: "MAG X870 TOMAHAWK WIFI (MS-7E51)".into() };
        let f = FakeConfig::new(0xD5, 0x92);
        assert_eq!(probe_on(&f, &msi, 0).unwrap().kind, Nct6687DR);
        let f = FakeConfig::new(0xD5, 0x92);
        assert_eq!(probe_on(&f, &Board::default, 0).unwrap().kind, Nct6687D);
    }

    #[test]
    fn probe_rejects_other_chips() {
        // ITE (ID 0x86), Fintek (0x07) - nie nasze, tryb opuszczony.
        for (id, rev) in [(0x86, 0x88), (0x07, 0x23), (0xD4, 0x99)] {
            let f = FakeConfig::new(id, rev);
            assert!(probe_on(&f, &Board::default, 0).is_none());
            assert!(!f.config.get());
        }
        // Zły adres bazowy.
        let f = FakeConfig::new(0xD4, 0x2A);
        f.cr.borrow_mut().insert(0x60, 0);
        f.cr.borrow_mut().insert(0x61, 0x2E);
        assert!(probe_on(&f, &Board::default, 0).is_none());
        assert!(!f.config.get());
    }

    #[test]
    fn identify_table() {
        let b = Board::default;
        assert_eq!(identify(0xB4, 0x73, &b), Some(Nct6771F));
        assert_eq!(identify(0xC7, 0x32, &b), Some(Nct6683D));
        assert_eq!(identify(0xD4, 0x41, &b), Some(Nct6686D));
        assert_eq!(identify(0xD8, 0x02, &b), Some(Nct6799D));
        assert_eq!(identify(0xD8, 0x06, &b), Some(Nct6701D));
        let nova = || Board { manufacturer: "ASRock".into(), product: "X870E Nova WiFi".into() };
        assert_eq!(identify(0xD4, 0x2A, &nova), Some(Nct5585D));
        assert_eq!(identify(0xD8, 0x02, &nova), Some(Nct6796DS));
        assert_eq!(identify(0x87, 0x87, &b), None);
        assert_eq!(T_6796.len(), 26);
        assert_eq!(T_6798[8].source, Some(TSENSOR));
        assert_eq!(T_679X[17].source, Some(SMBUSMASTER0));
        assert_eq!(T_679X[25].source, Some(SPARE_TEMP2));
    }

    #[test]
    fn board_names() {
        let board = |m: &str, p: &str| Board { manufacturer: m.into(), product: p.into() };
        let names = |kind: Kind, b: &Board| Names::of(kind, &Layout::of(kind), b, 0);
        // Płyta programisty (brak listy w LHM): nazwy domyślne.
        let n = names(Nct6796DR, &board("JGINYUE", "B760I GAMING"));
        assert_eq!(n.temps[0], (0, "CPU Core".to_string()));
        assert_eq!(n.temps[1], (1, "Temperature #1".to_string()));
        assert_eq!(n.fans[0], (0, "Fan #1".to_string()));
        assert_eq!(n.controls.len(), 7);
        // Listy przeniesione do boards.rs.
        let nova = names(Nct6796DS, &board("ASRock", "X870E Nova WiFi"));
        assert_eq!(nova.temps[4], (4, "Thermistor Sensor #1".to_string()));
        assert_eq!(nova.fans[4], (4, "Water Pump".to_string()));
        let hdv = names(Nct6796DS, &board("ASRock", "B650M-HDV/M.2"));
        assert_eq!(hdv.controls[5], (5, "Fan #6".to_string()));
        let mos = names(Nct5585D, &board("ASRock", "X870E Nova WiFi"));
        assert_eq!(mos.controls, vec![(1, "MOS Fan".to_string())]);
        assert!(names(Nct5585D, &Board::default()).temps.is_empty());
        // ASUS z NCT6798D.
        let asus = names(Nct6798D, &board("ASUSTeK COMPUTER INC.", "ROG CROSSHAIR VIII HERO"));
        assert_eq!(asus.temps.iter().find(|(i, _)| *i == 22).map(|(_, s)| s.as_str()), Some("PECI 1 Calibrated"));
        assert_eq!(asus.fans[5], (5, "Waterpump".to_string()));
        // MSI z NCT6687D-R: lista domyślna albo z dodatkami modelu.
        let msi = names(Nct6687DR, &board("Micro-Star International Co., Ltd.", "MAG X870E TOMAHAWK WIFI (MS-7E59)"));
        assert_eq!(msi.temps.last(), Some(&(4, "CPU Socket".to_string())));
        assert_eq!(names(Nct6687DR, &Board::default()).temps.len(), 4);
    }

    #[test]
    fn fan_counters() {
        assert_eq!(count13(0xFF, 0x1F, false), Some(0.0));
        assert_eq!(count13(0, 0x10, false), None);
        assert_eq!(count13(0xFF, 0xF8, true), Some(0.0));
        assert_eq!(decode_6701(0x80), None);
        assert_eq!(decode_6701(0x30), Some(48.0));
    }

    /// Odczyt z prawdziwego układu przez `superio::detect` (tylko odczyt, bez
    /// zapisów do monitora): `cargo test --release nuvoton_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn nuvoton_live() {
        let module = std::path::Path::new(r"D:/tune-center-main/dist/modules/LpcIO.bin");
        let chips = super::super::superio::detect(module).expect("LpcIO.bin");
        let (lpc, mut c) = chips
            .into_iter()
            .find(|(_, c)| c.name().starts_with("Nuvoton"))
            .expect("brak układu Nuvoton");
        println!("{} {} read_only={}", c.name(), c.id(), c.read_only());
        for _ in 0..3 {
            {
                let _lock = lpc.lock().expect("ISA lock");
                c.update(&lpc).unwrap();
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        let mut r = Readings::default();
        c.readings(&mut r);
        for t in &r.temps {
            println!("T {} | {} | {:?}", t.id, t.name, t.value);
        }
        for f in &r.fans {
            println!("F {} | {} | {:?} rpm | {:?} %", f.id, f.name, f.rpm, f.percent);
        }
        for s in &r.speeds {
            println!("S {} | {:?} rpm | {:?} %", s.name, s.rpm, s.percent);
        }
    }
}
