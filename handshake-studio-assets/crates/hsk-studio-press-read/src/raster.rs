//! Raster streams the payload proof cannot search as text (class `raster_unsearchable`).
//!
//! A stream is in the class ONLY when its dictionary is an image XObject (`/Subtype /Image`), or an
//! inline image, AND its filter chain ENDS in `DCTDecode`, `JPXDecode`, `CCITTFaxDecode` or
//! `JBIG2Decode` while every earlier filter is implemented. Crypt, unknown filters, raster filters in
//! any other position, and every non-image stream with an unimplemented filter stay outside the
//! class and block the proof (`PayloadProof::streams_not_decodable`).
//! The part of the chain that can be decoded (the prefix before the raster codec) is still decoded
//! and scanned as bytes: a needle in a JPEG comment segment is a hit; only a needle drawn as pixels
//! is out of reach, and that is what region removal of the image is for. A codec input that is a
//! complete zlib stream is not pixel data: `hidden_zlib` inflates it and the proof scans the result
//! (test `stray_streams_and_hidden_payloads_are_not_kept_text`).
//!
//! Fail closed on the codec name (lead decision PLR-040): the codec input must also have the codec's
//! own structure ([`codec_signature`]), otherwise the stream is NOT raster and blocks as undecodable:
//! - DCT: JPEG SOI, a chain of well-formed marker segments (entropy-coded data after each SOS), EOI,
//!   nothing but white space after it (ITU-T T.81 B.1);
//! - JPX: the JP2 signature box followed by a box chain that covers the data and holds an `ftyp` and
//!   a `jp2c` box, or a bare codestream; every codestream starts with SOC SIZ and ends with EOC
//!   (ISO/IEC 15444-1 A.4, I.4-I.5);
//! - JBIG2: the embedded organisation PDF uses (ISO 32000-2 7.4.7: no file header): a chain of
//!   segment headers with known segment types and known data lengths that covers the data exactly
//!   (ITU-T T.88 7.2);
//! - CCITT (no magic): `/Columns` equal to the image `/Width`, `/Rows` (when given) equal to `/Height`,
//!   K <= 0 (Group 4 or Group 3 one-dimensional), and every row decodes to exactly `/Width` pixels
//!   (ITU-T T.4 tables 2-3, T.6), `/Height` rows, nothing but an end-of-block mark, fill bits and zero
//!   or white-space bytes after them. Mixed Group 3 two-dimensional data (K > 0) is not raster.
//!
//! Tests: `raster_names_need_codec_structure` and the class tests above.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::syntax::{Dict, Object, Stream, is_ws};

/// Class name carried by [`crate::RasterStream`].
pub const RASTER_CLASS: &str = "raster_unsearchable";

pub(crate) fn is_raster_filter(name: &[u8]) -> bool {
    matches!(name, b"DCTDecode" | b"DCT" | b"JPXDecode" | b"CCITTFaxDecode" | b"CCF" | b"JBIG2Decode")
}

/// Filter names of a stream dictionary (`/Filter`; for inline images also the abbreviation `/F`).
/// `None` when the entry is not a name or an array of names.
fn filter_chain(dict: &Dict, inline: bool) -> Option<Vec<Vec<u8>>> {
    let v = dict.get("Filter").or_else(|| if inline { dict.get("F") } else { None });
    match v {
        None | Some(Object::Null) => Some(Vec::new()),
        Some(Object::Name(n)) => Some(vec![n.clone()]),
        Some(Object::Array(a)) => a.iter().map(|o| o.as_name().map(<[u8]>::to_vec)).collect(),
        Some(_) => None,
    }
}

/// Whether `filters` decodes a chain consisting of this one filter (single source of truth).
fn implemented(name: &[u8]) -> bool {
    let mut d = Dict::default();
    d.set("Filter", Object::Name(name.to_vec()));
    filters::is_decodable(&d)
}

/// `Some(raster filter name)` when the chain ends in a raster codec and every earlier filter is
/// implemented.
fn tail_of(chain: &[Vec<u8>]) -> Option<String> {
    let (last, head) = chain.split_last()?;
    (is_raster_filter(last) && head.iter().all(|n| implemented(n))).then(|| String::from_utf8_lossy(last).into_owned())
}

/// Class check for an image XObject stream dictionary.
pub(crate) fn raster_tail(dict: &Dict) -> Option<String> {
    if !dict.get("Subtype").is_some_and(|s| s.is_name("Image")) {
        return None;
    }
    tail_of(&filter_chain(dict, false)?)
}

/// What an inline image's filter chain means for the proof.
pub(crate) enum InlineClass {
    /// No filter, or only implemented filters: its bytes are plain content-stream bytes.
    Searchable,
    /// Ends in a raster codec after implemented filters.
    Raster(String),
    /// Anything else (Crypt, unknown, raster codec not last, malformed entry): blocks the proof.
    Blocking(String),
}

