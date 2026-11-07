//! The project bin (lane SMB-PROJECT-BIN-1; STU-VID-050 import, STU-VID-010 sequence, STU-VID-016
//! preset, STU-VID-021 track, STU-SHL-113 project-bin panel).
//!
//! [`StudioProjectBin`] (`hsk.studio.project_bin@1`) is a record set beside the document, one per
//! `document_id`: bin folders ([`StudioBinFolder`], a tree by `parent_id`) and media items
//! ([`StudioMediaItem`]), each item one imported file. An item's `source` is the clip source a clip
//! cut from it carries (`ClipSource::Media`: manifest handle plus content digest, never a filesystem
//! path, STU-VID-020), so a clip and its item are matched by that value and the package reports and
//! embeds the bytes as timeline media. The item also stores what import read from the file (kind,
//! format, duration in ticks, raster and frame rate, or sample rate and channels), so the bin lists
//! and places it without decoding.
//!
//! [`StudioProjectBinCommand`] creates a folder, imports items (all or none), moves items and removes
//! items no clip cuts from. Each takes the caller's `CommandContext` and returns the next bin and a
//! report of codes and counters; it holds no document it could change, so the revision and the commit
//! stay with the caller (the media markers pattern). The bounds, ids, command ids and the one-item-
//! per-digest rule are Studio's: the clauses state none. Vendor parity (Premiere bins, duplicate
//! imports) is UNVERIFIED (D5).
//!
//! [`sequence_from_settings`] builds a new empty sequence and its settings entry from a preset's
//! settings (initial video and audio track counts, STU-VID-016a); [`with_tracks`] appends tracks of any
//! kind (`cmd.sequence.addtracks`, `cmd.sequence.addaudiosubmixtrack`); [`with_clip`] places one clip on
//! a track. They mint no id: the caller gives every id, and the commit validates the document.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (workflow V4-01), every
//! list is counted before it is walked and charged against the [`Budget`], and a parse refuses bytes
//! over [`MAX_PROJECT_BIN_INPUT_BYTES`] whatever the budget allows.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, ClipSource, Code, ContentDigest, Diagnostic, Result, SequenceFrameRate, SequenceSchema, StudioClip, StudioDocument, StudioSequence,
    StudioSequenceSettings, StudioSequenceSettingsEntry, StudioTrack, TrackKind, TrackSchema, Unresolved, err, id, key,
    sequence_preset::parse_wire,
    timeline::{control_free, handle},
    values::Ctx,
    CUSTOM_EDITING_MODE,
};
use hsk_studio_accord::{CancellationToken, CommandContext, Ticks};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(ProjectBinSchema, "hsk.studio.project_bin@1");

/// Folders one bin may hold.
pub const MAX_BIN_FOLDERS: usize = 1024;
/// Media items one bin may hold.
pub const MAX_BIN_ITEMS: usize = 4096;
/// Deepest folder nesting (a root folder is depth 1).
pub const MAX_BIN_DEPTH: usize = 16;
/// Longest folder or item name, and longest format token, in bytes.
pub const MAX_BIN_TEXT_BYTES: usize = 256;
/// Most audio channels an item may declare (reel's decoder bound).
pub const MAX_BIN_CHANNELS: u16 = 8;
/// Largest wire form of a bin in bytes, whatever the caller's budget allows.
pub const MAX_PROJECT_BIN_INPUT_BYTES: usize = 16_777_216;

const CANCEL_STRIDE: usize = 256;

/// What an item holds: picture (with or without sound) or sound only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioMediaKind {
    Video,
    Audio,
}

/// One bin folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBinFolder {
    /// Bin-local key (`[A-Za-z0-9_-]{1,128}`), unique among the bin's folders.
    pub folder_id: String,
    pub name: String,
    /// The folder holding it; `null` = the bin root.
    pub parent_id: Option<String>,
}

/// One imported file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMediaItem {
    /// Bin-local key, unique among the bin's items.
    pub item_id: String,
    pub name: String,
    /// The folder holding it; `null` = the bin root.
    pub folder_id: Option<String>,
    /// The clip source of every clip cut from it: `ClipSource::Media`, one item per content digest.
    pub source: ClipSource,
    pub kind: StudioMediaKind,
    /// The format import detected (a key, e.g. reel's provider id `wav-pcm`, `y4m`).
    pub format: String,
    /// Length in the item's own time base, ticks; above zero.
    pub duration: u64,
    /// Video: ticks per frame of the picture, its raster. Absent for audio.
    pub frame_rate: Option<SequenceFrameRate>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Audio: sample rate and channels in the file. Absent for video.
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u16>,
}

