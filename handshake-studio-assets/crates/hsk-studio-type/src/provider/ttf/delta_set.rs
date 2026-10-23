use core::convert::TryFrom;

use crate::provider::ttf::parser::Stream;

#[derive(Clone, Copy, Debug)]
pub(crate) struct DeltaSetIndexMap<'a> {
    data: &'a [u8],
}

impl<'a> DeltaSetIndexMap<'a> {
    #[inline]
    pub(crate) fn new(data: &'a [u8]) -> Self {
        DeltaSetIndexMap { data }
    }

    #[inline]
    pub(crate) fn map(&self, mut index: u32) -> Option<(u16, u16)> {
        let mut s = Stream::new(self.data);
        let format = s.read::<u8>()?;
        let entry_format = s.read::<u8>()?;
        let map_count = if format == 0 {
            s.read::<u16>()? as u32
        } else {
            s.read::<u32>()?
        };

        if map_count == 0 {
            return None;
        }

        // 'If a given glyph ID is greater than mapCount-1, then the last entry is used.'
        if index >= map_count {
            index = map_count - 1;
        }

        let entry_size = ((entry_format >> 4) & 3) + 1;
        let inner_index_bit_count = u32::from((entry_format & 0xF) + 1);

        s.advance(usize::from(entry_size) * usize::try_from(index).ok()?);

        let mut n = 0u32;
        for b in s.read_bytes(usize::from(entry_size))? {
            n = (n << 8) + u32::from(*b);
        }

        let outer_index = n >> inner_index_bit_count;
        let inner_index = n & ((1 << inner_index_bit_count) - 1);
        Some((
            u16::try_from(outer_index).ok()?,
            u16::try_from(inner_index).ok()?,
        ))
    }
}

impl DeltaSetIndexMap<'_> {
    pub(crate) fn map_bounded(
        &self,
        mut index: u32,
        ctx: &crate::provider::Context<'_>,
    ) -> crate::provider::ProviderResult<Option<(u16, u16)>> {
        ctx.step()?;
        let Some(mut s) = Stream::new_at(self.data, 0) else {
            return Ok(None);
        };
        let Some(format) = s.read::<u8>() else {
            return Ok(None);
        };
        let Some(entry) = s.read::<u8>() else {
            return Ok(None);
        };
        let count = if format == 0 {
            s.read::<u16>().map(u32::from)
        } else if format == 1 {
            s.read::<u32>()
        } else {
            return Ok(None);
        };
        let Some(count) = count else {
            return Ok(None);
        };
        if count == 0 {
            return Ok(None);
        }
        if index >= count {
            index = count - 1;
        }
        let size = usize::from(((entry >> 4) & 3) + 1);
        let bits = u32::from((entry & 15) + 1);
        let offset = usize::try_from(index)
            .ok()
            .and_then(|n| n.checked_mul(size))
            .ok_or(crate::Error::Overflow)?;
        if s.advance_checked(offset).is_none() {
            return Ok(None);
        }
        let Some(bytes) = s.read_bytes(size) else {
            return Ok(None);
        };
        let mut n = 0u32;
        for byte in bytes {
            ctx.step()?;
            n = (n << 8) | u32::from(*byte);
        }
        let Ok(outer) = u16::try_from(n >> bits) else {
            return Ok(None);
        };
        let Ok(inner) = u16::try_from(n & ((1u32 << bits) - 1)) else {
            return Ok(None);
        };
        Ok(Some((outer, inner)))
    }
}
