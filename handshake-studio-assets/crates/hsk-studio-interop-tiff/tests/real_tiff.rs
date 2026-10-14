//! Corpus test over the image-rs `image-tiff` test images (see `oracle-corpus/tiff/provenance.json`).
//! `#[ignore]`d: the corpus is external test DATA, never committed. Run with
//! `cargo nextest run -p hsk-studio-interop-tiff --test real_tiff --run-ignored ignored-only --no-capture`.
//!
//! The corpus root comes from the checked-in `.config/oracle-corpus.toml` (V4-03): start at the workspace
//! root, walk up at most `max-ancestors` parents, first ancestor holding `<search>` as a directory.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_tiff::*;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn corpus_root() -> Option<PathBuf> {
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

/// `(file, sha256)` pairs from provenance.json (flat scan: each `"file"` is followed by its `"sha256"`).
fn provenance(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("\"file\": \"") {
        rest = &rest[i + 9..];
        let name: String = rest.chars().take_while(|c| *c != '"').collect();
        let Some(j) = rest.find("\"sha256\": \"") else { break };
        let hash: String = rest[j + 11..].chars().take_while(|c| *c != '"').collect();
        out.push((name, hash));
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn summary(f: &TiffFile) -> String {
    let p = &f.pages[0];
    format!(
        "pages={} big={} {}x{} spp={} bits={} fmt={:?} pi={}/{:?} comp={} pred={} planar={} alpha={:?} icc={} extra={:?}",
        f.pages.len(),
        f.big_tiff,
        p.width,
        p.height,
        p.samples_per_pixel,
        p.bits_per_sample,
        p.sample_format,
        p.source_photometric,
        p.photometric,
        p.compression,
        p.predictor,
        p.planar_in_file,
        p.alpha,
        p.icc.as_ref().map_or(0, Vec::len),
        p.extra_samples,
    )
}

/// Pinned decode summary (or refusal code) per corpus file. Values were checked against the file names and
/// the corpus README; the cross-file pixel equalities below are the independent oracle.
const EXPECT: &[(&str, &str)] = &[
    ("rgb-3c-8b.tiff", "OK pages=1 big=false 157x151 spp=3 bits=8 fmt=Uint pi=2/Rgb comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("rgb-3c-16b.tiff", "OK pages=1 big=false 157x151 spp=3 bits=16 fmt=Uint pi=2/Rgb comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("gradient-3c-32b-float.tiff", "OK pages=1 big=false 256x1 spp=3 bits=32 fmt=Float pi=2/Rgb comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("predictor-3-rgb-f32.tif", "OK pages=1 big=false 200x200 spp=3 bits=32 fmt=Float pi=2/Rgb comp=5 pred=3 planar=false alpha=None icc=0 extra=[]"),
    ("minisblack-1c-16b.tiff", "OK pages=1 big=false 157x151 spp=1 bits=16 fmt=Uint pi=1/BlackIsZero comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("minisblack-2c-8b-alpha.tiff", "OK pages=1 big=false 64x64 spp=2 bits=8 fmt=Uint pi=1/BlackIsZero comp=32773 pred=1 planar=true alpha=Associated icc=0 extra=[1]"),
    ("extra_bits_rgb_8b.tiff", "OK pages=1 big=false 8x8 spp=4 bits=8 fmt=Uint pi=2/Rgb comp=1 pred=1 planar=false alpha=None icc=0 extra=[0]"),
    ("cmyk-4c-8b.tiff", "OK pages=1 big=false 157x151 spp=5 bits=8 fmt=Uint pi=5/Cmyk comp=5 pred=2 planar=false alpha=Unassociated icc=0 extra=[2]"),
    ("cmyk-3c-16b.tiff", "OK pages=1 big=false 157x151 spp=4 bits=16 fmt=Uint pi=5/Cmyk comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("issue_69_lzw.tiff", "OK pages=1 big=false 45x45 spp=1 bits=16 fmt=Uint pi=1/BlackIsZero comp=5 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("issue_69_packbits.tiff", "OK pages=1 big=false 45x45 spp=1 bits=16 fmt=Uint pi=1/BlackIsZero comp=32773 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("tiled-rgb-u8.tif", "OK pages=1 big=false 374x499 spp=3 bits=8 fmt=Uint pi=2/Rgb comp=5 pred=2 planar=false alpha=None icc=0 extra=[]"),
    ("tiled-jpeg-rgb-u8.tif", "OK pages=1 big=false 374x499 spp=3 bits=8 fmt=Uint pi=2/Rgb comp=7 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("tiled-jpeg-ycbcr.tif", "OK pages=1 big=false 374x499 spp=3 bits=8 fmt=Uint pi=6/Rgb comp=7 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("planar-rgb-u8.tif", "OK pages=1 big=false 374x499 spp=3 bits=8 fmt=Uint pi=2/Rgb comp=5 pred=1 planar=true alpha=None icc=0 extra=[]"),
    ("palette-1c-8b.tiff", "ERR photometric_palette"),
    ("int16_zstd.tif", "ERR compression_unsupported(50000)"),
    ("logluv-3c-16b.tiff", "ERR photometric_logluv"),
    ("seq__seq-1c-8b-multipage-adeefdcc.tiff", "OK pages=3 big=false 16x16 spp=1 bits=8 fmt=Uint pi=1/BlackIsZero comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-1c-8b-deflate-20f3db0c.tiff", "OK pages=1 big=false 16x16 spp=1 bits=8 fmt=Uint pi=1/BlackIsZero comp=32946 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-1c-16b-bigendian-68f373a0.tiff", "OK pages=1 big=false 16x16 spp=1 bits=16 fmt=Uint pi=1/BlackIsZero comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-1c-32f-deflate-fpredict-390fe673.tiff", "OK pages=1 big=false 16x16 spp=1 bits=32 fmt=Float pi=1/BlackIsZero comp=32946 pred=3 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-1c-8b-bigtiff-20f3db0c.tiff", "OK pages=1 big=true 16x16 spp=1 bits=8 fmt=Uint pi=1/BlackIsZero comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-3c-8b-lzw-8743c999.tiff", "OK pages=1 big=false 16x16 spp=3 bits=8 fmt=Uint pi=2/Rgb comp=5 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-3c-16b-bigtiff-1b40ca6e.tiff", "OK pages=1 big=true 16x16 spp=3 bits=16 fmt=Uint pi=2/Rgb comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-4c-8b-rgba-unassoc-50969cda.tiff", "OK pages=1 big=false 16x16 spp=4 bits=8 fmt=Uint pi=2/Rgb comp=1 pred=1 planar=false alpha=Unassociated icc=0 extra=[2]"),
    ("seq__seq-4c-8b-rgba-50969cda.tiff", "OK pages=1 big=false 16x16 spp=4 bits=8 fmt=Uint pi=2/Rgb comp=1 pred=1 planar=false alpha=Associated icc=0 extra=[1]"),
    ("seq__seq-4c-16b-cmyk-c6e52592.tiff", "OK pages=1 big=false 16x16 spp=4 bits=16 fmt=Uint pi=5/Cmyk comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-3c-8b-tiled-8743c999.tiff", "OK pages=1 big=false 16x16 spp=3 bits=8 fmt=Uint pi=2/Rgb comp=1 pred=1 planar=false alpha=None icc=0 extra=[]"),
    ("seq__seq-3c-12b-contig-e6f40b4a.tiff", "ERR bits_unsupported(12)"),
];

/// Files that encode the same picture losslessly must decode to identical samples.
const SAME_PIXELS: &[&[&str]] = &[
    &["tiled-rgb-u8.tif", "planar-rgb-u8.tif"],
    &[
        "seq__seq-1c-8b-deflate-20f3db0c.tiff",
        "seq__seq-1c-8b-bigtiff-20f3db0c.tiff",
        "seq__seq-1c-8b-multipage-adeefdcc.tiff",
    ],
    &["seq__seq-3c-8b-lzw-8743c999.tiff", "seq__seq-3c-8b-tiled-8743c999.tiff"],
    &["seq__seq-4c-8b-rgba-50969cda.tiff", "seq__seq-4c-8b-rgba-unassoc-50969cda.tiff"],
    &["issue_69_lzw.tiff", "issue_69_packbits.tiff"],
];

fn mean_abs_diff(a: &Samples, b: &Samples) -> f64 {
    let (Samples::U8(a), Samples::U8(b)) = (a, b) else { panic!("u8 pages expected") };
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len() as f64
}

#[test]
#[ignore = "needs the downloaded image-tiff corpus under the oracle-corpus root"]
fn corpus_files_decode_or_refuse_as_pinned() {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: real_tiff corpus reason=oracle-corpus root not found via .config/oracle-corpus.toml");
        return;
    };
    let dir = root.join("tiff");
    let Ok(prov) = std::fs::read_to_string(dir.join("provenance.json")) else {
        println!("NOT_PROVEN: real_tiff corpus reason=tiff/provenance.json absent");
        return;
    };
    let listed = provenance(&prov);
    assert_eq!(listed.len(), EXPECT.len(), "every provenance file is pinned");
    let mut decoded = std::collections::BTreeMap::new();
    for (file, sha) in &listed {
        let bytes = std::fs::read(dir.join(file)).expect("corpus file listed in provenance");
        assert_eq!(&hex(&Sha256::digest(&bytes)), sha, "sha256 of {file}");
        let want = EXPECT.iter().find(|(f, _)| f == file).unwrap_or_else(|| panic!("{file} not pinned")).1;
        let got = match decode(&bytes, &Limits::default(), &CancellationToken::default()) {
            Ok(f) => {
                let s = format!("OK {}", summary(&f));
                decoded.insert(file.clone(), f);
                s
            }
            Err(e) => format!("ERR {e}"),
        };
        assert_eq!(got, want, "{file}");
        println!("PINNED {file} => {got}");
        // Hostile sweep on real bytes (header/IFD region mutations and truncations): typed result, never a panic.
        let small = Limits { max_page_decoded_bytes: 16 << 20, max_total_decoded_bytes: 32 << 20, ..Limits::default() };
        let live = CancellationToken::default();
        for i in 0..bytes.len().min(300) {
            for v in [0_u8, 0xFF] {
                let mut m = bytes.clone();
                m[i] = v;
                let _ = decode(&m, &small, &live);
            }
        }
        for k in 0..32 {
            let _ = decode(&bytes[..bytes.len() / 32 * k], &small, &live);
        }
    }
    for group in SAME_PIXELS {
        let first = decoded[group[0]].pages[0].pixels();
        for other in &group[1..] {
            assert_eq!(decoded[*other].pages[0].pixels(), first, "{} vs {}", group[0], other);
        }
    }
    // JPEG YCbCr is converted to RGB by this crate (the codec leaves Y/Cb/Cr; unconverted it differs by ~62): it must sit near the RGB JPEG and the lossless original.
    let ycc = decoded["tiled-jpeg-ycbcr.tif"].pages[0].pixels();
    let d_rgb = mean_abs_diff(ycc, decoded["tiled-jpeg-rgb-u8.tif"].pages[0].pixels());
    let d_orig = mean_abs_diff(ycc, decoded["tiled-rgb-u8.tif"].pages[0].pixels());
    println!("YCBCR mean abs diff vs jpeg-rgb {d_rgb:.2}, vs lossless {d_orig:.2}");
    assert!(d_rgb < 12.0 && d_orig < 12.0, "YCbCr samples are not RGB: {d_rgb} {d_orig}");
}
