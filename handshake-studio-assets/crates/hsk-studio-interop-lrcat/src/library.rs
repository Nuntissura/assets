#![deny(clippy::arithmetic_side_effects)]
//! Library-level import mapping on top of the fixed extraction: typed pick flags and colour
//! labels, collection kinds (collection, smart collection, collection set), validated collection
//! and keyword hierarchies (parent cycles, unresolved parents, depth cap), keyword synonyms and
//! smart-collection rule trees. The result is an import PROPOSAL for the CKC catalog owner
//! (STU-ASSET-001/004/019, STU-IO-189): this crate holds no catalog, store, index or query
//! engine, and nothing here evaluates a rule. Every unmapped item is a typed loss.
//! Lightroom names below (creation ids, owning modules, synonym table) are UNVERIFIED against a
//! Lightroom-written catalog (D5 oracle).
use crate::catalog_bytes::{Db, Schema, entry_bytes};
use crate::error::{LimitKind, LrcatError, Result};
use crate::extract::{
    CatalogProposal, CollectionProposal, Disposition, Dropped, EntityCount, Extractor, LossNote,
    PhotoProposal, extract_with,
};
use crate::lua_rules::{RuleGroup, RuleLimits, SmartRules, parse_smart_rules};
use crate::xmp::{PacketShape, XmpLibraryFields, XmpLimits, XmpRating, decode_xmp_packet, packet_shape};
use crate::source::{Grant, Limits};
use hsk_studio_accord::CancellationToken;
use std::collections::{HashMap, HashSet};

pub const LIBRARY_DECODER_ID: &str = "hsk-studio-interop-lrcat/library-mapping@1";
/// Hard ceiling for `LibraryLimits::max_hierarchy_depth`.
pub const HARD_MAX_HIERARCHY_DEPTH: u32 = 4096;

const SMART_MODULE: &str = "ag.library.smart_collection";
const KIND_COLLECTION: &str = "com.adobe.ag.library.collection";
const KIND_SMART: &str = "com.adobe.ag.library.smart_collection";
const KIND_SET: &str = "com.adobe.ag.library.group";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LibraryLimits {
    pub rules: RuleLimits,
    pub xmp: XmpLimits,
    /// Deepest keyword or collection nesting mapped as a hierarchy; deeper nodes are typed losses.
    pub max_hierarchy_depth: u32,
}

impl Default for LibraryLimits {
    fn default() -> Self {
        Self {
            rules: RuleLimits::default(),
            xmp: XmpLimits::default(),
            max_hierarchy_depth: 256,
        }
    }
}

