//! Lane SMB-PRESS-INK-MANAGER-1: the ink manager and PDF/X contract of STU-LAY-160 (Master Spec
//! v02.229, module 14) on press-write. Oracles are the spec's table and enumerations (seven-field
//! rows, member tokens, the 27 / 31 / 1,000 mixed-ink limits) and hand-computed separations of
//! pixel-aligned rectangles at 1 px/pt on a 100 x 100 pt page. Vendor (InDesign) parity of the ink
//! manager, of the generated swatches and of the separations preview is UNVERIFIED (D5).
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_press_read::{Limits, VectorEl, VectorMode};
use hsk_studio_press_write::*;

const BOX: [f64; 4] = [0.0, 0.0, 100.0, 100.0];
const FILL: VectorMode = VectorMode::Fill { even_odd: false };
const SPOT: &str = "PANTONE 185 C";

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<VectorEl> {
    vec![VectorEl::MoveTo([x0, y0]), VectorEl::LineTo([x1, y0]), VectorEl::LineTo([x1, y1]), VectorEl::LineTo([x0, y1]), VectorEl::Close]
}

fn draw(s: &mut Separator<'_>, r: &[VectorEl], paint: SepPaint) {
    s.draw(&SepDraw { path: r, mode: &FILL, paint, overprint: false, overprint_nonzero: false }).unwrap();
}

fn edit(m: &InkManager, e: InkEdit) -> Result<InkManager, InkError> {
    m.apply_edit(&actor(), &e).map(|o| o.manager)
}

// ---- STU-LAY-160 ink parameter table (MT-30845 AC-001..AC-004) ----

/// One bound-set row: the seven fields exactly as tabled (unknown stays unknown, never mirrored),
/// endpoints admitted, values just past them and non-finite values refused with the row's code, and
/// storage through an attributed edit that leaves the manager unchanged on refusal.
fn assert_ink_row(p: InkParameter, code: &str, hard_min: f64, hard_max: f64, unit: &str, precision: ParamPrecision, out_code: &str) {
    assert_eq!(p.code(), code);
    assert_eq!(InkParameter::from_code(code), Some(p));
    let s = p.spec();
    assert_eq!(s.parameter, p);
    assert_eq!((s.hard_min, s.hard_max), (ParamValue::Known(hard_min), ParamValue::Known(hard_max)), "{code} hard bounds");
    assert_eq!((s.soft_min, s.soft_max, s.default), (ParamValue::Unknown, ParamValue::Unknown, ParamValue::Unknown), "{code}: unknown stays unknown");
    assert_eq!(s.unit.code(), unit, "{code} unit");
    assert_eq!(s.precision, precision, "{code} precision");
    assert_eq!(p.check("Process Cyan", hard_min), Ok(hard_min));
    assert_eq!(p.check("Process Cyan", hard_max), Ok(hard_max));
    for bad in [hard_min - 0.001, hard_max + 0.001, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(p.check("Process Cyan", bad).unwrap_err().code(), out_code, "{code} {bad}");
    }
    let m = InkManager::default();
    assert_eq!(m.ink("Process Cyan").unwrap().parameter(p), None, "{code}: no default supplied");
    let set = edit(&m, InkEdit::SetParameter { ink: "Process Cyan".into(), parameter: p, value: Some(hard_max) }).unwrap();
    assert_eq!(set.ink("Process Cyan").unwrap().parameter(p), Some(hard_max));
    let refused = set.apply_edit(&actor(), &InkEdit::SetParameter { ink: "Process Cyan".into(), parameter: p, value: Some(hard_max + 1.0) });
    assert_eq!(refused.unwrap_err().code(), out_code);
    assert_eq!(set.ink("Process Cyan").unwrap().parameter(p), Some(hard_max), "refused edit leaves the manager unchanged");
    let cleared = edit(&set, InkEdit::SetParameter { ink: "Process Cyan".into(), parameter: p, value: None }).unwrap();
    assert_eq!(cleared.ink("Process Cyan").unwrap().parameter(p), None);
}

#[test]
fn ink_row_ink_angle() {
    assert_ink_row(InkParameter::InkAngle, "ink_angle", 0.0, 360.0, "deg", ParamPrecision::Unknown, "ink_angle_out_of_range");
}

