use std::{cmp::Ordering, collections::BTreeSet};

use hsscrap::{
    FolderRef, Lifecycle, MembershipId, ProjectMembership, ProjectRef, TagRef, Validate,
};
use rss_contracts::{Bounds, Error, HostRef, ItemId, Result, Revision, ScopeRef, SourceAppearance};
use serde::{Deserialize, Serialize};

use super::{host_ref, invalid, text};
use crate::{ProfileSelection, SourceLibrary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageSize {
    Items25,
    Items50,
    Items100,
}
impl PageSize {
    pub const fn get(self) -> usize {
        match self {
            Self::Items25 => 25,
            Self::Items50 => 50,
            Self::Items100 => 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WallSort {
    Newest,
    Oldest,
    SourcePriority,
    ProjectSaved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextFilter {
    AllAuthorized,
    Unplaced,
    Project {
        project: ProjectRef,
        scope: ScopeRef,
        folder: Option<FolderRef>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallFilter {
    pub search: String,
    pub tags: BTreeSet<TagRef>,
    pub unread_only: bool,
    pub bookmarked_only: bool,
}

/// OR branches preserve each selected group's rules. A directly selected source
/// has an unconditional branch. The host evaluates these over its canonical set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionBranch {
    pub source: HostRef,
    pub group: Option<HostRef>,
    pub group_keywords: Vec<String>,
    pub source_keywords: Vec<String>,
    pub group_priority: i32,
    pub prioritize: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallQuery {
    pub scope: ScopeRef,
    pub profile: HostRef,
    pub library_revision: Revision,
    pub selection_revision: Revision,
    /// Host-issued authority epoch, invalidated by account/profile revocation.
    pub authority_epoch: String,
    pub branches: Vec<SelectionBranch>,
    pub access_partitions: BTreeSet<ScopeRef>,
    pub context: ContextFilter,
    pub filter: WallFilter,
    pub sort: WallSort,
    pub page_size: PageSize,
}

impl WallQuery {
    /// Call after a canonical selection read and host authorization. IDs are not grants.
    pub fn from_library(
        library: &SourceLibrary,
        selection: &ProfileSelection,
        selection_revision: Revision,
        authority_epoch: String,
        authorized_sources: &BTreeSet<HostRef>,
        access_partitions: BTreeSet<ScopeRef>,
        focused_group: Option<&HostRef>,
        context: ContextFilter,
        filter: WallFilter,
        sort: WallSort,
        page_size: PageSize,
        bounds: &Bounds,
    ) -> Result<Self> {
        if focused_group.is_some_and(|group| !selection.selected_groups.contains(group)) {
            return Err(Error::Denied);
        }
        let sources = library.selected_sources(selection, authorized_sources)?;
        let mut branches = Vec::new();
        if focused_group.is_none() {
            for source in selection.selected_sources.intersection(&sources) {
                branches.push(SelectionBranch {
                    source: source.clone(),
                    group: None,
                    group_keywords: vec![],
                    source_keywords: vec![],
                    group_priority: 0,
                    prioritize: false,
                });
            }
        }
        for (group_ref, source) in &library.memberships {
            if !selection.selected_groups.contains(group_ref)
                || !sources.contains(source)
                || focused_group.is_some_and(|focused| focused != group_ref)
            {
                continue;
            }
            let group = library.groups.get(group_ref).ok_or(Error::NotFound)?;
            let rule = library
                .member_rules
                .get(&(group_ref.clone(), source.clone()));
            branches.push(SelectionBranch {
                source: source.clone(),
                group: Some(group_ref.clone()),
                group_keywords: group.keywords.clone(),
                source_keywords: rule.map(|r| r.keywords.clone()).unwrap_or_default(),
                group_priority: group.priority,
                prioritize: rule.is_some_and(|r| r.prioritize),
            });
        }
        let result = Self {
            scope: selection.owner_scope.clone(),
            profile: selection.profile_ref.clone(),
            library_revision: library.revision,
            selection_revision,
            authority_epoch,
            branches,
            access_partitions,
            context,
            filter,
            sort,
            page_size,
        };
        result.validate(bounds)?;
        Ok(result)
    }

    pub fn validate(&self, bounds: &Bounds) -> Result<()> {
        bounds.validate()?;
        super::encoded_bound(self, bounds)?;
        text(&self.scope.0, "query.scope", bounds.max_text_bytes)?;
        text(
            &self.authority_epoch,
            "query.authority_epoch",
            bounds.max_text_bytes,
        )?;
        host_ref(&self.profile, bounds)?;
        self.library_revision.validate()?;
        self.selection_revision.validate()?;
        if self.page_size.get() > bounds.max_page_items {
            return Err(invalid("query.page_size"));
        }
        if self.branches.len() > bounds.max_collection_items
            || self.access_partitions.len() > bounds.max_collection_items
            || self.filter.tags.len() > bounds.max_collection_items
        {
            return Err(invalid("query.collections"));
        }
        let mut unique = BTreeSet::new();
        for branch in &self.branches {
            host_ref(&branch.source, bounds)?;
            if let Some(group) = &branch.group {
                host_ref(group, bounds)?;
            }
            if !unique.insert((&branch.source, &branch.group)) {
                return Err(invalid("query.duplicate_branch"));
            }
            if branch.group.is_none()
                && (!branch.group_keywords.is_empty()
                    || !branch.source_keywords.is_empty()
                    || branch.group_priority != 0
                    || branch.prioritize)
            {
                return Err(invalid("query.direct_rule"));
            }
            for keywords in [&branch.group_keywords, &branch.source_keywords] {
                if keywords.len() > bounds.max_collection_items {
                    return Err(invalid("query.keywords"));
                }
                for keyword in keywords {
                    text(keyword, "query.keyword", bounds.max_text_bytes)?;
                }
            }
        }
        for partition in &self.access_partitions {
            text(&partition.0, "query.partition", bounds.max_text_bytes)?;
        }
        if self.filter.search.len() > bounds.max_text_bytes
            || self.filter.search.chars().any(char::is_control)
        {
            return Err(invalid("query.search"));
        }
        for tag in &self.filter.tags {
            host_ref(&tag.0, bounds)?;
        }
        if let ContextFilter::Project {
            project,
            scope,
            folder,
        } = &self.context
        {
            host_ref(&project.0, bounds)?;
            text(&scope.0, "query.context_scope", bounds.max_text_bytes)?;
            if let Some(folder) = folder {
                host_ref(&folder.0, bounds)?;
            }
        } else if self.sort == WallSort::ProjectSaved {
            return Err(invalid("query.project_saved_requires_context"));
        }
        Ok(())
    }
}

/// Normalized numeric UTC milliseconds, with unknown dates last. Publication
/// precision/offset conversion belongs to the host; never compare raw RFC3339 text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    pub priority: i32,
    pub utc_millis: Option<i64>,
    pub item_id: ItemId,
}
impl SortKey {
    pub fn compare(&self, other: &Self, sort: WallSort) -> Ordering {
        let date = match (self.utc_millis, other.utc_millis) {
            (Some(a), Some(b)) => {
                if sort == WallSort::Oldest {
                    a.cmp(&b)
                } else {
                    b.cmp(&a)
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        let priority = if sort == WallSort::SourcePriority {
            other.priority.cmp(&self.priority)
        } else {
            Ordering::Equal
        };
        priority.then(date).then(self.item_id.cmp(&other.item_id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectedContext {
    pub membership: ProjectMembership,
    pub folder: Option<FolderRef>,
}

/// One canonical item per query row. Metadata, tags and media are independently
/// authorized owner projections loaded by the wall, not a second item schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallRow {
    pub item_id: ItemId,
    pub order: SortKey,
    pub appearances: Vec<SourceAppearance>,
    /// None is explicitly no selected project context; never a global saved date.
    pub selected_context: Option<SelectedContext>,
}
impl WallRow {
    pub fn validate(&self, query: &WallQuery, bounds: &Bounds) -> Result<()> {
        if self.order.item_id != self.item_id
            || self.appearances.is_empty()
            || self.appearances.len() > bounds.max_collection_items
        {
            return Err(invalid("row.identity"));
        }
        let mut ids = BTreeSet::new();
        for appearance in &self.appearances {
            appearance.validate(bounds, &BTreeSet::new())?;
            if appearance.item_id != self.item_id
                || !query
                    .branches
                    .iter()
                    .any(|branch| branch.source == appearance.source_ref)
                || !query
                    .access_partitions
                    .contains(&appearance.access_partition)
            {
                return Err(Error::Denied);
            }
            if !ids.insert(&appearance.appearance_ref) {
                return Err(invalid("row.duplicate_appearance"));
            }
        }
        if let Some(context) = &self.selected_context {
            context.membership.validate(bounds, &BTreeSet::new())?;
            if context.membership.item_id != self.item_id
                || context.membership.lifecycle != Lifecycle::Active
            {
                return Err(Error::Denied);
            }
            match &query.context {
                ContextFilter::Project {
                    project,
                    scope,
                    folder,
                } if context.membership.project_ref == *project
                    && context.membership.scope == *scope
                    && context.folder == *folder => {}
                _ => return Err(Error::Denied),
            }
        } else if matches!(query.context, ContextFilter::Project { .. }) {
            return Err(invalid("row.selected_context"));
        }
        Ok(())
    }

    pub fn membership_id(&self) -> Option<MembershipId> {
        self.selected_context
            .as_ref()
            .map(|context| context.membership.membership_id)
    }
}
