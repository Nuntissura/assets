//! Lane SMB-PR-NEST-2 (feature `viewer`; export cases `media`, envelope cases `audio-batch`): the
//! three gaps SMB-PR-NEST-1 left ([STU-VID-030] nested sequence clips, [STU-VID-040] one reversible
//! step per operation):
//!
//! - **Export** of a sequence holding a nested clip resolves it (`timeline_export_job_with` and the
//!   document's `NestedDocument`): the decoded frame at a hand-picked tick equals the program
//!   monitor's frame for that tick, the audio equals the parent mix before the nest within 1e-6, and
//!   a missing inner source fails typed (never a black frame or silence).
//! - **Envelopes follow a nest**: Nest moves the nested clips' gain and pan envelopes into the new
//!   sequence's routing unchanged and the parent mix is the same sample for sample; the sequences
//!   and the routing entry are ONE undo step (undo byte-identical, redo re-applies); a refused entry
//!   write leaves both unchanged; Make Subsequence copies the envelopes under the copies' ids.
//! - **Program monitor without the timeline**: a monitor opened on a sequence with a nested clip
//!   renders it with the timeline panel never opened, and follows an undo that changes the nested
//!   sequence while the timeline panel is hidden.
//!
//! Oracles: hand-built solid source frames and a stereo test tone; the program monitor's own render
//! of the same tick; pulse's mix of the sequence before the nest. Premiere behaviour is UNVERIFIED
//! (D5): an envelope moving with its clip unchanged, in clip-relative ticks, is the declared Studio
//! reading.
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::monitor::{FrameSource, SourceFrame};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::timeline::TimelineCommand;
use hsk_studio_pulse::SourceExtents;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 25 fps ticks per frame (STU-VID-013); 48 kHz has 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RED: [f32; 3] = [0.8, 0.2, 0.1];
const BLUE: [f32; 3] = [0.1, 0.4, 0.9];

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-view-nest2-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cid(n: u32) -> String {
    format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
}
fn media(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}
/// Clip `n` of `source`: source frames `[si, so)` at timeline frame `start` (unit speed).
fn clip(n: u32, source: ClipSource, si: u64, so: u64, start: u64, group: Option<&str>) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: source,
        source_in: Ticks::new(si * F),
        source_out: Ticks::new(so * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new((so - si) * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: group.map(str::to_owned),
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn track(n: u32, kind: TrackKind, name: &str, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
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
fn sequence(id: &str, tracks: Vec<StudioTrack>) -> StudioSequence {
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks,
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}
/// NEST-1's fixture: V1 clip 1 `a` source [10, 30) at frames [0, 20) linked `lg-1` with A1 clip 3
/// `d` source [0, 20) at [0, 20); V1 clip 2 `b` source [0, 10) at [30, 40).
fn main_sequence() -> StudioSequence {
    sequence(
        "seq-main",
        vec![
            track(1, TrackKind::Video, "V1", vec![clip(1, media("a"), 10, 30, 0, Some("lg-1")), clip(2, media("b"), 0, 10, 30, None)]),
            track(2, TrackKind::Audio, "A1", vec![clip(3, media("d"), 0, 20, 0, Some("lg-1"))]),
        ],
    )
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

fn package(dir: &Path, sequences: Vec<StudioSequence>) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000009","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":sequences.iter().map(|s| serde_json::to_value(s).unwrap()).collect::<Vec<_>>()
    });
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("nest2.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The viewer with `sequences` open and NO panel opened.
fn opened_with(label: &str, sequences: Vec<StudioSequence>) -> (Scratch, Viewer) {
    let scratch = Scratch::new(label);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&scratch.0, sequences)).unwrap_or_else(|e| panic!("{e}"));
    (scratch, v)
}
/// NEST-1's fixture open with the timeline shown.
fn opened(label: &str) -> (Scratch, Viewer) {
    let (s, mut v) = opened_with(label, vec![main_sequence()]);
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    (s, v)
}

fn sequences(v: &Viewer) -> Vec<StudioSequence> {
    v.run().snapshot().unwrap().document().sequences.clone()
}
fn undo_labels(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned).collect()
}
fn status(v: &Viewer) -> Value {
    v.state_json()["status"].clone()
}
fn routing_bytes(v: &Viewer) -> Option<Vec<u8>> {
    v.run().document_entry("audio_routing").map(<[u8]>::to_vec)
}
/// The nested sequence clip on V1 of `seq-main`.
fn nested_clip(v: &Viewer) -> StudioClip {
    let s = sequences(v);
    let main = s.iter().find(|q| q.sequence_id == "seq-main").unwrap();
    main.tracks[0].clips.iter().find(|c| matches!(c.source_ref, ClipSource::Sequence { .. })).cloned().expect("nested clip on V1")
}

