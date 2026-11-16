//! Kompilacja shaderów z góry do pamięci podręcznej sterownika Nvidia.
//!
//! Shadery DirectX 12 kompiluje sterownik w chwili tworzenia potoku
//! (`CreateComputePipelineState` / `CreateGraphicsPipelineState`) i od razu
//! zapisuje wynik w `%LOCALAPPDATA%\NVIDIA\DXCache`. Klucz tej pamięci to:
//! - nazwa pliku .exe procesu - dlatego kompilacja działa w kopii Tune Center
//!   nazwanej tak jak gra (i nigdy równocześnie z grą),
//! - shader danego etapu i cała root signature - stan stały, formaty i układ
//!   wejścia nie wchodzą do klucza,
//! - gniazdo rozszerzeń shaderów NVAPI - ustawiamy je tak jak silnik gry
//!   (`NvAPI_D3D12_SetNvShaderExtnSlotSpace`, publiczne NVAPI).
//!
//! Ten moduł ma dwie strony: proces kompilujący (`run_host`, wywoływany przez
//! `tunecenter.exe --shader-warm ...`) i jego uruchamianie z interfejsu
//! (`Warm::start`), który czyta postęp ze standardowego wyjścia.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::dagor::{self, Container, StageCode};
use super::rootsig;

/// Argument wiersza poleceń procesu kompilującego.
pub const HOST_ARG: &str = "--shader-warm";

/// Próg czasu tworzenia potoku uznawanego za trafienie w pamięć podręczną.
const CACHED_MS: u64 = 250;

// ------------------------------ proces kompilujący --------------------------

#[cfg(windows)]
mod d3d {
    use std::collections::HashMap;
    use std::ffi::c_void;

    use windows::core::Interface;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::*;

    pub const NVIDIA: u32 = 0x10DE;

    pub struct Device {
        pub device: ID3D12Device,
        pub adapter: String,
        signatures: std::sync::Mutex<HashMap<Vec<u8>, ID3D12RootSignature>>,
    }

    // ID3D12Device jest wolnowątkowy (free-threaded) wg dokumentacji D3D12.
    unsafe impl Send for Device {}
    unsafe impl Sync for Device {}

    /// Urządzenie D3D12 na pierwszej karcie Nvidia.
    pub fn nvidia_device() -> Result<Device, String> {
        // SAFETY: wywołania DXGI/D3D12 z poprawnymi argumentami.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
            let mut index = 0;
            loop {
                let adapter = factory
                    .EnumAdapters1(index)
                    .map_err(|_| "nie znaleziono karty Nvidia / no Nvidia GPU found".to_string())?;
                index += 1;
                let desc = adapter.GetDesc1().map_err(|e| e.to_string())?;
                if desc.VendorId != NVIDIA {
                    continue;
                }
                let name = String::from_utf16_lossy(&desc.Description);
                let name = name.trim_end_matches('\0').to_owned();
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device)
                    .map_err(|e| format!("D3D12CreateDevice: {e}"))?;
                let device = device.ok_or("D3D12CreateDevice: brak urządzenia")?;
                return Ok(Device {
                    device,
                    adapter: name,
                    signatures: Default::default(),
                });
            }
        }
    }

    impl Device {
        /// Root signature z bloba; te same bloby tworzymy raz.
        pub fn root_signature(&self, blob: &[u8]) -> Result<ID3D12RootSignature, String> {
            if let Some(rs) = self.signatures.lock().unwrap().get(blob) {
                return Ok(rs.clone());
            }
            // SAFETY: blob z D3D12SerializeRootSignature.
            let rs: ID3D12RootSignature = unsafe { self.device.CreateRootSignature(0, blob) }
                .map_err(|e| format!("CreateRootSignature: {e}"))?;
            self.signatures
                .lock()
                .unwrap()
                .insert(blob.to_vec(), rs.clone());
            Ok(rs)
        }

        pub fn compute(&self, rs: &ID3D12RootSignature, cs: &[u8]) -> Result<(), String> {
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                // SAFETY: kopia wskaźnika bez AddRef; `rs` żyje dłużej niż wywołanie,
                // a ManuallyDrop nie zwolni go przy końcu struktury.
                pRootSignature: unsafe { std::mem::transmute_copy(rs) },
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: cs.as_ptr() as *const c_void,
                    BytecodeLength: cs.len(),
                },
                NodeMask: 0,
                CachedPSO: D3D12_CACHED_PIPELINE_STATE::default(),
                Flags: D3D12_PIPELINE_STATE_FLAG_NONE,
            };
            // SAFETY: opis z poprawnymi wskaźnikami na czas wywołania.
            let pso: windows::core::Result<ID3D12PipelineState> =
                unsafe { self.device.CreateComputePipelineState(&desc) };
            pso.map(|_| ()).map_err(|e| format!("{e}"))
        }

        pub fn raw(&self) -> *mut c_void {
            self.device.as_raw()
        }
    }
}

