//! SMB-CONVERT-TIFF focused tests (feature `import-tiff`): plain TIFF -> interop-tiff decode ->
//! interop-psd `raster_proposal` -> native package with receipt, typed losses and the original.
//! The first test assembles its TIFF bytes by hand (no encoder); the corpus test reads 4 files of
//! `oracle-corpus/tiff` through the checked-in `.config/oracle-corpus.toml`, each pinned by
//! SHA-256, and prints `NOT_PROVEN:` when the corpus is absent (V4-03).
#![cfg(feature = "import-tiff")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::{ImportError, SourceFormat, SourceMint, convert_to_native, import, sniff};
use hsk_studio_harness::{Harness, References, Selection, default_actor};
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
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_import(&bytes));
    assert!(run.summary());
    let report = run.finish("import");
    assert_eq!(report["verdict"], "pass", "{report}");
    assert_eq!(report["ops"][0]["detail"]["import"]["format"], "tiff");
    let layers = report["ops"][1]["detail"]["layers"].as_array().unwrap();
    assert_eq!((layers.len(), layers[0]["kind"].as_str()), (1, Some("raster")));
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
