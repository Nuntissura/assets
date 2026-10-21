//! Windows Job/process boundary authorized by `FF-DEC-003`.

#![allow(
    unsafe_code,
    reason = "FF-DEC-003: all Windows FFI is confined to this reviewed platform boundary"
)]

use super::{ExecutablePinExpectation, ExitObservation, ForceTerminationOutcome, PlatformError};
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    fs::Metadata,
    io::Read,
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, FALSE, GENERIC_READ, GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
        INVALID_HANDLE_VALUE, SetHandleInformation, TRUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo,
        GetFileInformationByHandle, GetFileInformationByHandleEx, OPEN_EXISTING,
    },
    System::{
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Memory::{GetProcessHeap, HEAP_ZERO_MEMORY, HeapAlloc, HeapFree},
        Pipes::CreatePipe,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
            WaitForSingleObject,
        },
    },
};

const HEX: &[u8; 16] = b"0123456789abcdef";
const SUPERVISOR_TERMINATION_CODE: u32 = 0xffff_fffc;
const SYNCHRONIZE_PROCESS: u32 = 0x0010_0000;
pub(crate) const PARENT_DEATH_RECEIPT_ENV: &str = "FFORAGER_PARENT_DEATH_RECEIPT";
pub(crate) const PARENT_DEATH_HELPER_TEST: &str =
    "platform::windows::tests::kill_on_job_close_parent_death_child_helper";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowsParentDeathObservation {
    pub direct_child_pid: u32,
    pub descendant_pid: u32,
    pub elapsed_millis: u64,
    pub kill_on_job_close_parent_death_observed: bool,
}

#[derive(Debug)]
struct OwnedWinHandle(HANDLE);

impl OwnedWinHandle {
    fn new(handle: HANDLE, operation: &'static str) -> Result<Self, PlatformError> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(PlatformError::last(operation))
        } else {
            Ok(Self(handle))
        }
    }

    const fn get(&self) -> HANDLE {
        self.0
    }

    fn into_file(mut self) -> std::fs::File {
        let handle = self.0;
        self.0 = null_mut();
        // SAFETY: ownership of one valid, non-aliased pipe handle transfers
        // from this wrapper to File exactly once.
        unsafe { std::fs::File::from_raw_handle(handle) }
    }
}

impl Drop for OwnedWinHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: this wrapper uniquely owns a valid HANDLE and invalidates
            // it before any transfer.
            unsafe {
                let _closed = CloseHandle(self.0);
            }
            self.0 = null_mut();
        }
    }
}

#[derive(Debug)]
struct AttributeList {
    heap: HANDLE,
    pointer: LPPROC_THREAD_ATTRIBUTE_LIST,
    initialized: bool,
}

impl AttributeList {
    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: documented two-call aligned process-heap attribute-list allocation"
    )]
    fn new(attribute_count: u32) -> Result<Self, PlatformError> {
        let heap = unsafe { GetProcessHeap() };
        if heap.is_null() {
            return Err(PlatformError::last("GetProcessHeap"));
        }
        let mut bytes = 0_usize;
        // SAFETY: the documented sizing call accepts a null list and writes
        // only the initialized usize pointer.
        unsafe {
            let _sizing_result =
                InitializeProcThreadAttributeList(null_mut(), attribute_count, 0, &raw mut bytes);
        }
        if bytes == 0 {
            return Err(PlatformError::last("size process attribute list"));
        }
        // SAFETY: GetProcessHeap returned a valid heap; the returned allocation
        // is aligned by the process heap and retained until Drop.
        let pointer = unsafe { HeapAlloc(heap, HEAP_ZERO_MEMORY, bytes) };
        if pointer.is_null() {
            return Err(PlatformError::last("HeapAlloc process attribute list"));
        }
        let mut result = Self {
            heap,
            pointer,
            initialized: false,
        };
        // SAFETY: pointer references the aligned `bytes` allocation required by
        // the successful sizing call and remains live in result.
        if unsafe { InitializeProcThreadAttributeList(pointer, attribute_count, 0, &raw mut bytes) }
            == FALSE
        {
            return Err(PlatformError::last("initialize process attribute list"));
        }
        result.initialized = true;
        Ok(result)
    }

    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: attribute values remain live through CreateProcessW"
    )]
    fn update(
        &mut self,
        attribute: usize,
        value: *const c_void,
        bytes: usize,
    ) -> Result<(), PlatformError> {
        // SAFETY: the list is initialized and the caller retains the exact
        // typed value for at least as long as CreateProcessW uses this list.
        if unsafe {
            UpdateProcThreadAttribute(self.pointer, 0, attribute, value, bytes, null_mut(), null())
        } == FALSE
        {
            Err(PlatformError::last("UpdateProcThreadAttribute"))
        } else {
            Ok(())
        }
    }
}

impl Drop for AttributeList {
    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: paired attribute-list delete and process-heap free"
    )]
    fn drop(&mut self) {
        // SAFETY: initialized lists are deleted once before their allocation is
        // freed from the same process heap.
        unsafe {
            if self.initialized {
                DeleteProcThreadAttributeList(self.pointer);
            }
            if !self.pointer.is_null() {
                let _freed = HeapFree(self.heap, 0, self.pointer.cast());
            }
        }
        self.pointer = null_mut();
    }
}

