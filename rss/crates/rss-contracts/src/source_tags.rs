//! Optional source metadata; tag catalogue ownership and authorization remain with the host.
use crate::{invalid, validate_ref, Bounds, Error, Result, Source, TagRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SOURCE_TAGS_EXTENSION: &str = "rss.source_tags";
pub const MAX_SOURCE_TAGS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceTags {
    pub version: u32,
    pub tag_refs: Vec<TagRef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTags {
    version: u32,
    tag_refs: Vec<WireReference>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReference {
    host_instance_id: String,
    owner_kind: String,
    native_id: String,
}

impl<'de> Deserialize<'de> for SourceTags {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let wire = WireTags::deserialize(deserializer)?;
        Ok(Self { version: wire.version, tag_refs: wire.tag_refs.into_iter().map(|tag| TagRef(crate::HostRef {
            host_instance_id:tag.host_instance_id, owner_kind:tag.owner_kind, native_id:tag.native_id,
        })).collect() })
    }
}

impl SourceTags {
    pub fn new(tag_refs: Vec<TagRef>, bounds: &Bounds) -> Result<Self> {
        let value = Self { version: 1, tag_refs };
        value.validate(bounds)?;
        Ok(value)
    }

    pub fn validate(&self, bounds: &Bounds) -> Result<()> {
        bounds.validate()?;
        if self.version != 1 { return Err(invalid("source_tags.version")); }
        if self.tag_refs.len() > MAX_SOURCE_TAGS.min(bounds.max_collection_items) {
            return Err(Error::LimitExceeded { field: "source_tags.tag_refs".into() });
        }
        let mut unique = BTreeSet::new();
        for tag in &self.tag_refs {
            validate_ref(&tag.0, bounds)?;
            if !unique.insert(tag) { return Err(invalid("source_tags.duplicate_reference")); }
        }
        Ok(())
    }
}

impl Source {
    /// Absence means no source tags, without inserting or normalizing extension data.
    pub fn source_tags(&self, bounds: &Bounds) -> Result<SourceTags> {
        let Some(value) = self.common.extensions.get(SOURCE_TAGS_EXTENSION) else {
            return SourceTags::new(vec![], bounds);
        };
        self.common.validate_bounded(bounds, &BTreeSet::new())?;
        let value: SourceTags = serde_json::from_value(value.clone()).map_err(|_| invalid("source_tags"))?;
        value.validate(bounds)?;
        Ok(value)
    }
}
