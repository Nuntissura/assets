//! Import orchestration: bounded open, version gate, identity, loss report, sanitized-export policy.
//! Nothing here claims editable support: `FigImport::editable` is the constant `NotProven`.
use crate::container::{
    CANVAS_PART, FigContainer, FigKind, IMAGES_PREFIX, Limits, META_PART, MetaKeys, Sha256Bytes,
    THUMBNAIL_PART, open_with_hash, sha256,
};
use crate::kiwi::decode_canvas;
use crate::semantic_decode::EditableStatus;
use crate::version_map::{VersionPolicy, check_version};
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

pub type PartName = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossKind {
    /// Kept byte-exact, not interpreted.
    PreservedOpaque,
    /// Not interpreted and not mapped into any Studio model.
    Unsupported,
}

impl LossKind {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::PreservedOpaque => "preserved_opaque",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossEntry {
    pub part: PartName,
    pub kind: LossKind,
    pub code: &'static str,
}

/// Source-qualified node id, preserved verbatim from the file and never reminted: `"session:local"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId {
    pub session: u32,
    pub local: u32,
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.session, self.local)
    }
}

/// A fully decoded census of the Kiwi message (never partial). Not an editable document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Census {
    pub node_type_histogram: std::collections::BTreeMap<String, u32>,
    pub node_count: u32,
    pub root_ids: Vec<SourceId>,
}

/// Definition names, kinds and field names/ids of the Kiwi schema chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaSummary {
    pub definitions: Vec<DefinitionSummary>,
    /// Index of the chunk that held the schema, and its sha256.
    pub chunk_index: usize,
    pub chunk_sha256: Sha256Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionSummary {
    pub name: String,
    pub kind: &'static str,
    pub fields: Vec<FieldSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSummary {
    pub name: String,
    pub id: u32,
    pub type_name: String,
    pub is_array: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FigImport {
    /// Every part and chunk byte-exact (opaque preservation).
    pub container: FigContainer,
    pub kind: FigKind,
    pub version: u32,
    /// sha256 of the exact input bytes.
    pub source_sha256: Sha256Bytes,
    pub parts_sha256: Vec<(PartName, Sha256Bytes)>,
    pub schema_summary: Option<SchemaSummary>,
    pub census: Option<Census>,
    pub loss: Vec<LossEntry>,
    /// Constant until original-produced semantic/render proof exists (STU-IO-186).
    pub editable: EditableStatus,
}

/// Result of an import that opened safely. Hostile, stale or canceled input is `Err` (no container
/// is handed back); an unsupported version is `OpaqueOnly` (container preserved, nothing decoded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FigOutcome {
    Imported(Box<FigImport>),
    OpaqueOnly {
        container: Box<FigContainer>,
        error: FigError,
    },
}

impl FigOutcome {
    pub fn error(&self) -> Option<&FigError> {
        match self {
            Self::Imported(_) => None,
            Self::OpaqueOnly { error, .. } => Some(error),
        }
    }
}

fn loss_for_part(name: &str) -> (LossKind, &'static str) {
    if name == CANVAS_PART {
        (LossKind::Unsupported, "semantic_decode_blocked")
    } else if name == META_PART {
        (LossKind::PreservedOpaque, "meta_json_top_level_keys_only")
    } else if name == THUMBNAIL_PART {
        (LossKind::PreservedOpaque, "thumbnail_not_decoded")
    } else if name.starts_with(IMAGES_PREFIX) {
        (LossKind::PreservedOpaque, "image_not_decoded")
    } else {
        (LossKind::PreservedOpaque, "unknown_part_preserved")
    }
}

fn base_loss(container: &FigContainer) -> Vec<LossEntry> {
    let mut loss: Vec<LossEntry> = container
        .parts
        .iter()
        .map(|p| {
            let (kind, code) = loss_for_part(&p.name);
            LossEntry {
                part: p.name.clone(),
                kind,
                code,
            }
        })
        .collect();
    match container.meta {
        MetaKeys::Unreadable => loss.push(LossEntry {
            part: META_PART.to_owned(),
            kind: LossKind::PreservedOpaque,
            code: "meta_json_unreadable",
        }),
        MetaKeys::NotObject => loss.push(LossEntry {
            part: META_PART.to_owned(),
            kind: LossKind::PreservedOpaque,
            code: "meta_json_not_object",
        }),
        _ => {}
    }
    if matches!(container.kind, FigKind::Unknown(_)) {
        loss.push(LossEntry {
            part: CANVAS_PART.to_owned(),
            kind: LossKind::PreservedOpaque,
            code: "canvas_kind_unclassified",
        });
    }
    loss
}

/// Opens, version-gates and identifies a .fig input. `expected_sha256` binds the call to the bytes
/// a previous inspection saw (`StaleSource` when they differ). Cancellation is checked at entry and
/// per entry/chunk; a canceled call returns `Canceled` and no partial result.
pub fn import_fig(
    bytes: &[u8],
    limits: &Limits,
    policy: &VersionPolicy,
    expected_sha256: Option<&Sha256Bytes>,
    cancel: &CancellationToken,
) -> Result<FigOutcome> {
    cancel.check().map_err(|_| FigError::Canceled)?;
    if bytes.len() > limits.max_bytes {
        return Err(FigError::BudgetExceeded("bytes"));
    }
    let source_sha256 = sha256(bytes);
    if let Some(expected) = expected_sha256
        && *expected != source_sha256
    {
        return Err(FigError::StaleSource);
    }
    let container = open_with_hash(bytes, source_sha256, limits, cancel)?;
    if let Err(error) = check_version(&container, policy) {
        return Ok(FigOutcome::OpaqueOnly {
            container: Box::new(container),
            error,
        });
    }
    let mut loss = base_loss(&container);
    let decoded = decode_canvas(&container, limits, cancel)?;
    loss.extend(decoded.loss);
    let parts_sha256 = container
        .parts
        .iter()
        .map(|p| (p.name.clone(), p.sha256))
        .collect();
    Ok(FigOutcome::Imported(Box::new(FigImport {
        kind: container.kind,
        version: container.version,
        source_sha256,
        parts_sha256,
        schema_summary: decoded.schema,
        census: decoded.census,
        loss,
        editable: EditableStatus::NotProven,
        container,
    })))
}

/// Which parts a sanitized export would carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SanitizePolicy {
    /// Keeps every part (STU-IO-188 preservation export).
    Preservation,
    /// Excludes the thumbnail, embedded images and `meta.json` (STU-IO-188 sanitized export).
    Sanitized,
}

