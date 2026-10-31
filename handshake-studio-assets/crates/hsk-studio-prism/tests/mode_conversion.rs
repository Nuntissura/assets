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

// ---------------------------------------------------------------- slice 2: grayscale -> bitmap (MT-31175, MT-31176)

use hsk_studio_prism::mode::bitmap::{grayscale_to_bitmap, BitmapMethod, BitmapOptions, GrayPlane, HalftoneShape, PatternTile};

fn opts(method: BitmapMethod) -> BitmapOptions {
    BitmapOptions { method, ..BitmapOptions::default() }
}

#[test]
fn mt_31175_bitmap_threshold_and_pattern_dither_values() {
    let src = [0_u8, 127, 128, 255];
    let plane = GrayPlane::Eight { width: 4, height: 1, data: &src };
    let o = BitmapOptions { threshold: Some(50.0), ..opts(BitmapMethod::HalfThreshold) };
    let r = grayscale_to_bitmap(&plane, 72.0, &o, None).unwrap();
    assert_eq!((r.plane.stride, r.plane.data.clone()), (1, vec![0xC0]));
    let src16 = [0_u16, 32_767, 32_768, 65_535];
    let r16 = grayscale_to_bitmap(&GrayPlane::Sixteen { width: 4, height: 1, data: &src16 }, 72.0, &o, None).unwrap();
    assert_eq!(r16.plane.data, vec![0xC0]);
    // 8 x 8 Bayer: tone 128/255 is white where the index is <= 31; row 0 = 0,32,8,40,2,34,10,42.
    let gray = [128_u8; 64];
    let p = grayscale_to_bitmap(&GrayPlane::Eight { width: 8, height: 8, data: &gray }, 72.0, &opts(BitmapMethod::PatternDither), None).unwrap();
    assert_eq!(p.plane.data[0], 0x55);
    assert_eq!(p.plane.black_count(), 32);
    // Width 10 pads rows to two bytes.
    let ten = [255_u8; 10];
    let t = grayscale_to_bitmap(&GrayPlane::Eight { width: 10, height: 1, data: &ten }, 72.0, &o, None).unwrap();
    assert_eq!((t.plane.stride, t.plane.data.len(), t.plane.black_count()), (2, 2, 0));
}

#[test]
fn mt_31175_bitmap_diffusion_is_deterministic_and_tone_preserving() {
    let two = [128_u8, 128];
    let d = grayscale_to_bitmap(&GrayPlane::Eight { width: 2, height: 1, data: &two }, 72.0, &BitmapOptions::default(), None).unwrap();
    // 128/255 >= 0.5 -> white, error -0.498; next 0.502 - 0.218 = 0.284 -> black.
    assert_eq!(d.plane.data, vec![0x40]);
    let quarter = [64_u8; 256];
    let plane = GrayPlane::Eight { width: 16, height: 16, data: &quarter };
    let a = grayscale_to_bitmap(&plane, 72.0, &BitmapOptions::default(), None).unwrap();
    let b = grayscale_to_bitmap(&plane, 72.0, &BitmapOptions::default(), None).unwrap();
    assert_eq!(a, b);
    let black = a.plane.black_count();
    assert!((184..=200).contains(&black), "tone 64/255 -> about 192 of 256 black, got {black}");
}

#[test]
fn mt_31175_bitmap_halftone_and_custom_pattern() {
    for shape in HalftoneShape::ALL {
        let o = BitmapOptions { frequency: Some(10.0), angle: Some(45.0), shape: Some(*shape), ..opts(BitmapMethod::HalftoneScreen) };
        let black = [0_u8; 72 * 72];
        let white = [255_u8; 72 * 72];
        let mid = [128_u8; 72 * 72];
        let run = |d: &[u8]| grayscale_to_bitmap(&GrayPlane::Eight { width: 72, height: 72, data: d }, 72.0, &o, None).unwrap().plane.black_count();
        assert_eq!(run(&black), 72 * 72, "{shape:?}");
        assert_eq!(run(&white), 0, "{shape:?}");
        let m = run(&mid);
        assert!((72 * 72 * 3 / 10..=72 * 72 * 7 / 10).contains(&m), "{shape:?} mid tone black count {m}");
    }
    let tile_data = [0_u8, 255];
    let tile = PatternTile { width: 2, height: 1, data: &tile_data };
    let o = BitmapOptions { pattern_name: Some("checker".into()), ..opts(BitmapMethod::CustomPattern) };
    let src = [128_u8; 4];
    let r = grayscale_to_bitmap(&GrayPlane::Eight { width: 4, height: 1, data: &src }, 72.0, &o, Some(tile)).unwrap();
    assert_eq!(r.plane.data, vec![0x50]);
}

