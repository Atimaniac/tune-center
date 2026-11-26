//! Raport awarii programu.
//!
//! - Panika (błąd w kodzie Rust): zapis do `tunecenter-crash.log` obok
//!   programu i okno z opisem błędu.
//! - Awaria natywna (np. naruszenie dostępu w bibliotece sterownika NVIDIA
//!   `nvml.dll` przy przeładowaniu sterownika): filtr nieobsłużonych wyjątków
//!   Windows zapisuje kod wyjątku, adres i moduł, pokazuje okno i kończy proces.
//! - Przy następnym starcie program pokazuje ostatni raport w swoim oknie
//!   (gdyby okno awarii się nie pokazało, np. przy autostarcie w zasobniku),
//!   po czym zmienia nazwę pliku na `tunecenter-crash-previous.log`.

use std::path::PathBuf;

const FILE: &str = "tunecenter-crash.log";
const PREVIOUS: &str = "tunecenter-crash-previous.log";

fn dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

fn header() -> String {
    format!(
        "Tune Center {} - {} ({})\n",
        env!("CARGO_PKG_VERSION"),
        crate::bench::now_text(),
        std::env::consts::ARCH
    )
}

/// Dopisuje raport do pliku (ostatnia awaria na końcu).
fn write(report: &str) {
    if let Some(path) = dir().map(|d| d.join(FILE)) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{report}\n----------------------------------------");
        }
    }
}

/// Okno systemowe z opisem błędu (działa też, gdy okno programu już nie żyje).
#[cfg(windows)]
fn message_box(title: &str, text: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    const MB_ICONERROR: u32 = 0x10;
    const MB_TOPMOST: u32 = 0x40000;
    const MB_SETFOREGROUND: u32 = 0x10000;
    // SAFETY: wskaźniki na zakończone zerem bufory UTF-16 żyjące do końca wywołania.
    unsafe {
        MessageBoxW(
            0,
            wide(text).as_ptr(),
            wide(title).as_ptr(),
            MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
fn message_box(_: &str, _: &str) {}

fn show(report: &str, closing: bool) {
    let title = crate::i18n::tr("Tune Center - błąd programu", "Tune Center - program error");
    let tail = if closing {
        crate::i18n::tr(
            "Program zostanie zamknięty. Raport zapisano w pliku tunecenter-crash.log obok programu - dołącz go do zgłoszenia na GitHubie.",
            "The program will close. The report was saved to tunecenter-crash.log next to the program - attach it to a GitHub issue.",
        )
    } else {
        crate::i18n::tr(
            "Program działa dalej, ale ta funkcja mogła przestać działać. Raport zapisano w pliku tunecenter-crash.log obok programu.",
            "The program keeps running, but this feature may have stopped. The report was saved to tunecenter-crash.log next to the program.",
        )
    };
    message_box(title, &format!("{report}\n\n{tail}"));
}

/// Instaluje obsługę paniki i awarii natywnych. Wołane na początku `main`.
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        // Najpierw oddajemy wentylatory płycie - dalej może już nie być okazji.
        crate::hw::service::emergency_restore();
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("?").to_owned();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "?".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        // Bez symboli (wersja wydaniowa bez .pdb) ramki to "<unknown>" - pomijamy je.
        let backtrace: String = std::backtrace::Backtrace::force_capture()
            .to_string()
            .lines()
            .filter(|l| !l.contains("<unknown>"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        let report = format!(
            "{}Panika w wątku \"{name}\": {message}\nMiejsce: {location}\n\n{backtrace}",
            header()
        );
        write(&report);
        // Panika w głównym wątku kończy program; w innym - tylko ten wątek.
        let main = name == "main";
        show(
            &crate::trf!(
                "{}\n({}, wątek \"{}\")",
                "{}\n({}, thread \"{}\")",
                message,
                location,
                name
            ),
            main,
        );
    }));
    #[cfg(windows)]
    native::install();
}

#[cfg(windows)]
mod native {
    use std::ffi::c_void;

    #[repr(C)]
    struct ExceptionRecord {
        code: u32,
        flags: u32,
        next: *mut ExceptionRecord,
        address: *mut c_void,
        params: u32,
        info: [usize; 15],
    }

    #[repr(C)]
    struct ExceptionPointers {
        record: *const ExceptionRecord,
        context: *mut c_void,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn SetUnhandledExceptionFilter(
            filter: Option<unsafe extern "system" fn(*const ExceptionPointers) -> i32>,
        ) -> *mut c_void;
        fn GetModuleHandleExW(flags: u32, address: *const c_void, module: *mut isize) -> i32;
        fn GetModuleFileNameW(module: isize, name: *mut u16, size: u32) -> u32;
    }

    pub fn install() {
        // SAFETY: rejestracja funkcji wołanej przez system przy nieobsłużonym wyjątku.
        unsafe {
            SetUnhandledExceptionFilter(Some(filter));
        }
    }

    /// Moduł (DLL/EXE), w którym leży adres.
    unsafe fn module_of(address: *const c_void) -> String {
        const FROM_ADDRESS: u32 = 0x4;
        const UNCHANGED_REFCOUNT: u32 = 0x2;
        let mut module = 0isize;
        if GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, address, &mut module) == 0 {
            return "?".into();
        }
        let mut buf = [0u16; 512];
        let n = GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
        let path = String::from_utf16_lossy(&buf[..n]);
        let offset = address as usize - module as usize;
        format!("{path} + 0x{offset:X}")
    }

    use super::describe;

    unsafe extern "system" fn filter(info: *const ExceptionPointers) -> i32 {
        crate::hw::service::emergency_restore();
        let (code, address) = match info.as_ref().and_then(|i| i.record.as_ref()) {
            Some(r) => (r.code, r.address as *const c_void),
            None => (0, std::ptr::null()),
        };
        let module = module_of(address);
        let hint = if module.to_lowercase().contains("nvml")
            || module.to_lowercase().contains("nvapi")
        {
            crate::i18n::tr(
                "\nBłąd w bibliotece sterownika NVIDIA - zwykle przy instalacji lub przeładowaniu sterownika karty.",
                "\nError in the NVIDIA driver library - usually while the graphics driver is being installed or reloaded.",
            )
        } else {
            ""
        };
        let report = format!(
            "{}Awaria: kod 0x{code:08X} - {}\nAdres: {module}{hint}",
            super::header(),
            describe(code)
        );
        super::write(&report);
        super::show(
            &crate::trf!(
                "Kod 0x{:08X} - {}\n{}{}",
                "Code 0x{:08X} - {}\n{}{}",
                code,
                describe(code),
                module,
                hint
            ),
            true,
        );
        1 // EXCEPTION_EXECUTE_HANDLER: proces się kończy
    }
}

