//! Swatch groups, palette scope and the swatch-group commands (lane SMB-COL-SWATCH-GROUPS-1;
//! STU-COL-177, STU-COL-178, the group operations of STU-COL-179).
//!
//! * `StudioSwatchGroup` (set-local key id, name, `root` flag): a palette's groups in panel order.
//!   Exactly one ROOT group per palette, listed FIRST; groups do not nest (STU-COL-177 names only
//!   flat named groups, so a group has no parent and no cycle can exist). A swatch belongs to at
//!   most one group: `StudioSwatch.group_ref` names a NAMED group, and an absent `group_ref` is
//!   membership of the root group (one encoding: naming the root in `group_ref` is refused). The
//!   order of a group's swatches is their relative order in the palette's `swatches` list. An
//!   empty group list is a palette with only its implicit root (record sets written before
//!   groups existed stay byte-identical); a non-empty list must carry the root explicitly.
//! * `StudioPaletteScope` `document` | `application` | `system` and `StudioSwatchPalette`
//!   (`hsk.studio.swatch_palette@1`) for the host palettes. The document palette is the document's
//!   own `StudioVectorRecords` swatches: its wire form has no scope field and no palette member, so
//!   a document can store only document-scope swatches. `StudioHostPalettes` holds the host's
//!   application and system palettes; a swatch reference resolves document -> application ->
//!   system (`StudioHostPalettes::resolve`, `validate_vector_records_with_palettes`) and an
//!   unresolvable one is a validation error (`DanglingReference` / `StudioPaletteError::Unresolvable`),
//!   never a silent black. The system palette is read-only: every group command on it is refused.
//! * `StudioSwatchGroupCommand` create-root, create, rename, delete (with a replace-on-delete
//!   target that receives the members), reorder, move-swatch (between groups and within one).
//!   Each takes the caller's `CommandContext` (actor, client session, command id, base revision,
//!   correlation), is atomic (a refusal changes nothing) and returns the prior palette state as
//!   its undo (`undo_swatch_group_command`). The report carries codes, counters and the opaque
//!   correlation only; Folio has no Observe edge, so the host emits it through Observe.
//!
//! Studio readings, vendor parity UNVERIFIED (D5): root listed first and not reorderable; delete
//! appends the members after the replacement group's own members in their prior order.
#![deny(clippy::arithmetic_side_effects)]
use super::tools::validate_swatch;
use super::{Code, Result, StudioSwatch, StudioVectorRecords, canceled, check, err, key};
use crate::Budget;
use hsk_studio_accord::{CancellationToken, CommandContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(SwatchPaletteSchema, "hsk.studio.swatch_palette@1");

/// Most groups of one palette.
pub const SWATCH_GROUPS_MAX: usize = 1024;
/// Most swatches of one host palette (the document limit is `TOOLS_MAX_ENTRIES`, the same value).
pub const PALETTE_SWATCHES_MAX: usize = super::TOOLS_MAX_ENTRIES;

fn is_false(v: &bool) -> bool {
    !*v
}
fn label_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}

/// One swatch group (STU-COL-177).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSwatchGroup {
    /// Set-local key id.
    pub group_id: String,
    pub name: String,
    /// The palette's root group (exactly one, listed first, not deletable).
    #[serde(default, skip_serializing_if = "is_false")]
    pub root: bool,
}

/// STU-COL-178 palette scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioPaletteScope {
    Document,
    Application,
    System,
}

impl StudioPaletteScope {
    /// Resolution order: document first, then application, then system.
    pub const RESOLUTION_ORDER: [Self; 3] = [Self::Document, Self::Application, Self::System];
    /// The system palette reflects the OS palette and is read-only.
    pub const fn read_only(self) -> bool {
        matches!(self, Self::System)
    }
    pub const fn code(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Application => "application",
            Self::System => "system",
        }
    }
}

/// A host swatch palette (`hsk.studio.swatch_palette@1`): the application or system palette, or a
/// detached document palette.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSwatchPalette {
    pub schema_id: SwatchPaletteSchema,
    /// Key id.
    pub palette_id: String,
    pub name: String,
    pub scope: StudioPaletteScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub swatches: Vec<StudioSwatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub swatch_groups: Vec<StudioSwatchGroup>,
}

