//! Wspólny interfejs dla kart Nvidia (NVAPI), AMD (ADL) i trybu demo.

pub type Result<T> = std::result::Result<T, String>;

#[derive(Default, Clone)]
pub struct Monitor {
    pub temp_c: Option<i32>,
    pub core_mhz: Option<f32>,
    pub mem_mhz: Option<f32>,
    pub voltage_mv: Option<f32>,
    pub usage_pct: Option<u32>,
    /// Najcieplejszy punkt karty (°C); nie każda karta go udostępnia.
    pub hotspot_c: Option<f32>,
    /// Obroty wentylatora (karty AMD).
    pub fan_rpm: Option<u32>,
    /// Wysterowanie wentylatora w procentach.
    pub fan_pct: Option<u32>,
    /// Pobór mocy karty w watach.
    pub power_w: Option<f32>,
    /// Dodatkowe odczyty (karty Nvidia: NVML i NVAPI).
    pub details: Details,
}

/// Szczegółowe odczyty karty do widoku czujników (jak w GPU-Z).
#[derive(Default, Clone)]
pub struct Details {
    /// Dokładna temperatura rdzenia (°C, z częścią ułamkową), gdy karta ją podaje.
    pub core_precise_c: Option<f32>,
    /// Karta nie udostępnia hotspotu w API sterownika (RTX 50).
    pub hotspot_hidden: bool,
    pub mem_temp_c: Option<f32>,
    pub mem_used_mb: Option<u32>,
    pub mem_total_mb: Option<u32>,
    pub pcie_tx_mbs: Option<f32>,
    pub pcie_rx_mbs: Option<f32>,
    pub pcie_gen: Option<u32>,
    pub pcie_width: Option<u32>,
    /// Obciążenie kontrolera pamięci, silnika wideo i magistrali (%).
    pub mem_ctrl_pct: Option<u32>,
    pub video_pct: Option<u32>,
    pub bus_pct: Option<u32>,
    pub encoder_pct: Option<u32>,
    pub decoder_pct: Option<u32>,
    pub video_mhz: Option<u32>,
    /// Moc samego układu GPU (W).
    pub chip_power_w: Option<f32>,
    /// Pobór mocy w % domyślnego limitu (TDP) i sam limit.
    pub power_pct_tdp: Option<f32>,
    pub tdp_w: Option<f32>,
    pub pstate: Option<u32>,
    /// Przyczyny ograniczania zegarów (tekst, np. "Idle", "Pwr").
    pub limit: Option<String>,
}

/// Stan wentylatora karty odczytany ze sterownika.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct FanStatus {
    pub rpm: Option<u32>,
    /// Wysterowanie w procentach.
    pub pct: Option<u32>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Vendor {
    Nvidia,
    Amd,
}

/// Ustawienie zegara. Dla Nvidii to offset względem wartości fabrycznej,
/// dla AMD bezwzględna wartość maksymalnego zegara.
#[derive(Clone, Copy)]
pub struct OffsetInfo {
    pub value_khz: i32,
    pub min_khz: i32,
    pub max_khz: i32,
    /// Wartość fabryczna (Nvidia: 0).
    pub default_khz: i32,
}

