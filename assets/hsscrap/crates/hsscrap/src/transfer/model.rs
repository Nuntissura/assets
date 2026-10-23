use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MODEL_VERSION: &str = "hsscrap.transfer.v1";

/// Type tags prevent coincident foreign UUIDs from resolving through another owner's mapping.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum OriginRef {
    Item(ItemId),
    Capture(CaptureId),
    Identity(IdentityId),
    Observation(ObservationId),
    Stream(StreamId),
    Member(MemberId),
    Binding(BindingId),
    Assignment(AssignmentId),
    Membership(MembershipId),
    Placement(PlacementId),
    Operation(OperationId),
    Tag(TagRef),
    Project(ProjectRef),
    Folder(FolderRef),
    Media(MediaRef),
    Occurrence(OccurrenceRef),
    Remote(HostRef),
    Host(HostRef),
    Scope(ScopeRef),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TransferRecord {
    Item(Item),
    Capture(URLCapture),
    Identity(SourceIdentity),
    Observation(MetadataObservation),
    Stream(ObservationStream),
    Member(GalleryMember),
    Remote(RemoteMediaReference),
    Binding(RetainedMediaBinding),
    Assignment(TagAssignment),
    Membership(ProjectMembership),
    Placement(FolderPlacement),
    Occurrence(OccurrenceReference),
}
impl TransferRecord {
    pub fn origin(&self) -> OriginRef {
        match self {
            Self::Item(v) => OriginRef::Item(v.item_id),
            Self::Capture(v) => OriginRef::Capture(v.capture_id),
            Self::Identity(v) => OriginRef::Identity(v.identity_id),
            Self::Observation(v) => OriginRef::Observation(v.observation_id),
            Self::Stream(v) => OriginRef::Stream(v.stream_id),
            Self::Member(v) => OriginRef::Member(v.member_id),
            Self::Remote(v) => OriginRef::Remote(v.reference_id.clone()),
            Self::Binding(v) => OriginRef::Binding(v.binding_id),
            Self::Assignment(v) => OriginRef::Assignment(v.assignment_id),
            Self::Membership(v) => OriginRef::Membership(v.membership_id),
            Self::Placement(v) => OriginRef::Placement(v.placement_id),
            Self::Occurrence(v) => OriginRef::Occurrence(v.occurrence_ref.clone()),
        }
    }
    pub fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        b.validate()?;
        preflight_record(self, b, &mut 0)?;
        match self {
            Self::Item(v) => v.validate(b, s),
            Self::Capture(v) => v.validate(b, s),
            Self::Identity(v) => v.validate(b, s),
            Self::Observation(v) => v.validate(b, s),
            Self::Stream(v) => v.validate(b, s),
            Self::Member(v) => v.validate(b, s),
            Self::Remote(v) => v.validate(b, s),
            Self::Binding(v) => v.validate(b, s),
            Self::Assignment(v) => v.validate(b, s),
            Self::Membership(v) => v.validate(b, s),
            Self::Placement(v) => v.validate(b, s),
            Self::Occurrence(v) => v.validate(b, s),
        }
    }
}

/// Exactly the hashed fields: explicit nulls belong to the contained typed record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferFields {
    pub model_version: String,
    pub export_stream_id: ExportStreamId,
    pub origin_instance_id: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub revision: Revision,
    pub projection_policy_id: String,
    pub audience_ref: ScopeRef,
    pub record: TransferRecord,
    #[serde(deserialize_with = "unique_transfer_features")]
    pub required_features: BTreeSet<String>,
    pub extensions: BTreeMap<String, serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferProjection {
    pub fields: TransferFields,
    pub payload_hash: String,
}

/// Host-persisted together with its exported projection; do not reset after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportCursor {
    pub export_stream_id: ExportStreamId,
    pub origin_instance_id: String,
    pub origin: OriginRef,
    pub projection_policy_id: String,
    pub audience_ref: ScopeRef,
    pub revision: Revision,
    pub content_hash: String,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ExportPlan {
    pub projection: TransferProjection,
    pub cursor: ExportCursor,
}

