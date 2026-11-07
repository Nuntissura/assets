//! Text behind a port (decision O4): this crate defines [`TextPort`]; shaping and rasterisation live in the host's
//! text engine (in Studio: `hsk-studio-type` through the adapter crate `hsk-studio-ui-text`), so `kumihimo-ui` never
//! depends on a Studio crate. [`Text`] wraps a port with the shaped-line cache and the glyph cache.
//!
//! Units (design-system.json scale_rules.text): shape at the logical size, rasterise at `size * s` physical px, baseline
//! snapped to a physical pixel, quarter-pixel horizontal bins, grey coverage. Tracking from the style is added here, not
//! in the port, so every port tracks the same way.
use crate::tokens::{TextStyle, TextToken};
use std::collections::HashMap;

/// A font face the port knows, chosen by the port (opaque here).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceId(pub u32);

/// One shaped glyph; x positions are logical px from the line's pen start, offsets included.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub face: FaceId,
    pub glyph_id: u32,
    /// Pen x of this glyph, logical px.
    pub x: f32,
    /// Vertical offset, logical px, positive up.
    pub y: f32,
    pub advance: f32,
    /// Byte range of the source text this glyph shows.
    pub cluster: (u32, u32),
}

/// One shaped line in logical px.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ShapedLine {
    pub glyphs: Vec<ShapedGlyph>,
    /// Advance width, logical px.
    pub width: f32,
    /// Cap height, logical px (used to centre a line in its box).
    pub cap_height: f32,
}

/// One glyph raster request: physical px size in 1/64 px, horizontal quarter-pixel bin 0..=3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: FaceId,
    pub glyph_id: u32,
    pub px_size_64: u32,
    pub subpx_x: u8,
}

/// A coverage mask: 0..=255, row-major, stride = width. `left`/`top` place the mask relative to the pen point on the
/// baseline (top is negative above the baseline).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GlyphMask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextError {
    /// The port has no face for the style (typed refusal, never a silent fallback).
    NoFace,
    /// The text engine refused the input; code is the engine's stable code.
    Refused(&'static str),
    /// Over a bound of the port or the cache.
    OverLimit,
}

/// What a host text engine provides. Implementations must be deterministic for equal inputs.
pub trait TextPort {
    /// Shapes `text` as one line at the logical size of `style` (tracking is applied by [`Text`]).
    fn shape(&mut self, text: &str, style: &TextStyle) -> Result<ShapedLine, TextError>;
    /// Rasterises one glyph at `key.px_size_64 / 64` physical px with `key.subpx_x / 4` px horizontal offset.
    fn rasterize(&mut self, key: GlyphKey) -> Result<GlyphMask, TextError>;
}

/// Counters of the two caches, readable by diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStats {
    pub shape_hits: u64,
    pub shape_misses: u64,
    pub mask_hits: u64,
    pub mask_misses: u64,
    /// Whole-cache resets on reaching capacity.
    pub resets: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    token: TextToken,
    weight: u16,
}

/// Longest string one label shapes, in bytes.
pub const MAX_LABEL_BYTES: usize = 4096;

/// The text system: a port plus the shaped-line and glyph caches. One per window.
pub struct Text {
    port: Box<dyn TextPort>,
    shaped: HashMap<ShapeKey, ShapedLine>,
    masks: HashMap<GlyphKey, GlyphMask>,
    max_lines: usize,
    max_masks: usize,
    lut: [u8; 256],
    gamma: f32,
    stats: TextStats,
}

