//! Host-owned recycle-bin intentions. Persistence and permanent deletion stay at the host.
use crate::{EntryId, Snapshot};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinItem {
    pub id: EntryId,
    pub previous_parent: Option<EntryId>,
}

/// Restore to the last live folder, falling back to root when it no longer exists.
pub fn restore_parent(item: &BinItem, live: &Snapshot) -> Option<EntryId> {
    item.previous_parent
        .as_ref()
        .filter(|id| {
            live.entries
                .iter()
                .any(|entry| &entry.id == *id && entry.kind == crate::EntryKind::Folder)
        })
        .cloned()
}

/// A purge plan requires the complete canonical inventory, never a filtered GUI slice.
pub fn purge_inventory(
    items: &[BinItem],
    complete: bool,
    maximum: usize,
) -> Result<Vec<EntryId>, &'static str> {
    if !complete {
        return Err("BIN_INVENTORY_INCOMPLETE");
    }
    if items.len() > maximum {
        return Err("BIN_INVENTORY_BUDGET");
    }
    let ids: BTreeSet<_> = items.iter().map(|item| item.id.clone()).collect();
    if ids.len() != items.len() || ids.iter().any(|id| id.is_empty()) {
        return Err("BIN_IDENTITY_INVALID");
    }
    Ok(ids.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn purge_rejects_partial_and_duplicate_inventory() {
        let item = BinItem {
            id: "file".into(),
            previous_parent: Some("old".into()),
        };
        assert!(purge_inventory(&[item.clone()], false, 10).is_err());
        assert!(purge_inventory(&[item.clone(), item.clone()], true, 10).is_err());
        assert!(purge_inventory(&[item.clone()], true, 0).is_err());
        assert_eq!(purge_inventory(&[item], true, 10).unwrap(), vec!["file"]);
    }

    #[test]
    fn purge_rejects_empty_identity_and_accepts_an_empty_complete_bin() {
        let item = BinItem {
            id: String::new(),
            previous_parent: None,
        };
        assert_eq!(
            purge_inventory(&[item], true, 1),
            Err("BIN_IDENTITY_INVALID")
        );
        assert_eq!(purge_inventory(&[], true, 0), Ok(vec![]));
    }
    #[test]
    fn restore_requires_a_live_folder() {
        let mut snapshot = Snapshot {
            context: crate::Context {
                scope: "s".into(),
                generation: 0,
                revision: 1,
            },
            entries: vec![],
            hierarchy_complete: true,
        };
        let item = BinItem {
            id: "file".into(),
            previous_parent: Some("old".into()),
        };
        assert_eq!(restore_parent(&item, &snapshot), None);
        snapshot.entries.push(crate::Entry::new(
            "old",
            None,
            "Folder",
            crate::EntryKind::Folder,
        ));
        assert_eq!(restore_parent(&item, &snapshot), Some("old".into()));
        snapshot.entries[0].kind = crate::EntryKind::Leaf;
        assert_eq!(restore_parent(&item, &snapshot), None);
    }
}
