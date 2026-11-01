//! Lane SMB-PRESS-INK-MANAGER-1 item 4: the transparency flattener preset record of STU-LAY-161
//! (Master Spec v02.229, module 14) beside the print presets: the two tabled resolutions as seven-field
//! rows (MT-30849 AC-001/AC-002), the raster-vector balance with undeclared bounds (SPEC GAP), the
//! three switches, the five-member flattener level of STU-LAY-135, stored presets edited by attributed
//! commands, and `flattener_preset_name` / `applied_flattener_preset` resolved against them with a
//! typed refusal on a dangling name. Oracles are the spec table and enumerations; vendor parity is
//! UNVERIFIED (D5). The flattener preview and flattening are not in this lane.
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, DomainId};
use hsk_studio_layout::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};

fn ctx(revision: u64, command: bool) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(
        actor,
        ClientSessionId::parse("window-1").unwrap(),
        command.then(|| CommandId::parse("layout.flattener_preset.edit").unwrap()),
        revision,
        CorrelationId::new(4),
        None,
    )
}

/// One tabled row: hard bounds as stated, soft bounds, default and precision unknown (never mirrored
/// or invented), endpoints admitted, values just past them and non-finite values refused, storage
/// unchanged on refusal.
fn assert_dpi_row(p: FlattenerParameter, name: &str, hard_min: f64, hard_max: f64) {
    assert_eq!(p.name(), name);
    assert!(!p.bounds_undeclared());
    let b = p.bounds();
    assert_eq!((b.hard_min, b.hard_max), (Some(hard_min), Some(hard_max)), "{name} hard bounds");
    assert_eq!((b.soft_min, b.soft_max, b.default, b.precision), (None, None, None, None), "{name}: unknown is never mirrored or invented");
    assert_eq!(b.unit.map(PrintUnit::code), Some("dpi"));
    assert_eq!(p.check(hard_min), Ok(hard_min));
    assert_eq!(p.check(hard_max), Ok(hard_max));
    assert_eq!(p.check(hard_min - 0.5), Err(FlattenerPresetError::ParameterOutOfBounds(p)));
    assert_eq!(p.check(hard_max + 0.5), Err(FlattenerPresetError::ParameterOutOfBounds(p)));
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(p.check(bad), Err(FlattenerPresetError::ParameterNotFinite(p)));
    }
    let mut v = FlattenerPresetValues::default();
    assert_eq!(v.get(p), None, "no default supplied");
    v.set(p, hard_max).unwrap();
    assert_eq!(v.get(p), Some(hard_max));
    assert!(v.set(p, hard_max + 1.0).is_err());
    assert_eq!(v.get(p), Some(hard_max), "a refused value leaves the record unchanged");
    // A non-integer dpi is stored as given: precision is unknown, so nothing is rounded.
    v.set(p, hard_min + 0.25).unwrap();
    assert_eq!(v.get(p), Some(hard_min + 0.25));
}

#[test]
fn flattener_row_line_art_and_text_resolution() {
    assert_dpi_row(FlattenerParameter::LineArtAndTextResolution, "line_art_and_text_resolution", 1.0, 9600.0);
}

#[test]
fn flattener_row_gradient_and_mesh_resolution() {
    assert_dpi_row(FlattenerParameter::GradientAndMeshResolution, "gradient_and_mesh_resolution", 0.0, 1200.0);
}

#[test]
fn flattener_raster_vector_balance_bounds_undeclared_spec_gap() {
    let p = FlattenerParameter::RasterVectorBalance;
    assert_eq!(p.name(), "raster_vector_balance");
    assert!(p.bounds_undeclared());
    let b = p.bounds();
    assert_eq!(
        (b.hard_min, b.hard_max, b.soft_min, b.soft_max, b.default, b.unit, b.precision),
        (None, None, None, None, None, None, None),
        "SPEC GAP: every field unknown, no bound invented"
    );
    // Any finite value is stored as given (no invented 0..100 range, no clamp); non-finite is refused.
    for ok in [-5.0, 0.0, 75.0, 100.0, 1.0e6] {
        assert_eq!(p.check(ok), Ok(ok));
    }
    assert_eq!(p.check(f64::NAN), Err(FlattenerPresetError::ParameterNotFinite(p)));
    let mut v = FlattenerPresetValues::default();
    assert_eq!(v.raster_vector_balance, None);
    v.set(p, 1.0e6).unwrap();
    assert_eq!(v.raster_vector_balance, Some(1.0e6));
}

