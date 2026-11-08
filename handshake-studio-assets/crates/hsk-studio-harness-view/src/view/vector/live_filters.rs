//! Vector live effects in the viewer (lane SMB-AI-LIVE-EFFECTS-1; STU-FX-002/003/007/011/012c/027,
//! STU-VEC-054, STU-VEC-154): the typed commands of STU-FX-007 over a vector layer's Folio
//! `StudioLiveFilterStack` (object scope) in the entry's vector records. Composite draws the stack
//! live through nib (`hsk_studio_composite::vector_live_filters`); the layer's source paths stay
//! editable and every render re-evaluates from them.
//!
//! Apply, update, reorder, toggle and remove are each ONE entry-only undo step
//! (`Run::set_tiles_and_entry`); bake is ONE paths-and-entry step (`Run::set_vector_paths_and_entry`):
//! the effected paths become the layer's paths (`AnchorPath::from_owned_path`), output `k` keeping
//! the style and width profile of source `k % n`; the stack goes, an envelope stays. Bake is never
//! implicit (STU-VEC-054: not on save or export). Apply and update resolve every parameter through
//! nib (`EffectRecord`, via composite's one converter) and store every resolved entry, defaults
//! included, so a stored record does not change if a table default changes later; a bad parameter,
//! a parameter whose default is unknown and unset, a law nib does not evaluate, a network path or
//! text bound to the layer, or a document unit other than px is refused before the commit by a
//! trial evaluation of the whole stack, with the composite code (`vector_effect_<nib code>`).
//!
//! The read lists each entry with every row of its kind's table (STU-VEC-100 seven fields, enum
//! members, provenance) beside the stored value, so a model can write a parameter without knowing
//! the effect (STU-VEC-154). Free distort corners are document coordinates: the transform commands
//! move them with the layer ([`transform_live_filters`]). No panel, menu or tool; no egui.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::{MAX_PATHS, PathRecord, mint_path_id};
use hsk_studio_composite::vector_live_filters::{
    MAX_LIVE_FILTER_ELEMENTS, REFUSE_EFFECT_NETWORK, REFUSE_EFFECT_TEXT_BOUND, any_enabled, cycled, effect_code, effect_record, evaluate_stack, record_params,
};
use hsk_studio_folio::{self as folio, GeometryUnit, Payload, StudioDocument, StudioLiveFilter, StudioLiveFilterParam, StudioLiveFilterStack};
use hsk_studio_nib::OwnedPath;
use hsk_studio_nib::anchor_path::AnchorPath;
use hsk_studio_nib::live_effect::{Captured, EffectKind, FREE_DISTORT_KEYS, ParamKind, Provenance, RECORD_VERSION};
use hsk_studio_nib::raster::Affine;
use serde_json::{Value, json};

fn param_json(p: &StudioLiveFilterParam) -> Value {
    serde_json::to_value(p).unwrap_or(Value::Null)
}

/// One table row of `kind` as JSON (the seven numeric fields, members, provenance).
fn row_json(spec: &hsk_studio_nib::live_effect::ParamSpec) -> Value {
    let provenance = match spec.provenance {
        Provenance::Recovered(clause) => json!({"recovered": clause}),
        Provenance::StudioDeclared => json!("studio_declared"),
    };
    let mut row = json!({"key": spec.key, "type": spec.param_type().token(), "provenance": provenance});
    match spec.kind {
        ParamKind::Numeric(n) => {
            row["hard_min"] = json!(n.hard_min);
            row["hard_max"] = json!(n.hard_max);
            row["soft_min"] = json!(n.soft_min);
            row["soft_max"] = json!(n.soft_max);
            row["default"] = json!(n.default);
            row["unit"] = json!(n.unit.token());
            if let hsk_studio_nib::live_effect::Unit::Context(selector) = n.unit {
                row["unit_selected_by"] = json!(selector);
            }
            row["precision"] = json!(n.precision);
            row["default_is_studio_decision"] = json!(n.default_is_studio_decision);
        }
        ParamKind::Enum(e) => {
            let members: Vec<Value> = e
                .members
                .iter()
                .map(|m| {
                    let captured = match m.captured {
                        Some(Captured::Bool(b)) => json!(b),
                        Some(Captured::Token(t)) => json!(t),
                        None => Value::Null,
                    };
                    json!({"id": m.id, "captured": captured})
                })
                .collect();
            row["members"] = Value::Array(members);
            row["default"] = json!(e.default);
            row["default_is_studio_decision"] = json!(e.default_is_studio_decision);
        }
        ParamKind::Bool { default } => row["default"] = json!(default),
        ParamKind::Seed { default } => row["default"] = json!(default),
    }
    row
}

