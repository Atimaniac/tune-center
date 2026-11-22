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

// ------------------------- Unity, Direct3D 11 -------------------------------

/// Argument procesu kompilującego shadery DirectX 11 (Unity albo Dagor DX11).
pub const HOST_ARG_D3D11: &str = "--shader-warm-d3d11";

#[cfg(windows)]
mod d3d11 {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::*;

    use super::super::unity::Stage;

    pub struct Device {
        device: ID3D11Device,
        pub adapter: String,
    }

    // Metody Create* urządzenia D3D11 są wolnowątkowe (bez flagi
    // D3D11_CREATE_DEVICE_SINGLETHREADED).
    unsafe impl Send for Device {}
    unsafe impl Sync for Device {}

    pub fn nvidia_device() -> Result<Device, String> {
        // SAFETY: wywołania DXGI/D3D11 z poprawnymi argumentami.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
            let mut index = 0;
            loop {
                let adapter = factory
                    .EnumAdapters1(index)
                    .map_err(|_| "nie znaleziono karty Nvidia / no Nvidia GPU found".to_string())?;
                index += 1;
                let desc = adapter.GetDesc1().map_err(|e| e.to_string())?;
                if desc.VendorId != super::d3d::NVIDIA {
                    continue;
                }
                let name = String::from_utf16_lossy(&desc.Description);
                let mut device = None;
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_FLAG(0),
                    Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
                .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
                return Ok(Device {
                    device: device.ok_or("D3D11CreateDevice: brak urządzenia")?,
                    adapter: name.trim_end_matches('\0').to_owned(),
                });
            }
        }
    }

    impl Device {
        /// Tworzy shader danego etapu. Sterownik kompiluje go w swoich wątkach;
        /// obiekt trzymamy do końca kompilacji (zwolnienie ją porzuca).
        pub fn create(&self, stage: Stage, code: &[u8]) -> Result<ID3D11DeviceChild, String> {
            let d = &self.device;
            let none: Option<&ID3D11ClassLinkage> = None;
            macro_rules! make {
                ($method:ident) => {{
                    let mut s = None;
                    d.$method(code, none, Some(&mut s))
                        .map(|()| s.map(ID3D11DeviceChild::from))
                }};
            }
            // SAFETY: kod to cały kontener DXBC.
            let r = unsafe {
                match stage {
                    Stage::Vertex => make!(CreateVertexShader),
                    Stage::Pixel => make!(CreatePixelShader),
                    Stage::Geometry => make!(CreateGeometryShader),
                    Stage::Hull => make!(CreateHullShader),
                    Stage::Domain => make!(CreateDomainShader),
                    Stage::Compute => make!(CreateComputeShader),
                }
            };
            r.map_err(|e| format!("{e}"))?
                .ok_or_else(|| "brak obiektu shadera".to_string())
        }
    }

    /// Czas procesora zużyty przez cały proces (wszystkie wątki, także
    /// wątki kompilatora sterownika), w ms.
    pub fn process_cpu_ms() -> u64 {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let (mut a, mut b, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: uchwyt bieżącego procesu, wskaźniki na lokalne struktury.
        if unsafe { GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u) }.is_err()
        {
            return 0;
        }
        let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        (t(k) + t(u)) / 10_000
    }

    /// Czeka, aż wątki sterownika skończą kompilować: zużycie procesora przez
    /// proces spada poniżej 5% jednego rdzenia przez 1 s z rzędu.
    pub fn wait_driver_idle(max: std::time::Duration) {
        let start = std::time::Instant::now();
        let mut last = process_cpu_ms();
        let mut quiet = 0;
        while quiet < 4 && start.elapsed() < max {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let now = process_cpu_ms();
            quiet = if now.saturating_sub(last) < 12 {
                quiet + 1
            } else {
                0
            };
            last = now;
        }
    }
}

