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
/// reduced to 8x8 block means and square-rooted (rough display gamma). The preview covers a window
/// of `k` raw pixels per preview pixel, with `k` tried from {1, 1.5, 2, width ratio, height ratio,
/// 0.98 and 0.96 of the smaller ratio}
/// (a 1:1 camera crop inside masked borders, or a reduced preview); each window is slid over the
/// raw (coarse search, then a one-block refinement around the best) and the best Pearson r of the
/// 48-cell-wide grid means is kept. Grid means come from a summed-area table. A portrait preview of
/// a landscape frame is compared against both quarter turns. Returns (r, preview w, preview h).
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
        let (tw, th) = (*tw, *th);
        // Summed-area table: sat[(y)(tw+1) + x] = sum of img over [0, x) x [0, y).
        let mut sat = vec![0.0f64; (tw + 1) * (th + 1)];
        for y in 0..th {
            let mut row = 0.0;
            for x in 0..tw {
                row += img[y * tw + x];
                sat[(y + 1) * (tw + 1) + x + 1] = sat[y * (tw + 1) + x + 1] + row;
            }
        }
        let rect = |x0: usize, y0: usize, x1: usize, y1: usize| {
            let n = ((x1 - x0) * (y1 - y0)).max(1) as f64;
            (sat[y1 * (tw + 1) + x1] - sat[y0 * (tw + 1) + x1] - sat[y1 * (tw + 1) + x0] + sat[y0 * (tw + 1) + x0]) / n
        };
        let score = |ox: usize, oy: usize, cw: usize, ch: usize| {
            let mut g = Vec::with_capacity(gx * gy);
            for cy in 0..gy {
                for cx in 0..gx {
                    let (x0, x1) = (ox + cx * cw / gx, ox + (cx + 1) * cw / gx);
                    let (y0, y1) = (oy + cy * ch / gy, oy + (cy + 1) * ch / gy);
                    g.push(rect(x0, y0, x1.max(x0 + 1), y1.max(y0 + 1)));
                }
            }
            pearson(&g, &p)
        };
        let ratios = [(tw * B) as f64 / pw as f64, (th * B) as f64 / ph as f64];
        let fit = ratios[0].min(ratios[1]);
        for k in [1.0, 1.5, 2.0, ratios[0], ratios[1], fit * 0.98, fit * 0.96] {
            let (cw, ch) = ((pw as f64 * k / B as f64).round() as usize, (ph as f64 * k / B as f64).round() as usize);
            if cw < gx || ch < gy || cw > tw || ch > th {
                continue;
            }
            let (rx, ry) = (tw - cw, th - ch);
            let (stx, sty) = ((rx / 16).max(1), (ry / 16).max(1));
            let mut local = (f64::MIN, 0, 0);
            for oy in (0..=ry).step_by(sty) {
                for ox in (0..=rx).step_by(stx) {
                    let r = score(ox, oy, cw, ch);
                    if r > local.0 {
                        local = (r, ox, oy);
                    }
                }
            }
            for oy in local.2.saturating_sub(sty)..=(local.2 + sty).min(ry) {
                for ox in local.1.saturating_sub(stx)..=(local.1 + stx).min(rx) {
                    best = best.max(score(ox, oy, cw, ch));
                }
            }
            best = best.max(local.0);
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
    // The pinned digest is the determinism and regression check (a second decode of the larger
    // files would double a debug-build test that already runs for tens of seconds).
    want(digest == e.digest, "pinned CFA digest differs".into());
    bad
}

fn check_all(test: &str, files: &[Expect]) {
    let bad: Vec<String> = files.iter().flat_map(|e| check(test, e)).collect();
    for e in files {
        println!("NOT_PROVEN: {test} {} cfa_samples_vs_independent_dng reason=no_approved_converter_run", e.file);
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// D70s (lossy type 1): raw SubIFD 3040 x 2014, 12 bit, Compression 34713, BGGR, curve maximum
/// 4095. Z6 (12-bit compressed): 4548 x 3030, 12 bit, RGGB, as in the raw.pixls.us Exif dump
/// (https://raw.pixls.us/getfile.php/3587/exif/: SubImage2 4548 x 3030, "Nikon NEF Compressed").
/// Preview r measured with the scale-searching oracle (slice 3 tree): D70s 0.997 (3008 x 2000
/// preview), Z6 0.986 (3024 x 2016); floors 0.97 / 0.95. Negative control: the D70s CFA against the
/// Z6 preview (another scene) measured 0.032, must stay below 0.5. Digests pinned at 1dcc906.
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
            r_floor: 0.97,
        },
        Expect {
            file: "nikon-z6-12bit-compressed.nef",
            sha256: "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36",
            dims: (4548, 3030, 12),
            white: 4095,
            cfa: CfaPattern::Bayer(Bayer::Rggb),
            model: "NIKON Z 6",
            digest: "af18a3a222d77b3e2f19ff416cba8d5c62488a2d1d6bf149db1f84b5c93ebe22",
            r_floor: 0.95,
        },
    ]);
    // Negative control: the oracle must not accept a frame of another scene.
    if let (Some(d70s), Some(z6)) = (
        read(TEST, "nikon-d70s-12bit-lossy-type1.nef", "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692"),
        read(TEST, "nikon-z6-12bit-compressed.nef", "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36"),
    ) {
        let (r, _, _) = preview_r(&decode_container(&d70s).unwrap(), &z6).unwrap();
        println!("control: d70s cfa vs z6 preview r={r}");
        assert!(r < 0.5, "the preview oracle accepted another scene: r={r}");
    }
}

