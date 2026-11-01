//! Lane SMB-PRESS-OBSERVE-1 (R51 F-R51-7, HOST INTEGRATION rule 5): every terminal outcome of the
//! total ink coverage evaluation (STU-LAY-250..254) and of the ink manager edits (STU-LAY-160) is
//! emitted through observe as codes, counters and opaque ids only. Oracles: the observe wire (the
//! `emit_detail` version-2 layout in observe's DESCRIPTOR) decoded byte by byte, and the typed results
//! the same calls return without a sink. Pages are pixel-aligned rectangles at 1 px/pt on a
//! 100 x 100 pt page, so coverage values are hand-computed. Vendor parity is UNVERIFIED (D5).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Error as ObserveError, Observe, Outcome, SinkPort};
use hsk_studio_press_read::{VectorEl, VectorMode};
use hsk_studio_press_write::*;

const BOX: [f64; 4] = [0.0, 0.0, 100.0, 100.0];
const FILL: VectorMode = VectorMode::Fill { even_odd: false };
const CORRELATION: u64 = 7;
const REVISION: u64 = 3;
// Wire enumerations of observe's DESCRIPTOR (detail_wire).
const OUTCOME_SUCCESS: u8 = 2;
const OUTCOME_FAILURE: u8 = 3;
const OUTCOME_CANCELED: u8 = 4;
const FAIL_VALIDATION: u8 = 1;
const FAIL_UNAVAILABLE: u8 = 2;
const FAIL_UNSUPPORTED: u8 = 4;
const DISP_CHANGED: u8 = 1;
const DISP_NO_CHANGE: u8 = 2;
const DISP_REJECTED: u8 = 3;
const DC_NONE: u8 = 0;
const DC_INVALID_INPUT: u8 = 1;
const DC_INVALID_TARGET: u8 = 7;
const DC_DUPLICATE_ADDRESS: u8 = 9;
const DC_MISSING_READ: u8 = 10;
const DC_BUDGET_EXCEEDED: u8 = 15;
const DC_VALIDATION_REJECTED: u8 = 17;

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<VectorEl> {
    vec![VectorEl::MoveTo([x0, y0]), VectorEl::LineTo([x1, y0]), VectorEl::LineTo([x1, y1]), VectorEl::LineTo([x0, y1]), VectorEl::Close]
}

fn draw(s: &mut Separator<'_>, r: &[VectorEl], paint: SepPaint, overprint: bool) {
    s.draw(&SepDraw { path: r, mode: &FILL, paint, overprint, overprint_nonzero: false }).unwrap();
}

/// C 20 % over (0..50)^2: 20 % total, 20 % single, 0 % black.
fn light_page() -> SeparationPreview {
    let inks = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 50.0, 50.0), SepPaint::Process([0.2, 0.0, 0.0, 0.0]), false);
    s.finish()
}

/// C 50 % over (10..60)^2 then the spot at 100 % over (40..90)^2, overprinting: 150 % total where they overlap.
fn overlap_page() -> SeparationPreview {
    let inks = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(10.0, 10.0, 60.0, 60.0), SepPaint::Process([0.5, 0.0, 0.0, 0.0]), false);
    draw(&mut s, &rect(40.0, 40.0, 90.0, 90.0), SepPaint::Spot { name: "PANTONE 185 C".into(), tint: 1.0 }, true);
    s.finish()
}

/// K 60 % over (0..50)^2: 60 % total, 60 % single, 60 % black.
fn black_page() -> SeparationPreview {
    let inks = InkManager::default();
    let mut s = Separator::new(BOX, 1.0, &inks, SepLimits::default()).unwrap();
    draw(&mut s, &rect(0.0, 0.0, 50.0, 50.0), SepPaint::Process([0.0, 0.0, 0.0, 0.6]), false);
    s.finish()
}

fn rule(scope: InkLimitScope) -> InkLimitRule {
    InkLimitRule { rule_id: "ink-limit-1".into(), severity: InkLimitSeverity::Error, scope }
}

fn page<'a>(p: &'a SeparationPreview) -> CoveragePage<'a> {
    CoveragePage { reference: PageReference { page: "p1".into(), spread: None }, preview: p, unmeasured: Vec::new() }
}

fn sites(limits: InkLimits) -> InkLimitSites {
    let mut s = InkLimitSites::default();
    s.declare(InkLimitSource::PreflightRuleInstance, limits).unwrap();
    s
}

