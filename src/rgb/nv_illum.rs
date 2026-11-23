//! Podświetlenie kart NVIDIA przez publiczne NVAPI (nvapi.h, rozdział
//! "Client Illumination"): `NvAPI_GPU_ClientIllumZonesGetInfo`,
//! `…GetControl`, `…SetControl`.
//!
//! Działa na kartach, których sterownik udostępnia strefy oświetlenia (np.
//! Founders Edition); wiele kart partnerów steruje diodami własnym układem
//! poza NVAPI i wtedy lista stref jest pusta.
//!
//! Struktury (wersja 1, wyrównanie 8, bez pól 64-bitowych):
//! - informacje o strefie: 140 B (typ u32, urządzenie u8, prov u8, miejsce
//!   u32, unia 64 B, rezerwa 64 B); parametry: wersja, liczba, rezerwa 64 B
//!   i 32 strefy = 4552 B,
//! - sterowanie strefą: 200 B (typ u32, tryb u32, unia danych 128 B,
//!   rezerwa 64 B); parametry: wersja, bDefault, liczba, rezerwa 64 B i
//!   32 strefy = 6476 B. Tryb 0 = ręczny; dane ręczne: RGB → R, G, B,
//!   jasność %; RGBW → R, G, B, W, jasność %; kolor stały / jeden kolor →
//!   jasność %.
//!
//! Program zapamiętuje sterowanie sprzed pierwszej zmiany i przywraca je
//! przy oddaniu sterowania.

use std::ffi::c_void;

use crate::nvcp::api::{self, Api, Handle};

const INFO_SIZE: usize = 4552;
const INFO_ZONE: usize = 140;
const CONTROL_SIZE: usize = 6476;
const CONTROL_ZONE: usize = 200;
/// Początek tablicy stref w parametrach (wersja, pola, rezerwa 64 B).
const INFO_HEAD: usize = 72;
const CONTROL_HEAD: usize = 76;
const MAX_ZONES: usize = 32;

const ZONE_RGB: u32 = 1;
const ZONE_COLOR_FIXED: u32 = 2;
const ZONE_RGBW: u32 = 3;
const ZONE_SINGLE_COLOR: u32 = 4;

