#![deny(clippy::arithmetic_side_effects)]
//! Maps a [`LibraryProposal`] onto the CKC catalog port commands of hsk-studio-kernel-bindings
//! (lane SMB-CKC-PORTS). Accepted lrcat data enters CKC only through those typed ports (module 14
//! lrcat clause: "accepted data enters existing CKC and Studio schemas through ResourceBroker ...";
//! STU-ASSET-004/010). The plan is pure data; [`apply_ckc_import`] sends it through an injected
//! [`CkcCatalogPort`]. Request ids derive from the source fingerprint and local ids, so re-applying
//! the same plan after a failure is replayed by the host, not applied twice. Anything the port
//! vocabulary cannot carry (custom label text, untranslatable smart rules, unresolved assets,
//! non-set parents) is a typed loss in the plan; a smart collection whose rule cannot be
//! translated in full is NOT created (dropping a criterion would widen it silently).
use crate::extract::{Disposition, LossNote};
use crate::library::{ColorLabel as LrLabel, Flag as LrFlag, HierarchyStatus, LibraryCollectionKind, LibraryProposal};
use crate::lua_rules::{Combine, KnownField, KnownOp, RuleGroup, RuleNode, RuleScalar};
use hsk_studio_kernel_bindings::{
    AssetId, CatalogField, CatalogOp, CatalogRule, CatalogValue, CkcActor, CkcCatalogPort, CkcError, CkcLimits,
    CkcReceipt, CollectionCommand, CollectionId, CollectionKind, ColorLabel, Criterion, Flag, KeywordCommand,
    KeywordPath, MetadataChange, MetadataMutation, Rating,
};
use std::collections::{BTreeMap, HashMap};

/// Metadata changes for one asset; the expected revision is read from CKC at apply time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedMetadata {
    pub request_id: String,
    pub asset: AssetId,
    pub changes: Vec<MetadataChange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CkcImportPlan {
    pub keywords: Vec<KeywordCommand>,
    /// Parents before children.
    pub collections: Vec<CollectionCommand>,
    pub metadata: Vec<PlannedMetadata>,
    pub losses: Vec<LossNote>,
}

#[derive(Default)]
struct Losses(BTreeMap<(&'static str, &'static str), LossNote>);

impl Losses {
    fn add(&mut self, entity: &'static str, code: &'static str, d: Disposition, id: i64) {
        let n = self.0.entry((entity, code)).or_insert_with(|| LossNote {
            entity: entity.to_owned(),
            field: String::new(),
            disposition: d,
            code,
            count: 0,
            first_local_id: Some(id),
        });
        n.count = n.count.saturating_add(1);
    }
}

fn op(k: KnownOp) -> CatalogOp {
    match k {
        KnownOp::Eq => CatalogOp::Eq,
        KnownOp::Ne => CatalogOp::Ne,
        KnownOp::Gt => CatalogOp::Gt,
        KnownOp::Ge => CatalogOp::Ge,
        KnownOp::Lt => CatalogOp::Lt,
        KnownOp::Le => CatalogOp::Le,
        KnownOp::InRange => CatalogOp::InRange,
        KnownOp::ContainsAny => CatalogOp::ContainsAny,
        KnownOp::ContainsAll => CatalogOp::ContainsAll,
        KnownOp::ContainsWords => CatalogOp::ContainsWords,
        KnownOp::ContainsNone => CatalogOp::ContainsNone,
        KnownOp::BeginsWith => CatalogOp::BeginsWith,
        KnownOp::EndsWith => CatalogOp::EndsWith,
    }
}

fn label_named(s: &str) -> Option<ColorLabel> {
    match s.to_ascii_lowercase().as_str() {
        "red" => Some(ColorLabel::Red),
        "yellow" => Some(ColorLabel::Yellow),
        "green" => Some(ColorLabel::Green),
        "blue" => Some(ColorLabel::Blue),
        "purple" => Some(ColorLabel::Purple),
        _ => None,
    }
}

