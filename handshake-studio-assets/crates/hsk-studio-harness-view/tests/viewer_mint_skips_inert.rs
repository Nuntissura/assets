//! Lane SMB-PR-ROUTING-REPAIR-1 (R57 F-R57-3, V4-06): the Viewer mints the first unused counter, so an
//! id a stored entry names could be taken by the next razor piece, nest or source edit, and a new
//! sequence could take a removed sequence's routing. Every Viewer mint of a clip, track or sequence id
//! skips what the stored audio routing, multicam and track display entries name. The oracle is the id
//! each scheme would have minted without the entry. No window is opened.
#![cfg(all(feature = "viewer", feature = "audio-batch"))]

mod support_context;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::source_monitor::{AssemblyMode, SourceEdit};
use hsk_studio_harness_view::view::timeline::TimelineCommand;
use hsk_studio_package::{AssetSource, Extras, Limits, MemoryAssetSource, Quarantined, WriteOptions, sha256_hex_of, write_package_with};
use hsk_studio_pulse::{AudioSources, SourceExtents};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const F: u64 = 10_160_640_000;
const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000009";
const RATE: u32 = 48_000;

/// The ids the schemes mint first for this document.
fn razor_clip(n: u32) -> String {
    format!("SCLP-019abcde-0000-7e1e-9000-{n:012x}")
}
fn source_clip(n: u32) -> String {
    format!("SCLP-019abcde-0000-7ed1-9000-{n:012x}")
}
fn nest_track(n: u32) -> String {
    format!("STRK-019abcde-0000-7e1e-9001-{n:012x}")
}
fn tid(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}
fn cid(n: u32) -> String {
    format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-viewer-mint-inert-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn media(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}
fn clip(n: u32, name: &str, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: media(name),
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}
fn track(n: u32, kind: TrackKind, name: &str, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind,
        index: 0,
        name: name.into(),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan: 0.0,
    }
}
fn main_sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(40 * F),
        tracks: vec![track(1, TrackKind::Video, "V1", vec![clip(1, "a", 40)]), track(2, TrackKind::Audio, "A1", vec![clip(2, "a", 40)])],
        markers: Vec::new(),
        work_area: None,
    }
}

struct Any;
impl folio::Resolver for Any {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

fn held(entry_token: &str, body: &[u8]) -> Quarantined {
    let sha256_hex = sha256_hex_of(body);
    Quarantined { name: format!("studio/{entry_token}.{sha256_hex}"), bytes: body.to_vec(), sha256_hex }
}
fn package(dir: &Path, sequences: Vec<StudioSequence>, entries: &[Quarantined]) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true,"opacity":1.0,"blend_mode":"normal","clipping_base":null,"masks":[],"filters":[],"styles":[],"transform":[1.0,0.0,0.0,1.0,0.0,0.0]}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":sequences.iter().map(|s| serde_json::to_value(s).unwrap()).collect::<Vec<_>>()
    });
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = MemoryAssetSource::new();
    let mut entries = entries.to_vec();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut bytes = Vec::new();
    write_package_with(&snapshot, &assets as &dyn AssetSource, &Extras { quarantined: &entries, previews: &[] }, &WriteOptions::default(), &Limits::default(), &token, &mut bytes).unwrap();
    let path = dir.join("mint.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// One second of silence-free mono at every source.
#[derive(Clone)]
struct Sources(Arc<Vec<Vec<f32>>>);
impl AudioSources for Sources {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        matches!(s, ClipSource::Media { .. }).then_some(self.0.as_slice())
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}
fn opened(label: &str, entries: &[Quarantined]) -> (Scratch, Viewer) {
    let scratch = Scratch::new(label);
    let path = package(&scratch.0, vec![main_sequence()], entries);
    let mut v = support_context::viewer(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    let sources = Sources(Arc::new(vec![vec![0.25; 2_000_000]]));
    v.open_multitrack(RATE, 960, sources, Arc::new(Ext)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    (scratch, v)
}
fn sequences(v: &Viewer) -> Vec<StudioSequence> {
    v.run().snapshot().unwrap().document().sequences.clone()
}
fn clip_ids(v: &Viewer) -> Vec<String> {
    sequences(v).iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()).map(|c| c.clip_id.clone()).collect()
}
fn track_ids(v: &Viewer) -> Vec<String> {
    sequences(v).iter().flat_map(|s| s.tracks.iter()).map(|t| t.track_id.clone()).collect()
}
fn gain(id: String) -> StudioClipGainEnvelope {
    StudioClipGainEnvelope { clip_id: id, points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -48.0 }] }
}
fn pan(id: String) -> StudioClipPanEnvelope {
    StudioClipPanEnvelope { clip_id: id, points: vec![StudioClipPanPoint { offset_ticks: 0, pan: -1.0 }] }
}
fn set_with(r: StudioAudioRouting) -> Vec<u8> {
    serde_json::to_vec(&StudioAudioRoutingSet { routings: vec![r] }).unwrap()
}

