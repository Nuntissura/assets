//! SMB-CONVERT-TIFF focused tests (feature `import-tiff`): plain TIFF -> interop-tiff decode ->
//! interop-psd `raster_proposal` -> native package with receipt, typed losses and the original.
//! The first test assembles its TIFF bytes by hand (no encoder); the corpus test reads 4 files of
//! `oracle-corpus/tiff` through the checked-in `.config/oracle-corpus.toml`, each pinned by
//! SHA-256, and prints `NOT_PROVEN:` when the corpus is absent (V4-03).
#![cfg(feature = "import-tiff")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::{ImportError, SourceFormat, SourceMint, convert_to_native, import, sniff};
use hsk_studio_harness::{Harness, References, Selection, asset_local_test_context};
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_tiff as tiff;
use hsk_studio_package::{ImportRole, Limits, read_package};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Little-endian classic TIFF with one IFD; entry values longer than 4 bytes are placed after it.
fn tiff_le(entries: &[(u16, u16, u32, Vec<u8>)]) -> Vec<u8> {
    let ifd_len = 2 + entries.len() * 12 + 4;
    let mut data_at = 8 + ifd_len;
    let mut ifd = (entries.len() as u16).to_le_bytes().to_vec();
    let mut data = Vec::new();
    for (tag, kind, count, value) in entries {
        ifd.extend(tag.to_le_bytes());
        ifd.extend(kind.to_le_bytes());
        ifd.extend(count.to_le_bytes());
        if value.len() <= 4 {
            let mut inline = value.clone();
            inline.resize(4, 0);
            ifd.extend(inline);
        } else {
            ifd.extend((data_at as u32).to_le_bytes());
            data.extend(value);
            if value.len() % 2 == 1 {
                data.push(0);
            }
            data_at = 8 + ifd_len + data.len();
        }
    }
    ifd.extend(0u32.to_le_bytes());
    let mut out = b"II*\0".to_vec();
    out.extend(8u32.to_le_bytes());
    out.extend(ifd);
    out.extend(data);
    out
}

const SHORT: u16 = 3;
const LONG: u16 = 4;
const UNDEFINED: u16 = 7;

fn short(v: u16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// 2x2 RGB 8-bit, uncompressed, orientation 6, with a Photoshop ImageSourceData block (tag 37724).
fn layered_photoshop_tiff(pixels: &[u8; 12]) -> Vec<u8> {
    let mut isd = b"Adobe Photoshop Document Data Block\0MIB8".to_vec();
    isd.extend_from_slice(b"8BIMLr16\x04\0\0\0\0\0\0\0");
    // Pixel data offset: header 8 + IFD (2 + 12*12 + 4) + BitsPerSample (6) + ImageSourceData (len, even).
    let pixel_at = 8 + 2 + 12 * 12 + 4 + 6 + isd.len();
    tiff_le(&[
        (256, SHORT, 1, short(2)),
        (257, SHORT, 1, short(2)),
        (258, SHORT, 3, [8u16, 8, 8].iter().flat_map(|v| v.to_le_bytes()).collect()),
        (259, SHORT, 1, short(1)),
        (262, SHORT, 1, short(2)),
        (273, LONG, 1, (pixel_at as u32).to_le_bytes().to_vec()),
        (274, SHORT, 1, short(6)),
        (277, SHORT, 1, short(3)),
        (278, SHORT, 1, short(2)),
        (279, LONG, 1, 12u32.to_le_bytes().to_vec()),
        (284, SHORT, 1, short(1)),
        (37724, UNDEFINED, isd.len() as u32, isd),
    ])
    .into_iter()
    .chain(pixels.iter().copied())
    .collect()
}

fn harness() -> Harness {
    Harness::new(Selection { providers: Vec::new(), references: References::TrustAll }).unwrap()
}

fn codes(receipt: &Value) -> Vec<String> {
    receipt["loss"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap().to_string()).collect()
}

#[test]
fn photoshop_tiff_imports_its_composite_exactly_and_records_the_layers_as_typed_loss() {
    let pixels = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 10, 20, 30];
    let bytes = layered_photoshop_tiff(&pixels);
    assert_eq!(sniff(&bytes), Some(SourceFormat::Tiff));
    // The hand-built page is what interop-tiff decodes (independent check of the fixture).
    let decoded = tiff::decode(&bytes, &tiff::Limits::default(), &CancellationToken::default()).expect("fixture decodes");
    assert_eq!(decoded.pages[0].pixels(), &tiff::Samples::U8(pixels.to_vec()));

    let cancel = CancellationToken::default();
    let (package, imported) = convert_to_native(&bytes, &TrustAllResolver, &cancel).expect("converted");
    assert_eq!(imported.format, SourceFormat::Tiff);
    assert_eq!(imported.receipt["layers"], 1);
    let receipt = &imported.receipt;
    assert_eq!(receipt["source_format"], "tiff");
    assert_eq!(receipt["tiff"]["image_source_data"]["envelope"], "valid");
    assert_eq!(receipt["tiff"]["image_source_data"]["byte_order"], "little");
    // interop-psd parses little-endian (`MIB8`) runs since 68cba91; this envelope-only fixture's run
    // is not a valid little-endian block (`8BIM` signature with a big-endian key `Lr16`), so the
    // reader's typed refusal is recorded and the composite is the fallback, never a misread layer.
    let parse_error = receipt["tiff"]["image_source_data"]["parse_error"].as_str().unwrap_or_default();
    assert_eq!(parse_error, "psd_tagged_block_limit", "{receipt}");
    let codes = codes(receipt);
    for code in ["tiff_photoshop_layers_not_imported", "tiff_orientation_not_applied", "profile_assumed_srgb"] {
        assert!(codes.iter().any(|c| c == code), "{code} missing from {codes:?}");
    }

    // Pixel mapping equals interop-psd's own raster route for the same planes (same tile digests).
    let plane = |c: usize| psd::Plane { width: 2, height: 2, depth: 8, bytes: pixels.iter().skip(c).step_by(3).copied().collect() };
    let limits = psd::Limits::default();
    let cfg = psd::ProposalConfig::new(&limits, folio::Budget::default(), &TrustAllResolver);
    let source = psd::SourceIdentity::of(&bytes);
    let raster = psd::RasterSource {
        format: "tiff",
        name: "Background",
        width: 2,
        height: 2,
        depth: 8,
        colour_mode: psd::ColorMode::Rgb,
        planes: vec![plane(0), plane(1), plane(2)],
        alpha: None,
        icc: None,
    };
    let direct = psd::raster_proposal(raster, &source, &cfg, &mut SourceMint::new(&source.sha256), None, &cancel).unwrap();
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).unwrap();
    assert!(!direct.tiles.is_empty());
    for tile in &direct.tiles {
        assert!(opened.assets.contains_key(&tile.content_sha256_hex), "tile {} not in the package", tile.content_sha256_hex);
    }
    // The original keeps the 37724 block byte-exact; the receipt entry is the one in the package.
    assert_eq!(opened.import_bytes(ImportRole::Original), Some(&bytes[..]));
    let stored: Value = serde_json::from_slice(opened.import_bytes(ImportRole::Receipt).unwrap()).unwrap();
    assert_eq!(&stored, receipt);

    // Open directly: one raster layer in a live session.
    let h = harness();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_import(&bytes));
    assert!(run.summary());
    let report = run.finish("import");
    assert_eq!(report["verdict"], "pass", "{report}");
    assert_eq!(report["ops"][0]["detail"]["import"]["format"], "tiff");
    let layers = report["ops"][1]["detail"]["layers"].as_array().unwrap();
    assert_eq!((layers.len(), layers[0]["kind"].as_str()), (1, Some("raster")));
}

