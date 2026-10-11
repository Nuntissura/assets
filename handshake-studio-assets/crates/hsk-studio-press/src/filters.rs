//! Stream filter decoding. Implemented: FlateDecode (+PNG/TIFF predictors), LZWDecode (+predictors),
//! ASCIIHexDecode, ASCII85Decode, RunLengthDecode. Everything else is a typed `UnsupportedFilter`
//! refusal (DCT, JPX, CCITT, JBIG2, Crypt).
//!
//! Bounds (ISO 32000-2 7.4 for the filter semantics; the limits are this crate's own):
//! - every filter stage writes at most `min(Limits::max_decoded_bytes, remaining budget)` bytes;
//! - the stage output is charged to the caller's budget, so a chain of expanding filters pays for
//!   every intermediate result, not only the last one;
//! - a stage that fails still costs: a cap overrun costs the cap, any other failure costs the input
//!   it examined, so a document of many hostile streams cannot be re-tried for free;
//! - the filter chain is at most `Limits::max_filters` long;
//! - predictor rows are never padded: the output of a predictor is never longer than its input, so
//!   hostile `/Columns` x `/Colors` x `/BitsPerComponent` cannot allocate a row the data does not hold.
//!
//! Hostile-input tests: `hostile_parser_dims_u32_max` (predictor dims, filter chain),
//! `aggregate_over_budget_many_streams`, `lzw_filter_decodes_and_is_bomb_bounded`,
//! `hostile_decoder_bounds_dims_and_aggregate`.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Limits, PressError, Result};
use crate::syntax::{Dict, Object, Stream, hex_val, is_ws};
use std::borrow::Cow;

