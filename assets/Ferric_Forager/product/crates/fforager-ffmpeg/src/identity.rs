//! Executable identity acquisition and pre-spawn revalidation.

use crate::{
    bounded_io::drain_required_bytes,
    platform::{ExecutablePinExpectation, spawn_verified},
};
use fforager_contracts::{
    FfmpegExecutableKindV1, FfmpegHostArchitectureV1, FfmpegHostIdentityV1,
    FfmpegHostOperatingSystemV1,
};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::File,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

const HEX: &[u8; 16] = b"0123456789abcdef";
const MAXIMUM_PROBE_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAXIMUM_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_EXECUTABLE_HASH_TIMEOUT: Duration = Duration::from_secs(30);

/// Exact file observation used to invalidate stale capability-cache entries.
///
/// Fields are intentionally opaque: external callers can consume observations
/// returned by bounded discovery, but cannot fabricate trusted file identity.
///
/// ```compile_fail
/// use fforager_ffmpeg::ExecutableObservation;
/// let _forged = ExecutableObservation {
///     canonical_path: "C:\\forged.exe".to_owned(),
///     file_identity: "forged".to_owned(),
///     content_sha256: "00".repeat(32),
///     size_bytes: 1,
/// };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutableObservation {
    /// Canonical absolute path observed without following a final symlink.
    canonical_path: String,
    /// Platform file identity (`volume:file-index` or `device:inode`).
    file_identity: String,
    /// SHA-256 of the complete executable bytes.
    content_sha256: String,
    /// File length bound into the observation.
    size_bytes: u64,
}

impl ExecutableObservation {
    #[must_use]
    pub fn canonical_path(&self) -> &str {
        &self.canonical_path
    }

    #[must_use]
    pub fn file_identity(&self) -> &str {
        &self.file_identity
    }

    #[must_use]
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
}

/// Version and capability observation cryptographically bound to one file.
///
/// Capability discovery is intentionally not a public API. External callers
/// cannot supply an injected environment or working directory to the trusted
/// probe boundary:
///
/// ```compile_fail
/// use fforager_contracts::FfmpegExecutableKindV1;
/// use fforager_ffmpeg::observe_executable_capabilities;
/// use std::{path::Path, time::Duration};
///
/// let injected = vec![("SYSTEMROOT".to_owned(), "C:\\attacker".to_owned())];
/// let _ = observe_executable_capabilities(
///     Path::new("C:\\ffmpeg.exe"),
///     FfmpegExecutableKindV1::Ffmpeg,
///     &injected,
///     Path::new("C:\\attacker"),
///     Duration::from_secs(1),
/// );
/// ```
///
/// ```compile_fail
/// use fforager_ffmpeg::ExecutableCapabilityObservation;
/// # fn forge(executable: fforager_ffmpeg::ExecutableObservation,
/// #          host: fforager_contracts::FfmpegHostIdentityV1) {
/// let _forged = ExecutableCapabilityObservation {
///     executable,
///     host,
///     version_output_sha256: "00".repeat(32),
///     normalized_version: "forged".to_owned(),
///     normalized_probe_sha256: "00".repeat(32),
///     capabilities: vec!["forged".to_owned()],
/// };
/// # }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutableCapabilityObservation {
    executable: ExecutableObservation,
    host: FfmpegHostIdentityV1,
    version_output_sha256: String,
    normalized_version: String,
    normalized_probe_sha256: String,
    capabilities: Vec<String>,
}

impl ExecutableCapabilityObservation {
    #[must_use]
    pub const fn executable(&self) -> &ExecutableObservation {
        &self.executable
    }

    #[must_use]
    pub const fn host(&self) -> FfmpegHostIdentityV1 {
        self.host
    }

    #[must_use]
    pub fn version_output_sha256(&self) -> &str {
        &self.version_output_sha256
    }

    #[must_use]
    pub fn normalized_version(&self) -> &str {
        &self.normalized_version
    }

