//! Stock PDF presets, save/load and real export (lane SMB-PDF-STOCK-PRESETS-1, MT-30842,
//! STU-LAY-168): the canonical codec round trip of the three writable stock presets against an
//! independent copy of their table columns, the hostile-text cases (V4-01/02/08), the typed codec
//! refusals, and a real one-page export per writable preset read back by press-read.
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_press::pdf_preset_codec::{decode, encode};
use hsk_studio_press::pdf_stock_presets as stock;
use hsk_studio_press::*;

/// Independent oracle: the three writable STU-LAY-159 columns (High Quality Print, Press Quality,
/// Smallest File Size), each cell in the codec's value form.
const COLUMNS: [(&str, [&str; 3]); 19] = [
    ("general.compatibility_level", ["enum \"1.4\"", "enum \"1.4\"", "enum \"1.5\""]),
    ("colour_management.colour_conversion_strategy", ["enum \"LeaveColorUnchanged\"", "enum \"CMYK\"", "enum \"sRGB\""]),
    ("colour_images.target_resolution", ["int 300", "int 300", "int 100"]),
    ("colour_images.downsample_threshold", ["real 1.5"; 3]),
    ("greyscale_images.target_resolution", ["int 300", "int 300", "int 150"]),
    ("monochrome_images.target_resolution", ["int 1200", "int 1200", "int 300"]),
    ("colour_images.filter", ["enum \"DCTEncode\""; 3]),
    ("colour_images.auto_filter", ["bool true"; 3]),
    ("fonts.embed_all_fonts", ["bool true", "bool true", "bool false"]),
    ("fonts.subset_fonts", ["bool true"; 3]),
    ("fonts.max_subset_percent", ["int 100"; 3]),
    ("general.optimisation", ["bool true"; 3]),
    ("general.compress_objects", ["enum \"Tags\"", "enum \"Tags\"", "enum \"All\""]),
    ("general.auto_rotate_pages", ["enum \"All\"", "enum \"None\"", "enum \"All\""]),
    ("general.allow_transparency", ["bool false"; 3]),
    ("fonts.cannot_embed_font_policy", ["enum \"Warning\"", "enum \"Error\"", "enum \"Warning\""]),
    ("colour_management.preserve_overprint", ["bool true"; 3]),
    ("standards_compliance.compliance_check", ["enum \"none\""; 3]),
    ("standards_compliance.standard_compliant_only", ["bool false"; 3]),
];
const A4: [f64; 4] = [0.0, 0.0, 595.276, 841.89];

fn writable() -> Vec<PdfPreset> {
    [stock::HIGH_QUALITY_PRINT, stock::PRESS_QUALITY, stock::SMALLEST_FILE_SIZE]
        .iter()
        .map(|id| stock_preset(id).unwrap().to_preset().unwrap())
        .collect()
}
fn press_quality_text() -> String {
    encode(&stock_preset(stock::PRESS_QUALITY).unwrap().to_preset().unwrap())
}
/// Replaces the line starting with `prefix` (exactly one) by `with` (`None` drops it).
fn edit_line(text: &str, prefix: &str, with: Option<&str>) -> String {
    assert_eq!(text.lines().filter(|l| l.starts_with(prefix)).count(), 1, "{prefix}");
    text.lines()
        .filter_map(|l| if l.starts_with(prefix) { with.map(str::to_owned) } else { Some(l.to_owned()) })
        .map(|l| l + "\n")
        .collect()
}
fn err(text: &str) -> CodecError {
    decode(text.as_bytes()).unwrap_err()
}
fn line_of(text: &str, prefix: &str) -> usize {
    text.lines().position(|l| l.starts_with(prefix)).unwrap() + 1
}

