use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_chronicle::{
    self as chronicle, Actor, Patch, Prepared, Property, PropertyAddress, PropertyRead,
    PropertyValue, PropertyWrite, RevisionEntry,
};
use hsk_studio_folio::{self as folio, NodeKind, NodeRef, Snapshot};
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};
use hsk_studio_package::{
    AssetSource, Extras, Limits, MemoryAssetSource, PixelFormat, PreviewImage, PreviewInfo,
    Quarantined, WriteOptions, list_previews, read_package, read_preview, sha256_hex_of,
    write_package_with,
};
use hsk_studio_session::*;
use serde_json::{Value, json};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

// ----- per-thread allocation accounting (memory bound assertions) -----------------------------
// Each test body runs on its own thread, so live/peak bytes are attributable to that body alone.

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

struct Tracking;
// SAFETY: every method forwards to the system allocator unchanged; the counters only observe.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        track(-(layout.size() as isize));
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            track(new_size as isize - layout.size() as isize);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

/// Live and peak bytes allocated by the current thread since `probe()`.
struct Probe {
    base: isize,
}
fn probe() -> Probe {
    let base = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(base));
    Probe { base }
}
impl Probe {
    fn peak(&self) -> usize {
        (PEAK.with(Cell::get) - self.base).max(0) as usize
    }
    fn retained(&self) -> usize {
        (LIVE.with(Cell::get) - self.base).max(0) as usize
    }
}

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
const PREVIEW_PIXELS: [u8; 12] = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];

fn preview_image() -> PreviewImage {
    PreviewImage {
        info: PreviewInfo {
            format: PixelFormat::Rgb8,
            width: 2,
            height: 2,
            color: "srgb".to_owned(),
        },
        pixels: PREVIEW_PIXELS.to_vec(),
    }
}

