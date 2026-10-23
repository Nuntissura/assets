//! Independent scan of decoded content streams for drawn glyphs (test support): text matrices
//! (`1 0 0 1 x y Tm`), single-glyph `Tj` and `TJ` arrays (lane SMB-LAYOUT-LONGDOC-2: press writes
//! text runs as `TJ` with adjustments). Each glyph's x after the first of an array is the previous
//! glyph's x plus its program advance (`advances`, 1/1000 em) times the size, minus the adjustment
//! (thousandths of the size). Codes are 2-byte glyph ids, hex (`<00A1>`) or literal (`( A)`).
#![allow(dead_code)]

/// The 2-byte codes of one string token.
fn codes(token: &str) -> Vec<u16> {
    let bytes: Vec<u8> = if let Some(hex) = token.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect()
    } else {
        let inner = token.strip_prefix('(').and_then(|t| t.strip_suffix(')')).unwrap();
        let mut out = Vec::new();
        let mut esc = false;
        for b in inner.bytes() {
            if esc {
                out.push(b);
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else {
                out.push(b);
            }
        }
        out
    };
    bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
}

/// Splits a `TJ` array body into string and number tokens.
fn tokens(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' => i += 1,
            b'<' => {
                let j = body[i..].find('>').unwrap() + i;
                out.push(body[i..=j].to_owned());
                i = j + 1;
            }
            b'(' => {
                let mut j = i + 1;
                while b[j] != b')' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                out.push(body[i..=j].to_owned());
                i = j + 1;
            }
            _ => {
                let j = body[i..].find(' ').map_or(body.len(), |k| k + i);
                out.push(body[i..j].to_owned());
                i = j;
            }
        }
    }
    out
}

/// `(glyph id, x, y)` of every drawn glyph, in stream order.
pub fn shown_runs(streams: &[Vec<u8>], advances: &[u16]) -> Vec<(u16, f64, f64)> {
    let mut out = Vec::new();
    for s in streams {
        let text = String::from_utf8_lossy(s).into_owned();
        let (mut x, mut y, mut size) = (0.0f64, 0.0f64, 0.0f64);
        let mut in_text = false;
        for line in text.lines() {
            let t: Vec<&str> = line.split(' ').collect();
            if line == "BT" {
                in_text = true;
            } else if line == "ET" {
                in_text = false;
            } else if in_text && t.len() == 3 && t[2] == "Tf" {
                size = t[1].parse().unwrap();
            } else if in_text && t.len() == 7 && t[6] == "Tm" {
                x = t[4].parse().unwrap();
                y = t[5].parse().unwrap();
            } else if in_text && line.ends_with(" Tj") {
                for g in codes(&line[..line.len() - 3]) {
                    out.push((g, x, y));
                    x += f64::from(advances[usize::from(g)]) * size / 1000.0;
                }
            } else if in_text && line.ends_with("] TJ") && line.starts_with('[') {
                for tok in tokens(&line[1..line.len() - 4]) {
                    if tok.starts_with('<') || tok.starts_with('(') {
                        for g in codes(&tok) {
                            out.push((g, x, y));
                            x += f64::from(advances[usize::from(g)]) * size / 1000.0;
                        }
                    } else {
                        let n: f64 = tok.parse().unwrap();
                        x -= n * size / 1000.0;
                    }
                }
            }
        }
    }
    out
}
