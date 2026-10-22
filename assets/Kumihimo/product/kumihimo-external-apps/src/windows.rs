//! Source pattern: Komorebi windows_api.rs SetWindowPos (NOACTIVATE/NOZORDER).
//! Dispatch is asynchronous; observation, identity and compensation remain host contracts.
use crate::*;
use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, System::Threading::*, UI::{HiDpi::*, WindowsAndMessaging::*}};

#[derive(Default)]
pub struct WindowsBackend;
pub struct WindowsLease { process: HANDLE, identity: WindowIdentity, thread_id: u32, marker_name: Vec<u16>, marker_token: Box<u8>, marker_installed: bool }
// Immutable retained Win32 handles/property identity may be queried across threads.
// Each hosting backend admits only one mutation worker; CloseHandle runs after its last Arc.
unsafe impl Send for WindowsLease {}
unsafe impl Sync for WindowsLease {}
impl WindowsLease {
    pub fn hwnd(&self) -> HWND { self.identity.hwnd as usize as HWND }
    pub fn identity(&self) -> WindowIdentity { self.identity }
    pub fn thread_id(&self) -> u32 { self.thread_id }
    /// Recheck immediately before an additional native-adapter effect.
    pub fn validate(&self) -> Result<(), Diagnostic> { validate(self, self.identity) }
}
impl Drop for WindowsLease {
    fn drop(&mut self) {
        // Remove only our own window-scoped marker; never touch a replacement window.
        if self.marker_installed && unsafe { GetPropW(self.hwnd(), self.marker_name.as_ptr()) } == (&*self.marker_token as *const u8 as HANDLE) {
            unsafe { RemovePropW(self.hwnd(), self.marker_name.as_ptr()); }
        }
        unsafe { CloseHandle(self.process); }
    }
}
pub(crate) fn os(code: &str, message: &str) -> Diagnostic {
    Diagnostic { code: code.into(), message: message.into(), os_error: Some(unsafe { GetLastError() }) }
}
fn creation(process: HANDLE) -> Result<u64, Diagnostic> {
    let mut start: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit = start; let mut kernel = start; let mut user = start;
    if unsafe { GetProcessTimes(process, &mut start, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(os("process_times", "Cannot query retained process creation time"));
    }
    Ok((u64::from(start.dwHighDateTime) << 32) | u64::from(start.dwLowDateTime))
}
fn validate(lease: &WindowsLease, expected: WindowIdentity) -> Result<(), Diagnostic> {
    if lease.identity != expected || unsafe { WaitForSingleObject(lease.process, 0) } != WAIT_TIMEOUT {
        return Err(Diagnostic::new("identity_gone", "Retained process is absent, exited, or differs from selection"));
    }
    if unsafe { GetProcessId(lease.process) } != expected.pid || creation(lease.process)? != expected.creation_time {
        return Err(Diagnostic::new("identity_replaced", "Retained process lifetime differs from selection"));
    }
    let mut owner = 0;
    let thread = unsafe { GetWindowThreadProcessId(lease.hwnd(), &mut owner) };
    if unsafe { IsWindow(lease.hwnd()) } == 0 || owner != expected.pid || thread != lease.thread_id {
        return Err(Diagnostic::new("window_replaced", "Window no longer belongs to the selected process/thread"));
    }
    if lease.marker_installed && unsafe { GetPropW(lease.hwnd(), lease.marker_name.as_ptr()) } != (&*lease.marker_token as *const u8 as HANDLE) {
        return Err(Diagnostic::new("window_lifetime_invalidated", "Window-scoped lease marker is absent or changed; same-process HWND reuse is rejected"));
    }
    Ok(())
}
pub(crate) struct DpiGuard(DPI_AWARENESS_CONTEXT);
impl DpiGuard {
    pub(crate) fn enter() -> Result<Self, Diagnostic> {
        let prior = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if prior.is_null() { Err(os("dpi_context", "Physical-pixel thread context is unavailable")) } else { Ok(Self(prior)) }
    }
}
impl Drop for DpiGuard { fn drop(&mut self) { unsafe { SetThreadDpiAwarenessContext(self.0); } } }
fn bounds(rect: RECT) -> Result<PixelBounds, Diagnostic> {
    let bounds = PixelBounds { x: rect.left, y: rect.top,
        width: rect.right.checked_sub(rect.left).ok_or_else(|| Diagnostic::new("bounds_overflow", "OS width overflows"))?,
        height: rect.bottom.checked_sub(rect.top).ok_or_else(|| Diagnostic::new("bounds_overflow", "OS height overflows"))? };
    bounds.validate()?; Ok(bounds)
}
impl WindowsBackend {
    pub fn bind_owned_parent(&mut self, target: WindowTarget) -> Result<WindowsLease, Diagnostic> {
        if target.pid != unsafe { GetCurrentProcessId() } { return Err(Diagnostic::new("parent_not_owned", "Native hosting parent must belong to this host process")); }
        self.bind_window(target, true).map(|(lease, _)| lease)
    }
    fn bind_window(&mut self, target: WindowTarget, owned_parent: bool) -> Result<(WindowsLease, WindowIdentity), Diagnostic> {
        if target.hwnd == 0 || target.pid == 0 || target.hwnd > usize::MAX as u64 {
            return Err(Diagnostic::new("invalid_target", "Explicit representable HWND and nonzero PID required"));
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, target.pid) };
        if process.is_null() { return Err(os("process_open", "Cannot retain selected process handle")); }
        let creation_time = match creation(process) { Ok(value) => value, Err(error) => { unsafe { CloseHandle(process); } return Err(error); } };
        let hwnd = target.hwnd as usize as HWND;
        let mut owner = 0;
        let thread_id = unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        let identity = WindowIdentity { hwnd: target.hwnd, pid: target.pid, creation_time };
        let marker_token = Box::new(0u8);
        let marker_name: Vec<u16> = format!("Kumihimo.ExternalApps.Lease.{}.{}", unsafe { GetCurrentProcessId() }, &*marker_token as *const u8 as usize).encode_utf16().chain(Some(0)).collect();
        let mut lease = WindowsLease { process, identity, thread_id, marker_name, marker_token, marker_installed:false };
        if owner != target.pid || target.expected_creation_time.is_some_and(|start| start != creation_time) {
            return Err(Diagnostic::new("selection_mismatch", "Explicit owner or creation time does not match live selection"));
        }
        validate(&lease, identity)?;
        if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd || (!owned_parent && unsafe { IsWindowVisible(hwnd) } == 0) {
            return Err(Diagnostic::new("unsupported_window", "Only explicitly selected visible top-level windows are supported"));
        }
        if unsafe { IsIconic(hwnd) } != 0 || unsafe { IsZoomed(hwnd) } != 0 {
            return Err(Diagnostic::new("unsupported_initial_state", "Only normal windows have quietly restorable placement"));
        }
        // Explicit selected-window metadata effect; UIPI denial rejects binding.
        // A destroyed window loses this window-specific property, even if its HWND is reused.
        if !unsafe { GetPropW(hwnd, lease.marker_name.as_ptr()) }.is_null() {
            return Err(Diagnostic::new("lease_marker_collision", "Existing window property is never overwritten"));
        }
        if unsafe { SetPropW(hwnd, lease.marker_name.as_ptr(), &*lease.marker_token as *const u8 as HANDLE) } == 0 {
            return Err(os("lease_marker_denied", "Cannot install selected-window lifetime witness"));
        }
        lease.marker_installed = true;
        validate(&lease, identity)?;
        Ok((lease, identity))
    }
}
impl WindowBackend for WindowsBackend {
    type Lease = WindowsLease;
    fn bind(&mut self, target: WindowTarget) -> Result<(WindowsLease, WindowIdentity), Diagnostic> { self.bind_window(target, false) }
    fn validate(&mut self, lease: &WindowsLease, identity: WindowIdentity) -> Result<(), Diagnostic> { validate(lease, identity) }
    fn observe(&mut self, lease: &WindowsLease) -> Result<Placement, Diagnostic> {
        lease.validate()?;
        let _dpi = DpiGuard::enter()?;
        let mut rect = unsafe { std::mem::zeroed() };
        if unsafe { GetWindowRect(lease.hwnd(), &mut rect) } == 0 { return Err(os("window_bounds", "Cannot observe selected OS window bounds")); }
        let state = if unsafe { IsIconic(lease.hwnd()) } != 0 { WindowState::Minimized }
            else if unsafe { IsZoomed(lease.hwnd()) } != 0 { WindowState::Maximized } else { WindowState::Normal };
        lease.validate()?;
        Ok(Placement { bounds: bounds(rect)?, state })
    }
    fn capabilities(&mut self, lease: &WindowsLease) -> Result<Capabilities, Diagnostic> {
        lease.validate()?;
        let _dpi = DpiGuard::enter()?;
        let monitor = unsafe { MonitorFromWindow(lease.hwnd(), MONITOR_DEFAULTTONEAREST) };
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() }; info.cbSize = size_of::<MONITORINFO>() as u32;
        if monitor.is_null() || unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 { return Err(os("monitor_info", "Cannot observe current monitor work area")); }
        let dpi = unsafe { GetDpiForWindow(lease.hwnd()) };
        if dpi == 0 { return Err(os("window_dpi", "Cannot observe selected window DPI")); }
        let mut sizing: MINMAXINFO = unsafe { std::mem::zeroed() }; let mut response = 0;
        // System message below WM_USER is marshalled by Windows; never use an app-private pointer message.
        if unsafe { SendMessageTimeoutW(lease.hwnd(), WM_GETMINMAXINFO, 0, &mut sizing as *mut MINMAXINFO as isize, SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT, 100, &mut response) } == 0 {
            return Err(os("minimum_size_unavailable", "Bounded minimum-size query failed; allocation is unsupported"));
        }
        lease.validate()?;
        Ok(Capabilities { work_area: bounds(info.rcWork)?, dpi, minimum_width: sizing.ptMinTrackSize.x.max(1), minimum_height: sizing.ptMinTrackSize.y.max(1),
            can_resize: (unsafe { GetWindowLongPtrW(lease.hwnd(), GWL_STYLE) } as u32 & WS_THICKFRAME) != 0 })
    }
    fn request_bounds(&mut self, lease: &WindowsLease, bounds: PixelBounds) -> Result<(), Diagnostic> {
        bounds.validate()?; lease.validate()?;
        let _dpi = DpiGuard::enter()?;
        if unsafe { IsIconic(lease.hwnd()) } != 0 || unsafe { IsZoomed(lease.hwnd()) } != 0 {
            return Err(Diagnostic::new("unsupported_state", "Quiet positioning does not alter minimized/maximized state"));
        }
        if unsafe { SetWindowPos(lease.hwnd(), null_mut(), bounds.x, bounds.y, bounds.width, bounds.height,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS) } == 0 {
            return Err(os("position_request", "OS rejected asynchronous quiet positioning request"));
        }
        Ok(())
    }
    fn request_restore(&mut self, lease: &WindowsLease, placement: Placement) -> Result<(), Diagnostic> {
        if placement.state != WindowState::Normal { return Err(Diagnostic::new("unsupported_restore_state", "Non-normal state restoration is unsupported without possible activation")); }
        self.request_bounds(lease, placement.bounds)
    }
    fn observation_pause(&mut self, milliseconds: u64) { std::thread::sleep(std::time::Duration::from_millis(milliseconds)); }
}
