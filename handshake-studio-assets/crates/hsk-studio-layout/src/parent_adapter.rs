//! Adapter between Folio's `hsk.studio.parent_page_records@1` (SMB-FOLIO-PARENT-PAGES) and the
//! layout-side [`ParentSet`]. Forward: parents -> `ParentPage` (id = `parent_id`, items from
//! `parent_page_items`), bindings -> `PageBinding` (`applied_parent`, replace -> `Replace` with a
//! replacement item built from `replacement_layer_id`, suppress -> `Suppress`). Reverse: the inverse,
//! using the layer assignment the forward direction returned (layout items carry no layer id).
//!
//! Item rectangles come from the layer: a text layer's `text_frame.bounds` (page coordinates, pt);
//! any other layer has no stored geometry in Folio, so the caller supplies its rectangle in
//! [`LayerRects`]; a layer with neither is a typed refusal, never a guessed rectangle. Folio marks no
//! page-number marker, so Folio-sourced items are `TextFrame` (text layer with a frame) or `Frame`.
//!
//! Declared gap: Folio has no record for page-local items (`PageBinding::local_items`). The reverse
//! direction refuses a binding that has any ([`ParentAdapterError::PageLocalItemsNoRecord`]) instead
//! of dropping them. Layout replacement ids must equal the replaced item id (Folio names no other).
//! InDesign override equality is UNVERIFIED (D5 oracle).
use crate::parents::{MAX_BINDINGS, MAX_ITEMS_PER_PARENT, MAX_PARENTS};
use crate::{
    ItemOverride, LayoutError, PageBinding, ParentItem, ParentItemKind, ParentPage, ParentSet,
    Rect, ResolvedItem,
};
use hsk_studio_folio::{
    GeometryUnit, Length, ParentPageSchema, StudioDocument, StudioItemOverride, StudioLayer,
    StudioPageParentBinding, StudioParentPage, StudioParentPageItem, StudioParentPageRecords,
};
use std::collections::BTreeMap;

/// Caller-supplied rectangles (page coordinates, points) by `layer_id`, for layers whose geometry
/// Folio does not store (anything but a text layer with a frame).
pub type LayerRects = BTreeMap<String, Rect>;

/// Layer assignment of the layout items: which document layer backs which item.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerAssignment {
    /// `(parent_id, item_id)` -> `layer_id`.
    pub parent_items: BTreeMap<(String, String), String>,
    /// `(page_id, parent_item_id)` -> replacement `layer_id`.
    pub replacements: BTreeMap<(String, String), String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParentAdapterError {
    /// The set or a derived value failed layout validation.
    Layout(LayoutError),
    /// `document_id` of the records differs from the document's.
    DocumentMismatch,
    /// A binding names a page the document does not hold.
    UnknownPage { page_id: String },
    /// A referenced layer is not in the document.
    MissingLayer { layer_id: String },
    /// The layer has no stored geometry and no rectangle was supplied.
    NoGeometry { layer_id: String },
    /// Geometry is not finite points with a non-negative size.
    InvalidGeometry { layer_id: String },
    /// Reverse: a layout binding carries page-local items; Folio has no record for them yet.
    PageLocalItemsNoRecord { page_id: String },
    /// Reverse: a layout item has no layer in the assignment.
    MissingLayerAssignment { what: String },
    /// Reverse: a replacement item's id differs from the item it replaces (not expressible).
    ReplacementIdMismatch { parent_item_id: String },
}
impl From<LayoutError> for ParentAdapterError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}
impl ParentAdapterError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::DocumentMismatch => "document_mismatch",
            Self::UnknownPage { .. } => "unknown_page",
            Self::MissingLayer { .. } => "missing_layer",
            Self::NoGeometry { .. } => "no_geometry",
            Self::InvalidGeometry { .. } => "invalid_geometry",
            Self::PageLocalItemsNoRecord { .. } => "page_local_items_no_record",
            Self::MissingLayerAssignment { .. } => "missing_layer_assignment",
            Self::ReplacementIdMismatch { .. } => "replacement_id_mismatch",
        }
    }
}
impl std::fmt::Display for ParentAdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ParentAdapterError {}