/// Proces kompilujący shadery gry Unity: `--shader-warm-unity <folder _Data>`.
/// Wątki przeglądające pliki gry przekazują unikalne kontenery DXBC do wątku
/// głównego, który tworzy je partiami na urządzeniu D3D11 karty Nvidia.
/// Wiersze stdout: `scan <pliki zrobione> <pliki wszystkie>`,
/// `progress <skompilowane> <znalezione>`, `result <ok> <błędy>`, `error ...`.
///
/// Sterownik Nvidii kompiluje shadery D3D11 w swoich wątkach po powrocie z
/// `Create*Shader` (wywołanie trwa poniżej 2 ms także przy nowym shaderze), a
/// zwolnienie obiektu przed końcem kompilacji ją porzuca. Pomiary na Last
/// Epoch (196 tys. shaderów): zwalnianie od razu - w pamięci podręcznej ok.
/// 140 B na shader i drugi przebieg dopisuje; trzymanie wszystkich - pełna
/// kompilacja (ok. 170 shaderów/s), ale 13 GB pamięci. Dlatego partie:
/// utworzenie `BATCH` shaderów, czekanie na bezczynność wątków sterownika,
/// zwolnienie partii.
#[cfg(windows)]
pub fn run_host_d3d11(args: &[String]) -> i32 {
    use super::unity;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    const BATCH: usize = 4096;

    let out = std::io::stdout();
    let say = |line: String| {
        let mut o = out.lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    };
    // Źródło: "unity <folder _Data>", "dagor <zrzut .shdump.bin DX11>" albo
    // "unreal <paczka .pak>" (archiwa shaderów SM5 z paczki).
    let (kind, source) = match (args.first().map(String::as_str), args.get(1)) {
        (Some(k @ ("unity" | "dagor" | "unreal")), Some(p)) => (k, p.clone()),
        (Some(p), None) => ("unity", p.to_owned()),
        _ => {
            say("fatal użycie: --shader-warm-d3d11 <unity|dagor|unreal> <ścieżka>".into());
            return 2;
        }
    };
    let data_dir = &source;
    // Unreal: archiwa SM5 (DXBC) wczytane w całości (Palworld: 191 + 9 MB).
    let unreal_archives: Vec<(super::unreal_pak::Archive, Vec<u8>)> = if kind == "unreal" {
        match super::unreal_pak::sm5_archives(Path::new(&source)) {
            Ok(list) => list,
            Err(e) => {
                say(format!("fatal {e}"));
                return 3;
            }
        }
    } else {
        Vec::new()
    };
    // (archiwum, shader) w jednej numeracji dla wątków skanujących.
    let unreal_items: Vec<(usize, usize)> = unreal_archives
        .iter()
        .enumerate()
        .flat_map(|(a, (arch, _))| (0..arch.shaders.len()).map(move |s| (a, s)))
        .collect();
    let dagor_dump = if kind == "dagor" {
        match super::dagor::Dump::open(Path::new(&source)) {
            Ok(d) => Some(d),
            Err(e) => {
                say(format!("fatal {e}"));
                return 3;
            }
        }
    } else {
        None
    };
    let device = match d3d11::nvidia_device() {
        Ok(d) => d,
        Err(e) => {
            say(format!("fatal {e}"));
            return 4;
        }
    };
    say(format!("adapter {}", device.adapter));
    let mut device = Some(device);

    let files = if dagor_dump.is_some() || kind == "unreal" {
        Vec::new()
    } else {
        unity::candidate_files(Path::new(data_dir))
    };
    let scan_total = dagor_dump
        .as_ref()
        .map_or(files.len(), |d| d.shader_count())
        .max(unreal_items.len());
    say(format!("scan 0 {scan_total}"));
    let next_file = AtomicUsize::new(0);
    let next_item = AtomicUsize::new(0);
    let items_done = AtomicUsize::new(0);
    let files_done = AtomicUsize::new(0);
    let found = AtomicUsize::new(0);
    let seen = Mutex::new(std::collections::HashSet::<[u8; 16]>::new());
    let errors = Mutex::new(Vec::<String>::new());
    // Mały bufor: gdy główny wątek czeka na sterownik, skanery stoją i nie
    // zakłócają pomiaru bezczynności.
    let (tx, rx) = std::sync::mpsc::sync_channel::<(unity::Stage, Vec<u8>)>(256);
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let scanners = (cpus / 4).clamp(1, 4);
    let (mut done, mut ok) = (0usize, 0usize);

    std::thread::scope(|scope| {
        // Dagor DX11: jeden zrzut, shadery rozpakowywane po kolei.
        if let Some(dump) = &dagor_dump {
            let tx = tx.clone();
            let (seen, found, errors, say) = (&seen, &found, &errors, &say);
            scope.spawn(move || {
                let result = dump.for_each_shader(|i, data| {
                    unity::carve_dxbc(data, |d| {
                        if seen.lock().unwrap().insert(d.digest()) {
                            found.fetch_add(1, Relaxed);
                            let _ = tx.send((d.stage, d.code.to_vec()));
                        }
                    });
                    if (i + 1) % 200 == 0 || i + 1 == scan_total {
                        say(format!("scan {} {scan_total}", i + 1));
                    }
                    true
                });
                if let Err(e) = result {
                    errors.lock().unwrap().push(e);
                }
                say(format!("scan {scan_total} {scan_total}"));
            });
        }
        // Unreal: rozpakowanie (Oodle) i wycięcie DXBC, kilka wątków.
        for _ in 0..if unreal_items.is_empty() { 0 } else { scanners } {
            let tx = tx.clone();
            let (items, archives, next_item, items_done, seen, found, errors, say) = (
                &unreal_items,
                &unreal_archives,
                &next_item,
                &items_done,
                &seen,
                &found,
                &errors,
                &say,
            );
            scope.spawn(move || loop {
                let i = next_item.fetch_add(1, Relaxed);
                let Some(&(a, s)) = items.get(i) else { break };
                let (arch, bytes) = &archives[a];
                match arch.code(bytes, s, None) {
                    Ok(code) => unity::carve_dxbc(&code, |d| {
                        if seen.lock().unwrap().insert(d.digest()) {
                            found.fetch_add(1, Relaxed);
                            let _ = tx.send((d.stage, d.code.to_vec()));
                        }
                    }),
                    Err(e) => {
                        let mut list = errors.lock().unwrap();
                        if list.len() < 10 {
                            list.push(e);
                        }
                    }
                }
                let n = items_done.fetch_add(1, Relaxed) + 1;
                if n % 500 == 0 || n == items.len() {
                    say(format!("scan {n} {}", items.len()));
                }
            });
        }
        for _ in 0..scanners.min(files.len()) {
            let tx = tx.clone();
            let (files, next_file, files_done, seen, found, errors, say) = (
                &files,
                &next_file,
                &files_done,
                &seen,
                &found,
                &errors,
                &say,
            );
            scope.spawn(move || loop {
                let i = next_file.fetch_add(1, Relaxed);
                let Some(path) = files.get(i) else { break };
                let result = unity::scan_file(path, &mut |d| {
                    if seen.lock().unwrap().insert(d.digest()) {
                        found.fetch_add(1, Relaxed);
                        let _ = tx.send((d.stage, d.code.to_vec()));
                    }
                });
                if let Err(e) = result {
                    let mut list = errors.lock().unwrap();
                    if list.len() < 10 {
                        list.push(format!("{}: {e}", path.display()));
                    }
                }
                let n = files_done.fetch_add(1, Relaxed) + 1;
                if n % 100 == 0 || n == files.len() {
                    say(format!("scan {n} {}", files.len()));
                }
            });
        }
        drop(tx);
        let mut jobs = rx.iter().peekable();
        while jobs.peek().is_some() {
            // Każda partia na nowym urządzeniu: sterownik trzyma wyniki w pamięci
            // do zwolnienia urządzenia (jedno urządzenie dla Last Epoch - 12,5 GB).
            let device = match device.take().map_or_else(d3d11::nvidia_device, Ok) {
                Ok(d) => d,
                Err(e) => {
                    say(format!("fatal {e}"));
                    return;
                }
            };
            // Shadery partii żyją, aż sterownik skończy je kompilować.
            let mut keep = Vec::with_capacity(BATCH);
            let mut count = 0;
            for (stage, code) in jobs.by_ref().take(BATCH) {
                count += 1;
                match device.create(stage, &code) {
                    Ok(shader) => keep.push(shader),
                    Err(e) => {
                        let mut list = errors.lock().unwrap();
                        if list.len() < 10 {
                            list.push(format!("{stage:?}: {e}"));
                        }
                    }
                }
            }
            d3d11::wait_driver_idle(std::time::Duration::from_secs(600));
            ok += keep.len();
            done += count;
            drop(keep);
            drop(device);
            say(format!("progress {done} {}", found.load(Relaxed)));
        }
    });
    for e in errors.into_inner().unwrap() {
        say(format!("error {e}"));
    }
    say(format!("result {ok} {}", done - ok));
    0
}