/// MT-30842 / STU-LAY-168: each writable stock preset survives encode -> decode with every setting,
/// description, provenance and the compliance token; the text is deterministic and its rows equal
/// the independent table columns. A renamed operator copy keeps its compliance token and parent.
#[test]
fn pdf_stock_preset_matrix_roundtrip() {
    for (i, p) in writable().iter().enumerate() {
        let text = encode(p);
        assert_eq!(text, encode(p), "deterministic");
        assert!(text.starts_with("hsk.studio.pdf_preset@1\n") && text.ends_with("\nend\n"));
        let back = decode(text.as_bytes()).unwrap();
        assert_eq!(&back, p, "{}", p.name());
        assert_eq!(encode(&back), text);
        assert_eq!(back.source(), &PresetSource::Shipped);
        assert_eq!(back.page_range(), (1, None));
        for (key, cells) in COLUMNS {
            let line = format!("setting {key} {}", cells[i]);
            assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{}: {line}", p.name());
        }
        assert_eq!(text.lines().filter(|l| l.starts_with("setting ")).count(), SETTINGS.len());
    }
    let pq = &writable()[1];
    let renamed = pq.edit_copy("operator.press-renamed", "PDF/X-4:2007").unwrap();
    let back = decode(encode(&renamed).as_bytes()).unwrap();
    assert_eq!(back, renamed);
    assert_eq!((back.name(), back.standards().compliance_check), ("PDF/X-4:2007", ComplianceCheck::None));
    assert_eq!(back.source(), &PresetSource::Operator { parent: Some(stock::PRESS_QUALITY.to_owned()) });
    // Every value kind and every provenance survives.
    let mut op = renamed.edit_copy("operator.kinds", "Kinds").unwrap();
    op.set("fonts.always_embed", SettingInput::Names(&["Inter", "Noto \"Sans\" \\ SC"])).unwrap();
    op.set("fonts.never_embed", SettingInput::Names(&["Symbol"])).unwrap();
    let q = [
        ("QFactor", QualityInput::Number(-0.0)),
        ("Blend", QualityInput::Integer(-3)),
        ("ColorTransform", QualityInput::Bool(true)),
        ("Method", QualityInput::Name("/Bicubic")),
        ("HSamples", QualityInput::Numbers(&[2.0, 1.0e-7, 1.0e300, 0.1])),
    ];
    op.set("greyscale_images.base_quality_dictionary", SettingInput::Quality(&q)).unwrap();
    op.set("output_intent.output_intent_profile", SettingInput::Text("Coated \"FOGRA39\"")).unwrap();
    op.set("colour_images.downsample_threshold", SettingInput::Real(1.234_567_890_123)).unwrap();
    assert_eq!(decode(encode(&op).as_bytes()).unwrap(), op);
    let draft_values: Vec<(String, SettingValue)> = op.settings().map(|(s, v)| (s.setting().key(), v.clone())).collect();
    for source in [SourceInput::Imported { origin: "joboptions:Press Quality.joboptions" }, SourceInput::Operator { parent: None }] {
        let head = PresetDraft {
            id: "imported.x",
            name: "X",
            descriptions: &[("en_US", "line one\nline two\ttab"), ("de", "Druck \u{7} bell"), ("fr_CA", "")],
            default_locale: "de",
            source,
            settings: &[],
        };
        let p = PdfPreset::from_values(&head, &draft_values).unwrap();
        assert_eq!(decode(encode(&p).as_bytes()).unwrap(), p);
    }
}

