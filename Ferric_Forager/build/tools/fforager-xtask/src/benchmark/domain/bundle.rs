//! Materialize admission inputs from both independently validated native controls.

use super::{
    Artifact, BTreeMap, BuildIdentity, Bundle, Calibration, ControlContext, ControlSample,
    DIMENSIONS, Dimension, Envelope, HostProfile, LIMIT, MODE, Manifest, Metric, NO_ARCHIVE,
    Observation, Operation, Path, PathBuf, ProcessCosts, ProcessRole, ProofClass, Sample,
    SampleOrigin, Sampling, Serialize, Tool, Topology, bounded, calibration, canonical_hash,
    derive_all_control, distribution, error, fs, host, noise, number, read_pin, relative,
};

fn read_profile(root: &Path, path: &str, platform: &str) -> Result<HostProfile, String> {
    let path = relative(root, path)?;
    if fs::metadata(&path).map_err(|e| e.to_string())?.len()
        > u64::try_from(LIMIT).map_err(|_| error("LIMIT"))?
    {
        return Err(error("PROFILE-BOUND"));
    }
    let profile: HostProfile = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| error(&format!("PROFILE-SCHEMA:{e}")))?;
    if profile.platform != platform {
        return Err(error("PROFILE-PLATFORM"));
    }
    Ok(profile)
}

fn new_directory(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let normalized = relative.replace('\\', "/");
    let path = Path::new(&normalized);
    if !normalized.starts_with(".fforager-artifacts/")
        || normalized.contains(' ')
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(error("BUNDLE-OUTPUT"));
    }
    let target = root.join(path);
    let parent = target.parent().ok_or_else(|| error("BUNDLE-OUTPUT"))?;
    if !parent
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(
            root.join(".fforager-artifacts")
                .canonicalize()
                .map_err(|e| e.to_string())?,
        )
    {
        return Err(error("BUNDLE-OUTPUT-ESCAPE"));
    }
    fs::create_dir(&target).map_err(|e| error(&format!("BUNDLE-NEW-OUTPUT:{e}")))?;
    Ok(target)
}

fn write_new(root: &Path, path: &str, value: &impl Serialize) -> Result<Artifact, String> {
    use std::io::Write;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT {
        return Err(error("BUNDLE-JSON-BOUND"));
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(path))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    Ok(Artifact {
        path: path.replace('\\', "/"),
        sha256: crate::encode_sha256(&bytes),
        bytes: u64::try_from(bytes.len()).map_err(|_| error("LIMIT"))?,
    })
}

fn identity(root: &Path, receipt: &calibration::NativeReceipt) -> Result<BuildIdentity, String> {
    let producer = &receipt.source.git_commit;
    let policy = crate::command_output(
        root,
        "git",
        &[
            "show",
            &format!("{producer}:build/architecture-policy.toml"),
        ],
    )?;
    let rules = crate::command_output(
        root,
        "git",
        &["show", &format!("{producer}:.GOV/rules/build-rules.yaml")],
    )?;
    Ok(BuildIdentity {
        source_commit: producer.clone(),
        source_tree: receipt.source_tree.clone(),
        artifact: receipt.ffmpeg.clone(),
        rustc: receipt.producer_rustc.clone(),
        cargo: receipt.producer_cargo.clone(),
        profile: "external_native_control".into(),
        features: Vec::new(),
        allocator: "external_FFmpeg_unspecified;Rust_fields_describe_collector".into(),
        dependency_decisions: BTreeMap::from([
            (
                "producer_architecture_policy_sha256".into(),
                crate::encode_sha256(policy.as_bytes()),
            ),
            (
                "producer_native_decision_registry_sha256".into(),
                crate::encode_sha256(rules.as_bytes()),
            ),
        ]),
        lock_sha256: receipt.lock_sha256.clone(),
    })
}

fn sampling(calibration: &Calibration) -> Result<Sampling, String> {
    let values = calibration
        .elapsed_ns
        .iter()
        .copied()
        .map(number)
        .collect::<Vec<_>>();
    let d = distribution(&values);
    if d.median <= 0.0 {
        return Err(error("CALIBRATION"));
    }
    let observed = (d.p75 - d.p25) / d.median;
    let mut slack_ppm = 0_u32;
    // Integer search rounds down without unchecked float-to-integer conversion.
    while slack_ppm < 500_000 && f64::from(slack_ppm + 1) / 1_000_000.0 <= 3.0 * observed {
        slack_ppm += 1;
    }
    let maximum_noise = slack_ppm.max(1);
    noise(&values, maximum_noise, 100_000)?;
    Ok(Sampling {
        independent_clusters: 50,
        pairs_per_cluster: 1,
        warmup_pairs: 3,
        random_seed: 23,
        bootstrap_resamples: 2_000,
        confidence_percent: 95,
        maximum_relative_noise_ppm: maximum_noise,
        maximum_outlier_ppm: 100_000,
        threshold_calibration_sha256: String::new(),
        maximum_candidate_baseline_ratio_ppm: 1_000_000 + slack_ppm,
        minimum_improvement_ppm: 0,
    })
}

