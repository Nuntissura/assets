#![cfg(feature = "clap")]
//! Native CLAP proof (cargo feature `clap`): a REAL CLAP library is loaded by the isolated worker and
//! processes a block. The library is the standalone fixture `tests/fixtures/clap-gain` (a `clack-plugin`
//! 0.2.0 stereo gain of exactly 0.5 with no parameters), built OUTSIDE the Studio workspace into its own
//! target directory; its source defines the expected output independently of the host code under test.
//!
//! Where the library comes from (no env var is required): the oracle corpus convention of the workspace.
//! `.config/oracle-corpus.toml` (plain `key = value` lines) names the corpus `search` path; walk up from the
//! workspace root (at most `max-ancestors` parents) to the first ancestor that holds it, then read
//! `<corpus>/score-plugin/clap-gain/MANIFEST.json` (written by `tools/run-round.sh` from
//! `.config/oracle-fixtures.toml`, which builds `tests/fixtures/clap-gain` offline and locked). The test pins
//! the library by the manifest `sha256` and binds it to the current fixture source by `source_sha256`
//! (sha256 of the sorted `"<sha256>  <relpath>\n"` lines of `tests/fixtures/clap-gain/src`). The variable
//! `HSK_SCORE_PLUGIN_CLAP_FIXTURE` (a path to a locally built library) remains only as an optional local
//! override; it is never required.
//! When the corpus root, the manifest, the library, or a hash is absent or different, each test prints a line
//! `NOT_PROVEN: <test> <case> reason=...` and returns (V4-03: the round counts it as NOT_PROVEN, never PASS).
//!
//! Not proven even when green: latency, state capture, audio-port discovery and thread affinity of real
//! plugins (see the `clap_backend` module docs); only a gain plugin without parameters was run.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::{
    BlockSpec, ChannelLayout, Engine, InputBlock, OutputBlock, RequestContext, sha256, to_hex,
};
use hsk_studio_score_plugin::*;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

const WORKER: &str = env!("CARGO_BIN_EXE_score-plugin-worker");
const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";
const DEADLINE: Duration = Duration::from_secs(20);
const FIXTURE_ID: &str = "hsk.score-plugin.clap-gain-fixture";

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 120s deadline: {e}"),
    }
}

fn actor() -> ActorContext {
    ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session-clap",
    )
    .unwrap()
}

const CORPUS_CONFIG: &str = ".config/oracle-corpus.toml";
const FIXTURE_DEST: &str = "score-plugin/clap-gain";
const FIXTURE_SOURCE: &str = "tests/fixtures/clap-gain/src";

/// Value of a plain `key = value` line (quotes stripped, `#` comment lines ignored).
fn config_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key && !line.trim_start().starts_with('#'))
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

/// String value of `"key": "value"` in a small JSON object (the manifest is machine-written, flat).
fn json_string(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))?;
    let rest = text[at + key.len() + 2..].trim_start().strip_prefix(':')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

/// Corpus root by the workspace convention: start at the workspace root (two levels above this crate),
/// walk up at most `max-ancestors` parents, first ancestor holding the `search` directory.
fn corpus_root() -> Result<PathBuf, String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("workspace root not found")?;
    let config = std::fs::read_to_string(workspace.join(CORPUS_CONFIG))
        .map_err(|_| format!("{CORPUS_CONFIG} unreadable"))?;
    let search = config_value(&config, "search").ok_or("corpus config has no search key")?;
    let ancestors: usize = config_value(&config, "max-ancestors")
        .and_then(|v| v.parse().ok())
        .ok_or("corpus config has no max-ancestors key")?;
    workspace
        .ancestors()
        .take(ancestors.saturating_add(1))
        .map(|dir| dir.join(&search))
        .find(|dir| dir.is_dir())
        .ok_or_else(|| format!("no ancestor holds the corpus root within {ancestors} parents"))
}

/// sha256 over the sorted `"<sha256>  <relpath>\n"` lines of every file under `dir` (the steward's
/// `source_sha256` recipe).
fn tree_sha(dir: &Path) -> Option<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Option<()> {
        for entry in std::fs::read_dir(dir).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else {
                let rel = path
                    .strip_prefix(base)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, to_hex(&sha256(&std::fs::read(&path).ok()?))));
            }
        }
        Some(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort();
    let lines: String = files
        .iter()
        .map(|(rel, hex)| format!("{hex}  {rel}\n"))
        .collect();
    Some(to_hex(&sha256(lines.as_bytes())))
}

