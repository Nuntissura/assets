//! Direct acquisition storage: confined handles, durable journal, and real redb archive.

use crate::ArchiveStore;
use fforager_contracts::{
    ARCHIVE_SCHEMA, ArchiveClaimOutcome, ArchiveClaimRequest, ArchiveCommitOutcome,
    ArchiveCommitRequest, ArchiveConfinementProof, ArchiveFilesystemCommitment, ArchiveIdentity,
    ArchiveKey, ArchiveLease, ArchiveLimits, ArchiveMembership, ArchiveNamespace,
    ArchivePlacementProof, ArchiveProvenance, ArchiveSuccessEvent, ArchiveSynchronizationProof,
    AssetId, DerivedOutputId, FilesystemProfileContract, JobId, LeaseToken,
    ReconciledArchiveOutput, TransactionId,
};
use fforager_core::engine::{
    FetchControl, FetchError, FetchRequest, FetchResult, FetchStorage, TransferReceipt,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNAL_LIMIT: usize = 1024 * 1024;
const JOURNAL_RECORD_LIMIT: u64 = 256;
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn storage(error: impl std::fmt::Display) -> FetchError {
    FetchError::Storage(error.to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::{DurableFetchStorage, digest};
    use crate::ArchiveStore;
    use fforager_contracts::{
        ARCHIVE_SCHEMA, ArchiveIdentity, ArchiveKey, ArchiveLimits, ArchiveMembership,
        ArchiveNamespace, AssetId,
    };
    use fforager_core::engine::{
        FetchControl, FetchEngine, FetchError, FetchRequest, FetchTransport, TransferReceipt,
    };
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Debug)]
    struct AbortingTransport(Arc<AtomicUsize>);

    impl FetchTransport for AbortingTransport {
        fn download(
            &mut self,
            _: &FetchRequest,
            _: &mut dyn Write,
            _: &FetchControl<'_>,
        ) -> Result<TransferReceipt, FetchError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(FetchError::Transport(
                "intentional fixture abort; no network".into(),
            ))
        }
    }

    #[test]
    fn hardlinked_stage_is_rejected_before_truncation() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = repository
            .join(".fforager-artifacts/test-runs")
            .join(format!("hardlinked-stage-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let request = FetchRequest {
            url: "https://example.invalid/hardlink-regression.wav".into(),
            output: root.join("media.wav"),
            state_dir: root.join("state"),
            max_bytes: 4096,
            timeout_ms: 30_000,
            cancel_file: None,
            allowed_loopback: None,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let mut engine =
            FetchEngine::new(AbortingTransport(calls.clone()), DurableFetchStorage::new());
        let cancel = AtomicBool::new(false);
        assert!(matches!(
            engine.fetch(&request, &cancel),
            Err(FetchError::Transport(_))
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let stages = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".fforager-"))
                    && path.extension() == Some(std::ffi::OsStr::new("part"))
            })
            .collect::<Vec<_>>();
        let [stage] = stages.as_slice() else {
            panic!("expected one actual aborted stage");
        };
        let sentinel = root.join("sentinel.bin");
        let original = b"Existing hardlinked content must survive rejected preparation.\n";
        fs::write(&sentinel, original).unwrap();
        fs::remove_file(stage).unwrap();
        fs::hard_link(&sentinel, stage).unwrap();
        let rejected = engine
            .fetch(&request, &cancel)
            .expect_err("hardlinked stage rejected");
        let observed = fs::read(&sentinel).unwrap();
        assert_eq!(
            observed, original,
            "preexisting sentinel was truncated or overwritten"
        );
        assert_eq!(digest(&observed), digest(original));
        assert!(
            matches!(rejected, FetchError::Storage(ref detail) if detail.contains("FF-STORAGE-E-HANDLE-CONFINEMENT"))
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "rejected stage reached transport"
        );
        assert!(!request.output.exists());
        let key = ArchiveKey {
            schema: ARCHIVE_SCHEMA,
            namespace: ArchiveNamespace::SourceAsset,
            identity: ArchiveIdentity::Asset(
                AssetId::new(format!("asset_{}", digest(request.url.as_bytes()))).unwrap(),
            ),
            identity_rule_version: 1,
            extractor_id: "generic-http".into(),
        };
        let archive = ArchiveStore::open(
            request.state_dir.join("archive.redb"),
            ArchiveLimits::default(),
        )
        .unwrap();
        assert_eq!(archive.membership(&key).unwrap(), ArchiveMembership::Absent);
    }
}

fn digest(bytes: &[u8]) -> String {
    lower_hex(&Sha256::digest(bytes))
}

fn lower_hex(bytes: &sha2::digest::Output<Sha256>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

fn now() -> Result<u64, FetchError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(storage)?
            .as_millis(),
    )
    .map_err(storage)
}

/// Production storage adapter. Unsupported filesystem capabilities fail before transfer.
#[derive(Debug, Default)]
pub struct DurableFetchStorage;