/// Layer info (PSD "layer and mask information" layer-info payload, big-endian, raw channels) for
/// two RGB 8-bit layers on a 2x2 canvas; assembled here, independently of interop-psd.
/// name, rect (top, left, bottom, right), blend key, opacity, channels (id, raw samples).
type TestLayer = (&'static str, [i32; 4], &'static [u8; 4], u8, Vec<(i16, Vec<u8>)>);

fn two_layer_info() -> Vec<u8> {
    let layers: [TestLayer; 2] = [
        ("base", [0, 0, 2, 2], b"norm", 255, vec![(0, vec![10; 4]), (1, vec![20; 4]), (2, vec![30; 4])]),
        ("top", [0, 1, 2, 2], b"mul ", 128, vec![(-1, vec![128, 255]), (0, vec![200, 201]), (1, vec![100, 101]), (2, vec![0, 1])]),
    ];
    let mut info = (layers.len() as i16).to_be_bytes().to_vec();
    for (name, rect, key, opacity, channels) in &layers {
        for r in rect {
            info.extend(r.to_be_bytes());
        }
        info.extend((channels.len() as u16).to_be_bytes());
        for (id, data) in channels {
            info.extend(id.to_be_bytes());
            info.extend(((2 + data.len()) as u32).to_be_bytes());
        }
        info.extend_from_slice(b"8BIM");
        info.extend_from_slice(*key);
        info.extend([*opacity, 0, 0x08, 0]);
        let mut extra = 0u32.to_be_bytes().to_vec();
        extra.extend(0u32.to_be_bytes());
        let mut pascal = vec![name.len() as u8];
        pascal.extend_from_slice(name.as_bytes());
        while !pascal.len().is_multiple_of(4) {
            pascal.push(0);
        }
        extra.extend(pascal);
        info.extend((extra.len() as u32).to_be_bytes());
        info.extend(extra);
    }
    for (_, _, _, _, channels) in &layers {
        for (_, data) in channels {
            info.extend(0u16.to_be_bytes());
            info.extend_from_slice(data);
        }
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    info
}

/// 2x2 RGB 8-bit TIFF whose tag 37724 carries big-endian (`8BIM`) Photoshop layer records.
fn big_endian_layered_tiff(info: &[u8]) -> Vec<u8> {
    let mut isd = b"Adobe Photoshop Document Data Block\0".to_vec();
    isd.extend_from_slice(b"8BIMLayr");
    isd.extend((info.len() as u32).to_be_bytes());
    isd.extend_from_slice(info);
    while !isd.len().is_multiple_of(4) {
        isd.push(0);
    }
    let pixel_at = 8 + 2 + 12 * 11 + 4 + 6 + isd.len() + isd.len() % 2;
    tiff_le(&[
        (256, SHORT, 1, short(2)),
        (257, SHORT, 1, short(2)),
        (258, SHORT, 3, [8u16, 8, 8].iter().flat_map(|v| v.to_le_bytes()).collect()),
        (259, SHORT, 1, short(1)),
        (262, SHORT, 1, short(2)),
        (273, LONG, 1, (pixel_at as u32).to_le_bytes().to_vec()),
        (277, SHORT, 1, short(3)),
        (278, SHORT, 1, short(2)),
        (279, LONG, 1, 12u32.to_le_bytes().to_vec()),
        (284, SHORT, 1, short(1)),
        (37724, UNDEFINED, isd.len() as u32, isd),
    ])
    .into_iter()
    .chain([9u8; 12])
    .collect()
}

/// SMB-CONVERT-FOLLOWUPS slice 3: Photoshop layer records in tag 37724 import as layers through
/// interop-psd (`image_source_document` + `to_proposal`): the same records in a PSD give the same
/// layer names and tile digests. The composite page is a typed loss, the original keeps the bytes.
#[test]
fn photoshop_tiff_imports_its_layers_through_interop_psd() {
    let info = two_layer_info();
    let bytes = big_endian_layered_tiff(&info);
    let cancel = CancellationToken::default();
    let (package, imported) = convert_to_native(&bytes, &TrustAllResolver, &cancel).expect("converted");
    let receipt = &imported.receipt;
    assert_eq!(receipt["layers"], 2, "{receipt}");
    let isd = &receipt["tiff"]["image_source_data"];
    assert_eq!((isd["envelope"].as_str(), isd["byte_order"].as_str(), isd["layers"].as_str()), (Some("valid"), Some("big"), Some("imported")), "{isd}");
    assert_eq!(isd["layer_records"], 2);
    let found = codes(receipt);
    assert!(found.iter().any(|c| c == "tiff_composite_not_imported"), "{found:?}");
    assert!(!found.iter().any(|c| c == "tiff_photoshop_layers_not_imported" || c == "image_data_missing"), "{found:?}");
    let names: Vec<&str> = imported.snapshot.document().layers.iter().map(|l| l.name.as_str()).collect();

    // Reference: the same layer info inside a PSD file, through interop-psd's PSD reader.
    let mut psd_file = b"8BPS".to_vec();
    psd_file.extend(1u16.to_be_bytes());
    psd_file.extend([0u8; 6]);
    psd_file.extend(3u16.to_be_bytes());
    psd_file.extend(2u32.to_be_bytes());
    psd_file.extend(2u32.to_be_bytes());
    psd_file.extend(8u16.to_be_bytes());
    psd_file.extend(3u16.to_be_bytes());
    psd_file.extend(0u32.to_be_bytes());
    psd_file.extend(0u32.to_be_bytes());
    let mut section = (info.len() as u32).to_be_bytes().to_vec();
    section.extend_from_slice(&info);
    section.extend(0u32.to_be_bytes());
    psd_file.extend((section.len() as u32).to_be_bytes());
    psd_file.extend(section);
    psd_file.extend(0u16.to_be_bytes());
    psd_file.extend([9u8; 12]);
    let limits = psd::Limits::default();
    let doc = psd::read_psd(&psd_file, &limits, &cancel).expect("reference psd");
    let cfg = psd::ProposalConfig::new(&limits, folio::Budget::default(), &TrustAllResolver);
    let source = psd::SourceIdentity::of(&bytes);
    let reference = psd::to_proposal(&doc, &source, &cfg, &mut SourceMint::new(&source.sha256), None, &cancel).unwrap();
    let ref_names: Vec<&str> = reference.snapshot.document().layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ref_names);
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).unwrap();
    for tile in &reference.tiles {
        assert!(opened.assets.contains_key(&tile.content_sha256_hex), "tile {} not in the package", tile.content_sha256_hex);
    }
    assert_eq!(opened.import_bytes(ImportRole::Original), Some(&bytes[..]));

    // Open directly: two raster layers in a live session.
    let h = harness();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_import(&bytes));
    assert!(run.summary());
    let report = run.finish("import");
    assert_eq!(report["verdict"], "pass", "{report}");
    assert_eq!(report["ops"][1]["detail"]["layers"].as_array().map(Vec::len), Some(2), "{report}");

    // Records that do not parse fall back to the composite with the typed loss and the parse error.
    let mut broken = info.clone();
    broken.truncate(info.len() - 3);
    let fallback = import(&big_endian_layered_tiff(&broken), &TrustAllResolver, &cancel).expect("composite fallback");
    assert_eq!(fallback.receipt["layers"], 1);
    assert!(codes(&fallback.receipt).iter().any(|c| c == "tiff_photoshop_layers_not_imported"));
    assert!(fallback.receipt["tiff"]["image_source_data"]["parse_error"].as_str().is_some());

    // A real Photoshop-written layered TIFF (Pillow test image, Photoshop CS Windows, sha256-pinned in
    // oracle-corpus/tiff-layered): its records are little-endian (`MIB8ryaL`); interop-psd parses them
    // (68cba91, import-only, 8-bit only), so the 2 layers import and the composite page is the typed loss.
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: tiff_layered_photoshop reason=corpus_root_absent");
        return;
    };
    let name = "tiff-layered/pillow-tiff-adobe-deflate-photoshop-cs.tif";
    let Ok(real) = std::fs::read(root.join(name)) else {
        println!("NOT_PROVEN: tiff_layered_photoshop {name} reason=file_absent");
        return;
    };
    if hsk_studio_package::sha256_hex_of(&real) != "3ec989816b1532e587a9619980db33a925c0428d7538290bb6b4eb1b6a8efade" {
        println!("NOT_PROVEN: tiff_layered_photoshop {name} reason=sha256_mismatch");
        return;
    }
    let imported = import(&real, &TrustAllResolver, &cancel).expect("real layered TIFF imports its layers");
    let isd = &imported.receipt["tiff"]["image_source_data"];
    assert_eq!((isd["envelope"].as_str(), isd["byte_order"].as_str(), isd["layers"].as_str()), (Some("valid"), Some("little"), Some("imported")), "{isd}");
    assert_eq!(isd["layer_records"], 2, "{isd}");
    assert!(isd.get("parse_error").is_none(), "{isd}");
    assert_eq!(imported.receipt["layers"], 2);
    let found = codes(&imported.receipt);
    assert!(found.iter().any(|c| c == "tiff_composite_not_imported"), "{found:?}");
    assert!(!found.iter().any(|c| c == "tiff_photoshop_layers_not_imported"), "{found:?}");
    let names: Vec<&str> = imported.snapshot.document().layers.iter().map(|l| l.name.as_str()).collect();
    // Folio lists layers top-first (interop-psd to_proposal order); the records are bottom-first.
    assert_eq!(names, ["Layer 1", "Background"]);
    // The records the harness imported: Background (R,G,B) and Layer 1 (alpha,R,G,B), both 0,0,374,278.
    let page = &tiff::decode(&real, &tiff::Limits::default(), &cancel).expect("decodes").pages[0];
    let run = tiff::image_source_data(page.image_source_data.as_ref().expect("tag 37724")).expect("envelope");
    let mut tagged = b"MIB8".to_vec();
    tagged.extend_from_slice(&run.blocks);
    let canvas = psd::ImageSourceCanvas { width: page.width, height: page.height, depth: 8, colour_mode: psd::ColorMode::Rgb, icc: page.icc.clone() };
    let doc = psd::image_source_document(&tagged, canvas, &psd::Limits::default(), &cancel).expect("little-endian records parse");
    let records: Vec<(&str, Vec<i16>, [i32; 4])> =
        doc.layers().iter().map(|l| (l.name.as_str(), l.channels.iter().map(|c| c.id).collect(), [l.rect.top, l.rect.left, l.rect.bottom, l.rect.right])).collect();
    assert_eq!(records, [("Background", vec![0, 1, 2], [0, 0, 374, 278]), ("Layer 1", vec![-1, 0, 1, 2], [0, 0, 374, 278])]);
    println!("NOT_PROVEN: tiff_layered_photoshop_parity {name} reason=no_independent_photoshop_oracle (layers parsed from file bytes only)");
}

