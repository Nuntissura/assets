//! Lane SMB-PR-MEDIA-MARKERS-1, brief items 3, 5 and 6 (STU-VID-034): the media item markers persist as
//! the document entry `studio/media_markers.<sha256>`, typed read and write, validated on read, and
//! commit through `SequenceRecordsStep` as one revision, one receipt, one inverse, one Observe frame.
//! The entry names no sequence or clip, so removing the last clip cut from an item leaves it valid and
//! unchanged; a document marker may not take an id it holds.
mod sequence_records_support;
use hsk_studio_accord::{ActorContext, ClientSessionId, CommandContext, CommandId, CorrelationId, Ticks};
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

fn digest(name: &str) -> ContentDigest {
    ContentDigest { algorithm: "sha256".into(), digest: name.into() }
}
/// The digest of `seq_b()`'s one clip.
fn cut_item() -> ContentDigest {
    digest("ab0001")
}
fn marker(id: &str, frame: u64) -> StudioMarker {
    StudioMarker {
        marker_id: id.into(),
        name: format!("Private name of {id}"),
        comment: "Private comment".into(),
        color: LabelColor::Mango,
        start: Ticks::new(frame * F24),
        duration: Ticks::new(F24),
        role: Some(MarkerRole::Chapter),
    }
}
fn markers(items: Vec<(ContentDigest, Vec<StudioMarker>)>) -> StudioMediaMarkerRecords {
    let mut r = StudioMediaMarkerRecords::new(doc_id());
    r.media_items = items.into_iter().map(|(content_digest, markers)| StudioMediaMarkerEntry { content_digest, markers }).collect();
    r
}
fn held_records() -> StudioMediaMarkerRecords {
    markers(vec![(cut_item(), vec![marker("m-1", 3)])])
}
fn valid_body() -> Vec<u8> {
    serde_json::to_vec(&held_records()).unwrap()
}
fn body_edited(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(&valid_body()).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}
fn marker_step(edit: RecordEdit<StudioMediaMarkerRecords>) -> SequenceRecordsStep {
    SequenceRecordsStep { media_markers: Some(edit), ..step(vec![], None, None) }
}
fn run_marker(session: &mut Session, command: &StudioMediaMarkerCommand, correlation: u64) -> Result<Applied, Failed> {
    let revision = session.document_revision();
    session.submit_media_marker_command_as(&context(command.command_id(), revision, correlation), command, revision, &token())
}
fn add(content_digest: ContentDigest, m: StudioMarker) -> StudioMediaMarkerCommand {
    StudioMediaMarkerCommand::Add { content_digest, marker: m }
}
fn remove(content_digest: ContentDigest, id: &str) -> StudioMediaMarkerCommand {
    StudioMediaMarkerCommand::Remove { content_digest, marker_id: id.into() }
}
fn the_entry(s: &Session) -> Option<Vec<u8>> {
    s.document_entry(MEDIA_MARKERS_ENTRY).map(<[u8]>::to_vec)
}
fn timeline_writing(expect: StudioSequence, write: StudioSequence) -> TimelineStep {
    TimelineStep { command_id: "cmd.edit".into(), correlation_id: 2, sequence_id: expect.sequence_id.clone(), expect, write }
}
fn removing_sequence(sequence: StudioSequence) -> MulticamStep {
    MulticamStep { command_id: "cmd.remove".into(), correlation_id: 1, sequences: vec![SequenceEdit::Remove { expect: sequence }], records_expect: None, records_write: None }
}

/// Every way an entry body can fail: what it is, its bytes, the Folio code a read refuses with.
fn invalid_cases() -> Vec<(&'static str, Vec<u8>, Code)> {
    let repeated = String::from_utf8(valid_body()).unwrap().replacen('{', &format!("{{\"document_id\":\"{}\",", doc_id()), 1);
    vec![
        ("another document", body_edited(|v| v["document_id"] = json!(identity("SDOC", 2))), Code::InvalidField),
        ("the same media item twice", body_edited(|v| {
            let mut e = v["media_items"][0].clone();
            e["markers"][0]["marker_id"] = json!("m-2");
            v["media_items"].as_array_mut().unwrap().push(e);
        }), Code::DuplicateIdentity),
        ("a marker id repeated", body_edited(|v| {
            let m = v["media_items"][0]["markers"][0].clone();
            v["media_items"][0]["markers"].as_array_mut().unwrap().push(m);
        }), Code::DuplicateIdentity),
        ("an item with no marker", body_edited(|v| v["media_items"][0]["markers"] = json!([])), Code::InvalidField),
        ("a digest with a control character", body_edited(|v| v["media_items"][0]["content_digest"]["digest"] = json!("a\u{1}")), Code::InvalidField),
        ("a marker id off the grammar", body_edited(|v| v["media_items"][0]["markers"][0]["marker_id"] = json!("a/b")), Code::InvalidId),
        ("a span past the last tick", body_edited(|v| {
            v["media_items"][0]["markers"][0]["start"] = json!(u64::MAX);
            v["media_items"][0]["markers"][0]["duration"] = json!(1);
        }), Code::InvalidField),
        ("a tick that is not one", body_edited(|v| v["media_items"][0]["markers"][0]["start"] = json!(-1)), Code::InvalidJson),
        ("a colour the clause's set does not hold", body_edited(|v| v["media_items"][0]["markers"][0]["color"] = json!("plaid")), Code::InvalidJson),
        ("a role field missing", body_edited(|v| drop(v["media_items"][0]["markers"][0].as_object_mut().unwrap().remove("role"))), Code::InvalidJson),
        ("an unknown field", body_edited(|v| v["vendor"] = json!(1)), Code::InvalidJson),
        ("a repeated key", repeated.into_bytes(), Code::DuplicateField),
        ("no JSON at all", b"{not json".to_vec(), Code::InvalidJson),
        ("nesting past the depth budget", "[".repeat(64).into_bytes(), Code::BudgetExceeded),
        ("bytes past the input limit", vec![b' '; MAX_MEDIA_MARKER_INPUT_BYTES + 1], Code::BudgetExceeded),
        ("more markers than the budget counts", body_edited(|v| {
            let many: Vec<Value> = (0..5000).map(|i| json!({"marker_id": format!("m{i}"), "name": "", "comment": "", "color": "mango", "start": 0, "duration": 0, "role": null})).collect();
            v["media_items"][0]["markers"] = json!(many);
        }), Code::BudgetExceeded),
    ]
}