fn manifest(
    root: &Path,
    profiles: &[HostProfile],
    active: &HostProfile,
    calibration: &Calibration,
    control_calibration: Artifact,
    receipt: &calibration::NativeReceipt,
) -> Result<Manifest, String> {
    let primary = identity(root, receipt)?;
    let read_version = |pin: &Artifact| -> Result<String, String> {
        let bytes = read_pin(root, pin, 1024 * 1024)?;
        let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        let version = text
            .lines()
            .next()
            .ok_or_else(|| error("TOOL-VERSION"))?
            .to_owned();
        if !bounded(&version, 256) {
            return Err(error("TOOL-VERSION"));
        }
        Ok(version)
    };
    let [ffmpeg_version, ffprobe_version] = receipt.version_captures.as_slice() else {
        return Err(error("TOOL-VERSIONS"));
    };
    let ffmpeg = Tool::Available {
        version: read_version(ffmpeg_version)?,
        artifact: receipt.ffmpeg.clone(),
    };
    let ffprobe = Tool::Available {
        version: read_version(ffprobe_version)?,
        artifact: receipt.ffprobe.clone(),
    };
    let mut sampling = sampling(calibration)?;
    sampling
        .threshold_calibration_sha256
        .clone_from(&active.baseline_calibration.sha256);
    Ok(Manifest {
        schema_id: "ff.benchmark-manifest@1".into(),
        operation: Operation::LocalPcmRemux,
        control_calibration: Some(control_calibration),
        run_id: format!(
            "native-control-{}",
            &active.baseline_calibration.sha256[..16]
        ),
        baseline: primary.clone(),
        candidate: primary,
        optional_comparison: Tool::Unavailable {
            reason: "Same-build external controls; no competing implementation comparison".into(),
        },
        ffmpeg,
        ffprobe: ffprobe.clone(),
        workers: BTreeMap::from([("ffprobe".into(), ffprobe)]),
        transport_identity: "file_only".into(),
        archive_identity: NO_ARCHIVE.into(),
        challenge_identity: NO_ARCHIVE.into(),
        corpus: receipt.input.clone(),
        profiles: profiles.iter().map(|p| p.id.clone()).collect(),
        active_profile_id: active.id.clone(),
        topology: Topology {
            processes: vec![
                ProcessRole {
                    role: "ffmpeg".into(),
                    parent: "external".into(),
                    artifact_role: "primary".into(),
                },
                ProcessRole {
                    role: "ffprobe".into(),
                    parent: "external".into(),
                    artifact_role: "ffprobe".into(),
                },
            ],
            input_urls_per_invocation: 1,
        },
        required_dimensions: DIMENSIONS.into_iter().map(str::to_owned).collect(),
        sampling,
    })
}

fn samples_and_observations(
    root: &Path,
    directory: &str,
    manifest: &Manifest,
    receipt: &Artifact,
    controls: &[ControlSample],
) -> Result<(Vec<Observation>, Vec<Sample>), String> {
    let digest = canonical_hash(manifest)?;
    let mut observations = Vec::new();
    let mut samples = Vec::new();
    for control in controls {
        let pin = write_new(
            root,
            &format!(
                "{directory}/control-{}-{}.json",
                control.pair, control.order
            ),
            &Envelope {
                schema_id: "ff.benchmark-domain@1".into(),
                operation: MODE.into(),
                control_receipt: receipt.clone(),
                pair: control.pair,
                order: control.order,
            },
        )?;
        let facts = derive_all_control(root, manifest, control)?;
        let observation_id = format!("control-{}-{}", control.pair, control.order);
        observations.push(Observation {
            schema_id: "ff.benchmark-observation@1".into(),
            id: observation_id.clone(),
            manifest_sha256: digest.clone(),
            profile_id: manifest.active_profile_id.clone(),
            build_side: control.side,
            build_artifact_sha256: control.invocation.program.sha256.clone(),
            topology: manifest.topology.clone(),
            dimensions: DIMENSIONS
                .into_iter()
                .map(|name| Dimension {
                    name: name.into(),
                    artifacts: vec![pin.clone()],
                    facts: facts[name].clone(),
                })
                .collect(),
            proof_class: ProofClass::Semantic,
        });
        let unavailable = Metric::Unavailable {
            reason: "Not collected by this wall-only external-tool control".into(),
        };
        samples.push(Sample {
            id: format!("sample-{}-{}", control.pair, control.order),
            manifest_sha256: digest.clone(),
            cluster_id: if control.warmup { 0 } else { control.pair - 2 },
            pair_index: control.pair,
            order_index: control.order,
            warmup: control.warmup,
            side: control.side,
            observation_id,
            elapsed_ns: control.elapsed_ns,
            origin: SampleOrigin::SuppliedMeasurement,
            costs: ProcessCosts {
                total_system_cpu_ns: unavailable.clone(),
                ferric_cpu_ns: unavailable.clone(),
                ffmpeg_cpu_ns: unavailable.clone(),
                other_worker_cpu_ns: unavailable.clone(),
                peak_rss_bytes: unavailable.clone(),
                io_bytes: unavailable,
            },
        });
    }
    Ok((observations, samples))
}