/// 4x4 solid source frame.
fn solid(rgb: [f32; 3]) -> Arc<SourceFrame> {
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    for _ in 0..16 {
        for c in rgb {
            colour.extend_from_slice(&c.to_le_bytes());
        }
        alpha.extend_from_slice(&1.0f32.to_le_bytes());
    }
    Arc::new(SourceFrame { width: 4, height: 4, colour_sha256: hsk_studio_prism::profile_hash(&colour), alpha_sha256: hsk_studio_prism::profile_hash(&alpha), colour, alpha })
}
/// Frames by media name: `red` / `a` red, `blue` / `b` blue (names left out are missing).
struct Solids(Vec<&'static str>);
impl FrameSource for Solids {
    fn frame(&self, source: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
        let ClipSource::Media { artifact_manifest_id: id, .. } = source else { return None };
        if !self.0.contains(&id.as_str()) {
            return None;
        }
        match id.as_str() {
            "red" | "a" => Some(solid(RED)),
            "blue" | "b" => Some(solid(BLUE)),
            _ => None,
        }
    }
}
fn all_solids() -> Solids {
    Solids(vec!["red", "blue", "a", "b"])
}
#[derive(Clone, Copy)]
struct Known;
impl SourceExtents for Known {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}
/// Stereo test tone of 40 frames for media `d`.
#[derive(Clone)]
struct Tone(Arc<Vec<Vec<f32>>>);
impl hsk_studio_pulse::AudioSources for Tone {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        matches!(s, ClipSource::Media { artifact_manifest_id: id, .. } if id == "d").then_some(self.0.as_slice())
    }
}
fn tone() -> Tone {
    let n = 40 * SPF;
    Tone(Arc::new(vec![(0..n).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect(), (0..n).map(|i| ((i as f32) * 0.013).cos() * 0.25).collect()]))
}
fn near(got: [f32; 4], want: [f32; 3], tol: f32) -> bool {
    (0..3).all(|c| (got[c] - want[c]).abs() <= tol)
}

// ---------------------------------------------------------------------------------------------
// Program monitor without the timeline
// ---------------------------------------------------------------------------------------------

fn frame(v: &mut Viewer, ctx: &egui::Context, time: f64) {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), time: Some(time), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
}
/// The program monitor's shown pixel at the raster centre.
fn monitor_centre(v: &Viewer) -> [f32; 4] {
    let m = v.monitor().expect("monitor open");
    let shown = m.shown().unwrap_or_else(|| panic!("no frame shown: {:?} {}", m.last_error(), status(v)));
    shown.image.pixel(shown.image.width / 2, shown.image.height / 2).unwrap()
}

/// `seq-main` holds only a nested clip (clip 10 -> `seq-n` [0, 20) at 0); `seq-n` plays `red`
/// [0, 10) then `blue` [10, 20). The program monitor opened from WORKSPACE with the timeline panel
/// never opened renders the nested red frame at frame 0. Then (timeline opened, the red clip lifted
/// inside `seq-n`, back to the parent, timeline HIDDEN) EDIT > Undo changes only the nested
/// sequence: the next refresh shows red again without the timeline panel.
#[test]
fn program_monitor_resolves_a_nested_clip_without_the_timeline_and_follows_an_undo() {
    let inner = sequence(
        "seq-n",
        vec![track(20, TrackKind::Video, "V1", vec![clip(20, media("red"), 0, 10, 0, None), clip(21, media("blue"), 0, 10, 10, None)])],
    );
    let main = sequence("seq-main", vec![track(10, TrackKind::Video, "V1", vec![clip(10, ClipSource::Sequence { sequence_id: "seq-n".into() }, 0, 20, 0, None)])]);
    let (_s, mut v) = opened_with("monitor", vec![main, inner]);
    let ctx = egui::Context::default();
    v.shell_command(ShellCommand::Panel(Panel::ProgramMonitor)).unwrap();
    v.set_monitor_sources(Box::new(all_solids()), Box::new(Known));
    frame(&mut v, &ctx, 1.0);
    assert!(v.timeline().is_none(), "the timeline panel was never opened");
    assert_eq!(v.monitor().unwrap().sequence().sequence_id, "seq-main");
    let px = monitor_centre(&v);
    assert!(near(px, RED, 1e-3), "nested red frame shown without the timeline: {px:?}");
    // Lift the red clip inside seq-n through the timeline, return to the parent, hide the timeline.
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    v.timeline_command(TimelineCommand::OpenNested(Some(cid(10)))).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    v.timeline_command(TimelineCommand::Select(Some(cid(20)))).unwrap();
    v.timeline_command(TimelineCommand::Lift).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    v.timeline_command(TimelineCommand::BackToParent).unwrap();
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    assert!(v.panel_open(Panel::Timeline) && !v.panel_visible(Panel::Timeline), "timeline hidden");
    frame(&mut v, &ctx, 2.0);
    let px = monitor_centre(&v);
    assert!(near(px, [0.0, 0.0, 0.0], 1e-3), "the lifted nested clip leaves black: {px:?}");
    // Undo (timeline hidden) restores the nested clip; the monitor host resyncs on the new revision.
    v.shell_command(ShellCommand::Undo).unwrap();
    frame(&mut v, &ctx, 3.0);
    let px = monitor_centre(&v);
    assert!(near(px, RED, 1e-3), "the next refresh shows the undone nested sequence: {px:?} {}", status(&v));
    assert_eq!(v.monitor().unwrap().document_revision(), v.run().snapshot().map(|s| s.document().revision));
}

