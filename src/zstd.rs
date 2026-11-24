//! Dekoder Zstandard (format z RFC 8878) - zrzuty shaderów Dagor Engine
//! (ze słownikiem) i paczki Unreal Engine. Tylko dekompresja.
//!
//! Ramka: nagłówek, bloki (surowy, RLE, skompresowany), opcjonalna suma
//! kontrolna (pomijana). Blok skompresowany: literały (surowe, RLE albo
//! kodowane Huffmanem w 1 lub 4 strumieniach) i sekwencje (długość literałów,
//! przesunięcie, długość dopasowania) kodowane FSE. Słownik: zawartość i
//! opcjonalnie tabele entropii oraz początkowe przesunięcia powtórzeń.

const MAGIC: u32 = 0xFD2F_B528;
const DICT_MAGIC: u32 = 0xEC30_A437;
const MAX_BLOCK: usize = 128 * 1024;

const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    0x80, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 0x10000,
];
const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 0x83, 0x103, 0x203,
    0x403, 0x803, 0x1003, 0x2003, 0x4003, 0x8003, 0x10003,
];
const LL_DEFAULT: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
const ML_DEFAULT: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_DEFAULT: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

fn err(what: &str) -> String {
    format!("zstd: {what}")
}

// ------------------------------- strumienie bitów ----------------------------

fn load64(data: &[u8], byte: usize) -> u64 {
    let mut b = [0u8; 8];
    if byte < data.len() {
        let n = (data.len() - byte).min(8);
        b[..n].copy_from_slice(&data[byte..byte + n]);
    }
    u64::from_le_bytes(b)
}

/// Bity `start..start+n` (bit i = bit i%8 bajtu i/8), n <= 56.
fn bits_at(data: &[u8], start: usize, n: u32) -> u64 {
    if n == 0 {
        return 0;
    }
    (load64(data, start / 8) >> (start % 8)) & ((1u64 << n) - 1)
}

/// Strumień czytany od początku, od najmłodszych bitów (opisy tabel FSE).
struct Forward<'a> {
    data: &'a [u8],
    bit: usize,
}

impl Forward<'_> {
    fn peek(&self, n: u32) -> u32 {
        bits_at(self.data, self.bit, n) as u32
    }
    fn skip(&mut self, n: u32) -> Result<(), String> {
        self.bit += n as usize;
        if self.bit > self.data.len() * 8 {
            return Err(err("ucięty opis tabeli"));
        }
        Ok(())
    }
    fn read(&mut self, n: u32) -> Result<u32, String> {
        let v = self.peek(n);
        self.skip(n)?;
        Ok(v)
    }
    fn bytes_used(&self) -> usize {
        self.bit.div_ceil(8)
    }
}

/// Strumień czytany od końca (Huffman, FSE): ostatni bajt zawiera znacznik
/// (najstarszy ustawiony bit), bity czytamy od najstarszych.
struct Backward<'a> {
    data: &'a [u8],
    /// Liczba jeszcze nieprzeczytanych bitów (ujemna - czytanie poza początek).
    pos: isize,
}

impl<'a> Backward<'a> {
    fn new(data: &'a [u8]) -> Result<Self, String> {
        let last = *data.last().ok_or_else(|| err("pusty strumień"))?;
        if last == 0 {
            return Err(err("brak znacznika końca strumienia"));
        }
        let marker = 7 - last.leading_zeros() as isize;
        Ok(Self {
            data,
            pos: (data.len() as isize - 1) * 8 + marker,
        })
    }

    fn peek(&self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        let start = self.pos - n as isize;
        if start >= 0 {
            bits_at(self.data, start as usize, n)
        } else {
            let avail = n as isize + start;
            if avail <= 0 {
                0
            } else {
                bits_at(self.data, 0, avail as u32) << (-start)
            }
        }
    }