/// The fixture library resolved from the corpus manifest and verified, or a reason it cannot be proven.
fn corpus_fixture() -> Result<PathBuf, String> {
    let dir = corpus_root()?.join(FIXTURE_DEST);
    let manifest = std::fs::read_to_string(dir.join("MANIFEST.json"))
        .map_err(|_| format!("{FIXTURE_DEST}/MANIFEST.json absent"))?;
    let library = json_string(&manifest, "library").ok_or("manifest has no library")?;
    let pinned = json_string(&manifest, "sha256").ok_or("manifest has no sha256")?;
    let source_pinned =
        json_string(&manifest, "source_sha256").ok_or("manifest has no source_sha256")?;
    let path = dir.join(&library);
    let bytes = std::fs::read(&path).map_err(|_| format!("library {library} absent"))?;
    if to_hex(&sha256(&bytes)) != pinned {
        return Err(format!(
            "library {library} differs from the manifest sha256"
        ));
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_SOURCE);
    if tree_sha(&source).as_deref() != Some(source_pinned.as_str()) {
        return Err(
            "fixture source differs from the manifest source_sha256 (stale library)".to_owned(),
        );
    }
    Ok(path)
}

/// The fixture library, or `None` after printing the `NOT_PROVEN:` line the round counts (V4-03: a test
/// that cannot prove its claim must never be a silent green). `case` names the claim in that line.
fn fixture_path(test: &str, case: &str) -> Option<PathBuf> {
    if let Some(local) = std::env::var_os("HSK_SCORE_PLUGIN_CLAP_FIXTURE") {
        return Some(PathBuf::from(local)); // optional local override, never required
    }
    match corpus_fixture() {
        Ok(path) => Some(path),
        Err(reason) => {
            println!("NOT_PROVEN: {test} {case} reason={reason}");
            None
        }
    }
}

fn approved(path: PathBuf, content_sha256: [u8; 32]) -> ApprovedPlugin {
    ApprovedPlugin {
        path,
        content_sha256,
        format: PluginFormat::Clap,
        arch: Arch::host().expect("known arch"),
        abi_version: SUPPORTED_CLAP_ABI,
    }
}

fn config(plugin: ApprovedPlugin) -> ProcessConfig {
    let grant = PluginGrant {
        actor: actor(),
        content_sha256: plugin.content_sha256,
    };
    ProcessConfig {
        plugin,
        grant: Some(grant),
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        max_block_frames: 256,
        parameters: Vec::new(),
        max_state_bytes: 64,
    }
}

fn clap_host() -> IsolatedHost {
    let mut options = HostOptions::default().with_worker_program(WORKER);
    options.backend = WorkerBackend::Clap;
    IsolatedHost::new(options).unwrap()
}

/// Operating-system view: does process `pid` have a module with this file name mapped?
fn module_loaded_in(pid: u32, module: &str) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("tasklist")
            .args([
                "/M",
                module,
                "/FI",
                &format!("PID eq {pid}"),
                "/FO",
                "CSV",
                "/NH",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .expect("tasklist runs");
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string(format!("/proc/{pid}/maps")).is_ok_and(|maps| maps.contains(module))
    }
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}

/// A scratch file the test creates and removes.
struct Scratch(PathBuf);

