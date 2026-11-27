//! Plan zasilania dla gier: gdy działa gra, program włącza plan wybrany przez
//! użytkownika (np. Ultimate Performance - rdzenie bez parkowania i pełne
//! taktowanie), a po jej zamknięciu wraca na drugi wybrany plan.
//!
//! Wątek co 2 s sprawdza procesy: gra to proces uruchomiony z folderu gry
//! wykrytej w sklepach (jak w Fast Load i kompilacji shaderów). Plan jest
//! zmieniany tylko przy uruchomieniu i zamknięciu gry - ręczna zmiana planu
//! w Windows między grami nie jest cofana.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::i18n::tr;
use crate::icons::Icon;
use crate::theme;

/// Plan zasilania Windows: identyfikator (GUID tekstem) i nazwa.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub id: String,
    pub name: String,
}

/// Krok przełączania planu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// Włączyć plan gry.
    Game,
    /// Wrócić na plan "po grze".
    Idle,
}

/// Co zrobić w tym obiegu. `switched` - plan gry włączył program; `tried` -
/// w tej sesji gry już próbowano go włączyć (jedna próba na uruchomienie gry,
/// więc ręczna zmiana planu w trakcie gry nie jest cofana).
/// Powrót na plan "po grze" następuje, gdy gra się zamknie albo funkcja
/// zostanie wyłączona w trakcie gry.
pub fn decide(enabled: bool, running: bool, switched: bool, tried: bool, has_game_plan: bool) -> Step {
    if switched && (!running || !enabled) {
        Step::Idle
    } else if enabled && running && !switched && !tried && has_game_plan {
        Step::Game
    } else {
        Step::Nothing
    }
}

#[cfg(windows)]
mod sys {
    use super::Plan;
    use crate::tune::power::{self, Guid};

    #[link(name = "powrprof")]
    extern "system" {
        fn PowerEnumerate(
            root: isize,
            scheme: *const Guid,
            sub: *const Guid,
            access: u32,
            index: u32,
            buffer: *mut u8,
            size: *mut u32,
        ) -> u32;
    }
    /// Wyliczanie planów (a nie ustawień w planie).
    const ACCESS_SCHEME: u32 = 16;

    pub fn plans() -> Vec<Plan> {
        let mut out = Vec::new();
        for index in 0..64 {
            let mut guid = Guid::default();
            let mut size = size_of::<Guid>() as u32;
            let code = unsafe {
                PowerEnumerate(0, std::ptr::null(), std::ptr::null(), ACCESS_SCHEME, index, &mut guid as *mut Guid as *mut u8, &mut size)
            };
            if code != 0 {
                break;
            }
            let name = power::name(&guid);
            out.push(Plan { id: guid.text(), name: if name.is_empty() { guid.text() } else { name } });
        }
        out
    }

    pub fn active() -> Option<String> {
        power::active().map(|g| g.text())
    }

    pub fn set_active(id: &str) -> Result<(), String> {
        power::set_active(&Guid::parse(id))
    }
}

#[cfg(not(windows))]
mod sys {
    use super::Plan;
    pub fn plans() -> Vec<Plan> {
        Vec::new()
    }
    pub fn active() -> Option<String> {
        None
    }
    pub fn set_active(_: &str) -> Result<(), String> {
        Err("Windows".into())
    }
}

pub use sys::{active, plans};

/// Ustawienia funkcji (z pliku ustawień programu).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    /// Plan na czas gry i plan po jej zamknięciu (GUID tekstem; "" - brak).
    pub game: String,
    pub idle: String,
    /// W trakcie gry nie pozwalać zasnąć rdzeniowi obsługującemu kartę graficzną.
    pub keep_awake: bool,
    /// W trakcie gry przypisywać jej procesy do rdzeni P.
    pub p_cores: bool,
}

#[derive(Default)]
struct Shared {
    settings: Settings,
    /// Nazwa działającej gry.
    running: Option<String>,
    /// Czy to program włączył plan gry (do przywrócenia przy zamknięciu).
    switched: bool,
    error: Option<String>,
    /// Rdzeń podtrzymywany w trakcie gry i błąd jego wykrycia.
    awake_cpu: Option<u16>,
    awake_error: Option<String>,
    /// Procesy gry przypisane do rdzeni P: (udane, odrzucone).
    p_cores: Option<(usize, usize)>,
    quit: bool,
}

