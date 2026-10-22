//! Bounded ephemeral adapter for standalone proof; no production durability claim.
use crate::{Error, Snapshot, Store};
use std::collections::BTreeMap;

pub struct MemoryStore {
    workspaces: BTreeMap<String, Snapshot>,
    capacity: usize,
}
impl MemoryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            workspaces: BTreeMap::new(),
            capacity,
        }
    }
}
impl Store for MemoryStore {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        Ok(self.workspaces.get(workspace).cloned().unwrap_or_default())
    }
    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        let current = self.workspaces.get(workspace).map_or(0, |s| s.revision);
        if current != expected || next.revision != expected.checked_add(1).ok_or(Error::Conflict)? {
            return Err(Error::Conflict);
        }
        if !self.workspaces.contains_key(workspace) && self.workspaces.len() >= self.capacity {
            return Err(Error::Capacity("workspaces"));
        }
        self.workspaces.insert(workspace.to_owned(), next);
        Ok(())
    }
}
