//! Lane SMB-PR-NEST-1 (STU-VID-030 / STU-VID-040): Nest, Make Subsequence and nested playback
//! resolution. Undo and redo are proven by applying the proposal's inverse and changes through
//! `apply_sequence_changes` (Folio-validated). Premiere behaviour (which track receives the nested
//! clip, track mirroring, the subsequence range cut) is UNVERIFIED (D5): the asserted values are
//! the declared Studio readings of module `nest`.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 60s deadline: {e}"),
    }
}

/// 25 fps ticks per frame; 48 kHz has 1920 samples per frame.
const F: u64 = 10_160_640_000;
const RATE: u32 = 48_000;

fn ident(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn cid(n: u32) -> String {
    ident("SCLP", n)
}
fn tid(n: u32) -> String {
    ident("STRK", n)
}

struct Media;
impl Resolver for Media {
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
        true
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

fn media(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab01".into() }, asset_id: None }
}
fn nested(id: &str) -> ClipSource {
    ClipSource::Sequence { sequence_id: id.into() }
}
/// Clip `n` of `source`: source frames `[si, si + frames)` at timeline frame `start`.
fn clip(n: u32, source: ClipSource, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: source,
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn linked(mut c: StudioClip, group: &str) -> StudioClip {
    c.link_group = Some(group.into());
    c
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
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        volume: 1.0,
        pan: 0.0,
        clips,
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
fn document(sequences: Vec<StudioSequence>) -> Snapshot {
    let unassigned = || ProfileBinding::Unassigned(());
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: ident("SDOC", 1),
        revision: 3,
        geometry_unit: GeometryUnit::Px,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences,
    };
    validate_document(document, Budget::default(), &CancellationToken::default(), &Media).expect("fixture is a valid Folio document")
}

struct Known;
impl SourceExtents for Known {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}
fn ctx<'a>(extents: &'a dyn SourceExtents, cancel: &'a CancellationToken) -> EditContext<'a> {
    EditContext { budget: Budget::default(), resolver: &Media, extents, cancel }
}

/// `seq-main`: V1 (idx 0) v1 src 0..10 @5 linked lg-a with A1 a1, and v3 src 0..10 @30; V2 (idx 1)
/// v2 src 0..8 @10 linked lg-b with A2 (idx 1) a2.
fn main_sequence() -> StudioSequence {
    sequence(
        "seq-main",
        vec![
            track(1, TrackKind::Video, 0, vec![linked(clip(1, media("v1"), 0, 10, 5), "lg-a"), clip(3, media("v3"), 0, 10, 30)]),
            track(2, TrackKind::Video, 1, vec![linked(clip(2, media("v2"), 0, 8, 10), "lg-b")]),
            track(11, TrackKind::Audio, 0, vec![linked(clip(11, media("a1"), 0, 10, 5), "lg-a")]),
            track(12, TrackKind::Audio, 1, vec![linked(clip(12, media("a2"), 0, 8, 10), "lg-b")]),
        ],
    )
}
fn nest_ids(sequence_id: &str) -> NestIds {
    NestIds {
        sequence_id: sequence_id.into(),
        sequence_name: "Nested Sequence 1".into(),
        track_ids: (60..64).map(tid).collect(),
        clip_ids: (50..60).map(cid).collect(),
        link_group: Some("lg-nest".into()),
        record_ids: (0..8).map(|n| format!("rec-{n}")).collect(),
    }
}
/// Folio's encoded document bytes at a fixed revision (the step changes the revision only).
fn bytes(snapshot: &Snapshot) -> Vec<u8> {
    let mut d = snapshot.document().clone();
    d.revision = 0;
    validate_document(d, Budget::default(), &CancellationToken::default(), &Media).unwrap().encoded_bytes().to_vec()
}
fn find<'a>(s: &'a Snapshot, id: &str) -> &'a StudioSequence {
    s.document().sequences.iter().find(|q| q.sequence_id == id).unwrap()
}

#[test]
fn nest_replaces_linked_selection_with_linked_nested_clips_and_undoes_byte_identical() {
    bounded_case("nest", || {
        let snap = document(vec![main_sequence()]);
        let cancel = CancellationToken::default();
        let c = ctx(&Known, &cancel);
        // v1 and v2 named; their link groups bring a1 and a2.
        let p = plan_nest(&snap, "seq-main", 3, &[cid(1), cid(2)], &nest_ids("seq-nest-1"), &c).unwrap();
        assert_eq!(p.command_id(), CMD_NEST);
        assert_eq!(p.target_revision(), 4);
        let main = find(p.snapshot(), "seq-main");
        let inner = find(p.snapshot(), "seq-nest-1");
        // Parent: V1 holds the nested video clip (lowest video track) then v3; V2 is empty; A1 the
        // nested audio clip; A2 empty. Both span the selection [5, 18) frames, linked.
        let v = &main.tracks[0].clips;
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].clip_id.as_str(), v[0].timeline_start, v[0].duration), (cid(50).as_str(), Ticks::new(5 * F), Ticks::new(13 * F)));
        assert_eq!(v[0].source_ref, nested("seq-nest-1"));
        assert_eq!((v[0].source_in, v[0].source_out), (Ticks::new(0), Ticks::new(13 * F)));
        assert_eq!(v[1].clip_id, cid(3));
        assert!(main.tracks[1].clips.is_empty() && main.tracks[3].clips.is_empty());
        let a = &main.tracks[2].clips;
        assert_eq!((a.len(), a[0].clip_id.as_str(), a[0].timeline_start), (1, cid(51).as_str(), Ticks::new(5 * F)));
        assert_eq!(v[0].link_group.as_deref(), Some("lg-nest"));
        assert_eq!(a[0].link_group.as_deref(), Some("lg-nest"));
        assert_eq!(p.nested_clip_ids(), [cid(50), cid(51)]);
        // New sequence: mirrored tracks (V1, V2, A1, A2 order, names kept), clips re-based to 0.
        assert_eq!(inner.frame_rate, main.frame_rate);
        let names: Vec<(&str, TrackKind, u32)> = inner.tracks.iter().map(|t| (t.name.as_str(), t.kind, t.index)).collect();
        assert_eq!(names, [("track-1", TrackKind::Video, 0), ("track-2", TrackKind::Video, 1), ("track-11", TrackKind::Audio, 0), ("track-12", TrackKind::Audio, 1)]);
        assert_eq!(inner.tracks[0].track_id, tid(60));
        let starts: Vec<(String, u64)> = inner.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.clip_id.clone(), c.timeline_start.value() / F)).collect();
        assert_eq!(starts, [(cid(1), 0), (cid(2), 5), (cid(11), 0), (cid(12), 5)]);
        assert_eq!(inner.duration, Ticks::new(13 * F));
        // The prospective document validated; undo restores the sequences byte-identical, redo
        // re-applies the step.
        let undone = apply_sequence_changes(p.snapshot(), 4, p.inverse(), &c).unwrap();
        assert_eq!(bytes(&undone), bytes(&snap));
        let redone = apply_sequence_changes(&undone, 5, p.changes(), &c).unwrap();
        assert_eq!(bytes(&redone), bytes(p.snapshot()));
    });
}

