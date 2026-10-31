//! Save a selection to a persistent channel and load a channel as a selection (lane
//! SMB-FOLIO-CHANNEL-RECORD; STU-RAS-022, STU-RAS-031, STU-RAS-144; MT-038, MT-30095, MT-050).
//! The channel is the Folio record `StudioRasterRecords.channels` ([`StudioChannelRecord`]):
//! save/load selection, alpha channels and the Channels surface are that one record. Pure CPU
//! sample conversion: no device, no GPU path, no ports.
//!
//! # Polarity (Studio reading; Photoshop behaviour UNVERIFIED, D5)
//!
//! Channel samples are the tone the Channels panel shows and an interchange file stores (`0` =
//! black, full scale = white). STU-RAS-144: a masked-area alpha stores the masked region and a
//! selected-area alpha the selected region; Studio reads "stores" as the dark, overlay-coloured
//! tone. What each kind loads as a selection (coverage `c`, tone `t`, both on `0..=1`):
//!
//! | kind                  | dark tone indicates | `c` from `t` | `t` from `c` |
//! |-----------------------|---------------------|--------------|--------------|
//! | `masked_area_alpha`   | masked region       | `t`          | `c`          |
//! | `selected_area_alpha` | selected region     | `1 - t`      | `1 - c`      |
//! | `spot_colour`         | ink                 | `1 - t`      | `1 - c`      |
//!
//! Converting a channel between kinds keeps the selection it loads as: the samples are inverted
//! exactly when the two kinds' polarities differ (masked-area <-> selected-area or spot), and the
//! two alpha kinds are never merged (MT-050 step 1, test `kind_conversion_inverts_samples_only_across_polarity`).
//!
//! # Precision
//!
//! A [`CoveragePlane`] holds 16-bit coverage. Depth 16 stores the tone as `u16` little-endian and
//! depth 32 as `f32` little-endian `tone / 65535`; both load back the identical plane. Depth 8
//! stores `round(tone * 255 / 65535)` and loads `tone8 * 257`: an 8-bit channel quantises the
//! selection to 256 levels (declared; test `save_then_load_is_identical_at_16_and_32_bits_and_quantised_at_8`).
//!
//! # Tiles
//!
//! A saved channel covers the document raster `width` x `height` with the cells of a canvas-origin
//! grid of `tile_edge` pixels, each cell one [`ChannelTilePayload`] the host stores under the
//! artifact handle its [`ChannelTileMint`] names (`object_key` `ch_<column>_<row>`, SHA-256 digest
//! over the payload bytes). Samples of an edge cell beyond the extent are zero bytes. Loading checks
//! every tile's size and digest before reading a sample.
//!
//! # Attribution and diagnostics (host integration rules 4 and 5)
//!
//! Every operation takes the caller's [`CommandContext`] (actor, client session, optional command
//! id, base document revision, correlation) and a [`SinkPort`]; it returns a new record set or
//! plane and emits exactly one terminal Observe frame with the outcome only (codes, never channel
//! names or sample data). No command id is registered here: the Channels surface owns its ids (D7).
//! Hostile sizes: every byte count is computed with checked arithmetic and compared with
//! [`SelectionLimits::max_bytes`] before allocating (V4-01/02).
#![deny(clippy::arithmetic_side_effects)]
use crate::Error;
use crate::selection::{CombineMode, CoveragePlane, SelectionError, SelectionLimits, combine};
use crate::tile::Rect;
use hsk_studio_accord::{CancellationToken, CommandContext, DomainId};
use hsk_studio_folio::{
    Code, ContentDigest, Length, GeometryUnit, StudioChannelColour, StudioChannelKind,
    StudioChannelRecord, StudioRasterRecords, TileRef, TileSchema, Unresolved, channel_list_budget, channel_name_ok,
    channel_sample_bytes, channel_tile_format, validate_channels,
};
use hsk_studio_observe::{
    Budget as ObserveBudget, Error as ObserveError, FailureCode, Observation, Observe, Outcome,
    Receipt, SinkPort,
};
use hsk_studio_prism::{hash_hex, profile_hash};
use std::fmt;

