//! Windows Job Object caps for one worker process (see `os_caps` for the contract). This is the only
//! module of the crate with `unsafe` (the crate root is `deny(unsafe_code)` with this module allowed
//! when `windows` and `quickjs` are both active, and `forbid(unsafe_code)` in every other build).
//!
//! Every call below is a plain Win32 call on handles this module creates and owns; the only pointers
//! passed are to stack-local, fully initialised structures of the size passed alongside them.
#![deny(clippy::arithmetic_side_effects)]
use crate::os_caps::{OsEvents, OsLimits};
use std::{ffi::c_void, io, mem, os::windows::io::AsRawHandle, process::Child, ptr, time::Duration};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
            JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject,
        },
    },
};

// Completion-port message ids (winnt.h; values confirmed against the winapi 0.3.9 constants:
// END_OF_JOB_TIME 1, END_OF_PROCESS_TIME 2, PROCESS_MEMORY_LIMIT 9, JOB_MEMORY_LIMIT 10).
const MSG_END_OF_JOB_TIME: u32 = 1;
const MSG_END_OF_PROCESS_TIME: u32 = 2;
const MSG_PROCESS_MEMORY_LIMIT: u32 = 9;
const MSG_JOB_MEMORY_LIMIT: u32 = 10;
/// Upper bound on packets read per drain (new/exit-process notices also arrive on the port).
const MAX_PACKETS: usize = 4096;

fn size_of<T>() -> u32 {
    u32::try_from(mem::size_of::<T>()).unwrap_or(u32::MAX)
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// One job holding exactly one worker process, plus the completion port its notices arrive on.
pub(crate) struct WorkerJob {
    job: HANDLE,
    port: HANDLE,
    memory_limit: usize,
    /// Kernel-accounted user + kernel time (100 ns) at which the current request is over budget.
    cpu_deadline_100ns: Option<i64>,
}

// SAFETY: kernel handles are process-wide values usable from any thread; every operation on them is
// a thread-safe Win32 call, and the guard is the sole owner that closes them.
unsafe impl Send for WorkerJob {}
unsafe impl Sync for WorkerJob {}

impl WorkerJob {
    /// Creates the job, subscribes its completion port, applies the memory cap and kill-on-close,
    /// then moves `child` into it. The CPU cap is armed per request (`arm_cpu`).
    pub(crate) fn attach(child: &Child, limits: OsLimits) -> io::Result<Self> {
        let memory_limit = usize::try_from(limits.process_memory_bytes).unwrap_or(usize::MAX);
        // SAFETY: see the module header; a failed creation returns null and is reported.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // From here the guard owns `job` and closes it on every early return.
        let mut guard = Self {
            job,
            port: ptr::null_mut(),
            memory_limit,
            cpu_deadline_100ns: None,
        };
        // SAFETY: as above; a null result is an error.
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        guard.port = port;
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: job,
            CompletionPort: port,
        };
        // SAFETY: `association` is a live, initialised struct of the size passed.
        check(unsafe {
            SetInformationJobObject(
                job,
                JobObjectAssociateCompletionPortInformation,
                ptr::from_ref(&association).cast::<c_void>(),
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>(),
            )
        })?;
        guard.set_limits(None)?;
        // SAFETY: `child` owns a live process handle with the access rights `CreateProcess` grants.
        check(unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) })?;
        Ok(guard)
    }

    /// Applies the full limit set. `cpu_limit_100ns` is an absolute per-process user-time limit.
    fn set_limits(&self, cpu_limit_100ns: Option<i64>) -> io::Result<()> {
        // SAFETY: an all-zero bit pattern is a valid value of this plain C struct.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        info.BasicLimitInformation.ActiveProcessLimit = 1;
        info.ProcessMemoryLimit = self.memory_limit;
        if let Some(limit) = cpu_limit_100ns {
            flags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
            info.BasicLimitInformation.PerProcessUserTimeLimit = limit;
        }
        info.BasicLimitInformation.LimitFlags = flags;
        // SAFETY: `info` is a live, initialised struct of the size passed.
        check(unsafe {
            SetInformationJobObject(
                self.job,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&info).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
            )
        })
    }

    /// (user, kernel) time the job's process has used so far, in 100 ns units.
    fn cpu_times_100ns(&self) -> io::Result<(i64, i64)> {
        // SAFETY: an all-zero bit pattern is a valid value of this plain C struct.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { mem::zeroed() };
        // SAFETY: `info` is writable and of the size passed; the returned length is not needed.
        check(unsafe {
            QueryInformationJobObject(
                self.job,
                JobObjectBasicAccountingInformation,
                ptr::from_mut(&mut info).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>(),
                ptr::null_mut(),
            )
        })?;
        Ok((info.TotalUserTime, info.TotalKernelTime))
    }

    /// Re-arms the CPU cap at `used + budget`. The kernel counts a process's whole lifetime, so a
    /// pooled worker needs a fresh allowance per request. Two enforcers share the allowance: the
    /// kernel's per-process user-time limit (a backstop: measured to act seconds late, see
    /// `os_caps`) and `cpu_exceeded`, which the host polls on the kernel's own accounting.
    pub(crate) fn arm_cpu(&mut self, budget: Duration) -> io::Result<()> {
        let (user, kernel) = self.cpu_times_100ns()?;
        let ticks = budget
            .as_nanos()
            .checked_div(100)
            .and_then(|ticks| i64::try_from(ticks).ok())
            .unwrap_or(i64::MAX);
        self.cpu_deadline_100ns = Some(user.saturating_add(kernel).saturating_add(ticks));
        self.set_limits(Some(user.saturating_add(ticks).max(1)))
    }

    /// Whether the worker has used more CPU (user + kernel) than the allowance armed for the
    /// current request, read from the kernel's job accounting.
    pub(crate) fn cpu_exceeded(&self) -> bool {
        let Some(deadline) = self.cpu_deadline_100ns else {
            return false;
        };
        self.cpu_times_100ns()
            .is_ok_and(|(user, kernel)| user.saturating_add(kernel) >= deadline)
    }

    /// Reads every pending notice without blocking and reports which caps fired.
    pub(crate) fn drain(&self) -> OsEvents {
        let mut events = OsEvents::default();
        for _ in 0..MAX_PACKETS {
            let mut message = 0_u32;
            let mut key = 0_usize;
            let mut overlapped: *mut OVERLAPPED = ptr::null_mut();
            // SAFETY: the three out-pointers refer to live locals; a zero timeout never blocks.
            let ok = unsafe {
                GetQueuedCompletionStatus(self.port, &raw mut message, &raw mut key, &raw mut overlapped, 0)
            };
            if ok == 0 && overlapped.is_null() {
                break;
            }
            match message {
                MSG_PROCESS_MEMORY_LIMIT | MSG_JOB_MEMORY_LIMIT => events.memory = true,
                MSG_END_OF_PROCESS_TIME | MSG_END_OF_JOB_TIME => events.cpu = true,
                _ => {}
            }
        }
        events
    }
}

impl Drop for WorkerJob {
    fn drop(&mut self) {
        // SAFETY: both handles are owned by this guard and closed exactly once; closing the job
        // kills any process still in it (KILL_ON_JOB_CLOSE).
        unsafe {
            if !self.port.is_null() {
                CloseHandle(self.port);
            }
            CloseHandle(self.job);
        }
    }
}
