//! Injected ports. The session owns none of the authority behind them: grants, the canonical
//! commit/receipt store and the Job Runtime stay outside (no second queue, no second ledger).
//! Public executable adapters over a transient store belong to kernel-bindings; the doubles that
//! enforce revision, epoch and idempotency for this crate's own proofs are private to its tests.
use crate::error::SessionError;
use hsk_studio_accord::{ActorContext, DomainId};
use hsk_studio_chronicle::RevisionUpdate;
use std::time::Instant;

/// What a grant is checked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Edit,
    Save,
    Export,
}

impl Action {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Save => "save",
            Self::Export => "export",
        }
    }
}

/// Opaque proof that a grant check passed; forwarded to the commit authority, which rechecks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantToken(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantError {
    Revoked,
    Missing,
}

/// Fresh grant check; the session calls it on every submit, replay, save and export.
pub trait GrantPort: Send + Sync {
    fn check(
        &self,
        actor: &ActorContext,
        resource: &DomainId,
        action: Action,
    ) -> Result<GrantToken, GrantError>;
}

/// Caller-supplied idempotency key (1..=128 bytes, no control characters).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: &str) -> Result<Self, SessionError> {
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(SessionError::InvalidKey);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What the authority is asked to commit: the successor document bytes at an expected revision,
/// fenced by epoch and bound to the key through `binding`.
pub struct CommitRequest<'a> {
    pub key: &'a IdempotencyKey,
    pub resource: &'a DomainId,
    pub grant: GrantToken,
    /// Lease epoch of this session; an authority that has moved on answers `StaleEpoch`.
    pub epoch: u64,
    pub expected_revision: u64,
    pub successor_revision: u64,
    /// Digest of principal, resource, action, provider, payload hash and expected revision.
    pub binding: &'a str,
    pub successor_document: &'a [u8],
    pub revision_updates: &'a [RevisionUpdate],
}

/// The authority's canonical receipt for a committed key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitReceipt {
    pub revision: u64,
    /// Authority-assigned, monotonically increasing.
    pub sequence: u64,
    pub binding: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed(CommitReceipt),
    Rejected(&'static str),
    /// The authority holds a different revision than `expected_revision`.
    Stale {
        current_revision: u64,
    },
    StaleEpoch,
    /// Timeout, lost acknowledgement or crash: the commit may or may not have happened.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    Found(CommitReceipt),
    Absent,
    /// The authority could not be asked (unreachable): the outcome is still unknown.
    Unknown,
}

pub trait CommitPort: Send + Sync {
    /// Commits atomically and idempotently by `request.key` + `request.binding`.
    fn commit(&self, request: &CommitRequest<'_>) -> CommitOutcome;
    /// Rediscovers the receipt of an earlier commit attempt.
    fn lookup(&self, key: &IdempotencyKey) -> Lookup;
}

pub struct JobScope<'a> {
    pub resource: &'a DomainId,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    pub jobs: u32,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobTicket(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobRefused {
    pub retry_after_ms: u64,
}

/// The existing Job Runtime, reached through a port. The session releases every ticket it was
/// given on every exit path.
pub trait JobPort: Send + Sync {
    fn admit(&self, scope: &JobScope<'_>, cost: Cost) -> Result<JobTicket, JobRefused>;
    fn release(&self, ticket: JobTicket);
}

/// Millisecond clock port (a test clock makes drain deadlines deterministic).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Monotonic wall-clock default.
#[derive(Clone, Copy, Debug)]
pub struct MonotonicClock {
    start: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
