//! Storage of resolved `clip-path`, `mask` and pattern references in the Folio vector records
//! (`hsk.studio.vector_records@1`, Folio ENG-7).
//!
//! A reference that `masking` could not prove to be a no-op is built here:
//! - a `clipPath` becomes a hidden group layer under the artboard whose members are the clip shapes
//!   converted to document points (one vector layer per member shape, winding = the clip rule),
//!   a `StudioVectorClip` naming that group, and a `StudioMaskAttachment` on the clipped layer;
//! - a `mask` becomes a hidden group layer holding the mask content walked with the normal import
//!   (paint included), a `StudioVectorMask` (mode from `mask-type`, region from `x/y/width/height`
//!   in document points) and an attachment;
//! - a pattern paint becomes a `StudioPattern` (`definition` model: tile artwork as a hidden group
//!   of the converted tile content in tile space) and a `StudioPatternPaint` that binds the style
//!   row of the painted fill or stroke to it. The row keeps the author's fallback colour (or a
//!   colour taken from the tile) as stand-in for readers of the design records alone.
//!
//! Tile space: points, origin at the top-left corner of the tile cell, one SVG user unit of the
//! pattern's content space = `px_to_pt` points. The placement matrix of a binding maps tile space
//! to document points and contains the element's own matrix, `patternTransform` and the cell
//! origin (`x`, `y`), so every use of one pattern shares one tile artwork. `objectBoundingBox`
//! units are converted with the bounding box of the element that uses them; the converted values
//! are the key of the shared record, so two elements with different boxes get two records.
//!
//! Content is built in a *frame*: the importer's layer, path and outset vectors are swapped for
//! fresh ones while the definition content is walked, so the geometry of a definition does not mix
//! with the document's own (bounding boxes and containment checks only see the element). A
//! definition that cannot be stored is rolled back to its `Mark` and reported as a typed loss.
//!
//! Work is bounded: nesting stops at `MAX_REFERENCE_DEPTH`, a loop is refused, a definition uses at
//! most `Limits::max_nodes` elements and `max_segments` segments in total (soft failure: that use is
//! kept as a typed loss), a clipPath has at most `MAX_CLIP_CHILDREN` members, and identical uses
//! share one record (`DefKey`).
use super::css;
use super::masking::{
    Applied, MAX_CLIP_CHILDREN, MAX_REFERENCE_DEPTH, PatternSpec, Target, mask_alpha,
    non_none_prop, rect_contains, user_len,
};
use super::*;
use hsk_studio_folio::{StudioClipRule, StudioMaskMode, StudioPatternSource, StudioTileType};

/// The vector records while the import builds them.
#[derive(Default)]
pub(super) struct Records {
    pub(super) patterns: Vec<StudioPattern>,
    pub(super) pattern_paints: Vec<StudioPatternPaint>,
    pub(super) clips: Vec<StudioVectorClip>,
    pub(super) masks: Vec<StudioVectorMask>,
    pub(super) attachments: Vec<StudioMaskAttachment>,
}

impl Records {
    pub(super) fn into_folio(self, document_id: String) -> StudioVectorRecords {
        let mut records = StudioVectorRecords::new(document_id);
        records.patterns = self.patterns;
        records.pattern_paints = self.pattern_paints;
        records.clips = self.clips;
        records.masks = self.masks;
        records.attachments = self.attachments;
        records
    }
}

/// What a stored definition is shared by: the definition element plus everything its geometry
/// depends on (the element matrix, and the object box only when a unit needs it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DefKey {
    Clip(u32, [u64; 6], [u64; 4]),
    Mask(u32, [u64; 6], [u64; 4]),
    /// Pattern element, tile size in user units, content map.
    Pattern(u32, [u64; 2], [u64; 6]),
    /// A pattern element used through an `href` template of a stored pattern.
    Template(u32),
}

impl DefKey {
    /// Node id of the definition element.
    pub(super) fn node(&self) -> u32 {
        match self {
            Self::Clip(n, ..) | Self::Mask(n, ..) | Self::Pattern(n, ..) | Self::Template(n) => *n,
        }
    }
}

fn bits6(m: Affine) -> [u64; 6] {
    [m.a, m.b, m.c, m.d, m.e, m.f].map(f64::to_bits)
}

fn bits4(b: Option<[f64; 4]>) -> [u64; 4] {
    b.unwrap_or([0.0; 4]).map(f64::to_bits)
}