const FULL: u16 = u16::MAX;
/// Largest grid cell edge in pixels.
pub const MAX_TILE_EDGE: u32 = 4096;

/// Typed refusal of a channel operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelError {
    /// No channel with that id.
    UnknownChannel,
    /// A channel with the new id already exists.
    DuplicateChannel,
    /// Name longer than `MAX_CHANNEL_NAME_BYTES`.
    NameTooLong,
    /// Empty name or a control character.
    InvalidName,
    /// Reorder target outside the list.
    IndexOutOfRange,
    /// The save extent differs from the existing channels' extent, or a plane cannot cover it.
    ExtentMismatch,
    /// Depth other than 8, 16 or 32.
    DepthUnsupported,
    /// Tile edge outside `1..=MAX_TILE_EDGE`.
    TileEdge,
    /// A tile the record names has no bytes.
    TileMissing,
    /// Tile bytes of the wrong size for one grid cell at the channel depth.
    TileSize,
    /// Tile bytes whose SHA-256 differs from the record digest.
    HashMismatch,
    /// A 32-bit sample that is NaN or infinite.
    Nonfinite,
    /// The conversion would not change the record (same kind).
    NoChange,
    Overflow,
    BudgetExceeded,
    Canceled,
    /// The resulting record set fails Folio validation (code).
    Record(Code),
    Selection(SelectionError),
    /// The document id is not a domain id (no attributed resource to report against).
    InvalidDocument,
}
impl ChannelError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownChannel => "channel_unknown",
            Self::DuplicateChannel => "channel_duplicate",
            Self::NameTooLong => "channel_name_too_long",
            Self::InvalidName => "channel_name_invalid",
            Self::IndexOutOfRange => "channel_index_out_of_range",
            Self::ExtentMismatch => "channel_extent_mismatch",
            Self::DepthUnsupported => "channel_depth_unsupported",
            Self::TileEdge => "channel_tile_edge",
            Self::TileMissing => "channel_tile_missing",
            Self::TileSize => "channel_tile_size",
            Self::HashMismatch => "channel_hash_mismatch",
            Self::Nonfinite => "channel_nonfinite",
            Self::NoChange => "channel_no_change",
            Self::Overflow => "overflow",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
            Self::Record(_) => "channel_record_invalid",
            Self::Selection(e) => e.code(),
            Self::InvalidDocument => "channel_document_invalid",
        }
    }
    fn outcome(self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::DepthUnsupported => Outcome::Failure(FailureCode::Unsupported),
            Self::TileMissing => Outcome::Failure(FailureCode::Unavailable),
            _ => Outcome::Failure(FailureCode::Validation),
        }
    }
}
impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ChannelError {}
impl From<Error> for ChannelError {
    fn from(e: Error) -> Self {
        match e {
            Error::Overflow => Self::Overflow,
            Error::BudgetExceeded => Self::BudgetExceeded,
            Error::Canceled => Self::Canceled,
            other => Self::Selection(SelectionError::Pigment(other)),
        }
    }
}
impl From<SelectionError> for ChannelError {
    fn from(e: SelectionError) -> Self {
        match e {
            SelectionError::Pigment(p) => p.into(),
            other => Self::Selection(other),
        }
    }
}

/// An operation's result plus the delivery of its one terminal Observe frame.
#[derive(Debug)]
pub struct Attributed<T> {
    pub result: Result<T, ChannelError>,
    pub delivery: Result<Receipt, ObserveError>,
}

