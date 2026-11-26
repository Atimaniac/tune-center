//! Dekompresja zlib / DEFLATE (RFC 1950, RFC 1951) - do pamięci shaderów
//! zapisanych przez silnik w tej postaci. Tylko odczyt, bez zależności.

/// Strumień zlib (2 bajty nagłówka, dane DEFLATE, Adler-32) o znanym
/// rozmiarze po rozpakowaniu. `None`: dane uszkodzone albo inny rozmiar.
pub fn zlib(data: &[u8], size: usize) -> Option<Vec<u8>> {
    let (&cmf, &flg) = (data.first()?, data.get(1)?);
    if cmf & 0x0F != 8 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return None;
    }
    let (out, used) = inflate(&data[2..], size)?;
    if out.len() != size {
        return None;
    }
    let sum = data.get(2 + used..2 + used + 4)?;
    (u32::from_be_bytes(sum.try_into().ok()?) == adler32(&out)).then_some(out)
}

fn adler32(b: &[u8]) -> u32 {
    let (mut a, mut s) = (1u32, 0u32);
    for chunk in b.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            s += a;
        }
        a %= 65521;
        s %= 65521;
    }
    s << 16 | a
}

struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
    bit: u32,
    n: u32,
}

impl Bits<'_> {
    fn need(&mut self, k: u32) -> Option<()> {
        while self.n < k {
            let byte = *self.b.get(self.pos)?;
            self.pos += 1;
            self.bit |= u32::from(byte) << self.n;
            self.n += 8;
        }
        Some(())
    }

    fn take(&mut self, k: u32) -> Option<u32> {
        if k == 0 {
            return Some(0);
        }
        self.need(k)?;
        let v = self.bit & ((1u32 << k) - 1);
        self.bit >>= k;
        self.n -= k;
        Some(v)
    }

    /// Do granicy bajtu (bloki niekompresowane).
    fn align(&mut self) {
        let drop = self.n % 8;
        self.bit >>= drop;
        self.n -= drop;
    }

    /// Bajty zużyte do tej pory (pełne, łącznie z buforem bitów).
    fn used(&self) -> usize {
        self.pos - (self.n / 8) as usize
    }
}

/// Kod Huffmana kanoniczny: liczba kodów każdej długości i symbole po kolei.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut left = 1i32;
        for &c in &counts[1..] {
            left = left * 2 - i32::from(c);
            if left < 0 {
                return None;
            }
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Some(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Surowe dane DEFLATE: (bajty, zużyte bajty wejścia). `limit` - najwięcej
/// bajtów wyjścia (więcej = dane uszkodzone).
pub fn inflate(data: &[u8], limit: usize) -> Option<(Vec<u8>, usize)> {
    let mut bits = Bits { b: data, pos: 0, bit: 0, n: 0 };
    let mut out: Vec<u8> = Vec::with_capacity(limit);
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => {
                bits.align();
                let len = bits.take(16)? as usize;
                let nlen = bits.take(16)? as usize;
                if len != !nlen & 0xFFFF || out.len() + len > limit {
                    return None;
                }
                // Bufor bitów jest pusty po wyrównaniu i dwóch słowach.
                let start = bits.used();
                out.extend_from_slice(data.get(start..start + len)?);
                bits = Bits { b: data, pos: start + len, bit: 0, n: 0 };
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let lit = Huffman::new(&lengths)?;
                let dist = Huffman::new(&[5u8; 30])?;
                block(&mut bits, &mut out, &lit, &dist, limit)?;
            }
            2 => {
                let hlit = bits.take(5)? as usize + 257;
                let hdist = bits.take(5)? as usize + 1;
                let hclen = bits.take(4)? as usize + 4;
                const ORDER: [usize; 19] =
                    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in &ORDER[..hclen] {
                    cl[o] = bits.take(3)? as u8;
                }
                let code = Huffman::new(&cl)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < lengths.len() {
                    let sym = code.decode(&mut bits)?;
                    let (value, repeat) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.take(2)? as usize),
                        17 => (0, 3 + bits.take(3)? as usize),
                        18 => (0, 11 + bits.take(7)? as usize),
                        _ => return None,
                    };
                    if i + repeat > lengths.len() {
                        return None;
                    }
                    lengths[i..i + repeat].fill(value);
                    i += repeat;
                }
                if lengths[256] == 0 {
                    return None;
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..])?;
                block(&mut bits, &mut out, &lit, &dist, limit)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some((out, bits.used()));
        }
    }
}

