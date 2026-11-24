//! Dostęp do procesora przez sterownik PawnIO i jego oficjalne, podpisane
//! moduły (github.com/namazso/PawnIO.Modules, LGPL-2.1):
//!
//! - `IntelMSR.bin` - rejestry MSR Intela; moduł sam ogranicza, które wolno
//!   czytać i zapisywać (zapis m.in. limitów mocy 0x610 i skrzynki OC 0x150),
//! - `RyzenSMU.bin` - polecenia SMU procesorów AMD Ryzen,
//! - `AMDFamily17.bin` - odczyt rejestrów SMN (mapa rdzeni AMD).
//!
//! Sygnatury funkcji pochodzą z nagłówka PawnIOLib.h (github.com/namazso/PawnIO).

use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};

use super::amd::{self, Mailbox, Smu};
use super::intel::Msr;

type Handle = *mut c_void;
type FnOpen = unsafe extern "system" fn(*mut Handle) -> i32;
type FnLoad = unsafe extern "system" fn(Handle, *const u8, usize) -> i32;
type FnExecute = unsafe extern "system" fn(
    Handle,
    *const c_char,
    *const u64,
    usize,
    *mut u64,
    usize,
    *mut usize,
) -> i32;
type FnClose = unsafe extern "system" fn(Handle) -> i32;

pub struct PawnIo {
    _lib: crate::dll::Library,
    handle: Handle,
    execute: FnExecute,
    close: FnClose,
}

