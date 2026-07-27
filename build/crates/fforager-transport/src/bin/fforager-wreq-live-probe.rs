use fforager_transport::{LiveProbeOptions, WreqAdjudicationAdapter};
use serde::Serialize;

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct FailedLiveProbe {
    schema_version: &'static str,
    evidence_class: &'static str,
    candidate: &'static str,
    error: String,
}

fn main() {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["--enable-live"] {
        eprintln!(
            "usage: cargo run -p fforager-transport --bin fforager-wreq-live-probe -- --enable-live"
        );
        std::process::exit(2);
    }
    let result = WreqAdjudicationAdapter::new().and_then(|adapter| {
        adapter.execute_live_wire_probe(LiveProbeOptions {
            maximum_body_bytes: 1024 * 1024,
        })
    });
    match result {
        Ok(report) => {
            let json = serde_json::to_string_pretty(&report)
                .unwrap_or_else(|_| serialization_failure_json());
            println!("{json}");
        }
        Err(error) => {
            let report = FailedLiveProbe {
                schema_version: "ff-wreq-live-probe-v1",
                evidence_class: "blocked_external_observation",
                candidate: "wreq-6.0.0-rc.29",
                error: error.to_string(),
            };
            let json = serde_json::to_string_pretty(&report)
                .unwrap_or_else(|_| serialization_failure_json());
            println!("{json}");
            std::process::exit(1);
        }
    }
}

fn serialization_failure_json() -> String {
    "{\"schema_version\":\"ff-wreq-live-probe-v1\",\"evidence_class\":\"internal_serialization_failure\"}"
        .to_owned()
}
