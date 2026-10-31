//! Stock PDF export presets (lane SMB-PDF-STOCK-PRESETS-1, STU-LAY-159, STU-LAY-168, MT-30841)
//! through the public API: the six records against an independent copy of the 19-row table, the
//! common defaults, per-value provenance, the compliance token apart from the name, the contract
//! additions, the PDF/X refusals and one assertion per open spec gap.
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_press::pdf_stock_presets as stock;
use hsk_studio_press::*;

const IDS: [&str; 6] = [
    "hsk.studio.pdf_preset.high_quality_print",
    "hsk.studio.pdf_preset.press_quality",
    "hsk.studio.pdf_preset.smallest_file_size",
    "hsk.studio.pdf_preset.pdf_x_1a_2001",
    "hsk.studio.pdf_preset.pdf_x_3_2002",
    "hsk.studio.pdf_preset.pdf_x_4",
];
const NAMES: [&str; 6] = ["High Quality Print", "Press Quality", "Smallest File Size", "PDF/X-1a:2001", "PDF/X-3:2002", "PDF/X-4"];

/// Independent oracle: STU-LAY-159 as the spec prints it (columns in spec order), each cell in
/// the setting's token form.
const TABLE: [(&str, &str, [&str; 6]); 19] = [
    ("PDF version", "general.compatibility_level", ["1.4", "1.4", "1.5", "1.3", "1.3", "1.4"]),
    (
        "Colour conversion",
        "colour_management.colour_conversion_strategy",
        ["LeaveColorUnchanged", "CMYK", "sRGB", "CMYK", "UseDeviceIndependentColor", "UseDeviceIndependentColor"],
    ),
    ("Colour image resolution", "colour_images.target_resolution", ["300", "300", "100", "300", "300", "300"]),
    ("Colour downsample threshold", "colour_images.downsample_threshold", ["1.5"; 6]),
    ("Greyscale resolution", "greyscale_images.target_resolution", ["300", "300", "150", "300", "300", "300"]),
    ("Monochrome resolution", "monochrome_images.target_resolution", ["1200", "1200", "300", "1200", "1200", "1200"]),
    ("Colour image filter", "colour_images.filter", ["DCTEncode"; 6]),
    ("Auto-filter colour images", "colour_images.auto_filter", ["true"; 6]),
    ("Embed all fonts", "fonts.embed_all_fonts", ["true", "true", "false", "true", "true", "true"]),
    ("Subset fonts", "fonts.subset_fonts", ["true"; 6]),
    ("Max subset percent", "fonts.max_subset_percent", ["100"; 6]),
    ("Optimize", "general.optimisation", ["true", "true", "true", "false", "false", "false"]),
    ("Compress objects", "general.compress_objects", ["Tags", "Tags", "All", "Off", "Off", "Off"]),
    ("Auto-rotate pages", "general.auto_rotate_pages", ["All", "None", "All", "None", "None", "None"]),
    ("Allow transparency", "general.allow_transparency", ["false", "false", "false", "false", "false", "true"]),
    ("Cannot-embed-font policy", "fonts.cannot_embed_font_policy", ["Warning", "Error", "Warning", "Error", "Error", "Error"]),
    ("Preserve overprint", "colour_management.preserve_overprint", ["true"; 6]),
    ("Compliance check", "standards_compliance.compliance_check", ["none", "none", "none", "PDF/X-1a:2001", "PDF/X-3:2002", "PDF/X-4:2007"]),
    ("PDF/X compliant only", "standards_compliance.standard_compliant_only", ["false", "false", "false", "true", "true", "true"]),
];
const PROFILE: &str = "Studio test output condition";

fn cell(v: &SettingValue) -> String {
    match v {
        SettingValue::Bool(b) => b.to_string(),
        SettingValue::Integer(n) => n.to_string(),
        SettingValue::Real(x) => format!("{x}"),
        SettingValue::Enum(e) => e.token().to_owned(),
        other => panic!("not a table cell: {other:?}"),
    }
}
fn writable() -> [&'static StockPreset; 3] {
    [&STOCK_PRESETS[0], &STOCK_PRESETS[1], &STOCK_PRESETS[2]]
}
/// An operator preset carrying every stock value of `p`, with `overrides` applied.
fn operator_from(p: &StockPreset, overrides: &[(&str, SettingValue)]) -> Result<PdfPreset, PresetError> {
    let values: Vec<(String, SettingValue)> = p
        .values()
        .into_iter()
        .map(|v| {
            let key = v.setting.key();
            let value = overrides.iter().find(|(k, _)| *k == key).map_or(v.value, |(_, o)| o.clone());
            (key, value)
        })
        .collect();
    let head = PresetDraft {
        id: "operator.from-stock",
        name: "From stock",
        descriptions: &[("en", "operator copy")],
        default_locale: "en",
        source: SourceInput::Operator { parent: Some(p.id) },
        settings: &[],
    };
    PdfPreset::from_values(&head, &values)
}

