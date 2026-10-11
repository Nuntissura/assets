//! Headless single-document session: open (package -> folio snapshot), submit a provider-proposed
//! chronicle transaction under revision checks (optionally keyed, granted, admitted and committed
//! through injected ports), save and export (package). One session owns one document, so edits
//! are serialized by `&mut self`; with a commit authority injected, the authority is canonical
//! and the session adopts a transaction locally only after the authority's receipt.
use crate::{
    admission::{Admission, JobGuard},
    error::{AuthorityFault, Failed, ReportStatus, SessionError},
    ports::{
        Action, Clock, CommitOutcome, CommitPort, CommitReceipt, CommitRequest, Cost, GrantError,
        GrantPort, GrantToken, IdempotencyKey, JobPort, JobScope, Lookup, MonotonicClock,
    },
    registry::{ProposalRequest, ProviderRegistry},
    source::{Handle, LazyAssets, SourceBox, load_parts, require_tile_assets},
};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_chronicle::{
    self as chronicle, Inverse, RevisionEntry, RevisionUpdate, SourceResult,
};
use hsk_studio_folio::{
    self as folio, NodeKind, NodeRef, Payload, Resolver, Snapshot, StudioDocument, Unresolved,
};
use hsk_studio_observe::{Budget as ObserveBudget, Observation, Observe, Outcome, SinkPort};
use hsk_studio_package::{
    Codec, Extras, Limits, LossNote, Manifest, PackageError, PackageReader, PreviewImage,
    PreviewRef, Quarantined, StreamSave, WriteOptions, WriteReceipt, read_package,
    save_atomic_streaming, sha256_hex_of, write_package_with,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

/// Most preview images a host may supply at once.
pub const MAX_SUPPLIED_PREVIEWS: usize = 16;

/// What the idempotency table does when it holds `Budgets::max_receipts` receipts and a new key
/// arrives. A receipt is acknowledged once the caller holds it: a committed `submit_keyed`
/// result, a replay of the key, or `Session::acknowledge_receipt`. A pending receipt (outcome
/// unknown) and a committed receipt adopted by `reconcile` or `drain` that no result has handed
/// to the caller yet are unacknowledged. Test:
/// `receipt_table_is_bounded_and_evicts_only_acknowledged_receipts` and
/// `unacknowledged_and_pending_receipts_are_kept_and_a_full_table_refuses_typed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptPolicy {
    /// Refuse the new key with `receipts_full`; every held receipt stays.
    RefuseWhenFull,
    /// Evict the oldest acknowledged committed receipt to admit the new key, and report it
    /// (`Applied::receipts_evicted`, `ReceiptStats::evicted`). When none qualifies the new key
    /// is refused with `receipts_full`. Replaying an evicted key is not a replay: the session
    /// is at a later revision, so the retry is refused `stale_revision` and applies nothing.
    EvictAcknowledged,
}

/// Bounds for every operation: package container limits, chronicle (which carries folio) budgets
/// and the idempotency table. Cancellation is the accord `CancellationToken` passed to each call.
#[derive(Clone, Copy, Debug)]
pub struct Budgets {
    pub limits: Limits,
    pub chronicle: chronicle::Budget,
    /// Idempotency receipts (committed and pending) a session keeps at once; `receipt_policy`
    /// decides what happens to a new key when the table is full.
    pub max_receipts: usize,
    pub receipt_policy: ReceiptPolicy,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            chronicle: chronicle::Budget::default(),
            max_receipts: 1024,
            receipt_policy: ReceiptPolicy::EvictAcknowledged,
        }
    }
}

impl Budgets {
    /// The folio budget used to open and validate documents.
    pub fn folio(&self) -> folio::Budget {
        self.chronicle.folio
    }

    fn valid(&self) -> bool {
        let l = &self.limits;
        let c = &self.chronicle;
        l.max_entries > 0
            && l.max_entry_bytes > 0
            && l.max_total_bytes > 0
            && l.max_manifest_bytes > 0
            && c.input_bytes > 0
            && c.reads > 0
            && c.writes > 0
            && c.value_bytes > 0
            && c.folio.input_bytes > 0
            && self.max_receipts > 0
    }
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// The principal operating this session (grants, binding and attribution of reports).
    pub actor: ActorContext,
    pub budgets: Budgets,
    /// Codec for asset blobs on save. `None` keeps what the opened package used.
    pub asset_codec: Option<Codec>,
    /// Lease epoch sent with every commit; the authority fences a session whose epoch is old.
    pub epoch: u64,
}

impl SessionConfig {
    pub fn new(actor: ActorContext) -> Self {
        Self {
            actor,
            budgets: Budgets::default(),
            asset_codec: None,
            epoch: 0,
        }
    }
}

/// Caller-owned collaborators. No global state: everything a session touches is in here.
pub struct SessionPorts {
    /// Receives exactly one terminal observation per operation.
    pub sink: Box<dyn SinkPort>,
    /// Decides which document references are available (defaults to nothing available).
    pub resolver: Box<dyn Resolver + Send + Sync>,
    /// Providers this session can submit to.
    pub providers: ProviderRegistry,
    /// Fresh grant check per operation. `None`: standalone use, everything is allowed.
    pub grants: Option<Box<dyn GrantPort>>,
    /// Canonical commit/receipt authority. `None`: the session is its own authority (standalone).
    pub commits: Option<Box<dyn CommitPort>>,
    /// Existing Job Runtime, reached through a port. `None`: only local admission applies.
    pub jobs: Option<Arc<dyn JobPort>>,
    /// Local bounded admission (slots and bytes); clone one handle to share a budget.
    pub admission: Admission,
    pub clock: Box<dyn Clock>,
}

impl SessionPorts {
    pub fn new(sink: Box<dyn SinkPort>) -> Self {
        Self {
            sink,
            resolver: Box::new(Unresolved),
            providers: ProviderRegistry::with_builtin(),
            grants: None,
            commits: None,
            jobs: None,
            admission: Admission::default(),
            clock: Box::new(MonotonicClock::new()),
        }
    }

    pub fn with_resolver(mut self, resolver: Box<dyn Resolver + Send + Sync>) -> Self {
        self.resolver = resolver;
        self
    }

    pub fn with_providers(mut self, providers: ProviderRegistry) -> Self {
        self.providers = providers;
        self
    }

    pub fn with_grants(mut self, grants: Box<dyn GrantPort>) -> Self {
        self.grants = Some(grants);
        self
    }

    pub fn with_commits(mut self, commits: Box<dyn CommitPort>) -> Self {
        self.commits = Some(commits);
        self
    }

    pub fn with_jobs(mut self, jobs: Arc<dyn JobPort>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    pub fn with_admission(mut self, admission: Admission) -> Self {
        self.admission = admission;
        self
    }

    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// Result of a committed (or no-change) submit.
#[derive(Clone, Debug)]
pub struct Applied {
    pub provider: String,
    pub command_id: String,
    /// The provider-side correlation id carried by the transaction.
    pub correlation_id: u64,
    pub actor: chronicle::Actor,
    pub previous_revision: u64,
    pub revision: u64,
    /// False when the transaction wrote values the document already held (revision unchanged).
    pub changed: bool,
    pub revision_updates: Vec<RevisionUpdate>,
    /// Transaction that restores the previous values; submit it like any other.
    pub inverse: Option<Inverse>,
    /// The authority's canonical receipt (`None` for no-change or a standalone session).
    pub commit: Option<CommitReceipt>,
    /// True when this is the stored receipt of an earlier identical request: nothing was
    /// recomputed or re-applied.
    pub replayed: bool,
    /// The token was canceled while the commit was already happening: the edit stands.
    pub cancel_too_late: bool,
    /// Acknowledged receipts evicted from the idempotency table to admit this submit's key
    /// (`ReceiptPolicy::EvictAcknowledged`); zero for a replay.
    pub receipts_evicted: u32,
    pub report: ReportStatus,
}

/// A snapshot of the idempotency table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiptStats {
    /// Receipts held now (committed and pending); at most `capacity` (test
    /// `receipt_table_is_bounded_and_evicts_only_acknowledged_receipts`).
    pub held: usize,
    pub capacity: usize,
    /// Pending receipts (commit outcome unknown or diverged).
    pub pending: usize,
    /// Committed receipts the caller has not been handed yet (adopted by `reconcile`/`drain`).
    pub unacknowledged: usize,
    /// Acknowledged receipts evicted since open.
    pub evicted: u64,
}

/// How a pending commit disagrees with the authority's receipt for its key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Disagreement {
    /// The receipt's binding is not the binding of the request this session sent.
    pub binding: bool,
    /// The receipt names another revision than the pending transaction's successor.
    pub revision: bool,
    /// The session's document is no longer at the pending transaction's base revision.
    pub base: bool,
}

