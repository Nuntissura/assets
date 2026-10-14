//! Real proprietary RAW samples (raw.pixls.us, CC0-1.0, test DATA only, never committed or
//! executed) against the vendor container decoders. `#[ignore]`d: run with
//! `cargo nextest run -p hsk-studio-develop --test real_raw --run-ignored ignored-only --no-capture`.
//!
//! Corpus: `<oracle corpus root>/raw-vendor/` where the root comes from the workspace convention
//! `.config/oracle-corpus.toml` (start at the workspace root, walk up at most `max-ancestors`
//! parents, first ancestor holding `search`). Every file is pinned by sha256 (the values of the
//! corpus `provenance.json`). An absent root, file or hash prints `NOT_PROVEN:` and proves nothing.
//!
//! Facts per file: CR2 raw frame size and precision are the SOF3 header of the sample's raw IFD
//! stream (read once by byte inspection, 2026-10-14) and must hold the Exif output size published by
//! raw.pixls.us (https://raw.pixls.us/getfile.php/1230/exif/IMG_8728.CR2.exif.txt:
//! Exif.Photo.PixelXDimension 3456, PixelYDimension 2304, Exif.Image.Model "Canon EOS DIGITAL
//! REBEL XT"). The CFA-sample ORACLE (the same file converted to uncompressed DNG by an independent
//! tool and read by this crate's DNG reader) is NOT_PROVEN until such a conversion is approved/run.
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

/// `<corpus root>/raw-vendor` per `.config/oracle-corpus.toml`; `Err` carries the reason.
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

/// Bytes of `file`, sha256-verified; `None` after printing NOT_PROVEN.
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

#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn cr2_350d_decodes_the_stored_frame_deterministically() {
    const TEST: &str = "cr2_350d_decodes_the_stored_frame_deterministically";
    let Some(bytes) = read(TEST, "canon-eos-350d-raw.cr2", "4b47ba27c0090efa091aaf99babc76d9a3e6f6f121b443d6c58c3ce7275cd770") else {
        return;
    };
    let started = std::time::Instant::now();
    let raw = decode_container(&bytes).unwrap();
    let elapsed = started.elapsed();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample), (3516, 2328, 12));
    assert!(raw.width >= 3456 && raw.height >= 2304, "stored frame holds the Exif output size");
    assert_eq!(raw.white_level, 4095);
    assert_eq!(raw.unique_camera_model.as_deref(), Some("Canon EOS DIGITAL REBEL XT"));
    assert_eq!(raw.container_sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
    assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    let digest = |raw: &DngRaw| {
        let mut h = Sha256::new();
        for y in 0..raw.height {
            for x in 0..raw.width {
                h.update(raw.sample(x, y).to_le_bytes());
            }
        }
        hex(&h.finalize())
    };
    let first = digest(&raw);
    assert_eq!(first, digest(&decode_container(&bytes).unwrap()), "decode is deterministic");
    // Not a zero/constant frame: the sensor data has real spread.
    let (mut lo, mut hi) = (u16::MAX, 0u16);
    for y in (0..raw.height).step_by(97) {
        for x in (0..raw.width).step_by(89) {
            lo = lo.min(raw.sample(x, y));
            hi = hi.max(raw.sample(x, y));
        }
    }
    assert!(hi > lo.saturating_add(256), "sample spread {lo}..{hi}");
    println!("cr2 350d: {}x{} bits 12 samples_sha256={first} range~{lo}..{hi} decode={elapsed:?}", raw.width, raw.height);
    println!("NOT_PROVEN: {TEST} cfa_samples_vs_independent_dng reason=no_approved_converter_run");
}

/// D1H facts: raw SubIFD read by byte inspection (2026-10-14): 2012 x 1324, BitsPerSample 12,
/// Compression 1, CFAPattern 2,1,1,0 (BGGR), IFD0 Model "NIKON D1H".
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn nef_d1h_uncompressed_decodes_deterministically() {
    const TEST: &str = "nef_d1h_uncompressed_decodes_deterministically";
    let Some(bytes) = read(TEST, "nikon-d1h-12bit-uncompressed.nef", "d110b80a8b10a3a67b5cd1c15ef33f1a6397557b73d5d3f4e565739f0c072571") else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample, raw.white_level), (2012, 1324, 12, 4095));
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Bggr));
    assert_eq!(raw.unique_camera_model.as_deref(), Some("NIKON D1H"));
    assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    let digest = samples_digest(&raw);
    assert_eq!(digest, samples_digest(&decode_container(&bytes).unwrap()), "decode is deterministic");
    // MSB-first packing check on real data: same-colour neighbours two columns apart are close on
    // average (a wrong bit order scrambles them; measured 162 vs 632 by byte inspection).
    let mut total = 0u64;
    for x in 0..raw.width - 2 {
        total += u64::from(raw.sample(x, 500).abs_diff(raw.sample(x + 2, 500)));
    }
    let mean = total / u64::from(raw.width - 2);
    assert!(mean < 300, "row 500 same-colour mean step {mean}");
    println!("nef d1h: 2012x1324 bits 12 samples_sha256={digest} row500_step={mean}");
    println!("NOT_PROVEN: {TEST} cfa_samples_vs_independent_dng reason=no_approved_converter_run");
}

