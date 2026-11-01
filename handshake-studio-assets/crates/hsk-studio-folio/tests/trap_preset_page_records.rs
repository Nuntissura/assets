//! Lane SMB-ID-TOC-PERSIST-1 item 2: the page's `applied_trap_preset` (STU-LAY-107) and the stored
//! trap presets it names (`hsk.studio.trap_preset_records@1`, STU-LAY-152 values by value). The
//! package stores `document.json` and record-set JSON verbatim, so the byte-identical round trips
//! below (the document through `inspect_bytes`, the Folio load path) are the package round trips.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
struct Caller;
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
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
fn page(key: &str, preset: Option<&str>) -> Value {
    let e = json!({"top": pt(36.0), "right": pt(36.0), "bottom": pt(36.0), "left": pt(36.0)});
    let mut v = json!({"page_id": key, "width": pt(612.0), "height": pt(792.0), "side": "right_hand", "margins": e, "bleed": null});
    if let Some(p) = preset {
        v["applied_trap_preset"] = json!(p);
    }
    v
}
fn document(pages: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],
        "page_spreads":[{"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",1),
            "name":"spread 1","parent":null,
            "bounds":{"x":pt(0.0),"y":pt(0.0),"width":pt(1836.0),"height":pt(792.0)},
            "pages":pages}],
        "layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}})
}
fn inspect(v: &Value) -> Result<Snapshot, Diagnostic> {
    match inspect_bytes(&serde_json::to_vec(v).unwrap(), Budget::default(), &CancellationToken::default(), &Caller)? {
        Inspection::Editable(s) => Ok(s),
        Inspection::ReadOnly(r) => Err(r.diagnostic().clone()),
    }
}
fn presets() -> Value {
    json!({"schema_id": "hsk.studio.trap_preset_records@1", "document_id": identity("SDOC", 1),
        "trap_presets": [
            {"name": "Press A", "default_trap_width": 0.25, "black_width": 0.5, "black_color_threshold": 100.0,
                "black_density": 1.6, "sliding_trap_threshold": 70.0, "step_threshold": 10.0, "color_reduction": 100.0,
                "trap_end_type": "overlap", "trap_image_placement": "images_over_spread"},
            {"name": "Unset values"}
        ]})
}
fn check(records: &Value, doc: &Value) -> Result<(), Diagnostic> {
    let r: StudioTrapPresetRecords = serde_json::from_value(records.clone()).expect("record shape");
    let d = inspect(doc).expect("document").document().clone();
    validate_trap_preset_records(&r, &d, &CancellationToken::default())
}

/// A page with an applied trap preset round-trips through the Folio load path byte-identically;
/// a page range assignment is the same reference on each page.
#[test]
fn page_applied_trap_preset_round_trips() {
    let v = document(vec![page("p1", Some("Press A")), page("p2", Some("Press A")), page("p3", None)]);
    let snap = inspect(&v).expect("valid");
    let pages = &snap.document().page_spreads[0].pages;
    assert_eq!(pages[0].applied_trap_preset, Some(StudioTrapPresetRef("Press A".into())));
    assert_eq!(pages[1].applied_trap_preset, pages[0].applied_trap_preset);
    assert_eq!(pages[2].applied_trap_preset, None);
    let bytes = snap.encoded_bytes().to_vec();
    let again = inspect(&serde_json::from_slice(&bytes).unwrap()).expect("re-open");
    assert_eq!(again.encoded_bytes(), bytes.as_slice(), "byte-identical re-save");
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["page_spreads"][0]["pages"][1]["applied_trap_preset"], json!("Press A"));
    assert!(check(&presets(), &v).is_ok());
}

