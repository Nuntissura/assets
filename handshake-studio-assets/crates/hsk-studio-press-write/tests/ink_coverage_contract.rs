//! Lane SMB-PRESS-INK-COVERAGE-1: the total ink coverage contract STU-LAY-250..254 (Master Spec
//! v02.226, module 14) on press-write's separations path. Every expected value is hand-computed from
//! pixel-aligned rectangles at 1 px/pt (72 dpi) on a 100 x 100 pt page, so a rectangle covers its
//! pixels fully. Vendor (InDesign/Acrobat) parity of coverage, of the opaque-ink rule and of the
//! stage attribution is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_press_read::{VectorEl, VectorMode, read_vector_document};
use hsk_studio_press_write::*;

const BOX: [f64; 4] = [0.0, 0.0, 100.0, 100.0];
const FILL: VectorMode = VectorMode::Fill { even_odd: false };
const SPOT: &str = "PANTONE 185 C";

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<VectorEl> {
    vec![VectorEl::MoveTo([x0, y0]), VectorEl::LineTo([x1, y0]), VectorEl::LineTo([x1, y1]), VectorEl::LineTo([x0, y1]), VectorEl::Close]
}

fn draw(s: &mut Separator<'_>, r: &[VectorEl], paint: SepPaint, overprint: bool) {
    s.draw(&SepDraw { path: r, mode: &FILL, paint, overprint, overprint_nonzero: false }).unwrap();
}

fn rule(severity: InkLimitSeverity) -> InkLimitRule {
    InkLimitRule { rule_id: "ink-limit-1".into(), severity, scope: InkLimitScope::Page }
}

fn page<'a>(p: &'a SeparationPreview) -> CoveragePage<'a> {
    CoveragePage { reference: PageReference { page: "p1".into(), spread: None }, preview: p, unmeasured: Vec::new() }
}

fn sites_total(total: f64) -> InkLimitSites {
    let mut s = InkLimitSites::default();
    s.declare(InkLimitSource::PreflightRuleInstance, InkLimits { total_ink_limit: Some(total), ..InkLimits::default() }).unwrap();
    s
}

fn eval(p: &SeparationPreview, sites: &InkLimitSites, severity: InkLimitSeverity) -> InkCoverageReport {
    let pages = [page(p)];
    evaluate_ink_coverage(&CoverageRequest { rule: rule(severity), sites, sample_resolution: 72.0, pages: &pages }).unwrap()
}

fn total_eval(r: &InkCoverageReport) -> &LimitEvaluation {
    r.evaluations.iter().find(|e| e.parameter == CoverageParameter::TotalInkLimit).unwrap()
}

