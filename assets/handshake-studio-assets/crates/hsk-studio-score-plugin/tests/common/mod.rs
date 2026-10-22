//! Shared helpers of the score-plugin integration tests (each test crate includes this module).
//!
//! Oracles the worker cannot edit: the gain/delay arithmetic is recomputed in the tests from the input
//! signal (exact binary fractions, so equality is bit-exact); the approved-file hash is the NIST SHA-256
//! vector of "abc"; worker liveness is read from the operating system (`tasklist`), not from the host's
//! own bookkeeping. Every child is one this test's host spawned; nothing opens a window.
#![allow(dead_code)]
use hsk_studio_accord::ActorContext;
use hsk_studio_score::{BlockSpec, ChannelLayout, OutputBlock};
use hsk_studio_score_plugin::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

pub const WORKER: &str = env!("CARGO_BIN_EXE_score-plugin-worker");
pub const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";
/// NIST FIPS 180-4 SHA-256("abc").
pub const ABC_SHA256: [u8; 32] = [
    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
    0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
];
pub const DEADLINE: Duration = Duration::from_secs(10);

pub fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
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

/// The approved "plugin" file ("abc"), removed when dropped (the test creates it, the test removes it).
pub struct PluginFile(pub PathBuf);

impl PluginFile {
    pub fn new() -> Self {
        Self::with_bytes(b"abc")
    }

    pub fn with_bytes(bytes: &[u8]) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "hsk-score-plugin-{}-{}.bin",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::AcqRel)
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }

    pub fn approved(&self) -> ApprovedPlugin {
        self.approved_with(ABC_SHA256)
    }

    pub fn approved_with(&self, content_sha256: [u8; 32]) -> ApprovedPlugin {
        ApprovedPlugin {
            path: self.0.clone(),
            content_sha256,
            format: PluginFormat::Clap,
            arch: Arch::host().expect("known arch"),
            abi_version: SUPPORTED_CLAP_ABI,
        }
    }
}

impl Drop for PluginFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn actor() -> ActorContext {
    ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session-7",
    )
    .unwrap()
}

pub fn grant() -> PluginGrant {
    PluginGrant {
        actor: actor(),
        content_sha256: ABC_SHA256,
    }
}

pub fn options(limits: HostLimits, latency: u32) -> HostOptions {
    let mut options = HostOptions::default().with_worker_program(WORKER);
    options.limits = limits;
    options.reference_latency = latency;
    options
}

pub fn host_with(limits: HostLimits, latency: u32) -> IsolatedHost {
    IsolatedHost::new(options(limits, latency)).unwrap()
}

pub fn host() -> IsolatedHost {
    host_with(HostLimits::default(), 0)
}

pub fn config(file: &PluginFile, gain: f64) -> ProcessConfig {
    ProcessConfig {
        plugin: file.approved(),
        grant: Some(grant()),
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        max_block_frames: 256,
        parameters: vec![Parameter { id: 0, value: gain }],
        max_state_bytes: 64,
    }
}

/// L[n] = ((n mod 251) / 256) - 0.5, R = -L: exact binary fractions, so gain 0.5 / 0.25 and the delay
/// are exact in f32.
pub fn signal(frames: usize) -> (Vec<f32>, Vec<f32>) {
    let left: Vec<f32> = (0..frames)
        .map(|n| (n % 251) as f32 / 256.0 - 0.5)
        .collect();
    let right = left.iter().map(|s| -s).collect();
    (left, right)
}

pub fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}

pub fn spec(frames: usize, start_sample: u64) -> BlockSpec {
    BlockSpec {
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        frames,
        start_sample,
    }
}

/// Operating-system view of a worker's liveness (not the host's bookkeeping).
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .creation_flags(0x0800_0000)
            .output()
            .expect("tasklist runs");
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    }
    #[cfg(not(windows))]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

pub fn silent(out: &OutputBlock, frames: usize) -> bool {
    (0..out.layout().channels()).all(|ch| {
        out.backing().chunks(out.capacity()).nth(ch).unwrap()[..frames]
            .iter()
            .all(|s| *s == 0.0)
    })
}

pub fn prefilled(frames: usize) -> OutputBlock {
    let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), frames).unwrap();
    for channel in out.channels_mut() {
        channel.fill(7.0);
    }
    out
}
