//! RED STUB (lane SMB-OBSERVE-DIAG-SINK-1): the API of the standalone host's durable diagnostics; it writes nothing.
//! The next commit fills it.
#![allow(unused_variables)]
use hsk_studio_observe::DeliveryClass;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const DIAG_SCHEMA: &str = "hsk.studio.diag@1";
pub const DIAG_DIR_ENV: &str = "STUDIO_DIAG_DIR";
pub const MAX_LINE_BYTES: usize = 8192;
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_FILES: usize = 16;
pub const MAX_CONSECUTIVE_FAILURES: u32 = 8;
pub const MAX_PANIC_RECORDS: u32 = 64;
pub const HEALTH_EXIT_DIAG_NOT_CONFIGURED: i32 = 3;
pub const HEALTH_EXIT_DIAG_UNAVAILABLE: i32 = 4;
pub const HEALTH_EXIT_DESCRIPTOR_MISMATCH: i32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagLimits {
    max_file_bytes: u64,
    max_files: usize,
}
impl DiagLimits {
    pub fn new(max_file_bytes: u64, max_files: usize) -> Self {
        Self { max_file_bytes, max_files }
    }
    pub fn max_file_bytes(self) -> u64 {
        self.max_file_bytes
    }
    pub fn max_files(self) -> usize {
        self.max_files
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OpLine<'a> {
    pub op: &'a str,
    pub n: u64,
    pub ok: bool,
    pub revision: Option<u64>,
    pub frames: u64,
    pub error: Option<&'a str>,
    pub correlation: Option<u64>,
    pub command_id: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagState {
    pub status: &'static str,
    pub code: Option<&'static str>,
    pub lines_written: u64,
    pub lines_dropped: u64,
    pub write_failures: u64,
    pub rotations: u64,
    pub files_removed: u64,
    pub remove_failures: u64,
}

#[derive(Clone, Debug)]
pub struct DiagSink {
    dir: PathBuf,
}
impl DiagSink {
    pub fn open(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
    }
    pub fn open_with(dir: &Path, limits: DiagLimits) -> Self {
        Self::open(dir)
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn state(&self) -> DiagState {
        DiagState { status: "stub", code: None, lines_written: 0, lines_dropped: 0, write_failures: 0, rotations: 0, files_removed: 0, remove_failures: 0 }
    }
    pub fn record_frame(&self, class: DeliveryClass, bytes: &[u8]) {}
    pub fn record_op(&self, line: &OpLine<'_>) {}
    pub fn record_run_start(&self, command: &str) {}
    pub fn record_run_end(&self, command: &str, exit: i32) {}
    pub fn record_health(&self) {}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Survey {
    pub files: u64,
    pub bytes: u64,
    pub panic_files: u64,
    pub unterminated_runs: u64,
    pub runs_surveyed: u64,
}
impl Survey {
    pub fn to_json(self) -> Value {
        json!({})
    }
}
pub fn survey(dir: &Path, own: Option<&DiagSink>) -> Survey {
    Survey::default()
}
pub fn install_process_sink(sink: &DiagSink) -> bool {
    false
}
pub fn process_sink() -> Option<DiagSink> {
    None
}
pub fn default_diag_dir() -> PathBuf {
    PathBuf::from("diag")
}
pub fn start_process(explicit: Option<PathBuf>) -> DiagSink {
    DiagSink::open(&explicit.unwrap_or_else(default_diag_dir))
}
pub fn install_panic_hook(sink: &DiagSink) {}
