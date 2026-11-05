//! Lane SMB-PR-TRACK-DISPLAY-1, brief items 2 and 4 (STU-VID-036): the track display records persist
//! as the document entry `studio/track_display.<sha256>`, typed read and write, validated on read, and
//! commit through `SequenceRecordsStep` as one revision, one receipt, one inverse, one Observe frame.
//! Item 3 as the lead replaced it: display state is a view preference and never blocks a document edit
//! or an undo; an entry naming a sequence the document does not hold is inert.
mod sequence_records_support;
use hsk_studio_accord::{ActorContext, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use hsk_studio_package::{LossNote, LossReason, PackageError, Quarantined, sha256_hex_of};
use hsk_studio_session::*;
use sequence_records_support::*;
use serde_json::{Value, json};

fn folio_code(e: &SessionError) -> Option<Code> {
    match e {
        SessionError::Package(PackageError::Document(d)) => Some(d.code),
        _ => None,
    }
}

fn correlation_of(report: &ReportStatus) -> u64 {
    match report {
        ReportStatus::Delivered(receipt) => receipt.correlation_id,
        other => panic!("the frame was not delivered: {other:?}"),
    }
}

fn display_entry(sequence_id: &str) -> StudioTrackDisplayEntry {
    StudioTrackDisplayEntry {
        sequence_id: sequence_id.into(),
        switches: StudioTrackDisplaySwitches { video_thumbnails: StudioVideoThumbnails::HeadAndTail, fx_badges: false, ..StudioTrackDisplaySwitches::STUDIO_DEFAULT },
        heights: StudioTrackHeights { video: 60, audio: 70, caption: 80, submix: 90 },
        height_presets: vec![StudioTrackHeightPreset { preset_id: "tall".into(), name: "Tall tracks".into(), heights: StudioTrackHeights::all(300) }],
    }
}
fn display(entries: Vec<StudioTrackDisplayEntry>) -> StudioTrackDisplayRecords {
    let mut r = StudioTrackDisplayRecords::new(doc_id());
    r.sequences = entries;
    r
}
fn valid_display() -> Vec<u8> {
    serde_json::to_vec(&display(vec![display_entry("seq-a")])).unwrap()
}
fn display_json(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(&valid_display()).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}
fn display_step(edit: RecordEdit<StudioTrackDisplayRecords>) -> SequenceRecordsStep {
    SequenceRecordsStep { track_display: Some(edit), ..step(vec![], None, None) }
}
fn run_display(session: &mut Session, command: &StudioTrackDisplayCommand, correlation: u64) -> Result<Applied, Failed> {
    let revision = session.document_revision();
    session.submit_track_display_command_as(&context(command.command_id(), revision, correlation), command, revision, &token())
}
fn expand(sequence_id: &str) -> StudioTrackDisplayCommand {
    StudioTrackDisplayCommand::ExpandAll { sequence_id: sequence_id.into() }
}
fn the_entry(s: &Session) -> Option<Vec<u8>> {
    s.document_entry(TRACK_DISPLAY_ENTRY).map(<[u8]>::to_vec)
}

/// Every way an entry body can fail: what it is, its bytes, the Folio code a read refuses with.
fn invalid_cases() -> Vec<(&'static str, Vec<u8>, Code)> {
    let repeated = String::from_utf8(valid_display()).unwrap().replacen("{", &format!("{{\"document_id\":\"{}\",", doc_id()), 1);
    vec![
        ("another document", display_json(|v| v["document_id"] = json!(identity("SDOC", 2))), Code::InvalidField),
        ("the same sequence twice", display_json(|v| {
            let e = v["sequences"][0].clone();
            v["sequences"].as_array_mut().unwrap().push(e);
        }), Code::DuplicateIdentity),
        ("a height of zero", display_json(|v| v["sequences"][0]["heights"]["video"] = json!(0)), Code::InvalidField),
        ("a height past u32", display_json(|v| v["sequences"][0]["heights"]["video"] = json!(u64::from(u32::MAX) + 1)), Code::InvalidJson),
        ("a thumbnail mode the clause does not declare", display_json(|v| v["sequences"][0]["switches"]["video_thumbnails"] = json!("sometimes")), Code::InvalidJson),
        ("a switch missing", display_json(|v| drop(v["sequences"][0]["switches"].as_object_mut().unwrap().remove("clip_names"))), Code::InvalidJson),
        ("an unknown field", display_json(|v| v["vendor"] = json!(1)), Code::InvalidJson),
        ("a repeated key", repeated.into_bytes(), Code::DuplicateField),
        ("no JSON at all", b"{not json".to_vec(), Code::InvalidJson),
        ("nesting past the depth budget", "[".repeat(64).into_bytes(), Code::BudgetExceeded),
        ("bytes past the input limit", vec![b' '; MAX_TRACK_DISPLAY_INPUT_BYTES + 1], Code::BudgetExceeded),
        ("more presets than the declared count", display_json(|v| {
            let presets: Vec<Value> = (0..=MAX_TRACK_HEIGHT_PRESETS).map(|i| json!({"preset_id": format!("p{i}"), "name": "p", "heights": {"video": 50, "audio": 50, "caption": 50, "submix": 50}})).collect();
            v["sequences"][0]["height_presets"] = json!(presets);
        }), Code::BudgetExceeded),
    ]
}

#[test]
fn a_package_without_the_entry_opens_unchanged_and_resolves_to_the_declared_defaults() {
    bounded_case("no_entry", || {
        assert!(DOCUMENT_ENTRY_TOKENS.contains(&TRACK_DISPLAY_ENTRY) && TRACK_DISPLAY_ENTRY == "track_display");
        let (mut session, frames) = open_counted(&package(&[]));
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        assert!(!session.is_dirty());
        assert_eq!(session.track_display_records(&token()).unwrap(), None);
        let resolved = session.resolved_track_displays(&token()).unwrap();
        assert_eq!(resolved.iter().map(|r| r.sequence_id.as_str()).collect::<Vec<_>>(), ["seq-a", "seq-b"], "document order");
        assert!(resolved.iter().all(|r| !r.stored && r.switches == StudioTrackDisplaySwitches::STUDIO_DEFAULT && r.heights == StudioTrackHeights::STUDIO_DEFAULT));
        assert_eq!(frames.count(), 1, "the open; a typed read sends no frame");
        assert_eq!(the_entry(&open(&session.export_checkpoint(&token()).unwrap().bytes)), None, "a save creates no entry");
    });
}

#[test]
fn a_valid_entry_opens_typed_without_a_loss_note() {
    bounded_case("valid_entry", || {
        let body = valid_display();
        let session = open(&package(&[entry(TRACK_DISPLAY_ENTRY, &body)]));
        assert!(session.loss().is_empty(), "not an opaque entry any more: {:?}", session.loss());
        assert!(!session.is_dirty());
        assert_eq!(the_entry(&session), Some(body));
        assert_eq!(session.track_display_records(&token()).unwrap(), Some(display(vec![display_entry("seq-a")])));
        let resolved = session.resolved_track_displays(&token()).unwrap();
        assert!(resolved[0].stored);
        assert_eq!(resolved[0].switches, display_entry("seq-a").switches);
        assert_eq!(resolved[0].height_presets, display_entry("seq-a").height_presets);
        assert!(!resolved[1].stored);
        // Model-visible: the resolved state is plain JSON with the spec's names.
        let seen = serde_json::to_value(&resolved[0]).unwrap();
        assert_eq!(seen["switches"]["video_thumbnails"], "head_and_tail");
        assert_eq!(seen["heights"]["submix"], 90);
        assert_eq!(seen["stored"], true);
    });
}

#[test]
fn an_invalid_entry_opens_is_noted_refuses_on_read_and_save_keeps_its_bytes() {
    bounded_case("invalid_entry", || {
        for (what, body, code) in invalid_cases() {
            let held = entry(TRACK_DISPLAY_ENTRY, &body);
            let settings_body = serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap();
            let frames = Frames::default();
            let mut session = try_open(&package(&[held.clone(), entry(SEQUENCE_SETTINGS_ENTRY, &settings_body)]), &frames).unwrap_or_else(|f| panic!("{what}: a bad entry locked the file out: {f:?}"));
            assert_eq!(frames.count(), 1, "{what}: one Observe frame for the open");
            assert_eq!(session.loss(), [LossNote { name: held.name.clone(), reason: LossReason::Opaque }], "{what}");
            assert!(!session.is_dirty(), "{what}");
            let refused = session.track_display_records(&token()).unwrap_err();
            assert_eq!((folio_code(&refused), refused.code()), (Some(code), "document_rejected"), "{what}: {refused:?}");
            assert_eq!(folio_code(&session.resolved_track_displays(&token()).unwrap_err()), Some(code), "{what}: no guess at what the bad entry names");
            // A commit that does not use it goes through and leaves its bytes alone.
            run(&mut session, &set_mode("seq-a", SHIPPED_MODE), 1).unwrap_or_else(|f| panic!("{what}: {f:?}"));
            assert_eq!(the_entry(&session), Some(body.clone()), "{what}");
            // A command that reads it is refused and changes nothing.
            let revision = session.document_revision();
            let failed = run_display(&mut session, &expand("seq-a"), 2).unwrap_err();
            assert!(["folio_rejected", "budget_exceeded"].contains(&failed.code()), "{what}: {failed:?}");
            assert_eq!((session.document_revision(), the_entry(&session)), (revision, Some(body.clone())), "{what}");
            // Save keeps the bytes, so a reopen sees the same thing.
            let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert_eq!(the_entry(&reopened), Some(body.clone()), "{what}");
            assert_eq!(reopened.loss().len(), 1, "{what}");
            assert_eq!(folio_code(&reopened.track_display_records(&token()).unwrap_err()), Some(code), "{what}");
        }
    });
}

#[test]
fn a_repeated_entry_refuses_on_read_and_a_misnamed_one_stays_opaque() {
    bounded_case("repeated", || {
        let one = entry(TRACK_DISPLAY_ENTRY, &valid_display());
        let two = entry(TRACK_DISPLAY_ENTRY, &serde_json::to_vec(&display(vec![display_entry("seq-b")])).unwrap());
        let mut session = open(&package(&[one, two]));
        assert_eq!(folio_code(&session.track_display_records(&token()).unwrap_err()), Some(Code::DuplicateIdentity));
        assert_eq!(session.loss().len(), 2);
        let again = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(folio_code(&again.track_display_records(&token()).unwrap_err()), Some(Code::DuplicateIdentity));

        let body = valid_display();
        let misnamed = Quarantined { name: format!("{DOCUMENT_ENTRY_PREFIX}{TRACK_DISPLAY_ENTRY}.{}", "0".repeat(64)), sha256_hex: sha256_hex_of(&body), bytes: body };
        let session = open(&package(std::slice::from_ref(&misnamed)));
        assert_eq!(session.loss(), [LossNote { name: misnamed.name.clone(), reason: LossReason::Opaque }]);
        assert_eq!(session.track_display_records(&token()).unwrap(), None);
    });
}

#[test]
fn typed_writes_validate_and_refusals_change_nothing() {
    bounded_case("typed_writes", || {
        let mut session = open(&package(&[]));
        let good = display(vec![display_entry("seq-a")]);
        session.set_track_display_records(Some(&good), &token()).unwrap();
        assert!(session.is_dirty());
        assert_eq!(session.document_revision(), 3, "a direct set is not a revision, as every document entry");
        assert_eq!(session.track_display_records(&token()).unwrap(), Some(good.clone()));
        let held = the_entry(&session);

        let mut bad_height = display_entry("seq-a");
        bad_height.heights.video = 0;
        let bad_height = display(vec![bad_height]);
        assert_eq!(folio_code(&session.set_track_display_records(Some(&bad_height), &token()).unwrap_err()), Some(Code::InvalidField));
        assert_eq!(folio_code(&session.set_document_entry(TRACK_DISPLAY_ENTRY, Some(b"{not json".to_vec()), &token()).unwrap_err()), Some(Code::InvalidJson));
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.set_track_display_records(None, &canceled).unwrap_err().code(), "canceled");
        assert_eq!(session.track_display_records(&canceled).unwrap_err().code(), "canceled");
        assert_eq!(session.resolved_track_displays(&canceled).unwrap_err().code(), "canceled");
        assert_eq!(the_entry(&session), held, "every refusal leaves the entry untouched");

        session.set_track_display_records(None, &token()).unwrap();
        assert_eq!(the_entry(&open(&session.export_checkpoint(&token()).unwrap().bytes)), None, "a removed entry is not saved");

        // An explicit set is the way out of bytes a commit cannot quote; its note goes with them.
        let mut bad = open(&package(&[entry(TRACK_DISPLAY_ENTRY, b"{not json")]));
        assert_eq!(bad.loss().len(), 1);
        assert!(bad.set_track_display_records(Some(&bad_height), &token()).is_err());
        assert_eq!(bad.loss().len(), 1, "still there, so still noted");
        bad.set_track_display_records(Some(&good), &token()).unwrap();
        assert!(bad.loss().is_empty(), "{:?}", bad.loss());
        assert_eq!(bad.track_display_records(&token()).unwrap(), Some(good));
    });
}