    fn read(&mut self, n: u32) -> u64 {
        let v = self.peek(n);
        self.pos -= n as isize;
        v
    }

    fn overflow(&self) -> bool {
        self.pos < 0
    }

    fn done(&self) -> bool {
        self.pos == 0
    }
}

// ------------------------------------ FSE ------------------------------------

#[derive(Clone, Copy, Default)]
struct FseEntry {
    symbol: u8,
    bits: u8,
    base: u16,
}

#[derive(Clone)]
struct Fse {
    log: u32,
    table: Vec<FseEntry>,
}

impl Fse {
    /// Tabela dekodowania z rozkładu (prawdopodobieństwa; -1 = "mniej niż 1").
    fn from_norm(norm: &[i16], log: u32) -> Result<Fse, String> {
        let size = 1usize << log;
        let mut table = vec![FseEntry::default(); size];
        let mut next = vec![0u32; norm.len()];
        let mut high = size - 1;
        for (s, &p) in norm.iter().enumerate() {
            if p == -1 {
                table[high].symbol = s as u8;
                high = high.wrapping_sub(1);
                next[s] = 1;
            } else {
                next[s] = p.max(0) as u32;
            }
        }
        let step = (size >> 1) + (size >> 3) + 3;
        let mask = size - 1;
        let mut pos = 0usize;
        for (s, &p) in norm.iter().enumerate() {
            for _ in 0..p.max(0) {
                table[pos].symbol = s as u8;
                loop {
                    pos = (pos + step) & mask;
                    if pos <= high || high == usize::MAX {
                        break;
                    }
                }
            }
        }
        if pos != 0 {
            return Err(err("zły rozkład FSE"));
        }
        for e in table.iter_mut() {
            let s = e.symbol as usize;
            let n = next[s];
            next[s] += 1;
            let bits = log - (31 - n.leading_zeros());
            e.bits = bits as u8;
            e.base = ((n << bits) as usize - size) as u16;
        }
        Ok(Fse { log, table })
    }

    /// Tabela o jednym symbolu (tryb RLE).
    fn rle(symbol: u8) -> Fse {
        Fse {
            log: 0,
            table: vec![FseEntry {
                symbol,
                bits: 0,
                base: 0,
            }],
        }
    }

    /// Opis tabeli z danych; zwraca tabelę i liczbę zużytych bajtów.
    fn read(data: &[u8], max_log: u32, max_symbol: usize) -> Result<(Fse, usize), String> {
        let mut f = Forward { data, bit: 0 };
        let log = f.read(4)? + 5;
        if log > max_log {
            return Err(err("za duża dokładność tabeli FSE"));
        }
        let mut remaining = (1i32 << log) + 1;
        let mut threshold = 1i32 << log;
        let mut nbits = log + 1;
        let mut norm: Vec<i16> = Vec::new();
        while remaining > 1 {
            if norm.len() > max_symbol {
                return Err(err("za dużo symboli FSE"));
            }
            let max = (2 * threshold - 1) - remaining;
            let raw = f.peek(nbits) as i32;
            let mut count;
            if (raw & (threshold - 1)) < max {
                count = raw & (threshold - 1);
                f.skip(nbits - 1)?;
            } else {
                count = raw & (2 * threshold - 1);
                if count >= threshold {
                    count -= max;
                }
                f.skip(nbits)?;
            }
            count -= 1;
            remaining -= count.abs();
            norm.push(count as i16);
            if count == 0 {
                // Kolejne zera: po 2 bity, 3 = czytaj dalej.
                loop {
                    let repeat = f.read(2)?;
                    for _ in 0..repeat {
                        norm.push(0);
                    }
                    if repeat != 3 {
                        break;
                    }
                }
            }
            while remaining < threshold && threshold > 1 {
                nbits -= 1;
                threshold >>= 1;
            }
        }
        if remaining != 1 || norm.len() > max_symbol + 1 {
            return Err(err("zły opis tabeli FSE"));
        }
        Ok((Fse::from_norm(&norm, log)?, f.bytes_used()))
    }
}

