//! Parent-page records (`hsk.studio.parent_page_records@1`), added by SMB-FOLIO-PARENT-PAGES.
//! Layout (SMB-LAYOUT-TYPESET) models InDesign "master pages" on its side and listed
//! `applied_parent`, `override_list` and `parent_page_items` as Folio gaps (STU-LAY-107/113); this
//! record set stores them beside the document, like the other per-document record sets
//! (`StudioDocument` gains no fields, so every existing document stays byte-identical).
//!
//! Members: per parent page a document-local `parent_id` (no Accord prefix is declared), a name,
//! an optional `based_on` parent (parents based on parents; a loop, including self, is
//! `RenderCycle`) and its `parent_page_items` (an item is a document layer, `layer_id` an `SLYR`
//! id; the layer carries geometry and frame data, nothing is duplicated here). Per document page a
//! binding holds the `applied_parent` (absent = the "None" parent) and the `override_list`: a
//! `replace` (page-local replacement layer) or `suppress` of an inherited item, each naming the
//! parent item by `parent_item_id`. Resolution: the items of the applied parent and of all its
//! ancestors are inheritable; an override naming an item outside that chain is `DanglingReference`.
//!
//! Declared gaps, not invented here: page-local items beyond overrides, `parent_transform`, and
//! InDesign override equality semantics (UNVERIFIED, D5 oracle). The layout adapter that maps this
//! set onto `hsk_studio_layout::ParentSet` is a follow-up lane.
//!
//! Hostile wire JSON: integer arithmetic is denied (workflow V4-01); records are charged through
//! the checked `Ctx` budget.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, ResolutionIssue, Resolver, Result, StudioDocument, canceled, err, id,
    values::{Ctx, label},
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(ParentPageSchema, "hsk.studio.parent_page@1");
schema_token!(ParentPageRecordsSchema, "hsk.studio.parent_page_records@1");

/// Parent pages per record set (a Folio hard ceiling; equals layout `MAX_PARENTS`).
pub const MAX_PARENT_PAGES: usize = 1024;
/// Items per parent page (equals layout `MAX_ITEMS_PER_PARENT`).
pub const MAX_PARENT_PAGE_ITEMS: usize = 10_000;
/// Page bindings per record set (equals layout `MAX_BINDINGS`).
pub const MAX_PAGE_BINDINGS: usize = 10_000;

/// One item of a parent page: a document layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioParentPageItem {
    /// Unique inside its parent page.
    pub item_id: String,
    /// `SLYR` id of a document layer.
    pub layer_id: String,
}

/// One parent page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioParentPage {
    pub schema_id: ParentPageSchema,
    /// Document-local key, unique in the set.
    pub parent_id: String,
    pub name: String,
    /// Parent this one is based on; its items are inherited below this page's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_page_items: Vec<StudioParentPageItem>,
}

/// A per-page override of one inherited item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "override_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioItemOverride {
    /// Replace the inherited item by a page-local document layer.
    Replace {
        parent_item_id: String,
        replacement_layer_id: String,
    },
    /// Detach and delete the inherited item on this page.
    Suppress { parent_item_id: String },
}
impl StudioItemOverride {
    /// The inherited item this override names.
    pub fn parent_item_id(&self) -> &str {
        match self {
            Self::Replace { parent_item_id, .. } | Self::Suppress { parent_item_id } => {
                parent_item_id
            }
        }
    }
}

/// The parent a document page applies, with its overrides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPageParentBinding {
    /// `StudioPage::page_id` of a page in the document.
    pub page_id: String,
    /// Absent = no parent (the "None" parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_parent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub override_list: Vec<StudioItemOverride>,
}

/// The parent pages and page bindings of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StudioParentPageRecords {
    pub schema_id: ParentPageRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<StudioParentPage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_bindings: Vec<StudioPageParentBinding>,
}
impl StudioParentPageRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: ParentPageRecordsSchema::V1,
            document_id: document_id.into(),
            parents: Vec::new(),
            page_bindings: Vec::new(),
        }
    }
    /// The parent page with this key.
    pub fn parent(&self, parent_id: &str) -> Option<&StudioParentPage> {
        self.parents.iter().find(|p| p.parent_id == parent_id)
    }
    /// The binding of this document page.
    pub fn binding(&self, page_id: &str) -> Option<&StudioPageParentBinding> {
        self.page_bindings.iter().find(|b| b.page_id == page_id)
    }
}

/// JSON schema (draft 2020-12) of the parent-page record set.
pub fn parent_page_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioParentPageRecords>()
}