pub struct PowerPlan {
    shared: Arc<Mutex<Shared>>,
    /// Lista planów do interfejsu, odświeżana co kilka sekund.
    plans: Vec<Plan>,
    active: Option<String>,
    read: Option<Instant>,
    dry: bool,
}

/// Nazwa gry, której proces działa: foldery gier (małymi literami) i nazwy.
fn running_game(games: &[(String, String)]) -> Option<(String, Vec<u32>)> {
    let procs = crate::game_watch::process_list();
    let (dir, name) = games.iter().find(|(dir, _)| procs.iter().any(|(_, p)| crate::game_watch::matches(dir, p)))?;
    // Wszystkie procesy z folderu tej gry (launcher, właściwa gra).
    let pids = procs.iter().filter(|(_, p)| crate::game_watch::matches(dir, p)).map(|(pid, _)| *pid).collect();
    Some((name.clone(), pids))
}

/// Przypisanie procesów gry do rdzeni P (zestawy procesorów Windows).
#[cfg(windows)]
mod cores {
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn SetProcessDefaultCpuSets(process: isize, ids: *const u32, count: u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const PROCESS_SET_LIMITED_INFORMATION: u32 = 0x2000;

    /// Ustawia domyślne zestawy procesorów procesu; pusta lista zdejmuje
    /// ograniczenie. `false`, gdy system odmówił dostępu do procesu (np. gra
    /// chroniona przez anticheat).
    pub fn set(pid: u32, ids: &[u32]) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_SET_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return false;
            }
            let ptr = if ids.is_empty() { std::ptr::null() } else { ids.as_ptr() };
            let ok = SetProcessDefaultCpuSets(h, ptr, ids.len() as u32) != 0;
            CloseHandle(h);
            ok
        }
    }
}

