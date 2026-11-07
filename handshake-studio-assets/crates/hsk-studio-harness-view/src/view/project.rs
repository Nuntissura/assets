//! New Project for video and audio work (lane SMB-PROJECT-BIN-1; STU-VID-010, 016, 021, 024, 050,
//! STU-SHL-113 project-bin): sequence from a preset, the project bin, import with a typed refusal per
//! file, add tracks (submix included), place a bin item on a track, and the source port the timeline,
//! mixer and program monitor bind from the bin of a saved project. Engine and commands only: no draw
//! code (the project-bin panel is a later Vulkan panel lane that calls these methods).
//!
//! # Manual (no-context operator or model)
//!
//! Every method needs an open document (a package; `document.new` of lane SMB-DOC-NEW-1 makes an empty
//! one). Each mutating method is ONE undo step named by its command id, attributed to the viewer's run
//! context, and survives save and reopen; refusals are typed codes and change nothing.
//!
//! - [`Viewer::project_new_sequence`] `(name, preset_id)` -> sequence id (`seq-<n>`): the preset's
//!   settings (stored presets first, then the shipped STU-VID-016a library, e.g.
//!   `legacy-arri-1080p-arri-1080p-24`), its initial video tracks `V1..` and audio tracks `A1..`, and
//!   its settings entry (`sequence.new`). Refusals `unknown_preset`, `presets_invalid`, Run's codes.
//! - [`Viewer::project_add_tracks`] `(sequence_id, kind, names)` -> track ids (`sequence.add_tracks`;
//!   kind `video` | `audio` | `caption` | `submix`, appended after the last track of that kind).
//! - [`Viewer::project_new_folder`] `(name, parent)` -> folder id (`bin-<n>`, `project_bin.new_folder`);
//!   [`Viewer::project_move_items`], [`Viewer::project_remove_items`] (`item_in_use` while a clip cuts
//!   from one).
//! - [`Viewer::project_import`] (feature `media`) `(paths, folder)`: each file is read (at most
//!   [`ImportLimits::max_file_bytes`](super::audio_import::ImportLimits)), classified (audio through
//!   reel's audio import, else video through reel's frame decoder) and staged in the session; every file
//!   read becomes one item (`item-<n>`) and all of them one `project_bin.import` step. A file Studio cannot
//!   read is a row of `refused` with its code (`MEDIA_IMPORT_UNSUPPORTED` naming the detected type,
//!   `already_imported`, `media_read`, the decode code) and the rest of the batch still imports.
//! - [`Viewer::project_place_item`] `(sequence_id, track_id, item_id, start, source_in, source_out)` ->
//!   clip id (`sequence.place_clip`): the item's whole length by default; an audio item goes on an audio
//!   track, a video item on a video track (`track_kind_mismatch`).
//! - [`Viewer::project_bind`] (feature `media`) `(sequence_id, width, height, sample_rate_hz)`: the
//!   program monitor opens on the sequence with every video item's frames (reel) and the bin's lengths,
//!   the export takes every audio item at `sample_rate_hz` as its sound port, and under `audio-batch`
//!   the timeline and mixer bind the same port (`open_multitrack`). An audio item at another rate is
//!   listed in `not_bound` as `audio_rate_mismatch` (conform, STU-VID-053, is not built).
//! - [`Viewer::project_bin_json`] (also `state_json()["project_bin"]`): folders and items with kind,
//!   format, duration ticks, raster, rate, channels and digest. No path is stored or shown.
//!
//! Studio readings (UNVERIFIED against Premiere, D5): one item per content digest (a second import of
//! the same bytes is `already_imported`), bin and command ids, default track names and the targeted
//! first track of each kind. Cuts: stills, the audio of a video file, conform, peak jobs, folder rename
//! and removal.
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::timeline::ReservedIds;
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{
    self as folio, ClipSchema, ClipSource, ClipTransitions, Speed, StudioBinFolder, StudioClip, StudioMediaKind, StudioProjectBin,
    StudioProjectBinCommand, StudioSequenceSettings, TrackKind,
};
use hsk_studio_session::{PROJECT_BIN_ENTRY, SEQUENCE_PRESETS_ENTRY};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Most files one import takes.
pub const MAX_IMPORT_FILES: usize = 256;
/// Most tracks one add-tracks call takes.
pub const MAX_ADD_TRACKS: usize = 64;

