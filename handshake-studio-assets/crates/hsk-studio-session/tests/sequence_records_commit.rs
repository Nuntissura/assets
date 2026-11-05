//! Lane SMB-PR-SEQUENCE-SESSION-1, brief item 2: a sequence-records change commits as ONE revision,
//! ONE receipt, ONE inverse, carrying the three Folio command ids with the caller's context, one
//! Observe frame per operation with codes and counters only. Refusals change nothing. "One step" is
//! proven by submitting the inverse and comparing the exact prior sequences and entries.
mod sequence_records_support;
use hsk_studio_accord::{ActorContext, ClientSessionId, CommandContext, CorrelationId};
use hsk_studio_folio::*;
use hsk_studio_session::*;
use sequence_records_support::*;
use std::sync::{Arc, Mutex};

fn correlation_of(report: &ReportStatus) -> u64 {
    match report {
        ReportStatus::Delivered(receipt) => receipt.correlation_id,
        other => panic!("the frame was not delivered: {other:?}"),
    }
}

/// A session whose `seq-a` has a settings entry.
fn with_entry() -> Session {
    let held = entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap());
    open(&package(&[held]))
}

#[test]
fn the_three_commands_commit_one_revision_and_one_frame_each_under_the_callers_context() {
    bounded_case("three_commands", || {
        let (mut session, frames) = open_counted(&package(&[]));
        assert_eq!(frames.count(), 1, "the open");

        // sequence.preset.apply: a shipped preset on an earlier-shape sequence holding a clip.
        let key = IdempotencyKey::parse("apply-1").unwrap();
        let receipts = session.receipt_count();
        let ctx = context("sequence.preset.apply", 3, 101);
        let applied = session.submit_sequence_command_keyed_as(&key, &ctx, &apply(SHIPPED_25, "seq-b"), 3, &token()).unwrap();
        assert_eq!((applied.provider.as_str(), applied.command_id.as_str(), applied.correlation_id), (SEQUENCE_RECORDS_STEP_PROVIDER, "sequence.preset.apply", 101));
        assert_eq!((applied.previous_revision, applied.revision, applied.changed), (3, 4, true), "one revision");
        assert_eq!(session.receipt_count(), receipts + 1, "one receipt");
        assert_eq!(applied.actor.principal_id, ctx.actor().principal_id(), "the caller's attribution");
        assert_eq!(correlation_of(&applied.report), 101, "the frame carries the command's correlation");
        assert_eq!(frames.count(), 2, "one frame");
        let shipped = shipped_sequence_presets();
        let want = shipped.preset(SHIPPED_25).unwrap().settings.clone();
        let live = session.sequence_settings_records(&token()).unwrap().unwrap();
        assert_eq!(live.entry("seq-b").map(|e| (e.editing_mode.as_str(), &e.settings)), Some((CUSTOM_EDITING_MODE, &want)));
        assert_eq!(session.snapshot().document().sequences[1].frame_rate, SequenceFrameRate { ticks_per_frame: F25 }, "the frame rate moves with the record");
        assert_eq!(session.sequence_presets(&token()).unwrap(), None, "apply stores no preset");
        assert_eq!(
            applied.sequence_settings_report,
            Some(StudioSequenceSettingsReport { command: "sequence.preset.apply", base_revision: 3, correlation: 101, presets: 0, settings_entries: 1, frame_rate_changed: true })
        );

        // sequence.editing_mode.set.
        let set = run(&mut session, &set_mode("seq-b", SHIPPED_MODE), 102).unwrap();
        assert_eq!((set.command_id.as_str(), set.previous_revision, set.revision, correlation_of(&set.report)), ("sequence.editing_mode.set", 4, 5, 102));
        assert_eq!(session.sequence_settings_records(&token()).unwrap().unwrap().entry("seq-b").unwrap().editing_mode, SHIPPED_MODE);
        assert_eq!(frames.count(), 3);

        // sequence.preset.save_from_sequence: saved in the stored set, the shipped library not copied.
        let saved = run(&mut session, &save("seq-b", "mine"), 103).unwrap();
        assert_eq!((saved.command_id.as_str(), saved.previous_revision, saved.revision, correlation_of(&saved.report)), ("sequence.preset.save_from_sequence", 5, 6, 103));
        let stored = session.sequence_presets(&token()).unwrap().unwrap();
        assert_eq!(stored.presets.len(), 1);
        assert_eq!(stored.preset("mine").unwrap().settings, want);
        assert_eq!(saved.sequence_settings_report.unwrap().presets, 1, "the report counts stored presets");
        assert_eq!(frames.count(), 4);

        // Frames hold codes and counters only: none of the command's text reaches the sink.
        let sent = String::from_utf8_lossy(&frames.bytes()).into_owned();
        for text in ["seq-b", "mine", SHIPPED_25, SHIPPED_MODE, "sequence.preset", "Saved preset", "editing_mode"] {
            assert!(!sent.contains(text), "{text} in an Observe frame");
        }
        // A stored preset is usable by a later apply, and shadows nothing it should not.
        let later = run(&mut session, &apply("mine", "seq-a"), 104).unwrap();
        assert_eq!(later.revision, 7);
        assert_eq!(session.sequence_settings_records(&token()).unwrap().unwrap().sequences.len(), 2);
    });
}