/// Stan dekodera FSE.
struct State<'t> {
    table: &'t Fse,
    state: usize,
}

impl<'t> State<'t> {
    fn new(table: &'t Fse, bits: &mut Backward) -> Self {
        let state = bits.read(table.log) as usize;
        Self { table, state }
    }
    fn symbol(&self) -> u8 {
        self.table.table[self.state].symbol
    }
    fn update(&mut self, bits: &mut Backward) {
        let e = self.table.table[self.state];
        self.state = e.base as usize + bits.read(e.bits as u32) as usize;
    }
}

// ---------------------------------- Huffman ----------------------------------

#[derive(Clone)]
struct Huffman {
    max_bits: u32,
    /// (symbol, liczba bitów) dla każdego `max_bits`-bitowego prefiksu.
    table: Vec<(u8, u8)>,
}

impl Huffman {
    /// Opis drzewa; zwraca tabelę i liczbę zużytych bajtów.
    fn read(data: &[u8]) -> Result<(Huffman, usize), String> {
        let header = *data.first().ok_or_else(|| err("brak opisu drzewa Huffmana"))? as usize;
        let mut weights: Vec<u8> = Vec::new();
        let used;
        if header < 128 {
            // Wagi kodowane FSE (dwa przeplatane stany).
            let body = data.get(1..1 + header).ok_or_else(|| err("ucięty opis Huffmana"))?;
            let (fse, n) = Fse::read(body, 6, 255)?;
            let mut bits = Backward::new(body.get(n..).ok_or_else(|| err("ucięte wagi Huffmana"))?)?;
            let mut s1 = State::new(&fse, &mut bits);
            let mut s2 = State::new(&fse, &mut bits);
            loop {
                if weights.len() >= 255 {
                    return Err(err("za dużo wag Huffmana"));
                }
                weights.push(s1.symbol());
                s1.update(&mut bits);
                if bits.overflow() {
                    weights.push(s2.symbol());
                    break;
                }
                weights.push(s2.symbol());
                s2.update(&mut bits);
                if bits.overflow() {
                    weights.push(s1.symbol());
                    break;
                }
            }
            used = 1 + header;
        } else {
            // Wagi wprost: po 4 bity.
            let count = header - 127;
            let bytes = data.get(1..1 + count.div_ceil(2)).ok_or_else(|| err("ucięte wagi Huffmana"))?;
            for i in 0..count {
                let b = bytes[i / 2];
                weights.push(if i % 2 == 0 { b >> 4 } else { b & 0x0F });
            }
            used = 1 + count.div_ceil(2);
        }
        Ok((Self::from_weights(&weights)?, used))
    }

    fn from_weights(weights: &[u8]) -> Result<Huffman, String> {
        let total: u32 = weights.iter().filter(|&&w| w > 0).map(|&w| 1u32 << (w - 1)).sum();
        if total == 0 {
            return Err(err("puste drzewo Huffmana"));
        }
        // Ostatnia waga dopełnia sumę do potęgi 2.
        let max_bits = 32 - total.leading_zeros();
        let left = (1u32 << max_bits) - total;
        if !left.is_power_of_two() {
            return Err(err("złe wagi Huffmana"));
        }
        let mut all = weights.to_vec();
        all.push((left.trailing_zeros() + 1) as u8);
        if max_bits > 11 {
            return Err(err("za długie kody Huffmana"));
        }
        let mut rank_start = [0u32; 13];
        let mut next = 0u32;
        for w in 1..=max_bits as usize {
            rank_start[w] = next;
            next += all.iter().filter(|&&x| x as usize == w).count() as u32 * (1 << (w - 1));
        }
        let mut table = vec![(0u8, 0u8); 1 << max_bits];
        for (s, &w) in all.iter().enumerate() {
            if w == 0 {
                continue;
            }
            let len = 1u32 << (w - 1);
            let start = rank_start[w as usize];
            for e in &mut table[start as usize..(start + len) as usize] {
                *e = (s as u8, (max_bits + 1 - w as u32) as u8);
            }
            rank_start[w as usize] += len;
        }
        Ok(Huffman { max_bits, table })
    }