pub(crate) fn file_identity(path: &Path, _metadata: &Metadata) -> Result<String, std::io::Error> {
    let wide = wide_nul(path.as_os_str());
    // SAFETY: the path is NUL-terminated and all output structures are valid
    // for the duration of their calls. The handle is closed by OwnedWinHandle.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    let handle = OwnedWinHandle::new(handle, "open file identity").map_err(|error| error.source)?;
    // SAFETY: zeroed Windows POD structures are valid output buffers.
    let mut basic = unsafe { zeroed() };
    // SAFETY: handle and output pointer are valid for the exact structure.
    if unsafe { GetFileInformationByHandle(handle.get(), &raw mut basic) } == FALSE {
        return Err(std::io::Error::last_os_error());
    }
    if basic.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "reparse-point executable rejected",
        ));
    }
    // SAFETY: zeroed FILE_ID_INFO is a valid output buffer.
    let mut identity: FILE_ID_INFO = unsafe { zeroed() };
    let size = u32::try_from(size_of::<FILE_ID_INFO>())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "FILE_ID_INFO size"))?;
    // SAFETY: exact FileIdInfo class, buffer, and length are supplied.
    if unsafe {
        GetFileInformationByHandleEx(handle.get(), FileIdInfo, (&raw mut identity).cast(), size)
    } == FALSE
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut file_id = String::with_capacity(32);
    for byte in identity.FileId.Identifier {
        file_id.push(char::from(HEX[usize::from(byte >> 4)]));
        file_id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(format!("windows:{}:{file_id}", identity.VolumeSerialNumber))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GovernedPathKind {
    File,
    Directory,
}

/// Retained non-reparse handle binding a governed media file or output directory.
#[derive(Debug)]
pub(crate) struct GovernedPathPin {
    handle: std::fs::File,
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
        self.handle
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| PlatformError::io("stat governed handle", error))
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

#[allow(
    clippy::unnecessary_wraps,
    reason = "platform-neutral pipe configuration is fallible on Unix"
)]
pub(crate) fn configure_cancellable_pipe(_file: &std::fs::File) -> Result<(), PlatformError> {
    // Windows Job containment closes all governed writer handles before join;
    // there is no hostile setsid-style scope escape in the declared profile.
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "FF-DEC-003 keeps the complete bounded handle, reparse, identity, size, deadline, and digest proof in one reviewed Windows boundary"
)]
fn pin_governed_path(
    path: &Path,
    kind: GovernedPathKind,
    maximum_bytes: Option<u64>,
    hash_timeout: Duration,
) -> Result<GovernedPathPin, PlatformError> {
    let wide = wide_nul(path.as_os_str());
    let (access, share, flags) = match kind {
        GovernedPathKind::File => (
            GENERIC_READ,
            FILE_SHARE_READ,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
        ),
        GovernedPathKind::Directory => (
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        ),
    };
    // SAFETY: path is NUL-terminated. File pins omit share-write/delete;
    // directory pins omit share-delete while permitting child file creation.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            share,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    };
    let mut file = OwnedWinHandle::new(handle, "pin governed path")?.into_file();
    let raw = file.as_raw_handle();
    // SAFETY: live handle and initialized output structure.
    let mut basic = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(raw, &raw mut basic) } == FALSE {
        return Err(PlatformError::last("query governed path attributes"));
    }
    if basic.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PlatformError::state(
            "pin governed path",
            "reparse point rejected",
        ));
    }
    let is_directory = basic.dwFileAttributes
        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY
        != 0;
    if is_directory != (kind == GovernedPathKind::Directory) {
        return Err(PlatformError::state(
            "pin governed path",
            "governed path kind changed",
        ));
    }
    // SAFETY: documented output structure and exact size.
    let mut identity: FILE_ID_INFO = unsafe { zeroed() };
    let identity_size = u32::try_from(size_of::<FILE_ID_INFO>())
        .map_err(|_| PlatformError::state("pin governed path", "identity size overflow"))?;
    if unsafe {
        GetFileInformationByHandleEx(raw, FileIdInfo, (&raw mut identity).cast(), identity_size)
    } == FALSE
    {
        return Err(PlatformError::last("query governed path identity"));
    }
    let mut file_id = String::with_capacity(32);
    for byte in identity.FileId.Identifier {
        file_id.push(char::from(HEX[usize::from(byte >> 4)]));
        file_id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    let identity = format!("windows:{}:{file_id}", identity.VolumeSerialNumber);
    let content_sha256 = if kind == GovernedPathKind::File {
        let maximum = maximum_bytes.ok_or_else(|| {
            PlatformError::state("pin governed file", "missing file byte ceiling")
        })?;
        let file_size = (u64::from(basic.nFileSizeHigh) << 32) | u64::from(basic.nFileSizeLow);
        if maximum == 0 || hash_timeout.is_zero() || file_size > maximum {
            return Err(PlatformError::state(
                "pin governed file",
                "file exceeds byte ceiling or hash deadline is zero",
            ));
        }
        let started = Instant::now();
        let mut observed = 0_u64;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            if started.elapsed() >= hash_timeout {
                return Err(PlatformError::state(
                    "hash governed file",
                    "hash deadline exceeded",
                ));
            }
            let count = file
                .read(&mut buffer)
                .map_err(|error| PlatformError::io("hash governed file", error))?;
            if count == 0 {
                break;
            }
            observed = observed
                .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
                .ok_or_else(|| PlatformError::state("hash governed file", "size overflow"))?;
            if observed > maximum {
                return Err(PlatformError::state(
                    "hash governed file",
                    "file exceeded byte ceiling while hashing",
                ));
            }
            digest.update(&buffer[..count]);
        }
        Some(hex_digest(digest.finalize()))
    } else {
        None
    };
    Ok(GovernedPathPin {
        handle: file,
        identity,
        content_sha256,
        kind,
        maximum_bytes,
        hash_timeout,
    })
}

#[derive(Debug)]
pub(crate) struct PlatformChild {
    process: OwnedWinHandle,
    job: OwnedWinHandle,
    stdout: Option<std::fs::File>,
    stderr: Option<std::fs::File>,
    exit: Option<ExitObservation>,
    forced: bool,
    attached_before_execution: bool,
    kill_on_close: bool,
    preexecution_active_processes: u32,
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: complete pre-execution Job-list and exact std-handle boundary"
)]
pub(crate) fn spawn(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<PlatformChild, PlatformError> {
    spawn_internal(executable, arguments, environment, working_directory, None)
}

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
        working_directory,
        Some(expectation),
    )
}

fn spawn_internal(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    expectation: Option<ExecutablePinExpectation<'_>>,
) -> Result<PlatformChild, PlatformError> {
    // This non-share-write/delete handle pins the exact executable bytes and
    // identity across path-based CreateProcessW and the suspended resume gate.
    let _executable_pin = pin_executable(executable, expectation)?;
    let boundary = PreparedBoundary::new()?;
    let (process, thread) = create_suspended_process(
        executable,
        arguments,
        environment,
        working_directory,
        &boundary,
        None,
    )?;
    let PreparedBoundary {
        stdout_read,
        stdout_write,
        stderr_read,
        stderr_write,
        stdin,
        job,
    } = boundary;
    drop(stdout_write);
    drop(stderr_write);
    drop(stdin);
    let preexecution_active_processes =
        verify_preexecution_assignment_and_resume(&job, &process, &thread)?;
    drop(thread);
    Ok(PlatformChild {
        process,
        job,
        stdout: Some(stdout_read.into_file()),
        stderr: Some(stderr_read.into_file()),
        exit: None,
        forced: false,
        attached_before_execution: true,
        kill_on_close: true,
        preexecution_active_processes,
    })
}

