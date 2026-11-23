//! Strojenie procesora Intel: limity mocy, offsety napięcia i IccMax.
//!
//! Logika jest niezależna od sterownika - działa na dowolnym [`Msr`], więc
//! kolejność odczytów i zapisów jest sprawdzana testami na symulowanym
//! procesorze. Każdy zapis ma tę samą postać: odczyt bieżącej wartości,
//! podmiana jednego pola, zapis, ponowny odczyt i porównanie.

use super::codec::*;

/// Dostęp do rejestrów MSR procesora.
pub trait Msr {
    fn read(&mut self, msr: u64) -> Result<u64, String>;
    fn write(&mut self, msr: u64, value: u64) -> Result<(), String>;
}

/// Dozwolony zakres offsetu napięcia. Dodatnie offsety podnoszą napięcie,
/// więc są mocno ograniczone; ujemne poniżej -250 mV i tak nie są stabilne.
pub const OFFSET_MIN_MV: i32 = -250;
pub const OFFSET_MAX_MV: i32 = 50;

pub const POWER_MIN_W: f64 = 1.0;
pub const POWER_MAX_W: f64 = 4095.0;
pub const TAU_MIN_S: f64 = 0.001;
pub const TAU_MAX_S: f64 = 448.0;
pub const ICCMAX_MIN_A: f32 = 1.0;
pub const ICCMAX_MAX_A: f32 = 511.75;

/// Ile razy pytamy procesor, czy skończył wykonywać polecenie skrzynki OC.
const MAILBOX_SPINS: usize = 1000;

/// Domena napięcia skrzynki OC.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Domain {
    pub id: u8,
    /// Nazwa po polsku i po angielsku; do wyświetlania służy [`Domain::label`].
    pub name: &'static str,
    pub name_en: &'static str,
    /// Istnieje tylko w procesorach hybrydowych (Alder Lake i nowsze).
    pub hybrid_only: bool,
}

pub const DOMAINS: [Domain; 3] = [
    Domain {
        id: DOMAIN_P_CORE,
        name: "Rdzenie P",
        name_en: "P-cores",
        hybrid_only: false,
    },
    Domain {
        id: DOMAIN_E_CORE,
        name: "Rdzenie E",
        name_en: "E-cores",
        hybrid_only: true,
    },
    Domain {
        id: DOMAIN_RING,
        name: "Cache (Ring)",
        name_en: "Cache (Ring)",
        hybrid_only: false,
    },
];

impl Domain {
    /// Nazwa domeny w bieżącym języku.
    pub fn label(&self) -> &'static str {
        crate::i18n::tr(self.name, self.name_en)
    }

    /// Nazwa z uwzględnieniem procesora: bez rdzeni E domena rdzeni to
    /// po prostu "Rdzenie".
    pub fn label_for(&self, hybrid: bool) -> &'static str {
        if !hybrid && self.id == DOMAIN_P_CORE {
            crate::i18n::tr("Rdzenie", "Cores")
        } else {
            self.label()
        }
    }
}

/// Ustawienia napięcia jednej domeny.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Voltage {
    pub offset_mv: i32,
    /// Maksymalny mnożnik OC zapisany w domenie (0 = bez zmian).
    pub max_ratio: u32,
    /// false = tryb adaptacyjny, true = stałe napięcie.
    pub override_mode: bool,
}

/// Wszystko, co zakładka "Procesor" pokazuje. Każda część może się nie udać
/// osobno (np. BIOS blokuje skrzynkę OC, ale limity mocy są dostępne).
#[derive(Clone, Debug)]
pub struct State {
    pub limits: Result<PowerLimits, String>,
    /// (domena, wynik odczytu)
    pub voltages: Vec<(Domain, Result<Voltage, String>)>,
    /// `None`: ten procesor nie jest obsługiwany dla IccMax.
    pub iccmax_a: Option<Result<f32, String>>,
    /// Limity mnożnika turbo rdzeni P i E dla 1..8 aktywnych rdzeni (tylko odczyt).
    pub p_turbo: Result<[u8; 8], String>,
    pub e_turbo: Option<Result<[u8; 8], String>>,
}