impl Disagreement {
    pub const fn any(self) -> bool {
        self.binding || self.revision || self.base
    }
}

/// The operator's decision for a diverged pending commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Divergence {
    /// The pending transaction was dropped; the session's document is unchanged and may now be
    /// behind the authority (the next commit then answers `stale_revision`).
    Discarded,
    /// The session's document was replaced by the authority's document at the receipt revision.
    Resynced,
}

/// Outcome of [`Session::discard_diverged`] / [`Session::resync_diverged`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DivergenceReport {
    pub key: String,
    pub resolution: Divergence,
    pub revision_before: u64,
    pub revision_after: u64,
    /// The revision the dropped pending transaction would have produced.
    pub draft_revision: u64,
    /// The authority's receipt that disagreed.
    pub authority: CommitReceipt,
    pub disagreement: Disagreement,
    pub report: ReportStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReceipt {
    pub revision: u64,
    pub entries: u32,
    pub bytes_written: u64,
    /// Unknown package entries re-emitted verbatim.
    pub quarantined_preserved: usize,
    /// Preview entries written (only previews that describe the revision being written).
    pub previews_preserved: usize,
    /// Preview images held or declared but not written: they described another revision, were
    /// replaced by host-supplied ones, or could not be read within bounds.
    pub previews_dropped: usize,
    pub report: ReportStatus,
}

/// Outcome of [`Session::set_previews`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewReceipt {
    pub revision: u64,
    pub accepted: usize,
    pub bytes: u64,
    pub report: ReportStatus,
}

/// In-memory package of the current revision, re-read and verified before it is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub revision: u64,
    pub bytes: Vec<u8>,
    pub sha256_hex: String,
    pub entries: u32,
    pub quarantined_preserved: usize,
    pub previews_preserved: usize,
    pub previews_dropped: usize,
    pub report: ReportStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Pending commits the authority had durably committed; adopted locally exactly once.
    pub adopted: u32,
    /// Pending commits the authority never saw; the caller may retry them safely.
    pub cleared: u32,
    /// Authority unreachable: still pending.
    pub still_unknown: u32,
    /// Authority receipt disagrees with the pending transaction: still pending, needs a human
    /// (`discard_diverged` or `resync_diverged`); not resolved automatically (test
    /// `diverged_pending_commit_is_resolved_only_by_the_operator_discard_or_resync`).
    pub diverged: u32,
    /// Keys counted in `diverged`.
    pub diverged_keys: Vec<String>,
    pub report: ReportStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrainReport {
    pub adopted: u32,
    pub cleared: u32,
    /// Keys of unresolved pending commits, reported rather than dropped.
    pub abandoned: Vec<String>,
    pub elapsed_ms: u64,
    pub deadline_met: bool,
    pub report: ReportStatus,
}

enum EntryState {
    Committed(Box<Applied>),
    Pending(Box<PendingCommit>),
}

struct Entry {
    binding: String,
    state: EntryState,
    /// Insertion order: eviction takes the oldest acknowledged receipt first.
    seq: u64,
    /// The caller holds this receipt (see `ReceiptPolicy`); only acknowledged receipts evict.
    acknowledged: bool,
}

struct PendingCommit {
    base_revision: u64,
    successor: Snapshot,
    slots: Vec<(usize, u64)>,
    draft: Applied,
    /// Set when the authority's receipt for this key disagreed (only an operator command clears it; same test).
    diverged: Option<(CommitReceipt, Disagreement)>,
}

enum Resolution {
    Adopted,
    Cleared,
    StillUnknown,
    Diverged,
}

/// Counts of `resolve_all`.
#[derive(Default)]
struct Tally {
    adopted: u32,
    cleared: u32,
    unknown: u32,
    diverged: u32,
}

impl Tally {
    fn bump(counter: &mut u32) {
        *counter = counter.saturating_add(1);
    }
}

struct Proposed {
    applied: Applied,
    successor: Option<Snapshot>,
    slots: Vec<(usize, u64)>,
}

struct Built {
    bytes: Vec<u8>,
    receipt: WriteReceipt,
    previews_written: usize,
    previews_dropped: usize,
}

pub struct Session {
    config: SessionConfig,
    ports: SessionPorts,
    resource: DomainId,
    snapshot: Snapshot,
    revisions: Vec<RevisionEntry>,
    /// The package the session was opened from (or last saved to), read on demand: asset and
    /// preview bodies are not held between operations (test
    /// `lazy_source_holds_no_assets_and_save_streams_them_on_demand`).
    source: Mutex<PackageReader<Handle>>,
    /// A second cursor over the same stream as `source`, forked to stream raw asset ranges.
    raw: Handle,
    source_bytes: Arc<AtomicU64>,
    quarantined: Vec<Quarantined>,
    loss: Vec<LossNote>,
    /// Previews declared by `source` (bodies stay in the source until written).
    preview_refs: Vec<PreviewRef>,
    /// The document revision the previews in `source` describe.
    preview_revision: u64,
    /// Host-supplied previews and the revision they were rendered for.
    supplied_previews: Option<(u64, Vec<PreviewImage>)>,
    asset_codec: Codec,
    origin: Option<PathBuf>,
    saved_revision: Option<u64>,
    receipts: BTreeMap<String, Entry>,
    receipt_seq: u64,
    receipts_evicted: u64,
    closed: bool,
    operations: u64,
    last_report: ReportStatus,
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Session>();
};

fn emit_report(
    sink: &mut dyn SinkPort,
    correlation_id: u64,
    revision: u64,
    resource: Option<&DomainId>,
    actor: &ActorContext,
    outcome: Outcome,
) -> ReportStatus {
    let Some(resource) = resource else {
        return ReportStatus::Unaddressed;
    };
    let budget = match ObserveBudget::new(0, 0) {
        Ok(budget) => budget,
        Err(error) => return ReportStatus::Undelivered(error),
    };
    let mut observe = Observe::new(
        correlation_id,
        revision,
        resource.clone(),
        actor.clone(),
        budget,
    );
    // A fresh token on purpose: the operation decided its own terminal outcome, and a cancel
    // raised afterwards must not recast an already committed edit as canceled.
    let fresh = CancellationToken::default();
    match observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        &fresh,
        sink,
    ) {
        Ok(receipt) => ReportStatus::Delivered(receipt),
        Err(error) => ReportStatus::Undelivered(error),
    }
}

/// Every chronicle property. Mirrored here because chronicle exports no property table; drift
/// is caught by `chronicle_property_table_matches_the_session_seed`, and `addressable` below
/// stops compiling when chronicle or folio gain a property or node kind.
const PROPERTIES: [chronicle::Property; 2] =
    [chronicle::Property::Name, chronicle::Property::Visible];

/// Whether chronicle can address `property` on a `kind` node. Exhaustive over node kind x
/// property on purpose: a new variant on either side is a compile error here, not a silent
/// fall-through.
fn addressable(kind: &NodeKind, property: chronicle::Property) -> bool {
    match (kind, property) {
        (NodeKind::Layer, chronicle::Property::Name | chronicle::Property::Visible)
        | (NodeKind::Artboard | NodeKind::PageSpread, chronicle::Property::Name) => true,
        (NodeKind::Artboard | NodeKind::PageSpread, chronicle::Property::Visible) => false,
    }
}