impl DurableFetchStorage {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn session(request: &FetchRequest, control: &FetchControl<'_>) -> Result<Session, FetchError> {
        request.validate()?;
        control.check()?;
        let output = std::path::absolute(&request.output).map_err(storage)?;
        let output_parent = output
            .parent()
            .ok_or_else(|| storage("missing output parent"))?;
        let name = output
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| storage("output name must be Unicode"))?
            .to_owned();
        platform::validate_name(&name)?;
        let output_root = platform::Root::open(output_parent, false)?;
        let state_root = platform::Root::open(&request.state_dir, true)?;
        // Probe real capabilities, including actual flush acknowledgements. Neither
        // lexical normalization nor the frozen data-only profile establishes these.
        let profile = output_root.profile()?;
        state_root.profile()?;
        output_root.sync()?;
        state_root.sync()?;
        control.check()?;
        let source_digest = digest(request.url.as_bytes());
        let output_identity = output
            .to_str()
            .ok_or_else(|| storage("output path must be Unicode"))?
            .to_owned();
        let key_digest = digest(format!("{source_digest}\n{output_identity}").as_bytes());
        let job = format!("job_{key_digest}");
        let asset = AssetId::new(format!("asset_{source_digest}")).map_err(storage)?;
        let key = ArchiveKey {
            schema: ARCHIVE_SCHEMA,
            namespace: ArchiveNamespace::SourceAsset,
            identity: ArchiveIdentity::Asset(asset.clone()),
            identity_rule_version: 1,
            extractor_id: "generic-http".to_owned(),
        };
        let lock = state_root.open_file(&format!("{key_digest}.lock"), false, false)?;
        // Windows sharing is exclusive for this retained handle; a process crash
        // releases ownership without requiring unsafe stale-lock deletion.
        let archive_file = state_root.open_database("archive.redb")?;
        let database = redb::Builder::new()
            .create_file(archive_file)
            .map_err(storage)?;
        let archive = ArchiveStore {
            database,
            limits: ArchiveLimits::default(),
        };
        archive.initialize_tables().map_err(storage)?;
        let journal_file = state_root.open_file(&format!("{key_digest}.journal"), false, false)?;
        let journal = Journal::open(journal_file, &key_digest)?;
        state_root.sync()?;
        Ok(Session {
            output_root,
            _state_root: state_root,
            _lock: lock,
            archive,
            journal,
            output,
            output_name: name,
            stage_name: format!(".fforager-{key_digest}.part"),
            key,
            asset,
            job,
            source_digest,
            key_digest,
            profile,
        })
    }
}

/// Owned staged writer. Its byte bound is enforced independently of the transport.
#[derive(Debug)]
pub struct DurableFetchWriter {
    session: Session,
    file: File,
    bytes: u64,
    maximum: u64,
    hash: Sha256,
    lease: ArchiveLease,
    provenance: ArchiveProvenance,
}