// ---------------------------------------------------------------------------------------------
// Envelopes follow a nest
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "audio-batch")]
fn gain_envelope(clip: &str) -> StudioClipGainEnvelope {
    StudioClipGainEnvelope { clip_id: clip.into(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -6.0 }, StudioClipGainPoint { offset_ticks: 10 * F, gain_db: 0.0 }] }
}
#[cfg(feature = "audio-batch")]
fn pan_envelope(clip: &str) -> StudioClipPanEnvelope {
    StudioClipPanEnvelope { clip_id: clip.into(), points: vec![StudioClipPanPoint { offset_ticks: 6 * F, pan: -0.5 }, StudioClipPanPoint { offset_ticks: 12 * F, pan: 0.7 }] }
}
/// Routing of `seq-main` with a gain and a pan envelope on clip 3.
#[cfg(feature = "audio-batch")]
fn enveloped_routing() -> StudioAudioRoutingSet {
    let mut r = StudioAudioRouting::new("seq-main");
    r.clip_gains.push(gain_envelope(&cid(3)));
    r.clip_pans.push(pan_envelope(&cid(3)));
    StudioAudioRoutingSet { routings: vec![r] }
}
/// The mix of `seq-main` (its bus stream with the stored routing), 24 frames, planar stereo.
#[cfg(feature = "audio-batch")]
fn main_mix(v: &Viewer) -> [Vec<f32>; 2] {
    let cancel = CancellationToken::default();
    let mut s = v.bus_stream("seq-main", 48_000, 960, tone(), &Known).unwrap_or_else(|e| panic!("{e}"));
    let n = 24 * SPF;
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    for at in (0..n).step_by(960) {
        s.render(&mut l[at..at + 960], &mut r[at..at + 960], &cancel).unwrap();
    }
    [l, r]
}
#[cfg(feature = "audio-batch")]
fn routing_of(v: &Viewer, id: &str) -> StudioAudioRouting {
    v.audio_routing().unwrap().unwrap().routings.into_iter().find(|r| r.sequence_id == id).unwrap_or_else(|| panic!("no routing for {id}"))
}