/// The per-document project bin, beside the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioProjectBin {
    pub schema_id: ProjectBinSchema,
    /// `SDOC` id of the document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<StudioBinFolder>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_items: Vec<StudioMediaItem>,
}
impl StudioProjectBin {
    /// An empty bin for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self { schema_id: ProjectBinSchema::V1, document_id: document_id.into(), folders: Vec::new(), media_items: Vec::new() }
    }
    pub fn item(&self, item_id: &str) -> Option<&StudioMediaItem> {
        self.media_items.iter().find(|i| i.item_id == item_id)
    }
    pub fn folder(&self, folder_id: &str) -> Option<&StudioBinFolder> {
        self.folders.iter().find(|f| f.folder_id == folder_id)
    }
    /// The item whose bytes are `digest`.
    pub fn item_of_digest(&self, digest: &ContentDigest) -> Option<&StudioMediaItem> {
        self.media_items.iter().find(|i| matches!(&i.source, ClipSource::Media { content_digest, .. } if content_digest == digest))
    }
}

fn text_ok(s: &str) -> bool {
    control_free(s) && s.len() <= MAX_BIN_TEXT_BYTES
}

fn media_ok(source: &ClipSource) -> bool {
    match source {
        ClipSource::Media { artifact_manifest_id, content_digest, asset_id } => {
            handle(artifact_manifest_id)
                && control_free(&content_digest.algorithm)
                && control_free(&content_digest.digest)
                && asset_id.as_deref().is_none_or(crate::is_catalog_asset_id)
        }
        _ => false,
    }
}

/// The fields of one item, without the bin context.
fn item_ok(i: &StudioMediaItem) -> Result<()> {
    let at = |f: &str| format!("media_items/{}/{f}", i.item_id);
    if !key(&i.item_id) {
        return Err(err(Code::InvalidId, "media_items/item_id"));
    }
    if !text_ok(&i.name) {
        return Err(err(Code::InvalidField, at("name")));
    }
    if !media_ok(&i.source) {
        return Err(err(Code::InvalidField, at("source")));
    }
    if !key(&i.format) {
        return Err(err(Code::InvalidField, at("format")));
    }
    if i.duration == 0 {
        return Err(err(Code::InvalidField, at("duration")));
    }
    let picture = matches!((i.frame_rate, i.width, i.height), (Some(r), Some(w), Some(h)) if r.ticks_per_frame > 0 && w > 0 && h > 0);
    let sound = matches!((i.sample_rate_hz, i.channels), (Some(r), Some(c)) if r > 0 && (1..=MAX_BIN_CHANNELS).contains(&c));
    let none_of_picture = i.frame_rate.is_none() && i.width.is_none() && i.height.is_none();
    let none_of_sound = i.sample_rate_hz.is_none() && i.channels.is_none();
    let fits = match i.kind {
        StudioMediaKind::Video => picture && none_of_sound,
        StudioMediaKind::Audio => sound && none_of_picture,
    };
    if !fits {
        return Err(err(Code::InvalidField, at("kind")));
    }
    Ok(())
}

