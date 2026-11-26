//! Rdzenie obsługujące przerwania karty graficznej i urządzeń dźwiękowych.
//!
//! Sterownik karty graficznej potrafi zająć rdzeń procesora na ułamek
//! milisekundy. Jeśli ten sam rdzeń obsługuje przerwania dźwięku (kontroler
//! HD Audio albo kontroler USB, do którego podłączony jest DAC), dźwięk czeka
//! w kolejce i słychać trzaski. Moduł pokazuje, które rdzenie Windows
//! przydzielił tym urządzeniom, i potrafi przypiąć kontroler dźwięku do
//! innego rdzenia (wpis "Affinity Policy" urządzenia, udokumentowany przez
//! Microsoft) - z kopią poprzedniego stanu do cofnięcia.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Gpu,
    Audio,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub role: Role,
    /// Nazwa urządzenia (karta graficzna albo urządzenia dźwiękowe).
    pub name: String,
    /// Kontroler, którego przerwania liczą się dla dźwięku (np. kontroler USB).
    pub controller: String,
    /// Identyfikator urządzenia PCI zgłaszającego przerwania.
    pub instance: String,
    /// Rdzeń przypięty wpisem w rejestrze (jeden rdzeń), jeśli jest.
    pub pinned: Option<u16>,
    /// Rdzenie przydzielone przez Windows (zbiór z rejestru).
    pub cores: Vec<u16>,
}

impl Device {
    /// Rdzeń urządzenia: przypięty albo pierwszy z przydzielonego zbioru.
    pub fn core(&self) -> Option<u16> {
        self.pinned.or_else(|| self.cores.first().copied())
    }

    /// Czy rdzeń jest pewny (przypięty albo zbiór z jednym rdzeniem).
    pub fn exact(&self) -> bool {
        self.pinned.is_some() || self.cores.len() == 1
    }
}

/// Stan sprzed przypięcia urządzenia - do cofnięcia.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Backup {
    pub instance: String,
    pub policy: Option<u32>,
    pub mask: Option<Vec<u8>>,
}

/// Nazwa z wpisu rejestru: zwykły tekst albo "@plik,%id%;Nazwa %1 - %2;(a,b)"
/// (odwołanie do pliku INF, wzorzec i jego argumenty).
pub fn display_name(raw: &str) -> String {
    if !raw.starts_with('@') {
        return raw.trim().to_owned();
    }
    let mut parts = raw.splitn(3, ';');
    let _inf = parts.next();
    let mut name = parts.next().unwrap_or("").to_owned();
    if let Some(args) = parts.next() {
        for (i, arg) in args.trim_matches(|c| c == '(' || c == ')').split(',').enumerate() {
            name = name.replace(&format!("%{}", i + 1), arg.trim());
        }
    }
    name.trim().to_owned()
}

/// Numery rdzeni z maski bitowej.
pub fn mask_cores(mask: u64) -> Vec<u16> {
    (0..64).filter(|b| mask >> b & 1 == 1).collect()
}

/// Maska z bajtów rejestru (DWORD, QWORD albo dane binarne, little-endian).
pub fn mask_from_bytes(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    let n = bytes.len().min(8);
    b[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(b)
}

/// Rdzeń karty graficznej: zmierzony, a bez pomiaru - z rejestru.
pub fn gpu_core(devices: &[Device], measured: Option<u16>) -> Option<u16> {
    measured.or_else(|| devices.iter().find(|d| d.role == Role::Gpu).and_then(Device::core))
}

/// Urządzenia dźwiękowe obsługiwane przez ten sam rdzeń co karta graficzna.
pub fn shared<'a>(devices: &'a [Device], gpu: Option<u16>) -> Vec<&'a Device> {
    let Some(gpu) = gpu else { return Vec::new() };
    devices.iter().filter(|d| d.role == Role::Audio && d.core() == Some(gpu)).collect()
}

