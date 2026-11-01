//! Lane SMB-ID-TOC-PERSIST-1 items 2 and 3: trap presets persisted in Folio
//! (`hsk.studio.trap_preset_records@1`) lift into layout's `TrapPreset` losslessly both ways, and a
//! stored preset is applied to a page range through the page's `applied_trap_preset` (STU-LAY-107,
//! STU-LAY-152) by an attributed command. Oracles are the spec table and the Studio rules in
//! `src/print.rs` (vendor parity UNVERIFIED, D5).
mod support_layout;
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, DomainId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};
use support_layout::{document, id};

fn ctx(revision: u64, command: bool) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(
        actor,
        ClientSessionId::parse("window-1").unwrap(),
        command.then(|| CommandId::parse("layout.page.apply_trap_preset").unwrap()),
        revision,
        CorrelationId::new(4),
        None,
    )
}

fn press() -> TrapPreset {
    let mut v = TrapPresetValues::default();
    for (p, x) in [
        (TrapParameter::DefaultTrapWidth, 0.25),
        (TrapParameter::BlackWidth, 0.5),
        (TrapParameter::BlackColorThreshold, 100.0),
        (TrapParameter::BlackDensity, 1.6),
        (TrapParameter::SlidingTrapThreshold, 70.0),
        (TrapParameter::StepThreshold, 10.0),
        (TrapParameter::ColorReduction, 100.0),
    ] {
        v.set(p, x).unwrap();
    }
    v.trap_end_type = Some(TrapEndType::Overlap);
    v.trap_image_placement = Some(TrapImagePlacement::ImagesOverSpread);
    TrapPreset::new("Press A", v).unwrap()
}

/// Every value, both enumerations and the name survive record -> Folio -> record; a stored preset
/// with unset values survives Folio -> record -> Folio; stored values outside the bound sets are
/// refused on the lift, never clamped.
#[test]
fn trap_preset_lift_is_lossless_both_ways() {
    let p = press();
    let stored = p.to_folio();
    assert_eq!((stored.name.as_str(), stored.black_density, stored.trap_end_type), ("Press A", Some(1.6), Some(StudioTrapEnd::Overlap)));
    assert_eq!(stored.trap_image_placement, Some(StudioTrapImagePlacement::ImagesOverSpread));
    assert_eq!(TrapPreset::from_folio(&stored).unwrap(), p);
    let sparse = StudioTrapPreset {
        name: "Sparse".into(),
        default_trap_width: None,
        black_width: Some(8.0),
        black_color_threshold: None,
        black_density: Some(12.0),
        sliding_trap_threshold: None,
        step_threshold: None,
        color_reduction: None,
        trap_end_type: Some(StudioTrapEnd::Miter),
        trap_image_placement: None,
    };
    let lifted = TrapPreset::from_folio(&sparse).unwrap();
    assert_eq!((lifted.values.default_trap_width, lifted.values.trap_image_placement), (None, None), "unset stays unset");
    assert_eq!(lifted.to_folio(), sparse);
    for m in TrapEndType::ALL {
        assert_eq!(TrapEndType::from(StudioTrapEnd::from(m)), m);
    }
    for m in TrapImagePlacement::ALL {
        assert_eq!(TrapImagePlacement::from(StudioTrapImagePlacement::from(m)), m);
    }
    let over = StudioTrapPreset { black_density: Some(12.001), ..sparse.clone() };
    assert_eq!(TrapPreset::from_folio(&over), Err(TrapPresetError::ParameterOutOfBounds(TrapParameter::BlackDensity)));
    let precise = StudioTrapPreset { black_density: Some(1.2345), ..sparse.clone() };
    assert_eq!(TrapPreset::from_folio(&precise), Err(TrapPresetError::ParameterPrecision(TrapParameter::BlackDensity)));
    let blank = StudioTrapPreset { name: " ".into(), ..sparse };
    assert_eq!(TrapPreset::from_folio(&blank), Err(TrapPresetError::NameEmpty));
}

