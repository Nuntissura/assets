//! PDF export presets (lane SMB-PDF-PRESETS-1, STU-IO-125..133, STU-IO-012) through the public API:
//! completeness, groups, domains, numeric contracts, standards, fonts, descriptions, provenance and
//! the resolution onto press-write `ExportOptions`, including the SD-13 refusals.
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_press::*;

/// Noto Sans SC Regular: OpenType with CFF outlines (`OTTO`).
const NOTO_SC_CFF: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/NotoSansSC-Regular.otf");
const SRGB: &[u8] = include_bytes!("../../hsk-studio-develop/tests/profiles/sRGB-v4.icc");
const A4: [f64; 4] = [0.0, 0.0, 595.276, 841.89];
const DESCS: &[(&str, &str)] = &[("en_US", "High-quality print"), ("de_DE", "Hohe Qualitaet"), ("fr", "Impression"), ("fr_CA", "Impression CA")];
const QUALITY: &[(&str, QualityInput<'static>)] =
    &[("QFactor", QualityInput::Number(0.15)), ("HSamples", QualityInput::Numbers(&[1.0, 1.0, 1.0, 1.0]))];
const PROFILE: &str = "Coated FOGRA39 (ISO 12647-2:2004)";

/// A valid value for every setting kind (the level is 1.4 so the preset resolves).
fn sample(spec: &SettingSpec) -> SettingInput<'static> {
    match spec.kind {
        SettingKind::Bool => SettingInput::Bool(false),
        SettingKind::Integer(c) => {
            let v = [c.default, c.hard_min].into_iter().find_map(|b| match b {
                Bound::Value(v) => Some(v),
                _ => None,
            });
            SettingInput::Integer(v.map_or(150, |v| v as i64))
        }
        SettingKind::Real(_) => SettingInput::Real(1.5),
        SettingKind::Enum(EnumDomain::CompatibilityLevel) => SettingInput::Token("1.4"),
        SettingKind::Enum(EnumDomain::ImageFilter) => SettingInput::Token("FlateEncode"),
        SettingKind::Enum(d) => SettingInput::Token(d.members()[0].token),
        SettingKind::FontList => SettingInput::Names(&[]),
        SettingKind::QualityDict => SettingInput::Quality(QUALITY),
        SettingKind::Text => SettingInput::Text(""),
    }
}

fn build_with(source: SourceInput<'_>, overrides: &[(&str, SettingInput<'_>)], omit: Option<&str>) -> Result<PdfPreset, PresetError> {
    let keys: Vec<String> = SETTINGS.iter().map(|s| s.setting().key()).collect();
    let mut entries: Vec<(&str, SettingInput<'_>)> = Vec::new();
    for (spec, key) in SETTINGS.iter().zip(&keys) {
        if omit == Some(key.as_str()) {
            continue;
        }
        let v = overrides.iter().find(|(k, _)| k == key).map_or_else(|| sample(spec), |(_, v)| *v);
        entries.push((key.as_str(), v));
    }
    PdfPreset::from_draft(&PresetDraft {
        id: "studio.high-quality-print",
        name: "High Quality Print",
        descriptions: DESCS,
        default_locale: "en_US",
        source,
        settings: &entries,
    })
}
fn build(overrides: &[(&str, SettingInput<'_>)]) -> Result<PdfPreset, PresetError> {
    build_with(SourceInput::Shipped, overrides, None)
}
fn operator(overrides: &[(&str, SettingInput<'_>)]) -> PdfPreset {
    build_with(SourceInput::Operator { parent: None }, overrides, None).unwrap()
}
fn r(group: PdfGroup, name: &'static str) -> SettingRef {
    SettingRef { group, name }
}
fn intent() -> OutputIntent<'static> {
    OutputIntent {
        icc: SRGB,
        components: 3,
        output_condition_identifier: "sRGB IEC61966-2.1",
        output_condition: None,
        registry: Some("http://www.color.org"),
        subtype: OutputIntentSubtype::GtsPdfa1,
    }
}

/// STU-IO-125: omitting any one setting refuses `preset_incomplete` naming it, even when its numeric
/// contract has a default (`image_memory`, `max_subset_percent`): nothing resolves from an ambient default.
#[test]
fn incomplete_draft_refuses() {
    assert!(build(&[]).is_ok());
    for spec in SETTINGS {
        let key = spec.setting().key();
        let err = build_with(SourceInput::Shipped, &[], Some(key.as_str())).unwrap_err();
        assert_eq!(err, PresetError::PresetIncomplete(spec.setting()), "{key}");
        assert_eq!(err.code(), "preset_incomplete");
        assert_eq!(err.to_string(), format!("preset_incomplete: {key}"));
    }
    let empty = PdfPreset::from_draft(&PresetDraft {
        id: "x",
        name: "x",
        descriptions: DESCS,
        default_locale: "en_US",
        source: SourceInput::Shipped,
        settings: &[],
    });
    assert_eq!(empty.unwrap_err(), PresetError::PresetIncomplete(r(PdfGroup::General, "compatibility_level")));
}

/// STU-IO-126 / STU-IO-127: ten groups, every setting declares one; each image group carries the same
/// sixteen-setting family in the same order; studio-assigned groups are labelled.
#[test]
fn every_setting_declares_its_group() {
    let tokens: Vec<&str> = PdfGroup::ALL.iter().map(|g| g.token()).collect();
    assert_eq!(
        tokens,
        [
            "general",
            "fonts",
            "colour_images",
            "greyscale_images",
            "monochrome_images",
            "colour_management",
            "output_intent",
            "standards_compliance",
            "postscript_and_dsc",
            "security"
        ]
    );
    assert_eq!(SETTINGS.len(), 77);
    let count = |g: PdfGroup| SETTINGS.iter().filter(|s| s.group == g).count();
    let counts: Vec<usize> = PdfGroup::ALL.iter().map(|g| count(*g)).collect();
    assert_eq!(counts, [10, 7, 16, 16, 16, 6, 1, 4, 1, 0]);
    let mut keys: Vec<String> = SETTINGS.iter().map(|s| s.setting().key()).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), SETTINGS.len(), "keys are unique");
    let family = |g: PdfGroup| SETTINGS.iter().filter(|s| s.group == g).map(|s| s.name).collect::<Vec<_>>();
    let colour = family(PdfGroup::ColourImages);
    assert_eq!(
        colour,
        [
            "anti_alias",
            "crop_to_frame",
            "minimum_resolution",
            "minimum_resolution_policy",
            "downsample",
            "downsample_type",
            "target_resolution",
            "bit_depth",
            "minimum_downsample_depth",
            "downsample_threshold",
            "encode",
            "filter",
            "auto_filter",
            "auto_filter_strategy",
            "base_quality_dictionary",
            "auto_colour_space_quality_dictionary"
        ]
    );
    assert_eq!(family(PdfGroup::GreyscaleImages), colour);
    assert_eq!(family(PdfGroup::MonochromeImages), colour);
    assert_eq!(
        family(PdfGroup::General),
        ["compatibility_level", "start_page", "end_page", "thumbnails", "optimisation", "binding", "auto_rotate_pages", "compress_objects", "image_memory", "allow_transparency"]
    );
    assert_eq!(
        family(PdfGroup::Fonts),
        ["embed_all_fonts", "subset_fonts", "max_subset_percent", "embed_open_type", "always_embed", "never_embed", "cannot_embed_font_policy"]
    );
    let declared: Vec<String> =
        SETTINGS.iter().filter(|s| s.group_basis == Basis::StudioDeclared).map(|s| s.setting().key()).collect();
    assert_eq!(
        declared,
        [
            "general.image_memory",
            "general.allow_transparency",
            "colour_management.colour_conversion_strategy",
            "colour_management.transfer_function_info",
            "colour_management.ucr_and_black_generation",
            "colour_management.default_rendering_intent",
            "colour_management.overprint_mode",
            "colour_management.preserve_overprint",
            "output_intent.output_intent_profile",
            "standards_compliance.compliance_check",
            "postscript_and_dsc.dsc_reporting_level"
        ]
    );
    assert_eq!(Basis::StudioDeclared.token(), "studio_declared");
    // Typed views: every image group has a family, other groups have none.
    let p = build(&[]).unwrap();
    for g in PdfGroup::ALL {
        assert_eq!(p.image_family(g).is_some(), g.is_image(), "{}", g.token());
    }
    let fam = p.image_family(PdfGroup::MonochromeImages).unwrap();
    assert_eq!(fam.target_resolution, 1200);
    assert_eq!(fam.base_quality_dictionary.len(), 2);
    assert_eq!(fam.base_quality_dictionary[1].value, QualityValue::Numbers(vec![1.0; 4]));
}

/// STU-IO-128: the fourteen enumerations exactly as tabled (plus the STU-LAY-159 additions), exact tokens, studio-declared members
/// labelled, single-observed-value rows partial.
#[test]
fn domains_are_exact() {
    use Basis::{Spec as S, StudioDeclared as D};
    use DomainCompleteness::{Complete as C, Partial as P};
    type Row = (&'static str, &'static [(&'static str, Basis)], DomainCompleteness);
    let expected: [Row; 15] = [
        ("compatibility_level", &[("1.3", S), ("1.4", S), ("1.5", S)], C),
        (
            "colour_conversion_strategy",
            &[("LeaveColorUnchanged", S), ("UseDeviceIndependentColor", S), ("CMYK", S), ("sRGB", S)],
            C,
        ),
        ("downsample_type", &[("Bicubic", S), ("None", S)], C),
        ("image_filter", &[("DCTEncode", S), ("FlateEncode", S), ("CCITTFaxEncode", S)], C),
        ("auto_filter_strategy", &[("JPEG", S), ("JPEG2000", S)], C),
        ("minimum_resolution_policy", &[("OK", S), ("Warning", S)], C),
        ("transfer_function_info", &[("Apply", S), ("Remove", S)], C),
        ("ucr_and_black_generation", &[("Preserve", S), ("Remove", S)], C),
        ("auto_rotate_pages", &[("All", S), ("None", S)], C),
        ("binding", &[("Left", S), ("Right", D)], P),
        ("compress_objects", &[("Tags", S), ("Off", S), ("All", S)], C),
        ("overprint_mode", &[("1", S)], P),
        ("cannot_embed_font_policy", &[("Warning", S), ("Error", D), ("OK", D)], P),
        ("default_rendering_intent", &[("Default", S)], P),
        ("compliance_check", &[("none", S), ("PDF/X-1a:2001", S), ("PDF/X-3:2002", S), ("PDF/X-4:2007", S)], P),
    ];
    assert_eq!(EnumDomain::ALL.len(), 15);
    for (domain, (setting, members, completeness)) in EnumDomain::ALL.iter().zip(expected) {
        assert_eq!(domain.setting(), setting);
        let got: Vec<(&str, Basis)> = domain.members().iter().map(|m| (m.token, m.basis)).collect();
        assert_eq!(got, members, "{setting}");
        assert_eq!(domain.completeness(), completeness, "{setting}");
        for (token, _) in members {
            let v = domain.parse(token).unwrap();
            assert_eq!((v.token(), v.domain()), (*token, *domain));
        }
    }
    assert_eq!(DomainCompleteness::Partial.token(), "partial");
    // Exact tokens: no case folding, no added or mapped member.
    for (domain, token) in [
        (EnumDomain::CompatibilityLevel, "1.6"),
        (EnumDomain::CompatibilityLevel, "2.0"),
        (EnumDomain::ImageFilter, "dctencode"),
        (EnumDomain::ImageFilter, "ccittfaxencode"),
        (EnumDomain::ColourConversionStrategy, "srgb"),
        (EnumDomain::Binding, "right"),
        (EnumDomain::CannotEmbedFontPolicy, "Ok"),
        (EnumDomain::DownsampleType, " Bicubic"),
    ] {
        assert_eq!(domain.parse(token), None, "{token:?}");
    }
    let key = "general.compatibility_level";
    assert_eq!(build(&[(key, SettingInput::Token("1.7"))]).unwrap_err(), PresetError::UnknownMember(r(PdfGroup::General, "compatibility_level")));
    // overprint_mode is an integer domain: the integer and its token are both accepted.
    let om = "colour_management.overprint_mode";
    assert!(build(&[(om, SettingInput::Integer(1))]).is_ok());
    assert_eq!(build(&[(om, SettingInput::Integer(0))]).unwrap_err().code(), "unknown_member");
    assert_eq!(build(&[("general.binding", SettingInput::Token("Right"))]).unwrap().value("general.binding"), Some(&SettingValue::Enum(EnumValue::Binding(Binding::Right))));
}

/// STU-IO-129: every contract stores its seven fields separately, unknown and absent distinct and
/// never set equal to the opposite bound.
#[test]
fn numeric_contracts_store_seven_fields() {
    use Bound::{Absent as A, Unknown as U, Value as V};
    let rows: [(&NumericContract, [Bound; 5], &str, u8); 10] = [
        (&COLOUR_IMAGE_RESOLUTION, [V(1.0), U, V(72.0), V(600.0), V(300.0)], "ppi", 0),
        (&GREYSCALE_IMAGE_RESOLUTION, [V(1.0), U, V(72.0), V(600.0), V(300.0)], "ppi", 0),
        (&MONOCHROME_IMAGE_RESOLUTION, [V(1.0), U, V(300.0), V(2400.0), V(1200.0)], "ppi", 0),
        (&DOWNSAMPLE_THRESHOLD, [V(1.0), U, V(1.0), V(3.0), V(1.5)], "ratio", 5),
        (&MAX_SUBSET_PERCENT, [V(0.0), V(100.0), V(0.0), V(100.0), V(100.0)], "percent", 0),
        (&IMAGE_MEMORY, [U, U, A, A, V(1_048_576.0)], "bytes", 0),
        (&START_PAGE, [V(1.0), U, V(1.0), U, V(1.0)], "page number", 0),
        (&END_PAGE, [V(-1.0), U, V(-1.0), U, V(-1.0)], "page number (-1 = last)", 0),
        (&DSC_REPORTING_LEVEL, [V(0.0), U, V(0.0), V(3.0), V(0.0)], "level", 0),
        (&BIT_DEPTH, [V(-1.0), U, V(-1.0), V(16.0), V(-1.0)], "bits (-1 = unchanged)", 0),
    ];
    for (c, b, unit, precision) in rows {
        assert_eq!([c.hard_min, c.hard_max, c.soft_min, c.soft_max, c.default], b, "{}", c.name);
        assert_eq!((c.unit, c.precision, c.spec_row), (unit, precision, true), "{}", c.name);
    }
    assert_ne!(IMAGE_MEMORY.hard_min, IMAGE_MEMORY.soft_min, "unknown and absent stay distinct");
    for c in [MINIMUM_RESOLUTION, MINIMUM_DOWNSAMPLE_DEPTH] {
        assert!(!c.spec_row);
        assert_eq!([c.hard_min, c.hard_max, c.soft_min, c.soft_max, c.default], [U; 5]);
    }
    // Each image group's target resolution carries its own contract; bit_depth the shared one.
    for (g, c) in [
        (PdfGroup::ColourImages, &COLOUR_IMAGE_RESOLUTION),
        (PdfGroup::GreyscaleImages, &GREYSCALE_IMAGE_RESOLUTION),
        (PdfGroup::MonochromeImages, &MONOCHROME_IMAGE_RESOLUTION),
    ] {
        let kind = |n: &str| SETTINGS.iter().find(|s| s.group == g && s.name == n).unwrap().kind;
        assert_eq!(kind("target_resolution"), SettingKind::Integer(c));
        assert_eq!(kind("bit_depth"), SettingKind::Integer(&BIT_DEPTH));
        assert_eq!(kind("downsample_threshold"), SettingKind::Real(&DOWNSAMPLE_THRESHOLD));
    }
}

/// STU-IO-129: outside a hard bound refuses; outside a soft bound is a typed warning; an unknown
/// bound is no bound (an observed value is never turned into one).
#[test]
fn hard_and_soft_bounds() {
    let hard = |key: &str, v: SettingInput<'static>| build(&[(key, v)]).unwrap_err();
    for (key, v) in [
        ("colour_images.target_resolution", SettingInput::Integer(0)),
        ("fonts.max_subset_percent", SettingInput::Integer(101)),
        ("fonts.max_subset_percent", SettingInput::Integer(-1)),
        ("general.start_page", SettingInput::Integer(0)),
        ("general.end_page", SettingInput::Integer(-2)),
        ("greyscale_images.bit_depth", SettingInput::Integer(-2)),
        ("monochrome_images.downsample_threshold", SettingInput::Real(0.5)),
        ("postscript_and_dsc.dsc_reporting_level", SettingInput::Integer(-1)),
    ] {
        let e = hard(key, v);
        assert_eq!(e.code(), "outside_hard_bounds", "{key}");
        assert_eq!(e.setting().unwrap().key(), key);
    }
    let warned = |key: &str, v: SettingInput<'static>| {
        let p = build(&[(key, v)]).unwrap();
        p.warnings().into_iter().map(|w| (w.setting.key(), w.value)).collect::<Vec<_>>()
    };
    assert_eq!(warned("colour_images.target_resolution", SettingInput::Integer(700)), [("colour_images.target_resolution".to_owned(), 700.0)]);
    // hard_max is unknown: a large value is legal and only warned.
    assert_eq!(warned("colour_images.target_resolution", SettingInput::Integer(100_000)).len(), 1);
    assert_eq!(warned("monochrome_images.target_resolution", SettingInput::Integer(150)).len(), 1);
    assert_eq!(warned("postscript_and_dsc.dsc_reporting_level", SettingInput::Integer(4)).len(), 1);
    assert_eq!(warned("colour_images.downsample_threshold", SettingInput::Real(3.5)).len(), 1);
    assert_eq!(warned("greyscale_images.bit_depth", SettingInput::Integer(32)).len(), 1);
    // Unknown and absent bounds do not bound.
    assert!(warned("general.image_memory", SettingInput::Integer(i64::MIN)).is_empty());
    assert!(warned("general.image_memory", SettingInput::Integer(i64::MAX)).is_empty());
    assert!(warned("colour_images.minimum_resolution", SettingInput::Integer(i64::MAX)).is_empty());
    assert!(warned("general.end_page", SettingInput::Integer(1_000_000)).is_empty());
    // Precision is stored, the value is not truncated; an integer is accepted for a real setting.
    let p = build(&[("colour_images.downsample_threshold", SettingInput::Real(1.234_567_89))]).unwrap();
    assert_eq!(p.image_family(PdfGroup::ColourImages).unwrap().downsample_threshold, 1.234_567_89);
    let p = build(&[("colour_images.downsample_threshold", SettingInput::Integer(2))]).unwrap();
    assert_eq!(p.value("colour_images.downsample_threshold"), Some(&SettingValue::Real(2.0)));
    // set on an operator preset reports the warning and keeps the value.
    let mut op = operator(&[]);
    let w = op.set("greyscale_images.target_resolution", SettingInput::Integer(50)).unwrap().unwrap();
    assert_eq!((w.setting.key(), w.value, w.soft_min, w.soft_max), ("greyscale_images.target_resolution".to_owned(), 50.0, Bound::Value(72.0), Bound::Value(600.0)));
    assert_eq!(PresetWarning::CODE, "outside_soft_bounds");
    assert_eq!(op.set("greyscale_images.target_resolution", SettingInput::Integer(0)).unwrap_err().code(), "outside_hard_bounds");
    assert_eq!(op.image_family(PdfGroup::GreyscaleImages).unwrap().target_resolution, 50);
}

/// STU-IO-130: a standard check with an empty output-intent profile is a validation error, at build
/// and at edit; `standard_compliant_only` alone asserts no standard.
#[test]
fn standard_without_output_intent_refuses() {
    for check in ["standards_compliance.standard_check_1a", "standards_compliance.standard_check_3"] {
        assert_eq!(build(&[(check, SettingInput::Bool(true))]).unwrap_err(), PresetError::StandardWithoutOutputIntent);
        let p = build(&[(check, SettingInput::Bool(true)), ("output_intent.output_intent_profile", SettingInput::Text(PROFILE))]).unwrap();
        assert!(p.standards().asserts_standard());
        assert_eq!(p.standards().output_intent_profile, PROFILE);
    }
    assert!(build(&[("standards_compliance.standard_compliant_only", SettingInput::Bool(true))]).is_ok());
    let mut op = operator(&[("standards_compliance.standard_check_3", SettingInput::Bool(true)), ("output_intent.output_intent_profile", SettingInput::Text(PROFILE))]);
    let e = op.set("output_intent.output_intent_profile", SettingInput::Text("")).unwrap_err();
    assert_eq!(e.code(), "standard_without_output_intent");
    assert_eq!(op.standards().output_intent_profile, PROFILE, "a refused edit leaves the preset unchanged");
    let mut op = operator(&[]);
    assert_eq!(op.set("standards_compliance.standard_check_1a", SettingInput::Bool(true)).unwrap_err(), PresetError::StandardWithoutOutputIntent);
    assert!(!op.standards().standard_check_1a);
}

/// STU-IO-131 / STU-IO-012: a font the writer refuses to embed yields a receipt naming the font and
/// the policy; under every policy the outcome is the export refusal with the declared gap.
#[test]
fn font_receipt_under_every_policy() {
    for (token, policy) in
        [("Warning", CannotEmbedFontPolicy::Warning), ("Error", CannotEmbedFontPolicy::Error), ("OK", CannotEmbedFontPolicy::Ok)]
    {
        let p = build(&[("fonts.cannot_embed_font_policy", SettingInput::Token(token))]).unwrap();
        assert_eq!(p.fonts().cannot_embed_font_policy, policy);
        let res = p.resolve(ResolveRequest::default()).unwrap();
        let mut ex = PdfExport::new(res.options, &Limits::default()).unwrap();
        let cff = ex.embed_truetype(NOTO_SC_CFF, 0, &[], &[]).unwrap_err();
        let garbage = ex.embed_font(b"not a font program", 0, &[], &[]).unwrap_err();
        for (err, cause) in [(cff, "font_cff_not_embedded"), (garbage, "font_unreadable")] {
            let receipt = p.font_receipt("NotoSansSC-Regular", &err).unwrap().unwrap();
            assert_eq!(receipt.font, "NotoSansSC-Regular");
            assert_eq!(receipt.policy, policy);
            assert_eq!(receipt.cause, cause);
            assert_eq!((receipt.outcome, receipt.gap), ("export_refused", FONT_GAP));
        }
        let status = res.receipt.status("fonts.cannot_embed_font_policy").unwrap();
        let expected = match policy {
            CannotEmbedFontPolicy::Error => SettingStatus::Applied,
            _ => SettingStatus::NotApplied { code: "writer_refuses_unembeddable_font" },
        };
        assert_eq!(status, expected, "{token}");
    }
    let p = build(&[]).unwrap();
    assert_eq!(p.font_receipt("Inter", &ExportError::NoPages), Ok(None));
    assert_eq!(p.font_receipt("", &ExportError::FontUnreadable).unwrap_err().code(), "font_name_invalid");
    // The typed font settings.
    let p = build(&[
        ("fonts.always_embed", SettingInput::Names(&["Inter", "Inter-Bold"])),
        ("fonts.never_embed", SettingInput::Names(&["Symbol"])),
        ("fonts.embed_all_fonts", SettingInput::Bool(true)),
        ("fonts.subset_fonts", SettingInput::Bool(true)),
    ])
    .unwrap();
    let f = p.fonts();
    assert_eq!((f.embed_all_fonts, f.subset_fonts, f.max_subset_percent, f.embed_open_type), (true, true, 100, false));
    assert_eq!((f.always_embed, f.never_embed), (&["Inter".to_owned(), "Inter-Bold".to_owned()][..], &["Symbol".to_owned()][..]));
    let rec = p.resolve(ResolveRequest::default()).unwrap().receipt;
    assert_eq!(rec.status("fonts.never_embed"), Some(SettingStatus::NotApplied { code: "writer_embeds_every_font" }));
    assert_eq!(rec.status("fonts.always_embed"), Some(SettingStatus::Applied));
}

/// STU-IO-132: the description map is kept per locale and rendered through the declared chain.
#[test]
fn descriptions_fall_back_in_order() {
    assert_eq!(DESCRIPTION_FALLBACK_CHAIN, ["requested_locale", "language", "default_locale"]);
    let p = build(&[]).unwrap();
    let d = p.descriptions();
    assert_eq!(d.len(), 4);
    assert_eq!(d.default_locale(), "en_US");
    assert_eq!(d.entries().collect::<Vec<_>>(), DESCS);
    let got = |loc: &str| {
        let x = d.render(loc);
        (x.locale, x.text, x.step)
    };
    assert_eq!(got("de_DE"), ("de_DE", "Hohe Qualitaet", FallbackStep::RequestedLocale));
    assert_eq!(got("de-de"), ("de_DE", "Hohe Qualitaet", FallbackStep::RequestedLocale));
    assert_eq!(got("de_AT"), ("de_DE", "Hohe Qualitaet", FallbackStep::Language));
    assert_eq!(got("fr_BE"), ("fr", "Impression", FallbackStep::Language), "the bare language tag wins");
    assert_eq!(got("fr_CA"), ("fr_CA", "Impression CA", FallbackStep::RequestedLocale));
    assert_eq!(got("ja_JP"), ("en_US", "High-quality print", FallbackStep::DefaultLocale));
    assert_eq!(got(""), ("en_US", "High-quality print", FallbackStep::DefaultLocale));
    assert_eq!(got("x"), ("en_US", "High-quality print", FallbackStep::DefaultLocale));
    assert_eq!(d.get("FR"), Some("Impression"));
    let mk = |descs: &[(&str, &str)], default: &str| {
        let keys: Vec<String> = SETTINGS.iter().map(|s| s.setting().key()).collect();
        let entries: Vec<(&str, SettingInput<'_>)> = SETTINGS.iter().zip(&keys).map(|(s, k)| (k.as_str(), sample(s))).collect();
        PdfPreset::from_draft(&PresetDraft { id: "p", name: "p", descriptions: descs, default_locale: default, source: SourceInput::Shipped, settings: &entries })
    };
    assert_eq!(mk(&[], "en").unwrap_err(), PresetError::NoDescriptions);
    assert_eq!(mk(&[("en_US", "a"), ("en-us", "b")], "en_US").unwrap_err(), PresetError::DuplicateLocale { position: 1 });
    assert_eq!(mk(&[("en_US", "a")], "de_DE").unwrap_err(), PresetError::DefaultLocaleMissing);
    for bad in ["e", "en US", "1n", "en__US", "en-", "en-toolongsubtag"] {
        assert_eq!(mk(&[(bad, "a")], bad).unwrap_err(), PresetError::InvalidLocale { position: 0 }, "{bad:?}");
    }
    assert!(mk(&[("en_us", "a")], "EN-US").is_ok(), "the default locale matches case- and separator-insensitively");
}

/// STU-IO-133: shipped presets are read-only; an edit copy is an operator preset with the parent
/// recorded; imported presets carry their origin.
#[test]
fn shipped_is_read_only_and_copies_record_parent() {
    let mut shipped = build(&[]).unwrap();
    assert_eq!((shipped.source(), shipped.source().token()), (&PresetSource::Shipped, "shipped"));
    assert!(shipped.source().is_read_only());
    let before = shipped.clone();
    assert_eq!(shipped.set("general.thumbnails", SettingInput::Bool(true)).unwrap_err(), PresetError::ShippedReadOnly);
    assert_eq!(shipped, before);
    let mut copy = shipped.edit_copy("operator.my-print", "My Print").unwrap();
    assert_eq!(copy.source(), &PresetSource::Operator { parent: Some("studio.high-quality-print".to_owned()) });
    assert_eq!((copy.id(), copy.name()), ("operator.my-print", "My Print"));
    assert!(copy.settings().zip(shipped.settings()).all(|(a, b)| a == b));
    assert_eq!(copy.set("general.thumbnails", SettingInput::Bool(true)), Ok(None));
    assert_eq!(copy.value("general.thumbnails"), Some(&SettingValue::Bool(true)));
    assert_eq!(shipped.value("general.thumbnails"), Some(&SettingValue::Bool(false)));
    assert_eq!(shipped.edit_copy("bad id", "x").unwrap_err(), PresetError::InvalidId);
    assert_eq!(shipped.edit_copy("ok", "").unwrap_err(), PresetError::InvalidName);
    let imported = build_with(SourceInput::Imported { origin: "joboptions:PDFX4 2008.joboptions" }, &[], None).unwrap();
    assert_eq!(imported.source(), &PresetSource::Imported { origin: "joboptions:PDFX4 2008.joboptions".to_owned() });
    assert_eq!(imported.source().token(), "imported");
    assert_eq!(build_with(SourceInput::Imported { origin: "" }, &[], None).unwrap_err(), PresetError::InvalidOrigin);
    let child = build_with(SourceInput::Operator { parent: Some("studio.high-quality-print") }, &[], None).unwrap();
    assert_eq!(child.source().token(), "operator");
    assert_eq!(build_with(SourceInput::Operator { parent: Some("") }, &[], None).unwrap_err(), PresetError::InvalidId);
}

/// Resolution builds every `ExportOptions` field from the preset or the request (the writer's own
/// default is version 1.7 with subsetting on; neither leaks in), and the options drive a real export.
#[test]
fn resolve_builds_every_option_from_the_preset() {
    let p = build(&[("general.compatibility_level", SettingInput::Token("1.5")), ("fonts.subset_fonts", SettingInput::Bool(false))]).unwrap();
    let res = p.resolve(ResolveRequest { info_title: Some("Proof"), ..ResolveRequest::default() }).unwrap();
    let o = res.options;
    assert_eq!((o.version, o.subset_fonts, o.info_title, o.pdfa2b), ("1.5", false, Some("Proof"), false));
    assert!(o.output_intent.is_none() && o.pdfx4.is_none());
    let mut ex = PdfExport::new(o, &Limits::default()).unwrap();
    let page = ex.add_page(PageSpec { media_box_pt: A4, trim_box_pt: None, bleed_box_pt: None }).unwrap();
    let square = [PathCommand::MoveTo(10.0, 10.0), PathCommand::LineTo(50.0, 10.0), PathCommand::LineTo(50.0, 50.0), PathCommand::Close];
    ex.fill_path(page, &square, FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
    let (bytes, _) = ex.finish(&CancellationToken::default()).unwrap();
    assert!(bytes.starts_with(b"%PDF-1.5"));
    let p14 = build(&[]).unwrap();
    assert_eq!(p14.compatibility_level(), CompatibilityLevel::V1_4);
    assert_eq!(p14.resolve(ResolveRequest::default()).unwrap().options.version, "1.4");
    // A declared profile with the caller's intent passes the intent through; PDF/A-2b is the caller's request.
    let pa = build(&[("output_intent.output_intent_profile", SettingInput::Text("sRGB IEC61966-2.1"))]).unwrap();
    let res = pa.resolve(ResolveRequest { output_intent: Some(intent()), pdfa2b: true, ..ResolveRequest::default() }).unwrap();
    assert!(res.options.pdfa2b && res.options.output_intent.is_some());
    assert_eq!(res.receipt.status("output_intent.output_intent_profile"), Some(SettingStatus::Applied));
}

/// No silent green: every setting is listed, applied or not applied with a code.
#[test]
fn resolve_receipt_lists_every_setting() {
    let p = build(&[
        ("colour_images.downsample", SettingInput::Bool(true)),
        ("colour_images.filter", SettingInput::Token("DCTEncode")),
        ("greyscale_images.bit_depth", SettingInput::Integer(8)),
        ("standards_compliance.standard_check_1a", SettingInput::Bool(true)),
        ("output_intent.output_intent_profile", SettingInput::Text(PROFILE)),
        ("colour_images.target_resolution", SettingInput::Integer(700)),
        ("general.compress_objects", SettingInput::Token("Off")),
    ])
    .unwrap();
    let rec = p.resolve(ResolveRequest::default()).unwrap().receipt;
    assert_eq!(rec.preset_id, "studio.high-quality-print");
    assert_eq!(rec.conformance, "not_proven");
    assert_eq!(rec.entries.len(), SETTINGS.len());
    assert!(rec.entries.iter().zip(SETTINGS).all(|(e, s)| e.setting == s.setting()));
    let na = |code| Some(SettingStatus::NotApplied { code });
    for (key, status) in [
        ("general.compatibility_level", Some(SettingStatus::Applied)),
        ("general.start_page", na("page_range_owned_by_caller")),
        ("general.binding", na("binding_not_written")),
        ("general.compress_objects", Some(SettingStatus::Applied)),
        ("general.image_memory", na("interpreter_setting_not_implemented")),
        ("colour_images.downsample", na("downsampling_not_implemented")),
        ("colour_images.target_resolution", na("downsampling_not_implemented")),
        ("colour_images.filter", na("recompression_not_implemented")),
        ("colour_images.base_quality_dictionary", na("recompression_not_implemented")),
        ("greyscale_images.downsample", Some(SettingStatus::Applied)),
        ("greyscale_images.filter", Some(SettingStatus::Applied)),
        ("greyscale_images.bit_depth", na("bit_depth_conversion_not_implemented")),
        ("monochrome_images.bit_depth", Some(SettingStatus::Applied)),
        ("monochrome_images.encode", na("writer_always_encodes")),
        ("fonts.max_subset_percent", na("subset_threshold_not_implemented")),
        ("fonts.subset_fonts", Some(SettingStatus::Applied)),
        ("colour_management.overprint_mode", na("overprint_not_implemented")),
        ("colour_management.transfer_function_info", na("postscript_colour_setting_not_implemented")),
        ("output_intent.output_intent_profile", na("output_intent_not_supplied")),
        ("standards_compliance.standard_check_1a", na("standard_check_not_implemented")),
        ("standards_compliance.standard_check_3", Some(SettingStatus::Applied)),
        ("postscript_and_dsc.dsc_reporting_level", na("postscript_setting_not_implemented")),
    ] {
        assert_eq!(rec.status(key), status, "{key}");
    }
    assert_eq!(rec.status("security.anything"), None);
    let applied = rec.entries.iter().filter(|e| e.status == SettingStatus::Applied).count();
    assert!(applied > 0 && applied < SETTINGS.len());
    assert_eq!(rec.warnings.len(), 1);
    assert_eq!(rec.warnings[0].setting.key(), "colour_images.target_resolution");
}

/// SD-13: the writer starts at 1.4 and writes PDF/X-4 only at 1.6+, while the preset domain is
/// 1.3-1.5. Level 1.3 and any PDF/X-4 request refuse typed; nothing is resolved onto another level.
#[test]
fn sd13_refusals() {
    let p13 = build(&[("general.compatibility_level", SettingInput::Token("1.3"))]).unwrap();
    let e = p13.resolve(ResolveRequest::default()).unwrap_err();
    assert_eq!(e, PresetError::CompatibilityLevelNotWritable(CompatibilityLevel::V1_3));
    assert_eq!(e.code(), "compatibility_level_not_writable");
    let x4 = PdfX4 { created: "D:20261031000000Z", modified: "D:20261031000000Z", trapped: false };
    for level in ["1.4", "1.5"] {
        let p = build(&[("general.compatibility_level", SettingInput::Token(level)), ("output_intent.output_intent_profile", SettingInput::Text(PROFILE))]).unwrap();
        let e = p.resolve(ResolveRequest { pdfx4: Some(x4), output_intent: Some(intent()), ..ResolveRequest::default() }).unwrap_err();
        assert_eq!(e.code(), "standard_level_conflict", "{level}");
        assert_eq!(e, PresetError::StandardLevelConflict(p.compatibility_level()));
    }
    let strict = build(&[
        ("standards_compliance.standard_check_3", SettingInput::Bool(true)),
        ("standards_compliance.standard_compliant_only", SettingInput::Bool(true)),
        ("output_intent.output_intent_profile", SettingInput::Text(PROFILE)),
    ])
    .unwrap();
    assert_eq!(strict.resolve(ResolveRequest::default()).unwrap_err(), PresetError::StandardComplianceNotProvable);
    let undeclared = build(&[]).unwrap();
    assert_eq!(
        undeclared.resolve(ResolveRequest { output_intent: Some(intent()), ..ResolveRequest::default() }).unwrap_err(),
        PresetError::OutputIntentNotDeclared
    );
    // Codes only through Observe: SD-13 and standards refusals are Unsupported, the rest Validation.
    assert_eq!(preset_outcome_for(&p13.resolve(ResolveRequest::default())), Outcome::Failure(FailureCode::Unsupported));
    assert_eq!(preset_outcome_for(&strict.resolve(ResolveRequest::default())), Outcome::Failure(FailureCode::Unsupported));
    assert_eq!(preset_outcome_for::<()>(&Err(PresetError::OutputIntentNotDeclared)), Outcome::Failure(FailureCode::Validation));
    assert_eq!(preset_outcome_for(&build(&[]).unwrap().resolve(ResolveRequest::default())), Outcome::Success);
}

/// V4-01/02: every count and length is bounded and checked before the owned copy is made.
#[test]
fn hostile_sizes_refuse_before_allocation() {
    let many: Vec<(&str, SettingInput<'_>)> = vec![("general.thumbnails", SettingInput::Bool(false)); MAX_PRESET_ENTRIES + 1];
    let d = PresetDraft { id: "p", name: "p", descriptions: DESCS, default_locale: "en_US", source: SourceInput::Shipped, settings: &many };
    assert_eq!(PdfPreset::from_draft(&d).unwrap_err(), PresetError::TooManySettings);
    let locales: Vec<String> = (0..=MAX_LOCALES).map(|i| format!("l{}", char::from(b'a' + (i % 26) as u8))).collect();
    let descs: Vec<(&str, &str)> = locales.iter().map(|l| (l.as_str(), "x")).collect();
    let d = PresetDraft { id: "p", name: "p", descriptions: &descs, default_locale: "la", source: SourceInput::Shipped, settings: &[] };
    assert_eq!(PdfPreset::from_draft(&d).unwrap_err(), PresetError::TooManyLocales);
    let long_text = "x".repeat(MAX_DESCRIPTION_BYTES + 1);
    let d = PresetDraft { id: "p", name: "p", descriptions: &[("en", long_text.as_str())], default_locale: "en", source: SourceInput::Shipped, settings: &[] };
    assert_eq!(PdfPreset::from_draft(&d).unwrap_err(), PresetError::DescriptionTooLong { position: 0 });
    let long_locale = format!("en{}", "-abcdefgh".repeat(4));
    assert!(long_locale.len() > MAX_LOCALE_BYTES);
    let d = PresetDraft { id: "p", name: "p", descriptions: &[(long_locale.as_str(), "x")], default_locale: long_locale.as_str(), source: SourceInput::Shipped, settings: &[] };
    assert_eq!(PdfPreset::from_draft(&d).unwrap_err(), PresetError::InvalidLocale { position: 0 });
    let names: Vec<&str> = vec!["Inter"; MAX_FONT_LIST + 1];
    assert_eq!(build(&[("fonts.always_embed", SettingInput::Names(&names))]).unwrap_err().code(), "list_too_long");
    assert!(build(&[("fonts.never_embed", SettingInput::Names(&names[..MAX_FONT_LIST]))]).is_ok());
    let long_name = "F".repeat(MAX_FONT_NAME_BYTES + 1);
    assert_eq!(build(&[("fonts.never_embed", SettingInput::Names(&[long_name.as_str()]))]).unwrap_err().code(), "font_name_invalid");
    let q: Vec<(&str, QualityInput<'_>)> = (0..=MAX_QUALITY_ENTRIES).map(|_| ("Q", QualityInput::Integer(1))).collect();
    assert_eq!(build(&[("colour_images.base_quality_dictionary", SettingInput::Quality(&q))]).unwrap_err().code(), "list_too_long");
    let nums = [1.0; MAX_QUALITY_NUMBERS + 1];
    let q = [("HSamples", QualityInput::Numbers(&nums))];
    assert_eq!(build(&[("colour_images.base_quality_dictionary", SettingInput::Quality(&q))]).unwrap_err().code(), "quality_dictionary_invalid");
    let long_profile = "p".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(build(&[("output_intent.output_intent_profile", SettingInput::Text(&long_profile))]).unwrap_err().code(), "text_invalid");
    let long_id = "i".repeat(MAX_PRESET_ID_BYTES + 1);
    let d = PresetDraft { id: &long_id, name: "p", descriptions: DESCS, default_locale: "en_US", source: SourceInput::Shipped, settings: &[] };
    assert_eq!(PdfPreset::from_draft(&d).unwrap_err(), PresetError::InvalidId);
    let long_preset_name = "n".repeat(MAX_PRESET_NAME_BYTES + 1);
    let d = PresetDraft { id: "p", name: &long_preset_name, descriptions: DESCS, default_locale: "en_US", source: SourceInput::Shipped, settings: &[] };
    assert_eq!(PdfPreset::from_draft(&d).unwrap_err(), PresetError::InvalidName);
}

/// Hostile keys and values refuse typed and name the setting (never echo caller text).
#[test]
fn hostile_values_refuse_typed() {
    for bad in ["general.nope", "nodot", "security.encrypt", ".compatibility_level", "General.compatibility_level", "general.compatibility_level.x"] {
        let e = build(&[]).and_then(|_| {
            let keys: Vec<String> = SETTINGS.iter().map(|s| s.setting().key()).collect();
            let mut entries: Vec<(&str, SettingInput<'_>)> = SETTINGS.iter().zip(&keys).map(|(s, k)| (k.as_str(), sample(s))).collect();
            entries.insert(3, (bad, SettingInput::Bool(true)));
            PdfPreset::from_draft(&PresetDraft { id: "p", name: "p", descriptions: DESCS, default_locale: "en_US", source: SourceInput::Shipped, settings: &entries })
        });
        assert_eq!(e.unwrap_err(), PresetError::UnknownSetting { position: 3 }, "{bad:?}");
    }
    let keys: Vec<String> = SETTINGS.iter().map(|s| s.setting().key()).collect();
    let mut entries: Vec<(&str, SettingInput<'_>)> = SETTINGS.iter().zip(&keys).map(|(s, k)| (k.as_str(), sample(s))).collect();
    entries.push(("fonts.subset_fonts", SettingInput::Bool(true)));
    let dup = PdfPreset::from_draft(&PresetDraft { id: "p", name: "p", descriptions: DESCS, default_locale: "en_US", source: SourceInput::Shipped, settings: &entries });
    assert_eq!(dup.unwrap_err(), PresetError::DuplicateSetting(r(PdfGroup::Fonts, "subset_fonts")));
    fn kind(key: &str, v: SettingInput<'_>) -> &'static str {
        build(&[(key, v)]).unwrap_err().code()
    }
    assert_eq!(kind("colour_images.target_resolution", SettingInput::Real(300.0)), "wrong_value_kind");
    assert_eq!(kind("general.thumbnails", SettingInput::Token("true")), "wrong_value_kind");
    assert_eq!(kind("general.binding", SettingInput::Integer(0)), "wrong_value_kind");
    assert_eq!(kind("fonts.always_embed", SettingInput::Text("Inter")), "wrong_value_kind");
    assert_eq!(kind("output_intent.output_intent_profile", SettingInput::Names(&[])), "wrong_value_kind");
    assert_eq!(kind("colour_images.downsample_type", SettingInput::Token("bicubic")), "unknown_member");
    assert_eq!(kind("colour_images.downsample_threshold", SettingInput::Real(f64::NAN)), "non_finite");
    assert_eq!(kind("colour_images.downsample_threshold", SettingInput::Real(f64::INFINITY)), "non_finite");
    assert_eq!(kind("fonts.never_embed", SettingInput::Names(&["Inter", ""])), "font_name_invalid");
    assert_eq!(kind("fonts.never_embed", SettingInput::Names(&["In\nter"])), "font_name_invalid");
    assert_eq!(kind("output_intent.output_intent_profile", SettingInput::Text("FOGRA\u{0}39")), "text_invalid");
    for q in [
        &[("Q Factor", QualityInput::Number(0.1))][..],
        &[("QFactor", QualityInput::Number(f64::NAN))][..],
        &[("QFactor", QualityInput::Number(0.1)), ("QFactor", QualityInput::Number(0.2))][..],
        &[("", QualityInput::Bool(true))][..],
        &[("Blend", QualityInput::Name("a b"))][..],
        &[("HSamples", QualityInput::Numbers(&[1.0, f64::INFINITY]))][..],
    ] {
        assert_eq!(kind("greyscale_images.auto_colour_space_quality_dictionary", SettingInput::Quality(q)), "quality_dictionary_invalid", "{q:?}");
    }
    let e = build(&[("fonts.never_embed", SettingInput::Names(&["secret\u{7}"]))]).unwrap_err();
    assert_eq!(e.to_string(), "font_name_invalid: fonts.never_embed", "the refusal names the setting, not the caller text");
    let mut op = operator(&[]);
    assert_eq!(op.set("general.nope", SettingInput::Bool(true)).unwrap_err().code(), "unknown_setting");
    assert_eq!(op.set("general.thumbnails", SettingInput::Integer(1)).unwrap_err().code(), "wrong_value_kind");
}

/// V4-07: every test the DESCRIPTOR `pdf_presets` entry and the module documentation cite exists.
#[test]
fn descriptor_cites_existing_preset_tests() {
    let d = DESCRIPTOR;
    let start = d.find("\"pdf_presets\":").expect("DESCRIPTOR lacks pdf_presets");
    let section = &d[start..];
    let source = include_str!("pdf_presets.rs");
    let mut cited = 0;
    for chunk in section.split("(test").skip(1) {
        let list = chunk.trim_start_matches('s').split(')').next().unwrap();
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            assert!(source.contains(&format!("fn {name}()")), "cited test {name} does not exist");
            cited += 1;
        }
    }
    assert!(cited >= 12, "found {cited} citations");
    for api in ["PdfPreset::from_draft", "PdfPreset::resolve", "PdfPreset::font_receipt", "Descriptions::render", "preset_outcome_for"] {
        assert!(d.contains(&format!("\"{api}\"")), "api {api} not listed");
    }
}
