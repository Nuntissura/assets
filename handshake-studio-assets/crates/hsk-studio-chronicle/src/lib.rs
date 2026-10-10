//! Pure source-local property patch preparation; no history stack or authority database.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{NodeKind, NodeRef, Payload, Resolver, Snapshot, StudioDocument};
use hsk_studio_observe::{Observation, Observe, Outcome, SinkPort};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fmt,
};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub account_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub principal_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub owner_account_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub owner_principal_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub access_space_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub session_id: String,
}
impl Actor {
    pub fn validated(&self) -> std::result::Result<ActorContext, Diagnostic> {
        ActorContext::new(
            &self.account_id,
            &self.principal_id,
            &self.owner_account_id,
            &self.owner_principal_id,
            &self.access_space_id,
            &self.session_id,
        )
        .map_err(|_| failure(Code::InvalidActor, None))
    }
}
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Property {
    Name,
    Visible,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyAddress {
    pub target: NodeRef,
    pub property: Property,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "value_kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PropertyValue {
    String(String),
    Bool(bool),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyRead {
    pub address: PropertyAddress,
    pub expected_revision: u64,
    pub expected_value: PropertyValue,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyWrite {
    pub address: PropertyAddress,
    pub value: PropertyValue,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    #[schemars(extend("const"=1))]
    pub transport_version: u8,
    pub document_id: String,
    pub base_revision: u64,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub command_id: String,
    pub correlation_id: u64,
    pub actor: Actor,
    pub reads: Vec<PropertyRead>,
    #[schemars(length(min = 1))]
    pub writes: Vec<PropertyWrite>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionEntry {
    pub address: PropertyAddress,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionUpdate {
    pub address: PropertyAddress,
    pub previous_revision: u64,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inverse {
    #[schemars(extend("const"=1))]
    pub transport_version: u8,
    pub document_id: String,
    pub source_command_id: String,
    pub reads: Vec<PropertyRead>,
    pub writes: Vec<PropertyWrite>,
}
impl Inverse {
    /// Fresh attribution/command identity is supplied by the caller, never copied as approval.
    pub fn invocation(
        &self,
        command_id: String,
        correlation_id: u64,
        actor: Actor,
        base_revision: u64,
    ) -> Patch {
        Patch {
            transport_version: self.transport_version,
            document_id: self.document_id.clone(),
            base_revision,
            command_id,
            correlation_id,
            actor,
            reads: self.reads.clone(),
            writes: self.writes.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceResult {
    Changed {
        document_id: String,
        command_id: String,
        correlation_id: u64,
        actor: Actor,
        observed_current_revision: u64,
        successor: Box<StudioDocument>,
        revision_updates: Vec<RevisionUpdate>,
        inverse: Inverse,
    },
    NoChange {
        document_id: String,
        command_id: String,
        correlation_id: u64,
        actor: Actor,
        observed_current_revision: u64,
    },
}
#[derive(Clone, Debug)]
pub struct Prepared {
    wire: SourceResult,
    snapshot: Option<Snapshot>,
}
impl Prepared {
    pub fn wire(&self) -> &SourceResult {
        &self.wire
    }
    pub fn successor(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }
    pub fn revision_updates(&self) -> &[RevisionUpdate] {
        match &self.wire {
            SourceResult::Changed {
                revision_updates, ..
            } => revision_updates,
            SourceResult::NoChange { .. } => &[],
        }
    }
    pub fn inverse(&self) -> Option<&Inverse> {
        match &self.wire {
            SourceResult::Changed { inverse, .. } => Some(inverse),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    InvalidInput,
    UnsupportedVersion,
    DuplicateField,
    InvalidActor,
    InvalidCommand,
    WrongDocument,
    InvalidTarget,
    WrongType,
    DuplicateAddress,
    MissingRead,
    RevisionUnavailable,
    RevisionConflict,
    ValueConflict,
    RevisionOverflow,
    BudgetExceeded,
    Canceled,
    FolioRejected,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub address: Option<PropertyAddress>,
}
fn failure(code: Code, address: Option<&PropertyAddress>) -> Diagnostic {
    Diagnostic {
        code,
        address: address.cloned(),
    }
}
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub input_bytes: usize,
    pub reads: usize,
    pub writes: usize,
    pub revisions: usize,
    pub value_bytes: usize,
    pub folio: hsk_studio_folio::Budget,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            input_bytes: 262_144,
            reads: 1024,
            writes: 1024,
            revisions: 8192,
            value_bytes: 65_536,
            folio: hsk_studio_folio::Budget::default(),
        }
    }
}
fn check(t: &CancellationToken) -> Result<()> {
    t.check().map_err(|_| failure(Code::Canceled, None))
}
fn addr_key(a: &PropertyAddress) -> (String, Property) {
    (a.target.node_id.clone(), a.property)
}
fn address_valid(a: &PropertyAddress) -> Result<()> {
    let prefix = match a.target.node_kind {
        NodeKind::Layer => "SLYR",
        NodeKind::Artboard => "SART",
        NodeKind::PageSpread => "SPGS",
    };
    if !DomainId::parse(&a.target.node_id).is_ok_and(|id| id.prefix() == prefix) {
        return Err(failure(Code::InvalidTarget, None));
    }
    if a.property == Property::Visible && a.target.node_kind != NodeKind::Layer {
        return Err(failure(Code::WrongType, Some(a)));
    }
    Ok(())
}
enum ValueRef<'a> {
    String(&'a str),
    Bool(bool),
}
impl ValueRef<'_> {
    fn equals(&self, v: &PropertyValue) -> bool {
        match (self, v) {
            (Self::String(a), PropertyValue::String(b)) => *a == b.as_str(),
            (Self::Bool(a), PropertyValue::Bool(b)) => a == b,
            _ => false,
        }
    }
    fn owned(&self) -> PropertyValue {
        match self {
            Self::String(s) => PropertyValue::String((*s).into()),
            Self::Bool(v) => PropertyValue::Bool(*v),
        }
    }
}
fn value<'a>(d: &'a StudioDocument, a: &PropertyAddress) -> Result<ValueRef<'a>> {
    address_valid(a)?;
    match a.target.node_kind {
        NodeKind::Layer => {
            let l = d
                .layers
                .iter()
                .find(|l| l.layer_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            if matches!(l.payload, Payload::Unsupported { .. }) {
                return Err(failure(Code::InvalidTarget, Some(a)));
            }
            match a.property {
                Property::Name => Ok(ValueRef::String(&l.name)),
                Property::Visible => Ok(ValueRef::Bool(l.visible)),
            }
        }
        NodeKind::Artboard => {
            let l = d
                .artboards
                .iter()
                .find(|l| l.artboard_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            Ok(ValueRef::String(&l.name))
        }
        NodeKind::PageSpread => {
            let l = d
                .page_spreads
                .iter()
                .find(|l| l.page_spread_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            Ok(ValueRef::String(&l.name))
        }
    }
}
fn value_type(a: &PropertyAddress, v: &PropertyValue) -> Result<()> {
    if !matches!(
        (a.property, v),
        (Property::Name, PropertyValue::String(_)) | (Property::Visible, PropertyValue::Bool(_))
    ) {
        return Err(failure(Code::WrongType, Some(a)));
    }
    Ok(())
}
fn size(v: &PropertyValue) -> usize {
    match v {
        PropertyValue::String(s) => s.len(),
        PropertyValue::Bool(_) => 0,
    }
}
fn set(d: &mut StudioDocument, a: &PropertyAddress, v: &PropertyValue) -> Result<()> {
    value_type(a, v)?;
    match a.target.node_kind {
        NodeKind::Layer => {
            let l = d
                .layers
                .iter_mut()
                .find(|l| l.layer_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            match v {
                PropertyValue::String(v) => l.name = v.clone(),
                PropertyValue::Bool(v) => l.visible = *v,
            }
        }
        NodeKind::Artboard => {
            let l = d
                .artboards
                .iter_mut()
                .find(|l| l.artboard_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            if let PropertyValue::String(v) = v {
                l.name = v.clone();
            }
        }
        NodeKind::PageSpread => {
            let l = d
                .page_spreads
                .iter_mut()
                .find(|l| l.page_spread_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            if let PropertyValue::String(v) = v {
                l.name = v.clone();
            }
        }
    }
    Ok(())
}
fn snapshot_budget(s: &Snapshot, b: hsk_studio_folio::Budget, t: &CancellationToken) -> Result<()> {
    check(t)?;
    let d = s.document();
    if s.encoded_bytes().len() > b.input_bytes
        || d.artboards
            .len()
            .checked_add(d.page_spreads.len())
            .and_then(|n| n.checked_add(d.layers.len()))
            .and_then(|n| n.checked_add(d.graph.operations.len()))
            .is_none_or(|n| n > b.nodes)
        || d.graph.edges.len() > b.edges
        || b.depth == 0
        || b.depth > 64
    {
        return Err(failure(Code::BudgetExceeded, None));
    }
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in s.encoded_bytes() {
        check(t)?;
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > b.depth {
                        return Err(failure(Code::BudgetExceeded, None));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let mut bytes = 0usize;
    for l in &d.layers {
        check(t)?;
        let mut add = |n: usize| -> Result<()> {
            bytes = bytes
                .checked_add(n)
                .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
            if bytes > b.payload_bytes {
                return Err(failure(Code::BudgetExceeded, None));
            }
            Ok(())
        };
        match &l.payload {
            Payload::Group => {}
            Payload::Raster { tiles } => {
                for tile in tiles {
                    check(t)?;
                    for text in [
                        &tile.object_key,
                        &tile.layer_id,
                        &tile.format,
                        &tile.colour_profile_id,
                        &tile.content_digest.algorithm,
                        &tile.content_digest.digest,
                        &tile.artifact_manifest_id,
                    ] {
                        add(text.len())?;
                    }
                    add(128)?;
                }
            }
            Payload::Vector { path_ids } => {
                for id in path_ids {
                    check(t)?;
                    add(id.len())?;
                }
            }
            Payload::Text { story_id } => add(story_id.len())?,
            Payload::PrimitiveReference {
                schema_id,
                object_id,
            } => {
                add(schema_id.len())?;
                add(object_id.len())?;
            }
            Payload::Unsupported {
                kind,
                schema_id,
                encoded_bytes,
                reason,
            } => {
                add(kind.len())?;
                add(schema_id.len())?;
                add(encoded_bytes.len())?;
                add(reason.len())?;
            }
        }
    }
    Ok(())
}
struct ByteCounter<'a> {
    bytes: usize,
    token: &'a CancellationToken,
    canceled: bool,
}
impl std::io::Write for ByteCounter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.token.check().is_err() {
            self.canceled = true;
            return Err(std::io::Error::other("canceled"));
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encoded_size<T: Serialize + ?Sized>(v: &T, t: &CancellationToken) -> Result<usize> {
    let mut counter = ByteCounter {
        bytes: 0,
        token: t,
        canceled: false,
    };
    serde_json::to_writer(&mut counter, v).map_err(|_| {
        failure(
            if counter.canceled {
                Code::Canceled
            } else {
                Code::BudgetExceeded
            },
            None,
        )
    })?;
    Ok(counter.bytes)
}
fn scalar_size(v: &PropertyValue, t: &CancellationToken) -> Result<usize> {
    match v {
        PropertyValue::String(s) => encoded_size(s, t),
        PropertyValue::Bool(v) => encoded_size(v, t),
    }
}
/// Stateless preparation. The caller serializes a fresh current read plus publication.
/// base_revision is provenance only; only declared current property reads gate this patch.
pub fn prepare(
    p: &Patch,
    current: &Snapshot,
    revisions: &[RevisionEntry],
    b: Budget,
    t: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Prepared> {
    check(t)?;
    if p.transport_version != 1 {
        return Err(failure(Code::UnsupportedVersion, None));
    }
    if p.command_id.is_empty()
        || p.command_id.len() > 128
        || p.command_id.chars().any(char::is_control)
    {
        return Err(failure(Code::InvalidCommand, None));
    }
    p.actor.validated()?;
    if !DomainId::parse(&p.document_id).is_ok_and(|id| id.prefix() == "SDOC")
        || p.document_id != current.document().document_id
    {
        return Err(failure(Code::WrongDocument, None));
    }
    if p.reads.len() > b.reads
        || p.writes.len() > b.writes
        || revisions.len() > b.revisions
        || current.encoded_bytes().len() > b.folio.input_bytes
    {
        return Err(failure(Code::BudgetExceeded, None));
    }
    if encoded_size(p, t)? > b.input_bytes {
        return Err(failure(Code::BudgetExceeded, None));
    }
    snapshot_budget(current, b.folio, t)?;
    if p.writes.is_empty() {
        return Err(failure(Code::InvalidInput, None));
    }
    let mut bytes = 0usize;
    for v in p
        .reads
        .iter()
        .map(|r| &r.expected_value)
        .chain(p.writes.iter().map(|w| &w.value))
    {
        check(t)?;
        bytes = bytes
            .checked_add(size(v))
            .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
        if bytes > b.value_bytes {
            return Err(failure(Code::BudgetExceeded, None));
        }
    }
    let mut vector = BTreeMap::new();
    for e in revisions {
        check(t)?;
        address_valid(&e.address)?;
        if vector.insert(addr_key(&e.address), e.revision).is_some() {
            return Err(failure(Code::DuplicateAddress, Some(&e.address)));
        }
    }
    let mut reads = BTreeMap::new();
    for read in &p.reads {
        check(t)?;
        address_valid(&read.address)?;
        value_type(&read.address, &read.expected_value)?;
        let key = addr_key(&read.address);
        if reads.insert(key.clone(), read).is_some() {
            return Err(failure(Code::DuplicateAddress, Some(&read.address)));
        }
        let actual = value(current.document(), &read.address)?;
        let revision = vector
            .get(&key)
            .ok_or_else(|| failure(Code::RevisionUnavailable, Some(&read.address)))?;
        if *revision != read.expected_revision {
            return Err(failure(Code::RevisionConflict, Some(&read.address)));
        }
        if !actual.equals(&read.expected_value) {
            return Err(failure(Code::ValueConflict, Some(&read.address)));
        }
    }
    let mut write_keys = BTreeSet::new();
    let mut changed = Vec::new();
    let mut updates = Vec::new();
    let mut inverse_writes = Vec::new();
    for write in &p.writes {
        check(t)?;
        address_valid(&write.address)?;
        value_type(&write.address, &write.value)?;
        let key = addr_key(&write.address);
        if !write_keys.insert(key.clone()) {
            return Err(failure(Code::DuplicateAddress, Some(&write.address)));
        }
        if !reads.contains_key(&key) {
            return Err(failure(Code::MissingRead, Some(&write.address)));
        }
        let actual = value(current.document(), &write.address)?;
        if !actual.equals(&write.value) {
            let previous_revision = *vector
                .get(&key)
                .ok_or_else(|| failure(Code::RevisionUnavailable, Some(&write.address)))?;
            let revision = previous_revision
                .checked_add(1)
                .ok_or_else(|| failure(Code::RevisionOverflow, Some(&write.address)))?;
            updates.push(RevisionUpdate {
                address: write.address.clone(),
                previous_revision,
                revision,
            });
            inverse_writes.push(PropertyWrite {
                address: write.address.clone(),
                value: actual.owned(),
            });
            changed.push(write);
        }
    }
    if changed.is_empty() {
        check(t)?;
        return Ok(Prepared {
            wire: SourceResult::NoChange {
                document_id: p.document_id.clone(),
                command_id: p.command_id.clone(),
                correlation_id: p.correlation_id,
                actor: p.actor.clone(),
                observed_current_revision: current.document().revision,
            },
            snapshot: None,
        });
    }
    let next = current
        .document()
        .revision
        .checked_add(1)
        .ok_or_else(|| failure(Code::RevisionOverflow, None))?;
    // Count the canonical prospective serialization without owning a second document.
    let mut prospective_bytes = encoded_size(current.document(), t)?;
    prospective_bytes = prospective_bytes
        .checked_sub(encoded_size(&current.document().revision, t)?)
        .and_then(|n| n.checked_add(next.to_string().len()))
        .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
    for write in &changed {
        check(t)?;
        let old = &reads
            .get(&addr_key(&write.address))
            .ok_or_else(|| failure(Code::MissingRead, Some(&write.address)))?
            .expected_value;
        let old_bytes = scalar_size(old, t)?;
        let new_bytes = scalar_size(&write.value, t)?;
        prospective_bytes = prospective_bytes
            .checked_sub(old_bytes)
            .and_then(|n| n.checked_add(new_bytes))
            .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
    }
    if prospective_bytes > b.folio.input_bytes {
        return Err(failure(Code::BudgetExceeded, None));
    }
    let mut d = current.document().clone();
    for write in changed {
        check(t)?;
        set(&mut d, &write.address, &write.value)?;
    }
    d.revision = next;
    let snapshot = hsk_studio_folio::validate_document(d, b.folio, t, resolver).map_err(|e| {
        failure(
            match e.code {
                hsk_studio_folio::Code::Canceled => Code::Canceled,
                hsk_studio_folio::Code::BudgetExceeded => Code::BudgetExceeded,
                _ => Code::FolioRejected,
            },
            None,
        )
    })?;
    let mut inverse_reads = p.reads.clone();
    for update in &updates {
        check(t)?;
        let read = inverse_reads
            .iter_mut()
            .find(|r| r.address == update.address)
            .ok_or_else(|| failure(Code::MissingRead, Some(&update.address)))?;
        read.expected_revision = update.revision;
        read.expected_value = value(snapshot.document(), &update.address)?.owned();
    }
    let inverse = Inverse {
        transport_version: 1,
        document_id: p.document_id.clone(),
        source_command_id: p.command_id.clone(),
        reads: inverse_reads,
        writes: inverse_writes,
    };
    check(t)?;
    // The wire projects the already validated canonical document; bound this ownership clone too.
    snapshot_budget(&snapshot, b.folio, t)?;
    let wire = SourceResult::Changed {
        document_id: p.document_id.clone(),
        command_id: p.command_id.clone(),
        correlation_id: p.correlation_id,
        actor: p.actor.clone(),
        observed_current_revision: current.document().revision,
        successor: Box::new(snapshot.document().clone()),
        revision_updates: updates,
        inverse,
    };
    check(t)?;
    Ok(Prepared {
        wire,
        snapshot: Some(snapshot),
    })
}
/// Delivery failure stays separate from already returned preparation/acceptance state.
#[derive(Clone, Copy, Debug)]
pub struct DeliveryReport {
    pub receipt: std::result::Result<hsk_studio_observe::Receipt, hsk_studio_observe::Error>,
    pub state: hsk_studio_observe::State,
}
pub fn deliver(
    p: &Patch,
    revision: u64,
    outcome: Outcome,
    sink: &mut impl SinkPort,
    t: &CancellationToken,
) -> Result<DeliveryReport> {
    let resource =
        DomainId::parse(&p.document_id).map_err(|_| failure(Code::WrongDocument, None))?;
    if resource.prefix() != "SDOC" {
        return Err(failure(Code::WrongDocument, None));
    }
    let mut observe = Observe::new(
        p.correlation_id,
        revision,
        resource,
        p.actor.validated()?,
        hsk_studio_observe::Budget::new(0, 0).map_err(|_| failure(Code::InvalidInput, None))?,
    );
    let receipt = observe.emit(
        Observation {
            correlation_id: p.correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        t,
        sink,
    );
    Ok(DeliveryReport {
        receipt,
        state: observe.state(),
    })
}
/// Adapt the actual preparation result to the existing Observe sink. Delivery is separate
/// from source acceptance; the caller retains the supplied result even on delivery failure.
pub fn deliver_result(
    p: &Patch,
    revision: u64,
    result: std::result::Result<&Prepared, &Diagnostic>,
    budget: Budget,
    sink: &mut impl SinkPort,
    token: &CancellationToken,
) -> Result<DeliveryReport> {
    use hsk_studio_observe::{
        DiagnosticAddress, DiagnosticCode, DiagnosticDetail, DiagnosticProperty, DiagnosticTarget,
        Disposition, FailureCode, MAX_DETAIL_COUNT,
    };
    let resource =
        DomainId::parse(&p.document_id).map_err(|_| failure(Code::WrongDocument, None))?;
    if resource.prefix() != "SDOC" {
        return Err(failure(Code::WrongDocument, None));
    }
    let actor = p.actor.validated()?;
    let capped = |n: usize| n.min(MAX_DETAIL_COUNT as usize) as u32;
    let mut counts = [capped(p.reads.len()), capped(p.writes.len()), 0, 0, 0];
    let (disposition, code, address, outcome) = match result {
        Ok(prepared) => {
            if p.reads.len() > budget.reads || p.writes.len() > budget.writes {
                return Err(failure(Code::BudgetExceeded, None));
            }
            let (document_id, command_id, correlation_id, result_actor, changed) =
                match prepared.wire() {
                    SourceResult::Changed {
                        document_id,
                        command_id,
                        correlation_id,
                        actor,
                        ..
                    } => (document_id, command_id, correlation_id, actor, true),
                    SourceResult::NoChange {
                        document_id,
                        command_id,
                        correlation_id,
                        actor,
                        ..
                    } => (document_id, command_id, correlation_id, actor, false),
                };
            if document_id != &p.document_id
                || command_id != &p.command_id
                || *correlation_id != p.correlation_id
                || result_actor != &p.actor
            {
                return Err(failure(Code::InvalidInput, None));
            }
            counts[2] = counts[0];
            counts[3] = counts[1];
            counts[4] = capped(prepared.revision_updates().len());
            (
                if changed {
                    Disposition::Changed
                } else {
                    Disposition::NoChange
                },
                None,
                None,
                Outcome::Success,
            )
        }
        Err(error) => {
            let code = match error.code {
                Code::InvalidInput => DiagnosticCode::InvalidInput,
                Code::UnsupportedVersion => DiagnosticCode::UnsupportedVersion,
                Code::DuplicateField => DiagnosticCode::DuplicateField,
                Code::InvalidActor => DiagnosticCode::InvalidActor,
                Code::InvalidCommand => DiagnosticCode::InvalidCommand,
                Code::WrongDocument => DiagnosticCode::WrongDocument,
                Code::InvalidTarget => DiagnosticCode::InvalidTarget,
                Code::WrongType => DiagnosticCode::WrongType,
                Code::DuplicateAddress => DiagnosticCode::DuplicateAddress,
                Code::MissingRead => DiagnosticCode::MissingRead,
                Code::RevisionUnavailable => DiagnosticCode::RevisionUnavailable,
                Code::RevisionConflict => DiagnosticCode::RevisionConflict,
                Code::ValueConflict => DiagnosticCode::ValueConflict,
                Code::RevisionOverflow => DiagnosticCode::RevisionOverflow,
                Code::BudgetExceeded => DiagnosticCode::BudgetExceeded,
                Code::Canceled => DiagnosticCode::Canceled,
                Code::FolioRejected => DiagnosticCode::ValidationRejected,
            };
            // A diagnostic supplied by a caller cannot serialize an unvalidated ID/property.
            let address = error
                .address
                .as_ref()
                .filter(|a| address_valid(a).is_ok())
                .map(|a| {
                    let target = match a.target.node_kind {
                        NodeKind::Layer => DiagnosticTarget::Layer,
                        NodeKind::Artboard => DiagnosticTarget::Artboard,
                        NodeKind::PageSpread => DiagnosticTarget::PageSpread,
                    };
                    let property = match a.property {
                        Property::Name => DiagnosticProperty::Name,
                        Property::Visible => DiagnosticProperty::Visible,
                    };
                    DiagnosticAddress::new(
                        target,
                        property,
                        DomainId::parse(&a.target.node_id).expect("validated address"),
                    )
                })
                .transpose()
                .map_err(|_| failure(Code::InvalidTarget, None))?;
            (
                Disposition::Rejected,
                Some(code),
                address,
                if error.code == Code::Canceled {
                    Outcome::Canceled
                } else {
                    Outcome::Failure(FailureCode::Validation)
                },
            )
        }
    };
    let detail = DiagnosticDetail::new(disposition, code, address, counts)
        .map_err(|_| failure(Code::InvalidInput, None))?;
    let mut observe = Observe::new(
        p.correlation_id,
        revision,
        resource,
        actor,
        hsk_studio_observe::Budget::new(0, 0).map_err(|_| failure(Code::InvalidInput, None))?,
    );
    let receipt = observe.emit_detail(
        Observation {
            correlation_id: p.correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        &detail,
        token,
        sink,
    );
    Ok(DeliveryReport {
        receipt,
        state: observe.state(),
    })
}
// Bounded unique-key decoding checks array counts before pushing, byte counts before cloning,
// and depth/cancellation at every visited JSON value. Value is temporary transport, never authority.
struct DecodeContext<'a> {
    budget: Budget,
    token: &'a CancellationToken,
    value_bytes: Cell<usize>,
}
struct RejectElement;
impl<'de> DeserializeSeed<'de> for RejectElement {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, _: D) -> std::result::Result<(), D::Error> {
        Err(de::Error::custom("budget"))
    }
}
struct Seed<'a, 'b> {
    context: &'a DecodeContext<'b>,
    depth: usize,
    array_limit: usize,
    value_text: bool,
}
impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Value, D::Error> {
        if self.depth > 32 {
            return Err(de::Error::custom("budget"));
        }
        if self.context.token.check().is_err() {
            return Err(de::Error::custom("canceled"));
        }
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded unique-key JSON")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid number"))
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Value, E> {
        if self.value_text {
            let n = self
                .context
                .value_bytes
                .get()
                .checked_add(v.len())
                .ok_or_else(|| E::custom("budget"))?;
            if n > self.context.budget.value_bytes {
                return Err(E::custom("budget"));
            }
            self.context.value_bytes.set(n);
        }
        Ok(v.into())
    }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Value, E> {
        self.visit_str(&v)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
        let mut out = Vec::new();
        loop {
            if out.len() >= self.array_limit {
                let _ = a.next_element_seed(RejectElement)?;
                break;
            }
            let next = a.next_element_seed(Seed {
                context: self.context,
                depth: self.depth + 1,
                array_limit: 16,
                value_text: false,
            })?;
            let Some(v) = next else {
                break;
            };
            if out.len() >= self.array_limit {
                return Err(de::Error::custom("budget"));
            }
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
        let mut out = serde_json::Map::new();
        while let Some(k) = a.next_key::<String>()? {
            if out.contains_key(&k) {
                return Err(de::Error::custom("duplicate_field"));
            }
            if out.len() >= 32 {
                return Err(de::Error::custom("budget"));
            }
            let limit = match k.as_str() {
                "reads" => self.context.budget.reads,
                "writes" => self.context.budget.writes,
                _ => 16,
            };
            let v = a.next_value_seed(Seed {
                context: self.context,
                depth: self.depth + 1,
                array_limit: limit,
                value_text: k == "value",
            })?;
            out.insert(k, v);
        }
        Ok(Value::Object(out))
    }
}
fn decode_value(
    input: &[u8],
    b: Budget,
    t: &CancellationToken,
    array_limit: usize,
) -> Result<Value> {
    check(t)?;
    if input.len() > b.input_bytes {
        return Err(failure(Code::BudgetExceeded, None));
    }
    let context = DecodeContext {
        budget: b,
        token: t,
        value_bytes: Cell::new(0),
    };
    let mut decoder = serde_json::Deserializer::from_slice(input);
    let value = Seed {
        context: &context,
        depth: 0,
        array_limit,
        value_text: false,
    }
    .deserialize(&mut decoder)
    .map_err(|e| {
        failure(
            if e.to_string().contains("duplicate_field") {
                Code::DuplicateField
            } else if e.to_string().contains("canceled") {
                Code::Canceled
            } else if e.to_string().contains("budget") {
                Code::BudgetExceeded
            } else {
                Code::InvalidInput
            },
            None,
        )
    })?;
    decoder
        .end()
        .map_err(|_| failure(Code::InvalidInput, None))?;
    check(t)?;
    Ok(value)
}
pub fn decode_patch(input: &[u8], b: Budget, t: &CancellationToken) -> Result<Patch> {
    let v = decode_value(input, b, t, 16)?;
    if v.get("transport_version").and_then(Value::as_u64) != Some(1) {
        return Err(failure(Code::UnsupportedVersion, None));
    }
    serde_json::from_value(v).map_err(|_| failure(Code::InvalidInput, None))
}
pub fn decode_revisions(
    input: &[u8],
    b: Budget,
    t: &CancellationToken,
) -> Result<Vec<RevisionEntry>> {
    serde_json::from_value(decode_value(input, b, t, b.revisions)?)
        .map_err(|_| failure(Code::InvalidInput, None))
}
pub fn patch_schema() -> schemars::Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<Patch>()
}
pub fn result_schema() -> schemars::Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<SourceResult>()
}
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-CHRONICLE","version":1,"scope":"Pure CON022-026 source-local patch preparation; no persisted StudioEditProposal/HistoryEntry, approval, EventLedger, CRDT, retained history or host acceptance","api":"decode_patch / decode_revisions / prepare / Inverse::invocation / deliver_result / legacy deliver","consumer":"chronicle-consumer --document FILE --revisions FILE --patch FILE [--patch FILE ...] [--inverse] [--cancel-before] [--cancel-after] [--delivery delivered|rejected|indeterminate] [--max-bytes N --max-reads N --max-writes N --max-value-bytes N]; --schema; --result-schema; --descriptor","input":"Explicit version1 actor/doc/base provenance, reads+writes/name or visible; caller-owned current revision entries; immutable Folio document; budget/cancellation/resolver","result":"changed with actual canonical successor/revision_updates/conditional inverse, or closed metadata-only no_change; typed failure with stable address and conflict dimension","recovery":"Refresh current snapshot and property vector; re-evaluate conflict against fresh current state; missing entries unavailable, no default zero; correct type/reference; invert with fresh attribution only when conditional reads still hold","parallel":"Caller serializes latest read and local publication; disjoint old-base footprints succeed; global successor revision is bookkeeping, not CAS; no private queue/database","manual":"Same typed transport and generated draft2020-12 schema shared by model/API/operator","argus":{"inspect":"actual snapshot, property updates, inverse and typed diagnostic address","steer":"same bounded prepare API","state":"explicit changed/no_change/failure plus delivery counters","capture":"caller captures protected actual output without focus stealing"},"diagnostics":"Observe emit_detail shared closed version2 stable code/validated conflict address/source disposition and capped attempted/admitted/changed counters; result retained independently of delivery; names/string values/raw input never telemetry; missing/indeterminate delivery cannot undo accepted source result","host":"Pending actual grants, serialized SurrealDB/EventLedger promotion, idempotency/restart, history cursor, native GUI and three diagnostic tiers"}"#;