/// C 50 % over (10..60)^2, then the spot at 100 % over (40..90)^2 (overprinting or knocking out).
fn two_rects(inks: &InkManager, overprint: bool) -> SeparationPreview {
    let mut s = Separator::new(BOX, 1.0, inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(10.0, 10.0, 60.0, 60.0), SepPaint::Process([0.5, 0.0, 0.0, 0.0]), false);
    draw(&mut s, &rect(40.0, 40.0, 90.0, 90.0), SepPaint::Spot { name: SPOT.into(), tint: 1.0 }, overprint);
    s.finish()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

// ---- STU-LAY-250: parameter table (MT-35425 AC-001/AC-003, MT-35426 AC-001..AC-005) ----

#[test]
fn coverage_parameter_table_seven_fields_unknown_never_mirrored() {
    use ParamValue::{Known, Unknown};
    let row = |p: CoverageParameter| {
        let s = p.spec();
        assert_eq!(s.parameter, p);
        (s.hard_min, s.hard_max, s.soft_min, s.soft_max, s.default, s.unit, s.precision)
    };
    assert_eq!(
        row(CoverageParameter::TotalInkLimit),
        (Known(0.0), Known(400.0), Unknown, Unknown, Unknown, CoverageUnit::Percent, ParamPrecision::Unknown)
    );
    assert_eq!(
        row(CoverageParameter::SingleInkLimit),
        (Known(0.0), Known(100.0), Unknown, Unknown, Unknown, CoverageUnit::Percent, ParamPrecision::Unknown)
    );
    assert_eq!(
        row(CoverageParameter::BlackInkLimit),
        (Known(0.0), Known(100.0), Unknown, Unknown, Unknown, CoverageUnit::Percent, ParamPrecision::Unknown)
    );
    // hard_max unknown stays unknown: not filled from soft_max, not invented.
    assert_eq!(
        row(CoverageParameter::CoverageWarningMargin),
        (Known(0.0), Unknown, Unknown, Unknown, Unknown, CoverageUnit::Percent, ParamPrecision::Unknown)
    );
    assert_eq!(
        row(CoverageParameter::CoverageSampleResolution),
        (Unknown, Unknown, Unknown, Unknown, Unknown, CoverageUnit::Dpi, ParamPrecision::Digits(0))
    );
    assert_eq!(CoverageParameter::ALL.map(|p| p.code()), [
        "total_ink_limit",
        "single_ink_limit",
        "black_ink_limit",
        "coverage_warning_margin",
        "coverage_sample_resolution"
    ]);
    assert_eq!((CoverageUnit::Percent.code(), CoverageUnit::Dpi.code()), ("percent", "dpi"));
    for p in CoverageParameter::ALL {
        assert_eq!(CoverageParameter::from_code(p.code()), Some(p));
    }
    // The three limits are three stored fields of one site: each is declared without the others.
    let mut sites = InkLimitSites::default();
    sites.declare(InkLimitSource::DocumentPolicy, InkLimits { black_ink_limit: Some(95.0), ..InkLimits::default() }).unwrap();
    let r = sites.resolve();
    assert_eq!((r.black_ink_limit.value, r.total_ink_limit.value, r.single_ink_limit.value), (Some(95.0), None, None));
    // The margin is its own field, never inferred from a limit.
    assert_eq!(r.coverage_warning_margin.value, None);
}

#[test]
fn total_ink_hard_max_scales_with_ink_count_never_clamped_to_400() {
    use ParamValue::Known;
    let t = CoverageParameter::TotalInkLimit;
    assert_eq!((t.hard_max_for_inks(4), t.hard_max_for_inks(5), t.hard_max_for_inks(7)), (Known(400.0), Known(500.0), Known(700.0)));
    assert_eq!(CoverageParameter::SingleInkLimit.hard_max_for_inks(7), Known(100.0));
    // Declaring 450 is not refused at 400 and not clamped.
    let sites = sites_total(450.0);
    assert_eq!(sites.resolve().total_ink_limit.value, Some(450.0));
    // On a five-ink separation (4 process + 1 spot) 450 is inside 0..=500 and is applied as typed.
    let inks = InkManager::default();
    let five = two_rects(&inks, true);
    assert_eq!(five.plates.len(), 5);
    let r = eval(&five, &sites, InkLimitSeverity::Error);
    assert_eq!((total_eval(&r).effective_limit, total_eval(&r).check_state), (Some(450.0), InkLimitCheckState::WithinLimit));
    // On a four-ink separation 450 is over the scaled hard_max 400: a typed validation error, not a clamp.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([1.0, 1.0, 1.0, 1.0]), false);
    let four = s.finish();
    let pages = [page(&four)];
    let err = evaluate_ink_coverage(&CoverageRequest { rule: rule(InkLimitSeverity::Error), sites: &sites, sample_resolution: 72.0, pages: &pages })
        .unwrap_err();
    assert_eq!(
        err,
        CoverageError::OutOfHardBounds {
            parameter: CoverageParameter::TotalInkLimit,
            source: InkLimitSource::PreflightRuleInstance,
            value: 450.0,
            hard_min: Known(0.0),
            hard_max: Known(400.0)
        }
    );
    assert_eq!((err.code(), err.field(), err.value()), ("ink_limit_out_of_hard_bounds", Some("total_ink_limit"), Some(450.0)));
    assert!(!err.message().is_empty());
    // Static hard bounds at declaration; the margin's unknown hard_max is not invented.
    let mut d = InkLimitSites::default();
    let site = InkLimitSource::ExportRecipe;
    assert!(d.declare(site, InkLimits { single_ink_limit: Some(100.5), ..InkLimits::default() }).is_err());
    assert!(d.declare(site, InkLimits { black_ink_limit: Some(-1.0), ..InkLimits::default() }).is_err());
    assert!(d.declare(site, InkLimits { total_ink_limit: Some(-0.5), ..InkLimits::default() }).is_err());
    assert!(d.declare(site, InkLimits { coverage_warning_margin: Some(-1.0), ..InkLimits::default() }).is_err());
    assert_eq!(
        d.declare(site, InkLimits { total_ink_limit: Some(f64::NAN), ..InkLimits::default() }).unwrap_err().code(),
        "ink_limit_not_finite"
    );
    assert!(d.declare(site, InkLimits { coverage_warning_margin: Some(1.0e9), ..InkLimits::default() }).is_ok());
    // A refused declaration leaves the site as it was.
    assert_eq!(d.declared(site).map(|l| l.coverage_warning_margin), Some(Some(1.0e9)));
}

// ---- STU-LAY-251: enumerations and precedence (MT-35427, MT-35428) ----

#[test]
fn ink_limit_enumerations_exact_members_round_trip() {
    assert_eq!(InkLimitSource::ALL.map(|m| m.code()), [
        "ink_limit_source.preflight_rule_instance",
        "ink_limit_source.export_recipe",
        "ink_limit_source.document_policy",
        "ink_limit_source.output_intent_profile",
        "ink_limit_source.press_condition_preset",
        "ink_limit_source.undeclared"
    ]);
    assert_eq!(InkLimitSeverity::ALL.map(|m| m.code()), ["severity.disabled", "severity.error", "severity.warning", "severity.informational"]);
    assert_eq!(InkLimitScope::ALL.map(|m| m.code()), [
        "ink_limit_scope.document",
        "ink_limit_scope.spread",
        "ink_limit_scope.page",
        "ink_limit_scope.object",
        "ink_limit_scope.plate"
    ]);
    assert_eq!(InkLimitCheckState::ALL.map(|m| m.code()), [
        "check_state.not_checked",
        "check_state.within_limit",
        "check_state.within_warning_margin",
        "check_state.over_limit",
        "check_state.indeterminate"
    ]);
    assert_eq!(CoverageInputStage::ALL.map(|m| m.code()), [
        "stage.spot_to_process_conversion",
        "stage.ink_alias_resolution",
        "stage.overprint_composition",
        "stage.transparency_flattening",
        "stage.placed_image_separation",
        "stage.authored_rich_black",
        "stage.opaque_ink_handling"
    ]);
    assert_eq!(CoverageMetric::ALL.map(|m| m.code()), ["metric.maximum", "metric.mean", "metric.area_above_limit"]);
    for m in InkLimitSource::ALL {
        assert_eq!(InkLimitSource::from_code(m.code()), Some(m));
    }
    for m in InkLimitSeverity::ALL {
        assert_eq!(InkLimitSeverity::from_code(m.code()), Some(m));
    }
    for m in InkLimitScope::ALL {
        assert_eq!(InkLimitScope::from_code(m.code()), Some(m));
    }
    for m in InkLimitCheckState::ALL {
        assert_eq!(InkLimitCheckState::from_code(m.code()), Some(m));
    }
    for m in CoverageInputStage::ALL {
        assert_eq!(CoverageInputStage::from_code(m.code()), Some(m));
    }
    for m in CoverageMetric::ALL {
        assert_eq!(CoverageMetric::from_code(m.code()), Some(m));
    }
    // No member is silently mapped onto another: unknown or near-miss tokens are refused.
    for bad in ["severity.fatal", "ink_limit_source.default", "check_state.pass", "stage.flattening", "metric.median", "ERROR", ""] {
        assert_eq!(
            (
                InkLimitSeverity::from_code(bad),
                InkLimitSource::from_code(bad),
                InkLimitCheckState::from_code(bad),
                CoverageInputStage::from_code(bad),
                CoverageMetric::from_code(bad)
            ),
            (None, None, None, None, None)
        );
    }
    // Precedence order is the member order.
    let mut sorted = InkLimitSource::ALL;
    sorted.sort();
    assert_eq!(sorted, InkLimitSource::ALL);
}

#[test]
fn effective_limit_is_first_declared_site_and_records_its_source() {
    let mut sites = InkLimitSites::default();
    sites.declare(InkLimitSource::OutputIntentProfile, InkLimits { total_ink_limit: Some(300.0), ..InkLimits::default() }).unwrap();
    sites
        .declare(InkLimitSource::ExportRecipe, InkLimits { total_ink_limit: Some(320.0), black_ink_limit: Some(95.0), ..InkLimits::default() })
        .unwrap();
    sites
        .declare(
            InkLimitSource::PressConditionPreset,
            InkLimits { single_ink_limit: Some(98.0), coverage_warning_margin: Some(10.0), ..InkLimits::default() },
        )
        .unwrap();
    let r = sites.resolve();
    // The export recipe precedes the output intent; each value records the site that supplied it.
    assert_eq!((r.total_ink_limit.value, r.total_ink_limit.source), (Some(320.0), InkLimitSource::ExportRecipe));
    assert_eq!((r.black_ink_limit.value, r.black_ink_limit.source), (Some(95.0), InkLimitSource::ExportRecipe));
    assert_eq!((r.single_ink_limit.value, r.single_ink_limit.source), (Some(98.0), InkLimitSource::PressConditionPreset));
    assert_eq!((r.coverage_warning_margin.value, r.coverage_warning_margin.source), (Some(10.0), InkLimitSource::PressConditionPreset));
    // An operator-typed rule instance wins over every other site.
    sites.declare(InkLimitSource::PreflightRuleInstance, InkLimits { total_ink_limit: Some(280.0), ..InkLimits::default() }).unwrap();
    let r = sites.resolve();
    assert_eq!((r.total_ink_limit.value, r.total_ink_limit.source), (Some(280.0), InkLimitSource::PreflightRuleInstance));
    assert_eq!(r.get(CoverageParameter::TotalInkLimit), r.total_ink_limit);
    // `undeclared` is terminal: it is not a site a value can be declared at.
    assert_eq!(
        sites.declare(InkLimitSource::Undeclared, InkLimits { total_ink_limit: Some(1.0), ..InkLimits::default() }).unwrap_err().code(),
        "ink_limit_undeclared_is_not_a_site"
    );
    // The evaluation and its finding carry the source.
    let inks = InkManager::default();
    let p = two_rects(&inks, true);
    sites.declare(InkLimitSource::PreflightRuleInstance, InkLimits { total_ink_limit: Some(120.0), ..InkLimits::default() }).unwrap();
    let rep = eval(&p, &sites, InkLimitSeverity::Error);
    assert_eq!(total_eval(&rep).limit_source, InkLimitSource::PreflightRuleInstance);
    assert_eq!(rep.findings[0].limit_source, InkLimitSource::PreflightRuleInstance);
}

// ---- STU-LAY-251 / STU-LAY-254: undeclared is not_checked, never a pass (MT-35427 AC-003/004, MT-35432 AC-001) ----

#[test]
fn studio_ships_undeclared_and_reports_not_checked_never_a_pass() {
    let inks = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 50.0, 50.0), SepPaint::Process([1.0, 1.0, 1.0, 1.0]), false);
    let p = s.finish();
    // What Studio ships: no site declares anything.
    let shipped = InkLimitSites::default();
    let r = shipped.resolve();
    for p in [CoverageParameter::TotalInkLimit, CoverageParameter::SingleInkLimit, CoverageParameter::BlackInkLimit] {
        assert_eq!((r.get(p).value, r.get(p).source), (None, InkLimitSource::Undeclared));
    }
    // 400 % ink on the page, still not_checked: undeclared is not zero, not 400 and not a pass.
    let rep = eval(&p, &shipped, InkLimitSeverity::Error);
    assert_eq!(rep.check_state, InkLimitCheckState::NotChecked);
    assert!(rep.findings.is_empty());
    for e in &rep.evaluations {
        assert_eq!((e.check_state, e.effective_limit, e.limit_source), (InkLimitCheckState::NotChecked, None, InkLimitSource::Undeclared));
        assert_ne!(e.check_state, InkLimitCheckState::WithinLimit);
    }
    assert_eq!(rep.evaluations.len(), 3);
    // A disabled rule instance is not evaluated either (not_checked, no finding).
    let off = eval(&p, &sites_total(100.0), InkLimitSeverity::Disabled);
    assert_eq!((off.check_state, off.findings.len()), (InkLimitCheckState::NotChecked, 0));
    // Once a site declares a limit the same page is checked.
    let on = eval(&p, &sites_total(300.0), InkLimitSeverity::Error);
    assert_eq!(on.check_state, InkLimitCheckState::OverLimit);
}

