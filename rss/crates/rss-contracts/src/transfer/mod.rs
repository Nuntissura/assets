//! RSS provenance inside an existing hsscrap transfer projection.
//!
//! Hosts supply an authorized consistent snapshot and complete exported reference set. Origin scope
//! is provenance, never a destination grant. Hosts sign the negotiated envelope and recheck access.
//! Reuse hsscrap's RFC8785 canonicalization and hashes; a hash is not an RFC7515 signature.
use crate::*;
use hsscrap::transfer::{OriginRef, TransferFields, TransferProjection, TransferRecord};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const REQUIRED_FEATURE: &str = "rss.transfer.v1";

/// Source identity and revision only; no URL, connection, executor or continuation is transferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRevision {
    pub source_ref: HostRef,
    pub revision: Revision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemExtension {
    pub origin_scope: ScopeRef,
    /// Attested by the snapshot owner, not inferred from the number of returned records.
    pub coverage: hsscrap::Coverage,
    pub sources: Vec<SourceRevision>,
    pub appearances: Vec<SourceAppearance>,
    pub states: Vec<AppearanceState>,
}

/// Trusted host inputs, not remote declarations. References must describe the actual exported set.
pub struct ExtensionContext<'a> {
    pub origin_scope: &'a ScopeRef,
    pub exported_origins: &'a BTreeSet<OriginRef>,
    pub exported_records: &'a [TransferRecord],
    /// Explicit approval to transfer these metadata keys; never infer safety from a namespace.
    pub allowed_extension_keys: &'a BTreeSet<String>,
}

fn common(
    value: &Extensions,
    context: &ExtensionContext<'_>,
    bounds: &Bounds,
    supported: &BTreeSet<String>,
) -> Result<()> {
    if value.extensions.len() > bounds.max_collection_items
        || value.required_features.len() > bounds.max_collection_items
        || value.extensions.len() > bounds.max_operation_bytes
    {
        return Err(Error::LimitExceeded {
            field: "transfer.extensions".into(),
        });
    }
    if value
        .extensions
        .keys()
        .any(|key| !context.allowed_extension_keys.contains(key))
    {
        return Err(Error::Denied);
    }
    // Check depth before the shared recursive validator or serde sees host-supplied Values.
    let mut pending: Vec<_> = value
        .extensions
        .values()
        .map(|value| (value, 0usize))
        .collect();
    let mut visited = 0usize;
    while let Some((value, depth)) = pending.pop() {
        visited = visited
            .checked_add(1)
            .ok_or_else(|| invalid("transfer.extensions"))?;
        if depth > 48 || visited > bounds.max_operation_bytes {
            return Err(Error::LimitExceeded {
                field: "transfer.extensions".into(),
            });
        }
        let children = match value {
            serde_json::Value::Array(values) => values.len(),
            serde_json::Value::Object(values) => values.len(),
            _ => 0,
        };
        let queued = visited
            .checked_add(pending.len())
            .and_then(|count| count.checked_add(children));
        if children > bounds.max_collection_items
            || queued.is_none_or(|count| count > bounds.max_operation_bytes)
        {
            return Err(Error::LimitExceeded {
                field: "transfer.extensions".into(),
            });
        }
        match value {
            serde_json::Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    value.validate_bounded(bounds, supported)?;
    Ok(())
}

fn validate(
    fields: &TransferFields,
    extension: &ItemExtension,
    context: &ExtensionContext<'_>,
    bounds: &Bounds,
    supported: &BTreeSet<String>,
) -> Result<()> {
    bounds.validate()?;
    if !supported.contains(REQUIRED_FEATURE) {
        return Err(hsscrap::Error::UnsupportedRequiredFeature {
            feature: REQUIRED_FEATURE.into(),
        }
        .into());
    }
    let TransferRecord::Item(item) = &fields.record else {
        return Err(invalid("transfer.item"));
    };
    if extension.origin_scope != *context.origin_scope
        || context.origin_scope.0.is_empty()
        || !context
            .exported_origins
            .contains(&OriginRef::Item(item.item_id))
    {
        return Err(Error::Denied);
    }
    bounded(
        &extension.origin_scope.0,
        "transfer.origin_scope",
        bounds.max_text_bytes,
    )?;
    for count in [
        extension.sources.len(),
        extension.appearances.len(),
        extension.states.len(),
        context.exported_origins.len(),
        context.exported_records.len(),
        context.allowed_extension_keys.len(),
    ] {
        if count > bounds.max_collection_items {
            return Err(Error::LimitExceeded {
                field: "transfer.references".into(),
            });
        }
    }
    let mut records = BTreeSet::new();
    for record in context.exported_records {
        let origin = record.origin();
        if !context.exported_origins.contains(&origin) || !records.insert(origin) {
            return Err(invalid("transfer.exported_records"));
        }
    }
    if !context
        .exported_records
        .iter()
        .any(|record| record == &fields.record)
    {
        return Err(invalid("transfer.item_reference"));
    }
    let mut identities = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for source in &extension.sources {
        validate_ref(&source.source_ref, bounds)?;
        source.revision.validate()?;
        if !sources.insert(&source.source_ref) || !identities.insert(&source.source_ref) {
            return Err(invalid("transfer.sources"));
        }
    }
    let mut appearances = BTreeSet::new();
    let mut used_sources = BTreeSet::new();
    for appearance in &extension.appearances {
        common(&appearance.common, context, bounds, supported)?;
        appearance.validate(bounds, supported)?;
        if appearance.item_id != item.item_id
            || appearance.access_partition != extension.origin_scope
            || !sources.contains(&appearance.source_ref)
            || !appearances.insert(&appearance.appearance_ref)
            || !identities.insert(&appearance.appearance_ref)
        {
            return Err(invalid("transfer.appearances"));
        }
        if let Some(id) = appearance.latest_observation_id {
            if !context.exported_records.iter().any(|record| {
                matches!(record,
                TransferRecord::Observation(observation) if observation.observation_id == id
                    && observation.item_id == item.item_id)
            }) {
                return Err(invalid("transfer.observation_reference"));
            }
        }
        used_sources.insert(&appearance.source_ref);
    }
    if sources != used_sources {
        return Err(invalid("transfer.source_reference"));
    }
    let mut states = BTreeSet::new();
    for state in &extension.states {
        common(&state.common, context, bounds, supported)?;
        validate_ref(&state.state_ref, bounds)?;
        validate_ref(&state.appearance_ref, bounds)?;
        state.revision.validate()?;
        if let Some(reference) = &state.legacy_item_ref {
            validate_ref(reference, bounds)?;
        }
        if state.item_id != item.item_id
            || state.scope != extension.origin_scope
            || !appearances.contains(&state.appearance_ref)
            || !states.insert(&state.state_ref)
            || !identities.insert(&state.state_ref)
        {
            return Err(invalid("transfer.states"));
        }
        if state.legacy_tag_refs.len() > bounds.max_collection_items {
            return Err(Error::LimitExceeded {
                field: "transfer.tags".into(),
            });
        }
        let mut tags = BTreeSet::new();
        for tag in &state.legacy_tag_refs {
            validate_ref(&tag.0, bounds)?;
            if !tags.insert(tag)
                || !context
                    .exported_origins
                    .contains(&OriginRef::Tag(tag.clone()))
            {
                return Err(invalid("transfer.tag_reference"));
            }
        }
    }
    Ok(())
}

fn bounded_value(extension: &ItemExtension, limit: usize) -> Result<serde_json::Value> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("RSS transfer byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut buffer, extension).map_err(|_| Error::LimitExceeded {
        field: "transfer.bytes".into(),
    })?;
    serde_json::from_slice(&buffer.bytes).map_err(|_| invalid("transfer.extension"))
}