/// MT-30841 AC-001/AC-002: six logical presets with stable ids and the spec's names; every one of
/// the 19 rows equals the independent table for all six; the three writable presets build and
/// hold the same values.
#[test]
fn pdf_stock_six_preset_defaults() {
    assert_eq!(stock::logical_count(), 6);
    assert_eq!(stock::STOCK_LOGICAL_COUNT, 6);
    assert_eq!(stock::STOCK_LOCALE_VARIANTS, 7, "a preset count and a preset FILE count are different numbers");
    let ids: Vec<&str> = stock_presets().iter().map(|p| p.id).collect();
    let names: Vec<&str> = stock_presets().iter().map(|p| p.name).collect();
    assert_eq!((ids.as_slice(), names.as_slice()), (&IDS[..], &NAMES[..]));
    let rows: Vec<(&str, &str)> = STOCK_ROWS.iter().map(|r| (r.row, r.setting)).collect();
    let oracle: Vec<(&str, &str)> = TABLE.iter().map(|(r, k, _)| (*r, *k)).collect();
    assert_eq!(rows, oracle, "19 rows in table order");
    for (i, p) in stock_presets().iter().enumerate() {
        assert_eq!(stock_preset(IDS[i]), Some(p));
        let prov = p.provenance();
        for (row, key, cells) in TABLE {
            let v = p.value(key).unwrap_or_else(|| panic!("{key}"));
            assert_eq!(cell(&v), cells[i], "{} / {row}", p.name);
            assert_eq!(prov.source(key), Some((StockValueSource::Captured { row }, false)), "{} / {row}", p.name);
        }
    }
    for p in writable() {
        let preset = p.to_preset().unwrap();
        assert_eq!((preset.id(), preset.name(), preset.source()), (p.id, p.name, &PresetSource::Shipped));
        for (row, key, _) in TABLE {
            assert_eq!(preset.value(key), p.value(key).as_ref(), "{} / {row}", p.name);
        }
        assert!(preset.warnings().is_empty(), "{}: no stock value lies outside a soft bound", p.name);
        assert_eq!(preset.descriptions().render("de_DE").locale, "en");
    }
    assert_eq!(stock_preset("hsk.studio.pdf_preset.HIGH_QUALITY_PRINT"), None, "ids match exactly");
}

/// MT-30841 AC-004: the defaults common to all six.
#[test]
fn stock_common_defaults() {
    assert_eq!(stock::STOCK_JPEG2000, Jpeg2000Defaults { tile_width: 256, tile_height: 256, quality: 30 });
    let acs = SettingValue::Quality(vec![
        QualityEntry { key: "QFactor".to_owned(), value: QualityValue::Number(0.15) },
        QualityEntry { key: "HSamples".to_owned(), value: QualityValue::Numbers(vec![1.0; 4]) },
        QualityEntry { key: "VSamples".to_owned(), value: QualityValue::Numbers(vec![1.0; 4]) },
    ]);
    let common = Some((StockValueSource::CommonDefault, false));
    for p in stock_presets() {
        let prov = p.provenance();
        for key in ["colour_images.auto_colour_space_quality_dictionary", "greyscale_images.auto_colour_space_quality_dictionary"] {
            assert_eq!(p.value(key), Some(acs.clone()), "{} {key}", p.name);
            assert_eq!(prov.source(key), common);
        }
        for (key, value) in [
            ("monochrome_images.filter", SettingValue::Enum(EnumValue::ImageFilter(ImageFilter::CcittFaxEncode))),
            ("general.image_memory", SettingValue::Integer(1_048_576)),
            ("general.start_page", SettingValue::Integer(1)),
            ("general.end_page", SettingValue::Integer(-1)),
        ] {
            assert_eq!(p.value(key), Some(value), "{} {key}", p.name);
            assert_eq!(prov.source(key), common, "{} {key}", p.name);
        }
        let jp: Vec<(&str, u32, &str)> = prov.not_representable.iter().map(|n| (n.name, n.value, n.code)).collect();
        assert_eq!(
            jp,
            [("jpeg2000_tile_width", 256, "not_representable"), ("jpeg2000_tile_height", 256, "not_representable"), ("jpeg2000_quality", 30, "not_representable")]
        );
    }
    for p in writable() {
        assert_eq!(p.to_preset().unwrap().page_range(), (1, None), "{}: the end page is nullable", p.name);
    }
}

