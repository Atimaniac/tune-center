//! Raport źródeł shaderów (`tunecenter.exe --shader-report [fragment nazwy
//! gry]`): co Tune Center widzi w plikach każdej gry - tylko odczyt, bez
//! kompilacji i bez dotykania pamięci podręcznej sterownika.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::engine::{self, Engine, ShaderKind};

/// Argument wiersza poleceń.
pub const ARG: &str = "--shader-report";
const FILE: &str = "tunecenter-shaders.txt";

pub fn run(filter: Option<&str>) -> Result<PathBuf, String> {
    let mut text = String::new();
    let mut say = |line: String| {
        println!("{line}");
        text.push_str(&line);
        text.push('\n');
    };
    say(format!("Tune Center {} - shader sources (read only)", env!("CARGO_PKG_VERSION")));
    #[cfg(windows)]
    match super::pick_adapter() {
        Ok((_, vendor, name)) => say(format!("GPU: {name} ({vendor:?})")),
        Err(e) => say(format!("GPU: {e}")),
    }
    let filter = filter.map(str::to_lowercase);
    for game in super::games::find_all() {
        if filter.as_ref().is_some_and(|f| !game.name.to_lowercase().contains(f)) {
            continue;
        }
        let info = engine::inspect(&game.dir);
        say(String::new());
        say(format!(
            "== {} [{}] {} - engine {} {} api {} encrypted {} anti-cheat {:?}",
            game.name,
            game.store.name(),
            game.dir.display(),
            info.engine.name(),
            info.version.as_deref().unwrap_or("-"),
            info.api.as_deref().unwrap_or("-"),
            info.encrypted,
            info.anti_cheat
        ));
        let (plan, ok) = super::plan(&info);
        say(format!("plan: {plan} ({})", if ok { "possible" } else { "not possible" }));
        let started = std::time::Instant::now();
        match info.engine {
            Engine::Source2 => source2(&game.dir, &mut say),
            Engine::Unreal if !info.encrypted => unreal(&game.dir, &mut say),
            Engine::Dagor => dagor(&game.dir, &info, &mut say),
            Engine::Unknown if info.shaders.iter().any(|s| s.kind == ShaderKind::LooseDxbc) => {
                loose(&game.dir, &mut say)
            }
            _ => {}
        }
        say(format!("({:.1} s)", started.elapsed().as_secs_f32()));
    }
    for dir in [
        std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)),
        Some(std::env::temp_dir()),
    ]
    .into_iter()
    .flatten()
    {
        let path = dir.join(FILE);
        if std::fs::write(&path, &text).is_ok() {
            return Ok(path);
        }
    }
    Err("nie udało się zapisać raportu / could not save the report".into())
}

fn source2(game: &Path, say: &mut dyn FnMut(String)) {
    say(format!("exe: {:?}", super::source2::game_exe(game)));
    let mut unique = HashSet::new();
    for vpk in super::source2::shader_vpks(game) {
        let p = match super::source2::Vpk::open(&vpk) {
            Ok(p) => p,
            Err(e) => {
                say(format!("{}: {e}", vpk.display()));
                continue;
            }
        };
        let files: Vec<_> = p.entries.iter().filter(|e| super::source2::is_dx_shader(&e.path)).collect();
        let (mut failed, mut errors) = (0, 0);
        for e in &files {
            match p.read(e) {
                Ok(b) => {
                    failed += super::source2::carve_vcs(&b, &mut |d| {
                        unique.insert(d.digest());
                    })
                }
                Err(_) => errors += 1,
            }
        }
        say(format!(
            "{}: files {}, DirectX .vcs {}, zstd frames not unpacked {failed}, read errors {errors}",
            vpk.display(),
            p.entries.len(),
            files.len()
        ));
    }
    say(format!("unique DXBC shaders: {}", unique.len()));
}

fn loose(game: &Path, say: &mut dyn FnMut(String)) {
    say(format!("exe: {:?}", super::loose::game_exe(game)));
    let mut unique = HashSet::new();
    let files = super::loose::shader_files(game);
    for (f, len) in files.iter().take(200) {
        let before = unique.len();
        let r = super::loose::scan_file(f, &mut |d| {
            unique.insert(d.digest());
        });
        say(format!("{} ({}): +{} {}", f.display(), engine::human(*len), unique.len() - before, r.err().unwrap_or_default()));
    }
    say(format!("shader files {}, unique DXBC shaders: {}", files.len(), unique.len()));
}