/// Emits the outcome of `result` once: codes only, the caller's actor and correlation, the command's
/// base revision, the document as the resource.
fn finish<T>(
    result: Result<T, ChannelError>,
    document_id: &str,
    cmd: &CommandContext,
    sink: &mut (impl SinkPort + ?Sized),
) -> Attributed<T> {
    let (result, resource) = match DomainId::parse(document_id) {
        Ok(d) => (result, Ok(d)),
        // No attributable resource: nothing is emitted, the frame is reported undeliverable.
        Err(_) => (Err(ChannelError::InvalidDocument), Err(ObserveError::InvalidDetail)),
    };
    let outcome = match &result {
        Ok(_) => Outcome::Success,
        Err(e) => e.outcome(),
    };
    let delivery = resource.and_then(|resource| {
        let correlation = cmd.correlation().value();
        let revision = cmd.base_revision();
        let mut observe = Observe::new(correlation, revision, resource, cmd.actor().clone(), ObserveBudget::new(0, 0)?);
        let event = Observation {
            correlation_id: correlation,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        };
        // A fresh token: the operation decided its outcome; a later cancel must not recast it.
        observe.emit(event, &CancellationToken::default(), sink)
    });
    Attributed { result, delivery }
}

/// Coverage (selection) of a 16-bit tone under `kind`'s polarity.
pub const fn coverage_from_tone(kind: StudioChannelKind, tone: u16) -> u16 {
    match kind {
        StudioChannelKind::MaskedAreaAlpha => tone,
        StudioChannelKind::SelectedAreaAlpha | StudioChannelKind::SpotColour => FULL.wrapping_sub(tone),
    }
}

/// Tone that loads as `coverage` under `kind`'s polarity (the inverse of [`coverage_from_tone`]).
pub const fn tone_from_coverage(kind: StudioChannelKind, coverage: u16) -> u16 {
    coverage_from_tone(kind, coverage)
}

/// True when `a` and `b` load the same tone as different selections.
pub const fn polarity_differs(a: StudioChannelKind, b: StudioChannelKind) -> bool {
    matches!(a, StudioChannelKind::MaskedAreaAlpha) != matches!(b, StudioChannelKind::MaskedAreaAlpha)
}

/// The bytes of one stored grid cell, for the host to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelTilePayload {
    pub channel_id: String,
    pub object_key: String,
    pub column: i64,
    pub row: i64,
    /// `coverage_u8`, `coverage_u16le` or `coverage_f32le`.
    pub format: &'static str,
    pub bytes: Vec<u8>,
    /// Lowercase SHA-256 hex over `bytes` (the record's `content_digest`).
    pub sha256_hex: String,
}

/// Where the host keeps tile bytes: the artifact handle a new payload is stored under. Folio and
/// Pigment mint no artifact handles.
pub trait ChannelTileMint {
    fn artifact_manifest_id(&mut self, channel_id: &str, object_key: &str, sha256_hex: &str) -> String;
}

/// Bytes of a stored tile, resolved by the host from the record's `TileRef`.
pub trait ChannelTileBytes {
    fn bytes(&self, tile: &TileRef) -> Option<&[u8]>;
}
/// Payloads returned by an earlier operation serve their own digests.
impl ChannelTileBytes for [ChannelTilePayload] {
    fn bytes(&self, tile: &TileRef) -> Option<&[u8]> {
        self.iter()
            .find(|p| p.sha256_hex == tile.content_digest.digest)
            .map(|p| p.bytes.as_slice())
    }
}

/// A new record set plus the payloads the host stores before committing it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelEdit {
    pub records: StudioRasterRecords,
    pub payloads: Vec<ChannelTilePayload>,
}

/// Grid and storage parameters of a save.
#[derive(Clone, Copy, Debug)]
pub struct ChannelGrid<'a> {
    /// Document raster width and height in pixels (the channel extent).
    pub width: u32,
    pub height: u32,
    /// Cell edge in pixels (`1..=MAX_TILE_EDGE`; the PSD importer's grid is 256).
    pub tile_edge: u32,
    /// `SCPF` id recorded on every tile.
    pub colour_profile_id: &'a str,
}

