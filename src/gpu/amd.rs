//! Karty AMD Radeon przez ADLX (`amdadlx64.dll`) - oficjalne API sterownika
//! Radeon, to samo, z którego korzysta AMD Software: Adrenalin Edition.
//! Wiązanie i numery pozycji metod: `crate::radeon::adlx` (nagłówki C ADLX
//! SDK: IGPUTuning.h, IGPUManualGFXTuning.h, IGPUManualVRAMTuning.h,
//! IGPUManualFanTuning.h, IPerformanceMonitoring.h).
//!
//! Różnica względem Nvidii: sterownik AMD nie przyjmuje offsetów, tylko
//! bezwzględne wartości maksymalnego zegara rdzenia i pamięci (w MHz).
//! Krzywa V/F nie jest dostępna.
//!
//! Wątki: interfejs, odczyt czujników (sampler) i sterowanie wentylatorem
//! mają po własnym `Amd`, ale wszystkie korzystają z jednej sesji ADLX
//! procesu (`Adlx::shared`) i każde wywołanie ADLX robią pod `Adlx::lock`.

use super::backend::*;
use crate::radeon::adlx::{
    describe, fan, fan_state, gfx, gpu, list, metrics, metrics_support, perf, state, system,
    tuning, vram, Adlx, Obj,
};
use std::sync::{Arc, Mutex};

const AMD_VENDOR_ID: &str = "1002";

struct Card {
    gpu: Obj,
    name: String,
    /// Klucz karty w pamięci procesu (`UniqueId` ADLX, inaczej pozycja na
    /// liście) - wspólny dla egzemplarzy `Amd` z różnych wątków.
    key: i32,
    /// Układ zintegrowany w procesorze.
    integrated: bool,
    /// Najwyższe obroty wentylatora według sterownika (do przeliczenia na %).
    fan_max_rpm: Option<i32>,
}

pub struct Amd {
    cards: Vec<Card>,
    /// IADLXGPUTuningServices
    tuning: Option<Obj>,
    /// IADLXPerformanceMonitoringServices
    perf: Option<Obj>,
    _adlx: Arc<Adlx>,
}

fn fail(what: &str, code: i32) -> String {
    format!("{what}: {}", describe(code))
}

fn bad_index() -> String {
    crate::i18n::tr("Nieprawidłowy indeks karty", "Invalid card index").to_string()
}

// ------------------------- wentylator: stan sprzed -------------------------
// Tryb automatyczny = przywrócenie krzywej wentylatora (i ZeroRPM / obrotów
// docelowych) odczytanych przed pierwszą zmianą w tym procesie. Nie używamy
// `IADLXGPUTuningServices::ResetToFactory`, bo przywraca fabryczne także
// zegary, napięcia i limit mocy ustawione przez użytkownika.

#[derive(Clone)]
struct FanSnapshot {
    /// Punkty krzywej: (wysterowanie %, temperatura °C).
    curve: Vec<(i32, i32)>,
    zero_rpm: Option<bool>,
    target_rpm: Option<i32>,
}

/// Stan wentylatorów sprzed zmian, wspólny dla wątków (klucz karty).
static FAN_SNAPSHOTS: Mutex<Vec<(i32, FanSnapshot)>> = Mutex::new(Vec::new());

/// Pierwsza odczytana wartość zegara (klucz karty, rdzeń = 0 / pamięć = 1) -
/// wartość "domyślna", gdy sterownik nie podaje fabrycznej.
static FIRST_CLOCK: Mutex<Vec<((i32, u8), i32)>> = Mutex::new(Vec::new());

fn first_clock(key: i32, domain: Domain, value: i32) -> i32 {
    let id = (key, (domain == Domain::Memory) as u8);
    let mut seen = FIRST_CLOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, v)) = seen.iter().find(|(k, _)| *k == id) {
        return *v;
    }
    seen.push((id, value));
    value
}

// ------------------------------ zegary --------------------------------------