#[test]
fn every_command_commits_one_revision_one_receipt_and_one_frame() {
    bounded_case("commands", || {
        let (mut session, frames) = open_counted(&package(&[]));
        let switches = StudioTrackDisplaySwitches { clip_markers: false, ..StudioTrackDisplaySwitches::STUDIO_DEFAULT };
        let commands = [
            StudioTrackDisplayCommand::SetSwitches { sequence_id: "seq-b".into(), switches },
            StudioTrackDisplayCommand::SetHeight { sequence_id: "seq-b".into(), kind: TrackKind::Audio, height: 111 },
            StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: "seq-b".into(), preset_id: "preset-private".into(), name: "Private name".into() },
            expand("seq-b"),
            StudioTrackDisplayCommand::MinimizeAll { sequence_id: "seq-b".into() },
            StudioTrackDisplayCommand::RecallHeightPreset { sequence_id: "seq-b".into(), preset_id: "preset-private".into() },
        ];
        for (i, command) in commands.iter().enumerate() {
            let correlation = 100 + i as u64;
            let revision = session.document_revision();
            let frames_before = frames.count();
            let receipts = session.receipt_count();
            let key = IdempotencyKey::parse(&format!("command-{i}")).unwrap();
            let ctx = context(command.command_id(), revision, correlation);
            let applied = session.submit_track_display_command_keyed_as(&key, &ctx, command, revision, &token()).unwrap();
            assert_eq!((applied.provider.as_str(), applied.command_id.as_str(), applied.correlation_id), (SEQUENCE_RECORDS_STEP_PROVIDER, command.command_id(), correlation));
            assert_eq!((applied.previous_revision, applied.revision, applied.changed), (revision, revision + 1, true), "one revision: {command:?}");
            assert_eq!(session.receipt_count(), receipts + 1, "one receipt");
            assert_eq!(applied.actor.principal_id, ctx.actor().principal_id(), "the caller's attribution");
            assert_eq!(correlation_of(&applied.report), correlation, "the frame carries the command's correlation");
            assert_eq!(frames.count(), frames_before + 1, "one frame");
            let report = applied.track_display_report.expect("a command reports");
            assert_eq!((report.command, report.base_revision, report.correlation, report.changed), (command.command_id(), revision, correlation, true));
        }
        let live = session.track_display_records(&token()).unwrap().unwrap();
        let held = live.entry("seq-b").unwrap();
        assert_eq!(held.switches, switches);
        assert_eq!(held.heights, StudioTrackHeights { audio: 111, ..StudioTrackHeights::STUDIO_DEFAULT }, "recalled over minimize-all");
        assert_eq!(held.height_presets.len(), 1);
        assert!(live.entry("seq-a").is_none(), "only the named sequence gains an entry");
        assert!(session.snapshot().document().sequences == vec![seq_a(), seq_b()], "the document's sequences do not change");

        // Codes and counters only: none of the command's text reaches the sink.
        let sent = String::from_utf8_lossy(&frames.bytes()).into_owned();
        for text in ["seq-b", "preset-private", "Private name", "track_display", "head_only"] {
            assert!(!sent.contains(text), "{text} in an Observe frame");
        }
    });
}

