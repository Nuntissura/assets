//! Lane SMB-TYPE-AUTHORED-TOKEN-1, brief items 1 and 2 (STU-TYP-130): the authored composition token
//! persists as the document entry `studio/composition_token.<sha256>`. A package without it opens
//! unchanged and reads as the first token with a typed note; a token this build does not know opens,
//! is noted and refuses on read, and its bytes survive. Vendor parity UNVERIFIED (D5).
mod sequence_records_support;
use hsk_studio_folio::*;
use hsk_studio_package::{LossNote, LossReason, PackageError};
use hsk_studio_session::*;
use sequence_records_support::*;
use serde_json::{Value, json};

const V1: &str = STUDIO_COMPOSITION_TOKENS[0];
const V3: &str = STUDIO_COMPOSITION_TOKENS[2];
const FUTURE: &str = "hsk-type-shaped-runs@4/future";

fn body(token: &str) -> Vec<u8> {
    serde_json::to_vec(&StudioCompositionTokenRecord::new(doc_id(), token)).unwrap()
}
fn wire(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(&body(V3)).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}
fn folio_code(e: &SessionError) -> Option<Code> {
    match e {
        SessionError::Package(PackageError::Document(d)) => Some(d.code),
        _ => None,
    }
}
fn stored(s: &Session) -> Option<Vec<u8>> {
    s.document_entry(COMPOSITION_TOKEN_ENTRY).map(<[u8]>::to_vec)
}

#[test]
fn a_package_without_the_entry_opens_unchanged_and_reads_as_the_first_token_with_a_typed_note() {
    bounded_case("no_entry", || {
        assert!(DOCUMENT_ENTRY_TOKENS.contains(&COMPOSITION_TOKEN_ENTRY) && COMPOSITION_TOKEN_ENTRY == "composition_token");
        let (session, frames) = open_counted(&package(&[]));
        assert!(session.loss().is_empty(), "a typed note on the read, not a loss note: {:?}", session.loss());
        assert!(!session.is_dirty());
        assert_eq!(session.composition_token_record(&token()).unwrap(), None);
        assert_eq!(
            session.authored_composition(&token()).unwrap(),
            AuthoredComposition { token: V1, stored: false, note: Some(COMPOSITION_TOKEN_ABSENT) }
        );
        assert_eq!(frames.count(), 1, "the open; a typed read sends no frame");
    });
}

#[test]
fn saving_a_document_with_no_token_writes_no_entry() {
    bounded_case("save_no_entry", || {
        let mut session = open(&package(&[]));
        let saved = session.export_checkpoint(&token()).unwrap().bytes;
        let reopened = open(&saved);
        assert_eq!(stored(&reopened), None);
        assert!(!reopened.authored_composition(&token()).unwrap().stored);
        // So does a save after an edit that has nothing to do with the token.
        let mut session = reopened;
        let revision = session.document_revision();
        let mut write = seq_a();
        write.name = "renamed".into();
        session.submit_sequence_records(&step(vec![SequenceEdit::Replace { expect: seq_a(), write }], None, None), revision, &token()).unwrap();
        assert_eq!(stored(&session), None);
        assert_eq!(stored(&open(&session.export_checkpoint(&token()).unwrap().bytes)), None);
    });
}

#[test]
fn a_stored_token_opens_typed_without_a_loss_note_and_is_what_the_document_composes_under() {
    bounded_case("stored", || {
        for (rank, known) in STUDIO_COMPOSITION_TOKENS.iter().enumerate() {
            let bytes = body(known);
            let mut session = open(&package(&[entry(COMPOSITION_TOKEN_ENTRY, &bytes)]));
            assert!(session.loss().is_empty(), "not an opaque entry any more: {:?}", session.loss());
            assert!(!session.is_dirty());
            assert_eq!(stored(&session), Some(bytes.clone()));
            assert_eq!(session.composition_token_record(&token()).unwrap().unwrap().composition_version, *known);
            let read = session.authored_composition(&token()).unwrap();
            assert_eq!(read, AuthoredComposition { token: STUDIO_COMPOSITION_TOKENS[rank], stored: true, note: None });
            // It survives a save and reopen byte for byte.
            let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert_eq!(stored(&reopened), Some(bytes));
            assert_eq!(reopened.authored_composition(&token()).unwrap().token, *known);
        }
    });
}

#[test]
fn a_token_this_build_does_not_know_opens_is_noted_and_refuses_on_read_and_save_keeps_its_bytes() {
    bounded_case("unknown", || {
        let bytes = body(FUTURE);
        let held = entry(COMPOSITION_TOKEN_ENTRY, &bytes);
        let (mut session, frames) = open_counted(&package(std::slice::from_ref(&held)));
        assert_eq!(session.loss(), &[LossNote { name: held.name.clone(), reason: LossReason::Opaque }]);
        assert_eq!(frames.count(), 1);
        let refused = session.authored_composition(&token()).unwrap_err();
        assert_eq!(refused.code(), COMPOSITION_TOKEN_UNKNOWN, "typed, and never the newest token");
        assert_eq!(session.composition_token_record(&token()).unwrap_err().code(), COMPOSITION_TOKEN_UNKNOWN);
        let saved = session.export_checkpoint(&token()).unwrap().bytes;
        let reopened = open(&saved);
        assert_eq!(stored(&reopened), Some(bytes), "byte for byte");
        assert_eq!(reopened.loss().len(), 1);
    });
}

