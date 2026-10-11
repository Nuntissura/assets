use std::collections::BTreeMap;

pub type EntryId = String;
pub use cabinet_peak::Tag;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub scope: String,
    pub generation: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Folder,
    Leaf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub open: bool,
    pub create: bool,
    pub rename: bool,
    pub move_entry: bool,
    pub delete: bool,
    pub accept_children: bool,
    pub duplicate: bool,
    pub download: bool,
    pub tag: bool,
    pub color: bool,
    pub open_parent: bool,
    pub copy_reference: bool,
    pub properties: bool,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            open: true,
            create: true,
            rename: true,
            move_entry: true,
            delete: true,
            accept_children: true,
            duplicate: true,
            download: true,
            tag: true,
            color: true,
            open_parent: true,
            copy_reference: true,
            properties: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildrenState {
    Loaded,
    Unloaded,
    Loading,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Stable occurrence identity; multiple tree occurrences may point to one resource.
    pub id: EntryId,
    pub resource_id: String,
    pub parent: Option<EntryId>,
    pub label: String,
    pub kind: EntryKind,
    pub revision: u64,
    pub order: i64,
    pub color: Option<[u8; 3]>,
    pub created_at: i64,
    pub modified_at: i64,
    pub size_bytes: Option<u64>,
    pub file_type: String,
    pub tags: Vec<Tag>,
    pub capabilities: Capabilities,
    pub children: ChildrenState,
}
impl Entry {
    pub fn new(
        id: impl Into<String>,
        parent: Option<String>,
        label: impl Into<String>,
        kind: EntryKind,
    ) -> Self {
        let id = id.into();
        Self {
            resource_id: id.clone(),
            id,
            parent,
            label: label.into(),
            kind,
            revision: 0,
            order: 0,
            color: None,
            created_at: 0,
            modified_at: 0,
            size_bytes: None,
            file_type: String::new(),
            tags: Vec::new(),
            capabilities: Capabilities::default(),
            children: ChildrenState::Loaded,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub context: Context,
    pub entries: Vec<Entry>,
    /// Includes every folder needed to decide ancestry, even when leaf membership is lazy.
    pub hierarchy_complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Sort {
    Manual,
    NameAscending,
    NameDescending,
    CreatedDescending,
    CreatedAscending,
    ModifiedDescending,
    ModifiedAscending,
    FileSizeAscending,
    FileSizeDescending,
    FileTypeAscending,
    FileTypeDescending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ClickBehavior {
    SingleOpenDoubleRename,
    SelectDoubleOpen,
}

/// Host persists this value per authenticated tree scope; overrides never copy the default.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Settings {
    pub tree_sort: Sort,
    pub folder_sorts: BTreeMap<EntryId, Sort>,
    pub click_behavior: ClickBehavior,
    pub double_click_ms: u64,
    pub show_tags: bool,
    pub peak: cabinet_peak::Settings,
    /// Saved folder shortcuts. Missing IDs may be unloaded, not deleted.
    #[cfg_attr(feature = "serde", serde(default))]
    pub pinned_folders: Vec<EntryId>,
    /// Host-defined patterns; visual suppression never changes watcher behavior.
    #[cfg_attr(feature = "serde", serde(default))]
    pub hidden_patterns: Vec<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub watcher_exclusions: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            tree_sort: Sort::Manual,
            folder_sorts: BTreeMap::new(),
            click_behavior: ClickBehavior::SingleOpenDoubleRename,
            double_click_ms: 300,
            show_tags: true,
            peak: cabinet_peak::Settings::default(),
            pinned_folders: Vec::new(),
            hidden_patterns: Vec::new(),
            watcher_exclusions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRow {
    pub id: EntryId,
    pub depth: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionModifiers {
    pub toggle: bool,
    pub range: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    Into(Option<EntryId>),
    Before(EntryId),
    After(EntryId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    LoadPage {
        id: EntryId,
        cursor: Option<String>,
    },
    MovePreflight {
        ids: Vec<EntryId>,
        placement: Placement,
    },
    MoveAuthorized {
        ids: Vec<EntryId>,
        placement: Placement,
        token: String,
    },
    WorkspaceSearch {
        query: String,
    },
    Duplicate {
        id: EntryId,
    },
    Download {
        id: EntryId,
    },
    OpenParent {
        id: EntryId,
    },
    CopyPath {
        id: EntryId,
    },
    CopyLink {
        id: EntryId,
    },
    Properties {
        id: EntryId,
    },
    SetColor {
        id: EntryId,
        color: Option<[u8; 3]>,
    },
    SetTags {
        id: EntryId,
        tags: Vec<Tag>,
    },
    Open {
        id: EntryId,
    },
    LoadChildren {
        id: EntryId,
    },
    Create {
        parent: Option<EntryId>,
        name: String,
        kind: EntryKind,
    },
    Rename {
        id: EntryId,
        name: String,
    },
    Move {
        ids: Vec<EntryId>,
        placement: Placement,
    },
    Delete {
        ids: Vec<EntryId>,
    },
    Undo {
        token: String,
    },
    Redo {
        token: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub context: Context,
    pub expected_revisions: BTreeMap<EntryId, u64>,
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub request_id: u64,
    pub context: Context,
    pub result: Result<Snapshot, String>,
    /// Opaque host-owned compensation token; never invent an inverse from stale display state.
    pub undo_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    DuplicateId(String),
    MissingParent(String),
    Cycle(String),
    MissingEntry(String),
    InvalidParent(String),
    PermissionDenied(String),
    IncompleteHierarchy,
    InvalidName,
    StaleContext,
    PendingOperation,
    InvalidPlacement,
    UnknownRequest,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Canonical atomic change batch. Expected revisions bind every updated/removed occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeltaBatch {
    pub base: Context,
    pub revision: u64,
    pub expected_revisions: BTreeMap<EntryId, u64>,
    pub upserts: Vec<Entry>,
    pub removed: Vec<EntryId>,
    pub hierarchy_complete: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildPage {
    pub request_id: u64,
    pub context: Context,
    pub parent: EntryId,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub revision: u64,
    pub entries: Vec<Entry>,
}
/// Only the trusted host issues this after checking the complete authoritative hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveGrant {
    pub context: Context,
    pub ids: Vec<EntryId>,
    pub placement: Placement,
    pub token: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub id: EntryId,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    pub request_id: u64,
    pub context: Context,
    pub hits: Vec<SearchHit>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BulkProgress {
    pub request_id: u64,
    pub context: Context,
    pub completed: usize,
    pub total: usize,
    pub message: String,
    /// Host-owned durable recovery handle; never a locally invented inverse operation.
    pub recovery_token: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub loaded: usize,
    pub projected: usize,
    pub rendered: usize,
    pub projection_rebuilds: u64,
}
