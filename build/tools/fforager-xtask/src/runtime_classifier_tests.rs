//! Integration proof of the public command handler and its persisted report.
use super::*;

const WP: &str = "WP-FF-020-equivalent-work-benchmark-design-v1";
const ARTIFACT: &str = "build/benches/classifier-proof.json";
const ORIGINAL_WP020_PACKET: &[u8] =
    include_bytes!("testdata/wp020-build-only-prerequisite-v1.json");

struct Fixture {
    root: PathBuf,
    packet: Value,
}

impl Fixture {
    fn new(label: &str, mutate: impl FnOnce(&mut Value)) -> Self {
        let source = repo_root().unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = source
            .join(".fforager-artifacts/test-runs")
            .join(format!("classifier-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for path in [
            ".gitignore",
            "rust-toolchain.toml",
            "build/Cargo.toml",
            "build/Cargo.lock",
            "build/architecture-policy.toml",
            "build/tooling-policy.toml",
            "build/rule-to-proof.toml",
            "build/deny.toml",
            "build/tools/fforager-xtask/Cargo.toml",
            "product/MODEL_MANUAL.md",
            ".GOV/codex.yaml",
            ".GOV/id-registry.yaml",
            ".GOV/rules/build-rules.yaml",
            ".GOV/spec/ferric_forager_technical_design_v0.3.0.md",
            ".GOV/templates/WORK_PACKET_CONTRACT_TEMPLATE.json",
            ".GOV/templates/WORK_PACKET_REQUIREMENTS.yaml",
            ".GOV/topology.yaml",
        ] {
            let destination = root.join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(source.join(path), destination).unwrap();
        }
        for path in [
            "build/fixtures/architecture",
            "build/fixtures/contracts",
            "build/fixtures/resource-durability-models",
            "build/fixtures/transport-v1",
            "build/crates/fforager-transport",
            "build/crates/fforager-testkit",
            "build/tools/fforager-xtask/src",
            "product/crates",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.name", "Ferric-classifier-proof"]);
        git(
            &root,
            &["config", "user.email", "classifier-proof@example.invalid"],
        );
        git(&root, &["add", "."]);
        git(&root, &["commit", "--quiet", "-m", "Classifier base"]);
        let base = git(&root, &["rev-parse", "HEAD"]);
        let mut packet: Value = serde_json::from_slice(ORIGINAL_WP020_PACKET).unwrap();
        packet["lifecycle"]["status"] = Value::String("IN_PROGRESS".to_owned());
        packet["source_control"]["base_sha"] = Value::String(base.trim().to_owned());
        mutate(&mut packet);
        let fixture = Self { root, packet };
        fixture.write_packet();
        let refinement = format!(".GOV/work_packets/{WP}/refinement.json");
        fs::copy(source.join(&refinement), fixture.root.join(&refinement)).unwrap();
        fs::create_dir_all(fixture.root.join(".GOV/taskboard")).unwrap();
        fs::write(
            fixture.root.join(".GOV/taskboard/taskboard.yaml"),
            format!(
                "current_focus:\n  statement: Classifier integration proof\n  active_wp_id: {WP}\n"
            ),
        )
        .unwrap();
        git(&fixture.root, &["add", "."]);
        git(
            &fixture.root,
            &[
                "commit",
                "--quiet",
                "-m",
                "Immutable prerequisite activation",
            ],
        );
        fixture.write(ARTIFACT, "{\"artifact\":\"benchmark-contract-input\"}\n");
        fixture
    }

    fn write(&self, path: &str, text: &str) {
        let destination = self.root.join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, text).unwrap();
    }

    fn write_packet(&self) {
        self.write(
            &format!(".GOV/work_packets/{WP}/packet.json"),
            &serde_json::to_string_pretty(&self.packet).unwrap(),
        );
    }

    fn invoke(&self) -> (Result<(), String>, Value) {
        let reports = self.root.join("build/reports");
        if reports.exists() {
            for entry in fs::read_dir(&reports).unwrap() {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
        }
        let result = run_runtime_truth_gate(
            &self.root,
            &[
                "runtime-truth-check".to_owned(),
                "--evidence-from-taskboard".to_owned(),
            ],
        );
        let report_path = fs::read_dir(&reports)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
        (result, report)
    }

    fn rejects(&self, diagnostic: &str) {
        let (result, report) = self.invoke();
        let error = result.expect_err("public handler accepted forbidden classifier input");
        assert!(error.contains(diagnostic), "expected {diagnostic}: {error}");
        assert_eq!(report["status"], "FAIL");
        assert_eq!(report["exit_code"], 1);
        assert!(
            report["checks"][0]["observed_result"]
                .as_str()
                .unwrap()
                .contains(diagnostic)
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    command_output(root, "git", args).unwrap()
}

#[test]
fn build_only_prerequisite_public_handler_accepts_original_wp020_input() {
    let fixture = Fixture::new("wp020-original", |_| {});
    let (result, report) = fixture.invoke();
    result.unwrap();
    assert_eq!(report["status"], "PASS");
    assert_eq!(report["exit_code"], 0);
    assert_eq!(
        report["executed_proof_classes"],
        serde_json::json!(["policy", "scenario_contract"])
    );
    let limitations = report["proof_limitations"].to_string();
    assert!(limitations.contains("no product progress"));
    assert!(limitations.contains("first-runnable-production-slice"));
    assert!(
        report["checks"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("build-only")
    );
}

type MutationCase = (&'static str, fn(&mut Value), &'static str);

const MUTATION_CASES: &[MutationCase] = &[
    (
        "none",
        |p| {
            p["scope"]["product_impact"] = serde_json::json!("NONE");
            p["extensions"]
                .as_object_mut()
                .unwrap()
                .remove("non_product_prerequisite");
        },
        "",
    ),
    (
        "none-prerequisite",
        |p| p["scope"]["product_impact"] = serde_json::json!("NONE"),
        "IMPACT-MISMATCH",
    ),
    (
        "runtime",
        |p| p["scope"]["product_impact"] = serde_json::json!("RUNTIME"),
        "IMPACT-MISMATCH",
    ),
    (
        "missing-declaration",
        |p| {
            p["extensions"]
                .as_object_mut()
                .unwrap()
                .remove("non_product_prerequisite");
        },
        "PREREQUISITE-MISSING",
    ),
    (
        "missing-authority",
        |p| {
            p["extensions"]["non_product_prerequisite"]["design_authority"] = serde_json::json!("");
        },
        "PREREQUISITE-SCHEMA",
    ),
    (
        "unbound-authority",
        |p| p["authority"]["spec_anchors"] = serde_json::json!([]),
        "BUILD-PREREQUISITE-AUTHORITY",
    ),
    (
        "not-prerequisite-authority",
        |p| {
            let anchor = ".GOV/spec/ferric_forager_technical_design_v0.3.0.md#phase-1--first-runnable-production-slice";
            p["extensions"]["non_product_prerequisite"]["design_authority"] =
                serde_json::json!(anchor);
            p["authority"]["spec_anchors"] = serde_json::json!([anchor]);
        },
        "BUILD-PREREQUISITE-AUTHORITY",
    ),
    (
        "missing-consumer",
        |p| {
            p["extensions"]["non_product_prerequisite"]["required_future_consumer"] =
                serde_json::json!("");
        },
        "PREREQUISITE-SCHEMA",
    ),
    (
        "missing-proof",
        |p| {
            p["extensions"]["non_product_prerequisite"]["required_future_proof"] =
                serde_json::json!("");
        },
        "PREREQUISITE-SCHEMA",
    ),
    (
        "current-runtime-after-gate",
        |p| {
            p["extensions"]["non_product_prerequisite"]["required_future_proof"] =
                serde_json::json!("FF-GATE-RUNTIME-001: runtime completed");
        },
        "PREREQUISITE-PROGRESS-CLAIM",
    ),
    (
        "current-product-inside-future",
        |p| {
            p["extensions"]["non_product_prerequisite"]["required_future_proof"] = serde_json::json!(
                "Future FF-GATE-RUNTIME-001 proof; product capability is complete"
            );
        },
        "PREREQUISITE-PROGRESS-CLAIM",
    ),
    (
        "progress",
        |p| {
            p["extensions"]["non_product_prerequisite"]["phase_progress"] = serde_json::json!(true);
        },
        "PREREQUISITE-COMPLETION-CLAIM",
    ),
    (
        "runtime-proof",
        |p| p["extensions"]["runtime_proof"] = serde_json::json!({}),
        "PREREQUISITE-RUNTIME-CLAIM",
    ),
    (
        "runtime-acceptance",
        |p| p["acceptance_matrix"][0]["proof_class"] = serde_json::json!("production_runtime"),
        "PREREQUISITE-RUNTIME-CLAIM",
    ),
    (
        "unsafe-scope",
        |p| {
            p["scope"]["allowed_paths"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!("build/../"));
        },
        "BUILD-PREREQUISITE-SCOPE",
    ),
    (
        "wildcard-scope",
        |p| {
            p["scope"]["allowed_paths"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!("build/*"));
        },
        "BUILD-PREREQUISITE-SCOPE",
    ),
];

#[test]
fn build_only_prerequisite_public_handler_rejects_mutations() {
    for (label, mutate, diagnostic) in MUTATION_CASES {
        let fixture = Fixture::new(label, *mutate);
        if *label == "none" {
            let (result, report) = fixture.invoke();
            result.unwrap();
            assert_eq!(report["status"], "PASS");
            assert_eq!(
                report["executed_proof_classes"],
                serde_json::json!(["policy"])
            );
        } else {
            fixture.rejects(diagnostic);
        }
    }
}

#[test]
fn build_only_prerequisite_public_handler_observes_artifacts_with_declaration_frozen() {
    let mut fixture = Fixture::new("frozen-counterfactual", |_| {});
    let declaration = fixture.packet["extensions"]["non_product_prerequisite"].clone();
    fixture.invoke().0.unwrap();
    fixture.write(
        "build/undeclared/behavior-change.rs",
        "pub fn undeclared() {}\n",
    );
    fixture.rejects("BUILD-PREREQUISITE-PATH");
    assert_eq!(
        fixture.packet["extensions"]["non_product_prerequisite"],
        declaration
    );
    fs::remove_file(fixture.root.join("build/undeclared/behavior-change.rs")).unwrap();
    fs::remove_file(fixture.root.join(ARTIFACT)).unwrap();
    fixture.rejects("BUILD-PREREQUISITE-ARTIFACT");
    fixture.write(ARTIFACT, "{}\n");
    fixture.write("unexpected-root.json", "{}\n");
    fixture.rejects("BUILD-PREREQUISITE-PATH");
    fs::remove_file(fixture.root.join("unexpected-root.json")).unwrap();
    fixture.write("product/crates/forbidden/src/lib.rs", "pub fn token() {}\n");
    fixture.rejects("BUILD-PREREQUISITE-PATH");
    fs::remove_file(fixture.root.join("product/crates/forbidden/src/lib.rs")).unwrap();
    fixture.packet["scope"]["allowed_paths"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("build/undeclared/"));
    fixture.write_packet();
    fixture.rejects("BUILD-PREREQUISITE-SCOPE");
    fixture.packet["scope"]["allowed_paths"]
        .as_array_mut()
        .unwrap()
        .pop();
    fixture.packet["extensions"]["non_product_prerequisite"]["required_future_consumer"] =
        serde_json::json!("rewritten");
    fixture.write_packet();
    fixture.rejects("PREREQUISITE-NOT-PREDECLARED");
}

#[test]
fn runtime_classifier_public_handler_keeps_none_product_paths_closed() {
    let fixture = Fixture::new("none-product", |p| {
        p["scope"]["product_impact"] = serde_json::json!("NONE");
    });
    fixture.write(
        "product/crates/forbidden/src/lib.rs",
        "pub fn changed() {}\n",
    );
    fixture.rejects("IMPACT-MISMATCH");
}

#[test]
fn gate_identifier_is_not_a_current_runtime_claim() {
    assert!(!asserts_forbidden_prerequisite_completion(
        "the backwards-compatible FF-GATE-RUNTIME-001 staged-artifact contract delivered by WP-FF-021 plus the WP-FF-011 equivalent-work runtime benchmark gate"
    ));
    assert!(!asserts_forbidden_prerequisite_completion(
        "ff-gate-runtime-001 contract delivered by its future consumer"
    ));
    for text in [
        "FF-GATE-RUNTIME-001: runtime completed",
        "FF-GATE-RUNTIME-001; product capability is complete",
        "Future proof: runtime has been delivered",
        "xFF-GATE-RUNTIME-001 delivered",
        "FF-GATE-RUNTIME-001_suffix delivered",
    ] {
        assert!(asserts_forbidden_prerequisite_completion(text), "{text}");
    }
}

#[test]
fn original_wp020_claim_scan_has_no_current_progress_claims() {
    fn inspect(value: &Value, path: &str, positives: &mut Vec<String>) {
        match value {
            Value::Object(fields) => {
                for (key, child) in fields {
                    inspect(child, &format!("{path}/{key}"), positives);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    inspect(child, &format!("{path}/{index}"), positives);
                }
            }
            Value::String(text) if asserts_forbidden_prerequisite_completion(text) => {
                positives.push(format!("{path}: {text}"));
            }
            _ => {}
        }
    }
    let packet: Value = serde_json::from_slice(ORIGINAL_WP020_PACKET).unwrap();
    let mut positives = Vec::new();
    inspect(&packet, "", &mut positives);
    for positive in &positives {
        eprintln!("WP020 scanner positive {positive}");
    }
    reject_progress_claims(&packet)
        .expect("original future/non-goal/reference text is not a current outcome");
}

#[test]
fn original_wp020_context_corrections_keep_current_claims_closed() {
    for text in [
        "FF-GATE-RUNTIME-001: runtime completed",
        "WP-FF-012-runtime-truth-gates-v1: runtime completed",
        "xWP-FF-012-runtime-v1 VALIDATED",
        "WP-FF-012-runtime-v1_suffix VALIDATED",
        "WP-FF-012-runtime--bad-v1 VALIDATED",
        "runtime completed a production slice",
        "Runtime complete — a milestone",
        "Product capability is complete, a milestone",
        "product is first complete, a milestone",
        "Product capability is not only complete but validated",
        "not only shipped product",
    ] {
        assert!(asserts_forbidden_prerequisite_completion(text), "{text}");
    }
    for text in [
        "no shipped product path changes",
        "Phase 0D lane, first complete a separately authorized classifier remediation",
        "WP-FF-012-runtime-truth-gates-v1 VALIDATED closure",
    ] {
        assert!(!asserts_forbidden_prerequisite_completion(text), "{text}");
    }
    for packet in [
        serde_json::json!({"lifecycle":{"next_action":"runtime completed a production slice"}}),
        serde_json::json!({"lifecycle":{"blockers":["product capability complete"]}}),
        serde_json::json!({"acceptance_matrix":[{"requirement":"runtime completed"}]}),
        serde_json::json!({"recovery":{"last_checkpoint":{"next_action":"runtime completed"}}}),
        serde_json::json!({"extensions":{"non_product_prerequisite":{"required_future_proof":"runtime completed"}}}),
        serde_json::json!({"scope":{"non_goals":[{"runtime_status":"DONE"}]}}),
        serde_json::json!({"recovery":{"non_goals":["runtime completed"]}}),
        serde_json::json!({"scope/non_goals":["runtime complete"]}),
        serde_json::json!({"scope":{"nested/non_goals":["runtime complete"]}}),
    ] {
        assert!(reject_progress_claims(&packet).is_err(), "{packet}");
    }
    reject_progress_claims(
        &serde_json::json!({"scope":{"non_goals":["Claiming product-ready or release-ready"]}}),
    )
    .unwrap();
}
