//! PDF text-string codec (ISO 32000-2 7.9.2.2 and Annex D.2): PDFDocEncoding, UTF-16BE with the
//! `FE FF` byte-order mark, UTF-8 with the `EF BB BF` mark. UTF-16LE (`FF FE`) is not defined by
//! the standard but occurs in the wild, so it is decoded (and written back as UTF-16BE) rather than
//! being scanned as PDFDocEncoding garbage. Decoding is lossless for matching; encoding is only
//! used for strings that were rewritten.
#![deny(clippy::arithmetic_side_effects)]

/// PDFDocEncoding 0x18..=0x1F (Table D.2: breve, caron, circumflex, dotaccent, hungarumlaut,
/// ogonek, ring, tilde).
const LOW: [char; 8] = ['\u{2D8}', '\u{2C7}', '\u{2C6}', '\u{2D9}', '\u{2DD}', '\u{2DB}', '\u{2DA}', '\u{2DC}'];

/// PDFDocEncoding 0x80..=0x9E (bullet ... zcaron). 0x9F is undefined; 0xA0 is the Euro sign.
const HIGH: [char; 31] = [
    '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{192}', '\u{2044}', '\u{2039}', '\u{203A}',
    '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}',
    '\u{FB02}', '\u{141}', '\u{152}', '\u{160}', '\u{178}', '\u{17D}', '\u{131}', '\u{142}', '\u{153}', '\u{161}', '\u{17E}',
];

/// How a decoded string was stored; `encode_text` keeps the family when the new text allows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Enc {
    PdfDoc,
    Utf16Be,
    Utf16Le,
    Utf8,
}

pub(crate) fn pdfdoc_char(b: u8) -> char {
    match b {
        0x18..=0x1F => LOW.get(usize::from(b.wrapping_sub(0x18))).copied().unwrap_or(char::from(b)),
        0x80..=0x9E => HIGH.get(usize::from(b.wrapping_sub(0x80))).copied().unwrap_or(char::from(b)),
        0xA0 => '\u{20AC}',
        _ => char::from(b),
    }
}

/// Inverse of [`pdfdoc_char`]. Undefined positions (0x7F, 0x9F, 0xAD) round-trip as themselves so
/// untouched text re-encodes unchanged.
pub(crate) fn pdfdoc_byte(c: char) -> Option<u8> {
    match c as u32 {
        0x1B => Some(0x1B),
        u @ (0..=0x17 | 0x20..=0x7F | 0x9F | 0xA1..=0xFF) => Some(u as u8),
        _ => LOW
            .iter()
            .position(|x| *x == c)
            .map(|i| 0x18u8.wrapping_add(i as u8))
            .or_else(|| HIGH.iter().position(|x| *x == c).map(|i| 0x80u8.wrapping_add(i as u8)))
            .or((c == '\u{20AC}').then_some(0xA0)),
    }
}

pub(crate) fn decode_text(bytes: &[u8]) -> (Vec<char>, Enc) {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]));
        (char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect(), Enc::Utf16Be)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]));
        (char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect(), Enc::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (String::from_utf8_lossy(rest).chars().collect(), Enc::Utf8)
    } else {
        // Byte 0x1B is read as ESC (U+001B), not the dotaccent of Table D.2: viewers treat it as the
        // language-escape marker in a PDFDocEncoded text string too (pdf.js `stringToPDFString` drops
        // 0x1B..0x1B), so matching sees and drops the escape (`norm.rs`; R9-P2, test
        // `language_escapes_and_invisible_characters_never_hide_text`). It encodes back to 0x1B.
        (bytes.iter().map(|b| if *b == 0x1B { '\u{1B}' } else { pdfdoc_char(*b) }).collect(), Enc::PdfDoc)
    }
}

pub(crate) fn utf16be_bytes(chars: &[char]) -> Vec<u8> {
    let mut out = Vec::with_capacity(chars.len().saturating_mul(2));
    let mut buf = [0u16; 2];
    for c in chars {
        for unit in c.encode_utf16(&mut buf) {
            out.extend_from_slice(&unit.to_be_bytes());
        }
    }
    out
}

pub(crate) fn encode_text(chars: &[char], hint: Enc) -> Vec<u8> {
    let utf16 = || {
        let mut out = vec![0xFE, 0xFF];
        out.extend(utf16be_bytes(chars));
        out
    };
    match hint {
        Enc::Utf8 => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend(chars.iter().collect::<String>().into_bytes());
            out
        }
        Enc::PdfDoc => match chars.iter().map(|c| pdfdoc_byte(*c)).collect::<Option<Vec<u8>>>() {
            // A PDFDocEncoding string that happens to start with a byte-order-mark spelling would
            // be misread as Unicode, so those go through UTF-16BE.
            Some(v) if !v.starts_with(&[0xFE, 0xFF]) && !v.starts_with(&[0xFF, 0xFE]) && !v.starts_with(&[0xEF, 0xBB, 0xBF]) => v,
            _ => utf16(),
        },
        Enc::Utf16Be | Enc::Utf16Le => utf16(),
    }
}
