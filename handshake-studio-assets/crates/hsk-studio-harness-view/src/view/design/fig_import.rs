//! `.fig` -> native package (lane SMB-FIG-EDIT-1 slice 1; [STU-IO-006] row 35 I + EP, [STU-IO-005]).
//!
//! The importer's proposal (`hsk-studio-interop-fig`: `import_fig`, `decode_document`, `to_native`)
//! is written as a native package the window opens and edits:
//! * document: the proposal's Folio document (geometry in px); each document instance's
//!   `component_instance` binding layer is dropped (the harness binds no primitive layer of that
//!   kind) and its expansion group is the instance's layer in the window (binding in the receipt);
//!   artboards round to whole pixels (the viewer frames whole pixels; a loss when they move);
//! * session entry `vector`: every proposal path as an anchor path record (document px), the
//!   proposal's paint styles with one `fill_style_id` node per styled vector layer, and its vector
//!   records (masks, clips, attachments, image paints, boolean groups);
//! * session entry `text`: one record per text layer (story, its first paragraph and character
//!   styles, the first baseline of Figma's stored layout as the pen origin); fills become linear
//!   light in the working space; a family the window has no font for draws with the bundled font
//!   (a loss per family);
//! * package records entry: the effects (`StudioRasterRecords`), rendered by the viewer's records
//!   path;
//! * receipt: provenance, every loss (importer mapping and decode losses, unresolved text styles,
//!   the adapter's own), the importer's not-proven list and the design records no entry carries
//!   (`design`: records, instance bindings with their origins, component origins);
//! * original: the `.fig` bytes, byte-identical.
#![deny(clippy::arithmetic_side_effects)]
use super::super::text::{self, FontFile, TextEntry, TextLayerRecord};
use super::super::vector::{self, PathRecord, VectorEntry, VectorStyle};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, GeometryUnit, Length, NodeKind, Payload, StudioDesignRecords};
use hsk_studio_interop_fig::{
    FigDocument, FigError, FigOutcome, IdMint, Limits, NATIVE_NOT_PROVEN, NativeConfig, NativeProposal, NodeLoss, Packaging, SourceId,
    VersionPolicy, decode_document, hex, import_fig, open_fig, to_native,
};
use hsk_studio_nib::anchor_path::AnchorPath;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// Longest parent chain walked for an absolute transform (deeper = refused as hostile).
pub const MAX_TRANSFORM_DEPTH: usize = 1024;

/// What one import produced (all losses, the instance bindings).
#[derive(Clone, Debug, PartialEq)]
pub struct FigImportReport {
    /// `path: code` per loss entry (importer and adapter).
    pub losses: Vec<String>,
    pub layers: usize,
    pub vector_paths: usize,
    pub text_layers: usize,
    pub instances: usize,
}

/// True when `path` starts like a design file: the `fig-kiwi` prelude, or a ZIP container named
/// `.fig` (the packaging Figma writes with images). The importer still decides what it is.
pub fn is_design_file(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    let named_fig = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("fig"));
    &head == b"fig-kiwi" || (named_fig && head_is_zip(&head))
}

/// The container owner's packaging verdict on a file head (`open_fig`, D16: no ZIP magic here): on
/// the 8-byte head a ZIP local header fails as `FigError::Zip` (no directory fits), a bare head as
/// `NotFig` (shorter than a prelude and version).
fn head_is_zip(head: &[u8]) -> bool {
    match open_fig(head, &Limits::default(), &CancellationToken::default()) {
        Ok(c) => c.packaging == Packaging::Zip,
        Err(e) => matches!(e, FigError::Zip(_)),
    }
}

/// Binds the instance primitive while `to_native` validates its own document (the binding layers
/// are dropped before the window opens the result).
struct Host;
impl folio::Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Absent
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Absent
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == hsk_studio_accord::SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

