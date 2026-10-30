//! Lane SMB-AE-MASK-FEATHER-2: re-applying a planar track to an existing mask
//! (`track_apply::apply_planar_mask`) replaces the path, its keyframes and the mode only; the
//! mask's `inverted` flag and its feather, opacity and expansion streams (lane
//! SMB-AE-MASK-FEATHER-1) are kept, static or animated (Studio reading; After Effects' behaviour is
//! UNVERIFIED, D5). A new mask id is appended with those absent. Red-first: at b64650e8 the re-apply
//! replaced the whole mask (inverted false, streams absent), so the `kept` tests failed there.
//! Also (feature `motion`): the 3D-plane mask site of `temporal::composition` (layer-space shaped
//! coverage over the plane's content rect) against `mask_coverage_shaped`, static and animated,
//! and its typed refusal naming the mask.
use hsk_studio_composite::track_apply::{TrackTiming, apply_planar_mask};
use hsk_studio_folio::{
    StudioCompositionKeyframe, StudioCompositionLayer, StudioCompositionMask, StudioCompositionMaskMode as M, StudioInterpolation,
    StudioKeyedValue, StudioLayerTiming, StudioRational,
};
use hsk_studio_pigment::track_math::{Homography, PlanarFrame, PlanarStatus};

#[cfg(feature = "motion")]
mod common;
#[cfg(feature = "motion")]
#[path = "support/composition_scenes.rs"]
mod scenes;

fn layer(masks: Vec<StudioCompositionMask>) -> StudioCompositionLayer {
    StudioCompositionLayer {
        layer_id: "SLYR-footage".to_owned(),
        timing: StudioLayerTiming { in_point: 0, out_point: 10_000, start_time: 0, stretch: StudioRational { numerator: 1, denominator: 1 } },
        parent: None,
        precomp: None,
        track_matte: None,
        streams: vec![],
        corner_pin: None,
        masks,
        three_d: None,
        camera: None,
        light: None,
        time_remap: None,
        frame_blending: hsk_studio_folio::StudioFrameBlending::Off,
        roto: None,
        effects: Vec::new(),
    }
}
fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<f64> {
    vec![x0, y0, 0.0, 0.0, 0.0, 0.0, x1, y0, 0.0, 0.0, 0.0, 0.0, x1, y1, 0.0, 0.0, 0.0, 0.0, x0, y1, 0.0, 0.0, 0.0, 0.0]
}
fn key(time: u64, value: Vec<f64>) -> StudioCompositionKeyframe {
    StudioCompositionKeyframe {
        time,
        value,
        in_interp: StudioInterpolation::Linear,
        out_interp: StudioInterpolation::Linear,
        in_tangent: vec![],
        out_tangent: vec![],
    }
}
fn keyed(v0: Vec<f64>, v1: Vec<f64>) -> StudioKeyedValue {
    StudioKeyedValue { static_value: v0.clone(), keyframes: vec![key(0, v0), key(300, v1)] }
}
/// A hand-written mask `id` over a rectangle, add, not inverted, no shape streams.
fn plain(id: &str) -> StudioCompositionMask {
    let path = rect_path(10.0, 10.0, 30.0, 25.0);
    StudioCompositionMask {
        mask_id: id.to_owned(),
        mode: M::Add,
        inverted: false,
        closed: true,
        static_value: path.clone(),
        keyframes: vec![key(0, path)],
        feather: None,
        opacity: None,
        expansion: None,
    }
}
/// Three tracked frames translating by (2, 1) px per frame.
fn planar() -> Vec<PlanarFrame> {
    (0..3)
        .map(|k| {
            let t = k as f64;
            PlanarFrame {
                frame: k,
                homography: Homography { m: [1.0, 0.0, 2.0 * t, 0.0, 1.0, t, 0.0, 0.0, 1.0] },
                quad: [[0.0; 2]; 4],
                features: 8,
                inliers: 8,
                status: PlanarStatus::Tracked,
            }
        })
        .collect()
}
const TIMING: TrackTiming = TrackTiming { first_tick: 0, ticks_per_frame: 100 };
fn track_path() -> Vec<f64> {
    rect_path(12.0, 11.0, 28.0, 22.0)
}
/// The path keyframes a re-apply of [`track_path`] over [`planar`] writes.
fn assert_tracked_path(m: &StudioCompositionMask, mode: M) {
    assert_eq!((m.mode, m.closed, m.keyframes.len()), (mode, true, 3));
    for (k, f) in m.keyframes.iter().enumerate() {
        let want: Vec<f64> = track_path().chunks_exact(6).flat_map(|v| [v[0] + 2.0 * k as f64, v[1] + k as f64, 0.0, 0.0, 0.0, 0.0]).collect();
        assert_eq!((f.time, &f.value), (100 * k as u64, &want), "key {k}");
    }
    assert_eq!(m.static_value, m.keyframes[0].value);
}