/// ILCE-7S facts: raw SubIFD read by byte inspection (2026-10-14): 2816 x 1872, BitsPerSample 14,
/// Compression 32767, one byte per pixel, CFAPattern 0,1,1,2 (RGGB), SonyToneCurve
/// 8000,10400,12900,14100 (curve maximum at code 0x7ff: 17204), IFD0 Model "ILCE-7S".
#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn arw_7s_craw_decodes_deterministically() {
    const TEST: &str = "arw_7s_craw_decodes_deterministically";
    let Some(bytes) = read(TEST, "sony-ilce-7s-14bit-compressed.arw", ARW_7S) else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample, raw.white_level), (2816, 1872, 14, 17204));
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Rggb));
    assert_eq!(raw.unique_camera_model.as_deref(), Some("ILCE-7S"));
    assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    let digest = samples_digest(&raw);
    assert_eq!(digest, samples_digest(&decode_container(&bytes).unwrap()), "decode is deterministic");
    // Group/interleave check on real data: same-colour neighbours two columns apart (inside one
    // 16-pixel group or across groups) and two rows apart differ comparably; a wrong interleave
    // or bit layout makes the horizontal step explode.
    let (mut hstep, mut vstep) = (0u64, 0u64);
    for y in (0..raw.height - 2).step_by(7) {
        for x in (0..raw.width - 2).step_by(5) {
            hstep += u64::from(raw.sample(x, y).abs_diff(raw.sample(x + 2, y)));
            vstep += u64::from(raw.sample(x, y).abs_diff(raw.sample(x, y + 2)));
        }
    }
    assert!(hstep < vstep.max(1) * 3 && vstep < hstep.max(1) * 3, "hstep {hstep} vstep {vstep}");
    println!("arw 7s: 2816x1872 bits 14 samples_sha256={digest} hstep={hstep} vstep={vstep}");
    println!("NOT_PROVEN: {TEST} cfa_samples_vs_independent_dng reason=no_approved_converter_run");
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

#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn vendor_files_route_to_typed_outcomes() {
    const TEST: &str = "vendor_files_route_to_typed_outcomes";
    let cases: [(&str, &str, DevelopError); 7] = [
        ("canon-eos-r6.cr3", "74abb0a113d075ad9887a058082f40dd2a938c4813a08474d82356f11a027778", DevelopError::DecoderUnavailable("cr3")),
        ("nikon-d70s-12bit-lossy-type1.nef", NEF_D70S, DevelopError::DecoderUnavailable("nef_compressed")),
        ("nikon-z6-12bit-compressed.nef", NEF_Z6, DevelopError::DecoderUnavailable("nef_compressed")),
        ("fujifilm-finepix-s5000.raf", RAF_S5000, DevelopError::DecoderUnavailable("raf")),
        ("fujifilm-x-s10-14bit-compressed-xtrans.raf", RAF_XS10, DevelopError::DecoderUnavailable("raf")),
        ("olympus-c5060wz-12bit.orf", ORF_C5060, DevelopError::DecoderUnavailable("orf")),
        ("panasonic-dmc-lx7.rw2", RW2_LX7, DevelopError::DecoderUnavailable("rw2")),
    ];
    for (file, sha, expected) in cases {
        if let Some(bytes) = read(TEST, file, sha) {
            assert_eq!(decode_container(&bytes).err(), Some(expected), "{file}");
        }
    }
}

const NEF_D70S: &str = "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692";
const NEF_Z6: &str = "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36";
const ARW_7S: &str = "a35ebb2fbec929daa5beb20d1ce5c15a8aac7b1a7a231455387f3df8a7442e07";
const RAF_S5000: &str = "dabd5e74521a6980156be9fd4b88d0c37b0fe4d0e0e6f5c12db8cffff1b76297";
const RAF_XS10: &str = "9419d1408ebf850395e5dd563beb96309466546252caf541466034c9cb7e9724";
const ORF_C5060: &str = "3fc12bc3f6320cfab456111d7805b446e329fdd1907680be55e2e163e5e084bd";
const RW2_LX7: &str = "18dfc3877e944020299635a8c2fad61fca2485b1c9b67645a495ee870a86e599";