/// Deterministic ids from the source digest (`uuid_v7`-shaped, version block `7f16`).
struct DigestMint {
    stem: String,
    next: u64,
}
impl IdMint for DigestMint {
    fn mint(&mut self, prefix: &'static str) -> String {
        let n = self.next;
        self.next = self.next.saturating_add(1);
        format!("{prefix}-{}-{}-7f16-8000-{n:012x}", self.stem.get(0..8).unwrap_or("00000000"), self.stem.get(8..12).unwrap_or("0000"))
    }
}

/// sRGB-encoded component to linear light (the text renderer's fill space).
fn linear(c: f64) -> f64 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// One loss of the receipt (`kind`: importer `mapping` / `decode`, or the window's `viewer`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Loss {
    path: String,
    kind: &'static str,
    code: String,
    count: u32,
    first: Option<String>,
}

fn node_losses(list: &[NodeLoss], kind: &'static str) -> Vec<Loss> {
    list.iter().map(|l| Loss { path: l.path.clone(), kind, code: l.code.to_owned(), count: l.count, first: l.first.map(|s| s.to_string()) }).collect()
}

fn viewer_loss(out: &mut Vec<Loss>, path: impl Into<String>, code: impl Into<String>) {
    let (path, code) = (path.into(), code.into());
    match out.iter_mut().find(|l| l.kind == "viewer" && l.path == path && l.code == code) {
        Some(l) => l.count = l.count.saturating_add(1),
        None => out.push(Loss { path, kind: "viewer", code, count: 1, first: None }),
    }
}

/// 2x3 affine `[a, b, tx, c, d, ty]` (x' = a x + b y + tx).
type Affine = [f64; 6];
const IDENTITY: Affine = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

fn compose(p: &Affine, l: &Affine) -> Affine {
    [
        p[0] * l[0] + p[1] * l[3],
        p[0] * l[1] + p[1] * l[4],
        p[0] * l[2] + p[1] * l[5] + p[2],
        p[3] * l[0] + p[4] * l[3],
        p[3] * l[1] + p[4] * l[4],
        p[3] * l[2] + p[4] * l[5] + p[5],
    ]
}

/// Absolute transform of a node: its own transform after every ancestor's (root first). `None`
/// for an unknown node, a chain deeper than [`MAX_TRANSFORM_DEPTH`] or a non-finite result.
fn absolute(doc: &FigDocument, index: &HashMap<SourceId, usize>, id: SourceId) -> Option<Affine> {
    let mut chain = Vec::new();
    let mut at = Some(id);
    while let Some(cur) = at {
        if chain.len() >= MAX_TRANSFORM_DEPTH {
            return None;
        }
        let n = doc.nodes.get(*index.get(&cur)?)?;
        chain.push(n.transform.map_or(IDENTITY, |m| [f64::from(m.m00), f64::from(m.m01), f64::from(m.m02), f64::from(m.m10), f64::from(m.m11), f64::from(m.m12)]));
        at = n.parent;
    }
    let abs = chain.iter().rev().fold(IDENTITY, |acc, l| compose(&acc, l));
    abs.iter().all(|v| v.is_finite()).then_some(abs)
}

/// Origin of a node in document px and whether its absolute transform is a pure translation.
fn origin(doc: &FigDocument, index: &HashMap<SourceId, usize>, id: SourceId) -> Option<([f64; 2], bool)> {
    let a = absolute(doc, index, id)?;
    let pure = (a[0] - 1.0).abs() < 1e-9 && a[1].abs() < 1e-9 && a[3].abs() < 1e-9 && (a[4] - 1.0).abs() < 1e-9;
    Some(([a[2], a[5]], pure))
}