/// V4-08 (escape): escapes are decoded before anything is compared; escaped text never becomes
/// structure; malformed escapes refuse typed.
#[test]
fn codec_adversarial_escape() {
    let pq = writable().remove(1);
    let mut op = pq.edit_copy("operator.escape", "Quote \" and \\ back").unwrap();
    op.set("output_intent.output_intent_profile", SettingInput::Text("\"\nsetting general.thumbnails bool true\nend")).unwrap_err();
    op.set("output_intent.output_intent_profile", SettingInput::Text("x\" setting general.thumbnails bool true \"")).unwrap();
    let text = encode(&op);
    assert_eq!(text.lines().filter(|l| l.starts_with("setting general.thumbnails ")).count(), 1);
    assert_eq!(decode(text.as_bytes()).unwrap(), op);
    // A description carrying a newline and a decoy directive stays one quoted token.
    let head = PresetDraft {
        id: "operator.desc",
        name: "D",
        descriptions: &[("en", "a\nsetting general.thumbnails bool true\nend\n# x")],
        default_locale: "en",
        source: SourceInput::Operator { parent: None },
        settings: &[],
    };
    let values: Vec<(String, SettingValue)> = pq.settings().map(|(s, v)| (s.setting().key(), v.clone())).collect();
    let p = PdfPreset::from_values(&head, &values).unwrap();
    let text = encode(&p);
    assert_eq!(text.lines().filter(|l| l.starts_with("description ")).count(), 1);
    assert_eq!(decode(text.as_bytes()).unwrap(), p);
    // Hand-written escapes decode first: \u{48} is `H`.
    let base = press_quality_text();
    let t = edit_line(&base, "name ", Some("name \"\\u{50}ress\\u{20}Quality\""));
    assert_eq!(decode(t.as_bytes()).unwrap().name(), "Press Quality");
    let t = edit_line(&base, "setting colour_images.filter ", Some("setting colour_images.filter enum \"\\u{43}CITTFaxEncode\""));
    assert_eq!(err(&t), CodecError::Preset(PresetError::FilterNotAdmittedForGroup(SettingRef { group: PdfGroup::ColourImages, name: "filter" })));
    for bad in ["\\x41", "\\u{}", "\\u{110000}", "\\u{d800}", "\\u{1234567}", "\\u41", "\\"] {
        let t = edit_line(&base, "name ", Some(&format!("name \"P{bad}\"")));
        assert_eq!(err(&t).code(), "codec_syntax", "{bad:?}");
    }
    let t = edit_line(&base, "name ", Some("name \"unterminated"));
    assert_eq!(err(&t), CodecError::Syntax { line: line_of(&base, "name ") });
    let t = edit_line(&base, "name ", Some("name \"raw\u{7}bell\""));
    assert_eq!(err(&t).code(), "codec_syntax", "a raw control character inside quotes");
    let t = edit_line(&base, "name ", Some("name \"a\"b"));
    assert_eq!(err(&t).code(), "codec_syntax", "a quote must end its token");
    // Keys are bare tokens: a quoted key is not matched against the table.
    let t = edit_line(&base, "setting general.thumbnails ", Some("setting \"general.thumbnails\" bool false"));
    assert_eq!(err(&t).code(), "codec_syntax");
}

/// V4-08 (comment / decoy): comment lines are skipped whole; a decoy never counts as a setting.
#[test]
fn codec_adversarial_comment_decoy() {
    let base = press_quality_text();
    let pq = decode(base.as_bytes()).unwrap();
    let mut with_comments = String::new();
    for l in base.lines() {
        with_comments.push_str(l);
        with_comments.push('\n');
        if l.starts_with("setting general.thumbnails ") {
            with_comments.push_str("# setting general.thumbnails bool true\n   #end\n\n");
        }
    }
    with_comments.push_str("# trailing comment after end\n");
    assert_eq!(decode(with_comments.as_bytes()).unwrap(), pq);
    // The real line removed, a commented decoy left: the setting is missing.
    let t = edit_line(&base, "setting general.thumbnails ", Some("# setting general.thumbnails bool false"));
    assert_eq!(err(&t), CodecError::MissingSetting(SettingRef { group: PdfGroup::General, name: "thumbnails" }));
    // `#` inside a quoted string is text, not a comment.
    let t = edit_line(&base, "name ", Some("name \"# Press\""));
    assert_eq!(decode(t.as_bytes()).unwrap().name(), "# Press");
    // A trailing `#` on a structural line is not a comment: extra tokens refuse.
    let t = edit_line(&base, "setting general.thumbnails ", Some("setting general.thumbnails bool false # note"));
    assert_eq!(err(&t).code(), "codec_syntax");
    // Nothing but comments after `end`.
    let t = format!("{base}setting general.thumbnails bool true\n");
    assert_eq!(err(&t).code(), "codec_syntax");
    // The header cannot be commented or preceded.
    assert_eq!(err(&format!("# c\n{base}")), CodecError::BadHeader);
}

