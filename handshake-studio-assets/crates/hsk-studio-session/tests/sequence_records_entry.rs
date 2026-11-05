//! Lane SMB-PR-SEQUENCE-SESSION-1, brief item 1 as the lead decided it (2026-11-05, refuse on read, as
//! multicam): the Folio sequence settings records and preset library persist as the session document
//! entries `studio/sequence_settings.<sha256>` and `studio/sequence_presets.<sha256>`, validated on
//! open, set and read. One bad entry does not lock the file out: the package opens, the entry is
//! reported as a `LossReason::Opaque` note, reading it gives the typed refusal, and save keeps it
//! byte-for-byte. Commits that would use it: tests/sequence_records_commit.rs.
mod sequence_records_support;
use hsk_studio_folio::{Code, shipped_sequence_presets};
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

fn valid_records() -> Vec<u8> {
    serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F25)])).unwrap()
}
fn valid_presets() -> Vec<u8> {
    serde_json::to_vec(&library(vec![preset("mine", F25)])).unwrap()
}
/// `valid_records` with its JSON changed by `edit`.
fn records_json(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(&valid_records()).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}
fn presets_json(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(&valid_presets()).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}

/// Every way an entry body can fail: what it is, its token, its bytes, the Folio code a read refuses with.
fn invalid_cases() -> Vec<(&'static str, &'static str, Vec<u8>, Code)> {
    let s = SEQUENCE_SETTINGS_ENTRY;
    let p = SEQUENCE_PRESETS_ENTRY;
    let duplicate_key = String::from_utf8(valid_records()).unwrap().replacen("{", &format!("{{\"document_id\":\"{}\",", doc_id()), 1);
    vec![
        ("a sequence the document does not hold", s, records_json(|v| v["sequences"][0]["sequence_id"] = json!("seq-x")), Code::DanglingReference),
        ("another document", s, records_json(|v| v["document_id"] = json!(identity("SDOC", 2))), Code::InvalidField),
        ("a frame rate the sequence does not have", s, records_json(|v| v["sequences"][0]["settings"]["frame_rate"]["ticks_per_frame"] = json!(F24)), Code::InvalidField),
        ("an unknown editing mode", s, records_json(|v| v["sequences"][0]["editing_mode"] = json!("no-such-mode")), Code::DanglingReference),
        ("the same sequence twice", s, records_json(|v| {
            let entry = v["sequences"][0].clone();
            v["sequences"].as_array_mut().unwrap().push(entry);
        }), Code::DuplicateIdentity),
        ("a float aspect ratio", s, records_json(|v| v["sequences"][0]["settings"]["pixel_aspect_ratio"]["numerator"] = json!(1.5)), Code::InvalidJson),
        ("a missing settings field", s, records_json(|v| drop(v["sequences"][0]["settings"].as_object_mut().unwrap().remove("immersive"))), Code::InvalidJson),
        ("an immersive view out of range", s, records_json(|v| v["sequences"][0]["settings"]["immersive"]["field_of_vertical_view"] = json!(500.0)), Code::InvalidField),
        ("an unknown field", s, records_json(|v| v["vendor"] = json!(1)), Code::InvalidJson),
        ("a repeated key", s, duplicate_key.into_bytes(), Code::DuplicateField),
        ("no JSON at all", s, b"{not json".to_vec(), Code::InvalidJson),
        ("nesting past the depth budget", s, "[".repeat(64).into_bytes(), Code::BudgetExceeded),
        ("bytes past the input budget", s, vec![b' '; 1_048_577], Code::BudgetExceeded),
        ("a preset set that is no JSON", p, b"\xff\xfe".to_vec(), Code::InvalidJson),
        ("the same preset twice", p, presets_json(|v| {
            let preset = v["presets"][0].clone();
            v["presets"].as_array_mut().unwrap().push(preset);
        }), Code::DuplicateIdentity),
        ("a preset with an invalid record", p, presets_json(|v| v["presets"][0]["settings"]["frame_size"]["width"] = json!(0)), Code::InvalidField),
        ("a preset field unknown", p, presets_json(|v| v["presets"][0]["vendor"] = json!(1)), Code::InvalidJson),
        ("a preset set past the input budget", p, serde_json::to_vec(&library((0..4097).map(|i| preset(&format!("p{i}"), F25)).collect())).unwrap(), Code::BudgetExceeded),
    ]
}

