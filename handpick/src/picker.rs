use crate::{ByteSpan, DeliveryStatus, ResultKey, SearchItem};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultCategory { Notes, Files, Folders, Tags, Actions, Tasks, Feeds }
impl ResultCategory {
    pub const ALL: [Self; 7] = [Self::Notes, Self::Files, Self::Folders, Self::Tags, Self::Actions, Self::Tasks, Self::Feeds];
    fn index(self) -> usize { Self::ALL.iter().position(|value| *value == self).unwrap() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind { Image, Text }

/// Opaque owner reference, never an executable URL or inline media payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewReference { pub kind: PreviewKind, pub owner_ref: String }

/// Additive presentation metadata; the existing SearchItem wire shape remains intact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickerItem {
    pub item: SearchItem,
    pub category: ResultCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// UTF-8 byte spans in the label; owners compute matches and hosts convert coordinates.
    #[serde(default)]
    pub highlights: Vec<ByteSpan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewReference>,
}

/// Counts describe supplied rows and independently declared canonical coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickerCoverage {
    pub category: ResultCategory,
    pub status: DeliveryStatus,
    pub loaded: u32,
    pub total: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerGroupView {
    pub category: ResultCategory,
    pub collapsed: bool,
    pub loaded: u32,
    pub total: Option<u32>,
    pub status: DeliveryStatus,
    pub shown: u32,
    pub has_more: bool,
    pub items: Vec<PickerItem>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerView { pub groups: Vec<PickerGroupView>, pub selected: Option<ResultKey> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerError { PayloadLimit, DuplicateResult, InvalidCoverage, InvalidPreview, InvalidHighlight, MissingResult }
impl std::fmt::Display for PickerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for PickerError {}

/// Bounded presentation state. Hosts first fence and authorize retrieval before admitting it here.
#[derive(Clone, Debug)]
pub struct Picker {
    items: Vec<PickerItem>, coverage: Vec<PickerCoverage>,
    collapsed: [bool; 7], visible: [usize; 7], selected: Option<ResultKey>,
}
impl Default for Picker { fn default() -> Self { Self::new() } }
impl Picker {
    pub fn new() -> Self { Self { items: Vec::new(), coverage: Vec::new(), collapsed: [false; 7], visible: [3; 7], selected: None } }
    /// Validate the entire replacement before mutating any state.
    pub fn set_results(&mut self, items: Vec<PickerItem>, coverage: Vec<PickerCoverage>) -> Result<(), PickerError> {
        if items.len() > 1024 || coverage.len() > 7 { return Err(PickerError::PayloadLimit); }
        for (index, row) in items.iter().enumerate() {
            if items[..index].iter().any(|previous| previous.item.key == row.item.key) { return Err(PickerError::DuplicateResult); }
            let size = row.item.label.len() + row.item.detail.as_ref().map_or(0, String::len)
                + row.location.as_ref().map_or(0, String::len);
            if size > 65536 || row.item.actions.len() > 64 || row.highlights.len() > 64 { return Err(PickerError::PayloadLimit); }
            if let Some(preview) = &row.preview {
                let lower = preview.owner_ref.to_ascii_lowercase();
                if preview.owner_ref.trim().is_empty() || preview.owner_ref.len() > 256 || preview.owner_ref.chars().any(char::is_control)
                    || preview.owner_ref.contains("://") || ["data:", "blob:", "file:", "javascript:", "http:", "https:"].iter().any(|prefix| lower.starts_with(prefix)) { return Err(PickerError::InvalidPreview); }
            }
            let mut end = 0;
            for span in &row.highlights {
                if span.start < end || span.start >= span.end || span.end > row.item.label.len()
                    || !row.item.label.is_char_boundary(span.start) || !row.item.label.is_char_boundary(span.end) { return Err(PickerError::InvalidHighlight); }
                end = span.end;
            }
        }
        for (index, value) in coverage.iter().enumerate() {
            if coverage[..index].iter().any(|previous| previous.category == value.category)
                || value.loaded as usize != items.iter().filter(|row| row.category == value.category).count()
                || value.total.is_some_and(|total| total < value.loaded) { return Err(PickerError::InvalidCoverage); }
        }
        if items.iter().any(|row| !coverage.iter().any(|value| value.category == row.category)) { return Err(PickerError::InvalidCoverage); }
        self.items = items; self.coverage = coverage; self.reconcile_selection(); Ok(())
    }
    pub fn update(&mut self, items: Vec<PickerItem>, coverage: Vec<PickerCoverage>) -> Result<(), PickerError> { self.set_results(items, coverage) }
    pub fn toggle(&mut self, category: ResultCategory) {
        let index = category.index(); self.collapsed[index] = !self.collapsed[index]; self.reconcile_selection();
    }
    /// Reveal three further loaded rows; has_more can additionally signal provider paging.
    pub fn show_more(&mut self, category: ResultCategory) {
        let index = category.index(); self.collapsed[index] = false; self.visible[index] = (self.visible[index] + 3).min(1024); self.reconcile_selection();
    }
    pub fn select(&mut self, key: ResultKey) -> Result<(), PickerError> {
        if !self.visible_keys().contains(&key) { return Err(PickerError::MissingResult); }
        self.selected = Some(key); Ok(())
    }
    /// Move among result rows only, clamping at the ends; headings never receive selection.
    pub fn move_selection(&mut self, delta: i32) {
        let keys = self.visible_keys();
        if keys.is_empty() { self.selected = None; return; }
        let index = self.selected.as_ref().and_then(|key| keys.iter().position(|value| value == key)).unwrap_or(0);
        let next = (index as i64 + delta as i64).clamp(0, keys.len() as i64 - 1) as usize;
        self.selected = Some(keys[next].clone());
    }
    fn visible_keys(&self) -> Vec<ResultKey> { self.view().groups.into_iter().flat_map(|group| group.items.into_iter().map(|row| row.item.key)).collect() }
    fn reconcile_selection(&mut self) {
        let keys = self.visible_keys();
        if self.selected.as_ref().map_or(true, |selected| !keys.contains(selected)) { self.selected = keys.first().cloned(); }
    }
    pub fn view(&self) -> PickerView {
        let groups = ResultCategory::ALL.into_iter().filter_map(|category| {
            let coverage = self.coverage.iter().find(|value| value.category == category)?;
            let index = category.index();
            let items: Vec<_> = if self.collapsed[index] { Vec::new() } else {
                self.items.iter().filter(|row| row.category == category).take(self.visible[index]).cloned().collect()
            };
            let shown = items.len() as u32;
            Some(PickerGroupView { category, collapsed: self.collapsed[index], loaded: coverage.loaded,
                total: coverage.total, status: coverage.status, shown,
                has_more: !self.collapsed[index] && (shown < coverage.loaded || coverage.total.is_some_and(|total| total > coverage.loaded)), items })
        }).collect();
        PickerView { groups, selected: self.selected.clone() }
    }
}
