//! Support for the oracle tests (SMB-PRISM-ENG-5): a minimal JSON reader (the crate has no serde),
//! fixture-profile writers, CIEDE2000 and a bounded-case runner. Everything here is test-only and
//! shares nothing with the engine under test except the public WideEngine API the tests call.
#![allow(dead_code)]
use crate::wide_support::*;
use hsk_studio_prism::*;

// ---------------------------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------------------------
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}
static NULL: Json = Json::Null;
impl Json {
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser { b: text.as_bytes(), i: 0 };
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing bytes at {}", p.i));
        }
        Ok(v)
    }
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(items) => items.iter().find(|(k, _)| k == key).map_or(&NULL, |(_, v)| v),
            _ => &NULL,
        }
    }
    pub fn has(&self, key: &str) -> bool {
        !matches!(self.get(key), Json::Null)
    }
    pub fn at(&self, i: usize) -> &Json {
        match self {
            Json::Arr(v) => v.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Json::Obj(items) => items.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }
    pub fn f64(&self) -> f64 {
        match self {
            Json::Num(v) => *v,
            other => panic!("expected number, got {other:?}"),
        }
    }
    pub fn i64(&self) -> i64 {
        self.f64() as i64
    }
    pub fn bool(&self) -> bool {
        match self {
            Json::Bool(v) => *v,
            other => panic!("expected bool, got {other:?}"),
        }
    }
    pub fn str(&self) -> &str {
        match self {
            Json::Str(v) => v,
            other => panic!("expected string, got {other:?}"),
        }
    }
    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            other => panic!("expected array, got {other:?}"),
        }
    }
    pub fn f64s(&self) -> Vec<f64> {
        self.arr().iter().map(Json::f64).collect()
    }
}
struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }
    fn eat(&mut self, lit: &str) -> Result<(), String> {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(format!("expected {lit} at {}", self.i))
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i).copied() {
            None => Err("unexpected end".into()),
            Some(b'{') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(Json::Obj(items));
                    }
                    if !items.is_empty() {
                        self.eat(",")?;
                        self.ws();
                    }
                    let k = self.string()?;
                    self.ws();
                    self.eat(":")?;
                    items.push((k, self.value()?));
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Json::Arr(items));
                    }
                    if !items.is_empty() {
                        self.eat(",")?;
                    }
                    items.push(self.value()?);
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.eat("true").map(|_| Json::Bool(true)),
            Some(b'f') => self.eat("false").map(|_| Json::Bool(false)),
            Some(b'n') => self.eat("null").map(|_| Json::Null),
            Some(_) => {
                let start = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                let text = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
                text.parse::<f64>().map(Json::Num).map_err(|e| format!("number {text:?}: {e}"))
            }
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.eat("\"")?;
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("bad escape")?;
                    self.i += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|e| e.to_string())?;
                            self.i += 4;
                            let ch = char::from_u32(u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?)
                                .unwrap_or('\u{fffd}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => return Err(format!("unknown escape {}", other as char)),
                    }
                }
                other => out.push(other),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Bounded case runner and helpers
