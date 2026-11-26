//! Nixxes (porty PC): dane gry w plikach DSAR (bloki LZ4). Tylko odczyt,
//! nic nie jest odszyfrowywane.
//!
//! - DSAR: "DSAR", u16, u16, u32 liczba bloków, u32 rozmiar nagłówka, u64
//!   rozmiar po rozpakowaniu, 8 B; potem 32 B na blok: u64 położenie po
//!   rozpakowaniu, u64 położenie w pliku, u32 rozmiar, u32 rozmiar zapisany,
//!   u8 kodek (3 - blok LZ4, 0 - zapisany wprost, 254 - luka czytana jako
//!   zera; 2 - GDeflate, nieczytany) i 7 B dopełnienia.
//! - Sucker Punch (Ghost of Tsushima, DirectX 12): `cache_pc\psarc\*.psarc`,
//!   każdy to PSARC w DSAR; kontenery DXIL wprost w paczkach `.xpps`, każda
//!   paczka to pula.
//! - Insomniac (Ratchet & Clank: Rift Apart, DirectX 12): `toc` i archiwa
//!   `d\`; shadery w `d\material` i `d\materialgraph` (całe strumienie DSAR,
//!   każdy to pula), każdy dwa razy: "IGSH", u32 rozmiar DXBC, u32 rozmiar
//!   DXIL, kontener DXBC i zaraz po nim DXIL. Zostaje DXIL; DXBC bez
//!   bliźniaka zostaje też. Compute i ray tracing - kontenery wprost w .exe.
//! - Stingray (HELLDIVERS 2, DirectX 11): `data\bundles.*.nxa`, każdy DSAR,
//!   tylko DXBC SM 5.0.
//!
//! PSARC (big-endian): "PSAR", u16, u16, kompresja ("zlib", "oodl"), u32
//! długość spisu (z nagłówkiem), u32 rozmiar wpisu, u32 wpisy, u32 rozmiar
//! bloku, u32 flagi. Wpis: MD5 nazwy, u32 pierwszy blok, u40 rozmiar, u40
//! położenie. Dalej rozmiary zapisanych bloków (u16 dla bloków do 64 KB,
//! inaczej u24; 0 - pełny blok); blok o rozmiarze równym zawartości jest
//! zapisany wprost. Wpis 0 to lista nazw (dopasowana po MD5).
//!
//! Root signature gry tworzą w trakcie gry - tu są stałe opisy z nagrań
//! znanych wersji (1.0, zakresy z jawnym przesunięciem), wybierane jak w grze:
//! Ghost of Tsushima - compute, compute bez zasobów, jedna graficzna;
//! Ratchet & Clank - potok z shaderem DXBC, compute, teselacja, graficzna.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::container::{read_at, Container, Kind};
use super::readers::{cached_detect, Bytes, Detected, Set12};

const PSARCS: &str = r"cache_pc\psarc";
const EXE_GOT: &str = "GhostOfTsushima.exe";
const EXE_RC: &str = "RiftApart.exe";
const EXE_HD2: &str = "helldivers2.exe";

/// Więcej rekordów bloków to plik uszkodzony (zmierzone najwyżej ~59 tys.).
const MAX_BLOCKS: u32 = 1 << 18;
/// Największy rozpakowywany blok LZ4 (zmierzone najwyżej 320 KB).
const MAX_BLOCK: u32 = 4 << 20;
/// Okno przeszukiwania strumienia.
const WINDOW: u64 = 16 << 20;
/// Największy przyjmowany kontener.
const MAX_CONTAINER: u32 = 16 << 20;
/// Spis PSARC i lista nazw - najwięcej bajtów.
const MAX_TOC: u32 = 32 << 20;
const MAX_NAMES: u64 = 64 << 20;
const MAX_ENTRIES: u32 = 1 << 24;
const MAX_SHADERS: usize = 1 << 20;
const MAX_HITS: usize = 4 << 20;
/// Rozpoznanie: tyle bajtów (spisy i rozpakowane dane) razem.
const DETECT_BUDGET: u64 = 64 << 20;

const STORED: u8 = 0;
const GDEFLATE: u8 = 2;
const LZ4: u8 = 3;
const GAP: u8 = 254;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn be40(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(*b.get(at)?) << 32 | u64::from(be32(b, at + 1)?))
}

/// Limit czytanych bajtów (wspólny dla wielu plików).
struct Meter {
    left: u64,
    used: u64,
}

impl Meter {
    fn new(limit: u64) -> Meter {
        Meter { left: limit, used: 0 }
    }
    /// Pobiera `n` bajtów z limitu; false - za mało (nic nie pobrane).
    fn take(&mut self, n: u64) -> bool {
        if n > self.left {
            return false;
        }
        self.left -= n;
        self.used += n;
        true
    }
}

// ----------------------------------- DSAR -------------------------------------

#[derive(Clone, Copy, Debug)]
struct Block {
    start: u64,
    at: u64,
    size: u32,
    packed: u32,
    codec: u8,
}

/// Spis bloków pliku DSAR.
struct Dsar {
    len: u64,
    blocks: Vec<Block>,
}

/// Ostatni rozpakowany blok (odczyty po kolei trafiają w ten sam).
#[derive(Default)]
struct Cache {
    block: Option<usize>,
    data: Vec<u8>,
}

impl Dsar {
    /// Spis bloków; `None` - to nie DSAR, plik uszkodzony albo spis ponad limit.
    fn parse(file: &File, meter: &mut Meter) -> Option<Dsar> {
        let file_len = file.metadata().ok()?.len();
        let mut head = [0u8; 32];
        read_at(file, 0, &mut head).ok()?;
        if &head[..4] != b"DSAR" {
            return None;
        }
        let n = le32(&head, 8)?;
        let header = u64::from(le32(&head, 12)?);
        let len = le64(&head, 16)?;
        if n == 0 || n > MAX_BLOCKS || 32 + 32 * u64::from(n) > header || header > file_len || len == 0 {
            return None;
        }
        if !meter.take(32 * u64::from(n)) {
            return None;
        }
        let mut blocks = Vec::with_capacity(n as usize);
        let mut end = 0u64;
        let mut chunk = vec![0u8; 32 * 4096.min(n as usize)];
        let mut i = 0u32;
        while i < n {
            let k = 4096.min(n - i) as usize;
            read_at(file, 32 + 32 * u64::from(i), &mut chunk[..32 * k]).ok()?;
            for r in chunk[..32 * k].as_chunks::<32>().0 {
                let (start, at) = (le64(r, 0)?, le64(r, 8)?);
                let (size, packed, codec) = (le32(r, 16)?, le32(r, 20)?, r[24]);
                // Odejmowanie zamiast sumy: wartości z pliku się nie przepełnią.
                if start < end
                    || start > len
                    || size == 0
                    || u64::from(size) > len - start
                    || at > file_len
                    || u64::from(packed) > file_len - at
                    || codec == STORED && packed != size
                {
                    return None;
                }
                end = start + u64::from(size);
                blocks.push(Block { start, at, size, packed, codec });
            }
            i += k as u32;
        }
        Some(Dsar { len, blocks })
    }