#[test]
fn flattener_level_five_members_and_switches_round_trip() {
    let codes: Vec<&str> = FlattenerLevel::ALL.iter().map(|l| l.code()).collect();
    assert_eq!(codes, ["flattener.low", "flattener.medium_low", "flattener.medium", "flattener.medium_high", "flattener.high"]);
    for l in FlattenerLevel::ALL {
        assert_eq!(FlattenerLevel::from_code(l.code()), Ok(l));
    }
    for near in ["flattener.Low", "flattener.medium-low", " flattener.high", "high", ""] {
        assert_eq!(FlattenerLevel::from_code(near), Err(FlattenerPresetError::UnknownMember("flattener_level")), "{near:?}");
    }
    let values = FlattenerPresetValues {
        line_art_and_text_resolution: Some(1200.0),
        gradient_and_mesh_resolution: Some(300.0),
        raster_vector_balance: Some(75.0),
        convert_all_text_to_outlines: Some(true),
        convert_all_strokes_to_outlines: Some(false),
        clip_complex_regions: Some(true),
        level: Some(FlattenerLevel::MediumHigh),
    };
    let preset = FlattenerPreset::new("Press Quality", values.clone()).unwrap();
    assert_eq!(preset.values, values, "every field round-trips separately");
    assert_eq!(FlattenerPreset::new("Unset", FlattenerPresetValues::default()).unwrap().values, FlattenerPresetValues::default());
}

#[test]
fn flattener_presets_store_attributed_edits_and_typed_refusals() {
    let cancel = CancellationToken::default();
    let empty = FlattenerPresets::default();
    assert_eq!((empty.revision(), empty.presets().len()), (0, 0));
    let high = FlattenerPreset::new("High", FlattenerPresetValues { line_art_and_text_resolution: Some(1200.0), ..Default::default() }).unwrap();
    let one = empty.apply(&ctx(0, true), &FlattenerPresetEdit::Put(high.clone()), &cancel).unwrap();
    assert_eq!((one.revision(), one.get("High")), (1, Some(&high)));
    assert_eq!(empty.presets().len(), 0, "the input store is unchanged");
    // Put replaces by exact name; names are case-sensitive and distinct.
    let higher = FlattenerPreset::new("High", FlattenerPresetValues { line_art_and_text_resolution: Some(2400.0), ..Default::default() }).unwrap();
    let two = one.apply(&ctx(1, true), &FlattenerPresetEdit::Put(higher.clone()), &cancel).unwrap();
    assert_eq!((two.revision(), two.presets().len(), two.get("High")), (2, 1, Some(&higher)));
    assert_eq!(two.get("high"), None);
    // Attribution and revision rules.
    assert_eq!(two.apply(&ctx(2, false), &FlattenerPresetEdit::Remove("High".into()), &cancel), Err(FlattenerPresetError::CommandMissing));
    assert_eq!(
        two.apply(&ctx(1, true), &FlattenerPresetEdit::Remove("High".into()), &cancel),
        Err(FlattenerPresetError::StaleRevision { expected: 1, current: 2 })
    );
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(two.apply(&ctx(2, true), &FlattenerPresetEdit::Remove("High".into()), &stop), Err(FlattenerPresetError::Canceled));
    assert_eq!(two.apply(&ctx(2, true), &FlattenerPresetEdit::Remove("Ghost".into()), &cancel).unwrap_err().code(), "flattener_preset_missing");
    let removed = two.apply(&ctx(2, true), &FlattenerPresetEdit::Remove("High".into()), &cancel).unwrap();
    assert_eq!((removed.revision(), removed.get("High")), (3, None));
    // A record built by field writes cannot bypass the bounds or the name rule.
    let mut forged = high.clone();
    forged.values.gradient_and_mesh_resolution = Some(1200.5);
    assert_eq!(
        one.apply(&ctx(1, true), &FlattenerPresetEdit::Put(forged), &cancel),
        Err(FlattenerPresetError::ParameterOutOfBounds(FlattenerParameter::GradientAndMeshResolution))
    );
    let mut bad_name = high.clone();
    bad_name.name = "tab\there".into();
    assert_eq!(one.apply(&ctx(1, true), &FlattenerPresetEdit::Put(bad_name), &cancel).unwrap_err().code(), "flattener_preset_name_invalid");
    // Name rules on construction (hostile lengths checked before copying).
    assert_eq!(FlattenerPreset::new("   ", FlattenerPresetValues::default()), Err(FlattenerPresetError::NameEmpty));
    assert_eq!(FlattenerPreset::new(&"n".repeat(MAX_FLATTENER_PRESET_NAME_BYTES + 1), FlattenerPresetValues::default()), Err(FlattenerPresetError::NameTooLong));
    assert!(FlattenerPreset::new(&"n".repeat(MAX_FLATTENER_PRESET_NAME_BYTES), FlattenerPresetValues::default()).is_ok());
}