#[test]
fn a_package_without_the_entry_opens_unchanged() {
    bounded_case("no_entry", || {
        assert!(DOCUMENT_ENTRY_TOKENS.contains(&MEDIA_MARKERS_ENTRY) && MEDIA_MARKERS_ENTRY == "media_markers");
        let (mut session, frames) = open_counted(&package(&[]));
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        assert!(!session.is_dirty());
        assert_eq!(session.media_marker_records(&token()).unwrap(), None);
        assert_eq!(frames.count(), 1, "the open; a typed read sends no frame");
        assert_eq!(the_entry(&open(&session.export_checkpoint(&token()).unwrap().bytes)), None, "a save creates no entry");
    });
}

#[test]
fn a_valid_entry_opens_typed_without_a_loss_note() {
    bounded_case("valid_entry", || {
        let body = valid_body();
        let session = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &body)]));
        assert!(session.loss().is_empty(), "not an opaque entry any more: {:?}", session.loss());
        assert!(!session.is_dirty());
        assert_eq!(the_entry(&session), Some(body));
        let read = session.media_marker_records(&token()).unwrap().unwrap();
        assert_eq!(read, held_records());
        assert_eq!(read.entry(&cut_item()).unwrap().markers, vec![marker("m-1", 3)]);
    });
}

#[test]
fn an_invalid_entry_opens_is_noted_refuses_on_read_and_save_keeps_its_bytes() {
    bounded_case("invalid_entry", || {
        for (what, body, code) in invalid_cases() {
            let held = entry(MEDIA_MARKERS_ENTRY, &body);
            let frames = Frames::default();
            let settings_body = serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap();
            let mut session = try_open(&package(&[held.clone(), entry(SEQUENCE_SETTINGS_ENTRY, &settings_body)]), &frames).unwrap_or_else(|f| panic!("{what}: a bad entry locked the file out: {f:?}"));
            assert_eq!(frames.count(), 1, "{what}: one Observe frame for the open");
            assert_eq!(session.loss(), [LossNote { name: held.name.clone(), reason: LossReason::Opaque }], "{what}");
            assert!(!session.is_dirty(), "{what}");
            let refused = session.media_marker_records(&token()).unwrap_err();
            assert_eq!((folio_code(&refused), refused.code()), (Some(code), "document_rejected"), "{what}: {refused:?}");
            // A commit that does not use it goes through and leaves its bytes alone.
            run(&mut session, &set_mode("seq-a", SHIPPED_MODE), 1).unwrap_or_else(|f| panic!("{what}: {f:?}"));
            assert_eq!(the_entry(&session), Some(body.clone()), "{what}");
            // A command that reads it is refused and changes nothing.
            let revision = session.document_revision();
            let failed = run_marker(&mut session, &add(cut_item(), marker("m-9", 1)), 2).unwrap_err();
            assert!(["folio_rejected", "budget_exceeded"].contains(&failed.code()), "{what}: {failed:?}");
            assert_eq!((session.document_revision(), the_entry(&session)), (revision, Some(body.clone())), "{what}");
            // Save keeps the bytes, so a reopen sees the same thing.
            let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert_eq!(the_entry(&reopened), Some(body.clone()), "{what}");
            assert_eq!(reopened.loss().len(), 1, "{what}");
            assert_eq!(folio_code(&reopened.media_marker_records(&token()).unwrap_err()), Some(code), "{what}");
        }
        // The id of a document marker: invalid against this document, a read does not pick a winner.
        let mut marked = seq_b();
        marked.markers.push(marker("taken", 1));
        marked.tracks[0].clips[0].markers.push(marker("taken-too", 1));
        for id in ["taken", "taken-too"] {
            let body = serde_json::to_vec(&markers(vec![(cut_item(), vec![marker(id, 3)])])).unwrap();
            let session = open(&package_of(&[seq_a(), marked.clone()], &[entry(MEDIA_MARKERS_ENTRY, &body)]));
            assert_eq!(session.loss().len(), 1, "{id}");
            assert_eq!(folio_code(&session.media_marker_records(&token()).unwrap_err()), Some(Code::DuplicateIdentity), "{id}");
        }
    });
}

