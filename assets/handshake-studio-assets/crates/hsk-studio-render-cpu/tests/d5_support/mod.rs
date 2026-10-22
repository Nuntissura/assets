// Copied from hsk-studio-motion/tests/d5_support/mod.rs (same workspace, test support only).
//! Shared helpers of the D5 Adobe oracle comparison tests (lane SMB-D5-AI-ID-AE-COMPARE): corpus root
//! resolution per the checked-in `.config/oracle-corpus.toml` (V4-03, no machine-local paths), a
//! FIPS 180-4 SHA-256 for pinning every corpus file read, a bounded JSON reader for the oracle files
//! (written by the session scripts' own `ser`), and the verdict ledger every comparison prints.
//!
//! Outcomes are typed and never silent: a missing root, file or pin mismatch prints a line starting
//! `NOT_PROVEN:` (counted by tools/run-round.sh), every case prints one `D5_VERDICT:` line, and a
//! MISMATCH fails the test unless the checked-in expected-mismatch file lists it with a reason.
#![allow(dead_code)]
#![deny(clippy::arithmetic_side_effects)]
use std::path::{Path, PathBuf};

/// Largest oracle file read (bytes); the AE grid is about 0.5 MB.
pub const MAX_FILE_BYTES: u64 = 8 << 20;

pub fn corpus_root() -> Option<PathBuf> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cfg = std::fs::read_to_string(ws.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        cfg.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let ws = ws.canonicalize().ok()?;
    ws.ancestors().skip(1).take(max).map(|a| a.join(&search)).find(|p| p.is_dir())
}

/// Reads `<root>/<dir>/<file>` and checks its SHA-256 against `pin`; `Err` carries the NOT_PROVEN reason.
pub fn read_pinned(root: &Path, dir: &str, file: &str, pin: &str) -> Result<Vec<u8>, String> {
    let path = root.join(dir).join(file);
    let meta = std::fs::metadata(&path).map_err(|_| format!("{dir}/{file} absent"))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{dir}/{file} larger than {MAX_FILE_BYTES} bytes"));
    }
    let bytes = std::fs::read(&path).map_err(|_| format!("{dir}/{file} unreadable"))?;
    let got = hex(&sha256(&bytes));
    if got != pin {
        return Err(format!("{dir}/{file} sha256 {got} != pinned {pin}"));
    }
    Ok(bytes)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------------------------
// SHA-256 (FIPS 180-4 section 6.2), written from the standard.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64usize {
            let (w15, w2) = (w[i.wrapping_sub(15)], w[i.wrapping_sub(2)]);
            let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
            let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
            w[i] = w[i.wrapping_sub(16)].wrapping_add(s0).wrapping_add(w[i.wrapping_sub(7)]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (chunk, word) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Bounded JSON reader (RFC 8259 values; depth and size bounded; numbers as f64).
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

pub const MAX_DEPTH: usize = 32;

impl J {
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            J::Num(v) => Some(*v),
            _ => None,
        }
    }
    pub fn arr(&self) -> Option<&[J]> {
        match self {
            J::Arr(v) => Some(v),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            J::Str(v) => Some(v),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, J::Null)
    }
    /// Numbers of an array of numbers.
    pub fn nums(&self) -> Option<Vec<f64>> {
        self.arr()?.iter().map(J::num).collect()
    }
    pub fn field_num(&self, key: &str) -> Option<f64> {
        self.get(key)?.num()
    }
}