#[derive(Default, Clone)]
pub struct Offsets {
    pub core: Option<OffsetInfo>,
    pub mem: Option<OffsetInfo>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Domain {
    Core,
    Memory,
}

/// Jeden punkt krzywej napięcie/częstotliwość.
#[derive(Clone, Copy)]
pub struct CurvePoint {
    /// Indeks punktu w tabeli sterownika.
    pub index: usize,
    pub volt_uv: u32,
    /// Aktualna częstotliwość punktu (z uwzględnieniem offsetu).
    pub freq_khz: u32,
    /// Aktualnie ustawiony offset punktu.
    pub delta_khz: i32,
}

pub trait Backend {
    fn gpus(&self) -> Vec<String>;
    fn vendor(&self, _gpu: usize) -> Vendor {
        Vendor::Nvidia
    }
    fn monitor(&mut self, gpu: usize) -> Monitor;
    /// Sama temperatura karty (°C) - tańsza od pełnego `monitor`, wołana co
    /// sekundę przez sterowanie wentylatorami karty.
    fn temperature(&mut self, gpu: usize) -> Option<i32> {
        self.monitor(gpu).temp_c
    }
    /// Szczegółowe odczyty (PCIe TX/RX, moc układu GPU) - kosztowne, potrzebne
    /// tylko w widoku NVIDIA > Overclock > Informacje.
    fn set_detail(&mut self, _on: bool) {}
    /// Stan wentylatora karty; `None`, gdy sterownik go nie podaje.
    fn fan(&mut self, _gpu: usize) -> Option<FanStatus> {
        None
    }
    /// Czy wentylatorem karty da się sterować przez sterownik.
    fn fan_controllable(&mut self, _gpu: usize) -> bool {
        false
    }
    /// `Some(p)`: stałe wysterowanie p %; `None`: sterowanie automatyczne karty.
    /// Wszystkie wentylatory karty dostają to samo ustawienie.
    fn set_fan(&mut self, _gpu: usize, _pct: Option<u32>) -> Result<()> {
        Err(crate::i18n::tr(
            "sterownik karty nie udostępnia sterowania wentylatorem",
            "the card driver does not expose fan control",
        )
        .to_string())
    }
    fn offsets(&mut self, gpu: usize) -> Result<Offsets>;
    fn set_offset(&mut self, gpu: usize, domain: Domain, khz: i32) -> Result<()>;
    fn curve(&mut self, gpu: usize) -> Result<Vec<CurvePoint>>;
    /// `deltas`: pary (indeks punktu, offset w kHz).
    fn set_curve(&mut self, gpu: usize, deltas: &[(usize, i32)]) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Tryb demo: symulowana karta, do oglądania GUI bez dotykania sprzętu.
// ---------------------------------------------------------------------------

pub struct Demo {
    core_khz: i32,
    mem_khz: i32,
    deltas: Vec<i32>,
}

const DEMO_POINTS: usize = 80;

impl Demo {
    pub fn new() -> Self {
        Self {
            core_khz: 0,
            mem_khz: 0,
            deltas: vec![0; DEMO_POINTS],
        }
    }

    fn volt_uv(i: usize) -> u32 {
        450_000 + (i as u32) * 10_000
    }

    fn base_khz(i: usize) -> i64 {
        let t = i as f64 / (DEMO_POINTS - 1) as f64;
        // Lekko wypukła krzywa od ok. 500 do 2100 MHz.
        ((500.0 + 1600.0 * t.powf(0.8)) * 1000.0) as i64
    }

    fn freq_khz(&self, i: usize) -> i64 {
        Self::base_khz(i) + self.deltas[i] as i64 + self.core_khz as i64
    }
}

impl Backend for Demo {
    fn gpus(&self) -> Vec<String> {
        vec![crate::i18n::tr("Symulowana karta (demo)", "Simulated card (demo)").to_string()]
    }

    fn monitor(&mut self, _gpu: usize) -> Monitor {
        let i = 45; // 900 mV
        Monitor {
            temp_c: Some(56),
            core_mhz: Some(self.freq_khz(i) as f32 / 1000.0),
            mem_mhz: Some(7000.0 + self.mem_khz as f32 / 1000.0),
            voltage_mv: Some(Self::volt_uv(i) as f32 / 1000.0),
            usage_pct: Some(42),
            hotspot_c: Some(67.0),
            fan_rpm: Some(1180),
            fan_pct: Some(38),
            power_w: Some(142.0),
            details: Details {
                mem_temp_c: Some(62.0),
                mem_used_mb: Some(3412),
                mem_total_mb: Some(12227),
                pcie_tx_mbs: Some(210.0),
                pcie_rx_mbs: Some(1450.0),
                pcie_gen: Some(5),
                pcie_width: Some(16),
                mem_ctrl_pct: Some(31),
                video_pct: Some(0),
                bus_pct: Some(6),
                encoder_pct: Some(0),
                decoder_pct: Some(0),
                video_mhz: Some(1800),
                chip_power_w: Some(98.0),
                power_pct_tdp: Some(56.8),
                tdp_w: Some(250.0),
                pstate: Some(0),
                limit: Some("Pwr".into()),
                core_precise_c: Some(56.4),
                hotspot_hidden: false,
            },
        }
    }

    fn offsets(&mut self, _gpu: usize) -> Result<Offsets> {
        Ok(Offsets {
            core: Some(OffsetInfo {
                value_khz: self.core_khz,
                min_khz: -500_000,
                max_khz: 500_000,
                default_khz: 0,
            }),
            mem: Some(OffsetInfo {
                value_khz: self.mem_khz,
                min_khz: -1_000_000,
                max_khz: 2_000_000,
                default_khz: 0,
            }),
        })
    }