    /// Wypełnia `out` od `from`; zakres bez bloku czyta się jako zera.
    fn read(&self, file: &File, cache: &mut Cache, from: u64, out: &mut [u8]) -> Result<(), String> {
        let end = from
            .checked_add(out.len() as u64)
            .filter(|&e| e <= self.len)
            .ok_or("DSAR: odczyt za końcem / read past the end")?;
        let mut pos = from;
        let mut i = self.blocks.partition_point(|b| b.start + u64::from(b.size) <= pos);
        while pos < end {
            let o = (pos - from) as usize;
            match self.blocks.get(i) {
                Some(b) if b.start <= pos => {
                    let lo = pos - b.start;
                    let n = (u64::from(b.size) - lo).min(end - pos) as usize;
                    let dst = &mut out[o..o + n];
                    match b.codec {
                        GAP => dst.fill(0),
                        STORED => read_at(file, b.at + lo, dst).map_err(|e| format!("DSAR: {e}"))?,
                        LZ4 => {
                            let data = self.unpack(file, cache, i)?;
                            dst.copy_from_slice(&data[lo as usize..lo as usize + n]);
                        }
                        GDEFLATE => return Err("DSAR: blok GDeflate (nieczytany) / a GDeflate block (not decoded)".into()),
                        c => return Err(format!("DSAR: nieznany kodek / unknown codec {c}")),
                    }
                    pos += n as u64;
                    i += 1;
                }
                next => {
                    let stop = next.map_or(end, |b| b.start.min(end));
                    out[o..(stop - from) as usize].fill(0);
                    pos = stop;
                }
            }
        }
        Ok(())
    }

    fn unpack<'c>(&self, file: &File, cache: &'c mut Cache, i: usize) -> Result<&'c [u8], String> {
        if cache.block != Some(i) {
            let b = self.blocks[i];
            if b.size > MAX_BLOCK || b.packed > MAX_BLOCK {
                return Err("DSAR: blok LZ4 ponad limit / an LZ4 block over the limit".into());
            }
            let mut raw = vec![0u8; b.packed as usize];
            read_at(file, b.at, &mut raw).map_err(|e| format!("DSAR: {e}"))?;
            let data = crate::lz4::decompress(&raw, b.size as usize)?;
            if data.len() != b.size as usize {
                return Err("DSAR: blok LZ4 innego rozmiaru / an LZ4 block of another size".into());
            }
            cache.block = Some(i);
            cache.data = data;
        }
        Ok(&cache.data)
    }
}

/// Strumień do przeszukania: czytany w dowolnym miejscu.
trait Source {
    fn len(&self) -> u64;
    fn read(&mut self, from: u64, out: &mut [u8]) -> Result<(), String>;
}

/// Plik: strumień DSAR albo zwykłe bajty.
struct Reader<'a> {
    file: &'a File,
    dsar: Option<&'a Dsar>,
    file_len: u64,
    cache: Cache,
}

impl<'a> Reader<'a> {
    fn new(file: &'a File, dsar: Option<&'a Dsar>) -> Option<Reader<'a>> {
        Some(Reader { file, dsar, file_len: file.metadata().ok()?.len(), cache: Cache::default() })
    }
}

impl Source for Reader<'_> {
    fn len(&self) -> u64 {
        self.dsar.map_or(self.file_len, |d| d.len)
    }
    fn read(&mut self, from: u64, out: &mut [u8]) -> Result<(), String> {
        match self.dsar {
            Some(d) => d.read(self.file, &mut self.cache, from, out),
            None => {
                if from.checked_add(out.len() as u64).is_none_or(|e| e > self.file_len) {
                    return Err("odczyt za końcem pliku / read past the end of the file".into());
                }
                read_at(self.file, from, out).map_err(|e| e.to_string())
            }
        }
    }
}

// ----------------------------------- PSARC ------------------------------------

/// Spis archiwum PSARC (bez nazw).
struct Psarc {
    block: u64,
    width: usize,
    oodle: bool,
    /// (pierwszy blok, rozmiar, położenie).
    entries: Vec<(u32, u64, u64)>,
    /// Rozmiary zapisanych bloków (po `width` bajtów).
    sizes: Vec<u8>,
}

impl Psarc {
    /// Spis i nazwy wpisów (indeks jak we wpisach; 0 - lista nazw).
    fn parse(r: &mut Reader, meter: &mut Meter) -> Option<(Psarc, Vec<Option<String>>)> {
        let length = r.len();
        let mut head = [0u8; 32];
        r.read(0, &mut head).ok()?;
        if &head[..4] != b"PSAR" {
            return None;
        }
        let oodle = match &head[8..12] {
            b"zlib" => false,
            b"oodl" => true,
            _ => return None,
        };
        let (toc_len, entry_size, n, bs) = (be32(&head, 12)?, be32(&head, 16)?, be32(&head, 20)?, be32(&head, 24)?);
        if !(1024..=1 << 24).contains(&bs)
            || entry_size < 30
            || n == 0
            || n > MAX_ENTRIES
            || u64::from(toc_len) > length
            || toc_len > MAX_TOC
            || 32 + u64::from(n) * u64::from(entry_size) > u64::from(toc_len)
            || !meter.take(u64::from(toc_len))
        {
            return None;
        }
        let mut toc = vec![0u8; toc_len as usize - 32];
        r.read(32, &mut toc).ok()?;
        let width = if bs <= 1 << 16 { 2 } else { 3 };
        let table = (n * entry_size) as usize;
        let blocks = ((toc.len() - table) / width) as u64;
        let mut entries = Vec::with_capacity(n as usize);
        let mut by_md5: HashMap<[u8; 16], usize> = HashMap::new();
        for i in 0..n as usize {
            let e = &toc[i * entry_size as usize..];
            let (first, size, offset) = (be32(e, 16)?, be40(e, 20)?, be40(e, 25)?);
            if u64::from(first) + size.div_ceil(u64::from(bs)) > blocks || offset > length {
                return None;
            }
            entries.push((first, size, offset));
            by_md5.entry(e[..16].try_into().ok()?).or_insert(i);
        }
        let sizes = toc.split_off(table);
        let p = Psarc { block: u64::from(bs), width, oodle, entries, sizes };
        let list_len = p.entries[0].1;
        if list_len > MAX_NAMES || !meter.take(list_len) {
            return None;
        }
        let mut list = vec![0u8; list_len as usize];
        p.read(r, &mut None, 0, 0, &mut list).ok()?;
        let mut names = vec![None; p.entries.len()];
        for name in list.split(|c| matches!(c, b'\n' | b'\r' | 0)).filter(|s| !s.is_empty()) {
            if let Some(&i) = by_md5.get(&md5(name)) {
                if i > 0 && names[i].is_none() {
                    names[i] = Some(String::from_utf8_lossy(name).into_owned());
                }
            }
        }
        Some((p, names))
    }

    /// Rozmiar zapisanego bloku.
    fn stored(&self, block: u64) -> Result<u64, String> {
        let at = usize::try_from(block).ok().and_then(|b| b.checked_mul(self.width));
        let b = at
            .and_then(|a| self.sizes.get(a..a + self.width))
            .ok_or("PSARC: blok poza spisem / a block past the table")?;
        let v = b.iter().fold(0u64, |v, &x| v << 8 | u64::from(x));
        Ok(if v == 0 { self.block } else { v })
    }

    /// Wypełnia `out` bajtami wpisu od `from`, rozpakowując tylko jego bloki.
    fn read(&self, r: &mut Reader, ooz: &mut Option<oozextract::Extractor>, entry: usize, from: u64, out: &mut [u8]) -> Result<(), String> {
        let &(first, size, offset) = self.entries.get(entry).ok_or("PSARC: brak wpisu / no such entry")?;
        let end = from
            .checked_add(out.len() as u64)
            .filter(|&e| e <= size)
            .ok_or("PSARC: odczyt za końcem wpisu / read past the entry's end")?;
        let bs = self.block;
        let mut b = from / bs;
        let mut at = offset;
        for k in 0..b {
            at = at.saturating_add(self.stored(u64::from(first) + k)?);
        }
        let (mut raw, mut data) = (Vec::new(), Vec::new());
        while b * bs < end {
            let pos = b * bs;
            let want = (size - pos).min(bs) as usize;
            let stored = self.stored(u64::from(first) + b)? as usize;
            raw.resize(stored, 0);
            r.read(at, &mut raw)?;
            at += stored as u64;
            let block: &[u8] = if stored == want {
                &raw
            } else {
                self.unpack(&raw, want, ooz, &mut data)?;
                &data
            };
            let lo = from.saturating_sub(pos) as usize;
            let hi = (end - pos).min(want as u64) as usize;
            let o = (pos + lo as u64 - from) as usize;
            out[o..o + hi - lo].copy_from_slice(&block[lo..hi]);
            b += 1;
        }
        Ok(())
    }

