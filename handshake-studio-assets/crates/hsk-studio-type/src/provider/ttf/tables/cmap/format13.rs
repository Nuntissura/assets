// https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-13-many-to-one-range-mappings

use core::convert::TryFrom;

use super::format12::SequentialMapGroup;
use crate::provider::ttf::GlyphId;
use crate::provider::ttf::parser::{LazyArray32, Stream};

/// A [format 13](https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-13-segmented-coverage)
/// subtable.
#[derive(Clone, Copy)]
pub struct Subtable13<'a> {
    groups: LazyArray32<'a, SequentialMapGroup>,
}

impl<'a> Subtable13<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        s.skip::<u16>(); // format
        s.skip::<u16>(); // reserved
        s.skip::<u32>(); // length
        s.skip::<u32>(); // language
        let count = s.read::<u32>()?;
        let groups = s.read_array32::<super::format12::SequentialMapGroup>(count)?;
        Some(Self { groups })
    }

    /// Returns a glyph index for a code point.
    pub fn glyph_index(&self, code_point: u32) -> Option<GlyphId> {
        for group in self.groups {
            let start_char_code = group.start_char_code;
            if code_point >= start_char_code && code_point <= group.end_char_code {
                return u16::try_from(group.start_glyph_id).ok().map(GlyphId);
            }
        }

        None
    }

    /// Calls `f` for each codepoint defined in this table.
    pub fn codepoints(&self, mut f: impl FnMut(u32)) {
        for group in self.groups {
            for code_point in group.start_char_code..=group.end_char_code {
                f(code_point);
            }
        }
    }
}

impl core::fmt::Debug for Subtable13<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Subtable13 {{ ... }}")
    }
}

impl Subtable13<'_> {
    pub fn glyph_index_metered(
        &self,
        code_point: u32,
        scope: &crate::provider::ttf::bounded::Scope<'_, '_>,
    ) -> Option<GlyphId> {
        scope.step()?;
        for group in self.groups {
            scope.step()?;
            let start_char_code = group.start_char_code;
            if code_point >= start_char_code && code_point <= group.end_char_code {
                return u16::try_from(group.start_glyph_id).ok().map(GlyphId);
            }
        }

        None
    }
    pub(crate) fn glyph_index_bounded(
        &self,
        code_point: u32,
        ctx: &crate::provider::Context<'_>,
    ) -> crate::provider::ProviderResult<Option<GlyphId>> {
        crate::provider::ttf::bounded::run(ctx, |scope| self.glyph_index_metered(code_point, scope))
    }
}