#[test]
fn make_subsequence_from_in_to_out_cuts_at_the_edges_and_leaves_the_parent() {
    bounded_case("subsequence", || {
        let snap = document(vec![main_sequence()]);
        let cancel = CancellationToken::default();
        let c = ctx(&Known, &cancel);
        let range = TickRange::new(Ticks::new(8 * F), Ticks::new(14 * F)).unwrap();
        let p = plan_make_subsequence(&snap, "seq-main", 3, &SubsequenceSource::Range(range), &nest_ids("seq-sub"), &c).unwrap();
        assert_eq!(p.command_id(), CMD_MAKE_SUBSEQUENCE_IN_OUT);
        assert_eq!(p.changes().len(), 1, "insert only");
        assert_eq!(find(p.snapshot(), "seq-main"), find(&snap, "seq-main"), "parent unchanged");
        let sub = find(p.snapshot(), "seq-sub");
        // v1 [5,15) -> [8,14) src 3..9 @0; v2 [10,18) -> [10,14) src 0..4 @2; same for a1 / a2; v3 outside.
        let got: Vec<(String, u64, u64, u64, u64)> = sub
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| (c.clip_id.clone(), c.source_in.value() / F, c.source_out.value() / F, c.timeline_start.value() / F, c.duration.value() / F))
            .collect();
        assert_eq!(got, [(cid(50), 3, 9, 0, 6), (cid(51), 0, 4, 2, 4), (cid(52), 3, 9, 0, 6), (cid(53), 0, 4, 2, 4)]);
        assert_eq!(sub.duration, Ticks::new(6 * F));
        let undone = apply_sequence_changes(p.snapshot(), 4, p.inverse(), &c).unwrap();
        assert_eq!(bytes(&undone), bytes(&snap));
    });
}