fn pt(l: &Length) -> Option<f64> {
    (l.unit == GeometryUnit::Pt && l.value.is_finite()).then_some(l.value)
}

fn frame_rect(layer: &StudioLayer) -> Result<Option<Rect>, ParentAdapterError> {
    let Some(f) = &layer.text_frame else {
        return Ok(None);
    };
    let bad = || ParentAdapterError::InvalidGeometry {
        layer_id: layer.layer_id.clone(),
    };
    let b = &f.bounds;
    let (x, y, w, h) = (
        pt(&b.x).ok_or_else(bad)?,
        pt(&b.y).ok_or_else(bad)?,
        pt(&b.width).ok_or_else(bad)?,
        pt(&b.height).ok_or_else(bad)?,
    );
    if w < 0.0 || h < 0.0 {
        return Err(bad());
    }
    let r = Rect {
        x0_pt: x,
        y0_pt: y,
        x1_pt: x + w,
        y1_pt: y + h,
    };
    if !r.valid() {
        return Err(bad());
    }
    Ok(Some(r))
}

fn item_for(
    document: &StudioDocument,
    rects: &LayerRects,
    item_id: &str,
    layer_id: &str,
) -> Result<ParentItem, ParentAdapterError> {
    let layer = document
        .layers
        .iter()
        .find(|l| l.layer_id == layer_id)
        .ok_or_else(|| ParentAdapterError::MissingLayer {
            layer_id: layer_id.to_string(),
        })?;
    let (kind, rect) = match frame_rect(layer)? {
        Some(r) => (ParentItemKind::TextFrame, r),
        None => match rects.get(layer_id) {
            Some(r) if r.valid() => (ParentItemKind::Frame, *r),
            Some(_) => {
                return Err(ParentAdapterError::InvalidGeometry {
                    layer_id: layer_id.to_string(),
                });
            }
            None => {
                return Err(ParentAdapterError::NoGeometry {
                    layer_id: layer_id.to_string(),
                });
            }
        },
    };
    Ok(ParentItem {
        id: item_id.to_string(),
        kind,
        rect,
    })
}

/// Folio records -> layout set, plus the layer assignment needed to go back. The result is
/// validated as a unit (`ParentSet::validate`), so a hostile record set is refused whole.
pub fn parent_set_from_records(
    records: &StudioParentPageRecords,
    document: &StudioDocument,
    rects: &LayerRects,
) -> Result<(ParentSet, LayerAssignment), ParentAdapterError> {
    if records.document_id != document.document_id {
        return Err(ParentAdapterError::DocumentMismatch);
    }
    if records.parents.len() > MAX_PARENTS || records.page_bindings.len() > MAX_BINDINGS {
        return Err(LayoutError::BudgetExceeded.into());
    }
    let mut assign = LayerAssignment::default();
    let mut set = ParentSet::default();
    for p in &records.parents {
        if p.parent_page_items.len() > MAX_ITEMS_PER_PARENT {
            return Err(LayoutError::BudgetExceeded.into());
        }
        let mut items = Vec::with_capacity(p.parent_page_items.len());
        for i in &p.parent_page_items {
            items.push(item_for(document, rects, &i.item_id, &i.layer_id)?);
            assign
                .parent_items
                .insert((p.parent_id.clone(), i.item_id.clone()), i.layer_id.clone());
        }
        set.parents.push(ParentPage {
            id: p.parent_id.clone(),
            name: p.name.clone(),
            based_on: p.based_on.clone(),
            items,
        });
    }
    for b in &records.page_bindings {
        if document.locate_page(&b.page_id).is_none() {
            return Err(ParentAdapterError::UnknownPage {
                page_id: b.page_id.clone(),
            });
        }
        if b.override_list.len() > MAX_ITEMS_PER_PARENT {
            return Err(LayoutError::BudgetExceeded.into());
        }
        let mut overrides = Vec::with_capacity(b.override_list.len());
        for o in &b.override_list {
            overrides.push(match o {
                StudioItemOverride::Replace {
                    parent_item_id,
                    replacement_layer_id,
                } => {
                    assign.replacements.insert(
                        (b.page_id.clone(), parent_item_id.clone()),
                        replacement_layer_id.clone(),
                    );
                    ItemOverride::Replace {
                        parent_item_id: parent_item_id.clone(),
                        replacement: item_for(
                            document,
                            rects,
                            parent_item_id,
                            replacement_layer_id,
                        )?,
                    }
                }
                StudioItemOverride::Suppress { parent_item_id } => ItemOverride::Suppress {
                    parent_item_id: parent_item_id.clone(),
                },
            });
        }
        set.bindings.push(PageBinding {
            page_id: b.page_id.clone(),
            parent_id: b.applied_parent.clone(),
            overrides,
            local_items: Vec::new(),
        });
    }
    set.validate()?;
    Ok((set, assign))
}