/// The entry as read: stored fields plus one row per table parameter with its stored value.
fn filter_json(f: &StudioLiveFilter) -> Value {
    let stored = |key: &str| f.params.iter().find(|p| p.key() == key).map(param_json).map_or(Value::Null, |p| p["value"].clone());
    let (known, rows): (bool, Vec<Value>) = match EffectKind::from_effect_id(&f.filter_kind) {
        Some(kind) => {
            let mut rows: Vec<Value> = kind
                .params()
                .iter()
                .map(|spec| {
                    let mut r = row_json(spec);
                    r["value"] = stored(spec.key);
                    r
                })
                .collect();
            // A stored key the table does not hold (a later law set) is listed as stored.
            rows.extend(f.params.iter().filter(|p| kind.param(p.key()).is_none()).map(param_json));
            (true, rows)
        }
        None => (false, f.params.iter().map(param_json).collect()),
    };
    json!({"instance_id": f.instance_id, "filter_kind": f.filter_kind, "version": f.version, "enabled": f.enabled, "known": known, "params": rows})
}

/// A fresh instance id of the stack (`lf<n>`, the smallest unused).
fn mint_instance(filters: &[StudioLiveFilter]) -> String {
    let mut n = 1_u64;
    loop {
        let id = format!("lf{n}");
        if !filters.iter().any(|f| f.instance_id == id) {
            return id;
        }
        n = n.saturating_add(1);
    }
}

