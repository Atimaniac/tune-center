//! Strojenie procesorów AMD Ryzen przez SMU: Curve Optimizer dla każdego
//! rdzenia oraz limity Precision Boost Overdrive (PPT, TDC, EDC, skalar).
//!
//! Numery poleceń SMU, kodowanie maski rdzenia i marginesu, adresy skrzynek
//! i bezpieczników rdzeni pochodzą z projektu ZenStates-Core (irusanov,
//! GPL-3.0; użyte jako odniesienie, bez kopiowania kodu). Położenie limitów
//! w tabeli PM: ryzen_monitor (Zen 3) i rigstats (Zen 5, sprawdzone na 9800X3D).
//! Polecenia trafiają do SMU przez oficjalny moduł PawnIO `RyzenSMU.bin`.
//!
//! Dwie rzeczy są zrobione okrężną drogą, bo nie ma dla nich pewnego,
//! bezpośredniego polecenia:
//!
//! - Ryzen 5000: margines ustawia się przez skrzynkę MP1 (polecenie 0x35),
//!   a odczytuje przez RSMU (0x7C). Odczyt jest sprawdzany przed pierwszym
//!   zapisem i po każdym zapisie.
//! - Bieżące PPT, TDC i EDC czyta się z tabeli PM. Jej układ zależy od wersji;
//!   dla wersji bez potwierdzonego układu wartość jest oznaczona jako
//!   niepotwierdzona, dopóki zapis i ponowny odczyt nie wskażą właściwego pola.
//!
//! Logika działa na dowolnym [`Smu`], więc jest sprawdzana testami na
//! symulowanym procesorze. Każdy zapis Curve Optimizera jest potwierdzany
//! odczytem.

/// Dostęp do SMU procesora.
pub trait Smu {
    /// Wysyła polecenie do skrzynki RSMU; zwraca argumenty odpowiedzi.
    fn command(&mut self, cmd: u32, args: [u32; 6]) -> Result<[u32; 6], String>;
    /// Wysyła polecenie do skrzynki MP1 (Curve Optimizer na Ryzen 5000).
    fn mp1_command(
        &mut self,
        mailbox: Mailbox,
        cmd: u32,
        args: [u32; 6],
    ) -> Result<[u32; 6], String>;
    /// Odczyt rejestru SMN (bezpieczniki z mapą rdzeni).
    fn read_smn(&mut self, address: u32) -> Result<u32, String>;
    /// Świeża tabela PM: (wersja, początkowe wartości jako liczby zmiennoprzecinkowe).
    fn pm_table(&mut self) -> Result<(u32, Vec<f32>), String>;
}

/// Adresy rejestrów jednej skrzynki SMU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mailbox {
    pub msg: u32,
    pub rsp: u32,
    pub args: u32,
}

/// Skrzynka MP1 procesorów Zen 3.
pub const MP1_ZEN3: Mailbox = Mailbox {
    msg: 0x03B1_0530,
    rsp: 0x03B1_057C,
    args: 0x03B1_09C4,
};

const SMU_OK: u32 = 0x01;
/// Ile razy pytamy SMU o zakończenie polecenia.
const MAILBOX_SPINS: usize = 8192;

/// Jedno polecenie skrzynki SMU wykonane na surowych rejestrach - ta sama
/// kolejność kroków, której używa moduł PawnIO dla skrzynki RSMU: poczekaj,
/// aż poprzednie polecenie się skończy, wyzeruj odpowiedź, wpisz argumenty,
/// wpisz numer polecenia, poczekaj na odpowiedź, odczytaj argumenty.
pub fn mailbox_transaction(
    read: &mut dyn FnMut(u32) -> Result<u32, String>,
    write: &mut dyn FnMut(u32, u32) -> Result<(), String>,
    mailbox: Mailbox,
    cmd: u32,
    args: [u32; 6],
) -> Result<[u32; 6], String> {
    let wait = |read: &mut dyn FnMut(u32) -> Result<u32, String>| -> Result<u32, String> {
        for _ in 0..MAILBOX_SPINS {
            let value = read(mailbox.rsp)?;
            if value != 0 {
                return Ok(value);
            }
        }
        Ok(0)
    };

    if wait(read)? == 0 {
        return Err(crate::i18n::tr(
            "SMU jest zajęte poprzednim poleceniem",
            "The SMU is busy with a previous command",
        )
        .to_string());
    }
    write(mailbox.rsp, 0)?;
    for (i, arg) in args.iter().enumerate() {
        write(mailbox.args + 4 * i as u32, *arg)?;
    }
    write(mailbox.msg, cmd)?;

    match wait(read)? {
        SMU_OK => {}
        0 => {
            return Err(crate::trf!(
                "SMU nie odpowiedziało na polecenie 0x{cmd:02X}",
                "The SMU did not respond to command 0x{cmd:02X}"
            ))
        }
        0xFC => {
            return Err(crate::trf!(
                "SMU odrzuciło polecenie 0x{cmd:02X}: zajęte",
                "The SMU rejected command 0x{cmd:02X}: busy"
            ))
        }
        0xFD => {
            return Err(crate::trf!(
                "SMU odrzuciło polecenie 0x{cmd:02X}: niespełniony warunek",
                "The SMU rejected command 0x{cmd:02X}: prerequisite not met"
            ))
        }
        0xFE => {
            return Err(crate::trf!(
                "SMU nie zna polecenia 0x{cmd:02X}",
                "The SMU does not know command 0x{cmd:02X}"
            ))
        }
        other => {
            return Err(crate::trf!(
                "polecenie 0x{cmd:02X} nie powiodło się (kod 0x{other:02X})",
                "command 0x{cmd:02X} failed (code 0x{other:02X})"
            ))
        }
    }

    let mut out = [0u32; 6];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = read(mailbox.args + 4 * i as u32)?;
    }
    Ok(out)
}