/// Miejsca, w których instalator PawnIO umieszcza bibliotekę.
fn library_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["ProgramW6432", "ProgramFiles"] {
        if let Ok(dir) = std::env::var(var) {
            let path = Path::new(&dir).join("PawnIO").join("PawnIOLib.dll");
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out.push(PathBuf::from("PawnIOLib.dll"));
    out
}

fn hresult(name: &str, code: i32) -> Result<(), String> {
    if code >= 0 {
        Ok(())
    } else {
        Err(crate::trf!(
            "{name}: błąd 0x{:08X}",
            "{name}: error 0x{:08X}",
            code as u32
        ))
    }
}

impl PawnIo {
    /// Otwiera sterownik i ładuje moduł z podanego pliku.
    pub fn open(module: &Path) -> Result<Self, String> {
        let blob = std::fs::read(module).map_err(|e| {
            crate::trf!(
                "Brak modułu {} ({e}). Uruchom ponownie build.ps1, który go pobiera.",
                "Module {} is missing ({e}). Run build.ps1 again; it downloads the module.",
                module.display()
            )
        })?;

        unsafe {
            let lib = library_candidates()
                .into_iter()
                .find_map(|path| crate::dll::Library::new(path).ok())
                .ok_or_else(|| {
                    crate::i18n::tr(
                        "Nie znaleziono PawnIOLib.dll. Zainstaluj sterownik PawnIO (pawnio.eu).",
                        "PawnIOLib.dll was not found. Install the PawnIO driver (pawnio.eu).",
                    )
                    .to_string()
                })?;

            let open: FnOpen = *lib.get::<FnOpen>(b"pawnio_open\0").map_err(|e| {
                crate::trf!(
                    "PawnIOLib: brak pawnio_open ({e})",
                    "PawnIOLib: pawnio_open is missing ({e})"
                )
            })?;
            let load: FnLoad = *lib.get::<FnLoad>(b"pawnio_load\0").map_err(|e| {
                crate::trf!(
                    "PawnIOLib: brak pawnio_load ({e})",
                    "PawnIOLib: pawnio_load is missing ({e})"
                )
            })?;
            let execute: FnExecute = *lib.get::<FnExecute>(b"pawnio_execute\0").map_err(|e| {
                crate::trf!(
                    "PawnIOLib: brak pawnio_execute ({e})",
                    "PawnIOLib: pawnio_execute is missing ({e})"
                )
            })?;
            let close: FnClose = *lib.get::<FnClose>(b"pawnio_close\0").map_err(|e| {
                crate::trf!(
                    "PawnIOLib: brak pawnio_close ({e})",
                    "PawnIOLib: pawnio_close is missing ({e})"
                )
            })?;

            let mut handle: Handle = std::ptr::null_mut();
            hresult("pawnio_open", open(&mut handle)).map_err(|e| {
                crate::trf!("{e}. Program musi działać jako administrator, a sterownik PawnIO musi być zainstalowany.", "{e}. The program must run as administrator and the PawnIO driver must be installed.")
            })?;

            if let Err(e) = hresult("pawnio_load", load(handle, blob.as_ptr(), blob.len())) {
                close(handle);
                let name = module.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                // 0x80070032 = ERROR_NOT_SUPPORTED: moduł nie znalazł swojego sprzętu
                // (np. moduł SMBus Intela na płycie z chipsetem AMD).
                if e.contains("0x80070032") {
                    return Err(crate::trf!(
                        "{e}. Moduł {name} nie obsługuje sprzętu tego komputera.",
                        "{e}. Module {name} does not support this computer's hardware."
                    ));
                }
                return Err(crate::trf!("{e}. Sterownik odrzucił moduł {} - plik jest uszkodzony, nie pasuje do tego procesora albo sterownik PawnIO jest zbyt stary.", "{e}. The driver rejected module {} - the file is damaged, does not match this processor, or the PawnIO driver is too old.",
                    module.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                ));
            }

            Ok(Self {
                _lib: lib,
                handle,
                execute,
                close,
            })
        }
    }

    /// Wywołuje funkcję modułu; `name` musi kończyć się bajtem zerowym.
    pub fn call(
        &self,
        name: &'static [u8],
        input: &[u64],
        output: &mut [u64],
    ) -> Result<(), String> {
        debug_assert_eq!(name.last(), Some(&0));
        let mut returned = 0usize;
        let code = unsafe {
            (self.execute)(
                self.handle,
                name.as_ptr() as *const c_char,
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut returned,
            )
        };
        let label = String::from_utf8_lossy(&name[..name.len() - 1]).into_owned();
        hresult(&label, code)
    }
}

impl Msr for PawnIo {
    fn read(&mut self, msr: u64) -> Result<u64, String> {
        let mut out = [0u64; 1];
        self.call(b"ioctl_read_msr\0", &[msr], &mut out)
            .map_err(|e| crate::trf!("odczyt MSR 0x{msr:X}: {e}", "reading MSR 0x{msr:X}: {e}"))?;
        Ok(out[0])
    }

    fn write(&mut self, msr: u64, value: u64) -> Result<(), String> {
        self.call(b"ioctl_write_msr\0", &[msr, value], &mut [])
            .map_err(|e| {
                crate::trf!("zapis MSR 0x{msr:X}: {e}. Jeśli to błąd dostępu, moduł IntelMSR.bin jest za stary i nie pozwala na zapis.", "writing MSR 0x{msr:X}: {e}. If this is an access error, the IntelMSR.bin module is too old and does not allow writing."
                )
            })
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle);
        }
    }
}

// ---------------------------------------------------------------------------
// AMD: polecenia SMU i odczyt SMN
// ---------------------------------------------------------------------------