#[test]
fn undo_and_redo_restore_the_exact_sequences_and_entries() {
    bounded_case("undo_redo", || {
        let mut session = open(&package(&[]));
        let commands = [apply(SHIPPED_25, "seq-b"), set_mode("seq-b", SHIPPED_MODE), save("seq-b", "mine")];
        for (i, command) in commands.iter().enumerate() {
            let before = values(&session);
            let applied = run(&mut session, command, 200 + i as u64).unwrap();
            let after = values(&session);
            assert_ne!(after, before, "{command:?} changed something");
            let revision = applied.revision;

            let undo = session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), revision, &token()).unwrap();
            assert_eq!((undo.previous_revision, undo.revision), (revision, revision + 1), "undo is one revision");
            assert_eq!(values(&session), before, "undo of {command:?}");

            let ctx = context("cmd.redo", revision + 1, 300 + i as u64);
            let redo = session.submit_sequence_records_as(&ctx, &fresh_inverse(&undo, "cmd.redo"), revision + 1, &token()).unwrap();
            assert_eq!(redo.revision, revision + 2);
            assert_eq!(correlation_of(&redo.report), 300 + i as u64);
            assert_eq!(values(&session), after, "redo of {command:?}");
        }
        // The first undo removes the entry it created: nothing is left behind.
        let mut fresh = open(&package(&[]));
        let applied = run(&mut fresh, &apply(SHIPPED_25, "seq-b"), 1).unwrap();
        fresh.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), applied.revision, &token()).unwrap();
        assert_eq!(fresh.document_entry(SEQUENCE_SETTINGS_ENTRY), None);
        assert_eq!(fresh.snapshot().document().sequences, vec![seq_a(), seq_b()]);
        // An inverse is bound to what it undoes: replaying it conflicts.
        assert_eq!(fresh.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo.again"), applied.revision + 1, &token()).unwrap_err().code(), "value_conflict");
    });
}

#[test]
fn a_records_only_step_and_a_step_that_changes_nothing() {
    bounded_case("records_only", || {
        let mut session = with_entry();
        let live = session.sequence_settings_records(&token()).unwrap();
        let next = records(vec![settings_entry("seq-a", SHIPPED_MODE, F25)]);
        let edit = step(vec![], Some(RecordEdit { expect: live.clone(), write: Some(next.clone()) }), None);
        let applied = session.submit_sequence_records(&edit, 3, &token()).unwrap();
        assert_eq!((applied.previous_revision, applied.revision, applied.changed), (3, 4, true), "a records-only step is one revision");
        assert!(applied.sequence_settings_report.is_none());
        assert_eq!(session.snapshot().document().sequences, vec![seq_a(), seq_b()]);
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(next.clone()));
        // Nothing to do: no revision, no inverse, no error.
        let same = step(vec![], Some(RecordEdit { expect: Some(next.clone()), write: Some(next.clone()) }), None);
        let noop = session.submit_sequence_records(&same, 4, &token()).unwrap();
        assert!(!noop.changed && noop.revision == 4 && noop.sequence_records_inverse.is_none());
        // A command that writes what is already there is no change either.
        let repeat = run(&mut session, &set_mode("seq-a", SHIPPED_MODE), 1).unwrap();
        assert!(!repeat.changed && repeat.revision == 4 && repeat.sequence_records_inverse.is_none());
        // The preset entry alone, and the entry removed.
        let presets = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) }));
        assert_eq!(session.submit_sequence_records(&presets, 4, &token()).unwrap().revision, 5);
        let remove = step(vec![], None, Some(RecordEdit { expect: Some(library(vec![preset("mine", F25)])), write: None }));
        assert_eq!(session.submit_sequence_records(&remove, 5, &token()).unwrap().revision, 6);
        assert_eq!(session.document_entry(SEQUENCE_PRESETS_ENTRY), None);
    });
}

#[test]
fn keyed_retries_replay_the_receipt_and_a_changed_request_conflicts() {
    bounded_case("keyed", || {
        let mut session = open(&package(&[]));
        let key = IdempotencyKey::parse("cmd-1").unwrap();
        let ctx = context("sequence.preset.apply", 3, 11);
        let first = session.submit_sequence_command_keyed_as(&key, &ctx, &apply(SHIPPED_25, "seq-b"), 3, &token()).unwrap();
        assert!(!first.replayed);
        let held = state(&session);
        let retry = session.submit_sequence_command_keyed_as(&key, &ctx, &apply(SHIPPED_25, "seq-b"), 3, &token()).unwrap();
        assert!(retry.replayed && retry.revision == first.revision, "the stored receipt, computed once");
        assert_eq!(retry.sequence_records_inverse, first.sequence_records_inverse);
        assert_eq!(state(&session), held, "nothing applied twice");
        let other = session.submit_sequence_command_keyed_as(&key, &ctx, &apply(SHIPPED_25, "seq-a"), 3, &token()).unwrap_err();
        assert_eq!(other.code(), "idempotency_conflict");
        // The step form under a key.
        let undo = fresh_inverse(&first, "cmd.undo");
        let step_key = IdempotencyKey::parse("undo-1").unwrap();
        let undone = session.submit_sequence_records_keyed(&step_key, &undo, 4, &token()).unwrap();
        assert_eq!(undone.revision, 5);
        let replay = session.submit_sequence_records_keyed(&step_key, &undo, 4, &token()).unwrap();
        assert!(replay.replayed && replay.revision == 5);
        assert_eq!(session.document_revision(), 5);
    });
}

