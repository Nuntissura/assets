//! Code -> DOM refresh: a text edit at revision R re-projects to the DOM at exactly R+1, and a key map
//! reports which nodes survived (old -> new), which subtrees were added and which were removed.
//!
//! Identity is structural, never guessed from content: children of matched parents are matched by
//! (kind/tag label, HTML `id`) with the common prefix and suffix first, then in-order inside the changed
//! middle (by label+id, then by label alone). Elements that carry an HTML `id` keep identity across
//! reordering inside one parent; anonymous siblings of the same tag are matched by order only.

use crate::dom::{NodeId, NodeKey, WebDoc, parse};
use crate::error::WebError;
use hsk_studio_accord::CancellationToken;
use std::collections::{HashMap, VecDeque};

/// Subtree roots reported individually; counts stay exact beyond this.
pub const MAX_REPORTED_ROOTS: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyMap {
    to_new: Vec<Option<NodeId>>,
    to_old: Vec<Option<NodeId>>,
    kept: usize,
    added_roots: Vec<NodeId>,
    removed_roots: Vec<NodeId>,
    added_nodes: usize,
    removed_nodes: usize,
}
impl KeyMap {
    /// Nodes present in both revisions (the document root counts).
    pub fn kept(&self) -> usize {
        self.kept
    }
    pub fn new_id(&self, old: NodeId) -> Option<NodeId> {
        self.to_new.get(old).copied().flatten()
    }
    pub fn old_id(&self, new: NodeId) -> Option<NodeId> {
        self.to_old.get(new).copied().flatten()
    }
    /// Map a key of the old revision to the key of the same node in the new revision.
    pub fn map_key(&self, old: &WebDoc, new: &WebDoc, key: &NodeKey) -> Option<NodeKey> {
        new.key_of(self.new_id(old.resolve(key)?)?)
    }
    /// Roots (new-revision ids) of subtrees without a counterpart in the old revision.
    pub fn added_roots(&self) -> &[NodeId] {
        &self.added_roots
    }
    /// Roots (old-revision ids) of subtrees without a counterpart in the new revision.
    pub fn removed_roots(&self) -> &[NodeId] {
        &self.removed_roots
    }
    pub fn added_nodes(&self) -> usize {
        self.added_nodes
    }
    pub fn removed_nodes(&self) -> usize {
        self.removed_nodes
    }
    pub fn added_keys(&self, new: &WebDoc) -> Vec<NodeKey> {
        self.added_roots
            .iter()
            .filter_map(|&id| new.key_of(id))
            .collect()
    }
    pub fn removed_keys(&self, old: &WebDoc) -> Vec<NodeKey> {
        self.removed_roots
            .iter()
            .filter_map(|&id| old.key_of(id))
            .collect()
    }
    /// True when more roots were added or removed than `MAX_REPORTED_ROOTS` lists.
    pub fn truncated(&self) -> bool {
        self.added_roots.len() >= MAX_REPORTED_ROOTS
            || self.removed_roots.len() >= MAX_REPORTED_ROOTS
    }
}

type Sig = (String, Option<String>);

fn signature(doc: &WebDoc, id: NodeId) -> Sig {
    let html_id = doc.attribute(id, "id").and_then(|a| doc.attr_value(a));
    (doc.label(id), html_id)
}

