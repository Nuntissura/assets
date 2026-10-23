//! Small operating-system boundary authorized by `FF-DEC-003`.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(all(test, unix))]
pub(crate) use unix::spawn;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use unix::terminate_owned_test_process;
#[cfg(unix)]
pub(crate) use unix::{
    GovernedPathPin, InheritedFdBinding, PlatformChild, configure_cancellable_pipe,
    create_governed_output, file_identity, pin_governed_directory, pin_governed_file,
    spawn_verified, spawn_verified_with_bindings,
};
#[cfg(all(test, windows))]
pub(crate) use windows::spawn;
#[cfg(windows)]
pub(crate) use windows::{
    GovernedPathPin, PARENT_DEATH_HELPER_TEST, PARENT_DEATH_RECEIPT_ENV, PlatformChild,
    configure_cancellable_pipe, file_identity, observe_kill_on_job_close_parent_death,
    pin_governed_directory, pin_governed_file, run_handle_list_fault_probe, spawn_verified,
};

#[derive(Clone, Copy)]
pub(crate) struct ExecutablePinExpectation<'a> {
    pub file_identity: &'a str,
    pub content_sha256: &'a str,
    pub maximum_bytes: u64,
    pub hash_timeout: std::time::Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForceTerminationOutcome {
    Requested,
    AlreadyEmpty,
}

pub(crate) fn append_windows_writable_environment(
    values: &mut Vec<(String, String)>,
    temporary: &str,
) {
    // Absent known-folder variables can expand from literal templates such as
    // `%SystemDrive%` and create cache state relative to the child cwd.
    for name in [
        "TEMP",
        "TMP",
        "PROGRAMDATA",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
    ] {
        values.push((name.to_owned(), temporary.to_owned()));
    }
}

pub(crate) fn append_windows_system_environment(
    values: &mut Vec<(String, String)>,
    system_root: &str,
) -> Result<(), &'static str> {
    let bytes = system_root.as_bytes();
    if system_root.contains('%')
        || bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
    {
        return Err("trusted SYSTEMROOT is not ordinary drive absolute");
    }
    values.push(("SYSTEMROOT".to_owned(), system_root.to_owned()));
    values.push(("WINDIR".to_owned(), system_root.to_owned()));
    values.push(("SYSTEMDRIVE".to_owned(), system_root[..2].to_owned()));
    Ok(())
}

/// Typed direct-child wait result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitObservation {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Windows exposes an opaque numeric status through `GetExitCodeProcess`;
    /// without debugger events it cannot be called either an exit or exception.
    pub windows_status_opaque: Option<u32>,
    pub forced_by_supervisor: bool,
}

impl ExitObservation {
    #[must_use]
    pub fn successful(&self) -> bool {
        self.exit_code == Some(0) && self.signal.is_none() && self.windows_status_opaque.is_none()
    }
}

/// Platform process-boundary error.
#[derive(Debug)]
pub struct PlatformError {
    pub operation: &'static str,
    pub source: std::io::Error,
}

impl PlatformError {
    pub(crate) fn last(operation: &'static str) -> Self {
        Self {
            operation,
            source: std::io::Error::last_os_error(),
        }
    }

    pub(crate) fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self { operation, source }
    }

    pub(crate) fn state(operation: &'static str, message: impl Into<String>) -> Self {
        Self::io(
            operation,
            std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()),
        )
    }
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.source)
    }
}

impl std::error::Error for PlatformError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::observe_executable;
    use std::io::Write;
    use std::path::Path;

    fn artifact_root() -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .join(format!(
                ".fforager-artifacts/test-runs/ffmpeg-stale-executable-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).expect("artifact root");
        root
    }

    #[test]
    fn stale_or_missing_executable_is_rejected_at_platform_pin_before_spawn() {
        let root = artifact_root();
        let source = std::env::current_exe().expect("current test executable");
        let candidate = root.join(if cfg!(windows) {
            "candidate.exe"
        } else {
            "candidate"
        });
        let replacement_path = root.join(if cfg!(windows) {
            "replacement.exe"
        } else {
            "replacement"
        });
        std::fs::copy(&source, &candidate).expect("copy candidate executable");
        let expected = observe_executable(&candidate).expect("observe candidate");
        std::fs::copy(&source, &replacement_path).expect("copy distinct replacement");
        let replacement = observe_executable(&replacement_path).expect("observe replacement");
        assert_eq!(replacement.content_sha256(), expected.content_sha256());
        assert_ne!(replacement.file_identity(), expected.file_identity());

        let mut mutated = std::fs::OpenOptions::new()
            .append(true)
            .open(&candidate)
            .expect("open same-inode mutation");
        mutated.write_all(b"stale").expect("mutate candidate bytes");
        mutated.sync_all().expect("sync mutation");
        drop(mutated);
        let content_error = spawn_verified(
            &candidate,
            &[],
            &[],
            &root,
            ExecutablePinExpectation {
                file_identity: expected.file_identity(),
                content_sha256: expected.content_sha256(),
                maximum_bytes: crate::identity::MAXIMUM_EXECUTABLE_BYTES,
                hash_timeout: std::time::Duration::from_secs(30),
            },
        )
        .expect_err("same-inode content mutation must fail before spawn");
        assert_eq!(content_error.operation, "verify pinned executable");

        std::fs::remove_file(&candidate).expect("remove mutated candidate");
        std::fs::rename(&replacement_path, &candidate).expect("install distinct replacement");
        let identity_error = spawn_verified(
            &candidate,
            &[],
            &[],
            &root,
            ExecutablePinExpectation {
                file_identity: expected.file_identity(),
                content_sha256: expected.content_sha256(),
                maximum_bytes: crate::identity::MAXIMUM_EXECUTABLE_BYTES,
                hash_timeout: std::time::Duration::from_secs(30),
            },
        )
        .expect_err("same-path identity replacement must fail before spawn");
        assert_eq!(identity_error.operation, "verify pinned executable");

        std::fs::remove_file(&candidate).expect("remove replacement");
        let missing_error = spawn_verified(
            &candidate,
            &[],
            &[],
            &root,
            ExecutablePinExpectation {
                file_identity: expected.file_identity(),
                content_sha256: expected.content_sha256(),
                maximum_bytes: crate::identity::MAXIMUM_EXECUTABLE_BYTES,
                hash_timeout: std::time::Duration::from_secs(30),
            },
        )
        .expect_err("missing executable must fail before spawn");
        assert_eq!(missing_error.operation, "pin executable");
    }
}