#[test]
fn refusals_leave_the_document_and_the_entries_untouched() {
    bounded_case("refusals", || {
        let (mut session, frames) = open_counted(&package(&[entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap())]));
        let held = state(&session);
        let receipts = session.receipt_count();
        let mut ops = 0usize;

        // Folio's command codes, each one frame under the command's correlation.
        let cases = [
            (apply("no-such-preset", "seq-a"), "preset_missing"),
            (apply(SHIPPED_25, "seq-x"), "sequence_missing"),
            (save("seq-b", "mine"), "settings_missing"),
            (save("seq-x", "mine"), "sequence_missing"),
            (save("seq-a", SHIPPED_25), "duplicate_preset"),
            (save("seq-a", "a/b"), "invalid_preset_id"),
            (set_mode("seq-a", "no-such-mode"), "mode_unknown"),
            (set_mode("seq-b", CUSTOM_EDITING_MODE), "settings_missing"),
        ];
        for (i, (command, code)) in cases.iter().enumerate() {
            let correlation = 500 + i as u64;
            let failed = run(&mut session, command, correlation).unwrap_err();
            assert_eq!(failed.code(), *code, "{command:?}");
            assert_eq!(correlation_of(&failed.report), correlation, "{command:?}");
            ops += 1;
        }
        let blank = StudioSequenceSettingsCommand::SavePresetFromSequence { sequence_id: "seq-a".into(), preset_id: "x".into(), name: " ".into(), category_path: vec![] };
        assert_eq!(run(&mut session, &blank, 1).unwrap_err().code(), "invalid_name");
        let deep = StudioSequenceSettingsCommand::SavePresetFromSequence { sequence_id: "seq-a".into(), preset_id: "x".into(), name: "x".into(), category_path: vec!["c".into(); 9] };
        assert_eq!(run(&mut session, &deep, 2).unwrap_err().code(), "invalid_category");
        ops += 2;

        // The context must fit the command and the session.
        let good = apply(SHIPPED_25, "seq-a");
        let wrong_command = context("sequence.editing_mode.set", 3, 601);
        assert_eq!(session.submit_sequence_command_as(&wrong_command, &good, 3, &token()).unwrap_err().code(), "command_mismatch");
        let no_command = CommandContext::new(actor(), ClientSessionId::parse("w").unwrap(), None, 3, CorrelationId::new(602), None);
        assert_eq!(session.submit_sequence_command_as(&no_command, &good, 3, &token()).unwrap_err().code(), "command_missing");
        let stale_base = context("sequence.preset.apply", 2, 603);
        assert_eq!(session.submit_sequence_command_as(&stale_base, &good, 3, &token()).unwrap_err().code(), "stale_revision");
        let stale_both = context("sequence.preset.apply", 2, 604);
        assert_eq!(session.submit_sequence_command_as(&stale_both, &good, 2, &token()).unwrap_err().code(), "stale_revision");
        let stranger = ActorContext::new("account-2", "principal-2", "owner", "owner-principal", "space", "session").unwrap();
        let foreign = CommandContext::new(stranger, ClientSessionId::parse("w").unwrap(), Some(hsk_studio_accord::CommandId::parse("sequence.preset.apply").unwrap()), 3, CorrelationId::new(605), None);
        let refused = session.submit_sequence_command_as(&foreign, &good, 3, &token()).unwrap_err();
        assert_eq!(refused.code(), "actor_mismatch");
        assert_eq!(correlation_of(&refused.report), 605);
        ops += 5;
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.submit_sequence_command_as(&context("sequence.preset.apply", 3, 606), &good, 3, &canceled).unwrap_err().code(), "canceled");
        ops += 1;

        // The step form: quotes, targets, command and revision.
        let live = session.sequence_settings_records(&token()).unwrap();
        let write = Some(records(vec![settings_entry("seq-a", SHIPPED_MODE, F25)]));
        let good_step = step(vec![], Some(RecordEdit { expect: live.clone(), write: write.clone() }), None);
        let code = |session: &mut Session, s: &SequenceRecordsStep, rev: u64| session.submit_sequence_records(s, rev, &token()).unwrap_err().code();
        let stale_quote = step(vec![], Some(RecordEdit { expect: None, write: write.clone() }), None);
        assert_eq!(code(&mut session, &stale_quote, 3), "value_conflict", "the quoted records are not the live ones");
        let stale_presets = step(vec![], None, Some(RecordEdit { expect: Some(library(vec![])), write: None }));
        assert_eq!(code(&mut session, &stale_presets, 3), "value_conflict", "the preset quote is checked too");
        let dangling = step(vec![], Some(RecordEdit { expect: live.clone(), write: Some(records(vec![settings_entry("seq-x", "custom", F25)])) }), None);
        assert_eq!(code(&mut session, &dangling, 3), "folio_rejected", "records must validate against the result");
        let wrong_rate = step(vec![], Some(RecordEdit { expect: live.clone(), write: Some(records(vec![settings_entry("seq-a", "custom", F24)])) }), None);
        assert_eq!(code(&mut session, &wrong_rate, 3), "folio_rejected");
        let bad_set = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("p", F25), preset("p", F25)])) }));
        assert_eq!(code(&mut session, &bad_set, 3), "folio_rejected");
        let huge = library((0..4096).map(|i| preset(&format!("p{i}"), F25)).collect());
        let over_budget = step(vec![], None, Some(RecordEdit { expect: None, write: Some(huge) }));
        assert_eq!(code(&mut session, &over_budget, 3), "budget_exceeded", "a body a reopen would refuse is refused now");
        let stale_sequence = step(vec![SequenceEdit::Replace { expect: seq_b(), write: sequence("seq-b", F25, vec![]) }, SequenceEdit::Replace { expect: sequence("seq-a", F24, vec![]), write: seq_a() }], None, None);
        assert_eq!(code(&mut session, &stale_sequence, 3), "value_conflict");
        let twice = step(vec![SequenceEdit::Remove { expect: seq_b() }, SequenceEdit::Remove { expect: seq_b() }], None, None);
        assert_eq!(code(&mut session, &twice, 3), "invalid_input");
        let unknown = step(vec![SequenceEdit::Remove { expect: sequence("seq-x", F25, vec![]) }], None, None);
        assert_eq!(code(&mut session, &unknown, 3), "invalid_target");
        let mut no_command_step = good_step.clone();
        no_command_step.command_id = String::new();
        assert_eq!(code(&mut session, &no_command_step, 3), "invalid_command");
        assert_eq!(code(&mut session, &good_step, 2), "stale_revision");
        assert_eq!(session.submit_sequence_records(&good_step, 3, &canceled).unwrap_err().code(), "canceled");
        ops += 12;
        let mismatch = context("sequence.preset.apply", 3, 700);
        assert_eq!(session.submit_sequence_records_as(&mismatch, &good_step, 3, &token()).unwrap_err().code(), "command_mismatch");
        ops += 1;

        assert_eq!(state(&session), held, "every refusal leaves sequences, entries and revision untouched");
        assert_eq!(session.receipt_count(), receipts);
        assert_eq!(frames.count(), 1 + ops, "exactly one frame per operation, refusals included");
        // The good step still commits afterwards.
        assert_eq!(session.submit_sequence_records(&good_step, 3, &token()).unwrap().revision, 4);
    });
}

