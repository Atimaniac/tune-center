//! Podzakładka NVIDIA / RADEON > Shader compilation: wyszukiwanie zainstalowanych gier,
//! rozpoznanie silnika i plików shaderów oraz (w kolejnych etapach)
//! kompilacja shaderów do pamięci podręcznej sterownika z góry, zanim gra
//! musi to zrobić w trakcie rozgrywki.
//!
//! Etap 1 (ten plik): wyszukiwarka, rozpoznanie, stan pamięci podręcznej.
//! Gry są tylko czytane - Tune Center niczego w nich nie zmienia.

pub mod dagor;
pub mod engine;
pub mod games;
#[cfg(windows)]
mod registry;
pub mod rootsig;
pub mod state;
pub mod store;
pub mod unity;
pub mod unreal;
pub mod unreal_pak;
pub mod warm;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::theme::{self, accent, muted, warn};
use engine::{Engine, Info, ShaderKind};
use games::Game;

pub struct Entry {
    pub game: Game,
    pub info: Info,
}

#[derive(Default)]
struct Scan {
    running: bool,
    entries: Option<Vec<Entry>>,
    cache_bytes: Option<u64>,
}

pub struct ShaderTab {
    scan: Arc<Mutex<Scan>>,
    filter: String,
    selected: Option<String>,
    /// Kompilacje w toku albo zakończone, po identyfikatorze gry.
    warms: std::collections::HashMap<String, warm::Warm>,
    /// Wybrane API kompilacji, po identyfikatorze gry.
    apis: std::collections::HashMap<String, Api>,
    /// Czyszczenie shaderów gier i gra czekająca na potwierdzenie.
    cleans: std::collections::HashMap<String, warm::Clean>,
    confirm_clean: Option<String>,
    /// Zapisane wyniki kompilacji (`shader-state.json`).
    saved: state::Saved,
    /// Kompilacje w toku: gra → dane do zapisania po zakończeniu.
    tracking: std::collections::HashMap<String, Track>,
    /// Kolejka gier do kompilacji (identyfikatory, po kolei).
    queue: std::collections::VecDeque<String>,
    /// Ostatni komunikat kolejki (np. gra pominięta, bo była uruchomiona).
    queue_note: Option<String>,
    /// Rozmiary pamięci podręcznej gier liczone w tle po kompilacji.
    sizes: Arc<Mutex<Vec<(String, Option<(u64, Option<usize>)>)>>>,
    /// Bieżąca wersja sterownika karty (NVIDIA albo AMD Software, z `tick`).
    driver: Option<String>,
    /// Stan folderów shaderów gier (odświeżany co kilka sekund), po grze.
    stores: std::collections::HashMap<String, (std::time::Instant, store::Info)>,
    /// Przywracanie shaderów z folderu gry, po grze.
    restores: Restores,
}

/// Przywracanie plików z folderu shaderów gry do pamięci podręcznej.
#[derive(Clone, Debug, PartialEq)]
enum Restore {
    Running,
    Done { restored: usize, failed: usize },
    Failed(String),
}

type Restores = Arc<Mutex<std::collections::HashMap<String, Restore>>>;

/// Kompilacja w toku - dane do zapisu wyniku.
struct Track {
    api: Api,
    exe: String,
    started: std::time::Instant,
    stamp: String,
}

/// Producent karty, dla której kompilujemy shadery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
}

pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_AMD: u32 = 0x1002;
pub const VENDOR_INTEL: u32 = 0x8086;

impl Vendor {
    pub fn from_id(id: u32) -> Option<Vendor> {
        match id {
            VENDOR_NVIDIA => Some(Vendor::Nvidia),
            VENDOR_AMD => Some(Vendor::Amd),
            VENDOR_INTEL => Some(Vendor::Intel),
            _ => None,
        }
    }

}

/// Karta do kompilacji: spośród kart NVIDIA, AMD i Intel ta z największą
/// pamięcią własną (karta osobna przed układem zintegrowanym w procesorze).
#[cfg(windows)]
pub fn pick_adapter() -> Result<(windows::Win32::Graphics::Dxgi::IDXGIAdapter1, Vendor, String), String> {
    use windows::Win32::Graphics::Dxgi::*;
    // SAFETY: wywołania DXGI z poprawnymi argumentami.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
        let mut best: Option<(usize, IDXGIAdapter1, Vendor, String)> = None;
        let mut index = 0;
        while let Ok(adapter) = factory.EnumAdapters1(index) {
            index += 1;
            let Ok(desc) = adapter.GetDesc1() else { continue };
            let Some(vendor) = Vendor::from_id(desc.VendorId) else {
                continue;
            };
            let memory = desc.DedicatedVideoMemory;
            if best.as_ref().is_some_and(|(m, ..)| *m >= memory) {
                continue;
            }
            let name = String::from_utf16_lossy(&desc.Description)
                .trim_end_matches('\0')
                .to_owned();
            best = Some((memory, adapter, vendor, name));
        }
        best.map(|(_, a, v, n)| (a, v, n)).ok_or_else(|| {
            tr(
                "nie znaleziono karty NVIDIA, AMD ani Intel",
                "no NVIDIA, AMD or Intel graphics card found",
            )
            .to_string()
        })
    }
}

/// Producent karty do kompilacji (ustalany raz).
pub fn vendor() -> Option<Vendor> {
    static VENDOR: std::sync::OnceLock<Option<Vendor>> = std::sync::OnceLock::new();
    #[cfg(windows)]
    {
        *VENDOR.get_or_init(|| pick_adapter().ok().map(|(_, v, _)| v))
    }
    #[cfg(not(windows))]
    {
        *VENDOR.get_or_init(|| None)
    }
}

