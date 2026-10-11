//! References that restrict or replace what an element paints: `clip-path`, `mask` and pattern
//! paint servers. Semantics follow SVG 1.1 (13.3 patterns, 14.3 clipping, 14.4 masking), SVG 2
//! (16.1.2 circular references, 16.1.6 `href` over `xlink:href`) and CSS Masking Level 1 (5.1 an
//! invalid `clip-path` reference clips nothing, 6.1 an empty `clipPath` clips everything, 7.1 an
//! invalid `mask` reference is a transparent black layer, 9.2 `mask-type`).
//!
//! Folio stores no pattern paint kind, no vector clip and no vector mask (its `StudioMask` is
//! raster coverage tiles; STU-VEC-017, STU-VEC-053 and STU-VEC-138 name the missing records), so
//! a definition is never carried over. Each use resolves to exactly one of
//! - an exact result that needs no record: the clip contains everything the element paints, or the
//!   mask is the identity over it (`*_noop_dropped`, Ignored), an empty clip or mask or a
//!   zero-region mask hides the element (the layer is hidden, Approximated), a pattern with a
//!   zero tile, a singular transform or no content paints nothing (Ignored);
//! - a typed loss with a specific code (Unsupported), the element staying as drawn without the
//!   reference.
//!
//! All walks are bounded: reference nesting stops at [`MAX_REFERENCE_DEPTH`], every reference
//! chain keeps a visited set, at most [`MAX_CLIP_CHILDREN`] children of one definition are
//! examined, and the total work of one import is charged against a budget derived from
//! `Limits` (`BudgetExceeded("reference_work")`).
use super::css::{self, UrlTarget};
use super::*;
use roxmltree::NodeId;

/// Deepest reference nesting followed (pattern `href` chains, `clip-path` inside `clipPath`).
const MAX_REFERENCE_DEPTH: usize = 8;
/// Children of one `clipPath` or `mask` examined; more makes the result "not provable".
const MAX_CLIP_CHILDREN: usize = 64;
/// Reference work units allowed per unit of `max_segments + max_nodes`.
pub(super) const REFERENCE_WORK_FACTOR: usize = 32;

/// Geometry an element paints, in document points, and its largest stroke outset.
pub(super) struct Content {
    subs: Vec<RawSubpath>,
    outset: f64,
}

/// Where the result of a reference lands: the layer to hide (none for the root `svg`) and the
/// first entry of `Importer::paths` the element contributed.
pub(super) struct Target {
    pub layer: Option<usize>,
    pub first_path: usize,
}

/// The element a reference applies to: its user space as a map into document points, its object
/// bounding box in that space, and everything it paints.
#[derive(Clone, Copy)]
struct Subject<'c> {
    to_doc: Affine,
    obb: Option<[f64; 4]>,
    content: &'c Content,
}

/// What a clip or mask does to the subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Restricts nothing the element paints.
    Free,
    /// Removes everything.
    Empty,
    /// Restricts the element in a way that cannot be proven away (or is not understood).
    Open,
    /// A `clip-path` reference loop (SVG 2, 16.1.2).
    Cycle,
    /// Nesting beyond [`MAX_REFERENCE_DEPTH`].
    TooDeep,
}

#[derive(Default)]
struct Seen {
    /// A shape of a `clipPath` that could not be analysed uses `clip-rule: evenodd`.
    evenodd: bool,
}

/// One child of a `clipPath` or `mask`.
enum Child<'d> {
    /// Contributes nothing (descriptive, animation, hidden, empty).
    Skip,
    /// Renders but is not analysed.
    Unknown,
    /// A plain axis-aligned rectangle (also through `use`).
    Rect(ClipRect<'d>),
}

struct ClipRect<'d> {
    /// The element whose paint properties apply.
    node: Node<'d, 'd>,
    /// `[x0, y0, x1, y1]` in the rectangle's own space.
    rect: [f64; 4],
    /// Own space into the space of the `clipPath` content.
    m: Affine,
    /// `clip-path` value on the rectangle (or its `use`).
    nested: Option<String>,
    via_use: bool,
}

