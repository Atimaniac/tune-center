//! Luminous Studio (FINAL FANTASY XV WINDOWS EDITION), tylko DirectX 11:
//! shadery SM5.0 (DXBC) to wpisy `.sb` archiwów `datas\**\*.earc`, każdy
//! jeden kontener. Archiwa mają tylko shadery wierzchołków i pikseli -
//! compute, geometrii i teselacji leżą jako surowe kontenery w .exe, w
//! bibliotekach DLL obok niego i w `HairWorks\*.bin` (stąd wycinane).
//!
//! Archiwum EARC 3.20 (magia "CRAF", little endian). Nagłówek 0x40: magia,
//! u32 wersja (bit 31 = chronione), u32 liczba wpisów, u32 rozmiar bloku,
//! u32 tabela wpisów, u32 lista URI, u32 lista ścieżek, u32 dane, u32 flagi,
//! u32 rozmiar kawałka w KiB, u64 skrót archiwum. Wpis 0x28: u64 skrót URI i
//! typu, u32 rozmiar, u32 rozmiar zapisany, u32 flagi (2 = kompresja), u32
//! położenie URI (tekst UTF-8 z zerem), u64 położenie danych, u32 położenie
//! ścieżki, u8 typ lokalizacji, u8 język, u16 klucz.
//!
//! Wpis skompresowany to kawałki: u32 długość spakowana, u32 długość po
//! rozpakowaniu, strumień zlib, wyrównanie do 4 B. Zaciemnienie układu (bez
//! tajnych kluczy - wszystko jest w pliku): w archiwum chronionym rozmiary i
//! położenie wpisu są XOR-owane łańcuchem FNV-1a 64 od skrótu archiwum, a
//! nagłówek pierwszego kawałka wpisu z niezerowym kluczem - dwoma krokami
//! LCG biblioteki C od tego klucza. Gra jest tylko czytana.

use std::fs::File;
use std::path::{Path, PathBuf};

use super::container::{carve_file, read_at, Container};
use super::readers::Detected;

const MAGIC: u32 = 0x4641_5243; // "CRAF"
const VERSION: u32 = 0x0003_0014; // 3.20
const PROTECTED: u32 = 0x8000_0000;
const COMPRESSED: u32 = 2;
const HEADER: usize = 0x40;
const ENTRY: usize = 0x28;

const MAX_ENTRIES: u32 = 1 << 22;
const MAX_URIS: u32 = 256 << 20;
const MAX_CHUNK: u64 = 64 << 20;
/// Najdłuższy URI (w grze najdłuższy .sb ma 97 B) i limit bajtów URI
/// trafień w jednym archiwum (wpisy mogą wskazywać jeden długi tekst).
const MAX_URI: usize = 1024;
const MAX_FOUND_URIS: usize = 64 << 20;
/// Największy wpis (kontener DXBC).
const MAX_ENTRY: u32 = 32 << 20;
/// Najgłębszy katalog w `datas`.
const MAX_DEPTH: usize = 16;

const EXE: &str = "ffxv_s.exe";
const DATAS: &str = "datas";
const SHADERGEN: &str = r"datas\shader\shadergen\autoexternal.earc";
const HAIRWORKS: &str = "HairWorks";
const SUFFIX: &[u8] = b".sb";

fn le16(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
fn le32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn le64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

/// Wersja archiwum bez bitu ochrony; `None` - to nie archiwum EARC.
fn version_of(path: &Path) -> Option<u32> {
    let file = File::open(path).ok()?;
    let mut head = [0u8; 8];
    read_at(&file, 0, &mut head).ok()?;
    (le32(&head, 0) == MAGIC).then(|| le32(&head, 4) & !PROTECTED)
}

/// Gra: archiwum shadergen w wersji 3.20 i plik .exe FINAL FANTASY XV
/// (inna gra Luminous, np. Forspoken, to DirectX 12).
pub fn detect(game: &Path) -> Option<Detected> {
    let shadergen = game.join(SHADERGEN);
    if version_of(&shadergen)? != VERSION {
        return None;
    }
    let exe = game.join(EXE);
    let exe_len = std::fs::metadata(&exe).ok().filter(|m| m.is_file())?.len();
    let gen_len = std::fs::metadata(&shadergen).map(|m| m.len()).unwrap_or(0);
    Some(Detected {
        engine: "Luminous Studio",
        version: Some("EARC 3.20".into()),
        api: "DirectX 11",
        files: vec![(shadergen, gen_len), (exe, exe_len)],
        exe: Some(EXE.into()),
    })
}

/// Wpis `.sb` archiwum (już odmaskowany).
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    flags: u32,
    at: u64,
    size: u32,
    stored: u32,
    key: u16,
}