#[test]
fn ink_row_ink_frequency() {
    assert_ink_row(InkParameter::InkFrequency, "ink_frequency", 1.0, 500.0, "count", ParamPrecision::Unknown, "ink_frequency_out_of_range");
}

#[test]
fn ink_row_neutral_density() {
    assert_ink_row(InkParameter::NeutralDensity, "neutral_density", 0.001, 10.0, "ratio", ParamPrecision::Digits(3), "neutral_density_out_of_range");
    // Precision 3: three decimal places are admitted as stored (no rounding), a fourth is refused.
    let p = InkParameter::NeutralDensity;
    for ok in [0.001, 1.7, 1.234, 9.999] {
        assert_eq!(p.check("Spot", ok), Ok(ok), "{ok}");
    }
    for bad in [1.2345, 0.0015, 9.9999] {
        assert_eq!(p.check("Spot", bad), Err(InkError::ParameterPrecision("Spot".into(), p)), "{bad}");
    }
    assert_eq!(p.check("Spot", 1.2345).unwrap_err().code(), "ink_parameter_precision");
    // validate() applies the same rule to a manager built by field writes.
    let mut m = InkManager::default();
    m.inks[0].neutral_density = Some(0.6105);
    assert_eq!(m.validate().unwrap_err().code(), "ink_parameter_precision");
    m.inks[0].neutral_density = Some(0.61);
    assert_eq!(m.validate(), Ok(()));
}

#[test]
fn ink_row_solidity() {
    assert_ink_row(InkParameter::Solidity, "solidity", 0.0, 1.0, "ratio", ParamPrecision::Unknown, "solidity_out_of_range");
}

// ---- ink_type, edits, conversion, aliasing (MT-30844 AC-002 typed values, MT-35695 AC-002) ----

#[test]
fn ink_type_four_members_exact_tokens_and_reused_by_plates() {
    let codes: Vec<&str> = InkType::ALL.iter().map(|t| t.code()).collect();
    assert_eq!(codes, ["ink.normal", "ink.opaque", "ink.transparent", "ink.opaque_ignore"]);
    for t in InkType::ALL {
        assert_eq!(InkType::from_code(t.code()), Some(t));
    }
    for near in ["ink.Normal", " ink.opaque", "ink.opaque-ignore", "opaque", ""] {
        assert_eq!(InkType::from_code(near), None, "{near:?} is no member");
    }
    // The same enumeration reaches the separations plates (no second ink-type vocabulary).
    let m = edit(&InkManager::default(), InkEdit::AddSpot { name: SPOT.into(), process_alternate: None }).unwrap();
    let m = edit(&m, InkEdit::SetInkType { ink: SPOT.into(), ink_type: InkType::OpaqueIgnore }).unwrap();
    let mut s = Separator::new(BOX, 1.0, &m, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Spot { name: SPOT.into(), tint: 1.0 });
    let p = s.finish();
    assert_eq!(p.plates.iter().find(|pl| pl.name == SPOT).unwrap().ink_type, InkType::OpaqueIgnore);
}

