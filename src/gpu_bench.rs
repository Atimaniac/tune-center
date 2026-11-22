//! Test stabilności karty graficznej (zakładka BENCH): 2 minuty pełnego
//! obciążenia przez Direct3D 11 (shadery obliczeniowe).
//!
//! Każda runda:
//! - pamięć: 8 × zapis wzorca (skrót indeksu i ziarna) do bufora 512 MB i
//!   sprawdzenie odczytem w shaderze - niezgodność = błąd pamięci (np. za
//!   wysoki zegar pamięci);
//! - „burn”: każdy wątek czyta losowo cały bufor i liczy FMA (~100 ms) -
//!   najwyższy pobór mocy z prób (rdzeń + pamięć naraz; samo FMA, rysowanie
//!   z teksturami i podział wątków dawały mniej); wynik niesprawdzany;
//! - rdzeń: łańcuchy FMA (16 × float4 na iterację) na ~1 mln wątków, liczba
//!   iteracji dobrana tak, żeby wywołanie trwało ok. 100 ms (daleko od
//!   2-sekundowego limitu TDR Windows); wyniki porównywane z wynikami tej
//!   samej rundy z pierwszego przebiegu - różnica = błąd obliczeń (za wysoki
//!   zegar rdzenia / za niskie napięcie).
//!
//! Wynik = średnia wydajność FP32 (GFLOPS) w teście; obok przepustowość
//! pamięci (GB/s). Zegary, temperaturę i moc zbiera `App::logic` co sekundę
//! z odczytów sterownika (wątek czujników karty).

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Czas testu.
pub const TEST_TIME: Duration = Duration::from_secs(120);
/// Rozgrzewka nieliczona do wyniku.
const WARMUP: Duration = Duration::from_secs(5);
/// Docelowy czas jednego wywołania obliczeń.
const TARGET_DISPATCH: Duration = Duration::from_millis(100);
/// Bufor pamięci (512 MB = limit widoku 2^27 słów 32-bitowych).
const MEM_BYTES: u32 = 512 * 1024 * 1024;
/// Wątki obliczeń: grupy × 256.
const ALU_GROUPS: u32 = 4096;
/// Zapis + sprawdzenie pamięci na rundę.
const MEM_PASSES: u32 = 8;
/// Różne ziarna obliczeń (wzorce z pierwszego przebiegu).
const SEEDS: u32 = 4;

const HLSL: &str = r#"
cbuffer P : register(b0) { uint seed; uint iters; uint count; uint pad; };
RWStructuredBuffer<uint> outBuf : register(u0);
RWByteAddressBuffer mem : register(u1);
RWByteAddressBuffer err : register(u2);

uint hash(uint x) {
    x ^= x >> 16; x *= 0x7feb352du; x ^= x >> 15; x *= 0x846ca68bu; x ^= x >> 16;
    return x;
}

uint4 pattern(uint i) {
    uint b = i * 4u;
    return uint4(hash(b ^ seed), hash((b + 1u) ^ seed), hash((b + 2u) ^ seed), hash((b + 3u) ^ seed));
}

[numthreads(256, 1, 1)]
void alu(uint3 id : SV_DispatchThreadID) {
    uint i = id.x;
    float4 a = float4(hash(i ^ seed) & 1023u, hash(i + 11u) & 1023u, hash(i + 23u) & 1023u, hash(i + 37u) & 1023u) * (1.0 / 1024.0);
    float4 b = a.wzyx + 0.5;
    float4 c = a.yxwz + 0.25;
    float4 d = a + 0.125;
    float4 m = float4(0.99990, 0.99991, 0.99992, 0.99993);
    float4 k = float4(0.00010, 0.00011, 0.00012, 0.00013);
    [loop] for (uint n = 0; n < iters; n++) {
        a = mad(a, m, k); b = mad(b, m, k); c = mad(c, m, k); d = mad(d, m, k);
        a = mad(a, m, k); b = mad(b, m, k); c = mad(c, m, k); d = mad(d, m, k);
        a = mad(a, m, k); b = mad(b, m, k); c = mad(c, m, k); d = mad(d, m, k);
        a = mad(a, m, k); b = mad(b, m, k); c = mad(c, m, k); d = mad(d, m, k);
    }
    float4 s = a + b * 3.0 + c * 5.0 + d * 7.0;
    outBuf[i] = asuint(s.x) ^ (asuint(s.y) * 3u) ^ (asuint(s.z) * 5u) ^ (asuint(s.w) * 7u);
}