/// Properties of a channel a save creates.
#[derive(Clone, Debug, PartialEq)]
pub struct NewChannel {
    pub channel_id: String,
    pub name: String,
    pub kind: StudioChannelKind,
    pub colour: StudioChannelColour,
    pub opacity: f64,
    pub depth: u8,
}

/// Where a save writes.
#[derive(Clone, Debug, PartialEq)]
pub enum SaveTarget {
    /// A new channel appended to the list.
    New(NewChannel),
    /// An existing channel: its stored selection combined with the saved one (`Replace`
    /// overwrites; `Extend`, `Diminish`, `Intersect` are the STU-RAS-118 combines).
    Existing { channel_id: String, mode: CombineMode },
}

fn check_name(name: &str) -> Result<(), ChannelError> {
    if name.len() > hsk_studio_folio::MAX_CHANNEL_NAME_BYTES {
        return Err(ChannelError::NameTooLong);
    }
    if !channel_name_ok(name) {
        return Err(ChannelError::InvalidName);
    }
    Ok(())
}

fn validated(records: StudioRasterRecords, cancel: &CancellationToken) -> Result<StudioRasterRecords, ChannelError> {
    let budget = channel_list_budget(&records.channels);
    validate_channels(&records.channels, budget, cancel, &Unresolved).map_err(|d| match d.code {
        Code::Canceled => ChannelError::Canceled,
        code => ChannelError::Record(code),
    })?;
    Ok(records)
}

/// Cells per side and the byte size of one cell, all checked against the budget before allocating.
struct Layout {
    cols: u32,
    rows: u32,
    edge: u32,
    sample_bytes: usize,
    cell_bytes: usize,
}
impl Layout {
    fn new(width: u32, height: u32, edge: u32, depth: u8, limits: &SelectionLimits) -> Result<Self, ChannelError> {
        if !(1..=MAX_TILE_EDGE).contains(&edge) {
            return Err(ChannelError::TileEdge);
        }
        if width == 0 || height == 0 || width > hsk_studio_folio::MAX_CHANNEL_SIDE || height > hsk_studio_folio::MAX_CHANNEL_SIDE {
            return Err(ChannelError::ExtentMismatch);
        }
        let sample_bytes = channel_sample_bytes(depth).ok_or(ChannelError::DepthUnsupported)?;
        let cols = width.div_ceil(edge);
        let rows = height.div_ceil(edge);
        let cell_samples = u64::from(edge).checked_mul(u64::from(edge)).ok_or(ChannelError::Overflow)?;
        let cell = cell_samples
            .checked_mul(sample_bytes as u64)
            .ok_or(ChannelError::Overflow)?;
        let total = u64::from(cols)
            .checked_mul(u64::from(rows))
            .and_then(|n| n.checked_mul(cell))
            .ok_or(ChannelError::Overflow)?;
        if total > limits.max_bytes {
            return Err(ChannelError::BudgetExceeded);
        }
        Ok(Self {
            cols,
            rows,
            edge,
            sample_bytes,
            cell_bytes: usize::try_from(cell).map_err(|_| ChannelError::Overflow)?,
        })
    }
    /// Bytes of one cell row (`edge * sample_bytes`; bounded by `cell_bytes`).
    fn row_bytes(&self) -> usize {
        (self.edge as usize).saturating_mul(self.sample_bytes)
    }
    fn tiles(&self) -> usize {
        (self.cols as usize).saturating_mul(self.rows as usize)
    }
}

fn encode_tone(out: &mut [u8], tone: u16, depth: u8) {
    match depth {
        8 => {
            let v = u32::from(tone).saturating_mul(255).saturating_add(32767) / 65535;
            out[0] = u8::try_from(v).unwrap_or(u8::MAX);
        }
        16 => out.copy_from_slice(&tone.to_le_bytes()),
        _ => out.copy_from_slice(&(f32::from(tone) / 65535.0).to_le_bytes()),
    }
}