    fn unpack(&self, raw: &[u8], want: usize, ooz: &mut Option<oozextract::Extractor>, data: &mut Vec<u8>) -> Result<(), String> {
        // Archiwum "oodl" może mieć też bloki zlib.
        if !self.oodle || raw.first() == Some(&0x78) {
            *data = super::inflate::zlib(raw, want).ok_or("PSARC: blok zlib uszkodzony / a corrupt zlib block")?;
            return Ok(());
        }
        data.clear();
        data.resize(want, 0);
        match ooz.get_or_insert_with(oozextract::Extractor::new).read_from_slice(raw, data) {
            Ok(n) if n == want => Ok(()),
            _ => Err("PSARC: blok Oodle uszkodzony / a corrupt Oodle block".into()),
        }
    }
}

/// Wpis archiwum PSARC jako strumień.
struct EntrySource<'a, 'f> {
    psarc: &'a Psarc,
    reader: &'a mut Reader<'f>,
    ooz: &'a mut Option<oozextract::Extractor>,
    entry: usize,
}

impl Source for EntrySource<'_, '_> {
    fn len(&self) -> u64 {
        self.psarc.entries[self.entry].1
    }
    fn read(&mut self, from: u64, out: &mut [u8]) -> Result<(), String> {
        self.psarc.read(self.reader, self.ooz, self.entry, from, out)
    }
}

/// MD5 (RFC 1321) - tylko do dopasowania nazw wpisów PSARC.
fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32).collect();
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    let mut h: [u32; 4] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];
    for chunk in msg.as_chunks::<64>().0 {
        let m: Vec<u32> = chunk.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i / 16 * 4 + i % 4]));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 16];
    for (o, x) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        o.copy_from_slice(&x.to_le_bytes());
    }
    out
}

// --------------------------------- wycinanie ----------------------------------

/// Kontener znaleziony w strumieniu.
#[derive(Clone, Debug)]
struct Hit {
    at: u64,
    size: u32,
    id: [u8; 16],
    kind: Kind,
    dxil: bool,
    /// Shader nie deklaruje żadnego zasobu.
    bare: bool,
    /// Nagłówek "IGSH" przed kontenerem: (rozmiar DXBC, rozmiar DXIL).
    igsh: Option<(u32, u32)>,
}

impl Hit {
    fn key(&self) -> ([u8; 16], u32) {
        (self.id, self.size)
    }
}

/// Czy shader nie deklaruje zasobów: DXIL - liczba zasobów w części PSV0,
/// DXBC - liczba powiązań w RDEF. Bez tych części - nie wiadomo (false).
fn binds_nothing(c: &Container) -> bool {
    if c.is_dxil() {
        c.part(b"PSV0").and_then(|p| le32(p, 4usize.checked_add(le32(p, 0)? as usize)?)) == Some(0)
    } else {
        c.part(b"RDEF").and_then(|p| le32(p, 8)) == Some(0)
    }
}

/// Każdy kontener z programem w strumieniu, po kolei (wnętrza kontenera nie są
/// przeszukiwane), oknami po `WINDOW`; kontener na granicy okna jest czytany
/// osobno. Odczyty są liczone w `meter` - po limicie przeszukiwanie staje.
/// `hit` zwraca true, gdy wystarczy. Błąd odczytu okna kończy przeszukiwanie
/// (kontenery znalezione wcześniej zostają przekazane).
fn carve(src: &mut dyn Source, meter: &mut Meter, hit: &mut dyn FnMut(&Hit, &Container) -> bool) -> Result<(), String> {
    let len = src.len();
    let mut buf = Vec::new();
    let mut pos = 0u64;
    while pos < len {
        // Przy kończącym się limicie - krótsze okno (nie poniżej 64 B).
        let want = (len - pos).min(WINDOW);
        let n = want.min(meter.left);
        if n < want && n < 64 || !meter.take(n) {
            return Ok(());
        }
        let n = n as usize;
        buf.resize(n, 0);
        src.read(pos, &mut buf)?;
        let last = pos + n as u64 == len;
        // Nagłówek przecięty końcem okna będzie cały w następnym.
        let mut next = if last { len } else { pos + n as u64 - 31 };
        let mut i = 0;
        while let Some(k) = buf[i..].windows(4).position(|w| w == b"DXBC") {
            let at = i + k;
            i = at + 1;
            if at + 32 > n {
                if last {
                    continue;
                }
                break;
            }
            let size = le32(&buf, at + 24).unwrap_or(0);
            let off = pos + at as u64;
            if le32(&buf, at + 20) != Some(1) || !(40..=MAX_CONTAINER).contains(&size) || u64::from(size) > len - off {
                continue;
            }
            let whole: Vec<u8>;
            let bytes: &[u8] = if at + size as usize <= n {
                &buf[at..at + size as usize]
            } else {
                if !meter.take(u64::from(size)) {
                    continue;
                }
                let mut b = vec![0u8; size as usize];
                if src.read(off, &mut b).is_err() {
                    continue;
                }
                whole = b;
                &whole
            };
            let Some(c) = Container::parse(bytes).filter(|c| c.bytes.len() == size as usize) else { continue };
            let Some(kind) = c.kind() else { continue };
            let dxil = c.is_dxil();
            let igsh = if dxil || off < 12 {
                None
            } else {
                let mut h = [0u8; 12];
                let ok = if at >= 12 {
                    h.copy_from_slice(&buf[at - 12..at]);
                    true
                } else {
                    src.read(off - 12, &mut h).is_ok()
                };
                (ok && &h[..4] == b"IGSH").then(|| (le32(&h, 4).unwrap_or(0), le32(&h, 8).unwrap_or(0)))
            };
            let h = Hit { at: off, size, id: c.identity(), kind, dxil, bare: binds_nothing(&c), igsh };
            if hit(&h, &c) {
                return Ok(());
            }
            i = (at + size as usize).min(n);
            next = next.max(off + u64::from(size));
        }
        pos = next;
    }
    Ok(())
}

// ---------------------------------- układy ------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Layout {
    SuckerPunch,
    Insomniac,
    Stingray,
}

impl Layout {
    fn name(self) -> &'static str {
        match self {
            Layout::SuckerPunch => "Sucker Punch",
            Layout::Insomniac => "Insomniac",
            Layout::Stingray => "Stingray",
        }
    }
}

fn layout(game: &Path) -> Option<Layout> {
    if game.join(PSARCS).is_dir() {
        Some(Layout::SuckerPunch)
    } else if game.join("toc").is_file() && game.join(r"d\materialgraph").is_file() {
        Some(Layout::Insomniac)
    } else if !bundles(game).is_empty() {
        Some(Layout::Stingray)
    } else {
        None
    }
}

/// Pliki katalogu, których nazwa (małymi literami) spełnia warunek; po nazwie.
fn files_in(dir: &Path, want: impl Fn(&str) -> bool) -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| want(&e.file_name().to_string_lossy().to_lowercase()))
        .filter_map(|e| Some((e.path(), e.metadata().ok().filter(|m| m.is_file())?.len())))
        .collect();
    out.sort();
    out
}

fn bundles(game: &Path) -> Vec<(PathBuf, u64)> {
    files_in(&game.join("data"), |n| n.starts_with("bundles.") && n.ends_with(".nxa"))
}

