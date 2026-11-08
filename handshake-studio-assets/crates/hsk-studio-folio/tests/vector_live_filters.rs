//! Lane SMB-AI-LIVE-EFFECTS-1: object-scope live filter stacks in `StudioVectorRecords::live_filters`
//! (STU-FX-002 `StudioLiveFilter` entries: `instance_id`, `filter_kind`, `version`, `enabled`, typed
//! `params`; STU-VEC-154 typed map `{type, key, value}` with type exactly Int | Real | Bool | String;
//! STU-FX-003 list order = application order). Absent members encode byte-identically; a full set
//! round-trips with exact reals; hostile records are refused with the code of the rule named next to
//! them. Folio checks shape only: an effect id or parameter Folio cannot see (nib's laws) still loads.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn layer(n: u32, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("l{n}"),
        "parent":null,"kind":kind,"payload":payload,"visible":true})
}
/// Layers 1 and 2 vector, 3 raster, 4 group.
fn document() -> StudioDocument {
    let v = |n: u32| json!({"payload_kind":"vector","path_ids":[identity("SVPT",n)]});
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1,"vector",v(1)),layer(2,"vector",v(2)),layer(3,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(4,"group",json!({"payload_kind":"group"}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn param(ty: &str, key: &str, value: Value) -> Value {
    json!({"type":ty,"key":key,"value":value})
}
fn filter(id: &str, kind: &str, params: Vec<Value>) -> Value {
    json!({"instance_id":id,"filter_kind":kind,"version":1,"enabled":true,"params":params})
}
fn full() -> Value {
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "live_filters":[
            {"layer_id":identity("SLYR",1),"filters":[
                filter("rough", "hsk.studio.vector_effect.roughen", vec![
                    param("String","roughen_mode",json!("mode.absolute")),
                    param("Real","roughen_size_absolute",json!(0.1)),
                    param("Real","roughen_detail",json!(-1.0e-300)),
                    param("String","roughen_points",json!("points.corner")),
                    param("Int","seed",json!(-9_007_199_254_740_993_i64))]),
                {"instance_id":"twist-2","filter_kind":"hsk.studio.vector_effect.twist","version":1,"enabled":false,
                 "params":[param("Real","twist_angle",json!(123_456.789_012_345))]}]},
            {"layer_id":identity("SLYR",2),"filters":[
                filter("step", "hsk.studio.vector_effect.step_repeat", vec![
                    param("Int","copies",json!(3)),
                    param("Bool","randomize",json!(true)),
                    param("Real","move_horizontal",json!(1.0e308))])]}]})
}
fn decode(v: &Value) -> Result<StudioVectorRecords, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}
/// `Err(UnknownField)` stands for a record serde refuses (the wire shape).
fn check(v: &Value) -> Result<(), Code> {
    let r = decode(v).map_err(|_| Code::UnknownField)?;
    validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}
fn edit(path: &[&str], v: Value) -> Value {
    let mut x = full();
    let mut at = &mut x;
    for k in &path[..path.len() - 1] {
        at = match k.parse::<usize>() {
            Ok(i) => &mut at[i],
            Err(_) => &mut at[*k],
        };
    }
    let last = path[path.len() - 1];
    match last.parse::<usize>() {
        Ok(i) => at[i] = v,
        Err(_) => at[last] = v,
    }
    x
}
fn bad(path: &[&str], v: Value) -> Result<(), Code> {
    check(&edit(path, v))
}

