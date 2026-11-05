//! Lane SMB-PR-SEQUENCE-SESSION-1, brief item 3: settings with immersive and preview (STU-VID-018,
//! STU-VID-019), a preset library (STU-VID-016a) and an editing mode (STU-VID-017) survive save and
//! reopen through the session. Undo after reopen: multicam does it (tests/multicam_commit.rs
//! `committed_multicam_step_survives_save_and_reopen`: an inverse taken before the save still undoes
//! on the reopened document; history itself is not persisted), so it is proven here the same way.
mod sequence_records_support;
use hsk_studio_folio::*;
use hsk_studio_session::*;
use sequence_records_support::*;

/// A session taken through the three commands: `seq-a` holds the immersive `vr-25` settings and a
/// shipped mode, and the library holds `vr-25` and the preset saved back from `seq-a`.
fn authored(session: &mut Session) -> Vec<Applied> {
    let library_step = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("vr-25", F25)])) }));
    let seeded = session.submit_sequence_records(&library_step, session.document_revision(), &token()).unwrap();
    let applied = run(session, &apply("vr-25", "seq-a"), 1).unwrap();
    let moded = run(session, &set_mode("seq-a", SHIPPED_MODE), 2).unwrap();
    let saved = run(session, &save("seq-a", "vr-25-copy"), 3).unwrap();
    vec![seeded, applied, moded, saved]
}

fn expect_authored(session: &Session) {
    let records = session.sequence_settings_records(&token()).unwrap().expect("settings survive");
    let entry = records.entry("seq-a").expect("the entry of seq-a");
    assert_eq!(entry.editing_mode, SHIPPED_MODE, "the editing mode");
    assert_eq!(entry.settings, settings(F25), "the whole record");
    assert_eq!(entry.settings.immersive, Some(immersive()), "all nine immersive fields");
    let preview = &entry.settings.preview_render_format;
    assert_eq!((preview.codec.as_deref(), preview.frame_size), (Some("apcs"), SequenceFrameSize { width: 1920, height: 960 }), "the preview format, its size not the sequence's");
    assert_ne!(preview.frame_size, entry.settings.frame_size);
    let presets = session.sequence_presets(&token()).unwrap().expect("the library survives");
    let ids: Vec<&str> = presets.presets.iter().map(|p| p.preset_id.as_str()).collect();
    assert_eq!(ids, ["vr-25", "vr-25-copy"], "the library, in order");
    assert_eq!(presets.preset("vr-25-copy").unwrap().settings, settings(F25));
}

