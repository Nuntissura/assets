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
fn tile(layer: u32, key: &str, format: &str) -> Value {
    json!({"object_key":key,"layer_id":identity("SLYR",layer),"column":0,"row":0,
        "width":{"value":256,"unit":"px"},"height":{"value":256,"unit":"px"},"format":format,
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"ab12"},"artifact_manifest_id":"manifest-a"})
}
/// Group (1) containing raster (2) with one tile `t_0_0`, plus a root raster (3).
fn document() -> Value {
    let layer = |n: u32, name: &str, parent: Value, kind: &str, payload: Value| {
        json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":name,"parent":parent,
            "kind":kind,"payload":payload,"visible":true})
    };
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px","colour_mode":2,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[
            layer(1,"group",Value::Null,"group",json!({"payload_kind":"group"})),
            layer(2,"leaf",json!({"node_kind":"layer","node_id":identity("SLYR",1)}),"raster",json!({"payload_kind":"raster","tiles":[tile(2,"t_0_0","rgb_f32le")]})),
            layer(3,"plain",Value::Null,"raster",json!({"payload_kind":"raster","tiles":[]})),
        ],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    })
}
fn appearance(opacity: f64, fill: f64, blend: u16, clipped: bool, isolation: &str) -> Value {
    json!({"opacity":opacity,"fill_opacity":fill,"blend_mode":blend,"clipped":clipped,"isolation":isolation})
}
fn with(layer: usize, appearance: Value) -> Value {
    let mut v = document();
    v["layers"][layer]["appearance"] = appearance;
    v
}
fn inspect(v: &Value, r: &dyn Resolver) -> Result<Inspection, Diagnostic> {
    inspect_bytes(&serde_json::to_vec(v).unwrap(), Budget::default(), &CancellationToken::default(), r)
}
fn open(v: &Value, r: &dyn Resolver) -> Snapshot {
    match inspect(v, r).expect("valid document") {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("expected an editable snapshot"),
    }
}
fn rejected(v: &Value) -> Code {
    inspect(v, &Caller(Resolution::Available)).expect_err("must be rejected").code
}

#[test]
fn old_documents_stay_valid_and_absent_appearance_is_the_identity_default() {
    bounded_case("old_documents_stay_valid_and_absent_appearance_is_the_identity_default", || {
        let snapshot = open(&document(), &Caller(Resolution::Available));
        let layers = &snapshot.document().layers;
        let group = layers[0].effective_appearance();
        assert_eq!((group.blend_mode, group.isolation, group.opacity, group.fill_opacity), (1, Isolation::PassThrough, 100.0, 100.0));
        let leaf = layers[1].effective_appearance();
        assert_eq!((leaf.blend_mode, leaf.isolation, leaf.clipped, leaf.mask.is_none()), (2, Isolation::Isolated, false, true));
        assert_eq!((leaf.opacity_fraction(), leaf.fill_opacity_fraction()), (1.0, 1.0));
        // an absent appearance is not encoded: the document re-serializes without the key
        let encoded = serde_json::to_string(snapshot.document()).unwrap();
        assert!(!encoded.contains("appearance"));
        // an explicit identity appearance is valid and kept
        let explicit = open(&with(1, appearance(100.0, 100.0, 2, false, "isolated")), &Caller(Resolution::Available));
        assert_eq!(explicit.document().layers[1].appearance, Some(LayerAppearance::identity(false)));
        assert!(serde_json::to_string(explicit.document()).unwrap().contains("\"appearance\""));
    });
}

#[test]
fn appearance_is_stored_survives_edits_and_invalid_values_are_typed() {
    bounded_case("appearance_is_stored_survives_edits_and_invalid_values_are_typed", || {
        let mut v = with(1, appearance(50.5, 25.0, 5, true, "isolated"));
        v["layers"][0]["appearance"] = appearance(80.0, 100.0, 5, false, "isolated");
        let snapshot = open(&v, &Caller(Resolution::Available));
        let leaf = snapshot.document().layers[1].effective_appearance();
        assert_eq!((leaf.opacity, leaf.fill_opacity, leaf.blend_mode, leaf.clipped), (50.5, 25.0, 5, true));
        assert_eq!(leaf.opacity_fraction(), 0.505);
        let token = CancellationToken::default();
        let renamed = snapshot
            .rename(&identity("SLYR", 2), "renamed".into(), 1, 2, Budget::default(), &token, &Caller(Resolution::Available))
            .expect("rename keeps appearance");
        assert_eq!(renamed.document().layers[1].appearance, snapshot.document().layers[1].appearance);
        assert_eq!(renamed.document().layers[0].appearance.as_ref().map(|a| a.isolation), Some(Isolation::Isolated));

        // typed rejections
        for (value, code) in [(100.5, Code::InvalidAppearance), (-0.1, Code::InvalidAppearance)] {
            assert_eq!(rejected(&with(1, appearance(value, 100.0, 2, false, "isolated"))), code);
            assert_eq!(rejected(&with(1, appearance(100.0, value, 2, false, "isolated"))), code);
        }
        for blend in [0u16, 36, 65535] {
            assert_eq!(rejected(&with(1, appearance(100.0, 100.0, blend, false, "isolated"))), Code::UnknownBlendMode);
        }
        for blend in [24u16, 25] {
            assert_eq!(rejected(&with(1, appearance(100.0, 100.0, blend, false, "isolated"))), Code::InapplicableBlendMode, "tool-only");
        }
        assert_eq!(rejected(&with(1, appearance(100.0, 100.0, 1, false, "isolated"))), Code::InapplicableBlendMode, "pass_through on a leaf");
        assert_eq!(rejected(&with(1, appearance(100.0, 100.0, 2, false, "pass_through"))), Code::InvalidAppearance, "leaf is never pass_through");
        assert_eq!(rejected(&with(0, appearance(100.0, 100.0, 1, false, "isolated"))), Code::InvalidAppearance, "group mode 1 means pass_through");
        assert_eq!(rejected(&with(0, appearance(100.0, 100.0, 5, false, "pass_through"))), Code::InvalidAppearance);
        // every member 31..=35 is in the known set
        for blend in 31..=35u16 {
            open(&with(1, appearance(100.0, 100.0, blend, false, "isolated")), &Caller(Resolution::Available));
        }
        // a pass-through group is valid
        open(&with(0, appearance(100.0, 100.0, 1, false, "pass_through")), &Caller(Resolution::Available));
    });
}

