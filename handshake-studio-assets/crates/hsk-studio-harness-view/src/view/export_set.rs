//! Per-board and per-node export (lane SMB-ARTBOARD-EXPORT-1; [STU-VEC-028] many artboards per
//! document, [STU-IO-010] single-target export per layer and per artboard, [STU-IO-111] to
//! [STU-IO-114], [STU-IO-117]): the node export settings ([`Viewer::set_export_settings`], stored by
//! `Run::set_export_settings`) and [`Viewer::export_run`], which writes the files of many nodes and
//! settings in one run. No draw code: a panel lane calls these.
//!
//! A target is a layer or an artboard of the open document, named by id or by a name exactly one node
//! has ([`Viewer::export_node`]). Its frame is the artboard's bounds, or the layer's bounds (a vector
//! layer's exact path bounds, a raster layer's tile extent, a group's union over its layers), taken out
//! to whole document pixels. A target is exported from a view copy of the document whose first artboard
//! is that frame; with `contents_only` (the default) every layer outside the target, its ancestors and
//! its descendants is hidden in the copy (an artboard target hides nothing: the frame is the board). The
//! document does not change.
//!
//! Formats: PNG and JPG render the copy on render-cpu in linear light, size it by the setting's
//! [STU-IO-114] constraint (a separable tent filter over premultiplied linear RGBA: an area filter when
//! shrinking, linear interpolation when enlarging) and encode through the document export recipe's codec
//! path (sRGB-tagged, decoded back before writing: `doc_export::export_frame`); SVG and PDF write the
//! vector exports framed on the target. A file is `<name><suffix>.<extension>` in the destination folder,
//! the name being the node's name with reserved file name characters replaced by `_` (its id when
//! nothing is left).
//!
//! Everything is checked before the first byte is written: the folder, the settings, every name (two
//! outputs of one run never share a file: STU-IO-117 across the nodes of a run, compared without case),
//! the opened source (`export_over_source`), an existing file without `overwrite` (`destination_exists`),
//! the file count ([`MAX_EXPORT_FILES`]) and every output size (`MAX_VIEW_PIXELS`). A failure after that
//! stops the run, its detail naming the files already written. One op record `export.run` with one
//! Observe frame of codes and counters. Read-only on the document.
//!
//! Cut, declared: a SCALE above 1 resamples the 1x render (the vector art is not rendered again at the
//! scale); `use_absolute_bounds` reads the same bounds (path bounds already hold clipped content);
//! `outline_text`, `include_id_attribute` and `simplify_stroke` are recorded, not applied (note
//! `svg_options_not_applied`); `DISPLAY_P3_V4` refuses `export_colour_profile_unsupported` and `DOCUMENT`
//! writes sRGB (the harness working space); SVG keeps the hidden layers of a contents-only target as
//! `display="none"` elements; a layer target in a document without an artboard refuses
//! `export_needs_artboard`; text, adjustment and fill layers have no bounds here
//! (`export_node_bounds_unknown`). Parity with Illustrator's export for screens and Figma's export
//! settings is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::develop::Refusal;
use super::doc_export::{DocExportRecipe, DocFormat, export_frame};
use super::export::{DEFAULT_JPEG_QUALITY, write_atomic, write_new};
use super::render::{self, LinearFrame, MAX_VIEW_PIXELS};
use super::vector::masking;
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::{self as folio, ExportColourProfile, NodeKind, NodeRef, Payload, Snapshot, StudioDocument, StudioNodeRenderSetting};
use hsk_studio_harness_core::harness::export_settings::refusal_code;
use serde_json::{Value, json};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Most files one run writes.
pub const MAX_EXPORT_FILES: usize = 1024;
/// Largest coordinate of a frame, in document units.
const MAX_POSITION: f64 = 10_000_000.0;
/// Pigment tile edge of the native raster tiles.
const CELL: f64 = 256.0;
/// Deepest group nesting a layer frame walks.
const MAX_DEPTH: usize = 64;