    #[must_use]
    pub fn normalized_probe_sha256(&self) -> &str {
        &self.normalized_probe_sha256
    }

    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    #[cfg(test)]
    pub(crate) fn test_fixture(
        executable: ExecutableObservation,
        host: FfmpegHostIdentityV1,
        version_output_sha256: String,
        normalized_version: String,
        normalized_probe_sha256: String,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            executable,
            host,
            version_output_sha256,
            normalized_version,
            normalized_probe_sha256,
            capabilities,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_mutate_executable_file_identity(&mut self, value: String) {
        self.executable.file_identity = value;
    }

    #[cfg(test)]
    pub(crate) fn test_mutate_executable_content(&mut self, value: String) {
        self.executable.content_sha256 = value;
    }

    #[cfg(test)]
    pub(crate) fn test_mutate_host(&mut self, value: FfmpegHostIdentityV1) {
        self.host = value;
    }

    #[cfg(test)]
    pub(crate) fn test_mutate_version_output(&mut self, value: String) {
        self.version_output_sha256 = value;
    }

    #[cfg(test)]
    pub(crate) fn test_mutate_normalized_version(&mut self, value: String) {
        self.normalized_version = value;
    }

    #[cfg(test)]
    pub(crate) fn test_mutate_probe(&mut self, value: String) {
        self.normalized_probe_sha256 = value;
    }

    #[cfg(test)]
    pub(crate) fn test_mutate_capabilities(&mut self, value: Vec<String>) {
        self.capabilities = value;
    }
}

#[cfg(test)]
impl ExecutableObservation {
    pub(crate) fn test_fixture(
        canonical_path: String,
        file_identity: String,
        content_sha256: String,
        size_bytes: u64,
    ) -> Self {
        Self {
            canonical_path,
            file_identity,
            content_sha256,
            size_bytes,
        }
    }
}

/// Failure while acquiring a stable executable observation.
#[derive(Debug)]
pub enum IdentityError {
    Io(std::io::Error),
    NotAbsolute,
    SymlinkRejected,
    NotRegularFile,
    ChangedDuringObservation,
    NonUnicodePath,
    UnsupportedCanonicalPath,
    ProbeFailed,
    ProbeOutputInvalid,
    ExecutableTooLarge,
    HashDeadlineExceeded,
    Platform(String),
}

/// Execute bounded version/capability probes through the same confined process
/// boundary used for media operations.
///
/// # Errors
///
/// Returns an identity, spawn, wait, containment, output-bound, or capability
/// parsing error. No caller-authored capability label is trusted.
#[allow(
    clippy::too_many_lines,
    reason = "the capability-cache observation keeps all bounded probes and their content binding in one auditable sequence"
)]
pub(crate) fn observe_executable_capabilities(
    path: &Path,
    kind: FfmpegExecutableKindV1,
    environment: &[(String, String)],
    working_directory: &Path,
    timeout: Duration,
) -> Result<ExecutableCapabilityObservation, IdentityError> {
    let executable = observe_executable_bounded(path, MAXIMUM_EXECUTABLE_BYTES, timeout)?;
    let version = run_probe(
        path,
        &executable,
        &["-version"],
        environment,
        working_directory,
        timeout,
    )?;
    let normalized_version_output = normalize_probe_output(&version)?;
    let normalized_version = normalized_version_output
        .lines()
        .next()
        .ok_or(IdentityError::ProbeOutputInvalid)?
        .to_owned();
    let probe_arguments: &[&str] = &["-hide_banner", "-h", "full"];
    let help = run_probe(
        path,
        &executable,
        probe_arguments,
        environment,
        working_directory,
        timeout,
    )?;
    let mut normalized_probe = normalize_probe_output(&help)?;
    let protocols = run_probe(
        path,
        &executable,
        &["-hide_banner", "-protocols"],
        environment,
        working_directory,
        timeout,
    )?;
    let normalized_protocols = normalize_probe_output(&protocols)?;
    let mut capabilities = Vec::new();
    match kind {
        FfmpegExecutableKindV1::Ffmpeg => {
            let demuxers = run_probe(
                path,
                &executable,
                &["-hide_banner", "-demuxers"],
                environment,
                working_directory,
                timeout,
            )?;
            let muxers = run_probe(
                path,
                &executable,
                &["-hide_banner", "-muxers"],
                environment,
                working_directory,
                timeout,
            )?;
            let normalized_demuxers = normalize_probe_output(&demuxers)?;
            let normalized_muxers = normalize_probe_output(&muxers)?;
            if listed_component(&normalized_demuxers, 'D', "aac") {
                capabilities.push("demuxer:aac".to_owned());
            }
            if listed_component(&normalized_demuxers, 'D', "h264") {
                capabilities.push("demuxer:h264".to_owned());
            }
            if listed_component(&normalized_muxers, 'E', "matroska") {
                capabilities.push("muxer:matroska".to_owned());
            }
            if listed_component(&normalized_muxers, 'E', "mp4") {
                capabilities.push("muxer:mp4".to_owned());
            }
            if normalized_probe.contains("-progress <url>")
                || normalized_probe
                    .contains("-progress url write program-readable progress information")
            {
                capabilities.push("progress".to_owned());
            }
            if (normalized_probe.contains("-c[:<stream_spec>] <codec>")
                || normalized_probe.contains("-c codec codec name"))
                && (normalized_probe.contains("'copy' to copy stream without reencoding")
                    || normalized_probe.contains("'copy' to copy stream"))
            {
                capabilities.push("stream_copy".to_owned());
            }
            normalized_probe.push_str("\n--demuxers--\n");
            normalized_probe.push_str(&normalized_demuxers);
            normalized_probe.push_str("\n--muxers--\n");
            normalized_probe.push_str(&normalized_muxers);
        }
        FfmpegExecutableKindV1::Ffprobe => {
            if (normalized_probe.contains("-output_format <format>")
                || normalized_probe
                    .contains("-output_format format set the output printing format"))
                && normalized_probe.to_ascii_lowercase().contains("json")
            {
                capabilities.push("json_output".to_owned());
            }
            if normalized_probe.contains("-show_streams") {
                capabilities.push("stream_metadata".to_owned());
            }
        }
    }
    if normalized_protocols.lines().any(|line| line.trim() == "fd") {
        capabilities.push("protocol:fd".to_owned());
    }
    normalized_probe.push_str("\n--protocols--\n");
    normalized_probe.push_str(&normalized_protocols);
    capabilities.sort();
    capabilities.dedup();
    if capabilities.is_empty() {
        return Err(IdentityError::ProbeOutputInvalid);
    }
    let after = observe_executable_bounded(path, MAXIMUM_EXECUTABLE_BYTES, timeout)?;
    if after != executable {
        return Err(IdentityError::ChangedDuringObservation);
    }
    Ok(ExecutableCapabilityObservation {
        executable,
        host: local_host_identity(),
        version_output_sha256: sha256_hex(normalized_version_output.as_bytes()),
        normalized_version,
        normalized_probe_sha256: sha256_hex(normalized_probe.as_bytes()),
        capabilities,
    })
}

