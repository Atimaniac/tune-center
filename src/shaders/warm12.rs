//! Kompilacja potoków DirectX 12 z shaderów, które niosą root signature albo
//! których silnik ma stałe root signature:
//! - `carved` - kontenery DXIL/DXBC z częścią RTS0 wycięte z plików dowolnej
//!   gry (silnik nie musi być rozpoznany),
//! - `red3` - pamięć shaderów REDengine 3 (Wiedźmin 3 w DirectX 12).
//!
//! Proces działa pod nazwą .exe gry (jak `warm::run_host`), czyta pliki gry
//! tylko do odczytu i tworzy potoki na urządzeniu D3D12, żeby sterownik
//! zapisał je w swojej pamięci podręcznej. Na kartach NVIDIA kluczem jest
//! shader etapu i root signature, więc wystarczy, że każda taka para pojawi
//! się w jednym potoku; na AMD tworzone są wszystkie potoki.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{self, Container, Kind};
use super::red3;

/// Argument procesu kompilującego: `--shader-warm-d3d12x <carved|red3> <folder gry>`.
pub const HOST_ARG: &str = "--shader-warm-d3d12x";

/// Bajtów próbkowanych z plików bez nazwy shaderów, zanim zostaną pominięte.
const SAMPLE_BUDGET: u64 = 4 << 30;
const SAMPLE_WINDOW: u64 = 4 << 20;
/// Najwięcej potoków jednej gry (pamięć planu).
const MAX_PIPELINES: usize = 250_000;

/// Skąd wziąć bajty shadera.
#[derive(Clone)]
enum Src {
    Carved { file: usize, at: u64, size: u32 },
    Red { file: usize, entry: red3::Entry },
    Mem(Arc<Vec<u8>>),
    Reader(super::readers::Bytes),
}

/// `Job::rs` dla root signature wbudowanej w shader (część RTS0).
const EMBEDDED: usize = usize::MAX;

/// Etapy potoku: VS, PS, GS, HS, DS, CS (indeksy w `sources`).
#[derive(Clone, Default)]
struct Job {
    rs: usize,
    stages: [Option<usize>; 6],
    /// Inne shadery wierzchołków do spróbowania, gdy sterownik odrzuci parę.
    alternatives: Vec<usize>,
}

#[derive(Default)]
struct Plan {
    files: Vec<std::fs::File>,
    sources: Vec<Src>,
    /// Root signature (zserializowane).
    signatures: Vec<Vec<u8>>,
    jobs: Vec<Job>,
}

impl Plan {
    fn load(&self, id: usize) -> Option<Arc<Vec<u8>>> {
        match &self.sources[id] {
            Src::Carved { file, at, size } => {
                let mut b = vec![0u8; *size as usize];
                container::read_at(&self.files[*file], *at, &mut b).ok()?;
                Some(Arc::new(b))
            }
            Src::Red { file, entry } => red3::container(&self.files[*file], entry).map(Arc::new),
            Src::Mem(b) => Some(b.clone()),
            Src::Reader(b) => b.get().map(Arc::new),
        }
    }

    /// NVIDIA: tylko potoki, które wnoszą nową parę (shader etapu, root signature).
    fn cover_units(&mut self) -> usize {
        let before = self.jobs.len();
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        self.jobs.retain(|j| {
            let mut new = false;
            for s in j.stages.iter().flatten() {
                new |= seen.insert((*s, j.rs));
            }
            new
        });
        before - self.jobs.len()
    }
}

