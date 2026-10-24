//! Lane SMB-LR-LIBRARY-WRITE slice 3: develop records (virtual copies, history, snapshots),
//! copy / paste / sync of settings per group, and batch apply over a selection. Lightroom Classic
//! behaviour is UNVERIFIED (D5): expectations are the documented semantics, computed by hand.
use hsk_studio_accord::DomainId;
use hsk_studio_develop::*;
use std::collections::{BTreeMap, BTreeSet};

fn profile() -> ProfileBinding {
    ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] }
}

fn recipe() -> DevelopRecipe {
    DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile())
}

/// A source edit touching many groups, with values that are not representable exactly in
/// decimal (copies must be bit-exact, not re-derived).
fn edited() -> DevelopRecipe {
    let mut r = recipe();
    r.exposure_stops = 0.1 + 0.2;
    r.adjust.tone.contrast = 33.3;
    r.adjust.presence.clarity = 12.0;
    r.adjust.detail.texture = 7.0;
    r.adjust.detail.sharpen.amount = 90.0;
    r.adjust.mixer.bands[2].hue = -20.0;
    r.adjust.grading.highlights.hue = 45.0;
    r.adjust.effects.grain.amount = 30.0;
    r.adjust.calibration.blue_saturation = 15.0;
    r.adjust.geometry.perspective_vertical = 10.0;
    r.adjust.geometry.upright_transforms = Some([[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]; 6]);
    r.crop.left = 0.1;
    r.crop.right = 0.9;
    r.enables.enable_detail = true;
    r.enables.enable_colour_adjustments = true;
    r.enables.enable_effects = true;
    r.enables.enable_calibration = true;
    r.enables.enable_transform = true;
    r
}

fn set(groups: &[SettingsGroup]) -> BTreeSet<SettingsGroup> {
    groups.iter().copied().collect()
}

#[test]
fn copy_paste_and_sync_settings_per_group() {
    let src = edited();
    let mut target = recipe();
    target.adjust.tone.blacks = -9.0;
    // Paste only basic tone and effects: exactly those fields change, bit-exact.
    let clip = SettingsClipboard::copy(&src, set(&[SettingsGroup::BasicTone, SettingsGroup::Effects])).unwrap();
    let (out, losses) = clip.paste(&target).unwrap();
    assert!(losses.is_empty());
    assert_eq!(out.exposure_stops.to_bits(), src.exposure_stops.to_bits());
    assert_eq!((out.adjust.tone.contrast, out.adjust.tone.blacks), (33.3, 0.0), "the whole basic-tone group is copied");
    assert_eq!(out.adjust.effects, src.adjust.effects);
    assert!(out.enables.enable_effects, "effects switch follows the source (sole member copied)");
    assert_eq!((out.adjust.presence, out.adjust.detail, out.adjust.mixer), (target.adjust.presence, target.adjust.detail, target.adjust.mixer));
    assert_eq!((out.crop, out.enables.enable_detail), (target.crop, false));
    // Shared switch: copying only Sharpening of the three detail groups keeps the target switch
    // on if either is on; copying all three follows the source.
    let (s1, _) = copy_groups(&src, &target, &set(&[SettingsGroup::Sharpening])).unwrap();
    assert!(s1.enables.enable_detail && s1.adjust.detail.sharpen == src.adjust.detail.sharpen);
    assert_eq!(s1.adjust.presence.clarity, 0.0, "presence not copied");
    let mut off = src.clone();
    off.enables.enable_detail = false;
    let mut on_target = target.clone();
    on_target.enables.enable_detail = true;
    let (s2, _) = copy_groups(&off, &on_target, &set(&[SettingsGroup::Presence, SettingsGroup::Sharpening, SettingsGroup::NoiseReduction])).unwrap();
    assert!(!s2.enables.enable_detail);
    // Sync everything: equals the source except image-specific state (stored Upright candidates;
    // the crop orientation is also kept but only orientation 1 validates today).
    let all: BTreeSet<SettingsGroup> = SettingsGroup::ALL.into_iter().collect();
    let (full, losses) = copy_groups(&src, &target, &all).unwrap();
    assert!(losses.is_empty());
    assert_eq!((full.crop.left, full.crop.right), (0.1, 0.9));
    assert_eq!(full.adjust.geometry.upright_transforms, None, "re-analysed on the target frame");
    let mut expect = src.clone();
    expect.adjust.geometry.upright_transforms = None;
    assert_eq!(full, expect);
    // An absolute white balance needs the target raw: the relative shift is copied, the loss named.
    let mut wb = src.clone();
    wb.white_balance_mode = WbMode::Temperature;
    wb.adjust.absolute_white_balance = Some(AbsoluteWhiteBalance { kelvin: 5000.0, tint: 3.0 });
    wb.adjust.white_balance.temperature = 11.0;
    let (w, losses) = copy_groups(&wb, &target, &set(&[SettingsGroup::WhiteBalance])).unwrap();
    assert_eq!((w.white_balance_mode, w.adjust.absolute_white_balance, w.adjust.white_balance.temperature), (WbMode::AsShot, None, 11.0));
    assert_eq!(losses.iter().map(|l| l.code).collect::<Vec<_>>(), ["white_balance_needs_target_raw"]);
    // Group keys are stable.
    assert_eq!(SettingsGroup::from_key("colour_grading"), Some(SettingsGroup::ColourGrading));
    assert_eq!(SettingsGroup::ALL.iter().map(|g| g.key()).collect::<BTreeSet<_>>().len(), 19);
}

