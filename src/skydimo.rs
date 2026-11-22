//! Taśma LED Skydimo (podświetlenie za monitorem) przez port szeregowy USB
//! (układ CH340, VID 1A86, PID 7523).
//!
//! Protokół jak w OpenRGB (`Controllers/SkydimoSerialController`, GPL – użyty
//! jako odniesienie, bez kopiowania kodu): 115200 bodów; zapytanie "Moni-A" →
//! odpowiedź "SKxxxx[,numer]" (model wyznacza liczbę diod); ramka koloru w
//! stylu Adalight: "Ada", 0x00, numer ostatniej diody (16 bitów, big-endian),
//! potem R, G, B każdej diody. Bez sumy kontrolnej. Taśma gaśnie, gdy przez
//! chwilę nie dostaje ramek, więc ramka jest powtarzana co sekundę.
//! Jasność to przeskalowanie kolorów - urządzenie nie ma osobnego ustawienia.

use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const VID: u16 = 0x1A86;
pub const PID: u16 = 0x7523;
const BAUD: u32 = 115_200;
/// Co ile powtarzamy ramkę (OpenRGB: 1,5 s; taśma gaśnie bez danych).
const KEEPALIVE: Duration = Duration::from_millis(1000);
/// Liczba diod, gdy modelu nie ma w tabeli.
const DEFAULT_LEDS: usize = 150;

/// Liczba diod modelu (tabela z OpenRGB); `None` = model nieznany.
pub fn led_count(model: &str) -> Option<usize> {
    let n = match model {
        "SK0121" => 51,
        "SK0124" => 54,
        "SK0127" => 65,
        "SK0132" => 77,
        "SK0134" => 71,
        "SK0149" => 107,
        "SK0201" => 40,
        "SK0202" => 60,
        "SK0204" => 50,
        "SK0F01" => 58,
        "SK0F02" => 50,
        "SK0402" => 72,
        "SK0403" => 96,
        "SK0404" => 144,
        "SK0801" => 2,
        "SK0802" => 18,
        "SK0803" => 10,
        "SK0901" => 14,
        "SK0E01" => 16,
        "SK0H01" => 2,
        "SK0H02" => 4,
        "SK0K01" => 120,
        "SK0K02" => 15,
        "SK0M01" => 24,
        "SK0N01" => 256,
        "SK0N02" => 1024,
        "SK0N03" => 253,
        "SK0S01" => 32,
        "SK0L21" => 76,
        "SK0L24" => 80,
        "SK0L27" => 96,
        "SK0L32" => 114,
        "SK0L34" => 112,
        "SKA124" => 70,
        "SKA127" => 81,
        "SKA132" => 95,
        "SKA134" => 93,
        "SKB124" => 104,
        "SKB127" => 122,
        "SKB132" => 138,
        "SKB134" => 142,
        _ => return None,
    };
    Some(n)
}

/// Model z odpowiedzi na "Moni-A" ("SK0L32,..." → "SK0L32").
pub fn parse_model(reply: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(reply);
    let text = text.trim_end_matches(['\r', '\n']);
    let model: String = text.chars().take(6).collect::<String>().to_uppercase();
    (model.len() == 6 && model.starts_with("SK")).then_some(model)
}

/// Ramka koloru: wszystkie diody w jednym kolorze, przeskalowanym jasnością.
pub fn frame(leds: usize, color: [u8; 3], brightness: u8) -> Vec<u8> {
    let scale = |c: u8| ((c as u32 * brightness.min(100) as u32 + 50) / 100) as u8;
    let rgb = [scale(color[0]), scale(color[1]), scale(color[2])];
    let last = leds.saturating_sub(1) as u16;
    let mut out = Vec::with_capacity(6 + leds * 3);
    out.extend_from_slice(b"Ada\0");
    out.extend_from_slice(&last.to_be_bytes());
    for _ in 0..leds {
        out.extend_from_slice(&rgb);
    }
    out
}

/// Ustawienie podświetlenia.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Setting {
    pub on: bool,
    pub color: [u8; 3],
    /// 0-100 %.
    pub brightness: u8,
}

/// Stan połączenia z taśmą.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Off,
    Searching,
    /// Połączono: port, model, liczba diod.
    Connected {
        port: String,
        model: String,
        leds: usize,
    },
    Error(String),
}

/// Wątek sterujący taśmą: łączy się z portem, wysyła kolor przy zmianie i
/// co sekundę (podtrzymanie), przy wyłączeniu gasi taśmę i zwalnia port.
pub struct Control {
    tx: Sender<Setting>,
    status: Arc<Mutex<Status>>,
}

impl Control {
    pub fn start(initial: Setting, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let status = Arc::new(Mutex::new(Status::Off));
        let st = status.clone();
        crate::spawn("skydimo", move || worker(rx, initial, st, repaint));
        Self { tx, status }
    }

    pub fn set(&self, setting: Setting) {
        let _ = self.tx.send(setting);
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status::Off)
    }
}

/// Port szeregowy taśmy (pierwszy z pasującym VID/PID).
fn find_port() -> Result<String, String> {
    let ports = serialport::available_ports().map_err(|e| e.to_string())?;
    ports
        .into_iter()
        .find(|p| {
            matches!(&p.port_type, serialport::SerialPortType::UsbPort(u) if u.vid == VID && u.pid == PID)
        })
        .map(|p| p.port_name)
        .ok_or_else(|| {
            crate::i18n::tr(
                "Nie znaleziono taśmy Skydimo (USB CH340, VID 1A86 PID 7523).",
                "Skydimo strip not found (USB CH340, VID 1A86 PID 7523).",
            )
            .to_owned()
        })
}