impl Scratch {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "hsk-score-plugin-clap-{}-{}.bin",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::AcqRel)
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn clap_fixture_gain_runs_in_the_worker() {
    bounded_case("clap_fixture_gain_runs_in_the_worker", || {
        let Some(path) = fixture_path("clap_fixture_gain_runs_in_the_worker", "clap-gain") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("fixture library readable");
        let plugin = approved(path, sha256(&bytes));
        let host = clap_host();
        let cancel = CancellationToken::default();
        let grant = PluginGrant {
            actor: actor(),
            content_sha256: plugin.content_sha256,
        };

        // Probe: runs the plugin entry inside a worker that is then reaped.
        let descriptor = host
            .probe(&plugin, Some(&grant), &cancel, DEADLINE)
            .unwrap();
        assert_eq!(descriptor.plugin_id, FIXTURE_ID);
        assert!(
            descriptor.name.contains("Gain Fixture"),
            "{}",
            descriptor.name
        );
        assert_eq!(
            (
                descriptor.format,
                descriptor.abi_version,
                descriptor.provider_identity.as_str()
            ),
            (PluginFormat::Clap, 1, "clack-host@0.2.0")
        );
        let after_probe = host.stats();
        assert_eq!((after_probe.spawned, after_probe.workers_live), (1, 0));

        // Activate and process two consecutive blocks through a score Engine.
        let module = plugin
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut instance = host
            .activate_isolated(&config(plugin.clone()), &cancel, DEADLINE)
            .unwrap();
        // Isolation, read from the operating system: the plugin library is mapped in the worker
        // process and NOT in this (host) process.
        let worker_pid = instance.pid().unwrap();
        assert!(
            module_loaded_in(worker_pid, &module),
            "fixture must be loaded in the worker"
        );
        assert!(
            !module_loaded_in(std::process::id(), &module),
            "the host process must never load it"
        );
        assert_eq!(
            (
                instance.lifecycle(),
                instance.latency_samples(),
                instance.state_version()
            ),
            (LifecycleState::Active, 0, 1)
        );
        let left: Vec<f32> = (0..512).map(|n| (n % 251) as f32 / 256.0 - 0.5).collect();
        let right: Vec<f32> = left.iter().map(|s| -s).collect();
        let resource = DomainId::parse(RESOURCE).unwrap();
        let engine = Engine::new(resource.clone(), 7);
        let ctx = RequestContext {
            resource,
            actor: Some(actor()),
            expected_revision: 7,
        };
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        for block in 0..2_usize {
            let range = block * 256..(block + 1) * 256;
            let spec = BlockSpec {
                sample_rate_hz: 48_000,
                layout: ChannelLayout::stereo(),
                frames: 256,
                start_sample: (block * 256) as u64,
            };
            let channels: [&[f32]; 2] = [&left[range.clone()], &right[range.clone()]];
            let mut processor = PluginProcessor::new(&mut instance, DEADLINE);
            let receipt = engine
                .process(
                    &ctx,
                    &InputBlock::new(&spec, &channels),
                    &mut processor,
                    &mut out,
                    &CancellationToken::default(),
                )
                .unwrap_or_else(|e| panic!("block {block}: {e} / {:?}", processor.take_error()));
            let plugin_receipt = processor.last_receipt().unwrap().clone();
            assert_eq!(
                (receipt.processed_frames, receipt.latency_samples),
                (256, 0)
            );
            assert_eq!(&*plugin_receipt.provider_identity, "clack-host@0.2.0");
            // State extension is not wired: reported as unavailable, never as complete.
            assert_eq!(plugin_receipt.completeness, Completeness::Unavailable);
            let want_l: Vec<f32> = left[range.clone()].iter().map(|s| s * 0.5).collect();
            let want_r: Vec<f32> = right[range].iter().map(|s| s * 0.5).collect();
            assert_eq!(
                bits(out.published_channel(0).unwrap()),
                bits(&want_l),
                "block {block} L"
            );
            assert_eq!(
                bits(out.published_channel(1).unwrap()),
                bits(&want_r),
                "block {block} R"
            );
        }
        let capture = instance.capture_state(&cancel, DEADLINE, 64).unwrap();
        assert_eq!(
            (capture.completeness, capture.bytes.len()),
            (Completeness::Unavailable, 0)
        );

        assert!(!module_loaded_in(std::process::id(), &module));
        let report = instance.retire_in_place();
        assert_eq!(
            (
                report.blocks_processed,
                report.killed,
                report.reaped,
                report.exit_code
            ),
            (2, false, true, Some(0))
        );
        let s = host.stats();
        assert_eq!((s.workers_live, s.in_flight_bytes), (0, 0));
        assert!(host.ledger().iter().all(|e| e.reaped));
    });
}

#[test]
fn clap_refuses_tampered_and_non_library_files() {
    bounded_case("clap_refuses_tampered_and_non_library_files", || {
        let Some(path) = fixture_path("clap_refuses_tampered_and_non_library_files", "clap-gain")
        else {
            return;
        };
        let mut bytes = std::fs::read(&path).expect("fixture library readable");
        let honest_hash = sha256(&bytes);
        let cancel = CancellationToken::default();
        let host = clap_host();

        // A library that differs by one byte from the approved content is refused before any worker runs.
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let tampered = Scratch::new(&bytes);
        let error = host
            .activate(
                &config(approved(tampered.0.clone(), honest_hash)),
                &cancel,
                DEADLINE,
            )
            .err()
            .unwrap();
        assert_eq!(error, PluginError::HashMismatch);
        assert_eq!(host.stats().spawned, 0);

        // Approved bytes that are not a CLAP library: the worker's load fails, typed, and it is reaped.
        let not_a_library = Scratch::new(b"abc");
        let hash = sha256(b"abc");
        let error = host
            .activate(
                &config(approved(not_a_library.0.clone(), hash)),
                &cancel,
                DEADLINE,
            )
            .err()
            .unwrap();
        assert_eq!(error, PluginError::Unavailable);
        let s = host.stats();
        assert_eq!(
            (s.spawned, s.reaped, s.workers_live, s.in_flight_bytes),
            (1, 1, 0, 0)
        );
    });
}
