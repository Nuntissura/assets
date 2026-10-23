#![deny(clippy::arithmetic_side_effects)]
//! Typed CKC catalog port contracts (lanes SMB-CKC-PORTS, SMB-CKC-PORTS-ALIGN). CKC is the single
//! asset library, media catalog and file manager (STU-ASSET-001); Studio "MUST NOT implement ... a
//! second metadata, keyword, rating, label or flag store; a second collection or smart-collection
//! mechanism" (STU-ASSET-004) and reaches CKC "through typed commands, never through direct
//! filesystem access or a private index" (STU-ASSET-010). This module holds ONLY request/receipt
//! types, their validation and the injected [`CkcCatalogPort`] trait; it stores, indexes and
//! evaluates nothing.
//!
//! Alignment (SMB-CKC-PORTS-ALIGN). Field names, value domains and ids follow the Master Spec
//! entity shapes CKC owns: `Asset.asset_id: UUID` (Spec 2.2.3.1), and the `PhotoAsset` library
//! metadata `rating: 0|1|2|3|4|5`, `color_label?: 'red'|'yellow'|'green'|'blue'|'purple'`,
//! `flag: 'none'|'pick'|'reject'`, `keywords: string[]`, `collections: UUID[]` plus the technical
//! fields `capture_time`, `camera_make`, `camera_model`, `lens_model`, `focal_length`, `aperture`,
//! `iso` (Spec 2.2.3.2). Command ids `asset.get` and `asset.search` are STU-ASSET-010's.
//! Revision semantics follow STU-SDB-004 (expected-revision compare-and-set, fail closed on
//! conflict; a duplicate idempotency key returns the prior receipt without a second mutation or
//! event) and STU-ASSET-023 (asset-granularity precondition); receipts carry the EventLedger event
//! id (STU-ARC-004, STU-ASSET-013). Batch metadata follows WP-KERNEL-005 MT-014/MT-018 ("all-target
//! validation before mutation and one batch receipt", "batch-safe updates"). The WP-KERNEL-005
//! contract (packet, refinement, MT-014/015/018/019/034/035/045/047) is summary-level and names no
//! wire fields, operators or command ids for these capabilities; every Studio name kept for that
//! reason is listed by [`CatalogField::ckc_field`] returning `None` or in [`PROPOSED_CKC_ADDITIONS`].

/// Port contract version; a host refuses a different major.
pub const CKC_PORT_VERSION: u32 = 3;

/// Studio names the CKC contract does not define yet, proposed as CKC additions (lane notes of
/// SMB-CKC-PORTS-ALIGN carry the rationale per entry).
pub const PROPOSED_CKC_ADDITIONS: &[&str] = &[
    "keyword hierarchy (KeywordPath root-first segments; PhotoAsset.keywords is string[])",
    "keyword synonyms (catalog.keyword.define_synonyms)",
    "collection sets (CollectionKind::Set and parent links; MT-034 defines flat collections)",
    "smart collections with a boolean rule tree (MT-047 saved searches are flat filters)",
    "rule operators (eq, ne, gt, ge, lt, le, in_range, contains_any, contains_all, contains_words, contains_none, begins_with, ends_with)",
    "fields file_format, folder, any_text, has_adjustments",
    "command ids asset.update_library_metadata, asset.update_library_metadata_batch, catalog.keyword.define_synonyms, catalog.collection.*, asset.write_xmp_sidecar",
    "asset-granularity revision on PhotoAsset library metadata (STU-ASSET-023 requires it; the CKC contract names none)",
    "whole keyword-set replacement (MetadataChange::SetKeywords; MT-019 names bulk add/remove only)",
    "collection listing with smart rules (catalog.collection.list) so Studio can preview smart-rule membership",
    "XMP sidecar byte read/write with a sidecar revision (asset.read_xmp_sidecar / asset.write_xmp_sidecar_bytes); Studio serializes, CKC owns the I/O",
];

