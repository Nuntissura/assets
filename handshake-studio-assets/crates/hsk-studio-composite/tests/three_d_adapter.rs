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
    source_with(&document(), rec, pixels)
}
fn source_with(doc: &Value, rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(doc);
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
            layer_mask: None,
            track_matte: None,
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
    // Lane SMB-AE-3D-3: a corner pin renders on a 3D layer (see the corner-pin test below). A pin
    // whose convex quads send part of the content across the pin's line at infinity (a 4 px square
    // inside "tf" widened to 20 px at its bottom edge: the denominator reaches 0 at content row 10)
    // is refused typed at the tick; Folio already refuses a folded (bow-tie) quad at open.
    at_tick(
        &|l| l[2]["corner_pin"] = json!({"from":[[8.0,5.0],[12.0,5.0],[12.0,9.0],[8.0,9.0]],"static_value":[8.0,5.0,12.0,5.0,20.0,9.0,0.0,9.0]}),
        three_d::COMPOSITION_3D_CORNER_PIN,
    );
    assert!(matches!(
        source(&edit(&|l| l[2]["corner_pin"] = json!({"from":[[0.0,0.0],[20.0,0.0],[20.0,14.0],[0.0,14.0]],"static_value":[0.0,0.0,20.0,14.0,20.0,0.0,0.0,14.0]})), &pixels),
        Err(TemporalError::Document(_))
    ));
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

#[test]
fn three_d_masks_and_track_mattes_from_records() {
    // Lane SMB-AE-3D-2 (STU-CMP-010 steps 2 and 7): "tf" carries a layer-space mask (rasterised
    // over its content rectangle, before the projection); "te" an alpha track matte from the 2D
    // layer "tb", which therefore does not render and no longer breaks the run.
    let pixels = Pixels::fixture();
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
        json!([{"mask_id":"m1","mode":"add","closed":true,
            "static_value":[x0,y0,0.0,0.0,0.0,0.0, x1,y0,0.0,0.0,0.0,0.0, x1,y1,0.0,0.0,0.0,0.0, x0,y1,0.0,0.0,0.0,0.0]}])
    };
    let mut l = layers();
    l[2]["masks"] = rect(4.0, 2.0, 14.0, 10.0);
    l[4]["track_matte"] = json!({"source_layer_id":identity("SLYR",3),"mode":"alpha","inverted":true});
    let src = source(&records(l.clone()), &pixels).expect("source");
    let scene = src.scene_at(0, &CancellationToken::default()).expect("scene");
    assert!(scene.layers.iter().all(|x| x.layer_id != identity("SLYR", 3)), "the matte source does not render");
    assert!(src.three_d_run_breaks().unwrap().is_empty(), "a matte source does not break the run");
    let pieces = |id: u8| scene.layers.iter().filter(move |x| x.layer_id == identity("SLYR", id)).collect::<Vec<_>>();
    // tf: the mask is the plane's layer-space coverage (20 x 14 texels, ones on 4..14 x 2..10).
    let tf = pieces(11);
    assert!(!tf.is_empty());
    for p in &tf {
        let SceneContent::Pinned(pin) = &p.content else { panic!("pinned") };
        let SceneContent::Lit(lit) = &pin.content else { panic!("masked content goes through the layer-space path: {:?}", pin.content) };
        let mask = lit.mask.as_ref().expect("mask");
        assert_eq!(mask.len(), 20 * 14);
        for (k, c) in mask.iter().enumerate() {
            let (i, j) = (k % 20, k / 20);
            let want = if (4..14).contains(&i) && (2..10).contains(&j) { 1.0 } else { 0.0 };
            assert_eq!(*c, want, "texel ({i}, {j})");
        }
        assert!(p.matte.as_ref().is_some_and(|m| m.layer.as_ref().is_some_and(|r| r.matte.is_none())), "rank matte only");
    }
    // te: every depth piece's rank matte carries the track matte (coverage = rank x matte).
    let te = pieces(10);
    assert!(!te.is_empty());
    for p in &te {
        let rank = p.matte.as_ref().and_then(|m| m.layer.as_ref()).expect("rank matte");
        let track = rank.matte.as_ref().expect("track matte under the rank matte");
        assert!(track.inverted && track.mode == hsk_studio_composite::SceneMatteMode::Alpha);
        assert_eq!(track.layer.as_ref().map(|x| x.layer_id.clone()), Some(identity("SLYR", 3)));
    }
    // A 3D matte source is projected through the camera (alone, unlit) and does not render.
    let mut m3 = l.clone();
    m3[4]["track_matte"] = json!({"source_layer_id":identity("SLYR",11),"mode":"luma","inverted":false});
    let s3 = source(&records(m3), &pixels).unwrap().scene_at(0, &CancellationToken::default()).unwrap();
    assert!(s3.layers.iter().all(|x| x.layer_id != identity("SLYR", 11)));
    let te3 = s3.layers.iter().find(|x| x.layer_id == identity("SLYR", 10)).expect("te");
    let track = te3.matte.as_ref().and_then(|m| m.layer.as_ref()).and_then(|r| r.matte.as_ref()).expect("track matte");
    assert!(matches!(track.layer.as_deref().map(|x| &x.content), Some(SceneContent::Pinned(_))), "projected 3D matte source");
    // Hostile: an empty mask path is refused (never drawn unmasked).
    let mut bad = layers();
    bad[2]["masks"] = json!([{"mask_id":"m1","mode":"add","closed":true,"static_value":[]}]);
    let e = source(&records(bad), &pixels).and_then(|s| s.scene_at(0, &CancellationToken::default()).map(|_| ()));
    assert!(e.is_err(), "an empty mask path is refused: {e:?}");
}