#[test]
fn a_repeated_entry_refuses_on_read_and_a_misnamed_one_stays_opaque() {
    bounded_case("repeated", || {
        let one = entry(MEDIA_MARKERS_ENTRY, &valid_body());
        let two = entry(MEDIA_MARKERS_ENTRY, &serde_json::to_vec(&markers(vec![(digest("other"), vec![marker("m-2", 3)])])).unwrap());
        let mut session = open(&package(&[one, two]));
        assert_eq!(folio_code(&session.media_marker_records(&token()).unwrap_err()), Some(Code::DuplicateIdentity));
        assert_eq!(session.loss().len(), 2);
        let again = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(folio_code(&again.media_marker_records(&token()).unwrap_err()), Some(Code::DuplicateIdentity));

        let body = valid_body();
        let misnamed = Quarantined { name: format!("{DOCUMENT_ENTRY_PREFIX}{MEDIA_MARKERS_ENTRY}.{}", "0".repeat(64)), sha256_hex: sha256_hex_of(&body), bytes: body };
        let session = open(&package(std::slice::from_ref(&misnamed)));
        assert_eq!(session.loss(), [LossNote { name: misnamed.name.clone(), reason: LossReason::Opaque }]);
        assert_eq!(session.media_marker_records(&token()).unwrap(), None);
    });
}

#[test]
fn typed_writes_validate_and_refusals_change_nothing() {
    bounded_case("typed_writes", || {
        let mut session = open(&package(&[]));
        let good = held_records();
        session.set_media_marker_records(Some(&good), &token()).unwrap();
        assert!(session.is_dirty());
        assert_eq!(session.document_revision(), 3, "a direct set is not a revision, as every document entry");
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(good.clone()));
        let held = the_entry(&session);

        let empty_item = markers(vec![(cut_item(), vec![])]);
        assert_eq!(folio_code(&session.set_media_marker_records(Some(&empty_item), &token()).unwrap_err()), Some(Code::InvalidField));
        assert_eq!(folio_code(&session.set_document_entry(MEDIA_MARKERS_ENTRY, Some(b"{not json".to_vec()), &token()).unwrap_err()), Some(Code::InvalidJson));
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.set_media_marker_records(None, &canceled).unwrap_err().code(), "canceled");
        assert_eq!(session.media_marker_records(&canceled).unwrap_err().code(), "canceled");
        assert_eq!(the_entry(&session), held, "every refusal leaves the entry untouched");

        session.set_media_marker_records(None, &token()).unwrap();
        assert_eq!(the_entry(&open(&session.export_checkpoint(&token()).unwrap().bytes)), None, "a removed entry is not saved");

        // An explicit set is the way out of bytes a commit cannot quote; its note goes with them.
        let mut bad = open(&package(&[entry(MEDIA_MARKERS_ENTRY, b"{not json")]));
        assert_eq!(bad.loss().len(), 1);
        assert!(bad.set_media_marker_records(Some(&empty_item), &token()).is_err());
        assert_eq!(bad.loss().len(), 1, "still there, so still noted");
        bad.set_media_marker_records(Some(&good), &token()).unwrap();
        assert!(bad.loss().is_empty(), "{:?}", bad.loss());
        assert_eq!(bad.media_marker_records(&token()).unwrap(), Some(good));
    });
}

#[test]
fn every_command_commits_one_revision_one_receipt_and_one_frame() {
    bounded_case("commands", || {
        let (mut session, frames) = open_counted(&package(&[]));
        let renamed = StudioMarker { name: "Private renamed".into(), ..marker("m-private", 9) };
        let commands = [
            add(cut_item(), marker("m-private", 3)),
            StudioMediaMarkerCommand::Replace { content_digest: cut_item(), marker: renamed.clone() },
            add(digest("private-digest"), marker("m-other", 4)),
            remove(digest("private-digest"), "m-other"),
        ];
        for (i, command) in commands.iter().enumerate() {
            let correlation = 100 + i as u64;
            let revision = session.document_revision();
            let frames_before = frames.count();
            let receipts = session.receipt_count();
            let key = IdempotencyKey::parse(&format!("command-{i}")).unwrap();
            let ctx = context(command.command_id(), revision, correlation);
            let applied = session.submit_media_marker_command_keyed_as(&key, &ctx, command, revision, &token()).unwrap();
            assert_eq!((applied.provider.as_str(), applied.command_id.as_str(), applied.correlation_id), (SEQUENCE_RECORDS_STEP_PROVIDER, command.command_id(), correlation));
            assert_eq!((applied.previous_revision, applied.revision, applied.changed), (revision, revision + 1, true), "one revision: {command:?}");
            assert_eq!(session.receipt_count(), receipts + 1, "one receipt");
            assert_eq!(applied.actor.principal_id, ctx.actor().principal_id(), "the caller's attribution");
            assert_eq!(correlation_of(&applied.report), correlation, "the frame carries the command's correlation");
            assert_eq!(frames.count(), frames_before + 1, "one frame");
            let report = applied.media_marker_report.expect("a command reports");
            assert_eq!((report.command, report.base_revision, report.correlation, report.changed), (command.command_id(), revision, correlation, true));
        }
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(markers(vec![(cut_item(), vec![renamed])])));
        assert!(session.snapshot().document().sequences == vec![seq_a(), seq_b()], "the document's sequences do not change");

        // Codes and counters only: none of the command's text reaches the sink.
        let sent = String::from_utf8_lossy(&frames.bytes()).into_owned();
        for text in ["m-private", "m-other", "Private", "private-digest", "ab0001", "media_marker"] {
            assert!(!sent.contains(text), "{text} in an Observe frame");
        }
    });
}