    /// Dekoduje `count` symboli z jednego strumienia.
    fn decode(&self, stream: &[u8], count: usize, out: &mut Vec<u8>) -> Result<(), String> {
        let mut bits = Backward::new(stream)?;
        for _ in 0..count {
            let (s, n) = self.table[bits.peek(self.max_bits) as usize];
            out.push(s);
            bits.read(n as u32);
        }
        if !bits.done() {
            return Err(err("zły strumień Huffmana"));
        }
        Ok(())
    }
}

// --------------------------------- dekoder -----------------------------------

/// Stan entropii przenoszony między blokami ramki (i ze słownika).
#[derive(Clone, Default)]
struct Entropy {
    huffman: Option<Huffman>,
    ll: Option<Fse>,
    of: Option<Fse>,
    ml: Option<Fse>,
    reps: [u32; 3],
}

/// Dekoder Zstandard, opcjonalnie ze słownikiem.
pub struct Decoder {
    content: Vec<u8>,
    entropy: Entropy,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Rozpakowuje dane bez słownika; wynik nie większy niż `max` bajtów.
pub fn decompress(src: &[u8], max: usize) -> Result<Vec<u8>, String> {
    Decoder::new().decompress(src, max)
}

fn u32_le(data: &[u8], at: usize) -> Result<u32, String> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| err("ucięte dane"))
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            content: Vec::new(),
            entropy: Entropy {
                reps: [1, 4, 8],
                ..Default::default()
            },
        }
    }

    /// Słownik: pełny (magia 0xEC30A437, tabele entropii, przesunięcia,
    /// zawartość) albo sama zawartość.
    pub fn with_dictionary(dict: &[u8]) -> Result<Self, String> {
        let mut d = Self::new();
        if dict.len() < 8 || u32_le(dict, 0)? != DICT_MAGIC {
            d.content = dict.to_vec();
            return Ok(d);
        }
        let mut at = 8;
        let (huf, n) = Huffman::read(&dict[at..])?;
        at += n;
        let (of, n) = Fse::read(&dict[at..], 8, 31)?;
        at += n;
        let (ml, n) = Fse::read(&dict[at..], 9, 52)?;
        at += n;
        let (ll, n) = Fse::read(&dict[at..], 9, 35)?;
        at += n;
        let reps = [u32_le(dict, at)?, u32_le(dict, at + 4)?, u32_le(dict, at + 8)?];
        at += 12;
        d.entropy = Entropy {
            huffman: Some(huf),
            ll: Some(ll),
            of: Some(of),
            ml: Some(ml),
            reps,
        };
        d.content = dict[at..].to_vec();
        Ok(d)
    }

    /// Rozpakowuje jedną lub więcej ramek; wynik nie większy niż `max` bajtów.
    pub fn decompress(&mut self, src: &[u8], max: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(max.min(1 << 28));
        let mut at = 0;
        while at < src.len() {
            let magic = u32_le(src, at)?;
            if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
                // Ramka pomijalna.
                let size = u32_le(src, at + 4)? as usize;
                at += 8 + size;
                continue;
            }
            if magic != MAGIC {
                return Err(err("to nie są dane zstd"));
            }
            at = self.frame(src, at + 4, &mut out, max)?;
        }
        Ok(out)
    }

    fn frame(&self, src: &[u8], mut at: usize, out: &mut Vec<u8>, max: usize) -> Result<usize, String> {
        let desc = *src.get(at).ok_or_else(|| err("ucięty nagłówek"))?;
        at += 1;
        let fcs_flag = desc >> 6;
        let single = desc & 0x20 != 0;
        let checksum = desc & 0x04 != 0;
        let dict_id = [0, 1, 2, 4][(desc & 3) as usize];
        if !single {
            at += 1; // deskryptor okna
        }
        at += dict_id;
        at += match fcs_flag {
            0 => single as usize,
            1 => 2,
            2 => 4,
            _ => 8,
        };
        if at > src.len() {
            return Err(err("ucięty nagłówek"));
        }
        // Historia: zawartość słownika + to, co już rozpakowane w tej ramce.
        let mut history = self.content.clone();
        let base = history.len();
        let mut entropy = self.entropy.clone();
        loop {
            let h = src.get(at..at + 3).ok_or_else(|| err("ucięty blok"))?;
            let header = h[0] as u32 | (h[1] as u32) << 8 | (h[2] as u32) << 16;
            at += 3;
            let last = header & 1 != 0;
            let kind = (header >> 1) & 3;
            let size = (header >> 3) as usize;
            match kind {
                0 => {
                    let data = src.get(at..at + size).ok_or_else(|| err("ucięty blok"))?;
                    history.extend_from_slice(data);
                    at += size;
                }
                1 => {
                    let b = *src.get(at).ok_or_else(|| err("ucięty blok"))?;
                    history.resize(history.len() + size, b);
                    at += 1;
                }
                2 => {
                    if size > MAX_BLOCK {
                        return Err(err("za duży blok"));
                    }
                    let data = src.get(at..at + size).ok_or_else(|| err("ucięty blok"))?;
                    block(data, &mut history, &mut entropy)?;
                    at += size;
                }
                _ => return Err(err("zły typ bloku")),
            }
            if out.len() + history.len() - base > max {
                return Err(err("wynik większy niż zapowiedziany"));
            }
            if last {
                break;
            }
        }
        if checksum {
            at += 4;
        }
        out.extend_from_slice(&history[base..]);
        Ok(at)
    }
}

