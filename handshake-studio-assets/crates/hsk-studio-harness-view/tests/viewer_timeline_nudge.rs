//! Lane SMB-PR-NUDGE-1 (feature `viewer`): every nudge STU-VID-035a names, driven through
//! `timeline_command` with no window. The oracle is hand-computed frames of the fixture below, the
//! run's document and the session history. Run with `--success-output immediate` to read the audit
//! table its first test prints. Premiere behaviour UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod support_context;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::timeline::TimelineCommand;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-harness-view-nudge-{}-{label}", std::process::id()));
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
fn tid(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}

/// Source frames `[si, so)` at timeline frame `start`, unit speed.
fn clip(n: u32, si: u64, so: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: format!("manifest-{n}"),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        },
        source_in: Ticks::new(si * F),
        source_out: Ticks::new(so * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new((so - si) * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: ClipRetime::default(),
    }
}

fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind,
        index,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        sync_locked: false,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan: 0.0,
    }
}

/// V1 (track 1): clip 1 source [10, 30) at [0, 20), clip 2 source [100, 130) at [20, 50), clip 3
/// source [10, 30) at [50, 70), clip 6 source [10, 20) at [100, 110), clip 9 source [10, 20) at
/// [300, 310) linked to clip 10. V2 (track 2): clip 7 source [10, 20) at [200, 210). V3 (track 3):
/// empty. A1 (track 4): clip 4 source [0, 20) at [60, 80), clip 10 source [10, 20) at [300, 310).
/// A2 (track 5): empty.
fn sequence() -> StudioSequence {
    let mut nine = clip(9, 10, 20, 300);
    nine.link_group = Some("lg-1".into());
    let mut ten = clip(10, 10, 20, 300);
    ten.link_group = Some("lg-1".into());
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-nudge".into(),
        name: "nudge".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(310 * F),
        tracks: vec![
            track(1, TrackKind::Video, 0, vec![clip(1, 10, 30, 0), clip(2, 100, 130, 20), clip(3, 10, 30, 50), clip(6, 10, 20, 100), nine]),
            track(2, TrackKind::Video, 1, vec![clip(7, 10, 20, 200)]),
            track(3, TrackKind::Video, 2, Vec::new()),
            track(4, TrackKind::Audio, 0, vec![clip(4, 0, 20, 60), ten]),
            track(5, TrackKind::Audio, 1, Vec::new()),
        ],
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

fn sequence_package(dir: &Path, seq: StudioSequence) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000007","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(seq).unwrap()]
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
    let path = dir.join("nudge.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Every source is 1000 frames long.
struct Long;
impl hsk_studio_pulse::SourceExtents for Long {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}
struct NoFrames;
impl hsk_studio_harness_view::view::monitor::FrameSource for NoFrames {
    fn frame(&self, _: &ClipSource, _: Ticks) -> Option<std::sync::Arc<hsk_studio_harness_view::view::monitor::SourceFrame>> {
        None
    }
}

fn open_in(v: &mut Viewer, dir: &Path, seq: StudioSequence) {
    v.open(&sequence_package(dir, seq)).unwrap_or_else(|e| panic!("{e}"));
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    v.set_monitor_sources(Box::new(NoFrames), Box::new(Long));
}

fn opened_with(label: &str, edit: impl FnOnce(&mut StudioSequence)) -> (Scratch, Viewer) {
    let scratch = Scratch::new(label);
    let mut seq = sequence();
    edit(&mut seq);
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    open_in(&mut v, &scratch.0, seq);
    (scratch, v)
}

fn opened(label: &str) -> (Scratch, Viewer) {
    opened_with(label, |_| {})
}

fn seq(v: &Viewer) -> StudioSequence {
    v.timeline_sequence().expect("timeline sequence").clone()
}
fn stored(v: &Viewer, n: u32) -> (String, StudioClip) {
    let s = seq(v);
    s.tracks
        .iter()
        .find_map(|t| t.clips.iter().find(|c| c.clip_id == cid(n)).map(|c| (t.track_id.clone(), c.clone())))
        .unwrap_or_else(|| panic!("clip {n} missing"))
}
/// (timeline start, end, source in, source out) of clip `n` in frames.
fn window(v: &Viewer, n: u32) -> (u64, u64, u64, u64) {
    let c = stored(v, n).1;
    (c.timeline_start.value() / F, c.timeline_end().unwrap().value() / F, c.source_in.value() / F, c.source_out.value() / F)
}
fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}
fn last_undo(v: &Viewer) -> String {
    v.run().history()["undo"].as_array().unwrap().last().and_then(Value::as_str).unwrap_or_default().to_owned()
}
fn step_of(v: &Viewer) -> Value {
    v.state_json()["timeline"]["multi_frame_step"].clone()
}
fn select(v: &mut Viewer, n: u32) {
    v.timeline_command(TimelineCommand::Select(Some(cid(n)))).unwrap();
}

