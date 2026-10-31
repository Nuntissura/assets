//! Lane SMB-ID-BOOKLET-1: print preferences (STU-LAY-156), the two print configurations and booklet
//! options (STU-LAY-157), and the imposition plan (STU-LAY-060). Expectations are hand-computed from
//! the spec tables and from the Studio readings stated in `src/booklet.rs` (InDesign parity
//! UNVERIFIED, D5).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_layout::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

fn options(booklet_type: BookletType, range: &str) -> BookletOptions {
    BookletOptions {
        booklet_type,
        page_range: range.into(),
        space_between_pages: 0.0,
        bleed_between_pages: 0.0,
        creep: 0.0,
        signature_size: 8,
        top_margin: 0.0,
        bottom_margin: 0.0,
        left_margin: 0.0,
        right_margin: 0.0,
        margins_uniform_size: false,
        auto_adjust_margins: false,
        print_blank_printer_spreads: true,
    }
}

fn slots(plan: &ImpositionPlan) -> Vec<Vec<Option<u32>>> {
    plan.spreads.iter().map(|s| s.slots.clone()).collect()
}

fn pairs(list: &[(u32, u32)]) -> Vec<Vec<Option<u32>>> {
    list.iter().map(|&(a, b)| vec![Some(a), Some(b)]).collect()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    Rect { x0_pt: x0, y0_pt: y0, x1_pt: x1, y1_pt: y1 }
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}
fn observe() -> Observe {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    Observe::new(1, 3, resource, actor, Budget::new(0, 0).unwrap())
}

/// Parameter, name, hard_min, hard_max, unit code, precision.
type BoundRow = (PrintParameter, &'static str, f64, f64, &'static str, Option<u32>);
/// Sets one booklet numeric.
type SetField = fn(&mut BookletOptions, f64);

// ---- STU-LAY-156 numeric bounds (MT-30820, MT-30819 AC-001) ----

#[test]
fn print_parameter_bounds_are_stored_separately_as_tabled() {
    bounded_case("print_parameter_bounds_are_stored_separately_as_tabled", || {
        use PrintParameter as P;
        let expected: [BoundRow; 15] = [
            (P::ScaleWidth, "scale_width", 0.0, 1000.0, "percent", None),
            (P::ScaleHeight, "scale_height", 0.0, 1000.0, "percent", None),
            (P::BitmapResolution, "bitmap_resolution", 72.0, 1200.0, "dpi", Some(0)),
            (P::BlackAngle, "black_angle", 0.0, 360.0, "deg", None),
            (P::CyanAngle, "cyan_angle", 0.0, 360.0, "deg", None),
            (P::MagentaAngle, "magenta_angle", 0.0, 360.0, "deg", None),
            (P::YellowAngle, "yellow_angle", 0.0, 360.0, "deg", None),
            (P::SpotAngle, "spot_angle", 0.0, 360.0, "deg", None),
            (P::CompositeAngle, "composite_angle", 0.0, 360.0, "deg", None),
            (P::BlackFrequency, "black_frequency", 1.0, 500.0, "count", None),
            (P::CyanFrequency, "cyan_frequency", 1.0, 500.0, "count", None),
            (P::MagentaFrequency, "magenta_frequency", 1.0, 500.0, "count", None),
            (P::YellowFrequency, "yellow_frequency", 1.0, 500.0, "count", None),
            (P::SpotFrequency, "spot_frequency", 1.0, 500.0, "count", None),
            (P::CompositeFrequency, "composite_frequency", 1.0, 500.0, "count", None),
        ];
        assert_eq!(P::ALL.len(), expected.len());
        for (i, (p, name, hard_min, hard_max, unit, precision)) in expected.into_iter().enumerate() {
            assert_eq!(P::ALL[i], p);
            assert_eq!(p.name(), name);
            let b = p.bounds();
            assert_eq!((b.hard_min, b.hard_max), (hard_min, hard_max), "{name}");
            // Unknown soft bounds and default stay unknown: not mirrored from the hard bounds.
            assert_eq!((b.soft_min, b.soft_max, b.default), (None, None, None), "{name}");
            assert_eq!(b.unit.code(), unit, "{name}");
            assert_eq!(b.precision, precision, "{name}");
        }
        assert_eq!(PrintUnit::LinesPerInch.code(), "count");
    });
}

#[test]
fn print_parameter_check_refuses_outside_hard_bounds_and_precision() {
    bounded_case("print_parameter_check_refuses_outside_hard_bounds_and_precision", || {
        use PrintParameter as P;
        for (p, ok, refused) in [
            (P::ScaleWidth, vec![0.0, 1000.0, 37.25], vec![-0.5, 1000.001]),
            (P::ScaleHeight, vec![0.0, 1000.0], vec![-1.0, 1001.0]),
            (P::BitmapResolution, vec![72.0, 1200.0, 300.0], vec![71.0, 1201.0]),
            (P::SpotAngle, vec![0.0, 360.0, 12.5], vec![-0.1, 360.5]),
            (P::CompositeFrequency, vec![1.0, 500.0, 133.3], vec![0.5, 500.5]),
        ] {
            for v in ok {
                assert_eq!(p.check(v), Ok(v), "{} {v}", p.name());
            }
            for v in refused {
                assert_eq!(p.check(v), Err(PrintError::ParameterOutOfBounds(p)), "{} {v}", p.name());
            }
            for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                assert_eq!(p.check(v), Err(PrintError::ParameterNotFinite(p)));
            }
        }
        // Precision 0: whole dpi only. Unknown precision admits fractions.
        assert_eq!(P::BitmapResolution.check(300.5), Err(PrintError::ParameterPrecision(P::BitmapResolution)));
        assert_eq!(PrintError::ParameterPrecision(P::BitmapResolution).code(), "print_parameter_precision");
        assert_eq!(PrintError::ParameterOutOfBounds(P::ScaleWidth).code(), "print_parameter_out_of_bounds");
    });
}