#[test]
fn published_reference_figures_keep_ranges_and_are_never_a_default() {
    let rows = &PUBLISHED_INK_LIMIT_REFERENCE;
    assert_eq!(rows.len(), 9);
    // A single published figure with a published variant beside it.
    assert_eq!((rows[0].low_percent, rows[0].high_percent, rows[0].variant_percent), (330.0, 330.0, Some(300.0)));
    // Ranges stay ranges, never reduced to a midpoint.
    let ranges: Vec<(f64, f64)> = rows.iter().filter(|r| r.low_percent != r.high_percent).map(|r| (r.low_percent, r.high_percent)).collect();
    assert_eq!(ranges, vec![(260.0, 300.0), (300.0, 320.0), (240.0, 260.0), (300.0, 350.0)]);
    assert!(rows.iter().all(|r| !r.provenance.is_empty() && !r.print_condition.is_empty()));
    // The table exists and still nothing resolves from it.
    assert_eq!(InkLimitSites::default().resolve().total_ink_limit.source, InkLimitSource::Undeclared);
}

// ---- STU-LAY-251: indeterminate is its own state (MT-35427 AC-005) ----

#[test]
fn unseparable_content_is_indeterminate_not_not_checked_or_within() {
    let inks = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([0.2, 0.0, 0.0, 0.0]), false);
    let p = s.finish();
    let sites = sites_total(300.0);
    // Clean page: within_limit.
    assert_eq!(eval(&p, &sites, InkLimitSeverity::Error).check_state, InkLimitCheckState::WithinLimit);
    // A placed asset whose link is missing cannot be measured.
    let mut pg = page(&p);
    pg.unmeasured.push(IndeterminateCause::MissingLink { object: Some(ObjectRef::Placed { link: "links/photo.tif".into() }) });
    let pages = [pg];
    let rep =
        evaluate_ink_coverage(&CoverageRequest { rule: rule(InkLimitSeverity::Warning), sites: &sites, sample_resolution: 72.0, pages: &pages }).unwrap();
    assert_eq!(rep.check_state, InkLimitCheckState::Indeterminate);
    assert_eq!(rep.findings.len(), 1);
    let f = &rep.findings[0];
    assert_eq!((f.check_state, f.severity, f.introduced_by), (InkLimitCheckState::Indeterminate, InkLimitSeverity::Warning, None));
    assert_eq!(rep.indeterminate_causes.iter().map(|c| c.code()).collect::<Vec<_>>(), ["missing_link"]);
    // A PDF page with an RGB object and no colour engine: the profile is unresolvable (and PDF
    // overprint is not read), so the page is indeterminate.
    let content = b"0.5 0 0 0 k 10 10 50 50 re f\n1 0 0 rg 0 0 5 5 re f\n";
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend(content);
    stream.extend(b"\nendstream");
    let objects: [Vec<u8>; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        stream,
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend(b"xref\n0 5\n0000000000 65535 f \n");
    for o in offsets {
        pdf.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    let cancel = CancellationToken::default();
    let doc = read_vector_document(&pdf, 1 << 20, &cancel).unwrap();
    let sep = separate_vector_page(&doc.pages[0], 1.0, &inks, None, SepLimits::default(), &cancel).unwrap();
    assert_eq!(sep.unseparated, 1);
    let rep = eval(&sep, &sites, InkLimitSeverity::Error);
    assert_eq!(rep.check_state, InkLimitCheckState::Indeterminate);
    let causes: Vec<&str> = rep.indeterminate_causes.iter().map(|c| c.code()).collect();
    assert_eq!(causes, ["unresolvable_profile", "pdf_overprint_not_read"]);
    // An undeclared limit stays not_checked even when the page is indeterminate.
    assert_eq!(eval(&sep, &InkLimitSites::default(), InkLimitSeverity::Error).check_state, InkLimitCheckState::NotChecked);
}

// ---- STU-LAY-252: evaluation on composed, separated device values (MT-35429, MT-35430) ----

#[test]
fn evaluation_measures_separated_device_values_with_three_metrics_at_declared_resolution() {
    // A spot converted to process through its alternate (C 0 M 91 Y 76 K 0): authored as one 100 %
    // swatch, it reaches the plates as 167 %.
    let mut inks = InkManager::default();
    let mut c = Ink::spot("Spot C", Some([0.0, 0.91, 0.76, 0.0]));
    c.convert_to_process = true;
    inks.inks.push(c);
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Spot { name: "Spot C".into(), tint: 1.0 }, false);
    let p = s.finish();
    assert_eq!(p.sample_resolution(), 72.0);
    let rep = eval(&p, &sites_total(150.0), InkLimitSeverity::Error);
    assert_eq!(rep.check_state, InkLimitCheckState::OverLimit);
    assert_eq!(rep.sample_resolution, 72.0);
    let m = total_eval(&rep).metrics.unwrap();
    // 100 pixels at 167 % on a 10000-pixel page: mean 1.67 %, 1 % of the region above the limit.
    assert!(close(m.maximum, 167.0) && close(m.mean, 1.67) && close(m.area_above_limit, 1.0), "{m:?}");
    assert_eq!(
        CoverageMetric::ALL.map(|k| m.get(k)),
        [m.maximum, m.mean, m.area_above_limit]
    );
    let f = &rep.findings[0];
    assert_eq!(f.introduced_by, Some(CoverageInputStage::SpotToProcessConversion));
    assert_eq!(f.sample_resolution, 72.0);
    // The same swatch on its own plate (no conversion) is 100 %: within the limit.
    let plain = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &plain, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Spot { name: "Spot C".into(), tint: 1.0 }, false);
    assert_eq!(eval(&s.finish(), &sites_total(150.0), InkLimitSeverity::Error).check_state, InkLimitCheckState::WithinLimit);
    // The declared sample resolution must be the one the separation ran at.
    let pages = [page(&p)];
    let sites = sites_total(150.0);
    assert_eq!(
        evaluate_ink_coverage(&CoverageRequest { rule: rule(InkLimitSeverity::Error), sites: &sites, sample_resolution: 150.0, pages: &pages }),
        Err(CoverageError::SampleResolutionMismatch { declared: 150.0, separated: 72.0 })
    );
    for bad in [0.0, -72.0, f64::NAN, f64::INFINITY] {
        assert_eq!(
            evaluate_ink_coverage(&CoverageRequest { rule: rule(InkLimitSeverity::Error), sites: &sites, sample_resolution: bad, pages: &pages })
                .unwrap_err()
                .code(),
            "invalid_sample_resolution"
        );
    }
}

