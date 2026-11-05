//! Lane SMB-PR-RETIME-OUTPUT-1: `Maintain audio pitch` ([STU-VID-031] "an independent boolean") in the
//! timeline mix. Measurement is independent of Score and of pulse: a zero-crossing count, a DFT
//! peak scan and an RMS envelope written here, in f64, from the rendered samples; none calls a Score
//! function. Control: the same clip with the flag off moves its pitch with the speed, so the
//! measurement can tell the two apart. Adobe parity UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use hsk_studio_score::{ClipSpeed, Session, TrackSpec};
use std::sync::Arc;

const RATE: u32 = 48_000;
/// 25 fps ticks per frame; one 48 kHz sample is `TPS` ticks.
const F: u64 = 10_160_640_000;
const TPS: u64 = 5_292_000;
const TAU: f64 = std::f64::consts::TAU;

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
    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 90s deadline: {e}"),
    }
}

#[derive(Clone)]
struct Audio(Arc<Vec<Vec<f32>>>);
impl AudioSources for Audio {
    fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
        Some(&self.0)
    }
}
struct Known;
impl SourceExtents for Known {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(u64::MAX / 2))
    }
}
fn source() -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: "tone".into(),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
        asset_id: None,
    }
}
/// `window` source samples played at `speed` from the head of the source.
fn sequence(window: usize, speed: (i64, u64), pitch: bool) -> StudioSequence {
    let out = ClipSpeed::new(speed.0.unsigned_abs(), speed.1, speed.0 < 0)
        .map_or(window as u64, |s| s.output_frames(window as u64));
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: source(),
        source_in: Ticks::new(0),
        source_out: Ticks::new(window as u64 * TPS),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(out * TPS),
        speed: Speed { numerator: speed.0, denominator: speed.1 },
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: ClipRetime { maintain_audio_pitch: pitch, ..ClipRetime::default() },
    };
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-a".into(),
        name: "a".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(out * TPS),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Audio,
            index: 0,
            name: "A1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            volume: 1.0,
            pan: 0.0,
            clips: vec![clip],
        }],
        markers: Vec::new(),
        work_area: None,
    }
}
fn tone(hz: f64, amp: f64, frames: usize) -> Vec<f32> {
    (0..frames).map(|i| (amp * (TAU * hz * i as f64 / f64::from(RATE)).sin()) as f32).collect()
}
/// Renders the whole mix through the block stream.
fn mix(seq: &StudioSequence, channels: &Arc<Vec<Vec<f32>>>, block: usize) -> Result<(Vec<f32>, Vec<f32>)> {
    let cancel = CancellationToken::default();
    let mut s = TimelineMixStream::new(seq, RATE, block, Audio(channels.clone()), &Known, StreamLimits::default(), &cancel)?;
    let n = s.length_samples() as usize;
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut at = 0;
    while at < n {
        let k = block.min(n - at);
        s.render(&mut l[at..at + k], &mut r[at..at + k], &cancel)?;
        at += k;
    }
    Ok((l, r))
}

// ---- independent measurement ----

