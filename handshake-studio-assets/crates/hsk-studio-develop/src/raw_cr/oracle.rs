//! Oracle parity of the clean-room decoders (lane SMB-RAW-CLEANROOM-2): each decoded CFA plane
//! against the stored CFA plane of the Adobe DNG Converter 18.7 uncompressed DNG (`-u -p0`) of the
//! same file, bit for bit, on the whole region the DNG stores (its offset in the sensor frame is
//! pinned per file; columns or rows the converter crops away are not compared). The DNG is read by
//! develop's own DNG reader (`crate::dng::decode`); no other RAW decoder is involved.
//!
//! Corpus: `<oracle corpus root>/raw-vendor|raw-vendor-cleanroom/` (RAW samples, raw.pixls.us,
//! CC0-1.0) and `<root>/raw-dng-oracle/` (converter output), root from `.config/oracle-corpus.toml`.
//! Every file is pinned by sha256; an absent root, file or hash prints `NOT_PROVEN:` and proves
//! nothing. `#[ignore]`d (needs the corpus).
#![allow(clippy::arithmetic_side_effects)]
use crate::{DevelopError, DngRaw};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn convention_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

fn corpus_root() -> Result<PathBuf, String> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).ok_or("workspace_root_absent")?.to_path_buf();
    let text = std::fs::read_to_string(ws.join(".config").join("oracle-corpus.toml")).map_err(|e| format!("corpus_convention_absent error={e}"))?;
    let search = convention_value(&text, "search").ok_or("corpus_convention_malformed key=search")?;
    let max: usize = convention_value(&text, "max-ancestors").and_then(|v| v.parse().ok()).ok_or("corpus_convention_malformed key=max-ancestors")?;
    ws.ancestors().take(max + 1).map(|a| a.join(&search)).find(|c| c.is_dir()).ok_or_else(|| format!("corpus_root_absent search={search}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read `<root>/<rel>` and check its pinned sha256; `None` (with a NOT_PROVEN line) otherwise.
pub(crate) fn read_pinned(test: &str, rel: &str, sha256: &str) -> Option<Vec<u8>> {
    let root = match corpus_root() {
        Ok(r) => r,
        Err(reason) => {
            println!("NOT_PROVEN: {test} {rel} reason={reason}");
            return None;
        }
    };
    let Ok(bytes) = std::fs::read(root.join(rel)) else {
        println!("NOT_PROVEN: {test} {rel} reason=file_absent");
        return None;
    };
    let got = hex(&Sha256::digest(&bytes));
    if got != sha256 {
        println!("NOT_PROVEN: {test} {rel} reason=sha256_mismatch got={got}");
        return None;
    }
    Some(bytes)
}

/// One corpus case: the RAW file, its converter DNG, and the DNG region's origin in the frame.
pub(crate) struct Case {
    pub raw: &'static str,
    pub raw_sha256: &'static str,
    pub dng: &'static str,
    pub dng_sha256: &'static str,
    pub origin: (u32, u32),
}

/// The converter DNG's stored CFA plane (before any LinearizationTable), read with develop's
/// TIFF/EP strip reader: (width, height, samples).
pub(crate) fn dng_stored_plane(bytes: &[u8]) -> (u32, u32, Vec<u16>) {
    use crate::tiffep::{self, Tiff};
    let (tiff, first) = Tiff::parse(bytes).expect("converter DNG parses");
    let ifds = tiff.walk(first).expect("IFDs");
    let raw = tiffep::cfa_ifd(&tiff, &ifds).expect("walk").expect("a CFA IFD");
    let (w, h, bits, compression) = tiffep::geometry(&tiff, raw).expect("geometry");
    assert_eq!((bits, compression), (16, 1), "uncompressed 16-bit converter output");
    (w, h, tiffep::uncompressed_strips(&tiff, raw, w, h, bits, bytes.len()).expect("strips"))
}

/// The converter DNG's LinearizationTable (tag 50712), if any.
pub(crate) fn dng_linearization(bytes: &[u8]) -> Option<Vec<u32>> {
    use crate::tiffep::{self, Tiff};
    let (tiff, first) = Tiff::parse(bytes).ok()?;
    let ifds = tiff.walk(first).ok()?;
    let raw = tiffep::cfa_ifd(&tiff, &ifds).ok()??;
    tiff.u32s(raw.find(50712)?).ok()
}

/// A decoder's stored plane: (width, height, samples) as the file codes them (lossy curve indices
/// before the curve).
pub(crate) type Plane = fn(&[u8]) -> Result<(u32, u32, Vec<u16>), DevelopError>;

/// Compare every case; returns the mismatching cases (empty = all bit-exact). Prints one
/// `ORACLE <file> equal=<bool> compared=<n>` line per case.
pub(crate) fn compare(test: &str, cases: &[Case], plane: Plane) -> Vec<String> {
    let mut bad = Vec::new();
    for c in cases {
        let (Some(raw_bytes), Some(dng_bytes)) = (read_pinned(test, c.raw, c.raw_sha256), read_pinned(test, c.dng, c.dng_sha256)) else {
            continue;
        };
        let (rw, rh, ours) = match plane(&raw_bytes) {
            Ok(p) => p,
            Err(e) => {
                println!("ORACLE {} decode_error={e}", c.raw);
                bad.push(format!("{} decode_error={e}", c.raw));
                continue;
            }
        };
        let (dw, dh, theirs) = dng_stored_plane(&dng_bytes);
        let (x0, y0) = c.origin;
        if x0 + dw > rw || y0 + dh > rh {
            bad.push(format!("{} region {dw}x{dh} at {:?} outside frame {rw}x{rh}", c.raw, c.origin));
            continue;
        }
        let mut diff = 0u64;
        let mut first = None;
        for y in 0..dh {
            for x in 0..dw {
                let a = ours[((y + y0) * rw + x + x0) as usize];
                let b = theirs[(y * dw + x) as usize];
                if a != b {
                    diff += 1;
                    first.get_or_insert((x, y, a, b));
                }
            }
        }
        println!("ORACLE {} equal={} compared={} frame={rw}x{rh} dng={dw}x{dh} at {:?}", c.raw, diff == 0, u64::from(dw) * u64::from(dh), c.origin);
        if diff != 0 {
            bad.push(format!("{} differs at {diff} sites, first (x, y, ours, dng) {first:?}", c.raw));
        }
    }
    bad
}

/// The stored plane of a decoder that returns a `DngRaw` (no curve: samples as decoded).
pub(crate) fn samples_of(r: Result<DngRaw, DevelopError>) -> Result<(u32, u32, Vec<u16>), DevelopError> {
    let r = r?;
    let s = (0..r.height).flat_map(|y| (0..r.width).map(move |x| (x, y))).map(|(x, y)| r.sample(x, y)).collect();
    Ok((r.width, r.height, s))
}