#[test]
fn three_d_planes_and_object_ids_from_records() {
    // Lane SMB-AE-3D-2 (STU-CMP-050): the planes the auxiliary channels are taken from: every 3D
    // content layer across the run break, bottom first, object id = 1-based stack index (camera 1,
    // light 2, tf 3, tb 4, te 5); cameras, lights and 2D layers are not planes.
    let pixels = Pixels::fixture();
    let src = source(&records(layers()), &pixels).expect("source");
    let (planes, ids, cam) = src.three_d_planes_at(0, &CancellationToken::default()).expect("planes");
    assert_eq!(planes.iter().map(|p| p.layer_id.clone()).collect::<Vec<_>>(), vec![identity("SLYR", 10), identity("SLYR", 11)]);
    assert_eq!(ids, vec![5, 3]);
    assert_eq!(cam, three_d::camera([10.0, 4.0, -60.0], [0.0; 3], 0.0, 0.0, 0.0, Some([16.0, 12.0, 0.0]), 50.0, 32, 24).unwrap());
    // Synthetic opaque content: the channels are exact against an independent per-pixel ray cast
    // in render-cpu `composition_three_d`; here the shape and refusals.
    let alphas: Vec<Vec<f32>> = planes.iter().map(|p| vec![1.0; (p.bounds.width * p.bounds.height) as usize]).collect();
    let aux = three_d::aux_channels(&planes, &alphas, &ids, &cam, 32, 24).expect("channels");
    assert_eq!(aux.ids, vec![(3, identity("SLYR", 11)), (5, identity("SLYR", 10))]);
    let names: Vec<&str> = aux.channels().iter().map(|c| c.0).collect();
    assert_eq!(names, three_d::AUX_CHANNEL_NAMES.to_vec(), "channel identifier");
    let covered = aux.coverage.iter().filter(|c| **c == 1.0).count();
    assert!(covered > 100 && aux.object_id.contains(&3.0) && aux.object_id.contains(&5.0), "{covered}");
    for i in 0..aux.depth.len() {
        let hit = aux.coverage[i] == 1.0;
        assert_eq!(hit, aux.depth[i].is_finite(), "Z is +inf exactly where nothing is hit");
        let n = (0..3).map(|k| f64::from(aux.normal[k][i]).powi(2)).sum::<f64>();
        assert!(if hit { (n - 1.0).abs() < 1e-6 } else { n == 0.0 }, "unit normals on hits: {n}");
    }
    // Hostile inputs are refused before any work.
    assert!(three_d::aux_channels(&planes, &alphas[..1], &ids, &cam, 32, 24).is_err(), "alpha planes per plane");
    assert!(three_d::aux_channels(&planes, &alphas, &[0, 3], &cam, 32, 24).is_err(), "object id 0 is the background");
    assert!(matches!(
        three_d::aux_channels(&planes, &alphas, &ids, &cam, 30_000, 30_000),
        Err(TemporalError::Composite(hsk_studio_composite::CompositeError::BudgetExceeded))
    ));
}