#[test]
fn ink_edits_are_attributed_to_the_caller_and_never_partial() {
    let base = InkManager::default();
    let a = ActorContext::new("acct-7", "user-7", "owner-7", "owner-user-7", "space-7", "session-7").unwrap();
    let out = base.apply_edit(&a, &InkEdit::SetParameter { ink: "Process Black".into(), parameter: InkParameter::InkAngle, value: Some(45.0) }).unwrap();
    assert_eq!(out.receipt.actor, a, "the receipt carries the caller's actor");
    assert_eq!((out.receipt.edit, out.receipt.ink.as_deref()), ("ink_edit.set_parameter", Some("Process Black")));
    assert_eq!(out.manager.ink("Process Black").unwrap().angle_deg, Some(45.0));
    assert_eq!(base, InkManager::default(), "the input manager is unchanged");
    // Captured typed error: a process ink cannot be aliased; the refusal leaves nothing behind.
    let with_spot = edit(&base, InkEdit::AddSpot { name: SPOT.into(), process_alternate: Some([0.0, 0.91, 0.76, 0.0]) }).unwrap();
    let alias = with_spot.apply_edit(&a, &InkEdit::SetAlias { ink: "Process Cyan".into(), alias: Some(SPOT.into()) });
    assert_eq!(alias.unwrap_err(), InkError::ProcessInkAliased("Process Cyan".into()));
    assert!(!with_spot.is_aliased("Process Cyan"));
    // Other typed refusals.
    let cases: [(InkEdit, &str); 7] = [
        (InkEdit::SetInkType { ink: "nope".into(), ink_type: InkType::Opaque }, "unknown_ink"),
        (InkEdit::SetAlias { ink: SPOT.into(), alias: Some("missing".into()) }, "unknown_ink"),
        (InkEdit::SetAlias { ink: SPOT.into(), alias: Some(SPOT.into()) }, "ink_alias_cycle"),
        (InkEdit::SetConvertToProcess { ink: "Process Yellow".into(), convert: true }, "process_ink_not_convertible"),
        (InkEdit::AddSpot { name: SPOT.into(), process_alternate: None }, "duplicate_ink"),
        (InkEdit::AddSpot { name: "  ".into(), process_alternate: None }, "invalid_ink_name"),
        (InkEdit::AddSpot { name: "Bad Alt".into(), process_alternate: Some([0.0, 1.5, 0.0, 0.0]) }, "process_alternate_out_of_range"),
    ];
    for (e, code) in cases {
        assert_eq!(with_spot.apply_edit(&a, &e).unwrap_err().code(), code, "{e:?}");
    }
    assert_eq!(with_spot.inks.len(), 5, "no refused edit added or changed an ink");
}

#[test]
fn spot_to_process_all_spots_aliasing_and_standard_lab_edits_drive_separation() {
    let m = InkManager::default();
    let m = edit(&m, InkEdit::AddSpot { name: "Spot A".into(), process_alternate: Some([0.1, 0.0, 0.0, 0.0]) }).unwrap();
    let m = edit(&m, InkEdit::AddSpot { name: "Spot B".into(), process_alternate: Some([0.0, 0.2, 0.0, 0.0]) }).unwrap();
    let m = edit(&m, InkEdit::AddSpot { name: "Spot C".into(), process_alternate: None }).unwrap();
    assert_eq!(m.target("Spot A"), Ok(InkTarget::Plate("Spot A".into())));
    // Per-ink conversion.
    let conv = edit(&m, InkEdit::SetConvertToProcess { ink: "Spot A".into(), convert: true }).unwrap();
    assert_eq!(conv.target("Spot A"), Ok(InkTarget::Process([0.1, 0.0, 0.0, 0.0])));
    assert_eq!(conv.target("Spot B"), Ok(InkTarget::Plate("Spot B".into())));
    // All spots to process; a spot without an alternate stays on its own plate (noted).
    let all = edit(&m, InkEdit::SetAllSpotsToProcess(true)).unwrap();
    assert!(all.all_spots_to_process);
    assert_eq!(all.target("Spot B"), Ok(InkTarget::Process([0.0, 0.2, 0.0, 0.0])));
    assert_eq!(all.target("Spot C"), Ok(InkTarget::PlateNoAlternate("Spot C".into())));
    // Aliasing: Spot C prints on Spot B's plate; removing the alias restores it.
    let aliased = edit(&m, InkEdit::SetAlias { ink: "Spot C".into(), alias: Some("Spot B".into()) }).unwrap();
    assert_eq!(aliased.target("Spot C"), Ok(InkTarget::Plate("Spot B".into())));
    let mut s = Separator::new(BOX, 1.0, &aliased, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Spot { name: "Spot C".into(), tint: 0.5 });
    let p = s.finish();
    let b = p.sample_pt(5.0, 5.0).unwrap().into_iter().find(|r| r.ink == "Spot B").unwrap();
    assert!((b.percent - 50.0).abs() < 1e-3, "{b:?}");
    assert!(p.plates.iter().all(|pl| pl.name != "Spot C"));
    let unaliased = edit(&aliased, InkEdit::SetAlias { ink: "Spot C".into(), alias: None }).unwrap();
    assert_eq!(unaliased.target("Spot C"), Ok(InkTarget::Plate("Spot C".into())));
    // Use standard Lab values for spots: stored and read back as a typed value.
    let lab = edit(&m, InkEdit::SetUseStandardLabForSpots(true)).unwrap();
    assert!(lab.use_standard_lab_for_spots && !m.use_standard_lab_for_spots);
}

// ---- Mixed-ink limits (MT-30844 AC-001) ----

