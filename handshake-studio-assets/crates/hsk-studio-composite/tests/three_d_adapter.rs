//! Lane SMB-AE-3D: a stored Folio composition with 3D layers, a two-node camera and a light,
//! evaluated by `CompositionTemporalSource`. A 2D layer between two 3D layers breaks the run
//! (STU-CMP-035a): each side expands on its own (`three_d::expand_run`, the same camera), and the
//! break is surfaced. Every planar-renderer refusal is typed. Pixel proofs against the ray-cast
//! oracle live in render-cpu / render-gpu `tests/composition_three_d.rs`.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::three_d::{self, Plane3d, RunBreak, Transform3d};
use hsk_studio_composite::{BlendSpace, SceneContent, TemporalError, TileInfo, TileInfoSource};
use hsk_studio_folio::{Budget, StudioCompositionRecords, TileRef};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{AlphaConvention, SampleFormat};
use serde_json::{Value, json};

mod common;
#[path = "support/composition_scenes.rs"]
mod scenes;
use common::{Available, open};
use scenes::{Pixels, composition_id, identity, tile_id};

fn tile(layer: u8, key: &str, w: u32, h: u32) -> Value {
    json!({"object_key":key,"layer_id":identity("SLYR",layer),"column":0,"row":0,
        "width":{"value":w,"unit":"px"},"height":{"value":h,"unit":"px"},"format":"caller-format",
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"00"},"artifact_manifest_id":"manifest-1"})
}
fn raster(n: u8, tiles: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),
        "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":tiles},"visible":true})
}
fn group(n: u8) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("null-{n}"),"parent":null,
        "kind":"group","payload":{"payload_kind":"group"},"visible":true})
}
fn document() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[group(6), group(8), group(9), raster(3, vec![tile(3,"tb",6,6)]), raster(10, vec![tile(10,"te",10,8)]),
            raster(11, vec![tile(11,"tf",20,14)])],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    common::assign_all_profiles(&mut v);
    v
}
fn timing() -> Value {
    json!({"in_point":0,"out_point":10_584_000_000u64 * 48,"start_time":0,"stretch":{"numerator":1,"denominator":1}})
}
fn s(v: f64) -> Value {
    json!({"static_value":[v]})
}
/// Topmost first: camera, light (from frame 10), plane "tf", 2D "tb", plane "te".
fn layers() -> Vec<Value> {
    vec![
        json!({"layer_id":identity("SLYR",6),"timing":timing(),"streams":[{"property":"position","static_value":[10.0,4.0]}],
            "three_d":{"position_z":s(-60.0)},
            "camera":{"zoom":s(50.0),"point_of_interest":{"static_value":[16.0,12.0,0.0]}}}),
        json!({"layer_id":identity("SLYR",8),"timing":{"in_point":10_584_000_000u64 * 10,"out_point":10_584_000_000u64 * 48,"start_time":0,"stretch":{"numerator":1,"denominator":1}},"three_d":{},
            "light":{"light_type":"point","colour":{"static_value":[1.0,1.0,1.0]},"intensity":s(120.0)}}),
        json!({"layer_id":identity("SLYR",11),"timing":timing(),"streams":[{"property":"position","static_value":[16.0,12.0]}],
            "three_d":{"rotation_y":s(35.0)}}),
        json!({"layer_id":identity("SLYR",3),"timing":timing(),"streams":[{"property":"position","static_value":[16.0,12.0]}]}),
        json!({"layer_id":identity("SLYR",10),"timing":timing(),"streams":[
                {"property":"position","static_value":[15.0,13.0]},{"property":"scale","static_value":[140.0,140.0]}],
            "three_d":{"position_z":s(1.0),"rotation_x":s(-40.0)}}),
    ]
}
fn records(layers: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[{
        "schema_id":"hsk.studio.composition@1","composition_id":composition_id(),"name":"comp",
        "width":32,"height":24,"pixel_aspect":{"numerator":1,"denominator":1},
        "frame_rate":{"numerator":24,"denominator":1},"duration":10_584_000_000u64 * 48,
        "background":{"r":0.1,"g":0.2,"b":0.3,"a":1.0,"colour_profile_id":null},"layers":layers}]})
}
struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        let t = self.0.get(&format!("{}/{}", tile.layer_id, tile.object_key))?;
        Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, colour_sha256: t.colour_sha256, alpha_sha256: t.alpha_sha256 })
    }
}
fn source(rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(&document());
    let records: StudioCompositionRecords = serde_json::from_value(rec.clone()).expect("records shape");
    let id = composition_id();
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &records,
        composition_id: &id,
        budget: Budget::default(),
        resolver: &Available,
        blend_space: BlendSpace::Encoded,
        info: &Info(pixels),
        space: scenes::space(),
        inputs: hsk_studio_composite::RenderInputs::default(),
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}
fn refused(e: TemporalError, feature: &str) {
    assert!(matches!(&e, TemporalError::Unsupported { feature: f, .. } if *f == feature), "{feature}: {e:?}");
}