impl LibraryLimits {
    pub fn validate(&self) -> Result<()> {
        self.rules
            .validate()
            .map_err(|_| LrcatError::Limit(LimitKind::Configuration))?;
        self.xmp
            .validate()
            .map_err(|_| LrcatError::Limit(LimitKind::Configuration))?;
        if self.max_hierarchy_depth == 0 || self.max_hierarchy_depth > HARD_MAX_HIERARCHY_DEPTH {
            return Err(LrcatError::Limit(LimitKind::Configuration));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    Pick,
    Reject,
    Unflagged,
}

/// Lightroom stores the label NAME of the active label set (UNVERIFIED); the default set's five
/// names are typed, any other text is kept as `Custom` with an approximate loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorLabel {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoLibraryState {
    pub local_id: i64,
    /// 0..=5 stars; `None` when the catalog has no valid rating.
    pub rating: Option<u8>,
    pub flag: Flag,
    pub label: Option<ColorLabel>,
    /// Library fields of the XMP packet the catalog keeps for this image, when it is a plain
    /// text packet (compressed packets are `xmp_compressed_not_inflated` losses).
    pub xmp: Option<XmpLibraryFields>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HierarchyStatus {
    Root,
    Child,
    /// The parent id resolves to no row: kept as a root.
    ParentUnresolved,
    /// Part of a parent cycle (including self-parent): detached as a root.
    ParentCycle,
    /// Nesting deeper than `max_hierarchy_depth`: parent kept, flagged.
    DepthExceeded,
    /// A repeated local id (corrupt b-tree): detached, not linked.
    DuplicateId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HierarchyNode {
    pub local_id: i64,
    /// Validated parent (`None` for roots and detached nodes).
    pub parent: Option<i64>,
    /// Distance from a root (saturating).
    pub depth: u32,
    pub status: HierarchyStatus,
}

/// Validates a parent forest in O(n) time and O(n) memory (n = `items.len()`, already bounded by
/// the extraction's row and retained-memory limits). Output order equals input order.
pub fn resolve_hierarchy(items: &[(i64, Option<i64>)], max_depth: u32) -> Vec<HierarchyNode> {
    const NEW: u8 = 0;
    const ON_PATH: u8 = 1;
    const DONE: u8 = 2;
    let n = items.len();
    let mut index: HashMap<i64, usize> = HashMap::with_capacity(n);
    let mut out: Vec<HierarchyNode> = items
        .iter()
        .map(|(id, parent)| HierarchyNode {
            local_id: *id,
            parent: *parent,
            depth: 0,
            status: HierarchyStatus::Root,
        })
        .collect();
    let mut state = vec![NEW; n];
    for (i, (id, _)) in items.iter().enumerate() {
        if index.contains_key(id) {
            out[i].parent = None;
            out[i].status = HierarchyStatus::DuplicateId;
            state[i] = DONE;
        } else {
            index.insert(*id, i);
        }
    }
    let mut path: Vec<usize> = Vec::new();
    for start in 0..n {
        if state[start] == DONE {
            continue;
        }
        path.clear();
        let mut cur = start;
        // Depth of the node the unwinding path hangs from (`None`: the last path node is a root).
        let base: Option<u32> = loop {
            if state[cur] == DONE {
                break Some(out[cur].depth);
            }
            if state[cur] == ON_PATH {
                let at = path.iter().position(|p| *p == cur).unwrap_or(0);
                for &c in &path[at..] {
                    out[c].parent = None;
                    out[c].depth = 0;
                    out[c].status = HierarchyStatus::ParentCycle;
                    state[c] = DONE;
                }
                path.truncate(at);
                break Some(0);
            }
            state[cur] = ON_PATH;
            path.push(cur);
            match out[cur].parent {
                None => break None,
                Some(p) => match index.get(&p) {
                    // `index` holds first occurrences only, never a `DuplicateId` row.
                    Some(&j) => cur = j,
                    None => {
                        out[cur].parent = None;
                        out[cur].status = HierarchyStatus::ParentUnresolved;
                        break None;
                    }
                },
            }
        };
        let mut depth = base;
        while let Some(c) = path.pop() {
            match depth {
                None => {
                    // Topmost node: a root (or an unresolved-parent root, status already set).
                    out[c].depth = 0;
                }
                Some(d) => {
                    let mine = d.saturating_add(1);
                    out[c].depth = mine;
                    out[c].status = if mine > max_depth {
                        HierarchyStatus::DepthExceeded
                    } else {
                        HierarchyStatus::Child
                    };
                }
            }
            state[c] = DONE;
            depth = Some(out[c].depth);
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryCollectionKind {
    Collection,
    SmartCollection,
    CollectionSet,
    Unrecognised(Option<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryCollection {
    pub local_id: i64,
    pub name: Option<String>,
    pub kind: LibraryCollectionKind,
    pub hierarchy: HierarchyNode,
    /// Decoded rule tree of a smart collection (never evaluated here).
    pub rules: Option<RuleGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryKeyword {
    pub local_id: i64,
    pub name: Option<String>,
    pub hierarchy: HierarchyNode,
    pub synonyms: Vec<String>,
}

/// Library import proposal. `catalog` is exactly what `extract_catalog` returns for the same
/// grant and limits; the library layer adds typed state and its own losses and counts.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryProposal {
    pub catalog: CatalogProposal,
    pub photos: Vec<PhotoLibraryState>,
    pub collections: Vec<LibraryCollection>,
    pub keywords: Vec<LibraryKeyword>,
    pub losses: Vec<LossNote>,
    /// Row accounting of the tables only the library layer decodes
    /// (`AgLibraryCollectionContent`, `AgLibraryKeywordSynonym`).
    pub counts: Vec<EntityCount>,
    /// Catalog-level loss codes this proposal translates (the catalog part keeps them).
    pub resolves: Vec<&'static str>,
    pub decoder: &'static str,
}

impl LibraryProposal {
    pub fn loss(&self, entity: &str, code: &str) -> Option<&LossNote> {
        self.losses.iter().find(|l| l.entity == entity && l.code == code)
    }
    pub fn count(&self, entity: &str) -> Option<&EntityCount> {
        self.counts.iter().find(|c| c.entity == entity)
    }
}

/// Rows the library layer read inside the extraction pass.
struct Raw {
    losses: Vec<LossNote>,
    counts: Vec<EntityCount>,
    rules: Vec<(i64, SmartRules)>,
    synonyms: Vec<(i64, String)>,
    xmp: Vec<(i64, XmpLibraryFields)>,
    retained: u64,
}

fn read_library_tables(
    db: &Db<'_>,
    schema: &Schema,
    limits: &Limits,
    lib: &LibraryLimits,
    cancel: &CancellationToken,
    catalog: &CatalogProposal,
) -> Result<Raw> {
    let x = Extractor::new(db, schema, limits, cancel);
    // Id sets of the entities the library rows refer to (charged before they are built).
    db.charge(
        entry_bytes::<i64>().saturating_mul(
            (catalog.collections.len() as u64)
                .saturating_add(catalog.keywords.len() as u64)
                .saturating_add(catalog.photos.len() as u64),
        ),
    )?;
    let collection_ids: HashSet<i64> = catalog.collections.iter().map(|c| c.local_id).collect();
    let keyword_ids: HashSet<i64> = catalog.keywords.iter().map(|k| k.local_id).collect();
    let image_ids: HashSet<i64> = catalog.photos.iter().map(|p| p.local_id).collect();

    let mut rules: Vec<(i64, SmartRules)> = Vec::new();
    let mut seen_rules: HashSet<i64> = HashSet::new();
    if let Some(t) = x.optional("AgLibraryCollectionContent") {
        let e = "AgLibraryCollectionContent";
        let walked = x.rows(e, t, &["collection", "content", "owningModule"], |id, v| {
            let module = x.text(e, "owningModule", &v[2], id)?;
            if module.as_deref() != Some(SMART_MODULE) {
                return Ok(Some(Dropped::new("owningModule", Disposition::Unsupported, "collection_content_module_unmapped")));
            }
            let Some(collection) = v[0].as_i64().filter(|c| collection_ids.contains(c)) else {
                return Ok(Some(Dropped::new("collection", Disposition::Unsupported, "orphan_row")));
            };
            if seen_rules.contains(&collection) {
                return Ok(Some(Dropped::new("collection", Disposition::Unsupported, "smart_rules_duplicate")));
            }
            let bytes = v[1].bytes().unwrap_or(&[]);
            match parse_smart_rules(bytes, &lib.rules) {
                Err(err) => Ok(Some(Dropped::new("content", Disposition::Unsupported, err.code()))),
                Ok(parsed) => {
                    // Aggregate check BEFORE the tree is retained (per-rule size is bounded by
                    // `RuleLimits`, the running total by `max_retained_text_bytes`).
                    db.charge(
                        entry_bytes::<(i64, SmartRules)>()
                            .saturating_add(entry_bytes::<i64>())
                            .saturating_add(parsed.retained_bytes),
                    )?;
                    seen_rules.insert(collection);
                    for (code, n) in &parsed.notes {
                        x.loss(e, "content", Disposition::Approximate, code, u64::from(*n), Some(id));
                    }
                    rules.push((collection, parsed));
                    Ok(None)
                }
            }
        })?;
        x.count(e, walked, rules.len() as u64);
    }

    let mut synonyms: Vec<(i64, String)> = Vec::new();
    if let Some(t) = x.optional("AgLibraryKeywordSynonym") {
        let e = "AgLibraryKeywordSynonym";
        let walked = x.rows(e, t, &["keyword", "name"], |id, v| {
            let keyword = x.int(e, "keyword", &v[0], id).filter(|k| keyword_ids.contains(k));
            let (Some(keyword), Some(name)) = (keyword, x.text(e, "name", &v[1], id)?) else {
                return Ok(Some(Dropped::new("keyword,name", Disposition::Unsupported, "orphan_row")));
            };
            db.charge(entry_bytes::<(i64, String)>())?;
            synonyms.push((keyword, name));
            Ok(None)
        })?;
        x.count(e, walked, synonyms.len() as u64);
    }

    let mut xmp: Vec<(i64, XmpLibraryFields)> = Vec::new();
    let mut seen_xmp: HashSet<i64> = HashSet::new();
    if let Some(t) = x.optional("Adobe_AdditionalMetadata") {
        let e = "Adobe_AdditionalMetadata";
        let walked = x.rows(e, t, &["image", "xmp"], |id, v| {
            let Some(image) = v[0].as_i64().filter(|i| image_ids.contains(i)) else {
                return Ok(Some(Dropped::new("image", Disposition::Unsupported, "orphan_row")));
            };
            if seen_xmp.contains(&image) {
                return Ok(Some(Dropped::new("image", Disposition::Unsupported, "xmp_duplicate")));
            }
            let bytes = v[1].bytes().unwrap_or(&[]);
            match packet_shape(bytes) {
                PacketShape::CompressedLooking => {
                    return Ok(Some(Dropped::new("xmp", Disposition::Opaque, "xmp_compressed_not_inflated")));
                }
                PacketShape::Other => {
                    return Ok(Some(Dropped::new("xmp", Disposition::Unsupported, "xmp_not_packet")));
                }
                PacketShape::Text => {}
            }
            match decode_xmp_packet(bytes, &lib.xmp) {
                Err(err) => Ok(Some(Dropped::new("xmp", Disposition::Unsupported, err.code()))),
                Ok(fields) => {
                    db.charge(
                        entry_bytes::<(i64, XmpLibraryFields)>()
                            .saturating_add(entry_bytes::<i64>())
                            .saturating_add(fields.retained_bytes),
                    )?;
                    seen_xmp.insert(image);
                    for (code, n) in &fields.notes {
                        x.loss(e, "xmp", Disposition::Approximate, code, u64::from(*n), Some(id));
                    }
                    xmp.push((image, fields));
                    Ok(None)
                }
            }
        })?;
        x.count(e, walked, xmp.len() as u64);
    }

    let (losses, counts) = x.finish();
    Ok(Raw {
        losses,
        counts,
        rules,
        synonyms,
        xmp,
        retained: db.retained(),
    })
}

/// Running retained-memory budget for the post-extraction mapping (shares the extraction's
/// `max_retained_text_bytes` with what the extraction already retained).
struct Budget {
    left: u64,
}

impl Budget {
    fn charge(&mut self, bytes: u64) -> Result<()> {
        self.left = self
            .left
            .checked_sub(bytes)
            .ok_or(LrcatError::Limit(LimitKind::RetainedText))?;
        Ok(())
    }
}

#[derive(Default)]
struct Losses(Vec<LossNote>);

impl Losses {
    fn add(&mut self, entity: &str, field: &str, disposition: Disposition, code: &'static str, id: Option<i64>) {
        match self
            .0
            .iter_mut()
            .find(|l| l.entity == entity && l.field == field && l.code == code)
        {
            Some(l) => l.count = l.count.saturating_add(1),
            None => self.0.push(LossNote {
                entity: entity.to_owned(),
                field: field.to_owned(),
                disposition,
                code,
                count: 1,
                first_local_id: id,
            }),
        }
    }
}

fn photo_state(p: &PhotoProposal, losses: &mut Losses) -> PhotoLibraryState {
    let flag = match p.pick {
        Some(1) => Flag::Pick,
        Some(-1) => Flag::Reject,
        _ => Flag::Unflagged,
    };
    let label = p.color_label.as_deref().filter(|s| !s.is_empty()).map(|s| match s {
        "Red" => ColorLabel::Red,
        "Yellow" => ColorLabel::Yellow,
        "Green" => ColorLabel::Green,
        "Blue" => ColorLabel::Blue,
        "Purple" => ColorLabel::Purple,
        other => {
            losses.add("Adobe_images", "colorLabels", Disposition::Approximate, "color_label_custom_text", Some(p.local_id));
            ColorLabel::Custom(other.to_owned())
        }
    });
    PhotoLibraryState {
        local_id: p.local_id,
        rating: p.rating.and_then(|r| u8::try_from(r).ok()),
        flag,
        label,
        xmp: None,
    }
}

fn collection_kind(c: &CollectionProposal) -> LibraryCollectionKind {
    match c.creation_id.as_deref() {
        Some(KIND_COLLECTION) => LibraryCollectionKind::Collection,
        Some(KIND_SMART) => LibraryCollectionKind::SmartCollection,
        Some(KIND_SET) => LibraryCollectionKind::CollectionSet,
        other => LibraryCollectionKind::Unrecognised(other.map(str::to_owned)),
    }
}

fn hierarchy_losses(entity: &str, nodes: &[HierarchyNode], losses: &mut Losses) {
    for h in nodes {
        let code = match h.status {
            HierarchyStatus::Root | HierarchyStatus::Child => continue,
            HierarchyStatus::ParentUnresolved => "parent_unresolved",
            HierarchyStatus::ParentCycle => "parent_cycle",
            HierarchyStatus::DepthExceeded => "hierarchy_depth_exceeded",
            HierarchyStatus::DuplicateId => "duplicate_rowid",
        };
        losses.add(entity, "parent", Disposition::Approximate, code, Some(h.local_id));
    }
}

/// Library import mapping: one bounded pass over the granted bytes (the fixed extraction plus
/// the smart-rule and synonym tables), then typed mapping of flags, labels, collection kinds,
/// hierarchies, synonyms and rule trees. Any error publishes nothing; nothing is evaluated,
/// stored or written.
pub fn extract_library(
    grant: &Grant,
    limits: &Limits,
    lib: &LibraryLimits,
    cancel: &CancellationToken,
) -> Result<LibraryProposal> {
    lib.validate()?;
    let (catalog, raw) = extract_with(grant, limits, cancel, |db, schema, limits, cancel, catalog| {
        read_library_tables(db, schema, limits, lib, cancel, catalog)
    })?;
    let mut budget = Budget {
        left: limits.max_retained_text_bytes.saturating_sub(raw.retained),
    };
    let mut losses = Losses(raw.losses);
    let mut resolves: Vec<&'static str> = Vec::new();

    budget.charge(entry_bytes::<PhotoLibraryState>().saturating_mul(catalog.photos.len() as u64))?;
    let mut photos: Vec<PhotoLibraryState> = catalog.photos.iter().map(|p| photo_state(p, &mut losses)).collect();
    let photo_at: HashMap<i64, usize> = photos.iter().enumerate().map(|(i, p)| (p.local_id, i)).collect();
    let decoded_xmp = !raw.xmp.is_empty();
    for (image, fields) in raw.xmp {
        let Some(&i) = photo_at.get(&image) else { continue };
        let state = &mut photos[i];
        let differs = match fields.rating {
            Some(XmpRating::Stars(n)) => state.rating != Some(n),
            Some(XmpRating::Rejected) => state.flag != Flag::Reject,
            None => false,
        };
        if differs {
            losses.add("Adobe_AdditionalMetadata", "xmp:Rating", Disposition::Approximate, "xmp_rating_differs_from_catalog", Some(image));
        }
        state.xmp = Some(fields);
    }
    if decoded_xmp {
        resolves.push("xmp_blob_not_inflated");
    }

    // Collections: kind, validated parent forest, rule trees attached to smart collections.
    let pairs: Vec<(i64, Option<i64>)> = catalog.collections.iter().map(|c| (c.local_id, c.parent)).collect();
    budget.charge(entry_bytes::<LibraryCollection>().saturating_mul(pairs.len() as u64).saturating_mul(2))?;
    let tree = resolve_hierarchy(&pairs, lib.max_hierarchy_depth);
    hierarchy_losses("AgLibraryCollection", &tree, &mut losses);
    let mut rules: HashMap<i64, RuleGroup> = raw.rules.into_iter().map(|(c, r)| (c, r.root)).collect();
    let kinds: HashMap<i64, LibraryCollectionKind> =
        catalog.collections.iter().map(|c| (c.local_id, collection_kind(c))).collect();
    let mut collections = Vec::with_capacity(catalog.collections.len());
    for (c, h) in catalog.collections.iter().zip(tree) {
        let kind = collection_kind(c);
        let e = "AgLibraryCollection";
        if let LibraryCollectionKind::Unrecognised(_) = kind {
            losses.add(e, "creationId", Disposition::Unsupported, "collection_kind_unmapped", Some(c.local_id));
        }
        if let Some(parent) = h.parent
            && kinds.get(&parent) != Some(&LibraryCollectionKind::CollectionSet)
        {
            losses.add(e, "parent", Disposition::Approximate, "parent_not_collection_set", Some(c.local_id));
        }
        let mine = rules.remove(&c.local_id);
        match (&kind, &mine) {
            (LibraryCollectionKind::SmartCollection, None) => {
                losses.add(e, "rules", Disposition::Missing, "smart_rules_absent", Some(c.local_id))
            }
            (LibraryCollectionKind::SmartCollection, Some(_)) => {}
            (_, Some(_)) => losses.add(e, "rules", Disposition::Unsupported, "rules_on_non_smart_collection", Some(c.local_id)),
            (_, None) => {}
        }
        collections.push(LibraryCollection {
            local_id: c.local_id,
            name: c.name.clone(),
            kind,
            hierarchy: h,
            rules: mine,
        });
    }
    if collections
        .iter()
        .any(|c| c.kind == LibraryCollectionKind::SmartCollection && c.rules.is_some())
    {
        resolves.push("smart_collection_rules_not_decoded");
    }
    if collections.iter().any(|c| c.kind == LibraryCollectionKind::CollectionSet) {
        resolves.push("collection_kind_unmapped");
    }
    if catalog.loss("AgLibraryCollectionContent", "collection_content_not_decoded").is_some() {
        resolves.push("collection_content_not_decoded");
    }
    debug_assert!(rules.is_empty(), "rule rows for unknown collections are refused at walk time");

    // Keywords: validated tree plus synonyms.
    let pairs: Vec<(i64, Option<i64>)> = catalog.keywords.iter().map(|k| (k.local_id, k.parent)).collect();
    budget.charge(entry_bytes::<LibraryKeyword>().saturating_mul(pairs.len() as u64).saturating_mul(2))?;
    let tree = resolve_hierarchy(&pairs, lib.max_hierarchy_depth);
    hierarchy_losses("AgLibraryKeyword", &tree, &mut losses);
    let mut keywords: Vec<LibraryKeyword> = catalog
        .keywords
        .iter()
        .zip(tree)
        .map(|(k, h)| LibraryKeyword {
            local_id: k.local_id,
            name: k.name.clone(),
            hierarchy: h,
            synonyms: Vec::new(),
        })
        .collect();
    let at: HashMap<i64, usize> = keywords.iter().enumerate().map(|(i, k)| (k.local_id, i)).collect();
    for (keyword, name) in raw.synonyms {
        // Synonym rows for unknown keywords are refused at walk time (orphan_row).
        if let Some(&i) = at.get(&keyword) {
            keywords[i].synonyms.push(name);
        }
    }
    if catalog.loss("unmapped_table", "unmapped_table").is_some()
        && raw.counts.iter().any(|c| c.entity == "AgLibraryKeywordSynonym")
    {
        resolves.push("unmapped_table");
    }
    for k in &keywords {
        budget.charge(k.name.as_ref().map_or(0, |n| n.len() as u64))?;
    }
    for c in &collections {
        budget.charge(c.name.as_ref().map_or(0, |n| n.len() as u64))?;
    }

    let mut losses = losses.0;
    losses.sort_by(|a, b| (&a.entity, &a.field, a.code).cmp(&(&b.entity, &b.field, b.code)));
    Ok(LibraryProposal {
        catalog,
        photos,
        collections,
        keywords,
        losses,
        counts: raw.counts,
        resolves,
        decoder: LIBRARY_DECODER_ID,
    })
}