/// The typed read of `entry_token`, as an error.
fn read_refusal(session: &Session, entry_token: &str) -> SessionError {
    if entry_token == SEQUENCE_SETTINGS_ENTRY {
        session.sequence_settings_records(&token()).expect_err("a read of an invalid entry is refused")
    } else {
        session.sequence_presets(&token()).expect_err("a read of an invalid entry is refused")
    }
}

#[test]
fn a_package_without_the_entries_opens_unchanged() {
    bounded_case("no_entries", || {
        assert!(DOCUMENT_ENTRY_TOKENS.contains(&SEQUENCE_SETTINGS_ENTRY) && SEQUENCE_SETTINGS_ENTRY == "sequence_settings");
        assert!(DOCUMENT_ENTRY_TOKENS.contains(&SEQUENCE_PRESETS_ENTRY) && SEQUENCE_PRESETS_ENTRY == "sequence_presets");
        let bytes = package(&[]);
        let (mut session, frames) = open_counted(&bytes);
        assert_eq!(frames.count(), 1);
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        assert!(!session.is_dirty());
        assert_eq!(session.document_revision(), 3);
        assert_eq!(session.snapshot().document().sequences, vec![seq_a(), seq_b()]);
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), None);
        assert_eq!(session.sequence_presets(&token()).unwrap(), None);
        let again = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(again.document_entry(SEQUENCE_SETTINGS_ENTRY), None);
        assert_eq!(again.document_entry(SEQUENCE_PRESETS_ENTRY), None);
    });
}

#[test]
fn valid_entries_open_typed_without_a_loss_note() {
    bounded_case("valid_entries", || {
        let (settings_body, presets_body) = (valid_records(), valid_presets());
        let bytes = package(&[entry(SEQUENCE_SETTINGS_ENTRY, &settings_body), entry(SEQUENCE_PRESETS_ENTRY, &presets_body)]);
        let session = open(&bytes);
        assert!(session.loss().is_empty(), "not an opaque entry any more: {:?}", session.loss());
        assert!(!session.is_dirty());
        assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(settings_body.as_slice()));
        assert_eq!(session.document_entry(SEQUENCE_PRESETS_ENTRY), Some(presets_body.as_slice()));
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(records(vec![settings_entry("seq-a", "custom", F25)])));
        assert_eq!(session.sequence_presets(&token()).unwrap(), Some(library(vec![preset("mine", F25)])));
        // The whole shipped library fits the default budgets as a stored entry.
        let shipped = serde_json::to_vec(&shipped_sequence_presets()).unwrap();
        let with_shipped = open(&package(&[entry(SEQUENCE_PRESETS_ENTRY, &shipped)]));
        assert_eq!(with_shipped.sequence_presets(&token()).unwrap().unwrap().presets.len(), 391);
    });
}

#[test]
fn an_invalid_entry_opens_is_noted_and_refuses_on_read() {
    bounded_case("invalid_entries", || {
        for (what, entry_token, body, code) in invalid_cases() {
            let held = entry(entry_token, &body);
            let bytes = package(std::slice::from_ref(&held));
            let frames = Frames::default();
            let session = try_open(&bytes, &frames).unwrap_or_else(|f| panic!("{what}: a bad entry locked the file out: {f:?}"));
            assert_eq!(frames.count(), 1, "{what}: one Observe frame for the open");
            assert_eq!(session.snapshot().document().sequences, vec![seq_a(), seq_b()], "{what}: the document is whole");
            // A caller sees it without reading the entry, through the existing open-time notes.
            assert_eq!(session.loss(), [LossNote { name: held.name.clone(), reason: LossReason::Opaque }], "{what}");
            assert!(!session.is_dirty(), "{what}");
            // Reading it is the typed refusal, and the bytes are held as they came.
            let refused = read_refusal(&session, entry_token);
            assert_eq!(folio_code(&refused), Some(code), "{what}: {refused:?}");
            assert_eq!(refused.code(), "document_rejected", "{what}");
            assert_eq!(session.document_entry(entry_token), Some(body.as_slice()), "{what}");
            // The other entry is not affected by this one.
            let other = if entry_token == SEQUENCE_SETTINGS_ENTRY { session.sequence_presets(&token()).map(|p| p.is_none()) } else { session.sequence_settings_records(&token()).map(|r| r.is_none()) };
            assert_eq!(other, Ok(true), "{what}");
        }
    });
}

