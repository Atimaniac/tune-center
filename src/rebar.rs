//! Wykrywanie Resizable BAR po rozmiarze okna pamięci karty graficznej.
//!
//! Bez Resizable BAR karta dostaje na magistrali PCI okno pamięci 256 MB;
//! z włączonym - okno wielkości zbliżonej do pamięci karty.
//!
//! Okna odczytujemy z zasobów przydzielonych urządzeniu przez Windows
//! (CfgMgr32, to samo źródło co karta "Zasoby" w Menedżerze urządzeń).
//! Duże okna 64-bitowe Windows opisuje jako zasób `MemLarge` - WMI
//! (`Win32_DeviceMemoryAddress`, używane do 0.17.6) ich nie zwraca, przez co
//! włączony ReBAR (np. RTX 5070: okno 16 GB) wyglądał na wyłączony.
//!
//! Odczyt trwa milisekundy, ale zostaje w osobnym wątku; niepowodzenie
//! niczego nie psuje: stan zostaje "nieznany".

use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Checking,
    /// Największe okno pamięci karty graficznej w MB.
    Known(u64),
    Unknown,
}

pub struct Probe {
    state: Arc<Mutex<State>>,
}

impl Probe {
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(State::Checking));
        let shared = state.clone();
        std::thread::spawn(move || {
            let result = largest_window_mb()
                .filter(|mb| *mb > 0)
                .map_or(State::Unknown, State::Known);
            *shared.lock().unwrap() = result;
        });
        Self { state }
    }

    pub fn state(&self) -> State {
        *self.state.lock().unwrap()
    }
}

/// Rozmiar zakresu z nagłówka `MEM_DES` / `MEM_LARGE_DES` (cfgmgr32.h):
/// Count u32, Type u32, Alloc_Base u64, Alloc_End u64, Flags u32, Reserved u32.
pub fn range_size(des: &[u8]) -> Option<u64> {
    let base = u64::from_le_bytes(des.get(8..16)?.try_into().ok()?);
    let end = u64::from_le_bytes(des.get(16..24)?.try_into().ok()?);
    (end >= base).then(|| end - base + 1)
}

#[cfg(windows)]
mod cm {
    //! Minimalne wiązanie do cfgmgr32.dll (Configuration Manager).

    pub type DevInst = u32;
    pub type Handle = usize;

    pub const CR_SUCCESS: u32 = 0;
    pub const ALLOC_LOG_CONF: u32 = 2;
    pub const RES_TYPE_MEM: u32 = 1;
    pub const RES_TYPE_MEM_LARGE: u32 = 7;
    /// CM_GETIDLIST_FILTER_CLASS | CM_GETIDLIST_FILTER_PRESENT.
    pub const FILTER_PRESENT_CLASS: u32 = 0x200 | 0x100;
    /// Klasa urządzeń "Karty graficzne" (GUID_DEVCLASS_DISPLAY).
    pub const DISPLAY_CLASS: &str = "{4d36e968-e325-11ce-bfc1-08002be10318}";

    #[link(name = "cfgmgr32")]
    extern "system" {
        pub fn CM_Get_Device_ID_List_SizeW(len: *mut u32, filter: *const u16, flags: u32) -> u32;
        pub fn CM_Get_Device_ID_ListW(
            filter: *const u16,
            buf: *mut u16,
            len: u32,
            flags: u32,
        ) -> u32;
        pub fn CM_Locate_DevNodeW(dev: *mut DevInst, id: *const u16, flags: u32) -> u32;
        pub fn CM_Get_First_Log_Conf(conf: *mut Handle, dev: DevInst, flags: u32) -> u32;
        pub fn CM_Get_Next_Res_Des(
            next: *mut Handle,
            current: Handle,
            resource: u32,
            resource_id: *mut u32,
            flags: u32,
        ) -> u32;
        pub fn CM_Get_Res_Des_Data_Size(size: *mut u32, des: Handle, flags: u32) -> u32;
        pub fn CM_Get_Res_Des_Data(des: Handle, buf: *mut u8, len: u32, flags: u32) -> u32;
        pub fn CM_Free_Res_Des_Handle(des: Handle) -> u32;
        pub fn CM_Free_Log_Conf_Handle(conf: Handle) -> u32;
    }
}

