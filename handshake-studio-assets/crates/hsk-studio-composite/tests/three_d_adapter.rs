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
        "layers":[group(6), group(8), raster(3, vec![tile(3,"tb",6,6)]), raster(10, vec![tile(10,"te",10,8)]),
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
/// Topmost first: camera, light, plane "tf", 2D "tb", plane "te".
fn layers() -> Vec<Value> {
    vec![
        json!({"layer_id":identity("SLYR",6),"timing":timing(),"streams":[{"property":"position","static_value":[10.0,4.0]}],
            "three_d":{"position_z":s(-60.0)},
            "camera":{"zoom":s(50.0),"point_of_interest":{"static_value":[16.0,12.0,0.0]}}}),
        json!({"layer_id":identity("SLYR",8),"timing":timing(),"three_d":{},
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
    at_open(&|l| l[2]["parent"] = json!(identity("SLYR", 3)), three_d::COMPOSITION_3D_PARENT);
    at_open(&|l| l[3]["parent"] = json!(identity("SLYR", 11)), three_d::COMPOSITION_3D_PARENT);
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
