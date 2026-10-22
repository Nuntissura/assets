//! A separate native-child route. GPU native_child rejection remains unchanged.
//! Only the owned native runner mints parents; no native handle enters Core/content.
use crate::*;
use kumihimo_external_apps::{hosting::*, windows::{WindowsBackend,WindowsLease}, windows_hosting::WindowsHostingBackend, SelectedWindow, WindowTarget, PixelBounds};
use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
use winit::window::Window;

#[derive(Clone)]
pub struct NativeParentBinding {
    window:Arc<Window>, lease:Arc<WindowsLease>, epoch:Arc<AtomicU64>,
    root:WorkspaceId, viewport:egui::ViewportId, wake:Arc<dyn Fn()+Send+Sync>,
}
impl NativeParentBinding {
    pub(crate) fn new(window:Arc<Window>, hwnd:u64, surface:SurfaceBinding, wake:Arc<dyn Fn()+Send+Sync>)->Result<Self> {
        surface.validate()?;
        let mut backend=WindowsBackend;
        let lease=backend.bind_owned_parent(WindowTarget { hwnd,pid:std::process::id(),expected_creation_time:None }).map_err(external_error)?;
        let binding=Self { window,lease:Arc::new(lease),epoch:Arc::new(AtomicU64::new(surface.epoch)),root:surface.root,viewport:surface.viewport,wake };
        binding.validate(surface)?;Ok(binding)
    }
    pub(crate) fn set_epoch(&self,epoch:u64) { self.epoch.store(epoch,Ordering::Release); }
    pub fn epoch(&self)->u64 { self.epoch.load(Ordering::Acquire) }
    pub fn identity(&self)->kumihimo_external_apps::WindowIdentity { self.lease.identity() }
    pub fn validate(&self,surface:SurfaceBinding)->Result<()> {
        self.lease.validate().map_err(external_error)?;
        if self.root != surface.root || self.viewport != surface.viewport || self.epoch() != surface.epoch || surface.epoch == 0 { return Err(AdapterError::Stale("native parent root/viewport/epoch")); }
        let size=self.window.inner_size();
        if size.width == 0 || size.height == 0 || surface.physical_size()? != [size.width,size.height] { return Err(AdapterError::Stale("native parent physical client extent")); }
        Ok(())
    }
    /// Backends retain the actual owned Window until observed detach permits release.
    pub fn hosting_backend(&self,selected:SelectedWindow<WindowsLease>)->Result<WindowsHostingBackend> {
        self.lease.validate().map_err(external_error)?;
        let mut backend=WindowsBackend;
        let identity=self.lease.identity();
        let parent=backend.bind_owned_parent(WindowTarget { hwnd:identity.hwnd,pid:identity.pid,expected_creation_time:Some(identity.creation_time) }).map_err(external_error)?;
        WindowsHostingBackend::new(selected,parent,self.epoch.clone(),self.window.clone(),self.wake.clone()).map_err(external_error)
    }
    /// Parent-client physical pixels after toolbar/navigation reservation. Full clips only.
    pub fn allocation(&self,surface:SurfaceBinding,slot:&RenderSlot)->Result<ChildAllocation> {
        self.validate(surface)?;
        project_child_allocation(surface,slot)
    }
}
/// Pure coordinate/precondition projection; fixture tests create no native window.
pub fn project_child_allocation(surface:SurfaceBinding,slot:&RenderSlot)->Result<ChildAllocation> {
        surface.validate()?;
        if slot.epoch != surface.epoch || slot.scale != surface.pixels_per_point || !slot.visible { return Err(AdapterError::Stale("native child slot epoch/visibility/scale")); }
        if slot.bounds!=slot.clip {return Err(AdapterError::Unsupported("native child requires an exact full DIP clip"));}
        if !surface.bounds.contains(slot.bounds) {return Err(AdapterError::Unsupported("native child allocation outside full owned client bounds"));}
        let physical=|rect:Rect|->Result<PixelBounds> {
            let convert=|value:f64|->Result<i32> { let rounded=value.round(); if !rounded.is_finite() || rounded < i32::MIN as f64 || rounded > i32::MAX as f64 { Err(AdapterError::Invalid("native child coordinate overflow")) } else { Ok(rounded as i32) } };
            let scale=surface.pixels_per_point as f64;
            let x=convert((rect.x()-surface.bounds.x())*scale)?;let y=convert((rect.y()-surface.bounds.y())*scale)?;
            let right=convert((rect.right()-surface.bounds.x())*scale)?;let bottom=convert((rect.bottom()-surface.bounds.y())*scale)?;
            Ok(PixelBounds { x,y,width:right.checked_sub(x).ok_or(AdapterError::Invalid("native child width"))?,height:bottom.checked_sub(y).ok_or(AdapterError::Invalid("native child height"))? })
        };
        let allocation=ChildAllocation { bounds:physical(slot.bounds)?,clip:physical(slot.clip)?,parent_epoch:surface.epoch };
        allocation.validate().map_err(external_error)?;Ok(allocation)
    }
