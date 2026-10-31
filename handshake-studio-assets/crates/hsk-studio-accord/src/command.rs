//! Typed command context (lane SMB-ATTRIBUTION-1, RF-06; GUI-DESKTOP-029/041/045/050,
//! STU-ARC-014/015): who issued a mutating command (actor and client session), which command,
//! against which document revision, and the correlation (plus the causing command's correlation)
//! that ties its frames, patches and history steps together.
//!
//! Like [`ActorContext`], every field is a bounded transport identifier: a context grants
//! nothing. A host maps its own wire envelope onto this type through one adapter
//! (`hsk-studio-kernel-bindings` `CommandContextWire`). Correlations are opaque `u64` values so
//! they fit Observe's correlation field; they are never an operation ordinal.
use crate::{ActorContext, Result, ValidationError};
use std::sync::atomic::{AtomicU64, Ordering};

/// Same bounds as the [`ActorContext`] ids: non-empty, at most 128 bytes, no control character.
fn bounded(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

/// The client session a command came from (one desktop window, one CLI process, one agent
/// session). Opaque; `InvalidContext` for an empty, oversize or control-character value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClientSessionId(String);

impl ClientSessionId {
    pub fn parse(value: &str) -> Result<Self> {
        if bounded(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ValidationError::InvalidContext)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A stable command id (a controls `Registry` / `RUN_COMMANDS` id such as `layer.rename`).
/// Opaque; `InvalidContext` for an empty, oversize or control-character value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CommandId(String);

impl CommandId {
    pub fn parse(value: &str) -> Result<Self> {
        if bounded(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ValidationError::InvalidContext)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The correlation of one command. Opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CorrelationId(u64);

impl CorrelationId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

/// The correlation of the command that caused this one (an undo names the step it undoes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CausationId(u64);

impl CausationId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

impl From<CorrelationId> for CausationId {
    fn from(correlation: CorrelationId) -> Self {
        Self(correlation.0)
    }
}

/// Where correlations come from. A host supplies its own (process birth, trace ids);
/// [`TransientCorrelation`] is the in-memory one.
pub trait CorrelationSource: Send + Sync {
    fn next(&self) -> CorrelationId;
}

/// Monotonic in-memory correlations, process-local and never persisted (asset-local: a host
/// replaces it with a source bound to its own trace ids). Saturates at `u64::MAX`.
#[derive(Debug)]
pub struct TransientCorrelation(AtomicU64);

impl TransientCorrelation {
    /// Correlations 1, 2, 3, ...
    pub fn new() -> Self {
        Self::starting_at(1)
    }

    /// Correlations `first`, `first + 1`, ...
    pub fn starting_at(first: u64) -> Self {
        Self(AtomicU64::new(first))
    }
}

impl Default for TransientCorrelation {
    fn default() -> Self {
        Self::new()
    }
}

impl CorrelationSource for TransientCorrelation {
    fn next(&self) -> CorrelationId {
        let previous = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_add(1)))
            .unwrap_or(u64::MAX);
        CorrelationId(previous)
    }
}

/// One mutating command's attribution. Transport only: it grants nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandContext {
    actor: ActorContext,
    client_session: ClientSessionId,
    command: Option<CommandId>,
    base_revision: u64,
    correlation: CorrelationId,
    causation: Option<CausationId>,
}

impl CommandContext {
    /// `command` is `None` for an operation without a registered command id; `base_revision`
    /// is the document revision the command was issued against.
    pub fn new(
        actor: ActorContext,
        client_session: ClientSessionId,
        command: Option<CommandId>,
        base_revision: u64,
        correlation: CorrelationId,
        causation: Option<CausationId>,
    ) -> Self {
        Self {
            actor,
            client_session,
            command,
            base_revision,
            correlation,
            causation,
        }
    }

    pub fn actor(&self) -> &ActorContext {
        &self.actor
    }

    pub fn client_session(&self) -> &ClientSessionId {
        &self.client_session
    }

    pub fn command(&self) -> Option<&CommandId> {
        self.command.as_ref()
    }

    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }

    pub fn correlation(&self) -> CorrelationId {
        self.correlation
    }

    pub fn causation(&self) -> Option<CausationId> {
        self.causation
    }
}