[numthreads(256, 1, 1)]
void fill(uint3 id : SV_DispatchThreadID) {
    uint i = id.y * 262144u + id.x;
    if (i >= count) return;
    mem.Store4(i * 16u, pattern(i));
}

[numthreads(256, 1, 1)]
void check(uint3 id : SV_DispatchThreadID) {
    uint i = id.y * 262144u + id.x;
    if (i >= count) return;
    uint4 v = mem.Load4(i * 16u);
    if (any(v != pattern(i))) {
        uint old;
        err.InterlockedAdd(0, 1u, old);
    }
}

// Maksymalny pobór mocy: każdy wątek czyta cały bufor 512 MB (pamięć i jej
// kontroler) i liczy FMA; wynik nie jest sprawdzany (bufor zmienia się w teście).
[numthreads(256, 1, 1)]
void burn(uint3 id : SV_DispatchThreadID) {
    uint i = id.x;
    float4 a = float4(i & 255u, 1, 2, 3) * (1.0 / 256.0);
    float4 b = a + 0.5;
    float4 m = float4(0.99990, 0.99991, 0.99992, 0.99993);
    float4 k = float4(0.00010, 0.00011, 0.00012, 0.00013);
    [loop] for (uint n = 0; n < iters; n++) {
        uint4 v = mem.Load4(((i * 7u + n * 4099u) & (count - 1u)) * 16u);
        a = mad(a, m, asfloat((v & 0x007FFFFFu) | 0x3F800000u) * 1e-6);
        b = mad(b, m, k); a = mad(a, m, k); b = mad(b, m, k); a = mad(a, m, k);
        b = mad(b, m, k); a = mad(a, m, k); b = mad(b, m, k); a = mad(a, m, k);
    }
    float4 s = a + b;
    if (s.x == 12345.678) outBuf[i] = 1u;
}
"#;

/// Odczyt karty co sekundę (z wątku czujników sterownika).
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub core_mhz: Option<f32>,
    pub mem_mhz: Option<f32>,
    pub temp: Option<f32>,
    pub watts: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Idle,
    /// Przygotowanie (urządzenie, shadery, dobór obciążenia).
    Starting,
    Running,
    /// Zakończony (`true` = cały czas, `false` = zatrzymany wcześniej).
    Done(bool),
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub phase: Phase,
    pub adapter: String,
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
    /// Średnia wydajność (GFLOPS FP32) i przepustowość pamięci (GB/s) po rozgrzewce.
    pub gflops: Option<f64>,
    pub bandwidth: Option<f64>,
    pub alu_errors: u64,
    pub mem_errors: u64,
    pub rounds: u64,
    pub samples: Vec<Sample>,
    /// Przyczyny ograniczania zegarów zebrane w teście (np. "Pwr").
    pub limits: Vec<String>,
}

impl State {
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Starting | Phase::Running)
    }

    pub fn elapsed(&self) -> Duration {
        match self.started {
            Some(s) => self
                .ended
                .unwrap_or_else(Instant::now)
                .saturating_duration_since(s),
            None => Duration::ZERO,
        }
    }

    pub fn errors(&self) -> u64 {
        self.alu_errors + self.mem_errors
    }

    /// (średnia, minimum, maksimum) wybranej serii.
    pub fn stats(&self, f: impl Fn(&Sample) -> Option<f32>) -> Option<(f32, f32, f32)> {
        let v: Vec<f32> = self.samples.iter().filter_map(f).collect();
        if v.is_empty() {
            return None;
        }
        let avg = v.iter().sum::<f32>() / v.len() as f32;
        let min = v.iter().cloned().fold(f32::MAX, f32::min);
        let max = v.iter().cloned().fold(f32::MIN, f32::max);
        Some((avg, min, max))
    }
}

