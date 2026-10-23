//! Lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 2: "convert audio to keyframes" on the motion side.
//! A per-frame amplitude series becomes three LINEAR-keyed slider properties of an amplitude layer
//! (STU-MOT-016 audio on the ordinary keyframe model, STU-MOT-017 effect parameters are ordinary
//! properties), and an expression reads the `both_channels` slider through the store's HostRead.
//! After Effects naming/scale parity UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_motion::*;

const TPF: u64 = TICKS_PER_SECOND / 30;

/// Test provider: the expression source is one property path; the result is its value at the tick.
struct ReadPath;
impl ExpressionProvider for ReadPath {
    fn profile_id(&self) -> &'static str {
        "test.read_path"
    }
    fn evaluate(&self, r: &ExprRequest<'_>, host: &dyn HostRead, fuel: &mut Fuel, _c: &CancellationToken) -> Result<Vec<f64>, ExprError> {
        host.read(&PropertyPath::new(r.source).unwrap(), r.tick, fuel)
    }
}

#[test]
fn amplitude_series_become_expression_readable_slider_keyframes() {
    let mut store = PropertyStore::new();
    let left = [10.0, 20.0, 40.0];
    let right = [0.0, 0.0, 20.0];
    let both = [5.0, 10.0, 30.0];
    let paths = insert_audio_amplitude(&mut store, "SCMP-main/SLYR-amplitude", [&left, &right, &both], 0, TPF).unwrap();
    assert_eq!(paths[2].as_str(), "SCMP-main/SLYR-amplitude/effects/both_channels/slider");
    let p = store.get(&paths[0]).unwrap();
    assert_eq!(p.keyframes().len(), 3);
    assert!(p.keyframes().iter().all(|k| k.in_interp == Interpolation::Linear && k.out_interp == Interpolation::Linear));
    assert_eq!(p.keyframes()[2].time.value(), 2 * TPF);

    // An opacity whose expression reads the both-channels slider.
    let opacity = PropertyPath::new("SCMP-main/SLYR-logo/transform/opacity").unwrap();
    store.insert(opacity.clone(), Property::new("opacity", vec![100.0], vec![]).unwrap().with_expression(paths[2].as_str()).unwrap()).unwrap();
    store.set_reads(&opacity, &[paths[2].clone()]).unwrap();
    // Halfway between frames 1 and 2: linear keys -> 30 (left) and 20 (both).
    let tick = EvalTick::new((TPF + TPF / 2) as i64);
    let ctx = StoreContext { expected_revision: store.revision(), tick, seed: 0 };
    let ev = store.evaluate_tick(&ctx, &ReadPath, &mut Fuel::new(1000), &CancellationToken::default()).unwrap();
    assert!((ev.get(&paths[0]).unwrap().value[0] - 30.0).abs() < 1e-9);
    assert!((ev.get(&paths[2]).unwrap().value[0] - 20.0).abs() < 1e-9);
    assert!((ev.get(&opacity).unwrap().value[0] - 20.0).abs() < 1e-9, "expression read the slider");

    // Refusals leave the store unchanged: occupied path, NaN, negative, length mismatch, empty.
    let rev = store.revision();
    let len = store.len();
    let r = |store: &mut PropertyStore, s: [&[f64]; 3]| insert_audio_amplitude(store, "SCMP-main/SLYR-amplitude", s, 0, TPF).err();
    assert_eq!(r(&mut store, [&left, &right, &both]), Some(MotionError::DuplicateProperty));
    let r2 = |store: &mut PropertyStore, s: [&[f64]; 3]| insert_audio_amplitude(store, "SCMP-main/SLYR-amp2", s, 0, TPF).err();
    assert_eq!(r2(&mut store, [&[1.0, f64::NAN, 2.0], &right, &both]), Some(MotionError::NonFinite));
    assert_eq!(r2(&mut store, [&left, &[0.0, -1.0, 0.0], &both]), Some(MotionError::OutOfBounds));
    assert_eq!(r2(&mut store, [&left, &right, &[1.0]]), Some(MotionError::ComponentMismatch));
    assert_eq!(r2(&mut store, [&[], &[], &[]]), Some(MotionError::KeyframeLimit));
    assert_eq!(insert_audio_amplitude(&mut store, "SCMP-main/SLYR-amp3", [&left, &right, &both], u64::MAX - 1, TPF).err(), Some(MotionError::TickOverflow));
    assert_eq!((store.revision(), store.len()), (rev, len));
}