/// Bounds every request is validated against before it is sent and again by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CkcLimits {
    pub max_key_bytes: usize,
    pub max_name_bytes: usize,
    pub max_keyword_segments: usize,
    pub max_segment_bytes: usize,
    pub max_changes: usize,
    pub max_members: usize,
    pub max_batch: usize,
    pub max_synonyms: usize,
    pub max_rule_depth: usize,
    pub max_rule_nodes: usize,
    pub max_text_bytes: usize,
    pub max_sort_keys: usize,
    pub max_page: u32,
    pub max_xmp_keywords: usize,
    /// Keywords one `SetKeywords` change may carry (SMB-LR-LIBRARY-WRITE).
    pub max_keywords_per_asset: usize,
    /// Collections a rule context may hold (smart-rule preview, SMB-LR-LIBRARY-WRITE).
    pub max_collections: usize,
    /// Nested smart-collection references followed by one evaluation.
    pub max_reference_depth: usize,
    /// Criterion evaluations one record evaluation may spend (bounds rule fan-out through references).
    pub max_eval_steps: usize,
    /// Assets one library edit may address after selection resolution.
    pub max_selection: usize,
    /// XMP sidecar bytes one write request may carry.
    pub max_sidecar_bytes: usize,
}

impl Default for CkcLimits {
    fn default() -> Self {
        Self {
            max_key_bytes: 128,
            max_name_bytes: 1024,
            max_keyword_segments: 32,
            max_segment_bytes: 255,
            max_changes: 256,
            max_members: 10_000,
            max_batch: 1000,
            max_synonyms: 64,
            max_rule_depth: 16,
            max_rule_nodes: 4096,
            max_text_bytes: 4096,
            max_sort_keys: 8,
            max_page: 1000,
            max_xmp_keywords: 4096,
            max_keywords_per_asset: 4096,
            max_collections: 10_000,
            max_reference_depth: 32,
            max_eval_steps: 1_000_000,
            max_selection: 100_000,
            max_sidecar_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Typed refusal. `Unauthorized` deliberately carries nothing (no name, path or existence leak).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CkcError {
    Unauthorized,
    NotFound,
    StaleRevision { current: u64 },
    Invalid(&'static str),
    Limit(&'static str),
    ParentNotSet,
    ParentCycle,
    Conflict(&'static str),
    Unavailable,
}

impl CkcError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "ckc_unauthorized",
            Self::NotFound => "ckc_not_found",
            Self::StaleRevision { .. } => "ckc_stale_revision",
            Self::Invalid(c) | Self::Limit(c) | Self::Conflict(c) => c,
            Self::ParentNotSet => "ckc_parent_not_set",
            Self::ParentCycle => "ckc_parent_cycle",
            Self::Unavailable => "ckc_unavailable",
        }
    }
}

/// Idempotency key (STU-SDB-004): ASCII `[A-Za-z0-9._:-]`, at most `max_key_bytes`.
pub(crate) fn check_key(s: &str, l: &CkcLimits) -> Result<(), CkcError> {
    if s.is_empty() || s.len() > l.max_key_bytes {
        return Err(CkcError::Limit("ckc_key_length"));
    }
    if !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')) {
        return Err(CkcError::Invalid("ckc_key_charset"));
    }
    Ok(())
}

fn check_text(s: &str, max: usize, code: &'static str) -> Result<(), CkcError> {
    if s.len() > max {
        return Err(CkcError::Limit(code));
    }
    if s.chars().any(char::is_control) {
        return Err(CkcError::Invalid("ckc_text_control_char"));
    }
    Ok(())
}

/// Canonical lowercase hyphenated UUID text (8-4-4-4-12 hex).
fn check_uuid(s: &str) -> Result<(), CkcError> {
    let b = s.as_bytes();
    let ok = b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        });
    if ok { Ok(()) } else { Err(CkcError::Invalid("ckc_uuid")) }
}

