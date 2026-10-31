//! Image > Mode (lane SMB-PS-MODE-CONVERT-1; STU-COL-106/107, STU-COL-118..121). Test names carry
//! the MT recipe names: `mt_31174_*` enumerations, legality and the discard receipt;
//! `mt_31175_*` bitmap / indexed records and conversions; `mt_31176_*` bitmap parameter rows;
//! `mt_31177_*` indexed parameter rows, colour table and its file codec; `mt_31178_*` depth rules
//! and per-operation availability; `mt_31179_*` the depth matrix.

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};
use hsk_studio_prism::mode::*;
use hsk_studio_prism::{DESCRIPTOR, MODE_CONVERSION_DESCRIPTOR};

fn code<T: std::fmt::Debug>(r: Result<T, ModeError>) -> &'static str {
    r.expect_err("expected a refusal").code()
}

const RGB: ChannelSet = ChannelSet { colour_channels: 3, alpha_channels: 0, spot_channels: 0 };

// ---------------------------------------------------------------- slice 1: contract (MT-31174, MT-31176/31177 rows, MT-31178, MT-31179)

#[test]
fn mt_31174_enumerations_match_stu_col_118() {
    let modes: Vec<&str> = DocumentMode::ALL.iter().map(|m| m.token()).collect();
    assert_eq!(modes, ["grayscale", "rgb", "cmyk", "lab", "bitmap", "indexed", "multichannel", "duotone"]);
    let new: Vec<&str> = NewDocumentMode::ALL.iter().map(|m| m.token()).collect();
    assert_eq!(new, ["grayscale", "rgb", "cmyk", "lab", "bitmap"]);
    for m in NewDocumentMode::ALL {
        assert_eq!(NewDocumentMode::try_from_mode(m.mode()), Ok(*m));
    }
    for m in [DocumentMode::Indexed, DocumentMode::Multichannel, DocumentMode::Duotone] {
        assert_eq!(code(NewDocumentMode::try_from_mode(m)), "not_a_creation_mode");
    }
    let ops: Vec<&str> = ChangeMode::ALL.iter().map(|c| c.token()).collect();
    assert_eq!(ops, ["to_grayscale", "to_rgb", "to_cmyk", "to_lab", "to_bitmap", "to_indexed", "to_multichannel"]);
    assert!(ChangeMode::ALL.iter().all(|c| c.target() != DocumentMode::Duotone));
    for c in ChangeMode::ALL {
        assert_eq!(ChangeMode::from_token(c.token()), Ok(*c));
    }
    assert_eq!(code(ChangeMode::from_token("to_duotone")), "to_duotone_not_a_conversion");
    assert_eq!(code(ChangeMode::from_token("TO_RGB")), "unknown_mode_token");
    assert_eq!(code(DocumentMode::from_token(" rgb")), "unknown_mode_token");
    assert_eq!(DocumentMode::from_token("duotone"), Ok(DocumentMode::Duotone));
}

#[test]
fn mt_31174_channel_discard_requires_the_echoed_receipt() {
    let channels = ChannelSet { colour_channels: 3, alpha_channels: 2, spot_channels: 1 };
    let plan = plan_mode_change(DocumentMode::Rgb, ChangeMode::ToIndexed, BitsPerChannel::Eight, channels).unwrap();
    assert_eq!((plan.to, plan.to_depth), (DocumentMode::Indexed, BitsPerChannel::Eight));
    assert_eq!(plan.discard.alpha_channels_lost, 2);
    assert_eq!(plan.discard.spot_channels_lost, 1);
    assert!(plan.discard.palette_quantised);
    assert!(plan.requires_confirmation());
    assert_eq!(code(plan.confirm(None)), "discard_not_confirmed");
    let wrong = DiscardReport { alpha_channels_lost: 1, ..plan.discard };
    assert_eq!(code(plan.confirm(Some(&wrong))), "discard_not_confirmed");
    let confirmed = plan.confirm(Some(&plan.discard)).unwrap();
    assert_eq!(confirmed.plan(), &plan);

    let gray = plan_mode_change(DocumentMode::Rgb, ChangeMode::ToGrayscale, BitsPerChannel::Sixteen, RGB).unwrap();
    assert_eq!(gray.discard.colour_channels_lost, 2);
    assert_eq!(gray.to_depth, BitsPerChannel::Sixteen);
    assert_eq!(code(gray.confirm(None)), "discard_not_confirmed");

    let cmyk = plan_mode_change(DocumentMode::Rgb, ChangeMode::ToCmyk, BitsPerChannel::Eight, RGB).unwrap();
    assert!(!cmyk.requires_confirmation());
    assert!(cmyk.confirm(None).is_ok());
    let multi = plan_mode_change(DocumentMode::Rgb, ChangeMode::ToMultichannel, BitsPerChannel::Eight, RGB).unwrap();
    assert!(multi.discard.composite_view_lossy && !multi.requires_confirmation());
}

