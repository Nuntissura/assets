//! Lane SMB-COL-SWATCH-GROUPS-1, MT-31240 (STU-COL-178): palette scopes document | application |
//! system, resolution in document -> application -> system order, unresolvable references as
//! validation errors, the read-only system palette and document-only storage.
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn document(n: u32) -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",n),"revision":3,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn design(n: u32) -> StudioDesignRecords {
    let colour = json!({"r": 0.5, "g": 0.25, "b": 1.0, "colour_profile_id": null});
    let fill = |_: u32| json!({"row_kind":"fill","paint":{"paint_kind":"solid","colour":colour},"opacity":100.0,"blend_mode":2,"visible":true});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",n),
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT","name":"s1",
        "publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":"s1","is_remote":false},"rows":[fill(0), fill(1), fill(2)]}]}))
    .expect("design shape")
}
fn swatch(n: u32, name: &str, rgb: [f64; 3]) -> Value {
    json!({"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",n),"name":name,"swatch_kind":"process","color_model":"RGB","components":rgb})
}
fn palette(id: &str, scope: &str, swatches: Vec<Value>) -> StudioSwatchPalette {
    serde_json::from_value(json!({"schema_id":"hsk.studio.swatch_palette@1","palette_id":id,"name":id,"scope":scope,"swatches":swatches})).expect("palette shape")
}
/// Document `n` binds rows 0, 1, 2 of style 1 to swatches 1, 2 and 3.
fn doc_records(n: u32, own: Vec<Value>) -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",n),"swatches":own,
        "swatch_paints":[{"style_id":identity("SSTY",1),"row":0,"swatch_id":identity("SSWT",1)},
                         {"style_id":identity("SSTY",1),"row":1,"swatch_id":identity("SSWT",2)},
                         {"style_id":identity("SSTY",1),"row":2,"swatch_id":identity("SSWT",3)}]}))
    .expect("records shape")
}
fn app() -> StudioSwatchPalette {
    palette("app", "application", vec![swatch(1, "App one", [0.0, 0.0, 1.0]), swatch(2, "App two", [0.0, 1.0, 0.0])])
}
fn sys() -> StudioSwatchPalette {
    palette("os", "system", vec![swatch(2, "OS two", [1.0, 1.0, 0.0]), swatch(3, "OS three", [1.0, 0.0, 0.0])])
}
fn validate(r: &StudioVectorRecords, n: u32, p: &StudioHostPalettes<'_>) -> Result<(), Code> {
    validate_vector_records_with_palettes(r, &document(n), Some(&design(n)), p, Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}

#[test]
fn references_resolve_document_then_application_then_system() {
    let (a, s) = (app(), sys());
    let host = StudioHostPalettes::new(Some(&a), Some(&s)).unwrap();
    // Document 1 owns swatch 1 (shadows the application's), the rest falls through.
    let d1 = doc_records(1, vec![swatch(1, "Doc one", [0.5, 0.5, 0.5])]);
    // Document 2 owns nothing: 1 and 2 from the application, 3 from the system.
    let d2 = doc_records(2, vec![]);
    let names = |r: &StudioVectorRecords| -> Vec<(StudioPaletteScope, String)> {
        (1..=3).map(|n| host.resolve(r, &identity("SSWT", n)).map(|x| (x.scope, x.swatch.name.clone())).unwrap()).collect()
    };
    use StudioPaletteScope::*;
    assert_eq!(names(&d1), vec![(Document, "Doc one".into()), (Application, "App two".into()), (System, "OS three".into())]);
    assert_eq!(names(&d2), vec![(Application, "App one".into()), (Application, "App two".into()), (System, "OS three".into())]);
    assert_eq!(StudioPaletteScope::RESOLUTION_ORDER, [Document, Application, System]);
    assert_eq!(validate(&d1, 1, &host), Ok(()));
    assert_eq!(validate(&d2, 2, &host), Ok(()));
    // The document palette travels with the document: its swatches round-trip, the host's do not.
    let wire = serde_json::to_string(&d1).unwrap();
    let back: StudioVectorRecords = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, d1);
    assert!(!wire.contains("App one") && !wire.contains("OS three"), "only document-scope swatches are stored");
    // A restored provider entry resolves the retained original reference again.
    let s_without = palette("os", "system", vec![swatch(2, "OS two", [1.0, 1.0, 0.0])]);
    let gone = StudioHostPalettes::new(Some(&a), Some(&s_without)).unwrap();
    assert_eq!(gone.resolve(&d2, &identity("SSWT", 3)).unwrap_err(), StudioPaletteError::Unresolvable);
    assert_eq!(host.resolve(&back, &identity("SSWT", 3)).map(|x| x.scope), Ok(System));
}