/// Pliki z shaderami układu (po nazwie).
fn sources(game: &Path, layout: Layout, with_exe: bool) -> Vec<(PathBuf, u64)> {
    match layout {
        Layout::SuckerPunch => files_in(&game.join(PSARCS), |n| n.ends_with(".psarc")),
        Layout::Insomniac => {
            let mut v: Vec<(PathBuf, u64)> = [r"d\material", r"d\materialgraph"]
                .iter()
                .map(|n| game.join(n))
                .chain(with_exe.then(|| game.join(EXE_RC)))
                .filter_map(|p| Some((p.clone(), std::fs::metadata(&p).ok().filter(|m| m.is_file())?.len())))
                .collect();
            v.dedup();
            v
        }
        Layout::Stingray => bundles(game),
    }
}

/// Otwarty plik źródłowy: spisy wspólne dla odczytów na żądanie.
struct Archive {
    path: PathBuf,
    dsar: Option<Dsar>,
    psarc: Option<Psarc>,
}

/// Odwiedzający źródła: (plik, wpis PSARC, nazwa puli, strumień, limit).
type Visitor<'a> = dyn FnMut(&Arc<Archive>, Option<usize>, &str, &mut dyn Source, &mut Meter) -> bool + 'a;

/// Wywołuje `f` (plik, wpis PSARC, nazwa puli, strumień) na każdym źródle
/// plików, póki zwraca true; `smallest`: najpierw najmniejsze (rozpoznanie).
/// Zwraca liczbę plików, których nie dało się otworzyć.
fn visit(
    files: &[(PathBuf, u64)],
    game: &Path,
    smallest: bool,
    meter: &mut Meter,
    f: &mut Visitor,
) -> usize {
    let mut order: Vec<&(PathBuf, u64)> = files.iter().collect();
    if smallest {
        order.sort_by_key(|x| x.1);
    }
    let mut failed = 0;
    for (path, _) in order {
        let rel = path.strip_prefix(game).unwrap_or(path).display().to_string();
        let Ok(file) = File::open(path) else {
            failed += 1;
            continue;
        };
        let dsar = Dsar::parse(&file, meter);
        let is_psarc = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psarc"));
        let plain = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
        if dsar.is_none() && !is_psarc && !plain {
            failed += 1;
            continue;
        }
        let mut arc = Archive { path: path.clone(), dsar, psarc: None };
        let mut names = Vec::new();
        if is_psarc {
            let Some(mut r) = Reader::new(&file, arc.dsar.as_ref()) else {
                failed += 1;
                continue;
            };
            let Some((p, n)) = Psarc::parse(&mut r, meter) else {
                failed += 1;
                continue;
            };
            arc.psarc = Some(p);
            names = n;
        }
        let arc = Arc::new(arc);
        let Some(mut r) = Reader::new(&file, arc.dsar.as_ref()) else {
            failed += 1;
            continue;
        };
        let go = match &arc.psarc {
            Some(p) => {
                let mut entries: Vec<usize> = (1..p.entries.len())
                    .filter(|&i| names[i].as_deref().is_some_and(|n| n.to_lowercase().ends_with(".xpps")))
                    .collect();
                if smallest {
                    entries.sort_by_key(|&i| p.entries[i].1);
                }
                let mut ooz = None;
                let mut go = true;
                for i in entries {
                    let pool = format!("{rel}:{}", names[i].as_deref().unwrap_or(""));
                    let mut src = EntrySource { psarc: p, reader: &mut r, ooz: &mut ooz, entry: i };
                    if !f(&arc, Some(i), &pool, &mut src, meter) {
                        go = false;
                        break;
                    }
                }
                go
            }
            None => f(&arc, None, &rel, &mut r, meter),
        };
        if !go {
            break;
        }
    }
    failed
}

// ------------------------------ root signature --------------------------------

/// Wiersz opisu 1.0: tablica `[0, widoczność, (typ zakresu, liczba, rejestr,
/// przestrzeń, przesunięcie)...]`, stałe `[1, widoczność, rejestr,
/// przestrzeń, liczba wartości]` albo główny deskryptor `[2|3|4, widoczność,
/// rejestr, przestrzeń, flagi (pomijane w 1.0)]`.
type Row = &'static [u32];

/// Stały opis: flagi i wiersze.
struct Rule {
    flags: u32,
    rows: &'static [Row],
}

const APPEND: u32 = u32::MAX;

/// Statyczne samplery (jak D3D12_STATIC_SAMPLER_DESC): filtr, adresowanie
/// U, V, W, bias (bity f32), anizotropia, porównanie, kolor brzegu, MinLOD,
/// MaxLOD (bity f32), rejestr, przestrzeń, widoczność.
type Sampler = [u32; 13];

/// Ghost of Tsushima (Steam 2215430, build 23879181).
const GOT_BUILD: &str = "Steam 2215430 build 23879181";
const GOT_CS_BARE: Rule = Rule { flags: 3072, rows: &[&[1, 0, 14, 0, 2], &[0, 0, 0, 1, 1, 1, 0]] };
const GOT_CS: Rule = Rule {
    flags: 3072,
    rows: &[
        &[1, 0, 14, 0, 2],
        &[0, 0, 0, 1, 1, 1, 0],
        &[0, 0, 2, 11, 0, 0, 0],
        &[0, 0, 0, 22, 0, 0, 0],
        &[0, 0, 3, 3, 0, 0, 0],
        &[0, 0, 1, 12, 0, 0, 0],
    ],
};
const GOT_GRAPHICS: Rule = Rule {
    flags: 3073,
    rows: &[
        &[1, 0, 14, 0, 2],
        &[0, 0, 0, 1, 1, 1, 0],
        &[0, 0, 2, 14, 0, 0, 0],
        &[0, 0, 0, 12, 0, 2, 0],
        &[0, 0, 0, 55, 0, 0, 0],
        &[0, 0, 3, 5, 0, 0, 0],
    ],
};
const GOT_SAMPLERS: [Sampler; 6] = [
    [0, 3, 3, 3, 0, 0, 0, 0, 0, 2_139_095_039, 9, 0, 0],
    [20, 3, 3, 3, 0, 0, 0, 0, 0, 2_139_095_039, 11, 0, 0],
    [21, 3, 3, 3, 0, 0, 0, 0, 0, 2_139_095_039, 12, 0, 0],
    [0, 1, 1, 1, 0, 0, 0, 0, 0, 2_139_095_039, 10, 0, 0],
    [21, 1, 1, 1, 0, 0, 0, 0, 0, 2_139_095_039, 13, 0, 0],
    [148, 3, 3, 3, 0, 0, 7, 0, 0, 2_139_095_039, 8, 0, 0],
];
/// Indeksy w `Set12::signatures` (Ghost of Tsushima).
const GOT_RS_CS_BARE: usize = 0;
const GOT_RS_CS: usize = 1;
const GOT_RS_GRAPHICS: usize = 2;