#[test]
fn a_repeated_entry_refuses_on_read_and_a_misnamed_one_stays_opaque() {
    bounded_case("repeated_misnamed", || {
        // Two valid entries of one token: neither is silently chosen, and both are noted.
        let one = entry(SEQUENCE_SETTINGS_ENTRY, &valid_records());
        let two = entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&records(vec![settings_entry("seq-b", "custom", F24)])).unwrap());
        let mut repeated = open(&package(&[one.clone(), two.clone()]));
        assert_eq!(folio_code(&repeated.sequence_settings_records(&token()).unwrap_err()), Some(Code::DuplicateIdentity));
        let mut noted: Vec<&str> = repeated.loss().iter().map(|n| n.name.as_str()).collect();
        noted.sort_unstable();
        let mut names = vec![one.name.as_str(), two.name.as_str()];
        names.sort_unstable();
        assert_eq!(noted, names);
        assert!(repeated.loss().iter().all(|n| n.reason == LossReason::Opaque));
        // Save keeps both, so a reopen sees the same thing.
        let again = open(&repeated.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(folio_code(&again.sequence_settings_records(&token()).unwrap_err()), Some(Code::DuplicateIdentity));
        assert_eq!(again.loss().len(), 2);

        // A name whose digest is not the body's is not a document entry of the token: as before this
        // lane, it opens as an opaque entry with its note, is kept verbatim, and reads as no records.
        let misnamed = |name: String, body: Vec<u8>| Quarantined { name, sha256_hex: sha256_hex_of(&body), bytes: body };
        let wrong = misnamed(format!("{DOCUMENT_ENTRY_PREFIX}{SEQUENCE_PRESETS_ENTRY}.{}", "0".repeat(64)), valid_presets());
        let bare = misnamed(format!("{DOCUMENT_ENTRY_PREFIX}{SEQUENCE_SETTINGS_ENTRY}"), valid_records());
        let longer = misnamed(format!("{DOCUMENT_ENTRY_PREFIX}{SEQUENCE_SETTINGS_ENTRY}_v2.abc"), b"x".to_vec());
        let mut session = open(&package(&[wrong.clone(), bare.clone(), longer.clone()]));
        let mut opaque: Vec<(String, LossReason)> = session.loss().iter().map(|n| (n.name.clone(), n.reason)).collect();
        opaque.sort_by(|a, b| a.0.cmp(&b.0));
        let mut expected: Vec<(String, LossReason)> = [&wrong, &bare, &longer].iter().map(|q| (q.name.clone(), LossReason::Opaque)).collect();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(opaque, expected);
        assert_eq!(session.sequence_presets(&token()).unwrap(), None);
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), None);
        let saved = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(saved.loss().len(), 3, "kept verbatim through a save");
    });
}

#[test]
fn a_canceled_open_is_canceled() {
    bounded_case("canceled_open", || {
        let bytes = package(&[entry(SEQUENCE_SETTINGS_ENTRY, &valid_records())]);
        let canceled = token();
        canceled.cancel();
        match Session::open_bytes(&bytes, SessionConfig::new(actor()), ports(&Frames::default()), &canceled) {
            Ok(_) => panic!("a canceled open succeeded"),
            Err(failed) => assert_eq!(failed.code(), "canceled"),
        }
    });
}

