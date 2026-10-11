use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_chronicle::{
    self as chronicle, Actor, Patch, Prepared, Property, PropertyAddress, PropertyRead,
    PropertyValue, PropertyWrite, RevisionEntry,
};
use hsk_studio_folio::{self as folio, NodeKind, NodeRef, Snapshot};
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};
use hsk_studio_package::{
    AssetSource, Extras, Limits, MemoryAssetSource, Quarantined, WriteOptions, read_package,
    sha256_hex_of, write_package_with,
};
use hsk_studio_session::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

// ----- fixtures ---------------------------------------------------------------------------

fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn address(n: u8, op: &str, port: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":op,"port_key":port})
}
fn layer(n: u8, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true})
}
fn document_json(digest: &str) -> Value {
    let tile = json!({"object_key":"tile-0","layer_id":identity("SLYR",2),"column":0,"row":0,"width":{"value":64,"unit":"px"},"height":{"value":64,"unit":"px"},"format":"caller-selected-format","colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1","content_digest":{"algorithm":"sha256","digest":digest},"artifact_manifest_id":"caller-owned-existing-handle"});
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[{"schema_id":"hsk.studio.artboard@1","artboard_id":identity("SART",1),"name":"board","parent":null,"bounds":{"x":{"value":0,"unit":"mm"},"y":{"value":0,"unit":"mm"},"width":{"value":100,"unit":"mm"},"height":{"value":50,"unit":"mm"}}}],
        "page_spreads":[],
        "layers":[layer(1,"group",json!({"payload_kind":"group"})),layer(2,"raster",json!({"payload_kind":"raster","tiles":[tile]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[
                {"layer_id":identity("SLYR",2),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]},
                {"layer_id":identity("SLYR",1),"operation_key":"mix","operation_kind":"composite","inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]}],
            "edges":[{"source":address(2,"source","out"),"target":address(1,"mix","inputs"),"dependency_kind":"data","input_ordinal":0}],
            "outputs":[address(1,"mix","out")]}
    })
}

struct Granted;
impl folio::Resolver for Granted {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, f: &str) -> bool {
        f == "caller-selected-format"
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

const BLOB: [u8; 6] = [9, 8, 7, 6, 5, 4];
const OPAQUE: [u8; 4] = [1, 2, 3, 4];

/// A real package file in a private temp directory (removed on drop).
struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("hsk-studio-session-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut assets = MemoryAssetSource::new();
        let digest = assets.insert(BLOB.to_vec());
        let snapshot = match folio::inspect_bytes(
            &serde_json::to_vec(&document_json(&digest.digest)).unwrap(),
            folio::Budget::default(),
            &token(),
            &Granted,
        )
        .unwrap()
        {
            folio::Inspection::Editable(s) => s,
            folio::Inspection::ReadOnly(_) => panic!("fixture became read-only"),
        };
        let opaque = Quarantined {
            name: "vendor/extra.bin".to_owned(),
            bytes: OPAQUE.to_vec(),
            sha256_hex: sha256_hex_of(&OPAQUE),
        };
        let mut bytes = Vec::new();
        write_package_with(
            &snapshot,
            &assets as &dyn AssetSource,
            &Extras {
                quarantined: &[opaque],
                previews: &[],
            },
            &WriteOptions::default(),
            &Limits::default(),
            &token(),
            &mut bytes,
        )
        .unwrap();
        let path = dir.join("doc.handshake");
        fs::write(&path, &bytes).unwrap();
        Self { dir, path, bytes }
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[derive(Clone, Default)]
struct Frames(Arc<Mutex<Vec<(DeliveryClass, usize)>>>);
impl Frames {
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    fn all_terminal(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .all(|(class, len)| *class == DeliveryClass::Terminal && *len > 0)
    }
}
struct TestSink(Frames);
impl SinkPort for TestSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.0.lock().unwrap().push((class, bytes.len()));
        Ok(())
    }
}

fn actor_context() -> ActorContext {
    ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .unwrap()
}
fn patch_actor() -> Actor {
    Actor {
        account_id: "account".into(),
        principal_id: "principal".into(),
        owner_account_id: "owner".into(),
        owner_principal_id: "owner-principal".into(),
        access_space_id: "space".into(),
        session_id: "session".into(),
    }
}

fn ports(frames: &Frames) -> SessionPorts {
    SessionPorts::new(Box::new(TestSink(frames.clone()))).with_resolver(Box::new(Granted))
}

fn open(fixture: &Fixture, frames: &Frames) -> Session {
    Session::open_path(
        &fixture.path,
        SessionConfig::new(actor_context()),
        ports(frames),
        &token(),
    )
    .unwrap()
}

fn name_address(layer: u8) -> PropertyAddress {
    PropertyAddress {
        target: NodeRef {
            node_kind: NodeKind::Layer,
            node_id: identity("SLYR", layer),
        },
        property: Property::Name,
    }
}

fn rename_patch(layer: u8, from: &str, to: &str, read_revision: u64, command: &str) -> Patch {
    Patch {
        transport_version: 1,
        document_id: identity("SDOC", 1),
        base_revision: 7,
        command_id: command.into(),
        correlation_id: 1,
        actor: patch_actor(),
        reads: vec![PropertyRead {
            address: name_address(layer),
            expected_revision: read_revision,
            expected_value: PropertyValue::String(from.into()),
        }],
        writes: vec![PropertyWrite {
            address: name_address(layer),
            value: PropertyValue::String(to.into()),
        }],
    }
}

fn json_bytes(patch: &Patch) -> Vec<u8> {
    serde_json::to_vec(patch).unwrap()
}

fn delivered(report: ReportStatus) -> hsk_studio_observe::Receipt {
    match report {
        ReportStatus::Delivered(receipt) => receipt,
        other => panic!("report not delivered: {other:?}"),
    }
}

// ----- tests ----------------------------------------------------------------------------------

#[test]
fn open_edit_undo_save_reopen_round_trip() {
    bounded_case("open_edit_undo_save_reopen_round_trip", || {
        let fixture = Fixture::new("roundtrip");
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);
        assert_eq!(session.document_revision(), 7);
        assert!(!session.is_dirty());
        assert_eq!(session.loss().len(), 1, "the opaque entry is a loss note");
        assert_eq!(
            session.revisions().len(),
            5,
            "2 layers x (name, visible) + artboard name"
        );
        assert!(session.revisions().iter().all(|e| e.revision == 0));
        assert_eq!(
            session.providers().ids().collect::<Vec<_>>(),
            [CHRONICLE_PROVIDER_ID]
        );
        assert_eq!(delivered(session.last_report()).outcome, Outcome::Success);

        let first = session
            .submit(
                CHRONICLE_PROVIDER_ID,
                &json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1")),
                7,
                &token(),
            )
            .unwrap();
        assert_eq!(
            (first.previous_revision, first.revision, first.changed),
            (7, 8, true)
        );
        assert_eq!(
            (
                first.revision_updates[0].previous_revision,
                first.revision_updates[0].revision
            ),
            (0, 1)
        );
        assert!(session.is_dirty());
        assert_eq!(
            session.snapshot().document().layers[0].name,
            "renamed-layer"
        );

        let undo = first
            .inverse
            .clone()
            .unwrap()
            .invocation("undo-1".into(), 2, patch_actor(), 8);
        let second = session
            .submit(CHRONICLE_PROVIDER_ID, &json_bytes(&undo), 8, &token())
            .unwrap();
        assert_eq!((second.previous_revision, second.revision), (8, 9));
        assert_eq!(session.snapshot().document().layers[0].name, "layer-1");

        let third = session
            .submit(
                CHRONICLE_PROVIDER_ID,
                &json_bytes(&rename_patch(1, "layer-1", "final-name", 2, "edit-2")),
                9,
                &token(),
            )
            .unwrap();
        assert_eq!(third.revision, 10);
        assert_eq!(delivered(third.report).outcome, Outcome::Success);

        let saved = session.save(&token()).unwrap();
        assert_eq!((saved.revision, saved.quarantined_preserved), (10, 1));
        assert!(!session.is_dirty());
        assert_eq!(
            fixture.files(),
            ["doc.handshake"],
            "no temp or marker left behind"
        );

        // Independent oracle: package's own reader, not the session.
        let reopened = read_package(
            &fs::read(&fixture.path).unwrap(),
            &Limits::default(),
            folio::Budget::default(),
            &Granted,
            &token(),
        )
        .unwrap();
        let document = reopened.snapshot().unwrap().document();
        assert_eq!(document.revision, 10);
        assert_eq!(document.layers[0].name, "final-name");
        assert_eq!(document.layers[1].name, "layer-2");
        assert_eq!(
            reopened.assets.get(&sha256_hex_of(&BLOB)),
            Some(&BLOB.to_vec())
        );
        assert_eq!(reopened.quarantined.len(), 1);
        assert_eq!(reopened.quarantined[0].bytes, OPAQUE);

        // A second session over the saved file sees revision 10 with fresh property revisions.
        let again = open(&fixture, &Frames::default());
        assert_eq!(again.document_revision(), 10);
        assert!(again.revisions().iter().all(|e| e.revision == 0));

        // open + 3 submits + save: exactly one terminal frame each.
        assert_eq!(session.operations(), 5);
        assert_eq!(frames.count(), 5);
        assert!(frames.all_terminal());
    });
}

#[test]
fn revision_checks_reject_stale_and_conflicting_edits_without_state_change() {
    bounded_case("revision_checks_reject_stale_and_conflicting_edits", || {
        let fixture = Fixture::new("revisions");
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);
        let good = json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1"));
        let mut codes = BTreeSet::new();
        let mut refuse = |session: &mut Session,
                          provider: &str,
                          params: &[u8],
                          expected: u64,
                          outcome: Outcome| {
            let state = |s: &Session| {
                (
                    s.document_revision(),
                    s.revisions().to_vec(),
                    s.snapshot().document().layers[0].name.clone(),
                )
            };
            let before = state(session);
            let failed = session
                .submit(provider, params, expected, &token())
                .unwrap_err();
            assert_eq!(delivered(failed.report).outcome, outcome);
            assert_eq!(
                state(session),
                before,
                "refusal must not change the session"
            );
            codes.insert(failed.code());
            failed.error
        };
        let validation = Outcome::Failure(FailureCode::Validation);
        assert_eq!(
            refuse(&mut session, CHRONICLE_PROVIDER_ID, &good, 6, validation),
            SessionError::StaleRevision {
                expected: 6,
                current: 7
            }
        );
        let wrong_property_revision = json_bytes(&rename_patch(1, "layer-1", "x", 5, "edit-x"));
        assert_eq!(
            refuse(
                &mut session,
                CHRONICLE_PROVIDER_ID,
                &wrong_property_revision,
                7,
                validation
            ),
            SessionError::Provider("revision_conflict")
        );
        let wrong_value = json_bytes(&rename_patch(1, "not-the-name", "x", 0, "edit-y"));
        assert_eq!(
            refuse(
                &mut session,
                CHRONICLE_PROVIDER_ID,
                &wrong_value,
                7,
                validation
            ),
            SessionError::Provider("value_conflict")
        );
        assert_eq!(
            refuse(
                &mut session,
                "no.such.provider",
                &good,
                7,
                Outcome::Failure(FailureCode::Unavailable)
            ),
            SessionError::ProviderUnavailable
        );
        assert!(matches!(
            refuse(
                &mut session,
                CHRONICLE_PROVIDER_ID,
                b"not json",
                7,
                validation
            ),
            SessionError::Provider(_)
        ));
        assert_eq!(
            codes.len(),
            5,
            "every refusal has its own stable code: {codes:?}"
        );

        // The good edit commits once; replaying the same bytes is refused, never applied twice.
        let applied = session
            .submit(CHRONICLE_PROVIDER_ID, &good, 7, &token())
            .unwrap();
        assert_eq!(applied.revision, 8);
        assert_eq!(
            session
                .submit(CHRONICLE_PROVIDER_ID, &good, 8, &token())
                .unwrap_err()
                .error,
            SessionError::Provider("revision_conflict")
        );
        assert_eq!(session.document_revision(), 8);

        // Writing the value the document already holds is a no-change success, not a revision.
        let same = json_bytes(&rename_patch(2, "layer-2", "layer-2", 0, "edit-same"));
        let noop = session
            .submit(CHRONICLE_PROVIDER_ID, &same, 8, &token())
            .unwrap();
        assert!(!noop.changed && noop.revision == 8 && noop.inverse.is_none());

        // One frame per operation, success and failure alike.
        assert_eq!(frames.count() as u64, session.operations());
    });
}

