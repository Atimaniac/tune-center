//! Ustawienia sterownika (DRS - Driver Settings), czyli to, co panel Nvidia
//! pokazuje w "Zarządzaj ustawieniami 3D": profil globalny, profile programów
//! i ich ustawienia.
//!
//! Zmiany zapisane przez `NvAPI_DRS_SaveSettings` są trwałe (przeżywają
//! restart), dlatego przed pierwszym zapisem w danym uruchomieniu programu
//! robimy kopię całej bazy ustawień (`NvAPI_DRS_SaveSettingsToFile`), a każdy
//! zapis sprawdzamy ponownym odczytem.

use super::api::{self, check, need, Api, Handle, WString};
use std::ffi::c_void;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::Arc;

pub const TYPE_DWORD: u32 = 0;

/// Skąd pochodzi bieżąca wartość ustawienia (`NVDRS_SETTING_LOCATION`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// Ustawiona w tym profilu.
    Current,
    /// Odziedziczona z profilu globalnego.
    Global,
    /// Z profilu bazowego.
    Base,
    /// Wartość domyślna sterownika.
    Default,
}

impl Location {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Current,
            1 => Self::Global,
            2 => Self::Base,
            _ => Self::Default,
        }
    }
}

// ------------------------------- struktury ---------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct Binary {
    length: u32,
    data: [u8; 4096],
}

/// Unia wartości ustawienia (DWORD / binarna / tekst / QWORD / float).
/// W nvapi.h te struktury są w `#pragma pack(push, 4)`, więc QWORD jest tu
/// parą u32 - unia ma wyrównanie 4 i 4100 bajtów, jak w sterowniku.
#[repr(C)]
#[derive(Clone, Copy)]
union Value {
    u32_: u32,
    u64_: [u32; 2],
    f32_: f32,
    binary: Binary,
    text: WString,
}

/// `NVDRS_SETTING_V1`.
#[repr(C)]
struct Setting {
    version: u32,
    name: WString,
    id: u32,
    kind: u32,
    location: u32,
    is_current_predefined: u32,
    is_predefined_valid: u32,
    predefined: Value,
    current: Value,
}

/// `NVDRS_PROFILE_V1`.
#[repr(C)]
struct Profile {
    version: u32,
    name: WString,
    gpu_support: u32,
    is_predefined: u32,
    num_apps: u32,
    num_settings: u32,
}

/// `NVDRS_APPLICATION_V4`.
#[repr(C)]
struct Application {
    version: u32,
    is_predefined: u32,
    app_name: WString,
    friendly_name: WString,
    launcher: WString,
    file_in_folder: WString,
    flags: u32,
    command_line: WString,
}

/// `NVDRS_SETTING_VALUES`.
#[repr(C)]
struct SettingValues {
    version: u32,
    count: u32,
    kind: u32,
    default: Value,
    values: [Value; 100],
}

// Rozmiary z nvapi.h (ustawienia: pack(4), reszta: pack(8)); wchodzą
// w numer wersji struktury, więc sterownik odrzuci każdy inny.
const _: () = assert!(size_of::<Value>() == 4100);
const _: () = assert!(size_of::<Setting>() == 12320);
const _: () = assert!(size_of::<Profile>() == 4116);
const _: () = assert!(size_of::<Application>() == 20492);
const _: () = assert!(size_of::<SettingValues>() == 414_112);

const SETTING_VER: u32 = api::ver::<Setting>(1);
const PROFILE_VER: u32 = api::ver::<Profile>(1);
const APPLICATION_VER: u32 = api::ver::<Application>(4);
const SETTING_VALUES_VER: u32 = api::ver::<SettingValues>(1);

/// Wyzerowana struktura na stercie (niektóre mają ponad 400 KB).
fn zeroed<T>() -> Box<T> {
    // SAFETY: wszystkie struktury DRS to liczby i tablice - zero jest
    // poprawną wartością.
    unsafe { Box::<T>::new_zeroed().assume_init() }
}

// ------------------------------ wyniki odczytu -----------------------------

/// Ustawienie odczytane z profilu.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    pub id: u32,
    pub name: String,
    pub kind: u32,
    /// Wartość DWORD (dla innych typów: `None`).
    pub value: Option<u32>,
    pub location: Location,
    /// Wartość przewidziana przez Nvidię dla tego profilu, jeśli jest.
    pub predefined: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileInfo {
    pub name: String,
    pub predefined: bool,
    pub apps: u32,
    pub settings: u32,
}

/// Które profile pokazać: globalny albo profil programu o danej nazwie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Global,
    Profile(String),
}

