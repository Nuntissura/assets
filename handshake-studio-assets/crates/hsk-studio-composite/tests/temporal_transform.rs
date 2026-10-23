//! Temporal lowering of resampling transforms (lane SMB-PIGMENT-TRANSFORM slice 4): fractional
//! translation, scale, rotation and anchor lower to pigment's layer-transform operator when the
//! temporal source declares the layer's resample filter. Fixture `tiled` (as tests/temporal.rs):
//! root group = [raster 2 (two tiles -> isolated group), raster 3 (one tile at (0, 64) + 64x64)].
//! Expected matrices come from the After Effects layer model written out by hand below.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::{
    TRANSFORM_MULTI_TILE_LAYER, TRANSFORM_REQUIRES_RESAMPLING, TRANSLATE_NON_RASTER_LAYER,
};
use hsk_studio_composite::{
    BlendSpace, CompositeError, FrameLowering, FrameRequest, LayerFrameState, Limits,
    TemporalError, TemporalSource, TileInfo, TileInfoSource, lower_frame,
};
use hsk_studio_folio::{Budget, Snapshot, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, Exactness, OutputTarget, PlanError, PlanNode, SampleFormat,
};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter, TransformError};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 128, height: 128 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
struct Info;
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        let h = |salt: u8| [tile.object_key.bytes().fold(salt, |a, b| a.wrapping_mul(31).wrapping_add(b)); 32];
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: h(1),
            alpha_sha256: h(2),
        })
    }
    fn mask_sha256(&self, tiles: &[TileRef]) -> Option<[u8; 32]> {
        Some([tiles.first()?.object_key.len() as u8; 32])
    }
}
fn tiled() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["layers"][1]["payload"] =
        json!({"payload_kind":"raster","tiles":[tile(2,"t00",0,0,64,64),tile(2,"t10",1,0,64,32)]});
    v["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",0,1,64,64)]});
    v
}
/// Layer 3 with a 64x64 coverage mask over its tile.
fn masked(linked: bool) -> Value {
    let mut v = tiled();
    let mut m = tile(3, "m1", 0, 1, 64, 64);
    m["format"] = json!("coverage_u8");
    v["layers"][2]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,
        "clipped":false,"isolation":"isolated","mask":{"tiles":[m],"enabled":true,"linked":linked}});
    v
}

/// Layer `n` animated with a fixed state, a declared filter and an optional anchor.
struct Source {
    layer: String,
    state: LayerFrameState,
    filter: Option<ResampleFilter>,
    anchor: Option<[f64; 2]>,
}
impl TemporalSource for Source {
    fn layer_state(&self, id: &str, _: i64, _: &CancellationToken) -> Result<Option<LayerFrameState>, TemporalError> {
        Ok((id == self.layer).then_some(self.state))
    }
    fn resample_filter(&self, id: &str) -> Option<ResampleFilter> {
        self.filter.filter(|_| id == self.layer)
    }
    fn layer_anchor(&self, _: &str, _: i64, _: &CancellationToken) -> Result<Option<[f64; 2]>, TemporalError> {
        Ok(self.anchor)
    }
}
fn source(n: u8, state: LayerFrameState, filter: Option<ResampleFilter>) -> Source {
    Source { layer: identity("SLYR", n), state, filter, anchor: None }
}
fn state(t: [f64; 2], scale: [f64; 2], rotation_deg: f64) -> LayerFrameState {
    LayerFrameState { translate_px: t, scale, rotation_deg, ..LayerFrameState::IDENTITY }
}
fn frame(snap: &Snapshot, src: &dyn TemporalSource) -> Result<FrameLowering, TemporalError> {
    let root = addr(1, "mix", "out");
    let req = FrameRequest {
        snapshot: snap,
        expected_revision: 7,
        blend_space: BlendSpace::Encoded,
        limits: Limits::default(),
        budget: Budget::default(),
        resolver: &Available,
        root: &root,
        target: target(),
        info: &Info,
        timings: &[],
    };
    lower_frame(&req, src, 0, &CancellationToken::default())
}
fn near(a: Affine2, b: [f64; 6]) {
    for (x, y) in a.m.iter().zip(b) {
        assert!((x - y).abs() < 1e-12, "{:?} vs {b:?}", a.m);
    }
}