#[test]
fn a_hostile_entry_opens_is_noted_refuses_on_read_and_survives_a_save() {
    bounded_case("hostile", || {
        let cases: Vec<(&str, Vec<u8>, Code)> = vec![
            ("not JSON", b"{not json".to_vec(), Code::InvalidJson),
            ("an unknown field", wire(|v| v["vendor"] = json!(1)), Code::InvalidJson),
            ("another document", wire(|v| v["document_id"] = json!(identity("SDOC", 2))), Code::InvalidField),
            ("an empty token", wire(|v| v["composition_version"] = json!("")), Code::InvalidField),
            ("a token over the limit", wire(|v| v["composition_version"] = json!("x".repeat(MAX_COMPOSITION_TOKEN_BYTES + 1))), Code::InvalidField),
            ("a token past ASCII", wire(|v| v["composition_version"] = json!("hsk-\u{e9}")), Code::InvalidField),
            ("bytes past the input limit", vec![b' '; MAX_COMPOSITION_TOKEN_INPUT_BYTES + 1], Code::BudgetExceeded),
            ("nesting past the depth budget", "[".repeat(64).into_bytes(), Code::BudgetExceeded),
        ];
        for (what, bytes, code) in cases {
            let held = entry(COMPOSITION_TOKEN_ENTRY, &bytes);
            let mut session = open(&package(std::slice::from_ref(&held)));
            assert_eq!(session.loss(), &[LossNote { name: held.name.clone(), reason: LossReason::Opaque }], "{what}");
            let refused = session.authored_composition(&token()).unwrap_err();
            assert_eq!(folio_code(&refused), Some(code), "{what}: {refused:?}");
            assert_eq!(stored(&open(&session.export_checkpoint(&token()).unwrap().bytes)), Some(bytes), "{what}");
        }
    });
}

#[test]
fn a_repeated_entry_refuses_on_read_and_a_misnamed_one_stays_opaque() {
    bounded_case("repeated", || {
        let held = [entry(COMPOSITION_TOKEN_ENTRY, &body(V1)), entry(COMPOSITION_TOKEN_ENTRY, &body(V3))];
        let session = open(&package(&held));
        assert_eq!(session.loss().len(), 2);
        assert_eq!(folio_code(&session.authored_composition(&token()).unwrap_err()), Some(Code::DuplicateIdentity), "neither is chosen");
        // A name whose digest is not the body's is no document entry of the token: it reads as absent.
        let mut misnamed = entry(COMPOSITION_TOKEN_ENTRY, &body(V3));
        misnamed.name = format!("{DOCUMENT_ENTRY_PREFIX}{COMPOSITION_TOKEN_ENTRY}.{}", "0".repeat(64));
        let session = open(&package(&[misnamed]));
        assert_eq!(session.loss().len(), 1);
        assert!(!session.authored_composition(&token()).unwrap().stored);
    });
}

#[test]
fn a_direct_set_validates_and_refusals_change_nothing() {
    bounded_case("set", || {
        let mut session = open(&package(&[]));
        let mine = StudioCompositionTokenRecord::new(doc_id(), V3);
        session.set_composition_token_record(Some(&mine), &token()).unwrap();
        assert_eq!(session.authored_composition(&token()).unwrap().token, V3);
        for bad in [StudioCompositionTokenRecord::new(doc_id(), FUTURE), StudioCompositionTokenRecord::new(identity("SDOC", 2), V3)] {
            assert!(session.set_composition_token_record(Some(&bad), &token()).is_err());
            assert_eq!(session.composition_token_record(&token()).unwrap().as_ref(), Some(&mine));
        }
        assert!(session.set_document_entry(COMPOSITION_TOKEN_ENTRY, Some(b"{not json".to_vec()), &token()).is_err());
        assert_eq!(session.composition_token_record(&token()).unwrap().as_ref(), Some(&mine));
        session.set_composition_token_record(None, &token()).unwrap();
        assert_eq!(session.composition_token_record(&token()).unwrap(), None);
    });
}

#[test]
fn a_new_document_entry_records_the_current_token() {
    bounded_case("new_document", || {
        let held = new_document_composition_entry(&doc_id()).unwrap();
        let session = open(&package(&[held]));
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        assert_eq!(session.authored_composition(&token()).unwrap(), AuthoredComposition { token: CURRENT_COMPOSITION_TOKEN, stored: true, note: None });
        assert!(session.composition_token_record(&token()).unwrap().unwrap().upgrades.is_empty());
    });
}

#[test]
fn a_canceled_read_is_canceled() {
    bounded_case("cancel", || {
        let session = open(&package(&[]));
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.authored_composition(&canceled).unwrap_err(), SessionError::Canceled);
        assert_eq!(session.composition_token_record(&canceled).unwrap_err(), SessionError::Canceled);
    });
}