#[test]
fn immersive_scene_and_capability_refusals_from_records() {
    // Lane SMB-AE-3D-2 (STU-CMP-055 / 055a): an equirectangular composition of the two planes and
    // the camera; every refusal is typed; the 18 unbuilt capabilities are refused by name.
    let pixels = Pixels::fixture();
    let settings = json!({"projection_type":1,"stereoscopic_type":0,"stereoscopic_eye":0,
        "captured_horizontal_view":360.0,"captured_vertical_view":180.0,
        "field_of_horizontal_view":108.0,"field_of_vertical_view":108.0,"ambisonics_monitoring_type":0});
    let with = |l: Vec<Value>, s: Value| {
        let mut r = records(l);
        r["compositions"][0]["immersive"] = s;
        r
    };
    let mut l = layers();
    l.remove(3); // the 2D layer "tb"
    let src = source(&with(l.clone(), settings.clone()), &pixels).expect("source");
    let scene = src.immersive_scene_at(0, &CancellationToken::default()).expect("immersive scene");
    assert_eq!((scene.width, scene.height, scene.background), (32, 24, Some([0.1, 0.2, 0.3, 1.0])));
    assert_eq!(scene.planes.iter().map(|p| p.layer_id.clone()).collect::<Vec<_>>(), vec![identity("SLYR", 10), identity("SLYR", 11)]);
    assert_eq!(scene.materials.len(), 2);
    assert!(scene.lights.is_empty(), "the light starts at frame 10");
    let parameters = three_d::immersive::IMMERSIVE_PARAMETERS;
    let layer = three_d::immersive::IMMERSIVE_LAYER;
    let at = |r: Value, feature: &str| refused(source(&r, &pixels).unwrap().immersive_scene_at(0, &CancellationToken::default()).unwrap_err(), feature);
    at(records(l.clone()), parameters);
    for (field, v) in [("projection_type", json!(0)), ("stereoscopic_type", json!(2)), ("captured_horizontal_view", json!(180.0))] {
        let mut s = settings.clone();
        s[field] = v;
        at(with(l.clone(), s), parameters);
    }
    at(with(layers(), settings.clone()), layer);
    let mut m = l.clone();
    m[3]["track_matte"] = json!({"source_layer_id":identity("SLYR",11),"mode":"alpha","inverted":false});
    at(with(m, settings.clone()), layer);
    let mut dof = l.clone();
    dof[0]["camera"]["depth_of_field"] = json!(true);
    at(with(dof, settings), layer);
    // The capability surface: two built, eighteen refused by name, unknown names refused apart.
    let built: Vec<&str> = three_d::immersive::IMMERSIVE_CAPABILITIES.iter().copied().filter(|c| three_d::immersive::immersive_capability(c).is_ok()).collect();
    assert_eq!(built, vec!["plane_to_sphere", "sphere_rotate"]);
    for c in three_d::immersive::IMMERSIVE_CAPABILITIES.iter().filter(|c| !built.contains(c)) {
        refused(three_d::immersive::immersive_capability(c).unwrap_err(), three_d::immersive::IMMERSIVE_UNSUPPORTED);
    }
    refused(three_d::immersive::immersive_capability("immersive_teleport").unwrap_err(), three_d::immersive::IMMERSIVE_UNKNOWN);
    // Hostile sphere rotations are refused before allocation.
    assert!(three_d::immersive::sphere_rotate(&[0.0; 3], &[1.0], 1, 1, f64::NAN, 0.0, 0.0).is_err());
    assert!(three_d::immersive::sphere_rotate(&[0.0; 3], &[1.0], 1, 2, 0.0, 0.0, 0.0).is_err(), "shape mismatch");
    assert!(three_d::immersive::sphere_rotate(&[], &[], 0, 0, 0.0, 0.0, 0.0).is_err());
}

