//! Uprawnienia administratora: sprawdzenie i ponowne uruchomienie programu
//! z podniesionymi uprawnieniami (pytanie UAC).
//!
//! Program działa bez administratora: karta graficzna (NVAPI/NVML), panel
//! Nvidia, kompilacja shaderów, dyski i pamięć działają tak samo. Administrator
//! jest potrzebny do: zakładki CPU (PawnIO), temperatur i mocy procesora oraz
//! czujników i wentylatorów płyty głównej (PawnIO), zmiany HAGS i autostartu.

/// Czy proces działa z uprawnieniami administratora.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    // SAFETY: funkcja bez argumentów.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    true
}

/// Uruchamia program ponownie jako administrator (z tymi samymi parametrami,
/// bez `--minimized`). `Ok` = nowy proces wystartował i ten można zamknąć;
/// odmowa w oknie UAC zwraca błąd.
#[cfg(windows)]
pub fn relaunch_as_admin() -> Result<(), String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--minimized" && a != "--restart")
        .chain(std::iter::once("--restart".to_owned()))
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a
            }
        })
        .collect();
    let (verb, file, params) = (
        HSTRING::from("runas"),
        HSTRING::from(exe.as_os_str()),
        HSTRING::from(args.join(" ")),
    );
    // SAFETY: wskaźniki na łańcuchy żyjące do końca wywołania.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Wartość > 32 oznacza sukces (dokumentacja ShellExecute).
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(crate::i18n::tr(
            "nie uruchomiono jako administrator (odmowa w oknie UAC?)",
            "not started as administrator (declined in the UAC prompt?)",
        )
        .to_string())
    }
}

#[cfg(not(windows))]
pub fn relaunch_as_admin() -> Result<(), String> {
    Err("Windows only".into())
}