/// Result of trying to store a `clip-path` or `mask` reference.
pub(super) enum Stored {
    /// Set-local id of the clip or mask record.
    Id(String),
    /// The reference removes everything the element paints.
    Empty,
    /// Not stored; the code names why.
    Refused(&'static str),
}

/// Result of trying to store a pattern paint.
pub(super) enum PatternStored {
    Refused(&'static str),
    Placed {
        pattern_id: String,
        /// Tile space -> document points.
        matrix: [f64; 6],
        /// First solid colour of the tile artwork, if any.
        sample: Option<StudioColor>,
    },
}

/// Where a pattern is used.
pub(super) struct Use {
    /// User space of the painted element -> document points.
    pub(super) m: Affine,
    /// Object bounding box of the element in its user space.
    pub(super) bbox: Option<[f64; 4]>,
    pub(super) depth: usize,
}

/// Importer vectors that a definition frame swaps out.
struct Frame {
    layers: Vec<StudioLayer>,
    paths: Vec<VectorPath>,
    outsets: Vec<f64>,
}

/// Lengths of the importer's collections, to cut back a definition that failed.
pub(super) struct Mark {
    def_layers: usize,
    def_paths: usize,
    patterns: usize,
    pattern_paints: usize,
    clips: usize,
    masks: usize,
    attachments: usize,
    nodes: usize,
    styles: usize,
    provenance: usize,
    design_units: usize,
    loss: usize,
    cache_log: usize,
}

/// Failures of a definition's own content that leave the import intact: the use stays a typed loss.
fn soft(e: &VectorError) -> Option<&'static str> {
    match e {
        VectorError::InvalidGeometry => Some("definition_geometry_out_of_range"),
        VectorError::BudgetExceeded("depth") => Some("definition_too_deep"),
        VectorError::BudgetExceeded("definition_elements" | "definition_segments") => {
            Some("definition_budget")
        }
        _ => None,
    }
}

/// A label that satisfies Folio's rules (non-empty, at most 128 bytes, no control characters).
fn short_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_control() {
            continue;
        }
        if out.len().saturating_add(c.len_utf8()) > 128 {
            break;
        }
        out.push(c);
    }
    if out.is_empty() {
        "definition".into()
    } else {
        out
    }
}

/// The `transform` and the geometry lengths of a clip member parse to finite user units.
fn member_understood(el: Node) -> bool {
    if plain_attr(el, "transform").is_some_and(|t| parse_transform(t).is_none()) {
        return false;
    }
    let lengths: &[&str] = match el.tag_name().name() {
        "rect" => &["x", "y", "width", "height", "rx", "ry"],
        "circle" => &["cx", "cy", "r"],
        "ellipse" => &["cx", "cy", "rx", "ry"],
        _ => &[],
    };
    lengths.iter().all(|name| user_len(el, name).is_some())
}

fn layer_parent(id: &str) -> Parent {
    Parent::Node(NodeRef {
        node_kind: NodeKind::Layer,
        node_id: id.to_owned(),
    })
}

fn winding_rule(w: Winding) -> StudioClipRule {
    if w == Winding::EvenOdd {
        StudioClipRule::EvenOdd
    } else {
        StudioClipRule::NonZero
    }
}

/// One shape that contributes to a clip.
enum Member<'d> {
    Shape(Node<'d, 'd>),
    /// `use` of a shape: the use element, the shape and the `x`/`y` shift.
    Use {
        el: Node<'d, 'd>,
        target: Node<'d, 'd>,
        shift: Affine,
    },
}

struct ClipPlan<'d> {
    members: Vec<Member<'d>>,
    /// The rule every member is built with.
    rule: Winding,
    /// Clip content space -> element user space.
    content_map: Affine,
    units_bbox: bool,
    /// `clip-path` on the clipPath itself.
    nested: Option<String>,
}

