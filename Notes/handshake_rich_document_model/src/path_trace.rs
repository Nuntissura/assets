//! Block-path lineage from actual transaction application, independent of IDs.

use crate::{BlockNode, Child, TransactionReceipt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitSide {
    Head,
    Tail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitBlockChild {
    pub old_index: usize,
    pub side: SplitSide,
    pub new_index: usize,
}

/// One entry per forward step. Counts are captured at that step, not from a
/// separately validated prefix or the final document. Split destinations come
/// from the same child-partition operation that mutates the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepPathTrace {
    Unchanged,
    InsertChild {
        parent_path: Vec<usize>,
        index: usize,
    },
    DeleteChild {
        parent_path: Vec<usize>,
        index: usize,
    },
    Merge {
        parent_path: Vec<usize>,
        index: usize,
        left_children_before: usize,
        right_children_before: usize,
        coalesced_boundary: bool,
    },
    Split {
        path: Vec<usize>,
        children_before: usize,
        head_children: usize,
        tail_children: usize,
        block_children: Vec<SplitBlockChild>,
    },
}

/// Successful transaction geometry. Original block paths are tracked by
/// position, never by equality of attributes or duplicated block_id values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionTrace {
    pub(crate) steps: Vec<StepPathTrace>,
    original_block_paths: Vec<Vec<usize>>,
}

impl TransactionTrace {
    pub fn steps(&self) -> &[StepPathTrace] {
        &self.steps
    }
    pub(crate) fn new(doc: &BlockNode) -> Self {
        fn collect(node: &BlockNode, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            out.push(path.clone());
            for (index, child) in node.children.iter().enumerate() {
                if let Child::Block(block) = child {
                    path.push(index);
                    collect(block, path, out);
                    path.pop();
                }
            }
        }
        let mut original_block_paths = Vec::new();
        collect(doc, &mut Vec::new(), &mut original_block_paths);
        Self {
            steps: Vec::new(),
            original_block_paths,
        }
    }

    /// Map an original block to its surviving final path. Returns None for an
    /// invalid original path or a removed block, even if an identical copy was
    /// inserted. A merged right container is removed; its children may survive
    /// inside the left container. A split preserves the original as its head;
    /// the new tail has no original-block identity. Hosts choose their ID policy.
    pub fn map_block_path(&self, original: &[usize]) -> Option<Vec<usize>> {
        if !self
            .original_block_paths
            .iter()
            .any(|path| path == original)
        {
            return None;
        }
        let mut path = original.to_vec();
        for step in &self.steps {
            path = step.map(path)?;
        }
        Some(path)
    }
}

impl StepPathTrace {
    fn map(&self, mut path: Vec<usize>) -> Option<Vec<usize>> {
        match self {
            Self::Unchanged => {}
            Self::InsertChild { parent_path, index } => {
                if path.starts_with(parent_path)
                    && path.len() > parent_path.len()
                    && path[parent_path.len()] >= *index
                {
                    path[parent_path.len()] += 1;
                }
            }
            Self::DeleteChild { parent_path, index } => {
                if path.starts_with(parent_path) && path.len() > parent_path.len() {
                    let child = &mut path[parent_path.len()];
                    if *child == *index {
                        return None;
                    }
                    if *child > *index {
                        *child -= 1;
                    }
                }
            }
            Self::Merge {
                parent_path,
                index,
                left_children_before,
                coalesced_boundary,
                ..
            } => {
                if path.starts_with(parent_path) && path.len() > parent_path.len() {
                    let depth = parent_path.len();
                    if path[depth] == *index {
                        if path.len() == depth + 1 {
                            return None;
                        }
                        path[depth] -= 1;
                        path[depth + 1] +=
                            *left_children_before - if *coalesced_boundary { 1 } else { 0 };
                    } else if path[depth] > *index {
                        path[depth] -= 1;
                    }
                }
            }
            Self::Split {
                path: target,
                block_children,
                ..
            } => {
                if path.starts_with(target) && path.len() > target.len() {
                    let child = block_children
                        .iter()
                        .find(|child| child.old_index == path[target.len()])?;
                    path[target.len()] = child.new_index;
                    if child.side == SplitSide::Tail {
                        path[target.len() - 1] += 1;
                    }
                } else if let Some((&index, parent)) = target.split_last() {
                    if path.starts_with(parent)
                        && path.len() > parent.len()
                        && path[parent.len()] > index
                    {
                        path[parent.len()] += 1;
                    }
                }
            }
        }
        Some(path)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TracedTransactionReceipt {
    pub receipt: TransactionReceipt,
    pub trace: TransactionTrace,
}