#[test]
fn a_step_that_would_not_fit_the_package_limit_is_refused() {
    bounded_case("package_limit", || {
        let mut small = SessionConfig::new(actor());
        small.budgets.limits.max_entry_bytes = 64 * 1024;
        let mut session = match Session::open_bytes(&package(&[]), small, ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        let shipped = shipped_sequence_presets();
        let too_large = step(vec![], None, Some(RecordEdit { expect: None, write: Some(shipped) }));
        assert_eq!(session.submit_sequence_records(&too_large, 3, &token()).unwrap_err().code(), "total_too_large");
        assert_eq!((session.document_revision(), session.document_entry(SEQUENCE_PRESETS_ENTRY)), (3, None));
    });
}

#[test]
fn removing_a_sequence_its_settings_entry_names_is_refused_as_multicam_refuses() {
    bounded_case("orphan_removal", || {
        let mut session = with_entry();
        let held = state(&session);
        // A multicam step cannot remove it (the only sequence removal path before this lane).
        let multicam = MulticamStep { command_id: "cmd.remove".into(), correlation_id: 1, sequences: vec![SequenceEdit::Remove { expect: seq_a() }], records_expect: None, records_write: None };
        assert_eq!(session.submit_multicam(&multicam, 3, &token()).unwrap_err().error.code(), "folio_rejected");
        assert_eq!(state(&session), held, "the refusal changes nothing");
        // Nor can a records step that leaves the entry behind, quoting it or not.
        let silent = step(vec![SequenceEdit::Remove { expect: seq_a() }], None, None);
        assert_eq!(session.submit_sequence_records(&silent, 3, &token()).unwrap_err().code(), "folio_rejected");
        let live = session.sequence_settings_records(&token()).unwrap();
        let quoted = step(vec![SequenceEdit::Remove { expect: seq_a() }], Some(RecordEdit { expect: live.clone(), write: live.clone() }), None);
        assert_eq!(session.submit_sequence_records(&quoted, 3, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(state(&session), held);
        // A sequence without an entry is removed as before.
        let free = MulticamStep { command_id: "cmd.remove".into(), correlation_id: 1, sequences: vec![SequenceEdit::Remove { expect: seq_b() }], records_expect: None, records_write: None };
        let removed = session.submit_multicam(&free, 3, &token()).unwrap();
        session.submit_multicam(&{ let mut u = removed.multicam_inverse.clone().unwrap(); u.command_id = "cmd.undo".into(); u }, 4, &token()).unwrap();
        // The removal and the entry together are one step, and undo restores both exactly.
        let before = values(&session);
        let together = step(vec![SequenceEdit::Remove { expect: seq_a() }], Some(RecordEdit { expect: live.clone(), write: None }), None);
        let applied = session.submit_sequence_records(&together, 5, &token()).unwrap();
        assert_eq!(applied.revision, 6, "one revision");
        assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY), None);
        assert_eq!(session.snapshot().document().sequences.len(), 1);
        session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 6, &token()).unwrap();
        assert_eq!(values(&session), before, "the sequence comes back at its index, with its entry");
        // A created sequence and its entry are one step too.
        let created = sequence("seq-c", F25, vec![]);
        let mut write = live.clone().unwrap();
        write.sequences.push(settings_entry("seq-c", "custom", F25));
        let create = step(vec![SequenceEdit::Insert { at: 2, write: created }], Some(RecordEdit { expect: live.clone(), write: Some(write.clone()) }), None);
        assert_eq!(session.submit_sequence_records(&create, 7, &token()).unwrap().revision, 8);
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(write));
    });
}

