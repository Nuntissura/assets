//! Same-build media controls. These measurements calibrate a host, never Ferric speed.
use super::{
    Artifact, Calibration, Deserialize, HostProfile, LIMIT, MAX_DURATION_NS, ObservationValue,
    Path, PathBuf, Random, SampleOrigin, Serialize, Side, artifact, bounded, canonical_hash, error,
    fs, hash, host_conditions, number, relative,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct WaveFacts {
    pub(super) channels: u16,
    pub(super) sample_rate: u32,
    pub(super) bits_per_sample: u16,
    pub(super) frames: u64,
    pub(super) payload_sha256: String,
    pub(super) metadata_chunks: Vec<(String, String)>,
}

pub(super) fn wave_facts(bytes: &[u8]) -> Result<WaveFacts, String> {
    if bytes.len() < 44
        || bytes.len() > 8 * 1024 * 1024
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
    {
        return Err(error("WAVE-HEADER"));
    }
    let word = |offset: usize| -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            bytes
                .get(offset..offset + 4)
                .ok_or_else(|| error("WAVE-BOUND"))?
                .try_into()
                .map_err(|_| error("WAVE-BOUND"))?,
        ))
    };
    if usize::try_from(word(4)?)
        .map_err(|_| error("WAVE-BOUND"))?
        .checked_add(8)
        != Some(bytes.len())
    {
        return Err(error("WAVE-LENGTH"));
    }
    let mut format = None;
    let mut payload = None;
    let mut metadata = Vec::new();
    let mut cursor = 12_usize;
    while cursor < bytes.len() {
        let length = usize::try_from(word(cursor + 4)?).map_err(|_| error("WAVE-BOUND"))?;
        let start = cursor.checked_add(8).ok_or_else(|| error("WAVE-BOUND"))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| error("WAVE-BOUND"))?;
        let body = bytes.get(start..end).ok_or_else(|| error("WAVE-BOUND"))?;
        match &bytes[cursor..cursor + 4] {
            b"fmt " if format.is_none() && body.len() >= 16 => {
                let short = |offset| u16::from_le_bytes([body[offset], body[offset + 1]]);
                let rate =
                    u32::from_le_bytes(body[4..8].try_into().map_err(|_| error("WAVE-FORMAT"))?);
                if short(0) != 1
                    || !(1..=2).contains(&short(2))
                    || short(14) != 16
                    || !(1..=384_000).contains(&rate)
                    || short(12) != short(2) * 2
                    || u32::from_le_bytes(body[8..12].try_into().map_err(|_| error("WAVE-FORMAT"))?)
                        != rate * u32::from(short(12))
                {
                    return Err(error("WAVE-PCM"));
                }
                format = Some((short(2), rate, short(14)));
            }
            b"data" if payload.is_none() => payload = Some(body),
            b"fmt " | b"data" => return Err(error("WAVE-DUPLICATE")),
            name => metadata.push((
                String::from_utf8(name.to_vec()).map_err(|_| error("WAVE-CHUNK"))?,
                super::super::encode_sha256(body),
            )),
        }
        cursor = end
            .checked_add(length & 1)
            .ok_or_else(|| error("WAVE-BOUND"))?;
        if metadata.len() > 32 {
            return Err(error("WAVE-CHUNK-BOUND"));
        }
    }
    let (channels, sample_rate, bits_per_sample) =
        format.ok_or_else(|| error("WAVE-MISSING-FORMAT"))?;
    let data = payload.ok_or_else(|| error("WAVE-MISSING-DATA"))?;
    if cursor != bytes.len() || data.is_empty() || data.len() % (usize::from(channels) * 2) != 0 {
        return Err(error("WAVE-ALIGNMENT"));
    }
    Ok(WaveFacts {
        channels,
        sample_rate,
        bits_per_sample,
        frames: u64::try_from(data.len() / (usize::from(channels) * 2))
            .map_err(|_| error("WAVE-BOUND"))?,
        payload_sha256: super::super::encode_sha256(data),
        metadata_chunks: metadata,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    streams: Vec<Stream>,
    format: Format,
    #[serde(default)]
    programs: Vec<serde_json::Value>,
    #[serde(default)]
    stream_groups: Vec<serde_json::Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stream {
    index: u32,
    codec_type: String,
    codec_name: String,
    channels: u16,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Format {
    #[serde(rename = "format_name")]
    name: String,
    duration: String,
    size: String,
}

fn probe_facts(text: &str, wave: &WaveFacts, length: u64) -> Result<Probe, String> {
    let probe: Probe =
        serde_json::from_str(text).map_err(|e| error(&format!("FFPROBE-SCHEMA:{e}")))?;
    let [stream] = probe.streams.as_slice() else {
        return Err(error("FFPROBE-STREAMS"));
    };
    let duration: f64 = probe
        .format
        .duration
        .parse()
        .map_err(|_| error("FFPROBE-DURATION"))?;
    let expected = number(wave.frames) / f64::from(wave.sample_rate);
    if stream.index != 0
        || stream.codec_type != "audio"
        || stream.codec_name != "pcm_s16le"
        || stream.channels != wave.channels
        || probe.format.name != "wav"
        || probe.format.size.parse::<u64>().ok() != Some(length)
        || !duration.is_finite()
        || (duration - expected).abs() > 0.000_001
        || !probe.programs.is_empty()
        || !probe.stream_groups.is_empty()
    {
        return Err(error("FFPROBE-CONCRETE-WORK"));
    }
    Ok(probe)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlSample {
    pub(super) execution_root: String,
    pub(super) pair: u32,
    pub(super) order: u32,
    pub(super) side: Side,
    pub(super) warmup: bool,
    pub(super) elapsed_ns: u64,
    pub(super) input: Artifact,
    pub(super) output: Artifact,
    pub(super) wave: WaveFacts,
    probe: Probe,
    probe_capture: Artifact,
    pub(super) invocation: ToolInvocation,
    pub(super) probe_invocation: ToolInvocation,
    pub(super) initial_inventory: Vec<String>,
    pub(super) final_inventory: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolInvocation {
    pub(super) program: Artifact,
    pub(super) argv: Vec<String>,
    pub(super) cwd: String,
    pub(super) child_pid: u32,
    pub(super) parent_pid: u32,
    pub(super) exit_code: i32,
    pub(super) elapsed_ns: u64,
    pub(super) stdout: Artifact,
    pub(super) stderr: Artifact,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeReceipt {
    schema_id: String,
    operation: String,
    profile_id: String,
    native_os: String,
    architecture: String,
    execution_root: String,
    pub(super) source: ControlSource,
    pub(super) source_tree: String,
    pub(super) lock_sha256: String,
    conditions_sha256: String,
    pub(super) ffmpeg: Artifact,
    pub(super) ffprobe: Artifact,
    pub(super) version_captures: Vec<Artifact>,
    pub(super) input: Artifact,
    raw_controls: Artifact,
    pub(super) producer_rustc: String,
    pub(super) producer_cargo: String,
    producer_artifact: Artifact,
    performance_claim: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlSource {
    pub(super) git_commit: String,
    dirty: bool,
    dirty_paths: Vec<String>,
    content_fingerprint: String,
}

fn write_pin(root: &Path, relative: &str, bytes: &[u8]) -> Result<Artifact, String> {
    fs::write(root.join(relative), bytes).map_err(|e| e.to_string())?;
    Ok(Artifact {
        path: relative.into(),
        sha256: super::super::encode_sha256(bytes),
        bytes: u64::try_from(bytes.len()).map_err(|_| error("LENGTH"))?,
    })
}

fn conditions(profile: &HostProfile) -> Result<String, String> {
    let mut value = serde_json::to_value(profile).map_err(|e| e.to_string())?;
    value
        .as_object_mut()
        .ok_or_else(|| error("HOST-SCHEMA"))?
        .remove("baseline_calibration");
    canonical_hash(&value)
}

fn source_lock(root: &Path, commit: &str) -> Result<String, String> {
    let bytes = super::super::command_output(
        root,
        "git",
        &["show", &format!("{commit}:build/Cargo.lock")],
    )?;
    Ok(super::super::encode_sha256(bytes.as_bytes()))
}

fn inventory(directory: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() || names.len() >= 4 {
            return Err(error("CONTROL-INVENTORY"));
        }
        names.push(
            entry
                .file_name()
                .into_string()
                .map_err(|_| error("CONTROL-INVENTORY"))?,
        );
    }
    names.sort();
    Ok(names)
}

fn invoke(
    root: &Path,
    program: &Artifact,
    argv: &[String],
    cwd: &str,
    capture: &str,
    deadline: Instant,
) -> Result<ToolInvocation, String> {
    use std::process::Stdio;
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(30));
    if timeout.is_zero() {
        return Err(error("CALIBRATION-DEADLINE"));
    }
    let stdout_path = format!("{capture}.stdout");
    let stderr_path = format!("{capture}.stderr");
    let stdout = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(&stdout_path))
        .map_err(|e| e.to_string())?;
    let stderr = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(&stderr_path))
        .map_err(|e| e.to_string())?;
    let mut command = super::super::Command::new(relative(root, &program.path)?);
    command
        .args(argv)
        .current_dir(relative(root, cwd)?)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    super::super::configure_quiet_process(&mut command);
    let started = Instant::now();
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let child_pid = child.id();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => return Err(fail_owned(&mut child, &format!("CONTROL-WAIT:{error}"))),
        }
        if started.elapsed() >= timeout {
            return Err(fail_owned(&mut child, "CONTROL-TIMEOUT"));
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).map_err(|_| error("DURATION"))?;
    let pin = |path: String| -> Result<Artifact, String> {
        let length = fs::metadata(root.join(&path))
            .map_err(|e| e.to_string())?
            .len();
        if length > 1024 * 1024 {
            return Err(error("CONTROL-CAPTURE-BOUND"));
        }
        Ok(Artifact {
            sha256: super::super::sha256_file(&root.join(&path))?,
            path,
            bytes: length,
        })
    };
    let result = ToolInvocation {
        program: program.clone(),
        argv: argv.to_vec(),
        cwd: cwd.into(),
        child_pid,
        parent_pid: std::process::id(),
        exit_code: status.code().ok_or_else(|| error("CONTROL-STATUS"))?,
        elapsed_ns,
        stdout: pin(stdout_path)?,
        stderr: pin(stderr_path)?,
    };
    if result.exit_code != 0 {
        return Err(error(&format!(
            "CONTROL-EXIT:{}:{}",
            result.exit_code, result.stderr.path
        )));
    }
    Ok(result)
}

fn fail_owned(child: &mut super::super::Child, cause: &str) -> String {
    // Only the child spawned above reaches the existing bounded tree cleanup.
    let termination = super::super::terminate_child_process_tree(child);
    let cleanup = super::super::cleanup_timed_out_child(child, &termination);
    format!("{}; {cleanup}", error(cause))
}

fn remux_args(input: &Path, output: &Path) -> Vec<String> {
    [
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
    ]
    .into_iter()
    .map(String::from)
    .chain([input.to_string_lossy().replace('\\', "/")])
    .chain(
        [
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
        ]
        .into_iter()
        .map(String::from),
    )
    .chain([output.to_string_lossy().replace('\\', "/")])
    .collect()
}

fn probe_args(output: &Path) -> Vec<String> {
    [
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
    ]
    .into_iter()
    .map(String::from)
    .chain([output.to_string_lossy().replace('\\', "/")])
    .collect()
}

pub(super) fn control_path(control: &ControlSample, pin: &Artifact) -> String {
    format!("{}/{}", control.execution_root, pin.path.replace('\\', "/"))
}

fn native_root(root: &str, native_os: &str) -> bool {
    bounded(root, 4096)
        && !root.ends_with('/')
        && !root.contains('\\')
        && !root
            .split('/')
            .any(|component| component == "." || component == "..")
        && if native_os == "windows" {
            root.as_bytes().get(1..3) == Some(b":/") && root.as_bytes()[0].is_ascii_alphabetic()
        } else {
            root.starts_with('/')
        }
}

pub(super) fn read_receipt(
    root: &Path,
    pin: &Artifact,
) -> Result<(NativeReceipt, Vec<ControlSample>), String> {
    if pin.bytes > LIMIT as u64 {
        return Err(error("CONTROL-JSON-BOUND"));
    }
    artifact(root, pin)?;
    let receipt: NativeReceipt =
        serde_json::from_slice(&fs::read(relative(root, &pin.path)?).map_err(|e| e.to_string())?)
            .map_err(|e| error(&format!("CONTROL-RECEIPT-SCHEMA:{e}")))?;
    if receipt.schema_id != "ff.benchmark-control-receipt@1"
        || receipt.operation != "local_pcm_remux"
        || receipt.performance_claim
        || receipt.source.dirty
        || !receipt.source.dirty_paths.is_empty()
        || !hash(&receipt.source.content_fingerprint, 64)
        || !hash(&receipt.source.git_commit, 40)
        || !hash(&receipt.source_tree, 40)
        || !hash(&receipt.lock_sha256, 64)
        || !matches!(receipt.native_os.as_str(), "windows" | "linux")
        || !bounded(&receipt.architecture, 32)
        || !bounded(&receipt.producer_rustc, 128)
        || !bounded(&receipt.producer_cargo, 128)
        || !native_root(&receipt.execution_root, &receipt.native_os)
    {
        return Err(error("CONTROL-RECEIPT"));
    }
    if receipt.raw_controls.bytes > LIMIT as u64 {
        return Err(error("CONTROL-JSON-BOUND"));
    }
    artifact(root, &receipt.raw_controls)?;
    let controls: Vec<ControlSample> = serde_json::from_slice(
        &fs::read(relative(root, &receipt.raw_controls.path)?).map_err(|e| e.to_string())?,
    )
    .map_err(|e| error(&format!("CONTROL-RAW-SCHEMA:{e}")))?;
    if controls.len() != 106 {
        return Err(error("CONTROL-COUNT"));
    }
    let mut random = Random(23);
    for (pair, values) in controls.chunks_exact(2).enumerate() {
        let baseline_first = random.next() & 1 == 0;
        for (order, value) in values.iter().enumerate() {
            let side = if (order == 0) == baseline_first {
                Side::Baseline
            } else {
                Side::Candidate
            };
            if value.pair as usize != pair
                || value.order as usize != order
                || value.side != side
                || value.warmup != (pair < 3)
                || value.elapsed_ns == 0
                || value.elapsed_ns > MAX_DURATION_NS
            {
                return Err(error("CONTROL-ORDER"));
            }
        }
    }
    Ok((receipt, controls))
}

fn check_control(
    root: &Path,
    receipt: &NativeReceipt,
    control: &ControlSample,
) -> Result<(), String> {
    if control.input.bytes > 8 * 1024 * 1024 || control.output.bytes > 8 * 1024 * 1024 {
        return Err(error("CONTROL-MEDIA-BOUND"));
    }
    if [
        &control.probe_capture,
        &control.invocation.stdout,
        &control.invocation.stderr,
        &control.probe_invocation.stdout,
        &control.probe_invocation.stderr,
    ]
    .iter()
    .any(|pin| pin.bytes > 1024 * 1024)
    {
        return Err(error("CONTROL-CAPTURE-BOUND"));
    }
    for pin in [
        &control.input,
        &control.output,
        &control.probe_capture,
        &control.invocation.stdout,
        &control.invocation.stderr,
        &control.probe_invocation.stdout,
        &control.probe_invocation.stderr,
    ] {
        artifact(root, pin)?;
    }
    let input =
        wave_facts(&fs::read(relative(root, &control.input.path)?).map_err(|e| e.to_string())?)?;
    let output =
        wave_facts(&fs::read(relative(root, &control.output.path)?).map_err(|e| e.to_string())?)?;
    let input_path = PathBuf::from(control_path(control, &control.input));
    let output_path = PathBuf::from(control_path(control, &control.output));
    let probe_text = fs::read_to_string(relative(root, &control.probe_invocation.stdout.path)?)
        .map_err(|e| e.to_string())?;
    let probe = probe_facts(&probe_text, &output, control.output.bytes)?;
    if control.execution_root != receipt.execution_root
        || control.input != receipt.input
        || control.wave != output
        || input.channels != output.channels
        || input.sample_rate != output.sample_rate
        || input.bits_per_sample != output.bits_per_sample
        || input.frames != output.frames
        || input.payload_sha256 != output.payload_sha256
        || serde_json::to_value(probe).map_err(|e| e.to_string())?
            != serde_json::to_value(&control.probe).map_err(|e| e.to_string())?
        || fs::read(relative(root, &control.probe_capture.path)?).map_err(|e| e.to_string())?
            != probe_text.as_bytes()
        || control.invocation.program != receipt.ffmpeg
        || control.probe_invocation.program != receipt.ffprobe
        || control.invocation.argv != remux_args(&input_path, &output_path)
        || control.probe_invocation.argv != probe_args(&output_path)
        || control.invocation.elapsed_ns != control.elapsed_ns
        || !control.initial_inventory.is_empty()
        || control.final_inventory != ["output.wav"]
        || control.invocation.cwd != control.probe_invocation.cwd
        || control.output.path.replace('\\', "/")
            != format!("{}/output.wav", control.invocation.cwd.replace('\\', "/"))
        || inventory(&root.join(&control.invocation.cwd))? != control.final_inventory
    {
        return Err(error("CONTROL-CONCRETE-WORK"));
    }
    for invocation in [&control.invocation, &control.probe_invocation] {
        if invocation.child_pid == 0
            || invocation.parent_pid == 0
            || invocation.child_pid == invocation.parent_pid
            || invocation.parent_pid != control.invocation.parent_pid
            || invocation.exit_code != 0
            || invocation.elapsed_ns == 0
            || invocation.elapsed_ns > MAX_DURATION_NS
        {
            return Err(error("CONTROL-INVOCATION"));
        }
    }
    Ok(())
}

fn binding(root: &Path, receipt: &NativeReceipt) -> Result<(), String> {
    let tree = super::super::command_output(
        root,
        "git",
        &[
            "rev-parse",
            &format!("{}^{{tree}}", receipt.source.git_commit),
        ],
    )?;
    super::super::command_output(
        root,
        "git",
        &[
            "merge-base",
            "--is-ancestor",
            &receipt.source.git_commit,
            "HEAD",
        ],
    )?;
    let producer_diff = super::super::command_output(
        root,
        "git",
        &[
            "diff",
            "--name-only",
            &receipt.source.git_commit,
            "--",
            "build/tools/fforager-xtask/src/benchmark.rs",
            "build/tools/fforager-xtask/src/benchmark",
            "build/tools/fforager-xtask/src/main.rs",
            "build/tools/fforager-xtask/Cargo.toml",
            "build/Cargo.toml",
            "build/Cargo.lock",
        ],
    )?;
    if tree.trim() != receipt.source_tree
        || !producer_diff.trim().is_empty()
        || source_lock(root, &receipt.source.git_commit)? != receipt.lock_sha256
    {
        return Err(error("CONTROL-SOURCE-BINDING"));
    }
    for pin in [
        &receipt.ffmpeg,
        &receipt.ffprobe,
        &receipt.input,
        &receipt.producer_artifact,
    ] {
        artifact(root, pin)?;
    }
    if receipt.version_captures.len() != 2 {
        return Err(error("CONTROL-VERSION"));
    }
    for pin in &receipt.version_captures {
        if pin.bytes > 1024 * 1024 {
            return Err(error("CONTROL-VERSION-BOUND"));
        }
        artifact(root, pin)?;
    }
    Ok(())
}

pub(super) fn verify(
    root: &Path,
    profile: &HostProfile,
    calibration: &Calibration,
) -> Result<Vec<ControlSample>, String> {
    if calibration.schema_id != "ff.benchmark-calibration@1"
        || calibration.profile_id != profile.id
        || calibration.origin != SampleOrigin::SuppliedMeasurement
    {
        return Err(error("CALIBRATION-SCHEMA"));
    }
    let pin = calibration
        .control_receipt
        .as_ref()
        .ok_or_else(|| error("CONTROL-RECEIPT-REQUIRED"))?;
    let (receipt, controls) = read_receipt(root, pin)?;
    binding(root, &receipt)?;
    if receipt.profile_id != profile.id
        || receipt.conditions_sha256 != conditions(profile)?
        || calibration.source_identity != pin.sha256
        || (profile.platform == "windows") != (receipt.native_os == "windows")
        || controls
            .iter()
            .filter(|c| !c.warmup)
            .map(|c| c.elapsed_ns)
            .collect::<Vec<_>>()
            != calibration.elapsed_ns
    {
        return Err(error("CONTROL-CALIBRATION-BINDING"));
    }
    for control in &controls {
        check_control(root, &receipt, control)?;
    }
    for pair in controls.chunks_exact(2) {
        if pair[0].output.sha256 != pair[1].output.sha256 || pair[0].wave != pair[1].wave {
            return Err(error("UNEQUAL-CONTROL-PAIR"));
        }
    }
    Ok(controls)
}

fn pinned_tool(
    root: &Path,
    source: &str,
    directory: &str,
    role: &str,
) -> Result<(PathBuf, Artifact), String> {
    let source = Path::new(source);
    if !source.is_absolute()
        || !source.is_file()
        || fs::metadata(source).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024
    {
        return Err(error("TOOL-PATH"));
    }
    let relative = format!("{directory}/{role}{}", std::env::consts::EXE_SUFFIX);
    let target = root.join(&relative);
    let before = super::super::sha256_file(source)?;
    fs::copy(source, &target).map_err(|e| e.to_string())?;
    if super::super::sha256_file(&target)? != before || super::super::sha256_file(source)? != before
    {
        return Err(error("TOOL-CHANGED"));
    }
    Ok((
        target,
        Artifact {
            path: relative,
            sha256: before,
            bytes: fs::metadata(source).map_err(|e| e.to_string())?.len(),
        },
    ))
}

#[derive(Debug)]
struct CalibrationInputs<'a> {
    host: &'a str,
    ffmpeg: &'a str,
    ffprobe: &'a str,
    fixture: &'a str,
    directory: &'a str,
}

fn calibration_inputs(arguments: &[String]) -> Result<CalibrationInputs<'_>, String> {
    let [
        host_flag,
        host,
        ffmpeg_flag,
        ffmpeg,
        probe_flag,
        ffprobe,
        fixture_flag,
        fixture,
        output_flag,
        directory,
    ] = arguments
    else {
        return Err("usage: benchmark-calibrate --host-profile <template.json> --ffmpeg <absolute> --ffprobe <absolute> --fixture <relative.wav> --output <new-artifact-directory>; native same-build controls, no performance claim".into());
    };
    if [
        host_flag.as_str(),
        ffmpeg_flag,
        probe_flag,
        fixture_flag,
        output_flag,
    ] != [
        "--host-profile",
        "--ffmpeg",
        "--ffprobe",
        "--fixture",
        "--output",
    ] {
        return Err(error("ARGUMENTS"));
    }
    Ok(CalibrationInputs {
        host,
        ffmpeg,
        ffprobe,
        fixture,
        directory,
    })
}