/// Wykonuje polecenie skrzynki OC (MSR 0x150) i zwraca dane odpowiedzi.
pub fn mailbox(
    msr: &mut impl Msr,
    cmd: u8,
    param1: u8,
    param2: u8,
    data: u32,
) -> Result<u32, String> {
    msr.write(MSR_OC_MAILBOX, mailbox_request(cmd, param1, param2, data))?;
    for _ in 0..MAILBOX_SPINS {
        let value = msr.read(MSR_OC_MAILBOX)?;
        if mailbox_busy(value) {
            continue;
        }
        let (status, data) = mailbox_response(value);
        if status != 0 {
            return Err(crate::trf!("procesor odrzucił polecenie 0x{cmd:02X} (kod {status}); zwykle oznacza to blokadę strojenia w BIOS-ie", "the CPU rejected command 0x{cmd:02X} (code {status}); this usually means tuning is locked in the BIOS"
            ));
        }
        return Ok(data);
    }
    Err(crate::trf!(
        "procesor nie odpowiedział na polecenie 0x{cmd:02X}",
        "the CPU did not respond to command 0x{cmd:02X}"
    ))
}

fn read_units(msr: &mut impl Msr) -> Result<Units, String> {
    Ok(decode_units(msr.read(MSR_RAPL_POWER_UNIT)?))
}

pub fn read_power_limits(msr: &mut impl Msr) -> Result<PowerLimits, String> {
    let units = read_units(msr)?;
    Ok(decode_power_limits(msr.read(MSR_PKG_POWER_LIMIT)?, units))
}

pub fn read_voltage(msr: &mut impl Msr, domain: Domain) -> Result<Voltage, String> {
    let data = mailbox(msr, OC_READ_VF, domain.id, 0, 0)?;
    Ok(Voltage {
        offset_mv: vf_offset_mv(data),
        max_ratio: vf_max_ratio(data),
        override_mode: vf_is_override(data),
    })
}

fn ia_vr(msr: &mut impl Msr) -> Result<u8, String> {
    Ok(ia_vr_address(mailbox(msr, OC_READ_VR_TOPOLOGY, 0, 0, 0)?))
}

pub fn read_iccmax(msr: &mut impl Msr) -> Result<f32, String> {
    let vr = ia_vr(msr)?;
    Ok(iccmax_amps(mailbox(msr, OC_READ_ICCMAX, vr, 0, 0)?))
}

pub fn read_state(msr: &mut impl Msr, hybrid: bool) -> State {
    State {
        limits: read_power_limits(msr),
        voltages: DOMAINS
            .iter()
            .filter(|d| hybrid || !d.hybrid_only)
            .map(|d| (*d, read_voltage(msr, *d)))
            .collect(),
        iccmax_a: hybrid.then(|| read_iccmax(msr)),
        p_turbo: msr.read(MSR_TURBO_RATIO_LIMIT).map(turbo_ratios),
        e_turbo: hybrid.then(|| msr.read(MSR_SECONDARY_TURBO_RATIO_LIMIT).map(turbo_ratios)),
    }
}

/// Ustawia PL1, PL2 i okno czasowe turbo. Zwraca wartości odczytane po zapisie.
pub fn set_power_limits(
    msr: &mut impl Msr,
    pl1_w: f64,
    pl2_w: f64,
    tau_s: f64,
) -> Result<PowerLimits, String> {
    for (name, watts) in [("PL1", pl1_w), ("PL2", pl2_w)] {
        if !(POWER_MIN_W..=POWER_MAX_W).contains(&watts) {
            return Err(crate::trf!(
                "{name} = {watts} W jest poza zakresem {POWER_MIN_W}-{POWER_MAX_W} W",
                "{name} = {watts} W is outside the range {POWER_MIN_W}-{POWER_MAX_W} W"
            ));
        }
    }
    if !(TAU_MIN_S..=TAU_MAX_S).contains(&tau_s) {
        return Err(crate::trf!(
            "okno czasowe {tau_s} s jest poza zakresem {TAU_MIN_S}-{TAU_MAX_S} s",
            "time window {tau_s} s is outside the range {TAU_MIN_S}-{TAU_MAX_S} s"
        ));
    }

    let units = read_units(msr)?;
    let old = msr.read(MSR_PKG_POWER_LIMIT)?;
    if decode_power_limits(old, units).locked {
        return Err(crate::i18n::tr(
            "limity mocy są zablokowane przez BIOS do następnego restartu",
            "power limits are locked by the BIOS until the next restart",
        )
        .to_string());
    }

    let new = encode_power_limits(old, pl1_w, pl2_w, tau_s, units);
    msr.write(MSR_PKG_POWER_LIMIT, new)?;

    let after = msr.read(MSR_PKG_POWER_LIMIT)?;
    if after != new {
        return Err(crate::trf!("procesor nie przyjął limitów mocy (zapisano 0x{new:016X}, odczytano 0x{after:016X})", "the CPU did not accept the power limits (wrote 0x{new:016X}, read back 0x{after:016X})"
        ));
    }
    Ok(decode_power_limits(after, units))
}

