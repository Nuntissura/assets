//! Board writes and the board export (lane SMB-BOARD-1; module docs of [`super`]): creating, editing,
//! moving and deleting board nodes as ONE undo step each, the generic-operation hooks (delete, arrange,
//! vector rewrites) and `board.export_png`.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::super::export::{self, ExportFormat};
use super::super::job_admission::settlement_json;
use super::super::render::Prefer;
use super::super::text::{self, TextEntry};
use super::super::vector::{self, PathRecord, VectorEntry, VectorStyle};
use super::{
    CONNECTOR_COLOUR, CONNECTOR_WIDTH, Ctx, EXPORT_MARGIN, LABEL_PT, SHAPE_TEXT_INSET, TEXT_INSET, TEXT_PT, Unresolved, anchor_path, connector_route, details, finite_coordinate, fitted_size,
    mint_path, outline, project, rect_json, rects_of, refused, route_points, text_baseline, text_ok, translated, uuid_parts,
};
use crate::PlacementWrite;
use hsk_studio_composite::board::{self as geo, BoardRect};
use hsk_studio_folio::{self as folio, Payload, StudioAreaText, StudioBoardNode, StudioBoardNodeFields, StudioBoardPoint, StudioConnectorCap, StudioConnectorEndpoint, StudioConnectorLineType, StudioDocument};
use hsk_studio_nib::anchor_path::AnchorPath;
use hsk_studio_session::{AdmittedJob, JobOutcome};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The number of lines a story of `t` starts with (its `\n` breaks; a wrap adds lines inside the node).
fn line_count(t: &str) -> usize {
    t.split('\n').count()
}

impl Viewer {
    // ----- text --------------------------------------------------------------------------------

    /// The font family board text uses: the bundled default, else the first registered one.
    pub(super) fn board_font(&self) -> Result<&text::FontFile, String> {
        self.fonts.iter().find(|f| f.family == text::DEFAULT_FONT).or_else(|| self.fonts.first()).ok_or_else(|| refused("board_font_unavailable", "no font registered"))
    }

    /// The advance of each line of `t` (split at `\n`) set at `size` points.
    pub(super) fn board_line_widths(&self, t: &str, size: f64) -> Result<Vec<f64>, String> {
        let font = self.board_font()?;
        let token = self.composition_token()?;
        t.split('\n')
            .map(|line| if line.is_empty() { Ok(0.0) } else { text::shaped_glyphs(font, line, size, token, self.run.actor(), self.cancel).map(|g| g.iter().map(|g| g.x_advance_pt).sum()) })
            .collect()
    }

