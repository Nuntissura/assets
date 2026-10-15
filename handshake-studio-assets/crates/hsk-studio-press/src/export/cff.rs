//! Bounded reader of the CFF facts the exporter needs to embed a CFF / OpenType-CFF font as a
//! CIDFontType0 (lane SMB-LAYOUT-PDF-EXPORT-3): whether the font is CID-keyed (`ROS` in the Top
//! DICT), its Registry / Ordering / Supplement, the glyph count (CharStrings INDEX) and, for a
//! CID-keyed font, the glyph-to-CID table from the charset (formats 0, 1 and 2). Structure per Adobe
//! Technical Note 5176 (CFF): header, Name / Top DICT / String INDEX, DICT operand encodings,
//! charset formats. Every read is a checked slice access; tables are bounded by the glyph count
//! (at most 65535).
#![deny(clippy::arithmetic_side_effects)]
use super::ExportError;

const BAD: ExportError = ExportError::FontUnreadable;
/// Strings of the CFF standard string set have SIDs below this (TN 5176 Appendix A).
const STANDARD_STRINGS: usize = 391;
/// DICT operands kept on the stack before an operator (TN 5176 allows 48).
const MAX_OPERANDS: usize = 48;

pub(super) struct CffFacts {
    pub cid_keyed: bool,
    pub num_glyphs: u16,
    /// `gid_to_cid[gid]` for a CID-keyed font (empty otherwise).
    pub gid_to_cid: Vec<u16>,
    /// ROS strings when they are custom strings (SID >= 391); `None` falls back to Adobe-Identity.
    pub registry: Option<String>,
    pub ordering: Option<String>,
    pub supplement: i64,
}

fn u8_at(b: &[u8], at: usize) -> Option<u8> {
    b.get(at).copied()
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}
fn offset(b: &[u8], at: usize, size: u8) -> Option<usize> {
    let mut v = 0usize;
    for k in 0..usize::from(size) {
        v = v.checked_mul(256)?.checked_add(usize::from(u8_at(b, at.checked_add(k)?)?))?;
    }
    Some(v)
}

/// An INDEX at `at`: its entries' byte ranges (absolute) and the offset just past it.
fn index(b: &[u8], at: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let count = usize::from(be16(b, at)?);
    if count == 0 {
        return Some((Vec::new(), at.checked_add(2)?));
    }
    let size = u8_at(b, at.checked_add(2)?)?;
    if !(1..=4).contains(&size) {
        return None;
    }
    let offs_at = at.checked_add(3)?;
    let data_base = offs_at.checked_add(count.checked_add(1)?.checked_mul(usize::from(size))?)?.checked_sub(1)?;
    let mut out = Vec::with_capacity(count);
    let mut prev = offset(b, offs_at, size)?;
    for i in 1..=count {
        let next = offset(b, offs_at.checked_add(i.checked_mul(usize::from(size))?)?, size)?;
        if next < prev {
            return None;
        }
        let (s, e) = (data_base.checked_add(prev)?, data_base.checked_add(next)?);
        if e > b.len() {
            return None;
        }
        out.push((s, e));
        prev = next;
    }
    let end = data_base.checked_add(prev)?;
    Some((out, end))
}

/// Parses a DICT into (operator, operands) pairs; operators 12 x are `1200 + x`.
fn dict(b: &[u8]) -> Option<Vec<(u16, Vec<f64>)>> {
    let mut out = Vec::new();
    let mut stack: Vec<f64> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let b0 = *b.get(i)?;
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    i = i.checked_add(1)?;
                    1200u16.checked_add(u16::from(*b.get(i)?))?
                } else {
                    u16::from(b0)
                };
                out.push((op, std::mem::take(&mut stack)));
                i = i.checked_add(1)?;
                continue;
            }
            28 => {
                stack.push(f64::from(i16::from_be_bytes([*b.get(i.checked_add(1)?)?, *b.get(i.checked_add(2)?)?])));
                i = i.checked_add(3)?;
            }
            29 => {
                let s = b.get(i.checked_add(1)?..i.checked_add(5)?)?;
                stack.push(f64::from(i32::from_be_bytes([s[0], s[1], s[2], s[3]])));
                i = i.checked_add(5)?;
            }
            30 => {
                // Real: nibbles until 0xf; the value is not needed by this reader.
                i = i.checked_add(1)?;
                loop {
                    let n = *b.get(i)?;
                    i = i.checked_add(1)?;
                    if n & 0x0f == 0x0f || n >> 4 == 0x0f {
                        break;
                    }
                }
                stack.push(0.0);
            }
            32..=246 => {
                stack.push(f64::from(i16::from(b0)) - 139.0);
                i = i.checked_add(1)?;
            }
            247..=250 => {
                let b1 = *b.get(i.checked_add(1)?)?;
                stack.push(f64::from(b0.checked_sub(247)?) * 256.0 + f64::from(b1) + 108.0);
                i = i.checked_add(2)?;
            }
            251..=254 => {
                let b1 = *b.get(i.checked_add(1)?)?;
                stack.push(-f64::from(b0.checked_sub(251)?) * 256.0 - f64::from(b1) - 108.0);
                i = i.checked_add(2)?;
            }
            _ => return None,
        }
        if stack.len() > MAX_OPERANDS {
            return None;
        }
    }
    Some(out)
}

