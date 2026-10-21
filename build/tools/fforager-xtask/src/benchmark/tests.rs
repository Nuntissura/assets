use super::*;

fn pin(root: &Path, name: &str, bytes: &[u8]) -> Artifact {
    fs::write(root.join(name), bytes).expect("fixture artifact");
    Artifact { path: name.into(), sha256: super::super::encode_sha256(bytes), bytes: u64::try_from(bytes.len()).expect("length") }
}

fn fixture(root: &Path) -> Bundle {
    let executable = pin(root, "statistical-fixture-input.txt", b"NOT AN EXECUTABLE: statistical contract fixture only\n");
    let build = BuildIdentity { source_commit: "1".repeat(40), source_tree: "2".repeat(40), artifact: executable.clone(),
        rustc: "statistical_fixture_not_a_toolchain_observation".into(), cargo: "statistical_fixture_not_a_toolchain_observation".into(),
        profile: "statistical_fixture".into(), features: vec![], allocator: "unmeasured_fixture".into(),
        dependency_decisions: BTreeMap::from([("fixture".into(), "no_dependency_promotion".into())]), lock_sha256: "3".repeat(64) };
    let unavailable = ObservationValue::Unavailable { reason: "Statistical fixture; host behavior was not measured".into() };
    let profiles = ["windows", "unix"].into_iter().map(|platform| {
        let calibration = Calibration { schema_id: "ff.benchmark-calibration@1".into(), profile_id: platform.into(),
            source_identity: "synthetic_control_not_host_variance".into(), origin: SampleOrigin::StatisticalFixture,
            elapsed_ns: (0..50).map(|index| 1_000_000 + index * 100).collect() };
        let calibration_pin = pin(root, &format!("{platform}-calibration.json"), &serde_json::to_vec(&calibration).expect("calibration"));
        HostProfile { schema_id: "ff.benchmark-host@1".into(), id: platform.into(), platform: platform.into(),
            machine: unavailable.clone(), os: unavailable.clone(), filesystem: unavailable.clone(), storage: unavailable.clone(),
            security: unavailable.clone(), power_governor: unavailable.clone(), affinity: unavailable.clone(),
            memory_pressure: unavailable.clone(), filesystem_cache: unavailable.clone(), dns_cache: unavailable.clone(), network_replay: unavailable.clone(),
            preparation: vec!["Create isolated replay state; record real control availability before measurements".into()],
            cleanup: vec!["Release owned handles and remove only owned replay state".into()], preparation_deadline_ms: 30_000,
            cleanup_deadline_ms: 30_000, metrics: METRICS.into_iter().map(|name| (name.into(), unavailable.clone())).collect(),
            baseline_calibration: calibration_pin }
    }).collect::<Vec<_>>();
    let topology = Topology { processes: vec![ProcessRole { role: "ferric".into(), parent: "external".into(), artifact_role: "primary".into() }], input_urls_per_invocation: 1 };
    let absent = Tool::Unavailable { reason: "No optional external tool is required by this statistical fixture".into() };
    let manifest = Manifest { schema_id: "ff.benchmark-manifest@1".into(), run_id: "contract-fixture".into(), baseline: build.clone(), candidate: build,
        optional_comparison: absent.clone(), ffmpeg: absent.clone(), ffprobe: absent, workers: BTreeMap::new(),
        transport_identity: "fixture_replay".into(), archive_identity: "fixture_archive".into(), challenge_identity: "fixture_none".into(),
        corpus: executable, profiles: vec!["windows".into(), "unix".into()], active_profile_id: "windows".into(), topology: topology.clone(),
        required_dimensions: DIMENSIONS.into_iter().map(String::from).collect(), sampling: Sampling { independent_clusters: 50,
            pairs_per_cluster: 2, warmup_pairs: 3, random_seed: 23, bootstrap_resamples: 2_000, confidence_percent: 95,
            maximum_relative_noise_ppm: 10_000, maximum_outlier_ppm: 0,
            threshold_calibration_sha256: profiles[0].baseline_calibration.sha256.clone(), maximum_candidate_baseline_ratio_ppm: 1_000_000,
            minimum_improvement_ppm: 0 } };
    let digest = canonical_hash(&manifest).expect("manifest digest");
    let dimensions = DIMENSIONS.into_iter().map(|name| {
        let facts = BTreeMap::from([("work".into(), format!("concrete_fixture_{name}"))]);
        let record = NormalizedRecord { schema_id: "ff.benchmark-normalized-record@1".into(), dimension: name.into(), facts: facts.clone() };
        Dimension { name: name.into(), artifacts: vec![pin(root, &format!("{name}.json"), &serde_json::to_vec(&record).expect("record"))], facts }
    }).collect::<Vec<_>>();
    let observations = [Side::Baseline, Side::Candidate].into_iter().map(|side| Observation { schema_id: "ff.benchmark-observation@1".into(),
        id: if side == Side::Baseline { "baseline" } else { "candidate" }.into(), manifest_sha256: digest.clone(), profile_id: "windows".into(),
        build_side: side, build_artifact_sha256: manifest.baseline.artifact.sha256.clone(), topology: topology.clone(), dimensions: dimensions.clone(),
        proof_class: ProofClass::Structural }).collect();
    let metric = Metric::Unavailable { reason: "not measured in a statistical fixture".into() };
    let costs = ProcessCosts { total_system_cpu_ns: metric.clone(), ferric_cpu_ns: metric.clone(), ffmpeg_cpu_ns: metric.clone(),
        other_worker_cpu_ns: metric.clone(), peak_rss_bytes: metric.clone(), io_bytes: metric };
    let mut random = Random(23);
    let mut samples = Vec::new();
    for pair in 0..103 {
        let first = random.next() & 1 == 0;
        for side in [Side::Baseline, Side::Candidate] {
            let baseline = side == Side::Baseline;
            samples.push(Sample { id: format!("{pair}-{}", u8::from(baseline)), manifest_sha256: digest.clone(),
                cluster_id: if pair < 3 { 0 } else { (pair - 3) / 2 + 1 }, pair_index: pair,
                order_index: u32::from(if baseline { !first } else { first }), warmup: pair < 3, side,
                observation_id: if baseline { "baseline" } else { "candidate" }.into(),
                elapsed_ns: (if baseline { 1_000_000 } else { 900_000 }) + u64::from(pair % 5) * 100,
                origin: SampleOrigin::StatisticalFixture, costs: costs.clone() });
        }
    }
    Bundle { schema_id: "ff.benchmark-bundle@1".into(), manifest, profiles, observations, samples }
}

