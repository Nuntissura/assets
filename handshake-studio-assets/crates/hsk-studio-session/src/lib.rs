//! Headless single-document operation orchestration, provider registration and test ports. Multi-
//! output delivery captures immutable revision/presets/output verification and uses existing Job
//! Runtime via a port for queue/watchfolder/cancel/retry, without a second job database or queue
//! authority. No built-in all-engine mega dependency.
#![forbid(unsafe_code)]

mod error;
mod registry;
mod session;

pub use error::{Failed, ProviderError, RegistryError, ReportStatus, SessionError};
pub use registry::{
    CHRONICLE_PROVIDER_ID, ChronicleProvider, MAX_PROVIDER_ID_BYTES, MAX_PROVIDERS,
    ProposalProvider, ProposalRequest, ProviderInfo, ProviderRegistry, provider_error,
};
pub use session::{Applied, Budgets, SaveReceipt, Session, SessionConfig, SessionPorts};

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-SESSION","version":1,"api":"Session::open_path / open_bytes / submit / save / save_to; ProviderRegistry::register / ids / list; SessionPorts{sink,resolver,providers}; SessionConfig{actor,budgets,asset_codec}","consumer":"session-consumer --descriptor | --demo DIR | --open IN.handshake --rename LAYER_ID NAME [--hide LAYER_ID] --save OUT.handshake [--cancel]","input":"A .handshake package file or bytes (package), a provider id plus opaque params (built-in chronicle.patch: a chronicle Patch as JSON), the expected document revision, an accord CancellationToken","output":"Applied (revisions, revision updates, inverse) and SaveReceipt, each carrying one Observe ReportStatus; failures return Failed{error,report} with a stable error code","limits":{"package":"package::Limits defaults","chronicle":"chronicle::Budget defaults (folio budget inside)","providers":64,"provider_id_bytes":64,"observe_frames_per_operation":1},"recovery":"A refused or canceled submit leaves the snapshot, property revisions and files untouched; atomic save keeps the last-good file byte-identical on any failure; unknown package entries are re-emitted verbatim; derived previews are dropped on save and counted","manual":"Build SessionConfig (ActorContext) and SessionPorts (SinkPort), Session::open_path, read session.revisions() for property revisions, build a chronicle Patch whose reads quote the current values and revisions, submit(CHRONICLE_PROVIDER_ID, patch_json, session.document_revision(), &token), then save(&token). Register more providers on session.providers_mut() or a ProviderRegistry passed in SessionPorts.","diagnostics":"SessionError::code() stable snake_case; one terminal Observe frame per operation (open, submit, save) with Success, Canceled or Failure(Validation|Unavailable|Unsupported); no names, paths or document text are emitted","authority":"Single document, single owner: edits are serialized by &mut self and the session is the only commit authority for its in-memory document. Not a job scheduler, queue, history store, grant authority or second CAS. Pending: idempotency keys and receipts, injected grant/commit/job ports, shutdown drain, asset streaming (assets are held in memory after open), multi-output delivery"}"#;