#[test]
fn nest_refusals_are_typed_and_change_nothing() {
    bounded_case("refusals", || {
        // seq-inner is nested by clip 20 of seq-main.
        let inner = sequence("seq-inner", vec![track(30, TrackKind::Video, 0, vec![clip(30, media("i"), 0, 10, 0)])]);
        let mut main = main_sequence();
        main.tracks[1].clips.push(clip(20, nested("seq-inner"), 0, 10, 20));
        main.duration = main.content_end().unwrap();
        let snap = document(vec![inner, main]);
        let before = bytes(&snap);
        let cancel = CancellationToken::default();
        let c = ctx(&Known, &cancel);
        let code = |clips: &[String], ids: &NestIds| plan_nest(&snap, "seq-main", 3, clips, ids, &c).unwrap_err().code();
        assert_eq!(code(&[], &nest_ids("seq-n")), "nest_empty_selection");
        // Cycle: the new sequence would be the parent itself, or the sequence a selected clip nests.
        assert_eq!(code(&[cid(1)], &nest_ids("seq-main")), "nest_cycle");
        assert_eq!(code(&[cid(20)], &nest_ids("seq-inner")), "nest_cycle");
        // Hostile ids: an existing sequence id, a repeated fresh id, an id already in the document,
        // a path-like sequence id (not a Folio key), a link group in use.
        let mut existing = inner_ids();
        existing.sequence_id = "seq-inner".into();
        assert_eq!(code(&[cid(3)], &existing), "duplicate_id");
        let mut repeated = nest_ids("seq-n");
        repeated.track_ids = vec![tid(60), tid(60), tid(61), tid(62)];
        assert_eq!(code(&[cid(1), cid(2)], &repeated), "duplicate_id");
        let mut taken = nest_ids("seq-n");
        taken.clip_ids[0] = cid(3);
        assert_eq!(code(&[cid(1)], &taken), "duplicate_id");
        assert_eq!(code(&[cid(1)], &nest_ids("../seq/evil")), "folio_rejected");
        let mut group = nest_ids("seq-n");
        group.link_group = Some("lg-b".into());
        assert_eq!(code(&[cid(1)], &group), "duplicate_id");
        assert_eq!(code(&[cid(99)], &nest_ids("seq-n")), "unknown_clip");
        // A locked track (here A1, reached through v1's link group).
        let mut locked = snap.document().clone();
        locked.sequences[1].tracks[2].locked = true;
        let locked = validate_document(locked, Budget::default(), &cancel, &Media).unwrap();
        assert_eq!(plan_nest(&locked, "seq-main", 3, &[cid(1)], &nest_ids("seq-n"), &c).unwrap_err().code(), "track_locked");
        // Make Subsequence: empty range, stale revision.
        let empty = TickRange::new(Ticks::new(40 * F), Ticks::new(41 * F)).unwrap();
        assert_eq!(plan_make_subsequence(&snap, "seq-main", 3, &SubsequenceSource::Range(empty), &nest_ids("seq-n"), &c).unwrap_err().code(), "nest_empty_selection");
        assert_eq!(plan_make_subsequence(&snap, "seq-main", 2, &SubsequenceSource::Clips(vec![cid(1)]), &nest_ids("seq-n"), &c).unwrap_err().code(), "stale_revision");
        assert_eq!(bytes(&snap), before, "every refusal leaves the document unchanged");
    });
}
fn inner_ids() -> NestIds {
    nest_ids("seq-x")
}

/// Media layer source tick of `layers` (one layer expected).
fn single(frame: &NestedFrame) -> (&ClipSource, Ticks, Vec<String>) {
    assert!(frame.unresolved.is_empty(), "{:?}", frame.unresolved);
    assert_eq!(frame.layers.len(), 1, "{:?}", frame.layers);
    let l = &frame.layers[0];
    (&l.layer.source, l.layer.source_tick, l.path.clone())
}

