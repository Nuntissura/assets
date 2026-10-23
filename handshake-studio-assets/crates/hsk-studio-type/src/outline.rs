//! Glyph outline extraction (STU-TYP-127: `TextEngine` owns outline extraction and rasterisation).
//! Outlines come from the vendored, bounded parser in font units, y up, default instance only.
#![deny(clippy::arithmetic_side_effects)]
use crate::provider::{Context, ttf};
use crate::{
    AdmissionPort, AllocationSnapshot, EpochPort, Error, FontResource, Generation, Reservation,
};
use hsk_studio_accord::CancellationToken;

/// Caller-granted font bytes with their explicit provenance (no host discovery).
#[derive(Clone, Copy, Debug)]
pub struct FontBytes<'a> {
    pub bytes: &'a [u8],
    pub resource: FontResource<'a>,
}

/// One outline command in font units, y up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutlineCommand {
    MoveTo([f32; 2]),
    LineTo([f32; 2]),
    QuadTo([f32; 2], [f32; 2]),
    CurveTo([f32; 2], [f32; 2], [f32; 2]),
    Close,
}

/// Monochrome outline of one glyph of the default instance; derived, disposable data.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphOutline {
    glyph_id: u32,
    units_per_em: u16,
    font_hash: [u8; 32],
    commands: Vec<OutlineCommand>,
    bounds: Option<[i16; 4]>,
}
impl GlyphOutline {
    /// Caller-built outline (tests, vector sources): glyph 0, zero font hash, no bounds.
    pub fn synthetic(units_per_em: u16, commands: Vec<OutlineCommand>) -> Self {
        Self {
            glyph_id: 0,
            units_per_em,
            font_hash: [0; 32],
            commands,
            bounds: None,
        }
    }
    pub fn glyph_id(&self) -> u32 {
        self.glyph_id
    }
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
    /// Content SHA-256 of the font the outline was extracted from (zero for synthetic outlines).
    pub fn font_hash(&self) -> [u8; 32] {
        self.font_hash
    }
    pub fn commands(&self) -> &[OutlineCommand] {
        &self.commands
    }
    /// Parser bounds `[x_min, y_min, x_max, y_max]` in font units; `None` for an empty glyph.
    pub fn bounds(&self) -> Option<[i16; 4]> {
        self.bounds
    }
}

/// Explicit caps; every one is checked before the work or allocation it bounds.
#[derive(Clone, Copy, Debug)]
pub struct OutlineLimits {
    pub max_font_bytes: u64,
    pub max_glyphs: u64,
    /// Outline commands per glyph.
    pub max_segments: u64,
    /// Outline commands per call (aggregate).
    pub max_total_segments: u64,
    pub max_work_units: u64,
}
impl Default for OutlineLimits {
    fn default() -> Self {
        Self {
            max_font_bytes: 32 << 20,
            max_glyphs: 65_536,
            max_segments: 16_384,
            max_total_segments: 1 << 22,
            max_work_units: 1 << 24,
        }
    }
}

/// Typed refusal for outline and raster work. `Type` carries the crate's fixed codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphError {
    Type(Error),
    GlyphOutlineUnavailable,
    VariableOutlineUnsupported,
    OutlineOverLimit,
    RasterOverLimit,
}
impl GlyphError {
    /// Stable snake-case code; carries no font names, bytes or coordinates.
    pub fn code(&self) -> &'static str {
        match self {
            GlyphError::Type(_) => "type_error",
            GlyphError::GlyphOutlineUnavailable => "glyph_outline_unavailable",
            GlyphError::VariableOutlineUnsupported => "variable_outline_unsupported",
            GlyphError::OutlineOverLimit => "outline_over_limit",
            GlyphError::RasterOverLimit => "raster_over_limit",
        }
    }
}
impl From<Error> for GlyphError {
    fn from(error: Error) -> Self {
        GlyphError::Type(error)
    }
}

const RECURSION: u64 = 64;

/// Outline work has no epoch or admission ledger; the Context needs both ports.
struct FixedEpoch;
impl EpochPort for FixedEpoch {
    fn current_epoch(&self) -> Result<u64, Error> {
        Ok(0)
    }
}
struct NoAdmission;
impl AdmissionPort for NoAdmission {
    fn reserve(&self, _: u64, _: u64) -> Result<Reservation<'_>, Error> {
        Err(Error::Budget)
    }
    fn snapshot(&self) -> Result<AllocationSnapshot, Error> {
        Ok(AllocationSnapshot::default())
    }
    fn retain_generation(&self, _: u64) -> Result<Generation<'_>, Error> {
        Err(Error::LeaseUnavailable)
    }
}