#[test]
fn the_as_form_of_the_step_and_the_command_answer_under_the_context() {
    bounded_case("as_forms", || {
        let (mut session, frames) = open_counted(&package(&[]));
        let write = display(vec![display_entry("seq-a")]);
        let s = display_step(RecordEdit { expect: None, write: Some(write.clone()) });
        let applied = session.submit_sequence_records_as(&context("cmd.test", 3, 77), &s, 3, &token()).unwrap();
        assert_eq!((applied.revision, correlation_of(&applied.report)), (4, 77));
        assert!(applied.track_display_report.is_none(), "a report is a command's");
        assert_eq!(session.track_display_records(&token()).unwrap(), Some(write));
        assert_eq!(frames.count(), 2);

        let good = expand("seq-a");
        let held = the_entry(&session);
        let refuse = |session: &mut Session, ctx: &CommandContext, rev: u64| session.submit_track_display_command_as(ctx, &good, rev, &token()).unwrap_err().code();
        assert_eq!(refuse(&mut session, &context("track_display.minimize_all", 4, 1), 4), "command_mismatch");
        let no_command = CommandContext::new(actor(), ClientSessionId::parse("w").unwrap(), None, 4, CorrelationId::new(2), None);
        assert_eq!(refuse(&mut session, &no_command, 4), "command_missing");
        assert_eq!(refuse(&mut session, &context("track_display.expand_all", 3, 3), 4), "stale_revision");
        assert_eq!(refuse(&mut session, &context("track_display.expand_all", 3, 4), 3), "stale_revision");
        let stranger = ActorContext::new("account-2", "principal-2", "owner", "owner-principal", "space", "session").unwrap();
        let foreign = CommandContext::new(stranger, ClientSessionId::parse("w").unwrap(), Some(CommandId::parse("track_display.expand_all").unwrap()), 4, CorrelationId::new(5), None);
        assert_eq!(refuse(&mut session, &foreign, 4), "actor_mismatch");
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.submit_track_display_command_as(&context("track_display.expand_all", 4, 6), &good, 4, &canceled).unwrap_err().code(), "canceled");
        assert_eq!((the_entry(&session), session.document_revision()), (held, 4), "refusals change nothing");
        assert_eq!(frames.count(), 2 + 6, "one frame per refused operation");
    });
}

