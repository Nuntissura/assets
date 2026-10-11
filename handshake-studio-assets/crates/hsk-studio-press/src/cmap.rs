//! CMap programs (ISO 32000-2 9.7.5 and 9.10.3; syntax per Adobe Technical Note 5014/5411): the
//! `/ToUnicode` map of a font (code -> text) and the facts of an embedded encoding CMap of a composite
//! font (codespace ranges, code -> CID ranges, `/WMode`, `/CMapName`, `usecmap`). Only the sections a
//! viewer reads are interpreted; nothing is executed. Bounded: at most `MAX_ENTRIES` mappings and a
//! code range of at most 65536 codes per range; anything larger is refused (`None` / malformed).
//! Hostile-size tests: `popup_irt_annotations_and_composite_font_appearances` (a four-billion-code
//! range) and `hostile_decoder_bounds_dims_and_aggregate` (code width, entry cap, aggregate budget).
//!
//! As viewers do (pdf.js `cmap.js` at da95557: `parseCMap` stops at `endcmap`, `CharCodeMap` is keyed
//! by the numeric code, a later definition replaces an earlier one; MuPDF `pdf-cmap-parse.c` at
//! 200426a): a `/ToUnicode` map is looked up by the code's NUMERIC value, whatever byte length the
//! map's own entries have; how many bytes a code takes is decided by the font's ENCODING CMap
//! codespace ranges ([`read_code`], pdf.js `CMap.readCharCode`: for n = 1..4 the first n bytes as a
//! number inside an n-byte range), never by the `/ToUnicode` map (R10-F4; test
//! `composite_codes_follow_the_encoding_codespace`).
#![deny(clippy::arithmetic_side_effects)]
use std::collections::BTreeMap;

const MAX_ENTRIES: usize = 262_144;
/// Largest code range one `bfrange` / `cidrange` may span.
const MAX_RANGE: u32 = 65_535;

/// One codespace range: `(byte length, low, high)`, compared as numbers (as pdf.js and MuPDF do).
pub(crate) type Space = (usize, u32, u32);

/// A `/ToUnicode` map.
pub(crate) struct Cmap {
    map: BTreeMap<u32, String>,
    /// Character -> lowest code that maps to exactly that character, built on first use.
    inverse: std::cell::OnceCell<BTreeMap<char, u32>>,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// A string (hexadecimal or literal), as bytes.
    Str(Vec<u8>),
    Name(Vec<u8>),
    Open,
    Close,
    /// A keyword or a number.
    Word(Vec<u8>),
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'/' | b'%' | b'{' | b'}')
}

fn is_space(b: u8) -> bool {
    b.is_ascii_whitespace() || b == 0
}