/// Sesja DRS. Uchwyty profili są ważne tylko do następnego `reload`, więc
/// profile wskazujemy nazwą i szukamy ich przy każdej operacji.
pub struct Drs {
    api: Arc<Api>,
    session: Handle,
    backed_up: Option<PathBuf>,
}

impl Drop for Drs {
    fn drop(&mut self) {
        if let Some(f) = self.api.drs_destroy_session {
            // SAFETY: sesja utworzona w `open`.
            unsafe { f(self.session) };
        }
    }
}

impl Drs {
    pub fn open(api: Arc<Api>) -> Result<Self, String> {
        let create = need(api.drs_create_session)?;
        let mut session: Handle = std::ptr::null_mut();
        // SAFETY: wskaźnik na zmienną lokalną.
        check(unsafe { create(&mut session) })?;
        let drs = Self {
            api,
            session,
            backed_up: None,
        };
        drs.reload()?;
        Ok(drs)
    }

    /// Wczytuje bazę ustawień od nowa (np. po zmianach w panelu Nvidia).
    pub fn reload(&self) -> Result<(), String> {
        let load = need(self.api.drs_load)?;
        // SAFETY: sesja jest ważna.
        check(unsafe { load(self.session) })
    }

    fn save(&self) -> Result<(), String> {
        let save = need(self.api.drs_save)?;
        // SAFETY: sesja jest ważna.
        check(unsafe { save(self.session) })
    }

    /// Kopia zapasowa przed pierwszą zmianą w tym uruchomieniu programu.
    fn ensure_backup(&mut self) -> Result<(), String> {
        if self.backed_up.is_some() {
            return Ok(());
        }
        let path = backup_dir()
            .ok_or_else(|| {
                crate::i18n::tr(
                    "brak katalogu na kopię zapasową",
                    "no folder for the backup",
                )
                .to_owned()
            })?
            .join(format!("drs-{}.nvbackup", unix_time()));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let save = need(self.api.drs_save_to_file)?;
        let wide = api::to_wide(&path.to_string_lossy());
        // SAFETY: tekst UTF-16 zakończony zerem.
        check(unsafe { save(self.session, wide.as_ptr()) }).map_err(|e| {
            crate::trf!(
                "nie udało się zrobić kopii zapasowej ({e}) - niczego nie zmieniono",
                "backup failed ({e}) - nothing was changed"
            )
        })?;
        self.backed_up = Some(path);
        Ok(())
    }

    /// Ścieżka kopii zapasowej zrobionej w tym uruchomieniu.
    pub fn backup_path(&self) -> Option<&PathBuf> {
        self.backed_up.as_ref()
    }

    /// Przywraca całą bazę ustawień z pliku kopii.
    pub fn restore_backup(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.ensure_backup()?;
        let load = need(self.api.drs_load_from_file)?;
        let wide = api::to_wide(&path.to_string_lossy());
        // SAFETY: tekst UTF-16 zakończony zerem.
        check(unsafe { load(self.session, wide.as_ptr()) })?;
        self.save()?;
        self.reload()
    }

    // ------------------------------- profile -------------------------------

    fn handle(&self, target: &Target) -> Result<Handle, String> {
        let mut handle: Handle = std::ptr::null_mut();
        match target {
            Target::Global => {
                let f = need(self.api.drs_global_profile)?;
                // SAFETY: sesja ważna, wskaźnik na zmienną lokalną.
                check(unsafe { f(self.session, &mut handle) })?;
            }
            Target::Profile(name) => {
                let f = need(self.api.drs_find_profile)?;
                let wide = api::to_wide(name);
                // SAFETY: jw.
                check(unsafe { f(self.session, wide.as_ptr(), &mut handle) })?;
            }
        }
        Ok(handle)
    }

    fn info_of(&self, handle: Handle) -> Result<ProfileInfo, String> {
        let f = need(self.api.drs_profile_info)?;
        let mut info = zeroed::<Profile>();
        info.version = PROFILE_VER;
        // SAFETY: struktura z poprawną wersją.
        check(unsafe {
            f(
                self.session,
                handle,
                &mut *info as *mut Profile as *mut c_void,
            )
        })?;
        Ok(ProfileInfo {
            name: api::from_wide(&info.name),
            predefined: info.is_predefined != 0,
            apps: info.num_apps,
            settings: info.num_settings,
        })
    }

    #[cfg_attr(not(test), allow(dead_code))] // test sprzętowy w nvcp/mod.rs
    pub fn info(&self, target: &Target) -> Result<ProfileInfo, String> {
        self.info_of(self.handle(target)?)
    }