#[test]
fn three_d_runs_camera_breaks_and_refusals() {
    let pixels = Pixels::fixture();
    let src = source(&records(layers()), &pixels).expect("source");
    let scene = src.scene_at(0, &CancellationToken::default()).expect("scene");
    // The expected runs, built from the same records by hand.
    let cam = three_d::camera([10.0, 4.0, -60.0], [0.0; 3], 0.0, 0.0, 0.0, Some([16.0, 12.0, 0.0]), 50.0, 32, 24).unwrap();
    let plane = |n: u8, key: &str, w: u32, h: u32, position: [f64; 3], scale: f64, rx: f64, ry: f64| {
        let tile = pixels.get(&tile_id(n, key)).unwrap().scene_tile(0, 0);
        Plane3d {
            layer_id: identity("SLYR", n),
            bounds: tile.placement,
            origin: [0.0, 0.0],
            content: SceneContent::Tiles(vec![tile]),
            transform: Transform3d {
                anchor: [f64::from(w) / 2.0, f64::from(h) / 2.0, 0.0],
                position,
                scale: [scale, scale],
                rotation: three_d::layer_rotation([0.0; 3], rx, ry, 0.0),
                parent: three_d::Affine3::IDENTITY,
            },
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
        }
    };
    let below = three_d::expand_run(&[plane(10, "te", 10, 8, [15.0, 13.0, 1.0], 1.4, -40.0, 0.0)], &cam, 32, 24, scenes::space(), 7).unwrap();
    let above = three_d::expand_run(&[plane(11, "tf", 20, 14, [16.0, 12.0, 0.0], 1.0, 0.0, 35.0)], &cam, 32, 24, scenes::space(), 7).unwrap();
    let n = below.len();
    assert_eq!(scene.layers.len(), n + 1 + above.len(), "{:?}", scene.layers.iter().map(|l| &l.layer_id).collect::<Vec<_>>());
    assert_eq!(&scene.layers[..n], &below[..], "the run below the 2D layer");
    assert_eq!(scene.layers[n].layer_id, identity("SLYR", 3), "the 2D layer breaks the run; camera and light draw nothing");
    assert_eq!(&scene.layers[n + 1..], &above[..], "the run above it");
    assert_eq!(src.three_d_run_breaks().unwrap(), vec![RunBreak { layer_id: identity("SLYR", 3), reason: three_d::RUN_BREAK_2D_LAYER }]);
    // Typed refusals.
    let edit = |f: &dyn Fn(&mut Vec<Value>)| {
        let mut l = layers();
        f(&mut l);
        records(l)
    };
    let at_open = |f: &dyn Fn(&mut Vec<Value>), feature: &str| refused(source(&edit(f), &pixels).map(|_| ()).unwrap_err(), feature);
    let at_tick = |f: &dyn Fn(&mut Vec<Value>), feature: &str| {
        refused(source(&edit(f), &pixels).unwrap().scene_at(0, &CancellationToken::default()).unwrap_err(), feature)
    };
    // Lane SMB-AE-3D-2: 2D and 3D layers parent each other; a camera or light parent is refused.
    at_open(&|l| l[2]["parent"] = json!(identity("SLYR", 6)), three_d::COMPOSITION_3D_PARENT);
    at_open(&|l| l[3]["parent"] = json!(identity("SLYR", 8)), three_d::COMPOSITION_3D_PARENT);
    at_open(&|l| l[2]["track_matte"] = json!({"source_layer_id":identity("SLYR",3),"mode":"alpha","inverted":false}), three_d::COMPOSITION_3D_LAYER_OPTION);
    at_tick(
        &|l| {
            l[0]["streams"] = json!([{"property":"position","static_value":[16.0,-100.0]}]);
            l[0]["three_d"] = json!({"position_z":s(0.0)});
        },
        three_d::COMPOSITION_3D_CAMERA_DEGENERATE,
    );
    at_tick(&|l| l[2]["three_d"] = json!({"position_z":s(-60.0),"rotation_y":s(90.0)}), three_d::COMPOSITION_3D_CROSSES_CAMERA);
}

