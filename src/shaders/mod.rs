//! Podzakładka NVIDIA > Shader compilation: wyszukiwanie zainstalowanych gier,
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
pub mod unity;
pub mod warm;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::theme::{accent, muted, warn};
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
}

/// Folder pamięci podręcznej shaderów DirectX sterownika Nvidia.
pub fn nvidia_cache_dir() -> Option<PathBuf> {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let dir = local.join("NVIDIA").join("DXCache");
    Some(dir)
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
            let cache = nvidia_cache_dir()
                .filter(|d| d.is_dir())
                .map(|d| dir_size(&d));
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
            .num_columns(6)
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
                compile_ui(
                    ui,
                    e,
                    &mut self.warms,
                    &mut self.apis,
                    &mut self.cleans,
                    &mut self.confirm_clean,
                );
            });
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

/// Kompilacja z góry dla wybranej gry: DirectX 11 (Unity, Dagor) albo
/// DirectX 12 (Dagor, compute), według wyboru API.
fn compile_ui(
    ui: &mut egui::Ui,
    e: &Entry,
    warms: &mut std::collections::HashMap<String, warm::Warm>,
    choices: &mut std::collections::HashMap<String, Api>,
    cleans: &mut std::collections::HashMap<String, warm::Clean>,
    confirm: &mut Option<String>,
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
    let api = match *choice {
        Api::Auto => match options.auto {
            Ok(a) => a,
            Err(why) => {
                missing(ui, why);
                return;
            }
        },
        a => a,
    };
    let d3d11 = api == Api::Dx11;
    let dump_of = |k: ShaderKind| e.info.shaders.iter().find(|s| s.kind == k);
    let dagor_dx12 = (api == Api::Dx12)
        .then(|| dump_of(ShaderKind::DagorDx12))
        .flatten();
    let dagor_dx11 = (d3d11 && e.info.engine == Engine::Dagor)
        .then(|| dump_of(ShaderKind::DagorDx11))
        .flatten();
    let (exe, job, about) = if let Some(dump) = dagor_dx11 {
        let Some(exe) = warm::dagor_game_exe(&e.game.dir) else {
            missing(
                ui,
                tr(
                    "Nie znaleziono pliku .exe gry w win64.",
                    "Game .exe not found in win64.",
                ),
            );
            return;
        };
        let about = crate::trf!(
            "Sterownik Nvidia kompiluje wszystkie shadery DirectX 11 ze zrzutu gry do swojej pamięci podręcznej pod nazwą {exe}, tak jak zrobiłaby to gra. Gra musi być zamknięta.",
            "The Nvidia driver compiles every DirectX 11 shader from the game's dump into its cache under the name {exe}, as the game would. The game must be closed."
        );
        (
            exe,
            warm::Job::DagorDx11 {
                dump: dump.path.clone(),
            },
            about,
        )
    } else if let Some(dump) = dagor_dx12 {
        let Some(exe) = warm::dagor_game_exe(&e.game.dir) else {
            missing(
                ui,
                tr(
                    "Nie znaleziono pliku .exe gry w win64.",
                    "Game .exe not found in win64.",
                ),
            );
            return;
        };
        let about = crate::trf!(
            "Sterownik Nvidia kompiluje shadery gry do swojej pamięci podręcznej pod nazwą {exe}, tak jak zrobiłaby to gra. Gra musi być zamknięta. Teraz: shadery obliczeniowe (compute); graficzne w następnej wersji.",
            "The Nvidia driver compiles the game's shaders into its cache under the name {exe}, as the game would. The game must be closed. Now: compute shaders; graphics shaders in the next version."
        );
        let job = warm::Job::Dagor {
            dump: dump.path.clone(),
            cbv_ranges: warm::dagor_cbv_ranges(&e.game.dir),
        };
        (exe, job, about)
    } else if d3d11 && e.info.engine == Engine::Unity {
        let Some((data_dir, exe)) = unity::game_layout(&e.game.dir) else {
            missing(
                ui,
                tr(
                    "Nie znaleziono pliku .exe i folderu _Data gry.",
                    "Game .exe and _Data folder not found.",
                ),
            );
            return;
        };
        let about = crate::trf!(
            "Tune Center wyszukuje w plikach gry wszystkie shadery DirectX 11 (ze wszystkimi wariantami), a sterownik Nvidia kompiluje je do swojej pamięci podręcznej pod nazwą {exe}, tak jak zrobiłaby to gra. Gra musi być zamknięta.",
            "Tune Center finds every DirectX 11 shader in the game files (with all variants) and the Nvidia driver compiles them into its cache under the name {exe}, as the game would. The game must be closed."
        );
        (exe, warm::Job::Unity { data_dir }, about)
    } else {
        return;
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
            *c.state.lock().unwrap() = warm::CleanState::Idle;
            let ctx = ui.ctx().clone();
            w.start(&exe, job, move || ctx.request_repaint());
        }
        if ui
            .add_enabled(
                !running && !cleaning,
                egui::Button::new(tr("Wyczyść shadery", "Clean shaders")),
            )
            .on_hover_text(crate::trf!(
                "Usuwa skompilowane shadery gry {exe} z pamięci podręcznej sterownika Nvidia i Windows (DXCache i D3DSCache, DirectX 11 i 12).",
                "Removes the compiled shaders of {exe} from the Nvidia driver and Windows caches (DXCache and D3DSCache, DirectX 11 and 12)."
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
                "Skompilowane shadery gry {exe} zostaną usunięte z pamięci podręcznej sterownika Nvidia i Windows (DXCache i D3DSCache, DirectX 11 i 12). Gra skompiluje je ponownie w trakcie rozgrywki - albo użyj \"Kompiluj shadery\". Gra musi być zamknięta.",
                "The compiled shaders of {exe} will be removed from the Nvidia driver and Windows caches (DXCache and D3DSCache, DirectX 11 and 12). The game will compile them again while playing - or use \"Compile shaders\". The game must be closed."
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
            nvidia_cache_dir().map(|d| engine::human(dir_size(&d)))
        );
    }
}
