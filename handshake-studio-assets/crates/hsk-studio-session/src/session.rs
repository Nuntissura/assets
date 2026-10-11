//! Headless single-document session: open (package -> folio snapshot), submit a provider-proposed
//! chronicle transaction under revision checks (optionally keyed, granted, admitted and committed
//! through injected ports), save and export (package). One session owns one document, so edits
//! are serialized by `&mut self`; with a commit authority injected, the authority is canonical
//! and the session adopts a transaction locally only after the authority's receipt.
use crate::{
    admission::{Admission, JobGuard},
    error::{Failed, ReportStatus, SessionError},
    ports::{
        Action, Clock, CommitOutcome, CommitPort, CommitReceipt, CommitRequest, Cost, GrantError,
        GrantPort, GrantToken, IdempotencyKey, JobPort, JobScope, Lookup, MonotonicClock,
    },
    registry::{ProposalRequest, ProviderRegistry},
};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_chronicle::{
    self as chronicle, Inverse, RevisionEntry, RevisionUpdate, SourceResult,
};
use hsk_studio_folio::{
    self as folio, ContentDigest, NodeKind, NodeRef, Payload, Resolver, Snapshot, StudioDocument,
    Unresolved,
};
use hsk_studio_observe::{Budget as ObserveBudget, Observation, Observe, Outcome, SinkPort};
use hsk_studio_package::{
    AssetError, AssetSource, Codec, Extras, Limits, LossNote, Manifest, Opened, PackageError,
    PackageReader, PreviewImage, Quarantined, WriteOptions, WriteReceipt, read_package,
    save_atomic, sha256_hex_of, write_package_with,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read, Seek},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Bounds for every operation: package container limits, chronicle (which carries folio) budgets