/// V4-08 (case / whitespace): tokens compare case-sensitively; blank runs, tabs and CRLF between
/// tokens are separators; whitespace inside a quoted string is kept exactly.
#[test]
fn codec_adversarial_case_whitespace() {
    let base = press_quality_text();
    let pq = decode(base.as_bytes()).unwrap();
    let spaced: String = base.lines().map(|l| if l.starts_with("setting ") { format!("  {}\t \r\n", l.replacen(' ', " \t  ", 3)) } else { format!("{l}\n") }).collect();
    assert_eq!(decode(spaced.as_bytes()).unwrap(), pq);
    let crlf = base.replace('\n', "\r\n").replacen("\r\n", "\n", 1);
    assert_eq!(decode(crlf.as_bytes()).unwrap(), pq);
    assert_eq!(err(&base.replacen('\n', "\r\n", 1)), CodecError::BadHeader, "the header line is exact");
    let thumbs = "setting general.thumbnails ";
    let ln = line_of(&base, thumbs);
    for (line, expected) in [
        ("Setting general.thumbnails bool false", CodecError::UnknownDirective { line: ln }),
        ("SETTING general.thumbnails bool false", CodecError::UnknownDirective { line: ln }),
        ("setting General.thumbnails bool false", CodecError::UnknownKey { line: ln }),
        ("setting general.Thumbnails bool false", CodecError::UnknownKey { line: ln }),
        ("setting general.thumbnails bool False", CodecError::Syntax { line: ln }),
        ("setting general.thumbnails Bool false", CodecError::Preset(PresetError::WrongValueKind(SettingRef { group: PdfGroup::General, name: "thumbnails" }))),
    ] {
        assert_eq!(err(&edit_line(&base, thumbs, Some(line))), expected, "{line}");
    }
    let t = edit_line(&base, "setting colour_images.filter ", Some("setting colour_images.filter enum \"dctencode\""));
    assert_eq!(err(&t).code(), "unknown_member");
    let t = edit_line(&base, "setting colour_images.filter ", Some("setting colour_images.filter enum \" DCTEncode\""));
    assert_eq!(err(&t).code(), "unknown_member", "whitespace inside quotes is significant");
    for (prefix, bad) in [
        ("setting colour_images.target_resolution ", "setting colour_images.target_resolution int 0300"),
        ("setting colour_images.target_resolution ", "setting colour_images.target_resolution int +300"),
        ("setting colour_images.downsample_threshold ", "setting colour_images.downsample_threshold real 1.50"),
        ("setting colour_images.downsample_threshold ", "setting colour_images.downsample_threshold real 1.5e0"),
    ] {
        assert_eq!(err(&edit_line(&base, prefix, Some(bad))).code(), "codec_syntax", "{bad}");
    }
    let t = edit_line(&base, "name ", Some("name \" Press  Quality \""));
    assert_eq!(decode(t.as_bytes()).unwrap().name(), " Press  Quality ");
}