/// Outcome of a pattern paint reference.
pub(super) enum PatternPaint {
    /// Paints nothing (exact).
    NoPaint,
    /// A real tiling that Folio cannot store.
    NotRepresentable,
    /// The reference is invalid (SVG 2 painting 13.2: the fallback applies).
    Invalid,
}

/// The last non-`none` value of a property given as attribute or in `style`.
fn non_none_prop(el: Node, name: &str) -> Option<String> {
    collect_props(el)
        .into_iter()
        .rev()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
        .filter(|v| !v.eq_ignore_ascii_case("none"))
}

/// `mask-type` of a `mask` element: `Some(true)` alpha, `Some(false)` luminance (the initial
/// value), `None` for an unusable value (CSS Masking 9.2).
fn mask_alpha(mask: Node) -> Option<bool> {
    let value = collect_props(mask)
        .into_iter()
        .rev()
        .find(|(n, _)| n == "mask-type")
        .map(|(_, v)| v);
    match value.as_deref() {
        None => Some(false),
        Some(v) if v.eq_ignore_ascii_case("luminance") => Some(false),
        Some(v) if v.eq_ignore_ascii_case("alpha") => Some(true),
        Some(_) => None,
    }
}

/// A length of a rectangle in user units; an absent attribute is 0, a font-relative or
/// percentage length is not analysed.
fn user_len(el: Node, name: &str) -> Option<f64> {
    match plain_attr(el, name) {
        None => Some(0.0),
        Some(text) => svgtypes::Length::from_str(text.trim())
            .ok()
            .and_then(unit_to_user)
            .filter(|v| v.is_finite()),
    }
}

/// Does the rectangle (in a space that maps into document points by `local_to_doc`) contain
/// everything the subject paints, strokes included? The content is mapped into the rectangle's
/// space, where its tight bounding box is compared exactly; the stroke outset is added in that
/// space. A singular map proves nothing.
fn rect_contains(content: &Content, local_to_doc: Affine, rect: [f64; 4]) -> bool {
    let Some(inverse) = local_to_doc.inverse() else {
        return false;
    };
    let low = local_to_doc.min_scale();
    if !low.is_finite() || low <= 1e-12 {
        return false;
    }
    let Some(b) = bbox_of(&content.subs, inverse) else {
        return true;
    };
    let pad = content.outset / low;
    // An infinite or NaN number makes every comparison below meaningless (an infinite tolerance
    // accepts anything): only finite boxes can prove containment.
    if !pad.is_finite() || !rect.iter().chain(b.iter()).all(|v| v.is_finite()) {
        return false;
    }
    let scale = rect.iter().chain(b.iter()).fold(1.0_f64, |m, v| m.max(v.abs()));
    let tol = 1e-9 * scale;
    b[0] - pad >= rect[0] - tol
        && b[1] - pad >= rect[1] - tol
        && b[2] + pad <= rect[2] + tol
        && b[3] + pad <= rect[3] + tol
}