fn external_error(error:kumihimo_external_apps::Diagnostic)->AdapterError { AdapterError::Host(format!("{}: {} ({:?})",error.code,error.message,error.os_error)) }
pub fn native_parent_matches(bound:u64,supplied:u64)->bool {bound!=0&&bound==supplied}
/// Actual pinned egui Popup/memory Areas state, not inference from Core float records.
pub fn native_child_overlays_blocked(ctx:&egui::Context,slots:&[RenderSlot],active_preview:bool)->bool {
    if active_preview || egui::Popup::is_any_open(ctx){return true;}
    let Some(layers)=ctx.memory(|memory|{
        let layers=memory.layer_ids();if layers.len()>128{return None;}
        Some(layers.filter(|layer|matches!(layer.order,egui::Order::Foreground|egui::Order::Tooltip)&&memory.areas().is_visible(layer)).collect::<Vec<_>>())
    }) else{return true;};
    layers.into_iter().any(|layer|{
            let Some(area)=egui::AreaState::load(ctx,layer.id) else{return true;};
            if area.size.is_none(){return true;}
            slots.iter().any(|slot|area.rect().intersect(to_egui(slot.bounds)).is_positive())
        })
}

/// Host-owned route for one canonical view/resource; no independent arrangement.
pub struct NativeChildRoute<B:HostingBackend+'static> {
    pub view:ViewId, pub resource:ResourceId, pub session:HostingSession<B>,
    last_allocation:Option<ChildAllocation>, visible:bool, pending_intent:Option<HostingOperation>, fallback:bool,
}
impl<B:HostingBackend+'static> NativeChildRoute<B> {
    pub fn new(view:ViewId,resource:ResourceId,session:HostingSession<B>)->Self { Self { view,resource,session,last_allocation:None,visible:false,pending_intent:None,fallback:false } }
    pub fn sync(&mut self,controller:&Controller,parent:&NativeParentBinding,surface:SurfaceBinding,report:&FrameReport)->Result<HostingReceipt> {
        let receipt=self.session.reconcile();
        if receipt.pending { return Ok(receipt); }
        if self.fallback {return Ok(receipt);}
        if !native_parent_matches(self.session.parent_handle(),parent.identity().hwnd) {
            self.fallback=true;
            let mut receipt=self.session.request(HostingOperation::Detach);
            receipt.diagnostic=Some(kumihimo_external_apps::Diagnostic::new("native_parent_migration_unsupported", "Supplied native root differs from the bound parent; return to desktop before an explicit new binding"));
            return Ok(receipt);
        }
        if let Some(operation)=self.pending_intent.take() {
            if receipt.phase == HostingPhase::Attached {
                match operation { HostingOperation::Attach(allocation)|HostingOperation::Resize(allocation)=>{self.last_allocation=Some(allocation);self.visible=true;},HostingOperation::Visibility(visible)=>self.visible=visible,_=>{} }
            }
        }
        parent.validate(surface)?;
        if report.epoch != surface.epoch || report.post_generation != controller.snapshot().generation { return Err(AdapterError::Stale("native child report generation/epoch")); }
        let record=controller.snapshot().views.get(&self.view).ok_or(AdapterError::Stale("native child view missing"))?;
        if record.resource != self.resource || record.closed.is_some() { return Err(AdapterError::Stale("native child canonical resource/view")); }
        // Floating content/overlays are not admitted by this first native stacking route.
        let floats=!controller.snapshot().floats.is_empty();
        if floats { return Err(AdapterError::Unsupported("native child surfaces with floating overlays")); }
        let slot=report.slots.iter().find(|slot|!report.native_child_overlay_blocked&&slot.view==self.view&&slot.resource==self.resource&&slot.frame==record.frame&&slot.visible);
        let operation=if let Some(slot)=slot {
            let allocation=parent.allocation(surface,slot)?;
            match self.session.phase() {
                HostingPhase::Preflight|HostingPhase::Detached=>Some(HostingOperation::Attach(allocation)),
                HostingPhase::Attached if !self.visible=>Some(HostingOperation::Visibility(true)),
                HostingPhase::Attached if self.last_allocation != Some(allocation)=>Some(HostingOperation::Resize(allocation)),
                _=>None,
            }
        } else if self.session.phase()==HostingPhase::Attached&&self.visible { Some(HostingOperation::Visibility(false)) } else { None };
        if let Some(operation)=operation { self.pending_intent=Some(operation);Ok(self.session.request(operation)) } else { Ok(self.session.receipt()) }
    }
    pub fn before_parent_destroy(&mut self)->HostingReceipt { self.session.before_parent_destroy() }
    pub fn overlay_ready(&mut self)->Result<bool> {self.session.presentation_clear_for_overlay().map_err(external_error)}
    /// No inference from RenderSlot.focused; unsupported native focus returns fallback.
    pub fn explicit_focus(&mut self)->HostingReceipt { self.session.request(HostingOperation::Focus { explicit_user_action:true }) }
}