#[test]
fn the_as_form_of_the_step_and_the_command_answer_under_the_context() {
    bounded_case("as_forms", || {
        let (mut session, frames) = open_counted(&package(&[]));
        let write = held_records();
        let s = marker_step(RecordEdit { expect: None, write: Some(write.clone()) });
        let applied = session.submit_sequence_records_as(&context("cmd.test", 3, 77), &s, 3, &token()).unwrap();
        assert_eq!((applied.revision, correlation_of(&applied.report)), (4, 77));
        assert!(applied.media_marker_report.is_none(), "a report is a command's");
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(write));
        assert_eq!(frames.count(), 2);

        let good = add(cut_item(), marker("m-2", 5));
        let held = the_entry(&session);
        let refuse = |session: &mut Session, ctx: &CommandContext, rev: u64| session.submit_media_marker_command_as(ctx, &good, rev, &token()).unwrap_err().code();
        assert_eq!(refuse(&mut session, &context("media_marker.remove", 4, 1), 4), "command_mismatch");
        let no_command = CommandContext::new(actor(), ClientSessionId::parse("w").unwrap(), None, 4, CorrelationId::new(2), None);
        assert_eq!(refuse(&mut session, &no_command, 4), "command_missing");
        assert_eq!(refuse(&mut session, &context("media_marker.add", 3, 3), 4), "stale_revision");
        assert_eq!(refuse(&mut session, &context("media_marker.add", 3, 4), 3), "stale_revision");
        let stranger = ActorContext::new("account-2", "principal-2", "owner", "owner-principal", "space", "session").unwrap();
        let foreign = CommandContext::new(stranger, ClientSessionId::parse("w").unwrap(), Some(CommandId::parse("media_marker.add").unwrap()), 4, CorrelationId::new(5), None);
        assert_eq!(refuse(&mut session, &foreign, 4), "actor_mismatch");
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.submit_media_marker_command_as(&context("media_marker.add", 4, 6), &good, 4, &canceled).unwrap_err().code(), "canceled");
        assert_eq!((the_entry(&session), session.document_revision()), (held, 4), "refusals change nothing");
        assert_eq!(frames.count(), 2 + 6, "one frame per refused operation");
    });
}

#[test]
fn undo_and_redo_restore_the_exact_entry() {
    bounded_case("undo_redo", || {
        let mut session = open(&package(&[]));
        let commands = [
            add(cut_item(), marker("m-1", 3)),
            add(cut_item(), marker("m-2", 4)),
            StudioMediaMarkerCommand::Replace { content_digest: cut_item(), marker: StudioMarker { comment: "changed".into(), ..marker("m-1", 3) } },
            add(digest("other"), marker("m-3", 4)),
            remove(digest("other"), "m-3"),
        ];
        for (i, command) in commands.iter().enumerate() {
            let before = the_entry(&session);
            let applied = run_marker(&mut session, command, 200 + i as u64).unwrap();
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
        // Removing the last marker removes the entry; its undo brings the exact bytes back.
        let mut fresh = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &valid_body())]));
        let removed = run_marker(&mut fresh, &remove(cut_item(), "m-1"), 1).unwrap();
        assert_eq!(the_entry(&fresh), None, "no empty entry is stored");
        assert_eq!(fresh.media_marker_records(&token()).unwrap(), None);
        fresh.submit_sequence_records(&fresh_inverse(&removed, "cmd.undo"), removed.revision, &token()).unwrap();
        assert_eq!(the_entry(&fresh), Some(valid_body()));
        // An inverse is bound to what it undoes: replaying it conflicts.
        assert_eq!(fresh.submit_sequence_records(&fresh_inverse(&removed, "cmd.again"), removed.revision + 1, &token()).unwrap_err().code(), "value_conflict");
    });
}

