//! Układy Super I/O płyty głównej (czujniki temperatury, obroty i sterowanie
//! wentylatorami) przez moduł PawnIO `LpcIO.bin`.
//!
//! Moduł daje dostęp do portu konfiguracji układu (0x2E/0x2F albo 0x4E/0x4F,
//! `ioctl_select_slot`) i - po `ioctl_find_bars` w trybie konfiguracji - do
//! zakresów portów (BAR) znalezionych w logicznych urządzeniach układu.
//! Każdy dostęp odbywa się pod blokadą "Global\Access_ISABUS.HTP.Method",
//! wspólną z innymi programami sprzętowymi.

use std::path::Path;

use crate::cpu::pawnio::{NamedLock, PawnIo};

use super::Readings;

/// Blokada szyny ISA (ta sama co w HWiNFO, LibreHardwareMonitor, AIDA64).
pub const ISA_LOCK: &str = r"Global\Access_ISABUS.HTP.Method";

/// Dostęp do portów Super I/O przez `LpcIO.bin`.
pub struct Lpc {
    io: PawnIo,
    /// Port rejestru konfiguracji wybranego gniazda (0x2E albo 0x4E).
    pub port: u16,
}

impl Lpc {
    pub fn open(module: &Path) -> Result<Self, String> {
        Ok(Self { io: PawnIo::open(module)?, port: 0 })
    }

    /// Blokada szyny ISA na czas serii operacji.
    pub fn lock(&self) -> Result<NamedLock, String> {
        NamedLock::acquire(ISA_LOCK, 100)
    }

    /// Gniazdo 0 = porty 0x2E/0x2F, 1 = 0x4E/0x4F. Zeruje znalezione BAR.
    pub fn select_slot(&mut self, slot: u8) -> Result<(), String> {
        self.io.call(b"ioctl_select_slot\0", &[slot as u64], &mut [])?;
        self.port = if slot == 0 { 0x2E } else { 0x4E };
        Ok(())
    }

    /// Szuka zakresów portów układu (wymaga trybu konfiguracji i poprawnego ID).
    /// Dopiero potem `inb`/`outb` mogą sięgać do tych zakresów.
    pub fn find_bars(&self) -> Result<(), String> {
        self.io.call(b"ioctl_find_bars\0", &[], &mut [])
    }

    /// Bajt z portu (port konfiguracji albo znaleziony BAR).
    pub fn inb(&self, port: u16) -> Result<u8, String> {
        let mut out = [0u64; 1];
        self.io.call(b"ioctl_pio_inb\0", &[port as u64], &mut out)?;
        Ok(out[0] as u8)
    }

    pub fn outb(&self, port: u16, value: u8) -> Result<(), String> {
        self.io.call(b"ioctl_pio_outb\0", &[port as u64, value as u64], &mut [])
    }

    /// Rejestr konfiguracji układu.
    pub fn sio_inb(&self, reg: u8) -> Result<u8, String> {
        let mut out = [0u64; 1];
        self.io.call(b"ioctl_superio_inb\0", &[reg as u64], &mut out)?;
        Ok(out[0] as u8)
    }

    /// Słowo z rejestrów konfiguracji (`reg` starszy bajt, `reg + 1` młodszy).
    pub fn sio_inw(&self, reg: u8) -> Result<u16, String> {
        let mut out = [0u64; 1];
        self.io.call(b"ioctl_superio_inw\0", &[reg as u64], &mut out)?;
        Ok(out[0] as u16)
    }

    pub fn sio_outb(&self, reg: u8, value: u8) -> Result<(), String> {
        self.io.call(b"ioctl_superio_outb\0", &[reg as u64, value as u64], &mut [])
    }
}

/// Jeden układ Super I/O z czujnikami i (opcjonalnie) sterowaniem wentylatorami.
pub trait Chip: Send {
    /// Nazwa jak w LibreHardwareMonitor, np. "Nuvoton NCT6796D-R".
    fn name(&self) -> String;
    /// Początek identyfikatorów jak w LibreHardwareMonitor, np. "/lpc/nct6796dr/0".
    fn id(&self) -> String;
    /// Czy układ udostępnia tylko odczyt (np. firmware producenta płyty).
    fn read_only(&self) -> bool {
        false
    }
    /// Odczyt rejestrów (pod blokadą ISA trzymaną przez wołającego).
    fn update(&mut self, lpc: &Lpc) -> Result<(), String>;
    /// Wyniki ostatniego `update`: temperatury (kind "SuperIO"), wentylatory
    /// ze sterowaniem i wszystkie obroty - z identyfikatorami i nazwami jak w
    /// LibreHardwareMonitor.
    fn readings(&self, out: &mut Readings);
    /// Ustawia wentylator `index` (indeks sterowania jak w identyfikatorze
    /// `.../control/<index>`) na `percent`, albo `None` - oddaje go płycie
    /// (stan sprzed pierwszej zmiany).
    fn set_control(&mut self, lpc: &Lpc, index: usize, percent: Option<f32>) -> Result<(), String>;
    /// Oddaje płycie wszystkie zmienione wentylatory (wyjście, awaria).
    fn restore_all(&mut self, lpc: &Lpc);
}

/// Funkcja rozpoznania układu w wybranym gnieździe: sama wchodzi i wychodzi z
/// trybu konfiguracji, a po rozpoznaniu woła `find_bars`. `index` - numer
/// układu wśród już wykrytych (`superIOIndex` w LHM, dla nazw czujników płyty).
pub type Probe = fn(&mut Lpc, slot: u8, index: usize) -> Option<Box<dyn Chip>>;

/// Płyta główna z SMBIOS: typ części układów i nazwy czujników zależą od modelu.
#[derive(Default, Clone, Debug)]
pub struct Board {
    pub manufacturer: String,
    pub product: String,
}

impl Board {
    pub fn from_registry() -> Self {
        use crate::tune::reg;
        const BIOS: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
        Self {
            manufacturer: reg::get_string(reg::HKLM, BIOS, "BaseBoardManufacturer").unwrap_or_default(),
            product: reg::get_string(reg::HKLM, BIOS, "BaseBoardProduct").unwrap_or_default(),
        }
    }
}

/// Rozpoznane układy z obu gniazd - każdy z własną instancją modułu
/// `LpcIO.bin`: wybór gniazda (`ioctl_select_slot`) zeruje w module znalezione
/// zakresy portów, więc jedna instancja obsłuży tylko jedno gniazdo.
pub fn detect(module: &Path) -> Result<Vec<(Lpc, Box<dyn Chip>)>, String> {
    let probes: [Probe; 3] = [super::nuvoton::probe, super::ite::probe, super::fintek::probe];
    let mut out = Vec::new();
    for slot in 0..2u8 {
        let mut lpc = Lpc::open(module)?;
        let found = {
            // Przy wykrywaniu czekamy dłużej: blokadę może trzymać inny program.
            let Ok(_lock) = NamedLock::acquire(ISA_LOCK, 2000) else { continue };
            if lpc.select_slot(slot).is_err() {
                continue;
            }
            let index = out.len();
            probes.iter().find_map(|p| p(&mut lpc, slot, index))
        };
        if let Some(chip) = found {
            out.push((lpc, chip));
        }
    }
    Ok(out)
}
