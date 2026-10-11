use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}
fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
struct Caller(Resolution);
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        self.0
    }
    fn profile(&self, _: &str) -> Resolution {
        self.0
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        self.0
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"r": r, "g": g, "b": b, "colour_profile_id": null})
}
fn rgba(r: f64, g: f64, b: f64, a: f64) -> Value {
    json!({"r": r, "g": g, "b": b, "a": a, "colour_profile_id": null})
}
fn publishing(key: &str) -> Value {
    json!({"description": "", "description_markdown": "", "documentation_links": [],
        "publish_key": key, "is_remote": false})
}
fn set(v: &mut Value, pointer: &str, value: Value) {
    let (parent, last) = pointer.rsplit_once('/').expect("pointer");
    match v.pointer_mut(parent).expect("parent") {
        Value::Array(a) => a[last.parse::<usize>().expect("index")] = value,
        Value::Object(o) => {
            o.insert(last.to_owned(), value);
        }
        _ => panic!("pointer parent is a scalar"),
    }
}
fn remove(v: &mut Value, pointer: &str) {
    let (parent, last) = pointer.rsplit_once('/').expect("pointer");
    match v.pointer_mut(parent).expect("parent") {
        Value::Object(o) => {
            o.remove(last).expect("member to remove");
        }
        _ => panic!("pointer parent is not an object"),
    }
}
fn easing() -> Value {
    json!({"easing_function_cubic_bezier": {"x1": 0.25, "y1": 0.1, "x2": 0.25, "y2": 1.0}})
}

// ---------------------------------------------------------------------------------------------
// Paint style

fn gradient_paint() -> Value {
    json!({"paint_kind": "gradient", "gradient": {
        "geometry": {"gradient_kind": "linear", "x1": 0.0, "y1": 0.0, "x2": 1.0, "y2": 0.0},
        "transform": [100.0, 0.0, 0.0, 50.0, 10.0, 20.0], "spread": "pad",
        "interpolation": "perceptual", "dither": false,
        "stops": [
            {"position": 0.0, "midpoint": 50.0, "opacity": 100.0, "colour": colour(1.0, 0.0, 0.0)},
            {"position": 100.0, "midpoint": null, "opacity": 50.0, "colour": colour(0.0, 0.0, 1.0)}]}})
}
fn style() -> Value {
    json!({"schema_id": "hsk.studio.style_registry@1", "style_id": identity("SSTY", 1),
        "kind": "PAINT", "name": "brand/primary", "publishing": publishing("brand-primary"),
        "rows": [
            {"row_kind": "fill", "paint": {"paint_kind": "solid", "colour": colour(1.0, 0.5, 0.0)},
                "opacity": 100.0, "blend_mode": 2, "visible": true},
            {"row_kind": "fill", "paint": gradient_paint(), "opacity": 80.0, "blend_mode": 3, "visible": true},
            {"row_kind": "stroke", "paint": {"paint_kind": "gradient", "gradient": {
                "geometry": {"gradient_kind": "radial", "cx": 0.5, "cy": 0.5, "r": 0.5, "fx": 0.4, "fy": 0.4, "fr": 0.0},
                "transform": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], "spread": "reflect",
                "interpolation": "classic", "dither": true,
                "stops": [
                    {"position": 10.0, "midpoint": null, "opacity": 100.0, "colour": colour(0.0, 0.0, 0.0)},
                    {"position": 90.0, "midpoint": null, "opacity": 100.0, "colour": colour(1.0, 1.0, 1.0)}]}},
                "stroke": {"weight": pt(2.0), "cap": "round", "join": "bevel", "align": "CENTER",
                    "miter_limit": 4.0, "dash": [pt(4.0), pt(2.0)], "dash_offset": pt(1.0)},
                "opacity": 100.0, "blend_mode": 2, "visible": false}]})
}
fn style_result(v: &Value, resolver: &dyn Resolver) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let s: StudioPaintStyle = serde_json::from_value(v.clone()).expect("style parses");
    validate_paint_style(
        &s,
        Budget::default(),
        &CancellationToken::default(),
        resolver,
    )
}
fn style_refused(pointer: &str, value: Value, code: Code, suffix: &str) {
    let mut v = style();
    set(&mut v, pointer, value);
    let d = style_result(&v, &Caller(Resolution::Available)).expect_err("must be refused");
    assert_eq!(d.code, code, "{pointer}: {}", d.target);
    assert!(
        d.target.ends_with(suffix),
        "{pointer}: target {} lacks {suffix}",
        d.target
    );
}

// ---------------------------------------------------------------------------------------------
// Document and records

