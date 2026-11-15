//! Kodowanie i dekodowanie rejestrów procesora Intel używanych do strojenia.
//! Czyste funkcje bez dostępu do sprzętu - w całości pokryte testami.
//!
//! Źródła:
//! - limity mocy (MSR 0x606, 0x610): Intel SDM, rozdział o RAPL,
//! - skrzynka OC (MSR 0x150): Intel jej nie dokumentuje; układ pól i numery
//!   poleceń pochodzą z projektu PowerMonkey (Apache-2.0), który powołuje się
//!   na publikację DOI:10.1109/SP40000.2020.00057 i nagłówki Intel FSP.

pub const MSR_OC_MAILBOX: u64 = 0x150;
pub const MSR_TURBO_RATIO_LIMIT: u64 = 0x1AD;
pub const MSR_RAPL_POWER_UNIT: u64 = 0x606;
pub const MSR_PKG_POWER_LIMIT: u64 = 0x610;
pub const MSR_SECONDARY_TURBO_RATIO_LIMIT: u64 = 0x650;

// Polecenia skrzynki OC.
pub const OC_READ_VR_TOPOLOGY: u8 = 0x04;
pub const OC_READ_VF: u8 = 0x10;
pub const OC_WRITE_VF: u8 = 0x11;
pub const OC_READ_ICCMAX: u8 = 0x16;
pub const OC_WRITE_ICCMAX: u8 = 0x17;

// Domeny napięcia skrzynki OC.
pub const DOMAIN_P_CORE: u8 = 0x00;
pub const DOMAIN_RING: u8 = 0x02;
pub const DOMAIN_E_CORE: u8 = 0x05;

// ------------------------------------------------------------ limity mocy

/// Jednostki z MSR 0x606.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Units {
    /// Wat na jednostkę pola mocy.
    pub power_w: f64,
    /// Sekunda na jednostkę pola czasu.
    pub time_s: f64,
}

pub fn decode_units(msr: u64) -> Units {
    Units {
        power_w: 1.0 / (1u64 << (msr & 0xF)) as f64,
        time_s: 1.0 / (1u64 << ((msr >> 16) & 0xF)) as f64,
    }
}

/// Zawartość MSR 0x610 w jednostkach fizycznych.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PowerLimits {
    pub pl1_w: f64,
    pub pl1_enabled: bool,
    /// Turbo Boost Power Time Window (tau) w sekundach.
    pub tau_s: f64,
    pub pl2_w: f64,
    pub pl2_enabled: bool,
    /// Bit 63: rejestr zablokowany przez BIOS do następnego restartu.
    pub locked: bool,
}

fn decode_tau(field: u64, units: Units) -> f64 {
    let y = field & 0x1F;
    let z = (field >> 5) & 0x3;
    (1u64 << y) as f64 * (1.0 + z as f64 / 4.0) * units.time_s
}

/// Najbliższe możliwe do zapisania okno czasowe (7-bitowe pole: 2^Y * (1 + Z/4)).
fn encode_tau(tau_s: f64, units: Units) -> u64 {
    let mut best = 0u64;
    let mut best_err = f64::MAX;
    for field in 0..128u64 {
        let err = (decode_tau(field, units) - tau_s).abs();
        if err < best_err {
            best_err = err;
            best = field;
        }
    }
    best
}

pub fn decode_power_limits(msr: u64, units: Units) -> PowerLimits {
    PowerLimits {
        pl1_w: (msr & 0x7FFF) as f64 * units.power_w,
        pl1_enabled: msr & (1 << 15) != 0,
        tau_s: decode_tau((msr >> 17) & 0x7F, units),
        pl2_w: ((msr >> 32) & 0x7FFF) as f64 * units.power_w,
        pl2_enabled: msr & (1 << 47) != 0,
        locked: msr & (1 << 63) != 0,
    }
}

fn power_field(watts: f64, units: Units) -> u64 {
    ((watts / units.power_w).round().max(0.0) as u64).min(0x7FFF)
}