#[test]
fn flattener_presets_hostile_count_is_bounded() {
    let cancel = CancellationToken::default();
    let mut store = FlattenerPresets::default();
    for i in 0..MAX_FLATTENER_PRESETS {
        let p = FlattenerPreset::new(&format!("P{i}"), FlattenerPresetValues::default()).unwrap();
        store = store.apply(&ctx(store.revision(), true), &FlattenerPresetEdit::Put(p), &cancel).unwrap();
    }
    assert_eq!(store.presets().len(), MAX_FLATTENER_PRESETS);
    let extra = FlattenerPreset::new("one more", FlattenerPresetValues::default()).unwrap();
    assert_eq!(store.apply(&ctx(store.revision(), true), &FlattenerPresetEdit::Put(extra), &cancel), Err(FlattenerPresetError::TooManyPresets));
    // Replacing a stored preset at the cap is still allowed (no growth).
    let again = FlattenerPreset::new("P0", FlattenerPresetValues { clip_complex_regions: Some(true), ..Default::default() }).unwrap();
    assert!(store.apply(&ctx(store.revision(), true), &FlattenerPresetEdit::Put(again), &cancel).is_ok());
}

#[test]
fn flattener_preset_names_resolve_against_stored_presets_or_refuse() {
    let cancel = CancellationToken::default();
    let medium = FlattenerPreset::new("[Medium Resolution]", FlattenerPresetValues { level: Some(FlattenerLevel::Medium), ..Default::default() }).unwrap();
    let store = FlattenerPresets::default().apply(&ctx(0, true), &FlattenerPresetEdit::Put(medium.clone()), &cancel).unwrap();
    // Print preferences / printer presets: flattener_preset_name.
    let mut prefs = PrintPreferences::default();
    assert_eq!(prefs.advanced.resolve_flattener_preset(&store), Ok(None));
    prefs.advanced.flattener_preset_name = Some(PrintValue::Text("[Medium Resolution]".into()));
    let printer = PrinterPreset::new("Proof", PresetType::Custom, prefs.clone()).unwrap();
    assert_eq!(printer.preferences.advanced.resolve_flattener_preset(&store), Ok(Some(&medium)));
    prefs.advanced.flattener_preset_name = Some(PrintValue::Text("[Low Resolution]".into()));
    assert_eq!(prefs.advanced.resolve_flattener_preset(&store), Err(FlattenerPresetError::PresetMissing("[Low Resolution]".into())));
    prefs.advanced.flattener_preset_name = Some(PrintValue::Bool(true));
    assert_eq!(prefs.advanced.resolve_flattener_preset(&store).unwrap_err().code(), "flattener_preset_name_not_text");
    // PDF export preferences: applied_flattener_preset.
    let mut panel = AdvancedPanel::default();
    assert_eq!(panel.resolve_flattener_preset(&store), Ok(None));
    panel.applied_flattener_preset = Some(PdfName::new("[Medium Resolution]").unwrap());
    assert_eq!(panel.resolve_flattener_preset(&store), Ok(Some(&medium)));
    panel.applied_flattener_preset = Some(PdfName::new("[medium resolution]").unwrap());
    assert_eq!(panel.resolve_flattener_preset(&store).unwrap_err().code(), "flattener_preset_missing");
    // Removing the stored preset makes a reference dangle: refused, never silently ignored.
    let gone = store.apply(&ctx(1, true), &FlattenerPresetEdit::Remove("[Medium Resolution]".into()), &cancel).unwrap();
    panel.applied_flattener_preset = Some(PdfName::new("[Medium Resolution]").unwrap());
    assert_eq!(panel.resolve_flattener_preset(&gone), Err(FlattenerPresetError::PresetMissing("[Medium Resolution]".into())));
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn flattener_preset_edits_report_through_observe_codes_only() {
    let cancel = CancellationToken::default();
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    let store = FlattenerPresets::default();
    let refused = store.apply(&ctx(0, true), &FlattenerPresetEdit::Remove("Ghost".into()), &cancel);
    let mut observe = Observe::new(4, 0, resource.clone(), actor.clone(), Budget::new(0, 0).unwrap());
    let mut sink = Collect(Vec::new());
    let receipt = report_command(&refused, FlattenerPresetError::failure_code, &ctx(0, true), &cancel, &mut observe, &mut sink).unwrap();
    assert_eq!((receipt.outcome, receipt.correlation_id, receipt.revision), (Outcome::Failure(FailureCode::Validation), 4, 0));
    assert!(sink.0.iter().all(|b| !String::from_utf8_lossy(b).contains("Ghost")), "no preset name reaches the diagnostic port");
    let p = FlattenerPreset::new("Ok", FlattenerPresetValues::default()).unwrap();
    let ok = store.apply(&ctx(0, true), &FlattenerPresetEdit::Put(p), &cancel);
    let mut observe = Observe::new(4, 0, resource, actor, Budget::new(0, 0).unwrap());
    let receipt = report_command(&ok, FlattenerPresetError::failure_code, &ctx(0, true), &cancel, &mut observe, &mut Collect(Vec::new())).unwrap();
    assert_eq!(receipt.outcome, Outcome::Success);
    assert_eq!(FlattenerPresetError::Canceled.failure_code(), None);
}