/// Foldery pamięci podręcznej shaderów DirectX sterownika karty:
/// - NVIDIA: `%LOCALAPPDATA%\NVIDIA\DXCache` (DirectX 11 i 12),
/// - AMD: `%LOCALAPPDATA%\AMD\DxcCache` (DirectX 12) i `AMD\DxCache` (DirectX 11),
/// - Intel: `%USERPROFILE%\AppData\LocalLow\Intel\ShaderCache` (DirectX 11 i 12).
///
/// Pierwszy folder jest główny (jego pliki leżą wprost w folderze shaderów
/// gry, patrz `store`).
pub fn cache_dirs() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    match vendor() {
        Some(Vendor::Nvidia) => vec![local.join("NVIDIA").join("DXCache")],
        Some(Vendor::Amd) => vec![
            local.join("AMD").join("DxcCache"),
            local.join("AMD").join("DxCache"),
        ],
        // LocalLow leży obok Local.
        Some(Vendor::Intel) => local
            .parent()
            .map(|appdata| vec![appdata.join("LocalLow").join("Intel").join("ShaderCache")])
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Główny folder pamięci podręcznej (pierwszy z `cache_dirs`).
pub fn main_cache_dir() -> Option<PathBuf> {
    cache_dirs().into_iter().next()
}

fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match entry.metadata() {
                Ok(m) if m.is_dir() => stack.push(entry.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// Co Tune Center może zrobić z shaderami tej gry (stan etapów).
/// Unreal z klasyczną paczką `.pak` (IoStore jeszcze nieobsługiwany).
fn has_pak(info: &Info) -> bool {
    info.shaders.iter().any(|s| {
        s.path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("pak"))
    }) && !info.shaders.iter().any(|s| {
        s.path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("utoc"))
    })
}

pub fn plan(info: &Info) -> (&'static str, bool) {
    let has = |k: ShaderKind| info.shaders.iter().any(|s| s.kind == k);
    match info.engine {
        Engine::Dagor if has(ShaderKind::DagorDx11) && has(ShaderKind::DagorDx12) => (
            tr(
                "DX11: wszystkie shadery; DX12: compute (graficzne w przygotowaniu)",
                "DX11: every shader; DX12: compute (graphics in progress)",
            ),
            true,
        ),
        Engine::Dagor if has(ShaderKind::DagorDx12) => (
            tr(
                "DirectX 12: compute gotowe, graficzne w przygotowaniu",
                "DirectX 12: compute ready, graphics in progress",
            ),
            true,
        ),
        Engine::Dagor if has(ShaderKind::DagorDx11) => (
            tr(
                "DirectX 11: wszystkie shadery ze zrzutu gry",
                "DirectX 11: every shader from the game dump",
            ),
            true,
        ),
        Engine::Unity if info.api.as_deref() == Some("DirectX 12") => (
            tr(
                "DirectX 12: Unity buduje potoki w trakcie gry - z samych plików nie da się",
                "DirectX 12: Unity builds pipelines at run time - not possible from files alone",
            ),
            false,
        ),
        Engine::Unity => (
            tr(
                "DirectX 11: wszystkie shadery z plików gry",
                "DirectX 11: every shader from the game files",
            ),
            true,
        ),
        Engine::Unreal if info.encrypted => (
            tr(
                "Unreal - paczki gry są szyfrowane, shaderów nie da się odczytać",
                "Unreal - the game's packages are encrypted, shaders cannot be read",
            ),
            false,
        ),
        Engine::Unreal if has_pak(info) => (
            tr(
                "DirectX 11: shadery SM5 z paczki gry (DX12 w przygotowaniu)",
                "DirectX 11: SM5 shaders from the game package (DX12 in progress)",
            ),
            true,
        ),
        Engine::Unreal => (
            tr(
                "Unreal - jeszcze nieobsługiwany",
                "Unreal - not supported yet",
            ),
            false,
        ),
        _ => (
            tr(
                "nie rozpoznano plików shaderów",
                "no shader files recognized",
            ),
            false,
        ),
    }
}

impl ShaderTab {
    pub fn new() -> Self {
        Self {
            scan: Arc::default(),
            filter: String::new(),
            selected: None,
            warms: Default::default(),
            apis: Default::default(),
            cleans: Default::default(),
            confirm_clean: None,
            saved: state::load(),
            tracking: Default::default(),
            queue: Default::default(),
            queue_note: None,
            sizes: Arc::default(),
            driver: None,
            stores: Default::default(),
            restores: Arc::default(),
        }
    }

    /// Uruchamia kompilację gry i zaczyna ją śledzić (wynik trafia do stanu).
    fn start_compile(
        &mut self,
        e: &Entry,
        api: Api,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(), String> {
        let (exe, job, _) = job_for(e, api)?.ok_or_else(|| {
            tr(
                "Nie ma czego kompilować dla tego API.",
                "Nothing to compile for this API.",
            )
            .to_owned()
        })?;
        #[cfg(windows)]
        if warm::process_running(&exe) {
            return Err(crate::trf!(
                "{} jest uruchomiona - kompilacja po zamknięciu gry.",
                "{} is running - compiling after the game closes.",
                e.game.name
            ));
        }
        self.cleans
            .entry(e.game.id.clone())
            .or_default()
            .state
            .lock()
            .unwrap()
            .clone_from(&warm::CleanState::Idle);
        self.warms
            .entry(e.game.id.clone())
            .or_default()
            .start(&exe, job, wake);
        self.tracking.insert(
            e.game.id.clone(),
            Track {
                api,
                exe,
                started: std::time::Instant::now(),
                stamp: state::stamp(&e.info),
            },
        );
        Ok(())
    }

    /// Stan gry na liście (z zapisanego wyniku, sterownika i plików gry).
    fn status(&self, e: &Entry) -> state::Status {
        state::status(
            self.saved.games.get(&e.game.id),
            &e.info,
            self.driver.as_deref(),
        )
    }

    /// Gry, które da się skompilować i które tego wymagają.
    fn needing(&self) -> Vec<String> {
        let scan = self.scan.lock().unwrap();
        scan.entries
            .iter()
            .flatten()
            .filter(|e| {
                plan(&e.info).1
                    && self.status(e).needs_compile()
                    && !self.tracking.contains_key(&e.game.id)
                    && !self.queue.contains(&e.game.id)
            })
            .map(|e| e.game.id.clone())
            .collect()
    }

    /// Dopisuje gry do kolejki (bez powtórzeń i bez gry właśnie kompilowanej).
    fn enqueue(&mut self, ids: Vec<String>) {
        for id in ids {
            if !self.queue.contains(&id) && !self.tracking.contains_key(&id) {
                self.queue.push_back(id);
            }
        }
    }

    /// Wołane regularnie z `App::logic` (także przy ukrytym oknie): zapis
    /// wyników zakończonych kompilacji, następna gra z kolejki, automatyczna
    /// kompilacja po nowym sterowniku.
    pub fn tick(&mut self, driver: Option<&str>, ctx: &egui::Context) {
        self.driver = driver.map(state::short_driver);

        // Zakończone kompilacje → zapis wyniku i pomiar pamięci podręcznej gry.
        let finished: Vec<(String, warm::Progress)> = self
            .tracking
            .keys()
            .filter_map(|id| {
                let p = self.warms.get(id)?.progress.lock().unwrap().clone();
                matches!(p.phase, warm::Phase::Done | warm::Phase::Failed(_))
                    .then(|| (id.clone(), p))
            })
            .collect();
        for (id, p) in finished {
            let Some(t) = self.tracking.remove(&id) else {
                continue;
            };
            if p.phase != warm::Phase::Done {
                continue;
            }
            self.saved.games.insert(
                id.clone(),
                state::Record {
                    when: crate::bench::now_text(),
                    driver: self.driver.clone().unwrap_or_default(),
                    api: if t.api == Api::Dx12 { "DX12" } else { "DX11" }.into(),
                    ok: p.ok,
                    failed: p.failed,
                    secs: t.started.elapsed().as_secs(),
                    cache_bytes: None,
                    stamp: t.stamp,
                    exe: t.exe.clone(),
                    linked: None,
                },
            );
            state::save(&self.saved);
            // Dowiązanie plików gry do jej folderu shaderów i pomiar rozmiaru.
            let (sizes, exe, ctx2) = (self.sizes.clone(), t.exe, ctx.clone());
            std::thread::spawn(move || {
                let done = warm::finish_game(&exe).ok();
                sizes.lock().unwrap().push((id, done));
                ctx2.request_repaint();
            });
        }
        let measured: Vec<_> = std::mem::take(&mut *self.sizes.lock().unwrap());
        if !measured.is_empty() {
            for (id, done) in measured {
                if let Some(r) = self.saved.games.get_mut(&id) {
                    r.cache_bytes = done.map(|(bytes, _)| bytes);
                    r.linked = done.and_then(|(_, linked)| linked);
                }
                self.stores.remove(&id);
            }
            state::save(&self.saved);
        }

        // Automatyczna kompilacja po instalacji nowego sterownika.
        if self.saved.auto_after_driver {
            if let Some(d) = self.driver.clone() {
                let scanned = self.scan.lock().unwrap().entries.is_some();
                if !scanned {
                    let ctx2 = ctx.clone();
                    self.rescan(move || ctx2.request_repaint());
                } else if self.saved.auto_driver != d {
                    let ids: Vec<String> = {
                        let scan = self.scan.lock().unwrap();
                        scan.entries
                            .iter()
                            .flatten()
                            .filter(|e| {
                                plan(&e.info).1
                                    && matches!(self.status(e), state::Status::NewDriver { .. })
                            })
                            .map(|e| e.game.id.clone())
                            .collect()
                    };
                    self.enqueue(ids);
                    self.saved.auto_driver = d;
                    state::save(&self.saved);
                }
            }
        }

        // Następna gra z kolejki, gdy nic się nie kompiluje.
        if self.tracking.is_empty() {
            while let Some(id) = self.queue.pop_front() {
                let entry = {
                    let scan = self.scan.lock().unwrap();
                    scan.entries
                        .iter()
                        .flatten()
                        .find(|e| e.game.id == id)
                        .map(|e| Entry {
                            game: e.game.clone(),
                            info: e.info.clone(),
                        })
                };
                let Some(e) = entry else { continue };
                let choice = self.apis.get(&id).copied().unwrap_or_default();
                let api = match resolve_api(choice, &api_options(&e.info)) {
                    Ok(a) => a,
                    Err(why) => {
                        self.queue_note = Some(format!("{}: {why}", e.game.name));
                        continue;
                    }
                };
                let ctx2 = ctx.clone();
                match self.start_compile(&e, api, move || ctx2.request_repaint()) {
                    Ok(()) => {
                        self.queue_note = None;
                        break;
                    }
                    Err(why) => self.queue_note = Some(why),
                }
            }
        }
        if !self.tracking.is_empty() || !self.queue.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    /// Skanuje gry w tle; `wake` odświeża okno po zakończeniu.
    pub fn rescan(&self, wake: impl FnOnce() + Send + 'static) {
        {
            let mut s = self.scan.lock().unwrap();
            if s.running {
                return;
            }
            s.running = true;
        }
        let shared = self.scan.clone();
        std::thread::spawn(move || {
            let entries: Vec<Entry> = games::find_all()
                .into_iter()
                .map(|game| {
                    let info = engine::inspect(&game.dir);
                    Entry { game, info }
                })
                .collect();
            let dirs: Vec<PathBuf> = cache_dirs().into_iter().filter(|d| d.is_dir()).collect();
            let cache = (!dirs.is_empty()).then(|| dirs.iter().map(|d| dir_size(d)).sum());
            let mut s = shared.lock().unwrap();
            s.entries = Some(entries);
            s.cache_bytes = cache;
            s.running = false;
            drop(s);
            wake();
        });
    }

    /// Rozmiar folderu pamięci podręcznej sterownika (po skanowaniu).
    pub fn cache_bytes(&self) -> Option<u64> {
        self.scan.lock().unwrap().cache_bytes
    }

    pub fn games_ui(&mut self, ui: &mut egui::Ui) {
        let started = {
            let s = self.scan.lock().unwrap();
            s.entries.is_some() || s.running
        };
        if !started {
            let ctx = ui.ctx().clone();
            self.rescan(move || ctx.request_repaint());
        }

        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(tr("Zainstalowane gry", "Installed games"))
                    .strong()
                    .color(accent()),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(tr("Szukaj...", "Search..."))
                    .desired_width(200.0),
            );
            let running = self.scan.lock().unwrap().running;
            if ui
                .add_enabled(
                    !running,
                    egui::Button::new(tr("Szukaj gier ponownie", "Scan again")),
                )
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.rescan(move || ctx.request_repaint());
            }
            if running {
                ui.spinner();
                ui.label(RichText::new(tr("Szukanie gier...", "Scanning games...")).color(muted()));
            }
        });
        ui.label(
            RichText::new(tr(
                "Steam, Epic Games, GOG, Ubisoft Connect, EA app i Xbox. Gry są tylko czytane - nic w nich nie jest zmieniane.",
                "Steam, Epic Games, GOG, Ubisoft Connect, EA app and Xbox. Games are only read - nothing in them is changed.",
            ))
            .small()
            .color(muted()),
        );
        ui.add_space(6.0);
        self.queue_ui(ui);
        ui.add_space(6.0);

        // Stan gier liczony przed zablokowaniem listy (status() czyta zapis).
        let statuses: std::collections::HashMap<String, state::Status> = {
            let scan = self.scan.lock().unwrap();
            scan.entries
                .iter()
                .flatten()
                .map(|e| (e.game.id.clone(), self.status(e)))
                .collect()
        };
        let mut request: Option<Api> = None;
        let scan = self.scan.lock().unwrap();
        let Some(entries) = &scan.entries else {
            return;
        };
        if entries.is_empty() {
            ui.label(tr("Nie znaleziono gier.", "No games found."));
            return;
        }
        let filter = self.filter.to_lowercase();
        let mut clicked: Option<String> = None;
        egui::Grid::new("shader_games")
            .num_columns(7)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in [
                    tr("Gra", "Game"),
                    tr("Sklep", "Store"),
                    tr("Silnik", "Engine"),
                    "API",
                    tr("Shadery", "Shaders"),
                    tr("Kompilacja z góry", "Precompile"),
                    tr("Stan", "Status"),
                ] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for e in entries
                    .iter()
                    .filter(|e| filter.is_empty() || e.game.name.to_lowercase().contains(&filter))
                {
                    let selected = self.selected.as_deref() == Some(e.game.id.as_str());
                    if ui.selectable_label(selected, &e.game.name).clicked() {
                        clicked = Some(e.game.id.clone());
                    }
                    ui.label(e.game.store.name());
                    let engine = match &e.info.version {
                        Some(v) => format!("{} {v}", e.info.engine.name()),
                        None => e.info.engine.name().to_owned(),
                    };
                    ui.label(engine);
                    ui.label(e.info.api.clone().unwrap_or_else(|| "?".into()));
                    ui.label(if e.info.shaders.is_empty() {
                        "-".to_owned()
                    } else if e.info.engine == Engine::Unity {
                        // Shadery Unity są w środku paczek z resztą danych gry -
                        // ich rozmiar będzie znany dopiero po przeczytaniu paczek.
                        crate::trf!(
                            "{} plików do przeszukania",
                            "{} files to search",
                            e.info.shaders.len()
                        )
                    } else {
                        engine::human(e.info.shader_bytes())
                    });
                    let (text, possible) = plan(&e.info);
                    ui.label(RichText::new(text).color(if possible { accent() } else { muted() }));
                    // Stan: kompilacja w toku, kolejka albo wynik ostatniej kompilacji.
                    let id = &e.game.id;
                    let (text, color) = if self.tracking.contains_key(id) {
                        let p = self
                            .warms
                            .get(id)
                            .map(|w| w.progress.lock().unwrap().clone())
                            .unwrap_or_default();
                        let pct = if p.total > 0 {
                            p.done * 100 / p.total
                        } else {
                            0
                        };
                        (
                            crate::trf!("Kompilacja... {pct}%", "Compiling... {pct}%"),
                            accent(),
                        )
                    } else if self.queue.contains(id) {
                        (tr("W kolejce", "Queued").to_owned(), accent())
                    } else if !possible {
                        (String::new(), muted())
                    } else {
                        match statuses.get(id) {
                            Some(state::Status::Fresh) => (
                                crate::trf!(
                                    "Skompilowano {}",
                                    "Compiled {}",
                                    self.saved
                                        .games
                                        .get(id)
                                        .map(|r| state::short_date(&r.when))
                                        .unwrap_or_default()
                                ),
                                theme::ok(),
                            ),
                            Some(state::Status::NewDriver { .. }) => (
                                tr(
                                    "Nowy sterownik - skompiluj ponownie",
                                    "New driver - compile again",
                                )
                                .to_owned(),
                                warn(),
                            ),
                            Some(state::Status::GameUpdated) => (
                                tr(
                                    "Gra zaktualizowana - skompiluj ponownie",
                                    "Game updated - compile again",
                                )
                                .to_owned(),
                                warn(),
                            ),
                            _ => (tr("Nie kompilowano", "Not compiled").to_owned(), muted()),
                        }
                    };
                    ui.label(RichText::new(text).color(color));
                    ui.end_row();
                }
            });
        if let Some(id) = clicked {
            self.selected = if self.selected.as_deref() == Some(id.as_str()) {
                None
            } else {
                Some(id)
            };
        }

        // Szczegóły wybranej gry.
        if let Some(e) = self
            .selected
            .as_deref()
            .and_then(|id| entries.iter().find(|e| e.game.id == id))
        {
            ui.add_space(10.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(RichText::new(&e.game.name).strong());
                ui.label(RichText::new(e.game.dir.display().to_string()).small().color(muted()));
                ui.label(RichText::new(&e.game.id).small().color(muted()));
                if !e.info.anti_cheat.is_empty() {
                    ui.label(
                        RichText::new(crate::trf!(
                            "Anty-cheat: {}. Tune Center tylko czyta pliki tej gry i kompiluje w swoim procesie - nie uruchamia jej ani nic do niej nie dodaje.",
                            "Anti-cheat: {}. Tune Center only reads this game's files and compiles in its own process - it never starts the game or adds anything to it.",
                            e.info.anti_cheat.join(", ")
                        ))
                        .color(warn()),
                    );
                }
                if e.info.scskiller_recorder {
                    ui.label(
                        RichText::new(tr(
                            "W folderze gry jest recorder SCSKiller (scskiller.cfg).",
                            "The SCSKiller recorder is in the game folder (scskiller.cfg).",
                        ))
                        .color(muted()),
                    );
                }
                let shown: Vec<_> = e
                    .info
                    .shaders
                    .iter()
                    .filter(|s| s.kind != ShaderKind::UnityAssets)
                    .collect();
                for s in &shown {
                    let kind = match s.kind {
                        ShaderKind::DagorDx11 => "DirectX 11",
                        ShaderKind::DagorDx12 => "DirectX 12",
                        ShaderKind::DagorVulkan => "Vulkan",
                        ShaderKind::UnrealArchive => "Unreal",
                        ShaderKind::UnityAssets => "Unity",
                    };
                    let file = s.path.strip_prefix(&e.game.dir).unwrap_or(&s.path);
                    ui.label(format!("{kind}: {} ({})", file.display(), engine::human(s.bytes)));
                }
                let unity = e.info.shaders.len() - shown.len();
                if unity > 0 {
                    ui.label(crate::trf!(
                        "Pliki Unity z obiektami shaderów: {} ({})",
                        "Unity files with shader objects: {} ({})",
                        unity,
                        engine::human(e.info.shader_bytes())
                    ));
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("Otwórz folder gry", "Open game folder")).clicked() {
                        let _ = std::process::Command::new("explorer").arg(&e.game.dir).spawn();
                    }
                });
                // Ostatnia kompilacja zapisana w stanie.
                if let Some(r) = self.saved.games.get(&e.game.id) {
                    let cache = r
                        .cache_bytes
                        .map(engine::human)
                        .unwrap_or_else(|| tr("liczenie...", "measuring...").to_owned());
                    ui.label(
                        RichText::new(crate::trf!(
                            "Ostatnia kompilacja: {} - sterownik {}, {}, shadery: {} (odrzucone: {}), czas: {} s, pamięć podręczna gry: {}",
                            "Last compile: {} - driver {}, {}, shaders: {} (rejected: {}), time: {} s, game cache: {}",
                            r.when,
                            r.driver,
                            r.api,
                            r.ok,
                            r.failed,
                            r.secs,
                            cache
                        ))
                        .small()
                        .color(muted()),
                    );
                    if !r.exe.is_empty() {
                        let fresh = statuses.get(&e.game.id) == Some(&state::Status::Fresh);
                        store_ui(ui, &e.game.id, &r.exe, fresh, &mut self.stores, &self.restores);
                    }
                }
                compile_ui(
                    ui,
                    e,
                    &mut self.warms,
                    &mut self.apis,
                    &mut self.cleans,
                    &mut self.confirm_clean,
                    &mut request,
                );
            });
        }
        // Kompilacja z przycisku: przez kolejkę zapisu wyników.
        let start = request.and_then(|api| {
            let e = self
                .selected
                .as_deref()
                .and_then(|id| entries.iter().find(|e| e.game.id == id))?;
            Some((
                Entry {
                    game: e.game.clone(),
                    info: e.info.clone(),
                },
                api,
            ))
        });
        drop(scan);
        if let Some((e, api)) = start {
            let ctx = ui.ctx().clone();
            self.queue.retain(|id| *id != e.game.id);
            if let Err(why) = self.start_compile(&e, api, move || ctx.request_repaint()) {
                self.queue_note = Some(why);
            }
        }
    }

    /// Kolejka: „Kompiluj wszystkie wymagające”, postęp i automatyczna
    /// kompilacja po nowym sterowniku.
    fn queue_ui(&mut self, ui: &mut egui::Ui) {
        let needing = self.needing();
        ui.horizontal_wrapped(|ui| {
            let label = crate::trf!(
                "Kompiluj wszystkie wymagające ({})",
                "Compile all that need it ({})",
                needing.len()
            );
            if ui
                .add_enabled(!needing.is_empty(), egui::Button::new(label))
                .clicked()
            {
                self.enqueue(needing.clone());
                self.queue_note = None;
            }
            if !self.queue.is_empty() || !self.tracking.is_empty() {
                let now = self.tracking.keys().next().and_then(|id| {
                    let scan = self.scan.lock().unwrap();
                    scan.entries
                        .iter()
                        .flatten()
                        .find(|e| &e.game.id == id)
                        .map(|e| e.game.name.clone())
                });
                ui.label(
                    RichText::new(crate::trf!(
                        "Teraz: {} - w kolejce: {}",
                        "Now: {} - queued: {}",
                        now.unwrap_or_else(|| "-".into()),
                        self.queue.len()
                    ))
                    .color(accent()),
                );
                if !self.queue.is_empty()
                    && ui.button(tr("Wyczyść kolejkę", "Clear queue")).clicked()
                {
                    self.queue.clear();
                }
            }
        });
        let mut auto = self.saved.auto_after_driver;
        if ui
            .checkbox(
                &mut auto,
                tr(
                    "Po instalacji nowego sterownika karty kompiluj ponownie w tle gry, które były skompilowane (sterownik czyści pamięć podręczną shaderów)",
                    "After a new graphics driver is installed, recompile in the background the games that were compiled (the driver clears its shader cache)",
                ),
            )
            .changed()
        {
            self.saved.auto_after_driver = auto;
            // Bieżący sterownik uznajemy za obsłużony - auto dotyczy następnych.
            if auto {
                self.saved.auto_driver = self.driver.clone().unwrap_or_default();
            }
            state::save(&self.saved);
        }
        if let Some(note) = &self.queue_note {
            ui.label(RichText::new(note).small().color(warn()));
        }
    }
}