fn bounds(w: f64, h: f64) -> Value {
    json!({"x": pt(0.0), "y": pt(0.0), "width": pt(w), "height": pt(h)})
}
fn layer(n: u32, kind: &str, parent: Value) -> Value {
    let payload = match kind {
        "group" => json!({"payload_kind": "group"}),
        _ => json!({"payload_kind": "raster", "tiles": []}),
    };
    json!({"schema_id": "hsk.studio.layer@1", "layer_id": identity("SLYR", n),
        "name": format!("layer {n}"), "parent": parent, "kind": kind, "payload": payload, "visible": true})
}
fn under(n: u32) -> Value {
    json!({"node_kind": "layer", "node_id": identity("SLYR", n)})
}
fn on_board() -> Value {
    json!({"node_kind": "artboard", "node_id": identity("SART", 1)})
}
/// Artboard 1; frame 1 with children 2 and 3; plain frame 4 with child 5; component set container
/// 10 with member containers 11 and 12 holding layers 13 and 14.
fn document() -> Value {
    json!({"schema_id": "hsk.studio.document@1", "document_id": identity("SDOC", 1), "revision": 1,
        "geometry_unit": "pt", "colour_mode": 2, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null,
        "working_space_spot": null, "blending_space": null,
        "artboards": [{"schema_id": "hsk.studio.artboard@1", "artboard_id": identity("SART", 1),
            "name": "screen", "parent": null, "bounds": bounds(800.0, 600.0)}],
        "page_spreads": [],
        "layers": [
            layer(1, "group", on_board()), layer(2, "raster", under(1)), layer(3, "raster", under(1)),
            layer(4, "group", on_board()), layer(5, "raster", under(4)),
            layer(10, "group", on_board()), layer(11, "group", under(10)), layer(12, "group", under(10)),
            layer(13, "raster", under(11)), layer(14, "raster", under(12))],
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []}})
}
fn open_document() -> Snapshot {
    let bytes = serde_json::to_vec(&document()).unwrap();
    match inspect_bytes(
        &bytes,
        Budget::default(),
        &CancellationToken::default(),
        &Caller(Resolution::Available),
    )
    .expect("valid document")
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("expected an editable snapshot"),
    }
}
fn node(kind: &str, id_text: String) -> Value {
    json!({"node_kind": kind, "node_id": id_text})
}
fn flow_layout() -> Value {
    json!({"schema_id": "hsk.studio.auto_layout@1", "layout_mode": "HORIZONTAL", "layout_wrap": "NO_WRAP",
        "primary_axis_sizing_mode": "AUTO", "counter_axis_sizing_mode": "FIXED",
        "primary_axis_align_items": "SPACE_BETWEEN", "counter_axis_align_items": "CENTER",
        "counter_axis_align_content": "AUTO", "item_spacing": 8.0, "counter_axis_spacing": null,
        "padding_top": 4.0, "padding_right": 4.0, "padding_bottom": 4.0, "padding_left": 4.0,
        "strokes_included_in_layout": false, "item_reverse_z_index": false})
}
fn plain_layout() -> Value {
    let mut v = flow_layout();
    for (k, value) in [
        ("layout_mode", json!("NONE")),
        ("primary_axis_sizing_mode", json!("AUTO")),
        ("counter_axis_sizing_mode", json!("AUTO")),
        ("primary_axis_align_items", json!("MIN")),
        ("counter_axis_align_items", json!("MIN")),
        ("item_spacing", json!(0.0)),
        ("padding_top", json!(0.0)),
        ("padding_right", json!(0.0)),
        ("padding_bottom", json!(0.0)),
        ("padding_left", json!(0.0)),
    ] {
        set(&mut v, &format!("/{k}"), value);
    }
    v
}
fn child_layout(grow: u8, positioning: &str) -> Value {
    json!({"layout_align": "INHERIT", "layout_grow": grow, "layout_positioning": positioning,
        "layout_sizing_horizontal": "FILL", "layout_sizing_vertical": "HUG"})
}
fn constraint(horizontal: &str, vertical: &str) -> Value {
    json!({"schema_id": "hsk.studio.constraint@1", "horizontal": horizontal, "vertical": vertical})
}
fn navigate() -> Value {
    json!({"type": "NODE", "destination_id": identity("SART", 1), "navigation": "NAVIGATE",
        "transition": {"type": "DISSOLVE", "easing": easing(), "duration": 300},
        "preserve_scroll_position": false, "reset_video_position": false,
        "reset_scroll_position": false, "reset_interactive_components": false})
}
fn property(name: &str, ty: &str, default: Value) -> Value {
    json!({"property_name": name, "type": ty, "default_value": default})
}
fn mode(id_text: &str, name: &str) -> Value {
    json!({"mode_id": id_text, "name": name})
}
fn colour_value(r: f64) -> Value {
    json!({"type": "COLOR", "value": rgba(r, 0.2, 0.3, 1.0)})
}
fn alias(target: u32, resolved: &str) -> Value {
    json!({"type": "VARIABLE_ALIAS", "id": identity("SVAR", target), "resolved_type": resolved})
}
fn variable(n: u32, name: &str, ty: &str, light: Value, dark: Value) -> Value {
    json!({"schema_id": "hsk.studio.variable@1", "variable_id": identity("SVAR", n), "name": name,
        "hidden_from_publishing": false, "publishing": publishing(name),
        "variable_collection_id": identity("SVCL", 1), "resolved_type": ty,
        "values_by_mode": {"light": light, "dark": dark}, "scopes": ["ALL_SCOPES"],
        "code_syntax": {"WEB": "--bg", "iOS": "bg"}})
}
fn records() -> Value {
    json!({"schema_id": "hsk.studio.design_records@1", "document_id": identity("SDOC", 1),
        "nodes": [
            {"node": node("layer", identity("SLYR", 1)), "auto_layout": flow_layout(),
                "size_clamps": {"min_width": 10.0, "max_width": 500.0, "min_height": null, "max_height": null}},
            {"node": node("layer", identity("SLYR", 4)), "auto_layout": plain_layout()},
            {"node": node("layer", identity("SLYR", 2)), "child_layout": child_layout(1, "AUTO"),
                "reactions": [
                    {"trigger": {"type": "ON_CLICK"}, "actions": [navigate(),
                        {"type": "SET_VARIABLE", "variable_id": identity("SVAR", 3),
                            "variable_value": {"type": "BOOLEAN", "value": true}}]},
                    {"trigger": null, "actions": [{"type": "BACK"}]}]},
            {"node": node("layer", identity("SLYR", 3)), "child_layout": child_layout(0, "ABSOLUTE"),
                "constraints": constraint("MAX", "STRETCH")},
            {"node": node("layer", identity("SLYR", 5)), "constraints": constraint("SCALE", "CENTER")},
            {"node": node("layer", identity("SLYR", 13)),
                "component_property_references": {"visible": "showIcon#12:3", "characters": "label#4:1"}},
            {"node": node("artboard", identity("SART", 1)),
                "explicit_variable_modes": {identity("SVCL", 1): "dark"},
                "reactions": [{"trigger": {"type": "AFTER_TIMEOUT", "timeout": 800},
                    "actions": [{"type": "URL", "url": "https://example.com/spec", "open_in_new_tab": true}]}]}],
        "components": [
            {"schema_id": "hsk.studio.component@1", "component_id": identity("SCMP", 2),
                "layer_id": identity("SLYR", 11), "publishing": publishing("button-small"),
                "variant_properties": {"size": "small"},
                "component_property_definitions": [property("label", "TEXT", json!("OK"))]},
            {"schema_id": "hsk.studio.component@1", "component_id": identity("SCMP", 3),
                "layer_id": identity("SLYR", 12), "publishing": publishing("button-large"),
                "variant_properties": {"size": "large"}, "component_property_definitions": []}],
        "component_sets": [
            {"schema_id": "hsk.studio.component@1", "component_id": identity("SCMP", 1),
                "layer_id": identity("SLYR", 10), "publishing": publishing("button"),
                "default_variant_id": identity("SCMP", 2),
                "variant_group_properties": {"size": ["small", "large"]},
                "component_property_definitions": [property("showIcon", "BOOLEAN", json!(false))]}],
        "component_instances": [
            {"schema_id": "hsk.studio.component_instance@1", "instance_id": identity("SCIN", 1),
                "main_component_id": identity("SCMP", 2),
                "component_properties": {
                    "showIcon": {"type": "BOOLEAN", "value": true, "bound_variable_id": identity("SVAR", 3)},
                    "label": {"type": "TEXT", "value": "Go"}},
                "overrides": [{"overridden_node_id": identity("SLYR", 13),
                    "overridden_fields": ["characters", "visible"],
                    "values": {"visible": {"type": "BOOLEAN", "value": false}}}],
                "scale_factor": 1.0, "is_exposed_instance": false}],
        "variable_collections": [
            {"schema_id": "hsk.studio.variable_collection@1", "collection_id": identity("SVCL", 1),
                "name": "theme", "hidden_from_publishing": false, "publishing": publishing("theme"),
                "is_extension": false, "modes": [mode("light", "Light"), mode("dark", "Dark")],
                "default_mode_id": "light",
                "variable_ids": [identity("SVAR", 1), identity("SVAR", 2), identity("SVAR", 3)]}],
        "variables": [
            variable(1, "color/bg", "COLOR", colour_value(0.9), colour_value(0.1)),
            variable(2, "color/bg-alias", "COLOR", alias(1, "COLOR"), alias(1, "COLOR")),
            variable(3, "flag/show", "BOOLEAN", json!({"type": "BOOLEAN", "value": true}),
                json!({"type": "BOOLEAN", "value": false}))],
        "paint_styles": [style()]})
}
fn result(v: &Value, resolver: &dyn Resolver) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioDesignRecords = serde_json::from_value(v.clone()).expect("records parse");
    let snapshot = open_document();
    validate_design_records(
        &r,
        snapshot.document(),
        Budget::default(),
        &CancellationToken::default(),
        resolver,
    )
}
fn refused_with(v: &Value, code: Code, suffix: &str, label: &str) {
    let d = result(v, &Caller(Resolution::Available)).expect_err("must be refused");
    assert_eq!(d.code, code, "{label}: {}", d.target);
    assert!(
        d.target.ends_with(suffix),
        "{label}: target {} lacks {suffix}",
        d.target
    );
}
/// The valid records with each `(pointer, value)` applied, which must be refused.
fn refused(edits: &[(&str, Value)], code: Code, suffix: &str) {
    let mut v = records();
    for (pointer, value) in edits {
        set(&mut v, pointer, value.clone());
    }
    refused_with(&v, code, suffix, edits[0].0);
}

