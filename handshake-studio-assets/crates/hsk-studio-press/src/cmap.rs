//! `/ToUnicode` CMap reader (ISO 32000 9.10.3; syntax per Adobe Technical Note 5411): maps the
//! character codes of show-operator strings to Unicode text so text drawn with composite (or
//! custom-encoded) fonts can be matched. Only `begincodespacerange`, `beginbfchar` and
//! `beginbfrange` are interpreted; nothing is executed. Bounded: at most `MAX_ENTRIES` mappings and
//! a code range of at most 65536 codes per `bfrange`; anything larger is refused (`None`).
use std::collections::{BTreeMap, BTreeSet};

const MAX_ENTRIES: usize = 262_144;

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

fn tokenize(data: &[u8]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'%' => {
                while i < data.len() && !matches!(data[i], 10 | 13) {
                    i += 1;
                }
            }
            b'<' if data.get(i + 1) == Some(&b'<') => i += 2,
            b'>' => i += 1,
            b'<' => {
                let (mut hex, mut hi) = (Vec::new(), None::<u8>);
                i += 1;
                while i < data.len() && data[i] != b'>' {
                    if let Some(v) = char::from(data[i]).to_digit(16) {
                        match hi.take() {
                            None => hi = Some(v as u8),
                            Some(h) => hex.push(h * 16 + v as u8),
                        }
                    }
                    i += 1;
                }
                if let Some(h) = hi {
                    hex.push(h * 16);
                }
                i += 1;
                out.push(Tok::Hex(hex));
            }
            b'[' => {
                out.push(Tok::Open);
                i += 1;
            }
            b']' => {
                out.push(Tok::Close);
                i += 1;
            }
            b'(' => {
                let mut depth = 1;
                i += 1;
                while i < data.len() && depth > 0 {
                    match data[i] {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'/' => {
                i += 1;
                while i < data.len() && !data[i].is_ascii_whitespace() && !matches!(data[i], b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'/' | b'%') {
                    i += 1;
                }
            }
            b if b.is_ascii_whitespace() || b == 0 => i += 1,
            _ => {
                let start = i;
                while i < data.len() && !data[i].is_ascii_whitespace() && data[i] != 0 && !matches!(data[i], b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'/' | b'%') {
                    i += 1;
                }
                if i == start {
                    i += 1; // stray delimiter: always advance
                } else {
                    out.push(Tok::Word(data[start..i].to_vec()));
                }
            }
        }
    }
    out
}

fn code(h: &[u8]) -> u32 {
    h.iter().fold(0u32, |a, b| (a << 8) | u32::from(*b))
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
    let mut i = 0;
    while i < toks.len() {
        let Tok::Word(w) = &toks[i] else {
            i += 1;
            continue;
        };
        i += 1;
        match w.as_slice() {
            b"begincodespacerange" => {
                while let (Some(Tok::Hex(lo)), Some(Tok::Hex(_))) = (toks.get(i), toks.get(i + 1)) {
                    if !(1..=4).contains(&lo.len()) {
                        return None;
                    }
                    space.insert(lo.len());
                    i += 2;
                }
            }
            b"beginbfchar" => {
                while let (Some(Tok::Hex(src)), Some(Tok::Hex(dst))) = (toks.get(i), toks.get(i + 1)) {
                    if !(1..=4).contains(&src.len()) {
                        return None;
                    }
                    seen.insert(src.len());
                    map.insert((src.len(), code(src)), units_text(&dst_units(dst)));
                    i += 2;
                }
            }
            b"beginbfrange" => {
                while let (Some(Tok::Hex(lo)), Some(Tok::Hex(hi))) = (toks.get(i), toks.get(i + 1)) {
                    i += 2;
                    if !(1..=4).contains(&lo.len()) || code(hi) < code(lo) || code(hi) - code(lo) > 65_535 {
                        return None;
                    }
                    seen.insert(lo.len());
                    let (first, last) = (code(lo), code(hi));
                    match toks.get(i) {
                        Some(Tok::Hex(dst)) => {
                            i += 1;
                            let base = dst_units(dst);
                            for (off, c) in (first..=last).enumerate() {
                                let mut units = base.clone();
                                if let Some(u) = units.last_mut() {
                                    *u = u.wrapping_add(off as u16);
                                }
                                map.insert((lo.len(), c), units_text(&units));
                            }
                        }
                        Some(Tok::Open) => {
                            i += 1;
                            let mut c = first;
                            while let Some(Tok::Hex(d)) = toks.get(i) {
                                map.insert((lo.len(), c), units_text(&dst_units(d)));
                                c = c.saturating_add(1);
                                i += 1;
                            }
                            if matches!(toks.get(i), Some(Tok::Close)) {
                                i += 1;
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
        if map.len() > MAX_ENTRIES {
            return None;
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
        let mut pos = 0;
        while pos < s.len() {
            let mut step = None;
            for &l in &self.lens {
                if pos + l <= s.len()
                    && let Some(t) = self.map.get(&(l, code(&s[pos..pos + l])))
                {
                    out.extend(t.chars());
                    step = Some(l);
                    break;
                }
            }
            pos += step.unwrap_or_else(|| self.lens.first().copied().unwrap_or(1).max(1));
        }
        out
    }
}
