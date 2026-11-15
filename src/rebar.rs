//! Wykrywanie Resizable BAR po rozmiarze okna pamięci karty graficznej.
//!
//! Bez Resizable BAR karta dostaje na magistrali PCI okno pamięci 256 MB;
//! z włączonym - okno wielkości zbliżonej do pamięci karty. Rozmiar okna
//! odczytuje jednorazowo PowerShell (WMI), w osobnym wątku. Niepowodzenie
//! niczego nie psuje: stan zostaje "nieznany".

use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Checking,
    /// Największe okno pamięci karty graficznej w MB.
    Known(u64),
    Unknown,
}

pub struct Probe {
    state: Arc<Mutex<State>>,
}

/// Skrypt wypisuje jedną liczbę: największy zakres pamięci (MB) przydzielony
/// kartom graficznym na magistrali PCI.
const SCRIPT: &str = r#"$ErrorActionPreference='Stop';$m=[uint64]0;Get-CimInstance Win32_VideoController|Where-Object{$_.PNPDeviceID -like 'PCI\*'}|ForEach-Object{$f="DeviceID='"+$_.PNPDeviceID.Replace('\','\\')+"'";$d=Get-CimInstance Win32_PnPEntity -Filter $f;Get-CimAssociatedInstance -InputObject $d -ResultClassName Win32_DeviceMemoryAddress}|ForEach-Object{$s=[uint64]$_.EndingAddress-[uint64]$_.StartingAddress+1;if($s -gt $m){$m=$s}};[uint64][math]::Floor($m/1MB)"#;

impl Probe {
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(State::Checking));
        let shared = state.clone();
        std::thread::spawn(move || {
            let result = run()
                .and_then(|out| parse(&out))
                .map_or(State::Unknown, State::Known);
            *shared.lock().unwrap() = result;
        });
        Self { state }
    }

    pub fn state(&self) -> State {
        *self.state.lock().unwrap()
    }
}

/// Ostatnia niepusta linia wyjścia jako liczba MB; zero oznacza brak danych.
pub fn parse(output: &str) -> Option<u64> {
    let line = output.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    line.parse::<u64>().ok().filter(|mb| *mb > 0)
}

#[cfg(windows)]
fn run() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(windows))]
fn run() -> Option<String> {
    let _ = SCRIPT;
    None
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_output() {
        assert_eq!(parse("16384\r\n"), Some(16384));
        assert_eq!(parse("\n256\n\n"), Some(256));
        assert_eq!(parse("0\n"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("Get-CimInstance : error"), None);
    }
}