/// STU-IO-125: every value the spec does not give is studio_declared per value; numeric ones with
/// a STU-IO-129 default column take that default.
#[test]
fn stock_provenance_receipt() {
    for p in stock_presets() {
        let prov = stock_provenance(p.id).unwrap();
        assert_eq!(prov.preset_id, p.id);
        assert_eq!(prov.entries.len(), SETTINGS.len());
        assert!(prov.entries.iter().zip(SETTINGS).all(|((r, _, _), s)| *r == s.setting()));
        let count = |f: &dyn Fn(StockValueSource) -> bool| prov.entries.iter().filter(|(_, s, _)| f(*s)).count();
        assert_eq!(count(&|s| matches!(s, StockValueSource::Captured { .. })), 19);
        assert_eq!(count(&|s| s == StockValueSource::CommonDefault), 6);
        assert_eq!(count(&|s| s == StockValueSource::StudioDeclared), SETTINGS.len() - 25);
        let from_default: Vec<String> = prov.entries.iter().filter(|(_, _, d)| *d).map(|(r, _, _)| r.key()).collect();
        assert_eq!(
            from_default,
            [
                "colour_images.bit_depth",
                "greyscale_images.bit_depth",
                "greyscale_images.downsample_threshold",
                "monochrome_images.bit_depth",
                "monochrome_images.downsample_threshold",
                "postscript_and_dsc.dsc_reporting_level"
            ]
        );
        for v in p.values().into_iter().filter(|v| v.default_column) {
            assert_eq!(v.source, StockValueSource::StudioDeclared);
            let spec = SETTINGS.iter().find(|s| s.setting() == v.setting).unwrap();
            let contract = match spec.kind {
                SettingKind::Integer(c) | SettingKind::Real(c) => c,
                _ => panic!("default column on a non-numeric setting"),
            };
            let n = match v.value {
                SettingValue::Integer(n) => n as f64,
                SettingValue::Real(x) => x,
                _ => panic!(),
            };
            assert_eq!(Bound::Value(n), contract.default, "{}", v.setting);
        }
        assert_eq!(prov.source("output_intent.output_intent_profile"), Some((StockValueSource::StudioDeclared, false)));
        assert_eq!(prov.description, StockValueSource::StudioDeclared);
        assert_eq!(prov.gaps, &StockGap::ALL[..]);
    }
    assert_eq!(StockValueSource::Captured { row: "Optimize" }.token(), "captured");
    assert_eq!(StockValueSource::CommonDefault.token(), "common_default");
    assert_eq!(StockValueSource::StudioDeclared.token(), "studio_declared");
    assert_eq!(stock_provenance("hsk.studio.pdf_preset.unknown"), None);
}

/// MT-30841 AC-003: the compliance token is its own field; renaming changes nothing about it and
/// nothing parses one from the other.
#[test]
fn compliance_token_is_not_the_name() {
    let x4 = stock_preset(stock::PDF_X_4).unwrap();
    assert_eq!((x4.name, x4.compliance.token()), ("PDF/X-4", "PDF/X-4:2007"));
    assert_eq!(ComplianceCheck::from_token(x4.name), None, "the name is not a compliance token");
    for p in stock_presets() {
        let mut renamed = *p;
        renamed.name = "Renamed by the operator";
        assert_eq!(renamed.compliance, p.compliance);
        assert_eq!(renamed.value("standards_compliance.compliance_check"), p.value("standards_compliance.compliance_check"));
    }
    // A record named like another standard keeps its own compliance field.
    let mut swapped = *stock_preset(stock::PDF_X_1A_2001).unwrap();
    swapped.name = "PDF/X-3:2002";
    assert_eq!(swapped.compliance, ComplianceCheck::PdfX1a2001);
    // An operator copy of a writable preset with a new name keeps compliance_check.
    let pq = stock_preset(stock::PRESS_QUALITY).unwrap().to_preset().unwrap();
    let copy = pq.edit_copy("operator.pdf-x-4", "PDF/X-4").unwrap();
    assert_eq!(copy.name(), "PDF/X-4");
    assert_eq!(copy.standards().compliance_check, ComplianceCheck::None);
    // An operator preset made from the X-4 record keeps PDF/X-4:2007 under any name.
    let op = operator_from(x4, &[("output_intent.output_intent_profile", SettingValue::Text(PROFILE.to_owned()))]).unwrap();
    let op = op.edit_copy("operator.renamed", "Anything").unwrap();
    assert_eq!(op.standards().compliance_check, ComplianceCheck::PdfX4_2007);
}