/// Ustawia offset napięcia domeny, nie ruszając jej mnożnika, trybu ani
/// napięcia docelowego. Zwraca offset odczytany po zapisie.
pub fn set_voltage_offset(
    msr: &mut impl Msr,
    domain: Domain,
    offset_mv: i32,
) -> Result<i32, String> {
    if !(OFFSET_MIN_MV..=OFFSET_MAX_MV).contains(&offset_mv) {
        return Err(crate::trf!("offset {offset_mv} mV jest poza dozwolonym zakresem {OFFSET_MIN_MV}..{OFFSET_MAX_MV} mV", "offset {offset_mv} mV is outside the allowed range {OFFSET_MIN_MV}..{OFFSET_MAX_MV} mV"
        ));
    }
    let old = mailbox(msr, OC_READ_VF, domain.id, 0, 0)?;
    mailbox(
        msr,
        OC_WRITE_VF,
        domain.id,
        0,
        vf_with_offset(old, offset_mv),
    )?;

    let after = vf_offset_mv(mailbox(msr, OC_READ_VF, domain.id, 0, 0)?);
    // Pole ma krok 1/1024 V, więc odczyt może się różnić o 1 mV.
    if (after - offset_mv).abs() > 1 {
        return Err(crate::trf!("{}: procesor nie przyjął offsetu (zadano {offset_mv} mV, odczytano {after} mV)", "{}: the CPU did not accept the offset (requested {offset_mv} mV, read back {after} mV)",
            domain.label()
        ));
    }
    Ok(after)
}

/// Ustawia limit prądu rdzeni (IccMax). Zwraca wartość odczytaną po zapisie.
/// Najmniejszy i największy mnożnik turbo przyjmowany do zapisu.
pub const RATIO_MIN: u8 = 8;
pub const RATIO_MAX: u8 = 80;

/// Rdzenie, których mnożniki zmieniamy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreKind {
    /// Rdzenie P (MSR 0x1AD).
    Performance,
    /// Rdzenie E (MSR 0x650).
    Efficient,
}

/// Sprawdza mnożniki przed zapisem: zakres i kolejność (przy większej liczbie
/// aktywnych rdzeni mnożnik nie może być wyższy - tak samo wymaga XTU).
pub fn check_turbo_ratios(ratios: &[u8; 8]) -> Result<(), String> {
    // 0 = grupa nieużywana (np. rdzenie E mają często tylko pierwszą grupę);
    // po zerze mogą być tylko zera.
    let used = ratios.iter().take_while(|r| **r != 0).count();
    if used == 0 || ratios[used..].iter().any(|r| *r != 0) {
        return Err(crate::i18n::tr(
            "pierwsza grupa musi mieć mnożnik, a po grupie nieużywanej (0) mogą być tylko nieużywane",
            "the first group needs a ratio, and only unused groups (0) may follow an unused one",
        )
        .to_string());
    }
    let ratios = &ratios[..used];
    if let Some(r) = ratios
        .iter()
        .find(|r| !(RATIO_MIN..=RATIO_MAX).contains(*r))
    {
        return Err(crate::trf!(
            "mnożnik {r} jest poza zakresem {RATIO_MIN}-{RATIO_MAX}",
            "ratio {r} is outside the range {RATIO_MIN}-{RATIO_MAX}"
        ));
    }
    if let Some(i) = (1..ratios.len()).find(|&i| ratios[i] > ratios[i - 1]) {
        return Err(crate::trf!(
            "mnożnik dla {} aktywnych rdzeni ({}) jest wyższy niż dla {} ({})",
            "the ratio for {} active cores ({}) is higher than for {} ({})",
            i + 1,
            ratios[i],
            i,
            ratios[i - 1]
        ));
    }
    Ok(())
}

