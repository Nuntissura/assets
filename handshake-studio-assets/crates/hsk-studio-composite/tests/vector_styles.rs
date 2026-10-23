//! Lane SMB-VECTOR-STYLES slice 1: composite's Folio adapter paints per-region fill styles of
//! vector networks, strokes Folio width profiles (`StudioVectorRecords::width_profiles`) with nib's
//! variable-width outline, and takes caller-resolved plan colours (`VectorAssets::plan_colour`).
//! Expected layers are the folio-free specs built from nib directly (hand-built networks and
//! profiles), prepared by `prepare_vector_layers`; the adapter's output must equal them.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{
    FillRule, OwnedPath, PathElement, PathPoint, REFUSE_NETWORK_REGION_STYLE, REFUSE_PAINT, REFUSE_PAINT_LINEAR,
    REFUSE_WIDTH_ASYMMETRIC, REFUSE_WIDTH_STYLE,
};
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioColor, StudioDesignRecords, StudioImageSource, StudioVectorNetwork, StudioVectorRecords};
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::network::{NetworkRegion, NetworkSegment, NetworkVertex, VectorNetwork};
use hsk_studio_nib::stroke::{StrokeOptions, outline_stroke};
use hsk_studio_nib::width::{WidthCap, WidthPoint, WidthProfile, outline_variable};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use serde_json::{Value, json};

mod common;
use common::*;

const LEFT: [u32; 4] = [0, 6, 4, 5];
const RIGHT: [u32; 4] = [1, 2, 3, 6];

/// A 10 x 10 square split by the divider 1-4: vertices 0..5, segments 0..6.
fn network(right_style: Option<&str>) -> Value {
    let mut right = json!({"winding_rule":"NONZERO","loops":[RIGHT],"fills":true});
    if let Some(s) = right_style {
        right["fill_style_id"] = json!(s);
    }
    json!({"schema_id":"hsk.studio.vector_network@1","path_id":identity("SVPT",1),
        "vertices":[{"x":2.0,"y":3.0},{"x":7.0,"y":3.0},{"x":12.0,"y":3.0},{"x":12.0,"y":13.0},{"x":7.0,"y":13.0},{"x":2.0,"y":13.0}],
        "segments":[{"start":0,"end":1},{"start":1,"end":2},{"start":2,"end":3},{"start":3,"end":4},{"start":4,"end":5},{"start":5,"end":0},{"start":1,"end":4}],
        "regions":[{"winding_rule":"NONZERO","loops":[LEFT],"fills":true}, right]})
}

/// The same network built in nib by hand (expected geometry).
fn nib_network() -> VectorNetwork {
    let v = |x: f64, y: f64| NetworkVertex::at(x, y);
    let region = |l: [u32; 4]| NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![l.iter().map(|k| *k as usize).collect()], fills: true, fill_style_id: None };
    VectorNetwork {
        vertices: vec![v(2.0, 3.0), v(7.0, 3.0), v(12.0, 3.0), v(12.0, 13.0), v(7.0, 13.0), v(2.0, 13.0)],
        segments: [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 0), (1, 4)].iter().map(|(a, b)| NetworkSegment::line(*a, *b)).collect(),
        regions: vec![region(LEFT), region(RIGHT)],
    }
}

fn polyline() -> OwnedPath {
    let p = PathPoint::new;
    OwnedPath {
        elements: vec![PathElement::MoveTo(p(2.0, 8.0)), PathElement::LineTo(p(14.0, 8.0)), PathElement::LineTo(p(20.0, 16.0))],
        fill_rule: FillRule::NonZero,
    }
}

struct Assets {
    network: Option<Value>,
    colour: Option<[f32; 3]>,
}
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (path_id == identity("SVPT", 1)).then(polyline)
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
    fn network(&self, path_id: &str) -> Option<StudioVectorNetwork> {
        let n = self.network.clone().filter(|_| path_id == identity("SVPT", 1))?;
        Some(serde_json::from_value(n).expect("network shape"))
    }
    fn plan_colour(&self, _: &StudioColor, _: BlendSpace) -> Option<[f32; 3]> {
        self.colour
    }
}

fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"paint_kind":"solid","colour":{"r":r,"g":g,"b":b,"colour_profile_id":null}})
}
fn fill_row(paint: Value) -> Value {
    json!({"row_kind":"fill","paint":paint,"opacity":100.0,"blend_mode":2,"visible":true})
}
fn stroke_row(paint: Value, cap: &str) -> Value {
    json!({"row_kind":"stroke","paint":paint,
        "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":cap,"join":"miter","align":"CENTER","miter_limit":4.0,
        "dash":[],"dash_offset":{"value":0.0,"unit":"px"}},"opacity":100.0,"blend_mode":2,"visible":true})
}
fn style(n: u8, rows: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",n),"kind":"PAINT",
        "name":format!("s{n}"),"publishing":{"description":"","description_markdown":"","documentation_links":[],
        "publish_key":format!("s{n}"),"is_remote":false},"rows":rows})
}
/// Layer 4 uses style 1 (`layer_rows`); style 2 (`region_rows`) is the region style.
fn design(layer_rows: Vec<Value>, region_rows: Vec<Value>) -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",4)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[style(1, layer_rows), style(2, region_rows)]}))
    .expect("design records shape")
}
fn records(width_points: Option<Value>) -> StudioVectorRecords {
    let mut r = json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)});
    if let Some(points) = width_points {
        r["width_profiles"] = json!([{"path_id":identity("SVPT",1),"points":points}]);
    }
    serde_json::from_value(r).expect("vector records shape")
}

