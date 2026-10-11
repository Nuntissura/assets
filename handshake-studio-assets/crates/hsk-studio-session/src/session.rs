//! Headless single-document session: open (package -> folio snapshot), submit a provider-proposed
//! chronicle transaction under revision checks, save (package atomic save). One session owns one
//! document, so edits are serialized by `&mut self` and there is exactly one commit authority.
use crate::{
    error::{Failed, ReportStatus, SessionError},
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
    AssetError, AssetSource, Codec, Extras, Limits, LossNote, Manifest, Opened, PackageReader,
    Quarantined, WriteOptions, save_atomic, write_package_with,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read, Seek},
    path::{Path, PathBuf},
};

/// Bounds for every operation: package container limits and chronicle (which carries folio)
/// budgets. Cancellation is the accord `CancellationToken` passed to each call.
#[derive(Clone, Copy, Debug, Default)]
pub struct Budgets {
    pub limits: Limits,
    pub chronicle: chronicle::Budget,
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
    }
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// The principal operating this session (attribution of open/save reports).
    pub actor: ActorContext,
    pub budgets: Budgets,
    /// Codec for asset blobs on save. `None` keeps what the opened package used.
    pub asset_codec: Option<Codec>,
}

impl SessionConfig {
    pub fn new(actor: ActorContext) -> Self {
        Self {
            actor,
            budgets: Budgets::default(),
            asset_codec: None,
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
}

impl SessionPorts {
    pub fn new(sink: Box<dyn SinkPort>) -> Self {
        Self {
            sink,
            resolver: Box::new(Unresolved),
            providers: ProviderRegistry::with_builtin(),
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
    pub report: ReportStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReceipt {
    pub revision: u64,
    pub entries: u32,
    pub bytes_written: u64,
    /// Unknown package entries re-emitted verbatim.
    pub quarantined_preserved: usize,
    /// Derived preview entries not rewritten (they describe the pre-edit document).
    pub previews_dropped: usize,
    pub report: ReportStatus,
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
    previews_dropped: usize,
    asset_codec: Codec,
    origin: Option<PathBuf>,
    saved_revision: Option<u64>,
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
        let reader = PackageReader::open(source, &config.budgets.limits, cancel)
            .map_err(|e| unaddressed(SessionError::from_package(e)))?;
        let manifest_revision = reader.manifest().revision;
        let resource = DomainId::parse(&reader.manifest().document_id).ok();
        let inherited = inherited_codec(reader.manifest());
        let correlation = 1;
        let opened = reader.into_opened(config.budgets.folio(), &*ports.resolver, cancel);
        let result = opened
            .map_err(SessionError::from_package)
            .and_then(Self::from_opened);
        match result {
            Ok(parts) => {
                let report = emit_report(
                    &mut *ports.sink,
                    correlation,
                    parts.snapshot.document().revision,
                    Some(&parts.resource),
                    &config.actor,
                    Outcome::Success,
                );
                let revision = parts.snapshot.document().revision;
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
                    previews_dropped: parts.previews,
                    origin,
                    saved_revision: Some(revision),
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
            previews,
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
            previews: previews.len(),
        })
    }

    /// Submits `params` to the registered provider `provider` against the document revision the
    /// caller last observed. Nothing is published unless the provider's transaction is computed
    /// on the session's current revision, advances it by exactly one and follows the session's
    /// property revision vector; any refusal leaves the session untouched.
    pub fn submit(
        &mut self,
        provider: &str,
        params: &[u8],
        expected_revision: u64,
        cancel: &CancellationToken,
    ) -> Result<Applied, Failed> {
        let result = self.submit_inner(provider, params, expected_revision, cancel);
        self.settle(result, |applied, report| applied.report = report)
    }

    fn submit_inner(
        &mut self,
        provider_id: &str,
        params: &[u8],
        expected: u64,
        cancel: &CancellationToken,
    ) -> Result<Applied, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let current = self.snapshot.document().revision;
        if expected != current {
            return Err(SessionError::StaleRevision { expected, current });
        }
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
            let mut slots = Vec::with_capacity(prepared.revision_updates().len());
            for update in prepared.revision_updates() {
                let slot = self
                    .revisions
                    .iter()
                    .position(|entry| entry.address == update.address)
                    .ok_or(SessionError::ProposalInconsistent)?;
                if self.revisions[slot].revision != update.previous_revision
                    || update.revision <= update.previous_revision
                    || slots.contains(&slot)
                {
                    return Err(SessionError::ProposalInconsistent);
                }
                slots.push(slot);
            }
            // Everything checked: publish. Nothing below can fail.
            for (slot, update) in slots.into_iter().zip(prepared.revision_updates()) {
                self.revisions[slot].revision = update.revision;
            }
            self.snapshot = successor.clone();
        }
        Ok(Applied {
            provider: provider_id.to_owned(),
            command_id,
            correlation_id,
            actor,
            previous_revision: current,
            revision,
            changed,
            revision_updates: prepared.revision_updates().to_vec(),
            inverse: prepared.inverse().cloned(),
            report: ReportStatus::Unaddressed,
        })
    }

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

    fn save_inner(
        &mut self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<SaveReceipt, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let limits = self.config.budgets.limits;
        let mut bytes = Vec::new();
        let written = write_package_with(
            &self.snapshot,
            &self.assets,
            &Extras {
                quarantined: &self.quarantined,
                previews: &[],
            },
            &WriteOptions {
                asset_codec: self.asset_codec,
                ..WriteOptions::default()
            },
            &limits,
            cancel,
            &mut bytes,
        )
        .map_err(SessionError::from_package)?;
        save_atomic(path, &bytes, &limits, cancel).map_err(SessionError::from_package)?;
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
            entries: written.entries,
            bytes_written: written.bytes_written,
            quarantined_preserved: self.quarantined.len(),
            previews_dropped: self.previews_dropped,
            report: ReportStatus::Unaddressed,
        })
    }

    /// Counts the operation, emits its single report and stores it as `last_report`.
    fn settle<T>(
        &mut self,
        result: Result<T, SessionError>,
        attach: impl FnOnce(&mut T, ReportStatus),
    ) -> Result<T, Failed> {
        self.operations += 1;
        let outcome = match &result {
            Ok(_) => Outcome::Success,
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

    pub fn origin(&self) -> Option<&Path> {
        self.origin.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_revision != Some(self.snapshot.document().revision)
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
    previews: usize,
}
