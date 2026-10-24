//! Develop records: virtual copies, history and snapshots, copy / paste / sync of settings per
//! group, and batch apply over a selection (lane SMB-LR-LIBRARY-WRITE; Lightroom Classic Develop
//! module parity rows LR-19 and the Library "virtual copy" / "sync settings" workflow; STU-RAW-128
//! snapshots, STU-RAW-017 multi-image apply on the canonical selection set).
//!
//! A [`DevelopRecord`] is a value: one develop state per (asset, copy). Durable authority is the
//! host's (14.12: SurrealDB/EventLedger develop records; the CKC catalog lists a virtual copy, this
//! crate stores nothing). Every mutation names the record revision it was computed against and
//! fails as [`RecordError::Conflict`] when it moved (STU-SDB-004 style compare-and-set); every
//! accepted mutation advances the revision by exactly one.
//!
//! History: each accepted edit is a step holding the complete resulting recipe. Moving to an
//! earlier step (undo) keeps the later steps until the next edit, which discards them (redo
//! branch). The history is bounded: beyond `max_history` the oldest steps are dropped and counted
//! in [`DevelopRecord::trimmed_steps`], never silently. Snapshots are complete named states
//! (STU-RAW-128), unique by name; applying one is an edit.
//!
//! Settings groups ([`SettingsGroup`]) partition the recipe the way the Copy / Sync Settings
//! dialog does. Image-specific state is never copied: as-shot neutral, camera matrix and
//! profiles, the lens-profile binding, stored Upright candidates and the crop orientation stay the
//! target's; an absolute white balance (resolved from the source raw) cannot be copied without the
//! target raw and is reported as a loss. Group switches (`enable_*`) follow the source when every
//! group sharing the switch is copied, otherwise the target keeps its switch on if either was on.
//! Lightroom Classic behaviour of each group is UNVERIFIED (D5).
//!
//! Spot removal (lane SMB-LR-HEALING-2, [`SettingsGroup::SpotRemoval`]): the STU-RAW-124 retouch
//! areas and distraction areas copy as recorded, i.e. their centres, radii and source points as the
//! same normalised coordinates on the target frame (declared reading; not re-matched to target
//! content), and `enable_retouch` / `enable_distraction_removal` follow the source (the group owns
//! both switches). Generative adapter fill records are pixel records of the source image: they are
//! not copied, the target keeps its own, and a source holding any reports
//! `adapter_fill_image_specific`.
#![deny(clippy::arithmetic_side_effects)]

use crate::DevelopError;
use crate::preset::{DevelopPreset, Snapshot};
use crate::recipe::{DevelopRecipe, Enables, WbMode};
use std::collections::{BTreeMap, BTreeSet};

/// Bounds of record operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordLimits {
    pub max_history: usize,
    pub max_snapshots: usize,
    pub max_name_bytes: usize,
    pub max_batch: usize,
}