#[test]
fn mt_31175_failed_conversion_preserves_the_source_and_refuses_typed() {
    let src = vec![10_u8, 20, 30, 40];
    let before = src.clone();
    let plane = GrayPlane::Eight { width: 4, height: 1, data: &src };
    let custom = opts(BitmapMethod::CustomPattern);
    assert_eq!(code(grayscale_to_bitmap(&plane, 72.0, &custom, None)), "pattern_missing");
    let named = BitmapOptions { pattern_name: Some("p".into()), ..custom.clone() };
    assert_eq!(code(grayscale_to_bitmap(&plane, 72.0, &named, None)), "pattern_missing");
    let t = [1_u8; 3];
    assert_eq!(code(grayscale_to_bitmap(&plane, 72.0, &named, Some(PatternTile { width: 2, height: 2, data: &t }))), "pattern_invalid");
    let tile_ok = PatternTile { width: 1, height: 1, data: &t[..1] };
    assert_eq!(code(grayscale_to_bitmap(&plane, 72.0, &custom, Some(tile_ok))), "pattern_missing");
    let hi = BitmapOptions { resolution: 300.0, ..BitmapOptions::default() };
    assert_eq!(code(grayscale_to_bitmap(&plane, 72.0, &hi, None)), "bitmap_resample_not_in_prism");
    assert_eq!(code(grayscale_to_bitmap(&plane, 72.0, &opts(BitmapMethod::HalfThreshold), None)), "parameter_required");
    let ht = opts(BitmapMethod::HalftoneScreen);
    assert_eq!(grayscale_to_bitmap(&plane, 72.0, &ht, None), Err(ModeError::ParameterRequired("bitmap.frequency")));
    let zero = BitmapOptions { frequency: Some(0.0), angle: Some(0.0), shape: Some(HalftoneShape::Round), ..ht.clone() };
    assert_eq!(grayscale_to_bitmap(&plane, 72.0, &zero, None), Err(ModeError::InvalidParameter("bitmap.frequency")));
    let nan = BitmapOptions { frequency: Some(f64::NAN), ..zero.clone() };
    assert_eq!(code(grayscale_to_bitmap(&plane, 72.0, &nan, None)), "non_finite_parameter");
    let no_shape = BitmapOptions { frequency: Some(10.0), shape: None, ..zero };
    assert_eq!(grayscale_to_bitmap(&plane, 72.0, &no_shape, None), Err(ModeError::ParameterRequired("bitmap.shape")));
    assert_eq!(src, before);
}

#[test]
fn mt_31175_bitmap_dims_u32_max_and_aggregate_over_budget() {
    let tiny = [0_u8; 4];
    let huge = GrayPlane::Eight { width: u32::MAX, height: u32::MAX, data: &tiny };
    assert_eq!(code(grayscale_to_bitmap(&huge, 72.0, &BitmapOptions::default(), None)), "pixel_budget_exceeded");
    let over = GrayPlane::Eight { width: 20_000, height: 20_000, data: &tiny };
    assert_eq!(code(grayscale_to_bitmap(&over, 72.0, &BitmapOptions::default(), None)), "pixel_budget_exceeded");
    let short = GrayPlane::Eight { width: 3, height: 2, data: &tiny };
    assert_eq!(code(grayscale_to_bitmap(&short, 72.0, &BitmapOptions::default(), None)), "buffer_length_mismatch");
    let empty = GrayPlane::Eight { width: 0, height: 4, data: &[] };
    assert_eq!(code(grayscale_to_bitmap(&empty, 72.0, &BitmapOptions::default(), None)), "empty_image");
}

#[test]
fn mt_31176_bitmap_option_defaults() {
    assert_eq!(BitmapOptions::default().method, BitmapMethod::DiffusionDither);
    assert_eq!(BitmapOptions::default().resolution, 72.0);
    let methods: Vec<&str> = BitmapMethod::ALL.iter().map(|m| m.token()).collect();
    assert_eq!(methods, ["half_threshold", "pattern_dither", "diffusion_dither", "halftone_screen", "custom_pattern"]);
}