pub(crate) fn observe_kill_on_job_close_parent_death(
    helper_executable: &Path,
    helper_arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    receipt_path: &Path,
    timeout: Duration,
) -> Result<WindowsParentDeathObservation, PlatformError> {
    if timeout.is_zero() {
        return Err(PlatformError::state(
            "observe Job parent death",
            "observation timeout is zero",
        ));
    }
    let started = Instant::now();
    let mut helper = spawn_internal(
        helper_executable,
        helper_arguments,
        environment,
        working_directory,
        None,
    )?;
    let (mut stdout, mut stderr) = helper.take_pipes()?;
    let helper_exit = helper
        .wait_timeout(timeout)?
        .ok_or_else(|| PlatformError::state("wait parent-death helper", "deadline exceeded"))?;
    let mut helper_output = String::new();
    let mut helper_diagnostic = String::new();
    stdout
        .read_to_string(&mut helper_output)
        .map_err(|error| PlatformError::io("read parent-death helper stdout", error))?;
    stderr
        .read_to_string(&mut helper_diagnostic)
        .map_err(|error| PlatformError::io("read parent-death helper stderr", error))?;
    if !helper_exit.successful() {
        return Err(PlatformError::state(
            "run parent-death helper",
            format!("{helper_exit:?}; stdout={helper_output}; stderr={helper_diagnostic}"),
        ));
    }
    let receipt = std::fs::read_to_string(receipt_path)
        .map_err(|error| PlatformError::io("read parent-death receipt", error))?;
    let mut fields = receipt.trim().split(',');
    let direct_child_pid = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| PlatformError::state("parse parent-death receipt", "direct PID"))?;
    let descendant_pid = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| PlatformError::state("parse parent-death receipt", "descendant PID"))?;
    if fields.next().is_some() || direct_child_pid == 0 || descendant_pid == 0 {
        return Err(PlatformError::state(
            "parse parent-death receipt",
            "unexpected PID receipt shape",
        ));
    }
    let deadline = started.checked_add(timeout).unwrap_or(started);
    while Instant::now() < deadline {
        if process_is_terminated(direct_child_pid)? && process_is_terminated(descendant_pid)? {
            return Ok(WindowsParentDeathObservation {
                direct_child_pid,
                descendant_pid,
                elapsed_millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                kill_on_job_close_parent_death_observed: true,
            });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(PlatformError::state(
        "observe Job parent death",
        "direct child or descendant survived KILL_ON_JOB_CLOSE deadline",
    ))
}

fn process_is_terminated(process_id: u32) -> Result<bool, PlatformError> {
    // SAFETY: OpenProcess receives one audited access bit and a nonzero PID
    // emitted by the owned helper child.
    let handle = unsafe { OpenProcess(SYNCHRONIZE_PROCESS, FALSE, process_id) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(87) {
            Ok(true)
        } else {
            Err(PlatformError::io("open parent-death process", error))
        };
    }
    let handle = OwnedWinHandle::new(handle, "own parent-death process query")?;
    // SAFETY: the synchronized query handle is live for this zero-time wait.
    match unsafe { WaitForSingleObject(handle.get(), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(PlatformError::last("query parent-death process state")),
        status => Err(PlatformError::state(
            "query parent-death process state",
            format!("unexpected wait status {status:#x}"),
        )),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the reviewed Windows pin keeps handle identity, byte ceiling, hash deadline, and content verification in one boundary"
)]
fn pin_executable(
    path: &Path,
    expectation: Option<ExecutablePinExpectation<'_>>,
) -> Result<std::fs::File, PlatformError> {
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
    let wide = wide_nul(path.as_os_str());
    // SAFETY: NUL-terminated path and null optional pointers. Omitting share
    // write/delete blocks substitution while this handle remains live.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    let mut file = OwnedWinHandle::new(handle, "pin executable")?.into_file();
    let raw = file.as_raw_handle();
    // SAFETY: live pinned file handle and initialized output structures.
    let mut basic = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(raw, &raw mut basic) } == FALSE {
        return Err(PlatformError::last("query pinned executable attributes"));
    }
    if basic.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PlatformError::state(
            "pin executable",
            "reparse-point executable rejected",
        ));
    }
    // SAFETY: zeroed FILE_ID_INFO is the documented output buffer.
    let mut identity: FILE_ID_INFO = unsafe { zeroed() };
    let identity_size = u32::try_from(size_of::<FILE_ID_INFO>())
        .map_err(|_| PlatformError::state("pin executable", "identity size overflow"))?;
    if unsafe {
        GetFileInformationByHandleEx(raw, FileIdInfo, (&raw mut identity).cast(), identity_size)
    } == FALSE
    {
        return Err(PlatformError::last("query pinned executable identity"));
    }
    let mut file_id = String::with_capacity(32);
    for byte in identity.FileId.Identifier {
        file_id.push(char::from(HEX[usize::from(byte >> 4)]));
        file_id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    let observed_identity = format!("windows:{}:{file_id}", identity.VolumeSerialNumber);
    let expected_size = file
        .metadata()
        .map_err(|error| PlatformError::io("stat pinned executable", error))?
        .len();
    if expected_size > maximum_bytes {
        return Err(PlatformError::state(
            "hash pinned executable",
            "executable exceeds audited byte ceiling",
        ));
    }
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
    if started.elapsed() >= hash_timeout || total != expected_size {
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
            "identity or content changed at CreateProcess boundary",
        ));
    }
    Ok(file)
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[derive(Debug)]
struct PreparedBoundary {
    stdout_read: OwnedWinHandle,
    stdout_write: OwnedWinHandle,
    stderr_read: OwnedWinHandle,
    stderr_write: OwnedWinHandle,
    stdin: OwnedWinHandle,
    job: OwnedWinHandle,
}