pub struct GpuBench {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    last_sample: Option<Instant>,
}

impl GpuBench {
    pub fn new() -> Self {
        Self {
            state: Arc::default(),
            stop: Arc::default(),
            last_sample: None,
        }
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
    }

    pub fn start(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        if self.state().running() {
            return;
        }
        self.stop.store(false, Relaxed);
        self.last_sample = None;
        if let Ok(mut s) = self.state.lock() {
            *s = State {
                phase: Phase::Starting,
                ..Default::default()
            };
        }
        let (state, stop) = (self.state.clone(), self.stop.clone());
        std::thread::spawn(move || {
            let result = run(&state, &stop, &wake);
            if let Ok(mut s) = state.lock() {
                s.ended = Some(Instant::now());
                s.phase = match result {
                    Ok(full) => Phase::Done(full),
                    Err(e) => Phase::Failed(e),
                };
            }
            wake();
        });
    }

    /// Co sekundę z `App::logic`: zegary, temperatura i moc karty.
    pub fn observe(&mut self, sample: Sample, limit: Option<&str>) {
        let now = Instant::now();
        if self
            .last_sample
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        let Ok(mut s) = self.state.lock() else { return };
        if s.phase != Phase::Running {
            return;
        }
        self.last_sample = Some(now);
        s.samples.push(sample);
        if let Some(l) = limit.filter(|l| !l.is_empty() && *l != "Idle") {
            for part in l.split([',', ' ']).filter(|p| !p.is_empty()) {
                if !s.limits.iter().any(|x| x == part) {
                    s.limits.push(part.to_owned());
                }
            }
        }
    }
}

