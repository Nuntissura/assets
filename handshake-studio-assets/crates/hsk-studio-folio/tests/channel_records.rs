//! Persistent channel records (lane SMB-FOLIO-CHANNEL-RECORD; STU-RAS-022/031/144; MT-038 AC-002,
//! MT-050 AC-005): `StudioRasterRecords.channels` holds alpha and spot channels as one record, the
//! record validates its ids, names, kinds, depth and full grid coverage of the document raster, and
//! a record set without channels re-serialises byte-identically.
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
        true
    }
}
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}}))
    .expect("document shape")
}
fn tile(channel: &str, column: i64, row: i64, edge: f64, format: &str) -> Value {
    json!({"object_key":format!("c_{column}_{row}"),"layer_id":channel,"column":column,"row":row,
        "width":{"value":edge,"unit":"px"},"height":{"value":edge,"unit":"px"},"format":format,
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":format!("{:064x}", column * 16 + row)},"artifact_manifest_id":"m"})
}
/// A 300 x 200 channel on a 256-pixel grid: cells (0,0) and (1,0).
fn channel(id: &str, kind: &str, depth: u8) -> Value {
    let format = channel_tile_format(depth).unwrap_or("coverage_u8");
    json!({"channel_id":id,"name":"Alpha 1","kind":kind,"visible":false,
        "colour":{"model":"rgb","r":1.0,"g":0.0,"b":0.0},"opacity":50.0,"depth":depth,"width":300,"height":200,
        "tiles":[tile(id,0,0,256.0,format),tile(id,1,0,256.0,format)]})
}
fn records(channels: Vec<Value>) -> StudioRasterRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "channels":channels}))
    .expect("record shape")
}
fn check(channels: Vec<Value>) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    validate_raster_records(&records(channels), &document(), Budget::default(), &CancellationToken::default(), &Caller)
}
fn refused(channels: Vec<Value>) -> Diagnostic {
    check(channels).expect_err("must be refused")
}
fn with(mut v: Value, f: impl FnOnce(&mut Value)) -> Value {
    f(&mut v);
    v
}

#[test]
fn record_sets_without_channels_reserialise_byte_identically() {
    // A record set written before this lane (no `channels` member) is the same bytes after a
    // decode/encode round trip, so every earlier record digest is unchanged.
    let before = format!(
        r#"{{"schema_id":"hsk.studio.raster_records@1","document_id":"{}","global_light":{{"angle":120.0,"altitude":30.0}},"layers":[{{"layer_id":"{}","mask":{{"density":50.0,"feather":0.0,"inverted":false}}}}]}}"#,
        identity("SDOC", 1),
        identity("SLYR", 2)
    );
    let parsed: StudioRasterRecords = serde_json::from_str(&before).expect("old shape still decodes");
    assert!(parsed.channels.is_empty());
    assert_eq!(serde_json::to_string(&parsed).unwrap(), before);
    assert!(!serde_json::to_string(&StudioRasterRecords::new(identity("SDOC", 1))).unwrap().contains("channels"));
    // A set with channels round-trips exactly too.
    let with_channels = records(vec![channel("alpha-1", "masked_area_alpha", 16), channel("spot-1", "spot_colour", 8)]);
    let bytes = serde_json::to_vec(&with_channels).unwrap();
    let again: StudioRasterRecords = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(again, with_channels);
    assert_eq!(serde_json::to_vec(&again).unwrap(), bytes);
    assert_eq!(again.channel("spot-1").map(|c| c.kind), Some(StudioChannelKind::SpotColour));
}

#[test]
fn alpha_and_spot_channels_validate_with_and_without_a_document() {
    for (kind, depth) in [("masked_area_alpha", 8), ("selected_area_alpha", 16), ("spot_colour", 32)] {
        assert_eq!(check(vec![channel("c", kind, depth)]).unwrap_or_else(|d| panic!("{kind}/{depth}: {d:?}")), vec![]);
    }
    let set = records(vec![channel("a", "masked_area_alpha", 8), channel("b", "selected_area_alpha", 8)]);
    assert_eq!(validate_channels(&set.channels, Budget::default(), &CancellationToken::default(), &Caller), Ok(vec![]));
    // An unresolved tile is a resolution issue, not a refusal.
    let issues = validate_channels(&set.channels, Budget::default(), &CancellationToken::default(), &Unresolved).unwrap();
    assert_eq!(issues.len(), 4);
    assert!(issues.iter().all(|i| i.disposition == Resolution::Unsupported && i.target.starts_with("channel/")));
    // Kind values are the STU-RAS-144 enumeration; component is never stored.
    assert_eq!(
        [StudioChannelKind::MaskedAreaAlpha, StudioChannelKind::SelectedAreaAlpha, StudioChannelKind::SpotColour]
            .map(StudioChannelKind::spec_value),
        [2, 3, 4]
    );
    let refused_kind = serde_json::from_value::<StudioRasterRecords>(json!({"schema_id":"hsk.studio.raster_records@1",
        "document_id":identity("SDOC",1),"channels":[channel("c","component",8)]}));
    assert!(refused_kind.is_err(), "the component kind is derived and never stored");
    let schema = serde_json::to_string(&raster_records_schema()).unwrap();
    assert!(schema.contains("masked_area_alpha") && schema.contains("spot_colour") && !schema.contains("\"component\""));
}