#[test]
fn mixed_ink_limits_27_spots_31_inks_1000_swatches_are_typed() {
    let mut many = InkManager::default();
    for i in 0..40 {
        many = edit(&many, InkEdit::AddSpot { name: format!("S{i}"), process_alternate: None }).unwrap();
    }
    let mixed = |spots: usize, process: usize| MixedInk {
        name: "M".into(),
        parts: ProcessInk::ALL[..process].iter().map(|p| (p.name().to_string(), 0.1)).chain((0..spots).map(|i| (format!("S{i}"), 0.5))).collect(),
    };
    // 27 spots accepted, 28 refused.
    assert_eq!(mixed(27, 0).validate(&many), Ok(()));
    assert_eq!(mixed(28, 0).validate(&many), Err(InkError::MixedInkTooManySpots("M".into())));
    // 31 inks in total accepted (27 spot + 4 process), 32 refused (counted before spots).
    assert_eq!(mixed(27, 4).validate(&many), Ok(()));
    assert_eq!(mixed(28, 4).validate(&many), Err(InkError::MixedInkTooManyInks("M".into())));
    assert_eq!(mixed(40, 0).validate(&many).unwrap_err().code(), "mixed_ink_too_many_inks");
    // A part listed twice would make both counts ambiguous: refused.
    let mut dup = mixed(2, 0);
    dup.parts.push(("S0".into(), 0.2));
    assert_eq!(dup.validate(&many).unwrap_err().code(), "mixed_ink_duplicate_part");
    // Mixed-ink group: exactly 1,000 swatches generated, 1,001 refused before any is built.
    let row = |ink: &str, repeat: u32| MixedInkGroupRow { ink: ink.into(), initial: 0.0, repeat, increment: 0.1 };
    let g1000 = MixedInkGroup { name: "G".into(), rows: vec![row("S0", 9), row("S1", 9), row("S2", 9)] };
    assert_eq!(g1000.swatch_count(), Ok(1000));
    let made = g1000.generate(&many).unwrap();
    assert_eq!(made.len(), 1000);
    assert_eq!(made[999].name, "G 1000");
    let g1001 = MixedInkGroup { name: "H".into(), rows: vec![row("S0", 6), row("S1", 10), row("S2", 12)] };
    assert_eq!(g1001.swatch_count(), Err(InkError::MixedInkGroupTooLarge("H".into())));
    assert_eq!(g1001.generate(&many).unwrap_err().code(), "mixed_ink_group_too_large");
    // Hostile repeat counts: checked arithmetic, no allocation.
    let hostile = MixedInkGroup { name: "X".into(), rows: vec![row("S0", u32::MAX), row("S1", u32::MAX), row("S2", u32::MAX)] };
    assert_eq!(hostile.swatch_count().unwrap_err().code(), "mixed_ink_group_too_large");
}

#[test]
fn ink_manager_hostile_sizes_are_typed_refusals() {
    let mut m = InkManager::default();
    m.inks.extend((0..MAX_INKS - 4).map(|i| Ink::spot(&format!("H{i}"), None)));
    assert_eq!(m.inks.len(), MAX_INKS);
    assert_eq!(m.validate(), Ok(()));
    assert_eq!(edit(&m, InkEdit::AddSpot { name: "one more".into(), process_alternate: None }).unwrap_err(), InkError::TooManyInks);
    let long = "n".repeat(257);
    assert_eq!(edit(&InkManager::default(), InkEdit::AddSpot { name: long, process_alternate: None }).unwrap_err(), InkError::InvalidName);
    assert_eq!(edit(&InkManager::default(), InkEdit::AddSpot { name: "n".repeat(256), process_alternate: None }).map(|m| m.inks.len()), Ok(5));
}

// ---- PDF/X standards (STU-LAY-160; MT-35695 AC-001) ----

