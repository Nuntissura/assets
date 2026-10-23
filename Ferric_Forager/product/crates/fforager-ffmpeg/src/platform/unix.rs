//! Linux process-group boundary. A process group is a signal scope, not hostile containment.

#![allow(
    unsafe_code,
    reason = "FF-DEC-003: all Unix process FFI is confined to this reviewed platform boundary"
)]

use super::{ExecutablePinExpectation, ExitObservation, ForceTerminationOutcome, PlatformError};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, c_char},
    fs::{File, Metadata, OpenOptions},
    io::Read,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    ptr::null_mut,
    time::{Duration, Instant},
};

pub(crate) fn file_identity(_path: &Path, metadata: &Metadata) -> Result<String, std::io::Error> {
    let inode = metadata.ino();
    if inode == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "zero inode cannot bind executable identity",
        ));
    }
    Ok(format!("unix:{}:{inode}", metadata.dev()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GovernedPathKind {
    File,
    Directory,
}

/// Retained no-follow descriptor binding governed media or its output directory.
#[derive(Debug)]
pub(crate) struct GovernedPathPin {
    file: File,
    identity: String,
    content_sha256: Option<String>,
    kind: GovernedPathKind,
    maximum_bytes: Option<u64>,
    hash_timeout: Duration,
}

impl GovernedPathPin {
    pub(crate) fn file_identity(&self) -> &str {
        &self.identity
    }

    pub(crate) fn content_sha256(&self) -> Option<&str> {
        self.content_sha256.as_deref()
    }

    pub(crate) fn file_size(&self) -> Result<u64, PlatformError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| PlatformError::io("stat governed descriptor", error))
    }

    pub(crate) fn verify_path(&self, path: &Path) -> Result<(), PlatformError> {
        let observed = pin_governed_path(path, self.kind, self.maximum_bytes, self.hash_timeout)?;
        if observed.identity != self.identity || observed.content_sha256 != self.content_sha256 {
            return Err(PlatformError::state(
                "verify governed path pin",
                "path identity or content changed",
            ));
        }
        Ok(())
    }

    pub(crate) fn finalize_written_content(&mut self) -> Result<(), PlatformError> {
        if self.kind != GovernedPathKind::File {
            return Err(PlatformError::state(
                "finalize governed output",
                "output pin is not a file",
            ));
        }
        let maximum = self.maximum_bytes.ok_or_else(|| {
            PlatformError::state("finalize governed output", "missing byte ceiling")
        })?;
        self.file
            .sync_all()
            .map_err(|error| PlatformError::io("sync governed output", error))?;
        self.content_sha256 = Some(hash_file_at(
            &self.file,
            maximum,
            self.hash_timeout,
            "hash governed output",
        )?);
        Ok(())
    }

    fn rewind_for_child(&self) -> Result<(), PlatformError> {
        // SAFETY: the retained descriptor is live and lseek only changes its
        // shared open-file-description offset before the child is created.
        let result = unsafe { libc::lseek(self.file.as_raw_fd(), 0, libc::SEEK_SET) };
        match result.cmp(&0) {
            std::cmp::Ordering::Equal => Ok(()),
            std::cmp::Ordering::Less => Err(PlatformError::last("rewind governed descriptor")),
            std::cmp::Ordering::Greater => Err(PlatformError::state(
                "rewind governed descriptor",
                "unexpected nonzero rewind offset",
            )),
        }
    }
}

#[derive(Debug)]
pub(crate) struct InheritedFdBinding<'a> {
    pin: &'a GovernedPathPin,
    target: i32,
}

impl<'a> InheritedFdBinding<'a> {
    pub(crate) fn new(pin: &'a GovernedPathPin, target: i32) -> Result<Self, PlatformError> {
        if !(64..=96).contains(&target) {
            return Err(PlatformError::state(
                "bind governed descriptor",
                "descriptor target is outside the audited runtime range",
            ));
        }
        Ok(Self { pin, target })
    }
}

pub(crate) fn pin_governed_file(
    path: &Path,
    maximum_bytes: u64,
    hash_timeout: Duration,
) -> Result<GovernedPathPin, PlatformError> {
    pin_governed_path(
        path,
        GovernedPathKind::File,
        Some(maximum_bytes),
        hash_timeout,
    )
}

pub(crate) fn pin_governed_directory(path: &Path) -> Result<GovernedPathPin, PlatformError> {
    pin_governed_path(path, GovernedPathKind::Directory, None, Duration::ZERO)
}

