//! SMB-SCORE-EFFECTS-4 slice 4 (score side of PR-23): clip source windows and fades, so a pulse
//! audio crossfade maps to an abutting fade-out / fade-in pair with progress k / frames.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

fn render_all(session: &mut Session, frames: usize, blocks: &[usize]) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0_f32; frames], vec![0.0_f32; frames]);
    let (mut at, mut i) = (0, 0);
    while at < frames {
        let len = blocks[i.min(blocks.len() - 1)].min(frames - at);
        i += 1;
        session.render(at as u64, &mut l[at..at + len], &mut r[at..at + len], &token).unwrap();
        at += len;
    }
    [l, r]
}

/// The documented clip gain: product of the fade gains that cover clip frame `k` of `len`.
fn faded(x: f32, k: u64, len: u64, fade_in: Option<Fade>, fade_out: Option<Fade>) -> f32 {
    let mut g = 1.0_f64;
    if let Some(f) = fade_in.filter(|f| k < f.frames) {
        g *= f.in_gain(k);
    }
    if let Some(f) = fade_out.filter(|f| k >= len - f.frames) {
        g *= f.out_gain(k - (len - f.frames));
    }
    if g == 1.0 { x } else { (f64::from(x) * g) as f32 }
}

#[test]
fn abutting_fades_form_exact_linear_and_equal_power_crossfades() {
    bounded_case("crossfade", || {
        // Source audio (stereo); clip A = source frames 1000..7000 at timeline 0, clip B = another
        // source's frames 0..5000 at timeline 5000; crossfade span [5000, 6000) = 1000 frames.
        let src_a = vec![noise(1, 10_000), noise(2, 10_000)];
        let src_b = vec![noise(3, 5000), noise(4, 5000)];
        for curve in [FadeCurve::Linear, FadeCurve::EqualPower] {
            let fade = Fade { frames: 1000, curve };
            let mut a = TrackSpec::from_source(&src_a, 1000, 6000, 0).unwrap();
            a.fade_out = Some(fade);
            let mut b = TrackSpec::from_source(&src_b, 0, 5000, 5000).unwrap();
            b.fade_in = Some(fade);
            let mut session = Session::new(RATE, 512, vec![a, b]).unwrap();
            let out = render_all(&mut session, 11_000, &[512]);
            for t in 0..11_000usize {
                for c in 0..2 {
                    let mut want = 0.0_f64;
                    if t < 6000 {
                        want += f64::from(faded(src_a[c][1000 + t], t as u64, 6000, None, Some(fade)));
                    }
                    if (5000..10_000).contains(&t) {
                        want += f64::from(faded(src_b[c][t - 5000], (t - 5000) as u64, 5000, Some(fade), None));
                    }
                    assert_eq!(out[c][t].to_bits(), (want as f32).to_bits(), "{curve:?} t {t} c {c}");
                }
            }
            // Progress is k / frames on both sides of the span: at frame k the outgoing gain plus
            // (linear) or squared plus (equal power) the incoming gain is exactly 1 in f64.
            for k in 0..1000 {
                let (o, i) = (fade.out_gain(k), fade.in_gain(k));
                let total = if curve == FadeCurve::Linear { o + i } else { o * o + i * i };
                assert!((total - 1.0).abs() < 1e-15, "{curve:?} {k}");
            }
            assert_eq!((fade.in_gain(0), fade.out_gain(0)), (0.0, 1.0));
        }
    });
}

#[test]
fn fades_apply_before_a_latent_stack_and_stay_aligned() {
    bounded_case("fade_latency", || {
        let clip = noise(7, 4000);
        let fade_in = Some(Fade { frames: 300, curve: FadeCurve::EqualPower });
        let fade_out = Some(Fade { frames: 700, curve: FadeCurve::Linear });
        let dynamics = DynamicsParams { knee_db: 0.0, ..DynamicsParams::compressor(0.0, 4.0) };
        let lookahead = SidechainDynamics::new(RATE, 2, SidechainParams { dynamics, key: KeySource::Internal, lookahead_ms: 4.0 }).unwrap();
        let mut t = TrackSpec::new(vec![clip.clone(), clip.clone()], 250);
        t.fade_in = fade_in;
        t.fade_out = fade_out;
        t.effects = vec![EffectSlot::new(Box::new(lookahead))];
        let mut session = Session::new(RATE, 333, vec![t]).unwrap();
        let out = render_all(&mut session, 5000, &[333]);
        for n in 0..5000usize {
            let want = if (250..4250).contains(&n) { faded(clip[n - 250], (n - 250) as u64, 4000, fade_in, fade_out) } else { 0.0 };
            assert_eq!(out[0][n].to_bits(), want.to_bits(), "n {n}");
        }
    });
}

#[test]
fn source_windows_and_fades_refuse_bad_ranges() {
    bounded_case("fade_hostile", || {
        let src = vec![vec![0.1_f32; 100]];
        assert_eq!(TrackSpec::from_source(&src, 0, 0, 0).err(), Some(ScoreError::OutOfRange));
        assert_eq!(TrackSpec::from_source(&src, 50, 51, 0).err(), Some(ScoreError::OutOfRange));
        assert_eq!(TrackSpec::from_source(&src, usize::MAX, 2, 0).err(), Some(ScoreError::OutOfRange));
        assert_eq!(TrackSpec::from_source(&[], 0, 1, 0).err(), Some(ScoreError::InvalidLayout));
        assert_eq!(TrackSpec::from_source(&[vec![0.0; 4], vec![0.0; 5]], 0, 1, 0).err(), Some(ScoreError::LengthMismatch));
        assert_eq!(TrackSpec::from_source(&src, 40, 60, 7).unwrap().clip[0].len(), 60);
        for frames in [0, 101, u64::MAX] {
            let mut t = TrackSpec::new(src.clone(), 0);
            t.fade_out = Some(Fade { frames, curve: FadeCurve::Linear });
            assert_eq!(Session::new(RATE, 64, vec![t]).err(), Some(ScoreError::OutOfRange), "{frames}");
        }
        let mut whole = TrackSpec::new(src.clone(), 0);
        whole.fade_in = Some(Fade { frames: 100, curve: FadeCurve::EqualPower });
        whole.fade_out = Some(Fade { frames: 100, curve: FadeCurve::EqualPower });
        assert!(Session::new(RATE, 64, vec![whole]).is_ok(), "overlapping fades multiply");
    });
}