// ---- STU-LAY-156 groups and enumerations (MT-30821..MT-30828) ----

#[test]
fn print_groups_carry_exactly_the_spec_fields_in_order() {
    bounded_case("print_groups_carry_exactly_the_spec_fields_in_order", || {
        let codes: Vec<&str> = PrintGroup::ALL.iter().map(|g| g.code()).collect();
        assert_eq!(
            codes,
            ["general", "setup", "marks_and_bleed", "output", "graphics", "colour_management", "advanced", "device"]
        );
        let counts: Vec<usize> = PrintGroup::ALL.iter().map(|g| g.fields().len()).collect();
        assert_eq!(counts, [10, 17, 16, 14, 9, 4, 5, 8]);
        assert_eq!(
            PrintGroup::Output.fields(),
            [
                "color_output", "trapping", "text_as_black", "flip", "negative", "screening", "simulate_overprint",
                "print_black", "print_cyan", "print_magenta", "print_yellow", "separation_screening",
                "composite_screening", "preserve_color_numbers",
            ]
        );
        // The record's fields, visited, are exactly the catalogue of each group, in order.
        let prefs = PrintPreferences::default();
        for group in PrintGroup::ALL {
            let mut names = Vec::new();
            prefs.visit_group(group, &mut |name, _| names.push(name));
            assert_eq!(names, group.fields(), "{}", group.code());
        }
        // Declared kinds: the four enumerations are members, the bounded numerics are parameters.
        let mut kinds = Vec::new();
        for group in PrintGroup::ALL {
            prefs.visit_group(group, &mut |name, field| match field {
                PrintField::Member(_) => kinds.push(("member", name)),
                PrintField::Parameter(p, _) => kinds.push((p.name(), name)),
                PrintField::Value(_) => {}
            });
        }
        assert_eq!(
            kinds,
            [
                ("member", "print_layers"),
                ("member", "print_page_orientation"),
                ("scale_width", "scale_width"),
                ("scale_height", "scale_height"),
                ("member", "trapping"),
                ("member", "send_image_data"),
                ("bitmap_resolution", "bitmap_resolution"),
            ]
        );
        let mut screens = Vec::new();
        prefs.screens.visit(&mut |name, _| screens.push(name));
        assert_eq!(screens.len(), 12);
        assert_eq!(screens[0], "black_angle");
        assert_eq!(screens[11], "composite_frequency");
    });
}

#[test]
fn print_enumerations_have_exactly_the_tabled_members() {
    bounded_case("print_enumerations_have_exactly_the_tabled_members", || {
        let codes = |all: Vec<&'static str>| all;
        assert_eq!(
            codes(PrintLayers::ALL.iter().map(|m| m.code()).collect()),
            ["print_layers.all", "print_layers.visible", "print_layers.visible_printable"]
        );
        assert_eq!(
            codes(PrintPageOrientation::ALL.iter().map(|m| m.code()).collect()),
            ["portrait", "landscape", "reverse_portrait", "reverse_landscape"]
        );
        assert_eq!(
            codes(Trapping::ALL.iter().map(|m| m.code()).collect()),
            ["trapping.off", "trapping.application_builtin", "trapping.in_rip"]
        );
        assert_eq!(
            codes(SendImageData::ALL.iter().map(|m| m.code()).collect()),
            ["image_data.all", "image_data.optimized_subsampling", "image_data.proxy", "image_data.none"]
        );
        assert_eq!(
            codes(PresetType::ALL.iter().map(|m| m.code()).collect()),
            ["preset_type.default", "preset_type.custom"]
        );
        for m in PrintLayers::ALL {
            assert_eq!(PrintLayers::from_code(m.code()), Ok(*m));
        }
        assert_eq!(SendImageData::from_code("image_data.none"), Ok(SendImageData::NoImageData));
        assert_eq!(Trapping::from_code("trapping.in_rip"), Ok(Trapping::InRip));
        // Exact codes only: no case folding, trimming, short forms or aliasing.
        for bad in ["PRINT_LAYERS.ALL", " print_layers.all", "all", "print_layers.visible ", ""] {
            assert_eq!(PrintLayers::from_code(bad), Err(PrintError::UnknownMember("PrintLayers")));
        }
        assert_eq!(PrintPageOrientation::from_code("Portrait"), Err(PrintError::UnknownMember("PrintPageOrientation")));
        assert_eq!(PresetType::from_code("preset_type.user"), Err(PrintError::UnknownMember("PresetType")));
        assert_eq!(PrintError::UnknownMember("PresetType").code(), "print_enum_member_unknown");
    });
}