#[test]
fn paint_styles_round_trip_and_invalid_paint_is_refused() {
    bounded_case("paint", || {
        let v = style();
        let s: StudioPaintStyle = serde_json::from_value(v.clone()).expect("style parses");
        assert_eq!(serde_json::to_value(&s).unwrap(), v, "round trip");
        assert!(
            style_result(&v, &Caller(Resolution::Available))
                .unwrap()
                .is_empty()
        );
        assert_eq!((StrokeCap::Round.wire(), StrokeJoin::Bevel.wire()), (2, 3));
        assert_eq!(StrokeCap::from_wire(3), Some(StrokeCap::Projecting));
        // An assigned colour profile is resolved through the caller once per distinct id.
        let mut profiled = v.clone();
        let profile = identity("SCPF", 1);
        set(
            &mut profiled,
            "/rows/0/paint/colour/colour_profile_id",
            json!(profile),
        );
        set(
            &mut profiled,
            "/rows/1/paint/gradient/stops/0/colour/colour_profile_id",
            json!(profile),
        );
        let issues = style_result(&profiled, &Caller(Resolution::Absent)).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].target.ends_with("/colour_profile")
                && issues[0].disposition == Resolution::Absent
        );
        assert!(
            style_result(&profiled, &Caller(Resolution::Available))
                .unwrap()
                .is_empty()
        );

        let g = "/rows/1/paint/gradient";
        style_refused(
            &format!("{g}/stops/0/midpoint"),
            json!(90.0),
            Code::InvalidField,
            "/stops/0/midpoint",
        );
        style_refused(
            &format!("{g}/stops/0/midpoint"),
            json!(12.9),
            Code::InvalidField,
            "/stops/0/midpoint",
        );
        style_refused(
            &format!("{g}/stops/1/position"),
            json!(-1.0),
            Code::InvalidField,
            "/stops/1/position",
        );
        style_refused(
            &format!("{g}/stops/1/opacity"),
            json!(101.0),
            Code::InvalidAppearance,
            "/stops/1/opacity",
        );
        style_refused(
            &format!("{g}/stops"),
            json!([]),
            Code::InvalidField,
            "/stops",
        );
        style_refused(
            &format!("{g}/transform"),
            json!([1.0, 2.0, 2.0, 4.0, 0.0, 0.0]),
            Code::InvalidField,
            "/transform",
        );
        style_refused(
            "/rows/0/paint/colour/r",
            json!(1.5),
            Code::InvalidField,
            "/colour",
        );
        style_refused(
            "/rows/2/stroke/dash",
            json!([pt(4.0), pt(2.0), pt(1.0)]),
            Code::InvalidField,
            "/dash",
        );
        style_refused(
            "/rows/2/stroke/miter_limit",
            json!(600.0),
            Code::InvalidField,
            "/miter_limit",
        );
        style_refused(
            "/rows/2/stroke/weight",
            pt(-1.0),
            Code::InvalidField,
            "/weight",
        );
        style_refused(
            "/rows/0/opacity",
            json!(100.5),
            Code::InvalidAppearance,
            "/opacity",
        );
        style_refused(
            "/rows/0/blend_mode",
            json!(24),
            Code::InapplicableBlendMode,
            "/blend_mode",
        );
        style_refused(
            "/rows/0/blend_mode",
            json!(1),
            Code::InapplicableBlendMode,
            "/blend_mode",
        );
        style_refused(
            "/rows/0/blend_mode",
            json!(36),
            Code::UnknownBlendMode,
            "/blend_mode",
        );
        style_refused("/kind", json!("TEXT"), Code::PayloadMismatch, "/kind");
        style_refused(
            "/name",
            json!("brand//primary"),
            Code::InvalidField,
            "/name",
        );
        style_refused(
            "/style_id",
            json!(identity("SLYR", 1)),
            Code::InvalidId,
            "style_id",
        );
        // Closed objects: an unknown member or an unsupported paint kind does not parse.
        for (pointer, value) in [
            ("/rows/0/paint/bogus", json!(1)),
            ("/rows/0/paint/paint_kind", json!("pattern")),
            ("/rows/2/stroke/cap", json!("square")),
        ] {
            let mut bad = v.clone();
            set(&mut bad, pointer, value);
            assert!(
                serde_json::from_value::<StudioPaintStyle>(bad).is_err(),
                "{pointer}"
            );
        }
        let budget = Budget {
            nodes: 2,
            ..Budget::default()
        };
        let s: StudioPaintStyle = serde_json::from_value(v).unwrap();
        let exceeded = validate_paint_style(
            &s,
            budget,
            &CancellationToken::default(),
            &Caller(Resolution::Available),
        );
        assert_eq!(exceeded.unwrap_err().code, Code::BudgetExceeded);
    });
}