#[test]
fn virtual_copies_snapshots_and_history() {
    let l = RecordLimits::default();
    let mut rec = DevelopRecord::new("rec-1", "00000000-0000-0000-0000-000000000001", recipe(), l).unwrap();
    let mut a = recipe();
    a.exposure_stops = 1.0;
    let mut b = a.clone();
    b.adjust.tone.contrast = 20.0;
    assert_eq!(rec.edit(1, "Exposure", a.clone()).unwrap(), 2);
    assert_eq!(rec.edit(2, "Contrast", b.clone()).unwrap(), 3);
    // Stale revision: typed conflict naming both revisions, nothing changed.
    let err = rec.edit(2, "Late", recipe()).unwrap_err();
    assert_eq!(err, RecordError::Conflict { record_id: "rec-1".into(), expected: 2, current: 3 });
    assert_eq!((rec.history().len(), rec.recipe()), (3, &b));
    // Undo / redo move the cursor; an edit after undo discards the redo branch.
    rec.undo(3).unwrap();
    assert_eq!((rec.recipe(), rec.cursor()), (&a, 1));
    rec.redo(4).unwrap();
    assert_eq!(rec.recipe(), &b);
    rec.go_to_step(5, 0).unwrap();
    let mut c = recipe();
    c.adjust.presence.vibrance = 40.0;
    rec.edit(6, "Vibrance", c.clone()).unwrap();
    assert_eq!(rec.history().iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["Import", "Vibrance"]);
    assert_eq!(rec.redo(7).unwrap_err().code(), "record_nothing_to_redo");
    // Snapshots: complete states, unique names, applying one is a history step.
    rec.take_snapshot(7, "Warm").unwrap();
    assert_eq!(rec.take_snapshot(8, "Warm").unwrap_err(), RecordError::NameTaken);
    rec.edit(8, "Reset", recipe()).unwrap();
    rec.apply_snapshot(9, "Warm").unwrap();
    assert_eq!((rec.recipe(), rec.history().last().unwrap().label.as_str()), (&c, "Snapshot: Warm"));
    rec.delete_snapshot(10, "Warm").unwrap();
    assert_eq!(rec.apply_snapshot(11, "Warm").unwrap_err(), RecordError::NotFound);
    // Virtual copies: same asset, own history, master pointer; a copy of a copy points at the master.
    let mut vc1 = rec.create_virtual_copy("rec-2", "Copy 1").unwrap();
    let vc2 = vc1.create_virtual_copy("rec-3", "Copy 2").unwrap();
    assert_eq!((vc1.asset_ref(), vc1.master_record(), vc2.master_record()), (rec.asset_ref(), Some("rec-1"), Some("rec-1")));
    assert_eq!((vc1.recipe(), vc1.history().len(), vc1.snapshots().len(), vc1.revision()), (&c, 1, 0, 1));
    vc1.edit(1, "B&W", b.clone()).unwrap();
    assert_eq!(rec.recipe(), &c, "editing a copy leaves the master");
    assert_eq!(rec.create_virtual_copy("rec-1", "Same").unwrap_err(), RecordError::NameTaken);
    // Set copy as master re-points the old master and every sibling; all revisions advance.
    let mut family = vec![rec, vc1, vc2];
    let mut expected: BTreeMap<String, u64> = family.iter().map(|r| (r.record_id().to_owned(), r.revision())).collect();
    expected.remove("rec-3");
    assert_eq!(set_copy_as_master(&mut family, "rec-2", &expected).unwrap_err().code(), "record_family_incomplete");
    expected.insert("rec-3".into(), 1);
    set_copy_as_master(&mut family, "rec-2", &expected).unwrap();
    let view: Vec<(Option<&str>, Option<&str>)> = family.iter().map(|r| (r.master_record(), r.copy_name())).collect();
    assert_eq!(view, [(Some("rec-2"), Some("Copy 1")), (None, None), (Some("rec-2"), Some("Copy 2"))]);
    assert_eq!(family.iter().map(DevelopRecord::revision).collect::<Vec<_>>(), [12, 3, 2]);
}