#[test]
fn print_preferences_validate_every_group_and_screen() {
    bounded_case("print_preferences_validate_every_group_and_screen", || {
        let mut p = PrintPreferences::default();
        assert_eq!(p.validate(), Ok(()));
        p.general.print_layers = Some(PrintLayers::VisiblePrintable);
        p.general.copies = Some(PrintValue::Number(3.0));
        p.setup.scale_width = Some(1000.0);
        p.setup.print_page_orientation = Some(PrintPageOrientation::ReverseLandscape);
        p.output.trapping = Some(Trapping::ApplicationBuiltin);
        p.graphics.send_image_data = Some(SendImageData::Proxy);
        p.advanced.bitmap_printing = Some(PrintValue::Bool(true));
        p.advanced.bitmap_resolution = Some(600.0);
        p.colour_management.intent = Some(PrintValue::Text("perceptual".into()));
        p.device.printer = Some(PrintValue::Text("press-1".into()));
        p.screens.black_angle = Some(45.0);
        p.screens.black_frequency = Some(150.0);
        assert_eq!(p.validate(), Ok(()));

        let refused = |edit: fn(&mut PrintPreferences), error: PrintError| {
            let mut q = p.clone();
            edit(&mut q);
            assert_eq!(q.validate(), Err(error));
        };
        refused(|q| q.setup.scale_height = Some(1000.5), PrintError::ParameterOutOfBounds(PrintParameter::ScaleHeight));
        refused(|q| q.advanced.bitmap_resolution = Some(71.0), PrintError::ParameterOutOfBounds(PrintParameter::BitmapResolution));
        refused(|q| q.advanced.bitmap_resolution = Some(600.25), PrintError::ParameterPrecision(PrintParameter::BitmapResolution));
        refused(|q| q.screens.magenta_frequency = Some(0.0), PrintError::ParameterOutOfBounds(PrintParameter::MagentaFrequency));
        refused(|q| q.screens.spot_angle = Some(f64::NAN), PrintError::ParameterNotFinite(PrintParameter::SpotAngle));
        refused(|q| q.general.copies = Some(PrintValue::Number(f64::INFINITY)), PrintError::FieldNotFinite("copies"));
        refused(
            |q| q.device.print_file = Some(PrintValue::Text("x".repeat(MAX_PRINT_TEXT_BYTES + 1))),
            PrintError::TextTooLong("print_file"),
        );
        // The bound text length itself is admitted.
        let mut q = p.clone();
        q.device.print_file = Some(PrintValue::Text("x".repeat(MAX_PRINT_TEXT_BYTES)));
        assert_eq!(q.validate(), Ok(()));
    });
}

#[test]
fn printer_preset_saves_and_applies_a_complete_state() {
    bounded_case("printer_preset_saves_and_applies_a_complete_state", || {
        let mut state = PrintPreferences::default();
        state.general.print_layers = Some(PrintLayers::Visible);
        state.advanced.bitmap_printing = Some(PrintValue::Bool(true));
        state.advanced.bitmap_resolution = Some(300.0);
        state.screens.cyan_angle = Some(15.0);
        let preset = PrinterPreset::new("Proof 300", PresetType::Custom, state.clone()).unwrap();
        assert_eq!(preset.name, "Proof 300");
        assert_eq!(preset.preset_type.code(), "preset_type.custom");
        assert_eq!(preset.apply(), state);
        let mut applied = preset.apply();
        applied.screens.cyan_angle = Some(75.0);
        assert_eq!(preset.preferences, state, "applying copies; the preset is unchanged");

        assert_eq!(PrinterPreset::new("", PresetType::Default, state.clone()), Err(PrintError::PresetNameEmpty));
        assert_eq!(PrinterPreset::new(" \t", PresetType::Default, state.clone()), Err(PrintError::PresetNameEmpty));
        assert_eq!(
            PrinterPreset::new(&"n".repeat(MAX_PRESET_NAME_BYTES + 1), PresetType::Default, state.clone()),
            Err(PrintError::PresetNameTooLong)
        );
        assert!(PrinterPreset::new(&"n".repeat(MAX_PRESET_NAME_BYTES), PresetType::Default, state.clone()).is_ok());
        let mut bad = state;
        bad.advanced.bitmap_resolution = Some(1300.0);
        assert_eq!(
            PrinterPreset::new("Bad", PresetType::Custom, bad),
            Err(PrintError::ParameterOutOfBounds(PrintParameter::BitmapResolution))
        );
    });
}

// ---- STU-LAY-157 booklet options and two configurations (MT-30830, MT-30831) ----

#[test]
fn booklet_type_has_exactly_five_members() {
    bounded_case("booklet_type_has_exactly_five_members", || {
        let codes: Vec<&str> = BookletType::ALL.iter().map(|t| t.code()).collect();
        assert_eq!(
            codes,
            [
                "booklet.two_up_saddle_stitch",
                "booklet.two_up_perfect_bound",
                "booklet.two_up_consecutive",
                "booklet.three_up_consecutive",
                "booklet.four_up_consecutive",
            ]
        );
        for t in BookletType::ALL {
            assert_eq!(BookletType::from_code(t.code()), Ok(t));
        }
        for bad in ["booklet.two_up_saddle", "two_up_saddle_stitch", "BOOKLET.TWO_UP_CONSECUTIVE", "booklet.five_up_consecutive"] {
            assert_eq!(BookletType::from_code(bad), Err(BookletError::BookletTypeUnknown));
        }
        let slots: Vec<u32> = BookletType::ALL.iter().map(|t| t.slots()).collect();
        assert_eq!(slots, [2, 2, 2, 3, 4]);
    });
}