/// A page written before the member existed loads with no preset applied and re-encodes without it.
#[test]
fn old_shape_page_loads_without_trap_preset() {
    let v = document(vec![page("p1", None)]);
    let snap = inspect(&v).expect("valid");
    assert_eq!(snap.document().page_spreads[0].pages[0].applied_trap_preset, None);
    let wire: Value = serde_json::from_slice(snap.encoded_bytes()).unwrap();
    assert_eq!(wire["page_spreads"][0]["pages"][0], page("p1", None), "nothing new is encoded");
    // The empty preset set is one line of wire.
    let empty = StudioTrapPresetRecords::new(identity("SDOC", 1));
    assert_eq!(serde_json::to_string(&empty).unwrap(), format!(r#"{{"schema_id":"hsk.studio.trap_preset_records@1","document_id":"{}"}}"#, identity("SDOC", 1)));
}

/// Stored trap presets: every member round-trips byte-identically with its kind; the records refuse
/// dangling page references, duplicate or invalid names, non-finite values and other documents.
#[test]
fn trap_preset_records_round_trip_and_refusals() {
    let r: StudioTrapPresetRecords = serde_json::from_value(presets()).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), presets(), "every member encoded as written, unset ones absent");
    let back: StudioTrapPresetRecords = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), bytes);
    let a = &r.trap_presets[0];
    assert_eq!((a.trap_end_type, a.trap_image_placement), (Some(StudioTrapEnd::Overlap), Some(StudioTrapImagePlacement::ImagesOverSpread)));
    assert_eq!(r.preset(&StudioTrapPresetRef("Press A".into())).map(|p| p.black_density), Some(Some(1.6)));
    assert_eq!(r.trap_presets[1].default_trap_width, None, "unset is never filled");
    let doc = document(vec![page("p1", Some("Press A")), page("p2", Some("Nowhere"))]);
    let e = check(&presets(), &doc).unwrap_err();
    assert_eq!((e.code, e.target.as_str()), (Code::DanglingReference, "p2/applied_trap_preset"));
    let ok_doc = document(vec![page("p1", Some("Unset values"))]);
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = presets();
        f(&mut v);
        check(&v, &ok_doc).unwrap_err()
    };
    assert_eq!(with(&|v| v["trap_presets"][1]["name"] = json!("Press A")).code, Code::DuplicateIdentity);
    assert_eq!(with(&|v| v["trap_presets"][1]["name"] = json!("  ")).target, "trap_presets/1/name");
    assert_eq!(with(&|v| v["trap_presets"][1]["name"] = json!("x".repeat(MAX_TRAP_PRESET_NAME_BYTES + 1))).code, Code::InvalidField);
    assert_eq!(with(&|v| v["document_id"] = json!(identity("SDOC", 2))).target, "document_id");
    let many: Vec<Value> = (0..=MAX_TRAP_PRESETS).map(|k| json!({"name": format!("p{k}")})).collect();
    assert_eq!(with(&|v| v["trap_presets"] = json!(many)).code, Code::BudgetExceeded);
    let mut nan: StudioTrapPresetRecords = serde_json::from_value(presets()).unwrap();
    nan.trap_presets[0].black_density = Some(f64::NAN);
    let d = inspect(&ok_doc).unwrap().document().clone();
    let e = validate_trap_preset_records(&nan, &d, &CancellationToken::default()).unwrap_err();
    assert_eq!((e.code, e.target.as_str()), (Code::InvalidField, "trap_presets/0/black_density"));
    // A page's reference itself must be a valid preset name (document validation).
    let bad = inspect(&document(vec![page("p1", Some(" "))])).unwrap_err();
    assert_eq!((bad.code, bad.target.as_str()), (Code::InvalidField, "p1/applied_trap_preset"));
    // Unknown members and members outside the enumerations are refused.
    for f in [
        (|v: &mut Value| v["trap_presets"][0]["surprise"] = json!(1)) as fn(&mut Value),
        |v| v["trap_presets"][0]["trap_end_type"] = json!("trap_end.miter"),
        |v| v["trap_presets"][0]["trap_image_placement"] = json!("spread"),
    ] {
        let mut v = presets();
        f(&mut v);
        assert!(serde_json::from_value::<StudioTrapPresetRecords>(v).is_err());
    }
    assert!(serde_json::to_string(&trap_preset_records_schema()).unwrap().contains("trap_image_placement"));
}