/// Flat PostScript-like tokens of a CMap program: dictionaries are not grouped (`<<` and `>>` are
/// skipped), so `/WMode 1` reads the same inside a dictionary as outside (pdf.js lexer). Literal
/// strings are decoded (escapes, octal), names have their `#xx` escapes decoded.
/// Returns the tokens and whether a delimiter stood where none can (a `)` or `}` that closes
/// nothing, a single `>`): viewers skip such a token with an error and read on, so what they read
/// around it is not certain.
fn tokenize(data: &[u8]) -> (Vec<Tok>, bool) {
    let mut out = Vec::new();
    let mut stray = false;
    let mut braces = 0usize;
    let mut i = 0usize;
    let at = |i: usize| data.get(i).copied();
    while let Some(b) = at(i) {
        match b {
            b'%' => {
                while at(i).is_some_and(|c| !matches!(c, 10 | 13)) {
                    i = i.saturating_add(1);
                }
            }
            b'<' if at(i.saturating_add(1)) == Some(b'<') => i = i.saturating_add(2),
            b'>' if at(i.saturating_add(1)) == Some(b'>') => i = i.saturating_add(2),
            b'>' => {
                stray = true;
                i = i.saturating_add(1);
            }
            b'{' => {
                braces = braces.saturating_add(1);
                i = i.saturating_add(1);
            }
            b'}' => {
                match braces.checked_sub(1) {
                    Some(d) => braces = d,
                    None => stray = true,
                }
                i = i.saturating_add(1);
            }
            b'<' => {
                let (mut hex, mut hi) = (Vec::new(), None::<u8>);
                i = i.saturating_add(1);
                while let Some(c) = at(i) {
                    if c == b'>' {
                        break;
                    }
                    if let Some(v) = crate::syntax::hex_val(c) {
                        match hi.take() {
                            None => hi = Some(v),
                            Some(h) => hex.push(h.wrapping_mul(16).wrapping_add(v)),
                        }
                    }
                    i = i.saturating_add(1);
                }
                if let Some(h) = hi {
                    hex.push(h.wrapping_mul(16));
                }
                i = i.saturating_add(1);
                out.push(Tok::Str(hex));
            }
            b'[' => {
                out.push(Tok::Open);
                i = i.saturating_add(1);
            }
            b']' => {
                out.push(Tok::Close);
                i = i.saturating_add(1);
            }
            b'(' => {
                let mut depth = 1u32;
                let mut s = Vec::new();
                i = i.saturating_add(1);
                while depth > 0 {
                    let Some(c) = at(i) else { break };
                    i = i.saturating_add(1);
                    match c {
                        b'\\' => {
                            let Some(e) = at(i) else { break };
                            i = i.saturating_add(1);
                            match e {
                                b'n' => s.push(b'\n'),
                                b'r' => s.push(b'\r'),
                                b't' => s.push(b'\t'),
                                b'b' => s.push(8),
                                b'f' => s.push(12),
                                b'0'..=b'7' => {
                                    let mut v = u32::from(e.saturating_sub(b'0'));
                                    for _ in 0..2 {
                                        match at(i) {
                                            Some(d @ b'0'..=b'7') => {
                                                v = v.saturating_mul(8).saturating_add(u32::from(d.saturating_sub(b'0')));
                                                i = i.saturating_add(1);
                                            }
                                            _ => break,
                                        }
                                    }
                                    s.push((v & 0xFF) as u8);
                                }
                                b'\r' => {
                                    if at(i) == Some(b'\n') {
                                        i = i.saturating_add(1);
                                    }
                                }
                                b'\n' => {}
                                other => s.push(other),
                            }
                        }
                        b'(' => {
                            depth = depth.saturating_add(1);
                            s.push(c);
                        }
                        b')' => {
                            depth = depth.saturating_sub(1);
                            if depth > 0 {
                                s.push(c);
                            }
                        }
                        _ => s.push(c),
                    }
                }
                out.push(Tok::Str(s));
            }
            b'/' => {
                i = i.saturating_add(1);
                let mut name = Vec::new();
                while let Some(c) = at(i).filter(|c| !is_space(*c) && !is_delim(*c)) {
                    if c == b'#'
                        && let (Some(h), Some(l)) = (at(i.saturating_add(1)).and_then(crate::syntax::hex_val), at(i.saturating_add(2)).and_then(crate::syntax::hex_val))
                    {
                        name.push(h.wrapping_mul(16).wrapping_add(l));
                        i = i.saturating_add(3);
                        continue;
                    }
                    name.push(c);
                    i = i.saturating_add(1);
                }
                out.push(Tok::Name(name));
            }
            b if is_space(b) => i = i.saturating_add(1),
            _ => {
                let start = i;
                while at(i).is_some_and(|c| !is_space(c) && !is_delim(c)) {
                    i = i.saturating_add(1);
                }
                if i == start {
                    stray = true;
                    i = i.saturating_add(1); // stray delimiter: advance past it
                } else if let Some(w) = data.get(start..i) {
                    out.push(Tok::Word(w.to_vec()));
                }
            }
        }
    }
    (out, stray)
}

/// Numeric value of a code string (big-endian; at most 4 bytes).
fn code(h: &[u8]) -> Option<u32> {
    (1..=4).contains(&h.len()).then(|| h.iter().fold(0u32, |a, b| a.wrapping_shl(8) | u32::from(*b)))
}

/// Destination text of a mapping: UTF-16BE, or one Latin-1 byte.
fn dst_units(dst: &[u8]) -> Vec<u16> {
    if dst.len() >= 2 {
        dst.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
    } else {
        dst.iter().map(|b| u16::from(*b)).collect()
    }
}

fn units_text(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied()).map(|r| r.unwrap_or('\u{FFFD}')).collect()
}

fn word_is(t: Option<&Tok>, w: &[u8]) -> bool {
    matches!(t, Some(Tok::Word(x)) if x == w)
}

fn int_of(t: Option<&Tok>) -> Option<i64> {
    match t {
        Some(Tok::Word(w)) => std::str::from_utf8(w).ok()?.parse().ok(),
        _ => None,
    }
}

/// The program up to `endcmap` (viewers stop there).
fn until_endcmap(toks: &[Tok]) -> &[Tok] {
    let end = toks.iter().position(|t| word_is(Some(t), b"endcmap")).unwrap_or(toks.len());
    toks.get(..end).unwrap_or(&[])
}

