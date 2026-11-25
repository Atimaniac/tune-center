//! Strojenie procesora (zakładka "Procesor"): Intel albo AMD Ryzen,
//! zależnie od tego, co wykryje CPUID.
//!
//! - `codec`  - kodowanie rejestrów Intela (czyste funkcje),
//! - `intel`  - operacje odczyt-zmiana-zapis-weryfikacja na dowolnym źródle MSR,
//! - `amd`    - Curve Optimizer i limity PBO na dowolnym źródle poleceń SMU,
//! - `pawnio` - dostęp do sprzętu przez sterownik PawnIO.

pub mod amd;
pub mod codec;
pub mod intel;
pub mod monitor;
pub mod pawnio;

/// Podstawowe informacje o procesorze z CPUID.
#[derive(Clone, Debug, Default)]
pub struct CpuInfo {
    pub name: String,
    pub intel: bool,
    pub amd: bool,
    /// Liczba rdzeni fizycznych (wypełniana dla AMD).
    pub physical_cores: usize,
    /// Procesor hybrydowy z rdzeniami P i E (Alder Lake, Raptor Lake).
    pub hybrid: bool,
    pub family: u32,
    pub model: u32,
}

/// Modele (rodzina 6) z rdzeniami P i E, dla których znamy układ skrzynki OC:
/// Alder Lake-S 0x97, Alder Lake-H/P 0x9A, Raptor Lake-S 0xB7 i 0xBF,
/// Raptor Lake-H/P 0xBA. Raptor Lake korzysta z tej samej platformy co
/// Alder Lake; dla niego układ nie był osobno potwierdzony.
const HYBRID_MODELS: [u32; 5] = [0x97, 0x9A, 0xB7, 0xBA, 0xBF];

#[cfg(target_arch = "x86_64")]
#[allow(unused_unsafe)]
pub fn cpu_info() -> CpuInfo {
    use std::arch::x86_64::__cpuid;

    let leaf0 = unsafe { __cpuid(0) };
    let mut vendor = Vec::new();
    for reg in [leaf0.ebx, leaf0.edx, leaf0.ecx] {
        vendor.extend_from_slice(&reg.to_le_bytes());
    }
    let intel = vendor == b"GenuineIntel";
    let amd = vendor == b"AuthenticAMD";
    let max_extended = unsafe { __cpuid(0x8000_0000) }.eax;

    let eax = unsafe { __cpuid(1) }.eax;
    let base_family = (eax >> 8) & 0xF;
    let family = if base_family == 0xF {
        base_family + ((eax >> 20) & 0xFF)
    } else {
        base_family
    };
    let model = if base_family == 0x6 || base_family == 0xF {
        ((eax >> 4) & 0xF) | ((eax >> 12) & 0xF0)
    } else {
        (eax >> 4) & 0xF
    };

    let mut name = String::new();
    if max_extended >= 0x8000_0004 {
        let mut bytes = Vec::new();
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            let r = unsafe { __cpuid(leaf) };
            for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                bytes.extend_from_slice(&reg.to_le_bytes());
            }
        }
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        name = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    }
    if is_engineering_name(&name) {
        let stepping = eax & 0xF;
        let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
        if let Some(real) = windows_name()
            .filter(|n| !is_engineering_name(n))
            .or_else(|| engineering_model(family, model, stepping, threads).map(str::to_owned))
        {
            name = real;
        }
    }

    // AMD: liczba wątków (0x80000008) podzielona przez wątki na rdzeń (0x8000001E).
    let mut physical_cores = 0;
    if amd && max_extended >= 0x8000_001E {
        let threads = (unsafe { __cpuid(0x8000_0008) }.ecx & 0xFF) as usize + 1;
        let per_core = ((unsafe { __cpuid(0x8000_001E) }.ebx >> 8) & 0xFF) as usize + 1;
        physical_cores = threads / per_core;
    }

    CpuInfo {
        name,
        intel,
        amd,
        physical_cores,
        hybrid: intel && family == 6 && HYBRID_MODELS.contains(&model),
        family,
        model,
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn cpu_info() -> CpuInfo {
    CpuInfo::default()
}

/// Nazwa próbki inżynierskiej Intela bez modelu, np. "Genuine Intel(R) 0000"
/// z CPUID albo "Genuine Intel 0000" z czujników (LibreHardwareMonitor).
pub fn is_engineering_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("genuine") && lower.contains("0000")
}

/// Nazwa procesora z Windows (`ProcessorNameString`).
fn windows_name() -> Option<String> {
    #[cfg(windows)]
    {
        crate::shaders::registry::Key::open(
            crate::shaders::registry::HKLM,
            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        )?
        .string("ProcessorNameString")
        .map(|n| n.trim().to_owned())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Znane próbki inżynierskie: rodzina, model, stepping i liczba wątków ->
/// właściwa nazwa (gdy Windows też podaje tylko "0000").
fn engineering_model(family: u32, model: u32, stepping: u32, threads: usize) -> Option<&'static str> {
    match (family, model, stepping, threads) {
        // Raptor Lake-HX, 8 rdzeni P + 12 E (28 wątków), CPUID B0671.
        (6, 0xB7, 1, 28) => Some("Intel(R) Core(TM) i7-13850HX"),
        _ => None,
    }
}

/// Nazwa procesora z CPUID (z poprawką dla próbek inżynierskich), raz na
/// uruchomienie programu.
pub fn name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| cpu_info().name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engineering_sample_names() {
        assert!(is_engineering_name("Genuine Intel(R) 0000"));
        assert!(is_engineering_name("Genuine Intel 0000"));
        assert!(!is_engineering_name("Intel(R) Core(TM) i7-13850HX"));
        assert_eq!(engineering_model(6, 0xB7, 1, 28), Some("Intel(R) Core(TM) i7-13850HX"));
        assert_eq!(engineering_model(6, 0xB7, 1, 32), None);
        assert_eq!(
            crate::hw_names::cpu_model(engineering_model(6, 0xB7, 1, 28).unwrap()),
            "i7-13850HX"
        );
        assert_eq!(crate::hw_names::cpu_model("Intel® Core™ i7-13850HX"), "i7-13850HX");
    }
}