#[test]
fn mt_31176_inert_method_fields_round_trip() {
    let o = BitmapOptions {
        method: BitmapMethod::HalfThreshold,
        resolution: 72.0,
        pattern_name: Some("dots \"x\"".into()),
        frequency: Some(53.0),
        angle: Some(45.0),
        shape: Some(HalftoneShape::Cross),
        threshold: Some(50.0),
    };
    let src = [0_u8, 255];
    let r = grayscale_to_bitmap(&GrayPlane::Eight { width: 2, height: 1, data: &src }, 72.0, &o, None).unwrap();
    assert_eq!(r.options, o);
    let j = r.options.to_json();
    for needle in ["\"method\":\"half_threshold\"", "\"frequency\":53.0", "\"angle\":45.0", "\"shape\":\"cross\"", "\"pattern_name\":\"dots \\\"x\\\"\""] {
        assert!(j.contains(needle), "{j} lacks {needle}");
    }
    let unset = BitmapOptions::default().to_json();
    assert!(unset.contains("\"frequency\":\"UNKNOWN\"") && unset.contains("\"shape\":null"), "{unset}");
    let tokens: Vec<&str> = HalftoneShape::ALL.iter().map(|s| s.token()).collect();
    assert_eq!(tokens, ["round", "diamond", "ellipse", "line", "square", "cross"]);
}

// ---------------------------------------------------------------- slice 3: colour table and file codec (MT-31175, MT-31177)

use hsk_studio_prism::mode::color_table::{shipped_table, ColorTable, ShippedTable, ACT_BLOCK_BYTES, ACT_TRAILER_BYTES};

#[test]
fn mt_31177_color_table_encode_decode_is_identity() {
    let t = ColorTable::new(&[RED, GREEN, BLUE], Some(1)).unwrap();
    let bytes = t.encode_act();
    assert_eq!(bytes.len(), ACT_TRAILER_BYTES);
    assert_eq!(&bytes[..9], &[255, 0, 0, 0, 255, 0, 0, 0, 255]);
    assert_eq!(&bytes[768..772], &[0, 3, 0, 1]);
    assert_eq!(ColorTable::decode_act(&bytes).unwrap(), t);
    let g = shipped_table(ShippedTable::Grayscale).unwrap();
    assert_eq!((g.len(), g.entries()[200]), (256, [200, 200, 200]));
    let gb = g.encode_act();
    assert_eq!(gb.len(), ACT_BLOCK_BYTES);
    assert_eq!(ColorTable::decode_act(&gb).unwrap(), g);
    let bw = shipped_table(ShippedTable::BlackWhite).unwrap();
    assert_eq!(bw.entries(), &[[0, 0, 0], [255, 255, 255]]);
    let bwb = bw.encode_act();
    assert_eq!(&bwb[768..772], &[0, 2, 0xFF, 0xFF]);
    assert_eq!(ColorTable::decode_act(&bwb).unwrap(), bw);
    for s in [ShippedTable::MacOs, ShippedTable::Windows] {
        assert_eq!(code(shipped_table(s)), "color_table_not_shipped");
        assert_eq!(s.declared_entries(), 256);
    }
    let mut e = bw.clone();
    e.set_entry(1, RED).unwrap();
    assert_eq!(code(e.set_entry(2, RED)), "color_table_index_out_of_range");
    assert_eq!(code(e.set_transparency_index(Some(2))), "transparency_index_out_of_range");
    e.set_transparency_index(Some(0)).unwrap();
    assert_eq!(ColorTable::decode_act(&e.encode_act()).unwrap(), e);
}

#[test]
fn mt_31177_color_table_hostile_input() {
    for len in [0_usize, 3, 767, 769, 771, 773] {
        assert_eq!(code(ColorTable::decode_act(&vec![0_u8; len])), "color_table_file_length", "{len}");
    }
    let mut f = vec![0_u8; ACT_TRAILER_BYTES];
    for (count, want) in [(0_u16, "color_table_file_count"), (257, "color_table_file_count")] {
        f[768..770].copy_from_slice(&count.to_be_bytes());
        f[770..772].copy_from_slice(&0xFFFF_u16.to_be_bytes());
        assert_eq!(code(ColorTable::decode_act(&f)), want, "{count}");
    }
    f[768..770].copy_from_slice(&4_u16.to_be_bytes());
    f[770..772].copy_from_slice(&4_u16.to_be_bytes());
    assert_eq!(code(ColorTable::decode_act(&f)), "transparency_index_out_of_range");
    f[770..772].copy_from_slice(&3_u16.to_be_bytes());
    assert_eq!(ColorTable::decode_act(&f).unwrap().transparency_index(), Some(3));
    assert_eq!(code(ColorTable::new(&[], None)), "color_table_empty");
    assert_eq!(code(ColorTable::new(&[[0, 0, 0]; 257], None)), "color_table_over_256");
    assert_eq!(code(ColorTable::new(&[[0, 0, 0]; 2], Some(2))), "transparency_index_out_of_range");
}