impl PowerPlan {
    /// `dry` - tryb demonstracyjny: plan i rdzeń nie są ruszane także po zmianie ustawień.
    pub fn start(settings: Settings, dry: bool, repaint: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Mutex::new(Shared { settings, ..Default::default() }));
        let s = shared.clone();
        crate::spawn("power-plan", move || {
            let mut games: Vec<(String, String)> = Vec::new();
            let mut listed: Option<Instant> = None;
            // Podtrzymywanie rdzenia karty: działający wątek i czy w tej sesji
            // gry już szukano rdzenia (jedna próba na uruchomienie gry).
            #[cfg(windows)]
            let mut awake: Option<crate::dpc::KeepAwake> = None;
            let mut tried = false;
            // W tej sesji gry już próbowano włączyć plan gry.
            let mut plan_tried = false;
            // Procesy gry przypisane do rdzeni P i czy się to udało.
            #[cfg(windows)]
            let mut pinned: std::collections::HashMap<u32, bool> = std::collections::HashMap::new();
            loop {
                let (settings, quit) = match s.lock() {
                    Ok(g) => (g.settings.clone(), g.quit),
                    Err(_) => break,
                };
                if quit {
                    break;
                }
                let found = if settings.enabled || settings.keep_awake || settings.p_cores {
                    // Lista gier odświeżana co 5 minut (nowo zainstalowane gry).
                    if listed.is_none_or(|t| t.elapsed() >= Duration::from_secs(300)) {
                        listed = Some(Instant::now());
                        games = crate::shaders::games::find_all()
                            .into_iter()
                            .map(|g| (g.dir.to_string_lossy().to_lowercase(), g.name))
                            .collect();
                    }
                    running_game(&games)
                } else {
                    None
                };
                let game_pids: Vec<u32> = found.as_ref().map(|f| f.1.clone()).unwrap_or_default();
                let now = found.map(|f| f.0);
                // Rdzenie P: każdy nowy proces gry dostaje zestawy procesorów
                // rdzeni P; po wyłączeniu opcji ograniczenie jest zdejmowane.
                #[cfg(windows)]
                let p_cores_state = {
                    let ids = crate::irq::p_core_set_ids();
                    if settings.p_cores && !ids.is_empty() {
                        for pid in &game_pids {
                            if !pinned.contains_key(pid) {
                                pinned.insert(*pid, cores::set(*pid, ids));
                            }
                        }
                    } else {
                        for (pid, ok) in pinned.drain() {
                            if ok {
                                cores::set(pid, &[]);
                            }
                        }
                    }
                    pinned.retain(|pid, _| game_pids.contains(pid));
                    let ok = pinned.values().filter(|v| **v).count();
                    (!pinned.is_empty()).then_some((ok, pinned.len() - ok))
                };
                #[cfg(not(windows))]
                let p_cores_state: Option<(usize, usize)> = {
                    let _ = &game_pids;
                    None
                };
                // Plan zasilania: przy uruchomieniu gry i po jej zamknięciu (albo
                // po wyłączeniu funkcji w trakcie gry).
                if now.is_none() {
                    plan_tried = false;
                }
                let switched = s.lock().map(|g| g.switched).unwrap_or(false);
                let step = decide(settings.enabled, now.is_some(), switched, plan_tried, !settings.game.is_empty());
                let result = match step {
                    Step::Game => {
                        plan_tried = true;
                        Some(sys::set_active(&settings.game))
                    }
                    // Bez wybranego planu "po grze" zostaje bieżący.
                    Step::Idle if !settings.idle.is_empty() => Some(sys::set_active(&settings.idle)),
                    _ => None,
                };
                // Rdzeń karty: nie zasypia, dopóki działa gra.
                let mut awake_error = None;
                #[cfg(windows)]
                let awake_cpu = {
                    if settings.keep_awake && now.is_some() {
                        if awake.is_none() && !tried {
                            tried = true;
                            match crate::dpc::detect_gpu_cpu() {
                                Ok(cpu) => awake = Some(crate::dpc::KeepAwake::start(cpu)),
                                Err(e) => awake_error = Some(e),
                            }
                        }
                    } else {
                        awake = None;
                        tried = false;
                    }
                    // Rdzeń pokazujemy dopiero, gdy pętla faktycznie na nim działa.
                    awake.as_ref().filter(|a| a.active()).map(|a| a.cpu)
                };
                #[cfg(not(windows))]
                let awake_cpu: Option<u16> = {
                    let _ = &mut tried;
                    None
                };
                if let Ok(mut g) = s.lock() {
                    let changed = g.running != now || g.awake_cpu != awake_cpu || awake_error.is_some() || g.p_cores != p_cores_state;
                    g.p_cores = p_cores_state;
                    if let Some(r) = &result {
                        g.error = r.as_ref().err().cloned();
                    }
                    match step {
                        Step::Game => g.switched = result.as_ref().is_some_and(|r| r.is_ok()),
                        Step::Idle => g.switched = false,
                        Step::Nothing => {}
                    }
                    if awake_error.is_some() || awake_cpu.is_some() || now.is_none() {
                        g.awake_error = awake_error;
                    }
                    g.awake_cpu = awake_cpu;
                    g.running = now;
                    drop(g);
                    if changed {
                        repaint();
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        Self { shared, plans: Vec::new(), active: None, read: None, dry }
    }

    /// Działająca gra, podtrzymywany rdzeń i błąd jego wykrycia.
    pub fn awake(&self) -> (Option<String>, Option<u16>, Option<String>) {
        self.shared.lock().map(|s| (s.running.clone(), s.awake_cpu, s.awake_error.clone())).unwrap_or_default()
    }

    /// Procesy działającej gry przypisane do rdzeni P: (udane, odrzucone
    /// przez system); `None`, gdy nic nie jest przypisane.
    pub fn p_cores(&self) -> Option<(usize, usize)> {
        self.shared.lock().ok().and_then(|s| s.p_cores)
    }

    pub fn set(&self, mut settings: Settings) {
        // Tryb demonstracyjny: nic nie trafia do systemu.
        if self.dry {
            settings.enabled = false;
            settings.keep_awake = false;
            settings.p_cores = false;
        }
        if let Ok(mut s) = self.shared.lock() {
            s.settings = settings;
        }
    }

    /// Strona w zakładce TUNE. Zwraca `true`, gdy ustawienia się zmieniły.
    pub fn ui(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> bool {
        if self.read.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            self.read = Some(Instant::now());
            self.plans = plans();
            self.active = active();
        }
        let before = settings.clone();
        let (running, error) = self.shared.lock().map(|s| (s.running.clone(), s.error.clone())).unwrap_or_default();
        let name_of = |id: &str| self.plans.iter().find(|p| p.id == id).map(|p| p.name.clone());

        theme::heading(ui, Icon::Power, "Power plan");
        theme::info(
            ui,
            tr(
                "Gdy uruchomisz grę, program włącza wybrany plan zasilania Windows, a po jej zamknięciu wraca na drugi wybrany plan. Na czas gry najlepszy jest plan z wyłączonym parkowaniem rdzeni i pełnym taktowaniem, np. Najwyższa wydajność (Ultimate Performance) - można go utworzyć na stronie System. Gra to każdy program uruchomiony z folderu gry wykrytej w Steam, Epic, GOG, Ubisoft, EA, Xbox, Battle.net i Riot Games. Zmianę planu ręcznie w Windows między grami program zostawia w spokoju.",
                "When you start a game, the program turns on the chosen Windows power plan, and when the game closes it returns to the second chosen plan. For gaming the best plan has core parking off and full clock speed, e.g. Ultimate Performance - you can create it on the System page. A game is any program started from the folder of a game detected in Steam, Epic, GOG, Ubisoft, EA, Xbox, Battle.net and Riot Games. A plan you change by hand in Windows between games is left alone.",
            ),
        );
        ui.add_space(6.0);
        ui.checkbox(&mut settings.enabled, tr("Przełączaj plan zasilania dla gier", "Switch the power plan for games"));
        // Rdzenie P: tylko w procesorach hybrydowych.
        let hybrid = !crate::irq::p_core_set_ids().is_empty();
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled(
                hybrid,
                egui::Checkbox::new(&mut settings.p_cores, tr("W trakcie gry przypisz grę do rdzeni P", "While a game runs, assign it to the P cores")),
            )
            .on_hover_text(tr(
                "Procesy uruchomionej gry dostają do dyspozycji tylko rdzenie wydajne (P); rdzenie E zostają dla reszty systemu. Pomaga w grach, których wątki Windows przenosi na rdzenie E - w innych nic nie zmienia albo szkodzi, więc sprawdź na stronie FPS przed i po. Używa zestawów procesorów Windows (CPU Sets); po zamknięciu gry albo wyłączeniu opcji nic nie zostaje. Gra chroniona przez anticheat może odrzucić zmianę - wtedy program to pokaże.",
                "The running game's processes get only the performance (P) cores; the E cores stay for the rest of the system. Helps in games whose threads Windows moves to E cores - in others it changes nothing or hurts, so check on the FPS page before and after. Uses Windows CPU Sets; nothing stays after the game closes or the option is turned off. A game protected by anti-cheat may reject the change - the program shows it then.",
            ))
            .on_disabled_hover_text(tr("Ten procesor nie ma rdzeni E.", "This processor has no E cores."));
            if settings.p_cores {
                match self.p_cores() {
                    Some((ok, 0)) => {
                        ui.label(RichText::new(crate::trf!("przypisane procesy: {ok}", "assigned processes: {ok}")).size(12.5).color(theme::ok()));
                    }
                    Some((ok, refused)) => {
                        ui.label(
                            RichText::new(crate::trf!(
                                "przypisane: {ok}, odrzucone przez system: {refused}",
                                "assigned: {ok}, refused by the system: {refused}"
                            ))
                            .size(12.5)
                            .color(theme::warn()),
                        );
                    }
                    None => {}
                }
            }
        });
        ui.add_space(6.0);

        let plans = self.plans.clone();
        let combo = |ui: &mut egui::Ui, id: &str, label: &str, value: &mut String| {
            ui.label(label);
            let shown = name_of(value).unwrap_or_else(|| tr("(nie wybrano)", "(not chosen)").to_owned());
            egui::ComboBox::from_id_salt(id).width(320.0).selected_text(shown).show_ui(ui, |ui| {
                for p in &plans {
                    ui.selectable_value(value, p.id.clone(), &p.name);
                }
            });
            ui.end_row();
        };
        egui::Grid::new("power_plan_grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            combo(ui, "power_plan_game", tr("Plan podczas gry", "Plan while a game runs"), &mut settings.game);
            combo(ui, "power_plan_idle", tr("Plan po zamknięciu gry", "Plan after the game closes"), &mut settings.idle);
            ui.label(tr("Aktywny teraz", "Active now"));
            let active = self.active.as_deref().and_then(name_of).unwrap_or_else(|| "?".to_owned());
            ui.label(RichText::new(active).strong().color(theme::accent()));
            ui.end_row();
            ui.label(tr("Gra", "Game"));
            match &running {
                Some(name) => ui.label(RichText::new(name).color(theme::ok())),
                None if settings.enabled || settings.p_cores || settings.keep_awake => {
                    ui.label(RichText::new(tr("żadna nie jest uruchomiona", "none is running")).color(theme::muted()))
                }
                None => ui.label(RichText::new(tr("funkcja wyłączona", "feature off")).color(theme::muted())),
            };
            ui.end_row();
        });
        if settings.enabled && (settings.game.is_empty() || settings.idle.is_empty()) {
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr("Wybierz oba plany - bez nich program niczego nie przełącza.", "Choose both plans - without them the program switches nothing."))
                    .color(theme::warn()),
            );
        }
        if let Some(e) = error {
            ui.label(RichText::new(e).color(theme::warn()));
        }