/// Literały bloku; zwraca je i liczbę zużytych bajtów.
fn literals(data: &[u8], entropy: &mut Entropy) -> Result<(Vec<u8>, usize), String> {
    let b0 = *data.first().ok_or_else(|| err("brak literałów"))? as usize;
    let kind = b0 & 3;
    let format = (b0 >> 2) & 3;
    let byte = |i: usize| -> Result<usize, String> {
        data.get(i).map(|&b| b as usize).ok_or_else(|| err("ucięty nagłówek literałów"))
    };
    if kind < 2 {
        let (size, header) = match format {
            0 | 2 => (b0 >> 3, 1),
            1 => ((b0 >> 4) | byte(1)? << 4, 2),
            _ => ((b0 >> 4) | byte(1)? << 4 | byte(2)? << 12, 3),
        };
        return if kind == 0 {
            let lit = data.get(header..header + size).ok_or_else(|| err("ucięte literały"))?;
            Ok((lit.to_vec(), header + size))
        } else {
            Ok((vec![byte(header)? as u8; size], header + 1))
        };
    }
    // Huffman: rozmiary po 10, 14 albo 18 bitów.
    let (streams, header, bits) = match format {
        0 => (1, 3, 10),
        1 => (4, 3, 10),
        2 => (4, 4, 14),
        _ => (4, 5, 18),
    };
    let mut h = 0u64;
    for i in 0..header {
        h |= (byte(i)? as u64) << (8 * i);
    }
    let regen = ((h >> 4) & ((1 << bits) - 1)) as usize;
    let comp = ((h >> (4 + bits)) & ((1 << bits) - 1)) as usize;
    let body = data.get(header..header + comp).ok_or_else(|| err("ucięte literały"))?;
    let mut at = 0;
    if kind == 2 {
        let (huf, n) = Huffman::read(body)?;
        entropy.huffman = Some(huf);
        at = n;
    }
    let huf = entropy.huffman.as_ref().ok_or_else(|| err("brak drzewa Huffmana"))?;
    let mut out = Vec::with_capacity(regen);
    let rest = &body[at..];
    if streams == 1 {
        huf.decode(rest, regen, &mut out)?;
    } else {
        let jump = rest.get(..6).ok_or_else(|| err("ucięta tabela strumieni"))?;
        let s = |i: usize| u16::from_le_bytes([jump[i], jump[i + 1]]) as usize;
        let (s1, s2, s3) = (s(0), s(2), s(4));
        let total = rest.len() - 6;
        if s1 + s2 + s3 > total {
            return Err(err("zła tabela strumieni"));
        }
        let quarter = regen.div_ceil(4);
        let mut start = 6;
        for (i, len) in [s1, s2, s3, total - s1 - s2 - s3].into_iter().enumerate() {
            let count = if i < 3 { quarter } else { regen.saturating_sub(3 * quarter) };
            huf.decode(&rest[start..start + len], count, &mut out)?;
            start += len;
        }
    }
    if out.len() != regen {
        return Err(err("zła liczba literałów"));
    }
    Ok((out, header + comp))
}