/// Gniazdo rozszerzeń shaderów Nvidii tak jak w silniku Dagor: slot 0,
/// przestrzeń 99 (`dxil::extension::nvidia`), na całe urządzenie.
#[cfg(windows)]
fn set_nvidia_extension_slot(device: *mut std::ffi::c_void) -> Result<(), String> {
    type Query = unsafe extern "C" fn(u32) -> *const std::ffi::c_void;
    type Init = unsafe extern "C" fn() -> i32;
    type SetSlot = unsafe extern "C" fn(*mut std::ffi::c_void, u32, u32) -> i32;
    // SAFETY: nvapi64.dll ze sterownika; identyfikatory z nvapi_interface.h.
    unsafe {
        let lib = libloading::Library::new("nvapi64.dll").map_err(|e| e.to_string())?;
        let query: Query = *lib
            .get::<Query>(b"nvapi_QueryInterface\0")
            .map_err(|e| e.to_string())?;
        let init = query(0x0150_E828);
        let set = query(0xAC2D_FEB5);
        if init.is_null() || set.is_null() {
            return Err("NVAPI: brak funkcji".into());
        }
        let init: Init = std::mem::transmute_copy(&init);
        let set: SetSlot = std::mem::transmute_copy(&set);
        let s = init();
        if s != 0 {
            return Err(format!("NvAPI_Initialize: {s}"));
        }
        let s = set(device, 0, 99);
        // Biblioteka zostaje załadowana do końca procesu.
        std::mem::forget(lib);
        if s != 0 {
            return Err(format!("NvAPI_D3D12_SetNvShaderExtnSlotSpace: {s}"));
        }
    }
    Ok(())
}

/// Właściwości root signature z wymagań shadera (`get_feature_requirement_mask`).
fn properties(headers: &[&dagor::Header], has_vertex_inputs: bool) -> rootsig::Properties {
    let mask = headers.iter().fold(0u64, |m, h| {
        m | (h.feature_flags_high as u64) << 32 | h.feature_flags_low as u64
    });
    rootsig::Properties {
        has_vertex_inputs,
        resource_heap_indexing: mask & rootsig::SHADER_REQUIRES_RESOURCE_DESCRIPTOR_HEAP_INDEXING
            != 0,
        sampler_heap_indexing: mask & rootsig::SHADER_REQUIRES_SAMPLER_DESCRIPTOR_HEAP_INDEXING
            != 0,
    }
}

