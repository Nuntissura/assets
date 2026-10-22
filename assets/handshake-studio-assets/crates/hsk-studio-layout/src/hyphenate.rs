//! Liang pattern hyphenation (Knuth-Liang, TeX). The en-US pattern set is the `hyph-en-us` file of
//! the hyph-utf8 package (Gerard D.C. Kuiken, 1990/2004/2005; Knuth's hyphen.tex patterns plus the
//! ushyphmax additions and exception list). Licence: copying and distribution, with or without
//! modification, are permitted in any medium without royalty provided the copyright notice and the
//! licence notice are preserved (recorded verbatim in `data/hyph-en-us.txt` and in [`EN_US_LICENCE`]).
//! Only ASCII letters hyphenate; a word containing any other alphabetic character is left whole,
//! never guessed. Pure and deterministic: lookups only, no iteration over hashed state.
#![deny(clippy::arithmetic_side_effects)]
use std::collections::HashMap;
use std::sync::OnceLock;

/// Licence and provenance of the embedded en-US patterns.
pub const EN_US_LICENCE: &str = "hyph-en-us (hyph-utf8): Copyright (C) 1990, 2004, 2005 Gerard D.C. Kuiken. Copying and distribution of this file, with or without modification, are permitted in any medium without royalty provided the copyright notice and this notice are preserved. Source https://github.com/hyphenation/tex-hyphen";
const EN_US_DATA: &str = include_str!("../data/hyph-en-us.txt");

/// Exceptions observed in the InDesign 21.6 oracle (D5 ID-01..ID-04 line ends, all hyphenation
/// points of each word; points the oracle never showed follow the Liang patterns): InDesign breaks
/// `ev-ery` (Liang: none), `differenc-es` (Liang: `dif-fer-ences`) and never `Extra-ordinarily`
/// (it breaks `Ex-`, `Extraor-`, `Extraordi-`).
pub const INDESIGN_EN_US_EXCEPTIONS: &[&str] = &["ev-ery", "dif-fer-enc-es", "ex-traor-di-nar-i-ly"];

/// Words longer than this many bytes are never hyphenated (hostile-input bound).
pub const MAX_WORD_BYTES: usize = 64;
const MAX_PATTERN_LINES: usize = 200_000;
const MAX_PATTERN_LEN: usize = 32;

/// Hyphenation limits. Defaults are InDesign's Hyphenation Settings defaults (words of at least 5
/// letters, 2 letters before the first break, 2 after the last, at most 3 consecutive hyphens);
/// equality with InDesign output is UNVERIFIED.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HyphenationParams {
    pub min_word_len: usize,
    /// Minimum letters before a hyphen.
    pub min_before: usize,
    /// Minimum letters after a hyphen.
    pub min_after: usize,
    /// Maximum consecutive hyphenated line ends (0 = unlimited). Every `u8` value is accepted: a
    /// word holds at most `MAX_WORD_BYTES - 1` breaks, so a limit at or above that never binds.
    pub consecutive_limit: u8,
    pub hyphenate_capitalized: bool,
    /// Advance of the hyphen glyph in points (shaped by the caller with the line's font).
    pub hyphen_advance_pt: f64,
    /// TeX `\hyphenpenalty`.
    pub hyphen_penalty: f64,
    /// TeX `\doublehyphendemerits`, added when two consecutive lines both end in a hyphen.
    pub double_hyphen_demerits: f64,
}
impl HyphenationParams {
    /// Refuses limits outside their domain: `min_word_len`, `min_before` or `min_after` above
    /// [`MAX_WORD_BYTES`] (no hyphenable word can satisfy them), `min_before + min_after` above
    /// [`MAX_WORD_BYTES`] (checked add, so `usize::MAX` refuses instead of overflowing), a negative
    /// or non-finite hyphen advance, a non-finite penalty, and negative or non-finite double-hyphen
    /// demerits. `consecutive_limit` accepts its whole `u8` range (see the field).
    pub fn validate(&self) -> Result<(), HyphenationError> {
        let ints_ok = self.min_word_len <= MAX_WORD_BYTES
            && self.min_before <= MAX_WORD_BYTES
            && self.min_after <= MAX_WORD_BYTES
            && self
                .min_before
                .checked_add(self.min_after)
                .is_some_and(|sum| sum <= MAX_WORD_BYTES);
        let floats_ok = self.hyphen_advance_pt.is_finite()
            && self.hyphen_advance_pt >= 0.0
            && self.hyphen_penalty.is_finite()
            && self.double_hyphen_demerits.is_finite()
            && self.double_hyphen_demerits >= 0.0;
        if ints_ok && floats_ok {
            Ok(())
        } else {
            Err(HyphenationError::InvalidParams)
        }
    }
}
impl Default for HyphenationParams {
    fn default() -> Self {
        Self {
            min_word_len: 5,
            min_before: 2,
            min_after: 2,
            consecutive_limit: 3,
            hyphenate_capitalized: true,
            hyphen_advance_pt: 0.0,
            hyphen_penalty: 50.0,
            double_hyphen_demerits: 10_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HyphenationError {
    /// A pattern file line is not `[.a-z0-9]` text, is empty, too long, or the file is too large.
    BadPatternData,
    /// A [`HyphenationParams`] limit is out of its domain (see [`HyphenationParams::validate`]).
    InvalidParams,
}
impl HyphenationError {
    /// Stable snake_case code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadPatternData => "bad_pattern_data",
            Self::InvalidParams => "invalid_hyphenation_params",
        }
    }
}
impl std::fmt::Display for HyphenationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for HyphenationError {}

/// A compiled Liang pattern set plus an exception list.
#[derive(Debug)]
pub struct Hyphenator {
    patterns: HashMap<Vec<u8>, Vec<u8>>,
    exceptions: HashMap<Vec<u8>, Vec<usize>>,
    max_pattern: usize,
}

impl Hyphenator {
    /// The embedded American English set (parsed once).
    pub fn en_us() -> &'static Hyphenator {
        static SET: OnceLock<Hyphenator> = OnceLock::new();
        SET.get_or_init(|| {
            Hyphenator::from_pattern_text(EN_US_DATA).expect("embedded en-US pattern data is valid")
        })
    }