/// The one corpus-root convention (`.config/oracle-corpus.toml`): walk up from the workspace root.
fn corpus_root() -> Option<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = std::fs::read_to_string(workspace.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        config.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let mut at = workspace.canonicalize().ok()?;
    for _ in 0..=max {
        let candidate = at.join(&search);
        if candidate.is_dir() {
            return Some(candidate);
        }
        at = at.parent()?.to_path_buf();
    }
    None
}

#[test]
fn tiff_corpus_files_import_with_typed_losses_or_pass_through_refusals() {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: tiff_corpus reason=corpus_root_absent");
        return;
    };
    let cancel = CancellationToken::default();
    let files = [
        ("rgb-3c-16b.tiff", "31ff3a1f416cb7281acfbcbb4b56ee8bb94e9f91489602ff2806e5a49abc03c0"),
        ("seq__seq-1c-8b-multipage-adeefdcc.tiff", "f9dd5eb92e193f121b2afcb3575d2be5566adb6198afcb5009516a77972d6428"),
        ("seq__seq-4c-8b-rgba-50969cda.tiff", "b38f1c39e0c64e8c2aff66b33f11e1581dc452dd55abfb1e9a5831b4aa0b6f8e"),
        ("int16_zstd.tif", "7eb1cab565d023001237b88b0550c730311d6321d4f03fe0a725fc49576146c5"),
    ];
    let mut checked = 0;
    for (name, sha256) in files {
        let Ok(bytes) = std::fs::read(root.join("tiff").join(name)) else {
            println!("NOT_PROVEN: tiff_corpus {name} reason=file_absent");
            continue;
        };
        let digest = hsk_studio_package::sha256_hex_of(&bytes);
        if digest != sha256 {
            println!("NOT_PROVEN: tiff_corpus {name} reason=sha256_mismatch {digest}");
            continue;
        }
        let direct = tiff::decode(&bytes, &tiff::Limits::default(), &cancel);
        let result = import(&bytes, &TrustAllResolver, &cancel);
        match name {
            "int16_zstd.tif" => {
                // interop-tiff's typed refusal passes through unchanged.
                let want = direct.err().map(|e| e.code()).expect("interop-tiff refuses zstd");
                assert_eq!(result.err(), Some(ImportError::Importer { format: SourceFormat::Tiff, code: want }));
            }
            _ => {
                let file = direct.expect("interop-tiff decodes");
                let imported = result.unwrap_or_else(|e| panic!("{name}: {e}"));
                let receipt = &imported.receipt;
                let codes = codes(receipt);
                assert_eq!(receipt["layers"], 1, "{name}");
                assert_eq!(receipt["tiff"]["pages"], file.pages.len(), "{name}");
                let skipped = codes.iter().filter(|c| *c == "tiff_pages_not_imported").count();
                assert_eq!(skipped, file.pages.len() - 1, "{name}: one typed loss per page not imported");
                let page = &file.pages[0];
                let associated = page.alpha == tiff::Alpha::Associated;
                assert_eq!(codes.iter().any(|c| c == "tiff_associated_alpha_unpremultiplied"), associated, "{name}");
                match name {
                    "rgb-3c-16b.tiff" => assert_eq!(receipt["tiff"]["page"]["bits_per_sample"], 16),
                    "seq__seq-1c-8b-multipage-adeefdcc.tiff" => assert!(file.pages.len() > 1, "multipage fixture"),
                    _ => assert!(associated, "{name}: associated-alpha fixture"),
                }
                println!("TIFF {name} pages={} tiles={} losses={codes:?}", file.pages.len(), receipt["tiles"]);
            }
        }
        checked += 1;
    }
    println!("TIFF corpus checked={checked}/4");
}