#[test]
fn public_admission_rejects_unequal_work_and_false_statistics() {
    let repo = super::super::repo_root().expect("repository");
    let root = super::super::disposable_artifact_root(&repo).join(format!("benchmark-contract-{}", std::process::id()));
    fs::create_dir_all(&root).expect("fixture directory");
    let good = fixture(&root);
    let report = evaluate(&root, good.clone()).expect("admitted statistical fixture");
    assert_eq!(report.proof_class, ProofClass::Structural);
    assert!(report.statistical_fixture && !report.performance_claim && !report.product_progress);
    assert_eq!(report.raw_samples.len(), 206);
    assert_eq!(report.paired_cluster_ratios.len(), 50);
    assert!(!report.unavailable_metrics.is_empty());
    for index in 0..DIMENSIONS.len() {
        let mut unequal = good.clone();
        unequal.observations[1].dimensions[index].facts.insert("work".into(), "cheaper_or_different".into());
        let dimension = &mut unequal.observations[1].dimensions[index];
        let record = NormalizedRecord { schema_id: "ff.benchmark-normalized-record@1".into(), dimension: dimension.name.clone(), facts: dimension.facts.clone() };
        dimension.artifacts[0] = pin(&root, &format!("negative-{index}.json"), &serde_json::to_vec(&record).expect("negative record"));
        assert!(evaluate(&root, unequal).expect_err("unequal concrete work").contains("UNEQUAL-WORK"), "dimension {} admitted", DIMENSIONS[index]);
    }
    let mut missing = good.clone(); missing.samples.pop();
    assert!(evaluate(&root, missing).expect_err("missing raw").contains("MISSING-RAW"));
    let mut duplicate = good.clone(); duplicate.samples[1].id = duplicate.samples[0].id.clone();
    assert!(evaluate(&root, duplicate).expect_err("duplicate").contains("SAMPLE-IDENTITY"));
    let mut provenance = good.clone(); provenance.samples[0].manifest_sha256 = "4".repeat(64);
    assert!(evaluate(&root, provenance).expect_err("manifest").contains("SAMPLE-IDENTITY"));
    let mut order = good.clone(); order.samples[0].order_index ^= 1;
    assert!(evaluate(&root, order).expect_err("order").contains("RANDOMIZED-PAIR-ORDER"));
    let mut cluster = good.clone(); cluster.samples[6].cluster_id = 40;
    assert!(evaluate(&root, cluster).expect_err("cluster").contains("CLUSTER-OR-WARMUP"));
    let mut slower = good.clone();
    for sample in &mut slower.samples { if sample.side == Side::Candidate { sample.elapsed_ns += 200_000; } }
    assert!(evaluate(&root, slower).expect_err("confidence").contains("CONFIDENCE-GATE"));
    let mut noisy = good.clone();
    for sample in &mut noisy.samples { if sample.pair_index % 2 == 0 { sample.elapsed_ns *= 2; } }
    assert!(evaluate(&root, noisy).is_err());
    let mut topology = good.clone(); topology.observations[1].topology.processes[0].parent = "shifted_worker".into();
    assert!(evaluate(&root, topology).expect_err("topology").contains("OBSERVATION-PROVENANCE"));
    let mut serialized = serde_json::to_value(&good).expect("JSON"); serialized["aggregate_mean"] = 1.into();
    assert!(serde_json::from_value::<Bundle>(serialized).is_err(), "unknown aggregate shortcut");
    let input = root.join("bundle.json"); fs::write(&input, serde_json::to_vec(&good).expect("input")).expect("write input");
    run(&root, &["--input".into(), "bundle.json".into(), "--report".into(), ".fforager-artifacts/benchmark-admission-report.json".into()]).expect("public handler");
    fs::write(root.join("statistical-fixture-input.txt"), b"tampered").expect("artifact mutation");
    assert!(evaluate(&root, good).expect_err("actual file mutation").contains("ARTIFACT-LENGTH"));
}
