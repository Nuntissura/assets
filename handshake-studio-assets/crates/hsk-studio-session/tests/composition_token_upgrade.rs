//! Lane SMB-TYPE-AUTHORED-TOKEN-1, brief items 4 and 5 (STU-TYP-130: "the upgrade MUST be a recorded,
//! undoable, diffable operation"): the upgrade is one session commit, one revision, one receipt, one
//! inverse, one Observe frame of codes and counters; its diff is the from-token, the to-token and the
//! stories it affects; the record survives a save and reopen. Vendor parity UNVERIFIED (D5).
mod sequence_records_support;
use hsk_studio_accord::{ActorContext, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use hsk_studio_session::*;
use sequence_records_support::*;
use serde_json::json;

const V1: &str = STUDIO_COMPOSITION_TOKENS[0];
const V2: &str = STUDIO_COMPOSITION_TOKENS[1];
const V3: &str = STUDIO_COMPOSITION_TOKENS[2];
const FUTURE: &str = "hsk-type-shaped-runs@4/future";
const COMMAND: &str = "type.composition.upgrade";
const STORY_A: &str = "STXT-019a0000-0000-7000-8000-00000000000a";
const STORY_B: &str = "STXT-019a0000-0000-7000-8000-00000000000b";

fn body(token: &str) -> Vec<u8> {
    serde_json::to_vec(&StudioCompositionTokenRecord::new(doc_id(), token)).unwrap()
}
fn text_entry() -> hsk_studio_package::Quarantined {
    let text = json!({"schema": "hsk.harness.text_layers@1", "layers": [], "stories": [{"story_id": STORY_B}, {"story_id": STORY_A}], "styles": {}});
    entry("text", &serde_json::to_vec(&text).unwrap())
}
fn upgrade(from: Option<&str>, to: &str) -> CompositionTokenStep {
    CompositionTokenStep { command_id: COMMAND.into(), correlation_id: 7, change: CompositionTokenChange::Upgrade { from: from.map(str::to_owned), to: to.to_owned() } }
}
fn restore(expect: Option<StudioCompositionTokenRecord>, write: Option<StudioCompositionTokenRecord>) -> CompositionTokenStep {
    CompositionTokenStep { command_id: COMMAND.into(), correlation_id: 7, change: CompositionTokenChange::Restore { expect, write } }
}
fn submit(s: &mut Session, step: &CompositionTokenStep) -> Result<Applied, Failed> {
    let revision = s.document_revision();
    s.submit_composition_token(step, revision, &token())
}
fn inverse_of(applied: &Applied) -> CompositionTokenStep {
    let mut inverse = applied.composition_token_inverse.clone().expect("a changed step has an inverse");
    inverse.command_id = "type.composition.undo".into();
    inverse.correlation_id = 8;
    inverse
}
fn stored(s: &Session) -> Option<Vec<u8>> {
    s.document_entry(COMPOSITION_TOKEN_ENTRY).map(<[u8]>::to_vec)
}
fn diff(from: &str, to: &str, stories: &[&str]) -> StudioCompositionUpgrade {
    StudioCompositionUpgrade { from_token: from.into(), to_token: to.into(), story_ids: stories.iter().map(|s| (*s).to_owned()).collect() }
}

#[test]
fn upgrading_a_document_with_no_token_creates_the_entry_and_undo_removes_it() {
    bounded_case("no_token", || {
        let (mut session, frames) = open_counted(&package(&[text_entry()]));
        let start = session.document_revision();
        assert_eq!(stored(&session), None);
        let applied = submit(&mut session, &upgrade(None, V3)).unwrap();
        assert!(applied.changed);
        assert_eq!((applied.previous_revision, applied.revision), (start, start + 1));
        assert_eq!(session.document_revision(), start + 1, "one revision");
        assert_eq!(applied.provider, COMPOSITION_TOKEN_STEP_PROVIDER);
        assert_eq!(applied.command_id, COMMAND);
        assert_eq!(applied.composition_upgrade, Some(diff(V1, V3, &[STORY_A, STORY_B])), "the diff");
        assert_eq!(frames.count(), 2, "the open and one frame");
        let created = stored(&session).expect("the upgrade creates the entry");
        assert_eq!(session.authored_composition(&token()).unwrap(), AuthoredComposition { token: V3, stored: true, note: None });
        assert_eq!(session.composition_token_record(&token()).unwrap().unwrap().upgrades, [diff(V1, V3, &[STORY_A, STORY_B])], "recorded");
        assert!(session.loss().is_empty());
        assert!(session.is_dirty());

        // Undo removes it; the document reads as it did, under a new revision.
        let undone = submit(&mut session, &inverse_of(&applied)).unwrap();
        assert_eq!(undone.revision, start + 2);
        assert_eq!(undone.composition_upgrade, Some(diff(V3, V1, &[STORY_A, STORY_B])));
        assert_eq!(stored(&session), None, "undo removes the entry");
        assert_eq!(session.authored_composition(&token()).unwrap(), AuthoredComposition { token: V1, stored: false, note: Some(COMPOSITION_TOKEN_ABSENT) });
        assert_eq!(frames.count(), 3);

        // The undo's inverse is the redo: the same record, byte for byte.
        let redone = submit(&mut session, &inverse_of(&undone)).unwrap();
        assert_eq!(redone.revision, start + 3);
        assert_eq!(redone.composition_upgrade, Some(diff(V1, V3, &[STORY_A, STORY_B])));
        assert_eq!(stored(&session), Some(created));
        assert_eq!(frames.count(), 4);
    });
}

#[test]
fn a_stored_token_upgrades_one_step_at_a_time_and_undo_walks_back() {
    bounded_case("chain", || {
        let start_body = body(V1);
        let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &start_body)]));
        let first = submit(&mut session, &upgrade(Some(V1), V2)).unwrap();
        let second = submit(&mut session, &upgrade(Some(V2), V3)).unwrap();
        assert_eq!(first.composition_upgrade, Some(diff(V1, V2, &[])));
        assert_eq!(second.composition_upgrade, Some(diff(V2, V3, &[])));
        let record = session.composition_token_record(&token()).unwrap().unwrap();
        assert_eq!(record.composition_version, V3);
        assert_eq!(record.upgrades, [diff(V1, V2, &[]), diff(V2, V3, &[])]);
        submit(&mut session, &inverse_of(&second)).unwrap();
        assert_eq!(session.authored_composition(&token()).unwrap().token, V2);
        submit(&mut session, &inverse_of(&first)).unwrap();
        assert_eq!(session.authored_composition(&token()).unwrap().token, V1);
        assert_eq!(stored(&session), Some(start_body), "the entry as it came");
    });
}

