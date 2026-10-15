//! Lane SMB-PULSE-PLAYBACK: the playback scheduler against a deterministic headless audio clock.
//! Stated tolerance: with `DropLate`, at every presentation `0 <= master - frame_start <
//! ticks_per_frame` (video never leads the audio clock and lags it by less than one frame, 40 ms
//! at 25 fps); audio sample time converts to ticks exactly (48 kHz = 5292000 ticks per sample).
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

/// 25 fps.
const F: u64 = 10_160_640_000;
/// 48 kHz: samples per 25 fps frame.
const SPF: u64 = 1920;

fn clip(n: u32, start: u64, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: ClipSource::Media {
            artifact_manifest_id: "manifest-a".into(),
            content_digest: ContentDigest {
                algorithm: "sha256".into(),
                digest: "abc123".into(),
            },
        },
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
/// 100 frames of video.
fn sequence(work_area: Option<(u64, u64)>) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(100 * F),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            volume: 1.0,
            pan: 0.0,
            clips: vec![clip(1, 0, 60), clip(2, 60, 40)],
        }],
        markers: Vec::new(),
        work_area: work_area.map(|(a, b)| WorkArea {
            start: Ticks::new(a * F),
            end: Ticks::new(b * F),
        }),
    }
}

/// Deterministic irregular presenter cadence (LCG), 1..=4000 samples per refresh.
fn cadence(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*state >> 33) % 4000 + 1
}

#[test]
fn playback_stays_within_one_frame_of_the_audio_clock() {
    bounded_case("playback", || {
        let seq = sequence(None);
        let mut clock = HeadlessClock::new(48_000);
        clock.advance(777); // the stream was already running: anchoring is relative

        // DropLate: irregular refreshes; every presentation within [0, 1 frame) behind audio.
        let mut s = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
        s.play(&clock).unwrap();
        let (mut rng, mut frames, mut last) = (7u64, 0u64, None::<u64>);
        loop {
            let d = s.poll(&clock).unwrap();
            match d.action {
                FrameAction::Present { frame, tick } => {
                    let lag = d.master.value() - tick.value();
                    assert!(d.master >= tick && lag < F, "frame {frame}: lag {lag}");
                    assert!(last.is_none_or(|l| frame > l));
                    frames += 1 + d.dropped;
                    last = Some(frame);
                }
                FrameAction::Hold => {}
                FrameAction::End => {
                    frames += d.dropped;
                    break;
                }
            }
            clock.advance(cadence(&mut rng));
        }
        let st = s.stats();
        assert_eq!(frames, 100, "every frame presented or counted dropped");
        assert_eq!((st.presented + st.dropped, st.leads), (100, 0));
        assert!(st.dropped > 0 && st.max_lag_ticks < F);
        assert_eq!(s.transport(), Transport::Ended);

        // PresentEveryFrame with a presenter at half real time: nothing dropped, lag reported.
        let mut slow = PlaybackScheduler::new(&seq, DropPolicy::PresentEveryFrame).unwrap();
        let mut clock = HeadlessClock::new(48_000);
        slow.play(&clock).unwrap();
        let mut presented = 0;
        while slow.poll(&clock).unwrap().action != FrameAction::End {
            presented += 1;
            clock.advance(2 * SPF);
        }
        assert_eq!((presented, slow.stats().dropped), (50, 0));
        assert_eq!(
            slow.stats().max_lag_ticks,
            49 * F,
            "frame 49 shown at audio frame 98"
        );

        // Transport: pause holds, scrub/step are frame-exact and clamped, play resumes there.
        let mut t = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
        let mut clock = HeadlessClock::new(48_000);
        t.play(&clock).unwrap();
        clock.advance(10 * SPF + 5);
        assert_eq!(
            t.poll(&clock).unwrap().action,
            FrameAction::Present {
                frame: 10,
                tick: Ticks::new(10 * F)
            }
        );
        t.pause(&clock).unwrap();
        clock.advance(50 * SPF);
        assert_eq!(t.poll(&clock).unwrap().action, FrameAction::Hold);
        assert_eq!(t.playhead(), Ticks::new(10 * F));
        t.scrub(Ticks::new(40 * F + 123), &clock).unwrap();
        assert_eq!(
            t.poll(&clock).unwrap().action,
            FrameAction::Present {
                frame: 40,
                tick: Ticks::new(40 * F)
            }
        );
        assert_eq!(t.step(1, &clock).unwrap(), Ticks::new(41 * F));
        assert_eq!(t.step(-5, &clock).unwrap(), Ticks::new(36 * F));
        assert_eq!(t.step(1000, &clock).unwrap(), Ticks::new(99 * F));
        assert_eq!(t.step(-1000, &clock).unwrap(), Ticks::new(0));
        t.scrub(Ticks::new(u64::MAX), &clock).unwrap();
        assert_eq!(t.playhead(), Ticks::new(99 * F));
        t.scrub(Ticks::new(20 * F), &clock).unwrap();
        t.play(&clock).unwrap();
        clock.advance(3 * SPF);
        assert_eq!(
            t.poll(&clock).unwrap().action,
            FrameAction::Present {
                frame: 23,
                tick: Ticks::new(23 * F)
            }
        );

        // The work area scopes the play range: starts at 10, ends at 20.
        let wa = sequence(Some((10, 20)));
        let mut w = PlaybackScheduler::new(&wa, DropPolicy::DropLate).unwrap();
        let mut clock = HeadlessClock::new(48_000);
        w.play(&clock).unwrap();
        assert_eq!(
            w.poll(&clock).unwrap().action,
            FrameAction::Present {
                frame: 10,
                tick: Ticks::new(10 * F)
            }
        );
        clock.advance(10 * SPF);
        assert_eq!(w.poll(&clock).unwrap().action, FrameAction::End);

        // Hostile clocks: a rate that does not divide the tick rate, and a clock that ran backwards.
        let odd = HeadlessClock::new(44_101);
        assert_eq!(
            PlaybackScheduler::new(&seq, DropPolicy::DropLate)
                .unwrap()
                .play(&odd)
                .unwrap_err(),
            PulseError::UnsupportedSampleRate
        );
        let mut back = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
        let mut ahead = HeadlessClock::new(48_000);
        ahead.advance(100);
        back.play(&ahead).unwrap();
        assert_eq!(
            back.poll(&HeadlessClock::new(48_000)).unwrap_err(),
            PulseError::Underflow
        );

        // What plays: one layer per enabled video track.
        let layers = frame_layers(
            &seq,
            Ticks::new(70 * F),
            &UnknownExtents,
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].source_tick, Ticks::new(10 * F));
    });
}