/// Tabela FSE sekwencji wg trybu (0 domyślna, 1 RLE, 2 opisana, 3 poprzednia).
fn seq_table(
    mode: u8,
    data: &[u8],
    at: &mut usize,
    slot: &mut Option<Fse>,
    default: (&[i16], u32),
    max_log: u32,
    max_symbol: usize,
) -> Result<(), String> {
    match mode {
        0 => *slot = Some(Fse::from_norm(default.0, default.1)?),
        1 => {
            let s = *data.get(*at).ok_or_else(|| err("ucięte sekwencje"))?;
            *at += 1;
            *slot = Some(Fse::rle(s));
        }
        2 => {
            let (t, n) = Fse::read(data.get(*at..).ok_or_else(|| err("ucięte sekwencje"))?, max_log, max_symbol)?;
            *at += n;
            *slot = Some(t);
        }
        _ => {
            if slot.is_none() {
                return Err(err("brak poprzedniej tabeli FSE"));
            }
        }
    }
    Ok(())
}

/// Blok skompresowany: dopisuje wynik do `history`.
fn block(data: &[u8], history: &mut Vec<u8>, entropy: &mut Entropy) -> Result<(), String> {
    let (lits, mut at) = literals(data, entropy)?;
    let b0 = *data.get(at).ok_or_else(|| err("brak sekwencji"))? as usize;
    let byte = |i: usize| -> Result<usize, String> {
        data.get(i).map(|&b| b as usize).ok_or_else(|| err("ucięte sekwencje"))
    };
    let count = if b0 < 128 {
        at += 1;
        b0
    } else if b0 < 255 {
        let n = ((b0 - 128) << 8) + byte(at + 1)?;
        at += 2;
        n
    } else {
        let n = byte(at + 1)? + (byte(at + 2)? << 8) + 0x7F00;
        at += 3;
        n
    };
    if count == 0 {
        history.extend_from_slice(&lits);
        return Ok(());
    }
    let modes = byte(at)? as u8;
    at += 1;
    seq_table(modes >> 6, data, &mut at, &mut entropy.ll, (&LL_DEFAULT, 6), 9, 35)?;
    seq_table((modes >> 4) & 3, data, &mut at, &mut entropy.of, (&OF_DEFAULT, 5), 8, 31)?;
    seq_table((modes >> 2) & 3, data, &mut at, &mut entropy.ml, (&ML_DEFAULT, 6), 9, 52)?;
    let (ll_t, of_t, ml_t) = (
        entropy.ll.as_ref().unwrap(),
        entropy.of.as_ref().unwrap(),
        entropy.ml.as_ref().unwrap(),
    );
    let mut bits = Backward::new(data.get(at..).ok_or_else(|| err("ucięte sekwencje"))?)?;
    let mut ll_s = State::new(ll_t, &mut bits);
    let mut of_s = State::new(of_t, &mut bits);
    let mut ml_s = State::new(ml_t, &mut bits);
    let reps = &mut entropy.reps;
    let mut lit_pos = 0usize;
    for i in 0..count {
        let of_code = of_s.symbol() as u32;
        let ml_code = ml_s.symbol() as usize;
        let ll_code = ll_s.symbol() as usize;
        if of_code > 31 || ml_code >= ML_BASE.len() || ll_code >= LL_BASE.len() {
            return Err(err("zły kod sekwencji"));
        }
        let of_value = (1u64 << of_code) as u32 + bits.read(of_code) as u32;
        let ml = ML_BASE[ml_code] as usize + bits.read(ML_BITS[ml_code] as u32) as usize;
        let ll = LL_BASE[ll_code] as usize + bits.read(LL_BITS[ll_code] as u32) as usize;
        // Przesunięcia powtórzeń (RFC 8878, 3.1.1.5).
        let offset = if of_value > 3 {
            let o = of_value - 3;
            *reps = [o, reps[0], reps[1]];
            o
        } else {
            let index = of_value - 1 + (ll == 0) as u32;
            match index {
                0 => reps[0],
                1 => {
                    let o = reps[1];
                    *reps = [o, reps[0], reps[2]];
                    o
                }
                2 => {
                    let o = reps[2];
                    *reps = [o, reps[0], reps[1]];
                    o
                }
                _ => {
                    let o = reps[0].wrapping_sub(1);
                    if o == 0 {
                        return Err(err("złe przesunięcie"));
                    }
                    *reps = [o, reps[0], reps[1]];
                    o
                }
            }
        } as usize;
        // Literały, potem dopasowanie (może nachodzić na samo siebie).
        let lit = lits.get(lit_pos..lit_pos + ll).ok_or_else(|| err("za mało literałów"))?;
        history.extend_from_slice(lit);
        lit_pos += ll;
        if offset == 0 || offset > history.len() {
            return Err(err("przesunięcie poza danymi"));
        }
        let start = history.len() - offset;
        if offset >= ml {
            history.extend_from_within(start..start + ml);
        } else {
            for k in 0..ml {
                let b = history[start + k];
                history.push(b);
            }
        }
        if i + 1 < count {
            ll_s.update(&mut bits);
            ml_s.update(&mut bits);
            of_s.update(&mut bits);
        }
    }
    if !bits.done() {
        return Err(err("zły strumień sekwencji"));
    }
    history.extend_from_slice(&lits[lit_pos..]);
    Ok(())
}