impl PreparedBoundary {
    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: create exact pipes, NUL input, and Job limits"
    )]
    fn new() -> Result<Self, PlatformError> {
        let security = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(|_| {
                PlatformError::io(
                    "SECURITY_ATTRIBUTES size",
                    std::io::Error::other("size overflow"),
                )
            })?,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: TRUE,
        };
        let (stdout_read, stdout_write) = create_pipe(&raw const security)?;
        let (stderr_read, stderr_write) = create_pipe(&raw const security)?;
        let nul = wide_nul(std::ffi::OsStr::new("NUL"));
        // SAFETY: NUL path and SECURITY_ATTRIBUTES are live and valid.
        let stdin = unsafe {
            CreateFileW(
                nul.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ,
                &raw const security,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        let stdin = OwnedWinHandle::new(stdin, "open NUL stdin")?;
        verify_inheritance(stdin.get(), true, "verify child stdin inheritance")?;
        verify_inheritance(stdout_write.get(), true, "verify child stdout inheritance")?;
        verify_inheritance(stderr_write.get(), true, "verify child stderr inheritance")?;
        verify_inheritance(stdout_read.get(), false, "verify parent stdout inheritance")?;
        verify_inheritance(stderr_read.get(), false, "verify parent stderr inheritance")?;

        // SAFETY: null security/name produces a non-inheritable unnamed Job.
        let job = OwnedWinHandle::new(
            unsafe { CreateJobObjectW(null(), null()) },
            "CreateJobObjectW",
        )?;
        verify_inheritance(job.get(), false, "verify Job handle inheritance")?;
        // SAFETY: zeroed documented POD is initialized below.
        let mut job_limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        job_limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let job_limit_size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|_| {
                PlatformError::io("Job limit size", std::io::Error::other("size overflow"))
            })?;
        // SAFETY: exact Job information class and structure are supplied.
        if unsafe {
            SetInformationJobObject(
                job.get(),
                JobObjectExtendedLimitInformation,
                (&raw const job_limits).cast(),
                job_limit_size,
            )
        } == FALSE
        {
            return Err(PlatformError::last("SetInformationJobObject"));
        }
        verify_kill_on_close(&job)?;
        Ok(Self {
            stdout_read,
            stdout_write,
            stderr_read,
            stderr_write,
            stdin,
            job,
        })
    }
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: create suspended process with Job and exact handle attributes"
)]
fn create_suspended_process(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    boundary: &PreparedBoundary,
    extra_child_handle: Option<HANDLE>,
) -> Result<(OwnedWinHandle, OwnedWinHandle), PlatformError> {
    let mut child_handles = vec![
        boundary.stdin.get(),
        boundary.stdout_write.get(),
        boundary.stderr_write.get(),
    ];
    if let Some(handle) = extra_child_handle {
        child_handles.push(handle);
    }
    let job_handles = [boundary.job.get()];
    let mut attributes = AttributeList::new(2)?;
    attributes.update(
        attribute_id(PROC_THREAD_ATTRIBUTE_JOB_LIST)?,
        job_handles.as_ptr().cast(),
        size_of::<HANDLE>(),
    )?;
    attributes.update(
        attribute_id(PROC_THREAD_ATTRIBUTE_HANDLE_LIST)?,
        child_handles.as_ptr().cast(),
        size_of::<HANDLE>() * child_handles.len(),
    )?;

    let application = wide_nul(executable.as_os_str());
    let mut command_line = encode_command_line(executable, arguments);
    let current_directory = wide_nul(working_directory.as_os_str());
    let environment_block = encode_environment(environment)?;
    // SAFETY: zeroed POD is initialized with all required fields.
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(|_| {
        PlatformError::io(
            "STARTUPINFOEXW size",
            std::io::Error::other("size overflow"),
        )
    })?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = boundary.stdin.get();
    startup.StartupInfo.hStdOutput = boundary.stdout_write.get();
    startup.StartupInfo.hStdError = boundary.stderr_write.get();
    startup.lpAttributeList = attributes.pointer;
    // SAFETY: zeroed PROCESS_INFORMATION is the documented output shape.
    let mut process_information: PROCESS_INFORMATION = unsafe { zeroed() };
    let flags = EXTENDED_STARTUPINFO_PRESENT
        | CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | CREATE_NO_WINDOW;
    // SAFETY: all pointers reference live, correctly terminated buffers and
    // exact initialized structures through this call.
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            TRUE,
            flags,
            environment_block.as_ptr().cast(),
            current_directory.as_ptr(),
            &raw const startup.StartupInfo,
            &raw mut process_information,
        )
    } == FALSE
    {
        return Err(PlatformError::last("CreateProcessW"));
    }
    let process = OwnedWinHandle::new(
        process_information.hProcess,
        "CreateProcessW process handle",
    )?;
    let thread = OwnedWinHandle::new(process_information.hThread, "CreateProcessW thread handle")?;
    verify_inheritance(process.get(), false, "verify process handle inheritance")?;
    verify_inheritance(thread.get(), false, "verify thread handle inheritance")?;
    Ok((process, thread))
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: pre-resume Job accounting and exact resume"
)]
fn verify_preexecution_assignment_and_resume(
    job: &OwnedWinHandle,
    process: &OwnedWinHandle,
    thread: &OwnedWinHandle,
) -> Result<u32, PlatformError> {
    verify_preexecution_assignment_and_resume_with_fault(
        job,
        process,
        thread,
        PreexecutionFault::None,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreexecutionFault {
    None,
    Query,
    ActiveCount,
    ResumeCount,
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: pre-resume Job accounting and exact resume"
)]
fn verify_preexecution_assignment_and_resume_with_fault(
    job: &OwnedWinHandle,
    process: &OwnedWinHandle,
    thread: &OwnedWinHandle,
    fault: PreexecutionFault,
) -> Result<u32, PlatformError> {
    if fault == PreexecutionFault::Query {
        let primary = PlatformError::state(
            "query pre-execution Job assignment",
            "injected Job query failure",
        );
        return Err(cleanup_failed_preexecution(job, process, primary));
    }
    let mut active_processes = match query_job_active_processes(job) {
        Ok(value) => value,
        Err(error) => return Err(cleanup_failed_preexecution(job, process, error)),
    };
    if fault == PreexecutionFault::ActiveCount {
        active_processes = 0;
    }
    if active_processes != 1 {
        let primary = PlatformError::state(
            "verify pre-execution Job assignment",
            format!("expected one active process before resume, observed {active_processes}"),
        );
        return Err(cleanup_failed_preexecution(job, process, primary));
    }
    // SAFETY: exact returned primary-thread handle is resumed once.
    let previous_suspend_count = if fault == PreexecutionFault::ResumeCount {
        2
    } else {
        unsafe { ResumeThread(thread.get()) }
    };
    if previous_suspend_count != 1 {
        let primary = PlatformError::state(
            "ResumeThread exact suspend count",
            format!("expected previous suspend count 1, observed {previous_suspend_count}"),
        );
        return Err(cleanup_failed_preexecution(job, process, primary));
    }
    Ok(active_processes)
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: bounded pre-handoff Job termination, direct wait, and active-zero query"
)]
fn cleanup_failed_preexecution(
    job: &OwnedWinHandle,
    process: &OwnedWinHandle,
    primary: PlatformError,
) -> PlatformError {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .unwrap_or_else(Instant::now);
    let mut cleanup_failures = Vec::new();
    if unsafe { TerminateJobObject(job.get(), SUPERVISOR_TERMINATION_CODE) } == FALSE {
        cleanup_failures
            .push(PlatformError::last("terminate failed pre-execution Job").to_string());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let millis = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX - 1);
    let wait = unsafe { WaitForSingleObject(process.get(), millis) };
    if wait != WAIT_OBJECT_0 {
        cleanup_failures.push(if wait == WAIT_FAILED {
            PlatformError::last("wait failed pre-execution direct child").to_string()
        } else {
            format!("wait failed pre-execution direct child returned {wait}")
        });
    }
    loop {
        match query_job_active_processes(job) {
            Ok(0) => break,
            Ok(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(active) => {
                cleanup_failures.push(format!(
                    "failed pre-execution Job retained {active} active processes at deadline"
                ));
                break;
            }
            Err(error) => {
                cleanup_failures.push(error.to_string());
                break;
            }
        }
    }
    if cleanup_failures.is_empty() {
        primary
    } else {
        PlatformError::state(
            "pre-execution verification cleanup",
            format!(
                "primary: {primary}; cleanup: {}",
                cleanup_failures.join("; ")
            ),
        )
    }
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: independent exact Job accounting query"
)]
fn query_job_active_processes(job: &OwnedWinHandle) -> Result<u32, PlatformError> {
    let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
    let size = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
        .map_err(|_| PlatformError::state("Job accounting size", "size overflow"))?;
    if unsafe {
        QueryInformationJobObject(
            job.get(),
            JobObjectBasicAccountingInformation,
            (&raw mut accounting).cast(),
            size,
            null_mut(),
        )
    } == FALSE
    {
        Err(PlatformError::last("QueryInformationJobObject"))
    } else {
        Ok(accounting.ActiveProcesses)
    }
}