/// Nest of clip 1 (its link group brings clip 3, which has a gain and a pan envelope): the
/// envelopes move unchanged (clip-relative offsets kept) from `seq-main`'s routing to the new
/// sequence's routing; `seq-main`'s mix is the same before and after, sample for sample within 1e-6
/// (the nested mix applies the moved envelopes); ONE undo step: EDIT > Undo restores the sequences
/// and the routing entry byte-identical, EDIT > Redo re-applies both.
#[cfg(feature = "audio-batch")]
#[test]
fn nest_moves_clip_envelopes_in_one_step_and_the_parent_mix_is_unchanged() {
    let (_s, mut v) = opened("envelopes");
    v.set_audio_routing(&enveloped_routing()).unwrap();
    let (seq_before, entry_before) = (sequences(&v), routing_bytes(&v));
    let mix_before = main_mix(&v);
    let undo = undo_labels(&v).len();
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    let labels = undo_labels(&v);
    assert_eq!(labels.len(), undo + 1, "one history step: {labels:?}");
    assert!(labels.last().is_some_and(|l| l.starts_with("sequence.nest ")), "{labels:?}");
    let inner = sequences(&v)[1].sequence_id.clone();
    let parent = routing_of(&v, "seq-main");
    assert!(parent.clip_gains.is_empty() && parent.clip_pans.is_empty(), "{parent:?}");
    let moved = routing_of(&v, &inner);
    assert_eq!((moved.clip_gains.as_slice(), moved.clip_pans.as_slice()), ([gain_envelope(&cid(3))].as_slice(), [pan_envelope(&cid(3))].as_slice()), "moved with their offsets");
    let mix_after = main_mix(&v);
    for ch in 0..2 {
        for (i, (want, got)) in mix_before[ch].iter().zip(&mix_after[ch]).enumerate() {
            assert!((got - want).abs() <= 1e-6, "ch {ch} sample {i}: {got} vs {want}");
        }
        assert!(mix_before[ch].iter().any(|s| s.abs() > 0.01), "audible");
    }
    let (seq_after, entry_after) = (sequences(&v), routing_bytes(&v));
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(sequences(&v), seq_before, "undo restores the sequences");
    assert_eq!(routing_bytes(&v), entry_before, "undo restores the routing entry byte-identical");
    v.shell_command(ShellCommand::Redo).unwrap();
    assert_eq!(sequences(&v), seq_after, "redo re-applies the nest");
    assert_eq!(routing_bytes(&v), entry_after, "redo re-applies the routing entry");
    assert!(nested_clip(&v).link_group.is_some());
}

/// `Run::submit_sequence_edits_and_entry` with an entry write the session refuses (an unknown
/// token, typed `unsafe_name`): the sequences, the routing entry and the history are unchanged
/// (the committed sequence step is rolled back, no split step). R44 F-R44-13 (lane
/// SMB-R44-FOLLOWUPS-1): the history holds a redo entry before the refused write (a second entry
/// write undone once), and that redo history survives the rollback unchanged (the F-R42-2 property).
#[cfg(feature = "audio-batch")]
#[test]
fn refused_entry_write_leaves_sequences_and_entry_unchanged() {
    use hsk_studio_harness_core::{Harness, Selection, default_actor};
    use hsk_studio_pulse::{EditContext, NestIds, SequenceChange, plan_nest};
    use hsk_studio_session::SequenceEdit;
    let scratch = Scratch::new("refused");
    let path = package(&scratch.0, vec![main_sequence()]);
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_path(&path), "{:?}", run.ops().last());
    run.set_document_entry("audio_routing", Some(serde_json::to_vec(&enveloped_routing()).unwrap())).unwrap();
    // A redo history to preserve: a second entry write as one undoable step, undone once.
    assert!(run.set_tiles_and_entry("audio_routing.write", "", None, "audio_routing", b"{\"routings\":[]}".to_vec()), "{:?}", run.ops().last());
    assert!(run.undo(), "{:?}", run.ops().last());
    assert_eq!(run.document_entry("audio_routing").map(<[u8]>::to_vec), Some(serde_json::to_vec(&enveloped_routing()).unwrap()));
    let snap = run.snapshot().unwrap().clone();
    let ids = NestIds {
        sequence_id: "nest-1".into(),
        sequence_name: "Nested Sequence 1".into(),
        track_ids: (60..62).map(|n| format!("STRK-019abcde-0000-7000-8000-{n:012x}")).collect(),
        clip_ids: (50..52).map(cid).collect(),
        link_group: Some("link-nest-1".into()),
        record_ids: Vec::new(),
    };
    let ctx = EditContext { budget: Budget::default(), resolver: &Any, extents: &hsk_studio_pulse::UnknownExtents, cancel: &cancel };
    let p = plan_nest(&snap, "seq-main", snap.document().revision, &[cid(1)], &ids, &ctx).unwrap();
    let edits = || -> Vec<SequenceEdit> {
        p.changes()
            .iter()
            .map(|c| match c {
                SequenceChange::Replace { expect, write } => SequenceEdit::Replace { expect: expect.clone(), write: write.clone() },
                SequenceChange::Insert { at, write } => SequenceEdit::Insert { at: *at, write: write.clone() },
                SequenceChange::Remove { expect } => SequenceEdit::Remove { expect: expect.clone() },
            })
            .collect()
    };
    let seqs = |run: &hsk_studio_harness_core::Run<'_>| run.snapshot().unwrap().document().sequences.clone();
    let entry = |run: &hsk_studio_harness_core::Run<'_>| run.document_entry("audio_routing").map(<[u8]>::to_vec);
    let (s0, e0, h0) = (seqs(&run), entry(&run), run.history());
    assert_eq!(h0["redo"].as_array().map(Vec::len), Some(1), "one redo entry before the refused write: {h0}");
    assert!(!run.submit_sequence_edits_and_entry("sequence.nest", edits(), "not-a-token", b"x".to_vec()));
    assert_eq!(run.ops().last().unwrap()["error"], "unsafe_name", "{:?}", run.ops().last());
    assert_eq!(seqs(&run), s0, "the sequence step was rolled back");
    assert_eq!(entry(&run), e0, "the entry is unchanged");
    assert_eq!(run.history(), h0, "no history step, redo history as it was");
    // The accepted form is one step; undo restores both.
    assert!(run.submit_sequence_edits_and_entry("sequence.nest", edits(), "audio_routing", b"{\"routings\":[]}".to_vec()), "{:?}", run.ops().last());
    assert_eq!(run.history()["undo"].as_array().unwrap().len(), h0["undo"].as_array().unwrap().len() + 1);
    assert!(run.undo(), "{:?}", run.ops().last());
    assert_eq!((seqs(&run), entry(&run)), (s0, e0));
}

