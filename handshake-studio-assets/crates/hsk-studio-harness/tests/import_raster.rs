//! SMB-CONVERT-RASTER focused tests (feature `import-raster`): PNG / JPEG -> interop-raster decode ->
//! interop-psd `raster_proposal` -> native package with receipt, typed losses and the original.
//! Sources are written by interop-raster's own encoders (PNG; the clean-room JPEG encoder) and the Exif
//! segment is assembled by hand.
#![cfg(feature = "import-raster")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::{ImportError, SourceFormat, convert_to_native, import, sniff};
use hsk_studio_harness::{Harness, References, Selection, default_actor};
use hsk_studio_interop_raster as raster;
use hsk_studio_package::{ImportRole, Limits, read_package};
use serde_json::Value;

fn codes(receipt: &Value) -> Vec<String> {
    receipt["loss"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap().to_string()).collect()
}

#[test]
fn png_imports_as_one_raster_layer_with_receipt_and_original() {
    let samples = raster::Samples::U8((0..4 * 3 * 4).map(|i| (i * 13 % 256) as u8).collect());
    let req = raster::PngEncodeRequest {
        width: 4,
        height: 3,
        colour: raster::Colour::Rgba,
        samples: &samples,
        icc: None,
        compression: raster::PngCompression::Balanced,
    };
    let bytes = raster::encode_png(&req).unwrap();
    assert_eq!(sniff(&bytes), Some(SourceFormat::Png));
    let cancel = CancellationToken::default();
    let (package, imported) = convert_to_native(&bytes, &TrustAllResolver, &cancel).expect("converted");
    let receipt = &imported.receipt;
    assert_eq!((receipt["source_format"].as_str(), receipt["layers"].as_u64()), (Some("png"), Some(1)), "{receipt}");
    assert_eq!(receipt["raster"]["colour"], "Rgba");
    assert_eq!(receipt["raster"]["png"]["source_colour_type"], 6);
    assert!(codes(receipt).iter().any(|c| c == "profile_assumed_srgb"), "{receipt}");
    assert!(!codes(receipt).iter().any(|c| c == "raster_orientation_not_applied"));
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).unwrap();
    assert_eq!(opened.import_bytes(ImportRole::Original), Some(&bytes[..]));
    let stored: Value = serde_json::from_slice(opened.import_bytes(ImportRole::Receipt).unwrap()).unwrap();
    assert_eq!(&stored, receipt);

    // Open directly: one raster layer in a live session.
    let h = Harness::new(Selection { providers: Vec::new(), references: References::TrustAll }).unwrap();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_import(&bytes));
    assert!(run.summary());
    let report = run.finish("import");
    assert_eq!(report["verdict"], "pass", "{report}");
    let layers = report["ops"][1]["detail"]["layers"].as_array().unwrap();
    assert_eq!((layers.len(), layers[0]["kind"].as_str()), (1, Some("raster")));

    // Recognised but refused by the codec owner: WebP; corrupt PNG passes interop-raster's code through.
    let webp = b"RIFF\x10\0\0\0WEBPVP8 \0\0\0\0";
    assert_eq!(sniff(webp), Some(SourceFormat::Webp));
    assert_eq!(import(webp, &TrustAllResolver, &cancel).err(), Some(ImportError::Importer { format: SourceFormat::Webp, code: "webp_decoder_unavailable" }));
    assert_eq!(import(&bytes[..bytes.len() - 12], &TrustAllResolver, &cancel).err().map(|e| e.code()), Some("truncated"));
}

/// Big-endian Exif APP1 payload with one IFD0 entry: Orientation (0x0112, SHORT) = `value`.
fn exif_app1(value: u16) -> Vec<u8> {
    let mut tiff = b"MM\0\x2a\0\0\0\x08".to_vec();
    tiff.extend(1u16.to_be_bytes());
    tiff.extend([0x01, 0x12, 0x00, 0x03, 0, 0, 0, 1]);
    tiff.extend(value.to_be_bytes());
    tiff.extend([0, 0, 0, 0, 0, 0]);
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(tiff);
    let mut seg = vec![0xFF, 0xE1];
    seg.extend(((payload.len() + 2) as u16).to_be_bytes());
    seg.extend(payload);
    seg
}

#[test]
fn jpeg_with_exif_orientation_6_records_raster_orientation_not_applied() {
    let samples = raster::Samples::U8((0..16 * 8 * 3).map(|i| (i * 7 % 256) as u8).collect());
    let req = raster::JpegEncodeRequest {
        width: 16,
        height: 8,
        colour: raster::Colour::Rgb,
        samples: &samples,
        quality: 90,
        subsampling: raster::JpegSubsampling::S420,
        icc: None,
    };
    let plain = raster::encode_jpeg(&req).unwrap();
    let mut bytes = plain[..2].to_vec();
    bytes.extend(exif_app1(6));
    bytes.extend(&plain[2..]);
    assert_eq!(sniff(&bytes), Some(SourceFormat::Jpeg));
    let cancel = CancellationToken::default();
    let (package, imported) = convert_to_native(&bytes, &TrustAllResolver, &cancel).expect("converted");
    let receipt = &imported.receipt;
    assert_eq!(receipt["source_format"], "jpeg", "{receipt}");
    assert_eq!(receipt["raster"]["exif_orientation"], 6);
    assert_eq!(receipt["raster"]["jpeg"]["process"], "baseline");
    assert_eq!((receipt["raster"]["width"].as_u64(), receipt["raster"]["height"].as_u64()), (Some(16), Some(8)), "not rotated");
    let codes = codes(receipt);
    assert!(codes.iter().any(|c| c == "raster_orientation_not_applied"), "{codes:?}");
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).unwrap();
    assert_eq!(opened.import_bytes(ImportRole::Original), Some(&bytes[..]));
    // Orientation 1 is the identity: no loss.
    let mut upright = plain[..2].to_vec();
    upright.extend(exif_app1(1));
    upright.extend(&plain[2..]);
    let (_, imported) = convert_to_native(&upright, &TrustAllResolver, &cancel).unwrap();
    assert!(!codes_has(&imported.receipt, "raster_orientation_not_applied"));
}

fn codes_has(receipt: &Value, code: &str) -> bool {
    codes(receipt).iter().any(|c| c == code)
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