/// Opis kodu wyjątku / kodu wyjścia procesu Windows.
pub fn describe(code: u32) -> &'static str {
    match code {
        0xC000_0005 => crate::i18n::tr(
            "naruszenie dostępu do pamięci (access violation)",
            "access violation",
        ),
        0xC000_00FD => crate::i18n::tr("przepełnienie stosu (stack overflow)", "stack overflow"),
        0xC000_001D => crate::i18n::tr("niedozwolona instrukcja", "illegal instruction"),
        0xC000_0094 => crate::i18n::tr("dzielenie przez zero", "integer division by zero"),
        0xC000_0409 => crate::i18n::tr(
            "przekroczenie bufora / przerwanie programu (fail fast)",
            "buffer overrun / fail fast",
        ),
        0xC000_0374 => crate::i18n::tr("uszkodzenie sterty (heap corruption)", "heap corruption"),
        0xE043_4352 => crate::i18n::tr("nieobsłużony wyjątek .NET", "unhandled .NET exception"),
        0x8000_4005 => crate::i18n::tr(
            "nieobsłużony wyjątek .NET (E_FAIL)",
            "unhandled .NET exception (E_FAIL)",
        ),
        _ => crate::i18n::tr("wyjątek / błąd procesu", "exception / process error"),
    }
}

/// Raport z poprzedniego uruchomienia (do pokazania w oknie programu).
pub fn previous() -> Option<String> {
    let path = dir()?.join(FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    // Ostatni raport z pliku.
    let last = text
        .split("----------------------------------------")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .last()?
        .to_owned();
    Some(last)
}

/// Po pokazaniu raport trafia do pliku z poprzednimi awariami.
pub fn acknowledge() {
    if let Some(d) = dir() {
        let (from, to) = (d.join(FILE), d.join(PREVIOUS));
        if let Ok(text) = std::fs::read_to_string(&from) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&to)
            {
                let _ = f.write_all(text.as_bytes());
            }
        }
        let _ = std::fs::remove_file(from);
    }
}

/// Folder programu (przycisk „Otwórz folder”).
pub fn folder() -> Option<PathBuf> {
    dir()
}
