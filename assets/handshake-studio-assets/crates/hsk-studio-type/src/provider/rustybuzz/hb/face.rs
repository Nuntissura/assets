use crate::Error;
use crate::provider::{Context, ProviderResult};

use crate::provider::ttf::GlyphId;
use crate::provider::ttf::gdef::GlyphClass;
use crate::provider::ttf::opentype_layout::LayoutTable;

use super::buffer::GlyphPropsFlags;
use super::ot_layout::TableIndex;
use super::ot_layout_common::{PositioningTable, SubstitutionTable};
use crate::provider::rustybuzz::Variation;

// https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#windows-platform-platform-id--3
const WINDOWS_SYMBOL_ENCODING: u16 = 0;
const WINDOWS_UNICODE_BMP_ENCODING: u16 = 1;
const WINDOWS_UNICODE_FULL_ENCODING: u16 = 10;

// https://docs.microsoft.com/en-us/typography/opentype/spec/name#platform-specific-encoding-and-language-ids-unicode-platform-platform-id--0
const UNICODE_1_0_ENCODING: u16 = 0;
const UNICODE_1_1_ENCODING: u16 = 1;
const UNICODE_ISO_ENCODING: u16 = 2;
const UNICODE_2_0_BMP_ENCODING: u16 = 3;
const UNICODE_2_0_FULL_ENCODING: u16 = 4;
//const UNICODE_VARIATION_ENCODING: u16 = 5;
const UNICODE_FULL_ENCODING: u16 = 6;

/// A font face handle.
pub struct hb_font_t<'a> {
    pub(crate) ttfp_face: crate::provider::ttf::Face<'a>,
    pub(crate) units_per_em: u16,
    pixels_per_em: Option<(u16, u16)>,
    pub(crate) points_per_em: Option<f32>,
    prefered_cmap_encoding_subtable: Option<u16>,
    pub(crate) gsub: Option<SubstitutionTable<'a>>,
    pub(crate) gpos: Option<PositioningTable<'a>>,
}

impl<'a> AsRef<crate::provider::ttf::Face<'a>> for hb_font_t<'a> {
    #[inline]
    fn as_ref(&self) -> &crate::provider::ttf::Face<'a> {
        &self.ttfp_face
    }
}

impl<'a> AsMut<crate::provider::ttf::Face<'a>> for hb_font_t<'a> {
    #[inline]
    fn as_mut(&mut self) -> &mut crate::provider::ttf::Face<'a> {
        &mut self.ttfp_face
    }
}

impl<'a> core::ops::Deref for hb_font_t<'a> {
    type Target = crate::provider::ttf::Face<'a>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.ttfp_face
    }
}

impl<'a> core::ops::DerefMut for hb_font_t<'a> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ttfp_face
    }
}

impl<'a> hb_font_t<'a> {
    /// Creates a new `Face` from data.
    ///
    /// Data will be referenced, not owned.
    pub fn from_slice(
        data: &'a [u8],
        face_index: u32,
        meter: &Context<'a>,
    ) -> ProviderResult<Self> {
        meter.step()?;
        let face = crate::provider::ttf::Face::parse_bounded(data, face_index, meter)?;
        Self::from_face(face, meter)
    }

    /// Creates a new [`Face`] from [`crate::provider::ttf::Face`].
    ///
    /// Data will be referenced, not owned.
    pub fn from_face(
        face: crate::provider::ttf::Face<'a>,
        meter: &Context<'a>,
    ) -> ProviderResult<Self> {
        meter.step()?;
        Ok(hb_font_t {
            units_per_em: face.units_per_em(),
            pixels_per_em: None,
            points_per_em: None,
            prefered_cmap_encoding_subtable: find_best_cmap_subtable(&face, meter)?,
            gsub: match face.tables().gsub {
                Some(table) => Some(SubstitutionTable::new(table, meter)?),
                None => None,
            },
            gpos: match face.tables().gpos {
                Some(table) => Some(PositioningTable::new(table, meter)?),
                None => None,
            },
            ttfp_face: face,
        })
    }

    // TODO: remove
    /// Returns face’s units per EM.
    #[inline]
    pub fn units_per_em(&self) -> i32 {
        self.units_per_em as i32
    }

    #[inline]
    pub(crate) fn pixels_per_em(&self) -> Option<(u16, u16)> {
        self.pixels_per_em
    }

    /// Sets pixels per EM.
    ///
    /// Used during raster glyphs processing and hinting.
    ///
    /// `None` by default.
    #[inline]
    pub fn set_pixels_per_em(&mut self, ppem: Option<(u16, u16)>) {
        self.pixels_per_em = ppem;
    }

    /// Sets point size per EM.
    ///
    /// Used for optical-sizing in Apple fonts.
    ///
    /// `None` by default.
    #[inline]
    pub fn set_points_per_em(&mut self, ptem: Option<f32>) {
        self.points_per_em = ptem;
    }

    /// Sets font variations.
    pub fn set_variations(
        &mut self,
        variations: &[Variation],
        meter: &Context<'_>,
    ) -> ProviderResult<()> {
        for variation in variations {
            meter.step()?;
            if !variation.value.is_finite() {
                return Err(Error::InvalidAxis);
            }
            self.ttfp_face
                .set_variation_bounded(variation.tag, variation.value, meter)?
                .ok_or(Error::InvalidAxis)?;
        }
        Ok(())
    }

    pub(crate) fn has_glyph(&self, c: u32, meter: &Context<'_>) -> ProviderResult<bool> {
        Ok(self.get_nominal_glyph(c, meter)?.is_some())
    }

