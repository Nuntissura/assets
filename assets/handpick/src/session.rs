use crate::*;

/// Host-selected resource ceilings, measured in UTF-8 bytes and item counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum bytes per draft or pin; total pin text is bounded by this times max_pins.
    pub max_text_bytes: usize,
    /// Maximum parked drafts.
    pub max_pins: usize,
    /// Maximum items per delivery channel.
    pub max_items: usize,
    /// Maximum combined label/detail bytes per item.
    pub max_item_bytes: usize,
    /// Maximum action descriptors per item.
    pub max_actions_per_item: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_counters_never_wrap_or_prevent_revocation() {
        let mut session = Session::new(
            ContextId::new("private").unwrap(),
            Limits {
                max_text_bytes: 16,
                max_pins: 1,
                max_items: 1,
                max_item_bytes: 16,
                max_actions_per_item: 1,
            },
        );
        session.set_draft("secret".into()).unwrap();
        session.generation = u64::MAX;
        assert_eq!(
            session.set_draft("replacement".into()),
            Err(Error::CounterExhausted)
        );
        assert_eq!(session.draft(), "secret");
        assert_eq!(session.begin(), Err(Error::CounterExhausted));
        assert_eq!(
            session.switch_context(ContextId::new("work").unwrap()),
            Err(Error::CounterExhausted)
        );
        assert!(session.draft().is_empty());
        assert_eq!(session.begin(), Err(Error::Revoked));
    }
}

/// Observable rejection. Failed operations preserve state unless explicitly revoking it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Revoked,
    Stale,
    TextLimit,
    PinLimit,
    OccupiedDraft,
    DuplicatePin,
    MissingPin,
    InvalidSpan,
    PayloadLimit,
    DuplicateResult,
    CounterExhausted,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Parked draft. Render its identity along the TOP edge; expose text ONLY in a hover/focus tooltip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    id: PinId,
    text: String,
}
impl Pin {
    /// Bookmark identity, separate from its hidden excerpt.
    pub fn id(&self) -> &PinId {
        &self.id
    }
    /// Original text for hover/focus tooltip and restoration; never a bookmark label.
    pub fn tooltip_text(&self) -> &str {
        &self.text
    }
}

/// Current channel state; no persistence or remote coverage guarantee is implied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    status: DeliveryStatus,
    items: Vec<SearchItem>,
}
impl Delivery {
    /// Observable status; callers must not treat errors or pending work as empty success.
    pub fn status(&self) -> DeliveryStatus {
        self.status
    }
    /// Authorized supplied items, bounded by session admission.
    pub fn items(&self) -> &[SearchItem] {
        &self.items
    }
}

