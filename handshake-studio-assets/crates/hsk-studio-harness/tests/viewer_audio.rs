//! Lane SMB-PULSE-PLAYBACK-2 (feature `audio-device`): the program monitor's audio out through
//! score-device. The device is score-device's software clocked device with its loopback
//! recorder, opened exactly as score-device's own tests open it (explicit grant, no OS audio).
//! Stated bounds: the device receives the timeline mix bit for bit, starting at the play
//! position (the clock waits for the queued silence); every presented video frame is within
//! [0, 1 frame) behind the device-played position and never ahead; 2x shuttle silences the
//! audio; looping wraps the audio at the out point exactly as the video.
#![cfg(feature = "audio-device")]

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use hsk_studio_folio::*;
use hsk_studio_harness::view::audio_out::AudioOut;
use hsk_studio_harness::view::monitor::{DropPolicy, FrameSource, ProgramMonitor, SourceFrame};
use hsk_studio_pulse::{AudioSources, SourceExtents, StreamLimits, TimelineMixStream};
use hsk_studio_score_device::{
    DeviceGrant, DeviceId, DeviceProvider, Direction, Limits, SOFTWARE_OUTPUT_DEVICE, SoftwareOptions,
    SoftwareProvider, StreamConfig,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 25 fps; 48 kHz = 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const SCALE: f32 = 1.0 / 1_048_576.0;

fn media(id: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: id.into(),
        content_digest: ContentDigest {
            algorithm: "sha256".into(),
            digest: "00".into(),
        },
    }
}
fn clip(n: u32, src: &str, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: media(src),
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(0),
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
/// 50 frames: V1 one clip, A1 one stereo ramp clip (left (i+1)/2^20, right the negative).
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(50 * F),
        tracks: vec![
            track(1, TrackKind::Video, vec![clip(1, "v", 50)]),
            track(2, TrackKind::Audio, vec![clip(2, "ramp", 50)]),
        ],
        markers: Vec::new(),
        work_area: None,
    }
}
struct Ramp(Vec<Vec<f32>>);
impl AudioSources for Ramp {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        matches!(s, ClipSource::Media { artifact_manifest_id: id, .. } if id == "ramp").then_some(self.0.as_slice())
    }
}
fn ramp() -> Ramp {
    let n = 50 * SPF;
    Ramp(vec![
        (0..n).map(|i| (i as f32 + 1.0) * SCALE).collect(),
        (0..n).map(|i| -(i as f32 + 1.0) * SCALE).collect(),
    ])
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(50 * F))
    }
}
struct Solid(Arc<SourceFrame>);
impl FrameSource for Solid {
    fn frame(&self, _: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
        Some(self.0.clone())
    }
}
fn solid() -> Solid {
    let colour: Vec<u8> = [0.5f32, 0.25, 0.125].iter().cycle().take(12).flat_map(|v| v.to_le_bytes()).collect();
    let alpha: Vec<u8> = std::iter::repeat_n(1.0f32.to_le_bytes(), 4).flatten().collect();
    Solid(Arc::new(SourceFrame {
        width: 2,
        height: 2,
        colour_sha256: hsk_studio_prism::profile_hash(&colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
        colour,
        alpha,
    }))
}
fn grant() -> DeviceGrant {
    DeviceGrant::new(
        ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap(),
        DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001").unwrap(),
        &[Direction::Output],
        None,
    )
}
fn open(provider: &SoftwareProvider, mix: &TimelineMixStream<Ramp>) -> hsk_studio_score_device::OutputStream {
    let config = StreamConfig {
        sample_rate_hz: 48_000,
        layout: mix.layout(),
        block_frames: 480,
        ring_blocks: 4,
    };
    provider
        .open_output(&DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).unwrap(), &config, Some(&grant()), &CancellationToken::default())
        .unwrap()
}