#[test]
fn batch_apply_is_all_or_nothing_over_the_selection() {
    let l = RecordLimits::default();
    let mut recs: Vec<DevelopRecord> =
        (1..=3).map(|i| DevelopRecord::new(&format!("r{i}"), &format!("asset-{i}"), recipe(), l).unwrap()).collect();
    recs[1].edit(1, "Own", edited()).unwrap();
    let clip = SettingsClipboard::copy(&edited(), set(&[SettingsGroup::Calibration, SettingsGroup::ColourMixer])).unwrap();
    let before = recs.clone();
    // One stale target refuses the whole batch; nothing changes anywhere.
    let stale = [("r1".to_owned(), 1), ("r2".to_owned(), 1), ("r3".to_owned(), 1)];
    let err = batch_apply(&mut recs, &stale, BatchOp::Paste(&clip), "Sync", &l).unwrap_err();
    assert_eq!(err.code(), "record_conflict");
    assert_eq!(recs, before);
    let dup = [("r1".to_owned(), 1), ("r1".to_owned(), 1)];
    assert_eq!(batch_apply(&mut recs, &dup, BatchOp::Paste(&clip), "Sync", &l).unwrap_err().code(), "record_selection_repeated");
    assert_eq!(batch_apply(&mut recs, &[], BatchOp::Paste(&clip), "Sync", &l).unwrap_err().code(), "record_selection_empty");
    assert_eq!(recs, before);
    // Valid selection: every record gets one step; groups outside the clipboard keep their values.
    let sel = [("r1".to_owned(), 1), ("r2".to_owned(), 2), ("r3".to_owned(), 1)];
    let out = batch_apply(&mut recs, &sel, BatchOp::Paste(&clip), "Sync Settings", &l).unwrap();
    assert_eq!(out.iter().map(|o| o.revision).collect::<Vec<_>>(), [2, 3, 2]);
    for r in &recs {
        assert_eq!(r.recipe().adjust.calibration, edited().adjust.calibration);
        assert_eq!(r.recipe().adjust.mixer, edited().adjust.mixer);
        assert_eq!(r.history().last().unwrap().label, "Sync Settings");
    }
    assert_eq!(recs[0].recipe().exposure_stops, 0.0, "basic tone not in the clipboard");
    assert_eq!(recs[1].recipe().exposure_stops, edited().exposure_stops);
    // Reset groups to caller defaults; preset over the selection with an amount.
    let groups = set(&[SettingsGroup::Calibration]);
    batch_apply(&mut recs, &[("r2".to_owned(), 3)], BatchOp::Reset(&recipe(), &groups), "Reset Calibration", &l).unwrap();
    assert_eq!(recs[1].recipe().adjust.calibration, Calibration::default());
    assert!(!recs[1].recipe().enables.enable_calibration);
    let preset = DevelopPreset::parse(b"hsk-develop-preset 1\nmeta name Punch\nset contrast 40\n").unwrap();
    let out = batch_apply(&mut recs, &[("r1".to_owned(), 2), ("r3".to_owned(), 2)], BatchOp::Preset(&preset, 0.5), "Punch", &l).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!((recs[0].recipe().adjust.tone.contrast, recs[2].recipe().adjust.tone.contrast), (20.0, 20.0));
    assert_eq!(
        batch_apply(&mut recs, &[("r1".to_owned(), 3)], BatchOp::Preset(&preset, f64::NAN), "x", &l).unwrap_err().code(),
        DevelopError::NonFinite.code()
    );
}