fn records(presets: &[TrapPreset]) -> StudioTrapPresetRecords {
    let mut r = StudioTrapPresetRecords::new(id("SDOC", 1));
    r.trap_presets = presets.iter().map(TrapPreset::to_folio).collect();
    r
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

/// A page range assignment puts the same reference on each page of the range, at revision + 1, under
/// the caller's command context; the input document is unchanged; the result validates against the
/// stored presets; clearing removes it; every refusal is typed and reported through Observe.
#[test]
fn trap_preset_applied_to_page_range_with_attribution() {
    let mut doc = document(vec![], [PageSide::LeftHand, PageSide::RightHand]);
    doc.page_spreads[0].pages.push(support_layout::page("page-3", PageSide::LeftHand));
    let presets = records(&[press()]);
    let a = StudioTrapPresetRef("Press A".into());
    let cancel = CancellationToken::default();
    let range = TrapPageRange { first: "page-1", last: "page-2" };
    let out = assign_trap_preset(&ctx(2, true), &doc, &presets, range, Some(&a), &cancel).unwrap();
    assert_eq!(out.pages, ["page-1", "page-2"]);
    assert_eq!(out.document.revision, 3);
    let applied: Vec<Option<&str>> = out.document.page_spreads[0].pages.iter().map(|p| p.applied_trap_preset.as_ref().map(StudioTrapPresetRef::as_str)).collect();
    assert_eq!(applied, [Some("Press A"), Some("Press A"), None]);
    assert!(doc.page_spreads[0].pages.iter().all(|p| p.applied_trap_preset.is_none()), "input unchanged");
    assert_eq!(validate_trap_preset_records(&presets, &out.document, &cancel), Ok(()));
    let one = TrapPageRange { first: "page-2", last: "page-2" };
    let cleared = assign_trap_preset(&ctx(3, true), &out.document, &presets, one, None, &cancel).unwrap();
    let applied: Vec<bool> = cleared.document.page_spreads[0].pages.iter().map(|p| p.applied_trap_preset.is_some()).collect();
    assert_eq!((applied, cleared.document.revision), (vec![true, false, false], 4));
    // Typed refusals.
    let run = |c: &CommandContext, r: TrapPageRange<'_>, p: Option<&StudioTrapPresetRef>, recs: &StudioTrapPresetRecords| {
        assign_trap_preset(c, &doc, recs, r, p, &cancel).map(|_| ())
    };
    assert_eq!(run(&ctx(2, false), range, Some(&a), &presets), Err(TrapAssignError::CommandMissing));
    assert_eq!(run(&ctx(1, true), range, Some(&a), &presets), Err(TrapAssignError::StaleRevision { expected: 1, current: 2 }));
    let ghost = StudioTrapPresetRef("Ghost".into());
    assert_eq!(run(&ctx(2, true), range, Some(&ghost), &presets), Err(TrapAssignError::PresetMissing("Ghost".into())));
    assert_eq!(run(&ctx(2, true), TrapPageRange { first: "page-1", last: "page-9" }, Some(&a), &presets), Err(TrapAssignError::PageMissing("page-9".into())));
    assert_eq!(run(&ctx(2, true), TrapPageRange { first: "page-3", last: "page-1" }, Some(&a), &presets), Err(TrapAssignError::PageRangeOrder));
    let other = StudioTrapPresetRecords::new(id("SDOC", 2));
    assert_eq!(run(&ctx(2, true), range, None, &other), Err(TrapAssignError::DocumentMismatch));
    let mut top = doc.clone();
    top.revision = u64::MAX;
    let e = assign_trap_preset(&ctx(u64::MAX, true), &top, &presets, range, Some(&a), &cancel).unwrap_err();
    assert_eq!(e.code(), "revision_overflow");
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(assign_trap_preset(&ctx(2, true), &doc, &presets, range, Some(&a), &stop).unwrap_err(), TrapAssignError::Canceled);
    // One Observe terminal outcome per command, codes only.
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    let refused = run(&ctx(2, true), range, Some(&ghost), &presets);
    let mut observe = Observe::new(4, 2, resource.clone(), actor.clone(), hsk_studio_observe::Budget::new(0, 0).unwrap());
    let receipt = report_command(&refused, TrapAssignError::failure_code, &ctx(2, true), &cancel, &mut observe, &mut Collect(Vec::new())).unwrap();
    assert_eq!((receipt.outcome, receipt.correlation_id, receipt.revision), (Outcome::Failure(FailureCode::Validation), 4, 2));
    let ok = assign_trap_preset(&ctx(2, true), &doc, &presets, range, Some(&a), &cancel);
    let mut observe = Observe::new(4, 2, resource, actor, hsk_studio_observe::Budget::new(0, 0).unwrap());
    let receipt = report_command(&ok, TrapAssignError::failure_code, &ctx(2, true), &cancel, &mut observe, &mut Collect(Vec::new())).unwrap();
    assert_eq!(receipt.outcome, Outcome::Success);
}
