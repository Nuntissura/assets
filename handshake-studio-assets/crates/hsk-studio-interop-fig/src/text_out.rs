//! Text outputs of an imported Figma document (lane SMB-INTEROP-FIG-INSTANCES-FRAMES): every text
//! layer's lines and runs (`NativeProposal::text_layouts`, from Figma's stored layout) with each
//! run's requested font resolved against fonts the caller grants, as Type `FontResource`s (Type
//! takes caller-granted fonts only; it never discovers host fonts and identity strings are the
//! caller's, never parsed from font files). The caller shapes each output with Type's
//! `TextEngine` and draws it (composite `prepare_text`).
//!
//! Nothing is substituted silently: a run whose (family, style) has no grant makes the layer a
//! [`MissingFont`] (typed, with the requested family and style) and no output; a layer without a
//! placed line or a solid fill is a typed loss as well (tests:
//! fig_instances_frames::text_output_resolves_granted_font_and_types_missing_family).
#![deny(clippy::arithmetic_side_effects)]
use crate::native::{FigTextLine, NativeProposal};
use crate::nodes::NodeLoss;
use hsk_studio_type::FontResource;
use std::collections::BTreeMap;

/// One caller-granted font: the Figma family and style it answers for and its Type resource.
#[derive(Clone, Copy, Debug)]
pub struct FigFontGrant<'a> {
    pub family: &'a str,
    pub style: &'a str,
    pub resource: FontResource<'a>,
}

/// One run of a text output with its resolved font.
#[derive(Clone, Copy, Debug)]
pub struct FigTextOutputRun<'a> {
    /// Byte range of the story text.
    pub start: u64,
    pub end: u64,
    pub size_px: f64,
    /// Straight RGBA, 0..=1.
    pub fill_rgba: [f64; 4],
    pub font: FontResource<'a>,
}

/// A text layer ready to shape and draw.
#[derive(Clone, Debug)]
pub struct FigTextOutput<'a> {
    pub layer_id: String,
    pub story_id: String,
    pub lines: Vec<FigTextLine>,
    pub runs: Vec<FigTextOutputRun<'a>>,
}

/// A text layer not drawn because a requested font has no grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingFont {
    pub layer_id: String,
    pub family: String,
    pub style: String,
}

#[derive(Clone, Debug, Default)]
pub struct FigTextOutputs<'a> {
    pub outputs: Vec<FigTextOutput<'a>>,
    pub missing_fonts: Vec<MissingFont>,
    /// Aggregated: `native.text.font=<family>/<style>` font_not_granted, and
    /// `native.text.output` text_output_unplaced | text_output_fill_not_solid |
    /// text_output_size_missing.
    pub loss: Vec<NodeLoss>,
}

/// Resolves the fonts of every text layout of `proposal` against `grants` (exact family and
/// style match; the first matching grant wins).
pub fn resolve_text_outputs<'a>(
    proposal: &NativeProposal,
    grants: &[FigFontGrant<'a>],
) -> FigTextOutputs<'a> {
    let mut out = FigTextOutputs::default();
    let mut loss: BTreeMap<(String, &'static str), (u32, Option<crate::SourceId>)> =
        BTreeMap::new();
    let mut lose = |path: String, code: &'static str, at| {
        let slot = loss.entry((path, code)).or_insert((0, Some(at)));
        slot.0 = slot.0.saturating_add(1);
    };
    for layout in &proposal.text_layouts {
        let mut runs = Vec::new();
        let mut ok = true;
        for r in &layout.runs {
            let family = r.family.clone().unwrap_or_default();
            let style = r.style.clone().unwrap_or_default();
            let grant = grants
                .iter()
                .find(|g| g.family == family && g.style == style);
            let Some(grant) = grant else {
                lose(
                    format!("native.text.font={family}/{style}"),
                    "font_not_granted",
                    layout.source,
                );
                let missing = MissingFont {
                    layer_id: layout.layer_id.clone(),
                    family,
                    style,
                };
                if !out.missing_fonts.contains(&missing) {
                    out.missing_fonts.push(missing);
                }
                ok = false;
                continue;
            };
            let (Some(size_px), Some(fill_rgba)) = (r.size_px, r.fill_rgba) else {
                lose(
                    "native.text.output".to_owned(),
                    if r.size_px.is_none() {
                        "text_output_size_missing"
                    } else {
                        "text_output_fill_not_solid"
                    },
                    layout.source,
                );
                ok = false;
                continue;
            };
            runs.push(FigTextOutputRun {
                start: r.start,
                end: r.end,
                size_px,
                fill_rgba,
                font: grant.resource,
            });
        }
        if layout.lines.is_empty() {
            lose(
                "native.text.output".to_owned(),
                "text_output_unplaced",
                layout.source,
            );
            ok = false;
        }
        if ok && !runs.is_empty() {
            out.outputs.push(FigTextOutput {
                layer_id: layout.layer_id.clone(),
                story_id: layout.story_id.clone(),
                lines: layout.lines.clone(),
                runs,
            });
        }
    }
    out.loss = loss
        .into_iter()
        .map(|((path, code), (count, first))| NodeLoss {
            path,
            code,
            count,
            first,
        })
        .collect();
    out
}