#[test]
fn pdfx_standards_six_members_exact_tokens() {
    let codes: Vec<&str> = PdfXStandard::ALL.iter().map(|s| s.code()).collect();
    assert_eq!(codes, ["pdfx.none", "pdfx.x1a_2001", "pdfx.x3_2002", "pdfx.x1a_2003", "pdfx.x3_2003", "pdfx.x4_2010"]);
    for s in PdfXStandard::ALL {
        assert_eq!(PdfXStandard::from_code(s.code()), Some(s));
    }
    for near in ["pdfx.x4", "pdfx.X4_2010", "PDF/X-4", "pdfx.x4_2008", ""] {
        assert_eq!(PdfXStandard::from_code(near), None, "{near:?}");
    }
    let implemented: Vec<PdfXStandard> = PdfXStandard::ALL.into_iter().filter(|s| s.implemented()).collect();
    assert_eq!(implemented, [PdfXStandard::None, PdfXStandard::X4_2010]);
    assert_eq!(PdfXStandard::X4_2010.gts_pdfx_version(), Some(PDFX4_VERSION));
    assert!(PdfXStandard::ALL.iter().filter(|s| **s != PdfXStandard::X4_2010).all(|s| s.gts_pdfx_version().is_none()));
}

/// A minimal ICC header the export accepts (size 132, CMYK data space, `acsp`, no tags).
fn cmyk_icc() -> Vec<u8> {
    let mut icc = vec![0u8; 132];
    icc[0..4].copy_from_slice(&132u32.to_be_bytes());
    icc[12..16].copy_from_slice(b"prtr");
    icc[16..20].copy_from_slice(b"CMYK");
    icc[20..24].copy_from_slice(b"Lab ");
    icc[36..40].copy_from_slice(b"acsp");
    icc
}

#[test]
fn pdfx_export_writes_only_the_implemented_identification() {
    let lim = Limits::default();
    let cancel = CancellationToken::default();
    let icc = cmyk_icc();
    let intent = OutputIntent { icc: &icc, components: 4, output_condition_identifier: "FOGRA39", output_condition: None, registry: None, subtype: OutputIntentSubtype::GtsPdfx };
    let x4 = PdfX4 { created: "D:20261101000000Z", modified: "D:20261101000000Z", trapped: false };
    let base = ExportOptions { version: "1.6", output_intent: Some(intent), ..ExportOptions::default() };
    assert_eq!(base.pdfx_standard(), PdfXStandard::None);
    // The four standards this crate does not implement are refused, never written or claimed.
    for s in [PdfXStandard::X1a2001, PdfXStandard::X3_2002, PdfXStandard::X1a2003, PdfXStandard::X3_2003] {
        let e = base.with_pdfx_standard(s, Some(x4)).unwrap_err();
        assert_eq!(e, ExportError::PdfXStandardNotImplemented(s));
        assert_eq!(e.code(), "pdfx_standard_not_implemented");
    }
    assert_eq!(base.with_pdfx_standard(PdfXStandard::X4_2010, None).unwrap_err(), ExportError::PdfXRequirement("pdfx4_identification"));
    assert_eq!(base.with_pdfx_standard(PdfXStandard::None, Some(x4)).unwrap_err(), ExportError::PdfXRequirement("identification_without_standard"));
    // pdfx.x4_2010: the X-4 identification is written and read back; conformance stays not_proven.
    let opts = base.with_pdfx_standard(PdfXStandard::X4_2010, Some(x4)).unwrap();
    assert_eq!(opts.pdfx_standard(), PdfXStandard::X4_2010);
    let mut ex = PdfExport::new(opts, &lim).unwrap();
    ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 100.0, 100.0], trim_box_pt: Some([5.0, 5.0, 95.0, 95.0]), bleed_box_pt: None }).unwrap();
    let (_, receipt) = ex.finish(&cancel).unwrap();
    assert_eq!(receipt.effective_pdfx_profile.as_deref(), Some(PDFX4_VERSION));
    assert_eq!(receipt.effective_pdfx_standard(), Some(PdfXStandard::X4_2010));
    assert_eq!(receipt.conformance, "not_proven");
    // pdfx.none: nothing written.
    let none = opts.with_pdfx_standard(PdfXStandard::None, None).unwrap();
    assert!(none.pdfx4.is_none());
    let mut ex = PdfExport::new(none, &lim).unwrap();
    ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 100.0, 100.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    let (bytes, receipt) = ex.finish(&cancel).unwrap();
    assert_eq!(receipt.effective_pdfx_standard(), Some(PdfXStandard::None));
    assert!(!String::from_utf8_lossy(&bytes).contains("GTS_PDFXVersion"));
}

// ---- Separations preview readouts (STU-LAY-160; MT-30844 AC-002, MT-35695 AC-005) ----