/// Łańcuch kluczy archiwum chronionego: FNV-1a 64 (podstawa i mnożnik) od
/// skrótu archiwum, kolejno po wpisach tabeli.
struct Mask {
    sub: u64,
}

const FNV_BASIS: u64 = 0xCBF2_9CE4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;

impl Mask {
    fn new(archive: u64) -> Mask {
        Mask { sub: FNV_BASIS ^ archive }
    }
    /// Odmaskowuje (i maskuje - to XOR) rozmiar, rozmiar zapisany, położenie.
    fn apply(&mut self, hash: u64, size: &mut u32, stored: &mut u32, at: &mut u64) {
        let k = self.sub.wrapping_mul(FNV_PRIME) ^ hash;
        *size ^= (k >> 32) as u32;
        *stored ^= k as u32;
        self.sub = k.wrapping_mul(FNV_PRIME) ^ !hash;
        *at ^= self.sub;
    }
}

/// Maska nagłówka pierwszego kawałka: dwa kroki LCG biblioteki C na 64
/// bitach; górna połowa - długość spakowana, dolna - po rozpakowaniu.
fn chunk_key(key: u16) -> u64 {
    let step = |x: u64| x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
    step(step(u64::from(key)))
}

/// Archiwum: wpisy `.sb` i rozmiar kawałka. `Ok(None)` - plik nie jest
/// archiwum 3.20; `Err` - tabele poza plikiem.
fn entries(file: &File, len: u64) -> Result<Option<(Vec<Entry>, u64)>, String> {
    let mut head = [0u8; HEADER];
    if len < HEADER as u64 || read_at(file, 0, &mut head).is_err() || le32(&head, 0) != MAGIC {
        return Ok(None);
    }
    let version = le32(&head, 4);
    if version & !PROTECTED != VERSION {
        return Ok(None);
    }
    let (count, table) = (le32(&head, 8), u64::from(le32(&head, 16)));
    let (uri_list, path_list) = (le32(&head, 20), le32(&head, 24));
    let chunk = u64::from(le32(&head, 36)) * 1024;
    if count > MAX_ENTRIES
        || table + u64::from(count) * ENTRY as u64 > len
        || uri_list > path_list
        || u64::from(path_list) > len
        || path_list - uri_list > MAX_URIS
        || chunk == 0
        || chunk > MAX_CHUNK
    {
        return Err("nagłówek archiwum poza zakresem / archive header out of range".into());
    }
    let mut rows = vec![0u8; count as usize * ENTRY];
    let mut uris = vec![0u8; (path_list - uri_list) as usize];
    if read_at(file, table, &mut rows).is_err() || read_at(file, u64::from(uri_list), &mut uris).is_err() {
        return Err("tabele archiwum ucięte / archive tables cut short".into());
    }
    let mut mask = (version & PROTECTED != 0).then(|| Mask::new(le64(&head, 40)));
    let mut found = Vec::new();
    let mut uri_bytes = 0usize;
    for (i, e) in rows.chunks_exact(ENTRY).enumerate() {
        let hash = le64(e, 0);
        let (mut size, mut stored, mut at) = (le32(e, 8), le32(e, 12), le64(e, 24));
        // Każdy wpis: klucze łańcucha wynikają z poprzednich.
        if let Some(m) = mask.as_mut() {
            m.apply(hash, &mut size, &mut stored, &mut at);
        }
        let uri = le32(e, 20);
        if uri < uri_list || uri >= path_list {
            continue;
        }
        let rest = &uris[(uri - uri_list) as usize..];
        let rest = &rest[..rest.len().min(MAX_URI)];
        let Some(end) = rest.iter().position(|&c| c == 0) else { continue };
        if !rest[..end].ends_with(SUFFIX) {
            continue;
        }
        if size > MAX_ENTRY || stored > MAX_ENTRY || u64::from(stored) > len || at > len - u64::from(stored) {
            return Err(format!("wpis {i} poza zakresem / entry {i} out of range"));
        }
        uri_bytes += end;
        if uri_bytes > MAX_FOUND_URIS {
            return Err("za dużo URI w archiwum / archive URIs over budget".into());
        }
        found.push(Entry { flags: le32(e, 16), at, size, stored, key: le16(e, 38) });
    }
    Ok(Some((found, chunk)))
}