impl Text {
    pub fn new(port: Box<dyn TextPort>) -> Self {
        let mut t = Self {
            port,
            shaped: HashMap::new(),
            masks: HashMap::new(),
            max_lines: 4096,
            max_masks: 8192,
            lut: [0; 256],
            gamma: 1.0,
            stats: TextStats::default(),
        };
        t.set_coverage_gamma(1.0);
        t
    }
    /// Capacity of each cache; reaching it resets that cache whole (atlas page reset semantics).
    pub fn with_capacity(mut self, lines: usize, masks: usize) -> Self {
        self.max_lines = lines.max(1);
        self.max_masks = masks.max(1);
        self
    }
    /// Coverage contrast: `cov' = cov ^ (1 / gamma)`. 1.0 is identity; above 1 darkens thin stems. Clamped to [0.5, 3].
    pub fn set_coverage_gamma(&mut self, gamma: f32) {
        let g = if gamma.is_finite() { gamma.clamp(0.5, 3.0) } else { 1.0 };
        self.gamma = g;
        for (i, v) in self.lut.iter_mut().enumerate() {
            *v = ((i as f32 / 255.0).powf(1.0 / g) * 255.0).round() as u8;
        }
    }
    pub fn coverage_gamma(&self) -> f32 {
        self.gamma
    }
    pub fn lut(&self) -> &[u8; 256] {
        &self.lut
    }
    pub fn stats(&self) -> TextStats {
        self.stats
    }
    /// Style of a token with an optional weight override (0 = the token's own weight).
    pub fn style(token: TextToken, weight: u16) -> TextStyle {
        let mut s = token.style();
        if weight != 0 {
            s.weight = weight;
        }
        s
    }
    /// Shaped line, cached by (text, token, weight); tracking applied.
    pub fn shape(&mut self, text: &str, token: TextToken, weight: u16) -> Result<ShapedLine, TextError> {
        if text.len() > MAX_LABEL_BYTES {
            return Err(TextError::OverLimit);
        }
        let key = ShapeKey { text: text.to_owned(), token, weight };
        if let Some(line) = self.shaped.get(&key) {
            self.stats.shape_hits = self.stats.shape_hits.saturating_add(1);
            return Ok(line.clone());
        }
        self.stats.shape_misses = self.stats.shape_misses.saturating_add(1);
        let style = Self::style(token, weight);
        let mut line = self.port.shape(text, &style)?;
        if style.tracking != 0.0 {
            for (i, g) in line.glyphs.iter_mut().enumerate() {
                g.x += style.tracking * i as f32;
            }
            line.width += style.tracking * line.glyphs.len() as f32;
        }
        if self.shaped.len() >= self.max_lines {
            self.shaped.clear();
            self.stats.resets = self.stats.resets.saturating_add(1);
        }
        self.shaped.insert(key, line.clone());
        Ok(line)
    }
    /// Advance width in logical px; 0 for a refused string (the refusal surfaces when painting).
    pub fn measure(&mut self, text: &str, token: TextToken, weight: u16) -> f32 {
        self.shape(text, token, weight).map(|l| l.width).unwrap_or(0.0)
    }
    /// Glyph coverage, cached by [`GlyphKey`].
    pub fn mask(&mut self, key: GlyphKey) -> Result<&GlyphMask, TextError> {
        if self.masks.contains_key(&key) {
            self.stats.mask_hits = self.stats.mask_hits.saturating_add(1);
        } else {
            self.stats.mask_misses = self.stats.mask_misses.saturating_add(1);
            let mask = self.port.rasterize(key)?;
            let len = (mask.width as usize).checked_mul(mask.height as usize).ok_or(TextError::OverLimit)?;
            if mask.data.len() != len {
                return Err(TextError::Refused("mask_shape"));
            }
            if self.masks.len() >= self.max_masks {
                self.masks.clear();
                self.stats.resets = self.stats.resets.saturating_add(1);
            }
            self.masks.insert(key, mask);
        }
        self.masks.get(&key).ok_or(TextError::OverLimit)
    }
    /// Drops every cached raster (on a scale change, scale_rules.monitor_change).
    pub fn clear_masks(&mut self) {
        self.masks.clear();
    }
    /// The longest prefix of `text` (whole chars) that fits `max_w` logical px with a trailing ellipsis, or the whole
    /// text when it fits.
    pub fn ellipsize(&mut self, text: &str, token: TextToken, weight: u16, max_w: f32) -> String {
        if self.measure(text, token, weight) <= max_w {
            return text.to_owned();
        }
        let ell = "\u{2026}";
        let mut cut: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        while let Some(end) = cut.pop() {
            let cand = format!("{}{ell}", &text[..end]);
            if self.measure(&cand, token, weight) <= max_w {
                return cand;
            }
        }
        ell.to_owned()
    }
}

/// A deterministic stand-in engine with no font: every scalar is a box of the style's cap height, advance
/// `0.56 * size` (digits `0.6 * size`, space `0.3 * size`). For layout tests and hosts with no text engine; never the
/// product look.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoxText;
impl TextPort for BoxText {
    fn shape(&mut self, text: &str, style: &TextStyle) -> Result<ShapedLine, TextError> {
        let mut x = 0.0f32;
        let mut glyphs = Vec::new();
        for (i, c) in text.char_indices() {
            let adv = if c == ' ' {
                0.3
            } else if c.is_ascii_digit() {
                0.6
            } else {
                0.56
            } * style.size;
            let end = i + c.len_utf8();
            glyphs.push(ShapedGlyph { face: FaceId(0), glyph_id: u32::from(c), x, y: 0.0, advance: adv, cluster: (i as u32, end as u32) });
            x += adv;
        }
        Ok(ShapedLine { glyphs, width: x, cap_height: (style.size * 0.7).round() })
    }
    fn rasterize(&mut self, key: GlyphKey) -> Result<GlyphMask, TextError> {
        if key.glyph_id == u32::from(' ') {
            return Ok(GlyphMask::default());
        }
        let size = key.px_size_64 as f32 / 64.0;
        let w = ((size * 0.56) - 2.0).max(1.0).round() as u32;
        let h = (size * 0.7).round().max(1.0) as u32;
        let len = (w as usize).checked_mul(h as usize).ok_or(TextError::OverLimit)?;
        let mut data = vec![255u8; len];
        // Quarter-pixel bins change the left column's coverage so bins are distinguishable.
        let left_cov = 255u8.saturating_sub(key.subpx_x.saturating_mul(60));
        for row in 0..h as usize {
            data[row * w as usize] = left_cov;
        }
        Ok(GlyphMask { left: 1, top: -(h as i32), width: w, height: h, data })
    }
}