fn decode_tone(sample: &[u8], depth: u8) -> Result<u16, ChannelError> {
    Ok(match depth {
        8 => u16::from(sample[0]).saturating_mul(257),
        16 => u16::from_le_bytes([sample[0], sample[1]]),
        _ => {
            let f = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
            if !f.is_finite() {
                return Err(ChannelError::Nonfinite);
            }
            let v = (f64::from(f.clamp(0.0, 1.0)) * 65535.0).round();
            // Clamped to 0..=1 above, so 0..=65535: exact.
            v as u16
        }
    })
}

/// Encodes `tone(x, y)` over the extent into grid payloads and the matching tile references.
fn encode_tiles(
    channel_id: &str,
    depth: u8,
    grid: &ChannelGrid<'_>,
    layout: &Layout,
    mint: &mut dyn ChannelTileMint,
    cancel: &CancellationToken,
    mut tone: impl FnMut(i64, i64) -> u16,
) -> Result<(Vec<TileRef>, Vec<ChannelTilePayload>), ChannelError> {
    let format = channel_tile_format(depth).ok_or(ChannelError::DepthUnsupported)?;
    let mut refs = Vec::with_capacity(layout.tiles());
    let mut payloads = Vec::with_capacity(layout.tiles());
    let edge = i64::from(layout.edge);
    let (width, height) = (i64::from(grid.width), i64::from(grid.height));
    for row in 0..layout.rows {
        for column in 0..layout.cols {
            cancel.check().map_err(|_| ChannelError::Canceled)?;
            let mut bytes = vec![0u8; layout.cell_bytes];
            let (x0, y0) = (i64::from(column).saturating_mul(edge), i64::from(row).saturating_mul(edge));
            let row_bytes = layout.row_bytes();
            for (dy, line) in bytes.chunks_exact_mut(row_bytes).enumerate() {
                let y = y0.saturating_add(dy as i64);
                for (dx, sample) in line.chunks_exact_mut(layout.sample_bytes).enumerate() {
                    let x = x0.saturating_add(dx as i64);
                    if x < width && y < height {
                        encode_tone(sample, tone(x, y), depth);
                    }
                }
            }
            let sha = hash_hex(&profile_hash(&bytes));
            let object_key = format!("ch_{column}_{row}");
            let px = |v: u32| Length {
                value: f64::from(v),
                unit: GeometryUnit::Px,
            };
            refs.push(TileRef {
                object_key: object_key.clone(),
                layer_id: channel_id.to_owned(),
                column: i64::from(column),
                row: i64::from(row),
                width: px(layout.edge),
                height: px(layout.edge),
                format: format.to_owned(),
                colour_profile_id: grid.colour_profile_id.to_owned(),
                schema_id: TileSchema::V1,
                content_digest: ContentDigest {
                    algorithm: "sha256".to_owned(),
                    digest: sha.clone(),
                },
                artifact_manifest_id: mint.artifact_manifest_id(channel_id, &object_key, &sha),
            });
            payloads.push(ChannelTilePayload {
                channel_id: channel_id.to_owned(),
                object_key,
                column: i64::from(column),
                row: i64::from(row),
                format,
                bytes,
                sha256_hex: sha,
            });
        }
    }
    Ok((refs, payloads))
}