/// Replaces every string equal to a key of `map` (ids are unique tokens) anywhere in `v`.
fn rekey(v: &mut Value, map: &BTreeMap<String, String>) {
    match v {
        Value::String(s) => {
            if let Some(to) = map.get(s.as_str()) {
                s.clone_from(to);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rekey(x, map)),
        Value::Object(o) => o.values_mut().for_each(|x| rekey(x, map)),
        _ => {}
    }
}

fn json_err(e: impl std::fmt::Display) -> String {
    format!("fig_import: {e}")
}

/// A native package of the `.fig` file `bytes` (module docs); `fonts` are the window's fonts.
pub fn fig_to_package(bytes: &[u8], fonts: &[FontFile], cancel: &CancellationToken) -> Result<(Vec<u8>, FigImportReport), String> {
    let limits = Limits::default();
    let outcome = import_fig(bytes, &limits, &VersionPolicy::observed(), None, cancel).map_err(|e| format!("fig_import: refused {}", e.code()))?;
    let import = match outcome {
        FigOutcome::Imported(i) => i,
        FigOutcome::OpaqueOnly { error, .. } => return Err(format!("fig_import: version unsupported ({})", error.code())),
    };
    let fig = decode_document(&import, &limits, cancel).map_err(|e| format!("fig_import: decode refused {}", e.code()))?;
    let digest = hex(&import.source_sha256);
    let mut mint = DigestMint { stem: digest.clone(), next: 1 };
    let cfg = NativeConfig { budget: folio::Budget::default(), resolver: &Host };
    let proposal = to_native(&fig, &cfg, &mut mint, cancel).map_err(|e| format!("fig_import: native refused {}", e.code()))?;
    build_package(bytes, &import, &fig, &proposal, &digest, fonts, cancel)
}

#[allow(clippy::too_many_lines)]
fn build_package(
    bytes: &[u8],
    import: &hsk_studio_interop_fig::FigImport,
    fig: &FigDocument,
    proposal: &NativeProposal,
    digest: &str,
    fonts: &[FontFile],
    cancel: &CancellationToken,
) -> Result<(Vec<u8>, FigImportReport), String> {
    let mut losses = node_losses(&proposal.mapping_loss, "mapping");
    losses.extend(node_losses(&proposal.decode_loss, "decode"));
    for s in &proposal.unresolved_text_styles {
        losses.push(Loss { path: format!("native.text.style={s}"), kind: "mapping", code: "text_style_unresolved".into(), count: 1, first: None });
    }
    let mut doc = proposal.snapshot.document().clone();
    let index: HashMap<SourceId, usize> = fig.nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();

    // Instance bindings: the binding layer goes, its expansion group is the instance's layer.
    let mut rekeyed: BTreeMap<String, String> = BTreeMap::new();
    let mut instances = Vec::new();
    for e in &proposal.expansions {
        let Some(binding) = &e.binding_layer_id else { continue };
        let Some(Payload::PrimitiveReference { object_id, .. }) = doc.layers.iter().find(|l| &l.layer_id == binding).map(|l| &l.payload) else { continue };
        let (at, pure) = origin(fig, &index, e.instance).unwrap_or(([0.0, 0.0], false));
        instances.push(json!({"layer_id": e.group_layer_id, "instance_id": object_id, "source": e.instance.to_string(), "origin_px": at, "translation_only": pure}));
        rekeyed.insert(binding.clone(), e.group_layer_id.clone());
    }
    let unbound = doc.layers.iter().filter(|l| l.kind == "component_instance" && !rekeyed.contains_key(&l.layer_id)).count();
    for _ in 0..unbound {
        viewer_loss(&mut losses, "viewer.instance", "instance_without_expansion_not_drawn");
    }
    doc.layers.retain(|l| l.kind != "component_instance");
    // Component origins (definition roots) for re-expansion after a variant switch or swap.
    let layer_source: BTreeMap<&str, SourceId> = proposal.id_map.iter().map(|e| (e.node.node_id.as_str(), e.source)).collect();
    let components: Vec<Value> = proposal
        .design
        .components
        .iter()
        .filter_map(|k| {
            let src = *layer_source.get(k.layer_id.as_str())?;
            let (at, pure) = origin(fig, &index, src)?;
            Some(json!({"component_id": k.component_id, "layer_id": k.layer_id, "origin_px": at, "translation_only": pure}))
        })
        .collect();

    // Artboards: whole pixels.
    for a in &mut doc.artboards {
        let raw = [a.bounds.x.value, a.bounds.y.value, a.bounds.width.value, a.bounds.height.value];
        let whole = [raw[0].round(), raw[1].round(), raw[2].ceil().max(1.0), raw[3].ceil().max(1.0)];
        if raw != whole {
            viewer_loss(&mut losses, format!("viewer.artboard={}", a.artboard_id), "artboard_rounded_to_pixels");
        }
        for (l, v) in [&mut a.bounds.x, &mut a.bounds.y, &mut a.bounds.width, &mut a.bounds.height].into_iter().zip(whole) {
            *l = Length { value: v, unit: GeometryUnit::Px };
        }
    }

    // Paths: anchor records; a path nib cannot edit leaves its layer (a loss).
    let mut records = Vec::with_capacity(proposal.paths.len());
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    for p in &proposal.paths {
        cancel.check().map_err(|_| "fig_import: canceled".to_string())?;
        match AnchorPath::from_owned_path(&p.geometry) {
            Ok(path) => records.push(PathRecord::plain(p.path_id.clone(), path, VectorStyle { fill: None, stroke: None, stroke_width: 0.0 })),
            Err(e) => {
                viewer_loss(&mut losses, "viewer.vector_path", format!("path_not_editable:{}", e.code()));
                dropped.insert(p.path_id.clone());
            }
        }
    }
    if !dropped.is_empty() {
        for l in &mut doc.layers {
            if let Payload::Vector { path_ids } = &mut l.payload {
                path_ids.retain(|id| !dropped.contains(id));
            }
        }
    }
    // The window composites in the harness's RGB working space (as the `.ai` import does): the
    // importer leaves it unassigned and the renderer refuses `no_working_profile` without one.
    let mut wire = serde_json::to_value(&doc).map_err(json_err)?;
    if wire["working_space_rgb"].is_null() {
        wire["working_space_rgb"] = json!(format!("SCPF-{}-{}-7f16-8000-000000000001", digest.get(0..8).unwrap_or("00000000"), digest.get(8..12).unwrap_or("0000")));
    }
    let doc: folio::StudioDocument = serde_json::from_value(wire).map_err(json_err)?;
    let snapshot = folio::validate_document(doc, folio::Budget::default(), cancel, &crate::harness::TrustAll).map_err(|d| format!("fig_import: folio {:?} {}", d.code, d.target))?;
    let doc = snapshot.document();
    // The import content of each instance layer (its children): the inspector's import record
    // applies while the layer still holds exactly these children.
    for b in &mut instances {
        let sig = super::instance::signature(doc, b["layer_id"].as_str().unwrap_or_default());
        b["signature"] = json!(sig);
    }
    let vector_layers: BTreeSet<&str> = doc.layers.iter().filter(|l| matches!(l.payload, Payload::Vector { .. })).map(|l| l.layer_id.as_str()).collect();

    // The vector entry: paint styles, one fill node per styled vector layer, vector records. Image
    // paints name import-original parts the window's vector assets cannot serve (composite refuses
    // them `image_paint_unresolved`): their rows leave the styles, each a loss.
    let mut image_rows: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for p in &proposal.vector_records.image_paints {
        image_rows.entry(p.style_id.as_str()).or_default().push(usize::try_from(p.row).unwrap_or(usize::MAX));
        viewer_loss(&mut losses, "viewer.image_paint", "image_paint_not_drawn_in_window");
    }
    let mut design = StudioDesignRecords::new(doc.document_id.clone());
    for s in &proposal.design.paint_styles {
        let mut s = s.clone();
        if let Some(rows) = image_rows.get_mut(s.style_id.as_str()) {
            rows.sort_unstable();
            for r in rows.iter().rev() {
                if *r < s.rows.len() {
                    s.rows.remove(*r);
                }
            }
            if s.rows.is_empty() {
                viewer_loss(&mut losses, format!("viewer.paint_style={}", s.style_id), "paint_style_only_image_rows");
                continue;
            }
        }
        match vector::style::validate_style(&s) {
            Ok(()) => design.paint_styles.push(s),
            Err(e) => viewer_loss(&mut losses, format!("viewer.paint_style={}", s.style_id), format!("paint_style_not_drawable:{e}")),
        }
    }
    let kept: BTreeSet<String> = design.paint_styles.iter().map(|s| s.style_id.clone()).collect();
    for n in &proposal.design.nodes {
        let Some(fill) = n.style_refs.get("fill_style_id") else { continue };
        if n.node.node_kind != NodeKind::Layer || !vector_layers.contains(n.node.node_id.as_str()) || !kept.contains(fill) {
            continue;
        }
        design.nodes.push(serde_json::from_value(json!({"node": {"node_kind": "layer", "node_id": n.node.node_id}, "style_refs": {"fill_style_id": fill}})).map_err(json_err)?);
    }
    let mut entry = VectorEntry::empty();
    entry.paths = records;
    entry.design = Some(design);
    let mut vrec = proposal.vector_records.clone();
    vrec.document_id.clone_from(&doc.document_id);
    vrec.image_paints.clear();
    // Attachments of a dropped binding layer: its expansion group keeps its own, else takes it.
    let own: BTreeSet<String> = vrec.attachments.iter().map(|a| a.layer_id.clone()).collect();
    vrec.attachments.retain_mut(|a| match rekeyed.get(&a.layer_id) {
        None => true,
        Some(group) if own.contains(group) => false,
        Some(group) => {
            a.layer_id.clone_from(group);
            true
        }
    });
    entry.records = vrec;
    entry.validate(doc, cancel)?;
    entry.validate_records(doc, cancel).map_err(|e| format!("fig_import: {e}"))?;
    let entry_bytes = entry.to_bytes(&doc.document_id)?;

    // The text entry.
    let text_layers: Vec<(&str, &str)> = doc.layers.iter().filter_map(|l| if let Payload::Text { story_id } = &l.payload { Some((l.layer_id.as_str(), story_id.as_str())) } else { None }).collect();
    let text_bytes = if text_layers.is_empty() {
        None
    } else {
        if text_layers.len() > text::MAX_TEXT_LAYERS {
            return Err(format!("fig_import: {} text layers exceed the window's {}", text_layers.len(), text::MAX_TEXT_LAYERS));
        }
        let mut styles = proposal.type_styles.clone();
        styles.document_id.clone_from(&doc.document_id);
        let mut substituted: BTreeSet<String> = BTreeSet::new();
        for st in &mut styles.styles {
            if let Some(f) = &mut st.fill {
                (f.r, f.g, f.b) = (linear(f.r), linear(f.g), linear(f.b));
                f.colour_profile_id = doc.working_space_rgb.clone();
            }
            if let Some(family) = &st.font_family
                && !fonts.iter().any(|f| &f.family == family)
            {
                let key = format!("{family}/{}", st.font_style.as_deref().unwrap_or(""));
                if substituted.insert(key.clone()) {
                    viewer_loss(&mut losses, format!("viewer.text.font={key}"), format!("font_substituted:{}", text::DEFAULT_FONT));
                }
                st.font_family = Some(text::DEFAULT_FONT.into());
            }
        }
        let mut layers = Vec::with_capacity(text_layers.len());
        let mut stories = Vec::with_capacity(text_layers.len());
        let mut default_styles: Option<(String, String)> = None;
        for (layer_id, story_id) in text_layers {
            let story = proposal.stories.iter().find(|s| s.story_id == story_id).ok_or_else(|| format!("fig_import: text layer {layer_id} has no story"))?;
            let len = text::story_text(story).map_or(usize::MAX, str::len);
            if len > text::MAX_STORY_BYTES {
                return Err(format!("fig_import: text layer {layer_id} story over the window's {} bytes", text::MAX_STORY_BYTES));
            }
            let layout = proposal.text_layouts.iter().find(|t| t.layer_id == layer_id);
            let origin_px = match layout.and_then(|t| t.lines.first()) {
                Some(line) => line.baseline_px,
                None => {
                    viewer_loss(&mut losses, "viewer.text.origin", "text_layout_missing_origin_zero");
                    [0.0, 0.0]
                }
            };
            if layout.is_some_and(|t| t.lines.len() > 1) {
                viewer_loss(&mut losses, "viewer.text.lines", "multi_line_text_drawn_on_one_line");
            }
            let runs: BTreeSet<&str> = story.character_ranges.iter().filter_map(|r| r.style_ref.as_deref()).collect();
            if runs.len() > 1 {
                viewer_loss(&mut losses, "viewer.text.runs", "style_runs_drawn_with_the_first_style");
            }
            let paragraph = story.paragraphs.first().and_then(|p| p.style_ref.clone());
            let character = story.character_ranges.first().and_then(|r| r.style_ref.clone());
            let (paragraph, character) = match (paragraph, character) {
                (Some(p), Some(c)) if styles.style(&p).is_some() && styles.style(&c).is_some() => (p, c),
                _ => {
                    viewer_loss(&mut losses, "viewer.text.style", "text_style_missing_default_used");
                    default_styles.get_or_insert_with(|| (format!("STYS-{}-{}-7f16-a000-000000000001", digest.get(0..8).unwrap_or("00000000"), digest.get(8..12).unwrap_or("0000")), format!("STYS-{}-{}-7f16-a000-000000000002", digest.get(0..8).unwrap_or("00000000"), digest.get(8..12).unwrap_or("0000")))).clone()
                }
            };
            stories.push(story.clone());
            layers.push(TextLayerRecord { layer_id: layer_id.to_owned(), story_id: story_id.to_owned(), origin_px, paragraph_style: paragraph, character_style: character });
        }
        if let Some((p, c)) = &default_styles {
            let profile = serde_json::to_value(&doc.working_space_rgb).unwrap_or(Value::Null);
            styles.styles.extend(serde_json::from_value::<Vec<folio::StudioTypeStyle>>(json!([
                {"schema_id": "hsk.studio.type_style@1", "style_id": p, "scope": "paragraph", "name": "Import/Paragraph", "align": "left"},
                {"schema_id": "hsk.studio.type_style@1", "style_id": c, "scope": "character", "name": "Import/Character", "font_family": text::DEFAULT_FONT, "font_size": {"value": 12.0, "unit": "px"}, "fill": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0, "colour_profile_id": profile}},
            ])).map_err(json_err)?);
        }
        let e = TextEntry { schema: text::ENTRY_SCHEMA.into(), layers, stories, styles };
        e.validate(doc, cancel)?;
        Some(e.to_bytes()?)
    };

    // Design records no entry carries: the receipt keeps them (binding layers rekeyed to the
    // expansion groups) with the instance bindings and component origins.
    let mut design_records = serde_json::to_value(&proposal.design).map_err(json_err)?;
    rekey(&mut design_records, &rekeyed);
    let original = hsk_studio_package::ImportEntry::new(hsk_studio_package::ImportRole::Original, bytes.to_vec());
    let report = FigImportReport {
        losses: losses.iter().map(|l| format!("{}: {}", l.path, l.code)).collect(),
        layers: doc.layers.len(),
        vector_paths: entry.paths.len(),
        text_layers: text_bytes.as_ref().map_or(0, |_| doc.layers.iter().filter(|l| matches!(l.payload, Payload::Text { .. })).count()),
        instances: instances.len(),
    };
    let receipt = json!({
        "receipt": "import", "receipt_version": 1, "source_format": "fig",
        "importer": "hsk-studio-interop-fig import_fig + decode_document + to_native (window adapter SMB-FIG-EDIT-1)",
        "source": {"sha256": digest, "byte_len": bytes.len(), "entry": original.name(), "fig_kind": import.kind.label(), "fig_version": import.version},
        "layers": doc.layers.len(), "artboards": doc.artboards.len(), "vector_paths": entry.paths.len(), "text_layers": report.text_layers,
        "loss": losses.iter().map(|l| json!({"path": l.path, "kind": l.kind, "code": l.code, "count": l.count, "first": l.first})).collect::<Vec<_>>(),
        "not_proven": NATIVE_NOT_PROVEN.iter().copied().chain(["window_render_parity (the window draws the native document through composite; no Figma render oracle)", "instance_expansion_after_edit (a variant switch, swap or override edit redraws the instance from its definition by translation; derived Figma layout data is not reapplied)"]).collect::<Vec<_>>(),
        "design": {"records": design_records, "instances": instances, "components": components},
    });
    let receipt = hsk_studio_package::ImportEntry::new(hsk_studio_package::ImportRole::Receipt, serde_json::to_vec(&receipt).map_err(json_err)?);
    let entry_sha = hsk_studio_package::sha256_hex_of(&entry_bytes);
    let mut held = vec![
        hsk_studio_package::Quarantined { name: format!("{}{}.{entry_sha}", hsk_studio_session::DOCUMENT_ENTRY_PREFIX, vector::ENTRY), bytes: entry_bytes, sha256_hex: entry_sha },
        receipt.to_opaque(),
        original.to_opaque(),
    ];
    if let Some(b) = text_bytes {
        let sha = hsk_studio_package::sha256_hex_of(&b);
        held.push(hsk_studio_package::Quarantined { name: format!("{}{}.{sha}", hsk_studio_session::DOCUMENT_ENTRY_PREFIX, text::ENTRY), bytes: b, sha256_hex: sha });
    }
    if !proposal.raster_records.layers.is_empty() {
        let mut r = proposal.raster_records.clone();
        r.document_id.clone_from(&doc.document_id);
        // Effects of a dropped binding layer move to its expansion group unless it has its own.
        let own: BTreeSet<String> = r.layers.iter().map(|l| l.layer_id.clone()).collect();
        r.layers.retain_mut(|l| match rekeyed.get(&l.layer_id) {
            None => true,
            Some(group) if own.contains(group) => false,
            Some(group) => {
                l.layer_id.clone_from(group);
                true
            }
        });
        let body = serde_json::to_vec(&r).map_err(json_err)?;
        held.push(hsk_studio_package::ImportEntry::new(hsk_studio_package::ImportRole::Records, body).to_opaque());
    }
    let mut out = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &hsk_studio_package::MemoryAssetSource::new(),
        &hsk_studio_package::Extras { quarantined: &held, previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        cancel,
        &mut out,
    )
    .map_err(|e| format!("fig_import: package {}", e.code()))?;
    Ok((out, report))
}