#[test]
fn a_timeline_step_cannot_break_the_entry_but_ordinary_edits_commit() {
    bounded_case("timeline_guard", || {
        let mut session = with_entry();
        let held = state(&session);
        // A frame rate the entry does not declare: a reopen would refuse the file.
        let rate = TimelineStep { command_id: "cmd.rate".into(), correlation_id: 1, sequence_id: "seq-a".into(), expect: seq_a(), write: sequence("seq-a", F24, vec![]) };
        assert_eq!(session.submit_timeline(&rate, 3, &token()).unwrap_err().error.code(), "folio_rejected");
        // The same drift through the other two paths that carry a sequence value (F-R55-4(d)).
        let drifted = SequenceEdit::Replace { expect: seq_a(), write: sequence("seq-a", F24, vec![]) };
        let multicam = MulticamStep { command_id: "cmd.rate".into(), correlation_id: 1, sequences: vec![drifted.clone()], records_expect: None, records_write: None };
        assert_eq!(session.submit_multicam(&multicam, 3, &token()).unwrap_err().error.code(), "folio_rejected");
        assert_eq!(session.submit_sequence_records(&step(vec![drifted], None, None), 3, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(state(&session), held);
        // A sequence without an entry, and a value edit keeping the rate, commit as before.
        let free = TimelineStep { command_id: "cmd.rate".into(), correlation_id: 2, sequence_id: "seq-b".into(), expect: seq_b(), write: sequence("seq-b", F25, vec![]) };
        assert_eq!(session.submit_timeline(&free, 3, &token()).unwrap().revision, 4);
        let mut renamed = seq_a();
        renamed.name = "renamed".into();
        let edit = TimelineStep { command_id: "cmd.name".into(), correlation_id: 3, sequence_id: "seq-a".into(), expect: seq_a(), write: renamed };
        assert_eq!(session.submit_timeline(&edit, 4, &token()).unwrap().revision, 5);
        assert!(session.sequence_settings_records(&token()).unwrap().is_some());
        // The rate and the record change together through the records step.
        let live = session.sequence_settings_records(&token()).unwrap();
        let mut value = session.snapshot().document().sequences[0].clone();
        let expect = value.clone();
        value.frame_rate = SequenceFrameRate { ticks_per_frame: F24 };
        let both = step(vec![SequenceEdit::Replace { expect, write: value }], Some(RecordEdit { expect: live, write: Some(records(vec![settings_entry("seq-a", "custom", F24)])) }), None);
        assert_eq!(session.submit_sequence_records(&both, 5, &token()).unwrap().revision, 6);
        assert!(session.sequence_settings_records(&token()).unwrap().is_some());
    });
}

#[test]
fn a_records_step_cannot_orphan_the_multicam_records() {
    bounded_case("multicam_orphan", || {
        let mut session = open(&package_of(&[seq_a(), seq_b()], &[]));
        let mut multicam = StudioMulticamRecords::new(doc_id());
        multicam.multicam_sequences.push(StudioMulticamSequence {
            sequence_id: "seq-b".into(),
            selection_order: MulticamSelectionOrder::SourceOrder,
            angle_order: multicam_angle_order(&seq_b(), MulticamSelectionOrder::SourceOrder),
        });
        session.set_multicam_records(Some(&multicam), &token()).unwrap();
        let held = state(&session);
        let remove = step(vec![SequenceEdit::Remove { expect: seq_b() }], None, None);
        assert_eq!(session.submit_sequence_records(&remove, 3, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(state(&session), held);
        assert_eq!(session.multicam_records(&token()).unwrap(), Some(multicam));
        // A sequence the multicam records do not name is removed.
        let free = step(vec![SequenceEdit::Remove { expect: seq_a() }], None, None);
        assert_eq!(session.submit_sequence_records(&free, 3, &token()).unwrap().revision, 4);
    });
}

fn dangling_records() -> StudioSequenceSettingsRecords {
    records(vec![settings_entry("seq-x", "custom", F25)])
}
/// The ways the settings entry can be invalid while the package still opens, each with the code a
/// command that reads it answers.
fn invalid_settings() -> Vec<(&'static str, Vec<hsk_studio_package::Quarantined>, &'static str)> {
    let good = serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap();
    let other = serde_json::to_vec(&records(vec![settings_entry("seq-b", "custom", F24)])).unwrap();
    vec![
        ("names a sequence the document does not hold", vec![entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&dangling_records()).unwrap())], "folio_rejected"),
        ("is not JSON", vec![entry(SEQUENCE_SETTINGS_ENTRY, b"{not json")], "folio_rejected"),
        ("is past the input budget", vec![entry(SEQUENCE_SETTINGS_ENTRY, &vec![b' '; 1_048_577])], "budget_exceeded"),
        ("is repeated", vec![entry(SEQUENCE_SETTINGS_ENTRY, &good), entry(SEQUENCE_SETTINGS_ENTRY, &other)], "folio_rejected"),
    ]
}

/// Lead decision 2026-11-05: a commit that would use an invalid entry is refused and leaves the
/// document and the entry untouched.
#[test]
fn a_commit_that_would_use_an_invalid_settings_entry_is_refused_and_changes_nothing() {
    bounded_case("use_invalid_settings", || {
        for (what, held, code) in invalid_settings() {
            let (mut session, frames) = open_counted(&package(&held));
            let before = state(&session);
            let receipts = session.receipt_count();
            let opened = frames.count();
            // All three commands read the entry.
            let commands = [apply(SHIPPED_25, "seq-a"), set_mode("seq-a", SHIPPED_MODE), save("seq-a", "mine")];
            for (i, command) in commands.iter().enumerate() {
                let correlation = 800 + i as u64;
                let failed = run(&mut session, command, correlation).unwrap_err();
                assert_eq!(failed.code(), code, "{what}: {command:?}");
                assert_eq!(correlation_of(&failed.report), correlation, "{what}");
            }
            assert_eq!(state(&session), before, "{what}: the document and the entry are untouched");
            assert_eq!(session.receipt_count(), receipts, "{what}");
            assert_eq!(frames.count(), opened + commands.len(), "{what}: one frame per refused command");
        }
        // A step's quote of bytes that do not parse, or of a repeated token, never matches.
        for (what, held, _) in invalid_settings().into_iter().filter(|(what, _, _)| matches!(*what, "is not JSON" | "is repeated")) {
            let mut session = open(&package(&held));
            let before = state(&session);
            let good = records(vec![settings_entry("seq-a", "custom", F25)]);
            for expect in [None, Some(good.clone())] {
                let quoted = step(vec![], Some(RecordEdit { expect, write: Some(good.clone()) }), None);
                assert_eq!(session.submit_sequence_records(&quoted, 3, &token()).unwrap_err().code(), "value_conflict", "{what}");
            }
            assert_eq!(state(&session), before, "{what}");
        }
        // Writing the same invalid records back is using them: refused once anything else changes.
        let dangling = serde_json::to_vec(&dangling_records()).unwrap();
        let mut session = open(&package(&[entry(SEQUENCE_SETTINGS_ENTRY, &dangling)]));
        let before = state(&session);
        let same = RecordEdit { expect: Some(dangling_records()), write: Some(dangling_records()) };
        let mut renamed = seq_b();
        renamed.name = "renamed".into();
        let with_edit = step(vec![SequenceEdit::Replace { expect: seq_b(), write: renamed }], Some(same.clone()), None);
        assert_eq!(session.submit_sequence_records(&with_edit, 3, &token()).unwrap_err().code(), "folio_rejected");
        let unwritten = step(vec![], Some(same), None);
        assert!(!session.submit_sequence_records(&unwritten, 3, &token()).unwrap().changed, "nothing to do, nothing written");
        // A replacement that is itself invalid is refused too.
        let still_bad = step(vec![], Some(RecordEdit { expect: Some(dangling_records()), write: Some(records(vec![settings_entry("seq-y", "custom", F25)])) }), None);
        assert_eq!(session.submit_sequence_records(&still_bad, 3, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(state(&session), before);
    });
}

#[test]
fn a_commit_that_would_use_an_invalid_preset_entry_is_refused_and_changes_nothing() {
    bounded_case("use_invalid_presets", || {
        let settings = entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap());
        let repeated_set = serde_json::to_vec(&library(vec![preset("p", F25), preset("p", F25)])).unwrap();
        for (what, body, parses) in [("is not JSON", b"\xff\xfe".to_vec(), false), ("repeats a preset", repeated_set, true)] {
            let mut session = open(&package(&[settings.clone(), entry(SEQUENCE_PRESETS_ENTRY, &body)]));
            let before = state(&session);
            // Apply and save read the set, even for a shipped preset: a stored one could shadow it.
            assert_eq!(run(&mut session, &apply(SHIPPED_25, "seq-a"), 1).unwrap_err().code(), "folio_rejected", "{what}");
            assert_eq!(run(&mut session, &save("seq-a", "mine"), 2).unwrap_err().code(), "folio_rejected", "{what}");
            if !parses {
                let quoted = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("q", F25)])) }));
                assert_eq!(session.submit_sequence_records(&quoted, 3, &token()).unwrap_err().code(), "value_conflict", "{what}");
            }
            assert_eq!(state(&session), before, "{what}");
            // Setting an editing mode consults no preset, so a bad set does not stop it.
            let applied = run(&mut session, &set_mode("seq-a", SHIPPED_MODE), 3).unwrap();
            assert_eq!(applied.revision, 4, "{what}");
            assert_eq!(session.document_entry(SEQUENCE_PRESETS_ENTRY), Some(body.as_slice()), "{what}: kept as it came");
        }
    });
}

