//! Optional host-native OLE receiver. Paths stay outside portable arrangement/content state.
//! ABI: pinned windows 0.61.3 IDropTarget_Impl; Microsoft RegisterDragDrop/ReleaseStgMedium.
use std::{collections::VecDeque, path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}};
use windows::{core::{implement, Ref, PCWSTR, w}, Win32::{
    Foundation::{HWND, HANDLE, POINT, POINTL, RECT},
    Graphics::Gdi::ScreenToClient,
    System::{Com::{IDataObject, FORMATETC, DVASPECT_CONTENT, TYMED_HGLOBAL},
        Ole::{IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize, RegisterDragDrop,
            RevokeDragDrop, ReleaseStgMedium, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE},
        SystemServices::MODIFIERKEYS_FLAGS, Threading::{GetCurrentProcessId, GetCurrentThreadId}},
    UI::{Shell::{DragQueryFileW, HDROP}, WindowsAndMessaging::{GetClientRect, GetWindowThreadProcessId, IsWindow, GetPropW, SetPropW, RemovePropW}},
}};

/// Read-only rendezvous for the bounded proof sender. Only registration owns mutation.
pub const RECEIVER_LIFETIME_PROPERTY: PCWSTR = w!("Kumihimo.NativeDropReceiver.Lifetime.v1");
// Never recycle a token inside a process, including after an Arc allocation or HWND is reused.
static NEXT_RECEIVER_MARKER: AtomicUsize = AtomicUsize::new(1);

const MAX_EVENTS: usize = 32;
const MAX_FILES: u32 = 16;
const MAX_PATH: u32 = 32_767;

#[derive(Clone, Debug)]
pub enum NativeDropKind { Hover, Leave, Drop(Vec<PathBuf>), Rejected(&'static str) }
/// Coordinates are captured synchronously in the OLE callback, in client physical pixels.
#[derive(Clone, Debug)]
pub struct NativeDropEvent {
    pub hwnd: isize,
    pub epoch: u64,
    pub client_pixels: Option<[i32; 2]>,
    pub kind: NativeDropKind,
}
struct Shared {
    hwnd: isize,
    marker: usize,
    epoch: u64,
    queue: VecDeque<NativeDropEvent>,
    offered: bool,
    overflowed: bool,
    wake: Box<dyn Fn() + Send + Sync>,
}
fn format() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, dwAspect: DVASPECT_CONTENT.0,
        lindex: -1, tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() }
}
impl Shared {
    fn point(&self, pt: &POINTL) -> Option<[i32; 2]> {
        let hwnd = HWND(self.hwnd as *mut _);
        let mut point = POINT { x: pt.x, y: pt.y };
        let mut rect = RECT::default();
        let mut pid = 0;
        // Every callback validates the owned live window before coordinate conversion.
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() { return None; }
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != GetCurrentProcessId() || GetPropW(hwnd, RECEIVER_LIFETIME_PROPERTY).0 as usize != self.marker
                || !ScreenToClient(hwnd, &mut point).as_bool()
                || GetClientRect(hwnd, &mut rect).is_err() { return None; }
        }
        (point.x >= rect.left && point.y >= rect.top && point.x < rect.right && point.y < rect.bottom)
            .then_some([point.x, point.y])
    }
    fn event(&mut self, point: Option<[i32; 2]>, kind: NativeDropKind) -> bool {
        // Never evict a pending release to make room for pointer/hover traffic.
        if matches!(kind, NativeDropKind::Hover) {
            if let Some(last) = self.queue.back_mut() {
                if matches!(last.kind, NativeDropKind::Hover) {
                    last.client_pixels = point;
                    (self.wake)();
                    return true;
                }
            }
        }
        if self.queue.len() >= MAX_EVENTS { self.overflowed = true; (self.wake)(); return false; }
        self.queue.push_back(NativeDropEvent { hwnd: self.hwnd, epoch: self.epoch, client_pixels: point, kind });
        (self.wake)();
        true
    }
    fn effect(&self, effect: *mut DROPEFFECT, accepted: bool) {
        if !effect.is_null() {
            unsafe { *effect = if accepted && (*effect).0 & DROPEFFECT_COPY.0 != 0 {
                DROPEFFECT_COPY
            } else { DROPEFFECT_NONE }; }
        }
    }
}
#[implement(IDropTarget)]
struct Receiver { shared: Arc<Mutex<Shared>> }
#[allow(non_snake_case)]
impl IDropTarget_Impl for Receiver_Impl {
    fn DragEnter(&self, data: Ref<'_, IDataObject>, _: MODIFIERKEYS_FLAGS, pt: &POINTL,
        effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let mut s = self.shared.lock().map_err(|_| windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32)))?;
        s.offered = data.as_ref().is_some_and(|d| unsafe { d.QueryGetData(&format()).is_ok() });
        let point = s.point(pt);
        let accepted = s.offered && point.is_some() && s.event(point, NativeDropKind::Hover);
        s.effect(effect, accepted);
        Ok(())
    }
    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        if let Ok(mut s) = self.shared.lock() {
            let point = s.point(pt);
            let accepted = s.offered && point.is_some() && s.event(point, NativeDropKind::Hover);
            s.effect(effect, accepted);
        } else if !effect.is_null() { unsafe { *effect = DROPEFFECT_NONE; } }
        Ok(())
    }
    fn DragLeave(&self) -> windows::core::Result<()> {
        if let Ok(mut s) = self.shared.lock() { s.offered = false; s.event(None, NativeDropKind::Leave); }
        Ok(())
    }
    fn Drop(&self, data: Ref<'_, IDataObject>, _: MODIFIERKEYS_FLAGS, pt: &POINTL,
        effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        if let Ok(mut s) = self.shared.lock() {
            let point = s.point(pt);
            let incoming_copy = !effect.is_null() && unsafe { (*effect).0 & DROPEFFECT_COPY.0 != 0 };
            let paths = if s.offered && point.is_some() && incoming_copy {
                data.as_ref().and_then(|d| read_paths(d).ok())
            } else { None };
            let accepted = match paths {
                Some(paths) => s.event(point, NativeDropKind::Drop(paths)),
                None => { s.event(point, NativeDropKind::Rejected("unsupported_or_outside_drop")); false }
            };
            s.effect(effect, accepted);
            s.offered = false;
        } else if !effect.is_null() { unsafe { *effect = DROPEFFECT_NONE; } }
        Ok(())
    }
}
fn read_paths(data: &IDataObject) -> windows::core::Result<Vec<PathBuf>> {
    use std::os::windows::ffi::OsStringExt;
    let mut medium = unsafe { data.GetData(&format())? };
    let result = (|| {
        if medium.tymed != TYMED_HGLOBAL.0 as u32 { return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32))); }
        let drop = HDROP(unsafe { medium.u.hGlobal.0 });
        let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
        if count == 0 || count > MAX_FILES { return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32))); }
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let len = unsafe { DragQueryFileW(drop, index, None) };
            if len == 0 || len > MAX_PATH { return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32))); }
            let mut buffer = vec![0; len as usize + 1];
            if unsafe { DragQueryFileW(drop, index, Some(&mut buffer)) } != len {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32)));
            }
            paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len as usize])));
        }
        Ok(paths)
    })();
    // IDataObject owns the medium; DragFinish would use the wrong ownership protocol.
    unsafe { ReleaseStgMedium(&mut medium); }
    result
}