/// Nowa wartość MSR 0x610: podmienia PL1, PL2 i okno czasowe PL1, włącza oba
/// limity, a pozostałe bity (clamp, okno PL2, zarezerwowane) zostawia bez zmian.
pub fn encode_power_limits(old: u64, pl1_w: f64, pl2_w: f64, tau_s: f64, units: Units) -> u64 {
    let mut msr = old;
    msr &= !0x7FFF;
    msr |= power_field(pl1_w, units);
    msr |= 1 << 15;
    msr &= !(0x7F << 17);
    msr |= encode_tau(tau_s, units) << 17;
    msr &= !(0x7FFF << 32);
    msr |= power_field(pl2_w, units) << 32;
    msr |= 1 << 47;
    msr
}

// ------------------------------------------------------------ skrzynka OC

/// Słowo zapisywane do MSR 0x150: bit 63 = uruchom, bity 55:48 = parametr 2,
/// 47:40 = parametr 1, 39:32 = polecenie, 31:0 = dane.
pub fn mailbox_request(cmd: u8, param1: u8, param2: u8, data: u32) -> u64 {
    (1u64 << 63)
        | ((param2 as u64) << 48)
        | ((param1 as u64) << 40)
        | ((cmd as u64) << 32)
        | data as u64
}

/// Czy procesor wciąż wykonuje polecenie.
pub fn mailbox_busy(msr: u64) -> bool {
    msr & (1 << 63) != 0
}

/// Kod zakończenia (0 = sukces) i dane odpowiedzi.
pub fn mailbox_response(msr: u64) -> (u8, u32) {
    (((msr >> 32) & 0xFF) as u8, msr as u32)
}

/// Offset napięcia: 11 bitów ze znakiem w jednostkach 1/1024 V.
pub fn offset_mv_to_field(mv: i32) -> u32 {
    ((mv as f64 * 1.024).round() as i32 as u32) & 0x7FF
}

pub fn offset_field_to_mv(field: u32) -> i32 {
    let field = (field & 0x7FF) as i32;
    let signed = if field & 0x400 != 0 {
        field - 0x800
    } else {
        field
    };
    (signed as f64 / 1.024).round() as i32
}

/// Dane polecenia 0x10/0x11: [7:0] maks. mnożnik, [19:8] napięcie docelowe,
/// [20] tryb (0 = adaptacyjny, 1 = stały), [31:21] offset.
pub fn vf_offset_mv(data: u32) -> i32 {
    offset_field_to_mv(data >> 21)
}

pub fn vf_max_ratio(data: u32) -> u32 {
    data & 0xFF
}

pub fn vf_is_override(data: u32) -> bool {
    data & (1 << 20) != 0
}

/// Podmienia sam offset; mnożnik, napięcie docelowe i tryb zostają.
pub fn vf_with_offset(data: u32, mv: i32) -> u32 {
    (data & 0x001F_FFFF) | (offset_mv_to_field(mv) << 21)
}

/// Adres regulatora rdzeni (VccIA) z odpowiedzi na polecenie 0x04 (Alder Lake).
pub fn ia_vr_address(topology: u32) -> u8 {
    ((topology & 0xF00) >> 8) as u8
}

/// IccMax: 11 bitów w jednostkach 0,25 A (Alder Lake i nowsze).
pub const ICCMAX_MASK: u32 = 0x7FF;
pub const ICCMAX_UNLIMITED: u32 = 1 << 31;

pub fn iccmax_amps(data: u32) -> f32 {
    (data & ICCMAX_MASK) as f32 / 4.0
}

/// Wartość do zapisu; przy maksimum ustawia też bit "bez limitu".
pub fn iccmax_data(amps: f32) -> u32 {
    let raw = ((amps * 4.0).round().max(4.0) as u32).min(ICCMAX_MASK);
    if raw == ICCMAX_MASK {
        raw | ICCMAX_UNLIMITED
    } else {
        raw
    }
}