#[test]
fn nested_frame_maps_speed_and_resolves_three_levels() {
    bounded_case("frames", || {
        // seq-c: media c0 src 0..100 @0. seq-b: clip 2 -> seq-c window 10..60 @0. seq-a: clip 1 ->
        // seq-b at speed 2 (source window 0..40, 20 frames) @5.
        let c_seq = sequence("seq-c", vec![track(3, TrackKind::Video, 0, vec![clip(3, media("c0"), 0, 100, 0)])]);
        let b_seq = sequence("seq-b", vec![track(2, TrackKind::Video, 0, vec![clip(2, nested("seq-c"), 10, 50, 0)])]);
        let mut fast = clip(1, nested("seq-b"), 0, 40, 5);
        fast.speed = Speed { numerator: 2, denominator: 1 };
        fast.duration = Ticks::new(20 * F);
        let a_seq = sequence("seq-a", vec![track(1, TrackKind::Video, 0, vec![fast])]);
        let snap = document(vec![c_seq.clone(), b_seq, a_seq.clone()]);
        let sequences = &snap.document().sequences;
        let cancel = CancellationToken::default();
        // Timeline frame 8 of seq-a: 3 frames into the clip at 2x = seq-b tick 6 frames; seq-b's
        // clip reads seq-c from 10, so seq-c tick 16 frames = media c0 source tick 16 frames.
        let frame = frame_layers_nested(sequences, &a_seq, Ticks::new(8 * F), &Known, 64, &cancel).unwrap();
        let (source, tick, path) = single(&frame);
        assert_eq!((source, tick), (&media("c0"), Ticks::new(16 * F)));
        assert_eq!(path, [cid(1), cid(2)]);
        // Equal to the inner sequence's own frame at the hand-computed tick.
        let inner = frame_layers(&c_seq, Ticks::new(16 * F), &Known, &cancel).unwrap();
        assert_eq!(frame.layers[0].layer, inner[0]);
        // The plain frame_layers still hands the Sequence source to the host (unchanged API).
        let plain = frame_layers(&a_seq, Ticks::new(8 * F), &Known, &cancel).unwrap();
        assert_eq!(plain[0].source, nested("seq-b"));
        // The layer budget bounds the expansion.
        assert_eq!(frame_layers_nested(sequences, &a_seq, Ticks::new(8 * F), &Known, 0, &cancel).unwrap_err().code(), "limit_exceeded");
        // Opaque to timeline operations: trimming the nested clip's tail later is bounded by the
        // nested sequence's duration (NestedExtents), and moves only the window onto seq-b.
        let ext = NestedExtents::new(sequences, &hsk_studio_pulse::UnknownExtents);
        assert_eq!(ext.extent(&nested("seq-b")), Some(Ticks::new(50 * F)));
        let c = EditContext { budget: Budget::default(), resolver: &Media, extents: &ext, cancel: &cancel };
        let trim = TimelineEdit::TrimOut { clip_id: cid(1), new_duration: Ticks::new(25 * F), ripple: false };
        let p = plan_timeline_edit(&snap, "seq-a", 3, &trim, &c).unwrap();
        let after = &find(p.snapshot(), "seq-a").tracks[0].clips[0];
        assert_eq!((after.source_in, after.source_out), (Ticks::new(0), Ticks::new(50 * F)));
        assert_eq!(find(p.snapshot(), "seq-b"), find(&snap, "seq-b"), "the nested sequence is untouched");
        let past = TimelineEdit::TrimOut { clip_id: cid(1), new_duration: Ticks::new(26 * F), ripple: false };
        assert_eq!(plan_timeline_edit(&snap, "seq-a", 3, &past, &c).unwrap_err().code(), "past_source_end");
    });
}

/// Stereo test tone of 100 frames for "tone"; nothing else.
#[derive(Clone)]
struct Tone(Vec<Vec<f32>>);
impl AudioSources for Tone {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        match s {
            ClipSource::Media { artifact_manifest_id, .. } if artifact_manifest_id == "tone" => Some(&self.0),
            _ => None,
        }
    }
}
#[derive(Clone)]
struct NoAudio;
impl AudioSources for NoAudio {
    fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
        None
    }
}
fn tone() -> Tone {
    let n = 100 * 1920;
    let l: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
    let r: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.013).cos() * 0.25).collect();
    Tone(vec![l, r])
}
fn render<S: AudioSources>(mut stream: TimelineMixStream<S>, frames: usize) -> [Vec<f32>; 2] {
    let cancel = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut at = 0;
    while at < frames {
        let n = 960.min(frames - at);
        stream.render(&mut l[at..at + n], &mut r[at..at + n], &cancel).unwrap();
        at += n;
    }
    [l, r]
}