// ----------------------- pliki pamięci podręcznej gry ----------------------

/// Argument procesu ustalającego pliki pamięci podręcznej gry.
pub const HOST_ARG_PROBE: &str = "--shader-cache-probe";

/// Pliki w folderze, których nie da się teraz otworzyć na wyłączność -
/// trzyma je otwarte jakiś proces (sterownik w jego imieniu).
#[cfg(windows)]
fn locked_files(dir: &Path) -> std::collections::BTreeSet<String> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut out = std::collections::BTreeSet::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let open = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path);
        // 32 = ERROR_SHARING_VIOLATION
        if matches!(&open, Err(err) if err.raw_os_error() == Some(32)) {
            out.insert(e.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

/// Proces pomiaru (uruchamiany pod nazwą pliku .exe gry): tworzy urządzenia
/// D3D11 i D3D12 na karcie Nvidia i wypisuje `file <nazwa>` dla plików
/// pamięci podręcznej, które sterownik otworzył dla tej nazwy procesu.
/// Sterownik wiąże pliki z nazwą .exe (skrót nazwy w nazwie pliku), więc są to
/// dokładnie pliki tej gry - bez znajomości algorytmu skrótu.
#[cfg(windows)]
pub fn run_cache_probe() -> i32 {
    let out = std::io::stdout();
    let say = |line: String| {
        let mut o = out.lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    };
    let Some(dir) = super::nvidia_cache_dir() else {
        say("fatal brak folderu DXCache".into());
        return 2;
    };
    let before = locked_files(&dir);
    let dx11 = d3d11::nvidia_device();
    let dx12 = d3d::nvidia_device();
    if let Err(e) = &dx11 {
        say(format!("warning D3D11: {e}"));
    }
    if let Err(e) = &dx12 {
        say(format!("warning D3D12: {e}"));
    }
    // Sterownik otwiera pliki przy tworzeniu urządzenia; chwila zapasu.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let after = locked_files(&dir);
    for name in after.difference(&before) {
        say(format!("file {name}"));
    }
    drop((dx11, dx12));
    0
}

/// Ile kolejnych numerów pliku traktujemy jako tę samą grę (sterownik
/// zakłada plik o numerze +1, gdy poprzedni jest pełny albo zajęty).
const CACHE_FAMILY: u32 = 8;

/// Ostatnie 8 cyfr szesnastkowych nazwy pliku pamięci podręcznej
/// (`1c68a91ac748b201.nvph` -> 0xC748B201).
pub fn cache_key(name: &str) -> Option<u32> {
    let stem = name.split('.').next()?;
    if stem.len() < 16 || !stem.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&stem[stem.len() - 8..], 16).ok()
}

/// Czy plik należy do gry o kluczach `keys` (ten sam skrót lub kolejne numery).
pub fn in_family(name: &str, keys: &[u32]) -> bool {
    cache_key(name).is_some_and(|k| keys.iter().any(|&g| k.abs_diff(g) <= CACHE_FAMILY))
}

/// Wynik czyszczenia pamięci podręcznej gry.
#[derive(Clone, Debug, PartialEq)]
pub enum CleanState {
    Idle,
    Running,
    Done {
        files: usize,
        bytes: u64,
        locked: usize,
    },
    Failed(String),
}

/// Czyszczenie shaderów jednej gry w tle.
#[derive(Clone)]
pub struct Clean {
    pub state: Arc<Mutex<CleanState>>,
}

impl Default for Clean {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(CleanState::Idle)),
        }
    }
}