impl StudioSwatchPalette {
    pub fn new(palette_id: impl Into<String>, name: impl Into<String>, scope: StudioPaletteScope) -> Self {
        Self { schema_id: SwatchPaletteSchema::V1, palette_id: palette_id.into(), name: name.into(), scope, swatches: Vec::new(), swatch_groups: Vec::new() }
    }
    /// The swatch `swatch_id`.
    pub fn swatch(&self, swatch_id: &str) -> Option<&StudioSwatch> {
        self.swatches.iter().find(|s| s.swatch_id == swatch_id)
    }
}

/// The group rules of one palette (STU-COL-177), without a budget context: group count, ids,
/// names, exactly one root listed first, and every swatch `group_ref` naming a named group.
pub(crate) fn check_groups(swatches: &[StudioSwatch], groups: &[StudioSwatchGroup], token: &CancellationToken) -> Result<()> {
    check(groups.len() <= SWATCH_GROUPS_MAX, Code::BudgetExceeded, "swatch_groups", "count")?;
    let mut ids: BTreeMap<&str, bool> = BTreeMap::new();
    let mut root = false;
    for (i, g) in groups.iter().enumerate() {
        canceled(token)?;
        let t = g.group_id.as_str();
        check(key(t), Code::InvalidId, "swatch_groups", "group_id")?;
        check(label_ok(&g.name), Code::InvalidField, t, "name")?;
        if ids.insert(t, g.root).is_some() {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if g.root {
            if root {
                return Err(err(Code::DuplicateIdentity, "swatch_groups/root"));
            }
            check(i == 0, Code::InvalidField, t, "root")?;
            root = true;
        }
    }
    if !groups.is_empty() && !root {
        return Err(err(Code::MissingField, "swatch_groups/root"));
    }
    for s in swatches {
        if let Some(g) = &s.group_ref {
            match ids.get(g.as_str()) {
                None => return Err(err(Code::DanglingReference, format!("{}/group_ref", s.swatch_id))),
                Some(true) => return Err(err(Code::InvalidField, format!("{}/group_ref", s.swatch_id))),
                Some(false) => {}
            }
        }
    }
    Ok(())
}

/// Validates a host palette: swatch count, ids and shapes (the `StudioSwatch` rules of the
/// document), tint bases inside the palette, and the group rules.
pub fn validate_swatch_palette(p: &StudioSwatchPalette, b: Budget, token: &CancellationToken) -> Result<()> {
    canceled(token)?;
    check(key(&p.palette_id), Code::InvalidId, "palette", "palette_id")?;
    check(label_ok(&p.name), Code::InvalidField, "palette", "name")?;
    check(p.swatches.len() <= PALETTE_SWATCHES_MAX, Code::BudgetExceeded, "swatches", "count")?;
    let total = p.swatches.len().saturating_add(p.swatch_groups.len());
    check(total <= b.nodes, Code::BudgetExceeded, "palette", "nodes")?;
    let mut seen: BTreeMap<&str, &StudioSwatch> = BTreeMap::new();
    for s in &p.swatches {
        canceled(token)?;
        let t = s.swatch_id.as_str();
        super::id(t, "SSWT", "swatches/swatch_id")?;
        if seen.insert(t, s).is_some() {
            return Err(err(Code::DuplicateIdentity, t));
        }
        validate_swatch(s, t)?;
    }
    for s in &p.swatches {
        if let Some(base) = &s.base_swatch_ref {
            let b = seen.get(base.as_str()).ok_or_else(|| err(Code::DanglingReference, format!("{}/base_swatch_ref", s.swatch_id)))?;
            check(matches!(b.swatch_kind, super::StudioSwatchKind::Process | super::StudioSwatchKind::Global | super::StudioSwatchKind::Spot), Code::InvalidField, &s.swatch_id, "base_swatch_ref")?;
        }
    }
    check_groups(&p.swatches, &p.swatch_groups, token)
}

/// Refusals of palette resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioPaletteError {
    /// A host slot holds a palette of another scope (`slot` is the slot's scope).
    WrongScope { slot: StudioPaletteScope, found: StudioPaletteScope },
    /// No palette in document -> application -> system order holds the swatch.
    Unresolvable,
}
impl StudioPaletteError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongScope { .. } => "palette_wrong_scope",
            Self::Unresolvable => "swatch_unresolvable",
        }
    }
}
impl std::fmt::Display for StudioPaletteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StudioPaletteError {}

/// The host's application and system palettes (each optional; a missing one resolves nothing).
#[derive(Clone, Copy, Debug, Default)]
pub struct StudioHostPalettes<'a> {
    application: Option<&'a StudioSwatchPalette>,
    system: Option<&'a StudioSwatchPalette>,
}