#[test]
fn booklet_options_margins_and_admission() {
    bounded_case("booklet_options_margins_and_admission", || {
        let mut o = options(BookletType::TwoUpSaddleStitch, "1-8");
        (o.top_margin, o.bottom_margin, o.left_margin, o.right_margin) = (12.0, 3.0, 4.0, 5.0);
        assert_eq!(o.effective_margins(), BookletMargins { top: 12.0, bottom: 3.0, left: 4.0, right: 5.0 });
        o.margins_uniform_size = true;
        assert_eq!(o.effective_margins(), BookletMargins { top: 12.0, bottom: 12.0, left: 12.0, right: 12.0 });
        assert_eq!(o.validate(), Ok(()));
        // Finite values >= 0 only, field by field.
        let cases: [(SetField, &str); 7] = [
            (|o, v| o.space_between_pages = v, "space_between_pages"),
            (|o, v| o.bleed_between_pages = v, "bleed_between_pages"),
            (|o, v| o.creep = v, "creep"),
            (|o, v| o.top_margin = v, "top_margin"),
            (|o, v| o.bottom_margin = v, "bottom_margin"),
            (|o, v| o.left_margin = v, "left_margin"),
            (|o, v| o.right_margin = v, "right_margin"),
        ];
        for (set, name) in cases {
            for bad in [-0.01, f64::NAN, f64::INFINITY] {
                let mut q = o.clone();
                set(&mut q, bad);
                assert_eq!(q.validate(), Err(BookletError::ValueInvalid(name)), "{name} {bad}");
            }
            let mut q = o.clone();
            set(&mut q, 0.0);
            assert_eq!(q.validate(), Ok(()));
        }
        // Signature size is checked for perfect bound only: a positive multiple of 4.
        let mut p = options(BookletType::TwoUpPerfectBound, "1-8");
        for bad in [0, 6, 10, MAX_IMPOSITION_PAGES + 4] {
            p.signature_size = bad;
            assert_eq!(p.validate(), Err(BookletError::SignatureSizeInvalid), "{bad}");
        }
        for good in [4, 8, 16, 32] {
            p.signature_size = good;
            assert_eq!(p.validate(), Ok(()));
        }
        let mut s = options(BookletType::TwoUpSaddleStitch, "1-8");
        s.signature_size = 6;
        assert_eq!(s.validate(), Ok(()), "saddle stitch does not use the signature size");
    });
}

#[test]
fn two_print_configurations_are_separate() {
    bounded_case("two_print_configurations_are_separate", || {
        let mut document = PrintPreferences::default();
        document.setup.paper_size = Some(PrintValue::Text("letter".into()));
        document.marks_and_bleed.crop_marks = Some(PrintValue::Bool(false));
        let mut booklet_prefs = document.clone();
        booklet_prefs.setup.paper_size = Some(PrintValue::Text("tabloid".into()));
        booklet_prefs.marks_and_bleed.crop_marks = Some(PrintValue::Bool(true));
        let mut configs = PrintConfigurations {
            document: document.clone(),
            booklet: BookletPrintConfiguration { preferences: booklet_prefs, options: options(BookletType::TwoUpSaddleStitch, "1-8") },
        };
        assert_eq!(configs.validate(), Ok(()));
        assert_ne!(configs.document, configs.booklet.preferences);
        configs.booklet.preferences.setup.scale_width = Some(50.0);
        assert_eq!(configs.document, document, "editing the booklet configuration leaves the document's");

        configs.booklet.preferences.setup.scale_width = Some(5000.0);
        let e = configs.validate().unwrap_err();
        assert_eq!(
            e,
            BookletError::Print {
                configuration: PrintConfigurationKind::Booklet,
                error: PrintError::ParameterOutOfBounds(PrintParameter::ScaleWidth),
            }
        );
        assert_eq!(e.code(), "print_parameter_out_of_bounds");
        configs.booklet.preferences.setup.scale_width = None;
        configs.document.screens.yellow_angle = Some(400.0);
        assert_eq!(
            configs.validate(),
            Err(BookletError::Print {
                configuration: PrintConfigurationKind::Document,
                error: PrintError::ParameterOutOfBounds(PrintParameter::YellowAngle),
            })
        );
        configs.document.screens.yellow_angle = None;
        configs.booklet.options.creep = -1.0;
        assert_eq!(configs.validate(), Err(BookletError::ValueInvalid("creep")));
    });
}

// ---- STU-LAY-060 imposition (MT-30829) ----