/// Blokada "Global\\Access_PCI": wspólna umowa programów sprzętowych
/// (HWiNFO, LibreHardwareMonitor, Ryzen Master...), żeby nie wchodzić sobie
/// w drogę przy dostępie do SMU. Moduły PawnIO wymagają jej przed wywołaniem.
struct PciLock(#[allow(dead_code)] isize);

/// Blokada nazwana Windows na czas dostępu do sprzętu (np. "Global\Access_SMBUS.HTP.Method"
/// dla magistrali SMBus - tę samą biorą LibreHardwareMonitor, HWiNFO, OpenRGB).
pub struct NamedLock(isize);

impl NamedLock {
    #[cfg(windows)]
    pub fn acquire(name: &str, timeout_ms: u32) -> Result<Self, String> {
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        // SAFETY: nazwa zakończona zerem; uchwyt zamyka Drop.
        unsafe {
            let handle = pci_mutex::CreateMutexW(std::ptr::null(), 0, wide.as_ptr());
            if handle == 0 {
                return Err(crate::trf!(
                    "nie udało się otworzyć blokady {name}",
                    "could not open the {name} lock"
                ));
            }
            let wait = pci_mutex::WaitForSingleObject(handle, timeout_ms);
            if wait != 0 && wait != 0x80 {
                pci_mutex::CloseHandle(handle);
                return Err(crate::trf!(
                    "inny program trzyma blokadę {name}",
                    "another program is holding the {name} lock"
                ));
            }
            Ok(Self(handle))
        }
    }

    #[cfg(not(windows))]
    pub fn acquire(_name: &str, _timeout_ms: u32) -> Result<Self, String> {
        Ok(Self(0))
    }
}

impl Drop for NamedLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: uchwyt z CreateMutexW, zdobyty w acquire.
        unsafe {
            pci_mutex::ReleaseMutex(self.0);
            pci_mutex::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
mod pci_mutex {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateMutexW(attributes: *const u8, initial_owner: i32, name: *const u16) -> isize;
        pub fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
        pub fn ReleaseMutex(handle: isize) -> i32;
        pub fn CloseHandle(handle: isize) -> i32;
    }
}

impl PciLock {
    #[cfg(windows)]
    fn acquire() -> Result<Self, String> {
        let name: Vec<u16> = "Global\\Access_PCI\0".encode_utf16().collect();
        unsafe {
            let handle = pci_mutex::CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle == 0 {
                return Err(crate::i18n::tr(
                    "nie udało się otworzyć blokady Access_PCI",
                    "could not open the Access_PCI lock",
                )
                .to_string());
            }
            // 0 = zdobyta, 0x80 = zdobyta po porzuceniu przez inny program
            let wait = pci_mutex::WaitForSingleObject(handle, 2000);
            if wait != 0 && wait != 0x80 {
                pci_mutex::CloseHandle(handle);
                return Err(crate::i18n::tr(
                    "inny program blokuje dostęp do SMU (Access_PCI)",
                    "another program is holding the SMU lock (Access_PCI)",
                )
                .to_string());
            }
            Ok(Self(handle))
        }
    }

    #[cfg(not(windows))]
    fn acquire() -> Result<Self, String> {
        Ok(Self(0))
    }
}

impl Drop for PciLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            pci_mutex::ReleaseMutex(self.0);
            pci_mutex::CloseHandle(self.0);
        }
    }
}

/// Sterownik procesorów AMD: SMU obowiązkowo, SMN opcjonalnie (bez niego
/// niedostępny jest tylko Curve Optimizer).
pub struct AmdDriver {
    smu: PawnIo,
    smn: Result<PawnIo, String>,
    /// Wersja tabeli PM, ustalana przy pierwszym odczycie.
    table_version: Option<u32>,
}

/// Ile komórek 64-bitowych tabeli PM czytamy (2 liczby na komórkę = 256 liczb;
/// limity leżą na początku tabeli).
const PM_TABLE_CELLS: usize = 128;

impl AmdDriver {
    pub fn open(smu_module: &Path, smn_module: &Path) -> Result<Self, String> {
        Ok(Self {
            smu: PawnIo::open(smu_module)?,
            smn: PawnIo::open(smn_module),
            table_version: None,
        })
    }
}

