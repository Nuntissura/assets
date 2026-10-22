//! A [Horizontal Metrics Variations Table](
//! https://docs.microsoft.com/en-us/typography/opentype/spec/hvar) implementation.

use crate::provider::ttf::delta_set::DeltaSetIndexMap;
use crate::provider::ttf::parser::{Offset, Offset32, Stream};
use crate::provider::ttf::var_store::ItemVariationStore;
use crate::provider::ttf::{GlyphId, NormalizedCoordinate};

/// A [Horizontal Metrics Variations Table](
/// https://docs.microsoft.com/en-us/typography/opentype/spec/hvar).
#[derive(Clone, Copy)]
pub struct Table<'a> {
    data: &'a [u8],
    variation_store: ItemVariationStore<'a>,
    advance_width_mapping_offset: Option<Offset32>,
    lsb_mapping_offset: Option<Offset32>,
    rsb_mapping_offset: Option<Offset32>,
}

impl<'a> Table<'a> {
    /// Parses a table from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);

        let version = s.read::<u32>()?;
        if version != 0x00010000 {
            return None;
        }

        let variation_store_offset = s.read::<Offset32>()?;
        let var_store_s = Stream::new_at(data, variation_store_offset.to_usize())?;
        let variation_store = ItemVariationStore::parse(var_store_s)?;

        Some(Table {
            data,
            variation_store,
            advance_width_mapping_offset: s.read::<Option<Offset32>>()?,
            lsb_mapping_offset: s.read::<Option<Offset32>>()?,
            rsb_mapping_offset: s.read::<Option<Offset32>>()?,
        })
    }

    /// Returns the advance width offset for a glyph.
    #[inline]
    pub fn advance_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let (outer_idx, inner_idx) = if let Some(offset) = self.advance_width_mapping_offset {
            DeltaSetIndexMap::new(self.data.get(offset.to_usize()..)?).map(glyph_id.0 as u32)?
        } else {
            // 'If there is no delta-set index mapping table for advance widths,
            // then glyph IDs implicitly provide the indices:
            // for a given glyph ID, the delta-set outer-level index is zero,
            // and the glyph ID is the delta-set inner-level index.'
            (0, glyph_id.0)
        };

        self.variation_store
            .parse_delta(outer_idx, inner_idx, coordinates)
    }

    /// Returns the left side bearing offset for a glyph.
    #[inline]
    pub fn left_side_bearing_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let set_data = self.data.get(self.lsb_mapping_offset?.to_usize()..)?;
        self.side_bearing_offset(glyph_id, coordinates, set_data)
    }

    /// Returns the right side bearing offset for a glyph.
    #[inline]
    pub fn right_side_bearing_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let set_data = self.data.get(self.rsb_mapping_offset?.to_usize()..)?;
        self.side_bearing_offset(glyph_id, coordinates, set_data)
    }

    fn side_bearing_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
        set_data: &[u8],
    ) -> Option<f32> {
        let (outer_idx, inner_idx) = DeltaSetIndexMap::new(set_data).map(glyph_id.0 as u32)?;
        self.variation_store
            .parse_delta(outer_idx, inner_idx, coordinates)
    }
}

impl core::fmt::Debug for Table<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Table {{ ... }}")
    }
}

impl<'a> Table<'a> {
    pub(crate) fn parse_bounded(
        data: &'a [u8],
        ctx: &crate::provider::Context<'_>,
    ) -> crate::provider::ProviderResult<Option<Self>> {
        ctx.step()?;
        // Header/store parsing creates borrowed slices only; no record traversal.
        Ok(Self::parse(data))
    }
    pub(crate) fn advance_offset_bounded(
        &self,
        glyph: GlyphId,
        coordinates: &[NormalizedCoordinate],
        ctx: &crate::provider::Context<'_>,
    ) -> crate::provider::ProviderResult<Option<f32>> {
        ctx.step()?;
        let pair = if let Some(offset) = self.advance_width_mapping_offset {
            let Some(data) = self.data.get(offset.to_usize()..) else {
                return Ok(None);
            };
            DeltaSetIndexMap::new(data).map_bounded(u32::from(glyph.0), ctx)?
        } else {
            Some((0, glyph.0))
        };
        let Some((outer, inner)) = pair else {
            return Ok(None);
        };
        self.variation_store
            .parse_delta_bounded(outer, inner, coordinates, ctx)
    }
    pub(crate) fn left_side_bearing_offset_bounded(
        &self,
        glyph: GlyphId,
        coordinates: &[NormalizedCoordinate],
        ctx: &crate::provider::Context<'_>,
    ) -> crate::provider::ProviderResult<Option<f32>> {
        ctx.step()?;
        let Some(offset) = self.lsb_mapping_offset else {
            return Ok(None);
        };
        let Some(data) = self.data.get(offset.to_usize()..) else {
            return Ok(None);
        };
        let Some((outer, inner)) =
            DeltaSetIndexMap::new(data).map_bounded(u32::from(glyph.0), ctx)?
        else {
            return Ok(None);
        };
        self.variation_store
            .parse_delta_bounded(outer, inner, coordinates, ctx)
    }
}