/// Runs `command` on the selected clip `n`, prints one row, checks the windows it left and that one
/// undo restores the exact sequence.
fn row(v: &mut Viewer, action: &str, n: u32, command: TimelineCommand, expected: &[(u32, (u64, u64, u64, u64))]) {
    select(v, n);
    let before = seq(v);
    let undo = undo_len(v);
    let key = command.key();
    let spec = command.spec_command();
    v.timeline_command(command).unwrap_or_else(|e| panic!("{action}: {e}"));
    assert_eq!(undo_len(v), undo + 1, "{action}: one history step");
    let op = last_undo(v);
    for (clip_no, want) in expected {
        assert_eq!(window(v, *clip_no), *want, "{action}: clip {clip_no}");
    }
    println!("AUDIT|{action}|command={key}|spec={spec:?}|op={op}|windows={expected:?}");
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(v), before, "{action}: undo restores the exact sequence");
}

/// The audit of STU-VID-035a: one row per action the clause names, what reaches it and what it does.
#[test]
fn audit_every_action_the_clause_names() {
    let (_s, mut v) = opened("audit");
    assert_eq!(step_of(&v), serde_json::json!(5), "the default");
    let c6 = |start: u64| (start, start + 10, 10, 20);
    for (action, right, many, start) in [("clip left one", false, false, 99), ("clip right one", true, false, 101), ("clip left many", false, true, 95), ("clip right many", true, true, 105)] {
        row(&mut v, action, 6, TimelineCommand::ClipNudge { right, many }, &[(6, c6(start))]);
    }
    for (action, right, many, si) in [("slip left one", false, false, 99), ("slip right one", true, false, 101), ("slip left many", false, true, 95), ("slip right many", true, true, 105)] {
        row(&mut v, action, 2, TimelineCommand::SlipNudge { right, many }, &[(2, (20, 50, si, si + 30))]);
    }
    for (action, right, many, d) in [("slide left one", false, false, -1i64), ("slide right one", true, false, 1), ("slide left many", false, true, -5), ("slide right many", true, true, 5)] {
        let at = |base: u64| base.checked_add_signed(d).unwrap();
        let expected = [(1, (0, at(20), 10, at(30))), (2, (at(20), at(50), 100, 130)), (3, (at(50), 70, at(10), 30))];
        row(&mut v, action, 2, TimelineCommand::SlideNudge { right, many }, &expected);
    }
    // The one step: every "many" variant reads it.
    v.timeline_command(TimelineCommand::SetMultiFrameStep(3)).unwrap();
    assert_eq!(step_of(&v), serde_json::json!(3));
    row(&mut v, "clip right many, step 3", 6, TimelineCommand::ClipNudge { right: true, many: true }, &[(6, c6(103))]);
    row(&mut v, "slip right many, step 3", 2, TimelineCommand::SlipNudge { right: true, many: true }, &[(2, (20, 50, 103, 133))]);
    row(&mut v, "slide left many, step 3", 2, TimelineCommand::SlideNudge { right: false, many: true }, &[(1, (0, 17, 10, 27)), (2, (17, 47, 100, 130)), (3, (47, 70, 7, 30))]);
    println!("AUDIT|clip up and down|no TimelineCommand variant at base; pulse has no planner that changes a track");
}

/// Facts the audit reports about the rows above: where a nudge refuses and what it overwrites. They
/// pin behaviour this lane found and did not change.
#[test]
fn audit_what_a_left_or_right_clip_nudge_does_at_its_edges() {
    let (_s, mut v) = opened("edges");
    // Left past zero refuses, where a slip or slide would commit the achieved delta.
    select(&mut v, 1);
    let code = v.timeline_command(TimelineCommand::ClipNudge { right: false, many: false }).unwrap_err();
    println!("AUDIT|clip left past zero|refused {code}");
    assert_eq!(code, "outside_range");
    // A linked clip refuses (`plan_move` refuses a link group).
    select(&mut v, 9);
    let code = v.timeline_command(TimelineCommand::ClipNudge { right: true, many: false }).unwrap_err();
    println!("AUDIT|clip right, linked|refused {code}");
    assert_eq!(code, "linked_move_unsupported");
    // Into a neighbour it overwrites: `MoveClip` is a lift plus an overwrite.
    select(&mut v, 2);
    v.timeline_command(TimelineCommand::ClipNudge { right: true, many: false }).unwrap();
    println!("AUDIT|clip right into a neighbour|clip 2 {:?}, clip 3 {:?}", window(&v, 2), window(&v, 3));
    assert_eq!(window(&v, 2), (21, 51, 100, 130));
    assert_eq!(window(&v, 3), (51, 70, 11, 30), "the neighbour lost a frame to the overwrite");
    v.shell_command(ShellCommand::Undo).unwrap();
    // A slide needs both neighbours abutting.
    select(&mut v, 6);
    let code = v.timeline_command(TimelineCommand::SlideNudge { right: true, many: false }).unwrap_err();
    println!("AUDIT|slide with no abutting neighbours|refused {code}");
    assert_eq!(code, "not_adjacent");
}