/// A real package file in a private temp directory (removed on drop).
struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with_blob(name, BLOB.to_vec())
    }

    fn with_blob(name: &str, blob: Vec<u8>) -> Self {
        let dir =
            std::env::temp_dir().join(format!("hsk-studio-session-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut assets = MemoryAssetSource::new();
        let digest = assets.insert(blob);
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
                previews: &[preview_image()],
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

// ----- slice 2: injected ports; the doubles really enforce epoch, revision and idempotency --

#[derive(Clone, Default)]
struct TestClock(Arc<AtomicU64>);
impl TestClock {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct GrantState {
    calls: Vec<Action>,
    revoked: bool,
}
#[derive(Clone, Default)]
struct Grants(Arc<Mutex<GrantState>>);
impl Grants {
    fn calls(&self) -> Vec<Action> {
        self.0.lock().unwrap().calls.clone()
    }
    fn set_revoked(&self, revoked: bool) {
        self.0.lock().unwrap().revoked = revoked;
    }
}
impl GrantPort for Grants {
    fn check(
        &self,
        actor: &ActorContext,
        resource: &DomainId,
        action: Action,
    ) -> Result<GrantToken, GrantError> {
        assert_eq!(actor.principal_id(), "principal");
        assert_eq!(resource.as_str(), identity("SDOC", 1));
        let mut state = self.0.lock().unwrap();
        state.calls.push(action);
        if state.revoked {
            Err(GrantError::Revoked)
        } else {
            Ok(GrantToken(state.calls.len() as u64))
        }
    }
}

#[derive(Default)]
struct JobState {
    admitted: u32,
    released: u32,
}
#[derive(Clone, Default)]
struct Jobs(Arc<Mutex<JobState>>);
impl Jobs {
    fn counts(&self) -> (u32, u32) {
        let state = self.0.lock().unwrap();
        (state.admitted, state.released)
    }
}
impl JobPort for Jobs {
    fn admit(&self, scope: &JobScope<'_>, cost: Cost) -> Result<JobTicket, JobRefused> {
        assert_eq!((scope.action, cost.jobs), (Action::Edit, 1));
        let mut state = self.0.lock().unwrap();
        state.admitted += 1;
        Ok(JobTicket(u64::from(state.admitted)))
    }
    fn release(&self, _: JobTicket) {
        self.0.lock().unwrap().released += 1;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    /// Commit durably, then answer Unknown (lost acknowledgement). One shot.
    LoseAck,
    /// Answer Unknown without committing (request lost). One shot.
    DropBeforeCommit,
}
struct AuthState {
    revision: u64,
    epoch: u64,
    sequence: u64,
    mode: Mode,
    receipts: BTreeMap<String, CommitReceipt>,
    documents: Vec<Vec<u8>>,
    commit_calls: u32,
    unreachable: bool,
    lookup_cost_ms: u64,
    clock: TestClock,
    cancel_in_commit: Option<CancellationToken>,
}
#[derive(Clone)]
struct Authority(Arc<Mutex<AuthState>>);
impl Authority {
    fn new(revision: u64, epoch: u64, clock: &TestClock) -> Self {
        Self(Arc::new(Mutex::new(AuthState {
            revision,
            epoch,
            sequence: 0,
            mode: Mode::Normal,
            receipts: BTreeMap::new(),
            documents: Vec::new(),
            commit_calls: 0,
            unreachable: false,
            lookup_cost_ms: 0,
            clock: clock.clone(),
            cancel_in_commit: None,
        })))
    }
    fn with<T>(&self, f: impl FnOnce(&mut AuthState) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
    /// Durable commits.
    fn commits(&self) -> usize {
        self.with(|s| s.documents.len())
    }
    fn commit_calls(&self) -> u32 {
        self.with(|s| s.commit_calls)
    }
    fn last_document(&self) -> Vec<u8> {
        self.with(|s| s.documents.last().cloned().unwrap())
    }
}
impl CommitPort for Authority {
    fn commit(&self, r: &CommitRequest<'_>) -> CommitOutcome {
        let mut s = self.0.lock().unwrap();
        s.commit_calls += 1;
        if r.epoch != s.epoch {
            return CommitOutcome::StaleEpoch;
        }
        if let Some(existing) = s.receipts.get(r.key.as_str()) {
            return if existing.binding == r.binding {
                CommitOutcome::Committed(existing.clone())
            } else {
                CommitOutcome::Rejected("idempotency_conflict")
            };
        }
        if r.expected_revision != s.revision {
            return CommitOutcome::Stale {
                current_revision: s.revision,
            };
        }
        if s.mode == Mode::DropBeforeCommit {
            s.mode = Mode::Normal;
            return CommitOutcome::Unknown;
        }
        s.revision = r.successor_revision;
        s.sequence += 1;
        let receipt = CommitReceipt {
            revision: r.successor_revision,
            sequence: s.sequence,
            binding: r.binding.to_owned(),
        };
        s.receipts
            .insert(r.key.as_str().to_owned(), receipt.clone());
        s.documents.push(r.successor_document.to_vec());
        if let Some(token) = &s.cancel_in_commit {
            token.cancel();
        }
        if s.mode == Mode::LoseAck {
            s.mode = Mode::Normal;
            return CommitOutcome::Unknown;
        }
        CommitOutcome::Committed(receipt)
    }

    fn lookup(&self, key: &IdempotencyKey) -> Lookup {
        let s = self.0.lock().unwrap();
        s.clock.advance(s.lookup_cost_ms);
        if s.unreachable {
            return Lookup::Unknown;
        }
        match s.receipts.get(key.as_str()) {
            Some(receipt) => Lookup::Found(receipt.clone()),
            None => Lookup::Absent,
        }
    }
}

const ID: &str = CHRONICLE_PROVIDER_ID;

fn key(text: &str) -> IdempotencyKey {
    IdempotencyKey::parse(text).unwrap()
}

fn document_of(bytes: &[u8]) -> Snapshot {
    match folio::inspect_bytes(bytes, folio::Budget::default(), &token(), &Granted).unwrap() {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("authority document became read-only"),
    }
}

struct Rig {
    fixture: Fixture,
    frames: Frames,
    clock: TestClock,
    grants: Grants,
    authority: Authority,
    jobs: Jobs,
    admission: Admission,
}
impl Rig {
    fn new(name: &str, capacity: u32) -> Self {
        let clock = TestClock::default();
        Self {
            fixture: Fixture::new(name),
            frames: Frames::default(),
            grants: Grants::default(),
            authority: Authority::new(7, 1, &clock),
            jobs: Jobs::default(),
            admission: Admission::new(capacity, 1 << 20, 25).unwrap(),
            clock,
        }
    }
    fn session(&self) -> Session {
        let mut config = SessionConfig::new(actor_context());
        config.epoch = 1;
        let ports = ports(&self.frames)
            .with_grants(Box::new(self.grants.clone()))
            .with_commits(Box::new(self.authority.clone()))
            .with_jobs(Arc::new(self.jobs.clone()))
            .with_admission(self.admission.clone())
            .with_clock(Box::new(self.clock.clone()));
        Session::open_path(&self.fixture.path, config, ports, &token()).unwrap()
    }
    /// Observe frames, operations and Job Runtime tickets all balance.
    fn assert_balanced(&self, session: &Session) {
        assert_eq!(self.frames.count() as u64, session.operations());
        let (admitted, released) = self.jobs.counts();
        assert_eq!(admitted, released, "every admitted job ticket is released");
        assert_eq!(self.admission.in_flight(), (0, 0));
    }
}

#[test]
fn keyed_submit_replays_the_canonical_receipt_and_rechecks_grants() {
    bounded_case("keyed_submit_replays_the_canonical_receipt", || {
        let rig = Rig::new("keyed", 2);
        let mut session = rig.session();
        let params = json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1"));
        let (k1, k2, k3) = (key("edit-0001"), key("edit-0002"), key("edit-0003"));
        let mut codes = BTreeSet::new();
        let mut note = |failed: Failed| {
            codes.insert(failed.code());
            failed.error
        };

        // A commit authority needs a caller-supplied key: this crate never mints identifiers.
        assert_eq!(
            note(session.submit(ID, &params, 7, &token()).unwrap_err()),
            SessionError::KeyRequired
        );
        assert_eq!(IdempotencyKey::parse(""), Err(SessionError::InvalidKey));
        assert_eq!(IdempotencyKey::parse("a\nb"), Err(SessionError::InvalidKey));

        let first = session.submit_keyed(&k1, ID, &params, 7, &token()).unwrap();
        assert!(!first.replayed && !first.cancel_too_late);
        assert_eq!((first.previous_revision, first.revision), (7, 8));
        let receipt = first.commit.clone().unwrap();
        assert_eq!((receipt.revision, receipt.sequence), (8, 1));
        // Independent oracle: the authority's own durable copy, parsed by folio.
        let durable = document_of(&rig.authority.last_document());
        assert_eq!(durable.document().layers[0].name, "renamed-layer");
        assert_eq!(durable.document().revision, 8);
        assert_eq!(durable.encoded_bytes(), session.snapshot().encoded_bytes());
        assert_eq!(rig.grants.calls(), [Action::Edit]);

        // Identical authorized retry: same canonical receipt, nothing re-applied, fresh grant
        // check, no admission or job slot consumed.
        let retry = session.submit_keyed(&k1, ID, &params, 7, &token()).unwrap();
        assert!(retry.replayed);
        assert_eq!(retry.commit, first.commit);
        assert_eq!((retry.revision, session.document_revision()), (8, 8));
        assert_eq!(rig.authority.commits(), 1);
        assert_eq!(rig.authority.commit_calls(), 1);
        assert_eq!(rig.grants.calls(), [Action::Edit, Action::Edit]);
        assert_eq!(rig.jobs.counts(), (1, 1));

        // Same key, anything else changed: conflict.
        let other = json_bytes(&rename_patch(1, "layer-1", "other", 0, "edit-1"));
        assert_eq!(
            note(
                session
                    .submit_keyed(&k1, ID, &other, 7, &token())
                    .unwrap_err()
            ),
            SessionError::IdempotencyConflict
        );
        assert_eq!(
            note(
                session
                    .submit_keyed(&k1, ID, &params, 8, &token())
                    .unwrap_err()
            ),
            SessionError::IdempotencyConflict
        );

        // A receipt does not outlive the grant: revoked on retry means refused.
        rig.grants.set_revoked(true);
        assert_eq!(
            note(
                session
                    .submit_keyed(&k1, ID, &params, 7, &token())
                    .unwrap_err()
            ),
            SessionError::GrantRevoked
        );
        rig.grants.set_revoked(false);

        // Stale expected revision, and a worker fenced by a newer epoch.
        assert_eq!(
            note(
                session
                    .submit_keyed(&k2, ID, &params, 7, &token())
                    .unwrap_err()
            ),
            SessionError::StaleRevision {
                expected: 7,
                current: 8
            }
        );
        rig.authority.with(|s| s.epoch = 2);
        let second = json_bytes(&rename_patch(2, "layer-2", "second", 0, "edit-3"));
        assert_eq!(
            note(
                session
                    .submit_keyed(&k3, ID, &second, 8, &token())
                    .unwrap_err()
            ),
            SessionError::StaleEpoch
        );
        assert_eq!(session.document_revision(), 8);
        assert_eq!(session.snapshot().document().layers[1].name, "layer-2");
        assert_eq!((rig.authority.commits(), session.receipt_count()), (1, 1));
        assert_eq!(codes.len(), 5, "distinct stable codes: {codes:?}");
        rig.assert_balanced(&session);
    });
}

#[test]
fn lost_acknowledgement_is_reconciled_and_applied_exactly_once() {
    bounded_case("lost_acknowledgement_is_reconciled_exactly_once", || {
        let rig = Rig::new("reconcile", 2);
        let mut session = rig.session();
        let first = json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1"));
        let (lost, other, dropped) = (key("edit-lost"), key("edit-other"), key("edit-dropped"));

        // Durable commit, lost acknowledgement.
        rig.authority.with(|s| s.mode = Mode::LoseAck);
        let failed = session
            .submit_keyed(&lost, ID, &first, 7, &token())
            .unwrap_err();
        assert_eq!(failed.error, SessionError::OutcomeUnknown);
        assert_eq!(
            delivered(failed.report).outcome,
            Outcome::Failure(FailureCode::Unavailable)
        );
        assert_eq!(
            session.document_revision(),
            7,
            "not adopted before the receipt"
        );
        assert_eq!(rig.authority.commits(), 1, "the authority did commit");
        assert_eq!(session.pending(), ["edit-lost"]);

        // Nothing else is submitted on top of an unresolved outcome.
        let second = json_bytes(&rename_patch(2, "layer-2", "second", 0, "edit-2"));
        assert_eq!(
            session
                .submit_keyed(&other, ID, &second, 7, &token())
                .unwrap_err()
                .error,
            SessionError::ReconciliationRequired
        );
        assert_eq!(rig.authority.commit_calls(), 1);

        // Reconcile finds the receipt and adopts it once.
        let report = session.reconcile(&token()).unwrap();
        assert_eq!(
            (
                report.adopted,
                report.cleared,
                report.still_unknown,
                report.diverged
            ),
            (1, 0, 0, 0)
        );
        assert_eq!(delivered(report.report).outcome, Outcome::Success);
        assert_eq!(session.document_revision(), 8);
        assert_eq!(
            session.snapshot().document().layers[0].name,
            "renamed-layer"
        );
        assert_eq!(
            session.snapshot().encoded_bytes(),
            rig.authority.last_document()
        );
        assert!(session.pending().is_empty());

        // Retrying the same key replays the receipt; the authority is not asked again.
        let retry = session
            .submit_keyed(&lost, ID, &first, 7, &token())
            .unwrap();
        assert!(retry.replayed);
        assert_eq!(retry.commit.map(|r| r.sequence), Some(1));
        assert_eq!(
            (rig.authority.commits(), rig.authority.commit_calls()),
            (1, 1)
        );

        // Request lost before the authority saw it: the retry looks it up, finds nothing and
        // only then commits, once.
        rig.authority.with(|s| s.mode = Mode::DropBeforeCommit);
        let failed = session
            .submit_keyed(&dropped, ID, &second, 8, &token())
            .unwrap_err();
        assert_eq!(failed.error, SessionError::OutcomeUnknown);
        assert_eq!(rig.authority.commits(), 1, "nothing was committed");
        let retried = session
            .submit_keyed(&dropped, ID, &second, 8, &token())
            .unwrap();
        assert!(!retried.replayed);
        assert_eq!((retried.previous_revision, retried.revision), (8, 9));
        assert_eq!(rig.authority.commits(), 2);
        assert_eq!(
            session.snapshot().encoded_bytes(),
            rig.authority.last_document()
        );
        assert!(session.pending().is_empty());
        rig.assert_balanced(&session);
    });
}

#[test]
fn admission_overload_cancel_after_commit_and_shutdown_drain() {
    bounded_case("admission_overload_cancel_after_commit_drain", || {
        let rig = Rig::new("drain", 1);
        let mut session = rig.session();
        session
            .providers_mut()
            .register("test.cancel-after-propose", Box::new(CancelAfterPropose))
            .unwrap();
        let first = json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1"));
        let second = json_bytes(&rename_patch(2, "layer-2", "second", 0, "edit-2"));

        // Capacity 1 and the slot is held elsewhere: refused at once, nothing started.
        let held = rig.admission.try_acquire(10).unwrap();
        let failed = session
            .submit_keyed(&key("edit-1"), ID, &first, 7, &token())
            .unwrap_err();
        assert_eq!(
            failed.error,
            SessionError::Overloaded { retry_after_ms: 25 }
        );
        assert_eq!(
            delivered(failed.report).outcome,
            Outcome::Failure(FailureCode::Unavailable)
        );
        assert_eq!(
            (rig.authority.commit_calls(), rig.jobs.counts()),
            (0, (0, 0))
        );
        drop(held);

        // Released slot: the same key now succeeds (the refusal recorded nothing).
        session
            .submit_keyed(&key("edit-1"), ID, &first, 7, &token())
            .unwrap();
        assert_eq!(rig.admission.in_flight(), (0, 0));

        // Error and cancel exits release the permit and the job ticket too.
        let wrong = json_bytes(&rename_patch(1, "not-the-name", "x", 1, "edit-x"));
        assert_eq!(
            session
                .submit_keyed(&key("edit-x"), ID, &wrong, 8, &token())
                .unwrap_err()
                .error,
            SessionError::Provider("value_conflict")
        );
        let t = token();
        let failed = session
            .submit_keyed(&key("edit-c"), "test.cancel-after-propose", &second, 8, &t)
            .unwrap_err();
        assert_eq!(failed.error, SessionError::Canceled);
        assert_eq!(delivered(failed.report).outcome, Outcome::Canceled);
        assert_eq!(rig.authority.commit_calls(), 1, "canceled before commit");
        assert_eq!(rig.admission.in_flight(), (0, 0));
        assert_eq!(rig.jobs.counts(), (3, 3));

        // Cancel that lands while the authority is committing cannot erase the edit.
        let late = token();
        rig.authority
            .with(|s| s.cancel_in_commit = Some(late.clone()));
        let applied = session
            .submit_keyed(&key("edit-late"), ID, &second, 8, &late)
            .unwrap();
        rig.authority.with(|s| s.cancel_in_commit = None);
        assert!(applied.cancel_too_late && applied.commit.is_some());
        assert_eq!(session.document_revision(), 9);
        assert_eq!(delivered(applied.report).outcome, Outcome::Success);

        // Shutdown with an unresolved outcome and an unreachable authority: drain closes
        // admission, stays inside its deadline on the test clock and reports what is left.
        rig.authority.with(|s| {
            s.mode = Mode::LoseAck;
            s.lookup_cost_ms = 1500;
        });
        let again = json_bytes(&rename_patch(1, "renamed-layer", "final", 1, "edit-5"));
        assert_eq!(
            session
                .submit_keyed(&key("edit-5"), ID, &again, 9, &token())
                .unwrap_err()
                .error,
            SessionError::OutcomeUnknown
        );
        rig.authority.with(|s| s.unreachable = true);
        let drained = session.drain(2000).unwrap();
        assert_eq!(drained.abandoned, ["edit-5"]);
        assert_eq!(
            (drained.adopted, drained.elapsed_ms, drained.deadline_met),
            (0, 1500, true)
        );
        assert_eq!(
            delivered(drained.report).outcome,
            Outcome::Failure(FailureCode::Unavailable)
        );
        assert!(session.is_closed());
        assert_eq!(
            session
                .submit_keyed(&key("edit-6"), ID, &again, 9, &token())
                .unwrap_err()
                .error,
            SessionError::Closed
        );

        // Authority reachable again: reconcile adopts it, the next drain is clean, and a final
        // save is still allowed on a closed session.
        rig.authority.with(|s| s.unreachable = false);
        assert_eq!(session.reconcile(&token()).unwrap().adopted, 1);
        assert_eq!(session.document_revision(), 10);
        let drained = session.drain(2000).unwrap();
        assert!(drained.abandoned.is_empty() && drained.deadline_met);
        assert_eq!(delivered(drained.report).outcome, Outcome::Success);
        assert_eq!(session.save(&token()).unwrap().revision, 10);
        assert_eq!(rig.grants.calls().last(), Some(&Action::Save));
        rig.assert_balanced(&session);

        // An operation larger than the whole budget is refused outright, not made to wait.
        let tiny = Admission::new(1, 16, 10).unwrap();
        let mut small = Session::open_path(
            &rig.fixture.path,
            SessionConfig::new(actor_context()),
            ports(&Frames::default()).with_admission(tiny.clone()),
            &token(),
        )
        .unwrap();
        assert_eq!(
            small.submit(ID, &first, 10, &token()).unwrap_err().code(),
            "cost_exceeds_budget"
        );
        assert_eq!(tiny.in_flight(), (0, 0));
    });
}

#[test]
fn export_checkpoint_is_verified_and_previews_follow_the_revision() {
    bounded_case("export_checkpoint_verified_previews", || {
        let fixture = Fixture::new("export");
        let frames = Frames::default();
        let grants = Grants::default();
        let mut session = Session::open_path(
            &fixture.path,
            SessionConfig::new(actor_context()),
            ports(&frames).with_grants(Box::new(grants.clone())),
            &token(),
        )
        .unwrap();

        let cp = session.export_checkpoint(&token()).unwrap();
        assert_eq!(
            (cp.revision, cp.previews_preserved, cp.previews_dropped),
            (7, 1, 0)
        );
        assert_eq!(cp.sha256_hex, sha256_hex_of(&cp.bytes));
        let limits = Limits::default();
        let reread = read_package(
            &cp.bytes,
            &limits,
            folio::Budget::default(),
            &Granted,
            &token(),
        )
        .unwrap();
        assert_eq!(reread.snapshot().unwrap().document().revision, 7);
        assert_eq!(reread.quarantined[0].bytes, OPAQUE);
        let previews = list_previews(&cp.bytes, &limits, &token()).unwrap();
        assert_eq!(previews.len(), 1);
        let image = read_preview(&cp.bytes, &limits, &previews[0].name, &token()).unwrap();
        assert_eq!(image.pixels, PREVIEW_PIXELS);
        assert_eq!(image.info, preview_image().info);

        // An unedited save keeps the preview; the file is read back independently.
        let saved = session.save(&token()).unwrap();
        assert_eq!((saved.previews_preserved, saved.previews_dropped), (1, 0));
        let on_disk = fs::read(&fixture.path).unwrap();
        assert_eq!(list_previews(&on_disk, &limits, &token()).unwrap().len(), 1);

        // After an edit the preview would describe the wrong content: dropped and counted,
        // while the earlier checkpoint stays the immutable revision 7 it was.
        let params = json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1"));
        session.submit(ID, &params, 7, &token()).unwrap();
        let later = session.export_checkpoint(&token()).unwrap();
        assert_eq!(
            (
                later.revision,
                later.previews_preserved,
                later.previews_dropped
            ),
            (8, 0, 1)
        );
        assert_ne!(later.sha256_hex, cp.sha256_hex);
        assert!(
            list_previews(&later.bytes, &limits, &token())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            read_package(
                &cp.bytes,
                &limits,
                folio::Budget::default(),
                &Granted,
                &token()
            )
            .unwrap()
            .snapshot()
            .unwrap()
            .document()
            .revision,
            7
        );

        // Export and save are granted per operation; a refusal touches nothing.
        grants.set_revoked(true);
        assert_eq!(
            session.export_checkpoint(&token()).unwrap_err().error,
            SessionError::GrantRevoked
        );
        assert_eq!(
            session.save(&token()).unwrap_err().error,
            SessionError::GrantRevoked
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), on_disk);
        assert_eq!(
            grants.calls(),
            [
                Action::Export,
                Action::Save,
                Action::Edit,
                Action::Export,
                Action::Export,
                Action::Save
            ]
        );
        assert_eq!(frames.count() as u64, session.operations());
    });
}

// ----- slice 3: lazy package source and host-supplied previews --------------------------------

const BIG: usize = 4 * 1024 * 1024;

fn big_blob() -> Vec<u8> {
    let mut x: u32 = 12345;
    (0..BIG)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

fn gray_preview(side: u32, fill: u8) -> PreviewImage {
    PreviewImage {
        info: PreviewInfo {
            format: PixelFormat::Gray8,
            width: side,
            height: side,
            color: "srgb".to_owned(),
        },
        pixels: vec![fill; (side * side) as usize],
    }
}

fn independent(path: &PathBuf) -> hsk_studio_package::Opened {
    read_package(
        &fs::read(path).unwrap(),
        &Limits::default(),
        folio::Budget::default(),
        &Granted,
        &token(),
    )
    .unwrap()
}

#[test]
fn lazy_source_holds_no_assets_and_save_streams_them_on_demand() {
    bounded_case("lazy_source_holds_no_assets_and_save_streams", || {
        let blob = big_blob();
        let fixture = Fixture::with_blob("lazy", blob.clone());
        let total = fixture.bytes.len() as u64;
        let frames = Frames::default();

        // Open verifies the whole archive in bounded chunks and keeps none of the 8 MiB asset.
        let mark = probe();
        let mut session = open(&fixture, &frames);
        let (open_peak, open_retained) = (mark.peak(), mark.retained());
        assert!(
            open_peak < BIG / 2,
            "open peak {open_peak} must stay far below the asset"
        );
        assert!(open_retained < BIG / 16, "open retained {open_retained}");
        assert!(session.resident_bytes() < 64 * 1024);
        let read_open = session.source_bytes_read();
        assert!(
            (total - 64 * 1024..=total + 64 * 1024).contains(&read_open),
            "one verification pass over the archive: read {read_open} of {total}"
        );

        // An edit touches no asset bytes at all.
        let params = json_bytes(&rename_patch(1, "layer-1", "renamed-layer", 0, "edit-1"));
        let mark = probe();
        session.submit(ID, &params, 7, &token()).unwrap();
        assert_eq!(session.source_bytes_read(), read_open);
        assert!(mark.peak() < 256 * 1024 && mark.retained() < 256 * 1024);

        // Save pulls the asset from the source on demand: read once, then one verification pass
        // over the written file; nothing is read twice and nothing stays resident afterwards.
        let before = session.source_bytes_read();
        let mark = probe();
        let saved = session.save(&token()).unwrap();
        let (save_peak, save_retained) = (mark.peak(), mark.retained());
        let save_read = session.source_bytes_read() - before;
        eprintln!(
            "measured: package {total} asset {BIG} | open peak {open_peak} retained {open_retained} read {read_open} | save peak {save_peak} retained {save_retained} read {save_read}"
        );
        assert!(
            save_read >= BIG as u64,
            "the asset was read from the source: {save_read}"
        );
        assert!(
            save_read <= BIG as u64 + total + 64 * 1024,
            "asset once + one verification pass, read {save_read}"
        );
        // Package's writer collects every asset before writing and save_atomic takes whole
        // bytes (output buffer + read-back), so one save costs a few copies of the package
        // (measured 4.1x here). That is the ceiling: the session itself adds none at rest.
        assert!(save_peak <= 5 * BIG, "save peak {save_peak}");
        assert!(save_retained < BIG / 16, "save retained {save_retained}");
        assert_eq!(
            (
                saved.revision,
                saved.previews_preserved,
                saved.previews_dropped
            ),
            (8, 0, 1)
        );

        // Independent oracle: the package reader, not the session.
        let opened = independent(&fixture.path);
        assert_eq!(
            opened.snapshot().unwrap().document().layers[0].name,
            "renamed-layer"
        );
        assert_eq!(opened.assets.get(&sha256_hex_of(&blob)), Some(&blob));
        assert_eq!(opened.quarantined[0].bytes, OPAQUE);

        // The saved file is the source from now on: Save As twice, same bytes (deterministic).
        let (copy_a, copy_b) = (
            fixture.dir.join("a.handshake"),
            fixture.dir.join("b.handshake"),
        );
        session.save_to(&copy_a, &token()).unwrap();
        fs::remove_file(&fixture.path).unwrap();
        session.save_to(&copy_b, &token()).unwrap();
        assert_eq!(fs::read(&copy_a).unwrap(), fs::read(&copy_b).unwrap());
        assert_eq!(session.origin(), Some(copy_b.as_path()));
        assert_eq!(frames.count() as u64, session.operations());
    });
}

#[test]
fn host_supplied_previews_follow_the_revision() {
    bounded_case("host_supplied_previews_follow_the_revision", || {
        let fixture = Fixture::new("previews");
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);
        let baseline = session.resident_bytes();
        let limits = Limits::default();
        let on_disk = |fixture: &Fixture| {
            list_previews(&fs::read(&fixture.path).unwrap(), &limits, &token()).unwrap()
        };

        // Refusals store nothing: wrong revision, wrong pixel count, duplicate content, too many.
        let fresh = gray_preview(4, 200);
        assert_eq!(
            session
                .set_previews(6, vec![fresh.clone()], &token())
                .unwrap_err()
                .error,
            SessionError::StaleRevision {
                expected: 6,
                current: 7
            }
        );
        let mut short = gray_preview(4, 1);
        short.pixels.pop();
        assert_eq!(
            session
                .set_previews(7, vec![short], &token())
                .unwrap_err()
                .code(),
            "preview_mismatch"
        );
        assert_eq!(
            session
                .set_previews(7, vec![fresh.clone(), fresh.clone()], &token())
                .unwrap_err()
                .code(),
            "preview_mismatch"
        );
        let many: Vec<PreviewImage> = (0..=MAX_SUPPLIED_PREVIEWS as u8)
            .map(|n| gray_preview(2, n))
            .collect();
        assert_eq!(
            session.set_previews(7, many, &token()).unwrap_err().code(),
            "too_many_entries"
        );
        assert_eq!(session.resident_bytes(), baseline);

        // Accepted for the current revision: replaces the source's preview at the next save.
        let accepted = session
            .set_previews(7, vec![fresh.clone()], &token())
            .unwrap();
        assert_eq!((accepted.accepted, accepted.bytes), (1, 16));
        assert_eq!(session.resident_bytes(), baseline + 16);
        let saved = session.save(&token()).unwrap();
        assert_eq!((saved.previews_preserved, saved.previews_dropped), (1, 1));
        let listed = on_disk(&fixture);
        assert_eq!(listed.len(), 1);
        let bytes = fs::read(&fixture.path).unwrap();
        let image = read_preview(&bytes, &limits, &listed[0].name, &token()).unwrap();
        assert_eq!(
            (image.pixels, image.info),
            (fresh.pixels.clone(), fresh.info.clone())
        );
        assert_eq!(
            session.resident_bytes(),
            baseline,
            "supplied previews are released once written"
        );

        // Now they live in the saved source and are read from it lazily on the next save.
        let again = session.save(&token()).unwrap();
        assert_eq!((again.previews_preserved, again.previews_dropped), (1, 0));
        assert_eq!(on_disk(&fixture).len(), 1);

        // After an edit the source preview describes the old revision; a host that supplied for
        // the old revision is refused, one that supplies for the new revision is accepted...
        let rename = |from: &str, to: &str, read: u64, name: &str| {
            json_bytes(&rename_patch(1, from, to, read, name))
        };
        session
            .submit(
                ID,
                &rename("layer-1", "renamed-layer", 0, "e1"),
                7,
                &token(),
            )
            .unwrap();
        assert_eq!(
            session
                .set_previews(7, vec![fresh.clone()], &token())
                .unwrap_err()
                .error,
            SessionError::StaleRevision {
                expected: 7,
                current: 8
            }
        );
        let one_by_one = PreviewImage {
            info: PreviewInfo {
                format: PixelFormat::Rgb8,
                width: 1,
                height: 1,
                color: "srgb".to_owned(),
            },
            pixels: vec![1, 2, 3],
        };
        session
            .set_previews(8, vec![one_by_one.clone()], &token())
            .unwrap();
        // ...and an edit after that makes the supplied set stale too: both are dropped, counted.
        session
            .submit(
                ID,
                &rename("renamed-layer", "renamed-again", 1, "e2"),
                8,
                &token(),
            )
            .unwrap();
        let cp = session.export_checkpoint(&token()).unwrap();
        assert_eq!(
            (cp.revision, cp.previews_preserved, cp.previews_dropped),
            (9, 0, 2)
        );
        assert!(
            list_previews(&cp.bytes, &limits, &token())
                .unwrap()
                .is_empty()
        );
        let saved = session.save(&token()).unwrap();
        assert_eq!((saved.previews_preserved, saved.previews_dropped), (0, 2));
        assert!(on_disk(&fixture).is_empty());
        let last = session.export_checkpoint(&token()).unwrap();
        assert_eq!(
            (last.previews_preserved, last.previews_dropped),
            (0, 0),
            "nothing left to drop"
        );
        assert_eq!(frames.count() as u64, session.operations());
    });
}

#[test]
fn lazy_failures_are_typed_cancel_reads_nothing_and_memory_sessions_work() {
    bounded_case("lazy_failures_typed_cancel_memory_sessions", || {
        let blob: Vec<u8> = (0..1_048_576u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let fixture = Fixture::with_blob("lazyfail", blob.clone());
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);

        // Parity with the package reader: the opaque entry is preserved and reported as loss.
        assert_eq!(session.loss().len(), 1);
        assert_eq!(session.loss()[0].name, "vendor/extra.bin");
        assert_eq!(
            session.loss()[0].reason,
            hsk_studio_package::LossReason::Opaque
        );

        // A canceled save reads nothing and leaves the file alone.
        let reads = session.source_bytes_read();
        let canceled = token();
        canceled.cancel();
        assert_eq!(
            session.save(&canceled).unwrap_err().error,
            SessionError::Canceled
        );
        assert_eq!(session.source_bytes_read(), reads);
        assert_eq!(fs::read(&fixture.path).unwrap(), fixture.bytes);

        // The asset body is only read (and verified) when needed: damage it behind the
        // session's back and the save fails with the true cause, not a vague "asset absent".
        let at = fixture
            .bytes
            .windows(64)
            .position(|w| w == &blob[..64])
            .expect("asset body in archive")
            + 10;
        let flip = |value: u8| {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = fs::OpenOptions::new()
                .write(true)
                .open(&fixture.path)
                .unwrap();
            file.seek(SeekFrom::Start(at as u64)).unwrap();
            file.write_all(&[value]).unwrap();
        };
        flip(blob[10] ^ 0xff);
        let damaged = fs::read(&fixture.path).unwrap();
        let failed = session.save(&token()).unwrap_err();
        assert_eq!(failed.code(), "hash_mismatch");
        assert_eq!(
            delivered(failed.report).outcome,
            Outcome::Failure(FailureCode::Validation)
        );
        assert_eq!(
            fs::read(&fixture.path).unwrap(),
            damaged,
            "last-good destination untouched"
        );
        let elsewhere = fixture.dir.join("elsewhere.handshake");
        assert_eq!(
            session.save_to(&elsewhere, &token()).unwrap_err().code(),
            "hash_mismatch"
        );
        assert!(!elsewhere.exists());
        assert_eq!(
            fixture.files(),
            ["doc.handshake"],
            "no temp or marker left behind"
        );
        assert_eq!(session.document_revision(), 7);

        // Nothing was cached: repair the byte and the same session saves correctly.
        flip(blob[10]);
        session.save_to(&elsewhere, &token()).unwrap();
        assert_eq!(
            independent(&elsewhere).assets.get(&sha256_hex_of(&blob)),
            Some(&blob)
        );

        // An in-memory session is lazy over its own copy, edits, and saves to a fresh path.
        let mut memory = Session::open_bytes(
            &fixture.bytes,
            SessionConfig::new(actor_context()),
            ports(&Frames::default()),
            &token(),
        )
        .unwrap();
        let params = json_bytes(&rename_patch(1, "layer-1", "from-memory", 0, "edit-m"));
        memory.submit(ID, &params, 7, &token()).unwrap();
        let target = fixture.dir.join("memory.handshake");
        memory.save_to(&target, &token()).unwrap();
        let opened = independent(&target);
        assert_eq!(
            opened.snapshot().unwrap().document().layers[0].name,
            "from-memory"
        );
        assert_eq!(opened.assets.get(&sha256_hex_of(&blob)), Some(&blob));
        memory.save(&token()).unwrap();
        assert_eq!(memory.origin(), Some(target.as_path()));
    });
}
