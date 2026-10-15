//! Bounded reader of the few sfnt (TrueType/OpenType) facts the exporter needs: outline flavour,
//! embedding permission (`OS/2 fsType`), glyph count (`maxp`), units per em and bounding box
//! (`head`), ascent/descent (`hhea`), cap height (`OS/2` version 2+) and the PostScript name
//! (`name` id 6). Every read is a checked slice access; nothing is allocated from a size the font
//! states except the bounded name string. Structure per the OpenType specification (Microsoft
//! OpenType spec 1.9, chapters "font file", "head", "hhea", "maxp", "OS/2", "name").
#![deny(clippy::arithmetic_side_effects)]
use super::ExportError;

/// Longest PostScript name kept (ISO 32000 names are limited to 127 bytes; 63 keeps them short).
const MAX_NAME: usize = 63;
/// Most table records read from the directory (real fonts have about 10 to 40).
const MAX_TABLES: usize = 512;
const BAD: ExportError = ExportError::FontUnreadable;

pub(super) struct FontFacts {
    pub num_glyphs: u16,
    pub units_per_em: u16,
    /// xMin, yMin, xMax, yMax in font units.
    pub bbox: [i16; 4],
    pub ascent: i16,
    pub descent: i16,
    pub cap_height: Option<i16>,
    pub ps_name: Option<String>,
    /// Byte range of the `CFF ` table of an OpenType-CFF font (`None` for TrueType outlines).
    pub cff: Option<(usize, usize)>,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}
fn bei16(b: &[u8], at: usize) -> Option<i16> {
    be16(b, at).map(|v| i16::from_be_bytes(v.to_be_bytes()))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// The byte range of table `tag`, bounds-checked against the file.
fn table_range(font: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let n = usize::from(be16(font, 4)?);
    if n > MAX_TABLES {
        return None;
    }
    for i in 0..n {
        let rec = i.checked_mul(16)?.checked_add(12)?;
        if font.get(rec..rec.checked_add(4)?)? == tag {
            let off = usize::try_from(be32(font, rec.checked_add(8)?)?).ok()?;
            let end = off.checked_add(usize::try_from(be32(font, rec.checked_add(12)?)?).ok()?)?;
            return (end <= font.len()).then_some((off, end));
        }
    }
    None
}

/// The bytes of table `tag`, bounds-checked against the file.
fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let n = usize::from(be16(font, 4)?);
    if n > MAX_TABLES {
        return None;
    }
    for i in 0..n {
        let rec = i.checked_mul(16)?.checked_add(12)?;
        if font.get(rec..rec.checked_add(4)?)? == tag {
            let off = usize::try_from(be32(font, rec.checked_add(8)?)?).ok()?;
            let len = usize::try_from(be32(font, rec.checked_add(12)?)?).ok()?;
            return font.get(off..off.checked_add(len)?);
        }
    }
    None
}

/// Reads the facts of a single TrueType-outline (`glyf`) or OpenType-CFF (`CFF `) font. Refusals:
/// a collection (`ttcf`) or `face_index != 0` -> `FontCollectionNotEmbedded`; CFF2 (variable CFF)
/// outlines without a `CFF ` table -> `FontCffNotEmbedded`; `fsType` Restricted License (bits 0-3 ==
/// 2) or Bitmap embedding only (bit 9) -> `FontEmbeddingRestricted`; anything unreadable ->
/// `FontUnreadable`.
pub(super) fn read(font: &[u8], face_index: u32) -> Result<FontFacts, ExportError> {
    let version = be32(font, 0).ok_or(BAD)?;
    match &version.to_be_bytes() {
        b"ttcf" => return Err(ExportError::FontCollectionNotEmbedded),
        b"OTTO" | b"true" | [0, 1, 0, 0] => {}
        _ => return Err(BAD),
    }
    if face_index != 0 {
        return Err(ExportError::FontCollectionNotEmbedded);
    }
    let mut cff = None;
    if table(font, b"glyf").is_none() {
        match table_range(font, b"CFF ") {
            Some(r) => cff = Some(r),
            None if table(font, b"CFF2").is_some() => return Err(ExportError::FontCffNotEmbedded),
            None => return Err(BAD),
        }
    } else if table(font, b"loca").is_none() {
        return Err(BAD);
    }
    let head = table(font, b"head").ok_or(BAD)?;
    if be32(head, 12) != Some(0x5F0F_3CF5) {
        return Err(BAD);
    }
    let units_per_em = be16(head, 18).filter(|u| (16..=16384).contains(u)).ok_or(BAD)?;
    let bbox = [bei16(head, 36).ok_or(BAD)?, bei16(head, 38).ok_or(BAD)?, bei16(head, 40).ok_or(BAD)?, bei16(head, 42).ok_or(BAD)?];
    let num_glyphs = be16(table(font, b"maxp").ok_or(BAD)?, 4).filter(|n| *n > 0).ok_or(BAD)?;
    let hhea = table(font, b"hhea").ok_or(BAD)?;
    let (ascent, descent) = (bei16(hhea, 4).ok_or(BAD)?, bei16(hhea, 6).ok_or(BAD)?);
    let mut cap_height = None;
    // A font without OS/2 states no restriction (OpenType: OS/2 is required, Apple fonts may omit it).
    if let Some(os2) = table(font, b"OS/2") {
        let fs_type = be16(os2, 8).ok_or(BAD)?;
        if fs_type & 0x000F == 0x0002 || fs_type & 0x0200 != 0 {
            return Err(ExportError::FontEmbeddingRestricted);
        }
        if be16(os2, 0).is_some_and(|v| v >= 2) {
            cap_height = bei16(os2, 88);
        }
    }
    Ok(FontFacts { num_glyphs, units_per_em, bbox, ascent, descent, cap_height, ps_name: ps_name(font), cff })
}

/// `name` id 6 (Windows Unicode UTF-16BE, else Macintosh Roman ASCII), kept only when every char
/// is a PDF-name-safe ASCII letter, digit, `-` or `_`.
fn ps_name(font: &[u8]) -> Option<String> {
    let name = table(font, b"name")?;
    let count = usize::from(be16(name, 2)?);
    let storage = usize::from(be16(name, 4)?);
    let mut best: Option<String> = None;
    for i in 0..count.min(MAX_TABLES) {
        let rec = i.checked_mul(12)?.checked_add(6)?;
        let (platform, name_id) = (be16(name, rec)?, be16(name, rec.checked_add(6)?)?);
        if name_id != 6 || !(platform == 3 || platform == 1) {
            continue;
        }
        let len = usize::from(be16(name, rec.checked_add(8)?)?);
        let off = storage.checked_add(usize::from(be16(name, rec.checked_add(10)?)?))?;
        let raw = name.get(off..off.checked_add(len)?)?;
        let s: String = if platform == 3 {
            raw.chunks_exact(2).map(|c| char::from_u32(u32::from(u16::from_be_bytes([c[0], c[1]]))).unwrap_or('?')).collect()
        } else {
            raw.iter().map(|b| char::from(*b)).collect()
        };
        if !s.is_empty() && s.len() <= MAX_NAME && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            best = Some(s);
            if platform == 3 {
                break;
            }
        }
    }
    best
}
