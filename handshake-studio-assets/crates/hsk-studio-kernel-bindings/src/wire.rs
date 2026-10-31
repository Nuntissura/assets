//! Versioned wire envelopes and their strict codec. The wire is compact JSON in canonical field
//! order (struct order, no whitespace); decoding refuses everything the encoder cannot produce:
//! unknown or duplicate fields, a foreign `bridge_version`, trailing bytes, oversize input,
//! malformed ids and non-canonical hex. Canonical form is enforced, not assumed: a decoded value
//! is re-encoded and must equal the input byte for byte, so interior whitespace, reordered keys,
//! `\u` escapes and omitted `null` fields are `non_canonical`. Nothing here names a host, a store
//! or a transport.
//!
//! Proven by `tests/bridge_contract.rs`: canonical-only decoding by `only_canonical_bytes_decode`
//! (whitespace, key order, escapes, omitted null, number forms, comment and case decoys, the host
//! boundary, replies); every other refusal and its stable code by `wire_strictness_and_descriptor`;
//! hostile sizes by `hostile_sizes_are_refused_typed`.
#![deny(clippy::arithmetic_side_effects)]
pub use crate::context::CommandContextWire;
use hsk_studio_accord::{ActorContext, DomainId};
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_session::Action;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Wire generation. Any other value is refused before the rest of the message is interpreted.
pub const BRIDGE_VERSION: u32 = 1;
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_REVISION_UPDATES: usize = 4096;
const MAX_CODE_BYTES: usize = 64;

/// Decode/encode bounds. Both are enforced before parsing and after, not taken from the input
/// (`wire_strictness_and_descriptor`: oversize; `hostile_sizes_are_refused_typed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted/produced message.
    pub max_bytes: usize,
    /// Largest decoded successor document carried by a commit.
    pub max_document_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 4 * 1024 * 1024,
            max_document_bytes: 1024 * 1024,
        }
    }
}

impl Limits {
    pub(crate) fn valid(&self) -> bool {
        self.max_bytes > 0 && self.max_document_bytes > 0
    }
}

/// Why a message was refused. Codes are stable snake_case and distinct per cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Empty,
    Oversize,
    Syntax,
    TrailingBytes,
    UnknownField,
    DuplicateKey,
    MissingField,
    UnknownVariant,
    InvalidType,
    WrongVersion,
    InvalidResourceId,
    InvalidActor,
    InvalidKey,
    InvalidHex,
    DocumentTooLarge,
    TooManyUpdates,
    InvalidLimits,
    InvalidCode,
    EncodeFailed,
    /// Well-formed and schema-valid, but not the exact bytes the encoder produces for that value.
    NonCanonical,
    /// The envelope's `command` names a client session or command id outside accord's bounds.
    InvalidCommandContext,
}

impl WireError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Oversize => "oversize",
            Self::Syntax => "syntax",
            Self::TrailingBytes => "trailing_bytes",
            Self::UnknownField => "unknown_field",
            Self::DuplicateKey => "duplicate_key",
            Self::MissingField => "missing_field",
            Self::UnknownVariant => "unknown_variant",
            Self::InvalidType => "invalid_type",
            Self::WrongVersion => "wrong_version",
            Self::InvalidResourceId => "invalid_resource_id",
            Self::InvalidActor => "invalid_actor",
            Self::InvalidKey => "invalid_key",
            Self::InvalidHex => "invalid_hex",
            Self::DocumentTooLarge => "document_too_large",
            Self::TooManyUpdates => "too_many_updates",
            Self::InvalidLimits => "invalid_limits",
            Self::InvalidCode => "invalid_code",
            Self::EncodeFailed => "encode_failed",
            Self::NonCanonical => "non_canonical",
            Self::InvalidCommandContext => "invalid_command_context",
        }
    }
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for WireError {}

// ----- vocabulary ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionWire {
    Edit,
    Save,
    Export,
}