/// SMB-CONVERT-FOLLOWUPS slice 2: a TIFF-based camera raw never imports as a plain TIFF. A hand-built
/// 2x2 RGB page carrying DNGVersion (50706), and the same page with a CFA photometric, are raws by the
/// container walk; corpus DNG, NEF and ARW files (sha256-pinned, `NOT_PROVEN:` when absent) are too,
/// and the CR2 is a raw by its magic. Without `import-raw` each is `format_unavailable` (format
/// `camera_raw`, feature `import-raw`); with `import-raw` develop decides and its result is never a
/// plain `tiff` import.
#[test]
fn camera_raw_never_imports_as_plain_tiff_without_import_raw() {
    let cancel = CancellationToken::default();
    let rgb = |photometric: u16, extra: Option<(u16, u16, u32, Vec<u8>)>| {
        let mut entries = vec![
            (256, SHORT, 1, short(2)),
            (257, SHORT, 1, short(2)),
            (258, SHORT, 3, [8u16, 8, 8].iter().flat_map(|v| v.to_le_bytes()).collect()),
            (259, SHORT, 1, short(1)),
            (262, SHORT, 1, short(photometric)),
            (273, LONG, 1, Vec::new()),
            (277, SHORT, 1, short(3)),
            (278, SHORT, 1, short(2)),
            (279, LONG, 1, 12u32.to_le_bytes().to_vec()),
        ];
        entries.extend(extra);
        // Pixel data follows the IFD and the 6-byte BitsPerSample value.
        let pixel_at = 8 + 2 + 12 * entries.len() + 4 + 6;
        entries[5].3 = (pixel_at as u32).to_le_bytes().to_vec();
        tiff_le(&entries).into_iter().chain([7u8; 12]).collect::<Vec<u8>>()
    };
    let plain = rgb(2, None);
    let plain_import = import(&plain, &TrustAllResolver, &cancel).expect("control: an unmarked page imports as plain TIFF");
    assert_eq!(plain_import.receipt["source_format"], "tiff");
    let dng = rgb(2, Some((50706, 1, 4, vec![1, 4, 0, 0])));
    let cfa = rgb(32803, None);
    let mut cases: Vec<(String, Vec<u8>)> = vec![("synthetic DNGVersion".into(), dng), ("synthetic CFA".into(), cfa)];
    match corpus_root() {
        None => println!("NOT_PROVEN: camera_raw_never_plain corpus reason=corpus_root_absent"),
        Some(root) => {
            for (name, sha256) in [
                ("dng/canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e"),
                ("raw-vendor/nikon-d1h-12bit-uncompressed.nef", "d110b80a8b10a3a67b5cd1c15ef33f1a6397557b73d5d3f4e565739f0c072571"),
                ("raw-vendor/sony-ilce-7s-14bit-compressed.arw", "a35ebb2fbec929daa5beb20d1ce5c15a8aac7b1a7a231455387f3df8a7442e07"),
                ("raw-vendor/canon-eos-350d-raw.cr2", "4b47ba27c0090efa091aaf99babc76d9a3e6f6f121b443d6c58c3ce7275cd770"),
            ] {
                match std::fs::read(root.join(name)) {
                    Ok(bytes) if hsk_studio_package::sha256_hex_of(&bytes) == sha256 => cases.push((name.into(), bytes)),
                    Ok(_) => println!("NOT_PROVEN: camera_raw_never_plain {name} reason=sha256_mismatch"),
                    Err(_) => println!("NOT_PROVEN: camera_raw_never_plain {name} reason=file_absent"),
                }
            }
        }
    }
    for (name, bytes) in &cases {
        let result = import(bytes, &TrustAllResolver, &cancel);
        if cfg!(feature = "import-raw") {
            match &result {
                Ok(imported) => assert_ne!(imported.receipt["source_format"], "tiff", "{name}: imported as plain TIFF"),
                Err(e) => assert!(matches!(e, ImportError::Importer { .. } | ImportError::FormatUnavailable { .. }), "{name}: {e:?}"),
            }
        } else {
            assert_eq!(
                result.err(),
                Some(ImportError::FormatUnavailable { format: SourceFormat::CameraRaw, feature: "import-raw", reason: "feature_not_enabled" }),
                "{name}"
            );
        }
        // Evidence of the failure class: whether interop-tiff alone would have decoded a first page.
        let ifd0_decodes = tiff::decode(bytes, &tiff::Limits::default(), &cancel).is_ok();
        println!("CAMERA_RAW {name}: never plain TIFF (interop-tiff alone decodes a page: {ifd0_decodes})");
    }
}

