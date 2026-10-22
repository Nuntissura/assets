//! Lane SMB-COMPOSITE-STROKE-GEOMETRY: a stroke row with its own geometry gets its own coverage
//! plane (the united outline, independent of the fill geometry); refusals are typed.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::{FillPaint, FillSpec, ImageFill, ImageInfo, ImageScale, VectorCoverage, VectorResolution, prepare_vector_layers};
use hsk_studio_pigment::blend_math::StudioBlendMode;

#[path = "support/vector_scenes.rs"]
mod scenes;
#[path = "support/stroke_geometry_scene.rs"]
mod stroke_scene;

fn res() -> VectorResolution {
    VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() }
}

#[test]
fn stroke_geometry_rows_have_their_own_planes_and_refuse_typed() {
    let cancel = CancellationToken::default();
    let vectors = prepare_vector_layers(&[stroke_scene::spec()], &res(), &cancel).unwrap();
    let plane = vectors.plane(scenes::LAYER).unwrap_or_else(|| panic!("{:?}", vectors.refusal(scenes::LAYER)));
    assert_eq!(plane.strokes.len(), 2);
    assert_eq!(plane.strokes.iter().map(|s| s.row).collect::<Vec<_>>(), vec![1, 2]);
    let rect = plane.placement.unwrap();
    // The plane covers the stroke outlines beyond the fill rectangle.
    assert!(rect.x <= 2 && rect.y <= 1 && rect.x + i64::from(rect.width) >= 30 && rect.y + i64::from(rect.height) >= 23, "{rect:?}");
    let at = |x: i64, y: i64| ((y - rect.y) * i64::from(rect.width) + (x - rect.x)) as usize;
    // Inside the diagonal band, outside the fill: stroke 255, fill 0.
    assert_eq!((plane.coverage[at(28, 7)], plane.strokes[0].coverage[at(28, 7)]), (0, 255));
    // Inside the fill, outside every outline: fill 255, strokes 0.
    assert_eq!((plane.coverage[at(10, 15)], plane.strokes[0].coverage[at(10, 15)], plane.strokes[1].coverage[at(10, 15)]), (255, 0, 0));
    // Where the two bands overlap the union counts once (full, never above 255).
    assert_eq!(plane.strokes[0].coverage[at(23, 8)], 255);
    // The old stroke row would have stroked the rectangle edge; this one does not.
    assert_eq!(plane.strokes[0].coverage[at(6, 15)], 0);

    // Typed refusals.
    let refusal = |spec: hsk_studio_composite::VectorLayerSpec| {
        let v = prepare_vector_layers(&[spec], &res(), &cancel).unwrap();
        v.refusal(scenes::LAYER)
    };
    let mut fill_after = stroke_scene::spec();
    fill_after.fills.push(FillSpec { paint: FillPaint::Solid([1.0; 4]), mode: StudioBlendMode::Normal, opacity: 1.0 });
    assert_eq!(refusal(fill_after), Some("vector_stroke_order"));
    let mut nested = stroke_scene::spec();
    nested.fills[1].paint = FillPaint::StrokeGeometry { outline: stroke_scene::bands(), paint: Box::new(FillPaint::StrokeGeometry { outline: vec![], paint: Box::new(FillPaint::Solid([1.0; 4])) }) };
    assert_eq!(refusal(nested), Some("vector_stroke_paint"));
    let mut image = stroke_scene::spec();
    let info = ImageInfo { width: 1, height: 1, format: hsk_studio_pigment::render_plan::SampleFormat::RgbF32Le, alpha: hsk_studio_pigment::render_plan::AlphaConvention::Straight, colour_sha256: [0; 32], alpha_sha256: [0; 32] };
    image.fills[1].paint = FillPaint::StrokeGeometry {
        outline: stroke_scene::bands(),
        paint: Box::new(FillPaint::Image(ImageFill { source_id: "x".into(), info, scale: ImageScale::Fill, transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], scaling_factor: 1.0, rotation: 0.0 })),
    };
    assert_eq!(refusal(image), Some("vector_stroke_paint"));
    let mut nan = stroke_scene::spec();
    nan.fills[1].paint = FillPaint::StrokeGeometry {
        outline: vec![OwnedPath { elements: vec![PathElement::MoveTo(PathPoint::new(f64::NAN, 0.0)), PathElement::LineTo(PathPoint::new(1.0, 1.0)), PathElement::Close], fill_rule: hsk_studio_composite::vector::FillRule::NonZero }],
        paint: Box::new(FillPaint::Solid([1.0; 4])),
    };
    assert_eq!(refusal(nan), Some("vector_geometry"));
    // An empty outline strokes nothing (zero coverage), the layer still draws its fill.
    let mut empty = stroke_scene::spec();
    empty.fills[1].paint = FillPaint::StrokeGeometry { outline: vec![], paint: Box::new(FillPaint::Solid([1.0; 4])) };
    let v = prepare_vector_layers(&[empty], &res(), &cancel).unwrap();
    let p = v.plane(scenes::LAYER).unwrap();
    assert!(p.strokes[0].coverage.iter().all(|c| *c == 0));
    assert!(p.coverage.contains(&255));
}
