//! Real compressed proprietary RAW samples (raw.pixls.us, CC0-1.0, test DATA only, never committed
//! or executed) against the compressed vendor decoders (lane SMB-DEVELOP-RAW-COMPRESSED).
//! `#[ignore]`d: run with
//! `cargo nextest run -p hsk-studio-develop --test real_raw_compressed --run-ignored ignored-only --no-capture`.
//!
//! Corpus: `<oracle corpus root>/raw-vendor/` (root from `.config/oracle-corpus.toml`, as in
//! `tests/real_raw.rs`); every file is pinned by the sha256 of the corpus `provenance.json`. An
//! absent root, file or hash prints `NOT_PROVEN:` and proves nothing.
//!
//! Oracles per file: (1) known dimensions/bit depth/CFA (byte inspection of the raw IFD / header);
//! (2) a pinned sha256 of the decoded CFA plane (determinism + regression); (3) INDEPENDENT: the
//! camera's own embedded JPEG preview (largest JPEG in the file, decoded by zune-jpeg) correlated
//! with the decoded CFA plane on a coarse block grid (Pearson r of sqrt(raw - floor) vs preview
//! luma); a wrong codec gives noise (r near 0). The r floor of each file is set below the value
//! measured on the first run (recorded in the lane notes), not chosen a priori.
use hsk_studio_develop::*;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).expect("crates/<crate>").to_path_buf()
}

fn convention_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

fn corpus_dir() -> Result<PathBuf, String> {
    let ws = workspace_root();
    let text = std::fs::read_to_string(ws.join(".config").join("oracle-corpus.toml"))
        .map_err(|e| format!("corpus_convention_absent error={e}"))?;
    let search = convention_value(&text, "search").ok_or("corpus_convention_malformed key=search")?;
    let max: usize = convention_value(&text, "max-ancestors")
        .and_then(|v| v.parse().ok())
        .ok_or("corpus_convention_malformed key=max-ancestors")?;
    ws.ancestors()
        .take(max + 1)
        .map(|a| a.join(&search))
        .find(|c| c.is_dir())
        .map(|root| root.join("raw-vendor"))
        .ok_or_else(|| format!("corpus_root_absent search={search}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read(test: &str, file: &str, sha256: &str) -> Option<Vec<u8>> {
    let dir = match corpus_dir() {
        Ok(dir) => dir,
        Err(reason) => {
            println!("NOT_PROVEN: {test} {file} reason={reason}");
            return None;
        }
    };
    let Ok(bytes) = std::fs::read(dir.join(file)) else {
        println!("NOT_PROVEN: {test} {file} reason=file_absent");
        return None;
    };
    let got = hex(&Sha256::digest(&bytes));
    if got != sha256 {
        println!("NOT_PROVEN: {test} {file} reason=sha256_mismatch got={got}");
        return None;
    }
    Some(bytes)
}

fn samples_digest(raw: &DngRaw) -> String {
    let mut h = Sha256::new();
    for y in 0..raw.height {
        for x in 0..raw.width {
            h.update(raw.sample(x, y).to_le_bytes());
        }
    }
    hex(&h.finalize())
}

/// Largest embedded JPEG (width, height, luma); `None` when no decodable JPEG is present.
fn preview_luma(bytes: &[u8]) -> Option<(usize, usize, Vec<f64>)> {
    let mut best: Option<(usize, usize, usize)> = None;
    for i in 0..bytes.len().saturating_sub(4) {
        if bytes[i] != 0xFF || bytes[i + 1] != 0xD8 || bytes[i + 2] != 0xFF {
            continue;
        }
        let mut d = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes[i..]));
        if d.decode_headers().is_err() {
            continue;
        }
        let Some(info) = d.info() else { continue };
        let (w, h) = (usize::from(info.width), usize::from(info.height));
        if w * h > best.map_or(0, |b| b.1 * b.2) {
            best = Some((i, w, h));
        }
    }
    let (at, w, h) = best.filter(|b| b.1 >= 160 && b.2 >= 120)?;
    let mut d = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes[at..]));
    let px = d.decode().ok()?;
    let c = d.output_colorspace()?.num_components();
    if px.len() < w * h * c {
        return None;
    }
    Some((w, h, px.chunks_exact(c).take(w * h).map(|p| p.iter().map(|&v| f64::from(v)).sum::<f64>() / c as f64).collect()))
}

/// Mean of `plane` (`w x h`) over a `gx x gy` grid of equal cells.
fn grid(plane: impl Fn(usize, usize) -> f64, w: usize, h: usize, gx: usize, gy: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(gx * gy);
    for cy in 0..gy {
        for cx in 0..gx {
            let (x0, x1) = (cx * w / gx, (cx + 1) * w / gx);
            let (y0, y1) = (cy * h / gy, (cy + 1) * h / gy);
            let mut s = 0.0;
            let mut n = 0.0f64;
            for y in (y0..y1).step_by(2) {
                for x in x0..x1 {
                    s += plane(x, y);
                    n += 1.0;
                }
            }
            out.push(s / n.max(1.0));
        }
    }
    out
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa.sqrt() * sbb.sqrt()).max(f64::MIN_POSITIVE)
}

