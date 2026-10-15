//! Camera colour on the real raw-vendor corpus (lane SMB-DEVELOP-CAMERA-COLOUR). `#[ignore]`d (the
//! raw.pixls.us CC0 corpus is never committed): run with
//! `cargo nextest run -p hsk-studio-develop --test camera_colour --run-ignored ignored-only --no-capture`.
//!
//! Oracle: the camera's own embedded JPEG preview (largest JPEG in the file, decoded by zune-jpeg),
//! rendered by the camera with its as-shot white balance. The raw is reduced to block means of the
//! camera-native signal (`DngRaw::site`), the preview to block means of linear sRGB, and the
//! preview window is located on the raw by luma correlation (scale and offset search). On preview
//! blocks that are neutral (CIELAB D65 chroma C* < 4, 25 < L* < 90) and unclipped in the raw:
//! (1) the as-shot neutral read from the file makes them neutral in camera space (white-balanced
//!     red/green and blue/green within `wb_tol`), whatever the matrix;
//! (2) with a colour route, `camera_to_xyz_d50` of `solve_colour` renders them neutral: CIELAB (D50)
//!     chroma at the preview block's lightness, i.e. CIE76 distance to the neutral of that
//!     lightness, median <= `neutral_de` (the stated bound);
//! (3) with a colour route, chromatic preview blocks (C* > 20) keep the preview's hue: median
//!     absolute hue difference <= `hue_deg` (the camera's JPEG look adds saturation, not hue).
//! An absent corpus root, file or hash prints `NOT_PROVEN:` and proves nothing.
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
    let text = std::fs::read_to_string(ws.join(".config").join("oracle-corpus.toml")).map_err(|e| format!("corpus_convention_absent error={e}"))?;
    let search = convention_value(&text, "search").ok_or("corpus_convention_malformed key=search")?;
    let max: usize = convention_value(&text, "max-ancestors").and_then(|v| v.parse().ok()).ok_or("corpus_convention_malformed key=max-ancestors")?;
    ws.ancestors()
        .take(max + 1)
        .map(|a| a.join(&search))
        .find(|c| c.is_dir())
        .map(|root| root.join("raw-vendor"))
        .ok_or_else(|| format!("corpus_root_absent search={search}"))
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
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if got != sha256 {
        println!("NOT_PROVEN: {test} {file} reason=sha256_mismatch got={got}");
        return None;
    }
    Some(bytes)
}