#[test]
fn mt_31177_color_table_dims_u32_max() {
    // The largest count field the format can carry is refused before any entry is read.
    let mut f = vec![0_u8; ACT_TRAILER_BYTES];
    f[768..770].copy_from_slice(&u16::MAX.to_be_bytes());
    assert_eq!(code(ColorTable::decode_act(&f)), "color_table_file_count");
}

#[test]
fn mt_31177_color_table_aggregate_over_budget() {
    // A file larger than the fixed layout is refused on its length alone (no allocation).
    let big = vec![0_u8; 1 << 20];
    assert_eq!(code(ColorTable::decode_act(&big)), "color_table_file_length");
    let mut over = vec![[1_u8, 2, 3]; 256];
    over.push([0, 0, 0]);
    assert_eq!(code(ColorTable::new(&over, None)), "color_table_over_256");
}

// ---------------------------------------------------------------- slice 4: RGB -> indexed (MT-31175, MT-31177)

use hsk_studio_prism::mode::indexed::{rgb_to_indexed, ForcedColors, IndexedDither, IndexedEnv, IndexedOptions, Matte, Palette, RgbImage};

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

fn flat(px: &[[u8; 3]]) -> Vec<u8> {
    px.iter().flatten().copied().collect()
}

#[test]
fn mt_31175_indexed_exact_web_uniform_palettes() {
    let data = flat(&[RED, GREEN, RED, BLUE]);
    let img = RgbImage::Rgb8 { width: 2, height: 2, data: &data };
    let r = rgb_to_indexed(&img, &IndexedOptions::default(), &IndexedEnv::default()).unwrap();
    assert_eq!(r.table.entries(), &[RED, GREEN, BLUE]);
    assert_eq!(r.indices, vec![0, 1, 0, 2]);
    assert_eq!((r.colors_used(), r.table.transparency_index()), (3, None));

    let many: Vec<[u8; 3]> = (0..272_u32).map(|i| [(i % 256) as u8, (i / 256) as u8, 7]).collect();
    let d = flat(&many);
    let big = RgbImage::Rgb8 { width: 17, height: 16, data: &d };
    assert_eq!(code(rgb_to_indexed(&big, &IndexedOptions::default(), &IndexedEnv::default())), "exact_palette_over_256");

    let w = flat(&[[10, 10, 10], [250, 0, 0]]);
    let web = rgb_to_indexed(&RgbImage::Rgb8 { width: 2, height: 1, data: &w }, &IndexedOptions::new(Palette::Web), &IndexedEnv::default()).unwrap();
    assert_eq!((web.table.len(), web.indices.clone()), (216, vec![0, 180]));

    let mut u = IndexedOptions::new(Palette::Uniform);
    u.set_colors(8).unwrap();
    let uni = rgb_to_indexed(&RgbImage::Rgb8 { width: 2, height: 1, data: &w }, &u, &IndexedEnv::default()).unwrap();
    assert_eq!(uni.table.len(), 8);
    assert_eq!(uni.table.entries()[4], [255, 0, 0]);
    assert_eq!(uni.indices, vec![0, 4]);
}