/// Validates the bin against the document: `SDOC` id equal to the document's; at most
/// [`MAX_BIN_FOLDERS`] folders and [`MAX_BIN_ITEMS`] items (counted before they are walked, then
/// charged against `Budget::nodes`); folder ids and item ids keys, each unique (`DuplicateIdentity`);
/// names free of control characters and at most [`MAX_BIN_TEXT_BYTES`] (`InvalidField`); a parent or
/// item folder that exists (`DanglingReference`); no folder cycle (`ContainmentCycle`) and no deeper
/// nesting than [`MAX_BIN_DEPTH`] (`BudgetExceeded`); every item a `ClipSource::Media` held by no other
/// item (`DuplicateIdentity`), with the fields of its kind and only those (`InvalidField`). Names and
/// handles share one count against `Budget::payload_bytes`.
pub fn validate_project_bin(bin: &StudioProjectBin, d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    id(&bin.document_id, "SDOC", "document_id")?;
    if bin.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if bin.folders.len() > MAX_BIN_FOLDERS || bin.media_items.len() > MAX_BIN_ITEMS {
        return Err(err(Code::BudgetExceeded, "project_bin"));
    }
    cx.nodes(bin.folders.len().saturating_add(bin.media_items.len()), "project_bin")?;
    let mut payload = 0usize;
    let mut charge = |n: usize, at: &str| -> Result<()> {
        payload = payload.checked_add(n).filter(|p| *p <= b.payload_bytes).ok_or_else(|| err(Code::BudgetExceeded, at.to_owned()))?;
        Ok(())
    };
    let mut parents: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for (n, f) in bin.folders.iter().enumerate() {
        if n.is_multiple_of(CANCEL_STRIDE) {
            cx.tick()?;
        }
        if !key(&f.folder_id) {
            return Err(err(Code::InvalidId, "folders/folder_id"));
        }
        if !text_ok(&f.name) {
            return Err(err(Code::InvalidField, format!("folders/{}/name", f.folder_id)));
        }
        charge(f.folder_id.len().saturating_add(f.name.len()), "folders")?;
        if parents.insert(f.folder_id.as_str(), f.parent_id.as_deref()).is_some() {
            return Err(err(Code::DuplicateIdentity, format!("folders/{}", f.folder_id)));
        }
    }
    for f in &bin.folders {
        // Walk to the root: at most MAX_BIN_DEPTH steps, so a cycle or a deep chain ends.
        let mut at = f.parent_id.as_deref();
        let mut depth = 1usize;
        while let Some(parent) = at {
            if parent == f.folder_id {
                return Err(err(Code::ContainmentCycle, format!("folders/{}", f.folder_id)));
            }
            at = *parents.get(parent).ok_or_else(|| err(Code::DanglingReference, format!("folders/{}/parent_id", f.folder_id)))?;
            depth = depth.saturating_add(1);
            if depth > MAX_BIN_DEPTH {
                return Err(err(Code::BudgetExceeded, format!("folders/{}/depth", f.folder_id)));
            }
        }
    }
    let (mut ids, mut digests) = (BTreeSet::new(), BTreeSet::new());
    for (n, i) in bin.media_items.iter().enumerate() {
        if n.is_multiple_of(CANCEL_STRIDE) {
            cx.tick()?;
        }
        item_ok(i)?;
        if !ids.insert(i.item_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("media_items/{}", i.item_id)));
        }
        if let Some(folder) = &i.folder_id
            && !parents.contains_key(folder.as_str())
        {
            return Err(err(Code::DanglingReference, format!("media_items/{}/folder_id", i.item_id)));
        }
        let ClipSource::Media { artifact_manifest_id, content_digest, .. } = &i.source else {
            return Err(err(Code::InvalidField, format!("media_items/{}/source", i.item_id)));
        };
        charge(
            i.item_id.len().saturating_add(i.name.len()).saturating_add(i.format.len()).saturating_add(artifact_manifest_id.len()).saturating_add(content_digest.digest.len()),
            "media_items",
        )?;
        if !digests.insert((content_digest.algorithm.as_str(), content_digest.digest.as_str())) {
            return Err(err(Code::DuplicateIdentity, format!("media_items/{}/source", i.item_id)));
        }
    }
    Ok(())
}

/// JSON schema (draft 2020-12) of the project bin.
pub fn project_bin_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioProjectBin>()
}

/// Parses wire JSON (bounded by [`MAX_PROJECT_BIN_INPUT_BYTES`], `Budget::input_bytes` and depth before
/// any parse; duplicate keys `DuplicateField`, unknown fields, a missing field and wrong types
/// `InvalidJson`) and validates it against the document.
pub fn parse_project_bin(input: &[u8], d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<StudioProjectBin> {
    if input.len() > MAX_PROJECT_BIN_INPUT_BYTES {
        return Err(err(Code::BudgetExceeded, "project_bin"));
    }
    let bin: StudioProjectBin = parse_wire(input, b, token, "project_bin")?;
    validate_project_bin(&bin, d, b, token)?;
    Ok(bin)
}

/// Command ids, in [`StudioProjectBinCommand`] variant order.
pub const PROJECT_BIN_COMMAND_IDS: [&str; 4] = ["project_bin.new_folder", "project_bin.import", "project_bin.move", "project_bin.remove"];

/// A typed project bin command.
#[derive(Clone, Debug, PartialEq)]
pub enum StudioProjectBinCommand {
    /// Adds `folder` (`cmd.file.new.bin`-like New Bin; its parent must exist).
    NewFolder { folder: StudioBinFolder },
    /// Adds every item, or none (STU-VID-050: the caller already refused what it could not read).
    Import { items: Vec<StudioMediaItem> },
    /// Moves the items into `folder_id` (`None` = the root).
    Move { item_ids: Vec<String>, folder_id: Option<String> },
    /// Removes the items; refused while a clip of the document cuts from one.
    Remove { item_ids: Vec<String> },
}
impl StudioProjectBinCommand {
    /// The command id the caller's context must carry.
    pub const fn command_id(&self) -> &'static str {
        match self {
            Self::NewFolder { .. } => PROJECT_BIN_COMMAND_IDS[0],
            Self::Import { .. } => PROJECT_BIN_COMMAND_IDS[1],
            Self::Move { .. } => PROJECT_BIN_COMMAND_IDS[2],
            Self::Remove { .. } => PROJECT_BIN_COMMAND_IDS[3],
        }
    }
}