#[test]
fn undo_and_redo_restore_the_exact_entry() {
    bounded_case("undo_redo", || {
        let mut session = open(&package(&[]));
        let commands = [expand("seq-a"), StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: "seq-a".into(), preset_id: "mine".into(), name: "Mine".into() }, StudioTrackDisplayCommand::MinimizeAll { sequence_id: "seq-a".into() }];
        for (i, command) in commands.iter().enumerate() {
            let before = the_entry(&session);
            let applied = run_display(&mut session, command, 200 + i as u64).unwrap();
            let after = the_entry(&session);
            assert_ne!(after, before, "{command:?} changed the entry");
            let revision = applied.revision;
            let undo = session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), revision, &token()).unwrap();
            assert_eq!((undo.previous_revision, undo.revision), (revision, revision + 1), "undo is one revision");
            assert_eq!(the_entry(&session), before, "undo of {command:?}");
            let ctx = context("cmd.redo", revision + 1, 300 + i as u64);
            let redo = session.submit_sequence_records_as(&ctx, &fresh_inverse(&undo, "cmd.redo"), revision + 1, &token()).unwrap();
            assert_eq!(redo.revision, revision + 2);
            assert_eq!(the_entry(&session), after, "redo of {command:?}");
        }
        // The first undo removes the entry it created: the sequence resolves to the defaults again.
        let mut fresh = open(&package(&[]));
        let applied = run_display(&mut fresh, &expand("seq-a"), 1).unwrap();
        fresh.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), applied.revision, &token()).unwrap();
        assert_eq!(the_entry(&fresh), None);
        assert!(!fresh.resolved_track_displays(&token()).unwrap()[0].stored);
        // An inverse is bound to what it undoes: replaying it conflicts.
        assert_eq!(fresh.submit_sequence_records(&fresh_inverse(&applied, "cmd.again"), applied.revision + 1, &token()).unwrap_err().code(), "value_conflict");
    });
}

#[test]
fn one_step_carries_the_entry_with_the_sequence_settings_and_presets() {
    bounded_case("one_step", || {
        let mut session = open(&package(&[]));
        let (settings_next, presets_next, display_next) = (records(vec![settings_entry("seq-a", "custom", F25)]), library(vec![preset("mine", F25)]), display(vec![display_entry("seq-a")]));
        let all = SequenceRecordsStep {
            track_display: Some(RecordEdit { expect: None, write: Some(display_next.clone()) }),
            ..step(vec![], Some(RecordEdit { expect: None, write: Some(settings_next.clone()) }), Some(RecordEdit { expect: None, write: Some(presets_next.clone()) }))
        };
        let receipts = session.receipt_count();
        let applied = session.submit_sequence_records_keyed(&IdempotencyKey::parse("all-1").unwrap(), &all, 3, &token()).unwrap();
        assert_eq!((applied.previous_revision, applied.revision), (3, 4), "one revision");
        assert_eq!(session.receipt_count(), receipts + 1, "one receipt");
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(settings_next));
        assert_eq!(session.sequence_presets(&token()).unwrap(), Some(presets_next));
        assert_eq!(session.track_display_records(&token()).unwrap(), Some(display_next.clone()));
        session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!((the_entry(&session), session.document_entry(SEQUENCE_SETTINGS_ENTRY), session.document_entry(SEQUENCE_PRESETS_ENTRY)), (None, None, None), "one undo for all three");

        // A step or a command that changes nothing: no revision, no inverse, no error, no entry.
        let noop = session.submit_sequence_records(&display_step(RecordEdit { expect: None, write: None }), 5, &token()).unwrap();
        assert!(!noop.changed && noop.revision == 5 && noop.sequence_records_inverse.is_none());
        let same = run_display(&mut session, &StudioTrackDisplayCommand::SetSwitches { sequence_id: "seq-a".into(), switches: StudioTrackDisplaySwitches::STUDIO_DEFAULT }, 9).unwrap();
        assert!(!same.changed && same.revision == 5 && same.sequence_records_inverse.is_none());
        assert_eq!(same.track_display_report.map(|r| r.changed), Some(false));
        assert_eq!(the_entry(&session), None);
    });
}

#[test]
fn keyed_retries_replay_the_receipt_and_a_changed_request_conflicts() {
    bounded_case("keyed", || {
        let mut session = open(&package(&[]));
        let key = IdempotencyKey::parse("cmd-1").unwrap();
        let ctx = context("track_display.expand_all", 3, 11);
        let first = session.submit_track_display_command_keyed_as(&key, &ctx, &expand("seq-b"), 3, &token()).unwrap();
        assert!(!first.replayed);
        let held = the_entry(&session);
        let retry = session.submit_track_display_command_keyed_as(&key, &ctx, &expand("seq-b"), 3, &token()).unwrap();
        assert!(retry.replayed && retry.revision == first.revision, "the stored receipt, computed once");
        assert_eq!((retry.sequence_records_inverse.clone(), retry.track_display_report), (first.sequence_records_inverse.clone(), first.track_display_report));
        assert_eq!((the_entry(&session), session.document_revision()), (held, 4), "nothing applied twice");
        let other = session.submit_track_display_command_keyed_as(&key, &ctx, &expand("seq-a"), 3, &token()).unwrap_err();
        assert_eq!(other.code(), "idempotency_conflict");
        // The step form under a key: its binding covers the entry it quotes.
        let write = display(vec![display_entry("seq-a")]);
        let step_key = IdempotencyKey::parse("step-1").unwrap();
        let live = session.track_display_records(&token()).unwrap();
        let s = display_step(RecordEdit { expect: live.clone(), write: Some(write.clone()) });
        assert_eq!(session.submit_sequence_records_keyed(&step_key, &s, 4, &token()).unwrap().revision, 5);
        assert!(session.submit_sequence_records_keyed(&step_key, &s, 4, &token()).unwrap().replayed);
        let changed = display_step(RecordEdit { expect: live, write: None });
        assert_eq!(session.submit_sequence_records_keyed(&step_key, &changed, 4, &token()).unwrap_err().code(), "idempotency_conflict");
    });
}