#[test]
fn mt_31175_indexed_local_palettes_bounded_and_deterministic() {
    let ramp: Vec<[u8; 3]> = (0..256_u32).map(|i| [i as u8, (255 - i) as u8, (i / 2) as u8]).collect();
    let d = flat(&ramp);
    let img = RgbImage::Rgb8 { width: 16, height: 16, data: &d };
    for p in [Palette::LocalAdaptive, Palette::LocalPerceptual, Palette::LocalSelective] {
        let mut o = IndexedOptions::new(p);
        o.set_colors(4).unwrap();
        let a = rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap();
        let b = rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap();
        assert_eq!(a, b, "{p:?} same input, same bytes");
        assert!((2..=4).contains(&a.table.len()), "{p:?} {}", a.table.len());
        assert!(a.indices.iter().all(|i| usize::from(*i) < a.table.len()));
        // Few colours: the distinct colours themselves.
        let few = flat(&[RED, GREEN]);
        let f = rgb_to_indexed(&RgbImage::Rgb8 { width: 2, height: 1, data: &few }, &o, &IndexedEnv::default()).unwrap();
        assert_eq!(f.table.entries(), &[RED, GREEN]);
    }
    for p in [Palette::MasterAdaptive, Palette::MasterPerceptual, Palette::MasterSelective] {
        assert_eq!(code(rgb_to_indexed(&img, &IndexedOptions::new(p), &IndexedEnv::default())), "master_palette_needs_several_images");
    }
    for p in [Palette::MacOs, Palette::Windows, Palette::Previous] {
        assert_eq!(code(rgb_to_indexed(&img, &IndexedOptions::new(p), &IndexedEnv::default())), "palette_table_not_captured");
    }
    assert_eq!(rgb_to_indexed(&img, &IndexedOptions::new(Palette::Uniform), &IndexedEnv::default()), Err(ModeError::ParameterRequired("indexed.colors")));
    let mut zero = IndexedOptions::new(Palette::LocalAdaptive);
    zero.set_colors(0).unwrap();
    assert_eq!(code(rgb_to_indexed(&img, &zero, &IndexedEnv::default())), "empty_palette");
}

#[test]
fn mt_31175_indexed_transparency_matte_forced() {
    let px = [255_u8, 0, 0, 255, 0, 0, 0, 0];
    let img = RgbImage::Rgba8 { width: 2, height: 1, data: &px };
    let mut o = IndexedOptions::default();
    o.transparency = true;
    let r = rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap();
    assert_eq!(r.table.entries(), &[RED, [0, 0, 0]]);
    assert_eq!((r.table.transparency_index(), r.indices.clone()), (Some(1), vec![0, 1]));

    let half = [0_u8, 0, 0, 128];
    let mut m = IndexedOptions::default();
    m.matte = Matte::White;
    let h = rgb_to_indexed(&RgbImage::Rgba8 { width: 1, height: 1, data: &half }, &m, &IndexedEnv::default()).unwrap();
    assert_eq!(h.table.entries(), &[[127, 127, 127]]);
    m.matte = Matte::ForegroundColor;
    assert_eq!(code(rgb_to_indexed(&RgbImage::Rgba8 { width: 1, height: 1, data: &half }, &m, &IndexedEnv::default())), "matte_color_required");
    let env = IndexedEnv { foreground_color: Some([0, 0, 255]), ..IndexedEnv::default() };
    let fg = rgb_to_indexed(&RgbImage::Rgba8 { width: 1, height: 1, data: &half }, &m, &env).unwrap();
    assert_eq!(fg.table.entries(), &[[0, 0, 127]]);

    let one = flat(&[RED]);
    let mut f = IndexedOptions::default();
    f.forced = ForcedColors::BlackWhite;
    let fr = rgb_to_indexed(&RgbImage::Rgb8 { width: 1, height: 1, data: &one }, &f, &IndexedEnv::default()).unwrap();
    assert_eq!((fr.table.entries(), fr.indices.as_slice()), (&[[0, 0, 0], [255, 255, 255], RED][..], &[2_u8][..]));
    let mut fu = IndexedOptions::new(Palette::Uniform);
    fu.forced = ForcedColors::Web;
    fu.set_colors(10).unwrap();
    assert_eq!(code(rgb_to_indexed(&RgbImage::Rgb8 { width: 1, height: 1, data: &one }, &fu, &IndexedEnv::default())), "forced_colors_exceed_count");
}