/// Zapisuje limity mnożnika turbo (8 grup aktywnych rdzeni) i sprawdza je
/// odczytem. Gdy procesor nie przyjmie wartości (np. zablokowany mnożnik),
/// przywraca poprzedni stan rejestru. Działa do restartu komputera.
pub fn set_turbo_ratios(
    msr: &mut impl Msr,
    kind: CoreKind,
    ratios: [u8; 8],
) -> Result<[u8; 8], String> {
    check_turbo_ratios(&ratios)?;
    let reg = match kind {
        CoreKind::Performance => MSR_TURBO_RATIO_LIMIT,
        CoreKind::Efficient => MSR_SECONDARY_TURBO_RATIO_LIMIT,
    };
    let before = msr.read(reg)?;
    let value = u64::from_le_bytes(ratios);
    if value == before {
        return Ok(ratios);
    }
    msr.write(reg, value)?;
    let after = msr.read(reg)?;
    if after != value {
        // Częściowo przyjęta wartość byłaby nieprzewidywalna - wracamy.
        let _ = msr.write(reg, before);
        return Err(crate::trf!(
            "procesor nie przyjął mnożników (zadano {:?}, odczytano {:?}); przywrócono poprzednie. Mnożnik może być zablokowany (procesor bez K lub BIOS).",
            "the CPU did not accept the ratios (requested {:?}, read back {:?}); the previous ones were restored. The multiplier may be locked (non-K CPU or BIOS).",
            ratios,
            turbo_ratios(after)
        ));
    }
    Ok(turbo_ratios(after))
}