/// Wybór API kompilacji shaderów.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Api {
    /// API, którego używa gra (z konfiguracji gry albo jej logu).
    #[default]
    Auto,
    Dx11,
    Dx12,
}

/// Co da się skompilować dla gry i co wybiera AUTO.
struct ApiOptions {
    dx11: Result<(), &'static str>,
    dx12: Result<(), &'static str>,
    /// Wynik AUTO, z opisem dlaczego.
    auto: Result<Api, &'static str>,
}

fn api_options(info: &Info) -> ApiOptions {
    let has = |k: ShaderKind| info.shaders.iter().any(|s| s.kind == k);
    let (dx11, dx12) = match info.engine {
        Engine::Dagor => (
            if has(ShaderKind::DagorDx11) { Ok(()) } else { Err(tr("gra nie ma zrzutu shaderów DirectX 11", "the game has no DirectX 11 shader dump")) },
            if has(ShaderKind::DagorDx12) { Ok(()) } else { Err(tr("gra nie ma zrzutu shaderów DirectX 12", "the game has no DirectX 12 shader dump")) },
        ),
        Engine::Unity => (
            Ok(()),
            Err(tr(
                "Unity w DirectX 12 buduje potoki dopiero w trakcie gry - z samych plików nie da się ich skompilować",
                "Unity on DirectX 12 builds pipelines at run time - they cannot be compiled from the files alone",
            )),
        ),
        Engine::Unreal if !info.encrypted && has_pak(info) => (
            Ok(()),
            Err(tr(
                "Unreal w DirectX 12: w przygotowaniu (potrzebne potoki i root signature)",
                "Unreal on DirectX 12: in progress (needs pipelines and root signatures)",
            )),
        ),
        _ => {
            let no = tr("ten silnik nie jest obsługiwany", "this engine is not supported");
            (Err(no), Err(no))
        }
    };
    let game = info.api.as_deref();
    let auto = match game {
        Some("DirectX 12") => dx12.map(|_| Api::Dx12),
        Some("DirectX 11") => dx11.map(|_| Api::Dx11),
        // API nieznane: najpierw DX12, potem DX11.
        _ => dx12.map(|_| Api::Dx12).or(dx11.map(|_| Api::Dx11)),
    };
    ApiOptions { dx11, dx12, auto }
}