/// A typed refusal; nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioProjectBinError {
    CommandMissing,
    CommandMismatch,
    StaleRevision { expected: u64, current: u64 },
    /// The held bin fails its own validation; the Folio diagnostic code.
    InvalidInput(Code),
    /// The command names no item, or names one twice.
    EmptyOrRepeated,
    /// The new folder or item fails the bin rules; the Folio diagnostic code.
    InvalidFolder(Code),
    InvalidItem(Code),
    DuplicateFolder,
    /// An item id, or the bytes of an item, the bin already holds.
    DuplicateItem,
    FolderMissing,
    ItemMissing,
    /// A clip of the document cuts from the item.
    ItemInUse,
    BudgetExceeded,
    Canceled,
}
impl StudioProjectBinError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CommandMissing => "command_missing",
            Self::CommandMismatch => "command_mismatch",
            Self::StaleRevision { .. } => "stale_revision",
            Self::InvalidInput(_) => "invalid_input",
            Self::EmptyOrRepeated => "empty_or_repeated",
            Self::InvalidFolder(_) => "invalid_folder",
            Self::InvalidItem(_) => "invalid_item",
            Self::DuplicateFolder => "duplicate_folder",
            Self::DuplicateItem => "duplicate_item",
            Self::FolderMissing => "folder_missing",
            Self::ItemMissing => "item_missing",
            Self::ItemInUse => "item_in_use",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }
}
impl std::fmt::Display for StudioProjectBinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StudioProjectBinError {}

/// What a host emits through Observe for one applied command: codes, counters and the opaque
/// correlation only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioProjectBinReport {
    pub command: &'static str,
    pub base_revision: u64,
    pub correlation: u64,
    pub folders: usize,
    pub media_items: usize,
    /// Items or folders the command added, moved or removed.
    pub touched: usize,
    pub changed: bool,
}

/// An applied command: the resulting bin and the report. The caller's input is the undo state.
#[derive(Clone, Debug, PartialEq)]
pub struct StudioProjectBinOutcome {
    pub bin: StudioProjectBin,
    pub report: StudioProjectBinReport,
}

fn distinct(ids: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    !ids.is_empty() && ids.iter().all(|i| seen.insert(i.as_str()))
}