#[test]
fn three_d_corner_pin_clipping_and_collapse_from_records() {
    // Lane SMB-AE-3D-3 (Studio readings, After Effects UNVERIFIED, D5). Pixel proofs against the
    // ray-cast oracle: render-cpu / render-gpu `composition_three_d` (pinned and clipped run).
    use hsk_studio_pigment::transform_math::{Affine2, Projective2};
    let pixels = Pixels::fixture();
    let cancel = CancellationToken::default();
    let cam = three_d::camera([10.0, 4.0, -60.0], [0.0; 3], 0.0, 0.0, 0.0, Some([16.0, 12.0, 0.0]), 50.0, 32, 24).unwrap();
    // (1) Corner pin on "tf" (STU-CMP-010 step 3 before step 5): the run equals the hand-built run
    // whose tf plane textures the pinned content over the pinned rectangle.
    let from = [[0.0, 0.0], [20.0, 0.0], [20.0, 14.0], [0.0, 14.0]];
    let to = [[1.0, -2.0], [19.0, 1.0], [22.0, 15.0], [-1.0, 12.0]];
    let mut l = layers();
    l[2]["corner_pin"] = json!({"from":from,"static_value":[1.0,-2.0,19.0,1.0,22.0,15.0,-1.0,12.0]});
    l.remove(3);
    let scene = source(&records(l), &pixels).unwrap().scene_at(0, &cancel).unwrap();
    let tile = |n: u8, key: &str| pixels.get(&tile_id(n, key)).unwrap().scene_tile(0, 0);
    let plane = |n: u8, key: &str, w: f64, h: f64, position: [f64; 3], scale: f64, rx: f64, ry: f64| Plane3d {
        layer_id: identity("SLYR", n),
        bounds: tile(n, key).placement,
        origin: [0.0, 0.0],
        content: SceneContent::Tiles(vec![tile(n, key)]),
        transform: Transform3d { anchor: [w / 2.0, h / 2.0, 0.0], position, scale: [scale, scale], rotation: three_d::layer_rotation([0.0; 3], rx, ry, 0.0), parent: three_d::Affine3::IDENTITY },
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        layer_mask: None,
        track_matte: None,
    };
    let te = plane(10, "te", 10.0, 8.0, [15.0, 13.0, 1.0], 1.4, -40.0, 0.0);
    let mut tf = plane(11, "tf", 20.0, 14.0, [16.0, 12.0, 0.0], 1.0, 0.0, 35.0);
    let id = Projective2::from_affine(&Affine2::translate(0.0, 0.0));
    let cp = id.then(&Projective2::from_quads(from, to).unwrap()).then(&id);
    let (cp, pinned) = three_d::pinned_bounds(&cp, tf.bounds).unwrap();
    assert_eq!((pinned.x, pinned.y, pinned.width, pinned.height), (-1, -2, 23, 17), "the pinned rectangle covers the pinned quad");
    tf.content = SceneContent::Pinned(Box::new(hsk_studio_composite::composition::PinnedContent { content: tf.content.clone(), pin: cp }));
    tf.bounds = pinned;
    let want = three_d::expand_run(&[te.clone(), tf.clone()], &cam, 32, 24, scenes::space(), 7).unwrap();
    assert_eq!(scene.layers, want, "pinned run");
    for p in scene.layers.iter().filter(|p| p.layer_id == identity("SLYR", 11)) {
        let SceneContent::Pinned(q) = &p.content else { panic!("pinned") };
        assert!(matches!(q.content, SceneContent::Tiles(_)), "one resample: the corner pin then the plane homography");
    }
    // Hostile pins: collinear corners (refused by Folio at open) and a pin beyond the extent bound
    // (refused typed at the tick).
    let pinned_with = |v: Value| {
        let mut l = layers();
        l[2]["corner_pin"] = json!({"from":from,"static_value":v});
        source(&records(l), &pixels).and_then(|s| s.scene_at(0, &cancel).map(|_| ()))
    };
    assert!(matches!(pinned_with(json!([0.0, 0.0, 10.0, 0.0, 20.0, 0.0, 0.0, 14.0])), Err(TemporalError::Document(_))), "collinear");
    refused(pinned_with(json!([0.0, 0.0, 1.0e7, 0.0, 1.0e7, 14.0, 0.0, 14.0])).unwrap_err(), three_d::COMPOSITION_3D_CORNER_PIN);
    // (2) Clipping: "tb" (2D) and "tf" (3D) clipped to their base "te" (3D, the bottom layer).
    let mut doc = document();
    for k in [3, 5] {
        doc["layers"][k]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":true,"isolation":"isolated"});
    }
    let clipped = source_with(&doc, &records(layers()), &pixels).unwrap().scene_at(0, &cancel).unwrap();
    let base_alone = three_d::expand_run(std::slice::from_ref(&te), &cam, 32, 24, scenes::space(), 7).unwrap().into_iter().next().unwrap();
    let tb = clipped.layers.iter().find(|x| x.layer_id == identity("SLYR", 3)).expect("tb");
    assert!(!tb.clipped, "the 2D layer clipped to a 3D base takes the base alpha as a matte");
    let m = tb.matte.as_ref().expect("clip matte");
    assert!(m.mode == hsk_studio_composite::SceneMatteMode::Alpha && !m.inverted);
    assert_eq!(m.layer.as_deref(), Some(&base_alone), "the base projected alone through the camera");
    let tf_pieces: Vec<_> = clipped.layers.iter().filter(|x| x.layer_id == identity("SLYR", 11)).collect();
    assert!(!tf_pieces.is_empty());
    for p in tf_pieces {
        let rank = p.matte.as_ref().and_then(|m| m.layer.as_ref()).expect("rank matte");
        assert_eq!(rank.matte.as_ref().and_then(|c| c.layer.as_deref()), Some(&base_alone), "clip matte under the rank matte");
    }
    // The bottom layer has no base; a clipped layer with a track matte is refused as on 2D layers.
    let mut lone = document();
    lone["layers"][4]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":true,"isolation":"isolated"});
    refused(source_with(&lone, &records(layers()), &pixels).unwrap().scene_at(0, &cancel).unwrap_err(), hsk_studio_composite::composition::CLIP_WITHOUT_BASE);
    let mut matted = layers();
    matted[2]["track_matte"] = json!({"source_layer_id":identity("SLYR",3),"mode":"alpha","inverted":false});
    refused(source_with(&doc, &records(matted), &pixels).unwrap().scene_at(0, &cancel).unwrap_err(), hsk_studio_composite::composition::CLIPPED_MATTE);
    // (3) Collapsed 3D precomposition (STU-CMP-016): layer 9 nests "te" (3D) and "tb" (2D); both
    // join the outer run with tf, each plane's chain followed by the precomposition layer's.
    let mut root = layers();
    root.truncate(3);
    root.remove(1);
    root.push(json!({"layer_id":identity("SLYR",9),"timing":timing(),"streams":[{"property":"position","static_value":[14.0,11.0]}],
        "three_d":{"position_z":s(3.0),"rotation_y":s(-25.0)},
        "precomp":{"composition_id":identity("SCOM",2),"collapse_transformations":true}}));
    let nested_layers = vec![
        json!({"layer_id":identity("SLYR",3),"timing":timing(),"streams":[{"property":"position","static_value":[20.0,9.0]}]}),
        json!({"layer_id":identity("SLYR",10),"timing":timing(),"streams":[{"property":"position","static_value":[15.0,13.0]},{"property":"scale","static_value":[140.0,140.0]}],
            "three_d":{"position_z":s(1.0),"rotation_x":s(-40.0)}}),
    ];
    let with_nested = |root: Vec<Value>, nested: Vec<Value>| {
        let mut r = records(root);
        let mut c = r["compositions"][0].clone();
        c["composition_id"] = json!(identity("SCOM", 2));
        c["name"] = json!("nested");
        c["background"] = Value::Null;
        c["layers"] = json!(nested);
        r["compositions"].as_array_mut().unwrap().push(c);
        r
    };
    let src = source(&with_nested(root.clone(), nested_layers.clone()), &pixels).expect("source");
    let scene = src.scene_at(0, &cancel).unwrap();
    assert!(scene.layers.iter().all(|x| x.layer_id != identity("SLYR", 9)), "the collapsed layer has no surface of its own");
    assert!(scene.layers.iter().any(|x| x.layer_id == identity("SLYR", 3)), "the nested 2D layer is a plane of the run");
    let (planes, ids, _) = src.three_d_planes_at(0, &cancel).unwrap();
    assert_eq!(planes.iter().map(|p| p.layer_id.clone()).collect::<Vec<_>>(), vec![identity("SLYR", 10), identity("SLYR", 3), identity("SLYR", 11)]);
    assert_eq!(ids, vec![3, 3, 2], "nested planes carry the precomposition layer's object id");
    // te's world point = the precomposition layer's map (anchor = nested centre 16, 12) after te's own.
    let outer = three_d::Affine3::layer([16.0, 12.0, 0.0], [14.0, 11.0, 3.0], [1.0, 1.0], &three_d::layer_rotation([0.0; 3], 0.0, -25.0, 0.0));
    for (x, y) in [(0.0, 0.0), (10.0, 8.0), (3.5, 6.0)] {
        let got = planes[0].transform.world(x, y);
        let want = outer.apply(te.transform.world(x, y));
        assert!((0..3).all(|k| (got[k] - want[k]).abs() < 1e-9), "{got:?} vs {want:?}");
    }
    assert!(src.three_d_run_breaks().unwrap().is_empty(), "nothing breaks the run");
    // A light inside a collapsed 3D precomposition is refused typed.
    let mut lit = nested_layers;
    lit.push(json!({"layer_id":identity("SLYR",8),"timing":timing(),"three_d":{},"light":{"light_type":"point","colour":{"static_value":[1.0,1.0,1.0]},"intensity":s(100.0)}}));
    refused(source(&with_nested(root, lit), &pixels).unwrap().scene_at(0, &cancel).unwrap_err(), three_d::COMPOSITION_3D_COLLAPSE_NESTED);
}