#[test]
fn mask_reference_uses_tile_reference_style_and_is_validated() {
    bounded_case("mask_reference_uses_tile_reference_style_and_is_validated", || {
        let mask = |tiles: Vec<Value>| {
            let mut a = appearance(100.0, 100.0, 2, false, "isolated");
            a["mask"] = json!({"tiles":tiles,"enabled":true,"linked":false});
            a
        };
        let good = with(1, mask(vec![tile(2, "m_0_0", "coverage_u8")]));
        let snapshot = open(&good, &Caller(Resolution::Available));
        let stored = snapshot.document().layers[1].appearance.as_ref().unwrap().mask.as_ref().unwrap();
        assert_eq!((stored.tiles.len(), stored.enabled, stored.linked), (1, true, false));
        assert!(snapshot.resolution_issues().is_empty());
        let absent = open(&good, &Caller(Resolution::Absent));
        let targets: Vec<&str> = absent.resolution_issues().iter().map(|i| i.target.as_str()).collect();
        assert!(targets.contains(&format!("{}/mask/m_0_0", identity("SLYR", 2)).as_str()));
        assert_eq!(serde_json::to_value(snapshot.document()).unwrap()["layers"][1]["appearance"]["mask"]["linked"], json!(false));
        // 16-bit coverage and an empty mask are valid, and a group may carry a mask
        open(&with(1, mask(vec![tile(2, "m_0_0", "coverage_u16le")])), &Caller(Resolution::Available));
        open(&with(1, mask(vec![])), &Caller(Resolution::Available));
        let mut grouped = appearance(100.0, 100.0, 1, false, "pass_through");
        grouped["mask"] = json!({"tiles":[tile(1, "g_0_0", "coverage_u8")],"enabled":false,"linked":true});
        open(&with(0, grouped), &Caller(Resolution::Available));
        // rejected: foreign layer id, key reused from the raster payload, colour format
        assert_eq!(rejected(&with(1, mask(vec![tile(3, "m_0_0", "coverage_u8")]))), Code::InvalidAppearance);
        assert_eq!(rejected(&with(1, mask(vec![tile(2, "t_0_0", "coverage_u8")]))), Code::InvalidAppearance);
        assert_eq!(rejected(&with(1, mask(vec![tile(2, "m_0_0", "rgb_f32le")]))), Code::InvalidField);
        assert_eq!(rejected(&with(1, mask(vec![tile(2, "m_0_0", "coverage_u8"), tile(2, "m_0_0", "coverage_u8")]))), Code::InvalidAppearance);
    });
}

#[test]
fn unknown_appearance_fields_stay_read_only_and_the_schema_describes_the_contract() {
    bounded_case("unknown_appearance_fields_stay_read_only_and_the_schema_describes_the_contract", || {
        let mut extra = appearance(100.0, 100.0, 2, false, "isolated");
        extra["knockout"] = json!(true);
        let bytes = serde_json::to_vec(&with(1, extra)).unwrap();
        let Inspection::ReadOnly(read_only) = inspect_bytes(&bytes, Budget::default(), &CancellationToken::default(), &Caller(Resolution::Available)).unwrap() else {
            panic!("an unknown appearance field must keep the document read-only")
        };
        assert_eq!(read_only.diagnostic().code, Code::UnknownField);
        assert_eq!(read_only.encoded_bytes(), bytes.as_slice(), "complete original bytes preserved");
        let mut masked = appearance(100.0, 100.0, 2, false, "isolated");
        masked["mask"] = json!({"tiles":[],"enabled":true,"linked":true,"density":50});
        assert!(matches!(inspect(&with(1, masked), &Caller(Resolution::Available)).unwrap(), Inspection::ReadOnly(_)));
        let schema = serde_json::to_string(&document_schema()).unwrap();
        for needle in ["fill_opacity", "isolation", "StudioMask", "blend_mode"] {
            assert!(schema.contains(needle), "{needle} is in the generated schema");
        }
        assert!(DESCRIPTOR.contains("appearance"));
    });
}
