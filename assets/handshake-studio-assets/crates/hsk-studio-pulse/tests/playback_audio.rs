//! Lane SMB-PULSE-PLAYBACK: the timeline audio path. The sequence's audio track (two clips, an
//! audio crossfade, track volume and pan) is mixed by score, and the score session is the master
//! clock of the playback scheduler. Stated tolerances: mixdown samples within 1e-6 of the hand
//! value (score sums in f64 and publishes f32); video presentations within `[0, 1 frame)` behind
//! the score position at every 480-sample block.
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
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

/// 25 fps; 48 kHz has 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;

fn source(name: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: name.into(),
        content_digest: ContentDigest {
            algorithm: "sha256".into(),
            digest: "abc123".into(),
        },
        asset_id: None,
    }
}
fn clip(n: u32, src: &str, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: source(src),
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
fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        kind,
        index: 0,
        name: format!("T{n}"),
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

/// Mono sources: "a" is 0.5, "b" is -0.25, each 100 frames long.
struct Sources(Vec<Vec<f32>>, Vec<Vec<f32>>);
impl AudioSources for Sources {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        match s {
            ClipSource::Media {
                artifact_manifest_id,
                ..
            } if artifact_manifest_id == "a" => Some(&self.0),
            ClipSource::Media {
                artifact_manifest_id,
                ..
            } if artifact_manifest_id == "b" => Some(&self.1),
            _ => None,
        }
    }
}
struct Extent;
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(100 * F))
    }
}

#[test]
fn timeline_audio_mixes_through_score_and_clocks_video_within_one_frame() {
    bounded_case("playback_audio", || {
        let cancel = CancellationToken::default();
        let sources = Sources(vec![vec![0.5; 100 * SPF]], vec![vec![-0.25; 100 * SPF]]);
        // V1 for the frame clock; A1: X = a[10..30) @0, Y = b[40..60) @20, a 4-frame linear
        // crossfade centred on the cut at frame 20 (samples 34560..42240), volume 0.5, pan 0.
        let mut a1 = track(
            2,
            TrackKind::Audio,
            vec![clip(2, "a", 10, 20, 0), clip(3, "b", 40, 20, 20)],
        );
        a1.volume = 0.5;
        a1.clips[0].transitions.tail = Some(TimelineTransition {
            schema_id: TransitionSchema::V1,
            transition_id: "xf".into(),
            transition_kind: AUDIO_CROSSFADE.into(),
            duration: Ticks::new(4 * F),
            alignment: TransitionAlignment::CenteredOnCut,
            center_offset: 0,
            parameters: vec![TransitionParameter {
                param_key: CROSSFADE_CURVE.into(),
                param_value: TransitionParameterValue::Enumeration("linear".into()),
            }],
            repeat_end_frame: false,
        });
        let mut seq = StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-main".into(),
            name: "main".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(40 * F),
            tracks: vec![track(1, TrackKind::Video, vec![clip(1, "v", 0, 40, 0)]), a1],
            markers: Vec::new(),
            work_area: None,
        };
        let mut mix = build_timeline_mix(&seq, 48_000, 480, &sources, &Extent, &cancel).unwrap();

        // Play: render 480-sample blocks; after each block the scheduler polls the score clock.
        let mut s = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
        s.play(&mix).unwrap();
        let (mut left, mut right) = (vec![0.0f32; 40 * SPF], vec![0.0f32; 40 * SPF]);
        let mut presented = Vec::new();
        for block in 0..(40 * SPF / 480) {
            let at = block * 480;
            let start = u64::try_from(at).unwrap();
            mix.render(
                start,
                &mut left[at..at + 480],
                &mut right[at..at + 480],
                &cancel,
            )
            .unwrap();
            if let FrameAction::Present { frame, tick } = s.poll(&mix).unwrap().action {
                let master = Ticks::new(mix.position() * 5_292_000);
                assert!(
                    master >= tick && master.value() - tick.value() < F,
                    "frame {frame}"
                );
                presented.push(frame);
            }
        }
        assert_eq!(s.poll(&mix).unwrap().action, FrameAction::End);
        assert_eq!(
            presented,
            (0..40).collect::<Vec<u64>>(),
            "every frame, none dropped"
        );
        assert_eq!(s.stats().leads, 0);

        // Mixdown values: fader 0.5 (score dB lane), centre pan cos(pi/4); linear crossfade.
        let g = 0.5f64.log10() * 20.0;
        let gain = 10f64.powf(g / 20.0) * std::f64::consts::FRAC_PI_4.cos();
        let close = |i: usize, want: f64| {
            assert!(
                (f64::from(left[i]) - want).abs() < 1e-6,
                "sample {i}: {} vs {want}",
                left[i]
            );
            assert!((f64::from(right[i]) - want).abs() < 1e-6);
        };
        close(1000, 0.5 * gain);
        close(38_400, (0.5 * 0.5 - 0.25 * 0.5) * gain); // p = 3840 / 7680 at the cut
        close(36_480, (0.5 * 0.75 - 0.25 * 0.25) * gain); // p = 1920 / 7680
        close(45_000, -0.25 * gain);
        close(76_799, -0.25 * gain);

        // Mute and solo: a muted track is silent; soloing the (empty) video track silences nothing
        // else because only audio tracks take part in solo.
        seq.tracks[1].muted = true;
        let mut silent = build_timeline_mix(&seq, 48_000, 480, &sources, &Extent, &cancel).unwrap();
        let (mut l, mut r) = (vec![1.0f32; 480], vec![1.0f32; 480]);
        silent.render(0, &mut l, &mut r, &cancel).unwrap();
        assert!(l.iter().chain(&r).all(|v| *v == 0.0));
        seq.tracks[1].muted = false;

        // Hostile: a clip off the sample grid, a missing source, more clips than score mixes.
        let mut ragged = seq.clone();
        ragged.tracks[1].clips[1].source_in = Ticks::new(40 * F + 1);
        ragged.tracks[1].clips[1].source_out = Ticks::new(60 * F + 1);
        assert_eq!(
            build_timeline_mix(&ragged, 48_000, 480, &sources, &Extent, &cancel).err(),
            Some(PulseError::NotFrameAligned)
        );
        let mut orphan = seq.clone();
        orphan.tracks[1].clips[1].source_ref = source("missing");
        assert_eq!(
            build_timeline_mix(&orphan, 48_000, 480, &sources, &Extent, &cancel).err(),
            Some(PulseError::SourceExtentUnknown)
        );
        let mut crowd = seq.clone();
        crowd.tracks[1].clips = (0..65)
            .map(|i| clip(100 + i, "a", 0, 1, u64::from(i)))
            .collect();
        assert_eq!(
            build_timeline_mix(&crowd, 48_000, 480, &sources, &Extent, &cancel).err(),
            Some(PulseError::LimitExceeded)
        );
        let mut past = seq.clone();
        past.tracks[1].clips[1].source_in = Ticks::new(90 * F);
        past.tracks[1].clips[1].source_out = Ticks::new(110 * F);
        past.tracks[1].clips[0].transitions.tail = None;
        assert_eq!(
            build_timeline_mix(&past, 48_000, 480, &sources, &Extent, &cancel).err(),
            Some(PulseError::PastSourceEnd)
        );
    });
}
