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

/// Co zrobić po zmianie stanu gry: `Some(plan)` - włączyć ten plan.
/// Zmiana tylko na zboczu (gra ruszyła / gra się zamknęła) i tylko gdy
/// funkcja jest włączona, a plan wybrany.
pub fn decide(enabled: bool, was_running: bool, running: bool, game_plan: &str, idle_plan: &str) -> Option<String> {
    if !enabled || was_running == running {
        return None;
    }
    let plan = if running { game_plan } else { idle_plan };
    (!plan.is_empty()).then(|| plan.to_owned())
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
}

#[derive(Default)]
struct Shared {
    settings: Settings,
    /// Nazwa działającej gry.
    running: Option<String>,
    /// Czy to program włączył plan gry (do przywrócenia przy zamknięciu).
    switched: bool,
    error: Option<String>,
    quit: bool,
}

pub struct PowerPlan {
    shared: Arc<Mutex<Shared>>,
    /// Lista planów do interfejsu, odświeżana co kilka sekund.
    plans: Vec<Plan>,
    active: Option<String>,
    read: Option<Instant>,
}

/// Nazwa gry, której proces działa: foldery gier (małymi literami) i nazwy.
fn running_game(games: &[(String, String)]) -> Option<String> {
    let procs = crate::game_watch::process_paths();
    games.iter().find(|(dir, _)| procs.iter().any(|p| crate::game_watch::matches(dir, p))).map(|(_, n)| n.clone())
}

impl PowerPlan {
    pub fn start(settings: Settings, repaint: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Mutex::new(Shared { settings, ..Default::default() }));
        let s = shared.clone();
        crate::spawn("power-plan", move || {
            let mut games: Vec<(String, String)> = Vec::new();
            let mut listed: Option<Instant> = None;
            loop {
                let (settings, was, quit) = match s.lock() {
                    Ok(g) => (g.settings.clone(), g.running.is_some(), g.quit),
                    Err(_) => break,
                };
                if quit {
                    break;
                }
                if settings.enabled {
                    // Lista gier odświeżana co 5 minut (nowo zainstalowane gry).
                    if listed.is_none_or(|t| t.elapsed() >= Duration::from_secs(300)) {
                        listed = Some(Instant::now());
                        games = crate::shaders::games::find_all()
                            .into_iter()
                            .map(|g| (g.dir.to_string_lossy().to_lowercase(), g.name))
                            .collect();
                    }
                    let now = running_game(&games);
                    let action = decide(true, was, now.is_some(), &settings.game, &settings.idle);
                    let result = action.as_deref().map(sys::set_active);
                    if let Ok(mut g) = s.lock() {
                        if was != now.is_some() || g.running != now {
                            if let Some(r) = &result {
                                g.error = r.as_ref().err().cloned();
                                g.switched = now.is_some() && r.is_ok();
                            }
                            g.running = now;
                            drop(g);
                            repaint();
                        }
                    }
                } else if was {
                    // Funkcja wyłączona w trakcie gry: stan gry jest zapominany.
                    if let Ok(mut g) = s.lock() {
                        g.running = None;
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        Self { shared, plans: Vec::new(), active: None, read: None }
    }

    pub fn set(&self, settings: Settings) {
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
                None if settings.enabled => {
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
    fn switches_only_on_edges() {
        // Gra ruszyła / zamknęła się.
        assert_eq!(decide(true, false, true, "G", "I"), Some("G".into()));
        assert_eq!(decide(true, true, false, "G", "I"), Some("I".into()));
        // Bez zmiany stanu nic nie robimy (ręczna zmiana planu zostaje).
        assert_eq!(decide(true, true, true, "G", "I"), None);
        assert_eq!(decide(true, false, false, "G", "I"), None);
        // Wyłączone albo plan nie wybrany.
        assert_eq!(decide(false, false, true, "G", "I"), None);
        assert_eq!(decide(true, false, true, "", "I"), None);
        assert_eq!(decide(true, true, false, "G", ""), None);
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