impl<'d> Importer<'_, 'd> {
    /// Charges `n` reference work units against the import budget.
    pub(super) fn spend_ref(&mut self, n: usize) -> Result<()> {
        self.ref_spent = self.ref_spent.saturating_add(n);
        if self.ref_spent > self.ref_budget {
            return Err(VectorError::BudgetExceeded("reference_work"));
        }
        Ok(())
    }

    /// The loss entry of a `pattern`, `clipPath` or `mask` definition. Definitions paint nothing
    /// by themselves and Folio has no record for them; what a use of one means is reported where
    /// it is used.
    pub(super) fn definition_note(&mut self, tag: &str, loc: &str) {
        let code = match tag {
            "pattern" => "pattern_definition_not_stored",
            "clipPath" => "clip_path_definition_not_stored",
            _ => "mask_definition_not_stored",
        };
        self.note(loc, LossKind::Ignored, code);
    }

    /// Properties of `node` as inherited from the document root down (no loss entries).
    fn style_at(&mut self, node: Node<'d, 'd>) -> Result<(Inherited, Local)> {
        let mut chain: Vec<Node<'d, 'd>> = node.ancestors().filter(Node::is_element).collect();
        chain.reverse();
        self.spend_ref(chain.len())?;
        let noise = self.loss.len();
        let mut st = Inherited::root();
        let mut local = Local::new();
        for el in chain {
            local = self.cascade(el, "", &mut st);
        }
        self.loss.truncate(noise);
        Ok((st, local))
    }

    /// Everything painted since `first`, rebuilt from the emitted anchors.
    fn content_since(&mut self, first: usize) -> Result<Content> {
        let points = self.paths.get(first..).map_or(0, |paths| {
            paths
                .iter()
                .fold(0usize, |n, p| n.saturating_add(p.anchors.len()))
        });
        self.spend_ref(points.saturating_add(1))?;
        let mut subs = Vec::new();
        for p in self.paths.get(first..).unwrap_or_default() {
            let anchors = p
                .anchors
                .iter()
                .map(|a| {
                    let pos = [a.position.x.value(), a.position.y.value()];
                    RawAnchor {
                        pos,
                        cin: [pos[0] + a.incoming.x.value(), pos[1] + a.incoming.y.value()],
                        cout: [pos[0] + a.outgoing.x.value(), pos[1] + a.outgoing.y.value()],
                    }
                })
                .collect();
            subs.push(RawSubpath {
                anchors,
                closed: p.closed,
            });
        }
        let outset = self
            .outsets
            .get(first..)
            .unwrap_or_default()
            .iter()
            .fold(0.0_f64, |m, v| m.max(*v));
        Ok(Content { subs, outset })
    }

    fn cached<'c>(
        &mut self,
        cache: &'c mut Option<Content>,
        first: usize,
    ) -> Result<&'c Content> {
        if cache.is_none() {
            *cache = Some(self.content_since(first)?);
        }
        cache.as_ref().ok_or(VectorError::InvalidGeometry)
    }

    /// Hides the target layer (an exact result: the element paints nothing); the root `svg` has
    /// no layer and reports the plain loss instead.
    fn hide(&mut self, target: &Target, loc: &str, code: &'static str, root_code: &'static str) {
        match target.layer.and_then(|i| self.layers.get_mut(i)) {
            Some(layer) => {
                layer.visible = false;
                self.note(loc, LossKind::Approximated, code);
            }
            None => self.note(loc, LossKind::Unsupported, root_code),
        }
    }

    /// Resolves the `clip-path` and `mask` an element carries, after its geometry was emitted.
    /// `m` maps the element's user space (its own `transform` included) into document points.
    pub(super) fn masking(
        &mut self,
        local: &Local,
        m: Affine,
        target: Target,
        loc: &str,
    ) -> Result<()> {
        if local.display_none {
            return Ok(());
        }
        let mut cache = None;
        if let Some(value) = &local.clip_path {
            self.clip_property(value, m, &target, &mut cache, loc)?;
        }
        if let Some(value) = &local.mask {
            self.mask_property(value, m, &target, &mut cache, loc)?;
        }
        Ok(())
    }

    fn clip_property(
        &mut self,
        value: &str,
        m: Affine,
        target: &Target,
        cache: &mut Option<Content>,
        loc: &str,
    ) -> Result<()> {
        let id = match css::url_function(value) {
            None => {
                // A basic shape or other <clip-source>: not stored.
                self.note(loc, LossKind::Unsupported, "unsupported_clip_path");
                return Ok(());
            }
            Some(f) if !f.rest.is_empty() => {
                self.note(loc, LossKind::Unsupported, "clip_path_value_unparsed");
                return Ok(());
            }
            Some(f) => match f.target {
                UrlTarget::Local(id) => id,
                UrlTarget::External => {
                    self.note(loc, LossKind::ExternalDenied, "external_clip_path_denied");
                    return Ok(());
                }
                UrlTarget::Malformed => {
                    self.note(loc, LossKind::Unsupported, "clip_path_value_unparsed");
                    return Ok(());
                }
            },
        };
        let Some(clip) = self
            .ids
            .get(id.as_str())
            .copied()
            .filter(|n| n.tag_name().name() == "clipPath")
        else {
            // CSS Masking 5.1: an invalid reference clips nothing.
            self.note(loc, LossKind::Ignored, "clip_path_reference_invalid");
            return Ok(());
        };
        let content = self.cached(cache, target.first_path)?;
        if content.subs.is_empty() {
            self.note(loc, LossKind::Ignored, "clip_path_noop_dropped");
            return Ok(());
        }
        let obb = m.inverse().and_then(|inv| bbox_of(&content.subs, inv));
        let subject = Subject {
            to_doc: m,
            obb,
            content,
        };
        let mut seen = Seen::default();
        let verdict = self.clip_contains(clip, &subject, 0, &mut Vec::new(), &mut seen)?;
        match verdict {
            Verdict::Free => self.note(loc, LossKind::Ignored, "clip_path_noop_dropped"),
            Verdict::Empty => self.hide(
                target,
                loc,
                "clip_path_empty_hides_element",
                "clip_path_not_representable",
            ),
            Verdict::Cycle => self.note(loc, LossKind::Unsupported, "clip_path_reference_cycle"),
            Verdict::TooDeep => self.note(loc, LossKind::Unsupported, "clip_path_depth_exceeded"),
            Verdict::Open => {
                self.note(loc, LossKind::Unsupported, "clip_path_not_representable");
                if seen.evenodd {
                    self.note(loc, LossKind::Unsupported, "clip_rule_evenodd");
                }
            }
        }
        Ok(())
    }

    fn mask_property(
        &mut self,
        value: &str,
        m: Affine,
        target: &Target,
        cache: &mut Option<Content>,
        loc: &str,
    ) -> Result<()> {
        let id = match css::url_function(value) {
            None => {
                self.note(loc, LossKind::Unsupported, "unsupported_mask");
                return Ok(());
            }
            Some(f) if !f.rest.is_empty() => {
                self.note(loc, LossKind::Unsupported, "mask_value_unparsed");
                return Ok(());
            }
            Some(f) => match f.target {
                UrlTarget::Local(id) => id,
                UrlTarget::External => {
                    self.note(loc, LossKind::ExternalDenied, "external_mask_denied");
                    return Ok(());
                }
                UrlTarget::Malformed => {
                    self.note(loc, LossKind::Unsupported, "mask_value_unparsed");
                    return Ok(());
                }
            },
        };
        let Some(mask) = self
            .ids
            .get(id.as_str())
            .copied()
            .filter(|n| n.tag_name().name() == "mask")
        else {
            // CSS Masking 7.1: an invalid reference is a transparent black layer.
            self.hide(
                target,
                loc,
                "mask_reference_invalid_hidden",
                "mask_not_representable",
            );
            return Ok(());
        };
        let content = self.cached(cache, target.first_path)?;
        if content.subs.is_empty() {
            self.note(loc, LossKind::Ignored, "mask_noop_dropped");
            return Ok(());
        }
        let obb = m.inverse().and_then(|inv| bbox_of(&content.subs, inv));
        let subject = Subject {
            to_doc: m,
            obb,
            content,
        };
        let mut seen = Seen::default();
        match self.mask_verdict(mask, &subject, &mut seen)? {
            Verdict::Free => self.note(loc, LossKind::Ignored, "mask_noop_dropped"),
            Verdict::Empty => self.hide(
                target,
                loc,
                "mask_empty_hides_element",
                "mask_not_representable",
            ),
            _ => {
                self.note(loc, LossKind::Unsupported, "mask_not_representable");
                match mask_alpha(mask) {
                    Some(true) => self.note(loc, LossKind::Unsupported, "mask_type_alpha"),
                    Some(false) => self.note(loc, LossKind::Unsupported, "mask_type_luminance"),
                    None => {}
                }
            }
        }
        Ok(())
    }

    /// Does `clip` restrict what the subject paints? Evaluated lazily and bounded: `stack` holds
    /// the `clipPath` elements being evaluated (loops), `depth` the nesting.
    fn clip_contains(
        &mut self,
        clip: Node<'d, 'd>,
        subject: &Subject<'_>,
        depth: usize,
        stack: &mut Vec<NodeId>,
        seen: &mut Seen,
    ) -> Result<Verdict> {
        if depth >= MAX_REFERENCE_DEPTH {
            return Ok(Verdict::TooDeep);
        }
        if stack.contains(&clip.id()) {
            return Ok(Verdict::Cycle);
        }
        self.checkpoint()?;
        self.spend_ref(1)?;
        stack.push(clip.id());
        let verdict = self.clip_inner(clip, subject, depth, stack, seen);
        stack.pop();
        verdict
    }

    fn clip_inner(
        &mut self,
        clip: Node<'d, 'd>,
        subject: &Subject<'_>,
        depth: usize,
        stack: &mut Vec<NodeId>,
        seen: &mut Seen,
    ) -> Result<Verdict> {
        let units_obb = match plain_attr(clip, "clipPathUnits") {
            None | Some("userSpaceOnUse") => false,
            Some("objectBoundingBox") => true,
            Some(_) => return Ok(Verdict::Open),
        };
        let own = match plain_attr(clip, "transform") {
            None => Affine::IDENTITY,
            Some(text) => match parse_transform(text) {
                Some(m) => m,
                None => return Ok(Verdict::Open),
            },
        };
        let content_map = if units_obb {
            // The order of `transform` and the bounding-box map is not pinned down here.
            let Some(b) = subject
                .obb
                .filter(|b| b[2] > b[0] && b[3] > b[1] && own.is_identity())
            else {
                return Ok(Verdict::Open);
            };
            Affine::translate(b[0], b[1]).then(Affine::scale(b[2] - b[0], b[3] - b[1]))
        } else {
            own
        };
        let space = subject.to_doc.then(content_map);
        // `clip-path` on the clipPath itself intersects: it must not restrict either.
        let mut level = Verdict::Free;
        if let Some(value) = non_none_prop(clip, "clip-path") {
            if !own.is_identity() {
                return Ok(Verdict::Open);
            }
            let nested = Subject {
                obb: None,
                ..*subject
            };
            level = self.nested_clip(&value, &nested, depth, stack, seen)?;
        }
        let (mut free, mut any, mut blocked) = (false, false, None);
        let mut examined = 0usize;
        for child in clip.children().filter(Node::is_element) {
            self.checkpoint()?;
            examined = examined.saturating_add(1);
            if examined > MAX_CLIP_CHILDREN {
                any = true;
                break;
            }
            self.spend_ref(1)?;
            match self.clip_child(child, seen)? {
                Child::Skip => {}
                Child::Unknown => any = true,
                Child::Rect(r) => {
                    any = true;
                    let at = space.then(r.m);
                    if !rect_contains(subject.content, at, r.rect) {
                        continue;
                    }
                    match &r.nested {
                        None => free = true,
                        Some(value) => {
                            let nested = Subject {
                                to_doc: at,
                                obb: Some(r.rect),
                                content: subject.content,
                            };
                            match self.nested_clip(value, &nested, depth, stack, seen)? {
                                Verdict::Free => free = true,
                                Verdict::Cycle => blocked = Some(Verdict::Cycle),
                                Verdict::TooDeep => blocked = Some(Verdict::TooDeep),
                                Verdict::Empty | Verdict::Open => {}
                            }
                        }
                    }
                }
            }
        }
        let children = if free {
            Verdict::Free
        } else if !any {
            Verdict::Empty
        } else {
            blocked.unwrap_or(Verdict::Open)
        };
        Ok(match (level, children) {
            (Verdict::Empty, _) | (_, Verdict::Empty) => Verdict::Empty,
            (Verdict::Free, v) | (v, _) => v,
        })
    }

    /// A `clip-path` property inside a clip or mask definition.
    fn nested_clip(
        &mut self,
        value: &str,
        subject: &Subject<'_>,
        depth: usize,
        stack: &mut Vec<NodeId>,
        seen: &mut Seen,
    ) -> Result<Verdict> {
        let Some(f) = css::url_function(value).filter(|f| f.rest.is_empty()) else {
            return Ok(Verdict::Open);
        };
        let UrlTarget::Local(id) = f.target else {
            return Ok(Verdict::Open);
        };
        let Some(node) = self
            .ids
            .get(id.as_str())
            .copied()
            .filter(|n| n.tag_name().name() == "clipPath")
        else {
            // CSS Masking 5.1: an invalid reference clips nothing.
            return Ok(Verdict::Free);
        };
        self.clip_contains(node, subject, depth.saturating_add(1), stack, seen)
    }

    /// Classifies one child of a `clipPath` or `mask`.
    fn clip_child(&mut self, child: Node<'d, 'd>, seen: &mut Seen) -> Result<Child<'d>> {
        if child.tag_name().namespace() != Some(SVG_NS) {
            return Ok(Child::Skip);
        }
        let tag = child.tag_name().name();
        match tag {
            "title" | "desc" | "metadata" | "animate" | "animateTransform" | "animateMotion"
            | "set" | "script" | "style" => return Ok(Child::Skip),
            "rect" | "use" | "path" | "circle" | "ellipse" | "polygon" | "polyline" | "line"
            | "text" => {}
            _ => return Ok(Child::Unknown),
        }
        let (st, local) = self.style_at(child)?;
        if local.display_none || !st.visible {
            return Ok(Child::Skip);
        }
        let own = match plain_attr(child, "transform") {
            None => Affine::IDENTITY,
            Some(text) => match parse_transform(text) {
                Some(m) => m,
                None => return Ok(Child::Unknown),
            },
        };
        let (shape, shift, via_use) = match tag {
            "rect" => (child, Affine::IDENTITY, false),
            "use" => {
                let target = href_of(child)
                    .and_then(|h| h.strip_prefix('#'))
                    .and_then(|id| self.ids.get(id).copied());
                let (Some(target), Some(x), Some(y)) =
                    (target, user_len(child, "x"), user_len(child, "y"))
                else {
                    return Ok(Child::Unknown);
                };
                if target.tag_name().name() != "rect"
                    || non_none_prop(target, "clip-path").is_some()
                    || collect_props(target).iter().any(|(n, _)| n == "visibility")
                {
                    return Ok(Child::Unknown);
                }
                (target, Affine::translate(x, y), true)
            }
            _ => {
                seen.evenodd |= st.clip_rule == Winding::EvenOdd;
                return Ok(Child::Unknown);
            }
        };
        if via_use && non_none_prop(shape, "display").is_some_and(|d| d == "none") {
            return Ok(Child::Skip);
        }
        let (Some(x), Some(y), Some(w), Some(h), Some(rx), Some(ry)) = (
            user_len(shape, "x"),
            user_len(shape, "y"),
            user_len(shape, "width"),
            user_len(shape, "height"),
            user_len(shape, "rx"),
            user_len(shape, "ry"),
        ) else {
            return Ok(Child::Unknown);
        };
        let target_own = if via_use {
            match plain_attr(shape, "transform") {
                None => Affine::IDENTITY,
                Some(text) => match parse_transform(text) {
                    Some(m) => m,
                    None => return Ok(Child::Unknown),
                },
            }
        } else {
            Affine::IDENTITY
        };
        if w <= 0.0 || h <= 0.0 {
            return Ok(Child::Skip);
        }
        if rx > 0.0 || ry > 0.0 {
            return Ok(Child::Unknown);
        }
        Ok(Child::Rect(ClipRect {
            node: if via_use { child } else { shape },
            rect: [x, y, x + w, y + h],
            m: own.then(shift).then(target_own),
            nested: non_none_prop(child, "clip-path"),
            via_use,
        }))
    }

    /// Does `mask` leave everything the subject paints untouched? Only the plain case is
    /// proven: the region covers the subject and the topmost child is an opaque rectangle (white
    /// for a luminance mask) that covers it.
    fn mask_verdict(
        &mut self,
        mask: Node<'d, 'd>,
        subject: &Subject<'_>,
        seen: &mut Seen,
    ) -> Result<Verdict> {
        self.checkpoint()?;
        self.spend_ref(1)?;
        if ["mask", "clip-path", "filter"]
            .iter()
            .any(|p| non_none_prop(mask, p).is_some())
        {
            return Ok(Verdict::Open);
        }
        let region_obb = match plain_attr(mask, "maskUnits") {
            None | Some("objectBoundingBox") => true,
            Some("userSpaceOnUse") => false,
            Some(_) => return Ok(Verdict::Open),
        };
        let content_obb = match plain_attr(mask, "maskContentUnits") {
            None | Some("userSpaceOnUse") => false,
            Some("objectBoundingBox") => true,
            Some(_) => return Ok(Verdict::Open),
        };
        let Some(alpha) = mask_alpha(mask) else {
            return Ok(Verdict::Open);
        };
        let view = self.user_viewport;
        let region = |name: &str, default_percent: f64, axis: usize| -> Option<f64> {
            let basis = if region_obb { 1.0 } else { view[axis] };
            let Some(text) = plain_attr(mask, name) else {
                return Some(default_percent / 100.0 * basis);
            };
            let l = svgtypes::Length::from_str(text.trim()).ok()?;
            let v = match (l.unit, region_obb) {
                (svgtypes::LengthUnit::Percent, _) => l.number / 100.0 * basis,
                (svgtypes::LengthUnit::None, true) => l.number,
                (_, true) => return None,
                (_, false) => unit_to_user(l)?,
            };
            v.is_finite().then_some(v)
        };
        let (Some(x), Some(y), Some(w), Some(h)) = (
            region("x", -10.0, 0),
            region("y", -10.0, 1),
            region("width", 120.0, 0),
            region("height", 120.0, 1),
        ) else {
            return Ok(Verdict::Open);
        };
        if w < 0.0 || h < 0.0 {
            return Ok(Verdict::Open);
        }
        if w == 0.0 || h == 0.0 {
            return Ok(Verdict::Empty);
        }
        let bbox = subject.obb.filter(|b| b[2] > b[0] && b[3] > b[1]);
        let rect = if region_obb {
            let Some(b) = bbox else {
                return Ok(Verdict::Open);
            };
            let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
            [b[0] + x * bw, b[1] + y * bh, b[0] + (x + w) * bw, b[1] + (y + h) * bh]
        } else {
            [x, y, x + w, y + h]
        };
        if !rect_contains(subject.content, subject.to_doc, rect) {
            return Ok(Verdict::Open);
        }
        let content_map = if content_obb {
            let Some(b) = bbox else {
                return Ok(Verdict::Open);
            };
            Affine::translate(b[0], b[1]).then(Affine::scale(b[2] - b[0], b[3] - b[1]))
        } else {
            Affine::IDENTITY
        };
        let mut top = None;
        let mut examined = 0usize;
        for child in mask.children().filter(Node::is_element) {
            self.checkpoint()?;
            examined = examined.saturating_add(1);
            if examined > MAX_CLIP_CHILDREN {
                return Ok(Verdict::Open);
            }
            self.spend_ref(1)?;
            match self.clip_child(child, seen)? {
                Child::Skip => {}
                other => top = Some(other),
            }
        }
        // No visible content: the mask value is 0 everywhere.
        let Some(top) = top else {
            return Ok(Verdict::Empty);
        };
        let Child::Rect(r) = top else {
            return Ok(Verdict::Open);
        };
        if r.via_use {
            return Ok(Verdict::Open);
        }
        let at = subject.to_doc.then(content_map).then(r.m);
        if !rect_contains(subject.content, at, r.rect) {
            return Ok(Verdict::Open);
        }
        if ["filter", "mask"]
            .iter()
            .any(|p| non_none_prop(r.node, p).is_some())
        {
            return Ok(Verdict::Open);
        }
        let (st, local) = self.style_at(r.node)?;
        let (red, green, blue, a) = match st.fill {
            PaintValue::Color(red, green, blue, a) => (red, green, blue, a),
            PaintValue::Current => st.color,
            _ => return Ok(Verdict::Open),
        };
        let opaque = a == 1.0 && st.fill_opacity == 1.0 && local.opacity == 1.0;
        let unstroked = matches!(st.stroke, PaintValue::None) || st.stroke_width == 0.0;
        let value_one = alpha || (red, green, blue) == (255, 255, 255);
        if !(opaque && unstroked && value_one) {
            return Ok(Verdict::Open);
        }
        if let Some(value) = &r.nested {
            let nested = Subject {
                to_doc: at,
                obb: Some(r.rect),
                content: subject.content,
            };
            if self.nested_clip(value, &nested, 0, &mut Vec::new(), seen)? != Verdict::Free {
                return Ok(Verdict::Open);
            }
        }
        Ok(Verdict::Free)
    }

    /// Resolves a `fill`/`stroke` `url(#id)` that names a `pattern` (SVG 1.1, 13.3): follows the
    /// `href` template chain (SVG 2: `href` over `xlink:href`), inherits attributes and content as
    /// the spec defines, and decides what Folio can hold: nothing but the exact "paints nothing"
    /// results.
    pub(super) fn pattern_paint(&mut self, id: &str, loc: &str) -> Result<PatternPaint> {
        let Some(first) = self
            .ids
            .get(id)
            .copied()
            .filter(|n| n.tag_name().name() == "pattern")
        else {
            return Ok(PatternPaint::Invalid);
        };
        let mut chain = vec![first];
        while let Some(&current) = chain.last() {
            self.checkpoint()?;
            self.spend_ref(1)?;
            let Some(href) = href_of(current) else { break };
            // External targets are denied where the element is walked; the chain ends here.
            let Some(next) = href
                .strip_prefix('#')
                .and_then(|t| self.ids.get(t).copied())
            else {
                break;
            };
            if next.tag_name().name() != "pattern" {
                self.note(loc, LossKind::Ignored, "pattern_href_invalid");
                break;
            }
            if chain.iter().any(|n| n.id() == next.id()) {
                self.note(loc, LossKind::Unsupported, "pattern_href_cycle");
                return Ok(PatternPaint::Invalid);
            }
            if chain.len() >= MAX_REFERENCE_DEPTH {
                self.note(loc, LossKind::Unsupported, "pattern_href_chain_too_long");
                return Ok(PatternPaint::NotRepresentable);
            }
            chain.push(next);
        }
        let attr = |name: &str| chain.iter().find_map(|n| plain_attr(*n, name));
        for name in ["patternUnits", "patternContentUnits"] {
            if !matches!(attr(name), None | Some("userSpaceOnUse" | "objectBoundingBox")) {
                self.note(loc, LossKind::Unsupported, "pattern_units_unparsed");
            }
        }
        if let Some(text) = attr("patternTransform") {
            match parse_transform(text) {
                None => self.note(loc, LossKind::Unsupported, "pattern_transform_unparsed"),
                Some(m) if m.inverse().is_none() => {
                    self.note(loc, LossKind::Ignored, "pattern_transform_singular_no_paint");
                    return Ok(PatternPaint::NoPaint);
                }
                Some(_) => {}
            }
        }
        // Zero disables rendering and is also the default; a negative value is an error.
        let size = |name: &str| match attr(name) {
            None => Some(0.0),
            Some(text) => svgtypes::Length::from_str(text.trim())
                .ok()
                .map(|l| l.number)
                .filter(|v| v.is_finite()),
        };
        match (size("width"), size("height")) {
            (Some(w), Some(h)) if w < 0.0 || h < 0.0 => {
                self.note(loc, LossKind::Unsupported, "pattern_size_negative");
                return Ok(PatternPaint::Invalid);
            }
            (Some(w), Some(h)) if w == 0.0 || h == 0.0 => {
                self.note(loc, LossKind::Ignored, "pattern_zero_size_no_paint");
                return Ok(PatternPaint::NoPaint);
            }
            (Some(_), Some(_)) => {}
            _ => {
                self.note(loc, LossKind::Unsupported, "pattern_size_unparsed");
                return Ok(PatternPaint::NotRepresentable);
            }
        }
        let has_content = chain.iter().any(|n| {
            n.children().any(|c| {
                c.is_element()
                    && c.tag_name().namespace() == Some(SVG_NS)
                    && !matches!(c.tag_name().name(), "title" | "desc" | "metadata")
            })
        });
        if !has_content {
            self.note(loc, LossKind::Ignored, "pattern_empty_no_paint");
            return Ok(PatternPaint::NoPaint);
        }
        Ok(PatternPaint::NotRepresentable)
    }
}