/// Classifies an inline image from the key/value operands recorded for `BI`.
pub(crate) fn classify_inline(operands: &[Object]) -> InlineClass {
    let mut d = Dict::default();
    for pair in operands.chunks(2) {
        if let [Object::Name(k), v] = pair {
            d.set_key(k, v.clone());
        }
    }
    let Some(chain) = filter_chain(&d, true) else {
        return InlineClass::Blocking("filter_entry".into());
    };
    if chain.iter().all(|n| implemented(n)) {
        return InlineClass::Searchable;
    }
    match tail_of(&chain) {
        Some(f) => InlineClass::Raster(f),
        None => {
            let bad = chain.iter().find(|n| !implemented(n)).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
            InlineClass::Blocking(bad)
        }
    }
}

/// Decodes every filter except the final raster codec. The result is the codec's input (a JPEG,
/// JPEG 2000, CCITT or JBIG2 codestream), which is scanned as bytes; `trailing` reports input left
/// after an end-of-data marker of the prefix filters.
pub(crate) fn decode_prefix(stream: &Stream, limits: &Limits, budget: &mut usize) -> Result<filters::Decoded> {
    let chain = filter_chain(&stream.dict, false).ok_or(PressError::Malformed("filter_entry"))?;
    let head = chain.get(..chain.len().saturating_sub(1)).unwrap_or(&[]);
    let mut dict = stream.dict.clone();
    // An explicit (possibly empty) array so the file-specification key `/F` is not read as a filter.
    dict.set("Filter", Object::Array(head.iter().map(|n| Object::Name(n.clone())).collect()));
    filters::decode_chain(&Stream { dict, data: stream.data.clone() }, limits, budget)
}

/// A raster stream whose codec input is itself a complete zlib stream is not pixel data (JPEG,
/// JPEG 2000, CCITT and JBIG2 streams are never valid zlib streams): returns the inflated bytes so
/// the proof scans them, bounded like any decode (a bomb is `InputLimit`), and whether non-whitespace
/// bytes follow the end of that zlib stream (they would be read by nobody). Without this, zlib
/// bytes of a removed string behind a raster filter name would read as unsearchable pixels.
pub(crate) fn hidden_zlib(codec_input: &[u8], limits: &Limits, budget: &mut usize) -> Result<Option<(Vec<u8>, bool)>> {
    let cap = limits.max_decoded_bytes.min(*budget);
    match filters::inflate_zlib(codec_input, cap) {
        Ok((v, used)) => {
            *budget = budget.saturating_sub(v.len());
            let trailing = codec_input.get(used..).is_some_and(|rest| rest.iter().any(|b| !crate::syntax::is_ws(*b)));
            Ok(Some((v, trailing)))
        }
        Err(miniz_oxide::inflate::TINFLStatus::HasMoreOutput) => {
            *budget = budget.saturating_sub(cap);
            Err(PressError::InputLimit(if cap < limits.max_decoded_bytes { "decoded_total" } else { "decoded_bytes" }))
        }
        Err(_) => Ok(None),
    }
}

// ------------------------------------------------------------------ codec signatures (PLR-040)

fn rest_is_ws(data: &[u8], from: usize) -> bool {
    data.get(from..).is_none_or(|r| r.iter().all(|b| is_ws(*b)))
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    let b: [u8; 2] = d.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(usize::from(u16::from_be_bytes(b)))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    let b: [u8; 4] = d.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(b))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    let b: [u8; 8] = d.get(at..at.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_be_bytes(b))
}

/// The codec input has the structure of `filter` (module docs). `Err` carries the typed reason the
/// stream is not raster.
pub(crate) fn codec_signature(filter: &str, data: &[u8], dict: &Dict) -> std::result::Result<(), &'static str> {
    match filter {
        "DCTDecode" | "DCT" => jpeg(data),
        "JPXDecode" => jpx(data),
        "JBIG2Decode" => jbig2(data),
        "CCITTFaxDecode" | "CCF" => ccitt(data, dict),
        _ => Err("not_a_raster_codec"),
    }
}

