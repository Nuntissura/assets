//! Explicit visible sample for a supplied fresh owned application instance.
//! Never launches/closes apps, sends keyboard/mouse input, or activates them.
#[cfg(all(feature="native-apps",windows))]
mod sample {
    use kumihimo_egui::*;
    use kumihimo_egui::{native::NativeHost,external_apps::{NativeChildRoute,NativeParentBinding}};
    use kumihimo_content::Controller;
    use kumihimo_core::{Command,Response,Change,EntityId,ResourceId,ViewId,WorkspaceId};
    use kumihimo_external_apps::{ExternalApps,SelectedWindow,WindowTarget,windows::{WindowsBackend,WindowsLease},windows_hosting::WindowsHostingBackend,hosting::{HostingSession,HostingOperation}};
    struct Host { demo:demo::DemoHost,selected:Option<SelectedWindow<WindowsLease>>,route:Option<NativeChildRoute<WindowsHostingBackend>>,view:ViewId,resource:ResourceId,root:WorkspaceId,fallback:bool,status:String }
    impl DesktopHost for Host {
        fn policy(&self)->HostPolicy{self.demo.policy()}
        fn capabilities(&self)->Capabilities{self.demo.capabilities()}
        fn dispatch(&mut self,controller:&mut Controller,command:Command)->Result<Response>{self.demo.dispatch(controller,command)}
        fn paint_content(&mut self,ui:&mut egui::Ui,slot:&RenderSlot,controller:&mut Controller)->Result<()> {
            if slot.view==self.view {
                ui.label(&self.status);
                if self.fallback {ui.label("Application is on the desktop or has an unresolved return obligation.");}
                Ok(())
            }else{
                // Controls live in the sibling host pane, never beneath the native child.
                if let Some(route)=&mut self.route {
                    if ui.button("Detach application to desktop").clicked(){self.fallback=true;let receipt=route.session.request(HostingOperation::Detach);self.status=format!("{:?}; pending={}",receipt.phase,receipt.pending);}
                    if self.fallback {ui.label("Application keyboard focus, menus/dialogs and fullscreen use the desktop fallback.");}
                }
                self.demo.paint_content(ui,slot,controller)
            }
        }
        fn content_visibility(&mut self,controller:&mut Controller,surface:SurfaceBinding,visible:&[ViewId])->Result<()>{self.demo.content_visibility(controller,surface,visible)}
        fn content_input(&mut self,controller:&mut Controller,slot:&RenderSlot,input:kumihimo_content::Input)->Result<()>{if slot.view==self.view{Err(AdapterError::Unsupported("external application input is native-owned; cross-process focus is not admitted"))}else{self.demo.content_input(controller,slot,input)}}
        fn native_action(&mut self,controller:&mut Controller,action:AdapterAction)->Result<()>{self.demo.native_action(controller,action)}
    }
    impl NativeHost for Host {
        fn native_apps_enabled(&self)->bool{true}
        fn native_parent_ready(&mut self,_:&mut Controller,surface:SurfaceBinding,parent:NativeParentBinding)->Result<()> {
            if surface.root!=self.root{return Ok(());}
            if let Some(selected)=self.selected.take(){
                let result=parent.hosting_backend(selected).and_then(|backend|HostingSession::new(backend,"selected-app-host-example").map_err(|error|AdapterError::Host(error.to_string())));
                match result{Ok(session)=>{self.route=Some(NativeChildRoute::new(self.view,self.resource,session));self.status="Native preflight accepted; awaiting current content allocation".into();},Err(error)=>{self.fallback=true;self.status=format!("Hosting unavailable; application remains on desktop: {error:?}");}}
            }
            Ok(())
        }
        fn native_app_slots(&mut self,controller:&mut Controller,surface:SurfaceBinding,parent:&NativeParentBinding,report:&FrameReport)->Result<()> {
            if surface.root!=self.root{return Ok(());}
            if let Some(route)=&mut self.route{
                let receipt=if self.fallback{route.session.reconcile()}else{match route.sync(controller,parent,surface,report){Ok(receipt)=>receipt,Err(error)=>{self.fallback=true;self.status=format!("Native route denied; returning to desktop: {error:?}");route.session.request(HostingOperation::Detach)}}};
                self.status=format!("{:?}; pending={}; {}",receipt.phase,receipt.pending,receipt.diagnostic.as_ref().map_or("",|error|error.code.as_str()));
            }
            Ok(())
        }
        fn before_native_parent_destroy(&mut self,_:&mut Controller,surface:SurfaceBinding,_:&NativeParentBinding)->Result<bool>{if surface.root!=self.root{return Ok(true);}Ok(self.route.as_mut().is_none_or(|route|route.before_parent_destroy().parent_destroy_allowed))}
        fn native_app_overlay_ready(&mut self,_:&mut Controller,surface:SurfaceBinding,_:&NativeParentBinding)->Result<bool>{if surface.root!=self.root{return Ok(true);}self.route.as_mut().map_or(Ok(true),|route|route.overlay_ready())}
        fn retry_native_parent_detach(&mut self,_:&mut Controller,surface:SurfaceBinding,_:&NativeParentBinding)->Result<()>{if surface.root==self.root{if let Some(route)=&mut self.route{self.fallback=true;route.session.request(HostingOperation::Detach);}}Ok(())}
        fn native_host_snapshot(&self,_:&Controller)->serde_json::Value{serde_json::json!({"external_app":self.route.as_ref().map(|route|route.session.receipt()),"recovery":self.route.as_ref().map(|route|route.session.recovery()),"recovery_bank":kumihimo_external_apps::retention::RecoveryBank::current_thread().inspect(),"fallback":self.fallback,"status":self.status})}
    }
    fn prepare_demo()->std::result::Result<(demo::Demo,ViewId,ResourceId),Box<dyn std::error::Error>> {
        let mut demo=demo::with_counts(2,2)?;let root=demo.root;
        let Change::Created(EntityId::Provider(provider))=demo.host.dispatch(&mut demo.controller,Command::RegisterProvider{key:"external-app-host-example".into(),supports_shared_views:false})?.outcome.result? else{return Err("External provider identity was not created".into())};
        let Change::Created(EntityId::Resource(resource))=demo.host.dispatch(&mut demo.controller,Command::RegisterResource{provider,key:"selected-installed-app".into(),restore_reference:"external-app:selected-app-host-example".into()})?.outcome.result? else{return Err("External resource identity was not created".into())};
        let index=demo.controller.snapshot().workspaces[&root].frames.len();
        let Change::Created(EntityId::Frame(frame))=demo.host.dispatch(&mut demo.controller,Command::CreateFrame{workspace:root,index})?.outcome.result? else{return Err("External frame identity was not created".into())};
        let Change::Created(EntityId::View(view))=demo.host.dispatch(&mut demo.controller,Command::CreateView{frame,resource,index:0})?.outcome.result? else{return Err("External view identity was not created".into())};
        // Reuse a real mounted document and its resource; never alias it to the app.
        demo.host.dispatch(&mut demo.controller,Command::MoveView{view:demo.text_view,destination:frame,index:1})?.outcome.result?;
        demo.host.dispatch(&mut demo.controller,Command::SetViewMetadata{view:demo.text_view,label:"Host document (tab visibility)".into(),pinned:false,unsaved:false})?.outcome.result?;
        demo.host.dispatch(&mut demo.controller,Command::SetViewMetadata{view,label:"Selected application".into(),pinned:false,unsaved:false})?.outcome.result?;
        demo.host.dispatch(&mut demo.controller,Command::SelectView{view})?.outcome.result?;
        // Give the app the full client width; keep fallback controls in the lower panes.
        use kumihimo_core::{Axis,LayoutItem,LayoutNode,LayoutPlan,NodeId,Scalar,WeightedNode};
        let siblings:Vec<_>=demo.controller.snapshot().workspaces[&root].frames.iter().copied().filter(|id|*id!=frame).collect();
        let mut nodes=std::collections::BTreeMap::new();
        nodes.insert(NodeId(2),LayoutNode::Leaf(LayoutItem::Frame(frame)));
        let mut children=Vec::new();
        for (index,sibling) in siblings.into_iter().enumerate(){let node=NodeId(index as u64+4);nodes.insert(node,LayoutNode::Leaf(LayoutItem::Frame(sibling)));children.push(WeightedNode{node,weight:Scalar::new(1.0)?});}
        nodes.insert(NodeId(3),LayoutNode::Split{axis:Axis::Horizontal,children});
        nodes.insert(NodeId(1),LayoutNode::Split{axis:Axis::Vertical,children:vec![WeightedNode{node:NodeId(2),weight:Scalar::new(3.0)?},WeightedNode{node:NodeId(3),weight:Scalar::new(1.0)?}]});
        demo.host.dispatch(&mut demo.controller,Command::SetLayout{workspace:root,plan:LayoutPlan{root:Some(NodeId(1)),nodes}})?.outcome.result?;
        Ok((demo,view,resource))
    }
    pub fn run()->std::result::Result<(),Box<dyn std::error::Error>>{
        let mut args=std::env::args().skip(1);let mut pid=None;let mut hwnd=None;
        // Validate every option before installing a selected-window lifetime marker.
        let mut options=native::NativeOptions{probe:false,timeout:std::time::Duration::from_secs(120),..Default::default()};
        while let Some(arg)=args.next(){match arg.as_str(){
            "--interactive"=>options.interactive=true,
            "--pid"=>pid=Some(args.next().ok_or("Missing PID")?.parse::<u32>()?),
            "--hwnd"=>hwnd=Some(args.next().ok_or("Missing HWND")?.parse::<u64>()?),
            "--output"=>options.output=args.next().ok_or("Missing output path")?.into(),
            "--timeout-seconds"=>{let seconds=args.next().ok_or("Missing timeout seconds")?.parse::<u64>()?;if !(1..=120).contains(&seconds){return Err("Timeout seconds must be 1..=120".into());}options.timeout=std::time::Duration::from_secs(seconds);},
            _=>return Err(format!("Unknown argument: {arg}").into())
        }}
        if !options.interactive{return Err("Visible hosting requires explicit --interactive and an agreed controlled desktop; supply --pid PID --hwnd HWND for a fresh owned instance".into());}
        let target=WindowTarget{hwnd:hwnd.ok_or("Missing --hwnd")?,pid:pid.ok_or("Missing --pid")?,expected_creation_time:None};
        let (demo,view,resource)=prepare_demo()?;let root=demo.root;
        let mut apps=ExternalApps::new(WindowsBackend);let selected=apps.bind(target,"selected-app-host-example")?;
        let host=Host{demo:demo.host,selected:Some(selected),route:None,view,resource,root,fallback:false,status:"Awaiting owned native parent readiness".into()};
        match native::run_with_recovery(demo.controller,host,root,options)?{
            native::NativeRunResult::Completed(outcome)=>println!("{}",serde_json::json!({"event":"host_completed","manifest":outcome.manifest})),
            native::NativeRunResult::Retained(owner)=>{
                let id=owner.recovery_id();println!("{}",owner.inspect());drop(owner);
                // Exercise the same bounded, retrievable ownership path used after caller Drop.
                let mut owner=native::RetainedNativeOwner::<Host>::retrieve(id)?;
                eprintln!("Recovery retained. Commands: inspect, retry, finish. EOF retains/pumps the owned parent; it never exits with an attached app.");
                loop {
                    let mut command=String::new();let count=match std::io::stdin().read_line(&mut command){Ok(count)=>count,Err(error)=>{eprintln!("Recovery input unavailable: {error}; ownership remains retained");0}};
                    match command.trim(){"inspect"=>println!("{}",owner.inspect()),"retry"=>{match owner.retry_detach(){Ok(ready)=>println!("{}",serde_json::json!({"event":"retry_detach","ready":ready})),Err(error)=>eprintln!("Explicit retry denied: {error:?}")}},"finish"=>{
                        match owner.finish(){Ok(result)=>{let outcome=result?;println!("{}",serde_json::json!({"event":"recovery_completed","manifest":outcome.manifest}));return Ok(());},Err(retained)=>{owner=retained;println!("{}",owner.inspect());}}
                    },_=>{}}
                    if count==0 {
                        // No new detach attempts: pump/reconcile existing work and preserve custody.
                        if matches!(owner.reconcile(),Ok(true)) {match owner.finish(){Ok(result)=>{result?;return Ok(());},Err(retained)=>owner=retained}}
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
        }
        Ok(())
    }
    #[test]
    fn manual_host_initializes_with_a_distinct_document_tab_without_native_effects(){
        let (demo,view,resource)=prepare_demo().expect("manual sample initialization");
        let snapshot=demo.controller.snapshot();let frame=snapshot.views[&view].frame;
        assert_eq!(snapshot.workspaces[&demo.root].frames.len(),3);
        assert_eq!(snapshot.views[&demo.text_view].frame,frame);
        assert_ne!(snapshot.views[&demo.text_view].resource,resource);
        assert_eq!(snapshot.frames[&frame].views.len(),2);
        assert_eq!(snapshot.frames[&frame].selected_view,Some(view));
        let geometry=snapshot.geometry(demo.root).expect("manual sample geometry");
        assert_eq!(geometry.frames[&frame].width(),geometry.viewport.width());
        assert!(geometry.frame_content[&frame].width()>=488.0);
        assert!(geometry.frame_content[&frame].height()>=138.0);
    }
}
fn main(){
    #[cfg(all(feature="native-apps",windows))]
    if let Err(error)=sample::run(){eprintln!("{error}");std::process::exit(1);}
    #[cfg(not(all(feature="native-apps",windows)))]
    eprintln!("foreign_app_host requires Windows, native-apps and explicit --interactive");
}