/// The press contract additions STU-LAY-159 needs.
#[test]
fn stock_contract_additions() {
    let hq = stock_preset(stock::HIGH_QUALITY_PRINT).unwrap().to_preset().unwrap();
    let mut op = hq.edit_copy("operator.hq", "HQ").unwrap();
    // CCITT is admitted on the monochrome filter only.
    for key in ["colour_images.filter", "greyscale_images.filter"] {
        let e = op.set(key, SettingInput::Token("CCITTFaxEncode")).unwrap_err();
        assert_eq!(e.code(), "filter_not_admitted_for_group", "{key}");
        assert_eq!(e.setting().unwrap().key(), key);
        assert_eq!(preset_outcome_for::<()>(&Err(e)), Outcome::Failure(FailureCode::Validation));
    }
    assert_eq!(op.set("monochrome_images.filter", SettingInput::Token("FlateEncode")), Ok(None));
    assert_eq!(op.set("monochrome_images.filter", SettingInput::Token("CCITTFaxEncode")), Ok(None));
    // allow_transparency and preserve_overprint: typed, group studio_declared, never applied.
    for key in ["general.allow_transparency", "colour_management.preserve_overprint"] {
        let spec = SETTINGS.iter().find(|s| s.setting().key() == key).unwrap();
        assert_eq!((spec.kind, spec.group_basis), (SettingKind::Bool, Basis::StudioDeclared));
    }
    let receipt = |p: &PdfPreset| p.resolve(ResolveRequest::default()).unwrap().receipt;
    for v in [true, false] {
        op.set("general.allow_transparency", SettingInput::Bool(v)).unwrap();
        op.set("colour_management.preserve_overprint", SettingInput::Bool(v)).unwrap();
        let rec = receipt(&op);
        assert_eq!(rec.status("general.allow_transparency"), Some(SettingStatus::NotApplied { code: "transparency_flattening_not_implemented" }));
        assert_eq!(rec.status("colour_management.preserve_overprint"), Some(SettingStatus::NotApplied { code: "overprint_not_implemented" }));
    }
    assert_eq!(receipt(&op).status("standards_compliance.compliance_check"), Some(SettingStatus::Applied));
    assert_eq!(receipt(&op).status("monochrome_images.filter"), Some(SettingStatus::NotApplied { code: "recompression_not_implemented" }));
    // compliance_check asserts a standard: the STU-IO-130 output-intent rule applies.
    let cc = "standards_compliance.compliance_check";
    assert_eq!(op.set(cc, SettingInput::Token("PDF/X-3:2002")).unwrap_err(), PresetError::StandardWithoutOutputIntent);
    assert_eq!(op.standards().compliance_check, ComplianceCheck::None, "a refused edit leaves the preset unchanged");
    op.set("output_intent.output_intent_profile", SettingInput::Text(PROFILE)).unwrap();
    op.set(cc, SettingInput::Token("PDF/X-3:2002")).unwrap();
    assert!(op.standards().asserts_standard());
    let rec = receipt(&op);
    assert_eq!(rec.status(cc), Some(SettingStatus::NotApplied { code: "standard_check_not_implemented" }));
    // SD-19 covers the compliance check: compliant-only with a standard the writer does not check.
    op.set("standards_compliance.standard_compliant_only", SettingInput::Bool(true)).unwrap();
    assert_eq!(op.resolve(ResolveRequest::default()).unwrap_err(), PresetError::StandardComplianceNotProvable);
    // SD-13: PDF/X-4:2007 needs 1.6+, no member of the level domain reaches it.
    op.set("standards_compliance.standard_compliant_only", SettingInput::Bool(false)).unwrap();
    op.set(cc, SettingInput::Token("PDF/X-4:2007")).unwrap();
    for level in ["1.4", "1.5"] {
        op.set("general.compatibility_level", SettingInput::Token(level)).unwrap();
        assert_eq!(op.resolve(ResolveRequest::default()).unwrap_err(), PresetError::StandardLevelConflict(op.compatibility_level()));
    }
    // Exact tokens only.
    for bad in ["PDF/X-4", "pdf/x-4:2007", "None", " none"] {
        assert_eq!(op.set(cc, SettingInput::Token(bad)).unwrap_err().code(), "unknown_member", "{bad:?}");
    }
    assert_eq!(EnumDomain::ComplianceCheck.completeness(), DomainCompleteness::Partial);
    assert_eq!(EnumDomain::ComplianceCheck.setting(), "compliance_check");
}