/// `timeline.rs` `mint_clip_ids`: a razor piece takes neither an inert gain or pan envelope nor an inert
/// multicam mode, and `plan_move`, which shares the mint, the same.
#[test]
fn a_razor_piece_skips_the_clip_ids_entries_name() {
    let mut r = StudioAudioRouting::new("seq-main");
    r.clip_gains.push(gain(razor_clip(1)));
    r.clip_pans.push(pan(razor_clip(2)));
    let mut modes = StudioMulticamRecords::new(DOC);
    modes.clips.push(StudioMulticamClip { clip_id: razor_clip(3), active_angle: 1, audio_angle: 1, audio_follows_video: true, follows_nest_setting: false, enabled: true });
    let entries = [held("audio_routing", &set_with(r)), held("multicam", &serde_json::to_vec(&modes).unwrap())];
    let (_s, mut v) = opened("razor", &entries);
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(10 * F))).unwrap();
    v.timeline_command(TimelineCommand::RazorAtPlayhead).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    let ids = clip_ids(&v);
    assert_eq!(ids.len(), 4, "{ids:?}");
    for inert in [razor_clip(1), razor_clip(2), razor_clip(3)] {
        assert!(!ids.contains(&inert), "{inert} was taken: {ids:?}");
    }
    assert!(ids.contains(&razor_clip(4)), "the first id no entry names: {ids:?}");
    // Move the piece: the same mint.
    let piece = razor_clip(4);
    v.timeline_command(TimelineCommand::MoveClip { clip_id: piece.clone(), start: Ticks::new(60 * F), track_id: None }).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    let ids = clip_ids(&v);
    for inert in [razor_clip(1), razor_clip(2), razor_clip(3)] {
        assert!(!ids.contains(&inert), "{inert} taken by the move: {ids:?}");
    }
}

/// `source_monitor.rs` `mint_clip_ids`: its own scheme, the same rule.
#[test]
fn a_source_edit_skips_the_clip_ids_entries_name() {
    let mut r = StudioAudioRouting::new("seq-main");
    r.clip_gains.push(gain(source_clip(1)));
    let (_s, mut v) = opened("source", &[held("audio_routing", &set_with(r))]);
    v.open_source_monitor(media("picture"), Ticks::new(20 * F), SequenceFrameRate { ticks_per_frame: F }, 64, 36).unwrap();
    let m = v.source_monitor_mut().unwrap();
    m.mark_in(0).unwrap();
    m.mark_out(4).unwrap();
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(10 * F))).unwrap();
    v.source_edit(&SourceEdit { mode: AssemblyMode::Insert, targets: Vec::new(), fit: None }).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    let ids = clip_ids(&v);
    assert!(!ids.contains(&source_clip(1)), "taken: {ids:?}");
    assert!(ids.contains(&source_clip(2)), "the first id no entry names: {ids:?}");
}

/// `nest.rs` `mint_track_ids`, `mint_clip_ids` and the sequence id: a nest takes no route, output, send
/// target, bus chain, automation lane, envelope, routing or display entry.
#[test]
fn a_nest_skips_the_track_clip_and_sequence_ids_entries_name() {
    let mut r = StudioAudioRouting::new("seq-main");
    r.routes.push(StudioTrackRoute { track_id: nest_track(1), output: None, sends: vec![] });
    r.routes.push(StudioTrackRoute {
        track_id: tid(2),
        output: Some(nest_track(2)),
        sends: vec![StudioAudioSend { send_id: "s1".into(), target: nest_track(3), level: 0.5, pre_fader: false, pan: 0.0 }],
    });
    r.bus_effects.push(StudioBusEffects { track_id: nest_track(4), effects: vec![] });
    r.track_automation.push(StudioTrackAutomation { track_id: nest_track(5), param: StudioTrackParam::Pan, points: vec![StudioAutomationPoint { offset_ticks: 0, value: 0.5 }] });
    r.clip_gains.push(gain(razor_clip(1)));
    let mut removed_nest = StudioAudioRouting::new("nest-1");
    removed_nest.master.volume = 0.1;
    let routing = serde_json::to_vec(&StudioAudioRoutingSet { routings: vec![r, removed_nest] }).unwrap();
    let mut display = StudioTrackDisplayRecords::new(DOC);
    display.sequences.push(StudioTrackDisplayEntry { sequence_id: "nest-2".into(), switches: StudioTrackDisplaySwitches::STUDIO_DEFAULT, heights: StudioTrackHeights::all(EXPANDED_TRACK_HEIGHT), height_presets: Vec::new() });
    let entries = [held("audio_routing", &routing), held("track_display", &serde_json::to_vec(&display).unwrap())];
    let (_s, mut v) = opened("nest", &entries);
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    let s = sequences(&v);
    assert_eq!(s.len(), 2, "one new sequence");
    assert_eq!(s[1].sequence_id, "nest-3", "neither the routing's nest-1 nor the display's nest-2");
    let (tracks, clips) = (track_ids(&v), clip_ids(&v));
    for n in 1..=5 {
        assert!(!tracks.contains(&nest_track(n)), "{}: {tracks:?}", nest_track(n));
    }
    assert!(tracks.contains(&nest_track(6)), "the first id no entry names: {tracks:?}");
    assert!(!clips.contains(&razor_clip(1)), "{clips:?}");
    // The routing's bytes are what they were, and nothing went live.
    assert_eq!(v.run().document_entry("audio_routing"), Some(routing.as_slice()));
}

