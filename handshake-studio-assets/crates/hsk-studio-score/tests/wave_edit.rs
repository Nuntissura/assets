//! SMB-AU-WAVEFORM-EDITOR slice 1: sample-accurate waveform editor operations (score wave_edit).
//! Oracles: hand-written sample arrays, the fade formulas of score's FadeCurve, dB arithmetic by
//! hand, and "a refused edit changes nothing" (bit-equal buffer).
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

fn tok() -> CancellationToken {
    CancellationToken::default()
}

fn buf(ch: Vec<Vec<f32>>) -> WaveBuffer {
    WaveBuffer::new(RATE, ch).unwrap()
}

#[test]
fn cut_copy_paste_delete_silence_insert_are_sample_accurate() {
    bounded_case("wave_edit_splice", || {
        let a: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..10).map(|i| -(i as f32)).collect();
        let mut w = buf(vec![a, b]);
        let (clip, r) = w.cut(2, 5, &tok()).unwrap();
        assert_eq!(clip.channels, vec![vec![2.0, 3.0, 4.0], vec![-2.0, -3.0, -4.0]]);
        assert_eq!((r.frames_before, r.frames_after, r.changed_start), (10, 7, 2));
        assert_eq!(w.channels()[0], vec![0.0, 1.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        w.paste(1, &clip, PasteMode::Insert, ALL_CHANNELS, &tok()).unwrap();
        assert_eq!(w.channels()[0], vec![0.0, 2.0, 3.0, 4.0, 1.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        assert_eq!(w.channels()[1], vec![0.0, -2.0, -3.0, -4.0, -1.0, -5.0, -6.0, -7.0, -8.0, -9.0]);
        // Overwrite and mix touch only the masked channel (channel 1 = mask 0b10).
        let one = WaveClip { channels: vec![vec![10.0, 20.0]] };
        w.paste(8, &one, PasteMode::Overwrite, 0b10, &tok()).unwrap();
        w.paste(0, &one, PasteMode::Mix, 0b10, &tok()).unwrap();
        assert_eq!(w.channels()[1], vec![10.0, 18.0, -3.0, -4.0, -1.0, -5.0, -6.0, -7.0, 10.0, 20.0]);
        assert_eq!(w.channels()[0][8..], [8.0, 9.0]);
        // Copy of channel 0 only.
        assert_eq!(w.copy(WaveSelection { start: 3, end: 5, channels: 0b01 }).unwrap().channels, vec![vec![4.0, 1.0]]);
        w.silence(WaveSelection { start: 4, end: 6, channels: 0b01 }, &tok()).unwrap();
        assert_eq!(w.channels()[0], vec![0.0, 2.0, 3.0, 4.0, 0.0, 0.0, 6.0, 7.0, 8.0, 9.0]);
        assert_eq!(w.channels()[1][4..6], [-1.0, -5.0]);
        w.insert_silence(10, 2, &tok()).unwrap();
        w.insert_silence(0, 1, &tok()).unwrap();
        assert_eq!(w.frames(), 13);
        assert_eq!((w.channels()[0][0], w.channels()[0][11], w.channels()[0][12], w.channels()[1][1]), (0.0, 0.0, 0.0, 10.0));
        w.delete(0, 1, &tok()).unwrap();
        w.delete(10, 12, &tok()).unwrap();
        w.reverse(WaveSelection { start: 0, end: 4, channels: 0b01 }, &tok()).unwrap();
        w.invert(WaveSelection { start: 9, end: 10, channels: ALL_CHANNELS }, &tok()).unwrap();
        assert_eq!(w.channels()[0], vec![4.0, 3.0, 2.0, 0.0, 0.0, 0.0, 6.0, 7.0, 8.0, -9.0]);
        assert_eq!(w.channels()[1][9], -20.0);
    });
}

#[test]
fn zero_crossings_snap_to_hand_found_boundaries() {
    bounded_case("wave_edit_zero_crossing", || {
        // Channel 0 crosses at boundaries 2 (0.25 | -0.25) and 4 (-0.5 | 0.1); channel 1 only at 4.
        let c0 = vec![0.5, 0.25, -0.25, -0.5, 0.1, 0.2, 0.3];
        let c1 = vec![0.5, 0.4, 0.3, 0.2, -0.1, -0.2, -0.3];
        let w = buf(vec![c0, c1]);
        let t = tok();
        let snap = |p, m, d, max| w.snap_to_zero_crossing(p, m, d, max, &t).unwrap();
        assert_eq!(snap(3, 0b01, SnapDirection::Nearest, 8), Some(2)); // tie 2 vs 4: earlier
        assert_eq!(snap(3, 0b01, SnapDirection::Later, 8), Some(4));
        assert_eq!(snap(3, 0b01, SnapDirection::Nearest, 0), None);
        assert_eq!(snap(3, ALL_CHANNELS, SnapDirection::Nearest, 8), Some(4)); // both channels
        assert_eq!(snap(1, 0b01, SnapDirection::Earlier, 8), Some(0)); // the file start counts
        assert_eq!(snap(6, ALL_CHANNELS, SnapDirection::Later, 8), Some(7)); // the file end counts
        assert!(w.is_zero_crossing(4, ALL_CHANNELS).unwrap() && !w.is_zero_crossing(2, ALL_CHANNELS).unwrap());
        let sel = WaveSelection { start: 1, end: 5, channels: 0b01 };
        assert_eq!(w.snap_selection(sel, true, 8, &t).unwrap(), Some(WaveSelection { start: 2, end: 4, channels: 0b01 }));
        assert_eq!(w.snap_selection(sel, false, 8, &t).unwrap(), Some(WaveSelection { start: 0, end: 7, channels: 0b01 }));
        assert_eq!(w.snap_selection(WaveSelection { start: 2, end: 3, channels: ALL_CHANNELS }, true, 8, &t).unwrap(), None);
    });
}

#[test]
fn fades_envelope_amplify_and_normalise_follow_hand_values() {
    bounded_case("wave_edit_gain", || {
        let t = tok();
        let mut w = buf(vec![vec![1.0; 4]]);
        w.fade(WaveSelection::all(0, 4), FadeDirection::In, FadeCurve::Linear, &t).unwrap();
        assert_eq!(w.channels()[0], vec![0.0, 0.25, 0.5, 0.75]);
        let mut w = buf(vec![vec![1.0; 4]]);
        w.fade(WaveSelection::all(0, 4), FadeDirection::Out, FadeCurve::Linear, &t).unwrap();
        assert_eq!(w.channels()[0], vec![0.75, 0.5, 0.25, 0.0]);
        // S-curve fade in: (1 - cos(pi k / n)) / 2; the fade out is its mirror (ends at exactly 0).
        let mut w = buf(vec![vec![1.0; 8]]);
        w.fade(WaveSelection::all(0, 8), FadeDirection::Out, FadeCurve::SCurve, &t).unwrap();
        for k in 0..8 {
            let expect = (1.0 - ((7 - k) as f64 / 8.0 * std::f64::consts::PI).cos()) / 2.0;
            assert_eq!(w.channels()[0][k], expect as f32, "k {k}");
        }
        // Envelope 0 dB at 0 -> -6.0206 dB at 2, held after: 1, 0.70711, 0.5, 0.5 on a constant 1.
        let half_db = 20.0 * 0.5f64.log10();
        let mut w = buf(vec![vec![1.0; 4]]);
        w.gain_envelope(WaveSelection::all(0, 4), &[(0, 0.0), (2, half_db)], &t).unwrap();
        let c = &w.channels()[0];
        assert_eq!((c[0], c[2], c[3]), (1.0, 0.5, 0.5));
        assert!((f64::from(c[1]) - 0.5f64.sqrt()).abs() < 1e-7, "{}", c[1]);
        // Amplify +6.0206 dB doubles; normalise linked vs per channel; a silent channel stays.
        let mut w = buf(vec![vec![0.25, -0.5], vec![0.125, 0.25], vec![0.0, 0.0]]);
        let r = w.amplify(WaveSelection { start: 0, end: 2, channels: 0b001 }, -half_db, &t).unwrap();
        assert_eq!(w.channels()[0], vec![0.5, -1.0]);
        assert_eq!(r.gains_db, vec![Some(-half_db)]);
        let r = w.normalize(WaveSelection::all(0, 2), half_db, true, &t).unwrap();
        assert!(r.gains_db.iter().all(|g| g.is_some_and(|g| (g - half_db).abs() < 1e-12)), "{:?}", r.gains_db);
        assert_eq!((w.channels()[0][1], w.channels()[1][1]), (-0.5, 0.125));
        let r = w.normalize(WaveSelection::all(0, 2), half_db, false, &t).unwrap();
        assert_eq!(r.gains_db[2], None);
        assert_eq!((w.channels()[0][1], w.channels()[1][1], w.channels()[2][1]), (-0.5, 0.5, 0.0));
    });
}

#[test]
fn hostile_arguments_are_refused_and_change_nothing() {
    bounded_case("wave_edit_hostile", || {
        assert_eq!(WaveBuffer::new(44_101, vec![vec![0.0]]).unwrap_err(), ScoreError::InvalidRate);
        assert_eq!(WaveBuffer::new(RATE, vec![]).unwrap_err(), ScoreError::ChannelMismatch);
        assert_eq!(WaveBuffer::new(RATE, vec![vec![0.0]; 17]).unwrap_err(), ScoreError::ChannelMismatch);
        assert_eq!(WaveBuffer::new(RATE, vec![vec![0.0], vec![]]).unwrap_err(), ScoreError::LengthMismatch);
        assert_eq!(WaveBuffer::new(RATE, vec![vec![f32::NAN]]).unwrap_err(), ScoreError::NotFinite);
        let t = tok();
        let original = buf(vec![vec![0.5, f32::MAX, -0.25, 0.0], vec![0.1, 0.2, 0.3, 0.4]]);
        let mut w = original.clone();
        let all = WaveSelection::all(0, 4);
        let errs = [
            w.delete(2, 2, &t).unwrap_err(),
            w.delete(3, 5, &t).unwrap_err(),
            w.delete(u64::MAX - 1, u64::MAX, &t).unwrap_err(),
            w.silence(WaveSelection { start: 0, end: 1, channels: 0b100 }, &t).unwrap_err(),
            w.silence(WaveSelection { start: 0, end: 1, channels: 0 }, &t).unwrap_err(),
            w.amplify(all, f64::NAN, &t).unwrap_err(),
            w.amplify(all, 48.5, &t).unwrap_err(),
            w.amplify(all, 6.0, &t).unwrap_err(), // f32::MAX * 2 is not finite
            w.normalize(all, 0.5, true, &t).unwrap_err(),
            w.gain_envelope(all, &[], &t).unwrap_err(),
            w.gain_envelope(all, &[(2, 0.0), (2, -1.0)], &t).unwrap_err(),
            w.gain_envelope(all, &[(5, 0.0)], &t).unwrap_err(),
            w.gain_envelope(all, &vec![(0, 0.0); MAX_WAVE_ENVELOPE_POINTS + 1], &t).unwrap_err(),
            w.insert_silence(0, MAX_WAVE_FRAMES, &t).unwrap_err(),
            w.insert_silence(5, 1, &t).unwrap_err(),
            w.insert_silence(0, 0, &t).unwrap_err(),
            w.paste(0, &WaveClip { channels: vec![vec![f32::MAX; 2], vec![0.0; 2]] }, PasteMode::Mix, ALL_CHANNELS, &t).unwrap_err(),
            w.paste(3, &WaveClip { channels: vec![vec![0.0; 2], vec![0.0; 2]] }, PasteMode::Overwrite, ALL_CHANNELS, &t).unwrap_err(),
            w.paste(0, &WaveClip { channels: vec![vec![0.0; 2]] }, PasteMode::Insert, ALL_CHANNELS, &t).unwrap_err(),
            w.paste(0, &WaveClip { channels: vec![vec![0.0; 2], vec![0.0]] }, PasteMode::Insert, ALL_CHANNELS, &t).unwrap_err(),
            w.paste(0, &WaveClip { channels: vec![vec![f32::INFINITY], vec![0.0]] }, PasteMode::Insert, ALL_CHANNELS, &t).unwrap_err(),
            w.snap_to_zero_crossing(5, ALL_CHANNELS, SnapDirection::Nearest, 1, &t).unwrap_err(),
        ];
        use ScoreError::*;
        assert_eq!(
            errs.to_vec(),
            vec![
                OutOfRange, OutOfRange, OutOfRange, ChannelMismatch, ChannelMismatch, NotFinite, OutOfRange, NotFinite, OutOfRange,
                OutOfRange, OutOfRange, OutOfRange, OutOfRange, CapacityExceeded, OutOfRange, OutOfRange, NotFinite, OutOfRange,
                ChannelMismatch, LengthMismatch, NotFinite, OutOfRange
            ]
        );
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(w.fade(all, FadeDirection::In, FadeCurve::Linear, &canceled).unwrap_err(), Canceled);
        assert_eq!(w.normalize(all, -1.0, true, &canceled).unwrap_err(), Canceled);
        assert_eq!(bits(&w.channels()[0]), bits(&original.channels()[0]));
        assert_eq!(bits(&w.channels()[1]), bits(&original.channels()[1]));
    });
}