fn control_cohort(
    root: &Path,
    path: &str,
    profile: &HostProfile,
    calibration: &Calibration,
    baseline: &ControlContext,
) -> Result<(Artifact, Calibration, Vec<ControlSample>), String> {
    let physical = relative(root, path)?;
    let bytes = fs::metadata(&physical).map_err(|e| e.to_string())?.len();
    if bytes > u64::try_from(LIMIT).map_err(|_| error("LIMIT"))? {
        return Err(error("CONTROL-JSON-BOUND"));
    }
    let pin = Artifact {
        path: path.replace('\\', "/"),
        sha256: crate::sha256_file(&physical)?,
        bytes,
    };
    let measured: Calibration = serde_json::from_slice(&read_pin(
        root,
        &pin,
        u64::try_from(LIMIT).map_err(|_| error("LIMIT"))?,
    )?)
    .map_err(|e| e.to_string())?;
    if measured.origin != SampleOrigin::SuppliedMeasurement {
        return Err(error("REAL-CONTROLS-REQUIRED"));
    }
    let receipt = measured
        .control_receipt
        .as_ref()
        .ok_or_else(|| error("CONTROL-RECEIPT-REQUIRED"))?;
    if baseline.contains_key(&receipt.sha256) {
        return Err(error("CONTROLS-REUSE-CALIBRATION"));
    }
    let controls = calibration::verify(root, profile, &measured)?;
    let active_receipt = calibration
        .control_receipt
        .as_ref()
        .ok_or_else(|| error("BASELINE-CONTROLS-REQUIRED"))?;
    let active_controls = baseline
        .get(&active_receipt.sha256)
        .ok_or_else(|| error("BASELINE-CONTROLS-REQUIRED"))?;
    let identities = active_controls
        .iter()
        .flat_map(|control| [&control.invocation, &control.probe_invocation])
        .map(|invocation| (invocation.parent_pid, invocation.child_pid))
        .collect::<std::collections::BTreeSet<_>>();
    if controls
        .iter()
        .flat_map(|control| [&control.invocation, &control.probe_invocation])
        .any(|invocation| identities.contains(&(invocation.parent_pid, invocation.child_pid)))
    {
        return Err(error("CONTROLS-REUSE-INVOCATION"));
    }
    Ok((pin, measured, controls))
}

pub(super) fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let [
        wf,
        windows,
        uf,
        unix,
        af,
        active_id,
        cf,
        controls_path,
        of,
        directory,
    ] = args
    else {
        return Err("usage: benchmark-bundle --windows-profile <profile.json> --unix-profile <profile.json> --active-profile <id> --controls <independent-calibration.json> --output <new-ignored-artifact-directory>".into());
    };
    if [wf.as_str(), uf, af, cf, of]
        != [
            "--windows-profile",
            "--unix-profile",
            "--active-profile",
            "--controls",
            "--output",
        ]
    {
        return Err(error("ARGUMENTS"));
    }
    let profiles = vec![
        read_profile(root, windows, "windows")?,
        read_profile(root, unix, "unix")?,
    ];
    let mut context = ControlContext::new();
    for profile in &profiles {
        host(root, profile, &mut context)?;
    }
    let active = profiles
        .iter()
        .find(|p| &p.id == active_id)
        .ok_or_else(|| error("ACTIVE-HOST"))?;
    let calibration: Calibration = serde_json::from_slice(&read_pin(
        root,
        &active.baseline_calibration,
        u64::try_from(LIMIT).map_err(|_| error("LIMIT"))?,
    )?)
    .map_err(|e| e.to_string())?;
    let (control_pin, measured, controls) =
        control_cohort(root, controls_path, active, &calibration, &context)?;
    let receipt_pin = measured
        .control_receipt
        .as_ref()
        .ok_or_else(|| error("CONTROL-RECEIPT-REQUIRED"))?;
    let (receipt, _) = calibration::read_receipt(root, receipt_pin)?;
    let manifest = manifest(root, &profiles, active, &calibration, control_pin, &receipt)?;
    new_directory(root, directory)?;
    let (observations, samples) =
        samples_and_observations(root, directory, &manifest, receipt_pin, &controls)?;
    let bundle = Bundle {
        schema_id: "ff.benchmark-bundle@1".into(),
        manifest,
        profiles,
        observations,
        samples,
    };
    let path = format!("{directory}/bundle.json");
    write_new(root, &path, &bundle)?;
    println!(
        "BENCHMARK_BUNDLE input={path}; same-build external controls; no Ferric performance claim; Rust source/toolchain identify the collector"
    );
    Ok(())
}