pub(crate) fn local_host_identity() -> FfmpegHostIdentityV1 {
    let operating_system = if cfg!(windows) {
        FfmpegHostOperatingSystemV1::Windows
    } else {
        FfmpegHostOperatingSystemV1::Linux
    };
    #[cfg(target_arch = "x86_64")]
    let architecture = FfmpegHostArchitectureV1::X86_64;
    #[cfg(target_arch = "aarch64")]
    let architecture = FfmpegHostArchitectureV1::Aarch64;
    FfmpegHostIdentityV1 {
        operating_system,
        architecture,
    }
}

fn run_probe(
    path: &Path,
    expected: &ExecutableObservation,
    arguments: &[&str],
    environment: &[(String, String)],
    working_directory: &Path,
    timeout: Duration,
) -> Result<Vec<u8>, IdentityError> {
    let arguments = arguments
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let mut child = spawn_verified(
        path,
        &arguments,
        environment,
        working_directory,
        ExecutablePinExpectation {
            file_identity: &expected.file_identity,
            content_sha256: &expected.content_sha256,
            maximum_bytes: MAXIMUM_EXECUTABLE_BYTES,
            hash_timeout: timeout,
        },
    )
    .map_err(|error| IdentityError::Platform(error.to_string()))?;
    let (stdout, stderr) = match child.take_pipes() {
        Ok(pipes) => pipes,
        Err(primary) => {
            return match child.cleanup_force_reap(timeout.min(Duration::from_secs(2))) {
                Ok(_) => Err(IdentityError::Platform(primary.to_string())),
                Err(cleanup) => Err(IdentityError::Platform(format!(
                    "{primary}; bounded child cleanup also failed: {cleanup}"
                ))),
            };
        }
    };
    let stdout_worker =
        std::thread::spawn(move || drain_required_bytes(stdout, MAXIMUM_PROBE_BYTES));
    let stderr_worker =
        std::thread::spawn(move || drain_required_bytes(stderr, MAXIMUM_PROBE_BYTES));
    let cleanup_timeout = timeout.min(Duration::from_secs(2));
    let process_result = (|| {
        let exit = child
            .wait_timeout(timeout)
            .map_err(|error| IdentityError::Platform(error.to_string()))?
            .ok_or(IdentityError::ProbeFailed)?;
        if !exit.successful() {
            return Err(IdentityError::ProbeFailed);
        }
        let containment_deadline = Instant::now()
            .checked_add(cleanup_timeout)
            .unwrap_or_else(Instant::now);
        loop {
            if child
                .declared_scope_empty()
                .map_err(|error| IdentityError::Platform(error.to_string()))?
            {
                return Ok(());
            }
            if Instant::now() >= containment_deadline {
                return Err(IdentityError::ProbeFailed);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    if let Err(error) = process_result {
        if let Err(cleanup) = child.cleanup_force_reap(cleanup_timeout) {
            // Joining while cleanup is unproven can deadlock on inherited pipe
            // writers. Detach the readers and let PlatformChild::drop make its
            // independent final bounded cleanup attempt.
            drop(stdout_worker);
            drop(stderr_worker);
            return Err(IdentityError::Platform(format!(
                "{error:?}; bounded child cleanup also failed: {cleanup}"
            )));
        }
        let stdout_join = stdout_worker.join();
        let stderr_join = stderr_worker.join();
        let _stdout_drained = stdout_join;
        let _stderr_drained = stderr_join;
        return Err(error);
    }
    let stdout_join = stdout_worker.join();
    let stderr_join = stderr_worker.join();
    let mut stdout = stdout_join
        .map_err(|_| IdentityError::ProbeFailed)?
        .map_err(IdentityError::Io)?;
    let stderr = stderr_join
        .map_err(|_| IdentityError::ProbeFailed)?
        .map_err(IdentityError::Io)?;
    if stdout.is_empty() {
        stdout = stderr;
    } else if !stderr.is_empty() {
        stdout.extend_from_slice(b"\n--stderr--\n");
        stdout.extend_from_slice(&stderr);
    }
    Ok(stdout)
}

fn normalize_probe_output(bytes: &[u8]) -> Result<String, IdentityError> {
    let text = std::str::from_utf8(bytes).map_err(|_| IdentityError::ProbeOutputInvalid)?;
    let normalized = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.is_empty() {
        Err(IdentityError::ProbeOutputInvalid)
    } else {
        Ok(normalized)
    }
}

fn listed_component(output: &str, mode: char, name: &str) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split_ascii_whitespace();
        fields.next().is_some_and(|flags| flags.contains(mode)) && fields.next() == Some(name)
    })
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "executable identity error: {self:?}")
    }
}