/// SD-15 / STU-LAY-159: the end page is nullable through the API; -1 stays inside the setting.
#[test]
fn page_range_end_is_nullable() {
    let hq = stock_preset(stock::HIGH_QUALITY_PRINT).unwrap().to_preset().unwrap();
    assert_eq!(hq.page_range(), (1, None));
    assert_eq!(hq.value("general.end_page"), Some(&SettingValue::Integer(-1)));
    let mut op = hq.edit_copy("operator.range", "Range").unwrap();
    op.set("general.start_page", SettingInput::Integer(3)).unwrap();
    op.set("general.end_page", SettingInput::Integer(5)).unwrap();
    assert_eq!(op.page_range(), (3, Some(5)));
    op.set("general.end_page", SettingInput::Integer(0)).unwrap();
    assert_eq!(op.page_range(), (3, Some(0)));
    // No known hard maximum: a page number beyond u32 reads as u32::MAX, nothing wraps.
    op.set("general.start_page", SettingInput::Integer(i64::MAX)).unwrap();
    op.set("general.end_page", SettingInput::Integer(i64::from(u32::MAX) + 1)).unwrap();
    assert_eq!(op.page_range(), (u32::MAX, Some(u32::MAX)));
    assert_eq!(op.set("general.end_page", SettingInput::Integer(-2)).unwrap_err().code(), "outside_hard_bounds");
}

/// SD-18 / SD-13: the PDF/X presets are not built (no output-intent profile was captured), and
/// with a profile supplied they still refuse at resolution, before any export.
#[test]
fn stock_pdfx_presets_refuse() {
    let expected = [
        (stock::PDF_X_1A_2001, PresetError::CompatibilityLevelNotWritable(CompatibilityLevel::V1_3)),
        (stock::PDF_X_3_2002, PresetError::CompatibilityLevelNotWritable(CompatibilityLevel::V1_3)),
        (stock::PDF_X_4, PresetError::StandardLevelConflict(CompatibilityLevel::V1_4)),
    ];
    for (id, at_resolution) in expected {
        let p = stock_preset(id).unwrap();
        let e = p.to_preset().unwrap_err();
        assert_eq!(e, PresetError::StockOutputIntentNotCaptured, "{id}");
        assert_eq!(e.code(), "stock_output_intent_not_captured");
        assert_eq!(preset_outcome_for::<()>(&Err(e)), Outcome::Failure(FailureCode::Unsupported));
        let op = operator_from(p, &[("output_intent.output_intent_profile", SettingValue::Text(PROFILE.to_owned()))]).unwrap();
        assert_eq!(op.resolve(ResolveRequest::default()).unwrap_err(), at_resolution, "{id}");
        // Without a profile the contract itself refuses the standard assertion.
        assert_eq!(operator_from(p, &[]).unwrap_err(), PresetError::StandardWithoutOutputIntent, "{id}");
    }
    // Shipped presets are read-only; editing goes through an operator copy.
    let mut hq = stock_preset(stock::HIGH_QUALITY_PRINT).unwrap().to_preset().unwrap();
    assert_eq!(hq.set("general.thumbnails", SettingInput::Bool(true)).unwrap_err(), PresetError::ShippedReadOnly);
    let copy = hq.edit_copy("operator.hq-copy", "HQ copy").unwrap();
    assert_eq!(copy.source(), &PresetSource::Operator { parent: Some(stock::HIGH_QUALITY_PRINT.to_owned()) });
}