/// Moves the free distort frames of `layer_id`'s stack by `m`: the corners are document
/// coordinates, so a moved layer distorts against a moved frame. Absolute sizes do not scale
/// (`scale_strokes_and_effects` default false, STU-VEC-157).
pub fn transform_live_filters(records: &mut folio::StudioVectorRecords, layer_id: &str, m: &Affine) {
    for s in records.live_filters.iter_mut().filter(|s| s.layer_id == layer_id) {
        for f in s.filters.iter_mut().filter(|f| f.filter_kind == EffectKind::FreeDistort.effect_id()) {
            for [kh, kv] in FREE_DISTORT_KEYS.iter().flatten() {
                let read = |params: &[StudioLiveFilterParam], key: &str| {
                    params.iter().find_map(|p| match p {
                        StudioLiveFilterParam::Real { key: k, value } if k == key => Some(*value),
                        _ => None,
                    })
                };
                let (Some(x), Some(y)) = (read(&f.params, kh), read(&f.params, kv)) else { continue };
                let (tx, ty) = (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
                for p in &mut f.params {
                    if let StudioLiveFilterParam::Real { key, value } = p {
                        if key == kh {
                            *value = tx;
                        } else if key == kv {
                            *value = ty;
                        }
                    }
                }
            }
        }
    }
}

impl Viewer {
    /// The live filter stack of vector layer `layer_id` as JSON (`{"layer", "filters": [...]}`,
    /// module docs); an empty list when it has none.
    pub fn live_filters(&self, layer_id: &str) -> Result<Value, String> {
        let entry = self.vector_entry()?;
        let filters: Vec<Value> = entry.records.live_filter_stack(layer_id).map(|s| s.filters.iter().map(filter_json).collect()).unwrap_or_default();
        Ok(json!({"layer": layer_id, "filters": filters}))
    }

    /// The layer's source paths, refused for what composite does not evaluate (module docs).
    fn live_filter_sources(doc: &StudioDocument, entry: &super::VectorEntry, layer_id: &str, op: &str) -> Result<(Vec<PathRecord>, Vec<OwnedPath>), String> {
        let Some(Payload::Vector { path_ids }) = doc.layers.iter().find(|l| l.layer_id == layer_id).map(|l| &l.payload) else {
            return Err(format!("{op}: {layer_id} is not a vector layer"));
        };
        if doc.geometry_unit != GeometryUnit::Px {
            return Err(format!("{op}: {}", hsk_studio_composite::vector::REFUSE_GEOMETRY_UNIT));
        }
        let records: Vec<PathRecord> = path_ids.iter().map(|id| entry.path(id).cloned().ok_or_else(|| format!("{op}: vector_path_unresolved {id}"))).collect::<Result<_, _>>()?;
        if records.iter().any(|r| r.network.is_some()) {
            return Err(format!("{op}: {REFUSE_EFFECT_NETWORK}"));
        }
        let bound = entry.records.path_texts.iter().any(|p| p.path_layer_id == layer_id) || entry.records.area_texts.iter().any(|a| a.container_layer_id == layer_id);
        if bound {
            return Err(format!("{op}: {REFUSE_EFFECT_TEXT_BOUND}"));
        }
        let owned = records.iter().map(|r| r.path.to_owned_path().map_err(|e| format!("{op}: {}", e.code()))).collect::<Result<Vec<_>, _>>()?;
        Ok((records, owned))
    }

    /// The effected paths of `filters` over the layer's sources (the render's own evaluation).
    fn live_filter_trial(&self, doc: &StudioDocument, entry: &super::VectorEntry, layer_id: &str, filters: &[StudioLiveFilter], op: &str) -> Result<(Vec<PathRecord>, Vec<OwnedPath>), String> {
        let (records, owned) = Self::live_filter_sources(doc, entry, layer_id, op)?;
        let mut budget = MAX_LIVE_FILTER_ELEMENTS;
        let out = evaluate_stack(filters, owned, &mut budget, self.cancel).map_err(|code| format!("{op}: {code}"))?;
        Ok((records, out))
    }

    /// Checks the edited stack of `layer_id` (trial evaluation, Folio validation) and commits the
    /// entry as ONE undo step.
    fn commit_live_filters(&mut self, op: &str, layer_id: &str, entry: &super::VectorEntry) -> Result<String, String> {
        let doc = self.vdoc()?.clone();
        if let Some(s) = entry.records.live_filter_stack(layer_id)
            && any_enabled(&s.filters)
        {
            self.live_filter_trial(&doc, entry, layer_id, &s.filters, op)?;
        }
        self.validate_records(entry)?;
        self.commit_entry(op, layer_id, entry)?;
        Ok(layer_id.to_string())
    }

    /// Applies live effect `filter_kind` (nib's effect id, `hsk.studio.vector_effect.<snake>`) to
    /// the top of `layer_id`'s stack with `params` over the table defaults (STU-FX-007
    /// `STUDIO_EFFECT_APPLY`): ONE undo step. Returns the new entry's instance id.
    pub fn apply_live_filter(&mut self, layer_id: &str, filter_kind: &str, params: &[StudioLiveFilterParam]) -> Result<String, String> {
        let r = (|| {
            let op = "live_filter_apply";
            let mut entry = self.vector_entry()?;
            let existing: &[StudioLiveFilter] = entry.records.live_filter_stack(layer_id).map_or(&[], |s| s.filters.as_slice());
            let mut f = StudioLiveFilter { instance_id: mint_instance(existing), filter_kind: filter_kind.to_string(), version: RECORD_VERSION, enabled: true, params: params.to_vec() };
            let record = effect_record(&f).map_err(|e| format!("{op}: {}", effect_code(e)))?;
            f.params = record_params(&record);
            let id = f.instance_id.clone();
            match entry.records.live_filters.iter_mut().find(|s| s.layer_id == layer_id) {
                Some(s) => s.filters.push(f),
                None => entry.records.live_filters.push(StudioLiveFilterStack { layer_id: layer_id.to_string(), filters: vec![f] }),
            }
            self.commit_live_filters(op, layer_id, &entry)?;
            Ok(id)
        })();
        self.report(r)
    }

    /// Writes one typed parameter of entry `instance_id` (STU-FX-007 `STUDIO_EFFECT_UPDATE`; refused
    /// unless nib admits the key, type and value): ONE undo step.
    pub fn update_live_filter(&mut self, layer_id: &str, instance_id: &str, param: &StudioLiveFilterParam) -> Result<String, String> {
        let r = (|| {
            let op = "live_filter_update";
            let mut entry = self.vector_entry()?;
            let f = live_filter_mut(&mut entry.records, layer_id, instance_id, op)?;
            let mut next = f.clone();
            next.params.retain(|p| p.key() != param.key());
            next.params.push(param.clone());
            let record = effect_record(&next).map_err(|e| format!("{op}: {}", effect_code(e)))?;
            next.params = record_params(&record);
            *f = next;
            self.commit_live_filters(op, layer_id, &entry)
        })();
        self.report(r)
    }

    /// Moves entry `instance_id` to position `to` of the stack (0 = applied first; STU-FX-007
    /// `STUDIO_EFFECT_REORDER`): ONE undo step.
    pub fn reorder_live_filter(&mut self, layer_id: &str, instance_id: &str, to: usize) -> Result<String, String> {
        let r = (|| {
            let op = "live_filter_reorder";
            let mut entry = self.vector_entry()?;
            let s = entry.records.live_filters.iter_mut().find(|s| s.layer_id == layer_id).ok_or_else(|| format!("{op}: {layer_id} has no live filters"))?;
            let from = s.filters.iter().position(|f| f.instance_id == instance_id).ok_or_else(|| format!("{op}: no entry {instance_id}"))?;
            if to >= s.filters.len() {
                return Err(format!("{op}: position {to} is past the stack"));
            }
            let f = s.filters.remove(from);
            s.filters.insert(to, f);
            self.commit_live_filters(op, layer_id, &entry)
        })();
        self.report(r)
    }

    /// Enables or disables entry `instance_id` (STU-FX-007 `STUDIO_EFFECT_TOGGLE`): ONE undo step.
    pub fn toggle_live_filter(&mut self, layer_id: &str, instance_id: &str, enabled: bool) -> Result<String, String> {
        let r = (|| {
            let op = "live_filter_toggle";
            let mut entry = self.vector_entry()?;
            live_filter_mut(&mut entry.records, layer_id, instance_id, op)?.enabled = enabled;
            self.commit_live_filters(op, layer_id, &entry)
        })();
        self.report(r)
    }

    /// Removes entry `instance_id`; the layer draws as without it (STU-FX-003, STU-FX-007
    /// `STUDIO_EFFECT_REMOVE`). An emptied stack goes: ONE undo step.
    pub fn remove_live_filter(&mut self, layer_id: &str, instance_id: &str) -> Result<String, String> {
        let r = (|| {
            let op = "live_filter_remove";
            let mut entry = self.vector_entry()?;
            let s = entry.records.live_filters.iter_mut().find(|s| s.layer_id == layer_id).ok_or_else(|| format!("{op}: {layer_id} has no live filters"))?;
            let before = s.filters.len();
            s.filters.retain(|f| f.instance_id != instance_id);
            if s.filters.len() == before {
                return Err(format!("{op}: no entry {instance_id}"));
            }
            entry.records.live_filters.retain(|s| !s.filters.is_empty());
            self.commit_live_filters(op, layer_id, &entry)
        })();
        self.report(r)
    }

    /// Bakes `layer_id`'s enabled live filters into its paths and removes the stack (STU-FX-007
    /// `STUDIO_EFFECT_BAKE`, this slice's part of STU-VEC-054 expand-appearance): ONE undo step.
    pub fn bake_live_filters(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let op = "live_filter_bake";
            let doc = self.vdoc()?.clone();
            let mut entry = self.vector_entry()?;
            let filters = entry.records.live_filter_stack(layer_id).map(|s| s.filters.clone()).ok_or_else(|| format!("{op}: {layer_id} has no live filters"))?;
            if !any_enabled(&filters) {
                return Err(format!("{op}: {layer_id} has no enabled live filter"));
            }
            let (records, out) = self.live_filter_trial(&doc, &entry, layer_id, &filters, op)?;
            if entry.paths.len().checked_add(out.len()).is_none_or(|n| n > MAX_PATHS) {
                return Err(format!("{op}: vector_entry_over_budget"));
            }
            let sources = cycled(&records, out.len());
            let mut ids = Vec::with_capacity(out.len());
            for (source, path) in sources.iter().zip(&out) {
                let anchor = AnchorPath::from_owned_path(path).map_err(|e| format!("{op}: {}", e.code()))?;
                let id = mint_path_id(&doc, &entry);
                entry.paths.push(PathRecord { path_id: id.clone(), path: anchor, style: source.style, network: None, width: source.width.clone() });
                ids.push(id);
            }
            entry.records.live_filters.retain(|s| s.layer_id != layer_id);
            self.commit_paths(op, &[(layer_id.to_string(), ids)], &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }
}

fn live_filter_mut<'a>(records: &'a mut folio::StudioVectorRecords, layer_id: &str, instance_id: &str, op: &str) -> Result<&'a mut StudioLiveFilter, String> {
    records
        .live_filters
        .iter_mut()
        .find(|s| s.layer_id == layer_id)
        .and_then(|s| s.filters.iter_mut().find(|f| f.instance_id == instance_id))
        .ok_or_else(|| format!("{op}: no entry {instance_id} on {layer_id}"))
}
