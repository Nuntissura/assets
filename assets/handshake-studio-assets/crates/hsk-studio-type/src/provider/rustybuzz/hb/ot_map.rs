use crate::Error;
use crate::provider::storage::ChargedVec;
use crate::provider::ttf::FromData;
use crate::provider::{Context, ProviderResult};
use core::cmp::Ordering;
use core::ops::Range;

use crate::provider::ttf::opentype_layout::{
    FeatureIndex, LanguageIndex, LookupIndex, ScriptIndex, VariationIndex,
};

use super::buffer::{glyph_flag, hb_buffer_t};
use super::ot_layout::{LayoutTableExt, TableIndex};
use super::ot_shape_plan::hb_ot_shape_plan_t;
use super::{Language, Script, hb_font_t, hb_mask_t, hb_tag_t, tag};

pub struct hb_ot_map_t<'a> {
    found_script: [bool; 2],
    chosen_script: [Option<hb_tag_t>; 2],
    global_mask: hb_mask_t,
    available_features: ChargedVec<'a, hb_tag_t>,
    features: ChargedVec<'a, feature_map_t>,
    lookups: [ChargedVec<'a, lookup_map_t>; 2],
    stages: [ChargedVec<'a, StageMap>; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct feature_map_t {
    tag: hb_tag_t,
    // GSUB/GPOS
    index: [Option<FeatureIndex>; 2],
    stage: [usize; 2],
    shift: u32,
    mask: hb_mask_t,
    // mask for value=1, for quick access
    one_mask: hb_mask_t,
    auto_zwnj: bool,
    auto_zwj: bool,
    random: bool,
    per_syllable: bool,
}

impl Ord for feature_map_t {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tag.cmp(&other.tag)
    }
}

impl PartialOrd for feature_map_t {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.tag.partial_cmp(&other.tag)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct lookup_map_t {
    pub index: LookupIndex,
    // TODO: to bitflags
    pub auto_zwnj: bool,
    pub auto_zwj: bool,
    pub random: bool,
    pub mask: hb_mask_t,
    pub per_syllable: bool,
}

#[derive(Clone, Copy)]
pub struct StageMap {
    // Cumulative
    pub last_lookup: usize,
    pub pause_func: Option<pause_func_t>,
}

// Pause functions return true if new glyph indices might have been added to the buffer.
// This is used to update buffer digest.
pub type pause_func_t = for<'a> fn(
    &hb_ot_shape_plan_t,
    &hb_font_t,
    &mut hb_buffer_t<'a>,
    &Context<'a>,
) -> ProviderResult<bool>;

