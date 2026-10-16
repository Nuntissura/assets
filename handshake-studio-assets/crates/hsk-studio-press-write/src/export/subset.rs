//! TrueType subsetting for `/FontFile2` (SMB-LAYOUT-PDF-EXPORT-2 item 5), glyph-id preserving:
//! the CIDFontType2 is written with `/CIDToGIDMap /Identity`, so every glyph keeps its id and the
//! glyphs not used become empty (`loca` entries of zero length). The kept glyph set is the used ids,
//! glyph 0 (`.notdef`) and, transitively, the components of composite glyphs. Tables written:
//! `head` (`indexToLocFormat` = 1, fresh `checkSumAdjustment`), `hhea`, `hmtx`, `maxp`, `loca`
//! (long), `glyf` and, when present, `cvt `, `fpgm`, `prep` and `OS/2` (ISO 32000-2 9.9 lists the
//! tables a CIDFontType2 program needs; `cmap`, `name`, `post` and layout tables are dropped).
//! Structure per the OpenType specification (chapters "font file", "glyf", "loca", "head").
//! Bounded: every read is a checked slice access, the output is never larger than the input plus
//! the table directory and padding, the composite closure visits each glyph once.
#![deny(clippy::arithmetic_side_effects)]
use super::ExportError;
use std::collections::BTreeSet;

const BAD: ExportError = ExportError::FontUnreadable;
const MAX_TABLES: usize = 512;
const KEEP: [&[u8; 4]; 10] = [b"OS/2", b"cvt ", b"fpgm", b"glyf", b"head", b"hhea", b"hmtx", b"loca", b"maxp", b"prep"];

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn tables(font: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let n = usize::from(be16(font, 4)?);
    if n > MAX_TABLES {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let rec = i.checked_mul(16)?.checked_add(12)?;
        let tag: [u8; 4] = font.get(rec..rec.checked_add(4)?)?.try_into().ok()?;
        let off = usize::try_from(be32(font, rec.checked_add(8)?)?).ok()?;
        let len = usize::try_from(be32(font, rec.checked_add(12)?)?).ok()?;
        out.push((tag, font.get(off..off.checked_add(len)?)?));
    }
    Some(out)
}

/// Byte ranges of every glyph in `glyf` from `loca`.
fn glyph_ranges(loca: &[u8], long: bool, num_glyphs: usize, glyf_len: usize) -> Option<Vec<(usize, usize)>> {
    let mut offs = Vec::with_capacity(num_glyphs.checked_add(1)?);
    for i in 0..=num_glyphs {
        let v = if long {
            usize::try_from(be32(loca, i.checked_mul(4)?)?).ok()?
        } else {
            usize::from(be16(loca, i.checked_mul(2)?)?).checked_mul(2)?
        };
        offs.push(v);
    }
    let mut out = Vec::with_capacity(num_glyphs);
    for w in offs.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a > b || b > glyf_len {
            return None;
        }
        out.push((a, b));
    }
    Some(out)
}