/// Pozycje metod jednej domeny zegara (rdzeń: Graphics, pamięć: VRAM).
struct Spec {
    supported: usize,
    get: usize,
    iid1: &'static str,
    iid2: &'static str,
    iid2_1: &'static str,
    v1_ranges: usize,
    v1_states: usize,
    v1_empty: usize,
    v1_valid: usize,
    v1_set: usize,
    v2_range: usize,
    v2_get: usize,
    v2_set: usize,
    v2_1_default: usize,
}

const GFX: Spec = Spec {
    supported: tuning::IS_SUPPORTED_MANUAL_GFX,
    get: tuning::GET_MANUAL_GFX,
    iid1: gfx::IID1,
    iid2: gfx::IID2,
    iid2_1: gfx::IID2_1,
    v1_ranges: gfx::V1_GET_RANGES,
    v1_states: gfx::V1_GET_STATES,
    v1_empty: gfx::V1_GET_EMPTY_STATES,
    v1_valid: gfx::V1_IS_VALID_STATES,
    v1_set: gfx::V1_SET_STATES,
    v2_range: gfx::V2_GET_MAX_FREQUENCY_RANGE,
    v2_get: gfx::V2_GET_MAX_FREQUENCY,
    v2_set: gfx::V2_SET_MAX_FREQUENCY,
    v2_1_default: gfx::V2_1_GET_MAX_FREQUENCY_DEFAULT,
};

const VRAM: Spec = Spec {
    supported: tuning::IS_SUPPORTED_MANUAL_VRAM,
    get: tuning::GET_MANUAL_VRAM,
    iid1: vram::IID1,
    iid2: vram::IID2,
    iid2_1: vram::IID2_1,
    v1_ranges: vram::V1_GET_RANGES,
    v1_states: vram::V1_GET_STATES,
    v1_empty: vram::V1_GET_EMPTY_STATES,
    v1_valid: vram::V1_IS_VALID_STATES,
    v1_set: vram::V1_SET_STATES,
    v2_range: vram::V2_GET_MAX_FREQUENCY_RANGE,
    v2_get: vram::V2_GET_MAX_FREQUENCY,
    v2_set: vram::V2_SET_MAX_FREQUENCY,
    v2_1_default: vram::V2_1_GET_MAX_FREQUENCY_DEFAULT,
};

fn spec(domain: Domain) -> &'static Spec {
    match domain {
        Domain::Core => &GFX,
        Domain::Memory => &VRAM,
    }
}

/// Interfejs strojenia jednej domeny: nowszy (min/max) albo starszy (stany).
enum Clock {
    /// IADLXManualGraphicsTuning2 / IADLXManualVRAMTuning2 i obiekt bazowy
    /// (do zapytania o wersję 2_1).
    V2(Obj, Obj),
    /// IADLXManualGraphicsTuning1 / IADLXManualVRAMTuning1.
    V1(Obj),
}

/// Stany (częstotliwość MHz, napięcie mV) z listy IADLXManualTuningStateList.
fn read_states(states: &Obj) -> Result<Vec<(i32, i32)>> {
    let mut out = Vec::new();
    for i in 0..states.count(list::SIZE) {
        let s = states
            .at(list::AT_TYPED, i)
            .map_err(|e| fail("ManualTuningStateList::At", e))?;
        let f = s
            .int(state::GET_FREQUENCY)
            .map_err(|e| fail("GetFrequency", e))?;
        let v = s.int(state::GET_VOLTAGE).map_err(|e| fail("GetVoltage", e))?;
        out.push((f, v));
    }
    Ok(out)
}

// -------------------------------- API ---------------------------------------