/// Host loop for `for_ms`: pump the ring, refresh the monitor, check the sync bound.
fn run(out: &mut AudioOut<Ramp>, m: &mut ProgramMonitor, for_ms: u64) -> u64 {
    let cancel = CancellationToken::default();
    let deadline = Instant::now() + Duration::from_millis(for_ms);
    let mut presented = 0;
    let mut checked = 0u64;
    while Instant::now() < deadline {
        out.pump(&cancel).unwrap();
        if let Some(frame) = m.refresh(&*out, &solid(), &Ext, &cancel).unwrap() {
            // The sequence sample the device plays: never before the presented frame (video not
            // ahead of audio) and less than one frame plus one device block (480 samples, the
            // callback granularity racing this check) after its start.
            if let Some(played) = out.played_sequence_sample().filter(|_| !m.scheduler().looping()) {
                let start = frame * SPF as u64;
                assert!(played >= start, "frame {frame} ahead of audio sample {played}");
                assert!(played < start + SPF as u64 + 480, "frame {frame} late: audio at sample {played}");
                checked += 1;
            }
            presented += 1;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("presented {presented}, sync-checked {checked}");
    presented
}

#[test]
fn audio_out_plays_the_timeline_mix_through_the_device_in_sync() {
    let cancel = CancellationToken::default();
    let provider = SoftwareProvider::with_options(Limits::default(), SoftwareOptions { loopback_frames: 1 << 18 });
    let mix = TimelineMixStream::new(&sequence(), 48_000, 480, ramp(), &Ext, StreamLimits::default(), &cancel).unwrap();
    let stream = open(&provider, &mix);
    let mut out = AudioOut::new(mix, stream).unwrap();
    let mut m = ProgramMonitor::new(sequence(), 2, 2, DropPolicy::DropLate).unwrap();
    // Idle: silence keeps the device running.
    run(&mut out, &mut m, 60);
    // Play from frame 10 at 1x for ~400 ms.
    out.command(&mut m, |m, c| m.scrub(Ticks::new(10 * F), c)).unwrap().unwrap();
    out.command(&mut m, |m, c| m.play(c)).unwrap().unwrap();
    assert!(out.audible());
    let presented = run(&mut out, &mut m, 400);
    let st = m.scheduler().stats();
    assert!(presented >= 5, "{presented} {}", m.state_json());
    assert_eq!(st.leads, 0, "video never ahead of audio");
    assert!(st.max_lag_ticks < F, "lag {} >= 1 frame", st.max_lag_ticks);
    // L twice: 2x, the audio goes silent.
    out.command(&mut m, |m, c| m.shuttle(false, c)).unwrap().unwrap();
    out.command(&mut m, |m, c| m.shuttle(false, c)).unwrap().unwrap();
    assert!(!out.audible());
    run(&mut out, &mut m, 120);
    let mixed_blocks = out.stats().mix_blocks;
    out.command(&mut m, |m, c| m.shuttle_stop(c)).unwrap().unwrap();
    run(&mut out, &mut m, 60);
    assert_eq!(out.stats().mix_blocks, mixed_blocks, "no audio while shuttling or paused");
    println!("{}", out.state_json());
    let report = out.close();
    assert!(report.reconciles() && !report.device_lost, "{report:?}");
    // The device received exactly the ramp from frame 10's first sample, contiguous, in both
    // channels, then silence; everything else is silence.
    let left: Vec<f32> = report.loopback_samples.chunks_exact(2).map(|f| f[0]).collect();
    let right: Vec<f32> = report.loopback_samples.chunks_exact(2).map(|f| f[1]).collect();
    let first = left.iter().position(|s| *s != 0.0).expect("audio reached the device");
    let len = left[first..].iter().take_while(|s| **s != 0.0).count();
    assert!(len >= 480 * 10, "only {len} samples played");
    for i in 0..len {
        let want = ((10 * SPF + i) as f32 + 1.0) * SCALE;
        assert_eq!(left[first + i].to_bits(), want.to_bits(), "sample {i}");
        assert_eq!(right[first + i].to_bits(), (-want).to_bits(), "sample {i}");
    }
    assert!(left[first + len..].iter().all(|s| *s == 0.0), "silence after the shuttle");
}

#[test]
fn looping_audio_wraps_at_the_out_point_and_config_is_checked() {
    let cancel = CancellationToken::default();
    let provider = SoftwareProvider::with_options(Limits::default(), SoftwareOptions { loopback_frames: 1 << 18 });
    let mix = TimelineMixStream::new(&sequence(), 48_000, 480, ramp(), &Ext, StreamLimits::default(), &cancel).unwrap();
    let stream = open(&provider, &mix);
    let mut out = AudioOut::new(mix, stream).unwrap();
    let mut m = ProgramMonitor::new(sequence(), 2, 2, DropPolicy::DropLate).unwrap();
    // In 2, out 3 (inclusive): an 80 ms span, looped for ~400 ms.
    m.set_in_out(2, 3).unwrap();
    out.command(&mut m, |m, c| m.set_looping(true, c)).unwrap().unwrap();
    out.command(&mut m, |m, c| m.play_in_to_out(c)).unwrap().unwrap();
    run(&mut out, &mut m, 400);
    assert!(m.scheduler().stats().wraps >= 2, "{}", m.state_json());
    assert!(out.stats().loop_wraps >= 2);
    out.command(&mut m, |m, c| m.pause(c)).unwrap().unwrap();
    run(&mut out, &mut m, 40);
    let report = out.close();
    let left: Vec<f32> = report.loopback_samples.chunks_exact(2).map(|f| f[0]).collect();
    let first = left.iter().position(|s| *s != 0.0).unwrap();
    // Played audio is the span 2..4 frames repeated: sample k of the span is ramp[2*SPF + k mod 2*SPF].
    let played: Vec<f32> = left[first..].iter().copied().take_while(|s| *s != 0.0).collect();
    assert!(played.len() > 4 * SPF, "{}", played.len());
    for (k, s) in played.iter().enumerate() {
        let want = ((2 * SPF + k % (2 * SPF)) as f32 + 1.0) * SCALE;
        assert_eq!(s.to_bits(), want.to_bits(), "looped sample {k}");
    }

    // A device stream that does not match the mix (rate, block) is refused before any audio.
    let provider = SoftwareProvider::new(Limits::default());
    let mix = TimelineMixStream::new(&sequence(), 48_000, 480, ramp(), &Ext, StreamLimits::default(), &cancel).unwrap();
    for (rate, block) in [(96_000, 480), (48_000, 960)] {
        let other = provider
            .open_output(
                &DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).unwrap(),
                &StreamConfig { sample_rate_hz: rate, layout: mix.layout(), block_frames: block, ring_blocks: 4 },
                Some(&grant()),
                &cancel,
            )
            .unwrap();
        let mix = TimelineMixStream::new(&sequence(), 48_000, 480, ramp(), &Ext, StreamLimits::default(), &cancel).unwrap();
        let refused = AudioOut::new(mix, other).err().unwrap();
        assert_eq!(refused.code, "audio_config", "{}", refused.detail);
    }
}