/// Pairs of positions (old index, new index) of matched children.
fn match_children(
    old: &WebDoc,
    old_kids: &[NodeId],
    new: &WebDoc,
    new_kids: &[NodeId],
) -> Vec<(usize, usize)> {
    let old_sigs: Vec<Sig> = old_kids.iter().map(|&c| signature(old, c)).collect();
    let new_sigs: Vec<Sig> = new_kids.iter().map(|&c| signature(new, c)).collect();
    let mut pairs = Vec::new();
    let mut prefix = 0;
    while prefix < old_sigs.len() && prefix < new_sigs.len() && old_sigs[prefix] == new_sigs[prefix]
    {
        pairs.push((prefix, prefix));
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_sigs.len() - prefix
        && suffix < new_sigs.len() - prefix
        && old_sigs[old_sigs.len() - 1 - suffix] == new_sigs[new_sigs.len() - 1 - suffix]
    {
        pairs.push((old_sigs.len() - 1 - suffix, new_sigs.len() - 1 - suffix));
        suffix += 1;
    }
    let old_mid = prefix..old_sigs.len() - suffix;
    let new_mid = prefix..new_sigs.len() - suffix;
    let mut old_free: Vec<usize> = old_mid.collect();
    let mut new_free: Vec<usize> = new_mid.collect();
    // Pass 0 matches elements that carry an HTML id by (label, id), in any order; pass 1 matches id-less
    // siblings by label in order; pass 2 matches what is left by label alone in order.
    for pass in 0..3 {
        if old_free.is_empty() || new_free.is_empty() {
            break;
        }
        let key = |s: &Sig| match pass {
            0 => s.1.is_some().then(|| s.clone()),
            1 => s.1.is_none().then(|| s.clone()),
            _ => Some((s.0.clone(), None)),
        };
        let mut queues: HashMap<Sig, VecDeque<usize>> = HashMap::new();
        for &o in &old_free {
            if let Some(k) = key(&old_sigs[o]) {
                queues.entry(k).or_default().push_back(o);
            }
        }
        let (mut last, mut used_old, mut used_new) = (
            None::<usize>,
            vec![false; old_sigs.len()],
            vec![false; new_sigs.len()],
        );
        for &n in &new_free {
            let Some(queue) = key(&new_sigs[n]).and_then(|k| queues.get_mut(&k)) else {
                continue;
            };
            if pass > 0 {
                while queue.front().is_some_and(|&o| last.is_some_and(|l| o <= l)) {
                    queue.pop_front();
                }
            }
            if let Some(o) = queue.pop_front() {
                pairs.push((o, n));
                used_old[o] = true;
                used_new[n] = true;
                last = Some(o);
            }
        }
        old_free.retain(|&o| !used_old[o]);
        new_free.retain(|&n| !used_new[n]);
    }
    pairs
}

/// Match two projections of the same document lineage. Cost is linear in the node count (plus the cost of
/// the changed middle of each child list).
pub fn key_map(old: &WebDoc, new: &WebDoc, cancel: &CancellationToken) -> Result<KeyMap, WebError> {
    let mut map = KeyMap {
        to_new: vec![None; old.len()],
        to_old: vec![None; new.len()],
        ..KeyMap::default()
    };
    map.to_new[0] = Some(0);
    map.to_old[0] = Some(0);
    map.kept = 1;
    let mut work = vec![(0usize, 0usize)];
    let mut ticks = 0u32;
    while let Some((o, n)) = work.pop() {
        ticks = ticks.wrapping_add(1);
        if ticks & 0x3ff == 0 {
            cancel.check()?;
        }
        let (old_kids, new_kids) = match (old.node(o), new.node(n)) {
            (Some(a), Some(b)) => (&a.children, &b.children),
            _ => continue,
        };
        let mut matched_old = vec![false; old_kids.len()];
        let mut matched_new = vec![false; new_kids.len()];
        for (oi, ni) in match_children(old, old_kids, new, new_kids) {
            matched_old[oi] = true;
            matched_new[ni] = true;
            let (oc, nc) = (old_kids[oi], new_kids[ni]);
            map.to_new[oc] = Some(nc);
            map.to_old[nc] = Some(oc);
            map.kept += 1;
            work.push((oc, nc));
        }
        for (i, &c) in old_kids.iter().enumerate() {
            if matched_old[i] {
                continue;
            }
            map.removed_nodes += old.descendants(c).count();
            if map.removed_roots.len() < MAX_REPORTED_ROOTS {
                map.removed_roots.push(c);
            }
        }
        for (i, &c) in new_kids.iter().enumerate() {
            if matched_new[i] {
                continue;
            }
            map.added_nodes += new.descendants(c).count();
            if map.added_roots.len() < MAX_REPORTED_ROOTS {
                map.added_roots.push(c);
            }
        }
    }
    Ok(map)
}

/// Re-parse edited source as the next revision of `old` and map identities across the edit. Equal to
/// `parse(new_src, old.revision() + 1)` plus the key map; any other target revision is `StaleRevision`.
pub fn refresh(
    old: &WebDoc,
    new_src: &str,
    new_revision: u64,
    cancel: &CancellationToken,
) -> Result<(WebDoc, KeyMap), WebError> {
    let expected = old
        .revision()
        .checked_add(1)
        .ok_or(WebError::RevisionOverflow)?;
    if new_revision != expected {
        return Err(WebError::StaleRevision {
            expected,
            found: new_revision,
        });
    }
    let new = parse(new_src, new_revision, old.limits(), cancel)?;
    let map = key_map(old, &new, cancel)?;
    Ok((new, map))
}
