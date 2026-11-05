//! Lane SMB-PR-EDIT-FOOTPRINT-1 (feature `viewer`): a clip moved through the Viewer stays the same
//! clip. The move is a lift plus an assembly overwrite, and the assembly sets only what its request
//! names, so each test compares the whole stored clip with what it was. Only the position, and the
//! transitions the move reports as dropped, may differ. Driven by `timeline_command`, no window.
#![cfg(feature = "viewer")]

mod support_context;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::timeline::TimelineCommand;
use std::path::{Path, PathBuf};

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-harness-view-move-{}-{label}", std::process::id()));
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

/// `frames` long at timeline frame `start`, reading source frames from `si`; unit speed.
fn clip(n: u32, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: format!("manifest-{n}"),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        },
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: ClipRetime::default(),
    }
}

fn video(n: u32, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind: TrackKind::Video,
        index,
        name: format!("V{n}"),
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

const HELD_AT: u64 = 17;

/// Clip 1 holds a frame and states every retime choice, is disabled and has a marker.
fn held() -> StudioClip {
    let mut one = clip(1, 10, 20, 0);
    one.enabled = false;
    one.retime = ClipRetime {
        frame_interpolation: FrameInterpolation::FrameBlending,
        maintain_audio_pitch: true,
        frame_hold: Some(ClipFrameHold { source_tick: Ticks::new(HELD_AT * F), anchor: FrameHoldAnchor::Playhead }),
    };
    one.markers.push(StudioMarker {
        marker_id: "mk-1".into(),
        name: "keep".into(),
        comment: String::new(),
        color: LabelColor::Rose,
        start: Ticks::new(12 * F),
        duration: Ticks::new(0),
        role: None,
    });
    one
}

/// V1: 1 held [0, 20), 2 half speed [30, 50), 3 reversed [60, 70), 4 [80, 90) with its tail
/// transition onto 5 [90, 100). V2: empty.
fn sequence() -> StudioSequence {
    let mut slow = clip(2, 40, 10, 30);
    slow.speed = Speed { numerator: 1, denominator: 2 };
    slow.duration = Ticks::new(20 * F);
    let mut reversed = clip(3, 60, 10, 60);
    reversed.speed = Speed { numerator: -1, denominator: 1 };
    let mut outgoing = clip(4, 10, 10, 80);
    outgoing.transitions.tail = Some(TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: "tr-1".into(),
        transition_kind: "cross_dissolve".into(),
        duration: Ticks::new(2 * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    });
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-move".into(),
        name: "move".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(100 * F),
        tracks: vec![video(1, 0, vec![held(), slow, reversed, outgoing, clip(5, 100, 10, 90)]), video(2, 1, Vec::new())],
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

fn sequence_package(dir: &Path) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000007","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence()).unwrap()]
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
    let path = dir.join("move.handshake");
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

fn opened(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(label);
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&sequence_package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    v.set_monitor_sources(Box::new(NoFrames), Box::new(Long));
    (scratch, v)
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
fn status(v: &Viewer) -> String {
    v.state_json()["status"].to_string()
}
fn move_clip(v: &mut Viewer, n: u32, start: u64, track: Option<u32>) {
    let command = TimelineCommand::MoveClip { clip_id: cid(n), start: Ticks::new(start * F), track_id: track.map(tid) };
    v.timeline_command(command).unwrap_or_else(|e| panic!("{e}: {}", status(v)));
}
/// The frame the clip shows `frames` into its playing window.
fn shown(clip: &StudioClip, frames: u64) -> u64 {
    let tick = Ticks::new(clip.timeline_start.value() + frames * F);
    hsk_studio_pulse::clip_source_at(clip, tick, &Long, false).expect("the clip plays there").0.value() / F
}

#[test]
fn a_held_clip_moved_through_the_viewer_keeps_the_frame() {
    let (_scratch, mut v) = opened("held");
    let before = stored(&v, 1).1;
    assert_eq!((shown(&before, 0), shown(&before, 15)), (HELD_AT, HELD_AT), "the fixture holds");
    let untouched = seq(&v);

    move_clip(&mut v, 1, 200, None);
    let (track, moved) = stored(&v, 1);
    assert_eq!(track, tid(1));
    assert_eq!(moved.timeline_start, Ticks::new(200 * F));
    assert_eq!((shown(&moved, 0), shown(&moved, 15)), (HELD_AT, HELD_AT), "status: {}", status(&v));
    assert_eq!(moved.retime, before.retime, "the whole record stays, not only the hold");

    // One undo restores the sequence, hold and all.
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), untouched);

    // Onto another track of the same kind.
    move_clip(&mut v, 1, 5, Some(2));
    let (track, moved) = stored(&v, 1);
    assert_eq!(track, tid(2));
    assert_eq!((shown(&moved, 0), shown(&moved, 15)), (HELD_AT, HELD_AT), "status: {}", status(&v));
}

/// The stored clip after a move is the stored clip before it with only the position changed.
#[test]
fn a_move_changes_only_the_position_of_a_clip() {
    let (_scratch, mut v) = opened("members");
    for n in [1, 2, 3] {
        let before = stored(&v, n).1;
        move_clip(&mut v, n, 200 + u64::from(n) * 50, None);
        let expected = StudioClip { timeline_start: Ticks::new((200 + u64::from(n) * 50) * F), ..before };
        assert_eq!(stored(&v, n).1, expected, "clip {n}, status: {}", status(&v));
    }
}

/// The one thing a move loses: the transition, with a note naming it.
#[test]
fn a_moved_clip_loses_its_transition_and_the_note_says_so() {
    let (_scratch, mut v) = opened("transition");
    let before = stored(&v, 4).1;
    move_clip(&mut v, 4, 200, None);
    let expected = StudioClip { timeline_start: Ticks::new(200 * F), transitions: ClipTransitions::default(), ..before };
    assert_eq!(stored(&v, 4).1, expected, "status: {}", status(&v));
    let note = status(&v);
    assert!(note.contains("dropped transitions") && note.contains("tr-1"), "{note}");
}