impl Write for DurableFetchWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = u64::try_from(bytes.len()).map_err(io::Error::other)?;
        if count > self.maximum.saturating_sub(self.bytes) {
            return Err(io::Error::other("FF-STORAGE-E-BYTE-LIMIT"));
        }
        let written = self.file.write(bytes)?;
        self.hash.update(&bytes[..written]);
        self.bytes += u64::try_from(written).map_err(io::Error::other)?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl FetchStorage for DurableFetchStorage {
    type Writer = DurableFetchWriter;

    fn source_identity(&self, writer: &Self::Writer) -> Result<String, FetchError> {
        Ok(writer.session.source_digest.clone())
    }

    fn recover(
        &mut self,
        request: &FetchRequest,
        control: &FetchControl<'_>,
    ) -> Result<Option<FetchResult>, FetchError> {
        let mut session = Self::session(request, control)?;
        session.recover(request.max_bytes, control)
    }

    fn prepare(
        &mut self,
        request: &FetchRequest,
        control: &FetchControl<'_>,
    ) -> Result<Self::Writer, FetchError> {
        let mut session = Self::session(request, control)?;
        control.check()?;
        if session.output_root.exists(&session.output_name)? {
            return Err(storage(
                "FF-STORAGE-E-OUTPUT-EXISTS: recovery required; refusing overwrite",
            ));
        }
        // A prefix without a validated record is deliberately restarted, never
        // treated as a resume offset or acknowledged durable media.
        let file = session
            .output_root
            .open_file(&session.stage_name, false, true)?;
        session.output_root.verify(&file, &session.stage_name)?;
        let provenance = ArchiveProvenance {
            job_id: JobId::new(session.job.clone()).map_err(storage)?,
            transaction_id: TransactionId::new(format!("transaction_{}", session.key_digest))
                .map_err(storage)?,
            source_locator_digest: session.source_digest.clone(),
            request_provenance_digest: digest(
                format!("{}\n{}", session.key_digest, request.max_bytes).as_bytes(),
            ),
        };
        let lease = session.acquire(&provenance)?;
        if let Err(primary) = session.journal.append(Phase::Prepared, None) {
            return match session.archive.release_claim(&lease) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(storage(format!(
                    "primary={primary}; claim cleanup={cleanup}"
                ))),
            };
        }
        Ok(DurableFetchWriter {
            session,
            file,
            bytes: 0,
            maximum: request.max_bytes,
            hash: Sha256::new(),
            lease,
            provenance,
        })
    }

    fn commit(
        &mut self,
        request: &FetchRequest,
        mut writer: Self::Writer,
        receipt: TransferReceipt,
        control: &FetchControl<'_>,
    ) -> Result<FetchResult, FetchError> {
        let result = (|| {
            control.check()?;
            if receipt.bytes != writer.bytes || writer.bytes > request.max_bytes {
                return Err(storage(
                    "FF-STORAGE-E-LENGTH: transfer and staged bytes disagree",
                ));
            }
            writer.file.flush().map_err(storage)?;
            writer.file.sync_all().map_err(storage)?;
            writer
                .session
                .output_root
                .verify(&writer.file, &writer.session.stage_name)?;
            let expected = lower_hex(&writer.hash.clone().finalize());
            let (observed_bytes, observed_digest) =
                hash_file(&mut writer.file, request.max_bytes, control)?;
            if observed_bytes != receipt.bytes || observed_digest != expected {
                return Err(storage(
                    "FF-STORAGE-E-HASH: staged file reread differs from transfer",
                ));
            }
            let mut commit = writer.session.commit_request(
                writer.lease.clone(),
                writer.provenance.clone(),
                observed_bytes,
                observed_digest,
            )?;
            control.check()?;
            writer
                .session
                .journal
                .append(Phase::Validated, Some(commit.clone()))?;
            writer.session.output_root.sync()?;
            control.check()?;
            writer
                .session
                .output_root
                .publish(&writer.file, &writer.session.output_name)?;
            writer
                .session
                .output_root
                .verify(&writer.file, &writer.session.output_name)?;
            writer.file.sync_all().map_err(storage)?;
            writer.session.output_root.sync()?;
            writer
                .session
                .journal
                .append(Phase::Placed, Some(commit.clone()))?;
            commit.committed_at_unix_millis = now()?;
            writer.session.finish(commit, false)
        })();
        if let Err(primary) = result {
            if writer
                .session
                .journal
                .last
                .as_ref()
                .is_some_and(|record| matches!(record.phase, Phase::Prepared | Phase::Aborted))
            {
                return match self.abort(writer) {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(storage(format!("primary={primary}; cleanup={cleanup}"))),
                };
            }
            return Err(primary);
        }
        result
    }

    fn abort(&mut self, mut writer: Self::Writer) -> Result<(), FetchError> {
        let mut failures = Vec::new();
        if let Err(error) = writer.file.flush() {
            failures.push(format!("flush={error}"));
        }
        if let Err(error) = writer.file.sync_all() {
            failures.push(format!("sync={error}"));
        }
        if let Err(error) = writer.session.journal.append(Phase::Aborted, None) {
            failures.push(format!("journal={error}"));
        }
        if let Err(error) = writer.session.archive.release_claim(&writer.lease) {
            failures.push(format!("claim={error}"));
        }
        // Keep stage and journal for explicit recovery; never remove final media.
        if failures.is_empty() {
            Ok(())
        } else {
            Err(storage(failures.join("; ")))
        }
    }
}

#[derive(Debug)]
struct Session {
    output_root: platform::Root,
    _state_root: platform::Root,
    _lock: File,
    archive: ArchiveStore,
    journal: Journal,
    output: PathBuf,
    output_name: String,
    stage_name: String,
    key: ArchiveKey,
    asset: AssetId,
    job: String,
    source_digest: String,
    key_digest: String,
    profile: FilesystemProfileContract,
}

impl Session {
    fn acquire(&self, provenance: &ArchiveProvenance) -> Result<ArchiveLease, FetchError> {
        let request = ArchiveClaimRequest {
            key: self.key.clone(),
            owner_job_id: provenance.job_id.clone(),
            lease_token: LeaseToken::new(format!("lease_{}", self.key_digest)).map_err(storage)?,
            requested_at_unix_millis: now()?,
            lease_duration_millis: 3_660_000,
            provenance: provenance.clone(),
        };
        match self.archive.claim(&request).map_err(storage)? {
            ArchiveClaimOutcome::Acquired { lease } => Ok(lease),
            other => Err(storage(format!("FF-STORAGE-E-CLAIM: {other:?}"))),
        }
    }