impl hb_ot_map_t<'_> {
    pub fn feature_available(&self, tag: hb_tag_t, meter: &Context<'_>) -> ProviderResult<bool> {
        for available in self.available_features.iter() {
            meter.step()?;
            if *available == tag {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub const MAX_BITS: u32 = 8;
    pub const MAX_VALUE: u32 = (1 << Self::MAX_BITS) - 1;

    #[inline]
    pub fn found_script(&self, table_index: TableIndex) -> bool {
        self.found_script[table_index]
    }

    #[inline]
    pub fn chosen_script(&self, table_index: TableIndex) -> Option<hb_tag_t> {
        self.chosen_script[table_index]
    }

    #[inline]
    pub fn get_global_mask(&self) -> hb_mask_t {
        self.global_mask
    }

    fn feature(
        &self,
        tag: hb_tag_t,
        meter: &Context<'_>,
    ) -> ProviderResult<Option<&feature_map_t>> {
        let mut lo = 0;
        let mut hi = self.features.len();
        while lo < hi {
            meter.step()?;
            let mid = lo + (hi - lo) / 2;
            match self.features[mid].tag.cmp(&tag) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Ok(Some(&self.features[mid])),
            }
        }
        Ok(None)
    }
    pub fn get_mask(&self, tag: hb_tag_t, meter: &Context<'_>) -> ProviderResult<(hb_mask_t, u32)> {
        Ok(self
            .feature(tag, meter)?
            .map_or((0, 0), |f| (f.mask, f.shift)))
    }
    pub fn get_1_mask(&self, tag: hb_tag_t, meter: &Context<'_>) -> ProviderResult<hb_mask_t> {
        Ok(self.feature(tag, meter)?.map_or(0, |f| f.one_mask))
    }
    pub fn get_feature_index(
        &self,
        table: TableIndex,
        tag: hb_tag_t,
        meter: &Context<'_>,
    ) -> ProviderResult<Option<FeatureIndex>> {
        Ok(self.feature(tag, meter)?.and_then(|f| f.index[table]))
    }
    pub fn get_feature_stage(
        &self,
        table: TableIndex,
        tag: hb_tag_t,
        meter: &Context<'_>,
    ) -> ProviderResult<Option<usize>> {
        Ok(self.feature(tag, meter)?.map(|f| f.stage[table]))
    }

    #[inline]
    pub fn stages(&self, table_index: TableIndex) -> &[StageMap] {
        &self.stages[table_index]
    }

    #[inline]
    pub fn lookup(&self, table_index: TableIndex, index: usize) -> &lookup_map_t {
        &self.lookups[table_index][index]
    }

    #[inline]
    pub fn stage_lookups(&self, table_index: TableIndex, stage: usize) -> &[lookup_map_t] {
        &self.lookups[table_index][self.stage_lookup_range(table_index, stage)]
    }

    #[inline]
    pub fn stage_lookup_range(&self, table_index: TableIndex, stage: usize) -> Range<usize> {
        let stages = &self.stages[table_index];
        let lookups = &self.lookups[table_index];
        let start = stage
            .checked_sub(1)
            .map_or(0, |prev| stages[prev].last_lookup);
        let end = stages
            .get(stage)
            .map_or(lookups.len(), |curr| curr.last_lookup);
        start..end
    }
}

pub type hb_ot_map_feature_flags_t = u32;
pub const F_NONE: u32 = 0x0000;
pub const F_GLOBAL: u32 = 0x0001; /* Feature applies to all characters; results in no mask allocated for it. */
pub const F_HAS_FALLBACK: u32 = 0x0002; /* Has fallback implementation, so include mask bit even if feature not found. */
pub const F_MANUAL_ZWNJ: u32 = 0x0004; /* Don't skip over ZWNJ when matching **context**. */
pub const F_MANUAL_ZWJ: u32 = 0x0008; /* Don't skip over ZWJ when matching **input**. */
pub const F_MANUAL_JOINERS: u32 = F_MANUAL_ZWNJ | F_MANUAL_ZWJ;
pub const F_GLOBAL_MANUAL_JOINERS: u32 = F_GLOBAL | F_MANUAL_JOINERS;
pub const F_GLOBAL_HAS_FALLBACK: u32 = F_GLOBAL | F_HAS_FALLBACK;
pub const F_GLOBAL_SEARCH: u32 = 0x0010; /* If feature not found in LangSys, look for it in global feature list and pick one. */
pub const F_RANDOM: u32 = 0x0020; /* Randomly select a glyph from an AlternateSubstFormat1 subtable. */
pub const F_PER_SYLLABLE: u32 = 0x0040; /* Contain lookup application to within syllable. */

pub struct hb_ot_map_builder_t<'f, 'a> {
    pub meter: &'f Context<'a>,
    face: &'f hb_font_t<'a>,
    found_script: [bool; 2],
    script_index: [Option<ScriptIndex>; 2],
    chosen_script: [Option<hb_tag_t>; 2],
    lang_index: [Option<LanguageIndex>; 2],
    current_stage: [usize; 2],
    feature_infos: ChargedVec<'a, feature_info_t>,
    stages: [ChargedVec<'a, stage_info_t>; 2],
    pub(crate) is_simple: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct feature_info_t {
    tag: hb_tag_t,
    // sequence number, used for stable sorting only
    seq: usize,
    max_value: u32,
    flags: hb_ot_map_feature_flags_t,
    // for non-global features, what should the unset glyphs take
    default_value: u32,
    // GSUB/GPOS
    stage: [usize; 2],
}

#[derive(Clone, Copy)]
struct stage_info_t {
    index: usize,
    pause_func: Option<pause_func_t>,
}

const GLOBAL_BIT_SHIFT: u32 = 8 * u32::SIZE as u32 - 1;
const GLOBAL_BIT_MASK: hb_mask_t = 1 << GLOBAL_BIT_SHIFT;

impl<'f, 'a> hb_ot_map_builder_t<'f, 'a> {
    pub fn new(
        face: &'f hb_font_t<'a>,
        script: Option<Script>,
        language: Option<&Language>,
        meter: &'f Context<'a>,
    ) -> ProviderResult<Self> {
        meter.step()?;
        // Fetch script/language indices for GSUB/GPOS.  We need these later to skip
        // features not available in either table and not waste precious bits for them.
        let (script_tags, lang_tags) = tag::tags_from_script_and_language(script, language, meter)?;

        let mut found_script = [false; 2];
        let mut script_index = [None; 2];
        let mut chosen_script = [None; 2];
        let mut lang_index = [None; 2];

        for (table_index, table) in face.layout_tables() {
            if let Some((found, idx, tag)) = table.select_script(&script_tags, meter)? {
                chosen_script[table_index] = Some(tag);
                found_script[table_index] = found;
                script_index[table_index] = Some(idx);

                if let Some(idx) = table.select_script_language(idx, &lang_tags, meter)? {
                    lang_index[table_index] = Some(idx);
                }
            }
        }

        Ok(Self {
            meter,
            face,
            found_script,
            script_index,
            chosen_script,
            lang_index,
            current_stage: [0, 0],
            feature_infos: ChargedVec::new(),
            stages: [ChargedVec::new(), ChargedVec::new()],
            is_simple: false,
        })
    }

    #[inline]
    pub fn chosen_script(&self, table_index: TableIndex) -> Option<hb_tag_t> {
        self.chosen_script[table_index]
    }

    #[inline]
    fn available_features(&self) -> ProviderResult<ChargedVec<'a, hb_tag_t>> {
        let mut tags = ChargedVec::with_capacity(self.feature_infos.len(), self.meter)?;
        for info in self.feature_infos.iter() {
            self.meter.step()?;
            if self.has_feature(info.tag)? {
                tags.push(info.tag, self.meter)?;
            }
        }
        Ok(tags)
    }

    pub fn has_feature(&self, tag: hb_tag_t) -> ProviderResult<bool> {
        for (table_index, table) in self.face.layout_tables() {
            if let Some(script_index) = self.script_index[table_index] {
                if table
                    .find_language_feature(
                        script_index,
                        self.lang_index[table_index],
                        tag,
                        self.meter,
                    )?
                    .is_some()
                {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    #[inline]
    pub fn add_feature(
        &mut self,
        tag: hb_tag_t,
        flags: hb_ot_map_feature_flags_t,
        value: u32,
    ) -> ProviderResult<()> {
        self.meter.step()?;
        if !tag.is_null() {
            let seq = self.feature_infos.len();
            self.feature_infos.push(
                feature_info_t {
                    tag,
                    seq,
                    max_value: value,
                    flags,
                    default_value: if flags & F_GLOBAL != 0 { value } else { 0 },
                    stage: self.current_stage,
                },
                self.meter,
            )?;
        }

        Ok(())
    }

    #[inline]
    pub fn enable_feature(
        &mut self,
        tag: hb_tag_t,
        flags: hb_ot_map_feature_flags_t,
        value: u32,
    ) -> ProviderResult<()> {
        self.meter.step()?;
        self.add_feature(tag, flags | F_GLOBAL, value)?;

        Ok(())
    }

    #[inline]
    pub fn disable_feature(&mut self, tag: hb_tag_t) -> ProviderResult<()> {
        self.meter.step()?;
        self.add_feature(tag, F_GLOBAL, 0)?;

        Ok(())
    }

    #[inline]
    pub fn add_gsub_pause(&mut self, pause: Option<pause_func_t>) -> ProviderResult<()> {
        self.meter.step()?;
        self.add_pause(TableIndex::GSUB, pause)?;

        Ok(())
    }

    #[inline]
    pub fn add_gpos_pause(&mut self, pause: Option<pause_func_t>) -> ProviderResult<()> {
        self.meter.step()?;
        self.add_pause(TableIndex::GPOS, pause)?;

        Ok(())
    }

    fn add_pause(
        &mut self,
        table_index: TableIndex,
        pause: Option<pause_func_t>,
    ) -> ProviderResult<()> {
        self.meter.step()?;
        self.stages[table_index].push(
            stage_info_t {
                index: self.current_stage[table_index],
                pause_func: pause,
            },
            self.meter,
        )?;

        self.current_stage[table_index] = self.current_stage[table_index]
            .checked_add(1)
            .ok_or(Error::Overflow)?;

        Ok(())
    }

    pub fn compile(&mut self) -> ProviderResult<hb_ot_map_t<'a>> {
        self.meter.step()?;
        // We default to applying required feature in stage 0.  If the required
        // feature has a tag that is known to the shaper, we apply required feature
        // in the stage for that tag.
        let mut required_index = [None; 2];
        let mut required_tag = [None; 2];

        for (table_index, table) in self.face.layout_tables() {
            self.meter.step()?;
            if let Some(script) = self.script_index[table_index] {
                let lang = self.lang_index[table_index];
                if let Some((idx, tag)) =
                    table.get_required_language_feature(script, lang, self.meter)?
                {
                    required_index[table_index] = Some(idx);
                    required_tag[table_index] = Some(tag);
                }
            }
        }

        let (features, required_stage, global_mask) = self.collect_feature_maps(required_tag)?;

        self.add_gsub_pause(None)?;
        self.add_gpos_pause(None)?;

        let (lookups, stages) =
            self.collect_lookup_stages(&features, required_index, required_stage)?;

        Ok(hb_ot_map_t {
            found_script: self.found_script,
            chosen_script: self.chosen_script,
            global_mask,
            available_features: self.available_features()?,
            features,
            lookups,
            stages,
        })
    }

    fn collect_feature_maps(
        &mut self,
        required_tag: [Option<hb_tag_t>; 2],
    ) -> ProviderResult<(ChargedVec<'a, feature_map_t>, [usize; 2], hb_mask_t)> {
        self.meter.step()?;
        let mut map_features = ChargedVec::with_capacity(self.feature_infos.len(), self.meter)?;
        let mut required_stage = [0; 2];
        let mut global_mask = GLOBAL_BIT_MASK;
        let mut next_bit = glyph_flag::DEFINED.count_ones() + 1;

        // Sort features and merge duplicates.
        self.dedup_feature_infos()?;

        for info in self.feature_infos.iter() {
            self.meter.step()?;
            let bits_needed = if info.flags & F_GLOBAL != 0 && info.max_value == 1 {
                // Uses the global bit.
                0
            } else {
                // Limit bits per feature.
                let v = info.max_value;
                let num_bits = 8 * core::mem::size_of_val(&v) as u32 - v.leading_zeros();
                hb_ot_map_t::MAX_BITS.min(num_bits)
            };

            if info.max_value == 0 || next_bit + bits_needed >= GLOBAL_BIT_SHIFT {
                // Feature disabled, or not enough bits.
                continue;
            }

            let mut found = false;
            let mut feature_index = [None; 2];

            for (table_index, table) in self.face.layout_tables() {
                self.meter.step()?;
                if required_tag[table_index] == Some(info.tag) {
                    required_stage[table_index] = info.stage[table_index];
                }

                if let Some(script) = self.script_index[table_index] {
                    let lang = self.lang_index[table_index];
                    if let Some(idx) =
                        table.find_language_feature(script, lang, info.tag, self.meter)?
                    {
                        feature_index[table_index] = Some(idx);
                        found = true;
                    }
                }
            }

            if !found && info.flags & F_GLOBAL_SEARCH != 0 {
                // hb_ot_layout_table_find_feature
                for (table_index, table) in self.face.layout_tables() {
                    self.meter.step()?;
                    if let Some(idx) = table.features.index_bounded(info.tag, self.meter)? {
                        feature_index[table_index] = Some(idx);
                        found = true;
                    }
                }
            }

            if !found && !info.flags & F_HAS_FALLBACK != 0 {
                continue;
            }

            let (shift, mask) = if info.flags & F_GLOBAL != 0 && info.max_value == 1 {
                // Uses the global bit
                (GLOBAL_BIT_SHIFT, GLOBAL_BIT_MASK)
            } else {
                let shift = next_bit;
                let mask = (1 << (next_bit + bits_needed)) - (1 << next_bit);
                next_bit += bits_needed;
                global_mask |= (info.default_value << shift) & mask;
                (shift, mask)
            };

            map_features.push(
                feature_map_t {
                    tag: info.tag,
                    index: feature_index,
                    stage: info.stage,
                    shift,
                    mask,
                    one_mask: (1 << shift) & mask,
                    auto_zwnj: info.flags & F_MANUAL_ZWNJ == 0,
                    auto_zwj: info.flags & F_MANUAL_ZWJ == 0,
                    random: info.flags & F_RANDOM != 0,
                    per_syllable: info.flags & F_PER_SYLLABLE != 0,
                },
                self.meter,
            )?;
        }

        if self.is_simple {
            bounded_sort(map_features.as_mut_slice(), self.meter)?;
        }

        Ok((map_features, required_stage, global_mask))
    }

    fn dedup_feature_infos(&mut self) -> ProviderResult<()> {
        self.meter.step()?;
        let feature_infos = &mut self.feature_infos;
        if feature_infos.is_empty() {
            return Ok(());
        }

        if !self.is_simple {
            bounded_sort(feature_infos.as_mut_slice(), self.meter)?;
        }

        let mut j = 0;
        for i in 1..feature_infos.len() {
            self.meter.step()?;
            if feature_infos[i].tag != feature_infos[j].tag {
                j += 1;
                feature_infos[j] = feature_infos[i];
            } else {
                if feature_infos[i].flags & F_GLOBAL != 0 {
                    feature_infos[j].flags |= F_GLOBAL;
                    feature_infos[j].max_value = feature_infos[i].max_value;
                    feature_infos[j].default_value = feature_infos[i].default_value;
                } else {
                    if feature_infos[j].flags & F_GLOBAL != 0 {
                        feature_infos[j].flags ^= F_GLOBAL;
                    }
                    feature_infos[j].max_value =
                        feature_infos[j].max_value.max(feature_infos[i].max_value);
                    // Inherit default_value from j
                }
                let flags = feature_infos[i].flags & F_HAS_FALLBACK;
                feature_infos[j].flags |= flags;
                feature_infos[j].stage[0] =
                    feature_infos[j].stage[0].min(feature_infos[i].stage[0]);
                feature_infos[j].stage[1] =
                    feature_infos[j].stage[1].min(feature_infos[i].stage[1]);
            }
        }

        feature_infos.truncate(j + 1);

        Ok(())
    }

    fn collect_lookup_stages(
        &self,
        map_features: &[feature_map_t],
        required_feature_index: [Option<FeatureIndex>; 2],
        required_feature_stage: [usize; 2],
    ) -> ProviderResult<(
        [ChargedVec<'a, lookup_map_t>; 2],
        [ChargedVec<'a, StageMap>; 2],
    )> {
        self.meter.step()?;
        let mut map_lookups = [ChargedVec::new(), ChargedVec::new()];
        let mut map_stages = [
            ChargedVec::with_capacity(self.stages[0].len(), self.meter)?,
            ChargedVec::with_capacity(self.stages[1].len(), self.meter)?,
        ];

        for table_index in TableIndex::iter() {
            self.meter.step()?;
            // Collect lookup indices for features.
            let mut stage_index = 0;
            let mut last_lookup = 0;

            let coords = self.face.ttfp_face.variation_coordinates();
            let variation_index = match self
                .face
                .layout_table(table_index)
                .and_then(|t| t.variations)
            {
                Some(variations) => variations.find_index_bounded(coords, self.meter)?,
                None => None,
            };

            for stage in 0..self.current_stage[table_index] {
                self.meter.step()?;
                if let Some(feature_index) = required_feature_index[table_index] {
                    if required_feature_stage[table_index] == stage {
                        self.add_lookups(
                            &mut map_lookups[table_index],
                            table_index,
                            feature_index,
                            variation_index,
                            GLOBAL_BIT_MASK,
                            true,
                            true,
                            false,
                            false,
                        )?;
                    }
                }

                for feature in map_features {
                    self.meter.step()?;
                    if let Some(feature_index) = feature.index[table_index] {
                        if feature.stage[table_index] == stage {
                            self.add_lookups(
                                &mut map_lookups[table_index],
                                table_index,
                                feature_index,
                                variation_index,
                                feature.mask,
                                feature.auto_zwnj,
                                feature.auto_zwj,
                                feature.random,
                                feature.per_syllable,
                            )?;
                        }
                    }
                }

                // Sort lookups and merge duplicates.
                let lookups = &mut map_lookups[table_index];
                let len = lookups.len();

                if last_lookup + 1 < len {
                    bounded_sort(&mut lookups[last_lookup..], self.meter)?;

                    let mut j = last_lookup;
                    for i in j + 1..len {
                        self.meter.step()?;
                        if lookups[i].index != lookups[j].index {
                            j += 1;
                            lookups[j] = lookups[i];
                        } else {
                            lookups[j].mask |= lookups[i].mask;
                            lookups[j].auto_zwnj &= lookups[i].auto_zwnj;
                            lookups[j].auto_zwj &= lookups[i].auto_zwj;
                        }
                    }

                    lookups.truncate(j + 1);
                }

                last_lookup = lookups.len();

                if let Some(info) = self.stages[table_index].get(stage_index) {
                    if info.index == stage {
                        map_stages[table_index].push(
                            StageMap {
                                last_lookup,
                                pause_func: info.pause_func,
                            },
                            self.meter,
                        )?;

                        stage_index += 1;
                    }
                }
            }
        }

        Ok((map_lookups, map_stages))
    }

    fn add_lookups(
        &self,
        lookups: &mut ChargedVec<'a, lookup_map_t>,
        table_index: TableIndex,
        feature_index: FeatureIndex,
        variation_index: Option<VariationIndex>,
        mask: hb_mask_t,
        auto_zwnj: bool,
        auto_zwj: bool,
        random: bool,
        per_syllable: bool,
    ) -> ProviderResult<()> {
        self.meter.step()?;
        let Some(table) = self.face.layout_table(table_index) else {
            return Ok(());
        };
        let alternative = match (variation_index, table.variations) {
            (Some(index), Some(variations)) => {
                variations.find_substitute_bounded(feature_index, index, self.meter)?
            }
            _ => None,
        };
        let feature = match alternative {
            Some(feature) => feature,
            None => match table.features.get_bounded(feature_index, self.meter)? {
                Some(feature) => feature,
                None => return Ok(()),
            },
        };
        let count = usize::from(feature.lookup_indices.len());
        let end = lookups.len().checked_add(count).ok_or(Error::Overflow)?;
        self.meter
            .check_lookups(u64::try_from(end).map_err(|_| Error::Overflow)?)?;
        // Verify the count pass before any mutation/allocation; no invalid lookup is skipped.
        for at in 0..feature.lookup_indices.len() {
            self.meter.step()?;
            let index = feature
                .lookup_indices
                .get_bounded(at, self.meter)?
                .ok_or(Error::CorruptFont)?;
            if index >= table.lookups.len() {
                return Err(Error::CorruptFont);
            }
        }
        lookups.reserve_total(end, self.meter)?;
        for at in 0..feature.lookup_indices.len() {
            self.meter.step()?;
            let index = feature
                .lookup_indices
                .get_bounded(at, self.meter)?
                .ok_or(Error::CorruptFont)?;
            lookups.push(
                lookup_map_t {
                    mask,
                    index,
                    auto_zwnj,
                    auto_zwj,
                    random,
                    per_syllable,
                },
                self.meter,
            )?;
        }
        Ok(())
    }
}

fn bounded_sort<T: Copy + Ord>(values: &mut [T], meter: &Context<'_>) -> ProviderResult<()> {
    for i in 1..values.len() {
        meter.step()?;
        let value = values[i];
        let mut at = i;
        while at != 0 {
            meter.step()?;
            if values[at - 1] <= value {
                break;
            }
            values[at] = values[at - 1];
            at -= 1;
        }
        values[at] = value;
    }
    Ok(())
}