#[test]
fn refusals_are_typed_and_change_nothing() {
    bounded_case("refusals", || {
        let bad_command = CompositionTokenStep { command_id: String::new(), ..upgrade(Some(V2), V3) };
        let cases: Vec<(&str, CompositionTokenStep, &str)> = vec![
            ("a target this build does not know", upgrade(Some(V2), FUTURE), COMPOSITION_TOKEN_UNKNOWN),
            ("a target past the byte limit", upgrade(Some(V2), &"x".repeat(MAX_COMPOSITION_TOKEN_BYTES + 1)), COMPOSITION_TOKEN_UNKNOWN),
            ("an empty target", upgrade(Some(V2), ""), COMPOSITION_TOKEN_UNKNOWN),
            ("a move to an older token", upgrade(Some(V2), V1), COMPOSITION_TOKEN_OLDER),
            ("a quote of another token", upgrade(Some(V1), V3), "value_conflict"),
            ("a quote of no entry", upgrade(None, V3), "value_conflict"),
            ("a command id that is no id", bad_command, "invalid_command"),
        ];
        for (what, step, code) in cases {
            let (mut session, frames) = open_counted(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &body(V2)), text_entry()]));
            let before = (session.document_revision(), stored(&session), session.receipt_count(), session.snapshot().encoded_bytes().to_vec());
            let refused = submit(&mut session, &step).unwrap_err();
            assert_eq!(refused.error.code(), code, "{what}");
            assert_eq!((session.document_revision(), stored(&session), session.receipt_count(), session.snapshot().encoded_bytes().to_vec()), before, "{what}");
            assert!(!session.is_dirty(), "{what}");
            assert_eq!(frames.count(), 2, "{what}: the open and one frame for the refusal");
        }
    });
}