pub(crate) fn configure_cancellable_pipe(file: &File) -> Result<(), PlatformError> {
    // SAFETY: F_GETFL/F_SETFL operate on the exact live pipe descriptor.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(PlatformError::last("query pipe status flags"));
    }
    // SAFETY: the live descriptor remains owned by File; O_NONBLOCK is the
    // only flag added so a supervisor stop token can bound escaped writers.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(PlatformError::last("enable cancellable pipe reads"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn terminate_owned_test_process(
    process_id: i32,
    timeout: Duration,
) -> Result<(), PlatformError> {
    if process_id <= 1 || timeout.is_zero() {
        return Err(PlatformError::state(
            "terminate owned test process",
            "invalid PID or zero timeout",
        ));
    }
    // SAFETY: callers supply the exact PID of a process created by the
    // current regression test; this helper remains inside FF-DEC-003.
    if unsafe { libc::kill(process_id, libc::SIGKILL) } < 0 {
        let failure = std::io::Error::last_os_error();
        if failure.raw_os_error() != Some(libc::ESRCH) {
            return Err(PlatformError::io("terminate owned test process", failure));
        }
    }
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: signal zero only queries the exact owned PID and has no
        // process-side effect.
        if unsafe { libc::kill(process_id, 0) } < 0 {
            let failure = std::io::Error::last_os_error();
            if failure.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if failure.raw_os_error() != Some(libc::EPERM) {
                return Err(PlatformError::io("query terminated test process", failure));
            }
        }
        if Instant::now() >= deadline {
            return Err(PlatformError::state(
                "terminate owned test process",
                "PID absence was not proven before the deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn create_governed_output(
    parent: &GovernedPathPin,
    path: &Path,
    maximum_bytes: u64,
    hash_timeout: Duration,
) -> Result<GovernedPathPin, PlatformError> {
    if parent.kind != GovernedPathKind::Directory || maximum_bytes == 0 || hash_timeout.is_zero() {
        return Err(PlatformError::state(
            "create governed output",
            "invalid parent pin or output limits",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| PlatformError::state("create governed output", "output has no basename"))?;
    if name.as_bytes().contains(&b'/') || name.as_bytes().contains(&0) {
        return Err(PlatformError::state(
            "create governed output",
            "output basename is not a single Unix path component",
        ));
    }
    let name = CString::new(name.as_bytes())
        .map_err(|_| PlatformError::state("create governed output", "interior NUL"))?;
    // SAFETY: parent is a retained O_DIRECTORY descriptor and name is one
    // NUL-terminated basename. O_EXCL+O_NOFOLLOW makes creation fail closed.
    let descriptor = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(PlatformError::last("create governed output with openat"));
    }
    // SAFETY: successful openat returned one newly owned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file
        .metadata()
        .map_err(|error| PlatformError::io("stat governed output", error))?;
    if !metadata.is_file() || metadata.ino() == 0 || metadata.len() != 0 {
        return Err(PlatformError::state(
            "create governed output",
            "new output is not an empty identified regular file",
        ));
    }
    Ok(GovernedPathPin {
        file,
        identity: format!("unix:{}:{}", metadata.dev(), metadata.ino()),
        content_sha256: None,
        kind: GovernedPathKind::File,
        maximum_bytes: Some(maximum_bytes),
        hash_timeout,
    })
}

fn pin_governed_path(
    path: &Path,
    kind: GovernedPathKind,
    maximum_bytes: Option<u64>,
    hash_timeout: Duration,
) -> Result<GovernedPathPin, PlatformError> {
    let mut flags = libc::O_CLOEXEC | libc::O_NOFOLLOW;
    if kind == GovernedPathKind::Directory {
        flags |= libc::O_DIRECTORY;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|error| PlatformError::io("pin governed path", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| PlatformError::io("stat governed path", error))?;
    if metadata.is_dir() != (kind == GovernedPathKind::Directory)
        || (kind == GovernedPathKind::File && !metadata.is_file())
        || metadata.ino() == 0
    {
        return Err(PlatformError::state(
            "pin governed path",
            "governed path kind or identity invalid",
        ));
    }
    let identity = format!("unix:{}:{}", metadata.dev(), metadata.ino());
    let content_sha256 = if kind == GovernedPathKind::File {
        let maximum = maximum_bytes.ok_or_else(|| {
            PlatformError::state("pin governed file", "missing file byte ceiling")
        })?;
        if maximum == 0 || hash_timeout.is_zero() || metadata.len() > maximum {
            return Err(PlatformError::state(
                "pin governed file",
                "file exceeds byte ceiling or hash deadline is zero",
            ));
        }
        Some(hash_file_at(
            &file,
            maximum,
            hash_timeout,
            "hash governed file",
        )?)
    } else {
        None
    };
    Ok(GovernedPathPin {
        file,
        identity,
        content_sha256,
        kind,
        maximum_bytes,
        hash_timeout,
    })
}

fn hash_file_at(
    file: &File,
    maximum: u64,
    timeout: Duration,
    operation: &'static str,
) -> Result<String, PlatformError> {
    let metadata = file
        .metadata()
        .map_err(|error| PlatformError::io(operation, error))?;
    if metadata.len() > maximum {
        return Err(PlatformError::state(operation, "file exceeds byte ceiling"));
    }
    let started = Instant::now();
    let mut offset = 0_u64;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if started.elapsed() >= timeout {
            return Err(PlatformError::state(operation, "hash deadline exceeded"));
        }
        let count = file
            .read_at(&mut buffer, offset)
            .map_err(|error| PlatformError::io(operation, error))?;
        if count == 0 {
            break;
        }
        offset = offset
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| PlatformError::state(operation, "size overflow"))?;
        if offset > maximum {
            return Err(PlatformError::state(
                operation,
                "file exceeded byte ceiling",
            ));
        }
        digest.update(&buffer[..count]);
    }
    if offset != metadata.len() {
        return Err(PlatformError::state(
            operation,
            "file size changed while hashing",
        ));
    }
    Ok(hex_digest(digest.finalize()))
}

#[derive(Debug)]
pub(crate) struct PlatformChild {
    direct_child: libc::pid_t,
    process_group: libc::pid_t,
    stdout: Option<File>,
    stderr: Option<File>,
    exit: Option<ExitObservation>,
    forced: bool,
    attached_before_execution: bool,
    kill_on_close: bool,
}

#[cfg(target_os = "linux")]
#[cfg(test)]
pub(crate) fn spawn(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<PlatformChild, PlatformError> {
    spawn_internal(
        executable,
        arguments,
        environment,
        Some(working_directory),
        None,
        &[],
        None,
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn spawn_verified(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    expectation: ExecutablePinExpectation<'_>,
) -> Result<PlatformChild, PlatformError> {
    spawn_internal(
        executable,
        arguments,
        environment,
        Some(working_directory),
        Some(expectation),
        &[],
        None,
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn spawn_verified_with_bindings(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory_pin: &GovernedPathPin,
    temporary_directory_pin: &GovernedPathPin,
    expectation: ExecutablePinExpectation<'_>,
    bindings: &[InheritedFdBinding<'_>],
) -> Result<PlatformChild, PlatformError> {
    spawn_internal(
        executable,
        arguments,
        environment,
        None,
        Some(expectation),
        bindings,
        Some((working_directory_pin, temporary_directory_pin)),
    )
}

#[cfg(target_os = "linux")]
fn spawn_internal(
    executable_path: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: Option<&Path>,
    expectation: Option<ExecutablePinExpectation<'_>>,
    bindings: &[InheritedFdBinding<'_>],
    trusted_directories: Option<(&GovernedPathPin, &GovernedPathPin)>,
) -> Result<PlatformChild, PlatformError> {
    let executable_pin = pin_executable(executable_path, expectation)?;
    let executable = CString::new(format!("/proc/self/fd/{}", executable_pin.as_raw_fd()))
        .map_err(|_| PlatformError::state("encode pinned executable", "interior NUL"))?;
    let argument_zero = c_path(executable_path, "encode executable argument zero")?;
    let working_directory = working_directory
        .map(|path| c_path(path, "encode working directory"))
        .transpose()?;
    let arguments = c_arguments(&argument_zero, arguments)?;
    let environment = c_environment(environment)?;
    let mut argument_pointers = c_pointers(&arguments);
    let mut environment_pointers = c_pointers(&environment);
    let (stdout_read, stdout_write) = cloexec_pipe("create stdout pipe")?;
    let (stderr_read, stderr_write) = cloexec_pipe("create stderr pipe")?;
    verify_private_pipe_descriptors(&[&stdout_read, &stdout_write, &stderr_read, &stderr_write])?;

    let inherited = prepare_inherited_bindings(bindings)?;
    let inherited_directories = trusted_directories
        .map(prepare_inherited_directories)
        .transpose()?;
    let pipe_descriptors = [
        stdout_read.as_raw_fd(),
        stdout_write.as_raw_fd(),
        stderr_read.as_raw_fd(),
        stderr_write.as_raw_fd(),
    ];
    let mut actions = SpawnFileActions::new()?;
    configure_spawn_file_actions(
        &mut actions,
        executable_pin.as_raw_fd(),
        pipe_descriptors,
        &inherited,
        inherited_directories.as_ref(),
        working_directory.as_ref(),
    )?;

    let attributes = SpawnAttributes::process_group()?;
    let mut direct_child = 0;
    // SAFETY: every pointer references a live NUL-terminated allocation;
    // actions and attributes were initialized and remain live through spawn.
    let result = unsafe {
        libc::posix_spawn(
            &raw mut direct_child,
            executable.as_ptr(),
            actions.as_ptr(),
            attributes.as_ptr(),
            argument_pointers.as_mut_ptr(),
            environment_pointers.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(errno_result("posix_spawn", result));
    }
    drop(attributes);
    drop(actions);
    drop(stdout_write);
    drop(stderr_write);

    let process_group = verify_process_group(direct_child)?;
    Ok(PlatformChild {
        direct_child,
        process_group,
        stdout: Some(File::from(stdout_read)),
        stderr: Some(File::from(stderr_read)),
        exit: None,
        forced: false,
        attached_before_execution: true,
        kill_on_close: false,
    })
}

#[cfg(target_os = "linux")]
fn configure_spawn_file_actions(
    actions: &mut SpawnFileActions,
    executable_descriptor: i32,
    pipe_descriptors: [i32; 4],
    inherited: &[PreparedInheritedFd],
    inherited_directories: Option<&PreparedInheritedDirectories>,
    working_directory: Option<&CString>,
) -> Result<(), PlatformError> {
    let [stdout_read, stdout_write, stderr_read, stderr_write] = pipe_descriptors;
    actions.add_open_null_stdin()?;
    actions.add_dup2(stdout_write, libc::STDOUT_FILENO)?;
    actions.add_dup2(stderr_write, libc::STDERR_FILENO)?;
    let directory_targets: &[i32] = if inherited_directories.is_some() {
        &[97, 98]
    } else {
        &[]
    };
    for binding in inherited {
        if [
            executable_descriptor,
            stdout_read,
            stdout_write,
            stderr_read,
            stderr_write,
        ]
        .contains(&binding.target)
            || directory_targets.contains(&binding.target)
        {
            return Err(PlatformError::state(
                "verify child descriptor topology",
                "audited media target collides with executable or pipe source",
            ));
        }
    }
    if inherited_directories.is_some()
        && [
            executable_descriptor,
            stdout_read,
            stdout_write,
            stderr_read,
            stderr_write,
        ]
        .iter()
        .any(|descriptor| directory_targets.contains(descriptor))
    {
        return Err(PlatformError::state(
            "verify child descriptor topology",
            "trusted directory target collides with executable or pipe source",
        ));
    }
    for binding in inherited {
        actions.add_dup2(binding.source.as_raw_fd(), binding.target)?;
    }
    if let Some(directories) = inherited_directories {
        actions.add_dup2(
            directories.working.source.as_raw_fd(),
            directories.working.target,
        )?;
        actions.add_dup2(
            directories.temporary.source.as_raw_fd(),
            directories.temporary.target,
        )?;
    }
    for descriptor in pipe_descriptors {
        actions.add_close(descriptor)?;
    }
    for binding in inherited {
        actions.add_close(binding.source.as_raw_fd())?;
    }
    if let Some(directories) = inherited_directories {
        actions.add_close(directories.working.source.as_raw_fd())?;
        actions.add_close(directories.temporary.source.as_raw_fd())?;
        actions.add_fchdir(directories.working.target)?;
        actions.add_close(directories.working.target)?;
    } else if let Some(working_directory) = working_directory {
        actions.add_chdir(working_directory)?;
    } else {
        return Err(PlatformError::state(
            "bind child working directory",
            "missing pathname or trusted directory pin",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PreparedInheritedFd {
    source: OwnedFd,
    target: i32,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PreparedInheritedDirectories {
    working: PreparedInheritedFd,
    temporary: PreparedInheritedFd,
}

#[cfg(target_os = "linux")]
fn duplicate_pin_descriptor(
    pin: &GovernedPathPin,
    target: i32,
    operation: &'static str,
) -> Result<PreparedInheritedFd, PlatformError> {
    // SAFETY: the retained pin descriptor is live; fcntl returns a new owned fd.
    let duplicate = unsafe { libc::fcntl(pin.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 256) };
    if duplicate < 0 {
        return Err(PlatformError::last(operation));
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returned a uniquely owned descriptor.
    let source = unsafe { OwnedFd::from_raw_fd(duplicate) };
    Ok(PreparedInheritedFd { source, target })
}

#[cfg(target_os = "linux")]
fn prepare_inherited_directories(
    (working, temporary): (&GovernedPathPin, &GovernedPathPin),
) -> Result<PreparedInheritedDirectories, PlatformError> {
    if working.kind != GovernedPathKind::Directory || temporary.kind != GovernedPathKind::Directory
    {
        return Err(PlatformError::state(
            "prepare trusted directory descriptors",
            "working and temporary pins must both be directories",
        ));
    }
    Ok(PreparedInheritedDirectories {
        working: duplicate_pin_descriptor(working, 97, "duplicate working directory descriptor")?,
        temporary: duplicate_pin_descriptor(
            temporary,
            98,
            "duplicate temporary directory descriptor",
        )?,
    })
}

#[cfg(target_os = "linux")]
fn prepare_inherited_bindings(
    bindings: &[InheritedFdBinding<'_>],
) -> Result<Vec<PreparedInheritedFd>, PlatformError> {
    let mut targets = std::collections::BTreeSet::new();
    let mut prepared = Vec::with_capacity(bindings.len());
    for binding in bindings {
        if !targets.insert(binding.target) {
            return Err(PlatformError::state(
                "prepare inherited descriptor",
                "duplicate child descriptor target",
            ));
        }
        binding.pin.rewind_for_child()?;
        // Duplicate every source above the audited target range before any
        // posix_spawn action is installed. This eliminates source==target,
        // mapping cycles, and collisions with long-lived host descriptors.
        // SAFETY: the retained pin descriptor is live; fcntl returns a new fd.
        let duplicate =
            unsafe { libc::fcntl(binding.pin.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 256) };
        if duplicate < 0 {
            return Err(PlatformError::last("duplicate inherited descriptor"));
        }
        // SAFETY: successful F_DUPFD_CLOEXEC returned a uniquely owned fd.
        let source = unsafe { OwnedFd::from_raw_fd(duplicate) };
        if source.as_raw_fd() == binding.target {
            return Err(PlatformError::state(
                "prepare inherited descriptor",
                "collision-free descriptor allocation failed",
            ));
        }
        prepared.push(PreparedInheritedFd {
            source,
            target: binding.target,
        });
    }
    Ok(prepared)
}

#[cfg(target_os = "linux")]
#[allow(
    clippy::too_many_lines,
    reason = "the reviewed Unix pin keeps descriptor identity, byte ceiling, hash deadline, relocation, and content verification in one boundary"
)]
fn pin_executable(
    path: &Path,
    expectation: Option<ExecutablePinExpectation<'_>>,
) -> Result<File, PlatformError> {
    let maximum_bytes = expectation.map_or(crate::identity::MAXIMUM_EXECUTABLE_BYTES, |value| {
        value.maximum_bytes
    });
    let hash_timeout = expectation.map_or(Duration::from_secs(30), |value| value.hash_timeout);
    if hash_timeout.is_zero() {
        return Err(PlatformError::state(
            "hash pinned executable",
            "executable hash deadline is zero",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| PlatformError::io("pin executable", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| PlatformError::io("stat pinned executable", error))?;
    if !metadata.is_file() || metadata.ino() == 0 {
        return Err(PlatformError::state(
            "pin executable",
            "executable is not a regular identified file",
        ));
    }
    if metadata.len() > maximum_bytes {
        return Err(PlatformError::state(
            "hash pinned executable",
            "executable exceeds audited byte ceiling",
        ));
    }
    let observed_identity = format!("unix:{}:{}", metadata.dev(), metadata.ino());
    let started = Instant::now();
    let mut total = 0_u64;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if started.elapsed() >= hash_timeout {
            return Err(PlatformError::state(
                "hash pinned executable",
                "executable hash deadline exceeded",
            ));
        }
        let count = file
            .read(&mut buffer)
            .map_err(|error| PlatformError::io("hash pinned executable", error))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                PlatformError::state("hash pinned executable", "executable size overflow")
            })?;
        if total > maximum_bytes {
            return Err(PlatformError::state(
                "hash pinned executable",
                "executable exceeds audited byte ceiling",
            ));
        }
        digest.update(&buffer[..count]);
    }
    if started.elapsed() >= hash_timeout || total != metadata.len() {
        return Err(PlatformError::state(
            "hash pinned executable",
            "executable hash deadline or stable-size check failed",
        ));
    }
    let observed_digest = hex_digest(digest.finalize());
    if let Some(expected) = expectation
        && (observed_identity != expected.file_identity
            || observed_digest != expected.content_sha256)
    {
        return Err(PlatformError::state(
            "verify pinned executable",
            "identity or content changed at posix_spawn boundary",
        ));
    }
    let relocated = duplicate_fd_at_least(file.as_raw_fd(), 512, "relocate executable pin")?;
    drop(file);
    // SAFETY: duplicate_fd_at_least returned a unique owned descriptor.
    Ok(unsafe { File::from_raw_fd(relocated) })
}

#[cfg(target_os = "linux")]
fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(not(target_os = "linux"))]
#[cfg(test)]
pub(crate) fn spawn(
    _executable: &Path,
    _arguments: &[String],
    _environment: &[(String, String)],
    _working_directory: &Path,
) -> Result<PlatformChild, PlatformError> {
    Err(PlatformError::io(
        "spawn process group",
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "FF-DEC-003 authorizes the libc spawn boundary only for Linux-native proof",
        ),
    ))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn spawn_verified(
    _executable: &Path,
    _arguments: &[String],
    _environment: &[(String, String)],
    _working_directory: &Path,
    _expectation: ExecutablePinExpectation<'_>,
) -> Result<PlatformChild, PlatformError> {
    Err(PlatformError::io(
        "spawn verified process group",
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "verified executable spawning is supported only by Linux-native proof",
        ),
    ))
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct SpawnFileActions {
    value: libc::posix_spawn_file_actions_t,
    initialized: bool,
}

#[cfg(target_os = "linux")]
impl SpawnFileActions {
    fn new() -> Result<Self, PlatformError> {
        // SAFETY: zeroed storage is used only as the output of the init call.
        let mut value = unsafe { std::mem::zeroed() };
        // SAFETY: `value` is valid writable storage for the documented type.
        let result = unsafe { libc::posix_spawn_file_actions_init(&raw mut value) };
        if result != 0 {
            return Err(errno_result("initialize posix_spawn file actions", result));
        }
        Ok(Self {
            value,
            initialized: true,
        })
    }

    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        &raw const self.value
    }

    fn add_open_null_stdin(&mut self) -> Result<(), PlatformError> {
        const DEV_NULL: &[u8] = b"/dev/null\0";
        // SAFETY: actions are initialized and the path is NUL-terminated.
        let result = unsafe {
            libc::posix_spawn_file_actions_addopen(
                &raw mut self.value,
                libc::STDIN_FILENO,
                DEV_NULL.as_ptr().cast(),
                libc::O_RDONLY,
                0,
            )
        };
        check_errno("bind /dev/null stdin", result)
    }

    fn add_dup2(&mut self, source: i32, target: i32) -> Result<(), PlatformError> {
        // SAFETY: actions are initialized and both descriptors are owned or
        // reserved standard descriptors.
        let result =
            unsafe { libc::posix_spawn_file_actions_adddup2(&raw mut self.value, source, target) };
        check_errno("bind child pipe", result)
    }

    fn add_close(&mut self, descriptor: i32) -> Result<(), PlatformError> {
        // SAFETY: actions are initialized and the descriptor is an owned pipe.
        let result =
            unsafe { libc::posix_spawn_file_actions_addclose(&raw mut self.value, descriptor) };
        check_errno("close child pipe endpoint", result)
    }

    fn add_chdir(&mut self, directory: &CString) -> Result<(), PlatformError> {
        // SAFETY: actions are initialized and directory is NUL-terminated and live.
        let result = unsafe {
            libc::posix_spawn_file_actions_addchdir_np(&raw mut self.value, directory.as_ptr())
        };
        check_errno("bind child working directory", result)
    }

    fn add_fchdir(&mut self, directory: i32) -> Result<(), PlatformError> {
        // SAFETY: actions are initialized and the descriptor is installed by
        // an earlier dup2 action from one retained O_DIRECTORY pin.
        let result =
            unsafe { libc::posix_spawn_file_actions_addfchdir_np(&raw mut self.value, directory) };
        check_errno("bind pinned child working directory", result)
    }
}

#[cfg(target_os = "linux")]
impl Drop for SpawnFileActions {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: a successfully initialized action object is destroyed once.
            let _destroyed = unsafe { libc::posix_spawn_file_actions_destroy(&raw mut self.value) };
            self.initialized = false;
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct SpawnAttributes {
    value: libc::posix_spawnattr_t,
    initialized: bool,
}

#[cfg(target_os = "linux")]
impl SpawnAttributes {
    fn process_group() -> Result<Self, PlatformError> {
        // SAFETY: zeroed storage is used only as the output of the init call.
        let mut value = unsafe { std::mem::zeroed() };
        // SAFETY: `value` is valid writable storage for the documented type.
        let result = unsafe { libc::posix_spawnattr_init(&raw mut value) };
        if result != 0 {
            return Err(errno_result("initialize posix_spawn attributes", result));
        }
        let mut attributes = Self {
            value,
            initialized: true,
        };
        // POSIX specifies pgroup zero as a new group whose ID is the child PID.
        // SAFETY: attributes are initialized and the scalar pgroup is valid.
        check_errno("set posix_spawn process group", unsafe {
            libc::posix_spawnattr_setpgroup(&raw mut attributes.value, 0)
        })?;
        let flags = i16::try_from(libc::POSIX_SPAWN_SETPGROUP)
            .map_err(|_| PlatformError::state("set posix_spawn flags", "flag overflow"))?;
        // SAFETY: attributes are initialized and the documented flag is supplied.
        check_errno("set posix_spawn flags", unsafe {
            libc::posix_spawnattr_setflags(&raw mut attributes.value, flags)
        })?;
        Ok(attributes)
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        &raw const self.value
    }
}

#[cfg(target_os = "linux")]
impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: a successfully initialized attribute object is destroyed once.
            let _destroyed = unsafe { libc::posix_spawnattr_destroy(&raw mut self.value) };
            self.initialized = false;
        }
    }
}

#[cfg(target_os = "linux")]
fn c_path(path: &Path, operation: &'static str) -> Result<CString, PlatformError> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|error| PlatformError::state(operation, error.to_string()))
}

#[cfg(target_os = "linux")]
fn c_arguments(executable: &CString, arguments: &[String]) -> Result<Vec<CString>, PlatformError> {
    std::iter::once(Ok(executable.clone()))
        .chain(arguments.iter().map(|argument| {
            CString::new(argument.as_bytes())
                .map_err(|error| PlatformError::state("encode process argument", error.to_string()))
        }))
        .collect()
}

#[cfg(target_os = "linux")]
fn c_environment(environment: &[(String, String)]) -> Result<Vec<CString>, PlatformError> {
    environment
        .iter()
        .map(|(key, value)| {
            if key.is_empty() || key.contains('=') {
                return Err(PlatformError::state(
                    "encode process environment",
                    "invalid environment key",
                ));
            }
            CString::new(format!("{key}={value}")).map_err(|error| {
                PlatformError::state("encode process environment", error.to_string())
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn c_pointers(values: &[CString]) -> Vec<*mut c_char> {
    values
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(null_mut()))
        .collect()
}

#[cfg(target_os = "linux")]
fn cloexec_pipe(operation: &'static str) -> Result<(OwnedFd, OwnedFd), PlatformError> {
    let mut descriptors = [-1_i32; 2];
    // SAFETY: the two-element descriptor array is a valid output buffer.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(PlatformError::last(operation));
    }
    // SAFETY: successful pipe2 returned two uniquely owned descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    // SAFETY: successful pipe2 returned two uniquely owned descriptors.
    let write = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    let relocated_read = duplicate_fd_at_least(read.as_raw_fd(), 512, operation)?;
    // SAFETY: the successful first duplicate is immediately made owned so a
    // later write-end duplication failure cannot leak it.
    let relocated_read = unsafe { OwnedFd::from_raw_fd(relocated_read) };
    let relocated_write = duplicate_fd_at_least(write.as_raw_fd(), 512, operation)?;
    // SAFETY: the successful second duplicate is uniquely owned.
    let relocated_write = unsafe { OwnedFd::from_raw_fd(relocated_write) };
    drop(read);
    drop(write);
    Ok((relocated_read, relocated_write))
}

#[cfg(target_os = "linux")]
fn duplicate_fd_at_least(
    descriptor: i32,
    minimum: i32,
    operation: &'static str,
) -> Result<i32, PlatformError> {
    // SAFETY: descriptor is retained by its owner; fcntl returns a distinct
    // CLOEXEC descriptor no lower than the audited minimum.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, minimum) };
    if duplicate < minimum {
        if duplicate >= 0 {
            // SAFETY: a nonnegative unexpected result is still a newly owned fd.
            unsafe {
                libc::close(duplicate);
            }
        }
        Err(if duplicate < 0 {
            PlatformError::last(operation)
        } else {
            PlatformError::state(operation, "descriptor relocation violated minimum")
        })
    } else {
        Ok(duplicate)
    }
}

#[cfg(target_os = "linux")]
fn verify_private_pipe_descriptors(descriptors: &[&OwnedFd]) -> Result<(), PlatformError> {
    for descriptor in descriptors {
        let raw = descriptor.as_raw_fd();
        if raw <= libc::STDERR_FILENO {
            return Err(PlatformError::state(
                "verify private pipe descriptor",
                format!("expected descriptor above stderr, observed {raw}"),
            ));
        }
        // SAFETY: the descriptor is owned and F_GETFD does not mutate it.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        if flags < 0 {
            return Err(PlatformError::last("query pipe descriptor flags"));
        }
        if flags & libc::FD_CLOEXEC == 0 {
            return Err(PlatformError::state(
                "verify pipe descriptor close-on-exec",
                format!("descriptor {raw} was inheritable"),
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_process_group(direct_child: libc::pid_t) -> Result<libc::pid_t, PlatformError> {
    // SAFETY: direct_child came from successful posix_spawn and no pointer is used.
    let observed = unsafe { libc::getpgid(direct_child) };
    let observed = if observed < 0 {
        Err(PlatformError::last("verify dedicated process group"))
    } else {
        Ok(observed)
    };
    verify_process_group_observation(direct_child, observed, PrehandoffWaitFault::None)
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrehandoffWaitFault {
    None,
    #[cfg(test)]
    InterruptedOnce,
    #[cfg(test)]
    FailedOnce,
}

#[cfg(target_os = "linux")]
fn verify_process_group_observation(
    direct_child: libc::pid_t,
    observed: Result<libc::pid_t, PlatformError>,
    wait_fault: PrehandoffWaitFault,
) -> Result<libc::pid_t, PlatformError> {
    let primary = match observed {
        Ok(process_group) if process_group == direct_child => return Ok(direct_child),
        Ok(process_group) => PlatformError::state(
            "verify dedicated process group",
            format!("expected pgid {direct_child}, observed {process_group}"),
        ),
        Err(error) => error,
    };
    match cleanup_unhanded_direct_child(direct_child, wait_fault) {
        Ok(()) => Err(primary),
        Err(cleanup) => Err(PlatformError::state(
            "verify dedicated process group cleanup",
            format!("primary: {primary}; cleanup: {cleanup}"),
        )),
    }
}

#[cfg(target_os = "linux")]
fn cleanup_unhanded_direct_child(
    direct_child: libc::pid_t,
    mut wait_fault: PrehandoffWaitFault,
) -> Result<(), PlatformError> {
    // This PID came directly from successful posix_spawn and has not been
    // handed to any caller. Never broaden cleanup to the unverified group.
    // SAFETY: direct_child is the exact positive PID returned by posix_spawn.
    if unsafe { libc::kill(direct_child, libc::SIGKILL) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(PlatformError::io("terminate unhanded direct child", error));
        }
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .unwrap_or_else(Instant::now);
    loop {
        let injected_error = match take_prehandoff_wait_fault(&mut wait_fault) {
            PrehandoffWaitFault::None => None,
            #[cfg(test)]
            PrehandoffWaitFault::InterruptedOnce => {
                Some(std::io::Error::from(std::io::ErrorKind::Interrupted))
            }
            #[cfg(test)]
            PrehandoffWaitFault::FailedOnce => {
                Some(std::io::Error::other("injected waitpid failure"))
            }
        };
        let result = if let Some(error) = injected_error {
            Err(error)
        } else {
            let mut status = 0;
            // SAFETY: exact direct child and initialized status output; WNOHANG
            // keeps each attempt bounded so the outer deadline remains effective.
            let observed = unsafe { libc::waitpid(direct_child, &raw mut status, libc::WNOHANG) };
            if observed < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(observed)
            }
        };
        match result {
            Ok(observed) if observed == direct_child => return Ok(()),
            Ok(0) => {}
            Ok(observed) => {
                return Err(PlatformError::state(
                    "reap unhanded direct child",
                    format!("expected pid {direct_child}, observed {observed}"),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(PlatformError::io("reap unhanded direct child", error));
            }
        }
        if Instant::now() >= deadline {
            return Err(PlatformError::state(
                "reap unhanded direct child",
                "bounded reap deadline exceeded",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn take_prehandoff_wait_fault(fault: &mut PrehandoffWaitFault) -> PrehandoffWaitFault {
    let current = *fault;
    *fault = PrehandoffWaitFault::None;
    current
}

#[cfg(target_os = "linux")]
fn check_errno(operation: &'static str, result: i32) -> Result<(), PlatformError> {
    if result == 0 {
        Ok(())
    } else {
        Err(errno_result(operation, result))
    }
}

#[cfg(target_os = "linux")]
fn errno_result(operation: &'static str, errno: i32) -> PlatformError {
    PlatformError::io(operation, std::io::Error::from_raw_os_error(errno))
}

impl PlatformChild {
    pub(crate) fn take_pipes(&mut self) -> Result<(File, File), PlatformError> {
        let stdout = self
            .stdout
            .take()
            .ok_or_else(|| PlatformError::state("take stdout", "stdout already taken"))?;
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| PlatformError::state("take stderr", "stderr already taken"))?;
        Ok((stdout, stderr))
    }

    pub(crate) fn request_graceful_stop(&mut self) -> Result<bool, PlatformError> {
        if self.declared_scope_empty()? {
            return Ok(true);
        }
        self.signal_group(libc::SIGTERM, "send SIGTERM to process group")?;
        Ok(true)
    }

    pub(crate) fn force_terminate(&mut self) -> Result<ForceTerminationOutcome, PlatformError> {
        if self.declared_scope_empty()? {
            return Ok(ForceTerminationOutcome::AlreadyEmpty);
        }
        self.signal_group(libc::SIGKILL, "send SIGKILL to process group")?;
        self.forced = true;
        Ok(ForceTerminationOutcome::Requested)
    }

    fn signal_group(&self, signal: i32, operation: &'static str) -> Result<(), PlatformError> {
        // SAFETY: the positive process-group id was independently verified
        // immediately after spawn and is negated to address exactly that group.
        let result = unsafe { libc::kill(-self.process_group, signal) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) && self.declared_scope_empty()? {
            Ok(())
        } else {
            Err(PlatformError::io(operation, error))
        }
    }

    pub(crate) fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ExitObservation>, PlatformError> {
        if let Some(exit) = &self.exit {
            return Ok(Some(exit.clone()));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let mut status = 0;
            // SAFETY: exact direct child and initialized status output; WNOHANG bounds the call.
            let result =
                unsafe { libc::waitpid(self.direct_child, &raw mut status, libc::WNOHANG) };
            if result == self.direct_child {
                let observation = decode_wait_status(status, self.forced)?;
                self.exit = Some(observation.clone());
                return Ok(Some(observation));
            }
            if result < 0 {
                return Err(PlatformError::last("waitpid direct child"));
            }
            if result != 0 {
                return Err(PlatformError::state(
                    "waitpid direct child",
                    format!(
                        "expected pid {} or zero, observed {result}",
                        self.direct_child
                    ),
                ));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub(crate) fn declared_scope_empty(&self) -> Result<bool, PlatformError> {
        // SAFETY: signal zero probes only the exact verified process group.
        let result = unsafe { libc::kill(-self.process_group, 0) };
        if result == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(true),
            Some(libc::EPERM) => Ok(false),
            _ => Err(PlatformError::io("probe process group", error)),
        }
    }

    pub(crate) const fn attached_before_execution(&self) -> bool {
        self.attached_before_execution
    }

    pub(crate) const fn kill_on_close(&self) -> bool {
        self.kill_on_close
    }

    /// Force the exact verified process group when needed, reap the direct
    /// child once, and independently prove group absence within one deadline.
    pub(crate) fn cleanup_force_reap(
        &mut self,
        timeout: Duration,
    ) -> Result<ExitObservation, PlatformError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        if !self.declared_scope_empty()? {
            self.force_terminate()?;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let exit = self.wait_timeout(remaining)?.ok_or_else(|| {
            PlatformError::state("cleanup direct-child wait", "bounded wait timed out")
        })?;
        loop {
            if self.declared_scope_empty()? {
                return Ok(exit);
            }
            if Instant::now() >= deadline {
                return Err(PlatformError::state(
                    "cleanup process-group accounting",
                    "process group remained at deadline",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for PlatformChild {
    fn drop(&mut self) {
        let _bounded_cleanup = self.cleanup_force_reap(Duration::from_secs(2));
    }
}

fn decode_wait_status(status: i32, forced: bool) -> Result<ExitObservation, PlatformError> {
    if libc::WIFEXITED(status) {
        return Ok(ExitObservation {
            exit_code: Some(libc::WEXITSTATUS(status)),
            signal: None,
            windows_status_opaque: None,
            forced_by_supervisor: false,
        });
    }
    if libc::WIFSIGNALED(status) {
        let signal = libc::WTERMSIG(status);
        return Ok(ExitObservation {
            exit_code: None,
            signal: Some(signal),
            windows_status_opaque: None,
            forced_by_supervisor: forced && signal == libc::SIGKILL,
        });
    }
    Err(PlatformError::state(
        "decode direct-child wait status",
        format!("unexpected wait status {status:#x}"),
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::fs::symlink;

    fn environment() -> Vec<(String, String)> {
        vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())]
    }

    #[test]
    fn pinned_descriptor_spawn_preserves_governed_argument_zero() {
        let mut child = spawn(
            Path::new("/usr/bin/setsid"),
            &["--version".to_owned()],
            &environment(),
            Path::new("/"),
        )
        .expect("spawn pinned setsid");
        let (mut stdout, mut stderr) = child.take_pipes().expect("version pipes");
        let exit = child
            .wait_timeout(Duration::from_secs(5))
            .expect("wait for setsid")
            .expect("setsid exit");
        let mut version = String::new();
        stdout.read_to_string(&mut version).expect("stdout version");
        stderr.read_to_string(&mut version).expect("stderr version");
        assert!(exit.successful(), "version output={version:?}");
        assert!(
            version
                .lines()
                .any(|line| line.starts_with("setsid from util-linux ")),
            "pinned spawn changed argv[0]: {version:?}"
        );
        assert!(child.declared_scope_empty().expect("setsid group empty"));
    }

    #[test]
    fn failed_group_verification_reaps_exact_pid_and_retries_interrupted_wait() {
        let mut child = spawn(
            Path::new("/usr/bin/dash"),
            &["-c".to_owned(), "exec /bin/sleep 30".to_owned()],
            &environment(),
            Path::new("/"),
        )
        .expect("owned child fixture");
        let (_stdout, _stderr) = child.take_pipes().expect("fixture pipes");
        let direct_child = child.direct_child;
        let result = verify_process_group_observation(
            direct_child,
            Ok(direct_child.saturating_add(1)),
            PrehandoffWaitFault::InterruptedOnce,
        );
        let error = result.expect_err("wrong process group must fail closed");
        assert_eq!(error.operation, "verify dedicated process group");
        assert!(error.to_string().contains("expected pgid"));
        assert!(child.declared_scope_empty().expect("verified group empty"));
        child.exit = Some(ExitObservation {
            exit_code: None,
            signal: Some(libc::SIGKILL),
            windows_status_opaque: None,
            forced_by_supervisor: true,
        });
    }

    #[test]
    fn group_query_and_wait_failure_are_returned_as_correlated_cleanup_error() {
        let mut child = spawn(
            Path::new("/usr/bin/dash"),
            &["-c".to_owned(), "exec /bin/sleep 30".to_owned()],
            &environment(),
            Path::new("/"),
        )
        .expect("owned child fixture");
        let (_stdout, _stderr) = child.take_pipes().expect("fixture pipes");
        let direct_child = child.direct_child;
        let result = verify_process_group_observation(
            direct_child,
            Err(PlatformError::state(
                "verify dedicated process group",
                "injected getpgid failure",
            )),
            PrehandoffWaitFault::FailedOnce,
        );
        let error = result.expect_err("unproven reap must preserve both failures");
        assert_eq!(error.operation, "verify dedicated process group cleanup");
        let text = error.to_string();
        assert!(text.contains("injected getpgid failure"));
        assert!(text.contains("injected waitpid failure"));
        let mut status = 0;
        // SAFETY: this exact PID was started by this test and cleanup already
        // sent SIGKILL; this wait only reaps the deliberately injected failure.
        let reaped = unsafe { libc::waitpid(direct_child, &raw mut status, 0) };
        assert_eq!(reaped, direct_child);
        assert!(child.declared_scope_empty().expect("verified group empty"));
        child.exit = Some(decode_wait_status(status, true).expect("decode fallback reap"));
    }

    fn artifact_root(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("repository root")
            .join(format!(
                ".fforager-artifacts/test-runs/unix-fd-{label}-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).expect("create artifact root");
        root
    }

    fn spawn_current_test_helper(
        helper: &str,
        working_directory: &Path,
        bindings: &[InheritedFdBinding<'_>],
    ) -> PlatformChild {
        let executable = std::env::current_exe().expect("current test executable");
        let arguments = vec![
            "--exact".to_owned(),
            format!("platform::unix::tests::{helper}"),
            "--ignored".to_owned(),
            "--test-threads=1".to_owned(),
        ];
        spawn_internal(
            &executable,
            &arguments,
            &environment(),
            Some(working_directory),
            None,
            bindings,
            None,
        )
        .expect("spawn native inherited-fd helper")
    }

    fn assert_helper_success(child: &mut PlatformChild) {
        let (mut stdout, mut stderr) = child.take_pipes().expect("helper pipes");
        let exit = child
            .wait_timeout(Duration::from_secs(30))
            .expect("wait helper")
            .expect("helper exit");
        let mut output = String::new();
        let mut diagnostic = String::new();
        stdout.read_to_string(&mut output).expect("helper stdout");
        stderr
            .read_to_string(&mut diagnostic)
            .expect("helper stderr");
        assert!(
            exit.successful(),
            "native inherited-fd helper failed: {exit:?}; stdout={output}; stderr={diagnostic}"
        );
    }

    #[test]
    #[ignore = "spawned by exact-FD boundary regressions"]
    fn inherited_fd_input_child_helper() {
        let mut bytes = [0_u8; 64];
        // SAFETY: the parent regression binds its exact retained input to fd64
        // before this helper starts.
        let count = unsafe { libc::read(64, bytes.as_mut_ptr().cast(), bytes.len()) };
        assert!(count >= 0, "read inherited fd64");
        assert_eq!(
            &bytes[..usize::try_from(count).expect("read count")],
            b"pinned-object"
        );
    }

    #[test]
    #[ignore = "spawned by exact-FD boundary regressions"]
    fn inherited_fd_copy_child_helper() {
        let mut bytes = [0_u8; 4096];
        loop {
            // SAFETY: the parent regression binds exact retained input and
            // output files to fd64 and fd96 before this helper starts.
            let count = unsafe { libc::read(64, bytes.as_mut_ptr().cast(), bytes.len()) };
            assert!(count >= 0, "read inherited fd64");
            if count == 0 {
                break;
            }
            let count = usize::try_from(count).expect("read count");
            let mut written = 0;
            while written < count {
                // SAFETY: fd96 is the exact precreated output and the slice is
                // live for the bounded write.
                let result = unsafe {
                    libc::write(96, bytes[written..count].as_ptr().cast(), count - written)
                };
                assert!(result > 0, "write inherited fd96");
                written += usize::try_from(result).expect("write count");
            }
        }
    }

    #[test]
    fn spawn_binds_group_pipes_and_caches_nonzero_reap() {
        let arguments = vec!["-c".to_owned(), "printf proof; exit 7".to_owned()];
        let mut child = spawn(
            Path::new("/usr/bin/bash"),
            &arguments,
            &environment(),
            Path::new("/"),
        )
        .expect("spawn");
        assert!(child.attached_before_execution());
        assert!(!child.kill_on_close());
        let (mut stdout, _stderr) = child.take_pipes().expect("pipes");
        let mut output = String::new();
        stdout.read_to_string(&mut output).expect("read stdout");
        assert_eq!(output, "proof");
        let exit = child
            .wait_timeout(Duration::from_secs(2))
            .expect("wait")
            .expect("exit");
        assert_eq!(exit.exit_code, Some(7));
        assert_eq!(
            child.wait_timeout(Duration::ZERO).expect("cached"),
            Some(exit)
        );
        assert!(child.declared_scope_empty().expect("group absence"));
    }

    #[test]
    fn graceful_stop_sends_term_to_the_verified_group() {
        let arguments = vec![
            "-c".to_owned(),
            "trap 'exit 0' TERM; echo ready; while :; do sleep 1; done".to_owned(),
        ];
        let mut child = spawn(
            Path::new("/usr/bin/dash"),
            &arguments,
            &environment(),
            Path::new("/"),
        )
        .expect("spawn");
        let (stdout, _stderr) = child.take_pipes().expect("pipes");
        let mut reader = BufReader::new(stdout);
        let mut ready = String::new();
        reader.read_line(&mut ready).expect("ready marker");
        assert_eq!(ready, "ready\n");
        assert!(child.request_graceful_stop().expect("TERM group"));
        let exit = child
            .wait_timeout(Duration::from_secs(2))
            .expect("wait")
            .expect("exit");
        assert!(!exit.forced_by_supervisor);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child.declared_scope_empty().expect("group probe") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(child.declared_scope_empty().expect("active zero"));
    }

    #[test]
    fn force_terminate_kills_descendant_after_direct_child_was_reaped() {
        let arguments = vec!["-c".to_owned(), "sleep 30 & exit 0".to_owned()];
        let mut child = spawn(
            Path::new("/usr/bin/dash"),
            &arguments,
            &environment(),
            Path::new("/"),
        )
        .expect("spawn");
        let (_stdout, _stderr) = child.take_pipes().expect("pipes");
        let direct_exit = child
            .wait_timeout(Duration::from_secs(2))
            .expect("wait")
            .expect("direct exit");
        assert_eq!(direct_exit.exit_code, Some(0));
        assert!(!child.declared_scope_empty().expect("descendant active"));
        child.force_terminate().expect("kill remaining group");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child.declared_scope_empty().expect("group probe") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(child.declared_scope_empty().expect("active zero"));
        assert_eq!(
            child.wait_timeout(Duration::ZERO).expect("cached wait"),
            Some(direct_exit),
            "descendant termination must not rewrite or repeat the direct-child reap"
        );
    }

    #[test]
    fn forced_direct_wait_is_typed_as_sigkill() {
        let arguments = vec!["-c".to_owned(), "while :; do sleep 1; done".to_owned()];
        let mut child = spawn(
            Path::new("/usr/bin/dash"),
            &arguments,
            &environment(),
            Path::new("/"),
        )
        .expect("spawn");
        let (_stdout, _stderr) = child.take_pipes().expect("pipes");
        child.force_terminate().expect("kill group");
        let exit = child
            .wait_timeout(Duration::from_secs(2))
            .expect("wait")
            .expect("exit");
        assert_eq!(exit.exit_code, None);
        assert_eq!(exit.signal, Some(libc::SIGKILL));
        assert!(exit.forced_by_supervisor);
    }

    #[test]
    fn inherited_input_is_exact_pin_after_path_swap() {
        let root = artifact_root("input-swap");
        let input = root.join("input.aac");
        let retained = root.join("retained.aac");
        let outside = root.join("outside.aac");
        std::fs::write(&input, b"pinned-object").expect("input");
        std::fs::write(&outside, b"substituted-object").expect("outside");
        let pin = pin_governed_file(&input, 1024, Duration::from_secs(2)).expect("pin");
        std::fs::rename(&input, &retained).expect("rename pinned input");
        symlink(&outside, &input).expect("substitute input path");
        let bindings = [InheritedFdBinding::new(&pin, 64).expect("binding")];
        let mut child =
            spawn_current_test_helper("inherited_fd_input_child_helper", &root, &bindings);
        assert_helper_success(&mut child);
        let mut retained_bytes = [0_u8; 13];
        assert_eq!(
            pin.file
                .read_at(&mut retained_bytes, 0)
                .expect("read retained exact input"),
            retained_bytes.len()
        );
        assert_eq!(&retained_bytes, b"pinned-object");
        assert_eq!(
            std::fs::read(&input).expect("substituted bytes"),
            b"substituted-object"
        );
    }

    #[test]
    fn inherited_output_stays_on_exact_inode_after_path_substitution() {
        let root = artifact_root("output-parent-swap");
        let parent_path = root.join("output");
        let renamed_parent = root.join("output-retained");
        let outside = root.join("outside");
        std::fs::create_dir_all(&parent_path).expect("parent");
        std::fs::create_dir_all(&outside).expect("outside");
        let parent = pin_governed_directory(&parent_path).expect("pin parent");
        let output_path = parent_path.join("result.bin");
        let mut output =
            create_governed_output(&parent, &output_path, 1024, Duration::from_secs(2))
                .expect("exclusive output");
        let rename = std::fs::rename(&parent_path, &renamed_parent);
        assert!(
            rename.is_err(),
            "the retained directory pin must deny rename on the governed drvfs test root"
        );
        let retained_output = parent_path.join("retained.bin");
        let outside_target = outside.join("redirected.bin");
        std::fs::rename(&output_path, &retained_output)
            .expect("rename exact precreated output after openat");
        symlink(&outside_target, &output_path).expect("substitute output pathname");
        let source_path = root.join("exact-source.bin");
        std::fs::write(&source_path, b"exact-output").expect("source");
        let source =
            pin_governed_file(&source_path, 1024, Duration::from_secs(2)).expect("source pin");
        let bindings = [
            InheritedFdBinding::new(&source, 64).expect("source binding"),
            InheritedFdBinding::new(&output, 96).expect("output binding"),
        ];
        let mut child =
            spawn_current_test_helper("inherited_fd_copy_child_helper", &root, &bindings);
        assert_helper_success(&mut child);
        let mut exact_bytes = [0_u8; 12];
        assert_eq!(
            output
                .file
                .read_at(&mut exact_bytes, 0)
                .expect("read exact output fd"),
            exact_bytes.len()
        );
        assert_eq!(&exact_bytes, b"exact-output");
        let retained_metadata = std::fs::metadata(&retained_output).expect("retained metadata");
        assert_eq!(
            output.file_identity(),
            file_identity(&retained_output, &retained_metadata).expect("retained identity")
        );
        assert!(output_path.is_symlink());
        assert!(!outside_target.exists());
        assert!(
            output.finalize_written_content().is_err(),
            "substituted output path must remain fail closed on drvfs"
        );
        drop(output);
        assert_eq!(
            std::fs::read(&retained_output).expect("retained exact output"),
            b"exact-output"
        );
    }

    #[test]
    fn prepared_bindings_are_collision_free_and_rewind_for_repeated_spawns() {
        let root = artifact_root("binding-collision");
        let input = root.join("input.bin");
        std::fs::write(&input, b"repeatable").expect("input");
        let pin = pin_governed_file(&input, 1024, Duration::from_secs(2)).expect("pin");
        for _ in 0..2 {
            let bindings = [InheritedFdBinding::new(&pin, 64).expect("binding")];
            let prepared = prepare_inherited_bindings(&bindings).expect("prepare");
            assert_eq!(prepared.len(), 1);
            assert!(prepared[0].source.as_raw_fd() >= 256);
            assert_ne!(prepared[0].source.as_raw_fd(), prepared[0].target);
            let mut bytes = [0_u8; 10];
            let count = pin.file.read_at(&mut bytes, 0).expect("read pin");
            assert_eq!(&bytes[..count], b"repeatable");
        }
    }

    #[test]
    fn occupied_preferred_targets_do_not_clobber_bound_media_or_executable() {
        let root = artifact_root("occupied-binding-parent");
        let mut child =
            spawn_current_test_helper("occupied_preferred_targets_child_helper", &root, &[]);
        assert_helper_success(&mut child);
    }

    #[test]
    #[ignore = "spawned in a dedicated process by the target-collision regression"]
    fn occupied_preferred_targets_child_helper() {
        let root = artifact_root("occupied-binding-targets");
        let input_path = root.join("input.bin");
        let output_path = root.join("output.bin");
        std::fs::write(&input_path, b"occupied-target-proof").expect("input");
        let input =
            pin_governed_file(&input_path, 1024, Duration::from_secs(2)).expect("input pin");
        let parent = pin_governed_directory(&root).expect("parent pin");
        let mut output =
            create_governed_output(&parent, &output_path, 1024, Duration::from_secs(2))
                .expect("output pin");

        // Own every descriptor opened here with File so cleanup cannot close
        // any unrelated host descriptor. Opening until the highest audited
        // target is live fills all otherwise-free slots below it.
        let null = OpenOptions::new()
            .read(true)
            .open("/dev/null")
            .expect("open occupancy source");
        let source = duplicate_fd_at_least(null.as_raw_fd(), 256, "relocate occupancy source")
            .expect("relocate occupancy source");
        // SAFETY: the duplicate is uniquely owned in this dedicated child.
        let source = unsafe { OwnedFd::from_raw_fd(source) };
        let mut occupied = Vec::new();
        for target in 64..=96 {
            // SAFETY: this isolated helper intentionally owns the audited
            // target range; dup2 returns a distinct descriptor on success.
            assert_eq!(unsafe { libc::dup2(source.as_raw_fd(), target) }, target);
            // SAFETY: each successful dup2 target is now uniquely owned here.
            occupied.push(unsafe { OwnedFd::from_raw_fd(target) });
        }

        let bindings = [
            InheritedFdBinding::new(&input, 64).expect("input binding"),
            InheritedFdBinding::new(&output, 96).expect("output binding"),
        ];
        let mut child =
            spawn_current_test_helper("inherited_fd_copy_child_helper", &root, &bindings);
        assert_helper_success(&mut child);
        output.finalize_written_content().expect("finalize output");
        assert_eq!(
            std::fs::read(&output_path).expect("exact output"),
            b"occupied-target-proof"
        );
    }

    #[test]
    fn natural_exit_racing_force_is_never_labelled_supervisor_forced() {
        let arguments = vec!["-c".to_owned(), "exit 0".to_owned()];
        let mut child = spawn(
            Path::new("/usr/bin/dash"),
            &arguments,
            &environment(),
            Path::new("/"),
        )
        .expect("spawn");
        let (_stdout, _stderr) = child.take_pipes().expect("pipes");
        let exit = child
            .wait_timeout(Duration::from_secs(2))
            .expect("wait")
            .expect("exit");
        assert_eq!(
            child
                .force_terminate()
                .expect("force after exact cached reap"),
            ForceTerminationOutcome::AlreadyEmpty
        );
        assert_eq!(exit.exit_code, Some(0));
        assert!(!exit.forced_by_supervisor);
        assert_eq!(
            child.wait_timeout(Duration::ZERO).expect("cached reap"),
            Some(exit)
        );
    }

    #[test]
    fn exited_wait_status_never_inherits_force_request_provenance() {
        let exit = decode_wait_status(0, true).expect("normal wait status");
        assert_eq!(exit.exit_code, Some(0));
        assert_eq!(exit.signal, None);
        assert!(!exit.forced_by_supervisor);
    }

    #[test]
    fn retained_root_fchdir_and_temp_fd_fail_closed_on_post_preflight_path_substitution() {
        let root = artifact_root("trusted-directory-swap");
        let working_path = root.join("working");
        let temporary_path = working_path.join("tmp");
        let outside = root.join("outside");
        std::fs::create_dir_all(&temporary_path).expect("temporary");
        std::fs::create_dir_all(&outside).expect("outside");
        let working = pin_governed_directory(&working_path).expect("working pin");
        let temporary = pin_governed_directory(&temporary_path).expect("temporary pin");
        let retained = root.join("retained-working");
        let substituted = match std::fs::rename(&working_path, &retained) {
            Ok(()) => {
                symlink(&outside, &working_path).expect("substitute working path");
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                working
                    .verify_path(&working_path)
                    .expect("drvfs retained working identity");
                temporary
                    .verify_path(&temporary_path)
                    .expect("drvfs retained temporary identity");
                false
            }
            Err(error) => panic!("unexpected working-path substitution failure: {error}"),
        };

        let executable_path = Path::new("/usr/bin/dash");
        let executable =
            crate::identity::observe_executable(executable_path).expect("dash identity");
        let arguments = vec![
            "-c".to_owned(),
            "pwd; printf pinned > \"$TMPDIR/sentinel\"".to_owned(),
        ];
        let environment = vec![
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("TMPDIR".to_owned(), "/proc/self/fd/98".to_owned()),
        ];
        let mut child = spawn_verified_with_bindings(
            executable_path,
            &arguments,
            &environment,
            &working,
            &temporary,
            ExecutablePinExpectation {
                file_identity: executable.file_identity(),
                content_sha256: executable.content_sha256(),
                maximum_bytes: crate::identity::MAXIMUM_EXECUTABLE_BYTES,
                hash_timeout: Duration::from_secs(5),
            },
            &[],
        )
        .expect("spawn through retained directories");
        let (mut stdout, mut stderr) = child.take_pipes().expect("pipes");
        let exit = child
            .wait_timeout(Duration::from_secs(30))
            .expect("wait")
            .expect("exit");
        let mut output = String::new();
        let mut diagnostic = String::new();
        stdout.read_to_string(&mut output).expect("stdout");
        stderr.read_to_string(&mut diagnostic).expect("stderr");
        assert!(exit.successful(), "stderr={diagnostic}");
        let expected_suffix = if substituted {
            "retained-working"
        } else {
            "working"
        };
        assert!(output.trim().ends_with(expected_suffix), "pwd={output}");
        let effective_working = if substituted {
            &retained
        } else {
            &working_path
        };
        assert_eq!(
            std::fs::read(effective_working.join("tmp/sentinel")).expect("pinned temp write"),
            b"pinned"
        );
        assert!(!outside.join("sentinel").exists());
    }
}