    fn set_offset(&mut self, _gpu: usize, domain: Domain, khz: i32) -> Result<()> {
        match domain {
            Domain::Core => self.core_khz = khz,
            Domain::Memory => self.mem_khz = khz,
        }
        Ok(())
    }

    fn curve(&mut self, _gpu: usize) -> Result<Vec<CurvePoint>> {
        Ok((0..DEMO_POINTS)
            .map(|i| CurvePoint {
                index: i,
                volt_uv: Self::volt_uv(i),
                freq_khz: self.freq_khz(i).max(0) as u32,
                delta_khz: self.deltas[i],
            })
            .collect())
    }

    fn set_curve(&mut self, _gpu: usize, deltas: &[(usize, i32)]) -> Result<()> {
        for &(i, d) in deltas {
            if let Some(slot) = self.deltas.get_mut(i) {
                *slot = d;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Kilka sterowników naraz (np. karta Nvidia i AMD w jednym komputerze):
// wspólna lista kart, każde wywołanie trafia do właściwego sterownika.
// ---------------------------------------------------------------------------

pub struct Multi {
    parts: Vec<Box<dyn Backend>>,
    /// Dla każdej karty z listy: (indeks sterownika, indeks karty w sterowniku).
    map: Vec<(usize, usize)>,
}

impl Multi {
    pub fn new(parts: Vec<Box<dyn Backend>>) -> Self {
        let mut map = Vec::new();
        for (p, part) in parts.iter().enumerate() {
            for g in 0..part.gpus().len() {
                map.push((p, g));
            }
        }
        Self { parts, map }
    }

    fn route(&mut self, gpu: usize) -> Result<(&mut Box<dyn Backend>, usize)> {
        let (p, g) = *self.map.get(gpu).ok_or_else(|| {
            crate::i18n::tr("Nieprawidłowy indeks karty", "Invalid card index").to_string()
        })?;
        Ok((&mut self.parts[p], g))
    }
}

impl Backend for Multi {
    fn gpus(&self) -> Vec<String> {
        self.parts.iter().flat_map(|p| p.gpus()).collect()
    }

    fn vendor(&self, gpu: usize) -> Vendor {
        match self.map.get(gpu) {
            Some(&(p, g)) => self.parts[p].vendor(g),
            None => Vendor::Nvidia,
        }
    }

    fn monitor(&mut self, gpu: usize) -> Monitor {
        match self.route(gpu) {
            Ok((b, g)) => b.monitor(g),
            Err(_) => Monitor::default(),
        }
    }

    fn temperature(&mut self, gpu: usize) -> Option<i32> {
        let (b, g) = self.route(gpu).ok()?;
        b.temperature(g)
    }

    fn set_detail(&mut self, on: bool) {
        for b in self.parts.iter_mut() {
            b.set_detail(on);
        }
    }

    fn fan(&mut self, gpu: usize) -> Option<FanStatus> {
        let (b, g) = self.route(gpu).ok()?;
        b.fan(g)
    }

    fn fan_controllable(&mut self, gpu: usize) -> bool {
        match self.route(gpu) {
            Ok((b, g)) => b.fan_controllable(g),
            Err(_) => false,
        }
    }

    fn set_fan(&mut self, gpu: usize, pct: Option<u32>) -> Result<()> {
        let (b, g) = self.route(gpu)?;
        b.set_fan(g, pct)
    }

    fn offsets(&mut self, gpu: usize) -> Result<Offsets> {
        let (b, g) = self.route(gpu)?;
        b.offsets(g)
    }

    fn set_offset(&mut self, gpu: usize, domain: Domain, khz: i32) -> Result<()> {
        let (b, g) = self.route(gpu)?;
        b.set_offset(g, domain, khz)
    }

    fn curve(&mut self, gpu: usize) -> Result<Vec<CurvePoint>> {
        let (b, g) = self.route(gpu)?;
        b.curve(g)
    }

    fn set_curve(&mut self, gpu: usize, deltas: &[(usize, i32)]) -> Result<()> {
        let (b, g) = self.route(gpu)?;
        b.set_curve(g, deltas)
    }
}