#[test]
fn step_and_command_refusals_leave_the_document_and_the_entry_untouched() {
    bounded_case("refusals", || {
        let (mut session, frames) = open_counted(&package(&[entry(TRACK_DISPLAY_ENTRY, &valid_display())]));
        let held = (the_entry(&session), session.document_revision());
        let receipts = session.receipt_count();
        let opened = frames.count();
        let mut ops = 0usize;
        for (command, code) in [
            (expand("seq-x"), "sequence_missing"),
            (StudioTrackDisplayCommand::SetHeight { sequence_id: "seq-a".into(), kind: TrackKind::Video, height: 0 }, "invalid_height"),
            (StudioTrackDisplayCommand::RecallHeightPreset { sequence_id: "seq-a".into(), preset_id: "none".into() }, "preset_missing"),
            (StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: "seq-a".into(), preset_id: "tall".into(), name: "Again".into() }, "duplicate_preset"),
            (StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: "seq-a".into(), preset_id: "a/b".into(), name: "Bad".into() }, "invalid_preset_id"),
            (StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: "seq-a".into(), preset_id: "ok".into(), name: " ".into() }, "invalid_name"),
        ] {
            let failed = run_display(&mut session, &command, 500).unwrap_err();
            assert_eq!(failed.code(), code, "{command:?}");
            assert_eq!(correlation_of(&failed.report), 500);
            ops += 1;
        }
        let live = session.track_display_records(&token()).unwrap();
        let write = Some(display(vec![display_entry("seq-b")]));
        let code = |session: &mut Session, s: &SequenceRecordsStep, rev: u64| session.submit_sequence_records(s, rev, &token()).unwrap_err().code();
        assert_eq!(code(&mut session, &display_step(RecordEdit { expect: None, write: write.clone() }), 3), "value_conflict", "the quote is not the live records");
        let mut over = display(vec![display_entry("seq-a")]);
        over.sequences[0].heights.video = 0;
        assert_eq!(code(&mut session, &display_step(RecordEdit { expect: live.clone(), write: Some(over) }), 3), "folio_rejected", "the records must validate");
        assert_eq!(code(&mut session, &display_step(RecordEdit { expect: live.clone(), write: write.clone() }), 2), "stale_revision");
        ops += 3;
        let mut bad_command = display_step(RecordEdit { expect: live.clone(), write: write.clone() });
        bad_command.command_id = String::new();
        assert_eq!(code(&mut session, &bad_command, 3), "invalid_command");
        ops += 1;
        assert_eq!((the_entry(&session), session.document_revision()), held, "refusals change nothing");
        assert_eq!(session.receipt_count(), receipts);
        assert_eq!(frames.count(), opened + ops, "one frame per refused operation");

        // A body a reopen would refuse is refused now: the most presets one sequence may hold, each
        // with the longest name, is more than this budget's input bytes.
        let mut small = SessionConfig::new(actor());
        small.budgets.chronicle.folio.input_bytes = 8 * 1024;
        let mut small = match Session::open_bytes(&package(&[]), small, ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        let mut heavy = display_entry("seq-a");
        heavy.height_presets = (0..MAX_TRACK_HEIGHT_PRESETS).map(|i| StudioTrackHeightPreset { preset_id: format!("p{i}"), name: "n".repeat(128), heights: StudioTrackHeights::all(100) }).collect();
        let heavy = display(vec![heavy]);
        assert_eq!(small.submit_sequence_records(&display_step(RecordEdit { expect: None, write: Some(heavy.clone()) }), 3, &token()).unwrap_err().code(), "budget_exceeded");
        assert_eq!(the_entry(&small), None);

        // The package limit binds this entry with every other.
        let body_len = serde_json::to_vec(&heavy).unwrap().len() as u64;
        let mut limited = SessionConfig::new(actor());
        limited.budgets.limits.max_entry_bytes = body_len - 1;
        let mut tight = match Session::open_bytes(&package(&[]), limited, ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert_eq!(tight.submit_sequence_records(&display_step(RecordEdit { expect: None, write: Some(heavy.clone()) }), 3, &token()).unwrap_err().code(), "total_too_large");
        assert_eq!(tight.set_track_display_records(Some(&heavy), &token()).unwrap_err().code(), "total_too_large");
        assert_eq!(the_entry(&tight), None);
    });
}

#[test]
fn a_step_replaces_an_entry_it_can_quote_and_refuses_bytes_it_cannot() {
    bounded_case("replace_invalid", || {
        // Parsable but invalid: a step can quote it and write valid records, and the note goes with it.
        let mut stale_entry = display_entry("seq-a");
        stale_entry.heights.video = 0;
        let stale = display(vec![stale_entry]);
        let mut session = open(&package(&[entry(TRACK_DISPLAY_ENTRY, &serde_json::to_vec(&stale).unwrap())]));
        assert_eq!(session.loss().len(), 1);
        let keep = SequenceRecordsStep {
            track_display: Some(RecordEdit { expect: Some(stale.clone()), write: Some(stale.clone()) }),
            ..step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![])) }))
        };
        assert_eq!(session.submit_sequence_records(&keep, 3, &token()).unwrap_err().code(), "folio_rejected", "keeping it while anything else changes is using it");
        let good = display(vec![display_entry("seq-a")]);
        let replace = display_step(RecordEdit { expect: Some(stale.clone()), write: Some(good.clone()) });
        let applied = session.submit_sequence_records(&replace, 3, &token()).unwrap();
        assert_eq!(session.track_display_records(&token()).unwrap(), Some(good));
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        // The undo would write the invalid records back, so it is refused and the repair stands.
        assert_eq!(session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap_err().code(), "folio_rejected");
        // Bytes that do not parse cannot be quoted, a `None` quote included.
        let mut corrupt = open(&package(&[entry(TRACK_DISPLAY_ENTRY, b"{not json")]));
        let any = display_step(RecordEdit { expect: None, write: None });
        assert_eq!(corrupt.submit_sequence_records(&any, 3, &token()).unwrap_err().code(), "value_conflict");
        assert_eq!(the_entry(&corrupt), Some(b"{not json".to_vec()));
    });
}

/// `seq-c` holding one clip, and `seq-b` with its clip nesting `seq-c`.
fn nested_pair() -> (StudioSequence, StudioSequence) {
    let mut inner = seq_b().tracks[0].clips[0].clone();
    inner.clip_id = identity("SCLP", 9);
    let source = sequence("seq-c", F24, vec![inner]);
    let mut parent = seq_b();
    parent.tracks[0].clips[0].source_ref = ClipSource::Sequence { sequence_id: "seq-c".into() };
    (source, parent)
}
fn renamed(applied: &Applied, command: &str) -> MulticamStep {
    let mut inverse = applied.multicam_inverse.clone().expect("a changed step has an inverse");
    inverse.command_id = command.into();
    inverse
}
fn sequence_ids(session: &Session) -> Vec<String> {
    session.resolved_track_displays(&token()).unwrap().into_iter().map(|r| r.sequence_id).collect()
}
fn inert_ids(session: &Session) -> Vec<String> {
    session.inert_track_display_entries(&token()).unwrap().into_iter().map(|e| e.sequence_id).collect()
}