/// Correlation of the decoded CFA plane with the embedded preview on a 48-cell-wide grid; a
/// portrait preview of a landscape frame is compared after both quarter turns (max taken).
fn preview_r(raw: &DngRaw, bytes: &[u8]) -> Option<(f64, usize, usize)> {
    let (pw, ph, luma) = preview_luma(bytes)?;
    let (w, h) = (raw.width as usize, raw.height as usize);
    let mut floor = u16::MAX;
    for y in (0..raw.height).step_by(13) {
        for x in (0..raw.width).step_by(11) {
            floor = floor.min(raw.sample(x, y));
        }
    }
    let rawv = |x: usize, y: usize| f64::from(raw.sample(x as u32, y as u32).saturating_sub(floor)).sqrt();
    let gx = 48;
    let rotated = (pw > ph) != (w > h);
    let gy = (gx * if rotated { w } else { h } / if rotated { h } else { w }).max(8);
    let prev = |x: usize, y: usize| luma[y * pw + x];
    let p = grid(prev, pw, ph, gx, gy);
    let r = if !rotated {
        pearson(&grid(rawv, w, h, gx, gy), &p)
    } else {
        // raw rotated 90 cw: (x, y) of the turned frame (h wide, w tall) maps to raw (y, h-1-x).
        let cw = grid(|x, y| rawv(y, h - 1 - x), h, w, gx, gy);
        let ccw = grid(|x, y| rawv(w - 1 - y, x), h, w, gx, gy);
        pearson(&cw, &p).max(pearson(&ccw, &p))
    };
    Some((r, pw, ph))
}

/// Same-colour steps two columns and two rows apart: a wrong bit layout makes one explode.
fn steps(raw: &DngRaw, period: u32) -> (u64, u64) {
    let (mut hs, mut vs) = (0u64, 0u64);
    for y in (0..raw.height - period).step_by(7) {
        for x in (0..raw.width - period).step_by(5) {
            hs += u64::from(raw.sample(x, y).abs_diff(raw.sample(x + period, y)));
            vs += u64::from(raw.sample(x, y).abs_diff(raw.sample(x, y + period)));
        }
    }
    (hs, vs)
}

struct Expect {
    file: &'static str,
    sha256: &'static str,
    dims: (u32, u32, u16),
    white: u32,
    cfa: CfaPattern,
    model: &'static str,
    digest: &'static str,
    r_floor: f64,
}

/// Decode, print the measured facts, and return every mismatch against `e` (the caller asserts
/// after all files ran, so one run reports every file).
fn check(test: &str, e: &Expect) -> Vec<String> {
    let Some(bytes) = read(test, e.file, e.sha256) else { return Vec::new() };
    let started = std::time::Instant::now();
    let raw = match decode_container(&bytes) {
        Ok(raw) => raw,
        Err(err) => return vec![format!("{}: decode refused {err}", e.file)],
    };
    let elapsed = started.elapsed();
    let digest = samples_digest(&raw);
    let period = if raw.cfa_pattern == CfaPattern::XTrans { 6 } else { 2 };
    let (hs, vs) = steps(&raw, period);
    let r = preview_r(&raw, &bytes);
    println!(
        "{}: {}x{} bits {} white {} cfa {:?} model {:?} samples_sha256={digest} hstep={hs} vstep={vs} preview_r={:?} decode={elapsed:?}",
        e.file, raw.width, raw.height, raw.bits_per_sample, raw.white_level, raw.cfa_pattern, raw.unique_camera_model, r
    );
    let mut bad = Vec::new();
    let mut want = |ok: bool, what: String| {
        if !ok {
            bad.push(format!("{}: {what}", e.file));
        }
    };
    let dims = (raw.width, raw.height, raw.bits_per_sample);
    want(dims == e.dims, format!("dims {dims:?} != {:?}", e.dims));
    want(raw.white_level == e.white, format!("white {} != {}", raw.white_level, e.white));
    want(raw.cfa_pattern == e.cfa, format!("cfa {:?} != {:?}", raw.cfa_pattern, e.cfa));
    want(raw.unique_camera_model.as_deref() == Some(e.model), format!("model {:?}", raw.unique_camera_model));
    want(raw.solve_colour().err() == Some(DevelopError::MissingColourData), "colour not refused".into());
    want(hs < vs.max(1) * 3 && vs < hs.max(1) * 3, format!("hstep {hs} vstep {vs}"));
    match r {
        Some((r, _, _)) => want(r >= e.r_floor, format!("preview correlation {r} < {}", e.r_floor)),
        None => println!("NOT_PROVEN: {test} {} preview_oracle reason=no_decodable_embedded_jpeg", e.file),
    }
    want(digest == e.digest, "pinned CFA digest differs".into());
    let again = decode_container(&bytes).map(|r| samples_digest(&r)).ok();
    want(again.as_deref() == Some(digest.as_str()), "decode is not deterministic".into());
    bad
}

fn check_all(test: &str, files: &[Expect]) {
    let bad: Vec<String> = files.iter().flat_map(|e| check(test, e)).collect();
    assert!(bad.is_empty(), "{bad:#?}");
}

/// D70s: raw SubIFD 3040 x 2014 12-bit Compression 34713, maker note 0x0096 version 0x44 0x10
/// (lossy type 1); Z6: 6064 x 4040 12-bit 34713. Facts from byte inspection 2026-10-15.
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn nef_huffman_d70s_and_z6_decode_and_match_the_embedded_preview() {
    const TEST: &str = "nef_huffman_d70s_and_z6_decode_and_match_the_embedded_preview";
    check_all(TEST, &[
        Expect {
            file: "nikon-d70s-12bit-lossy-type1.nef",
            sha256: "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692",
            dims: (3040, 2014, 12),
            white: 0,
            cfa: CfaPattern::Bayer(Bayer::Bggr),
            model: "NIKON D70s",
            digest: "",
            r_floor: 0.0,
        },
        Expect {
            file: "nikon-z6-12bit-compressed.nef",
            sha256: "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36",
            dims: (6064, 4040, 12),
            white: 0,
            cfa: CfaPattern::Bayer(Bayer::Rggb),
            model: "NIKON Z 6",
            digest: "",
            r_floor: 0.0,
        },
    ]);
}