/// Make Subsequence shares `ids_for`: its own prefix, the same rule.
#[test]
fn a_subsequence_skips_a_routing_of_its_id() {
    let r = StudioAudioRouting::new("subsequence-1");
    let (_s, mut v) = opened("subsequence", &[held("audio_routing", &set_with(r))]);
    v.timeline_command(TimelineCommand::MakeSubsequence(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    let s = sequences(&v);
    assert_eq!(s[1].sequence_id, "subsequence-2", "{:?}", s.iter().map(|q| q.sequence_id.clone()).collect::<Vec<_>>());
}

/// A file the entries do not parse in cannot name anything: minting keeps its first ids and the step works.
#[test]
fn an_entry_that_does_not_parse_reserves_nothing() {
    let (_s, mut v) = opened("damaged", &[held("audio_routing", b"{not json")]);
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(10 * F))).unwrap();
    v.timeline_command(TimelineCommand::RazorAtPlayhead).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    assert!(clip_ids(&v).contains(&razor_clip(1)));
}

/// Option A through the harness, without assuming which entry a caller passes: a sequence step and a
/// multicam entry together cannot change a declared source's video tracks, on either harness path, and
/// nothing moves. Only `Session::submit_multicam` carrying rewritten records can; no planner produces it.
#[test]
fn the_harness_sequence_and_entry_paths_cannot_carry_a_track_change_on_a_multicam_source() {
    use hsk_studio_harness_core::{Harness, Selection};
    use hsk_studio_session::SequenceEdit;
    let scratch = Scratch::new("harness");
    let mut v2 = track(3, TrackKind::Video, "V2", vec![clip(3, "a", 40)]);
    v2.index = 1;
    let mut source = main_sequence();
    source.tracks.push(v2);
    let mut nested = clip(9, "a", 4);
    nested.source_ref = ClipSource::Sequence { sequence_id: "seq-main".into() };
    let mut parent = main_sequence();
    parent.sequence_id = "seq-parent".into();
    parent.duration = Ticks::new(4 * F);
    parent.tracks = vec![track(8, TrackKind::Video, "V1", vec![nested])];
    let order = |s: &StudioSequence| multicam_angle_order(s, MulticamSelectionOrder::SourceOrder);
    let mut records = StudioMulticamRecords::new(DOC);
    records.multicam_sequences.push(StudioMulticamSequence { sequence_id: "seq-main".into(), selection_order: MulticamSelectionOrder::SourceOrder, angle_order: order(&source) });
    records.clips.push(StudioMulticamClip { clip_id: cid(9), active_angle: 2, audio_angle: 2, audio_follows_video: true, follows_nest_setting: false, enabled: true });
    let body = serde_json::to_vec(&records).unwrap();
    let path = package(&scratch.0, vec![source.clone(), parent], &[held("multicam", &body)]);
    let mut without_v2 = source.clone();
    without_v2.tracks.retain(|t| t.track_id != tid(3));
    let mut rewritten = records.clone();
    rewritten.multicam_sequences[0].angle_order = order(&without_v2);
    rewritten.clips[0].active_angle = 1;
    rewritten.clips[0].audio_angle = 1;
    let rewritten = serde_json::to_vec(&rewritten).unwrap();

    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(support_context::context(), &cancel);
    assert!(run.open_path(&path), "{:?}", run.ops().last());
    let seqs = |run: &hsk_studio_harness_core::Run<'_>| run.snapshot().unwrap().document().sequences.clone();
    let held_now = seqs(&run);
    assert!(!run.submit_sequence_and_entry("drop_angle", without_v2.clone(), "multicam", rewritten.clone()));
    assert_eq!(run.ops().last().unwrap()["error"], "folio_rejected", "{:?}", run.ops().last());
    assert!(!run.submit_sequence_edits_and_entry("drop_angle", vec![SequenceEdit::Replace { expect: source, write: without_v2 }], "multicam", rewritten));
    assert_eq!(run.ops().last().unwrap()["error"], "folio_rejected", "{:?}", run.ops().last());
    assert_eq!(seqs(&run), held_now);
    assert_eq!(run.document_entry("multicam"), Some(body.as_slice()));
}