/// Applies one command at `revision` (the caller's current document revision; the context's base
/// revision must equal it and its command id must be `cmd.command_id()`). The bin is validated first
/// (`InvalidInput`), the command runs on a copy, and the result is validated again and must fit what a
/// parse under `b` accepts (`BudgetExceeded`). A refusal returns the typed error.
pub fn apply_project_bin_command(
    document: &StudioDocument,
    bin: &StudioProjectBin,
    ctx: &CommandContext,
    revision: u64,
    cmd: &StudioProjectBinCommand,
    b: Budget,
    token: &CancellationToken,
) -> std::result::Result<StudioProjectBinOutcome, StudioProjectBinError> {
    use StudioProjectBinCommand as C;
    use StudioProjectBinError as E;
    let c = ctx.command().ok_or(E::CommandMissing)?;
    if c.as_str() != cmd.command_id() {
        return Err(E::CommandMismatch);
    }
    if ctx.base_revision() != revision {
        return Err(E::StaleRevision { expected: ctx.base_revision(), current: revision });
    }
    let held = |d: Diagnostic| match d.code {
        Code::Canceled => E::Canceled,
        Code::BudgetExceeded => E::BudgetExceeded,
        code => E::InvalidInput(code),
    };
    validate_project_bin(bin, document, b, token).map_err(held)?;
    let mut next = bin.clone();
    let folder_exists = |id: &Option<String>| id.as_ref().is_none_or(|f| bin.folder(f).is_some());
    let touched = match cmd {
        C::NewFolder { folder } => {
            if bin.folder(&folder.folder_id).is_some() {
                return Err(E::DuplicateFolder);
            }
            if !folder_exists(&folder.parent_id) {
                return Err(E::FolderMissing);
            }
            if bin.folders.len() >= MAX_BIN_FOLDERS {
                return Err(E::BudgetExceeded);
            }
            next.folders.push(folder.clone());
            1
        }
        C::Import { items } => {
            if items.is_empty() {
                return Err(E::EmptyOrRepeated);
            }
            if items.len() > MAX_BIN_ITEMS.saturating_sub(bin.media_items.len()) {
                return Err(E::BudgetExceeded);
            }
            for (n, i) in items.iter().enumerate() {
                if n.is_multiple_of(CANCEL_STRIDE) {
                    token.check().map_err(|_| E::Canceled)?;
                }
                item_ok(i).map_err(|d| E::InvalidItem(d.code))?;
                if !folder_exists(&i.folder_id) {
                    return Err(E::FolderMissing);
                }
                let ClipSource::Media { content_digest, .. } = &i.source else { return Err(E::InvalidItem(Code::InvalidField)) };
                let repeated = next.item(&i.item_id).is_some() || next.item_of_digest(content_digest).is_some();
                if repeated {
                    return Err(E::DuplicateItem);
                }
                next.media_items.push(i.clone());
            }
            items.len()
        }
        C::Move { item_ids, folder_id } => {
            if !distinct(item_ids) {
                return Err(E::EmptyOrRepeated);
            }
            if !folder_exists(folder_id) {
                return Err(E::FolderMissing);
            }
            for item_id in item_ids {
                let item = next.media_items.iter_mut().find(|i| &i.item_id == item_id).ok_or(E::ItemMissing)?;
                item.folder_id.clone_from(folder_id);
            }
            item_ids.len()
        }
        C::Remove { item_ids } => {
            if !distinct(item_ids) {
                return Err(E::EmptyOrRepeated);
            }
            // By content digest (review F6): a clip of the same bytes under another manifest handle or
            // asset_id cuts from this item too.
            let digest_of = |s: &ClipSource| match s {
                ClipSource::Media { content_digest, .. } => Some(content_digest.clone()),
                _ => None,
            };
            let used: Vec<ContentDigest> = document.sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()).filter_map(|c| digest_of(&c.source_ref)).collect();
            for item_id in item_ids {
                token.check().map_err(|_| E::Canceled)?;
                let at = next.media_items.iter().position(|i| &i.item_id == item_id).ok_or(E::ItemMissing)?;
                if digest_of(&next.media_items[at].source).is_some_and(|d| used.contains(&d)) {
                    return Err(E::ItemInUse);
                }
                next.media_items.remove(at);
            }
            item_ids.len()
        }
    };
    let changed = next != *bin;
    if changed {
        validate_project_bin(&next, document, b, token).map_err(|d| match d.code {
            Code::Canceled => E::Canceled,
            Code::BudgetExceeded => E::BudgetExceeded,
            code if matches!(cmd, C::NewFolder { .. }) => E::InvalidFolder(code),
            code => E::InvalidItem(code),
        })?;
        let bytes = serde_json::to_vec(&next).map_err(|_| E::InvalidInput(Code::InvalidJson))?;
        if bytes.len() > MAX_PROJECT_BIN_INPUT_BYTES.min(b.input_bytes) {
            return Err(E::BudgetExceeded);
        }
    }
    let report = StudioProjectBinReport {
        command: cmd.command_id(),
        base_revision: ctx.base_revision(),
        correlation: ctx.correlation().value(),
        folders: next.folders.len(),
        media_items: next.media_items.len(),
        touched,
        changed,
    };
    Ok(StudioProjectBinOutcome { bin: next, report })
}

// ---------------------------------------------------------------------------------------------
// Sequences and tracks.

/// A track at the record defaults: enabled, unlocked, sync-locked, not muted or soloed, unit gain,
/// centre pan; `targeted` as given. `index` is set by [`with_tracks`].
fn track(kind: TrackKind, track_id: &str, name: &str, targeted: bool) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: track_id.to_owned(),
        kind,
        index: 0,
        name: name.to_owned(),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted,
        muted: false,
        soloed: false,
        clips: Vec::new(),
        volume: 1.0,
        pan: 0.0,
    }
}