#[test]
fn settings_library_and_mode_survive_save_and_reopen_through_the_session() {
    bounded_case("save_reopen_file", || {
        let dir = std::env::temp_dir().join(format!("hsk-studio-session-sequence-records-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.handshake");

        let mut session = open(&package(&[]));
        let steps = authored(&mut session);
        assert_eq!(session.document_revision(), 7);
        assert!(session.is_dirty());
        expect_authored(&session);
        let live = state(&session);
        session.save_to(&path, &token()).unwrap();
        assert!(!session.is_dirty(), "the save carried everything");

        let frames = Frames::default();
        let mut reopened = match Session::open_path(&path, SessionConfig::new(actor()), ports(&frames), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert!(reopened.loss().is_empty(), "{:?}", reopened.loss());
        assert_eq!(state(&reopened), live, "sequences, both entries byte-identical, revision");
        expect_authored(&reopened);

        // The inverses taken before the save still undo on the reopened document, newest first, and
        // the last undo is the document the package started as.
        let mut revision = reopened.document_revision();
        for (i, applied) in steps.iter().enumerate().rev() {
            let undo = reopened.submit_sequence_records(&fresh_inverse(applied, "cmd.undo"), revision, &token()).unwrap();
            revision = undo.revision;
            assert!(undo.changed, "step {i}");
        }
        assert_eq!(values(&reopened), (vec![seq_a(), seq_b()], None, None), "back to the start, no entry left");
        reopened.save_to(&path, &token()).unwrap();
        let again = match Session::open_path(&path, SessionConfig::new(actor()), ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert_eq!((again.document_entry(SEQUENCE_SETTINGS_ENTRY), again.document_entry(SEQUENCE_PRESETS_ENTRY)), (None, None), "an undone entry is not saved");
        drop(again);
        drop(reopened);
        drop(session);
        std::fs::remove_dir_all(&dir).unwrap();
    });
}

#[test]
fn a_checkpoint_round_trips_and_a_later_save_keeps_the_bytes() {
    bounded_case("checkpoint", || {
        let mut session = open(&package(&[]));
        authored(&mut session);
        let live = state(&session);
        let checkpoint = session.export_checkpoint(&token()).unwrap();
        let mut first = open(&checkpoint.bytes);
        assert!(first.loss().is_empty(), "{:?}", first.loss());
        assert_eq!(state(&first), live);
        let second = open(&first.export_checkpoint(&token()).unwrap().bytes);
        assert_eq!(state(&second), live, "a reopen and re-save does not touch the entries");
        expect_authored(&second);
    });
}

#[test]
fn the_records_are_part_of_the_document_they_were_saved_with() {
    bounded_case("wrong_document", || {
        // The entries of one document's package do not validate against another document: the file
        // opens, and reading them is refused.
        let mut session = open(&package(&[]));
        authored(&mut session);
        let body = session.document_entry(SEQUENCE_SETTINGS_ENTRY).unwrap().to_vec();
        let other = open(&package_of(&[seq_b()], &[entry(SEQUENCE_SETTINGS_ENTRY, &body)]));
        assert_eq!(other.loss().len(), 1);
        assert_eq!(other.sequence_settings_records(&token()).unwrap_err().code(), "document_rejected");
    });
}

/// Lead decision 2026-11-05: save keeps an invalid entry byte-for-byte unless a successful commit
/// replaces it, and never drops it silently.
#[test]
fn save_keeps_an_invalid_entry_byte_for_byte_until_a_commit_replaces_it() {
    bounded_case("save_keeps_invalid", || {
        let dir = std::env::temp_dir().join(format!("hsk-studio-session-sequence-invalid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.handshake");
        // Valid JSON naming a sequence the document does not hold, and bytes that are not JSON at all.
        let dangling_records = records(vec![settings_entry("seq-x", "custom", F25)]);
        let dangling = serde_json::to_vec(&dangling_records).unwrap();
        let garbage = b"{not json".to_vec();
        let reopen_file = |path: &std::path::Path| match Session::open_path(path, SessionConfig::new(actor()), ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        for (body, presets_body) in [(dangling.clone(), garbage.clone()), (garbage.clone(), serde_json::to_vec(&library(vec![preset("p", F25), preset("p", F25)])).unwrap())] {
            let held = [entry(SEQUENCE_SETTINGS_ENTRY, &body), entry(SEQUENCE_PRESETS_ENTRY, &presets_body)];
            let names = |s: &Session| {
                let mut n: Vec<String> = s.loss().iter().map(|l| l.name.clone()).collect();
                n.sort_unstable();
                n
            };
            let mut expected: Vec<String> = held.iter().map(|q| q.name.clone()).collect();
            expected.sort_unstable();

            // Through the checkpoint path and through the file path, twice.
            let mut session = open(&package(&held));
            let mut checkpointed = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert_eq!(checkpointed.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(body.as_slice()));
            assert_eq!(checkpointed.document_entry(SEQUENCE_PRESETS_ENTRY), Some(presets_body.as_slice()));
            assert_eq!(names(&checkpointed), expected, "the entries come back under the same names, still noted");
            checkpointed.save_to(&path, &token()).unwrap();
            let mut from_file = reopen_file(&path);
            assert_eq!(from_file.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(body.as_slice()));
            assert_eq!(from_file.document_entry(SEQUENCE_PRESETS_ENTRY), Some(presets_body.as_slice()));
            assert_eq!(names(&from_file), expected);

            // A commit that does not use them saves them unchanged too.
            let mut renamed = seq_b();
            renamed.name = "renamed".into();
            let edit = TimelineStep { command_id: "cmd.name".into(), correlation_id: 1, sequence_id: "seq-b".into(), expect: seq_b(), write: renamed.clone() };
            assert_eq!(from_file.submit_timeline(&edit, 3, &token()).unwrap().revision, 4);
            from_file.save_to(&path, &token()).unwrap();
            drop(from_file);
            let after_commit = reopen_file(&path);
            assert_eq!(after_commit.document_revision(), 4);
            assert_eq!(after_commit.snapshot().document().sequences[1], renamed);
            assert_eq!(after_commit.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(body.as_slice()));
            assert_eq!(after_commit.document_entry(SEQUENCE_PRESETS_ENTRY), Some(presets_body.as_slice()));
            drop(after_commit);
            drop(checkpointed);
            // The session that opened them is unchanged by all of this.
            assert_eq!(session.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(body.as_slice()));
        }

        // A successful commit is what replaces it, and the save then holds only the new bytes.
        let mut session = open(&package(&[entry(SEQUENCE_SETTINGS_ENTRY, &dangling)]));
        let good = records(vec![settings_entry("seq-a", "custom", F25)]);
        let repair = step(vec![], Some(RecordEdit { expect: Some(dangling_records), write: Some(good.clone()) }), None);
        assert_eq!(session.submit_sequence_records(&repair, 3, &token()).unwrap().revision, 4);
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        session.save_to(&path, &token()).unwrap();
        let repaired = reopen_file(&path);
        assert!(repaired.loss().is_empty(), "{:?}", repaired.loss());
        assert_eq!(repaired.sequence_settings_records(&token()).unwrap(), Some(good));
        assert_ne!(repaired.document_entry(SEQUENCE_SETTINGS_ENTRY), Some(dangling.as_slice()));
        drop(repaired);
        drop(session);
        std::fs::remove_dir_all(&dir).unwrap();
    });
}