/// Rdzeń, na który przenieść dźwięk: środek zakresu procesorów logicznych,
/// parzysty, różny od rdzenia karty i jego sąsiada (druga nitka tego samego
/// rdzenia), od rdzenia 0 i od rdzeni już zajętych przez przypięte urządzenia.
pub fn pick_target(gpu: Option<u16>, used: &[u16], cpus: u16) -> Option<u16> {
    let cpus = cpus.min(64);
    let busy = |c: u16| c < 2 || used.contains(&c) || gpu.is_some_and(|g| c / 2 == g / 2);
    let mid = cpus / 2 / 2 * 2;
    (0..cpus).map(|i| (mid + i) % cpus).filter(|c| c % 2 == 0).find(|c| !busy(*c)).or_else(|| (0..cpus).find(|c| !busy(*c)))
}

#[cfg(windows)]
mod sys {
    use super::{mask_cores, mask_from_bytes, Backup, Device, Role};

    const HKLM: isize = 0x8000_0002u32 as i32 as isize;
    const KEY_READ: u32 = 0x2_0019;
    const KEY_SET_VALUE: u32 = 0x2;
    const REG_BINARY: u32 = 3;
    const REG_DWORD: u32 = 4;
    const CR_SUCCESS: u32 = 0;
    /// CM_GETIDLIST_FILTER_CLASS | CM_GETIDLIST_FILTER_PRESENT.
    const FILTER_PRESENT_CLASS: u32 = 0x200 | 0x100;
    const CM_PROB_DISABLED: u32 = 22;
    const CLASS_DISPLAY: &str = "{4d36e968-e325-11ce-bfc1-08002be10318}";
    const CLASS_MEDIA: &str = "{4d36e96c-e325-11ce-bfc1-08002be10318}";

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: isize, sub: *const u16, options: u32, access: u32, out: *mut isize) -> i32;
        fn RegQueryValueExW(key: isize, name: *const u16, reserved: *mut u32, kind: *mut u32, data: *mut u8, len: *mut u32) -> i32;
        fn RegSetValueExW(key: isize, name: *const u16, reserved: u32, kind: u32, data: *const u8, len: u32) -> i32;
        fn RegDeleteValueW(key: isize, name: *const u16) -> i32;
        fn RegCloseKey(key: isize) -> i32;
    }

    #[link(name = "cfgmgr32")]
    extern "system" {
        fn CM_Get_Device_ID_List_SizeW(len: *mut u32, filter: *const u16, flags: u32) -> u32;
        fn CM_Get_Device_ID_ListW(filter: *const u16, buffer: *mut u16, len: u32, flags: u32) -> u32;
        fn CM_Locate_DevNodeW(dev: *mut u32, id: *const u16, flags: u32) -> u32;
        fn CM_Get_DevNode_Status(status: *mut u32, problem: *mut u32, dev: u32, flags: u32) -> u32;
        fn CM_Get_Parent(parent: *mut u32, dev: u32, flags: u32) -> u32;
        fn CM_Get_Device_IDW(dev: u32, buffer: *mut u16, len: u32, flags: u32) -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn open(path: &str, access: u32) -> Option<isize> {
        let mut key = 0isize;
        (unsafe { RegOpenKeyExW(HKLM, wide(path).as_ptr(), 0, access, &mut key) } == 0).then_some(key)
    }

    /// Surowe bajty wartości rejestru.
    fn value(path: &str, name: &str) -> Option<Vec<u8>> {
        let key = open(path, KEY_READ)?;
        let mut buf = vec![0u8; 1024];
        let mut len = buf.len() as u32;
        let code = unsafe { RegQueryValueExW(key, wide(name).as_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), buf.as_mut_ptr(), &mut len) };
        unsafe { RegCloseKey(key) };
        (code == 0).then(|| {
            buf.truncate(len as usize);
            buf
        })
    }

    fn text(path: &str, name: &str) -> Option<String> {
        let bytes = value(path, name)?;
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
        Some(String::from_utf16_lossy(&units))
    }

    fn enum_path(instance: &str) -> String {
        format!(r"SYSTEM\CurrentControlSet\Enum\{instance}")
    }

    fn policy_path(instance: &str) -> String {
        format!(r"{}\Device Parameters\Interrupt Management\Affinity Policy", enum_path(instance))
    }

    /// Nazwa urządzenia: FriendlyName albo DeviceDesc.
    fn name_of(instance: &str) -> String {
        let path = enum_path(instance);
        text(&path, "FriendlyName").or_else(|| text(&path, "DeviceDesc")).map(|raw| super::display_name(&raw)).unwrap_or_default()
    }

    /// Obecne, włączone urządzenia danej klasy (identyfikatory).
    fn class_devices(class: &str) -> Vec<(String, u32)> {
        let filter = wide(class);
        let mut len = 0u32;
        let mut out = Vec::new();
        unsafe {
            if CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), FILTER_PRESENT_CLASS) != CR_SUCCESS || len == 0 {
                return out;
            }
            let mut buf = vec![0u16; len as usize];
            if CM_Get_Device_ID_ListW(filter.as_ptr(), buf.as_mut_ptr(), len, FILTER_PRESENT_CLASS) != CR_SUCCESS {
                return out;
            }
            for id in buf.split(|&c| c == 0).filter(|s| !s.is_empty()) {
                let mut z = id.to_vec();
                z.push(0);
                let mut dev = 0u32;
                if CM_Locate_DevNodeW(&mut dev, z.as_ptr(), 0) != CR_SUCCESS {
                    continue;
                }
                let (mut status, mut problem) = (0u32, 0u32);
                if CM_Get_DevNode_Status(&mut status, &mut problem, dev, 0) != CR_SUCCESS || problem == CM_PROB_DISABLED {
                    continue;
                }
                out.push((String::from_utf16_lossy(id), dev));
            }
        }
        out
    }

    /// Najbliższy przodek na magistrali PCI (albo samo urządzenie, jeśli jest na PCI).
    fn pci_ancestor(instance: &str, mut dev: u32) -> Option<String> {
        if instance.to_ascii_uppercase().starts_with(r"PCI\") {
            return Some(instance.to_owned());
        }
        for _ in 0..12 {
            let mut parent = 0u32;
            if unsafe { CM_Get_Parent(&mut parent, dev, 0) } != CR_SUCCESS {
                return None;
            }
            let mut buf = [0u16; 400];
            if unsafe { CM_Get_Device_IDW(parent, buf.as_mut_ptr(), buf.len() as u32, 0) } != CR_SUCCESS {
                return None;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let id = String::from_utf16_lossy(&buf[..end]);
            if id.to_ascii_uppercase().starts_with(r"PCI\") {
                return Some(id);
            }
            dev = parent;
        }
        None
    }

    /// Przypięty rdzeń (DevicePolicy 4 z jednym bitem) i zbiór przydzielony przez Windows.
    fn affinity(instance: &str) -> (Option<u16>, Vec<u16>) {
        let policy = policy_path(instance);
        let pinned = value(&policy, "DevicePolicy")
            .filter(|p| mask_from_bytes(p) == 4)
            .and_then(|_| value(&policy, "AssignmentSetOverride"))
            .map(|m| mask_cores(mask_from_bytes(&m)))
            .filter(|c| c.len() == 1)
            .map(|c| c[0]);
        let temporal = format!(r"{}\Device Parameters\Interrupt Management\Affinity Policy - Temporal", enum_path(instance));
        let cores = value(&temporal, "TargetSet").map(|m| mask_cores(mask_from_bytes(&m))).unwrap_or_default();
        (pinned, cores)
    }

    pub fn scan() -> Vec<Device> {
        let mut out: Vec<Device> = Vec::new();
        for (id, _) in class_devices(CLASS_DISPLAY) {
            if !id.to_ascii_uppercase().starts_with(r"PCI\") {
                continue;
            }
            let (pinned, cores) = affinity(&id);
            let name = name_of(&id);
            out.push(Device { role: Role::Gpu, controller: name.clone(), name, instance: id, pinned, cores });
        }
        for (id, dev) in class_devices(CLASS_MEDIA) {
            let Some(pci) = pci_ancestor(&id, dev) else { continue };
            let name = name_of(&id);
            // Kilka urządzeń dźwiękowych na jednym kontrolerze to jeden wiersz.
            if let Some(d) = out.iter_mut().find(|d| d.role == Role::Audio && d.instance.eq_ignore_ascii_case(&pci)) {
                if !name.is_empty() && !d.name.split(", ").any(|n| n == name) {
                    d.name = format!("{}, {name}", d.name);
                }
                continue;
            }
            let (pinned, cores) = affinity(&pci);
            out.push(Device { role: Role::Audio, name, controller: name_of(&pci), instance: pci, pinned, cores });
        }
        out
    }

    fn restart(instance: &str) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("pnputil")
            .args(["/restart-device", instance])
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| format!("pnputil: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(crate::i18n::tr(
                "Zapisano, ale urządzenia nie udało się zrestartować - zmiana zadziała po restarcie komputera.",
                "Saved, but the device could not be restarted - the change takes effect after restarting the computer.",
            )
            .to_owned())
        }
    }

    fn write(instance: &str, policy: Option<u32>, mask: Option<&[u8]>) -> Result<(), String> {
        let no_admin = || crate::i18n::tr("wymaga uprawnień administratora", "needs administrator rights").to_owned();
        let key = open(&policy_path(instance), KEY_SET_VALUE).ok_or_else(no_admin)?;
        let mut ok = true;
        unsafe {
            match policy {
                Some(p) => ok &= RegSetValueExW(key, wide("DevicePolicy").as_ptr(), 0, REG_DWORD, p.to_le_bytes().as_ptr(), 4) == 0,
                None => {
                    RegDeleteValueW(key, wide("DevicePolicy").as_ptr());
                }
            }
            match mask {
                Some(m) => ok &= RegSetValueExW(key, wide("AssignmentSetOverride").as_ptr(), 0, REG_BINARY, m.as_ptr(), m.len() as u32) == 0,
                None => {
                    RegDeleteValueW(key, wide("AssignmentSetOverride").as_ptr());
                }
            }
            RegCloseKey(key);
        }
        if ok {
            Ok(())
        } else {
            Err(no_admin())
        }
    }

    /// Przypina przerwania urządzenia do rdzenia `cpu` i restartuje urządzenie.
    /// Zwraca kopię poprzedniego stanu; `Err` z kopią, gdy zapis się udał, a
    /// restart urządzenia nie.
    pub fn pin(instance: &str, cpu: u16) -> Result<Backup, (Option<Backup>, String)> {
        let path = policy_path(instance);
        let backup = Backup {
            instance: instance.to_owned(),
            policy: value(&path, "DevicePolicy").map(|p| mask_from_bytes(&p) as u32),
            mask: value(&path, "AssignmentSetOverride"),
        };
        // 4 = IrqPolicySpecifiedProcessors.
        write(instance, Some(4), Some(&(1u64 << cpu.min(63)).to_le_bytes())).map_err(|e| (None, e))?;
        match restart(instance) {
            Ok(()) => Ok(backup),
            Err(e) => Err((Some(backup), e)),
        }
    }

    pub fn restore(backup: &Backup) -> Result<(), String> {
        write(&backup.instance, backup.policy, backup.mask.as_deref())?;
        restart(&backup.instance)
    }
}

