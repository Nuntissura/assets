//! Object styles (lane SMB-LAYOUT-UI; STU-LAY-039, STU-LAY-142): `resolve_object_style` folds the
//! `based_on` chain (ancestors first; a set field overrides, an absent field inherits; category
//! enables are the union along the chain, UNVERIFIED against InDesign, D5); `apply_object_style`
//! sets, in ONE operation, every enabled category Studio stores on the target layer:
//!
//! - text frame general options: column count, gutter, inset, vertical justification, and the
//!   frame's `vertical_balance_columns` (layout-flow records);
//! - baseline options: first-baseline kind and offset; auto-sizing options: the sizing type;
//! - paragraph style: every paragraph of the frame's story (`apply_next_paragraph_style`: the
//!   style's `next_style` from the second paragraph on);
//! - anchored-object options: the layer's anchored settings (when the layer is anchored);
//! - frame-fitting options: the layer's graphic frame `frame_fitting` and fitting anchor
//!   (long-document records);
//! - and records the style as the layer's applied object style.
//!
//! Fill, stroke, corner, export-tagging and ARIA fields are stored on the style and reported in
//! `stored_only` (Folio holds no layer fill/stroke/tag record to receive them; rendering gap).
//! A missing style or layer is `invalid_input`; a cycle is `invalid_input`.
#![deny(clippy::arithmetic_side_effects)]
use crate::LayoutError;
use hsk_studio_folio::{
    StudioAppliedObjectStyle, StudioDocument, StudioFrameFlowOptions, StudioLayoutFlowRecords,
    StudioLongDocRecords, StudioObjectStyle, StudioObjectStyleCategories, StudioTextStory,
    StudioTypeStyleRecords,
};

macro_rules! inherit {
    ($dst:ident, $src:ident, $($f:ident),* $(,)?) => {
        $( if $src.$f.is_some() { $dst.$f = $src.$f.clone(); } )*
    };
}

fn union(a: &mut StudioObjectStyleCategories, b: &StudioObjectStyleCategories) {
    macro_rules! or {
        ($($f:ident),*) => { $( a.$f |= b.$f; )* };
    }
    or!(
        enable_fill, enable_stroke, enable_stroke_and_corner_options, enable_paragraph_style,
        enable_text_frame_general_options, enable_text_frame_baseline_options,
        enable_text_frame_auto_sizing_options, enable_text_frame_column_rule_options,
        enable_text_frame_footnote_options, enable_story_options, enable_text_wrap_and_others,
        enable_anchored_object_options, enable_frame_fitting_options, enable_export_tagging,
        enable_object_export_alt_text_options, enable_object_export_tagged_pdf_options,
        enable_object_export_epub_options, enable_transform_attributes, enable_flex_layout_attributes
    );
}

/// The style with its `based_on` chain folded in (module docs).
pub fn resolve_object_style(
    records: &StudioLayoutFlowRecords,
    style_id: &str,
) -> Result<StudioObjectStyle, LayoutError> {
    let mut chain = Vec::new();
    let mut at = Some(style_id);
    while let Some(id) = at {
        if chain.len() > records.object_styles.len() {
            return Err(LayoutError::InvalidInput);
        }
        let s = records.object_style(id).ok_or(LayoutError::InvalidInput)?;
        chain.push(s);
        at = s.based_on.as_deref();
    }
    let mut out = StudioObjectStyle::new(style_id, chain[0].name.clone());
    out.based_on = chain[0].based_on.clone();
    for s in chain.iter().rev() {
        union(&mut out.categories, &s.categories);
        inherit!(
            out, s, fill, stroke_colour, stroke_weight, miter_limit, end_cap, end_join, stroke_type,
            left_line_end, right_line_end, gap_colour, gap_tint, stroke_alignment,
            left_arrow_head_scale, right_arrow_head_scale, arrow_head_alignment, corner_radii,
            corner_option, paragraph_style, applied_named_grid, keyboard_shortcut,
            extended_keyboard_shortcut, text_column_count, text_column_gutter, inset_spacing,
            vertical_justification, vertical_balance_columns, first_baseline_offset,
            minimum_first_baseline_offset, auto_sizing_type, anchored_settings, frame_fitting,
            fitting_anchor, alt_text, epub_aria_role, epub_aria_label, epub_aria_label_source_type,
        );
        out.overprint_gap |= s.overprint_gap;
        out.apply_next_paragraph_style |= s.apply_next_paragraph_style;
        out.nonprinting |= s.nonprinting;
        out.emit_css |= s.emit_css;
        if !s.export_tags.is_empty() {
            out.export_tags = s.export_tags.clone();
        }
    }
    Ok(out)
}

/// What one application produced: the edited document, records and story (`None` = unchanged),
/// and the enabled categories Studio stores only on the style.
#[derive(Clone, Debug, PartialEq)]
pub struct AppliedObjectStyle {
    pub document: StudioDocument,
    pub flow: StudioLayoutFlowRecords,
    pub longdoc: Option<StudioLongDocRecords>,
    pub story: Option<StudioTextStory>,
    pub stored_only: Vec<&'static str>,
}

