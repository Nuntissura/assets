//! Desktop-only cases owned once by bridge_contracts; fixtures are never engine proof.
use kumihimo_egui::{AdapterAction, AdapterError, DesktopAdapter, DesktopHost, SurfaceBinding};
use kumihimo_render_bridge::{
    frame::*,
    gpu::{decode_engine_png, sha256},
    provider::{new_composition, vector_pick},
};

fn request() -> FrameRequest {
    FrameRequest {
        version: 1,
        binding: FrameBinding {
            scope: ContentScope {
                identity: ScopeIdentity {
                    lineage: "test-layout".into(),
                    incarnation: "test-core".into(),
                },
                session: "test-session".into(),
                provider: 1,
                resource: 2,
                target: ScopeTarget::View(3),
                connection: 1,
                instance: 1,
                mount: 1,
            },
            engine_incarnation: 1,
            geometry_revision: 1,
            surface_epoch: 1,
        },
        request_id: 1,
        sequence: 1,
        deadline_ms: 1000,
        width: 2,
        height: 2,
    }
}
fn png() -> Vec<u8> {
    use image::ImageEncoder;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
            2,
            2,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    bytes
}
#[test]
fn exact_png_hash_header_limits_and_top_left_rgba_are_enforced_before_decode() {
    let bytes = png();
    let expected = sha256(&bytes);
    let limits = BridgeLimits::default();
    let frame = decode_engine_png(&bytes, request(), &expected, &limits).unwrap();
    assert_eq!(&frame.rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(&frame.rgba[8..12], &[0, 0, 255, 255]);
    assert_eq!(frame.row_stride, 8);
    assert!(decode_engine_png(&bytes, request(), &"0".repeat(64), &limits).is_err());
    let mut bomb = bytes.clone();
    bomb[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(decode_engine_png(&bomb, request(), &sha256(&bomb), &limits).is_err());
    let tiny = BridgeLimits {
        max_rgba_bytes: 15,
        ..limits
    };
    assert!(decode_engine_png(&bytes, request(), &expected, &tiny).is_err());
    let mut wrong = request();
    wrong.width = 1;
    assert!(decode_engine_png(&bytes, wrong, &expected, &BridgeLimits::default()).is_err());
}
#[test]
fn canonical_slots_map_single_scaled_input_and_reject_popout_without_mutation() {
    let mut composition = new_composition().unwrap();
    let ctx = egui::Context::default();
    let mut adapter = DesktopAdapter::default();
    let surface = SurfaceBinding {
        root: composition.root,
        viewport: egui::ViewportId::ROOT,
        epoch: 1,
        bounds: kumihimo_core::Rect::new(0.0, 0.0, 1000.0, 720.0).unwrap(),
        pixels_per_point: 1.5,
    };
    let mut result = None;
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(kumihimo_egui::to_egui(surface.bounds));
    raw.viewports
        .entry(surface.viewport)
        .or_default()
        .native_pixels_per_point = Some(1.5);
    let _ = ctx.run(raw, |ctx| {
        result = Some(adapter.show(
            ctx,
            &mut composition.controller,
            surface,
            &mut composition.host,
        ))
    });
    let report = result.unwrap().unwrap();
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert_eq!(report.slots.len(), 4);
    let slot = &composition.host.slots[&composition.views[0]];
    let global = egui::pos2(
        (slot.bounds.x() + 20.0) as f32,
        (slot.bounds.y() + 10.0) as f32,
    );
    let local = slot.local_point(global).unwrap();
    assert_eq!(
        kumihimo_render_bridge::mapping::local_dip_to_engine_css(
            [local.x as f64, local.y as f64],
            slot.scale as f64,
            slot.physical_size
        )
        .unwrap(),
        [30, 15]
    );
    assert!(slot.local_point(egui::pos2(-1.0, -1.0)).is_err());
    let before = composition.controller.snapshot().clone();
    assert!(matches!(
        composition.host.native_action(
            &mut composition.controller,
            AdapterAction::PopoutView(composition.views[0])
        ),
        Err(AdapterError::Unsupported(_))
    ));
    assert_eq!(&before, composition.controller.snapshot());
    assert!(vector_pick([0.5, 0.5]));
    assert!(!vector_pick([0.95, 0.95]));
    assert!(!vector_pick([f32::NAN, 0.5]));
}