fn prepare_directory(root: &Path, directory: &str) -> Result<(), String> {
    let normalized = directory.replace('\\', "/");
    if !normalized.starts_with(".fforager-artifacts/")
        || Path::new(directory)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || root.join(directory).exists()
    {
        return Err(error("NEW-OWNED-DIRECTORY"));
    }
    fs::create_dir_all(root.join(directory)).map_err(|e| e.to_string())?;
    if !root
        .join(directory)
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(
            root.join(".fforager-artifacts")
                .canonicalize()
                .map_err(|e| e.to_string())?,
        )
    {
        return Err(error("DIRECTORY-ESCAPE"));
    }
    Ok(())
}

#[derive(Debug)]
struct CalibrationTools {
    ffmpeg: Artifact,
    ffprobe: Artifact,
    producer: Artifact,
    rustc: String,
    cargo: String,
}

fn preparation_timeout(deadline: Instant) -> Result<Duration, String> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(30));
    if remaining.is_zero() {
        return Err(error("PREPARATION-DEADLINE"));
    }
    Ok(remaining)
}

fn prepare_tools(
    root: &Path,
    inputs: &CalibrationInputs<'_>,
    deadline: Instant,
) -> Result<CalibrationTools, String> {
    let (_, ffmpeg) = pinned_tool(root, inputs.ffmpeg, inputs.directory, "ffmpeg")?;
    let (_, ffprobe) = pinned_tool(root, inputs.ffprobe, inputs.directory, "ffprobe")?;
    let producer_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (_, producer) = pinned_tool(
        root,
        producer_exe.to_str().ok_or_else(|| error("TOOL-UNICODE"))?,
        inputs.directory,
        "collector",
    )?;
    let rustc = super::super::command_output_with_timeout(
        root,
        "rustc",
        &["--version"],
        preparation_timeout(deadline)?,
    )?
    .trim()
    .to_owned();
    let cargo = super::super::command_output_with_timeout(
        root,
        "cargo",
        &["--version"],
        preparation_timeout(deadline)?,
    )?
    .trim()
    .to_owned();
    if !bounded(&rustc, 128) || !bounded(&cargo, 128) {
        return Err(error("PRODUCER-TOOLCHAIN"));
    }
    Ok(CalibrationTools {
        ffmpeg,
        ffprobe,
        producer,
        rustc,
        cargo,
    })
}

