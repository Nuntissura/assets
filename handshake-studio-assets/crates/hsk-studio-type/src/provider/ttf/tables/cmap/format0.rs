use crate::provider::ttf::GlyphId;
use crate::provider::ttf::parser::{NumFrom, Stream};

/// A [format 0](https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-0-byte-encoding-table)
/// subtable.
#[derive(Clone, Copy, Debug)]
pub struct Subtable0<'a> {
    /// Just a list of 256 8bit glyph IDs.
    pub glyph_ids: &'a [u8],
}

impl<'a> Subtable0<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        s.skip::<u16>(); // format
        s.skip::<u16>(); // length
        s.skip::<u16>(); // language
        let glyph_ids = s.read_bytes(256)?;
        Some(Self { glyph_ids })
    }

    /// Returns a glyph index for a code point.
    pub fn glyph_index(&self, code_point: u32) -> Option<GlyphId> {
        let glyph_id = *self.glyph_ids.get(usize::num_from(code_point))?;
        // Make sure that the glyph is not zero, the array always has 256 ids,
        // but some codepoints may be mapped to zero.
        if glyph_id != 0 {
            Some(GlyphId(u16::from(glyph_id)))
        } else {
            None
        }
    }

    /// Calls `f` for each codepoint defined in this table.
    pub fn codepoints(&self, mut f: impl FnMut(u32)) {
        for (i, glyph_id) in self.glyph_ids.iter().enumerate() {
            // In contrast to every other format, here we take a look at the glyph
            // id and check whether it is zero because otherwise this method would
            // always simply call `f` for `0..256` which would be kind of pointless
            // (this array always has length 256 even when the face has fewer glyphs).
            if *glyph_id != 0 {
                f(i as u32);
            }
        }
    }
}

impl Subtable0<'_> {
    pub fn glyph_index_metered(
        &self,
        code_point: u32,
        scope: &crate::provider::ttf::bounded::Scope<'_, '_>,
    ) -> Option<GlyphId> {
        scope.step()?;
        let glyph_id = *self.glyph_ids.get(usize::num_from(code_point))?;
        // Make sure that the glyph is not zero, the array always has 256 ids,
        // but some codepoints may be mapped to zero.
        if glyph_id != 0 {
            Some(GlyphId(u16::from(glyph_id)))
        } else {
            None
        }
    }
    pub(crate) fn glyph_index_bounded(
        &self,
        code_point: u32,
        ctx: &crate::provider::Context<'_>,
    ) -> crate::provider::ProviderResult<Option<GlyphId>> {
        crate::provider::ttf::bounded::run(ctx, |scope| self.glyph_index_metered(code_point, scope))
    }
}