/// Najprostsza poprawna ramka zstd: jeden surowy blok (do testów, dane do 128 KB).
#[cfg(test)]
pub fn raw_frame(data: &[u8]) -> Vec<u8> {
    assert!(data.len() >= 256 && data.len() <= MAX_BLOCK);
    let mut f = MAGIC.to_le_bytes().to_vec();
    // Jeden segment, rozmiar treści na 2 bajtach (minus 256).
    f.push(0x60);
    f.extend_from_slice(&((data.len() - 256) as u16).to_le_bytes());
    let header = ((data.len() as u32) << 3) | 1;
    f.extend_from_slice(&header.to_le_bytes()[..3]);
    f.extend_from_slice(data);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministyczne dane podobne do tekstu shaderów (generator LCG).
    pub fn sample(seed: u64, len: usize) -> Vec<u8> {
        const WORDS: [&str; 16] = [
            "float4 ", "half3 ", "tex2D(", "sampler ", "return ", "dot(", "normalize(", "mul(",
            "cbuffer ", "register(b0); ", "SV_Target ", "+ ", "* ", "0.5f, ", "\n", "uv.xy ",
        ];
        let mut x = seed;
        let mut out = Vec::with_capacity(len + 16);
        while out.len() < len {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = (x >> 33) as usize;
            if r % 9 == 0 {
                out.push((r >> 8) as u8);
            } else {
                out.extend_from_slice(WORDS[r % WORDS.len()].as_bytes());
            }
        }
        out.truncate(len);
        out
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("zstd_testdata")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn frames_from_reference_encoder() {
        // Dane spakowane oryginalnym zstd (poziomy 1, 3, 19; dane losowe,
        // powtarzalne, wiele bloków, z sumą kontrolną).
        for (name, seed, len) in [
            ("text-l1.zst", 1, 50_000),
            ("text-l3.zst", 1, 50_000),
            ("text-l19.zst", 1, 50_000),
            ("multi-block-l3.zst", 2, 600_000),
            ("checksum-l5.zst", 3, 20_000),
        ] {
            let want = sample(seed, len);
            let got = decompress(&fixture(name), len).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(got == want, "{name}: różne dane");
        }
        let random: Vec<u8> = (0..20_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        assert_eq!(decompress(&fixture("random-l3.zst"), 20_000).unwrap(), random);
        let mut rle = vec![0u8; 300_000];
        rle.extend(std::iter::repeat_n(b"abcd".as_slice(), 1000).flatten());
        assert_eq!(decompress(&fixture("rle-l3.zst"), rle.len()).unwrap(), rle);
        let raw: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(decompress(&raw_frame(&raw), raw.len()).unwrap(), raw);
        // Za mały limit wyniku to błąd.
        assert!(decompress(&fixture("text-l3.zst"), 1000).is_err());
    }

    #[test]
    fn damaged_data_gives_errors_not_panics() {
        // Ucięte i przekłamane dane: błąd albo inne dane, nigdy panika.
        let dict = fixture("dict.bin");
        let cases: Vec<(Vec<u8>, bool)> = ["text-l3.zst", "text-l19.zst", "random-l3.zst", "checksum-l5.zst"]
            .iter()
            .map(|n| (fixture(n), false))
            .chain((0..3).map(|i| (fixture(&format!("dict-sample{i}.zst")), true)))
            .collect();
        let mut x = 12345u64;
        for (data, with_dict) in cases {
            for _ in 0..300 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let mut d = data.clone();
                match x % 3 {
                    0 => d.truncate((x >> 20) as usize % d.len()),
                    _ => {
                        let i = (x >> 20) as usize % d.len();
                        d[i] ^= 1 << ((x >> 8) % 8);
                    }
                }
                let result = std::panic::catch_unwind(|| {
                    let mut dec = if with_dict {
                        Decoder::with_dictionary(&dict).unwrap()
                    } else {
                        Decoder::new()
                    };
                    let _ = dec.decompress(&d, 1 << 20);
                });
                assert!(result.is_ok(), "panika dla uszkodzonych danych");
            }
        }
    }

    #[test]
    fn dictionary_from_reference_encoder() {
        // Słownik wytrenowany przez zstd na próbkach; 8 próbek spakowanych z nim.
        let dict = fixture("dict.bin");
        assert_eq!(u32_le(&dict, 0).unwrap(), DICT_MAGIC);
        let mut d = Decoder::with_dictionary(&dict).unwrap();
        for i in 0..8u64 {
            let want = sample(100 + i, 3000 + i as usize * 500);
            let got = d
                .decompress(&fixture(&format!("dict-sample{i}.zst")), want.len())
                .unwrap_or_else(|e| panic!("próbka {i}: {e}"));
            assert!(got == want, "próbka {i}: różne dane");
        }
        // Słownik z samą zawartością.
        let raw = sample(7, 4000);
        let mut d = Decoder::with_dictionary(&raw).unwrap();
        let want = sample(8, 5000);
        assert_eq!(d.decompress(&fixture("rawdict-sample.zst"), want.len()).unwrap(), want);
    }
}