/// API kompilacji z wyboru użytkownika (AUTO = API gry, jeśli obsługiwane).
fn resolve_api(choice: Api, options: &ApiOptions) -> Result<Api, &'static str> {
    match choice {
        Api::Auto => options.auto,
        a => Ok(a),
    }
}

/// Zadanie kompilacji gry dla API: (nazwa .exe gry, zadanie, opis).
/// `Ok(None)` - dla tego API i silnika nie ma czego kompilować.
fn job_for(e: &Entry, api: Api) -> Result<Option<(String, warm::Job, String)>, &'static str> {
    let d3d11 = api == Api::Dx11;
    let dump_of = |k: ShaderKind| e.info.shaders.iter().find(|s| s.kind == k);
    let dagor_dx12 = (api == Api::Dx12)
        .then(|| dump_of(ShaderKind::DagorDx12))
        .flatten();
    let dagor_dx11 = (d3d11 && e.info.engine == Engine::Dagor)
        .then(|| dump_of(ShaderKind::DagorDx11))
        .flatten();
    let no_exe = tr(
        "Nie znaleziono pliku .exe gry w win64.",
        "Game .exe not found in win64.",
    );
    Ok(Some(if let Some(dump) = dagor_dx11 {
        let exe = warm::dagor_game_exe(&e.game.dir).ok_or(no_exe)?;
        let about = crate::trf!(
            "Sterownik karty kompiluje wszystkie shadery DirectX 11 ze zrzutu gry do swojej pamięci podręcznej pod nazwą {exe}, tak jak zrobiłaby to gra. Gra musi być zamknięta.",
            "The graphics driver compiles every DirectX 11 shader from the game's dump into its cache under the name {exe}, as the game would. The game must be closed."
        );
        let job = warm::Job::DagorDx11 {
            dump: dump.path.clone(),
        };
        (exe, job, about)
    } else if let Some(dump) = dagor_dx12 {
        let exe = warm::dagor_game_exe(&e.game.dir).ok_or(no_exe)?;
        let about = crate::trf!(
            "Sterownik karty kompiluje shadery gry do swojej pamięci podręcznej pod nazwą {exe}, tak jak zrobiłaby to gra. Gra musi być zamknięta. Teraz: shadery obliczeniowe (compute); graficzne w następnej wersji.",
            "The graphics driver compiles the game's shaders into its cache under the name {exe}, as the game would. The game must be closed. Now: compute shaders; graphics shaders in the next version."
        );
        let job = warm::Job::Dagor {
            dump: dump.path.clone(),
            cbv_ranges: warm::dagor_cbv_ranges(&e.game.dir),
        };
        (exe, job, about)
    } else if d3d11 && e.info.engine == Engine::Unity {
        let (data_dir, exe) = unity::game_layout(&e.game.dir).ok_or(tr(
            "Nie znaleziono pliku .exe i folderu _Data gry.",
            "Game .exe and _Data folder not found.",
        ))?;
        let about = crate::trf!(
            "Tune Center wyszukuje w plikach gry wszystkie shadery DirectX 11 (ze wszystkimi wariantami), a sterownik karty kompiluje je do swojej pamięci podręcznej pod nazwą {exe}, tak jak zrobiłaby to gra. Gra musi być zamknięta.",
            "Tune Center finds every DirectX 11 shader in the game files (with all variants) and the graphics driver compiles them into its cache under the name {exe}, as the game would. The game must be closed."
        );
        (exe, warm::Job::Unity { data_dir }, about)
    } else if d3d11 && e.info.engine == Engine::Unreal {
        let missing = tr(
            "Nie znaleziono paczki .pak albo pliku *-Shipping.exe gry.",
            "Game .pak or *-Shipping.exe not found.",
        );
        let pak = unreal_pak::find_pak(&e.game.dir).ok_or(missing)?;
        let exe = warm::unreal_game_exe(&e.game.dir).ok_or(missing)?;
        let about = crate::trf!(
            "Tune Center czyta archiwa shaderów DirectX 11 (SM5) z paczki gry, a sterownik karty kompiluje je do swojej pamięci podręcznej pod nazwą {exe}, tak jak zrobiłaby to gra. Gra musi być zamknięta.",
            "Tune Center reads the DirectX 11 (SM5) shader archives from the game package and the graphics driver compiles them into its cache under the name {exe}, as the game would. The game must be closed."
        );
        (exe, warm::Job::Unreal { pak }, about)
    } else {
        return Ok(None);
    }))
}