fn seed_revisions(document: &StudioDocument) -> Vec<RevisionEntry> {
    let mut vector = Vec::new();
    let mut seed = |node_kind: NodeKind, node_id: &str| {
        for property in PROPERTIES {
            if addressable(&node_kind, property) {
                vector.push(RevisionEntry {
                    address: chronicle::PropertyAddress {
                        target: NodeRef {
                            node_kind: node_kind.clone(),
                            node_id: node_id.to_owned(),
                        },
                        property,
                    },
                    revision: 0,
                });
            }
        }
    };
    for layer in &document.layers {
        if !matches!(layer.payload, Payload::Unsupported { .. }) {
            seed(NodeKind::Layer, &layer.layer_id);
        }
    }
    for artboard in &document.artboards {
        seed(NodeKind::Artboard, &artboard.artboard_id);
    }
    for spread in &document.page_spreads {
        seed(NodeKind::PageSpread, &spread.page_spread_id);
    }
    vector
}

/// Sets the addressed property to a neutral value so two documents can be compared without it.
/// False when the address names no such node or property. Exhaustive like `addressable`.
fn mask_property(document: &mut StudioDocument, address: &chronicle::PropertyAddress) -> bool {
    let id = address.target.node_id.as_str();
    match (&address.target.node_kind, address.property) {
        (&NodeKind::Layer, chronicle::Property::Name) => document
            .layers
            .iter_mut()
            .find(|layer| layer.layer_id == id)
            .map(|layer| layer.name.clear())
            .is_some(),
        (&NodeKind::Layer, chronicle::Property::Visible) => document
            .layers
            .iter_mut()
            .find(|layer| layer.layer_id == id)
            .map(|layer| layer.visible = false)
            .is_some(),
        (&NodeKind::Artboard, chronicle::Property::Name) => document
            .artboards
            .iter_mut()
            .find(|artboard| artboard.artboard_id == id)
            .map(|artboard| artboard.name.clear())
            .is_some(),
        (&NodeKind::PageSpread, chronicle::Property::Name) => document
            .page_spreads
            .iter_mut()
            .find(|spread| spread.page_spread_id == id)
            .map(|spread| spread.name.clear())
            .is_some(),
        (&NodeKind::Artboard | &NodeKind::PageSpread, chronicle::Property::Visible) => false,
    }
}

/// True when `successor` is `base` plus new values at exactly the declared `updates` addresses
/// (and the successor's revision). A provider may have prepared its transaction from some other
/// copy of the document; nothing else it changed may ride along unrevisioned.
fn derives_from(
    base: &StudioDocument,
    successor: &StudioDocument,
    updates: &[RevisionUpdate],
) -> bool {
    let mut expected = base.clone();
    let mut actual = successor.clone();
    expected.revision = actual.revision;
    updates
        .iter()
        .all(|u| mask_property(&mut expected, &u.address) && mask_property(&mut actual, &u.address))
        && expected == actual
}

fn inherited_codec(manifest: &Manifest) -> Codec {
    let mut codec = Codec::Raw;
    for entry in &manifest.entries {
        match entry.codec {
            Codec::ShuffleZstd => return Codec::ShuffleZstd,
            Codec::Zstd => codec = Codec::Zstd,
            Codec::Raw => {}
        }
    }
    codec
}

impl Session {
    /// Opens a package file. The file stays open as the lazy source: its structure and every
    /// entry checksum are verified in bounded chunks, but asset and preview bodies are not kept.
    pub fn open_path(
        path: &Path,
        config: SessionConfig,
        ports: SessionPorts,
        cancel: &CancellationToken,
    ) -> Result<Session, Failed> {
        match File::open(path) {
            Ok(file) => Self::open_stream(
                Box::new(file),
                Some(path.to_path_buf()),
                config,
                ports,
                cancel,
            ),
            Err(error) => Err(Failed {
                error: SessionError::Io(error.kind()),
                report: ReportStatus::Unaddressed,
            }),
        }
    }

    /// Opens a package held in memory (no origin path, so `save_to` is the only way to save).
    /// The session keeps its own copy of `bytes` until a save replaces the source.
    pub fn open_bytes(
        bytes: &[u8],
        config: SessionConfig,
        ports: SessionPorts,
        cancel: &CancellationToken,
    ) -> Result<Session, Failed> {
        Self::open_stream(
            Box::new(Cursor::new(bytes.to_vec())),
            None,
            config,
            ports,
            cancel,
        )
    }

    fn open_stream(
        source: SourceBox,
        origin: Option<PathBuf>,
        config: SessionConfig,
        mut ports: SessionPorts,
        cancel: &CancellationToken,
    ) -> Result<Session, Failed> {
        let unaddressed = |error: SessionError| Failed {
            error,
            report: ReportStatus::Unaddressed,
        };
        if !config.budgets.valid() {
            return Err(unaddressed(SessionError::InvalidBudget));
        }
        let source_bytes = Arc::new(AtomicU64::new(0));
        let (for_reader, mut raw) = Handle::pair(source, Arc::clone(&source_bytes));
        let reader = PackageReader::open(for_reader, &config.budgets.limits, cancel)
            .map_err(|e| unaddressed(SessionError::from_package(e)))?;
        let manifest_revision = reader.manifest().revision;
        let resource = DomainId::parse(&reader.manifest().document_id).ok();
        let inherited = inherited_codec(reader.manifest());
        let correlation = 1;
        let result = load_parts(
            reader.manifest(),
            reader.entries(),
            &mut raw,
            &config.budgets.limits,
            config.budgets.folio(),
            &*ports.resolver,
            cancel,
        )
        .and_then(|parts| {
            let resource = DomainId::parse(&parts.snapshot.document().document_id)
                .map_err(|_| SessionError::ReadOnlyDocument)?;
            Ok((resource, parts))
        });
        match result {
            Ok((resource, parts)) => {
                let revision = parts.snapshot.document().revision;
                let report = emit_report(
                    &mut *ports.sink,
                    correlation,
                    revision,
                    Some(&resource),
                    &config.actor,
                    Outcome::Success,
                );
                Ok(Session {
                    asset_codec: config.asset_codec.unwrap_or(inherited),
                    config,
                    ports,
                    resource,
                    revisions: seed_revisions(parts.snapshot.document()),
                    snapshot: parts.snapshot,
                    preview_refs: reader.previews(),
                    source: Mutex::new(reader),
                    raw,
                    source_bytes,
                    quarantined: parts.quarantined,
                    loss: parts.loss,
                    preview_revision: revision,
                    supplied_previews: None,
                    origin,
                    saved_revision: Some(revision),
                    receipts: BTreeMap::new(),
                    receipt_seq: 0,
                    receipts_evicted: 0,
                    closed: false,
                    operations: 1,
                    last_report: report,
                })
            }
            Err(error) => {
                let report = emit_report(
                    &mut *ports.sink,
                    correlation,
                    manifest_revision,
                    resource.as_ref(),
                    &config.actor,
                    error.outcome(),
                );
                Err(Failed { error, report })
            }
        }
    }

    // ----- submit ------------------------------------------------------------------------

    /// Submits `params` to the registered provider `provider` against the document revision the
    /// caller last observed. Nothing is published unless the provider's transaction is computed
    /// on the session's current revision, advances it by exactly one and follows the session's
    /// property revision vector; any refusal leaves the session untouched. Without a commit
    /// authority the session commits locally; with one, use [`Session::submit_keyed`].
    pub fn submit(
        &mut self,
        provider: &str,
        params: &[u8],
        expected_revision: u64,
        cancel: &CancellationToken,
    ) -> Result<Applied, Failed> {
        let result = self.submit_inner(None, provider, params, expected_revision, cancel);
        self.settle(result, |applied, report| applied.report = report)
    }

    /// As [`Session::submit`], idempotent under `key`: an identical retry (same principal,
    /// resource, action, provider, payload hash and expected revision) returns the stored
    /// receipt after a fresh grant check and applies nothing; the same key with anything else
    /// changed is `idempotency_conflict`. With a commit authority, an unknown outcome is kept
    /// pending and resolved by lookup, not by blind re-application (test
    /// `lost_acknowledgement_is_reconciled_and_applied_exactly_once`).
    pub fn submit_keyed(
        &mut self,
        key: &IdempotencyKey,
        provider: &str,
        params: &[u8],
        expected_revision: u64,
        cancel: &CancellationToken,
    ) -> Result<Applied, Failed> {
        let result = self.submit_inner(Some(key), provider, params, expected_revision, cancel);
        self.settle(result, |applied, report| applied.report = report)
    }