fn prepare(assets: &Assets, design: &StudioDesignRecords, records: &StudioVectorRecords, space: BlendSpace) -> PreparedVectors {
    let mut d = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!("px");
    prepare_vectors(&open(&d), design, records, assets, space, &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
fn refusal(assets: &Assets, design: &StudioDesignRecords, records: &StudioVectorRecords, space: BlendSpace) -> Option<&'static str> {
    prepare(assets, design, records, space).refusal(&identity("SLYR", 4))
}
fn expected(geometry: Vec<OwnedPath>, fills: Vec<FillSpec>) -> PreparedVectors {
    let spec = VectorLayerSpec { layer_id: identity("SLYR", 4), geometry: VectorGeometry::Paths(geometry), fills };
    prepare_vector_layers(&[spec], &VectorResolution::default(), &CancellationToken::default()).unwrap()
}
fn row(paint: FillPaint) -> FillSpec {
    FillSpec { paint, mode: StudioBlendMode::Normal, opacity: 1.0 }
}
fn solid(r: f32, g: f32, b: f32) -> FillPaint {
    FillPaint::Solid([r, g, b, 1.0])
}

#[test]
fn region_fill_styles_paint_their_regions_between_fills_and_strokes() {
    let assets = Assets { network: Some(network(Some(&identity("SSTY", 2)))), colour: None };
    let design = design(vec![fill_row(colour(1.0, 0.0, 0.0)), stroke_row(colour(0.0, 0.0, 1.0), "butt")], vec![fill_row(colour(0.0, 0.5, 0.25))]);
    let got = prepare(&assets, &design, &records(None), BlendSpace::Encoded);
    let net = nib_network();
    let faces: Vec<OwnedPath> = net.fill_geometry().unwrap().iter().map(|g| g.to_owned_path().unwrap()).collect();
    let stroke = net.stroke_geometry().unwrap().to_owned_path().unwrap();
    let outline = outline_stroke(&stroke, &hsk_studio_composite::vector::StrokeStyle::new(2.0), &StrokeOptions::default()).unwrap().path;
    let want = expected(
        vec![faces[0].clone()],
        vec![
            row(solid(1.0, 0.0, 0.0)),
            row(FillPaint::StrokeGeometry { outline: vec![faces[1].clone()], paint: Box::new(solid(0.0, 0.5, 0.25)) }),
            row(FillPaint::StrokeGeometry { outline: vec![outline], paint: Box::new(solid(0.0, 0.0, 1.0)) }),
        ],
    );
    assert_eq!(got.refusal(&identity("SLYR", 4)), None);
    assert_eq!(got, want);
    // A region style holding a stroke row is refused typed; an unresolved region style too.
    let stroked = design_with_region(vec![fill_row(colour(0.0, 0.5, 0.25)), stroke_row(colour(0.0, 0.0, 1.0), "butt")]);
    assert_eq!(refusal(&assets, &stroked, &records(None), BlendSpace::Encoded), Some(REFUSE_NETWORK_REGION_STYLE));
    let dangling = Assets { network: Some(network(Some(&identity("SSTY", 9)))), colour: None };
    assert_eq!(refusal(&dangling, &stroked, &records(None), BlendSpace::Encoded), Some("vector_fill_style_unresolved"));
}

fn design_with_region(region_rows: Vec<Value>) -> StudioDesignRecords {
    design(vec![fill_row(colour(1.0, 0.0, 0.0))], region_rows)
}

#[test]
fn folio_width_profiles_stroke_with_the_variable_outline() {
    let assets = Assets { network: None, colour: None };
    let design = design(vec![fill_row(colour(1.0, 0.0, 0.0)), stroke_row(colour(0.0, 0.0, 1.0), "round")], vec![]);
    let points = json!([{"position":0.0,"width":1.0},{"position":0.5,"width":6.0},{"position":1.0,"width":2.0}]);
    let got = prepare(&assets, &design, &records(Some(points)), BlendSpace::Encoded);
    let profile = WidthProfile { points: vec![WidthPoint { t: 0.0, width: 1.0 }, WidthPoint { t: 0.5, width: 6.0 }, WidthPoint { t: 1.0, width: 2.0 }] };
    let outline = outline_variable(&polyline(), &profile, WidthCap::Round, &BooleanOptions::default()).unwrap().path;
    let want = expected(vec![polyline()], vec![row(solid(1.0, 0.0, 0.0)), row(FillPaint::StrokeGeometry { outline: vec![outline], paint: Box::new(solid(0.0, 0.0, 1.0)) })]);
    assert_eq!(got.refusal(&identity("SLYR", 4)), None);
    assert_eq!(got, want);
    // Independent outgoing width and a projecting cap are refused typed (nib has neither).
    let asymmetric = json!([{"position":0.0,"width":1.0,"width_out":3.0},{"position":1.0,"width":2.0}]);
    assert_eq!(refusal(&assets, &design, &records(Some(asymmetric)), BlendSpace::Encoded), Some(REFUSE_WIDTH_ASYMMETRIC));
    let projecting = projecting_design();
    let ok = json!([{"position":0.0,"width":1.0},{"position":1.0,"width":2.0}]);
    assert_eq!(refusal(&assets, &projecting, &records(Some(ok)), BlendSpace::Encoded), Some(REFUSE_WIDTH_STYLE));
}

fn projecting_design() -> StudioDesignRecords {
    design(vec![stroke_row(colour(0.0, 0.0, 1.0), "projecting")], vec![])
}

#[test]
fn caller_plan_colours_are_used_and_checked() {
    let design = design(vec![fill_row(colour(0.5, 0.5, 0.5))], vec![]);
    // Default: a linear-light plan takes only 0 and 1.
    let plain = Assets { network: None, colour: None };
    assert_eq!(refusal(&plain, &design, &records(None), BlendSpace::LinearLight), Some(REFUSE_PAINT_LINEAR));
    // The caller resolves the working-space colour into plan units.
    let resolved = Assets { network: None, colour: Some([0.25, 0.125, 0.0625]) };
    let got = prepare(&resolved, &design, &records(None), BlendSpace::LinearLight);
    assert_eq!(got, expected(vec![polyline()], vec![row(solid(0.25, 0.125, 0.0625))]));
    // A non-finite caller colour is refused typed.
    let hostile = Assets { network: None, colour: Some([f32::NAN, 0.0, 0.0]) };
    assert_eq!(refusal(&hostile, &design, &records(None), BlendSpace::LinearLight), Some(REFUSE_PAINT));
}