impl std::error::Error for IdentityError {}

impl From<std::io::Error> for IdentityError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Observe an absolute regular executable and reject identity changes during hashing.
///
/// # Errors
///
/// Returns a typed error for relative paths, symlinks, non-files, I/O failure,
/// non-Unicode paths, or a file changed while its digest was acquired.
pub fn observe_executable(path: &Path) -> Result<ExecutableObservation, IdentityError> {
    observe_executable_bounded(
        path,
        MAXIMUM_EXECUTABLE_BYTES,
        DEFAULT_EXECUTABLE_HASH_TIMEOUT,
    )
}

pub(crate) fn observe_executable_bounded(
    path: &Path,
    maximum_bytes: u64,
    timeout: Duration,
) -> Result<ExecutableObservation, IdentityError> {
    if !path.is_absolute() {
        return Err(IdentityError::NotAbsolute);
    }
    let link_metadata = std::fs::symlink_metadata(path)?;
    if link_metadata.file_type().is_symlink() {
        return Err(IdentityError::SymlinkRejected);
    }
    if !link_metadata.is_file() {
        return Err(IdentityError::NotRegularFile);
    }
    let canonical = std::fs::canonicalize(path)?;
    let before = std::fs::metadata(&canonical)?;
    let before_identity = crate::platform::file_identity(&canonical, &before)?;
    let before_len = before.len();
    if before_len > maximum_bytes {
        return Err(IdentityError::ExecutableTooLarge);
    }
    if timeout.is_zero() {
        return Err(IdentityError::HashDeadlineExceeded);
    }

    let mut file = File::open(&canonical)?;
    let started = Instant::now();
    let mut total = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if started.elapsed() >= timeout {
            return Err(IdentityError::HashDeadlineExceeded);
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or(IdentityError::ExecutableTooLarge)?;
        if total > maximum_bytes {
            return Err(IdentityError::ExecutableTooLarge);
        }
        hasher.update(&buffer[..count]);
    }
    if started.elapsed() >= timeout {
        return Err(IdentityError::HashDeadlineExceeded);
    }

    let after = file.metadata()?;
    if total != before_len
        || before_len != after.len()
        || before_identity != crate::platform::file_identity(&canonical, &after)?
    {
        return Err(IdentityError::ChangedDuringObservation);
    }
    let canonical_path = contract_canonical_path(&canonical)?;
    let content_sha256 = hex_bytes(&hasher.finalize());
    Ok(ExecutableObservation {
        canonical_path,
        file_identity: before_identity,
        content_sha256,
        size_bytes: before_len,
    })
}