/// MT-30842: duplicate key, unknown key, missing setting, over-long value and the head-field and
/// contract refusals, typed; codes only through Observe.
#[test]
fn codec_typed_refusals() {
    let base = press_quality_text();
    let thumbs = "setting general.thumbnails ";
    let ln = line_of(&base, thumbs);
    let dup = base.replacen(thumbs, &format!("{thumbs}bool false\n{thumbs}"), 1);
    assert_eq!(err(&dup), CodecError::DuplicateKey { line: ln + 1 });
    assert_eq!(err(&dup).code(), "codec_duplicate_key");
    let t = edit_line(&base, thumbs, Some("setting general.thumbnail bool false"));
    assert_eq!(err(&t), CodecError::UnknownKey { line: ln });
    let t = edit_line(&base, thumbs, Some("setting security.encrypt bool false"));
    assert_eq!(err(&t).code(), "codec_unknown_key");
    let t = edit_line(&base, "setting standards_compliance.compliance_check ", None);
    let missing = err(&t);
    assert_eq!(missing, CodecError::MissingSetting(SettingRef { group: PdfGroup::StandardsCompliance, name: "compliance_check" }));
    assert_eq!(missing.to_string(), "codec_missing_setting: standards_compliance.compliance_check");
    let long = "x".repeat(MAX_CODEC_VALUE_BYTES + 1);
    let profile = "setting output_intent.output_intent_profile ";
    let t = edit_line(&base, profile, Some(&format!("{profile}text \"{long}\"")));
    assert_eq!(err(&t), CodecError::ValueTooLong { line: line_of(&base, profile) });
    assert!(!err(&t).to_string().contains("xxx"), "a refusal never echoes caller text");
    let t = edit_line(&base, profile, Some(&format!("{profile}text \"{}\"", "p".repeat(MAX_TEXT_BYTES + 1))));
    assert_eq!(err(&t).code(), "text_invalid");
    let t = edit_line(&base, thumbs, Some("setting general.thumbnails int 1"));
    assert_eq!(err(&t).code(), "wrong_value_kind");
    let t = base.replacen("\nname ", "\nid \"again\"\nname ", 1);
    assert_eq!(err(&t), CodecError::DuplicateField { line: 3 });
    let t = edit_line(&base, "id ", None);
    assert_eq!(err(&t), CodecError::MissingField { field: "id" });
    let t = edit_line(&base, "end", None);
    assert_eq!(err(&t), CodecError::MissingField { field: "end" });
    let t = edit_line(&base, "source ", Some("source vendor"));
    assert_eq!(err(&t).code(), "codec_syntax");
    let t = edit_line(&base, "source ", Some("source imported \"\""));
    assert_eq!(err(&t).code(), "invalid_origin");
    let t = edit_line(&base, "description ", None);
    assert_eq!(err(&t).code(), "no_descriptions");
    let t = edit_line(&base, "end", Some("finish"));
    assert_eq!(err(&t).code(), "codec_unknown_directive");
    let t = edit_line(&base, "setting standards_compliance.compliance_check ", Some("setting standards_compliance.compliance_check enum \"PDF/X-4:2007\""));
    assert_eq!(err(&t), CodecError::Preset(PresetError::StandardWithoutOutputIntent));
    // Codes only through Observe.
    assert_eq!(codec_outcome_for(&decode(base.as_bytes())), Outcome::Success);
    assert_eq!(codec_outcome_for(&decode(dup.as_bytes())), Outcome::Failure(FailureCode::Validation));
    assert_eq!(codec_outcome_for::<()>(&Err(CodecError::Preset(PresetError::StockOutputIntentNotCaptured))), Outcome::Failure(FailureCode::Unsupported));
}

