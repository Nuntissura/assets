use rss_contracts::{Bounds, Error, Extensions, HostRef, Result, Revision, ScopeRef, Source, TagRef};
use std::collections::{BTreeMap, BTreeSet};
use crate::rules::MemberRule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroup {
    pub group_ref: HostRef,
    pub owner_scope: ScopeRef,
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub priority: i32,
    pub tag_refs: BTreeSet<TagRef>,
    pub revision: Revision,
    pub common: Extensions,
}

/// Host-owned persisted snapshot. Commit changed snapshots with a compare-and-swap on revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLibrary {
    pub owner_scope: ScopeRef,
    pub revision: Revision,
    pub sources: BTreeMap<HostRef, Source>,
    pub groups: BTreeMap<HostRef, SourceGroup>,
    pub memberships: BTreeSet<(HostRef, HostRef)>,
    pub member_rules: BTreeMap<(HostRef, HostRef), MemberRule>,
}
impl SourceLibrary {
    pub fn empty(owner_scope: ScopeRef) -> Self {
        Self { owner_scope, revision: Revision(1), sources: BTreeMap::new(), groups: BTreeMap::new(), memberships: BTreeSet::new(), member_rules: BTreeMap::new() }
    }
    pub(crate) fn authorize(&self, scope: &ScopeRef, expected: Revision) -> Result<()> {
        if scope != &self.owner_scope { return Err(Error::Denied); }
        if expected != self.revision { return Err(Error::Conflict); }
        Ok(())
    }
    pub(crate) fn advance(&mut self) -> Result<()> {
        self.revision = self.revision.next()?;
        Ok(())
    }
    pub fn add_source(&self, scope: &ScopeRef, expected: Revision, source: Source, bounds: &Bounds) -> Result<Self> {
        self.authorize(scope, expected)?;
        bounds.validate()?;
        source.source_tags(bounds)?;
        if source.owner_scope != self.owner_scope { return Err(Error::Denied); }
        source.revision.validate()?;
        if self.sources.contains_key(&source.source_ref) {
            return if self.sources.get(&source.source_ref) == Some(&source) { Ok(self.clone()) } else { Err(Error::Conflict) };
        }
        if self.sources.len() >= bounds.max_collection_items { return Err(Error::LimitExceeded { field: "sources".into() }); }
        bounds.text("source.original_url", &source.original_url, bounds.max_url_bytes)?;
        bounds.text("source.display_name", &source.display_name, bounds.max_text_bytes)?;
        let mut next = self.clone();
        next.sources.insert(source.source_ref.clone(), source);
        next.advance()?;
        Ok(next)
    }
    pub fn set_paused(&self, scope: &ScopeRef, expected: Revision, source_ref: &HostRef, paused: bool) -> Result<Self> {
        self.authorize(scope, expected)?;
        let source = self.sources.get(source_ref).ok_or(Error::NotFound)?;
        if source.owner_scope != *scope { return Err(Error::Denied); }
        if source.paused == paused { return Ok(self.clone()); }
        let mut next = self.clone();
        let changed = next.sources.get_mut(source_ref).ok_or(Error::NotFound)?;
        changed.paused = paused;
        changed.revision = changed.revision.next()?;
        next.advance()?;
        Ok(next)
    }
    /// Removes subscription selection only. Captured items, tags and project placements have other owners.
    pub fn remove_source(&self, scope: &ScopeRef, expected: Revision, source_ref: &HostRef) -> Result<Self> {
        self.authorize(scope, expected)?;
        let source = self.sources.get(source_ref).ok_or(Error::NotFound)?;
        if source.owner_scope != *scope { return Err(Error::Denied); }
        let mut next = self.clone();
        next.sources.remove(source_ref);
        next.memberships.retain(|(_, source)| source != source_ref);
        next.member_rules.retain(|(_, source), _| source != source_ref);
        next.advance()?;
        Ok(next)
    }
}
