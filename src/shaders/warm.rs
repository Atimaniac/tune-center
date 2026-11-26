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
pub(super) const CACHED_MS: u64 = 250;

// ------------------------------ proces kompilujący --------------------------

#[cfg(windows)]
pub(super) mod d3d {
    use std::collections::HashMap;
    use std::ffi::c_void;

    use windows::core::Interface;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;

    use super::super::Vendor;

    pub struct Device {
        pub device: ID3D12Device,
        pub adapter: String,
        pub vendor: Vendor,
        signatures: std::sync::Mutex<HashMap<Vec<u8>, ID3D12RootSignature>>,
    }

    // ID3D12Device jest wolnowątkowy (free-threaded) wg dokumentacji D3D12.
    unsafe impl Send for Device {}
    unsafe impl Sync for Device {}

    /// Urządzenie D3D12 na karcie NVIDIA albo AMD (patrz `pick_adapter`).
    pub fn gpu_device() -> Result<Device, String> {
        let (adapter, vendor, name) = super::super::pick_adapter()?;
        // SAFETY: karta z DXGI, poprawne argumenty.
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device)
                .map_err(|e| format!("D3D12CreateDevice: {e}"))?;
            let device = device.ok_or("D3D12CreateDevice: brak urządzenia")?;
            Ok(Device {
                device,
                adapter: name,
                vendor,
                signatures: Default::default(),
            })
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

        /// Potok graficzny ze stanem `state` (etapy puste = brak etapu).
        pub fn graphics(
            &self,
            rs: &ID3D12RootSignature,
            stages: &super::GraphicsStages,
            state: &super::GraphicsState,
        ) -> Result<(), String> {
            use windows::Win32::Graphics::Dxgi::Common::*;
            let code = |b: &[u8]| D3D12_SHADER_BYTECODE {
                pShaderBytecode: if b.is_empty() { std::ptr::null() } else { b.as_ptr() as *const c_void },
                BytecodeLength: b.len(),
            };
            // Nazwy semantyk: teksty zakończone zerem, żyją do końca wywołania.
            let names: Vec<std::ffi::CString> = state
                .inputs
                .iter()
                .map(|i| std::ffi::CString::new(i.semantic.as_str()).unwrap_or_default())
                .collect();
            let inputs: Vec<D3D12_INPUT_ELEMENT_DESC> = state
                .inputs
                .iter()
                .zip(&names)
                .map(|(i, n)| D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: windows::core::PCSTR(n.as_ptr() as *const u8),
                    SemanticIndex: i.index,
                    Format: DXGI_FORMAT(i.format as i32),
                    InputSlot: i.slot,
                    AlignedByteOffset: i.offset,
                    InputSlotClass: if i.instance_rate > 0 {
                        D3D12_INPUT_CLASSIFICATION_PER_INSTANCE_DATA
                    } else {
                        D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA
                    },
                    InstanceDataStepRate: i.instance_rate,
                })
                .collect();
            let s = state;
            let mut blend = D3D12_BLEND_DESC {
                AlphaToCoverageEnable: s.alpha_to_coverage.into(),
                IndependentBlendEnable: s.independent_blend.into(),
                ..Default::default()
            };
            for (rt, b) in blend.RenderTarget.iter_mut().zip(&s.blend) {
                rt.BlendEnable = b.enable.into();
                rt.SrcBlend = D3D12_BLEND(b.src as i32);
                rt.DestBlend = D3D12_BLEND(b.dst as i32);
                rt.BlendOp = D3D12_BLEND_OP(b.op as i32);
                rt.SrcBlendAlpha = D3D12_BLEND(b.src_alpha as i32);
                rt.DestBlendAlpha = D3D12_BLEND(b.dst_alpha as i32);
                rt.BlendOpAlpha = D3D12_BLEND_OP(b.op_alpha as i32);
                rt.LogicOp = D3D12_LOGIC_OP_NOOP;
                rt.RenderTargetWriteMask = b.mask;
            }
            let mut rtv = [DXGI_FORMAT_UNKNOWN; 8];
            for (slot, f) in rtv.iter_mut().zip(&s.render_targets) {
                *slot = DXGI_FORMAT(*f as i32);
            }
            let stencil_op = D3D12_DEPTH_STENCILOP_DESC {
                StencilFailOp: D3D12_STENCIL_OP(s.stencil_fail as i32),
                StencilDepthFailOp: D3D12_STENCIL_OP(s.stencil_depth_fail as i32),
                StencilPassOp: D3D12_STENCIL_OP(s.stencil_pass as i32),
                StencilFunc: D3D12_COMPARISON_FUNC(s.stencil_func as i32),
            };
            let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                // SAFETY: kopia wskaźnika bez AddRef (jak w `compute`).
                pRootSignature: unsafe { std::mem::transmute_copy(rs) },
                VS: code(&stages.vs),
                PS: code(&stages.ps),
                DS: code(&stages.ds),
                HS: code(&stages.hs),
                GS: code(&stages.gs),
                StreamOutput: D3D12_STREAM_OUTPUT_DESC::default(),
                BlendState: blend,
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: if s.wireframe { D3D12_FILL_MODE_WIREFRAME } else { D3D12_FILL_MODE_SOLID },
                    CullMode: D3D12_CULL_MODE(s.cull as i32),
                    FrontCounterClockwise: s.front_ccw.into(),
                    DepthBias: s.depth_bias,
                    DepthBiasClamp: 0.0,
                    SlopeScaledDepthBias: s.slope_bias,
                    DepthClipEnable: s.depth_clip.into(),
                    MultisampleEnable: false.into(),
                    AntialiasedLineEnable: false.into(),
                    ForcedSampleCount: s.forced_samples,
                    ConservativeRaster: if s.conservative && !s.wireframe {
                        D3D12_CONSERVATIVE_RASTERIZATION_MODE_ON
                    } else {
                        D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF
                    },
                },
                DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                    DepthEnable: s.depth_enable.into(),
                    DepthWriteMask: if s.depth_write { D3D12_DEPTH_WRITE_MASK_ALL } else { D3D12_DEPTH_WRITE_MASK_ZERO },
                    DepthFunc: D3D12_COMPARISON_FUNC(s.depth_func as i32),
                    StencilEnable: s.stencil_enable.into(),
                    StencilReadMask: s.stencil_read,
                    StencilWriteMask: s.stencil_write,
                    FrontFace: stencil_op,
                    BackFace: stencil_op,
                },
                InputLayout: D3D12_INPUT_LAYOUT_DESC {
                    pInputElementDescs: if inputs.is_empty() { std::ptr::null() } else { inputs.as_ptr() },
                    NumElements: inputs.len() as u32,
                },
                IBStripCutValue: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE(s.topology as i32),
                NumRenderTargets: s.render_targets.len().min(8) as u32,
                RTVFormats: rtv,
                DSVFormat: DXGI_FORMAT(s.depth_format as i32),
                SampleDesc: DXGI_SAMPLE_DESC { Count: s.samples.max(1), Quality: 0 },
                NodeMask: 0,
                CachedPSO: D3D12_CACHED_PIPELINE_STATE::default(),
                Flags: D3D12_PIPELINE_STATE_FLAG_NONE,
            };
            // SAFETY: opis z poprawnymi wskaźnikami na czas wywołania.
            let pso: windows::core::Result<ID3D12PipelineState> =
                unsafe { self.device.CreateGraphicsPipelineState(&desc) };
            pso.map(|_| ()).map_err(|e| format!("{e}"))
        }

        /// Potok compute z gotowego bloba root signature.
        pub fn compute_with(&self, rs_blob: &[u8], cs: &[u8]) -> Result<(), String> {
            let rs = self.root_signature(rs_blob)?;
            self.compute(&rs, cs)
        }

        /// Czy karta obsługuje mesh shadery (D3D12_OPTIONS7).
        pub fn mesh_shaders(&self) -> bool {
            let mut o = D3D12_FEATURE_DATA_D3D12_OPTIONS7::default();
            // SAFETY: struktura opcji o właściwym rozmiarze.
            unsafe {
                self.device.CheckFeatureSupport(
                    D3D12_FEATURE_D3D12_OPTIONS7,
                    &mut o as *mut _ as *mut c_void,
                    std::mem::size_of_val(&o) as u32,
                )
            }
            .is_ok()
                && o.MeshShaderTier != D3D12_MESH_SHADER_TIER_NOT_SUPPORTED
        }

        pub fn raw(&self) -> *mut c_void {
            self.device.as_raw()
        }

        /// Prosi sterownik o zapisanie wyników kompilacji w tle i czeka na
        /// koniec (`ID3D12Device6::SetBackgroundProcessingMode`, publiczne
        /// D3D12). Sterownik AMD zapisuje pamięć podręczną za wywołaniami
        /// `Create*PipelineState`; zakończenie procesu przed zapisem gubi
        /// resztę.
        pub fn commit(&self, max: std::time::Duration) -> Result<(), String> {
            use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
            let device6: ID3D12Device6 = self
                .device
                .cast()
                .map_err(|e| format!("ID3D12Device6: {e}"))?;
            // SAFETY: zdarzenie tworzone i zamykane tutaj; urządzenie żyje.
            unsafe {
                let event = CreateEventW(None, true, false, None)
                    .map_err(|e| format!("CreateEventW: {e}"))?;
                let result = device6.SetBackgroundProcessingMode(
                    D3D12_BACKGROUND_PROCESSING_MODE_ALLOWED,
                    D3D12_MEASUREMENTS_ACTION_COMMIT_RESULTS,
                    Some(event),
                    None,
                );
                if result.is_ok() {
                    let ms = max.as_millis().min(u32::MAX as u128) as u32;
                    WaitForSingleObject(event, ms);
                }
                let _ = windows::Win32::Foundation::CloseHandle(event);
                result.map_err(|e| format!("SetBackgroundProcessingMode: {e}"))
            }
        }
    }
}

