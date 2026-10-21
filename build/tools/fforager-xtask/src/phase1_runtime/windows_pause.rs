//! FF-DEC-005: retained owned-actor identity; only watchers support pause/resume.
#![allow(
    unsafe_code,
    reason = "FF-DEC-005: scoped owned-actor identity and watcher-only pause/resume guard"
)]

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Diagnostics::Debug::{
    DebugActiveProcess, DebugActiveProcessStop, DebugSetProcessKillOnExit,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};

struct OwnedProcess {
    handle: HANDLE,
    pid: u32,
    creation: u64,
}

impl OwnedProcess {
    fn open(pid: u32, image: &Path, launched_after: SystemTime) -> Result<Self, String> {
        if pid == 0 || pid == std::process::id() {
            return Err("invalid owned actor PID".into());
        }
        // SAFETY: requests query and exit-wait access only; no inheritance or pointer argument.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut result = Self {
            handle,
            pid,
            creation: 0,
        };
        result.creation = result.creation_time()?;
        let lower = launched_after
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?;
        let lower = (lower.as_nanos() / 100)
            .checked_add(116_444_736_000_000_000)
            .ok_or("creation clock overflow")?;
        if u128::from(result.creation) < lower {
            return Err("owned actor predates this invocation".into());
        }
        let mut buffer = vec![0_u16; 32768];
        let mut length = u32::try_from(buffer.len()).map_err(|error| error.to_string())?;
        // SAFETY: retained live process handle, sized writable UTF-16 buffer and length.
        if unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        buffer.truncate(length as usize);
        let observed = std::path::PathBuf::from(
            String::from_utf16(&buffer).map_err(|error| error.to_string())?,
        );
        let expected = image.canonicalize().map_err(|error| error.to_string())?;
        let observed = observed.canonicalize().map_err(|error| error.to_string())?;
        if !observed
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
        {
            return Err("owned actor image differs from the staged executable".into());
        }
        result.verify_live()?;
        Ok(result)
    }

    fn verify_live(&self) -> Result<(), String> {
        if self.creation_time()? != self.creation {
            return Err("owned actor creation identity changed".into());
        }
        // SAFETY: retained query/synchronize handle; a zero timeout never blocks.
        if unsafe { WaitForSingleObject(self.handle, 0) } != WAIT_TIMEOUT {
            return Err("owned actor is not running".into());
        }
        Ok(())
    }

    fn wait_exit(&self, timeout: Duration) -> Result<(), String> {
        let millis = u32::try_from(timeout.as_millis()).map_err(|error| error.to_string())?;
        // SAFETY: this guard retains the exact owned process handle with SYNCHRONIZE access.
        if unsafe { WaitForSingleObject(self.handle, millis) } != WAIT_OBJECT_0 {
            return Err("owned actor exit was not acknowledged before its deadline".into());
        }
        Ok(())
    }

    fn creation_time(&self) -> Result<u64, String> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: retained query handle and four correctly sized initialized outputs.
        if unsafe {
            GetProcessTimes(
                self.handle,
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: this instance owns the single OpenProcess handle.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

pub(super) struct OwnedWorker {
    identity: OwnedProcess,
}

impl OwnedWorker {
    pub(super) fn open(pid: u32, image: &Path, launched_after: SystemTime) -> Result<Self, String> {
        if image.file_name() != Some(std::ffi::OsStr::new("fforager-worker.exe")) {
            return Err("worker identity requires the fixed staged worker executable".into());
        }
        Ok(Self {
            identity: OwnedProcess::open(pid, image, launched_after)?,
        })
    }

    pub(super) fn pid(&self) -> u32 {
        self.identity.pid
    }

    pub(super) fn verify_live(&self) -> Result<(), String> {
        self.identity.verify_live()
    }

    pub(super) fn wait_exit(&self, timeout: Duration) -> Result<(), String> {
        self.identity.wait_exit(timeout)
    }
}

pub(super) struct OwnedWatcher {
    identity: OwnedProcess,
    paused_at: Option<Instant>,
}

impl OwnedWatcher {
    pub(super) fn open(pid: u32, image: &Path, launched_after: SystemTime) -> Result<Self, String> {
        if image.file_name() != Some(std::ffi::OsStr::new("fforager-watcher.exe")) {
            return Err("watcher identity requires the fixed staged watcher executable".into());
        }
        Ok(Self {
            identity: OwnedProcess::open(pid, image, launched_after)?,
            paused_at: None,
        })
    }

    pub(super) fn pid(&self) -> u32 {
        self.identity.pid
    }

    pub(super) fn wait_exit(&self, timeout: Duration) -> Result<(), String> {
        self.identity.wait_exit(timeout)
    }

    pub(super) fn pause(&mut self) -> Result<(), String> {
        if self.paused_at.is_some() {
            return Err("watcher pause identity/state mismatch".into());
        }
        self.identity.verify_live()?;
        // SAFETY: PID is tied to a retained handle, creation time and exact staged image;
        // the debugger only controls this invocation's actual watcher.
        if unsafe { DebugActiveProcess(self.identity.pid) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        self.paused_at = Some(Instant::now());
        // SAFETY: applies to this validator's debugger relationship; never kill on debugger exit.
        if unsafe { DebugSetProcessKillOnExit(0) } == 0 {
            let error = std::io::Error::last_os_error().to_string();
            self.resume()?;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn resume(&mut self) -> Result<(), String> {
        let Some(started) = self.paused_at else {
            return Ok(());
        };
        // SAFETY: detach exactly the PID attached by this guard; retained handle
        // prevents lifetime ambiguity, and Drop retries if this call fails.
        if unsafe { DebugActiveProcessStop(self.identity.pid) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        self.paused_at = None;
        if started.elapsed() > Duration::from_secs(2) {
            return Err("watcher pause exceeded its two-second maximum".into());
        }
        Ok(())
    }
}

impl Drop for OwnedWatcher {
    fn drop(&mut self) {
        if let Err(error) = self.resume() {
            eprintln!("FF-RUNTIME-E-WATCHER-DETACH: {error}");
        }
    }
}