/// and the idempotency table. Cancellation is the accord `CancellationToken` passed to each call.
#[derive(Clone, Copy, Debug)]
pub struct Budgets {
    pub limits: Limits,
    pub chronicle: chronicle::Budget,
    /// Idempotency receipts (committed and pending) a session keeps; further new keys are
    /// refused with `receipts_full` rather than evicting a receipt exactly-once depends on.
    pub max_receipts: usize,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            chronicle: chronicle::Budget::default(),
            max_receipts: 1024,
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

/// Asset bytes verified at open, granted again at save (the package never owns a second CAS).
struct HeldAssets(BTreeMap<String, Vec<u8>>);

impl AssetSource for HeldAssets {
    fn read(&self, digest: &ContentDigest, max: u64) -> Result<Vec<u8>, AssetError> {
        if digest.algorithm != "sha256" {
            return Err(AssetError::Absent);
        }
        match self.0.get(&digest.digest) {
            None => Err(AssetError::Absent),
            Some(bytes) if bytes.len() as u64 > max => Err(AssetError::TooLarge),
            Some(bytes) => Ok(bytes.clone()),
        }
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
    pub report: ReportStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReceipt {
    pub revision: u64,
    pub entries: u32,
    pub bytes_written: u64,
    /// Unknown package entries re-emitted verbatim.
    pub quarantined_preserved: usize,
    /// Preview entries rewritten (only while the document is still the revision they describe).
    pub previews_preserved: usize,
    /// Preview entries not rewritten: the document was edited since open, or the preview could
    /// not be read within bounds.
    pub previews_dropped: usize,
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
    /// Authority receipt disagrees with the pending transaction: still pending, needs a human.
    pub diverged: u32,
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
}

struct PendingCommit {
    base_revision: u64,
    successor: Snapshot,
    slots: Vec<(usize, u64)>,
    draft: Applied,
}

enum Resolution {
    Adopted,
    Cleared,
    StillUnknown,
    Diverged,
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
    assets: HeldAssets,
    quarantined: Vec<Quarantined>,
    loss: Vec<LossNote>,
    previews: Vec<PreviewImage>,
    previews_skipped: usize,
    opened_revision: u64,
    asset_codec: Codec,
    origin: Option<PathBuf>,
    saved_revision: Option<u64>,
    receipts: BTreeMap<String, Entry>,
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

fn seed_revisions(document: &StudioDocument) -> Vec<RevisionEntry> {
    let entry = |node_kind: NodeKind, node_id: &str, property: chronicle::Property| RevisionEntry {
        address: chronicle::PropertyAddress {
            target: NodeRef {
                node_kind,
                node_id: node_id.to_owned(),
            },
            property,
        },
        revision: 0,
    };
    let mut vector = Vec::new();
    for layer in &document.layers {
        if !matches!(layer.payload, Payload::Unsupported { .. }) {
            vector.push(entry(
                NodeKind::Layer,
                &layer.layer_id,
                chronicle::Property::Name,
            ));
            vector.push(entry(
                NodeKind::Layer,
                &layer.layer_id,
                chronicle::Property::Visible,
            ));
        }
    }
    for artboard in &document.artboards {
        vector.push(entry(
            NodeKind::Artboard,
            &artboard.artboard_id,
            chronicle::Property::Name,
        ));
    }
    for spread in &document.page_spreads {
        vector.push(entry(
            NodeKind::PageSpread,
            &spread.page_spread_id,
            chronicle::Property::Name,
        ));
    }
    vector
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

/// Reads every declared preview within bounds. A preview that cannot be read is skipped (and
/// counted), never a reason to refuse the document; only cancellation aborts.
fn load_previews<R: Read + Seek>(
    reader: &mut PackageReader<R>,
    budget: u64,
    cancel: &CancellationToken,
) -> Result<(Vec<PreviewImage>, usize), SessionError> {
    let mut loaded = Vec::new();
    let (mut skipped, mut total) = (0usize, 0u64);
    for declared in reader.previews() {
        let size = declared.info.byte_len();
        if total.saturating_add(size) > budget {
            skipped += 1;
            continue;
        }
        match reader.read_preview(&declared.name, cancel) {
            Ok(image) => {
                total += size;
                loaded.push(image);
            }
            Err(PackageError::Canceled) => return Err(SessionError::Canceled),
            Err(_) => skipped += 1,
        }
    }
    Ok((loaded, skipped))
}

impl Session {
    /// Opens a package file. The file is read as a bounded stream under `config.budgets`.
    pub fn open_path(
        path: &Path,
        config: SessionConfig,
        ports: SessionPorts,
        cancel: &CancellationToken,
    ) -> Result<Session, Failed> {
        match File::open(path) {
            Ok(file) => Self::open_stream(file, Some(path.to_path_buf()), config, ports, cancel),
            Err(error) => Err(Failed {
                error: SessionError::Io(error.kind()),
                report: ReportStatus::Unaddressed,
            }),
        }
    }

    /// Opens a package held in memory (no origin path, so `save_to` is the only way to save).
    pub fn open_bytes(
        bytes: &[u8],
        config: SessionConfig,
        ports: SessionPorts,
        cancel: &CancellationToken,
    ) -> Result<Session, Failed> {
        Self::open_stream(Cursor::new(bytes), None, config, ports, cancel)
    }

    fn open_stream<R: Read + Seek>(
        source: R,
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
        let mut reader = PackageReader::open(source, &config.budgets.limits, cancel)
            .map_err(|e| unaddressed(SessionError::from_package(e)))?;
        let manifest_revision = reader.manifest().revision;
        let resource = DomainId::parse(&reader.manifest().document_id).ok();
        let inherited = inherited_codec(reader.manifest());
        let correlation = 1;
        let result = load_previews(&mut reader, config.budgets.limits.max_entry_bytes, cancel)
            .and_then(|(previews, skipped)| {
                reader
                    .into_opened(config.budgets.folio(), &*ports.resolver, cancel)
                    .map_err(SessionError::from_package)
                    .and_then(Self::from_opened)
                    .map(|mut parts| {
                        parts.previews = previews;
                        parts.previews_skipped = skipped;
                        parts
                    })
            });
        match result {
            Ok(parts) => {
                let revision = parts.snapshot.document().revision;
                let report = emit_report(
                    &mut *ports.sink,
                    correlation,
                    revision,
                    Some(&parts.resource),
                    &config.actor,
                    Outcome::Success,
                );
                Ok(Session {
                    asset_codec: config.asset_codec.unwrap_or(inherited),
                    config,
                    ports,
                    resource: parts.resource,
                    snapshot: parts.snapshot,
                    revisions: parts.revisions,
                    assets: parts.assets,
                    quarantined: parts.quarantined,
                    loss: parts.loss,
                    previews: parts.previews,
                    previews_skipped: parts.previews_skipped,
                    opened_revision: revision,
                    origin,
                    saved_revision: Some(revision),
                    receipts: BTreeMap::new(),
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

    fn from_opened(opened: Opened) -> Result<OpenedParts, SessionError> {
        let Opened {
            inspection,
            assets,
            quarantined,
            loss,
            ..
        } = opened;
        let snapshot = match inspection {
            folio::Inspection::Editable(snapshot) => snapshot,
            folio::Inspection::ReadOnly(_) => return Err(SessionError::ReadOnlyDocument),
        };
        let resource = DomainId::parse(&snapshot.document().document_id)
            .map_err(|_| SessionError::ReadOnlyDocument)?;
        Ok(OpenedParts {
            revisions: seed_revisions(snapshot.document()),
            resource,
            snapshot,
            assets: HeldAssets(assets),
            quarantined,
            loss,
            previews: Vec::new(),
            previews_skipped: 0,
        })
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
    /// pending and resolved by lookup, never by blind re-application.
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
            if let Some(Entry {
                state: EntryState::Committed(applied),
                ..
            }) = self.receipts.get(key.as_str())
            {
                let mut replay = Applied::clone(applied);
                replay.replayed = true;
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
        if key.is_some() && self.receipts.len() >= self.config.budgets.max_receipts {
            return Err(SessionError::ReceiptsFull);
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
            if let (Some(key), Some(binding)) = (key, binding) {
                self.receipts.insert(
                    key.as_str().to_owned(),
                    Entry {
                        binding,
                        state: EntryState::Committed(Box::new(applied.clone())),
                    },
                );
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
                        self.store_pending(key, binding, current, successor, slots, applied);
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
                    self.store_pending(key, binding, current, successor, slots, applied);
                    return Err(SessionError::OutcomeUnknown);
                }
            }
        }
        self.publish(successor, &slots);
        // A cancel that arrived while the commit was happening cannot erase the edit.
        applied.cancel_too_late = cancel.check().is_err();
        if let (Some(key), Some(binding)) = (key, binding) {
            self.receipts.insert(
                key.as_str().to_owned(),
                Entry {
                    binding,
                    state: EntryState::Committed(Box::new(applied.clone())),
                },
            );
        }
        Ok(applied)
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

    fn store_pending(
        &mut self,
        key: &IdempotencyKey,
        binding: &str,
        base_revision: u64,
        successor: Snapshot,
        slots: Vec<(usize, u64)>,
        draft: Applied,
    ) {
        self.receipts.insert(
            key.as_str().to_owned(),
            Entry {
                binding: binding.to_owned(),
                state: EntryState::Pending(Box::new(PendingCommit {
                    base_revision,
                    successor,
                    slots,
                    draft,
                })),
            },
        );
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
                let Entry { binding, state } = entry;
                let EntryState::Pending(pending) = state else {
                    self.receipts
                        .insert(key.to_owned(), Entry { binding, state });
                    return Resolution::StillUnknown;
                };
                let consistent = receipt.binding == binding
                    && receipt.revision == pending.draft.revision
                    && pending.base_revision == self.snapshot.document().revision;
                if !consistent {
                    self.receipts.insert(
                        key.to_owned(),
                        Entry {
                            binding,
                            state: EntryState::Pending(pending),
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
                self.receipts.insert(
                    key.to_owned(),
                    Entry {
                        binding,
                        state: EntryState::Committed(Box::new(draft)),
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
    fn resolve_all(&mut self, deadline: Option<(u64, u64)>) -> [u32; 4] {
        let mut tally = [0u32; 4];
        for key in self.pending_keys() {
            if let Some((start, limit)) = deadline
                && self.ports.clock.now_ms().saturating_sub(start) >= limit
            {
                tally[2] += 1;
                continue;
            }
            match self.resolve_pending(&key) {
                Resolution::Adopted => tally[0] += 1,
                Resolution::Cleared => tally[1] += 1,
                Resolution::StillUnknown => tally[2] += 1,
                Resolution::Diverged => tally[3] += 1,
            }
        }
        tally
    }

    /// Looks up every pending commit: found -> adopted exactly once, absent -> cleared (the
    /// caller may retry it with the same key), unreachable -> still pending.
    pub fn reconcile(&mut self, cancel: &CancellationToken) -> Result<ReconcileReport, Failed> {
        let result = cancel
            .check()
            .map_err(|_| SessionError::Canceled)
            .map(|()| {
                let [adopted, cleared, still_unknown, diverged] = self.resolve_all(None);
                ReconcileReport {
                    adopted,
                    cleared,
                    still_unknown,
                    diverged,
                    report: ReportStatus::Unaddressed,
                }
            });
        self.settle_with(
            result,
            |r| {
                if r.still_unknown + r.diverged > 0 {
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
    /// clock, and reports (never drops) whatever is still unresolved.
    pub fn drain(&mut self, deadline_ms: u64) -> Result<DrainReport, Failed> {
        self.closed = true;
        let start = self.ports.clock.now_ms();
        let [adopted, cleared, _, _] = self.resolve_all(Some((start, deadline_ms)));
        let elapsed_ms = self.ports.clock.now_ms().saturating_sub(start);
        let result = Ok(DrainReport {
            adopted,
            cleared,
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

    /// Serializes the current revision. Previews are rewritten only while the document is still
    /// the revision they were made for; after an edit they would describe the wrong content.
    fn build_package(&self, cancel: &CancellationToken) -> Result<Built, SessionError> {
        let fresh = self.snapshot.document().revision == self.opened_revision;
        let (previews, dropped): (&[PreviewImage], usize) = if fresh {
            (&self.previews, self.previews_skipped)
        } else {
            (&[], self.previews.len() + self.previews_skipped)
        };
        let mut bytes = Vec::new();
        let receipt = write_package_with(
            &self.snapshot,
            &self.assets,
            &Extras {
                quarantined: &self.quarantined,
                previews,
            },
            &WriteOptions {
                asset_codec: self.asset_codec,
                ..WriteOptions::default()
            },
            &self.config.budgets.limits,
            cancel,
            &mut bytes,
        )
        .map_err(SessionError::from_package)?;
        Ok(Built {
            bytes,
            receipt,
            previews_written: previews.len(),
            previews_dropped: dropped,
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
        let built = self.build_package(cancel)?;
        save_atomic(path, &built.bytes, &limits, cancel).map_err(SessionError::from_package)?;
        let revision = self.snapshot.document().revision;
        let landed = File::open(path)
            .map_err(|e| SessionError::Io(e.kind()))
            .and_then(|file| {
                PackageReader::open(file, &limits, cancel).map_err(SessionError::from_package)
            })?;
        if landed.manifest().revision != revision
            || landed.manifest().document_id != self.snapshot.document().document_id
        {
            return Err(SessionError::VerifyMismatch);
        }
        self.saved_revision = Some(revision);
        self.origin = Some(path.to_path_buf());
        Ok(SaveReceipt {
            revision,
            entries: built.receipt.entries,
            bytes_written: built.receipt.bytes_written,
            quarantined_preserved: self.quarantined.len(),
            previews_preserved: built.previews_written,
            previews_dropped: built.previews_dropped,
            report: ReportStatus::Unaddressed,
        })
    }

    /// Writes the current revision to memory and re-reads it with the package reader before
    /// returning it, so the receipt proves the exact immutable revision it carries.
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
        self.operations += 1;
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
}

struct OpenedParts {
    resource: DomainId,
    snapshot: Snapshot,
    revisions: Vec<RevisionEntry>,
    assets: HeldAssets,
    quarantined: Vec<Quarantined>,
    loss: Vec<LossNote>,
    previews: Vec<PreviewImage>,
    previews_skipped: usize,
}
