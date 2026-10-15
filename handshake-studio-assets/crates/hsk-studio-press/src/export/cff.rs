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

/// A DICT operator with its operands' raw bytes.
type RawEntry<'a> = (u16, Vec<&'a [u8]>);

/// DICT entries with their operands' raw bytes (for rewriting with the other operands unchanged).
fn dict_raw(b: &[u8]) -> Option<Vec<RawEntry<'_>>> {
    let mut out = Vec::new();
    let mut operands: Vec<&[u8]> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let b0 = *b.get(i)?;
        let len = match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    1200u16.checked_add(u16::from(*b.get(i.checked_add(1)?)?))?
                } else {
                    u16::from(b0)
                };
                out.push((op, std::mem::take(&mut operands)));
                i = i.checked_add(if b0 == 12 { 2 } else { 1 })?;
                continue;
            }
            28 => 3,
            29 => 5,
            30 => {
                let mut j = i.checked_add(1)?;
                loop {
                    let n = *b.get(j)?;
                    j = j.checked_add(1)?;
                    if n & 0x0f == 0x0f || n >> 4 == 0x0f {
                        break;
                    }
                }
                j.checked_sub(i)?
            }
            32..=246 => 1,
            247..=254 => 2,
            _ => return None,
        };
        operands.push(b.get(i..i.checked_add(len)?)?);
        if operands.len() > MAX_OPERANDS {
            return None;
        }
        i = i.checked_add(len)?;
    }
    Some(out)
}

/// The 5-byte DICT integer (operator 29) for `v`.
fn int5(v: usize) -> Option<[u8; 5]> {
    let b = i32::try_from(v).ok()?.to_be_bytes();
    Some([29, b[0], b[1], b[2], b[3]])
}

/// Encodes DICT entries; operators in `patch` get their operands replaced.
fn encode_dict(entries: &[RawEntry<'_>], patch: &[(u16, Vec<[u8; 5]>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (op, operands) in entries {
        match patch.iter().find(|(p, _)| p == op) {
            Some((_, ints)) => ints.iter().for_each(|v| out.extend_from_slice(v)),
            None => operands.iter().for_each(|o| out.extend_from_slice(o)),
        }
        if *op >= 1200 {
            out.push(12);
            out.push(u8::try_from(op.saturating_sub(1200)).unwrap_or(0));
        } else {
            out.push(u8::try_from(*op).unwrap_or(0));
        }
    }
    out
}

/// An INDEX of `items` with 4-byte offsets.
fn write_index(items: &[Vec<u8>]) -> Option<Vec<u8>> {
    let count = u16::try_from(items.len()).ok()?;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    if count == 0 {
        return Some(out);
    }
    out.push(4);
    let mut off = 1u32;
    out.extend_from_slice(&off.to_be_bytes());
    for it in items {
        off = off.checked_add(u32::try_from(it.len()).ok()?)?;
        out.extend_from_slice(&off.to_be_bytes());
    }
    for it in items {
        out.extend_from_slice(it);
    }
    Some(out)
}

/// Byte length of a charset (formats 0, 1, 2) for `n` glyphs.
fn charset_len(cff: &[u8], at: usize, n: usize) -> Option<usize> {
    let format = u8_at(cff, at)?;
    let mut p = at.checked_add(1)?;
    match format {
        0 => 1usize.checked_add(n.checked_sub(1)?.checked_mul(2)?),
        1 | 2 => {
            let mut covered = 1usize;
            while covered < n {
                let left = if format == 1 { usize::from(u8_at(cff, p.checked_add(2)?)?) } else { usize::from(be16(cff, p.checked_add(2)?)?) };
                covered = covered.checked_add(left.checked_add(1)?)?;
                p = p.checked_add(if format == 1 { 3 } else { 4 })?;
            }
            p.checked_sub(at)
        }
        _ => None,
    }
}

/// Byte length of an FDSelect (formats 0 and 3) for `n` glyphs.
fn fdselect_len(cff: &[u8], at: usize, n: usize) -> Option<usize> {
    match u8_at(cff, at)? {
        0 => n.checked_add(1),
        3 => {
            let ranges = usize::from(be16(cff, at.checked_add(1)?)?);
            ranges.checked_mul(3)?.checked_add(5)
        }
        _ => None,
    }
}