/// Ratchet & Clank: Rift Apart (Steam 1895880, build 23987138).
const RC_BUILD: &str = "Steam 1895880 build 23987138";
/// Wspólne tablice compute, teselacji i graficznej.
const RC_TABLES: &[Row] = &[&[0, 0, 0, 128, 0, 0, 0], &[0, 0, 2, 14, 0, 0, 0], &[0, 0, 1, 16, 0, 0, 0], &[0, 0, 3, 16, 0, 0, 0]];
const RC_DXBC: Rule = Rule {
    flags: 29,
    rows: &[
        &[2, 1, 0, 0, 0],
        &[2, 1, 1, 0, 0],
        &[2, 5, 0, 0, 0],
        &[2, 5, 1, 0, 0],
        &[2, 5, 2, 0, 0],
        &[2, 5, 3, 0, 0],
        &[2, 5, 4, 0, 0],
        &[0, 5, 0, 1, 0, 0, APPEND],
        &[0, 5, 0, 1, 1, 0, APPEND],
        &[0, 5, 0, 1, 2, 0, APPEND],
        &[0, 5, 0, 1, 3, 0, APPEND],
        &[0, 5, 3, 1, 0, 0, APPEND],
    ],
};
const RC_CS: Rule = Rule { flags: 62, rows: RC_TABLES };
const RC_TESS: Rule = Rule { flags: 17, rows: RC_TABLES };
const RC_GRAPHICS: Rule = Rule { flags: 29, rows: RC_TABLES };
/// Indeksy w `Set12::signatures` (Ratchet & Clank).
const RC_RS_DXBC: usize = 0;
const RC_RS_CS: usize = 1;
const RC_RS_GRAPHICS: usize = 3;

/// Serializuje opis w wersji 1.0 (zakresy z jawnym przesunięciem).
#[cfg(windows)]
fn serialize(rule: &Rule, samplers: &[Sampler]) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::Win32::Graphics::Direct3D12::*;
    let mut ranges: Vec<Vec<D3D12_DESCRIPTOR_RANGE>> = Vec::with_capacity(rule.rows.len());
    for r in rule.rows {
        let mut list = Vec::new();
        match r.first() {
            Some(0) => {
                for c in r.get(2..).unwrap_or(&[]).chunks(5) {
                    let &[t, count, base, space, offset] = c else {
                        return Err("wiersz tablicy: niepełny zakres / table row: incomplete range".into());
                    };
                    list.push(D3D12_DESCRIPTOR_RANGE {
                        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE(t as i32),
                        NumDescriptors: count,
                        BaseShaderRegister: base,
                        RegisterSpace: space,
                        OffsetInDescriptorsFromTableStart: offset,
                    });
                }
            }
            Some(1) if r.len() >= 5 => {}
            Some(2..=4) if r.len() >= 4 => {}
            _ => return Err("wiersz: zły / row: malformed".into()),
        }
        ranges.push(list);
    }
    let params: Vec<D3D12_ROOT_PARAMETER> = rule
        .rows
        .iter()
        .zip(&ranges)
        .map(|(r, range)| D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE(r[0] as i32),
            Anonymous: match r[0] {
                0 => D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: range.len() as u32,
                        pDescriptorRanges: range.as_ptr(),
                    },
                },
                1 => D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: r[2], RegisterSpace: r[3], Num32BitValues: r[4] },
                },
                _ => D3D12_ROOT_PARAMETER_0 { Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: r[2], RegisterSpace: r[3] } },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY(r[1] as i32),
        })
        .collect();
    let statics: Vec<D3D12_STATIC_SAMPLER_DESC> = samplers
        .iter()
        .map(|s| D3D12_STATIC_SAMPLER_DESC {
            Filter: D3D12_FILTER(s[0] as i32),
            AddressU: D3D12_TEXTURE_ADDRESS_MODE(s[1] as i32),
            AddressV: D3D12_TEXTURE_ADDRESS_MODE(s[2] as i32),
            AddressW: D3D12_TEXTURE_ADDRESS_MODE(s[3] as i32),
            MipLODBias: f32::from_bits(s[4]),
            MaxAnisotropy: s[5],
            ComparisonFunc: D3D12_COMPARISON_FUNC(s[6] as i32),
            BorderColor: D3D12_STATIC_BORDER_COLOR(s[7] as i32),
            MinLOD: f32::from_bits(s[8]),
            MaxLOD: f32::from_bits(s[9]),
            ShaderRegister: s[10],
            RegisterSpace: s[11],
            ShaderVisibility: D3D12_SHADER_VISIBILITY(s[12] as i32),
        })
        .collect();
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_0,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_0: D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: params.len() as u32,
                pParameters: if params.is_empty() { std::ptr::null() } else { params.as_ptr() },
                NumStaticSamplers: statics.len() as u32,
                pStaticSamplers: if statics.is_empty() { std::ptr::null() } else { statics.as_ptr() },
                Flags: D3D12_ROOT_SIGNATURE_FLAGS(rule.flags as i32),
            },
        },
    };
    let mut blob: Option<ID3DBlob> = None;
    let mut error: Option<ID3DBlob> = None;
    // SAFETY: wskaźniki w `desc` prowadzą do żyjących wektorów.
    let result = unsafe { D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut error)) };
    let bytes = |b: &ID3DBlob| -> Vec<u8> {
        // SAFETY: blob z D3D12.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec() }
    };
    match (result, blob) {
        (Ok(()), Some(b)) => Ok(bytes(&b)),
        _ => Err(error
            .map(|e| String::from_utf8_lossy(&bytes(&e)).trim_end_matches('\0').to_owned())
            .unwrap_or_else(|| "D3D12SerializeVersionedRootSignature".into())),
    }
}

/// Root signature układu w kolejności indeksów `*_RS_*`.
#[cfg(windows)]
fn signatures(layout: Layout) -> Result<Vec<Vec<u8>>, String> {
    match layout {
        Layout::SuckerPunch => [GOT_CS_BARE, GOT_CS, GOT_GRAPHICS].iter().map(|r| serialize(r, &GOT_SAMPLERS)).collect(),
        Layout::Insomniac => [RC_DXBC, RC_CS, RC_TESS, RC_GRAPHICS].iter().map(|r| serialize(r, &[])).collect(),
        Layout::Stingray => Err("Stingray to DirectX 11 / Stingray is DirectX 11".into()),
    }
}

#[cfg(not(windows))]
fn signatures(_layout: Layout) -> Result<Vec<Vec<u8>>, String> {
    Err("root signature tylko w Windows / root signatures need Windows".into())
}

/// Root signature shadera z puli, jak wybiera ją gra; `None` - etap, którego
/// pule nie łączą (HS, DS, GS: potoki z teselacją nie są znane z plików).
fn pick(layout: Layout, h: &Hit) -> Option<usize> {
    match (layout, h.kind) {
        (Layout::SuckerPunch, Kind::Compute) => Some(if h.bare { GOT_RS_CS_BARE } else { GOT_RS_CS }),
        (Layout::SuckerPunch, Kind::Vertex | Kind::Pixel) => Some(GOT_RS_GRAPHICS),
        (Layout::Insomniac, Kind::Compute | Kind::Vertex | Kind::Pixel) if !h.dxil => Some(RC_RS_DXBC),
        (Layout::Insomniac, Kind::Compute) => Some(RC_RS_CS),
        (Layout::Insomniac, Kind::Vertex | Kind::Pixel) => Some(RC_RS_GRAPHICS),
        _ => None,
    }
}

// -------------------------------- DirectX 12 ----------------------------------

/// Czy w najmniejszych źródłach jest kontener shadera (ograniczony odczyt).
fn has_shader(game: &Path, files: &[(PathBuf, u64)]) -> bool {
    let mut meter = Meter::new(DETECT_BUDGET);
    let mut found = false;
    visit(files, game, true, &mut meter, &mut |_, _, _, src, meter| {
        let _ = carve(src, meter, &mut |h, _| {
            found = h.kind != Kind::Other;
            found
        });
        !found && meter.left > 0
    });
    found
}