/// Obsługiwane generacje procesorów desktopowych.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Generation {
    /// Ryzen 5000 (Vermeer).
    Zen3,
    /// Ryzen 7000 (Raphael).
    Zen4,
    /// Ryzen 9000 (Granite Ridge).
    Zen5,
}

impl Generation {
    /// Rozpoznanie po rodzinie i modelu z CPUID.
    pub fn detect(family: u32, model: u32) -> Option<Self> {
        match (family, model) {
            (0x19, 0x20) | (0x19, 0x21) => Some(Self::Zen3),
            (0x19, 0x61) => Some(Self::Zen4),
            (0x1A, 0x44) => Some(Self::Zen5),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Zen3 => "Zen 3 (Ryzen 5000)",
            Self::Zen4 => "Zen 4 (Ryzen 7000)",
            Self::Zen5 => "Zen 5 (Ryzen 9000)",
        }
    }
}

/// Polecenia Curve Optimizera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveCommands {
    pub set_core: u32,
    /// `Some`: zapis idzie przez skrzynkę MP1 o tych adresach, a nie przez RSMU.
    pub set_mailbox: Option<Mailbox>,
    /// Odczyt, zawsze przez RSMU.
    pub get_core: u32,
    /// Adres bezpiecznika z mapą wyłączonych rdzeni pierwszego CCD.
    pub core_fuse: u32,
    /// Dozwolony zakres marginesu.
    pub min: i32,
    pub max: i32,
}

/// Polecenia RSMU danej generacji.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Commands {
    pub set_ppt: u32,
    pub set_tdc: u32,
    pub set_edc: u32,
    pub set_scalar: u32,
    pub get_scalar: u32,
    pub curve: Option<CurveCommands>,
}

/// Odstęp adresów bezpieczników rdzeni kolejnych CCD.
const CORE_FUSE_CCD_STRIDE: u32 = 1 << 25;
const CORE_SLOTS_PER_CCD: u32 = 8;

pub fn commands(generation: Generation) -> Commands {
    match generation {
        // Zen 3: odniesienie oznacza zapis marginesu przez RSMU jako niepewny,
        // więc zapis idzie potwierdzoną drogą przez MP1 (0x35). Odczyt przez
        // RSMU 0x7C jest sprawdzany przed pierwszym zapisem i po każdym zapisie.
        Generation::Zen3 => Commands {
            set_ppt: 0x53,
            set_tdc: 0x54,
            set_edc: 0x55,
            set_scalar: 0x58,
            get_scalar: 0x6C,
            curve: Some(CurveCommands {
                set_core: 0x35,
                set_mailbox: Some(MP1_ZEN3),
                get_core: 0x7C,
                core_fuse: 0x3008_1D98,
                min: -30,
                max: CO_MAX,
            }),
        },
        Generation::Zen4 | Generation::Zen5 => Commands {
            set_ppt: 0x56,
            set_tdc: 0x57,
            set_edc: 0x58,
            set_scalar: 0x5B,
            get_scalar: 0x6D,
            curve: Some(CurveCommands {
                set_core: 0x06,
                set_mailbox: None,
                get_core: 0xD5,
                core_fuse: if generation == Generation::Zen4 {
                    0x3008_1CD0
                } else {
                    0x304A_03DC
                },
                min: CO_MIN,
                max: CO_MAX,
            }),
        },
    }
}

/// Najszerszy zakres Curve Optimizera (Zen 4 i 5; Zen 3 ma -30). Ujemne
/// wartości obniżają napięcie; dodatnie je podnoszą, dlatego są ograniczone
/// mocniej niż w BIOS-ie (tam do +50 na Zen 4 i 5).
pub const CO_MIN: i32 = -50;
pub const CO_MAX: i32 = 30;

pub const PPT_MIN_W: u32 = 15;
pub const PPT_MAX_W: u32 = 1000;
pub const CURRENT_MIN_A: u32 = 10;
pub const CURRENT_MAX_A: u32 = 1000;
pub const SCALAR_MIN: u32 = 1;
pub const SCALAR_MAX: u32 = 10;

/// Rdzeń fizyczny: numer kolejny w systemie i położenie w procesorze.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Core {
    /// Numer rdzenia tak, jak liczy go system (od 0).
    pub index: usize,
    pub ccd: u32,
    /// Numer miejsca w CCD (0-7).
    pub slot: u32,
}

impl Core {
    /// Maska rdzenia w argumencie polecenia: [31:28] CCD, [23:20] rdzeń.
    pub fn mask(self) -> u32 {
        (self.ccd << 28) | ((self.slot % 8) << 20)
    }
}

