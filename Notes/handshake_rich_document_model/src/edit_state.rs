use super::history::UndoManager;
use super::node::BlockNode;

#[derive(Debug, Clone)]
pub struct DocumentEditState {
    document: BlockNode,
    history: UndoManager,
    edit_revision: u64,
}

#[derive(Debug)]
pub struct EditCommit<R> {
    pub output: R,
    pub content_changed: bool,
    pub edit_revision: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EditError<E> {
    StaleRevision,
    RevisionOverflow,
    Mutation(E),
}

impl DocumentEditState {
    pub fn new(document: BlockNode) -> Self {
        Self {
            document,
            history: UndoManager::new(),
            edit_revision: 0,
        }
    }

    pub fn document(&self) -> &BlockNode {
        &self.document
    }

    pub fn history(&self) -> &UndoManager {
        &self.history
    }

    pub fn edit_revision(&self) -> u64 {
        self.edit_revision
    }

    pub fn edit<R, E>(
        &mut self,
        expected_revision: u64,
        mutate: impl FnOnce(&mut BlockNode, &mut UndoManager) -> Result<R, E>,
    ) -> Result<EditCommit<R>, EditError<E>> {
        if expected_revision != self.edit_revision {
            return Err(EditError::StaleRevision);
        }
        let mut document = self.document.clone();
        let mut history = self.history.clone();
        let output = mutate(&mut document, &mut history).map_err(EditError::Mutation)?;
        let content_changed = document != self.document;
        let edit_revision = if content_changed {
            self.edit_revision
                .checked_add(1)
                .ok_or(EditError::RevisionOverflow)?
        } else {
            self.edit_revision
        };
        self.document = document;
        self.history = history;
        self.edit_revision = edit_revision;
        Ok(EditCommit {
            output,
            content_changed,
            edit_revision,
        })
    }
}
