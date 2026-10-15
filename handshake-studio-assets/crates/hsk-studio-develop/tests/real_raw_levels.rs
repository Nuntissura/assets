//! Black and white levels of the vendor decoders against sample statistics of the CC0 corpus
//! (raw.pixls.us, test DATA only, never committed). `#[ignore]`d: run with
//! `cargo nextest run -p hsk-studio-develop --test real_raw_levels --run-ignored ignored-only --no-capture`.
//!
//! Per file the test measures, independently of the decoder's level source where the file allows:
//! the per-site mean over optically masked regions (CR3 IAD1 masked-left and masked-top areas; for
//! CR2 the masked columns are the decoder's own source, so they pin it), and the per-site 0.1 %
//! dark quantile of the image region. Whites are pinned; a white counts as proven only when the
//! sample saturates (>= 16 samples at or above it), otherwise a NOT_PROVEN line is printed and the
//! decoder keeps the white named in `unapplied`. Corpus root and sha256 pins as `tests/real_raw.rs`.
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

/// A frame rectangle: columns `.0..2`, rows `.1..3`.
type Rect = (u32, u32, u32, u32);

/// Per 2x2 site (frame origin, row-major) over `r`: mean and 0.1 % quantile.
fn site_stats(raw: &DngRaw, r: Rect) -> ([f64; 4], [u16; 4]) {
    let mut hist = vec![vec![0u64; 65_536]; 4];
    let (mut sum, mut n) = ([0f64; 4], [0f64; 4]);
    for y in r.1..r.3 {
        for x in r.0..r.2 {
            let s = ((y % 2) * 2 + x % 2) as usize;
            let v = raw.sample(x, y);
            hist[s][v as usize] += 1;
            sum[s] += f64::from(v);
            n[s] += 1.0;
        }
    }
    let quantile = |h: &Vec<u64>| {
        let total: u64 = h.iter().sum();
        let mut acc = 0;
        h.iter().position(|c| {
            acc += c;
            acc * 1000 >= total
        })
        .unwrap_or(0) as u16
    };
    ([0, 1, 2, 3].map(|i| sum[i] / n[i].max(1.0)), [0, 1, 2, 3].map(|i| quantile(&hist[i])))
}

/// Applied black at 2x2 site `s` (frame origin).
fn black_at(raw: &DngRaw, s: usize) -> f64 {
    let (rr, rc) = raw.black_repeat;
    let (y, x) = ((s / 2) as u32 % rr, (s % 2) as u32 % rc);
    raw.black_level[(y * rc + x) as usize]
}

/// Samples of `r` at or above `level`.
fn at_or_above(raw: &DngRaw, r: Rect, level: u32) -> u64 {
    let mut n = 0;
    for y in r.1..r.3 {
        for x in r.0..r.2 {
            n += u64::from(u32::from(raw.sample(x, y)) >= level);
        }
    }
    n
}

struct Case {
    file: &'static str,
    sha256: &'static str,
    /// Applied black per 2x2 site (frame origin), within 0.01.
    black: [f64; 4],
    /// Where the black comes from (reported).
    source: &'static str,
    /// Optically masked regions whose per-site means must agree with the black within 1.5 codes.
    masked: &'static [Rect],
    /// Image region for the dark-floor band and the white evidence.
    image: Rect,
    white: u32,
    /// The samples prove the white (>= 16 at or above it); otherwise the white stays unread.
    white_proven: bool,
    unapplied: &'static [&'static str],
}