/// Names of the parts the policy would exclude from an export, in container order. The preservation
/// policy excludes none. There is no write path in this slice.
pub fn sanitized_summary(import: &FigImport, policy: SanitizePolicy) -> Vec<PartName> {
    match policy {
        SanitizePolicy::Preservation => Vec::new(),
        SanitizePolicy::Sanitized => import
            .container
            .parts
            .iter()
            .filter(|p| {
                p.name == THUMBNAIL_PART || p.name == META_PART || p.name.starts_with(IMAGES_PREFIX)
            })
            .map(|p| p.name.clone())
            .collect(),
    }
}

/// Receipt of one `report` call. `editable` is `NotProven` on every receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FigReceipt {
    pub receipt: Receipt,
    pub editable: EditableStatus,
}

/// Maps an import result to exactly one Observe terminal outcome (no ids, part names or text).
/// An import with losses is still `Success`; the loss list is the detail. Unsupported version and
/// unrecognised input are `Failure(Unsupported)`, a blocked/missing decoder `Failure(Unavailable)`,
/// hostile or malformed input `Failure(Validation)`, cancellation `Canceled`. `correlation_id` and
/// `revision` must equal the values `observe` was created with.
pub fn report(
    result: &Result<FigOutcome>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<FigReceipt, hsk_studio_observe::Error> {
    let error = match result {
        Ok(outcome) => outcome.error(),
        Err(error) => Some(error),
    };
    let outcome = match error {
        None => Outcome::Success,
        Some(error) => match error.failure_code() {
            None => Outcome::Canceled,
            Some(code) => Outcome::Failure(code),
        },
    };
    let receipt = observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )?;
    Ok(FigReceipt {
        receipt,
        editable: EditableStatus::NotProven,
    })
}