/// Folder shaderów gry (patrz `store`): ile plików jest dowiązanych do pamięci
/// podręcznej sterownika i - gdy z niej zniknęły - przywrócenie ich z folderu.
fn store_ui(
    ui: &mut egui::Ui,
    id: &str,
    exe: &str,
    fresh: bool,
    stores: &mut std::collections::HashMap<String, (std::time::Instant, store::Info)>,
    restores: &Restores,
) {
    let cache = cache_dirs();
    let Some(dir) = store::game_dir(exe).filter(|_| !cache.is_empty()) else {
        return;
    };
    let restore = restores.lock().unwrap().get(id).cloned();
    let stale = stores
        .get(id)
        .is_none_or(|(at, _)| at.elapsed() > std::time::Duration::from_secs(3));
    if stale {
        stores.insert(
            id.to_owned(),
            (std::time::Instant::now(), store::info(&cache, &dir)),
        );
    }
    let info = stores[id].1;
    if info.files == 0 {
        return;
    }
    ui.label(
        RichText::new(crate::trf!(
            "Folder shaderów gry: {} plików ({}) dowiązanych do pamięci podręcznej sterownika - {}",
            "Game shader folder: {} files ({}) linked to the driver cache - {}",
            info.files,
            engine::human(info.bytes),
            dir.display()
        ))
        .small()
        .color(muted()),
    );
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(tr("Otwórz folder shaderów", "Open shader folder"))
            .on_hover_text(tr(
                "Pliki w tym folderze to twarde dowiązania do plików w DXCache: te same dane pod drugą nazwą, bez kopii na dysku.",
                "The files in this folder are hard links to the files in DXCache: the same data under a second name, with no copy on disk.",
            ))
            .clicked()
        {
            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
        }
        if info.missing > 0 && restore != Some(Restore::Running) {
            ui.label(
                RichText::new(crate::trf!(
                    "W pamięci podręcznej sterownika brakuje {} plików tej gry.",
                    "{} of this game's files are missing from the driver cache.",
                    info.missing
                ))
                .color(warn()),
            );
            // Po zmianie sterownika pliki z folderu są nieaktualne - wtedy
            // potrzebna jest ponowna kompilacja, nie przywracanie.
            if fresh
                && ui
                    .button(tr("Przywróć z folderu", "Restore from folder"))
                    .on_hover_text(tr(
                        "Przywraca brakujące pliki bez ponownej kompilacji.",
                        "Puts the missing files back without compiling again.",
                    ))
                    .clicked()
            {
                restores
                    .lock()
                    .unwrap()
                    .insert(id.to_owned(), Restore::Running);
                stores.remove(id);
                let (shared, id, exe, ctx) =
                    (restores.clone(), id.to_owned(), exe.to_owned(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let result = match warm::restore_game(&exe) {
                        Ok((restored, failed)) => Restore::Done { restored, failed },
                        Err(e) => Restore::Failed(e),
                    };
                    shared.lock().unwrap().insert(id, result);
                    ctx.request_repaint();
                });
            }
        }
        if restore == Some(Restore::Running) {
            ui.spinner();
        }
    });
    match restore {
        Some(Restore::Done { restored, failed }) => {
            ui.label(
                RichText::new(crate::trf!(
                    "Przywrócono {restored} plików, nieudane: {failed}.",
                    "Restored {restored} files, failed: {failed}."
                ))
                .color(if failed == 0 { accent() } else { warn() }),
            );
        }
        Some(Restore::Failed(err)) => {
            ui.label(
                RichText::new(crate::trf!("Przywracanie: {err}", "Restoring: {err}")).color(warn()),
            );
        }
        _ => {}
    }
}