pub(crate) fn parse_to_unicode(data: &[u8]) -> Option<Cmap> {
    let (all, _) = tokenize(data);
    let toks = until_endcmap(&all);
    let mut map: BTreeMap<u32, String> = BTreeMap::new();
    let mut i = 0usize;
    let next = |i: usize, by: usize| i.saturating_add(by);
    while let Some(tok) = toks.get(i) {
        i = next(i, 1);
        let Tok::Word(w) = tok else { continue };
        match w.as_slice() {
            b"beginbfchar" => {
                while let (Some(Tok::Str(src)), Some(Tok::Str(dst))) = (toks.get(i), toks.get(next(i, 1))) {
                    map.insert(code(src)?, units_text(&dst_units(dst)));
                    i = next(i, 2);
                    if map.len() > MAX_ENTRIES {
                        return None;
                    }
                }
            }
            b"beginbfrange" => {
                while let (Some(Tok::Str(lo)), Some(Tok::Str(hi))) = (toks.get(i), toks.get(next(i, 1))) {
                    i = next(i, 2);
                    let (first, last) = (code(lo)?, code(hi)?);
                    if last < first || last.saturating_sub(first) > MAX_RANGE {
                        return None;
                    }
                    match toks.get(i) {
                        Some(Tok::Str(dst)) => {
                            i = next(i, 1);
                            let base = dst_units(dst);
                            let mut c = first;
                            for off in 0..=last.saturating_sub(first) {
                                let mut units = base.clone();
                                if let Some(u) = units.last_mut() {
                                    *u = u.wrapping_add(off as u16);
                                }
                                map.insert(c, units_text(&units));
                                c = c.saturating_add(1);
                            }
                        }
                        Some(Tok::Open) => {
                            i = next(i, 1);
                            let mut c = first;
                            while let Some(Tok::Str(d)) = toks.get(i) {
                                map.insert(c, units_text(&dst_units(d)));
                                c = c.saturating_add(1);
                                i = next(i, 1);
                                if map.len() > MAX_ENTRIES {
                                    return None;
                                }
                            }
                            if matches!(toks.get(i), Some(Tok::Close)) {
                                i = next(i, 1);
                            }
                        }
                        _ => return None,
                    }
                    if map.len() > MAX_ENTRIES {
                        return None;
                    }
                }
            }
            _ => {}
        }
    }
    if map.is_empty() {
        return None;
    }
    Some(Cmap { map, inverse: std::cell::OnceCell::new() })
}

impl Cmap {
    /// Text of the code with this numeric value.
    pub(crate) fn lookup(&self, value: u32) -> Option<&str> {
        self.map.get(&value).map(String::as_str)
    }

    /// Text of a string of one-byte codes (a simple font); codes without a mapping are skipped.
    pub(crate) fn read_bytes(&self, s: &[u8]) -> Vec<char> {
        s.iter().filter_map(|b| self.lookup(u32::from(*b))).flat_map(str::chars).collect()
    }

    /// Lowest code value this map reads as exactly the character `c`.
    pub(crate) fn inverse(&self, c: char) -> Option<u32> {
        let inverse = self.inverse.get_or_init(|| {
            let mut inv: BTreeMap<char, u32> = BTreeMap::new();
            for (code, s) in &self.map {
                let mut it = s.chars();
                if let (Some(c), None) = (it.next(), it.next()) {
                    inv.entry(c).or_insert(*code);
                }
            }
            inv
        });
        inverse.get(&c).copied()
    }
}

/// The code at `pos` of `s` under codespace `space`: `(value, byte length)`, or `None` when no range
/// holds it (pdf.js `CMap.readCharCode`; see the module docs).
pub(crate) fn read_code(space: &[Space], s: &[u8], pos: usize) -> Option<(u32, usize)> {
    let mut c = 0u32;
    for n in 1..=4usize {
        let b = *s.get(pos.checked_add(n)?.checked_sub(1)?)?;
        c = c.wrapping_shl(8) | u32::from(b);
        if space.iter().any(|(len, lo, hi)| *len == n && *lo <= c && c <= *hi) {
            return Some((c, n));
        }
    }
    None
}

/// The bytes of code `value` under codespace `space`: the shortest form [`read_code`] reads back as
/// exactly that code.
pub(crate) fn code_bytes(space: &[Space], value: u32) -> Option<Vec<u8>> {
    for n in 1..=4usize {
        let bytes = value.to_be_bytes();
        let b = bytes.get(4usize.checked_sub(n)?..)?;
        if bytes.get(..4usize.checked_sub(n)?)?.iter().any(|x| *x != 0) {
            continue;
        }
        if read_code(space, b, 0) == Some((value, n)) {
            return Some(b.to_vec());
        }
    }
    None
}

