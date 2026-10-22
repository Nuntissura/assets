//! Projection of the typed IDML model into a Folio proposal: page spreads with pages (trim size,
//! margins, bleed), text-frame layers with thread links, and one validated `StudioTextStory` per
//! story. Everything goes through Folio's own boundary (`inspect_bytes`, `validate_text_story`), so
//! Folio, not this crate, decides what a valid layout document is. Nothing is committed.
use crate::idml_decode::{Links, LossBook, Pos, canceled, thread_chains};
use crate::ids::IdMint;
use crate::model::*;
use crate::{IdmlError, Result};
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_TEXT_STORY};
use hsk_studio_folio::{
    Code, ContentDigest, Diagnostic, Inspection, Resolution, Resolver, StudioTextStory, TileRef,
    inspect_bytes, validate_text_story,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Known<'a>(pub(crate) &'a BTreeSet<String>);
impl Resolver for Known<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> Resolution {
        if schema_id == SCHEMA_STUDIO_TEXT_STORY && self.0.contains(object_id) {
            Resolution::Available
        } else {
            Resolution::Absent
        }
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn story_content(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Absent
    }
}

pub(crate) fn folio_err(d: Diagnostic) -> IdmlError {
    if d.code == Code::Canceled {
        IdmlError::Canceled
    } else {
        IdmlError::Folio(d.code)
    }
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn apply(t: &Affine, x: f64, y: f64) -> (f64, f64) {
    (t[0] * x + t[2] * y + t[4], t[1] * x + t[3] * y + t[5])
}

/// `[x0, y0, x1, y1]` of the points under `t`.
fn bbox(points: impl Iterator<Item = (f64, f64)>, t: &Affine) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (x, y) in points {
        let (x, y) = apply(t, x, y);
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    b
}

fn key_of(raw: &str, seen: &mut BTreeSet<String>) -> String {
    let mut k: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    if k.is_empty() {
        k.push('_');
    }
    let base = k.clone();
    let mut n = 1;
    while !seen.insert(k.clone()) {
        k = format!("{base}_{n}");
        n += 1;
    }
    k
}

pub(crate) fn language_key(raw: &str) -> Option<String> {
    let t = raw
        .trim()
        .trim_start_matches("Language/")
        .trim_start_matches("$ID/");
    let k: String = t
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    (!k.is_empty()).then_some(k)
}

struct Styles<'a> {
    mint: &'a mut dyn IdMint,
    by_name: BTreeMap<(bool, String), String>,
}
impl Styles<'_> {
    /// `None` for the "no style" sentinels, else the STYS id of this (kind, name).
    fn reference(&mut self, paragraph: bool, name: &str) -> Option<String> {
        if name.is_empty()
            || name.contains("[No paragraph style]")
            || name.contains("[No character style]")
        {
            return None;
        }
        let mint = &mut *self.mint;
        Some(
            self.by_name
                .entry((paragraph, name.to_owned()))
                .or_insert_with(|| mint.mint("STYS"))
                .clone(),
        )
    }
}

fn story_json(s: &StoryModel, story_id: &str, styles: &mut Styles) -> Value {
    let mut text = String::new();
    let mut paragraphs: Vec<Value> = Vec::new();
    // (start, end, style_ref, language)
    let mut ranges: Vec<(usize, usize, Option<String>, Option<String>)> = Vec::new();
    for para in &s.paragraphs {
        let start = text.len();
        for run in &para.runs {
            let from = text.len();
            text.push_str(&run.text);
            if run.br {
                text.push('\n');
            }
            if text.len() == from {
                continue;
            }
            let style = styles.reference(false, &run.char_style);
            let lang = run.language.as_deref().and_then(language_key);
            match ranges.last_mut() {
                Some(last) if last.1 == from && last.2 == style && last.3 == lang => {
                    last.1 = text.len();
                }
                _ => ranges.push((from, text.len(), style, lang)),
            }
        }
        if text.len() > start {
            let rtl = para
                .attrs
                .iter()
                .any(|(k, v)| k == "ParagraphDirection" && v.contains("RightToLeft"));
            paragraphs.push(json!({
                "start": start, "end": text.len(),
                "style_ref": styles.reference(true, &para.style),
                "paragraph_direction": if rtl { json!("right_to_left") } else { Value::Null },
            }));
        }
    }
    let ranges: Vec<Value> = ranges
        .into_iter()
        .map(|(a, b, style, lang)| json!({"start": a, "end": b, "style_ref": style, "applied_language": lang}))
        .collect();
    json!({
        "schema_id": "hsk.studio.text_story@1",
        "story_id": story_id,
        "text_kind": "area",
        "story_direction": if s.right_to_left { "right_to_left" } else { "left_to_right" },
        "content": {"content_kind": "inline", "text_utf8": text},
        "paragraphs": paragraphs,
        "character_ranges": ranges,
    })
}

