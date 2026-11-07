//! The only `unsafe` in this crate: Win32 window-style and foreground calls winit does not expose. All Vulkan `unsafe`
//! stays in hsk-studio-vulkan. Read-only observations plus style changes on a window this process just created, after
//! Kumihimo's `configure_owned_quiet` and `os_observation` (kumihimo-egui native.rs at e75e34ff).
#![allow(unsafe_code)]
use crate::error::HostError;
use windows_sys::Win32::Foundation::{GetLastError, HWND, SetLastError};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic, IsProcessDPIAware,
    IsWindow, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SetWindowLongPtrW, ShowWindow, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};

fn hwnd(raw: isize) -> HWND {
    raw as HWND
}

/// True when `raw` is a live window owned by this process.
pub(crate) fn owned(raw: isize) -> bool {
    let h = hwnd(raw);
    // SAFETY: read-only queries; a stale handle makes IsWindow return 0.
    unsafe {
        if raw == 0 || IsWindow(h) == 0 {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        pid == std::process::id()
    }
}

/// Sets WS_EX_NOACTIVATE and WS_EX_TOOLWINDOW and clears WS_EX_APPWINDOW on a window this process owns.
pub(crate) fn make_quiet(raw: isize) -> Result<(), HostError> {
    if !owned(raw) {
        return Err(HostError::Window("foreign_window"));
    }
    let h = hwnd(raw);
    // SAFETY: `h` is a live window of this process (checked above); style bits only.
    unsafe {
        let style = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetLastError(0);
        let previous = SetWindowLongPtrW(
            h,
            GWL_EXSTYLE,
            (style & !(WS_EX_APPWINDOW as isize)) | WS_EX_NOACTIVATE as isize | WS_EX_TOOLWINDOW as isize,
        );
        if previous == 0 && GetLastError() != 0 {
            return Err(HostError::Window("quiet_style"));
        }
    }
    Ok(())
}

/// The quiet style bits now on the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuietStyle {
    pub no_activate: bool,
    pub tool_window: bool,
    pub app_window: bool,
}

pub(crate) fn quiet_style(raw: isize) -> QuietStyle {
    if !owned(raw) {
        return QuietStyle::default();
    }
    // SAFETY: read-only query on a live window of this process.
    let style = unsafe { GetWindowLongPtrW(hwnd(raw), GWL_EXSTYLE) };
    QuietStyle {
        no_activate: style & WS_EX_NOACTIVATE as isize != 0,
        tool_window: style & WS_EX_TOOLWINDOW as isize != 0,
        app_window: style & WS_EX_APPWINDOW as isize != 0,
    }
}

/// The foreground window and whether this process owns it.
pub(crate) fn foreground() -> (isize, bool) {
    // SAFETY: read-only query.
    let fg = unsafe { GetForegroundWindow() } as isize;
    (fg, fg != 0 && owned(fg))
}

/// Minimises without activating anything (SW_SHOWMINNOACTIVE; winit's set_minimized uses SW_MINIMIZE, which
/// activates the next window).
pub(crate) fn minimize_quiet(raw: isize) -> Result<(), HostError> {
    if !owned(raw) {
        return Err(HostError::Window("foreign_window"));
    }
    // SAFETY: live window of this process.
    unsafe { ShowWindow(hwnd(raw), SW_SHOWMINNOACTIVE) };
    Ok(())
}

/// Restores without activating (SW_SHOWNOACTIVATE).
pub(crate) fn restore_quiet(raw: isize) -> Result<(), HostError> {
    if !owned(raw) {
        return Err(HostError::Window("foreign_window"));
    }
    // SAFETY: live window of this process.
    unsafe { ShowWindow(hwnd(raw), SW_SHOWNOACTIVATE) };
    Ok(())
}

pub(crate) fn minimized(raw: isize) -> bool {
    // SAFETY: read-only query.
    owned(raw) && unsafe { IsIconic(hwnd(raw)) } != 0
}

/// True when the process is DPI aware (winit makes it per-monitor v2 when the event loop is built; this call cannot
/// tell v2 from system awareness).
pub(crate) fn process_dpi_aware() -> bool {
    // SAFETY: read-only query.
    unsafe { IsProcessDPIAware() != 0 }
}
