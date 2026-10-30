//! Export of the Folio vector records: the layer tree walk, the `clip-path` and `mask` attributes
//! of attached layers, and the `<clipPath>`, `<mask>` and `<pattern>` definitions.
//!
//! Everything is written in the export's pixel space (`document point * k`) with
//! `userSpaceOnUse` units, because the import bakes every transform and unit into document
//! points: a clip is its member paths, a mask is its region plus its content layers, a pattern is
//! its tile cell, its artwork in tile space and its placement matrix as `patternTransform`.
//! Definitions are queued when first referenced and written after the body (a worklist, no
//! recursion), so a definition that references another one cannot grow the stack.
use super::*;
use hsk_studio_folio::{
    StudioClipRule, StudioMaskMode, StudioPatternPlacement, StudioPatternSource, StudioTileType,
};

pub(super) fn recs_of(r: &StudioVectorRecords, unit_pt: f64) -> Recs<'_> {
    Recs {
        clips: r.clips.iter().map(|c| (c.clip_id.as_str(), c)).collect(),
        masks: r.masks.iter().map(|m| (m.mask_id.as_str(), m)).collect(),
        patterns: r
            .patterns
            .iter()
            .map(|p| (p.pattern_id.as_str(), p))
            .collect(),
        paints: r
            .pattern_paints
            .iter()
            .map(|p| ((p.style_id.as_str(), p.row), p))
            .collect(),
        attach: r
            .attachments
            .iter()
            .map(|a| (a.layer_id.as_str(), a))
            .collect(),
        markers: r
            .stroke_markers
            .iter()
            .map(|m| ((m.style_id.as_str(), m.row), m))
            .collect(),
        unit_pt,
    }
}

/// Source layers of the records that are hidden: they exist only to be written inside their
/// definition, not as elements of the document.
pub(super) fn definition_roots(tree: &Tree<'_>, r: &StudioVectorRecords) -> BTreeSet<usize> {
    let sources = r
        .clips
        .iter()
        .map(|c| c.source_layer_id.as_str())
        .chain(r.masks.iter().map(|m| m.source_layer_id.as_str()))
        .chain(r.patterns.iter().filter_map(|p| match &p.source {
            StudioPatternSource::Definition {
                content_layer_id, ..
            } => Some(content_layer_id.as_str()),
            StudioPatternSource::LiveSource { .. } => None,
        }));
    let mut skip = BTreeSet::new();
    for id in sources {
        if let Some(&i) = tree.index.get(id)
            && !tree.doc.layers[i].visible
        {
            skip.insert(i);
        }
    }
    skip
}

impl<'a> Writer<'a> {
    // ------------------------------------------------------------------------------------------
    // References
    // ------------------------------------------------------------------------------------------

