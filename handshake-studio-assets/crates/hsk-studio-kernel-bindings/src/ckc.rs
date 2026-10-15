#![deny(clippy::arithmetic_side_effects)]
//! Typed CKC catalog port contracts (lane SMB-CKC-PORTS). CKC is the single asset library, media
//! catalog and file manager (STU-ASSET-001); Studio "MUST NOT implement ... a second metadata,
//! keyword, rating, label or flag store; a second collection or smart-collection mechanism"
//! (STU-ASSET-004) and reaches CKC "through typed commands, never through direct filesystem access
//! or a private index" (STU-ASSET-010). This module therefore holds ONLY request/receipt types,
//! their validation and the [`CkcCatalogPort`] trait a host injects ("standalone reusable assets
//! MUST consume artifact/catalog bytes through injected typed provider ports", STU-ASSET-004). It
//! stores, indexes and evaluates nothing. The capability vocabulary is the Library/DAM set of Spec
//! 10.10.4.5 inherited by STU-ASSET-019: 0-to-5 star rating, colour labels Red/Yellow/Green/Blue/
//! Purple, Flagged/Unflagged/Rejected, hierarchical keywords, manual collections and rule-based
//! smart collections, XMP metadata write. Mutations of the asset record carry the
//! expected-revision precondition at asset granularity (STU-ASSET-023); every call names the
//! acting principal and whether it is an operator or a model lane (STU-ASSET-022); a denied call is
//! `Unauthorized` and leaks no asset name or path (STU-ASSET-007/021). The XMP sidecar request is
//! "bounded explicit XMP interchange read/write through CKC ... as external transport" (14.12):
//! CKC performs the write, Studio only asks.

/// Port contract version; a host refuses a different major.
pub const CKC_PORT_VERSION: u32 = 1;

/// Bounds every request is validated against before it is sent and again by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CkcLimits {
    pub max_id_bytes: usize,
    pub max_name_bytes: usize,
    pub max_keyword_segments: usize,
    pub max_segment_bytes: usize,
    pub max_changes: usize,
    pub max_members: usize,
    pub max_synonyms: usize,
    pub max_rule_depth: usize,
    pub max_rule_nodes: usize,
    pub max_text_bytes: usize,
    pub max_sort_keys: usize,
    pub max_page: u32,
    pub max_xmp_keywords: usize,
}