struct TrustAllResolver;
impl folio::Resolver for TrustAllResolver {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

// ---- Lane SMB-TIFF-CHANNELS-1 (R52 F-R52-2, TIFF part; STU-RAS-022, STU-RAS-043, STU-IO-006 row 3) ----
// TIFF extra samples beyond the image's own alpha import as Folio channel records (tiles stored as
// package assets, as PSD channels are), and `export_tiff_with_channels` writes channel records back as
// extra sample planes; the record fields TIFF cannot carry are typed losses. The channel "saved from a
// selection" is assembled here as Pigment's save rule stores it (tone = full - coverage, selected-area
// alpha, 256-pixel grid): Pigment is not a dependency of this feature unit. How Photoshop marks alpha
// channels in a TIFF (ExtraSamples value 0) is UNVERIFIED (D5).

use hsk_studio_harness::import::{TiffChannelExport, export_tiff_with_channels};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Depth {
    D8,
    D16,
    D32,
}

impl Depth {
    fn bits(self) -> u8 {
        match self {
            Depth::D8 => 8,
            Depth::D16 => 16,
            Depth::D32 => 32,
        }
    }
    fn per(self) -> usize {
        usize::from(self.bits() / 8)
    }
    fn format(self) -> &'static str {
        match self {
            Depth::D8 => "coverage_u8",
            Depth::D16 => "coverage_u16le",
            Depth::D32 => "coverage_f32le",
        }
    }
    /// `n` samples from a 0..=65535 ramp `f`, at this depth (floats in 0..=1).
    fn samples(self, n: usize, f: impl Fn(usize) -> u32) -> tiff::Samples {
        match self {
            Depth::D8 => tiff::Samples::U8((0..n).map(|i| (f(i) >> 8) as u8).collect()),
            Depth::D16 => tiff::Samples::U16((0..n).map(|i| f(i) as u16).collect()),
            Depth::D32 => tiff::Samples::F32((0..n).map(|i| f(i) as f32 / 65535.0).collect()),
        }
    }
}