#[cfg(not(windows))]
mod sys {
    use super::{Backup, Device};
    pub fn scan() -> Vec<Device> {
        Vec::new()
    }
    pub fn pin(_: &str, _: u16) -> Result<Backup, (Option<Backup>, String)> {
        Err((None, "Windows".into()))
    }
    pub fn restore(_: &Backup) -> Result<(), String> {
        Err("Windows".into())
    }
}

pub use sys::{pin, restore, scan};

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(role: Role, pinned: Option<u16>, cores: &[u16]) -> Device {
        Device { role, name: "n".into(), controller: "c".into(), instance: "PCI\\X".into(), pinned, cores: cores.to_vec() }
    }

    #[test]
    fn masks() {
        assert_eq!(mask_cores(0x4000), vec![14]);
        assert_eq!(mask_cores(0b110), vec![1, 2]);
        assert_eq!(mask_from_bytes(&[0, 0, 0x10, 0, 0, 0, 0, 0]), 1 << 20);
        assert_eq!(mask_from_bytes(&[0x00, 0x40, 0, 0]), 0x4000);
        assert_eq!(mask_from_bytes(&[]), 0);
    }

    #[test]
    fn names_from_registry() {
        assert_eq!(display_name("NVIDIA GeForce RTX 5070"), "NVIDIA GeForce RTX 5070");
        assert_eq!(display_name("@oem12.inf,%dev%;aune USB DAC"), "aune USB DAC");
        assert_eq!(
            display_name("@usbxhci.inf,%pci\\ven_8086.desc%;Intel(R) USB %1 eXtensible Host Controller - %2 (Microsoft);(3.20,1.20)"),
            "Intel(R) USB 3.20 eXtensible Host Controller - 1.20 (Microsoft)"
        );
    }

    #[test]
    fn core_and_sharing() {
        let gpu = dev(Role::Gpu, None, &[1, 2, 5]);
        let usb = dev(Role::Audio, Some(14), &[14]);
        let hda = dev(Role::Audio, None, &[1]);
        assert_eq!(gpu.core(), Some(1));
        assert!(!gpu.exact());
        assert!(usb.exact() && hda.exact());
        let all = vec![gpu, usb, hda];
        assert_eq!(gpu_core(&all, None), Some(1));
        assert_eq!(shared(&all, Some(1)).len(), 1);
        // Pomiar ma pierwszeństwo przed rejestrem.
        assert_eq!(gpu_core(&all, Some(14)), Some(14));
        assert_eq!(shared(&all, Some(14))[0].pinned, Some(14));
        assert!(shared(&all, None).is_empty());
    }

    #[test]
    fn target_core() {
        // 28 procesorów, karta na 1: środek zakresu.
        assert_eq!(pick_target(Some(1), &[], 28), Some(14));
        // Zajęty środek - następny parzysty.
        assert_eq!(pick_target(Some(1), &[14], 28), Some(16));
        // Karta na środku: omijamy ją i jej sąsiada.
        assert_eq!(pick_target(Some(8), &[], 16), Some(10));
        assert_eq!(pick_target(Some(9), &[], 16), Some(10));
        // Mały procesor: nie rdzeń 0/1 i nie rdzeń karty.
        assert_eq!(pick_target(Some(2), &[], 4), None);
        assert_eq!(pick_target(Some(1), &[], 4), Some(2));
        assert_eq!(pick_target(None, &[], 8), Some(4));
    }

    /// Urządzenia tego komputera: `cargo test irq_live -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn irq_live() {
        let d = scan();
        for x in &d {
            eprintln!("{:?} | {} | via {} | pinned {:?} cores {:?} | {}", x.role, x.name, x.controller, x.pinned, x.cores, x.instance);
        }
        assert!(d.iter().any(|x| x.role == Role::Gpu));
    }
}

