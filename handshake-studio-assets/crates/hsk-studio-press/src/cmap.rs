//! `/ToUnicode` CMap reader (ISO 32000 9.10.3; syntax per Adobe Technical Note 5411): maps the
//! character codes of show-operator strings to Unicode text so text drawn with composite (or
//! custom-encoded) fonts can be matched. Only `begincodespacerange`, `beginbfchar` and
//! `beginbfrange` are interpreted; nothing is executed. Bounded: at most `MAX_ENTRIES` mappings and
//! a code range of at most 65536 codes per `bfrange`; anything larger is refused (`None`).
//! Hostile-size tests: `popup_irt_annotations_and_composite_font_appearances` (a four-billion-code
//! range) and `hostile_decoder_bounds_dims_and_aggregate` (code width, entry cap, aggregate budget).
#![deny(clippy::arithmetic_side_effects)]
use std::collections::{BTreeMap, BTreeSet};

const MAX_ENTRIES: usize = 262_144;
/// Largest code range one `bfrange` may span.
const MAX_RANGE: u32 = 65_535;

pub(crate) struct Cmap {
    /// Code lengths in bytes, ascending.
    lens: Vec<usize>,
    map: BTreeMap<(usize, u32), String>,
}

enum Tok {
    Hex(Vec<u8>),
    Open,
    Close,
    Word(Vec<u8>),
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'/' | b'%')
}

fn tokenize(data: &[u8]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&b) = data.get(i) {
        match b {
            b'%' => {
                while data.get(i).is_some_and(|c| !matches!(c, 10 | 13)) {
                    i = i.saturating_add(1);
                }
            }
            b'<' if data.get(i.saturating_add(1)) == Some(&b'<') => i = i.saturating_add(2),
            b'>' => i = i.saturating_add(1),
            b'<' => {
                let (mut hex, mut hi) = (Vec::new(), None::<u8>);
                i = i.saturating_add(1);
                while let Some(&c) = data.get(i) {
                    if c == b'>' {
                        break;
                    }
                    if let Some(v) = char::from(c).to_digit(16) {
                        let v = v as u8;
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
                out.push(Tok::Hex(hex));
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
                i = i.saturating_add(1);
                while depth > 0 {
                    let Some(&c) = data.get(i) else { break };
                    match c {
                        b'\\' => i = i.saturating_add(1),
                        b'(' => depth = depth.saturating_add(1),
                        b')' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    i = i.saturating_add(1);
                }
            }
            b'/' => {
                i = i.saturating_add(1);
                while data.get(i).is_some_and(|c| !c.is_ascii_whitespace() && !is_delim(*c)) {
                    i = i.saturating_add(1);
                }
            }
            b if b.is_ascii_whitespace() || b == 0 => i = i.saturating_add(1),
            _ => {
                let start = i;
                while data.get(i).is_some_and(|c| !c.is_ascii_whitespace() && *c != 0 && !is_delim(*c)) {
                    i = i.saturating_add(1);
                }
                if i == start {
                    i = i.saturating_add(1); // stray delimiter: advance past it
                } else if let Some(w) = data.get(start..i) {
                    out.push(Tok::Word(w.to_vec()));
                }
            }
        }
    }
    out
}

fn code(h: &[u8]) -> u32 {
    h.iter().fold(0u32, |a, b| a.wrapping_shl(8) | u32::from(*b))
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

pub(crate) fn parse_to_unicode(data: &[u8]) -> Option<Cmap> {
    let toks = tokenize(data);
    let (mut space, mut seen): (BTreeSet<usize>, BTreeSet<usize>) = (BTreeSet::new(), BTreeSet::new());
    let mut map: BTreeMap<(usize, u32), String> = BTreeMap::new();
    let mut i = 0usize;
    let next = |i: usize, by: usize| i.saturating_add(by);
    while let Some(tok) = toks.get(i) {
        i = next(i, 1);
        let Tok::Word(w) = tok else { continue };
        match w.as_slice() {
            b"begincodespacerange" => {
                while let (Some(Tok::Hex(lo)), Some(Tok::Hex(_))) = (toks.get(i), toks.get(next(i, 1))) {
                    if !(1..=4).contains(&lo.len()) {
                        return None;
                    }
                    space.insert(lo.len());
                    i = next(i, 2);
                }
            }
            b"beginbfchar" => {
                while let (Some(Tok::Hex(src)), Some(Tok::Hex(dst))) = (toks.get(i), toks.get(next(i, 1))) {
                    if !(1..=4).contains(&src.len()) {
                        return None;
                    }
                    seen.insert(src.len());
                    map.insert((src.len(), code(src)), units_text(&dst_units(dst)));
                    i = next(i, 2);
                    if map.len() > MAX_ENTRIES {
                        return None;
                    }
                }
            }
            b"beginbfrange" => {
                while let (Some(Tok::Hex(lo)), Some(Tok::Hex(hi))) = (toks.get(i), toks.get(next(i, 1))) {
                    i = next(i, 2);
                    let (first, last) = (code(lo), code(hi));
                    if !(1..=4).contains(&lo.len()) || last < first || last.saturating_sub(first) > MAX_RANGE {
                        return None;
                    }
                    seen.insert(lo.len());
                    match toks.get(i) {
                        Some(Tok::Hex(dst)) => {
                            i = next(i, 1);
                            let base = dst_units(dst);
                            let mut c = first;
                            for off in 0..=last.saturating_sub(first) {
                                let mut units = base.clone();
                                if let Some(u) = units.last_mut() {
                                    *u = u.wrapping_add(off as u16);
                                }
                                map.insert((lo.len(), c), units_text(&units));
                                c = c.saturating_add(1);
                            }
                        }
                        Some(Tok::Open) => {
                            i = next(i, 1);
                            let mut c = first;
                            while let Some(Tok::Hex(d)) = toks.get(i) {
                                map.insert((lo.len(), c), units_text(&dst_units(d)));
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
    let lens: Vec<usize> = if space.is_empty() { seen.into_iter().collect() } else { space.into_iter().collect() };
    Some(Cmap { lens, map })
}

impl Cmap {
    /// Text of a show-operator string; codes without a mapping are skipped.
    pub(crate) fn decode(&self, s: &[u8]) -> Vec<char> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < s.len() {
            let mut step = None;
            for &l in &self.lens {
                let Some(chunk) = s.get(pos..pos.saturating_add(l)).filter(|c| c.len() == l) else { continue };
                if let Some(t) = self.map.get(&(l, code(chunk))) {
                    out.extend(t.chars());
                    step = Some(l);
                    break;
                }
            }
            pos = pos.saturating_add(step.unwrap_or_else(|| self.lens.first().copied().unwrap_or(1).max(1)));
        }
        out
    }
}