        let changed = *settings != before;
        if changed {
            self.set(settings.clone());
        }
        changed
    }
}

impl Drop for PowerPlan {
    fn drop(&mut self) {
        // Program zamykany w trakcie gry: wracamy na plan "po grze".
        if let Ok(mut s) = self.shared.lock() {
            s.quit = true;
            if s.switched && !s.settings.idle.is_empty() {
                let _ = sys::set_active(&s.settings.idle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_steps() {
        // Gra ruszyła: plan gry, raz na sesję gry.
        assert_eq!(decide(true, true, false, false, true), Step::Game);
        assert_eq!(decide(true, true, false, true, true), Step::Nothing);
        // Plan gry działa: nic do zrobienia (ręczna zmiana planu zostaje).
        assert_eq!(decide(true, true, true, true, true), Step::Nothing);
        // Gra się zamknęła albo funkcję wyłączono w trakcie gry: powrót.
        assert_eq!(decide(true, false, true, true, true), Step::Idle);
        assert_eq!(decide(false, true, true, true, true), Step::Idle);
        // Bez gry, wyłączone albo bez wybranego planu gry: nic.
        assert_eq!(decide(true, false, false, false, true), Step::Nothing);
        assert_eq!(decide(false, true, false, false, true), Step::Nothing);
        assert_eq!(decide(true, true, false, false, false), Step::Nothing);
    }

    /// Przełączenie planu i powrót (zmienia aktywny plan na ułamek sekundy).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn power_plan_switch_live() {
        let before = active().expect("active plan");
        let other = plans().into_iter().find(|p| p.id != before).expect("second plan");
        sys::set_active(&other.id).unwrap();
        let during = active();
        sys::set_active(&before).unwrap();
        assert_eq!(during.as_deref(), Some(other.id.as_str()));
        assert_eq!(active().as_deref(), Some(before.as_str()));
    }

    /// Plany tego komputera: `cargo test power_plans_live -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn power_plans_live() {
        let list = plans();
        let active = active();
        for p in &list {
            eprintln!("{} {} {}", if Some(&p.id) == active.as_ref() { "*" } else { " " }, p.id, p.name);
        }
        assert!(!list.is_empty());
        assert!(list.iter().any(|p| Some(&p.id) == active.as_ref()));
    }
}