/// Bajty wpisu po rozpakowaniu; `None` - nie rozpakowują się do rozmiaru.
fn read_entry(file: &File, e: &Entry, chunk: u64) -> Option<Vec<u8>> {
    let mut raw = vec![0u8; e.stored as usize];
    read_at(file, e.at, &mut raw).ok()?;
    let size = e.size as usize;
    if e.flags & COMPRESSED == 0 {
        raw.truncate(size);
        return (raw.len() == size).then_some(raw);
    }
    let mut out = Vec::with_capacity(size);
    let mut p = 0usize;
    while out.len() < size {
        p = (p + 3) & !3;
        if p + 8 > raw.len() {
            return None;
        }
        let (mut c, mut d) = (le32(&raw, p), le32(&raw, p + 4));
        if p == 0 && e.key != 0 {
            let k = chunk_key(e.key);
            c ^= (k >> 32) as u32;
            d ^= k as u32;
        }
        p += 8;
        let (c, d) = (c as usize, d as usize);
        if d == 0 || d as u64 > chunk || d > size - out.len() || c > raw.len() - p {
            return None;
        }
        out.extend_from_slice(&super::inflate::zlib(&raw[p..p + c], d)?);
        p += c;
    }
    Some(out)
}

/// Pliki `*.earc` w `datas` (rekurencyjnie, posortowane).
fn archives(game: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(game.join(DATAS), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(kind) = e.file_type() else { continue };
            let path = e.path();
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if has_ext(&path, "earc") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn has_ext(p: &Path, ext: &str) -> bool {
    p.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext))
}

/// Pliki .exe i .dll obok gry oraz `HairWorks\*.bin` (posortowane).
fn binaries(game: &Path) -> Vec<PathBuf> {
    let list = |dir: PathBuf, exts: &[&str]| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.path())
            .filter(|p| exts.iter().any(|x| has_ext(p, x)))
            .collect();
        v.sort();
        v
    };
    let mut out = list(game.to_path_buf(), &["exe", "dll"]);
    out.extend(list(game.join(HAIRWORKS), &["bin"]));
    out
}

/// Podaje do `each` wpisy `.sb` jednego archiwum; zwraca ich liczbę.
fn read_archive(path: &Path, each: &mut dyn FnMut(&[u8])) -> Result<usize, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let Some((mut list, chunk)) = entries(&file, len)? else { return Ok(0) };
    // Kolejno w pliku - odczyt sekwencyjny.
    list.sort_by_key(|e| e.at);
    let mut n = 0;
    for e in &list {
        // Wpis, który nie jest jednym kontenerem, pomijamy.
        match read_entry(&file, e, chunk) {
            Some(b) if Container::parse(&b).is_some() => {
                each(&b);
                n += 1;
            }
            _ => {}
        }
    }
    Ok(n)
}

