//! Czujniki i wentylatory bez procesu .NET: sterownik PawnIO, jego podpisane
//! moduły i własny kod. Zastępuje proces `sensor-bridge` (LibreHardwareMonitor).
//!
//! Identyfikatory i nazwy czujników są takie same jak w LibreHardwareMonitor
//! (np. `/lpc/nct6796dr/0/control/0`, "CPU Package"), żeby zapisane krzywe
//! wentylatorów i wybrane czujniki działały dalej bez zmian.

pub mod acpi;
pub mod battery;
pub mod boards;
pub mod cpu;
pub mod fintek;
pub mod ite;
pub mod memory;
pub mod nuvoton;
pub mod service;
pub mod superio;

use std::path::PathBuf;

use crate::bridge::{Fan, Power, Speed, Temp};

/// Odczyty jednego cyklu w formacie zakładek programu.
#[derive(Default, Clone, Debug)]
pub struct Readings {
    pub temps: Vec<Temp>,
    /// Wentylatory, którymi można sterować.
    pub fans: Vec<Fan>,
    /// Wszystkie czujniki obrotów (także bez sterowania).
    pub speeds: Vec<Speed>,
    pub powers: Vec<Power>,
}

impl Readings {
    pub fn temp(&mut self, id: String, name: &str, hw: &str, kind: &str, value: Option<f32>) {
        self.temps.push(Temp { id, name: name.into(), hw: hw.into(), kind: kind.into(), value });
    }

    pub fn power(&mut self, name: &str, hw: &str, kind: &str, value: Option<f32>) {
        self.powers.push(Power { name: name.into(), hw: hw.into(), kind: kind.into(), value });
    }
}

/// Moduł PawnIO w folderze programu (`modules\<name>`).
pub fn module_path(name: &str) -> Option<PathBuf> {
    // Testy uruchamiane spoza folderu programu wskazują moduły zmienną.
    #[cfg(test)]
    if let Some(dir) = std::env::var_os("TC_MODULES") {
        return Some(PathBuf::from(dir).join(name));
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("modules").join(name))
}

/// Wartość poza zakresem czujnika temperatury (brak czujnika, zwarcie) - pomijana.
pub fn valid_temp(c: f32) -> Option<f32> {
    (c.is_finite() && c > -40.0 && c < 150.0).then_some(c)
}
