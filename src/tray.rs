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

    // ------------------------- ikona w zasobniku ---------------------------
    //
    // Ikona żyje w osobnym wątku z ukrytym oknem i własną pętlą komunikatów:
    // `Shell_NotifyIconW` wysyła do tego okna kliknięcia, a menu otwiera się
    // przez `TrackPopupMenu`. Wątek interfejsu (egui) tylko włącza i wyłącza
    // ikonę komunikatem do tego okna.

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND as Hwnd, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
        DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
        PostMessageW as Post, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
        SetForegroundWindow as Foreground, TrackPopupMenu, TranslateMessage, HICON, ICONINFO,
        MF_STRING, MSG, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_APP, WM_DESTROY,
        WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_POPUP,
    };

    /// Kliknięcia ikony (uCallbackMessage).
    const WM_TRAY: u32 = WM_APP + 1;
    /// Włącz (wParam = 1) / wyłącz (0) ikonę - z wątku interfejsu.
    const WM_TRAY_SET: u32 = WM_APP + 2;
    const ID_SHOW: usize = 1;
    const ID_QUIT: usize = 2;
    /// Rozmiar ikony w zasobniku (system skaluje do swojego).
    const ICON_SIZE: u32 = 32;

    /// Stan wątku ikony: uchwyt ikony i czy ma być widoczna.
    struct State {
        icon: HICON,
        visible: bool,
        shown: bool,
        /// "TaskbarCreated" - pasek zadań utworzony od nowa (restart Eksploratora).
        taskbar_created: u32,
    }

    thread_local! {
        static STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) };
    }

    fn notify_data(hwnd: Hwnd, icon: HICON) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: icon,
            ..Default::default()
        };
        for (dst, src) in d.szTip.iter_mut().zip(WINDOW_TITLE.encode_utf16()) {
            *dst = src;
        }
        d
    }

    /// Dodaje albo usuwa ikonę zgodnie ze stanem.
    fn sync_icon(hwnd: Hwnd) {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let Some(s) = s.as_mut() else { return };
            let d = notify_data(hwnd, s.icon);
            // SAFETY: struktura z rozmiarem; okno żyje.
            unsafe {
                if s.visible && !s.shown {
                    s.shown = Shell_NotifyIconW(NIM_ADD, &d).as_bool();
                } else if !s.visible && s.shown {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &d);
                    s.shown = false;
                }
            }
        });
    }

    /// Menu pod prawym przyciskiem: Pokaż / Zakończ.
    unsafe fn popup_menu(hwnd: Hwnd) {
        let Ok(menu) = CreatePopupMenu() else { return };
        let show: Vec<u16> = wide(crate::i18n::tr("Pokaż Tune Center", "Show Tune Center"));
        let quit: Vec<u16> = wide(crate::i18n::tr("Zakończ", "Quit"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SHOW, PCWSTR(show.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, PCWSTR(quit.as_ptr()));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Bez tego menu nie znika po kliknięciu obok (zalecenie dokumentacji).
        let _ = Foreground(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = Post(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            ID_SHOW => show_window(),
            ID_QUIT => quit_app(),
            _ => {}
        }
    }

    unsafe extern "system" fn wndproc(hwnd: Hwnd, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP => show_window(),
                    WM_RBUTTONUP => popup_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TRAY_SET => {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.visible = wparam.0 != 0;
                    }
                });
                sync_icon(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.visible = false;
                    }
                });
                sync_icon(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.taskbar_created == m)) => {
                // Nowy pasek zadań nie zna naszej ikony - dodajemy ją ponownie.
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.shown = false;
                    }
                });
                sync_icon(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Obraz RGBA (dowolny kwadrat) zmniejszony do `ICON_SIZE` średnią z pól.
    fn scaled(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
        let n = ICON_SIZE;
        let mut out = vec![0u8; (n * n * 4) as usize];
        for y in 0..n {
            for x in 0..n {
                let (x0, x1) = (x * w / n, ((x + 1) * w / n).max(x * w / n + 1));
                let (y0, y1) = (y * h / n, ((y + 1) * h / n).max(y * h / n + 1));
                let mut sum = [0u32; 4];
                let mut count = 0;
                for sy in y0..y1.min(h) {
                    for sx in x0..x1.min(w) {
                        let i = ((sy * w + sx) * 4) as usize;
                        for c in 0..4 {
                            sum[c] += rgba[i + c] as u32;
                        }
                        count += 1;
                    }
                }
                let o = ((y * n + x) * 4) as usize;
                for c in 0..4 {
                    out[o + c] = (sum[c] / count.max(1)) as u8;
                }
            }
        }
        out
    }

    /// Ikona Windows z obrazu RGBA (32 bity z przezroczystością).
    unsafe fn make_icon(rgba: &[u8], w: u32, h: u32) -> Result<HICON, String> {
        let px = scaled(rgba, w, h);
        let n = ICON_SIZE as i32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n,
                // Ujemna wysokość: wiersze od góry.
                biHeight: -n,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| format!("CreateDIBSection: {e}"))?;
        // RGBA -> BGRA.
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, px.len());
        for (d, s) in dst.chunks_exact_mut(4).zip(px.chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
        let mask = CreateBitmap(n, n, 1, 1, None);
        let ii = ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        };
        let icon = CreateIconIndirect(&ii).map_err(|e| format!("CreateIconIndirect: {e}"));
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }

    /// Wątek ikony: okno, ikona i pętla komunikatów. Zwraca uchwyt okna przez
    /// kanał (albo błąd).
    fn tray_thread(rgba: Vec<u8>, w: u32, h: u32, visible: bool, ready: std::sync::mpsc::Sender<Result<isize, String>>) {
        // SAFETY: okno i ikona tworzone i niszczone w tym wątku.
        unsafe {
            let class = wide("TuneCenter.Tray");
            let Ok(module) = GetModuleHandleW(None) else {
                let _ = ready.send(Err("GetModuleHandleW".into()));
                return;
            };
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: module.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(class.as_ptr()),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module.into()),
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = ready.send(Err(format!("CreateWindowExW: {e}")));
                    return;
                }
            };
            let icon = match make_icon(&rgba, w, h) {
                Ok(i) => i,
                Err(e) => {
                    let _ = DestroyWindow(hwnd);
                    let _ = ready.send(Err(e));
                    return;
                }
            };
            let created = wide("TaskbarCreated");
            STATE.with(|s| {
                *s.borrow_mut() = Some(State {
                    icon,
                    visible,
                    shown: false,
                    taskbar_created: RegisterWindowMessageW(PCWSTR(created.as_ptr())),
                })
            });
            sync_icon(hwnd);
            let _ = ready.send(Ok(hwnd.0 as isize));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = DestroyIcon(icon);
        }
    }

    pub struct Tray {
        /// Ukryte okno wątku ikony.
        window: isize,
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            // Usunięcie ikony z zasobnika i koniec wątku (WM_DESTROY).
            unsafe {
                PostMessageW(self.window, WM_CLOSE, 0, 0);
            }
        }
    }

    impl Tray {
        pub fn new(enabled: bool, rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, String> {
            if rgba.len() < (width * height * 4) as usize || width == 0 || height == 0 {
                return Err("zły obraz ikony".into());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("tray".into())
                .spawn(move || tray_thread(rgba, width, height, enabled, tx))
                .map_err(|e| e.to_string())?;
            let window = rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "zasobnik: brak odpowiedzi wątku ikony".to_string())??;
            ENABLED.store(enabled, Ordering::Relaxed);
            std::thread::spawn(watch);
            Ok(Self { window })
        }

        #[cfg(test)]
        pub fn window_handle(&self) -> isize {
            self.window
        }

        /// Włącza lub wyłącza minimalizowanie do zasobnika (i samą ikonę).
        pub fn set_enabled(&self, enabled: bool) {
            ENABLED.store(enabled, Ordering::Relaxed);
            unsafe {
                PostMessageW(self.window, WM_TRAY_SET, enabled as usize, 0);
            }
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

#[cfg(all(test, windows))]
mod tests {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_MODIFY, NOTIFYICONDATAW};

    /// Czy powłoka zna ikonę okna `window` (NIM_MODIFY działa tylko dla istniejącej).
    fn icon_exists(window: isize) -> bool {
        let d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: HWND(window as *mut _),
            uID: 1,
            ..Default::default()
        };
        // SAFETY: struktura z rozmiarem.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d).as_bool() }
    }

    /// Ikona w zasobniku tego komputera: `cargo test tray_icon_shows -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn tray_icon_shows() {
        let rgba: Vec<u8> = (0..64 * 64).flat_map(|i| [40, 180, (i % 255) as u8, 255]).collect();
        let tray = super::Tray::new(true, rgba, 64, 64).expect("ikona");
        let window = tray.window_handle();
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("widoczna: {}", icon_exists(window));
        assert!(icon_exists(window));
        tray.set_enabled(false);
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("po wyłączeniu: {}", icon_exists(window));
        assert!(!icon_exists(window));
        tray.set_enabled(true);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(icon_exists(window));
        drop(tray);
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("po zamknięciu: {}", icon_exists(window));
        assert!(!icon_exists(window));
    }
}#[cfg(windows)]
pub use imp::{activate_other_instance, tray_host_present};