impl From<Action> for ActionWire {
    fn from(action: Action) -> Self {
        match action {
            Action::Edit => Self::Edit,
            Action::Save => Self::Save,
            Action::Export => Self::Export,
        }
    }
}

impl From<ActionWire> for Action {
    fn from(action: ActionWire) -> Self {
        match action {
            ActionWire::Edit => Self::Edit,
            ActionWire::Save => Self::Save,
            ActionWire::Export => Self::Export,
        }
    }
}

/// Portable attribution fields (the real authenticated principal arrives with a host adapter).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorWire {
    pub account_id: String,
    pub principal_id: String,
    pub owner_account_id: String,
    pub owner_principal_id: String,
    pub access_space_id: String,
    pub session_id: String,
}

impl ActorWire {
    pub fn from_context(actor: &ActorContext) -> Self {
        Self {
            account_id: actor.account_id().to_owned(),
            principal_id: actor.principal_id().to_owned(),
            owner_account_id: actor.owner_account_id().to_owned(),
            owner_principal_id: actor.owner_principal_id().to_owned(),
            access_space_id: actor.access_space_id().to_owned(),
            session_id: actor.session_id().to_owned(),
        }
    }

    pub fn to_context(&self) -> Result<ActorContext, WireError> {
        ActorContext::new(
            &self.account_id,
            &self.principal_id,
            &self.owner_account_id,
            &self.owner_principal_id,
            &self.access_space_id,
            &self.session_id,
        )
        .map_err(|_| WireError::InvalidActor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationWire {
    Open,
    Submit,
    Reconcile,
    Drain,
    Save,
    Export,
    Grant,
    Commit,
    Job,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeWire {
    Success,
    Canceled,
    FailureValidation,
    FailureUnavailable,
    FailureLoss,
    FailureUnsupported,
}

impl OutcomeWire {
    /// `None` for progress, which is not a terminal diagnostic.
    pub fn from_observe(outcome: Outcome) -> Option<Self> {
        match outcome {
            Outcome::Progress => None,
            Outcome::Success => Some(Self::Success),
            Outcome::Canceled => Some(Self::Canceled),
            Outcome::Failure(FailureCode::Validation) => Some(Self::FailureValidation),
            Outcome::Failure(FailureCode::Unavailable) => Some(Self::FailureUnavailable),
            Outcome::Failure(FailureCode::Loss) => Some(Self::FailureLoss),
            Outcome::Failure(FailureCode::Unsupported) => Some(Self::FailureUnsupported),
        }
    }

    pub fn to_observe(self) -> Outcome {
        match self {
            Self::Success => Outcome::Success,
            Self::Canceled => Outcome::Canceled,
            Self::FailureValidation => Outcome::Failure(FailureCode::Validation),
            Self::FailureUnavailable => Outcome::Failure(FailureCode::Unavailable),
            Self::FailureLoss => Outcome::Failure(FailureCode::Loss),
            Self::FailureUnsupported => Outcome::Failure(FailureCode::Unsupported),
        }
    }
}

/// A host-side refusal. Static strings so session's `CommitOutcome::Rejected(&'static str)` can
/// carry them without allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    GrantRevoked,
    GrantMissing,
    IdempotencyConflict,
    SuccessorMismatch,
    ResourceUnknown,
    ReceiptsFull,
    InvalidRequest,
    /// The request claims a lease epoch the host has not issued (newer than the host's own).
    EpochNotIssued,
}

impl RejectCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GrantRevoked => "grant_revoked",
            Self::GrantMissing => "grant_missing",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::SuccessorMismatch => "successor_mismatch",
            Self::ResourceUnknown => "resource_unknown",
            Self::ReceiptsFull => "receipts_full",
            Self::InvalidRequest => "invalid_request",
            Self::EpochNotIssued => "epoch_not_issued",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    Revoked,
    Missing,
}

// ----- request ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionUpdateWire {
    /// Compact JSON of the property address, opaque to the bridge.
    pub address: String,
    pub previous_revision: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitMsg {
    pub grant_token: u64,
    pub successor_revision: u64,
    /// Session-computed digest of principal, resource, action, provider, payload and revision.
    pub binding: String,
    /// Successor document bytes, lowercase hex.
    pub document_hex: String,
    pub revision_updates: Vec<RevisionUpdateWire>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticMsg {
    pub operation: OperationWire,
    pub outcome: OutcomeWire,
    /// Stable snake_case code (the character set is enforced by `valid_code`); by construction it
    /// carries no names, paths or document text.
    pub code: Option<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Body {
    GrantCheck { action: ActionWire },
    Commit(CommitMsg),
    /// Read-only reconcile request: "what did you record under this envelope's key?"
    Lookup,
    JobAdmit { action: ActionWire, jobs: u32, bytes: u64 },
    JobRelease { ticket: u64 },
    Diagnostic(DiagnosticMsg),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub bridge_version: u32,
    /// Per-exchange reply-matching counter minted by the adapter; a reply is accepted only when
    /// it echoes this value. It is NOT the command correlation (that is `command.correlation`).
    pub correlation_id: u64,
    /// Required for `commit` and `lookup`.
    pub idempotency_key: Option<String>,
    pub actor: ActorWire,
    pub resource_id: String,
    pub expected_revision: u64,
    pub lease_epoch: u64,
    pub body: Body,
    /// The mutating command this exchange belongs to (lane SMB-ATTRIBUTION-1). Absent, it is not
    /// encoded at all, so an envelope without it keeps its exact bytes; `null` is non-canonical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandContextWire>,
}

// ----- reply -----------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptWire {
    pub revision: u64,
    pub sequence: u64,
    pub binding: String,
    /// Opaque host reference (an event-ledger event id on a real host); carried as bounded text
    /// and not interpreted.
    pub host_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantDecision {
    pub granted: bool,
    pub token: u64,
    pub reason: Option<DenyReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CommitDecision {
    Accepted(ReceiptWire),
    Rejected { code: RejectCode },
    Stale { current_revision: u64 },
    StaleEpoch,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LookupDecision {
    Found(ReceiptWire),
    Absent,
}

/// Why the host refused a job request outright (as opposed to `Refused`, which is load: retry).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobDenyCode {
    /// Admission: the principal holds no standing grant for the resource and action.
    GrantMissing,
    /// Admission: the principal's grant for the resource and action was withdrawn.
    GrantRevoked,
    /// Release: the ticket was admitted for another account, principal or resource.
    NotTicketOwner,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum JobDecision {
    Admitted { ticket: u64 },
    Refused { retry_after_ms: u64 },
    Denied { code: JobDenyCode },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorReply {
    /// A [`WireError::code`] of the request the host could not accept.
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplyBody {
    Grant(GrantDecision),
    Commit(CommitDecision),
    Lookup(LookupDecision),
    Job(JobDecision),
    Ack,
    Error(ErrorReply),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub bridge_version: u32,
    /// Echo of the request's correlation id (`0` when the request could not be decoded).
    pub correlation_id: u64,
    pub result: ReplyBody,
}

impl Reply {
    pub fn new(correlation_id: u64, result: ReplyBody) -> Self {
        Self {
            bridge_version: BRIDGE_VERSION,
            correlation_id,
            result,
        }
    }
}

/// Terminal observe outcome of a host reply (what the diagnostic for it reports).
pub fn diagnostic_outcome(body: &ReplyBody) -> Outcome {
    match body {
        ReplyBody::Commit(CommitDecision::Accepted(_)) => Outcome::Success,
        ReplyBody::Commit(CommitDecision::Unknown) => Outcome::Failure(FailureCode::Unavailable),
        ReplyBody::Commit(_) => Outcome::Failure(FailureCode::Validation),
        ReplyBody::Grant(grant) if grant.granted => Outcome::Success,
        ReplyBody::Grant(_) => Outcome::Failure(FailureCode::Validation),
        ReplyBody::Lookup(_) | ReplyBody::Ack => Outcome::Success,
        ReplyBody::Job(JobDecision::Admitted { .. }) => Outcome::Success,
        ReplyBody::Job(JobDecision::Refused { .. }) => Outcome::Failure(FailureCode::Unavailable),
        ReplyBody::Job(JobDecision::Denied { .. }) => Outcome::Failure(FailureCode::Validation),
        ReplyBody::Error(_) => Outcome::Failure(FailureCode::Validation),
    }
}

// ----- hex -------------------------------------------------------------------------------------

pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Lowercase hex only (`hex_encode` emits nothing else; `wire_strictness_and_descriptor` refuses
/// uppercase); bounded by `max_bytes` before any byte is decoded.
pub fn hex_decode(text: &str, max_bytes: usize) -> Result<Vec<u8>, WireError> {
    let raw = text.as_bytes();
    if !raw.len().is_multiple_of(2) {
        return Err(WireError::InvalidHex);
    }
    if raw.len() / 2 > max_bytes {
        return Err(WireError::DocumentTooLarge);
    }
    // Digit values are 0..=15 by construction (the match arms bound `b`), so the wrapping
    // operations never actually wrap.
    let digit = |b: u8| match b {
        b'0'..=b'9' => Ok(b.wrapping_sub(b'0')),
        b'a'..=b'f' => Ok(b.wrapping_sub(b'a').wrapping_add(10)),
        _ => Err(WireError::InvalidHex),
    };
    raw.chunks_exact(2)
        .map(|pair| Ok((digit(pair[0])?.wrapping_shl(4)) | digit(pair[1])?))
        .collect()
}

// ----- validation ------------------------------------------------------------------------------

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_KEY_BYTES && !value.chars().any(char::is_control)
}

fn valid_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CODE_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Envelope {
    pub fn validate(&self, limits: &Limits) -> Result<(), WireError> {
        if !limits.valid() {
            return Err(WireError::InvalidLimits);
        }
        if self.bridge_version != BRIDGE_VERSION {
            return Err(WireError::WrongVersion);
        }
        DomainId::parse(&self.resource_id).map_err(|_| WireError::InvalidResourceId)?;
        self.actor.to_context()?;
        if let Some(command) = &self.command {
            command.to_context(&self.actor)?;
        }
        match (&self.body, self.idempotency_key.as_deref()) {
            (Body::Commit(_) | Body::Lookup, None) => return Err(WireError::InvalidKey),
            (_, Some(key)) if !valid_text(key) => return Err(WireError::InvalidKey),
            _ => {}
        }
        match &self.body {
            Body::Commit(commit) => {
                if !valid_text(&commit.binding) {
                    return Err(WireError::InvalidKey);
                }
                if commit.revision_updates.len() > MAX_REVISION_UPDATES {
                    return Err(WireError::TooManyUpdates);
                }
                hex_decode(&commit.document_hex, limits.max_document_bytes)?;
            }
            Body::Diagnostic(diagnostic) => {
                if diagnostic.code.as_deref().is_some_and(|c| !valid_code(c)) {
                    return Err(WireError::InvalidCode);
                }
            }
            Body::GrantCheck { .. }
            | Body::Lookup
            | Body::JobAdmit { .. }
            | Body::JobRelease { .. } => {}
        }
        Ok(())
    }
}

impl Reply {
    pub fn validate(&self) -> Result<(), WireError> {
        if self.bridge_version != BRIDGE_VERSION {
            return Err(WireError::WrongVersion);
        }
        match &self.result {
            ReplyBody::Error(error) if !valid_code(&error.code) => Err(WireError::InvalidCode),
            ReplyBody::Commit(CommitDecision::Accepted(receipt))
            | ReplyBody::Lookup(LookupDecision::Found(receipt))
                if !valid_text(&receipt.binding) || !valid_text(&receipt.host_ref) =>
            {
                Err(WireError::InvalidKey)
            }
            _ => Ok(()),
        }
    }
}

// ----- codec -----------------------------------------------------------------------------------

fn classify(error: &serde_json::Error) -> WireError {
    let message = error.to_string();
    if message.starts_with("trailing characters") {
        WireError::TrailingBytes
    } else if message.starts_with("unknown field") {
        WireError::UnknownField
    } else if message.starts_with("duplicate field") {
        WireError::DuplicateKey
    } else if message.starts_with("missing field") {
        WireError::MissingField
    } else if message.starts_with("unknown variant") {
        WireError::UnknownVariant
    } else if message.starts_with("invalid type")
        || message.starts_with("invalid value")
        || message.starts_with("invalid length")
    {
        WireError::InvalidType
    } else {
        WireError::Syntax
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    bridge_version: u32,
}

fn decode<T: DeserializeOwned + Serialize>(bytes: &[u8], limits: &Limits) -> Result<T, WireError> {
    if !limits.valid() {
        return Err(WireError::InvalidLimits);
    }
    if bytes.is_empty() {
        return Err(WireError::Empty);
    }
    if bytes.len() > limits.max_bytes {
        return Err(WireError::Oversize);
    }
    // A message is exactly one JSON object: nothing (not even whitespace) outside the braces.
    if bytes[0] != b'{' {
        return Err(WireError::Syntax);
    }
    if bytes.last().is_some_and(u8::is_ascii_whitespace) {
        return Err(WireError::TrailingBytes);
    }
    // The generation is read first and alone: a foreign version is `wrong_version`
    // (`wire_strictness_and_descriptor`), not a misleading unknown-field error from a schema this
    // decoder does not speak.
    let probe = VersionProbe::deserialize(&mut serde_json::Deserializer::from_slice(bytes))
        .map_err(|e| classify(&e))?;
    if probe.bridge_version != BRIDGE_VERSION {
        return Err(WireError::WrongVersion);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = T::deserialize(&mut deserializer).map_err(|e| classify(&e))?;
    deserializer.end().map_err(|e| classify(&e))?;
    // Canonical form is enforced by construction: whatever the parser tolerated (interior
    // whitespace, reordered keys, `\u` escapes, an omitted `null` field) is refused unless the
    // decoded value re-encodes to exactly the bytes received.
    let canonical = serde_json::to_vec(&value).map_err(|_| WireError::EncodeFailed)?;
    if canonical != bytes {
        return Err(WireError::NonCanonical);
    }
    Ok(value)
}

fn encode<T: Serialize>(value: &T, limits: &Limits) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::to_vec(value).map_err(|_| WireError::EncodeFailed)?;
    if bytes.len() > limits.max_bytes {
        return Err(WireError::Oversize);
    }
    Ok(bytes)
}

/// Canonical bytes of a valid envelope; a message that would not decode is not produced
/// (`wire_strictness_and_descriptor`: a foreign version is refused at encode; a decoded value
/// re-encodes to its input).
pub fn encode_envelope(envelope: &Envelope, limits: &Limits) -> Result<Vec<u8>, WireError> {
    envelope.validate(limits)?;
    encode(envelope, limits)
}

pub fn decode_envelope(bytes: &[u8], limits: &Limits) -> Result<Envelope, WireError> {
    let envelope: Envelope = decode(bytes, limits)?;
    envelope.validate(limits)?;
    Ok(envelope)
}

pub fn encode_reply(reply: &Reply, limits: &Limits) -> Result<Vec<u8>, WireError> {
    reply.validate()?;
    encode(reply, limits)
}

pub fn decode_reply(bytes: &[u8], limits: &Limits) -> Result<Reply, WireError> {
    let reply: Reply = decode(bytes, limits)?;
    reply.validate()?;
    Ok(reply)
}