/// A flat plane for the coverage tests: layer space = content plan, `w x w` texels at world
/// `position` (z = 0 maps 1:1 under the default camera).
fn flat_plane(n: u8, w: u32, position: [f64; 3], scale: f64, opacity: f64) -> Plane3d {
    Plane3d {
        layer_id: identity("SLYR", n),
        bounds: hsk_studio_pigment::Rect { x: 0, y: 0, width: w, height: w },
        origin: [0.0, 0.0],
        content: SceneContent::None,
        transform: Transform3d { anchor: [0.0; 3], position, scale: [scale, scale], rotation: three_d::layer_rotation([0.0; 3], 0.0, 0.0, 0.0), parent: three_d::Affine3::IDENTITY },
        mode: StudioBlendMode::Normal,
        opacity,
        fill_opacity: 1.0,
        layer_mask: None,
        track_matte: None,
    }
}

#[test]
fn anti_aliased_aux_channels_match_hand_values() {
    // Lane SMB-AE-3D-3 (STU-CMP-050; `three_d::coverage` module docs): a 4 x 4 box filter. Plane A
    // (id 7) at z = 0 covers screen x 10..18, y 4..12 at depth zoom; plane B (id 9) at z = -zoom/2
    // is magnified 2x and covers screen x 14.5..22.5, y 0..8 at depth zoom/2.
    use three_d::coverage::{aux_channels_aa, id_matte_aa, pixel_coverage};
    let cam = three_d::default_camera(32, 24);
    let zoom = cam.zoom;
    let a = flat_plane(1, 8, [10.0, 4.0, 0.0], 1.0, 1.0);
    let b = flat_plane(2, 4, [15.25, 6.0, -zoom / 2.0], 1.0, 1.0);
    let ones = |p: &Plane3d| vec![1.0f32; (p.bounds.width * p.bounds.height) as usize];
    let alphas = vec![ones(&a), ones(&b)];
    let at = |x: usize, y: usize| y * 32 + x;
    let aux = aux_channels_aa(&[a.clone(), b.clone()], &alphas, &[7, 9], &cam, 32, 24, 4).expect("channels");
    let c = &aux.channels;
    let close = |v: f32, want: f64| (f64::from(v) - want).abs() <= 1e-5 * want.abs().max(1.0);
    // (14, 5): B covers the right half in front of A: 0.5 / 0.5, the tie goes to the nearer B.
    let p = at(14, 5);
    assert_eq!((c.object_id[p], aux.id_coverage[p], c.coverage[p]), (9.0, 0.5, 1.0));
    assert!(close(c.depth[p], zoom / 2.0), "{}", c.depth[p]);
    assert_eq!([c.normal[0][p], c.normal[1][p], c.normal[2][p]], [0.0, 0.0, -1.0], "camera-space normal facing the camera");
    // (12, 5): A alone; (22, 5): B's right edge half covered; (9, 5): nothing.
    let p = at(12, 5);
    assert_eq!((c.object_id[p], aux.id_coverage[p], c.coverage[p]), (7.0, 1.0, 1.0));
    assert!(close(c.depth[p], zoom));
    let p = at(22, 5);
    assert_eq!((c.object_id[p], aux.id_coverage[p], c.coverage[p]), (9.0, 0.5, 0.5));
    let p = at(9, 5);
    assert_eq!((c.object_id[p], aux.id_coverage[p], c.coverage[p], c.depth[p]), (0.0, 0.0, 0.0, f32::INFINITY));
    assert_eq!(id_matte_aa(&aux, &[9])[at(22, 5)], 0.5, "the ID matte is anti-aliased");
    // Non-refractive transparency: B at opacity 0.5 lets A through.
    let half = Plane3d { opacity: 0.5, ..b.clone() };
    let t = aux_channels_aa(&[a.clone(), half.clone()], &alphas, &[7, 9], &cam, 32, 24, 4).unwrap();
    let p = at(14, 5);
    assert_eq!((t.channels.object_id[p], t.id_coverage[p], t.channels.coverage[p]), (7.0, 0.75, 1.0), "A 0.75, B 0.25");
    let ranked = pixel_coverage(&[a.clone(), half], &alphas, &cam, 32, 24, 4).unwrap();
    let e = ranked.pixel(at(16, 5));
    assert_eq!(e.iter().map(|x| (x.plane, x.coverage)).collect::<Vec<_>>(), vec![(1, 0.5), (0, 0.5)], "ties rank the nearer plane first");
    // One sample per pixel on opaque content: ids and depths equal the per-centre channels.
    let one = aux_channels_aa(&[a.clone(), b.clone()], &alphas, &[7, 9], &cam, 32, 24, 1).unwrap();
    let centre = three_d::aux_channels(&[a.clone(), b.clone()], &alphas, &[7, 9], &cam, 32, 24).unwrap();
    assert_eq!(one.channels.object_id, centre.object_id);
    assert_eq!(one.channels.depth, centre.depth);
    // Hostile input is refused before per-pixel work.
    let nan = Plane3d { transform: Transform3d { position: [f64::NAN, 0.0, 0.0], ..a.transform }, ..a.clone() };
    assert!(matches!(aux_channels_aa(&[nan, b.clone()], &alphas, &[7, 9], &cam, 32, 24, 4), Err(TemporalError::InvalidState { field: "three_d_transform", .. })), "NaN transform (NaN depths)");
    let mut bad = alphas.clone();
    bad[0][3] = f32::NAN;
    assert!(matches!(aux_channels_aa(&[a.clone(), b.clone()], &bad, &[7, 9], &cam, 32, 24, 4), Err(TemporalError::InvalidState { field: "aux_channels", .. })), "NaN alpha");
    for s in [0, three_d::coverage::MAX_AUX_SAMPLES + 1] {
        refused(aux_channels_aa(&[a.clone(), b.clone()], &alphas, &[7, 9], &cam, 32, 24, s).unwrap_err(), three_d::AUX_MATTE_PARAMETER);
    }
    assert!(matches!(aux_channels_aa(&[a.clone(), b.clone()], &alphas, &[7, 9], &cam, 8192, 8192, 16), Err(TemporalError::Composite(hsk_studio_composite::CompositeError::BudgetExceeded))));
    // The EXR carries every channel, idCoverage included.
    let back = read_exr_names(&aux.to_exr().unwrap());
    assert_eq!(back, { let mut v = three_d::coverage::AUX_AA_CHANNEL_NAMES.map(String::from).to_vec(); v.sort(); v });
}