#[test]
fn lights_materials_and_shadow_diagnostics_from_records() {
    // Lane SMB-AE-3D slice 2: no 2D layer between the planes (one run), a shadow-casting point light
    // and an ambient light; "tf" casts, "te" accepts.
    let pixels = Pixels::fixture();
    let mut l = layers();
    l.remove(3);
    l[1] = json!({"layer_id":identity("SLYR",8),"timing":timing(),"streams":[{"property":"position","static_value":[14.0,6.0]}],
        "three_d":{"position_z":s(-30.0)},
        "light":{"light_type":"point","colour":{"static_value":[1.0,0.5,0.25]},"intensity":s(120.0),"falloff_type":"smooth",
            "casts_shadows":true,"point_of_interest":{"static_value":[16.0,12.0,0.0]},
            "values":{"falloff_start":s(20.0),"falloff_distance":s(80.0),"shadow_darkness":s(80.0)}}});
    l.push(json!({"layer_id":identity("SLYR",3),"timing":timing(),"three_d":{},
        "light":{"light_type":"ambient","colour":{"static_value":[1.0,1.0,1.0]},"intensity":s(-25.0)}}));
    l[1]["three_d"]["material"] = json!({});
    l[2]["three_d"]["material"] = json!({"casts_shadows":true,"values":{"diffuse":s(80.0),"specular_shininess":s(20.0)}});
    l[3]["three_d"]["material"] = json!({"accepts_shadows":true,"values":{"light_transmission":s(25.0)}});
    let src = source(&records(l), &pixels).expect("source");
    let scene = src.scene_at(0, &CancellationToken::default()).expect("scene");
    let lit = |id: u8| -> Vec<&three_d::LitContent> {
        scene
            .layers
            .iter()
            .filter(|x| x.layer_id == identity("SLYR", id))
            .map(|x| match &x.content {
                SceneContent::Pinned(p) => match &p.content {
                    SceneContent::Lit(lit) => &**lit,
                    other => panic!("lit content: {other:?}"),
                },
                other => panic!("pinned: {other:?}"),
            })
            .collect()
    };
    let (tf, te) = (lit(11), lit(10));
    assert!(!tf.is_empty() && !te.is_empty());
    let point = &te[0].lights[0];
    assert_eq!((point.kind, point.position, point.intensity), (three_d::LightKind::Point, [14.0, 6.0, -30.0], 1.2));
    assert_eq!((point.falloff, point.falloff_start, point.falloff_distance, point.shadow_darkness), (three_d::Falloff::Smooth, 20.0, 80.0, 0.8));
    let dir = [2.0 / 940f64.sqrt(), 6.0 / 940f64.sqrt(), 30.0 / 940f64.sqrt()];
    assert!((0..3).all(|k| (point.direction[k] - dir[k]).abs() < 1e-12), "{:?}", point.direction);
    assert_eq!((te[0].lights[1].kind, te[0].lights[1].intensity), (three_d::LightKind::Ambient, -0.25), "intensity is never clamped");
    assert_eq!(te[0].casters.iter().map(|c| c.layer_id.clone()).collect::<Vec<_>>(), vec![identity("SLYR", 11)], "te receives tf's shadow");
    assert_eq!(te[0].casters[0].light_transmission, 0.0);
    assert!(tf[0].casters.is_empty(), "tf does not accept shadows");
    assert_eq!((tf[0].material.diffuse, tf[0].material.specular_shininess, tf[0].material.ambient), (0.8, 20.0, 1.0));
    assert_eq!(te[0].material.light_transmission, 0.25);
    // STU-CMP-045a: the triples with some but not all switches on, naming the switches that are off.
    let diag = src.three_d_shadow_diagnostics().unwrap();
    let off = |light: u8, caster: u8, receiver: u8| {
        diag.iter()
            .find(|d| d.light == identity("SLYR", light) && d.caster == identity("SLYR", caster) && d.receiver == identity("SLYR", receiver))
            .map(|d| d.off.clone())
    };
    assert_eq!(off(8, 11, 10), None, "all three on: the shadow is cast, no diagnostic");
    assert_eq!(off(8, 10, 11), Some(vec![three_d::SHADOW_CASTER_OFF, three_d::SHADOW_RECEIVER_OFF]));
    assert_eq!(off(3, 11, 10), Some(vec![three_d::SHADOW_LIGHT_OFF]));
    assert_eq!(off(3, 10, 11), None, "all three off: the default, no diagnostic");
    assert_eq!(diag.len(), 2);
    // Slice 3: the camera's depth of field reaches every plane (thin lens, members from records).
    let mut l = layers();
    l[0]["camera"]["depth_of_field"] = json!(true);
    l[0]["camera"]["values"] = json!({"focus_distance":s(61.0),"aperture":s(30.0),"blur_level":s(150.0)});
    let dof = source(&records(l), &pixels).unwrap().scene_at(0, &CancellationToken::default()).unwrap();
    let lens = dof
        .layers
        .iter()
        .filter_map(|x| match &x.content {
            SceneContent::Pinned(p) => match &p.content {
                SceneContent::Lit(lit) => Some((lit.lights.len(), lit.dof.map(|d| (d.1, d.2)))),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let want = three_d::DepthOfField { focus_distance: 61.0, aperture: 30.0, blur_level: 1.5 };
    assert!(!lens.is_empty() && lens.iter().all(|x| *x == (0, Some((50.0, want)))), "{lens:?}");
    // Hostile defocus is refused before any output is allocated.
    assert!(three_d::defocus(&[0.0; 3], &[1.0], 1, 1, &[three_d::MAX_DOF_RADIUS + 1.0]).is_err());
    assert!(three_d::defocus(&[0.0; 3], &[1.0], 1, 1, &[f64::NAN]).is_err());
    assert!(three_d::defocus(&[0.0; 3], &[1.0], 1, 2, &[0.0]).is_err(), "shape mismatch");
    // A light outside its window lights nothing: the planes stay unlit.
    let plain = source(&records(layers()), &pixels).unwrap().scene_at(0, &CancellationToken::default()).unwrap();
    assert!(plain.layers.iter().all(|x| !matches!(&x.content, SceneContent::Pinned(p) if matches!(p.content, SceneContent::Lit(_)))));
}

type V3 = [f64; 3];
/// Independent rotations (f64 sin/cos; y down, z away): X, then Y, then Z.
fn turn_xyz(v: V3, rx: f64, ry: f64, rz: f64) -> V3 {
    let (s, c) = rx.to_radians().sin_cos();
    let v = [v[0], c * v[1] - s * v[2], s * v[1] + c * v[2]];
    let (s, c) = ry.to_radians().sin_cos();
    let v = [c * v[0] + s * v[2], v[1], -s * v[0] + c * v[2]];
    let (s, c) = rz.to_radians().sin_cos();
    [c * v[0] - s * v[1], s * v[0] + c * v[1], v[2]]
}
fn add3(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub3(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot3(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn unit3(a: V3) -> V3 {
    let l = dot3(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}
/// Pinhole projection of `p` by a two-node camera at `eye` looking at `poi` (zoom 50, 32 x 24).
fn project(eye: V3, poi: V3, p: V3) -> [f64; 2] {
    let f = unit3(sub3(poi, eye));
    let d = unit3(sub3([0.0, 1.0, 0.0], [f[0] * f[1], f[1] * f[1], f[2] * f[1]]));
    let r = [d[1] * f[2] - d[2] * f[1], d[2] * f[0] - d[0] * f[2], d[0] * f[1] - d[1] * f[0]];
    let q = sub3(p, eye);
    [16.0 + 50.0 * dot3(q, r) / dot3(q, f), 12.0 + 50.0 * dot3(q, d) / dot3(q, f)]
}

#[test]
fn three_d_parent_chains_from_records() {
    // Lane SMB-AE-3D-2 (STU-MOT-008): plane "tf" under a 3D null (turned about X, Y and Z, scaled
    // 115 % x 90 %, so the child plane is sheared), the 2D layer "tb" under the same null (it takes
    // the null's 2D part only), then the camera under the null as well. Every expectation is the
    // test's own geometry (f64 sin/cos), not `three_d`'s.
    let pixels = Pixels::fixture();
    let null = |scale: [f64; 2]| {
        json!({"layer_id":identity("SLYR",9),"timing":timing(),"streams":[
                {"property":"position","static_value":[16.0,12.0]},{"property":"rotation","static_value":[6.0]},
                {"property":"scale","static_value":[scale[0],scale[1]]}],
            "three_d":{"position_z":s(4.0),"rotation_x":s(12.0),"rotation_y":s(-15.0)}})
    };
    let with = |camera_parented: bool, scale: [f64; 2]| {
        let mut l = layers();
        l[2]["parent"] = json!(identity("SLYR", 9));
        l[3]["parent"] = json!(identity("SLYR", 9));
        if camera_parented {
            l[0]["parent"] = json!(identity("SLYR", 9));
        }
        l.push(null(scale));
        records(l)
    };
    // The null maps its layer space (anchor 0: a content-less layer) to world.
    let null_world = |p: V3| add3([16.0, 12.0, 4.0], turn_xyz([1.15 * p[0], 0.9 * p[1], p[2]], 12.0, -15.0, 6.0));
    // tf's own transform (anchor = content centre (10, 7), position (16, 12, 0), Y turn 35).
    let tf_world = |x: f64, y: f64| null_world(add3([16.0, 12.0, 0.0], turn_xyz([x - 10.0, y - 7.0, 0.0], 0.0, 35.0, 0.0)));
    let corners = [[0.0, 0.0], [20.0, 0.0], [20.0, 14.0], [0.0, 14.0], [7.25, 3.5]];
    let check_tf = |scene: &hsk_studio_composite::SceneComposition, eye: V3, poi: V3| {
        let pieces: Vec<_> = scene.layers.iter().filter(|x| x.layer_id == identity("SLYR", 11)).collect();
        assert!(!pieces.is_empty(), "tf is drawn");
        for piece in pieces {
            let SceneContent::Pinned(p) = &piece.content else { panic!("pinned: {:?}", piece.content) };
            for c in corners {
                let (x, y, w) = p.pin.apply_h(c[0], c[1]);
                let want = project(eye, poi, tf_world(c[0], c[1]));
                assert!((x / w - want[0]).abs() < 1e-9 && (y / w - want[1]).abs() < 1e-9, "{c:?}: {:?} vs {want:?}", (x / w, y / w));
            }
        }
    };
    let src = source(&with(false, [115.0, 90.0]), &pixels).expect("source");
    let scene = src.scene_at(0, &CancellationToken::default()).expect("scene");
    check_tf(&scene, [10.0, 4.0, -60.0], [16.0, 12.0, 0.0]);
    // The 2D child takes the null's 2D part: position (16, 12), Z turn 6, scale 115 % x 90 %.
    let tb = scene.layers.iter().find(|x| x.layer_id == identity("SLYR", 3)).expect("tb");
    for c in [[0.0, 0.0], [6.0, 0.0], [6.0, 6.0], [1.5, 4.0]] {
        let child = [16.0 + c[0] - 3.0, 12.0 + c[1] - 3.0, 0.0];
        let want = add3([16.0, 12.0, 0.0], turn_xyz([1.15 * child[0], 0.9 * child[1], 0.0], 0.0, 0.0, 6.0));
        let got = tb.matrix.apply(c[0], c[1]);
        assert!((got.0 - want[0]).abs() < 1e-9 && (got.1 - want[1]).abs() < 1e-9, "{c:?}: {got:?} vs {want:?}");
    }
    // A two-node camera under the null: its position and point of interest move with the chain.
    let moved = source(&with(true, [115.0, 90.0]), &pixels).unwrap().scene_at(0, &CancellationToken::default()).unwrap();
    check_tf(&moved, null_world([10.0, 4.0, -60.0]), null_world([16.0, 12.0, 0.0]));
    // Hostile: a zero parent scale is refused at the tick, before any plane is expanded.
    refused(
        source(&with(false, [0.0, 90.0]), &pixels).unwrap().scene_at(0, &CancellationToken::default()).unwrap_err(),
        three_d::COMPOSITION_3D_PARENT_DEGENERATE,
    );
    // The chain's inverse is exact enough for the shading and shadow lookups.
    let chain = three_d::Affine3::layer([1.0, 2.0, 3.0], [16.0, 12.0, 4.0], [1.15, 0.9], &three_d::layer_rotation([0.0; 3], 12.0, -15.0, 6.0));
    let back = chain.inverse().unwrap().apply(chain.apply([3.0, -7.0, 11.0]));
    assert!(back.iter().zip([3.0, -7.0, 11.0]).all(|(a, b)| (a - b).abs() < 1e-12), "{back:?}");
}