impl Amd {
    pub fn load() -> Result<Self> {
        let adlx = Adlx::shared()?;
        let _g = Adlx::lock();
        let sys = adlx.system();
        let gpus = sys
            .get(system::GET_GPUS)
            .map_err(|e| fail("GetGPUs", e))?;
        let tuning = sys.get(system::GET_GPU_TUNING_SERVICES).ok();
        let perf = sys.get(system::GET_PERFORMANCE_MONITORING_SERVICES).ok();

        let mut cards: Vec<Card> = Vec::new();
        for i in 0..gpus.count(list::SIZE) {
            let Ok(g) = gpus.at(list::AT_TYPED, i) else {
                continue;
            };
            let amd = g
                .text(gpu::VENDOR_ID)
                .map(|v| v.trim().eq_ignore_ascii_case(AMD_VENDOR_ID))
                .unwrap_or(true);
            if !amd {
                continue;
            }
            let name = g
                .text(gpu::NAME)
                .ok()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("AMD GPU {}", cards.len() + 1));
            let key = g.int(gpu::UNIQUE_ID).unwrap_or(i as i32);
            let integrated = g.int(gpu::TYPE) == Ok(gpu::TYPE_INTEGRATED);
            let fan_max_rpm = perf
                .as_ref()
                .and_then(|p| p.get_for(perf::GET_SUPPORTED_GPU_METRICS, &g).ok())
                .filter(|s| s.flag(metrics_support::IS_SUPPORTED_GPU_FAN_SPEED) == Ok(true))
                .and_then(|s| s.int_pair(metrics_support::GET_GPU_FAN_SPEED_RANGE).ok())
                .map(|(_, max)| max)
                .filter(|max| *max > 0);
            cards.push(Card {
                gpu: g,
                name,
                key,
                integrated,
                fan_max_rpm,
            });
        }
        drop(gpus);
        if cards.is_empty() {
            return Err(
                crate::i18n::tr("Nie znaleziono żadnej karty AMD", "No AMD card found").to_string(),
            );
        }
        Ok(Self {
            cards,
            tuning,
            perf,
            _adlx: adlx.clone(),
        })
    }

    fn card(&self, gpu: usize) -> Result<&Card> {
        self.cards.get(gpu).ok_or_else(bad_index)
    }

    fn services(&self) -> Result<&Obj> {
        self.tuning.as_ref().ok_or_else(|| {
            crate::i18n::tr(
                "Sterownik nie udostępnia usługi strojenia karty (ADLX GPUTuningServices).",
                "The driver does not provide the GPU tuning service (ADLX GPUTuningServices).",
            )
            .to_string()
        })
    }

    /// Odczyty czujników; wymaga `Adlx::lock`.
    fn read_monitor(&self, gpu: usize) -> Monitor {
        let mut m = Monitor::default();
        let (Ok(c), Some(p)) = (self.card(gpu), self.perf.as_ref()) else {
            return m;
        };
        let Ok(x) = p.get_for(perf::GET_CURRENT_GPU_METRICS, &c.gpu) else {
            return m;
        };
        let real = |slot: usize| x.double(slot).ok().filter(|v| v.is_finite());
        let int = |slot: usize| x.int(slot).ok();
        m.temp_c = real(metrics::GPU_TEMPERATURE).map(|v| v.round() as i32);
        m.hotspot_c = real(metrics::GPU_HOTSPOT_TEMPERATURE)
            .filter(|v| *v > 0.0)
            .map(|v| v as f32);
        m.core_mhz = int(metrics::GPU_CLOCK_SPEED)
            .filter(|v| *v >= 0)
            .map(|v| v as f32);
        m.mem_mhz = int(metrics::GPU_VRAM_CLOCK_SPEED)
            .filter(|v| *v >= 0)
            .map(|v| v as f32);
        m.voltage_mv = int(metrics::GPU_VOLTAGE)
            .filter(|v| *v > 0)
            .map(|v| v as f32);
        m.usage_pct = real(metrics::GPU_USAGE).map(|v| v.clamp(0.0, 100.0).round() as u32);
        m.fan_rpm = int(metrics::GPU_FAN_SPEED)
            .filter(|v| *v >= 0)
            .map(|v| v as u32);
        // ADLX podaje tylko obroty; procent liczymy względem najwyższych
        // obrotów z zakresu sterownika.
        m.fan_pct = match (m.fan_rpm, c.fan_max_rpm) {
            (Some(rpm), Some(max)) => {
                Some(((rpm as f64 * 100.0 / max as f64).round() as u32).min(100))
            }
            _ => None,
        };
        // Układ zintegrowany w procesorze (typ z ADLX albo brak odczytu
        // wentylatora) podaje tu moc całego procesora, np. 157 W na Ryzen 9
        // 9950X3D - to nie jest pobór karty, więc go pomijamy (moc procesora
        // pokazuje zakładka POWER).
        let integrated = c.integrated || (m.fan_rpm.is_none() && m.fan_pct.is_none());
        // Moc całej karty; starsze karty podają tylko moc układu.
        m.power_w = [metrics::GPU_TOTAL_BOARD_POWER, metrics::GPU_POWER]
            .into_iter()
            .filter_map(real)
            .find(|v| *v > 0.0)
            .filter(|_| !integrated)
            .map(|v| v as f32);
        m
    }

    /// IADLXManualFanTuning karty; wymaga `Adlx::lock`.
    fn fan_tuning(&self, c: &Card) -> Result<Obj> {
        let s = self.services()?;
        if s.flag_for(tuning::IS_SUPPORTED_MANUAL_FAN, &c.gpu) != Ok(true) {
            return Err(crate::i18n::tr(
                "Ta karta nie pozwala sterować wentylatorem przez sterownik.",
                "This card does not allow fan control through the driver.",
            )
            .to_string());
        }
        let base = s
            .get_for(tuning::GET_MANUAL_FAN, &c.gpu)
            .map_err(|e| fail("GetManualFanTuning", e))?;
        base.query(fan::IID).ok_or_else(|| fail("IADLXManualFanTuning", 12))
    }

    /// Bieżąca krzywa wentylatora: (wysterowanie %, temperatura °C).
    fn read_fan_curve(ft: &Obj) -> Result<Vec<(i32, i32)>> {
        let states = ft
            .get(fan::GET_STATES)
            .map_err(|e| fail("GetFanTuningStates", e))?;
        let mut out = Vec::new();
        for i in 0..states.count(list::SIZE) {
            let s = states
                .at(list::AT_TYPED, i)
                .map_err(|e| fail("ManualFanTuningStateList::At", e))?;
            let speed = s
                .int(fan_state::GET_FAN_SPEED)
                .map_err(|e| fail("GetFanSpeed", e))?;
            let temp = s
                .int(fan_state::GET_TEMPERATURE)
                .map_err(|e| fail("GetTemperature", e))?;
            out.push((speed, temp));
        }
        Ok(out)
    }

    /// Zapis krzywej wentylatora jak w przykładzie AMD (ManualFanTuning):
    /// pusta lista od sterownika, wypełnienie, sprawdzenie, zapis.
    fn write_fan_curve(ft: &Obj, points: &[(i32, i32)]) -> Result<()> {
        let empty = ft
            .get(fan::GET_EMPTY_STATES)
            .map_err(|e| fail("GetEmptyFanTuningStates", e))?;
        if empty.count(list::SIZE) as usize != points.len() {
            return Err(crate::i18n::tr(
                "Sterownik zmienił liczbę punktów krzywej wentylatora.",
                "The driver changed the number of fan curve points.",
            )
            .to_string());
        }
        for (i, &(speed, temp)) in points.iter().enumerate() {
            let s = empty
                .at(list::AT_TYPED, i as u32)
                .map_err(|e| fail("ManualFanTuningStateList::At", e))?;
            s.set_int(fan_state::SET_FAN_SPEED, speed)
                .map_err(|e| fail("SetFanSpeed", e))?;
            s.set_int(fan_state::SET_TEMPERATURE, temp)
                .map_err(|e| fail("SetTemperature", e))?;
        }
        ft.validate(fan::IS_VALID_STATES, &empty).map_err(|(e, i)| {
            crate::trf!(
                "IsValidFanTuningStates: {} (punkt {i})",
                "IsValidFanTuningStates: {} (point {i})",
                describe(e)
            )
        })?;
        ft.call_with(fan::SET_STATES, &empty)
            .map_err(|e| fail("SetFanTuningStates", e))
    }

    /// Stałe wysterowanie `p` %; wymaga `Adlx::lock`.
    fn fan_fixed(&self, c: &Card, p: u32) -> Result<()> {
        let ft = self.fan_tuning(c)?;
        let curve = Self::read_fan_curve(&ft).unwrap_or_default();
        let zero_supported = ft.flag(fan::IS_SUPPORTED_ZERO_RPM) == Ok(true);
        let target_supported = ft.flag(fan::IS_SUPPORTED_TARGET_FAN_SPEED) == Ok(true);

        // Stan sprzed pierwszej zmiany - do powrotu w tryb automatyczny.
        {
            let mut snaps = FAN_SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
            if !snaps.iter().any(|(k, _)| *k == c.key) {
                let snap = FanSnapshot {
                    curve: curve.clone(),
                    zero_rpm: zero_supported
                        .then(|| ft.flag(fan::GET_ZERO_RPM).ok())
                        .flatten(),
                    target_rpm: target_supported
                        .then(|| ft.int(fan::GET_TARGET_FAN_SPEED).ok())
                        .flatten(),
                };
                snaps.push((c.key, snap));
            }
        }

        // ZeroRPM zatrzymałby wentylator poniżej progu temperatury mimo
        // ustawionego wysterowania; zostaje włączony tylko dla 0 %.
        if zero_supported {
            ft.set_flag(fan::SET_ZERO_RPM, p == 0)
                .map_err(|e| fail("SetZeroRPMState", e))?;
        }

        if !curve.is_empty() {
            // Płaska krzywa: każdy punkt z tą samą wartością (najbliższą
            // dozwolonej), temperatury punktów bez zmian.
            let (speed, _) = ft
                .ranges(fan::GET_RANGES)
                .map_err(|e| fail("GetFanTuningRanges", e))?;
            let v = (p.min(100) as i32).clamp(speed.min, speed.max.max(speed.min));
            let flat: Vec<(i32, i32)> = curve.iter().map(|&(_, t)| (v, t)).collect();
            Self::write_fan_curve(&ft, &flat)
        } else if target_supported {
            // Starsze karty: tylko docelowe obroty (RPM) - procent zakresu.
            let r = ft
                .range(fan::GET_TARGET_FAN_SPEED_RANGE)
                .map_err(|e| fail("GetTargetFanSpeedRange", e))?;
            let rpm = r.min + ((r.max - r.min).max(0) as i64 * p.min(100) as i64 / 100) as i32;
            ft.set_int(fan::SET_TARGET_FAN_SPEED, rpm)
                .map_err(|e| fail("SetTargetFanSpeed", e))
        } else {
            Err(crate::i18n::tr(
                "Ta karta nie pozwala sterować wentylatorem przez sterownik.",
                "This card does not allow fan control through the driver.",
            )
            .to_string())
        }
    }

    /// Powrót do stanu sprzed zmian; wymaga `Adlx::lock`.
    fn fan_auto(&self, c: &Card) -> Result<()> {
        let snap = {
            let mut snaps = FAN_SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
            match snaps.iter().position(|(k, _)| *k == c.key) {
                Some(i) => snaps.remove(i).1,
                // Ten proces nie zmieniał wentylatora - nie ma czego przywracać.
                None => return Ok(()),
            }
        };
        let restore = || -> Result<()> {
            let ft = self.fan_tuning(c)?;
            if !snap.curve.is_empty() {
                Self::write_fan_curve(&ft, &snap.curve)?;
            }
            if let Some(t) = snap.target_rpm {
                ft.set_int(fan::SET_TARGET_FAN_SPEED, t)
                    .map_err(|e| fail("SetTargetFanSpeed", e))?;
            }
            if let Some(z) = snap.zero_rpm {
                ft.set_flag(fan::SET_ZERO_RPM, z)
                    .map_err(|e| fail("SetZeroRPMState", e))?;
            }
            Ok(())
        };
        let result = restore();
        if result.is_err() {
            // Następna próba przywróci ten sam stan.
            let mut snaps = FAN_SNAPSHOTS.lock().unwrap_or_else(|e| e.into_inner());
            snaps.push((c.key, snap.clone()));
        }
        result
    }

    /// Interfejs strojenia domeny; `None`, gdy karta go nie ma. Wymaga
    /// `Adlx::lock`.
    fn clock(&self, c: &Card, spec: &Spec) -> Result<Option<Clock>> {
        let s = self.services()?;
        if s.flag_for(spec.supported, &c.gpu) != Ok(true) {
            return Ok(None);
        }
        let base = s
            .get_for(spec.get, &c.gpu)
            .map_err(|e| fail("GetManualTuning", e))?;
        if let Some(v2) = base.query(spec.iid2) {
            return Ok(Some(Clock::V2(v2, base)));
        }
        Ok(base.query(spec.iid1).map(Clock::V1))
    }

    /// Zakres i bieżący maksymalny zegar domeny (wartości w kHz).
    fn clock_info(&self, c: &Card, domain: Domain) -> Result<Option<OffsetInfo>> {
        let spec = spec(domain);
        let Some(clock) = self.clock(c, spec)? else {
            return Ok(None);
        };
        let (min, max, value, default) = match &clock {
            Clock::V2(t, base) => {
                let r = t.range(spec.v2_range).map_err(|e| fail("GetMaxFrequencyRange", e))?;
                let v = t.int(spec.v2_get).map_err(|e| fail("GetMaxFrequency", e))?;
                let d = base
                    .query(spec.iid2_1)
                    .and_then(|t| t.int(spec.v2_1_default).ok())
                    .filter(|d| *d > 0);
                (r.min, r.max, v, d)
            }
            Clock::V1(t) => {
                let (r, _) = t.ranges(spec.v1_ranges).map_err(|e| fail("GetTuningRanges", e))?;
                let states = t.get(spec.v1_states).map_err(|e| fail("GetTuningStates", e))?;
                let Some(&(top, _)) = read_states(&states)?.last() else {
                    return Ok(None);
                };
                (r.min, r.max, top, None)
            }
        };
        let first = first_clock(c.key, domain, value);
        Ok((max > min).then(|| OffsetInfo {
            value_khz: value.saturating_mul(1000),
            min_khz: min.saturating_mul(1000),
            max_khz: max.saturating_mul(1000),
            default_khz: default.unwrap_or(first).saturating_mul(1000),
        }))
    }

    /// Ustawia maksymalny zegar domeny (MHz); wymaga `Adlx::lock`.
    fn set_clock(&self, c: &Card, domain: Domain, mhz: i32) -> Result<()> {
        let spec = spec(domain);
        let label = match domain {
            Domain::Core => crate::i18n::tr("zegar rdzenia", "core clock"),
            Domain::Memory => crate::i18n::tr("zegar pamięci", "memory clock"),
        };
        let not_allowed = || {
            crate::trf!(
                "Ta karta nie pozwala zmieniać ustawienia: {label}",
                "This card does not allow changing: {label}"
            )
        };
        let info = self.clock_info(c, domain)?.ok_or_else(not_allowed)?;
        let (min, max) = (info.min_khz / 1000, info.max_khz / 1000);
        if mhz < min || mhz > max {
            return Err(crate::trf!(
                "{label}: {mhz} MHz poza zakresem sterownika ({min}-{max} MHz)",
                "{label}: {mhz} MHz is outside the driver range ({min}-{max} MHz)"
            ));
        }
        match self.clock(c, spec)?.ok_or_else(not_allowed)? {
            Clock::V2(t, _) => t
                .set_int(spec.v2_set, mhz)
                .map_err(|e| fail("SetMaxFrequency", e)),
            Clock::V1(t) => {
                // Najwyższy stan dostaje nową częstotliwość, niższe nie mogą
                // jej przekraczać; napięcia bez zmian (przykład AMD:
                // ManualGraphicsTuning - pusta lista, wypełnienie, sprawdzenie).
                let states = t.get(spec.v1_states).map_err(|e| fail("GetTuningStates", e))?;
                let current = read_states(&states)?;
                let empty = t
                    .get(spec.v1_empty)
                    .map_err(|e| fail("GetEmptyTuningStates", e))?;
                if current.is_empty() || empty.count(list::SIZE) as usize != current.len() {
                    return Err(not_allowed());
                }
                let last = current.len() - 1;
                for (i, &(f, v)) in current.iter().enumerate() {
                    let s = empty
                        .at(list::AT_TYPED, i as u32)
                        .map_err(|e| fail("ManualTuningStateList::At", e))?;
                    let f = if i == last { mhz } else { f.min(mhz) };
                    s.set_int(state::SET_FREQUENCY, f)
                        .map_err(|e| fail("SetFrequency", e))?;
                    s.set_int(state::SET_VOLTAGE, v)
                        .map_err(|e| fail("SetVoltage", e))?;
                }
                t.validate(spec.v1_valid, &empty).map_err(|(e, i)| {
                    crate::trf!(
                        "IsValidTuningStates: {} (stan {i})",
                        "IsValidTuningStates: {} (state {i})",
                        describe(e)
                    )
                })?;
                t.call_with(spec.v1_set, &empty)
                    .map_err(|e| fail("SetTuningStates", e))
            }
        }
    }
}