    /// A fresh area story of `t` with its two styles in `entry`, under ids no story or style of the text
    /// entry holds (so an undo of the binding returns to the old story): the story id.
    pub(super) fn board_story(&self, doc: &StudioDocument, entry: &mut TextEntry, t: &str, size: f64, align: &str) -> Result<String, String> {
        if entry.stories.len() >= text::MAX_TEXT_LAYERS {
            return Err(refused("text_entry_over_budget", format!("the text entry holds at most {} stories", text::MAX_TEXT_LAYERS)));
        }
        let family = self.board_font()?.family.clone();
        let (high, low) = uuid_parts(&doc.document_id);
        let taken = |e: &TextEntry, id: &str| e.stories.iter().any(|s| s.story_id == id) || e.styles.style(id).is_some();
        let mut n = u64::try_from(entry.stories.len()).unwrap_or(0);
        let (story_id, paragraph, character) = loop {
            let ids = (format!("STXT-{high}-{low}-7ede-b000-{n:012x}"), format!("STYS-{high}-{low}-7ede-b400-{n:012x}"), format!("STYS-{high}-{low}-7ede-b800-{n:012x}"));
            if !(taken(entry, &ids.0) || taken(entry, &ids.1) || taken(entry, &ids.2)) {
                break ids;
            }
            n = n.wrapping_add(1);
        };
        let profile = serde_json::to_value(&doc.working_space_rgb).unwrap_or(Value::Null);
        let len = t.len();
        // One paragraph per line: each `\n` ends its paragraph (review F12; the partition tiles the text).
        let mut paragraphs = Vec::new();
        let mut start = 0_usize;
        for (at, _) in t.match_indices('\n') {
            let end = at.saturating_add(1);
            paragraphs.push(json!({"start": start, "end": end, "style_ref": paragraph, "paragraph_direction": null}));
            start = end;
        }
        if start < len || paragraphs.is_empty() {
            paragraphs.push(json!({"start": start, "end": len, "style_ref": paragraph, "paragraph_direction": null}));
        }
        let story: folio::StudioTextStory = serde_json::from_value(json!({
            "schema_id": "hsk.studio.text_story@1", "story_id": story_id, "text_kind": "area", "story_direction": "left_to_right",
            "content": {"content_kind": "inline", "text_utf8": t},
            "paragraphs": paragraphs,
            "character_ranges": [{"start": 0, "end": len, "style_ref": character, "applied_language": "en"}],
        }))
        .map_err(|e| refused("board_text_invalid", e))?;
        let styles: Vec<folio::StudioTypeStyle> = serde_json::from_value(json!([
            {"schema_id": "hsk.studio.type_style@1", "style_id": paragraph, "scope": "paragraph", "name": "Board/Paragraph", "align": align},
            {"schema_id": "hsk.studio.type_style@1", "style_id": character, "scope": "character", "name": "Board/Character",
             "font_family": family, "font_size": {"value": size, "unit": "pt"},
             "fill": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0, "colour_profile_id": profile}},
        ]))
        .map_err(|e| refused("board_text_invalid", e))?;
        entry.stories.push(story);
        entry.styles.styles.extend(styles);
        entry.validate(doc, self.cancel)?;
        Ok(story_id)
    }

    /// The text size, alignment and inset of a node kind's story.
    pub(super) const fn text_setting(fields: &StudioBoardNodeFields) -> (f64, &'static str, f64) {
        match fields {
            StudioBoardNodeFields::Section { .. } => (LABEL_PT, "left", 12.0),
            StudioBoardNodeFields::ShapeWithText { .. } => (TEXT_PT, "center", SHAPE_TEXT_INSET),
            _ => (TEXT_PT, "center", TEXT_INSET),
        }
    }

    // ----- ids and commits ---------------------------------------------------------------------

    /// A fresh layer id: never one the document holds nor one an entry still names.
    pub(super) fn board_layer_id(&self, doc: &StudioDocument, entry: &VectorEntry) -> Result<String, String> {
        let text_entry = self.text_entry().unwrap_or_else(|_| TextEntry::empty(&doc.document_id));
        let r = &entry.records;
        let mut salt = u64::try_from(doc.layers.len()).unwrap_or(0);
        for _ in 0..4096_u32 {
            let id = super::super::layers::mint_layer_id(doc, salt);
            salt = salt.wrapping_add(1);
            let story = format!("STXT-{}", id.get(5..).unwrap_or_default());
            let named = text_entry.layers.iter().any(|l| l.layer_id == id)
                || text_entry.stories.iter().any(|s| s.story_id == story)
                || r.area_texts.iter().any(|a| a.container_layer_id == id)
                || r.path_texts.iter().any(|p| p.path_layer_id == id)
                || r.board_nodes.iter().any(|n| n.layer_id == id)
                || entry.live.iter().any(|l| l.layer_id == id);
            if !named {
                return Ok(id);
            }
        }
        Err(refused("board_id_exhausted", "no free layer id"))
    }

    /// Adds a path record of `path` and `style`: its fresh id.
    pub(super) fn board_path(doc: &StudioDocument, entry: &mut VectorEntry, path: AnchorPath, style: VectorStyle) -> Result<String, String> {
        if entry.paths.len() >= vector::MAX_PATHS {
            return Err(refused("vector_entry_over_budget", format!("at most {} path records", vector::MAX_PATHS)));
        }
        let id = mint_path(doc, entry);
        entry.paths.push(PathRecord::plain(id.clone(), path, style));
        Ok(id)
    }

    /// Writes `path` and `style` over the record `existing` names, in place (review F2: the step's entry
    /// snapshot keeps the old geometry, so nothing grows with edits), else into a fresh record: its id.
    pub(super) fn board_put_path(doc: &StudioDocument, entry: &mut VectorEntry, existing: Option<&str>, path: AnchorPath, style: VectorStyle) -> Result<String, String> {
        if let Some(id) = existing
            && let Some(record) = entry.paths.iter_mut().find(|p| p.path_id == id)
        {
            *record = PathRecord::plain(id.to_owned(), path, style);
            return Ok(id.to_owned());
        }
        Self::board_path(doc, entry, path, style)
    }

    /// Validates the entries on the document the step produces, writes the text entry (when given),
    /// then commits the structure writes or path changes and the vector entry as ONE step (entry-only
    /// when neither is given).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn board_commit(&mut self, op: &str, doc: &StudioDocument, entry: &VectorEntry, text_entry: Option<&TextEntry>, writes: &[PlacementWrite], changes: &[(String, Vec<String>)], target: &str) -> Result<u64, String> {
        if !vector::entry_persistable() {
            return Err(refused("vector_entry_not_persistable", "the session holds no vector entry token"));
        }
        let created: Vec<folio::StudioLayer> = writes.iter().filter_map(|w| w.create.as_ref().map(|(l, _)| l.clone())).collect();
        let deleted: Vec<String> = writes.iter().filter(|w| w.to.is_none()).map(|w| w.layer_id.clone()).collect();
        let proj = project(doc, changes, &created, &deleted);
        entry.validate(&proj, self.cancel)?;
        entry.validate_records(&proj, self.cancel)?;
        let bytes = entry.to_bytes(&doc.document_id)?;
        if let Some(t) = text_entry {
            t.validate(&proj, self.cancel)?;
            self.run.set_document_entry(text::ENTRY, Some(t.to_bytes()?)).map_err(|e| refused("board_text_entry", e))?;
        }
        let ok = if writes.is_empty() { self.run.set_vector_paths_and_entry(op, changes, Some((vector::ENTRY, bytes))) } else { self.run.restructure_and_entry(op, writes, vector::ENTRY, bytes) };
        self.after_edit(ok, format!("{op} {target}"))
    }

    // ----- create ------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_text_node(&mut self, op: &str, board: Option<&str>, at: [f64; 2], fields: StudioBoardNodeFields, t: &str, asked: [Option<f64>; 2], style: VectorStyle) -> Result<Value, String> {
        text_ok(t, !matches!(fields, StudioBoardNodeFields::Section { .. }))?;
        if !at.iter().all(|v| finite_coordinate(*v)) {
            return Err(refused("board_position_invalid", format!("coordinates within +-{}", folio::BOARD_COORDINATE_LIMIT)));
        }
        let c = self.board_ctx()?;
        c.whiteboard_only()?;
        let board = self.target_board(&c, board)?;
        let (size_pt, align, inset) = Self::text_setting(&fields);
        let size = match (&fields, asked) {
            (StudioBoardNodeFields::Section { .. }, [Some(w), Some(h)]) => [w, h],
            _ => fitted_size(&fields, &self.board_line_widths(t, size_pt)?, asked),
        };
        let rect = BoardRect { x: at[0], y: at[1], w: size[0], h: size[1] };
        let path = anchor_path(&outline(&fields, &rect)?)?;
        let lines = details(&fields, &rect)?;
        let Ctx { doc, mut entry, .. } = c;
        let id = self.board_layer_id(&doc, &entry)?;
        let path_id = Self::board_path(&doc, &mut entry, path, style)?;
        // The outline is the text container; a shape's interior lines are a second path of the layer.
        let mut path_ids = vec![path_id.clone()];
        if let Some(lines) = lines {
            path_ids.push(Self::board_path(&doc, &mut entry, lines, style)?);
        }
        let mut text_entry = self.text_entry()?;
        let story = self.board_story(&doc, &mut text_entry, t, size_pt, align)?;
        let (first_baseline, first_baseline_min) = text_baseline(&fields, &rect, line_count(t));
        entry.records.area_texts.push(StudioAreaText {
            text_id: story.clone(),
            container_layer_id: id.clone(),
            container_id: path_id.clone(),
            inset,
            first_baseline,
            first_baseline_min,
            auto_size: folio::StudioAutoSize::Off,
        });
        let kind = fields.kind();
        entry.records.board_nodes.push(StudioBoardNode { layer_id: id.clone(), board_id: board.clone(), node: fields });
        let name: String = t.split('\n').next().unwrap_or_default().chars().take(32).collect();
        let (parent, place) = self.insertion_point()?;
        let layer = super::super::layers::new_layer(&id, &name, &parent, kind, json!({"payload_kind": "vector", "path_ids": path_ids}))?;
        let operation = super::super::layers::operation_for(&id, false)?;
        let writes = [PlacementWrite { layer_id: id.clone(), to: Some(place), create: Some((layer, operation)) }];
        let revision = self.board_commit(op, &doc, &entry, Some(&text_entry), &writes, &[], &id)?;
        Ok(json!({"node_id": id, "kind": kind, "board_id": board, "bounds": rect_json(&rect), "text_id": story, "revision": revision}))
    }

    /// The node an endpoint names exists and can be attached to (a vector layer, not a connector):
    /// typed refusals.
    pub(super) fn check_endpoint(c: &Ctx, e: &StudioConnectorEndpoint, own: Option<&str>) -> Result<(), String> {
        let point_ok = |p: &StudioBoardPoint| finite_coordinate(p.x) && finite_coordinate(p.y);
        match e {
            StudioConnectorEndpoint::Position(p) if !point_ok(&p.position) => return Err(refused("board_position_invalid", "endpoint position")),
            StudioConnectorEndpoint::PositionAndNode(p) if !point_ok(&p.position) => return Err(refused("board_position_invalid", "endpoint position")),
            _ => {}
        }
        let Some(node) = e.node() else { return Ok(()) };
        if Some(node) == own {
            return Err(refused("connector_endpoint_self", node));
        }
        let layer = c.doc.layers.iter().find(|l| l.layer_id == node).ok_or_else(|| refused("connector_endpoint_node_unresolvable", node))?;
        if !matches!(layer.payload, Payload::Vector { .. }) || matches!(c.record(node).map(|r| &r.node), Some(StudioBoardNodeFields::Connector { .. })) {
            return Err(refused("connector_endpoint_node_not_attachable", node));
        }
        Ok(())
    }

    /// The drawing of a connector record on `rects` (its route and path), or why it does not resolve now.
    pub(super) fn connector_drawing(fields: &StudioBoardNodeFields, rects: &BTreeMap<String, BoardRect>, width: f64) -> Result<(Vec<[f64; 2]>, AnchorPath), Unresolved> {
        let StudioBoardNodeFields::Connector { connector_line_type, connector_start, connector_end, connector_start_stroke_cap, connector_end_stroke_cap, .. } = fields else {
            return Err(Unresolved::Geometry("board_node_kind_mismatch".into()));
        };
        let route = connector_route(connector_start, connector_end, *connector_line_type, rects)?;
        let drawn = geo::connector_drawing(&route, *connector_start_stroke_cap, *connector_end_stroke_cap, width).map_err(|e| Unresolved::Geometry(e.code().into()))?;
        let path = anchor_path(&drawn).map_err(Unresolved::Geometry)?;
        Ok((route, path))
    }

    /// A connector drawing a create or a set must have: an unresolved one is refused typed (a degenerate one
    /// `board_geometry_invalid`).
    fn connector_drawing_now(fields: &StudioBoardNodeFields, rects: &BTreeMap<String, BoardRect>, width: f64) -> Result<(Vec<[f64; 2]>, AnchorPath), String> {
        Self::connector_drawing(fields, rects, width).map_err(|u| match u {
            Unresolved::Broken(ends) => refused("connector_endpoint_node_unresolvable", ends.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(",")),
            Unresolved::Degenerate => refused(geo::BoardGeometryError::Invalid.code(), "both ends meet"),
            Unresolved::Geometry(code) if code.starts_with("board:") => code,
            Unresolved::Geometry(code) => refused(&code, "connector route"),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_connector(&mut self, op: &str, board: Option<&str>, start: StudioConnectorEndpoint, end: StudioConnectorEndpoint, line_type: StudioConnectorLineType, caps: [StudioConnectorCap; 2], stroke: Option<[u8; 3]>, width: Option<f64>) -> Result<Value, String> {
        let width = width.unwrap_or(CONNECTOR_WIDTH);
        if !(width.is_finite() && width > 0.0 && width <= vector::MAX_STROKE_WIDTH) {
            return Err(refused("board_stroke_width_invalid", format!("in (0, {}]", vector::MAX_STROKE_WIDTH)));
        }
        if line_type == StudioConnectorLineType::Curved {
            return Err(refused(geo::BoardGeometryError::LineTypeNotAvailable.code(), "CURVED is not drawn in this build"));
        }
        let c = self.board_ctx()?;
        c.whiteboard_only()?;
        let board = self.target_board(&c, board)?;
        Self::check_endpoint(&c, &start, None)?;
        Self::check_endpoint(&c, &end, None)?;
        let mut fields = StudioBoardNodeFields::Connector {
            connector_line_type: line_type,
            connector_start: start,
            connector_end: end,
            connector_start_stroke_cap: caps[0],
            connector_end_stroke_cap: caps[1],
            corner_radius: None,
            rotation: 0.0,
            last_route: Vec::new(),
        };
        let (route, path) = Self::connector_drawing_now(&fields, &c.rects(&BTreeMap::new()), width)?;
        if let StudioBoardNodeFields::Connector { last_route, .. } = &mut fields {
            *last_route = route_points(&route);
        }
        let colour = stroke.unwrap_or(CONNECTOR_COLOUR);
        let style = VectorStyle { fill: Some(colour), stroke: Some(colour), stroke_width: width };
        let Ctx { doc, mut entry, .. } = c;
        let id = self.board_layer_id(&doc, &entry)?;
        let path_id = Self::board_path(&doc, &mut entry, path, style)?;
        entry.records.board_nodes.push(StudioBoardNode { layer_id: id.clone(), board_id: board.clone(), node: fields });
        let (parent, place) = self.insertion_point()?;
        let layer = super::super::layers::new_layer(&id, "Connector", &parent, "connector", json!({"payload_kind": "vector", "path_ids": [path_id]}))?;
        let operation = super::super::layers::operation_for(&id, false)?;
        let writes = [PlacementWrite { layer_id: id.clone(), to: Some(place), create: Some((layer, operation)) }];
        let revision = self.board_commit(op, &doc, &entry, None, &writes, &[], &id)?;
        Ok(json!({"node_id": id, "kind": "connector", "board_id": board, "route": route, "revision": revision}))
    }

    // ----- edit --------------------------------------------------------------------------------

    /// Redraws node `node` of `c` (its outline in `rect`, its current style unless `style`) over its own
    /// path records (review F2) and keeps its area-text binding on the outline. The layer's path list
    /// changes only when the interior lines come or go: that change, if any.
    pub(super) fn board_redraw(c: &mut Ctx, node: &str, fields: &StudioBoardNodeFields, rect: &BoardRect, style: Option<VectorStyle>, lines: usize) -> Result<Option<(String, Vec<String>)>, String> {
        let old = c.layer_paths(node).ok_or_else(|| refused("board_node_unknown", node))?;
        let path = anchor_path(&outline(fields, rect)?)?;
        let interior = details(fields, rect)?;
        let style = style.or_else(|| c.style(node)).ok_or_else(|| refused("board_node_unknown", node))?;
        let id = Self::board_put_path(&c.doc, &mut c.entry, old.first().map(String::as_str), path, style)?;
        let (first_baseline, first_baseline_min) = text_baseline(fields, rect, lines);
        for a in c.entry.records.area_texts.iter_mut().filter(|a| a.container_layer_id == node) {
            a.container_id.clone_from(&id);
            a.first_baseline = first_baseline;
            a.first_baseline_min = first_baseline_min;
        }
        let mut ids = vec![id];
        match (interior, old.get(1)) {
            (Some(p), existing) => ids.push(Self::board_put_path(&c.doc, &mut c.entry, existing.map(String::as_str), p, style)?),
            // Lines gone (a shape type change): their record leaves with them (the step's snapshot keeps it).
            (None, Some(gone)) => c.entry.paths.retain(|p| &p.path_id != gone),
            (None, None) => {}
        }
        Ok((ids != old).then(|| (node.to_owned(), ids)))
    }

    pub(super) fn set_record(c: &mut Ctx, node: &str, fields: StudioBoardNodeFields) {
        if let Some(r) = c.entry.records.board_nodes.iter_mut().find(|r| r.layer_id == node) {
            r.node = fields;
        }
    }

    /// Reroutes every live connector attached to a node of `moved` over its own path record (`rects` hold the
    /// moved bounds): the rerouted connectors. A connector that does not resolve now (broken, degenerate:
    /// review F5) keeps its drawing and never blocks the edit; a read reports it.
    pub(super) fn reroute(c: &mut Ctx, rects: &BTreeMap<String, BoardRect>, moved: &BTreeSet<String>) -> Result<Vec<String>, String> {
        let attached: Vec<StudioBoardNode> = c
            .entry
            .records
            .board_nodes
            .iter()
            .filter(|n| c.doc.layers.iter().any(|l| l.layer_id == n.layer_id))
            .filter(|n| matches!(&n.node, StudioBoardNodeFields::Connector { connector_start, connector_end, .. } if [connector_start, connector_end].iter().any(|e| e.node().is_some_and(|x| moved.contains(x)))))
            .cloned()
            .collect();
        let mut rerouted = Vec::new();
        for n in attached {
            let style = c.style(&n.layer_id).ok_or_else(|| refused("board_node_unknown", &n.layer_id))?;
            let Ok((route, path)) = Self::connector_drawing(&n.node, rects, style.stroke_width.max(f64::MIN_POSITIVE)) else { continue };
            let own = c.layer_paths(&n.layer_id).and_then(|p| p.first().cloned());
            let id = Self::board_put_path(&c.doc, &mut c.entry, own.as_deref(), path, style)?;
            if own.as_deref() != Some(id.as_str()) {
                return Err(refused("board_geometry_invalid", format!("connector {} holds no path record", n.layer_id)));
            }
            let mut fields = n.node.clone();
            if let StudioBoardNodeFields::Connector { last_route, .. } = &mut fields {
                *last_route = route_points(&route);
            }
            Self::set_record(c, &n.layer_id, fields);
            rerouted.push(n.layer_id.clone());
        }
        Ok(rerouted)
    }

    /// Changes a node's fields with `edit`, then redraws it fitted to its text in its place (top-left
    /// kept; a section keeps its size): ONE step.
    pub(super) fn edit_node(&mut self, op: &str, node: &str, edit: impl FnOnce(&mut StudioBoardNodeFields) -> Result<(), String>) -> Result<Value, String> {
        self.edit_node_text(op, node, None, edit)
    }

    /// [`Self::edit_node`] with a new text (a fresh story bound in the same step).
    pub(super) fn edit_node_text(&mut self, op: &str, node: &str, new_text: Option<&str>, edit: impl FnOnce(&mut StudioBoardNodeFields) -> Result<(), String>) -> Result<Value, String> {
        let mut c = self.board_ctx()?;
        let rec = c.node(node)?;
        let mut fields = rec.node.clone();
        edit(&mut fields)?;
        let rect = c.rect(node).ok_or_else(|| refused("board_node_unknown", node))?;
        let text_entry_now = self.text_entry()?;
        let current_text = c.text_id(node).and_then(|id| text_entry_now.stories.iter().find(|s| s.story_id == id).and_then(text::story_text).map(str::to_owned)).unwrap_or_default();
        let t = new_text.map_or(current_text, str::to_owned);
        let (size_pt, align, _) = Self::text_setting(&fields);
        let new_rect = match &fields {
            StudioBoardNodeFields::Section { .. } => rect,
            StudioBoardNodeFields::Connector { .. } => return Err(refused("board_node_kind_mismatch", "use board.connector.set")),
            StudioBoardNodeFields::ShapeWithText { .. } => {
                // A shape keeps a size larger than its text.
                let size = fitted_size(&fields, &self.board_line_widths(&t, size_pt)?, [None, None]);
                BoardRect { x: rect.x, y: rect.y, w: size[0].max(rect.w), h: size[1].max(rect.h) }
            }
            StudioBoardNodeFields::Sticky { .. } => {
                let size = fitted_size(&fields, &self.board_line_widths(&t, size_pt)?, [None, None]);
                BoardRect { x: rect.x, y: rect.y, w: size[0], h: size[1] }
            }
        };
        let mut text_entry = None;
        if let Some(t) = new_text {
            let mut te = text_entry_now;
            let story = self.board_story(&c.doc, &mut te, t, size_pt, align)?;
            for a in c.entry.records.area_texts.iter_mut().filter(|a| a.container_layer_id == node) {
                a.text_id.clone_from(&story);
            }
            text_entry = Some(te);
        }
        let changes: Vec<(String, Vec<String>)> = Self::board_redraw(&mut c, node, &fields, &new_rect, None, line_count(&t))?.into_iter().collect();
        Self::set_record(&mut c, node, fields);
        if new_rect != rect {
            let mut rects = c.rects(&BTreeMap::new());
            rects.insert(node.to_owned(), new_rect);
            Self::reroute(&mut c, &rects, &BTreeSet::from([node.to_owned()]))?;
        }
        let Ctx { doc, entry, .. } = c;
        let revision = self.board_commit(op, &doc, &entry, text_entry.as_ref(), &[], &changes, node)?;
        let after = self.board_ctx()?;
        let rec = after.node(node)?;
        let rects = after.rects(&BTreeMap::new());
        let te = self.text_entry().ok();
        let mut v = self.node_json(&after, &rec, &rects, &self.text_overset(), te.as_ref());
        v["revision"] = json!(revision);
        Ok(v)
    }

    pub(super) fn board_restyle(&mut self, op: &str, node: &str, fill: [u8; 3]) -> Result<Value, String> {
        let mut c = self.board_ctx()?;
        let rec = c.node(node)?;
        let rect = c.rect(node).ok_or_else(|| refused("board_node_unknown", node))?;
        let mut style = c.style(node).ok_or_else(|| refused("board_node_unknown", node))?;
        style.fill = Some(fill);
        let lines = c.text_id(node).and_then(|id| self.text_entry().ok()?.stories.iter().find(|s| s.story_id == id).and_then(text::story_text).map(line_count)).unwrap_or(1);
        let changes: Vec<(String, Vec<String>)> = Self::board_redraw(&mut c, node, &rec.node, &rect, Some(style), lines)?.into_iter().collect();
        let Ctx { doc, entry, .. } = c;
        let revision = self.board_commit(op, &doc, &entry, None, &[], &changes, node)?;
        Ok(json!({"node_id": node, "fill": fill, "revision": revision}))
    }

    pub(super) fn set_connector(&mut self, op: &str, node: &str, start: Option<StudioConnectorEndpoint>, end: Option<StudioConnectorEndpoint>, line_type: Option<StudioConnectorLineType>, caps: [Option<StudioConnectorCap>; 2]) -> Result<Value, String> {
        if line_type == Some(StudioConnectorLineType::Curved) {
            return Err(refused(geo::BoardGeometryError::LineTypeNotAvailable.code(), "CURVED is not drawn in this build"));
        }
        let mut c = self.board_ctx()?;
        let rec = c.node(node)?;
        let mut fields = rec.node.clone();
        let StudioBoardNodeFields::Connector { connector_line_type, connector_start, connector_end, connector_start_stroke_cap, connector_end_stroke_cap, .. } = &mut fields else {
            return Err(refused("board_node_kind_mismatch", "not a connector"));
        };
        if let Some(e) = start {
            Self::check_endpoint(&c, &e, Some(node))?;
            *connector_start = e;
        }
        if let Some(e) = end {
            Self::check_endpoint(&c, &e, Some(node))?;
            *connector_end = e;
        }
        if let Some(l) = line_type {
            *connector_line_type = l;
        }
        if let Some(x) = caps[0] {
            *connector_start_stroke_cap = x;
        }
        if let Some(x) = caps[1] {
            *connector_end_stroke_cap = x;
        }
        let style = c.style(node).ok_or_else(|| refused("board_node_unknown", node))?;
        let (route, path) = Self::connector_drawing_now(&fields, &c.rects(&BTreeMap::new()), style.stroke_width.max(f64::MIN_POSITIVE))?;
        if let StudioBoardNodeFields::Connector { last_route, .. } = &mut fields {
            *last_route = route_points(&route);
        }
        let own = c.layer_paths(node).and_then(|p| p.first().cloned());
        let id = Self::board_put_path(&c.doc, &mut c.entry, own.as_deref(), path, style)?;
        Self::set_record(&mut c, node, fields);
        let changes = if own.as_deref() == Some(id.as_str()) { Vec::new() } else { vec![(node.to_owned(), vec![id])] };
        let Ctx { doc, entry, .. } = c;
        let revision = self.board_commit(op, &doc, &entry, None, &[], &changes, node)?;
        Ok(json!({"node_id": node, "route": route, "revision": revision}))
    }

    /// `board.node.move`: the node (a section with its members) translated over its own path records, and
    /// its connectors rerouted, as ONE entry step (review F2, F5).
    pub(super) fn move_node(&mut self, op: &str, node: &str, to: [f64; 2]) -> Result<Value, String> {
        if !to.iter().all(|v| finite_coordinate(*v)) {
            return Err(refused("board_position_invalid", format!("coordinates within +-{}", folio::BOARD_COORDINATE_LIMIT)));
        }
        let mut c = self.board_ctx()?;
        let rec = c.node(node)?;
        if matches!(rec.node, StudioBoardNodeFields::Connector { .. }) {
            return Err(refused("board_node_not_movable", "a connector follows its endpoints (board.connector.set)"));
        }
        let rects = c.rects(&BTreeMap::new());
        let rect = *rects.get(node).ok_or_else(|| refused("board_node_unknown", node))?;
        let d = [to[0] - rect.x, to[1] - rect.y];
        let mut moving = vec![node.to_owned()];
        if matches!(rec.node, StudioBoardNodeFields::Section { .. }) {
            moving.extend(Self::section_members(&c, node, &rects));
        }
        for m in &moving {
            let paths = c.layer_paths(m).ok_or_else(|| refused("board_node_unknown", m))?;
            for p in &paths {
                let record = c.entry.paths.iter_mut().find(|r| &r.path_id == p).ok_or_else(|| refused("vector_path_unresolved", p))?;
                *record = PathRecord::plain(p.clone(), translated(&record.path, d), record.style);
            }
        }
        // The records are moved already: the bounds now are the moved bounds.
        let moved = c.rects(&BTreeMap::new());
        let rerouted = Self::reroute(&mut c, &moved, &moving.iter().cloned().collect())?;
        let Ctx { doc, entry, .. } = c;
        let revision = self.board_commit(op, &doc, &entry, None, &[], &[], node)?;
        Ok(json!({"node_id": node, "moved": moving, "rerouted": rerouted, "bounds": rect_json(&BoardRect { x: to[0], y: to[1], ..rect }), "revision": revision}))
    }

    pub(super) fn delete_node(&mut self, op: &str, node: &str) -> Result<Value, String> {
        let c = self.board_ctx()?;
        c.node(node)?;
        let Ctx { doc, mut entry, .. } = c;
        entry.records.board_nodes.retain(|n| n.layer_id != node);
        entry.records.area_texts.retain(|a| a.container_layer_id != node);
        let writes = [PlacementWrite { layer_id: node.to_owned(), to: None, create: None }];
        let revision = self.board_commit(op, &doc, &entry, None, &writes, &[], node)?;
        let broken: Vec<&String> = entry
            .records
            .board_nodes
            .iter()
            .filter(|n| matches!(&n.node, StudioBoardNodeFields::Connector { connector_start, connector_end, .. } if [connector_start, connector_end].iter().any(|e| e.node() == Some(node))))
            .map(|n| &n.layer_id)
            .collect();
        Ok(json!({"node_id": node, "broken_connectors": broken, "revision": revision}))
    }

    // ----- generic operations on board nodes (review F1, F6) -------------------------------------

    /// The vector entry a layer delete of `deleted` leaves (review F1): `candidate` (the entry the delete
    /// already writes, else the current one) without the board records and text bindings of deleted board
    /// nodes, so the delete's own undo step carries them. `candidate` unchanged when no board node goes.
    pub(in crate::view) fn board_records_after_delete(&self, deleted: &[String], candidate: Option<Vec<u8>>) -> Option<Vec<u8>> {
        let Some(snap) = self.run.snapshot() else { return candidate };
        let doc = snap.document();
        let entry = match &candidate {
            Some(bytes) => VectorEntry::decode(bytes, doc, self.cancel),
            None => self.vector_entry(),
        };
        let Ok(mut entry) = entry else { return candidate };
        let gone: BTreeSet<&str> = deleted.iter().map(String::as_str).collect();
        let nodes: BTreeSet<String> = entry.records.board_nodes.iter().filter(|n| gone.contains(n.layer_id.as_str())).map(|n| n.layer_id.clone()).collect();
        if nodes.is_empty() || !vector::entry_persistable() {
            return candidate;
        }
        entry.records.board_nodes.retain(|n| !nodes.contains(&n.layer_id));
        entry.records.area_texts.retain(|a| !nodes.contains(&a.container_layer_id));
        entry.to_bytes(&doc.document_id).ok().or(candidate)
    }

    /// Review F6: pathfinder, Create Outlines and Outline Stroke rewrite a layer into plain vector paths, which
    /// would leave a board node's record behind: refused `board_node_not_vector_editable` (a "convert to plain
    /// vector" command is a next-lane candidate).
    pub(in crate::view) fn board_refuse_vector_rewrite(&self, op: &str, layers: &[String]) -> Result<(), String> {
        let Ok(entry) = self.vector_entry() else { return Ok(()) };
        match layers.iter().find(|l| entry.records.board_nodes.iter().any(|n| &n.layer_id == *l)) {
            Some(l) => Err(format!("{op}: {}", refused("board_node_not_vector_editable", l))),
            None => Ok(()),
        }
    }

    /// Review F6: after align, distribute or transform wrote `writes` (each layer's fresh path ids, in the
    /// order of its old ones) into `entry`, a board node's text stays bound to its outline and every connector
    /// attached to a written node reroutes over its own path record, in the same step.
    pub(in crate::view) fn board_after_transform(&self, doc: &StudioDocument, entry: &mut VectorEntry, writes: &[(String, Vec<String>)]) -> Result<(), String> {
        let board: BTreeSet<&str> = entry.records.board_nodes.iter().map(|n| n.layer_id.as_str()).collect();
        let written: Vec<&(String, Vec<String>)> = writes.iter().filter(|(l, _)| board.contains(l.as_str())).collect();
        if written.is_empty() {
            return Ok(());
        }
        let old: BTreeMap<&str, Vec<String>> = doc
            .layers
            .iter()
            .filter_map(|l| match &l.payload {
                Payload::Vector { path_ids } => Some((l.layer_id.as_str(), path_ids.clone())),
                _ => None,
            })
            .collect();
        for (layer, ids) in &written {
            let Some(before) = old.get(layer.as_str()) else { continue };
            for a in entry.records.area_texts.iter_mut().filter(|a| &a.container_layer_id == layer) {
                if let Some(at) = before.iter().position(|p| p == &a.container_id)
                    && let Some(new) = ids.get(at)
                {
                    a.container_id.clone_from(new);
                }
            }
        }
        let proj = project(doc, writes, &[], &[]);
        let mut c = Ctx { doc: proj, entry: std::mem::replace(entry, VectorEntry::empty()), whiteboard: true };
        let rects = rects_of(&c.doc, &c.entry, &BTreeMap::new());
        let moved: BTreeSet<String> = written.iter().map(|(l, _)| l.clone()).collect();
        let result = Self::reroute(&mut c, &rects, &moved);
        *entry = c.entry;
        result.map(|_| ())
    }

    // ----- export ------------------------------------------------------------------------------

    /// `board.export_png` ([STU-WB-127], [STU-WB-134] "capture the board region to bytes"): the board's
    /// content bounds (its nodes' union plus [`EXPORT_MARGIN`]; the board's own bounds when it holds none
    /// or `content_bounds` is false) rendered on the CPU path and written as PNG, admitted through the
    /// viewer's JobPort before the first byte. The document does not change.
    pub(super) fn board_export_png(&mut self, board: Option<&str>, path: &Path, overwrite: bool, content_bounds: bool) -> Result<Value, String> {
        if path.as_os_str().is_empty() {
            return Err(refused("export_path_missing", "no output path"));
        }
        if path.exists() && !overwrite {
            return Err(refused("overwrite_required", path.display()));
        }
        let job = self.admit_export_job().map_err(|r| refused(r.code, r.detail))?;
        self.board_export_admitted(job, board, path, content_bounds)
    }

    /// The export of an admitted `job`, settled once with its outcome (review F13): `completed` (its
    /// settlement as `job` in the result), `canceled` when the run's token is, else `failed`.
    pub(super) fn board_export_admitted(&mut self, job: AdmittedJob, board: Option<&str>, path: &Path, content_bounds: bool) -> Result<Value, String> {
        let result = self.board_export_inner(board, path, content_bounds);
        let canceled = self.cancel.check().is_err();
        match result {
            Ok(mut report) => {
                report["job"] = settlement_json(&job.complete(self.document_head()));
                Ok(report)
            }
            Err(e) if canceled => {
                job.settle(JobOutcome::Canceled);
                Err(if e.starts_with("board: canceled") { e } else { refused("canceled", e) })
            }
            Err(e) => {
                job.settle(JobOutcome::Failed("board_export_failed"));
                Err(e)
            }
        }
    }

    pub(super) fn board_export_inner(&mut self, board: Option<&str>, path: &Path, content_bounds: bool) -> Result<Value, String> {
        self.board_cancel_check()?;
        let c = self.board_ctx()?;
        let board = self.target_board(&c, board)?;
        let at = c.doc.artboards.iter().position(|a| a.artboard_id == board).ok_or_else(|| refused("board_unknown", &board))?;
        let mut region: Option<[f64; 4]> = None;
        if content_bounds {
            let rects = c.rects(&BTreeMap::new());
            for n in c.entry.records.board_nodes.iter().filter(|n| n.board_id == board) {
                let Some(r) = rects.get(&n.layer_id) else { continue };
                region = Some(match region {
                    None => [r.x, r.y, r.x + r.w, r.y + r.h],
                    Some(a) => [a[0].min(r.x), a[1].min(r.y), a[2].max(r.x + r.w), a[3].max(r.y + r.h)],
                });
            }
        }
        let mut copy = c.doc.clone();
        let mut frame = copy.artboards.remove(at);
        if let Some(a) = region {
            let (x, y) = ((a[0] - EXPORT_MARGIN).floor(), (a[1] - EXPORT_MARGIN).floor());
            frame.bounds.x.value = x;
            frame.bounds.y.value = y;
            frame.bounds.width.value = ((a[2] + EXPORT_MARGIN).ceil() - x).max(1.0);
            frame.bounds.height.value = ((a[3] + EXPORT_MARGIN).ceil() - y).max(1.0);
        }
        let bounds = [frame.bounds.x.value, frame.bounds.y.value, frame.bounds.width.value, frame.bounds.height.value];
        copy.artboards.insert(0, frame);
        let snap = folio::validate_document(copy, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| refused("board_export_view", format!("{:?} at {}", d.code, d.target)))?;
        let text = self.prepared_text()?;
        let vectors = self.prepared_vectors()?;
        let frame = super::super::render::render_full_with_records(&snap, &self.assets, Some(&text), vectors.as_ref(), self.records.as_ref(), Prefer::Cpu, None, None, self.cancel).map_err(|e| refused("board_export_render", e))?;
        let exported = export::encode_frame(&frame, ExportFormat::Png).map_err(|code| refused(code, "png encode"))?;
        // Nothing reaches the disk once the run is canceled.
        self.board_cancel_check()?;
        export::write_atomic(path, &exported.bytes).map_err(|code| refused(code, path.display()))?;
        Ok(json!({"board_id": board, "path": path.to_string_lossy(), "bytes": exported.bytes.len(), "width": frame.width, "height": frame.height, "bounds": bounds, "revision": frame.revision, "render_notes": frame.notes}))
    }
}