/// Frequency from the positive-going zero crossings of the middle of `x`, interpolated linearly.
fn zero_cross_hz(x: &[f32]) -> f64 {
    let (lo, hi) = (x.len() / 5, x.len() * 4 / 5);
    let (mut first, mut last, mut count) = (None, 0.0, 0u32);
    for i in lo..hi {
        let (a, b) = (f64::from(x[i]), f64::from(x[i + 1]));
        if a < 0.0 && b >= 0.0 {
            let t = i as f64 + a / (a - b);
            if first.is_none() {
                first = Some(t);
            }
            last = t;
            count += 1;
        }
    }
    let first = first.expect("no crossing");
    f64::from(count - 1) * f64::from(RATE) / (last - first)
}
/// Magnitude of the Hann-windowed DFT of `x` at `hz`.
fn dft(x: &[f32], hz: f64) -> f64 {
    let n = x.len() as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (TAU * i as f64 / n).cos();
        let p = TAU * hz * i as f64 / f64::from(RATE);
        re += w * f64::from(*v) * p.cos();
        im += w * f64::from(*v) * p.sin();
    }
    (re * re + im * im).sqrt() / (n / 4.0)
}
/// The middle `n` samples.
fn middle(x: &[f32], n: usize) -> &[f32] {
    let n = n.min(x.len());
    &x[(x.len() - n) / 2..][..n]
}
/// Strongest frequency on a 2 Hz scan of `lo..hi`, on the middle of `x`.
fn peak_hz(x: &[f32], lo: u32, hi: u32) -> f64 {
    let seg = middle(x, 8192);
    (lo..hi)
        .step_by(2)
        .map(|hz| (f64::from(hz), dft(seg, f64::from(hz))))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
        .0
}
/// RMS of `x` in hops of 256; the first and last hop above half of the largest RMS, in samples.
fn envelope_span(x: &[f32]) -> (usize, usize) {
    let rms: Vec<f64> = x
        .chunks(256)
        .map(|c| (c.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    let top = rms.iter().copied().fold(0.0, f64::max);
    let first = rms.iter().position(|v| *v > top / 2.0).unwrap();
    let last = rms.iter().rposition(|v| *v > top / 2.0).unwrap();
    (first * 256, (last + 1) * 256)
}

fn cents(hz: f64, want: f64) -> f64 {
    1200.0 * (hz / want).log2()
}

#[test]
fn the_flag_keeps_pitch_and_duration_at_every_speed() {
    bounded_case("pitch_table", || {
        // (speed, source window in samples): the window keeps the output a few seconds long.
        let rows: [((i64, u64), usize); 11] = [
            ((2, 1), 96_000),
            ((1, 2), 48_000),
            ((3, 2), 96_000),
            ((3, 4), 72_000),
            ((4, 1), 96_000),
            ((1, 4), 24_000),
            ((5, 1), 120_000),
            ((8, 1), 192_000),
            ((16, 1), 240_000),
            ((1, 8), 12_000),
            ((1, 16), 6_000),
        ];
        let mut wrong = Vec::new();
        for (speed, window) in rows {
            let audio = Arc::new(vec![tone(440.0, 0.5, window)]);
            let r = f64::from(speed.0 as i32) / f64::from(speed.1 as i32);
            let (on, _) = mix(&sequence(window, speed, true), &audio, 4096).unwrap();
            let (off, _) = mix(&sequence(window, speed, false), &audio, 4096).unwrap();
            // Duration: the exact sample count of the rational law, and the envelope fills it.
            let want_len = (window as u128 * u128::from(speed.1)).div_ceil(u128::from(speed.0.unsigned_abs())) as usize;
            let span = envelope_span(&on);
            let (hz_on, hz_off) = (zero_cross_hz(&on), zero_cross_hz(&off));
            let (peak_on, moved) = (peak_hz(&on, 200, 900), 440.0 * r);
            let seg = middle(&on, 8192);
            let leak = dft(seg, moved) / dft(seg, 440.0);
            println!(
                "PITCH {speed:?}: len {} want {want_len}; on {hz_on:.2} Hz ({:+.2} cents), dft peak {peak_on}; off {hz_off:.2} Hz, expected {moved:.2}; leak at moved pitch {leak:.5}; envelope {span:?}",
                on.len(),
                cents(hz_on, 440.0)
            );
            let fine = on.len() == want_len
                && cents(hz_on, 440.0).abs() < 5.0
                && (peak_on - 440.0).abs() <= 4.0
                && leak < 0.02
                && span.0 <= 2048
                && span.1 + 2048 >= want_len;
            if !fine {
                wrong.push(format!("{speed:?}: len {} of {want_len}, {hz_on:.2} Hz, peak {peak_on}, leak {leak:.5}, envelope {span:?}", on.len()));
            }
            // The control: the same measurement sees the pitch move with the flag off.
            assert!(cents(hz_off, moved).abs() < 5.0, "control {speed:?}: {hz_off} against {moved}");
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    });
}

#[test]
fn a_burst_inside_the_clip_lands_where_the_speed_puts_it() {
    bounded_case("burst", || {
        // A 0.5 s tone at 1.0 s in a 3 s window: at 2x it sits at 0.5 s for 0.25 s, at 1/2 at 2.0 s for 1 s.
        let mut x = vec![0.0f32; 144_000];
        let burst = tone(660.0, 0.5, 24_000);
        x[48_000..72_000].copy_from_slice(&burst);
        let audio = Arc::new(vec![x]);
        for (speed, start, end) in [((2i64, 1u64), 24_000usize, 36_000usize), ((1, 2), 96_000, 144_000)] {
            let (on, _) = mix(&sequence(144_000, speed, true), &audio, 2048).unwrap();
            let (a, b) = envelope_span(&on);
            println!("BURST {speed:?}: {a}..{b}, expected {start}..{end}");
            assert!(a.abs_diff(start) <= 2048 && b.abs_diff(end) <= 2048, "{speed:?}: {a}..{b} against {start}..{end}");
            let seg = &on[start + 2048..end - 2048];
            assert!(cents(zero_cross_hz(seg), 660.0).abs() < 5.0, "{speed:?}: {}", zero_cross_hz(seg));
        }
    });
}

#[test]
fn stereo_keeps_each_channel_and_both_lengths() {
    bounded_case("stereo", || {
        let audio = Arc::new(vec![tone(330.0, 0.4, 96_000), tone(660.0, 0.4, 96_000)]);
        let (l, r) = mix(&sequence(96_000, (3, 2), true), &audio, 1024).unwrap();
        assert_eq!((l.len(), r.len()), (64_000, 64_000));
        let (hl, hr) = (zero_cross_hz(&l), zero_cross_hz(&r));
        println!("STEREO left {hl:.2} right {hr:.2}");
        assert!(cents(hl, 330.0).abs() < 5.0 && cents(hr, 660.0).abs() < 5.0, "{hl} {hr}");
        // Level kept: a steady tone stays at its amplitude within 1 dB (independent RMS).
        let rms = (middle(&l, 24_000).iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / 24_000.0).sqrt();
        let db = 20.0 * (rms * 2f64.sqrt() / 0.4).log10();
        assert!(db.abs() < 1.0, "{db} dB");
    });
}

#[test]
fn reverse_with_the_flag_plays_backward_at_the_kept_pitch() {
    bounded_case("reverse", || {
        // Rising sweep 300 -> 600 Hz over 2 s. Forward playback rises; reversed falls, whatever the
        // speed, and at -2 with the flag it covers the same frequencies, not twice them.
        let n = 96_000usize;
        let mut phase = 0.0f64;
        let sweep: Vec<f32> = (0..n)
            .map(|i| {
                phase += TAU * (300.0 + 300.0 * i as f64 / n as f64) / f64::from(RATE);
                (0.5 * phase.sin()) as f32
            })
            .collect();
        let audio = Arc::new(vec![sweep]);
        let band = |x: &[f32]| -> (f64, f64) {
            let k = x.len() / 10;
            (zero_cross_hz(&x[k..3 * k]), zero_cross_hz(&x[7 * k..9 * k]))
        };
        let (fwd, _) = mix(&sequence(n, (2, 1), true), &audio, 4096).unwrap();
        let (rev, _) = mix(&sequence(n, (-2, 1), true), &audio, 4096).unwrap();
        let (rev_off, _) = mix(&sequence(n, (-2, 1), false), &audio, 4096).unwrap();
        let ((f_a, f_b), (r_a, r_b), (o_a, o_b)) = (band(&fwd), band(&rev), band(&rev_off));
        println!("REVERSE forward {f_a:.1}->{f_b:.1}, reversed {r_a:.1}->{r_b:.1}, reversed flag off {o_a:.1}->{o_b:.1}");
        assert_eq!((fwd.len(), rev.len()), (48_000, 48_000));
        assert!(f_a < f_b, "forward rises: {f_a} {f_b}");
        assert!(r_a > r_b, "reversed falls: {r_a} {r_b}");
        // Mirror: reversed start = forward end, within 3 %.
        assert!((r_a / f_b - 1.0).abs() < 0.03 && (r_b / f_a - 1.0).abs() < 0.03, "{r_a} {f_b} {r_b} {f_a}");
        // The control: flag off at -2 is an octave up.
        assert!((o_a / r_a - 2.0).abs() < 0.1, "{o_a} {r_a}");
    });
}

fn fnv(l: &[f32], r: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in l.iter().chain(r) {
        for b in v.to_bits().to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[test]
fn flag_off_is_byte_identical_to_the_resampler() {
    bounded_case("off_identity", || {
        let cancel = CancellationToken::default();
        let audio = Arc::new(vec![tone(440.0, 0.5, 96_000), tone(555.0, 0.25, 96_000)]);
        for speed in [(2i64, 1u64), (1, 2), (3, 2), (-1, 1), (-3, 4), (1, 1)] {
            let (l, r) = mix(&sequence(96_000, speed, false), &audio, 4096).unwrap();
            // The Score path alone, with no pulse in it.
            let (al, ar) = (l.len(), r.len());
            let spec = if speed == (1, 1) {
                TrackSpec::from_source(&audio, 0, 96_000, 0).unwrap()
            } else {
                let s = ClipSpeed::new(speed.0.unsigned_abs(), speed.1, speed.0 < 0).unwrap();
                TrackSpec::from_source_at_speed(&audio, 0, 96_000, s, 0).unwrap()
            };
            let mut session = Session::new(RATE, 4096, vec![spec]).unwrap();
            let (mut dl, mut dr) = (vec![0.0f32; al], vec![0.0f32; ar]);
            let mut at = 0;
            while at < al {
                let k = 4096.min(al - at);
                session.render(at as u64, &mut dl[at..at + k], &mut dr[at..at + k], &cancel).unwrap();
                at += k;
            }
            assert_eq!(l.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), dl.iter().map(|v| (v + 0.0).to_bits()).collect::<Vec<_>>(), "{speed:?}");
            assert_eq!(r.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), dr.iter().map(|v| (v + 0.0).to_bits()).collect::<Vec<_>>(), "{speed:?}");
            // For a reader comparing two runs; the proof is the equality above.
            println!("OFF_DIGEST {speed:?} {:016x}", fnv(&l, &r));
        }
    });
}

#[test]
fn the_flag_changes_nothing_when_the_speed_keeps_the_pitch_anyway() {
    bounded_case("unit_identity", || {
        let audio = Arc::new(vec![tone(440.0, 0.5, 48_000)]);
        for speed in [(1i64, 1u64), (-1, 1), (2, 2)] {
            let on = mix(&sequence(48_000, speed, true), &audio, 4096).unwrap();
            let off = mix(&sequence(48_000, speed, false), &audio, 4096).unwrap();
            assert_eq!(fnv(&on.0, &on.1), fnv(&off.0, &off.1), "{speed:?}");
        }
    });
}

#[test]
fn block_size_and_seek_do_not_change_the_kept_pitch_output() {
    bounded_case("blocks", || {
        let cancel = CancellationToken::default();
        let audio = Arc::new(vec![tone(440.0, 0.5, 48_000)]);
        let seq = sequence(48_000, (3, 2), true);
        let whole = mix(&seq, &audio, 8192).unwrap();
        for block in [64, 1000, 4096] {
            let got = mix(&seq, &audio, block).unwrap();
            assert_eq!(fnv(&got.0, &got.1), fnv(&whole.0, &whole.1), "block {block}");
        }
        // The single-session mix agrees.
        let mut session = build_timeline_mix(&seq, RATE, 8192, &Audio(audio.clone()), &Known, &cancel).unwrap();
        assert_eq!(session.length_samples() as usize, whole.0.len());
        let (mut l, mut r) = (vec![0.0f32; whole.0.len()], vec![0.0f32; whole.0.len()]);
        let mut at = 0;
        while at < l.len() {
            let k = 8192.min(l.len() - at);
            session.render(at as u64, &mut l[at..at + k], &mut r[at..at + k], &cancel).unwrap();
            at += k;
        }
        assert_eq!(fnv(&l, &r), fnv(&whole.0, &whole.1));
        // A seek into the clip gives the same samples from there.
        let mut s = TimelineMixStream::new(&seq, RATE, 1024, Audio(audio.clone()), &Known, StreamLimits::default(), &cancel).unwrap();
        s.seek(10_000);
        let (mut a, mut b) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
        s.render(&mut a, &mut b, &cancel).unwrap();
        assert_eq!(a, whole.0[10_000..11_024].to_vec());
    });
}

#[test]
fn hostile_input_answers_typed() {
    bounded_case("hostile", || {
        let cancel = CancellationToken::default();
        let good = Arc::new(vec![tone(440.0, 0.5, 48_000)]);
        // A speed Score refuses: the same refusal with the flag on as off.
        for speed in [(17i64, 1u64), (1, 17), (70_000, 1)] {
            let off = mix(&sequence(48_000, speed, false), &good, 4096).map(|_| ()).unwrap_err().code();
            let on = mix(&sequence(48_000, speed, true), &good, 4096).map(|_| ()).unwrap_err().code();
            assert_eq!(on, off, "{speed:?}");
        }
        // Non-finite source samples: refused, nothing rendered.
        let mut bad = tone(440.0, 0.5, 48_000);
        bad[1000] = f32::NAN;
        let bad = Arc::new(vec![bad]);
        assert!(mix(&sequence(48_000, (2, 1), true), &bad, 4096).is_err());
        // A window past the source: the refusal of the flag-off path.
        let seq = sequence(96_000, (2, 1), true);
        let off = mix(&sequence(96_000, (2, 1), false), &good, 4096).map(|_| ()).unwrap_err().code();
        assert_eq!(mix(&seq, &good, 4096).map(|_| ()).unwrap_err().code(), off);
        // Canceled before any frame is stretched.
        let stopped = CancellationToken::default();
        stopped.cancel();
        let seq = sequence(48_000, (2, 1), true);
        assert_eq!(build_timeline_mix(&seq, RATE, 4096, &Audio(good.clone()), &Known, &stopped).map(|_| ()).unwrap_err().code(), "canceled");
        // Resident bound: refused before the stretch allocates.
        let limits = StreamLimits { max_resident_frames: 1000, ..StreamLimits::default() };
        let e = TimelineMixStream::new(&seq, RATE, 4096, Audio(good.clone()), &Known, limits, &cancel).map(|_| ()).unwrap_err();
        assert_eq!(e.code(), "limit_exceeded");
        // A very short window still gives its exact length.
        for (window, speed, want) in [(1usize, (16i64, 1u64), 1usize), (3, (2, 1), 2), (5, (1, 16), 80), (1, (1, 16), 16)] {
            let audio = Arc::new(vec![tone(440.0, 0.5, window)]);
            assert_eq!(mix(&sequence(window, speed, true), &audio, 4096).unwrap().0.len(), want, "{window} {speed:?}");
        }
        let _ = cancel;
    });
}

#[test]
fn the_cost_of_the_stretch_at_session_build_is_reported() {
    bounded_case("load", || {
        // Reports the cost of the stretch at session build; asserts only that the output is whole.
        let audio = Arc::new(vec![tone(440.0, 0.5, 480_000), tone(330.0, 0.5, 480_000)]);
        let start = std::time::Instant::now();
        let (l, _) = mix(&sequence(480_000, (2, 1), true), &audio, 4096).unwrap();
        println!("LOAD 10 s stereo source at 2x, flag on: {:?} including render", start.elapsed());
        let start = std::time::Instant::now();
        let (o, _) = mix(&sequence(480_000, (2, 1), false), &audio, 4096).unwrap();
        println!("LOAD same, flag off: {:?}", start.elapsed());
        assert_eq!(l.len(), o.len());
    });
}