fn contract_canonical_path(path: &Path) -> Result<String, IdentityError> {
    let canonical = path.to_str().ok_or(IdentityError::NonUnicodePath)?;
    #[cfg(windows)]
    {
        let drive_path = canonical
            .strip_prefix(r"\\?\")
            .ok_or(IdentityError::UnsupportedCanonicalPath)?;
        let bytes = drive_path.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            return Ok(drive_path.to_owned());
        }
        Err(IdentityError::UnsupportedCanonicalPath)
    }
    #[cfg(not(windows))]
    Ok(canonical.to_owned())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

fn hex_bytes(digest: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_is_rejected_before_io() {
        assert!(matches!(
            observe_executable(Path::new("relative-tool")),
            Err(IdentityError::NotAbsolute)
        ));
    }

    #[test]
    fn executable_observation_enforces_byte_and_deadline_bounds() {
        let executable = std::env::current_exe().expect("current executable");
        assert!(matches!(
            observe_executable_bounded(&executable, 1, Duration::from_secs(1)),
            Err(IdentityError::ExecutableTooLarge)
        ));
        assert!(matches!(
            observe_executable_bounded(&executable, u64::MAX, Duration::ZERO),
            Err(IdentityError::HashDeadlineExceeded)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn verified_drive_namespace_path_is_normalized_but_other_namespaces_fail_closed() {
        assert_eq!(
            contract_canonical_path(Path::new(r"\\?\C:\trusted\ffmpeg.exe"))
                .expect("drive-local canonical path"),
            r"C:\trusted\ffmpeg.exe"
        );
        assert!(matches!(
            contract_canonical_path(Path::new(r"\\?\UNC\host\share\ffmpeg.exe")),
            Err(IdentityError::UnsupportedCanonicalPath)
        ));
        for rejected in [
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\ffmpeg.exe",
            r"\\.\C:\trusted\ffmpeg.exe",
            r"\\server\share\ffmpeg.exe",
            r"\??\C:\trusted\ffmpeg.exe",
            r"C:\trusted\ffmpeg.exe",
            r"\\?\C:relative\ffmpeg.exe",
        ] {
            assert!(
                matches!(
                    contract_canonical_path(Path::new(rejected)),
                    Err(IdentityError::UnsupportedCanonicalPath)
                ),
                "unexpectedly accepted {rejected}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn real_canonical_identity_uses_contract_safe_drive_form() {
        let executable = std::env::current_exe().expect("current executable");
        let observed = observe_executable(&executable).expect("stable executable observation");
        let bytes = observed.canonical_path.as_bytes();
        assert!(bytes.len() >= 3);
        assert!(bytes[0].is_ascii_alphabetic());
        assert_eq!(&bytes[1..3], b":\\");
        assert!(!observed.canonical_path.starts_with(r"\\"));
    }

    #[test]
    #[ignore = "requires explicit platform-native FFmpeg and ffprobe paths"]
    fn real_tool_capability_observations_are_content_bound() {
        let ffmpeg = std::env::var_os("FFORAGER_TEST_FFMPEG").expect("FFmpeg path");
        let ffprobe = std::env::var_os("FFORAGER_TEST_FFPROBE").expect("ffprobe path");
        let cwd = std::env::current_dir().expect("current directory");
        let temporary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .join(".fforager-artifacts/test-runs/ffmpeg-identity-capability");
        std::fs::create_dir_all(&temporary).expect("artifact temporary directory");
        let environment = if cfg!(windows) {
            let system_root = std::env::var("SYSTEMROOT").expect("SYSTEMROOT");
            let mut environment = Vec::new();
            crate::platform::append_windows_system_environment(&mut environment, &system_root)
                .expect("governed system environment");
            crate::platform::append_windows_writable_environment(
                &mut environment,
                &temporary.display().to_string(),
            );
            environment
        } else {
            vec![
                ("TMPDIR".to_owned(), temporary.display().to_string()),
                ("LANG".to_owned(), "C".to_owned()),
                ("LC_ALL".to_owned(), "C".to_owned()),
            ]
        };
        let ffmpeg = observe_executable_capabilities(
            Path::new(&ffmpeg),
            FfmpegExecutableKindV1::Ffmpeg,
            &environment,
            &cwd,
            Duration::from_secs(30),
        )
        .expect("FFmpeg capability observation");
        let ffprobe = observe_executable_capabilities(
            Path::new(&ffprobe),
            FfmpegExecutableKindV1::Ffprobe,
            &environment,
            &cwd,
            Duration::from_secs(30),
        )
        .expect("ffprobe capability observation");
        assert_eq!(
            ffmpeg.capabilities,
            [
                "demuxer:aac",
                "demuxer:h264",
                "muxer:matroska",
                "muxer:mp4",
                "progress",
                "protocol:fd",
                "stream_copy",
            ]
        );
        assert_eq!(
            ffprobe.capabilities,
            ["json_output", "protocol:fd", "stream_metadata"]
        );
        assert_ne!(
            ffmpeg.executable.content_sha256,
            ffprobe.executable.content_sha256
        );
    }
}