fn attribute_id(value: u32) -> Result<usize, PlatformError> {
    usize::try_from(value)
        .map_err(|_| PlatformError::state("convert process attribute id", "value overflow"))
}

#[allow(unsafe_code, reason = "FF-DEC-003: independent Job limit query")]
fn verify_kill_on_close(job: &OwnedWinHandle) -> Result<(), PlatformError> {
    // SAFETY: zeroed documented POD is a valid output buffer.
    let mut observed: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| PlatformError::state("Job limit size", "size overflow"))?;
    // SAFETY: exact Job information class and structure are supplied.
    if unsafe {
        QueryInformationJobObject(
            job.get(),
            JobObjectExtendedLimitInformation,
            (&raw mut observed).cast(),
            size,
            null_mut(),
        )
    } == FALSE
    {
        return Err(PlatformError::last("query Job kill-on-close limit"));
    }
    if observed.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE == 0 {
        return Err(PlatformError::state(
            "verify Job kill-on-close limit",
            "JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE was not retained",
        ));
    }
    Ok(())
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: exact owner/child handle-inheritance assertion"
)]
fn verify_inheritance(
    handle: HANDLE,
    expected: bool,
    operation: &'static str,
) -> Result<(), PlatformError> {
    let mut flags = 0_u32;
    // SAFETY: `handle` is owned and valid, and `flags` is an initialized output.
    if unsafe { GetHandleInformation(handle, &raw mut flags) } == FALSE {
        return Err(PlatformError::last(operation));
    }
    let observed = flags & HANDLE_FLAG_INHERIT != 0;
    if observed == expected {
        Ok(())
    } else {
        Err(PlatformError::state(
            operation,
            format!("expected inheritable={expected}, observed {observed}"),
        ))
    }
}

#[allow(
    unsafe_code,
    reason = "FF-DEC-003: CreatePipe with exact inheritance then cleared parent end"
)]
fn create_pipe(
    security: *const SECURITY_ATTRIBUTES,
) -> Result<(OwnedWinHandle, OwnedWinHandle), PlatformError> {
    let mut read = null_mut();
    let mut write = null_mut();
    // SAFETY: both output pointers and SECURITY_ATTRIBUTES remain live.
    if unsafe { CreatePipe(&raw mut read, &raw mut write, security, 0) } == FALSE {
        return Err(PlatformError::last("CreatePipe"));
    }
    let read = OwnedWinHandle::new(read, "CreatePipe read")?;
    let write = OwnedWinHandle::new(write, "CreatePipe write")?;
    // SAFETY: exact owned read handle is made non-inheritable before spawn.
    if unsafe { SetHandleInformation(read.get(), HANDLE_FLAG_INHERIT, 0) } == FALSE {
        return Err(PlatformError::last("clear parent pipe inheritance"));
    }
    Ok((read, write))
}

/// Raw observation from the protected handle allow-list and one deliberately
/// weakened fault mutation. This prerequisite-only oracle must never be used
/// to create a production child; it exists so report evidence is not inferred
/// from parent-side flags or an allowed stdout descendant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowsForbiddenHandleObservation {
    pub protected_exit: ExitObservation,
    pub protected_bytes: Vec<u8>,
    pub mutated_exit: ExitObservation,
    pub mutated_bytes: Vec<u8>,
}

pub(crate) fn run_handle_list_fault_probe(
    executable: &Path,
    environment: &[(String, String)],
    working_directory: &Path,
    timeout: Duration,
) -> Result<WindowsForbiddenHandleObservation, PlatformError> {
    let (protected_read, protected_write) = create_sentinel_pipe()?;
    let protected_arguments = sentinel_command(protected_write.get());
    let mut protected = spawn(
        executable,
        &protected_arguments,
        environment,
        working_directory,
    )?;
    let (_stdout, _stderr) = protected.take_pipes()?;
    let protected_exit = protected
        .wait_timeout(timeout)?
        .ok_or_else(|| PlatformError::state("protected sentinel wait", "timed out"))?;
    if !protected.declared_scope_empty()? {
        protected.cleanup_force_reap(timeout)?;
    }
    drop(protected_write);
    let mut protected_bytes = Vec::new();
    protected_read
        .into_file()
        .read_to_end(&mut protected_bytes)
        .map_err(|error| PlatformError::io("read protected sentinel", error))?;

    let (mutated_read, mutated_write) = create_sentinel_pipe()?;
    let mutated_arguments = sentinel_command(mutated_write.get());
    let _pin = pin_executable(executable, None)?;
    let boundary = PreparedBoundary::new()?;
    let (process, thread) = create_suspended_process(
        executable,
        &mutated_arguments,
        environment,
        working_directory,
        &boundary,
        Some(mutated_write.get()),
    )?;
    let PreparedBoundary {
        stdout_read,
        stdout_write,
        stderr_read,
        stderr_write,
        stdin,
        job,
    } = boundary;
    drop(stdout_write);
    drop(stderr_write);
    drop(stdin);
    let active = verify_preexecution_assignment_and_resume(&job, &process, &thread)?;
    drop(thread);
    let mut mutated = PlatformChild {
        process,
        job,
        stdout: Some(stdout_read.into_file()),
        stderr: Some(stderr_read.into_file()),
        exit: None,
        forced: false,
        attached_before_execution: true,
        kill_on_close: true,
        preexecution_active_processes: active,
    };
    let (_stdout, _stderr) = mutated.take_pipes()?;
    let mutated_exit = mutated
        .wait_timeout(timeout)?
        .ok_or_else(|| PlatformError::state("mutated sentinel wait", "timed out"))?;
    if !mutated.declared_scope_empty()? {
        mutated.cleanup_force_reap(timeout)?;
    }
    drop(mutated_write);
    let mut mutated_bytes = Vec::new();
    mutated_read
        .into_file()
        .read_to_end(&mut mutated_bytes)
        .map_err(|error| PlatformError::io("read mutated sentinel", error))?;
    Ok(WindowsForbiddenHandleObservation {
        protected_exit,
        protected_bytes,
        mutated_exit,
        mutated_bytes,
    })
}