fn version(size: usize) -> u32 {
    size as u32 | (1 << 16)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

struct Gpu {
    api: std::sync::Arc<Api>,
    gpu: Handle,
    index: usize,
    name: String,
    zones: usize,
    original: Option<Vec<u8>>,
}

impl Gpu {
    fn get(&self) -> Result<Vec<u8>, String> {
        let f = api::need(self.api.illum_zones_get_control)?;
        let mut buf = vec![0u8; CONTROL_SIZE];
        buf[0..4].copy_from_slice(&version(CONTROL_SIZE).to_le_bytes());
        // SAFETY: bufor ma rozmiar struktury podanej w polu wersji.
        api::check(unsafe { f(self.gpu, buf.as_mut_ptr() as *mut c_void) })?;
        Ok(buf)
    }

    fn set(&self, buf: &mut [u8]) -> Result<(), String> {
        let f = api::need(self.api.illum_zones_set_control)?;
        // SAFETY: jak wyżej.
        api::check(unsafe { f(self.gpu, buf.as_mut_ptr() as *mut c_void) })
    }
}

/// Sterowanie ze zmienionym kolorem i jasnością stref.
fn modified(original: &[u8], rgb: [u8; 3], brightness: u8) -> Vec<u8> {
    let mut b = original.to_vec();
    // bDefault = 0: bieżące (nie domyślne) ustawienia.
    b[4..8].copy_from_slice(&0u32.to_le_bytes());
    let count = (u32_at(&b, 8) as usize).min(MAX_ZONES);
    for z in 0..count {
        let at = CONTROL_HEAD + z * CONTROL_ZONE;
        let kind = u32_at(&b, at);
        b[at + 4..at + 8].copy_from_slice(&0u32.to_le_bytes()); // tryb ręczny
        let d = at + 8;
        match kind {
            ZONE_RGB => {
                b[d..d + 3].copy_from_slice(&rgb);
                b[d + 3] = brightness;
            }
            ZONE_RGBW => {
                b[d..d + 3].copy_from_slice(&rgb);
                b[d + 3] = 0;
                b[d + 4] = brightness;
            }
            ZONE_COLOR_FIXED | ZONE_SINGLE_COLOR => b[d] = brightness,
            _ => {}
        }
    }
    b
}

impl super::Device for Gpu {
    fn info(&self) -> super::Info {
        super::Info {
            id: format!("nvidia-illum:{}", self.index),
            name: self.name.clone(),
            family: super::Family::NvidiaGpu,
            detail: crate::trf!("stref: {}", "zones: {}", self.zones),
            restores: true,
            effects: vec![super::Effect::Static],
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        let current = self.get()?;
        if self.original.is_none() {
            self.original = Some(current.clone());
        }
        let (rgb, brightness) = match s.mode {
            super::Mode::Color => (s.color, s.brightness.min(100)),
            _ => ([0, 0, 0], 0),
        };
        let mut b = modified(&current, rgb, brightness);
        self.set(&mut b)?;
        // Weryfikacja odczytem: pierwsza strefa RGB ma nasz kolor.
        let after = self.get()?;
        let count = (u32_at(&after, 8) as usize).min(MAX_ZONES);
        for z in 0..count {
            let at = CONTROL_HEAD + z * CONTROL_ZONE;
            if u32_at(&after, at) == ZONE_RGB && after[at + 8..at + 12] != b[at + 8..at + 12] {
                return Err(crate::i18n::tr(
                    "Sterownik nie przyjął koloru karty.",
                    "The driver did not accept the card color.",
                )
                .into());
            }
        }
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        if let Some(mut o) = self.original.take() {
            self.set(&mut o)?;
        }
        Ok(())
    }
}

/// Karty ze strefami oświetlenia (tylko odczyty).
pub fn discover() -> Result<Vec<Box<dyn super::Device>>, String> {
    let Ok(api) = Api::load() else {
        return Ok(Vec::new());
    };
    let api = std::sync::Arc::new(api);
    let (Some(enum_gpus), Some(info_fn)) = (api.enum_gpus, api.illum_zones_get_info) else {
        return Ok(Vec::new());
    };
    let mut gpus: [Handle; 64] = [std::ptr::null_mut(); 64];
    let mut count = 0u32;
    // SAFETY: tablica na NVAPI_MAX_PHYSICAL_GPUS (64) uchwytów.
    if unsafe { enum_gpus(gpus.as_mut_ptr(), &mut count) } != api::OK {
        return Ok(Vec::new());
    }
    let mut out: Vec<Box<dyn super::Device>> = Vec::new();
    for (index, &gpu) in gpus.iter().take(count as usize).enumerate() {
        let mut info = vec![0u8; INFO_SIZE];
        info[0..4].copy_from_slice(&version(INFO_SIZE).to_le_bytes());
        // SAFETY: bufor ma rozmiar struktury podanej w polu wersji.
        if unsafe { info_fn(gpu, info.as_mut_ptr() as *mut c_void) } != api::OK {
            continue;
        }
        let zones = (u32_at(&info, 4) as usize).min(MAX_ZONES);
        let controllable = (0..zones).any(|z| {
            let t = u32_at(&info, INFO_HEAD + z * INFO_ZONE);
            (ZONE_RGB..=ZONE_SINGLE_COLOR).contains(&t)
        });
        if !controllable {
            continue;
        }
        out.push(Box::new(Gpu {
            api: api.clone(),
            gpu,
            index,
            name: crate::trf!("Karta NVIDIA {}", "NVIDIA card {}", index + 1),
            zones,
            original: None,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod hardware {
    /// Strefy oświetlenia kart (tylko odczyt):
    /// `cargo test nv_illum_scan -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn nv_illum_scan() {
        let api = super::Api::load().expect("NVAPI");
        let mut gpus: [super::Handle; 64] = [std::ptr::null_mut(); 64];
        let mut count = 0u32;
        unsafe { api.enum_gpus.unwrap()(gpus.as_mut_ptr(), &mut count) };
        for &gpu in gpus.iter().take(count as usize) {
            let mut info = vec![0u8; super::INFO_SIZE];
            info[0..4].copy_from_slice(&super::version(super::INFO_SIZE).to_le_bytes());
            let st = unsafe {
                api.illum_zones_get_info.unwrap()(gpu, info.as_mut_ptr() as *mut std::ffi::c_void)
            };
            println!(
                "ZonesGetInfo: {st} ({}), stref: {}",
                super::api::describe(st),
                super::u32_at(&info, 4)
            );
        }
        println!(
            "{:?}",
            super::discover().map(|d| d.iter().map(|x| x.info()).collect::<Vec<_>>())
        );
    }
}