/// JPEG marker structure (ITU-T T.81 B.1.1): SOI, segments, entropy-coded data after SOS, EOI.
fn jpeg(d: &[u8]) -> std::result::Result<(), &'static str> {
    if !d.starts_with(&[0xFF, 0xD8]) {
        return Err("jpeg_no_soi");
    }
    let mut p = 2usize;
    loop {
        if d.get(p) != Some(&0xFF) {
            return Err("jpeg_marker_expected");
        }
        while d.get(p) == Some(&0xFF) {
            p = p.saturating_add(1);
        }
        let m = *d.get(p).ok_or("jpeg_truncated")?;
        p = p.saturating_add(1);
        match m {
            0xD9 => return if rest_is_ws(d, p) { Ok(()) } else { Err("jpeg_data_after_eoi") },
            0x01 | 0xD0..=0xD7 => {}
            0x00 | 0xD8 => return Err("jpeg_marker"),
            _ => {
                let len = be16(d, p).ok_or("jpeg_truncated")?;
                if len < 2 {
                    return Err("jpeg_segment_length");
                }
                p = p.checked_add(len).filter(|e| *e <= d.len()).ok_or("jpeg_segment_length")?;
                if m == 0xDA {
                    // Entropy-coded data runs to the next marker that is neither a stuffed 0xFF00
                    // nor a restart marker.
                    loop {
                        let at = d.get(p..).and_then(|r| r.iter().position(|b| *b == 0xFF)).ok_or("jpeg_no_eoi")?;
                        let q = p.saturating_add(at);
                        let mut r = q.saturating_add(1);
                        while d.get(r) == Some(&0xFF) {
                            r = r.saturating_add(1);
                        }
                        match d.get(r) {
                            None => return Err("jpeg_no_eoi"),
                            Some(0x00 | 0xD0..=0xD7) => p = r.saturating_add(1),
                            Some(_) => {
                                p = q;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

const JP2_SIGNATURE: [u8; 12] = [0, 0, 0, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A];

/// JPEG 2000 codestream bounds (ISO/IEC 15444-1 A.4): SOC SIZ first, EOC last.
fn jpx_codestream(c: &[u8]) -> std::result::Result<(), &'static str> {
    let end = c.iter().rposition(|b| !is_ws(*b)).map_or(0, |i| i.saturating_add(1));
    let c = c.get(..end).unwrap_or(&[]);
    if c.len() >= 6 && c.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) && c.ends_with(&[0xFF, 0xD9]) { Ok(()) } else { Err("jpx_codestream") }
}

/// JP2/JPX box chain (ISO/IEC 15444-1 I.4) or a bare codestream.
fn jpx(d: &[u8]) -> std::result::Result<(), &'static str> {
    if !d.starts_with(&JP2_SIGNATURE) {
        return jpx_codestream(d);
    }
    let (mut p, mut ftyp, mut jp2c) = (0usize, false, false);
    while !rest_is_ws(d, p) {
        let lbox = be32(d, p).ok_or("jpx_box")?;
        let tbox = d.get(p.saturating_add(4)..p.saturating_add(8)).ok_or("jpx_box")?;
        let (header, len) = match lbox {
            0 => (8usize, d.len().saturating_sub(p)),
            1 => (16usize, usize::try_from(be64(d, p.saturating_add(8)).ok_or("jpx_box")?).map_err(|_| "jpx_box_length")?),
            n => (8usize, usize::try_from(n).map_err(|_| "jpx_box_length")?),
        };
        if len < header {
            return Err("jpx_box_length");
        }
        let end = p.checked_add(len).filter(|e| *e <= d.len()).ok_or("jpx_box_length")?;
        match tbox {
            b"ftyp" => ftyp = true,
            b"jp2c" => {
                jpx_codestream(d.get(p.saturating_add(header)..end).unwrap_or(&[]))?;
                jp2c = true;
            }
            _ => {}
        }
        p = end;
    }
    if ftyp && jp2c { Ok(()) } else { Err("jpx_boxes") }
}

/// Segment types of ITU-T T.88 7.3 (table 2).
const JBIG2_TYPES: [u8; 21] = [0, 4, 6, 7, 16, 20, 22, 23, 36, 38, 39, 40, 42, 43, 48, 49, 50, 51, 52, 53, 62];

/// JBIG2 embedded organisation (ITU-T T.88 7.2; ISO 32000-2 7.4.7 removes the file header).
fn jbig2(d: &[u8]) -> std::result::Result<(), &'static str> {
    if d.starts_with(&[0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Err("jbig2_file_header");
    }
    let (mut p, mut segments) = (0usize, 0usize);
    while !rest_is_ws(d, p) {
        let at = |o: usize| p.checked_add(o).ok_or("jbig2_segment");
        let number = be32(d, p).ok_or("jbig2_segment")?;
        let flags = *d.get(at(4)?).ok_or("jbig2_segment")?;
        if !JBIG2_TYPES.contains(&(flags & 0x3F)) {
            return Err("jbig2_segment_type");
        }
        let referred = *d.get(at(5)?).ok_or("jbig2_segment")?;
        let (count, mut q) = match referred >> 5 {
            5 | 6 => return Err("jbig2_referred"),
            7 => {
                let count = usize::try_from(be32(d, at(5)?).ok_or("jbig2_segment")? & 0x1FFF_FFFF).map_err(|_| "jbig2_referred")?;
                // count + 1 retention bits, rounded up to bytes.
                (count, at(9)?.checked_add(count.checked_add(8).ok_or("jbig2_referred")? / 8).ok_or("jbig2_referred")?)
            }
            short => (usize::from(short), at(6)?),
        };
        let size: usize = if number <= 256 {
            1
        } else if number <= 65_536 {
            2
        } else {
            4
        };
        q = q.checked_add(count.checked_mul(size).ok_or("jbig2_referred")?).ok_or("jbig2_referred")?;
        q = q.checked_add(if flags & 0x40 != 0 { 4 } else { 1 }).ok_or("jbig2_segment")?;
        let len = be32(d, q).ok_or("jbig2_segment")?;
        if len == u32::MAX {
            return Err("jbig2_unknown_length");
        }
        p = q
            .checked_add(4)
            .and_then(|h| h.checked_add(usize::try_from(len).ok()?))
            .filter(|e| *e <= d.len())
            .ok_or("jbig2_segment_length")?;
        segments = segments.saturating_add(1);
    }
    if segments > 0 { Ok(()) } else { Err("jbig2_empty") }
}

/// ITU-T T.4 tables 2 and 3: (code bits, run length); terminating 0..=63, make-up 64..=1728,
/// extended make-up 1792..=2560 (same for both colours).
const WHITE_CODES: &[(&str, u16)] = &[
    ("00110101", 0), ("000111", 1), ("0111", 2), ("1000", 3), ("1011", 4), ("1100", 5), ("1110", 6), ("1111", 7),
    ("10011", 8), ("10100", 9), ("00111", 10), ("01000", 11), ("001000", 12), ("000011", 13), ("110100", 14),
    ("110101", 15), ("101010", 16), ("101011", 17), ("0100111", 18), ("0001100", 19), ("0001000", 20), ("0010111", 21),
    ("0000011", 22), ("0000100", 23), ("0101000", 24), ("0101011", 25), ("0010011", 26), ("0100100", 27),
    ("0011000", 28), ("00000010", 29), ("00000011", 30), ("00011010", 31), ("00011011", 32), ("00010010", 33),
    ("00010011", 34), ("00010100", 35), ("00010101", 36), ("00010110", 37), ("00010111", 38), ("00101000", 39),
    ("00101001", 40), ("00101010", 41), ("00101011", 42), ("00101100", 43), ("00101101", 44), ("00000100", 45),
    ("00000101", 46), ("00001010", 47), ("00001011", 48), ("01010010", 49), ("01010011", 50), ("01010100", 51),
    ("01010101", 52), ("00100100", 53), ("00100101", 54), ("01011000", 55), ("01011001", 56), ("01011010", 57),
    ("01011011", 58), ("01001010", 59), ("01001011", 60), ("00110010", 61), ("00110011", 62), ("00110100", 63),
    ("11011", 64), ("10010", 128), ("010111", 192), ("0110111", 256), ("00110110", 320), ("00110111", 384),
    ("01100100", 448), ("01100101", 512), ("01101000", 576), ("01100111", 640), ("011001100", 704), ("011001101", 768),
    ("011010010", 832), ("011010011", 896), ("011010100", 960), ("011010101", 1024), ("011010110", 1088),
    ("011010111", 1152), ("011011000", 1216), ("011011001", 1280), ("011011010", 1344), ("011011011", 1408),
    ("010011000", 1472), ("010011001", 1536), ("010011010", 1600), ("011000", 1664), ("010011011", 1728),
    ("00000001000", 1792), ("00000001100", 1856), ("00000001101", 1920), ("000000010010", 1984), ("000000010011", 2048),
    ("000000010100", 2112), ("000000010101", 2176), ("000000010110", 2240), ("000000010111", 2304),
    ("000000011100", 2368), ("000000011101", 2432), ("000000011110", 2496), ("000000011111", 2560),
];
const BLACK_CODES: &[(&str, u16)] = &[
    ("0000110111", 0), ("010", 1), ("11", 2), ("10", 3), ("011", 4), ("0011", 5), ("0010", 6), ("00011", 7),
    ("000101", 8), ("000100", 9), ("0000100", 10), ("0000101", 11), ("0000111", 12), ("00000100", 13), ("00000111", 14),
    ("000011000", 15), ("0000010111", 16), ("0000011000", 17), ("0000001000", 18), ("00001100111", 19),
    ("00001101000", 20), ("00001101100", 21), ("00000110111", 22), ("00000101000", 23), ("00000010111", 24),
    ("00000011000", 25), ("000011001010", 26), ("000011001011", 27), ("000011001100", 28), ("000011001101", 29),
    ("000001101000", 30), ("000001101001", 31), ("000001101010", 32), ("000001101011", 33), ("000011010010", 34),
    ("000011010011", 35), ("000011010100", 36), ("000011010101", 37), ("000011010110", 38), ("000011010111", 39),
    ("000001101100", 40), ("000001101101", 41), ("000011011010", 42), ("000011011011", 43), ("000001010100", 44),
    ("000001010101", 45), ("000001010110", 46), ("000001010111", 47), ("000001100100", 48), ("000001100101", 49),
    ("000001010010", 50), ("000001010011", 51), ("000000100100", 52), ("000000110111", 53), ("000000111000", 54),
    ("000000100111", 55), ("000000101000", 56), ("000001011000", 57), ("000001011001", 58), ("000000101011", 59),
    ("000000101100", 60), ("000001011010", 61), ("000001100110", 62), ("000001100111", 63), ("0000001111", 64),
    ("000011001000", 128), ("000011001001", 192), ("000001011011", 256), ("000000110011", 320), ("000000110100", 384),
    ("000000110101", 448), ("0000001101100", 512), ("0000001101101", 576), ("0000001001010", 640),
    ("0000001001011", 704), ("0000001001100", 768), ("0000001001101", 832), ("0000001110010", 896),
    ("0000001110011", 960), ("0000001110100", 1024), ("0000001110101", 1088), ("0000001110110", 1152),
    ("0000001110111", 1216), ("0000001010010", 1280), ("0000001010011", 1344), ("0000001010100", 1408),
    ("0000001010101", 1472), ("0000001011010", 1536), ("0000001011011", 1600), ("0000001100100", 1664),
    ("0000001100101", 1728), ("00000001000", 1792), ("00000001100", 1856), ("00000001101", 1920),
    ("000000010010", 1984), ("000000010011", 2048), ("000000010100", 2112), ("000000010101", 2176),
    ("000000010110", 2240), ("000000010111", 2304), ("000000011100", 2368), ("000000011101", 2432),
    ("000000011110", 2496), ("000000011111", 2560),
];
/// T.4 4.2.1.3.2 / T.6 two-dimensional mode codes. Values: 0 pass, 1 horizontal, 2..=8 vertical
/// offset -3..=3 (value - 5), 9 extension, 10 EOL (or half of EOFB).
const MODE_CODES: &[(&str, u16)] = &[
    ("0001", 0),
    ("001", 1),
    ("0000010", 2),
    ("000010", 3),
    ("010", 4),
    ("1", 5),
    ("011", 6),
    ("000011", 7),
    ("0000011", 8),
    ("0000001", 9),
    ("000000000001", 10),
];

const PEEK: u32 = 13;
type Lut = Vec<Option<(u16, u8)>>;

fn lut(codes: &[(&str, u16)]) -> Lut {
    let mut out: Lut = vec![None; 1 << PEEK];
    for (bits, value) in codes {
        let len = u32::try_from(bits.len()).unwrap_or(PEEK).min(PEEK);
        let code = u32::from_str_radix(bits, 2).unwrap_or(0);
        let spread = PEEK.saturating_sub(len);
        let base = code.checked_shl(spread).unwrap_or(0);
        for suffix in 0..1u32.checked_shl(spread).unwrap_or(1) {
            if let Some(slot) = usize::try_from(base | suffix).ok().and_then(|i| out.get_mut(i)) {
                *slot = Some((*value, u8::try_from(len).unwrap_or(0)));
            }
        }
    }
    out
}

fn luts() -> &'static (Lut, Lut, Lut) {
    static LUTS: std::sync::OnceLock<(Lut, Lut, Lut)> = std::sync::OnceLock::new();
    LUTS.get_or_init(|| (lut(WHITE_CODES), lut(BLACK_CODES), lut(MODE_CODES)))
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn left(&self) -> usize {
        self.data.len().saturating_mul(8).saturating_sub(self.pos)
    }
    fn peek(&self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let bit = self.pos.saturating_add(usize::try_from(i).unwrap_or(0));
            let b = self.data.get(bit / 8).map_or(0, |byte| byte.wrapping_shr(u32::try_from(7usize.saturating_sub(bit % 8)).unwrap_or(0)) & 1);
            v = v.wrapping_shl(1) | u32::from(b);
        }
        v
    }
    fn code(&mut self, table: &Lut) -> Option<u16> {
        let (v, len) = (*table.get(usize::try_from(self.peek(PEEK)).ok()?)?)?;
        if usize::from(len) > self.left() {
            return None;
        }
        self.pos = self.pos.saturating_add(usize::from(len));
        Some(v)
    }
    /// One run: make-up codes followed by a terminating code.
    fn run(&mut self, table: &Lut, max: u32) -> Option<u32> {
        let mut total = 0u32;
        loop {
            let v = self.code(table)?;
            total = total.checked_add(u32::from(v)).filter(|t| *t <= max)?;
            if v < 64 {
                return Some(total);
            }
        }
    }
    fn align(&mut self) {
        let rem = self.pos % 8;
        if rem != 0 {
            self.pos = self.pos.saturating_add(8usize.saturating_sub(rem));
        }
    }
    /// Zero fill bits then an EOL (eleven zeros and a one); consumed only when present.
    fn eol(&mut self) -> bool {
        let mut zeros = 0usize;
        while zeros < self.left() && self.peek_at(zeros) == 0 {
            zeros = zeros.saturating_add(1);
        }
        if zeros >= 11 && zeros < self.left() {
            self.pos = self.pos.saturating_add(zeros).saturating_add(1);
            return true;
        }
        false
    }
    fn peek_at(&self, offset: usize) -> u8 {
        let bit = self.pos.saturating_add(offset);
        self.data.get(bit / 8).map_or(0, |byte| byte.wrapping_shr(u32::try_from(7usize.saturating_sub(bit % 8)).unwrap_or(0)) & 1)
    }
    /// Only zero bits remain.
    fn rest_zero(&self) -> bool {
        (0..self.left()).all(|i| self.peek_at(i) == 0)
    }
}

const MAX_CCITT_COLUMNS: u32 = 1 << 20;
const MAX_CCITT_ROWS: u32 = 1 << 24;

/// One T.6 two-dimensional row against `reference` (changing elements). `Ok(true)`: EOFB found.
fn g4_row(r: &mut BitReader<'_>, w: u32, reference: &[u32], cur: &mut Vec<u32>) -> std::result::Result<bool, &'static str> {
    let (white_lut, black_lut, mode_lut) = luts();
    cur.clear();
    let mut a0: i64 = -1;
    let mut white = true;
    while a0 < i64::from(w) {
        let mode = r.code(mode_lut).ok_or("ccitt_mode")?;
        let start = reference.partition_point(|x| i64::from(*x) <= a0);
        let i = if (start % 2 == 0) == white { start } else { start.saturating_add(1) };
        let b1 = reference.get(i).copied().unwrap_or(w).min(w);
        let b2 = reference.get(i.saturating_add(1)).copied().unwrap_or(w).min(w);
        match mode {
            0 => a0 = i64::from(b2),
            1 => {
                let from = u32::try_from(a0.max(0)).map_err(|_| "ccitt_row")?;
                let (first, second) = if white { (white_lut, black_lut) } else { (black_lut, white_lut) };
                let a1 = from.checked_add(r.run(first, w).ok_or("ccitt_run")?).filter(|v| *v <= w).ok_or("ccitt_run")?;
                let a2 = a1.checked_add(r.run(second, w).ok_or("ccitt_run")?).filter(|v| *v <= w).ok_or("ccitt_run")?;
                cur.extend([a1, a2]);
                a0 = i64::from(a2);
            }
            2..=8 => {
                let a1 = i64::from(b1).checked_add(i64::from(mode)).and_then(|v| v.checked_sub(5)).ok_or("ccitt_row")?;
                if a1 > i64::from(w) || a1 < 0 || (a0 >= 0 && a1 <= a0) {
                    return Err("ccitt_vertical");
                }
                cur.push(u32::try_from(a1).map_err(|_| "ccitt_row")?);
                white = !white;
                a0 = a1;
            }
            10 if a0 < 0 && cur.is_empty() => return Ok(true),
            _ => return Err("ccitt_mode"),
        }
        if cur.len() > usize::try_from(w).unwrap_or(usize::MAX).saturating_add(2) {
            return Err("ccitt_row");
        }
    }
    Ok(false)
}

/// One T.4 one-dimensional (modified Huffman) row.
fn mh_row(r: &mut BitReader<'_>, w: u32) -> std::result::Result<(), &'static str> {
    let (white_lut, black_lut, _) = luts();
    let (mut a, mut white) = (0u32, true);
    loop {
        let run = r.run(if white { white_lut } else { black_lut }, w).ok_or("ccitt_run")?;
        a = a.checked_add(run).filter(|v| *v <= w).ok_or("ccitt_run")?;
        if a == w {
            return Ok(());
        }
        white = !white;
    }
}

fn int_of(d: Option<&Dict>, key: &str) -> Option<i64> {
    d.and_then(|d| d.get(key)).and_then(Object::as_int)
}

fn bool_of(d: Option<&Dict>, key: &str, default: bool) -> bool {
    match d.and_then(|d| d.get(key)) {
        Some(Object::Bool(b)) => *b,
        _ => default,
    }
}

/// `/DecodeParms` entry of the LAST filter of the chain.
fn last_parms(dict: &Dict) -> Option<&Dict> {
    let n = filter_chain(dict, false).map_or(0, |c| c.len());
    match dict.get("DecodeParms") {
        Some(Object::Dict(d)) if n == 1 => Some(d),
        Some(Object::Array(a)) => a.get(n.checked_sub(1)?).and_then(Object::as_dict),
        _ => None,
    }
}

/// Size a raster codec decodes `data` to, read from the codec's own header BEFORE any decoder runs
/// (R13-F5; `render_bounds.rs` sizes the renderer's allocations with it): `(width, height,
/// components)`, each the largest the header declares. JPEG: the SOF frame header (ITU-T T.81 B.2.2);
/// JPEG 2000: the `ihdr` box and every `SIZ` marker (ISO/IEC 15444-1 I.5.3.1, A.5.1); JBIG2: the page
/// information segment and every region segment information field (ITU-T T.88 7.4.1, 7.4.8); CCITT:
/// `/Columns` (default 1728) and `/Rows` of the decode parameters (`dict` is the image dictionary whose
/// last filter is the codec). `None` when the header is not readable (the codec then refuses too).
pub(crate) fn codec_dims(filter: &[u8], data: &[u8], dict: &Dict) -> Option<(u64, u64, u64)> {
    match filter {
        b"DCTDecode" | b"DCT" => jpeg_dims(data),
        b"JPXDecode" => jpx_dims(data),
        b"JBIG2Decode" => jbig2_dims(data),
        b"CCITTFaxDecode" | b"CCF" => {
            let parms = last_parms(dict);
            let columns = int_of(parms, "Columns").unwrap_or(1728);
            let rows = int_of(parms, "Rows").unwrap_or(0);
            Some((u64::try_from(columns).ok()?, u64::try_from(rows).unwrap_or(0), 1))
        }
        _ => None,
    }
}

fn jpeg_dims(d: &[u8]) -> Option<(u64, u64, u64)> {
    let mut p = 2usize;
    if !d.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    while p < d.len() {
        if d.get(p) != Some(&0xFF) {
            p = p.checked_add(1)?;
            continue;
        }
        let m = *d.get(p.checked_add(1)?)?;
        match m {
            0xFF => p = p.checked_add(1)?,
            0x00 | 0x01 | 0xD0..=0xD9 => p = p.checked_add(2)?,
            0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => {
                // FF Cn, Lf (2), P (1), Y (2), X (2), Nf (1)
                let h = be16(d, p.checked_add(5)?)?;
                let w = be16(d, p.checked_add(7)?)?;
                let nf = *d.get(p.checked_add(9)?)?;
                return Some((u64::try_from(w).ok()?, u64::try_from(h).ok()?, u64::from(nf)));
            }
            _ => p = p.checked_add(2)?.checked_add(be16(d, p.checked_add(2)?)?)?,
        }
    }
    None
}

/// Largest of every `SIZ` (Xsiz, Ysiz, Csiz) in a codestream starting at `c`.
fn siz_dims(c: &[u8]) -> Option<(u64, u64, u64)> {
    if !c.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
        return None;
    }
    let w = be32(c, 8)?;
    let h = be32(c, 12)?;
    let n = be16(c, 40)?;
    Some((u64::from(w), u64::from(h), u64::try_from(n).ok()?))
}

fn jpx_dims(d: &[u8]) -> Option<(u64, u64, u64)> {
    if !d.starts_with(&JP2_SIGNATURE) {
        return siz_dims(d);
    }
    let mut best: Option<(u64, u64, u64)> = None;
    let mut keep = |v: Option<(u64, u64, u64)>| {
        if let Some((w, h, n)) = v {
            let b = best.unwrap_or((0, 0, 0));
            best = Some((b.0.max(w), b.1.max(h), b.2.max(n)));
        }
    };
    // Top-level boxes and the boxes of the `jp2h` super-box (one level: ihdr lives there).
    let mut stack: Vec<(usize, usize)> = vec![(0, d.len())];
    let mut boxes = 0usize;
    while let Some((mut p, end)) = stack.pop() {
        while p < end {
            boxes = boxes.checked_add(1)?;
            if boxes > 4096 {
                return best;
            }
            let lbox = be32(d, p)?;
            let tbox = d.get(p.checked_add(4)?..p.checked_add(8)?)?;
            let (header, len) = match lbox {
                0 => (8usize, end.checked_sub(p)?),
                1 => (16usize, usize::try_from(be64(d, p.checked_add(8)?)?).ok()?),
                n => (8usize, usize::try_from(n).ok()?),
            };
            let body = p.checked_add(header)?;
            let next = p.checked_add(len).filter(|e| *e <= end && len >= header)?;
            match tbox {
                b"jp2h" => stack.push((body, next)),
                b"ihdr" => keep(be32(d, body.checked_add(4)?).zip(be32(d, body)).zip(be16(d, body.checked_add(8)?)).and_then(|((w, h), n)| {
                    Some((u64::from(w), u64::from(h), u64::try_from(n).ok()?))
                })),
                b"jp2c" => keep(siz_dims(d.get(body..next)?)),
                _ => {}
            }
            p = next;
        }
    }
    best
}

/// JBIG2 region segment types carrying a region segment information field (T.88 7.3 table 2).
const JBIG2_REGIONS: [u8; 12] = [4, 6, 7, 20, 22, 23, 36, 38, 39, 40, 42, 43];

fn jbig2_dims(d: &[u8]) -> Option<(u64, u64, u64)> {
    let (mut p, mut w, mut h, mut segments) = (0usize, 0u64, 0u64, 0usize);
    while p < d.len() && !rest_is_ws(d, p) {
        segments = segments.checked_add(1)?;
        if segments > 1 << 20 {
            break;
        }
        let number = be32(d, p)?;
        let flags = *d.get(p.checked_add(4)?)?;
        let referred = *d.get(p.checked_add(5)?)?;
        let (count, mut q) = match referred >> 5 {
            7 => {
                let count = usize::try_from(be32(d, p.checked_add(5)?)? & 0x1FFF_FFFF).ok()?;
                (count, p.checked_add(9)?.checked_add(count.checked_add(8)? / 8)?)
            }
            short => (usize::from(short), p.checked_add(6)?),
        };
        let size: usize = if number <= 256 {
            1
        } else if number <= 65_536 {
            2
        } else {
            4
        };
        q = q.checked_add(count.checked_mul(size)?)?;
        q = q.checked_add(if flags & 0x40 != 0 { 4 } else { 1 })?;
        let len = be32(d, q)?;
        let data = q.checked_add(4)?;
        let kind = flags & 0x3F;
        if kind == 48 || JBIG2_REGIONS.contains(&kind) {
            // Page information and region segment information both start with width, height.
            let sw = be32(d, data)?;
            let sh = be32(d, data.checked_add(4)?)?;
            // A striped page of unknown height (0xFFFFFFFF) grows to at most the decoder's 65535 rows.
            w = w.max(u64::from(sw).min(65_535));
            h = h.max(u64::from(sh).min(65_535));
        }
        if len == u32::MAX {
            break;
        }
        p = data.checked_add(usize::try_from(len).ok()?)?;
    }
    (w > 0 && h > 0).then_some((w, h, 1))
}

/// CCITT data that decodes to exactly `/Width` x `/Height` (module docs).
fn ccitt(data: &[u8], dict: &Dict) -> std::result::Result<(), &'static str> {
    let parms = last_parms(dict);
    let k = int_of(parms, "K").unwrap_or(0);
    if k > 0 {
        return Err("ccitt_mixed_2d");
    }
    let columns = int_of(parms, "Columns").unwrap_or(1728);
    let width = int_of(Some(dict), "Width").ok_or("ccitt_no_width")?;
    let height = int_of(Some(dict), "Height").ok_or("ccitt_no_height")?;
    let rows = int_of(parms, "Rows").unwrap_or(0);
    if columns != width || (rows != 0 && rows != height) {
        return Err("ccitt_dimensions");
    }
    let w = u32::try_from(width).ok().filter(|v| (1..=MAX_CCITT_COLUMNS).contains(v)).ok_or("ccitt_dimensions")?;
    let h = u32::try_from(height).ok().filter(|v| (1..=MAX_CCITT_ROWS).contains(v)).ok_or("ccitt_dimensions")?;
    let byte_align = bool_of(parms, "EncodedByteAlign", false);
    let mut r = BitReader { data, pos: 0 };
    let (mut reference, mut cur): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
    let mut decoded = 0u32;
    while decoded < h {
        if byte_align {
            r.align();
        }
        if k < 0 {
            if r.peek(24) == 0x00_10_01 {
                break;
            }
            if g4_row(&mut r, w, &reference, &mut cur)? {
                break;
            }
            std::mem::swap(&mut reference, &mut cur);
        } else {
            let mut eols = 0u32;
            while r.eol() {
                eols = eols.saturating_add(1);
            }
            if eols >= 2 || r.rest_zero() {
                break; // RTC, or the data ended
            }
            mh_row(&mut r, w)?;
        }
        decoded = decoded.saturating_add(1);
    }
    if decoded != h {
        return Err("ccitt_rows");
    }
    // After the last row: an end-of-block mark (EOFB or RTC), fill bits, then zero or white-space bytes.
    while r.eol() {}
    r.align();
    if rest_is_ws(data, r.pos / 8) { Ok(()) } else { Err("ccitt_data_after_rows") }
}