    fn attachment_of(&self, layer_id: &str) -> Option<&'a StudioMaskAttachment> {
        self.recs
            .as_ref()
            .and_then(|r| r.attach.get(layer_id).copied())
    }

    /// Id of the `<clipPath>` of clip record `clip_id`, queued on first use.
    fn ref_clip(&mut self, clip_id: &str) -> Option<String> {
        let clip: &'a StudioVectorClip = *self.recs.as_ref()?.clips.get(clip_id)?;
        if let Some(id) = self.clip_ids.get(clip.clip_id.as_str()) {
            return Some(id.clone());
        }
        let id = self.defs.fresh("c");
        self.clip_ids.insert(clip.clip_id.as_str(), id.clone());
        self.pending.push_back(Pending::Clip(clip, id.clone()));
        Some(id)
    }

    /// Id of the `<mask>` of mask record `mask_id`, queued on first use; `None` when the mask
    /// cannot be written (its layers are then written unmasked, with a loss).
    fn ref_mask(&mut self, mask_id: &str, label: &str) -> Option<String> {
        let mask: &'a StudioVectorMask = *self.recs.as_ref()?.masks.get(mask_id)?;
        if let Some(cached) = self.mask_ids.get(mask.mask_id.as_str()) {
            return cached.clone();
        }
        let id = if mask.mode == StudioMaskMode::Vector {
            note(
                &mut self.loss,
                label,
                LossKind::Unsupported,
                "mask_mode_vector_not_exported",
            );
            None
        } else {
            if mask.invert_mask {
                note(
                    &mut self.loss,
                    label,
                    LossKind::Unsupported,
                    "mask_invert_not_exported",
                );
            }
            if mask.mask_clip {
                note(
                    &mut self.loss,
                    label,
                    LossKind::Approximated,
                    "mask_clip_not_exported",
                );
            }
            let id = self.defs.fresh("m");
            self.pending.push_back(Pending::Mask(mask, id.clone()));
            Some(id)
        };
        self.mask_ids.insert(mask.mask_id.as_str(), id.clone());
        id
    }

    /// Id of the `<pattern>` that row `row` of style `style_id` paints with, queued on first use.
    /// `None` when the row is not bound or its binding cannot be written (stand-in paint then).
    pub(super) fn ref_pattern(
        &mut self,
        style_id: &str,
        row: usize,
        label: &str,
    ) -> Option<String> {
        let row = u32::try_from(row).ok()?;
        let paint = *self.recs.as_ref()?.paints.get(&(style_id, row))?;
        let pattern: &'a StudioPattern = *self
            .recs
            .as_ref()?
            .patterns
            .get(paint.pattern_id.as_str())?;
        let StudioPatternPlacement::Matrix { matrix } = &paint.placement else {
            note(
                &mut self.loss,
                label,
                LossKind::Unsupported,
                "pattern_parametric_placement_not_exported",
            );
            return None;
        };
        if !matches!(pattern.source, StudioPatternSource::Definition { .. }) {
            note(
                &mut self.loss,
                label,
                LossKind::Unsupported,
                "pattern_live_source_not_exported",
            );
            return None;
        }
        let key = (pattern.pattern_id.as_str(), matrix.map(f64::to_bits));
        if let Some(id) = self.pattern_ids.get(&key) {
            return Some(id.clone());
        }
        let id = self.defs.fresh("p");
        self.pattern_ids.insert(key, id.clone());
        self.pending
            .push_back(Pending::Pattern(pattern, id.clone(), *matrix));
        Some(id)
    }

    /// ` clip-path="url(#..)" mask="url(#..)"` for the attachment of a layer. `masks` is false
    /// for the members of a clipPath, where a mask has no effect.
    pub(super) fn reference_attrs(&mut self, layer_id: &str, label: &str, masks: bool) -> String {
        let Some(a) = self.attachment_of(layer_id) else {
            return String::new();
        };
        let mut out = String::new();
        if let Some(c) = &a.clip_id
            && let Some(id) = self.ref_clip(c)
        {
            out.push_str(&format!(" clip-path=\"url(#{id})\""));
        }
        if let Some(m) = &a.mask_id {
            if !masks {
                note(
                    &mut self.loss,
                    label,
                    LossKind::Ignored,
                    "clip_member_mask_not_exported",
                );
            } else if let Some(id) = self.ref_mask(m, label) {
                out.push_str(&format!(" mask=\"url(#{id})\""));
            }
        }
        out
    }

    // ------------------------------------------------------------------------------------------
    // Layer tree
    // ------------------------------------------------------------------------------------------

    /// Writes `roots` and everything below them as `<g>` and `<path>` elements. `skip` holds the
    /// hidden definition roots (written in their definition only); `unhide` is a root that is
    /// written even though it is in `skip` or hidden. Iterative, no recursion.
    pub(super) fn write_layers(
        &mut self,
        tree: &Tree<'a>,
        roots: &[usize],
        depth0: usize,
        unhide: Option<usize>,
        skip: &BTreeSet<usize>,
        body: &mut String,
    ) -> Result<()> {
        let doc = tree.doc;
        // Iterative walk: (layer index, entering).
        let mut stack: Vec<(usize, bool)> = roots
            .iter()
            .rev()
            .filter(|&&i| !skip.contains(&i) || Some(i) == unhide)
            .map(|&i| (i, true))
            .collect();
        let mut depth = depth0;
        while let Some((i, entering)) = stack.pop() {
            let layer = &doc.layers[i];
            if !entering {
                depth = depth.saturating_sub(1);
                body.push_str(&"  ".repeat(depth));
                body.push_str("</g>\n");
                continue;
            }
            let tag = if matches!(layer.payload, Payload::Group) {
                "g"
            } else {
                "path"
            };
            let label = format!("/layer[{}]", i.saturating_add(1));
            let mut extra = String::new();
            if layer.name != tag
                && !layer.name.is_empty()
                && (!self.in_def || !self.emitted.contains(&layer.name))
            {
                self.emitted.insert(layer.name.clone());
                extra.push_str(&format!(" id=\"{}\"", attr_text(&layer.name)));
            }
            if let Some(a) = &layer.appearance
                && a.opacity < 100.0
            {
                extra.push_str(&format!(" opacity=\"{}\"", num(a.opacity / 100.0)));
            }
            if !layer.visible && Some(i) != unhide {
                extra.push_str(" display=\"none\"");
            }
            extra.push_str(&self.reference_attrs(&layer.layer_id, &label, true));
            match &layer.payload {
                Payload::Group => {
                    appearance_losses(layer, &label, &mut self.loss);
                    body.push_str(&"  ".repeat(depth));
                    body.push_str(&format!("<g{extra}>\n"));
                    depth = depth.saturating_add(1);
                    stack.push((i, false));
                    for &c in tree
                        .children
                        .get(&Some(layer.layer_id.as_str()))
                        .into_iter()
                        .flatten()
                        .rev()
                    {
                        if !skip.contains(&c) {
                            stack.push((c, true));
                        }
                    }
                }
                Payload::Vector { .. } => {
                    let Some(indices) = tree.by_layer.get(layer.layer_id.as_str()) else {
                        continue;
                    };
                    let group: Vec<(&VectorPath, &[Seg])> = indices
                        .iter()
                        .map(|&j| (&tree.paths[j], tree.segs[j].as_slice()))
                        .collect();
                    if !group.is_empty() {
                        appearance_losses(layer, &label, &mut self.loss);
                        self.layer_paths(body, depth, &group, &extra, &label)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------------------------------
    // Definitions
    // ------------------------------------------------------------------------------------------

    /// Writes every queued definition (and those they queue) to `<defs>`.
    pub(super) fn drain_defs(&mut self, tree: &Tree<'a>, skip: &BTreeSet<usize>) -> Result<()> {
        self.in_def = true;
        while let Some(pending) = self.pending.pop_front() {
            match pending {
                Pending::Clip(clip, id) => self.write_clip(tree, clip, &id)?,
                Pending::Mask(mask, id) => self.write_mask(tree, skip, mask, &id)?,
                Pending::Pattern(pattern, id, matrix) => {
                    self.write_pattern(tree, skip, pattern, &id, matrix)?;
                }
            }
        }
        self.in_def = false;
        Ok(())
    }

    /// Children of `root` in document order (the content of a group written without a wrapper).
    fn children_of(tree: &Tree<'a>, root: usize) -> Vec<usize> {
        tree.children
            .get(&Some(tree.doc.layers[root].layer_id.as_str()))
            .cloned()
            .unwrap_or_default()
    }

    fn write_clip(&mut self, tree: &Tree<'a>, clip: &'a StudioVectorClip, id: &str) -> Result<()> {
        let Some(&root) = tree.index.get(clip.source_layer_id.as_str()) else {
            return Ok(());
        };
        let doc = tree.doc;
        let label = format!("/layer[{}]", root.saturating_add(1));
        // Members: the vector layers below the root, visible ones only (a hidden member adds
        // nothing to the region), in document order.
        let mut leaves = Vec::new();
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            let layer = &doc.layers[i];
            match &layer.payload {
                Payload::Vector { .. } => leaves.push(i),
                Payload::Group => {
                    if i != root && self.attachment_of(&layer.layer_id).is_some() {
                        note(
                            &mut self.loss,
                            &label,
                            LossKind::Unsupported,
                            "clip_group_reference_not_exported",
                        );
                    }
                    for &c in tree
                        .children
                        .get(&Some(layer.layer_id.as_str()))
                        .into_iter()
                        .flatten()
                        .rev()
                    {
                        if doc.layers[c].visible {
                            stack.push(c);
                        }
                    }
                }
                _ => note(
                    &mut self.loss,
                    &label,
                    LossKind::Unsupported,
                    "clip_member_kind_not_exported",
                ),
            }
        }
        let root_layer = &doc.layers[root];
        let nested = if matches!(root_layer.payload, Payload::Group) {
            self.reference_attrs(&root_layer.layer_id, &label, false)
        } else {
            String::new()
        };
        let rule = if clip.clip_rule == StudioClipRule::EvenOdd {
            " clip-rule=\"evenodd\""
        } else {
            ""
        };
        let mut out =
            format!("    <clipPath id=\"{id}\" clipPathUnits=\"userSpaceOnUse\"{nested}>\n");
        for leaf in leaves {
            let layer = &doc.layers[leaf];
            let Some(indices) = tree.by_layer.get(layer.layer_id.as_str()) else {
                continue;
            };
            let d = indices
                .iter()
                .map(|&j| path_data(&tree.paths[j], &tree.segs[j], self.k))
                .collect::<Vec<_>>()
                .join(" ");
            let refs = self.reference_attrs(&layer.layer_id, &label, false);
            out.push_str(&format!("      <path d=\"{d}\"{rule}{refs}/>\n"));
        }
        out.push_str("    </clipPath>\n");
        self.defs.text.push_str(&out);
        Ok(())
    }

    fn write_mask(
        &mut self,
        tree: &Tree<'a>,
        skip: &BTreeSet<usize>,
        mask: &'a StudioVectorMask,
        id: &str,
    ) -> Result<()> {
        let Some(&root) = tree.index.get(mask.source_layer_id.as_str()) else {
            return Ok(());
        };
        let (ppi, k) = (self.ppi, self.k);
        // A mask without a region is unbounded: a region far beyond any artboard stands for it.
        let (x, y, w, h) = match &mask.region {
            Some(b) => (
                to_pt(&b.x, ppi)? * k,
                to_pt(&b.y, ppi)? * k,
                to_pt(&b.width, ppi)? * k,
                to_pt(&b.height, ppi)? * k,
            ),
            None => (-1.0e6, -1.0e6, 2.0e6, 2.0e6),
        };
        let mut head = format!(
            "    <mask id=\"{id}\" maskUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
            num(x),
            num(y),
            num(w),
            num(h)
        );
        if mask.mode == StudioMaskMode::Alpha {
            head.push_str(" mask-type=\"alpha\"");
        }
        head.push_str(">\n");
        let layer = &tree.doc.layers[root];
        let plain = matches!(layer.payload, Payload::Group)
            && layer.appearance.is_none()
            && self.attachment_of(&layer.layer_id).is_none();
        let (roots, unhide) = if plain {
            (Self::children_of(tree, root), None)
        } else {
            (vec![root], Some(root))
        };
        let mut content = String::new();
        self.write_layers(tree, &roots, 3, unhide, skip, &mut content)?;
        self.defs.text.push_str(&head);
        self.defs.text.push_str(&content);
        self.defs.text.push_str("    </mask>\n");
        Ok(())
    }

    /// Is `clip` the tile cell `[0, w] x [0, h]` (one closed straight rectangle)? An import adds
    /// it when tile content leaves the cell, which SVG patterns clip by default.
    fn is_cell_clip(tree: &Tree<'a>, clip: &StudioVectorClip, w: f64, h: f64) -> bool {
        let Some(&root) = tree.index.get(clip.source_layer_id.as_str()) else {
            return false;
        };
        let members = Self::children_of(tree, root);
        let [only] = members.as_slice() else {
            return false;
        };
        let Some(paths) = tree.by_layer.get(tree.doc.layers[*only].layer_id.as_str()) else {
            return false;
        };
        let [j] = paths.as_slice() else {
            return false;
        };
        let (path, segs) = (&tree.paths[*j], &tree.segs[*j]);
        if !path.closed
            || segs.len() != 4
            || !segs.iter().all(|s| s.cin == s.pos && s.cout == s.pos)
        {
            return false;
        }
        let tol = 1e-9 * w.max(h).max(1.0);
        let near =
            |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() <= tol && (a[1] - b[1]).abs() <= tol;
        [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]]
            .iter()
            .all(|c| segs.iter().any(|s| near(s.pos, *c)))
    }

    fn write_pattern(
        &mut self,
        tree: &Tree<'a>,
        skip: &BTreeSet<usize>,
        pattern: &'a StudioPattern,
        id: &str,
        matrix: [f64; 6],
    ) -> Result<()> {
        let StudioPatternSource::Definition {
            content_layer_id,
            tile_width,
            tile_height,
            brick_offset,
            ..
        } = &pattern.source
        else {
            return Ok(());
        };
        let Some(&root) = tree.index.get(content_layer_id.as_str()) else {
            return Ok(());
        };
        let (ppi, k) = (self.ppi, self.k);
        let label = format!("/layer[{}]", root.saturating_add(1));
        let (tw, th) = (to_pt(tile_width, ppi)?, to_pt(tile_height, ppi)?);
        let (hs, vs) = (
            to_pt(&pattern.horizontal_spacing, ppi)?,
            to_pt(&pattern.vertical_spacing, ppi)?,
        );
        if pattern.tile_type != StudioTileType::Grid || brick_offset.is_some_and(|b| b != 0.0) {
            note(
                &mut self.loss,
                &label,
                LossKind::Approximated,
                "pattern_tile_type_not_exported",
            );
        }
        if hs < 0.0 || vs < 0.0 {
            note(
                &mut self.loss,
                &label,
                LossKind::Unsupported,
                "pattern_negative_spacing_not_exported",
            );
        }
        // Positive spacing is empty space after each tile: a larger cell with the tile at its
        // origin.
        let (cell_w, cell_h) = ((tw + hs.max(0.0)) * k, (th + vs.max(0.0)) * k);
        let u = self.recs.as_ref().map_or(1.0, |r| r.unit_pt) * k;
        let transform = if matrix == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
            String::new()
        } else {
            format!(
                " patternTransform=\"matrix({} {} {} {} {} {})\"",
                num(matrix[0]),
                num(matrix[1]),
                num(matrix[2]),
                num(matrix[3]),
                num(matrix[4] * u),
                num(matrix[5] * u)
            )
        };
        let layer = &tree.doc.layers[root];
        let attach = self.attachment_of(&layer.layer_id);
        let cell_clip = attach
            .and_then(|a| a.clip_id.as_deref())
            .and_then(|c| self.recs.as_ref().and_then(|r| r.clips.get(c).copied()))
            .is_some_and(|c| Self::is_cell_clip(tree, c, tw, th));
        let other =
            attach.is_some_and(|a| a.mask_id.is_some() || (a.clip_id.is_some() && !cell_clip));
        let plain = matches!(layer.payload, Payload::Group) && layer.appearance.is_none() && !other;
        let (roots, unhide) = if plain {
            (Self::children_of(tree, root), None)
        } else {
            (vec![root], Some(root))
        };
        let mut content = String::new();
        self.write_layers(tree, &roots, 3, unhide, skip, &mut content)?;
        self.defs.text.push_str(&format!(
            "    <pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" width=\"{}\" height=\"{}\"{transform}>\n",
            num(cell_w),
            num(cell_h)
        ));
        self.defs.text.push_str(&content);
        self.defs.text.push_str("    </pattern>\n");
        Ok(())
    }
}