/// Kompilacja z góry dla wybranej gry: DirectX 11 (Unity, Dagor) albo
/// DirectX 12 (Dagor, compute), według wyboru API.
fn compile_ui(
    ui: &mut egui::Ui,
    e: &Entry,
    warms: &mut std::collections::HashMap<String, warm::Warm>,
    choices: &mut std::collections::HashMap<String, Api>,
    cleans: &mut std::collections::HashMap<String, warm::Clean>,
    confirm: &mut Option<String>,
    request: &mut Option<Api>,
) {
    let missing = |ui: &mut egui::Ui, text: &str| {
        ui.label(RichText::new(text).color(warn()));
    };
    let options = api_options(&e.info);
    if options.dx11.is_err() && options.dx12.is_err() {
        return;
    }
    ui.add_space(6.0);
    let choice = choices.entry(e.game.id.clone()).or_default();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(tr("API kompilacji:", "Compile for:")).color(muted()));
        for (api, label, available) in [
            (Api::Auto, "AUTO", Ok(())),
            (Api::Dx11, "DX11", options.dx11),
            (Api::Dx12, "DX12", options.dx12),
        ] {
            let button = ui.add_enabled(available.is_ok(), egui::Button::selectable(*choice == api, label));
            let button = match available {
                Err(why) => button.on_disabled_hover_text(why),
                Ok(()) => button,
            };
            if button.clicked() {
                *choice = api;
            }
        }
        let game_api = e.info.api.as_deref().unwrap_or(tr("nieznane", "unknown"));
        match (*choice, options.auto) {
            (Api::Auto, Ok(a)) => ui.label(
                RichText::new(crate::trf!(
                    "AUTO = {} (gra: {game_api})",
                    "AUTO = {} (game: {game_api})",
                    if a == Api::Dx12 { "DX12" } else { "DX11" }
                ))
                .small()
                .color(muted()),
            ),
            (Api::Auto, Err(_)) => ui.label(
                RichText::new(crate::trf!("gra: {game_api}", "game: {game_api}")).small().color(muted()),
            ),
            (api, _) => {
                let mismatch = match api {
                    Api::Dx11 => game_api == "DirectX 12",
                    _ => game_api == "DirectX 11",
                };
                if mismatch {
                    ui.label(
                        RichText::new(crate::trf!(
                            "gra używa {game_api} - pamięć podręczna innego API nie przyspieszy gry",
                            "the game uses {game_api} - another API's cache will not speed the game up"
                        ))
                        .small()
                        .color(warn()),
                    )
                } else {
                    ui.label("")
                }
            }
        };
    });
    let api = match resolve_api(*choice, &options) {
        Ok(a) => a,
        Err(why) => {
            missing(ui, why);
            return;
        }
    };
    let d3d11 = api == Api::Dx11;
    let (exe, _job, about) = match job_for(e, api) {
        Ok(Some(j)) => j,
        Ok(None) => return,
        Err(why) => {
            missing(ui, why);
            return;
        }
    };
    ui.label(
        RichText::new(tr("Kompilacja shaderów", "Shader compilation"))
            .strong()
            .color(accent()),
    );
    ui.label(RichText::new(about).small().color(muted()));
    let w = warms.entry(e.game.id.clone()).or_default();
    let p = w.progress.lock().unwrap().clone();
    let running = p.phase == warm::Phase::Running;
    let c = cleans.entry(e.game.id.clone()).or_default();
    let clean_state = c.state.lock().unwrap().clone();
    let cleaning = clean_state == warm::CleanState::Running;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !running && !cleaning,
                egui::Button::new(tr("Kompiluj shadery", "Compile shaders")),
            )
            .clicked()
        {
            *request = Some(api);
        }
        if ui
            .add_enabled(
                !running && !cleaning,
                egui::Button::new(tr("Wyczyść shadery", "Clean shaders")),
            )
            .on_hover_text(crate::trf!(
                "Usuwa skompilowane shadery gry {exe} z pamięci podręcznej sterownika karty i Windows (DXCache / DxcCache i D3DSCache, DirectX 11 i 12) oraz z folderu shaderów gry.",
                "Removes the compiled shaders of {exe} from the graphics driver and Windows caches (DXCache / DxcCache and D3DSCache, DirectX 11 and 12) and from the game shader folder."
            ))
            .clicked()
        {
            *confirm = Some(e.game.id.clone());
        }
        if running || cleaning {
            ui.spinner();
        }
    });

    // Potwierdzenie czyszczenia.
    if confirm.as_deref() == Some(e.game.id.as_str()) {
        let mut answer = None;
        egui::Modal::new(ui.scope_id().with(("clean_confirm", &e.game.id))).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.label(
                RichText::new(tr(
                    "Czy jesteś pewny, że chcesz wyczyścić?",
                    "Are you sure you want to clean?",
                ))
                .strong()
                .size(16.0),
            );
            ui.add_space(6.0);
            ui.label(crate::trf!(
                "Skompilowane shadery gry {exe} zostaną usunięte z pamięci podręcznej sterownika karty i Windows (DXCache / DxcCache i D3DSCache, DirectX 11 i 12) oraz z folderu shaderów gry. Gra skompiluje je ponownie w trakcie rozgrywki - albo użyj \"Kompiluj shadery\". Gra musi być zamknięta.",
                "The compiled shaders of {exe} will be removed from the graphics driver and Windows caches (DXCache / DxcCache and D3DSCache, DirectX 11 and 12) and from the game shader folder. The game will compile them again while playing - or use \"Compile shaders\". The game must be closed."
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(tr("Tak, wyczyść", "Yes, clean")).color(warn()))
                    .clicked()
                {
                    answer = Some(true);
                }
                if ui.button(tr("Anuluj", "Cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(yes) = answer {
            *confirm = None;
            if yes {
                let ctx = ui.ctx().clone();
                c.start(&exe, move || ctx.request_repaint());
            }
        }
    }
    match &clean_state {
        warm::CleanState::Done {
            files,
            bytes,
            locked,
        } => {
            let mut text = crate::trf!(
                "Wyczyszczono: {} plików ({}).",
                "Cleaned: {} files ({}).",
                files,
                engine::human(*bytes)
            );
            if *locked > 0 {
                text.push(' ');
                text.push_str(&crate::trf!(
                    "Nie usunięto {} plików - używa ich inny proces.",
                    "{} files were not removed - another process is using them.",
                    locked
                ));
            }
            ui.label(RichText::new(text).color(if *locked == 0 { accent() } else { warn() }));
        }
        warm::CleanState::Failed(err) => {
            ui.label(
                RichText::new(crate::trf!("Czyszczenie: {err}", "Cleaning: {err}")).color(warn()),
            );
        }
        _ => {}
    }
    // Unity: najpierw przegląd plików gry (znalezione shadery kompilują się
    // równolegle, więc liczba wszystkich rośnie do końca przeglądu).
    if let Some((files_done, files)) = p.scan.filter(|(d, t)| running && d < t) {
        ui.label(
            RichText::new(crate::trf!(
                "Przeglądanie plików gry: {} / {}, znalezione shadery: {}",
                "Scanning the game files: {} / {}, shaders found: {}",
                files_done,
                files,
                p.total
            ))
            .small()
            .color(muted()),
        );
    }
    if running || p.total > 0 {
        let fraction = if p.total > 0 {
            p.done as f32 / p.total as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(420.0)
                .text(crate::trf!(
                    "{} / {} shaderów",
                    "{} / {} shaders",
                    p.done,
                    p.total
                )),
        );
    }
    match &p.phase {
        warm::Phase::Done if d3d11 => {
            let text = crate::trf!(
                "Gotowe: sterownik skompilował {} shaderów (shadery, które już były w pamięci podręcznej, pomija), odrzucone: {} ({}).",
                "Done: the driver compiled {} shaders (shaders already in its cache are skipped), rejected: {} ({}).",
                p.ok,
                p.failed,
                p.adapter
            );
            ui.label(RichText::new(text).color(if p.failed == 0 { accent() } else { warn() }));
        }
        warm::Phase::Done => {
            let text = crate::trf!(
                "Gotowe: {} shaderów w pamięci podręcznej sterownika (nowo skompilowane: ok. {}, już były: ok. {}), odrzucone: {} ({}).",
                "Done: {} shaders in the driver cache (newly compiled: about {}, already there: about {}), rejected: {} ({}).",
                p.ok,
                p.ok.saturating_sub(p.cached),
                p.cached,
                p.failed,
                p.adapter
            );
            ui.label(RichText::new(text).color(if p.failed == 0 { accent() } else { warn() }));
        }
        warm::Phase::Failed(err) => {
            ui.label(RichText::new(crate::trf!("Błąd: {err}", "Error: {err}")).color(warn()));
        }
        _ => {}
    }
    for line in p.errors.iter().chain(p.warnings.iter()).take(5) {
        ui.label(RichText::new(line).small().color(muted()));
    }
}

#[cfg(test)]
mod hardware {
    //! Skan gier na tym komputerze (tylko odczyt):
    //! `cargo test shaders_scan -- --ignored --nocapture`.
    use super::*;

    #[test]
    #[ignore]
    fn shaders_scan() {
        let t = std::time::Instant::now();
        for game in games::find_all() {
            let info = engine::inspect(&game.dir);
            let (plan, ok) = plan(&info);
            println!(
                "{} [{}] {} {:?} api={:?} ac={:?} shadery={} ({}) scskiller={} -> {} {}",
                game.name,
                game.store.name(),
                info.engine.name(),
                info.version,
                info.api,
                info.anti_cheat,
                info.shaders.len(),
                engine::human(info.shader_bytes()),
                info.scskiller_recorder,
                ok,
                plan
            );
        }
        println!("czas: {:?}", t.elapsed());
        println!(
            "cache sterownika: {:?}",
            cache_dirs().iter().map(|d| engine::human(dir_size(d))).collect::<Vec<_>>()
        );
    }
}