/// The lead's case: a display toggle on a nested sequence, the undo of the nest, the display state back.
#[test]
fn a_display_toggle_on_a_nested_sequence_does_not_block_undo_and_comes_back_with_it() {
    bounded_case("nest_undo", || {
        let mut session = open(&package(&[]));
        let (source, parent) = nested_pair();
        let nest = MulticamStep {
            command_id: "sequence.nest".into(),
            correlation_id: 1,
            sequences: vec![SequenceEdit::Insert { at: 2, write: source }, SequenceEdit::Replace { expect: seq_b(), write: parent }],
            records_expect: None,
            records_write: None,
        };
        let nested = session.submit_multicam(&nest, 3, &token()).unwrap();
        assert_eq!(sequence_ids(&session), ["seq-a", "seq-b", "seq-c"]);
        run_display(&mut session, &expand("seq-c"), 2).unwrap();
        let held = the_entry(&session).expect("the toggle made an entry");

        // The undo of the nest removes the sequence, commits normally and leaves the entry alone.
        let undone = session.submit_multicam(&renamed(&nested, "cmd.undo"), 5, &token()).unwrap();
        assert_eq!((undone.previous_revision, undone.revision), (5, 6), "one revision");
        assert_eq!(session.snapshot().document().sequences, vec![seq_a(), seq_b()], "the document as before the nest");
        assert_eq!(the_entry(&session).as_ref(), Some(&held), "the entry is not touched");
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        assert_eq!(sequence_ids(&session), ["seq-a", "seq-b"], "the document's sequences only");
        assert_eq!(inert_ids(&session), ["seq-c"]);

        // The redo brings the sequence back, and its display state with it, with no extra work.
        session.submit_multicam(&renamed(&undone, "cmd.redo"), 6, &token()).unwrap();
        assert_eq!(the_entry(&session).as_ref(), Some(&held));
        assert!(inert_ids(&session).is_empty());
        let back = session.resolved_track_displays(&token()).unwrap().into_iter().find(|r| r.sequence_id == "seq-c").unwrap();
        assert!(back.stored);
        assert_eq!(back.heights, StudioTrackHeights::all(EXPANDED_TRACK_HEIGHT));
    });
}

#[test]
fn a_records_step_removes_a_sequence_without_touching_the_entry_and_its_undo_brings_it_back() {
    bounded_case("records_removal", || {
        let body = valid_display();
        let mut session = open(&package(&[entry(TRACK_DISPLAY_ENTRY, &body)]));
        let removed = session.submit_sequence_records(&step(vec![SequenceEdit::Remove { expect: seq_a() }], None, None), 3, &token()).unwrap();
        assert_eq!(removed.revision, 4, "one revision");
        assert_eq!(the_entry(&session), Some(body.clone()));
        assert_eq!(inert_ids(&session), ["seq-a"]);
        assert_eq!(sequence_ids(&session), ["seq-b"]);
        session.submit_sequence_records(&fresh_inverse(&removed, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!(the_entry(&session), Some(body));
        assert!(inert_ids(&session).is_empty());
        assert!(session.resolved_track_displays(&token()).unwrap()[0].stored);

        // A timeline step edits a sequence and cannot remove one; it commits with an entry present.
        let mut named = seq_a();
        named.name = "renamed".into();
        let rename = TimelineStep { command_id: "cmd.rename".into(), correlation_id: 2, sequence_id: "seq-a".into(), expect: seq_a(), write: named };
        assert_eq!(session.submit_timeline(&rename, 5, &token()).unwrap().revision, 6);
    });
}

#[test]
fn an_inert_entry_survives_save_and_reopen_byte_identically() {
    bounded_case("inert_save", || {
        let held = display(vec![display_entry("seq-y"), display_entry("seq-a"), display_entry("seq-x")]);
        let body = serde_json::to_vec(&held).unwrap();
        let mut session = open(&package(&[entry(TRACK_DISPLAY_ENTRY, &body)]));
        assert!(session.loss().is_empty(), "inert is not invalid: {:?}", session.loss());
        assert_eq!(session.track_display_records(&token()).unwrap(), Some(held.clone()), "not refused on read");
        assert_eq!(inert_ids(&session), ["seq-y", "seq-x"], "stable order");
        assert_eq!(session.inert_track_display_entries(&token()).unwrap()[1], display_entry("seq-x"));

        let dir = std::env::temp_dir().join(format!("hsk-studio-session-track-display-inert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.handshake");
        session.save_to(&path, &token()).unwrap();
        let reopened = match Session::open_path(&path, SessionConfig::new(actor()), ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert_eq!(the_entry(&reopened), Some(body.clone()), "byte-identical through a file");
        assert!(reopened.loss().is_empty());
        assert_eq!(inert_ids(&reopened), ["seq-y", "seq-x"]);
        let checkpoint = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(the_entry(&checkpoint), Some(body.clone()), "byte-identical through a checkpoint");
        drop(reopened);
        std::fs::remove_dir_all(&dir).unwrap();

        // Nothing removes one on its own: commits and a save leave it. A records step can, explicitly.
        session.submit_sequence_records(&step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) })), 3, &token()).unwrap();
        run_display(&mut session, &expand("seq-b"), 2).unwrap();
        assert_eq!(inert_ids(&session), ["seq-y", "seq-x"]);
        let live = session.track_display_records(&token()).unwrap();
        let mut without = live.clone().unwrap();
        without.sequences.retain(|e| e.sequence_id != "seq-y");
        let revision = session.document_revision();
        let removed = session.submit_sequence_records(&display_step(RecordEdit { expect: live.clone(), write: Some(without) }), revision, &token()).unwrap();
        assert_eq!(removed.revision, revision + 1);
        assert_eq!(inert_ids(&session), ["seq-x"]);
        session.submit_sequence_records(&fresh_inverse(&removed, "cmd.undo"), revision + 1, &token()).unwrap();
        assert_eq!(session.track_display_records(&token()).unwrap(), live, "and an undo restores it");
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.inert_track_display_entries(&canceled).unwrap_err().code(), "canceled");
    });
}

#[test]
fn the_list_refuses_while_the_entry_is_invalid_and_the_limit_refuses_typed() {
    bounded_case("inert_limit", || {
        let bad = open(&package(&[entry(TRACK_DISPLAY_ENTRY, b"{not json")]));
        assert_eq!(folio_code(&bad.inert_track_display_entries(&token()).unwrap_err()), Some(Code::InvalidJson));

        let plain = |id: String| { let mut e = display_entry("seq-a"); e.sequence_id = id; e.height_presets.clear(); e };
        let full = display((0..MAX_TRACK_DISPLAY_ENTRIES).map(|i| plain(format!("gone-{i}"))).collect());
        let (mut session, frames) = open_counted(&package(&[entry(TRACK_DISPLAY_ENTRY, &serde_json::to_vec(&full).unwrap())]));
        assert!(session.loss().is_empty());
        let held = the_entry(&session);
        let opened = frames.count();
        let failed = run_display(&mut session, &expand("seq-a"), 9).unwrap_err();
        assert_eq!((failed.code(), correlation_of(&failed.report)), ("budget_exceeded", 9), "a new entry at the limit");
        let mut over = full.clone();
        over.sequences.push(plain("seq-a".into()));
        assert_eq!(session.submit_sequence_records(&display_step(RecordEdit { expect: Some(full.clone()), write: Some(over) }), 3, &token()).unwrap_err().code(), "budget_exceeded");
        assert_eq!((the_entry(&session), session.document_revision(), frames.count()), (held, 3, opened + 2), "the refusals change nothing");
        // Removing an inert entry makes room.
        let mut fewer = full.clone();
        fewer.sequences.remove(0);
        assert_eq!(session.submit_sequence_records(&display_step(RecordEdit { expect: Some(full), write: Some(fewer) }), 3, &token()).unwrap().revision, 4);
        assert_eq!(run_display(&mut session, &expand("seq-a"), 10).unwrap().revision, 5);
    });
}

