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

use super::serial::{self, OpenError, Port};

pub const VID: u16 = 0x1A86;
pub const PID: u16 = 0x7523;
const BAUD: u32 = 115_200;
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

/// Port szeregowy taśmy (pierwszy z pasującym VID/PID).
fn find_port() -> Result<String, String> {
    serial::find_port(VID, PID).ok_or_else(|| {
            crate::i18n::tr(
                "Nie znaleziono taśmy Skydimo (USB CH340, VID 1A86 PID 7523).",
                "Skydimo strip not found (USB CH340, VID 1A86 PID 7523).",
            )
            .to_owned()
        })
}

struct Device {
    port: Port,
    name: String,
    model: String,
    leds: usize,
}

fn open() -> Result<Device, String> {
    let name = find_port()?;
    let port = Port::open(&name, BAUD, 300).map_err(|e| match e {
        OpenError::Busy => crate::trf!(
            "Port {name} jest zajęty - zamknij aplikację SkyDimo (albo OpenRGB), żeby Tune Center mógł sterować taśmą.",
            "Port {name} is busy - close the SkyDimo app (or OpenRGB) so Tune Center can control the strip."
        ),
        OpenError::Other(e) => e,
    })?;
    port.clear_input();
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

/// Taśma jako urządzenie RGB. Port jest otwierany dopiero przy pierwszym
/// ustawieniu koloru i zwalniany przy oddaniu sterowania (aplikacja SkyDimo
/// może wtedy z niego korzystać).
struct Strip {
    port_name: String,
    device: Option<Device>,
}

impl super::Device for Strip {
    fn info(&self) -> super::Info {
        let detail = match &self.device {
            Some(d) => crate::trf!(
                "{}, model {}, {} diod",
                "{}, model {}, {} LEDs",
                d.name,
                d.model,
                d.leds
            ),
            None => format!("{}, USB CH340 {VID:04X}:{PID:04X}", self.port_name),
        };
        super::Info {
            id: "skydimo".into(),
            name: "Skydimo".into(),
            family: super::Family::Skydimo,
            detail,
            restores: true,
            effects: vec![super::Effect::Static],
        }
    }

    fn apply(&mut self, s: &super::Setting) -> Result<(), String> {
        if self.device.is_none() {
            self.device = Some(open()?);
        }
        let Some(d) = &mut self.device else {
            return Ok(());
        };
        let data = frame(d.leds, s.rgb(), 100);
        if let Err(e) = d.port.write_all(&data).and_then(|_| d.port.flush()) {
            // Odłączona taśma albo port przejęty - następna próba otworzy go od nowa.
            let name = d.name.clone();
            self.device = None;
            return Err(format!("{name}: {e}"));
        }
        Ok(())
    }

    fn release(&mut self) -> Result<(), String> {
        // Gasimy taśmę i zwalniamy port.
        if let Some(d) = self.device.take() {
            let _ = d.port.write_all(&frame(d.leds, [0, 0, 0], 0));
            let _ = d.port.flush();
        }
        Ok(())
    }

    fn needs_keepalive(&self) -> bool {
        true
    }
}

/// Taśma na liście portów (bez otwierania - port może trzymać aplikacja SkyDimo).
pub fn discover() -> Result<Vec<Box<dyn super::Device>>, String> {
    Ok(match find_port() {
        Ok(port_name) => vec![Box::new(Strip {
            port_name,
            device: None,
        })],
        Err(_) => Vec::new(),
    })
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