/// Margines w dolnych 16 bitach argumentu (ze znakiem).
pub fn margin_arg(margin: i32) -> u32 {
    (margin as u32) & 0xFFFF
}

fn one(value: u32) -> [u32; 6] {
    [value, 0, 0, 0, 0, 0]
}

/// Mapa rdzeni z bezpieczników. Procesory z 6 rdzeniami na CCD mają 8 miejsc,
/// z których dwa są wyłączone - numer rdzenia w systemie nie musi więc
/// odpowiadać numerowi miejsca. Mapa jest odrzucana, jeśli liczba włączonych
/// miejsc nie zgadza się z liczbą rdzeni z CPUID.
pub fn core_map(
    smu: &mut impl Smu,
    curve: CurveCommands,
    physical_cores: usize,
) -> Result<Vec<Core>, String> {
    if physical_cores == 0 || physical_cores > 16 {
        return Err(crate::trf!(
            "nieobsługiwana liczba rdzeni: {physical_cores}",
            "unsupported core count: {physical_cores}"
        ));
    }
    let ccds = if physical_cores > 8 { 2 } else { 1 };

    let mut cores = Vec::new();
    for ccd in 0..ccds {
        let fuse = smu.read_smn(curve.core_fuse + CORE_FUSE_CCD_STRIDE * ccd)?;
        if fuse == 0xFFFF_FFFF {
            return Err(crate::trf!(
                "nie udało się odczytać mapy rdzeni CCD{ccd}",
                "could not read the core map of CCD{ccd}"
            ));
        }
        let disabled = fuse & 0xFF;
        for slot in 0..CORE_SLOTS_PER_CCD {
            if disabled & (1 << slot) == 0 {
                cores.push(Core {
                    index: cores.len(),
                    ccd,
                    slot,
                });
            }
        }
    }

    if cores.len() != physical_cores {
        return Err(crate::trf!("mapa rdzeni z procesora daje {} rdzeni, a system widzi {physical_cores}; Curve Optimizer jest wyłączony, żeby nie trafić w zły rdzeń", "the core map from the CPU gives {} cores but the system sees {physical_cores}; Curve Optimizer is disabled to avoid hitting the wrong core",
            cores.len()
        ));
    }
    Ok(cores)
}

pub fn get_margin(smu: &mut impl Smu, curve: CurveCommands, core: Core) -> Result<i32, String> {
    let out = smu.command(curve.get_core, one(core.mask()))?;
    // Zwykle 32 bity ze znakiem; na wypadek odpowiedzi 16-bitowej rozszerzamy znak.
    let margin = match out[0] {
        raw @ 0x8000..=0xFFFF => raw as u16 as i16 as i32,
        raw => raw as i32,
    };
    // Wartość spoza zakresu BIOS-u oznacza, że odpowiedź nie jest marginesem.
    if !(-50..=50).contains(&margin) {
        return Err(crate::trf!(
            "rdzeń {}: nieoczekiwana odpowiedź SMU ({margin})",
            "core {}: unexpected SMU response ({margin})",
            core.index
        ));
    }
    Ok(margin)
}

/// Ustawia margines jednego rdzenia i potwierdza go odczytem.
pub fn set_margin(
    smu: &mut impl Smu,
    curve: CurveCommands,
    core: Core,
    margin: i32,
) -> Result<i32, String> {
    if !(curve.min..=curve.max).contains(&margin) {
        return Err(crate::trf!(
            "margines {margin} jest poza dozwolonym zakresem {}..{}",
            "margin {margin} is outside the allowed range {}..{}",
            curve.min,
            curve.max
        ));
    }
    // Odczyt musi działać, zanim cokolwiek zapiszemy - inaczej zapisu nie da
    // się potwierdzić.
    get_margin(smu, curve, core)?;

    let args = one((core.mask() & 0xFFF0_0000) | margin_arg(margin));
    match curve.set_mailbox {
        Some(mailbox) => smu.mp1_command(mailbox, curve.set_core, args)?,
        None => smu.command(curve.set_core, args)?,
    };
    let after = get_margin(smu, curve, core)?;
    if after != margin {
        return Err(crate::trf!(
            "rdzeń {}: procesor nie przyjął marginesu (zadano {margin}, odczytano {after})",
            "core {}: the CPU did not accept the margin (requested {margin}, read back {after})",
            core.index
        ));
    }
    Ok(after)
}

fn check(name: &str, value: u32, min: u32, max: u32, unit: &str) -> Result<(), String> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(crate::trf!(
            "{name} = {value} {unit} jest poza zakresem {min}-{max} {unit}",
            "{name} = {value} {unit} is outside the range {min}-{max} {unit}"
        ))
    }
}

/// Limit mocy pakietu (PPT) w watach.
pub fn set_ppt(smu: &mut impl Smu, cmds: Commands, watts: u32) -> Result<(), String> {
    check("PPT", watts, PPT_MIN_W, PPT_MAX_W, "W")?;
    smu.command(cmds.set_ppt, one(watts * 1000)).map(|_| ())
}

/// Limit prądu ciągłego (TDC) w amperach.
pub fn set_tdc(smu: &mut impl Smu, cmds: Commands, amps: u32) -> Result<(), String> {
    check("TDC", amps, CURRENT_MIN_A, CURRENT_MAX_A, "A")?;
    smu.command(cmds.set_tdc, one(amps * 1000)).map(|_| ())
}