#[test]
fn nested_audio_equals_the_inner_mix_and_an_unresolvable_inner_source_is_reported() {
    bounded_case("audio", || {
        // seq-n: A1 tone src 2..12 @1 (11 frames long); seq-top: A1 clip -> seq-n 0..11 @3.
        let inner = sequence("seq-n", vec![track(5, TrackKind::Audio, 0, vec![clip(5, media("tone"), 2, 10, 1)])]);
        let top = sequence("seq-top", vec![track(6, TrackKind::Audio, 0, vec![clip(6, nested("seq-n"), 0, 11, 3)])]);
        let snap = document(vec![inner.clone(), top.clone()]);
        let sequences = &snap.document().sequences;
        let cancel = CancellationToken::default();
        let limits = StreamLimits::default();
        let sources = NestedAudioSources::new(sequences, &top, tone(), RATE, 960, &Known, limits, &cancel).unwrap();
        assert!(sources.reports().is_empty(), "{:?}", sources.reports());
        let ext = NestedExtents::new(sequences, &Known);
        let parent = render(TimelineMixStream::new(&top, RATE, 960, sources, &ext, limits, &cancel).unwrap(), 14 * 1920);
        let alone = render(TimelineMixStream::new(&inner, RATE, 960, tone(), &Known, limits, &cancel).unwrap(), 11 * 1920);
        // The parent plays the inner mix from tick 0 at its own frame 3, sample for sample.
        for ch in 0..2 {
            assert!(parent[ch][..3 * 1920].iter().all(|v| *v == 0.0));
            for (i, want) in alone[ch].iter().enumerate() {
                let got = parent[ch][3 * 1920 + i];
                assert!((got - want).abs() <= 1e-6, "ch {ch} sample {i}: {got} vs {want}");
            }
            assert!(alone[ch].iter().any(|v| v.abs() > 0.01), "the tone is audible");
        }
        // The inner media is missing: a typed report for the nested sequence, and the parent mix
        // still renders (the nested clip plays as silence of its length; nothing is dropped silently).
        let missing = NestedAudioSources::new(sequences, &top, NoAudio, RATE, 960, &Known, limits, &cancel).unwrap();
        assert_eq!(missing.reports(), [NestAudioReport { sequence_id: "seq-n".into(), clip_ids: vec![cid(6)], code: "source_extent_unknown" }]);
        let silent = render(TimelineMixStream::new(&top, RATE, 960, missing, &ext, limits, &cancel).unwrap(), 14 * 1920);
        assert!(silent[0].iter().chain(silent[1].iter()).all(|v| *v == 0.0));
        // The nested buffers are bounded before allocation.
        let tight = StreamLimits { max_resident_frames: 10 * 1920, ..limits };
        assert_eq!(NestedAudioSources::new(sequences, &top, tone(), RATE, 960, &Known, tight, &cancel).err().map(|e| e.code()), Some("limit_exceeded"));
    });
}

// ---- lane SMB-PR-NEST-2: clip envelopes follow a nest ----

fn gains(clip: &str) -> StudioClipGainEnvelope {
    StudioClipGainEnvelope { clip_id: clip.into(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -9.0 }, StudioClipGainPoint { offset_ticks: 6 * F, gain_db: 2.0 }] }
}
fn pans(clip: &str) -> StudioClipPanEnvelope {
    StudioClipPanEnvelope { clip_id: clip.into(), points: vec![StudioClipPanPoint { offset_ticks: F, pan: -0.75 }, StudioClipPanPoint { offset_ticks: 8 * F, pan: 0.6 }] }
}