struct Collect {
    frames: Vec<(DeliveryClass, Vec<u8>)>,
    fail: Option<DeliveryError>,
}
impl Collect {
    fn new() -> Self {
        Self { frames: Vec::new(), fail: None }
    }
}
impl SinkPort for Collect {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if let Some(e) = self.fail {
            return Err(e);
        }
        self.frames.push((class, bytes.to_vec()));
        Ok(())
    }
}

fn emitter() -> Observe {
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    Observe::new(CORRELATION, REVISION, resource, actor(), Budget::new(0, 0).unwrap())
}

/// The fields of one emit_detail frame (version 2).
#[derive(Debug, PartialEq)]
struct Wire {
    version: u8,
    outcome: u8,
    failure: u8,
    disposition: u8,
    code: u8,
    counts: [u32; 5],
}

fn decode(bytes: &[u8]) -> Wire {
    assert_eq!(&bytes[..4], b"HSKO");
    let (version, outcome, failure) = (bytes[4], bytes[5], bytes[6]);
    let mut p = 8 + 5 * 8;
    for _ in 0..7 {
        let n = usize::from(u16::from_be_bytes([bytes[p], bytes[p + 1]]));
        p += 2 + n;
    }
    let (disposition, code, address) = (bytes[p], bytes[p + 1], bytes[p + 2]);
    assert_eq!(address, 0, "no address on a press-write terminal");
    p += 3;
    let mut counts = [0u32; 5];
    for c in &mut counts {
        *c = u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap());
        p += 4;
    }
    assert_eq!(p, bytes.len(), "no trailing bytes");
    Wire { version, outcome, failure, disposition, code, counts }
}

/// Exactly one terminal frame, decoded.
fn one_terminal(sink: &Collect) -> Wire {
    assert_eq!(sink.frames.len(), 1, "exactly one terminal event");
    assert_eq!(sink.frames[0].0, DeliveryClass::Terminal);
    decode(&sink.frames[0].1)
}

