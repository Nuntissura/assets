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

/// Correlation of the decoded CFA plane with the embedded preview. The raw (minus its floor) is
/// reduced to 8x8 block means and square-rooted (rough display gamma); the preview covers a window
/// of `k` raw pixels per preview pixel (`k` = the raw/preview size ratio rounded down to a half,
/// e.g. 1.0 when the preview is the camera's crop, 1.5 for a reduced preview); that window is slid
/// over the raw in 8-pixel steps (masked borders and crop offsets differ per body) and the best
/// Pearson r on a 48-cell-wide grid is kept. A portrait preview of a landscape frame is compared
/// against both quarter turns. Returns (r, preview width, preview height).
fn preview_r(raw: &DngRaw, bytes: &[u8]) -> Option<(f64, usize, usize)> {
    const B: usize = 8;
    let (pw, ph, luma) = preview_luma(bytes)?;
    let (w, h) = (raw.width as usize, raw.height as usize);
    let mut floor = u16::MAX;
    for y in (0..raw.height).step_by(13) {
        for x in (0..raw.width).step_by(11) {
            floor = floor.min(raw.sample(x, y));
        }
    }
    let (sw, sh) = (w / B, h / B);
    let mut small = vec![0.0f64; sw * sh];
    for by in 0..sh {
        for bx in 0..sw {
            let mut s = 0.0;
            for y in by * B..(by + 1) * B {
                for x in bx * B..(bx + 1) * B {
                    s += f64::from(raw.sample(x as u32, y as u32).saturating_sub(floor));
                }
            }
            small[by * sw + bx] = (s / (B * B) as f64).sqrt();
        }
    }
    let rotated = (pw > ph) != (w > h);
    // Orient the reduced raw like the preview (both quarter turns when rotated).
    let turns: Vec<(usize, usize, Vec<f64>)> = if !rotated {
        vec![(sw, sh, small)]
    } else {
        let cw: Vec<f64> = (0..sw * sh).map(|i| { let (x, y) = (i % sh, i / sh); small[(sh - 1 - x) * sw + y] }).collect();
        let ccw: Vec<f64> = (0..sw * sh).map(|i| { let (x, y) = (i % sh, i / sh); small[x * sw + (sw - 1 - y)] }).collect();
        vec![(sh, sw, cw), (sh, sw, ccw)]
    };
    let gx = 48;
    let gy = (gx * ph / pw).max(8);
    let p = grid(|x, y| luma[y * pw + x], pw, ph, gx, gy);
    let mut best = f64::MIN;
    for (tw, th, img) in &turns {
        let ratio = (*tw * B) as f64 / pw as f64;
        let ratio = ratio.min((*th * B) as f64 / ph as f64);
        let k = ((ratio * 2.0).floor() / 2.0).max(0.5).min(ratio);
        let (cw, ch) = (((pw as f64 * k) / B as f64) as usize, ((ph as f64 * k) / B as f64) as usize);
        if cw < gx || ch < gy || cw > *tw || ch > *th {
            continue;
        }
        for oy in 0..=(th - ch) {
            for ox in 0..=(tw - cw) {
                let g = grid(|x, y| img[(oy + y) * tw + ox + x], cw, ch, gx, gy);
                best = best.max(pearson(&g, &p));
            }
        }
    }
    (best > f64::MIN).then_some((best, pw, ph))
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

/// D70s (lossy type 1): raw SubIFD 3040 x 2014, 12 bit, Compression 34713, BGGR, curve maximum
/// 4095. Z6 (12-bit compressed): 4548 x 3030, 12 bit, RGGB, as in the raw.pixls.us Exif dump
/// (https://raw.pixls.us/getfile.php/3587/exif/: SubImage2 4548 x 3030, "Nikon NEF Compressed").
/// Preview r measured at 1dcc906: D70s 0.938 (3008 x 2000 preview), Z6 0.885 (3024 x 2016); floors
/// 0.90 / 0.85. Digests pinned from the same run.
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn nef_huffman_d70s_and_z6_decode_and_match_the_embedded_preview() {
    const TEST: &str = "nef_huffman_d70s_and_z6_decode_and_match_the_embedded_preview";
    check_all(TEST, &[
        Expect {
            file: "nikon-d70s-12bit-lossy-type1.nef",
            sha256: "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692",
            dims: (3040, 2014, 12),
            white: 4095,
            cfa: CfaPattern::Bayer(Bayer::Bggr),
            model: "NIKON D70s",
            digest: "291da6d41094b5da303e2cb7d704e464daa0edf03245fb14a9d8e592c92b4e11",
            r_floor: 0.90,
        },
        Expect {
            file: "nikon-z6-12bit-compressed.nef",
            sha256: "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36",
            dims: (4548, 3030, 12),
            white: 4095,
            cfa: CfaPattern::Bayer(Bayer::Rggb),
            model: "NIKON Z 6",
            digest: "af18a3a222d77b3e2f19ff416cba8d5c62488a2d1d6bf149db1f84b5c93ebe22",
            r_floor: 0.85,
        },
    ]);
}

/// EOS R6 (raw.pixls.us id 4659): main CRAW track CMP1 version 0x200, 3584 x 2386 stored sensor
/// (displayed 3408 x 2272), 14 bit, 4 planes, CFA code 0 (RGGB), 3 wavelet levels, one tile,
/// header 872 bytes (byte inspection 2026-10-15).
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn cr3_crx_r6_decodes_and_matches_the_embedded_preview() {
    const TEST: &str = "cr3_crx_r6_decodes_and_matches_the_embedded_preview";
    check_all(TEST, &[Expect {
        file: "canon-eos-r6.cr3",
        sha256: "74abb0a113d075ad9887a058082f40dd2a938c4813a08474d82356f11a027778",
        dims: (3584, 2386, 14),
        white: 16383,
        cfa: CfaPattern::Bayer(Bayer::Rggb),
        model: "Canon EOS R6",
        digest: "",
        r_floor: 0.0,
    }]);
}
