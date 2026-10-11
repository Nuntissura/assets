use crate::contracts::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// Host-independent presentation and command state. Canonical entries change only through validated host snapshots/deltas.
pub struct Cabinet {
    snapshot: Snapshot,
    expanded: BTreeSet<EntryId>,
    selected: HashSet<EntryId>,
    index: HashMap<EntryId, usize>,
    children_index: HashMap<Option<EntryId>, Vec<usize>>,
    sorted_children: RefCell<HashMap<Option<EntryId>, Vec<EntryId>>>,
    projection: RefCell<Option<Arc<[VisibleRow]>>>,
    projection_positions: RefCell<HashMap<EntryId, usize>>,
    projection_rebuilds: Cell<u64>,
    rendered: Cell<usize>,
    pages: HashMap<EntryId, Option<String>>,
    grants: HashMap<String, MoveGrant>,
    search_result: Option<SearchResult>,
    bulk_progress: Option<BulkProgress>,
    focused: Option<EntryId>,
    anchor: Option<EntryId>,
    filter: String,
    settings: Settings,
    pending: Option<Request>,
    error: Option<String>,
    undo: Vec<String>,
    redo: Vec<String>,
    next_request: u64,
}

impl Cabinet {
    pub fn new(snapshot: Snapshot) -> Result<Self, Error> {
        validate_snapshot(&snapshot)?;
        let (index, children_index) = build_indices(&snapshot);
        Ok(Self {
            snapshot,
            expanded: BTreeSet::new(),
            selected: HashSet::new(),
            index,
            children_index,
            sorted_children: RefCell::new(HashMap::new()),
            projection: RefCell::new(None),
            projection_positions: RefCell::new(HashMap::new()),
            projection_rebuilds: Cell::new(0),
            rendered: Cell::new(0),
            pages: HashMap::new(),
            grants: HashMap::new(),
            search_result: None,
            bulk_progress: None,
            focused: None,
            anchor: None,
            filter: String::new(),
            settings: Settings::default(),
            pending: None,
            error: None,
            undo: Vec::new(),
            redo: Vec::new(),
            next_request: 1,
        })
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.index
            .get(id)
            .map(|index| &self.snapshot.entries[*index])
    }
    pub fn filter(&self) -> &str {
        &self.filter
    }
    pub fn set_filter(&mut self, filter: &str) {
        if self.filter == filter {
            return;
        }
        self.filter = filter.to_owned();
        self.invalidate();
        let rows = self.projected_rows();
        let visible: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        self.selected.retain(|id| visible.contains(id.as_str()));
        if self
            .focused
            .as_ref()
            .is_some_and(|id| !visible.contains(id.as_str()))
        {
            self.focused = rows
                .iter()
                .find(|r| self.selected.contains(&r.id))
                .or_else(|| rows.first())
                .map(|r| r.id.clone());
        }
        if self
            .anchor
            .as_ref()
            .is_some_and(|id| !visible.contains(id.as_str()))
        {
            self.anchor = self.focused.clone();
        }
    }
    pub fn sort(&self) -> Sort {
        self.settings.tree_sort
    }
    pub fn set_sort(&mut self, sort: Sort) {
        if self.settings.tree_sort != sort {
            self.settings.tree_sort = sort;
            self.sorted_children.borrow_mut().clear();
            self.invalidate();
        }
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    pub fn apply_settings(&mut self, settings: Settings) -> Result<(), Error> {
        if !(100..=1000).contains(&settings.double_click_ms) || !settings.peak.valid() {
            return Err(Error::InvalidPlacement);
        }
        let mut pins = HashSet::new();
        for id in &settings.pinned_folders {
            if !pins.insert(id) {
                return Err(Error::DuplicateId(id.clone()));
            }
            match self.entry(id) {
                Some(e) if e.kind == EntryKind::Folder => {}
                None if !self.snapshot.hierarchy_complete => {}
                Some(_) => return Err(Error::InvalidParent(id.clone())),
                None => return Err(Error::MissingEntry(id.clone())),
            }
        }
        for id in settings.folder_sorts.keys() {
            let entry = self.entry(id);
            if entry.is_none() && !self.snapshot.hierarchy_complete {
                continue;
            }
            let entry = entry.ok_or_else(|| Error::MissingEntry(id.clone()))?;
            if entry.kind != EntryKind::Folder {
                return Err(Error::InvalidParent(id.clone()));
            }
        }
        if self.settings == settings {
            return Ok(());
        }
        if self.settings.tree_sort != settings.tree_sort {
            self.sorted_children.borrow_mut().clear();
        } else {
            let folders: HashSet<_> = self
                .settings
                .folder_sorts
                .keys()
                .chain(settings.folder_sorts.keys())
                .cloned()
                .collect();
            for id in folders {
                if self.settings.folder_sorts.get(&id) != settings.folder_sorts.get(&id) {
                    self.sorted_children.borrow_mut().remove(&Some(id));
                }
            }
        }
        let visibility_changed = self.settings.hidden_patterns != settings.hidden_patterns;
        let projection_changed = visibility_changed
            || self.settings.tree_sort != settings.tree_sort
            || self.settings.folder_sorts != settings.folder_sorts;
        self.settings = settings;
        if projection_changed {
            self.invalidate();
        }
        if visibility_changed {
            let hidden: Vec<_> = self
                .selected
                .iter()
                .filter(|id| self.hidden(id))
                .cloned()
                .collect();
            for id in hidden {
                self.selected.remove(&id);
            }
            if self.focused.as_ref().is_some_and(|id| self.hidden(id)) {
                self.focused = self.projected_rows().first().map(|r| r.id.clone());
            }
            if self.anchor.as_ref().is_some_and(|id| self.hidden(id)) {
                self.anchor = self.focused.clone();
            }
        }
        Ok(())
    }
    pub fn set_folder_sort(&mut self, id: &str, sort: Option<Sort>) -> Result<(), Error> {
        let entry = self
            .entry(id)
            .ok_or_else(|| Error::MissingEntry(id.to_owned()))?;
        if entry.kind != EntryKind::Folder {
            return Err(Error::InvalidParent(id.to_owned()));
        }
        if let Some(sort) = sort {
            self.settings.folder_sorts.insert(id.to_owned(), sort);
        } else {
            self.settings.folder_sorts.remove(id);
        }
        self.sorted_children
            .borrow_mut()
            .remove(&Some(id.to_owned()));
        self.invalidate();
        Ok(())
    }
    pub fn effective_sort(&self, parent: Option<&str>) -> Sort {
        parent
            .and_then(|id| self.settings.folder_sorts.get(id))
            .copied()
            .unwrap_or(self.settings.tree_sort)
    }
    pub fn expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }
    pub fn selected(&self) -> Vec<EntryId> {
        let _ = self.projected_rows();
        let positions = self.projection_positions.borrow();
        let mut selected: Vec<_> = self.selected.iter().cloned().collect();
        selected.sort_by(|a, b| {
            positions
                .get(a)
                .unwrap_or(&usize::MAX)
                .cmp(positions.get(b).unwrap_or(&usize::MAX))
                .then_with(|| a.cmp(b))
        });
        selected
    }
    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }
    pub fn pending(&self) -> Option<&Request> {
        self.pending.as_ref()
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn can_undo(&self) -> bool {
        self.pending.is_none() && !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        self.pending.is_none() && !self.redo.is_empty()
    }

    pub fn rows(&self) -> Vec<VisibleRow> {
        self.projected_rows().to_vec()
    }
    pub fn projected_rows(&self) -> Arc<[VisibleRow]> {
        if let Some(rows) = self.projection.borrow().as_ref() {
            return Arc::clone(rows);
        }
        let rows: Arc<[VisibleRow]> = self.build_rows().into();
        *self.projection_positions.borrow_mut() = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id.clone(), index))
            .collect();
        self.projection_rebuilds
            .set(self.projection_rebuilds.get().saturating_add(1));
        *self.projection.borrow_mut() = Some(Arc::clone(&rows));
        rows
    }
    fn invalidate(&self) {
        *self.projection.borrow_mut() = None;
    }
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.contains(id)
    }
    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            loaded: self.snapshot.entries.len(),
            projected: self.projected_rows().len(),
            rendered: self.rendered.get(),
            projection_rebuilds: self.projection_rebuilds.get(),
        }
    }
    pub fn record_rendered(&self, count: usize) {
        self.rendered.set(count);
    }
    fn build_rows(&self) -> Vec<VisibleRow> {
        let by_id: HashMap<&str, &Entry> = self
            .index
            .iter()
            .map(|(id, index)| (id.as_str(), &self.snapshot.entries[*index]))
            .collect();
        let mut children: HashMap<Option<&str>, Vec<&Entry>> = self
            .children_index
            .iter()
            .map(|(parent, indices)| {
                (
                    parent.as_deref(),
                    indices
                        .iter()
                        .map(|index| &self.snapshot.entries[*index])
                        .collect(),
                )
            })
            .collect();
        for (parent, entries) in &mut children {
            if let Some(ids) = self
                .sorted_children
                .borrow()
                .get(&parent.map(str::to_owned))
            {
                *entries = ids.iter().filter_map(|id| self.entry(id)).collect();
                continue;
            }
            entries.sort_by(|a, b| {
                let folder_first =
                    (a.kind != EntryKind::Folder).cmp(&(b.kind != EntryKind::Folder));
                folder_first
                    .then_with(|| match self.effective_sort(*parent) {
                        Sort::Manual => a.order.cmp(&b.order),
                        Sort::NameAscending => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
                        Sort::NameDescending => b.label.to_lowercase().cmp(&a.label.to_lowercase()),
                        Sort::CreatedDescending => b.created_at.cmp(&a.created_at),
                        Sort::ModifiedDescending => b.modified_at.cmp(&a.modified_at),
                        Sort::CreatedAscending => a.created_at.cmp(&b.created_at),
                        Sort::ModifiedAscending => a.modified_at.cmp(&b.modified_at),
                        Sort::FileSizeAscending => compare_size(a.size_bytes, b.size_bytes, false),
                        Sort::FileSizeDescending => compare_size(a.size_bytes, b.size_bytes, true),
                        Sort::FileTypeAscending => compare_type(a, b, false),
                        Sort::FileTypeDescending => compare_type(a, b, true),
                    })
                    .then_with(|| a.label.cmp(&b.label))
                    .then_with(|| a.id.cmp(&b.id))
            });
            self.sorted_children.borrow_mut().insert(
                parent.map(str::to_owned),
                entries.iter().map(|e| e.id.clone()).collect(),
            );
        }
        let query = self.filter.trim().to_lowercase();
        let mut included = HashSet::new();
        if !query.is_empty() {
            for entry in &self.snapshot.entries {
                if entry.label.to_lowercase().contains(&query) {
                    let mut current = Some(entry.id.as_str());
                    while let Some(id) = current {
                        if !included.insert(id) {
                            break;
                        }
                        current = by_id[id].parent.as_deref();
                    }
                }
            }
        }
        let mut stack: Vec<(&Entry, usize)> = children
            .get(&None)
            .into_iter()
            .flatten()
            .rev()
            .map(|e| (*e, 0))
            .collect();
        let mut rows = Vec::new();
        while let Some((entry, depth)) = stack.pop() {
            if self
                .settings
                .hidden_patterns
                .iter()
                .any(|pattern| path_matches(&entry.label, pattern))
            {
                continue;
            }
            if !query.is_empty() && !included.contains(entry.id.as_str()) {
                continue;
            }
            rows.push(VisibleRow {
                id: entry.id.clone(),
                depth,
            });
            if self.expanded(&entry.id) || !query.is_empty() {
                if let Some(kids) = children.get(&Some(entry.id.as_str())) {
                    stack.extend(kids.iter().rev().map(|e| (*e, depth + 1)));
                }
            }
        }
        rows
    }
    pub fn expand(&mut self, id: &str, expanded: bool) -> Result<(), Error> {
        let entry = self
            .entry(id)
            .ok_or_else(|| Error::MissingEntry(id.to_owned()))?;
        if entry.kind != EntryKind::Folder {
            return Err(Error::InvalidParent(id.to_owned()));
        }
        self.invalidate();
        if expanded {
            self.expanded.insert(id.to_owned());
        } else {
            self.expanded.remove(id);
        }
        Ok(())
    }
    pub fn select(&mut self, id: &str, modifiers: SelectionModifiers) -> Result<(), Error> {
        let rows = self.projected_rows();
        let target = rows
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| Error::MissingEntry(id.to_owned()))?;
        if modifiers.range {
            let anchor = self
                .anchor
                .as_deref()
                .and_then(|a| rows.iter().position(|r| r.id == a))
                .unwrap_or(target);
            if !modifiers.toggle {
                self.selected.clear();
            }
            for row in &rows[anchor.min(target)..=anchor.max(target)] {
                self.selected.insert(row.id.clone());
            }
            if self.anchor.is_none() {
                self.anchor = Some(id.to_owned());
            }
        } else {
            if modifiers.toggle {
                if !self.selected.remove(id) {
                    self.selected.insert(id.to_owned());
                }
            } else {
                self.selected.clear();
                self.selected.insert(id.to_owned());
            }
            self.anchor = Some(id.to_owned());
        }
        self.focused = Some(id.to_owned());
        Ok(())
    }
    pub fn navigate(
        &mut self,
        navigation: Navigation,
        modifiers: SelectionModifiers,
    ) -> Result<(), Error> {
        let rows = self.projected_rows();
        if rows.is_empty() {
            return Ok(());
        }
        let index = self
            .focused
            .as_deref()
            .and_then(|id| rows.iter().position(|r| r.id == id));
        let current = index.unwrap_or(0);
        let target = match navigation {
            Navigation::Home => 0,
            Navigation::End => rows.len() - 1,
            Navigation::Up => {
                if index.is_none() {
                    0
                } else {
                    current.saturating_sub(1)
                }
            }
            Navigation::Down => {
                if index.is_none() {
                    0
                } else {
                    (current + 1).min(rows.len() - 1)
                }
            }
            Navigation::Left => {
                let id = &rows[current].id;
                if self.expanded(id) {
                    self.expand(id, false)?;
                    return Ok(());
                }
                self.entry(id)
                    .and_then(|e| e.parent.as_deref())
                    .and_then(|p| rows.iter().position(|r| r.id == p))
                    .unwrap_or(current)
            }
            Navigation::Right => {
                let id = &rows[current].id;
                if self.entry(id).is_some_and(|e| e.kind == EntryKind::Folder) && !self.expanded(id)
                {
                    self.expand(id, true)?;
                    return self.select(id, modifiers);
                }
                if current + 1 < rows.len() && rows[current + 1].depth > rows[current].depth {
                    current + 1
                } else {
                    current
                }
            }
        };
        self.select(&rows[target].id, modifiers)
    }
    pub fn reveal(&mut self, id: &str) -> Result<(), Error> {
        self.entry(id)
            .ok_or_else(|| Error::MissingEntry(id.to_owned()))?;
        let mut ancestors = Vec::new();
        let mut parent = self.entry(id).and_then(|e| e.parent.clone());
        while let Some(p) = parent {
            parent = self.entry(&p).and_then(|e| e.parent.clone());
            ancestors.push(p);
        }
        self.expanded.extend(ancestors);
        self.invalidate();
        // Reveal must make the requested row visible even through an existing text filter.
        self.filter.clear();
        self.invalidate();
        self.select(id, SelectionModifiers::default())
    }
    fn validate_replacement(&self, next: &Snapshot) -> Result<(), Error> {
        validate_snapshot(next)?;
        if same_context(&self.snapshot.context, &next.context) {
            if next.context.revision < self.snapshot.context.revision {
                return Err(Error::StaleContext);
            }
            if next.context.revision == self.snapshot.context.revision && next != &self.snapshot {
                return Err(Error::StaleContext);
            }
            for entry in &next.entries {
                if let Some(old) = self.entry(&entry.id) {
                    if old.revision > entry.revision {
                        return Err(Error::StaleContext);
                    }
                    if old.revision == entry.revision {
                        // Lazy child loading is presentation state; all other entry fields are canonical.
                        if !canonical_equal(entry, old) {
                            return Err(Error::StaleContext);
                        }
                    }
                }
            }
        } else if next.context.scope == self.snapshot.context.scope
            && next.context.generation < self.snapshot.context.generation
        {
            return Err(Error::StaleContext);
        }
        Ok(())
    }
    fn install(&mut self, next: Snapshot) {
        let same = same_context(&self.snapshot.context, &next.context);
        let ids: HashSet<&str> = next.entries.iter().map(|e| e.id.as_str()).collect();
        if !same {
            self.sorted_children.borrow_mut().clear();
        } else {
            let mut changed = HashSet::new();
            for entry in &next.entries {
                match self.entry(&entry.id) {
                    Some(old) => {
                        if sort_inputs_changed(old, entry) {
                            changed.insert(old.parent.clone());
                            changed.insert(entry.parent.clone());
                        }
                    }
                    None => {
                        changed.insert(entry.parent.clone());
                    }
                }
            }
            for old in &self.snapshot.entries {
                if !ids.contains(old.id.as_str()) {
                    changed.insert(old.parent.clone());
                }
            }
            for parent in changed {
                self.sorted_children.borrow_mut().remove(&parent);
            }
        }
        if same {
            self.selected.retain(|id| ids.contains(id.as_str()));
            let folders: HashSet<&str> = next
                .entries
                .iter()
                .filter(|e| e.kind == EntryKind::Folder)
                .map(|e| e.id.as_str())
                .collect();
            self.settings.folder_sorts.retain(|id, _| {
                folders.contains(id.as_str())
                    || (!next.hierarchy_complete && !ids.contains(id.as_str()))
            });
            self.settings.pinned_folders.retain(|id| {
                folders.contains(id.as_str())
                    || (!next.hierarchy_complete && !ids.contains(id.as_str()))
            });
            self.expanded.retain(|id| folders.contains(id.as_str()));
            if self
                .focused
                .as_ref()
                .is_some_and(|id| !ids.contains(id.as_str()))
            {
                self.focused = None;
            }
            if self
                .anchor
                .as_ref()
                .is_some_and(|id| !ids.contains(id.as_str()))
            {
                self.anchor = None;
            }
        } else {
            self.expanded.clear();
            self.selected.clear();
            self.focused = None;
            self.anchor = None;
            self.filter.clear();
            self.settings = Settings::default();
            self.bulk_progress = None;
            self.undo.clear();
            self.redo.clear();
            self.error = None;
        }
        self.snapshot = next;
        (self.index, self.children_index) = build_indices(&self.snapshot);
        self.grants.clear();
        self.pages.clear();
        self.search_result = None;
        self.invalidate();
    }
    pub fn replace(&mut self, snapshot: Snapshot) -> Result<(), Error> {
        self.validate_replacement(&snapshot)?;
        let invalidated = if snapshot != self.snapshot {
            self.pending.take().map(|request| request.id)
        } else {
            None
        };
        self.install(snapshot);
        if let Some(id) = invalidated {
            self.error = Some(format!("Request {id} invalidated by canonical refresh; reconcile host state before retrying"));
        }
        Ok(())
    }
    // The virtual root has no entry/capabilities. Root writes are requests only: the host must
    // authorize them with its own root policy before producing a canonical completion.
    fn parent_allowed(&self, parent: Option<&str>, create: bool) -> Result<(), Error> {
        if let Some(id) = parent {
            let entry = self
                .entry(id)
                .ok_or_else(|| Error::MissingEntry(id.to_owned()))?;
            if entry.kind != EntryKind::Folder {
                return Err(Error::InvalidParent(id.to_owned()));
            }
            if !entry.capabilities.accept_children || (create && !entry.capabilities.create) {
                return Err(Error::PermissionDenied(id.to_owned()));
            }
        }
        Ok(())
    }
    fn canonical_roots(&self, ids: &[EntryId]) -> Result<Vec<EntryId>, Error> {
        if ids.is_empty() {
            return Err(Error::InvalidPlacement);
        }
        let set: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
        for id in &set {
            self.entry(id)
                .ok_or_else(|| Error::MissingEntry((*id).to_owned()))?;
        }
        let mut roots = Vec::new();
        for id in ids.iter().map(String::as_str).filter(|id| set.contains(id)) {
            let mut parent = self.entry(id).and_then(|e| e.parent.as_deref());
            let mut covered = false;
            while let Some(p) = parent {
                if set.contains(p) {
                    covered = true;
                    break;
                }
                parent = self.entry(p).and_then(|e| e.parent.as_deref());
            }
            if !covered && !roots.iter().any(|root| root == id) {
                roots.push(id.to_owned());
            }
        }
        Ok(roots)
    }
    pub fn validate_move(&self, ids: &[EntryId], placement: &Placement) -> Result<(), Error> {
        if !self.snapshot.hierarchy_complete {
            return Err(Error::IncompleteHierarchy);
        }
        self.validate_loaded_move(ids, placement)
    }
    pub fn validate_move_preflight(
        &self,
        ids: &[EntryId],
        placement: &Placement,
    ) -> Result<(), Error> {
        self.validate_loaded_move(ids, placement)
    }
    fn validate_loaded_move(&self, ids: &[EntryId], placement: &Placement) -> Result<(), Error> {
        let roots = self.canonical_roots(ids)?;
        for index in self.subtree_indices(&roots) {
            let entry = &self.snapshot.entries[index];
            if !entry.capabilities.move_entry {
                return Err(Error::PermissionDenied(entry.id.clone()));
            }
        }
        let parent = match placement {
            Placement::Into(parent) => parent.as_deref(),
            Placement::Before(id) | Placement::After(id) => {
                let target = self
                    .entry(id)
                    .ok_or_else(|| Error::MissingEntry(id.clone()))?;
                if self.effective_sort(target.parent.as_deref()) != Sort::Manual {
                    return Err(Error::InvalidPlacement);
                }
                if ids.contains(id)
                    || roots
                        .iter()
                        .any(|root| self.entry(root).is_some_and(|e| e.kind != target.kind))
                {
                    return Err(Error::InvalidPlacement);
                }
                target.parent.as_deref()
            }
        };
        self.parent_allowed(parent, false)?;
        for root in roots {
            let mut current = parent;
            while let Some(id) = current {
                if id == root {
                    return Err(Error::Cycle(root));
                }
                current = self.entry(id).and_then(|e| e.parent.as_deref());
            }
        }
        Ok(())
    }
    pub fn request(&mut self, mut operation: Operation) -> Result<Request, Error> {
        if self.pending.is_some() {
            return Err(Error::PendingOperation);
        }
        match &operation {
            Operation::LoadPage { id, cursor } => {
                let entry = self
                    .entry(id)
                    .ok_or_else(|| Error::MissingEntry(id.clone()))?;
                if entry.kind != EntryKind::Folder {
                    return Err(Error::InvalidParent(id.clone()));
                }
                if !entry.capabilities.open {
                    return Err(Error::PermissionDenied(id.clone()));
                }
                if self.pages.get(id).cloned().flatten() != *cursor {
                    return Err(Error::StaleContext);
                }
            }
            Operation::WorkspaceSearch { query } => {
                validate_name(query)?;
            }
            Operation::MovePreflight { ids, placement } => {
                self.validate_loaded_move(ids, placement)?;
            }
            Operation::MoveAuthorized {
                ids,
                placement,
                token,
            } => {
                let grant = self
                    .grants
                    .get(token)
                    .ok_or(Error::PermissionDenied(token.clone()))?;
                if grant.context != self.snapshot.context
                    || grant.ids != *ids
                    || grant.placement != *placement
                {
                    return Err(Error::StaleContext);
                }
                self.validate_loaded_move(ids, placement)?;
            }
            Operation::Duplicate { id }
            | Operation::Download { id }
            | Operation::OpenParent { id }
            | Operation::CopyPath { id }
            | Operation::CopyLink { id }
            | Operation::Properties { id }
            | Operation::SetColor { id, .. }
            | Operation::SetTags { id, .. } => {
                let entry = self
                    .entry(id)
                    .ok_or_else(|| Error::MissingEntry(id.clone()))?;
                let allowed = match &operation {
                    Operation::Duplicate { .. } => entry.capabilities.duplicate,
                    Operation::Download { .. } => entry.capabilities.download,
                    Operation::OpenParent { .. } => entry.capabilities.open_parent,
                    Operation::CopyPath { .. } | Operation::CopyLink { .. } => {
                        entry.capabilities.copy_reference
                    }
                    Operation::Properties { .. } => entry.capabilities.properties,
                    // Color decorates this occurrence; tagging resolves entry.resource_id in the host.
                    Operation::SetColor { .. } => entry.capabilities.color,
                    Operation::SetTags { tags, .. } => {
                        validate_tags(tags)?;
                        entry.capabilities.tag
                    }
                    _ => unreachable!(),
                };
                if !allowed {
                    return Err(Error::PermissionDenied(id.clone()));
                }
                if matches!(operation, Operation::Duplicate { .. }) {
                    self.parent_allowed(entry.parent.as_deref(), true)?;
                    if entry.kind == EntryKind::Folder && !self.snapshot.hierarchy_complete {
                        return Err(Error::IncompleteHierarchy);
                    }
                }
            }
            Operation::Open { id } => {
                let e = self
                    .entry(id)
                    .ok_or_else(|| Error::MissingEntry(id.clone()))?;
                if !e.capabilities.open {
                    return Err(Error::PermissionDenied(id.clone()));
                }
            }
            Operation::LoadChildren { id } => {
                let e = self
                    .entry(id)
                    .ok_or_else(|| Error::MissingEntry(id.clone()))?;
                if e.kind != EntryKind::Folder {
                    return Err(Error::InvalidParent(id.clone()));
                }
                if !e.capabilities.open {
                    return Err(Error::PermissionDenied(id.clone()));
                }
            }
            Operation::Create { parent, name, .. } => {
                validate_name(name)?;
                self.parent_allowed(parent.as_deref(), true)?;
            }
            Operation::Rename { id, name } => {
                validate_name(name)?;
                let e = self
                    .entry(id)
                    .ok_or_else(|| Error::MissingEntry(id.clone()))?;
                if !e.capabilities.rename {
                    return Err(Error::PermissionDenied(id.clone()));
                }
            }
            Operation::Move { ids, placement } => {
                self.validate_move(ids, placement)?;
            }
            Operation::Delete { ids } => {
                if !self.snapshot.hierarchy_complete {
                    return Err(Error::IncompleteHierarchy);
                }
                let roots = self.canonical_roots(ids)?;
                for index in self.subtree_indices(&roots) {
                    let entry = &self.snapshot.entries[index];
                    if !entry.capabilities.delete {
                        return Err(Error::PermissionDenied(entry.id.clone()));
                    }
                }
            }
            Operation::Undo { token } => {
                if self.undo.last() != Some(token) {
                    return Err(Error::InvalidPlacement);
                }
            }
            Operation::Redo { token } => {
                if self.redo.last() != Some(token) {
                    return Err(Error::InvalidPlacement);
                }
            }
        }
        if let Operation::Move { ids, .. }
        | Operation::Delete { ids }
        | Operation::MovePreflight { ids, .. } = &mut operation
        {
            *ids = self.canonical_roots(ids)?;
        }
        let expected_revisions = self.request_revisions(&operation);
        let request = Request {
            id: self.next_request,
            context: self.snapshot.context.clone(),
            expected_revisions,
            operation,
        };
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or(Error::InvalidPlacement)?;
        self.bulk_progress = None;
        if let Operation::MoveAuthorized { token, .. } = &request.operation {
            self.grants.remove(token);
        }
        self.pending = Some(request.clone());
        self.error = None;
        Ok(request)
    }
    pub fn request_undo(&mut self) -> Result<Request, Error> {
        let token = self.undo.last().cloned().ok_or(Error::InvalidPlacement)?;
        self.request(Operation::Undo { token })
    }
    pub fn request_redo(&mut self) -> Result<Request, Error> {
        let token = self.redo.last().cloned().ok_or(Error::InvalidPlacement)?;
        self.request(Operation::Redo { token })
    }
    /// Stop waiting locally. This does not cancel or roll back any host effect: the host must still
    /// refresh canonical state before deciding whether to retry a possibly committed operation.
    pub fn abandon_pending(&mut self, request_id: u64) -> Result<(), Error> {
        if self.pending.as_ref().map(|r| r.id) != Some(request_id) {
            return Err(Error::UnknownRequest);
        }
        self.pending = None;
        self.error = Some(
            "UI waiting abandoned; host effects are unknown until canonical refresh".to_owned(),
        );
        Ok(())
    }
    pub fn complete(&mut self, completion: Completion) -> Result<(), Error> {
        let request = self.pending.as_ref().ok_or(Error::UnknownRequest)?;
        if request.id != completion.request_id {
            return Err(Error::UnknownRequest);
        }
        if request.context != completion.context || request.context != self.snapshot.context {
            return Err(Error::StaleContext);
        }
        if let Ok(next) = &completion.result {
            if !same_context(&request.context, &next.context) {
                self.error = Some("Host completion rejected: scope/generation mismatch; refresh canonical state or abandon UI waiting".to_owned());
                return Err(Error::StaleContext);
            }
            if let Err(error) = self.validate_replacement(next) {
                self.error = Some(format!("Host completion rejected: {error}. Refresh canonical state or abandon UI waiting."));
                return Err(error);
            }
        }
        let request = self.pending.take().unwrap();
        match completion.result {
            Err(message) => {
                self.error = Some(message);
            }
            Ok(next) => {
                self.install(next);
                self.bulk_progress = None;
                self.error = None;
                match request.operation {
                    Operation::Undo { .. } => {
                        self.undo.pop();
                        if let Some(token) = completion.undo_token {
                            self.redo.push(token);
                        }
                    }
                    Operation::Redo { .. } => {
                        self.redo.pop();
                        if let Some(token) = completion.undo_token {
                            self.undo.push(token);
                        }
                    }
                    Operation::Open { .. }
                    | Operation::LoadChildren { .. }
                    | Operation::LoadPage { .. }
                    | Operation::WorkspaceSearch { .. }
                    | Operation::MovePreflight { .. }
                    | Operation::Download { .. }
                    | Operation::OpenParent { .. }
                    | Operation::CopyPath { .. }
                    | Operation::CopyLink { .. }
                    | Operation::Properties { .. } => {}
                    _ => {
                        self.redo.clear();
                        if let Some(token) = completion.undo_token {
                            self.undo.push(token);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn same_context(a: &Context, b: &Context) -> bool {
    a.scope == b.scope && a.generation == b.generation
}
fn validate_name(name: &str) -> Result<(), Error> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        Err(Error::InvalidName)
    } else {
        Ok(())
    }
}
fn validate_snapshot(snapshot: &Snapshot) -> Result<(), Error> {
    if snapshot.context.scope.trim().is_empty() {
        return Err(Error::StaleContext);
    }
    validate_entry_refs(snapshot.entries.iter())
}
fn validate_entry_refs<'a>(input: impl IntoIterator<Item = &'a Entry>) -> Result<(), Error> {
    let input: Vec<&Entry> = input.into_iter().collect();
    let mut entries = HashMap::new();
    for entry in &input {
        validate_tags(&entry.tags)?;
        if entry.resource_id.trim().is_empty() {
            return Err(Error::MissingEntry(entry.id.clone()));
        }
        if entry.id.trim().is_empty() {
            return Err(Error::MissingEntry(entry.id.clone()));
        }
        if entries.insert(entry.id.as_str(), entry).is_some() {
            return Err(Error::DuplicateId(entry.id.clone()));
        }
    }
    for entry in &input {
        if let Some(parent) = entry.parent.as_deref() {
            let p = entries
                .get(parent)
                .ok_or_else(|| Error::MissingParent(parent.to_owned()))?;
            if p.kind != EntryKind::Folder {
                return Err(Error::InvalidParent(parent.to_owned()));
            }
        }
    }
    // Walk each chain once; marked completed paths keep arbitrary-depth validation linear.
    let mut done = HashSet::new();
    for entry in &input {
        let mut path = Vec::new();
        let mut visiting = HashSet::new();
        let mut current = Some(entry.id.as_str());
        while let Some(id) = current {
            if done.contains(id) {
                break;
            }
            if !visiting.insert(id) {
                return Err(Error::Cycle(id.to_owned()));
            }
            path.push(id);
            current = entries[id].parent.as_deref();
        }
        done.extend(path);
    }
    Ok(())
}

fn compare_size(a: Option<u64>, b: Option<u64>, descending: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            if descending {
                b.cmp(&a)
            } else {
                a.cmp(&b)
            }
        }
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
    }
}
fn file_type(entry: &Entry) -> String {
    if !entry.file_type.trim().is_empty() {
        entry.file_type.trim().to_lowercase()
    } else {
        entry
            .label
            .rsplit_once('.')
            .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
            .map(|(_, ext)| ext.to_lowercase())
            .unwrap_or_default()
    }
}

fn compare_type(a: &Entry, b: &Entry, descending: bool) -> std::cmp::Ordering {
    let a = file_type(a);
    let b = file_type(b);
    match (a.is_empty(), b.is_empty()) {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => {
            if descending {
                b.cmp(&a)
            } else {
                a.cmp(&b)
            }
        }
    }
}
fn validate_tags(tags: &[Tag]) -> Result<(), Error> {
    let mut ids = HashSet::new();
    for tag in tags {
        if tag.id.trim().is_empty()
            || tag.label.trim().is_empty()
            || tag.id.chars().any(char::is_control)
            || tag.label.chars().any(char::is_control)
        {
            return Err(Error::InvalidName);
        }
        if !ids.insert(tag.id.as_str()) {
            return Err(Error::DuplicateId(tag.id.clone()));
        }
    }
    Ok(())
}

fn build_indices(
    snapshot: &Snapshot,
) -> (
    HashMap<EntryId, usize>,
    HashMap<Option<EntryId>, Vec<usize>>,
) {
    let mut index = HashMap::new();
    let mut children = HashMap::new();
    for (position, entry) in snapshot.entries.iter().enumerate() {
        index.insert(entry.id.clone(), position);
        children
            .entry(entry.parent.clone())
            .or_insert_with(Vec::new)
            .push(position);
    }
    (index, children)
}
fn path_matches(label: &str, pattern: &str) -> bool {
    // Deliberately small glob grammar: '*' matches any span, '?' one Unicode scalar.
    let text: Vec<char> = label.chars().collect();
    let pat: Vec<char> = pattern.chars().collect();
    let (mut t, mut p, mut star, mut restart) = (0, 0, None, 0);
    while t < text.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) {
            t += 1;
            p += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            p += 1;
            restart = t;
        } else if let Some(at) = star {
            restart += 1;
            t = restart;
            p = at + 1;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

impl Cabinet {
    fn hidden(&self, id: &str) -> bool {
        let mut current = Some(id);
        while let Some(id) = current {
            let Some(entry) = self.entry(id) else {
                return false;
            };
            if self
                .settings
                .hidden_patterns
                .iter()
                .any(|pattern| path_matches(&entry.label, pattern))
            {
                return true;
            }
            current = entry.parent.as_deref();
        }
        false
    }
    fn subtree_indices(&self, roots: &[EntryId]) -> Vec<usize> {
        let mut stack: Vec<usize> = roots
            .iter()
            .filter_map(|id| self.index.get(id).copied())
            .collect();
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        while let Some(index) = stack.pop() {
            if !seen.insert(index) {
                continue;
            }
            result.push(index);
            if let Some(children) = self
                .children_index
                .get(&Some(self.snapshot.entries[index].id.clone()))
            {
                stack.extend(children);
            }
        }
        result
    }
    pub fn full_path(&self, id: &str) -> Result<String, Error> {
        Ok(self
            .breadcrumbs(id)?
            .iter()
            .map(|id| self.entry(id).unwrap().label.as_str())
            .collect::<Vec<_>>()
            .join("/"))
    }
    pub fn breadcrumbs(&self, id: &str) -> Result<Vec<EntryId>, Error> {
        self.entry(id)
            .ok_or_else(|| Error::MissingEntry(id.to_owned()))?;
        let mut path = Vec::new();
        let mut current = Some(id);
        while let Some(id) = current {
            path.push(id.to_owned());
            current = self.entry(id).and_then(|e| e.parent.as_deref());
        }
        path.reverse();
        Ok(path)
    }
    pub fn pin_folder(&mut self, id: &str, pinned: bool) -> Result<(), Error> {
        let entry = self
            .entry(id)
            .ok_or_else(|| Error::MissingEntry(id.to_owned()))?;
        if entry.kind != EntryKind::Folder {
            return Err(Error::InvalidParent(id.to_owned()));
        }
        self.settings
            .pinned_folders
            .retain(|existing| existing != id);
        if pinned {
            self.settings.pinned_folders.push(id.to_owned());
        }
        Ok(())
    }
    pub fn navigate_path(&mut self, path: &str) -> Result<(), Error> {
        let mut candidates = vec![None];
        for segment in path.trim_matches('/').split('/') {
            let mut next = Vec::new();
            for parent in candidates {
                if let Some(children) = self.children_index.get(&parent) {
                    for index in children {
                        let entry = &self.snapshot.entries[*index];
                        if entry.label == segment {
                            next.push(Some(entry.id.clone()));
                        }
                    }
                }
            }
            candidates = next;
            if candidates.is_empty() {
                return Err(Error::InvalidPlacement);
            }
        }
        if candidates.len() != 1 {
            return Err(Error::InvalidPlacement);
        }
        self.reveal(candidates[0].as_deref().ok_or(Error::InvalidPlacement)?)
    }
    pub fn apply_delta(&mut self, batch: DeltaBatch) -> Result<(), Error> {
        if batch.base != self.snapshot.context || batch.revision <= batch.base.revision {
            return Err(Error::StaleContext);
        }
        for (id, revision) in &batch.expected_revisions {
            if self.entry(id).map(|e| e.revision) != Some(*revision) {
                return Err(Error::StaleContext);
            }
        }
        let mut touched = HashSet::new();
        for id in &batch.removed {
            if !touched.insert(id.as_str()) {
                return Err(Error::DuplicateId(id.clone()));
            }
            let current = self
                .entry(id)
                .ok_or_else(|| Error::MissingEntry(id.clone()))?;
            if batch.expected_revisions.get(id) != Some(&current.revision) {
                return Err(Error::StaleContext);
            }
        }
        for entry in &batch.upserts {
            if !touched.insert(entry.id.as_str()) {
                return Err(Error::DuplicateId(entry.id.clone()));
            }
            if let Some(current) = self.entry(&entry.id) {
                if batch.expected_revisions.get(&entry.id) != Some(&current.revision) {
                    return Err(Error::StaleContext);
                }
            } else if batch.expected_revisions.contains_key(&entry.id) {
                return Err(Error::StaleContext);
            }
        }
        let removed: HashSet<&str> = batch.removed.iter().map(String::as_str).collect();
        let updates: HashSet<&str> = batch.upserts.iter().map(|e| e.id.as_str()).collect();
        for entry in &batch.upserts {
            if let Some(old) = self.entry(&entry.id) {
                if entry.revision < old.revision
                    || (entry.revision == old.revision && !canonical_equal(old, entry))
                {
                    return Err(Error::StaleContext);
                }
            }
        }
        // Validate a borrowed overlay: unchanged payloads never clone during a small delta.
        validate_entry_refs(
            self.snapshot
                .entries
                .iter()
                .filter(|e| !removed.contains(e.id.as_str()) && !updates.contains(e.id.as_str()))
                .chain(batch.upserts.iter()),
        )?;
        let mut changed = HashSet::new();
        for id in &batch.removed {
            let old = self.entry(id).unwrap();
            changed.insert(old.parent.clone());
            changed.insert(Some(id.clone()));
        }
        for entry in &batch.upserts {
            match self.entry(&entry.id) {
                Some(old) => {
                    if sort_inputs_changed(old, entry) {
                        changed.insert(old.parent.clone());
                        changed.insert(entry.parent.clone());
                    }
                }
                None => {
                    changed.insert(entry.parent.clone());
                }
            }
        }
        let membership_changed = !batch.removed.is_empty()
            || batch.upserts.iter().any(|e| {
                self.entry(&e.id)
                    .is_none_or(|old| old.parent != e.parent || old.kind != e.kind)
            });
        if !membership_changed {
            for entry in batch.upserts {
                let index = self.index[&entry.id];
                self.snapshot.entries[index] = entry;
            }
        } else {
            self.snapshot
                .entries
                .retain(|entry| !removed.contains(entry.id.as_str()));
            let mut positions: HashMap<_, _> = self
                .snapshot
                .entries
                .iter()
                .enumerate()
                .map(|(index, e)| (e.id.clone(), index))
                .collect();
            for entry in batch.upserts {
                if let Some(index) = positions.get(&entry.id) {
                    self.snapshot.entries[*index] = entry;
                } else {
                    positions.insert(entry.id.clone(), self.snapshot.entries.len());
                    self.snapshot.entries.push(entry);
                }
            }
        }
        self.snapshot.context.revision = batch.revision;
        if let Some(complete) = batch.hierarchy_complete {
            self.snapshot.hierarchy_complete = complete;
        }
        if membership_changed {
            (self.index, self.children_index) = build_indices(&self.snapshot);
        }
        for parent in changed {
            self.sorted_children.borrow_mut().remove(&parent);
        }
        self.selected.retain(|id| self.index.contains_key(id));
        let folders: HashSet<_> = self
            .snapshot
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Folder)
            .map(|e| e.id.as_str())
            .collect();
        self.expanded.retain(|id| folders.contains(id.as_str()));
        let complete = self.snapshot.hierarchy_complete;
        self.settings.folder_sorts.retain(|id, _| {
            folders.contains(id.as_str()) || (!complete && !self.index.contains_key(id))
        });
        self.settings.pinned_folders.retain(|id| {
            folders.contains(id.as_str()) || (!complete && !self.index.contains_key(id))
        });
        for id in batch.removed {
            self.settings.folder_sorts.remove(&id);
            self.settings.pinned_folders.retain(|p| p != &id);
            self.pages.remove(&id);
        }
        if self
            .focused
            .as_ref()
            .is_some_and(|id| !self.index.contains_key(id))
        {
            self.focused = None;
        }
        if self
            .anchor
            .as_ref()
            .is_some_and(|id| !self.index.contains_key(id))
        {
            self.anchor = None;
        }
        if let Some(request) = self.pending.take() {
            self.error = Some(format!(
                "Request {} invalidated by delta; reconcile host state before retrying",
                request.id
            ));
        }
        self.grants.clear();
        self.search_result = None;
        self.invalidate();
        Ok(())
    }
    pub fn next_page_cursor(&self, id: &str) -> Option<&str> {
        self.pages.get(id).and_then(|c| c.as_deref())
    }
    pub fn request_child_page(&mut self, id: &str) -> Result<Request, Error> {
        let cursor = self.pages.get(id).cloned().flatten();
        self.request(Operation::LoadPage {
            id: id.to_owned(),
            cursor,
        })
    }
    pub fn complete_child_page(&mut self, page: ChildPage) -> Result<(), Error> {
        let request = self.pending.as_ref().ok_or(Error::UnknownRequest)?;
        if request.id != page.request_id {
            return Err(Error::UnknownRequest);
        }
        if request.context != page.context || page.context != self.snapshot.context {
            return Err(Error::StaleContext);
        }
        match &request.operation {
            Operation::LoadPage { id, cursor } if id == &page.parent && cursor == &page.cursor => {}
            _ => return Err(Error::InvalidPlacement),
        }
        if page.next_cursor == page.cursor && page.next_cursor.is_some() {
            return Err(Error::InvalidPlacement);
        }
        if page
            .entries
            .iter()
            .any(|e| e.parent.as_deref() != Some(page.parent.as_str()))
        {
            return Err(Error::InvalidParent(page.parent));
        }
        let mut expected = BTreeMap::new();
        for e in &page.entries {
            if let Some(old) = self.entry(&e.id) {
                expected.insert(e.id.clone(), old.revision);
            }
        }
        let mut parent = self.entry(&page.parent).unwrap().clone();
        parent.children = if page.next_cursor.is_some() {
            ChildrenState::Unloaded
        } else {
            ChildrenState::Loaded
        };
        expected.insert(parent.id.clone(), parent.revision);
        let mut entries = page.entries;
        entries.push(parent);
        let old_pages = self.pages.clone();
        self.apply_delta(DeltaBatch {
            base: page.context,
            revision: page.revision,
            expected_revisions: expected,
            upserts: entries,
            removed: Vec::new(),
            hierarchy_complete: None,
        })?;
        self.pages = old_pages;
        self.pages.insert(page.parent, page.next_cursor);
        self.error = None;
        Ok(())
    }
    pub fn fail_child_page(&mut self, request_id: u64, message: String) -> Result<(), Error> {
        let request = self.pending.as_ref().ok_or(Error::UnknownRequest)?;
        if request.id != request_id {
            return Err(Error::UnknownRequest);
        }
        let id = match &request.operation {
            Operation::LoadPage { id, .. } => id.clone(),
            _ => return Err(Error::InvalidPlacement),
        };
        let index = *self
            .index
            .get(&id)
            .ok_or_else(|| Error::MissingEntry(id.clone()))?;
        self.snapshot.entries[index].children = ChildrenState::Failed(message.clone());
        self.pending = None;
        self.error = Some(message);
        self.invalidate();
        Ok(())
    }

    pub fn accept_move_preflight(
        &mut self,
        request_id: u64,
        grant: MoveGrant,
    ) -> Result<(), Error> {
        let request = self.pending.as_ref().ok_or(Error::UnknownRequest)?;
        if request.id != request_id {
            return Err(Error::UnknownRequest);
        }
        if grant.token.trim().is_empty()
            || grant.context != request.context
            || grant.context != self.snapshot.context
        {
            return Err(Error::StaleContext);
        }
        match &request.operation {
            Operation::MovePreflight { ids, placement }
                if ids == &grant.ids && placement == &grant.placement => {}
            _ => return Err(Error::InvalidPlacement),
        }
        self.validate_loaded_move(&grant.ids, &grant.placement)?;
        self.pending = None;
        self.grants.insert(grant.token.clone(), grant);
        self.error = None;
        Ok(())
    }
    pub fn complete_search(&mut self, result: SearchResult) -> Result<(), Error> {
        let request = self.pending.as_ref().ok_or(Error::UnknownRequest)?;
        if request.id != result.request_id {
            return Err(Error::UnknownRequest);
        }
        if result.context != request.context || result.context != self.snapshot.context {
            return Err(Error::StaleContext);
        }
        if !matches!(request.operation, Operation::WorkspaceSearch { .. }) {
            return Err(Error::InvalidPlacement);
        }
        let mut seen = HashSet::new();
        for hit in &result.hits {
            if hit.id.trim().is_empty() || !seen.insert(&hit.id) {
                return Err(Error::DuplicateId(hit.id.clone()));
            }
        }
        self.pending = None;
        self.search_result = Some(result);
        Ok(())
    }
    pub fn approved_moves(&self) -> Vec<MoveGrant> {
        self.grants.values().cloned().collect()
    }
    pub fn search_result(&self) -> Option<&SearchResult> {
        self.search_result.as_ref()
    }
    pub fn bulk_progress(&self) -> Option<&BulkProgress> {
        self.bulk_progress.as_ref()
    }
    pub fn update_bulk_progress(&mut self, progress: BulkProgress) -> Result<(), Error> {
        let request = self.pending.as_ref().ok_or(Error::UnknownRequest)?;
        if request.id != progress.request_id {
            return Err(Error::UnknownRequest);
        }
        if request.context != progress.context || progress.context != self.snapshot.context {
            return Err(Error::StaleContext);
        }
        if progress.completed > progress.total
            || self.bulk_progress.as_ref().is_some_and(|old| {
                old.completed > progress.completed || old.total != progress.total
            })
        {
            return Err(Error::InvalidPlacement);
        }
        if !matches!(
            request.operation,
            Operation::Delete { .. }
                | Operation::Move { .. }
                | Operation::MoveAuthorized { .. }
                | Operation::Duplicate { .. }
                | Operation::Download { .. }
        ) {
            return Err(Error::InvalidPlacement);
        }
        self.bulk_progress = Some(progress);
        Ok(())
    }
}

impl Cabinet {
    // Cache this context-bound list when the dialog opens; the request rechecks all rights.
    #[cfg(feature = "egui")]
    pub(crate) fn move_destinations(&self, id: &str) -> Vec<(Option<EntryId>, bool)> {
        let source = vec![id.to_owned()];
        let allowed = self
            .validate_loaded_move(&source, &Placement::Into(None))
            .is_ok();
        let excluded: HashSet<usize> = self.subtree_indices(&source).into_iter().collect();
        let mut result = vec![(None, allowed)];
        result.extend(
            self.snapshot
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.kind == EntryKind::Folder)
                .map(|(index, e)| {
                    (
                        Some(e.id.clone()),
                        allowed && e.capabilities.accept_children && !excluded.contains(&index),
                    )
                }),
        );
        result
    }
    fn request_revisions(&self, operation: &Operation) -> BTreeMap<EntryId, u64> {
        let mut ids = HashSet::new();
        match operation {
            Operation::Open { id }
            | Operation::LoadChildren { id }
            | Operation::LoadPage { id, .. }
            | Operation::Download { id }
            | Operation::OpenParent { id }
            | Operation::CopyPath { id }
            | Operation::CopyLink { id }
            | Operation::Properties { id }
            | Operation::Rename { id, .. }
            | Operation::SetColor { id, .. }
            | Operation::SetTags { id, .. } => {
                ids.insert(id.clone());
            }
            Operation::Create { parent, .. } => {
                if let Some(parent) = parent {
                    ids.insert(parent.clone());
                }
            }
            Operation::Duplicate { id } => {
                for index in self.subtree_indices(std::slice::from_ref(id)) {
                    ids.insert(self.snapshot.entries[index].id.clone());
                }
                if let Some(parent) = self.entry(id).and_then(|e| e.parent.clone()) {
                    ids.insert(parent);
                }
            }
            Operation::Delete { ids: sources } => {
                for index in self.subtree_indices(sources) {
                    let entry = &self.snapshot.entries[index];
                    ids.insert(entry.id.clone());
                    if let Some(parent) = &entry.parent {
                        ids.insert(parent.clone());
                    }
                }
            }
            Operation::Move {
                ids: sources,
                placement,
            }
            | Operation::MovePreflight {
                ids: sources,
                placement,
            }
            | Operation::MoveAuthorized {
                ids: sources,
                placement,
                ..
            } => {
                for index in self.subtree_indices(sources) {
                    let entry = &self.snapshot.entries[index];
                    ids.insert(entry.id.clone());
                    if let Some(parent) = &entry.parent {
                        ids.insert(parent.clone());
                    }
                }
                match placement {
                    Placement::Into(parent) => {
                        if let Some(parent) = parent {
                            ids.insert(parent.clone());
                        }
                    }
                    Placement::Before(id) | Placement::After(id) => {
                        ids.insert(id.clone());
                        if let Some(parent) = self.entry(id).and_then(|e| e.parent.clone()) {
                            ids.insert(parent);
                        }
                    }
                }
            }
            Operation::WorkspaceSearch { .. } | Operation::Undo { .. } | Operation::Redo { .. } => {
            }
        }
        ids.into_iter()
            .filter_map(|id| self.entry(&id).map(|e| (id.clone(), e.revision)))
            .collect()
    }
    /// Acknowledge an effect-free host operation without copying the canonical snapshot.
    pub fn acknowledge(&mut self, request_id: u64, context: &Context) -> Result<(), Error> {
        let pending = self.pending.as_ref().ok_or(Error::UnknownRequest)?;
        if pending.id != request_id {
            return Err(Error::UnknownRequest);
        }
        if &pending.context != context || context != &self.snapshot.context {
            return Err(Error::StaleContext);
        }
        if !matches!(
            pending.operation,
            Operation::Open { .. }
                | Operation::Download { .. }
                | Operation::OpenParent { .. }
                | Operation::CopyPath { .. }
                | Operation::CopyLink { .. }
                | Operation::Properties { .. }
        ) {
            return Err(Error::InvalidPlacement);
        }
        self.pending = None;
        self.bulk_progress = None;
        self.error = None;
        Ok(())
    }
}

fn canonical_equal(a: &Entry, b: &Entry) -> bool {
    // Exhaustive binding forces new canonical fields to participate when Entry evolves.
    let Entry {
        id,
        resource_id,
        parent,
        label,
        kind,
        revision,
        order,
        color,
        created_at,
        modified_at,
        size_bytes,
        file_type,
        tags,
        capabilities,
        children: _,
    } = a;
    id == &b.id
        && resource_id == &b.resource_id
        && parent == &b.parent
        && label == &b.label
        && kind == &b.kind
        && revision == &b.revision
        && order == &b.order
        && color == &b.color
        && created_at == &b.created_at
        && modified_at == &b.modified_at
        && size_bytes == &b.size_bytes
        && file_type == &b.file_type
        && tags == &b.tags
        && capabilities == &b.capabilities
}
fn sort_inputs_changed(a: &Entry, b: &Entry) -> bool {
    a.parent != b.parent
        || a.kind != b.kind
        || a.label != b.label
        || a.order != b.order
        || a.created_at != b.created_at
        || a.modified_at != b.modified_at
        || a.size_bytes != b.size_bytes
        || a.file_type != b.file_type
}
