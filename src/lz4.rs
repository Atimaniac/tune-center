//! Dekompresja bloku LZ4 (format "LZ4 Block" z opisu formatu LZ4):
//! sekwencje [token][długość literałów+][literały][przesunięcie][długość
//! dopasowania+]. Używana w plikach Unity i paczkach Unreal.

/// Rozpakowuje blok LZ4; wynik nie może przekroczyć `max` bajtów.
pub fn decompress(src: &[u8], max: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(max);
    let mut i = 0;
    let err = |what: &str| format!("LZ4: {what}");
    // Długość z bajtami przedłużenia (255 = czytaj dalej).
    let length = |i: &mut usize, base: usize| -> Result<usize, String> {
        let mut n = base;
        if base == 15 {
            loop {
                let b = *src.get(*i).ok_or_else(|| err("ucięta długość"))?;
                *i += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        Ok(n)
    };
    while i < src.len() {
        let token = src[i];
        i += 1;
        let lit = length(&mut i, (token >> 4) as usize)?;
        let end = i.checked_add(lit).filter(|&e| e <= src.len()).ok_or_else(|| err("ucięte literały"))?;
        if out.len() + lit > max {
            return Err(err("wynik większy niż zapowiedziany"));
        }
        out.extend_from_slice(&src[i..end]);
        i = end;
        // Ostatnia sekwencja ma same literały.
        if i == src.len() {
            break;
        }
        let offset = u16::from_le_bytes([
            *src.get(i).ok_or_else(|| err("ucięte przesunięcie"))?,
            *src.get(i + 1).ok_or_else(|| err("ucięte przesunięcie"))?,
        ]) as usize;
        i += 2;
        if offset == 0 || offset > out.len() {
            return Err(err("złe przesunięcie"));
        }
        let len = length(&mut i, (token & 0x0F) as usize)? + 4;
        if out.len() + len > max {
            return Err(err("wynik większy niż zapowiedziany"));
        }
        // Kopia bajt po bajcie: dopasowanie może nachodzić na samo siebie.
        let start = out.len() - offset;
        for k in 0..len {
            let b = out[start + k];
            out.push(b);
        }
    }
    Ok(out)
}

/// Najprostszy poprawny blok LZ4: całe dane jako literały (do testów).
#[cfg(test)]
pub fn compress_literals(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 255 + 2);
    let n = data.len();
    out.push(((n.min(15)) as u8) << 4);
    if n >= 15 {
        let mut rest = n - 15;
        while rest >= 255 {
            out.push(255);
            rest -= 255;
        }
        out.push(rest as u8);
    }
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_round_trip() {
        for n in [0usize, 1, 14, 15, 16, 269, 270, 271, 1000] {
            let data: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
            assert_eq!(decompress(&compress_literals(&data), n).unwrap(), data, "n = {n}");
        }
    }

    #[test]
    fn overlapping_match() {
        // "ab", potem dopasowanie: przesunięcie 2, długość 4 + 3 = 7 ->
        // "ababababa", na końcu literał "!".
        let block = [0x23, b'a', b'b', 0x02, 0x00, 0x10, b'!'];
        assert_eq!(decompress(&block, 64).unwrap(), b"ababababa!");
        // Za mały limit wyniku i złe przesunięcie to błędy.
        assert!(decompress(&block, 5).is_err());
        assert!(decompress(&[0x10, b'a', 0x05, 0x00], 64).is_err());
    }

    #[test]
    fn long_match_length() {
        // Literał "x", dopasowanie długości 15 + 4 + 10 = 29 (bajt przedłużenia),
        // literał "y".
        let block = [0x1F, b'x', 0x01, 0x00, 10, 0x10, b'y'];
        let mut want = vec![b'x'; 30];
        want.push(b'y');
        assert_eq!(decompress(&block, 64).unwrap(), want);
    }
}