/// Shader ze zrzutu Dagora potrzebny potokom graficznym: nagłówek i kod.
enum Stored {
    Pixel((dagor::Header, Vec<u8>)),
    Vertex {
        vs: (dagor::Header, Vec<u8>),
        hs: Option<(dagor::Header, Vec<u8>)>,
        ds: Option<(dagor::Header, Vec<u8>)>,
        gs: Option<(dagor::Header, Vec<u8>)>,
    },
}

/// Urządzenie D3D12 dla testów sprzętowych innych modułów.
#[cfg(all(test, windows))]
pub fn test_device() -> Result<d3d::Device, String> {
    d3d::gpu_device()
}

/// Kod etapów potoku graficznego (pusty = brak etapu).
#[derive(Clone, Debug, Default)]
pub struct GraphicsStages {
    pub vs: Vec<u8>,
    pub ps: Vec<u8>,
    pub hs: Vec<u8>,
    pub ds: Vec<u8>,
    pub gs: Vec<u8>,
}

/// Element układu wejścia (format jako wartość DXGI_FORMAT).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InputElement {
    pub semantic: String,
    pub index: u32,
    pub format: u32,
    pub slot: u32,
    pub offset: u32,
    /// 0 = dane wierzchołków, >0 = dane instancji co tyle instancji.
    pub instance_rate: u32,
}

/// Mieszanie jednego celu renderowania (wartości wyliczeń D3D12).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blend {
    pub enable: bool,
    pub src: u32,
    pub dst: u32,
    pub op: u32,
    pub src_alpha: u32,
    pub dst_alpha: u32,
    pub op_alpha: u32,
    /// D3D12_COLOR_WRITE_ENABLE (4 bity).
    pub mask: u8,
}

impl Default for Blend {
    fn default() -> Self {
        // ONE, ZERO, ADD - mieszanie wyłączone.
        Self { enable: false, src: 2, dst: 1, op: 1, src_alpha: 2, dst_alpha: 1, op_alpha: 1, mask: 0xF }
    }
}

