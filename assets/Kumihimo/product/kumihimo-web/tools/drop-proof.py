"""Bounded WP018 DOM File-to-host proof using configured Node/Playwright/Chrome."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from urllib.parse import urlsplit


SCRIPT = r'''
const fs=require('fs'),path=require('path'),crypto=require('crypto');
const config=JSON.parse(fs.readFileSync(0,'utf8'));
const {chromium}=require(path.join(config.modules,'playwright'));
const assert=(condition,message)=>{if(!condition)throw new Error(message);};
const hash=bytes=>crypto.createHash('sha256').update(bytes).digest('hex');
const evidence={version:1,scope:'WP018 real browser File/DataTransfer -> production DOM listeners -> Rust admission/host content; trusted headless browser pointer/title/control gestures; disk File drops use synthetic untrusted DragEvent, not physical OS browser file dragging',limitations:['Physical OS/browser drag input is NOT_PROVEN','Protected dragover mode is injected accessor-denial simulation','Delayed file read is injected; actual File bytes remain browser-owned'],cases:[],screenshots:[],errors:[],bindings:[]};
let context,page,deadline;
const canonical=()=>page.evaluate(()=>({rust:window.kumihimo.snapshot(),host:window.kumihimo.hostSnapshot()}));
const idle=()=>page.evaluate(async()=>{await window.kumihimo.adapter.whenIdle();});
const taskDigest=s=>JSON.stringify(s.host.tasks.map(t=>({id:t.id,body:t.body,title:t.title,links:t.links,veto:t.veto})));
const arrangement=s=>JSON.stringify({generation:s.rust.generation,identity:s.rust.identity,frames:s.rust.frames,layouts:s.rust.layouts});
async function capture(name){assert(evidence.screenshots.length<4,'four screenshot bound');const filename=path.join(config.out,name+'.png');await page.screenshot({path:filename,fullPage:false});const bytes=fs.readFileSync(filename);assert(bytes.length<=2*1024*1024,'screenshot byte bound');evidence.screenshots.push({path:filename,sha256:hash(bytes)});}
async function targets(){
  const s=await canonical();
  const records=s.rust.frames.filter(f=>f.visible&&f.expanded&&!f.child_workspace&&f.selected_view).map(f=>({frame:f,view:s.rust.views.find(v=>v.id===f.selected_view)}));
  const a=records[0],b=records.find(r=>r.view.resource!==a?.view.resource);
  assert(a&&b&&a.frame.depth>=1&&b.frame.depth>=1,'two visible nested targets with distinct resources required');
  return [a,b].map(r=>({frame:r.frame.id,view:r.view.id,resource:r.view.resource,provider:r.view.provider}));
}
async function transfer(target,name,text,{action='import',delay=false,internal=null}={}){
  return page.evaluate(({target,name,text,action,delay,internal})=>{
    const bridge=window.kumihimo,a=bridge.adapter;
    const state=bridge.snapshot(),frame=state.frames.find(f=>f.id===target.frame);
    const node=document.querySelector(`.frame[data-frame="${target.frame}"] .frame-content`);
    if(!node||node.hidden||!frame?.content)throw new Error('target DOM content unavailable');
    const rect=node.getBoundingClientRect();
    const x=Math.max(rect.x,frame.clip.x)+Math.min(20,Math.min(rect.width,frame.clip.width)/2),y=Math.max(rect.y,frame.clip.y)+Math.min(20,Math.min(rect.height,frame.clip.height)/2);
    if(x<0||y<0||x>=innerWidth||y>=innerHeight)throw new Error('target point outside inspected browser viewport');
    const transfer=new DataTransfer();
    if(internal)transfer.setData('application/x-kumihimo-asset',JSON.stringify(internal));
    else transfer.items.add(new File([text],name,{type:name.endsWith('.bin')?'application/octet-stream':'text/plain'}));
    const file=internal?null:transfer.files[0];
    const guard={file_reads:0,data_reads:0};
    Object.defineProperty(transfer,'files',{configurable:true,get(){guard.file_reads++;throw new Error('protected hover file access');}});
    const getData=transfer.getData.bind(transfer);
    Object.defineProperty(transfer,'getData',{configurable:true,value(){guard.data_reads++;throw new Error('protected hover payload access');}});
    const modifiers={ctrlKey:action==='copy',shiftKey:action==='open'};
    const hover=new DragEvent('dragover',{bubbles:true,cancelable:true,dataTransfer:transfer,clientX:x,clientY:y,...modifiers});
    node.dispatchEvent(hover);
    const preview=a.dropPreview?{frame:a.dropPreview.destination.frame,view:a.dropPreview.destination.view,action:a.dropPreview.action,source:a.dropPreview.source}:null;
    const hoverLabel=node.getAttribute('data-file-drop');
    delete transfer.files;delete transfer.getData;
    let original;
    if(delay){
      original=File.prototype.arrayBuffer;
      File.prototype.arrayBuffer=function(){if(this===file){bridge.proofDeferred={read_started:true,file,resolve:null};return new Promise(resolve=>{bridge.proofDeferred.resolve=()=>original.call(file).then(resolve);});}return original.call(this);};
    }
    const drop=new DragEvent('drop',{bubbles:true,cancelable:true,dataTransfer:transfer,clientX:x,clientY:y,...modifiers});
    try{node.dispatchEvent(drop);}finally{if(original)File.prototype.arrayBuffer=original;}
    return {source:internal?'internal_assets':'browser_files',file:internal?null:{name:file.name,size:file.size,type:file.type,actual_file:file instanceof File},transfer_actual:transfer instanceof DataTransfer,guard,hover:{trusted:hover.isTrusted,prevented:hover.defaultPrevented,label:hoverLabel,preview,effect:transfer.dropEffect},drop:{trusted:drop.isTrusted,prevented:drop.defaultPrevented},target,point:{x,y}};
  },{target,name,text,action,delay,internal});
}
async function settled(){await page.waitForFunction(()=>window.kumihimo.adapter.pendingDrops.size===0);await idle();return canonical();}
async function pointerPreviewCancelled(selector,kind,target,{blank=false}={}){
  const before=await canonical(),box=await page.locator(selector).boundingBox();assert(box,'pointer source unavailable');
  const sourceFrame=await page.locator(selector).evaluate(el=>el.closest('[data-frame]')?.dataset.frame??null);
  const eventMark=await page.evaluate(()=>window.kumihimo.adapter.eventSequence);
  const start={x:blank?box.x+1:box.x+box.width/2,y:blank?box.y+1:box.y+box.height/2};
  const frame=before.rust.frames.find(f=>f.id===target.frame),destination={x:Math.max(frame.content.x,frame.clip.x)+12,y:Math.max(frame.content.y,frame.clip.y)+25};
  await page.mouse.move(start.x,start.y);await page.mouse.down();
  if(kind==='resize-split')await page.mouse.move(start.x+12,start.y+12,{steps:3});
  else await page.mouse.move(destination.x,destination.y,{steps:4});
  await page.waitForFunction(kind=>{const a=window.kumihimo.adapter;return a.drag?.kind===kind&&a.previewState?.token&&a.previewState.geometry?.frames?.length&&document.querySelector('#workspace').hasPointerCapture(a.drag.pointerId);},kind);
  const pending=await page.evaluate(mark=>{const a=window.kumihimo.adapter;return {drag:{kind:a.drag.kind,frame:a.drag.frame,pointer_id:a.drag.pointerId},preview:a.previewState,captured:document.querySelector('#workspace').hasPointerCapture(a.drag.pointerId),events:a.events.filter(e=>e.sequence>mark)};},eventMark);
  assert(pending.events.some(e=>e.type==='event'&&e.source==='pointerdown'&&e.action===kind&&e.trusted),'trusted browser pointerdown absent');
  if(kind==='drag-frame')assert(pending.drag.frame===sourceFrame,'pointer captured a different canonical frame');
  await page.keyboard.press('Escape');await page.mouse.up();await idle();const after=await canonical();
  const cleared=await page.evaluate(()=>({drag:window.kumihimo.adapter.drag,preview:window.kumihimo.adapter.previewState}));
  assert(!cleared.drag&&!cleared.preview,'Escape retained pointer drag/preview');
  assert(arrangement(before)===arrangement(after)&&taskDigest(before)===taskDigest(after)&&before.host.revision===after.host.revision,'cancelled arrangement gesture mutated canonical/host state');
  return {provenance:'trusted Chromium-generated pointer and Escape in private headless page; no desktop cursor',selector,blank,start,destination,before,pending,after};
}
async function internalControlDrag(sourceFrame,target,action){
  const selector=`.frame[data-frame="${sourceFrame}"] .host-asset-drag`;
  const button=page.locator(selector);assert(await button.isVisible(),'separate asset control absent');
  const box=await button.boundingBox(),before=await canonical(),frame=before.rust.frames.find(f=>f.id===target.frame);
  const start={x:box.x+box.width/2,y:box.y+box.height/2},end={x:Math.max(frame.content.x,frame.clip.x)+18,y:Math.max(frame.content.y,frame.clip.y)+35};
  const mark=await page.evaluate(()=>window.kumihimo.assetControlStarts.length);
  if(action==='copy')await page.keyboard.down('Control');
  try{
    await page.mouse.move(start.x,start.y);await page.mouse.down();await page.mouse.move(start.x+8,start.y+2,{steps:2});
    await page.mouse.move(end.x,end.y,{steps:6});await page.mouse.move(end.x+1,end.y+1);
    await page.waitForFunction(({mark,action,target})=>{const b=window.kumihimo,a=b.adapter;return b.assetControlStarts.length>mark&&a.dropPreview?.action===action&&a.dropPreview.destination.frame===target.frame&&!a.drag;},{mark,action,target});
    await page.mouse.up();
  }finally{if(action==='copy')await page.keyboard.up('Control');}
  const after=await settled(),starts=await page.evaluate(mark=>window.kumihimo.assetControlStarts.slice(mark),mark);
  assert(starts.length===1&&starts[0].trusted&&starts[0].control&&starts[0].payload?.asset&&starts[0].effect_allowed==='copyLink','trusted dragstart from separate live control absent');
  assert(after.host.last_recipient?.view===target.view&&after.host.last_recipient?.frame===target.frame&&after.host.last_recipient?.action.toLowerCase()===action,'actual internal control drop recipient/action mismatch');
  return {provenance:'trusted Chromium-generated HTML dragstart/dragover/drop on actual task-link control; no synthetic internal MIME',selector,start,end,starts,before,after};
}
async function run(){
  context=await chromium.launchPersistentContext(config.profile,{executablePath:config.browser,headless:true,viewport:{width:1400,height:1200},acceptDownloads:false,args:['--no-first-run','--no-default-browser-check','--disable-background-networking']});
  context.setDefaultTimeout(3500);
  await context.route('**/*',r=>{const u=new URL(r.request().url());return ['http:','https:'].includes(u.protocol)&&u.hostname!=='127.0.0.1'?r.abort('blockedbyclient'):r.continue();});
  page=context.pages()[0]||await context.newPage();
  page.on('pageerror',e=>{if(evidence.errors.length<16)evidence.errors.push(String(e).slice(0,500));});
  evidence.browser_executable_sha256=hash(fs.readFileSync(config.browser));
  evidence.node_version=process.version;evidence.playwright_version=require(path.join(config.modules,'playwright/package.json')).version;
  evidence.proof_helper_sha256=hash(fs.readFileSync(config.helper));
  const build=JSON.parse(fs.readFileSync(config.build,'utf8'));evidence.build_manifest_sha256=hash(fs.readFileSync(config.build));
  assert(build.wasm_bindgen==='wasm-bindgen 0.2.128','exact binding CLI absent');
  for(const source of build.sources){const filename=path.resolve(config.project,source.path);assert(hash(fs.readFileSync(filename))===source.sha256,'source differs from manifest: '+source.path);}
  evidence.source_hashes_verified=build.sources.length;
  for(const artifact of build.generated){const response=await context.request.get(new URL('/pkg/'+path.basename(artifact.path),config.url).href);assert(response.ok(),'package not served');const sha=hash(await response.body());assert(sha===artifact.sha256,'served package differs from manifest');evidence.bindings.push({path:artifact.path,sha256:sha,served:true});}
  for(const name of ['index.html','adapter.mjs','pilot.mjs','workspace.css']){const source=build.sources.find(s=>s.path.replaceAll('\\','/').endsWith('/web/'+name));assert(source,'web source missing from binding: '+name);const response=await context.request.get(new URL('/'+name,config.url).href);assert(response.ok(),'web source not served: '+name);assert(hash(await response.body())===source.sha256,'served web source differs: '+name);}
  await page.goto(config.url,{waitUntil:'networkidle'});await page.waitForFunction(()=>window.kumihimo?.ready===true);await idle();
  await page.locator('#task-count').fill('2');await page.locator('#detail-count').fill('1');await page.locator('#nesting-depth').fill('1');await page.locator('#new-workspace').click();await page.waitForFunction(()=>window.kumihimo?.ready===true);await idle();
  const initial=await canonical();assert(initial.host.drop_capabilities?.actions.browser_files.includes('import'),'host drop capability absent');
  await page.evaluate(()=>{
    const a=window.kumihimo.adapter,original=a.route.bind(a);window.kumihimo.dropProofRoutes=[];
    a.route=request=>{const response=original(request);if(request.route.startsWith('drop_')){const record={route:request.route,token:request.token??response.result?.token,ok:response.ok,error:response.error,result:response.result,source:request.source,action:request.action,destination:request.destination,metadata:request.items,content_sizes:request.contents?.map(text=>new TextEncoder().encode(text).length)};if(window.kumihimo.dropProofRoutes.length<40)window.kumihimo.dropProofRoutes.push(record);}return response;};
    window.kumihimo.assetControlStarts=[];
    document.querySelector('#workspace').addEventListener('dragstart',event=>{let payload;try{payload=JSON.parse(event.dataTransfer.getData('application/x-kumihimo-asset'));}catch{}window.kumihimo.assetControlStarts.push({trusted:event.isTrusted,control:event.target.matches('.host-asset-drag'),payload,effect_allowed:event.dataTransfer.effectAllowed});});
  });
  const [first,second]=await targets();
  const sourceFrame=(await canonical()).rust.frames.find(f=>f.workspace===initial.rust.root&&f.views.length>1);
  assert(sourceFrame,'root title/tab source absent');
  const titleSelector=`.frame[data-frame="${sourceFrame.id}"] .frame-title`,headerSelector=`.frame-header[data-frame="${sourceFrame.id}"]`;
  const titleState=await page.locator(titleSelector).evaluate(el=>({draggable:el.draggable,host_asset:el.dataset.hostAsset}));
  assert(!titleState.draggable&&!titleState.host_asset,'frame title still owns asset dragging');
  evidence.cases.push({id:'POINTER-TITLE-ARRANGEMENT-CANCEL',...(await pointerPreviewCancelled(titleSelector,'drag-frame',second))});
  evidence.cases.push({id:'POINTER-BLANK-HEADER-ARRANGEMENT-CANCEL',...(await pointerPreviewCancelled(headerSelector,'drag-frame',second,{blank:true}))});
  const tabs=page.locator(`.frame[data-frame="${sourceFrame.id}"] [data-action="select"]`),tabBefore=await canonical();
  await tabs.nth(1).click();await idle();let selected=await canonical();
  const nextId=await tabs.nth(1).getAttribute('data-view');assert(selected.rust.frames.find(f=>f.id===sourceFrame.id).selected_view===nextId,'trusted tab click did not select canonical view');
  await tabs.nth(1).focus();await page.keyboard.press('ArrowLeft');await idle();const tabAfter=await canonical();
  assert(tabAfter.rust.frames.find(f=>f.id===sourceFrame.id).selected_view===sourceFrame.selected_view,'keyboard tab selection did not restore original view');
  assert(taskDigest(tabBefore)===taskDigest(tabAfter)&&tabBefore.host.revision===tabAfter.host.revision,'tab selection changed host meaning');
  evidence.cases.push({id:'POINTER-KEYBOARD-TABS',before:tabBefore,selected,after:tabAfter});
  evidence.cases.push({id:'POINTER-SPLIT-RESIZE-CANCEL',...(await pointerPreviewCancelled('[data-action="resize-split"]','resize-split',second))});
  for(const [i,target,text]of [[1,first,'WP018 fixture alpha'],[2,second,'WP018 fixture beta']]){
    const before=await canonical(),dom=await transfer(target,`fixture-${i}.txt`,text),after=await settled();
    assert(dom.file?.actual_file&&dom.transfer_actual,'actual File/DataTransfer missing');assert(dom.guard.file_reads===0&&dom.guard.data_reads===0,'hover accessed protected payload');
    assert(dom.hover.preview?.frame===target.frame&&dom.hover.preview?.view===target.view&&dom.hover.preview.action==='import'&&dom.hover.label,'exact advertised preview absent');
    const task=after.host.tasks.find(t=>t.resource===target.resource),prior=before.host.tasks.find(t=>t.resource===target.resource);
    assert(task.body===prior.body+'\n'+text,'canonical host fixture content mismatch');assert(after.host.last_recipient?.view===target.view&&after.host.last_recipient.frame===target.frame&&after.host.last_recipient.resource===target.resource,'canonical exact recipient mismatch');
    assert(arrangement(before)===arrangement(after),'content import changed arrangement');
    evidence.cases.push({id:`DOM-FILE-EXACT-${i}`,dom,before,after,fixture_text:text});
  }
  await capture('file-drop-two-targets');
  for(const [name,text]of [['unsupported.bin','fixture unsupported'],['oversized.txt','x'.repeat(8193)]]){
    const before=await canonical(),mark=await page.evaluate(()=>window.kumihimo.dropProofRoutes.length),dom=await transfer(first,name,text),after=await settled(),routes=await page.evaluate(mark=>window.kumihimo.dropProofRoutes.slice(mark),mark);
    assert(routes.some(r=>r.route==='drop_admit'&&!r.ok),'host refusal absent');assert(taskDigest(before)===taskDigest(after)&&before.host.revision===after.host.revision,'rejected file changed host');
    evidence.cases.push({id:name.endsWith('.bin')?'DOM-FILE-UNSUPPORTED':'DOM-FILE-OVERSIZED',dom,routes,before,after});
  }
  const cancelBefore=await canonical(),mark=await page.evaluate(()=>window.kumihimo.dropProofRoutes.length),delayed=await transfer(first,'cancelled.txt','fixture cancelled',{delay:true});
  await page.waitForFunction(()=>window.kumihimo.proofDeferred?.read_started&&window.kumihimo.adapter.pendingDrops.size===1);
  await capture('file-drop-pending');await page.keyboard.press('Escape');
  await page.evaluate(()=>{window.kumihimo.proofDeferred.resolve();});
  const cancelAfter=await settled(),cancelRoutes=await page.evaluate(mark=>window.kumihimo.dropProofRoutes.slice(mark),mark);
  assert(cancelRoutes.filter(r=>r.route==='drop_cancel'&&r.ok).length===1&&!cancelRoutes.some(r=>r.route==='drop_complete'),'Escape did not consume exactly one pending drop');
  assert(taskDigest(cancelBefore)===taskDigest(cancelAfter)&&cancelBefore.host.revision===cancelAfter.host.revision&&arrangement(cancelBefore)===arrangement(cancelAfter),'cancelled read mutated host/arrangement');
  evidence.cases.push({id:'DOM-FILE-ASYNC-ESCAPE',provenance:'actual keyboard Escape; injected File.arrayBuffer delay around real browser File bytes',dom:delayed,routes:cancelRoutes,before:cancelBefore,after:cancelAfter});
  const assets=await canonical(),asset=assets.host.tasks.find(t=>t.resource===first.resource);
  const linkDom=await internalControlDrag(first.frame,second,'link'),linked=linkDom.after;
  const destination=linked.host.tasks.find(t=>t.resource===second.resource);
  assert(destination.links?.[asset.id]===assets.host.revision,'canonical internal host link absent');
  evidence.cases.push({id:'DOM-INTERNAL-LINK',dom:linkDom,before:assets,after:linked});
  const copyBefore=await canonical(),source=copyBefore.host.tasks.find(t=>t.id===asset.id),copyDom=await internalControlDrag(first.frame,second,'copy'),copied=copyDom.after;
  assert(copied.host.tasks.find(t=>t.resource===second.resource).body===source.body,'canonical independent internal text copy mismatch');
  evidence.cases.push({id:'DOM-INTERNAL-COPY',dom:copyDom,before:copyBefore,after:copied});
  await capture('file-drop-completed-cancelled');
  evidence.final=await canonical();evidence.routes=await page.evaluate(()=>window.kumihimo.dropProofRoutes);evidence.product_events=await page.evaluate(()=>window.kumihimo.events.slice(-60));
  assert(!evidence.errors.length,'browser page error');evidence.verdict='DOM_FILE_BOUNDARY_PASSED_PENDING_IMAGE_INSPECTION';
}
(async()=>{
  try{await Promise.race([run(),new Promise((_,reject)=>{deadline=setTimeout(()=>reject(new Error('30 second drop proof deadline')),30000);})]);}
  catch(error){evidence.verdict='FAIL';evidence.failure=String(error).slice(0,1000);process.exitCode=1;if(page&&!page.isClosed()){try{evidence.failure_state=await canonical();if(evidence.screenshots.length<4)await capture('file-drop-failure');}catch(e){evidence.failure_capture_error=String(e).slice(0,500);}}}
  finally{clearTimeout(deadline);if(context){try{await context.close();evidence.owned_cleanup=true;}catch(error){evidence.owned_cleanup=false;process.exitCode=1;}}const output=JSON.stringify(evidence,null,2);if(Buffer.byteLength(output)>2*1024*1024){evidence.verdict='FAIL';evidence.failure='evidence exceeds 2MiB';evidence.cases=[];process.exitCode=1;}fs.writeFileSync(path.join(config.out,'drop-evidence.json'),JSON.stringify(evidence,null,2));process.stdout.write(JSON.stringify({verdict:evidence.verdict,evidence:path.join(config.out,'drop-evidence.json'),screenshots:evidence.screenshots,owned_cleanup:evidence.owned_cleanup})+'\n');}
})();
'''


def main() -> None:
    project = Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--node", default=os.environ.get("KUMIHIMO_NODE"))
    parser.add_argument("--node-modules", type=Path, default=os.environ.get("KUMIHIMO_NODE_MODULES"))
    parser.add_argument("--browser", type=Path, default=os.environ.get("KUMIHIMO_BROWSER"))
    parser.add_argument("--out", type=Path, default=project / ".local/web/drop-proof")
    parser.add_argument("--build-manifest", type=Path, default=project / ".local/web/build-manifest.json")
    args = parser.parse_args()
    url = urlsplit(args.url)
    if url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port or url.username or url.password:
        parser.error("proof URL must be explicit http://127.0.0.1:port/")
    if not args.node or not args.node_modules or not args.browser:
        parser.error("configure existing Node, node-modules and browser; no installation")
    node = Path(shutil.which(args.node) or args.node)
    if not node.is_file() or not args.browser.is_file():
        parser.error("configured Node/browser executable absent")
    if not (args.node_modules / "playwright/package.json").is_file():
        parser.error("configured node-modules has no Playwright")
    out = args.out.resolve()
    if not out.is_relative_to((project / ".local").resolve()):
        parser.error("evidence output must remain inside project ignored .local")
    build = args.build_manifest.resolve(strict=True)
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="drop-profile-", dir=out) as profile:
        config = {"url": args.url, "project": str(project), "modules": str(args.node_modules.resolve()), "browser": str(args.browser.resolve()), "out": str(out), "build": str(build), "profile": profile, "helper": str(Path(__file__).resolve())}
        script = Path(profile) / "drop-proof.cjs"
        script.write_text(SCRIPT, encoding="utf-8", newline="\n")
        result = subprocess.run([str(node), str(script)], input=json.dumps(config), text=True, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), check=False)
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