impl Smu for AmdDriver {
    fn command(&mut self, cmd: u32, args: [u32; 6]) -> Result<[u32; 6], String> {
        let _lock = PciLock::acquire()?;
        let mut input = [0u64; 7];
        input[0] = cmd as u64;
        for (slot, arg) in input[1..].iter_mut().zip(args) {
            *slot = arg as u64;
        }
        let mut output = [0u64; 6];
        self.smu
            .call(b"ioctl_send_smu_command\0", &input, &mut output)
            .map_err(|e| {
                crate::trf!(
                    "polecenie SMU 0x{cmd:02X}: {e} (SMU odrzuciło polecenie)",
                    "SMU command 0x{cmd:02X}: {e} (the SMU rejected the command)"
                )
            })?;
        Ok(output.map(|v| v as u32))
    }

    /// Skrzynka MP1 nie ma własnej funkcji w module, więc wykonujemy ją na
    /// rejestrach SMU (moduł pozwala na dostęp do zakresu skrzynek 0x3B10xxx),
    /// trzymając blokadę przez całe polecenie.
    fn mp1_command(
        &mut self,
        mailbox: Mailbox,
        cmd: u32,
        args: [u32; 6],
    ) -> Result<[u32; 6], String> {
        let _lock = PciLock::acquire()?;
        let smu = &self.smu;
        let mut read = |address: u32| -> Result<u32, String> {
            let mut output = [0u64; 1];
            smu.call(b"ioctl_read_smu_register\0", &[address as u64], &mut output)
                .map_err(|e| {
                    crate::trf!(
                        "odczyt rejestru SMU 0x{address:08X}: {e}",
                        "reading SMU register 0x{address:08X}: {e}"
                    )
                })?;
            Ok(output[0] as u32)
        };
        let mut write = |address: u32, value: u32| -> Result<(), String> {
            smu.call(
                b"ioctl_write_smu_register\0",
                &[address as u64, value as u64],
                &mut [],
            )
            .map_err(|e| {
                crate::trf!(
                    "zapis rejestru SMU 0x{address:08X}: {e}",
                    "writing SMU register 0x{address:08X}: {e}"
                )
            })
        };
        amd::mailbox_transaction(&mut read, &mut write, mailbox, cmd, args)
    }

    fn pm_table(&mut self) -> Result<(u32, Vec<f32>), String> {
        let _lock = PciLock::acquire()?;
        let version = match self.table_version {
            Some(version) => version,
            None => {
                let mut info = [0u64; 2];
                self.smu
                    .call(b"ioctl_resolve_pm_table\0", &[], &mut info)
                    .map_err(|e| crate::trf!("tabela PM: {e}", "PM table: {e}"))?;
                let version = info[0] as u32;
                self.table_version = Some(version);
                version
            }
        };
        self.smu
            .call(b"ioctl_update_pm_table\0", &[], &mut [])
            .map_err(|e| {
                crate::trf!("odświeżenie tabeli PM: {e}", "refreshing the PM table: {e}")
            })?;
        let mut cells = [0u64; PM_TABLE_CELLS];
        self.smu
            .call(b"ioctl_read_pm_table\0", &[], &mut cells)
            .map_err(|e| crate::trf!("odczyt tabeli PM: {e}", "reading the PM table: {e}"))?;

        // Każda komórka to dwie kolejne liczby 32-bitowe tabeli.
        let mut table = Vec::with_capacity(PM_TABLE_CELLS * 2);
        for cell in cells {
            table.push(f32::from_bits(cell as u32));
            table.push(f32::from_bits((cell >> 32) as u32));
        }
        Ok((version, table))
    }

    fn read_smn(&mut self, address: u32) -> Result<u32, String> {
        let smn = self.smn.as_ref().map_err(|e| e.clone())?;
        let _lock = PciLock::acquire()?;
        let mut output = [0u64; 1];
        smn.call(b"ioctl_read_smn\0", &[address as u64], &mut output)
            .map_err(|e| {
                crate::trf!(
                    "odczyt SMN 0x{address:08X}: {e}",
                    "reading SMN 0x{address:08X}: {e}"
                )
            })?;
        Ok(output[0] as u32)
    }
}