/// C 50 % over (10..60)^2 and the spot at 100 % over (40..90)^2, knocking out.
fn preview(inks: &InkManager) -> SeparationPreview {
    let mut s = Separator::new(BOX, 1.0, inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(10.0, 10.0, 60.0, 60.0), SepPaint::Process([0.5, 0.0, 0.0, 0.0]));
    draw(&mut s, &rect(40.0, 40.0, 90.0, 90.0), SepPaint::Spot { name: SPOT.into(), tint: 1.0 });
    s.finish()
}

#[test]
fn separations_readouts_plate_switches_threshold_and_coverage_are_typed() {
    let inks = edit(&InkManager::default(), InkEdit::AddSpot { name: SPOT.into(), process_alternate: None }).unwrap();
    let p = preview(&inks);
    let state = SeparationsViewState { plates: vec![PlateToggle { ink: SPOT.into(), shown: false }], ink_limit_threshold: None };
    let r = p.readouts(&state).unwrap();
    let names: Vec<(&str, bool)> = r.plates.iter().map(|pl| (pl.ink.as_str(), pl.shown)).collect();
    assert_eq!(names, [("Process Cyan", true), ("Process Magenta", true), ("Process Yellow", true), ("Process Black", true), (SPOT, false)]);
    // Coverage readouts, hand-computed: C 50 % on 2500 - 400 = 2100 px (the spot knocks out 20 x 20);
    // the spot 100 % on 2500 px; mean over 10,000 px.
    let cyan = &r.plates[0].coverage;
    assert_eq!(cyan.inked_pixels, 2100);
    assert!((cyan.max_percent - 50.0).abs() < 1e-3 && (cyan.mean_percent - 10.5).abs() < 1e-3, "{cyan:?}");
    let spot = &r.plates[4];
    assert_eq!((spot.coverage.inked_pixels, spot.process, spot.ink_type), (2500, None, InkType::Normal));
    assert!((spot.coverage.mean_percent - 25.0).abs() < 1e-3);
    assert_eq!(r.ink_limit_view, None);
    // The state drives the render: the composite with the spot switched off.
    let view = p.view_for(&state).unwrap();
    assert_eq!(view, PlateView::Composite(vec![true, true, true, true, false]));
    assert_eq!(p.render(&view), p.render(&PlateView::Composite(vec![true, true, true, true, false])));
    assert_ne!(p.render(&view), p.render(&PlateView::Composite(vec![])), "switching the spot off changes the composite");
    // Ink-limit view at 80 %: only the spot square totals over 80 (100 %); 50 x 50 = 2500 px marked.
    let limit = SeparationsViewState { plates: vec![], ink_limit_threshold: Some(80.0) };
    let v = p.readouts(&limit).unwrap().ink_limit_view.unwrap();
    assert_eq!((v.threshold_percent, v.structural_max_percent, v.pixels_over), (80.0, 500.0, 2500));
    assert!((v.max_total_percent - 100.0).abs() < 1e-3);
    assert_eq!(v.first_px_over, Some((40, 10)), "row-major first pixel of the square (canvas rows top-down)");
    let red = p.render(&p.view_for(&limit).unwrap()).chunks(3).filter(|c| **c == [255u8, 0, 0]).count() as u64;
    assert_eq!(red, v.pixels_over, "the typed readout counts exactly what the view marks");
    // Typed refusals.
    let bad = |plates: Vec<PlateToggle>, t: Option<f64>| p.readouts(&SeparationsViewState { plates, ink_limit_threshold: t }).unwrap_err().code();
    let on = |ink: &str| PlateToggle { ink: ink.into(), shown: true };
    assert_eq!(bad(vec![on("Nope")], None), "separations_view_unknown_plate");
    assert_eq!(bad(vec![on(SPOT), on(SPOT)], None), "separations_view_duplicate_plate");
    assert_eq!(bad(vec![on(SPOT); MAX_SEP_PLATES + 1], None), "separations_view_too_many_toggles");
    for t in [-0.1, 500.1, f64::NAN, f64::INFINITY] {
        assert_eq!(bad(vec![], Some(t)), "separations_view_threshold_out_of_range", "{t}");
    }
    assert_eq!(p.readouts(&SeparationsViewState { plates: vec![], ink_limit_threshold: Some(500.0) }).unwrap().ink_limit_view.unwrap().pixels_over, 0);
}
