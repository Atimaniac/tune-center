//! Krótkie nazwy modeli karty graficznej i procesora na przyciskach zakładek
//! (np. "RTX 5070", "9070 XT", "i7-12700KF", "Ryzen 7 9800X3D").

use std::sync::OnceLock;

/// Słowa pomijane w nazwie: producent, marka i dopiski.
fn is_noise(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "nvidia" | "geforce" | "amd" | "radeon" | "rx" | "intel" | "core" | "gpu" | "graphics"
            | "processor" | "cpu" | "with"
    )
}

/// Usuwa znaki ®, ™ i dopiski "(R)", "(TM)".
fn clean(name: &str) -> String {
    name.replace("(R)", " ")
        .replace("(r)", " ")
        .replace("(TM)", " ")
        .replace("(tm)", " ")
        .replace(['®', '™'], " ")
}

/// Model karty: "NVIDIA GeForce RTX 5070" -> "RTX 5070",
/// "AMD Radeon RX 9070 XT" -> "9070 XT". Pusty, gdy nazwa nie ma modelu
/// (np. "AMD Radeon(TM) Graphics" - układ zintegrowany).
pub fn gpu_model(name: &str) -> String {
    clean(name)
        .split_whitespace()
        .filter(|w| !is_noise(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Model procesora: "12th Gen Intel(R) Core(TM) i7-12700KF" -> "i7-12700KF",
/// "Intel(R) Core(TM) Ultra 7 265K" -> "Ultra 7 265K",
/// "AMD Ryzen 7 9800X3D 8-Core Processor" -> "Ryzen 7 9800X3D".
pub fn cpu_model(name: &str) -> String {
    let text = clean(name);
    // Bez częstotliwości: "... CPU @ 3.60GHz".
    let text = text.split('@').next().unwrap_or("");
    let words: Vec<&str> = text.split_whitespace().collect();
    // Intel Core i3/i5/i7/i9: sam symbol, np. "i7-12700KF".
    if let Some(w) = words.iter().find(|w| {
        let b = w.as_bytes();
        b.len() > 3 && b[0] == b'i' && b[1].is_ascii_digit() && b[2] == b'-'
    }) {
        return (*w).to_owned();
    }
    words
        .iter()
        .enumerate()
        .filter(|(i, w)| {
            let lower = w.to_ascii_lowercase();
            // "12th Gen", "8-Core", "16-Core".
            let generation = lower.ends_with("th") && lower[..lower.len() - 2].chars().all(|c| c.is_ascii_digit())
                || lower == "gen" && *i > 0;
            !(is_noise(w) || generation || lower.ends_with("-core"))
        })
        .map(|(_, w)| *w)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Napis zakładki karty: model wybranej karty NVIDIA albo AMD (ta sama karta,
/// co w Shader compilation). `None` - nie ma karty NVIDIA ani AMD albo jej
/// nazwa nie ma modelu.
pub fn gpu_label() -> Option<&'static str> {
    static LABEL: OnceLock<Option<String>> = OnceLock::new();
    LABEL
        .get_or_init(|| {
            #[cfg(windows)]
            {
                let (_, _, name) = crate::shaders::pick_adapter().ok()?;
                Some(gpu_model(&name)).filter(|m| !m.is_empty())
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
        .as_deref()
}

/// Napis zakładki procesora: model procesora.
pub fn cpu_label() -> Option<&'static str> {
    static LABEL: OnceLock<Option<String>> = OnceLock::new();
    LABEL
        .get_or_init(|| Some(cpu_model(&crate::cpu::cpu_info().name)).filter(|m| !m.is_empty()))
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_names() {
        assert_eq!(gpu_model("NVIDIA GeForce RTX 5070"), "RTX 5070");
        assert_eq!(gpu_model("NVIDIA GeForce RTX 5070 Ti"), "RTX 5070 Ti");
        assert_eq!(gpu_model("NVIDIA GeForce GTX 1080"), "GTX 1080");
        assert_eq!(gpu_model("NVIDIA GeForce RTX 4060 Laptop GPU"), "RTX 4060 Laptop");
        assert_eq!(gpu_model("AMD Radeon RX 9070 XT"), "9070 XT");
        assert_eq!(gpu_model("AMD Radeon RX 7900 XTX"), "7900 XTX");
        assert_eq!(gpu_model("AMD Radeon(TM) Graphics"), "");
    }

    #[test]
    fn cpu_names() {
        assert_eq!(cpu_model("12th Gen Intel(R) Core(TM) i7-12700KF"), "i7-12700KF");
        assert_eq!(cpu_model("Intel(R) Core(TM) i9-9900K CPU @ 3.60GHz"), "i9-9900K");
        assert_eq!(cpu_model("Intel(R) Core(TM) Ultra 7 265K"), "Ultra 7 265K");
        assert_eq!(cpu_model("AMD Ryzen 7 9800X3D 8-Core Processor"), "Ryzen 7 9800X3D");
        assert_eq!(
            cpu_model("AMD Ryzen 9 9950X3D 16-Core Processor            "),
            "Ryzen 9 9950X3D"
        );
        assert_eq!(cpu_model("AMD Ryzen 5 5600G with Radeon Graphics"), "Ryzen 5 5600G");
        assert_eq!(cpu_model("Intel(R) Xeon(R) W-2295 CPU @ 3.00GHz"), "Xeon W-2295");
    }
}