fn unreal(game: &Path, say: &mut dyn FnMut(String)) {
    say(format!(
        "exe: {:?}, UE 5.{}",
        super::warm::unreal_game_exe(game),
        super::ue12::game_minor(game).map_or("?".into(), |m| m.to_string())
    ));
    // IoStore: biblioteki shaderów i czy mają kod DirectX 11.
    let io = super::unreal::IoShaders::open(game);
    if !io.tocs.is_empty() || !io.notes.is_empty() {
        let sm5 = (0..io.libs.len()).filter(|&l| io.has_sm5(l)).count();
        say(format!(
            "IoStore: containers {}, shader libraries {} (with DirectX 11 SM5: {sm5})",
            io.tocs.len(),
            io.libs.len()
        ));
        for n in io.notes.iter().take(5) {
            say(format!("  {n}"));
        }
    }
    // Pamięci potoków DirectX 12.
    let caches = super::ue12::pso_caches(game);
    if caches.is_empty() {
        say("DX12 pipeline cache (*.upipelinecache): none".into());
        return;
    }
    let mut wanted = HashSet::new();
    let mut mesh = 0;
    for (name, bytes) in &caches {
        match super::ue12::PsoCache::parse(bytes) {
            Ok(c) => {
                say(format!(
                    "{name}: v{} platform {} graphics {} compute {} skipped {}",
                    c.version,
                    c.platform,
                    c.graphics.len(),
                    c.compute.len(),
                    c.skipped
                ));
                for g in &c.graphics {
                    if g.ms != [0; 20] {
                        mesh += 1;
                    }
                    for h in [g.vs, g.ps, g.gs] {
                        if h != [0; 20] {
                            wanted.insert(h);
                        }
                    }
                }
                wanted.extend(c.compute.iter().copied());
            }
            Err(e) => say(format!("{name}: {e}")),
        }
    }
    let mut notes = Vec::new();
    let shaders = super::ue12::load_shaders(game, &wanted, &mut notes);
    let parsed = shaders.values().filter(|b| super::ue12::parse_packed(b).is_some()).count();
    say(format!(
        "shaders needed {}, found in archives {}, with DXIL container {parsed}, mesh pipelines (skipped) {mesh}",
        wanted.len(),
        shaders.len()
    ));
    for n in notes.iter().take(5) {
        say(format!("  {n}"));
    }
}

fn dagor(game: &Path, info: &engine::Info, say: &mut dyn FnMut(String)) {
    say(format!("exe: {:?}", super::warm::dagor_game_exe(game)));
    let path = game.join("cache").join("dx12.cache");
    if !path.is_file() {
        say("cache\\dx12.cache: none (DX12 graphics pipelines appear after the first DX12 session)".into());
        return;
    }
    let cache = match super::dagor_cache::Cache::open(&path) {
        Ok(c) => c,
        Err(e) => {
            say(format!("cache\\dx12.cache: {e}"));
            return;
        }
    };
    say(format!(
        "cache\\dx12.cache: VS+PS pairs {}, variants {}, states {}, input layouts {}, framebuffers {}",
        cache.graphics.len(),
        cache.graphics.iter().map(|g| g.variants.len()).sum::<usize>(),
        cache.static_states.len(),
        cache.input_layouts.len(),
        cache.framebuffers.len()
    ));
    let Some(dump) = info.shaders.iter().find(|s| s.kind == ShaderKind::DagorDx12) else {
        return;
    };
    let Ok(dump) = super::dagor::Dump::open(&dump.path) else {
        say("DX12 shader dump: unreadable".into());
        return;
    };
    let hashes: HashSet<[u8; 20]> = (0..dump.shader_count())
        .filter_map(|i| dump.metadata(i)?.get(4..24)?.try_into().ok())
        .collect();
    let vs = cache.graphics.iter().filter(|g| hashes.contains(&g.vs)).count();
    let ps = cache.graphics.iter().filter(|g| hashes.contains(&g.ps)).count();
    say(format!(
        "pairs with VS in dump {vs} / {}, with PS in dump {ps} (others use the engine's empty pixel shader)",
        cache.graphics.len()
    ));
}
