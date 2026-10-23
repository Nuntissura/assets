use rss_contracts::{AppearanceState, Error, HostRef, ItemId, Result, Revision, ScopeRef, SourceAppearance};
use std::collections::{BTreeMap, BTreeSet};

pub fn update_state(current: &AppearanceState, scope: &ScopeRef, expected: Revision,
    read: Option<bool>, bookmarked: Option<bool>) -> Result<AppearanceState> {
    if &current.scope != scope { return Err(Error::Denied); }
    if current.revision != expected { return Err(Error::Conflict); }
    let mut next = current.clone();
    if let Some(value) = read { next.read = value; }
    if let Some(value) = bookmarked { next.bookmarked = value; }
    if next != *current { next.revision = next.revision.next()?; }
    Ok(next)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAppearances {
    pub item_id: ItemId,
    pub appearances: Vec<SourceAppearance>,
    pub states: Vec<AppearanceState>,
}

/// Groups display identity while retaining every independently authored appearance state.
pub fn group_items(scope: &ScopeRef, selected_sources: &BTreeSet<HostRef>,
    authorized_partitions: &BTreeSet<ScopeRef>, appearances: &[SourceAppearance], states: &[AppearanceState]) -> Vec<ItemAppearances> {
    let mut items = BTreeMap::<ItemId, ItemAppearances>::new();
    for appearance in appearances.iter().filter(|a| selected_sources.contains(&a.source_ref)
        && authorized_partitions.contains(&a.access_partition)) {
        let item = items.entry(appearance.item_id).or_insert_with(|| ItemAppearances {
            item_id: appearance.item_id, appearances: vec![], states: vec![],
        });
        if !item.appearances.iter().any(|a| a.appearance_ref == appearance.appearance_ref) {
            item.appearances.push(appearance.clone());
        }
    }
    for state in states.iter().filter(|s| &s.scope == scope) {
        if let Some(item) = items.get_mut(&state.item_id) {
            if item.appearances.iter().any(|a| a.appearance_ref == state.appearance_ref)
                && !item.states.iter().any(|s| s.state_ref == state.state_ref) { item.states.push(state.clone()); }
        }
    }
    items.into_values().collect()
}
