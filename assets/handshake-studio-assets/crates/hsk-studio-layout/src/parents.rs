//! Parent pages (InDesign "master pages"): page items that document pages inherit, with per-page
//! overrides. LAYOUT-SIDE MODEL ONLY: Folio stores none of it yet (its layout module lists
//! `applied_parent`, `parent_transform`, `override_list` and `parent_page_items` as declared gaps,
//! STU-LAY-107/113), so this set lives beside the document and is keyed by Folio page keys. A
//! parent may be based on another parent (its items stack above the ancestor's; an item with the
//! same id replaces the ancestor's). A page bound to a parent shows the parent's resolved items
//! below its own; an override replaces one inherited item for that page (a local copy with its own
//! geometry, still traceable to the parent item) or suppresses it (detached and deleted). Equality
//! with InDesign's override semantics is UNVERIFIED.
use crate::{LayoutError, Rect};
use std::collections::BTreeSet;

pub const MAX_PARENTS: usize = 1024;
pub const MAX_ITEMS_PER_PARENT: usize = 10_000;
pub const MAX_BINDINGS: usize = 10_000;
pub const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentItemKind {
    /// A graphic frame / shape.
    Frame,
    /// A text frame (not threaded by this model).
    TextFrame,
    /// The current-page-number marker.
    PageNumberMarker,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParentItem {
    /// Unique inside its parent page.
    pub id: String,
    pub kind: ParentItemKind,
    /// Page coordinates, points.
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParentPage {
    pub id: String,
    pub name: String,
    pub based_on: Option<String>,
    pub items: Vec<ParentItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemOverride {
    /// Replace the inherited item `parent_item_id` on this page by `replacement` (same z position).
    Replace {
        parent_item_id: String,
        replacement: ParentItem,
    },
    /// Detach and delete the inherited item on this page.
    Suppress { parent_item_id: String },
}
impl ItemOverride {
    fn target(&self) -> &str {
        match self {
            Self::Replace { parent_item_id, .. } | Self::Suppress { parent_item_id } => {
                parent_item_id
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageBinding {
    /// Folio page key (`StudioPage::page_id`).
    pub page_id: String,
    /// `None` = no parent (the "None" master).
    pub parent_id: Option<String>,
    pub overrides: Vec<ItemOverride>,
    /// Items that exist on this page only, above every inherited item.
    pub local_items: Vec<ParentItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemSource {
    Parent { parent_id: String },
    /// A page-local replacement of the parent item `parent_id`/`item`.
    Override { parent_id: String },
    Local,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedItem {
    pub item: ParentItem,
    pub source: ItemSource,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParentSet {
    pub parents: Vec<ParentPage>,
    pub bindings: Vec<PageBinding>,
}

fn good_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID_BYTES && !s.chars().any(char::is_control)
}
fn good_item(i: &ParentItem) -> bool {
    good_id(&i.id) && i.rect.valid()
}

impl ParentSet {
    /// Checks ids, sizes, rectangles, `based_on` references and cycles, and every binding.
    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.parents.len() > MAX_PARENTS || self.bindings.len() > MAX_BINDINGS {
            return Err(LayoutError::BudgetExceeded);
        }
        let mut ids = BTreeSet::new();
        for p in &self.parents {
            if !good_id(&p.id) || p.name.len() > 256 || !ids.insert(p.id.as_str()) {
                return Err(LayoutError::InvalidInput);
            }
            if p.items.len() > MAX_ITEMS_PER_PARENT {
                return Err(LayoutError::BudgetExceeded);
            }
            let mut items = BTreeSet::new();
            for i in &p.items {
                if !good_item(i) {
                    return Err(if good_id(&i.id) {
                        LayoutError::InvalidGeometry
                    } else {
                        LayoutError::InvalidInput
                    });
                }
                if !items.insert(i.id.as_str()) {
                    return Err(LayoutError::InvalidInput);
                }
            }
        }
        for p in &self.parents {
            self.chain(&p.id)?; // missing base or cycle
        }
        let mut pages = BTreeSet::new();
        for b in &self.bindings {
            if !good_id(&b.page_id) || !pages.insert(b.page_id.as_str()) {
                return Err(LayoutError::InvalidInput);
            }
            self.resolve_binding(b)?;
        }
        Ok(())
    }

    fn parent(&self, id: &str) -> Option<&ParentPage> {
        self.parents.iter().find(|p| p.id == id)
    }

    /// `id` and its ancestors, base-most first.
    fn chain(&self, id: &str) -> Result<Vec<&ParentPage>, LayoutError> {
        let mut chain: Vec<&ParentPage> = Vec::new();
        let mut at = Some(id);
        while let Some(i) = at {
            let p = self.parent(i).ok_or(LayoutError::InvalidInput)?;
            if chain.iter().any(|c| c.id == p.id) {
                return Err(LayoutError::InvalidInput);
            }
            chain.push(p);
            at = p.based_on.as_deref();
        }
        chain.reverse();
        Ok(chain)
    }

    /// The items a parent shows: ancestors first, a same-id item of a descendant replaces the
    /// ancestor's at the ancestor's z position.
    pub fn parent_items(&self, parent_id: &str) -> Result<Vec<(ParentItem, String)>, LayoutError> {
        let mut out: Vec<(ParentItem, String)> = Vec::new();
        for p in self.chain(parent_id)? {
            for i in &p.items {
                match out.iter_mut().find(|(o, _)| o.id == i.id) {
                    Some(slot) => *slot = (i.clone(), p.id.clone()),
                    None => out.push((i.clone(), p.id.clone())),
                }
            }
        }
        Ok(out)
    }

    fn resolve_binding(&self, b: &PageBinding) -> Result<Vec<ResolvedItem>, LayoutError> {
        let mut out: Vec<ResolvedItem> = Vec::new();
        let inherited = match &b.parent_id {
            Some(id) => self.parent_items(id)?,
            None => Vec::new(),
        };
        if b.parent_id.is_none() && !b.overrides.is_empty() {
            return Err(LayoutError::InvalidInput);
        }
        if b.overrides.len() > MAX_ITEMS_PER_PARENT || b.local_items.len() > MAX_ITEMS_PER_PARENT {
            return Err(LayoutError::BudgetExceeded);
        }
        let mut targets = BTreeSet::new();
        for o in &b.overrides {
            if !targets.insert(o.target()) || !inherited.iter().any(|(i, _)| i.id == o.target()) {
                return Err(LayoutError::InvalidInput);
            }
            if let ItemOverride::Replace { replacement, .. } = o
                && !good_item(replacement)
            {
                return Err(LayoutError::InvalidGeometry);
            }
        }
        for (item, from) in inherited {
            match b.overrides.iter().find(|o| o.target() == item.id) {
                Some(ItemOverride::Suppress { .. }) => {}
                Some(ItemOverride::Replace { replacement, .. }) => out.push(ResolvedItem {
                    item: replacement.clone(),
                    source: ItemSource::Override { parent_id: from },
                }),
                None => out.push(ResolvedItem {
                    item,
                    source: ItemSource::Parent { parent_id: from },
                }),
            }
        }
        let mut local_ids = BTreeSet::new();
        for i in &b.local_items {
            if !good_item(i) || !local_ids.insert(i.id.as_str()) {
                return Err(LayoutError::InvalidInput);
            }
            out.push(ResolvedItem {
                item: i.clone(),
                source: ItemSource::Local,
            });
        }
        Ok(out)
    }

    /// The items shown on `page_id`, bottom first. A page with no binding shows nothing. Validates
    /// the whole set first so a hostile set is refused as a unit.
    pub fn resolve(&self, page_id: &str) -> Result<Vec<ResolvedItem>, LayoutError> {
        self.validate()?;
        match self.bindings.iter().find(|b| b.page_id == page_id) {
            Some(b) => self.resolve_binding(b),
            None => Ok(Vec::new()),
        }
    }
}