fn as_offset(v: f64) -> Option<usize> {
    (v.is_finite() && v >= 0.0 && v.fract() == 0.0 && v < 4.0e9).then_some(v as usize)
}

/// Reads the facts of a bare CFF program (the `CFF ` table of an OpenType font).
pub(super) fn read(cff: &[u8]) -> Result<CffFacts, ExportError> {
    let hdr = usize::from(u8_at(cff, 2).ok_or(BAD)?);
    if u8_at(cff, 0) != Some(1) {
        return Err(BAD);
    }
    let (_, after_name) = index(cff, hdr).ok_or(BAD)?;
    let (top, after_top) = index(cff, after_name).ok_or(BAD)?;
    let (strings, _) = index(cff, after_top).ok_or(BAD)?;
    let &(ts, te) = top.first().ok_or(BAD)?;
    let entries = dict(cff.get(ts..te).ok_or(BAD)?).ok_or(BAD)?;
    let get = |op: u16| entries.iter().find(|(o, _)| *o == op).map(|(_, v)| v.clone());
    let charstrings = get(17).and_then(|v| v.first().copied()).and_then(as_offset).ok_or(BAD)?;
    let (glyphs, _) = index(cff, charstrings).ok_or(BAD)?;
    let num_glyphs = u16::try_from(glyphs.len()).map_err(|_| BAD)?;
    if num_glyphs == 0 {
        return Err(BAD);
    }
    let string = |sid: f64| -> Option<String> {
        let sid = as_offset(sid)?;
        let i = sid.checked_sub(STANDARD_STRINGS)?;
        let &(s, e) = strings.get(i)?;
        let t = cff.get(s..e)?;
        (t.len() <= 64 && t.iter().all(|c| (0x20..0x7f).contains(c))).then(|| t.iter().map(|c| char::from(*c)).collect())
    };
    let Some(ros) = get(1230) else {
        return Ok(CffFacts { cid_keyed: false, num_glyphs, gid_to_cid: Vec::new(), registry: None, ordering: None, supplement: 0 });
    };
    if ros.len() != 3 {
        return Err(BAD);
    }
    let charset = get(15).and_then(|v| v.first().copied()).and_then(as_offset).ok_or(BAD)?;
    let gid_to_cid = charset_table(cff, charset, num_glyphs).ok_or(BAD)?;
    Ok(CffFacts {
        cid_keyed: true,
        num_glyphs,
        gid_to_cid,
        registry: string(ros[0]),
        ordering: string(ros[1]),
        supplement: if ros[2].is_finite() { ros[2] as i64 } else { 0 },
    })
}

/// The charset as a glyph-to-CID table (glyph 0 is CID 0).
fn charset_table(cff: &[u8], at: usize, n: u16) -> Option<Vec<u16>> {
    let n = usize::from(n);
    let mut out = Vec::with_capacity(n);
    out.push(0u16);
    let format = u8_at(cff, at)?;
    let mut p = at.checked_add(1)?;
    match format {
        0 => {
            while out.len() < n {
                out.push(be16(cff, p)?);
                p = p.checked_add(2)?;
            }
        }
        1 | 2 => {
            while out.len() < n {
                let first = be16(cff, p)?;
                let left = if format == 1 { u16::from(u8_at(cff, p.checked_add(2)?)?) } else { be16(cff, p.checked_add(2)?)? };
                p = p.checked_add(if format == 1 { 3 } else { 4 })?;
                for k in 0..=left {
                    if out.len() >= n {
                        break;
                    }
                    out.push(first.checked_add(k)?);
                }
            }
        }
        _ => return None,
    }
    Some(out)
}