#[test]
fn hostile_record_inputs_are_bounded() {
    let small = RecordLimits { max_history: 4, max_snapshots: 2, max_name_bytes: 16, max_batch: 2 };
    let mut rec = DevelopRecord::new("r", "a", recipe(), small).unwrap();
    // 100 edits keep the last 4 steps and count what was dropped.
    for i in 0..100u32 {
        let mut x = recipe();
        x.exposure_stops = f64::from(i) / 100.0;
        rec.edit(rec.revision(), "step", x).unwrap();
    }
    assert_eq!((rec.history().len(), rec.trimmed_steps(), rec.cursor()), (4, 97, 3));
    assert_eq!(rec.recipe().exposure_stops, 0.99);
    // Snapshot cap and names (empty, control characters, over the byte bound).
    rec.take_snapshot(rec.revision(), "a").unwrap();
    rec.take_snapshot(rec.revision(), "b").unwrap();
    assert_eq!(rec.take_snapshot(rec.revision(), "c").unwrap_err().code(), "record_snapshot_limit");
    for bad in ["", "   ", "a\u{7}b", "seventeen-bytes!!"] {
        assert_eq!(rec.take_snapshot(rec.revision(), bad).unwrap_err(), RecordError::NameInvalid, "{bad:?}");
    }
    // An invalid recipe is refused and the record is unchanged.
    let snapshot = rec.clone();
    let mut nan = recipe();
    nan.adjust.tone.contrast = f64::NAN;
    assert!(rec.edit(rec.revision(), "bad", nan).is_err());
    let mut wb = recipe();
    wb.white_balance_mode = WbMode::Temperature;
    assert_eq!(rec.edit(rec.revision(), "bad", wb).unwrap_err(), RecordError::Develop(DevelopError::UnsupportedWhiteBalance));
    assert_eq!(rec, snapshot);
    // Selection over the batch bound, and an unknown record, refuse before any change.
    let mut recs = vec![rec.clone()];
    let clip = SettingsClipboard::copy(&recipe(), set(&[SettingsGroup::BasicTone])).unwrap();
    let three = [("r".to_owned(), 1), ("s".to_owned(), 1), ("t".to_owned(), 1)];
    assert_eq!(batch_apply(&mut recs, &three, BatchOp::Paste(&clip), "x", &small).unwrap_err().code(), "record_selection_limit");
    assert_eq!(batch_apply(&mut recs, &[("zz".to_owned(), 1)], BatchOp::Paste(&clip), "x", &small).unwrap_err(), RecordError::NotFound);
    assert_eq!(recs[0], rec);
    assert_eq!(DevelopRecord::new("r", "a", recipe(), RecordLimits { max_history: 0, ..small }).unwrap_err().code(), "record_history_limit");
}

