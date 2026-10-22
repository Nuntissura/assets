//! SMB-DEVELOP-DNG-WRITER (feature `viewer`): `develop SOURCE OUT --dng ...` through the CLI on the synthetic DNG
//! of tests/common (assembled byte by byte, no corpus). The exported DNG is read back with develop's own reader
//! (raw data must equal the source sample for sample) and with the interop-tiff container facade (preview, XMP).
//! Lightroom Classic / Adobe DNG Converter equivalence is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_harness::cli;
use hsk_studio_interop_tiff as tiff;
use serde_json::Value;

fn cli(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let o = cli::run(&args);
    (o.exit, serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", o.stdout)))
}

const XMP: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" crs:Exposure2012=\"+0.50\"/></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";

#[test]
fn cli_dng_export_preserves_the_raw_frame_and_carries_preview_and_xmp() {
    let scratch = Scratch::new("develop-dng");
    let source_bytes = synthetic_dng(64, 48);
    let src = scratch.0.join("synthetic.dng");
    std::fs::write(&src, &source_bytes).unwrap();
    let xmp = scratch.0.join("recipe.xmp");
    std::fs::write(&xmp, XMP).unwrap();
    let (src, dir) = (src.to_string_lossy().into_owned(), &scratch.0);
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let xmp_path = xmp.to_string_lossy().into_owned();
    let source = hsk_studio_develop::decode_container(&source_bytes).unwrap();
    let adjust = ["--adjust", "develop.exposure", "0.5"];
    for (name, extra) in [
        ("plain.dng", vec!["--dng"]),
        ("ljpeg.dng", vec!["--dng", "--dng-compression", "ljpeg", "--dng-xmp", xmp_path.as_str()]),
        ("nopreview.dng", vec!["--dng", "--dng-preview", "none"]),
        ("full.dng", vec!["--dng", "--dng-preview", "full", "--dng-xmp", xmp_path.as_str()]),
    ] {
        let out = path(name);
        let mut args = vec!["develop", src.as_str(), out.as_str()];
        args.extend_from_slice(&extra);
        args.extend_from_slice(&adjust);
        let (exit, report) = cli(&args);
        assert_eq!(exit, 0, "{name}: {report}");
        let export = &report["export"];
        assert_eq!(export["format"], "dng", "{name}");
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(export["bytes"].as_u64(), Some(bytes.len() as u64), "{name}");
        assert_eq!(export["file_sha256"].as_str(), Some(hsk_studio_package::sha256_hex_of(&bytes).as_str()), "{name}");
        // The raw frame of the exported file is the source frame, whatever the recipe says.
        let back = hsk_studio_develop::decode_container(&bytes).unwrap();
        assert_eq!(back.samples(), source.samples(), "{name}: raw samples preserved");
        assert_eq!((back.width, back.height, back.white_level), (source.width, source.height, source.white_level), "{name}");
        assert_eq!(back.unique_camera_model, source.unique_camera_model, "{name}");
        let c = tiff::Container::parse(&bytes, &tiff::Limits::default()).unwrap();
        let ifds = c.walk().unwrap();
        let with_xmp = extra.contains(&"--dng-xmp");
        let xmp_entry = ifds[0].find(700);
        assert_eq!(xmp_entry.map(|e| c.bytes(e).unwrap().to_vec()), with_xmp.then(|| XMP.as_bytes().to_vec()), "{name}: XMP packet");
        assert_eq!(export["xmp_bytes"].as_u64(), Some(if with_xmp { XMP.len() as u64 } else { 0 }), "{name}");
        match name {
            "nopreview.dng" => {
                assert_eq!(ifds.len(), 1, "raw IFD is IFD0");
                assert!(export["preview"].is_null());
            }
            _ => {
                assert_eq!(ifds.len(), 2, "{name}: preview IFD0 + raw SubIFD");
                assert_eq!(c.uints(ifds[0].find(254).unwrap()).unwrap(), vec![1], "{name}");
                assert_eq!(export["preview"]["width"], 64, "{name}: a 64x48 source is not upscaled");
            }
        }
        if name == "ljpeg.dng" {
            assert_eq!(export["compression"], "losslessjpeg");
            assert!(bytes.len() < std::fs::metadata(dir.join("plain.dng")).unwrap().len() as usize);
        }
    }
    // The preview shows the recipe: a different exposure changes the preview but not the raw samples.
    let brighter = path("bright.dng");
    let (exit, _) = cli(&["develop", src.as_str(), brighter.as_str(), "--dng", "--adjust", "develop.exposure", "2"]);
    assert_eq!(exit, 0);
    let (a, b) = (std::fs::read(dir.join("plain.dng")).unwrap(), std::fs::read(dir.join("bright.dng")).unwrap());
    assert_ne!(a, b);
    assert_eq!(hsk_studio_develop::decode_container(&a).unwrap().samples(), hsk_studio_develop::decode_container(&b).unwrap().samples());
}

#[test]
fn cli_dng_refusals_and_usage_errors_are_typed_and_write_nothing() {
    let scratch = Scratch::new("develop-dng-refuse");
    let src = scratch.0.join("synthetic.dng");
    std::fs::write(&src, synthetic_dng(64, 48)).unwrap();
    let src = src.to_string_lossy().into_owned();
    let out = scratch.0.join("never.dng");
    let out_s = out.to_string_lossy().into_owned();
    let bad_xmp = scratch.0.join("bad.xmp");
    std::fs::write(&bad_xmp, "not an xmp packet").unwrap();
    let bad_xmp = bad_xmp.to_string_lossy().into_owned();
    let missing = scratch.0.join("missing.xmp").to_string_lossy().into_owned();
    let refused = |extra: &[&str], code: &str| {
        let mut args = vec!["develop", src.as_str(), out_s.as_str(), "--dng"];
        args.extend_from_slice(extra);
        let (exit, report) = cli(&args);
        assert_eq!((exit, report["error"].as_str()), (1, Some(code)), "{extra:?}: {report}");
        assert!(!out.exists(), "{extra:?}: refusal wrote a file");
    };
    refused(&["--dng-xmp", bad_xmp.as_str()], "xmp_invalid");
    refused(&["--dng-xmp", missing.as_str()], "xmp_unreadable");
    let usage = |extra: &[&str]| {
        let mut args = vec!["develop", src.as_str(), out_s.as_str()];
        args.extend_from_slice(extra);
        let (exit, _) = cli(&args);
        assert_eq!(exit, 2, "{extra:?}");
        assert!(!out.exists());
    };
    usage(&["--dng", "--png"]);
    usage(&["--dng", "--jpeg"]);
    usage(&["--dng", "--tiff"]);
    usage(&["--dng", "--depth", "16"]);
    usage(&["--dng", "--quality", "50"]);
    usage(&["--dng", "--compression", "zip"]);
    usage(&["--dng", "--dng-compression", "zip"]);
    usage(&["--dng", "--dng-preview", "huge"]);
    usage(&["--png", "--dng-preview", "none"]);
    usage(&["--tiff", "--dng-compression", "ljpeg"]);
    usage(&["--jpeg", "--dng-allow-dropped"]);
    usage(&["--dng-xmp", "x.xmp"]);
}