pub(crate) fn jcs<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    handshake_canonical_json::jcs_bytes(value).map_err(|e| invalid("transfer_jcs", &e.to_string()))
}
fn unique_transfer_features<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<BTreeSet<String>, D::Error> {
    let values = Vec::<String>::deserialize(decoder)?;
    let count = values.len();
    let values: BTreeSet<_> = values.into_iter().collect();
    if values.len() != count {
        return Err(serde::de::Error::custom("duplicate required feature"));
    }
    Ok(values)
}
fn sort_set(value: &mut serde_json::Value) -> Result<()> {
    if let Some(values) = value.as_array_mut() {
        let mut keyed: Vec<_> = values
            .iter()
            .map(|v| Ok((jcs(v)?, v.clone())))
            .collect::<Result<_>>()?;
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        *values = keyed.into_iter().map(|(_, v)| v).collect();
    }
    Ok(())
}
/// Trusted, already validated field bytes. Model sets sort by JCS bytes; semantic arrays retain order.
/// Untrusted callers use decode_projection/plan_export/plan_import, which enforce aggregate bounds.
pub fn canonical_fields_bytes(fields: &TransferFields) -> Result<Vec<u8>> {
    let mut value =
        serde_json::to_value(fields).map_err(|e| invalid("transfer", &e.to_string()))?;
    normalize_sets(&mut value)?;
    jcs(&value)
}
fn normalize_sets(value: &mut serde_json::Value) -> Result<()> {
    if let Some(features) = value.get_mut("required_features") {
        sort_set(features)?;
    }
    let Some(record) = value.get_mut("record").and_then(|v| v.get_mut("value")) else {
        return Ok(());
    };
    if let Some(features) = record.get_mut("required_features") {
        sort_set(features)?;
    }
    if let Some(members) = record
        .get_mut("gallery_snapshot")
        .and_then(|v| v.get_mut("members"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for member in members {
            if let Some(features) = member.get_mut("required_features") {
                sort_set(features)?;
            }
        }
    }
    if let Some(layout) = record
        .get_mut("presentation")
        .and_then(|v| v.get_mut("layout"))
    {
        if let Some(features) = layout.get_mut("required_features") {
            sort_set(features)?;
        }
    }
    Ok(())
}
pub fn decode_projection(
    input: &[u8],
    b: &Bounds,
    s: &BTreeSet<String>,
) -> Result<TransferProjection> {
    b.validate()?;
    handshake_canonical_json::jcs_from_slice(input, b.max_operation_bytes)
        .map_err(|e| invalid("transfer_json", &e.to_string()))?;
    let raw: serde_json::Value =
        serde_json::from_slice(input).map_err(|e| invalid("transfer_json", &e.to_string()))?;
    let raw_fields = raw
        .get("fields")
        .ok_or_else(|| invalid("transfer_fields", "required"))?;
    bounded_value(raw_fields, b, 0, &mut 0)?;
    let mut raw_fields = raw_fields.clone();
    normalize_sets(&mut raw_fields)?;
    let value: TransferProjection =
        serde_json::from_value(raw).map_err(|e| invalid("transfer_json", &e.to_string()))?;
    validate_fields(&value.fields, b, s)?;
    if jcs(&raw_fields)? != canonical_fields_bytes(&value.fields)? {
        return Err(invalid(
            "transfer_fields",
            "raw fields were missing, extra or changed by typed decoding",
        ));
    }
    verify_hash(&value)?;
    Ok(value)
}
pub(crate) fn verify_hash(projection: &TransferProjection) -> Result<()> {
    let expected =
        handshake_canonical_json::sha256_hex(&canonical_fields_bytes(&projection.fields)?);
    if expected != projection.payload_hash {
        return Err(Error::Conflict {
            field: "payload_hash".into(),
        });
    }
    Ok(())
}
pub(crate) fn origin_parts(origin: &OriginRef) -> Result<(String, String)> {
    let value = serde_json::to_value(origin).map_err(|e| invalid("origin", &e.to_string()))?;
    let kind = value["kind"]
        .as_str()
        .ok_or_else(|| invalid("origin_kind", "required"))?
        .to_owned();
    let id = match &value["id"] {
        serde_json::Value::String(s) => s.clone(),
        v => String::from_utf8(jcs(v)?).map_err(|_| invalid("origin_id", "UTF8 required"))?,
    };
    Ok((kind, id))
}

/// Validate the entire serialized tree before calling the trusted typed JCS serializer.
pub(crate) fn bounded_value(
    value: &serde_json::Value,
    b: &Bounds,
    depth: usize,
    total: &mut usize,
) -> Result<()> {
    if depth > 64 {
        return Err(invalid("transfer", "maximum nesting depth exceeded"));
    }
    *total = total
        .checked_add(1)
        .ok_or_else(|| invalid("transfer", "size overflow"))?;
    if *total > b.max_operation_bytes {
        return Err(Error::LimitExceeded {
            field: "transfer".into(),
            limit: b.max_operation_bytes,
        });
    }
    match value {
        serde_json::Value::String(s) => {
            b.text("transfer_string", s, b.max_operation_bytes)?;
            *total = total
                .checked_add(s.len())
                .ok_or_else(|| invalid("transfer", "size overflow"))?;
        }
        serde_json::Value::Number(n) => {
            if n.as_i64()
                .is_some_and(|v| !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&v))
                || n.as_u64().is_some_and(|v| v > 9_007_199_254_740_991)
                || n.as_f64().is_none_or(|v| {
                    !v.is_finite() || (v.fract() == 0.0 && v.abs() > 9_007_199_254_740_991.0)
                })
            {
                return Err(invalid(
                    "transfer_number",
                    "unsafe integer or nonfinite value; encode exact integers as decimal strings",
                ));
            }
        }
        serde_json::Value::Array(a) => {
            if a.len() > b.max_collection_items {
                return Err(Error::LimitExceeded {
                    field: "transfer_array".into(),
                    limit: b.max_collection_items,
                });
            }
            for v in a {
                bounded_value(v, b, depth + 1, total)?;
            }
        }
        serde_json::Value::Object(o) => {
            if o.len() > b.max_collection_items.max(64) {
                return Err(Error::LimitExceeded {
                    field: "transfer_object".into(),
                    limit: b.max_collection_items.max(64),
                });
            }
            for (k, v) in o {
                b.text("transfer_key", k, b.max_text_bytes)?;
                *total = total
                    .checked_add(k.len())
                    .ok_or_else(|| invalid("transfer", "size overflow"))?;
                bounded_value(v, b, depth + 1, total)?;
            }
        }
        _ => {}
    }
    if *total > b.max_operation_bytes {
        return Err(Error::LimitExceeded {
            field: "transfer".into(),
            limit: b.max_operation_bytes,
        });
    }
    Ok(())
}
pub(crate) fn preflight_extensions(
    common: &Extensions,
    b: &Bounds,
    total: &mut usize,
) -> Result<()> {
    if common.extensions.len() > b.max_collection_items {
        return Err(Error::LimitExceeded {
            field: "extensions".into(),
            limit: b.max_collection_items,
        });
    }
    for value in common.extensions.values() {
        bounded_value(value, b, 0, total)?;
    }
    Ok(())
}
pub(crate) fn preflight_record(
    record: &TransferRecord,
    b: &Bounds,
    total: &mut usize,
) -> Result<()> {
    let common = match record {
        TransferRecord::Item(v) => &v.common,
        TransferRecord::Capture(v) => &v.common,
        TransferRecord::Identity(v) => &v.common,
        TransferRecord::Observation(v) => &v.common,
        TransferRecord::Stream(v) => &v.common,
        TransferRecord::Member(v) => &v.common,
        TransferRecord::Remote(v) => &v.common,
        TransferRecord::Binding(v) => &v.common,
        TransferRecord::Assignment(v) => &v.common,
        TransferRecord::Membership(v) => &v.common,
        TransferRecord::Placement(v) => &v.common,
        TransferRecord::Occurrence(v) => &v.common,
    };
    preflight_extensions(common, b, total)?;
    if let TransferRecord::Observation(v) = record {
        if let Some(g) = &v.gallery_snapshot {
            if g.members.len() > b.max_collection_items {
                return Err(Error::LimitExceeded {
                    field: "gallery_snapshot".into(),
                    limit: b.max_collection_items,
                });
            }
            for member in &g.members {
                preflight_extensions(&member.common, b, total)?;
            }
        }
    }
    if let TransferRecord::Occurrence(v) = record {
        if let Some(p) = &v.presentation {
            preflight_extensions(&p.layout, b, total)?;
        }
    }
    Ok(())
}
pub(crate) fn validate_fields(f: &TransferFields, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
    b.validate()?;
    f.revision.validate()?;
    if f.model_version != MODEL_VERSION {
        return Err(Error::UnsupportedRequiredFeature {
            feature: f.model_version.clone(),
        });
    }
    for (name, v) in [
        ("origin_instance_id", &f.origin_instance_id),
        ("projection_policy_id", &f.projection_policy_id),
        ("audience_ref", &f.audience_ref.0),
    ] {
        b.text(name, v, b.max_text_bytes)?;
        if v.is_empty() {
            return Err(invalid(name, "required"));
        }
    }
    if origin_parts(&f.record.origin())? != (f.origin_kind.clone(), f.origin_id.clone()) {
        return Err(Error::Conflict {
            field: "origin_record".into(),
        });
    }
    // Bound extension depth before record validators or serde recurse into typed Values.
    let mut total = 0;
    for value in f.extensions.values() {
        bounded_value(value, b, 0, &mut total)?;
    }
    preflight_record(&f.record, b, &mut total)?;
    f.record.validate(b, s)?;
    Extensions {
        extensions: f.extensions.clone(),
        required_features: f.required_features.clone(),
    }
    .validate_bounded(b, s)?;
    let value = serde_json::to_value(f).map_err(|e| invalid("transfer", &e.to_string()))?;
    bounded_value(&value, b, 0, &mut 0)?;
    b.text(
        "transfer",
        &String::from_utf8(jcs(f)?).map_err(|_| invalid("transfer", "UTF8 required"))?,
        b.max_operation_bytes,
    )
}