#[cfg(all(test, windows))]
mod live {
    use super::*;

    /// Przypięcie i cofnięcie na wskazanym urządzeniu PCI (`TC_IRQ_DEV`,
    /// administrator; urządzenie jest dwa razy restartowane).
    #[test]
    #[ignore]
    fn irq_pin_restore_live() {
        let instance = std::env::var("TC_IRQ_DEV").expect("TC_IRQ_DEV");
        let read = |i: &str| {
            let p = format!(r"SYSTEM\CurrentControlSet\Enum\{i}\Device Parameters\Interrupt Management\Affinity Policy");
            (
                crate::tune::reg::get_dword(crate::tune::reg::HKLM, &p, "DevicePolicy"),
                std::process::Command::new("reg").args(["query", &format!(r"HKLM\{p}"), "/v", "AssignmentSetOverride"]).output().map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().last().unwrap_or("").to_owned()).unwrap_or_default(),
            )
        };
        let before = read(&instance);
        eprintln!("before: {before:?}");
        let backup = match pin(&instance, 24) {
            Ok(b) => b,
            Err((b, e)) => {
                eprintln!("pin: {e}");
                b.expect("backup")
            }
        };
        let during = read(&instance);
        eprintln!("pinned: {during:?}; backup {backup:?}");
        assert_eq!(during.0, Some(4));
        assert_eq!(during.1, "0000000100000000");
        let restored = restore(&backup);
        let after = read(&instance);
        eprintln!("restored ({restored:?}): {after:?}");
        assert_eq!(after, before);
    }
}