/// Stan potoku graficznego (wartości wyliczeń D3D12 i DXGI_FORMAT).
/// Domyślny jest neutralny: sterownik NVIDIA nie wlicza stanu do klucza
/// pamięci podręcznej, AMD i Intel - tak, więc tam stan musi być gry.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphicsState {
    pub inputs: Vec<InputElement>,
    /// Formaty celów renderowania, do 8 (0 = brak celu w tym gnieździe).
    pub render_targets: Vec<u32>,
    pub samples: u32,
    /// DXGI_FORMAT bufora głębi (0 = brak).
    pub depth_format: u32,
    /// D3D12_PRIMITIVE_TOPOLOGY_TYPE (3 = trójkąty, 4 = łaty).
    pub topology: u32,
    pub wireframe: bool,
    /// D3D12_CULL_MODE (1 brak, 2 przód, 3 tył).
    pub cull: u32,
    /// Przód trójkąta przeciwnie do wskazówek zegara (UE: tak, Dagor: nie).
    pub front_ccw: bool,
    pub depth_bias: i32,
    pub slope_bias: f32,
    pub depth_clip: bool,
    pub forced_samples: u32,
    pub conservative: bool,
    pub depth_enable: bool,
    pub depth_write: bool,
    pub depth_func: u32,
    pub stencil_enable: bool,
    pub stencil_read: u8,
    pub stencil_write: u8,
    pub stencil_func: u32,
    pub stencil_fail: u32,
    pub stencil_depth_fail: u32,
    pub stencil_pass: u32,
    pub alpha_to_coverage: bool,
    pub independent_blend: bool,
    pub blend: [Blend; 8],
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            render_targets: Vec::new(),
            samples: 1,
            depth_format: 0,
            topology: 3,
            wireframe: false,
            cull: 1,
            front_ccw: false,
            depth_bias: 0,
            slope_bias: 0.0,
            depth_clip: true,
            forced_samples: 0,
            conservative: false,
            depth_enable: false,
            depth_write: false,
            depth_func: 7, // GREATER_EQUAL
            stencil_enable: false,
            stencil_read: 0xFF,
            stencil_write: 0xFF,
            stencil_func: 8, // ALWAYS
            stencil_fail: 1, // KEEP
            stencil_depth_fail: 1,
            stencil_pass: 1,
            alpha_to_coverage: false,
            independent_blend: false,
            blend: [Blend::default(); 8],
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
        let lib = crate::dll::Library::new("nvapi64.dll").map_err(|e| e.to_string())?;
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
    let device = match d3d::gpu_device() {
        Ok(d) => d,
        Err(e) => {
            say(format!("fatal {e}"));
            return 4;
        }
    };
    say(format!("adapter {}", device.adapter));
    // Gniazdo rozszerzeń NVAPI tylko na karcie NVIDIA (AMD go nie ma).
    if device.vendor == super::Vendor::Nvidia {
        if let Err(e) = set_nvidia_extension_slot(device.raw()) {
            say(format!("warning {e}"));
        }
    }

    // Pamięć potoków gry (cache\dx12.cache): potoki graficzne do odtworzenia.
    let pipelines = args.get(2).and_then(|p| match super::dagor_cache::Cache::open(Path::new(p)) {
        Ok(c) => Some(c),
        Err(e) => {
            say(format!("warning {e}"));
            None
        }
    });
    // Skróty shaderów potrzebnych potokom graficznym.
    let wanted: std::collections::HashSet<[u8; 20]> = pipelines
        .iter()
        .flat_map(|c| c.graphics.iter().flat_map(|g| [g.vs, g.ps]))
        .collect();
    let mut stages: std::collections::HashMap<[u8; 20], Stored> = Default::default();

    // Lista zadań: shadery compute z rozpakowanym kodem.
    let mut jobs: Vec<(usize, StageCode, Vec<u8>)> = Vec::new();
    let read = dump.for_each_shader(|i, data| {
        let hash = dump
            .metadata(i)
            .and_then(|m| m.get(4..24))
            .map(|h| <[u8; 20]>::try_from(h).unwrap());
        let code = |s: &StageCode| data.get(s.bytecode.clone()).map(|c| (s.header, c.to_vec()));
        match dump.container(i) {
            Some(Container::Single(s)) => {
                if s.header.stage == dagor::Stage::Compute && s.bytecode.end <= data.len() {
                    let code = data[s.bytecode.clone()].to_vec();
                    jobs.push((i, s, code));
                } else if let Some(h) = hash.filter(|h| wanted.contains(h)) {
                    // Shader wierzchołków bez innych etapów też bywa
                    // kontenerem pojedynczym.
                    if let Some(c) = code(&s) {
                        let stored = if s.header.stage == dagor::Stage::Vertex {
                            Stored::Vertex { vs: c, hs: None, ds: None, gs: None }
                        } else {
                            Stored::Pixel(c)
                        };
                        stages.insert(h, stored);
                    }
                }
            }
            Some(Container::Vertex { vertex, hull, domain, geometry }) => {
                if let (Some(h), Some(vs)) = (hash.filter(|h| wanted.contains(h)), code(&vertex)) {
                    stages.insert(
                        h,
                        Stored::Vertex {
                            vs,
                            hs: hull.as_ref().and_then(code),
                            ds: domain.as_ref().and_then(code),
                            gs: geometry.as_ref().and_then(code),
                        },
                    );
                }
            }
            _ => {}
        }
        true
    });
    if let Err(e) = read {
        say(format!("fatal {e}"));
        return 3;
    }
    // Potoki graficzne: (para VS+PS, wariant); siatki (topologia 0) pomijamy.
    let graphics_jobs: Vec<(usize, usize)> = pipelines
        .iter()
        .flat_map(|c| {
            c.graphics.iter().enumerate().flat_map(|(g, p)| {
                p.variants
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.topology != 0)
                    .map(move |(v, _)| (g, v))
            })
        })
        .collect();
    let total = jobs.len() + graphics_jobs.len();
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
    let chunk = jobs.len().div_ceil(threads).max(1);
    let graphics_chunk = graphics_jobs.len().div_ceil(threads).max(1);
    // Potoki graficzne bez shadera pikseli w zrzucie: gra używa wtedy
    // pustego shadera pikseli sterownika Dagora - tworzymy potok bez niego.
    let null_ps = std::sync::atomic::AtomicUsize::new(0);
    let missing_vs = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        if let Some(cache) = &pipelines {
            for part in graphics_jobs.chunks(graphics_chunk) {
                let (device, done, ok, cached, errors, say, stages, null_ps, missing_vs) = (
                    &device, &done, &ok, &cached, &errors, &say, &stages, &null_ps, &missing_vs,
                );
                scope.spawn(move || {
                    use std::sync::atomic::Ordering::Relaxed;
                    for &(g, v) in part {
                        let pair = &cache.graphics[g];
                        let variant = &cache.graphics[g].variants[v];
                        let result = match stages.get(&pair.vs) {
                            Some(Stored::Vertex { vs, hs, ds, gs }) => {
                                let ps = match stages.get(&pair.ps) {
                                    Some(Stored::Pixel(ps)) => Some(ps),
                                    _ => {
                                        null_ps.fetch_add(1, Relaxed);
                                        None
                                    }
                                };
                                let mut headers = vec![&vs.0];
                                headers.extend([ps, hs.as_ref(), ds.as_ref(), gs.as_ref()].into_iter().flatten().map(|s| &s.0));
                                let props = properties(&headers, vs.0.in_out_semantic_mask != 0);
                                let empty = dagor::Usage::default();
                                let sig = rootsig::graphics(
                                    props,
                                    &vs.0.usage,
                                    ps.map_or(&empty, |p| &p.0.usage),
                                    hs.as_ref().map(|s| &s.0.usage),
                                    ds.as_ref().map(|s| &s.0.usage),
                                    gs.as_ref().map(|s| &s.0.usage),
                                    cbv_root_descriptors,
                                );
                                let code = GraphicsStages {
                                    vs: vs.1.clone(),
                                    ps: ps.map(|p| p.1.clone()).unwrap_or_default(),
                                    hs: hs.as_ref().map(|s| s.1.clone()).unwrap_or_default(),
                                    ds: ds.as_ref().map(|s| s.1.clone()).unwrap_or_default(),
                                    gs: gs.as_ref().map(|s| s.1.clone()).unwrap_or_default(),
                                };
                                let state = cache.state(variant);
                                let started = std::time::Instant::now();
                                let r = rootsig::serialize(&sig)
                                    .and_then(|blob| device.root_signature(&blob))
                                    .and_then(|rs| device.graphics(&rs, &code, &state));
                                if r.is_ok() && started.elapsed() < std::time::Duration::from_millis(CACHED_MS) {
                                    cached.fetch_add(1, Relaxed);
                                }
                                r
                            }
                            _ => {
                                missing_vs.fetch_add(1, Relaxed);
                                Err("brak shadera wierzchołków w zrzucie / vertex shader not in dump".into())
                            }
                        };
                        match result {
                            Ok(()) => {
                                ok.fetch_add(1, Relaxed);
                            }
                            Err(e) => {
                                let mut list = errors.lock().unwrap();
                                if list.len() < 10 {
                                    list.push(format!("G{g}/{v}: {e}"));
                                }
                            }
                        }
                        let n = done.fetch_add(1, Relaxed) + 1;
                        if n % 50 == 0 || n == total {
                            say(format!("progress {n} {total}"));
                        }
                    }
                });
            }
        }
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
    if !graphics_jobs.is_empty() {
        say(format!(
            "warning potoki graficzne z dx12.cache / graphics pipelines from dx12.cache: {}, bez shadera pikseli / without pixel shader: {}, brak VS / missing VS: {}",
            graphics_jobs.len(),
            null_ps.into_inner(),
            missing_vs.into_inner()
        ));
    }
    say(format!(
        "result {ok} {} {}",
        total - ok,
        cached.into_inner()
    ));
    // AMD: sterownik zapisuje pamięć podręczną w tle - czekamy na zapis.
    if device.vendor == super::Vendor::Amd {
        if let Err(e) = device.commit(std::time::Duration::from_secs(1800)) {
            say(format!("warning {e}"));
        }
    }
    // Zwolnienie urządzenia przed końcem procesu - sterownik dopisuje wtedy
    // pamięć podręczną na dysk.
    drop(device);
    0
}

// ------------------------ Unreal 5, Direct3D 12 -----------------------------

/// Argument procesu kompilującego potoki DirectX 12 gry Unreal 5.
pub const HOST_ARG_UE12: &str = "--shader-warm-ue12";