/// A resolved swatch reference and the scope that held it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StudioResolvedSwatch<'a> {
    pub scope: StudioPaletteScope,
    pub swatch: &'a StudioSwatch,
}

impl<'a> StudioHostPalettes<'a> {
    /// Refuses a palette of the wrong scope in either slot.
    pub fn new(application: Option<&'a StudioSwatchPalette>, system: Option<&'a StudioSwatchPalette>) -> std::result::Result<Self, StudioPaletteError> {
        for (slot, p) in [(StudioPaletteScope::Application, application), (StudioPaletteScope::System, system)] {
            if let Some(p) = p
                && p.scope != slot
            {
                return Err(StudioPaletteError::WrongScope { slot, found: p.scope });
            }
        }
        Ok(Self { application, system })
    }
    pub fn application(&self) -> Option<&'a StudioSwatchPalette> {
        self.application
    }
    pub fn system(&self) -> Option<&'a StudioSwatchPalette> {
        self.system
    }
    /// The swatch outside the document: application first, then system.
    pub fn external(&self, swatch_id: &str) -> Option<StudioResolvedSwatch<'a>> {
        [(StudioPaletteScope::Application, self.application), (StudioPaletteScope::System, self.system)]
            .into_iter()
            .find_map(|(scope, p)| p.and_then(|p| p.swatch(swatch_id)).map(|swatch| StudioResolvedSwatch { scope, swatch }))
    }
    /// Resolves `swatch_id` in document -> application -> system order (STU-COL-178).
    pub fn resolve(&self, document: &'a StudioVectorRecords, swatch_id: &str) -> std::result::Result<StudioResolvedSwatch<'a>, StudioPaletteError> {
        if let Some(swatch) = document.swatch(swatch_id) {
            return Ok(StudioResolvedSwatch { scope: StudioPaletteScope::Document, swatch });
        }
        self.external(swatch_id).ok_or(StudioPaletteError::Unresolvable)
    }
}

/// A palette that holds swatches and groups: the document's vector records (scope `document`) or
/// a host `StudioSwatchPalette`.
pub trait StudioSwatchContainer: Clone {
    fn scope(&self) -> StudioPaletteScope;
    fn swatch_list(&self) -> &[StudioSwatch];
    fn group_list(&self) -> &[StudioSwatchGroup];
    fn replace_palette(&mut self, swatches: Vec<StudioSwatch>, groups: Vec<StudioSwatchGroup>);
}
impl StudioSwatchContainer for StudioVectorRecords {
    fn scope(&self) -> StudioPaletteScope {
        StudioPaletteScope::Document
    }
    fn swatch_list(&self) -> &[StudioSwatch] {
        &self.swatches
    }
    fn group_list(&self) -> &[StudioSwatchGroup] {
        &self.swatch_groups
    }
    fn replace_palette(&mut self, swatches: Vec<StudioSwatch>, groups: Vec<StudioSwatchGroup>) {
        self.swatches = swatches;
        self.swatch_groups = groups;
    }
}
impl StudioSwatchContainer for StudioSwatchPalette {
    fn scope(&self) -> StudioPaletteScope {
        self.scope
    }
    fn swatch_list(&self) -> &[StudioSwatch] {
        &self.swatches
    }
    fn group_list(&self) -> &[StudioSwatchGroup] {
        &self.swatch_groups
    }
    fn replace_palette(&mut self, swatches: Vec<StudioSwatch>, groups: Vec<StudioSwatchGroup>) {
        self.swatches = swatches;
        self.swatch_groups = groups;
    }
}

/// STU-COL-177 group operations (STU-COL-179 typed commands). Indexes count NAMED groups (the root
/// stays first) or, for `MoveSwatch`, the target group's members without the moved swatch;
/// `None` appends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StudioSwatchGroupCommand {
    CreateRoot { group_id: String, name: String },
    Create { group_id: String, name: String, index: Option<usize> },
    Rename { group_id: String, name: String },
    /// Members of the deleted group move to `replace_with` (the root or another named group).
    Delete { group_id: String, replace_with: String },
    Reorder { group_id: String, index: usize },
    MoveSwatch { swatch_id: String, group_id: String, index: Option<usize> },
}

/// Stable command ids, one per group operation plus undo.
pub const SWATCH_GROUP_COMMAND_IDS: [&str; 7] = [
    "swatch.group.create_root",
    "swatch.group.create",
    "swatch.group.rename",
    "swatch.group.delete",
    "swatch.group.reorder",
    "swatch.group.move_swatch",
    "swatch.group.undo",
];

