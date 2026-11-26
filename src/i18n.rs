//! Język interfejsu: polski (domyślny) albo angielski.
//!
//! Każdy napis widoczny dla użytkownika jest zapisany w kodzie w obu
//! językach: `tr("polski", "english")` dla zwykłego tekstu i
//! `trf!("polski {x}", "english {x}", ...)` dla tekstu z wstawianymi
//! wartościami. Wybór języka jest globalny i można go zmienić w trakcie pracy
//! (zakładka OPTIONS); napisy powstałe wcześniej, np. komunikat w pasku
//! stanu, zostają w języku, w którym powstały.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn set_english(english: bool) {
    ENGLISH.store(english, Ordering::Relaxed);
}

/// Kod języka zapisywany w ustawieniach: "pl" albo "en".
pub fn code() -> &'static str {
    if english() {
        "en"
    } else {
        "pl"
    }
}

/// Napis w bieżącym języku.
pub fn tr(pl: &'static str, en: &'static str) -> &'static str {
    if english() {
        en
    } else {
        pl
    }
}

/// Tłumaczenie nazw, które w kodzie służą też za stałe klucze (tytuły
/// podzespołów w licznikach mocy i w czujnikach) i dlatego nie mogą zależeć
/// od języka. Nieznany napis wraca bez zmian.
pub fn tr_dyn(pl: &'static str) -> &'static str {
    if !english() {
        return pl;
    }
    match pl {
        "Procesor" => "Processor",
        "Karta graficzna" => "Graphics card",
        "Zasilacz" => "Power supply",
        "Bateria" => "Battery",
        "Płyta główna" => "Motherboard",
        "Dysk" => "Drive",
        "Pamięć RAM" => "RAM",
        other => other,
    }
}

/// `format!` w bieżącym języku. Oba teksty muszą używać tych samych
/// argumentów w tej samej kolejności.
#[macro_export]
macro_rules! trf {
    ($pl:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::i18n::english() {
            format!($en $(, $($arg)*)?)
        } else {
            format!($pl $(, $($arg)*)?)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_language_for_plain_and_formatted_text() {
        set_english(false);
        let n = 3;
        assert_eq!(tr("Zapisz", "Save"), "Zapisz");
        assert_eq!(
            crate::trf!("Profil {} z {n}", "Profile {} of {n}", 2),
            "Profil 2 z 3"
        );
        assert_eq!(tr_dyn("Dysk"), "Dysk");
        set_english(true);
        assert_eq!(tr("Zapisz", "Save"), "Save");
        assert_eq!(
            crate::trf!("Profil {} z {n}", "Profile {} of {n}", 2),
            "Profile 2 of 3"
        );
        assert_eq!(
            crate::trf!("bez argumentów", "no arguments"),
            "no arguments"
        );
        assert_eq!(tr_dyn("Dysk"), "Drive");
        assert_eq!(tr_dyn("Fan #1"), "Fan #1");
        assert_eq!(code(), "en");
        set_english(false);
        assert_eq!(code(), "pl");
    }
}