#[test]
fn unresolvable_references_are_validation_errors_never_a_fallback_colour() {
    let a = app();
    let only_app = StudioHostPalettes::new(Some(&a), None).unwrap();
    let d2 = doc_records(2, vec![]);
    assert_eq!(only_app.resolve(&d2, &identity("SSWT", 3)).unwrap_err().code(), "swatch_unresolvable");
    assert_eq!(validate(&d2, 2, &only_app), Err(Code::DanglingReference));
    // Without host palettes only the document scope exists.
    assert_eq!(validate_vector_records(&d2, &document(2), Some(&design(2)), Budget::default(), &CancellationToken::default()).map_err(|d| d.code), Err(Code::DanglingReference));
    // A palette in the wrong slot is refused; a host palette with a bad swatch fails validation.
    let s = sys();
    assert_eq!(StudioHostPalettes::new(Some(&s), None).unwrap_err(), StudioPaletteError::WrongScope { slot: StudioPaletteScope::Application, found: StudioPaletteScope::System });
    assert_eq!(StudioHostPalettes::new(None, Some(&a)).unwrap_err().code(), "palette_wrong_scope");
    let bad = palette("app", "application", vec![swatch(1, "Bad", [2.0, 0.0, 0.0])]);
    let host = StudioHostPalettes::new(Some(&bad), Some(&s)).unwrap();
    assert_eq!(validate(&d2, 2, &host), Err(Code::InvalidField));
    assert_eq!(validate_swatch_palette(&bad, Budget::default(), &CancellationToken::default()).map_err(|d| d.code), Err(Code::InvalidField));
    // A document cannot carry another scope: neither a scope field nor a palette member exists.
    let mut v = serde_json::to_value(doc_records(1, vec![swatch(1, "Doc one", [0.5, 0.5, 0.5])])).unwrap();
    v["swatches"][0]["scope"] = json!("application");
    assert!(serde_json::from_value::<StudioVectorRecords>(v.clone()).is_err());
    v["swatches"][0].as_object_mut().unwrap().remove("scope");
    v["palettes"] = json!([{"scope":"application"}]);
    assert!(serde_json::from_value::<StudioVectorRecords>(v).is_err());
}

#[test]
fn system_palette_is_read_only_and_application_palette_is_editable() {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let ctx = |c: &str| CommandContext::new(actor.clone(), ClientSessionId::parse("w").unwrap(), Some(CommandId::parse(c).unwrap()), 1, CorrelationId::new(9), None);
    let s = sys();
    let cmd = StudioSwatchGroupCommand::CreateRoot { group_id: "root".into(), name: "OS".into() };
    assert_eq!(apply_swatch_group_command(&s, &ctx(cmd.command_id()), 1, &cmd).unwrap_err(), StudioSwatchGroupError::ReadOnlyScope);
    let mv = StudioSwatchGroupCommand::MoveSwatch { swatch_id: identity("SSWT", 2), group_id: "root".into(), index: None };
    assert_eq!(apply_swatch_group_command(&s, &ctx(mv.command_id()), 1, &mv).unwrap_err().code(), "read_only_scope");
    assert_eq!(s, sys(), "the system palette is unchanged");
    assert!(StudioPaletteScope::System.read_only() && !StudioPaletteScope::Application.read_only() && !StudioPaletteScope::Document.read_only());
    let a = app();
    let o = apply_swatch_group_command(&a, &ctx(cmd.command_id()), 1, &cmd).unwrap();
    assert_eq!((o.palette.scope, o.palette.swatch_groups.len(), o.report.scope), (StudioPaletteScope::Application, 1, StudioPaletteScope::Application));
    assert_eq!(validate_swatch_palette(&o.palette, Budget::default(), &CancellationToken::default()), Ok(()));
    // Round trip of a host palette.
    let wire = serde_json::to_value(&o.palette).unwrap();
    assert_eq!(wire["schema_id"], "hsk.studio.swatch_palette@1");
    assert_eq!(serde_json::from_value::<StudioSwatchPalette>(wire).unwrap(), o.palette);
}

#[test]
fn host_palettes_are_budgeted() {
    let many: Vec<Value> = (0..=PALETTE_SWATCHES_MAX as u32).map(|n| swatch(n + 10, "S", [0.0, 0.0, 0.0])).collect();
    let p = palette("app", "application", many);
    assert_eq!(validate_swatch_palette(&p, Budget { nodes: usize::MAX, ..Budget::default() }, &CancellationToken::default()).map_err(|d| d.code), Err(Code::BudgetExceeded), "count over the palette bound");
    let a = app();
    assert_eq!(validate_swatch_palette(&a, Budget { nodes: 1, ..Budget::default() }, &CancellationToken::default()).map_err(|d| d.code), Err(Code::BudgetExceeded), "charged against Budget::nodes");
}