fn filter_names(dict: &Dict, limits: Option<&Limits>) -> Result<Vec<Vec<u8>>> {
    let names = match dict.get("Filter").or_else(|| dict.get("F")) {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(items)) => {
            if let Some(l) = limits
                && items.len() > l.max_filters
            {
                return Err(PressError::InputLimit("filter_chain"));
            }
            items
                .iter()
                .map(|o| match o {
                    Object::Name(n) => Ok(n.clone()),
                    _ => Err(PressError::Malformed("filter_entry")),
                })
                .collect::<Result<Vec<_>>>()?
        }
        Some(_) => return Err(PressError::UnsupportedFilter("indirect_filter".into())),
    };
    Ok(names)
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
            for (slot, o) in out.iter_mut().zip(items.iter()) {
                match o {
                    Object::Dict(d) => *slot = Some(d.clone()),
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
    match filter_names(dict, None) {
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
/// current operation; it is reduced by the output of every stage (and by the cost of a failed one).
pub(crate) fn decode_stream(stream: &Stream, limits: &Limits, budget: &mut usize) -> Result<Vec<u8>> {
    let names = filter_names(&stream.dict, Some(limits))?;
    let parms = decode_parms(&stream.dict, names.len())?;
    let mut data: Cow<'_, [u8]> = Cow::Borrowed(stream.data.as_slice());
    if names.is_empty() {
        let cap = limits.max_decoded_bytes.min(*budget);
        if data.len() > cap {
            return Err(limit_error(limits, cap));
        }
    }
    for (name, parm) in names.iter().zip(parms.iter()) {
        let cap = limits.max_decoded_bytes.min(*budget);
        let out = match stage(name, parm.as_ref(), &data, cap) {
            Ok(out) if out.len() <= cap => out,
            Ok(_) => {
                *budget = budget.saturating_sub(cap);
                return Err(limit_error(limits, cap));
            }
            Err(e) => {
                let spent = if matches!(e, PressError::InputLimit(_)) { cap } else { data.len() };
                *budget = budget.saturating_sub(spent);
                return Err(match e {
                    PressError::InputLimit("decoded_bytes") => limit_error(limits, cap),
                    other => other,
                });
            }
        };
        *budget = budget.saturating_sub(out.len());
        data = Cow::Owned(out);
    }
    if names.is_empty() {
        *budget = budget.saturating_sub(data.len());
    }
    Ok(data.into_owned())
}

/// The cap was the per-stream limit (`decoded_bytes`) or what remained of the operation budget (`decoded_total`).
fn limit_error(limits: &Limits, cap: usize) -> PressError {
    if cap < limits.max_decoded_bytes { PressError::InputLimit("decoded_total") } else { PressError::InputLimit("decoded_bytes") }
}

/// One filter stage (plus its predictor) with output capped at `cap` bytes.
fn stage(name: &[u8], parm: Option<&Dict>, data: &[u8], cap: usize) -> Result<Vec<u8>> {
    match name {
        b"FlateDecode" | b"Fl" => {
            let inflated = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, cap).map_err(|e| {
                if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput {
                    PressError::InputLimit("decoded_bytes")
                } else {
                    PressError::Malformed("flate_data")
                }
            })?;
            apply_predictor(inflated, parm, cap)
        }
        b"LZWDecode" | b"LZW" => {
            let early = parm_int(parm, "EarlyChange", 1);
            let early = usize::try_from(early).ok().filter(|e| *e <= 1).ok_or(PressError::Malformed("lzw_early_change"))?;
            let out = lzw(data, early, cap)?;
            apply_predictor(out, parm, cap)
        }
        b"ASCIIHexDecode" | b"AHx" => ascii_hex(data, cap),
        b"ASCII85Decode" | b"A85" => ascii85(data, cap),
        b"RunLengthDecode" | b"RL" => run_length(data, cap),
        other => Err(PressError::UnsupportedFilter(String::from_utf8_lossy(other).into_owned())),
    }
}

fn parm_int(parm: Option<&Dict>, key: &str, default: i64) -> i64 {
    parm.and_then(|d| d.get(key)).and_then(Object::as_int).unwrap_or(default)
}

/// PNG (10..=15) and TIFF (2) predictors, ISO 32000-2 7.4.4.4. The output is never longer than the
/// input: a final partial row stays partial.
fn apply_predictor(data: Vec<u8>, parm: Option<&Dict>, cap: usize) -> Result<Vec<u8>> {
    let predictor = parm_int(parm, "Predictor", 1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = usize::try_from(parm_int(parm, "Colors", 1)).ok().filter(|c| (1..=32).contains(c));
    let bpc = usize::try_from(parm_int(parm, "BitsPerComponent", 8)).ok().filter(|b| matches!(b, 1 | 2 | 4 | 8 | 16));
    let columns = usize::try_from(parm_int(parm, "Columns", 1)).ok().filter(|c| (1..=(1 << 20)).contains(c));
    let (Some(colors), Some(bpc), Some(columns)) = (colors, bpc, columns) else {
        return Err(PressError::Malformed("predictor_parms"));
    };
    // colors <= 32, bpc <= 16, columns <= 2^20: the product is at most 2^29.
    let pixel_bits = colors.checked_mul(bpc).ok_or(PressError::Malformed("predictor_parms"))?;
    let row_bytes = pixel_bits.checked_mul(columns).ok_or(PressError::Malformed("predictor_parms"))?.div_ceil(8);
    let bpp = pixel_bits.div_ceil(8).max(1);
    let out = match predictor {
        2 => {
            if bpc != 8 {
                return Err(PressError::UnsupportedFilter("tiff_predictor_bpc".into()));
            }
            let mut out = data;
            for row in out.chunks_mut(row_bytes) {
                for i in colors..row.len() {
                    let left = row.get(i.saturating_sub(colors)).copied().unwrap_or(0);
                    if let Some(slot) = row.get_mut(i) {
                        *slot = slot.wrapping_add(left);
                    }
                }
            }
            out
        }
        10..=15 => png_unpredict(&data, row_bytes, bpp)?,
        _ => return Err(PressError::UnsupportedFilter("predictor".into())),
    };
    if out.len() > cap {
        return Err(PressError::InputLimit("decoded_bytes"));
    }
    Ok(out)
}

/// Reverses the PNG row filters. Each row is a tag byte plus up to `row_bytes` bytes; the output
/// holds the rows without tags, so it is shorter than the input.
fn png_unpredict(data: &[u8], row_bytes: usize, bpp: usize) -> Result<Vec<u8>> {
    let stride = row_bytes.checked_add(1).ok_or(PressError::Malformed("predictor_parms"))?;
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let mut prev: Option<usize> = None; // start of the previous row in `out`
    for chunk in data.chunks(stride) {
        let Some((&tag, body)) = chunk.split_first() else { break };
        if tag > 4 {
            return Err(PressError::Malformed("png_filter_tag"));
        }
        let start = out.len();
        out.extend_from_slice(body);
        for i in 0..body.len() {
            let at = start.saturating_add(i);
            let left = if i >= bpp { out.get(at.saturating_sub(bpp)).copied().unwrap_or(0) } else { 0 };
            let up = prev.and_then(|p| out.get(p.saturating_add(i)).copied()).unwrap_or(0);
            let up_left = if i >= bpp { prev.and_then(|p| out.get(p.saturating_add(i).saturating_sub(bpp)).copied()).unwrap_or(0) } else { 0 };
            let add = match tag {
                0 => 0,
                1 => left,
                2 => up,
                3 => u8::try_from(u16::from(left).saturating_add(u16::from(up)) / 2).unwrap_or(u8::MAX),
                _ => paeth(left, up, up_left),
            };
            if let Some(slot) = out.get_mut(at) {
                *slot = slot.wrapping_add(add);
            }
        }
        prev = Some(start);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i32::from(a).wrapping_add(i32::from(b)).wrapping_sub(i32::from(c));
    let (pa, pb, pc) = (p.abs_diff(i32::from(a)), p.abs_diff(i32::from(b)), p.abs_diff(i32::from(c)));
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Bit masks for LZW code widths 0..=12.
const MASK: [u32; 13] = [0, 1, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095];

/// PDF LZWDecode: MSB-first codes of 9..=12 bits, 256 = clear, 257 = end of data. Dictionary
/// entries are (prefix code, byte, length) triples (max 4096), so memory is constant and the
/// output is bounded by `cap`.
fn lzw(data: &[u8], early: usize, cap: usize) -> Result<Vec<u8>> {
    let mut prefix = [0u16; 4096];
    let mut byte = [0u8; 4096];
    let mut length = [0u16; 4096];
    for (i, (b, l)) in byte.iter_mut().zip(length.iter_mut()).take(256).enumerate() {
        *b = u8::try_from(i).unwrap_or(0);
        *l = 1;
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
            pos = pos.saturating_add(1);
            acc = acc.wrapping_shl(8) | u32::from(b);
            nbits = nbits.saturating_add(8);
        }
        let left = nbits.saturating_sub(width);
        let code = usize::try_from(acc.wrapping_shr(u32::try_from(left).unwrap_or(0)) & MASK[width]).unwrap_or(0);
        nbits = left;
        acc &= MASK.get(nbits).copied().unwrap_or(0);
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
            entry_len = usize::from(length[p]).saturating_add(1);
            kwkwk = true;
        } else {
            return Err(PressError::Malformed("lzw_code"));
        }
        if out.len().saturating_add(entry_len) > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
        let start = out.len();
        let end = start.saturating_add(entry_len);
        out.resize(end, 0);
        let first;
        if kwkwk {
            // string(prev) followed by its own first byte
            let p = prev.unwrap_or(0);
            first = first_byte(p, &prefix, &byte, &length);
            if let Some(slot) = out.get_mut(end.saturating_sub(1)) {
                *slot = first;
            }
            let mut c = p;
            for slot in out[start..end.saturating_sub(1)].iter_mut().rev() {
                *slot = byte[c];
                c = usize::from(prefix[c]);
            }
        } else {
            let mut c = code;
            for slot in out[start..end].iter_mut().rev() {
                *slot = byte[c];
                c = usize::from(prefix[c]);
            }
            first = out.get(start).copied().unwrap_or(0);
        }
        if let Some(p) = prev
            && next < 4096
        {
            prefix[next] = u16::try_from(p).unwrap_or(0);
            byte[next] = first;
            length[next] = length[p].saturating_add(1);
            next = next.saturating_add(1);
        }
        prev = Some(code);
        let probe = next.saturating_add(early);
        width = if probe >= 2048 {
            12
        } else if probe >= 1024 {
            11
        } else if probe >= 512 {
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
        let v = match hex_val(b) {
            Some(v) => v,
            None if is_ws(b) => continue,
            None => return Err(PressError::Malformed("asciihex")),
        };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => {
                out.push(h.wrapping_mul(16).wrapping_add(v));
                if out.len() > cap {
                    return Err(PressError::InputLimit("decoded_bytes"));
                }
            }
        }
    }
    if let Some(h) = hi {
        out.push(h.wrapping_mul(16));
        if out.len() > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
    }
    Ok(out)
}

fn ascii85_value(group: &[u32; 5]) -> u64 {
    group.iter().fold(0u64, |acc, g| acc.saturating_mul(85).saturating_add(u64::from(*g)))
}

fn ascii85(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut group = [0u32; 5];
    let mut n = 0usize;
    for &b in data {
        match b {
            b'~' => break,
            _ if is_ws(b) => {}
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                if let Some(slot) = group.get_mut(n) {
                    *slot = u32::from(b.saturating_sub(b'!'));
                }
                n = n.saturating_add(1);
                if n == 5 {
                    let v = u32::try_from(ascii85_value(&group)).map_err(|_| PressError::Malformed("ascii85"))?;
                    out.extend_from_slice(&v.to_be_bytes());
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
        // The group padded with 'u' may pass 2^32; only its leading bytes are kept, as before.
        let bytes = (ascii85_value(&group) as u32).to_be_bytes();
        out.extend_from_slice(bytes.get(..n.saturating_sub(1)).unwrap_or(&[]));
        if out.len() > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
    }
    Ok(out)
}

fn run_length(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&len) = data.get(i) {
        i = i.saturating_add(1);
        match len {
            128 => break,
            0..=127 => {
                let n = usize::from(len).saturating_add(1);
                let end = i.saturating_add(n).min(data.len());
                out.extend_from_slice(data.get(i..end).unwrap_or(&[]));
                i = end;
            }
            _ => {
                let n = 257usize.saturating_sub(usize::from(len));
                let Some(&b) = data.get(i) else {
                    return Err(PressError::Malformed("runlength"));
                };
                i = i.saturating_add(1);
                out.extend(std::iter::repeat_n(b, n));
            }
        }
        if out.len() > cap {
            return Err(PressError::InputLimit("decoded_bytes"));
        }
    }
    Ok(out)
}