#[test]
fn a_restore_is_only_the_undo_or_the_redo_of_a_recorded_upgrade() {
    bounded_case("restore", || {
        let record = |token: &str| StudioCompositionTokenRecord::new(doc_id(), token);
        let at_v3 = {
            let mut r = record(V3);
            r.upgrades.push(diff(V1, V3, &[]));
            r
        };
        let cases: Vec<(&str, Vec<u8>, CompositionTokenStep, &str)> = vec![
            ("older with no record of the move", body(V3), restore(Some(record(V3)), Some(record(V1))), COMPOSITION_TOKEN_OLDER),
            ("removing an entry that records no upgrade", body(V3), restore(Some(record(V3)), None), COMPOSITION_TOKEN_OLDER),
            ("newer with no record of the move", body(V1), restore(Some(record(V1)), Some(record(V3))), COMPOSITION_TOKEN_UNRECORDED),
            ("a target this build does not know", body(V1), restore(Some(record(V1)), Some(record(FUTURE))), COMPOSITION_TOKEN_UNKNOWN),
            ("a quote of another record", body(V1), restore(Some(record(V2)), Some(at_v3.clone())), "value_conflict"),
            ("a quote of no entry", body(V1), restore(None, Some(at_v3.clone())), "value_conflict"),
            ("a record of another document", body(V1), restore(Some(record(V1)), Some(StudioCompositionTokenRecord::new(identity("SDOC", 2), V1))), "folio_rejected"),
        ];
        for (what, held, step, code) in cases {
            let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &held)]));
            let (revision, before) = (session.document_revision(), stored(&session));
            assert_eq!(submit(&mut session, &step).unwrap_err().error.code(), code, "{what}");
            assert_eq!((session.document_revision(), stored(&session)), (revision, before), "{what}");
        }
        // The forward move a restore may make is one that records itself.
        let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &body(V1))]));
        assert_eq!(submit(&mut session, &restore(Some(record(V1)), Some(at_v3))).unwrap().composition_upgrade, Some(diff(V1, V3, &[])));
    });
}

#[test]
fn the_same_token_is_no_change_and_creates_no_entry() {
    bounded_case("same", || {
        let (mut session, frames) = open_counted(&package(&[]));
        let revision = session.document_revision();
        let applied = submit(&mut session, &upgrade(None, V1)).unwrap();
        assert!(!applied.changed && applied.composition_upgrade.is_none() && applied.composition_token_inverse.is_none());
        assert_eq!(session.document_revision(), revision);
        assert_eq!(stored(&session), None);
        assert!(!session.is_dirty());
        assert_eq!(frames.count(), 2);
        let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &body(V3))]));
        assert!(!submit(&mut session, &upgrade(Some(V3), V3)).unwrap().changed);
    });
}

#[test]
fn an_entry_that_does_not_read_blocks_the_upgrade_and_keeps_its_bytes() {
    bounded_case("invalid", || {
        // A token from a later build: the upgrade cannot say where it starts.
        let future = body(FUTURE);
        let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &future)]));
        assert_eq!(submit(&mut session, &upgrade(Some(FUTURE), V3)).unwrap_err().error.code(), COMPOSITION_TOKEN_UNKNOWN);
        assert_eq!(stored(&session), Some(future.clone()));
        assert_eq!(session.loss().len(), 1, "the note stays until the entry is replaced");
        // Bytes that do not parse and a repeated token cannot be quoted, as the sequence entries.
        let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, b"{not json")]));
        assert_eq!(submit(&mut session, &upgrade(None, V3)).unwrap_err().error.code(), "value_conflict");
        assert_eq!(stored(&session).as_deref(), Some(&b"{not json"[..]));
        let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &body(V1)), entry(COMPOSITION_TOKEN_ENTRY, &body(V2))]));
        assert_eq!(submit(&mut session, &upgrade(Some(V1), V3)).unwrap_err().error.code(), "value_conflict");
        // A text entry the diff cannot read: no shorter diff is recorded.
        let mut session = open(&package(&[entry("text", b"{not json")]));
        assert_eq!(submit(&mut session, &upgrade(None, V3)).unwrap_err().error.code(), "folio_rejected");
        assert_eq!(stored(&session), None);
    });
}

