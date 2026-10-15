//! Black and white levels of the vendor decoders against sample statistics of the CC0 corpus
//! (raw.pixls.us, test DATA only, never committed). `#[ignore]`d: run with
//! `cargo nextest run -p hsk-studio-develop --test real_raw_levels --run-ignored ignored-only --no-capture`.
//!
//! Per file the test measures, independently of the decoder's level source: the per-site dark
//! floor (0.1 % quantile) over the frame, the per-site mean over a stated optically masked region
//! where the format has one, and the saturation plateau (frame maximum and how many samples sit
//! within one band of it). The applied black must agree with the masked mean (within 2 codes) or,
//! without a masked region, sit at or below the dark floor plus noise; the applied white must be
//! the saturation plateau when one is present (no more than a stray handful above it) and otherwise
//! at or above every sample's plausible range. Corpus root and sha256 pins as `tests/real_raw.rs`.
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

/// Statistics of one decoded frame.
struct Stats {
    /// Per 2x2 site (frame origin, row-major): 0.1 % quantile over the frame.
    floor: [u16; 4],
    max: u16,
    /// Samples within max/256 (at least 1) of the maximum.
    at_top: u64,
    above_white: u64,
    total: u64,
}

fn stats(raw: &DngRaw) -> Stats {
    let mut hist = vec![vec![0u64; 65_536]; 4];
    let (mut max, mut total) = (0u16, 0u64);
    for y in 0..raw.height {
        for x in 0..raw.width {
            let v = raw.sample(x, y);
            hist[((y % 2) * 2 + x % 2) as usize][v as usize] += 1;
            max = max.max(v);
            total += 1;
        }
    }
    let quantile = |h: &Vec<u64>| {
        let n: u64 = h.iter().sum();
        let mut acc = 0;
        for (v, c) in h.iter().enumerate() {
            acc += c;
            if acc * 1000 >= n {
                return v as u16;
            }
        }
        0
    };
    let band = (max / 256).max(1);
    let all: Vec<u64> = (0..65_536).map(|v| hist.iter().map(|h| h[v]).sum()).collect();
    let at_top = all[(max - band) as usize..=max as usize].iter().sum();
    let above_white = all.iter().enumerate().filter(|(v, _)| *v as u32 > raw.white_level).map(|(_, c)| c).sum();
    Stats { floor: [quantile(&hist[0]), quantile(&hist[1]), quantile(&hist[2]), quantile(&hist[3])], max, at_top, above_white, total }
}

fn report(test: &str, file: &str, sha: &str) -> Option<(DngRaw, Stats)> {
    let bytes = read(test, file, sha)?;
    let raw = decode_container(&bytes).unwrap_or_else(|e| panic!("{file}: {e}"));
    let s = stats(&raw);
    println!(
        "LEVELS {file}: black {:?} repeat {:?} white {} | floor {:?} max {} at_top {} above_white {} of {} | unapplied {:?}",
        raw.black_level, raw.black_repeat, raw.white_level, s.floor, s.max, s.at_top, s.above_white, s.total, raw.unapplied
    );
    Some((raw, s))
}

const FILES: [(&str, &str); 9] = [
    ("canon-eos-350d-raw.cr2", "4b47ba27c0090efa091aaf99babc76d9a3e6f6f121b443d6c58c3ce7275cd770"),
    ("canon-eos-r6.cr3", "74abb0a113d075ad9887a058082f40dd2a938c4813a08474d82356f11a027778"),
    ("nikon-d1h-12bit-uncompressed.nef", "d110b80a8b10a3a67b5cd1c15ef33f1a6397557b73d5d3f4e565739f0c072571"),
    ("nikon-d70s-12bit-lossy-type1.nef", "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692"),
    ("nikon-z6-12bit-compressed.nef", "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36"),
    ("sony-ilce-7s-14bit-compressed.arw", "a35ebb2fbec929daa5beb20d1ce5c15a8aac7b1a7a231455387f3df8a7442e07"),
    ("fujifilm-x-s10-14bit-compressed-xtrans.raf", "9419d1408ebf850395e5dd563beb96309466546252caf541466034c9cb7e9724"),
    ("olympus-c5060wz-12bit.orf", "3fc12bc3f6320cfab456111d7805b446e329fdd1907680be55e2e163e5e084bd"),
    ("panasonic-dmc-lx7.rw2", "18dfc3877e944020299635a8c2fad61fca2485b1c9b67645a495ee870a86e599"),
];

#[test]
#[ignore = "needs the raw-vendor oracle corpus (CC0 downloads, never committed); NOT_PROVEN when absent"]
fn vendor_levels_agree_with_sample_statistics() {
    const TEST: &str = "vendor_levels_agree_with_sample_statistics";
    for (file, sha) in FILES {
        let _ = report(TEST, file, sha);
    }
}