/// Channel names of an uncompressed scanline OpenEXR file (independent minimal parse).
fn read_exr_names(b: &[u8]) -> Vec<String> {
    read_exr(b).0.into_iter().map(|c| c.0).collect()
}

/// An independent minimal reader of the writer's subset (single-part scanline, uncompressed FLOAT
/// channels): every channel's plane and every `string` attribute.
#[allow(clippy::type_complexity)]
fn read_exr(b: &[u8]) -> (Vec<(String, Vec<f32>)>, Vec<(String, String)>) {
    let i32_at = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    assert_eq!((i32_at(0), i32_at(4)), (20000630, 2), "magic, version 2 single-part scanline");
    let cstr = |o: usize| -> (String, usize) {
        let e = o + b[o..].iter().position(|c| *c == 0).unwrap();
        (String::from_utf8(b[o..e].to_vec()).unwrap(), e + 1)
    };
    let (mut o, mut names, mut strings, mut window) = (8usize, Vec::new(), Vec::new(), [0i32; 4]);
    while b[o] != 0 {
        let (name, n) = cstr(o);
        let (kind, n) = cstr(n);
        let size = i32_at(n) as usize;
        let v = n + 4;
        match (name.as_str(), kind.as_str()) {
            ("channels", "chlist") => {
                let mut c = v;
                while b[c] != 0 {
                    let (ch, k) = cstr(c);
                    assert_eq!(i32_at(k), 2, "FLOAT");
                    names.push(ch);
                    c = k + 16;
                }
            }
            ("dataWindow", "box2i") => window = [i32_at(v), i32_at(v + 4), i32_at(v + 8), i32_at(v + 12)],
            (_, "string") => strings.push((name.clone(), String::from_utf8(b[v..v + size].to_vec()).unwrap())),
            _ => {}
        }
        o = v + size;
    }
    o += 1;
    let (w, h) = ((window[2] - window[0] + 1) as usize, (window[3] - window[1] + 1) as usize);
    let mut planes: Vec<(String, Vec<f32>)> = names.iter().map(|n| (n.clone(), Vec::new())).collect();
    for y in 0..h {
        let at = u64::from_le_bytes(b[o + 8 * y..o + 8 * y + 8].try_into().unwrap()) as usize;
        assert_eq!(i32_at(at), y as i32);
        let mut p = at + 8;
        for plane in &mut planes {
            for _ in 0..w {
                plane.1.push(f32::from_le_bytes(b[p..p + 4].try_into().unwrap()));
                p += 4;
            }
        }
    }
    (planes, strings)
}