#[test]
fn warning_margin_lowers_severity_and_single_black_limits_are_separate() {
    let inks = InkManager::default();
    let p = two_rects(&inks, true); // overlap 150 %
    let mut sites = InkLimitSites::default();
    sites
        .declare(
            InkLimitSource::DocumentPolicy,
            InkLimits { total_ink_limit: Some(160.0), coverage_warning_margin: Some(20.0), single_ink_limit: Some(90.0), ..InkLimits::default() },
        )
        .unwrap();
    let rep = eval(&p, &sites, InkLimitSeverity::Error);
    // 150 is within (160 - 20, 160]: a finding at one severity lower.
    assert_eq!(rep.check_state, InkLimitCheckState::WithinWarningMargin);
    let total = rep.findings.iter().find(|f| f.limit == CoverageParameter::TotalInkLimit).unwrap();
    assert_eq!((total.check_state, total.severity), (InkLimitCheckState::WithinWarningMargin, InkLimitSeverity::Warning));
    // The single-ink limit has its own evaluation: the spot plate is at 100 % > 90 %.
    let single = rep.findings.iter().find(|f| f.limit == CoverageParameter::SingleInkLimit).unwrap();
    assert_eq!((single.check_state, single.severity), (InkLimitCheckState::OverLimit, InkLimitSeverity::Error));
    assert!(single.per_ink_contribution.iter().any(|r| r.ink == SPOT && r.percent == 100.0));
    // Black is undeclared: not_checked, no finding.
    let black = rep.evaluations.iter().find(|e| e.parameter == CoverageParameter::BlackInkLimit).unwrap();
    assert_eq!(black.check_state, InkLimitCheckState::NotChecked);
    assert!(rep.findings.iter().all(|f| f.limit != CoverageParameter::BlackInkLimit));
    assert_eq!(
        InkLimitSeverity::ALL.map(|s| s.lowered()),
        [InkLimitSeverity::Disabled, InkLimitSeverity::Warning, InkLimitSeverity::Informational, InkLimitSeverity::Informational]
    );
}