#[test]
fn saddle_stitch_8_12_16_pages_pair_outermost_first() {
    bounded_case("saddle_stitch_8_12_16_pages_pair_outermost_first", || {
        let plan = impose(&options(BookletType::TwoUpSaddleStitch, "1-8"), 8).unwrap();
        assert_eq!(slots(&plan), pairs(&[(8, 1), (2, 7), (6, 3), (4, 5)]));
        let sheet_side: Vec<(u32, Option<SheetSide>)> = plan.spreads.iter().map(|s| (s.sheet, s.side)).collect();
        use SheetSide::{Back, Front};
        assert_eq!(sheet_side, [(0, Some(Front)), (0, Some(Back)), (1, Some(Front)), (1, Some(Back))]);
        assert_eq!((plan.selected_pages, plan.positions, plan.omitted_blank_spreads), (8, 8, 0));

        let plan = impose(&options(BookletType::TwoUpSaddleStitch, "1-12"), 12).unwrap();
        assert_eq!(slots(&plan), pairs(&[(12, 1), (2, 11), (10, 3), (4, 9), (8, 5), (6, 7)]));

        let plan = impose(&options(BookletType::TwoUpSaddleStitch, "1-16"), 16).unwrap();
        assert_eq!(
            slots(&plan),
            pairs(&[(16, 1), (2, 15), (14, 3), (4, 13), (12, 5), (6, 11), (10, 7), (8, 9)])
        );
        let seq: Vec<u32> = plan.spreads.iter().map(|s| s.sequence).collect();
        assert_eq!(seq, (0..8).collect::<Vec<_>>());

        // A sub-range keeps the document ordinals: pages 3..10 of a 12-page document.
        let plan = impose(&options(BookletType::TwoUpSaddleStitch, "3-10"), 12).unwrap();
        assert_eq!(slots(&plan), pairs(&[(10, 3), (4, 9), (8, 5), (6, 7)]));
        // Written order is kept: "5-8, 1-4" puts 5 outermost.
        let plan = impose(&options(BookletType::TwoUpSaddleStitch, " 5 - 8 , 1-4 "), 8).unwrap();
        assert_eq!(slots(&plan), pairs(&[(4, 5), (6, 3), (2, 7), (8, 1)]));
    });
}

#[test]
fn saddle_stitch_pads_to_a_multiple_of_four_with_blanks_at_the_end() {
    bounded_case("saddle_stitch_pads_to_a_multiple_of_four_with_blanks_at_the_end", || {
        let plan = impose(&options(BookletType::TwoUpSaddleStitch, "1-10"), 10).unwrap();
        assert_eq!((plan.selected_pages, plan.positions), (10, 12));
        assert_eq!(
            slots(&plan),
            [
                vec![None, Some(1)],
                vec![Some(2), None],
                vec![Some(10), Some(3)],
                vec![Some(4), Some(9)],
                vec![Some(8), Some(5)],
                vec![Some(6), Some(7)],
            ]
        );
        // One page: the back of the only sheet is wholly blank.
        let mut o = options(BookletType::TwoUpSaddleStitch, "1");
        let plan = impose(&o, 1).unwrap();
        assert_eq!(slots(&plan), [vec![None, Some(1)], vec![None, None]]);
        o.print_blank_printer_spreads = false;
        let plan = impose(&o, 1).unwrap();
        assert_eq!(slots(&plan), [vec![None, Some(1)]]);
        assert_eq!(plan.omitted_blank_spreads, 1);
    });
}

#[test]
fn perfect_bound_signatures_are_each_saddle_ordered() {
    bounded_case("perfect_bound_signatures_are_each_saddle_ordered", || {
        let plan = impose(&options(BookletType::TwoUpPerfectBound, "1-16"), 16).unwrap();
        assert_eq!(
            slots(&plan),
            pairs(&[(8, 1), (2, 7), (6, 3), (4, 5), (16, 9), (10, 15), (14, 11), (12, 13)])
        );
        let sig: Vec<(u32, u32)> = plan.spreads.iter().map(|s| (s.signature, s.sheet)).collect();
        assert_eq!(sig, [(0, 0), (0, 0), (0, 1), (0, 1), (1, 0), (1, 0), (1, 1), (1, 1)]);

        // 12 pages, signatures of 8: the last signature holds 4 pages.
        let plan = impose(&options(BookletType::TwoUpPerfectBound, "1-12"), 12).unwrap();
        assert_eq!(slots(&plan), pairs(&[(8, 1), (2, 7), (6, 3), (4, 5), (12, 9), (10, 11)]));
        assert_eq!(plan.positions, 12);

        // Signatures of 4 over 8 pages.
        let mut o = options(BookletType::TwoUpPerfectBound, "1-8");
        o.signature_size = 4;
        let plan = impose(&o, 8).unwrap();
        assert_eq!(slots(&plan), pairs(&[(4, 1), (2, 3), (8, 5), (6, 7)]));

        // 9 pages, signatures of 8: the last signature (1 page padded to 4) has a wholly blank side.
        let mut o = options(BookletType::TwoUpPerfectBound, "1-9");
        let plan = impose(&o, 9).unwrap();
        assert_eq!(plan.positions, 12);
        assert_eq!(slots(&plan)[4..], [vec![None, Some(9)], vec![None, None]]);
        o.print_blank_printer_spreads = false;
        let plan = impose(&o, 9).unwrap();
        assert_eq!(plan.spreads.len(), 5);
        assert_eq!(plan.omitted_blank_spreads, 1);
        assert_eq!(plan.spreads[4].sequence, 4);
    });
}

