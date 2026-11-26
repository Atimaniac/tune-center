//! Strefy termiczne ACPI z licznika wydajności Windows ("Thermal Zone
//! Information", bez sterownika i bez uprawnień administratora). Zastępują
//! temperaturę płyty, gdy jej układu Super I/O nie da się odczytać.
//!
//! Wiele płyt podaje tu stałą atrapę z BIOS-u (np. 301 K = 27,9 °C), która nie
//! jest pomiarem - strefa pokazuje się dopiero, gdy jej wartość zmieni się o
//! co najmniej 0,5 °C. Odczyt co 10 s.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

use super::Readings;

const PDH_MORE_DATA: u32 = 0x8000_07D2;

#[derive(Default)]
pub struct Acpi {
    last: Option<Instant>,
    zones: Vec<(String, f32)>,
    /// Pierwsza wartość strefy i czy już się zmieniła (żywy czujnik).
    seen: HashMap<String, (f32, bool)>,
}

impl Acpi {
    /// Dopisuje żywe strefy jako temperatury płyty (`kind` "Motherboard").
    pub fn read(&mut self, out: &mut Readings) {
        if self.last.is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
            self.last = Some(Instant::now());
            let mut list = Vec::new();
            for (name, celsius) in zones() {
                if !(1.0..110.0).contains(&celsius) {
                    continue;
                }
                let s = self.seen.entry(name.clone()).or_insert((celsius, false));
                if (celsius - s.0).abs() >= 0.5 {
                    s.1 = true;
                }
                if s.1 {
                    list.push((name, celsius));
                }
            }
            self.zones = list;
        }
        for (name, c) in &self.zones {
            out.temp(format!("/acpi/{name}"), &format!("ACPI {name}"), "ACPI", "Motherboard", Some(*c));
        }
    }
}

/// Wszystkie strefy: (nazwa, °C), nazwa bez przedrostka ("\_TZ.TZ00" -> "TZ00").
fn zones() -> Vec<(String, f32)> {
    let mut out = Vec::new();
    unsafe {
        let mut query = PDH_HQUERY::default();
        if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
            return out;
        }
        let mut counter = PDH_HCOUNTER::default();
        if PdhAddEnglishCounterW(query, w!(r"\Thermal Zone Information(*)\Temperature"), 0, &mut counter) == 0
            && PdhCollectQueryData(query) == 0
        {
            let (mut size, mut count) = (0u32, 0u32);
            if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None) == PDH_MORE_DATA
                && size > 0
            {
                // Bufor w elementach (wyrównanie), z miejscem na nazwy za nimi.
                let item = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
                let mut buf = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (size as usize).div_ceil(item)];
                if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(buf.as_mut_ptr())) == 0 {
                    for it in buf.iter().take(count as usize) {
                        if it.FmtValue.CStatus != 0 {
                            continue;
                        }
                        let kelvin = it.FmtValue.Anonymous.doubleValue;
                        let celsius = ((kelvin - 273.15) * 10.0).round() as f32 / 10.0;
                        let name = it.szName.to_string().unwrap_or_default();
                        let name = name.rsplit('.').next().unwrap_or("TZ").to_owned();
                        out.push((name, celsius));
                    }
                }
            }
        }
        let _ = PdhCloseQuery(query);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn acpi_zones_live() {
        // Na komputerze bez stref lista jest pusta - test tylko pokazuje wynik.
        let z = super::zones();
        eprintln!("ACPI zones: {z:?}");
        assert!(z.iter().all(|(n, _)| !n.is_empty()));
    }
}