/// A file with one bad entry stays editable: only what uses the entry is refused.
#[test]
fn a_commit_that_does_not_use_the_invalid_entry_commits_and_keeps_its_bytes() {
    bounded_case("not_using_invalid", || {
        for (what, held, _) in invalid_settings() {
            let mut session = open(&package(&held));
            let kept = session.document_entry(SEQUENCE_SETTINGS_ENTRY).map(<[u8]>::to_vec);
            // A timeline step, a frame-rate change included: the guard protects valid entries only.
            let rate = TimelineStep { command_id: "cmd.rate".into(), correlation_id: 1, sequence_id: "seq-a".into(), expect: seq_a(), write: sequence("seq-a", F24, vec![]) };
            assert_eq!(session.submit_timeline(&rate, 3, &token()).unwrap().revision, 4, "{what}");
            // A sequence removal.
            let remove = MulticamStep { command_id: "cmd.remove".into(), correlation_id: 2, sequences: vec![SequenceEdit::Remove { expect: seq_b() }], records_expect: None, records_write: None };
            assert_eq!(session.submit_multicam(&remove, 4, &token()).unwrap().revision, 5, "{what}");
            // A records step on the other entry.
            let presets = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) }));
            assert_eq!(session.submit_sequence_records(&presets, 5, &token()).unwrap().revision, 6, "{what}");
            assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY).map(<[u8]>::to_vec), kept, "{what}: byte-for-byte");
            assert!(session.sequence_settings_records(&token()).is_err(), "{what}: still refused on read");
        }
    });
}

#[test]
fn a_successful_commit_replaces_a_parsable_invalid_entry() {
    bounded_case("replace_invalid", || {
        let dangling = entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&dangling_records()).unwrap());
        let mut session = open(&package(std::slice::from_ref(&dangling)));
        assert_eq!(session.loss().len(), 1);
        let good = records(vec![settings_entry("seq-a", "custom", F25)]);
        let repair = step(vec![], Some(RecordEdit { expect: Some(dangling_records()), write: Some(good.clone()) }), None);
        let applied = session.submit_sequence_records(&repair, 3, &token()).unwrap();
        assert_eq!((applied.revision, applied.changed), (4, true));
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(good.clone()));
        assert!(session.loss().is_empty(), "the note named an entry that is gone: {:?}", session.loss());
        // Its undo would write invalid records back: refused, the repair stands. Declared reading.
        let held = state(&session);
        assert_eq!(session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(state(&session), held);
        // Commands work again.
        assert_eq!(run(&mut session, &set_mode("seq-a", SHIPPED_MODE), 1).unwrap().revision, 5);
        // A commit can also remove it.
        let mut removed = open(&package(std::slice::from_ref(&dangling)));
        let remove = step(vec![], Some(RecordEdit { expect: Some(dangling_records()), write: None }), None);
        assert_eq!(removed.submit_sequence_records(&remove, 3, &token()).unwrap().revision, 4);
        assert_eq!(removed.document_entry(SEQUENCE_SETTINGS_ENTRY), None);
        assert!(removed.loss().is_empty());
    });
}