/// Layout set -> Folio records for `document_id`. Page-local items are refused (no Folio record).
pub fn records_from_parent_set(
    set: &ParentSet,
    document_id: &str,
    assign: &LayerAssignment,
) -> Result<StudioParentPageRecords, ParentAdapterError> {
    set.validate()?;
    let mut out = StudioParentPageRecords::new(document_id);
    for p in &set.parents {
        let mut items = Vec::with_capacity(p.items.len());
        for i in &p.items {
            let layer = assign
                .parent_items
                .get(&(p.id.clone(), i.id.clone()))
                .ok_or_else(|| ParentAdapterError::MissingLayerAssignment {
                    what: format!("{}/{}", p.id, i.id),
                })?;
            items.push(StudioParentPageItem {
                item_id: i.id.clone(),
                layer_id: layer.clone(),
            });
        }
        out.parents.push(StudioParentPage {
            schema_id: ParentPageSchema::V1,
            parent_id: p.id.clone(),
            name: p.name.clone(),
            based_on: p.based_on.clone(),
            parent_page_items: items,
        });
    }
    for b in &set.bindings {
        if !b.local_items.is_empty() {
            return Err(ParentAdapterError::PageLocalItemsNoRecord {
                page_id: b.page_id.clone(),
            });
        }
        let mut list = Vec::with_capacity(b.overrides.len());
        for o in &b.overrides {
            list.push(match o {
                ItemOverride::Replace {
                    parent_item_id,
                    replacement,
                } => {
                    if &replacement.id != parent_item_id {
                        return Err(ParentAdapterError::ReplacementIdMismatch {
                            parent_item_id: parent_item_id.clone(),
                        });
                    }
                    let layer = assign
                        .replacements
                        .get(&(b.page_id.clone(), parent_item_id.clone()))
                        .ok_or_else(|| ParentAdapterError::MissingLayerAssignment {
                            what: format!("{}/{}", b.page_id, parent_item_id),
                        })?;
                    StudioItemOverride::Replace {
                        parent_item_id: parent_item_id.clone(),
                        replacement_layer_id: layer.clone(),
                    }
                }
                ItemOverride::Suppress { parent_item_id } => StudioItemOverride::Suppress {
                    parent_item_id: parent_item_id.clone(),
                },
            });
        }
        out.page_bindings.push(StudioPageParentBinding {
            page_id: b.page_id.clone(),
            applied_parent: b.parent_id.clone(),
            override_list: list,
        });
    }
    Ok(out)
}

/// The effective items of `page_id` (parent chain + overrides), bottom first. A page without a
/// binding shows nothing.
pub fn resolve_page_from_records(
    records: &StudioParentPageRecords,
    document: &StudioDocument,
    rects: &LayerRects,
    page_id: &str,
) -> Result<Vec<ResolvedItem>, ParentAdapterError> {
    let (set, _) = parent_set_from_records(records, document, rects)?;
    Ok(set.resolve(page_id)?)
}