#[test]
fn reapply_keeps_each_shape_property_static_and_animated() {
    type Set = fn(&mut StudioCompositionMask, bool);
    let cases: [(&str, Set); 4] = [
        ("inverted", |m, _| m.inverted = true),
        ("feather", |m, animated| {
            m.feather = Some(if animated { keyed(vec![0.0, 0.0], vec![6.0, 2.0]) } else { StudioKeyedValue::constant(vec![3.0, 1.5]) })
        }),
        ("opacity", |m, animated| {
            m.opacity = Some(if animated { keyed(vec![0.25], vec![1.0]) } else { StudioKeyedValue::constant(vec![0.4]) })
        }),
        ("expansion", |m, animated| {
            m.expansion = Some(if animated { keyed(vec![-2.0], vec![5.0]) } else { StudioKeyedValue::constant(vec![-1.5]) })
        }),
    ];
    for (name, set) in cases {
        for animated in [false, true] {
            let mut m = plain("planar");
            set(&mut m, animated);
            let target = layer(vec![plain("before"), m.clone(), plain("after")]);
            let out = apply_planar_mask(&target, "planar", M::Subtract, &track_path(), &planar(), TIMING).unwrap();
            assert_eq!(target.masks[1], m, "{name}: the input record is untouched");
            assert_eq!(out.masks.len(), 3, "{name}: replaced in place, not appended");
            assert_eq!((&out.masks[0], &out.masks[2]), (&plain("before"), &plain("after")), "{name}: other masks unchanged");
            let got = &out.masks[1];
            assert_tracked_path(got, M::Subtract);
            assert_eq!(
                (got.inverted, &got.feather, &got.opacity, &got.expansion),
                (m.inverted, &m.feather, &m.opacity, &m.expansion),
                "{name} (animated {animated}) is kept"
            );
        }
    }
}

#[test]
fn reapply_keeps_all_shape_streams_together_and_a_new_id_is_plain() {
    let mut m = plain("planar");
    m.inverted = true;
    m.feather = Some(keyed(vec![1.0, 1.0], vec![8.0, 4.0]));
    m.opacity = Some(StudioKeyedValue::constant(vec![0.75]));
    m.expansion = Some(keyed(vec![0.0], vec![-3.0]));
    let out = apply_planar_mask(&layer(vec![m.clone()]), "planar", M::Intersect, &track_path(), &planar(), TIMING).unwrap();
    let got = &out.masks[0];
    assert_tracked_path(got, M::Intersect);
    assert_eq!(StudioCompositionMask { mode: M::Add, static_value: m.static_value.clone(), keyframes: m.keyframes.clone(), ..got.clone() }, m);
    // Applying twice is stable: the shape survives every re-apply.
    let again = apply_planar_mask(&out, "planar", M::Add, &track_path(), &planar(), TIMING).unwrap();
    assert_eq!((&again.masks[0].feather, &again.masks[0].opacity, &again.masks[0].expansion, again.masks[0].inverted), (&m.feather, &m.opacity, &m.expansion, true));
    // A new id is appended plain: not inverted, no feather / opacity / expansion.
    let added = apply_planar_mask(&out, "fresh", M::Add, &track_path(), &planar(), TIMING).unwrap();
    assert_eq!(added.masks.len(), 2);
    let fresh = &added.masks[1];
    assert_tracked_path(fresh, M::Add);
    assert!(!fresh.inverted && fresh.feather.is_none() && fresh.opacity.is_none() && fresh.expansion.is_none());
    assert_eq!(added.masks[0], *got);
    println!("NOT_PROVEN: After Effects re-track behaviour on a mask's feather, opacity, expansion and inverted (Studio reading, D5)");
}

