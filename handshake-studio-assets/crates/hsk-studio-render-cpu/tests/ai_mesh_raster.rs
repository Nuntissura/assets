//! Lane SMB-AI-TYPE-PATH-RECOLOR slice 3: CPU rasterisation of a gradient mesh (STU-VEC-034,
//! STU-COL-036). On an axis-aligned grid patch the patch map is affine, so every pixel centre
//! must carry the exact bilinear colour of its `(u, v)` (hand-derived below). GPU parity is
//! NOT_PROVEN: render-gpu has no vector or triangle pipeline (module docs).
use hsk_studio_nib::mesh::{GradientMesh, MeshLimits};
use hsk_studio_render_cpu::mesh::*;

fn lerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

#[test]
fn ai_mesh_raster_bilinear_colours_transparency_frame_and_hostile_input() {
    let lim = MeshLimits::default();
    let (red, green, blue, white) = ([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
    let mut m = GradientMesh::grid([0.0, 0.0, 4.0, 4.0], 1, 1, red, &lim).unwrap();
    m.set_colour(0, 1, green).unwrap();
    m.set_colour(1, 0, blue).unwrap();
    m.set_colour(1, 1, white).unwrap();
    let frame = MeshFrame { x0: 0.0, y0: 0.0, width: 4, height: 4, px_per_unit: 1.0 };
    let r = rasterize_gradient_mesh(&m, &frame, &MeshRasterLimits::default()).unwrap();
    // Pixel (x, y) centre = (x + 0.5, y + 0.5) -> u = (x + 0.5) / 4, v = (y + 0.5) / 4;
    // colour = lerp(lerp(red, green, u), lerp(blue, white, u), v), opaque.
    for y in 0..4u32 {
        for x in 0..4u32 {
            let (u, v) = ((x as f32 + 0.5) / 4.0, (y as f32 + 0.5) / 4.0);
            let want = lerp(lerp(red, green, u), lerp(blue, white, u), v);
            let got = r.pixel(x, y).unwrap();
            assert!(got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-6), "({x},{y}) {got:?} != {want:?}");
        }
    }
    // Point transparency composites premultiplied: every point at alpha 0.5 -> colour x 0.5, alpha 0.5.
    let mut half = GradientMesh::grid([0.0, 0.0, 4.0, 4.0], 1, 1, [0.0, 0.0, 1.0, 0.5], &lim).unwrap();
    let r = rasterize_gradient_mesh(&half, &frame, &MeshRasterLimits::default()).unwrap();
    assert_eq!(r.pixel(1, 2), Some([0.0, 0.0, 0.5, 0.5]));
    half.set_colour(0, 0, [0.0, 0.0, 1.0, 0.0]).unwrap();
    // The frame maps units to pixels: px = (x - x0) * s. At s = 2, x0 = y0 = -1 the mesh covers
    // pixels 2..10; pixel (0, 0) is empty, pixel (9, 9) inside.
    let shifted = MeshFrame { x0: -1.0, y0: -1.0, width: 12, height: 12, px_per_unit: 2.0 };
    let r = rasterize_gradient_mesh(&m, &shifted, &MeshRasterLimits::default()).unwrap();
    assert_eq!(r.pixel(0, 0), Some([0.0; 4]));
    assert_eq!(r.pixel(1, 1), Some([0.0; 4]));
    assert_eq!(r.pixel(9, 9).unwrap()[3], 1.0);
    assert_eq!(r.pixel(10, 10), Some([0.0; 4]));
    assert_eq!(r.pixel(12, 0), None);
    // Hostile frames, budgets and meshes.
    let bad = |f: MeshFrame| rasterize_gradient_mesh(&m, &f, &MeshRasterLimits::default()).unwrap_err();
    assert_eq!(bad(MeshFrame { width: 0, ..frame }), MeshRasterError::InvalidInput("frame"));
    assert_eq!(bad(MeshFrame { px_per_unit: f64::NAN, ..frame }), MeshRasterError::InvalidInput("frame"));
    assert_eq!(bad(MeshFrame { width: u32::MAX, height: u32::MAX, ..frame }), MeshRasterError::OverLimit("max_pixels"));
    let tight = MeshRasterLimits { max_work: 3, ..MeshRasterLimits::default() };
    assert_eq!(rasterize_gradient_mesh(&m, &frame, &tight), Err(MeshRasterError::OverLimit("max_work")));
    let mut nan = m.clone();
    nan.v_handles[0][1] = [f64::NAN, 0.0];
    assert_eq!(rasterize_gradient_mesh(&nan, &frame, &MeshRasterLimits::default()).unwrap_err().code(), "mesh_invalid_input");
}