#[test]
fn live_filter_records_round_trip_exactly_and_absent_members_are_byte_identical() {
    // Absent member: the bytes of a record set written before it.
    let before = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    assert_eq!(serde_json::to_string(&StudioVectorRecords::new(identity("SDOC", 1))).unwrap(), before);
    // The full set validates and round-trips value for value, reals bit for bit.
    let f = full();
    assert_eq!(check(&f), Ok(()));
    let r = decode(&f).unwrap();
    assert_eq!(serde_json::to_value(&r).unwrap(), f);
    let text = serde_json::to_string(&r).unwrap();
    let again: StudioVectorRecords = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_string(&again).unwrap(), text);
    let again = serde_json::to_value(&again).unwrap();
    for (got, want) in [(&again["live_filters"], &f["live_filters"])] {
        let reals = |v: &Value| -> Vec<u64> {
            let mut out = Vec::new();
            for s in v.as_array().unwrap() {
                for e in s["filters"].as_array().unwrap() {
                    for p in e["params"].as_array().unwrap() {
                        if p["type"] == "Real" {
                            out.push(p["value"].as_f64().unwrap().to_bits());
                        }
                    }
                }
            }
            out
        };
        assert_eq!(reals(got), reals(want));
        assert_eq!(reals(want).len(), 4);
    }
}

#[test]
fn the_parameter_type_vocabulary_is_closed() {
    let p = ["live_filters", "0", "filters", "0", "params", "1"];
    // STU-VEC-154: exactly Int | Real | Bool | String, and the value must be of that type.
    for (case, v) in [
        ("unknown type token", param("Float", "roughen_size_absolute", json!(1.0))),
        ("lower-case token", param("real", "roughen_size_absolute", json!(1.0))),
        ("no type", json!({"key":"roughen_size_absolute","value":1.0})),
        ("Int with a fraction", param("Int", "roughen_size_absolute", json!(1.5))),
        ("Bool as text", param("Bool", "roughen_size_absolute", json!("true"))),
        ("String as a number", param("String", "roughen_size_absolute", json!(3))),
        ("Real as text", param("Real", "roughen_size_absolute", json!("1.0"))),
        ("no value", json!({"type":"Real","key":"roughen_size_absolute"})),
        ("extra field", json!({"type":"Real","key":"roughen_size_absolute","value":1.0,"unit":"pt"})),
    ] {
        assert_eq!(bad(&p, v), Err(Code::UnknownField), "{case}");
    }
    // An integer literal is a valid Real (JSON has one number type).
    assert_eq!(bad(&p, param("Real", "roughen_size_absolute", json!(2))), Ok(()));
    assert_eq!(bad(&["live_filters", "0", "filters", "0", "colour"], json!("red")), Err(Code::UnknownField), "unknown entry field");
    assert_eq!(bad(&["live_filters", "0", "mask"], json!(null)), Err(Code::UnknownField), "unknown stack field");
}

