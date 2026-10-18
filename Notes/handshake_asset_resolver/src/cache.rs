use std::collections::HashMap;
use crate::EmbedResolutionState;

/// Per-editor resolution cache (AC-9): keyed by asset id, so a second render of the same
/// embed reuses the terminal state instead of issuing a second fetch. Stored in
/// `RichEditorState` (owned by the shell frame) so it persists across frames — NOT inside a
/// renderer function. The renderer calls [`Self::needs_fetch`] before spawning; once a state
/// is terminal ([`EmbedResolutionState::is_terminal`]) the asset is never re-fetched.
#[derive(Debug, Default)]
pub struct EmbedResolutionCache {
    states: HashMap<String, EmbedResolutionState>,
}

impl EmbedResolutionCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached state for `asset_id`, if any.
    pub fn get(&self, asset_id: &str) -> Option<&EmbedResolutionState> {
        self.states.get(asset_id)
    }

    /// Insert / replace the state for `asset_id`.
    pub fn insert(&mut self, asset_id: impl Into<String>, state: EmbedResolutionState) {
        self.states.insert(asset_id.into(), state);
    }

    pub fn remove(&mut self, key: &str) -> Option<EmbedResolutionState> {
        self.states.remove(key)
    }

    /// Remove every workspace-local resolution when the owning editor changes workspace.
    pub fn clear(&mut self) {
        self.states.clear();
    }

    /// True when `asset_id` has NO cached state yet, OR its cached state is still `Resolving`
    /// (i.e. a fetch has not completed). A TERMINAL state (Ok/Err) returns `false` — the AC-9
    /// caching invariant: a resolved/failed asset is never re-fetched. The renderer marks the
    /// asset `Resolving` before spawning so a re-render mid-flight does not double-fetch.
    pub fn needs_fetch(&self, asset_id: &str) -> bool {
        match self.states.get(asset_id) {
            None => true,
            Some(state) => {
                !state.is_terminal() && !matches!(state, EmbedResolutionState::Resolving)
            }
        }
    }

    /// True when `asset_id` is currently marked `Resolving` (a fetch is in flight).
    pub fn is_resolving(&self, asset_id: &str) -> bool {
        matches!(
            self.states.get(asset_id),
            Some(EmbedResolutionState::Resolving)
        )
    }

    /// Number of cached entries (test/diagnostic helper).
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// True when the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}
