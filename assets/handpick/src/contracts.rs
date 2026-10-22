use serde::{Deserialize, Serialize};

/// Maximum UTF-8 bytes in an opaque host-supplied identity.
pub const MAX_ID_BYTES: usize = 256;

macro_rules! identity {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            /// Validate a nonempty, bounded host identity; no database syntax is interpreted.
            pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
                Self::try_from(value.into())
            }
            /// Return the opaque identity for routing, never as a content excerpt.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = &'static str;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.is_empty() || value.len() > MAX_ID_BYTES {
                    Err("identity must contain 1..=256 UTF-8 bytes")
                } else {
                    Ok(Self(value))
                }
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}
identity!(
    ContextId,
    "Opaque authorized account/profile/workspace context supplied by the host."
);
identity!(ProviderId, "Opaque search provider identity.");
identity!(SessionId, "Host-unique lifecycle identity; never reused for a replacement session.");
identity!(EditorId, "Opaque native Notes editor handle; content, selection and undo remain with its owner.");
identity!(NoteId, "Canonical Notes identity, never a Quicknote entity.");
identity!(BlockId, "Permanent Notes-owned nested block identity.");
identity!(OperationId, "Stable owner-deduplicated canonical operation identity.");
identity!(FolderId, "Owner-resolved Notes container identity.");
identity!(RevisionId, "Opaque owner revision or content hash fence.");
identity!(RelationshipId, "Owner-derived committed relationship identity.");
identity!(
    ResultId,
    "Opaque canonical result identity, meaningful within its provider."
);
identity!(
    ActionId,
    "Opaque action identity routed to its owning host service."
);
identity!(
    PinId,
    "Host-supplied unique bookmark identity, unrelated to its hidden excerpt."
);

/// Supported wire version. Unknown versions fail deserialization explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireVersion {
    /// Initial foundation contract; not a finalized query grammar.
    #[serde(rename = "handpick.v1")]
    V1,
}

/// Versioned value for transport across application adapters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    /// Explicit contract version.
    pub version: WireVersion,
    /// Application-independent payload.
    pub payload: T,
}
impl<T> Envelope<T> {
    /// Wrap a foundation payload in the supported version.
    pub fn v1(payload: T) -> Self {
        Self {
            version: WireVersion::V1,
            payload,
        }
    }
}

/// Capability available from an adapter, distinct from an executable action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Search,
    Complete,
    Open,
    RevealSetting,
    RunCommand,
    ApplyFilter,
    Capture,
}

/// Explicit intent; discovering an action never executes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Open,
    RevealSetting,
    RunCommand,
    ApplyFilter,
    ApplyCompletion,
    Capture,
}

/// Host-authorized action descriptor; owners must recheck access at execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    /// Opaque owner-routed identity.
    pub id: ActionId,
    /// Kind of intent, without a database or GUI handle.
    pub kind: ActionKind,
}

/// Stable result identity across reordering and updates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultKey {
    /// Owning provider.
    pub provider: ProviderId,
    /// Canonical identity within that provider.
    pub result: ResultId,
}

/// Authorized suggestion/result descriptor; the session bounds supplied payloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchItem {
    /// Stable identity, never a rendered row index.
    pub key: ResultKey,
    /// Host-provided display text.
    pub label: String,
    /// Optional host-authorized context excerpt.
    pub detail: Option<String>,
    /// Explicit actions; no action executes when a result arrives.
    pub actions: Vec<Action>,
}

/// Independent delivery channel; empty suggestions do not imply empty content search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Suggestions,
    Search,
}

/// Observable provider state. Only `Complete` denotes a completed result collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Complete,
    Offline,
    Partial,
    Failed,
    StaleIndex,
}

/// Token fencing asynchronous delivery; hosts retain it unchanged with their request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fence {
    /// Authorized context at dispatch.
    pub context: ContextId,
    /// Monotonically increasing session request generation.
    pub generation: u64,
    /// Draft revision at dispatch.
    pub draft_revision: u64,
}

/// Explicit UTF-8 byte range in the original draft, with an exclusive end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteSpan {
    /// Inclusive byte start.
    pub start: usize,
    /// Exclusive byte end.
    pub end: usize,
}

/// Replacement computed against a fenced original draft. Hosts convert UTF-16/grapheme coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionEdit {
    /// Context/generation/revision of the source draft.
    pub fence: Fence,
    /// Range in original UTF-8 bytes; character boundaries are validated.
    pub span: ByteSpan,
    /// Literal replacement; never parsed as query syntax or saved automatically.
    pub replacement: String,
}
