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
        Engine::Dagor if has(ShaderKind::DagorDx12) => (
            tr(
                "DirectX 12: compute gotowe, graficzne w przygotowaniu",
                "DirectX 12: compute ready, graphics in progress",
            ),
            true,
        ),
        Engine::Dagor if has(ShaderKind::DagorDx11) => (
            tr("DirectX 11 - w przygotowaniu", "DirectX 11 - in progress"),
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
                compile_ui(ui, e, &mut self.warms);
            });
        }
    }
}

/// Kompilacja z góry dla wybranej gry: Dagor DirectX 12 (compute) albo
/// Unity DirectX 11 (wszystkie shadery z plików gry).
fn compile_ui(
    ui: &mut egui::Ui,
    e: &Entry,
    warms: &mut std::collections::HashMap<String, warm::Warm>,
) {
    let missing = |ui: &mut egui::Ui, text: &str| {
        ui.label(RichText::new(text).color(warn()));
    };
    let unity = e.info.engine == Engine::Unity && e.info.api.as_deref() != Some("DirectX 12");
    let dagor = e
        .info
        .shaders
        .iter()
        .find(|s| s.kind == ShaderKind::DagorDx12);
    let (exe, job, about) = if let Some(dump) = dagor {
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
    } else if unity {
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
    ui.add_space(6.0);
    ui.label(
        RichText::new(tr("Kompilacja shaderów", "Shader compilation"))
            .strong()
            .color(accent()),
    );
    ui.label(RichText::new(about).small().color(muted()));
    let w = warms.entry(e.game.id.clone()).or_default();
    let p = w.progress.lock().unwrap().clone();
    let running = p.phase == warm::Phase::Running;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !running,
                egui::Button::new(tr("Kompiluj shadery", "Compile shaders")),
            )
            .clicked()
        {
            let ctx = ui.ctx().clone();
            w.start(&exe, job, move || ctx.request_repaint());
        }
        if running {
            ui.spinner();
        }
    });
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
        warm::Phase::Done if unity => {
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