#[test]
fn design_records_round_trip_and_node_rules_are_refused() {
    bounded_case("nodes", || {
        // The document is untouched by design records and still round-trips byte-identically.
        let snapshot = open_document();
        assert_eq!(
            serde_json::to_value(snapshot.document()).unwrap(),
            document()
        );
        let v = records();
        let r: StudioDesignRecords = serde_json::from_value(v.clone()).expect("records parse");
        assert_eq!(serde_json::to_value(&r).unwrap(), v, "round trip");
        assert!(
            result(&v, &Caller(Resolution::Available))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.node_design(&identity("SLYR", 3))
                .unwrap()
                .constraints
                .as_ref()
                .unwrap()
                .vertical,
            ConstraintType::Stretch
        );
        assert_eq!(
            r.nodes[0]
                .auto_layout
                .as_ref()
                .unwrap()
                .horizontal_padding(),
            (4.0, false)
        );
        assert_eq!(StudioAutoLayout::none().layout_mode, LayoutMode::None);
        assert!(design_records_schema().as_value().is_object());
        // An empty record set is valid and encodes only its identity.
        let empty = StudioDesignRecords::new(identity("SDOC", 1));
        assert_eq!(
            serde_json::to_value(&empty)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            2
        );

        let r0 = "/nodes/2/reactions/0";
        refused(
            &[("/document_id", json!(identity("SDOC", 2)))],
            Code::InvalidField,
            "document_id",
        );
        refused(
            &[("/nodes/0/auto_layout/layout_mode", json!("GRID"))],
            Code::UnsupportedDescriptor,
            "/auto_layout/layout_mode",
        );
        refused(
            &[
                ("/nodes/0/auto_layout/layout_mode", json!("NONE")),
                ("/nodes/0/auto_layout/layout_wrap", json!("WRAP")),
            ],
            Code::InvalidField,
            "/auto_layout/layout_wrap",
        );
        refused(
            &[(
                "/nodes/0/auto_layout/counter_axis_align_content",
                json!("SPACE_BETWEEN"),
            )],
            Code::InvalidField,
            "/counter_axis_align_content",
        );
        refused(
            &[("/nodes/0/auto_layout/item_spacing", json!(100001.0))],
            Code::InvalidField,
            "/item_spacing",
        );
        refused(
            &[("/nodes/0/auto_layout/padding_left", json!(-1.0))],
            Code::InvalidField,
            "/padding_left",
        );
        refused(
            &[("/nodes/0/size_clamps/min_width", json!(600.0))],
            Code::InvalidField,
            "/size_clamps/width",
        );
        refused(
            &[("/nodes/1/auto_layout", flow_layout())],
            Code::InvalidField,
            "/constraints",
        );
        refused(
            &[("/nodes/2/constraints", constraint("MIN", "MIN"))],
            Code::InvalidField,
            "/constraints",
        );
        refused(
            &[("/nodes/4/child_layout", child_layout(0, "AUTO"))],
            Code::InvalidField,
            "/child_layout",
        );
        refused(
            &[("/nodes/2/child_layout/layout_grow", json!(2))],
            Code::InvalidField,
            "/layout_grow",
        );
        refused(
            &[("/nodes/4/node", node("layer", identity("SLYR", 2)))],
            Code::DuplicateIdentity,
            &identity("SLYR", 2),
        );
        refused(
            &[("/nodes/4/node", node("layer", identity("SLYR", 99)))],
            Code::DanglingReference,
            &identity("SLYR", 99),
        );
        refused(
            &[("/nodes/6/node", node("layer", identity("SART", 1)))],
            Code::DanglingReference,
            &identity("SART", 1),
        );
        refused(
            &[(
                &format!("{r0}/actions/0/destination_id"),
                json!(identity("SLYR", 99)),
            )],
            Code::DanglingReference,
            "/destination_id",
        );
        refused(
            &[(
                &format!("{r0}/actions/1/variable_value"),
                json!({"type": "FLOAT", "value": 1.0}),
            )],
            Code::InvalidField,
            "/variable_value",
        );
        refused(
            &[(
                &format!("{r0}/actions/1/variable_id"),
                json!(identity("SVAR", 99)),
            )],
            Code::DanglingReference,
            "/variable_id",
        );
        refused(
            &[(
                &format!("{r0}/trigger"),
                json!({"type": "KEYBOARD", "device": "KEYBOARD", "key_codes": []}),
            )],
            Code::InvalidField,
            "/trigger",
        );
        refused(
            &[(
                &format!("{r0}/trigger"),
                json!({"type": "AFTER_TIMEOUT", "timeout": 3_600_001}),
            )],
            Code::InvalidField,
            "/trigger",
        );
        refused(
            &[(
                &format!("{r0}/actions/0/transition"),
                json!({"type": "PUSH", "easing": easing(), "duration": 300}),
            )],
            Code::InvalidField,
            "/transition",
        );
        refused(
            &[(
                &format!("{r0}/actions/0/transition/easing"),
                json!({"easing_function_cubic_bezier": {"x1": 0.0, "y1": 0.0, "x2": 1.0, "y2": 1.0},
                    "easing_function_spring": {"mass": 1.0, "stiffness": 100.0, "damping": 15.0, "initial_velocity": 0.0}}),
            )],
            Code::InvalidField,
            "/transition/easing",
        );
        refused(
            &[(&format!("{r0}/actions/0/transition/easing"), json!({}))],
            Code::InvalidField,
            "/transition/easing",
        );
        refused(
            &[(
                "/nodes/2/component_property_references",
                json!({"visible": "showIcon#1"}),
            )],
            Code::InvalidField,
            "/component_property_references",
        );
        refused(
            &[(
                "/nodes/6/explicit_variable_modes",
                json!({identity("SVCL", 1): "missing"}),
            )],
            Code::DanglingReference,
            "/explicit_variable_modes",
        );
        refused(
            &[(
                "/nodes/2/explicit_variable_modes",
                json!({identity("SVCL", 1): "dark"}),
            )],
            Code::InvalidField,
            "/explicit_variable_modes",
        );
        // Closed unions: an unknown member, a stray member of a unit-like variant or an unsupported
        // action type does not parse.
        for (pointer, value) in [
            (
                format!("{r0}/trigger"),
                json!({"type": "ON_CLICK", "bogus": 1}),
            ),
            (format!("{r0}/trigger"), json!({"type": "GRIDLOCK"})),
            (
                format!("{r0}/actions/0"),
                json!({"type": "CONDITIONAL", "conditional_blocks": []}),
            ),
            (
                "/nodes/0/auto_layout/layout_wrap".to_owned(),
                json!("SOMETIMES"),
            ),
            ("/nodes/0/size_clamps".to_owned(), json!({"min_width": 1.0})),
        ] {
            let mut bad = v.clone();
            set(&mut bad, &pointer, value);
            assert!(
                serde_json::from_value::<StudioDesignRecords>(bad).is_err(),
                "{pointer}"
            );
        }
    });
}