/// Okna pamięci (bajty) przydzielone kartom graficznym PCI: (identyfikator
/// urządzenia, rozmiary okien).
#[cfg(windows)]
pub fn display_windows() -> Vec<(String, Vec<u64>)> {
    use cm::*;
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let filter = wide(DISPLAY_CLASS);
    let mut out = Vec::new();
    // SAFETY: wywołania cfgmgr32 z buforami o rozmiarach podanych przez API;
    // każdy uchwyt jest zwalniany.
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), FILTER_PRESENT_CLASS)
            != CR_SUCCESS
        {
            return out;
        }
        let mut list = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(
            filter.as_ptr(),
            list.as_mut_ptr(),
            len,
            FILTER_PRESENT_CLASS,
        ) != CR_SUCCESS
        {
            return out;
        }
        for id in list.split(|&c| c == 0).filter(|s| !s.is_empty()) {
            let name = String::from_utf16_lossy(id);
            if !name.to_uppercase().starts_with("PCI\\") {
                continue;
            }
            let id: Vec<u16> = id.iter().copied().chain([0]).collect();
            let mut dev = 0;
            if CM_Locate_DevNodeW(&mut dev, id.as_ptr(), 0) != CR_SUCCESS {
                continue;
            }
            let mut conf = 0;
            if CM_Get_First_Log_Conf(&mut conf, dev, ALLOC_LOG_CONF) != CR_SUCCESS {
                continue;
            }
            let mut sizes = Vec::new();
            for kind in [RES_TYPE_MEM, RES_TYPE_MEM_LARGE] {
                let mut current = conf;
                let mut first = true;
                loop {
                    let (mut next, mut res_id) = (0, 0);
                    if CM_Get_Next_Res_Des(&mut next, current, kind, &mut res_id, 0) != CR_SUCCESS {
                        break;
                    }
                    if !first {
                        CM_Free_Res_Des_Handle(current);
                    }
                    first = false;
                    current = next;
                    let mut size = 0;
                    if CM_Get_Res_Des_Data_Size(&mut size, next, 0) == CR_SUCCESS && size >= 24 {
                        let mut buf = vec![0u8; size as usize];
                        if CM_Get_Res_Des_Data(next, buf.as_mut_ptr(), size, 0) == CR_SUCCESS {
                            if let Some(s) = range_size(&buf) {
                                sizes.push(s);
                            }
                        }
                    }
                }
                if !first {
                    CM_Free_Res_Des_Handle(current);
                }
            }
            CM_Free_Log_Conf_Handle(conf);
            out.push((name, sizes));
        }
    }
    out
}

/// Największe okno pamięci spośród kart graficznych, w MB.
#[cfg(windows)]
fn largest_window_mb() -> Option<u64> {
    display_windows()
        .iter()
        .flat_map(|(_, sizes)| sizes.iter().copied())
        .max()
        .map(|b| b >> 20)
}

#[cfg(not(windows))]
fn largest_window_mb() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::range_size;

    #[test]
    fn reads_range_size() {
        // Count, Type, Base 0x40_0000_0000, End 0x43_FFFF_FFFF (16 GB).
        let mut des = vec![1, 0, 0, 0, 0, 0, 0, 0];
        des.extend_from_slice(&0x40_0000_0000u64.to_le_bytes());
        des.extend_from_slice(&0x43_FFFF_FFFFu64.to_le_bytes());
        des.extend_from_slice(&[0; 8]);
        assert_eq!(range_size(&des), Some(16 << 30));
        assert_eq!(range_size(&des[..20]), None);
    }

    /// Okna pamięci kart w tym komputerze:
    /// `cargo test rebar_windows -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn rebar_windows() {
        for (id, sizes) in super::display_windows() {
            let mb: Vec<u64> = sizes.iter().map(|s| s >> 20).collect();
            println!("{id}: {mb:?} MB");
        }
    }
}