#[test]
fn mt_31174_illegal_changes_are_refused_typed() {
    let g = ChannelSet { colour_channels: 1, alpha_channels: 0, spot_channels: 0 };
    let k = ChannelSet { colour_channels: 4, alpha_channels: 0, spot_channels: 0 };
    let p = plan_mode_change;
    assert_eq!(code(p(DocumentMode::Rgb, ChangeMode::ToRgb, BitsPerChannel::Eight, RGB)), "same_mode");
    assert_eq!(code(p(DocumentMode::Rgb, ChangeMode::ToCmyk, BitsPerChannel::ThirtyTwo, RGB)), "depth_32_unavailable");
    assert_eq!(code(p(DocumentMode::Rgb, ChangeMode::ToLab, BitsPerChannel::ThirtyTwo, RGB)), "depth_32_unavailable");
    assert_eq!(code(p(DocumentMode::Rgb, ChangeMode::ToIndexed, BitsPerChannel::Sixteen, RGB)), "indexed_requires_rgb_8_bit_source");
    assert_eq!(code(p(DocumentMode::Cmyk, ChangeMode::ToIndexed, BitsPerChannel::Eight, k)), "indexed_requires_rgb_8_bit_source");
    assert_eq!(code(p(DocumentMode::Cmyk, ChangeMode::ToBitmap, BitsPerChannel::Eight, k)), "bitmap_requires_grayscale_source");
    assert_eq!(code(p(DocumentMode::Grayscale, ChangeMode::ToBitmap, BitsPerChannel::ThirtyTwo, g)), "bitmap_requires_8_or_16_bit_source");
    assert_eq!(code(p(DocumentMode::Bitmap, ChangeMode::ToRgb, BitsPerChannel::One, g)), "bitmap_converts_only_to_grayscale");
    assert_eq!(code(p(DocumentMode::Cmyk, ChangeMode::ToRgb, BitsPerChannel::ThirtyTwo, k)), "depth_32_unavailable");
    assert_eq!(code(p(DocumentMode::Rgb, ChangeMode::ToCmyk, BitsPerChannel::One, RGB)), "one_bit_bitmap_only");
    assert_eq!(code(p(DocumentMode::Rgb, ChangeMode::ToCmyk, BitsPerChannel::Eight, k)), "channel_set_mismatch");
    let m2 = ChannelSet { colour_channels: 2, alpha_channels: 0, spot_channels: 0 };
    assert_eq!(code(p(DocumentMode::Multichannel, ChangeMode::ToRgb, BitsPerChannel::Eight, m2)), "multichannel_channel_count");
    let mg = p(DocumentMode::Multichannel, ChangeMode::ToGrayscale, BitsPerChannel::Eight, m2).unwrap();
    assert_eq!(mg.discard.colour_channels_lost, 1);
    let bg = p(DocumentMode::Bitmap, ChangeMode::ToGrayscale, BitsPerChannel::One, g).unwrap();
    assert_eq!(bg.to_depth, BitsPerChannel::Eight);
    let gb = p(DocumentMode::Grayscale, ChangeMode::ToBitmap, BitsPerChannel::Sixteen, ChannelSet { alpha_channels: 1, ..g }).unwrap();
    assert_eq!(gb.to_depth, BitsPerChannel::One);
    assert!(gb.discard.tonal_precision_lost && gb.discard.alpha_channels_lost == 1 && gb.requires_confirmation());
}