/// Largest embedded JPEG as interleaved RGB8 (width, height, pixels).
fn preview_rgb(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
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
    if c != 3 || px.len() < w * h * 3 {
        return None;
    }
    Some((w, h, px[..w * h * 3].to_vec()))
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

const SRGB_TO_XYZ_D65: [f64; 9] = [0.4124564, 0.3575761, 0.1804375, 0.2126729, 0.7151522, 0.0721750, 0.0193339, 0.1191920, 0.9503041];
const D65: [f64; 3] = [0.95047, 1.0, 1.08883];

fn mul(m: &[f64; 9], v: &[f64; 3]) -> [f64; 3] {
    [m[0] * v[0] + m[1] * v[1] + m[2] * v[2], m[3] * v[0] + m[4] * v[1] + m[5] * v[2], m[6] * v[0] + m[7] * v[1] + m[8] * v[2]]
}

fn lab(xyz: &[f64; 3], white: &[f64; 3]) -> [f64; 3] {
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(xyz[0] / white[0]), f(xyz[1] / white[1]), f(xyz[2] / white[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
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

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Camera-native block means: per block of `b` x `b` sites (b a multiple of the CFA period), the
/// mean signal of each channel and whether any site is near clipping.
struct Blocks {
    w: usize,
    h: usize,
    b: usize,
    rgb: Vec<[f64; 3]>,
    clipped: Vec<bool>,
}

fn blocks(raw: &DngRaw, b: usize) -> Blocks {
    let (w, h) = (raw.width as usize / b, raw.height as usize / b);
    let mut rgb = vec![[0.0; 3]; w * h];
    let mut clipped = vec![false; w * h];
    for by in 0..h {
        for bx in 0..w {
            let (mut s, mut n) = ([0.0f64; 3], [0.0f64; 3]);
            for y in by * b..(by + 1) * b {
                for x in bx * b..(bx + 1) * b {
                    let (c, v) = raw.site(x as u32, y as u32);
                    s[c as usize] += v;
                    n[c as usize] += 1.0;
                    if v > 0.97 {
                        clipped[by * w + bx] = true;
                    }
                }
            }
            rgb[by * w + bx] = [s[0] / n[0].max(1.0), s[1] / n[1].max(1.0), s[2] / n[2].max(1.0)];
        }
    }
    Blocks { w, h, b, rgb, clipped }
}

/// Mean of `blocks` over raw-block rectangle [x0, x1) x [y0, y1) (in blocks), and any clipping.
fn region(bl: &Blocks, x0: usize, y0: usize, x1: usize, y1: usize) -> ([f64; 3], bool) {
    let (mut s, mut n, mut clip) = ([0.0f64; 3], 0.0f64, false);
    for y in y0..y1.max(y0 + 1).min(bl.h) {
        for x in x0..x1.max(x0 + 1).min(bl.w) {
            let v = bl.rgb[y * bl.w + x];
            for k in 0..3 {
                s[k] += v[k];
            }
            n += 1.0;
            clip |= bl.clipped[y * bl.w + x];
        }
    }
    ([s[0] / n.max(1.0), s[1] / n.max(1.0), s[2] / n.max(1.0)], clip)
}

/// One grid cell: preview linear sRGB mean, raw camera-native mean, raw clipping.
struct Cell {
    preview: [f64; 3],
    camera: [f64; 3],
    clipped: bool,
}

/// Locate the preview on the raw (scale k raw pixels per preview pixel, offset) by Pearson
/// correlation of luma grids, then return the 48-wide grid of cells and the best r.
fn cells(raw: &DngRaw, bl: &Blocks, pw: usize, ph: usize, px: &[u8]) -> Option<(Vec<Cell>, f64)> {
    let gx = 48usize;
    let gy = (gx * ph / pw).max(8);
    let mut prev = Vec::with_capacity(gx * gy);
    for cy in 0..gy {
        for cx in 0..gx {
            let (x0, x1, y0, y1) = (cx * pw / gx, (cx + 1) * pw / gx, cy * ph / gy, (cy + 1) * ph / gy);
            let mut s = [0.0; 3];
            let mut n = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = &px[(y * pw + x) * 3..(y * pw + x) * 3 + 3];
                    for k in 0..3 {
                        s[k] += srgb_to_linear(f64::from(p[k]) / 255.0);
                    }
                    n += 1.0;
                }
            }
            prev.push([s[0] / n, s[1] / n, s[2] / n]);
        }
    }
    let pl: Vec<f64> = prev.iter().map(|p| (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).sqrt()).collect();
    let (rw, rh) = (raw.width as f64, raw.height as f64);
    let fit = (rw / pw as f64).min(rh / ph as f64);
    let mut best = (f64::MIN, 0.0, 0.0, 0.0);
    for k in [fit, fit * 0.99, fit * 0.98, fit * 0.97, fit * 0.96, fit * 0.95, 1.0, 1.5, 2.0] {
        let (cw, ch) = (pw as f64 * k, ph as f64 * k);
        if cw > rw || ch > rh || cw < (gx * bl.b) as f64 {
            continue;
        }
        let (rx, ry) = (rw - cw, rh - ch);
        let steps = 24;
        let score = |ox: f64, oy: f64| {
            let g: Vec<f64> = (0..gx * gy)
                .map(|i| {
                    let (cx, cy) = (i % gx, i / gx);
                    let bx0 = ((ox + cx as f64 * cw / gx as f64) / bl.b as f64) as usize;
                    let by0 = ((oy + cy as f64 * ch / gy as f64) / bl.b as f64) as usize;
                    let bx1 = ((ox + (cx + 1) as f64 * cw / gx as f64) / bl.b as f64) as usize;
                    let by1 = ((oy + (cy + 1) as f64 * ch / gy as f64) / bl.b as f64) as usize;
                    region(bl, bx0, by0, bx1, by1).0[1].max(0.0).sqrt()
                })
                .collect();
            pearson(&g, &pl)
        };
        for iy in 0..=steps {
            for ix in 0..=steps {
                let (ox, oy) = (rx * ix as f64 / steps as f64, ry * iy as f64 / steps as f64);
                let r = score(ox, oy);
                if r > best.0 {
                    best = (r, k, ox, oy);
                }
            }
        }
        // Local refinement around the best offset of this scale.
        if best.1 == k {
            let (sx, sy) = (rx / steps as f64, ry / steps as f64);
            for iy in -4i32..=4 {
                for ix in -4i32..=4 {
                    let (ox, oy) = ((best.2 + sx * f64::from(ix) / 4.0).clamp(0.0, rx), (best.3 + sy * f64::from(iy) / 4.0).clamp(0.0, ry));
                    let r = score(ox, oy);
                    if r > best.0 {
                        best = (r, k, ox, oy);
                    }
                }
            }
        }
    }
    if best.0 == f64::MIN {
        return None;
    }
    let (r, k, ox, oy) = best;
    let (cw, ch) = (pw as f64 * k, ph as f64 * k);
    let out = (0..gx * gy)
        .map(|i| {
            let (cx, cy) = (i % gx, i / gx);
            let b = bl.b as f64;
            // Inner half of each cell, away from edges the alignment may miss by a block.
            let x0 = ((ox + (cx as f64 + 0.25) * cw / gx as f64) / b) as usize;
            let y0 = ((oy + (cy as f64 + 0.25) * ch / gy as f64) / b) as usize;
            let x1 = ((ox + (cx as f64 + 0.75) * cw / gx as f64) / b) as usize;
            let y1 = ((oy + (cy as f64 + 0.75) * ch / gy as f64) / b) as usize;
            let (camera, clipped) = region(bl, x0, y0, x1, y1);
            Cell { preview: prev[i], camera, clipped }
        })
        .collect();
    Some((out, r))
}

struct Expect {
    file: &'static str,
    sha256: &'static str,
    route: &'static str,
    /// Tolerance on white-balanced red/green and blue/green of preview-neutral blocks (median).
    wb_tol: f64,
    /// Stated bound: median CIE76 to the neutral of the same lightness.
    neutral_de: f64,
    hue_deg: f64,
    /// Why this file's preview cannot prove colour (measurements are printed, nothing asserted).
    weak_oracle: Option<&'static str>,
}

const FILES: &[Expect] = &[
    Expect { file: "canon-eos-r6.cr3", sha256: "74abb0a113d075ad9887a058082f40dd2a938c4813a08474d82356f11a027778", route: "spectral_table", wb_tol: 0.05, neutral_de: 3.0, hue_deg: 12.0, weak_oracle: None },
    Expect { file: "olympus-c5060wz-12bit.orf", sha256: "3fc12bc3f6320cfab456111d7805b446e329fdd1907680be55e2e163e5e084bd", route: "file_matrix", wb_tol: 0.05, neutral_de: 3.0, hue_deg: 12.0, weak_oracle: Some("preview_is_a_160x120_thumbnail") },
    Expect { file: "nikon-z6-12bit-compressed.nef", sha256: "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36", route: "none", wb_tol: 0.05, neutral_de: 0.0, hue_deg: 0.0, weak_oracle: None },
    Expect { file: "fujifilm-x-s10-14bit-compressed-xtrans.raf", sha256: "9419d1408ebf850395e5dd563beb96309466546252caf541466034c9cb7e9724", route: "none", wb_tol: 0.05, neutral_de: 0.0, hue_deg: 0.0, weak_oracle: None },
    Expect { file: "panasonic-dmc-lx7.rw2", sha256: "18dfc3877e944020299635a8c2fad61fca2485b1c9b67645a495ee870a86e599", route: "none", wb_tol: 0.05, neutral_de: 0.0, hue_deg: 0.0, weak_oracle: None },
];

fn check(test: &str, e: &Expect) -> Vec<String> {
    let mut bad = Vec::new();
    let Some(bytes) = read(test, e.file, e.sha256) else { return bad };
    let mut raw = decode_container(&bytes).unwrap_or_else(|err| panic!("{}: {err}", e.file));
    // A single-illuminant route is refused by default (SMB-DEVELOP-CAMERA-COLOUR-2); this oracle
    // measures it opted in.
    if raw.colour_route.single_illuminant() {
        if raw.solve_colour().err() != Some(DevelopError::ApproximateColourNotAllowed) {
            bad.push(format!("{}: single-illuminant route not refused by default", e.file));
        }
        raw.allow_approximate_colour();
    }
    let route = raw.colour_route.key();
    println!("COLOUR {} route={:?} neutral={:?}", e.file, raw.colour_route, raw.color.as_shot_neutral);
    if route != e.route {
        bad.push(format!("{}: route {route} != {}", e.file, e.route));
    }
    let Some(neutral) = raw.color.as_shot_neutral else {
        bad.push(format!("{}: no as-shot neutral read", e.file));
        return bad;
    };
    let solution = raw.solve_colour();
    if (e.route == "none") != solution.is_err() {
        bad.push(format!("{}: solve {:?}", e.file, solution.as_ref().map(|s| s.cct)));
    }
    // Everything after this point measures against the preview (the oracle).
    let oracle_start = bad.len();
    let Some((pw, ph, px)) = preview_rgb(&bytes) else {
        println!("NOT_PROVEN: {test} {} reason=no_decodable_embedded_jpeg", e.file);
        return bad;
    };
    let period = if raw.cfa_pattern == CfaPattern::XTrans { 6 } else { 2 };
    let bl = blocks(&raw, period * 6);
    let Some((cells, r)) = cells(&raw, &bl, pw, ph, &px) else {
        println!("NOT_PROVEN: {test} {} reason=preview_not_located", e.file);
        return bad;
    };
    let pl = |c: &Cell| lab(&mul(&SRGB_TO_XYZ_D65, &c.preview), &D65);
    let neutral_cells: Vec<&Cell> = cells
        .iter()
        .filter(|c| {
            let l = pl(c);
            !c.clipped && l[0] > 25.0 && l[0] < 90.0 && l[1].hypot(l[2]) < 4.0 && c.camera.iter().all(|v| *v > 0.002)
        })
        .collect();
    println!("COLOUR {} preview {pw}x{ph} r={r:.3} neutral_cells={}", e.file, neutral_cells.len());
    if r < 0.9 || neutral_cells.len() < 8 {
        println!("NOT_PROVEN: {test} {} reason=too_few_neutral_preview_cells r={r:.3} n={}", e.file, neutral_cells.len());
        return bad;
    }
    // (1) White balance alone: white-balanced camera RGB of neutral cells is neutral.
    let wb = |c: &Cell| [c.camera[0] / neutral[0], c.camera[1] / neutral[1], c.camera[2] / neutral[2]];
    let rg = median(neutral_cells.iter().map(|c| { let v = wb(c); v[0] / v[1] }).collect());
    let bg = median(neutral_cells.iter().map(|c| { let v = wb(c); v[2] / v[1] }).collect());
    println!("COLOUR {} white-balanced neutral cells: median r/g {rg:.4} b/g {bg:.4}", e.file);
    if (rg - 1.0).abs() > e.wb_tol || (bg - 1.0).abs() > e.wb_tol {
        bad.push(format!("{}: white-balanced neutral r/g {rg:.4} b/g {bg:.4} outside 1 +- {}", e.file, e.wb_tol));
    }
    let Ok(s) = solution else { return bad };
    // (2) Neutral cells develop neutral: CIELAB D50 chroma at the preview cell's lightness.
    let raw_lab = |c: &Cell| {
        let xyz = mul(&s.camera_to_xyz_d50, &c.camera);
        let y_prev = mul(&SRGB_TO_XYZ_D65, &c.preview)[1];
        let k = y_prev / xyz[1].max(1e-9);
        lab(&[xyz[0] * k, xyz[1] * k, xyz[2] * k], &[0.96422, 1.0, 0.82521])
    };
    let chroma: Vec<f64> = neutral_cells.iter().map(|c| { let l = raw_lab(c); l[1].hypot(l[2]) }).collect();
    let (med, p90) = (median(chroma.clone()), { let mut v = chroma.clone(); v.sort_by(f64::total_cmp); v[v.len() * 9 / 10] });
    println!("COLOUR {} neutral cells: CIE76 to neutral median {med:.2} p90 {p90:.2} (cct {:.0} K, illuminant weight {:.3}, white {:?})", e.file, s.cct, s.weight, s.white_xy);
    // STU-COL-218: a two-calibration route interpolates (the R6's as-shot light lies between A and D65).
    if e.route == "spectral_table" && !(s.weight > 0.0 && s.weight < 1.0) {
        bad.push(format!("{}: illuminant weight {} not strictly between the two calibrations", e.file, s.weight));
    }
    if med > e.neutral_de {
        bad.push(format!("{}: neutral median dE76 {med:.2} > {}", e.file, e.neutral_de));
    }
    // (3) Chromatic cells keep the preview's hue.
    let hues: Vec<f64> = cells
        .iter()
        .filter(|c| { let l = pl(c); !c.clipped && l[0] > 20.0 && l[0] < 90.0 && l[1].hypot(l[2]) > 20.0 })
        .map(|c| {
            let (a, b) = (pl(c), raw_lab(c));
            let d = (b[2].atan2(b[1]) - a[2].atan2(a[1])).to_degrees();
            (d + 540.0).rem_euclid(360.0) - 180.0
        })
        .map(f64::abs)
        .collect();
    if hues.len() >= 8 {
        let mh = median(hues.clone());
        println!("COLOUR {} chromatic cells {}: median |hue difference| {mh:.1} deg", e.file, hues.len());
        if mh > e.hue_deg {
            bad.push(format!("{}: median hue difference {mh:.1} > {}", e.file, e.hue_deg));
        }
    } else {
        println!("NOT_PROVEN: {test} {} hue reason=too_few_chromatic_cells n={}", e.file, hues.len());
    }
    if let Some(why) = e.weak_oracle {
        // Measured and printed; a thumbnail-sized preview is not evidence either way.
        for m in bad.drain(oracle_start..) {
            println!("NOT_PROVEN: {test} {} reason={why} measured: {m}", e.file);
        }
        println!("NOT_PROVEN: {test} {} colour_route reason={why}", e.file);
    }
    bad
}

#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn as_shot_white_balance_and_colour_routes_render_preview_neutrals_neutral() {
    let bad: Vec<String> = FILES.iter().flat_map(|e| check("camera_colour", e)).collect();
    assert!(bad.is_empty(), "{bad:#?}");
}