impl Default for CkcLimits {
    fn default() -> Self {
        Self {
            max_id_bytes: 128,
            max_name_bytes: 1024,
            max_keyword_segments: 32,
            max_segment_bytes: 255,
            max_changes: 256,
            max_members: 10_000,
            max_synonyms: 64,
            max_rule_depth: 16,
            max_rule_nodes: 4096,
            max_text_bytes: 4096,
            max_sort_keys: 8,
            max_page: 1000,
            max_xmp_keywords: 4096,
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

fn check_id(s: &str, l: &CkcLimits) -> Result<(), CkcError> {
    if s.is_empty() || s.len() > l.max_id_bytes {
        return Err(CkcError::Limit("ckc_id_length"));
    }
    if !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')) {
        return Err(CkcError::Invalid("ckc_id_charset"));
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

/// `PRIM-Asset` handle (STU-ASSET-003/005): opaque, ASCII `[A-Za-z0-9._:-]`, bounded.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(String);

impl AssetId {
    pub fn new(id: &str, limits: &CkcLimits) -> Result<Self, CkcError> {
        check_id(id, limits)?;
        Ok(Self(id.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Catalog collection identity (manual collection, collection set or smart collection).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollectionId(String);

impl CollectionId {
    pub fn new(id: &str, limits: &CkcLimits) -> Result<Self, CkcError> {
        check_id(id, limits)?;
        Ok(Self(id.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 0-to-5 star rating (10.10.4.5): `Unrated` is 0 stars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rating {
    Unrated,
    Stars(u8),
}

impl Rating {
    pub fn from_stars(n: u8) -> Result<Self, CkcError> {
        match n {
            0 => Ok(Self::Unrated),
            1..=5 => Ok(Self::Stars(n)),
            _ => Err(CkcError::Invalid("ckc_rating_range")),
        }
    }
    pub fn stars(self) -> u8 {
        match self {
            Self::Unrated => 0,
            Self::Stars(n) => n,
        }
    }
}

/// Pick/Reject flags: Flagged, Unflagged, Rejected (10.10.4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Flag {
    Flagged,
    Unflagged,
    Rejected,
}

/// Colour labels: Red, Yellow, Green, Blue, Purple (10.10.4.5); the set is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ColorLabel {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

/// Hierarchical keyword, root first (10.10.4.5 "Hierarchical keywords").
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

/// One change to an asset's catalog metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataChange {
    SetRating(Rating),
    SetFlag(Flag),
    SetLabel(Option<ColorLabel>),
    AddKeyword(KeywordPath),
    RemoveKeyword(KeywordPath),
}

/// Asset metadata mutation (retag / rate / flag / label), applied atomically under the
/// expected-revision precondition (STU-ASSET-023). `request_id` is the idempotency key: a host
/// replays the original receipt for a repeated id instead of re-applying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataMutation {
    pub request_id: String,
    pub asset: AssetId,
    pub expected_revision: u64,
    pub changes: Vec<MetadataChange>,
}

impl MetadataMutation {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_id(&self.request_id, l)?;
        check_id(self.asset.as_str(), l)?;
        if self.changes.is_empty() {
            return Err(CkcError::Invalid("ckc_no_changes"));
        }
        if self.changes.len() > l.max_changes {
            return Err(CkcError::Limit("ckc_changes"));
        }
        Ok(())
    }
}

/// Catalog-wide keyword commands (keyword-level, not asset-level).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeywordCommand {
    DefineSynonyms { request_id: String, keyword: KeywordPath, synonyms: Vec<String> },
}

impl KeywordCommand {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        let Self::DefineSynonyms { request_id, synonyms, .. } = self;
        check_id(request_id, l)?;
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

/// Field a rule or sort refers to. Names are Studio/CKC vocabulary, not a source product's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogField {
    Rating,
    Flag,
    Label,
    Keyword,
    FileName,
    FileFormat,
    CaptureTime,
    Collection,
    Folder,
    CameraModel,
    Lens,
    Iso,
    AnyText,
    HasAdjustments,
}

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
    ContainsNone,
    BeginsWith,
    EndsWith,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogValue {
    Int(i64),
    Text(String),
    Bool(bool),
    Rating(Rating),
    Flag(Flag),
    Label(ColorLabel),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Criterion {
    pub field: CatalogField,
    pub op: CatalogOp,
    pub value: CatalogValue,
    pub value2: Option<CatalogValue>,
}

/// Rule tree of a smart collection or a search (rule-based smart collections, 10.10.4.5; a
/// search expression per STU-ASSET-010 / STU-AUT-127). Typed and bounded; never evaluated here.
#[derive(Clone, Debug, PartialEq, Eq)]
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
        Rating | Iso | CaptureTime => Kind::Ordinal,
        Flag | Label => Kind::Exact,
        HasAdjustments => Kind::Bool,
        Keyword | FileName | FileFormat | Collection | Folder | CameraModel | Lens | AnyText => Kind::Text,
    }
}

fn value_fits(f: CatalogField, v: &CatalogValue) -> bool {
    use CatalogField as F;
    use CatalogValue as V;
    matches!(
        (f, v),
        (F::Rating, V::Rating(_) | V::Int(0..=5))
            | (F::Iso, V::Int(_))
            | (F::CaptureTime, V::Text(_))
            | (F::Flag, V::Flag(_))
            | (F::Label, V::Label(_))
            | (F::HasAdjustments, V::Bool(_))
            | (
                F::Keyword | F::FileName | F::FileFormat | F::Collection | F::Folder | F::CameraModel | F::Lens | F::AnyText,
                V::Text(_)
            )
    )
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
    /// Manual grouping (10.10.4.5 "Collections").
    Manual,
    /// Container of collections; the only legal parent kind.
    Set,
}

/// Collection and smart-collection commands. A parent must be a `Set` and no parent chain may
/// cycle (`ParentNotSet`, `ParentCycle`); deletes and rule changes carry the expected revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectionCommand {
    Create { request_id: String, id: CollectionId, name: String, parent: Option<CollectionId>, kind: CollectionKind },
    CreateSmart { request_id: String, id: CollectionId, name: String, parent: Option<CollectionId>, rule: CatalogRule },
    AddMembers { request_id: String, collection: CollectionId, assets: Vec<AssetId> },
    RemoveMembers { request_id: String, collection: CollectionId, assets: Vec<AssetId> },
    SetRule { request_id: String, collection: CollectionId, expected_revision: u64, rule: CatalogRule },
    Delete { request_id: String, collection: CollectionId, expected_revision: u64 },
}

impl CollectionCommand {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Create { request_id, .. }
            | Self::CreateSmart { request_id, .. }
            | Self::AddMembers { request_id, .. }
            | Self::RemoveMembers { request_id, .. }
            | Self::SetRule { request_id, .. }
            | Self::Delete { request_id, .. } => request_id,
        }
    }

    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_id(self.request_id(), l)?;
        match self {
            Self::Create { name, .. } | Self::CreateSmart { name, .. } => {
                if name.is_empty() {
                    return Err(CkcError::Invalid("ckc_name_empty"));
                }
                check_text(name, l.max_name_bytes, "ckc_name_length")?;
            }
            Self::AddMembers { assets, .. } | Self::RemoveMembers { assets, .. }
                if assets.is_empty() || assets.len() > l.max_members =>
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

/// `asset.search` (STU-ASSET-010): optional rule, sort keys (ties broken by asset id) and an
/// offset page. A smart collection becomes a batch input as a `catalog_query` (STU-AUT-127).
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// Catalog metadata of one asset as CKC projects it (read model; Studio never edits it directly).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetMetadata {
    pub asset: AssetId,
    pub revision: u64,
    pub rating: Rating,
    pub flag: Flag,
    pub label: Option<ColorLabel>,
    pub keywords: Vec<KeywordPath>,
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
    /// Registered as a derived CKC artifact only (STU-ASSET-012/014), no file beside the original.
    CatalogArtifactOnly,
}

/// Request that CKC write an XMP sidecar for an asset. Studio supplies the fields; CKC owns the
/// serialization, the bytes and the write (STU-ASSET-019, 14.12 interchange through CKC).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmpSidecarWrite {
    pub request_id: String,
    pub asset: AssetId,
    pub expected_revision: u64,
    pub rating: Option<XmpRatingValue>,
    pub label: Option<String>,
    pub keywords: Vec<String>,
    pub hierarchical_keywords: Vec<KeywordPath>,
    pub placement: SidecarPlacement,
}

impl XmpSidecarWrite {
    pub fn validate(&self, l: &CkcLimits) -> Result<(), CkcError> {
        check_id(&self.request_id, l)?;
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

/// Receipt of an accepted command: the target, its new revision and who acted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CkcReceipt {
    pub request_id: String,
    pub target: String,
    pub revision: u64,
    pub actor: CkcActor,
}

/// The injected CKC provider port. Implementations (the Handshake host adapter; in tests the
/// feature-gated double) MUST re-validate every request, authorize the actor per call through the
/// ResourceBroker (STU-ASSET-021) and enforce the expected revision (STU-ASSET-023).
pub trait CkcCatalogPort {
    fn get(&self, actor: &CkcActor, asset: &AssetId) -> Result<AssetMetadata, CkcError>;
    fn search(&self, actor: &CkcActor, request: &SearchRequest) -> Result<SearchPage, CkcError>;
    fn mutate_metadata(&self, actor: &CkcActor, mutation: &MetadataMutation) -> Result<CkcReceipt, CkcError>;
    fn keyword(&self, actor: &CkcActor, command: &KeywordCommand) -> Result<CkcReceipt, CkcError>;
    fn collection(&self, actor: &CkcActor, command: &CollectionCommand) -> Result<CkcReceipt, CkcError>;
    fn request_xmp_sidecar(&self, actor: &CkcActor, request: &XmpSidecarWrite) -> Result<CkcReceipt, CkcError>;
}