fn uuid_text(bytes: [u8; 16]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// `Asset.asset_id: UUID` (Spec 2.2.3.1; the `PRIM-Asset` handle of STU-ASSET-005).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(String);

impl AssetId {
    pub fn new(uuid: &str) -> Result<Self, CkcError> {
        check_uuid(uuid)?;
        Ok(Self(uuid.to_owned()))
    }
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(uuid_text(bytes))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Collection identity: `PhotoAsset.collections: UUID[]` (Spec 2.2.3.2; collections per MT-034).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollectionId(String);

impl CollectionId {
    pub fn new(uuid: &str) -> Result<Self, CkcError> {
        check_uuid(uuid)?;
        Ok(Self(uuid.to_owned()))
    }
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(uuid_text(bytes))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `PhotoAsset.rating: 0|1|2|3|4|5` (Spec 2.2.3.2); 0 is unrated. CKC clamps to 0..5 on its side
/// (WP-KERNEL-005 MT-018 "ratings (0..5 clamp)"); this port refuses out-of-range values before
/// sending so nothing reaches the clamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rating(u8);

impl Rating {
    pub const UNRATED: Self = Self(0);
    pub fn new(stars: u8) -> Result<Self, CkcError> {
        if stars <= 5 { Ok(Self(stars)) } else { Err(CkcError::Invalid("ckc_rating_range")) }
    }
    pub fn value(self) -> u8 {
        self.0
    }
}

/// `PhotoAsset.flag: 'none'|'pick'|'reject'` (Spec 2.2.3.2); shown as Flagged / Unflagged /
/// Rejected (Spec 10.10.4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Flag {
    Pick,
    Unflagged,
    Reject,
}

impl Flag {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Unflagged => "none",
            Self::Reject => "reject",
        }
    }
    pub fn from_wire(s: &str) -> Option<Self> {
        [Self::Pick, Self::Unflagged, Self::Reject].into_iter().find(|f| f.wire() == s)
    }
}

/// `PhotoAsset.color_label?: 'red'|'yellow'|'green'|'blue'|'purple'` (Spec 2.2.3.2, 10.10.4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ColorLabel {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl ColorLabel {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
        }
    }
    pub fn from_wire(s: &str) -> Option<Self> {
        [Self::Red, Self::Yellow, Self::Green, Self::Blue, Self::Purple].into_iter().find(|c| c.wire() == s)
    }
}

/// Hierarchical keyword, root first (Spec 10.10.4.5 "Hierarchical keywords"). PROPOSED CKC
/// addition: `PhotoAsset.keywords` is `string[]` (CKC "tags", MT-019) with no hierarchy;
/// [`KeywordPath::leaf`] is the flat keyword a host without hierarchy stores.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeywordPath(Vec<String>);

impl KeywordPath {
    pub fn new<S: AsRef<str>>(segments: &[S], limits: &CkcLimits) -> Result<Self, CkcError> {
        if segments.is_empty() {
            return Err(CkcError::Invalid("ckc_keyword_empty"));
        }
        if segments.len() > limits.max_keyword_segments {
            return Err(CkcError::Limit("ckc_keyword_segments"));
        }
        let mut out = Vec::with_capacity(segments.len());
        for s in segments {
            let s = s.as_ref();
            if s.is_empty() || s.contains('|') {
                return Err(CkcError::Invalid("ckc_keyword_segment"));
            }
            check_text(s, limits.max_segment_bytes, "ckc_keyword_segment_length")?;
            out.push(s.to_owned());
        }
        Ok(Self(out))
    }
    pub fn segments(&self) -> &[String] {
        &self.0
    }
    pub fn leaf(&self) -> &str {
        self.0.last().map_or("", String::as_str)
    }
}

/// Who acts (STU-ASSET-022): the same commands for operators and model lanes, attributed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActorKind {
    Operator,
    Model,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CkcActor {
    pub kind: ActorKind,
    pub principal_id: String,
}

/// One change to the library metadata of the PhotoAsset whose `source_asset` is the addressed
/// asset (Spec 2.2.3.2 fields `rating`, `flag`, `color_label`, `keywords`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataChange {
    SetRating(Rating),
    SetFlag(Flag),
    SetColorLabel(Option<ColorLabel>),
    AddKeyword(KeywordPath),
    RemoveKeyword(KeywordPath),
    /// Replaces the whole keyword set (order kept; duplicates refused). PROPOSED CKC addition.
    SetKeywords(Vec<KeywordPath>),
}

/// Library metadata update of one asset (command id `asset.update_library_metadata`, PROPOSED):
/// applied atomically under the expected-revision precondition; `idempotency_key` replays the
/// prior receipt (STU-SDB-004, STU-ASSET-023).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataMutation {
    pub idempotency_key: String,
    pub asset_id: AssetId,
    pub expected_revision: u64,
    pub changes: Vec<MetadataChange>,
}