impl Clean {
    /// Ustala pliki gry (proces pomiaru pod nazwą `exe_name`) i je usuwa.
    pub fn start(&self, exe_name: &str, wake: impl Fn() + Send + 'static) {
        {
            let mut s = self.state.lock().unwrap();
            if *s == CleanState::Running {
                return;
            }
            *s = CleanState::Running;
        }
        let shared = self.state.clone();
        let exe = exe_name.to_owned();
        std::thread::spawn(move || {
            let result = clean_game(&exe);
            *shared.lock().unwrap() = match result {
                Ok((files, bytes, locked)) => CleanState::Done {
                    files,
                    bytes,
                    locked,
                },
                Err(e) => CleanState::Failed(e),
            };
            wake();
        });
    }
}

/// Ścieżki .exe zapisane w bazie pamięci podręcznej shaderów Windows
/// (`D3DSCache\<skrót>\*.dxcache` = SQLite; tabela `app_id(exe_path, ...)`
/// leży w pierwszych stronach pliku, tekst ASCII).
pub fn dxcache_exe_paths(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i].is_ascii_alphabetic() && bytes[i + 1] == b':' && bytes[i + 2] == b'\\' {
            let mut end = i + 3;
            while end < bytes.len() && (0x20..0x7F).contains(&bytes[end]) && end - i < 520 {
                end += 1;
            }
            let text = String::from_utf8_lossy(&bytes[i..end]);
            let lower = text.to_ascii_lowercase();
            if let Some(pos) = lower.find(".exe") {
                out.push(text[..pos + 4].to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Foldery `D3DSCache` należące do programu o nazwie pliku `exe` (z dowolnej
/// ścieżki: gra, kopie robocze SCSKiller, kompilacja Tune Center).
fn d3dscache_folders(exe: &str) -> Vec<PathBuf> {
    use std::io::Read;
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let root = PathBuf::from(local).join("D3DSCache");
    let wanted = exe.to_lowercase();
    let mut out = Vec::new();
    for dir in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let dir = dir.path();
        if !dir.is_dir() {
            continue;
        }
        let matches = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|f| {
                let path = f.path();
                if path.extension().is_none_or(|e| e != "dxcache") {
                    return false;
                }
                let mut buf = Vec::new();
                let read = std::fs::File::open(&path)
                    .and_then(|file| file.take(256 * 1024).read_to_end(&mut buf));
                read.is_ok()
                    && dxcache_exe_paths(&buf).iter().any(|p| {
                        p.rsplit('\\')
                            .next()
                            .is_some_and(|name| name.to_lowercase() == wanted)
                    })
            });
        if matches {
            out.push(dir);
        }
    }
    out
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Pliki pamięci podręcznej gry ustalone pomiarem, potem usunięte.
/// Zwraca (usunięte pliki, bajty, pliki zajęte przez inny proces).
fn clean_game(exe: &str) -> Result<(usize, u64, usize), String> {
    #[cfg(windows)]
    if process_running(exe) {
        return Err(crate::trf!(
            "Gra {exe} jest uruchomiona - zamknij ją przed czyszczeniem.",
            "The game {exe} is running - close it before cleaning."
        ));
    }
    let dir = super::nvidia_cache_dir().ok_or("brak folderu DXCache")?;
    let host = staged_host(exe)?;
    let mut cmd = std::process::Command::new(&host);
    cmd.arg(HOST_ARG_PROBE)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&host);
    let output = output?;
    let text = String::from_utf8_lossy(&output.stdout);
    let keys: Vec<u32> = text
        .lines()
        .filter_map(|l| l.strip_prefix("file "))
        .filter_map(cache_key)
        .collect();
    if keys.is_empty() {
        let detail = text
            .lines()
            .find(|l| l.starts_with("fatal") || l.starts_with("warning"));
        return Err(match detail {
            Some(d) => d.to_owned(),
            None => crate::i18n::tr(
                "nie udało się ustalić plików pamięci podręcznej gry",
                "could not determine the game's cache files",
            )
            .to_string(),
        });
    }
    let (mut files, mut bytes, mut locked) = (0, 0, 0);
    for e in std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if !in_family(&name, &keys) {
            continue;
        }
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(e.path()) {
            Ok(()) => {
                files += 1;
                bytes += len;
            }
            Err(_) => locked += 1,
        }
    }
    // Pamięć podręczna shaderów Windows (D3DSCache): DirectX 12 gry, kopie
    // robocze SCSKiller i nasze kompilacje - folder na każdą ścieżkę .exe.
    for dir in d3dscache_folders(exe) {
        let size = dir_size(&dir);
        let count = std::fs::read_dir(&dir).map(|r| r.count()).unwrap_or(0);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                files += count;
                bytes += size;
            }
            Err(_) => locked += count.max(1),
        }
    }
    Ok((files, bytes, locked))
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
    /// Przegląd plików gry (Unity): (przejrzane, wszystkie).
    pub scan: Option<(usize, usize)>,
    pub adapter: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Co kompilować.