/// The parent mix before a nest equals the mix after it, sample for sample within 1e-6, when the
/// nested clip's gain and pan envelopes move (unchanged, clip-relative offsets) into the routing of
/// the new sequence and the nested mix applies them (`NestedAudioSources::with_routing`); without
/// them (`new`) the nested mix differs, so the envelopes are what the equality proves.
#[test]
fn nested_mix_applies_moved_clip_envelopes() {
    bounded_case("envelopes", || {
        // seq-top: A1 tone src 2..12 @1 with a gain and a pan envelope.
        let top = sequence("seq-top", vec![track(6, TrackKind::Audio, 0, vec![clip(6, media("tone"), 2, 10, 1)])]);
        let snap = document(vec![top.clone()]);
        let cancel = CancellationToken::default();
        let c = ctx(&Known, &cancel);
        let limits = StreamLimits::default();
        let (g, p) = (gains(&cid(6)), pans(&cid(6)));
        let before = render(TimelineMixStream::with_clip_envelopes(&top, RATE, 960, tone(), &Known, limits, std::slice::from_ref(&g), std::slice::from_ref(&p), &cancel).unwrap(), 14 * 1920);
        let proposal = plan_nest(&snap, "seq-top", 3, &[cid(6)], &nest_ids("seq-n"), &c).unwrap();
        let sequences = &proposal.snapshot().document().sequences;
        let after_top = find(proposal.snapshot(), "seq-top").clone();
        // The host's move: the parent keeps no envelope of the moved clip, the new sequence's
        // routing holds them unchanged (offsets are ticks from the clip's own timeline_start).
        let mut routing = StudioAudioRouting::new("seq-n");
        routing.clip_gains.push(g.clone());
        routing.clip_pans.push(p.clone());
        let set = StudioAudioRoutingSet { routings: vec![StudioAudioRouting::new("seq-top"), routing] };
        validate_audio_routing(&set, proposal.snapshot().document(), Budget::default(), &cancel).expect("the moved envelopes validate on the nested sequence's audio track");
        let ext = NestedExtents::new(sequences, &Known);
        let nested = NestedAudioSources::with_routing(sequences, &after_top, tone(), &set, RATE, 960, &Known, limits, &cancel).unwrap();
        assert!(nested.reports().is_empty(), "{:?}", nested.reports());
        let after = render(TimelineMixStream::with_clip_envelopes(&after_top, RATE, 960, nested, &ext, limits, &[], &[], &cancel).unwrap(), 14 * 1920);
        for ch in 0..2 {
            for (i, (want, got)) in before[ch].iter().zip(&after[ch]).enumerate() {
                assert!((got - want).abs() <= 1e-6, "ch {ch} sample {i}: {got} vs {want}");
            }
        }
        assert!(before[0].iter().any(|v| v.abs() > 0.01) && before[1].iter().any(|v| v.abs() > 0.01), "audible on both channels");
        // `new` (no routing) is the plain nested mix: it differs from the enveloped parent mix.
        let plain = NestedAudioSources::new(sequences, &after_top, tone(), RATE, 960, &Known, limits, &cancel).unwrap();
        let unenveloped = render(TimelineMixStream::new(&after_top, RATE, 960, plain, &ext, limits, &cancel).unwrap(), 14 * 1920);
        let worst = (0..2).flat_map(|ch| before[ch].iter().zip(&unenveloped[ch]).map(|(a, b)| (a - b).abs()).collect::<Vec<_>>()).fold(0f32, f32::max);
        assert!(worst > 0.05, "the envelopes change the mix (worst difference {worst})");
        // An envelope the nested mix refuses (an offset off the sample grid) is reported, never
        // dropped silently.
        let mut bad = set.clone();
        bad.routings[1].clip_gains[0].points[1].offset_ticks = 6 * F + 1;
        let refused = NestedAudioSources::with_routing(sequences, &after_top, tone(), &bad, RATE, 960, &Known, limits, &cancel).unwrap();
        assert_eq!(refused.reports(), [NestAudioReport { sequence_id: "seq-n".into(), clip_ids: proposal.nested_clip_ids().to_vec(), code: "not_frame_aligned" }]);
    });
}

/// Make Subsequence lists `(parent clip id, copy id)` per copy (range pieces included), so the host
/// can copy per-clip records under the fresh ids; Nest lists none (its clips keep their ids).
#[test]
fn make_subsequence_lists_the_copies_of_each_parent_clip() {
    bounded_case("copies", || {
        let snap = document(vec![main_sequence()]);
        let cancel = CancellationToken::default();
        let c = ctx(&Known, &cancel);
        let range = TickRange::new(Ticks::new(8 * F), Ticks::new(14 * F)).unwrap();
        let p = plan_make_subsequence(&snap, "seq-main", 3, &SubsequenceSource::Range(range), &nest_ids("seq-sub"), &c).unwrap();
        assert_eq!(p.copied_clips(), [(cid(1), cid(50)), (cid(2), cid(51)), (cid(11), cid(52)), (cid(12), cid(53))]);
        let p = plan_make_subsequence(&snap, "seq-main", 3, &SubsequenceSource::Clips(vec![cid(12)]), &nest_ids("seq-sub"), &c).unwrap();
        // Clip 12's link group brings clip 2 (track order: V2 before A2).
        assert_eq!(p.copied_clips(), [(cid(2), cid(50)), (cid(12), cid(51))]);
        let n = plan_nest(&snap, "seq-main", 3, &[cid(1)], &nest_ids("seq-n"), &c).unwrap();
        assert!(n.copied_clips().is_empty());
    });
}

