//! The export parameter dictionary (lane SMB-PR-EXPORT-RECIPE-1 items 2 and 3; STU-VID-061, 061a, 061b,
//! STU-IO-102..106): the const table equals, cell for cell, the spec's facet tables kept verbatim in
//! tests/fixtures/stu-vid-061b-rows.txt (Master Spec v02.229 module 14 lines 24952..25501, SHA-256 pinned).
//! Nothing is read from the spec at build or test time.
use hsk_studio_reel::export_parameters::{CODEC_SELECTION, CREDENTIAL_OUTSIDE_PUBLISHING, DEGENERATE_BOUNDS, EXPORT_PARAMETER_COUNT};
use hsk_studio_reel::*;
use sha2::Digest;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("fixtures/stu-vid-061b-rows.txt");
const FIXTURE_SHA256: &str = "c19e4b92f419d768904ce66fc4336dc914b2cbb3457978608cbb9292fff89ab3";

fn type_code(text: &str) -> ExportTypeCode {
    match text {
        "container / group header (no value of its own)" => ExportTypeCode::GroupHeader,
        "signed integer" => ExportTypeCode::SignedInteger,
        "floating point" => ExportTypeCode::FloatingPoint,
        "boolean" => ExportTypeCode::Boolean,
        "string" => ExportTypeCode::String,
        "enumerated integer chosen from a constrained list" => ExportTypeCode::EnumeratedInteger,
        "button / action (no stored value)" => ExportTypeCode::Action,
        "tab or section group" => ExportTypeCode::TabGroup,
        "file or folder path string" => ExportTypeCode::Path,
        "opaque / arbitrary data" => ExportTypeCode::Opaque,
        "multi-instance group (repeating rows)" => ExportTypeCode::RepeatingGroup,
        "colour" => ExportTypeCode::Colour,
        other => panic!("type column {other:?}"),
    }
}

fn facet(token: &str) -> ExportFacet {
    *ExportFacet::ALL.iter().find(|f| f.token() == token).unwrap_or_else(|| panic!("facet {token:?}"))
}

struct FixtureRow {
    facet: ExportFacet,
    cells: Vec<String>,
}

/// Every table row of the fixture with the facet of the heading above it.
fn fixture_rows() -> Vec<FixtureRow> {
    let text = FIXTURE.replace("\r\n", "\n");
    let mut current = None;
    let mut rows = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("**facet `") {
            current = Some(facet(rest.split('`').next().unwrap()));
        } else if line.starts_with("| `") {
            let cells: Vec<String> = line.trim().trim_matches('|').split('|').map(|c| c.trim().to_owned()).collect();
            assert_eq!(cells.len(), 13, "{line}");
            rows.push(FixtureRow { facet: current.expect("a facet heading first"), cells });
        }
    }
    rows
}

#[test]
fn fixture_is_the_v02_229_facet_tables() {
    let text = FIXTURE.replace("\r\n", "\n");
    let digest = hex(&sha2::Sha256::digest(text.as_bytes()));
    assert_eq!((digest.as_str(), text.len()), (FIXTURE_SHA256, 58_934));
}