#[derive(Debug)]
struct CalibrationRun {
    profile: HostProfile,
    source: super::super::SourceState,
    directory: String,
    input: Artifact,
    input_wave: WaveFacts,
    tools: CalibrationTools,
    deadline: Instant,
    versions: Vec<Artifact>,
}

impl CalibrationRun {
    fn prepare(root: &Path, inputs: &CalibrationInputs<'_>) -> Result<Self, String> {
        let preparation_started = Instant::now();
        let profile_path = relative(root, inputs.host)?;
        if fs::metadata(&profile_path)
            .map_err(|e| e.to_string())?
            .len()
            > LIMIT as u64
        {
            return Err(error("HOST-JSON-BOUND"));
        }
        let profile: HostProfile =
            serde_json::from_slice(&fs::read(profile_path).map_err(|e| e.to_string())?)
                .map_err(|e| error(&format!("HOST-SCHEMA:{e}")))?;
        host_conditions(&profile)?;
        let preparation_deadline =
            preparation_started + Duration::from_millis(u64::from(profile.preparation_deadline_ms));
        if (profile.platform == "windows") != cfg!(windows)
            || (profile.platform == "unix") != cfg!(unix)
        {
            return Err(error("NATIVE-HOST"));
        }
        let source = super::super::source_state(root)?;
        if source.dirty {
            return Err(error("CLEAN-MEASUREMENT-SOURCE"));
        }
        prepare_directory(root, inputs.directory)?;
        let input_path = relative(root, inputs.fixture)?;
        if fs::metadata(&input_path).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
            return Err(error("MEDIA-BOUND"));
        }
        let bytes = fs::read(&input_path).map_err(|e| e.to_string())?;
        let input_wave = wave_facts(&bytes)?;
        let input = write_pin(root, &format!("{}/input.wav", inputs.directory), &bytes)?;
        let tools = prepare_tools(root, inputs, preparation_deadline)?;
        let ffmpeg_version = invoke(
            root,
            &tools.ffmpeg,
            &["-version".into()],
            inputs.directory,
            &format!("{}/ffmpeg-version", inputs.directory),
            preparation_deadline,
        )?;
        let probe_version = invoke(
            root,
            &tools.ffprobe,
            &["-version".into()],
            inputs.directory,
            &format!("{}/ffprobe-version", inputs.directory),
            preparation_deadline,
        )?;
        preparation_timeout(preparation_deadline)?;
        Ok(Self {
            profile,
            source,
            directory: inputs.directory.into(),
            input,
            input_wave,
            tools,
            deadline: Instant::now() + Duration::from_mins(15),
            versions: vec![ffmpeg_version.stdout, probe_version.stdout],
        })
    }

    fn collect(&self, root: &Path) -> Result<Vec<ControlSample>, String> {
        let mut samples = Vec::new();
        let mut random = Random(23);
        for pair in 0..53 {
            let baseline_first = random.next() & 1 == 0;
            for order in 0..2 {
                let side = if (order == 0) == baseline_first {
                    Side::Baseline
                } else {
                    Side::Candidate
                };
                samples.push(self.control(root, pair, order, side)?);
                write_pin(
                    root,
                    &format!("{}/raw-controls.json", self.directory),
                    &serde_json::to_vec_pretty(&samples).map_err(|e| e.to_string())?,
                )?;
            }
            let offset = samples.len() - 2;
            if samples[offset].wave != samples[offset + 1].wave
                || samples[offset].output.sha256 != samples[offset + 1].output.sha256
            {
                return Err(error("UNEQUAL-CONTROL-PAIR"));
            }
        }
        Ok(samples)
    }

    fn checked_output(&self, output: &Path) -> Result<(Vec<u8>, WaveFacts), String> {
        if fs::metadata(output).map_err(|e| e.to_string())?.len() > 8 * 1024 * 1024 {
            return Err(error("MEDIA-BOUND"));
        }
        let bytes = fs::read(output).map_err(|e| e.to_string())?;
        let wave = wave_facts(&bytes)?;
        if wave.channels != self.input_wave.channels
            || wave.sample_rate != self.input_wave.sample_rate
            || wave.bits_per_sample != self.input_wave.bits_per_sample
            || wave.frames != self.input_wave.frames
            || wave.payload_sha256 != self.input_wave.payload_sha256
        {
            return Err(error("CHANGED-MEDIA-WORK"));
        }
        Ok((bytes, wave))
    }

    fn control(
        &self,
        root: &Path,
        pair: u32,
        order: u32,
        side: Side,
    ) -> Result<ControlSample, String> {
        let case = format!("{}/pair-{pair}-{order}", self.directory);
        fs::create_dir(root.join(&case)).map_err(|e| e.to_string())?;
        let initial_inventory = inventory(&root.join(&case))?;
        let output_relative = format!("{case}/output.wav");
        let output = root.join(&output_relative);
        let remux_arguments = remux_args(&root.join(&self.input.path), &output);
        let invocation = invoke(
            root,
            &self.tools.ffmpeg,
            &remux_arguments,
            &case,
            &format!("{}/pair-{pair}-{order}-remux", self.directory),
            self.deadline,
        )?;
        let elapsed_ns = invocation.elapsed_ns;
        if elapsed_ns == 0 || elapsed_ns > MAX_DURATION_NS {
            return Err(error("MEDIA-BOUND"));
        }
        let (output_bytes, wave) = self.checked_output(&output)?;
        let probe_invocation = invoke(
            root,
            &self.tools.ffprobe,
            &probe_args(&output),
            &case,
            &format!("{}/pair-{pair}-{order}-probe", self.directory),
            self.deadline,
        )?;
        let probe_text = fs::read_to_string(relative(root, &probe_invocation.stdout.path)?)
            .map_err(|e| e.to_string())?;
        if probe_text.len() > 1024 * 1024 {
            return Err(error("FFPROBE-BOUND"));
        }
        let probe_capture = write_pin(
            root,
            &format!("{}/pair-{pair}-{order}-probe.json", self.directory),
            probe_text.as_bytes(),
        )?;
        let probe = probe_facts(
            &probe_text,
            &wave,
            u64::try_from(output_bytes.len()).map_err(|_| error("LENGTH"))?,
        )?;
        let output_pin = Artifact {
            path: output_relative,
            sha256: super::super::sha256_file(&output)?,
            bytes: u64::try_from(output_bytes.len()).map_err(|_| error("LENGTH"))?,
        };
        let final_inventory = inventory(&root.join(&case))?;
        if !initial_inventory.is_empty() || final_inventory != ["output.wav"] {
            return Err(error("CONTROL-INVENTORY"));
        }
        Ok(ControlSample {
            execution_root: root.to_string_lossy().replace('\\', "/"),
            pair,
            order,
            side,
            warmup: pair < 3,
            elapsed_ns,
            input: self.input.clone(),
            output: output_pin,
            wave,
            probe,
            probe_capture,
            invocation,
            probe_invocation,
            initial_inventory,
            final_inventory,
        })
    }

    fn update_profile(&mut self) -> Result<(), String> {
        self.profile.metrics.insert("wall_ns".into(), ObservationValue::Observed { value: "Native monotonic Instant: complete FFmpeg child spawn through exit/reap; inspection excluded".into() });
        self.profile.os = ObservationValue::Observed {
            value: format!(
                "Rust native OS={}; architecture={}; kernel/version not queried",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        };
        self.profile.machine = ObservationValue::Observed {
            value: format!(
                "architecture={}; available_parallelism={}; physical machine model not queried",
                std::env::consts::ARCH,
                std::thread::available_parallelism().map_err(|e| e.to_string())?
            ),
        };
        Ok(())
    }

    fn receipt(&self, root: &Path, samples: &[ControlSample]) -> Result<NativeReceipt, String> {
        Ok(NativeReceipt {
            schema_id: "ff.benchmark-control-receipt@1".into(),
            operation: "local_pcm_remux".into(),
            profile_id: self.profile.id.clone(),
            native_os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            execution_root: root.to_string_lossy().replace('\\', "/"),
            source_tree: super::super::command_output(root, "git", &["rev-parse", "HEAD^{tree}"])?
                .trim()
                .into(),
            lock_sha256: source_lock(root, &self.source.git_commit)?,
            source: ControlSource {
                git_commit: self.source.git_commit.clone(),
                dirty: self.source.dirty,
                dirty_paths: self.source.dirty_paths.clone(),
                content_fingerprint: self.source.content_fingerprint.clone(),
            },
            conditions_sha256: conditions(&self.profile)?,
            ffmpeg: self.tools.ffmpeg.clone(),
            ffprobe: self.tools.ffprobe.clone(),
            version_captures: self.versions.clone(),
            input: self.input.clone(),
            producer_rustc: self.tools.rustc.clone(),
            producer_cargo: self.tools.cargo.clone(),
            producer_artifact: self.tools.producer.clone(),
            raw_controls: write_pin(
                root,
                &format!("{}/raw-controls.json", self.directory),
                &serde_json::to_vec_pretty(samples).map_err(|e| e.to_string())?,
            )?,
            performance_claim: false,
        })
    }

    fn finish(mut self, root: &Path, samples: &[ControlSample]) -> Result<(), String> {
        let cleanup_started = Instant::now();
        if super::super::source_state(root)? != self.source {
            return Err(error("MEASUREMENT-SOURCE-CHANGED"));
        }
        artifact(root, &self.tools.ffmpeg)?;
        artifact(root, &self.tools.ffprobe)?;
        artifact(root, &self.input)?;
        self.update_profile()?;
        let receipt = self.receipt(root, samples)?;
        let control_receipt = write_pin(
            root,
            &format!("{}/identity.json", self.directory),
            &serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
        )?;
        let calibration = Calibration {
            schema_id: "ff.benchmark-calibration@1".into(),
            profile_id: self.profile.id.clone(),
            source_identity: control_receipt.sha256.clone(),
            origin: SampleOrigin::SuppliedMeasurement,
            elapsed_ns: samples
                .iter()
                .filter(|sample| !sample.warmup)
                .map(|sample| sample.elapsed_ns)
                .collect(),
            control_receipt: Some(control_receipt),
        };
        self.profile.baseline_calibration = write_pin(
            root,
            &format!("{}/calibration.json", self.directory),
            &serde_json::to_vec_pretty(&calibration).map_err(|e| e.to_string())?,
        )?;
        write_pin(
            root,
            &format!("{}/profile.json", self.directory),
            &serde_json::to_vec_pretty(&self.profile).map_err(|e| e.to_string())?,
        )?;
        verify(root, &self.profile, &calibration)?;
        if cleanup_started.elapsed()
            > Duration::from_millis(u64::from(self.profile.cleanup_deadline_ms))
        {
            return Err(error("CLEANUP-DEADLINE"));
        }
        println!(
            "BENCHMARK_CONTROL samples={} measured={} output={}; no product performance claim",
            samples.len(),
            calibration.elapsed_ns.len(),
            self.directory
        );
        Ok(())
    }
}

pub(crate) fn calibrate(root: &Path, arguments: &[String]) -> Result<(), String> {
    let inputs = calibration_inputs(arguments)?;
    let run = CalibrationRun::prepare(root, &inputs)?;
    let samples = run.collect(root)?;
    run.finish(root, &samples)
}