fn evaluate(rule: InkLimitRule, sites: &InkLimitSites, resolution: f64, pages: &[CoveragePage<'_>]) -> (Result<InkCoverageReport, CoverageError>, Emission, Collect) {
    let req = CoverageRequest { rule, sites, sample_resolution: resolution, pages };
    let cancel = CancellationToken::default();
    let (mut observe, mut sink) = (emitter(), Collect::new());
    let (result, emission) =
        evaluate_ink_coverage_observed(&req, ObserveRoute { correlation_id: CORRELATION, revision: REVISION, cancel: &cancel, observe: &mut observe, sink: &mut sink });
    (result, emission, sink)
}

fn success(disposition: u8, counts: [u32; 5]) -> Wire {
    Wire { version: 2, outcome: OUTCOME_SUCCESS, failure: 0, disposition, code: DC_NONE, counts }
}

fn failure(failure: u8, code: u8, counts: [u32; 5]) -> Wire {
    Wire { version: 2, outcome: OUTCOME_FAILURE, failure, disposition: DISP_REJECTED, code, counts }
}

// ---- coverage: the five check states ----

#[test]
fn each_check_state_emits_exactly_one_terminal_with_its_code_and_counters() {
    let unmeasured = {
        let p = light_page();
        (p, true)
    };
    // (name, preview, limits, margin/limits, indeterminate, expected state code, expected wire)
    let total = |limit: f64| InkLimits { total_ink_limit: Some(limit), ..InkLimits::default() };
    let light = light_page();
    let overlap = overlap_page();
    let cases: Vec<(&str, &SeparationPreview, InkLimitSites, bool, InkLimitCheckState, Wire)> = vec![
        ("within_limit", &light, sites(total(300.0)), false, InkLimitCheckState::WithinLimit, success(DISP_NO_CHANGE, [3, 0, 1, 0, 0])),
        (
            "within_warning_margin",
            &overlap,
            sites(InkLimits { total_ink_limit: Some(160.0), coverage_warning_margin: Some(20.0), ..InkLimits::default() }),
            false,
            InkLimitCheckState::WithinWarningMargin,
            success(DISP_NO_CHANGE, [3, 0, 1, 0, 0]),
        ),
        ("over_limit", &overlap, sites(total(100.0)), false, InkLimitCheckState::OverLimit, failure(FAIL_VALIDATION, DC_VALIDATION_REJECTED, [3, 0, 0, 0, 0])),
        ("not_checked", &light, InkLimitSites::default(), false, InkLimitCheckState::NotChecked, failure(FAIL_UNAVAILABLE, DC_MISSING_READ, [3, 0, 0, 0, 0])),
        ("indeterminate", &unmeasured.0, sites(total(300.0)), unmeasured.1, InkLimitCheckState::Indeterminate, failure(FAIL_UNSUPPORTED, DC_MISSING_READ, [3, 0, 0, 0, 0])),
    ];
    for (name, preview, sites, indeterminate, state, wire) in cases {
        let mut pg = page(preview);
        if indeterminate {
            pg.unmeasured.push(IndeterminateCause::MissingLink { object: Some(ObjectRef::Placed { link: "links/photo.tif".into() }) });
        }
        let pages = [pg];
        let (result, emission, sink) = evaluate(rule(InkLimitScope::Page), &sites, 72.0, &pages);
        let report = result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(report.check_state, state, "{name}");
        assert_eq!(emission.code, state.code(), "{name}: typed terminal code");
        assert_eq!(one_terminal(&sink), wire, "{name}: wire");
        let receipt = emission.receipt.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!((receipt.correlation_id, receipt.revision, receipt.sequence), (CORRELATION, REVISION, 1), "{name}");
    }
}

#[test]
fn the_terminal_state_is_the_worst_of_the_three_limits() {
    // Total is within its limit; the black limit is exceeded (60 % > 40 %): the evaluation is over the limit.
    let p = black_page();
    let s = sites(InkLimits { total_ink_limit: Some(300.0), black_ink_limit: Some(40.0), ..InkLimits::default() });
    let pages = [page(&p)];
    let (result, emission, sink) = evaluate(rule(InkLimitScope::Page), &s, 72.0, &pages);
    let report = result.unwrap();
    assert_eq!(report.check_state, InkLimitCheckState::WithinLimit, "the total limit alone is within its limit");
    assert_eq!(emission.code, InkLimitCheckState::OverLimit.code());
    assert_eq!(one_terminal(&sink), failure(FAIL_VALIDATION, DC_VALIDATION_REJECTED, [3, 0, 0, 0, 0]));
}

// ---- coverage: refusals ----

#[test]
fn each_refusal_emits_one_terminal_with_its_typed_code() {
    let p = light_page();
    let s = sites(InkLimits { total_ink_limit: Some(300.0), ..InkLimits::default() });
    let one = [page(&p)];
    let many: Vec<CoveragePage<'_>> = (0..MAX_COVERAGE_PAGES + 1).map(|_| page(&p)).collect();
    let cases: Vec<(&str, InkLimitRule, f64, &[CoveragePage<'_>], Wire)> = vec![
        ("no_coverage_pages", rule(InkLimitScope::Page), 72.0, &[], failure(FAIL_VALIDATION, DC_INVALID_INPUT, [0; 5])),
        ("too_many_coverage_pages", rule(InkLimitScope::Page), 72.0, &many, failure(FAIL_VALIDATION, DC_BUDGET_EXCEEDED, [0; 5])),
        ("ink_limit_scope_not_evaluated", rule(InkLimitScope::Object), 72.0, &one, failure(FAIL_UNSUPPORTED, DC_INVALID_INPUT, [0; 5])),
        ("sample_resolution_mismatch", rule(InkLimitScope::Page), 150.0, &one, failure(FAIL_VALIDATION, DC_INVALID_INPUT, [0; 5])),
        ("invalid_sample_resolution", rule(InkLimitScope::Page), f64::NAN, &one, failure(FAIL_VALIDATION, DC_INVALID_INPUT, [0; 5])),
    ];
    for (code, r, resolution, pages, wire) in cases {
        let (result, emission, sink) = evaluate(r, &s, resolution, pages);
        assert_eq!(result.unwrap_err().code(), code);
        assert_eq!(emission.code, code);
        assert_eq!(one_terminal(&sink), wire, "{code}");
        assert!(emission.receipt.is_ok(), "{code}");
    }
}

// ---- coverage: emission never changes a result ----

#[test]
fn results_are_unchanged_with_and_without_a_sink_and_under_every_delivery_failure() {
    let p = overlap_page();
    let s = sites(InkLimits { total_ink_limit: Some(100.0), ..InkLimits::default() });
    let pages = [page(&p)];
    let req = CoverageRequest { rule: rule(InkLimitScope::Page), sites: &s, sample_resolution: 72.0, pages: &pages };
    let plain = evaluate_ink_coverage(&req);
    // Delivered.
    let (delivered, emission, _) = evaluate(rule(InkLimitScope::Page), &s, 72.0, &pages);
    assert_eq!(delivered, plain);
    assert!(emission.receipt.is_ok());
    // Every delivery failure: the result is the same, the failure is reported on the emission only.
    for error in [DeliveryError::Saturated, DeliveryError::Rejected, DeliveryError::Unavailable, DeliveryError::Indeterminate] {
        let cancel = CancellationToken::default();
        let (mut observe, mut sink) = (emitter(), Collect { frames: Vec::new(), fail: Some(error) });
        let (result, emission) = evaluate_ink_coverage_observed(
            &req,
            ObserveRoute { correlation_id: CORRELATION, revision: REVISION, cancel: &cancel, observe: &mut observe, sink: &mut sink },
        );
        assert_eq!(result, plain, "{error:?}");
        assert_eq!(emission.receipt, Err(ObserveError::Delivery(error)), "{error:?}");
        assert!(sink.frames.is_empty());
    }
    // A stale revision is refused by observe, not by the evaluation.
    let cancel = CancellationToken::default();
    let (mut observe, mut sink) = (emitter(), Collect::new());
    let (result, emission) = evaluate_ink_coverage_observed(
        &req,
        ObserveRoute { correlation_id: CORRELATION, revision: REVISION + 1, cancel: &cancel, observe: &mut observe, sink: &mut sink },
    );
    assert_eq!(result, plain);
    assert_eq!(emission.receipt, Err(ObserveError::RevisionMismatch));
    // A canceled token turns the terminal into the reserved canceled outcome; the report is still returned.
    let cancel = CancellationToken::default();
    cancel.cancel();
    let (mut observe, mut sink) = (emitter(), Collect::new());
    let (result, emission) = evaluate_ink_coverage_observed(
        &req,
        ObserveRoute { correlation_id: CORRELATION, revision: REVISION, cancel: &cancel, observe: &mut observe, sink: &mut sink },
    );
    assert_eq!(result, plain);
    assert_eq!(emission.receipt.unwrap().outcome, Outcome::Canceled);
    assert_eq!(decode(&sink.frames[0].1).outcome, OUTCOME_CANCELED);
    // One emitter carries one terminal: a second evaluation on the same emitter reports it closed.
    let cancel = CancellationToken::default();
    let (mut observe, mut sink) = (emitter(), Collect::new());
    for expect_ok in [true, false] {
        let (result, emission) = evaluate_ink_coverage_observed(
            &req,
            ObserveRoute { correlation_id: CORRELATION, revision: REVISION, cancel: &cancel, observe: &mut observe, sink: &mut sink },
        );
        assert_eq!(result, plain);
        assert_eq!(emission.receipt.is_ok(), expect_ok);
    }
    assert_eq!(sink.frames.len(), 1);
}

// ---- no text ----

#[test]
fn no_rule_page_ink_or_link_text_reaches_the_diagnostic_port() {
    let p = overlap_page();
    let s = sites(InkLimits { total_ink_limit: Some(100.0), ..InkLimits::default() });
    let mut pg = page(&p);
    pg.unmeasured.push(IndeterminateCause::MissingLink { object: Some(ObjectRef::Placed { link: "links/photo.tif".into() }) });
    let pages = [pg];
    let (_, _, coverage_sink) = evaluate(rule(InkLimitScope::Page), &s, 72.0, &pages);
    let cancel = CancellationToken::default();
    let (mut observe, mut ink_sink) = (emitter(), Collect::new());
    let (refused, _) = InkManager::default().apply_edit_observed(
        &actor(),
        &InkEdit::SetInkType { ink: "PANTONE 185 C".into(), ink_type: InkType::Opaque },
        ObserveRoute { correlation_id: CORRELATION, revision: REVISION, cancel: &cancel, observe: &mut observe, sink: &mut ink_sink },
    );
    assert_eq!(refused.unwrap_err().code(), "unknown_ink");
    for sink in [&coverage_sink, &ink_sink] {
        assert_eq!(sink.frames.len(), 1);
        let text = String::from_utf8_lossy(&sink.frames[0].1).into_owned();
        for private in ["ink-limit-1", "p1", "links/photo.tif", "photo", "PANTONE", "185"] {
            // "p1" would also match the opaque ids by accident; the frame ids contain none of these.
            assert!(!text.contains(private), "{private} reached the diagnostic port");
        }
    }
}

// ---- ink manager edits ----

fn edit(manager: &InkManager, e: &InkEdit) -> (Result<InkEditOutcome, InkError>, Emission, Collect) {
    let cancel = CancellationToken::default();
    let (mut observe, mut sink) = (emitter(), Collect::new());
    let (result, emission) =
        manager.apply_edit_observed(&actor(), e, ObserveRoute { correlation_id: CORRELATION, revision: REVISION, cancel: &cancel, observe: &mut observe, sink: &mut sink });
    (result, emission, sink)
}

#[test]
fn an_applied_ink_edit_emits_one_changed_or_no_change_terminal() {
    let m = InkManager::default();
    let added = InkEdit::AddSpot { name: "PANTONE 185 C".into(), process_alternate: None };
    let (result, emission, sink) = edit(&m, &added);
    assert_eq!(result.unwrap().manager, m.apply_edit(&actor(), &added).unwrap().manager);
    assert_eq!(emission.code, "ink_edit.applied");
    assert_eq!(one_terminal(&sink), success(DISP_CHANGED, [0, 1, 0, 1, 1]));
    // An edit that leaves the manager equal is applied but changes nothing.
    let (result, emission, sink) = edit(&m, &InkEdit::SetAllSpotsToProcess(false));
    assert_eq!(result.unwrap().manager, m);
    assert_eq!(emission.code, "ink_edit.no_change");
    assert_eq!(one_terminal(&sink), success(DISP_NO_CHANGE, [0, 1, 0, 1, 0]));
}

#[test]
fn each_refused_ink_edit_emits_one_terminal_with_its_typed_code() {
    let m = InkManager::default();
    let full = {
        let mut f = InkManager::default();
        while f.inks.len() < MAX_INKS {
            f.inks.push(Ink::spot(&format!("Spot {}", f.inks.len()), None));
        }
        f
    };
    let cases: Vec<(&str, &InkManager, InkEdit, u8)> = vec![
        ("unknown_ink", &m, InkEdit::SetInkType { ink: "Nope".into(), ink_type: InkType::Opaque }, DC_INVALID_TARGET),
        ("duplicate_ink", &m, InkEdit::AddSpot { name: "Process Cyan".into(), process_alternate: None }, DC_DUPLICATE_ADDRESS),
        ("invalid_ink_name", &m, InkEdit::AddSpot { name: "  ".into(), process_alternate: None }, DC_INVALID_INPUT),
        (
            "ink_angle_out_of_range",
            &m,
            InkEdit::SetParameter { ink: "Process Cyan".into(), parameter: InkParameter::InkAngle, value: Some(361.0) },
            DC_VALIDATION_REJECTED,
        ),
        ("process_ink_cannot_be_aliased", &m, InkEdit::SetAlias { ink: "Process Cyan".into(), alias: Some("Process Black".into()) }, DC_VALIDATION_REJECTED),
        ("too_many_inks", &full, InkEdit::AddSpot { name: "One more".into(), process_alternate: None }, DC_BUDGET_EXCEEDED),
    ];
    for (code, manager, e, dc) in cases {
        let before = manager.clone();
        let (result, emission, sink) = edit(manager, &e);
        assert_eq!(result.unwrap_err().code(), code);
        assert_eq!(*manager, before, "{code}: a refused edit leaves the manager unchanged");
        assert_eq!(emission.code, code);
        assert_eq!(one_terminal(&sink), failure(FAIL_VALIDATION, dc, [0, 1, 0, 0, 0]), "{code}");
    }
}

#[test]
fn ink_edit_results_are_unchanged_by_the_sink() {
    let m = InkManager::default();
    let e = InkEdit::AddSpot { name: "PANTONE 185 C".into(), process_alternate: Some([0.0, 1.0, 0.8, 0.0]) };
    let plain = m.apply_edit(&actor(), &e);
    let (with_sink, _, _) = edit(&m, &e);
    assert_eq!(with_sink, plain);
    let cancel = CancellationToken::default();
    let (mut observe, mut sink) = (emitter(), Collect { frames: Vec::new(), fail: Some(DeliveryError::Unavailable) });
    let (failed_sink, emission) =
        m.apply_edit_observed(&actor(), &e, ObserveRoute { correlation_id: CORRELATION, revision: REVISION, cancel: &cancel, observe: &mut observe, sink: &mut sink });
    assert_eq!(failed_sink, plain);
    assert_eq!(emission.receipt, Err(ObserveError::Delivery(DeliveryError::Unavailable)));
    // The receipt keeps the caller's actor (HOST rule 4): the diagnostic port does not replace it.
    assert_eq!(plain.unwrap().receipt.actor, actor());
}