#[test]
fn opaque_inks_contribute_per_declared_ink_type_not_a_plain_sum() {
    let mut inks = InkManager::default();
    let mut metallic = Ink::spot("Metallic", None);
    metallic.ink_type = InkType::Opaque;
    inks.inks.push(metallic);
    let build = |inks: &InkManager, tint: f64| {
        let mut s = Separator::new(BOX, 1.0, inks, SepLimits::default()).unwrap();
        draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([1.0, 1.0, 0.0, 0.0]), false);
        draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Spot { name: "Metallic".into(), tint }, true);
        s.finish()
    };
    // C 100 + M 100 under a 100 % opaque ink: the opaque ink obscures them, total 100 %, not 300 %.
    let solid = build(&inks, 1.0);
    assert_eq!(solid.plates.iter().map(|p| p.ink_type).collect::<Vec<_>>()[4], InkType::Opaque);
    assert!(close(solid.total_pt(5.0, 5.0).unwrap(), 100.0));
    assert_eq!(eval(&solid, &sites_total(250.0), InkLimitSeverity::Error).check_state, InkLimitCheckState::WithinLimit);
    // At 50 %: 50 + (100 + 100) x (1 - 0.5) = 150 %.
    assert!(close(build(&inks, 0.5).total_pt(5.0, 5.0).unwrap(), 150.0));
    // opaque_ignore obscures the same way; normal and transparent inks add.
    for (ty, want) in [(InkType::OpaqueIgnore, 100.0), (InkType::Normal, 300.0), (InkType::Transparent, 300.0)] {
        let mut m = inks.clone();
        m.inks[4].ink_type = ty;
        assert!(close(build(&m, 1.0).total_pt(5.0, 5.0).unwrap(), want), "{ty:?}");
    }
    let mut normal = inks.clone();
    normal.inks[4].ink_type = InkType::Normal;
    assert_eq!(eval(&build(&normal, 1.0), &sites_total(250.0), InkLimitSeverity::Error).check_state, InkLimitCheckState::OverLimit);
    // An opaque ink whose own tone crosses the limit is attributed to opaque ink handling.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(20.0, 20.0, 30.0, 30.0), SepPaint::Spot { name: "Metallic".into(), tint: 0.95 }, false);
    let rep = eval(&s.finish(), &sites_total(90.0), InkLimitSeverity::Error);
    assert_eq!(rep.findings[0].introduced_by, Some(CoverageInputStage::OpaqueInkHandling));
}