/// The kind token of a track (`video`, `audio`, `caption`, `submix`).
pub fn track_kind(token: &str) -> Option<TrackKind> {
    Some(match token {
        "video" => TrackKind::Video,
        "audio" => TrackKind::Audio,
        "caption" => TrackKind::Caption,
        "submix" => TrackKind::Submix,
        _ => return None,
    })
}

/// A key `prefix-<n>` unused by `taken`.
fn mint_key(prefix: &str, taken: &BTreeSet<&str>) -> String {
    let mut n: u64 = 1;
    loop {
        let id = format!("{prefix}-{n}");
        if !taken.contains(id.as_str()) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// `count` fresh ids `<PREFIX>-<document time fields>-7e1e-<block>-<n>`, unused by `used` and `reserved`
/// (the scheme of the timeline's clip and track mints, own blocks: tracks `9002`, clips `9003`).
fn mint_ids(doc: &folio::StudioDocument, prefix: &str, block: &str, used: &BTreeSet<&str>, reserved: &BTreeSet<String>, count: usize) -> Vec<String> {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let hex = |s: Option<&str>, n: usize| s.filter(|v| v.len() == n && v.bytes().all(|b| b.is_ascii_hexdigit())).map_or_else(|| "0".repeat(n), str::to_owned);
    let (high, low) = (hex(uuid.get(0..8), 8), hex(uuid.get(9..13), 4));
    let mut out = Vec::with_capacity(count);
    let mut n: u64 = 1;
    while out.len() < count {
        let id = format!("{prefix}-{high}-{low}-7e1e-{block}-{n:012x}");
        if !used.contains(id.as_str()) && !reserved.contains(&id) && !out.contains(&id) {
            out.push(id);
        }
        n = n.wrapping_add(1);
    }
    out
}

fn track_ids_of(doc: &folio::StudioDocument) -> BTreeSet<&str> {
    doc.sequences.iter().flat_map(|s| s.tracks.iter()).map(|t| t.track_id.as_str()).collect()
}

/// `name` cut to the bin's text bound at a character boundary, control characters replaced.
fn bin_text(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().map(|c| if c.is_control() { '_' } else { c }) {
        if out.len().saturating_add(c.len_utf8()) > folio::MAX_BIN_TEXT_BYTES {
            break;
        }
        out.push(c);
    }
    if out.is_empty() { "untitled".to_owned() } else { out }
}

impl Viewer {
    /// The stored project bin (`None`: none stored, or nothing is open); `Err` when it does not read.
    pub fn project_bin(&self) -> Result<Option<StudioProjectBin>, String> {
        let Some(doc) = self.run.snapshot().map(|s| s.document()) else { return Ok(None) };
        let Some(bytes) = self.run.document_entry(PROJECT_BIN_ENTRY) else { return Ok(None) };
        folio::parse_project_bin(bytes, doc, folio::Budget::default(), self.cancel).map(Some).map_err(|d| format!("project_bin_invalid {:?} {}", d.code, d.target))
    }

    /// The project bin as state (module docs): `null` without one.
    pub fn project_bin_json(&self) -> Value {
        match self.project_bin() {
            Ok(None) => Value::Null,
            Err(e) => json!({"error": e}),
            Ok(Some(bin)) => json!({
                "folders": bin.folders.iter().map(|f| json!({"folder_id": f.folder_id, "name": f.name, "parent_id": f.parent_id})).collect::<Vec<_>>(),
                "media_items": bin.media_items.iter().map(|i| {
                    let digest = match &i.source { ClipSource::Media { content_digest, .. } => content_digest.digest.clone(), _ => String::new() };
                    json!({
                        "item_id": i.item_id, "name": i.name, "folder_id": i.folder_id, "kind": i.kind, "format": i.format,
                        "duration": i.duration, "ticks_per_frame": i.frame_rate.map(|r| r.ticks_per_frame), "width": i.width, "height": i.height,
                        "sample_rate_hz": i.sample_rate_hz, "channels": i.channels, "sha256": digest,
                    })
                }).collect::<Vec<_>>(),
            }),
        }
    }

    fn bin_or_empty(&self) -> Result<StudioProjectBin, String> {
        let doc = self.run.snapshot().map(|s| s.document()).ok_or("not_open")?;
        Ok(self.project_bin()?.unwrap_or_else(|| StudioProjectBin::new(doc.document_id.clone())))
    }

    /// Runs one bin command through the run (one undo step); `Err` is the run's refusal code.
    fn bin_command(&mut self, command: &StudioProjectBinCommand) -> Result<(), String> {
        if self.run.project_bin_command(command) {
            return Ok(());
        }
        Err(self.last_error(command.command_id()))
    }

    fn last_error(&self, fallback: &str) -> String {
        self.run.ops().last().and_then(|o| o.get("error").and_then(Value::as_str)).map_or_else(|| format!("{fallback}_refused"), str::to_owned)
    }

    /// The settings of `preset_id`: a stored preset first, then the shipped library.
    fn preset_settings(&self, preset_id: &str) -> Result<StudioSequenceSettings, String> {
        if let Some(bytes) = self.run.document_entry(SEQUENCE_PRESETS_ENTRY) {
            let stored = folio::parse_sequence_presets(bytes, folio::Budget::default(), self.cancel).map_err(|d| format!("presets_invalid {:?}", d.code))?;
            if let Some(p) = stored.preset(preset_id) {
                return Ok(p.settings.clone());
            }
        }
        folio::shipped_sequence_presets().preset(preset_id).map(|p| p.settings.clone()).ok_or_else(|| "unknown_preset".to_owned())
    }

    /// New sequence `name` from preset `preset_id` (module docs); returns its id.
    pub fn project_new_sequence(&mut self, name: &str, preset_id: &str) -> Result<String, String> {
        #[allow(unreachable_code)]
        if true {
            return Err("not_built".into());
        }
        let settings = self.preset_settings(preset_id)?;
        let doc = self.run.snapshot().map(|s| s.document()).ok_or("not_open")?;
        let reserved = ReservedIds::of(&self.run);
        let sequences: BTreeSet<&str> = doc.sequences.iter().map(|s| s.sequence_id.as_str()).chain(reserved.sequences.iter().map(String::as_str)).collect();
        let sequence_id = mint_key("seq", &sequences);
        let count = usize::from(settings.initial_video_track_count).saturating_add(usize::from(settings.initial_audio_track_count));
        let track_ids = mint_ids(doc, "STRK", "9002", &track_ids_of(doc), &reserved.tracks, count);
        let (sequence, entry) = folio::sequence_from_settings(&sequence_id, &bin_text(name), &settings, &track_ids).map_err(|d| format!("invalid_sequence {:?} {}", d.code, d.target))?;
        if !self.run.new_sequence(sequence, entry) {
            return Err(self.last_error("sequence.new"));
        }
        self.say(format!("project: new sequence {sequence_id} from preset {preset_id}"));
        Ok(sequence_id)
    }

    /// Appends one track of `kind` per name to the sequence (module docs); returns their ids.
    pub fn project_add_tracks(&mut self, sequence_id: &str, kind: TrackKind, names: &[String]) -> Result<Vec<String>, String> {
        if names.is_empty() || names.len() > MAX_ADD_TRACKS {
            return Err("track_count".into());
        }
        let doc = self.run.snapshot().map(|s| s.document()).ok_or("not_open")?;
        let reserved = ReservedIds::of(&self.run);
        let ids = mint_ids(doc, "STRK", "9002", &track_ids_of(doc), &reserved.tracks, names.len());
        let tracks: Vec<(TrackKind, String, String)> = ids.iter().zip(names).map(|(id, name)| (kind, id.clone(), bin_text(name))).collect();
        if !self.run.add_tracks(sequence_id, &tracks) {
            return Err(self.last_error("sequence.add_tracks"));
        }
        Ok(ids)
    }

    /// A new bin folder (module docs); returns its id.
    pub fn project_new_folder(&mut self, name: &str, parent: Option<&str>) -> Result<String, String> {
        let bin = self.bin_or_empty()?;
        let taken: BTreeSet<&str> = bin.folders.iter().map(|f| f.folder_id.as_str()).collect();
        let folder_id = mint_key("bin", &taken);
        let folder = StudioBinFolder { folder_id: folder_id.clone(), name: bin_text(name), parent_id: parent.map(str::to_owned) };
        self.bin_command(&StudioProjectBinCommand::NewFolder { folder })?;
        Ok(folder_id)
    }

    /// Moves bin items into `folder` (`None` = the root).
    pub fn project_move_items(&mut self, item_ids: &[String], folder: Option<&str>) -> Result<(), String> {
        self.bin_command(&StudioProjectBinCommand::Move { item_ids: item_ids.to_vec(), folder_id: folder.map(str::to_owned) })
    }

    /// Removes bin items no clip cuts from.
    pub fn project_remove_items(&mut self, item_ids: &[String]) -> Result<(), String> {
        self.bin_command(&StudioProjectBinCommand::Remove { item_ids: item_ids.to_vec() })
    }

    /// Places bin item `item_id` on track `track_id` at `start` (module docs); returns the clip id.
    pub fn project_place_item(&mut self, sequence_id: &str, track_id: &str, item_id: &str, start: Ticks, source_in: Option<Ticks>, source_out: Option<Ticks>) -> Result<String, String> {
        let bin = self.bin_or_empty()?;
        let item = bin.item(item_id).ok_or("unknown_item")?;
        let doc = self.run.snapshot().map(|s| s.document()).ok_or("not_open")?;
        let seq = doc.sequences.iter().find(|s| s.sequence_id == sequence_id).ok_or("unknown_sequence")?;
        let track = seq.tracks.iter().find(|t| t.track_id == track_id).ok_or("unknown_track")?;
        let fits = matches!((item.kind, track.kind), (StudioMediaKind::Video, TrackKind::Video) | (StudioMediaKind::Audio, TrackKind::Audio));
        if !fits {
            return Err("track_kind_mismatch".into());
        }
        let (a, b) = (source_in.map_or(0, Ticks::value), source_out.map_or(item.duration, Ticks::value));
        if a >= b || b > item.duration {
            return Err("source_window".into());
        }
        let reserved = ReservedIds::of(&self.run);
        let used: BTreeSet<&str> = doc.sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()).map(|c| c.clip_id.as_str()).collect();
        let clip_id = mint_ids(doc, "SCLP", "9003", &used, &reserved.clips, 1).remove(0);
        let clip = StudioClip {
            schema_id: ClipSchema::V1,
            clip_id: clip_id.clone(),
            source_ref: item.source.clone(),
            source_in: Ticks::new(a),
            source_out: Ticks::new(b),
            timeline_start: start,
            duration: Ticks::new(b.saturating_sub(a)),
            speed: Speed::UNIT,
            enabled: true,
            link_group: None,
            markers: Vec::new(),
            transitions: ClipTransitions::default(),
            retime: Default::default(),
        };
        if !self.run.place_clip(sequence_id, track_id, clip) {
            return Err(self.last_error("sequence.place_clip"));
        }
        Ok(clip_id)
    }
}

#[cfg(feature = "media")]
mod media {
    use super::super::Viewer;
    use super::super::audio_import::{ImportLimits, ImportOptions, ImportedAudio, import_audio_external, read_audio_file, sniff};
    use super::super::media::{MediaFrames, ReelFrameDecoder};
    use super::{MAX_IMPORT_FILES, bin_text, mint_key};
    use hsk_studio_accord::Ticks;
    use hsk_studio_folio::{self as folio, ClipSource, ContentDigest, SequenceFrameRate, StudioMediaItem, StudioMediaKind, StudioProjectBinCommand};
    use hsk_studio_pulse::{AudioSources, SourceExtents};
    use hsk_studio_reel as reel;
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// The bin's audio items decoded at one rate: the sound port a timeline, mixer and export bind.
    #[derive(Clone)]
    pub struct BinAudio(Arc<Vec<(ClipSource, Arc<Vec<Vec<f32>>>)>>);
    impl AudioSources for BinAudio {
        fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
            self.0.iter().find(|(s, _)| s == source).map(|(_, p)| p.as_slice())
        }
    }

    /// Every bin item's length in its own ticks, from the bin.
    #[derive(Clone)]
    pub struct BinExtents(Arc<Vec<(ClipSource, Ticks)>>);
    impl SourceExtents for BinExtents {
        fn extent(&self, source: &ClipSource) -> Option<Ticks> {
            self.0.iter().find(|(s, _)| s == source).map(|(_, t)| *t)
        }
    }

    fn media_source(hex: String) -> ClipSource {
        ClipSource::Media { artifact_manifest_id: format!("sha256:{hex}"), content_digest: ContentDigest { algorithm: "sha256".into(), digest: hex }, asset_id: None }
    }

    /// A reel provider or decoder id as a format key.
    fn format_key(id: &str) -> String {
        let s: String = id.chars().take(64).map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
        if s.is_empty() { "unknown".into() } else { s }
    }

    /// What one file is, read without the bin: the item minus its id and folder.
    struct Read {
        kind: StudioMediaKind,
        format: String,
        duration: u64,
        frame_rate: Option<SequenceFrameRate>,
        width: Option<u32>,
        height: Option<u32>,
        sample_rate_hz: Option<u32>,
        channels: Option<u16>,
    }

    impl Viewer {
        fn decode_audio_item(&self, bytes: &[u8]) -> Result<ImportedAudio, (String, Value)> {
            let external = self.audio_decoder.clone();
            import_audio_external(&mut Cursor::new(bytes), &ImportLimits::default(), &ImportOptions::default(), external.as_deref(), self.cancel)
                .map_err(|e| (e.code.to_owned(), json!({"detail": e.detail})))
        }

        /// Classifies one file (module docs); `Err` is its refusal code and detail.
        fn classify(&self, bytes: &[u8], source: &ClipSource) -> Result<Read, (String, Value)> {
            let head = &bytes[..bytes.len().min(64)];
            let provider = reel::Registry::standard().detect(&mut Cursor::new(bytes)).ok().map(|p| format_key(p.descriptor().id));
            if !sniff(head) {
                let mut decoder = ReelFrameDecoder::new();
                if decoder.register(source, Arc::from(bytes)).is_ok()
                    && let Ok((units, ticks_per_unit, width, height)) = decoder.layout(source, self.cancel)
                {
                    let duration = units.checked_mul(ticks_per_unit).filter(|d| *d > 0).ok_or_else(|| ("media_empty".to_owned(), json!({})))?;
                    return Ok(Read {
                        kind: StudioMediaKind::Video,
                        format: provider.unwrap_or_else(|| "video".into()),
                        duration,
                        frame_rate: Some(SequenceFrameRate { ticks_per_frame: ticks_per_unit }),
                        width: Some(width),
                        height: Some(height),
                        sample_rate_hz: None,
                        channels: None,
                    });
                }
            }
            match self.decode_audio_item(bytes) {
                Ok(audio) => {
                    let duration = audio.duration().map_err(|e| (e.code.to_owned(), json!({"detail": e.detail})))?.value();
                    Ok(Read {
                        kind: StudioMediaKind::Audio,
                        format: provider.unwrap_or_else(|| format_key(audio.decoder_id)),
                        duration,
                        frame_rate: None,
                        width: None,
                        height: None,
                        sample_rate_hz: Some(audio.sample_rate),
                        channels: Some(audio.channels_in_file.min(folio::MAX_BIN_CHANNELS)),
                    })
                }
                // A file that sniffed as audio and did not decode keeps its decode code.
                Err(refusal) if sniff(head) => Err(refusal),
                Err(_) => Err(("MEDIA_IMPORT_UNSUPPORTED".to_owned(), json!({"detected": reel::detect_type(head)}))),
            }
        }

        /// Imports `paths` into the bin, into `folder` (module docs). Returns `{imported: [{item_id,
        /// name, kind}], refused: [{name, code, ...}]}`; `Err` only when nothing could be tried
        /// (`not_open`, `too_many_files`, `no_files`) or the one bin step is refused.
        pub fn project_import(&mut self, paths: &[PathBuf], folder: Option<&str>) -> Result<Value, String> {
            if paths.is_empty() {
                return Err("no_files".into());
            }
            if paths.len() > MAX_IMPORT_FILES {
                return Err("too_many_files".into());
            }
            let bin = self.bin_or_empty()?;
            let limits = ImportLimits::default();
            let (mut items, mut imported, mut refused) = (Vec::new(), Vec::new(), Vec::new());
            let mut taken: BTreeSet<String> = bin.media_items.iter().map(|i| i.item_id.clone()).collect();
            let mut digests: BTreeSet<String> = BTreeSet::new();
            for path in paths {
                let name = bin_text(&path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()));
                let bytes = match read_audio_file(path, &limits) {
                    Ok(b) => b,
                    Err(e) => {
                        refused.push(json!({"name": name, "code": "media_read", "detail": e.code}));
                        continue;
                    }
                };
                let hex = match reel::full_digest(&mut Cursor::new(&bytes[..]), 1 << 20, self.cancel) {
                    Ok(d) => reel::hex(&d),
                    Err(e) => {
                        refused.push(json!({"name": name, "code": e.code()}));
                        continue;
                    }
                };
                let source = media_source(hex.clone());
                if bin.item_of_digest(&ContentDigest { algorithm: "sha256".into(), digest: hex.clone() }).is_some() || !digests.insert(hex) {
                    refused.push(json!({"name": name, "code": "already_imported"}));
                    continue;
                }
                let read = match self.classify(&bytes, &source) {
                    Ok(r) => r,
                    Err((code, detail)) => {
                        refused.push(json!({"name": name, "code": code, "detail": detail}));
                        continue;
                    }
                };
                if let Err(code) = self.run.stage_tile(bytes) {
                    refused.push(json!({"name": name, "code": code}));
                    continue;
                }
                let item_id = {
                    let refs: BTreeSet<&str> = taken.iter().map(String::as_str).collect();
                    mint_key("item", &refs)
                };
                taken.insert(item_id.clone());
                imported.push(json!({"item_id": item_id, "name": name, "kind": read.kind}));
                items.push(StudioMediaItem {
                    item_id,
                    name,
                    folder_id: folder.map(str::to_owned),
                    source,
                    kind: read.kind,
                    format: read.format,
                    duration: read.duration,
                    frame_rate: read.frame_rate,
                    width: read.width,
                    height: read.height,
                    sample_rate_hz: read.sample_rate_hz,
                    channels: read.channels,
                });
            }
            if !items.is_empty() {
                self.bin_command(&StudioProjectBinCommand::Import { items })?;
            }
            self.say(format!("project: imported {} file(s), refused {}", imported.len(), refused.len()));
            Ok(json!({"imported": imported, "refused": refused}))
        }

        /// The bytes of a bin item, from the session (staged at import, or the saved package).
        fn item_bytes(&self, source: &ClipSource) -> Result<Vec<u8>, String> {
            let ClipSource::Media { content_digest, .. } = source else { return Err("not_media".into()) };
            self.run.asset_bytes(content_digest).map_err(str::to_owned)
        }

        /// Binds the bin to the sequence's monitor, export and (under `audio-batch`) timeline and
        /// mixer (module docs). Returns `{video, audio, not_bound: [{item_id, code}]}`.
        pub fn project_bind(&mut self, sequence_id: &str, width: u32, height: u32, sample_rate_hz: u32) -> Result<Value, String> {
            let bin = self.bin_or_empty()?;
            if sample_rate_hz == 0 {
                return Err("audio_rate".into());
            }
            let extents = BinExtents(Arc::new(bin.media_items.iter().map(|i| (i.source.clone(), Ticks::new(i.duration))).collect()));
            let mut decoder = ReelFrameDecoder::new();
            let (mut audio, mut not_bound, mut video) = (Vec::new(), Vec::new(), 0usize);
            for item in &bin.media_items {
                let bytes = match self.item_bytes(&item.source) {
                    Ok(b) => b,
                    Err(code) => {
                        not_bound.push(json!({"item_id": item.item_id, "code": code}));
                        continue;
                    }
                };
                match item.kind {
                    StudioMediaKind::Video => match decoder.register(&item.source, Arc::from(bytes)) {
                        Ok(()) => video = video.saturating_add(1),
                        Err(e) => not_bound.push(json!({"item_id": item.item_id, "code": e.code})),
                    },
                    StudioMediaKind::Audio if item.sample_rate_hz != Some(sample_rate_hz) => {
                        not_bound.push(json!({"item_id": item.item_id, "code": "audio_rate_mismatch", "sample_rate_hz": item.sample_rate_hz}));
                    }
                    StudioMediaKind::Audio => match self.decode_audio_item(&bytes) {
                        Ok(decoded) => audio.push((item.source.clone(), decoded.planar)),
                        Err((code, _)) => not_bound.push(json!({"item_id": item.item_id, "code": code})),
                    },
                }
            }
            if self.timeline.as_ref().is_some_and(|t| t.sequence_id != sequence_id) {
                self.timeline = None;
            }
            let on_it = self.monitor.as_ref().is_some_and(|m| m.monitor.sequence().sequence_id == sequence_id);
            if !on_it {
                self.open_monitor(sequence_id, width, height).map_err(|e| format!("{} {}", e.code, e.detail))?;
            }
            let frames = MediaFrames::new(decoder, hsk_studio_pulse::FrameCacheLimits::default(), 4).map_err(|e| format!("{} {}", e.code, e.detail))?;
            self.set_monitor_sources(Box::new(frames), Box::new(extents.clone()));
            let sources = BinAudio(Arc::new(audio));
            let bound_audio = sources.0.len();
            self.set_export_audio(sources.clone(), Arc::new(extents.clone()), sample_rate_hz)?;
            #[cfg(feature = "audio-batch")]
            self.open_multitrack(sample_rate_hz, 960, sources, Arc::new(extents))?;
            #[cfg(not(feature = "audio-batch"))]
            drop((sources, extents));
            self.say(format!("project: {sequence_id} bound to {video} video and {bound_audio} audio item(s), {} not bound", not_bound.len()));
            Ok(json!({"video": video, "audio": bound_audio, "not_bound": not_bound}))
        }
    }
}

#[cfg(feature = "media")]
pub use media::{BinAudio, BinExtents};
