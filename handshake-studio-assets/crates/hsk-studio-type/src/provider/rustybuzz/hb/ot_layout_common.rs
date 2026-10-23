use crate::Error;
use crate::provider::rustybuzz::hb::set_digest::{hb_set_digest_ext, hb_set_digest_t};
use crate::provider::storage::ChargedVec;
use crate::provider::ttf::gpos::PositioningSubtable;
use crate::provider::ttf::gsub::SubstitutionSubtable;
use crate::provider::ttf::opentype_layout::{Coverage, Lookup};
use crate::provider::{Context, ProviderResult};

#[allow(dead_code)]
pub mod lookup_flags {
    pub const RIGHT_TO_LEFT: u16 = 0x0001;
    pub const IGNORE_BASE_GLYPHS: u16 = 0x0002;
    pub const IGNORE_LIGATURES: u16 = 0x0004;
    pub const IGNORE_MARKS: u16 = 0x0008;
    pub const IGNORE_FLAGS: u16 = 0x000E;
    pub const USE_MARK_FILTERING_SET: u16 = 0x0010;
    pub const MARK_ATTACHMENT_TYPE_MASK: u16 = 0xFF00;
}

pub struct PositioningTable<'a> {
    pub inner: crate::provider::ttf::opentype_layout::LayoutTable<'a>,
    pub lookups: ChargedVec<'a, PositioningLookup<'a>>,
}

impl<'a> PositioningTable<'a> {
    pub fn new(
        inner: crate::provider::ttf::opentype_layout::LayoutTable<'a>,
        meter: &Context<'a>,
    ) -> ProviderResult<Self> {
        meter.check_lookups(u64::from(inner.lookups.len()))?;
        let mut lookups = ChargedVec::with_capacity(usize::from(inner.lookups.len()), meter)?;
        for i in 0..inner.lookups.len() {
            meter.step()?;
            let lookup = inner
                .lookups
                .get_bounded(i, meter)?
                .ok_or(Error::CorruptFont)?;
            lookups.push(PositioningLookup::parse(lookup, meter)?, meter)?;
        }
        Ok(Self { inner, lookups })
    }
}

pub trait CoverageExt {
    fn collect(&self, set_digest: &mut hb_set_digest_t, meter: &Context<'_>) -> ProviderResult<()>;
}

impl CoverageExt for Coverage<'_> {
    fn collect(&self, set_digest: &mut hb_set_digest_t, meter: &Context<'_>) -> ProviderResult<()> {
        match *self {
            Self::Format1 { glyphs } => {
                for i in 0..glyphs.len() {
                    meter.step()?;
                    set_digest.add(glyphs.get_bounded(i, meter)?.ok_or(Error::CorruptFont)?);
                }
            }
            Self::Format2 { records } => {
                for i in 0..records.len() {
                    meter.step()?;
                    let record = records.get_bounded(i, meter)?.ok_or(Error::CorruptFont)?;
                    if record.start > record.end {
                        return Err(Error::CorruptFont);
                    }
                    set_digest.add_range(record.start, record.end);
                }
            }
        }
        Ok(())
    }
}

pub struct PositioningLookup<'a> {
    pub subtables: ChargedVec<'a, PositioningSubtable<'a>>,
    pub set_digest: hb_set_digest_t,
    pub props: u32,
}

impl<'a> PositioningLookup<'a> {
    pub fn parse(lookup: Lookup<'a>, meter: &Context<'a>) -> ProviderResult<Self> {
        meter.check_lookups(u64::from(lookup.subtables.len()))?;
        let mut subtables = ChargedVec::with_capacity(usize::from(lookup.subtables.len()), meter)?;
        let mut set_digest = hb_set_digest_t::new();

        for i in 0..lookup.subtables.len() {
            meter.step()?;
            let subtable = lookup
                .subtables
                .get_bounded::<PositioningSubtable>(i, meter)?
                .ok_or(Error::CorruptFont)?;
            subtable.coverage().collect(&mut set_digest, meter)?;

            subtables.push(subtable, meter)?;
        }
        Ok(Self {
            subtables,
            set_digest,
            props: lookup_props(lookup),
        })
    }
}

pub struct SubstitutionTable<'a> {
    pub inner: crate::provider::ttf::opentype_layout::LayoutTable<'a>,
    pub lookups: ChargedVec<'a, SubstLookup<'a>>,
}

impl<'a> SubstitutionTable<'a> {
    pub fn new(
        inner: crate::provider::ttf::opentype_layout::LayoutTable<'a>,
        meter: &Context<'a>,
    ) -> ProviderResult<Self> {
        meter.check_lookups(u64::from(inner.lookups.len()))?;
        let mut lookups = ChargedVec::with_capacity(usize::from(inner.lookups.len()), meter)?;
        for i in 0..inner.lookups.len() {
            meter.step()?;
            let lookup = inner
                .lookups
                .get_bounded(i, meter)?
                .ok_or(Error::CorruptFont)?;
            lookups.push(SubstLookup::parse(lookup, meter)?, meter)?;
        }
        Ok(Self { inner, lookups })
    }
}

pub struct SubstLookup<'a> {
    pub subtables: ChargedVec<'a, SubstitutionSubtable<'a>>,
    pub set_digest: hb_set_digest_t,
    pub reverse: bool,
    pub props: u32,
}

impl<'a> SubstLookup<'a> {
    pub fn parse(lookup: Lookup<'a>, meter: &Context<'a>) -> ProviderResult<Self> {
        meter.check_lookups(u64::from(lookup.subtables.len()))?;
        let mut subtables = ChargedVec::with_capacity(usize::from(lookup.subtables.len()), meter)?;
        let mut set_digest = hb_set_digest_t::new();
        let mut reverse = lookup.subtables.len() != 0;
        for i in 0..lookup.subtables.len() {
            meter.step()?;
            let subtable = lookup
                .subtables
                .get_bounded::<SubstitutionSubtable>(i, meter)?
                .ok_or(Error::CorruptFont)?;
            subtable.coverage().collect(&mut set_digest, meter)?;
            reverse &= subtable.is_reverse();
            subtables.push(subtable, meter)?;
        }
        Ok(Self {
            subtables,
            set_digest,
            reverse,
            props: lookup_props(lookup),
        })
    }
}

// lookup_props is a 32-bit integer where the lower 16-bit is LookupFlag and
// higher 16-bit is mark-filtering-set if the lookup uses one.
// Not to be confused with glyph_props which is very similar. */
fn lookup_props(lookup: Lookup) -> u32 {
    let mut props = u32::from(lookup.flags.0);
    if let Some(set) = lookup.mark_filtering_set {
        props |= u32::from(set) << 16;
    }
    props
}