impl super::super::Viewer {
    /// Converts the design file `source` (`.fig`) into a native package at `dest` (never over an
    /// existing file without `overwrite`) and opens it (module docs). Returns the import report.
    pub fn import_fig(&mut self, source: &Path, dest: &Path, overwrite: bool) -> Result<FigImportReport, String> {
        let r = (|| {
            if dest.exists() && !overwrite {
                return Err(format!("fig_import: {} exists (tick overwrite to replace it)", dest.display()));
            }
            let len = std::fs::metadata(source).map_err(|e| format!("fig_import: {e}"))?.len();
            if len > crate::import::MAX_SOURCE_BYTES {
                return Err(format!("fig_import: {len} bytes exceeds {}", crate::import::MAX_SOURCE_BYTES));
            }
            let bytes = std::fs::read(source).map_err(|e| format!("fig_import: {e}"))?;
            let (package, report) = fig_to_package(&bytes, &self.fonts, self.cancel)?;
            super::super::export::write_atomic(dest, &package).map_err(|e| format!("fig_import: {e}"))?;
            self.open(dest)?;
            self.design.reset();
            Ok(report)
        })();
        match &r {
            Ok(rep) => self.say(format!("design file {} -> {} ({} layers, {} paths, {} instances, {} loss entries)", source.display(), dest.display(), rep.layers, rep.vector_paths, rep.instances, rep.losses.len())),
            Err(e) => self.say(e.clone()),
        }
        r
    }
}
