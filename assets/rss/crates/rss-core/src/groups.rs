use crate::sources::{SourceGroup, SourceLibrary};
use rss_contracts::{Bounds, Error, HostRef, Result, Revision, ScopeRef};

impl SourceLibrary {
    pub fn save_group(&self, scope: &ScopeRef, expected: Revision, mut group: SourceGroup, bounds: &Bounds) -> Result<Self> {
        self.authorize(scope, expected)?;
        bounds.validate()?;
        if group.owner_scope != *scope { return Err(Error::Denied); }
        if group.name.trim().is_empty() { return Err(Error::Invalid { field: "group.name".into() }); }
        for (field, value) in [("group.name", &group.name), ("group.description", &group.description)] {
            bounds.text(field, value, bounds.max_text_bytes)?;
        }
        for (field, len) in [("group.keywords", group.keywords.len()), ("group.tags", group.tag_refs.len())] {
            if len > bounds.max_collection_items { return Err(Error::LimitExceeded { field: field.into() }); }
        }
        for keyword in &group.keywords { bounds.text("group.keywords", keyword, bounds.max_text_bytes)?; }
        if let Some(current) = self.groups.get(&group.group_ref) {
            if current.owner_scope != *scope { return Err(Error::Denied); }
            if group.revision != current.revision { return Err(Error::Conflict); }
            if current == &group { return Ok(self.clone()); }
            group.revision = group.revision.next()?;
        } else {
            if group.revision != Revision(1) { return Err(Error::Invalid { field: "group.revision".into() }); }
            if self.groups.len() >= bounds.max_collection_items { return Err(Error::LimitExceeded { field: "groups".into() }); }
        }
        let mut next = self.clone();
        next.groups.insert(group.group_ref.clone(), group);
        next.advance()?;
        Ok(next)
    }
    pub fn attach(&self, scope: &ScopeRef, expected: Revision, group_ref: &HostRef, source_ref: &HostRef, bounds: &Bounds) -> Result<Self> {
        self.authorize(scope, expected)?;
        let group = self.groups.get(group_ref).ok_or(Error::NotFound)?;
        let source = self.sources.get(source_ref).ok_or(Error::NotFound)?;
        if group.owner_scope != *scope || source.owner_scope != *scope { return Err(Error::Denied); }
        let key = (group_ref.clone(), source_ref.clone());
        if self.memberships.contains(&key) { return Ok(self.clone()); }
        if self.memberships.len() >= bounds.max_collection_items { return Err(Error::LimitExceeded { field: "memberships".into() }); }
        let mut next = self.clone(); next.memberships.insert(key); next.advance()?; Ok(next)
    }
    pub fn detach(&self, scope: &ScopeRef, expected: Revision, group_ref: &HostRef, source_ref: &HostRef) -> Result<Self> {
        self.authorize(scope, expected)?;
        let group = self.groups.get(group_ref).ok_or(Error::NotFound)?;
        let source = self.sources.get(source_ref).ok_or(Error::NotFound)?;
        if group.owner_scope != *scope || source.owner_scope != *scope { return Err(Error::Denied); }
        let key = (group_ref.clone(), source_ref.clone());
        if !self.memberships.contains(&key) { return Ok(self.clone()); }
        let mut next = self.clone(); next.memberships.remove(&key); next.member_rules.remove(&key); next.advance()?; Ok(next)
    }
    pub fn remove_group(&self, scope: &ScopeRef, expected: Revision, group_ref: &HostRef) -> Result<Self> {
        self.authorize(scope, expected)?;
        let group = self.groups.get(group_ref).ok_or(Error::NotFound)?;
        if group.owner_scope != *scope { return Err(Error::Denied); }
        let mut next = self.clone(); next.groups.remove(group_ref);
        next.memberships.retain(|(group, _)| group != group_ref);
        next.member_rules.retain(|(group, _), _| group != group_ref);
        next.advance()?; Ok(next)
    }
}
