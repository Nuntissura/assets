//! SMB-CONVERT-COLOUR focused tests: an embedded ICC profile is converted by Prism (interop-psd
//! `PrismConverter`) into the linear-light sRGB working space on import; a missing or unusable
//! profile is a typed loss entry, never silent. Inputs are TIFFs written in the test by
//! interop-tiff's encoder; the sRGB v4 source profile is the CC0 Prism fixture checked in beside
//! interop-psd's tests.
#![cfg(feature = "import-tiff")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::{ImportedDocument, convert_to_native};
use hsk_studio_interop_psd::srgb_to_linear;
use hsk_studio_interop_tiff as tiff;
use hsk_studio_package::{ImportRole, Limits, read_package};
use serde_json::Value;

const SRGB_V4: &[u8] = include_bytes!("../../hsk-studio-interop-psd/tests/fixtures/icc/sRGB-v4.icc");

fn encode(color: tiff::ColorModel, samples: Vec<u8>, width: u32, icc: Option<&[u8]>) -> Vec<u8> {
    let samples = tiff::Samples::U8(samples);
    tiff::encode(&tiff::EncodeRequest {
        width,
        height: 1,
        color,
        alpha: tiff::Alpha::None,
        bit_depth: 8,
        samples: &samples,
        icc,
        compression: tiff::Compression::None,
        predictor: tiff::Predictor::None,
        big_tiff: false,
        resolution: None,
        orientation: None,
    })
    .expect("fixture encodes")
}

/// Converts and reopens; returns the receipt, the package's tile assets and the original bytes.
fn convert(bytes: &[u8]) -> (Value, Vec<Vec<u8>>, Vec<u8>) {
    let cancel = CancellationToken::default();
    let (package, imported): (Vec<u8>, ImportedDocument) = convert_to_native(bytes, &TrustAllResolver, &cancel).expect("converted");
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).unwrap();
    let stored: Value = serde_json::from_slice(opened.import_bytes(ImportRole::Receipt).unwrap()).unwrap();
    assert_eq!(stored, imported.receipt, "the receipt entry in the package is the import receipt");
    let original = opened.import_bytes(ImportRole::Original).unwrap().to_vec();
    (imported.receipt, opened.assets.values().cloned().collect(), original)
}

fn first_floats(tile: &[u8], n: usize) -> Vec<f32> {
    tile.chunks_exact(4).take(n).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn loss_codes(receipt: &Value) -> Vec<String> {
    receipt["loss"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap().to_string()).collect()
}

#[test]
fn embedded_profiles_convert_through_prism_into_the_working_space() {
    let pixels = vec![200u8, 100, 50, 10, 128, 250];
    let tagged = encode(tiff::ColorModel::Rgb, pixels.clone(), 2, Some(SRGB_V4));
    let (receipt, tiles, original) = convert(&tagged);
    let colour = &receipt["colour"];
    assert_eq!(colour["status"], "converted", "{colour}");
    assert_eq!(colour["icc_converted"], true);
    assert_eq!(colour["assumed_profile"], false);
    assert_eq!(colour["embedded_profile_sha256"], hsk_studio_package::sha256_hex_of(SRGB_V4));
    assert!(colour["prism_frames"].as_u64().unwrap() > 0, "Prism ran: {colour}");
    assert!(!loss_codes(&receipt).iter().any(|c| c == "profile_assumed_srgb"));
    // The original profile bytes stay in the original entry.
    assert_eq!(original, tagged);
    assert!(original.windows(SRGB_V4.len()).any(|w| w == SRGB_V4));

    // sRGB v4 -> linear sRGB through Prism equals the IEC 61966-2-1 decode the importer uses for an
    // untagged 8-bit source (same primaries; parametric curve vs closed form, within 2e-3).
    assert_eq!(tiles.len(), 1);
    let converted = first_floats(&tiles[0], 6);
    let expected: Vec<f32> = pixels.iter().map(|&v| srgb_to_linear(f32::from(v) / 255.0)).collect();
    for (got, want) in converted.iter().zip(&expected) {
        assert!((got - want).abs() < 2e-3, "converted {converted:?} vs closed form {expected:?}");
    }
    // The untagged twin takes the recorded assumption instead (typed, never silent).
    let (untagged, _, _) = convert(&encode(tiff::ColorModel::Rgb, pixels, 2, None));
    assert_eq!(untagged["colour"]["status"], "assumed");
    assert!(loss_codes(&untagged).iter().any(|c| c == "profile_assumed_srgb"));
    println!("COLOUR converted={converted:?} transform={}", colour["transform"]);
}

#[test]
fn missing_or_unusable_profiles_are_typed_losses() {
    // A profile Prism cannot parse: the import still maps (sRGB assumption) and the refusal is a
    // loss entry on document/profile carrying Prism's code, reported in the colour summary.
    let junk = vec![0xABu8; 200];
    let (receipt, _, original) = convert(&encode(tiff::ColorModel::Rgb, vec![1, 2, 3], 1, Some(&junk)));
    let colour = &receipt["colour"];
    assert_ne!(colour["status"], "converted", "{colour}");
    assert_eq!(colour["icc_converted"], false);
    assert_eq!(colour["embedded_profile_sha256"], hsk_studio_package::sha256_hex_of(&junk));
    let profile_losses = colour["losses"].as_array().unwrap();
    assert!(profile_losses.iter().any(|e| e["path"] == "document/profile"), "{colour}");
    assert!(original.windows(junk.len()).any(|w| w == &junk[..]), "profile bytes kept in the original");

    // CMYK without a profile: no default profile is invented; typed unmapped layer + loss.
    let (cmyk, _, _) = convert(&encode(tiff::ColorModel::Cmyk, vec![0, 64, 128, 255], 1, None));
    assert_eq!(cmyk["colour"]["status"], "unmapped_or_not_applicable", "{}", cmyk["colour"]);
    assert!(loss_codes(&cmyk).iter().any(|c| c == "cmyk_profile_missing"), "{:?}", loss_codes(&cmyk));
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