/// Proces kompilujący: `--shader-warm-ue12 <folder gry>`. Potoki z pamięci
/// potoków gry (`*.upipelinecache`), shadery SM6 z archiwów gry, root
/// signature jak w RHI D3D12 silnika.
#[cfg(windows)]
pub fn run_host_ue12(args: &[String]) -> i32 {
    use super::ue12;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    let out = std::io::stdout();
    let say = |line: String| {
        let mut o = out.lock();
        let _ = writeln!(o, "{line}");
        let _ = o.flush();
    };
    let Some(game) = args.first().map(PathBuf::from) else {
        say("fatal użycie: --shader-warm-ue12 <folder gry>".into());
        return 2;
    };
    // Potoki ze wszystkich pamięci potoków SM6 gry, bez powtórzeń.
    let mut graphics: Vec<ue12::GraphicsPso> = Vec::new();
    let mut compute: Vec<ue12::Hash> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (name, bytes) in ue12::pso_caches(&game) {
        match ue12::PsoCache::parse(&bytes) {
            Ok(c) => {
                say(format!(
                    "warning {name}: v{} platforma / platform {}, potoki graficzne / graphics {}, compute {}, pominięte / skipped {}",
                    c.version,
                    c.platform,
                    c.graphics.len(),
                    c.compute.len(),
                    c.skipped
                ));
                for g in c.graphics {
                    if seen.insert(format!("{g:?}")) {
                        graphics.push(g);
                    }
                }
                for h in c.compute {
                    if !compute.contains(&h) {
                        compute.push(h);
                    }
                }
            }
            Err(e) => say(format!("warning {name}: {e}")),
        }
    }
    if graphics.is_empty() && compute.is_empty() {
        say("fatal gra nie ma pamięci potoków DirectX 12 (*.upipelinecache) - nie ma z czego odtworzyć potoków".into());
        return 3;
    }
    // Siatki (mesh/amplification) wymagają opisu strumieniowego - pomijamy.
    let mesh = graphics.iter().filter(|g| g.ms != [0; 20]).count();
    graphics.retain(|g| g.ms == [0; 20] && g.vs != [0; 20]);
    let zero = [0u8; 20];
    let wanted: std::collections::HashSet<ue12::Hash> = graphics
        .iter()
        .flat_map(|g| [g.vs, g.ps, g.gs])
        .chain(compute.iter().copied())
        .filter(|h| *h != zero)
        .collect();
    let mut notes = Vec::new();
    let shaders = ue12::load_shaders(&game, &wanted, &mut notes);
    for n in notes.iter().take(5) {
        say(format!("warning {n}"));
    }
    let minor = ue12::game_minor(&game);
    say(format!(
        "warning UE 5.{}, shadery / shaders {} z / of {}, siatki pominięte / mesh skipped {mesh}",
        minor.map_or("?".into(), |m| m.to_string()),
        shaders.len(),
        wanted.len()
    ));
    let device = match d3d::gpu_device() {
        Ok(d) => d,
        Err(e) => {
            say(format!("fatal {e}"));
            return 4;
        }
    };
    say(format!("adapter {}", device.adapter));
    let target = ue12::Target {
        // Bez wersji: 5.4 (próg zmian samplerów); wynik w ostrzeżeniu wyżej.
        minor: minor.unwrap_or(4),
        mesh_shaders: device.mesh_shaders(),
    };
    let total = graphics.len() + compute.len();
    say(format!("progress 0 {total}"));
    let (done, ok, cached, missing) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let errors = Mutex::new(Vec::<String>::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
    // Zadanie: Ok(indeks potoku graficznego) albo Err(indeks compute).
    let jobs: Vec<Result<usize, usize>> = (0..graphics.len()).map(Ok).chain((0..compute.len()).map(Err)).collect();
    let chunk = jobs.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for part in jobs.chunks(chunk) {
            let (device, graphics, compute, shaders, done, ok, cached, missing, errors, say) = (
                &device, &graphics, &compute, &shaders, &done, &ok, &cached, &missing, &errors, &say,
            );
            scope.spawn(move || {
                let packed = |h: &ue12::Hash| shaders.get(h).and_then(|b| ue12::parse_packed(b));
                for job in part {
                    let started = std::time::Instant::now();
                    let result: Result<(), String> = match *job {
                        Ok(i) => {
                            let g = &graphics[i];
                            match (packed(&g.vs), (g.ps != zero).then(|| packed(&g.ps)), (g.gs != zero).then(|| packed(&g.gs))) {
                                (Some(vs), Some(None), _) | (Some(vs), _, Some(None)) => {
                                    let _ = vs;
                                    missing.fetch_add(1, Relaxed);
                                    Err("brak shadera w archiwach / shader not in archives".into())
                                }
                                (Some(vs), ps, gs) => {
                                    let (ps, gs) = (ps.flatten(), gs.flatten());
                                    let mut stages = vec![(ue12::Stage::Vertex, vs.counts)];
                                    if let Some(p) = &ps {
                                        stages.push((ue12::Stage::Pixel, p.counts));
                                    }
                                    if let Some(s) = &gs {
                                        stages.push((ue12::Stage::Geometry, s.counts));
                                    }
                                    let code = GraphicsStages {
                                        vs: vs.container.to_vec(),
                                        ps: ps.map(|p| p.container.to_vec()).unwrap_or_default(),
                                        gs: gs.map(|s| s.container.to_vec()).unwrap_or_default(),
                                        ..Default::default()
                                    };
                                    ue12::serialize(&ue12::root_signature(&stages, target))
                                        .and_then(|blob| device.root_signature(&blob))
                                        .and_then(|rs| device.graphics(&rs, &code, &g.state()))
                                }
                                (None, ..) => {
                                    missing.fetch_add(1, Relaxed);
                                    Err("brak shadera w archiwach / shader not in archives".into())
                                }
                            }
                        }
                        Err(i) => match packed(&compute[i]) {
                            Some(cs) => ue12::serialize(&ue12::root_signature(&[(ue12::Stage::Compute, cs.counts)], target))
                                .and_then(|blob| device.compute_with(&blob, cs.container)),
                            None => {
                                missing.fetch_add(1, Relaxed);
                                Err("brak shadera w archiwach / shader not in archives".into())
                            }
                        },
                    };
                    match result {
                        Ok(()) => {
                            ok.fetch_add(1, Relaxed);
                            if started.elapsed() < std::time::Duration::from_millis(CACHED_MS) {
                                cached.fetch_add(1, Relaxed);
                            }
                        }
                        Err(e) => {
                            let mut list = errors.lock().unwrap();
                            if list.len() < 10 {
                                list.push(match job {
                                    Ok(i) => format!("G{i}: {e}"),
                                    Err(i) => format!("C{i}: {e}"),
                                });
                            }
                        }
                    }
                    let n = done.fetch_add(1, Relaxed) + 1;
                    if n % 50 == 0 || n == total {
                        say(format!("progress {n} {total}"));
                    }
                }
            });
        }
    });
    for e in errors.into_inner().unwrap() {
        say(format!("error {e}"));
    }
    let missing = missing.into_inner();
    if missing > 0 {
        say(format!("warning potoki bez shadera w archiwach / pipelines with a shader missing: {missing}"));
    }
    let ok = ok.into_inner();
    say(format!("result {ok} {} {}", total - ok, cached.into_inner()));
    if device.vendor == super::Vendor::Amd {
        if let Err(e) = device.commit(std::time::Duration::from_secs(1800)) {
            say(format!("warning {e}"));
        }
    }
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

    use super::super::unity::Stage;

    pub struct Device {
        device: ID3D11Device,
        pub adapter: String,
        pub vendor: super::super::Vendor,
    }

    // Metody Create* urządzenia D3D11 są wolnowątkowe (bez flagi
    // D3D11_CREATE_DEVICE_SINGLETHREADED).
    unsafe impl Send for Device {}
    unsafe impl Sync for Device {}

    /// Urządzenie D3D11 na karcie NVIDIA albo AMD (patrz `pick_adapter`).
    pub fn gpu_device() -> Result<Device, String> {
        let (adapter, vendor, name) = super::super::pick_adapter()?;
        // SAFETY: karta z DXGI, poprawne argumenty.
        unsafe {
            {
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
                Ok(Device {
                    device: device.ok_or("D3D11CreateDevice: brak urządzenia")?,
                    adapter: name,
                    vendor,
                })
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

        /// Układ wejścia zbudowany z sygnatury wejść shadera wierzchołków
        /// (ISGN): jeden bufor, elementy kolejno, formaty 32-bitowe o liczbie
        /// składowych z maski. Sterownik AMD kompiluje shader wierzchołków
        /// dopiero z układem wejścia - gry zwykle opisują wejścia tak samo
        /// jak sygnatura, więc trafienie w pamięć podręczną jest prawdopodobne.
        /// `None`: shader bez wejść z bufora albo sygnatura nieczytelna.
        pub fn input_layout(&self, vs: &[u8]) -> Option<ID3D11DeviceChild> {
            use windows::Win32::Graphics::Dxgi::Common::*;
            let elements = super::super::unity::input_signature(vs);
            if elements.is_empty() {
                return None;
            }
            let format = |component: u32, count: u32| -> DXGI_FORMAT {
                let table = match component {
                    1 => [DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R32G32_UINT, DXGI_FORMAT_R32G32B32_UINT, DXGI_FORMAT_R32G32B32A32_UINT],
                    2 => [DXGI_FORMAT_R32_SINT, DXGI_FORMAT_R32G32_SINT, DXGI_FORMAT_R32G32B32_SINT, DXGI_FORMAT_R32G32B32A32_SINT],
                    _ => [DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R32G32B32_FLOAT, DXGI_FORMAT_R32G32B32A32_FLOAT],
                };
                table[(count.clamp(1, 4) - 1) as usize]
            };
            let descs: Vec<D3D11_INPUT_ELEMENT_DESC> = elements
                .iter()
                .map(|e| D3D11_INPUT_ELEMENT_DESC {
                    // Nazwa zakończona zerem leży w kodzie shadera (żyje do
                    // końca wywołania).
                    SemanticName: windows::core::PCSTR(vs[e.name_at..].as_ptr()),
                    SemanticIndex: e.index,
                    Format: format(e.component, e.count),
                    InputSlot: 0,
                    AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                })
                .collect();
            let mut layout = None;
            // SAFETY: opisy i kod żyją do końca wywołania.
            unsafe { self.device.CreateInputLayout(&descs, vs, Some(&mut layout)) }.ok()?;
            layout.map(ID3D11DeviceChild::from)
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
        (Some(k @ ("unity" | "dagor" | "unreal" | "iostore" | "source2" | "loose" | "luminous" | "gow1" | "frostbite")), Some(p)) => {
            (k, p.clone())
        }
        (Some(p), None) => ("unity", p.to_owned()),
        _ => {
            say("fatal użycie: --shader-warm-d3d11 <unity|dagor|unreal|source2|loose> <ścieżka>".into());
            return 2;
        }
    };
    // Source 2: pliki .vcs DirectX z paczek VPK gry (folder gry).
    let source2_vpks: Vec<super::source2::Vpk> = if kind == "source2" {
        super::source2::shader_vpks(Path::new(&source))
            .iter()
            .filter_map(|p| match super::source2::Vpk::open(p) {
                Ok(v) => Some(v),
                Err(e) => {
                    say(format!("warning {}: {e}", p.display()));
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let source2_items: Vec<(usize, usize)> = source2_vpks
        .iter()
        .enumerate()
        .flat_map(|(v, vpk)| {
            vpk.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| super::source2::is_dx_shader(&e.path))
                .map(move |(i, _)| (v, i))
        })
        .collect();
    if kind == "source2" && source2_items.is_empty() {
        say("fatal nie znaleziono plików .vcs DirectX w paczkach VPK gry".into());
        return 3;
    }
    // Unreal 5 (IoStore): grupy shaderów z bibliotek z kodem SM5 (DXBC);
    // biblioteki z samym DXIL (DirectX 12) pomijamy.
    let io = (kind == "iostore").then(|| super::unreal::IoShaders::open(Path::new(&source)));
    let io_items: Vec<(usize, usize)> = match &io {
        Some(io) => {
            for n in io.notes.iter().take(5) {
                say(format!("warning {n}"));
            }
            (0..io.libs.len())
                .filter(|&l| io.has_sm5(l))
                .flat_map(|l| (0..io.libs[l].1.groups.len()).map(move |g| (l, g)))
                .collect()
        }
        None => Vec::new(),
    };
    if kind == "iostore" && io_items.is_empty() {
        say("fatal kontenery IoStore gry nie mają shaderów DirectX 11 (SM5) - gra ma tylko DirectX 12 albo są szyfrowane".into());
        return 3;
    }
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
    let device = match d3d11::gpu_device() {
        Ok(d) => d,
        Err(e) => {
            say(format!("fatal {e}"));
            return 4;
        }
    };
    say(format!("adapter {}", device.adapter));
    let mut device = Some(device);

    let files = match kind {
        "unity" => unity::candidate_files(Path::new(data_dir)),
        "loose" => super::loose::shader_files(Path::new(data_dir))
            .into_iter()
            .map(|(p, _)| p)
            .collect(),
        _ => Vec::new(),
    };
    let loose = kind == "loose";
    let scan_total = dagor_dump
        .as_ref()
        .map_or(files.len(), |d| d.shader_count())
        .max(unreal_items.len())
        .max(source2_items.len())
        .max(io_items.len());
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
    let zstd_failed = AtomicUsize::new(0);
    // Shadery wierzchołków i ile z nich dostało układ wejścia (AMD, Intel).
    let (mut vertex, mut layouts) = (0usize, 0usize);
    // Diagnostyka: TC_INPUT_LAYOUT=1 buduje układy wejścia także na NVIDII.
    let want_layouts = std::env::var_os("TC_INPUT_LAYOUT").is_some_and(|v| v == "1");

    std::thread::scope(|scope| {
        // Czytniki silników (`readers`): bajty z kontenerami DXBC.
        if super::readers::KINDS.contains(&kind) && !super::readers::is_dx12(kind) {
            let tx = tx.clone();
            let (seen, found, errors, say, source) = (&seen, &found, &errors, &say, &source);
            scope.spawn(move || {
                let result = super::readers::for_each11(
                    kind,
                    Path::new(source),
                    &mut |done, total| say(format!("scan {done} {total}")),
                    &mut |bytes| {
                        unity::carve_dxbc(bytes, |d| {
                            if seen.lock().unwrap().insert(d.digest()) {
                                found.fetch_add(1, Relaxed);
                                let _ = tx.send((d.stage, d.code.to_vec()));
                            }
                        })
                    },
                );
                if let Err(e) = result {
                    errors.lock().unwrap().push(e);
                }
            });
        }
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
        // Unreal IoStore: grupy shaderów (porcje, Oodle), kilka wątków.
        for _ in 0..if io_items.is_empty() { 0 } else { scanners } {
            let tx = tx.clone();
            let (items, io, next_item, items_done, seen, found, errors, say) = (
                &io_items,
                io.as_ref().unwrap(),
                &next_item,
                &items_done,
                &seen,
                &found,
                &errors,
                &say,
            );
            scope.spawn(move || loop {
                let i = next_item.fetch_add(1, Relaxed);
                let Some(&(l, g)) = items.get(i) else { break };
                match io.group(l, g) {
                    Ok(data) => {
                        for (_, code) in io.libs[l].1.split(g, &data) {
                            unity::carve_dxbc(code, |d| {
                                if seen.lock().unwrap().insert(d.digest()) {
                                    found.fetch_add(1, Relaxed);
                                    let _ = tx.send((d.stage, d.code.to_vec()));
                                }
                            });
                        }
                    }
                    Err(e) => {
                        let mut list = errors.lock().unwrap();
                        if list.len() < 10 {
                            list.push(e);
                        }
                    }
                }
                let n = items_done.fetch_add(1, Relaxed) + 1;
                if n % 100 == 0 || n == items.len() {
                    say(format!("scan {n} {}", items.len()));
                }
            });
        }
        // Source 2: pliki .vcs z paczek VPK, kilka wątków.
        let zstd_failed = &zstd_failed;
        for _ in 0..if source2_items.is_empty() { 0 } else { scanners } {
            let tx = tx.clone();
            let (items, vpks, next_item, items_done, seen, found, errors, say) = (
                &source2_items,
                &source2_vpks,
                &next_item,
                &items_done,
                &seen,
                &found,
                &errors,
                &say,
            );
            scope.spawn(move || loop {
                let i = next_item.fetch_add(1, Relaxed);
                let Some(&(v, e)) = items.get(i) else { break };
                let entry = &vpks[v].entries[e];
                match vpks[v].read(entry) {
                    Ok(bytes) => {
                        let failed = super::source2::carve_vcs(&bytes, &mut |d| {
                            if seen.lock().unwrap().insert(d.digest()) {
                                found.fetch_add(1, Relaxed);
                                let _ = tx.send((d.stage, d.code.to_vec()));
                            }
                        });
                        zstd_failed.fetch_add(failed, Relaxed);
                    }
                    Err(err) => {
                        let mut list = errors.lock().unwrap();
                        if list.len() < 10 {
                            list.push(format!("{}: {err}", entry.path));
                        }
                    }
                }
                let n = items_done.fetch_add(1, Relaxed) + 1;
                if n % 100 == 0 || n == items.len() {
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
                let mut send = |d: unity::Dxbc| {
                    if seen.lock().unwrap().insert(d.digest()) {
                        found.fetch_add(1, Relaxed);
                        let _ = tx.send((d.stage, d.code.to_vec()));
                    }
                };
                let result = if loose {
                    super::loose::scan_file(path, &mut send)
                } else {
                    unity::scan_file(path, &mut send).map(|_| ())
                };
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
            let device = match device.take().map_or_else(d3d11::gpu_device, Ok) {
                Ok(d) => d,
                Err(e) => {
                    say(format!("fatal {e}"));
                    return;
                }
            };
            // Shadery partii żyją, aż sterownik skończy je kompilować.
            let mut keep = Vec::with_capacity(BATCH);
            let mut count = 0;
            let mut created = 0;
            for (stage, code) in jobs.by_ref().take(BATCH) {
                count += 1;
                match device.create(stage, &code) {
                    Ok(shader) => {
                        keep.push(shader);
                        created += 1;
                        // NVIDIA kompiluje shader wierzchołków bez układu
                        // wejścia; AMD i Intel - dopiero z nim.
                        if stage == unity::Stage::Vertex
                            && (device.vendor != super::Vendor::Nvidia || want_layouts)
                        {
                            vertex += 1;
                            if let Some(layout) = device.input_layout(&code) {
                                keep.push(layout);
                                layouts += 1;
                            }
                        }
                    }
                    Err(e) => {
                        let mut list = errors.lock().unwrap();
                        if list.len() < 10 {
                            list.push(format!("{stage:?}: {e}"));
                        }
                    }
                }
            }
            d3d11::wait_driver_idle(std::time::Duration::from_secs(600));
            ok += created;
            done += count;
            drop(keep);
            drop(device);
            say(format!("progress {done} {}", found.load(Relaxed)));
        }
    });
    for e in errors.into_inner().unwrap() {
        say(format!("error {e}"));
    }
    if vertex > 0 {
        say(format!(
            "warning układ wejścia z sygnatury / input layout from signature: {layouts} / {vertex}"
        ));
    }
    let zstd_failed = zstd_failed.into_inner();
    if zstd_failed > 0 {
        say(format!(
            "warning ramki zstd nierozpakowane / zstd frames not unpacked: {zstd_failed}"
        ));
    }
    if found.load(Relaxed) == 0 && loose {
        say("warning w plikach gry nie ma jawnych shaderów DXBC / no plain DXBC shaders in the game files".into());
    }
    if found.load(Relaxed) == 0 && super::readers::KINDS.contains(&kind) {
        say("fatal czytnik nie znalazł shaderów DirectX 11 / the reader found no DirectX 11 shaders".into());
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
    let dirs = super::cache_dirs();
    if dirs.is_empty() {
        say("fatal brak karty NVIDIA ani AMD".into());
        return 2;
    }
    for name in opened_cache_files(&dirs, &say) {
        say(format!("file {name}"));
    }
    0
}

/// Pliki w folderach: nazwa -> (rozmiar, czas zapisu).
fn cache_snapshot(
    dirs: &[PathBuf],
) -> std::collections::BTreeMap<String, (u64, Option<std::time::SystemTime>)> {
    dirs.iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .filter_map(|e| {
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            Some((
                e.file_name().to_string_lossy().into_owned(),
                (m.len(), m.modified().ok()),
            ))
        })
        .collect()
}

/// Kod najprostszego shadera obliczeniowego (DXBC, cs_5_0) - na karcie Intel
/// sterownik otwiera pliki pamięci gry dopiero przy pierwszym shaderze.
#[cfg(windows)]
fn tiny_compute_shader() -> Result<Vec<u8>, String> {
    use windows::core::PCSTR;
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    const HLSL: &str = "[numthreads(1, 1, 1)] void main() {}";
    let mut code = None;
    // SAFETY: tekst i nazwy żyją do końca wywołania.
    unsafe {
        D3DCompile(
            HLSL.as_ptr() as *const _,
            HLSL.len(),
            PCSTR(c"tc_probe".as_ptr() as *const u8),
            None,
            None,
            PCSTR(c"main".as_ptr() as *const u8),
            PCSTR(c"cs_5_0".as_ptr() as *const u8),
            0,
            0,
            &mut code,
            None,
        )
        .map_err(|e| format!("D3DCompile: {e}"))?;
        let code = code.ok_or("D3DCompile: brak kodu")?;
        Ok(std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize()).to_vec())
    }
}

/// Pliki pamięci podręcznej, które sterownik otwiera dla nazwy tego procesu:
/// różnica zajętych plików przed i po utworzeniu urządzeń D3D11 i D3D12.
/// Na karcie Intel dodatkowo: jeden malutki shader na każdym urządzeniu i
/// pliki nowe albo zmienione w tym czasie (sterownik Intela zakłada plik gry
/// przy pierwszym shaderze i nie zawsze trzyma go otwartym).
#[cfg(windows)]
fn opened_cache_files(dirs: &[PathBuf], say: &dyn Fn(String)) -> Vec<String> {
    let all = |dirs: &[PathBuf]| -> std::collections::BTreeSet<String> {
        dirs.iter().flat_map(|d| locked_files(d)).collect()
    };
    let intel = super::vendor() == Some(super::Vendor::Intel);
    let before = all(dirs);
    let files_before = cache_snapshot(dirs);
    let dx11 = d3d11::gpu_device();
    let dx12 = d3d::gpu_device();
    if let Err(e) = &dx11 {
        say(format!("warning D3D11: {e}"));
    }
    if let Err(e) = &dx12 {
        say(format!("warning D3D12: {e}"));
    }
    let mut keep = Vec::new();
    if intel {
        match tiny_compute_shader() {
            Ok(cs) => {
                if let Ok(d) = &dx11 {
                    if let Ok(s) = d.create(super::unity::Stage::Compute, &cs) {
                        keep.push(s);
                    }
                }
                if let Ok(d) = &dx12 {
                    let empty = rootsig::serialize(&rootsig::RootSignature::default());
                    if let Ok(rs) = empty.and_then(|b| d.root_signature(&b)) {
                        let _ = d.compute(&rs, &cs);
                    }
                }
            }
            Err(e) => say(format!("warning {e}")),
        }
    }
    // Sterownik otwiera pliki przy tworzeniu urządzenia; chwila zapasu.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let after = all(dirs);
    let mut names: Vec<String> = after.difference(&before).cloned().collect();
    if intel {
        for (name, stat) in cache_snapshot(dirs) {
            if files_before.get(&name) != Some(&stat) && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    drop(keep);
    drop((dx11, dx12));
    names
}

// ------------------ folder shaderów gry (dowiązania) ------------------------
//
// Te trzy procesy działają jako kopia programu poza pakietem Microsoft Store,
// więc piszą do prawdziwego `DXCache` i do folderu danych aplikacji bez
// uprawnień zastrzeżonych pakietu (patrz `store`).

/// Proces dowiązujący pliki pamięci podręcznej gry do jej folderu shaderów:
/// `--shader-cache-link <folder gry>`.
pub const HOST_ARG_LINK: &str = "--shader-cache-link";
/// Proces przywracający pliki z folderu gry: `--shader-cache-restore <folder gry>`.
pub const HOST_ARG_RESTORE: &str = "--shader-cache-restore";
/// Proces usuwający shadery gry:
/// `--shader-cache-delete <folder gry albo -> <exe gry> <klucz hex>...`.
pub const HOST_ARG_DELETE: &str = "--shader-cache-delete";

fn say_line(line: String) {
    let out = std::io::stdout();
    let mut o = out.lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

/// Wypisuje `linked <pliki> <bajty> <nieudane>`.
#[cfg(windows)]
pub fn run_cache_link(args: &[String]) -> i32 {
    let dirs = super::cache_dirs();
    let Some(store) = args.first().filter(|_| !dirs.is_empty()) else {
        say_line("fatal brak folderu pamięci podręcznej albo folderu gry".into());
        return 2;
    };
    let mut keys: Vec<String> = opened_cache_files(&dirs, &say_line)
        .iter()
        .filter_map(|n| cache_key(n))
        .collect();
    keys.sort();
    keys.dedup();
    if keys.is_empty() {
        say_line("fatal sterownik nie otworzył plików pamięci podręcznej gry".into());
        return 2;
    }
    match super::store::link_game(&dirs, Path::new(store), &keys) {
        Ok(l) => {
            say_line(format!("linked {} {} {}", l.files, l.bytes, l.failed));
            0
        }
        Err(e) => {
            say_line(format!("fatal {e}"));
            1
        }
    }
}

/// Wypisuje `restored <przywrócone> <nieudane>`.
pub fn run_cache_restore(args: &[String]) -> i32 {
    let dirs = super::cache_dirs();
    let Some(store) = args.first().filter(|_| !dirs.is_empty()) else {
        say_line("fatal brak folderu pamięci podręcznej albo folderu gry".into());
        return 2;
    };
    match super::store::restore(&dirs, Path::new(store)) {
        Ok((restored, failed)) => {
            say_line(format!("restored {restored} {failed}"));
            0
        }
        Err(e) => {
            say_line(format!("fatal {e}"));
            1
        }
    }
}

/// Wypisuje `cleaned <pliki> <bajty> <zajęte>`.
pub fn run_cache_delete(args: &[String]) -> i32 {
    let dirs = super::cache_dirs();
    let (Some(store), Some(exe)) = (args.first(), args.get(1)) else {
        say_line("fatal brak folderu gry albo nazwy gry".into());
        return 2;
    };
    let keys: Vec<String> = args[2..].to_vec();
    let (mut files, mut bytes, mut locked) = (0, 0, 0);
    for e in dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
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
    clean_d3dscache(exe, &mut files, &mut bytes, &mut locked);
    // Folder gry: druga nazwa tych samych plików - bez niej miejsce na dysku
    // nie zostałoby zwolnione.
    if store != "-" {
        let _ = std::fs::remove_dir_all(store);
    }
    say_line(format!("cleaned {files} {bytes} {locked}"));
    0
}

/// NVIDIA: ile kolejnych numerów pliku traktujemy jako tę samą grę (sterownik
/// zakłada plik o numerze +1, gdy poprzedni jest pełny albo zajęty).
const CACHE_FAMILY: u32 = 8;

fn hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Klucz gry z nazwy pliku pamięci podręcznej sterownika:
/// - NVIDIA (`1c68a91ac748b201.nvph`): `nv:` + ostatnie 8 cyfr nazwy,
/// - AMD DirectX 12 (`207c35a9.dfac411e.71efbc0e.2b1a674a.0.parc`): `amd:` +
///   pierwszy człon - skrót nazwy .exe, wspólny dla wszystkich plików gry,
/// - AMD DirectX 11 (`<48 cyfr>.bin` / `.bin2`): `amd:` + pierwsze 16 cyfr,
/// - Intel (`<64 cyfry>` bez rozszerzenia): `intel:` + cała nazwa.
pub fn cache_key(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let mut parts = lower.split('.');
    let stem = parts.next()?;
    if lower.ends_with(".parc") && stem.len() == 8 && hex(stem) {
        return Some(format!("amd:{stem}."));
    }
    if (lower.ends_with(".bin") || lower.ends_with(".bin2")) && stem.len() >= 32 && hex(stem) {
        return Some(format!("amd:{}", &stem[..16]));
    }
    // Intel: 64 cyfry bez rozszerzenia - każdy plik osobno.
    if stem.len() == 64 && hex(stem) && !lower.contains('.') {
        return Some(format!("intel:{stem}"));
    }
    if stem.len() >= 16 && hex(stem) {
        return Some(format!("nv:{}", &stem[stem.len() - 8..]));
    }
    None
}

/// Czy plik należy do gry o kluczach `keys` (NVIDIA: ten sam skrót lub
/// kolejne numery; AMD: ten sam skrót nazwy .exe).
pub fn in_family(name: &str, keys: &[String]) -> bool {
    let Some(key) = cache_key(name) else {
        return false;
    };
    keys.iter().any(|g| match (key.strip_prefix("nv:"), g.strip_prefix("nv:")) {
        (Some(a), Some(b)) => match (u32::from_str_radix(a, 16), u32::from_str_radix(b, 16)) {
            (Ok(a), Ok(b)) => a.abs_diff(b) <= CACHE_FAMILY,
            _ => false,
        },
        _ => key == *g,
    })
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
    let keys = game_cache_keys(exe)?;
    // Usuwa proces poza pakietem (`run_cache_delete`): pliki w DXCache,
    // foldery D3DSCache i folder shaderów gry.
    let mut args = vec![
        HOST_ARG_DELETE.to_owned(),
        super::store::game_dir(exe).map_or("-".into(), |d| d.display().to_string()),
        exe.to_owned(),
    ];
    args.extend(keys.iter().cloned());
    let nums = staged_result(exe, &args, "cleaned")?;
    match nums.as_slice() {
        [files, bytes, locked] => Ok((*files as usize, *bytes, *locked as usize)),
        _ => Err(unexpected_output()),
    }
}

fn unexpected_output() -> String {
    crate::i18n::tr(
        "nieoczekiwana odpowiedź procesu pomocniczego",
        "unexpected reply from the helper process",
    )
    .to_string()
}

/// Uruchamia kopię programu pod nazwą `exe` z argumentami `args` i zwraca jej
/// standardowe wyjście. Kopia jest potem usuwana.
fn run_staged(exe: &str, args: &[String]) -> Result<String, String> {
    let host = staged_host(exe)?;
    let mut cmd = std::process::Command::new(&host);
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&host);
    Ok(String::from_utf8_lossy(&output?.stdout).into_owned())
}

/// Liczby z wiersza `<słowo> <liczba>...` na wyjściu procesu pomocniczego;
/// wiersz `fatal ...` jest błędem.
fn staged_result(exe: &str, args: &[String], word: &str) -> Result<Vec<u64>, String> {
    let text = run_staged(exe, args)?;
    if let Some(nums) = text
        .lines()
        .find_map(|l| l.strip_prefix(word)?.strip_prefix(' '))
    {
        return Ok(nums.split(' ').filter_map(|w| w.parse().ok()).collect());
    }
    Err(text
        .lines()
        .find_map(|l| l.strip_prefix("fatal "))
        .map_or_else(unexpected_output, str::to_owned))
}

/// Dowiązuje pliki pamięci podręcznej gry do jej folderu shaderów (patrz
/// `store`). Gra musi być zamknięta.
pub fn link_game(exe: &str) -> Result<super::store::Linked, String> {
    #[cfg(windows)]
    if process_running(exe) {
        return Err(crate::i18n::tr("gra jest uruchomiona", "the game is running").into());
    }
    let store = super::store::game_dir(exe).ok_or("brak folderu danych aplikacji")?;
    let args = [HOST_ARG_LINK.to_owned(), store.display().to_string()];
    match staged_result(exe, &args, "linked")?.as_slice() {
        [files, bytes, failed] => Ok(super::store::Linked {
            files: *files as usize,
            bytes: *bytes,
            failed: *failed as usize,
        }),
        _ => Err(unexpected_output()),
    }
}

/// Przywraca w pamięci podręcznej sterownika pliki z folderu shaderów gry.
/// Zwraca (przywrócone, nieudane).
pub fn restore_game(exe: &str) -> Result<(usize, usize), String> {
    let store = super::store::game_dir(exe).ok_or("brak folderu danych aplikacji")?;
    let args = [HOST_ARG_RESTORE.to_owned(), store.display().to_string()];
    match staged_result(exe, &args, "restored")?.as_slice() {
        [restored, failed] => Ok((*restored as usize, *failed as usize)),
        _ => Err(unexpected_output()),
    }
}

/// Po kompilacji: dowiązuje pliki gry do jej folderu shaderów i zwraca
/// (rozmiar pamięci podręcznej gry razem z D3DSCache, liczba dowiązanych
/// plików). Gdy dowiązać się nie da, zostaje sam pomiar rozmiaru.
pub fn finish_game(exe: &str) -> Result<(u64, Option<usize>), String> {
    match link_game(exe) {
        Ok(linked) if linked.files > 0 => {
            let d3ds: u64 = d3dscache_folders(exe).iter().map(|d| dir_size(d)).sum();
            Ok((linked.bytes + d3ds, Some(linked.files)))
        }
        _ => game_cache_bytes(exe).map(|b| (b, None)),
    }
}

/// Klucze plików pamięci podręcznej sterownika (NVIDIA albo AMD) dla gry:
/// proces pomiaru uruchomiony pod nazwą jej .exe (patrz `run_cache_probe`).
fn game_cache_keys(exe: &str) -> Result<Vec<String>, String> {
    let text = run_staged(exe, &[HOST_ARG_PROBE.to_owned()])?;
    let mut keys: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("file "))
        .filter_map(cache_key)
        .collect();
    keys.sort();
    keys.dedup();
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
    Ok(keys)
}

/// Rozmiar pamięci podręcznej gry: pliki sterownika karty (NVIDIA DXCache,
/// AMD DxcCache i DxCache) i foldery D3DSCache tej gry. Nic nie usuwa. Gra
/// musi być zamknięta.
pub fn game_cache_bytes(exe: &str) -> Result<u64, String> {
    #[cfg(windows)]
    if process_running(exe) {
        return Err(crate::i18n::tr("gra jest uruchomiona", "the game is running").into());
    }
    let keys = game_cache_keys(exe)?;
    let mut bytes: u64 = super::cache_dirs()
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .filter(|e| in_family(&e.file_name().to_string_lossy(), &keys))
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum();
    bytes += d3dscache_folders(exe)
        .iter()
        .map(|d| dir_size(d))
        .sum::<u64>();
    Ok(bytes)
}

/// Pamięć podręczna shaderów Windows (D3DSCache): DirectX 12 gry, kopie
/// robocze SCSKiller i nasze kompilacje - folder na każdą ścieżkę .exe.
fn clean_d3dscache(exe: &str, files: &mut usize, bytes: &mut u64, locked: &mut usize) {
    for dir in d3dscache_folders(exe) {
        let size = dir_size(&dir);
        let count = std::fs::read_dir(&dir).map(|r| r.count()).unwrap_or(0);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                *files += count;
                *bytes += size;
            }
            Err(_) => *locked += count.max(1),
        }
    }
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
    Dagor {
        dump: PathBuf,
        cbv_ranges: bool,
        /// `cache\dx12.cache` gry - potoki graficzne do odtworzenia.
        pipelines: Option<PathBuf>,
    },
    /// Folder `*_Data` gry Unity (DirectX 11).
    Unity { data_dir: PathBuf },
    /// Zrzut shaderów Dagor DirectX 11 (kontenery DXBC).
    DagorDx11 { dump: PathBuf },
    /// Paczka `.pak` gry Unreal (archiwa shaderów SM5, DirectX 11).
    Unreal { pak: PathBuf },
    /// Folder gry Unreal 5 - potoki DirectX 12 z pamięci potoków gry.
    UnrealDx12 { game: PathBuf },
    /// Folder gry Unreal 5 z kontenerami IoStore (shadery SM5).
    UnrealIo { game: PathBuf },
    /// Folder gry Source 2 (pliki .vcs DirectX z paczek VPK).
    Source2 { game: PathBuf },
    /// Folder gry bez rozpoznanego silnika (jawne kontenery DXBC).
    Loose { game: PathBuf },
    /// Folder gry z kontenerami DirectX 12 z wbudowaną root signature.
    Carved { game: PathBuf },
    /// Folder gry REDengine 3 (pamięć shaderów DirectX 12).
    Red3 { game: PathBuf },
    /// Folder gry odczytywanej przez czytnik silnika (`readers`, rodzaj `kind`).
    Reader { kind: String, game: PathBuf },
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
                Job::Dagor { dump, cbv_ranges, pipelines } => {
                    cmd.arg(HOST_ARG)
                        .arg(dump)
                        .arg(if *cbv_ranges { "1" } else { "0" });
                    if let Some(p) = pipelines {
                        cmd.arg(p);
                    }
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
                Job::UnrealDx12 { game } => {
                    cmd.arg(HOST_ARG_UE12).arg(game);
                }
                Job::UnrealIo { game } => {
                    cmd.arg(HOST_ARG_D3D11).arg("iostore").arg(game);
                }
                Job::Source2 { game } => {
                    cmd.arg(HOST_ARG_D3D11).arg("source2").arg(game);
                }
                Job::Loose { game } => {
                    cmd.arg(HOST_ARG_D3D11).arg("loose").arg(game);
                }
                Job::Carved { game } => {
                    cmd.arg(super::warm12::HOST_ARG).arg("carved").arg(game);
                }
                Job::Red3 { game } => {
                    cmd.arg(super::warm12::HOST_ARG).arg("red3").arg(game);
                }
                Job::Reader { kind, game } if super::readers::is_dx12(kind) => {
                    cmd.arg(super::warm12::HOST_ARG).arg(format!("reader:{kind}")).arg(game);
                }
                Job::Reader { kind, game } => {
                    cmd.arg(HOST_ARG_D3D11).arg(kind).arg(game);
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
        assert_eq!(cache_key("1c68a91ac748b201.nvph").as_deref(), Some("nv:c748b201"));
        assert_eq!(cache_key("0002a91a62c6cdf9.nvph").as_deref(), Some("nv:62c6cdf9"));
        assert_eq!(cache_key("index.toc"), None);
        let keys = vec!["nv:62c6cdfa".to_string()];
        assert!(in_family("1c68a91a62c6cdf9.nvph", &keys));
        assert!(in_family("0002a91a62c6cdfa.nvph", &keys));
        assert!(!in_family("1c68a91a62c6ce10.nvph", &keys));
        assert!(!in_family("1c68a91ac748b201.nvph", &keys));
    }

    #[test]
    fn amd_cache_file_family() {
        // DirectX 12: pierwszy człon nazwy to skrót nazwy .exe.
        let parc = "207c35a9.dfac411e.71efbc0e.2b1a674a.0.parc";
        assert_eq!(cache_key(parc).as_deref(), Some("amd:207c35a9."));
        let keys = vec![cache_key(parc).unwrap()];
        assert!(in_family("207c35a9.11111111.22222222.33333333.1.parc", &keys));
        assert!(!in_family("207c35aa.dfac411e.71efbc0e.2b1a674a.0.parc", &keys));
        // Klucz AMD nie łapie plików NVIDIA i odwrotnie.
        assert!(!in_family("1c68a91a207c35a9.nvph", &keys));
        // DirectX 11: 48 cyfr, grupą jest pierwsze 16.
        let bin = "0123456789abcdef0011223344556677889900aabbccddee.bin";
        assert_eq!(cache_key(bin).as_deref(), Some("amd:0123456789abcdef"));
        assert!(in_family(
            "0123456789ABCDEFffffffffffffffffffffffffffffffff.bin2",
            &[cache_key(bin).unwrap()]
        ));
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

#[cfg(test)]
mod cache_size {
    /// Rozmiar pamięci podręcznej gry bez usuwania:
    /// `TC_CLEAN_EXE=<exe> cargo test game_cache_size --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn game_cache_size() {
        let exe = std::env::var("TC_CLEAN_EXE").expect("TC_CLEAN_EXE");
        println!(
            "{exe}: {:?}",
            super::game_cache_bytes(&exe).map(|b| b >> 20)
        );
    }
}