struct CancelAfterPropose;
impl ProposalProvider for CancelAfterPropose {
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let prepared = ChronicleProvider.propose(request)?;
        request.cancel.cancel();
        Ok(prepared)
    }
}
struct CancelBeforePropose;
impl ProposalProvider for CancelBeforePropose {
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        request.cancel.cancel();
        ChronicleProvider.propose(request)
    }
}

#[test]
fn cancellation_budgets_and_hostile_input_leave_files_and_state_intact() {
    bounded_case("cancellation_budgets_and_hostile_input", || {
        let fixture = Fixture::new("cancel");
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);
        session
            .providers_mut()
            .register("test.cancel-after-propose", Box::new(CancelAfterPropose))
            .unwrap();
        session
            .providers_mut()
            .register("test.cancel-before-propose", Box::new(CancelBeforePropose))
            .unwrap();
        let params = json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1"));
        let canceled = Outcome::Canceled;

        // Pre-cancelled, cancelled inside the provider, cancelled after the provider finished
        // but before commit: three different points, all publish nothing.
        for (provider, cancelled_up_front) in [
            (CHRONICLE_PROVIDER_ID, true),
            ("test.cancel-before-propose", false),
            ("test.cancel-after-propose", false),
        ] {
            let t = token();
            if cancelled_up_front {
                t.cancel();
            }
            let failed = session.submit(provider, &params, 7, &t).unwrap_err();
            assert_eq!(failed.error, SessionError::Canceled);
            assert_eq!(delivered(failed.report).outcome, canceled);
            assert_eq!(session.document_revision(), 7);
            assert_eq!(session.snapshot().document().layers[0].name, "layer-1");
            assert!(session.revisions().iter().all(|e| e.revision == 0));
        }

        // Save: a cancelled save writes nothing and the last-good file is byte-identical.
        let t = token();
        t.cancel();
        assert_eq!(session.save(&t).unwrap_err().error, SessionError::Canceled);
        assert_eq!(fs::read(&fixture.path).unwrap(), fixture.bytes);
        assert_eq!(fixture.files(), ["doc.handshake"]);
        // A directory is never a destination.
        assert_eq!(
            session.save_to(&fixture.dir, &token()).unwrap_err().code(),
            "destination_invalid"
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), fixture.bytes);

        // Budget: a value budget below the rename's value bytes is refused by chronicle.
        let mut config = SessionConfig::new(actor_context());
        config.budgets.chronicle.value_bytes = 8;
        let mut small =
            Session::open_path(&fixture.path, config, ports(&frames), &token()).unwrap();
        assert_eq!(
            small
                .submit(CHRONICLE_PROVIDER_ID, &params, 7, &token())
                .unwrap_err()
                .error,
            SessionError::Provider("budget_exceeded")
        );
        let mut tiny_limits = SessionConfig::new(actor_context());
        tiny_limits.budgets.limits.max_entries = 2;
        assert_eq!(
            Session::open_path(&fixture.path, tiny_limits, ports(&frames), &token())
                .err()
                .unwrap()
                .code(),
            "too_many_entries"
        );

        // Hostile or damaged input: typed refusal, no panic, no frame (document unknown).
        let quiet = Frames::default();
        for bytes in [
            b"not a package".to_vec(),
            fixture.bytes[..fixture.bytes.len() / 2].to_vec(),
        ] {
            let failed = Session::open_bytes(
                &bytes,
                SessionConfig::new(actor_context()),
                ports(&quiet),
                &token(),
            )
            .err()
            .unwrap();
            assert_eq!(
                failed.report,
                ReportStatus::Unaddressed,
                "{}",
                failed.code()
            );
        }
        assert_eq!(quiet.count(), 0);
        let precancelled = token();
        precancelled.cancel();
        assert_eq!(
            Session::open_path(
                &fixture.path,
                SessionConfig::new(actor_context()),
                ports(&quiet),
                &precancelled
            )
            .err()
            .unwrap()
            .error,
            SessionError::Canceled
        );
        // In-memory sessions have no origin to save to.
        let mut memory = Session::open_bytes(
            &fixture.bytes,
            SessionConfig::new(actor_context()),
            ports(&quiet),
            &token(),
        )
        .unwrap();
        assert_eq!(memory.save(&token()).unwrap_err().code(), "no_destination");
        assert_eq!(quiet.count(), 2, "open + refused save, one frame each");
    });
}