    /// Wszystkie profile, które mają przypisane programy (tak jak lista
    /// "Wybierz program" w panelu Nvidia), posortowane po nazwie.
    pub fn program_profiles(&self) -> Result<Vec<ProfileInfo>, String> {
        let count = need(self.api.drs_num_profiles)?;
        let enumerate = need(self.api.drs_enum_profiles)?;
        let mut n = 0u32;
        // SAFETY: wskaźnik na zmienną lokalną.
        check(unsafe { count(self.session, &mut n) })?;
        let mut out = Vec::new();
        for index in 0..n {
            let mut handle: Handle = std::ptr::null_mut();
            // SAFETY: indeks w zakresie zgłoszonym przez sterownik.
            if unsafe { enumerate(self.session, index, &mut handle) } != api::OK {
                continue;
            }
            if let Ok(info) = self.info_of(handle) {
                if info.apps > 0 {
                    out.push(info);
                }
            }
        }
        out.sort_by_key(|p| p.name.to_lowercase());
        Ok(out)
    }

    /// Nazwy plików programów przypisanych do profilu.
    pub fn apps(&self, target: &Target) -> Result<Vec<String>, String> {
        let handle = self.handle(target)?;
        let f = need(self.api.drs_enum_apps)?;
        let info = self.info_of(handle)?;
        let capacity = info.apps.clamp(1, 64);
        let mut apps: Vec<Application> = (0..capacity)
            .map(|_| {
                let mut a = *zeroed::<Application>();
                a.version = APPLICATION_VER;
                a
            })
            .collect();
        let mut count = capacity;
        // SAFETY: tablica `count` struktur z poprawną wersją.
        check(unsafe {
            f(
                self.session,
                handle,
                0,
                &mut count,
                apps.as_mut_ptr() as *mut c_void,
            )
        })?;
        Ok(apps
            .iter()
            .take(count as usize)
            .map(|a| api::from_wide(&a.app_name))
            .collect())
    }

    /// Profil, do którego należy program (nazwa pliku, np. "game.exe").
    pub fn profile_of_app(&self, exe: &str) -> Result<Option<String>, String> {
        let f = need(self.api.drs_find_app)?;
        let wide = api::to_wide(&exe.to_lowercase());
        let mut handle: Handle = std::ptr::null_mut();
        let mut app = zeroed::<Application>();
        app.version = APPLICATION_VER;
        // SAFETY: struktura z poprawną wersją, tekst zakończony zerem.
        let status = unsafe {
            f(
                self.session,
                wide.as_ptr(),
                &mut handle,
                &mut *app as *mut Application as *mut c_void,
            )
        };
        match status {
            api::OK => Ok(Some(self.info_of(handle)?.name)),
            api::EXECUTABLE_NOT_FOUND | api::PROFILE_NOT_FOUND => Ok(None),
            s => Err(api::describe(s)),
        }
    }

    /// Tworzy profil użytkownika dla programu i przypisuje do niego plik.
    /// Zwraca nazwę profilu (istniejącego, jeśli program już ma profil).
    pub fn add_program(&mut self, exe: &str) -> Result<String, String> {
        let exe = exe.trim().to_lowercase();
        if exe.is_empty() {
            return Err(crate::i18n::tr("pusta nazwa pliku", "empty file name").to_owned());
        }
        if let Some(existing) = self.profile_of_app(&exe)? {
            return Ok(existing);
        }
        self.ensure_backup()?;
        let create = need(self.api.drs_create_profile)?;
        let add = need(self.api.drs_create_app)?;
        let name = exe.trim_end_matches(".exe").to_owned();
        let name = if self.handle(&Target::Profile(name.clone())).is_ok() {
            format!("{name} ({exe})")
        } else {
            name
        };
        let mut profile = zeroed::<Profile>();
        profile.version = PROFILE_VER;
        profile.name = *api::to_wide(&name);
        let mut handle: Handle = std::ptr::null_mut();
        // SAFETY: struktura z poprawną wersją.
        check(unsafe {
            create(
                self.session,
                &mut *profile as *mut Profile as *mut c_void,
                &mut handle,
            )
        })?;
        let mut app = zeroed::<Application>();
        app.version = APPLICATION_VER;
        app.app_name = *api::to_wide(&exe);
        // SAFETY: jw.
        let status = unsafe {
            add(
                self.session,
                handle,
                &mut *app as *mut Application as *mut c_void,
            )
        };
        if status != api::OK {
            // Nie zostawiamy pustego profilu.
            if let Some(delete) = self.api.drs_delete_profile {
                // SAFETY: uchwyt właśnie utworzonego profilu.
                unsafe { delete(self.session, handle) };
            }
            return Err(api::describe(status));
        }
        self.save()?;
        Ok(name)
    }