#[test]
fn one_step_carries_the_entry_with_the_other_records_and_a_marker_can_change_scope_in_it() {
    bounded_case("one_step", || {
        let mut session = open(&package(&[]));
        let (settings_next, presets_next, markers_next) = (records(vec![settings_entry("seq-a", "custom", F25)]), library(vec![preset("mine", F25)]), held_records());
        let all = SequenceRecordsStep {
            media_markers: Some(RecordEdit { expect: None, write: Some(markers_next.clone()) }),
            ..step(vec![], Some(RecordEdit { expect: None, write: Some(settings_next.clone()) }), Some(RecordEdit { expect: None, write: Some(presets_next.clone()) }))
        };
        let receipts = session.receipt_count();
        let applied = session.submit_sequence_records_keyed(&IdempotencyKey::parse("all-1").unwrap(), &all, 3, &token()).unwrap();
        assert_eq!((applied.previous_revision, applied.revision), (3, 4), "one revision");
        assert_eq!(session.receipt_count(), receipts + 1, "one receipt");
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(markers_next.clone()));
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(settings_next));
        session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!((the_entry(&session), session.document_entry(SEQUENCE_SETTINGS_ENTRY), session.document_entry(SEQUENCE_PRESETS_ENTRY)), (None, None, None), "one undo for all three");

        // A step or a command that changes nothing: no revision, no inverse, no error, no entry.
        let noop = session.submit_sequence_records(&marker_step(RecordEdit { expect: None, write: None }), 5, &token()).unwrap();
        assert!(!noop.changed && noop.revision == 5 && noop.sequence_records_inverse.is_none());
        assert_eq!(the_entry(&session), None);
        session.set_media_marker_records(Some(&markers_next), &token()).unwrap();
        let same = run_marker(&mut session, &StudioMediaMarkerCommand::Replace { content_digest: cut_item(), marker: marker("m-1", 3) }, 9).unwrap();
        assert!(!same.changed && same.revision == 5 && same.sequence_records_inverse.is_none());
        assert_eq!(same.media_marker_report.map(|r| r.changed), Some(false));

        // One step moves a marker from the media item to the clip: the id space holds at the end of it.
        let mut with_marker = seq_b();
        with_marker.tracks[0].clips[0].markers.push(marker("m-1", 3));
        let scope_change = SequenceRecordsStep {
            media_markers: Some(RecordEdit { expect: Some(markers_next.clone()), write: None }),
            ..step(vec![SequenceEdit::Replace { expect: seq_b(), write: with_marker.clone() }], None, None)
        };
        let moved = session.submit_sequence_records(&scope_change, 5, &token()).unwrap();
        assert_eq!(moved.revision, 6);
        assert_eq!(the_entry(&session), None);
        assert_eq!(session.snapshot().document().sequences[1], with_marker);
        session.submit_sequence_records(&fresh_inverse(&moved, "cmd.undo"), 6, &token()).unwrap();
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(markers_next.clone()));
        assert_eq!(session.snapshot().document().sequences[1], seq_b(), "and back, in one undo");
    });
}

#[test]
fn keyed_retries_replay_the_receipt_and_a_changed_request_conflicts() {
    bounded_case("keyed", || {
        let mut session = open(&package(&[]));
        let key = IdempotencyKey::parse("cmd-1").unwrap();
        let ctx = context("media_marker.add", 3, 11);
        let command = add(cut_item(), marker("m-1", 3));
        let first = session.submit_media_marker_command_keyed_as(&key, &ctx, &command, 3, &token()).unwrap();
        assert!(!first.replayed);
        let held = the_entry(&session);
        let retry = session.submit_media_marker_command_keyed_as(&key, &ctx, &command, 3, &token()).unwrap();
        assert!(retry.replayed && retry.revision == first.revision, "the stored receipt, computed once");
        assert_eq!((retry.sequence_records_inverse.clone(), retry.media_marker_report), (first.sequence_records_inverse.clone(), first.media_marker_report));
        assert_eq!((the_entry(&session), session.document_revision()), (held, 4), "nothing applied twice");
        let other = session.submit_media_marker_command_keyed_as(&key, &ctx, &add(cut_item(), marker("m-2", 3)), 3, &token()).unwrap_err();
        assert_eq!(other.code(), "idempotency_conflict");
        // The step form under a key: its binding covers the entry it quotes.
        let write = markers(vec![(digest("other"), vec![marker("m-7", 1)])]);
        let step_key = IdempotencyKey::parse("step-1").unwrap();
        let live = session.media_marker_records(&token()).unwrap();
        let s = marker_step(RecordEdit { expect: live.clone(), write: Some(write) });
        assert_eq!(session.submit_sequence_records_keyed(&step_key, &s, 4, &token()).unwrap().revision, 5);
        assert!(session.submit_sequence_records_keyed(&step_key, &s, 4, &token()).unwrap().replayed);
        let changed = marker_step(RecordEdit { expect: live, write: None });
        assert_eq!(session.submit_sequence_records_keyed(&step_key, &changed, 4, &token()).unwrap_err().code(), "idempotency_conflict");
    });
}