pub fn set_iccmax(msr: &mut impl Msr, amps: f32) -> Result<f32, String> {
    if !(ICCMAX_MIN_A..=ICCMAX_MAX_A).contains(&amps) {
        return Err(crate::trf!(
            "IccMax {amps} A jest poza zakresem {ICCMAX_MIN_A}-{ICCMAX_MAX_A} A",
            "IccMax {amps} A is outside the range {ICCMAX_MIN_A}-{ICCMAX_MAX_A} A"
        ));
    }
    let vr = ia_vr(msr)?;
    let data = iccmax_data(amps);
    // Jak w kodzie odniesienia: najpierw sama wartość, potem (przy maksimum)
    // ta sama wartość z bitem "bez limitu".
    mailbox(msr, OC_WRITE_ICCMAX, vr, 0, data & ICCMAX_MASK)?;
    if data & ICCMAX_UNLIMITED != 0 {
        mailbox(msr, OC_WRITE_ICCMAX, vr, 0, data)?;
    }

    let after = iccmax_amps(mailbox(msr, OC_READ_ICCMAX, vr, 0, 0)?);
    if (after - iccmax_amps(data)).abs() > 0.26 {
        return Err(crate::trf!(
            "procesor nie przyjął IccMax (zadano {:.2} A, odczytano {after:.2} A)",
            "the CPU did not accept IccMax (requested {:.2} A, read back {after:.2} A)",
            iccmax_amps(data)
        ));
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Symulowany procesor: zwykłe rejestry plus skrzynka OC.
    struct Fake {
        regs: HashMap<u64, u64>,
        vf: HashMap<u8, u32>,
        iccmax: u32,
        vr: u8,
        /// Odrzucaj zapisy skrzynki (jak przy blokadzie w BIOS-ie).
        oc_locked: bool,
        /// Zapisy do zwykłych rejestrów, w kolejności.
        writes: Vec<(u64, u64)>,
        /// Polecenia skrzynki, w kolejności: (polecenie, parametr 1, dane).
        commands: Vec<(u8, u8, u32)>,
    }

    impl Fake {
        fn new() -> Self {
            let mut regs = HashMap::new();
            regs.insert(MSR_RAPL_POWER_UNIT, 0x000A_0E03);
            // PL1 125 W włączony, tau 28 s, PL2 253 W włączony.
            let tau: u64 = 14 | (3 << 5);
            regs.insert(
                MSR_PKG_POWER_LIMIT,
                1000 | (1 << 15) | (1 << 16) | (tau << 17) | (2024u64 << 32) | (1 << 47),
            );
            regs.insert(MSR_TURBO_RATIO_LIMIT, 0x2E2E_2E2E_2E2E_3030);
            regs.insert(MSR_SECONDARY_TURBO_RATIO_LIMIT, 0x2424_2424_2424_2424);
            let mut vf = HashMap::new();
            // domena P: mnożnik 0x30, tryb adaptacyjny, offset 0
            vf.insert(DOMAIN_P_CORE, 0x30);
            vf.insert(DOMAIN_E_CORE, 0x24);
            vf.insert(DOMAIN_RING, 0x28);
            Self {
                regs,
                vf,
                iccmax: 960,
                vr: 3,
                oc_locked: false,
                writes: Vec::new(),
                commands: Vec::new(),
            }
        }

        fn run_mailbox(&mut self, request: u64) -> u64 {
            let cmd = ((request >> 32) & 0xFF) as u8;
            let p1 = ((request >> 40) & 0xFF) as u8;
            let data = request as u32;
            self.commands.push((cmd, p1, data));
            let ok = |data: u32| data as u64;
            let fail = |code: u64| code << 32;
            match cmd {
                OC_READ_VR_TOPOLOGY => ok((self.vr as u32) << 8),
                OC_READ_VF => match self.vf.get(&p1) {
                    Some(v) => ok(*v),
                    None => fail(4),
                },
                OC_WRITE_VF if self.oc_locked => fail(7),
                OC_WRITE_VF => {
                    self.vf.insert(p1, data);
                    ok(0)
                }
                OC_READ_ICCMAX if p1 == self.vr => ok(self.iccmax),
                OC_WRITE_ICCMAX if p1 == self.vr && !self.oc_locked => {
                    self.iccmax = data;
                    ok(0)
                }
                _ => fail(5),
            }
        }
    }

    impl Msr for Fake {
        fn read(&mut self, msr: u64) -> Result<u64, String> {
            self.regs
                .get(&msr)
                .copied()
                .ok_or_else(|| format!("brak MSR 0x{msr:X}"))
        }

        fn write(&mut self, msr: u64, value: u64) -> Result<(), String> {
            if msr == MSR_OC_MAILBOX {
                let response = self.run_mailbox(value);
                self.regs.insert(MSR_OC_MAILBOX, response);
            } else {
                self.writes.push((msr, value));
                // zablokowany rejestr limitów ignoruje zapis
                let locked = msr == MSR_PKG_POWER_LIMIT
                    && self.regs.get(&msr).is_some_and(|v| v & (1 << 63) != 0);
                if !locked {
                    self.regs.insert(msr, value);
                }
            }
            Ok(())
        }
    }

    #[test]
    fn reads_whole_state() {
        let mut cpu = Fake::new();
        let s = read_state(&mut cpu, true);
        let limits = s.limits.unwrap();
        assert_eq!(
            (limits.pl1_w, limits.pl2_w, limits.tau_s),
            (125.0, 253.0, 28.0)
        );
        assert_eq!(s.voltages.len(), 3);
        assert_eq!(s.voltages[0].1.clone().unwrap().max_ratio, 0x30);
        assert_eq!(s.iccmax_a.unwrap().unwrap(), 240.0);
        assert_eq!(s.p_turbo.unwrap()[0], 0x30);
        assert_eq!(s.e_turbo.unwrap().unwrap()[0], 0x24);
        // odczyt stanu niczego nie zapisuje do zwykłych rejestrów
        assert!(cpu.writes.is_empty());
        assert!(cpu
            .commands
            .iter()
            .all(|(c, _, _)| *c != OC_WRITE_VF && *c != OC_WRITE_ICCMAX));
    }

    #[test]
    fn non_hybrid_skips_e_cores_and_iccmax() {
        let mut cpu = Fake::new();
        let s = read_state(&mut cpu, false);
        assert_eq!(s.voltages.len(), 2);
        assert!(s.iccmax_a.is_none() && s.e_turbo.is_none());
        assert!(cpu
            .commands
            .iter()
            .all(|(c, p, _)| *c == OC_READ_VF && *p != DOMAIN_E_CORE));
    }

    #[test]
    fn sets_power_limits_with_one_write() {
        let mut cpu = Fake::new();
        let after = set_power_limits(&mut cpu, 65.0, 180.0, 56.0).unwrap();
        assert_eq!((after.pl1_w, after.pl2_w, after.tau_s), (65.0, 180.0, 56.0));
        assert_eq!(cpu.writes.len(), 1);
        assert_eq!(cpu.writes[0].0, MSR_PKG_POWER_LIMIT);
        // bit clamp PL1 zachowany
        assert!(cpu.writes[0].1 & (1 << 16) != 0);
    }

    #[test]
    fn refuses_locked_or_out_of_range_power_limits() {
        let mut cpu = Fake::new();
        assert!(set_power_limits(&mut cpu, 0.0, 180.0, 56.0).is_err());
        assert!(set_power_limits(&mut cpu, 65.0, 5000.0, 56.0).is_err());
        assert!(set_power_limits(&mut cpu, 65.0, 180.0, 0.0).is_err());
        let v = cpu.regs[&MSR_PKG_POWER_LIMIT];
        cpu.regs.insert(MSR_PKG_POWER_LIMIT, v | (1 << 63));
        assert!(set_power_limits(&mut cpu, 65.0, 180.0, 56.0).is_err());
        assert!(cpu.writes.is_empty(), "nic nie może zostać zapisane");
    }

    #[test]
    fn sets_offset_and_keeps_other_fields() {
        let mut cpu = Fake::new();
        let p = DOMAINS[0];
        assert_eq!(set_voltage_offset(&mut cpu, p, -100).unwrap(), -100);
        // kolejność: odczyt, zapis, odczyt kontrolny
        let cmds: Vec<u8> = cpu.commands.iter().map(|c| c.0).collect();
        assert_eq!(cmds, [OC_READ_VF, OC_WRITE_VF, OC_READ_VF]);
        // zapisane dane: mnożnik 0x30 bez zmian + offset -100 mV
        assert_eq!(cpu.commands[1], (OC_WRITE_VF, DOMAIN_P_CORE, 0xF340_0030));
        // inne domeny nietknięte
        assert_eq!(cpu.vf[&DOMAIN_E_CORE], 0x24);
        let e = DOMAINS[1];
        assert_eq!(set_voltage_offset(&mut cpu, e, -120).unwrap(), -120);
        assert_eq!(read_voltage(&mut cpu, e).unwrap().max_ratio, 0x24);
    }

    #[test]
    fn refuses_offsets_outside_safe_range_without_touching_cpu() {
        let mut cpu = Fake::new();
        assert!(set_voltage_offset(&mut cpu, DOMAINS[0], -251).is_err());
        assert!(set_voltage_offset(&mut cpu, DOMAINS[0], 51).is_err());
        assert!(set_voltage_offset(&mut cpu, DOMAINS[0], 500).is_err());
        assert!(cpu.commands.is_empty());
    }

    #[test]
    fn sets_turbo_ratios_and_checks_order() {
        let mut cpu = Fake::new();
        let new = [0x31, 0x31, 0x2F, 0x2F, 0x2E, 0x2E, 0x2E, 0x2E];
        assert_eq!(
            set_turbo_ratios(&mut cpu, CoreKind::Performance, new).unwrap(),
            new
        );
        assert_eq!(cpu.regs[&MSR_TURBO_RATIO_LIMIT], u64::from_le_bytes(new));
        let e = [0x25; 8];
        set_turbo_ratios(&mut cpu, CoreKind::Efficient, e).unwrap();
        assert_eq!(
            cpu.regs[&MSR_SECONDARY_TURBO_RATIO_LIMIT],
            u64::from_le_bytes(e)
        );

        // Rosnący mnożnik i zakres - odrzucone bez zapisu.
        let writes = cpu.writes.len();
        assert!(set_turbo_ratios(
            &mut cpu,
            CoreKind::Performance,
            [40, 41, 40, 40, 40, 40, 40, 40]
        )
        .is_err());
        assert!(set_turbo_ratios(&mut cpu, CoreKind::Performance, [90; 8]).is_err());
        assert!(set_turbo_ratios(&mut cpu, CoreKind::Performance, [7; 8]).is_err());
        assert!(
            set_turbo_ratios(&mut cpu, CoreKind::Performance, [40, 0, 40, 0, 0, 0, 0, 0]).is_err()
        );
        assert!(set_turbo_ratios(&mut cpu, CoreKind::Performance, [0; 8]).is_err());
        assert_eq!(cpu.writes.len(), writes);
    }

    #[test]
    fn accepts_unused_groups() {
        let mut cpu = Fake::new();
        let e = [36, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            set_turbo_ratios(&mut cpu, CoreKind::Efficient, e).unwrap(),
            e
        );
    }

    #[test]
    fn restores_ratios_when_cpu_ignores_them() {
        struct Locked(u64, Vec<u64>);
        impl Msr for Locked {
            fn read(&mut self, _msr: u64) -> Result<u64, String> {
                Ok(self.0)
            }
            fn write(&mut self, _msr: u64, value: u64) -> Result<(), String> {
                // Zablokowany procesor przyjmuje zapis, ale nic nie zmienia.
                self.1.push(value);
                Ok(())
            }
        }
        let before = 0x2E2E_2E2E_2E2E_3030;
        let mut cpu = Locked(before, Vec::new());
        assert!(set_turbo_ratios(&mut cpu, CoreKind::Performance, [0x32; 8]).is_err());
        assert_eq!(cpu.1.last(), Some(&before));
    }

    #[test]
    fn reports_locked_mailbox() {
        let mut cpu = Fake::new();
        cpu.oc_locked = true;
        let err = set_voltage_offset(&mut cpu, DOMAINS[0], -50).unwrap_err();
        assert!(err.contains("kod 7"), "{err}");
        assert_eq!(read_voltage(&mut cpu, DOMAINS[0]).unwrap().offset_mv, 0);
        assert!(set_iccmax(&mut cpu, 200.0).is_err());
        assert_eq!(cpu.iccmax, 960);
    }

    #[test]
    fn sets_iccmax_through_discovered_regulator() {
        let mut cpu = Fake::new();
        assert_eq!(set_iccmax(&mut cpu, 307.0).unwrap(), 307.0);
        assert_eq!(cpu.iccmax, 1228);
        // zapis poszedł na adres regulatora odczytany poleceniem 0x04
        let write = cpu
            .commands
            .iter()
            .find(|c| c.0 == OC_WRITE_ICCMAX)
            .unwrap();
        assert_eq!(write.1, 3);
        // maksimum: druga komenda z bitem "bez limitu"
        cpu.commands.clear();
        set_iccmax(&mut cpu, 511.75).unwrap();
        let writes: Vec<u32> = cpu
            .commands
            .iter()
            .filter(|c| c.0 == OC_WRITE_ICCMAX)
            .map(|c| c.2)
            .collect();
        assert_eq!(writes, [ICCMAX_MASK, ICCMAX_MASK | ICCMAX_UNLIMITED]);
        assert!(set_iccmax(&mut cpu, 0.5).is_err());
    }

    #[test]
    fn mailbox_times_out_when_cpu_stays_busy() {
        struct Stuck;
        impl Msr for Stuck {
            fn read(&mut self, _msr: u64) -> Result<u64, String> {
                Ok(1 << 63)
            }
            fn write(&mut self, _msr: u64, _value: u64) -> Result<(), String> {
                Ok(())
            }
        }
        assert!(mailbox(&mut Stuck, OC_READ_VF, 0, 0, 0).is_err());
    }
}

#[cfg(test)]
mod hardware {
    //! Mnożniki turbo na tym komputerze: odczyt, zapis o 1 niżej w ostatniej
    //! grupie, odczyt kontrolny, przywrócenie (administrator):
    //! `cargo test intel_ratio_roundtrip -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn intel_ratio_roundtrip() {
        let exe = std::env::current_exe().unwrap();
        let module = exe.parent().unwrap().join("modules").join("IntelMSR.bin");
        let mut io = crate::cpu::pawnio::PawnIo::open(&module).expect("PawnIO");
        for (kind, reg) in [
            (CoreKind::Performance, MSR_TURBO_RATIO_LIMIT),
            (CoreKind::Efficient, MSR_SECONDARY_TURBO_RATIO_LIMIT),
        ] {
            let before = turbo_ratios(io.read(reg).expect("odczyt"));
            println!("{kind:?}: {before:?}");
            let mut lower = before;
            let last = before.iter().rposition(|r| *r != 0).unwrap();
            lower[last] -= 1;
            match set_turbo_ratios(&mut io, kind, lower) {
                Ok(after) => println!("  zapis {lower:?} -> odczyt {after:?}"),
                Err(e) => println!("  zapis nieudany: {e}"),
            }
            let restored = set_turbo_ratios(&mut io, kind, before).expect("przywrócenie");
            println!("  przywrócono {restored:?}");
            assert_eq!(restored, before);
        }
    }
}