fn create_sentinel_pipe() -> Result<(OwnedWinHandle, OwnedWinHandle), PlatformError> {
    let security = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| PlatformError::state("sentinel security", "size overflow"))?,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: TRUE,
    };
    create_pipe(&raw const security)
}

fn sentinel_command(handle: HANDLE) -> Vec<String> {
    let numeric = handle as usize;
    vec![
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-Command".to_owned(),
        format!(
            "$h=[IntPtr]::new({numeric}); $s=[Microsoft.Win32.SafeHandles.SafeFileHandle]::new($h,$false); try {{ $f=[System.IO.FileStream]::new($s,[System.IO.FileAccess]::Write); $b=[Text.Encoding]::ASCII.GetBytes('LEAK'); $f.Write($b,0,$b.Length); $f.Flush(); exit 0 }} catch {{ exit 23 }}"
        ),
    ]
}

impl PlatformChild {
    pub(crate) fn take_pipes(&mut self) -> Result<(std::fs::File, std::fs::File), PlatformError> {
        let stdout = self.stdout.take().ok_or_else(|| {
            PlatformError::io(
                "take stdout",
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdout already taken"),
            )
        })?;
        let stderr = self.stderr.take().ok_or_else(|| {
            PlatformError::io(
                "take stderr",
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stderr already taken"),
            )
        })?;
        Ok((stdout, stderr))
    }

    #[allow(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "cross-platform interface; hidden/no-console Windows has no supported graceful control"
    )]
    pub(crate) const fn request_graceful_stop(&mut self) -> Result<bool, PlatformError> {
        Ok(false)
    }

    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: terminate the exact owned Job as one bounded scope"
    )]
    pub(crate) fn force_terminate(&mut self) -> Result<ForceTerminationOutcome, PlatformError> {
        if self.declared_scope_empty()? {
            return Ok(ForceTerminationOutcome::AlreadyEmpty);
        }
        // SAFETY: job is a uniquely owned valid Job handle.
        if unsafe { TerminateJobObject(self.job.get(), SUPERVISOR_TERMINATION_CODE) } == FALSE {
            let error = PlatformError::last("TerminateJobObject");
            // Process-tree exit may race termination. Only an independent
            // active-zero observation converts that race into success.
            if self.declared_scope_empty()? {
                return Ok(ForceTerminationOutcome::AlreadyEmpty);
            }
            return Err(error);
        }
        self.forced = true;
        Ok(ForceTerminationOutcome::Requested)
    }

    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: bounded wait and one exit-code read on exact process handle"
    )]
    pub(crate) fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ExitObservation>, PlatformError> {
        if let Some(exit) = &self.exit {
            return Ok(Some(exit.clone()));
        }
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: process is a valid uniquely owned process handle.
        let wait = unsafe { WaitForSingleObject(self.process.get(), millis) };
        if wait == WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait == WAIT_FAILED {
            return Err(PlatformError::last("WaitForSingleObject direct child"));
        }
        if wait != WAIT_OBJECT_0 {
            return Err(PlatformError::io(
                "WaitForSingleObject direct child",
                std::io::Error::other(format!("unexpected wait result {wait}")),
            ));
        }
        let mut code = 0_u32;
        // SAFETY: exit code pointer is initialized and the process is signaled.
        if unsafe { GetExitCodeProcess(self.process.get(), &raw mut code) } == FALSE {
            return Err(PlatformError::last("GetExitCodeProcess"));
        }
        let forced_by_supervisor = self.forced && code == SUPERVISOR_TERMINATION_CODE;
        let observation = ExitObservation {
            exit_code: (code == 0).then_some(0),
            signal: None,
            windows_status_opaque: (code != 0).then_some(code),
            // Windows supplies only an opaque numeric termination status here;
            // it does not prove exception provenance. This flag is limited to
            // the exact code used by this owned Job termination request.
            forced_by_supervisor,
        };
        self.exit = Some(observation.clone());
        Ok(Some(observation))
    }

    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: independent exact Job accounting query"
    )]
    pub(crate) fn declared_scope_empty(&self) -> Result<bool, PlatformError> {
        Ok(self.active_process_count()? == 0)
    }

    #[allow(
        unsafe_code,
        reason = "FF-DEC-003: independent exact Job accounting query"
    )]
    pub(crate) fn active_process_count(&self) -> Result<u32, PlatformError> {
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        let size =
            u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).map_err(|_| {
                PlatformError::io(
                    "Job accounting size",
                    std::io::Error::other("size overflow"),
                )
            })?;
        // SAFETY: exact information class and initialized output buffer.
        if unsafe {
            QueryInformationJobObject(
                self.job.get(),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                size,
                null_mut(),
            )
        } == FALSE
        {
            return Err(PlatformError::last("QueryInformationJobObject"));
        }
        Ok(accounting.ActiveProcesses)
    }

    pub(crate) const fn preexecution_active_process_count(&self) -> u32 {
        self.preexecution_active_processes
    }

    pub(crate) const fn attached_before_execution(&self) -> bool {
        self.attached_before_execution
    }

    pub(crate) const fn kill_on_close(&self) -> bool {
        self.kill_on_close
    }

    /// Force the exact owned Job when needed, reap the direct child once, and
    /// independently prove Job accounting reached zero within one deadline.
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
                    "cleanup Job accounting",
                    "active processes remained at deadline",
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

