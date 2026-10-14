//! SMB-HARNESS-UI-3 slice 2 (feature `viewer`): the viewer's brush mix is pigment's
//! `masked_replace` itself. Parity: a soft stroke painted by the viewer (64 x 64 engine blocks
//! inside a 256-pixel tile) equals, byte for byte, one direct `masked_replace` call that this test
//! builds on its own over the whole 32 x 32 canvas region (a different block decomposition).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_harness::view::edit::BrushSettings;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::Viewer;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort, TileAddress};
use hsk_studio_pigment::brush::{BrushDynamics, BrushLimits, BrushTip, Channel, Control, StrokeSample, crop_to, paint_replacement, place_dabs, stroke_coverage};
use hsk_studio_pigment::caller::{MemoryAdmission, SerializedOwner};
use hsk_studio_pigment::render_plan::MaskFormat;
use hsk_studio_pigment::{Grid, Mask, Outcome, Rect, Request, ResolvedTile, TileItem, masked_replace};
use hsk_studio_prism::{ProfileInput, WORKING_PROFILE_LINEAR_SRGB, profile_hash};

struct Quiet;
impl SinkPort for Quiet {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}

fn paper_tile(v: &Viewer) -> (String, TileRef) {
    let snap = v.run().snapshot().unwrap();
    let layer = snap.document().layers.iter().find(|l| l.name == "paper").unwrap();
    let Payload::Raster { tiles } = &layer.payload else { panic!("raster") };
    (layer.layer_id.clone(), tiles[0].clone())
}

/// `w x h` region of a tile body (`rgb_f32le` cell, then `f32le` alpha cell; 256-pixel stride).
fn region(body: &[u8], w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
    let (mut c, mut a) = (Vec::new(), Vec::new());
    for y in 0..h {
        c.extend_from_slice(&body[y * 256 * 12..y * 256 * 12 + w * 12]);
        let base = 256 * 256 * 12 + y * 256 * 4;
        a.extend_from_slice(&body[base..base + w * 4]);
    }
    (c, a)
}