#[test]
fn step_and_command_refusals_leave_the_document_and_the_entry_untouched() {
    bounded_case("refusals", || {
        let (mut session, frames) = open_counted(&package(&[entry(MEDIA_MARKERS_ENTRY, &valid_body())]));
        let held = (the_entry(&session), session.document_revision());
        let receipts = session.receipt_count();
        let opened = frames.count();
        let mut ops = 0usize;
        for (command, code) in [
            (add(cut_item(), marker("m-1", 9)), "duplicate_marker"),
            (add(digest("other"), marker("m-1", 9)), "duplicate_marker"),
            (add(cut_item(), marker("a/b", 9)), "invalid_marker"),
            (add(digest("a\u{1}"), marker("m-9", 9)), "invalid_digest"),
            (StudioMediaMarkerCommand::Replace { content_digest: cut_item(), marker: marker("m-9", 9) }, "marker_missing"),
            (StudioMediaMarkerCommand::Replace { content_digest: digest("other"), marker: marker("m-1", 9) }, "marker_missing"),
            (remove(digest("other"), "m-1"), "marker_missing"),
            (remove(cut_item(), "m-9"), "marker_missing"),
        ] {
            let failed = run_marker(&mut session, &command, 500).unwrap_err();
            assert_eq!(failed.code(), code, "{command:?}");
            assert_eq!(correlation_of(&failed.report), 500);
            ops += 1;
        }
        let live = session.media_marker_records(&token()).unwrap();
        let write = Some(markers(vec![(digest("other"), vec![marker("m-7", 1)])]));
        let code = |session: &mut Session, s: &SequenceRecordsStep, rev: u64| session.submit_sequence_records(s, rev, &token()).unwrap_err().code();
        assert_eq!(code(&mut session, &marker_step(RecordEdit { expect: None, write: write.clone() }), 3), "value_conflict", "the quote is not the live records");
        assert_eq!(code(&mut session, &marker_step(RecordEdit { expect: live.clone(), write: Some(markers(vec![(cut_item(), vec![])])) }), 3), "folio_rejected", "the records must validate");
        assert_eq!(code(&mut session, &marker_step(RecordEdit { expect: live.clone(), write: write.clone() }), 2), "stale_revision");
        ops += 3;
        let mut bad_command = marker_step(RecordEdit { expect: live.clone(), write: write.clone() });
        bad_command.command_id = String::new();
        assert_eq!(code(&mut session, &bad_command, 3), "invalid_command");
        ops += 1;
        assert_eq!((the_entry(&session), session.document_revision()), held, "refusals change nothing");
        assert_eq!(session.receipt_count(), receipts);
        assert_eq!(frames.count(), opened + ops, "one frame per refused operation");

        // A body a reopen would refuse is refused now: more bytes than this budget's input.
        let mut small = SessionConfig::new(actor());
        small.budgets.chronicle.folio.input_bytes = 8 * 1024;
        let mut small = match Session::open_bytes(&package(&[]), small, ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        let heavy = markers(vec![(cut_item(), (0..64).map(|i| StudioMarker { comment: "c".repeat(256), ..marker(&format!("m{i}"), 1) }).collect())]);
        assert_eq!(small.submit_sequence_records(&marker_step(RecordEdit { expect: None, write: Some(heavy.clone()) }), 3, &token()).unwrap_err().code(), "budget_exceeded");
        assert_eq!(the_entry(&small), None);

        // The package limit binds this entry with every other.
        let body_len = serde_json::to_vec(&heavy).unwrap().len() as u64;
        let mut limited = SessionConfig::new(actor());
        limited.budgets.limits.max_entry_bytes = body_len - 1;
        let mut tight = match Session::open_bytes(&package(&[]), limited, ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert_eq!(tight.submit_sequence_records(&marker_step(RecordEdit { expect: None, write: Some(heavy.clone()) }), 3, &token()).unwrap_err().code(), "total_too_large");
        assert_eq!(tight.set_media_marker_records(Some(&heavy), &token()).unwrap_err().code(), "total_too_large");
        assert_eq!(the_entry(&tight), None);
    });
}

#[test]
fn a_step_replaces_an_entry_it_can_quote_and_refuses_bytes_it_cannot() {
    bounded_case("replace_invalid", || {
        // Parsable but invalid: a step can quote it and write valid records, and the note goes with it.
        let stale = markers(vec![(cut_item(), vec![])]);
        let mut session = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &serde_json::to_vec(&stale).unwrap())]));
        assert_eq!(session.loss().len(), 1);
        let keep = SequenceRecordsStep {
            media_markers: Some(RecordEdit { expect: Some(stale.clone()), write: Some(stale.clone()) }),
            ..step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![])) }))
        };
        assert_eq!(session.submit_sequence_records(&keep, 3, &token()).unwrap_err().code(), "folio_rejected", "keeping it while anything else changes is using it");
        let replace = marker_step(RecordEdit { expect: Some(stale.clone()), write: Some(held_records()) });
        let applied = session.submit_sequence_records(&replace, 3, &token()).unwrap();
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(held_records()));
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        // The invalid records cannot go back and nothing else changed, so no undo exists and the answer
        // says so (R56 F-R56-7).
        assert_eq!(applied.sequence_records_inverse, None);
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::Unavailable(vec![MEDIA_MARKERS_ENTRY])));
        // With another record edit, that edit keeps its undo and the repair stands.
        let mut session = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &serde_json::to_vec(&stale).unwrap())]));
        let with_other = SequenceRecordsStep {
            media_markers: Some(RecordEdit { expect: Some(stale.clone()), write: Some(held_records()) }),
            ..step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) }))
        };
        let applied = session.submit_sequence_records(&with_other, 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::WithoutEntries(vec![MEDIA_MARKERS_ENTRY])));
        assert_eq!(applied.sequence_records_inverse.as_ref().unwrap().media_markers, None, "the entry is left alone");
        session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!(session.sequence_presets(&token()).unwrap(), None);
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(held_records()));
        // Bytes that do not parse cannot be quoted, a `None` quote included.
        let mut corrupt = open(&package(&[entry(MEDIA_MARKERS_ENTRY, b"{not json")]));
        let any = marker_step(RecordEdit { expect: None, write: None });
        assert_eq!(corrupt.submit_sequence_records(&any, 3, &token()).unwrap_err().code(), "value_conflict");
        assert_eq!(the_entry(&corrupt), Some(b"{not json".to_vec()));
    });
}