#[test]
fn typed_writes_validate_and_refusals_change_nothing() {
    bounded_case("typed_writes", || {
        let mut session = open(&package(&[]));
        let good = records(vec![settings_entry("seq-a", "custom", F25)]);
        session.set_sequence_settings_records(Some(&good), &token()).unwrap();
        session.set_sequence_presets(Some(&library(vec![preset("mine", F25)])), &token()).unwrap();
        assert!(session.is_dirty());
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(good.clone()));
        assert_eq!(session.document_revision(), 3, "a direct set is not a revision, as every document entry");
        let held = state(&session);

        let dangling = records(vec![settings_entry("seq-x", "custom", F25)]);
        assert_eq!(folio_code(&session.set_sequence_settings_records(Some(&dangling), &token()).unwrap_err()), Some(Code::DanglingReference));
        let wrong_rate = records(vec![settings_entry("seq-a", "custom", F24)]);
        assert_eq!(folio_code(&session.set_sequence_settings_records(Some(&wrong_rate), &token()).unwrap_err()), Some(Code::InvalidField));
        let bad_set = library(vec![preset("mine", F25), preset("mine", F25)]);
        assert_eq!(folio_code(&session.set_sequence_presets(Some(&bad_set), &token()).unwrap_err()), Some(Code::DuplicateIdentity));
        // The untyped entry point refuses what a read would refuse.
        assert_eq!(folio_code(&session.set_document_entry(SEQUENCE_SETTINGS_ENTRY, Some(b"{not json".to_vec()), &token()).unwrap_err()), Some(Code::InvalidJson));
        assert_eq!(folio_code(&session.set_document_entry(SEQUENCE_PRESETS_ENTRY, Some(b"[]".to_vec()), &token()).unwrap_err()), Some(Code::InvalidJson));
        let canceled = token();
        canceled.cancel();
        assert_eq!(session.set_sequence_settings_records(None, &canceled).unwrap_err().code(), "canceled");
        assert_eq!(session.set_sequence_presets(None, &canceled).unwrap_err().code(), "canceled");
        assert_eq!(session.sequence_settings_records(&canceled).unwrap_err().code(), "canceled");
        assert_eq!(session.sequence_presets(&canceled).unwrap_err().code(), "canceled");
        assert_eq!(state(&session), held, "every refusal leaves the entries untouched");

        // Removing an entry removes it from the next save.
        session.set_sequence_settings_records(None, &token()).unwrap();
        session.set_sequence_presets(None, &token()).unwrap();
        let again = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!((again.document_entry(SEQUENCE_SETTINGS_ENTRY), again.document_entry(SEQUENCE_PRESETS_ENTRY)), (None, None));

        // The package limit binds these entries with every other (the bound a reopen enforces).
        let mut small = SessionConfig::new(actor());
        small.budgets.limits.max_entry_bytes = 64 * 1024;
        let mut tight = match Session::open_bytes(&package(&[]), small, ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        let shipped = shipped_sequence_presets();
        assert_eq!(tight.set_sequence_presets(Some(&shipped), &token()).unwrap_err().code(), "total_too_large");
        assert_eq!(tight.document_entry(SEQUENCE_PRESETS_ENTRY), None);
    });
}

#[test]
fn an_explicit_set_replaces_an_invalid_entry_and_its_note() {
    bounded_case("explicit_set", || {
        // Corrupt bytes cannot be quoted by a commit, so this is the way out of them. It is explicit,
        // so it is not a silent drop, and a refused set leaves the bad bytes as they were.
        let bad = b"{not json".to_vec();
        let mut session = open(&package(&[entry(SEQUENCE_SETTINGS_ENTRY, &bad)]));
        assert_eq!(session.loss().len(), 1);
        let dangling = records(vec![settings_entry("seq-x", "custom", F25)]);
        assert!(session.set_sequence_settings_records(Some(&dangling), &token()).is_err());
        assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(bad.as_slice()));
        assert_eq!(session.loss().len(), 1, "still there, so still noted");
        let good = records(vec![settings_entry("seq-a", "custom", F25)]);
        session.set_sequence_settings_records(Some(&good), &token()).unwrap();
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(good.clone()));
        assert!(session.loss().is_empty(), "the note named an entry that is gone: {:?}", session.loss());
        let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
        assert!(reopened.loss().is_empty());
        assert_eq!(reopened.sequence_settings_records(&token()).unwrap(), Some(good));

        // A set replaces every entry of a repeated token, and a removal removes them all.
        let one = entry(SEQUENCE_PRESETS_ENTRY, &valid_presets());
        let two = entry(SEQUENCE_PRESETS_ENTRY, &serde_json::to_vec(&library(vec![preset("other", F25)])).unwrap());
        let mut repeated = open(&package(&[one, two]));
        assert_eq!(repeated.loss().len(), 2);
        repeated.set_sequence_presets(None, &token()).unwrap();
        assert_eq!(repeated.sequence_presets(&token()).unwrap(), None);
        assert!(repeated.loss().is_empty());
        let reopened = open(&repeated.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(reopened.document_entry(SEQUENCE_PRESETS_ENTRY), None);
    });
}