impl StudioSwatchGroupCommand {
    pub const fn command_id(&self) -> &'static str {
        match self {
            Self::CreateRoot { .. } => SWATCH_GROUP_COMMAND_IDS[0],
            Self::Create { .. } => SWATCH_GROUP_COMMAND_IDS[1],
            Self::Rename { .. } => SWATCH_GROUP_COMMAND_IDS[2],
            Self::Delete { .. } => SWATCH_GROUP_COMMAND_IDS[3],
            Self::Reorder { .. } => SWATCH_GROUP_COMMAND_IDS[4],
            Self::MoveSwatch { .. } => SWATCH_GROUP_COMMAND_IDS[5],
        }
    }
}

/// Typed refusals of the group commands. A refusal changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioSwatchGroupError {
    /// The context carries no command id.
    CommandMissing,
    /// The context's command id is not the id of this command.
    CommandMismatch,
    StaleRevision { expected: u64, current: u64 },
    /// The palette is the read-only system palette.
    ReadOnlyScope,
    SecondRoot,
    RootMissing,
    RootNotDeletable,
    RootNotMovable,
    GroupMissing,
    SwatchMissing,
    DuplicateGroup,
    InvalidGroupId,
    InvalidName,
    /// Missing, equal to the deleted group, or not a group of the palette.
    InvalidReplacement,
    IndexOutOfRange,
    BudgetExceeded,
    /// The resulting (or restored) palette fails the palette rules; the Folio diagnostic code.
    InvalidPalette(Code),
}
impl StudioSwatchGroupError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CommandMissing => "command_missing",
            Self::CommandMismatch => "command_mismatch",
            Self::StaleRevision { .. } => "stale_revision",
            Self::ReadOnlyScope => "read_only_scope",
            Self::SecondRoot => "second_root",
            Self::RootMissing => "root_missing",
            Self::RootNotDeletable => "root_not_deletable",
            Self::RootNotMovable => "root_not_movable",
            Self::GroupMissing => "group_missing",
            Self::SwatchMissing => "swatch_missing",
            Self::DuplicateGroup => "duplicate_group",
            Self::InvalidGroupId => "invalid_group_id",
            Self::InvalidName => "invalid_name",
            Self::InvalidReplacement => "invalid_replacement",
            Self::IndexOutOfRange => "index_out_of_range",
            Self::BudgetExceeded => "budget_exceeded",
            Self::InvalidPalette(_) => "invalid_palette",
        }
    }
}
impl std::fmt::Display for StudioSwatchGroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StudioSwatchGroupError {}

/// The palette state a command replaced: its undo.
#[derive(Clone, Debug, PartialEq)]
pub struct StudioSwatchPaletteState {
    pub swatches: Vec<StudioSwatch>,
    pub swatch_groups: Vec<StudioSwatchGroup>,
}

/// What a host emits through Observe for one applied command: codes, counters and the opaque
/// correlation only (no names, ids or colours).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioSwatchEditReport {
    pub command: &'static str,
    pub scope: StudioPaletteScope,
    pub base_revision: u64,
    pub correlation: u64,
    /// Swatches whose group membership changed.
    pub swatches_moved: usize,
    /// Groups of the resulting palette (the implicit root counts as one).
    pub groups: usize,
}

/// An applied command: the new palette holder, the undo state and the report.
#[derive(Clone, Debug, PartialEq)]
pub struct StudioSwatchGroupOutcome<T> {
    pub palette: T,
    pub undo: StudioSwatchPaletteState,
    pub report: StudioSwatchEditReport,
}

fn admit<T: StudioSwatchContainer>(p: &T, ctx: &CommandContext, revision: u64, command_id: &str) -> std::result::Result<(), StudioSwatchGroupError> {
    let c = ctx.command().ok_or(StudioSwatchGroupError::CommandMissing)?;
    if c.as_str() != command_id {
        return Err(StudioSwatchGroupError::CommandMismatch);
    }
    if ctx.base_revision() != revision {
        return Err(StudioSwatchGroupError::StaleRevision { expected: ctx.base_revision(), current: revision });
    }
    if p.scope().read_only() {
        return Err(StudioSwatchGroupError::ReadOnlyScope);
    }
    Ok(())
}

fn position(groups: &[StudioSwatchGroup], group_id: &str) -> std::result::Result<usize, StudioSwatchGroupError> {
    groups.iter().position(|g| g.group_id == group_id).ok_or(StudioSwatchGroupError::GroupMissing)
}