#[test]
fn introduced_by_names_live_stages_and_never_unperformed_ones() {
    let live: Vec<CoverageInputStage> = CoverageInputStage::ALL.into_iter().filter(|s| s.performed()).collect();
    assert_eq!(
        live,
        [
            CoverageInputStage::SpotToProcessConversion,
            CoverageInputStage::InkAliasResolution,
            CoverageInputStage::OverprintComposition,
            CoverageInputStage::AuthoredRichBlack,
            CoverageInputStage::OpaqueInkHandling
        ]
    );
    let inks = InkManager::default();
    // Overprint: the knockout would leave 100 % under a 120 % limit; overprinting keeps C under the spot.
    let rep = eval(&two_rects(&inks, true), &sites_total(120.0), InkLimitSeverity::Error);
    assert_eq!(rep.findings[0].introduced_by, Some(CoverageInputStage::OverprintComposition));
    // Knockout: 100 % max, within.
    assert_eq!(eval(&two_rects(&inks, false), &sites_total(120.0), InkLimitSeverity::Error).check_state, InkLimitCheckState::WithinLimit);
    // Authored rich black C 60 M 40 Y 40 K 100 = 240 %.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([0.6, 0.4, 0.4, 1.0]), false);
    let rep = eval(&s.finish(), &sites_total(200.0), InkLimitSeverity::Error);
    assert_eq!(rep.findings[0].introduced_by, Some(CoverageInputStage::AuthoredRichBlack));
    // Registration (separation All) is a hand-typed registration swatch.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Registration(1.0), false);
    let rep = eval(&s.finish(), &sites_total(300.0), InkLimitSeverity::Error);
    assert_eq!(rep.findings[0].introduced_by, Some(CoverageInputStage::AuthoredRichBlack));
    // Ink alias: a mixed ink whose spot part is aliased onto another spot's plate.
    let mut al = InkManager::default();
    let mut a = Ink::spot("Spot A", None);
    a.alias = Some("Spot B".into());
    al.inks.extend([a, Ink::spot("Spot B", None)]);
    let mut s = Separator::new(BOX, 1.0, &al, SepLimits::default()).unwrap();
    let mixed = MixedInk { name: "Mix".into(), parts: vec![("Process Cyan".into(), 1.0), ("Spot A".into(), 1.0)] };
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Mixed { ink: mixed, tint: 1.0 }, false);
    let rep = eval(&s.finish(), &sites_total(150.0), InkLimitSeverity::Error);
    assert_eq!(rep.findings[0].introduced_by, Some(CoverageInputStage::InkAliasResolution));
    // Plain authored process values (C 100 M 100 Y 100, no black): no enumerated stage raised them.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([1.0, 1.0, 1.0, 0.0]), false);
    let rep = eval(&s.finish(), &sites_total(250.0), InkLimitSeverity::Error);
    assert_eq!((rep.check_state, rep.findings[0].introduced_by), (InkLimitCheckState::OverLimit, None));
    // Transparency is painted opaque by the preview (no flattener): the page is indeterminate and no
    // finding names transparency_flattening.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([1.0, 1.0, 1.0, 1.0]), false);
    s.note(SepNote::TransparencyFlattenedOpaque);
    let rep = eval(&s.finish(), &sites_total(250.0), InkLimitSeverity::Error);
    assert_eq!(rep.check_state, InkLimitCheckState::Indeterminate);
    assert!(rep.findings.iter().all(|f| f.introduced_by.is_none_or(|s| s.performed())));
    assert_eq!(rep.indeterminate_causes.iter().map(|c| c.code()).collect::<Vec<_>>(), ["transparency_not_flattened"]);
}

// ---- STU-LAY-253: the structured finding (MT-35431) ----