    pub(crate) fn get_nominal_glyph(
        &self,
        c: u32,
        meter: &Context<'_>,
    ) -> ProviderResult<Option<GlyphId>> {
        meter.step()?;
        let Some(index) = self.prefered_cmap_encoding_subtable else {
            return Ok(None);
        };
        let Some(cmap) = self.tables().cmap else {
            return Ok(None);
        };
        let Some(subtable) = cmap.subtables.get_bounded(index, meter)? else {
            return Err(Error::CorruptFont);
        };
        let c = if subtable.platform_id == crate::provider::ttf::PlatformId::Macintosh && c > 0x7f {
            let mut mapped = 0;
            for (index, value) in UNICODE_TO_MACROMAN.iter().enumerate() {
                meter.step()?;
                if u32::from(*value) == c {
                    mapped = u32::try_from(index + 0x80).map_err(|_| Error::Overflow)?;
                    break;
                }
            }
            mapped
        } else {
            c
        };
        let glyph = subtable.glyph_index_bounded(c, meter)?;
        if glyph.is_some() {
            return Ok(glyph);
        }
        if subtable.platform_id == crate::provider::ttf::PlatformId::Windows
            && subtable.encoding_id == WINDOWS_SYMBOL_ENCODING
            && c <= 0xff
        {
            // One explicitly bounded compatibility lookup, not recursive fallback.
            return subtable.glyph_index_bounded(0xf000 + c, meter);
        }
        Ok(None)
    }

    pub(crate) fn glyph_h_advance(
        &self,
        glyph: GlyphId,
        meter: &Context<'_>,
    ) -> ProviderResult<i32> {
        meter.step()?;
        Ok(i32::from(
            self.ttfp_face
                .glyph_hor_advance_bounded(glyph, meter)?
                .ok_or(Error::CorruptFont)?,
        ))
    }

    pub(crate) fn glyph_props(&self, glyph: GlyphId, meter: &Context<'_>) -> ProviderResult<u16> {
        meter.step()?;
        let table = match self.tables().gdef {
            Some(v) => v,
            None => return Ok(0),
        };

        Ok(match table.glyph_class_bounded(glyph, meter)? {
            Some(GlyphClass::Base) => GlyphPropsFlags::BASE_GLYPH.bits(),
            Some(GlyphClass::Ligature) => GlyphPropsFlags::LIGATURE.bits(),
            Some(GlyphClass::Mark) => {
                let class = table.glyph_mark_attachment_class_bounded(glyph, meter)?;
                (class << 8) | GlyphPropsFlags::MARK.bits()
            }
            _ => 0,
        })
    }

    pub(crate) fn layout_table(&self, table_index: TableIndex) -> Option<&LayoutTable<'a>> {
        match table_index {
            TableIndex::GSUB => self.gsub.as_ref().map(|table| &table.inner),
            TableIndex::GPOS => self.gpos.as_ref().map(|table| &table.inner),
        }
    }

    pub(crate) fn layout_tables(
        &self,
    ) -> impl Iterator<Item = (TableIndex, &LayoutTable<'a>)> + '_ {
        TableIndex::iter().filter_map(move |idx| self.layout_table(idx).map(|table| (idx, table)))
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct hb_glyph_extents_t {
    pub x_bearing: i32,
    pub y_bearing: i32,
    pub width: i32,
    pub height: i32,
}

unsafe impl bytemuck::Zeroable for hb_glyph_extents_t {}
unsafe impl bytemuck::Pod for hb_glyph_extents_t {}

fn find_best_cmap_subtable(
    face: &crate::provider::ttf::Face,
    meter: &Context<'_>,
) -> ProviderResult<Option<u16>> {
    use crate::provider::ttf::PlatformId;
    let choices = [
        (PlatformId::Windows, WINDOWS_SYMBOL_ENCODING),
        (PlatformId::Windows, WINDOWS_UNICODE_FULL_ENCODING),
        (PlatformId::Unicode, UNICODE_FULL_ENCODING),
        (PlatformId::Unicode, UNICODE_2_0_FULL_ENCODING),
        (PlatformId::Windows, WINDOWS_UNICODE_BMP_ENCODING),
        (PlatformId::Unicode, UNICODE_2_0_BMP_ENCODING),
        (PlatformId::Unicode, UNICODE_ISO_ENCODING),
        (PlatformId::Unicode, UNICODE_1_1_ENCODING),
        (PlatformId::Unicode, UNICODE_1_0_ENCODING),
        (PlatformId::Macintosh, 0),
    ];
    let Some(cmap) = face.tables().cmap else {
        return Ok(None);
    };
    for (platform, encoding) in choices {
        meter.step()?;
        for index in 0..cmap.subtables.len() {
            meter.step()?;
            let table = cmap
                .subtables
                .get_bounded(index, meter)?
                .ok_or(Error::CorruptFont)?;
            if table.platform_id == platform && table.encoding_id == encoding {
                return Ok(Some(index));
            }
        }
    }
    Ok(None)
}

#[rustfmt::skip]
static UNICODE_TO_MACROMAN: &[u16] = &[
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1,
    0x00E0, 0x00E2, 0x00E4, 0x00E3, 0x00E5, 0x00E7, 0x00E9, 0x00E8,
    0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC,
    0x2020, 0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF,
    0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211,
    0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8,
    0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153,
    0x2013, 0x2014, 0x201C, 0x201D, 0x2018, 0x2019, 0x00F7, 0x25CA,
    0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02,
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1,
    0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC, 0x00D3, 0x00D4,
    0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC,
    0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];

fn unicode_to_macroman(c: u32) -> u32 {
    let u = c as u16;
    let Some(index) = UNICODE_TO_MACROMAN.iter().position(|m| *m == u) else {
        return 0;
    };
    (0x80 + index) as u32
}