/// Lane SMB-LR-HEALING-2: the spot-removal group copies the retouch and distraction areas bit-exact
/// (normalised coordinates, declared) with their two switches, through copy / paste and through a
/// batch sync; adapter fill records stay the target's and are named as a loss; other groups never
/// touch the retouch stack.
#[test]
fn spot_removal_group_copies_and_syncs() {
    let mut src = recipe();
    src.adjust.retouch.areas = vec![
        RetouchArea { feather: 0.1 + 0.2, ..RetouchArea::circle(SpotType::Heal, (0.3, 0.4), 0.05, (0.45, 0.4)) },
        RetouchArea { source_state: SourceState::EngineChosen, ..RetouchArea::circle(SpotType::Clone, (0.7, 0.2), 0.03, (0.7, 0.2)) },
    ];
    src.adjust.retouch.distraction = vec![DistractionArea { masks: vec![Brush { dabs: vec![(0.5, 0.5)], radius: 0.04, flow: 1.0, feather: 0.0 }], seed: 9 }];
    src.enables.enable_retouch = true;
    src.enables.enable_distraction_removal = true;
    src.adjust.tone.contrast = 20.0;
    let mut target = recipe();
    target.adjust.tone.blacks = -9.0;
    assert_eq!(SettingsGroup::from_key("spot_removal"), Some(SettingsGroup::SpotRemoval));

    let clip = SettingsClipboard::copy(&src, set(&[SettingsGroup::SpotRemoval])).unwrap();
    let (pasted, losses) = clip.paste(&target).unwrap();
    assert!(losses.is_empty());
    let mut expect = target.clone();
    expect.adjust.retouch.areas = src.adjust.retouch.areas.clone();
    expect.adjust.retouch.distraction = src.adjust.retouch.distraction.clone();
    expect.enables.enable_retouch = true;
    expect.enables.enable_distraction_removal = true;
    assert_eq!(pasted, expect, "only the spot-removal group changed");
    assert_eq!(pasted.adjust.retouch.areas[0].feather.to_bits(), (0.1f64 + 0.2).to_bits());

    // Another group leaves the retouch stack alone; copying spot removal from an empty source clears it.
    let (other, _) = copy_groups(&src, &pasted, &set(&[SettingsGroup::BasicTone])).unwrap();
    assert_eq!(other.adjust.retouch, pasted.adjust.retouch);
    let (cleared, _) = copy_groups(&recipe(), &pasted, &set(&[SettingsGroup::SpotRemoval])).unwrap();
    assert!(cleared.adjust.retouch.is_empty() && !cleared.enables.enable_retouch && !cleared.enables.enable_distraction_removal);

    // Adapter fill records are image-specific: not copied, the target keeps its own, the loss named.
    let fill = AdapterFillRecord {
        adapter_id: "studio.adapter.genfill".into(),
        model_id: "vendor/remove".into(),
        model_version: "1".into(),
        ran_locally: true,
        fill_method: "generative".into(),
        image_extent: (100, 100),
        search_rect: AdapterRect { x: 0, y: 0, width: 50, height: 50 },
        target_rect: AdapterRect { x: 10, y: 10, width: 5, height: 5 },
        black_level: 0.0,
        gamma_type: "linear".into(),
        input_digest: [1; 32],
        patch_digest: [2; 32],
        remap_info: Vec::new(),
        variations: Vec::new(),
        selected_variation: None,
    };
    let mut with_fill = src.clone();
    with_fill.enables.enable_distraction_removal = false;
    with_fill.adjust.retouch.adapter_fills = vec![fill];
    let (no_fill, losses) = copy_groups(&with_fill, &target, &set(&[SettingsGroup::SpotRemoval])).unwrap();
    assert!(no_fill.adjust.retouch.adapter_fills.is_empty());
    assert_eq!(losses, vec![GroupLoss { group: SettingsGroup::SpotRemoval, code: "adapter_fill_image_specific" }]);

    // Sync over a selection: one history step per record, every record gets the spots.
    let l = RecordLimits::default();
    let mut recs = vec![DevelopRecord::new("r1", "a1", recipe(), l).unwrap(), DevelopRecord::new("r2", "a2", target.clone(), l).unwrap()];
    let sel: Vec<(String, u64)> = recs.iter().map(|r| (r.record_id().to_owned(), r.revision())).collect();
    let out = batch_apply(&mut recs, &sel, BatchOp::Paste(&clip), "Sync spot removal", &l).unwrap();
    assert_eq!(out.len(), 2);
    for r in &recs {
        assert_eq!(r.recipe().adjust.retouch.areas, src.adjust.retouch.areas);
        assert!(r.recipe().enables.enable_retouch);
        assert_eq!(r.history().last().unwrap().label, "Sync spot removal");
    }
}