struct Device {
    port: Box<dyn serialport::SerialPort>,
    name: String,
    model: String,
    leds: usize,
}

fn open() -> Result<Device, String> {
    let name = find_port()?;
    let mut port = serialport::new(&name, BAUD)
        .timeout(Duration::from_millis(300))
        .open()
        .map_err(|e| {
            if e.kind() == serialport::ErrorKind::NoDevice
                || e.kind() == serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied)
                || e.to_string().to_lowercase().contains("access")
                || e.to_string().contains("dostęp")
            {
                crate::trf!(
                    "Port {name} jest zajęty - zamknij aplikację SkyDimo (albo OpenRGB), żeby Tune Center mógł sterować taśmą.",
                    "Port {name} is busy - close the SkyDimo app (or OpenRGB) so Tune Center can control the strip."
                )
            } else {
                format!("{name}: {e}")
            }
        })?;
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(b"Moni-A")
        .map_err(|e| format!("{name}: {e}"))?;
    let mut reply = Vec::new();
    let mut buf = [0u8; 64];
    for _ in 0..10 {
        match port.read(&mut buf) {
            Ok(n) if n > 0 => {
                reply.extend_from_slice(&buf[..n]);
                if reply.ends_with(b"\n") || reply.ends_with(b"\r") || reply.len() >= 63 {
                    break;
                }
            }
            _ => break,
        }
    }
    let model = parse_model(&reply).unwrap_or_else(|| "?".to_owned());
    let leds = led_count(&model).unwrap_or(DEFAULT_LEDS);
    Ok(Device {
        port,
        name,
        model,
        leds,
    })
}

fn worker(
    rx: Receiver<Setting>,
    mut setting: Setting,
    status: Arc<Mutex<Status>>,
    repaint: impl Fn(),
) {
    let set_status = |s: Status| {
        if let Ok(mut st) = status.lock() {
            if *st != s {
                *st = s;
                repaint();
            }
        }
    };
    let mut device: Option<Device> = None;
    loop {
        if setting.on {
            if device.is_none() {
                set_status(Status::Searching);
                match open() {
                    Ok(d) => {
                        set_status(Status::Connected {
                            port: d.name.clone(),
                            model: d.model.clone(),
                            leds: d.leds,
                        });
                        device = Some(d);
                    }
                    Err(e) => set_status(Status::Error(e)),
                }
            }
            if let Some(d) = &mut device {
                let data = frame(d.leds, setting.color, setting.brightness);
                if let Err(e) = d.port.write_all(&data).and_then(|_| d.port.flush()) {
                    // Odłączona taśma albo port przejęty - spróbujemy ponownie.
                    set_status(Status::Error(format!("{}: {e}", d.name)));
                    device = None;
                }
            }
        } else if let Some(mut d) = device.take() {
            // Wyłączenie: gasimy taśmę i zwalniamy port (np. dla aplikacji SkyDimo).
            let _ = d.port.write_all(&frame(d.leds, [0, 0, 0], 0));
            let _ = d.port.flush();
            set_status(Status::Off);
        } else {
            set_status(Status::Off);
        }

        // Czekamy na zmianę albo na kolejne podtrzymanie; bez połączenia
        // ponawiamy próbę co 3 s.
        let wait = if setting.on && device.is_none() {
            Duration::from_secs(3)
        } else {
            KEEPALIVE
        };
        match rx.recv_timeout(wait) {
            Ok(s) => {
                setting = s;
                // Kilka zmian naraz (przeciąganie suwaka) - bierzemy ostatnią.
                while let Ok(s) = rx.try_recv() {
                    setting = s;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Koniec programu: taśma zgaśnie sama po chwili bez ramek.
}

#[cfg(test)]
mod tests {
    use super::{frame, led_count, parse_model};

    #[test]
    fn frame_layout() {
        let f = frame(2, [255, 128, 0], 100);
        assert_eq!(f, b"Ada\0\x00\x01\xff\x80\x00\xff\x80\x00");
        // Jasność 50 %: kolory przeskalowane.
        let f = frame(1, [200, 100, 0], 50);
        assert_eq!(&f[6..], &[100, 50, 0]);
        // 300 diod: ostatni indeks 299 = 0x012B.
        assert_eq!(&frame(300, [0, 0, 0], 0)[4..6], &[0x01, 0x2B]);
    }

    #[test]
    fn models() {
        assert_eq!(
            parse_model(b"SK0L32,\x01\x02\r\n").as_deref(),
            Some("SK0L32")
        );
        assert_eq!(parse_model(b"sk0121\r\n").as_deref(), Some("SK0121"));
        assert_eq!(parse_model(b"XX"), None);
        assert_eq!(led_count("SK0L32"), Some(114));
        assert_eq!(led_count("SKZZZZ"), None);
    }
}

#[cfg(test)]
mod hardware {
    use std::io::Write;

    /// Odczyt modelu i krótki test koloru (czerwony, potem wyłączenie):
    /// `cargo test skydimo_probe -- --ignored --nocapture` (aplikacja SkyDimo
    /// musi być zamknięta).
    #[test]
    #[ignore]
    fn skydimo_probe() {
        let mut d = super::open().expect("taśma");
        println!("port {} model {} diod {}", d.name, d.model, d.leds);
        d.port
            .write_all(&super::frame(d.leds, [255, 0, 0], 40))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        d.port
            .write_all(&super::frame(d.leds, [0, 0, 0], 0))
            .unwrap();
    }
}