#[test]
fn viewer_brush_equals_a_direct_masked_replace_call() {
    let scratch = Scratch::new("brush-parity");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(32, 32, "paper", [40, 120, 200])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    let (layer_id, tile) = paper_tile(&v);
    let before = v.run().render_assets().unwrap()[&tile.content_digest.digest].clone();

    // A soft, half-opacity stroke: partial coverage everywhere along its edge.
    let brush = BrushSettings { diameter_px: 14.0, hardness_pct: 30.0, spacing_pct: 10.0, opacity_pct: 60.0, rgb8: [250, 20, 10] };
    v.set_brush(brush);
    let samples: Vec<StrokeSample> = [(3.0, 5.0), (28.5, 26.0)].iter().map(|&(x, y)| StrokeSample { x, y, pressure: Some(1.0), time_ms: None }).collect();
    v.paint(&layer_id, &samples).unwrap_or_else(|e| panic!("{e}"));
    let (_, painted) = paper_tile(&v);
    let after = v.run().render_assets().unwrap()[&painted.content_digest.digest].clone();

    // The same stroke coverage, computed here from pigment's brush with the viewer's settings.
    let limits = BrushLimits::default();
    let tip = BrushTip::round(brush.diameter_px, brush.hardness_pct, brush.spacing_pct);
    let dynamics = BrushDynamics {
        size: Channel { base_value: 100.0, minimum: 0.0, jitter: 0.0, control: Control::PenPressure },
        opacity: Channel { base_value: brush.opacity_pct, ..Channel::off() },
        flow: Channel::off(),
    };
    let dabs = place_dabs(&tip, &dynamics, &samples, &limits).unwrap();
    let cell = Rect { x: 0, y: 0, width: 256, height: 256 };
    let coverage = stroke_coverage(&dabs, tip.hardness_pct, cell, &limits).unwrap();
    let canvas = Rect { x: 0, y: 0, width: 32, height: 32 };
    let mask_plane = crop_to(&coverage, canvas, &limits).unwrap();
    let partial = mask_plane.data.iter().filter(|m| **m > 0 && **m < u16::MAX).count();
    assert!(partial > 50, "the stroke has partial coverage ({partial} pixels)");
    let mask = mask_plane.to_mask_bytes(MaskFormat::CoverageU16Le).unwrap();

    // One direct engine call over the 32 x 32 region (1024 pixels): its own grid and view.
    let linear = |v: u8| {
        let c = f32::from(v) / 255.0;
        if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let rgb = brush.rgb8.map(linear);
    let (colour, alpha) = region(&before, 32, 32);
    let (rc, ra) = paint_replacement(canvas, rgb, &limits).unwrap();
    let document = DomainId::parse(&v.run().snapshot().unwrap().document().document_id).unwrap();
    let layer = DomainId::parse(&layer_id).unwrap();
    let profile = DomainId::parse(&tile.colour_profile_id).unwrap();
    let mut view = tile.clone();
    view.width.value = 32.0;
    view.height.value = 32.0;
    let items = [TileItem { before: view_of(&view, canvas, &colour, &alpha, &profile), replacement: view_of(&view, canvas, &rc, &ra, &profile), expected_property_revision: 3 }];
    let actor = ActorContext::new("a", "p", "oa", "op", "s", "parity").unwrap();
    let request = Request {
        transport_version: 1,
        operation: "masked_replace",
        document_id: &document,
        layer_id: &layer,
        command_id: "parity",
        correlation_id: 9,
        actor: &actor,
        base_revision: 1,
        cancel_epoch: 2,
        grid: Grid { origin_x: 0, origin_y: 0, tile_width: 32, tile_height: 32 },
        rectangle: canvas,
        mask: Mask { bounds: canvas, sample_format: "coverage_u16le", bytes: &mask, stride_bytes: 64, sha256: profile_hash(&mask) },
        tiles: &items,
        byte_limit: 4 << 20,
    };
    let mut owner = SerializedOwner::new(
        document.clone(),
        layer.clone(),
        actor.clone(),
        2,
        vec![(TileAddress::new(layer.clone(), &tile.object_key, tile.column, tile.row).unwrap(), 3)],
        MemoryAdmission::new(4 << 20).unwrap(),
        Quiet,
    )
    .unwrap();
    let Outcome::Accepted(patch) = masked_replace(&request, &mut owner, &CancellationToken::default()) else {
        panic!("engine refused the direct call")
    };
    let (want_c, want_a) = (patch.updates[0].after.colour.to_vec(), patch.updates[0].after.alpha.to_vec());
    let (got_c, got_a) = region(&after, 32, 32);
    assert_ne!(got_c, colour, "the stroke changed the region");
    assert_eq!(got_c, want_c, "viewer colour == direct masked_replace");
    assert_eq!(got_a, want_a, "viewer alpha == direct masked_replace");
    // Outside the canvas region the tile body is untouched.
    assert_eq!(&after[32 * 12..256 * 12], &before[32 * 12..256 * 12]);
}

/// The engine's view of a 32 x 32 region (tight strides, linear light, straight alpha).
fn view_of<'a>(view: &'a TileRef, bounds: Rect, c: &'a [u8], a: &'a [u8], profile: &'a DomainId) -> ResolvedTile<'a> {
    ResolvedTile {
        tile_ref: view,
        bounds,
        sample_format: "rgb_f32le",
        colour_bytes: c,
        colour_stride_bytes: u64::from(bounds.width) * 12,
        colour_sha256: profile_hash(c),
        alpha_bytes: a,
        alpha_stride_bytes: u64::from(bounds.width) * 4,
        alpha_sha256: profile_hash(a),
        transfer: "linear_light",
        alpha_association: "straight",
        profile: ProfileInput { profile_id: profile, bytes: WORKING_PROFILE_LINEAR_SRGB, expected_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB) },
    }
}