/// Ghost of Tsushima albo Ratchet & Clank: Rift Apart - po układzie folderów
/// i kontenerze shadera w najmniejszych źródłach (wynik zapamiętany).
pub fn detect(game: &Path) -> Option<Detected> {
    let layout = layout(game).filter(|l| *l != Layout::Stingray)?;
    let files = sources(game, layout, false);
    if files.is_empty() || !cached_detect("nixxes", game, &files, || has_shader(game, &files)) {
        return None;
    }
    let exe = if layout == Layout::SuckerPunch { EXE_GOT } else { EXE_RC };
    Some(Detected {
        engine: "Nixxes",
        version: Some(layout.name().into()),
        api: "DirectX 12",
        files: sources(game, layout, true),
        exe: game.join(exe).is_file().then(|| exe.to_string()),
    })
}

/// Bajty shadera czytane na żądanie (spisy wspólne, plik otwierany przy
/// każdym odczycie; sprawdzane tożsamością kontenera).
fn lazy(arc: Arc<Archive>, entry: Option<usize>, at: u64, size: u32, id: [u8; 16]) -> Bytes {
    Bytes::Lazy(Arc::new(move || {
        let file = File::open(&arc.path).ok()?;
        let mut r = Reader::new(&file, arc.dsar.as_ref())?;
        let mut b = vec![0u8; size as usize];
        match (entry, &arc.psarc) {
            (Some(e), Some(p)) => p.read(&mut r, &mut None, e, at, &mut b).ok()?,
            _ => r.read(at, &mut b).ok()?,
        }
        let ok = Container::parse(&b).is_some_and(|c| c.bytes.len() == b.len() && c.identity() == id);
        ok.then_some(b)
    }))
}

/// Źródło (paczka albo strumień) z jego kontenerami.
struct Unit {
    arc: Arc<Archive>,
    entry: Option<usize>,
    hits: Vec<Hit>,
}

/// Shadery DirectX 12 gry: pule po źródłach, rozdzielone według root
/// signature, którą gra wybrałaby dla ich etapów.
pub fn index(game: &Path, say: &dyn Fn(String)) -> Result<Set12, String> {
    let layout = layout(game)
        .filter(|l| *l != Layout::Stingray)
        .ok_or("to nie gra Nixxes DirectX 12 / not a Nixxes DirectX 12 game")?;
    let files = sources(game, layout, true);
    if files.is_empty() {
        return Err(format!("{}: brak plików z shaderami / no shader files", layout.name()));
    }
    let mut set = Set12 { signatures: signatures(layout)?, ..Set12::default() };
    let mut meter = Meter::new(u64::MAX);
    let mut units: Vec<Unit> = Vec::new();
    let (mut containers, mut unreadable, mut over_cap) = (0usize, 0usize, 0usize);
    // DXBC z bliźniakiem DXIL tuż za nim.
    let mut twins: HashSet<([u8; 16], u32)> = HashSet::new();
    let total = files.len();
    let mut done_files = HashSet::new();
    let failed = visit(&files, game, false, &mut meter, &mut |arc, entry, _, src, meter| {
        if done_files.insert(arc.path.clone()) {
            say(format!("pliki / files: {}/{total}", done_files.len()));
        }
        let mut hits: Vec<Hit> = Vec::new();
        let r = carve(src, meter, &mut |h, _| {
            containers += 1;
            if containers > MAX_HITS {
                over_cap += 1;
                return true;
            }
            if let Some(prev) = hits.last() {
                if h.dxil
                    && !prev.dxil
                    && prev.at + u64::from(prev.size) == h.at
                    && prev.kind == h.kind
                    && prev.igsh == Some((prev.size, h.size))
                {
                    twins.insert(prev.key());
                }
            }
            hits.push(h.clone());
            false
        });
        if r.is_err() {
            unreadable += 1;
        }
        if !hits.is_empty() {
            units.push(Unit { arc: arc.clone(), entry, hits });
        }
        true
    });
    let mut ids: HashMap<([u8; 16], u32), usize> = HashMap::new();
    let (mut dropped, mut libraries, mut other_stages, mut kept_dxbc) = (HashSet::new(), 0usize, 0usize, HashSet::new());
    for u in &units {
        let mut by_rs: Vec<(usize, Vec<usize>)> = Vec::new();
        for h in &u.hits {
            if twins.contains(&h.key()) {
                dropped.insert(h.key());
                continue;
            }
            if h.kind == Kind::Other {
                libraries += 1;
                continue;
            }
            let Some(rs) = pick(layout, h) else {
                other_stages += 1;
                continue;
            };
            let id = match ids.get(&h.key()) {
                Some(&id) => id,
                None if set.shaders.len() < MAX_SHADERS => {
                    set.shaders.push(lazy(u.arc.clone(), u.entry, h.at, h.size, h.id));
                    ids.insert(h.key(), set.shaders.len() - 1);
                    set.shaders.len() - 1
                }
                None => {
                    over_cap += 1;
                    continue;
                }
            };
            if !h.dxil {
                kept_dxbc.insert(h.key());
            }
            match by_rs.iter_mut().find(|(r, _)| *r == rs) {
                Some((_, v)) => {
                    if !v.contains(&id) {
                        v.push(id);
                    }
                }
                None => by_rs.push((rs, vec![id])),
            }
        }
        for (rs, members) in by_rs {
            set.pools.push((members, Some(rs)));
        }
    }
    if set.shaders.is_empty() {
        return Err(format!(
            "{}: brak shaderów w archiwach (pliki nieczytelne: {}) / no shaders in the archives (unreadable files: {})",
            layout.name(),
            failed + unreadable,
            failed + unreadable
        ));
    }
    let build = if layout == Layout::SuckerPunch { GOT_BUILD } else { RC_BUILD };
    set.notes.push(format!(
        "{}: kontenery / containers: {containers}, shadery / shaders: {}, pule / pools: {}, rozpakowane / decompressed: {:.2} GB",
        layout.name(),
        set.shaders.len(),
        set.pools.len(),
        meter.used as f64 / 1e9
    ));
    set.notes.push(format!(
        "root signature ze stałych reguł znanej wersji gry ({build}); aktualizacja gry może je zmienić / root signatures come from fixed rules of a known game build ({build}); a game update may change them"
    ));
    if !dropped.is_empty() || !kept_dxbc.is_empty() {
        set.notes.push(format!(
            "pominięte bliźniaki DXBC shaderów DXIL / DXBC twins of DXIL shaders left out: {}, DXBC bez bliźniaka (zachowane) / DXBC without a twin (kept): {}",
            dropped.len(),
            kept_dxbc.len()
        ));
    }
    if libraries > 0 {
        set.notes.push(format!("pominięte biblioteki ray tracingu / ray tracing libraries skipped: {libraries}"));
    }
    if other_stages > 0 {
        set.notes.push(format!(
            "pominięte shadery HS, DS, GS (pliki nie łączą potoków z teselacją) / HS, DS, GS shaders skipped (the files don't pair tessellation pipelines): {other_stages}"
        ));
    }
    if failed + unreadable > 0 {
        set.notes.push(format!(
            "warning pliki albo wpisy nieczytelne (shadery sprzed błędu zostają) / unreadable files or entries (shaders before the failure are kept): {}",
            failed + unreadable
        ));
    }
    if over_cap > 0 {
        set.notes.push(format!("warning kontenery ponad limit, pominięte / containers over the cap, skipped: {over_cap}"));
    }
    Ok(set)
}

// -------------------------------- DirectX 11 ----------------------------------

/// HELLDIVERS 2: paczka `data\bundles.*.nxa` otwiera się jako DSAR.
pub fn detect_stingray(game: &Path) -> Option<Detected> {
    if layout(game)? != Layout::Stingray {
        return None;
    }
    let files = bundles(game);
    let smallest = files.iter().min_by_key(|f| f.1)?;
    let mut meter = Meter::new(DETECT_BUDGET);
    Dsar::parse(&File::open(&smallest.0).ok()?, &mut meter)?;
    let exe = [game.join(EXE_HD2), game.join("bin").join(EXE_HD2)].iter().any(|p| p.is_file());
    Some(Detected {
        engine: "Stingray",
        version: Some(Layout::Stingray.name().into()),
        api: "DirectX 11",
        files,
        exe: exe.then(|| EXE_HD2.to_string()),
    })
}