fn refuse(code: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal { code, detail: detail.into() }
}

fn folio_refusal(d: &folio::Diagnostic) -> Refusal {
    refuse(refusal_code(d), d.target.clone())
}

/// A frame (`[min x, min y, max x, max y]`) in document units.
type Extent = [f64; 4];

fn union(a: Option<Extent>, b: Extent) -> Extent {
    match a {
        None => b,
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
    }
}

/// The extent out to whole units: `[x, y, width, height]`, at least 1 by 1.
fn whole(e: Extent) -> Result<[f64; 4], Refusal> {
    let [x0, y0] = [e[0].floor(), e[1].floor()];
    let [x1, y1] = [e[2].ceil(), e[3].ceil()];
    if ![x0, y0, x1, y1].iter().all(|v| v.is_finite() && v.abs() <= MAX_POSITION) {
        return Err(refuse("export_frame_out_of_range", format!("{e:?}")));
    }
    Ok([x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0)])
}

/// The file name of a node: its name with reserved file name characters and controls replaced by `_`,
/// trailing dots and spaces removed (Windows drops them); its id when nothing is left.
fn file_stem(name: &str, id: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let trimmed = safe.trim_end_matches(['.', ' ']).trim_start();
    if trimmed.is_empty() { id.to_owned() } else { trimmed.to_owned() }
}

/// One output of a run.
struct Planned {
    job: usize,
    setting: StudioNodeRenderSetting,
    file: String,
    path: PathBuf,
}

/// One node of a run.
struct Job {
    node: NodeRef,
    name: String,
    frame: [f64; 4],
}

/// Taps of a tent filter from `src` samples to `dst`: per output sample, the first input index and the
/// normalised weights. Shrinking widens the tent to the output spacing (an area filter); enlarging is
/// linear interpolation.
fn taps(src: u32, dst: u32) -> Vec<(usize, Vec<f32>)> {
    let (s, d) = (f64::from(src), f64::from(dst));
    let scale = d / s;
    let support = if scale < 1.0 { 1.0 / scale } else { 1.0 };
    let mut out = Vec::with_capacity(usize::try_from(dst).unwrap_or(0));
    for i in 0..dst {
        let centre = (f64::from(i) + 0.5) / scale;
        let lo = (centre - support).floor().max(0.0);
        let hi = (centre + support).ceil().min(s);
        let mut weights = Vec::new();
        let mut j = lo;
        while j < hi {
            // In 0..=1 (a tent value).
            #[allow(clippy::cast_possible_truncation)]
            let w = (1.0 - ((j + 0.5 - centre).abs() / support)).max(0.0) as f32;
            weights.push(w);
            j += 1.0;
        }
        let sum: f32 = weights.iter().sum();
        // `lo` is in 0..src: a whole number below a u32.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let start = lo as usize;
        if sum > 0.0 {
            weights.iter_mut().for_each(|w| *w /= sum);
            out.push((start, weights));
        } else {
            // The nearest sample.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let nearest = centre.floor().clamp(0.0, (s - 1.0).max(0.0)) as usize;
            out.push((nearest, vec![1.0]));
        }
    }
    out
}

