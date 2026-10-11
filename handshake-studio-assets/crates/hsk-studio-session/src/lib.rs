//! Headless single-document operation orchestration, provider registration and test ports. Multi-
//! output delivery captures immutable revision/presets/output verification and uses existing Job
//! Runtime via a port for queue/watchfolder/cancel/retry, without a second job database or queue
//! authority. No built-in all-engine mega dependency.
#![forbid(unsafe_code)]

mod admission;
mod error;
mod ports;
mod registry;
mod session;
mod source;

pub use admission::{Admission, Permit};
pub use error::{Failed, ProviderError, RegistryError, ReportStatus, SessionError};
pub use hsk_studio_package::{PixelFormat, PreviewImage, PreviewInfo};
pub use ports::{
    Action, Clock, CommitOutcome, CommitPort, CommitReceipt, CommitRequest, Cost, GrantError,
    GrantPort, GrantToken, IdempotencyKey, JobPort, JobRefused, JobScope, JobTicket, Lookup,
    MonotonicClock,
};
pub use registry::{
    CHRONICLE_PROVIDER_ID, ChronicleProvider, MAX_PROVIDER_ID_BYTES, MAX_PROVIDERS,
    ProposalProvider, ProposalRequest, ProviderInfo, ProviderRegistry, provider_error,
};
pub use session::{
    Applied, Budgets, Checkpoint, DrainReport, MAX_SUPPLIED_PREVIEWS, PreviewReceipt,
    ReconcileReport, SaveReceipt, Session, SessionConfig, SessionPorts,
};

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-SESSION","version":3,"api":"Session::open_path / open_bytes / submit / submit_keyed / reconcile / close_admission / drain / set_previews / save / save_to / export_checkpoint / source_bytes_read / resident_bytes; ProviderRegistry::register / ids / list; SessionPorts{sink,resolver,providers,grants,commits,jobs,admission,clock}; SessionConfig{actor,budgets,asset_codec,epoch}","consumer":"session-consumer --descriptor | --demo DIR | --open IN.handshake --rename LAYER_ID NAME [--hide LAYER_ID] --save OUT.handshake [--cancel]","input":"A .handshake package file or bytes (package), a provider id plus opaque params (built-in chronicle.patch: a chronicle Patch as JSON), the expected document revision, an optional caller-supplied IdempotencyKey, an accord CancellationToken","output":"Applied (revisions, revision updates, inverse, authority receipt, replayed, cancel_too_late), SaveReceipt, Checkpoint (verified in-memory package + sha256), ReconcileReport, DrainReport, each carrying one Observe ReportStatus; failures return Failed{error,report} with a stable error code","limits":{"package":"package::Limits defaults","chronicle":"chronicle::Budget defaults (folio budget inside)","providers":64,"provider_id_bytes":64,"max_receipts":1024,"admission":"slots and bytes, non-blocking, RAII release; default 16 slots / 256 MiB","observe_frames_per_operation":1},"recovery":"A refused, overloaded or canceled submit leaves the snapshot, property revisions, receipts and files untouched; an unknown commit outcome stays pending under its key until reconcile/retry looks it up (adopted exactly once, or cleared and retried safely); atomic streaming save keeps the last-good file byte-identical on any failure (a corrupted source asset fails during the stream with hash_mismatch or length_mismatch and the temp is discarded); unknown package entries are re-emitted verbatim; asset and preview bodies stay in the source package and are read one at a time on demand (hash-verified; a corrupted body fails the save with a typed code and leaves the last-good file intact); previews are written only while they describe the revision being written: host-supplied ones (set_previews, for the current revision) replace the source previews, anything else is dropped and counted","manual":"Build SessionConfig (ActorContext, epoch) and SessionPorts (SinkPort; optionally GrantPort, CommitPort, JobPort, Admission, Clock), Session::open_path, read session.revisions(), build a chronicle Patch whose reads quote the current values and revisions, submit_keyed(&key, CHRONICLE_PROVIDER_ID, patch_json, session.document_revision(), &token) (a commit authority requires a key), on outcome_unknown call reconcile, then save or export_checkpoint. Shutdown: drain(deadline_ms) closes admission and reports unresolved keys.","diagnostics":"SessionError::code() stable snake_case; one terminal Observe frame per operation (open, submit, reconcile, drain, save, export) with Success, Canceled or Failure(Validation|Unavailable|Unsupported); no names, paths or document text are emitted","authority":"Single document, single owner: edits are serialized by &mut self. Standalone, the session is its own commit authority; with a CommitPort the authority is canonical and the session adopts a transaction only after its receipt. Grants, the commit/receipt store and the Job Runtime are injected ports; the session mints no identifiers and owns no queue, ledger, history store or second CAS. Memory: at rest the session holds the document, preserved unknown entries and supplied previews; save streams through package save_atomic_streaming (raw assets flow from the source file range to the temp file in fixed chunks, SHA-256 and length verified on the way, zstd or shuffle_zstd assets one blob at a time; the finished temp is re-verified from disk before the atomic replace), so save peak heap does not grow with asset size (measured 1.3 MiB at 1 MiB and at 16 MiB assets); export_checkpoint still buffers the whole package because a Checkpoint is an in-memory verified package by contract. Lineage: a provider's successor is accepted only if it equals the live document plus new values at exactly the declared revision updates (proposal_not_derived otherwise). Pending: multi-output delivery, public wire envelopes (kernel-bindings), host-scoped multi-document arbitration"}"#;
