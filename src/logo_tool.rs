//! Jednorazowe przygotowanie plików logo z obrazu źródłowego (`make_logo`)
//! albo z makiety (`concept_logo`, obecne okrągłe logo):
//! `TC_LOGO=<obraz> cargo test make_logo -- --ignored --nocapture`.
//!
//! Tworzy w `assets/`: `icon.png` (źródło 1024 px, białe tło usunięte),
//! `icon-128.rgba` (tekstura w programie i ikona zasobnika), `icon.ico`
//! (16-256 px, plik .exe i instalator) oraz obrazki kreatora instalatora
//! `wizard-small.bmp` i `wizard-large.bmp`.

use image::imageops::FilterType;
use image::{Rgba, RgbaImage};

/// Usuwa białe tło połączone z krawędzią obrazu (wypełnianie od brzegów),
/// z miękkim przejściem na jasnych pikselach brzegowych.
fn remove_white_background(img: &mut RgbaImage) {
    let (w, h) = img.dimensions();
    let white = |p: &Rgba<u8>| p[0] > 235 && p[1] > 235 && p[2] > 235;
    let mut seen = vec![false; (w * h) as usize];
    let mut stack: Vec<(u32, u32)> = Vec::new();
    for x in 0..w {
        stack.push((x, 0));
        stack.push((x, h - 1));
    }
    for y in 0..h {
        stack.push((0, y));
        stack.push((w - 1, y));
    }
    while let Some((x, y)) = stack.pop() {
        let i = (y * w + x) as usize;
        if seen[i] || !white(img.get_pixel(x, y)) {
            continue;
        }
        seen[i] = true;
        img.get_pixel_mut(x, y)[3] = 0;
        if x > 0 {
            stack.push((x - 1, y));
        }
        if x + 1 < w {
            stack.push((x + 1, y));
        }
        if y > 0 {
            stack.push((x, y - 1));
        }
        if y + 1 < h {
            stack.push((x, y + 1));
        }
    }
    // Piksele przy usuniętym tle: przezroczystość według jasności (bez
    // białej obwódki na ciemnym tle).
    let copy = img.clone();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let p = copy.get_pixel(x, y);
            if p[3] == 0 {
                continue;
            }
            let near_bg = [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
                .iter()
                .any(|&(a, b)| copy.get_pixel(a, b)[3] == 0);
            if near_bg {
                let light = p[0].min(p[1]).min(p[2]) as u32;
                if light > 180 {
                    let a = 255 - ((light - 180) * 255 / 75).min(255);
                    img.get_pixel_mut(x, y)[3] = a as u8;
                }
            }
        }
    }
}