/// `seq` with each `(kind, track_id, name)` appended after the last track of its kind (at the end when
/// it has none), and every kind's `index` renumbered in stack order. `InvalidField` for a name that is
/// not free of control characters or over [`MAX_BIN_TEXT_BYTES`]; ids and counts are the commit's to
/// validate (`STRK` grammar, uniqueness, `MAX_TRACKS_PER_SEQUENCE`).
pub fn with_tracks(seq: &StudioSequence, tracks: &[(TrackKind, String, String)]) -> Result<StudioSequence> {
    let mut next = seq.clone();
    if tracks.len() > super::MAX_TRACKS_PER_SEQUENCE.saturating_sub(seq.tracks.len()) {
        return Err(err(Code::BudgetExceeded, format!("{}/tracks", seq.sequence_id)));
    }
    for (kind, track_id, name) in tracks {
        if !text_ok(name) {
            return Err(err(Code::InvalidField, format!("{track_id}/name")));
        }
        let at = next.tracks.iter().rposition(|t| t.kind == *kind).map_or(next.tracks.len(), |i| i.saturating_add(1));
        let first = !next.tracks.iter().any(|t| t.kind == *kind);
        next.tracks.insert(at, track(*kind, track_id, name, first));
    }
    let mut stack = [0u32; 4];
    for t in &mut next.tracks {
        let slot = match t.kind {
            TrackKind::Video => 0,
            TrackKind::Audio => 1,
            TrackKind::Caption => 2,
            TrackKind::Submix => 3,
        };
        t.index = stack[slot];
        stack[slot] = stack[slot].saturating_add(1);
    }
    Ok(next)
}

/// A new empty sequence named `name` at the preset's frame rate, holding the preset's initial video
/// tracks `V1..` then audio tracks `A1..` (STU-VID-016a counts), and the settings entry that goes with
/// it (editing mode `custom`). `track_ids` gives the ids, video first; its length must be the two
/// counts' sum (`InvalidField`).
pub fn sequence_from_settings(
    sequence_id: &str,
    name: &str,
    settings: &StudioSequenceSettings,
    track_ids: &[String],
) -> Result<(StudioSequence, StudioSequenceSettingsEntry)> {
    let (video, audio) = (usize::from(settings.initial_video_track_count), usize::from(settings.initial_audio_track_count));
    if track_ids.len() != video.saturating_add(audio) {
        return Err(err(Code::InvalidField, format!("{sequence_id}/tracks")));
    }
    if !key(sequence_id) {
        return Err(err(Code::InvalidId, "sequence_id"));
    }
    if !text_ok(name) {
        return Err(err(Code::InvalidField, format!("{sequence_id}/name")));
    }
    let empty = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: sequence_id.to_owned(),
        name: name.to_owned(),
        frame_rate: settings.frame_rate,
        duration: Ticks::new(0),
        tracks: Vec::new(),
        markers: Vec::new(),
        work_area: None,
    };
    let (vids, auds) = track_ids.split_at(video);
    let mut tracks: Vec<(TrackKind, String, String)> = vids.iter().enumerate().map(|(n, id)| (TrackKind::Video, id.clone(), format!("V{}", n.saturating_add(1)))).collect();
    tracks.extend(auds.iter().enumerate().map(|(n, id)| (TrackKind::Audio, id.clone(), format!("A{}", n.saturating_add(1)))));
    let seq = with_tracks(&empty, &tracks)?;
    let entry = StudioSequenceSettingsEntry { sequence_id: sequence_id.to_owned(), editing_mode: CUSTOM_EDITING_MODE.to_owned(), settings: settings.clone() };
    Ok((seq, entry))
}