/// UI-thread-owned registration; unregister while its owned HWND is still alive.
pub(crate) struct Registration {
    hwnd: isize,
    marker: usize,
    _target: IDropTarget,
    shared: Arc<Mutex<Shared>>,
}
impl Registration {
    pub fn new(hwnd: isize, epoch: u64, wake: impl Fn() + Send + Sync + 'static) -> crate::Result<Self> {
        let mut pid = 0;
        let window = HWND(hwnd as *mut _);
        let thread = unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        if hwnd == 0 || epoch == 0 || pid != unsafe { GetCurrentProcessId() }
            || thread != unsafe { GetCurrentThreadId() } || !unsafe { IsWindow(Some(window)) }.as_bool()
            || !unsafe { GetPropW(window, RECEIVER_LIFETIME_PROPERTY) }.0.is_null() {
            return Err(crate::AdapterError::Invalid("drop receiver requires fresh owned UI-thread HWND"));
        }
        // Exhaustion rejects before OLE/property/registration effects. Reserved tokens stay retired.
        let marker = NEXT_RECEIVER_MARKER.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |next| next.checked_add(1)).map_err(|_| crate::AdapterError::Budget("drop receiver lifetime exhausted"))?;
        unsafe { OleInitialize(None) }.map_err(|e| crate::AdapterError::Host(e.to_string()))?;
        let shared = Arc::new(Mutex::new(Shared { hwnd, marker, epoch, queue: VecDeque::new(), offered: false, overflowed: false, wake: Box::new(wake) }));
        if let Err(e) = unsafe { SetPropW(window, RECEIVER_LIFETIME_PROPERTY, Some(HANDLE(marker as *mut _))) } {
            // Compensate only a marker matching this registration, never another owner.
            remove_owned_marker(window, marker);
            unsafe { OleUninitialize(); }
            return Err(crate::AdapterError::Host(e.to_string()));
        }
        let target: IDropTarget = Receiver { shared: shared.clone() }.into();
        if let Err(e) = unsafe { RegisterDragDrop(window, &target) } {
            remove_owned_marker(window, marker);
            unsafe { OleUninitialize(); }
            return Err(crate::AdapterError::Host(e.to_string()));
        }
        Ok(Self { hwnd, marker, _target: target, shared })
    }
    pub fn drain(&self) -> Vec<NativeDropEvent> {
        self.shared.lock().map(|mut s| {
            let mut events: Vec<_> = s.queue.drain(..).collect();
            if s.overflowed {
                s.overflowed = false;
                events.push(NativeDropEvent { hwnd: s.hwnd, epoch: s.epoch, client_pixels: None,
                    kind: NativeDropKind::Rejected("native_drop_queue_full") });
            }
            events
        }).unwrap_or_default()
    }
    pub fn set_epoch(&self, epoch: u64) {
        if let Ok(mut s) = self.shared.lock() { s.epoch = epoch; }
    }
}
impl std::ops::Drop for Registration {
    fn drop(&mut self) {
        let window = HWND(self.hwnd as *mut _);
        let mut pid = 0;
        unsafe {
            let thread = GetWindowThreadProcessId(window, Some(&mut pid));
            if pid == GetCurrentProcessId() && thread == GetCurrentThreadId()
                && GetPropW(window, RECEIVER_LIFETIME_PROPERTY).0 as usize == self.marker {
                let _ = RevokeDragDrop(window);
                remove_owned_marker(window, self.marker);
            }
            OleUninitialize();
        }
    }
}
fn remove_owned_marker(window: HWND, marker: usize) {
    unsafe {
        let mut pid = 0;
        let thread = GetWindowThreadProcessId(window, Some(&mut pid));
        if marker != 0 && pid == GetCurrentProcessId() && thread == GetCurrentThreadId()
            && GetPropW(window, RECEIVER_LIFETIME_PROPERTY).0 as usize == marker {
            let _ = RemovePropW(window, RECEIVER_LIFETIME_PROPERTY);
        }
    }
}
