//! Stream filter decoding. Implemented: FlateDecode (+PNG/TIFF predictors), ASCIIHexDecode,
//! ASCII85Decode, RunLengthDecode. Everything else is a typed `UnsupportedFilter` refusal
//! (LZW, DCT, JPX, CCITT, JBIG2, Crypt). Every decode is bounded by `Limits` and a shared budget.
use crate::error::{Limits, PressError, Result};
use crate::syntax::{Dict, Object, Stream};

fn filter_names(dict: &Dict) -> Result<Vec<Vec<u8>>> {
    match dict.get("Filter").or_else(|| dict.get("F")) {
        None | Some(Object::Null) => Ok(Vec::new()),
        Some(Object::Name(n)) => Ok(vec![n.clone()]),
        Some(Object::Array(items)) => items
            .iter()
            .map(|o| match o {
                Object::Name(n) => Ok(n.clone()),
                _ => Err(PressError::Malformed("filter_entry")),
            })
            .collect(),
        Some(_) => Err(PressError::UnsupportedFilter("indirect_filter".into())),
    }
}

fn decode_parms(dict: &Dict, count: usize) -> Result<Vec<Option<Dict>>> {
    let raw = dict.get("DecodeParms").or_else(|| dict.get("DP"));
    let mut out: Vec<Option<Dict>> = vec![None; count];
    match raw {
        None | Some(Object::Null) => {}
        Some(Object::Dict(d)) => {
            if let Some(slot) = out.first_mut() {
                *slot = Some(d.clone());
            }
        }
        Some(Object::Array(items)) => {
            for (i, o) in items.iter().enumerate().take(count) {
                match o {
                    Object::Dict(d) => out[i] = Some(d.clone()),
                    Object::Null => {}
                    _ => return Err(PressError::UnsupportedFilter("indirect_decodeparms".into())),
                }
            }
        }
        Some(_) => return Err(PressError::UnsupportedFilter("indirect_decodeparms".into())),
    }
    Ok(out)
}

/// True when the stream has no filter, or only filters this module implements.
pub(crate) fn is_decodable(dict: &Dict) -> bool {
    match filter_names(dict) {
        Ok(names) => names.iter().all(|n| {
            matches!(
                n.as_slice(),
                b"FlateDecode"
                    | b"Fl"
                    | b"LZWDecode"
                    | b"LZW"
                    | b"ASCIIHexDecode"
                    | b"AHx"
                    | b"ASCII85Decode"
                    | b"A85"
                    | b"RunLengthDecode"
                    | b"RL"
            )
        }),
        Err(_) => false,
    }
}

/// Decodes the full filter chain. `budget` is the remaining decoded-byte allowance for the
/// current operation and is reduced by the final output size.
pub(crate) fn decode_stream(stream: &Stream, limits: &Limits, budget: &mut usize) -> Result<Vec<u8>> {
    let names = filter_names(&stream.dict)?;
    let parms = decode_parms(&stream.dict, names.len())?;
    let cap = limits.max_decoded_bytes.min(*budget);
    let mut data: Vec<u8> = stream.data.clone();
    for (name, parm) in names.iter().zip(parms.iter()) {
        data = match name.as_slice() {
            b"FlateDecode" | b"Fl" => {
                let inflated = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, cap).map_err(|e| {
                    if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput {
                        PressError::InputLimit("decoded_bytes")
                    } else {
                        PressError::Malformed("flate_data")
                    }
                })?;
                apply_predictor(inflated, parm.as_ref(), cap)?
            }
            b"LZWDecode" | b"LZW" => {
                let early = parm_int(parm.as_ref(), "EarlyChange", 1);
                if !(0..=1).contains(&early) {
                    return Err(PressError::Malformed("lzw_early_change"));
                }
                let out = lzw(&data, early as usize, cap)?;
                apply_predictor(out, parm.as_ref(), cap)?
            }
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(&data, cap)?,
            b"ASCII85Decode" | b"A85" => ascii85(&data, cap)?,
            b"RunLengthDecode" | b"RL" => run_length(&data, cap)?,
            other => return Err(PressError::UnsupportedFilter(String::from_utf8_lossy(other).into_owned())),
        };
        if data.len() > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
    }
    if data.len() > *budget {
        return Err(PressError::InputLimit("decoded_total"));
    }
    *budget -= data.len();
    Ok(data)
}

