use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_chronicle::{
    self as chronicle, Actor, Patch, Prepared, Property, PropertyAddress, PropertyRead,
    PropertyValue, PropertyWrite, RevisionEntry,
};
use hsk_studio_folio::{self as folio, NodeKind, NodeRef, Snapshot};
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};
use hsk_studio_package::{
    AssetSource, Codec, Extras, Limits, MemoryAssetSource, PixelFormat, PreviewImage, PreviewInfo,
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
        Self::with_extra_entries(name, blob, Vec::new())
    }

    fn with_extra_entries(name: &str, blob: Vec<u8>, more: Vec<Quarantined>) -> Self {
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
        let mut opaque = vec![Quarantined {
            name: "vendor/extra.bin".to_owned(),
            bytes: OPAQUE.to_vec(),
            sha256_hex: sha256_hex_of(&OPAQUE),
        }];
        opaque.extend(more);
        let mut bytes = Vec::new();
        write_package_with(
            &snapshot,
            &assets as &dyn AssetSource,
            &Extras {
                quarantined: &opaque,
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
            11,
            "group (name, visible, opacity, blend_mode, placement: it fills a root output) + raster (name, visible, opacity, blend_mode, tiles; no placement: its parent is the root but it feeds the group) + artboard name"
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

/// Renames layer 1 like `RenameFirstLayer`, but prepares from a copy of the live snapshot (same
/// document id, same revision) that already carries an unrevisioned edit of layer 2 when `forge`.
struct PrepareFromEditedCopy {
    forge: bool,
}
impl ProposalProvider for PrepareFromEditedCopy {
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let mut base = request.snapshot.clone();
        if self.forge {
            let mut edited = request.snapshot.document().clone();
            edited.layers[1].name = "smuggled".into();
            base = folio::validate_document(
                edited,
                request.budget.folio,
                request.cancel,
                request.resolver,
            )
            .map_err(|_| ProviderError::Rejected("forge_failed"))?;
        }
        let revision = request
            .revisions
            .iter()
            .find(|e| e.address == name_address(1))
            .map_or(0, |e| e.revision);
        let patch = rename_patch(1, "layer-1", "honest", revision, "forged-base");
        chronicle::prepare(
            &patch,
            &base,
            request.revisions,
            request.budget,
            request.cancel,
            request.resolver,
        )
        .map_err(provider_error)
    }
}

#[test]
fn successor_that_does_not_derive_from_the_live_snapshot_is_refused_before_commit() {
    bounded_case("forged_successor_refused_before_commit", || {
        let rig = Rig::new("forged-base", 2);
        let mut session = rig.session();
        for (id, forge) in [("test.honest", false), ("test.forged", true)] {
            session
                .providers_mut()
                .register(id, Box::new(PrepareFromEditedCopy { forge }))
                .unwrap();
        }
        let revisions = session.revisions().to_vec();
        let before = session.snapshot().encoded_bytes().to_vec();

        // The forged successor is the right document and the right revision (8) and its declared
        // revision update is legal, so every earlier check passes; only lineage catches it.
        let refused = session
            .submit_keyed(&key("forge-0001"), "test.forged", b"", 7, &token())
            .unwrap_err();
        assert_eq!(refused.error, SessionError::ProposalNotDerived);
        assert_eq!(refused.code(), "proposal_not_derived");
        assert_eq!(
            refused.error.outcome(),
            Outcome::Failure(FailureCode::Validation)
        );
        assert_eq!(session.document_revision(), 7);
        assert_eq!(session.snapshot().document().layers[0].name, "layer-1");
        assert_eq!(session.snapshot().document().layers[1].name, "layer-2");
        assert_eq!(session.snapshot().encoded_bytes(), before.as_slice());
        assert_eq!(session.revisions(), revisions.as_slice());
        assert_eq!(
            (rig.authority.commit_calls(), session.receipt_count()),
            (0, 0)
        );

        // The same provider on the live snapshot is an ordinary proposal and commits.
        let applied = session
            .submit_keyed(&key("forge-0002"), "test.honest", b"", 7, &token())
            .unwrap();
        assert_eq!((applied.previous_revision, applied.revision), (7, 8));
        assert_eq!(session.snapshot().document().layers[0].name, "honest");
        assert_eq!(session.snapshot().document().layers[1].name, "layer-2");
        assert_eq!(rig.authority.commits(), 1);
        rig.assert_balanced(&session);
    });
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
    /// Commit durably, but record and answer a receipt bound to some other request. One shot.
    OtherBinding,
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
        let binding = if s.mode == Mode::OtherBinding {
            s.mode = Mode::Normal;
            OTHER_BINDING.to_owned()
        } else {
            r.binding.to_owned()
        };
        let receipt = CommitReceipt {
            revision: r.successor_revision,
            sequence: s.sequence,
            binding,
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
const OTHER_BINDING: &str = "binding-of-another-request";

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
        self.session_with(|_| {})
    }
    fn session_with(&self, tune: impl FnOnce(&mut SessionConfig)) -> Session {
        let mut config = SessionConfig::new(actor_context());
        config.epoch = 1;
        tune(&mut config);
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
        // The save streams: raw assets flow from the source file to the temp file through fixed
        // chunks, so the peak sits far below even one copy of the asset (the buffered save cost
        // 4.25x here); `save_peak_memory_does_not_grow_with_asset_size` shows it is size-free.
        assert!(save_peak < BIG / 2, "save peak {save_peak}");
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

/// Incompressible synthetic bytes (a fixed LCG), `size` long.
fn synthetic_blob(size: usize) -> Vec<u8> {
    let mut x: u32 = 99;
    (0..size)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn save_peak_memory_does_not_grow_with_asset_size() {
    bounded_case("save_peak_independent_of_asset_size", || {
        // Each fixture writes its package under the test temp root (TMP; the Studio env points it
        // at the artifact root on D:) and removes it on drop. The allocator counts only the
        // measured thread, from `probe()` to the end of the save.
        const SMALL: usize = 1 << 20;
        const LARGE: usize = 16 << 20;
        let measure = |size: usize| {
            let blob = synthetic_blob(size);
            let fixture = Fixture::with_blob(&format!("stream-{size}"), blob.clone());
            let frames = Frames::default();
            let mut session = open(&fixture, &frames);
            let params = json_bytes(&rename_patch(1, "layer-1", "edited", 0, "edit-1"));
            session.submit(ID, &params, 7, &token()).unwrap();
            let before = session.source_bytes_read();
            let mark = probe();
            let saved = session.save(&token()).unwrap();
            let (peak, retained) = (mark.peak(), mark.retained());
            assert!(
                session.source_bytes_read() - before >= size as u64,
                "the asset was streamed from the source"
            );
            assert_eq!((saved.revision, saved.quarantined_preserved), (8, 1));
            // Independent oracle: the package reader, not the session.
            let opened = independent(&fixture.path);
            assert_eq!(
                opened.snapshot().unwrap().document().layers[0].name,
                "edited"
            );
            assert_eq!(opened.assets.get(&sha256_hex_of(&blob)), Some(&blob));
            (peak, retained)
        };
        let (small_peak, small_retained) = measure(SMALL);
        let (large_peak, large_retained) = measure(LARGE);
        eprintln!(
            "measured: save peak {small_peak} at {SMALL} B asset, {large_peak} at {LARGE} B asset; retained {small_retained}/{large_retained}"
        );
        // The old buffered save held several copies of the package (about 4x the asset: 64 MiB
        // here); streaming holds a few fixed chunks whatever the asset size.
        assert!(
            large_peak < LARGE / 3,
            "save peak {large_peak} for a {LARGE} B asset"
        );
        assert!(
            large_peak <= small_peak + (1 << 20),
            "peak grew with the asset: {small_peak} -> {large_peak}"
        );
        assert!(small_retained < 256 * 1024 && large_retained < 256 * 1024);
    });
}

#[test]
fn compressed_source_assets_stream_through_save_and_round_trip() {
    bounded_case("compressed_source_assets_stream", || {
        // Highly compressible, so zstd is chosen per blob and the entry is stored smaller.
        let blob: Vec<u8> = (0..(256 * 1024usize)).map(|i| (i / 64) as u8).collect();
        let fixture = Fixture::with_blob("codec", blob.clone());
        let frames = Frames::default();
        let mut config = SessionConfig::new(actor_context());
        config.asset_codec = Some(Codec::Zstd);
        let mut session =
            Session::open_path(&fixture.path, config, ports(&frames), &token()).unwrap();

        // Raw source entry, zstd requested: the writer plans and writes from two opens of the
        // same bytes.
        let compressed = fixture.dir.join("zstd.handshake");
        session.save_to(&compressed, &token()).unwrap();
        let opened = independent(&compressed);
        assert_eq!(opened.assets.get(&sha256_hex_of(&blob)), Some(&blob));
        let entry = opened
            .manifest
            .entries
            .iter()
            .find(|e| e.name.starts_with("assets/"))
            .unwrap();
        assert_eq!(entry.codec, Codec::Zstd);
        assert!(entry.len < blob.len() as u64 / 4, "stored {} B", entry.len);

        // Zstd source entry: decoded and verified as one blob, then written again.
        let mut again = Session::open_path(
            &compressed,
            SessionConfig::new(actor_context()),
            ports(&Frames::default()),
            &token(),
        )
        .unwrap();
        let second = fixture.dir.join("zstd-again.handshake");
        again.save_to(&second, &token()).unwrap();
        assert_eq!(fs::read(&compressed).unwrap(), fs::read(&second).unwrap());
        assert_eq!(
            independent(&second).assets.get(&sha256_hex_of(&blob)),
            Some(&blob)
        );
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

// ----- receipt eviction, diverged-commit resolution and the negative paths the review listed --

fn edit_key(n: u32) -> IdempotencyKey {
    key(&format!("edit-{n:04}"))
}

/// Renames layer 1 from `from` to `to`, quoting property revision `read_revision`.
fn layer1_rename(from: &str, to: &str, read_revision: u64, n: u32) -> Vec<u8> {
    json_bytes(&rename_patch(
        1,
        from,
        to,
        read_revision,
        &format!("cmd-{n}"),
    ))
}

#[test]
fn receipt_table_is_bounded_and_evicts_only_acknowledged_receipts() {
    bounded_case("receipt_table_bounded_evicts_acknowledged", || {
        // Policy EvictAcknowledged (the default): the table never exceeds its bound, the oldest
        // acknowledged receipt goes first, and every eviction is reported.
        let rig = Rig::new("evict", 4);
        let mut session = rig.session_with(|c| c.budgets.max_receipts = 3);
        assert_eq!(
            session.receipt_stats().capacity,
            3,
            "the configured bound is the table bound"
        );
        let (mut revision, mut property_revision, mut name) = (7u64, 0u64, "layer-1".to_owned());
        let mut params = Vec::new();
        let mut evicted = Vec::new();
        for n in 1..=5u32 {
            let to = format!("name-{n}");
            let p = layer1_rename(&name, &to, property_revision, n);
            let applied = session
                .submit_keyed(&edit_key(n), ID, &p, revision, &token())
                .unwrap();
            evicted.push(applied.receipts_evicted);
            assert!(session.receipt_count() <= 3, "bound held after edit {n}");
            (revision, property_revision, name) = (applied.revision, property_revision + 1, to);
            params.push(p);
        }
        assert_eq!(evicted, [0, 0, 0, 1, 1]);
        let stats = session.receipt_stats();
        assert_eq!(
            (
                stats.held,
                stats.pending,
                stats.unacknowledged,
                stats.evicted
            ),
            (3, 0, 0, 2)
        );
        assert_eq!(rig.authority.commits(), 5, "every edit committed once");

        // The newest receipts still replay; nothing is re-applied.
        let replay = session
            .submit_keyed(&edit_key(5), ID, &params[4], 11, &token())
            .unwrap();
        assert!(replay.replayed && replay.receipts_evicted == 0);
        assert_eq!(rig.authority.commit_calls(), 5);

        // The oldest were evicted: they are not held, and replaying one is refused by the
        // revision check (typed, nothing applied) rather than applied a second time.
        assert_eq!(
            session
                .acknowledge_receipt(&edit_key(1))
                .unwrap_err()
                .code(),
            "receipt_not_held"
        );
        assert_eq!(
            session
                .submit_keyed(&edit_key(1), ID, &params[0], 7, &token())
                .unwrap_err()
                .error,
            SessionError::StaleRevision {
                expected: 7,
                current: 12
            }
        );
        assert_eq!(
            (rig.authority.commit_calls(), session.document_revision()),
            (5, 12)
        );
        let kept = session.acknowledge_receipt(&edit_key(3)).unwrap();
        assert!(kept.replayed);
        assert_eq!(kept.commit.map(|c| c.sequence), Some(3));
        assert_eq!(session.receipt_stats().evicted, 2, "reads evict nothing");
        rig.assert_balanced(&session);

        // Policy RefuseWhenFull: a full table refuses a new key typed, evicts nothing, starts
        // nothing, and still replays the keys it holds.
        let rig = Rig::new("refuse", 4);
        let mut session = rig.session_with(|c| {
            c.budgets.max_receipts = 2;
            c.budgets.receipt_policy = ReceiptPolicy::RefuseWhenFull;
        });
        let (p1, p2) = (
            layer1_rename("layer-1", "name-1", 0, 1),
            layer1_rename("name-1", "name-2", 1, 2),
        );
        session
            .submit_keyed(&edit_key(1), ID, &p1, 7, &token())
            .unwrap();
        session
            .submit_keyed(&edit_key(2), ID, &p2, 8, &token())
            .unwrap();
        let p3 = layer1_rename("name-2", "name-3", 2, 3);
        let refused = session
            .submit_keyed(&edit_key(3), ID, &p3, 9, &token())
            .unwrap_err();
        assert_eq!(refused.error, SessionError::ReceiptsFull);
        assert_eq!(refused.code(), "receipts_full");
        assert_eq!(
            delivered(refused.report).outcome,
            Outcome::Failure(FailureCode::Validation)
        );
        assert_eq!(
            (
                rig.authority.commit_calls(),
                session.receipt_count(),
                session.receipt_stats().evicted
            ),
            (2, 2, 0)
        );
        assert!(
            session
                .submit_keyed(&edit_key(1), ID, &p1, 7, &token())
                .unwrap()
                .replayed
        );
        rig.assert_balanced(&session);
    });
}

#[test]
fn unacknowledged_and_pending_receipts_are_kept_and_a_full_table_refuses_typed() {
    bounded_case("unacknowledged_pending_receipts_kept", || {
        let rig = Rig::new("unacknowledged", 2);
        let mut session = rig.session_with(|c| c.budgets.max_receipts = 2);
        let p1 = layer1_rename("layer-1", "name-1", 0, 1);
        let second = json_bytes(&rename_patch(2, "layer-2", "second", 0, "cmd-2"));
        let p3 = layer1_rename("name-1", "name-3", 1, 3);

        // A pending receipt is never an eviction victim, and nothing is stored beside it.
        rig.authority.with(|s| s.mode = Mode::LoseAck);
        assert_eq!(
            session
                .submit_keyed(&edit_key(1), ID, &p1, 7, &token())
                .unwrap_err()
                .error,
            SessionError::OutcomeUnknown
        );
        let stats = session.receipt_stats();
        assert_eq!((stats.held, stats.pending, stats.evicted), (1, 1, 0));
        assert_eq!(
            session
                .submit_keyed(&edit_key(2), ID, &second, 7, &token())
                .unwrap_err()
                .error,
            SessionError::ReconciliationRequired
        );
        assert_eq!(session.pending(), ["edit-0001"]);
        assert_eq!(rig.authority.commit_calls(), 1);

        // Adopted by reconcile: committed, but the caller was never handed it.
        assert_eq!(session.reconcile(&token()).unwrap().adopted, 1);
        let stats = session.receipt_stats();
        assert_eq!((stats.held, stats.pending, stats.unacknowledged), (1, 0, 1));
        rig.authority.with(|s| s.mode = Mode::LoseAck);
        assert_eq!(
            session
                .submit_keyed(&edit_key(2), ID, &second, 8, &token())
                .unwrap_err()
                .error,
            SessionError::OutcomeUnknown
        );
        assert_eq!(session.reconcile(&token()).unwrap().adopted, 1);
        let stats = session.receipt_stats();
        assert_eq!((stats.held, stats.unacknowledged, stats.evicted), (2, 2, 0));

        // Full of receipts the caller has not seen: a new key is refused typed, nothing started,
        // nothing evicted.
        let calls = rig.authority.commit_calls();
        let refused = session
            .submit_keyed(&edit_key(3), ID, &p3, 9, &token())
            .unwrap_err();
        assert_eq!(refused.error, SessionError::ReceiptsFull);
        assert_eq!(rig.authority.commit_calls(), calls);
        assert_eq!(session.receipt_stats().evicted, 0);
        assert_eq!(session.document_revision(), 9);

        // Acknowledging the first makes only that one evictable.
        let handed = session.acknowledge_receipt(&edit_key(1)).unwrap();
        assert!(handed.replayed);
        assert_eq!(
            handed.commit.map(|c| (c.revision, c.sequence)),
            Some((8, 1))
        );
        assert_eq!(session.receipt_stats().unacknowledged, 1);
        let applied = session
            .submit_keyed(&edit_key(3), ID, &p3, 9, &token())
            .unwrap();
        assert_eq!(applied.receipts_evicted, 1);
        assert_eq!(
            session
                .acknowledge_receipt(&edit_key(1))
                .unwrap_err()
                .code(),
            "receipt_not_held"
        );
        let stats = session.receipt_stats();
        assert_eq!(
            (stats.held, stats.unacknowledged, stats.evicted),
            (2, 1, 1),
            "the receipt adopted for edit-0002 is still held and still unacknowledged"
        );
        rig.assert_balanced(&session);
    });
}

/// The authority's own document: the session's current document with a revision and a layer
/// name of the authority's choosing.
fn authority_document(base: &Value, revision: u64, layer_name: &str) -> Vec<u8> {
    let mut value = base.clone();
    value["revision"] = json!(revision);
    value["layers"][0]["name"] = json!(layer_name);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn diverged_pending_commit_is_resolved_only_by_the_operator_discard_or_resync() {
    bounded_case("diverged_commit_operator_resolution", || {
        // ---- discard path ---------------------------------------------------------------
        let rig = Rig::new("diverged-discard", 2);
        let mut session = rig.session();
        let first = layer1_rename("layer-1", "name-1", 0, 1);
        let second = json_bytes(&rename_patch(2, "layer-2", "second", 0, "cmd-2"));
        let (k_a, k_b, k_c) = (edit_key(1), edit_key(2), edit_key(3));

        // A pending key whose receipt agrees (or is a committed key) is not diverged: the
        // operator commands refuse it and leave everything as it is.
        rig.authority.with(|s| s.mode = Mode::LoseAck);
        assert_eq!(
            session
                .submit_keyed(&k_a, ID, &first, 7, &token())
                .unwrap_err()
                .error,
            SessionError::OutcomeUnknown
        );
        assert_eq!(
            session.discard_diverged(&k_a, &token()).unwrap_err().error,
            SessionError::NotDiverged
        );
        assert_eq!(session.pending(), ["edit-0001"]);
        assert_eq!(session.reconcile(&token()).unwrap().adopted, 1);
        assert_eq!(
            session.discard_diverged(&k_a, &token()).unwrap_err().code(),
            "not_diverged"
        );

        // The authority answers with a receipt bound to another request: refused, kept pending.
        rig.authority.with(|s| s.mode = Mode::OtherBinding);
        let failed = session
            .submit_keyed(&k_b, ID, &second, 8, &token())
            .unwrap_err();
        assert_eq!(failed.error, SessionError::ReceiptMismatch);
        assert_eq!(
            delivered(failed.report).outcome,
            Outcome::Failure(FailureCode::Validation)
        );
        assert_eq!(session.pending(), ["edit-0002"]);
        assert_eq!(session.diverged(), ["edit-0002"]);
        assert_eq!(session.document_revision(), 8, "not adopted");

        // Reconcile and retries never resolve it: a person decides.
        let report = session.reconcile(&token()).unwrap();
        assert_eq!(
            (
                report.adopted,
                report.cleared,
                report.still_unknown,
                report.diverged
            ),
            (0, 0, 0, 1)
        );
        assert_eq!(report.diverged_keys, ["edit-0002"]);
        assert_eq!(
            delivered(report.report).outcome,
            Outcome::Failure(FailureCode::Unavailable)
        );
        assert_eq!(
            session
                .submit_keyed(&k_b, ID, &second, 8, &token())
                .unwrap_err()
                .error,
            SessionError::ReceiptMismatch
        );
        let other = layer1_rename("name-1", "name-9", 1, 9);
        assert_eq!(
            session
                .submit_keyed(&k_c, ID, &other, 8, &token())
                .unwrap_err()
                .error,
            SessionError::ReconciliationRequired
        );
        assert_eq!(
            session.acknowledge_receipt(&k_b).unwrap_err().error,
            SessionError::ReceiptMismatch
        );
        assert_eq!(
            (rig.authority.commit_calls(), session.document_revision()),
            (2, 8)
        );

        // Typed refusals of the command itself change nothing.
        assert_eq!(
            session
                .discard_diverged(&edit_key(77), &token())
                .unwrap_err()
                .error,
            SessionError::ReceiptNotHeld
        );
        rig.grants.set_revoked(true);
        assert_eq!(
            session.discard_diverged(&k_b, &token()).unwrap_err().error,
            SessionError::GrantRevoked
        );
        rig.grants.set_revoked(false);
        rig.authority.with(|s| s.unreachable = true);
        assert_eq!(
            session.discard_diverged(&k_b, &token()).unwrap_err().error,
            SessionError::OutcomeUnknown
        );
        rig.authority.with(|s| s.unreachable = false);
        let canceled = token();
        canceled.cancel();
        assert_eq!(
            session.discard_diverged(&k_b, &canceled).unwrap_err().error,
            SessionError::Canceled
        );
        assert_eq!(session.diverged(), ["edit-0002"]);

        // Discard: the pending commit is dropped, the document is untouched, submits are open.
        let before = session.snapshot().encoded_bytes().to_vec();
        let done = session.discard_diverged(&k_b, &token()).unwrap();
        assert_eq!(
            (
                done.resolution,
                done.revision_before,
                done.revision_after,
                done.draft_revision
            ),
            (Divergence::Discarded, 8, 8, 9)
        );
        assert_eq!(done.authority.binding, OTHER_BINDING);
        assert_eq!(
            done.disagreement,
            Disagreement {
                binding: true,
                revision: false,
                base: false
            }
        );
        assert_eq!(delivered(done.report).outcome, Outcome::Success);
        assert_eq!(session.snapshot().encoded_bytes(), before.as_slice());
        assert!(session.pending().is_empty() && session.diverged().is_empty());
        assert_eq!(
            session.discard_diverged(&k_b, &token()).unwrap_err().error,
            SessionError::ReceiptNotHeld
        );
        // The authority did commit that other request (revision 9), so the discarded session is
        // behind it and says so, typed, instead of pretending to be in sync.
        assert_eq!(
            session
                .submit_keyed(&k_c, ID, &other, 8, &token())
                .unwrap_err()
                .error,
            SessionError::StaleRevision {
                expected: 8,
                current: 9
            }
        );
        rig.assert_balanced(&session);

        // ---- resync path ----------------------------------------------------------------
        let rig = Rig::new("diverged-resync", 2);
        // Folio node budget exactly the fixture's (1 artboard + 2 layers + 2 operations).
        let mut session = rig.session_with(|c| c.budgets.chronicle.folio.nodes = 5);
        let base: Value = serde_json::from_slice(session.snapshot().encoded_bytes()).unwrap();
        rig.authority.with(|s| s.mode = Mode::OtherBinding);
        let failed = session
            .submit_keyed(&k_a, ID, &first, 7, &token())
            .unwrap_err();
        assert_eq!(failed.error, SessionError::ReceiptMismatch);
        let untouched = session.snapshot().encoded_bytes().to_vec();
        let revisions = session.revisions().to_vec();

        let mut foreign = base.clone();
        foreign["document_id"] = json!(identity("SDOC", 2));
        foreign["revision"] = json!(8);
        let mut over_budget = base.clone();
        over_budget["revision"] = json!(8);
        over_budget["artboards"].as_array_mut().unwrap().push(json!({
            "schema_id":"hsk.studio.artboard@1","artboard_id":identity("SART",2),"name":"extra","parent":null,
            "bounds":base["artboards"][0]["bounds"].clone()
        }));
        let mut missing_asset = base.clone();
        missing_asset["revision"] = json!(8);
        missing_asset["layers"][1]["payload"]["tiles"][0]["content_digest"]["digest"] =
            json!(sha256_hex_of(b"an asset the source package does not hold"));
        let refusals: [(Vec<u8>, &str); 7] = [
            (
                serde_json::to_vec(&foreign).unwrap(),
                "authority_document_wrong_document",
            ),
            (
                authority_document(&base, 9, "x"),
                "authority_document_wrong_revision",
            ),
            (b"{not json".to_vec(), "authority_document_invalid"),
            (Vec::new(), "authority_document_invalid"),
            (vec![b' '; 1 << 21], "authority_document_too_large"),
            (
                serde_json::to_vec(&over_budget).unwrap(),
                "authority_document_too_large",
            ),
            (
                serde_json::to_vec(&missing_asset).unwrap(),
                "authority_document_missing_asset",
            ),
        ];
        for (bytes, code) in &refusals {
            let failed = session.resync_diverged(&k_a, bytes, &token()).unwrap_err();
            assert_eq!(failed.code(), *code);
            assert_eq!(
                failed.error.outcome(),
                Outcome::Failure(FailureCode::Validation)
            );
            // Nothing changed: still pending, still diverged, document and vector untouched.
            assert_eq!(session.diverged(), ["edit-0001"], "{code}");
            assert_eq!(session.snapshot().encoded_bytes(), untouched.as_slice());
            assert_eq!(session.revisions(), revisions.as_slice());
            assert_eq!(session.receipt_count(), 1);
        }

        // The authority's document at the receipt's revision replaces the session's.
        let done = session
            .resync_diverged(
                &k_a,
                &authority_document(&base, 8, "authority-name"),
                &token(),
            )
            .unwrap();
        assert_eq!(
            (
                done.resolution,
                done.revision_before,
                done.revision_after,
                done.authority.revision
            ),
            (Divergence::Resynced, 7, 8, 8)
        );
        assert_eq!(delivered(done.report).outcome, Outcome::Success);
        assert_eq!(session.document_revision(), 8);
        assert_eq!(
            session.snapshot().document().layers[0].name,
            "authority-name"
        );
        assert!(session.revisions().iter().all(|e| e.revision == 0));
        assert!(session.pending().is_empty() && session.diverged().is_empty());
        assert_eq!(session.receipt_count(), 0, "the divergent receipt is gone");
        assert!(session.is_dirty());

        // In sync again: the next edit commits on top of the authority's document.
        let applied = session
            .submit_keyed(&k_b, ID, &second, 8, &token())
            .unwrap();
        assert_eq!((applied.previous_revision, applied.revision), (8, 9));
        let durable = document_of(&rig.authority.last_document());
        assert_eq!(durable.document().layers[0].name, "authority-name");
        assert_eq!(durable.document().layers[1].name, "second");
        assert_eq!(durable.encoded_bytes(), session.snapshot().encoded_bytes());
        rig.assert_balanced(&session);
    });
}

/// Prepares a transaction against a snapshot whose document id is not the session's.
struct ForeignDocument;
impl ProposalProvider for ForeignDocument {
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let mut foreign = request.snapshot.document().clone();
        foreign.document_id = identity("SDOC", 2);
        let base = folio::validate_document(
            foreign,
            request.budget.folio,
            request.cancel,
            request.resolver,
        )
        .map_err(|_| ProviderError::Rejected("forge_failed"))?;
        let mut patch = rename_patch(1, "layer-1", "foreign", 0, "foreign-doc");
        patch.document_id = identity("SDOC", 2);
        chronicle::prepare(
            &patch,
            &base,
            request.revisions,
            request.budget,
            request.cancel,
            request.resolver,
        )
        .map_err(provider_error)
    }
}

/// Prepares against its own property revision vector (layer 1 name at revision 5), not the
/// session's (revision 0).
struct ForgedPropertyRevision;
impl ProposalProvider for ForgedPropertyRevision {
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let mut own = request.revisions.to_vec();
        for entry in &mut own {
            if entry.address == name_address(1) {
                entry.revision = 5;
            }
        }
        let patch = rename_patch(1, "layer-1", "forged", 5, "forged-revision");
        chronicle::prepare(
            &patch,
            request.snapshot,
            &own,
            request.budget,
            request.cancel,
            request.resolver,
        )
        .map_err(provider_error)
    }
}

#[test]
fn inconsistent_proposals_are_refused_and_a_zero_deadline_drain_reports_what_it_left() {
    bounded_case("proposal_inconsistent_and_zero_deadline_drain", || {
        let rig = Rig::new("inconsistent", 2);
        let mut session = rig.session();
        let providers: [(&str, Box<dyn ProposalProvider>); 2] = [
            ("test.foreign-document", Box::new(ForeignDocument)),
            ("test.forged-revision", Box::new(ForgedPropertyRevision)),
        ];
        for (id, provider) in providers {
            session.providers_mut().register(id, provider).unwrap();
        }
        let before = session.snapshot().encoded_bytes().to_vec();
        let revisions = session.revisions().to_vec();

        // Wrong document, and a property revision the session vector does not hold: both are
        // proposals the session refuses before any commit authority is asked.
        for (n, id) in ["test.foreign-document", "test.forged-revision"]
            .into_iter()
            .enumerate()
        {
            let refused = session
                .submit_keyed(&edit_key(n as u32 + 1), id, b"", 7, &token())
                .unwrap_err();
            assert_eq!(refused.error, SessionError::ProposalInconsistent, "{id}");
            assert_eq!(refused.code(), "proposal_inconsistent");
            assert_eq!(
                delivered(refused.report).outcome,
                Outcome::Failure(FailureCode::Validation)
            );
            assert_eq!(session.snapshot().encoded_bytes(), before.as_slice());
            assert_eq!(session.revisions(), revisions.as_slice());
            assert_eq!(
                (rig.authority.commit_calls(), session.receipt_count()),
                (0, 0)
            );
        }
        rig.assert_balanced(&session);

        // A zero deadline looks nothing up: the pending key is reported abandoned, not dropped,
        // even though the authority is reachable; a later drain with time resolves it.
        let p1 = layer1_rename("layer-1", "name-1", 0, 1);
        rig.authority.with(|s| s.mode = Mode::LoseAck);
        assert_eq!(
            session
                .submit_keyed(&edit_key(9), ID, &p1, 7, &token())
                .unwrap_err()
                .error,
            SessionError::OutcomeUnknown
        );
        let drained = session.drain(0).unwrap();
        assert_eq!(drained.abandoned, ["edit-0009"]);
        assert_eq!(
            (
                drained.adopted,
                drained.cleared,
                drained.elapsed_ms,
                drained.deadline_met
            ),
            (0, 0, 0, true)
        );
        assert_eq!(
            delivered(drained.report).outcome,
            Outcome::Failure(FailureCode::Unavailable)
        );
        assert_eq!(session.pending(), ["edit-0009"]);
        assert_eq!(session.document_revision(), 7);
        let drained = session.drain(2000).unwrap();
        assert!(drained.abandoned.is_empty());
        assert_eq!((drained.adopted, session.document_revision()), (1, 8));
        rig.assert_balanced(&session);
    });
}

/// Independent oracle for the table test: reads a property straight from the document fields.
fn value_at(document: &folio::StudioDocument, address: &PropertyAddress) -> PropertyValue {
    let id = address.target.node_id.as_str();
    match (&address.target.node_kind, address.property) {
        (NodeKind::Layer, Property::Name) => PropertyValue::String(
            document
                .layers
                .iter()
                .find(|l| l.layer_id == id)
                .unwrap()
                .name
                .clone(),
        ),
        (NodeKind::Layer, Property::Visible) => PropertyValue::Bool(
            document
                .layers
                .iter()
                .find(|l| l.layer_id == id)
                .unwrap()
                .visible,
        ),
        (NodeKind::Artboard, Property::Name) => PropertyValue::String(
            document
                .artboards
                .iter()
                .find(|a| a.artboard_id == id)
                .unwrap()
                .name
                .clone(),
        ),
        (NodeKind::PageSpread, Property::Name) => PropertyValue::String(
            document
                .page_spreads
                .iter()
                .find(|s| s.page_spread_id == id)
                .unwrap()
                .name
                .clone(),
        ),
        (NodeKind::Layer, Property::Opacity) => PropertyValue::number(
            document.layers.iter().find(|l| l.layer_id == id).unwrap().effective_appearance().opacity,
        )
        .unwrap(),
        (NodeKind::Layer, Property::BlendMode) => PropertyValue::Number(
            document.layers.iter().find(|l| l.layer_id == id).unwrap().effective_appearance().blend_mode.into(),
        ),
        (NodeKind::Layer, Property::Tiles) => {
            match &document.layers.iter().find(|l| l.layer_id == id).unwrap().payload {
                folio::Payload::Raster { tiles } => PropertyValue::Tiles(chronicle::TileList(tiles.clone())),
                // Chronicle refuses tiles of a non-raster layer before it compares a value.
                _ => PropertyValue::Tiles(chronicle::TileList(Vec::new())),
            }
        }
        // Chronicle refuses these pairs before it reads a value; the probe only needs a value of
        // the property's type.
        (NodeKind::Artboard | NodeKind::PageSpread, Property::Visible) => PropertyValue::Bool(true),
        (NodeKind::Artboard | NodeKind::PageSpread, Property::Opacity | Property::BlendMode) => {
            PropertyValue::Number(1.into())
        }
        (NodeKind::Artboard | NodeKind::PageSpread, Property::Tiles) => {
            PropertyValue::Tiles(chronicle::TileList(Vec::new()))
        }
        // Structure edits have their own proofs (tests/structure.rs); the table loop skips them.
        (_, Property::Placement) => PropertyValue::Placement(None),
    }
}

/// Writes one property through chronicle against the live snapshot and the session's own revision
/// vector. With `smuggle` it prepares from a copy of the live snapshot whose artboard was renamed.
struct ProbePair {
    address: PropertyAddress,
    smuggle: bool,
}
impl ProposalProvider for ProbePair {
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let live = request.snapshot.document();
        let current = value_at(live, &self.address);
        let written = match (&current, self.address.property) {
            (PropertyValue::Bool(v), _) => PropertyValue::Bool(!v),
            (PropertyValue::String(_), _) => PropertyValue::String(format!("probe-{}", live.revision)),
            (PropertyValue::Number(n), Property::Opacity) => {
                PropertyValue::Number(if n.as_f64() == Some(100.0) { 50 } else { 100 }.into())
            }
            (PropertyValue::Number(n), _) => {
                PropertyValue::Number(if n.as_u64() == Some(2) { 10 } else { 2 }.into())
            }
            // An empty tile list needs no new asset (every remaining tile is in the source).
            (PropertyValue::Tiles(_), _) => PropertyValue::Tiles(chronicle::TileList(Vec::new())),
            (PropertyValue::Placement(_), _) => PropertyValue::Placement(None),
        };
        let mut base = request.snapshot.clone();
        if self.smuggle {
            let mut edited = live.clone();
            edited.artboards[0].name = "smuggled".into();
            base = folio::validate_document(
                edited,
                request.budget.folio,
                request.cancel,
                request.resolver,
            )
            .map_err(|_| ProviderError::Rejected("forge_failed"))?;
        }
        let revision = request
            .revisions
            .iter()
            .find(|e| e.address == self.address)
            .map_or(0, |e| e.revision);
        let patch = Patch {
            transport_version: 1,
            document_id: identity("SDOC", 1),
            base_revision: live.revision,
            command_id: "probe".into(),
            correlation_id: 1,
            actor: patch_actor(),
            reads: vec![PropertyRead {
                address: self.address.clone(),
                expected_revision: revision,
                expected_value: current,
            }],
            writes: vec![PropertyWrite {
                address: self.address.clone(),
                value: written,
            }],
        };
        chronicle::prepare(
            &patch,
            &base,
            request.revisions,
            request.budget,
            request.cancel,
            request.resolver,
        )
        .map_err(provider_error)
    }
}

#[test]
fn session_consumes_the_chronicle_addressability_table() {
    bounded_case(
        "session_consumes_the_chronicle_addressability_table",
        || {
            // A document with every node kind chronicle can address, plus a layer it cannot.
            let mut assets = MemoryAssetSource::new();
            let digest = assets.insert(BLOB.to_vec());
            let mut doc = document_json(&digest.digest);
            let bounds = doc["artboards"][0]["bounds"].clone();
            doc["page_spreads"] = json!([{"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",1),"name":"spread","parent":null,"bounds":bounds}]);
            doc["layers"].as_array_mut().unwrap().push(json!({
            "schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",3),"name":"opaque","parent":null,
            "kind":"vendor-thing","visible":true,
            "payload":{"payload_kind":"unsupported","kind":"vendor-thing","schema_id":"vendor.thing@1","encoded_bytes":[1,2,3],"reason":"preserved"}
        }));
            let snapshot = match folio::inspect_bytes(
                &serde_json::to_vec(&doc).unwrap(),
                folio::Budget::default(),
                &token(),
                &Granted,
            )
            .unwrap()
            {
                folio::Inspection::Editable(s) => s,
                folio::Inspection::ReadOnly(_) => panic!("fixture became read-only"),
            };
            let mut bytes = Vec::new();
            write_package_with(
                &snapshot,
                &assets as &dyn AssetSource,
                &Extras {
                    quarantined: &[],
                    previews: &[],
                },
                &WriteOptions::default(),
                &Limits::default(),
                &token(),
                &mut bytes,
            )
            .unwrap();
            let mut session = Session::open_bytes(
                &bytes,
                SessionConfig::new(actor_context()),
                ports(&Frames::default()),
                &token(),
            )
            .unwrap();

            // The seed is chronicle's own seed, not a session-side rebuild of it.
            assert_eq!(
                session.revisions(),
                chronicle::seed_revisions(session.snapshot().document()).as_slice()
            );

            // Every (node, property) pair: chronicle's table decides what the session seeds and what
            // it lets through. An addressable pair commits, a pair chronicle marks non-addressable is
            // refused by the session with chronicle's code and changes nothing, and the unsupported
            // layer (addressable by type, not by content) is refused invalid_target.
            let nodes = [
                (NodeKind::Layer, identity("SLYR", 1)),
                (NodeKind::Layer, identity("SLYR", 2)),
                (NodeKind::Layer, identity("SLYR", 3)),
                (NodeKind::Artboard, identity("SART", 1)),
                (NodeKind::PageSpread, identity("SPGS", 1)),
            ];
            let unsupported = identity("SLYR", 3);
            let (mut committed, mut refused, mut probe) = (0u64, 0usize, 0usize);
            for (kind, node_id) in &nodes {
                for property in chronicle::PROPERTIES {
                    // Placement (create/move/delete) is proven in tests/structure.rs, where the
                    // fixture has a layer graph; this fixture's counts cover the five property kinds.
                    if property == Property::Placement {
                        continue;
                    }
                    let address = PropertyAddress {
                        target: NodeRef {
                            node_kind: kind.clone(),
                            node_id: node_id.clone(),
                        },
                        property,
                    };
                    let table_says = chronicle::addressable(kind, property);
                    // `tiles` is a raster layer's property: seeded and writable only on layer 2.
                    let raster_only = property != Property::Tiles || *node_id == identity("SLYR", 2);
                    let expect_commit = table_says && *node_id != unsupported && raster_only;
                    assert_eq!(
                        session.revisions().iter().any(|e| e.address == address),
                        expect_commit,
                        "seed disagrees with the chronicle table on {address:?}"
                    );
                    let id = format!("test.probe-{probe}");
                    probe += 1;
                    session
                        .providers_mut()
                        .register(
                            &id,
                            Box::new(ProbePair {
                                address: address.clone(),
                                smuggle: false,
                            }),
                        )
                        .unwrap();
                    let (revision, bytes_before, revisions_before) = (
                        session.document_revision(),
                        session.snapshot().encoded_bytes().to_vec(),
                        session.revisions().to_vec(),
                    );
                    let verdict = session.submit(&id, b"", revision, &token());
                    if expect_commit {
                        let applied = verdict.unwrap_or_else(|e| panic!("{address:?}: {e:?}"));
                        assert!(applied.changed, "{address:?}");
                        assert_eq!(applied.revision, revision + 1);
                        committed += 1;
                        let entry = session
                            .revisions()
                            .iter()
                            .find(|e| e.address == address)
                            .unwrap();
                        assert_eq!(entry.revision, 1, "{address:?}");
                    } else {
                        let code = if table_says {
                            "invalid_target"
                        } else {
                            "wrong_type"
                        };
                        assert_eq!(
                            verdict.unwrap_err().error,
                            SessionError::Provider(code),
                            "{address:?}"
                        );
                        refused += 1;
                        assert_eq!(session.document_revision(), revision);
                        assert_eq!(session.snapshot().encoded_bytes(), bytes_before.as_slice());
                        assert_eq!(session.revisions(), revisions_before.as_slice());
                    }
                }
            }
            // Layer 1 (name, visible, opacity, blend_mode), layer 2 (the same + tiles), the artboard
            // and the page spread (name) commit; tiles of the group, the four non-name properties
            // of the artboard and of the page spread, and all five of the unsupported layer are
            // refused.
            assert_eq!((committed, refused), (11, 14));
            assert_eq!(session.document_revision(), 7 + committed);

            // The session derives lineage through chronicle's mask: a successor that carries one more
            // change than the declared update (here a renamed artboard beside a visibility change) is
            // not a derivation of the live document, so it is refused before anything is published.
            let target = PropertyAddress {
                target: NodeRef {
                    node_kind: NodeKind::Layer,
                    node_id: identity("SLYR", 1),
                },
                property: Property::Visible,
            };
            session
                .providers_mut()
                .register(
                    "test.probe-smuggle",
                    Box::new(ProbePair {
                        address: target,
                        smuggle: true,
                    }),
                )
                .unwrap();
            let (revision, bytes_before) = (
                session.document_revision(),
                session.snapshot().encoded_bytes().to_vec(),
            );
            assert_eq!(
                session
                    .submit("test.probe-smuggle", b"", revision, &token())
                    .unwrap_err()
                    .error,
                SessionError::ProposalNotDerived
            );
            assert_eq!(session.document_revision(), revision);
            assert_eq!(session.snapshot().encoded_bytes(), bytes_before.as_slice());
        },
    );
}

#[test]
fn import_entries_round_trip_without_loss_notes_and_unknown_entries_still_report() {
    bounded_case("import_entries_round_trip", || {
        let imports = [
            hsk_studio_package::ImportEntry::new(
                hsk_studio_package::ImportRole::Records,
                b"{\"records\":[]}".to_vec(),
            ),
            hsk_studio_package::ImportEntry::new(
                hsk_studio_package::ImportRole::Receipt,
                b"{\"loss\":[]}".to_vec(),
            ),
            hsk_studio_package::ImportEntry::new(
                hsk_studio_package::ImportRole::Original,
                (0..4096u32).map(|i| (i % 251) as u8).collect(),
            ),
        ];
        let fixture = Fixture::with_extra_entries(
            "importentries",
            BLOB.to_vec(),
            imports.iter().map(|e| e.to_opaque()).collect(),
        );
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);

        // Import entries are typed, not losses; the unknown opaque entry still is.
        assert_eq!(session.imports().len(), 3);
        assert_eq!(session.loss().len(), 1);
        assert_eq!(session.loss()[0].name, "vendor/extra.bin");
        assert_eq!(
            session.loss()[0].reason,
            hsk_studio_package::LossReason::Opaque
        );

        // Checkpoint re-read accounts for them apart from the genuinely quarantined entry.
        let checkpoint = session.export_checkpoint(&token()).unwrap();
        assert_eq!(checkpoint.imports_preserved, 3);
        assert_eq!(checkpoint.quarantined_preserved, 1);
        let reopened = read_package(
            &checkpoint.bytes,
            &Limits::default(),
            folio::Budget::default(),
            &Granted,
            &token(),
        )
        .unwrap();
        assert_eq!(reopened.imports.len(), 3);
        assert_eq!(reopened.loss.len(), 1);
        for entry in &imports {
            assert_eq!(
                reopened.import_bytes(entry.role),
                Some(entry.bytes.as_slice()),
                "{:?} is byte-identical",
                entry.role
            );
        }

        // Save, reopen through the session: still typed, still no loss note.
        let receipt = session.save(&token()).unwrap();
        assert_eq!(receipt.imports_preserved, 3);
        assert_eq!(receipt.quarantined_preserved, 1);
        let again = open(&fixture, &frames);
        assert_eq!(again.imports(), session.imports());
        assert_eq!(again.loss().len(), 1);
    });
}

/// Lane SMB-UI-SUPPORT-APIS: a pixel edit is a staged tile body plus a chronicle `tiles` write; it
/// commits, serves its bytes to viewers, saves and reopens, and its inverse (undo) restores the
/// original tile; a write naming an unstaged digest is refused before anything is published.
#[test]
fn pixel_edits_stage_tiles_commit_undo_save_and_reopen() {
    bounded_case("pixel_edits_stage_tiles_commit_undo_save_and_reopen", || {
        let fixture = Fixture::new("pixeledit");
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);
        let raster = identity("SLYR", 2);
        let tiles_at = PropertyAddress {
            target: NodeRef { node_kind: NodeKind::Layer, node_id: raster.clone() },
            property: Property::Tiles,
        };
        let original = match &session.snapshot().document().layers[1].payload {
            folio::Payload::Raster { tiles } => tiles.clone(),
            other => panic!("{other:?}"),
        };
        let old = original[0].content_digest.clone();
        assert_eq!(session.render_assets(&token()).unwrap(), BTreeMap::from([(old.digest.clone(), BLOB.to_vec())]));

        let painted: Vec<u8> = (0..64u8).collect();
        let new = session.stage_tile(painted.clone(), &token()).unwrap();
        assert_eq!(session.stage_tile(painted.clone(), &token()).unwrap(), new, "same bytes, one copy");
        assert_eq!(session.staged_bytes(), 64);
        assert_eq!(session.tile_bytes(&new, &token()).unwrap(), painted);
        let mut edited = original.clone();
        edited[0].content_digest = new.clone();
        let tiles_patch = |from: &[folio::TileRef], to: Vec<folio::TileRef>, read_revision: u64, command: &str| Patch {
            transport_version: 1,
            document_id: identity("SDOC", 1),
            // Provenance only: the reads gate the patch.
            base_revision: 7,
            command_id: command.into(),
            correlation_id: 9,
            actor: patch_actor(),
            reads: vec![PropertyRead { address: tiles_at.clone(), expected_revision: read_revision, expected_value: PropertyValue::Tiles(chronicle::TileList(from.to_vec())) }],
            writes: vec![PropertyWrite { address: tiles_at.clone(), value: PropertyValue::Tiles(chronicle::TileList(to)) }],
        };

        // A digest that is neither in the package nor staged is refused, nothing changes.
        let mut ghost = original.clone();
        ghost[0].content_digest.digest = "ab".repeat(32);
        let (revision, before) = (session.document_revision(), session.snapshot().encoded_bytes().to_vec());
        let refused = session.submit(CHRONICLE_PROVIDER_ID, &json_bytes(&tiles_patch(&original, ghost, 0, "ghost")), revision, &token()).unwrap_err();
        assert_eq!(refused.error.code(), "missing_asset");
        assert_eq!((session.document_revision(), session.snapshot().encoded_bytes()), (revision, before.as_slice()));

        // Paint: commit, the render assets now serve the painted tile.
        let applied = session.submit(CHRONICLE_PROVIDER_ID, &json_bytes(&tiles_patch(&original, edited.clone(), 0, "paint")), revision, &token()).unwrap();
        assert!(applied.changed);
        assert_eq!(session.render_assets(&token()).unwrap(), BTreeMap::from([(new.digest.clone(), painted.clone())]));
        let painted_path = fixture.dir.join("painted.handshake");
        session.save_to(&painted_path, &token()).unwrap();
        let reopened = Session::open_path(&painted_path, SessionConfig::new(actor_context()), ports(&frames), &token()).unwrap();
        assert_eq!(reopened.tile_bytes(&new, &token()).unwrap(), painted, "the staged tile was written");

        // Undo through the inverse: the original tile is back, saved and reopened byte-exact.
        let inverse = applied.inverse.unwrap().invocation("undo".into(), 10, patch_actor(), session.document_revision());
        session.submit(CHRONICLE_PROVIDER_ID, &json_bytes(&inverse), session.document_revision(), &token()).unwrap();
        match &session.snapshot().document().layers[1].payload {
            folio::Payload::Raster { tiles } => assert_eq!(tiles, &original),
            other => panic!("{other:?}"),
        }
        let undone_path = fixture.dir.join("undone.handshake");
        session.save_to(&undone_path, &token()).unwrap();
        let again = Session::open_path(&undone_path, SessionConfig::new(actor_context()), ports(&frames), &token()).unwrap();
        assert_eq!(again.render_assets(&token()).unwrap(), BTreeMap::from([(old.digest.clone(), BLOB.to_vec())]));
        // The painted body stays staged for redo, and the original tile replaced before the first
        // save was retained from the old source (that is what let the undo above commit).
        assert_eq!(session.staged_bytes(), 64 + BLOB.len() as u64);

        // Bounds: one body over the package entry limit is refused typed.
        let mut small = SessionConfig::new(actor_context());
        small.budgets.limits.max_entry_bytes = 64 * 1024;
        let mut bounded = Session::open_path(&fixture.path, small, ports(&frames), &token()).unwrap();
        assert_eq!(bounded.stage_tile(vec![0; 64 * 1024 + 1], &token()).unwrap_err().code(), "entry_too_large");
        assert_eq!(bounded.staged_bytes(), 0);
    });
}

/// SMB-HARNESS-UI-3 slice 3: typed document entries (`studio/<token>.<sha256>`) hold state that
/// belongs to the document but is not a Folio record (the active selection, text stories); they
/// survive save, checkpoint and reopen without a loss note, replace in place, and stay bounded.
#[test]
fn document_entries_save_reopen_and_stay_bounded() {
    bounded_case("document_entries_save_reopen_and_stay_bounded", || {
        // A studio-named entry whose name digest is not its body's is an ordinary opaque loss.
        let forged = Quarantined {
            name: format!("{DOCUMENT_ENTRY_PREFIX}selection.{}", "0".repeat(64)),
            bytes: b"forged".to_vec(),
            sha256_hex: sha256_hex_of(b"forged"),
        };
        let fixture = Fixture::with_extra_entries("docentries", BLOB.to_vec(), vec![forged.clone()]);
        let frames = Frames::default();
        let mut session = open(&fixture, &frames);
        assert_eq!(session.document_entry("selection"), None, "a forged name is not a typed entry");
        assert!(session.loss().iter().any(|n| n.name == forged.name));
        assert!(!session.is_dirty());

        session.set_document_entry("selection", Some(b"selection-1".to_vec()), &token()).unwrap();
        assert_eq!(session.document_entry("selection"), Some(&b"selection-1"[..]));
        assert!(session.is_dirty(), "an entry change needs a save");
        let path = fixture.dir.join("entries.handshake");
        session.save_to(&path, &token()).unwrap();
        assert!(!session.is_dirty());
        let reopened = Session::open_path(&path, SessionConfig::new(actor_context()), ports(&frames), &token()).unwrap();
        assert_eq!(reopened.document_entry("selection"), Some(&b"selection-1"[..]));
        let typed = format!("{DOCUMENT_ENTRY_PREFIX}selection.{}", sha256_hex_of(b"selection-1"));
        assert!(!reopened.loss().iter().any(|n| n.name == typed), "the typed entry is no loss");

        // Replace in place (one entry per token), checkpoint carries it, removal drops it.
        session.set_document_entry("selection", Some(b"selection-2".to_vec()), &token()).unwrap();
        session.set_document_entry("text", Some(b"{}".to_vec()), &token()).unwrap();
        let checkpoint = session.export_checkpoint(&token()).unwrap();
        let from_bytes = Session::open_bytes(&checkpoint.bytes, SessionConfig::new(actor_context()), ports(&frames), &token()).unwrap();
        assert_eq!(from_bytes.document_entry("selection"), Some(&b"selection-2"[..]));
        assert_eq!(from_bytes.document_entry("text"), Some(&b"{}"[..]));
        let read = read_package(&checkpoint.bytes, &Limits::default(), folio::Budget::default(), &Granted, &token()).unwrap();
        assert_eq!(read.quarantined.iter().filter(|q| q.name.starts_with("studio/selection.")).count(), 2, "the forged entry and one typed entry");
        session.set_document_entry("selection", None, &token()).unwrap();
        session.save_to(&path, &token()).unwrap();
        let removed = Session::open_path(&path, SessionConfig::new(actor_context()), ports(&frames), &token()).unwrap();
        assert_eq!(removed.document_entry("selection"), None);
        assert_eq!(removed.document_entry("text"), Some(&b"{}"[..]));

        // Refusals change nothing: an unknown token, and all opaque entries over the reopen bound.
        assert_eq!(session.set_document_entry("secrets", Some(vec![1]), &token()).unwrap_err().code(), "unsafe_name");
        let mut small = SessionConfig::new(actor_context());
        small.budgets.limits.max_entry_bytes = 64 * 1024;
        let mut bounded = Session::open_path(&fixture.path, small, ports(&frames), &token()).unwrap();
        assert_eq!(bounded.set_document_entry("selection", Some(vec![0; 64 * 1024]), &token()).unwrap_err().code(), "total_too_large");
        assert_eq!(bounded.document_entry("selection"), None);
        assert!(!bounded.is_dirty());
    });
}

/// SMB-HARNESS-UI-3 slice 5: the pixel-edit path with a commit authority (CommitPort). The
/// authority receives the successor document naming the staged tile; the tile body stays
/// session-local until save, which writes it; a digest that is neither in the package nor staged
/// is refused before the authority is called; a lost acknowledgement is reconciled and the staged
/// body is still served and saved; undo and redo commit through the authority too.
#[test]
fn commit_authority_tile_path_commits_stages_saves_and_reconciles() {
    bounded_case("commit_authority_tile_path", || {
        let rig = Rig::new("authoritytiles", 4);
        let mut session = rig.session();
        let raster = identity("SLYR", 2);
        let tiles_at = PropertyAddress {
            target: NodeRef { node_kind: NodeKind::Layer, node_id: raster },
            property: Property::Tiles,
        };
        let original = match &session.snapshot().document().layers[1].payload {
            folio::Payload::Raster { tiles } => tiles.clone(),
            other => panic!("{other:?}"),
        };
        let patch = |from: &[folio::TileRef], to: Vec<folio::TileRef>, read_revision: u64, command: &str| Patch {
            transport_version: 1,
            document_id: identity("SDOC", 1),
            base_revision: 7,
            command_id: command.into(),
            correlation_id: 11,
            actor: patch_actor(),
            reads: vec![PropertyRead { address: tiles_at.clone(), expected_revision: read_revision, expected_value: PropertyValue::Tiles(chronicle::TileList(from.to_vec())) }],
            writes: vec![PropertyWrite { address: tiles_at.clone(), value: PropertyValue::Tiles(chronicle::TileList(to)) }],
        };

        // Unstaged digest: refused missing_asset before the authority sees anything.
        let mut ghost = original.clone();
        ghost[0].content_digest.digest = "cd".repeat(32);
        let calls = rig.authority.commit_calls();
        let refused = session.submit_keyed(&key("tile-ghost"), ID, &json_bytes(&patch(&original, ghost, 0, "ghost")), 7, &token()).unwrap_err();
        assert_eq!(refused.error.code(), "missing_asset");
        assert_eq!((rig.authority.commit_calls(), rig.authority.commits(), session.document_revision()), (calls, 0, 7));

        // Staged tile, keyed commit: the authority holds the successor naming the staged digest.
        let painted: Vec<u8> = (100..164u8).collect();
        let staged = session.stage_tile(painted.clone(), &token()).unwrap();
        let mut edited = original.clone();
        edited[0].content_digest = staged.clone();
        let applied = session.submit_keyed(&key("tile-paint"), ID, &json_bytes(&patch(&original, edited.clone(), 0, "paint")), 7, &token()).unwrap();
        assert!(applied.changed && applied.commit.is_some());
        assert_eq!(rig.authority.commits(), 1);
        let committed = document_of(&rig.authority.last_document());
        assert_eq!(committed.encoded_bytes(), session.snapshot().encoded_bytes(), "the authority holds the published document");
        assert!(String::from_utf8_lossy(&rig.authority.last_document()).contains(&staged.digest));
        assert_eq!(session.render_assets(&token()).unwrap(), BTreeMap::from([(staged.digest.clone(), painted.clone())]));
        // The authority holds the document only: the staged body is written by the session save.
        let saved = rig.fixture.dir.join("authority-painted.handshake");
        session.save_to(&saved, &token()).unwrap();
        // Reopens report to their own sink: the rig balances the authority session only.
        let reopened = Session::open_path(&saved, SessionConfig::new(actor_context()), ports(&Frames::default()), &token()).unwrap();
        assert_eq!(reopened.tile_bytes(&staged, &token()).unwrap(), painted);
        assert_eq!(reopened.snapshot().encoded_bytes(), committed.encoded_bytes());

        // Undo through the authority after a lost acknowledgement: reconcile adopts it exactly once.
        let inverse = applied.inverse.clone().unwrap().invocation("undo".into(), 12, patch_actor(), 8);
        rig.authority.with(|s| s.mode = Mode::LoseAck);
        let lost = session.submit_keyed(&key("tile-undo"), ID, &json_bytes(&inverse), 8, &token()).unwrap_err();
        assert_eq!(lost.error.code(), "outcome_unknown");
        assert_eq!(session.document_revision(), 8, "nothing adopted before the lookup");
        let report = session.reconcile(&token()).unwrap();
        assert_eq!((report.adopted, report.still_unknown), (1, 0));
        assert_eq!(session.document_revision(), 9);
        match &session.snapshot().document().layers[1].payload {
            folio::Payload::Raster { tiles } => assert_eq!(tiles, &original),
            other => panic!("{other:?}"),
        }
        assert_eq!(rig.authority.commits(), 2);
        // Redo finds the staged body still held, and the save writes it.
        let redo = patch(&original, edited, 2, "redo");
        session.submit_keyed(&key("tile-redo"), ID, &json_bytes(&redo), 9, &token()).unwrap();
        let redone = rig.fixture.dir.join("authority-redone.handshake");
        session.save_to(&redone, &token()).unwrap();
        let again = Session::open_path(&redone, SessionConfig::new(actor_context()), ports(&Frames::default()), &token()).unwrap();
        assert_eq!(again.render_assets(&token()).unwrap(), BTreeMap::from([(staged.digest.clone(), painted)]));
        assert_eq!(rig.authority.commits(), 3);
        rig.assert_balanced(&session);
    });
}