/// R55 F-R55-4(a), (c): a sequence with no settings entry passes validation with no report, and nest
/// and multicam create such sequences (they reach the session as an insert). The query lists them,
/// and nothing creates an entry for them.
#[test]
fn the_sequences_without_settings_are_listed_in_document_order_r55_f4() {
    bounded_case("list_without_settings", || {
        let (mut session, frames) = open_counted(&package(&[]));
        let list = |s: &Session| s.sequences_without_settings(&token()).unwrap();
        assert_eq!(list(&session), ["seq-a", "seq-b"], "no entry stored: every sequence, in document order");
        assert!(!session.is_dirty(), "a read changes nothing");
        assert_eq!(frames.count(), 1, "a read sends no frame, as the other typed reads");

        // An entry takes its sequence off the list.
        run(&mut session, &apply(SHIPPED_25, "seq-b"), 1).unwrap();
        assert_eq!(list(&session), ["seq-a"]);

        // A created sequence is listed at its document position, and no entry appears for it.
        let created = sequence("seq-c", F25, vec![]);
        let insert = MulticamStep { command_id: "cmd.nest".into(), correlation_id: 2, sequences: vec![SequenceEdit::Insert { at: 0, write: created }], records_expect: None, records_write: None };
        let held = session.document_entry(SEQUENCE_SETTINGS_ENTRY).map(<[u8]>::to_vec);
        session.submit_multicam(&insert, 4, &token()).unwrap();
        assert_eq!(list(&session), ["seq-c", "seq-a"], "document order, not creation order");
        assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY).map(<[u8]>::to_vec), held, "no entry is created for it");

        // Listing is not a repeatable surprise: same answer twice, and the state does not move.
        let before = state(&session);
        assert_eq!(list(&session), list(&session));
        assert_eq!(state(&session), before);

        // Every sequence with an entry: an empty list, not an error.
        run(&mut session, &apply(SHIPPED_25, "seq-a"), 3).unwrap();
        run(&mut session, &apply(SHIPPED_25, "seq-c"), 4).unwrap();
        assert!(list(&session).is_empty());
        // A document with no sequences has nothing to list.
        assert!(list(&open(&package_of(&[], &[]))).is_empty());
    });
}

#[test]
fn listing_sequences_without_settings_refuses_while_the_entry_is_invalid_r55_f4() {
    bounded_case("list_refusal", || {
        for (what, held, _) in invalid_settings() {
            let session = open(&package(&held));
            let refused = session.sequences_without_settings(&token()).unwrap_err();
            assert_eq!(refused.code(), "document_rejected", "{what}: {refused:?}");
            assert_eq!(refused, session.sequence_settings_records(&token()).unwrap_err(), "{what}: the same refusal as the read");
        }
        let canceled = token();
        canceled.cancel();
        assert_eq!(open(&package(&[])).sequences_without_settings(&canceled).unwrap_err().code(), "canceled");
        // A bad preset entry is not the settings entry: the list still answers.
        let session = open(&package(&[entry(SEQUENCE_PRESETS_ENTRY, b"{not json")]));
        assert_eq!(session.sequences_without_settings(&token()).unwrap(), ["seq-a", "seq-b"]);
    });
}

/// R55 F-R55-7: Folio's ApplyPreset hands back a document at the unchanged revision, not validated
/// into a snapshot. The session step is what bumps and validates: every shipped preset applied through
/// the session commits one revision, and the document it leaves passes `validate_document`.
#[test]
fn a_preset_applied_through_the_session_commits_one_revision_and_validates_r55_f7() {
    bounded_case("applied_validates", || {
        let mut session = open(&package(&[]));
        let mut revision = session.document_revision();
        let start = revision;
        for (i, preset) in shipped_sequence_presets().presets.iter().enumerate() {
            // seq-b holds a clip: whether a rate change re-times it is the spec owner's (F-R55-7).
            let applied = run(&mut session, &apply(&preset.preset_id, "seq-b"), 1000 + i as u64).unwrap();
            let next = revision + u64::from(applied.changed);
            assert_eq!((applied.previous_revision, applied.revision, session.document_revision()), (revision, next, next), "{}", preset.preset_id);
            validate_document(session.snapshot().document().clone(), Budget::default(), &token(), &Granted).unwrap_or_else(|d| panic!("{}: {d:?}", preset.preset_id));
            assert_eq!(session.snapshot().document().sequences[1].frame_rate, preset.settings.frame_rate, "{}", preset.preset_id);
            let records = session.sequence_settings_records(&token()).unwrap().unwrap();
            assert_eq!(records.entry("seq-b").unwrap().settings, preset.settings, "{}", preset.preset_id);
            revision = next;
        }
        assert!(revision > start, "the walk changed the document");
        // Saved and reopened, the last of them is still a valid document with a valid entry.
        let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        assert_eq!(reopened.document_revision(), revision);
        assert!(reopened.sequence_settings_records(&token()).unwrap().is_some());
    });
}

/// R55 F-R55-6: the session still answers a budget overrun as itself once Folio names it.
#[test]
fn a_command_over_the_folio_budget_answers_budget_exceeded_r55_f6() {
    bounded_case("command_budget", || {
        let mut small = SessionConfig::new(actor());
        small.budgets.chronicle.folio.nodes = 64;
        let held = entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap());
        let mut session = match Session::open_bytes(&package(&[held]), small, ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        let before = state(&session);
        // Apply sees the shipped library, 391 presets against a budget of 64.
        let failed = run(&mut session, &apply(SHIPPED_25, "seq-b"), 41).unwrap_err();
        assert_eq!(failed.code(), "budget_exceeded", "{failed:?}");
        assert_eq!(correlation_of(&failed.report), 41);
        assert_eq!(state(&session), before);
        // Set consults no preset, so the same budget lets it through.
        assert_eq!(run(&mut session, &set_mode("seq-a", SHIPPED_MODE), 42).unwrap().revision, 4);
    });
}