fn parm_int(parm: Option<&Dict>, key: &str, default: i64) -> i64 {
    parm.and_then(|d| d.get(key)).and_then(Object::as_int).unwrap_or(default)
}

fn apply_predictor(data: Vec<u8>, parm: Option<&Dict>, cap: usize) -> Result<Vec<u8>> {
    let predictor = parm_int(parm, "Predictor", 1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = parm_int(parm, "Colors", 1);
    let bpc = parm_int(parm, "BitsPerComponent", 8);
    let columns = parm_int(parm, "Columns", 1);
    if !(1..=32).contains(&colors) || !matches!(bpc, 1 | 2 | 4 | 8 | 16) || !(1..=(1 << 20)).contains(&columns) {
        return Err(PressError::Malformed("predictor_parms"));
    }
    let (colors, bpc, columns) = (colors as usize, bpc as usize, columns as usize);
    let row_bytes = (colors * bpc * columns).div_ceil(8);
    let bpp = (colors * bpc).div_ceil(8).max(1);
    match predictor {
        2 => {
            if bpc != 8 {
                return Err(PressError::UnsupportedFilter("tiff_predictor_bpc".into()));
            }
            let mut out = data;
            for row in out.chunks_mut(row_bytes) {
                for i in colors..row.len() {
                    row[i] = row[i].wrapping_add(row[i - colors]);
                }
            }
            Ok(out)
        }
        10..=15 => {
            let stride = row_bytes + 1;
            let rows = data.len().div_ceil(stride);
            if rows.saturating_mul(row_bytes) > cap {
                return Err(PressError::InputLimit("decoded_bytes"));
            }
            let mut out: Vec<u8> = Vec::with_capacity(rows * row_bytes);
            let mut prev = vec![0u8; row_bytes];
            for chunk in data.chunks(stride) {
                let tag = chunk[0];
                let mut row = vec![0u8; row_bytes];
                let body = &chunk[1..];
                row[..body.len().min(row_bytes)].copy_from_slice(&body[..body.len().min(row_bytes)]);
                for i in 0..row_bytes {
                    let left = if i >= bpp { row[i - bpp] } else { 0 };
                    let up = prev[i];
                    let up_left = if i >= bpp { prev[i - bpp] } else { 0 };
                    row[i] = row[i].wrapping_add(match tag {
                        0 => 0,
                        1 => left,
                        2 => up,
                        3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                        4 => paeth(left, up, up_left),
                        _ => return Err(PressError::Malformed("png_filter_tag")),
                    });
                }
                out.extend_from_slice(&row);
                prev = row;
            }
            Ok(out)
        }
        _ => Err(PressError::UnsupportedFilter("predictor".into())),
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i32::from(a) + i32::from(b) - i32::from(c);
    let (pa, pb, pc) = ((p - i32::from(a)).abs(), (p - i32::from(b)).abs(), (p - i32::from(c)).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// PDF LZWDecode: MSB-first codes of 9..=12 bits, 256 = clear, 257 = end of data. Dictionary
/// entries are (prefix code, byte, length) triples (max 4096), so memory is constant and the
/// output is bounded by `cap`.
fn lzw(data: &[u8], early: usize, cap: usize) -> Result<Vec<u8>> {
    let mut prefix = [0u16; 4096];
    let mut byte = [0u8; 4096];
    let mut length = [0u16; 4096];
    for i in 0..256usize {
        byte[i] = i as u8;
        length[i] = 1;
    }
    let mut out: Vec<u8> = Vec::new();
    let mut next = 258usize;
    let mut width = 9usize;
    let mut prev: Option<usize> = None;
    let (mut acc, mut nbits) = (0u32, 0usize);
    let mut pos = 0usize;
    let first_byte = |code: usize, prefix: &[u16; 4096], byte: &[u8; 4096], length: &[u16; 4096]| -> u8 {
        let mut c = code;
        while length[c] > 1 {
            c = usize::from(prefix[c]);
        }
        byte[c]
    };
    loop {
        while nbits < width {
            let Some(&b) = data.get(pos) else {
                return Ok(out); // tolerate a missing EOD
            };
            pos += 1;
            acc = (acc << 8) | u32::from(b);
            nbits += 8;
        }
        let code = ((acc >> (nbits - width)) & ((1 << width) - 1)) as usize;
        nbits -= width;
        acc &= (1u32 << nbits) - 1;
        match code {
            257 => return Ok(out),
            256 => {
                next = 258;
                width = 9;
                prev = None;
                continue;
            }
            _ => {}
        }
        let entry_len;
        let kwkwk;
        if code < 256 || (code >= 258 && code < next) {
            entry_len = usize::from(length[code]);
            kwkwk = false;
        } else if code == next && next < 4096 {
            let Some(p) = prev else {
                return Err(PressError::Malformed("lzw_code"));
            };
            entry_len = usize::from(length[p]) + 1;
            kwkwk = true;
        } else {
            return Err(PressError::Malformed("lzw_code"));
        }
        if out.len().saturating_add(entry_len) > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
        let start = out.len();
        out.resize(start + entry_len, 0);
        let first;
        if kwkwk {
            // string(prev) followed by its own first byte
            let p = prev.unwrap_or(0);
            first = first_byte(p, &prefix, &byte, &length);
            out[start + entry_len - 1] = first;
            let mut c = p;
            for slot in out[start..start + entry_len - 1].iter_mut().rev() {
                *slot = byte[c];
                c = usize::from(prefix[c]);
            }
        } else {
            let mut c = code;
            for slot in out[start..start + entry_len].iter_mut().rev() {
                *slot = byte[c];
                c = usize::from(prefix[c]);
            }
            first = out[start];
        }
        if let Some(p) = prev
            && next < 4096
        {
            prefix[next] = p as u16;
            byte[next] = first;
            length[next] = length[p] + 1;
            next += 1;
        }
        prev = Some(code);
        width = if next + early >= 2048 {
            12
        } else if next + early >= 1024 {
            11
        } else if next + early >= 512 {
            10
        } else {
            9
        };
    }
}

fn ascii_hex(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ if crate::syntax::is_ws(b) => continue,
            _ => return Err(PressError::Malformed("asciihex")),
        };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => {
                out.push(h * 16 + v);
                if out.len() > cap {
                    return Err(PressError::InputLimit("decoded_bytes"));
                }
            }
        }
    }
    if let Some(h) = hi {
        out.push(h * 16);
    }
    Ok(out)
}

fn ascii85(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut group = [0u32; 5];
    let mut n = 0usize;
    let mut i = 0usize;
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'~' => break,
            _ if crate::syntax::is_ws(b) => {}
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = u32::from(b - b'!');
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |acc, g| acc * 85 + u64::from(*g));
                    if v > u64::from(u32::MAX) {
                        return Err(PressError::Malformed("ascii85"));
                    }
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            _ => return Err(PressError::Malformed("ascii85")),
        }
        if out.len() > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
    }
    if n == 1 {
        return Err(PressError::Malformed("ascii85"));
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |acc, g| acc * 85 + u64::from(*g));
        let bytes = (v as u32).to_be_bytes();
        out.extend_from_slice(&bytes[..n - 1]);
    }
    Ok(out)
}

fn run_length(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let len = data[i];
        i += 1;
        match len {
            128 => break,
            0..=127 => {
                let n = usize::from(len) + 1;
                let end = (i + n).min(data.len());
                out.extend_from_slice(&data[i..end]);
                i = end;
            }
            _ => {
                let n = 257 - usize::from(len);
                let Some(&b) = data.get(i) else {
                    return Err(PressError::Malformed("runlength"));
                };
                i += 1;
                out.extend(std::iter::repeat_n(b, n));
            }
        }
        if out.len() > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
    }
    Ok(out)
}