fn wide_nul(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn encode_environment(environment: &[(String, String)]) -> Result<Vec<u16>, PlatformError> {
    let mut sorted = environment.to_vec();
    sorted.sort_by_key(|item| item.0.to_uppercase());
    let mut block = Vec::new();
    for (key, value) in sorted {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            return Err(PlatformError::io(
                "encode environment",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid environment binding",
                ),
            ));
        }
        block.extend(std::ffi::OsStr::new(&format!("{key}={value}")).encode_wide());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn encode_command_line(executable: &Path, arguments: &[String]) -> Vec<u16> {
    let mut command = quote_windows_argument(&executable.as_os_str().to_string_lossy());
    for argument in arguments {
        command.push(' ');
        command.push_str(&quote_windows_argument(argument));
    }
    std::ffi::OsStr::new(&command)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn quote_windows_argument(argument: &str) -> String {
    if !argument.is_empty()
        && !argument
            .bytes()
            .any(|byte| byte == b' ' || byte == b'\t' || byte == b'"')
    {
        return argument.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut slashes = 0_usize;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            quoted.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
            quoted.push('"');
            slashes = 0;
            continue;
        }
        quoted.extend(std::iter::repeat_n('\\', slashes));
        slashes = 0;
        quoted.push(character);
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    fn powershell() -> std::path::PathBuf {
        Path::new(&std::env::var("SystemRoot").expect("SystemRoot"))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe")
    }

    fn trusted_environment() -> Vec<(String, String)> {
        let mut environment = ["PATH", "PATHEXT"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect::<Vec<_>>();
        crate::platform::append_windows_system_environment(
            &mut environment,
            &std::env::var("SystemRoot").expect("SystemRoot"),
        )
        .expect("governed system environment");
        let temporary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .join(".fforager-artifacts/test-runs/ffmpeg-platform-windows");
        std::fs::create_dir_all(&temporary).expect("artifact temporary directory");
        crate::platform::append_windows_writable_environment(
            &mut environment,
            &temporary.display().to_string(),
        );
        environment
    }

    fn wait_for_test_job_empty(
        child: &PlatformChild,
        timeout: Duration,
    ) -> Result<bool, PlatformError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        if child.declared_scope_empty()? {
            return Ok(true);
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let empty = child.declared_scope_empty()?;
            if Instant::now() >= deadline {
                return Ok(false);
            }
            if empty {
                return Ok(true);
            }
        }
    }

    #[test]
    fn every_preexecution_verification_failure_terminates_waits_and_proves_active_zero() {
        for (fault, expected) in [
            (
                PreexecutionFault::Query,
                "query pre-execution Job assignment",
            ),
            (
                PreexecutionFault::ActiveCount,
                "verify pre-execution Job assignment",
            ),
            (
                PreexecutionFault::ResumeCount,
                "ResumeThread exact suspend count",
            ),
        ] {
            let boundary = PreparedBoundary::new().expect("prepared boundary");
            let arguments = vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                "Start-Sleep -Seconds 30".to_owned(),
            ];
            let (process, thread) = create_suspended_process(
                &powershell(),
                &arguments,
                &trusted_environment(),
                Path::new(env!("CARGO_MANIFEST_DIR")),
                &boundary,
                None,
            )
            .expect("suspended fixture");
            let error = verify_preexecution_assignment_and_resume_with_fault(
                &boundary.job,
                &process,
                &thread,
                fault,
            )
            .expect_err("fault must fail before handoff");
            assert_eq!(error.operation, expected);
            assert_eq!(
                query_job_active_processes(&boundary.job).expect("independent active query"),
                0,
                "{fault:?} must prove active zero before returning"
            );
            // SAFETY: the exact owned process handle must already be signaled
            // by the bounded direct wait inside pre-handoff cleanup.
            assert_eq!(
                unsafe { WaitForSingleObject(process.get(), 0) },
                WAIT_OBJECT_0
            );
        }
    }

    #[test]
    #[ignore = "spawned abruptly by the KILL_ON_JOB_CLOSE parent-death regression"]
    fn kill_on_job_close_parent_death_child_helper() {
        let receipt = std::env::var_os(PARENT_DEATH_RECEIPT_ENV).expect("receipt path");
        let executable = powershell();
        let command = concat!(
            "$tool=Join-Path $env:SystemRoot 'System32\\ping.exe'; ",
            "$p=Start-Process -FilePath $tool -ArgumentList '-n','30','127.0.0.1' ",
            "-WindowStyle Hidden -PassThru; ",
            "[Console]::Out.WriteLine(\"$PID,$($p.Id)\"); Start-Sleep -Seconds 30"
        );
        let arguments = vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            command.to_owned(),
        ];
        let mut child = spawn(
            &executable,
            &arguments,
            &trusted_environment(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("spawn owned Job child and descendant");
        let (stdout, _stderr) = child.take_pipes().expect("child pipes");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read direct and descendant PIDs");
        let mut receipt_file = std::fs::File::create(receipt).expect("create parent-death receipt");
        receipt_file
            .write_all(line.trim().as_bytes())
            .expect("write parent-death receipt");
        receipt_file.sync_all().expect("sync parent-death receipt");
        std::mem::forget(child);
        std::process::exit(0);
    }

    #[test]
    fn kill_on_job_close_terminates_child_and_descendant_after_abrupt_parent_exit() {
        let temporary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .join(format!(
                ".fforager-artifacts/test-runs/windows-parent-death-{}",
                std::process::id()
            ));
        std::fs::create_dir_all(&temporary).expect("parent-death root");
        let receipt = temporary.join("pids.txt");
        let mut environment = trusted_environment();
        environment.push((
            PARENT_DEATH_RECEIPT_ENV.to_owned(),
            receipt.display().to_string(),
        ));
        let arguments = vec![
            "--exact".to_owned(),
            PARENT_DEATH_HELPER_TEST.to_owned(),
            "--ignored".to_owned(),
            "--test-threads=1".to_owned(),
        ];
        let observation = observe_kill_on_job_close_parent_death(
            &std::env::current_exe().expect("current test executable"),
            &arguments,
            &environment,
            &temporary,
            &receipt,
            Duration::from_secs(20),
        )
        .expect("parent-death observation");
        assert!(observation.kill_on_job_close_parent_death_observed);
        assert_ne!(observation.direct_child_pid, observation.descendant_pid);
        assert!(observation.elapsed_millis < 20_000);
    }

    #[test]
    fn windows_argument_quoting_preserves_spaces_quotes_and_trailing_slashes() {
        assert_eq!(quote_windows_argument("plain"), "plain");
        assert_eq!(quote_windows_argument("two words"), "\"two words\"");
        assert_eq!(quote_windows_argument("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(quote_windows_argument("tail\\"), "tail\\");
        assert_eq!(
            quote_windows_argument("tail slash \\"),
            "\"tail slash \\\\\""
        );
    }

    #[test]
    fn spawn_proves_preexecution_job_and_caches_nonzero_direct_wait() {
        let executable = powershell();
        let arguments = vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            "[Console]::Out.Write('proof'); exit 7".to_owned(),
        ];
        let mut child = spawn(
            &executable,
            &arguments,
            &trusted_environment(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("spawn");
        assert!(child.attached_before_execution());
        assert!(child.kill_on_close());
        assert_eq!(child.preexecution_active_process_count(), 1);
        assert_eq!(child.active_process_count().expect("active count"), 1);
        let (mut stdout, mut stderr) = child.take_pipes().expect("pipes");
        let mut output = String::new();
        stdout.read_to_string(&mut output).expect("read stdout");
        let mut diagnostics = String::new();
        stderr
            .read_to_string(&mut diagnostics)
            .expect("read stderr");
        assert_eq!(output, "proof", "stderr: {diagnostics}");
        let exit = child
            .wait_timeout(Duration::from_secs(10))
            .expect("wait")
            .expect("exit");
        assert_eq!(exit.exit_code, None);
        assert_eq!(exit.signal, None);
        assert_eq!(exit.windows_status_opaque, Some(7));
        assert!(!exit.forced_by_supervisor);
        assert_eq!(
            child.wait_timeout(Duration::ZERO).expect("cached"),
            Some(exit)
        );
        assert!(
            wait_for_test_job_empty(&child, Duration::from_secs(10))
                .expect("bounded active-zero observation")
        );
        assert_eq!(child.active_process_count().expect("active count"), 0);
        assert_eq!(
            child
                .force_terminate()
                .expect("force after exact cached reap"),
            ForceTerminationOutcome::AlreadyEmpty
        );
    }

    #[test]
    fn job_empty_wait_requires_actual_zero_and_natural_release() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let control_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .join(format!(
                ".fforager-artifacts/test-runs/job-empty-release-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&control_root).expect("owned release root");
        let release = control_root.join("release");
        let release_literal = release.to_str().expect("release path").replace('\'', "''");
        let command = format!(
            "$deadline=[DateTime]::UtcNow.AddSeconds(30); while (!(Test-Path -LiteralPath '{release_literal}')) {{ if ([DateTime]::UtcNow -ge $deadline) {{ exit 29 }}; Start-Sleep -Milliseconds 5 }}; exit 0"
        );
        let mut child = spawn(
            &powershell(),
            &[
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                command,
            ],
            &trusted_environment(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("spawn release-held Job");
        let (_stdout, _stderr) = child.take_pipes().expect("pipes");
        assert_eq!(
            child
                .wait_timeout(Duration::ZERO)
                .expect("held direct wait"),
            None
        );
        let held_active = child.active_process_count().expect("held Job count");
        assert!(held_active > 0, "the real Job must be occupied");
        let negative_started = Instant::now();
        let held_empty = wait_for_test_job_empty(&child, Duration::from_millis(25))
            .expect("bounded occupied Job observation");
        eprintln!(
            "JOB-EMPTY-PROOF held_active={held_active} observed_empty={held_empty} elapsed_us={}",
            negative_started.elapsed().as_micros()
        );
        assert!(!held_empty, "an occupied Job must never acknowledge empty");
        assert!(negative_started.elapsed() >= Duration::from_millis(25));
        assert!(child.active_process_count().expect("still held Job") > 0);

        let release_writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            std::fs::write(release, b"release").expect("naturally release owned Job");
        });
        let empty = wait_for_test_job_empty(&child, Duration::from_secs(10))
            .expect("bounded naturally released Job observation");
        release_writer.join().expect("release writer");
        assert!(empty, "natural release must reach observed zero accounting");
        let exit = child
            .wait_timeout(Duration::from_secs(10))
            .expect("direct wait")
            .expect("direct exit");
        assert_eq!(exit.exit_code, Some(0));
        assert!(!exit.forced_by_supervisor);
        assert_eq!(
            child
                .wait_timeout(Duration::ZERO)
                .expect("cached direct wait"),
            Some(exit)
        );
        let final_active = child.active_process_count().expect("final Job count");
        eprintln!("JOB-EMPTY-PROOF natural_release_empty={empty} final_active={final_active}");
        assert_eq!(final_active, 0);
        assert_eq!(
            child.force_terminate().expect("already empty Job"),
            ForceTerminationOutcome::AlreadyEmpty
        );
        std::fs::remove_dir_all(control_root).expect("remove owned release root");
    }

    #[test]
    fn force_terminate_cleans_descendant_after_direct_child_was_reaped() {
        let executable = powershell();
        let command = concat!(
            "$tool=Join-Path $env:SystemRoot 'System32\\ping.exe'; ",
            "$p=Start-Process -FilePath $tool -ArgumentList '-n','30','127.0.0.1' ",
            "-WindowStyle Hidden -PassThru; [Console]::Out.WriteLine($p.Id); exit 0"
        );
        let arguments = vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            command.to_owned(),
        ];
        let mut child = spawn(
            &executable,
            &arguments,
            &trusted_environment(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("spawn");
        assert_eq!(child.preexecution_active_process_count(), 1);
        let (_stdout, _stderr) = child.take_pipes().expect("pipes");
        let direct_exit = child
            .wait_timeout(Duration::from_secs(10))
            .expect("wait")
            .expect("direct exit");
        assert_eq!(direct_exit.exit_code, Some(0));
        assert!(!child.declared_scope_empty().expect("descendant active"));
        assert!(child.active_process_count().expect("active count") > 0);
        child.force_terminate().expect("terminate whole Job");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !child.declared_scope_empty().expect("active query")
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(child.declared_scope_empty().expect("active zero"));
        assert_eq!(child.active_process_count().expect("active count"), 0);
        assert_eq!(
            child.wait_timeout(Duration::ZERO).expect("cached wait"),
            Some(direct_exit),
            "descendant termination must not rewrite or repeat the direct-child reap"
        );
    }

    #[test]
    fn forced_direct_wait_is_bound_to_the_supervisor_job_exit_code() {
        let executable = powershell();
        let arguments = vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            "Start-Sleep -Seconds 30".to_owned(),
        ];
        let mut child = spawn(
            &executable,
            &arguments,
            &trusted_environment(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("spawn");
        let (_stdout, _stderr) = child.take_pipes().expect("pipes");
        child.force_terminate().expect("terminate Job");
        let exit = child
            .wait_timeout(Duration::from_secs(5))
            .expect("wait")
            .expect("exit");
        assert_eq!(exit.exit_code, None);
        assert_eq!(
            exit.windows_status_opaque,
            Some(SUPERVISOR_TERMINATION_CODE)
        );
        assert!(exit.forced_by_supervisor);
        assert!(child.declared_scope_empty().expect("active zero"));
    }

    #[test]
    fn handle_list_excludes_forbidden_inheritable_sentinel_and_mutation_leaks_it() {
        let executable = powershell();
        let working_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
        let observation = run_handle_list_fault_probe(
            &executable,
            &trusted_environment(),
            working_directory,
            Duration::from_secs(10),
        )
        .expect("handle-list fault observation");
        assert!(observation.protected_exit.windows_status_opaque.is_some());
        assert!(!observation.protected_exit.successful());
        assert!(!observation.protected_exit.forced_by_supervisor);
        assert!(observation.protected_bytes.is_empty());
        assert!(observation.mutated_exit.successful());
        assert_eq!(observation.mutated_bytes, b"LEAK");
    }
}