/// Every cell of every fixture row equals the const table (items 2 and 3).
#[test]
fn every_row_and_cell_equals_the_spec_table() {
    let rows = fixture_rows();
    assert_eq!(rows.len(), EXPORT_PARAMETER_COUNT);
    assert_eq!(export_parameters().len(), EXPORT_PARAMETER_COUNT);
    for r in &rows {
        let c = &r.cells;
        let id = c[0].trim_matches('`');
        let p = export_parameter(id).unwrap_or_else(|| panic!("{id} missing"));
        let label = (c[1] != "--").then_some(c[1].as_str());
        assert_eq!(p.identifier, id);
        assert_eq!(p.label, label, "{id} label");
        assert_eq!(p.label_source, if label.is_some() { LabelSource::Recovered } else { LabelSource::IdentifierFallback }, "{id}");
        assert_eq!(p.type_code, type_code(&c[2]), "{id} type");
        assert_eq!(p.facet, r.facet, "{id} facet");
        let declared = [p.hard_min, p.hard_max, p.soft_min, p.soft_max, p.default].map(|v| v.to_string());
        assert_eq!(declared.as_slice(), &c[3..8], "{id} bounds and default");
        assert_eq!(c[8], "--", "{id} unit cell");
        assert_eq!(p.unit, ExportUnit::Undeclared, "{id} unit");
        assert_eq!(p.precision.map_or("--".to_owned(), |v| v.to_string()), c[9], "{id} precision");
        assert_eq!(if p.is_slider { "yes" } else { "no" }, c[10], "{id} slider");
        assert_eq!(p.used_by_preset_count.to_string(), c[11], "{id} presets");
        assert_eq!(p.used_by_formats.join(", "), c[12], "{id} formats");
        let bound = if p.hard_min.is_absent() && p.hard_max.is_absent() { BoundState::UnboundedInSource } else { BoundState::DeclaredHardOnly };
        assert_eq!(p.bound_state, bound, "{id} bound_state");
    }
}

#[test]
fn counts_coverage_and_lookup() {
    let all = export_parameters();
    let mut by_facet: BTreeMap<&str, usize> = BTreeMap::new();
    for p in all {
        *by_facet.entry(p.facet.token()).or_default() += 1;
    }
    let want: BTreeMap<&str, usize> = [
        ("other", 178),
        ("publishing_destination", 86),
        ("video_codec", 41),
        ("audio", 36),
        ("rate_control", 34),
        ("colour", 22),
        ("layout", 16),
        ("gop_structure", 13),
        ("multiplexing", 10),
        ("video_frame", 5),
        ("vr_immersive", 5),
        ("captions", 4),
        ("metadata", 2),
        ("performance", 1),
    ]
    .into_iter()
    .collect();
    assert_eq!(by_facet, want);
    let mut by_type: BTreeMap<u8, usize> = BTreeMap::new();
    for p in all {
        *by_type.entry(p.type_code.code()).or_default() += 1;
    }
    let types: BTreeMap<u8, usize> = [(1, 71), (2, 226), (3, 23), (4, 3), (5, 1), (6, 24), (7, 18), (8, 72), (9, 11), (10, 1), (11, 2), (12, 1)].into_iter().collect();
    assert_eq!(by_type, types);
    // Sorted and distinct: lookup is a binary search.
    assert!(all.windows(2).all(|w| w[0].identifier.as_bytes() < w[1].identifier.as_bytes()));
    assert_eq!(label_coverage(), (453, 44, 409));
    assert!(all.iter().all(|p| p.soft_min.is_absent() && p.soft_max.is_absent() && p.default.is_absent() && p.unit == ExportUnit::Undeclared));
    // Bound shapes as measured from the table.
    let none = all.iter().filter(|p| p.hard_min.is_absent() && p.hard_max.is_absent()).count();
    let max_only: Vec<&str> = all.iter().filter(|p| p.hard_min.is_absent() && !p.hard_max.is_absent()).map(|p| p.identifier).collect();
    assert_eq!(none, 365);
    assert_eq!(max_only.len(), 8, "{max_only:?}");
    assert!(all.iter().all(|p| p.hard_max.is_absent() <= p.hard_min.is_absent()), "no min-only row");
    assert_eq!(all.iter().filter(|p| p.precision == Some(2)).count(), 209);
    assert_eq!(all.iter().filter(|p| p.is_slider).count(), 37);
    // Unlabelled rows display their identifier (STU-IO-106).
    let unlabelled = export_parameter("ADBEVideoFPS").unwrap();
    assert_eq!((unlabelled.display_label(), unlabelled.label_source.token()), ("ADBEVideoFPS", "identifier_fallback"));
    // STU-IO-144's worked example.
    let mode = export_parameter("ADBEAACAudioMode").unwrap();
    assert_eq!((mode.hard_min, mode.hard_max, mode.precision), (DeclaredValue::Integer(0), DeclaredValue::Integer(3), Some(2)));
    // Bare numbers are real identifiers; unknown and over-long ones are not found.
    for id in ["0", "13", "51"] {
        assert!(export_parameter(id).is_some(), "{id}");
    }
    assert!(export_parameter("ADBEVideoWidthX").is_none() && export_parameter(&"A".repeat(4096)).is_none());
}