#[test]
fn hostile_stacks_are_refused_with_the_rule_code() {
    let f0 = ["live_filters", "0", "filters", "0"];
    let at = |rest: &[&'static str]| -> Vec<&'static str> { f0.iter().copied().chain(rest.iter().copied()).collect() };
    assert_eq!(bad(&["live_filters", "0", "layer_id"], json!(identity("SLYR", 3))), Err(Code::PayloadMismatch), "vector layers only (raster)");
    assert_eq!(bad(&["live_filters", "0", "layer_id"], json!(identity("SLYR", 4))), Err(Code::PayloadMismatch), "group target ring is out of this slice");
    assert_eq!(bad(&["live_filters", "0", "layer_id"], json!(identity("SLYR", 9))), Err(Code::DanglingReference));
    assert_eq!(bad(&["live_filters", "0", "layer_id"], json!("layer-1")), Err(Code::InvalidId));
    assert_eq!(bad(&["live_filters", "1", "layer_id"], json!(identity("SLYR", 1))), Err(Code::DuplicateIdentity), "one stack per layer");
    assert_eq!(bad(&at(&["instance_id"]), json!("twist-2")), Err(Code::DuplicateIdentity), "instance ids are unique in a stack");
    assert_eq!(bad(&at(&["instance_id"]), json!("two words")), Err(Code::InvalidId));
    assert_eq!(bad(&at(&["instance_id"]), json!("")), Err(Code::InvalidId));
    assert_eq!(bad(&at(&["filter_kind"]), json!("")), Err(Code::InvalidField));
    assert_eq!(bad(&at(&["filter_kind"]), json!("x".repeat(129))), Err(Code::InvalidField));
    assert_eq!(bad(&at(&["filter_kind"]), json!("hsk.studio.vector_effect.rough\u{7}en")), Err(Code::InvalidField));
    assert_eq!(bad(&at(&["params", "1", "key"]), json!("roughen size")), Err(Code::InvalidField), "key grammar");
    assert_eq!(bad(&at(&["params", "1", "key"]), json!("roughen_mode")), Err(Code::DuplicateIdentity), "one value per key");
    assert_eq!(bad(&at(&["params", "0", "value"]), json!("m".repeat(257))), Err(Code::InvalidField), "string length");
    assert_eq!(bad(&at(&["params", "0", "value"]), json!("mode.\nabsolute")), Err(Code::InvalidField), "control characters");
    assert_eq!(bad(&at(&["params", "0", "value"]), json!("")), Err(Code::InvalidField));
    // Folio cannot see nib's laws: an effect, version or parameter a later law set may define loads,
    // and composite refuses it typed (tests/vector_live_filters.rs in hsk-studio-composite).
    assert_eq!(bad(&at(&["filter_kind"]), json!("hsk.studio.vector_effect.scribble")), Ok(()));
    assert_eq!(bad(&at(&["version"]), json!(99)), Ok(()));
    assert_eq!(bad(&at(&["params", "1", "key"]), json!("roughen_future_knob")), Ok(()));
    assert_eq!(bad(&at(&["params", "1", "value"]), json!(-1.0e308)), Ok(()), "finite reals of any size: nib owns the bounds");
}

#[test]
fn counts_are_checked_before_any_entry_is_examined() {
    // More stacks than TOOLS_MAX_ENTRIES: refused on the count (the entries would also be duplicates).
    let mut many = full();
    many["live_filters"] = Value::Array(vec![json!({"layer_id":identity("SLYR",1),"filters":[]}); TOOLS_MAX_ENTRIES + 1]);
    assert_eq!(check(&many), Err(Code::BudgetExceeded));
    // Filters per stack and parameters per filter: the declared engine limits, exactly at and over.
    let filters = |n: usize| -> Value { Value::Array((0..n).map(|i| filter(&format!("f{i}"), "hsk.studio.vector_effect.twist", vec![param("Real", "twist_angle", json!(1.0))])).collect()) };
    assert_eq!(bad(&["live_filters", "0", "filters"], filters(64)), Ok(()));
    assert_eq!(bad(&["live_filters", "0", "filters"], filters(65)), Err(Code::BudgetExceeded));
    let params = |n: usize| -> Value { Value::Array((0..n).map(|i| param("Real", &format!("k{i}"), json!(1.0))).collect()) };
    assert_eq!(bad(&["live_filters", "0", "filters", "0", "params"], params(32)), Ok(()));
    assert_eq!(bad(&["live_filters", "0", "filters", "0", "params"], params(33)), Err(Code::BudgetExceeded));
    // A stack with no filter is a valid, empty stack (it draws like no stack).
    assert_eq!(bad(&["live_filters", "0", "filters"], json!([])), Ok(()));
}

#[test]
fn non_finite_reals_from_a_caller_are_refused() {
    // JSON cannot carry NaN or infinity; a caller building records in memory can.
    for bad_value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut r = decode(&full()).unwrap();
        r.live_filters[0].filters[0].params[1] = StudioLiveFilterParam::Real { key: "roughen_size_absolute".into(), value: bad_value };
        let got = validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code);
        assert_eq!(got, Err(Code::InvalidField), "{bad_value}");
    }
    let r = decode(&full()).unwrap();
    assert_eq!(r.live_filter_stack(&identity("SLYR", 2)).map(|s| (s.filters.len(), s.any_enabled())), Some((1, true)));
    assert_eq!(r.live_filter_stack(&identity("SLYR", 1)).map(|s| s.filters[1].enabled), Some(false));
    assert_eq!(r.live_filters[0].filters[0].params[0].key(), "roughen_mode");
}