fn block(bits: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, limit: usize) -> Option<()> {
    loop {
        let sym = lit.decode(bits)? as usize;
        match sym {
            0..=255 => {
                if out.len() >= limit {
                    return None;
                }
                out.push(sym as u8);
            }
            256 => return Some(()),
            _ => {
                let i = sym - 257;
                let len = *LEN_BASE.get(i)? as usize + bits.take(u32::from(LEN_EXTRA[i]))? as usize;
                let d = dist.decode(bits)? as usize;
                let back = *DIST_BASE.get(d)? as usize + bits.take(u32::from(DIST_EXTRA[d]))? as usize;
                if back > out.len() || out.len() + len > limit {
                    return None;
                }
                let from = out.len() - back;
                for k in 0..len {
                    let b = out[from + k];
                    out.push(b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflates_zlib_streams() {
        // .NET DeflateStream("hello hello hello hello") + Adler-32: kod stały.
        let fixed = [
            0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03, 0x08,
            0xb1,
        ];
        assert_eq!(zlib(&fixed, 23).unwrap(), b"hello hello hello hello");
        assert!(zlib(&fixed, 22).is_none());
        // Zła suma kontrolna.
        let mut bad = fixed;
        bad[15] ^= 1;
        assert!(zlib(&bad, 23).is_none());
        // Blok niekompresowany (poziom 0), Adler-32 "abc" = 0x024d0127.
        let stored = [0x78, 0x01, 0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c', 0x02, 0x4d, 0x01, 0x27];
        assert_eq!(zlib(&stored, 3).unwrap(), b"abc");
    }

    #[test]
    fn inflates_dynamic_huffman_block() {
        // .NET DeflateStream (blok z kodem dynamicznym).
        let deflated: [u8; 145] = [
            0x45, 0xd0, 0x41, 0x0e, 0x02, 0x31, 0x0c, 0x43, 0xd1, 0xab, 0xcc, 0x11, 0xea, 0xa4, 0x6d,
            0x1a, 0xcd, 0x69, 0x90, 0x40, 0x62, 0x87, 0x54, 0x16, 0x5c, 0x1f, 0x94, 0x4c, 0xcc, 0xd2,
            0x7f, 0xf5, 0xe4, 0xfd, 0xfa, 0x1c, 0xed, 0x78, 0x3f, 0x6f, 0xf7, 0xc7, 0x3e, 0xda, 0xb9,
            0x7f, 0x13, 0x35, 0x1d, 0x1e, 0x41, 0x2a, 0x2c, 0x5d, 0x11, 0xb4, 0x82, 0x0d, 0x8b, 0xd0,
            0x2b, 0x4c, 0x9b, 0x11, 0x46, 0x85, 0xe1, 0x23, 0xc2, 0x64, 0x40, 0x8f, 0x60, 0x15, 0xba,
            0x6a, 0x84, 0x55, 0x41, 0x87, 0x44, 0xf0, 0x0a, 0x62, 0x48, 0x18, 0xa1, 0xf0, 0x8b, 0x4a,
            0x2b, 0x5a, 0x5a, 0x41, 0xac, 0xa4, 0x15, 0xc4, 0x7a, 0x4f, 0x2c, 0xa8, 0x5d, 0x33, 0xb5,
            0x20, 0xd7, 0x56, 0x72, 0x41, 0xaf, 0xb5, 0xf4, 0x82, 0xe0, 0x29, 0x09, 0x06, 0xc5, 0xa3,
            0xa7, 0x18, 0x24, 0xf7, 0x99, 0x64, 0x21, 0x59, 0x57, 0x92, 0x85, 0x64, 0xf1, 0xeb, 0xde,
            0x3f, 0x19, 0x69, 0x16, 0x9a, 0xa1, 0x76, 0x7e, 0x01, 0x00,
        ];
        let text: String = (0..24).map(|i| format!("row {i} shader {};", (i * 7919) % 1000)).collect();
        let (out, _) = inflate(&deflated, 4096).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), text);
    }
}