/// The `group_ref` that puts a swatch into group `g`.
fn membership(g: &StudioSwatchGroup) -> Option<String> {
    if g.root { None } else { Some(g.group_id.clone()) }
}

/// Removes the swatches at `picked` (ascending) and appends them, in order, after the last swatch
/// whose `group_ref` is `target` (or at the end when the group has none), setting `target`.
fn append_members(swatches: &mut Vec<StudioSwatch>, picked: &[usize], target: &Option<String>, at: Option<usize>) -> std::result::Result<(), StudioSwatchGroupError> {
    let mut moved: Vec<StudioSwatch> = Vec::with_capacity(picked.len());
    for &i in picked.iter().rev() {
        moved.push(swatches.remove(i));
    }
    moved.reverse();
    let members: Vec<usize> = swatches.iter().enumerate().filter(|(_, s)| &s.group_ref == target).map(|(i, _)| i).collect();
    let insert_at = match at {
        Some(i) if i > members.len() => return Err(StudioSwatchGroupError::IndexOutOfRange),
        Some(i) if i < members.len() => members[i],
        _ => members.last().map_or(swatches.len(), |&last| last.saturating_add(1)),
    };
    for (k, mut s) in moved.into_iter().enumerate() {
        s.group_ref = target.clone();
        swatches.insert(insert_at.saturating_add(k), s);
    }
    Ok(())
}

fn apply_to(swatches: &mut Vec<StudioSwatch>, groups: &mut Vec<StudioSwatchGroup>, cmd: &StudioSwatchGroupCommand) -> std::result::Result<usize, StudioSwatchGroupError> {
    use StudioSwatchGroupError as E;
    let new_group = |groups: &[StudioSwatchGroup], group_id: &str, name: &str| -> std::result::Result<(), StudioSwatchGroupError> {
        if !key(group_id) {
            return Err(E::InvalidGroupId);
        }
        if !label_ok(name) {
            return Err(E::InvalidName);
        }
        if groups.iter().any(|g| g.group_id == group_id) {
            return Err(E::DuplicateGroup);
        }
        if groups.len() >= SWATCH_GROUPS_MAX {
            return Err(E::BudgetExceeded);
        }
        Ok(())
    };
    let named = groups.len().saturating_sub(1);
    match cmd {
        StudioSwatchGroupCommand::CreateRoot { group_id, name } => {
            if groups.iter().any(|g| g.root) {
                return Err(E::SecondRoot);
            }
            new_group(groups, group_id, name)?;
            groups.insert(0, StudioSwatchGroup { group_id: group_id.clone(), name: name.clone(), root: true });
            Ok(0)
        }
        StudioSwatchGroupCommand::Create { group_id, name, index } => {
            if !groups.first().is_some_and(|g| g.root) {
                return Err(E::RootMissing);
            }
            new_group(groups, group_id, name)?;
            let i = index.unwrap_or(named);
            if i > named {
                return Err(E::IndexOutOfRange);
            }
            groups.insert(i.saturating_add(1), StudioSwatchGroup { group_id: group_id.clone(), name: name.clone(), root: false });
            Ok(0)
        }
        StudioSwatchGroupCommand::Rename { group_id, name } => {
            let i = position(groups, group_id)?;
            if !label_ok(name) {
                return Err(E::InvalidName);
            }
            groups[i].name = name.clone();
            Ok(0)
        }
        StudioSwatchGroupCommand::Delete { group_id, replace_with } => {
            let i = position(groups, group_id)?;
            if groups[i].root {
                return Err(E::RootNotDeletable);
            }
            if replace_with == group_id {
                return Err(E::InvalidReplacement);
            }
            let r = groups.iter().position(|g| &g.group_id == replace_with).ok_or(E::InvalidReplacement)?;
            let target = membership(&groups[r]);
            let picked: Vec<usize> = swatches.iter().enumerate().filter(|(_, s)| s.group_ref.as_deref() == Some(group_id.as_str())).map(|(k, _)| k).collect();
            append_members(swatches, &picked, &target, None)?;
            groups.remove(i);
            Ok(picked.len())
        }
        StudioSwatchGroupCommand::Reorder { group_id, index } => {
            let i = position(groups, group_id)?;
            if groups[i].root {
                return Err(E::RootNotMovable);
            }
            if *index >= named {
                return Err(E::IndexOutOfRange);
            }
            let g = groups.remove(i);
            groups.insert(index.saturating_add(1), g);
            Ok(0)
        }
        StudioSwatchGroupCommand::MoveSwatch { swatch_id, group_id, index } => {
            let s = swatches.iter().position(|s| &s.swatch_id == swatch_id).ok_or(E::SwatchMissing)?;
            if groups.is_empty() {
                return Err(E::RootMissing);
            }
            let g = position(groups, group_id)?;
            let target = membership(&groups[g]);
            let changed = swatches[s].group_ref != target;
            append_members(swatches, &[s], &target, *index)?;
            Ok(usize::from(changed))
        }
    }
}