/// Samples as little-endian bytes (the channel tile sample layout).
fn le_bytes(s: &tiff::Samples) -> Vec<u8> {
    match s {
        tiff::Samples::U8(v) => v.clone(),
        tiff::Samples::U16(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        tiff::Samples::F32(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        other => panic!("no channel layout for {other:?}"),
    }
}

/// A plane of `w x h` little-endian samples cut into 256-pixel cells (zero padding), as
/// (column, row, bytes).
fn cells(plane: &[u8], w: usize, h: usize, per: usize) -> Vec<(i64, i64, Vec<u8>)> {
    let mut out = Vec::new();
    for row in 0..h.div_ceil(256) {
        for column in 0..w.div_ceil(256) {
            let mut cell = vec![0u8; 256 * 256 * per];
            for y in row * 256..h.min(row * 256 + 256) {
                for x in column * 256..w.min(column * 256 + 256) {
                    let (src, dst) = ((y * w + x) * per, ((y - row * 256) * 256 + (x - column * 256)) * per);
                    cell[dst..dst + per].copy_from_slice(&plane[src..src + per]);
                }
            }
            out.push((column as i64, row as i64, cell));
        }
    }
    out
}

/// The plane a channel record's tiles hold (little-endian samples), read back from `assets`.
fn record_plane(c: &folio::StudioChannelRecord, assets: &BTreeMap<String, Vec<u8>>, per: usize) -> Vec<u8> {
    let (w, h) = (c.width as usize, c.height as usize);
    let mut plane = vec![0u8; w * h * per];
    for t in &c.tiles {
        let bytes = assets.get(&t.content_digest.digest).unwrap_or_else(|| panic!("tile {} not stored", t.content_digest.digest));
        let (x0, y0) = (t.column as usize * 256, t.row as usize * 256);
        for y in y0..h.min(y0 + 256) {
            for x in x0..w.min(x0 + 256) {
                let (dst, src) = ((y * w + x) * per, ((y - y0) * 256 + (x - x0)) * per);
                plane[dst..dst + per].copy_from_slice(&bytes[src..src + per]);
            }
        }
    }
    plane
}

/// Converts to a native package and reads it back: the package assets, the raster records and the
/// import receipt.
fn converted(bytes: &[u8]) -> (BTreeMap<String, Vec<u8>>, folio::StudioRasterRecords, Value, Vec<String>) {
    let cancel = CancellationToken::default();
    let (package, imported) = convert_to_native(bytes, &TrustAllResolver, &cancel).expect("converted");
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).expect("package reads");
    let records: folio::StudioRasterRecords = serde_json::from_slice(opened.import_bytes(ImportRole::Records).expect("records entry")).expect("records parse");
    let layer_tiles = imported
        .snapshot
        .document()
        .layers
        .iter()
        .flat_map(|l| match &l.payload {
            folio::Payload::Raster { tiles } => tiles.iter().map(|t| t.content_digest.digest.clone()).collect(),
            _ => Vec::new(),
        })
        .collect();
    (opened.assets, records, imported.receipt, layer_tiles)
}

fn loss_triples(export: &TiffChannelExport) -> Vec<(String, psd::LossKind, &'static str)> {
    export.loss.iter().map(|e| (e.path.clone(), e.kind, e.code)).collect()
}

/// What an interchange round trip keeps of a channel's pixels: depth, extent and tiles by grid cell,
/// format and content digest.
type ChannelPixels = (u8, u32, u32, Vec<(i64, i64, String, String)>);
fn pixels_of(c: &folio::StudioChannelRecord) -> ChannelPixels {
    (c.depth, c.width, c.height, c.tiles.iter().map(|t| (t.column, t.row, t.format.clone(), t.content_digest.digest.clone())).collect())
}

/// R52 F-R52-2 (TIFF): a document with an alpha channel (a TIFF extra sample beyond the image's own
/// alpha) and a channel saved from a selection exports to TIFF and re-imports with both channel
/// records and identical pixels, at 8, 16 and 32 (float) bits; the fields TIFF cannot carry are typed
/// losses of the export and the import.
#[test]
fn tiff_alpha_channel_and_saved_selection_round_trip_as_channel_records() {
    let (w, h) = (300u32, 3u32);
    let n = (w * h) as usize;
    for depth in [Depth::D8, Depth::D16, Depth::D32] {
        let label = format!("{depth:?}");
        // Source: RGBA (unassociated image alpha) plus one unspecified extra sample, the alpha channel.
        let rgba = depth.samples(n * 4, |i| ((i * 2654 + 17) % 65536) as u32);
        let alpha_channel = depth.samples(n, |i| ((i % w as usize) * 218 + (i / w as usize) * 9001) as u32 % 65536);
        let base = tiff::EncodeRequest {
            width: w,
            height: h,
            color: tiff::ColorModel::Rgba,
            alpha: tiff::Alpha::Unassociated,
            bit_depth: depth.bits(),
            samples: &rgba,
            icc: None,
            compression: tiff::Compression::Deflate,
            predictor: if depth == Depth::D32 { tiff::Predictor::None } else { tiff::Predictor::Horizontal },
            big_tiff: false,
            resolution: None,
            orientation: None,
        };
        let source = tiff::encode_with_planes(&base, &[tiff::ExtraPlane { extra_sample: 0, samples: alpha_channel.clone() }], &tiff::EncodeOptions::default())
            .expect("source TIFF");

        // Import: the extra sample is a channel record; its tiles are package assets with the exact samples.
        let (assets, records, receipt, layer_tiles) = converted(&source);
        let first = codes(&receipt);
        assert!(!first.iter().any(|c| c == "tiff_extra_samples_not_imported"), "{label}: {first:?}");
        assert!(first.iter().any(|c| c == "tiff_channel_display_defaulted"), "{label}: {first:?}");
        assert_eq!(records.channels.len(), 1, "{label}");
        let alpha = &records.channels[0];
        assert_eq!(
            (alpha.channel_id.as_str(), alpha.name.as_str(), alpha.kind, alpha.visible, alpha.opacity, alpha.depth, alpha.width, alpha.height),
            ("tiff-channel-4", "Alpha 1", folio::StudioChannelKind::MaskedAreaAlpha, false, 50.0, depth.bits(), w, h),
            "{label}"
        );
        assert_eq!(alpha.colour, folio::StudioChannelColour::Rgb { r: 1.0, g: 0.0, b: 0.0 }, "{label}");
        assert_eq!(alpha.tiles.len(), 2, "{label}: 300 x 3 on the 256 grid");
        assert!(alpha.tiles.iter().all(|t| t.format == depth.format()), "{label}");
        assert_eq!(record_plane(alpha, &assets, depth.per()), le_bytes(&alpha_channel), "{label}: exact samples");

        // A channel saved from a selection (selected-area alpha: tone = full - coverage).
        let coverage: Vec<u32> = (0..n).map(|i| ((i * 40503 + 7) % 65536) as u32).collect();
        let tone = depth.samples(n, |i| 65535 - coverage[i]);
        let mut saved_tiles = BTreeMap::new();
        let profile = alpha.tiles[0].colour_profile_id.clone();
        let tiles: Vec<Value> = cells(&le_bytes(&tone), w as usize, h as usize, depth.per())
            .into_iter()
            .map(|(column, row, bytes)| {
                let digest = hsk_studio_package::sha256_hex_of(&bytes);
                saved_tiles.insert(digest.clone(), bytes);
                serde_json::json!({
                    "object_key": format!("ch_{column}_{row}"), "layer_id": "saved", "column": column, "row": row,
                    "width": {"value": 256.0, "unit": "px"}, "height": {"value": 256.0, "unit": "px"},
                    "format": depth.format(), "colour_profile_id": profile, "schema_id": "hsk.studio.raster_tile@1",
                    "content_digest": {"algorithm": "sha256", "digest": digest},
                    "artifact_manifest_id": format!("edited/saved/ch_{column}_{row}"),
                })
            })
            .collect();
        let saved: folio::StudioChannelRecord = serde_json::from_value(serde_json::json!({
            "channel_id": "saved", "name": "Saved selection", "kind": "selected_area_alpha", "visible": false,
            "colour": {"model": "rgb", "r": 0.0, "g": 0.0, "b": 1.0}, "opacity": 40.0, "depth": depth.bits(), "width": w, "height": h,
            "tiles": tiles,
        }))
        .unwrap_or_else(|e| panic!("{label}: saved record {e}"));
        let mut edited = records.clone();
        edited.channels.push(saved.clone());

        // Export: the image (colour + its alpha) as before, both channel records as extra planes.
        let mut lookup = |digest: &str| assets.get(digest).or_else(|| saved_tiles.get(digest)).cloned();
        let export = export_tiff_with_channels(&base, &edited.channels, &mut lookup, &tiff::EncodeOptions::default())
            .unwrap_or_else(|e| panic!("{label}: export {e:?}"));
        let mut want = Vec::new();
        for i in 0..2 {
            want.push((format!("channel[{i}]/name"), psd::LossKind::Unsupported, "tiff_channel_name_not_exported"));
            want.push((format!("channel[{i}]/kind"), psd::LossKind::Transformed, "tiff_channel_kind_not_exported"));
            want.push((format!("channel[{i}]/colour"), psd::LossKind::Unsupported, "tiff_channel_colour_not_exported"));
            want.push((format!("channel[{i}]/opacity"), psd::LossKind::Unsupported, "tiff_channel_opacity_not_exported"));
        }
        assert_eq!(loss_triples(&export), want, "{label}");
        // The file itself: the image samples, the alpha kind first, then one unspecified plane per channel.
        let page = tiff::decode(&export.bytes, &tiff::Limits::default(), &CancellationToken::default()).unwrap().pages.remove(0);
        assert_eq!(page.extra_samples, [2, 0, 0], "{label}");
        let planes = page.extra_planes();
        assert_eq!(planes[1].samples, alpha_channel, "{label}");
        assert_eq!(planes[2].samples, tone, "{label}");

        // Re-import: both channel records with identical pixels, and the layer pixels unchanged.
        let (assets2, records2, receipt2, layer_tiles2) = converted(&export.bytes);
        assert_eq!(layer_tiles2, layer_tiles, "{label}: layer tiles");
        let ids: Vec<&str> = records2.channels.iter().map(|c| c.channel_id.as_str()).collect();
        assert_eq!(ids, ["tiff-channel-4", "tiff-channel-5"], "{label}");
        assert_eq!(pixels_of(&records2.channels[0]), pixels_of(alpha), "{label}: the alpha channel");
        assert_eq!(pixels_of(&records2.channels[1]), pixels_of(&saved), "{label}: the saved channel");
        assert_eq!(record_plane(&records2.channels[1], &assets2, depth.per()), le_bytes(&tone), "{label}");
        // What TIFF did not carry comes back as the import defaults, typed on import.
        let back = &records2.channels[1];
        assert_eq!((back.name.as_str(), back.kind, back.opacity), ("Alpha 2", folio::StudioChannelKind::MaskedAreaAlpha, 50.0), "{label}");
        let defaulted = codes(&receipt2).iter().filter(|c| *c == "tiff_channel_display_defaulted").count();
        assert_eq!(defaulted, 2, "{label}");
        assert_eq!(receipt2["tiff"]["channels"].as_array().map(Vec::len), Some(2), "{label}: {receipt2}");
    }
}

/// Hostile and out-of-envelope cases are bounded and typed (V4-01/02): more extra samples than Folio
/// holds channels (dims-style: the rest are typed losses), channel tile bytes over the decode budget
/// before any tile is allocated (aggregate_over_budget-style: a 1 x 262144 page whose 56 channels
/// would need 3.5 GiB of 256-pixel cells), and export refusals and losses for missing or altered tile
/// bytes, a hostile tile edge, a depth or extent the image does not have and more channels than TIFF
/// takes.
#[test]
fn tiff_channel_mapping_is_bounded_and_export_refuses_what_it_cannot_write() {
    let cancel = CancellationToken::default();
    let opts = tiff::EncodeOptions::default();
    let rgb = tiff::Samples::U8(vec![40; 12]);
    let base = tiff::EncodeRequest {
        width: 2,
        height: 2,
        color: tiff::ColorModel::Rgb,
        alpha: tiff::Alpha::None,
        bit_depth: 8,
        samples: &rgb,
        icc: None,
        compression: tiff::Compression::None,
        predictor: tiff::Predictor::None,
        big_tiff: false,
        resolution: None,
        orientation: None,
    };
    let planes = |k: usize| -> Vec<tiff::ExtraPlane> {
        (0..k).map(|i| tiff::ExtraPlane { extra_sample: 0, samples: tiff::Samples::U8(vec![i as u8; 4]) }).collect()
    };
    // 60 extra samples: Folio holds 56 channels; the other four are typed losses.
    let source = tiff::encode_with_planes(&base, &planes(60), &opts).unwrap();
    let (assets, records, receipt, _) = converted(&source);
    assert_eq!(records.channels.len(), folio::MAX_CHANNELS);
    let lost: Vec<String> = receipt["loss"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["code"] == "tiff_extra_samples_not_imported")
        .map(|e| e["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(lost, (56..60).map(|k| format!("page[0]/extra_sample[{k}]")).collect::<Vec<_>>());

    // Channel cells over the decode budget: refused before any channel tile is allocated.
    let tall = tiff::Samples::U8(vec![0; 3 * 262_144]);
    let tall_req = tiff::EncodeRequest { width: 1, height: 262_144, samples: &tall, compression: tiff::Compression::Deflate, ..base };
    let tall_planes: Vec<tiff::ExtraPlane> =
        (0..56).map(|_| tiff::ExtraPlane { extra_sample: 0, samples: tiff::Samples::U8(vec![0; 262_144]) }).collect();
    let tall_file = tiff::encode_with_planes(&tall_req, &tall_planes, &opts).unwrap();
    assert_eq!(import(&tall_file, &TrustAllResolver, &cancel).err(), Some(ImportError::Importer { format: SourceFormat::Tiff, code: "tiff_channel_tiles_over_budget" }));

    // Export refusals: a tile the host cannot supply, altered tile bytes, a hostile tile edge.
    let one = records.channels[..1].to_vec();
    let mut none = |_: &str| None;
    assert_eq!(export_tiff_with_channels(&base, &one, &mut none, &opts).unwrap_err(), tiff::TiffError::Usage("channel_tile_missing"));
    let mut altered = |d: &str| assets.get(d).map(|b| b.iter().map(|x| x ^ 1).collect());
    assert_eq!(export_tiff_with_channels(&base, &one, &mut altered, &opts).unwrap_err(), tiff::TiffError::Usage("channel_tile_digest"));
    let mut lookup = |d: &str| assets.get(d).cloned();
    for edge in [0.0, 0.5, 1e12, f64::NAN] {
        let mut hostile = one.clone();
        hostile[0].tiles[0].width.value = edge;
        assert_eq!(export_tiff_with_channels(&base, &hostile, &mut lookup, &opts).unwrap_err(), tiff::TiffError::Usage("channel_tile_edge"), "{edge}");
    }
    // A depth or extent other than the image's: typed losses, the image is written without them.
    let mut deep = one.clone();
    deep[0].depth = 16;
    let mut wide = one.clone();
    wide[0].width = 3;
    for (records, code) in [(deep, "tiff_channel_depth_not_exported"), (wide, "tiff_channel_extent_not_exported")] {
        let export = export_tiff_with_channels(&base, &records, &mut lookup, &opts).unwrap();
        assert_eq!(loss_triples(&export), [("channel[0]".to_string(), psd::LossKind::Unsupported, code)]);
        let page = tiff::decode(&export.bytes, &tiff::Limits::default(), &cancel).unwrap().pages.remove(0);
        assert_eq!(page.samples_per_pixel, 3, "{code}");
    }
    // 62 channels on RGB: 61 fit the 64 samples per pixel TIFF is written with; the last is typed.
    let many: Vec<folio::StudioChannelRecord> = (0..62)
        .map(|k| {
            let mut c = records.channels[0].clone();
            c.channel_id = format!("c{k}");
            c
        })
        .collect();
    let export = export_tiff_with_channels(&base, &many, &mut lookup, &opts).unwrap();
    let count_losses: Vec<(String, psd::LossKind, &'static str)> =
        loss_triples(&export).into_iter().filter(|(_, _, c)| *c == "tiff_channel_count_not_exported").collect();
    assert_eq!(count_losses, [("channel[61]".to_string(), psd::LossKind::Unsupported, "tiff_channel_count_not_exported")]);
    let page = tiff::decode(&export.bytes, &tiff::Limits::default(), &cancel).unwrap().pages.remove(0);
    assert_eq!(page.samples_per_pixel, 64);
}