#[test]
fn consecutive_two_three_and_four_up() {
    bounded_case("consecutive_two_three_and_four_up", || {
        let plan = impose(&options(BookletType::TwoUpConsecutive, "1-8"), 8).unwrap();
        assert_eq!(slots(&plan), pairs(&[(1, 2), (3, 4), (5, 6), (7, 8)]));
        assert!(plan.spreads.iter().all(|s| s.side.is_none() && s.creep_offset_pt == 0.0));

        let plan = impose(&options(BookletType::ThreeUpConsecutive, "1-8"), 8).unwrap();
        assert_eq!(
            slots(&plan),
            [vec![Some(1), Some(2), Some(3)], vec![Some(4), Some(5), Some(6)], vec![Some(7), Some(8), None]]
        );
        assert_eq!(plan.positions, 9);

        let plan = impose(&options(BookletType::ThreeUpConsecutive, "1-12"), 12).unwrap();
        assert_eq!(plan.spreads.len(), 4);
        assert_eq!(plan.spreads[3].slots, [Some(10), Some(11), Some(12)]);

        let plan = impose(&options(BookletType::FourUpConsecutive, "1-16"), 16).unwrap();
        let expected: Vec<Vec<Option<u32>>> = (0..4).map(|s| (1..=4).map(|j| Some(s * 4 + j)).collect()).collect();
        assert_eq!(slots(&plan), expected);
        let sheets: Vec<u32> = plan.spreads.iter().map(|s| s.sheet).collect();
        assert_eq!(sheets, [0, 1, 2, 3]);

        let plan = impose(&options(BookletType::FourUpConsecutive, "2,4,6"), 6).unwrap();
        assert_eq!(slots(&plan), [vec![Some(2), Some(4), Some(6), None]]);
    });
}