    fn check_grant(&self, action: Action) -> Result<GrantToken, SessionError> {
        match &self.ports.grants {
            None => Ok(GrantToken(0)),
            Some(port) => port
                .check(&self.config.actor, &self.resource, action)
                .map_err(|error| match error {
                    GrantError::Revoked => SessionError::GrantRevoked,
                    GrantError::Missing => SessionError::GrantMissing,
                }),
        }
    }

    fn binding(&self, action: Action, provider: &str, params: &[u8], expected: u64) -> String {
        let actor = &self.config.actor;
        sha256_hex_of(
            format!(
                "hsk-studio-session-binding-1\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                actor.account_id(),
                actor.principal_id(),
                self.resource.as_str(),
                action.as_str(),
                provider,
                sha256_hex_of(params),
                expected
            )
            .as_bytes(),
        )
    }

    fn admit_job(&self, bytes: u64) -> Result<Option<JobGuard>, SessionError> {
        let Some(port) = &self.ports.jobs else {
            return Ok(None);
        };
        let ticket = port
            .admit(
                &JobScope {
                    resource: &self.resource,
                    action: Action::Edit,
                },
                Cost { jobs: 1, bytes },
            )
            .map_err(|refused| SessionError::Overloaded {
                retry_after_ms: refused.retry_after_ms,
            })?;
        Ok(Some(JobGuard {
            port: Arc::clone(port),
            ticket: Some(ticket),
        }))
    }

    fn submit_inner(
        &mut self,
        key: Option<&IdempotencyKey>,
        provider_id: &str,
        params: &[u8],
        expected: u64,
        cancel: &CancellationToken,
    ) -> Result<Applied, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        if self.closed {
            return Err(SessionError::Closed);
        }
        if key.is_none() && self.ports.commits.is_some() {
            return Err(SessionError::KeyRequired);
        }
        // Fresh grant check on every submit and every retry, before any receipt is consulted.
        let grant = self.check_grant(Action::Edit)?;
        let binding = key.map(|_| self.binding(Action::Edit, provider_id, params, expected));
        if let (Some(key), Some(binding)) = (key, binding.as_deref())
            && let Some(entry) = self.receipts.get(key.as_str())
        {
            if entry.binding != binding {
                return Err(SessionError::IdempotencyConflict);
            }
            if matches!(entry.state, EntryState::Pending(_)) {
                match self.resolve_pending(key.as_str()) {
                    Resolution::Adopted | Resolution::Cleared => {}
                    Resolution::StillUnknown => return Err(SessionError::OutcomeUnknown),
                    Resolution::Diverged => return Err(SessionError::ReceiptMismatch),
                }
            }
            if let Some(entry) = self.receipts.get_mut(key.as_str())
                && let EntryState::Committed(applied) = &entry.state
            {
                // Handing the receipt back is the acknowledgement: it becomes evictable.
                entry.acknowledged = true;
                let mut replay = Applied::clone(applied);
                replay.replayed = true;
                replay.receipts_evicted = 0;
                return Ok(replay);
            }
            // Cleared: the authority never saw it; fall through to a fresh, safe attempt.
        }
        if self
            .receipts
            .values()
            .any(|entry| matches!(entry.state, EntryState::Pending(_)))
        {
            return Err(SessionError::ReconciliationRequired);
        }
        let current = self.snapshot.document().revision;
        if expected != current {
            return Err(SessionError::StaleRevision { expected, current });
        }
        if key.is_some() {
            // Room is established before any work starts; the eviction itself happens only when
            // the new receipt is stored, so a submit that fails evicts nothing.
            self.ensure_receipt_room()?;
        }
        // Admission is acquired after the grant and released on every exit (RAII); it is not
        // held while a pending outcome is resolved (that path ran above, before this point).
        let cost_bytes =
            (params.len() as u64).saturating_add(self.snapshot.encoded_bytes().len() as u64);
        let _permit = self.ports.admission.try_acquire(cost_bytes)?;
        let _job = self.admit_job(cost_bytes)?;

        let Proposed {
            applied,
            successor,
            slots,
        } = self.propose_checked(provider_id, params, current, cancel)?;
        let Some(successor) = successor else {
            // No-change: nothing to commit anywhere; the receipt is still stored under the key.
            let mut applied = applied;
            if let (Some(key), Some(binding)) = (key, binding) {
                let stored = EntryState::Committed(Box::new(applied.clone()));
                applied.receipts_evicted = self.insert_receipt(key, binding, stored, true);
            }
            return Ok(applied);
        };