#[test]
fn component_sets_instances_and_overrides_are_validated() {
    bounded_case("components", || {
        let id2 = identity("SCMP", 2);
        refused(
            &[("/components/1/variant_properties", json!(null))],
            Code::VariantAssignmentIncomplete,
            &identity("SCMP", 3),
        );
        refused(
            &[(
                "/component_sets/0/variant_group_properties",
                json!({"size": ["small", "large"], "tone": ["a", "b"]}),
            )],
            Code::VariantAssignmentIncomplete,
            &id2,
        );
        refused(
            &[("/components/1/variant_properties", json!({"size": "small"}))],
            Code::VariantAssignmentDuplicate,
            &identity("SCMP", 3),
        );
        refused(
            &[(
                "/components/0/variant_properties",
                json!({"size": "medium"}),
            )],
            Code::InvalidField,
            "/variant_properties",
        );
        refused(
            &[(
                "/components/0/variant_properties",
                json!({"size": "small", "tone": "a"}),
            )],
            Code::InvalidField,
            "/variant_properties",
        );
        refused(
            &[(
                "/component_sets/0/default_variant_id",
                json!(identity("SCMP", 9)),
            )],
            Code::DanglingReference,
            "/default_variant_id",
        );
        refused(
            &[(
                "/component_sets/0/variant_group_properties",
                json!({"size": ["small", "small"]}),
            )],
            Code::InvalidField,
            "/variant_group_properties",
        );
        refused(
            &[("/components/1/component_id", json!(id2))],
            Code::DuplicateIdentity,
            &id2,
        );
        refused(
            &[("/components/0/layer_id", json!(identity("SLYR", 2)))],
            Code::DanglingReference,
            "/layer_id",
        );
        refused(
            &[(
                "/components/0/component_property_definitions",
                json!([property(
                    "swap",
                    "INSTANCE_SWAP",
                    json!(identity("SCMP", 3))
                )]),
            )],
            Code::InvalidField,
            "/options",
        );
        refused(
            &[(
                "/components/0/component_property_definitions",
                json!([property("flag", "BOOLEAN", json!("yes"))]),
            )],
            Code::InvalidField,
            "/value",
        );
        refused(
            &[(
                "/components/0/component_property_definitions",
                json!([
                    property("a", "TEXT", json!("x")),
                    property("a", "TEXT", json!("y"))
                ]),
            )],
            Code::InvalidField,
            "/property_name",
        );
        // A variant assignment on a component that is not inside a set.
        refused(
            &[
                ("/components/0/layer_id", json!(identity("SLYR", 4))),
                (
                    "/component_sets/0/default_variant_id",
                    json!(identity("SCMP", 3)),
                ),
            ],
            Code::InvalidField,
            "/variant_properties",
        );

        let i0 = "/component_instances/0";
        refused(
            &[(
                &format!("{i0}/main_component_id"),
                json!(identity("SCMP", 1)),
            )],
            Code::InvalidField,
            "/main_component_id",
        );
        refused(
            &[(
                &format!("{i0}/main_component_id"),
                json!(identity("SCMP", 99)),
            )],
            Code::DanglingReference,
            "/main_component_id",
        );
        refused(
            &[(&format!("{i0}/scale_factor"), json!(0.001))],
            Code::InvalidField,
            "/scale_factor",
        );
        refused(
            &[(&format!("{i0}/scale_factor"), json!(100.5))],
            Code::InvalidField,
            "/scale_factor",
        );
        refused(
            &[(
                &format!("{i0}/overrides/0/overridden_fields"),
                json!(["characters", "name"]),
            )],
            Code::InvalidField,
            "/overridden_fields",
        );
        refused(
            &[(
                &format!("{i0}/overrides/0/overridden_fields"),
                json!(["visible", "visible"]),
            )],
            Code::InvalidField,
            "/overridden_fields",
        );
        refused(
            &[(
                &format!("{i0}/overrides/0/overridden_fields"),
                json!(["characters"]),
            )],
            Code::InvalidField,
            "/values",
        );
        refused(
            &[(
                &format!("{i0}/overrides"),
                json!([
                {"overridden_node_id": identity("SLYR", 13), "overridden_fields": ["visible"]},
                {"overridden_node_id": identity("SLYR", 13), "overridden_fields": ["opacity"]}]),
            )],
            Code::DuplicateIdentity,
            "/overridden_node_id",
        );
        refused(
            &[(
                &format!("{i0}/component_properties/showIcon/type"),
                json!("TEXT"),
            )],
            Code::InvalidField,
            "/component_properties/1/value",
        );
        refused(
            &[(
                &format!("{i0}/component_properties/showIcon"),
                json!({"type": "TEXT", "value": "x"}),
            )],
            Code::InvalidField,
            "/component_properties/1",
        );
        refused(
            &[(
                &format!("{i0}/component_properties"),
                json!({"missing": {"type": "TEXT", "value": "x"}}),
            )],
            Code::InvalidField,
            "/component_properties/0",
        );
        refused(
            &[(
                &format!("{i0}/component_properties/showIcon/bound_variable_id"),
                json!(identity("SVAR", 1)),
            )],
            Code::InvalidField,
            "/variable",
        );
        refused(
            &[(
                &format!("{i0}/component_properties/showIcon/bound_variable_id"),
                json!(identity("SVAR", 99)),
            )],
            Code::DanglingReference,
            "/variable",
        );
        // Deleted main (explicit null) is legal and the typed properties are then not resolvable.
        let mut deleted = records();
        set(
            &mut deleted,
            &format!("{i0}/main_component_id"),
            json!(null),
        );
        assert!(
            result(&deleted, &Caller(Resolution::Available))
                .unwrap()
                .is_empty()
        );
        let mut missing = records();
        remove(&mut missing, &format!("{i0}/main_component_id"));
        assert!(
            serde_json::from_value::<StudioDesignRecords>(missing).is_err(),
            "null must be explicit"
        );
    });
}