/// Proces kompilujący: `--shader-warm <zrzut> <cbv-ranges 0|1>`.
/// Wypisuje na stdout wiersze `progress <zrobione> <wszystkie>`, na koniec
/// `result <ok> <błędy>` i do 10 wierszy `error ...`. Kod wyjścia 0 = OK.
#[cfg(windows)]
pub fn run_host(args: &[String]) -> i32 {
    let out = std::io::stdout();
    let say = |line: String| {
        let mut o = out.lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    };
    let (Some(dump_path), Some(cbv)) = (args.first(), args.get(1)) else {
        say("fatal użycie: --shader-warm <zrzut> <0|1>".into());
        return 2;
    };
    let cbv_root_descriptors = cbv != "1";
    let dump = match dagor::Dump::open(Path::new(dump_path)) {
        Ok(d) => d,
        Err(e) => {
            say(format!("fatal {e}"));
            return 3;
        }
    };
    let device = match d3d::nvidia_device() {
        Ok(d) => d,
        Err(e) => {
            say(format!("fatal {e}"));
            return 4;
        }
    };
    say(format!("adapter {}", device.adapter));
    if let Err(e) = set_nvidia_extension_slot(device.raw()) {
        say(format!("warning {e}"));
    }

    // Lista zadań: shadery compute z rozpakowanym kodem.
    let mut jobs: Vec<(usize, StageCode, Vec<u8>)> = Vec::new();
    let read = dump.for_each_shader(|i, data| {
        if let Some(Container::Single(s)) = dump.container(i) {
            if s.header.stage == dagor::Stage::Compute && s.bytecode.end <= data.len() {
                let code = data[s.bytecode.clone()].to_vec();
                jobs.push((i, s, code));
            }
        }
        true
    });
    if let Err(e) = read {
        say(format!("fatal {e}"));
        return 3;
    }
    let total = jobs.len();
    say(format!("progress 0 {total}"));

    let done = std::sync::atomic::AtomicUsize::new(0);
    let ok = std::sync::atomic::AtomicUsize::new(0);
    // Potoki utworzone w czasie krótszym niż CACHED_MS sterownik wziął z
    // pamięci podręcznej (kompilacja trwa dziesiątki-setki ms).
    let cached = std::sync::atomic::AtomicUsize::new(0);
    let errors = Mutex::new(Vec::<String>::new());
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(2, 16);
    let chunk = total.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for part in jobs.chunks(chunk) {
            let (device, done, ok, cached, errors, say) =
                (&device, &done, &ok, &cached, &errors, &say);
            scope.spawn(move || {
                for (index, stage, code) in part {
                    let props = properties(&[&stage.header], false);
                    let sig = rootsig::compute(props, &stage.header.usage, cbv_root_descriptors);
                    let result =
                        rootsig::serialize(&sig).and_then(|blob| device.root_signature(&blob));
                    let started = std::time::Instant::now();
                    let result = result.and_then(|rs| device.compute(&rs, code));
                    match result {
                        Ok(()) => {
                            ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if started.elapsed() < std::time::Duration::from_millis(CACHED_MS) {
                                cached.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            let mut list = errors.lock().unwrap();
                            if list.len() < 10 {
                                list.push(format!("#{index}: {e}"));
                            }
                        }
                    }
                    let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if n % 50 == 0 || n == total {
                        say(format!("progress {n} {total}"));
                    }
                }
            });
        }
    });
    let ok = ok.into_inner();
    for e in errors.into_inner().unwrap() {
        say(format!("error {e}"));
    }
    say(format!(
        "result {ok} {} {}",
        total - ok,
        cached.into_inner()
    ));
    // Zwolnienie urządzenia przed końcem procesu - sterownik dopisuje wtedy
    // pamięć podręczną na dysk.
    drop(device);
    0
}

// --------------------------- strona interfejsu ------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Idle,
    Running,
    Done,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    /// Ile potoków sterownik miał już w pamięci podręcznej (szybkie utworzenie).
    pub cached: usize,
    pub adapter: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Uruchomiona kompilacja jednej gry.
#[derive(Clone, Default)]
pub struct Warm {
    pub progress: Arc<Mutex<Progress>>,
}

/// Czy działa proces o tej nazwie pliku (np. gra).
#[cfg(windows)]
pub fn process_running(exe_name: &str) -> bool {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("IMAGENAME eq {exe_name}"),
            "/NH",
            "/FO",
            "CSV",
        ])
        .creation_flags(0x0800_0000)
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .to_lowercase()
            .contains(&format!("\"{}\"", exe_name.to_lowercase())),
        Err(_) => false,
    }
}

/// Kopia Tune Center nazwana jak gra, w osobnym katalogu tymczasowym.
fn staged_host(exe_name: &str) -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir().join("TuneCenter").join("warm");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(exe_name);
    std::fs::copy(&me, &target)
        .map_err(|e| crate::trf!("kopiowanie programu: {e}", "copying the program: {e}"))?;
    Ok(target)
}