#[test]
fn creep_is_linear_from_outermost_to_innermost_sheet() {
    bounded_case("creep_is_linear_from_outermost_to_innermost_sheet", || {
        let mut o = options(BookletType::TwoUpSaddleStitch, "1-16");
        o.creep = 3.0;
        let plan = impose(&o, 16).unwrap();
        let creep: Vec<f64> = plan.spreads.iter().map(|s| s.creep_offset_pt).collect();
        assert_eq!(creep, [0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        // One sheet: nothing nests inside it.
        o.page_range = "1-4".into();
        let plan = impose(&o, 4).unwrap();
        assert!(plan.spreads.iter().all(|s| s.creep_offset_pt == 0.0));
        // Perfect bound: per signature.
        let mut p = options(BookletType::TwoUpPerfectBound, "1-16");
        p.creep = 2.0;
        let plan = impose(&p, 16).unwrap();
        let creep: Vec<f64> = plan.spreads.iter().map(|s| s.creep_offset_pt).collect();
        assert_eq!(creep, [0.0, 0.0, 2.0, 2.0, 0.0, 0.0, 2.0, 2.0]);
        // Consecutive: none.
        let mut c = options(BookletType::TwoUpConsecutive, "1-8");
        c.creep = 5.0;
        assert!(impose(&c, 8).unwrap().spreads.iter().all(|s| s.creep_offset_pt == 0.0));
    });
}

#[test]
fn page_range_refusals_are_typed() {
    bounded_case("page_range_refusals_are_typed", || {
        let refuse = |range: &str, pages: u32| impose(&options(BookletType::TwoUpSaddleStitch, range), pages).unwrap_err();
        assert_eq!(refuse("", 8), BookletError::PageRangeEmpty);
        assert_eq!(refuse("  \t", 8), BookletError::PageRangeEmpty);
        for malformed in ["1-", "-3", "0", "0-4", "5-3", "1,,2", "1,", "+1", "1-2-3", "a", "1 2", "1.5", "4294967296", "\u{0661}"] {
            assert_eq!(refuse(malformed, 8), BookletError::PageRangeMalformed, "{malformed:?}");
        }
        assert_eq!(refuse("9", 8), BookletError::PageRangeOutOfDocument);
        assert_eq!(refuse("1-9", 8), BookletError::PageRangeOutOfDocument);
        assert_eq!(refuse("1", 0), BookletError::PageRangeOutOfDocument);
        let mut p = options(BookletType::TwoUpPerfectBound, "1-8");
        p.signature_size = 6;
        assert_eq!(impose(&p, 8), Err(BookletError::SignatureSizeInvalid));
        assert_eq!(parse_page_range("1-3,7", 8), Ok(vec![1, 2, 3, 7]));
        assert_eq!(BookletError::PageRangeMalformed.code(), "page_range_malformed");
        assert_eq!(BookletError::SignatureSizeInvalid.code(), "signature_size_invalid");
    });
}

#[test]
fn imposition_dims_u32_max_is_refused_before_allocation() {
    bounded_case("imposition_dims_u32_max_is_refused_before_allocation", || {
        let max = u32::MAX.to_string();
        assert_eq!(parse_page_range(&format!("1-{max}"), u32::MAX), Err(BookletError::PageCountOverBound));
        assert_eq!(parse_page_range(&max, u32::MAX), Ok(vec![u32::MAX]));
        let o = options(BookletType::FourUpConsecutive, &format!("1-{max}"));
        assert_eq!(impose(&o, u32::MAX), Err(BookletError::PageCountOverBound));
        // At the bound itself the plan is built.
        let o = options(BookletType::TwoUpSaddleStitch, &format!("1-{MAX_IMPOSITION_PAGES}"));
        let plan = impose(&o, MAX_IMPOSITION_PAGES).unwrap();
        assert_eq!(plan.spreads.len(), (MAX_IMPOSITION_PAGES / 2) as usize);
        assert_eq!(plan.spreads[0].slots, [Some(MAX_IMPOSITION_PAGES), Some(1)]);
    });
}

#[test]
fn imposition_aggregate_over_budget_is_refused_before_allocation() {
    bounded_case("imposition_aggregate_over_budget_is_refused_before_allocation", || {
        // Each run is under the bound; their running sum is not.
        assert_eq!(parse_page_range("1-60000,1-60000", 60_000), Err(BookletError::PageCountOverBound));
        let many = vec!["1-100000"; 40].join(",");
        assert_eq!(parse_page_range(&many, 100_000), Err(BookletError::PageCountOverBound));
        // The range text itself is bounded.
        let long = vec!["1"; MAX_PAGE_RANGE_BYTES / 2 + 1].join(",");
        assert!(long.len() > MAX_PAGE_RANGE_BYTES);
        assert_eq!(parse_page_range(&long, 8), Err(BookletError::PageRangeTooLong));
        assert_eq!(impose(&options(BookletType::TwoUpConsecutive, &long), 8), Err(BookletError::PageRangeTooLong));
        assert_eq!(BookletError::PageCountOverBound.layout_error(), LayoutError::BudgetExceeded);
    });
}

// ---- sheet geometry ----

#[test]
fn sheet_geometry_saddle_stitch_with_creep_toward_the_fold() {
    bounded_case("sheet_geometry_saddle_stitch_with_creep_toward_the_fold", || {
        let mut o = options(BookletType::TwoUpSaddleStitch, "1-8");
        o.creep = 3.0;
        o.bleed_between_pages = 5.0; // no space between pages: no interior bleed shows
        o.top_margin = 10.0;
        (o.bottom_margin, o.left_margin, o.right_margin) = (99.0, 99.0, 99.0);
        o.margins_uniform_size = true; // top governs all four
        let plan = impose(&o, 8).unwrap();
        let req = SheetRequest { page_width_pt: 100.0, page_height_pt: 200.0, paper: None };
        let sheets = sheet_geometry(&plan, &o, &req).unwrap();
        assert_eq!(sheets.len(), 4);
        for s in &sheets {
            assert_eq!((s.width_pt, s.height_pt, s.fold_x_pt), (220.0, 220.0, Some(110.0)));
        }
        let outer = &sheets[0];
        assert_eq!(outer.creep_offset_pt, 0.0);
        assert_eq!(outer.pages[0].page, Some(8));
        assert_eq!(outer.pages[0].trim, rect(10.0, 10.0, 110.0, 210.0));
        assert_eq!(outer.pages[1].trim, rect(110.0, 10.0, 210.0, 210.0));
        assert_eq!(outer.pages[0].bleed, outer.pages[0].trim);
        let inner = &sheets[2];
        assert_eq!((inner.sequence, inner.creep_offset_pt), (2, 3.0));
        assert_eq!(inner.pages[0].trim, rect(13.0, 10.0, 113.0, 210.0));
        assert_eq!(inner.pages[1].trim, rect(107.0, 10.0, 207.0, 210.0));
        assert_eq!((inner.pages[0].page, inner.pages[1].page), (Some(6), Some(3)));
    });
}

#[test]
fn sheet_geometry_consecutive_with_space_bleed_and_margins() {
    bounded_case("sheet_geometry_consecutive_with_space_bleed_and_margins", || {
        let mut o = options(BookletType::ThreeUpConsecutive, "1-3");
        o.space_between_pages = 20.0;
        o.bleed_between_pages = 4.0;
        (o.top_margin, o.bottom_margin, o.left_margin, o.right_margin) = (5.0, 6.0, 7.0, 8.0);
        let plan = impose(&o, 3).unwrap();
        let req = SheetRequest { page_width_pt: 100.0, page_height_pt: 150.0, paper: None };
        let sheets = sheet_geometry(&plan, &o, &req).unwrap();
        let s = &sheets[0];
        assert_eq!((s.width_pt, s.height_pt, s.fold_x_pt), (355.0, 161.0, None));
        let trims: Vec<Rect> = s.pages.iter().map(|p| p.trim).collect();
        assert_eq!(trims, [rect(7.0, 5.0, 107.0, 155.0), rect(127.0, 5.0, 227.0, 155.0), rect(247.0, 5.0, 347.0, 155.0)]);
        let bleeds: Vec<Rect> = s.pages.iter().map(|p| p.bleed).collect();
        assert_eq!(bleeds, [rect(7.0, 5.0, 111.0, 155.0), rect(123.0, 5.0, 231.0, 155.0), rect(243.0, 5.0, 347.0, 155.0)]);
        // Bleed between pages is capped at half the space.
        o.bleed_between_pages = 50.0;
        let s = &sheet_geometry(&plan, &o, &req).unwrap()[0];
        assert_eq!(s.pages[1].bleed, rect(117.0, 5.0, 237.0, 155.0));
    });
}

#[test]
fn sheet_geometry_auto_adjust_margins_and_refusals() {
    bounded_case("sheet_geometry_auto_adjust_margins_and_refusals", || {
        let mut o = options(BookletType::TwoUpSaddleStitch, "1-4");
        o.auto_adjust_margins = true;
        o.top_margin = 40.0; // ignored: the pages are centred on the paper
        let plan = impose(&o, 4).unwrap();
        let paper = PaperSize { width_pt: 300.0, height_pt: 300.0 };
        let req = SheetRequest { page_width_pt: 100.0, page_height_pt: 200.0, paper: Some(paper) };
        let s = &sheet_geometry(&plan, &o, &req).unwrap()[0];
        assert_eq!((s.width_pt, s.height_pt, s.fold_x_pt), (300.0, 300.0, Some(150.0)));
        assert_eq!(s.pages[0].trim, rect(50.0, 50.0, 150.0, 250.0));
        assert_eq!(s.pages[1].trim, rect(150.0, 50.0, 250.0, 250.0));

        let no_paper = SheetRequest { paper: None, ..req };
        assert_eq!(sheet_geometry(&plan, &o, &no_paper), Err(BookletError::PaperSizeRequired));
        let small = SheetRequest { paper: Some(PaperSize { width_pt: 150.0, height_pt: 300.0 }), ..req };
        assert_eq!(sheet_geometry(&plan, &o, &small), Err(BookletError::SheetDoesNotFit));
        o.auto_adjust_margins = false;
        assert_eq!(sheet_geometry(&plan, &o, &small), Err(BookletError::SheetDoesNotFit));
        // With fixed margins the given paper is the sheet.
        o.top_margin = 0.0;
        let s = &sheet_geometry(&plan, &o, &req).unwrap()[0];
        assert_eq!((s.width_pt, s.height_pt), (300.0, 300.0));
        assert_eq!(s.pages[0].trim, rect(0.0, 0.0, 100.0, 200.0));
        for (w, h) in [(0.0, 10.0), (10.0, -1.0), (f64::NAN, 10.0), (10.0, f64::INFINITY)] {
            let bad = SheetRequest { page_width_pt: w, page_height_pt: h, paper: None };
            assert_eq!(sheet_geometry(&plan, &o, &bad), Err(BookletError::PageSizeInvalid));
        }
        let bad_paper = SheetRequest { paper: Some(PaperSize { width_pt: f64::NAN, height_pt: 10.0 }), ..req };
        assert_eq!(sheet_geometry(&plan, &o, &bad_paper), Err(BookletError::PageSizeInvalid));
        // Finite inputs whose sheet overflows are refused, not returned as infinities.
        o.left_margin = f64::MAX;
        o.right_margin = f64::MAX;
        assert_eq!(sheet_geometry(&plan, &o, &no_paper), Err(BookletError::PageSizeInvalid));
        assert_eq!(BookletError::SheetDoesNotFit.layout_error(), LayoutError::InvalidGeometry);
    });
}

// ---- diagnostics through layout report ----

#[test]
fn print_and_booklet_outcomes_report_through_observe_codes_only() {
    bounded_case("print_and_booklet_outcomes_report_through_observe_codes_only", || {
        let token = CancellationToken::default();
        let ok = impose(&options(BookletType::TwoUpSaddleStitch, "1-8"), 8).map_err(BookletError::layout_error);
        let receipt = report(&ok, 1, 3, &token, &mut observe(), &mut Collect(Vec::new())).unwrap();
        assert_eq!(receipt.outcome, Outcome::Success);
        let refused = impose(&options(BookletType::TwoUpSaddleStitch, "1-9"), 8).map_err(BookletError::layout_error);
        assert_eq!(refused, Err(LayoutError::InvalidInput));
        let receipt = report(&refused, 1, 3, &token, &mut observe(), &mut Collect(Vec::new())).unwrap();
        assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Validation));
        let print = PrintParameter::BitmapResolution.check(10.0).map_err(PrintError::layout_error);
        let receipt = report(&print, 1, 3, &token, &mut observe(), &mut Collect(Vec::new())).unwrap();
        assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Validation));
        assert_eq!(PrintError::PresetNameTooLong.layout_error(), LayoutError::BudgetExceeded);
        // Every code is a non-empty snake_case token (no free text).
        let codes = [
            BookletError::BookletTypeUnknown.code(),
            BookletError::ValueInvalid("creep").code(),
            BookletError::PageRangeEmpty.code(),
            BookletError::PageRangeTooLong.code(),
            BookletError::PageRangeOutOfDocument.code(),
            BookletError::PageCountOverBound.code(),
            BookletError::PageSizeInvalid.code(),
            BookletError::PaperSizeRequired.code(),
            PrintError::FieldNotFinite("copies").code(),
            PrintError::TextTooLong("printer").code(),
            PrintError::PresetNameEmpty.code(),
            PrintError::ParameterNotFinite(PrintParameter::SpotAngle).code(),
        ];
        for c in codes {
            assert!(!c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{c}");
        }
    });
}
