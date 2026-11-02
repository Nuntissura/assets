//! Lane SMB-R53-FOLLOWUPS-1, STUDIO-R53 F-R53-2: the typed outcomes of `export_tiff_with_channels`
//! (lane SMB-TIFF-CHANNELS-1) that had no test. Two export losses (a spot channel's solidity, a
//! visible channel's visibility), each with the complete file written; three tile errors (another
//! size, another format, off the grid), each refusing with nothing written. One defect per fixture,
//! so each test fails when exactly its own check is removed (red proof: the logged temporary
//! mutation in the lane file). Oracle: the export's own documented codes and interop-tiff's decoder.
#![cfg(feature = "import-tiff")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness_core::import::export_tiff_with_channels;
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_tiff as tiff;
use hsk_studio_prism::{hash_hex, profile_hash};
use serde_json::json;
use std::collections::BTreeMap;

/// The channel's tone: four 8-bit samples of the 2 x 2 image.
const TONE: [u8; 4] = [10, 80, 160, 250];

fn digest(bytes: &[u8]) -> String {
    hash_hex(&profile_hash(bytes))
}

/// A 2 x 2 8-bit channel record with one 2 x 2 tile at (`column`, `row`) of `format` holding `bytes`.
fn channel(kind: &str, visible: bool, format: &str, column: i64, row: i64, bytes: &[u8]) -> folio::StudioChannelRecord {
    serde_json::from_value(json!({
        "channel_id": "ch", "name": "Tile test channel", "kind": kind, "visible": visible,
        "colour": {"model": "rgb", "r": 0.0, "g": 0.0, "b": 1.0}, "opacity": 40.0, "depth": 8, "width": 2, "height": 2,
        "tiles": [{
            "object_key": "ch_0_0", "layer_id": "ch", "column": column, "row": row,
            "width": {"value": 2.0, "unit": "px"}, "height": {"value": 2.0, "unit": "px"},
            "format": format, "colour_profile_id": "SCPF-019abcde-0000-7000-8000-000000000001", "schema_id": "hsk.studio.raster_tile@1",
            "content_digest": {"algorithm": "sha256", "digest": digest(bytes)},
            "artifact_manifest_id": "edited/ch/ch_0_0",
        }],
    }))
    .expect("channel record")
}

/// Exports `records` over a 2 x 2 RGB image, serving every tile from `store`.
fn export(records: &[folio::StudioChannelRecord], store: &[&[u8]]) -> tiff::Result<hsk_studio_harness_core::import::TiffChannelExport> {
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
    let tiles: BTreeMap<String, Vec<u8>> = store.iter().map(|b| (digest(b), b.to_vec())).collect();
    let mut lookup = |d: &str| tiles.get(d).cloned();
    export_tiff_with_channels(&base, records, &mut lookup, &tiff::EncodeOptions::default())
}

fn losses(e: &hsk_studio_harness_core::import::TiffChannelExport) -> Vec<(String, psd::LossKind, &'static str)> {
    e.loss.iter().map(|l| (l.path.clone(), l.kind, l.code)).collect()
}

/// The written file is complete: the image's three samples plus the channel's plane, exactly.
fn assert_complete(e: &hsk_studio_harness_core::import::TiffChannelExport) {
    let page = tiff::decode(&e.bytes, &tiff::Limits::default(), &CancellationToken::default()).expect("written file decodes").pages.remove(0);
    assert_eq!(page.samples_per_pixel, 4);
    assert_eq!(page.extra_samples, [0]);
    assert_eq!(page.extra_planes()[0].samples, tiff::Samples::U8(TONE.to_vec()));
}

/// Expected losses of one written channel; `opacity` and `visible` vary.
fn written(opacity: &'static str, visible: bool) -> Vec<(String, psd::LossKind, &'static str)> {
    let mut want = vec![
        ("channel[0]/name".to_string(), psd::LossKind::Unsupported, "tiff_channel_name_not_exported"),
        ("channel[0]/kind".to_string(), psd::LossKind::Transformed, "tiff_channel_kind_not_exported"),
        ("channel[0]/colour".to_string(), psd::LossKind::Unsupported, "tiff_channel_colour_not_exported"),
        ("channel[0]/opacity".to_string(), psd::LossKind::Unsupported, opacity),
    ];
    if visible {
        want.push(("channel[0]/visible".to_string(), psd::LossKind::Unsupported, "tiff_channel_visibility_not_exported"));
    }
    want
}

#[test]
fn spot_channel_solidity_is_a_typed_loss_and_the_file_is_complete() {
    let spot = channel("spot_colour", false, "coverage_u8", 0, 0, &TONE);
    let e = export(&[spot], &[&TONE]).expect("spot channel exports");
    assert_eq!(losses(&e), written("tiff_channel_solidity_not_exported", false));
    assert_complete(&e);
    // The same channel as an alpha kind records opacity, not solidity.
    let alpha = channel("masked_area_alpha", false, "coverage_u8", 0, 0, &TONE);
    assert_eq!(losses(&export(&[alpha], &[&TONE]).unwrap()), written("tiff_channel_opacity_not_exported", false));
}

#[test]
fn visible_channel_visibility_is_a_typed_loss_and_the_file_is_complete() {
    let shown = channel("masked_area_alpha", true, "coverage_u8", 0, 0, &TONE);
    let e = export(&[shown], &[&TONE]).expect("visible channel exports");
    assert_eq!(losses(&e), written("tiff_channel_opacity_not_exported", true));
    assert_complete(&e);
    // A hidden channel has no visibility loss.
    let hidden = channel("masked_area_alpha", false, "coverage_u8", 0, 0, &TONE);
    assert!(!losses(&export(&[hidden], &[&TONE]).unwrap()).iter().any(|l| l.2 == "tiff_channel_visibility_not_exported"));
}

#[test]
fn channel_tile_of_another_size_is_refused_with_nothing_written() {
    // Digest and format match; only the byte count differs from 2 x 2 x 1.
    let longer = [10u8, 80, 160, 250, 7];
    assert_eq!(export(&[channel("masked_area_alpha", false, "coverage_u8", 0, 0, &longer)], &[&longer]).unwrap_err(), tiff::TiffError::Usage("channel_tile_size"));
    let shorter = [10u8, 80, 160];
    assert_eq!(export(&[channel("masked_area_alpha", false, "coverage_u8", 0, 0, &shorter)], &[&shorter]).unwrap_err(), tiff::TiffError::Usage("channel_tile_size"));
}

#[test]
fn channel_tile_of_another_format_is_refused_with_nothing_written() {
    // An 8-bit channel whose tile claims 16-bit samples; size and digest fit the 8-bit layout.
    let tile = channel("masked_area_alpha", false, "coverage_u16le", 0, 0, &TONE);
    assert_eq!(export(&[tile], &[&TONE]).unwrap_err(), tiff::TiffError::Usage("channel_tile_format"));
}

#[test]
fn channel_tile_off_the_grid_is_refused_with_nothing_written() {
    for (column, row) in [(-1, 0), (0, -1), (i64::MIN, 0)] {
        let tile = channel("masked_area_alpha", false, "coverage_u8", column, row, &TONE);
        assert_eq!(export(&[tile], &[&TONE]).unwrap_err(), tiff::TiffError::Usage("channel_tile_grid"), "({column}, {row})");
    }
}