#[test]
fn mt_31175_indexed_dither_modes_and_preserve_exact() {
    let mid = [128_u8; 8 * 8 * 3];
    let img = RgbImage::Rgb8 { width: 8, height: 8, data: &mid };
    let mut o = IndexedOptions::new(Palette::Uniform);
    o.set_colors(8).unwrap();
    let plain = rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap();
    assert!(plain.indices.iter().all(|i| *i == 7), "128 is nearer 255 than 0");
    o.dither = IndexedDither::Diffusion;
    assert_eq!(rgb_to_indexed(&img, &o, &IndexedEnv::default()), Err(ModeError::ParameterRequired("indexed.dither_amount")));
    o.set_dither_amount(100).unwrap();
    let dif = rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap();
    assert!(dif.indices.contains(&0) && dif.indices.contains(&7));
    assert_eq!(dif, rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap());
    o.dither = IndexedDither::Pattern;
    assert_eq!(rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap(), rgb_to_indexed(&img, &o, &IndexedEnv::default()).unwrap());

    // Noise needs an explicit seed; the seed fixes the bytes.
    let pair: Vec<[u8; 3]> = (0..256).map(|i| if i % 2 == 0 { [100, 100, 100] } else { [110, 110, 110] }).collect();
    let d = flat(&pair);
    let two = RgbImage::Rgb8 { width: 16, height: 16, data: &d };
    let mut n = IndexedOptions::default();
    n.dither = IndexedDither::Noise;
    assert_eq!(rgb_to_indexed(&two, &n, &IndexedEnv::default()), Err(ModeError::ParameterRequired("indexed.noise_seed")));
    let env = IndexedEnv { noise_seed: Some(7), ..IndexedEnv::default() };
    let a = rgb_to_indexed(&two, &n, &env).unwrap();
    assert_eq!(a, rgb_to_indexed(&two, &n, &env).unwrap());
    let exact: Vec<u8> = (0..256).map(|i| (i % 2) as u8).collect();
    assert_ne!(a.indices, exact, "noise moves some pixels to the neighbouring entry");
    n.preserve_exact_colors = true;
    assert_eq!(rgb_to_indexed(&two, &n, &env).unwrap().indices, exact, "exact colours are preserved");
}

#[test]
fn mt_31175_indexed_dims_u32_max_and_aggregate_over_budget() {
    let tiny = [0_u8; 8];
    let img = RgbImage::Rgba8 { width: u32::MAX, height: 2, data: &tiny };
    assert_eq!(code(rgb_to_indexed(&img, &IndexedOptions::default(), &IndexedEnv::default())), "pixel_budget_exceeded");
    let over = RgbImage::Rgb8 { width: 20_000, height: 20_000, data: &tiny };
    assert_eq!(code(rgb_to_indexed(&over, &IndexedOptions::default(), &IndexedEnv::default())), "pixel_budget_exceeded");
    let short = RgbImage::Rgb8 { width: 2, height: 2, data: &tiny };
    assert_eq!(code(rgb_to_indexed(&short, &IndexedOptions::default(), &IndexedEnv::default())), "buffer_length_mismatch");
    let empty = RgbImage::Rgb8 { width: 4, height: 0, data: &[] };
    assert_eq!(code(rgb_to_indexed(&empty, &IndexedOptions::default(), &IndexedEnv::default())), "empty_image");
}

#[test]
fn mt_31177_read_only_counts_and_dither_amount_bounds() {
    for p in Palette::ALL {
        let mut o = IndexedOptions::new(*p);
        let read_only = matches!(p, Palette::Exact | Palette::MacOs | Palette::Windows | Palette::Web | Palette::Previous);
        assert_eq!(p.colors_read_only(), read_only, "{p:?}");
        if read_only {
            assert_eq!(code(o.set_colors(16)), "indexed_colors_read_only");
            assert_eq!(o.colors(), None);
        } else {
            assert_eq!(code(o.set_colors(257)), "parameter_out_of_range");
            o.set_colors(256).unwrap();
            assert_eq!(o.colors(), Some(256));
        }
    }
    assert_eq!(Palette::ALL.len(), 12);
    assert_eq!(IndexedOptions::default().palette(), Palette::Exact);
    let mut o = IndexedOptions::default();
    assert_eq!(code(o.set_dither_amount(0)), "parameter_out_of_range");
    assert_eq!(code(o.set_dither_amount(101)), "parameter_out_of_range");
    o.set_dither_amount(1).unwrap();
    o.set_dither_amount(100).unwrap();
    assert_eq!(o.dither_amount(), Some(100));
    // Inert while dither is not diffusion: kept and returned unchanged.
    let px = flat(&[RED]);
    let r = rgb_to_indexed(&RgbImage::Rgb8 { width: 1, height: 1, data: &px }, &o, &IndexedEnv::default()).unwrap();
    assert_eq!(r.options.dither_amount(), Some(100));
    let mattes: Vec<&str> = Matte::ALL.iter().map(|m| m.token()).collect();
    assert_eq!(mattes, ["none", "foreground_color", "background_color", "white", "black", "semi_gray", "netscape_gray"]);
}