/// `StudioMarker` says its id is unique among the document's markers. A document marker may not take an
/// id the entry holds, by any commit path; the entry's own writes are checked against the document.
#[test]
fn one_marker_id_space_holds_over_the_three_scopes_through_every_commit_path_mt_33736() {
    bounded_case("id_space", || {
        let body = valid_body();
        let mut session = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &body)]));
        let mut on_clip = seq_b();
        on_clip.tracks[0].clips[0].markers.push(marker("m-1", 1));
        let mut on_sequence = seq_b();
        on_sequence.markers.push(marker("m-1", 1));
        for (what, write) in [("a clip marker", &on_clip), ("a sequence marker", &on_sequence)] {
            let refused = session.submit_timeline(&timeline_writing(seq_b(), write.clone()), 3, &token()).unwrap_err();
            assert_eq!(refused.code(), "folio_rejected", "{what} through a timeline step");
            let multicam = MulticamStep { command_id: "cmd.m".into(), correlation_id: 1, sequences: vec![SequenceEdit::Replace { expect: seq_b(), write: write.clone() }], records_expect: None, records_write: None };
            assert_eq!(session.submit_multicam(&multicam, 3, &token()).unwrap_err().error.code(), "folio_rejected", "{what} through a multicam step");
            let records_step = step(vec![SequenceEdit::Replace { expect: seq_b(), write: write.clone() }], None, None);
            assert_eq!(session.submit_sequence_records(&records_step, 3, &token()).unwrap_err().code(), "folio_rejected", "{what} through a records step");
        }
        assert_eq!((the_entry(&session), session.document_revision()), (Some(body.clone()), 3), "every refusal changes nothing");
        assert_eq!(session.snapshot().document().sequences, vec![seq_a(), seq_b()]);
        // Any other edit commits, and a marker with a free id too.
        let mut free = seq_b();
        free.tracks[0].clips[0].markers.push(marker("m-free", 1));
        let edited = session.submit_timeline(&timeline_writing(seq_b(), free.clone()), 3, &token()).unwrap();
        assert_eq!(edited.revision, 4);
        assert_eq!(the_entry(&session), Some(body.clone()));
        // The entry's own writes: a command, a step and a set all see the document's ids.
        assert_eq!(run_marker(&mut session, &add(cut_item(), marker("m-free", 1)), 1).unwrap_err().code(), "duplicate_marker");
        let taken = markers(vec![(cut_item(), vec![marker("m-free", 1)])]);
        let live = session.media_marker_records(&token()).unwrap();
        assert_eq!(session.submit_sequence_records(&marker_step(RecordEdit { expect: live, write: Some(taken.clone()) }), 4, &token()).unwrap_err().code(), "folio_rejected");
        assert_eq!(folio_code(&session.set_media_marker_records(Some(&taken), &token()).unwrap_err()), Some(Code::DuplicateIdentity));
        // An undo cannot bring a document marker back onto an id taken since: refused, the caller's to settle.
        session.submit_timeline(&edited.timeline_inverse.clone().expect("a timeline inverse"), 4, &token()).unwrap();
        let revision = session.document_revision();
        run_marker(&mut session, &add(cut_item(), marker("m-free", 1)), 2).unwrap();
        let redo = timeline_writing(seq_b(), free);
        assert_eq!(session.submit_timeline(&redo, revision + 1, &token()).unwrap_err().code(), "folio_rejected");
        // An entry already invalid does not hold a commit up; it stays as it came.
        let stale = serde_json::to_vec(&markers(vec![(cut_item(), vec![])])).unwrap();
        let mut bad = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &stale)]));
        assert_eq!(bad.submit_timeline(&timeline_writing(seq_b(), on_clip), 3, &token()).unwrap().revision, 4);
        assert_eq!(the_entry(&bad), Some(stale));
    });
}

