use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub workspace: String,
    pub actor: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Host must validate the authenticated actor and current grants on every call.
pub trait Authorization {
    fn allows(&self, context: &Context, access: Access) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Customer,
    Organization,
    SupportCase,
    Interaction,
    Product,
    Distributor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseStatus {
    New,
    Assigned,
    Pending,
    Closed,
    Rejected,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    Text(String),
    Integer(#[serde(with = "decimal_integer")] i64),
    Boolean(bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Text,
    Integer,
    Boolean,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDefinition {
    pub key: String,
    pub label: String,
    pub applies_to: Vec<Kind>,
    pub value_type: FieldType,
}

/// CRM-owned customer and descriptive catalogue records. Stock, orders and identity use ExternalRef.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub fields: BTreeMap<String, Value>,
    pub case_status: Option<CaseStatus>,
    pub client_terms: Option<crate::ClientTerms>,
    pub prices: Option<crate::PriceBook>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRef {
    pub owner: String,
    pub resource_type: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    Record(String),
    External(ExternalRef),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub source: String,
    pub label: String,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// revision must be zero; service assigns revision one.
    Create(Record),
    /// record.revision is the expected current revision.
    Update(Record),
    DefineField(FieldDefinition),
    DefinePricingTier(crate::PricingTier),
    Link(Relation),
    Unlink(Relation),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation_id: String,
    pub command: Command,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub workspace_revision: u64,
    pub record_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub actor: String,
    pub request: Request,
    pub outcome: Outcome,
}

/// Atomic persistent unit. Store implementations must reject stale compare-exchange.
/// Receipts and record changes commit together. Never prune receipts silently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub records: BTreeMap<String, Record>,
    pub fields: BTreeMap<String, FieldDefinition>,
    pub relations: Vec<Relation>,
    pub receipts: BTreeMap<String, Receipt>,
    pub pricing_tiers: BTreeMap<String, crate::PricingTier>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema_version: 2,
            revision: 0,
            records: Default::default(),
            fields: Default::default(),
            relations: Default::default(),
            receipts: Default::default(),
            pricing_tiers: Default::default(),
        }
    }
}

pub trait Store {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error>;
    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Denied,
    Invalid(&'static str),
    NotFound,
    Conflict,
    OperationReused,
    Capacity(&'static str),
    Storage(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug)]
pub struct Limits {
    pub records: usize,
    pub relations: usize,
    pub fields: usize,
    pub operations: usize,
    pub snapshot_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            records: 10_000,
            relations: 50_000,
            fields: 128,
            operations: 20_000,
            snapshot_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub record: Record,
    pub distance: usize,
}

/// Query order is (edit distance, record ID); cursors are fenced by workspace revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub workspace: String,
    pub query: String,
    pub tag: Option<String>,
    pub fields: BTreeMap<String, Value>,
    pub kind: Option<Kind>,
    pub revision: u64,
    pub distance: usize,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub hits: Vec<SearchHit>,
    pub next: Option<Cursor>,
    pub total: usize,
    pub revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchFilter {
    pub query: String,
    pub tag: Option<String>,
    pub fields: BTreeMap<String, Value>,
    pub kind: Option<Kind>,
}

pub(crate) mod decimal_integer {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        let text = String::deserialize(deserializer)?;
        let value: i64 = text.parse().map_err(serde::de::Error::custom)?;
        if value.to_string() != text {
            return Err(serde::de::Error::custom("noncanonical integer"));
        }
        Ok(value)
    }
}
