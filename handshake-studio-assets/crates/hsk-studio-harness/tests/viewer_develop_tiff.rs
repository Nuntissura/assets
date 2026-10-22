//! SMB-DEVELOP-EXPORT-TIFF (feature `viewer`): `develop SOURCE OUT --tiff ...` through the CLI on the synthetic
//! DNG of tests/common (assembled byte by byte, no corpus). The TIFF is read back with hsk-studio-interop-tiff and
//! compared with the 16-bit PNG export of the same recipe. Lightroom Classic export equivalence is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::cli;
use hsk_studio_harness::view::develop::SRGB_V4_ICC;
use hsk_studio_interop_tiff as tiff;
use serde_json::Value;

fn cli(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let o = cli::run(&args);
    (o.exit, serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", o.stdout)))
}

fn read(path: &std::path::Path) -> tiff::TiffFile {
    tiff::decode(&std::fs::read(path).unwrap(), &tiff::Limits::default(), &CancellationToken::default()).unwrap()
}

#[test]
fn cli_tiff_export_matches_the_png_samples_and_embeds_the_profile() {
    let scratch = Scratch::new("develop-tiff");
    let src = scratch.0.join("synthetic.dng");
    std::fs::write(&src, synthetic_dng(64, 48)).unwrap();
    let (src, dir) = (src.to_string_lossy().into_owned(), &scratch.0);
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let adjust = ["--adjust", "develop.exposure", "0.5", "--adjust", "develop.contrast", "20"];
    let run = |name: &str, format: &[&str]| {
        let out = path(name);
        let mut args = vec!["develop", src.as_str(), out.as_str()];
        args.extend_from_slice(format);
        args.extend_from_slice(&adjust);
        let (exit, report) = cli(&args);
        assert_eq!(exit, 0, "{report}");
        report
    };
    let png = run("ref.png", &["--png", "--depth", "16"]);
    let want = png["export"]["samples_sha256"].as_str().unwrap().to_owned();
    for (name, format) in [
        ("plain.tif", vec!["--tiff"]),
        ("zip-be.tif", vec!["--tiff", "--compression", "zip", "--predictor", "--byte-order", "be", "--dpi", "300"]),
        ("lzw-big.tif", vec!["--tiff", "--compression", "lzw", "--bigtiff", "--colour-space", "srgb"]),
    ] {
        let report = run(name, &format);
        let file = read(&dir.join(name));
        let page = &file.pages[0];
        assert_eq!((page.width, page.height, page.bits_per_sample), (64, 48, 16), "{name}");
        assert_eq!(page.icc.as_deref(), Some(SRGB_V4_ICC), "{name}");
        let tiff::Samples::U16(samples) = page.pixels() else { panic!("{name}: not 16-bit") };
        let be: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
        assert_eq!(hsk_studio_package::sha256_hex_of(&be), want, "{name}: TIFF pixels equal the PNG export");
        assert_eq!(report["export"]["format"], "tiff", "{name}");
        assert_eq!(report["export"]["bytes"].as_u64(), Some(std::fs::metadata(dir.join(name)).unwrap().len()), "{name}");
        assert_eq!(report["export"]["colour_space"], "srgb");
    }
    let be = read(&dir.join("zip-be.tif"));
    assert_eq!((be.byte_order, be.big_tiff, be.pages[0].compression, be.pages[0].predictor), (tiff::ByteOrder::Big, false, 8, 2));
    assert_eq!(be.pages[0].resolution.map(|r| r.x), Some((300, 1)));
    let big = read(&dir.join("lzw-big.tif"));
    assert_eq!((big.byte_order, big.big_tiff, big.pages[0].compression), (tiff::ByteOrder::Little, true, 5));
    // 8-bit TIFF equals the 8-bit PNG buffer.
    let png8 = run("ref8.png", &["--png", "--depth", "8"]);
    run("eight.tif", &["--tiff", "--depth", "8"]);
    let eight = read(&dir.join("eight.tif"));
    let tiff::Samples::U8(samples) = eight.pages[0].pixels() else { panic!("not 8-bit") };
    assert_eq!(hsk_studio_package::sha256_hex_of(samples), png8["export"]["samples_sha256"].as_str().unwrap());
}

#[test]
fn cli_tiff_refusals_and_usage_errors_are_typed_and_write_nothing() {
    let scratch = Scratch::new("develop-tiff-refuse");
    let src = scratch.0.join("synthetic.dng");
    std::fs::write(&src, synthetic_dng(64, 48)).unwrap();
    let src = src.to_string_lossy().into_owned();
    let out = scratch.0.join("never.tif");
    let out_s = out.to_string_lossy().into_owned();
    let refused = |extra: &[&str], code: &str, detail: &str| {
        let mut args = vec!["develop", src.as_str(), out_s.as_str(), "--tiff"];
        args.extend_from_slice(extra);
        let (exit, report) = cli(&args);
        assert_eq!((exit, report["error"].as_str()), (1, Some(code)), "{extra:?}: {report}");
        assert!(report["detail"]["message"].as_str().unwrap().contains(detail), "{extra:?}: {report}");
        assert!(!out.exists(), "{extra:?}: refusal wrote a file");
    };
    refused(&["--colour-space", "adobe-rgb"], "colour_space_profile_missing", "adobe_rgb_1998");
    refused(&["--colour-space", "prophoto"], "colour_space_profile_missing", "prophoto_rgb");
    refused(&["--colour-space", "display-p3"], "profile_colour_space_mismatch", "colorants_differ");
    refused(&["--dpi", "0"], "invalid_resolution", "");
    refused(&["--dpi", "70000"], "invalid_resolution", "");
    let usage = |extra: &[&str]| {
        let mut args = vec!["develop", src.as_str(), out_s.as_str()];
        args.extend_from_slice(extra);
        let (exit, _) = cli(&args);
        assert_eq!(exit, 2, "{extra:?}");
        assert!(!out.exists());
    };
    usage(&["--tiff", "--png"]);
    usage(&["--tiff", "--jpeg"]);
    usage(&["--tiff", "--predictor"]);
    usage(&["--tiff", "--quality", "50"]);
    usage(&["--tiff", "--compression", "bzip"]);
    usage(&["--tiff", "--byte-order", "middle"]);
    usage(&["--tiff", "--colour-space", "cmyk"]);
    usage(&["--tiff", "--depth", "12"]);
    usage(&["--png", "--dpi", "300"]);
    usage(&["--jpeg", "--compression", "zip"]);
    usage(&["--png", "--bigtiff"]);
}
