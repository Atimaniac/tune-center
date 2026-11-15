//! Uruchamianie wraz z systemem przez Harmonogram zadań Windows.
//! Zwykły wpis w autostarcie nie wystarczy, bo program wymaga uprawnień
//! administratora; zadanie z "najwyższymi uprawnieniami" startuje bez pytania UAC.

const TASK: &str = "TuneCenter";
/// Zadania utworzone przez wcześniejsze nazwy programu.
const OLD_TASKS: &[&str] = &["PCTune", "FanCtl"];

#[cfg(windows)]
fn schtasks(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("schtasks")
        .args(args)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn schtasks(_args: &[&str]) -> bool {
    false
}

pub fn is_enabled() -> bool {
    schtasks(&["/Query", "/TN", TASK])
}

/// Usuwa zadania autostartu spod starych nazw programu. Jeśli autostart był
/// włączony, tworzy go ponownie pod nową nazwą i z aktualną ścieżką.
pub fn migrate() {
    let mut had_old = false;
    for &task in OLD_TASKS {
        if schtasks(&["/Query", "/TN", task]) {
            had_old = true;
            schtasks(&["/Delete", "/TN", task, "/F"]);
        }
    }
    if had_old && !is_enabled() {
        let _ = set(true);
    }
}

pub fn set(enabled: bool) -> Result<(), String> {
    let ok = if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" --minimized", exe.display());
        schtasks(&[
            "/Create", "/TN", TASK, "/TR", &command, "/SC", "ONLOGON", "/RL", "HIGHEST", "/F",
        ])
    } else {
        schtasks(&["/Delete", "/TN", TASK, "/F"])
    };
    if ok {
        Ok(())
    } else {
        Err(crate::i18n::tr(
            "Nie udało się zmienić autostartu (wymagane uprawnienia administratora).",
            "Could not change autostart (administrator rights required).",
        )
        .into())
    }
}