/// The channel's stored tones as a full-extent 16-bit plane (`bounds` = the extent). Every tile's
/// size and digest are checked before any sample is read.
fn decode_tones(
    channel: &StudioChannelRecord,
    tiles: &(impl ChannelTileBytes + ?Sized),
    limits: &SelectionLimits,
    cancel: &CancellationToken,
) -> Result<CoveragePlane, ChannelError> {
    let bounds = Rect {
        x: 0,
        y: 0,
        width: channel.width,
        height: channel.height,
    };
    let plane_bytes = bounds.area().checked_mul(2).ok_or(ChannelError::Overflow)?;
    if plane_bytes > limits.max_bytes {
        return Err(ChannelError::BudgetExceeded);
    }
    let first = channel.tiles.first().ok_or(ChannelError::TileMissing)?;
    let edge = first.width.value;
    if !(edge >= 1.0 && edge <= f64::from(MAX_TILE_EDGE) && edge.fract() == 0.0) || first.height.value != edge {
        return Err(ChannelError::TileEdge);
    }
    // Integral and in range: exact.
    let edge = edge as u32;
    let layout = Layout::new(channel.width, channel.height, edge, channel.depth, &SelectionLimits {
        max_bytes: u64::MAX,
        ..*limits
    })?;
    let mut resolved = Vec::with_capacity(channel.tiles.len());
    for t in &channel.tiles {
        cancel.check().map_err(|_| ChannelError::Canceled)?;
        let bytes = tiles.bytes(t).ok_or(ChannelError::TileMissing)?;
        if bytes.len() != layout.cell_bytes {
            return Err(ChannelError::TileSize);
        }
        if hash_hex(&profile_hash(bytes)) != t.content_digest.digest {
            return Err(ChannelError::HashMismatch);
        }
        resolved.push((t, bytes));
    }
    let n = usize::try_from(bounds.area()).map_err(|_| ChannelError::Overflow)?;
    let mut data = vec![0u16; n];
    let e = i64::from(edge);
    let (width, height) = (i64::from(channel.width), i64::from(channel.height));
    for (t, bytes) in resolved {
        cancel.check().map_err(|_| ChannelError::Canceled)?;
        let (x0, y0) = (t.column.saturating_mul(e), t.row.saturating_mul(e));
        for (dy, line) in bytes.chunks_exact(layout.row_bytes()).enumerate() {
            let y = y0.saturating_add(dy as i64);
            for (dx, sample) in line.chunks_exact(layout.sample_bytes).enumerate() {
                let x = x0.saturating_add(dx as i64);
                // Every sample is decoded (a non-finite padding sample is refused too).
                let tone = decode_tone(sample, channel.depth)?;
                if (0..width).contains(&x) && (0..height).contains(&y) {
                    let at = usize::try_from(y.saturating_mul(width).saturating_add(x)).map_err(|_| ChannelError::Overflow)?;
                    *data.get_mut(at).ok_or(ChannelError::Overflow)? = tone;
                }
            }
        }
    }
    Ok(CoveragePlane { bounds, data })
}

/// Loads `channel` as a selection: its tones read through the kind's polarity, combined into
/// `into` with `mode` (`Replace` or no `into` = the channel alone). The plane covers the channel
/// extent. Read only: the record set is not changed.
pub fn load_selection(
    channel: &StudioChannelRecord,
    tiles: &(impl ChannelTileBytes + ?Sized),
    into: Option<&CoveragePlane>,
    mode: CombineMode,
    limits: &SelectionLimits,
    cancel: &CancellationToken,
) -> Result<CoveragePlane, ChannelError> {
    let mut plane = decode_tones(channel, tiles, limits, cancel)?;
    for v in &mut plane.data {
        *v = coverage_from_tone(channel.kind, *v);
    }
    Ok(combine(into, &plane, mode, limits)?)
}

/// [`load_selection`] with attribution: the caller's command context and one terminal Observe
/// frame (outcome code only).
#[allow(clippy::too_many_arguments)]
pub fn load_selection_attributed(
    records: &StudioRasterRecords,
    channel_id: &str,
    tiles: &(impl ChannelTileBytes + ?Sized),
    into: Option<&CoveragePlane>,
    mode: CombineMode,
    limits: &SelectionLimits,
    cmd: &CommandContext,
    cancel: &CancellationToken,
    sink: &mut (impl SinkPort + ?Sized),
) -> Attributed<CoveragePlane> {
    let result = records
        .channel(channel_id)
        .ok_or(ChannelError::UnknownChannel)
        .and_then(|c| load_selection(c, tiles, into, mode, limits, cancel));
    finish(result, &records.document_id, cmd, sink)
}