pub(crate) fn project(
    model: &IdmlModel,
    limits: &Limits,
    mint: &mut dyn IdMint,
    cancel: &CancellationToken,
    loss: &mut LossBook,
) -> Result<LayoutProposal> {
    let chains = thread_chains(model, loss, cancel)?;
    // Folio holds one chain per story: keep the first, report the rest.
    let mut kept: BTreeMap<Pos, Links> = BTreeMap::new();
    let mut story_has_chain = BTreeSet::new();
    for chain in &chains {
        let (si, fi) = chain[0];
        if !story_has_chain.insert(model.spreads[si].frames[fi].story_id.as_str()) {
            loss.note(
                &model.spreads[si].part,
                "TextFrame",
                LossKind::Approximated,
                "extra_thread_chain_not_projected",
            );
            continue;
        }
        for (i, pos) in chain.iter().enumerate() {
            kept.insert(
                *pos,
                (
                    i.checked_sub(1).map(|p| chain[p]),
                    chain.get(i + 1).copied(),
                ),
            );
        }
    }

    let mut ids: Vec<IdLink> = Vec::new();
    let document_id = mint.mint("SDOC");
    let mut story_ids: BTreeMap<&str, String> = BTreeMap::new();
    for s in &model.stories {
        story_ids.insert(s.id.as_str(), mint.mint("STXT"));
    }
    let mut layer_ids: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut page_keys_seen = BTreeSet::new();
    let mut spreads_json = Vec::new();
    let mut layers_json = Vec::new();
    for (si, s) in model.spreads.iter().enumerate() {
        canceled(cancel)?;
        let spread_id = mint.mint("SPGS");
        ids.push(IdLink {
            kind: "spread",
            idml: s.id.clone(),
            folio: spread_id.clone(),
        });
        // Page rectangles in spread coordinates, pages ordered left to right.
        let mut order: Vec<usize> = (0..s.pages.len()).collect();
        let rects: Vec<[f64; 4]> = s
            .pages
            .iter()
            .map(|p| {
                let b = p.bounds_pt;
                bbox([(b[1], b[0]), (b[3], b[2])].into_iter(), &p.item_transform)
            })
            .collect();
        order.sort_by(|a, b| rects[*a][0].total_cmp(&rects[*b][0]));
        let mut page_keys = vec![String::new(); s.pages.len()];
        let mut pages_json = Vec::new();
        let (mut total_w, mut max_h) = (0.0f64, 0.0f64);
        for &pi in &order {
            let p = &s.pages[pi];
            let (w, h) = (
                p.bounds_pt[3] - p.bounds_pt[1],
                p.bounds_pt[2] - p.bounds_pt[0],
            );
            total_w += w;
            max_h = max_h.max(h);
            let key = key_of(&p.id, &mut page_keys_seen);
            ids.push(IdLink {
                kind: "page",
                idml: p.id.clone(),
                folio: key.clone(),
            });
            let m = p
                .margins_pt
                .or(model.document.margins_pt)
                .unwrap_or_else(|| {
                    loss.note(
                        &s.part,
                        "Page",
                        LossKind::Approximated,
                        "margins_defaulted_zero",
                    );
                    [0.0; 4]
                });
            let side = if !model.document.facing_pages {
                "single_sided"
            } else if rects[pi][2] <= 1e-6 {
                "left_hand"
            } else {
                "right_hand"
            };
            let bleed = model.document.bleed_pt.map_or(Value::Null, |b| {
                json!({"top": pt(b[0]), "bottom": pt(b[1]), "inside_or_left": pt(b[2]), "outside_or_right": pt(b[3])})
            });
            pages_json.push(json!({
                "page_id": key, "width": pt(w), "height": pt(h), "side": side,
                "margins": {"top": pt(m[0]), "right": pt(m[3]), "bottom": pt(m[1]), "left": pt(m[2])},
                "bleed": bleed,
            }));
            page_keys[pi] = key;
        }
        let label: Vec<&str> = order.iter().map(|i| s.pages[*i].name.as_str()).collect();
        spreads_json.push(json!({
            "schema_id": "hsk.studio.page_spread@1",
            "page_spread_id": spread_id,
            "name": if label.is_empty() { s.id.clone() } else { label.join("-") },
            "parent": Value::Null,
            "bounds": {"x": pt(0.0), "y": pt(0.0), "width": pt(total_w), "height": pt(max_h)},
            "pages": pages_json,
        }));

        for (fi, f) in s.frames.iter().enumerate() {
            let Some((prev, next)) = kept.get(&(si, fi)).copied() else {
                continue;
            };
            let layer_id = mint.mint("SLYR");
            layer_ids.insert((si, fi), layer_id.clone());
            ids.push(IdLink {
                kind: "frame",
                idml: f.id.clone(),
                folio: layer_id.clone(),
            });
            let t = &f.item_transform;
            if t[1].abs() > 1e-9 || t[2].abs() > 1e-9 {
                loss.note(
                    &s.part,
                    "TextFrame",
                    LossKind::Approximated,
                    "frame_transform_not_axis_aligned",
                );
            }
            let b = bbox(f.path.iter().map(|p| (p.anchor[0], p.anchor[1])), t);
            let overlap = |r: &[f64; 4]| {
                (b[2].min(r[2]) - b[0].max(r[0])).max(0.0)
                    * (b[3].min(r[3]) - b[1].max(r[1])).max(0.0)
            };
            let best = (0..rects.len()).max_by(|a, c| {
                overlap(&rects[*a])
                    .total_cmp(&overlap(&rects[*c]))
                    .then(c.cmp(a))
            });
            let Some(mut best) = best else {
                return Err(IdmlError::NotIdml("frame_without_page"));
            };
            if overlap(&rects[best]) <= 0.0 {
                loss.note(
                    &s.part,
                    "TextFrame",
                    LossKind::Approximated,
                    "frame_off_page",
                );
                let cx = (b[0] + b[2]) / 2.0;
                best = (0..rects.len())
                    .min_by(|a, c| {
                        let d = |i: &usize| ((rects[*i][0] + rects[*i][2]) / 2.0 - cx).abs();
                        d(a).total_cmp(&d(c))
                    })
                    .unwrap_or(best);
            }
            let r = rects[best];
            let (w, h) = (b[2] - b[0], b[3] - b[1]);
            if w <= 0.0 || h <= 0.0 || !w.is_finite() || !h.is_finite() {
                return Err(IdmlError::InvalidGeometry);
            }
            let p = &f.prefs;
            let name = f
                .attrs
                .iter()
                .find(|(k, _)| k == "Name")
                .map_or("Text frame", |(_, v)| v.as_str());
            let story = story_ids
                .get(f.story_id.as_str())
                .ok_or(IdmlError::DanglingStory)?;
            // Link ids are resolved after every layer id exists.
            layers_json.push((
                prev,
                next,
                json!({
                    "schema_id": "hsk.studio.layer@1", "layer_id": layer_id, "name": name,
                    "parent": {"node_kind": "page_spread", "node_id": spread_id},
                    "kind": "text", "payload": {"payload_kind": "text", "story_id": story},
                    "visible": true,
                    "text_frame": {
                        "page_id": page_keys[best],
                        "bounds": {"x": pt(b[0] - r[0]), "y": pt(b[1] - r[1]), "width": pt(w), "height": pt(h)},
                        "inset_spacing": {"top": pt(p.inset_pt[0]), "right": pt(p.inset_pt[3]), "bottom": pt(p.inset_pt[2]), "left": pt(p.inset_pt[1])},
                        "text_column_count": p.column_count,
                        "text_column_gutter": pt(p.column_gutter_pt),
                        "first_baseline_offset": serde_json::to_value(p.first_baseline).unwrap_or(Value::Null),
                        "minimum_first_baseline_offset": pt(p.min_first_baseline_pt),
                        "vertical_justification": serde_json::to_value(p.vertical_justification).unwrap_or(Value::Null),
                        "auto_sizing_type": serde_json::to_value(p.auto_size).unwrap_or(Value::Null),
                        "previous_text_frame": Value::Null,
                        "next_text_frame": Value::Null,
                    },
                }),
            ));
        }
    }
    let mut layers: Vec<Value> = Vec::new();
    for (prev, next, mut layer) in layers_json {
        let link = |l: Option<(usize, usize)>| {
            l.and_then(|p| layer_ids.get(&p))
                .map_or(Value::Null, |id| json!(id))
        };
        layer["text_frame"]["previous_text_frame"] = link(prev);
        layer["text_frame"]["next_text_frame"] = link(next);
        layers.push(layer);
    }

    let mut styles = Styles {
        mint,
        by_name: BTreeMap::new(),
    };
    let mut stories = Vec::new();
    let mut unresolved = Vec::new();
    let known: BTreeSet<String> = story_ids.values().cloned().collect();
    let resolver = Known(&known);
    for s in &model.stories {
        canceled(cancel)?;
        let folio_id = story_ids[s.id.as_str()].clone();
        ids.push(IdLink {
            kind: "story",
            idml: s.id.clone(),
            folio: folio_id.clone(),
        });
        let wire = story_json(s, &folio_id, &mut styles);
        let story: StudioTextStory =
            serde_json::from_value(wire).map_err(|_| IdmlError::Folio(Code::InvalidField))?;
        unresolved.extend(
            validate_text_story(&story, limits.folio, cancel, &resolver).map_err(folio_err)?,
        );
        stories.push(ProposedStory {
            story,
            idml_id: s.id.clone(),
            part: s.part.clone(),
            reflow_certified: s.composition.is_empty(),
            composition: s.composition.clone(),
        });
    }
    let style_refs = styles
        .by_name
        .into_iter()
        .map(|((paragraph, idml_name), folio)| StyleRef {
            folio,
            idml_name,
            paragraph,
        })
        .collect();

    loss.note(
        "designmap.xml",
        "Document",
        LossKind::Approximated,
        "colour_mode_assumed_rgb",
    );
    let wire = json!({
        "schema_id": "hsk.studio.document@1", "document_id": document_id, "revision": 1,
        "geometry_unit": "pt", "colour_mode": 3, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null,
        "working_space_spot": null, "blending_space": null,
        "artboards": [], "page_spreads": spreads_json, "layers": layers,
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []},
    });
    let bytes = serde_json::to_vec(&wire).map_err(|_| IdmlError::Folio(Code::InvalidJson))?;
    let snapshot =
        match inspect_bytes(&bytes, limits.folio, cancel, &resolver).map_err(folio_err)? {
            Inspection::Editable(s) => s,
            Inspection::ReadOnly(r) => return Err(IdmlError::Folio(r.diagnostic().code)),
        };
    Ok(LayoutProposal {
        snapshot,
        stories,
        style_refs,
        unresolved,
        ids,
    })
}