/// One font dict of a CID-keyed CFF with its Private dict and local Subrs.
struct Fd<'a> {
    entries: Vec<RawEntry<'a>>,
    private: Vec<RawEntry<'a>>,
    subrs: Option<&'a [u8]>,
}

/// A CID-keyed CFF with only the glyphs of `keep` (glyph ids, glyph 0 always kept): unused
/// CharStrings become `endchar`, so glyph ids, the charset and the FDSelect stay valid; the font
/// is rebuilt (header, Name, Top DICT with 5-byte offsets, String, Global Subr, charset, FDSelect,
/// CharStrings, FDArray, Private dicts and their local Subrs).
pub(super) fn subset_cid(cff: &[u8], keep: &std::collections::BTreeSet<u16>) -> Result<Vec<u8>, ExportError> {
    let hdr = usize::from(u8_at(cff, 2).ok_or(BAD)?);
    let (_, after_name) = index(cff, hdr).ok_or(BAD)?;
    let (top, after_top) = index(cff, after_name).ok_or(BAD)?;
    let (_, after_strings) = index(cff, after_top).ok_or(BAD)?;
    let (_, after_gsubrs) = index(cff, after_strings).ok_or(BAD)?;
    let &(ts, te) = top.first().ok_or(BAD)?;
    let top_raw = cff.get(ts..te).ok_or(BAD)?;
    let entries = dict_raw(top_raw).ok_or(BAD)?;
    let parsed = dict(top_raw).ok_or(BAD)?;
    let off = |op: u16| parsed.iter().find(|(o, _)| *o == op).and_then(|(_, v)| v.first().copied()).and_then(as_offset);
    if !parsed.iter().any(|(o, _)| *o == 1230) {
        return Err(BAD);
    }
    let (charset_at, cs_at, fda_at, fds_at) = (off(15).ok_or(BAD)?, off(17).ok_or(BAD)?, off(1236).ok_or(BAD)?, off(1237).ok_or(BAD)?);
    let (glyphs, _) = index(cff, cs_at).ok_or(BAD)?;
    let n = glyphs.len();
    let charset = cff.get(charset_at..charset_at.checked_add(charset_len(cff, charset_at, n).ok_or(BAD)?).ok_or(BAD)?).ok_or(BAD)?;
    let fdselect = cff.get(fds_at..fds_at.checked_add(fdselect_len(cff, fds_at, n).ok_or(BAD)?).ok_or(BAD)?).ok_or(BAD)?;
    let (fds, _) = index(cff, fda_at).ok_or(BAD)?;
    let mut charstrings = Vec::with_capacity(n);
    for (gid, (s, e)) in glyphs.iter().enumerate() {
        let kept = gid == 0 || u16::try_from(gid).is_ok_and(|g| keep.contains(&g));
        charstrings.push(if kept { cff.get(*s..*e).ok_or(BAD)?.to_vec() } else { vec![14u8] });
    }
    let cs_index = write_index(&charstrings).ok_or(BAD)?;
    let mut fd_list = Vec::with_capacity(fds.len());
    for (s, e) in &fds {
        let raw = cff.get(*s..*e).ok_or(BAD)?;
        let fd_entries = dict_raw(raw).ok_or(BAD)?;
        let p = dict(raw).ok_or(BAD)?;
        let pv = p.iter().find(|(o, _)| *o == 18).map(|(_, v)| v.clone()).ok_or(BAD)?;
        let (psize, poff) = (pv.first().copied().and_then(as_offset).ok_or(BAD)?, pv.get(1).copied().and_then(as_offset).ok_or(BAD)?);
        let praw = cff.get(poff..poff.checked_add(psize).ok_or(BAD)?).ok_or(BAD)?;
        let private = dict_raw(praw).ok_or(BAD)?;
        let subrs_rel = dict(praw).ok_or(BAD)?.iter().find(|(o, _)| *o == 19).and_then(|(_, v)| v.first().copied()).and_then(as_offset);
        let subrs = match subrs_rel {
            Some(rel) => {
                let at = poff.checked_add(rel).ok_or(BAD)?;
                let (_, end) = index(cff, at).ok_or(BAD)?;
                Some(cff.get(at..end).ok_or(BAD)?)
            }
            None => None,
        };
        fd_list.push(Fd { entries: fd_entries, private, subrs });
    }
    // Every rewritten offset is a 5-byte integer, so sizes do not depend on the offset values.
    let top_dict = |a: [usize; 4]| -> Option<Vec<u8>> {
        Some(encode_dict(&entries, &[(15, vec![int5(a[0])?]), (17, vec![int5(a[1])?]), (1236, vec![int5(a[2])?]), (1237, vec![int5(a[3])?])]))
    };
    let top_len = write_index(&[top_dict([0; 4]).ok_or(BAD)?]).ok_or(BAD)?.len();
    let head = cff.get(..after_name).ok_or(BAD)?;
    let strings_gsubrs = cff.get(after_top..after_gsubrs).ok_or(BAD)?;
    let charset_new = head.len().checked_add(top_len).and_then(|v| v.checked_add(strings_gsubrs.len())).ok_or(BAD)?;
    let fds_new = charset_new.checked_add(charset.len()).ok_or(BAD)?;
    let cs_new = fds_new.checked_add(fdselect.len()).ok_or(BAD)?;
    let fda_new = cs_new.checked_add(cs_index.len()).ok_or(BAD)?;
    let private_dict = |fd: &Fd<'_>| -> Option<Vec<u8>> {
        if fd.subrs.is_none() {
            return Some(encode_dict(&fd.private, &[]));
        }
        let len = encode_dict(&fd.private, &[(19u16, vec![int5(0)?])]).len();
        Some(encode_dict(&fd.private, &[(19, vec![int5(len)?])]))
    };
    let privates: Vec<Vec<u8>> = fd_list.iter().map(private_dict).collect::<Option<_>>().ok_or(BAD)?;
    let fd_dict = |fd: &Fd<'_>, size: usize, at: usize| -> Option<Vec<u8>> { Some(encode_dict(&fd.entries, &[(18, vec![int5(size)?, int5(at)?])])) };
    let fda_len = write_index(&fd_list.iter().map(|fd| fd_dict(fd, 0, 0)).collect::<Option<Vec<_>>>().ok_or(BAD)?).ok_or(BAD)?.len();
    let mut at = fda_new.checked_add(fda_len).ok_or(BAD)?;
    let mut fd_dicts = Vec::with_capacity(fd_list.len());
    let mut tail = Vec::new();
    for (fd, pd) in fd_list.iter().zip(&privates) {
        fd_dicts.push(fd_dict(fd, pd.len(), at).ok_or(BAD)?);
        tail.extend_from_slice(pd);
        at = at.checked_add(pd.len()).ok_or(BAD)?;
        if let Some(s) = fd.subrs {
            tail.extend_from_slice(s);
            at = at.checked_add(s.len()).ok_or(BAD)?;
        }
    }
    let mut out = Vec::with_capacity(at);
    out.extend_from_slice(head);
    out.extend(write_index(&[top_dict([charset_new, cs_new, fda_new, fds_new]).ok_or(BAD)?]).ok_or(BAD)?);
    out.extend_from_slice(strings_gsubrs);
    out.extend_from_slice(charset);
    out.extend_from_slice(fdselect);
    out.extend(cs_index);
    out.extend(write_index(&fd_dicts).ok_or(BAD)?);
    out.extend(tail);
    Ok(out)
}

/// The charstring bytes of glyph `gid` (test and diagnostic aid).
#[cfg(test)]
pub(super) fn charstring(cff: &[u8], gid: usize) -> Option<&[u8]> {
    let hdr = usize::from(u8_at(cff, 2)?);
    let (_, after_name) = index(cff, hdr)?;
    let (top, _) = index(cff, after_name)?;
    let &(ts, te) = top.first()?;
    let entries = dict(cff.get(ts..te)?)?;
    let at = entries.iter().find(|(o, _)| *o == 17).and_then(|(_, v)| v.first().copied()).and_then(as_offset)?;
    let (glyphs, _) = index(cff, at)?;
    let &(s, e) = glyphs.get(gid)?;
    cff.get(s..e)
}
