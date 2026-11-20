//! Minimalizowanie do zasobnika systemowego (obszar ikon przy zegarze).
//!
//! Okno zminimalizowane przyciskiem "_" jest ukrywane, a w zasobniku zostaje
//! ikona: lewy klik przywraca okno, prawy otwiera menu (Pokaż / Zakończ).
//!
//! Ukrywanie i przywracanie odbywa się bezpośrednio funkcjami Windows, a nie
//! przez egui, bo ukryte okno nie jest odświeżane i samo by się nie obudziło.
//! Sterowanie wentylatorami i licznik energii działają w osobnym wątku, więc
//! ukrycie okna ich nie zatrzymuje.

/// Tytuł głównego okna; po nim wyszukujemy okno do ukrycia i przywrócenia.
pub const WINDOW_TITLE: &str = "Tune Center";

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::time::Duration;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::WINDOW_TITLE;

    static ENABLED: AtomicBool = AtomicBool::new(false);
    static QUITTING: AtomicBool = AtomicBool::new(false);
    /// Uchwyt głównego okna (0 = jeszcze nie znaleziono).
    static HWND: AtomicIsize = AtomicIsize::new(0);

    const SW_HIDE: i32 = 0;
    const SW_RESTORE: i32 = 9;
    const WM_CLOSE: u32 = 0x0010;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(
            callback: unsafe extern "system" fn(isize, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(hwnd: isize, process_id: *mut u32) -> u32;
        fn GetWindowTextW(hwnd: isize, text: *mut u16, max_count: i32) -> i32;
        fn IsWindow(hwnd: isize) -> i32;
        fn IsIconic(hwnd: isize) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn CreateMutexW(attributes: *const u8, initial_owner: i32, name: *const u16) -> isize;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: isize) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    }

    const SW_SHOW: i32 = 5;
    const ERROR_ALREADY_EXISTS: u32 = 183;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Czy działa obszar powiadomień (zasobnik) powłoki. Explorer i powłoki,
    /// które go zastępują, tworzą okno klasy `Shell_TrayWnd` - do niego trafiają
    /// ikony z `Shell_NotifyIcon`. Bez niego ikony nie widać, więc okna nie
    /// chowamy (inaczej nie dałoby się go przywrócić).
    pub fn tray_host_present() -> bool {
        let class = wide("Shell_TrayWnd");
        unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) != 0 }
    }

    /// Szuka okna programu w innym procesie (inna kopia Tune Center).
    unsafe extern "system" fn find_other_callback(hwnd: isize, lparam: isize) -> i32 {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != GetCurrentProcessId() {
            let mut buf = [0u16; 128];
            let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
            if String::from_utf16_lossy(&buf[..len.min(buf.len())]) == WINDOW_TITLE {
                *(lparam as *mut isize) = hwnd;
                return 0;
            }
        }
        1
    }

    /// Jedna kopia programu: gdy z tego samego folderu już działa Tune Center,
    /// pokazuje jego okno (także schowane do zasobnika) i zwraca `true` - wtedy
    /// ta kopia ma się zakończyć. Dzięki temu ponowne uruchomienie programu
    /// przywraca okno nawet wtedy, gdy powłoka nie pokazuje ikony w zasobniku.
    /// `show`: `false` przy starcie z autostartu (`--minimized`) - wtedy nie
    /// wyciągamy okna, tylko kończymy.
    /// `wait`: kopia uruchomiona przez "Uruchom jako administrator" czeka do
    /// 10 s, aż poprzednia się zamknie, zamiast ją pokazywać.
    pub fn activate_other_instance(show: bool, wait: bool) -> bool {
        // Nazwa zależy od folderu programu, żeby kopia robocza i zainstalowana
        // nie blokowały się nawzajem.
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.display().to_string()))
            .unwrap_or_default()
            .to_lowercase()
            .replace(['\\', ':'], "/");
        let name = wide(&format!("Local\\TuneCenter.Instance.{dir}"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            // Uchwyt zostaje otwarty do końca procesu (celowo nie zamykamy).
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if handle == 0 || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
                return false;
            }
            if !wait || std::time::Instant::now() > deadline {
                break;
            }
            unsafe { CloseHandle(handle) };
            std::thread::sleep(Duration::from_millis(200));
        }
        if wait {
            // Poprzednia kopia nie zamknęła się w 10 s - nie startujemy drugiej.
            return true;
        }
        if show {
            let mut found: isize = 0;
            unsafe {
                EnumWindows(find_other_callback, &mut found as *mut isize as isize);
                if found != 0 {
                    ShowWindow(found, SW_SHOW);
                    ShowWindow(found, SW_RESTORE);
                    SetForegroundWindow(found);
                }
            }
        }
        true
    }

    /// Szuka okna naszego procesu o tytule programu.
    unsafe extern "system" fn find_callback(hwnd: isize, lparam: isize) -> i32 {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == GetCurrentProcessId() {
            let mut buf = [0u16; 128];
            let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
            if String::from_utf16_lossy(&buf[..len.min(buf.len())]) == WINDOW_TITLE {
                *(lparam as *mut isize) = hwnd;
                return 0; // znalezione - koniec wyliczania
            }
        }
        1
    }

    fn main_window() -> Option<isize> {
        let cached = HWND.load(Ordering::Relaxed);
        if cached != 0 && unsafe { IsWindow(cached) } != 0 {
            return Some(cached);
        }
        let mut found: isize = 0;
        unsafe {
            EnumWindows(find_callback, &mut found as *mut isize as isize);
        }
        HWND.store(found, Ordering::Relaxed);
        (found != 0).then_some(found)
    }

    fn show_window() {
        if let Some(hwnd) = main_window() {
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            }
        }
    }

    /// Zamyka program tak samo jak przycisk "X": okno musi być widoczne, żeby
    /// zamknięcie przeszło zwykłą drogą i wentylatory wróciły pod BIOS.
    fn quit_app() {
        QUITTING.store(true, Ordering::Relaxed);
        if let Some(hwnd) = main_window() {
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
        }
    }

    /// Co chwilę sprawdza, czy okno zostało zminimalizowane, i wtedy je ukrywa.
    fn watch() {
        loop {
            std::thread::sleep(Duration::from_millis(150));
            if !ENABLED.load(Ordering::Relaxed) || QUITTING.load(Ordering::Relaxed) {
                continue;
            }
            // Bez zasobnika w powłoce okno zostaje zwyczajnie zminimalizowane.
            if !tray_host_present() {
                continue;
            }
            if let Some(hwnd) = main_window() {
                unsafe {
                    if IsIconic(hwnd) != 0 && IsWindowVisible(hwnd) != 0 {
                        ShowWindow(hwnd, SW_HIDE);
                    }
                }
            }
        }
    }

    pub struct Tray {
        icon: TrayIcon,
    }

    impl Tray {
        /// Musi być wywołane w głównym wątku (tam, gdzie działa pętla okna).
        pub fn new(enabled: bool, rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, String> {
            let image = Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())?;

            let show = MenuItem::new(
                crate::i18n::tr("Pokaż Tune Center", "Show Tune Center"),
                true,
                None,
            );
            let quit = MenuItem::new(crate::i18n::tr("Zakończ", "Quit"), true, None);
            let menu = Menu::new();
            menu.append(&show).map_err(|e| e.to_string())?;
            menu.append(&quit).map_err(|e| e.to_string())?;
            let show_id = show.id().clone();
            let quit_id = quit.id().clone();

            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if event.id == show_id {
                    show_window();
                } else if event.id == quit_id {
                    quit_app();
                }
            }));
            TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    show_window();
                }
            }));

            let icon = TrayIconBuilder::new()
                .with_tooltip(WINDOW_TITLE)
                .with_icon(image)
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()
                .map_err(|e| e.to_string())?;

            let tray = Self { icon };
            tray.set_enabled(enabled);
            std::thread::spawn(watch);
            Ok(tray)
        }

        /// Włącza lub wyłącza minimalizowanie do zasobnika (i samą ikonę).
        pub fn set_enabled(&self, enabled: bool) {
            ENABLED.store(enabled, Ordering::Relaxed);
            let _ = self.icon.set_visible(enabled);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// Poza Windows zasobnik nie jest obsługiwany; okno minimalizuje się zwyczajnie.
    pub struct Tray;

    impl Tray {
        pub fn new(
            _enabled: bool,
            _rgba: Vec<u8>,
            _width: u32,
            _height: u32,
        ) -> Result<Self, String> {
            Err(crate::i18n::tr(
                "Zasobnik systemowy jest obsługiwany tylko w Windows.",
                "The system tray is supported only on Windows.",
            )
            .to_string())
        }

        pub fn set_enabled(&self, _enabled: bool) {}
    }
}

pub use imp::Tray;
#[cfg(windows)]
pub use imp::{activate_other_instance, tray_host_present};