struct RenameFirstLayer;
impl ProposalProvider for RenameFirstLayer {
    fn summary(&self) -> &'static str {
        "test provider: renames the first layer to the UTF-8 params"
    }
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let to = std::str::from_utf8(request.params)
            .map_err(|_| ProviderError::Rejected("params_not_utf8"))?;
        let from = request.snapshot.document().layers[0].name.clone();
        let revision = request
            .revisions
            .iter()
            .find(|e| e.address == name_address(1))
            .map_or(0, |e| e.revision);
        let patch = rename_patch(1, &from, to, revision, "provider-rename");
        chronicle::prepare(
            &patch,
            request.snapshot,
            request.revisions,
            request.budget,
            request.cancel,
            request.resolver,
        )
        .map_err(provider_error)
    }
}

/// Proposes against a snapshot captured earlier, whatever the session now holds.
struct ReplayAgainstOldSnapshot {
    snapshot: Snapshot,
    revisions: Vec<RevisionEntry>,
}
impl ProposalProvider for ReplayAgainstOldSnapshot {
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let patch = rename_patch(1, "layer-1", "from-the-past", 0, "replay");
        chronicle::prepare(
            &patch,
            &self.snapshot,
            &self.revisions,
            request.budget,
            request.cancel,
            request.resolver,
        )
        .map_err(provider_error)
    }
}