#[test]
fn finding_carries_thirteen_fields_and_serialises_as_data() {
    let inks = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    s.draw_object(
        &SepDraw { path: &rect(10.0, 10.0, 60.0, 60.0), mode: &FILL, paint: SepPaint::Process([0.5, 0.0, 0.0, 0.0]), overprint: false, overprint_nonzero: false },
        ObjectRef::Named("frame-7".into()),
    )
    .unwrap();
    s.draw_object(
        &SepDraw { path: &rect(40.0, 40.0, 90.0, 90.0), mode: &FILL, paint: SepPaint::Spot { name: SPOT.into(), tint: 1.0 }, overprint: true, overprint_nonzero: false },
        ObjectRef::Placed { link: "links/logo \"v2\".pdf".into() },
    )
    .unwrap();
    // Not composing at the worst location: elsewhere on the page.
    draw(&mut s, &rect(0.0, 90.0, 5.0, 100.0), SepPaint::Process([0.25, 0.0, 0.0, 0.0]), false);
    let p = s.finish();
    let rep = eval(&p, &sites_total(120.0), InkLimitSeverity::Error);
    assert_eq!(rep.findings.len(), 1);
    let f = &rep.findings[0];
    assert_eq!(f.rule_id, "ink-limit-1");
    assert_eq!((f.severity, f.check_state), (InkLimitSeverity::Error, InkLimitCheckState::OverLimit));
    assert_eq!((f.effective_limit, f.limit_source), (120.0, InkLimitSource::PreflightRuleInstance));
    // 2500 px of C 50 + 2500 px of the spot + 50 px of C 25 = 376250 / 10000 px; the 20 x 20 overlap is above.
    assert_eq!((f.measured_maximum, f.measured_mean, f.area_above_limit), (150.0, 37.625, 4.0));
    let contrib: Vec<(&str, f64)> = f.per_ink_contribution.iter().map(|r| (r.ink.as_str(), r.percent)).collect();
    assert_eq!(contrib, [("Process Cyan", 50.0), ("Process Magenta", 0.0), ("Process Yellow", 0.0), ("Process Black", 0.0), (SPOT, 100.0)]);
    assert_eq!(f.page_reference.page, PageReference { page: "p1".into(), spread: None });
    assert_eq!(f.page_reference.region, Some(DocRegion { x0: 40.0, y0: 40.0, x1: 60.0, y1: 60.0 }));
    assert_eq!(f.object_references, [ObjectRef::Named("frame-7".into()), ObjectRef::Placed { link: "links/logo \"v2\".pdf".into() }]);
    assert_eq!((f.introduced_by, f.sample_resolution), (Some(CoverageInputStage::OverprintComposition), 72.0));
    assert_eq!(
        f.to_json(),
        concat!(
            r#"{"rule_id":"ink-limit-1","limit":"total_ink_limit","severity":"severity.error","check_state":"check_state.over_limit","#,
            r#""effective_limit":120,"limit_source":"ink_limit_source.preflight_rule_instance","measured_maximum":150,"measured_mean":37.625,"#,
            r#""area_above_limit":4,"per_ink_contribution":[{"ink":"Process Cyan","percent":50},{"ink":"Process Magenta","percent":0},"#,
            r#"{"ink":"Process Yellow","percent":0},{"ink":"Process Black","percent":0},{"ink":"PANTONE 185 C","percent":100}],"#,
            r#""page_reference":{"page":"p1","spread":null,"region":{"x0":40,"y0":40,"x1":60,"y1":60}},"#,
            r#""object_references":[{"named":"frame-7"},{"placed_link":"links/logo \"v2\".pdf"}],"#,
            r#""introduced_by":"stage.overprint_composition","sample_resolution":72}"#
        )
    );
    let report = rep.to_json();
    assert!(report.starts_with(r#"{"rule_id":"ink-limit-1","scope":"ink_limit_scope.page","severity":"severity.error","check_state":"check_state.over_limit""#), "{report}");
    assert!(report.contains(&f.to_json()));
    // Escaping: control characters and quotes in caller-supplied ids stay valid JSON strings.
    let mut r2 = rule(InkLimitSeverity::Error);
    r2.rule_id = "a\"b\\c\nd\u{1}".into();
    let pages = [page(&p)];
    let sites = sites_total(120.0);
    let rep = evaluate_ink_coverage(&CoverageRequest { rule: r2, sites: &sites, sample_resolution: 72.0, pages: &pages }).unwrap();
    assert!(rep.findings[0].to_json().starts_with(r#"{"rule_id":"a\"b\\c\nd\u0001","#));
}

// ---- STU-LAY-254: no repair inside an evaluation (MT-35432 AC-003) ----

#[test]
fn evaluation_never_re_separates_the_document() {
    let inks = InkManager::default();
    let p = two_rects(&inks, true);
    let before = p.clone();
    let rep = eval(&p, &sites_total(120.0), InkLimitSeverity::Error);
    assert_eq!(rep.check_state, InkLimitCheckState::OverLimit);
    // The separation is read only: same planes, same readings after the over-limit evaluation.
    assert_eq!(p, before);
    assert_eq!(p.total_pt(50.0, 50.0), Some(150.0));
    // Re-evaluating gives the same verdict (nothing was brought under the limit).
    assert_eq!(eval(&p, &sites_total(120.0), InkLimitSeverity::Error), rep);
}

// ---- Scopes, page lists and hostile sizes (V4-01/V4-02) ----

#[test]
fn document_and_spread_scopes_aggregate_pages_and_others_are_refused() {
    let inks = InkManager::default();
    let hot = two_rects(&inks, true);
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 10.0, 10.0), SepPaint::Process([0.1, 0.0, 0.0, 0.0]), false);
    let cool = s.finish();
    let sites = sites_total(120.0);
    let pg = |name: &str, spread: Option<&str>, p: &'static SeparationPreview| CoveragePage {
        reference: PageReference { page: name.into(), spread: spread.map(Into::into) },
        preview: p,
        unmeasured: Vec::new(),
    };
    let hot: &'static SeparationPreview = Box::leak(Box::new(hot));
    let cool: &'static SeparationPreview = Box::leak(Box::new(cool));
    let pages = [pg("1", Some("s1"), cool), pg("2", Some("s1"), hot)];
    let mut r = rule(InkLimitSeverity::Error);
    r.scope = InkLimitScope::Document;
    let doc = evaluate_ink_coverage(&CoverageRequest { rule: r.clone(), sites: &sites, sample_resolution: 72.0, pages: &pages }).unwrap();
    assert_eq!(doc.check_state, InkLimitCheckState::OverLimit);
    let f = &doc.findings[0];
    assert_eq!(f.page_reference.page.page, "2");
    // Mean over both pages: (100 x 10) + 375000 = 376000 over 20000 px; 400 of 20000 px above.
    assert!(close(f.measured_mean, 18.8) && close(f.area_above_limit, 2.0), "{f:?}");
    r.scope = InkLimitScope::Spread;
    assert!(evaluate_ink_coverage(&CoverageRequest { rule: r.clone(), sites: &sites, sample_resolution: 72.0, pages: &pages }).is_ok());
    let mixed = [pg("1", Some("s1"), cool), pg("2", Some("s2"), hot)];
    assert_eq!(
        evaluate_ink_coverage(&CoverageRequest { rule: r.clone(), sites: &sites, sample_resolution: 72.0, pages: &mixed }).unwrap_err().code(),
        "ink_limit_scope_mismatch"
    );
    r.scope = InkLimitScope::Page;
    assert_eq!(
        evaluate_ink_coverage(&CoverageRequest { rule: r.clone(), sites: &sites, sample_resolution: 72.0, pages: &pages }).unwrap_err().code(),
        "ink_limit_scope_mismatch"
    );
    for scope in [InkLimitScope::Object, InkLimitScope::Plate] {
        r.scope = scope;
        assert_eq!(
            evaluate_ink_coverage(&CoverageRequest { rule: r.clone(), sites: &sites, sample_resolution: 72.0, pages: &pages[..1] }),
            Err(CoverageError::ScopeNotEvaluated(scope))
        );
    }
}

#[test]
fn coverage_hostile_sizes_are_typed_refusals() {
    let inks = InkManager::default();
    let p = two_rects(&inks, true);
    let sites = sites_total(120.0);
    let req = |rule: InkLimitRule, pages: &[CoveragePage<'_>]| {
        evaluate_ink_coverage(&CoverageRequest { rule, sites: &sites, sample_resolution: 72.0, pages }).map(|_| ()).map_err(|e| e.code())
    };
    // Page list over budget, before any pixel is read.
    let mut r = rule(InkLimitSeverity::Error);
    r.scope = InkLimitScope::Document;
    let many: Vec<CoveragePage<'_>> = (0..=MAX_COVERAGE_PAGES).map(|_| page(&p)).collect();
    assert_eq!(req(r.clone(), &many), Err("too_many_coverage_pages"));
    assert_eq!(req(r.clone(), &[]), Err("no_coverage_pages"));
    // Indeterminate cause list over budget.
    let mut pg = page(&p);
    pg.unmeasured = (0..=MAX_INDETERMINATE_CAUSES).map(|_| IndeterminateCause::Passthrough { object: None }).collect();
    assert_eq!(req(rule(InkLimitSeverity::Error), &[pg]), Err("too_many_indeterminate_causes"));
    // Rule id and page reference bounds.
    let mut bad = rule(InkLimitSeverity::Error);
    bad.rule_id = " ".into();
    assert_eq!(req(bad.clone(), &[page(&p)]), Err("invalid_rule_id"));
    bad.rule_id = "r".repeat(MAX_RULE_ID_BYTES + 1);
    assert_eq!(req(bad, &[page(&p)]), Err("invalid_rule_id"));
    let mut pg = page(&p);
    pg.reference.page = String::new();
    assert_eq!(req(rule(InkLimitSeverity::Error), &[pg]), Err("invalid_page_reference"));
    // Object references at the worst location are budgeted: 300 overprinting quarter-sample objects
    // all compose at pixel (0, 0).
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    let tiny = rect(0.0, 99.75, 0.25, 100.0);
    for _ in 0..300 {
        s.draw(&SepDraw { path: &tiny, mode: &FILL, paint: SepPaint::Process([0.5, 0.0, 0.0, 0.0]), overprint: true, overprint_nonzero: true })
            .unwrap();
    }
    let stack = s.finish();
    let low = sites_total(10.0);
    let pages = [page(&stack)];
    assert_eq!(
        evaluate_ink_coverage(&CoverageRequest { rule: rule(InkLimitSeverity::Error), sites: &low, sample_resolution: 72.0, pages: &pages })
            .unwrap_err()
            .code(),
        "too_many_object_references"
    );
    const { assert!(300 > MAX_COVERAGE_OBJECT_REFS) };
    // Object references are bounded strings.
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    let d = SepDraw { path: &tiny, mode: &FILL, paint: SepPaint::Process([0.5, 0.0, 0.0, 0.0]), overprint: false, overprint_nonzero: false };
    assert_eq!(s.draw_object(&d, ObjectRef::Named(String::new())), Err(SepError::InvalidInput));
    assert_eq!(s.draw_object(&d, ObjectRef::Placed { link: "x".repeat(MAX_OBJECT_REF_BYTES + 1) }), Err(SepError::InvalidInput));
    // The object record is charged to the byte budget with the planes: a refused draw leaves the planes unchanged.
    let tight = SepLimits { max_bytes: 4 * 40_000 + 100, ..SepLimits::default() };
    let mut s = Separator::new(BOX, 1.0, &inks, tight).unwrap();
    let big: Vec<VectorEl> = (0..64).flat_map(|k| rect(f64::from(k), 0.0, f64::from(k) + 0.5, 10.0)).collect();
    let d = SepDraw { path: &big, mode: &FILL, paint: SepPaint::Process([1.0, 0.0, 0.0, 0.0]), overprint: false, overprint_nonzero: false };
    assert_eq!(s.draw(&d), Err(SepError::Budget));
    assert_eq!(s.finish().total_pt(0.25, 5.0), Some(0.0));
}
