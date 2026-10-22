//! Lane SMB-PS-SMART-FILTER-WIRING: face-aware liquify needs an on-device face-landmark model
//! (STU-FX-024 face-aware mode, STU-RAS-019); none is installed and nothing is downloaded, so it
//! refuses `model_unavailable` after the inputs are checked.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::liquify::{FaceAwareSliders, LiquifyLimits, LiquifyMesh, face_aware_liquify};

#[test]
fn face_aware_liquify_refuses_model_unavailable() {
    let mesh = LiquifyMesh::new(Rect { x: 0, y: 0, width: 32, height: 32 }, 4, &LiquifyLimits::default()).unwrap();
    let sliders = FaceAwareSliders { eye_size: 20.0, smile: 35.0, face_width: -10.0, ..FaceAwareSliders::default() };
    assert_eq!(face_aware_liquify(&mesh, &sliders).unwrap_err().code(), "model_unavailable");
    assert_eq!(face_aware_liquify(&mesh, &FaceAwareSliders::default()).unwrap_err().code(), "model_unavailable");
    let bad = FaceAwareSliders { jawline: f64::NAN, ..FaceAwareSliders::default() };
    assert_eq!(face_aware_liquify(&mesh, &bad).unwrap_err().code(), "nonfinite");
    // The mesh is untouched (the function only reads it).
    assert!(mesh.is_identity());
}