/// Component glyph ids of a composite glyph (`numberOfContours < 0`).
fn components(glyph: &[u8]) -> Option<Vec<u16>> {
    let contours = i16::from_be_bytes(glyph.get(0..2)?.try_into().ok()?);
    if contours >= 0 {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut at = 10usize;
    loop {
        let flags = be16(glyph, at)?;
        out.push(be16(glyph, at.checked_add(2)?)?);
        let mut step = 4usize;
        step = step.checked_add(if flags & 0x0001 != 0 { 4 } else { 2 })?;
        if flags & 0x0008 != 0 {
            step = step.checked_add(2)?;
        } else if flags & 0x0040 != 0 {
            step = step.checked_add(4)?;
        } else if flags & 0x0080 != 0 {
            step = step.checked_add(8)?;
        }
        at = at.checked_add(step)?;
        if flags & 0x0020 == 0 {
            return Some(out);
        }
        if at >= glyph.len() {
            return None;
        }
    }
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        for (d, s) in word.iter_mut().zip(chunk) {
            *d = *s;
        }
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

/// The kept glyph set: `used`, glyph 0 and every composite component (ids >= the glyph count are
/// ignored; the caller already refused them).
pub(super) fn closure(font: &[u8], used: &BTreeSet<u16>) -> Result<BTreeSet<u16>, ExportError> {
    let t = tables(font).ok_or(BAD)?;
    let find = |tag: &[u8; 4]| t.iter().find(|(k, _)| k == tag).map(|(_, v)| *v);
    let (head, maxp, loca, glyf) = (find(b"head").ok_or(BAD)?, find(b"maxp").ok_or(BAD)?, find(b"loca").ok_or(BAD)?, find(b"glyf").ok_or(BAD)?);
    let n = usize::from(be16(maxp, 4).ok_or(BAD)?);
    let long = be16(head, 50).ok_or(BAD)? == 1;
    let ranges = glyph_ranges(loca, long, n, glyf.len()).ok_or(BAD)?;
    let mut keep: BTreeSet<u16> = BTreeSet::new();
    let mut work: Vec<u16> = std::iter::once(0).chain(used.iter().copied()).collect();
    while let Some(g) = work.pop() {
        if usize::from(g) >= n || !keep.insert(g) {
            continue;
        }
        let (a, b) = *ranges.get(usize::from(g)).ok_or(BAD)?;
        if a < b {
            work.extend(components(glyf.get(a..b).ok_or(BAD)?).ok_or(BAD)?);
        }
    }
    Ok(keep)
}

/// A font program with only the glyphs of `keep` (ids preserved; module docs).
pub(super) fn subset(font: &[u8], keep: &BTreeSet<u16>) -> Result<Vec<u8>, ExportError> {
    let t = tables(font).ok_or(BAD)?;
    let find = |tag: &[u8; 4]| t.iter().find(|(k, _)| k == tag).map(|(_, v)| *v);
    let (head, maxp, loca, glyf) = (find(b"head").ok_or(BAD)?, find(b"maxp").ok_or(BAD)?, find(b"loca").ok_or(BAD)?, find(b"glyf").ok_or(BAD)?);
    if head.len() < 54 {
        return Err(BAD);
    }
    let n = usize::from(be16(maxp, 4).ok_or(BAD)?);
    let long = be16(head, 50).ok_or(BAD)? == 1;
    let ranges = glyph_ranges(loca, long, n, glyf.len()).ok_or(BAD)?;
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity(n.checked_add(1).ok_or(BAD)?.checked_mul(4).ok_or(BAD)?);
    for (g, (a, b)) in ranges.iter().enumerate() {
        new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).map_err(|_| BAD)?.to_be_bytes());
        if u16::try_from(g).is_ok_and(|g| keep.contains(&g)) {
            new_glyf.extend_from_slice(glyf.get(*a..*b).ok_or(BAD)?);
            while new_glyf.len() & 3 != 0 {
                new_glyf.push(0);
            }
        }
    }
    new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).map_err(|_| BAD)?.to_be_bytes());
    let mut new_head = head.to_vec();
    new_head[8..12].copy_from_slice(&[0; 4]);
    new_head[50..52].copy_from_slice(&1u16.to_be_bytes());
    let mut out_tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for tag in KEEP {
        let data = match tag {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            _ => match find(tag) {
                Some(d) => d.to_vec(),
                None if matches!(tag, b"hhea" | b"hmtx") => return Err(BAD),
                None => continue,
            },
        };
        out_tables.push((*tag, data));
    }
    let count = u16::try_from(out_tables.len()).map_err(|_| BAD)?;
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow.checked_mul(2).is_some_and(|p| p <= count) {
        pow = pow.saturating_mul(2);
        log = log.saturating_add(1);
    }
    let search = pow.saturating_mul(16);
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&search.to_be_bytes());
    out.extend_from_slice(&log.to_be_bytes());
    out.extend_from_slice(&count.saturating_mul(16).saturating_sub(search).to_be_bytes());
    let dir_end = usize::from(count).checked_mul(16).and_then(|d| d.checked_add(12)).ok_or(BAD)?;
    let mut offset = dir_end;
    let mut body = Vec::new();
    let mut head_at = None;
    for (tag, data) in &out_tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).map_err(|_| BAD)?.to_be_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).map_err(|_| BAD)?.to_be_bytes());
        if tag == b"head" {
            head_at = Some(offset);
        }
        body.extend_from_slice(data);
        while body.len() & 3 != 0 {
            body.push(0);
        }
        offset = dir_end.checked_add(body.len()).ok_or(BAD)?;
    }
    out.extend_from_slice(&body);
    // checkSumAdjustment = 0xB1B0AFBA - checksum of the whole font (with the field at zero).
    let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    let at = head_at.ok_or(BAD)?.checked_add(8).ok_or(BAD)?;
    out.get_mut(at..at.checked_add(4).ok_or(BAD)?).ok_or(BAD)?.copy_from_slice(&adjust.to_be_bytes());
    Ok(out)
}

/// Six upper-case letters naming a subset (ISO 32000-2 9.9.2 subset tag), from a digest.
pub(super) fn tag(digest: &[u8; 32]) -> String {
    digest.iter().take(6).map(|b| char::from(b'A'.saturating_add(b.checked_rem(26).unwrap_or(0)))).collect()
}
