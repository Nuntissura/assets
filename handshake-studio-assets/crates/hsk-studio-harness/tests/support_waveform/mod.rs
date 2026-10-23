//! Shared fixtures of the waveform editor tests (lane SMB-AU-WAVEFORM-EDITOR): an empty document
//! package and a stereo source.
#![allow(dead_code)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, *};

pub const RATE: u32 = 48_000;

pub fn package(dir: &std::path::Path) -> std::path::PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000002","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[]
    });
    struct Any;
    impl folio::Resolver for Any {
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
        fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
            Resolution::Available
        }
    }
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("waveform.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Stereo source: a 440 Hz sine at 0.5 and a slow ramp, 4800 frames.
pub fn source() -> Vec<Vec<f32>> {
    let sine: Vec<f32> = (0..4800).map(|n| (0.5 * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / f64::from(RATE)).sin()) as f32).collect();
    let ramp: Vec<f32> = (0..4800).map(|n| (n as f32 / 4800.0) - 0.5).collect();
    vec![sine, ramp]
}