impl Default for RecordLimits {
    fn default() -> Self {
        Self { max_history: 1000, max_snapshots: 256, max_name_bytes: 256, max_batch: 100_000 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecordError {
    /// The record moved since `expected` was read.
    Conflict { record_id: String, expected: u64, current: u64 },
    NotFound,
    NameInvalid,
    NameTaken,
    Limit(&'static str),
    Invalid(&'static str),
    Develop(DevelopError),
}

impl RecordError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "record_conflict",
            Self::NotFound => "record_not_found",
            Self::NameInvalid => "record_name_invalid",
            Self::NameTaken => "record_name_taken",
            Self::Limit(c) | Self::Invalid(c) => c,
            Self::Develop(e) => e.code(),
        }
    }
}

impl From<DevelopError> for RecordError {
    fn from(e: DevelopError) -> Self {
        Self::Develop(e)
    }
}

fn check_name(name: &str, l: &RecordLimits) -> Result<(), RecordError> {
    if name.trim().is_empty() || name.len() > l.max_name_bytes || name.chars().any(char::is_control) {
        return Err(RecordError::NameInvalid);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Settings groups

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingsGroup {
    WhiteBalance,
    BasicTone,
    Presence,
    ToneCurve,
    ColourMixer,
    ColourGrading,
    PointColour,
    Grayscale,
    Sharpening,
    NoiseReduction,
    LensCorrections,
    Transform,
    Effects,
    Calibration,
    Crop,
    Masking,
    CreativeLook,
    ProcessVersion,
    SpotRemoval,
}

impl SettingsGroup {
    pub const ALL: [Self; 19] = [
        Self::WhiteBalance,
        Self::BasicTone,
        Self::Presence,
        Self::ToneCurve,
        Self::ColourMixer,
        Self::ColourGrading,
        Self::PointColour,
        Self::Grayscale,
        Self::Sharpening,
        Self::NoiseReduction,
        Self::LensCorrections,
        Self::Transform,
        Self::Effects,
        Self::Calibration,
        Self::Crop,
        Self::Masking,
        Self::CreativeLook,
        Self::ProcessVersion,
        Self::SpotRemoval,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Self::WhiteBalance => "white_balance",
            Self::BasicTone => "basic_tone",
            Self::Presence => "presence",
            Self::ToneCurve => "tone_curve",
            Self::ColourMixer => "colour_mixer",
            Self::ColourGrading => "colour_grading",
            Self::PointColour => "point_colour",
            Self::Grayscale => "grayscale",
            Self::Sharpening => "sharpening",
            Self::NoiseReduction => "noise_reduction",
            Self::LensCorrections => "lens_corrections",
            Self::Transform => "transform",
            Self::Effects => "effects",
            Self::Calibration => "calibration",
            Self::Crop => "crop",
            Self::Masking => "masking",
            Self::CreativeLook => "creative_look",
            Self::ProcessVersion => "process_version",
            Self::SpotRemoval => "spot_removal",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.key() == key)
    }

    /// The group switch this group shares (if any).
    const fn switch(self) -> Option<Switch> {
        Some(match self {
            Self::Presence | Self::Sharpening | Self::NoiseReduction => Switch::Detail,
            Self::ToneCurve => Switch::ToneCurve,
            Self::ColourMixer | Self::ColourGrading | Self::PointColour => Switch::Colour,
            Self::Grayscale => Switch::Grayscale,
            Self::LensCorrections => Switch::Lens,
            Self::Transform => Switch::Transform,
            Self::Effects => Switch::Effects,
            Self::Calibration => Switch::Calibration,
            Self::Masking => Switch::Mask,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Switch {
    Detail,
    ToneCurve,
    Colour,
    Grayscale,
    Lens,
    Transform,
    Effects,
    Calibration,
    Mask,
}

fn switch_mut(e: &mut Enables, s: Switch) -> &mut bool {
    match s {
        Switch::Detail => &mut e.enable_detail,
        Switch::ToneCurve => &mut e.enable_tone_curve,
        Switch::Colour => &mut e.enable_colour_adjustments,
        Switch::Grayscale => &mut e.enable_grayscale_mix,
        Switch::Lens => &mut e.enable_lens_corrections,
        Switch::Transform => &mut e.enable_transform,
        Switch::Effects => &mut e.enable_effects,
        Switch::Calibration => &mut e.enable_calibration,
        Switch::Mask => &mut e.enable_mask_group_based_corrections,
    }
}

/// A group (or part of it) that could not be copied as authored, with a stable code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupLoss {
    pub group: SettingsGroup,
    pub code: &'static str,
}

/// `target` with the `groups` of `source` copied in (validated; `target` is not changed).
pub fn copy_groups(
    source: &DevelopRecipe,
    target: &DevelopRecipe,
    groups: &BTreeSet<SettingsGroup>,
) -> Result<(DevelopRecipe, Vec<GroupLoss>), DevelopError> {
    let mut out = target.clone();
    let mut losses = Vec::new();
    let (s, o) = (&source.adjust, &mut out.adjust);
    for g in groups {
        match g {
            SettingsGroup::WhiteBalance => {
                o.white_balance = s.white_balance;
                if source.white_balance_mode != WbMode::AsShot || target.white_balance_mode != WbMode::AsShot {
                    // An absolute white balance is resolved from a raw; the target's raw is not here.
                    losses.push(GroupLoss { group: *g, code: "white_balance_needs_target_raw" });
                }
            }
            SettingsGroup::BasicTone => {
                out.exposure_stops = source.exposure_stops;
                o.tone = s.tone;
            }
            SettingsGroup::Presence => {
                o.presence = s.presence;
                o.detail.texture = s.detail.texture;
                o.detail.dehaze = s.detail.dehaze;
            }
            SettingsGroup::ToneCurve => {
                out.curves = source.curves.clone();
                out.curve_refine_saturation = source.curve_refine_saturation;
                o.parametric = s.parametric;
            }
            SettingsGroup::ColourMixer => o.mixer = s.mixer,
            SettingsGroup::ColourGrading => o.grading = s.grading,
            SettingsGroup::PointColour => o.point_colours.clone_from(&s.point_colours),
            SettingsGroup::Grayscale => o.grayscale = s.grayscale,
            SettingsGroup::Sharpening => o.detail.sharpen = s.detail.sharpen,
            SettingsGroup::NoiseReduction => o.detail.noise = s.detail.noise,
            SettingsGroup::LensCorrections => {
                let binding = o.optics.profile.take();
                o.optics = s.optics.clone();
                // A source profile correction cannot apply without the target lens's own binding.
                if s.optics.lens_profile_enable && s.optics.profile.is_some() && binding.is_none() {
                    losses.push(GroupLoss { group: *g, code: "lens_profile_needs_target_binding" });
                }
                o.optics.profile = binding;
            }
            SettingsGroup::Transform => {
                o.geometry = s.geometry.clone();
                // Upright candidates are analysed per image: the target re-analyses its own frame.
                o.geometry.upright_transforms = None;
                if !s.geometry.upright_guides.is_empty() {
                    losses.push(GroupLoss { group: *g, code: "upright_guides_image_specific" });
                }
            }
            SettingsGroup::Effects => o.effects = s.effects,
            SettingsGroup::Calibration => o.calibration = s.calibration,
            SettingsGroup::Crop => {
                let orientation = out.crop.orientation;
                out.crop = source.crop;
                out.crop.orientation = orientation;
            }
            SettingsGroup::Masking => o.local.clone_from(&s.local),
            SettingsGroup::CreativeLook => o.look.clone_from(&s.look),
            SettingsGroup::ProcessVersion => {
                out.process_version = source.process_version;
                out.engine_version = source.engine_version;
            }
            SettingsGroup::SpotRemoval => {
                o.retouch.areas.clone_from(&s.retouch.areas);
                o.retouch.distraction.clone_from(&s.retouch.distraction);
                out.enables.enable_retouch = source.enables.enable_retouch;
                out.enables.enable_distraction_removal = source.enables.enable_distraction_removal;
                if !s.retouch.adapter_fills.is_empty() {
                    losses.push(GroupLoss { group: *g, code: "adapter_fill_image_specific" });
                }
            }
        }
    }
    // Group switches.
    let switches = [
        Switch::Detail,
        Switch::ToneCurve,
        Switch::Colour,
        Switch::Grayscale,
        Switch::Lens,
        Switch::Transform,
        Switch::Effects,
        Switch::Calibration,
        Switch::Mask,
    ];
    let mut src_enables = source.enables;
    for sw in switches {
        let members: Vec<SettingsGroup> = SettingsGroup::ALL.into_iter().filter(|g| g.switch() == Some(sw)).collect();
        let copied = members.iter().filter(|m| groups.contains(m)).count();
        if copied == 0 {
            continue;
        }
        let from = *switch_mut(&mut src_enables, sw);
        let to = switch_mut(&mut out.enables, sw);
        *to = if copied == members.len() { from } else { *to || from };
    }
    if out.adjust.version < source.adjust.version {
        out.adjust.version = source.adjust.version;
    }
    out.validate()?;
    Ok((out, losses))
}

/// Copied settings waiting to be pasted (Copy Settings dialog: groups + source state).
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsClipboard {
    pub groups: BTreeSet<SettingsGroup>,
    pub source: DevelopRecipe,
}

impl SettingsClipboard {
    pub fn copy(source: &DevelopRecipe, groups: BTreeSet<SettingsGroup>) -> Result<Self, DevelopError> {
        source.validate()?;
        Ok(Self { groups, source: source.clone() })
    }
    pub fn paste(&self, target: &DevelopRecipe) -> Result<(DevelopRecipe, Vec<GroupLoss>), DevelopError> {
        copy_groups(&self.source, target, &self.groups)
    }
}

// ------------------------------------------------------------------------------------------------
// Records

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryStep {
    pub label: String,
    pub recipe: DevelopRecipe,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevelopRecord {
    record_id: String,
    asset_ref: String,
    master_record: Option<String>,
    copy_name: Option<String>,
    revision: u64,
    history: Vec<HistoryStep>,
    cursor: usize,
    snapshots: Vec<Snapshot>,
    trimmed_steps: u64,
    limits: RecordLimits,
}

impl DevelopRecord {
    /// A master record (history starts with one `Import` step holding `recipe`).
    pub fn new(record_id: &str, asset_ref: &str, recipe: DevelopRecipe, limits: RecordLimits) -> Result<Self, RecordError> {
        check_name(record_id, &limits)?;
        check_name(asset_ref, &limits)?;
        recipe.validate()?;
        if limits.max_history == 0 {
            return Err(RecordError::Limit("record_history_limit"));
        }
        Ok(Self {
            record_id: record_id.to_owned(),
            asset_ref: asset_ref.to_owned(),
            master_record: None,
            copy_name: None,
            revision: 1,
            history: vec![HistoryStep { label: "Import".into(), recipe }],
            cursor: 0,
            snapshots: Vec::new(),
            trimmed_steps: 0,
            limits,
        })
    }

    pub fn record_id(&self) -> &str {
        &self.record_id
    }
    pub fn asset_ref(&self) -> &str {
        &self.asset_ref
    }
    pub fn master_record(&self) -> Option<&str> {
        self.master_record.as_deref()
    }
    pub fn copy_name(&self) -> Option<&str> {
        self.copy_name.as_deref()
    }
    pub fn is_virtual_copy(&self) -> bool {
        self.master_record.is_some()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn recipe(&self) -> &DevelopRecipe {
        &self.history[self.cursor].recipe
    }
    pub fn history(&self) -> &[HistoryStep] {
        &self.history
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }
    /// Oldest history steps dropped by the `max_history` bound.
    pub fn trimmed_steps(&self) -> u64 {
        self.trimmed_steps
    }

    fn check(&self, expected: u64) -> Result<(), RecordError> {
        if expected != self.revision {
            return Err(RecordError::Conflict { record_id: self.record_id.clone(), expected, current: self.revision });
        }
        Ok(())
    }

    fn bump(&mut self) -> Result<(), RecordError> {
        self.revision = self.revision.checked_add(1).ok_or(RecordError::Limit("record_revision_overflow"))?;
        Ok(())
    }

    /// Records an edit: validates `recipe`, discards steps after the cursor, appends the step.
    pub fn edit(&mut self, expected_revision: u64, label: &str, recipe: DevelopRecipe) -> Result<u64, RecordError> {
        self.check(expected_revision)?;
        check_name(label, &self.limits)?;
        recipe.validate()?;
        self.push_step(label, recipe)
    }

    fn push_step(&mut self, label: &str, recipe: DevelopRecipe) -> Result<u64, RecordError> {
        self.revision.checked_add(1).ok_or(RecordError::Limit("record_revision_overflow"))?;
        self.history.truncate(self.cursor.saturating_add(1));
        self.history.push(HistoryStep { label: label.to_owned(), recipe });
        if self.history.len() > self.limits.max_history {
            let drop = self.history.len().saturating_sub(self.limits.max_history);
            self.history.drain(..drop);
            self.trimmed_steps = self.trimmed_steps.saturating_add(drop as u64);
        }
        self.cursor = self.history.len().saturating_sub(1);
        self.bump()?;
        Ok(self.revision)
    }

    /// Moves the cursor to history step `index` (undo / redo / click a step).
    pub fn go_to_step(&mut self, expected_revision: u64, index: usize) -> Result<u64, RecordError> {
        self.check(expected_revision)?;
        if index >= self.history.len() {
            return Err(RecordError::NotFound);
        }
        if index == self.cursor {
            return Ok(self.revision);
        }
        self.cursor = index;
        self.bump()?;
        Ok(self.revision)
    }

    pub fn undo(&mut self, expected_revision: u64) -> Result<u64, RecordError> {
        let i = self.cursor.checked_sub(1).ok_or(RecordError::Invalid("record_nothing_to_undo"))?;
        self.go_to_step(expected_revision, i)
    }

    pub fn redo(&mut self, expected_revision: u64) -> Result<u64, RecordError> {
        let i = self.cursor.saturating_add(1);
        if i >= self.history.len() {
            return Err(RecordError::Invalid("record_nothing_to_redo"));
        }
        self.go_to_step(expected_revision, i)
    }

    /// Stores the current state as a named snapshot (STU-RAW-128: complete state, unique name).
    pub fn take_snapshot(&mut self, expected_revision: u64, name: &str) -> Result<u64, RecordError> {
        self.check(expected_revision)?;
        check_name(name, &self.limits)?;
        if self.snapshots.iter().any(|s| s.name == name) {
            return Err(RecordError::NameTaken);
        }
        if self.snapshots.len() >= self.limits.max_snapshots {
            return Err(RecordError::Limit("record_snapshot_limit"));
        }
        self.snapshots.push(Snapshot { name: name.to_owned(), recipe: self.recipe().clone() });
        self.bump()?;
        Ok(self.revision)
    }

    pub fn delete_snapshot(&mut self, expected_revision: u64, name: &str) -> Result<u64, RecordError> {
        self.check(expected_revision)?;
        let i = self.snapshots.iter().position(|s| s.name == name).ok_or(RecordError::NotFound)?;
        self.snapshots.remove(i);
        self.bump()?;
        Ok(self.revision)
    }

    /// Applies a snapshot as an edit (history step `Snapshot: <name>`).
    pub fn apply_snapshot(&mut self, expected_revision: u64, name: &str) -> Result<u64, RecordError> {
        self.check(expected_revision)?;
        let recipe = self.snapshots.iter().find(|s| s.name == name).ok_or(RecordError::NotFound)?.recipe.clone();
        recipe.validate()?;
        let label: String = format!("Snapshot: {name}").chars().take(self.limits.max_name_bytes).collect();
        self.push_step(&label, recipe)
    }

    /// A virtual copy of this record's current state: same asset, its own history (one step) and
    /// no snapshots; a copy of a copy refers to the same master.
    pub fn create_virtual_copy(&self, record_id: &str, copy_name: &str) -> Result<Self, RecordError> {
        check_name(record_id, &self.limits)?;
        check_name(copy_name, &self.limits)?;
        if record_id == self.record_id {
            return Err(RecordError::NameTaken);
        }
        Ok(Self {
            record_id: record_id.to_owned(),
            asset_ref: self.asset_ref.clone(),
            master_record: Some(self.master_record.clone().unwrap_or_else(|| self.record_id.clone())),
            copy_name: Some(copy_name.to_owned()),
            revision: 1,
            history: vec![HistoryStep { label: format!("Virtual copy of {}", self.record_id).chars().take(self.limits.max_name_bytes).collect(), recipe: self.recipe().clone() }],
            cursor: 0,
            snapshots: Vec::new(),
            trimmed_steps: 0,
            limits: self.limits,
        })
    }
}

/// "Set copy as master": the copy becomes the master of its asset; the old master and every
/// sibling copy point to it (the old master takes the copy's name). All touched records advance
/// their revision; `expected` lists the revision read for each record of the asset (all of them
/// must be named, so no sibling is left pointing at the old master).
pub fn set_copy_as_master(records: &mut [DevelopRecord], copy_id: &str, expected: &BTreeMap<String, u64>) -> Result<(), RecordError> {
    let copy = records.iter().find(|r| r.record_id == copy_id).ok_or(RecordError::NotFound)?;
    let old_master = copy.master_record.clone().ok_or(RecordError::Invalid("record_not_a_copy"))?;
    let asset = copy.asset_ref.clone();
    let copy_name = copy.copy_name.clone();
    let family: Vec<usize> = (0..records.len()).filter(|&i| records[i].asset_ref == asset).collect();
    if !family.iter().any(|&i| records[i].record_id == old_master) {
        return Err(RecordError::NotFound);
    }
    for &i in &family {
        let r = &records[i];
        match expected.get(&r.record_id) {
            Some(e) => r.check(*e)?,
            None => return Err(RecordError::Invalid("record_family_incomplete")),
        }
        r.revision.checked_add(1).ok_or(RecordError::Limit("record_revision_overflow"))?;
    }
    for &i in &family {
        let r = &mut records[i];
        if r.record_id == copy_id {
            r.master_record = None;
            r.copy_name = None;
        } else {
            if r.record_id == old_master {
                r.copy_name.clone_from(&copy_name);
            }
            r.master_record = Some(copy_id.to_owned());
        }
        r.bump()?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Batch apply

/// What a batch applies to each record of the selection.
#[derive(Clone, Copy, Debug)]
pub enum BatchOp<'a> {
    /// Paste / sync: the clipboard's groups of its source.
    Paste(&'a SettingsClipboard),
    /// A develop preset with an amount (0..=2).
    Preset(&'a DevelopPreset, f64),
    /// Resets `groups` to `defaults` (per-camera defaults are the caller's registry, STU-RAW-128).
    Reset(&'a DevelopRecipe, &'a BTreeSet<SettingsGroup>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct BatchOutcome {
    pub record_id: String,
    pub revision: u64,
    pub losses: Vec<GroupLoss>,
}

/// Applies `op` to every record named in `selection` (record id -> expected revision; the
/// canonical selection set, STU-RAW-017) as one history step labelled `label`. All targets are
/// checked and computed before any record changes; one conflict or refusal changes nothing.
pub fn batch_apply(
    records: &mut [DevelopRecord],
    selection: &[(String, u64)],
    op: BatchOp<'_>,
    label: &str,
    limits: &RecordLimits,
) -> Result<Vec<BatchOutcome>, RecordError> {
    if selection.is_empty() {
        return Err(RecordError::Invalid("record_selection_empty"));
    }
    if selection.len() > limits.max_batch {
        return Err(RecordError::Limit("record_selection_limit"));
    }
    check_name(label, limits)?;
    let mut seen = BTreeSet::new();
    let mut staged: Vec<(usize, DevelopRecipe, Vec<GroupLoss>)> = Vec::with_capacity(selection.len());
    for (id, expected) in selection {
        if !seen.insert(id.as_str()) {
            return Err(RecordError::Invalid("record_selection_repeated"));
        }
        let i = records.iter().position(|r| &r.record_id == id).ok_or(RecordError::NotFound)?;
        let r = &records[i];
        r.check(*expected)?;
        r.revision.checked_add(1).ok_or(RecordError::Limit("record_revision_overflow"))?;
        let (recipe, losses) = match op {
            BatchOp::Paste(clip) => clip.paste(r.recipe())?,
            BatchOp::Preset(p, amount) => (p.apply(r.recipe(), amount)?, Vec::new()),
            BatchOp::Reset(defaults, groups) => copy_groups(defaults, r.recipe(), groups)?,
        };
        staged.push((i, recipe, losses));
    }
    let mut out = Vec::with_capacity(staged.len());
    for (i, recipe, losses) in staged {
        let r = &mut records[i];
        let revision = r.push_step(label, recipe)?;
        out.push(BatchOutcome { record_id: r.record_id.clone(), revision, losses });
    }
    Ok(out)
}