/// Applies `style_id` to `layer_id` (module docs). `story` is the frame's story when the
/// paragraph-style category should reach it; `type_styles` resolves `next_style`.
pub fn apply_object_style(
    document: &StudioDocument,
    flow: &StudioLayoutFlowRecords,
    longdoc: Option<&StudioLongDocRecords>,
    story: Option<&StudioTextStory>,
    type_styles: Option<&StudioTypeStyleRecords>,
    layer_id: &str,
    style_id: &str,
) -> Result<AppliedObjectStyle, LayoutError> {
    let s = resolve_object_style(flow, style_id)?;
    let c = &s.categories;
    let mut doc = document.clone();
    let mut flow = flow.clone();
    let mut longdoc = longdoc.cloned();
    let layer = doc
        .layers
        .iter_mut()
        .find(|l| l.layer_id == layer_id)
        .ok_or(LayoutError::InvalidInput)?;
    let mut out_story = None;
    if let Some(f) = layer.text_frame.as_mut() {
        if c.enable_text_frame_general_options {
            if let Some(n) = s.text_column_count {
                f.text_column_count = n;
            }
            if let Some(g) = &s.text_column_gutter {
                f.text_column_gutter = g.clone();
            }
            if let Some(i) = &s.inset_spacing {
                f.inset_spacing = i.clone();
            }
            if let Some(v) = s.vertical_justification {
                f.vertical_justification = v;
            }
            if let Some(b) = s.vertical_balance_columns {
                match flow.frame_options.iter_mut().find(|o| o.layer_id == layer_id) {
                    Some(o) => o.vertical_balance_columns = b,
                    None => flow.frame_options.push(StudioFrameFlowOptions {
                        layer_id: layer_id.to_owned(),
                        vertical_balance_columns: b,
                    }),
                }
            }
        }
        if c.enable_text_frame_baseline_options {
            if let Some(k) = s.first_baseline_offset {
                f.first_baseline_offset = k;
            }
            if let Some(m) = &s.minimum_first_baseline_offset {
                f.minimum_first_baseline_offset = m.clone();
            }
        }
        if c.enable_text_frame_auto_sizing_options
            && let Some(a) = s.auto_sizing_type
        {
            f.auto_sizing_type = a;
        }
        if c.enable_paragraph_style
            && let Some(p) = &s.paragraph_style
            && let Some(st) = story
        {
            let shows = matches!(&layer.payload, hsk_studio_folio::Payload::Text { story_id } if *story_id == st.story_id);
            if !shows {
                return Err(LayoutError::InvalidInput);
            }
            let next = if s.apply_next_paragraph_style {
                type_styles
                    .and_then(|t| t.style(p))
                    .and_then(|ps| ps.next_style.clone())
            } else {
                None
            };
            let mut edited = st.clone();
            for (k, para) in edited.paragraphs.iter_mut().enumerate() {
                para.style_ref = Some(match (&next, k) {
                    (Some(n), k) if k > 0 => n.clone(),
                    _ => p.clone(),
                });
            }
            out_story = Some(edited);
        }
    }
    if c.enable_anchored_object_options
        && let Some(a) = &s.anchored_settings
        && let Some(obj) = flow.anchored_objects.iter_mut().find(|o| o.layer_id == layer_id)
    {
        obj.settings = a.clone();
    }
    if c.enable_frame_fitting_options
        && let Some(ld) = longdoc.as_mut()
        && let Some(g) = ld.graphic_frames.iter_mut().find(|g| g.layer_id == layer_id)
    {
        if s.frame_fitting.is_some() {
            g.frame_fitting = s.frame_fitting;
        }
        if let Some(a) = s.fitting_anchor {
            g.anchor = a;
        }
    }
    let mut stored_only = Vec::new();
    for (on, name) in [
        (c.enable_fill, "fill"),
        (c.enable_stroke, "stroke"),
        (c.enable_stroke_and_corner_options, "stroke_and_corner_options"),
        (c.enable_text_frame_column_rule_options, "text_frame_column_rule_options"),
        (c.enable_text_frame_footnote_options, "text_frame_footnote_options"),
        (c.enable_story_options, "story_options"),
        (c.enable_text_wrap_and_others, "text_wrap_and_others"),
        (c.enable_export_tagging, "export_tagging"),
        (c.enable_object_export_alt_text_options, "object_export_alt_text_options"),
        (c.enable_object_export_tagged_pdf_options, "object_export_tagged_pdf_options"),
        (c.enable_object_export_epub_options, "object_export_epub_options"),
        (c.enable_transform_attributes, "transform_attributes"),
        (c.enable_flex_layout_attributes, "flex_layout_attributes"),
    ] {
        if on {
            stored_only.push(name);
        }
    }
    match flow.applied_object_styles.iter_mut().find(|a| a.layer_id == layer_id) {
        Some(a) => a.style_id = style_id.to_owned(),
        None => flow.applied_object_styles.push(StudioAppliedObjectStyle {
            layer_id: layer_id.to_owned(),
            style_id: style_id.to_owned(),
        }),
    }
    Ok(AppliedObjectStyle { document: doc, flow, longdoc, story: out_story, stored_only })
}