enum ClipPlanned<'d> {
    Plan(ClipPlan<'d>),
    /// No member contributes: the clip removes everything.
    Empty,
    Refused(&'static str),
}

struct MaskPlan {
    alpha: bool,
    /// Mask content space -> element user space.
    content_map: Affine,
    units_bbox: bool,
    /// `[x0, y0, x1, y1]` of the mask region in document points.
    region: [f64; 4],
    /// The map is axis-aligned, so the region rectangle is exact.
    exact: bool,
}

enum MaskPlanned {
    Plan(MaskPlan),
    Refused(&'static str),
}

impl<'d> Importer<'_, 'd> {
    // ------------------------------------------------------------------------------------------
    // Bookkeeping shared by the three kinds
    // ------------------------------------------------------------------------------------------

    /// Charges one element of definition content to its allowance.
    fn spend_inst(&mut self) -> Result<()> {
        self.inst_elements = self.inst_elements.saturating_add(1);
        if self.inst_elements > self.limits.max_nodes {
            return Err(VectorError::BudgetExceeded("definition_elements"));
        }
        Ok(())
    }

    fn next_set_id(&mut self, prefix: &str) -> String {
        self.ordinal = self.ordinal.saturating_add(1);
        format!("{prefix}-{}", self.ordinal)
    }

    fn mark(&self) -> Mark {
        Mark {
            def_layers: self.def_layers.len(),
            def_paths: self.def_paths.len(),
            patterns: self.records.patterns.len(),
            pattern_paints: self.records.pattern_paints.len(),
            clips: self.records.clips.len(),
            masks: self.records.masks.len(),
            attachments: self.records.attachments.len(),
            nodes: self.nodes.len(),
            styles: self.styles.len(),
            provenance: self.gradient_provenance.len(),
            design_units: self.design_units,
            loss: self.loss.len(),
            cache_log: self.def_cache_log.len(),
        }
    }

    /// Cuts every collection back to `mark`. Work counters stay as they are: the work was done.
    fn rollback(&mut self, mark: Mark) {
        self.def_layers.truncate(mark.def_layers);
        self.def_paths.truncate(mark.def_paths);
        self.records.patterns.truncate(mark.patterns);
        self.records.pattern_paints.truncate(mark.pattern_paints);
        self.records.clips.truncate(mark.clips);
        self.records.masks.truncate(mark.masks);
        self.records.attachments.truncate(mark.attachments);
        self.attach_index.retain(|_, i| *i < mark.attachments);
        self.nodes.truncate(mark.nodes);
        self.styles.truncate(mark.styles);
        self.style_index.retain(|_, i| *i < mark.styles);
        self.gradient_provenance.truncate(mark.provenance);
        self.design_units = mark.design_units;
        let keep = mark.loss.min(self.loss.len());
        for entry in self.loss.drain(keep..) {
            self.frame_notes
                .remove(&(entry.path, entry.kind, entry.code));
        }
        while self.def_cache_log.len() > mark.cache_log {
            if let Some(key) = self.def_cache_log.pop() {
                self.def_cache.remove(&key);
            }
        }
    }

    fn cache_def(&mut self, key: DefKey, id: String) {
        self.def_cache.insert(key, id);
        self.def_cache_log.push(key);
    }

    /// Swaps the importer's layer, path and outset vectors for fresh ones (the first layer is the
    /// definition group) and returns the document's.
    fn enter_frame(&mut self, first: Vec<StudioLayer>) -> Frame {
        self.frames = self.frames.saturating_add(1);
        Frame {
            layers: std::mem::replace(&mut self.layers, first),
            paths: std::mem::take(&mut self.paths),
            outsets: std::mem::take(&mut self.outsets),
        }
    }

    /// Restores the document's vectors and returns the definition's.
    fn leave_frame(&mut self, saved: Frame) -> Frame {
        self.frames = self.frames.saturating_sub(1);
        Frame {
            layers: std::mem::replace(&mut self.layers, saved.layers),
            paths: std::mem::replace(&mut self.paths, saved.paths),
            outsets: std::mem::replace(&mut self.outsets, saved.outsets),
        }
    }

    fn def_group(&self, id: &str, kind: &str, node: Node) -> StudioLayer {
        let label = plain_attr(node, "id").unwrap_or(kind);
        StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: id.to_owned(),
            name: short_name(&format!("{kind}:{label}")),
            parent: Parent::Node(NodeRef {
                node_kind: NodeKind::Artboard,
                node_id: self.artboard_id.clone(),
            }),
            kind: "group".into(),
            payload: Payload::Group,
            visible: false,
            appearance: None,
            text_frame: None,
        }
    }

    /// Element locator (`/svg[1]/defs[1]/clipPath[1]`), the same shape the walk reports.
    fn locate(&mut self, node: Node) -> Result<String> {
        let key = node.id().get();
        if let Some(loc) = self.locators.get(&key) {
            return Ok(loc.clone());
        }
        let mut parts: Vec<String> = Vec::new();
        for el in node.ancestors().filter(Node::is_element) {
            let name = el.tag_name().name();
            let mut ordinal = 1usize;
            let mut sibling = el.prev_sibling();
            while let Some(s) = sibling {
                if s.is_element() && s.tag_name().name() == name {
                    ordinal = ordinal.saturating_add(1);
                }
                sibling = s.prev_sibling();
            }
            self.spend_ref(ordinal)?;
            parts.push(format!("{name}[{ordinal}]"));
        }
        parts.reverse();
        let loc = format!("/{}", parts.join("/"));
        self.locators.insert(key, loc.clone());
        Ok(loc)
    }

    /// Attaches a stored clip and/or mask to the layer `target` names.
    pub(super) fn attach(&mut self, target: &Target, clip: Option<String>, mask: Option<String>) {
        let layer_id = target
            .layer
            .and_then(|i| self.layers.get(i))
            .map(|l| l.layer_id.clone());
        if let Some(layer_id) = layer_id {
            self.attach_id(layer_id, clip, mask);
        }
    }

    fn attach_id(&mut self, layer_id: String, clip: Option<String>, mask: Option<String>) {
        if let Some(&i) = self.attach_index.get(&layer_id)
            && let Some(a) = self.records.attachments.get_mut(i)
        {
            if clip.is_some() {
                a.clip_id = clip;
            }
            if mask.is_some() {
                a.mask_id = mask;
            }
            return;
        }
        self.attach_index
            .insert(layer_id.clone(), self.records.attachments.len());
        self.records.attachments.push(StudioMaskAttachment {
            layer_id,
            clip_id: clip,
            mask_id: mask,
        });
    }

    /// Commits a built frame: its layers and paths join the definition store.
    fn commit_frame(&mut self, frame: Frame) {
        self.def_layers.extend(frame.layers);
        self.def_paths.extend(frame.paths);
    }

    // ------------------------------------------------------------------------------------------
    // Clip
    // ------------------------------------------------------------------------------------------

    /// Does the `clip-path` value name a clipPath in `objectBoundingBox` units?
    fn clip_value_uses_bbox(&self, value: &str) -> bool {
        css::url_function(value)
            .filter(|f| f.rest.is_empty())
            .and_then(|f| match f.target {
                css::UrlTarget::Local(id) => Some(id),
                _ => None,
            })
            .and_then(|id| self.ids.get(id.as_str()).copied())
            .is_some_and(|n| {
                n.tag_name().name() == "clipPath"
                    && plain_attr(n, "clipPathUnits") == Some("objectBoundingBox")
            })
    }

    fn plan_clip(&mut self, clip: Node<'d, 'd>, obb: Option<[f64; 4]>) -> Result<ClipPlanned<'d>> {
        use ClipPlanned::Refused as Refuse;
        let units_bbox = match plain_attr(clip, "clipPathUnits") {
            None | Some("userSpaceOnUse") => false,
            Some("objectBoundingBox") => true,
            Some(_) => return Ok(Refuse("clip_units_unparsed")),
        };
        let own = match plain_attr(clip, "transform") {
            None => Affine::IDENTITY,
            Some(text) => match parse_transform(text) {
                Some(m) => m,
                None => return Ok(Refuse("clip_transform_unparsed")),
            },
        };
        let content_map = if units_bbox {
            // The order of `transform` and the bounding-box map is not pinned down (UNVERIFIED):
            // a clipPath with both is not stored.
            let Some(b) = obb.filter(|b| b[2] > b[0] && b[3] > b[1] && own.is_identity()) else {
                return Ok(Refuse("clip_units_not_stored"));
            };
            Affine::translate(b[0], b[1]).then(Affine::scale(b[2] - b[0], b[3] - b[1]))
        } else {
            own
        };
        let nested = non_none_prop(clip, "clip-path");
        if let Some(value) = &nested {
            if !own.is_identity() {
                return Ok(Refuse("clip_nested_transform"));
            }
            if self.clip_value_uses_bbox(value) {
                return Ok(Refuse("clip_nested_units"));
            }
        }
        let mut members = Vec::new();
        let mut sensitive: Option<Winding> = None;
        let mut first_rule: Option<Winding> = None;
        let mut examined = 0usize;
        for child in clip.children().filter(Node::is_element) {
            self.checkpoint()?;
            examined = examined.saturating_add(1);
            if examined > MAX_CLIP_CHILDREN {
                return Ok(Refuse("clip_too_many_members"));
            }
            self.spend_ref(1)?;
            if child.tag_name().namespace() != Some(SVG_NS) {
                continue;
            }
            let tag = child.tag_name().name();
            match tag {
                "title" | "desc" | "metadata" | "animate" | "animateTransform"
                | "animateMotion" | "set" | "script" | "style" => continue,
                "path" | "rect" | "circle" | "ellipse" | "polygon" | "polyline" | "line"
                | "use" => {}
                _ => return Ok(Refuse("clip_member_unsupported")),
            }
            let (mut st, local) = self.style_at(child)?;
            if local.display_none || !st.visible {
                continue;
            }
            let (shape, shift) = if tag == "use" {
                let target = href_of(child)
                    .and_then(|h| h.strip_prefix('#'))
                    .and_then(|id| self.ids.get(id).copied());
                let (Some(target), Some(x), Some(y)) =
                    (target, user_len(child, "x"), user_len(child, "y"))
                else {
                    return Ok(Refuse("clip_member_unsupported"));
                };
                if !matches!(
                    target.tag_name().name(),
                    "path" | "rect" | "circle" | "ellipse" | "polygon" | "polyline" | "line"
                ) {
                    return Ok(Refuse("clip_member_unsupported"));
                }
                // The instance inherits from the `use`; the shape's own declarations apply on top.
                let noise = self.loss.len();
                let own_local = self.cascade(target, "", &mut st);
                self.loss.truncate(noise);
                if own_local.display_none || !st.visible {
                    continue;
                }
                (target, Some((child, Affine::translate(x, y))))
            } else {
                (child, None)
            };
            let shape_tag = shape.tag_name().name();
            if shape_tag == "line" {
                // A line has no area: it adds nothing to a clip.
                continue;
            }
            // A member whose transform or lengths are not understood would silently shrink the
            // clip (the shape builder reports it and draws nothing): not stored.
            if !member_understood(child) || !member_understood(shape) {
                return Ok(Refuse("clip_member_unparsed"));
            }
            let rule = st.clip_rule;
            first_rule.get_or_insert(rule);
            if matches!(shape_tag, "path" | "polygon" | "polyline") {
                // The rule only changes the region of shapes that can overlap themselves.
                match sensitive {
                    None => sensitive = Some(rule),
                    Some(r) if r != rule => return Ok(Refuse("clip_rule_mixed")),
                    Some(_) => {}
                }
            }
            members.push(match shift {
                Some((el, shift)) => Member::Use {
                    el,
                    target: shape,
                    shift,
                },
                None => Member::Shape(shape),
            });
        }
        if members.is_empty() {
            return Ok(ClipPlanned::Empty);
        }
        Ok(ClipPlanned::Plan(ClipPlan {
            members,
            rule: sensitive.or(first_rule).unwrap_or(Winding::NonZero),
            content_map,
            units_bbox,
            nested,
        }))
    }

    /// Stores the clipPath `clip` as it applies to an element whose user space maps to the
    /// document by `m` and whose object box is `obb`.
    pub(super) fn store_clip(
        &mut self,
        clip: Node<'d, 'd>,
        m: Affine,
        obb: Option<[f64; 4]>,
        depth: usize,
    ) -> Result<Stored> {
        let node = clip.id().get();
        if self.ref_stack.len() >= MAX_REFERENCE_DEPTH {
            return Ok(Stored::Refused("clip_path_depth_exceeded"));
        }
        if self.ref_stack.contains(&node) {
            return Ok(Stored::Refused("clip_path_reference_cycle"));
        }
        self.spend_ref(1)?;
        let plan = match self.plan_clip(clip, obb)? {
            ClipPlanned::Plan(plan) => plan,
            ClipPlanned::Empty => return Ok(Stored::Empty),
            ClipPlanned::Refused(code) => return Ok(Stored::Refused(code)),
        };
        let key = DefKey::Clip(
            node,
            bits6(m),
            if plan.units_bbox { bits4(obb) } else { [0; 4] },
        );
        if let Some(id) = self.def_cache.get(&key) {
            return Ok(Stored::Id(id.clone()));
        }
        let mark = self.mark();
        self.ref_stack.push(node);
        let built = self.build_clip(clip, &plan, m, depth);
        self.ref_stack.pop();
        match built {
            Ok(Some(id)) => {
                self.cache_def(key, id.clone());
                Ok(Stored::Id(id))
            }
            Ok(None) => {
                self.rollback(mark);
                Ok(Stored::Empty)
            }
            Err(e) => match soft(&e) {
                Some(code) => {
                    self.rollback(mark);
                    Ok(Stored::Refused(code))
                }
                None => Err(e),
            },
        }
    }

    /// Builds the clip group and its record; `None` when nothing is left to clip with.
    fn build_clip(
        &mut self,
        clip: Node<'d, 'd>,
        plan: &ClipPlan<'d>,
        m: Affine,
        depth: usize,
    ) -> Result<Option<String>> {
        self.spend_inst()?;
        let loc = self.locate(clip)?;
        let group_id = self.mint_id("SLYR")?;
        let (st, _) = self.style_at(clip)?;
        let group = self.def_group(&group_id, "clip", clip);
        let ctx = Ctx {
            matrix: m.then(plan.content_map),
            st,
            parent: layer_parent(&group_id),
            depth: depth.saturating_add(1),
        };
        let saved = self.enter_frame(vec![group]);
        let before = self.clip_rule.replace(plan.rule);
        let mut result = self.clip_members(plan, &ctx);
        self.clip_rule = before;
        let mut hidden = false;
        if result.is_ok()
            && let Some(value) = &plan.nested
        {
            // `clip-path` on the clipPath intersects it: stored as a clip of the group.
            let target = Target {
                layer: Some(0),
                first_path: 0,
                depth,
            };
            match self.clip_property(value, m, &target, &mut None, &loc) {
                Ok(Applied::Hidden) => hidden = true,
                Ok(_) => {}
                Err(e) => result = Err(e),
            }
        }
        let frame = self.leave_frame(saved);
        result?;
        if hidden || frame.paths.is_empty() {
            return Ok(None);
        }
        self.commit_frame(frame);
        let clip_id = self.next_set_id("clip");
        self.records.clips.push(StudioVectorClip {
            clip_id: clip_id.clone(),
            source_layer_id: group_id,
            clip_rule: winding_rule(plan.rule),
        });
        Ok(Some(clip_id))
    }

    /// Walks the members of a clip as geometry-only shapes (see `Importer::emit_clip_shape`).
    fn clip_members(&mut self, plan: &ClipPlan<'d>, ctx: &Ctx) -> Result<()> {
        for member in &plan.members {
            self.checkpoint()?;
            self.spend_element()?;
            match member {
                Member::Shape(el) => {
                    let loc = self.locate(*el)?;
                    self.shape(*el, ctx, &loc)?;
                }
                Member::Use { el, target, shift } => {
                    let loc = self.locate(*el)?;
                    let mut inner = ctx.clone();
                    let local = self.cascade(*el, &loc, &mut inner.st);
                    let mut matrix = ctx.matrix;
                    if let Some(t) = self.own_transform(*el, &loc)? {
                        matrix = matrix.then(t);
                    }
                    inner.matrix = matrix.then(*shift);
                    let (layers, paths) = (self.layers.len(), self.paths.len());
                    self.shape(*target, &inner, &loc)?;
                    // `clip-path` on the `use` clips the instance it creates.
                    if let Some(value) = &local.clip_path
                        && self.layers.len() > layers
                    {
                        let only = Local {
                            clip_path: Some(value.clone()),
                            ..Local::new()
                        };
                        self.masking(
                            &only,
                            inner.matrix,
                            Target {
                                layer: Some(self.layers.len().saturating_sub(1)),
                                first_path: paths,
                                depth: ctx.depth,
                            },
                            &loc,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------------------------------
    // Mask
    // ------------------------------------------------------------------------------------------

    fn plan_mask(
        &mut self,
        mask: Node<'d, 'd>,
        m: Affine,
        obb: Option<[f64; 4]>,
    ) -> Result<MaskPlanned> {
        use MaskPlanned::Refused as Refuse;
        self.checkpoint()?;
        if ["mask", "clip-path", "filter"]
            .iter()
            .any(|p| non_none_prop(mask, p).is_some())
        {
            return Ok(Refuse("mask_own_reference"));
        }
        if non_none_prop(mask, "color-interpolation")
            .is_some_and(|v| v.eq_ignore_ascii_case("linearRGB"))
        {
            return Ok(Refuse("mask_color_interpolation"));
        }
        let region_bbox = match plain_attr(mask, "maskUnits") {
            None | Some("objectBoundingBox") => true,
            Some("userSpaceOnUse") => false,
            Some(_) => return Ok(Refuse("mask_units_unparsed")),
        };
        let content_bbox = match plain_attr(mask, "maskContentUnits") {
            None | Some("userSpaceOnUse") => false,
            Some("objectBoundingBox") => true,
            Some(_) => return Ok(Refuse("mask_units_unparsed")),
        };
        let Some(alpha) = mask_alpha(mask) else {
            return Ok(Refuse("mask_type_unusable"));
        };
        let Some([x, y, w, h]) = self.mask_region_raw(mask, region_bbox) else {
            return Ok(Refuse("mask_region_unparsed"));
        };
        if w < 0.0 || h < 0.0 {
            return Ok(Refuse("mask_region_invalid"));
        }
        let bbox = obb.filter(|b| b[2] > b[0] && b[3] > b[1]);
        let rect = if region_bbox {
            let Some(b) = bbox else {
                return Ok(Refuse("mask_units_not_stored"));
            };
            let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
            [
                b[0] + x * bw,
                b[1] + y * bh,
                b[0] + (x + w) * bw,
                b[1] + (y + h) * bh,
            ]
        } else {
            [x, y, x + w, y + h]
        };
        let content_map = if content_bbox {
            let Some(b) = bbox else {
                return Ok(Refuse("mask_units_not_stored"));
            };
            Affine::translate(b[0], b[1]).then(Affine::scale(b[2] - b[0], b[3] - b[1]))
        } else {
            Affine::IDENTITY
        };
        let corners = [
            [rect[0], rect[1]],
            [rect[2], rect[1]],
            [rect[2], rect[3]],
            [rect[0], rect[3]],
        ]
        .map(|p| m.apply(p));
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for c in corners {
            lo = [lo[0].min(c[0]), lo[1].min(c[1])];
            hi = [hi[0].max(c[0]), hi[1].max(c[1])];
        }
        if !lo
            .iter()
            .chain(hi.iter())
            .all(|v| v.is_finite() && v.abs() <= COORDINATE_LIMIT)
        {
            return Ok(Refuse("mask_region_out_of_range"));
        }
        Ok(MaskPlanned::Plan(MaskPlan {
            alpha,
            content_map,
            units_bbox: region_bbox || content_bbox,
            region: [lo[0], lo[1], hi[0], hi[1]],
            exact: (m.b == 0.0 && m.c == 0.0) || (m.a == 0.0 && m.d == 0.0),
        }))
    }

    /// Stores the mask `mask` as it applies to an element (see `store_clip`).
    pub(super) fn store_mask(
        &mut self,
        mask: Node<'d, 'd>,
        m: Affine,
        obb: Option<[f64; 4]>,
        depth: usize,
    ) -> Result<Stored> {
        let node = mask.id().get();
        if self.ref_stack.len() >= MAX_REFERENCE_DEPTH {
            return Ok(Stored::Refused("mask_depth_exceeded"));
        }
        if self.ref_stack.contains(&node) {
            return Ok(Stored::Refused("mask_reference_cycle"));
        }
        self.spend_ref(1)?;
        let plan = match self.plan_mask(mask, m, obb)? {
            MaskPlanned::Plan(plan) => plan,
            MaskPlanned::Refused(code) => return Ok(Stored::Refused(code)),
        };
        let key = DefKey::Mask(
            node,
            bits6(m),
            if plan.units_bbox { bits4(obb) } else { [0; 4] },
        );
        if let Some(id) = self.def_cache.get(&key) {
            return Ok(Stored::Id(id.clone()));
        }
        let mark = self.mark();
        self.ref_stack.push(node);
        let built = self.build_mask(mask, &plan, m, depth);
        self.ref_stack.pop();
        match built {
            Ok(Some(id)) => {
                self.cache_def(key, id.clone());
                Ok(Stored::Id(id))
            }
            Ok(None) => {
                self.rollback(mark);
                // Content that was not understood is not an empty mask: the element stays drawn.
                Ok(Stored::Refused("mask_content_not_stored"))
            }
            Err(e) => match soft(&e) {
                Some(code) => {
                    self.rollback(mark);
                    Ok(Stored::Refused(code))
                }
                None => Err(e),
            },
        }
    }

    fn build_mask(
        &mut self,
        mask: Node<'d, 'd>,
        plan: &MaskPlan,
        m: Affine,
        depth: usize,
    ) -> Result<Option<String>> {
        self.spend_inst()?;
        let loc = self.locate(mask)?;
        let group_id = self.mint_id("SLYR")?;
        let (st, _) = self.style_at(mask)?;
        let group = self.def_group(&group_id, "mask", mask);
        let ctx = Ctx {
            matrix: m.then(plan.content_map),
            st,
            parent: layer_parent(&group_id),
            depth: depth.saturating_add(1),
        };
        let saved = self.enter_frame(vec![group]);
        let before = self.clip_rule.take();
        let result = self.walk_children(mask, &ctx, &loc);
        self.clip_rule = before;
        let frame = self.leave_frame(saved);
        result?;
        if frame.layers.len() <= 1 {
            return Ok(None);
        }
        if !plan.exact {
            self.note(&loc, LossKind::Approximated, "mask_region_bounding_box");
        }
        self.commit_frame(frame);
        let [x0, y0, x1, y1] = plan.region;
        let mask_id = self.next_set_id("mask");
        self.records.masks.push(StudioVectorMask {
            mask_id: mask_id.clone(),
            source_layer_id: group_id,
            mode: if plan.alpha {
                StudioMaskMode::Alpha
            } else {
                StudioMaskMode::Luminance
            },
            mask_clip: false,
            invert_mask: false,
            region: Some(Bounds {
                x: pt(x0),
                y: pt(y0),
                width: pt(x1 - x0),
                height: pt(y1 - y0),
            }),
        });
        Ok(Some(mask_id))
    }

    // ------------------------------------------------------------------------------------------
    // Pattern
    // ------------------------------------------------------------------------------------------

    /// The typed loss of a pattern paint that could not be stored; the author's fallback colour
    /// still stands in for it.
    pub(super) fn pattern_refused(
        &mut self,
        loc: &str,
        reason: Option<&'static str>,
        fallback: &Fallback,
    ) {
        self.note(
            loc,
            LossKind::Unsupported,
            "pattern_paint_not_representable",
        );
        if let Some(code) = reason {
            self.note(loc, LossKind::Unsupported, code);
        }
        if matches!(fallback, Fallback::Color(..) | Fallback::Current) {
            self.note(loc, LossKind::Approximated, "pattern_fallback_color_used");
        }
    }

    /// Stores the tiling `spec` as painted onto an element (`at`).
    pub(super) fn store_pattern(
        &mut self,
        spec: &PatternSpec<'d>,
        at: &Use,
    ) -> Result<PatternStored> {
        use PatternStored::Refused as Refuse;
        let node = spec.first.id().get();
        if self.ref_stack.len() >= MAX_REFERENCE_DEPTH {
            return Ok(Refuse("pattern_depth_exceeded"));
        }
        if self.ref_stack.contains(&node) {
            return Ok(Refuse("pattern_reference_cycle"));
        }
        self.spend_ref(1)?;
        let bbox = at.bbox.filter(|b| b[2] > b[0] && b[3] > b[1]);
        let view = self.user_viewport;
        let units_bbox = spec.units_obb;
        let raw = |text: Option<&str>, axis: usize| -> Option<f64> {
            let Some(text) = text else {
                return Some(0.0);
            };
            let l = svgtypes::Length::from_str(text.trim()).ok()?;
            let v = if units_bbox {
                match l.unit {
                    svgtypes::LengthUnit::Percent => l.number / 100.0,
                    svgtypes::LengthUnit::None => l.number,
                    _ => return None,
                }
            } else if l.unit == svgtypes::LengthUnit::Percent {
                l.number / 100.0 * view[axis]
            } else {
                unit_to_user(l)?
            };
            v.is_finite().then_some(v)
        };
        let (Some(rx), Some(ry), Some(rw), Some(rh)) = (
            raw(spec.x, 0),
            raw(spec.y, 1),
            raw(spec.width, 0),
            raw(spec.height, 1),
        ) else {
            return Ok(Refuse("pattern_size_unparsed"));
        };
        let (x, y, w, h) = if units_bbox {
            let Some(b) = bbox else {
                return Ok(Refuse("pattern_degenerate_bbox"));
            };
            let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
            (b[0] + rx * bw, b[1] + ry * bh, rw * bw, rh * bh)
        } else {
            (rx, ry, rw, rh)
        };
        if ![x, y, w, h].iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 {
            return Ok(Refuse("pattern_size_unparsed"));
        }
        // Pattern content space -> tile cell space (user units, origin at the cell's top-left).
        let content_map = if let Some(text) = spec.view_box {
            match svgtypes::ViewBox::from_str(text) {
                Ok(vb) if vb.w > 0.0 && vb.h > 0.0 && vb.w.is_finite() && vb.h.is_finite() => {
                    let par = spec
                        .aspect
                        .and_then(|v| svgtypes::AspectRatio::from_str(v).ok())
                        .unwrap_or_default();
                    fit_view_box(vb, par, w, h)
                }
                _ => return Ok(Refuse("pattern_view_box_invalid")),
            }
        } else if spec.content_obb {
            // UNVERIFIED: content units scale by the box size from the cell origin (the spec
            // text does not say whether the box origin is added).
            let Some(b) = bbox else {
                return Ok(Refuse("pattern_degenerate_bbox"));
            };
            Affine::scale(b[2] - b[0], b[3] - b[1])
        } else {
            Affine::IDENTITY
        };
        let p = self.px_to_pt;
        let content = Affine::scale(p, p).then(content_map);
        let tile = [w * p, h * p];
        let place =
            at.m.then(spec.transform)
                .then(Affine::translate(x, y))
                .then(Affine::scale(1.0 / p, 1.0 / p));
        let in_range = |v: f64| v.is_finite() && v.abs() <= COORDINATE_LIMIT;
        if !place.finite()
            || place.inverse().is_none()
            || !in_range(place.e)
            || !in_range(place.f)
            || !tile.iter().all(|v| in_range(*v) && *v > 0.0)
        {
            return Ok(Refuse("pattern_geometry_out_of_range"));
        }
        let matrix = [place.a, place.b, place.c, place.d, place.e, place.f];
        let key = DefKey::Pattern(node, [w.to_bits(), h.to_bits()], bits6(content_map));
        if let Some(id) = self.def_cache.get(&key).cloned() {
            let sample = self.pattern_samples.get(&id).cloned().flatten();
            return Ok(PatternStored::Placed {
                pattern_id: id,
                matrix,
                sample,
            });
        }
        let mark = self.mark();
        self.ref_stack.push(node);
        let built = self.build_pattern(spec, content, tile, at.depth);
        self.ref_stack.pop();
        match built {
            Ok(Some((id, sample))) => {
                self.cache_def(key, id.clone());
                for template in &spec.chain {
                    self.cache_def(DefKey::Template(*template), String::new());
                }
                self.pattern_samples.insert(id.clone(), sample.clone());
                Ok(PatternStored::Placed {
                    pattern_id: id,
                    matrix,
                    sample,
                })
            }
            Ok(None) => {
                self.rollback(mark);
                Ok(Refuse("pattern_content_not_stored"))
            }
            Err(e) => match soft(&e) {
                Some(code) => {
                    self.rollback(mark);
                    Ok(Refuse(code))
                }
                None => Err(e),
            },
        }
    }

    /// Builds the tile artwork group and the `StudioPattern`; returns its id and the first solid
    /// colour of the artwork. `None` when the tile content produced no layer.
    fn build_pattern(
        &mut self,
        spec: &PatternSpec<'d>,
        content: Affine,
        tile: [f64; 2],
        depth: usize,
    ) -> Result<Option<(String, Option<StudioColor>)>> {
        self.spend_inst()?;
        let loc = self.locate(spec.first)?;
        let group_id = self.mint_id("SLYR")?;
        let (st, _) = self.style_at(spec.owner)?;
        let group = self.def_group(&group_id, "pattern", spec.first);
        let ctx = Ctx {
            matrix: content,
            st,
            parent: layer_parent(&group_id),
            depth: depth.saturating_add(1),
        };
        let first_node = self.nodes.len();
        let saved = self.enter_frame(vec![group]);
        let before = self.clip_rule.take();
        let mut result = self.walk_children(spec.owner, &ctx, &loc);
        self.clip_rule = before;
        // `overflow` other than visible clips the content to the tile cell (SVG 1.1 13.3).
        let mut overflows = false;
        if result.is_ok() && spec.clipped {
            match self.content_since(0) {
                Ok(c) => {
                    overflows = !rect_contains(&c, Affine::IDENTITY, [0.0, 0.0, tile[0], tile[1]]);
                }
                Err(e) => result = Err(e),
            }
        }
        let frame = self.leave_frame(saved);
        result?;
        if frame.layers.len() <= 1 {
            return Ok(None);
        }
        let sample = self.first_solid(&frame.layers, first_node);
        self.commit_frame(frame);
        if overflows {
            let clip_id = self.rect_clip([0.0, 0.0, tile[0], tile[1]])?;
            self.attach_id(group_id.clone(), Some(clip_id), None);
        }
        let pattern_id = self.next_set_id("pattern");
        self.records.patterns.push(StudioPattern {
            pattern_id: pattern_id.clone(),
            name: short_name(plain_attr(spec.first, "id").unwrap_or("pattern")),
            tile_type: StudioTileType::Grid,
            horizontal_spacing: pt(0.0),
            vertical_spacing: pt(0.0),
            source: StudioPatternSource::Definition {
                content_layer_id: group_id,
                tile_width: pt(tile[0]),
                tile_height: pt(tile[1]),
                brick_offset: None,
                overlap_order: None,
                // A pattern is fixed in the user space of the element, so it moves with it.
                move_tile_with_art: true,
            },
        });
        Ok(Some((pattern_id, sample)))
    }

    /// First solid paint, in layer order, among the layers built since `first_node` designs.
    fn first_solid(&self, layers: &[StudioLayer], first_node: usize) -> Option<StudioColor> {
        let styles: BTreeMap<&str, &StudioPaintStyle> = self
            .styles
            .iter()
            .map(|s| (s.style_id.as_str(), s))
            .collect();
        let designs: BTreeMap<&str, &str> = self
            .nodes
            .get(first_node..)
            .unwrap_or_default()
            .iter()
            .filter_map(|n| {
                n.style_refs
                    .get(LAYER_STYLE_REF)
                    .map(|s| (n.node.node_id.as_str(), s.as_str()))
            })
            .collect();
        for layer in layers {
            let style = designs
                .get(layer.layer_id.as_str())
                .and_then(|id| styles.get(id));
            for row in style.into_iter().flat_map(|s| s.rows.iter()) {
                match row {
                    StudioAppearanceRow::Fill {
                        paint: StudioPaint::Solid { colour },
                        ..
                    }
                    | StudioAppearanceRow::Stroke {
                        paint: StudioPaint::Solid { colour },
                        ..
                    } => return Some(colour.clone()),
                    _ => {}
                }
            }
        }
        None
    }

    /// A hidden group with one rectangle layer, stored as a clip: the tile cell that clips
    /// overflowing tile content.
    fn rect_clip(&mut self, r: [f64; 4]) -> Result<String> {
        self.spend_inst()?;
        let group_id = self.mint_id("SLYR")?;
        let layer_id = self.mint_id("SLYR")?;
        let path_id = self.mint_id("SVPT")?;
        let corner = |x: f64, y: f64| -> Result<Anchor> {
            Ok(Anchor {
                position: point([x, y])?,
                incoming: point([0.0, 0.0])?,
                outgoing: point([0.0, 0.0])?,
                handle_mirroring: Mirroring::None,
            })
        };
        let anchors = vec![
            corner(r[0], r[1])?,
            corner(r[2], r[1])?,
            corner(r[2], r[3])?,
            corner(r[0], r[3])?,
        ];
        self.def_paths.push(VectorPath {
            path_id: path_id.clone(),
            layer_id: layer_id.clone(),
            closed: true,
            anchors,
            winding: Winding::NonZero,
        });
        self.def_layers.push(StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: group_id.clone(),
            name: "tile-clip".into(),
            parent: Parent::Node(NodeRef {
                node_kind: NodeKind::Artboard,
                node_id: self.artboard_id.clone(),
            }),
            kind: "group".into(),
            payload: Payload::Group,
            visible: false,
            appearance: None,
            text_frame: None,
        });
        self.def_layers.push(StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id,
            name: "tile-cell".into(),
            parent: layer_parent(&group_id),
            kind: "vector".into(),
            payload: Payload::Vector {
                path_ids: vec![path_id],
            },
            visible: true,
            appearance: None,
            text_frame: None,
        });
        let clip_id = self.next_set_id("clip");
        self.records.clips.push(StudioVectorClip {
            clip_id: clip_id.clone(),
            source_layer_id: group_id,
            clip_rule: StudioClipRule::NonZero,
        });
        Ok(clip_id)
    }
}