/// Single owner of bounded in-memory draft, pins and asynchronous delivery state.
/// Context switches clear private state; hosts own current grants, durable storage and effects.
#[derive(Debug)]
pub struct Session {
    context: ContextId,
    active: bool,
    limits: Limits,
    draft: String,
    revision: u64,
    generation: u64,
    pins: Vec<Pin>,
    suggestions: Delivery,
    search: Delivery,
    selected: Option<ResultKey>,
}
impl Session {
    /// Start an empty authorized session with explicit host-selected budgets.
    pub fn new(context: ContextId, limits: Limits) -> Self {
        Self {
            context,
            active: true,
            limits,
            draft: String::new(),
            revision: 0,
            generation: 0,
            pins: Vec::new(),
            suggestions: Self::pending(),
            search: Self::pending(),
            selected: None,
        }
    }
    fn pending() -> Delivery {
        Delivery {
            status: DeliveryStatus::Pending,
            items: Vec::new(),
        }
    }
    fn require_active(&self) -> Result<(), Error> {
        if self.active {
            Ok(())
        } else {
            Err(Error::Revoked)
        }
    }
    fn next(&self) -> Result<(u64, u64), Error> {
        Ok((
            self.generation
                .checked_add(1)
                .ok_or(Error::CounterExhausted)?,
            self.revision
                .checked_add(1)
                .ok_or(Error::CounterExhausted)?,
        ))
    }
    pub(crate) fn invalidate(&mut self) {
        self.suggestions = Self::pending();
        self.search = Self::pending();
        self.selected = None;
    }
    fn replace_draft(&mut self, text: String, counters: (u64, u64)) {
        self.draft = text;
        self.generation = counters.0;
        self.revision = counters.1;
        self.invalidate();
    }
    fn validate_text(&self, text: &str) -> Result<(), Error> {
        if text.len() > self.limits.max_text_bytes {
            Err(Error::TextLimit)
        } else {
            Ok(())
        }
    }
    fn validate_pin_id(&self, id: &PinId) -> Result<(), Error> {
        if self.pins.iter().any(|p| &p.id == id) {
            Err(Error::DuplicatePin)
        } else {
            Ok(())
        }
    }
    /// Original unsaved text. This core never claims durable capture.
    pub fn draft(&self) -> &str {
        &self.draft
    }
    /// Current draft revision.
    pub fn draft_revision(&self) -> u64 {
        self.revision
    }
    /// Top-strip bookmarks; render excerpts only through hover/focus tooltips.
    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }
    /// Read a delivery channel without merging suggestions with content results.
    pub fn delivery(&self, channel: Channel) -> &Delivery {
        match channel {
            Channel::Suggestions => &self.suggestions,
            Channel::Search => &self.search,
        }
    }
    /// Change literal text atomically; oversize inputs leave existing text and pins untouched.
    pub fn set_draft(&mut self, text: String) -> Result<(), Error> {
        self.require_active()?;
        self.validate_text(&text)?;
        let counters = self.next()?;
        self.replace_draft(text, counters);
        Ok(())
    }
    /// Revoke current access immediately, even if counters are exhausted. Clears all private state.
    pub fn revoke(&mut self) {
        self.active = false;
        self.draft.clear();
        self.pins.clear();
        self.invalidate();
    }
    /// Enter a freshly authorized context, including a renewed grant for the same identity.
    /// On counter exhaustion the session remains revoked, with no old private state exposed.
    pub fn switch_context(&mut self, context: ContextId) -> Result<(), Error> {
        self.revoke();
        let counters = self.next()?;
        self.context = context;
        self.active = true;
        self.replace_draft(String::new(), counters);
        Ok(())
    }
    /// Park the draft and clear the field atomically. Hosts supply IDs unique across their lifecycle.
    pub fn pin(&mut self, id: PinId) -> Result<(), Error> {
        self.require_active()?;
        self.validate_pin_id(&id)?;
        if self.pins.len() >= self.limits.max_pins {
            return Err(Error::PinLimit);
        }
        let counters = self.next()?;
        let text = std::mem::take(&mut self.draft);
        self.pins.push(Pin { id, text });
        self.replace_draft(String::new(), counters);
        Ok(())
    }
    /// Restore a pin. An occupied field requires a fresh ID to park its text in the freed slot.
    /// All checks precede mutation, so missing/duplicate IDs cannot overwrite either draft.
    pub fn restore(&mut self, id: &PinId, occupied_pin: Option<PinId>) -> Result<(), Error> {
        self.require_active()?;
        let index = self
            .pins
            .iter()
            .position(|p| &p.id == id)
            .ok_or(Error::MissingPin)?;
        if !self.draft.is_empty() {
            let replacement = occupied_pin.as_ref().ok_or(Error::OccupiedDraft)?;
            self.validate_pin_id(replacement)?;
        }
        let counters = self.next()?;
        let restored = self.pins.remove(index).text;
        if !self.draft.is_empty() {
            let text = std::mem::take(&mut self.draft);
            // Validated above before taking either text; count stays within the existing capacity.
            self.pins.insert(
                index,
                Pin {
                    id: occupied_pin.expect("occupied draft ID validated"),
                    text,
                },
            );
        }
        self.replace_draft(restored, counters);
        Ok(())
    }
    /// Dispatch a new search/suggestion round, clearing old selection and fencing all older rounds.
    pub fn begin(&mut self) -> Result<Fence, Error> {
        self.require_active()?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(Error::CounterExhausted)?;
        self.invalidate();
        Ok(self.fence())
    }
    pub(crate) fn fence(&self) -> Fence {
        Fence {
            context: self.context.clone(),
            generation: self.generation,
            draft_revision: self.revision,
        }
    }
    fn check_fence(&self, fence: &Fence) -> Result<(), Error> {
        self.require_active()?;
        if *fence == self.fence() {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    }
    /// Admit a delivery only for the current context/round/draft; reject oversize or duplicate rows.
    /// Hosts must authorize the entire payload before calling this method.
    pub fn deliver(
        &mut self,
        fence: &Fence,
        channel: Channel,
        status: DeliveryStatus,
        items: Vec<SearchItem>,
    ) -> Result<(), Error> {
        self.check_fence(fence)?;
        if items.len() > self.limits.max_items {
            return Err(Error::PayloadLimit);
        }
        for (index, item) in items.iter().enumerate() {
            let size = item
                .label
                .len()
                .checked_add(item.detail.as_ref().map_or(0, String::len))
                .ok_or(Error::PayloadLimit)?;
            if size > self.limits.max_item_bytes
                || item.actions.len() > self.limits.max_actions_per_item
            {
                return Err(Error::PayloadLimit);
            }
            if items[..index]
                .iter()
                .any(|previous| previous.key == item.key)
            {
                return Err(Error::DuplicateResult);
            }
        }
        let delivery = Delivery { status, items };
        match channel {
            Channel::Suggestions => self.suggestions = delivery,
            Channel::Search => self.search = delivery,
        }
        if let Some(key) = &self.selected {
            if !self
                .suggestions
                .items
                .iter()
                .chain(&self.search.items)
                .any(|item| &item.key == key)
            {
                self.selected = None;
            }
        }
        Ok(())
    }
    /// Select a stable existing identity; no action executes here.
    pub fn select(&mut self, key: ResultKey) -> Result<(), Error> {
        self.require_active()?;
        if !self
            .suggestions
            .items
            .iter()
            .chain(&self.search.items)
            .any(|item| item.key == key)
        {
            return Err(Error::Stale);
        }
        self.selected = Some(key);
        Ok(())
    }
    /// Current selected identity; frontend row reordering cannot retarget it.
    pub fn selected(&self) -> Option<&ResultKey> {
        self.selected.as_ref()
    }
    /// Apply a literal completion atomically; stale drafts and non-UTF-8 boundaries are rejected.
    /// Returns the caret position as a UTF-8 byte offset. Grapheme editing policy remains host-owned.
    pub fn complete(&mut self, edit: CompletionEdit) -> Result<usize, Error> {
        self.check_fence(&edit.fence)?;
        let ByteSpan { start, end } = edit.span;
        if start > end
            || end > self.draft.len()
            || !self.draft.is_char_boundary(start)
            || !self.draft.is_char_boundary(end)
        {
            return Err(Error::InvalidSpan);
        }
        let size = (self.draft.len() - (end - start))
            .checked_add(edit.replacement.len())
            .ok_or(Error::TextLimit)?;
        if size > self.limits.max_text_bytes {
            return Err(Error::TextLimit);
        }
        let counters = self.next()?;
        let caret = start + edit.replacement.len();
        let mut text = String::with_capacity(size);
        text.push_str(&self.draft[..start]);
        text.push_str(&edit.replacement);
        text.push_str(&self.draft[end..]);
        self.replace_draft(text, counters);
        Ok(caret)
    }
}