    fn commit_request(
        &self,
        lease: ArchiveLease,
        provenance: ArchiveProvenance,
        bytes: u64,
        sha256: String,
    ) -> Result<ArchiveCommitRequest, FetchError> {
        let output = ReconciledArchiveOutput {
            final_output_identity: self
                .output
                .to_str()
                .ok_or_else(|| storage("non-Unicode output"))?
                .to_owned(),
            artifact_size_bytes: bytes,
            reconciliation_receipt_digest: digest(
                format!("{}:{bytes}:{sha256}", self.key_digest).as_bytes(),
            ),
            artifact_digest: sha256,
            filesystem_commitment: ArchiveFilesystemCommitment {
                profile: self.profile.clone(),
                placement: ArchivePlacementProof::SameFilesystem,
                synchronization: ArchiveSynchronizationProof::DataAndParentDirectory,
                confinement: ArchiveConfinementProof::RootHandleVerified,
            },
            asset_ids: vec![self.asset.clone()],
            derived_output_ids: vec![
                DerivedOutputId::new(format!("output_{}", self.key_digest)).map_err(storage)?,
            ],
        };
        Ok(ArchiveCommitRequest {
            lease,
            provenance,
            output,
            success_event: ArchiveSuccessEvent::PerAsset {
                asset_id: self.asset.clone(),
            },
            committed_at_unix_millis: now()?,
        })
    }

    fn finish(
        &mut self,
        commit: ArchiveCommitRequest,
        recovered: bool,
    ) -> Result<FetchResult, FetchError> {
        let record = match self.archive.commit_success(&commit).map_err(storage)? {
            ArchiveCommitOutcome::Inserted { record }
            | ArchiveCommitOutcome::AlreadyCommitted { record } => record,
        };
        if record.output != commit.output || record.key != self.key {
            return Err(storage("FF-STORAGE-E-ARCHIVE-MISMATCH"));
        }
        self.journal.append(Phase::Archived, Some(commit))?;
        Ok(FetchResult {
            job_id: self.job.clone(),
            output: self.output.clone(),
            bytes: record.output.artifact_size_bytes,
            sha256: record.output.artifact_digest,
            archive_sequence: record.commit_sequence,
            recovered,
        })
    }

    fn recover(
        &mut self,
        maximum: u64,
        control: &FetchControl<'_>,
    ) -> Result<Option<FetchResult>, FetchError> {
        control.check()?;
        let pending = self
            .journal
            .last
            .as_ref()
            .and_then(|record| record.commit.clone());
        let Some(mut commit) = pending else {
            return self.recover_without_commit();
        };
        if commit.lease.key != self.key
            || commit.output.final_output_identity != self.output.to_string_lossy()
        {
            return Err(storage("FF-STORAGE-E-RECOVERY-IDENTITY"));
        }
        let final_exists = self.output_root.exists(&self.output_name)?;
        let path = if final_exists {
            &self.output_name
        } else {
            &self.stage_name
        };
        if !self.output_root.exists(path)? {
            return Err(storage("FF-STORAGE-E-RECOVERY-OUTPUT-MISSING"));
        }
        let mut file = self.output_root.open_existing(path)?;
        let (bytes, sha256) = hash_file(&mut file, maximum, control)?;
        if bytes != commit.output.artifact_size_bytes || sha256 != commit.output.artifact_digest {
            return Err(storage("FF-STORAGE-E-RECOVERY-HASH"));
        }
        file.sync_all().map_err(storage)?;
        control.check()?;
        if !final_exists {
            self.output_root.publish(&file, &self.output_name)?;
        }
        self.output_root.verify(&file, &self.output_name)?;
        self.output_root.sync()?;
        if final_exists {
            control.check()?;
        }
        match self.archive.membership(&self.key).map_err(storage)? {
            ArchiveMembership::Committed { record } => {
                if record.output != commit.output
                    || record.provenance != commit.provenance
                    || record.success_event != commit.success_event
                    || record.claim_lease_token != commit.lease.token
                    || record.claim_lease_generation != commit.lease.generation
                {
                    return Err(storage("FF-STORAGE-E-RECOVERY-ARCHIVE"));
                }
                if self
                    .journal
                    .last
                    .as_ref()
                    .is_some_and(|body| !matches!(body.phase, Phase::Archived))
                {
                    if final_exists {
                        control.check()?;
                    }
                    commit.committed_at_unix_millis = record.committed_at_unix_millis;
                    self.journal.append(Phase::Archived, Some(commit))?;
                }
                return Ok(Some(FetchResult {
                    job_id: self.job.clone(),
                    output: self.output.clone(),
                    bytes,
                    sha256,
                    archive_sequence: record.commit_sequence,
                    recovered: true,
                }));
            }
            ArchiveMembership::Claimed { lease } if !lease.is_stale_at(now()?) => {
                if lease.owner_job_id != commit.provenance.job_id
                    || lease.claim_provenance != commit.provenance
                {
                    return Err(storage("FF-STORAGE-E-RECOVERY-LEASE"));
                }
                commit.lease = *lease;
            }
            ArchiveMembership::Absent | ArchiveMembership::Claimed { .. } => {
                commit.lease = self.acquire(&commit.provenance)?;
            }
            ArchiveMembership::Imported { .. } => {
                return Err(storage("FF-STORAGE-E-RECOVERY-IMPORTED"));
            }
        }
        if final_exists {
            control.check()?;
        }
        commit.committed_at_unix_millis = now()?;
        if self
            .journal
            .last
            .as_ref()
            .is_some_and(|body| matches!(body.phase, Phase::Validated))
        {
            self.journal.append(Phase::Placed, Some(commit.clone()))?;
        }
        self.finish(commit, true).map(Some)
    }