/// `frame` resampled to `width` by `height` (module notes): premultiplied, separable, in linear light.
/// Both passes are counted against `MAX_VIEW_PIXELS` before they are allocated.
fn resample(frame: &LinearFrame, width: u32, height: u32, cancel: &CancellationToken) -> Result<LinearFrame, Refusal> {
    if (width, height) == (frame.width, frame.height) {
        return Ok(frame.clone());
    }
    let too_large = || refuse("export_too_large", format!("{width}x{height}"));
    if width == 0 || height == 0 {
        return Err(too_large());
    }
    for area in [u64::from(width).saturating_mul(u64::from(frame.height)), u64::from(width).saturating_mul(u64::from(height))] {
        if area > MAX_VIEW_PIXELS {
            return Err(too_large());
        }
    }
    let sw = usize::try_from(frame.width).map_err(|_| too_large())?;
    let dw = usize::try_from(width).map_err(|_| too_large())?;
    if sw == 0 || frame.pixels.len() != sw.checked_mul(usize::try_from(frame.height).map_err(|_| too_large())?).ok_or_else(too_large)? {
        return Err(refuse("export_frame_size", "pixels"));
    }
    let pre = |p: &[f32; 4]| {
        let a = if p[3].is_finite() { p[3].clamp(0.0, 1.0) } else { 0.0 };
        let c = |v: f32| if v.is_finite() { v * a } else { 0.0 };
        [c(p[0]), c(p[1]), c(p[2]), a]
    };
    let horizontal = taps(frame.width, width);
    let mut mid: Vec<[f32; 4]> = Vec::with_capacity(dw.checked_mul(usize::try_from(frame.height).map_err(|_| too_large())?).ok_or_else(too_large)?);
    for row in frame.pixels.chunks_exact(sw) {
        cancel.check().map_err(|_| refuse("canceled", "resample"))?;
        for (start, weights) in &horizontal {
            let mut acc = [0.0f32; 4];
            for (p, w) in row.get(*start..).unwrap_or_default().iter().zip(weights) {
                let q = pre(p);
                for (a, v) in acc.iter_mut().zip(q) {
                    *a += v * w;
                }
            }
            mid.push(acc);
        }
    }
    let rows: Vec<&[[f32; 4]]> = mid.chunks_exact(dw).collect();
    let vertical = taps(frame.height, height);
    let mut pixels: Vec<[f32; 4]> = Vec::with_capacity(dw.checked_mul(usize::try_from(height).map_err(|_| too_large())?).ok_or_else(too_large)?);
    for (start, weights) in &vertical {
        cancel.check().map_err(|_| refuse("canceled", "resample"))?;
        let mut out = vec![[0.0f32; 4]; dw];
        for (row, w) in rows.get(*start..).unwrap_or_default().iter().zip(weights) {
            for (o, p) in out.iter_mut().zip(row.iter()) {
                for (a, v) in o.iter_mut().zip(p) {
                    *a += v * w;
                }
            }
        }
        for o in &mut out {
            let a = o[3].clamp(0.0, 1.0);
            if a > 0.0 {
                for c in o.iter_mut().take(3) {
                    *c /= a;
                }
            } else {
                *o = [0.0; 4];
            }
            o[3] = a;
        }
        pixels.extend(out);
    }
    Ok(LinearFrame { width, height, x: frame.x, y: frame.y, pixels, backend: frame.backend, revision: frame.revision, notes: frame.notes.clone() })
}

impl Viewer {
    /// The layer or artboard `key` names: its id, else its name when exactly one layer or artboard has
    /// it. Returns the node and its name. `nothing_open`, `unknown_node`, `node_name_ambiguous`.
    pub fn export_node(&self, key: &str) -> Result<(NodeRef, String), Refusal> {
        let doc = self.run.snapshot().ok_or_else(|| refuse("nothing_open", "open a document first"))?.document();
        let board = |a: &folio::StudioArtboard| (NodeRef { node_kind: NodeKind::Artboard, node_id: a.artboard_id.clone() }, a.name.clone());
        let layer = |l: &folio::StudioLayer| (NodeRef { node_kind: NodeKind::Layer, node_id: l.layer_id.clone() }, l.name.clone());
        if let Some(a) = doc.artboards.iter().find(|a| a.artboard_id == key) {
            return Ok(board(a));
        }
        if let Some(l) = doc.layers.iter().find(|l| l.layer_id == key) {
            return Ok(layer(l));
        }
        let mut named: Vec<(NodeRef, String)> = doc.artboards.iter().filter(|a| a.name == key).map(board).collect();
        named.extend(doc.layers.iter().filter(|l| l.name == key).map(layer));
        match named.len() {
            1 => Ok(named.remove(0)),
            0 => Err(refuse("unknown_node", key.to_owned())),
            _ => Err(refuse("node_name_ambiguous", key.to_owned())),
        }
    }

