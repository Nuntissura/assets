#![cfg(all(feature="native-apps",windows))]
//! Memory-only adapter proof. No HWND construction, launch, focus or input calls.
use kumihimo_egui::*;
use kumihimo_core::{Core,Identity,Limits,WorkspaceId,ViewId,ResourceId,FrameId,Rect,Command,Response};
use kumihimo_content::{Controller,ContentLimits};
fn surface()->SurfaceBinding {SurfaceBinding {root:WorkspaceId::from_raw(1),viewport:egui::ViewportId::ROOT,epoch:4,bounds:Rect::new(0.0,0.0,1000.0,700.0).unwrap(),pixels_per_point:1.5}}
fn slot()->RenderSlot {let bounds=Rect::new(20.0,48.0,400.0,300.0).unwrap();RenderSlot {view:ViewId::from_raw(2),resource:ResourceId::from_raw(3),frame:FrameId::from_raw(4),workspace:WorkspaceId::from_raw(1),canonical_bounds:Rect::new(20.0,20.0,400.0,328.0).unwrap(),bounds,clip:bounds,physical_size:[600,450],scale:1.5,epoch:4,focused:false,visible:true}}
#[test]
fn native_projection_preserves_toolbar_reservation_and_physical_scale() {
    let projected=external_apps::project_child_allocation(surface(),&slot()).unwrap();
    assert_eq!((projected.bounds.x,projected.bounds.y,projected.bounds.width,projected.bounds.height),(30,72,600,450));
    assert_eq!(projected.bounds,projected.clip);assert_eq!(projected.parent_epoch,4);
}
#[test]
fn stale_epoch_scale_partial_clip_and_invisible_slots_reject() {
    let mut slot=slot();slot.epoch=3;assert!(external_apps::project_child_allocation(surface(),&slot).is_err());slot.epoch=4;
    slot.scale=1.0;assert!(external_apps::project_child_allocation(surface(),&slot).is_err());slot.scale=1.5;
    slot.clip=Rect::new(20.0,48.0,390.0,300.0).unwrap();assert!(external_apps::project_child_allocation(surface(),&slot).is_err());slot.clip=slot.bounds;
    slot.clip=Rect::new(20.0,48.0,399.9,300.0).unwrap();assert!(external_apps::project_child_allocation(surface(),&slot).is_err());slot.clip=slot.bounds;
    slot.visible=false;assert!(external_apps::project_child_allocation(surface(),&slot).is_err());
}
#[test]
fn same_epoch_different_parent_is_never_an_eligible_native_route() {
    assert!(external_apps::native_parent_matches(10,10));assert!(!external_apps::native_parent_matches(10,20));assert!(!external_apps::native_parent_matches(0,0));
}
#[test]
fn actual_egui_popup_and_foreground_areas_block_native_presentation() {
    let context=egui::Context::default();
    let _=context.run(egui::RawInput{screen_rect:Some(to_egui(surface().bounds)),..Default::default()},|context|{
        egui::Popup::open_id(context,egui::Id::new("fixture-menu"));
        assert!(external_apps::native_child_overlays_blocked(context,&[slot()],false));
        assert!(external_apps::native_child_overlays_blocked(context,&[slot()],true));
    });
    let context=egui::Context::default();assert!(!external_apps::native_child_overlays_blocked(&context,&[slot()],false));
    let _=context.run(egui::RawInput{screen_rect:Some(to_egui(surface().bounds)),..Default::default()},|context|{
        egui::Area::new(egui::Id::new("fixture-foreground")).order(egui::Order::Foreground).fixed_pos(egui::pos2(20.0,48.0)).show(context,|ui|{ui.label("Overlay");});
        assert!(external_apps::native_child_overlays_blocked(context,&[slot()],false));
    });
}
struct OptIn;
impl DesktopHost for OptIn {
    fn dispatch(&mut self,_:&mut Controller,_:Command)->Result<Response>{Err(AdapterError::Unsupported("fixture dispatch"))}
    fn paint_content(&mut self,_:&mut egui::Ui,_:&RenderSlot,_:&mut Controller)->Result<()>{Ok(())}
}
impl native::NativeHost for OptIn {fn native_apps_enabled(&self)->bool{true}}
#[test]
fn legacy_run_rejects_opt_in_before_any_native_initialization() {
    let core=Core::new(Identity::new("native-app-test","fixture").unwrap(),Limits::default(),8).unwrap();
    let controller=Controller::new(core,"fixture",ContentLimits::default()).unwrap();
    let result=native::run(controller,OptIn,WorkspaceId::from_raw(1),native::NativeOptions::default());
    assert!(matches!(result,Err(AdapterError::Unsupported("native-app hosts require run_with_recovery before HWND effects"))));
}