#[test]
fn the_keyed_form_replays_and_the_as_form_checks_its_context() {
    bounded_case("forms", || {
        let mut session = open(&package(&[text_entry()]));
        let revision = session.document_revision();
        let key = IdempotencyKey::parse("composition-upgrade-1").unwrap();
        let step = upgrade(None, V3);
        let first = session.submit_composition_token_keyed(&key, &step, revision, &token()).unwrap();
        assert!(!first.replayed);
        assert_eq!(session.receipt_count(), 1, "one receipt");
        let entry_bytes = stored(&session);
        let again = session.submit_composition_token_keyed(&key, &step, revision, &token()).unwrap();
        assert!(again.replayed);
        assert_eq!(again.composition_upgrade, first.composition_upgrade);
        assert_eq!((session.document_revision(), stored(&session)), (revision + 1, entry_bytes));
        let other = upgrade(None, V2);
        assert_eq!(session.submit_composition_token_keyed(&key, &other, revision, &token()).unwrap_err().error.code(), "idempotency_conflict");

        let (mut session, frames) = open_counted(&package(&[]));
        let revision = session.document_revision();
        let ctx = context(COMMAND, revision, 4242);
        let applied = session.submit_composition_token_as(&ctx, &upgrade(None, V3), revision, &token()).unwrap();
        assert!(applied.changed);
        assert_eq!(frames.count(), 2);
        match &applied.report {
            ReportStatus::Delivered(receipt) => assert_eq!(receipt.correlation_id, 4242, "the frame carries the command's correlation"),
            other => panic!("the frame was not delivered: {other:?}"),
        }
        let revision = session.document_revision();
        let stranger = ActorContext::new("other", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let foreign = CommandContext::new(stranger, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(COMMAND).unwrap()), revision, CorrelationId::new(5), None);
        let step = upgrade(Some(V3), V3);
        assert_eq!(session.submit_composition_token_as(&foreign, &step, revision, &token()).unwrap_err().error.code(), "actor_mismatch");
        assert_eq!(session.submit_composition_token_as(&context("type.other", revision, 5), &step, revision, &token()).unwrap_err().error.code(), "command_mismatch");
        assert_eq!(session.submit_composition_token_as(&context(COMMAND, revision + 1, 5), &step, revision, &token()).unwrap_err().error.code(), "stale_revision");
    });
}

#[test]
fn a_frame_carries_codes_and_counters_only() {
    bounded_case("frames", || {
        let (mut session, frames) = open_counted(&package(&[text_entry()]));
        let applied = submit(&mut session, &upgrade(None, V3)).unwrap();
        submit(&mut session, &upgrade(Some(V3), FUTURE)).unwrap_err();
        submit(&mut session, &inverse_of(&applied)).unwrap();
        let sent = frames.bytes();
        for text in [STORY_A, STORY_B, V1, V3, FUTURE, "hsk-type-shaped-runs", COMMAND] {
            assert!(!sent.windows(text.len()).any(|w| w == text.as_bytes()), "{text} reached a frame");
        }
    });
}

#[test]
fn a_canceled_upgrade_changes_nothing() {
    bounded_case("cancel", || {
        let mut session = open(&package(&[]));
        let canceled = token();
        canceled.cancel();
        let revision = session.document_revision();
        assert_eq!(session.submit_composition_token(&upgrade(None, V3), revision, &canceled).unwrap_err().error, SessionError::Canceled);
        assert_eq!((session.document_revision(), stored(&session)), (revision, None));
    });
}

#[test]
fn the_token_survives_save_and_reopen_and_an_inverse_taken_before_the_save_still_undoes() {
    bounded_case("round_trip", || {
        // No token: a save writes none, the upgrade writes it, a reopen reads it.
        let mut session = open(&package(&[text_entry()]));
        assert_eq!(stored(&open(&session.export_checkpoint(&token()).unwrap().bytes)), None);
        let applied = submit(&mut session, &upgrade(None, V3)).unwrap();
        let saved = session.export_checkpoint(&token()).unwrap().bytes;
        let mut reopened = open(&saved);
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        assert_eq!(reopened.authored_composition(&token()).unwrap(), AuthoredComposition { token: V3, stored: true, note: None });
        assert_eq!(reopened.composition_token_record(&token()).unwrap().unwrap().upgrades, [diff(V1, V3, &[STORY_A, STORY_B])], "the record survives");
        assert_eq!(reopened.document_revision(), session.document_revision());
        // The undo reaches a reopened document; saved again, the file has no token.
        submit(&mut reopened, &inverse_of(&applied)).unwrap();
        let undone = reopened.export_checkpoint(&token()).unwrap().bytes;
        assert_eq!(stored(&open(&undone)), None);
        assert!(!open(&undone).authored_composition(&token()).unwrap().stored);
    });
}