/// Settings and multicam keep their rule; only the display entry was released from it.
#[test]
fn a_removal_is_held_by_the_settings_entry_and_not_by_the_display_entry() {
    bounded_case("both_entries", || {
        let settings_held = records(vec![settings_entry("seq-a", "custom", F25)]);
        let display_body = valid_display();
        let mut session = open(&package(&[entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&settings_held).unwrap()), entry(TRACK_DISPLAY_ENTRY, &display_body)]));
        let remove_a = || SequenceEdit::Remove { expect: seq_a() };
        let multicam = MulticamStep { command_id: "cmd.remove".into(), correlation_id: 1, sequences: vec![remove_a()], records_expect: None, records_write: None };
        assert_eq!(session.submit_multicam(&multicam, 3, &token()).unwrap_err().error.code(), "folio_rejected", "the settings entry names it");
        assert_eq!(session.submit_sequence_records(&step(vec![remove_a()], None, None), 3, &token()).unwrap_err().code(), "folio_rejected");
        let settings_gone = step(vec![remove_a()], Some(RecordEdit { expect: Some(settings_held.clone()), write: None }), None);
        assert_eq!(session.submit_sequence_records(&settings_gone, 3, &token()).unwrap().revision, 4);
        assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY), None);
        assert_eq!(the_entry(&session), Some(display_body), "the display entry stays");
        assert_eq!(inert_ids(&session), ["seq-a"]);
    });
}

#[test]
fn an_entry_already_invalid_does_not_hold_a_removal_up_and_a_new_sequence_resolves_to_the_defaults() {
    bounded_case("not_judged", || {
        let mut bad = display_entry("seq-a");
        bad.heights.video = 0;
        let stale = serde_json::to_vec(&display(vec![bad])).unwrap();
        let mut session = open(&package(&[entry(TRACK_DISPLAY_ENTRY, &stale)]));
        let free = MulticamStep { command_id: "cmd.remove".into(), correlation_id: 1, sequences: vec![SequenceEdit::Remove { expect: seq_b() }], records_expect: None, records_write: None };
        assert_eq!(session.submit_multicam(&free, 3, &token()).unwrap().revision, 4);
        assert_eq!(the_entry(&session), Some(stale));

        // F-R55-4(c) does not come back: a created sequence gets no entry and is not a gap.
        let mut session = open(&package(&[entry(TRACK_DISPLAY_ENTRY, &valid_display())]));
        let created = sequence("seq-c", F25, vec![]);
        let insert = MulticamStep { command_id: "cmd.create".into(), correlation_id: 1, sequences: vec![SequenceEdit::Insert { at: 2, write: created }], records_expect: None, records_write: None };
        assert_eq!(session.submit_multicam(&insert, 3, &token()).unwrap().revision, 4);
        let resolved = session.resolved_track_displays(&token()).unwrap();
        assert_eq!(resolved.iter().map(|r| (r.sequence_id.as_str(), r.stored)).collect::<Vec<_>>(), [("seq-a", true), ("seq-b", false), ("seq-c", false)]);
        assert_eq!(resolved[2].heights, StudioTrackHeights::STUDIO_DEFAULT);
    });
}