/// Make Subsequence copies the envelopes of the clips it copies under the copies' ids into the
/// new sequence's routing (one step; the parent's routing is unchanged): from clips the points are
/// the same; from the in-to-out range [5, 35) clip 3's copy starts 5 frames into its source, so its
/// points shift by 5 frames and the cut puts an edge point at 0 (gain -3 dB, linear between -6 dB
/// at -5 and 0 dB at 5; the pan points land inside the copy at 1 and 7).
#[cfg(feature = "audio-batch")]
#[test]
fn make_subsequence_copies_clip_envelopes_under_the_new_ids() {
    use hsk_studio_harness_view::view::source_monitor::{MonitorKey, MonitorTarget};
    let (_s, mut v) = opened("subsequence");
    v.set_audio_routing(&enveloped_routing()).unwrap();
    let parent = routing_of(&v, "seq-main");
    let (seq_before, entry_before) = (sequences(&v), routing_bytes(&v));
    let copy_of_3 = |v: &Viewer| -> (String, String) {
        let sub = sequences(v).into_iter().nth(1).expect("new sequence");
        let id = sub.tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clips[0].clip_id.clone();
        (sub.sequence_id, id)
    };
    v.timeline_command(TimelineCommand::MakeSubsequence(vec![cid(3)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    let (sub, copy) = copy_of_3(&v);
    assert_ne!(copy, cid(3), "the copy has a fresh id");
    let r = routing_of(&v, &sub);
    let mut want_g = gain_envelope(&cid(3));
    want_g.clip_id.clone_from(&copy);
    let mut want_p = pan_envelope(&cid(3));
    want_p.clip_id.clone_from(&copy);
    assert_eq!((r.clip_gains, r.clip_pans), (vec![want_g], vec![want_p]));
    assert_eq!(routing_of(&v, "seq-main"), parent, "the parent's routing is unchanged");
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!((sequences(&v), routing_bytes(&v)), (seq_before.clone(), entry_before.clone()), "one step, undone byte-identical");
    // From the program monitor's in-to-out range [5, 35).
    v.timeline_command(TimelineCommand::Select(None)).unwrap();
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(5 * F))).unwrap();
    v.monitor_key(MonitorTarget::Program, MonitorKey::MarkIn).unwrap();
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(34 * F))).unwrap();
    v.monitor_key(MonitorTarget::Program, MonitorKey::MarkOut).unwrap();
    v.timeline_command(TimelineCommand::MakeSubsequence(Vec::new())).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    let (sub, copy) = copy_of_3(&v);
    let r = routing_of(&v, &sub);
    let g: Vec<(u64, f64)> = r.clip_gains.iter().filter(|e| e.clip_id == copy).flat_map(|e| e.points.iter().map(|p| (p.offset_ticks, p.gain_db))).collect();
    assert_eq!(g, [(0, -3.0), (5 * F, 0.0)]);
    let p: Vec<(u64, f64)> = r.clip_pans.iter().filter(|e| e.clip_id == copy).flat_map(|e| e.points.iter().map(|p| (p.offset_ticks, p.pan))).collect();
    assert_eq!(p, [(F, -0.5), (7 * F, 0.7)]);
}

// ---------------------------------------------------------------------------------------------
// Export of a sequence with a nested clip
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "media")]
mod export {
    use super::*;
    use hsk_studio_harness_view::view::monitor::{DropPolicy, ProgramMonitor};
    use hsk_studio_harness_view::view::timeline_export::{NestedDocument, TimelineExport, timeline_export_job_with};
    use hsk_studio_reel::*;
    use std::io::Cursor;