/// A `/WMode` value written in a CMap program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WModeTok {
    /// An integer (pdf.js: `Number.isInteger`; MuPDF: an integer token): non-zero is vertical.
    Int(i64),
    /// A number with a fraction part (`1.5`): viewers disagree.
    Other,
}

/// What an embedded encoding CMap program says (up to `endcmap`).
#[derive(Debug, Default)]
pub(crate) struct Program {
    /// `/CMapName` followed by a name.
    pub name: Option<Vec<u8>>,
    /// Every `/WMode` followed by a number, in order.
    pub wmodes: Vec<WModeTok>,
    /// The name right before `usecmap` (`Some(Err)`: `usecmap` with something else before it).
    pub usecmap: Option<std::result::Result<Vec<u8>, ()>>,
    pub space: Vec<Space>,
    /// `(low, high, first CID)` of every `cidrange` and `cidchar` entry, in order.
    pub cid: Vec<(u32, u32, u32)>,
    /// A section that does not have the syntax viewers read (they skip it; its codes are unknown).
    pub malformed: bool,
    /// A delimiter stood where none can (`tokenize`): what viewers read around it is not certain.
    pub stray: bool,
}

/// The integer a word STARTS with when only non-number bytes follow it (`1def` is 1, `0x` is 0); `None`
/// when the word does not start with digits (after one optional sign) or a `.` or digit follows them.
fn leading_int(w: &[u8]) -> Option<i64> {
    let sign = usize::from(matches!(w.first(), Some(b'+' | b'-')));
    let digits = w.get(sign..)?.iter().take_while(|b| b.is_ascii_digit()).count();
    let end = sign.checked_add(digits)?;
    if digits == 0 || digits > 18 || w.get(end).is_some_and(|b| *b == b'.' || *b == b'+' || *b == b'-') {
        return None;
    }
    std::str::from_utf8(w.get(..end)?).ok()?.parse().ok()
}