/// The single validator: ids and document binding, layer references (`DanglingReference`), unique
/// parents and items (`DuplicateIdentity`; one document layer is at most one parent item or
/// replacement), `based_on` references (`DanglingReference`) and loops (`RenderCycle`), bindings
/// of pages the document holds, applied parents the set holds, and overrides that name an item the
/// applied parent chain does not hold (`DanglingReference`; an override without an applied parent
/// is `InvalidField`; two overrides of one item are `DuplicateIdentity`). Records count against
/// `Budget::nodes`, names against `Budget::payload_bytes`.
pub fn validate_parent_page_records(
    r: &StudioParentPageRecords,
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    let mut cx = Ctx::new(b, token, resolver);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.parents.len() > MAX_PARENT_PAGES {
        return Err(err(Code::BudgetExceeded, "parents"));
    }
    if r.page_bindings.len() > MAX_PAGE_BINDINGS {
        return Err(err(Code::BudgetExceeded, "page_bindings"));
    }
    cx.nodes(r.parents.len(), "parents")?;
    cx.nodes(r.page_bindings.len(), "page_bindings")?;
    let doc_layers: BTreeSet<&str> = d.layers.iter().map(|l| l.layer_id.as_str()).collect();
    let layer_ref = |layer: &str, target: String| -> Result<()> {
        id(layer, "SLYR", &target)?;
        if doc_layers.contains(layer) {
            Ok(())
        } else {
            Err(err(Code::DanglingReference, target))
        }
    };
    let mut used_layers: BTreeSet<&str> = BTreeSet::new();
    let mut by_id: BTreeMap<&str, &StudioParentPage> = BTreeMap::new();
    for p in &r.parents {
        cx.tick()?;
        let t = p.parent_id.as_str();
        if !label(t) {
            return Err(err(Code::InvalidField, "parent_id"));
        }
        if !label(&p.name) {
            return Err(err(Code::InvalidField, format!("{t}/name")));
        }
        cx.text(&p.name, t)?;
        if by_id.insert(t, p).is_some() {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if p.parent_page_items.len() > MAX_PARENT_PAGE_ITEMS {
            return Err(err(Code::BudgetExceeded, format!("{t}/parent_page_items")));
        }
        cx.nodes(p.parent_page_items.len(), t)?;
        let mut item_ids = BTreeSet::new();
        for item in &p.parent_page_items {
            cx.tick()?;
            let target = format!("{t}/{}", item.item_id);
            if !label(&item.item_id) {
                return Err(err(Code::InvalidField, format!("{t}/item_id")));
            }
            if !item_ids.insert(item.item_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, target));
            }
            layer_ref(&item.layer_id, format!("{target}/layer_id"))?;
            if !used_layers.insert(&item.layer_id) {
                return Err(err(Code::DuplicateIdentity, &item.layer_id));
            }
        }
    }
    for p in &r.parents {
        if let Some(base) = &p.based_on
            && !by_id.contains_key(base.as_str())
        {
            return Err(err(
                Code::DanglingReference,
                format!("{}/based_on", p.parent_id),
            ));
        }
    }
    // `based_on` is a forest: follow each chain, bounded by the set size.
    for p in &r.parents {
        cx.tick()?;
        let mut seen = BTreeSet::new();
        let mut cursor = Some(p.parent_id.as_str());
        while let Some(node) = cursor {
            if !seen.insert(node) {
                return Err(err(Code::RenderCycle, &p.parent_id));
            }
            cursor = by_id.get(node).and_then(|n| n.based_on.as_deref());
        }
    }
    let mut bound_pages = BTreeSet::new();
    for bind in &r.page_bindings {
        cx.tick()?;
        let t = bind.page_id.as_str();
        if d.locate_page(t).is_none() {
            return Err(err(Code::DanglingReference, format!("{t}/page_id")));
        }
        if !bound_pages.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if bind.override_list.len() > MAX_PARENT_PAGE_ITEMS {
            return Err(err(Code::BudgetExceeded, format!("{t}/override_list")));
        }
        cx.nodes(bind.override_list.len(), t)?;
        let inheritable: BTreeSet<&str> = match &bind.applied_parent {
            Some(parent) => {
                if !by_id.contains_key(parent.as_str()) {
                    return Err(err(Code::DanglingReference, format!("{t}/applied_parent")));
                }
                let mut items = BTreeSet::new();
                let mut cursor = Some(parent.as_str());
                while let Some(page) = cursor.and_then(|c| by_id.get(c)) {
                    items.extend(page.parent_page_items.iter().map(|i| i.item_id.as_str()));
                    cursor = page.based_on.as_deref();
                }
                items
            }
            None => {
                if !bind.override_list.is_empty() {
                    return Err(err(Code::InvalidField, format!("{t}/override_list")));
                }
                BTreeSet::new()
            }
        };
        let mut overridden = BTreeSet::new();
        for o in &bind.override_list {
            cx.tick()?;
            let target = format!("{t}/override_list/{}", o.parent_item_id());
            if !inheritable.contains(o.parent_item_id()) {
                return Err(err(Code::DanglingReference, target));
            }
            if !overridden.insert(o.parent_item_id()) {
                return Err(err(Code::DuplicateIdentity, target));
            }
            if let StudioItemOverride::Replace {
                replacement_layer_id,
                ..
            } = o
            {
                layer_ref(replacement_layer_id, format!("{target}/replacement_layer_id"))?;
                if !used_layers.insert(replacement_layer_id) {
                    return Err(err(Code::DuplicateIdentity, replacement_layer_id));
                }
            }
        }
    }
    canceled(token)?;
    Ok(cx.issues)
}
