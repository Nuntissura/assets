use super::*;

/// Closing hides the drawer; the host still owns and recovers the native editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuicknoteClose {
    Closed,
    NeedsSaveOrPin,
}

impl WritingSession {
    /// Rebind an unchanged native snapshot read from the canonical Notes owner.
    /// The host must verify both identities/revisions through its owner before calling.
    pub fn bind_committed(
        &mut self,
        editor: EditorId,
        native_revision: u64,
        note: NoteId,
        note_revision: RevisionId,
    ) -> Result<(), Error> {
        self.active()?;
        self.quicknote_idle()?;
        if let Some((active_editor, active_revision)) = &self.editor {
            if active_editor != &editor {
                return Err(Error::OccupiedDraft);
            }
            if *active_revision != native_revision {
                return Err(Error::Stale);
            }
        }
        if self.pins.iter().any(|pin| pin.editor == editor) {
            return Err(Error::DuplicatePin);
        }
        if self
            .bookmarks
            .iter()
            .any(|pin| pin.editor == editor && pin.revision != native_revision)
        {
            return Err(Error::Stale);
        }
        let next = self.next()?;
        self.editor = Some((editor.clone(), native_revision));
        self.committed_editor = Some((editor, native_revision, note, note_revision));
        self.generation = next;
        self.results.invalidate();
        Ok(())
    }

    pub fn bookmarks(&self) -> &[WritingPin] {
        &self.bookmarks
    }

    /// Canonical address for the current exact editor revision, supplied by its owner.
    pub fn committed_note(&self) -> Option<(&NoteId, &RevisionId)> {
        self.committed_editor
            .as_ref()
            .and_then(|(editor, revision, note, note_revision)| {
                self.editor
                    .as_ref()
                    .filter(|(active, current)| active == editor && current == revision)
                    .map(|_| (note, note_revision))
            })
    }

    /// Pin without parking, replacing, saving, or closing the active native editor.
    pub fn bookmark(&mut self, fence: &WritingFence, id: PinId) -> Result<(), Error> {
        self.validate(fence)?;
        self.quicknote_idle()?;
        if let Some(existing) = self.bookmarks.iter().find(|pin| pin.id == id) {
            return if existing.editor == fence.editor && existing.revision == fence.revision {
                Ok(())
            } else {
                Err(Error::DuplicatePin)
            };
        }
        if self
            .pins
            .iter()
            .chain(&self.bookmarks)
            .any(|pin| pin.id == id || pin.editor == fence.editor)
        {
            return Err(Error::DuplicatePin);
        }
        if self.pins.len() + self.bookmarks.len() >= self.max_pins {
            return Err(Error::PinLimit);
        }
        let next = self.next()?;
        self.bookmarks.push(WritingPin {
            id,
            editor: fence.editor.clone(),
            revision: fence.revision,
        });
        self.generation = next;
        self.results.invalidate();
        Ok(())
    }

    /// Remove only a bookmark; the host retains its native recovery or canonical Note.
    pub fn remove_bookmark(&mut self, id: &PinId) -> Result<(), Error> {
        self.active()?;
        self.quicknote_idle()?;
        let index = self
            .bookmarks
            .iter()
            .position(|pin| &pin.id == id)
            .ok_or(Error::MissingPin)?;
        let next = self.next()?;
        self.bookmarks.remove(index);
        self.generation = next;
        self.results.invalidate();
        Ok(())
    }

    /// Reopen the same handle and keep its bookmark. An occupied different draft is never overwritten.
    pub fn open_bookmark(&mut self, id: &PinId) -> Result<(), Error> {
        self.active()?;
        self.quicknote_idle()?;
        let bookmark = self
            .bookmarks
            .iter()
            .find(|pin| &pin.id == id)
            .ok_or(Error::MissingPin)?;
        if self
            .editor
            .as_ref()
            .is_some_and(|(editor, _)| editor != &bookmark.editor)
        {
            return Err(Error::OccupiedDraft);
        }
        let next = self.next()?;
        self.editor = Some((bookmark.editor.clone(), bookmark.revision));
        self.expanded = true;
        self.generation = next;
        self.results.invalidate();
        Ok(())
    }

    /// A bookmark or matching current canonical commitment permits closing the drawer.
    /// Host recovery must acknowledge the pinned revision before its native view is released.
    pub fn close_quicknote(&mut self, fence: &WritingFence) -> Result<QuicknoteClose, Error> {
        self.validate(fence)?;
        self.quicknote_idle()?;
        let bookmarked = self
            .bookmarks
            .iter()
            .any(|pin| pin.editor == fence.editor && pin.revision == fence.revision);
        let committed = self.committed_note().is_some();
        if !bookmarked && !committed {
            return Ok(QuicknoteClose::NeedsSaveOrPin);
        }
        let next = self.next()?;
        self.editor = None;
        self.expanded = false;
        self.generation = next;
        self.results.invalidate();
        Ok(QuicknoteClose::Closed)
    }

    /// Body emptiness restores ordinary search without deleting title or owner-native state.
    pub fn present_quicknote(
        &mut self,
        fence: &WritingFence,
        body_is_empty: bool,
    ) -> Result<bool, Error> {
        self.validate(fence)?;
        if self.quicknote_idle().is_err() {
            return Ok(self.expanded);
        }
        if body_is_empty && self.expanded {
            let next = self.next()?;
            self.expanded = false;
            self.generation = next;
            self.results.invalidate();
        }
        Ok(self.expanded)
    }

    fn quicknote_idle(&self) -> Result<(), Error> {
        if self.composition.is_some() || self.pending_save.is_some() {
            Err(Error::Stale)
        } else {
            Ok(())
        }
    }
}