fn check_changes(changes: &[MetadataChange], l: &CkcLimits) -> Result<(), CkcError> {
    if changes.is_empty() {
        return Err(CkcError::Invalid("ckc_no_changes"));
    }
    if changes.len() > l.max_changes {
        return Err(CkcError::Limit("ckc_changes"));
    }
    for change in changes {
        if let MetadataChange::SetKeywords(set) = change {
            check_keyword_set(set, l)?;
        }
    }
    Ok(())
}

/// A whole keyword set: at most `max_keywords_per_asset` entries (checked before any hashing), no
/// repeated path.
pub(crate) fn check_keyword_set(set: &[KeywordPath], l: &CkcLimits) -> Result<(), CkcError> {
    if set.len() > l.max_keywords_per_asset {
        return Err(CkcError::Limit("ckc_keyword_set"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for k in set {
        if !seen.insert(k) {
            return Err(CkcError::Invalid("ckc_keyword_repeated"));
        }
    }
    Ok(())
}

impl MetadataMutation {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_key(&self.idempotency_key, l)?;
        check_changes(&self.changes, l)
    }
}

/// One target of a batch: asset, its expected revision and its changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTarget {
    pub asset_id: AssetId,
    pub expected_revision: u64,
    pub changes: Vec<MetadataChange>,
}

/// Batch library metadata update (WP-KERNEL-005 MT-014 "all-target-validation-before-mutation
/// guardrails and one batch receipt", MT-018 "batch-safe updates"): either every target applies
/// or none does; one receipt per batch. Repeated assets are refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataBatch {
    pub idempotency_key: String,
    pub targets: Vec<BatchTarget>,
}

impl MetadataBatch {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_key(&self.idempotency_key, l)?;
        if self.targets.is_empty() || self.targets.len() > l.max_batch {
            return Err(CkcError::Limit("ckc_batch"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in &self.targets {
            check_changes(&t.changes, l)?;
            if !seen.insert(&t.asset_id) {
                return Err(CkcError::Invalid("ckc_batch_repeated_asset"));
            }
        }
        Ok(())
    }
}

/// Catalog-wide keyword commands (PROPOSED: CKC has no synonym concept).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeywordCommand {
    DefineSynonyms { idempotency_key: String, keyword: KeywordPath, synonyms: Vec<String> },
}

impl KeywordCommand {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        let Self::DefineSynonyms { idempotency_key, synonyms, .. } = self;
        check_key(idempotency_key, l)?;
        if synonyms.len() > l.max_synonyms {
            return Err(CkcError::Limit("ckc_synonyms"));
        }
        for s in synonyms {
            if s.is_empty() {
                return Err(CkcError::Invalid("ckc_synonym_empty"));
            }
            check_text(s, l.max_segment_bytes, "ckc_synonym_length")?;
        }
        Ok(())
    }
}

/// Field a rule or sort refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogField {
    Rating,
    Flag,
    ColorLabel,
    Keywords,
    Collections,
    OriginalFilename,
    CaptureTime,
    CameraMake,
    CameraModel,
    LensModel,
    FocalLength,
    Aperture,
    Iso,
    FileFormat,
    Folder,
    AnyText,
    HasAdjustments,
}

impl CatalogField {
    /// The CKC-owned field name this maps to (Spec 2.2.3.1 `Asset`, 2.2.3.2 `PhotoAsset`), or
    /// `None` for a Studio field the CKC contract does not define (PROPOSED addition).
    pub const fn ckc_field(self) -> Option<&'static str> {
        Some(match self {
            Self::Rating => "PhotoAsset.rating",
            Self::Flag => "PhotoAsset.flag",
            Self::ColorLabel => "PhotoAsset.color_label",
            Self::Keywords => "PhotoAsset.keywords",
            Self::Collections => "PhotoAsset.collections",
            Self::OriginalFilename => "Asset.original_filename",
            Self::CaptureTime => "PhotoAsset.capture_time",
            Self::CameraMake => "PhotoAsset.camera_make",
            Self::CameraModel => "PhotoAsset.camera_model",
            Self::LensModel => "PhotoAsset.lens_model",
            Self::FocalLength => "PhotoAsset.focal_length",
            Self::Aperture => "PhotoAsset.aperture",
            Self::Iso => "PhotoAsset.iso",
            Self::FileFormat | Self::Folder | Self::AnyText | Self::HasAdjustments => return None,
        })
    }
}