impl Warm {
    /// Uruchamia kompilację zrzutu `dump` w procesie nazwanym `exe_name`.
    pub fn start(
        &self,
        exe_name: &str,
        dump: &Path,
        cbv_ranges: bool,
        wake: impl Fn() + Send + 'static,
    ) {
        {
            let mut p = self.progress.lock().unwrap();
            if p.phase == Phase::Running {
                return;
            }
            *p = Progress {
                phase: Phase::Running,
                ..Default::default()
            };
        }
        let shared = self.progress.clone();
        let fail = move |shared: &Arc<Mutex<Progress>>, text: String| {
            shared.lock().unwrap().phase = Phase::Failed(text);
        };
        #[cfg(windows)]
        if process_running(exe_name) {
            fail(
                &shared,
                crate::trf!(
                    "Gra {exe_name} jest uruchomiona - zamknij ją przed kompilacją.",
                    "The game {exe_name} is running - close it before compiling."
                ),
            );
            wake();
            return;
        }
        let host = match staged_host(exe_name) {
            Ok(h) => h,
            Err(e) => {
                fail(&shared, e);
                wake();
                return;
            }
        };
        let dump = dump.to_path_buf();
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(&host);
            cmd.arg(HOST_ARG)
                .arg(&dump)
                .arg(if cbv_ranges { "1" } else { "0" })
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000 | 0x0000_4000); // bez okna, niski priorytet (BELOW_NORMAL)
            }
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    fail(&shared, e.to_string());
                    wake();
                    return;
                }
            };
            if let Some(out) = child.stdout.take() {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    apply_line(&shared, &line);
                    wake();
                }
            }
            let status = child.wait();
            let _ = std::fs::remove_file(&host);
            let mut p = shared.lock().unwrap();
            if p.phase == Phase::Running {
                p.phase = match status {
                    Ok(s) if s.success() => Phase::Done,
                    Ok(s) => Phase::Failed(crate::trf!(
                        "kod wyjścia {}",
                        "exit code {}",
                        s.code().unwrap_or(-1)
                    )),
                    Err(e) => Phase::Failed(e.to_string()),
                };
            }
            drop(p);
            wake();
        });
    }
}

/// Interpretuje jeden wiersz wyjścia procesu kompilującego.
pub fn apply_line(shared: &Arc<Mutex<Progress>>, line: &str) {
    let mut p = shared.lock().unwrap();
    let mut words = line.splitn(2, ' ');
    let (kind, rest) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let nums: Vec<usize> = rest.split(' ').filter_map(|w| w.parse().ok()).collect();
    match kind {
        "progress" if nums.len() == 2 => {
            p.done = nums[0];
            p.total = nums[1];
        }
        "result" if nums.len() >= 2 => {
            p.ok = nums[0];
            p.failed = nums[1];
            p.cached = nums.get(2).copied().unwrap_or(0);
        }
        "adapter" => p.adapter = rest.to_owned(),
        "error" => p.errors.push(rest.to_owned()),
        "warning" => p.warnings.push(rest.to_owned()),
        "fatal" => p.phase = Phase::Failed(rest.to_owned()),
        _ => {}
    }
}

/// Plik .exe gry Dagor: największy w `win64` bez wariantu "-min-cpu".
pub fn dagor_game_exe(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir.join("win64"))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            let ok =
                lower.ends_with(".exe") && !lower.contains("min-cpu") && !lower.contains("crash");
            ok.then(|| (e.metadata().map(|m| m.len()).unwrap_or(0), name))
        })
        .max()
        .map(|(_, n)| n)
}

/// Tryb buforów stałych z nagłówka `cache\dx12.cache` (CX12, flaga 1 =
/// tabele zakresów CBV). Bez pliku: deskryptory główne.
pub fn dagor_cbv_ranges(dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(dir.join("cache").join("dx12.cache"))
        .map(|b| b.into_iter().take(32).collect::<Vec<u8>>())
    else {
        return false;
    };
    bytes.len() >= 32
        && &bytes[0..4] == b"CX12"
        && u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]) & 1 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_output() {
        let shared = Arc::new(Mutex::new(Progress {
            phase: Phase::Running,
            ..Default::default()
        }));
        for line in [
            "adapter NVIDIA GeForce RTX 5070",
            "progress 50 2388",
            "error #12: E_INVALIDARG",
            "result 2387 1 2000",
        ] {
            apply_line(&shared, line);
        }
        let p = shared.lock().unwrap().clone();
        assert_eq!(
            (p.done, p.total, p.ok, p.failed, p.cached),
            (50, 2388, 2387, 1, 2000)
        );
        assert_eq!(p.adapter, "NVIDIA GeForce RTX 5070");
        assert_eq!(p.errors, vec!["#12: E_INVALIDARG".to_string()]);
        apply_line(&shared, "fatal brak karty");
        assert_eq!(
            shared.lock().unwrap().phase,
            Phase::Failed("brak karty".into())
        );
    }
}
