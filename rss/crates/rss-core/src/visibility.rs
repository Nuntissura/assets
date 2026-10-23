use crate::sources::SourceLibrary;
use rss_contracts::{Error, HostRef, Result, ScopeRef};
use std::collections::BTreeSet;

/// Supplied after host account/profile authorization; these references never carry grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSelection {
    pub owner_scope: ScopeRef,
    pub profile_ref: HostRef,
    pub selected_groups: BTreeSet<HostRef>,
    pub selected_sources: BTreeSet<HostRef>,
}
impl SourceLibrary {
    pub fn selected_sources(&self, selection: &ProfileSelection, authorized_sources: &BTreeSet<HostRef>) -> Result<BTreeSet<HostRef>> {
        if selection.owner_scope != self.owner_scope { return Err(Error::Denied); }
        for group in &selection.selected_groups {
            let group = self.groups.get(group).ok_or(Error::NotFound)?;
            if group.owner_scope != self.owner_scope { return Err(Error::Denied); }
        }
        let mut selected = selection.selected_sources.clone();
        for (group, source) in &self.memberships {
            if selection.selected_groups.contains(group) { selected.insert(source.clone()); }
        }
        selected.retain(|source| authorized_sources.contains(source)
            && self.sources.get(source).is_some_and(|s| s.owner_scope == self.owner_scope));
        Ok(selected)
    }
    pub fn acquisition_sources(&self, selected: &BTreeSet<HostRef>) -> BTreeSet<HostRef> {
        selected.iter().filter(|id| self.sources.get(*id).is_some_and(|source|
            source.owner_scope == self.owner_scope && !source.paused)).cloned().collect()
    }
    pub fn ungrouped_sources(&self, authorized_sources: &BTreeSet<HostRef>) -> BTreeSet<HostRef> {
        let grouped: BTreeSet<HostRef> = self.memberships.iter().filter(|(group, _)| self.groups.get(group)
            .is_some_and(|g| g.owner_scope == self.owner_scope)).map(|(_, source)| source.clone()).collect();
        self.sources.iter().filter(|(id, source)| source.owner_scope == self.owner_scope
            && authorized_sources.contains(*id) && !grouped.contains(*id)).map(|(id, _)| id.clone()).collect()
    }
}