    /// Usuwa profil utworzony przez użytkownika (profili Nvidii nie usuwamy).
    pub fn delete_profile(&mut self, name: &str) -> Result<(), String> {
        let target = Target::Profile(name.to_owned());
        let handle = self.handle(&target)?;
        if self.info_of(handle)?.predefined {
            return Err(crate::i18n::tr(
                "profilu przygotowanego przez Nvidię nie można usunąć - można go przywrócić do domyślnych",
                "Nvidia's predefined profiles cannot be deleted - restore defaults instead",
            )
            .to_owned());
        }
        self.ensure_backup()?;
        let f = need(self.api.drs_delete_profile)?;
        // SAFETY: uchwyt profilu z tej sesji.
        check(unsafe { f(self.session, handle) })?;
        self.save()
    }

    // ------------------------------ ustawienia -----------------------------

    fn read_raw(&self, handle: Handle, id: u32) -> Result<Option<Read>, String> {
        let f = need(self.api.drs_get_setting)?;
        let mut s = zeroed::<Setting>();
        s.version = SETTING_VER;
        // SAFETY: struktura z poprawną wersją.
        let status = unsafe {
            f(
                self.session,
                handle,
                id,
                &mut *s as *mut Setting as *mut c_void,
            )
        };
        if status == api::SETTING_NOT_FOUND {
            return Ok(None);
        }
        check(status)?;
        Ok(Some(to_read(&s)))
    }

    /// Ustawienie w profilu; `None`, gdy nie ma go nigdzie (wtedy obowiązuje
    /// wartość domyślna sterownika - patrz `default_value`).
    pub fn get(&self, target: &Target, id: u32) -> Result<Option<Read>, String> {
        self.read_raw(self.handle(target)?, id)
    }

    /// Ustawia kilka wartości naraz (jedna opcja z panelu bywa kilkoma
    /// ustawieniami sterownika), jeden zapis bazy, potem weryfikacja.
    pub fn set_many(&mut self, target: &Target, pairs: &[(u32, u32)]) -> Result<(), String> {
        self.ensure_backup()?;
        let handle = self.handle(target)?;
        let f = need(self.api.drs_set_setting)?;
        for &(id, value) in pairs {
            let mut s = zeroed::<Setting>();
            s.version = SETTING_VER;
            s.id = id;
            s.kind = TYPE_DWORD;
            s.current.u32_ = value;
            // SAFETY: struktura z poprawną wersją.
            check(unsafe { f(self.session, handle, &mut *s as *mut Setting as *mut c_void) })?;
        }
        self.save()?;
        self.reload()?;
        let handle = self.handle(target)?;
        for &(id, value) in pairs {
            let read = self.read_raw(handle, id)?.and_then(|r| r.value);
            if read != Some(value) {
                return Err(crate::trf!(
                    "sterownik nie przyjął wartości 0x{value:08X} dla 0x{id:08X} (odczyt: {read:?})",
                    "the driver rejected value 0x{value:08X} for 0x{id:08X} (read back: {read:?})"
                ));
            }
        }
        Ok(())
    }

    /// Przywraca ustawienie do wartości domyślnej profilu. W profilu programu
    /// oznacza to "użyj ustawienia globalnego".
    pub fn restore(&mut self, target: &Target, ids: &[u32]) -> Result<(), String> {
        self.ensure_backup()?;
        let handle = self.handle(target)?;
        let restore = need(self.api.drs_restore_setting)?;
        let delete = self.api.drs_delete_profile_setting;
        for &id in ids {
            // SAFETY: uchwyt profilu z tej sesji.
            let status = unsafe { restore(self.session, handle, id) };
            if status == api::OK || status == api::SETTING_NOT_FOUND {
                continue;
            }
            // Ustawienie dodane przez użytkownika w jego profilu nie ma
            // "wartości domyślnej" - wtedy po prostu je usuwamy.
            match delete {
                // SAFETY: jw.
                Some(f) => {
                    let s = unsafe { f(self.session, handle, id) };
                    if s != api::OK && s != api::SETTING_NOT_FOUND {
                        return Err(api::describe(s));
                    }
                }
                None => return Err(api::describe(status)),
            }
        }
        self.save()?;
        self.reload()
    }

    pub fn restore_profile(&mut self, target: &Target) -> Result<(), String> {
        self.ensure_backup()?;
        let handle = self.handle(target)?;
        let f = need(self.api.drs_restore_profile)?;
        // SAFETY: uchwyt profilu z tej sesji.
        check(unsafe { f(self.session, handle) })?;
        self.save()?;
        self.reload()
    }