/// How long the one step lasts: as long as the document's timeline. The panel entry only hides and
/// shows an open timeline; closing the document drops it, and the next one starts at the default.
#[test]
fn audit_how_long_the_step_lasts() {
    let (s, mut v) = opened("lasts");
    v.timeline_command(TimelineCommand::SetMultiFrameStep(3)).unwrap();
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    println!("AUDIT|step 3 after the panel entry hides the timeline|step {}", step_of(&v));
    assert_eq!(step_of(&v), serde_json::json!(3));
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    assert_eq!(step_of(&v), serde_json::json!(3), "shown again");
    v.close();
    assert_eq!(v.state_json()["timeline"], Value::Null);
    open_in(&mut v, &s.0, sequence());
    println!("AUDIT|step 3 after the document closes and a document opens|step {}", step_of(&v));
    assert_eq!(step_of(&v), serde_json::json!(5), "not kept: no store holds it");
}

// ---- Up and down by one track: the command the audit found missing.

fn track_nudge(v: &mut Viewer, up: bool) -> Result<(), String> {
    v.timeline_command(TimelineCommand::ClipTrackNudge { up })
}
fn status(v: &Viewer) -> String {
    v.state_json()["status"].to_string()
}

/// Up to V2, up to V3, no further. One undo step each; undo and redo restore the exact sequences.
#[test]
fn up_and_down_move_the_selected_clip_one_track_with_one_undo() {
    let (_s, mut v) = opened("updown");
    select(&mut v, 6);
    let base = seq(&v);
    let clip = stored(&v, 6).1;
    let undo = undo_len(&v);
    track_nudge(&mut v, true).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_eq!(stored(&v, 6), (tid(2), clip.clone()), "the clip keeps its time and every field");
    assert_eq!(undo_len(&v), undo + 1, "one history step");
    assert!(last_undo(&v).starts_with("timeline_clip_track_nudge"), "{}", last_undo(&v));
    let on_v2 = seq(&v);
    track_nudge(&mut v, true).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_eq!(stored(&v, 6), (tid(3), clip.clone()));
    assert_eq!(track_nudge(&mut v, true).unwrap_err(), "nudge_no_track");
    assert!(status(&v).contains("nudge_no_track"), "{}", status(&v));
    assert_eq!(undo_len(&v), undo + 2, "a refusal is no step");
    track_nudge(&mut v, false).unwrap();
    assert_eq!(stored(&v, 6).0, tid(2), "down");
    v.shell_command(ShellCommand::Undo).unwrap();
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), on_v2);
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), base, "undo restores the exact sequence");
    v.shell_command(ShellCommand::Redo).unwrap();
    assert_eq!(seq(&v), on_v2, "redo");
}

/// Everything a clip holds survives: speed, retime, the hold, the marker, disabled.
#[test]
fn a_track_nudge_keeps_every_member_of_the_clip() {
    let (_s, mut v) = opened_with("keeps", |s| {
        let c = s.tracks[0].clips.iter_mut().find(|c| c.clip_id == cid(6)).unwrap();
        c.speed = Speed { numerator: 1, denominator: 2 };
        c.duration = Ticks::new(20 * F);
        c.enabled = false;
        c.retime = ClipRetime {
            frame_interpolation: FrameInterpolation::FrameBlending,
            maintain_audio_pitch: true,
            frame_hold: Some(ClipFrameHold { source_tick: Ticks::new(17 * F), anchor: FrameHoldAnchor::Playhead }),
        };
        c.markers.push(StudioMarker { marker_id: "mk-1".into(), name: "keep".into(), comment: String::new(), color: LabelColor::Rose, start: Ticks::new(12 * F), duration: Ticks::new(0), role: None });
    });
    select(&mut v, 6);
    let before = stored(&v, 6).1;
    track_nudge(&mut v, true).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_eq!(stored(&v, 6), (tid(2), before), "status: {}", status(&v));
}