/// R44 finding F-R44-7 (lane SMB-R44-FOLLOWUPS-1): a valid, acyclic document in which each of 12
/// sequences nests the one below it on 4 tracks (4^12 = 16777216 leaf visits) is refused
/// `limit_exceeded` by the layer budget, charged per visited layer (expanded Sequence layers
/// included), not walked until the cancel token. The refusal is asserted, never a time. The same
/// shape one level deep (seq-1 nests seq-0 on 4 tracks: 4 Sequence layers + 16 media layers = 20
/// visits) resolves under a budget of 20 and is refused at 19, so expansions are charged, not only
/// media layers.
#[test]
fn nested_frame_fan_out_is_bounded_by_the_layer_budget() {
    bounded_case("fan-out", || {
        // seq-0: 4 media tracks. seq-k (k = 1..=12): 4 video tracks, each one clip nesting seq-(k-1).
        let (mut clip_n, mut track_n) = (100u32, 100u32);
        fn next(c: &mut u32) -> u32 {
            *c += 1;
            *c
        }
        let build = |depth: u32, clip_n: &mut u32, track_n: &mut u32| -> Vec<StudioSequence> {
            let mut out = Vec::new();
            let leaf: Vec<StudioTrack> = (0..4).map(|i| track(next(track_n), TrackKind::Video, i, vec![clip(next(clip_n), media("m"), 0, 10, 0)])).collect();
            out.push(sequence("seq-0", leaf));
            for k in 1..=depth {
                let below = format!("seq-{}", k - 1);
                let tracks: Vec<StudioTrack> = (0..4).map(|i| track(next(track_n), TrackKind::Video, i, vec![clip(next(clip_n), nested(&below), 0, 10, 0)])).collect();
                out.push(sequence(&format!("seq-{k}"), tracks));
            }
            out
        };
        let deep = document(build(12, &mut clip_n, &mut track_n));
        let top = deep.document().sequences.last().unwrap().clone();
        assert_eq!(top.sequence_id, "seq-12");
        let cancel = CancellationToken::default();
        let e = frame_layers_nested(&deep.document().sequences, &top, Ticks::new(2 * F), &Known, 256, &cancel).unwrap_err();
        assert_eq!(e.code(), "limit_exceeded");
        assert!(cancel.check().is_ok(), "refused by the budget, not by cancellation");
        // One level: 4 Sequence layers + 16 media layers = 20 visits.
        let shallow = document(build(1, &mut clip_n, &mut track_n));
        let top = shallow.document().sequences.last().unwrap().clone();
        let frame = frame_layers_nested(&shallow.document().sequences, &top, Ticks::new(2 * F), &Known, 20, &cancel).unwrap();
        assert_eq!((frame.layers.len(), frame.unresolved.len()), (16, 0));
        assert_eq!(frame_layers_nested(&shallow.document().sequences, &top, Ticks::new(2 * F), &Known, 19, &cancel).unwrap_err().code(), "limit_exceeded");
        // Unresolved layers are charged too: 4 tracks nesting an unknown sequence need 4 visits.
        let lost = sequence("seq-lost", (0..4).map(|i| track(next(&mut track_n), TrackKind::Video, i, vec![clip(next(&mut clip_n), nested("seq-none"), 0, 10, 0)])).collect());
        let lone = vec![lost.clone()];
        let frame = frame_layers_nested(&lone, &lost, Ticks::new(2 * F), &Known, 4, &cancel).unwrap();
        assert_eq!(frame.unresolved.len(), 4);
        assert_eq!(frame_layers_nested(&lone, &lost, Ticks::new(2 * F), &Known, 3, &cancel).unwrap_err().code(), "limit_exceeded");
    });
}