#[test]
fn variables_modes_aliases_and_publishing_are_validated() {
    bounded_case("variables", || {
        let v1 = identity("SVAR", 1);
        let mut gap = records();
        remove(&mut gap, "/variables/0/values_by_mode/dark");
        refused_with(&gap, Code::InvalidField, "/values_by_mode", "missing mode");
        let mut extra = records();
        set(
            &mut extra,
            "/variables/0/values_by_mode/night",
            colour_value(0.0),
        );
        refused_with(
            &extra,
            Code::InvalidField,
            "/values_by_mode",
            "unknown mode",
        );
        refused(
            &[("/variables/0/values_by_mode/light", alias(2, "COLOR"))],
            Code::VariableAliasCycle,
            "",
        );
        refused(
            &[("/variables/1/values_by_mode/light", alias(99, "COLOR"))],
            Code::DanglingReference,
            "/values_by_mode",
        );
        refused(
            &[("/variables/1/values_by_mode/light", alias(3, "COLOR"))],
            Code::InvalidField,
            "/values_by_mode/type",
        );
        refused(
            &[(
                "/variables/0/values_by_mode/light",
                json!({"type": "FLOAT", "value": 1.0}),
            )],
            Code::InvalidField,
            "/values_by_mode/type",
        );
        refused(
            &[(
                "/variables/0/values_by_mode/light",
                json!({"type": "COLOR", "value": rgba(1.5, 0.0, 0.0, 1.0)}),
            )],
            Code::InvalidField,
            "/values_by_mode/colour",
        );
        refused(
            &[(
                "/variables/0/code_syntax",
                json!({"WEB": "x", "WINDOWS": "y"}),
            )],
            Code::InvalidField,
            &format!("{v1}/code_syntax"),
        );
        refused(
            &[("/variables/0/scopes", json!(["EVERYTHING"]))],
            Code::InvalidField,
            "/scopes",
        );
        refused(
            &[("/variables/0/scopes", json!(["GAP", "GAP"]))],
            Code::InvalidField,
            "/scopes",
        );
        refused(
            &[("/variables/0/name", json!("color//bg"))],
            Code::InvalidField,
            "/name",
        );
        refused(
            &[(
                "/variables/0/variable_collection_id",
                json!(identity("SVCL", 9)),
            )],
            Code::DanglingReference,
            "/variable_collection_id",
        );
        refused(
            &[(
                "/variables/0/publishing/documentation_links",
                json!([{"name": "x", "url": ""}]),
            )],
            Code::InvalidField,
            "/publishing",
        );
        refused(
            &[(
                "/variable_collections/0/variable_ids",
                json!([identity("SVAR", 1), identity("SVAR", 2)]),
            )],
            Code::InvalidField,
            "/variable_ids",
        );
        refused(
            &[("/variable_collections/0/is_extension", json!(true))],
            Code::UnsupportedDescriptor,
            "/is_extension",
        );
        refused(
            &[("/variable_collections/0/default_mode_id", json!("night"))],
            Code::DanglingReference,
            "/default_mode_id",
        );
        refused(
            &[(
                "/variable_collections/0/modes",
                json!([mode("light", "Light"), mode("light", "Again")]),
            )],
            Code::InvalidField,
            "/modes",
        );
        refused(
            &[("/variable_collections/0/modes", json!([]))],
            Code::InvalidField,
            "/modes",
        );
        let mut twice = records();
        let second = twice["variables"][0].clone();
        twice["variables"].as_array_mut().unwrap().push(second);
        refused_with(&twice, Code::DuplicateIdentity, &v1, "duplicate variable");

        // Budgets and cancellation reject without a partial result.
        let v = records();
        let r: StudioDesignRecords = serde_json::from_value(v.clone()).unwrap();
        let snapshot = open_document();
        let small = Budget {
            nodes: 5,
            ..Budget::default()
        };
        let e = validate_design_records(
            &r,
            snapshot.document(),
            small,
            &CancellationToken::default(),
            &Caller(Resolution::Available),
        );
        assert_eq!(e.unwrap_err().code, Code::BudgetExceeded);
        let tiny = Budget {
            payload_bytes: 8,
            ..Budget::default()
        };
        let e = validate_design_records(
            &r,
            snapshot.document(),
            tiny,
            &CancellationToken::default(),
            &Caller(Resolution::Available),
        );
        assert_eq!(e.unwrap_err().code, Code::BudgetExceeded);
        let token = CancellationToken::default();
        token.cancel();
        let e = validate_design_records(
            &r,
            snapshot.document(),
            Budget::default(),
            &token,
            &Caller(Resolution::Available),
        );
        assert_eq!(e.unwrap_err().code, Code::Canceled);

        // A colour profile that the caller cannot resolve is reported, not refused.
        let mut profiled = v;
        set(
            &mut profiled,
            "/variables/0/values_by_mode/light/value/colour_profile_id",
            json!(identity("SCPF", 1)),
        );
        let issues = result(&profiled, &Caller(Resolution::Unauthorized)).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].target.ends_with("/colour_profile")
                && issues[0].disposition == Resolution::Unauthorized
        );
    });
}