/// Rule operators (PROPOSED: the CKC contract defines none; MT-047 saved searches reproduce
/// tags/exclusions/ratings/favorite/color/scope as flat filters).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    /// Inclusive `value ..= value2`.
    InRange,
    ContainsAny,
    ContainsAll,
    ContainsWords,
    /// The MT-047 "exclusions" filter is `ContainsNone` on `Keywords`.
    ContainsNone,
    BeginsWith,
    EndsWith,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CatalogValue {
    Int(i64),
    /// Finite real (focal length, aperture).
    Real(f64),
    Text(String),
    Bool(bool),
    Rating(Rating),
    Flag(Flag),
    ColorLabel(ColorLabel),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub field: CatalogField,
    pub op: CatalogOp,
    pub value: CatalogValue,
    pub value2: Option<CatalogValue>,
}

/// Rule tree of a smart collection or a search (`asset.search`, STU-ASSET-010; a smart collection
/// as batch input is a `catalog_query`, STU-AUT-127). Typed and bounded; never evaluated here.
#[derive(Clone, Debug, PartialEq)]
pub enum CatalogRule {
    All(Vec<CatalogRule>),
    Any(Vec<CatalogRule>),
    NoneOf(Vec<CatalogRule>),
    Match(Criterion),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ordinal,
    Text,
    Exact,
    Bool,
}

fn field_kind(f: CatalogField) -> Kind {
    use CatalogField::*;
    match f {
        Rating | Iso | CaptureTime | FocalLength | Aperture => Kind::Ordinal,
        Flag | ColorLabel => Kind::Exact,
        HasAdjustments => Kind::Bool,
        Keywords | Collections | OriginalFilename | CameraMake | CameraModel | LensModel | FileFormat | Folder | AnyText => {
            Kind::Text
        }
    }
}

fn value_fits(f: CatalogField, v: &CatalogValue) -> bool {
    use CatalogField as F;
    use CatalogValue as V;
    match (f, v) {
        (F::Rating, V::Rating(_) | V::Int(0..=5)) | (F::Iso, V::Int(_)) => true,
        (F::FocalLength | F::Aperture, V::Real(x)) => x.is_finite(),
        (F::FocalLength | F::Aperture, V::Int(_)) => true,
        (F::CaptureTime, V::Text(_)) | (F::Flag, V::Flag(_)) | (F::ColorLabel, V::ColorLabel(_)) => true,
        (F::HasAdjustments, V::Bool(_)) => true,
        (
            F::Keywords | F::Collections | F::OriginalFilename | F::CameraMake | F::CameraModel | F::LensModel | F::FileFormat
            | F::Folder | F::AnyText,
            V::Text(_),
        ) => true,
        _ => false,
    }
}

impl Criterion {
    fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        use CatalogOp::*;
        let ok_op = match field_kind(self.field) {
            Kind::Ordinal => matches!(self.op, Eq | Ne | Gt | Ge | Lt | Le | InRange),
            Kind::Exact | Kind::Bool => matches!(self.op, Eq | Ne),
            Kind::Text => matches!(
                self.op,
                Eq | Ne | ContainsAny | ContainsAll | ContainsWords | ContainsNone | BeginsWith | EndsWith
            ),
        };
        if !ok_op {
            return Err(CkcError::Invalid("ckc_rule_op_for_field"));
        }
        for v in std::iter::once(&self.value).chain(self.value2.iter()) {
            if !value_fits(self.field, v) {
                return Err(CkcError::Invalid("ckc_rule_value_for_field"));
            }
            if let CatalogValue::Text(t) = v {
                check_text(t, l.max_text_bytes, "ckc_rule_text_length")?;
            }
        }
        if (self.op == InRange) != self.value2.is_some() {
            return Err(CkcError::Invalid("ckc_rule_range_operand"));
        }
        Ok(())
    }
}