/// Obrazek kreatora: logo na środku tła w kolorze `bg`.
fn wizard(src: &RgbaImage, w: u32, h: u32, logo: u32, bg: [u8; 3]) -> image::RgbImage {
    let small = image::imageops::resize(src, logo, logo, FilterType::Lanczos3);
    let mut out = image::RgbImage::from_pixel(w, h, image::Rgb(bg));
    let (ox, oy) = ((w - logo) / 2, (h - logo) / 2);
    for (x, y, p) in small.enumerate_pixels() {
        let a = p[3] as u32;
        let dst = out.get_pixel_mut(ox + x, oy + y);
        for c in 0..3 {
            dst[c] = ((p[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8;
        }
    }
    out
}

#[test]
#[ignore]
fn make_logo() {
    let path = std::env::var("TC_LOGO").expect("TC_LOGO");
    let mut img = image::open(&path).expect("odczyt obrazu").to_rgba8();
    println!("źródło: {}x{}", img.width(), img.height());
    remove_white_background(&mut img);
    write_icons(&img);
}

/// Zapisuje z logo `img` (RGBA, przezroczyste tło) pliki ikon programu i
/// obrazki kreatora instalatora (`icon.png`, `icon-128.rgba`, `icon.ico`,
/// `wizard-small.bmp`, `wizard-large.bmp`).
fn write_icons(img: &RgbaImage) {
    let img = img.clone();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");

    let master = image::imageops::resize(&img, 1024, 1024, FilterType::Lanczos3);
    master.save(assets.join("icon.png")).unwrap();

    let tex = image::imageops::resize(&img, 128, 128, FilterType::Lanczos3);
    std::fs::write(assets.join("icon-128.rgba"), tex.as_raw()).unwrap();

    let frames: Vec<image::codecs::ico::IcoFrame> = [16u32, 20, 24, 32, 40, 48, 64, 128, 256]
        .iter()
        .map(|&s| {
            let r = image::imageops::resize(&img, s, s, FilterType::Lanczos3);
            image::codecs::ico::IcoFrame::as_png(r.as_raw(), s, s, image::ExtendedColorType::Rgba8)
                .unwrap()
        })
        .collect();
    let file = std::fs::File::create(assets.join("icon.ico")).unwrap();
    image::codecs::ico::IcoEncoder::new(file)
        .encode_images(&frames)
        .unwrap();

    // Kreator Inno Setup (Modern): mały 55x58 (biały pasek u góry), duży 164x314.
    wizard(&img, 55, 58, 52, [255, 255, 255])
        .save(assets.join("wizard-small.bmp"))
        .unwrap();
    wizard(&img, 164, 314, 150, [13, 17, 23])
        .save(assets.join("wizard-large.bmp"))
        .unwrap();
    println!("zapisano pliki logo w {}", assets.display());
}

/// Logo z nagłówka makiety (okrągły pierścień "TUNE"): wycina koło
/// `TC_LOGO_CIRCLE=cx,cy,r` z obrazu `TC_CONCEPT`, ciemne tło makiety
/// zamienia na przezroczystość i zapisuje teksturę `assets/logo-header.rgba`
/// (160x160 RGBA). `TC_PREVIEW=<plik.png>` zapisuje dodatkowo podgląd.
#[test]
#[ignore]
fn concept_logo() {
    let path = std::env::var("TC_CONCEPT").expect("TC_CONCEPT");
    let img = image::open(&path).expect("odczyt obrazu").to_rgba8();
    println!("źródło: {}x{}", img.width(), img.height());
    let circle: Vec<f32> = std::env::var("TC_LOGO_CIRCLE")
        .expect("TC_LOGO_CIRCLE")
        .split(',')
        .map(|v| v.trim().parse().unwrap())
        .collect();
    let (cx, cy, r) = (circle[0], circle[1], circle[2]);
    let side = (2.0 * r).ceil() as u32;
    let (x0, y0) = ((cx - r).round() as u32, (cy - r).round() as u32);
    let mut out = image::imageops::crop_imm(&img, x0, y0, side, side).to_image();
    for (x, y, p) in out.enumerate_pixels_mut() {
        let dx = x as f32 + 0.5 - r;
        let dy = y as f32 + 0.5 - r;
        let d = (dx * dx + dy * dy).sqrt() / r;
        // Wnętrze pierścienia zostaje pełne; dalej zostają tylko jasne
        // piksele (pierścień i poświata), a ciemne tło znika.
        let bright = p[0].max(p[1]).max(p[2]) as f32;
        let by_light = ((bright - 30.0) / 70.0).clamp(0.0, 1.0);
        let inside = if d < 0.72 { 1.0 } else { 0.0 };
        let edge = ((1.0 - d) / 0.06).clamp(0.0, 1.0);
        let a = (by_light.max(inside) * edge * 255.0).round() as u8;
        p[3] = a;
    }
    let tex = image::imageops::resize(&out, 160, 160, FilterType::Lanczos3);
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    std::fs::write(assets.join("logo-header.rgba"), tex.as_raw()).unwrap();
    if let Ok(preview) = std::env::var("TC_PREVIEW") {
        // Podgląd na tle okna programu.
        let mut bg = RgbaImage::from_pixel(160, 160, Rgba([5, 10, 20, 255]));
        image::imageops::overlay(&mut bg, &tex, 0, 0);
        image::imageops::resize(&bg, 320, 320, FilterType::Nearest)
            .save(preview)
            .unwrap();
    }
    println!("zapisano assets/logo-header.rgba");
    // Ta sama grafika jako ikona programu, zasobnika i instalatora - bez
    // półprzezroczystej poświaty, czyste koło z wygładzoną krawędzią (na pasku
    // zadań poświata wyglądała jak ciemny kwadrat).
    let mut icon = image::imageops::crop_imm(&img, x0, y0, side, side).to_image();
    for (x, y, p) in icon.enumerate_pixels_mut() {
        let dx = x as f32 + 0.5 - r;
        let dy = y as f32 + 0.5 - r;
        let d = (dx * dx + dy * dy).sqrt() / r;
        p[3] = (((0.92 - d) / 0.03).clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    write_icons(&icon);
}