struct Bounded {
    commands: Vec<OutlineCommand>,
    cap: u64,
    over: bool,
    nonfinite: bool,
}
impl Bounded {
    fn push(&mut self, command: OutlineCommand, points: &[f32]) {
        if self.over || self.nonfinite {
            return;
        }
        if points.iter().any(|v| !v.is_finite()) {
            self.nonfinite = true;
            return;
        }
        if self.commands.len() as u64 >= self.cap {
            self.over = true;
            return;
        }
        self.commands.push(command);
    }
}
impl ttf::OutlineBuilder for Bounded {
    fn move_to(&mut self, x: f32, y: f32) {
        self.push(OutlineCommand::MoveTo([x, y]), &[x, y]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(OutlineCommand::LineTo([x, y]), &[x, y]);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.push(OutlineCommand::QuadTo([x1, y1], [x, y]), &[x1, y1, x, y]);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.push(
            OutlineCommand::CurveTo([x1, y1], [x2, y2], [x, y]),
            &[x1, y1, x2, y2, x, y],
        );
    }
    fn close(&mut self) {
        self.push(OutlineCommand::Close, &[]);
    }
}

/// Extracts default-instance monochrome outlines. Verifies the content hash and face index like
/// shaping does, refuses CFF2 and colour/bitmap/SVG faces (`UnsupportedProfile`), refuses glyph ids
/// outside the face (`glyph_outline_unavailable`); an empty glyph (space) yields no commands.
pub fn glyph_outlines(
    font: &FontBytes<'_>,
    glyph_ids: &[u32],
    limits: &OutlineLimits,
    cancel: &CancellationToken,
) -> Result<Vec<GlyphOutline>, GlyphError> {
    cancel.check().map_err(|_| Error::Canceled)?;
    if limits.max_glyphs == 0
        || limits.max_segments == 0
        || limits.max_total_segments == 0
        || limits.max_work_units == 0
    {
        return Err(Error::InvalidInput.into());
    }
    if glyph_ids.len() as u64 > limits.max_glyphs {
        return Err(GlyphError::OutlineOverLimit);
    }
    if font.bytes.is_empty() || font.bytes.len() as u64 > limits.max_font_bytes {
        return Err(Error::Budget.into());
    }
    let epoch = FixedEpoch;
    let admission = NoAdmission;
    let ctx = Context::new(
        cancel,
        &epoch,
        0,
        &admission,
        limits.max_work_units,
        1,
        RECURSION,
    )?;
    if crate::validate::hash(font.bytes, &ctx)? != font.resource.content_hash {
        return Err(Error::HashMismatch.into());
    }
    let raw = ttf::RawFace::parse(font.bytes, font.resource.face_index).map_err(|error| {
        match error {
            ttf::FaceParsingError::FaceIndexOutOfBounds => Error::UnavailableFace,
            ttf::FaceParsingError::UnknownMagic => Error::UnsupportedFont,
            _ => Error::CorruptFont,
        }
    })?;
    if raw
        .table_bounded(ttf::Tag::from_bytes(b"CFF2"), &ctx)?
        .is_some()
    {
        return Err(Error::UnsupportedProfile.into());
    }
    let face = ttf::Face::parse_bounded(font.bytes, font.resource.face_index, &ctx)?;
    let units_per_em = face.units_per_em();
    let glyph_count = face.number_of_glyphs();
    let mut out = Vec::with_capacity(glyph_ids.len());
    let mut total: u64 = 0;
    for &glyph_id in glyph_ids {
        ctx.step()?;
        let id = u16::try_from(glyph_id)
            .ok()
            .filter(|id| *id < glyph_count)
            .ok_or(GlyphError::GlyphOutlineUnavailable)?;
        let remaining = limits
            .max_total_segments
            .checked_sub(total)
            .ok_or(GlyphError::OutlineOverLimit)?;
        let mut builder = Bounded {
            commands: Vec::new(),
            cap: limits.max_segments.min(remaining),
            over: false,
            nonfinite: false,
        };
        let rect = face.outline_glyph(ttf::GlyphId(id), &mut builder);
        if builder.over {
            return Err(GlyphError::OutlineOverLimit);
        }
        if builder.nonfinite {
            return Err(Error::CorruptFont.into());
        }
        let bounds = match rect {
            Some(r) => Some([r.x_min, r.y_min, r.x_max, r.y_max]),
            None if builder.commands.is_empty() => None,
            None => return Err(Error::CorruptFont.into()),
        };
        ctx.units(builder.commands.len() as u64)?;
        total = total
            .checked_add(builder.commands.len() as u64)
            .ok_or(Error::Overflow)?;
        out.push(GlyphOutline {
            glyph_id,
            units_per_em,
            font_hash: font.resource.content_hash,
            commands: builder.commands,
            bounds,
        });
    }
    ctx.poll()?;
    Ok(out)
}