/// The 3D-plane mask site (lane SMB-AE-MASK-FEATHER-1 wiring): a 3D layer's masks are
/// layer-space shaped coverage over its content rect, carried on the plane's lit content.
#[cfg(feature = "motion")]
mod three_d_plane {
    use super::{common, rect_path, scenes};
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_composite::mask_paths::{COMPOSITION_MASK_FEATHER_OUT_OF_RANGE, MaskAtTick, MaskShape, mask_coverage_shaped};
    use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
    use hsk_studio_composite::{BlendSpace, SceneContent, TemporalError, TileInfo, TileInfoSource};
    use hsk_studio_folio::{Budget, StudioCompositionMaskMode as M, StudioCompositionRecords, TileRef};
    use hsk_studio_pigment::Rect;
    use hsk_studio_pigment::render_plan::{AlphaConvention, SampleFormat};
    use hsk_studio_pigment::transform_math::Affine2;
    use scenes::{Pixels, composition_id, identity};
    use serde_json::{Value, json};

    const TPF: u64 = 10_584_000_000;
    fn tile(layer: u8, key: &str, w: u32, h: u32) -> Value {
        json!({"object_key":key,"layer_id":identity("SLYR",layer),"column":0,"row":0,
            "width":{"value":w,"unit":"px"},"height":{"value":h,"unit":"px"},"format":"caller-format",
            "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
            "content_digest":{"algorithm":"sha256","digest":"00"},"artifact_manifest_id":"manifest-1"})
    }
    fn document() -> Value {
        let raster = |n: u8, t: Value| {
            json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),
                "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[t]},"visible":true})
        };
        let mut v = json!({
            "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
            "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
            "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
            "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",6),"name":"camera","parent":null,
                "kind":"group","payload":{"payload_kind":"group"},"visible":true},
                {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",8),"name":"light","parent":null,
                "kind":"group","payload":{"payload_kind":"group"},"visible":true}, raster(11, tile(11, "tf", 20, 14))],
            "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
        });
        common::assign_all_profiles(&mut v);
        v
    }
    fn timing() -> Value {
        json!({"in_point":0,"out_point":TPF * 48,"start_time":0,"stretch":{"numerator":1,"denominator":1}})
    }
    fn s(v: f64) -> Value {
        json!({"static_value":[v]})
    }
    /// Camera, a light from frame 10 (the `three_d_adapter` fixture) and the 3D plane "tf" (20 x 14)
    /// carrying `mask`.
    fn records(mask: Value) -> Value {
        json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[{
            "schema_id":"hsk.studio.composition@1","composition_id":composition_id(),"name":"comp",
            "width":32,"height":24,"pixel_aspect":{"numerator":1,"denominator":1},
            "frame_rate":{"numerator":24,"denominator":1},"duration":TPF * 48,
            "background":{"r":0.1,"g":0.2,"b":0.3,"a":1.0,"colour_profile_id":null},"layers":[
                {"layer_id":identity("SLYR",6),"timing":timing(),"streams":[{"property":"position","static_value":[10.0,4.0]}],
                 "three_d":{"position_z":s(-60.0)},"camera":{"zoom":s(50.0),"point_of_interest":{"static_value":[16.0,12.0,0.0]}}},
                {"layer_id":identity("SLYR",8),"timing":{"in_point":TPF * 10,"out_point":TPF * 48,"start_time":0,"stretch":{"numerator":1,"denominator":1}},
                 "three_d":{},"light":{"light_type":"point","colour":{"static_value":[1.0,1.0,1.0]},"intensity":s(120.0)}},
                {"layer_id":identity("SLYR",11),"timing":timing(),"streams":[{"property":"position","static_value":[16.0,12.0]}],
                 "three_d":{"rotation_y":s(35.0)},"masks":[mask]}]}]})
    }
    struct Info<'a>(&'a Pixels);
    impl TileInfoSource for Info<'_> {
        fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
            let t = self.0.get(&format!("{}/{}", tile.layer_id, tile.object_key))?;
            Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, colour_sha256: t.colour_sha256, alpha_sha256: t.alpha_sha256 })
        }
    }
    /// The plane's layer-space mask coverage of every scene piece of "tf" at `frame`.
    fn plane_masks(mask: Value, frame: u64, pixels: &Pixels) -> Result<Vec<Vec<f32>>, TemporalError> {
        let snap = common::open(&document());
        let records: StudioCompositionRecords = serde_json::from_value(records(mask)).expect("records shape");
        let id = composition_id();
        let input = CompositionInput {
            snapshot: &snap,
            expected_revision: 7,
            records: &records,
            composition_id: &id,
            budget: Budget::default(),
            resolver: &common::Available,
            blend_space: BlendSpace::Encoded,
            info: &Info(pixels),
            space: scenes::space(),
            inputs: hsk_studio_composite::RenderInputs::default(),
        };
        let scene = CompositionTemporalSource::new(&input, &CancellationToken::default())?.scene_at((frame * TPF) as i64, &CancellationToken::default())?;
        let pieces: Vec<Vec<f32>> = scene
            .layers
            .iter()
            .filter(|l| l.layer_id == identity("SLYR", 11))
            .map(|l| {
                let SceneContent::Pinned(pin) = &l.content else { panic!("pinned: {:?}", l.content) };
                let SceneContent::Lit(lit) = &pin.content else { panic!("lit: {:?}", pin.content) };
                lit.mask.clone().expect("layer-space mask")
            })
            .collect();
        assert!(!pieces.is_empty(), "the plane renders");
        Ok(pieces)
    }
    fn mask(streams: Value) -> Value {
        let mut m = json!({"mask_id":"m1","mode":"add","inverted":true,"closed":true,"static_value":rect_path(4.0, 2.0, 14.0, 10.0)});
        for (k, v) in streams.as_object().cloned().unwrap_or_default() {
            m[k] = v;
        }
        m
    }
    /// `mask_coverage_shaped` over the plane's 20 x 14 content rect in layer space.
    fn oracle(shape: MaskShape) -> Vec<f32> {
        let at = MaskAtTick { mode: M::Add, inverted: true, closed: true, path: rect_path(4.0, 2.0, 14.0, 10.0) };
        mask_coverage_shaped(&[(at, shape)], &Affine2::IDENTITY, Rect { x: 0, y: 0, width: 20, height: 14 }, 1 << 20).unwrap().unwrap()
    }

    #[test]
    fn three_d_plane_masks_use_the_shaped_coverage() {
        common::bounded_case("three_d_plane_masks_use_the_shaped_coverage", || {
            let pixels = Pixels::fixture();
            let shape = MaskShape { feather_px: [2.0, 1.0], opacity: 0.5, expansion_px: 1.0 };
            let fixed = mask(json!({"feather":{"static_value":[2.0,1.0]},"opacity":{"static_value":[0.5]},"expansion":{"static_value":[1.0]}}));
            for m in plane_masks(fixed, 0, &pixels).unwrap() {
                assert_eq!(m, oracle(shape), "static shape on the 3D plane");
            }
            assert_ne!(oracle(shape), oracle(MaskShape::default()));
            // Animated: feather [0, 0] -> [4, 2] and opacity 0.25 -> 0.75 over frames 0..2.
            let key = |t: u64, v: Value| json!({"time":t,"value":v,"in_interp":"linear","out_interp":"linear"});
            let animated = mask(json!({
                "feather":{"static_value":[0.0,0.0],"keyframes":[key(0, json!([0.0,0.0])), key(TPF * 2, json!([4.0,2.0]))]},
                "opacity":{"static_value":[0.25],"keyframes":[key(0, json!([0.25])), key(TPF * 2, json!([0.75]))]}}));
            for (frame, feather, opacity) in [(0u64, [0.0, 0.0], 0.25), (1, [2.0, 1.0], 0.5), (2, [4.0, 2.0], 0.75)] {
                let want = oracle(MaskShape { feather_px: feather, opacity, expansion_px: 0.0 });
                for m in plane_masks(animated.clone(), frame, &pixels).unwrap() {
                    assert_eq!(m, want, "frame {frame}");
                }
            }
            // A feather beyond pigment's limit is refused typed, naming the mask.
            let wide = mask(json!({"feather":{"static_value":[0.0, 20000.0]}}));
            match plane_masks(wide, 0, &pixels) {
                Err(TemporalError::Unsupported { layer_id, feature }) => {
                    assert_eq!((layer_id, feature), (format!("{}/masks/m1", identity("SLYR", 11)), COMPOSITION_MASK_FEATHER_OUT_OF_RANGE));
                }
                other => panic!("{other:?}"),
            }
        });
    }
}