#[derive(Default)]
struct Collector(Vec<Vec<u8>>);
impl SinkPort for Collector {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn mt_31174_outcomes_go_through_observe_as_codes_only() {
    let ctx = ModeObserveContext {
        correlation_id: 7,
        revision: 3,
        resource: DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000003").unwrap(),
        actor: ActorContext::new("account", "principal", "owner-account", "owner-principal", "space", "session").unwrap(),
    };
    let mut sink = Collector::default();
    let ok = emit_mode_outcome(&ctx, Ok(()), &CancellationToken::default(), &mut sink).unwrap();
    assert_eq!(ok.outcome, Outcome::Success);
    let err = emit_mode_outcome(&ctx, Err(ModeError::PaletteTableNotCaptured), &CancellationToken::default(), &mut sink).unwrap();
    assert_eq!(err.outcome, Outcome::Failure(FailureCode::Unsupported));
    let bad = emit_mode_outcome(&ctx, Err(ModeError::DiscardNotConfirmed), &CancellationToken::default(), &mut sink).unwrap();
    assert_eq!(bad.outcome, Outcome::Failure(FailureCode::Validation));
    assert_eq!(sink.0.len(), 3);
    for frame in &sink.0 {
        let text = String::from_utf8_lossy(frame);
        assert!(!text.contains("palette") && !text.contains("discard") && !text.contains("mode"), "no text on the wire");
    }
}

#[test]
fn mt_31176_bitmap_rows_store_seven_separate_fields() {
    let expect = [
        ("bitmap.resolution", Some(72.0), "ppi"),
        ("bitmap.frequency", None, "lines_per_inch"),
        ("bitmap.angle", None, "degrees"),
        ("bitmap.threshold", None, "percent"),
    ];
    for (row, (key, default, unit)) in BITMAP_PARAM_ROWS.iter().zip(expect) {
        assert_eq!(row.key, key);
        assert_eq!((row.hard_min, row.hard_max, row.soft_min, row.soft_max), (None, None, None, None), "{key}");
        assert_eq!((row.default, row.unit, row.precision), (default, Some(unit), None), "{key}");
        let j = row.to_json();
        for f in ["hard_min", "hard_max", "soft_min", "soft_max", "precision"] {
            assert!(j.contains(&format!("\"{f}\":\"UNKNOWN\"")), "{j}");
        }
        // No clamp on an UNKNOWN side.
        assert_eq!(row.check(1.0e9), Ok(1.0e9));
        assert_eq!(row.check(-1.0e9), Ok(-1.0e9));
        assert_eq!(row.check(f64::INFINITY), Err(ModeError::NonFiniteParameter(key)));
    }
    assert!(BITMAP_RESOLUTION.to_json().contains("\"default\":72.0"));
    assert!(BITMAP_FREQUENCY.to_json().contains("\"default\":\"UNKNOWN\""));
}

#[test]
fn mt_31177_indexed_rows_store_seven_separate_fields() {
    let c = INDEXED_COLORS;
    assert_eq!((c.key, c.hard_min, c.hard_max, c.soft_min, c.soft_max, c.default, c.unit, c.precision), ("indexed.colors", None, Some(256.0), None, None, None, Some("count"), Some(0)));
    let d = INDEXED_DITHER_AMOUNT;
    assert_eq!((d.key, d.hard_min, d.hard_max, d.soft_min, d.soft_max, d.default, d.unit, d.precision), ("indexed.dither_amount", Some(1.0), Some(100.0), None, None, None, Some("percent"), Some(0)));
    assert!(c.to_json().contains("\"hard_min\":\"UNKNOWN\"") && c.to_json().contains("\"hard_max\":256.0"));
    assert_eq!(c.check(257.0), Err(ModeError::ParameterOutOfRange("indexed.colors")));
    assert_eq!(c.check(-3.0), Ok(-3.0), "hard_min is not declared: no guessed minimum");
    assert_eq!(code(d.check(0.0)), "parameter_out_of_range");
    assert_eq!(code(d.check(101.0)), "parameter_out_of_range");
    assert_eq!(INDEXED_PARAM_ROWS, [c, d]);
}

#[test]
fn mt_31178_depth_rules_and_per_operation_availability() {
    let bits: Vec<u8> = BitsPerChannel::ALL.iter().map(|b| b.bits()).collect();
    assert_eq!(bits, [1, 8, 16, 32]);
    assert!(BitsPerChannel::ALL.iter().all(|b| b.is_float_linear_unbounded() == (*b == BitsPerChannel::ThirtyTwo)));
    assert_eq!(code(BitsPerChannel::from_bits(24)), "unknown_bits_per_channel");
    for b in BitsPerChannel::ALL {
        assert_eq!(BitsPerChannel::from_bits(b.bits()), Ok(*b));
    }
    let report = change_availability(DocumentMode::Rgb, BitsPerChannel::ThirtyTwo, RGB);
    let codes: Vec<(&str, Result<u8, &str>)> = report.iter().map(|(op, r)| (op.token(), r.map(|d| d.bits()).map_err(|e| e.code()))).collect();
    assert_eq!(
        codes,
        [
            ("to_grayscale", Ok(32)),
            ("to_rgb", Err("same_mode")),
            ("to_cmyk", Err("depth_32_unavailable")),
            ("to_lab", Err("depth_32_unavailable")),
            ("to_bitmap", Err("bitmap_requires_grayscale_source")),
            ("to_indexed", Err("indexed_requires_rgb_8_bit_source")),
            ("to_multichannel", Err("depth_32_unavailable")),
        ]
    );
    let rgb8 = change_availability(DocumentMode::Rgb, BitsPerChannel::Eight, RGB);
    assert!(rgb8.iter().filter(|(_, r)| r.is_ok()).count() == 5);
}

#[test]
fn mt_31179_matrix_matches_stu_col_121() {
    use BitsPerChannel as B;
    use DocumentMode as M;
    let table: [(M, [bool; 4]); 8] = [
        (M::Bitmap, [true, false, false, false]),
        (M::Grayscale, [false, true, true, true]),
        (M::Rgb, [false, true, true, true]),
        (M::Cmyk, [false, true, true, false]),
        (M::Lab, [false, true, true, false]),
        (M::Indexed, [false, true, false, false]),
        (M::Duotone, [false, true, true, false]),
        (M::Multichannel, [false, true, true, false]),
    ];
    for (mode, row) in table {
        for (depth, want) in B::ALL.iter().zip(row) {
            assert_eq!(depth_available(mode, *depth), want, "{mode:?} {depth:?}");
            assert_eq!(check_mode_depth(mode, *depth).is_ok(), want);
        }
    }
    assert_eq!(code(check_mode_depth(M::Cmyk, B::ThirtyTwo)), "depth_32_unavailable");
    assert_eq!(code(check_mode_depth(M::Lab, B::ThirtyTwo)), "depth_32_unavailable");
    assert_eq!(code(check_mode_depth(M::Rgb, B::One)), "one_bit_bitmap_only");
    assert_eq!(code(check_mode_depth(M::Indexed, B::Sixteen)), "indexed_requires_8_bit");
    assert_eq!(code(check_mode_depth(M::Bitmap, B::Eight)), "bitmap_requires_1_bit");
}

#[test]
fn mt_31179_matrix_is_studio_normative_never_vendor() {
    for mode in DocumentMode::ALL {
        for depth in BitsPerChannel::ALL {
            let cell = depth_cell(*mode, *depth);
            assert_eq!(cell.source, AvailabilitySource::StudioNormative);
            assert_eq!(cell.source.token(), "studio_normative");
        }
    }
    assert!(MODE_CONVERSION_DESCRIPTOR.contains("\"source\":\"studio_normative\",\"vendor_derived\":false"));
    assert!(DESCRIPTOR.contains("\"mode_conversion\":\"see MODE_CONVERSION_DESCRIPTOR"));
}
