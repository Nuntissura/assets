//! Create Outlines for any composed text (lane SMB-AI-AREA-TYPE-1; STU-VEC-038 "Converting text to
//! outlines produces standard vector geometry", STU-TYP-127: Type owns outline extraction). Point,
//! area and path layouts all expose their placed glyphs through [`ComposedGlyphs`]; each placed
//! glyph's outline (font units, y up) is scaled by `font_size_pt / units_per_em` and mapped by the
//! glyph transform into document units (y down), exactly as [`crate::place_outline`] does for
//! path text.
//!
//! Output: one [`OutlinedGlyph`] per glyph with ink, in glyph order, carrying the caller's paint
//! index (the story's fill row) and the glyph's contours as ONE compound path: every contour is a
//! closed subpath (a missing close is added), so counters stay holes under the non-zero rule
//! (declared reading of Illustrator's compound-path result; contour order and winding are the
//! font's, Illustrator parity UNVERIFIED, D5). Glyphs without outline commands (spaces) produce
//! nothing. Bounds (V4-01/02): glyph and aggregate command counts are checked before allocation.
#![deny(clippy::arithmetic_side_effects)]
use crate::{AreaTextLayout, BaselineCommand, GlyphOutline, OutlineCommand, PathTextLayout, PointTextLayout};

/// A composed layout's placed glyphs: `(glyph index, run, transform)`.
pub trait ComposedGlyphs {
    fn placed(&self) -> Vec<(usize, usize, [f64; 6])>;
}
impl ComposedGlyphs for PathTextLayout {
    fn placed(&self) -> Vec<(usize, usize, [f64; 6])> {
        self.glyphs.iter().map(|g| (g.index, g.run, g.transform)).collect()
    }
}
impl ComposedGlyphs for AreaTextLayout {
    fn placed(&self) -> Vec<(usize, usize, [f64; 6])> {
        self.glyphs.iter().map(|g| (g.index, g.run, g.transform)).collect()
    }
}
impl ComposedGlyphs for PointTextLayout {
    fn placed(&self) -> Vec<(usize, usize, [f64; 6])> {
        self.glyphs.iter().map(|g| (g.index, g.run, g.transform)).collect()
    }
}

/// Caller bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreateOutlinesLimits {
    pub max_glyphs: usize,
    /// Output commands (aggregate).
    pub max_commands: usize,
}
impl Default for CreateOutlinesLimits {
    fn default() -> Self {
        Self { max_glyphs: 1 << 20, max_commands: 1 << 24 }
    }
}

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateOutlinesError {
    InvalidInput(&'static str),
    OverLimit(&'static str),
}
impl CreateOutlinesError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "create_outlines_invalid_input",
            Self::OverLimit(_) => "create_outlines_over_limit",
        }
    }
}

/// What the caller states per glyph of the story (indexed by glyph index).
#[derive(Clone, Copy, Debug)]
pub struct OutlineSource<'a> {
    pub outline: &'a GlyphOutline,
    pub font_size_pt: f64,
    /// The story fill row of the glyph.
    pub paint: usize,
}

/// One outlined glyph: a compound path in document units.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlinedGlyph {
    pub index: usize,
    pub paint: usize,
    pub contours: usize,
    pub commands: Vec<BaselineCommand>,
}

/// Outlines of every placed glyph of `layout` (module docs). `sources[i]` describes glyph `i`.
pub fn create_outlines(layout: &dyn ComposedGlyphs, sources: &[OutlineSource<'_>], limits: &CreateOutlinesLimits) -> Result<Vec<OutlinedGlyph>, CreateOutlinesError> {
    let placed = layout.placed();
    if placed.len() > limits.max_glyphs || sources.len() > limits.max_glyphs {
        return Err(CreateOutlinesError::OverLimit("max_glyphs"));
    }
    let mut total = 0_usize;
    for (index, _, _) in &placed {
        let s = sources.get(*index).ok_or(CreateOutlinesError::InvalidInput("glyph_index"))?;
        total = total.checked_add(s.outline.commands().len()).and_then(|t| t.checked_add(1)).ok_or(CreateOutlinesError::OverLimit("max_commands"))?;
        if total > limits.max_commands {
            return Err(CreateOutlinesError::OverLimit("max_commands"));
        }
    }
    let mut out = Vec::new();
    for (index, _, m) in placed {
        let s = &sources[index];
        if s.outline.commands().is_empty() {
            continue;
        }
        let upem = f64::from(s.outline.units_per_em());
        if !(s.font_size_pt.is_finite() && s.font_size_pt > 0.0 && upem > 0.0) {
            return Err(CreateOutlinesError::InvalidInput("font_size"));
        }
        if !m.iter().all(|v| v.is_finite()) {
            return Err(CreateOutlinesError::InvalidInput("transform"));
        }
        let k = s.font_size_pt / upem;
        let map = |p: [f32; 2]| {
            let (x, y) = (f64::from(p[0]) * k, f64::from(p[1]) * k);
            [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]
        };
        let mut commands = Vec::with_capacity(s.outline.commands().len().saturating_add(1));
        let (mut open, mut contours) = (false, 0_usize);
        for c in s.outline.commands() {
            match *c {
                OutlineCommand::MoveTo(p) => {
                    if open {
                        commands.push(BaselineCommand::Close);
                    }
                    commands.push(BaselineCommand::MoveTo(map(p)));
                    open = true;
                    contours = contours.saturating_add(1);
                }
                OutlineCommand::LineTo(p) => commands.push(BaselineCommand::LineTo(map(p))),
                OutlineCommand::QuadTo(a, p) => commands.push(BaselineCommand::QuadTo(map(a), map(p))),
                OutlineCommand::CurveTo(a, b, p) => commands.push(BaselineCommand::CubicTo(map(a), map(b), map(p))),
                OutlineCommand::Close => {
                    if open {
                        commands.push(BaselineCommand::Close);
                        open = false;
                    }
                }
            }
        }
        if open {
            commands.push(BaselineCommand::Close);
        }
        if contours > 0 {
            out.push(OutlinedGlyph { index, paint: s.paint, contours, commands });
        }
    }
    Ok(out)
}