/// Limit prądu szczytowego (EDC) w amperach.
pub fn set_edc(smu: &mut impl Smu, cmds: Commands, amps: u32) -> Result<(), String> {
    check("EDC", amps, CURRENT_MIN_A, CURRENT_MAX_A, "A")?;
    smu.command(cmds.set_edc, one(amps * 1000)).map(|_| ())
}

/// Położenie limitów w tabeli PM (numery kolejnych liczb zmiennoprzecinkowych).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub ppt: usize,
    pub tdc: usize,
    pub edc: usize,
    /// Układ tej wersji tabeli jest potwierdzony na sprzęcie.
    pub verified: bool,
}

/// Układ dla danej generacji i wersji tabeli. Dla wersji spoza listy zwraca
/// układ najbliższej znanej wersji jako niepotwierdzony.
pub fn layout(generation: Generation, version: u32) -> Layout {
    match generation {
        Generation::Zen3 => Layout {
            ppt: 0,
            tdc: 2,
            edc: 8,
            // 0x380905 w źródle jest wyprowadzone z sąsiednich wersji.
            verified: matches!(version, 0x38_0804 | 0x38_0805 | 0x38_0904),
        },
        Generation::Zen4 | Generation::Zen5 => Layout {
            ppt: 2,
            tdc: 8,
            edc: 63,
            verified: version == 0x62_0105,
        },
    }
}

/// Który limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Limit {
    Ppt,
    Tdc,
    Edc,
}

impl Layout {
    pub fn index(&self, limit: Limit) -> usize {
        match limit {
            Limit::Ppt => self.ppt,
            Limit::Tdc => self.tdc,
            Limit::Edc => self.edc,
        }
    }

    fn set_index(&mut self, limit: Limit, index: usize) {
        match limit {
            Limit::Ppt => self.ppt = index,
            Limit::Tdc => self.tdc = index,
            Limit::Edc => self.edc = index,
        }
    }
}

/// Bieżące limity odczytane z tabeli PM. `None`: wartość w tabeli nie
/// wygląda jak limit (poza zakresem albo nie jest liczbą).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub ppt_w: Option<f32>,
    pub tdc_a: Option<f32>,
    pub edc_a: Option<f32>,
    pub version: u32,
}

fn plausible(table: &[f32], index: usize) -> Option<f32> {
    let value = *table.get(index)?;
    (value.is_finite() && (1.0..=2000.0).contains(&value)).then_some(value)
}

pub fn read_limits(smu: &mut impl Smu, layout: Layout) -> Result<Limits, String> {
    let (version, table) = smu.pm_table()?;
    Ok(Limits {
        ppt_w: plausible(&table, layout.ppt),
        tdc_a: plausible(&table, layout.tdc),
        edc_a: plausible(&table, layout.edc),
        version,
    })
}

/// Wynik sprawdzenia zapisu limitu w tabeli PM.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Readback {
    /// Tabela pokazuje zadaną wartość w oczekiwanym polu.
    Confirmed,
    /// Zadana wartość jest w innym, jedynym pasującym polu - układ poprawiono.
    Relocated(usize),
    /// Polecenie przyjęte, ale tabela nie pokazuje zadanej wartości.
    Unconfirmed,
}

/// Po zapisie limitu szuka zadanej wartości w tabeli PM. Jeśli jest w polu
/// z układu - układ się zgadza. Jeśli nie, a dokładnie jedno inne pole ma tę
/// wartość, to ono jest limitem i układ zostaje poprawiony.
pub fn confirm_limit(
    smu: &mut impl Smu,
    layout: &mut Layout,
    limit: Limit,
    value: u32,
) -> Result<Readback, String> {
    let (_, table) = smu.pm_table()?;
    let matches = |index: usize| {
        table
            .get(index)
            .is_some_and(|v| (v - value as f32).abs() < 0.01)
    };

    if matches(layout.index(limit)) {
        return Ok(Readback::Confirmed);
    }
    let found: Vec<usize> = (0..table.len()).filter(|i| matches(*i)).collect();
    if let [index] = found[..] {
        layout.set_index(limit, index);
        return Ok(Readback::Relocated(index));
    }
    Ok(Readback::Unconfirmed)
}

pub fn get_scalar(smu: &mut impl Smu, cmds: Commands) -> Result<f32, String> {
    let scalar = f32::from_bits(smu.command(cmds.get_scalar, one(0))?[0]);
    if (0.0..=10.0).contains(&scalar) {
        Ok(scalar)
    } else {
        Err(crate::trf!(
            "nieoczekiwana wartość skalara PBO ({scalar})",
            "unexpected PBO scalar value ({scalar})"
        ))
    }
}

/// Skalar PBO (1x-10x). Zwraca wartość odczytaną po zapisie.
pub fn set_scalar(smu: &mut impl Smu, cmds: Commands, scalar: u32) -> Result<f32, String> {
    check(
        crate::i18n::tr("Skalar PBO", "PBO scalar"),
        scalar,
        SCALAR_MIN,
        SCALAR_MAX,
        "x",
    )?;
    smu.command(cmds.set_scalar, one(scalar * 100))?;
    get_scalar(smu, cmds)
}