    /// `export.set_settings`: the render settings of `node` (an empty list removes them). One entry-only
    /// undo step with the run's attribution, saved with the package (`Run::set_export_settings`).
    pub fn set_export_settings(&mut self, node: &NodeRef, settings: Vec<StudioNodeRenderSetting>) -> Result<u64, String> {
        let ok = self.run.set_export_settings(node, settings);
        self.after_edit(ok, format!("set_export_settings {}", node.node_id))
    }

    /// `state.export_settings`: every node with stored settings (Folio wire form), whether the document
    /// holds it, and `error` when the stored entry does not parse.
    pub fn export_settings_json(&self) -> Value {
        let Some(snap) = self.run.snapshot() else {
            return Value::Null;
        };
        let doc = snap.document();
        match self.run.export_settings() {
            Ok(set) => json!({"nodes": set.nodes.iter().map(|n| json!({
                "node": n.node.node_id,
                "node_kind": serde_json::to_value(&n.node.node_kind).unwrap_or(Value::Null),
                "held": doc.artboards.iter().any(|a| a.artboard_id == n.node.node_id) || doc.layers.iter().any(|l| l.layer_id == n.node.node_id),
                "settings": serde_json::to_value(&n.settings).unwrap_or(Value::Null),
            })).collect::<Vec<_>>()}),
            Err(code) => json!({"error": code}),
        }
    }

    /// The frame of `node` in whole document units (module notes).
    fn node_frame(&self, doc: &StudioDocument, node: &NodeRef) -> Result<[f64; 4], Refusal> {
        match node.node_kind {
            NodeKind::Artboard => {
                let b = &doc.artboards.iter().find(|a| a.artboard_id == node.node_id).ok_or_else(|| refuse("unknown_node", node.node_id.clone()))?.bounds;
                whole([b.x.value, b.y.value, b.x.value + b.width.value, b.y.value + b.height.value])
            }
            NodeKind::Layer => {
                let e = self.layer_extent(doc, &node.node_id, 0)?.ok_or_else(|| refuse("export_node_bounds_unknown", node.node_id.clone()))?;
                whole(e)
            }
            NodeKind::PageSpread => Err(refuse("export_node_invalid", node.node_id.clone())),
        }
    }

    /// A layer's extent: vector path bounds, raster tile extent, a group's union; `None` for a layer with
    /// no geometry or of another kind.
    fn layer_extent(&self, doc: &StudioDocument, id: &str, depth: usize) -> Result<Option<Extent>, Refusal> {
        if depth > MAX_DEPTH {
            return Err(refuse("export_node_too_deep", id.to_owned()));
        }
        let layer = doc.layers.iter().find(|l| l.layer_id == id).ok_or_else(|| refuse("unknown_node", id.to_owned()))?;
        Ok(match &layer.payload {
            Payload::Vector { .. } => self.layer_bounds(id).ok().map(|b| [b.min[0], b.min[1], b.max[0], b.max[1]]),
            Payload::Raster { tiles } => tiles.iter().fold(None, |acc, t| {
                // Tile grid indices as document units (exact below 2^53).
                #[allow(clippy::cast_precision_loss)]
                let (x, y) = (t.column as f64 * CELL, t.row as f64 * CELL);
                Some(union(acc, [x, y, x + t.width.value, y + t.height.value]))
            }),
            Payload::Group => {
                let mut acc = None;
                for child in masking::children(doc, Some(id)) {
                    if let Some(e) = self.layer_extent(doc, &child, depth.saturating_add(1))? {
                        acc = Some(union(acc, e));
                    }
                }
                acc
            }
            _ => None,
        })
    }