fn value(field: KnownField, v: &RuleScalar) -> Option<(CatalogField, CatalogValue)> {
    use RuleScalar as S;
    Some(match (field, v) {
        (KnownField::Rating, S::Int(n)) => (CatalogField::Rating, CatalogValue::Int(*n)),
        (KnownField::Pick, S::Int(1)) => (CatalogField::Flag, CatalogValue::Flag(Flag::Flagged)),
        (KnownField::Pick, S::Int(0)) => (CatalogField::Flag, CatalogValue::Flag(Flag::Unflagged)),
        (KnownField::Pick, S::Int(-1)) => (CatalogField::Flag, CatalogValue::Flag(Flag::Rejected)),
        (KnownField::LabelColor, S::Str(s)) => (CatalogField::Label, CatalogValue::Label(label_named(s)?)),
        (KnownField::Iso, S::Int(n)) => (CatalogField::Iso, CatalogValue::Int(*n)),
        (KnownField::HasAdjustments, S::Bool(b)) => (CatalogField::HasAdjustments, CatalogValue::Bool(*b)),
        (f, S::Str(s)) => {
            let field = match f {
                KnownField::Keywords => CatalogField::Keyword,
                KnownField::Filename => CatalogField::FileName,
                KnownField::FileFormat => CatalogField::FileFormat,
                KnownField::CaptureTime => CatalogField::CaptureTime,
                KnownField::Collection => CatalogField::Collection,
                KnownField::Folder => CatalogField::Folder,
                KnownField::CameraModel => CatalogField::CameraModel,
                KnownField::Lens => CatalogField::Lens,
                KnownField::AnySearchableText => CatalogField::AnyText,
                _ => return None,
            };
            (field, CatalogValue::Text(s.clone()))
        }
        _ => return None,
    })
}

/// Translates a decoded rule group in full, or not at all.
pub(crate) fn translate_rule(g: &RuleGroup) -> Option<CatalogRule> {
    let mut children = Vec::with_capacity(g.children.len());
    for child in &g.children {
        children.push(match child {
            RuleNode::Group(inner) => translate_rule(inner)?,
            RuleNode::Criterion(c) => {
                let (field, v) = value(c.field.known?, &c.value)?;
                let k = c.operation.known?;
                let value2 = if k == KnownOp::InRange {
                    let (f2, v2) = value(c.field.known?, c.value2.as_ref()?)?;
                    if f2 != field {
                        return None;
                    }
                    Some(v2)
                } else {
                    None
                };
                CatalogRule::Match(Criterion { field, op: op(k), value: v, value2 })
            }
        });
    }
    Some(match g.combine.as_ref()? {
        Combine::All => CatalogRule::All(children),
        Combine::Any => CatalogRule::Any(children),
        Combine::NoneOf => CatalogRule::NoneOf(children),
        Combine::Unrecognised(_) => return None,
    })
}