#[test]
fn fractional_motion_lowers_to_the_transform_operator() {
    bounded_case("fractional_motion_lowers_to_the_transform_operator", || {
        let snap = open(&tiled());
        // Translate (0.5, -1.25), scale (1.5, 0.5), rotate 30 degrees about the tile centre (32, 96).
        let s = state([0.5, -1.25], [1.5, 0.5], 30.0);
        let f = frame(&snap, &source(3, s, Some(ResampleFilter::Bicubic))).unwrap();
        let root = f.lowering.plan.root.0;
        assert_eq!(f.extensions.layers.len(), 1);
        let e = &f.extensions.layers[0];
        assert_eq!((e.group.0, e.layer), (root, 1));
        let t = e.transform.expect("transform entry");
        assert_eq!(t.filter, ResampleFilter::Bicubic);
        assert!(!t.transform_mask);
        // Hand-derived: x' = a + t + R S (x - a); R = [c -s; s c], S = diag(1.5, 0.5).
        let (sin, cos) = (0.5f64, 3f64.sqrt() / 2.0);
        let (a, b, c, d) = (1.5 * cos, -0.5 * sin, 1.5 * sin, 0.5 * cos);
        let (ax, ay) = (32.0, 96.0);
        near(t.affine, [a, b, ax + 0.5 - (a * ax + b * ay), c, d, ay - 1.25 - (c * ax + d * ay)]);
        // The anchor maps to anchor + translation.
        let (x, y) = t.affine.apply(32.0, 96.0);
        assert!((x - 32.5).abs() < 1e-9 && (y - 94.75).abs() < 1e-9);
        // The tile itself stays where it was authored; the ledger records the operator's class.
        let tile = f.lowering.plan.nodes.iter().find_map(|n| match n {
            PlanNode::Tile(t) if t.placement.y == 64 => Some(t.placement),
            _ => None,
        });
        assert_eq!(tile, Some(Rect { x: 0, y: 64, width: 64, height: 64 }));
        assert!(f.lowering.exactness.operators.iter().any(|o| o.key == "transform_bicubic" && o.exactness == Exactness::Approximate));
        assert_eq!(f.applied, [(identity("SLYR", 3), s)]);

        // An explicit anchor (10, 70) and a quarter turn: x' = (10 - (y - 70), 70 + (x - 10)).
        let mut anchored = source(3, state([0.0, 0.0], [1.0, 1.0], 90.0), Some(ResampleFilter::NearestNeighbour));
        anchored.anchor = Some([10.0, 70.0]);
        let t = frame(&snap, &anchored).unwrap().extensions.layers[0].transform.unwrap();
        assert_eq!(t.affine.m, [0.0, -1.0, 80.0, 1.0, 0.0, 60.0]);

        // A fractional translation alone is a pure translate matrix.
        let half = frame(&snap, &source(3, state([0.5, 0.25], [1.0, 1.0], 0.0), Some(ResampleFilter::Bilinear))).unwrap();
        assert_eq!(half.extensions.layers[0].transform.unwrap().affine, Affine2::translate(0.5, 0.25));

        // An integer translation keeps the exact tile-move path: no envelope, the same lowering as
        // a source that declares no filter.
        let int = state([3.0, -2.0], [1.0, 1.0], 0.0);
        let with = frame(&snap, &source(3, int, Some(ResampleFilter::Bicubic))).unwrap();
        let without = frame(&snap, &source(3, int, None)).unwrap();
        assert!(with.extensions.is_empty());
        assert_eq!(with, without);
    });
}

#[test]
fn masks_collapse_and_refusals_stay_typed() {
    bounded_case("masks_collapse_and_refusals_stay_typed", || {
        let s = state([0.5, 0.0], [1.25, 1.25], 10.0);
        let bilinear = Some(ResampleFilter::Bilinear);
        // A linked mask moves with its content; an unlinked one stays in place.
        for linked in [true, false] {
            let f = frame(&open(&masked(linked)), &source(3, s, bilinear)).unwrap();
            let t = f.extensions.layers[0].transform.unwrap();
            assert_eq!(t.transform_mask, linked);
            let root = &f.lowering.plan.nodes[f.lowering.plan.root.0 as usize];
            let PlanNode::Group(g) = root else { panic!("root group") };
            assert_eq!(g.layers[1].mask.unwrap().placement, Rect { x: 0, y: 64, width: 64, height: 64 });
        }
        let snap = open(&tiled());
        // A scale of 0 renders nothing: the layer is hidden for the frame, no entry.
        let flat = frame(&snap, &source(3, state([0.0; 2], [0.0, 1.0], 0.0), bilinear)).unwrap();
        assert_eq!(flat.hidden, [identity("SLYR", 3)]);
        assert!(flat.extensions.is_empty());
        let unsupported = |n: u8, feature| Err(TemporalError::Unsupported { layer_id: identity("SLYR", n), feature });
        // No declared filter: refused as before, never a silent default.
        assert_eq!(frame(&snap, &source(3, s, None)), unsupported(3, TRANSFORM_REQUIRES_RESAMPLING));
        // A raster layer lowered to two tiles would seam at the tile edge: refused.
        assert_eq!(frame(&snap, &source(2, s, bilinear)), unsupported(2, TRANSFORM_MULTI_TILE_LAYER));
        // A group is not addressed by tiles.
        assert_eq!(frame(&snap, &source(1, s, bilinear)), unsupported(1, TRANSLATE_NON_RASTER_LAYER));
        // Hostile matrices are pigment's typed refusals.
        match frame(&snap, &source(3, state([0.5, 0.0], [1.0e9, 1.0], 0.0), bilinear)) {
            Err(TemporalError::Composite(CompositeError::Plan(PlanError::TransformMatrix { error, .. }))) => {
                assert_eq!(error, TransformError::ScaleOutOfRange);
            }
            other => panic!("{other:?}"),
        }
        let mut nan_anchor = source(3, s, bilinear);
        nan_anchor.anchor = Some([f64::NAN, 0.0]);
        assert_eq!(
            frame(&snap, &nan_anchor),
            Err(TemporalError::InvalidState { layer_id: identity("SLYR", 3), field: "anchor_px" })
        );
        assert_eq!(
            frame(&snap, &source(3, state([f64::NAN, 0.0], [2.0, 1.0], 0.0), bilinear)),
            Err(TemporalError::InvalidState { layer_id: identity("SLYR", 3), field: "translate_px" })
        );
    });
}