// ---------------------------------------------------------------------------------------------
pub fn bounded(name: &str, secs: u64, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost {secs}s deadline: {e}"),
    }
}
pub fn intent_of(name: &str) -> Intent {
    match name {
        "relative_colorimetric" => Intent::RelativeColorimetric,
        "perceptual" => Intent::Perceptual,
        "saturation" => Intent::Saturation,
        "absolute_colorimetric" => Intent::AbsoluteColorimetric,
        other => panic!("unknown intent {other}"),
    }
}
pub fn to_f32(values: &[f64]) -> Vec<f32> {
    values.iter().map(|v| *v as f32).collect()
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn sha256_hex(bytes: &[u8]) -> String {
    hash_hex(&profile_hash(bytes))
}

// ---------------------------------------------------------------------------------------------
// T0 profile specifications -> ICC bytes (v4.2 matrix/TRC and Gray kTRC, exact s15Fixed16 values)
// ---------------------------------------------------------------------------------------------
fn xyz_tag_s15(v: [i64; 3]) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        t.extend_from_slice(&(c as i32).to_be_bytes());
    }
    t
}
fn trc_blob(trc: &Json) -> Vec<u8> {
    match trc.get("kind").str() {
        "para" => {
            let mut t = b"para\0\0\0\0".to_vec();
            t.extend_from_slice(&(trc.get("func").i64() as u16).to_be_bytes());
            t.extend_from_slice(&[0, 0]);
            for p in trc.get("params_s15").arr() {
                t.extend_from_slice(&(p.i64() as i32).to_be_bytes());
            }
            t
        }
        "curv_gamma" => {
            let mut t = b"curv\0\0\0\0".to_vec();
            t.extend_from_slice(&be32(1));
            t.extend_from_slice(&(trc.get("u8f8").i64() as u16).to_be_bytes());
            t.extend_from_slice(&[0, 0]);
            t
        }
        "curv_table" => {
            let entries = trc.get("entries_u16").arr();
            let mut t = b"curv\0\0\0\0".to_vec();
            t.extend_from_slice(&be32(entries.len() as u32));
            for e in entries {
                t.extend_from_slice(&(e.i64() as u16).to_be_bytes());
            }
            t
        }
        "curv_identity" => curv(None),
        other => panic!("unknown trc kind {other}"),
    }
}
pub fn t0_profile_bytes(spec: &Json) -> Vec<u8> {
    let w = spec.get("wtpt_s15").arr();
    let wtpt = xyz_tag_s15([w[0].i64(), w[1].i64(), w[2].i64()]);
    let trc = trc_blob(spec.get("trc"));
    let version = if spec.get("version").str() == "2.1" { 0x0210_0000 } else { 0x0420_0000 };
    match spec.get("space").str() {
        "RGB" => {
            let c = spec.get("colorants_s15");
            let col = |i: usize| xyz_tag_s15([c.at(i).at(0).i64(), c.at(i).at(1).i64(), c.at(i).at(2).i64()]);
            assemble(
                version,
                b"mntr",
                b"RGB ",
                b"XYZ ",
                &[wtpt, col(0), col(1), col(2), trc],
                &[(b"wtpt", 0), (b"rXYZ", 1), (b"gXYZ", 2), (b"bXYZ", 3), (b"rTRC", 4), (b"gTRC", 4), (b"bTRC", 4)],
            )
        }
        "GRAY" => assemble(version, b"mntr", b"GRAY", b"XYZ ", &[wtpt, trc], &[(b"wtpt", 0), (b"kTRC", 1)]),
        other => panic!("unknown space {other}"),
    }
}
/// s15Fixed16 value of tag `sig` (an `XYZ ` tag) read straight from profile bytes.
pub fn read_xyz_tag(bytes: &[u8], sig: &[u8; 4]) -> Option<[f64; 3]> {
    let n = u32::from_be_bytes(bytes.get(128..132)?.try_into().ok()?) as usize;
    for i in 0..n {
        let e = 132 + i * 12;
        if &bytes[e..e + 4] == sig {
            let off = u32::from_be_bytes(bytes[e + 4..e + 8].try_into().ok()?) as usize;
            let mut out = [0.0; 3];
            for (k, v) in out.iter_mut().enumerate() {
                *v = f64::from(i32::from_be_bytes(bytes[off + 8 + k * 4..off + 12 + k * 4].try_into().ok()?)) / 65536.0;
            }
            return Some(out);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Color math (f64): XYZ <-> Lab (ICC PCS illuminant) and CIEDE2000 (Sharma, Wu, Dalal 2005)
// ---------------------------------------------------------------------------------------------
pub fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let (eps, kappa) = (216.0 / 24389.0, 24389.0 / 27.0);
    let f = |t: f64| if t > eps { t.cbrt() } else { (kappa * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}
pub fn delta_e_2000(l1: [f64; 3], l2: [f64; 3]) -> f64 {
    let (kl, kc, kh) = (1.0, 1.0, 1.0);
    let (rad, deg) = (std::f64::consts::PI / 180.0, 180.0 / std::f64::consts::PI);
    let c1 = l1[1].hypot(l1[2]);
    let c2 = l2[1].hypot(l2[2]);
    let cbar7 = ((c1 + c2) / 2.0).powi(7);
    let g = 0.5 * (1.0 - (cbar7 / (cbar7 + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * l1[1], (1.0 + g) * l2[1]);
    let (c1p, c2p) = (a1p.hypot(l1[2]), a2p.hypot(l2[2]));
    let hp = |b: f64, ap: f64| {
        if b == 0.0 && ap == 0.0 {
            0.0
        } else {
            let h = b.atan2(ap) * deg;
            if h < 0.0 { h + 360.0 } else { h }
        }
    };
    let (h1p, h2p) = (hp(l1[2], a1p), hp(l2[2], a2p));
    let dlp = l2[0] - l1[0];
    let dcp = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d > 180.0 { d - 360.0 } else if d < -180.0 { d + 360.0 } else { d }
    };
    let dhp_big = 2.0 * (c1p * c2p).sqrt() * (dhp * rad / 2.0).sin();
    let lbar = (l1[0] + l2[0]) / 2.0;
    let cbarp = (c1p + c2p) / 2.0;
    let hbarp = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * ((hbarp - 30.0) * rad).cos() + 0.24 * ((2.0 * hbarp) * rad).cos()
        + 0.32 * ((3.0 * hbarp + 6.0) * rad).cos()
        - 0.20 * ((4.0 * hbarp - 63.0) * rad).cos();
    let dtheta = 30.0 * (-((hbarp - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cbarp.powi(7) / (cbarp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lbar - 50.0).powi(2) / (20.0 + (lbar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cbarp;
    let sh = 1.0 + 0.015 * cbarp * t;
    let rt = -(2.0 * dtheta * rad).sin() * rc;
    let (a, b, c) = (dlp / (kl * sl), dcp / (kc * sc), dhp_big / (kh * sh));
    (a * a + b * b + c * c + rt * b * c).sqrt()
}