/// STU-LAY-168: one assertion per declared spec gap that it is still open.
#[test]
fn stock_gaps_stay_open() {
    let codes: Vec<(&str, Option<&str>)> = StockGap::ALL.iter().map(|g| (g.code(), g.spec_delta())).collect();
    assert_eq!(
        codes,
        [
            ("locale_variants_not_captured", None),
            ("stock_output_intent_not_captured", Some("SD-18")),
            ("jpeg2000_not_representable", Some("SD-17")),
            ("ccitt_outside_image_filter_domain", Some("SD-14")),
            ("end_page_sentinel_conflict", Some("SD-15")),
            ("cannot_embed_error_provenance_conflict", Some("SD-16")),
        ]
    );
    for gap in StockGap::ALL {
        match gap {
            StockGap::LocaleVariantsNotCaptured => {
                assert_eq!(stock_presets().len(), stock::STOCK_LOGICAL_COUNT, "no locale variant is materialised");
                assert!(stock_presets().iter().all(|p| p.description.is_ascii()));
            }
            StockGap::OutputIntentNotCaptured => {
                for p in stock_presets().iter().filter(|p| p.compliance != ComplianceCheck::None) {
                    assert_eq!(p.to_preset().unwrap_err(), PresetError::StockOutputIntentNotCaptured);
                    assert_eq!(p.value("output_intent.output_intent_profile"), Some(SettingValue::Text(String::new())));
                }
            }
            StockGap::Jpeg2000NotRepresentable => {
                assert!(SETTINGS.iter().all(|s| !s.name.contains("jpeg2000") && !s.name.contains("tile")));
            }
            StockGap::CcittOutsideFilterDomain => {
                let filter = SETTINGS.iter().find(|s| s.setting().key() == "colour_images.filter").unwrap();
                assert_eq!(filter.kind, SettingKind::Enum(EnumDomain::ImageFilter));
                let mut op = STOCK_PRESETS[0].to_preset().unwrap().edit_copy("o", "o").unwrap();
                assert_eq!(op.set("colour_images.filter", SettingInput::Token("CCITTFaxEncode")).unwrap_err().code(), "filter_not_admitted_for_group");
            }
            StockGap::EndPageSentinel => {
                assert_eq!(END_PAGE.hard_min, Bound::Value(-1.0));
                assert_eq!(STOCK_PRESETS[0].value("general.end_page"), Some(SettingValue::Integer(-1)));
            }
            StockGap::CannotEmbedErrorProvenance => {
                let error = CannotEmbedFontPolicy::MEMBERS.iter().find(|m| m.token == "Error").unwrap();
                assert_eq!(error.basis, Basis::StudioDeclared);
                let pq = stock_preset(stock::PRESS_QUALITY).unwrap();
                assert_eq!(pq.provenance().source("fonts.cannot_embed_font_policy"), Some((StockValueSource::Captured { row: "Cannot-embed-font policy" }, false)));
            }
        }
    }
}

/// V4-07: every test the DESCRIPTOR `pdf_stock_presets` entry cites exists; the new API is listed.
#[test]
fn stock_descriptor_cites_existing_tests() {
    let d = DESCRIPTOR;
    let start = d.find("\"pdf_stock_presets\":").expect("DESCRIPTOR lacks pdf_stock_presets");
    let end = start + d[start..].find("\"pdf_presets\":").expect("pdf_presets follows");
    let section = &d[start..end];
    let sources = [include_str!("pdf_stock_six_preset_defaults.rs"), include_str!("pdf_stock_preset_matrix_roundtrip.rs"), include_str!("../src/preset.rs")];
    let mut cited = 0;
    for chunk in section.split("(test").skip(1) {
        let list = chunk.trim_start_matches('s').split(')').next().unwrap();
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            assert!(sources.iter().any(|s| s.contains(&format!("fn {name}()"))), "cited test {name} does not exist");
            cited += 1;
        }
    }
    assert!(cited >= 15, "found {cited} citations");
    for api in ["PdfPreset::from_values", "PdfPreset::page_range", "ResolveReceipt::note", "StockPreset::to_preset", "stock_provenance", "pdf_preset_codec::decode"] {
        assert!(d.contains(&format!("\"{api}\"")), "api {api} not listed");
    }
}