#[test]
fn channel_refusals_are_typed() {
    let ok = || channel("c", "masked_area_alpha", 8);
    // Duplicate id.
    assert_eq!(refused(vec![ok(), ok()]).code, Code::DuplicateIdentity);
    // Id outside the key grammar.
    assert_eq!(refused(vec![with(ok(), |v| v["channel_id"] = json!("bad id"))]).code, Code::InvalidId);
    // Bad depth, and a tile format that does not match the depth.
    assert_eq!(refused(vec![with(ok(), |v| v["depth"] = json!(12))]).code, Code::InvalidColourDepth);
    assert_eq!(refused(vec![with(ok(), |v| v["depth"] = json!(16))]).code, Code::InvalidColourDepth);
    // Names: empty, too long, control character.
    for name in [String::new(), "n".repeat(MAX_CHANNEL_NAME_BYTES + 1), "a\u{7}".to_string()] {
        let d = refused(vec![with(ok(), |v| v["name"] = json!(name))]);
        assert_eq!((d.code, d.target.as_str()), (Code::InvalidField, "c/name"));
    }
    assert!(check(vec![with(ok(), |v| v["name"] = json!("n".repeat(MAX_CHANNEL_NAME_BYTES)))]).is_ok());
    // Opacity and colour ranges.
    assert_eq!(refused(vec![with(ok(), |v| v["opacity"] = json!(100.5))]).target, "c/opacity");
    assert_eq!(refused(vec![with(ok(), |v| v["colour"] = json!({"model":"lab","l":50.0,"a":200.0,"b":0.0}))]).target, "c/colour");
    // Tiles that do not cover the document raster: a missing cell, a repeated cell, a cell outside
    // the grid, mixed tile sizes, no tiles.
    let missing = with(ok(), |v| v["tiles"].as_array_mut().unwrap().truncate(1));
    assert_eq!(refused(vec![missing]).target, "c/tiles");
    let repeated = with(ok(), |v| v["tiles"][1] = with(tile("c", 0, 0, 256.0, "coverage_u8"), |t| t["object_key"] = json!("x")));
    assert_eq!(refused(vec![repeated]).target, "c/tiles");
    let outside = with(ok(), |v| v["tiles"][1] = tile("c", 2, 0, 256.0, "coverage_u8"));
    assert_eq!(refused(vec![outside]).target, "c/tiles");
    let negative = with(ok(), |v| v["tiles"][1] = tile("c", -1, 0, 256.0, "coverage_u8"));
    assert_eq!(refused(vec![negative]).target, "c/tiles");
    let mixed = with(ok(), |v| v["tiles"][1] = tile("c", 1, 0, 128.0, "coverage_u8"));
    assert_eq!(refused(vec![mixed]).target, "c/tiles");
    let none = with(ok(), |v| v["tiles"] = json!([]));
    assert_eq!(refused(vec![none]).target, "c/tiles");
    // A tile that names another channel.
    let foreign = with(ok(), |v| v["tiles"][1]["layer_id"] = json!("other"));
    assert_eq!(refused(vec![foreign]).code, Code::PayloadMismatch);
    // Channels of one set share the document extent; zero and over-ceiling extents are refused.
    let wide = with(channel("d", "spot_colour", 8), |v| v["width"] = json!(400));
    assert_eq!(refused(vec![ok(), wide]).target, "d/extent");
    assert_eq!(refused(vec![with(ok(), |v| v["height"] = json!(0))]).target, "c/extent");
    assert_eq!(refused(vec![with(ok(), |v| v["width"] = json!(MAX_CHANNEL_SIDE + 1))]).target, "c/extent");
    // Non-integral tile size.
    let fractional = with(ok(), |v| {
        for t in v["tiles"].as_array_mut().unwrap() {
            t["width"]["value"] = json!(255.5);
        }
    });
    assert_eq!(refused(vec![fractional]).code, Code::InvalidUnit);
}

#[test]
fn channel_counts_are_bounded_before_tiles_are_examined() {
    // More channels than the ceiling: refused before any channel is read.
    let many: Vec<Value> = (0..=MAX_CHANNELS).map(|i| channel(&format!("c{i}"), "masked_area_alpha", 8)).collect();
    assert_eq!(refused(many).code, Code::BudgetExceeded);
    // A huge declared extent with few tiles is refused by the cell count, not by a scan.
    let huge = with(channel("c", "masked_area_alpha", 8), |v| {
        v["width"] = json!(MAX_CHANNEL_SIDE);
        v["height"] = json!(MAX_CHANNEL_SIDE);
        for t in v["tiles"].as_array_mut().unwrap() {
            t["width"]["value"] = json!(1.0);
            t["height"]["value"] = json!(1.0);
        }
    });
    assert_eq!(refused(vec![huge]).target, "c/tiles");
    // Tiles count against the node budget.
    let small = Budget { nodes: 2, ..Budget::default() };
    let d = validate_channels(&records(vec![channel("c", "masked_area_alpha", 8)]).channels, small, &CancellationToken::default(), &Caller)
        .expect_err("over the node budget");
    assert_eq!(d.code, Code::BudgetExceeded);
    // The exact budget of a list admits it; one node less does not.
    let set = records(vec![channel("c", "masked_area_alpha", 8)]);
    let exact = channel_list_budget(&set.channels);
    assert_eq!(exact.nodes, 3);
    assert!(validate_channels(&set.channels, exact, &CancellationToken::default(), &Caller).is_ok());
    let short = Budget { nodes: 2, ..exact };
    assert!(validate_channels(&set.channels, short, &CancellationToken::default(), &Caller).is_err());
    // Cancellation is honoured.
    let token = CancellationToken::default();
    token.cancel();
    let d = validate_channels(&records(vec![channel("c", "masked_area_alpha", 8)]).channels, Budget::default(), &token, &Caller)
        .expect_err("canceled");
    assert_eq!(d.code, Code::Canceled);
}