    /// Przywraca ustawienia fabryczne wszystkich profili.
    pub fn restore_all(&mut self) -> Result<(), String> {
        self.ensure_backup()?;
        let f = need(self.api.drs_restore_all)?;
        // SAFETY: sesja ważna.
        check(unsafe { f(self.session) })?;
        self.save()?;
        self.reload()
    }

    /// Wartość domyślna sterownika i dozwolone wartości ustawienia.
    pub fn defaults(&self, id: u32) -> Option<(u32, Vec<u32>)> {
        let f = self.api.drs_setting_values?;
        let mut v = zeroed::<SettingValues>();
        v.version = SETTING_VALUES_VER;
        let mut max = 100u32;
        // SAFETY: struktura z poprawną wersją, `max` = pojemność tablicy.
        let status = unsafe { f(id, &mut max, &mut *v as *mut SettingValues as *mut c_void) };
        if status != api::OK || v.kind != TYPE_DWORD {
            return None;
        }
        // SAFETY: typ DWORD - aktywne pole unii to u32.
        let default = unsafe { v.default.u32_ };
        let values = v.values[..(v.count.min(100) as usize)]
            .iter()
            .map(|x| unsafe { x.u32_ })
            .collect();
        Some((default, values))
    }

    /// Ustawienia zapisane w profilu (tylko te, które profil zmienia).
    pub fn profile_settings(&self, target: &Target) -> Result<Vec<Read>, String> {
        let handle = self.handle(target)?;
        let f = need(self.api.drs_enum_settings)?;
        let info = self.info_of(handle)?;
        let mut out = Vec::new();
        let mut start = 0u32;
        while start < info.settings.max(1) {
            let mut batch: Vec<Setting> = (0..32)
                .map(|_| {
                    let mut s = *zeroed::<Setting>();
                    s.version = SETTING_VER;
                    s
                })
                .collect();
            let mut count = batch.len() as u32;
            // SAFETY: tablica `count` struktur z poprawną wersją.
            let status = unsafe {
                f(
                    self.session,
                    handle,
                    start,
                    &mut count,
                    batch.as_mut_ptr() as *mut c_void,
                )
            };
            if status == api::END_ENUMERATION || count == 0 {
                break;
            }
            check(status)?;
            out.extend(batch.iter().take(count as usize).map(to_read));
            start += count;
        }
        Ok(out)
    }

    /// Wszystkie identyfikatory ustawień znane sterownikowi.
    pub fn all_ids(&self) -> Vec<u32> {
        let Some(f) = self.api.drs_setting_ids else {
            return Vec::new();
        };
        let mut ids = vec![0u32; 4096];
        let mut count = ids.len() as u32;
        // SAFETY: tablica `count` elementów.
        if unsafe { f(ids.as_mut_ptr(), &mut count) } != api::OK {
            return Vec::new();
        }
        ids.truncate(count as usize);
        ids
    }

    /// Nazwa ustawienia według sterownika (po angielsku).
    pub fn name_of(&self, id: u32) -> Option<String> {
        let f = self.api.drs_setting_name?;
        let mut name = api::to_wide("");
        // SAFETY: bufor NvAPI_UnicodeString.
        (unsafe { f(id, name.as_mut_ptr()) } == api::OK).then(|| api::from_wide(&name[..]))
    }

    /// Odczyt ustawienia profilu na podstawie samego identyfikatora - do
    /// widoku zaawansowanego.
    pub fn read_any(&self, target: &Target, id: u32) -> Result<Option<Read>, String> {
        self.get(target, id)
    }
}

fn to_read(s: &Setting) -> Read {
    let dword = s.kind == TYPE_DWORD;
    Read {
        id: s.id,
        name: api::from_wide(&s.name),
        kind: s.kind,
        // SAFETY: dla typu DWORD aktywne pole unii to u32.
        value: dword.then_some(unsafe { s.current.u32_ }),
        location: Location::from_raw(s.location),
        predefined: (dword && s.is_predefined_valid != 0).then_some(unsafe { s.predefined.u32_ }),
    }
}

/// Katalog na kopie zapasowe bazy ustawień: obok programu.
pub fn backup_dir() -> Option<PathBuf> {
    Some(
        std::env::current_exe()
            .ok()?
            .parent()?
            .join("nvidia-backup"),
    )
}

/// Kopie zapasowe od najnowszej.
pub fn backups() -> Vec<PathBuf> {
    let Some(dir) = backup_dir() else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "nvbackup"))
        .collect();
    files.sort();
    files.reverse();
    files
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