#[test]
fn providers_are_instance_owned_enumerable_and_not_trusted() {
    bounded_case(
        "providers_are_instance_owned_enumerable_and_not_trusted",
        || {
            let fixture = Fixture::new("providers");
            let mut a = open(&fixture, &Frames::default());
            let b_frames = Frames::default();
            let mut b = open(&fixture, &b_frames);

            a.providers_mut()
                .register("test.rename-first", Box::new(RenameFirstLayer))
                .unwrap();
            let replay = ReplayAgainstOldSnapshot {
                snapshot: a.snapshot().clone(),
                revisions: a.revisions().to_vec(),
            };
            a.providers_mut()
                .register("test.replay-old", Box::new(replay))
                .unwrap();
            assert_eq!(
                a.providers().ids().collect::<Vec<_>>(),
                [
                    CHRONICLE_PROVIDER_ID,
                    "test.rename-first",
                    "test.replay-old"
                ]
            );
            let listed = a.providers().list();
            assert_eq!(listed.len(), 3);
            assert!(listed[1].summary.contains("renames the first layer"));
            assert_eq!(
                a.providers()
                    .get("test.rename-first")
                    .map(|p| p.summary().is_empty()),
                Some(false)
            );
            assert_eq!(
                a.providers_mut()
                    .register("test.rename-first", Box::new(RenameFirstLayer)),
                Err(RegistryError::DuplicateId)
            );
            assert_eq!(
                a.providers_mut()
                    .register("Bad Id", Box::new(RenameFirstLayer)),
                Err(RegistryError::InvalidId)
            );

            // Provider computed on the live snapshot commits.
            let applied = a
                .submit("test.rename-first", b"from-provider", 7, &token())
                .unwrap();
            assert_eq!(
                (
                    applied.revision,
                    a.snapshot().document().layers[0].name.as_str()
                ),
                (8, "from-provider")
            );

            // A provider that computed on an older snapshot is refused by the session.
            let stale = a.submit("test.replay-old", b"", 8, &token()).unwrap_err();
            assert_eq!(
                stale.error,
                SessionError::StaleRevision {
                    expected: 7,
                    current: 8
                }
            );
            assert_eq!(a.document_revision(), 8);
            assert_eq!(a.snapshot().document().layers[0].name, "from-provider");

            // No global state: the other session never saw the registrations or the edit.
            assert_eq!(
                b.providers().ids().collect::<Vec<_>>(),
                [CHRONICLE_PROVIDER_ID]
            );
            assert_eq!(
                b.submit("test.rename-first", b"x", 7, &token())
                    .unwrap_err()
                    .error,
                SessionError::ProviderUnavailable
            );
            assert_eq!(b.document_revision(), 7);
            assert_eq!(b_frames.count(), 2);
        },
    );
}