/// Traps of the brief, stored exactly as declared.
#[test]
fn declared_cells_are_kept_as_declared() {
    let p = |id| *export_parameter(id).unwrap();
    for id in ["ADBEVideoCodec", "ADBEAudioCodec"] {
        assert_eq!((p(id).hard_min, p(id).hard_max), (DeclaredValue::Integer(0), DeclaredValue::Integer(-1)), "{id}");
    }
    let fps = p("ADBEVideoFPS");
    assert_eq!((fps.type_code, fps.hard_min, fps.hard_max), (ExportTypeCode::Boolean, DeclaredValue::Integer(1), DeclaredValue::Integer(254_016_000_000)));
    assert_eq!(p("ADBEVideoAspect").hard_min, DeclaredValue::Text("0,0"));
    assert_eq!((p("ADBEAudioNumChannels").hard_min, p("ADBEAudioNumChannels").hard_max), (DeclaredValue::Integer(-51), DeclaredValue::Integer(32)));
    assert_eq!(p("ADBEFTPPassword").type_code, ExportTypeCode::EnumeratedInteger);
    assert_eq!(p("ADBEStockRefreshToken").type_code, ExportTypeCode::Path);
    assert_eq!(p("ADBEAudioCodec").used_by_preset_count, 1701, "above 1,541: no upper bound asserted");
    assert_eq!(p("ADBEVideoTargetBitrate").label, Some("The target data rate allowed by the encoder."));
    assert_eq!((p("ADBEVideoTargetBitrate").hard_min, p("ADBEVideoTargetBitrate").hard_max), (DeclaredValue::Decimal(0.19), DeclaredValue::Integer(240)));
    // The degenerate list: exactly the signed-integer rows whose minimum is not below their maximum.
    let mut degenerate: Vec<&str> = export_parameters()
        .iter()
        .filter(|r| r.type_code == ExportTypeCode::SignedInteger)
        .filter(|r| matches!((r.hard_min, r.hard_max), (DeclaredValue::Integer(a), DeclaredValue::Integer(b)) if a >= b))
        .map(|r| r.identifier)
        .collect();
    degenerate.sort_unstable();
    assert_eq!(degenerate, DEGENERATE_BOUNDS);
    assert!(DEGENERATE_BOUNDS.iter().all(|id| export_parameter(id).unwrap().usable_hard_bounds() == (None, None)));
    // Credential rows outside the publishing facet, and the codec-selection rule.
    assert!(CREDENTIAL_OUTSIDE_PUBLISHING.iter().all(|id| export_parameter(id).unwrap().facet != ExportFacet::PublishingDestination));
    let mut codecs: Vec<&str> = export_parameters()
        .iter()
        .filter(|r| r.type_code == ExportTypeCode::SignedInteger && matches!(r.label, Some("Video Codec" | "Audio Codec")))
        .map(|r| r.identifier)
        .collect();
    codecs.sort_unstable();
    assert_eq!(codecs, CODEC_SELECTION);
    // Closed enumerations.
    assert_eq!(ExportTypeCode::ALL.map(|t| t.code()), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    assert_eq!(BoundState::ALL.map(|b| b.token()), ["declared_both", "declared_hard_only", "declared_soft_only", "unbounded_in_source", "observed_only"]);
}