impl CatalogRule {
    /// Validates depth, node count, operator/field/value compatibility and text bounds with an
    /// explicit stack (no recursion), so a hostile tree is refused without deep call stacks.
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        let mut stack: Vec<(&CatalogRule, usize)> = vec![(self, 1)];
        let mut nodes = 0usize;
        while let Some((rule, depth)) = stack.pop() {
            nodes = nodes.saturating_add(1);
            if nodes > l.max_rule_nodes {
                return Err(CkcError::Limit("ckc_rule_nodes"));
            }
            if depth > l.max_rule_depth {
                return Err(CkcError::Limit("ckc_rule_depth"));
            }
            match rule {
                CatalogRule::Match(c) => c.validate(l)?,
                CatalogRule::All(v) | CatalogRule::Any(v) | CatalogRule::NoneOf(v) => {
                    if v.is_empty() {
                        return Err(CkcError::Invalid("ckc_rule_empty_group"));
                    }
                    if v.len() > l.max_rule_nodes.saturating_sub(nodes) {
                        return Err(CkcError::Limit("ckc_rule_nodes"));
                    }
                    for child in v {
                        stack.push((child, depth.saturating_add(1)));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionKind {
    /// Manual collection / photo set (MT-034).
    Manual,
    /// Container of collections; the only legal parent kind (PROPOSED: collection sets).
    Set,
}

/// Collection commands (MT-034 collections with membership; sets and smart collections are
/// PROPOSED additions). A parent must be a `Set` and no parent chain may cycle; deletes and rule
/// changes carry the expected revision (STU-SDB-004).
#[derive(Clone, Debug, PartialEq)]
pub enum CollectionCommand {
    Create { idempotency_key: String, collection_id: CollectionId, name: String, parent: Option<CollectionId>, kind: CollectionKind },
    CreateSmart { idempotency_key: String, collection_id: CollectionId, name: String, parent: Option<CollectionId>, rule: CatalogRule },
    AddMembers { idempotency_key: String, collection_id: CollectionId, asset_ids: Vec<AssetId> },
    RemoveMembers { idempotency_key: String, collection_id: CollectionId, asset_ids: Vec<AssetId> },
    SetRule { idempotency_key: String, collection_id: CollectionId, expected_revision: u64, rule: CatalogRule },
    Delete { idempotency_key: String, collection_id: CollectionId, expected_revision: u64 },
}

impl CollectionCommand {
    pub fn idempotency_key(&self) -> &str {
        match self {
            Self::Create { idempotency_key, .. }
            | Self::CreateSmart { idempotency_key, .. }
            | Self::AddMembers { idempotency_key, .. }
            | Self::RemoveMembers { idempotency_key, .. }
            | Self::SetRule { idempotency_key, .. }
            | Self::Delete { idempotency_key, .. } => idempotency_key,
        }
    }

    pub const fn command_id(&self) -> &'static str {
        match self {
            Self::Create { .. } => "catalog.collection.create",
            Self::CreateSmart { .. } => "catalog.collection.create_smart",
            Self::AddMembers { .. } => "catalog.collection.add_members",
            Self::RemoveMembers { .. } => "catalog.collection.remove_members",
            Self::SetRule { .. } => "catalog.collection.set_rule",
            Self::Delete { .. } => "catalog.collection.delete",
        }
    }

    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_key(self.idempotency_key(), l)?;
        match self {
            Self::Create { name, .. } | Self::CreateSmart { name, .. } => {
                if name.is_empty() {
                    return Err(CkcError::Invalid("ckc_name_empty"));
                }
                check_text(name, l.max_name_bytes, "ckc_name_length")?;
            }
            Self::AddMembers { asset_ids, .. } | Self::RemoveMembers { asset_ids, .. }
                if asset_ids.is_empty() || asset_ids.len() > l.max_members =>
            {
                return Err(CkcError::Limit("ckc_members"));
            }
            _ => {}
        }
        match self {
            Self::CreateSmart { rule, .. } | Self::SetRule { rule, .. } => rule.validate(l),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortKey {
    pub field: CatalogField,
    pub descending: bool,
}

/// `asset.search` (STU-ASSET-010): optional rule, sort keys (ties broken by asset id), offset page.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchRequest {
    pub rule: Option<CatalogRule>,
    pub sort: Vec<SortKey>,
    pub offset: u64,
    pub limit: u32,
}

impl SearchRequest {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        if self.sort.len() > l.max_sort_keys {
            return Err(CkcError::Limit("ckc_sort_keys"));
        }
        if self.limit == 0 || self.limit > l.max_page {
            return Err(CkcError::Limit("ckc_page"));
        }
        if let Some(r) = &self.rule {
            r.validate(l)?;
        }
        Ok(())
    }
}

/// Library metadata of one asset as CKC projects it (`asset.get` read model, Spec 2.2.3.2 field
/// names; `revision` is the STU-ASSET-023 precondition value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetMetadata {
    pub asset_id: AssetId,
    pub revision: u64,
    pub rating: Rating,
    pub flag: Flag,
    pub color_label: Option<ColorLabel>,
    pub keywords: Vec<KeywordPath>,
    pub collections: Vec<CollectionId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPage {
    pub assets: Vec<AssetMetadata>,
    pub total: u64,
    pub next_offset: Option<u64>,
}

/// `xmp:Rating` value domain of the public XMP specification: -1 rejected, 0..5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmpRatingValue {
    Rejected,
    Stars(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarPlacement {
    /// `<original basename>.xmp` beside the original, written by CKC through its own owner.
    BesideOriginal,
    /// Registered as a derived CKC asset of kind `sidecar_xmp` (Spec 2.2.3.1 `AssetKind::SIDECAR_XMP`,
    /// STU-ASSET-012/014) with no file beside the original.
    CatalogArtifactOnly,
}

/// Request that CKC write an XMP sidecar (command id `asset.write_xmp_sidecar`, PROPOSED). Studio
/// supplies the fields; CKC owns serialization, bytes and write (STU-ASSET-019, 14.12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmpSidecarWrite {
    pub idempotency_key: String,
    pub asset_id: AssetId,
    pub expected_revision: u64,
    pub rating: Option<XmpRatingValue>,
    pub label: Option<String>,
    pub keywords: Vec<String>,
    pub hierarchical_keywords: Vec<KeywordPath>,
    pub placement: SidecarPlacement,
}

impl XmpSidecarWrite {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_key(&self.idempotency_key, l)?;
        if let Some(XmpRatingValue::Stars(n)) = self.rating
            && n > 5
        {
            return Err(CkcError::Invalid("ckc_rating_range"));
        }
        if let Some(label) = &self.label {
            check_text(label, l.max_name_bytes, "ckc_label_length")?;
        }
        if self.keywords.len().saturating_add(self.hierarchical_keywords.len()) > l.max_xmp_keywords {
            return Err(CkcError::Limit("ckc_xmp_keywords"));
        }
        for k in &self.keywords {
            check_text(k, l.max_segment_bytes, "ckc_keyword_segment_length")?;
        }
        Ok(())
    }
}

/// Kind of a listed collection; a smart collection carries its rule.
#[derive(Clone, Debug, PartialEq)]
pub enum ListedKind {
    Manual,
    Set,
    Smart(CatalogRule),
}

/// One collection as CKC lists it (`catalog.collection.list`, PROPOSED): what Studio needs to
/// preview smart-rule membership and resolve collection criteria. Membership of manual
/// collections comes from `AssetMetadata.collections`; Studio stores none of it.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionSummary {
    pub collection_id: CollectionId,
    pub name: String,
    pub parent: Option<CollectionId>,
    pub kind: ListedKind,
    pub revision: u64,
}

/// Sidecar bytes CKC holds for an asset with their own revision (a sidecar edit is a
/// compare-and-set on it, STU-SDB-004).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidecarBytes {
    pub bytes: Vec<u8>,
    pub revision: u64,
}

/// Write of Studio-serialized XMP sidecar bytes (command id `asset.write_xmp_sidecar_bytes`,
/// PROPOSED). Studio owns the XMP serialization (file interop, 14.12 "bounded explicit XMP
/// interchange read/write through CKC"); CKC owns the I/O, placement and EventLedger record.
/// `expected_sidecar_revision` is `None` when Studio read no sidecar (the write must create one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmpSidecarBytesWrite {
    pub idempotency_key: String,
    pub asset_id: AssetId,
    pub expected_sidecar_revision: Option<u64>,
    pub bytes: Vec<u8>,
    pub placement: SidecarPlacement,
}