fn finish<T: StudioSwatchContainer>(p: &T, swatches: Vec<StudioSwatch>, groups: Vec<StudioSwatchGroup>, ctx: &CommandContext, command: &'static str, moved: usize) -> std::result::Result<StudioSwatchGroupOutcome<T>, StudioSwatchGroupError> {
    check_groups(&swatches, &groups, &CancellationToken::default()).map_err(|d| StudioSwatchGroupError::InvalidPalette(d.code))?;
    let undo = StudioSwatchPaletteState { swatches: p.swatch_list().to_vec(), swatch_groups: p.group_list().to_vec() };
    let group_count = groups.len().max(1);
    let mut palette = p.clone();
    palette.replace_palette(swatches, groups);
    let report = StudioSwatchEditReport { command, scope: p.scope(), base_revision: ctx.base_revision(), correlation: ctx.correlation().value(), swatches_moved: moved, groups: group_count };
    Ok(StudioSwatchGroupOutcome { palette, undo, report })
}

/// Applies one group command to `palette` at `revision` (the caller's current document or palette
/// revision; the context's base revision must equal it). Atomic: a refusal returns the typed error
/// and `palette` is not changed (the caller keeps it). The context's command id must be
/// `cmd.command_id()`.
pub fn apply_swatch_group_command<T: StudioSwatchContainer>(palette: &T, ctx: &CommandContext, revision: u64, cmd: &StudioSwatchGroupCommand) -> std::result::Result<StudioSwatchGroupOutcome<T>, StudioSwatchGroupError> {
    admit(palette, ctx, revision, cmd.command_id())?;
    let mut swatches = palette.swatch_list().to_vec();
    let mut groups = palette.group_list().to_vec();
    let moved = apply_to(&mut swatches, &mut groups, cmd)?;
    finish(palette, swatches, groups, ctx, cmd.command_id(), moved)
}

/// Restores the palette state an earlier command replaced (its `undo`), as one more attributed
/// command (`swatch.group.undo`) at `revision`. The state is caller-supplied, so before anything is
/// restored it must hold at most `PALETTE_SWATCHES_MAX` swatches with unique `SSWT` ids, each passing
/// the palette's swatch rules (`validate_swatch`), and the group rules (STUDIO-R53 F-R53-6; tests
/// `swatch_group_undo.rs`); a refusal is `InvalidPalette` with the Folio code.
pub fn undo_swatch_group_command<T: StudioSwatchContainer>(palette: &T, ctx: &CommandContext, revision: u64, state: &StudioSwatchPaletteState) -> std::result::Result<StudioSwatchGroupOutcome<T>, StudioSwatchGroupError> {
    let command = SWATCH_GROUP_COMMAND_IDS[6];
    admit(palette, ctx, revision, command)?;
    if state.swatches.len() > PALETTE_SWATCHES_MAX {
        return Err(StudioSwatchGroupError::InvalidPalette(Code::BudgetExceeded));
    }
    for s in &state.swatches {
        let t = s.swatch_id.as_str();
        super::id(t, "SSWT", "swatches/swatch_id").and_then(|()| validate_swatch(s, t)).map_err(|d| StudioSwatchGroupError::InvalidPalette(d.code))?;
    }
    let current: BTreeMap<&str, &Option<String>> = palette.swatch_list().iter().map(|s| (s.swatch_id.as_str(), &s.group_ref)).collect();
    let moved = state.swatches.iter().filter(|s| current.get(s.swatch_id.as_str()).is_some_and(|g| **g != s.group_ref)).count();
    let ids: BTreeSet<&str> = state.swatches.iter().map(|s| s.swatch_id.as_str()).collect();
    if ids.len() != state.swatches.len() {
        return Err(StudioSwatchGroupError::InvalidPalette(Code::DuplicateIdentity));
    }
    finish(palette, state.swatches.clone(), state.swatch_groups.clone(), ctx, command, moved)
}