/// The partner takes the same index step, so V1 + A1 go to V2 + A2. One undo step for both.
#[test]
fn a_linked_clip_takes_its_partner_along() {
    let (_s, mut v) = opened("linked");
    select(&mut v, 9);
    let base = seq(&v);
    let undo = undo_len(&v);
    track_nudge(&mut v, true).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_eq!((stored(&v, 9).0, stored(&v, 10).0), (tid(2), tid(5)));
    assert_eq!(undo_len(&v), undo + 1);
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), base);
}

#[test]
fn refusals_are_named_and_change_nothing() {
    let (_s, mut v) = opened("none");
    assert_eq!(track_nudge(&mut v, true).unwrap_err(), "no_selection");
    let (_s, mut v) = opened_with("locked", |s| s.tracks[1].locked = true);
    select(&mut v, 6);
    let base = seq(&v);
    let undo = undo_len(&v);
    assert_eq!(track_nudge(&mut v, true).unwrap_err(), "track_locked");
    assert_eq!((seq(&v), undo_len(&v)), (base, undo));
    // Clip 8 on V1 sits under clip 7 on V2: refused, not overwritten.
    let (_s, mut v) = opened_with("occupied", |s| s.tracks[0].clips.insert(4, clip(8, 10, 20, 205)));
    select(&mut v, 8);
    let base = seq(&v);
    assert_eq!(track_nudge(&mut v, true).unwrap_err(), "nudge_range_occupied");
    assert_eq!(seq(&v), base);
}

/// The planner's up and down are the rows the panel draws: one row up or down within the kind.
#[test]
fn up_lands_on_the_row_above_in_the_drawn_order() {
    let (_s, mut v) = opened_with("rows", |s| s.tracks[4].clips.push(clip(11, 10, 15, 0)));
    let row = |v: &Viewer, n: u32| {
        let s = seq(v);
        let track = stored(v, n).0;
        let drawn = hsk_studio_harness_view::view::timeline::track_order(&s);
        drawn.iter().position(|i| s.tracks[*i].track_id == track).unwrap()
    };
    for (n, up) in [(6, true), (6, false), (11, true), (11, false)] {
        select(&mut v, n);
        let before = row(&v, n);
        track_nudge(&mut v, up).unwrap_or_else(|e| panic!("clip {n}: {e}: {}", status(&v)));
        let want = if up { before - 1 } else { before + 1 };
        assert_eq!(row(&v, n), want, "clip {n} up={up}");
    }
}

/// A transition that stops playing is removed, the status note names it, undo brings it back.
#[test]
fn a_transition_that_stops_playing_is_noted() {
    let (_s, mut v) = opened_with("transition", |s| {
        s.tracks[0].clips[1].transitions.tail = Some(TimelineTransition {
            schema_id: TransitionSchema::V1,
            transition_id: "tr-1".into(),
            transition_kind: "cross_dissolve".into(),
            duration: Ticks::new(2 * F),
            alignment: TransitionAlignment::CenteredOnCut,
            center_offset: 0,
            parameters: Vec::new(),
            repeat_end_frame: false,
        });
    });
    select(&mut v, 2);
    let base = seq(&v);
    track_nudge(&mut v, true).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_eq!(stored(&v, 2).1.transitions.tail, None);
    let note = status(&v);
    assert!(note.contains("dropped transitions") && note.contains("tr-1"), "{note}");
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), base);
}

/// The module 14 table names all six ([STU-VID-041]); left and right had no id at base.
#[test]
fn every_clip_nudge_names_its_spec_command() {
    let ids = [
        TimelineCommand::ClipNudge { right: false, many: false },
        TimelineCommand::ClipNudge { right: false, many: true },
        TimelineCommand::ClipNudge { right: true, many: false },
        TimelineCommand::ClipNudge { right: true, many: true },
        TimelineCommand::ClipTrackNudge { up: true },
        TimelineCommand::ClipTrackNudge { up: false },
    ]
    .map(|c| c.spec_command());
    assert_eq!(
        ids,
        ["cmd.timeline.nudge.left.one", "cmd.timeline.nudge.left.several", "cmd.timeline.nudge.right.one", "cmd.timeline.nudge.right.several", "cmd.timeline.nudge.up", "cmd.timeline.nudge.down"].map(Some)
    );
}