/// Parses an embedded encoding CMap program (module docs).
pub(crate) fn parse_program(data: &[u8]) -> Program {
    let (all, stray) = tokenize(data);
    let toks = until_endcmap(&all);
    let mut p = Program { stray, ..Program::default() };
    let mut i = 0usize;
    let next = |i: usize, by: usize| i.saturating_add(by);
    while let Some(tok) = toks.get(i) {
        i = next(i, 1);
        match tok {
            Tok::Name(n) if n == b"WMode" => {
                if let Some(Tok::Word(w)) = toks.get(i) {
                    let text = std::str::from_utf8(w).unwrap_or("");
                    if let Ok(v) = text.parse::<i64>() {
                        p.wmodes.push(WModeTok::Int(v));
                    } else if let Ok(v) = text.parse::<f64>() {
                        // pdf.js reads `1.0` as the integer 1; MuPDF ignores a real.
                        p.wmodes.push(if v.fract() == 0.0 && v.is_finite() && v.abs() < 9.0e15 { WModeTok::Int(v as i64) } else { WModeTok::Other });
                    } else if let Some(v) = leading_int(w) {
                        // R11-F6: pdf.js ends a number at the first byte that is not part of it, so
                        // `1def` is 1 followed by `def` (test wmode_spellings_and_identity_names_are_read_as_viewers_do).
                        p.wmodes.push(WModeTok::Int(v));
                    } else if w.first().is_some_and(|b| b.is_ascii_digit() || matches!(*b, b'+' | b'-' | b'.')) {
                        // Any other number-like word (`--1`, `1.5x`): what a viewer reads is not known.
                        p.wmodes.push(WModeTok::Other);
                    }
                }
            }
            Tok::Name(n) if n == b"CMapName" => {
                if let Some(Tok::Name(v)) = toks.get(i) {
                    p.name = Some(v.clone());
                }
            }
            Tok::Word(w) if w == b"usecmap" => {
                p.usecmap = Some(match i.checked_sub(2).and_then(|k| toks.get(k)) {
                    Some(Tok::Name(n)) => Ok(n.clone()),
                    _ => Err(()),
                });
            }
            Tok::Word(w) if w == b"begincodespacerange" => loop {
                match (toks.get(i), toks.get(next(i, 1))) {
                    (Some(Tok::Str(lo)), Some(Tok::Str(hi))) if lo.len() == hi.len() => {
                        match (code(lo), code(hi)) {
                            (Some(a), Some(b)) => p.space.push((lo.len(), a, b)),
                            _ => p.malformed = true,
                        }
                        i = next(i, 2);
                    }
                    (t, _) => {
                        if !word_is(t, b"endcodespacerange") {
                            p.malformed = true;
                        }
                        break;
                    }
                }
            },
            Tok::Word(w) if w == b"begincidrange" => loop {
                match (toks.get(i), toks.get(next(i, 1)), int_of(toks.get(next(i, 2)))) {
                    (Some(Tok::Str(lo)), Some(Tok::Str(hi)), Some(cid)) => {
                        match (code(lo), code(hi), u32::try_from(cid).ok()) {
                            (Some(a), Some(b), Some(c)) if a <= b && b.saturating_sub(a) <= MAX_RANGE => p.cid.push((a, b, c)),
                            _ => p.malformed = true,
                        }
                        i = next(i, 3);
                    }
                    (t, _, _) => {
                        if !word_is(t, b"endcidrange") {
                            p.malformed = true;
                        }
                        break;
                    }
                }
                if p.cid.len() > MAX_ENTRIES {
                    p.malformed = true;
                    break;
                }
            },
            Tok::Word(w) if w == b"begincidchar" => loop {
                match (toks.get(i), int_of(toks.get(next(i, 1)))) {
                    (Some(Tok::Str(src)), Some(cid)) => {
                        match (code(src), u32::try_from(cid).ok()) {
                            (Some(a), Some(c)) => p.cid.push((a, a, c)),
                            _ => p.malformed = true,
                        }
                        i = next(i, 2);
                    }
                    (t, _) => {
                        if !word_is(t, b"endcidchar") {
                            p.malformed = true;
                        }
                        break;
                    }
                }
                if p.cid.len() > MAX_ENTRIES {
                    p.malformed = true;
                    break;
                }
            },
            _ => {}
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tounicode_is_read_by_code_value_and_programs_by_token() {
        // A later definition of the same numeric code wins, whatever its byte length.
        let m = parse_to_unicode(b"1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <0041> <0058> <41> <0059> endbfchar").unwrap();
        assert_eq!(m.lookup(0x41), Some("Y"));
        // Literal strings are strings; nothing after endcmap is read.
        let m = parse_to_unicode(b"1 beginbfchar (A) <0053> endbfchar endcmap 1 beginbfchar <42> <0054> endbfchar").unwrap();
        assert_eq!((m.lookup(0x41), m.lookup(0x42)), (Some("S"), None));
        // /WMode in every spelling a viewer reads; the name before usecmap; codespace and CID ranges.
        for (prog, mode) in [
            ("/WMode 1 def", WModeTok::Int(1)),
            ("/WMode 1 pop", WModeTok::Int(1)),
            ("<< /WMode 1 >> def", WModeTok::Int(1)),
            ("[/WMode 1] def", WModeTok::Int(1)),
            ("/WMode 1.0 def", WModeTok::Int(1)),
            ("/WMode 0.5 def", WModeTok::Other),
        ] {
            assert_eq!(parse_program(prog.as_bytes()).wmodes, vec![mode], "{prog}");
        }
        let p = parse_program(b"/CMapName /Identity-V def /H usecmap 2 begincodespacerange <00> <80> <8140> <9FFC> endcodespacerange 1 begincidrange <0000> <00FF> 5 endcidrange 1 begincidchar <8140> 633 endcidchar endcmap /WMode 1 def");
        assert_eq!(p.name.as_deref(), Some(&b"Identity-V"[..]));
        assert_eq!(p.usecmap, Some(Ok(b"H".to_vec())));
        assert_eq!(p.space, vec![(1, 0, 0x80), (2, 0x8140, 0x9FFC)]);
        assert_eq!(p.cid, vec![(0, 0xFF, 5), (0x8140, 0x8140, 633)]);
        assert!(p.wmodes.is_empty() && !p.malformed, "after endcmap nothing counts");
        assert!(parse_program(b"1 begincodespacerange <00> endcodespacerange").malformed);
        assert!(parse_program(b"begincmap } endcmap").stray && !parse_program(b"{ /a 1 } begincmap << /b 2 >> endcmap").stray);
        // Codes split by the codespace, shortest matching length first (pdf.js readCharCode).
        let space = [(1, 0, 0x80), (2, 0x8140, 0x9FFC)];
        assert_eq!(read_code(&space, &[0x41, 0x81, 0x40], 0), Some((0x41, 1)));
        assert_eq!(read_code(&space, &[0x41, 0x81, 0x40], 1), Some((0x8140, 2)));
        assert_eq!(read_code(&space, &[0xFF], 0), None);
        assert_eq!(code_bytes(&space, 0x8140), Some(vec![0x81, 0x40]));
        assert_eq!(code_bytes(&space, 0x41), Some(vec![0x41]));
        assert_eq!(code_bytes(&[(2, 0, 0xFFFF)], 0x41), Some(vec![0, 0x41]));
    }
}
