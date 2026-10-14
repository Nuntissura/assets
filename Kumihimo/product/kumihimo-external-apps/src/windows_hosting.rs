//! One owned mutation worker per selected app. A stalled Win32 call keeps its
//! leases/parent keepalive and unresolved operation; it is never killed or joined on UI.
use crate::{hosting::*, windows::{DpiGuard, WindowsBackend, WindowsLease, os}, *};
use std::{any::Any, mem::size_of, ptr::null_mut, sync::{Arc, atomic::{AtomicU64, Ordering}, mpsc::{sync_channel, Receiver, TryRecvError}}};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, System::Threading::*, UI::{HiDpi::*, WindowsAndMessaging::*}};

pub struct WindowsHostingBackend {
    app: Arc<WindowsLease>, parent: Arc<WindowsLease>, epoch: Arc<AtomicU64>,
    // Actual owned GUI window is retained even while the caller's runner is recovering.
    keepalive: Arc<dyn Any + Send + Sync>, wake: Arc<dyn Fn() + Send + Sync>,
    original: NativeWindowSnapshot, child_style: u32, child_ex_style: u32,
    worker: Option<(Receiver<Result<(), Diagnostic>>, std::thread::JoinHandle<()>)>,
}
impl WindowsHostingBackend {
    pub fn new(
        selected: SelectedWindow<WindowsLease>, parent: WindowsLease,
        epoch: Arc<AtomicU64>, keepalive: Arc<dyn Any + Send + Sync>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, Diagnostic> {
        if selected.pending().is_some() { return Err(Diagnostic::new("tiling_pending", "Selected window has an unresolved desktop-positioning request")); }
        let app = Arc::new(selected.lease); let parent = Arc::new(parent);
        app.validate()?; parent.validate()?;
        if parent.thread_id()!=unsafe {GetCurrentThreadId()} {
            return Err(Diagnostic::new("parent_origin_thread", "Hosting must be admitted on the owned parent's originating UI thread and recovery bank"));
        }
        if app.identity().pid==parent.identity().pid || app.hwnd()==parent.hwnd() {
            return Err(Diagnostic::new("hosting_not_foreign", "Native child must be a distinct external process/window, never the host parent itself"));
        }
        let original = snapshot(&app)?;
        Ok(Self { app, parent, epoch, keepalive, wake, original,
            child_style:(original.style & !(WS_POPUP | WS_CAPTION | WS_THICKFRAME)) | WS_CHILD,
            child_ex_style:original.ex_style, worker:None })
    }
    pub fn app_identity(&self) -> WindowIdentity { self.app.identity() }
    pub fn parent_identity(&self) -> WindowIdentity { self.parent.identity() }
}
fn check(app: &WindowsLease, parent: &WindowsLease, epoch: &AtomicU64, expected: u64) -> Result<(), Diagnostic> {
    app.validate()?; parent.validate()?;
    if expected == 0 || epoch.load(Ordering::Acquire) != expected { return Err(Diagnostic::new("parent_epoch_stale", "Native parent epoch changed before this effect")); }
    Ok(())
}
fn snapshot(app: &WindowsLease) -> Result<NativeWindowSnapshot, Diagnostic> {
    app.validate()?; let _dpi = DpiGuard::enter()?;
    let hwnd = app.hwnd(); let parent = unsafe { GetParent(hwnd) };
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 { return Err(os("hosting_bounds", "Cannot independently read application window bounds")); }
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    if style & WS_CHILD != 0 && !parent.is_null() {
        let mut origin = POINT { x:rect.left,y:rect.top }; let mut end = POINT { x:rect.right,y:rect.bottom };
        if unsafe { ScreenToClient(parent, &mut origin) } == 0 || unsafe { ScreenToClient(parent, &mut end) } == 0 { return Err(os("hosting_client_coordinates", "Cannot observe native child parent-client geometry")); }
        rect = RECT { left:origin.x,top:origin.y,right:end.x,bottom:end.y };
    }
    let bounds = PixelBounds { x:rect.left,y:rect.top,width:rect.right.checked_sub(rect.left).ok_or_else(||Diagnostic::new("hosting_bounds_overflow", "Width overflow"))?,height:rect.bottom.checked_sub(rect.top).ok_or_else(||Diagnostic::new("hosting_bounds_overflow", "Height overflow"))? }; bounds.validate()?;
    let state = if unsafe { IsIconic(hwnd) } != 0 { WindowState::Minimized } else if unsafe { IsZoomed(hwnd) } != 0 { WindowState::Maximized } else { WindowState::Normal };
    app.validate()?;
    Ok(NativeWindowSnapshot { parent:parent as usize as u64,owner:unsafe { GetWindow(hwnd, GW_OWNER) } as usize as u64,style,ex_style:unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32,placement:Placement { bounds,state },visible:unsafe { IsWindowVisible(hwnd) } != 0 })
}
fn style(hwnd: HWND, index: i32, value: u32) -> Result<(), Diagnostic> {
    unsafe { SetLastError(0); }
    let prior = unsafe { SetWindowLongPtrW(hwnd, index, value as isize) };
    if prior == 0 && unsafe { GetLastError() } != 0 { Err(os("hosting_style", "Native style change failed")) } else { Ok(()) }
}
fn parent(hwnd: HWND, target: HWND) -> Result<(), Diagnostic> {
    unsafe { SetLastError(0); }
    let prior = unsafe { SetParent(hwnd, target) };
    if prior.is_null() && unsafe { GetLastError() } != 0 { Err(os("hosting_parent", "Native parent change failed")) } else { Ok(()) }
}
fn position(hwnd: HWND, bounds: PixelBounds, visibility: Option<bool>, frame_changed: bool) -> Result<(), Diagnostic> {
    bounds.validate()?;
    let mut flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER;
    if frame_changed { flags |= SWP_FRAMECHANGED; }
    if let Some(visible) = visibility { flags |= if visible { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW }; }
    // Deliberately synchronous on the owned worker: worker completion is an ordering witness.
    if unsafe { SetWindowPos(hwnd, null_mut(), bounds.x,bounds.y,bounds.width,bounds.height, flags) } == 0 { Err(os("hosting_position", "Native positioning failed, partial state requires independent observation")) } else { Ok(()) }
}
fn execute(app: &WindowsLease, parent_lease: &WindowsLease, epoch: &AtomicU64, expected: u64, original: NativeWindowSnapshot, child_style: u32, child_ex_style: u32, operation: HostingOperation) -> Result<(), Diagnostic> {
    let _dpi = DpiGuard::enter()?;
    let check_now = || check(app,parent_lease,epoch,expected);
    check_now()?;
    match operation {
        HostingOperation::Attach(allocation) => {
            style(app.hwnd(),GWL_STYLE,child_style)?; check_now()?;
            style(app.hwnd(),GWL_EXSTYLE,child_ex_style)?; check_now()?;
            parent(app.hwnd(),parent_lease.hwnd())?; check_now()?;
            position(app.hwnd(),allocation.bounds,Some(true),true)?;
        },
        HostingOperation::Resize(allocation) => { position(app.hwnd(),allocation.bounds,None,false)?; },
        HostingOperation::Visibility(visible) => { let current=snapshot(app)?; check_now()?; position(app.hwnd(),current.placement.bounds,Some(visible),false)?; },
        HostingOperation::Detach => {
            // This path is the only supported durable fallback: original parent/owner are desktop/none.
            parent(app.hwnd(),null_mut())?; check_now()?;
            style(app.hwnd(),GWL_STYLE,original.style)?; check_now()?;
            style(app.hwnd(),GWL_EXSTYLE,original.ex_style)?; check_now()?;
            position(app.hwnd(),original.placement.bounds,Some(original.visible),true)?;
        },
        HostingOperation::Focus { .. } => return Err(Diagnostic::new("focus_unsupported", "Cross-process focus routing is not admitted; use detached application focus until controlled input support is proven")),
    }
    check_now()
}
impl HostingBackend for WindowsHostingBackend {
    fn preflight(&mut self) -> Result<HostingPreflight, Diagnostic> {
        self.validate(self.current_parent_epoch())?;
        if self.original.parent != 0 || self.original.owner != 0 || self.original.placement.state != WindowState::Normal || !self.original.visible || self.original.style & WS_CHILD != 0 {
            return Err(Diagnostic::new("hosting_window_mode", "Requires an unowned visible normal desktop application window"));
        }
        let app_dpi=unsafe { GetWindowDpiAwarenessContext(self.app.hwnd()) }; let parent_dpi=unsafe { GetWindowDpiAwarenessContext(self.parent.hwnd()) };
        if app_dpi.is_null() || parent_dpi.is_null() || unsafe { AreDpiAwarenessContextsEqual(app_dpi,parent_dpi) } == 0 {
            return Err(Diagnostic::new("hosting_dpi_mismatch", "Exact application/parent DPI contexts differ; reparenting would risk a process DPI reset"));
        }
        if self.original.ex_style & (WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT) != 0 || self.original.style & WS_CAPTION != WS_CAPTION || !unsafe { GetMenu(self.app.hwnd()) }.is_null() {
            return Err(Diagnostic::new("hosting_stacking_or_menu", "Topmost/layered/transparent/fullscreen/native-menu windows are unsupported; desktop fallback remains available"));
        }
        let caps=WindowsBackend.capabilities(&self.app)?;
        if !caps.can_resize { return Err(Diagnostic::new("hosting_not_resizable", "Selected application does not expose resize support")); }
        Ok(HostingPreflight { original:self.original,parent:self.parent.identity().hwnd,parent_epoch:self.current_parent_epoch(),child_style:self.child_style,child_ex_style:self.child_ex_style,minimum_width:caps.minimum_width,minimum_height:caps.minimum_height,
            limitations:vec!["Fully clipped, unobscured rectangular slots only; native children draw above parent GPU content".into(), "Cross-process focus/keyboard routing, application menus/dialogs, secondary windows and fullscreen are unsupported; detach to desktop for those routes".into(), "Tab hiding changes presentation only; no application suspension or close".into()] })
    }
    fn validate(&mut self, parent_epoch: u64) -> Result<(), Diagnostic> { check(&self.app,&self.parent,&self.epoch,parent_epoch) }
    fn observe(&mut self) -> Result<NativeWindowSnapshot, Diagnostic> { self.parent.validate()?; snapshot(&self.app) }
    fn current_parent_epoch(&self) -> u64 { self.epoch.load(Ordering::Acquire) }
    fn admission_key(&self)->String { let identity=self.app.identity();format!("native-app:{}:{}:{}",identity.hwnd,identity.pid,identity.creation_time) }
    fn dispatch(&mut self, operation: HostingOperation) -> Result<(), Diagnostic> {
        if self.worker.is_some() { return Err(Diagnostic::new("hosting_worker_pending", "One exact operation worker is already held")); }
        let expected=match operation { HostingOperation::Attach(allocation)|HostingOperation::Resize(allocation)=>allocation.parent_epoch,_=>self.current_parent_epoch() };
        self.validate(expected)?;
        if matches!(operation,HostingOperation::Attach(_))&&unsafe {IsWindowVisible(self.parent.hwnd())}==0 {
            return Err(Diagnostic::new("hosting_parent_hidden", "Attachment requires a presented owned parent; hidden automation retains the desktop fallback"));
        }
        if let HostingOperation::Attach(allocation)|HostingOperation::Resize(allocation)=operation {
            allocation.validate()?;
            let mut client:RECT=unsafe { std::mem::zeroed() }; let _dpi=DpiGuard::enter()?;
            if unsafe { GetClientRect(self.parent.hwnd(),&mut client) } == 0 { return Err(os("parent_client_bounds", "Cannot inspect owned parent's physical client bounds")); }
            if !(PixelBounds { x:0,y:0,width:client.right-client.left,height:client.bottom-client.top }).contains(allocation.bounds) { return Err(Diagnostic::new("outside_parent_client", "Native child allocation exceeds current parent client bounds")); }
        }
        if matches!(operation,HostingOperation::Focus { .. }) { return Err(Diagnostic::new("focus_unsupported", "Focus is explicitly unavailable; detach for ordinary application input")); }
        let (sender,receiver)=sync_channel(1);
        let app=self.app.clone(); let parent_lease=self.parent.clone(); let epoch=self.epoch.clone(); let keepalive=self.keepalive.clone(); let wake=self.wake.clone();
        let original=self.original; let child_style=self.child_style; let child_ex_style=self.child_ex_style;
        let worker=std::thread::Builder::new().name("kumihimo-native-child".into()).spawn(move|| {
            let _keepalive=keepalive;
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||execute(&app,&parent_lease,&epoch,expected,original,child_style,child_ex_style,operation)))
                .unwrap_or_else(|_|Err(Diagnostic::new("hosting_worker_panic", "Owned worker panicked; independently reconcile native state")));
            let _=sender.send(result); wake();
        }).map_err(|error|Diagnostic::new("hosting_worker_spawn", &error.to_string()))?;
        self.worker=Some((receiver,worker)); Ok(())
    }
    fn completion(&mut self) -> Result<Option<Result<(), Diagnostic>>, Diagnostic> {
        let Some((receiver,_))=&self.worker else { return Err(Diagnostic::new("hosting_no_worker", "No dispatched worker is held")); };
        match receiver.try_recv() {
            Ok(result)=>{ self.worker.take(); Ok(Some(result)) },
            Err(TryRecvError::Empty)=>Ok(None),
            Err(TryRecvError::Disconnected)=>Err(Diagnostic::new("hosting_worker_disconnected", "Worker completion channel ended without a settlement result")),
        }
    }
}