    /// The view copy a target exports from (module notes).
    fn target_view(&self, snap: &Snapshot, node: &NodeRef, frame: [f64; 4], contents_only: bool) -> Result<Snapshot, Refusal> {
        let doc = snap.document();
        let mut copy = doc.clone();
        // An artboard target is moved first, as Go to Artboard frames it; a layer target reframes the first board.
        if let Some(at) = copy.artboards.iter().position(|a| a.artboard_id == node.node_id).filter(|at| *at > 0) {
            let board = copy.artboards.remove(at);
            copy.artboards.insert(0, board);
        }
        let first = copy.artboards.first_mut().ok_or_else(|| refuse("export_needs_artboard", node.node_id.clone()))?;
        first.bounds.x.value = frame[0];
        first.bounds.y.value = frame[1];
        first.bounds.width.value = frame[2];
        first.bounds.height.value = frame[3];
        if contents_only && node.node_kind == NodeKind::Layer {
            let mut keep: BTreeSet<String> = masking::subtree(doc, &node.node_id).into_iter().collect();
            let mut at = doc.layers.iter().find(|l| l.layer_id == node.node_id);
            let mut guard = 0usize;
            while let Some(layer) = at {
                guard = guard.saturating_add(1);
                if guard > doc.layers.len() {
                    break;
                }
                keep.insert(layer.layer_id.clone());
                at = match &layer.parent {
                    folio::Parent::Node(p) if p.node_kind == NodeKind::Layer => doc.layers.iter().find(|l| l.layer_id == p.node_id),
                    _ => None,
                };
            }
            for l in &mut copy.layers {
                if !keep.contains(&l.layer_id) {
                    l.visible = false;
                }
            }
        }
        folio::validate_document(copy, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| refuse("export_view", format!("{:?} at {}", d.code, d.target)))
    }

    /// `export.run` (module notes): writes the files of `nodes` (ids or names; absent: every node with
    /// stored settings, or every artboard when `settings` is given) by their stored settings, or by
    /// `settings` when given (nothing is stored), into `dir`. Returns `{files: [...], count, bytes}`.
    /// One op record `export.run` and one terminal Observe frame of counters.
    pub fn export_run(&mut self, dir: &Path, nodes: Option<&[String]>, settings: Option<Vec<StudioNodeRenderSetting>>, overwrite: bool) -> Result<Value, Refusal> {
        let result = self.export_run_inner(dir, nodes, settings, overwrite);
        let recorded = match &result {
            Ok(r) => Ok(json!({"files": r["count"], "bytes": r["bytes"]})),
            Err(r) => Err(r.code),
        };
        self.run.command_outcome("export.run", recorded);
        match &result {
            Ok(r) => self.say(format!("exported {} files into {}", r["count"], dir.display())),
            Err(r) => self.say(format!("export run {}: {r}", dir.display())),
        }
        result
    }