impl XmpSidecarBytesWrite {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_key(&self.idempotency_key, l)?;
        if self.bytes.is_empty() || self.bytes.len() > l.max_sidecar_bytes {
            return Err(CkcError::Limit("ckc_sidecar_bytes"));
        }
        Ok(())
    }
}

/// Receipt of an accepted command: command id, target, new revision, the EventLedger event that
/// recorded it (STU-ARC-004 `event_ledger_event_id`, STU-ASSET-013) and who acted (STU-ASSET-022).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CkcReceipt {
    pub idempotency_key: String,
    pub command_id: &'static str,
    pub target: String,
    pub revision: u64,
    pub event_ledger_event_id: String,
    pub actor: CkcActor,
}

/// Command ids per port method: `asset.get` and `asset.search` are STU-ASSET-010's; the rest are
/// PROPOSED (the CKC contract names no command ids for them).
pub const COMMAND_IDS: &[(&str, &str)] = &[
    ("get", "asset.get"),
    ("search", "asset.search"),
    ("update_library_metadata", "asset.update_library_metadata"),
    ("update_library_metadata_batch", "asset.update_library_metadata_batch"),
    ("keyword", "catalog.keyword.define_synonyms"),
    ("collection", "catalog.collection.*"),
    ("request_xmp_sidecar", "asset.write_xmp_sidecar"),
    ("list_collections", "catalog.collection.list"),
    ("read_xmp_sidecar", "asset.read_xmp_sidecar"),
    ("write_xmp_sidecar_bytes", "asset.write_xmp_sidecar_bytes"),
];