const CASES: [Case; 9] = [
    Case {
        file: "canon-eos-350d-raw.cr2",
        sha256: "4b47ba27c0090efa091aaf99babc76d9a3e6f6f121b443d6c58c3ce7275cd770",
        black: [264.069, 273.483, 273.953, 263.086],
        source: "mean of the masked columns 2..50 left of the SensorInfo image area (52, 19)-(3507, 2322)",
        masked: &[(2, 19, 50, 2323)],
        image: (52, 19, 3508, 2323),
        white: 4095,
        white_proven: false,
        unapplied: &["cr2_white_level_unread", "cr2_sensor_borders_unread", "cr2_cfa_pattern_unverified"],
    },
    Case {
        file: "canon-eos-r6.cr3",
        sha256: "74abb0a113d075ad9887a058082f40dd2a938c4813a08474d82356f11a027778",
        black: [512.0; 4],
        source: "CTMD maker note ColorData v33 (black 343..346, white 811)",
        masked: &[(0, 96, 144, 2384), (144, 0, 3576, 96)],
        image: (144, 96, 3576, 2384),
        white: 14008,
        white_proven: false,
        unapplied: &["cr3_active_area_unread"],
    },
    Case {
        file: "nikon-d1h-12bit-uncompressed.nef",
        sha256: "d110b80a8b10a3a67b5cd1c15ef33f1a6397557b73d5d3f4e565739f0c072571",
        black: [0.0; 4],
        source: "no maker-note level (type-1 note): 0",
        masked: &[],
        image: (0, 0, 2012, 1324),
        white: 3953,
        white_proven: true,
        unapplied: &["nef_makernote_curve_unread"],
    },
    Case {
        file: "nikon-d70s-12bit-lossy-type1.nef",
        sha256: "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692",
        black: [0.0; 4],
        source: "no maker-note 0x003D: 0",
        masked: &[],
        image: (0, 0, 3040, 2014),
        white: 4080,
        white_proven: true,
        unapplied: &[],
    },
    Case {
        file: "nikon-z6-12bit-compressed.nef",
        sha256: "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36",
        black: [252.0; 4],
        source: "maker note 0x003D = 1008 (14-bit units) / 4",
        masked: &[],
        image: (0, 0, 4548, 3030),
        white: 4095,
        white_proven: true,
        unapplied: &[],
    },
    Case {
        file: "sony-ilce-7s-14bit-compressed.arw",
        sha256: "a35ebb2fbec929daa5beb20d1ce5c15a8aac7b1a7a231455387f3df8a7442e07",
        black: [512.0; 4],
        source: "encrypted SR2SubIFD 0x7310 (key 11 22 33 44)",
        masked: &[],
        image: (0, 0, 2816, 1872),
        white: 17204,
        white_proven: false,
        unapplied: &["arw_white_level_unread"],
    },
    Case {
        file: "fujifilm-x-s10-14bit-compressed-xtrans.raf",
        sha256: "9419d1408ebf850395e5dd563beb96309466546252caf541466034c9cb7e9724",
        black: [1022.0; 4],
        source: "raw IFD 0xF00A (36 values, all 1022)",
        masked: &[],
        image: (6, 13, 6246, 4173),
        white: 16383,
        white_proven: false,
        unapplied: &["raf_white_level_unread", "raf_crop_unread"],
    },
    Case {
        file: "olympus-c5060wz-12bit.orf",
        sha256: "3fc12bc3f6320cfab456111d7805b446e329fdd1907680be55e2e163e5e084bd",
        black: [0.0; 4],
        source: "maker note 0x1012 = 0 (ValidBits 12)",
        masked: &[],
        image: (0, 0, 2608, 1949),
        white: 4080,
        white_proven: true,
        unapplied: &["orf_active_area_unread"],
    },
    Case {
        file: "panasonic-dmc-lx7.rw2",
        sha256: "18dfc3877e944020299635a8c2fad61fca2485b1c9b67645a495ee870a86e599",
        black: [142.0; 4],
        source: "IFD0 0x001C-0x001E = 127 + 15 (format 4 codec offset)",
        masked: &[],
        image: (8, 8, 1896, 1268),
        white: 4095,
        white_proven: false,
        unapplied: &["rw2_white_level_unread", "rw2_borders_unread", "rw2_defect_pixels_unfilled"],
    },
];

/// Black: pinned per site; masked-region means within 1.5 codes; the image's per-site 0.1 % dark
/// quantile in [black - 8, black + 64] (the black is not above the darkest content and not far
/// below it). White: pinned; when proven, >= 16 image samples at or above it.
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn vendor_levels_agree_with_sample_statistics() {
    const TEST: &str = "vendor_levels_agree_with_sample_statistics";
    let mut bad = Vec::new();
    for c in &CASES {
        let Some(bytes) = read(TEST, c.file, c.sha256) else { continue };
        let raw = decode_container(&bytes).unwrap_or_else(|e| panic!("{}: {e}", c.file));
        let (_, floor) = site_stats(&raw, c.image);
        let top = at_or_above(&raw, c.image, raw.white_level);
        let mut masked_means = Vec::new();
        for s in 0..4 {
            let b = black_at(&raw, s);
            if (b - c.black[s]).abs() > 0.01 {
                bad.push(format!("{}: site {s} black {b} != {}", c.file, c.black[s]));
            }
            let q = f64::from(floor[s]);
            if q < b - 8.0 || q > b + 64.0 {
                bad.push(format!("{}: site {s} dark quantile {q} outside [{}, {}]", c.file, b - 8.0, b + 64.0));
            }
        }
        for &m in c.masked {
            let (mean, _) = site_stats(&raw, m);
            masked_means.push(mean);
            for s in 0..4 {
                if (mean[s] - black_at(&raw, s)).abs() > 1.5 {
                    bad.push(format!("{}: masked {m:?} site {s} mean {} vs black {}", c.file, mean[s], black_at(&raw, s)));
                }
            }
        }
        if raw.white_level != c.white {
            bad.push(format!("{}: white {} != {}", c.file, raw.white_level, c.white));
        }
        if c.white_proven && top < 16 {
            bad.push(format!("{}: white {} not reached ({top} samples)", c.file, raw.white_level));
        }
        if raw.unapplied != c.unapplied {
            bad.push(format!("{}: unapplied {:?} != {:?}", c.file, raw.unapplied, c.unapplied));
        }
        println!(
            "LEVELS {}: black {:?} (source: {}) white {} (at or above: {top}) dark_q0.1% {floor:?} masked_means {masked_means:?} unapplied {:?}",
            c.file, c.black, c.source, raw.white_level, raw.unapplied
        );
        if !c.white_proven {
            println!("NOT_PROVEN: {TEST} {} white_level reason=no_saturated_region_in_sample", c.file);
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