    #[derive(Default)]
    struct Collect {
        chunks: Vec<(i64, Vec<u8>)>,
    }
    impl ChunkSink for Collect {
        fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
            self.chunks.push((c.pts, c.bytes.to_vec()));
            Ok(())
        }
    }
    fn read(bytes: &[u8], track: u32, count: u64) -> Collect {
        let request = SegmentRequest {
            codec: "isobmff".to_owned(),
            start_pts: Ticks::new(0),
            count,
            track: Some(track),
            output: OutputMode::Decoded,
            prefer: SourcePreference::Original,
            purpose: Purpose::Export,
            grant: Grant { content_id: ContentId::parse("nest-export").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
            expected_revision: 1,
        };
        let mut sink = Collect::default();
        Registry::standard()
            .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink)
            .expect("reel reads the export");
        sink
    }
    /// BT.709 limited-range 8-bit Y'CbCr back to R'G'B'.
    fn rgb(y: u8, cb: u8, cr: u8) -> [f32; 3] {
        let y = (f32::from(y) - 16.0) / 219.0;
        let cb = (f32::from(cb) - 128.0) / 224.0;
        let cr = (f32::from(cr) - 128.0) / 224.0;
        let r = y + 1.5748 * cr;
        let b = y + 1.8556 * cb;
        let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
        [r, g, b]
    }
    #[derive(Clone)]
    struct NoAudio;
    impl hsk_studio_pulse::AudioSources for NoAudio {
        fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
            None
        }
    }
    fn job<V: FrameSource + Send + 'static, A: hsk_studio_pulse::AudioSources + Send + Sync + 'static>(
        sequence: StudioSequence,
        video: V,
        audio: Option<A>,
        output: PathBuf,
        nested: NestedDocument,
    ) -> Result<ExportJob, hsk_studio_harness_view::view::monitor::MonitorFail> {
        timeline_export_job_with(
            TimelineExport {
                sequence,
                in_tick: 0,
                out_tick: 20 * F,
                recipe: ExportRecipe {
                    recipe_id: "studio.dnxhr-hq.f32.mov@1".to_owned(),
                    container: Container::Mov,
                    video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)),
                    audio: audio.is_some().then_some(PcmChoice { bits: 32, float: true }),
                },
                width: 64,
                height: 32,
                bit_depth: 8,
                chroma: ChromaFormat::Yuv422,
                video_sources: video,
                video_extents: Known,
                audio: audio.map(|a| (a, 48_000)),
                audio_extents: Known,
                output,
                overwrite: false,
                artifact: "seq-main nested".into(),
            },
            nested,
            &CancellationToken::default(),
        )
    }

    /// `seq-main` after Nest of clip 1 (video `a` and audio `d` in the nested sequence) exports
    /// frames [0, 20): 20 frames; the decoded frame at hand-picked frame 6 equals the program
    /// monitor's frame for that tick (the nested red source; 8-bit VC-3 bound 0.02); the PCM equals
    /// the mix of `seq-main` BEFORE the nest within 1e-6 (the nested mix is the clip's own mix).
    #[test]
    fn export_resolves_a_nested_clip_like_the_program_monitor_and_the_parent_mix() {
        let (s, mut v) = opened("export");
        let before = sequences(&v)[0].clone();
        v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        let doc = sequences(&v);
        let main = doc[0].clone();
        let out = s.0.join("nested.mov");
        let nested = NestedDocument::new(doc.clone(), StudioAudioRoutingSet::default());
        let mut queue = ExportQueue::new();
        let _handle = queue.submit(job(main.clone(), all_solids(), Some(tone()), out.clone(), nested).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail)));
        let receipt = queue.run().remove(0);
        assert_eq!(receipt.outcome, ExportOutcome::Complete, "{receipt:?}");
        assert_eq!((receipt.frames_encoded, receipt.audio_frames), (20, 20 * SPF as u64));
        let bytes = std::fs::read(&out).unwrap();
        // Video: frame 6 against the program monitor's own render of that tick.
        let mut monitor = ProgramMonitor::new(main.clone(), 64, 32, DropPolicy::PresentEveryFrame).unwrap();
        monitor.set_document(None, doc.clone());
        let cancel = CancellationToken::default();
        let shown = monitor.render_at(Ticks::new(6 * F), &all_solids(), &Known, &cancel).unwrap();
        let video = read(&bytes, 1, 20);
        assert_eq!(video.chunks.len(), 20);
        let (w, h) = (64usize, 32usize);
        let (pts, f6) = &video.chunks[6];
        assert_eq!(*pts, (6 * F) as i64);
        let (yp, cb, cr) = (&f6[..w * h], &f6[w * h..w * h + w / 2 * h], &f6[w * h + w / 2 * h..]);
        for (x, y) in [(24usize, 8usize), (32, 16), (40, 24)] {
            let want = shown.pixel(x as u32, y as u32).unwrap();
            assert!(near(want, RED, 1e-3), "the monitor shows the nested source: {want:?}");
            let got = rgb(yp[y * w + x], cb[y * (w / 2) + x / 2], cr[y * (w / 2) + x / 2]);
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() <= 0.02, "pixel ({x},{y}) channel {c}: decoded {got:?} vs monitor {want:?}");
            }
        }
        // Audio: the mix of seq-main before the nest, frames [0, 20).
        let audio = read(&bytes, 2, 20 * SPF as u64);
        let got: Vec<f32> = audio.chunks.iter().flat_map(|c| c.1.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect::<Vec<_>>()).collect();
        let mut mix = hsk_studio_pulse::TimelineMixStream::new(&before, 48_000, 4800, tone(), &Known, hsk_studio_pulse::StreamLimits::default(), &cancel).unwrap();
        let mut want = Vec::new();
        let (mut l, mut r) = (vec![0f32; 4800], vec![0f32; 4800]);
        for _ in 0..(20 * SPF / 4800) {
            mix.render(&mut l, &mut r, &cancel).unwrap();
            for (a, b) in l.iter().zip(&r) {
                want.push(*a);
                want.push(*b);
            }
        }
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!((g - w).abs() <= 1e-6, "sample {i}: {g} vs {w}");
        }
        assert!(want.iter().any(|x| x.abs() > 0.01), "the range is not silent");
    }

    /// The exported PCM (track 2) of `bytes` over `frames` frames, interleaved stereo.
    #[cfg(feature = "audio-batch")]
    fn pcm(bytes: &[u8], frames: u64) -> Vec<f32> {
        let audio = read(bytes, 2, frames * SPF as u64);
        audio.chunks.iter().flat_map(|c| c.1.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect::<Vec<_>>()).collect()
    }

    /// Runs one export job to its file and returns the file's bytes.
    #[cfg(feature = "audio-batch")]
    fn run_job(job: ExportJob, out: &Path) -> Vec<u8> {
        let mut queue = ExportQueue::new();
        let _handle = queue.submit(job);
        let receipt = queue.run().remove(0);
        assert_eq!(receipt.outcome, ExportOutcome::Complete, "{receipt:?}");
        std::fs::read(out).unwrap()
    }

    /// Interleaves `frames` frames of a planar stereo mix.
    #[cfg(feature = "audio-batch")]
    fn interleave(mix: &[Vec<f32>; 2], frames: usize) -> Vec<f32> {
        (0..frames * SPF).flat_map(|i| [mix[0][i], mix[1][i]]).collect()
    }

    /// R44 F-R44-13 (lane SMB-R44-FOLLOWUPS-1): export with MOVED envelopes end to end. `seq-main`
    /// carries the gain and pan envelopes of clip 3 (`enveloped_routing`); after Nest of clip 1 they
    /// live in the nested sequence's routing, and the export of `seq-main` with the document's
    /// routing equals the mix of `seq-main` BEFORE the nest (its bus stream with the envelopes) within
    /// 1e-6 over frames [0, 20).
    #[cfg(feature = "audio-batch")]
    #[test]
    fn export_with_moved_envelopes_equals_the_pre_nest_mix() {
        let (s, mut v) = opened("export-envelopes");
        v.set_audio_routing(&enveloped_routing()).unwrap();
        let before = main_mix(&v);
        v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        let doc = sequences(&v);
        let routing = v.audio_routing().unwrap().unwrap();
        assert!(routing_of(&v, "seq-main").clip_gains.is_empty(), "the envelopes moved");
        let out = s.0.join("nested-envelopes.mov");
        let bytes = run_job(job(doc[0].clone(), all_solids(), Some(tone()), out.clone(), NestedDocument::new(doc.clone(), routing)).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail)), &out);
        let (got, want) = (pcm(&bytes, 20), interleave(&before, 20));
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!((g - w).abs() <= 1e-6, "sample {i}: {g} vs pre-nest mix {w}");
        }
        assert!(want.iter().any(|x| x.abs() > 0.01), "the range is not silent");
    }

    /// R44 F-R44-3 (lane SMB-R44-FOLLOWUPS-1): the root mix of an export is the stream the mixer
    /// plays. `seq-main` (no nest) with a clip pan envelope on clip 3 and a master volume lane
    /// exports PCM equal to `Viewer::bus_stream`'s render of the same routing within 1e-6 over frames
    /// [0, 20); negative control: pulse's plain `TimelineMixStream` of the same sequence differs by
    /// more than 1e-3 there (the export before this lane).
    #[cfg(feature = "audio-batch")]
    #[test]
    fn export_root_mix_equals_the_bus_stream_with_a_clip_pan_and_a_master_lane() {
        let (s, mut v) = opened("export-root-mix");
        let mut r = StudioAudioRouting::new("seq-main");
        r.clip_pans.push(pan_envelope(&cid(3)));
        r.master.volume_automation = vec![StudioAutomationPoint { offset_ticks: 0, value: 1.0 }, StudioAutomationPoint { offset_ticks: 16 * F, value: 0.25 }];
        let set = StudioAudioRoutingSet { routings: vec![r] };
        v.set_audio_routing(&set).unwrap();
        let mixer = main_mix(&v);
        let main = sequences(&v)[0].clone();
        let out = s.0.join("root-mix.mov");
        let bytes = run_job(job(main.clone(), all_solids(), Some(tone()), out.clone(), NestedDocument::new(sequences(&v), v.audio_routing().unwrap().unwrap())).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail)), &out);
        let (got, want) = (pcm(&bytes, 20), interleave(&mixer, 20));
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!((g - w).abs() <= 1e-6, "sample {i}: export {g} vs bus stream {w}");
        }
        assert!(want.iter().any(|x| x.abs() > 0.01), "the range is not silent");
        // Negative control: the plain mix ignores the pan envelope and the master lane.
        let cancel = CancellationToken::default();
        let mut plain = hsk_studio_pulse::TimelineMixStream::new(&main, 48_000, 960, tone(), &Known, hsk_studio_pulse::StreamLimits::default(), &cancel).unwrap();
        let n = 20 * SPF;
        let (mut l, mut rr) = (vec![0f32; n], vec![0f32; n]);
        for at in (0..n).step_by(960) {
            plain.render(&mut l[at..at + 960], &mut rr[at..at + 960], &cancel).unwrap();
        }
        let plain: Vec<f32> = l.iter().zip(&rr).flat_map(|(a, b)| [*a, *b]).collect();
        let worst = got.iter().zip(&plain).map(|(g, p)| (g - p).abs()).fold(0f32, f32::max);
        assert!(worst > 1e-3, "the plain mix must differ (worst {worst})");
    }

    /// A nested clip never exports black or silent: an inner video source the decoder cannot give
    /// fails the export typed (`export-frame-unavailable`, no output file); an inner audio source
    /// that is missing refuses the job `nest_unresolved` naming the nested sequence; a document
    /// that does not hold the nested sequence refuses it `nest_unresolved` naming the clip.
    #[test]
    fn export_of_a_nested_clip_with_a_missing_inner_source_fails_typed() {
        let (s, mut v) = opened("export-missing");
        v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        let doc = sequences(&v);
        let main = doc[0].clone();
        let inner = doc[1].sequence_id.clone();
        let nested = || NestedDocument::new(doc.clone(), StudioAudioRoutingSet::default());
        // Video: the nested clip's source `a` is missing (only `b`, which plays later, is there).
        let out = s.0.join("missing-video.mov");
        let mut queue = ExportQueue::new();
        let _handle = queue.submit(job(main.clone(), Solids(vec!["b"]), None::<NoAudio>, out.clone(), nested()).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail)));
        let receipt = queue.run().remove(0);
        match &receipt.outcome {
            ExportOutcome::Failed { detail, .. } => assert!(detail.contains("export-frame-unavailable"), "{detail}"),
            other => panic!("a missing inner frame must fail the export, got {other:?}"),
        }
        assert!(!out.exists(), "no output file");
        // Audio: the nested sequence's source `d` is missing.
        let e = job(main.clone(), all_solids(), Some(NoAudio), s.0.join("never.mov"), nested()).err().expect("refused");
        assert_eq!(e.code, "nest_unresolved");
        assert!(e.detail.contains(&inner), "{}", e.detail);
        // A document without the nested sequence.
        let partial = NestedDocument::new(vec![main.clone()], StudioAudioRoutingSet::default());
        let e = job(main.clone(), all_solids(), Some(tone()), s.0.join("never.mov"), partial).err().expect("refused");
        assert_eq!(e.code, "nest_unresolved");
        assert!(e.detail.starts_with(&nested_clip(&v).clip_id), "{}", e.detail);
        assert!(!s.0.join("never.mov").exists());
    }
}