impl Drop for GpuBench {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

#[cfg(not(windows))]
fn run(
    _: &Arc<Mutex<State>>,
    _: &Arc<AtomicBool>,
    _: &(impl Fn() + Send + Sync),
) -> Result<bool, String> {
    Err("tylko Windows".into())
}

#[cfg(windows)]
fn run(
    state: &Arc<Mutex<State>>,
    stop: &Arc<AtomicBool>,
    wake: &(impl Fn() + Send + Sync),
) -> Result<bool, String> {
    use windows::core::PCSTR;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;

    // SAFETY: wywołania D3D11/DXGI z poprawnymi opisami; kontekst używany
    // tylko w tym wątku, zasoby żyją do końca funkcji.
    unsafe {
        // Karta z największą pamięcią (dedykowana; bez renderera programowego).
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
        let mut best: Option<(usize, IDXGIAdapter1, String)> = None;
        let mut i = 0;
        while let Ok(a) = factory.EnumAdapters1(i) {
            i += 1;
            let d = a.GetDesc1().map_err(|e| e.to_string())?;
            if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let name = String::from_utf16_lossy(&d.Description)
                .trim_end_matches('\0')
                .to_owned();
            if best
                .as_ref()
                .is_none_or(|(m, _, _)| d.DedicatedVideoMemory > *m)
            {
                best = Some((d.DedicatedVideoMemory, a, name));
            }
        }
        let (_, adapter, name) = best.ok_or("nie znaleziono karty graficznej")?;
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
        let device: ID3D11Device = device.ok_or("brak urządzenia D3D11")?;
        let ctx: ID3D11DeviceContext = context.ok_or("brak kontekstu D3D11")?;
        if let Ok(mut s) = state.lock() {
            s.adapter = name;
        }

        let blob = |entry: &str, target: &str| -> Result<Vec<u8>, String> {
            let entry_c = format!("{entry}\0");
            let target_c = format!("{target}\0");
            let mut code = None;
            let mut errors = None;
            let r = D3DCompile(
                HLSL.as_ptr() as *const _,
                HLSL.len(),
                PCSTR(c"gpu_bench".as_ptr() as *const u8),
                None,
                None,
                PCSTR(entry_c.as_ptr()),
                PCSTR(target_c.as_ptr()),
                0,
                0,
                &mut code,
                Some(&mut errors),
            );
            if let Err(e) = r {
                let msg = errors
                    .map(|b: ID3DBlob| {
                        String::from_utf8_lossy(std::slice::from_raw_parts(
                            b.GetBufferPointer() as *const u8,
                            b.GetBufferSize(),
                        ))
                        .into_owned()
                    })
                    .unwrap_or_default();
                return Err(format!("D3DCompile {entry}: {e} {msg}"));
            }
            let code = code.ok_or("D3DCompile: brak kodu")?;
            let bytes = std::slice::from_raw_parts(
                code.GetBufferPointer() as *const u8,
                code.GetBufferSize(),
            );
            Ok(bytes.to_vec())
        };
        let compile = |entry: &str| -> Result<ID3D11ComputeShader, String> {
            let bytes = blob(entry, "cs_5_0")?;
            let mut cs = None;
            device
                .CreateComputeShader(&bytes, None, Some(&mut cs))
                .map_err(|e| format!("CreateComputeShader: {e}"))?;
            cs.ok_or_else(|| "CreateComputeShader: brak shadera".to_owned())
        };
        let cs_alu = compile("alu")?;
        let cs_fill = compile("fill")?;
        let cs_check = compile("check")?;
        let cs_burn = compile("burn")?;

        // Bufory.
        let buffer = |bytes: u32, raw: bool, structured: bool| -> Result<ID3D11Buffer, String> {
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: bytes,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: if raw {
                    D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS.0 as u32
                } else if structured {
                    D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32
                } else {
                    0
                },
                StructureByteStride: if structured { 4 } else { 0 },
            };
            let mut b = None;
            device
                .CreateBuffer(&desc, None, Some(&mut b))
                .map_err(|e| format!("CreateBuffer {bytes} B: {e}"))?;
            b.ok_or_else(|| "CreateBuffer: brak bufora".to_owned())
        };
        let staging = |bytes: u32| -> Result<ID3D11Buffer, String> {
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: bytes,
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut b = None;
            device
                .CreateBuffer(&desc, None, Some(&mut b))
                .map_err(|e| format!("CreateBuffer: {e}"))?;
            b.ok_or_else(|| "CreateBuffer: brak bufora".to_owned())
        };
        let threads = ALU_GROUPS * 256;
        let out = buffer(threads * 4, false, true)?;
        let mem = buffer(MEM_BYTES, true, false)?;
        let err = buffer(16, true, false)?;
        let out_read = staging(threads * 4)?;
        let err_read = staging(16)?;

        let uav = |b: &ID3D11Buffer,
                   elements: u32,
                   raw: bool|
         -> Result<ID3D11UnorderedAccessView, String> {
            let desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
                Format: if raw {
                    DXGI_FORMAT_R32_TYPELESS
                } else {
                    Default::default()
                },
                ViewDimension: D3D11_UAV_DIMENSION_BUFFER,
                Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                    Buffer: D3D11_BUFFER_UAV {
                        FirstElement: 0,
                        NumElements: elements,
                        Flags: if raw {
                            D3D11_BUFFER_UAV_FLAG_RAW.0 as u32
                        } else {
                            0
                        },
                    },
                },
            };
            let mut v = None;
            device
                .CreateUnorderedAccessView(b, Some(&desc), Some(&mut v))
                .map_err(|e| format!("CreateUnorderedAccessView: {e}"))?;
            v.ok_or_else(|| "UAV: brak widoku".to_owned())
        };
        let uavs = [
            Some(uav(&out, threads, false)?),
            Some(uav(&mem, MEM_BYTES / 4, true)?),
            Some(uav(&err, 4, true)?),
        ];

        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut cb = None;
        device
            .CreateBuffer(&cb_desc, None, Some(&mut cb))
            .map_err(|e| format!("CreateBuffer cb: {e}"))?;
        let cb: ID3D11Buffer = cb.ok_or("brak bufora stałych")?;
        let mut q = None;
        device
            .CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut q),
            )
            .map_err(|e| format!("CreateQuery: {e}"))?;
        let query: ID3D11Query = q.ok_or("brak zapytania")?;

        ctx.CSSetUnorderedAccessViews(0, 3, Some(uavs.as_ptr()), None);
        // Licznik błędów pamięci: zawartość nowego bufora nie jest gwarantowana.
        if let Some(v) = &uavs[2] {
            ctx.ClearUnorderedAccessViewUint(v, &[0; 4]);
        }
        ctx.CSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
        let count = MEM_BYTES / 16;
        let set = |seed: u32, iters: u32| {
            let p = [seed, iters, count, 0u32];
            ctx.UpdateSubresource(&cb, 0, None, p.as_ptr() as *const _, 0, 0);
        };
        // Czeka, aż karta skończy wszystko, co zlecono.
        let finish = || {
            ctx.End(&query);
            ctx.Flush();
            let mut done: i32 = 0;
            while ctx
                .GetData(&query, Some(&mut done as *mut i32 as *mut _), 4, 0)
                .map(|_| done == 0)
                .unwrap_or(false)
            {
                std::thread::yield_now();
            }
        };
        let alu = |seed: u32, iters: u32| -> Duration {
            set(seed, iters);
            ctx.CSSetShader(&cs_alu, None);
            let t = Instant::now();
            ctx.Dispatch(ALU_GROUPS, 1, 1);
            finish();
            t.elapsed()
        };
        let read_u32s = |src: &ID3D11Buffer, dst: &ID3D11Buffer, n: usize| -> Vec<u32> {
            ctx.CopyResource(dst, src);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if ctx.Map(dst, 0, D3D11_MAP_READ, 0, Some(&mut m)).is_err() {
                return Vec::new();
            }
            let v = std::slice::from_raw_parts(m.pData as *const u32, n).to_vec();
            ctx.Unmap(dst, 0);
            v
        };
        // Dobór liczby iteracji: wywołanie ~100 ms.
        let mut iters = 64u32;
        for _ in 0..6 {
            let t = alu(0, iters);
            let scale = TARGET_DISPATCH.as_secs_f64() / t.as_secs_f64().max(1e-4);
            iters = ((iters as f64 * scale.clamp(0.25, 8.0)) as u32).clamp(16, 200_000);
        }
        // Przebieg „burn” (maksymalny pobór mocy) ~100 ms.
        let burn = |iters: u32| -> Duration {
            set(0x0B0B, iters);
            ctx.CSSetShader(&cs_burn, None);
            let t = Instant::now();
            ctx.Dispatch(ALU_GROUPS, 1, 1);
            finish();
            t.elapsed()
        };
        let mut burn_iters = 64u32;
        for _ in 0..6 {
            let t = burn(burn_iters);
            let scale = TARGET_DISPATCH.as_secs_f64() / t.as_secs_f64().max(1e-4);
            burn_iters = ((burn_iters as f64 * scale.clamp(0.25, 8.0)) as u32).clamp(4, 100_000);
        }
        // Wzorce obliczeń (pierwszy przebieg).
        let mut reference = Vec::new();
        for seed in 0..SEEDS {
            alu(seed + 1, iters);
            reference.push(read_u32s(&out, &out_read, threads as usize));
        }

        if let Ok(mut s) = state.lock() {
            s.phase = Phase::Running;
            s.started = Some(Instant::now());
        }
        wake();
        let start = Instant::now();
        let (mut alu_time, mut alu_flops, mut mem_time, mut mem_bytes) = (0f64, 0f64, 0f64, 0f64);
        let mut round = 0u64;
        let full = loop {
            if stop.load(Relaxed) {
                break false;
            }
            if start.elapsed() >= TEST_TIME {
                break true;
            }
            // Pamięć.
            let groups_y = count.div_ceil(262_144);
            let t = Instant::now();
            for p in 0..MEM_PASSES {
                set(0xA5A5_0000 ^ (round as u32 * MEM_PASSES + p), 0);
                ctx.CSSetShader(&cs_fill, None);
                ctx.Dispatch(1024, groups_y, 1);
                ctx.CSSetShader(&cs_check, None);
                ctx.Dispatch(1024, groups_y, 1);
            }
            finish();
            let mt = t.elapsed().as_secs_f64();
            // Maksymalny pobór mocy (pamięć + FMA).
            burn(burn_iters);
            // Rdzeń.
            let seed = (round % SEEDS as u64) as u32;
            let at = alu(seed + 1, iters).as_secs_f64();
            let result = read_u32s(&out, &out_read, threads as usize);
            let bad = result
                .iter()
                .zip(&reference[seed as usize])
                .filter(|(a, b)| a != b)
                .count() as u64;
            let mem_bad = read_u32s(&err, &err_read, 1).first().copied().unwrap_or(0) as u64;
            // Reset sterownika (TDR) po niestabilnym podkręceniu: urządzenie
            // przestaje działać, a odczyty są puste - to też wynik testu.
            if let Err(e) = device.GetDeviceRemovedReason() {
                return Err(crate::trf!(
                    "sterownik karty zresetował się w trakcie testu ({e}) - ustawienia karty są niestabilne",
                    "the graphics driver reset during the test ({e}) - the card settings are unstable"
                ));
            }
            if result.len() != threads as usize {
                return Err(crate::i18n::tr(
                    "nie udało się odczytać wyników z karty",
                    "could not read results from the card",
                )
                .to_owned());
            }
            if start.elapsed() >= WARMUP {
                alu_time += at;
                alu_flops += threads as f64 * iters as f64 * 16.0 * 4.0 * 2.0;
                mem_time += mt;
                mem_bytes += MEM_BYTES as f64 * 2.0 * MEM_PASSES as f64;
            }
            round += 1;
            if let Ok(mut s) = state.lock() {
                s.alu_errors += bad;
                s.mem_errors = mem_bad;
                s.rounds = round;
                if alu_time > 0.0 {
                    s.gflops = Some(alu_flops / alu_time / 1e9);
                    s.bandwidth = Some(mem_bytes / mem_time / 1e9);
                }
            }
            if round.is_multiple_of(4) {
                wake();
            }
        };
        Ok(full)
    }
}

/// Zapisany wynik testu karty.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub when: String,
    pub gpu: String,
    pub gflops: f64,
    pub bandwidth: f64,
    pub errors: u64,
    pub core_avg: f32,
    pub mem_avg: f32,
    pub temp_max: f32,
    pub watts_avg: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 s testu na prawdziwej karcie (zamiast 2 min - przerwany):
    /// `cargo test gpu_bench_run --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gpu_bench_run() {
        let mut b = GpuBench::new();
        b.start(|| {});
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(25) && b.state().running() {
            std::thread::sleep(Duration::from_millis(500));
        }
        b.stop();
        while b.state().running() {
            std::thread::sleep(Duration::from_millis(100));
        }
        let s = b.state();
        println!(
            "{} {:?}: rundy {}, GFLOPS {:?}, GB/s {:?}, błędy obliczeń {}, pamięci {}",
            s.adapter, s.phase, s.rounds, s.gflops, s.bandwidth, s.alu_errors, s.mem_errors
        );
        assert!(!matches!(s.phase, Phase::Failed(_)));
    }
}