/// Stan pokazywany w zakładce.
#[derive(Clone, Debug)]
pub struct State {
    pub scalar: Result<f32, String>,
    /// `None`: Curve Optimizer nie jest obsługiwany dla tej generacji.
    pub cores: Option<Result<Vec<(Core, Result<i32, String>)>, String>>,
    /// Bieżące PPT, TDC i EDC z tabeli PM.
    pub limits: Result<Limits, String>,
}

pub fn read_state(
    smu: &mut impl Smu,
    cmds: Commands,
    layout: Layout,
    physical_cores: usize,
) -> State {
    let cores = cmds.curve.map(|curve| {
        core_map(smu, curve, physical_cores).map(|cores| {
            cores
                .into_iter()
                .map(|core| (core, get_margin(smu, curve, core)))
                .collect()
        })
    });
    State {
        scalar: get_scalar(smu, cmds),
        cores,
        limits: read_limits(smu, layout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Symulowany Ryzen. `zen3` przełącza numery poleceń i drogę zapisu.
    struct Fake {
        zen3: bool,
        /// Maska wyłączonych miejsc każdego CCD.
        disabled: Vec<u32>,
        margins: HashMap<u32, i32>,
        scalar: f32,
        fuse_base: u32,
        /// Tabela PM i jej wersja.
        table: Vec<f32>,
        version: u32,
        /// Pola tabeli, do których SMU wpisuje PPT, TDC i EDC.
        limit_fields: [usize; 3],
        /// Polecenia RSMU: (numer, pierwszy argument).
        log: Vec<(u32, u32)>,
        /// Polecenia MP1: (numer, pierwszy argument).
        mp1_log: Vec<(u32, u32)>,
        reject_writes: bool,
        reject_get: bool,
    }

    impl Fake {
        fn new(generation: Generation, disabled: &[u32]) -> Self {
            let zen3 = generation == Generation::Zen3;
            let cmds = commands(generation);
            let mut table = vec![0.0f32; 128];
            let limit_fields = if zen3 { [0, 2, 8] } else { [2, 8, 63] };
            table[limit_fields[0]] = 142.0;
            table[limit_fields[1]] = 95.0;
            table[limit_fields[2]] = 140.0;
            Self {
                zen3,
                disabled: disabled.to_vec(),
                margins: HashMap::new(),
                scalar: 1.0,
                fuse_base: cmds.curve.unwrap().core_fuse,
                table,
                version: if zen3 { 0x38_0804 } else { 0x62_0105 },
                limit_fields,
                log: Vec::new(),
                mp1_log: Vec::new(),
                reject_writes: false,
                reject_get: false,
            }
        }

        fn exists(&self, mask: u32) -> bool {
            let ccd = (mask >> 28) as usize;
            let slot = (mask >> 20) & 0xF;
            ccd < self.disabled.len() && slot < 8 && self.disabled[ccd] & (1 << slot) == 0
        }

        fn store_margin(&mut self, arg: u32) -> Result<[u32; 6], String> {
            let mask = arg & 0xFFF0_0000;
            if self.reject_writes {
                return Err("SMU odrzuciło polecenie".into());
            }
            if !self.exists(mask) {
                return Err("nie ma takiego rdzenia".into());
            }
            self.margins
                .insert(mask, (arg & 0xFFFF) as u16 as i16 as i32);
            Ok(one(0))
        }
    }

    impl Smu for Fake {
        fn command(&mut self, cmd: u32, args: [u32; 6]) -> Result<[u32; 6], String> {
            self.log.push((cmd, args[0]));
            let mask = args[0] & 0xFFF0_0000;
            let (set, get, ppt, set_scalar, get_scalar) = if self.zen3 {
                (0xFFFF, 0x7C, 0x53, 0x58, 0x6C)
            } else {
                (0x06, 0xD5, 0x56, 0x5B, 0x6D)
            };
            match cmd {
                c if c == set => self.store_margin(args[0]),
                c if c == get && self.reject_get => Err("SMU nie zna polecenia".into()),
                c if c == get && self.exists(mask) => {
                    Ok(one(*self.margins.get(&mask).unwrap_or(&0) as u32))
                }
                c if c == get => Err("nie ma takiego rdzenia".into()),
                c if c == set_scalar => {
                    self.scalar = args[0] as f32 / 100.0;
                    Ok(one(0))
                }
                c if c == get_scalar => Ok(one(self.scalar.to_bits())),
                c if (ppt..ppt + 3).contains(&c) => {
                    let field = self.limit_fields[(c - ppt) as usize];
                    self.table[field] = args[0] as f32 / 1000.0;
                    Ok(one(0))
                }
                _ => Err(format!("nieznane polecenie 0x{cmd:X}")),
            }
        }

        fn mp1_command(
            &mut self,
            mailbox: Mailbox,
            cmd: u32,
            args: [u32; 6],
        ) -> Result<[u32; 6], String> {
            assert_eq!(mailbox, MP1_ZEN3);
            self.mp1_log.push((cmd, args[0]));
            if self.zen3 && cmd == 0x35 {
                self.store_margin(args[0])
            } else {
                Err(format!("nieznane polecenie MP1 0x{cmd:X}"))
            }
        }

        fn read_smn(&mut self, address: u32) -> Result<u32, String> {
            let ccd = ((address - self.fuse_base) / CORE_FUSE_CCD_STRIDE) as usize;
            Ok(self.disabled.get(ccd).copied().unwrap_or(0xFFFF_FFFF) | 0xAB00)
        }

        fn pm_table(&mut self) -> Result<(u32, Vec<f32>), String> {
            Ok((self.version, self.table.clone()))
        }
    }

    fn zen4() -> (Commands, CurveCommands) {
        let cmds = commands(Generation::Zen4);
        (cmds, cmds.curve.unwrap())
    }

    fn zen3() -> (Commands, CurveCommands) {
        let cmds = commands(Generation::Zen3);
        (cmds, cmds.curve.unwrap())
    }

    #[test]
    fn detects_generations() {
        assert_eq!(Generation::detect(0x19, 0x21), Some(Generation::Zen3));
        assert_eq!(Generation::detect(0x19, 0x61), Some(Generation::Zen4));
        assert_eq!(Generation::detect(0x1A, 0x44), Some(Generation::Zen5));
        assert_eq!(Generation::detect(0x17, 0x71), None);
        assert_eq!(
            commands(Generation::Zen5).curve.unwrap().core_fuse,
            0x304A_03DC
        );
        // Zen 3: zapis przez MP1, węższy zakres
        let curve = commands(Generation::Zen3).curve.unwrap();
        assert_eq!(
            (curve.set_core, curve.set_mailbox, curve.get_core),
            (0x35, Some(MP1_ZEN3), 0x7C)
        );
        assert_eq!((curve.min, curve.max), (-30, 30));
        assert_eq!(commands(Generation::Zen4).curve.unwrap().set_mailbox, None);
    }

    #[test]
    fn encodes_mask_and_margin() {
        let core = Core {
            index: 9,
            ccd: 1,
            slot: 3,
        };
        assert_eq!(core.mask(), 0x1030_0000);
        assert_eq!(margin_arg(-30), 0xFFE2);
        assert_eq!(margin_arg(15), 0x000F);
        assert_eq!(margin_arg(0), 0);
    }

    #[test]
    fn maps_eight_core_cpu() {
        let (_, curve) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        let cores = core_map(&mut cpu, curve, 8).unwrap();
        assert_eq!(cores.len(), 8);
        assert_eq!(
            cores[7],
            Core {
                index: 7,
                ccd: 0,
                slot: 7
            }
        );
    }

    #[test]
    fn maps_twelve_core_cpu_with_disabled_slots() {
        let (_, curve) = zen4();
        // 2 x 6 rdzeni; w CCD0 wyłączone miejsca 6 i 7, w CCD1 miejsca 0 i 5
        let mut cpu = Fake::new(Generation::Zen4, &[0xC0, 0x21]);
        let cores = core_map(&mut cpu, curve, 12).unwrap();
        assert_eq!(cores.len(), 12);
        assert_eq!(
            cores[5],
            Core {
                index: 5,
                ccd: 0,
                slot: 5
            }
        );
        // siódmy rdzeń systemu to pierwsze WŁĄCZONE miejsce drugiego CCD
        assert_eq!(
            cores[6],
            Core {
                index: 6,
                ccd: 1,
                slot: 1
            }
        );
        assert_eq!(
            cores[11],
            Core {
                index: 11,
                ccd: 1,
                slot: 7
            }
        );
    }

    #[test]
    fn rejects_core_map_that_disagrees_with_cpuid() {
        let (_, curve) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0xC0]);
        assert!(core_map(&mut cpu, curve, 8).is_err());
        // brak drugiego CCD w odczycie
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        assert!(core_map(&mut cpu, curve, 16).is_err());
        assert!(core_map(&mut cpu, curve, 0).is_err());
        assert!(core_map(&mut cpu, curve, 32).is_err());
    }

    #[test]
    fn sets_and_verifies_margin_on_the_right_core() {
        let (_, curve) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0xC0, 0xC0]);
        let cores = core_map(&mut cpu, curve, 12).unwrap();
        assert_eq!(set_margin(&mut cpu, curve, cores[7], -25).unwrap(), -25);
        // zapis: RSMU 0x06, CCD1 miejsce 1, margines -25; nic przez MP1
        assert!(cpu.log.contains(&(0x06, 0x1010_0000 | 0xFFE7)));
        assert!(cpu.mp1_log.is_empty());
        assert_eq!(get_margin(&mut cpu, curve, cores[7]).unwrap(), -25);
        // pozostałe rdzenie bez zmian
        assert_eq!(get_margin(&mut cpu, curve, cores[6]).unwrap(), 0);
        assert_eq!(get_margin(&mut cpu, curve, cores[1]).unwrap(), 0);
        assert_eq!(set_margin(&mut cpu, curve, cores[0], 10).unwrap(), 10);
    }

    #[test]
    fn zen3_writes_through_mp1_and_reads_through_rsmu() {
        let (_, curve) = zen3();
        // 5600X: jeden CCD, sześć rdzeni
        let mut cpu = Fake::new(Generation::Zen3, &[0xC0]);
        let cores = core_map(&mut cpu, curve, 6).unwrap();
        assert_eq!(set_margin(&mut cpu, curve, cores[3], -20).unwrap(), -20);
        // zapis poszedł skrzynką MP1 poleceniem 0x35
        assert_eq!(cpu.mp1_log, [(0x35, 0x0030_0000 | 0xFFEC)]);
        // RSMU dostało tylko odczyty 0x7C: przed zapisem i po nim
        assert_eq!(cpu.log, [(0x7C, 0x0030_0000), (0x7C, 0x0030_0000)]);
        // zakres Zen 3 to -30..30
        assert!(set_margin(&mut cpu, curve, cores[3], -31).is_err());
        assert_eq!(cpu.mp1_log.len(), 1);
    }

    #[test]
    fn zen3_does_not_write_when_readback_is_unavailable() {
        let (_, curve) = zen3();
        let mut cpu = Fake::new(Generation::Zen3, &[0x00]);
        cpu.reject_get = true;
        let core = Core {
            index: 0,
            ccd: 0,
            slot: 0,
        };
        assert!(set_margin(&mut cpu, curve, core, -10).is_err());
        assert!(
            cpu.mp1_log.is_empty(),
            "bez działającego odczytu nie wolno zapisywać"
        );
    }

    #[test]
    fn refuses_margin_outside_range_without_touching_cpu() {
        let (_, curve) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        let core = Core {
            index: 0,
            ccd: 0,
            slot: 0,
        };
        assert!(set_margin(&mut cpu, curve, core, -51).is_err());
        assert!(set_margin(&mut cpu, curve, core, 31).is_err());
        assert!(cpu.log.is_empty());
    }

    #[test]
    fn reports_rejected_write() {
        let (_, curve) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        cpu.reject_writes = true;
        let core = Core {
            index: 0,
            ccd: 0,
            slot: 0,
        };
        assert!(set_margin(&mut cpu, curve, core, -10).is_err());
        assert_eq!(get_margin(&mut cpu, curve, core).unwrap(), 0);
    }

    #[test]
    fn pbo_limits_use_milliunits_and_ranges() {
        let (cmds, _) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        set_ppt(&mut cpu, cmds, 142).unwrap();
        set_tdc(&mut cpu, cmds, 110).unwrap();
        set_edc(&mut cpu, cmds, 170).unwrap();
        assert_eq!(cpu.log, [(0x56, 142_000), (0x57, 110_000), (0x58, 170_000)]);
        assert!(set_ppt(&mut cpu, cmds, 5).is_err());
        assert!(set_edc(&mut cpu, cmds, 5000).is_err());
        assert_eq!(cpu.log.len(), 3);
    }

    #[test]
    fn reads_limits_from_known_layouts() {
        let mut cpu = Fake::new(Generation::Zen5, &[0x00]);
        let l = layout(Generation::Zen5, 0x62_0105);
        assert!(l.verified);
        let limits = read_limits(&mut cpu, l).unwrap();
        assert_eq!(
            (limits.ppt_w, limits.tdc_a, limits.edc_a),
            (Some(142.0), Some(95.0), Some(140.0))
        );

        let mut cpu = Fake::new(Generation::Zen3, &[0x00]);
        let l = layout(Generation::Zen3, 0x38_0804);
        assert_eq!((l.ppt, l.tdc, l.edc, l.verified), (0, 2, 8, true));
        assert_eq!(read_limits(&mut cpu, l).unwrap().edc_a, Some(140.0));
        // nieznana wersja: ten sam układ, ale niepotwierdzony
        assert!(!layout(Generation::Zen4, 0x54_0004).verified);
        assert!(!layout(Generation::Zen3, 0x38_0905).verified);
    }

    #[test]
    fn implausible_table_values_are_not_shown_as_limits() {
        let mut cpu = Fake::new(Generation::Zen5, &[0x00]);
        cpu.table[2] = f32::NAN;
        cpu.table[8] = 0.0;
        cpu.table[63] = 1.0e9;
        let limits = read_limits(&mut cpu, layout(Generation::Zen5, 0x62_0105)).unwrap();
        assert_eq!(
            (limits.ppt_w, limits.tdc_a, limits.edc_a),
            (None, None, None)
        );
    }

    #[test]
    fn write_confirms_layout_or_finds_the_real_field() {
        let (cmds, _) = zen4();
        // układ zgodny z tabelą
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        let mut l = layout(Generation::Zen4, 0x54_0004);
        set_ppt(&mut cpu, cmds, 123).unwrap();
        assert_eq!(
            confirm_limit(&mut cpu, &mut l, Limit::Ppt, 123).unwrap(),
            Readback::Confirmed
        );

        // tabela innej wersji: EDC siedzi w polu 61, a nie 63
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        cpu.table[63] = 7.0;
        cpu.limit_fields[2] = 61;
        let mut l = layout(Generation::Zen4, 0x54_0004);
        set_edc(&mut cpu, cmds, 177).unwrap();
        assert_eq!(
            confirm_limit(&mut cpu, &mut l, Limit::Edc, 177).unwrap(),
            Readback::Relocated(61)
        );
        assert_eq!(l.edc, 61);
        // po poprawce odczyt pokazuje właściwą wartość
        assert_eq!(read_limits(&mut cpu, l).unwrap().edc_a, Some(177.0));

        // wartość w dwóch polach naraz albo w żadnym: brak potwierdzenia, układ bez zmian
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        cpu.limit_fields[1] = 40;
        cpu.table[41] = 88.0;
        let mut l = layout(Generation::Zen4, 0x54_0004);
        set_tdc(&mut cpu, cmds, 88).unwrap();
        assert_eq!(
            confirm_limit(&mut cpu, &mut l, Limit::Tdc, 88).unwrap(),
            Readback::Unconfirmed
        );
        assert_eq!(l.tdc, 8);
    }

    #[test]
    fn scalar_round_trip() {
        let (cmds, _) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0x00]);
        assert_eq!(get_scalar(&mut cpu, cmds).unwrap(), 1.0);
        assert_eq!(set_scalar(&mut cpu, cmds, 7).unwrap(), 7.0);
        assert!(cpu.log.contains(&(0x5B, 700)));
        assert!(set_scalar(&mut cpu, cmds, 0).is_err());
        assert!(set_scalar(&mut cpu, cmds, 11).is_err());
    }

    #[test]
    fn reads_whole_state() {
        let (cmds, curve) = zen4();
        let mut cpu = Fake::new(Generation::Zen4, &[0xC0]);
        let core2 = Core {
            index: 2,
            ccd: 0,
            slot: 2,
        };
        set_margin(&mut cpu, curve, core2, -15).unwrap();
        let state = read_state(&mut cpu, cmds, layout(Generation::Zen4, 0x62_0105), 6);
        let cores = state.cores.unwrap().unwrap();
        assert_eq!(cores.len(), 6);
        assert_eq!(cores[2].1.clone().unwrap(), -15);
        assert_eq!(state.scalar.unwrap(), 1.0);
        assert_eq!(state.limits.unwrap().ppt_w, Some(142.0));
    }

    /// Rejestry skrzynki z prostym modelem SMU: po wpisaniu polecenia
    /// odpowiedź pojawia się dopiero przy trzecim odczycie.
    struct Registers {
        values: HashMap<u32, u32>,
        pending: Option<(u32, usize)>,
        writes: Vec<(u32, u32)>,
        answer: u32,
    }

    fn run(reg: &mut Registers, cmd: u32, args: [u32; 6]) -> Result<[u32; 6], String> {
        let cell = std::cell::RefCell::new(reg);
        let mut read = |addr: u32| -> Result<u32, String> {
            let mut reg = cell.borrow_mut();
            if addr == MP1_ZEN3.rsp {
                if let Some((status, left)) = reg.pending {
                    if left == 0 {
                        reg.pending = None;
                        reg.values.insert(addr, status);
                        // SMU odsyła w pierwszym argumencie wartość + 1
                        let a0 = reg.values[&MP1_ZEN3.args];
                        reg.values.insert(MP1_ZEN3.args, a0 + 1);
                    } else {
                        reg.pending = Some((status, left - 1));
                    }
                }
            }
            Ok(*reg.values.get(&addr).unwrap_or(&0))
        };
        let mut write = |addr: u32, value: u32| -> Result<(), String> {
            let mut reg = cell.borrow_mut();
            reg.writes.push((addr, value));
            reg.values.insert(addr, value);
            if addr == MP1_ZEN3.msg {
                let answer = reg.answer;
                reg.pending = Some((answer, 2));
            }
            Ok(())
        };
        mailbox_transaction(&mut read, &mut write, MP1_ZEN3, cmd, args)
    }

    #[test]
    fn mailbox_follows_the_handshake_order() {
        let mut reg = Registers {
            values: HashMap::from([(MP1_ZEN3.rsp, 1)]),
            pending: None,
            writes: Vec::new(),
            answer: SMU_OK,
        };
        let out = run(&mut reg, 0x35, [10, 20, 30, 40, 50, 60]).unwrap();
        assert_eq!(out, [11, 20, 30, 40, 50, 60]);
        // kolejność zapisów: zerowanie odpowiedzi, sześć argumentów, numer polecenia
        assert_eq!(reg.writes[0], (MP1_ZEN3.rsp, 0));
        for i in 0..6u32 {
            assert_eq!(
                reg.writes[1 + i as usize],
                (MP1_ZEN3.args + 4 * i, 10 * (i + 1))
            );
        }
        assert_eq!(reg.writes[7], (MP1_ZEN3.msg, 0x35));
        assert_eq!(reg.writes.len(), 8);
    }

    #[test]
    fn mailbox_reports_busy_and_rejected() {
        // odpowiedź 0 na starcie: poprzednie polecenie wciąż trwa - nic nie zapisujemy
        let mut reg = Registers {
            values: HashMap::new(),
            pending: None,
            writes: Vec::new(),
            answer: SMU_OK,
        };
        assert!(run(&mut reg, 0x35, [0; 6]).is_err());
        assert!(reg.writes.is_empty());
        // SMU nie zna polecenia
        let mut reg = Registers {
            values: HashMap::from([(MP1_ZEN3.rsp, 1)]),
            pending: None,
            writes: Vec::new(),
            answer: 0xFE,
        };
        let err = run(&mut reg, 0x99, [0; 6]).unwrap_err();
        assert!(err.contains("nie zna"), "{err}");
    }
}