pub fn parse(text: &str) -> Result<J, String> {
    let mut p = Parser { b: text.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing bytes at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn bump(&mut self) {
        self.i = self.i.saturating_add(1);
    }
    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.bump();
        }
    }
    fn lit(&mut self, word: &str, v: J) -> Result<J, String> {
        let end = self.i.checked_add(word.len()).ok_or("offset overflow")?;
        if self.b.get(self.i..end) == Some(word.as_bytes()) {
            self.i = end;
            Ok(v)
        } else {
            Err(format!("bad literal at {}", self.i))
        }
    }
    fn value(&mut self, depth: usize) -> Result<J, String> {
        if depth > MAX_DEPTH {
            return Err("depth bound".into());
        }
        let next = depth.saturating_add(1);
        self.ws();
        match self.peek() {
            Some(b'n') => self.lit("null", J::Null),
            Some(b't') => self.lit("true", J::Bool(true)),
            Some(b'f') => self.lit("false", J::Bool(false)),
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b'[') => {
                self.bump();
                let mut out = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.bump();
                    return Ok(J::Arr(out));
                }
                loop {
                    out.push(self.value(next)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.bump(),
                        Some(b']') => {
                            self.bump();
                            return Ok(J::Arr(out));
                        }
                        _ => return Err(format!("array syntax at {}", self.i)),
                    }
                }
            }
            Some(b'{') => {
                self.bump();
                let mut out = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.bump();
                    return Ok(J::Obj(out));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(format!("object syntax at {}", self.i));
                    }
                    self.bump();
                    out.push((key, self.value(next)?));
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.bump(),
                        Some(b'}') => {
                            self.bump();
                            return Ok(J::Obj(out));
                        }
                        _ => return Err(format!("object syntax at {}", self.i)),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.bump();
                }
                let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "number utf8")?;
                s.parse::<f64>().map(J::Num).map_err(|_| format!("bad number {s}"))
            }
            _ => Err(format!("unexpected byte at {}", self.i)),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(format!("string expected at {}", self.i));
        }
        self.bump();
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            self.bump();
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| "string utf8".into()),
                b'\\' => {
                    let e = self.peek().ok_or("unterminated escape")?;
                    self.bump();
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let end = self.i.checked_add(4).ok_or("offset overflow")?;
                            let hex4 = self.b.get(self.i..end).ok_or("short \\u escape")?;
                            let s = std::str::from_utf8(hex4).map_err(|_| "\\u utf8")?;
                            let code = u32::from_str_radix(s, 16).map_err(|_| "\\u hex")?;
                            self.i = end;
                            let ch = char::from_u32(code).unwrap_or('\u{fffd}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(format!("bad escape at {}", self.i)),
                    }
                }
                _ => out.push(c),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Verdicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Match,
    Mismatch,
    NotSupported,
    NotProven,
}

impl Verdict {
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Match => "MATCH",
            Verdict::Mismatch => "MISMATCH",
            Verdict::NotSupported => "NOT_SUPPORTED",
            Verdict::NotProven => "NOT_PROVEN",
        }
    }
}

/// Collects one verdict per case variant, prints it, and enforces the expected-mismatch list.
pub struct Ledger {
    pub test: &'static str,
    pub expected: Vec<(String, String, String)>,
    pub rows: Vec<(String, String, Verdict, String)>,
}

impl Ledger {
    /// `expected_text`: the checked-in expected-mismatch file, lines `<case> <variant> | <reason>`
    /// (`#` comments and blank lines ignored).
    pub fn new(test: &'static str, expected_text: &str) -> Self {
        let expected = expected_text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let (head, reason) = l.split_once('|')?;
                let mut it = head.split_whitespace();
                Some((it.next()?.to_owned(), it.next()?.to_owned(), reason.trim().to_owned()))
            })
            .collect();
        Self { test, expected, rows: Vec::new() }
    }

    pub fn record(&mut self, case: &str, variant: &str, verdict: Verdict, detail: String) {
        if verdict == Verdict::NotProven {
            println!("NOT_PROVEN: {} {case} {variant} reason={detail}", self.test);
        }
        println!("D5_VERDICT: {case} {variant} {} {detail}", verdict.word());
        self.rows.push((case.to_owned(), variant.to_owned(), verdict, detail));
    }

    /// Prints the per-verdict counts and panics on any MISMATCH that has no expected-mismatch entry.
    pub fn finish(self) {
        let count = |v: Verdict| self.rows.iter().filter(|r| r.2 == v).count();
        println!(
            "D5_SUMMARY: {} rows={} MATCH={} MISMATCH={} NOT_SUPPORTED={} NOT_PROVEN={}",
            self.test,
            self.rows.len(),
            count(Verdict::Match),
            count(Verdict::Mismatch),
            count(Verdict::NotSupported),
            count(Verdict::NotProven)
        );
        let unexpected: Vec<String> = self
            .rows
            .iter()
            .filter(|r| r.2 == Verdict::Mismatch)
            .filter(|r| !self.expected.iter().any(|e| e.0 == r.0 && (e.1 == r.1 || e.1 == "*")))
            .map(|r| format!("{} {} ({})", r.0, r.1, r.3))
            .collect();
        assert!(unexpected.is_empty(), "unexpected MISMATCH: {unexpected:#?}");
    }
}

/// Largest absolute difference of two equal-length series (`None` on a length difference).
pub fn max_abs(a: &[f64], b: &[f64]) -> Option<f64> {
    (a.len() == b.len()).then(|| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max))
}

// The SHA-256 known-answer self-check runs once, in its owning target (hsk-studio-motion d5_ae_oracle).