/// Każdy shader gry: wpisy `.sb` archiwów, potem kontenery wycięte z .exe,
/// DLL i HairWorks. Postęp po każdym archiwum i pliku.
pub fn for_each_shader(
    game: &Path,
    progress: &mut dyn FnMut(usize, usize),
    each: &mut dyn FnMut(&[u8]),
) -> Result<usize, String> {
    let archives = archives(game);
    let binaries = binaries(game);
    let total = archives.len() + binaries.len();
    progress(0, total);
    let (mut count, mut failed, mut first_error) = (0usize, 0usize, None);
    let mut done = 0;
    for path in &archives {
        match read_archive(path, each) {
            Ok(n) => count += n,
            Err(e) => {
                failed += 1;
                first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
            }
        }
        done += 1;
        progress(done, total);
    }
    for path in &binaries {
        let carved = File::open(path).and_then(|file| {
            let len = file.metadata()?.len();
            carve_file(&file, 0, len, &mut |_, c| {
                each(c.bytes);
                count += 1;
            })
        });
        if let Err(e) = carved {
            failed += 1;
            first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
        }
        done += 1;
        progress(done, total);
    }
    if count == 0 {
        let why = first_error.map(|e| format!(" ({failed} plików nieczytelnych / unreadable files; {e})")).unwrap_or_default();
        return Err(format!("brak shaderów DirectX 11 w {DATAS} / no DirectX 11 shaders in {DATAS}{why}"));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::build;
    use super::*;

    /// Kontener SM5 danego typu programu z dopełnieniem `pad` bajtów.
    fn shader(kind: u32, pad: usize, fill: u8) -> Vec<u8> {
        let program = ((kind << 16) | 0x50).to_le_bytes().to_vec();
        build(&[(b"SHEX", program), (b"PAD_", vec![fill; pad])])
    }

    fn adler32(b: &[u8]) -> u32 {
        let (mut a, mut s) = (1u32, 0u32);
        for &x in b {
            a = (a + u32::from(x)) % 65_521;
            s = (s + a) % 65_521;
        }
        (s << 16) | a
    }

    /// Strumień zlib z blokami bez kompresji.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = data.chunks(0xFFFF).collect();
        for (i, b) in blocks.iter().enumerate() {
            z.push(u8::from(i + 1 == blocks.len()));
            let n = b.len() as u16;
            z.extend_from_slice(&n.to_le_bytes());
            z.extend_from_slice(&(!n).to_le_bytes());
            z.extend_from_slice(b);
        }
        z.extend_from_slice(&adler32(data).to_be_bytes());
        z
    }

    /// Wpis testowy: URI, bajty, kompresja, klucz.
    struct Item<'a> {
        uri: &'a str,
        data: Vec<u8>,
        compress: bool,
        key: u16,
    }

    /// Archiwum EARC 3.20 (chronione albo nie) z kawałkami po 1 KiB.
    fn earc(items: &[Item], protected: bool) -> Vec<u8> {
        let chunk = 1024usize;
        let archive_hash = 0x1234_5678_9ABC_DEF0u64;
        let table = HEADER;
        let uri_list = table + items.len() * ENTRY;
        let mut uris = Vec::new();
        let mut uri_at = Vec::new();
        for it in items {
            uri_at.push((uri_list + uris.len()) as u32);
            uris.extend_from_slice(it.uri.as_bytes());
            uris.push(0);
        }
        let path_list = uri_list + uris.len();
        let mut data = Vec::new();
        let data_at = path_list.next_multiple_of(16);
        let mut rows = Vec::new();
        let mut mask = Mask::new(archive_hash);
        for (i, it) in items.iter().enumerate() {
            let at = (data_at + data.len()) as u64;
            let mut stored = Vec::new();
            if it.compress {
                for (k, part) in it.data.chunks(chunk).enumerate() {
                    while stored.len() % 4 != 0 {
                        stored.push(0);
                    }
                    let z = zlib_stored(part);
                    let (mut c, mut d) = (z.len() as u32, part.len() as u32);
                    if k == 0 && it.key != 0 {
                        let key = chunk_key(it.key);
                        c ^= (key >> 32) as u32;
                        d ^= key as u32;
                    }
                    stored.extend_from_slice(&c.to_le_bytes());
                    stored.extend_from_slice(&d.to_le_bytes());
                    stored.extend_from_slice(&z);
                }
            } else {
                stored = it.data.clone();
            }
            let hash = 0xA000 + i as u64;
            let (mut size, mut n, mut off) = (it.data.len() as u32, stored.len() as u32, at);
            if protected {
                mask.apply(hash, &mut size, &mut n, &mut off);
            }
            rows.extend_from_slice(&hash.to_le_bytes());
            rows.extend_from_slice(&size.to_le_bytes());
            rows.extend_from_slice(&n.to_le_bytes());
            rows.extend_from_slice(&(if it.compress { COMPRESSED } else { 0 }).to_le_bytes());
            rows.extend_from_slice(&uri_at[i].to_le_bytes());
            rows.extend_from_slice(&off.to_le_bytes());
            rows.extend_from_slice(&0u32.to_le_bytes());
            rows.extend_from_slice(&[0, 0]);
            rows.extend_from_slice(&it.key.to_le_bytes());
            data.extend_from_slice(&stored);
            while data.len() % 16 != 0 {
                data.push(0);
            }
        }
        let mut b = vec![0u8; HEADER];
        let version = VERSION | if protected { PROTECTED } else { 0 };
        for (at, v) in [
            (0, MAGIC),
            (4, version),
            (8, items.len() as u32),
            (12, 0x200),
            (16, table as u32),
            (20, uri_list as u32),
            (24, path_list as u32),
            (28, data_at as u32),
            (36, (chunk / 1024) as u32),
        ] {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[40..48].copy_from_slice(&archive_hash.to_le_bytes());
        b.extend_from_slice(&rows);
        b.extend_from_slice(&uris);
        b.resize(data_at, 0);
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn mask_and_chunk_key_are_xor() {
        let (mut s, mut n, mut o) = (100u32, 200u32, 300u64);
        Mask::new(7).apply(9, &mut s, &mut n, &mut o);
        assert_ne!((s, n, o), (100, 200, 300));
        Mask::new(7).apply(9, &mut s, &mut n, &mut o);
        assert_eq!((s, n, o), (100, 200, 300));
        assert_eq!(chunk_key(0), 12_345u64.wrapping_mul(1_103_515_245).wrapping_add(12_345));
    }

    #[test]
    fn reads_sb_entries_and_carves_binaries() {
        let vs = shader(1, 1500, 0x11); // dwa kawałki po 1 KiB
        let ps = shader(0, 20, 0x22);
        let cs = shader(5, 8, 0x33);
        let game = std::env::temp_dir().join(format!("tc-luminous-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&game);
        let gen_dir = game.join(r"datas\shader\shadergen");
        let lvl_dir = game.join(r"datas\level\a");
        std::fs::create_dir_all(&gen_dir).unwrap();
        std::fs::create_dir_all(&lvl_dir).unwrap();
        std::fs::create_dir_all(game.join(HAIRWORKS)).unwrap();

        // Gry jeszcze nie ma: brak .exe.
        let shadergen = earc(
            &[
                Item { uri: "data://shader/a.vs.sb", data: vs.clone(), compress: true, key: 0x5A5A },
                Item { uri: "data://shader/a.tpd", data: vec![9; 40], compress: false, key: 0 },
                Item { uri: "data://shader/b.ps.sb", data: ps.clone(), compress: false, key: 0 },
                Item { uri: "data://shader/junk.sb", data: vec![1; 64], compress: true, key: 3 },
            ],
            true,
        );
        std::fs::write(gen_dir.join("autoexternal.earc"), &shadergen).unwrap();
        assert!(detect(&game).is_none());

        let level = earc(&[Item { uri: "data://level/c.ps.sb", data: ps.clone(), compress: true, key: 0 }], false);
        std::fs::write(lvl_dir.join("x.EARC"), &level).unwrap();
        std::fs::write(game.join(r"datas\broken.earc"), &shadergen[..HEADER + 8]).unwrap();
        let mut exe = vec![0xCCu8; 300];
        exe.extend_from_slice(&cs);
        exe.extend_from_slice(&[0u8; 100]);
        std::fs::write(game.join(EXE), &exe).unwrap();
        std::fs::write(game.join(HAIRWORKS).join("hair.bin"), &cs).unwrap();

        let d = detect(&game).expect("detect");
        assert_eq!((d.engine, d.api, d.version.as_deref()), ("Luminous Studio", "DirectX 11", Some("EARC 3.20")));
        assert_eq!(d.exe.as_deref(), Some(EXE));

        let mut got = Vec::new();
        let mut steps = Vec::new();
        let n = for_each_shader(&game, &mut |a, b| steps.push((a, b)), &mut |b| got.push(b.to_vec())).unwrap();
        // Kolejno: poziom (ps), shadergen (vs, ps), .exe (cs), HairWorks (cs).
        assert_eq!(n, 5);
        assert_eq!(got, vec![ps.clone(), vs, ps, cs.clone(), cs]);
        // 3 archiwa (jedno uszkodzone) + .exe + .bin.
        assert_eq!(steps.first(), Some(&(0, 5)));
        assert_eq!(steps.last(), Some(&(5, 5)));

        // Zmieniony bajt maski: wpis poza zakresem, bez paniki.
        let mut bad = shadergen.clone();
        bad[40] ^= 0xFF;
        let file_path = game.join("bad.earc");
        std::fs::write(&file_path, &bad).unwrap();
        let file = File::open(&file_path).unwrap();
        let r = entries(&file, bad.len() as u64);
        assert!(r.is_err() || r.unwrap().is_some());
        drop(file);
        let _ = std::fs::remove_dir_all(&game);
    }
}