    fn recover_without_commit(&mut self) -> Result<Option<FetchResult>, FetchError> {
        if self.output_root.exists(&self.output_name)? {
            return Err(storage(
                "FF-STORAGE-E-UNOWNED-OUTPUT: no validated recovery record",
            ));
        }
        if let ArchiveMembership::Claimed { lease } =
            self.archive.membership(&self.key).map_err(storage)?
        {
            if lease.owner_job_id.as_str() != self.job
                || lease.claim_provenance.transaction_id.as_str()
                    != format!("transaction_{}", self.key_digest)
                || lease.claim_provenance.source_locator_digest != self.source_digest
            {
                return Err(storage("FF-STORAGE-E-RECOVERY-LEASE"));
            }
            self.archive.release_claim(&lease).map_err(storage)?;
        }
        Ok(None)
    }
}

fn hash_file(
    file: &mut File,
    maximum: u64,
    control: &FetchControl<'_>,
) -> Result<(u64, String), FetchError> {
    file.seek(SeekFrom::Start(0)).map_err(storage)?;
    let mut bytes = 0_u64;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        control.check()?;
        let count = file.read(&mut buffer).map_err(storage)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(u64::try_from(count).map_err(storage)?)
            .ok_or_else(|| FetchError::LimitExceeded("stored byte count overflow".into()))?;
        if bytes > maximum {
            return Err(FetchError::LimitExceeded(format!(
                "stored bytes {bytes} exceed admitted bound {maximum}"
            )));
        }
        hash.update(&buffer[..count]);
    }
    control.check()?;
    Ok((bytes, lower_hex(&hash.finalize())))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Validated,
    Placed,
    Archived,
    Aborted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalBody {
    schema: u32,
    sequence: u64,
    previous: String,
    identity: String,
    phase: Phase,
    commit: Option<ArchiveCommitRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalRecord {
    body: JournalBody,
    checksum: String,
}

#[derive(Debug)]
struct Journal {
    file: File,
    identity: String,
    sequence: u64,
    previous: String,
    last: Option<JournalBody>,
}

impl Journal {
    fn validate_next(&self, body: &JournalBody) -> Result<(), FetchError> {
        let allowed = match self.last.as_ref().map(|last| &last.phase) {
            None | Some(Phase::Aborted) => matches!(body.phase, Phase::Prepared),
            Some(Phase::Prepared) => matches!(
                body.phase,
                Phase::Validated | Phase::Aborted | Phase::Prepared
            ),
            Some(Phase::Validated) => matches!(body.phase, Phase::Placed),
            Some(Phase::Placed) => matches!(body.phase, Phase::Archived),
            Some(Phase::Archived) => false,
        };
        if !allowed
            || matches!(body.phase, Phase::Prepared | Phase::Aborted) != body.commit.is_none()
        {
            return Err(storage("FF-STORAGE-E-JOURNAL-PHASE"));
        }
        if let Some(commit) = &body.commit {
            commit.validate(ArchiveLimits::default()).map_err(storage)?;
            if let Some(previous) = self.last.as_ref().and_then(|last| last.commit.as_ref())
                && (commit.output != previous.output
                    || commit.provenance != previous.provenance
                    || commit.success_event != previous.success_event
                    || commit.lease.key != previous.lease.key
                    || commit.lease.owner_job_id != previous.lease.owner_job_id
                    || commit.lease.claim_provenance != previous.lease.claim_provenance)
            {
                return Err(storage("FF-STORAGE-E-JOURNAL-COMMIT"));
            }
        }
        Ok(())
    }

    fn open(mut file: File, identity: &str) -> Result<Self, FetchError> {
        if file.metadata().map_err(storage)?.len() > JOURNAL_LIMIT as u64 {
            return Err(storage("FF-STORAGE-E-JOURNAL-BOUND"));
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(storage)?;
        let mut journal = Self {
            file,
            identity: identity.to_owned(),
            sequence: 0,
            previous: ZERO_HASH.to_owned(),
            last: None,
        };
        let valid_end = data
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        for line in data[..valid_end]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let record: JournalRecord = serde_json::from_slice(line).map_err(storage)?;
            if record.body.schema != 1
                || record.body.sequence != journal.sequence + 1
                || record.body.identity != identity
                || record.body.previous != journal.previous
                || record.checksum != digest(&serde_json::to_vec(&record.body).map_err(storage)?)
                || record.body.sequence > JOURNAL_RECORD_LIMIT
            {
                return Err(storage("FF-STORAGE-E-JOURNAL-INTEGRITY"));
            }
            journal.validate_next(&record.body)?;
            journal.sequence = record.body.sequence;
            journal.previous = record.checksum;
            journal.last = Some(record.body);
        }
        // Only an incomplete trailing write is truncated; complete corrupt records fail.
        if valid_end < data.len() {
            journal.file.set_len(valid_end as u64).map_err(storage)?;
            journal.file.sync_all().map_err(storage)?;
        }
        journal.file.seek(SeekFrom::End(0)).map_err(storage)?;
        Ok(journal)
    }

    fn append(
        &mut self,
        phase: Phase,
        commit: Option<ArchiveCommitRequest>,
    ) -> Result<(), FetchError> {
        if self.sequence >= JOURNAL_RECORD_LIMIT {
            return Err(storage("FF-STORAGE-E-JOURNAL-RECORD-BOUND"));
        }
        let body = JournalBody {
            schema: 1,
            sequence: self.sequence + 1,
            previous: self.previous.clone(),
            identity: self.identity.clone(),
            phase,
            commit,
        };
        self.validate_next(&body)?;
        let checksum = digest(&serde_json::to_vec(&body).map_err(storage)?);
        let mut wire = serde_json::to_vec(&JournalRecord {
            body: body.clone(),
            checksum: checksum.clone(),
        })
        .map_err(storage)?;
        wire.push(b'\n');
        if self.file.metadata().map_err(storage)?.len() + wire.len() as u64 > JOURNAL_LIMIT as u64 {
            return Err(storage("FF-STORAGE-E-JOURNAL-BYTE-BOUND"));
        }
        self.file.write_all(&wire).map_err(storage)?;
        self.file.sync_all().map_err(storage)?;
        self.sequence = body.sequence;
        self.previous = checksum;
        self.last = Some(body);
        Ok(())
    }
}

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "FF-DEC-004: approved storage handle, volume, identity and flush boundary"
)]
mod platform {
    use super::{FetchError, File, FilesystemProfileContract, Path, PathBuf, storage};
    use std::ffi::OsString;
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Component;
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtCreateFile, NtSetInformationFile,
    };
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::Foundation::UNICODE_STRING;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FlushFileBuffers, GetFileInformationByHandle,
        GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    use windows_sys::Win32::System::SystemInformation::{
        GetProductInfo, OSVERSIONINFOW, PRODUCT_CORE,
    };

    const REPARSE: u32 = 0x400;
    const OPEN_REPARSE: u32 = 0x0020_0000;
    const BACKUP: u32 = 0x0200_0000;
    const DELETE_ACCESS: u32 = 0x0001_0000;

    pub(super) fn validate_name(name: &str) -> Result<(), FetchError> {
        if name.is_empty()
            || name.len() > 240
            || name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        {
            return Err(storage("FF-STORAGE-E-NAME"));
        }
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(storage("FF-STORAGE-E-DEVICE-NAME"));
        }
        Ok(())
    }

    #[derive(Debug)]
    pub(super) struct Root {
        path: PathBuf,
        handles: Vec<File>,
        volume: u32,
        final_path: Vec<u16>,
    }

    impl Root {
        pub(super) fn open(path: &Path, create: bool) -> Result<Self, FetchError> {
            if path.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(storage("FF-STORAGE-E-PARENT-TRAVERSAL"));
            }
            let absolute = std::path::absolute(path).map_err(storage)?;
            if absolute.components().count() > 32
                || absolute.as_os_str().encode_wide().count() > 4096
            {
                return Err(storage(
                    "FF-STORAGE-E-ROOT-BOUND: maximum 32 components and 4096 UTF-16 units",
                ));
            }
            if !matches!(absolute.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
            {
                return Err(storage(
                    "FF-STORAGE-E-ROOT: only local drive roots supported",
                ));
            }
            let mut current = PathBuf::new();
            let mut handles = Vec::new();
            for component in absolute.components() {
                current.push(component.as_os_str());
                if matches!(component, Component::Prefix(_)) {
                    continue;
                }
                let final_component = current == absolute;
                let file = if let Component::Normal(name) = component {
                    relative_open(
                        handles
                            .last()
                            .ok_or_else(|| storage("missing parent handle"))?,
                        name,
                        if create { 3 } else { 1 },
                        true,
                        final_component,
                        3,
                    )?
                } else {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(final_component)
                        .share_mode(3)
                        .custom_flags(BACKUP | OPEN_REPARSE)
                        .open(&current)
                        .map_err(storage)?
                };
                let metadata = information(&file)?;
                if metadata.dwFileAttributes & 0x10 == 0 || metadata.dwFileAttributes & REPARSE != 0
                {
                    return Err(storage("FF-STORAGE-E-ROOT-REPARSE"));
                }
                handles.push(file);
            }
            let last = handles
                .last()
                .ok_or_else(|| storage("FF-STORAGE-E-NO-ROOT-HANDLE"))?;
            let info = information(last)?;
            let final_path = final_name(last)?;
            Ok(Self {
                path: absolute,
                handles,
                volume: info.dwVolumeSerialNumber,
                final_path,
            })
        }

        fn handle(&self) -> &File {
            &self.handles[self.handles.len() - 1]
        }

        pub(super) fn sync(&self) -> Result<(), FetchError> {
            // SAFETY: retained live directory handle; no pointer arguments.
            if unsafe { FlushFileBuffers(self.handle().as_raw_handle()) } == 0 {
                return Err(storage(format!(
                    "FF-STORAGE-E-DIRECTORY-FLUSH: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }

        pub(super) fn profile(&self) -> Result<FilesystemProfileContract, FetchError> {
            let mut fs = [0_u16; 64];
            let mut serial = 0;
            let mut flags = 0;
            let mut version = OSVERSIONINFOW {
                dwOSVersionInfoSize: u32::try_from(size_of::<OSVERSIONINFOW>()).map_err(storage)?,
                ..OSVERSIONINFOW::default()
            };
            let mut product = 0;
            // SAFETY: all buffers are live and sized, optional output pointers null.
            let ok = unsafe {
                GetVolumeInformationByHandleW(
                    self.handle().as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    &raw mut serial,
                    std::ptr::null_mut(),
                    &raw mut flags,
                    fs.as_mut_ptr(),
                    u32::try_from(fs.len()).map_err(storage)?,
                )
            };
            // SAFETY: initialized structure with correct size, live product output.
            if ok == 0
                || unsafe { RtlGetVersion(&raw mut version) } < 0
                || unsafe {
                    GetProductInfo(
                        version.dwMajorVersion,
                        version.dwMinorVersion,
                        0,
                        0,
                        &raw mut product,
                    )
                } == 0
            {
                return Err(storage(std::io::Error::last_os_error()));
            }
            let end = fs.iter().position(|c| *c == 0).unwrap_or(fs.len());
            if String::from_utf16_lossy(&fs[..end]) != "NTFS"
                || serial != self.volume
                || version.dwMajorVersion != 10
                || version.dwBuildNumber != 26200
                || product != PRODUCT_CORE
            {
                return Err(storage(
                    "FF-STORAGE-E-FILESYSTEM-PROFILE: observed host is outside supported frozen profile",
                ));
            }
            Ok(FilesystemProfileContract::windows_11_26200_ntfs_v1())
        }

        pub(super) fn exists(&self, name: &str) -> Result<bool, FetchError> {
            validate_name(name)?;
            match std::fs::symlink_metadata(self.path.join(name)) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(storage(error)),
            }
        }

        pub(super) fn open_file(
            &self,
            name: &str,
            create_new: bool,
            truncate: bool,
        ) -> Result<File, FetchError> {
            validate_name(name)?;
            let file = relative_open(
                self.handle(),
                std::ffi::OsStr::new(name),
                if create_new { 2 } else { 3 },
                false,
                true,
                0,
            )?;
            self.verify(&file, name)?;
            if truncate {
                file.set_len(0).map_err(storage)?;
            }
            Ok(file)
        }

        pub(super) fn open_existing(&self, name: &str) -> Result<File, FetchError> {
            validate_name(name)?;
            let file = relative_open(self.handle(), std::ffi::OsStr::new(name), 1, false, true, 0)?;
            self.verify(&file, name)?;
            Ok(file)
        }

        pub(super) fn open_database(&self, name: &str) -> Result<File, FetchError> {
            self.open_file(name, false, false)
        }

        pub(super) fn verify(&self, file: &File, name: &str) -> Result<(), FetchError> {
            let info = information(file)?;
            let mut expected = self.final_path.clone();
            if expected.last() != Some(&u16::from(b'\\')) {
                expected.push(u16::from(b'\\'));
            }
            expected.extend(OsString::from(name).encode_wide());
            if info.dwFileAttributes & (REPARSE | 0x10) != 0
                || info.nNumberOfLinks != 1
                || info.dwVolumeSerialNumber != self.volume
                || !String::from_utf16_lossy(&final_name(file)?)
                    .eq_ignore_ascii_case(&String::from_utf16_lossy(&expected))
            {
                return Err(storage("FF-STORAGE-E-HANDLE-CONFINEMENT"));
            }
            Ok(())
        }

        pub(super) fn publish(&self, file: &File, name: &str) -> Result<(), FetchError> {
            validate_name(name)?;
            let target: Vec<u16> = OsString::from(name).encode_wide().collect();
            let offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
            let bytes = offset
                .checked_add(target.len() * 2)
                .ok_or_else(|| storage("rename overflow"))?;
            let mut allocation = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
            let pointer = allocation.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
            let mut status_block = IO_STATUS_BLOCK::default();
            // SAFETY: usize allocation gives sufficient alignment and byte extent;
            // fixed header and aligned FileName field address initialize the UTF-16 tail. The
            // owning stage handle is retained; ReplaceIfExists remains false.
            let status = unsafe {
                (*pointer).Anonymous.ReplaceIfExists = false;
                (*pointer).RootDirectory = self.handle().as_raw_handle();
                (*pointer).FileNameLength = u32::try_from(target.len() * 2).map_err(storage)?;
                std::ptr::copy_nonoverlapping(
                    target.as_ptr(),
                    (&raw mut (*pointer).FileName).cast::<u16>(),
                    target.len(),
                );
                NtSetInformationFile(
                    file.as_raw_handle(),
                    &raw mut status_block,
                    pointer.cast(),
                    u32::try_from(bytes).map_err(storage)?,
                    FileRenameInformation,
                )
            };
            if status < 0 {
                return Err(storage(format!(
                    "FF-STORAGE-E-NO-OVERWRITE-RENAME: NTSTATUS {status:#x}"
                )));
            }
            Ok(())
        }
    }

    fn relative_open(
        parent: &File,
        name: &std::ffi::OsStr,
        disposition: u32,
        directory: bool,
        writable: bool,
        sharing: u32,
    ) -> Result<File, FetchError> {
        let text = name
            .to_str()
            .ok_or_else(|| storage("FF-STORAGE-E-NONUNICODE-COMPONENT"))?;
        validate_name(text)?;
        let mut wide: Vec<u16> = name.encode_wide().collect();
        let length = u16::try_from(wide.len() * 2).map_err(storage)?;
        let mut unicode = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: wide.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).map_err(storage)?,
            RootDirectory: parent.as_raw_handle(),
            ObjectName: &raw mut unicode,
            Attributes: 0x40 | 0x1000, // OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE
            SecurityDescriptor: std::ptr::null_mut(),
            SecurityQualityOfService: std::ptr::null_mut(),
        };
        let mut handle = std::ptr::null_mut();
        let mut status_block = IO_STATUS_BLOCK::default();
        let access = 0x8000_0000
            | 0x0010_0000
            | if writable {
                0x4000_0000 | if directory { 0 } else { DELETE_ACCESS }
            } else {
                0
            };
        // SAFETY: a live retained parent anchors exactly one validated component;
        // no path separators or parent traversal enter the object name. All output
        // storage lives through this synchronous call, and no handle is inherited.
        let status = unsafe {
            NtCreateFile(
                &raw mut handle,
                access,
                &raw const attributes,
                &raw mut status_block,
                std::ptr::null(),
                0,
                sharing,
                disposition,
                0x0020_0000 | 0x20 | if directory { 1 } else { 0x40 },
                std::ptr::null(),
                0,
            )
        };
        if status < 0 {
            return Err(storage(format!(
                "FF-STORAGE-E-ROOTED-OPEN: NTSTATUS {status:#x}"
            )));
        }
        // SAFETY: success transferred one owned kernel file handle; File closes it.
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn information(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION, FetchError> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: live file handle and correctly sized initialized output.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
            return Err(storage(std::io::Error::last_os_error()));
        }
        Ok(info)
    }

    fn final_name(file: &File) -> Result<Vec<u16>, FetchError> {
        let mut buffer = vec![0_u16; 32768];
        // SAFETY: live file handle and writable UTF-16 buffer with exact length.
        let count = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).map_err(storage)?,
                0,
            )
        };
        if count == 0 || count as usize >= buffer.len() {
            return Err(storage("FF-STORAGE-E-FINAL-HANDLE-PATH"));
        }
        buffer.truncate(count as usize);
        Ok(buffer)
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{FetchError, File, FilesystemProfileContract, Path, storage};
    pub(super) fn validate_name(_: &str) -> Result<(), FetchError> {
        Ok(())
    }
    #[derive(Debug)]
    pub(super) struct Root;
    impl Root {
        pub(super) fn open(_: &Path, _: bool) -> Result<Self, FetchError> {
            Err(storage(
                "FF-STORAGE-E-CONFINEMENT-UNSUPPORTED: no approved native backend for this platform",
            ))
        }
        pub(super) fn sync(&self) -> Result<(), FetchError> {
            Err(storage("directory flush unavailable"))
        }
        pub(super) fn profile(&self) -> Result<FilesystemProfileContract, FetchError> {
            Err(storage("filesystem profile unavailable"))
        }
        pub(super) fn exists(&self, _: &str) -> Result<bool, FetchError> {
            Err(storage("confined lookup unavailable"))
        }
        pub(super) fn open_file(&self, _: &str, _: bool, _: bool) -> Result<File, FetchError> {
            Err(storage("confined open unavailable"))
        }
        pub(super) fn open_existing(&self, _: &str) -> Result<File, FetchError> {
            Err(storage("confined open unavailable"))
        }
        pub(super) fn open_database(&self, _: &str) -> Result<File, FetchError> {
            Err(storage("confined database open unavailable"))
        }
        pub(super) fn verify(&self, _: &File, _: &str) -> Result<(), FetchError> {
            Err(storage("confinement verification unavailable"))
        }
        pub(super) fn publish(&self, _: &File, _: &str) -> Result<(), FetchError> {
            Err(storage("confined rename unavailable"))
        }
    }
}