impl Drop for Amd {
    fn drop(&mut self) {
        // `Release` obiektów ADLX też pod blokadą.
        let _g = Adlx::lock();
        self.cards.clear();
        self.tuning = None;
        self.perf = None;
    }
}

impl Backend for Amd {
    fn gpus(&self) -> Vec<String> {
        self.cards.iter().map(|c| c.name.clone()).collect()
    }

    fn vendor(&self, _gpu: usize) -> Vendor {
        Vendor::Amd
    }

    fn monitor(&mut self, gpu: usize) -> Monitor {
        let _g = Adlx::lock();
        self.read_monitor(gpu)
    }

    fn fan(&mut self, gpu: usize) -> Option<FanStatus> {
        let m = self.monitor(gpu);
        (m.fan_rpm.is_some() || m.fan_pct.is_some()).then_some(FanStatus {
            rpm: m.fan_rpm,
            pct: m.fan_pct,
        })
    }

    fn fan_controllable(&mut self, gpu: usize) -> bool {
        let _g = Adlx::lock();
        let Ok(c) = self.card(gpu) else {
            return false;
        };
        let Ok(ft) = self.fan_tuning(c) else {
            return false;
        };
        let curve = Self::read_fan_curve(&ft).is_ok_and(|c| !c.is_empty())
            && ft.ranges(fan::GET_RANGES).is_ok();
        curve || ft.flag(fan::IS_SUPPORTED_TARGET_FAN_SPEED) == Ok(true)
    }