/// A commit authority whose answers the test sets.
#[derive(Clone, Default)]
struct Authority(Arc<Mutex<AuthorityState>>);
#[derive(Default)]
struct AuthorityState {
    answer_unknown: bool,
    wrong_binding: bool,
    receipt: Option<CommitReceipt>,
    committed_documents: Vec<Vec<u8>>,
}
impl CommitPort for Authority {
    fn commit(&self, request: &CommitRequest<'_>) -> CommitOutcome {
        let mut state = self.0.lock().unwrap();
        let binding = if state.wrong_binding { "someone-else".to_owned() } else { request.binding.to_owned() };
        let receipt = CommitReceipt { revision: request.successor_revision, sequence: 1, binding };
        state.receipt = Some(receipt.clone());
        state.committed_documents.push(request.successor_document.to_vec());
        if state.answer_unknown { CommitOutcome::Unknown } else { CommitOutcome::Committed(receipt) }
    }
    fn lookup(&self, _: &IdempotencyKey) -> Lookup {
        match &self.0.lock().unwrap().receipt {
            Some(receipt) => Lookup::Found(receipt.clone()),
            None => Lookup::Absent,
        }
    }
}
fn open_with_authority(bytes: &[u8], authority: &Authority) -> Session {
    let ports = ports(&Frames::default()).with_commits(Box::new(authority.clone()));
    match Session::open_bytes(bytes, SessionConfig::new(actor()), ports, &token()) {
        Ok(s) => s,
        Err(f) => panic!("{f:?}"),
    }
}

#[test]
fn a_commit_authority_gets_the_successor_and_entries_publish_with_it() {
    bounded_case("authority", || {
        let authority = Authority::default();
        let mut session = open_with_authority(&package(&[]), &authority);
        // A key is required, as for every submit under an authority.
        let ctx = context("sequence.preset.apply", 3, 900);
        assert_eq!(session.submit_sequence_command_as(&ctx, &apply(SHIPPED_25, "seq-b"), 3, &token()).unwrap_err().code(), "key_required");
        // An unknown outcome publishes nothing until the lookup adopts it, then everything at once.
        authority.0.lock().unwrap().answer_unknown = true;
        let key = IdempotencyKey::parse("apply-1").unwrap();
        let held = state(&session);
        assert_eq!(session.submit_sequence_command_keyed_as(&key, &ctx, &apply(SHIPPED_25, "seq-b"), 3, &token()).unwrap_err().code(), "outcome_unknown");
        assert_eq!(state(&session), held, "pending: neither the document nor the entry");
        // No direct write may slip under a pending commit.
        let direct = records(vec![settings_entry("seq-a", "custom", F25)]);
        assert_eq!(session.set_sequence_settings_records(Some(&direct), &token()).unwrap_err().code(), "reconciliation_required");
        let report = session.reconcile(&token()).unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(session.document_revision(), 4);
        assert_eq!(session.snapshot().document().sequences[1].frame_rate, SequenceFrameRate { ticks_per_frame: F25 });
        assert!(session.sequence_settings_records(&token()).unwrap().unwrap().entry("seq-b").is_some(), "the entry arrives with its successor");
        let documents = authority.0.lock().unwrap().committed_documents.len();
        assert_eq!(documents, 1, "the authority saw the successor once");
        // A retry of the key is the stored receipt.
        let replay = session.submit_sequence_command_keyed_as(&key, &ctx, &apply(SHIPPED_25, "seq-b"), 3, &token()).unwrap();
        assert!(replay.replayed && replay.revision == 4);
    });
}

#[test]
fn a_resync_cannot_leave_the_settings_entry_invalid() {
    bounded_case("resync", || {
        let authority = Authority::default();
        let held = entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap());
        let mut session = open_with_authority(&package(&[held]), &authority);
        authority.0.lock().unwrap().wrong_binding = true;
        let key = IdempotencyKey::parse("presets-1").unwrap();
        let presets = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) }));
        assert_eq!(session.submit_sequence_records_keyed(&key, &presets, 3, &token()).unwrap_err().code(), "receipt_mismatch");
        assert_eq!(session.document_entry(SEQUENCE_PRESETS_ENTRY), None, "a diverged commit publishes nothing");
        // The authority's document no longer holds the sequence the entry names.
        let mut document = session.snapshot().document().clone();
        document.revision = 4;
        document.sequences.remove(0);
        let bytes = serde_json::to_vec(&document).unwrap();
        assert_eq!(session.resync_diverged(&key, &bytes, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(session.document_revision(), 3, "nothing changed");
        // The operator can clear the entry while the key is pending, and then resync.
        session.set_sequence_settings_records(None, &token()).unwrap();
        session.resync_diverged(&key, &bytes, &token()).unwrap();
        assert_eq!(session.document_revision(), 4);
        let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(reopened.document_revision(), 4, "what the session writes reopens");

        // An entry already invalid does not hold a resync up; it stays as it came.
        let authority = Authority::default();
        let bad = serde_json::to_vec(&dangling_records()).unwrap();
        let mut session = open_with_authority(&package(&[entry(SEQUENCE_SETTINGS_ENTRY, &bad)]), &authority);
        authority.0.lock().unwrap().wrong_binding = true;
        assert_eq!(session.submit_sequence_records_keyed(&key, &presets, 3, &token()).unwrap_err().code(), "receipt_mismatch");
        session.resync_diverged(&key, &bytes, &token()).unwrap();
        assert_eq!(session.document_revision(), 4);
        assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(bad.as_slice()));
    });
}