#[allow(clippy::too_many_arguments)]
fn save(
    records: &StudioRasterRecords,
    plane: &CoveragePlane,
    target: &SaveTarget,
    grid: &ChannelGrid<'_>,
    tiles: &(impl ChannelTileBytes + ?Sized),
    mint: &mut dyn ChannelTileMint,
    limits: &SelectionLimits,
    cancel: &CancellationToken,
) -> Result<ChannelEdit, ChannelError> {
    if let Some(c) = records.channels.first()
        && (c.width, c.height) != (grid.width, grid.height)
    {
        return Err(ChannelError::ExtentMismatch);
    }
    let mut out = records.clone();
    let (record, coverage) = match target {
        SaveTarget::New(n) => {
            check_name(&n.name)?;
            if records.channel(&n.channel_id).is_some() {
                return Err(ChannelError::DuplicateChannel);
            }
            if records.channels.len() >= hsk_studio_folio::MAX_CHANNELS {
                return Err(ChannelError::BudgetExceeded);
            }
            let record = StudioChannelRecord {
                channel_id: n.channel_id.clone(),
                name: n.name.clone(),
                kind: n.kind,
                visible: false,
                colour: n.colour.clone(),
                opacity: n.opacity,
                depth: n.depth,
                width: grid.width,
                height: grid.height,
                tiles: Vec::new(),
            };
            (record, None)
        }
        SaveTarget::Existing { channel_id, mode } => {
            let c = records.channel(channel_id).ok_or(ChannelError::UnknownChannel)?;
            let combined = match mode {
                CombineMode::Replace => None,
                _ => {
                    let stored = load_selection(c, tiles, None, CombineMode::Replace, limits, cancel)?;
                    Some(combine(Some(&stored), plane, *mode, limits)?)
                }
            };
            (c.clone(), combined)
        }
    };
    let layout = Layout::new(grid.width, grid.height, grid.tile_edge, record.depth, limits)?;
    let source = coverage.as_ref().unwrap_or(plane);
    let kind = record.kind;
    let (refs, payloads) = encode_tiles(&record.channel_id, record.depth, grid, &layout, mint, cancel, |x, y| {
        tone_from_coverage(kind, source.at(x, y))
    })?;
    let saved = StudioChannelRecord { tiles: refs, ..record };
    match out.channels.iter_mut().find(|c| c.channel_id == saved.channel_id) {
        Some(slot) => *slot = saved,
        None => out.channels.push(saved),
    }
    Ok(ChannelEdit {
        records: validated(out, cancel)?,
        payloads,
    })
}

/// Saves `plane` (the selection, coverage outside its bounds = 0) to a channel over the document
/// raster `grid.width` x `grid.height`: a new channel appended to the list, or an existing one
/// replaced or combined with its stored selection. Returns a new record set (validated by Folio)
/// and the payloads the host stores; `records` is not changed. The extent must equal the existing
/// channels' extent.
#[allow(clippy::too_many_arguments)]
pub fn save_selection(
    records: &StudioRasterRecords,
    plane: &CoveragePlane,
    target: &SaveTarget,
    grid: &ChannelGrid<'_>,
    tiles: &(impl ChannelTileBytes + ?Sized),
    mint: &mut dyn ChannelTileMint,
    limits: &SelectionLimits,
    cmd: &CommandContext,
    cancel: &CancellationToken,
    sink: &mut (impl SinkPort + ?Sized),
) -> Attributed<ChannelEdit> {
    let result = save(records, plane, target, grid, tiles, mint, limits, cancel);
    finish(result, &records.document_id, cmd, sink)
}