#[test]
fn cryptomatte_hashes_exr_and_mattes_match_the_specification() {
    // Lane SMB-AE-3D-3 (STU-CMP-050 Cryptomatte; Cryptomatte specification v1.2.0 worked examples).
    use three_d::coverage::pixel_coverage;
    use three_d::cryptomatte::{self as cm, build, hash_to_float, matte_from_ranks, murmur3_32, name_id, type_key};
    // Hash and conversion hand values (specification "Example code and conversions", "Manifest").
    assert_eq!(murmur3_32(b"torus", 0), 4_053_562_365);
    assert_eq!(murmur3_32(&[0xd1, 0x80, 0xd0, 0xb0, 0xd0, 0xb2, 0xd0, 0xbd, 0xd0, 0xb8, 0xd0, 0xbd, 0xd0, 0xb0], 0), 2_518_896_926, "UTF-8 bytes are hashed");
    assert_eq!(name_id("torus").to_bits(), 0xf19c_73fd);
    assert!((f64::from(name_id("torus")) / -1.549_436_248_32e30 - 1.0).abs() < 1e-9);
    for (name, hex) in [("bunny", 0x1385_1a76u32), ("default", 0x42c9_679f), ("bunny_porcelain_mat", 0xb24b_26d4)] {
        assert_eq!(name_id(name).to_bits(), hex, "{name}");
    }
    assert_eq!(type_key("CryptoObject"), "3ae39a5");
    // Exponents 0 and 255 toggle bit 23: never a zero, subnormal, infinity or NaN.
    assert_eq!(hash_to_float(0x0000_0001).to_bits(), 0x0080_0001);
    assert_eq!(hash_to_float(0x7f80_0000).to_bits(), 0x7f00_0000);
    assert!(hash_to_float(0xffff_ffff).is_finite() && hash_to_float(0).is_normal());
    // Two planes (the hand case of `anti_aliased_aux_channels_match_hand_values`) named objA / objB.
    let cam = three_d::default_camera(32, 24);
    let a = flat_plane(1, 8, [10.0, 4.0, 0.0], 1.0, 1.0);
    let b = flat_plane(2, 4, [15.25, 6.0, -cam.zoom / 2.0], 1.0, 1.0);
    let alphas = [vec![1.0f32; 64], vec![1.0f32; 16]];
    let cov = pixel_coverage(&[a, b], &alphas, &cam, 32, 24, 4).unwrap();
    let names = ["objA".to_owned(), "objB".to_owned()];
    let obj = build(&cov, &names, "CryptoObject", 4).unwrap();
    let mat = build(&cov, &["material(x)".to_owned(), "material(x)".to_owned()], "CryptoMaterial", 4).unwrap();
    let at = |x: usize, y: usize| y * 32 + x;
    let (ia, ib) = (name_id("objA"), name_id("objB"));
    let (lo, hi) = if ia.to_bits() < ib.to_bits() { (ia, ib) } else { (ib, ia) };
    let p = at(14, 5);
    assert_eq!([obj.ranks[0].0[p].to_bits(), obj.ranks[1].0[p].to_bits()], [lo.to_bits(), hi.to_bits()], "a 0.5 / 0.5 tie ranks the smaller ID bits first");
    assert_eq!((obj.ranks[0].1[p], obj.ranks[1].1[p], obj.ranks[2].1[p], obj.ranks[2].0[p]), (0.5, 0.5, 0.0, 0.0), "unused ranks are ID 0, coverage 0");
    assert_eq!((mat.ranks[0].0[p], mat.ranks[0].1[p]), (name_id("material(x)"), 1.0), "one shared name merges");
    assert_eq!(obj.matte(&["objB"])[at(22, 5)], 0.5, "anti-aliased edge");
    assert_eq!(obj.matte(&["objA", "objB"])[p], 1.0, "overlapping objects sum");
    assert_eq!(obj.matte(&["torus"])[p], 0.0);
    // One EXR file with both types: the channel names, the metadata keys and a manifest that parses.
    let file = cm::to_exr(&[obj.clone(), mat.clone()]).unwrap();
    let (planes, strings) = read_exr(&file);
    let mut want: Vec<String> = ["CryptoMaterial", "CryptoObject"].iter().flat_map(|t| ["00", "01"].iter().flat_map(move |k| ["A", "B", "G", "R"].iter().map(move |c| format!("{t}{k}.{c}")))).collect();
    want.sort();
    assert_eq!(planes.iter().map(|p| p.0.clone()).collect::<Vec<_>>(), want);
    let key = type_key("CryptoObject");
    let get = |k: &str| strings.iter().find(|s| s.0 == format!("cryptomatte/{key}/{k}")).map(|s| s.1.clone()).unwrap_or_else(|| panic!("{k}"));
    assert_eq!((get("name").as_str(), get("hash").as_str(), get("conversion").as_str()), ("CryptoObject", "MurmurHash3_32", "uint32_to_float32"));
    let manifest: serde_json::Map<String, Value> = serde_json::from_str(&get("manifest")).unwrap();
    assert_eq!(manifest.get("objA"), Some(&json!(format!("{:08x}", ia.to_bits()))));
    // Matte extraction from the planes read back equals the in-memory matte, bit for bit.
    let plane = |n: &str| &planes.iter().find(|p| p.0 == n).unwrap().1[..];
    let ranks = [(plane("CryptoObject00.R"), plane("CryptoObject00.G")), (plane("CryptoObject00.B"), plane("CryptoObject00.A")), (plane("CryptoObject01.R"), plane("CryptoObject01.G")), (plane("CryptoObject01.B"), plane("CryptoObject01.A"))];
    assert_eq!(matte_from_ranks(&ranks, &["objB"]).unwrap(), obj.matte(&["objB"]));
    // Hostile input: names that need JSON escaping round-trip; bad levels, type names, huge name
    // sets and oversized names are refused before allocation.
    let odd = ["a\"b\\c\n\u{1}".to_owned(), "\u{43a}\u{43e}\u{442}".to_owned()];
    let m: serde_json::Map<String, Value> = serde_json::from_str(&build(&cov, &odd, "CryptoAsset", 2).unwrap().manifest_json()).unwrap();
    assert!(odd.iter().all(|n| m.contains_key(n)));
    for levels in [0, 3, 18] {
        assert_eq!(build(&cov, &names, "CryptoObject", levels).unwrap_err(), cm::CRYPTO_PARAMETER);
    }
    for t in ["", "Crypto.Object", "CryptoObjectWithAVeryLongName"] {
        assert_eq!(build(&cov, &names, t, 4).unwrap_err(), cm::CRYPTO_PARAMETER, "{t:?}");
    }
    let many: Vec<String> = (0..=cm::MAX_CRYPTO_NAMES).map(|k| format!("n{k}")).collect();
    assert_eq!(build(&cov, &many, "CryptoObject", 4).unwrap_err(), cm::CRYPTO_PARAMETER, "more than 65536 ids");
    assert_eq!(build(&cov, &[names[0].clone(), "x".repeat(cm::MAX_NAME_BYTES + 1)], "CryptoObject", 4).unwrap_err(), cm::CRYPTO_PARAMETER);
    assert_eq!(build(&cov, &names[..1], "CryptoObject", 4).unwrap_err(), cm::CRYPTO_PARAMETER, "a plane without a name");
    assert!(matte_from_ranks(&[(&[0.0][..], &[f32::NAN][..])], &["objA"]).is_err());
    // The adapter names the planes: layer ids, and one material name per evaluated material.
    let pixels = Pixels::fixture();
    let src = source(&records(layers()), &pixels).unwrap();
    let (planes, objects, materials, _) = src.three_d_cryptomatte_names(0, &CancellationToken::default()).unwrap();
    assert_eq!(objects, planes.iter().map(|p| p.layer_id.clone()).collect::<Vec<_>>());
    assert_eq!(materials, vec![three_d::Material::DEFAULT.name(); 2]);
}