/// EOS R6 (raw.pixls.us id 4659): main CRAW track CMP1 version 0x200, 3584 x 2386 stored sensor
/// (displayed 3408 x 2272), 14 bit, 4 planes, CFA code 0 (RGGB), 3 wavelet levels, one tile,
/// header 872 bytes (byte inspection 2026-10-15). Preview r measured: 0.987 (3408 x 2272 preview);
/// floor 0.96. Digest pinned at ec53c53.
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
        digest: "73d26ba5f75c2361d8eb197336335f95e1e41f8fffb2714fe3f7bb2b3e4b0c27",
        r_floor: 0.96,
    }]);
}

/// X-S10 (raw.pixls.us id 4190): header mode 3 (lossy compressed), raw IFD 6384 x 4182, 14 bit,
/// `IS` stream flag 0, sensor type 16 (X-Trans), 9 stripes, 697 six-row groups, X-Trans layout
/// record equal to develop's X-Trans grid (byte inspection 2026-10-15). The S5000 (SuperCCD, no
/// TIFF raw block) stays decoder_unavailable:raf (tests/real_raw.rs routing test). Preview r
/// measured on the slice 3 tree: 0.948 (4416 x 2944 reduced preview); floor 0.92; digest pinned
/// from the same run. Debug decode ~25 s.
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn raf_xtrans_lossy_x_s10_decodes_and_matches_the_embedded_preview() {
    const TEST: &str = "raf_xtrans_lossy_x_s10_decodes_and_matches_the_embedded_preview";
    check_all(TEST, &[Expect {
        file: "fujifilm-x-s10-14bit-compressed-xtrans.raf",
        sha256: "9419d1408ebf850395e5dd563beb96309466546252caf541466034c9cb7e9724",
        dims: (6384, 4182, 14),
        white: 16383,
        cfa: CfaPattern::XTrans,
        model: "X-S10",
        digest: "a7ccc3650bdead2da2dba81c3b56c98061d259d83444f1a8f872e9abe193fbf1",
        r_floor: 0.92,
    }]);
}

/// C5060WZ (raw.pixls.us id 1799): `IIRS` magic, IFD0 2608 x 1949, 12 bit, Compression 1, 122
/// strips summing to 2608 * 1949 * 1.5 bytes with one gap (two-field layout), Exif CFAPattern
/// (byte inspection 2026-10-15; the previous lane's SMB-DEVELOP-RAW-VENDOR blocker). The only
/// embedded JPEG is the 160 x 120 thumbnail: preview r measured 0.984, floor 0.95; digest pinned
/// from the same run.
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn orf_two_field_c5060wz_decodes_and_matches_the_embedded_preview() {
    const TEST: &str = "orf_two_field_c5060wz_decodes_and_matches_the_embedded_preview";
    check_all(TEST, &[Expect {
        file: "olympus-c5060wz-12bit.orf",
        sha256: "3fc12bc3f6320cfab456111d7805b446e329fdd1907680be55e2e163e5e084bd",
        dims: (2608, 1949, 12),
        white: 4095,
        cfa: CfaPattern::Bayer(Bayer::Rggb),
        model: "C5060WZ",
        digest: "1e6a220bfa5fbdced0bc9acf6c62cecc3cf287ad3df527e3fe980f154534ab37",
        r_floor: 0.95,
    }]);
}

/// DMC-LX7 (raw.pixls.us id 7010): `IIU` magic, sensor 1904 x 1276 (tags 0x0002/0x0003), 12 bit,
/// compression 34316 (byte inspection 2026-10-15; the previous lanes' RW2 blocker); CFA code 2
/// (GRBG). Preview r measured: 0.978 (1920 x 1280 preview), floor 0.95; digest pinned from the
/// same run.
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn rw2_lx7_decodes_and_matches_the_embedded_preview() {
    const TEST: &str = "rw2_lx7_decodes_and_matches_the_embedded_preview";
    check_all(TEST, &[Expect {
        file: "panasonic-dmc-lx7.rw2",
        sha256: "18dfc3877e944020299635a8c2fad61fca2485b1c9b67645a495ee870a86e599",
        dims: (1904, 1276, 12),
        white: 4095,
        cfa: CfaPattern::Bayer(Bayer::Grbg),
        model: "DMC-LX7",
        digest: "148fcc6e0589faf945c490b6abb210e8c8d9688035bdd8e69b1a732999b4f3f4",
        r_floor: 0.95,
    }]);
}