    fn set_fan(&mut self, gpu: usize, pct: Option<u32>) -> Result<()> {
        let _g = Adlx::lock();
        let c = self.card(gpu)?;
        match pct {
            Some(p) => self.fan_fixed(c, p),
            None => self.fan_auto(c),
        }
    }

    /// Dla AMD "offsety" to bezwzględne maksymalne zegary (wartości w kHz).
    fn offsets(&mut self, gpu: usize) -> Result<Offsets> {
        let _g = Adlx::lock();
        let c = self.card(gpu)?;
        let out = Offsets {
            core: self.clock_info(c, Domain::Core)?,
            mem: self.clock_info(c, Domain::Memory)?,
        };
        if out.core.is_none() && out.mem.is_none() {
            return Err(crate::i18n::tr(
                "Ta karta nie pozwala na ręczne strojenie zegarów przez sterownik (ADLX).",
                "This card does not allow manual clock tuning through the driver (ADLX).",
            )
            .to_string());
        }
        Ok(out)
    }

    fn set_offset(&mut self, gpu: usize, domain: Domain, khz: i32) -> Result<()> {
        let _g = Adlx::lock();
        let c = self.card(gpu)?;
        self.set_clock(c, domain, khz / 1000)
    }

    fn curve(&mut self, _gpu: usize) -> Result<Vec<CurvePoint>> {
        Err(crate::i18n::tr(
            "Krzywa V/F nie jest dostępna dla kart AMD.",
            "The V/F curve is not available for AMD cards.",
        )
        .to_string())
    }

    fn set_curve(&mut self, _gpu: usize, _deltas: &[(usize, i32)]) -> Result<()> {
        Err(crate::i18n::tr(
            "Krzywa V/F nie jest dostępna dla kart AMD.",
            "The V/F curve is not available for AMD cards.",
        )
        .to_string())
    }
}