/// V4-01/02: input size, line count, line length, token count and declared list counts are
/// checked before anything is allocated for them.
#[test]
fn codec_hostile_sizes() {
    assert_eq!(decode(&vec![b'#'; MAX_CODEC_BYTES + 1]).unwrap_err(), CodecError::TooLarge);
    assert_eq!(decode(&[0xFF, 0xFE]).unwrap_err(), CodecError::NotUtf8);
    assert_eq!(decode(b"").unwrap_err(), CodecError::BadHeader);
    let many = format!("{CODEC_HEADER}\n{}", "#\n".repeat(MAX_CODEC_LINES));
    assert_eq!(err(&many), CodecError::TooManyLines);
    let wide = format!("{CODEC_HEADER}\n#{}\n", "x".repeat(MAX_CODEC_LINE_BYTES));
    assert_eq!(err(&wide), CodecError::LineTooLong { line: 2 });
    let base = press_quality_text();
    let thumbs = "setting general.thumbnails ";
    let ln = line_of(&base, thumbs);
    let t = edit_line(&base, thumbs, Some(&format!("setting general.thumbnails{}", " a".repeat(MAX_CODEC_TOKENS))));
    assert_eq!(err(&t), CodecError::TooManyTokens { line: ln });
    let fonts = "setting fonts.always_embed ";
    for n in ["513", "9223372036854775807"] {
        let t = edit_line(&base, fonts, Some(&format!("{fonts}fonts {n} \"Inter\"")));
        assert_eq!(err(&t).code(), "list_too_long", "{n}");
    }
    let t = edit_line(&base, fonts, Some(&format!("{fonts}fonts 2 \"Inter\"")));
    assert_eq!(err(&t).code(), "codec_syntax", "the declared count must match");
    let t = edit_line(&base, fonts, Some(&format!("{fonts}fonts -1")));
    assert_eq!(err(&t).code(), "codec_syntax");
    let acs = "setting colour_images.auto_colour_space_quality_dictionary ";
    let t = edit_line(&base, acs, Some(&format!("{acs}quality 33")));
    assert_eq!(err(&t).code(), "list_too_long");
    let t = edit_line(&base, acs, Some(&format!("{acs}quality 1 \"HSamples\" nums 17")));
    assert_eq!(err(&t).code(), "quality_dictionary_invalid");
    let t = edit_line(&base, acs, Some(&format!("{acs}quality 1 \"HSamples\" nums 3 1.0 1.0")));
    assert_eq!(err(&t).code(), "codec_syntax");
    let t = edit_line(&base, acs, Some(&format!("{acs}quality 2 \"QFactor\" num 0.15")));
    assert_eq!(err(&t).code(), "codec_syntax");
    let descs: String = (0..=MAX_LOCALES).map(|i| format!("description \"l{}{}\" \"x\"\n", char::from(b'a' + (i / 26) as u8), char::from(b'a' + (i % 26) as u8))).collect();
    let t = base.replacen("description ", &format!("{descs}description "), 1);
    assert_eq!(err(&t).code(), "too_many_locales");
}

/// MT-30842 step 1: each writable stock preset resolves onto press-write and exports a real
/// one-page PDF whose header press-read reads back at the preset's level (1.4, 1.4, 1.5).
#[test]
fn stock_presets_export_real_pdf() {
    let cancel = CancellationToken::default();
    for (p, version) in writable().iter().zip(["1.4", "1.4", "1.5"]) {
        let res = p.resolve(ResolveRequest { info_title: Some("Stock proof"), ..ResolveRequest::default() }).unwrap();
        assert_eq!((res.options.version, res.options.subset_fonts), (version, true));
        assert!(res.options.output_intent.is_none() && res.options.pdfx4.is_none() && !res.options.pdfa2b);
        assert_eq!(res.receipt.note("fonts.subset_fonts"), Some(ReceiptNote::NameKeyedCffEmbeddedWhole));
        let mut ex = PdfExport::new(res.options, &Limits::default()).unwrap();
        let page = ex.add_page(PageSpec { media_box_pt: A4, trim_box_pt: None, bleed_box_pt: None }).unwrap();
        let square = [PathCommand::MoveTo(10.0, 10.0), PathCommand::LineTo(50.0, 10.0), PathCommand::LineTo(50.0, 50.0), PathCommand::Close];
        ex.fill_path(page, &square, FillRule::NonZero, Colour::DeviceGray(0.0)).unwrap();
        let (bytes, _) = ex.finish(&cancel).unwrap();
        assert!(bytes.starts_with(format!("%PDF-{version}").as_bytes()), "{}", p.name());
        let read = PdfDoc::open(&bytes, &Limits::default(), &cancel).unwrap().inspect();
        assert_eq!((read.version.as_str(), read.page_count), (version, 1), "{}", p.name());
        let (start, end) = p.page_range();
        assert_eq!(end.unwrap_or(read.page_count), 1, "the nullable end selects the final page");
        assert_eq!(start, 1);
    }
    // The PDF/X stock presets never reach the writer.
    for id in [stock::PDF_X_1A_2001, stock::PDF_X_3_2002, stock::PDF_X_4] {
        assert_eq!(stock_preset(id).unwrap().to_preset().unwrap_err(), PresetError::StockOutputIntentNotCaptured);
    }
}