/// Limity mnożnika turbo z MSR 0x1AD / 0x650: osiem pól po 8 bitów.
pub fn turbo_ratios(msr: u64) -> [u8; 8] {
    msr.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Typowa wartość MSR 0x606: moc 1/8 W, energia 2^-14 J, czas 2^-10 s.
    const UNITS_MSR: u64 = 0x000A_0E03;

    #[test]
    fn units() {
        let u = decode_units(UNITS_MSR);
        assert_eq!(u.power_w, 0.125);
        assert_eq!(u.time_s, 1.0 / 1024.0);
    }

    #[test]
    fn power_limits_round_trip() {
        let u = decode_units(UNITS_MSR);
        // PL1 = 125 W (1000 jednostek), włączony + clamp, tau = 56 s (Y=14, Z=3),
        // PL2 = 253 W (2024 jednostki), włączony.
        let tau_field: u64 = 14 | (3 << 5);
        let msr: u64 =
            1000 | (1 << 15) | (1 << 16) | (tau_field << 17) | (2024u64 << 32) | (1 << 47);
        let d = decode_power_limits(msr, u);
        assert_eq!(d.pl1_w, 125.0);
        assert_eq!(d.pl2_w, 253.0);
        assert_eq!(d.tau_s, 28.0);
        assert!(d.pl1_enabled && d.pl2_enabled && !d.locked);

        let new = encode_power_limits(msr, 65.0, 180.0, 56.0, u);
        let d2 = decode_power_limits(new, u);
        assert_eq!(d2.pl1_w, 65.0);
        assert_eq!(d2.pl2_w, 180.0);
        assert_eq!(d2.tau_s, 56.0);
        // clamp PL1 (bit 16) zostaje nietknięty
        assert!(new & (1 << 16) != 0);
        // zapis tych samych wartości niczego nie zmienia
        assert_eq!(encode_power_limits(new, 65.0, 180.0, 56.0, u), new);
    }

    #[test]
    fn lock_bit() {
        let u = decode_units(UNITS_MSR);
        assert!(decode_power_limits(1 << 63, u).locked);
    }

    #[test]
    fn mailbox_words() {
        // Powszechnie znana postać zapisu offsetu -100 mV dla domeny 0.
        let data = vf_with_offset(0, -100);
        assert_eq!(data, 0xF340_0000);
        assert_eq!(
            mailbox_request(OC_WRITE_VF, DOMAIN_P_CORE, 0, data),
            0x8000_0011_F340_0000
        );
        // Odczyt domeny 2 (cache).
        assert_eq!(
            mailbox_request(OC_READ_VF, DOMAIN_RING, 0, 0),
            0x8000_0210_0000_0000
        );
        assert!(mailbox_busy(0x8000_0010_0000_0000));
        assert_eq!(mailbox_response(0x0000_0007_0000_1234), (7, 0x1234));
    }

    #[test]
    fn voltage_offsets() {
        for mv in [-250, -120, -100, -50, -5, 0, 5, 50] {
            let back = offset_field_to_mv(offset_mv_to_field(mv));
            assert!((back - mv).abs() <= 1, "{mv} -> {back}");
        }
        assert_eq!(vf_offset_mv(0xF340_0000), -100);
        // offset nie rusza mnożnika, napięcia docelowego ani trybu
        let before = 0x0012_3430u32 | (1 << 20);
        let after = vf_with_offset(before, -120);
        assert_eq!(after & 0x001F_FFFF, before & 0x001F_FFFF);
        assert_eq!(vf_offset_mv(after), -120);
        assert_eq!(vf_max_ratio(after), 0x30);
        assert!(vf_is_override(after));
    }

    #[test]
    fn iccmax() {
        assert_eq!(iccmax_amps(1023), 255.75);
        assert_eq!(iccmax_data(240.0), 960);
        assert_eq!(iccmax_data(0.0), 4);
        assert_eq!(iccmax_data(511.75), ICCMAX_MASK | ICCMAX_UNLIMITED);
        assert_eq!(ia_vr_address(0x0000_1300), 3);
    }

    #[test]
    fn ratios() {
        assert_eq!(
            turbo_ratios(0x2E2E_2E2E_2E2E_3030),
            [0x30, 0x30, 0x2E, 0x2E, 0x2E, 0x2E, 0x2E, 0x2E]
        );
    }
}