/// `seq` with `clip` on track `track_id`, kept in start order, and the sequence duration moved to the
/// new content end. `DanglingReference` for an unknown track, `InvalidField` when the clip overlaps
/// one already there or its end overflows; the rest is the commit's to validate.
pub fn with_clip(seq: &StudioSequence, track_id: &str, clip: StudioClip) -> Result<StudioSequence> {
    let mut next = seq.clone();
    let t = next.tracks.iter_mut().find(|t| t.track_id == track_id).ok_or_else(|| err(Code::DanglingReference, track_id.to_owned()))?;
    let start = clip.timeline_start.value();
    let end = clip.timeline_end().ok_or_else(|| err(Code::InvalidField, format!("{}/timeline_end", clip.clip_id)))?.value();
    let overlaps = t.clips.iter().any(|c| c.timeline_end().is_none_or(|e| c.timeline_start.value() < end && start < e.value()));
    if overlaps {
        return Err(err(Code::InvalidField, format!("{}/overlap", clip.clip_id)));
    }
    let at = t.clips.iter().position(|c| c.timeline_start.value() > start).unwrap_or(t.clips.len());
    t.clips.insert(at, clip);
    next.duration = next.content_end().ok_or_else(|| err(Code::InvalidField, format!("{}/duration", seq.sequence_id)))?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsk_studio_accord::{ActorContext, ClientSessionId, CommandId, CorrelationId};

    const DOC: &str = "SDOC-019abcde-0000-7000-8000-0000000000b1";

    fn document() -> StudioDocument {
        serde_json::from_value(serde_json::json!({
            "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
            "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
            "artboards":[],"page_spreads":[],"layers":[],
            "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
            "sequences":[]
        }))
        .unwrap()
    }

    fn token() -> CancellationToken {
        CancellationToken::default()
    }

    fn item(n: u32) -> StudioMediaItem {
        let hex = format!("{n:064x}");
        StudioMediaItem {
            item_id: format!("item-{n}"),
            name: format!("take_{n}.wav"),
            folder_id: None,
            source: ClipSource::Media { artifact_manifest_id: format!("sha256:{hex}"), content_digest: ContentDigest { algorithm: "sha256".into(), digest: hex }, asset_id: None },
            kind: StudioMediaKind::Audio,
            format: "wav-pcm".into(),
            duration: 1000,
            frame_rate: None,
            width: None,
            height: None,
            sample_rate_hz: Some(48_000),
            channels: Some(2),
        }
    }

    fn folder(id: &str, parent: Option<&str>) -> StudioBinFolder {
        StudioBinFolder { folder_id: id.into(), name: id.into(), parent_id: parent.map(str::to_owned) }
    }

    fn code(bin: &StudioProjectBin) -> Option<Code> {
        validate_project_bin(bin, &document(), Budget::default(), &token()).err().map(|d| d.code)
    }

    fn ctx(command: &str) -> CommandContext {
        let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
        CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), 7, CorrelationId::new(41), None)
    }

    fn run(bin: &StudioProjectBin, cmd: &StudioProjectBinCommand) -> std::result::Result<StudioProjectBinOutcome, StudioProjectBinError> {
        apply_project_bin_command(&document(), bin, &ctx(cmd.command_id()), 7, cmd, Budget::default(), &token())
    }

    /// V4-02: list lengths are counted before any element is walked, nesting is bounded, and a wire form
    /// over the hard byte bound is refused before parsing.
    #[test]
    fn counts_depth_and_wire_bytes_are_bounded_before_the_walk() {
        let mut bin = StudioProjectBin::new(DOC);
        bin.media_items = (0..u32::try_from(MAX_BIN_ITEMS).unwrap() + 1).map(item).collect();
        assert_eq!(code(&bin), Some(Code::BudgetExceeded));
        let mut bin = StudioProjectBin::new(DOC);
        bin.folders = (0..=MAX_BIN_DEPTH).map(|n| folder(&format!("f{n}"), n.checked_sub(1).map(|p| format!("f{p}")).as_deref())).collect();
        assert_eq!(code(&bin), Some(Code::BudgetExceeded), "a chain deeper than MAX_BIN_DEPTH");
        bin.folders.truncate(MAX_BIN_DEPTH);
        assert_eq!(code(&bin), None);
        let huge = vec![b' '; MAX_PROJECT_BIN_INPUT_BYTES + 1];
        assert_eq!(parse_project_bin(&huge, &document(), Budget::default(), &token()).err().map(|d| d.code), Some(Code::BudgetExceeded));
    }

    /// Cycles, dangling parents and folders, repeated ids and bytes, and fields of the wrong kind are
    /// typed refusals.
    #[test]
    fn folder_tree_identity_and_kind_fields_are_validated() {
        let mut bin = StudioProjectBin::new(DOC);
        bin.folders = vec![folder("a", Some("b")), folder("b", Some("a"))];
        assert_eq!(code(&bin), Some(Code::ContainmentCycle));
        bin.folders = vec![folder("a", Some("gone"))];
        assert_eq!(code(&bin), Some(Code::DanglingReference));
        bin.folders = vec![folder("a", None)];
        let mut one = item(1);
        one.folder_id = Some("zz".into());
        bin.media_items = vec![one];
        assert_eq!(code(&bin), Some(Code::DanglingReference));
        let mut twin = item(2);
        twin.source = item(1).source;
        bin.media_items = vec![item(1), twin];
        assert_eq!(code(&bin), Some(Code::DuplicateIdentity), "one item per digest");
        let mut wrong = item(3);
        wrong.width = Some(16);
        bin.media_items = vec![wrong];
        assert_eq!(code(&bin), Some(Code::InvalidField), "an audio item with a raster");
        let mut path = item(4);
        path.source = ClipSource::Media { artifact_manifest_id: "cards/a.wav".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab".into() }, asset_id: None };
        bin.media_items = vec![path];
        assert_eq!(code(&bin), Some(Code::InvalidField), "a path is not a handle");
        let mut nested = item(5);
        nested.source = ClipSource::Sequence { sequence_id: "seq-1".into() };
        bin.media_items = vec![nested];
        assert_eq!(code(&bin), Some(Code::InvalidField));
    }

    /// The commands: all-or-none import, moves, an item in use stays, and the context must fit.
    #[test]
    fn commands_are_atomic_and_checked_against_their_context() {
        let empty = StudioProjectBin::new(DOC);
        let added = run(&empty, &StudioProjectBinCommand::NewFolder { folder: folder("sound", None) }).unwrap();
        assert_eq!((added.report.folders, added.report.changed), (1, true));
        let bin = added.bin;
        assert_eq!(run(&bin, &StudioProjectBinCommand::NewFolder { folder: folder("sound", None) }), Err(StudioProjectBinError::DuplicateFolder));
        let mut twin = item(2);
        twin.source = item(1).source;
        let refused = run(&bin, &StudioProjectBinCommand::Import { items: vec![item(1), twin] });
        assert_eq!(refused, Err(StudioProjectBinError::DuplicateItem), "nothing of the batch lands");
        let bin = run(&bin, &StudioProjectBinCommand::Import { items: vec![item(1), item(2)] }).unwrap().bin;
        let moved = run(&bin, &StudioProjectBinCommand::Move { item_ids: vec!["item-1".into()], folder_id: Some("sound".into()) }).unwrap();
        assert_eq!(moved.bin.item("item-1").unwrap().folder_id.as_deref(), Some("sound"));
        assert_eq!(run(&bin, &StudioProjectBinCommand::Move { item_ids: vec!["item-1".into(), "item-1".into()], folder_id: None }), Err(StudioProjectBinError::EmptyOrRepeated));
        assert_eq!(run(&bin, &StudioProjectBinCommand::Remove { item_ids: vec!["item-9".into()] }), Err(StudioProjectBinError::ItemMissing));
        let removed = run(&bin, &StudioProjectBinCommand::Remove { item_ids: vec!["item-2".into()] }).unwrap();
        assert_eq!(removed.report.media_items, 1);
        let wrong = apply_project_bin_command(&document(), &bin, &ctx("project_bin.move"), 7, &StudioProjectBinCommand::Remove { item_ids: vec!["item-2".into()] }, Budget::default(), &token());
        assert_eq!(wrong, Err(StudioProjectBinError::CommandMismatch));
        let stale = apply_project_bin_command(&document(), &bin, &ctx("project_bin.remove"), 8, &StudioProjectBinCommand::Remove { item_ids: vec!["item-2".into()] }, Budget::default(), &token());
        assert_eq!(stale, Err(StudioProjectBinError::StaleRevision { expected: 7, current: 8 }));
    }

    /// Tracks join their kind's stack and every kind is renumbered; a preset sequence holds its counts.
    #[test]
    fn tracks_join_their_kind_and_a_preset_sequence_holds_its_counts() {
        let preset = crate::shipped_sequence_presets().presets.into_iter().next().unwrap();
        let (v, a) = (usize::from(preset.settings.initial_video_track_count), usize::from(preset.settings.initial_audio_track_count));
        let ids: Vec<String> = (0..v + a).map(|n| format!("STRK-019abcde-0000-7000-8000-{n:012x}")).collect();
        assert!(sequence_from_settings("seq-1", "main", &preset.settings, &ids[1..]).is_err(), "one id per track");
        let (seq, entry) = sequence_from_settings("seq-1", "main", &preset.settings, &ids).unwrap();
        assert_eq!((seq.tracks.len(), entry.sequence_id.as_str(), seq.frame_rate), (v + a, "seq-1", preset.settings.frame_rate));
        let grown = with_tracks(&seq, &[(TrackKind::Video, "STRK-x".into(), "V9".into()), (TrackKind::Submix, "STRK-y".into(), "SUB".into())]).unwrap();
        assert_eq!(grown.tracks[v].track_id, "STRK-x", "after the last video track");
        assert_eq!(grown.tracks[v].index, u32::try_from(v).unwrap());
        assert_eq!((grown.tracks.last().unwrap().kind, grown.tracks.last().unwrap().index), (TrackKind::Submix, 0));
        assert!(with_tracks(&seq, &[(TrackKind::Audio, "STRK-z".into(), "bad\nname".into())]).is_err());
    }
}