#[derive(Clone, Debug)]
pub enum Job {
    /// Zrzut shaderów Dagor DirectX 12 (compute) i tryb buforów stałych.
    Dagor { dump: PathBuf, cbv_ranges: bool },
    /// Folder `*_Data` gry Unity (DirectX 11).
    Unity { data_dir: PathBuf },
    /// Zrzut shaderów Dagor DirectX 11 (kontenery DXBC).
    DagorDx11 { dump: PathBuf },
    /// Paczka `.pak` gry Unreal (archiwa shaderów SM5, DirectX 11).
    Unreal { pak: PathBuf },
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
    // Testy sprzętowe: plik testowy nie jest programem - wskazujemy tunecenter.exe.
    #[cfg(test)]
    let me = std::env::var_os("TC_HOST_EXE").map_or(me, PathBuf::from);
    let dir = std::env::temp_dir().join("TuneCenter").join("warm");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(exe_name);
    std::fs::copy(&me, &target)
        .map_err(|e| crate::trf!("kopiowanie programu: {e}", "copying the program: {e}"))?;
    Ok(target)
}

impl Warm {
    /// Uruchamia kompilację w procesie nazwanym `exe_name`.
    pub fn start(&self, exe_name: &str, job: Job, wake: impl Fn() + Send + 'static) {
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
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(&host);
            match &job {
                Job::Dagor { dump, cbv_ranges } => {
                    cmd.arg(HOST_ARG)
                        .arg(dump)
                        .arg(if *cbv_ranges { "1" } else { "0" });
                }
                Job::Unity { data_dir } => {
                    cmd.arg(HOST_ARG_D3D11).arg("unity").arg(data_dir);
                }
                Job::Unreal { pak } => {
                    cmd.arg(HOST_ARG_D3D11).arg("unreal").arg(pak);
                }
                Job::DagorDx11 { dump } => {
                    cmd.arg(HOST_ARG_D3D11).arg("dagor").arg(dump);
                }
            }
            cmd.stdout(std::process::Stdio::piped())
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
        "scan" if nums.len() == 2 => p.scan = Some((nums[0], nums[1])),
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

/// Plik .exe gry Unreal: `<Projekt>\Binaries\Win64\*-Shipping.exe` (właściwy
/// proces gry; plik w głównym folderze to tylko program uruchamiający).
pub fn unreal_game_exe(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| !e.file_name().eq_ignore_ascii_case("Engine"))
        .filter_map(|e| std::fs::read_dir(e.path().join("Binaries").join("Win64")).ok())
        .flat_map(|list| list.flatten())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            (lower.ends_with(".exe") && !lower.contains("crash")).then(|| {
                (
                    lower.contains("-shipping"),
                    e.metadata().map(|m| m.len()).unwrap_or(0),
                    name,
                )
            })
        })
        .max()
        .map(|(_, _, n)| n)
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
    fn finds_exe_paths_in_d3dscache_db() {
        let mut db = vec![0u8, 7, 21];
        db.extend_from_slice(br#"D:\Steam\steamapps\common\Last Epoch\Last Epoch.exe"#);
        db.extend_from_slice(&[0, 1, b'x']);
        db.extend_from_slice(br#"C:\nope\file.txt"#);
        db.push(0);
        assert_eq!(
            dxcache_exe_paths(&db),
            vec![r#"D:\Steam\steamapps\common\Last Epoch\Last Epoch.exe"#.to_string()]
        );
    }

    #[test]
    fn cache_file_family() {
        assert_eq!(cache_key("1c68a91ac748b201.nvph"), Some(0xC748_B201));
        assert_eq!(cache_key("0002a91a62c6cdf9.nvph"), Some(0x62C6_CDF9));
        assert_eq!(cache_key("index.toc"), None);
        let keys = [0x62C6_CDFA];
        assert!(in_family("1c68a91a62c6cdf9.nvph", &keys));
        assert!(in_family("0002a91a62c6cdfa.nvph", &keys));
        assert!(!in_family("1c68a91a62c6ce10.nvph", &keys));
        assert!(!in_family("1c68a91ac748b201.nvph", &keys));
    }

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

#[cfg(test)]
mod clean_hardware {
    //! Czyszczenie pamięci podręcznej gry (USUWA pliki!):
    //! `TC_CLEAN_EXE=PillarsOfEternity.exe cargo test clean_game_cache -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn clean_game_cache() {
        let exe = std::env::var("TC_CLEAN_EXE").expect("TC_CLEAN_EXE");
        println!("foldery D3DSCache: {:?}", super::d3dscache_folders(&exe));
        println!("wynik: {:?}", super::clean_game(&exe));
    }
}

#[cfg(test)]
mod clean_list {
    //! Bez usuwania: foldery D3DSCache gry.
    //! `TC_CLEAN_EXE=<exe> cargo test d3dscache_list -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn d3dscache_list() {
        let exe = std::env::var("TC_CLEAN_EXE").expect("TC_CLEAN_EXE");
        for d in super::d3dscache_folders(&exe) {
            println!("{} {} MB", d.display(), super::dir_size(&d) >> 20);
        }
    }
}