/// Każdy kontener DXBC (SM 5.0) z paczek; zwraca ich liczbę.
pub fn for_each_shader_stingray(
    game: &Path,
    progress: &mut dyn FnMut(usize, usize),
    each: &mut dyn FnMut(&[u8]),
) -> Result<usize, String> {
    let files = bundles(game);
    let total = files.len();
    progress(0, total);
    let (mut count, mut done, mut errors) = (0usize, 0usize, Vec::new());
    let mut meter = Meter::new(u64::MAX);
    let failed = visit(&files, game, false, &mut meter, &mut |_, _, name, src, meter| {
        if let Err(e) = carve(src, meter, &mut |h, c| {
            if !h.dxil && h.kind != Kind::Other {
                each(c.bytes);
                count += 1;
            }
            false
        }) {
            errors.push(format!("{name}: {e}"));
        }
        done += 1;
        progress(done, total);
        true
    });
    if count == 0 {
        let why = errors.first().map(|e| format!(" ({e})")).unwrap_or_default();
        return Err(format!(
            "brak shaderów DirectX 11 w data\\bundles.*.nxa (nieczytelne: {}) / no DirectX 11 shaders in data\\bundles.*.nxa (unreadable: {}){why}",
            failed + errors.len(),
            failed + errors.len()
        ));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::super::container::tests::{build, dxil};
    use super::*;

    /// Plik DSAR z bloków (kodek, bajty po rozpakowaniu).
    fn dsar(blocks: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let header = 32 + 32 * blocks.len();
        let mut table = Vec::new();
        let mut data = Vec::new();
        let mut start = 0u64;
        for (codec, plain) in blocks {
            let stored = match *codec {
                LZ4 => crate::lz4::compress_literals(plain),
                STORED => plain.clone(),
                _ => Vec::new(),
            };
            table.extend_from_slice(&start.to_le_bytes());
            table.extend_from_slice(&((header + data.len()) as u64).to_le_bytes());
            table.extend_from_slice(&(plain.len() as u32).to_le_bytes());
            table.extend_from_slice(&(stored.len() as u32).to_le_bytes());
            table.push(*codec);
            table.extend_from_slice(&[0; 7]);
            data.extend_from_slice(&stored);
            start += plain.len() as u64;
        }
        let mut f = b"DSAR".to_vec();
        f.extend_from_slice(&[1, 0, 1, 0]);
        f.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
        f.extend_from_slice(&(header as u32).to_le_bytes());
        f.extend_from_slice(&start.to_le_bytes());
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&table);
        f.extend_from_slice(&data);
        f
    }

    /// Dzieli bajty na bloki LZ4 po `n` bajtów.
    fn lz4_blocks(b: &[u8], n: usize) -> Vec<(u8, Vec<u8>)> {
        b.chunks(n).map(|c| (LZ4, c.to_vec())).collect()
    }

    fn adler32(b: &[u8]) -> u32 {
        let (mut a, mut s) = (1u32, 0u32);
        for &x in b {
            a = (a + u32::from(x)) % 65_521;
            s = (s + a) % 65_521;
        }
        s << 16 | a
    }

    /// Strumień zlib z jednym blokiem zapisanym wprost.
    fn zlib_stored(b: &[u8]) -> Vec<u8> {
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
        z.extend_from_slice(&adler32(b).to_be_bytes());
        z
    }

    /// Archiwum PSARC (bloki 1 KB: pełne bloki co drugi przez zlib).
    fn psarc(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        const BS: usize = 1024;
        let names: Vec<u8> = entries.iter().rev().map(|e| e.0).collect::<Vec<_>>().join("\n").into_bytes();
        let all: Vec<(Option<&str>, &[u8])> =
            std::iter::once((None, &names[..])).chain(entries.iter().map(|(n, d)| (Some(*n), &d[..]))).collect();
        let (mut sizes, mut data, mut rows) = (Vec::new(), Vec::new(), Vec::new());
        for (name, bytes) in &all {
            let first = sizes.len() as u32;
            let at = data.len();
            for (k, c) in bytes.chunks(BS).enumerate() {
                let stored = if k % 2 == 1 { zlib_stored(c) } else { c.to_vec() };
                sizes.push(if stored.len() == BS { 0 } else { stored.len() as u16 });
                data.extend_from_slice(&stored);
            }
            rows.push((name.map_or([0u8; 16], |n| md5(n.as_bytes())), first, bytes.len() as u64, at));
        }
        let toc_len = 32 + 30 * rows.len() + 2 * sizes.len();
        let mut f = b"PSAR".to_vec();
        f.extend_from_slice(&[0, 1, 0, 4]);
        f.extend_from_slice(b"zlib");
        for v in [toc_len as u32, 30, rows.len() as u32, BS as u32, 0] {
            f.extend_from_slice(&v.to_be_bytes());
        }
        for (md, first, size, at) in rows {
            f.extend_from_slice(&md);
            f.extend_from_slice(&first.to_be_bytes());
            f.extend_from_slice(&size.to_be_bytes()[3..]);
            f.extend_from_slice(&((toc_len + at) as u64).to_be_bytes()[3..]);
        }
        for s in sizes {
            f.extend_from_slice(&s.to_be_bytes());
        }
        f.extend_from_slice(&data);
        f
    }

    /// Część PSV0 z liczbą zasobów.
    fn psv0(resources: u32) -> Vec<u8> {
        let mut b = 24u32.to_le_bytes().to_vec();
        b.extend_from_slice(&[0; 24]);
        b.extend_from_slice(&resources.to_le_bytes());
        b
    }

    fn dxil_shader(kind: u32, salt: u8, resources: u32) -> Vec<u8> {
        build(&[(b"DXIL", dxil(kind)), (b"PSV0", psv0(resources)), (b"SALT", vec![salt; 8])])
    }

    fn sm5(kind: u32, salt: u8) -> Vec<u8> {
        let mut p = ((kind << 16) | 0x50).to_le_bytes().to_vec();
        p.extend_from_slice(&[salt; 12]);
        build(&[(b"SHEX", p)])
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tc-nixxes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn md5_matches_known_digests() {
        let hex = |b: [u8; 16]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(md5(&[b'a'; 100])), "36a92cc94a9e0fa21f625f8bfb007adf");
    }

    #[test]
    fn reads_dsar_blocks_gaps_and_carves() {
        let a = dxil_shader(5, 1, 0);
        let b = sm5(1, 2);
        let mut stream = vec![7u8; 100];
        stream.extend_from_slice(&a);
        stream.extend_from_slice(&[9u8; 50]);
        let gap_at = stream.len();
        let mut blocks = lz4_blocks(&stream, 70);
        blocks.push((GAP, vec![0u8; 40]));
        let mut tail = vec![3u8; 33];
        tail.extend_from_slice(&b);
        blocks.push((STORED, tail.clone()));
        let dir = temp("dsar");
        let path = dir.join("x.nxa");
        std::fs::write(&path, dsar(&blocks)).unwrap();
        let file = File::open(&path).unwrap();
        let d = Dsar::parse(&file, &mut Meter::new(u64::MAX)).unwrap();
        let mut r = Reader::new(&file, Some(&d)).unwrap();
        let mut whole = vec![0u8; r.len() as usize];
        r.read(0, &mut whole).unwrap();
        assert_eq!(&whole[..gap_at], &stream[..]);
        assert!(whole[gap_at..gap_at + 40].iter().all(|&x| x == 0));
        assert_eq!(&whole[gap_at + 40..], &tail[..]);
        assert!(r.read(whole.len() as u64 - 1, &mut [0u8; 2]).is_err());
        let mut found = Vec::new();
        carve(&mut r, &mut Meter::new(u64::MAX), &mut |h, c| {
            found.push((h.kind, h.dxil, h.bare, c.bytes.to_vec()));
            false
        })
        .unwrap();
        assert_eq!(found, vec![(Kind::Compute, true, true, a), (Kind::Vertex, false, false, b)]);
        // Ucięty albo zepsuty plik nie panikuje.
        let raw = std::fs::read(&path).unwrap();
        for cut in (0..raw.len()).step_by(13) {
            std::fs::write(&path, &raw[..cut]).unwrap();
            let f = File::open(&path).unwrap();
            if let Some(d) = Dsar::parse(&f, &mut Meter::new(u64::MAX)) {
                let mut r = Reader::new(&f, Some(&d)).unwrap();
                let _ = carve(&mut r, &mut Meter::new(u64::MAX), &mut |_, _| false);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carves_across_window_edge() {
        // Kontener przecinający koniec okna - czytany osobno.
        struct Mem(Vec<u8>);
        impl Source for Mem {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read(&mut self, from: u64, out: &mut [u8]) -> Result<(), String> {
                let s = self.0.get(from as usize..from as usize + out.len()).ok_or("end")?;
                out.copy_from_slice(s);
                Ok(())
            }
        }
        let c = sm5(0, 4);
        let mut b = vec![0u8; WINDOW as usize - 20];
        b.extend_from_slice(&c);
        b.extend_from_slice(&[1; 10]);
        let mut n = 0;
        carve(&mut Mem(b), &mut Meter::new(u64::MAX), &mut |h, _| {
            n += 1;
            assert_eq!(h.at, WINDOW - 20);
            false
        })
        .unwrap();
        assert_eq!(n, 1);
    }

    /// Ghost of Tsushima: PSARC w DSAR, paczki .xpps.
    fn got_game(name: &str) -> (PathBuf, Vec<Vec<u8>>) {
        let shaders = vec![
            dxil_shader(1, 1, 2), // VS
            dxil_shader(0, 2, 3), // PS
            dxil_shader(5, 3, 0), // CS bez zasobów
            dxil_shader(5, 4, 5), // CS
            dxil_shader(3, 5, 1), // HS - pominięty
        ];
        let mut pkg = vec![0xAAu8; 300];
        for s in &shaders {
            pkg.extend_from_slice(s);
            pkg.extend_from_slice(&[0x55; 77]);
        }
        let archive = psarc(&[("shaders/a.xpps", pkg), ("other/b.bin", sm5(1, 9))]);
        let dir = temp(name);
        std::fs::create_dir_all(dir.join(PSARCS)).unwrap();
        std::fs::write(dir.join(PSARCS).join("a.psarc"), dsar(&lz4_blocks(&archive, 500))).unwrap();
        (dir, shaders)
    }

    #[test]
    fn reads_psarc_inside_dsar() {
        let (dir, shaders) = got_game("got-read");
        assert_eq!(layout(&dir), Some(Layout::SuckerPunch));
        let files = sources(&dir, Layout::SuckerPunch, true);
        assert!(has_shader(&dir, &files));
        let mut seen = Vec::new();
        visit(&files, &dir, false, &mut Meter::new(u64::MAX), &mut |_, entry, name, src, meter| {
            seen.push((entry, name.replace('\\', "/")));
            let mut n = 0;
            carve(src, meter, &mut |_, _| {
                n += 1;
                false
            })
            .unwrap();
            assert_eq!(n, shaders.len());
            true
        });
        // Tylko paczka .xpps.
        assert_eq!(seen, vec![(Some(1), "cache_pc/psarc/a.psarc:shaders/a.xpps".to_string())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn indexes_got_pools_per_root_signature() {
        let (dir, shaders) = got_game("got-index");
        let set = index(&dir, &|_| {}).unwrap();
        assert_eq!(set.signatures.len(), 3);
        assert_eq!(set.shaders.len(), 4);
        let mut pools: Vec<(usize, usize)> = set.pools.iter().map(|(m, rs)| (rs.unwrap(), m.len())).collect();
        pools.sort();
        assert_eq!(pools, vec![(GOT_RS_CS_BARE, 1), (GOT_RS_CS, 1), (GOT_RS_GRAPHICS, 2)]);
        let bare = &set.pools.iter().find(|p| p.1 == Some(GOT_RS_CS_BARE)).unwrap().0;
        assert_eq!(set.shaders[bare[0]].get().unwrap(), shaders[2]);
        assert!(set.notes.iter().any(|n| n.contains("23879181")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn indexes_rc_without_dxbc_twins() {
        let dir = temp("rc");
        std::fs::create_dir_all(dir.join("d")).unwrap();
        std::fs::write(dir.join("toc"), b"toc").unwrap();
        let twin_dxbc = sm5(0, 1);
        let twin_dxil = dxil_shader(0, 1, 1);
        let lone = sm5(1, 2);
        let cs = dxil_shader(5, 3, 1);
        let mut m = vec![0u8; 64];
        m.extend_from_slice(b"IGSH");
        m.extend_from_slice(&(twin_dxbc.len() as u32).to_le_bytes());
        m.extend_from_slice(&(twin_dxil.len() as u32).to_le_bytes());
        m.extend_from_slice(&twin_dxbc);
        m.extend_from_slice(&twin_dxil);
        m.extend_from_slice(&[0; 16]);
        m.extend_from_slice(&lone);
        std::fs::write(dir.join(r"d\material"), dsar(&lz4_blocks(&m, 128))).unwrap();
        std::fs::write(dir.join(r"d\materialgraph"), dsar(&lz4_blocks(&cs, 1000))).unwrap();
        assert_eq!(layout(&dir), Some(Layout::Insomniac));
        let set = index(&dir, &|_| {}).unwrap();
        assert_eq!(set.signatures.len(), 4);
        assert_eq!(set.shaders.len(), 3);
        let mut pools: Vec<(usize, usize)> = set.pools.iter().map(|(m, rs)| (rs.unwrap(), m.len())).collect();
        pools.sort();
        assert_eq!(pools, vec![(RC_RS_DXBC, 1), (RC_RS_CS, 1), (RC_RS_GRAPHICS, 1)]);
        let got: Vec<Vec<u8>> = set.shaders.iter().map(|s| s.get().unwrap()).collect();
        assert!(got.contains(&twin_dxil) && got.contains(&lone) && !got.contains(&twin_dxbc));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stingray_bundles_give_dxbc() {
        let dir = temp("hd2");
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let (vs, ps) = (sm5(1, 1), sm5(0, 2));
        let mut b = vec![1u8; 1000];
        b.extend_from_slice(&vs);
        b.extend_from_slice(&dxil_shader(1, 3, 0));
        b.extend_from_slice(&ps);
        std::fs::write(dir.join("data").join("bundles.00.nxa"), dsar(&lz4_blocks(&b, 256))).unwrap();
        assert!(detect(&dir).is_none());
        let d = detect_stingray(&dir).unwrap();
        assert_eq!((d.engine, d.api), ("Stingray", "DirectX 11"));
        let mut got = Vec::new();
        let mut last = (0, 0);
        let n = for_each_shader_stingray(&dir, &mut |a, b| last = (a, b), &mut |c| got.push(c.to_vec())).unwrap();
        assert_eq!((n, got, last), (2, vec![vs, ps], (1, 1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn serializes_rule_root_signatures() {
        for layout in [Layout::SuckerPunch, Layout::Insomniac] {
            let sigs = signatures(layout).unwrap();
            assert!(sigs.iter().all(|s| s.len() > 24), "{layout:?}");
        }
    }
}