    /// The embedded en-US set plus [`INDESIGN_EN_US_EXCEPTIONS`]: the hyphenation points InDesign
    /// 21.6 (`$ID/English: USA`) used in the D5 oracle where they differ from the Liang patterns
    /// (lane SMB-ID-COMPOSER-PARITY). InDesign's dictionary itself is not available to Studio, so any
    /// other word where the two disagree stays a typed gap (`d5_id_expected_mismatch.txt`).
    pub fn en_us_indesign() -> &'static Hyphenator {
        static SET: OnceLock<Hyphenator> = OnceLock::new();
        SET.get_or_init(|| {
            let mut set = Hyphenator::from_pattern_text(EN_US_DATA).expect("embedded en-US pattern data is valid");
            for word in INDESIGN_EN_US_EXCEPTIONS {
                set.add_exception(word).expect("embedded InDesign exception list is valid");
            }
            set
        })
    }

    /// Parses `[patterns]` (one TeX pattern per line, digits between letters, `.` word boundary)
    /// then `[exceptions]` (hyphenated lowercase words); `#` comment lines are ignored.
    pub fn from_pattern_text(text: &str) -> Result<Self, HyphenationError> {
        let mut set = Self {
            patterns: HashMap::new(),
            exceptions: HashMap::new(),
            max_pattern: 0,
        };
        let mut section = 0u8;
        for (n, line) in text.lines().enumerate() {
            if n >= MAX_PATTERN_LINES {
                return Err(HyphenationError::BadPatternData);
            }
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line {
                "[patterns]" => section = 1,
                "[exceptions]" => section = 2,
                _ if line.len() > MAX_PATTERN_LEN => return Err(HyphenationError::BadPatternData),
                _ if section == 1 => set.add_pattern(line)?,
                _ if section == 2 => set.add_exception(line)?,
                _ => return Err(HyphenationError::BadPatternData),
            }
        }
        Ok(set)
    }

    fn add_pattern(&mut self, line: &str) -> Result<(), HyphenationError> {
        let (mut letters, mut levels) = (Vec::new(), vec![0u8]);
        for b in line.bytes() {
            match b {
                b'0'..=b'9' => {
                    // `levels` starts with one entry and only grows; b is an ASCII digit.
                    if let Some(last) = levels.last_mut() {
                        *last = b.wrapping_sub(b'0');
                    }
                }
                b'a'..=b'z' | b'.' => {
                    letters.push(b);
                    levels.push(0);
                }
                _ => return Err(HyphenationError::BadPatternData),
            }
        }
        if letters.is_empty() {
            return Err(HyphenationError::BadPatternData);
        }
        self.max_pattern = self.max_pattern.max(letters.len());
        self.patterns.insert(letters, levels);
        Ok(())
    }

    fn add_exception(&mut self, line: &str) -> Result<(), HyphenationError> {
        let (mut word, mut points) = (Vec::new(), Vec::new());
        for b in line.bytes() {
            match b {
                b'-' => points.push(word.len()),
                b'a'..=b'z' => word.push(b),
                _ => return Err(HyphenationError::BadPatternData),
            }
        }
        if word.is_empty() {
            return Err(HyphenationError::BadPatternData);
        }
        self.exceptions.insert(word, points);
        Ok(())
    }

    /// Break positions inside `word` as letter counts `k` (a hyphen may follow the first `k`
    /// letters), ascending, honouring `min_before` / `min_after`. Empty when `word` is not pure
    /// ASCII letters, is shorter than `min_word_len`, longer than [`MAX_WORD_BYTES`], or capitalised
    /// with `hyphenate_capitalized` off, and when `p` is refused by [`HyphenationParams::validate`]
    /// ([`Hyphenator::try_points`] reports that refusal as a typed error).
    pub fn points(&self, word: &str, p: &HyphenationParams) -> Vec<usize> {
        self.try_points(word, p).unwrap_or_default()
    }

    /// [`Hyphenator::points`] with the parameter refusal typed: `Err(InvalidParams)` when `p` fails
    /// [`HyphenationParams::validate`]. All index arithmetic is checked or saturating.
    pub fn try_points(
        &self,
        word: &str,
        p: &HyphenationParams,
    ) -> Result<Vec<usize>, HyphenationError> {
        p.validate()?;
        let n = word.len();
        // validate() bounds the sum by MAX_WORD_BYTES; checked again so no path can overflow.
        let sides = p
            .min_before
            .checked_add(p.min_after)
            .ok_or(HyphenationError::InvalidParams)?;
        if n > MAX_WORD_BYTES
            || n < p.min_word_len.max(sides).max(2)
            || !word.bytes().all(|b| b.is_ascii_alphabetic())
        {
            return Ok(Vec::new());
        }
        if !p.hyphenate_capitalized && word.as_bytes()[0].is_ascii_uppercase() {
            return Ok(Vec::new());
        }
        let lower: Vec<u8> = word.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let allowed = |k: usize| {
            k >= p.min_before.max(1)
                && k.checked_add(p.min_after.max(1)).is_some_and(|end| end <= n)
        };
        if let Some(points) = self.exceptions.get(&lower) {
            return Ok(points.iter().copied().filter(|k| allowed(*k)).collect());
        }
        // n <= MAX_WORD_BYTES here, so the saturating sums below never saturate.
        let len = n.saturating_add(2);
        let mut text = Vec::with_capacity(len);
        text.push(b'.');
        text.extend_from_slice(&lower);
        text.push(b'.');
        let mut levels = vec![0u8; len.saturating_add(1)];
        for i in 0..len {
            let hi = i.saturating_add(self.max_pattern).min(len);
            for j in i.saturating_add(1)..=hi {
                if let Some(pat) = text.get(i..j).and_then(|key| self.patterns.get(key)) {
                    for (o, v) in pat.iter().enumerate() {
                        if let Some(slot) = levels.get_mut(i.saturating_add(o)) {
                            *slot = (*slot).max(*v);
                        }
                    }
                }
            }
        }
        Ok((1..n)
            .filter(|k| levels.get(k.saturating_add(1)).is_some_and(|v| v & 1 == 1) && allowed(*k))
            .collect())
    }

    /// Every hyphenable word of `text` as `(word start byte, break bytes)`; break bytes are absolute
    /// byte offsets in `text` at which a hyphen may be inserted. Words are maximal alphanumeric
    /// runs; a run that holds any char other than an ASCII letter is skipped whole.
    pub fn hyphenate_text(&self, text: &str, p: &HyphenationParams) -> Vec<(usize, Vec<usize>)> {
        let mut out = Vec::new();
        let mut start: Option<usize> = None;
        let mut tainted = false;
        let mut flush = |start: &mut Option<usize>, end: usize, tainted: &mut bool| {
            if let Some(s) = start.take()
                && !*tainted
            {
                let pts = self.points(&text[s..end], p);
                if !pts.is_empty() {
                    out.push((s, pts.into_iter().map(|k| s.saturating_add(k)).collect()));
                }
            }
            *tainted = false;
        };
        for (i, c) in text.char_indices() {
            if c.is_ascii_alphabetic() {
                start.get_or_insert(i);
            } else if c.is_alphanumeric() {
                tainted = true;
                start.get_or_insert(i);
            } else {
                flush(&mut start, i, &mut tainted);
            }
        }
        flush(&mut start, text.len(), &mut tainted);
        out
    }
}

/// Renders `word` with `-` at its break points (test and diagnostic aid).
pub fn hyphenated(h: &Hyphenator, word: &str, p: &HyphenationParams) -> String {
    let pts = h.points(word, p);
    let mut s = String::with_capacity(word.len().saturating_add(pts.len()));
    for (k, c) in word.chars().enumerate() {
        if pts.contains(&k) {
            s.push('-');
        }
        s.push(c);
    }
    s
}
