//! Profile wentylatorów dla gier: wątek co 2 s sprawdza uruchomione procesy
//! i zgłasza grę, której folder (albo plik .exe) ma przypisany profil.
//!
//! Gra jest rozpoznawana po ścieżce procesu: każdy proces uruchomiony z
//! folderu gry (wykrytego w Steam/Epic/GOG/Ubisoft/EA/Xbox) albo dokładnie
//! wskazany plik .exe (gry dodane ręcznie). Ścieżki procesów: Toolhelp32 +
//! `QueryFullProcessImageNameW` (bez uprawnień administratora działa dla
//! procesów użytkownika, czyli gier).

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Reguła: ścieżka (folder gry albo plik .exe, małymi literami), nazwa gry, profil.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub path: String,
    pub name: String,
    pub profile: String,
}

/// Uruchomiona gra z przypisanym profilem.
#[derive(Clone, Debug, PartialEq)]
pub struct Active {
    pub name: String,
    pub profile: String,
}

/// Czy ścieżka procesu należy do reguły (folder: proces w środku; .exe: ten plik).
pub fn matches(rule_path: &str, process: &str) -> bool {
    let p = process.to_lowercase().replace('/', "\\");
    let r = rule_path.to_lowercase().replace('/', "\\");
    if r.ends_with(".exe") {
        p == r
    } else {
        let r = r.trim_end_matches('\\');
        p.starts_with(&format!("{r}\\"))
    }
}

/// Pierwsza reguła pasująca do któregoś z procesów; przy kilku grach wygrywa
/// ta z najnowszym procesem (procesy są podane w kolejności tworzenia).
pub fn find(rules: &[Rule], processes: &[String]) -> Option<Active> {
    processes.iter().rev().find_map(|p| {
        rules.iter().find(|r| matches(&r.path, p)).map(|r| Active {
            name: r.name.clone(),
            profile: r.profile.clone(),
        })
    })
}

/// Pełne ścieżki uruchomionych procesów (w kolejności z Toolhelp32).
#[cfg(windows)]
pub fn process_list() -> Vec<(u32, String)> {
    #[repr(C)]
    struct Entry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe: [u16; 260],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Process32FirstW(snap: isize, entry: *mut Entry) -> i32;
        fn Process32NextW(snap: isize, entry: *mut Entry) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn QueryFullProcessImageNameW(
            process: isize,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == -1 || snap == 0 {
            return out;
        }
        let mut e: Entry = std::mem::zeroed();
        e.size = std::mem::size_of::<Entry>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            if e.pid > 4 {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, e.pid);
                if h != 0 {
                    let mut buf = [0u16; 1024];
                    let mut len = buf.len() as u32;
                    if QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0 {
                        out.push((e.pid, String::from_utf16_lossy(&buf[..len as usize])));
                    }
                    CloseHandle(h);
                }
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    out
}

/// Pełne ścieżki uruchomionych procesów (w kolejności z Toolhelp32).
#[cfg(windows)]
pub fn process_paths() -> Vec<String> {
    process_list().into_iter().map(|(_, path)| path).collect()
}

#[cfg(not(windows))]
pub fn process_list() -> Vec<(u32, String)> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn process_paths() -> Vec<String> {
    Vec::new()
}

/// Wątek obserwujący procesy.
pub struct Watcher {
    rules: Arc<Mutex<Vec<Rule>>>,
    active: Arc<Mutex<Option<Active>>>,
}

impl Watcher {
    pub fn start(rules: Vec<Rule>, wake: impl Fn() + Send + 'static) -> Self {
        let rules = Arc::new(Mutex::new(rules));
        let active = Arc::new(Mutex::new(None));
        let (r, a) = (rules.clone(), active.clone());
        crate::spawn("game-watch", move || loop {
            let current = {
                let rules = r.lock().map(|r| r.clone()).unwrap_or_default();
                if rules.is_empty() {
                    None
                } else {
                    find(&rules, &process_paths())
                }
            };
            let changed = {
                let mut a = a.lock().unwrap();
                let changed = *a != current;
                *a = current;
                changed
            };
            if changed {
                wake();
            }
            std::thread::sleep(Duration::from_secs(2));
        });
        Self { rules, active }
    }

    pub fn set_rules(&self, rules: Vec<Rule>) {
        if let Ok(mut r) = self.rules.lock() {
            *r = rules;
        }
    }

    pub fn active(&self) -> Option<Active> {
        self.active.lock().ok().and_then(|a| a.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{find, matches, Rule};

    #[test]
    fn folder_and_exe_rules() {
        assert!(matches(
            r"C:\Games\Last Epoch",
            r"C:\Games\Last Epoch\Last Epoch.exe"
        ));
        assert!(matches(
            r"c:\games\last epoch\",
            r"C:\Games\Last Epoch\sub\x.exe"
        ));
        assert!(!matches(
            r"C:\Games\Last Epoch",
            r"C:\Games\Last Epoch 2\x.exe"
        ));
        assert!(matches(r"D:\x\game.exe", r"d:\X\Game.exe"));
        assert!(!matches(r"D:\x\game.exe", r"D:\x\other.exe"));
        let rules = vec![
            Rule {
                path: r"C:\A".into(),
                name: "A".into(),
                profile: "Gaming".into(),
            },
            Rule {
                path: r"C:\B".into(),
                name: "B".into(),
                profile: "Cichy".into(),
            },
        ];
        let procs = vec![
            r"C:\A\a.exe".to_owned(),
            r"C:\Windows\x.exe".to_owned(),
            r"C:\B\b.exe".to_owned(),
        ];
        // Najnowszy proces (ostatni) wygrywa.
        assert_eq!(find(&rules, &procs).unwrap().name, "B");
        assert!(find(&rules, &procs[1..2]).is_none());
    }
}