fn hex8(bytes: &[u8; 32]) -> String {
    bytes.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Builds the import plan. `resolve` maps an lrcat image local id to the CKC asset it was
/// imported as (CKC owns asset identity and file import; this crate never opens a path).
pub fn plan_ckc_import(
    lib: &LibraryProposal,
    resolve: &dyn Fn(i64) -> Option<AssetId>,
    limits: &CkcLimits,
) -> Result<CkcImportPlan, CkcError> {
    let fp = hex8(&lib.catalog.fingerprint.sha256_main);
    let mut losses = Losses::default();

    // Keyword paths (root first) from the validated forest; a broken chain is a loss.
    let by_id: HashMap<i64, &crate::library::LibraryKeyword> = lib.keywords.iter().map(|k| (k.local_id, k)).collect();
    let mut paths: HashMap<i64, KeywordPath> = HashMap::new();
    let mut keyword_cmds = Vec::new();
    for k in &lib.keywords {
        if matches!(k.hierarchy.status, HierarchyStatus::DuplicateId | HierarchyStatus::DepthExceeded) {
            losses.add("AgLibraryKeyword", "keyword_not_imported", Disposition::Unsupported, k.local_id);
            continue;
        }
        let mut names: Vec<String> = Vec::new();
        let mut cur = Some(k);
        while let Some(node) = cur {
            let Some(name) = node.name.clone() else { break };
            names.push(name);
            cur = node.hierarchy.parent.and_then(|p| by_id.get(&p).copied());
            if names.len() > limits.max_keyword_segments {
                break;
            }
        }
        names.reverse();
        match KeywordPath::new(&names, limits) {
            Ok(path) if names.len() == usize::try_from(k.hierarchy.depth).unwrap_or(usize::MAX).saturating_add(1) => {
                if !k.synonyms.is_empty() {
                    keyword_cmds.push(KeywordCommand::DefineSynonyms {
                        request_id: format!("lrcat-{fp}-ks{}", k.local_id),
                        keyword: path.clone(),
                        synonyms: k.synonyms.clone(),
                    });
                }
                paths.insert(k.local_id, path);
            }
            _ => losses.add("AgLibraryKeyword", "keyword_path_not_representable", Disposition::Unsupported, k.local_id),
        }
    }

    // Collections: parents first (by depth, then id); smart rules translated in full or skipped.
    let ids: HashMap<i64, CollectionId> = lib
        .collections
        .iter()
        .filter_map(|c| CollectionId::new(&format!("lrcat-{fp}-c{}", c.local_id), limits).ok().map(|id| (c.local_id, id)))
        .collect();
    let kinds: HashMap<i64, &LibraryCollectionKind> = lib.collections.iter().map(|c| (c.local_id, &c.kind)).collect();
    let mut order: Vec<&crate::library::LibraryCollection> = lib.collections.iter().collect();
    order.sort_by_key(|c| (c.hierarchy.depth, c.local_id));
    let mut created: HashMap<i64, bool> = HashMap::new();
    let mut collection_cmds = Vec::new();
    for c in order {
        let e = "AgLibraryCollection";
        let Some(id) = ids.get(&c.local_id).cloned() else { continue };
        let parent = match c.hierarchy.parent {
            Some(p) if kinds.get(&p) == Some(&&LibraryCollectionKind::CollectionSet) && created.get(&p) == Some(&true) => ids.get(&p).cloned(),
            Some(_) => {
                losses.add(e, "collection_parent_dropped", Disposition::Approximate, c.local_id);
                None
            }
            None => None,
        };
        let request_id = format!("lrcat-{fp}-cc{}", c.local_id);
        let name = c.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Collection {}", c.local_id));
        let cmd = match &c.kind {
            LibraryCollectionKind::CollectionSet => CollectionCommand::Create { request_id, id, name, parent, kind: CollectionKind::Set },
            LibraryCollectionKind::Collection => CollectionCommand::Create { request_id, id, name, parent, kind: CollectionKind::Manual },
            LibraryCollectionKind::SmartCollection => match c.rules.as_ref().and_then(translate_rule) {
                Some(rule) => CollectionCommand::CreateSmart { request_id, id, name, parent, rule },
                None => {
                    losses.add(e, "smart_rule_not_translatable", Disposition::Unsupported, c.local_id);
                    created.insert(c.local_id, false);
                    continue;
                }
            },
            LibraryCollectionKind::Unrecognised(_) => {
                losses.add(e, "collection_kind_not_imported", Disposition::Unsupported, c.local_id);
                created.insert(c.local_id, false);
                continue;
            }
        };
        if cmd.validate(limits).is_err() {
            losses.add(e, "collection_command_invalid", Disposition::Unsupported, c.local_id);
            created.insert(c.local_id, false);
            continue;
        }
        created.insert(c.local_id, true);
        collection_cmds.push(cmd);
    }
    // Manual membership, one command per collection in member order.
    let mut members: BTreeMap<i64, Vec<AssetId>> = BTreeMap::new();
    for m in &lib.catalog.collection_members {
        let manual = kinds.get(&m.collection) == Some(&&LibraryCollectionKind::Collection) && created.get(&m.collection) == Some(&true);
        match (manual, resolve(m.image)) {
            (true, Some(asset)) => members.entry(m.collection).or_default().push(asset),
            (true, None) => losses.add("AgLibraryCollectionImage", "asset_unresolved", Disposition::Missing, m.image),
            (false, _) => losses.add("AgLibraryCollectionImage", "member_collection_not_imported", Disposition::Unsupported, m.collection),
        }
    }
    for (c, assets) in members {
        for (n, chunk) in assets.chunks(limits.max_members.max(1)).enumerate() {
            if let Some(id) = ids.get(&c) {
                collection_cmds.push(CollectionCommand::AddMembers {
                    request_id: format!("lrcat-{fp}-cm{c}-{n}"),
                    collection: id.clone(),
                    assets: chunk.to_vec(),
                });
            }
        }
    }

    // Per-asset metadata: rating, flag, label, assigned keywords.
    let mut assigned: HashMap<i64, Vec<KeywordPath>> = HashMap::new();
    for a in &lib.catalog.keyword_assignments {
        match paths.get(&a.keyword) {
            Some(p) => assigned.entry(a.image).or_default().push(p.clone()),
            None => losses.add("AgLibraryKeywordImage", "keyword_not_imported", Disposition::Unsupported, a.keyword),
        }
    }
    let mut metadata = Vec::new();
    for p in &lib.photos {
        let Some(asset) = resolve(p.local_id) else {
            losses.add("Adobe_images", "asset_unresolved", Disposition::Missing, p.local_id);
            continue;
        };
        let mut changes = Vec::new();
        if let Some(r) = p.rating {
            match Rating::from_stars(r) {
                Ok(r) => changes.push(MetadataChange::SetRating(r)),
                Err(_) => losses.add("Adobe_images", "rating_not_imported", Disposition::Unsupported, p.local_id),
            }
        }
        changes.push(MetadataChange::SetFlag(match p.flag {
            LrFlag::Pick => Flag::Flagged,
            LrFlag::Reject => Flag::Rejected,
            LrFlag::Unflagged => Flag::Unflagged,
        }));
        match &p.label {
            None => {}
            Some(LrLabel::Red) => changes.push(MetadataChange::SetLabel(Some(ColorLabel::Red))),
            Some(LrLabel::Yellow) => changes.push(MetadataChange::SetLabel(Some(ColorLabel::Yellow))),
            Some(LrLabel::Green) => changes.push(MetadataChange::SetLabel(Some(ColorLabel::Green))),
            Some(LrLabel::Blue) => changes.push(MetadataChange::SetLabel(Some(ColorLabel::Blue))),
            Some(LrLabel::Purple) => changes.push(MetadataChange::SetLabel(Some(ColorLabel::Purple))),
            Some(LrLabel::Custom(_)) => losses.add("Adobe_images", "label_not_in_ckc_set", Disposition::Unsupported, p.local_id),
        }
        for k in assigned.remove(&p.local_id).unwrap_or_default() {
            changes.push(MetadataChange::AddKeyword(k));
        }
        for chunk in changes.chunks(limits.max_changes.max(1)) {
            let n = metadata.len();
            metadata.push(PlannedMetadata { request_id: format!("lrcat-{fp}-m{}-{n}", p.local_id), asset: asset.clone(), changes: chunk.to_vec() });
        }
    }
    Ok(CkcImportPlan { keywords: keyword_cmds, collections: collection_cmds, metadata, losses: losses.0.into_values().collect() })
}

/// Sends a plan through an injected port: synonyms, collections (parents first), membership,
/// then per-asset metadata with the expected revision read from CKC. Stops at the first refusal;
/// re-applying the same plan replays the receipts already issued.
pub fn apply_ckc_import(port: &dyn CkcCatalogPort, actor: &CkcActor, plan: &CkcImportPlan) -> Result<Vec<CkcReceipt>, CkcError> {
    let mut receipts = Vec::new();
    for k in &plan.keywords {
        receipts.push(port.keyword(actor, k)?);
    }
    for c in &plan.collections {
        receipts.push(port.collection(actor, c)?);
    }
    for m in &plan.metadata {
        let current = port.get(actor, &m.asset)?.revision;
        let mutation = MetadataMutation { request_id: m.request_id.clone(), asset: m.asset.clone(), expected_revision: current, changes: m.changes.clone() };
        receipts.push(port.mutate_metadata(actor, &mutation)?);
    }
    Ok(receipts)
}
