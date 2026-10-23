//! Finite file-remux observations derived from pinned physical control evidence.

use super::{
    Artifact, BTreeMap, BuildIdentity, Bundle, Calibration, ControlContext, DIMENSIONS,
    Deserialize, Dimension, HostProfile, LIMIT, MAX_DURATION_NS, Manifest, Metric, Observation,
    Operation, Path, PathBuf, ProcessCosts, ProcessRole, ProofClass, Sample, SampleOrigin,
    Sampling, Serialize, Side, Tool, Topology, artifact, bounded, calibration, canonical_hash,
    distribution, error, fs, host, noise, number, relative,
};
use calibration::{ControlSample, ToolInvocation, WaveFacts};
use serde_json::Value;

const MODE: &str = "local_pcm_remux";
const NO_ARCHIVE: &str = "not_applicable_local_pcm_remux";

#[path = "domain/bundle.rs"]
mod materialize;

pub(super) fn bundle(root: &Path, args: &[String]) -> Result<(), String> {
    materialize::run(root, args)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub(super) schema_id: String,
    pub(super) operation: String,
    pub(super) control_receipt: Artifact,
    pub(super) pair: u32,
    pub(super) order: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    streams: Vec<Stream>,
    format: Format,
    #[serde(default)]
    programs: Vec<Value>,
    #[serde(default)]
    stream_groups: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stream {
    index: u32,
    codec_type: String,
    codec_name: String,
    channels: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Format {
    #[serde(rename = "format_name")]
    name: String,
    duration: String,
    size: String,
}

fn read_pin(root: &Path, pin: &Artifact, maximum: u64) -> Result<Vec<u8>, String> {
    if pin.bytes > maximum {
        return Err(error("DOMAIN-ARTIFACT-BOUND"));
    }
    artifact(root, pin)?;
    fs::read(relative(root, &pin.path)?).map_err(|e| e.to_string())
}

fn arguments(actual: &[String], expected: &[&str], control: &ControlSample) -> Result<(), String> {
    if actual.len() != expected.len() {
        return Err(error("DOMAIN-ARGV"));
    }
    for (actual, expected) in actual.iter().zip(expected) {
        let matches = match *expected {
            "@input" => actual == &calibration::control_path(control, &control.input),
            "@output" => actual == &calibration::control_path(control, &control.output),
            literal => actual == literal,
        };
        if !matches {
            return Err(error("DOMAIN-ARGV"));
        }
    }
    Ok(())
}

fn captures(root: &Path, invocation: &ToolInvocation, probe: bool) -> Result<(), String> {
    if invocation.exit_code != 0
        || invocation.child_pid == 0
        || invocation.parent_pid == 0
        || invocation.child_pid == invocation.parent_pid
        || invocation.elapsed_ns == 0
        || invocation.elapsed_ns > MAX_DURATION_NS
    {
        return Err(error("DOMAIN-INVOCATION"));
    }
    let stdout = read_pin(root, &invocation.stdout, 1024 * 1024)?;
    let stderr = read_pin(root, &invocation.stderr, 1024 * 1024)?;
    if !stderr.is_empty() || (!probe && !stdout.is_empty()) {
        return Err(error("DOMAIN-TOOL-OUTPUT"));
    }
    Ok(())
}

fn tool_identity(actual: &Artifact, declared: &Tool) -> bool {
    matches!(declared, Tool::Available { artifact, .. }
        if actual.sha256 == artifact.sha256 && actual.bytes == artifact.bytes)
}

fn manifest_matches(manifest: &Manifest, control: &ControlSample) -> Result<(), String> {
    let primary = match control.side {
        Side::Baseline => &manifest.baseline,
        Side::Candidate => &manifest.candidate,
    };
    let topology = Topology {
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
    };
    if primary.profile != "external_native_control"
        || manifest.operation != Operation::LocalPcmRemux
        || manifest.transport_identity != "file_only"
        || manifest.archive_identity != NO_ARCHIVE
        || manifest.challenge_identity != NO_ARCHIVE
        || manifest.topology != topology
        || manifest.workers.len() != 1
        || !manifest
            .workers
            .get("ffprobe")
            .is_some_and(|tool| tool_identity(&control.probe_invocation.program, tool))
        || !tool_identity(&control.invocation.program, &manifest.ffmpeg)
        || !tool_identity(&control.probe_invocation.program, &manifest.ffprobe)
        || primary.artifact.sha256 != control.invocation.program.sha256
        || primary.artifact.bytes != control.invocation.program.bytes
        || manifest.corpus.sha256 != control.input.sha256
        || manifest.corpus.bytes != control.input.bytes
    {
        return Err(error("DOMAIN-MANIFEST"));
    }
    Ok(())
}

fn inventory(root: &Path, control: &ControlSample) -> Result<(), String> {
    let output = relative(root, &control.output.path)?;
    let directory = output.parent().ok_or_else(|| error("DOMAIN-OUTPUT"))?;
    let expected = output
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| error("DOMAIN-OUTPUT"))?;
    if !control.initial_inventory.is_empty()
        || control.final_inventory != [expected]
        || relative(root, &control.invocation.cwd)? != directory
        || relative(root, &control.probe_invocation.cwd)? != directory
    {
        return Err(error("DOMAIN-INVENTORY"));
    }
    let entries = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if entries.len() != 1
        || entries[0].file_name() != expected
        || !entries[0].file_type().map_err(|e| e.to_string())?.is_file()
    {
        return Err(error("DOMAIN-SIDECAR-OR-ARCHIVE"));
    }
    Ok(())
}

fn media(root: &Path, control: &ControlSample) -> Result<WaveFacts, String> {
    let input = calibration::wave_facts(&read_pin(root, &control.input, 4 * 1024 * 1024)?)?;
    let output = calibration::wave_facts(&read_pin(root, &control.output, 8 * 1024 * 1024)?)?;
    if input.channels != output.channels
        || input.sample_rate != output.sample_rate
        || input.bits_per_sample != output.bits_per_sample
        || input.frames != output.frames
        || input.payload_sha256 != output.payload_sha256
        || output != control.wave
    {
        return Err(error("DOMAIN-CHANGED-MEDIA"));
    }
    let probe: Probe = serde_json::from_slice(&read_pin(
        root,
        &control.probe_invocation.stdout,
        1024 * 1024,
    )?)
    .map_err(|e| error(&format!("DOMAIN-PROBE:{e}")))?;
    let [stream] = probe.streams.as_slice() else {
        return Err(error("DOMAIN-PROBE-STREAMS"));
    };
    let duration = probe
        .format
        .duration
        .parse::<f64>()
        .map_err(|_| error("DOMAIN-PROBE-DURATION"))?;
    if stream.index != 0
        || stream.codec_type != "audio"
        || stream.codec_name != "pcm_s16le"
        || stream.channels != output.channels
        || probe.format.name != "wav"
        || probe.format.size.parse::<u64>().ok() != Some(control.output.bytes)
        || !duration.is_finite()
        || (duration - number(output.frames) / f64::from(output.sample_rate)).abs() > 0.000_001
        || !probe.programs.is_empty()
        || !probe.stream_groups.is_empty()
    {
        return Err(error("DOMAIN-PROBE-WORK"));
    }
    Ok(output)
}

fn normalized_graph(input: &Artifact) -> Result<String, String> {
    use fforager_contracts::{
        AssetId, EntityIdentities, ExtensionMap, GraphLimits, ItemId, NodeId, NodeKind, Provenance,
        RepresentationId, SchemaVersion, SourceGraph, SourceNode, TriState,
    };
    let convert = |e: fforager_contracts::IdError| e.to_string();
    let node = NodeId::new(format!("node_{}", input.sha256)).map_err(convert)?;
    let graph = SourceGraph {
        schema: SchemaVersion { major: 1, minor: 0 },
        roots: vec![node.clone()],
        nodes: vec![SourceNode {
            id: node,
            kind: NodeKind::Media,
            identities: EntityIdentities {
                item: Some(ItemId::new(format!("item_{}", input.sha256)).map_err(convert)?),
                representations: vec![
                    RepresentationId::new(format!("repr_{}", input.sha256)).map_err(convert)?,
                ],
                assets: vec![AssetId::new(format!("asset_{}", input.sha256)).map_err(convert)?],
                ..EntityIdentities::default()
            },
            provenance: Provenance {
                extractor_key: MODE.into(),
                source_identity: input.sha256.clone(),
                canonical_url: format!("urn:sha256:{}", input.sha256),
                parent_node: None,
            },
            title: TriState::Unknown,
            description: TriState::Unknown,
            extensions: ExtensionMap::default(),
        }],
        edges: Vec::new(),
        continuations: Vec::new(),
    };
    graph
        .validate(GraphLimits::default())
        .map_err(|e| e.to_string())?;
    canonical_hash(&graph)
}

pub(super) fn read_envelope(root: &Path, dimension: &Dimension) -> Result<Envelope, String> {
    let [pin] = dimension.artifacts.as_slice() else {
        return Err(error("DOMAIN-ROLES"));
    };
    envelope(root, pin)
}

fn envelope(root: &Path, pin: &Artifact) -> Result<Envelope, String> {
    let envelope: Envelope = serde_json::from_slice(&read_pin(
        root,
        pin,
        u64::try_from(LIMIT).map_err(|_| error("LIMIT"))?,
    )?)
    .map_err(|e| error(&format!("DOMAIN-SCHEMA:{e}")))?;
    if envelope.schema_id != "ff.benchmark-domain@1"
        || envelope.operation != MODE
        || envelope.order > 1
        || envelope.pair > 52
    {
        return Err(error("DOMAIN-MODE"));
    }
    Ok(envelope)
}

pub(super) fn derive_all_control(
    root: &Path,
    manifest: &Manifest,
    control: &ControlSample,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    manifest_matches(manifest, control)?;
    captures(root, &control.invocation, false)?;
    captures(root, &control.probe_invocation, true)?;
    if control.invocation.parent_pid != control.probe_invocation.parent_pid
        || control.elapsed_ns != control.invocation.elapsed_ns
        || control.warmup != (control.pair < 3)
    {
        return Err(error("DOMAIN-TOPOLOGY"));
    }
    arguments(
        &control.invocation.argv,
        &[
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-n",
            "-protocol_whitelist",
            "file",
            "-f",
            "wav",
            "-i",
            "@input",
            "-map",
            "0:a:0",
            "-c:a",
            "copy",
            "-map_metadata",
            "-1",
            "-fflags",
            "+bitexact",
            "-f",
            "wav",
            "@output",
        ],
        control,
    )?;
    arguments(
        &control.probe_invocation.argv,
        &[
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-f",
            "wav",
            "-show_entries",
            "format=format_name,duration,size:stream=index,codec_type,codec_name,channels",
            "-of",
            "json",
            "@output",
        ],
        control,
    )?;
    inventory(root, control)?;
    let wave = media(root, control)?;
    DIMENSIONS
        .into_iter()
        .map(|name| {
            let facts = projection(name, control, &wave)?;
            Ok((
                name.to_owned(),
                BTreeMap::from([
                    ("operation".into(), MODE.into()),
                    ("normalized_work".into(), facts.to_string()),
                ]),
            ))
        })
        .collect()
}

fn projection(name: &str, control: &ControlSample, wave: &WaveFacts) -> Result<Value, String> {
    use serde_json::json;
    Ok(match name {
        "source_graph" => {
            json!({"normalized_source_graph_sha256":normalized_graph(&control.input)?,"source_files":1})
        }
        "request_sequence" => {
            json!({"protocol_allowlist":["file"],"http_requests":0,"http_retries":0,"input_files":1})
        }
        "bytes_and_fragments" => {
            json!({"input_bytes":control.input.bytes,"output_bytes":control.output.bytes,"payload_sha256":wave.payload_sha256,"file_fragments":1})
        }
        "selected_streams" => {
            json!({"index":0,"codec":"pcm_s16le","channels":wave.channels,"rate":wave.sample_rate,"bits":wave.bits_per_sample,"frames":wave.frames})
        }
        "output_and_ffprobe_facts" => {
            json!({"output_sha256":control.output.sha256,"bytes":control.output.bytes,"frames":wave.frames,"rate":wave.sample_rate,"single_audio_stream":true})
        }
        "metadata_and_sidecars" => {
            json!({"metadata_chunks":wave.metadata_chunks,"metadata_policy":"strip","sidecars":[],"owned_outputs":1})
        }
        "archive_action" => {
            json!({"operation":NO_ARCHIVE,"initial_owned_files":0,"final_owned_files":1})
        }
        "validation" => {
            json!({"pcm_payload_preserved":true,"independent_wave_decode":true,"actual_ffprobe_reconciled":true,"owned_inventory_reconciled":true})
        }
        "failure_class" => {
            json!({"ffmpeg_exit":control.invocation.exit_code,"ffprobe_exit":control.probe_invocation.exit_code,"stderr_bytes":0})
        }
        "cache_and_network_state" => {
            json!({"network":"file_protocol_only","os_page_cache":"uncontrolled","policy":"same_pinned_input_three_warmup_pairs"})
        }
        "process_topology" => {
            json!({"ordered_processes":[{"role":"ffmpeg","sha256":control.invocation.program.sha256},{"role":"ffprobe","sha256":control.probe_invocation.program.sha256}],"shared_owned_parent":true,"input_locators_per_invocation":1})
        }
        _ => return Err(error("DOMAIN-DIMENSION")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(root: &Path, name: &str, bytes: &[u8]) -> Artifact {
        fs::write(root.join(name), bytes).expect("component evidence file");
        Artifact {
            path: name.into(),
            sha256: crate::encode_sha256(bytes),
            bytes: u64::try_from(bytes.len()).expect("length"),
        }
    }

    // Constructed invocation records exercise normalization only. This fixture
    // never passes the native receipt boundary and proves no executed tool run.
    fn fixture(root: &Path, bytes: &[u8]) -> (Manifest, ControlSample) {
        fs::create_dir_all(root.join("work")).expect("owned work inventory");
        let input = pin(root, "input.wav", bytes);
        let output = pin(root, "work/output.wav", bytes);
        let wave = calibration::wave_facts(bytes).expect("real PCM bytes");
        let probe = serde_json::json!({"streams":[{"index":0,"codec_type":"audio","codec_name":"pcm_s16le","channels":wave.channels}],
            "format":{"format_name":"wav","duration":format!("{:.6}", number(wave.frames)/f64::from(wave.sample_rate)),"size":bytes.len().to_string()}});
        let probe_pin = pin(
            root,
            "probe.json",
            &serde_json::to_vec(&probe).expect("probe JSON"),
        );
        let empty = pin(root, "empty.txt", b"");
        let ffmpeg = pin(
            root,
            "semantic-ffmpeg-identity.txt",
            b"semantic input, not an executable",
        );
        let ffprobe = pin(
            root,
            "semantic-ffprobe-identity.txt",
            b"another semantic input, not an executable",
        );
        let execution_root = root.to_string_lossy().replace('\\', "/");
        let argv = vec![
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-n",
            "-protocol_whitelist",
            "file",
            "-f",
            "wav",
            "-i",
            "@input",
            "-map",
            "0:a:0",
            "-c:a",
            "copy",
            "-map_metadata",
            "-1",
            "-fflags",
            "+bitexact",
            "-f",
            "wav",
            "@output",
        ]
        .into_iter()
        .map(|s| match s {
            "@input" => format!("{execution_root}/input.wav"),
            "@output" => format!("{execution_root}/work/output.wav"),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>();
        let probe_argv = vec![
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-f",
            "wav",
            "-show_entries",
            "format=format_name,duration,size:stream=index,codec_type,codec_name,channels",
            "-of",
            "json",
            "@output",
        ]
        .into_iter()
        .map(|s| {
            if s == "@output" {
                format!("{execution_root}/work/output.wav")
            } else {
                s.into()
            }
        })
        .collect::<Vec<_>>();
        let invocation = serde_json::json!({"program":ffmpeg,"argv":argv,"cwd":"work","child_pid":101,"parent_pid":100,"exit_code":0,"elapsed_ns":1,"stdout":empty,"stderr":empty});
        let probe_invocation = serde_json::json!({"program":ffprobe,"argv":probe_argv,"cwd":"work","child_pid":102,"parent_pid":100,"exit_code":0,"elapsed_ns":1,"stdout":probe_pin,"stderr":empty});
        let control: ControlSample = serde_json::from_value(serde_json::json!({"execution_root":execution_root,"pair":3,"order":0,"side":"baseline","warmup":false,"elapsed_ns":1,
            "input":input,"output":output,"wave":wave,"probe":probe,"probe_capture":probe_pin,"invocation":invocation,"probe_invocation":probe_invocation,"initial_inventory":[],"final_inventory":["output.wav"]})).expect("semantic control shape");
        let manifest = manifest_fixture(root, input, ffmpeg, ffprobe);
        (manifest, control)
    }

    fn manifest_fixture(
        root: &Path,
        input: Artifact,
        ffmpeg: Artifact,
        ffprobe: Artifact,
    ) -> Manifest {
        let mut manifest = super::super::tests::fixture(root).manifest;
        manifest.operation = Operation::LocalPcmRemux;
        manifest.baseline.artifact = ffmpeg.clone();
        manifest.candidate.artifact = ffmpeg.clone();
        manifest.baseline.profile = "external_native_control".into();
        manifest.candidate.profile = "external_native_control".into();
        manifest.corpus = input;
        manifest.ffmpeg = Tool::Available {
            version: "semantic-only".into(),
            artifact: ffmpeg,
        };
        manifest.ffprobe = Tool::Available {
            version: "semantic-only".into(),
            artifact: ffprobe.clone(),
        };
        manifest.workers = BTreeMap::from([(
            "ffprobe".into(),
            Tool::Available {
                version: "semantic-only".into(),
                artifact: ffprobe,
            },
        )]);
        manifest.transport_identity = "file_only".into();
        manifest.archive_identity = NO_ARCHIVE.into();
        manifest.challenge_identity = NO_ARCHIVE.into();
        manifest.topology = Topology {
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
        };
        manifest
    }

    #[test]
    fn physical_domain_rejects_rehashed_cheaper_work_and_missing_effects() {
        let repo = crate::repo_root().expect("repository root");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = crate::disposable_artifact_root(&repo)
            .join(format!("benchmark-domain-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&root).expect("owned semantic stage");
        let bytes = fs::read(repo.join("build/fixtures/benchmarks/media.wav"))
            .expect("committed physical PCM fixture");
        let (manifest, control) = fixture(&root, &bytes);
        let facts =
            derive_all_control(&root, &manifest, &control).expect("semantic normalization only");
        assert_eq!(facts.len(), DIMENSIONS.len());
        let original = serde_json::to_value(&control).expect("control value");
        for mutation in [
            "metadata",
            "extra-request",
            "failed-exit",
            "shifted-parent",
            "missing-validation",
        ] {
            let mut changed: ControlSample =
                serde_json::from_value(original.clone()).expect("control clone");
            match mutation {
                "metadata" => {
                    let index = changed
                        .invocation
                        .argv
                        .iter()
                        .position(|s| s == "-map_metadata")
                        .expect("metadata work");
                    changed
                        .invocation
                        .argv
                        .drain(index..index + 2)
                        .for_each(drop);
                }
                "extra-request" => changed
                    .invocation
                    .argv
                    .push("https://invalid.example/extra".into()),
                "failed-exit" => changed.invocation.exit_code = 1,
                "shifted-parent" => changed.probe_invocation.parent_pid += 1,
                "missing-validation" => {
                    changed.probe_invocation.stdout.path = "absent-validation.json".into();
                }
                _ => unreachable!(),
            }
            assert!(
                derive_all_control(&root, &manifest, &changed).is_err(),
                "{mutation} admitted"
            );
        }
        fs::write(root.join("work/undeclared-sidecar.json"), b"{}").expect("extra actual effect");
        assert!(derive_all_control(&root, &manifest, &control).is_err());
        fs::remove_file(root.join("work/undeclared-sidecar.json")).expect("remove owned sidecar");
        let mut altered = bytes.clone();
        *altered.last_mut().expect("PCM payload") ^= 1;
        let mut changed: ControlSample = serde_json::from_value(original).expect("control clone");
        changed.output = pin(&root, "work/output.wav", &altered);
        changed.wave = calibration::wave_facts(&altered).expect("altered valid PCM");
        assert!(
            derive_all_control(&root, &manifest, &changed)
                .expect_err("rehashed quality change")
                .contains("DOMAIN-CHANGED-MEDIA")
        );
    }
}
