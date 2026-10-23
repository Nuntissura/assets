//! Property-expression dependency policy (STU-MOT-078): a dependency cycle is a validation error
//! naming the participating property paths, refused at commit (graph unchanged), never a hang.
//! Iterative traversal only; deterministic ordering (`BTreeMap`/`BTreeSet`).
use crate::{error::MotionError, path::PropertyPath};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_READS_PER_EXPRESSION: usize = 4096;
pub const MAX_GRAPH_NODES: usize = 65_536;

/// Outcome of resolving one textual reference to stable property paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    One(PropertyPath),
    None,
    Many(Vec<PropertyPath>),
}

/// Host-owned alias/name resolution. References never resolve by silent guess.
pub trait ReferenceResolver {
    fn resolve(&self, reference: &str) -> Resolution;
}

/// Maps references to paths: zero matches -> `ReferenceMissing`, several -> `AmbiguousReference`.
pub fn resolve_references(
    references: &[&str],
    resolver: &dyn ReferenceResolver,
) -> Result<Vec<PropertyPath>, MotionError> {
    let mut paths = Vec::with_capacity(references.len());
    for reference in references {
        match resolver.resolve(reference) {
            Resolution::One(path) => paths.push(path),
            Resolution::None => {
                return Err(MotionError::ReferenceMissing {
                    reference: (*reference).to_owned(),
                });
            }
            Resolution::Many(candidates) => {
                return Err(MotionError::AmbiguousReference {
                    reference: (*reference).to_owned(),
                    candidates: candidates.len(),
                });
            }
        }
    }
    Ok(paths)
}

#[derive(Clone, Debug, Default)]
pub struct DepGraph {
    declared: BTreeSet<PropertyPath>,
    reads: BTreeMap<PropertyPath, BTreeSet<PropertyPath>>,
}

impl DepGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an existing property so expressions may read it or be committed on it.
    pub fn declare(&mut self, path: PropertyPath) -> Result<(), MotionError> {
        if !self.declared.contains(&path) && self.declared.len() >= MAX_GRAPH_NODES {
            return Err(MotionError::NodeLimit);
        }
        self.declared.insert(path);
        Ok(())
    }

    pub fn is_declared(&self, path: &PropertyPath) -> bool {
        self.declared.contains(path)
    }

    pub fn reads_of(&self, path: &PropertyPath) -> Vec<&PropertyPath> {
        self.reads
            .get(path)
            .map(|set| set.iter().collect())
            .unwrap_or_default()
    }

    /// Replaces the read set of `path`'s expression. Atomic: on any error the graph is unchanged.
    /// Cycle participants are listed in cycle order starting at `path` (`[path]` for a self read).
    pub fn commit_expression(
        &mut self,
        path: &PropertyPath,
        reads: &[PropertyPath],
    ) -> Result<(), MotionError> {
        if reads.len() > MAX_READS_PER_EXPRESSION {
            return Err(MotionError::ReadLimit);
        }
        for node in std::iter::once(path).chain(reads.iter()) {
            if !self.declared.contains(node) {
                return Err(MotionError::ReferenceMissing {
                    reference: node.as_str().to_owned(),
                });
            }
        }
        let wanted: BTreeSet<PropertyPath> = reads.iter().cloned().collect();
        if let Some(participants) = self.find_cycle(path, &wanted) {
            return Err(MotionError::Cycle { participants });
        }
        if wanted.is_empty() {
            self.reads.remove(path);
        } else {
            self.reads.insert(path.clone(), wanted);
        }
        Ok(())
    }

    pub fn remove_expression(&mut self, path: &PropertyPath) {
        self.reads.remove(path);
    }

    /// Smallest-start-first iterative DFS: is `path` reachable from one of its new reads?
    fn find_cycle(
        &self,
        path: &PropertyPath,
        wanted: &BTreeSet<PropertyPath>,
    ) -> Option<Vec<PropertyPath>> {
        for start in wanted {
            if start == path {
                return Some(vec![path.clone()]);
            }
            let mut parent: BTreeMap<&PropertyPath, &PropertyPath> = BTreeMap::new();
            let mut seen: BTreeSet<&PropertyPath> = BTreeSet::new();
            let mut stack: Vec<&PropertyPath> = vec![start];
            seen.insert(start);
            while let Some(node) = stack.pop() {
                let Some(next) = self.reads.get(node) else {
                    continue;
                };
                for target in next.iter().rev() {
                    if target == path {
                        let mut chain = vec![node];
                        let mut cursor = node;
                        while let Some(previous) = parent.get(cursor) {
                            chain.push(previous);
                            cursor = previous;
                        }
                        chain.reverse();
                        let mut participants = vec![path.clone()];
                        participants.extend(chain.into_iter().cloned());
                        return Some(participants);
                    }
                    if seen.insert(target) {
                        parent.insert(target, node);
                        stack.push(target);
                    }
                }
            }
        }
        None
    }

    /// Topological evaluation order: every property after the properties its expression reads.
    /// Ties resolve by smallest path, so the order is deterministic.
    pub fn eval_order(&self) -> Vec<PropertyPath> {
        let mut nodes: BTreeSet<&PropertyPath> = self.declared.iter().collect();
        for (owner, set) in &self.reads {
            nodes.insert(owner);
            nodes.extend(set.iter());
        }
        let mut waiting: BTreeMap<&PropertyPath, usize> = BTreeMap::new();
        let mut dependents: BTreeMap<&PropertyPath, Vec<&PropertyPath>> = BTreeMap::new();
        for node in &nodes {
            let count = self.reads.get(*node).map_or(0, BTreeSet::len);
            waiting.insert(node, count);
        }
        for (owner, set) in &self.reads {
            for dependency in set {
                dependents.entry(dependency).or_default().push(owner);
            }
        }
        let mut ready: BTreeSet<&PropertyPath> = waiting
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(node, _)| *node)
            .collect();
        let mut order = Vec::with_capacity(nodes.len());
        while let Some(node) = ready.pop_first() {
            order.push(node.clone());
            for dependent in dependents.get(node).into_iter().flatten() {
                if let Some(count) = waiting.get_mut(dependent) {
                    *count -= 1;
                    if *count == 0 {
                        ready.insert(dependent);
                    }
                }
            }
        }
        order
    }
}
