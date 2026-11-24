//! Systemowe okna "Otwórz" i "Zapisz jako" (Windows: IFileOpenDialog,
//! IFileSaveDialog). Okno jest modalne dla wywołującego wątku.

use std::path::PathBuf;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
};

#[derive(Default)]
pub struct FileDialog {
    title: Option<String>,
    /// (opis, rozszerzenia bez kropki)
    filters: Vec<(String, Vec<String>)>,
    file_name: Option<String>,
}

impl FileDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }

    pub fn add_filter(mut self, name: &str, extensions: &[&str]) -> Self {
        self.filters
            .push((name.to_owned(), extensions.iter().map(|e| e.to_string()).collect()));
        self
    }

    pub fn set_file_name(mut self, name: &str) -> Self {
        self.file_name = Some(name.to_owned());
        self
    }

    /// Okno "Otwórz"; `None` - anulowano.
    pub fn pick_file(self) -> Option<PathBuf> {
        self.run(false)
    }

    /// Okno "Zapisz jako"; `None` - anulowano.
    pub fn save_file(self) -> Option<PathBuf> {
        self.run(true)
    }

    fn run(self, save: bool) -> Option<PathBuf> {
        // SAFETY: COM w trybie STA na czas okna (jeśli wątek ma już COM w innym
        // trybie, korzystamy z niego bez CoUninitialize); teksty filtrów żyją
        // do końca wywołań.
        unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let result = self.show(save);
            if init.is_ok() {
                CoUninitialize();
            }
            result
        }
    }

    unsafe fn show(&self, save: bool) -> Option<PathBuf> {
        let dialog: IFileDialog = if save {
            CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)
                .ok()?
                .into()
        } else {
            CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
                .ok()?
                .into()
        };
        // Filtry: "Opis" + "*.json;*.txt".
        let names: Vec<HSTRING> = self.filters.iter().map(|(n, _)| HSTRING::from(n.as_str())).collect();
        let specs: Vec<HSTRING> = self
            .filters
            .iter()
            .map(|(_, ext)| {
                HSTRING::from(
                    ext.iter()
                        .map(|e| format!("*.{e}"))
                        .collect::<Vec<_>>()
                        .join(";"),
                )
            })
            .collect();
        let table: Vec<COMDLG_FILTERSPEC> = names
            .iter()
            .zip(&specs)
            .map(|(n, s)| COMDLG_FILTERSPEC {
                pszName: PCWSTR(n.as_ptr()),
                pszSpec: PCWSTR(s.as_ptr()),
            })
            .collect();
        if !table.is_empty() {
            let _ = dialog.SetFileTypes(&table);
        }
        // Zapis: dopisz rozszerzenie pierwszego filtra, gdy go nie wpisano.
        let default_ext = self
            .filters
            .first()
            .and_then(|(_, e)| e.first())
            .map(|e| HSTRING::from(e.as_str()));
        if let Some(ext) = &default_ext {
            let _ = dialog.SetDefaultExtension(&*ext);
        }
        if let Some(t) = &self.title {
            let _ = dialog.SetTitle(&HSTRING::from(t.as_str()));
        }
        if let Some(n) = &self.file_name {
            let _ = dialog.SetFileName(&HSTRING::from(n.as_str()));
        }
        // Błąd = anulowano albo okno się nie otworzyło.
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        text.map(PathBuf::from)
    }
}