fn say_line(line: String) {
    let out = std::io::stdout();
    let mut o = out.lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

// --------------------------------- carved ------------------------------------

fn plan_carved(game: &Path, say: &dyn Fn(String)) -> Result<Plan, String> {
    let files = container::candidate_files(game);
    let mut plan = Plan::default();
    let mut index = container::Index::default();
    let mut ids: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut seen: HashSet<[u8; 16]> = HashSet::new();
    let mut sampled = 0u64;
    let total = files.len();
    say(format!("scan 0 {total}"));
    for (n, (path, len)) in files.iter().enumerate() {
        if n % 20 == 0 {
            say(format!("scan {n} {total}"));
        }
        let Ok(file) = std::fs::File::open(path) else { continue };
        // Plik bez nazwy shaderów: najpierw próbki, w całości tylko z trafieniem.
        if !container::named(path) {
            if sampled >= SAMPLE_BUDGET {
                continue;
            }
            let mut hit = false;
            for (start, n) in container::samples(*len, SAMPLE_WINDOW) {
                sampled += n;
                let _ = container::carve_file(&file, start, n, &mut |_, c| {
                    hit |= c.kind().is_some() && (c.root_signature().is_some() || c.is_dxil());
                });
            }
            if !hit {
                continue;
            }
        }
        let slot = plan.files.len();
        let _ = container::carve_file(&file, 0, *len, &mut |at, c| {
            if seen.insert(c.identity()) {
                index.add(slot, at, c, &mut ids);
            }
        });
        plan.files.push(file);
    }
    say(format!("scan {total} {total}"));
    if index.without_rs > 0 {
        say(format!(
            "warning shadery DXIL bez wbudowanej root signature (pominięte - silnik tworzy ją w trakcie gry) / DXIL shaders without an embedded root signature (skipped - the engine builds it at run time): {}",
            index.without_rs
        ));
    }
    if index.shaders.is_empty() {
        return Err(
            "w plikach gry nie ma shaderów DirectX 12 z wbudowaną root signature / the game files hold no DirectX 12 shaders with an embedded root signature"
                .into(),
        );
    }
    say(format!(
        "warning shadery z root signature / shaders with a root signature: {} ({} root signature, pliki / files {})",
        index.shaders.len(),
        index.signatures.len(),
        plan.files.len()
    ));
    for s in &index.shaders {
        plan.sources.push(Src::Carved { file: s.file, at: s.at, size: s.size });
    }
    plan.signatures = index.signatures.clone();
    for p in index.plan() {
        let job = match p {
            container::Pipeline::Compute { cs } => Job { rs: index.rs[cs], stages: [None, None, None, None, None, Some(cs)], ..Default::default() },
            container::Pipeline::Graphics { vs, ps, alternatives } => Job {
                rs: index.rs[vs],
                stages: [Some(vs), ps, None, None, None, None],
                alternatives,
            },
        };
        plan.jobs.push(job);
    }
    Ok(plan)
}

// ---------------------------------- red3 -------------------------------------

#[cfg(windows)]
fn plan_red3(game: &Path, say: &dyn Fn(String)) -> Result<Plan, String> {
    let mut plan = Plan::default();
    for s in [red3::Signature::Compute, red3::Signature::Extended, red3::Signature::Graphics] {
        let (flags, rows) = red3::description(s);
        plan.signatures.push(red3::serialize(flags, &rows)?);
    }
    let sig_id = |s: red3::Signature| match s {
        red3::Signature::Compute => 0,
        red3::Signature::Extended => 1,
        red3::Signature::Graphics => 2,
    };

    // Shadery materiałów i techniki.
    let path = red3::materials_path(game);
    say("scan 0 2".into());
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", red3::MATERIALS))?;
    let m = red3::read_materials(&bytes).ok_or_else(|| {
        format!("{}: nieznany układ pliku / unknown file layout", red3::MATERIALS)
    })?;
    drop(bytes);
    plan.files.push(std::fs::File::open(&path).map_err(|e| e.to_string())?);
    let mut source_of: HashMap<u64, usize> = HashMap::new();
    let mut seen: HashSet<[u64; 6]> = HashSet::new();
    let (mut rejected, mut missing) = (0usize, 0usize);
    for t in &m.techniques {
        let Some(sig) = red3::signature_for(t) else {
            rejected += 1;
            continue;
        };
        // Compute techniki to osobny potok; graficzna część bez niego.
        let mut graphics = *t;
        graphics[5] = 0;
        let mut parts = Vec::new();
        if sig != red3::Signature::Compute {
            parts.push((graphics, sig));
            if t[5] != 0 {
                parts.push(([0, 0, 0, 0, 0, t[5]], red3::Signature::Compute));
            }
        } else {
            parts.push(([0, 0, 0, 0, 0, t[5]], sig));
        }
        for (keys, sig) in parts {
            if !seen.insert(keys) {
                continue;
            }
            if keys.iter().any(|k| *k != 0 && !m.shaders.contains_key(k)) {
                missing += 1;
                continue;
            }
            let mut job = Job { rs: sig_id(sig), ..Default::default() };
            for (slot, key) in keys.iter().enumerate() {
                if *key == 0 {
                    continue;
                }
                let id = *source_of.entry(*key).or_insert_with(|| {
                    plan.sources.push(Src::Red { file: 0, entry: m.shaders[key] });
                    plan.sources.len() - 1
                });
                job.stages[slot] = Some(id);
            }
            plan.jobs.push(job);
        }
    }
    say(format!(
        "warning shadery materiałów / material shaders: {}, techniki / techniques: {}, potoki / pipelines: {}, odrzucone / rejected: {rejected}, bez shadera / missing a shader: {missing}",
        m.shaders.len(),
        m.techniques.len(),
        plan.jobs.len()
    ));

    // Shadery silnika: compute osobno, pary VS+PS po semantykach.
    say("scan 1 2".into());
    let spath = red3::static_path(game);
    if let Ok(sbytes) = std::fs::read(&spath) {
        if let Some(list) = red3::read_static(&sbytes) {
            let file = std::fs::File::open(&spath).map_err(|e| e.to_string())?;
            let mut shaders: Vec<(Kind, Arc<Vec<u8>>)> = Vec::new();
            for e in &list {
                if let Some(c) = red3::container(&file, e) {
                    if let Some(kind) = Container::parse(&c).and_then(|x| x.kind()) {
                        shaders.push((kind, Arc::new(c)));
                    }
                }
            }
            let engine_jobs = plan.jobs.len();
            let id_of = |b: &Arc<Vec<u8>>, plan: &mut Plan| {
                plan.sources.push(Src::Mem(b.clone()));
                plan.sources.len() - 1
            };
            let vs: Vec<(Arc<Vec<u8>>, Vec<(String, u32, u32)>)> = shaders
                .iter()
                .filter(|(k, _)| *k == Kind::Vertex)
                .filter_map(|(_, b)| {
                    let (gives, position) = container::vs_gives(&Container::parse(b)?);
                    position.then(|| (b.clone(), gives))
                })
                .collect();
            let mut vs_ids: HashMap<usize, usize> = HashMap::new();
            let mut paired = vec![false; vs.len()];
            for (kind, b) in &shaders {
                match kind {
                    Kind::Compute => {
                        let id = id_of(b, &mut plan);
                        plan.jobs.push(Job { rs: 0, stages: [None, None, None, None, None, Some(id)], ..Default::default() });
                    }
                    Kind::Pixel => {
                        let Some(c) = Container::parse(b) else { continue };
                        let needs = container::ps_needs(&c);
                        let fits: Vec<usize> = (0..vs.len()).filter(|&v| container::links(&needs, &vs[v].1)).take(4).collect();
                        let Some((&first, rest)) = fits.split_first() else { continue };
                        paired[first] = true;
                        let mut vid = |v: usize, plan: &mut Plan| *vs_ids.entry(v).or_insert_with(|| id_of(&vs[v].0, plan));
                        let vs_id = vid(first, &mut plan);
                        let alternatives = rest.iter().map(|&v| vid(v, &mut plan)).collect();
                        let ps_id = id_of(b, &mut plan);
                        plan.jobs.push(Job { rs: 2, stages: [Some(vs_id), Some(ps_id), None, None, None, None], alternatives });
                    }
                    _ => {}
                }
            }
            for v in 0..vs.len() {
                if !paired[v] {
                    let id = *vs_ids.entry(v).or_insert_with(|| id_of(&vs[v].0, &mut plan));
                    plan.jobs.push(Job { rs: 2, stages: [Some(id), None, None, None, None, None], ..Default::default() });
                }
            }
            say(format!(
                "warning shadery silnika / engine shaders: {}, potoki / pipelines: {}",
                shaders.len(),
                plan.jobs.len() - engine_jobs
            ));
        }
    }
    say("scan 2 2".into());
    if plan.jobs.is_empty() {
        return Err("żadna technika nie tworzy potoku / no technique forms a pipeline".into());
    }
    Ok(plan)
}

// ------------------------------ czytniki silników ----------------------------

/// Plan z czytnika silnika (`readers`): potoki dokładne i pule łączone po
/// semantykach (shadery pul są wczytywane raz, tylko po sygnatury).
fn plan_reader(kind: &str, game: &Path, say: &dyn Fn(String)) -> Result<Plan, String> {
    say("scan 0 1".into());
    let set = super::readers::index12(kind, game, say)?;
    for n in &set.notes {
        say(format!("warning {n}"));
    }
    let mut plan = Plan { signatures: set.signatures, ..Default::default() };
    plan.sources = set.shaders.into_iter().map(Src::Reader).collect();
    let count = plan.sources.len();
    let valid = |i: &Option<usize>| i.is_none_or(|i| i < count);
    for p in set.pipelines {
        if !p.stages.iter().all(valid) || p.rs.is_some_and(|r| r >= plan.signatures.len()) {
            continue;
        }
        plan.jobs.push(Job { rs: p.rs.unwrap_or(EMBEDDED), stages: p.stages, alternatives: Vec::new() });
    }
    let exact = plan.jobs.len();
    for (members, rs) in set.pools {
        if rs.is_some_and(|r| r >= plan.signatures.len()) {
            continue;
        }
        // Shadery wierzchołków z SV_Position: (id, root signature, wyjścia).
        let mut vs: Vec<(usize, Vec<u8>, Vec<(String, u32, u32)>)> = Vec::new();
        let mut ps: Vec<(usize, Vec<u8>, Vec<(String, u32, u32)>)> = Vec::new();
        for &id in members.iter().filter(|&&i| i < count) {
            let Some(bytes) = plan.load(id) else { continue };
            let Some(c) = Container::parse(&bytes) else { continue };
            let embedded = c.root_signature().map(<[u8]>::to_vec).unwrap_or_default();
            if rs.is_none() && embedded.is_empty() {
                continue;
            }
            let job_rs = rs.unwrap_or(EMBEDDED);
            match c.kind() {
                Some(Kind::Compute) => plan.jobs.push(Job { rs: job_rs, stages: [None, None, None, None, None, Some(id)], ..Default::default() }),
                Some(Kind::Vertex) => {
                    let (gives, position) = container::vs_gives(&c);
                    if position {
                        vs.push((id, embedded, gives));
                    }
                }
                Some(Kind::Pixel) => ps.push((id, embedded, container::ps_needs(&c))),
                _ => {}
            }
        }
        let mut paired = vec![false; vs.len()];
        for (p, p_rs, needs) in &ps {
            // Z wbudowaną root signature para musi mieć tę samą.
            let mut fits: Vec<usize> = (0..vs.len())
                .filter(|&v| (rs.is_some() || vs[v].1 == *p_rs) && container::links(needs, &vs[v].2))
                .collect();
            fits.sort_by_key(|v| paired[*v]);
            fits.truncate(4);
            let Some((&first, rest)) = fits.split_first() else { continue };
            paired[first] = true;
            plan.jobs.push(Job {
                rs: rs.unwrap_or(EMBEDDED),
                stages: [Some(vs[first].0), Some(*p), None, None, None, None],
                alternatives: rest.iter().map(|&v| vs[v].0).collect(),
            });
        }
        for (v, (id, ..)) in vs.iter().enumerate() {
            if !paired[v] {
                plan.jobs.push(Job { rs: rs.unwrap_or(EMBEDDED), stages: [Some(*id), None, None, None, None, None], ..Default::default() });
            }
        }
    }
    say(format!(
        "warning shadery / shaders: {count}, potoki z plików gry / pipelines from the game files: {exact}, z pul / from pools: {}",
        plan.jobs.len() - exact
    ));
    say("scan 1 1".into());
    if plan.jobs.is_empty() {
        return Err("czytnik nie znalazł potoków / the reader found no pipelines".into());
    }
    Ok(plan)
}

// --------------------------------- proces ------------------------------------

#[cfg(windows)]
pub fn run_host(args: &[String]) -> i32 {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    use std::sync::Mutex;

    use super::warm::{d3d, GraphicsStages, GraphicsState, CACHED_MS};

    let say = say_line;
    let (Some(kind), Some(game)) = (args.first(), args.get(1).map(PathBuf::from)) else {
        say("fatal użycie: --shader-warm-d3d12x <carved|red3> <folder gry>".into());
        return 2;
    };
    let device = match d3d::gpu_device() {
        Ok(d) => d,
        Err(e) => {
            say(format!("fatal {e}"));
            return 4;
        }
    };
    say(format!("adapter {}", device.adapter));
    let planned = match kind.as_str() {
        "carved" => plan_carved(&game, &say),
        "red3" => plan_red3(&game, &say),
        k if k.starts_with("reader:") => plan_reader(&k["reader:".len()..], &game, &say),
        other => Err(format!("nieznany rodzaj / unknown kind: {other}")),
    };
    let mut plan = match planned {
        Ok(p) => p,
        Err(e) => {
            say(format!("fatal {e}"));
            return 3;
        }
    };
    if device.vendor == super::Vendor::Nvidia {
        let dropped = plan.cover_units();
        if dropped > 0 {
            say(format!(
                "warning NVIDIA: pominięte potoki bez nowego shadera / pipelines with no new shader skipped: {dropped}"
            ));
        }
    }
    if plan.jobs.len() > MAX_PIPELINES {
        say(format!("warning potoki ponad limit / pipelines over the cap: {}", plan.jobs.len() - MAX_PIPELINES));
        plan.jobs.truncate(MAX_PIPELINES);
    }
    let total = plan.jobs.len();
    say(format!("progress 0 {total}"));
    let (done, ok, cached, missing) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let errors = Mutex::new(Vec::<String>::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
    let chunk = total.div_ceil(threads).max(1);
    let plan = &plan;
    std::thread::scope(|scope| {
        for (part_no, part) in plan.jobs.chunks(chunk).enumerate() {
            let (device, done, ok, cached, missing, errors) = (&device, &done, &ok, &cached, &missing, &errors);
            scope.spawn(move || {
                for (k, job) in part.iter().enumerate() {
                    let started = std::time::Instant::now();
                    let result = (|| -> Result<(), String> {
                        let load = |id: Option<usize>| -> Result<Vec<u8>, String> {
                            match id {
                                None => Ok(Vec::new()),
                                Some(i) => plan.load(i).map(|b| b.to_vec()).ok_or_else(|| {
                                    missing.fetch_add(1, Relaxed);
                                    "shader nieczytelny / shader unreadable".to_string()
                                }),
                            }
                        };
                        // Root signature planu albo wbudowana w pierwszy etap, który ją ma.
                        let signature = |codes: &[&[u8]]| {
                            if job.rs != EMBEDDED {
                                return device.root_signature(&plan.signatures[job.rs]);
                            }
                            let blob = codes
                                .iter()
                                .find_map(|c| Container::parse(c).and_then(|x| x.root_signature()))
                                .ok_or("shader bez root signature / shader without a root signature")?;
                            device.root_signature(blob)
                        };
                        if let Some(cs) = job.stages[5] {
                            let cs = load(Some(cs))?;
                            return device.compute(&signature(&[&cs])?, &cs);
                        }
                        let ps = load(job.stages[1])?;
                        let mut stages = GraphicsStages {
                            vs: load(job.stages[0])?,
                            ps,
                            gs: load(job.stages[2])?,
                            hs: load(job.stages[3])?,
                            ds: load(job.stages[4])?,
                        };
                        let rs = signature(&[&stages.vs, &stages.ps, &stages.gs, &stages.hs, &stages.ds])?;
                        let mut state = GraphicsState { depth_format: 40, ..Default::default() }; // D32_FLOAT
                        if let Some(p) = Container::parse(&stages.ps) {
                            state.render_targets = container::ps_targets(&p).0;
                        }
                        let topologies: &[u32] = if !stages.hs.is_empty() {
                            &[4]
                        } else if !stages.gs.is_empty() {
                            &[3, 1, 2]
                        } else {
                            &[3]
                        };
                        let mut last = String::new();
                        for (n, vs_id) in job.stages[0].iter().chain(job.alternatives.iter()).enumerate() {
                            if n > 0 {
                                stages.vs = load(Some(*vs_id))?;
                            }
                            state.inputs = Container::parse(&stages.vs).map(|v| container::vs_layout(&v)).unwrap_or_default();
                            for &t in topologies {
                                state.topology = t;
                                match device.graphics(&rs, &stages, &state) {
                                    Ok(()) => return Ok(()),
                                    Err(e) => last = e,
                                }
                            }
                        }
                        Err(last)
                    })();
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
                                list.push(format!("P{}: {e}", part_no * chunk + k));
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
        say(format!("warning shadery nieczytelne / unreadable shaders: {missing}"));
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

#[cfg(not(windows))]
pub fn run_host(_args: &[String]) -> i32 {
    2
}

#[cfg(all(test, windows))]
mod hardware {
    //! Plan i potoki na prawdziwej grze z tego komputera:
    //! `cargo test warm12_red3 -- --ignored --nocapture`.
    use super::*;

    fn witcher() -> Option<PathBuf> {
        super::super::games::find_all()
            .into_iter()
            .map(|g| g.dir)
            .find(|d| red3::detect(d))
    }

    #[test]
    #[ignore]
    fn warm12_red3_plan() {
        let Some(game) = witcher() else {
            println!("brak gry REDengine 3");
            return;
        };
        let t = std::time::Instant::now();
        let mut plan = plan_red3(&game, &|l| println!("{l}")).unwrap();
        println!("potoki {} w {:?}", plan.jobs.len(), t.elapsed());
        let dropped = plan.cover_units();
        println!("NVIDIA: potoki {} (pominięte {dropped})", plan.jobs.len());
        let device = super::super::warm::test_device().unwrap();
        let (mut ok, mut failed) = (0, 0);
        for job in plan.jobs.iter().take(300) {
            let rs = device.root_signature(&plan.signatures[job.rs]).unwrap();
            let r = if let Some(cs) = job.stages[5] {
                device.compute(&rs, &plan.load(cs).unwrap())
            } else {
                let load = |i: Option<usize>| i.map(|i| plan.load(i).unwrap().to_vec()).unwrap_or_default();
                let stages = super::super::warm::GraphicsStages {
                    vs: load(job.stages[0]),
                    ps: load(job.stages[1]),
                    gs: load(job.stages[2]),
                    hs: load(job.stages[3]),
                    ds: load(job.stages[4]),
                };
                let mut state = super::super::warm::GraphicsState { depth_format: 40, ..Default::default() };
                if let Some(p) = Container::parse(&stages.ps) {
                    state.render_targets = container::ps_targets(&p).0;
                }
                state.inputs = Container::parse(&stages.vs).map(|v| container::vs_layout(&v)).unwrap_or_default();
                if !stages.hs.is_empty() {
                    state.topology = 4;
                }
                device.graphics(&rs, &stages, &state)
            };
            match r {
                Ok(()) => ok += 1,
                Err(e) => {
                    if failed < 5 {
                        println!("błąd: {e}");
                    }
                    failed += 1
                }
            }
        }
        println!("pierwsze 300: ok {ok}, błędy {failed}");
        assert!(ok > failed);
    }
}