/// Brief item 4: everything authored survives save and reopen through the session, byte-identical.
#[test]
fn authored_display_state_survives_save_and_reopen_through_the_session() {
    bounded_case("save_reopen", || {
        let dir = std::env::temp_dir().join(format!("hsk-studio-session-track-display-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.handshake");
        let mut session = open(&package(&[]));
        let switches = StudioTrackDisplaySwitches {
            video_thumbnails: StudioVideoThumbnails::Continuous,
            audio_waveforms: StudioAudioWaveforms { shown: true, scaling: StudioWaveformScaling::Dynamic, rectified: true },
            content_credential_badges: false,
            through_edits: false,
            ..StudioTrackDisplaySwitches::STUDIO_DEFAULT
        };
        let commands = [
            StudioTrackDisplayCommand::SetSwitches { sequence_id: "seq-a".into(), switches },
            StudioTrackDisplayCommand::SetHeight { sequence_id: "seq-a".into(), kind: TrackKind::Submix, height: 123 },
            StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: "seq-a".into(), preset_id: "mine".into(), name: "Mine".into() },
            StudioTrackDisplayCommand::MinimizeAll { sequence_id: "seq-b".into() },
        ];
        let steps: Vec<Applied> = commands.iter().enumerate().map(|(i, c)| run_display(&mut session, c, i as u64).unwrap()).collect();
        assert_eq!(session.document_revision(), 7);
        let expect = |s: &Session| {
            let live = s.track_display_records(&token()).unwrap().unwrap();
            let a = live.entry("seq-a").unwrap();
            assert_eq!(a.switches, switches);
            assert_eq!(a.heights, StudioTrackHeights { submix: 123, ..StudioTrackHeights::STUDIO_DEFAULT });
            assert_eq!(a.height_presets, vec![StudioTrackHeightPreset { preset_id: "mine".into(), name: "Mine".into(), heights: a.heights }]);
            assert_eq!(live.entry("seq-b").unwrap().heights, StudioTrackHeights::all(MIN_TRACK_HEIGHT));
        };
        expect(&session);
        let live = the_entry(&session);
        session.save_to(&path, &token()).unwrap();
        assert!(!session.is_dirty(), "the save carried everything");

        let mut reopened = match Session::open_path(&path, SessionConfig::new(actor()), ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        assert_eq!(the_entry(&reopened), live, "byte-identical");
        assert_eq!(reopened.document_revision(), 7);
        expect(&reopened);

        // A checkpoint round trips too, and a later save keeps the bytes.
        let from_checkpoint = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(the_entry(&from_checkpoint), live);
        expect(&from_checkpoint);

        // The inverses taken before the save still undo on the reopened document, newest first.
        let mut revision = reopened.document_revision();
        for applied in steps.iter().rev() {
            revision = reopened.submit_sequence_records(&fresh_inverse(applied, "cmd.undo"), revision, &token()).unwrap().revision;
        }
        assert_eq!(the_entry(&reopened), None, "back to the start");
        reopened.save_to(&path, &token()).unwrap();
        let again = match Session::open_path(&path, SessionConfig::new(actor()), ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert_eq!(the_entry(&again), None, "an undone entry is not saved");
        drop(again);
        drop(reopened);
        drop(session);
        std::fs::remove_dir_all(&dir).unwrap();
    });
}

/// MT-33739's own case. Two sequences hold different display states; every thumbnail mode, rectified
/// on and off with both scales, each switch alone, then named video and audio heights saved and
/// recalled around expand-all and minimize-all, each state through a reopen. The oracle is the entry's
/// JSON against values written out here, and the sequences against the fixture's: no clip timing moves.
/// The fixture's clip holds no sample data, so "no samples change" is proven only as far as the clip's
/// fields go.
#[test]
fn two_sequences_keep_independent_display_state_through_reopen_mt_33739() {
    bounded_case("mt_case", || {
        fn reopened(session: &mut Session) -> Session {
            let again = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert!(again.loss().is_empty(), "{:?}", again.loss());
            assert_eq!(again.snapshot().document().sequences, vec![seq_a(), seq_b()], "no clip timing changes");
            again
        }
        fn entry_json(session: &Session, sequence_id: &str) -> Value {
            let all: Value = serde_json::from_slice(session.document_entry(TRACK_DISPLAY_ENTRY).unwrap()).unwrap();
            all["sequences"].as_array().unwrap().iter().find(|e| e["sequence_id"] == sequence_id).unwrap().clone()
        }
        let set = |sequence_id: &str, switches| StudioTrackDisplayCommand::SetSwitches { sequence_id: sequence_id.into(), switches };
        let mut session = open(&package(&[]));

        let modes = [("off", StudioVideoThumbnails::Off), ("head_only", StudioVideoThumbnails::HeadOnly), ("head_and_tail", StudioVideoThumbnails::HeadAndTail), ("continuous", StudioVideoThumbnails::Continuous)];
        for (i, (wire, mode)) in modes.iter().enumerate() {
            let (other_wire, other_mode) = modes[(i + 1) % modes.len()];
            for (scale_wire, scaling) in [("logarithmic", StudioWaveformScaling::Logarithmic), ("dynamic", StudioWaveformScaling::Dynamic)] {
                for rectified in [false, true] {
                    let audio_waveforms = StudioAudioWaveforms { shown: true, scaling, rectified };
                    let a = StudioTrackDisplaySwitches { video_thumbnails: *mode, audio_waveforms, ..StudioTrackDisplaySwitches::STUDIO_DEFAULT };
                    let b = StudioTrackDisplaySwitches { video_thumbnails: other_mode, audio_names: false, ..StudioTrackDisplaySwitches::STUDIO_DEFAULT };
                    run_display(&mut session, &set("seq-a", a), 1).unwrap();
                    run_display(&mut session, &set("seq-b", b), 2).unwrap();
                    let again = reopened(&mut session);
                    let (seen_a, seen_b) = (entry_json(&again, "seq-a"), entry_json(&again, "seq-b"));
                    assert_eq!(seen_a["switches"]["video_thumbnails"], *wire);
                    assert_eq!(seen_a["switches"]["audio_waveforms"], json!({"shown": true, "scaling": scale_wire, "rectified": rectified}));
                    assert_eq!(seen_a["switches"]["audio_names"], true);
                    assert_eq!(seen_b["switches"]["video_thumbnails"], other_wire);
                    assert_eq!(seen_b["switches"]["audio_waveforms"], json!({"shown": true, "scaling": "logarithmic", "rectified": false}));
                    assert_eq!(seen_b["switches"]["audio_names"], false);
                }
            }
        }

        // Each on/off switch alone: turning one off changes that key and no other.
        let defaults = serde_json::to_value(StudioTrackDisplaySwitches::STUDIO_DEFAULT).unwrap();
        let flags: Vec<String> = defaults.as_object().unwrap().iter().filter(|(_, v)| v.is_boolean()).map(|(k, _)| k.clone()).collect();
        assert_eq!(flags.len(), 16, "every declared on/off switch");
        for flag in &flags {
            let mut wire = defaults.clone();
            wire[flag.as_str()] = json!(false);
            let switches: StudioTrackDisplaySwitches = serde_json::from_value(wire.clone()).unwrap();
            run_display(&mut session, &set("seq-a", switches), 3).unwrap();
            let again = reopened(&mut session);
            assert_eq!(entry_json(&again, "seq-a")["switches"], wire, "{flag}");
            assert_eq!(entry_json(&again, "seq-b")["switches"]["audio_names"], false, "the other sequence is not touched");
        }

        // Named video and audio heights, saved and recalled around expand-all and minimize-all.
        for (kind, height) in [(TrackKind::Video, 111), (TrackKind::Audio, 222)] {
            run_display(&mut session, &StudioTrackDisplayCommand::SetHeight { sequence_id: "seq-a".into(), kind, height }, 4).unwrap();
        }
        let named = json!({"video": 111, "audio": 222, "caption": 48, "submix": 48});
        run_display(&mut session, &StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: "seq-a".into(), preset_id: "edit".into(), name: "Edit".into() }, 5).unwrap();
        run_display(&mut session, &expand("seq-a"), 6).unwrap();
        let again = reopened(&mut session);
        assert_eq!(entry_json(&again, "seq-a")["heights"], json!({"video": 200, "audio": 200, "caption": 200, "submix": 200}));
        assert_eq!(entry_json(&again, "seq-a")["height_presets"], json!([{"preset_id": "edit", "name": "Edit", "heights": named}]));
        run_display(&mut session, &StudioTrackDisplayCommand::RecallHeightPreset { sequence_id: "seq-a".into(), preset_id: "edit".into() }, 7).unwrap();
        assert_eq!(entry_json(&reopened(&mut session), "seq-a")["heights"], named);
        run_display(&mut session, &StudioTrackDisplayCommand::MinimizeAll { sequence_id: "seq-a".into() }, 8).unwrap();
        let again = reopened(&mut session);
        assert_eq!(entry_json(&again, "seq-a")["heights"], json!({"video": 16, "audio": 16, "caption": 16, "submix": 16}));
        assert_eq!(entry_json(&again, "seq-b")["heights"], json!({"video": 48, "audio": 48, "caption": 48, "submix": 48}), "the other sequence keeps its heights");
        assert!(entry_json(&again, "seq-b").get("height_presets").is_none(), "and has no preset");
    });
}