/// Attach before hsscrap plan_export hashes fields. Refuse replacing any existing RSS semantics.
pub fn attach_item_extension(
    mut fields: TransferFields,
    extension: ItemExtension,
    context: &ExtensionContext<'_>,
    bounds: &Bounds,
    supported: &BTreeSet<String>,
) -> Result<TransferFields> {
    if fields.extensions.contains_key(REQUIRED_FEATURE)
        || fields.required_features.contains(REQUIRED_FEATURE)
    {
        return Err(Error::Conflict);
    }
    validate(&fields, &extension, context, bounds, supported)?;
    let value = bounded_value(&extension, bounds.max_operation_bytes)?;
    fields.extensions.insert(REQUIRED_FEATURE.into(), value);
    fields.required_features.insert(REQUIRED_FEATURE.into());
    Ok(fields)
}

/// Untrusted bytes retain the existing hsscrap duplicate-key, JCS, hash and required-feature proof.
/// Success authenticates neither a sender nor live grants; the host verifies its signed envelope.
pub fn decode_item_projection(
    input: &[u8],
    context: &ExtensionContext<'_>,
    bounds: &Bounds,
    supported: &BTreeSet<String>,
) -> Result<(TransferProjection, Option<ItemExtension>)> {
    let projection = hsscrap::transfer::decode_projection(input, bounds, supported)?;
    let value = projection.fields.extensions.get(REQUIRED_FEATURE);
    let required = projection
        .fields
        .required_features
        .contains(REQUIRED_FEATURE);
    let Some(value) = value else {
        if required {
            return Err(invalid("transfer.extension_required"));
        }
        return Ok((projection, None));
    };
    if !required {
        return Err(invalid("transfer.feature_required"));
    }
    let extension: ItemExtension =
        serde_json::from_value(value.clone()).map_err(|_| invalid("transfer.extension"))?;
    validate(&projection.fields, &extension, context, bounds, supported)?;
    if bounded_value(&extension, bounds.max_operation_bytes)? != *value {
        return Err(invalid("transfer.extension_fields"));
    }
    Ok((projection, Some(extension)))
}

#[cfg(test)]
mod tests;
