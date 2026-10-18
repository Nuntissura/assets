use std::time::Duration;

use super::{ParentExit, ParentWatch};

// ----------------------------------------------------------------------------------------------------
// Windows parent-handle watch (the REAL implementation; behind the ParentWatch seam).
// ----------------------------------------------------------------------------------------------------

/// The `SYNCHRONIZE` process-access right (`0x0010_0000`). In windows-sys 0.61 the named `SYNCHRONIZE`
/// constant lives under `Win32::Storage::FileSystem` as a `FILE_ACCESS_RIGHTS`, but the access mask is a
/// plain `u32` shared across object types, so we name the value locally to OR it with the process rights
/// without pulling in the FileSystem access-rights type. This is the standard kernel object SYNCHRONIZE
/// right needed for `WaitForSingleObject` on a process handle.
#[cfg(windows)]
const SYNCHRONIZE_RIGHT: u32 = 0x0010_0000;

/// The real Windows parent watch: `OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION)` to get
/// a handle to the parent, then `WaitForSingleObject(handle, timeout)`; when it signals, the parent has
/// EXITED, and `GetExitCodeProcess` reads the OS exit code. This is the field-standard Crashpad-style
/// parent observation. The handle is held for the watcher's life and closed on drop.
#[cfg(windows)]
pub struct WindowsParentWatch {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: a Win32 process HANDLE is a kernel object reference valid process-wide, not tied to the thread
// that opened it; the OS APIs we call (WaitForSingleObject / GetExitCodeProcess / CloseHandle) are all
// thread-safe to call on the same handle. We hand the watch to a dedicated watch thread (the lifecycle
// owns exclusive use — only that thread waits on it), so moving it across the thread boundary is sound.
#[cfg(windows)]
unsafe impl Send for WindowsParentWatch {}

#[cfg(windows)]
impl WindowsParentWatch {
    /// Open a SYNCHRONIZE + QUERY handle to `pid`. Returns `None` if the process cannot be opened
    /// (already gone / access denied) — the caller treats that as a vanished parent.
    pub fn open(pid: u32) -> Option<Self> {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: OpenProcess is a documented Win32 call; we pass a valid access mask + pid and check
        // the returned handle against null before using it. bInheritHandle = FALSE (0).
        let handle = unsafe {
            OpenProcess(
                SYNCHRONIZE_RIGHT | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }
}

#[cfg(windows)]
impl ParentWatch for WindowsParentWatch {
    fn wait_for_parent_exit(&self, timeout: Duration) -> Option<ParentExit> {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: `self.handle` is a valid process handle opened with SYNCHRONIZE.
        let wait = unsafe { WaitForSingleObject(self.handle, millis) };
        if wait == WAIT_TIMEOUT {
            return None; // still alive; poll again
        }
        if wait != WAIT_OBJECT_0 {
            // WAIT_FAILED / WAIT_ABANDONED: the handle is unusable; treat the parent as vanished.
            return Some(ParentExit::Vanished);
        }
        // The parent has exited; read its exit code.
        let mut code: u32 = 0;
        // SAFETY: handle is valid; `code` is a valid out pointer.
        let ok = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        if ok == 0 {
            Some(ParentExit::Vanished)
        } else {
            Some(ParentExit::Exited { code })
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsParentWatch {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        if !self.handle.is_null() {
            // SAFETY: closing a handle we opened and have not closed yet.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}