/// The injected CKC provider port. Implementations (the Handshake host adapter over CKC; in tests
/// the feature-gated double) MUST re-validate every request, authorize the actor per call through
/// the ResourceBroker (STU-ASSET-021), enforce the expected revision and replay a duplicate
/// idempotency key's prior receipt without a second mutation or event (STU-SDB-004).
pub trait CkcCatalogPort {
    fn get(&self, actor: &CkcActor, asset_id: &AssetId) -> Result<AssetMetadata, CkcError>;
    fn search(&self, actor: &CkcActor, request: &SearchRequest) -> Result<SearchPage, CkcError>;
    fn update_library_metadata(&self, actor: &CkcActor, mutation: &MetadataMutation) -> Result<CkcReceipt, CkcError>;
    fn update_library_metadata_batch(&self, actor: &CkcActor, batch: &MetadataBatch) -> Result<CkcReceipt, CkcError>;
    fn keyword(&self, actor: &CkcActor, command: &KeywordCommand) -> Result<CkcReceipt, CkcError>;
    fn collection(&self, actor: &CkcActor, command: &CollectionCommand) -> Result<CkcReceipt, CkcError>;
    fn request_xmp_sidecar(&self, actor: &CkcActor, request: &XmpSidecarWrite) -> Result<CkcReceipt, CkcError>;
    /// Collections with their kinds and smart rules (port v3; a host without it is `Unavailable`).
    fn list_collections(&self, actor: &CkcActor) -> Result<Vec<CollectionSummary>, CkcError> {
        let _ = actor;
        Err(CkcError::Unavailable)
    }
    /// Current sidecar bytes of an asset, `None` when it has none (port v3).
    fn read_xmp_sidecar(&self, actor: &CkcActor, asset_id: &AssetId) -> Result<Option<SidecarBytes>, CkcError> {
        let _ = (actor, asset_id);
        Err(CkcError::Unavailable)
    }
    /// Writes Studio-serialized sidecar bytes under the sidecar-revision precondition (port v3);
    /// a moved sidecar is `StaleRevision { current }` (`current` 0 = the sidecar read is gone).
    fn write_xmp_sidecar_bytes(&self, actor: &CkcActor, request: &XmpSidecarBytesWrite) -> Result<CkcReceipt, CkcError> {
        let _ = (actor, request);
        Err(CkcError::Unavailable)
    }
}
