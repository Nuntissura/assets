use crate::SourceLibrary;
use rss_contracts::{Bounds, Error, HostRef, Result, Revision, ScopeRef, SourceTags, TagRef, SOURCE_TAGS_EXTENSION};
use std::collections::BTreeSet;

impl SourceLibrary {
    /// The host validates catalogue access before committing the proposed library with its own CAS.
    pub fn set_source_tags(&self, scope: &ScopeRef, expected: Revision, source_ref: &HostRef,
        expected_source: Revision, tag_refs: Vec<TagRef>, bounds: &Bounds) -> Result<Self> {
        self.authorize(scope, expected)?;
        let source = self.sources.get(source_ref).ok_or(Error::NotFound)?;
        if source.owner_scope != *scope { return Err(Error::Denied); }
        if source.revision != expected_source { return Err(Error::Conflict); }
        let previous = source.source_tags(bounds)?;
        let next_tags = SourceTags::new(tag_refs, bounds)?;
        let previous_refs: BTreeSet<_> = previous.tag_refs.iter().collect();
        if previous_refs == next_tags.tag_refs.iter().collect() { return Ok(self.clone()); }
        let mut next = self.clone();
        let changed = next.sources.get_mut(source_ref).ok_or(Error::NotFound)?;
        changed.common.extensions.insert(SOURCE_TAGS_EXTENSION.into(),
            serde_json::to_value(next_tags).map_err(|_| rss_contracts::Error::Invalid { field: "source_tags".into() })?);
        changed.source_tags(bounds)?;
        changed.revision = changed.revision.next()?;
        next.advance()?;
        Ok(next)
    }
}