/// Brief item 5, by table. The marker lives on the media item, not on a clip.
#[test]
fn removing_the_last_clip_cut_from_an_item_leaves_the_entry_valid_and_unchanged() {
    bounded_case("removal", || {
        type Removal = (&'static str, fn(&mut Session) -> Vec<StudioSequence>);
        let table: [Removal; 4] = [
            ("the last clip, through a timeline step", |s| {
                let mut empty = seq_b();
                empty.tracks[0].clips.clear();
                empty.duration = Ticks::new(0);
                s.submit_timeline(&timeline_writing(seq_b(), empty.clone()), 3, &token()).unwrap();
                vec![seq_a(), empty]
            }),
            ("the last clip, through a records step", |s| {
                let mut empty = seq_b();
                empty.tracks[0].clips.clear();
                empty.duration = Ticks::new(0);
                s.submit_sequence_records(&step(vec![SequenceEdit::Replace { expect: seq_b(), write: empty.clone() }], None, None), 3, &token()).unwrap();
                vec![seq_a(), empty]
            }),
            ("the sequence holding it, through a multicam step", |s| {
                s.submit_multicam(&removing_sequence(seq_b()), 3, &token()).unwrap();
                vec![seq_a()]
            }),
            ("every sequence, through a records step", |s| {
                let gone = vec![SequenceEdit::Remove { expect: seq_a() }, SequenceEdit::Remove { expect: seq_b() }];
                s.submit_sequence_records(&step(gone, None, None), 3, &token()).unwrap();
                vec![]
            }),
        ];
        let body = valid_body();
        for (what, removal) in table {
            let (mut session, frames) = open_counted(&package(&[entry(MEDIA_MARKERS_ENTRY, &body)]));
            let opened = frames.count();
            let left = removal(&mut session);
            assert_eq!(session.snapshot().document().sequences, left, "{what}");
            assert_eq!(frames.count(), opened + 1, "{what}: one frame");
            assert_eq!(session.document_revision(), 4, "{what}: one revision");
            assert_eq!(the_entry(&session), Some(body.clone()), "{what}: byte-identical");
            assert!(session.loss().is_empty(), "{what}: {:?}", session.loss());
            assert_eq!(session.media_marker_records(&token()).unwrap(), Some(held_records()), "{what}: valid");
            let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert_eq!(the_entry(&reopened), Some(body.clone()), "{what}: through a save");
            assert!(reopened.loss().is_empty(), "{what}");
            assert_eq!(reopened.media_marker_records(&token()).unwrap(), Some(held_records()), "{what}");
        }
    });
}

/// Brief item 5's other row. The entry names no sequence or clip, so nothing can orphan it and no created
/// sequence is missing one (R55 F-R55-4 (a)-(d) do not come back).
#[test]
fn the_entry_names_no_sequence_or_clip_r55_f4() {
    bounded_case("names_nothing", || {
        let body = valid_body();
        let text = String::from_utf8(body.clone()).unwrap();
        for name in ["seq-a", "seq-b", "SCLP-", "STRK-", "camera-1", "sequence_id", "clip_id", "artifact_manifest_id", "asset_id", "frame_rate"] {
            assert!(!text.contains(name), "{name} in the entry");
        }
        // (c) A sequence created with a clip of the same item needs no entry change; the item's markers are its.
        let mut session = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &body)]));
        let mut cut_again = seq_b().tracks[0].clips[0].clone();
        cut_again.clip_id = identity("SCLP", 9);
        let created = sequence("seq-c", F24, vec![cut_again]);
        let insert = MulticamStep { command_id: "cmd.create".into(), correlation_id: 1, sequences: vec![SequenceEdit::Insert { at: 2, write: created }], records_expect: None, records_write: None };
        assert_eq!(session.submit_multicam(&insert, 3, &token()).unwrap().revision, 4);
        assert_eq!(the_entry(&session), Some(body.clone()));
        // (a) An item with no marker has no entry, and that is a state: nothing to report. An item no clip
        // is cut from is valid: the entry opens with no clip of it in the document.
        let nothing_cut = serde_json::to_vec(&markers(vec![(digest("never-cut"), vec![marker("m-1", 1)])])).unwrap();
        let opened = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &nothing_cut)]));
        assert!(opened.loss().is_empty());
        assert!(opened.media_marker_records(&token()).unwrap().unwrap().entry(&digest("never-cut")).is_some());
        // (d) No value of the document is copied: a timeline step that changes the rate leaves it valid.
        let mut faster = seq_b();
        faster.frame_rate = SequenceFrameRate { ticks_per_frame: F25 };
        let mut retimed = open(&package(&[entry(MEDIA_MARKERS_ENTRY, &body)]));
        assert_eq!(retimed.submit_timeline(&timeline_writing(seq_b(), faster), 3, &token()).unwrap().revision, 4);
        assert_eq!(retimed.media_marker_records(&token()).unwrap(), Some(held_records()));
    });
}

/// Brief item 6: everything authored survives save and reopen through the session, byte-identical.
#[test]
fn authored_media_markers_survive_save_and_reopen_through_the_session() {
    bounded_case("save_reopen", || {
        let dir = std::env::temp_dir().join(format!("hsk-studio-session-media-markers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.handshake");
        let mut session = open(&package(&[]));
        let roles = [
            None,
            Some(MarkerRole::Chapter),
            Some(MarkerRole::WebLink { url: "https://example.test/a".into() }),
            Some(MarkerRole::CuePoint),
            Some(MarkerRole::Caption { text: "a caption".into() }),
        ];
        let authored: Vec<StudioMarker> = roles.iter().enumerate().map(|(i, role)| StudioMarker { role: role.clone(), duration: Ticks::new(if i == 0 { 0 } else { F24 }), ..marker(&format!("m-{i}"), i as u64) }).collect();
        let steps: Vec<Applied> = authored.iter().enumerate().map(|(i, m)| run_marker(&mut session, &add(cut_item(), m.clone()), i as u64).unwrap()).collect();
        assert_eq!(session.document_revision(), 8);
        let expect = |s: &Session| {
            let live = s.media_marker_records(&token()).unwrap().unwrap();
            assert_eq!(live, markers(vec![(cut_item(), authored.clone())]));
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
        assert_eq!(reopened.document_revision(), 8);
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