        let mut applied = applied;
        if let (Some(key), Some(binding)) = (key, binding.as_deref()) {
            let outcome = self.ports.commits.as_ref().map(|port| {
                port.commit(&CommitRequest {
                    key,
                    resource: &self.resource,
                    grant,
                    epoch: self.config.epoch,
                    expected_revision: current,
                    successor_revision: applied.revision,
                    binding,
                    successor_document: successor.encoded_bytes(),
                    revision_updates: &applied.revision_updates,
                })
            });
            match outcome {
                None => {}
                Some(CommitOutcome::Committed(receipt)) => {
                    if receipt.revision != applied.revision || receipt.binding != binding {
                        // The authority answered with a receipt for something else: keep the
                        // transaction pending and diverged for an operator decision.
                        let disagreement = Disagreement {
                            binding: receipt.binding != binding,
                            revision: receipt.revision != applied.revision,
                            base: false,
                        };
                        let pending = PendingCommit {
                            base_revision: current,
                            successor,
                            slots,
                            draft: applied,
                            diverged: Some((receipt, disagreement)),
                        };
                        self.store_pending(key, binding, pending);
                        return Err(SessionError::ReceiptMismatch);
                    }
                    applied.commit = Some(receipt);
                }
                Some(CommitOutcome::Rejected(code)) => {
                    return Err(SessionError::CommitRejected(code));
                }
                Some(CommitOutcome::Stale { current_revision }) => {
                    return Err(SessionError::StaleRevision {
                        expected: current,
                        current: current_revision,
                    });
                }
                Some(CommitOutcome::StaleEpoch) => return Err(SessionError::StaleEpoch),
                Some(CommitOutcome::Unknown) => {
                    let pending = PendingCommit {
                        base_revision: current,
                        successor,
                        slots,
                        draft: applied,
                        diverged: None,
                    };
                    self.store_pending(key, binding, pending);
                    return Err(SessionError::OutcomeUnknown);
                }
            }
        }
        self.publish(successor, &slots);
        // A cancel that arrived while the commit was happening does not erase the edit (test
        // `admission_overload_cancel_after_commit_and_shutdown_drain`).
        applied.cancel_too_late = cancel.check().is_err();
        if let (Some(key), Some(binding)) = (key, binding) {
            let stored = EntryState::Committed(Box::new(applied.clone()));
            applied.receipts_evicted = self.insert_receipt(key, binding, stored, true);
        }
        Ok(applied)
    }

    // ----- the idempotency table ---------------------------------------------------------

    /// The oldest committed receipt the caller already holds.
    fn oldest_evictable(&self) -> Option<String> {
        self.receipts
            .iter()
            .filter(|(_, entry)| {
                entry.acknowledged && matches!(entry.state, EntryState::Committed(_))
            })
            .min_by_key(|(_, entry)| entry.seq)
            .map(|(key, _)| key.clone())
    }

    /// Whether a new key can be stored without exceeding `max_receipts`.
    fn ensure_receipt_room(&self) -> Result<(), SessionError> {
        let budgets = &self.config.budgets;
        if self.receipts.len() < budgets.max_receipts {
            return Ok(());
        }
        match budgets.receipt_policy {
            ReceiptPolicy::RefuseWhenFull => Err(SessionError::ReceiptsFull),
            ReceiptPolicy::EvictAcknowledged => self
                .oldest_evictable()
                .map(|_| ())
                .ok_or(SessionError::ReceiptsFull),
        }
    }

    /// Stores a receipt under `key`, evicting acknowledged receipts first when the table is full
    /// and the policy allows it. Returns how many were evicted. `ensure_receipt_room` ran before
    /// the work this receipt records, so a victim exists whenever one is needed here.
    fn insert_receipt(
        &mut self,
        key: &IdempotencyKey,
        binding: String,
        state: EntryState,
        acknowledged: bool,
    ) -> u32 {
        let mut evicted = 0u32;
        while self.receipts.len() >= self.config.budgets.max_receipts
            && self.config.budgets.receipt_policy == ReceiptPolicy::EvictAcknowledged
        {
            let Some(victim) = self.oldest_evictable() else {
                break;
            };
            self.receipts.remove(&victim);
            evicted = evicted.saturating_add(1);
            self.receipts_evicted = self.receipts_evicted.saturating_add(1);
        }
        self.receipt_seq = self.receipt_seq.saturating_add(1);
        self.receipts.insert(
            key.as_str().to_owned(),
            Entry {
                binding,
                state,
                seq: self.receipt_seq,
                acknowledged,
            },
        );
        evicted
    }

    /// Runs the provider and checks everything about its proposal without publishing anything.
    fn propose_checked(
        &self,
        provider_id: &str,
        params: &[u8],
        current: u64,
        cancel: &CancellationToken,
    ) -> Result<Proposed, SessionError> {
        let prepared = {
            let provider = self
                .ports
                .providers
                .get(provider_id)
                .ok_or(SessionError::ProviderUnavailable)?;
            provider
                .propose(&ProposalRequest {
                    snapshot: &self.snapshot,
                    revisions: &self.revisions,
                    params,
                    budget: self.config.budgets.chronicle,
                    resolver: &*self.ports.resolver,
                    cancel,
                })
                .map_err(|error| match error {
                    crate::ProviderError::Canceled => SessionError::Canceled,
                    crate::ProviderError::Rejected(code) => SessionError::Provider(code),
                })?
        };
        // Last point at which a cancel still prevents publication.
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let (observed, command_id, correlation_id, actor, changed) = match prepared.wire() {
            SourceResult::Changed {
                document_id,
                command_id,
                correlation_id,
                actor,
                observed_current_revision,
                ..
            } => (
                (document_id, *observed_current_revision),
                command_id.clone(),
                *correlation_id,
                actor.clone(),
                true,
            ),
            SourceResult::NoChange {
                document_id,
                command_id,
                correlation_id,
                actor,
                observed_current_revision,
            } => (
                (document_id, *observed_current_revision),
                command_id.clone(),
                *correlation_id,
                actor.clone(),
                false,
            ),
        };
        if observed.0 != &self.snapshot.document().document_id {
            return Err(SessionError::ProposalInconsistent);
        }
        if observed.1 != current {
            return Err(SessionError::StaleRevision {
                expected: observed.1,
                current,
            });
        }
        let mut revision = current;
        let mut successor_snapshot = None;
        let mut slots = Vec::new();
        if changed {
            let successor = prepared
                .successor()
                .ok_or(SessionError::ProposalInconsistent)?;
            revision = current
                .checked_add(1)
                .ok_or(SessionError::ProposalInconsistent)?;
            if successor.document().document_id != self.snapshot.document().document_id
                || successor.document().revision != revision
            {
                return Err(SessionError::ProposalInconsistent);
            }
            for update in prepared.revision_updates() {
                let slot = self
                    .revisions
                    .iter()
                    .position(|entry| entry.address == update.address)
                    .ok_or(SessionError::ProposalInconsistent)?;
                if self.revisions[slot].revision != update.previous_revision
                    || update.revision <= update.previous_revision
                    || slots.iter().any(|(taken, _)| *taken == slot)
                {
                    return Err(SessionError::ProposalInconsistent);
                }
                slots.push((slot, update.revision));
            }
            // Lineage: the successor must be the live document advanced by the declared writes.
            if !derives_from(
                self.snapshot.document(),
                successor.document(),
                prepared.revision_updates(),
            ) {
                return Err(SessionError::ProposalNotDerived);
            }
            successor_snapshot = Some(successor.clone());
        }
        Ok(Proposed {
            applied: Applied {
                provider: provider_id.to_owned(),
                command_id,
                correlation_id,
                actor,
                previous_revision: current,
                revision,
                changed,
                revision_updates: prepared.revision_updates().to_vec(),
                inverse: prepared.inverse().cloned(),
                commit: None,
                replayed: false,
                cancel_too_late: false,
                receipts_evicted: 0,
                report: ReportStatus::Unaddressed,
            },
            successor: successor_snapshot,
            slots,
        })
    }

    /// Everything was checked; this cannot fail.
    fn publish(&mut self, successor: Snapshot, slots: &[(usize, u64)]) {
        for (slot, revision) in slots {
            self.revisions[*slot].revision = *revision;
        }
        self.snapshot = successor;
    }

    fn store_pending(&mut self, key: &IdempotencyKey, binding: &str, pending: PendingCommit) {
        // A pending receipt is unacknowledged by definition and is not an eviction victim; room
        // for it was established by `ensure_receipt_room` before the commit was sent.
        let state = EntryState::Pending(Box::new(pending));
        self.insert_receipt(key, binding.to_owned(), state, false);
    }

    fn disagreement(
        &self,
        receipt: &CommitReceipt,
        binding: &str,
        pending: &PendingCommit,
    ) -> Disagreement {
        Disagreement {
            binding: receipt.binding != binding,
            revision: receipt.revision != pending.draft.revision,
            base: pending.base_revision != self.snapshot.document().revision,
        }
    }

    // ----- reconcile and drain -----------------------------------------------------------

    /// Asks the authority about one pending key and adopts, clears or keeps it.
    fn resolve_pending(&mut self, key: &str) -> Resolution {
        let (Some(port), Ok(parsed)) = (&self.ports.commits, IdempotencyKey::parse(key)) else {
            return Resolution::StillUnknown;
        };
        match port.lookup(&parsed) {
            Lookup::Unknown => Resolution::StillUnknown,
            Lookup::Absent => {
                self.receipts.remove(key);
                Resolution::Cleared
            }
            Lookup::Found(receipt) => {
                let Some(entry) = self.receipts.remove(key) else {
                    return Resolution::StillUnknown;
                };
                let Entry {
                    binding,
                    state,
                    seq,
                    acknowledged,
                } = entry;
                let EntryState::Pending(mut pending) = state else {
                    self.receipts.insert(
                        key.to_owned(),
                        Entry {
                            binding,
                            state,
                            seq,
                            acknowledged,
                        },
                    );
                    return Resolution::StillUnknown;
                };
                let disagreement = self.disagreement(&receipt, &binding, &pending);
                if disagreement.any() {
                    // Never resolved here: it stays pending and is listed by `diverged()`
                    // until an operator calls `discard_diverged` or `resync_diverged`.
                    pending.diverged = Some((receipt, disagreement));
                    self.receipts.insert(
                        key.to_owned(),
                        Entry {
                            binding,
                            state: EntryState::Pending(pending),
                            seq,
                            acknowledged,
                        },
                    );
                    return Resolution::Diverged;
                }
                let PendingCommit {
                    successor,
                    slots,
                    mut draft,
                    ..
                } = *pending;
                self.publish(successor, &slots);
                draft.commit = Some(receipt);
                // Adopted here, not handed to the caller: unacknowledged until a replay of the
                // key or `acknowledge_receipt` returns it, so eviction does not take it (test
                // `unacknowledged_and_pending_receipts_are_kept_and_a_full_table_refuses_typed`).
                self.receipts.insert(
                    key.to_owned(),
                    Entry {
                        binding,
                        state: EntryState::Committed(Box::new(draft)),
                        seq,
                        acknowledged: false,
                    },
                );
                Resolution::Adopted
            }
        }
    }

    fn pending_keys(&self) -> Vec<String> {
        self.receipts
            .iter()
            .filter(|(_, entry)| matches!(entry.state, EntryState::Pending(_)))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Resolves every pending commit by lookup. With `deadline`, entries not reached before the
    /// deadline are left pending (and counted as unknown).
    fn resolve_all(&mut self, deadline: Option<(u64, u64)>) -> Tally {
        let mut tally = Tally::default();
        for key in self.pending_keys() {
            if let Some((start, limit)) = deadline
                && self.ports.clock.now_ms().saturating_sub(start) >= limit
            {
                Tally::bump(&mut tally.unknown);
                continue;
            }
            match self.resolve_pending(&key) {
                Resolution::Adopted => Tally::bump(&mut tally.adopted),
                Resolution::Cleared => Tally::bump(&mut tally.cleared),
                Resolution::StillUnknown => Tally::bump(&mut tally.unknown),
                Resolution::Diverged => Tally::bump(&mut tally.diverged),
            }
        }
        tally
    }

    /// Looks up every pending commit: found -> adopted exactly once, absent -> cleared (the
    /// caller may retry it with the same key), unreachable -> still pending, a receipt that
    /// disagrees -> still pending and listed in `diverged_keys` for an operator decision.
    pub fn reconcile(&mut self, cancel: &CancellationToken) -> Result<ReconcileReport, Failed> {
        let result = cancel
            .check()
            .map_err(|_| SessionError::Canceled)
            .map(|()| {
                let tally = self.resolve_all(None);
                ReconcileReport {
                    adopted: tally.adopted,
                    cleared: tally.cleared,
                    still_unknown: tally.unknown,
                    diverged: tally.diverged,
                    diverged_keys: self.diverged(),
                    report: ReportStatus::Unaddressed,
                }
            });
        self.settle_with(
            result,
            |r| {
                if r.still_unknown > 0 || r.diverged > 0 {
                    Outcome::Failure(hsk_studio_observe::FailureCode::Unavailable)
                } else {
                    Outcome::Success
                }
            },
            |r, report| r.report = report,
        )
    }

    /// Stops accepting submits (`closed`); save, export, reconcile and drain stay available.
    pub fn close_admission(&mut self) {
        self.closed = true;
    }

    /// Shutdown: closes admission, resolves pending commits until `deadline_ms` on the injected
    /// clock, and reports whatever is still unresolved instead of dropping it (tests
    /// `admission_overload_cancel_after_commit_and_shutdown_drain`,
    /// `inconsistent_proposals_are_refused_and_a_zero_deadline_drain_reports_what_it_left`).
    pub fn drain(&mut self, deadline_ms: u64) -> Result<DrainReport, Failed> {
        self.closed = true;
        let start = self.ports.clock.now_ms();
        let tally = self.resolve_all(Some((start, deadline_ms)));
        let elapsed_ms = self.ports.clock.now_ms().saturating_sub(start);
        let result = Ok(DrainReport {
            adopted: tally.adopted,
            cleared: tally.cleared,
            abandoned: self.pending_keys(),
            elapsed_ms,
            deadline_met: elapsed_ms <= deadline_ms,
            report: ReportStatus::Unaddressed,
        });
        self.settle_with(
            result,
            |r| {
                if r.abandoned.is_empty() && r.deadline_met {
                    Outcome::Success
                } else {
                    Outcome::Failure(hsk_studio_observe::FailureCode::Unavailable)
                }
            },
            |r, report| r.report = report,
        )
    }

    // ----- receipts and the operator's decision on a diverged commit -----------------------

    /// Keys of pending commits whose authority receipt disagreed with the transaction the
    /// session sent. They stay pending, and block every later submit with
    /// `reconciliation_required`, until `discard_diverged` or `resync_diverged` resolves them.
    pub fn diverged(&self) -> Vec<String> {
        self.receipts
            .iter()
            .filter(
                |(_, entry)| matches!(&entry.state, EntryState::Pending(p) if p.diverged.is_some()),
            )
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// A snapshot of the idempotency table: size, bound, pending and unacknowledged receipts
    /// and the eviction count.
    pub fn receipt_stats(&self) -> ReceiptStats {
        let count =
            |keep: &dyn Fn(&Entry) -> bool| self.receipts.values().filter(|e| keep(e)).count();
        ReceiptStats {
            held: self.receipts.len(),
            capacity: self.config.budgets.max_receipts,
            pending: count(&|e| matches!(e.state, EntryState::Pending(_))),
            unacknowledged: count(&|e| {
                !e.acknowledged && matches!(e.state, EntryState::Committed(_))
            }),
            evicted: self.receipts_evicted,
        }
    }

    /// Hands the caller the committed receipt held under `key` (as a replay) and marks it
    /// acknowledged, which makes it evictable. A pending key answers `outcome_unknown` (or
    /// `receipt_mismatch` when diverged); a key that is not held answers `receipt_not_held`.
    /// Grants are checked afresh, like any replay.
    pub fn acknowledge_receipt(&mut self, key: &IdempotencyKey) -> Result<Applied, Failed> {
        let result = self.acknowledge_inner(key);
        self.settle(result, |applied, report| applied.report = report)
    }

    fn acknowledge_inner(&mut self, key: &IdempotencyKey) -> Result<Applied, SessionError> {
        self.check_grant(Action::Edit)?;
        let Some(entry) = self.receipts.get_mut(key.as_str()) else {
            return Err(SessionError::ReceiptNotHeld);
        };
        match &entry.state {
            EntryState::Pending(pending) if pending.diverged.is_some() => {
                Err(SessionError::ReceiptMismatch)
            }
            EntryState::Pending(_) => Err(SessionError::OutcomeUnknown),
            EntryState::Committed(applied) => {
                entry.acknowledged = true;
                let mut replay = Applied::clone(applied);
                replay.replayed = true;
                replay.receipts_evicted = 0;
                Ok(replay)
            }
        }
    }

    /// Operator decision: drop a diverged pending commit. The session's document is untouched
    /// (it may now be behind the authority; the next commit then answers `stale_revision`) and
    /// the key stops blocking submits. The command looks the authority up again first, so it
    /// acts on the receipt as it is now: `not_diverged` when the key is not held pending
    /// diverged, `outcome_unknown` when the authority cannot be asked; nothing changes then.
    pub fn discard_diverged(
        &mut self,
        key: &IdempotencyKey,
        cancel: &CancellationToken,
    ) -> Result<DivergenceReport, Failed> {
        let result = self.discard_inner(key, cancel);
        self.settle(result, |report, status| report.report = status)
    }

    fn discard_inner(
        &mut self,
        key: &IdempotencyKey,
        cancel: &CancellationToken,
    ) -> Result<DivergenceReport, SessionError> {
        let (authority, disagreement, draft_revision) = self.diverged_precondition(key, cancel)?;
        self.receipts.remove(key.as_str());
        let revision = self.snapshot.document().revision;
        Ok(DivergenceReport {
            key: key.as_str().to_owned(),
            resolution: Divergence::Discarded,
            revision_before: revision,
            revision_after: revision,
            draft_revision,
            authority,
            disagreement,
            report: ReportStatus::Unaddressed,
        })
    }

    /// Operator decision: replace the session's document with `authority_document`, the
    /// authority's own document at the revision its receipt names, and drop the diverged
    /// pending commit. The bytes are untrusted: they are bounded by the folio input budget,
    /// parsed by folio under the session budgets, and must name this session's document at the
    /// receipt's revision with every raster asset present in the source package; otherwise
    /// `authority_document_*` and nothing changes. The property revision vector restarts at 0,
    /// as after an open. Same lookup preconditions as [`Session::discard_diverged`].
    pub fn resync_diverged(
        &mut self,
        key: &IdempotencyKey,
        authority_document: &[u8],
        cancel: &CancellationToken,
    ) -> Result<DivergenceReport, Failed> {
        let result = self.resync_inner(key, authority_document, cancel);
        self.settle(result, |report, status| report.report = status)
    }

    fn resync_inner(
        &mut self,
        key: &IdempotencyKey,
        authority_document: &[u8],
        cancel: &CancellationToken,
    ) -> Result<DivergenceReport, SessionError> {
        let (authority, disagreement, draft_revision) = self.diverged_precondition(key, cancel)?;
        let snapshot =
            self.inspect_authority_document(authority_document, authority.revision, cancel)?;
        let revision_before = self.snapshot.document().revision;
        let revision_after = snapshot.document().revision;
        self.revisions = seed_revisions(snapshot.document());
        self.snapshot = snapshot;
        self.receipts.remove(key.as_str());
        Ok(DivergenceReport {
            key: key.as_str().to_owned(),
            resolution: Divergence::Resynced,
            revision_before,
            revision_after,
            draft_revision,
            authority,
            disagreement,
            report: ReportStatus::Unaddressed,
        })
    }

    /// Both operator commands act only on a pending key whose receipt, looked up now, still
    /// disagrees. Returns that receipt, how it disagrees and the revision the dropped commit
    /// would have produced.
    fn diverged_precondition(
        &self,
        key: &IdempotencyKey,
        cancel: &CancellationToken,
    ) -> Result<(CommitReceipt, Disagreement, u64), SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        self.check_grant(Action::Edit)?;
        let Some(entry) = self.receipts.get(key.as_str()) else {
            return Err(SessionError::ReceiptNotHeld);
        };
        let (EntryState::Pending(pending), Some(port)) = (&entry.state, &self.ports.commits) else {
            return Err(SessionError::NotDiverged);
        };
        let receipt = match port.lookup(key) {
            Lookup::Unknown => return Err(SessionError::OutcomeUnknown),
            Lookup::Absent => return Err(SessionError::NotDiverged),
            Lookup::Found(receipt) => receipt,
        };
        let disagreement = self.disagreement(&receipt, &entry.binding, pending);
        if !disagreement.any() {
            return Err(SessionError::NotDiverged);
        }
        Ok((receipt, disagreement, pending.draft.revision))
    }

    /// Parses the authority's document under the session budgets and checks it is this
    /// session's document at `receipt_revision` and writable from the source package.
    fn inspect_authority_document(
        &self,
        bytes: &[u8],
        receipt_revision: u64,
        cancel: &CancellationToken,
    ) -> Result<Snapshot, SessionError> {
        let fault = SessionError::AuthorityDocument;
        let budget = self.config.budgets.folio();
        if bytes.len() > budget.input_bytes {
            return Err(fault(AuthorityFault::TooLarge));
        }
        let inspection = folio::inspect_bytes(bytes, budget, cancel, &*self.ports.resolver)
            .map_err(|diagnostic| match diagnostic.code {
                folio::Code::Canceled => SessionError::Canceled,
                folio::Code::BudgetExceeded => fault(AuthorityFault::TooLarge),
                _ => fault(AuthorityFault::Invalid),
            })?;
        let snapshot = match inspection {
            folio::Inspection::Editable(snapshot) => snapshot,
            folio::Inspection::ReadOnly(_) => return Err(fault(AuthorityFault::ReadOnly)),
        };
        let document = snapshot.document();
        if document.document_id != self.snapshot.document().document_id {
            return Err(fault(AuthorityFault::WrongDocument));
        }
        if document.revision != receipt_revision {
            return Err(fault(AuthorityFault::WrongRevision));
        }
        let source = self.source.lock().unwrap_or_else(PoisonError::into_inner);
        require_tile_assets(document, source.manifest()).map_err(|error| match error {
            PackageError::InvalidDigest => fault(AuthorityFault::Invalid),
            _ => fault(AuthorityFault::MissingAsset),
        })?;
        drop(source);
        Ok(snapshot)
    }

    // ----- save and export ---------------------------------------------------------------

    /// Saves to the path the session was opened from.
    pub fn save(&mut self, cancel: &CancellationToken) -> Result<SaveReceipt, Failed> {
        match self.origin.clone() {
            Some(path) => self.save_to(&path, cancel),
            None => {
                let result = Err(SessionError::NoDestination);
                self.settle(result, |receipt: &mut SaveReceipt, report| {
                    receipt.report = report
                })
            }
        }
    }

    /// Atomically saves the current revision to `path` (last-good destination stays intact on any
    /// failure), then re-reads the destination's manifest to confirm the revision landed.
    pub fn save_to(
        &mut self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<SaveReceipt, Failed> {
        let result = self.save_inner(path, cancel);
        self.settle(result, |receipt, report| receipt.report = report)
    }

    /// The previews to write: only those that describe the revision being written. Host-supplied
    /// ones for this revision replace the source's; anything rendered for another revision is
    /// dropped and counted (second value).
    fn gather_previews(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(Vec<PreviewImage>, usize), SessionError> {
        let revision = self.snapshot.document().revision;
        let mut images: Vec<PreviewImage> = Vec::new();
        let mut dropped = 0usize;
        let mut replaced = false;
        if let Some((rendered_for, supplied)) = &self.supplied_previews {
            if *rendered_for == revision {
                images.extend(supplied.iter().cloned());
                replaced = true;
            } else {
                dropped = dropped.saturating_add(supplied.len());
            }
        }
        if replaced || self.preview_revision != revision {
            dropped = dropped.saturating_add(self.preview_refs.len());
        } else {
            let mut reader = self.source.lock().unwrap_or_else(PoisonError::into_inner);
            for declared in &self.preview_refs {
                match reader.read_preview(&declared.name, cancel) {
                    Ok(image) => images.push(image),
                    Err(PackageError::Canceled) => return Err(SessionError::Canceled),
                    Err(_) => dropped = dropped.saturating_add(1),
                }
            }
        }
        Ok((images, dropped))
    }

    /// The writer only knows "asset absent"; the source knows why.
    fn writer_cause(assets: &LazyAssets<'_>, error: PackageError) -> SessionError {
        let cause = if matches!(error, PackageError::Asset(_)) {
            assets.take_failure().unwrap_or(error)
        } else {
            error
        };
        SessionError::from_package(cause)
    }

    fn write_options(&self) -> WriteOptions {
        WriteOptions {
            asset_codec: self.asset_codec,
            ..WriteOptions::default()
        }
    }

    /// Serializes the current revision into memory (export only: a checkpoint is an in-memory
    /// package by contract). Asset bodies are pulled from the source package one at a time while
    /// the writer needs them.
    fn build_package(&self, cancel: &CancellationToken) -> Result<Built, SessionError> {
        let (images, dropped) = self.gather_previews(cancel)?;
        let assets = LazyAssets::new(&self.source, &self.raw, cancel);
        let mut bytes = Vec::new();
        let receipt = write_package_with(
            &self.snapshot,
            &assets,
            &Extras {
                quarantined: &self.quarantined,
                previews: &images,
            },
            &self.write_options(),
            &self.config.budgets.limits,
            cancel,
            &mut bytes,
        )
        .map_err(|error| Self::writer_cause(&assets, error))?;
        Ok(Built {
            bytes,
            receipt,
            previews_written: images.len(),
            previews_dropped: dropped,
        })
    }

    /// Supplies fresh preview images rendered by the host for `rendered_for_revision`, which must
    /// be the current document revision. They replace the previews in the source package the
    /// next time the session saves or exports at that revision; if the document is edited
    /// first they no longer describe it and are dropped (and counted) instead of written.
    pub fn set_previews(
        &mut self,
        rendered_for_revision: u64,
        images: Vec<PreviewImage>,
        cancel: &CancellationToken,
    ) -> Result<PreviewReceipt, Failed> {
        let result = self.set_previews_inner(rendered_for_revision, images, cancel);
        self.settle(result, |receipt, report| receipt.report = report)
    }

    fn set_previews_inner(
        &mut self,
        rendered_for: u64,
        images: Vec<PreviewImage>,
        cancel: &CancellationToken,
    ) -> Result<PreviewReceipt, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        self.check_grant(Action::Save)?;
        let current = self.snapshot.document().revision;
        if rendered_for != current {
            return Err(SessionError::StaleRevision {
                expected: rendered_for,
                current,
            });
        }
        let limits = self.config.budgets.limits;
        if images.len() > MAX_SUPPLIED_PREVIEWS {
            return Err(SessionError::Package(PackageError::TooManyEntries));
        }
        let mismatch = || SessionError::Package(PackageError::PreviewMismatch);
        let dimension_ok = |d: u32| (1..=limits.max_preview_dimension).contains(&d);
        let (mut seen, mut total) = (BTreeSet::new(), 0u64);
        for image in &images {
            cancel.check().map_err(|_| SessionError::Canceled)?;
            let info = &image.info;
            if !dimension_ok(info.width) || !dimension_ok(info.height) {
                return Err(mismatch());
            }
            let expected = info.byte_len();
            let color_ok = (1..=64).contains(&info.color.len())
                && info
                    .color
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
            if expected > limits.max_preview_bytes
                || image.pixels.len() as u64 != expected
                || !color_ok
                || !seen.insert(sha256_hex_of(&image.pixels))
            {
                return Err(mismatch());
            }
            // The running aggregate is checked as it grows, before anything is kept.
            total = total
                .checked_add(expected)
                .filter(|sum| *sum <= limits.max_entry_bytes)
                .ok_or(SessionError::Package(PackageError::TotalTooLarge))?;
        }
        let accepted = images.len();
        self.supplied_previews = Some((current, images));
        Ok(PreviewReceipt {
            revision: current,
            accepted,
            bytes: total,
            report: ReportStatus::Unaddressed,
        })
    }

    fn save_inner(
        &mut self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<SaveReceipt, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        self.check_grant(Action::Save)?;
        let limits = self.config.budgets.limits;
        let (images, previews_dropped) = self.gather_previews(cancel)?;
        // Streamed: raw assets flow from the source file into the temp file through one bounded
        // chunk (hash- and length-verified on the way), so peak memory does not grow with the
        // assets. The package re-verifies the finished temp from disk before the atomic replace.
        let assets = LazyAssets::new(&self.source, &self.raw, cancel);
        let written = save_atomic_streaming(
            path,
            &StreamSave {
                doc: &self.snapshot,
                assets: &assets,
                extras: Extras {
                    quarantined: &self.quarantined,
                    previews: &images,
                },
                options: self.write_options(),
            },
            &limits,
            cancel,
        )
        .map_err(|error| Self::writer_cause(&assets, error))?;
        let revision = self.snapshot.document().revision;
        let (landed, landed_raw) = File::open(path)
            .map_err(|e| SessionError::Io(e.kind()))
            .and_then(|file| {
                let (for_reader, raw) =
                    Handle::pair(Box::new(file), Arc::clone(&self.source_bytes));
                PackageReader::open(for_reader, &limits, cancel)
                    .map(|reader| (reader, raw))
                    .map_err(SessionError::from_package)
            })?;
        if landed.manifest().revision != revision
            || landed.manifest().document_id != self.snapshot.document().document_id
        {
            return Err(SessionError::VerifyMismatch);
        }
        // The file just written is the source from now on (and the old one is released); the
        // previews it holds describe exactly this revision.
        self.preview_refs = landed.previews();
        self.preview_revision = revision;
        self.supplied_previews = None;
        *self
            .source
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = landed;
        self.raw = landed_raw;
        self.saved_revision = Some(revision);
        self.origin = Some(path.to_path_buf());
        Ok(SaveReceipt {
            revision,
            entries: written.entries,
            bytes_written: written.bytes_written,
            quarantined_preserved: self.quarantined.len(),
            previews_preserved: images.len(),
            previews_dropped,
            report: ReportStatus::Unaddressed,
        })
    }

    /// Writes the current revision to memory and re-reads it with the package reader before
    /// returning it, so the receipt names the exact immutable revision it carries (test
    /// `export_checkpoint_is_verified_and_previews_follow_the_revision`).
    pub fn export_checkpoint(&mut self, cancel: &CancellationToken) -> Result<Checkpoint, Failed> {
        let result = self.export_inner(cancel);
        self.settle(result, |checkpoint, report| checkpoint.report = report)
    }

    fn export_inner(&self, cancel: &CancellationToken) -> Result<Checkpoint, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        self.check_grant(Action::Export)?;
        let built = self.build_package(cancel)?;
        let revision = self.snapshot.document().revision;
        let reread = read_package(
            &built.bytes,
            &self.config.budgets.limits,
            self.config.budgets.folio(),
            &*self.ports.resolver,
            cancel,
        )
        .map_err(SessionError::from_package)?;
        let same = reread
            .snapshot()
            .is_some_and(|s| s.encoded_bytes() == self.snapshot.encoded_bytes())
            && reread.manifest.revision == revision
            && reread.quarantined.len() == self.quarantined.len()
            && reread.previews.len() == built.previews_written;
        if !same {
            return Err(SessionError::VerifyMismatch);
        }
        Ok(Checkpoint {
            revision,
            sha256_hex: sha256_hex_of(&built.bytes),
            entries: built.receipt.entries,
            quarantined_preserved: self.quarantined.len(),
            previews_preserved: built.previews_written,
            previews_dropped: built.previews_dropped,
            bytes: built.bytes,
            report: ReportStatus::Unaddressed,
        })
    }

    // ----- bookkeeping -------------------------------------------------------------------

    /// Counts the operation, emits its single report and stores it as `last_report`.
    fn settle<T>(
        &mut self,
        result: Result<T, SessionError>,
        attach: impl FnOnce(&mut T, ReportStatus),
    ) -> Result<T, Failed> {
        self.settle_with(result, |_| Outcome::Success, attach)
    }

    fn settle_with<T>(
        &mut self,
        result: Result<T, SessionError>,
        ok_outcome: impl FnOnce(&T) -> Outcome,
        attach: impl FnOnce(&mut T, ReportStatus),
    ) -> Result<T, Failed> {
        self.operations = self.operations.saturating_add(1);
        let outcome = match &result {
            Ok(value) => ok_outcome(value),
            Err(error) => error.outcome(),
        };
        let report = emit_report(
            &mut *self.ports.sink,
            self.operations,
            self.snapshot.document().revision,
            Some(&self.resource),
            &self.config.actor,
            outcome,
        );
        self.last_report = report;
        match result {
            Ok(mut value) => {
                attach(&mut value, report);
                Ok(value)
            }
            Err(error) => Err(Failed { error, report }),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn document_revision(&self) -> u64 {
        self.snapshot.document().revision
    }

    /// Per-property revision vector (starts at 0 for every editable property at open).
    pub fn revisions(&self) -> &[RevisionEntry] {
        &self.revisions
    }

    pub fn actor(&self) -> &ActorContext {
        &self.config.actor
    }

    pub fn epoch(&self) -> u64 {
        self.config.epoch
    }

    pub fn origin(&self) -> Option<&Path> {
        self.origin.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_revision != Some(self.snapshot.document().revision)
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Entries the package kept verbatim or could not import (opaque / unlisted).
    pub fn loss(&self) -> &[LossNote] {
        &self.loss
    }

    /// Operations performed, counting the open.
    pub fn operations(&self) -> u64 {
        self.operations
    }

    pub fn last_report(&self) -> ReportStatus {
        self.last_report
    }

    /// Idempotency receipts held (committed and pending).
    pub fn receipt_count(&self) -> usize {
        self.receipts.len()
    }

    /// Keys whose commit outcome is still unknown.
    pub fn pending(&self) -> Vec<String> {
        self.pending_keys()
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.ports.providers
    }

    pub fn providers_mut(&mut self) -> &mut ProviderRegistry {
        &mut self.ports.providers
    }

    /// Total bytes read from the package source so far (open verification, on-demand asset and
    /// preview reads, post-save verification). An edit reads none.
    pub fn source_bytes_read(&self) -> u64 {
        self.source_bytes.load(Ordering::Relaxed)
    }

    /// Bytes the session itself keeps resident: the document, preserved unknown entries and
    /// host-supplied previews. Asset and source preview bodies are not part of it (test
    /// `lazy_source_holds_no_assets_and_save_streams_them_on_demand`).
    pub fn resident_bytes(&self) -> u64 {
        let quarantined: usize = self.quarantined.iter().map(|q| q.bytes.len()).sum();
        let supplied: usize = self
            .supplied_previews
            .iter()
            .flat_map(|(_, images)| images)
            .map(|image| image.pixels.len())
            .sum();
        (self.snapshot.encoded_bytes().len())
            .saturating_add(quarantined)
            .saturating_add(supplied) as u64
    }
}