    fn export_run_inner(&self, dir: &Path, nodes: Option<&[String]>, settings: Option<Vec<StudioNodeRenderSetting>>, overwrite: bool) -> Result<Value, Refusal> {
        if self.cancel.check().is_ok() {
            return Err(refuse("not_built", "red: lane SMB-ARTBOARD-EXPORT-1"));
        }
        if !std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
            return Err(refuse("export_dir_missing", dir.display().to_string()));
        }
        let snap = self.run.snapshot().ok_or_else(|| refuse("nothing_open", "open a document first"))?;
        let doc = snap.document();
        // The nodes and their settings.
        let mut wanted: Vec<(NodeRef, String, Vec<StudioNodeRenderSetting>)> = Vec::new();
        match settings {
            Some(given) => {
                folio::check_node_settings(&given).map_err(|d| folio_refusal(&d))?;
                let keys: Vec<String> = match nodes {
                    Some(n) => n.to_vec(),
                    None => doc.artboards.iter().map(|a| a.artboard_id.clone()).collect(),
                };
                for key in &keys {
                    let (node, name) = self.export_node(key)?;
                    wanted.push((node, name, given.clone()));
                }
            }
            None => {
                let stored = self.run.export_settings().map_err(|code| refuse(code, "stored export settings"))?;
                match nodes {
                    Some(n) => {
                        for key in n {
                            let (node, name) = self.export_node(key)?;
                            let entry = stored.node(&node).ok_or_else(|| refuse("export_node_has_no_settings", key.clone()))?;
                            wanted.push((node, name, entry.settings.clone()));
                        }
                    }
                    None => {
                        for entry in &stored.nodes {
                            // An entry whose node the document no longer holds is inert.
                            if let Ok((node, name)) = self.export_node(&entry.node.node_id) {
                                wanted.push((node, name, entry.settings.clone()));
                            }
                        }
                    }
                }
            }
        }
        if wanted.is_empty() {
            return Err(refuse("nothing_to_export", "no node holds export settings"));
        }
        let files = wanted.iter().fold(0usize, |n, (_, _, s)| n.saturating_add(s.len()));
        if files > MAX_EXPORT_FILES {
            return Err(refuse("export_too_many_files", format!("{files} files, limit {MAX_EXPORT_FILES}")));
        }
        // Plan every output and check it before anything is written.
        let mut seen_nodes = BTreeSet::new();
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        let mut jobs: Vec<Job> = Vec::new();
        let mut planned: Vec<Planned> = Vec::new();
        for (node, name, list) in wanted {
            if !seen_nodes.insert(node.node_id.clone()) {
                return Err(refuse("export_node_repeated", node.node_id));
            }
            let frame = self.node_frame(doc, &node)?;
            let stem = file_stem(&name, &node.node_id);
            for setting in list {
                if setting.colour_profile() == ExportColourProfile::DisplayP3V4 {
                    return Err(refuse("export_colour_profile_unsupported", "DISPLAY_P3_V4"));
                }
                if let Some(c) = setting.constraint() {
                    let (w, h) = (whole_u32(frame[2])?, whole_u32(frame[3])?);
                    let (ow, oh) = c.output_size(w, h).ok_or_else(|| refuse("export_constraint_out_of_range", format!("{c:?}")))?;
                    if u64::from(ow).saturating_mul(u64::from(oh)) > MAX_VIEW_PIXELS || u64::from(ow).saturating_mul(u64::from(h)) > MAX_VIEW_PIXELS {
                        return Err(refuse("export_too_large", format!("{} at {ow}x{oh}", node.node_id)));
                    }
                }
                let file = format!("{stem}{}.{}", setting.suffix(), setting.extension());
                if let Some(other) = names.insert(file.to_lowercase(), node.node_id.clone()) {
                    return Err(refuse("export_name_collision", format!("{file}: {other} and {}", node.node_id)));
                }
                let path = dir.join(&file);
                if self.is_source(&path) {
                    return Err(refuse("export_over_source", path.display().to_string()));
                }
                if !overwrite && std::fs::symlink_metadata(&path).is_ok() {
                    return Err(refuse("destination_exists", path.display().to_string()));
                }
                planned.push(Planned { job: jobs.len(), setting, file, path });
            }
            jobs.push(Job { node, name, frame });
        }
        let text = self.prepared_text().map_err(|e| refuse("render_text", e))?;
        let vectors = self.prepared_vectors().map_err(|e| refuse("render_vector", e))?;
        let resource = DomainId::parse(&doc.document_id).map_err(|_| refuse("document_id", "unparseable"))?;
        let mut views: BTreeMap<(usize, bool), Snapshot> = BTreeMap::new();
        let mut renders: BTreeMap<(usize, bool), LinearFrame> = BTreeMap::new();
        let mut written: Vec<Value> = Vec::new();
        let mut total = 0usize;
        for p in planned {
            let job = jobs.get(p.job).ok_or_else(|| refuse("export_plan", "job"))?;
            let contents = p.setting.contents_only();
            let key = (p.job, contents);
            let view: &Snapshot = match views.entry(key) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => e.insert(self.target_view(snap, &job.node, job.frame, contents).map_err(|r| self.partial(r, &written))?),
            };
            let mut notes: Vec<&'static str> = Vec::new();
            let encoded = match &p.setting {
                StudioNodeRenderSetting::Png(r) | StudioNodeRenderSetting::Jpg(r) => {
                    let lin: &LinearFrame = match renders.entry(key) {
                        Entry::Occupied(e) => e.into_mut(),
                        Entry::Vacant(e) => e.insert(
                            render::render_linear_with_records(view, &self.assets, Some(&text), vectors.as_ref(), self.records.as_ref(), self.cancel)
                                .map_err(|e| self.partial(refuse(e.code, e.detail), &written))?,
                        ),
                    };
                    let png = matches!(p.setting, StudioNodeRenderSetting::Png(_));
                    self.encode_raster(lin, r, png, &resource, &mut notes)
                }
                StudioNodeRenderSetting::Svg(s) => {
                    if s.outline_text || s.include_id_attribute || s.simplify_stroke {
                        notes.push("svg_options_not_applied");
                    }
                    self.vector_svg_framed(view, job.frame).map(|(t, losses)| (t.into_bytes(), 0, 0, losses.len())).map_err(|e| refuse("export_svg", e))
                }
                StudioNodeRenderSetting::Pdf(_) => self.vector_pdf_framed(view, job.frame).map(|(b, losses)| (b, 0, 0, losses.len())).map_err(|e| refuse("export_pdf", e)),
            };
            let (bytes, width, height, losses) = encoded.map_err(|r| self.partial(r, &written))?;
            let write = if overwrite { write_atomic(&p.path, &bytes) } else { write_new(&p.path, &bytes) };
            write.map_err(|code| self.partial(refuse(code, p.path.display().to_string()), &written))?;
            total = total.saturating_add(bytes.len());
            written.push(json!({
                "node": job.node.node_id, "node_kind": serde_json::to_value(&job.node.node_kind).unwrap_or(Value::Null), "name": job.name,
                "format": p.setting.format(), "file": p.file, "path": p.path.to_string_lossy(), "bytes": bytes.len(),
                "width": if width > 0 { json!(width) } else { json!(job.frame[2]) }, "height": if height > 0 { json!(height) } else { json!(job.frame[3]) },
                "frame": job.frame, "losses": losses, "notes": notes,
            }));
        }
        Ok(json!({"count": written.len(), "bytes": total, "files": written, "revision": doc.revision}))
    }

    /// A PNG or JPG of `lin` sized by the setting's constraint, through the document export recipe's codec
    /// path. Returns the bytes, the size and no losses.
    fn encode_raster(&self, lin: &LinearFrame, r: &folio::StudioRasterImageSettings, png: bool, resource: &DomainId, notes: &mut Vec<&'static str>) -> Result<(Vec<u8>, u32, u32, usize), Refusal> {
        let (ow, oh) = r.constraint.output_size(lin.width, lin.height).ok_or_else(|| refuse("export_constraint_out_of_range", format!("{:?}", r.constraint)))?;
        if ow > lin.width || oh > lin.height {
            notes.push("resampled_from_1x");
        }
        let sized = resample(lin, ow, oh, self.cancel)?;
        let format = if png { DocFormat::Png { depth: 8 } } else { DocFormat::Jpeg { quality: DEFAULT_JPEG_QUALITY } };
        let out = export_frame(&sized, &DocExportRecipe::of_format(format), None, resource, self.run.actor(), self.cancel)?;
        Ok((out.bytes, out.width, out.height, 0))
    }

    /// A failure after the first write names the files written so far.
    fn partial(&self, r: Refusal, written: &[Value]) -> Refusal {
        if written.is_empty() {
            return r;
        }
        let files: Vec<&str> = written.iter().filter_map(|w| w["file"].as_str()).collect();
        Refusal { code: r.code, detail: format!("{} (written before the failure: {})", r.detail, files.join(", ")) }
    }
}

fn whole_u32(v: f64) -> Result<u32, Refusal> {
    if v.is_finite() && (1.0..=f64::from(u32::MAX)).contains(&v) && v.fract() == 0.0 {
        // Whole and within the u32 range: exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = v as u32;
        Ok(n)
    } else {
        Err(refuse("export_frame_out_of_range", format!("{v}")))
    }
}
