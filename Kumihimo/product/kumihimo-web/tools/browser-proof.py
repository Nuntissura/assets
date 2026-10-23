"""Run the owning quiet browser batch with an already configured Node/Playwright/Chrome."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
from urllib.parse import urlsplit


SCRIPT = r'''
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const config = JSON.parse(fs.readFileSync(0, 'utf8'));
const {chromium} = require(path.join(config.modules, 'playwright'));
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const hash = value => crypto.createHash('sha256').update(value).digest('hex');
const evidence = {version:1, scope:'WP013 real browser; no physical input/native popup/other-platform/performance claim', cases:[], events:[], screenshots:[], errors:[]};
let context, page, deadline;
const tasks = host => host.tasks.map(t=>({id:t.id,column:t.column,status:t.status,resource:t.resource,provider:t.provider,views:t.views,veto:t.veto,title_sha256:hash(t.title),body_sha256:hash(t.body)}));
const membership = state => JSON.stringify({root:state.root,workspaces:state.workspaces.map(w=>({id:w.id,owner:w.owner,frames:w.frames})),views:state.views.map(v=>({id:v.id,frame:v.frame,resource:v.resource,provider:v.provider,group:v.group})),resources:state.resources,frames:state.frames.map(f=>({id:f.id,workspace:f.workspace,child_workspace:f.child_workspace,views:f.views}))});
const canonical = async () => await page.evaluate(() => ({rust:window.kumihimo.snapshot(),host:window.kumihimo.hostSnapshot()}));
const idle = async () => await page.evaluate(async()=>{await window.kumihimo.adapter.whenIdle();});
const route = async request => {
  const response=await page.evaluate(req=>window.kumihimo.route(req),request);
  assert(response.version===1, 'versioned response missing');
  return response;
};
const observed = state => ({rust:state.rust,host:{revision:state.host.revision,tasks:tasks(state.host),last_recipient:state.host.last_recipient}});
async function diagnosticStep(label, action) {
  let timer;
  try {return await Promise.race([action(),new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error(label+' diagnostic timeout')),3000);})]);}
  finally {clearTimeout(timer);}
}
async function captureFailure() {
  if(!page||page.isClosed()) {evidence.failure_capture={page_alive:false};return;}
  evidence.failure_capture={page_alive:true};
  try {
    evidence.failure_capture.dom=await diagnosticStep('DOM',()=>page.evaluate(()=>{
      const bridge=window.kumihimo, adapter=bridge?.adapter;
      const last=adapter?.lastCommand;
      return {
        url:location.href,document_state:document.readyState,bridge_present:!!bridge,ready:bridge?.ready??null,
        status:document.querySelector('#status')?.textContent?.slice(0,500),
        status_state:document.querySelector('#status')?.dataset.state,
        capabilities:document.querySelector('#capabilities')?.textContent?.slice(0,500),
        selected_frame:document.querySelector('#frame-select')?.value,
        selected_view:document.querySelector('#view-select')?.value,
        destination:document.querySelector('#destination-select')?.value,
        proof_routes:bridge?.proofRoutes?.slice(-40)??[],
        proof_native:bridge?.proofNative?.slice(0,24)??[],
        proof_native_overflow:bridge?.proofNativeOverflow??null,
        proof_native_active:bridge?.proofNativeActive??null,
        adapter:adapter?{
          disposed:adapter.disposed,render_ticket:adapter.renderTicket,measure_ticket:adapter.measureTicket,
          preview:adapter.previewState,last_command:last?{
            request:last.request,response:{version:last.response.version,route:last.response.route,ok:last.response.ok,
              error:last.response.error,receipt:last.response.result?.receipt,
              postcommit_error:last.response.result?.postcommit_error,
              current_generation:last.response.result?.current?.generation}
          }:null,events:adapter.events.slice(-80)
        }:null
      };
    }));
  } catch(error) {evidence.failure_capture.dom_error=String(error).slice(0,500);}
  try {
    const state=await diagnosticStep('canonical',canonical);
    evidence.failure_capture.canonical=observed(state);
  } catch(error) {evidence.failure_capture.canonical_error=String(error).slice(0,500);}
  try {await diagnosticStep('screenshot',()=>capture('failure-state'));}
  catch(error) {evidence.failure_capture.screenshot_error=String(error).slice(0,500);}
}
async function capture(name) {
  assert(evidence.screenshots.length<9,'capture count bound');
  const filename=path.join(config.out, name+'.png');
  await page.screenshot({path:filename,fullPage:false});
  assert(fs.statSync(filename).size<=2*1024*1024,'capture byte bound');
  evidence.screenshots.push({path:filename,sha256:hash(fs.readFileSync(filename))});
}
async function bounds() {
  return await page.evaluate(()=>({
    workspace:document.querySelector('#workspace').getBoundingClientRect().toJSON(),
    frames:[...document.querySelectorAll('[data-frame].frame')].map(el=>({id:el.dataset.frame,rect:el.getBoundingClientRect().toJSON(),hidden:el.hidden})),
    controls:[...document.querySelectorAll('button,input,select,textarea,[role="tab"],[role="separator"]')].slice(0,200).map(el=>({tag:el.tagName,id:el.id,role:el.getAttribute('role'),name:el.getAttribute('aria-label')||el.textContent?.trim().slice(0,100),selected:el.getAttribute('aria-selected'),expanded:el.getAttribute('aria-expanded'),value:el.getAttribute('aria-valuenow'),rect:el.getBoundingClientRect().toJSON()}))
  }));
}
async function selectFrame(id) { await page.locator('#frame-select').selectOption(id); }
async function visibleContent(placeholder=false) {
  const state=await canonical();
  const frame=state.rust.frames.find(f=>f.visible&&!f.child_workspace&&f.selected_view&&(!placeholder||state.rust.views.find(v=>v.id===f.selected_view)?.placeholder));
  assert(frame,'reachable canonical leaf content absent');
  if(!frame.expanded) {await page.locator(`[data-action="expand"][data-frame="${frame.id}"]`).click();await idle();}
  const content=page.locator(`.frame[data-frame="${frame.id}"] .frame-content`);
  await content.scrollIntoViewIfNeeded(); await idle();
  const fresh=await canonical();
  const details=await content.evaluate(el=>({view:el.dataset.view,placeholder:el.dataset.placeholder,text:el.dataset.placeholder?el.textContent.slice(0,180):undefined,rect:el.getBoundingClientRect().toJSON(),width:innerWidth,height:innerHeight,hidden:el.hidden}));
  const reference=fresh.rust.frames.find(f=>f.id===frame.id);
  assert(!details.hidden&&details.rect.width>0&&details.rect.height>0&&details.rect.right>0&&details.rect.bottom>0&&details.rect.x<details.width&&details.rect.y<details.height,'leaf content outside visible viewport');
  assert(details.view===reference.selected_view,'content reference differs from fresh Rust');
  for(const key of ['x','y','width','height'])assert(Math.abs(details.rect[key]-reference.content[key])<2.1,'leaf content bounds differ from Rust '+key);
  if(placeholder)assert(details.placeholder&&details.text.includes('Provider unavailable'),'visible recovery message absent');
  return {content,state:fresh,details,frame:reference};
}
async function cancelledPointerPreview(source) {
  // The browser emits these events; no event dispatch or adapter cancellation is used.
  const handle=page.locator('[data-action="resize-split"]').first();
  await handle.focus(); await handle.scrollIntoViewIfNeeded(); await idle();
  const before=await canonical();
  const target=await handle.evaluate(el=>({workspace:el.dataset.workspace,node:el.dataset.node,axis:el.dataset.axis,rect:el.getBoundingClientRect().toJSON()}));
  const mark=await page.evaluate(()=>{const b=window.kumihimo;b.proofNativeActive=true;return {route:b.proofSequence,event:b.adapter.eventSequence,native:b.proofNative.length};});
  const x=target.rect.x+target.rect.width/2,y=target.rect.y+target.rect.height/2;
  await page.mouse.move(x,y);await page.mouse.down();
  await page.mouse.move(x+(target.axis==='horizontal'?8:0),y+(target.axis==='vertical'?8:0));await idle();
  const pending=await page.evaluate(()=>{const b=window.kumihimo,a=b.adapter;return {preview:a.previewState,drag:a.drag,captured:a.drag?document.querySelector('#workspace').hasPointerCapture(a.drag.pointerId):false,routes:b.proofRoutes.slice(),event_sequence:a.eventSequence};});
  assert(pending.drag?.kind==='resize-split'&&pending.captured,'real pointer capture absent before '+source);
  assert(pending.preview?.token&&pending.preview.geometry?.frames?.length,'accepted nonempty Rust preview absent before '+source);
  const accepted=pending.routes.findLast(r=>r.sequence>mark.route&&r.route==='preview'&&r.action==='resize_split'&&r.ok&&r.token===pending.preview.token);
  assert(accepted,'actual accepted resize preview response absent before '+source);
  assert(JSON.stringify((await canonical()).rust.layouts)===JSON.stringify(before.rust.layouts),'preview mutated canonical layout');
  const cancelPhase=await page.evaluate(token=>{const b=window.kumihimo;return {route:b.proofSequence,event:b.adapter.eventSequence,native:b.proofNative.length,accepted_token:token};},pending.preview.token);
  const cancelMark=cancelPhase.route;
  if(source==='lostpointercapture') {
    await page.evaluate(pointer=>document.querySelector('#workspace').releasePointerCapture(pointer),pending.drag.pointerId);
    await page.mouse.move(x+(target.axis==='horizontal'?9:0),y+(target.axis==='vertical'?9:0));
  } else if(source==='blur') {
    await page.evaluate(()=>document.querySelector('#restricted-popup-frame').contentDocument.querySelector('#request-popup').focus({preventScroll:true}));
  } else await page.keyboard.press('Escape');
  await page.waitForFunction(({source,token,event,phase})=>{
    const b=window.kumihimo;
    if(b.adapter.previewState||b.adapter.drag||b.proofNativeOverflow)return false;
    if(source==='blur'){
      const cancellations=b.proofRoutes.filter(r=>r.sequence>phase.route&&r.route==='cancel_preview'&&r.request_token===token);
      const phaseReasons=b.adapter.events.filter(e=>e.sequence>phase.event&&e.type==='cancel');
      const reasons=phaseReasons.filter(e=>e.source==='blur');
      const windows=b.proofNative.slice(phase.native).filter(e=>e.type==='blur'&&e.trusted&&e.window_target);
      return phase.accepted_token===token&&cancellations.length===1&&cancellations[0].ok&&phaseReasons.length===1&&reasons.length===1&&windows.length===1&&(windows[0].token===token||(windows[0].token===null&&windows[0].route_sequence>=cancellations[0].sequence&&windows[0].adapter_event_sequence>=reasons[0].sequence));
    }
    return b.adapter.events.some(e=>e.sequence>event&&e.type==='cancel'&&e.source===source)&&b.proofNative.some(e=>(source==='Escape'?e.type==='keydown'&&e.key==='Escape':e.type===source)&&e.trusted&&e.token===token);
  },{source,token:pending.preview.token,event:pending.event_sequence,phase:cancelPhase});
  await idle();
  const result=await page.evaluate(({mark,token,native})=>{const b=window.kumihimo;return {preview:b.adapter.previewState,drag:b.adapter.drag,native:b.proofNative.slice(native),routes:b.proofRoutes.filter(r=>r.sequence>mark&&(r.request_token===token||r.token===token)),events:b.adapter.events.slice(-20)};},{mark:cancelMark,token:pending.preview.token,native:mark.native});
  const cancellations=result.routes.filter(r=>r.route==='cancel_preview'&&r.request_token===pending.preview.token);
  assert(cancellations.length===1&&cancellations[0].ok,'real Rust cancellation response absent or duplicate');
  assert(!result.routes.some(r=>r.action==='commit_preview'),'cancelled token was committed');
  const after=await canonical();
  assert(membership(after.rust)===membership(before.rust)&&JSON.stringify(after.rust.layouts)===JSON.stringify(before.rust.layouts),'cancellation altered canonical arrangement');
  const split=state=>state.nodes.find(n=>n.id===target.node&&n.workspace===target.workspace);
  assert(JSON.stringify(split(after.rust).weights)===JSON.stringify(split(before.rust).weights),'cancelled split weights changed');
  assert(JSON.stringify(tasks(after.host))===JSON.stringify(tasks(before.host)),'cancellation changed host task meaning');
  assert(!result.preview&&!result.drag&&!(await page.evaluate(()=>window.kumihimo.proofNativeOverflow)),'cancelled preview/token remains or native observation overflow');
  await page.mouse.up();await idle();
  const released=await canonical();
  assert(membership(released.rust)===membership(before.rust)&&JSON.stringify(released.rust.layouts)===JSON.stringify(before.rust.layouts)&&JSON.stringify(tasks(released.host))===JSON.stringify(tasks(before.host)),'pointer release altered cancelled canonical arrangement or host meaning');
  const releasedRoutes=await page.evaluate(({mark,token})=>window.kumihimo.proofRoutes.filter(r=>r.sequence>mark&&(r.request_token===token||r.token===token)),{mark:cancelMark,token:pending.preview.token});
  assert(!releasedRoutes.some(r=>r.action==='commit_preview'||r.receipt),'pointer release committed cancelled token or produced command receipt');
  await page.evaluate(()=>{window.kumihimo.proofNativeActive=false;});
  return {source,before:observed(before),accepted,pointer_id:pending.drag.pointerId,token:pending.preview.token,cancellation_marker:cancelPhase,result,after:observed(after),released:observed(released),released_routes:releasedRoutes};
}
async function run() {
  context=await chromium.launchPersistentContext(config.profile,{executablePath:config.browser,headless:true,viewport:{width:1280,height:900},acceptDownloads:false,args:['--no-first-run','--no-default-browser-check','--disable-background-networking']});
  context.setDefaultTimeout(8000);
  await context.route('**/*',async routed=>{
    const url=new URL(routed.request().url());
    if (['http:','https:'].includes(url.protocol) && url.hostname!=='127.0.0.1') return routed.abort('blockedbyclient');
    return routed.continue();
  });
  page=context.pages()[0]||await context.newPage();
  page.on('console', message=>{if(evidence.events.length<120)evidence.events.push({type:'console',level:message.type(),text:message.text().slice(0,300)});});
  page.on('pageerror', error=>{if(evidence.errors.length<20)evidence.errors.push(String(error).slice(0,500));});
  context.on('page', popup=>{if(popup!==page) { evidence.errors.push('Unexpected additional browser page'); void popup.close(); }});
  evidence.browser_version=context.browser()?.version()||await page.evaluate(()=>navigator.userAgent);
  evidence.browser_executable_sha256=hash(fs.readFileSync(config.browser));
  evidence.node_version=process.version;
  evidence.playwright_version=require(path.join(config.modules,'playwright/package.json')).version;
  evidence.build=JSON.parse(fs.readFileSync(config.build,'utf8'));
  assert(evidence.build.wasm_bindgen==='wasm-bindgen 0.2.128','exact CLI version not bound');
  for (const artifact of evidence.build.generated) {
    const target=new URL('/pkg/'+path.basename(artifact.path),config.url).href;
    const result=await context.request.get(target);
    assert(result.ok(),'generated package response');
    assert(hash(await result.body())===artifact.sha256,'served module hash differs from build manifest');
  }
  await page.goto(config.url,{waitUntil:'networkidle'});
  await page.waitForFunction(()=>window.kumihimo?.ready===true);
  await idle();
  const startup=await canonical();
  await page.locator('#task-count').fill('4');
  await page.locator('#detail-count').fill('1');
  await page.locator('#nesting-depth').fill('2');
  await page.locator('#new-workspace').click();
  await page.waitForFunction(()=>window.kumihimo?.ready===true); await idle();
  const initial=await canonical();
  assert(initial.host.tasks.length===4&&initial.host.tasks.length!==startup.host.tasks.length,'actual host-configured count was not applied');
  assert(initial.rust.frames.length>initial.host.tasks.length,'actual nested frames absent');
  assert(initial.rust.frames.some(f=>f.depth>=2),'requested recursive depth absent');
  for(const f of initial.rust.frames) assert(typeof f.id==='string'&&Number.isFinite(f.rect.x),'decimal identity/finite geometry');
  const first=initial.rust.frames.find(f=>f.workspace===initial.rust.root);
  await page.locator(`[data-action="expand"][data-frame="${first.id}"]`).click(); await idle();
  await page.locator(`[data-action="expand"][data-frame="${first.id}"]`).click(); await idle();
  await capture('case001-normal');
  const normalBounds=await bounds();
  assert(normalBounds.workspace.x>0&&normalBounds.workspace.y>0,'nonzero host origin required');
  for (const dom of normalBounds.frames.filter(f=>!f.hidden&&f.rect.width>0)) {
    const rust=(await canonical()).rust.frames.find(f=>f.id===dom.id);
    assert(rust,'DOM frame absent from canonical Rust');
    for(const key of ['x','y','width','height']) assert(Math.abs(dom.rect[key]-rust.rect[key])<2.1,'DOM/Rust geometry disagreement '+dom.id+' '+key);
  }
  await page.setViewportSize({width:480,height:700}); await idle();
  await capture('case001-narrow');
  const narrow=await canonical();
  assert(membership(narrow.rust)===membership(initial.rust),'resize lost membership');
  const narrowBounds=await bounds();
  await page.setViewportSize({width:1280,height:900}); await idle();
  evidence.cases.push({id:'EVAL-KUM-013-001',startup:observed(startup),before:observed(initial),normal_bounds:normalBounds,narrow:observed(narrow),narrow_bounds:narrowBounds,requested_counts:{tasks:4,details:1,depth:2}});

  // Passive proof observation retains rejected preview errors; the adapter sees
  // precisely its original request/response and no host text is copied here.
  await page.evaluate(()=>{
    const bridge=window.kumihimo, adapter=bridge.adapter, original=adapter.route.bind(adapter);
    bridge.proofRoutes=[];
    bridge.proofSequence=0;
    bridge.proofMoves=[];
    bridge.proofMoveCount=0;
    bridge.proofMoveOverflow=false;
    bridge.proofNative=[];
    bridge.proofNativeActive=false;bridge.proofNativeOverflow=false;
    const observe=event=>{if(!bridge.proofNativeActive)return;if(bridge.proofNative.length>=24){bridge.proofNativeOverflow=true;return;}bridge.proofNative.push({type:event.type,key:event.key,trusted:event.isTrusted,window_target:event.target===window,pointer_id:event.pointerId,token:adapter.previewState?.token??null,route_sequence:bridge.proofSequence,adapter_event_sequence:adapter.eventSequence,view:event.target.closest?.('.frame-content')?.dataset.view,x:event.clientX,y:event.clientY});};
    document.querySelector('#workspace').addEventListener('lostpointercapture',observe,{capture:true});
    document.querySelector('#workspace').addEventListener('click',observe,{capture:true});
    window.addEventListener('blur',observe,{capture:true});
    window.addEventListener('keydown',event=>{if(event.key==='Escape')observe(event);},{capture:true});
    adapter.route=function(request) {
      const sequence=++bridge.proofSequence;
      const response=original(request);
      bridge.proofRoutes.push({sequence,route:request.route,action:request.action?.kind,
        frame:request.action?.frame,rect:request.action?.rect,ok:response.ok,
        error:response.error,receipt:response.result?.receipt,request_token:request.token??request.action?.token,
        token:response.result?.token,resize:request.action?.kind==='resize_split'?{identity:request.identity,expected_generation:request.expected_generation,workspace:request.action.workspace,node:request.action.node,weights:request.action.weights}:undefined,
        input:request.route==='input'?{identity:request.identity,epoch:request.epoch,view:request.view,resource:request.resource,provider:request.provider,x:request.x,y:request.y,scale:request.scale}:undefined,
        input_result:request.route==='input'?response.result:undefined});
      if(bridge.proofRoutes.length>80)bridge.proofRoutes.splice(0,bridge.proofRoutes.length-80);
      if(request.route==='command'&&request.action?.kind==='move_frame') {
        bridge.proofMoveCount++;
        if(bridge.proofMoves.length>=8)bridge.proofMoveOverflow=true;
        else bridge.proofMoves.push(JSON.parse(JSON.stringify({sequence,
          request:{version:request.version,route:request.route,identity:request.identity,
            actor:request.actor,operation:request.operation,expected_generation:request.expected_generation,
            action:{kind:request.action.kind,frame:request.action.frame,destination:request.action.destination,index:request.action.index}},
          response:{version:response.version,route:response.route,ok:response.ok,error:response.error,
            result:{receipt:response.result?.receipt}}
        })));
      }
      return response;
    };
  });
  const gestureBefore=await canonical();
  await selectFrame(first.id);
  // Real header pointer entry followed by Escape cancels the pending Rust preview.
  const header=page.locator(`[data-action="drag-frame"][data-frame="${first.id}"]`);
  const headerBox=await header.boundingBox(); assert(headerBox,'draggable header is not reachable');
  await page.mouse.move(headerBox.x+25,headerBox.y+12); await page.mouse.down();
  await page.mouse.move(headerBox.x+65,headerBox.y+40,{steps:4});
  await page.keyboard.press('Escape'); await page.mouse.up(); await idle();
  assert(membership((await canonical()).rust)===membership(gestureBefore.rust),'Escape moved membership');
  await page.locator('[data-action="float"]').click(); await idle();
  assert((await canonical()).rust.frames.find(f=>f.id===first.id).floating,'float click did not commit');
  await page.locator('[data-action="redock"]').click(); await idle();
  assert(!(await canonical()).rust.frames.find(f=>f.id===first.id).floating,'redock click did not commit');
  const tabs=page.locator('[data-action="select"][data-view]');
  if(await tabs.count()>1) { await tabs.nth(1).focus(); await page.keyboard.press('Enter'); await idle(); }
  const moveDest=await page.locator('#destination-select option').evaluateAll(options=>options.map(o=>o.value).filter(Boolean));
  assert(moveDest.length>1,'move destination alternatives absent');
  const eligibleDestination=gestureBefore.rust.workspaces.find(workspace=>{
    const owner=gestureBefore.rust.frames.find(frame=>frame.id===workspace.owner);
    return owner&&owner.workspace===gestureBefore.rust.root&&owner.id!==first.id;
  })?.id;
  assert(eligibleDestination&&moveDest.includes(eligibleDestination),'eligible foreign child workspace absent');
  await page.locator('#destination-select').selectOption(eligibleDestination);
  const parentMoveMark=await page.evaluate(()=>{const b=window.kumihimo;b.proofMoves=[];b.proofMoveCount=0;b.proofMoveOverflow=false;return b.proofSequence;});
  await page.locator('[data-action="move-frame"]').click(); await idle();
  const parentPhase=await page.evaluate(({since,frame,destination})=>{const b=window.kumihimo;return {count:b.proofMoveCount,overflow:b.proofMoveOverflow,matches:b.proofMoves.filter(m=>m.sequence>since&&m.request.action.frame===frame&&m.request.action.destination===destination)};},{since:parentMoveMark,frame:first.id,destination:eligibleDestination});
  assert(!parentPhase.overflow&&parentPhase.count===1&&parentPhase.matches.length===1,'parent move observation missing, duplicate, wrong target or overflow');
  const parentMove=parentPhase.matches[0];
  assert(parentMove?.request.action.kind==='move_frame'&&parentMove.request.action.destination===eligibleDestination&&parentMove.response.ok===true&&parentMove.response.result.receipt.ok===true,'actual parent move route rejected');
  assert(parentMove.request.action.frame===first.id&&parentMove.response.result.receipt.replayed===false&&parentMove.request.actor===parentMove.response.result.receipt.actor&&parentMove.request.operation===parentMove.response.result.receipt.operation&&parentMove.request.identity.lineage===parentMove.response.result.receipt.identity.lineage&&parentMove.request.identity.incarnation===parentMove.response.result.receipt.identity.incarnation,'parent move actual request/receipt correlation failed');
  assert((await canonical()).rust.frames.find(f=>f.id===first.id).workspace===eligibleDestination,'parent move click did not commit');
  await page.locator('#destination-select').selectOption(gestureBefore.rust.root);
  const returnMoveMark=await page.evaluate(()=>{const b=window.kumihimo;b.proofMoves=[];b.proofMoveCount=0;b.proofMoveOverflow=false;return b.proofSequence;});
  await page.locator('[data-action="move-frame"]').click(); await idle();
  const returnPhase=await page.evaluate(({since,frame,destination})=>{const b=window.kumihimo;return {count:b.proofMoveCount,overflow:b.proofMoveOverflow,matches:b.proofMoves.filter(m=>m.sequence>since&&m.request.action.frame===frame&&m.request.action.destination===destination)};},{since:returnMoveMark,frame:first.id,destination:gestureBefore.rust.root});
  assert(!returnPhase.overflow&&returnPhase.count===1&&returnPhase.matches.length===1,'return move observation missing, duplicate, wrong target or overflow');
  const returnMove=returnPhase.matches[0];
  assert(returnMove?.request.action.kind==='move_frame'&&returnMove.request.action.destination===gestureBefore.rust.root&&returnMove.response.ok===true&&returnMove.response.result.receipt.ok===true,'actual return parent move route rejected');
  assert(returnMove.request.action.frame===first.id&&returnMove.response.result.receipt.replayed===false&&returnMove.request.actor===returnMove.response.result.receipt.actor&&returnMove.request.operation===returnMove.response.result.receipt.operation&&returnMove.request.identity.lineage===returnMove.response.result.receipt.identity.lineage&&returnMove.request.identity.incarnation===returnMove.response.result.receipt.identity.incarnation,'return move actual request/receipt correlation failed');
  assert((await canonical()).rust.frames.find(f=>f.id===first.id).workspace===gestureBefore.rust.root,'return parent move click did not commit');
  await page.locator('#dock-edge').selectOption('Left');
  await page.locator('[data-action="dock"]').click(); await idle();
  const slider=page.locator('#resize-weight');
  await slider.focus(); await page.keyboard.press('ArrowRight'); await page.keyboard.press('Enter'); await idle();
  const handles=page.locator('[data-action="resize-split"]');
  if(await handles.count()) { const b=await handles.first().boundingBox(); if(b){await page.mouse.move(b.x+b.width/2,b.y+b.height/2);await page.mouse.down();await page.mouse.move(b.x+b.width/2+16,b.y+b.height/2,{steps:3});await page.mouse.up();await idle();} }
  await page.locator('summary').click(); await idle();
  const cancellations=[];
  for(const source of ['Escape','lostpointercapture','blur'])cancellations.push(await cancelledPointerPreview(source));
  await page.locator('summary').click();await idle();
  const originalMetrics=await page.evaluate(()=>({width:innerWidth,height:innerHeight,deviceScaleFactor:devicePixelRatio}));
  const dprSession=await context.newCDPSession(page);
  let scaleVariant;
  try {
    await dprSession.send('Emulation.setDeviceMetricsOverride',{width:originalMetrics.width+1,height:originalMetrics.height,deviceScaleFactor:1.5,mobile:false});
    await page.waitForFunction(()=>devicePixelRatio===1.5&&window.kumihimo.snapshot().scale===1.5);await idle();
    const visible=await visibleContent();
    const scaleBefore=visible.state;
    assert(scaleBefore.rust.scale!==gestureBefore.rust.scale&&scaleBefore.rust.scale===1.5,'actual engine DPR did not reach Rust');
    const inputMark=await page.evaluate(()=>{const b=window.kumihimo;b.proofNativeActive=true;return {route:b.proofSequence,native:b.proofNative.length};});
    const r=visible.details.rect;
    const clip=visible.frame.clip,left=Math.max(1,r.x,clip.x),top=Math.max(1,r.y,clip.y),right=Math.min(visible.details.width-1,r.right,clip.x+clip.width),bottom=Math.min(visible.details.height-1,r.bottom,clip.y+clip.height);
    assert(right>left&&bottom>top,'scaled leaf clip is unreachable');
    await page.mouse.click((left+right)/2,(top+bottom)/2);await idle();
    const scaleAfter=await canonical();
    const input=await page.evaluate(mark=>({native:window.kumihimo.proofNative.slice(mark.native).filter(e=>e.type==='click'),routes:window.kumihimo.proofRoutes.filter(r=>r.sequence>mark.route&&r.route==='input'),dpr:devicePixelRatio}),inputMark);
    assert(input.native.length===1&&input.native[0].trusted&&input.native[0].view===visible.frame.selected_view&&!(await page.evaluate(()=>window.kumihimo.proofNativeOverflow)),'actual trusted leaf click absent');
    assert(input.routes.length===1&&input.routes[0].ok,'actual input route rejected or duplicated');
    const event=input.native[0],request=input.routes[0].input,view=scaleBefore.rust.views.find(v=>v.id===visible.frame.selected_view),recipient=scaleAfter.host.last_recipient;
    assert(request.view===view.id&&request.resource===view.resource&&request.provider===view.provider&&request.epoch===scaleBefore.rust.epoch&&request.scale===input.dpr&&request.x===event.x&&request.y===event.y,'observed input recipient/context differs from canonical');
    assert(recipient?.view===view.id&&recipient.resource===view.resource&&recipient.provider===view.provider,'provider received wrong leaf');
    assert(Math.abs(recipient.local.x-(event.x-visible.frame.content.x))<1e-8&&Math.abs(recipient.local.y-(event.y-visible.frame.content.y))<1e-8,'actual provider-local coordinates differ from observed CSS click');
    await page.evaluate(()=>{window.kumihimo.proofNativeActive=false;});
    await capture('case002-browser-scale');
    scaleVariant={provenance:'actual Chromium CDP device-metrics emulation; no physical-monitor claim',original_metrics:originalMetrics,metrics:{width:originalMetrics.width+1,height:originalMetrics.height,deviceScaleFactor:input.dpr},bounds:visible.details,before:observed(scaleBefore),input,after:observed(scaleAfter)};
  } finally {
    await dprSession.send('Emulation.setDeviceMetricsOverride',{...originalMetrics,mobile:false});
    await page.waitForFunction(scale=>devicePixelRatio===scale&&window.kumihimo.snapshot().scale===scale,originalMetrics.deviceScaleFactor);await idle();await dprSession.detach();
  }
  scaleVariant.restored=observed(await canonical());
  const afterGesture=await canonical();
  assert(JSON.stringify(tasks(afterGesture.host))===JSON.stringify(tasks(gestureBefore.host)),'arrangement altered host task meaning');
  const stale=await route({version:1,route:'preview',identity:gestureBefore.rust.identity,expected_generation:gestureBefore.rust.generation,action:{kind:'equalize',workspace:gestureBefore.rust.root}});
  assert(stale.ok===false,'stale generation preview accepted');
  await capture('case002-interactions');
  evidence.cases.push({id:'EVAL-KUM-013-002',before:observed(gestureBefore),after:observed(afterGesture),parent_move:parentMove,return_move:returnMove,stale,cancellations,scale_variant:scaleVariant,event_entry:'actual mouse/keyboard/controls'});

  const taskBefore=await canonical();
  await page.locator('#task-body').focus();
  await page.keyboard.press('Control+A'); await page.keyboard.press('ArrowLeft');
  const contentKeys=await canonical();
  assert(contentKeys.rust.generation===taskBefore.rust.generation,'editor shortcut mutated layout');
  assert(await page.locator('#task-body').evaluate(el=>el===document.activeElement),'editor shortcut lost content focus');
  await page.locator('#task-title').fill('Browser-owned edit');
  await page.locator('#task-body').fill('Browser proof host body');
  await page.locator('[data-action="save-task"]').click(); await idle();
  const edited=await canonical();
  assert(edited.host.revision!==taskBefore.host.revision,'host edit did not advance revision');
  await page.locator('#task-column').selectOption('done');
  await page.locator('#task-status').selectOption('complete');
  await page.locator('[data-action="domain"]').click(); await idle();
  const domain=await canonical();
  assert(domain.host.tasks.some(t=>t.column==='done'&&t.status==='complete'),'explicit domain action did not commit');
  const owning=domain.host.tasks[0];
  const wrongRecipient=await route({version:1,route:'host_command',identity:domain.rust.identity,actor:'browser-host-proof',expected_revision:domain.host.revision,view:owning.views[0],resource:domain.host.tasks[1].resource,provider:owning.provider,action:{kind:'domain',column:'incorrect',status:'incorrect'}});
  assert(!wrongRecipient.ok,'stale/wrong recipient was substituted');
  assert(JSON.stringify(tasks((await canonical()).host))===JSON.stringify(tasks(domain.host)),'wrong recipient changed host meaning');
  const staleHost=await route({version:1,route:'host_command',identity:domain.rust.identity,actor:'browser-host-proof',expected_revision:taskBefore.host.revision,view:owning.views[0],resource:owning.resource,provider:owning.provider,action:{kind:'domain',column:'incorrect',status:'incorrect'}});
  assert(!staleHost.ok,'stale host revision accepted');
  await page.locator('[data-action="veto"]').click(); await idle();
  const vetoBefore=await canonical();
  await page.locator('[data-action="close"]').click(); await idle();
  assert(membership((await canonical()).rust)===membership(vetoBefore.rust),'veto closed ownership');
  await page.locator('[data-action="veto"]').click(); await idle();
  await page.locator('[data-action="provider-toggle"]').click(); await idle();
  const unavailable=await canonical();
  assert(await page.locator('[data-placeholder]').count()>0,'provider recovery placeholder absent');
  const unavailableVisible=await visibleContent(true);
  await capture('case003-provider-recovery');
  await page.locator('[data-action="provider-toggle"]').click(); await idle();
  const recovered=await canonical();
  assert(membership(recovered.rust)===membership(unavailableVisible.state.rust),'provider recovery changed retained references');
  assert(recovered.rust.providers.every(p=>p.available)&&!recovered.rust.views.some(v=>v.placeholder),'provider recovery did not restore availability/mounts');
  evidence.cases.push({id:'EVAL-KUM-013-003',before:observed(taskBefore),edited:observed(edited),domain:observed(domain),wrong_recipient:wrongRecipient,stale_host:staleHost,unavailable:observed(unavailable),visible_placeholder:{bounds:unavailableVisible.details,canonical:observed(unavailableVisible.state)},recovered:observed(recovered)});

  await page.locator('[data-action="save-layout"]').click(); await idle();
  const savedState=await canonical();
  const stored=await page.evaluate(()=>({layout:localStorage.getItem(window.kumihimo.storage.layoutKey),host:localStorage.getItem(window.kumihimo.storage.hostKey)}));
  assert(stored.layout&&stored.host,'actual persisted bytes absent');
  assert(JSON.parse(stored.layout).version===1,'saved wrapper version');
  const bytes={layout_bytes:Buffer.byteLength(stored.layout),layout_sha256:hash(stored.layout),host_bytes:Buffer.byteLength(stored.host),host_sha256:hash(stored.host),layout:stored.layout,host:stored.host};
  // Persisted host text remains in the actual store; evidence exposes its hashes.
  delete bytes.host;
  await page.reload({waitUntil:'networkidle'}); await page.waitForFunction(()=>window.kumihimo?.ready); await idle();
  const fresh=await canonical();
  assert(fresh.rust.identity.incarnation!==savedState.rust.identity.incarnation,'reload did not create fresh WASM incarnation');
  assert(membership(fresh.rust)===membership(savedState.rust),'persisted layout membership lost');
  assert(JSON.stringify(fresh.rust.layouts)===JSON.stringify(savedState.rust.layouts),'persisted layout proportions changed');
  assert(JSON.stringify(tasks(fresh.host))===JSON.stringify(tasks(savedState.host)),'host store lost');
  // This restore-reference fault changes only the stored provider key. Runtime
  // availability is not persisted by the unchanged schema-1 Core codec.
  const missingWrapper=JSON.parse(stored.layout),missingCodec=JSON.parse(missingWrapper.layout);
  assert(missingCodec.schema===1&&missingCodec.providers.length===1,'saved provider fault fixture shape changed');
  const originalProviderKey=missingCodec.providers[0].key;
  missingCodec.providers[0].key='proof-missing-provider-key';
  missingWrapper.layout=JSON.stringify(missingCodec);
  const missingLayout=JSON.stringify(missingWrapper);
  missingCodec.providers[0].key=originalProviderKey;
  assert(JSON.stringify(missingCodec)===JSON.stringify(JSON.parse(JSON.parse(stored.layout).layout)),'provider-key fault changed other codec fields');
  await page.evaluate(layout=>localStorage.setItem(window.kumihimo.storage.layoutKey,layout),missingLayout);
  const missingStored=await page.evaluate(()=>({layout:localStorage.getItem(window.kumihimo.storage.layoutKey),host:localStorage.getItem(window.kumihimo.storage.hostKey)}));
  assert(missingStored.layout===missingLayout&&missingStored.host===stored.host,'actual missing-provider stored pair differs');
  await page.reload({waitUntil:'networkidle'});await page.waitForFunction(()=>window.kumihimo?.ready);await idle();
  const missingFresh=await canonical();
  assert(missingFresh.rust.identity.incarnation!==fresh.rust.identity.incarnation,'missing-provider reload reused WASM incarnation');
  assert(membership(missingFresh.rust)===membership(savedState.rust),'missing-provider restore lost retained numeric references');
  assert(JSON.stringify(missingFresh.rust.layouts)===JSON.stringify(savedState.rust.layouts),'missing-provider restore changed saved layout proportions');
  assert(JSON.stringify(tasks(missingFresh.host))===JSON.stringify(tasks(savedState.host))&&missingFresh.host.revision===savedState.host.revision,'missing-provider restore changed separately owned host meaning');
  assert(missingFresh.rust.providers.some(p=>p.availability==='Missing'&&!p.available)&&missingFresh.rust.views.some(v=>v.placeholder),'actual provider restore did not report Missing');
  const missingVisible=await visibleContent(true);
  await capture('case004-missing-provider-reload');
  await selectFrame(missingVisible.frame.id);
  await page.locator('[data-action="provider-toggle"]').click();await idle();
  const recoveredVisible=await visibleContent();
  assert(membership(recoveredVisible.state.rust)===membership(missingFresh.rust),'missing-provider recovery changed retained references');
  assert(JSON.stringify(recoveredVisible.state.rust.layouts)===JSON.stringify(missingFresh.rust.layouts),'missing-provider recovery changed layout proportions');
  assert(recoveredVisible.state.rust.providers.every(p=>p.available)&&!recoveredVisible.state.rust.views.some(v=>v.placeholder),'missing-provider recovery did not restore real mounts');
  await page.evaluate(()=>{window.kumihimo.recoveryClick=null;document.querySelector('#workspace').addEventListener('click',event=>{window.kumihimo.recoveryClick={trusted:event.isTrusted,view:event.target.closest('.frame-content')?.dataset.view,x:event.clientX,y:event.clientY};},{once:true,capture:true});});
  const recoveryRect=recoveredVisible.details.rect,recoveryClip=recoveredVisible.frame.clip;
  const recoveryLeft=Math.max(1,recoveryRect.x,recoveryClip.x),recoveryTop=Math.max(1,recoveryRect.y,recoveryClip.y),recoveryRight=Math.min(recoveredVisible.details.width-1,recoveryRect.right,recoveryClip.x+recoveryClip.width),recoveryBottom=Math.min(recoveredVisible.details.height-1,recoveryRect.bottom,recoveryClip.y+recoveryClip.height);
  assert(recoveryRight>recoveryLeft&&recoveryBottom>recoveryTop,'restored mount is not reachable');
  await page.evaluate(()=>{
    const bridge=window.kumihimo,adapter=bridge.adapter,original=adapter.route;
    bridge.recoveryInput={count:0,overflow:false,records:[]};
    adapter.route=function(request){
      const response=original.call(this,request);
      if(request.route==='input'){
        const capture=bridge.recoveryInput;capture.count++;
        if(capture.records.length>=2)capture.overflow=true;
        else capture.records.push(JSON.parse(JSON.stringify({request:{version:request.version,route:request.route,identity:request.identity,epoch:request.epoch,view:request.view,resource:request.resource,provider:request.provider,x:request.x,y:request.y,scale:request.scale},response:{version:response.version,route:response.route,ok:response.ok,error:response.error,result:response.result}})));
      }
      return response;
    };
    bridge.restoreRecoveryRoute=()=>{adapter.route=original;delete bridge.restoreRecoveryRoute;};
  });
  try {await page.mouse.click((recoveryLeft+recoveryRight)/2,(recoveryTop+recoveryBottom)/2);await idle();}
  finally {await page.evaluate(()=>window.kumihimo.restoreRecoveryRoute());}
  const missingRecovered=await canonical(),recoveryClick=await page.evaluate(()=>window.kumihimo.recoveryClick);
  const recoveryInput=await page.evaluate(()=>window.kumihimo.recoveryInput);
  const recoveryView=recoveredVisible.state.rust.views.find(v=>v.id===recoveredVisible.frame.selected_view),recoveryRecipient=missingRecovered.host.last_recipient;
  assert(recoveryClick?.trusted&&recoveryClick.view===recoveryView.id,'actual restored-content click absent');
  assert(!recoveryInput.overflow&&recoveryInput.count===1&&recoveryInput.records.length===1,'restored input observation missing, duplicated or overflowed');
  const recoveryRequest=recoveryInput.records[0].request,recoveryResponse=recoveryInput.records[0].response,recoveryBefore=recoveredVisible.state.rust;
  assert(recoveryRequest.version===1&&recoveryRequest.route==='input'&&recoveryRequest.identity.lineage===recoveryBefore.identity.lineage&&recoveryRequest.identity.incarnation===recoveryBefore.identity.incarnation&&recoveryRequest.epoch===recoveryBefore.epoch&&recoveryRequest.view===recoveryView.id&&recoveryRequest.resource===recoveryView.resource&&recoveryRequest.provider===recoveryView.provider&&recoveryRequest.scale===recoveryBefore.scale&&recoveryRequest.x===recoveryClick.x&&recoveryRequest.y===recoveryClick.y,'restored input request differs from fresh canonical recipient or trusted click');
  assert(recoveryResponse.version===1&&recoveryResponse.route==='input'&&recoveryResponse.ok===true&&recoveryResponse.result.epoch===recoveryBefore.epoch,'actual restored input response did not acknowledge current epoch');
  assert(recoveryResponse.result.recipient.view===recoveryView.id&&recoveryResponse.result.recipient.resource===recoveryView.resource&&recoveryResponse.result.recipient.provider===recoveryView.provider&&Math.abs(recoveryResponse.result.local.x-(recoveryClick.x-recoveredVisible.frame.content.x))<1e-8&&Math.abs(recoveryResponse.result.local.y-(recoveryClick.y-recoveredVisible.frame.content.y))<1e-8,'actual restored input response recipient/local mismatch');
  assert(recoveryRecipient?.view===recoveryView.id&&recoveryRecipient.resource===recoveryView.resource&&recoveryRecipient.provider===recoveryView.provider,'restored provider did not acknowledge exact recipient');
  assert(Math.abs(recoveryRecipient.local.x-(recoveryClick.x-recoveredVisible.frame.content.x))<1e-8&&Math.abs(recoveryRecipient.local.y-(recoveryClick.y-recoveredVisible.frame.content.y))<1e-8,'restored provider local input mismatch');
  assert(JSON.stringify(tasks(missingRecovered.host))===JSON.stringify(tasks(savedState.host))&&missingRecovered.host.revision===savedState.host.revision,'recovery input changed host meaning');
  const missingProvider={provenance:'injected unavailable-provider restore-reference fault: only persisted provider key changed',stored:{layout:missingStored.layout,layout_sha256:hash(missingStored.layout),layout_bytes:Buffer.byteLength(missingStored.layout),host_sha256:hash(missingStored.host),host_bytes:Buffer.byteLength(missingStored.host)},fresh:observed(missingFresh),visible_placeholder:{bounds:missingVisible.details,canonical:observed(missingVisible.state)},recovery_click:recoveryClick,recovery_input:recoveryInput,recovered:observed(missingRecovered)};
  await page.evaluate(pair=>{localStorage.setItem(window.kumihimo.storage.layoutKey,pair.layout);localStorage.setItem(window.kumihimo.storage.hostKey,pair.host);},stored);
  const restoredBytes=await page.evaluate(()=>({layout:localStorage.getItem(window.kumihimo.storage.layoutKey),host:localStorage.getItem(window.kumihimo.storage.hostKey)}));
  assert(restoredBytes.layout===stored.layout&&restoredBytes.host===stored.host,'original valid stored pair was not restored exactly');
  await page.reload({waitUntil:'networkidle'});await page.waitForFunction(()=>window.kumihimo?.ready);await idle();
  const validRestored=await canonical();
  assert(validRestored.rust.identity.incarnation!==missingRecovered.rust.identity.incarnation&&membership(validRestored.rust)===membership(savedState.rust),'original valid session restore failed');
  assert(JSON.stringify(validRestored.rust.layouts)===JSON.stringify(savedState.rust.layouts),'original valid session layout proportions changed');
  assert(JSON.stringify(tasks(validRestored.host))===JSON.stringify(tasks(savedState.host)),'original valid host store restore failed');
  missingProvider.valid_restored=observed(validRestored);
  const saveRoute=await route({version:1,route:'save'});
  const invalid=[];
  for(const layout of ['{','x'.repeat(1100000),JSON.stringify({...JSON.parse(saveRoute.result.layout),version:2})]) {
    const before=await canonical();
    const result=await route({version:1,route:'load',layout,host:saveRoute.result.host,fresh_incarnation:'invalid-candidate'});
    assert(!result.ok,'invalid candidate accepted');
    assert(JSON.stringify((await canonical()).rust)===JSON.stringify(before.rust),'invalid candidate replaced state');
    invalid.push({ok:result.ok,error:result.error,input_bytes:Buffer.byteLength(layout)});
  }
  const refusal=await page.evaluate(async()=>{
    const original=Storage.prototype.setItem;
    Storage.prototype.setItem=function(){throw new DOMException('proof injected refusal','QuotaExceededError');};
    try {await window.kumihimo.adapter.save();return window.kumihimo.storage.last;} finally {Storage.prototype.setItem=original;}
  });
  assert(refusal?.ok===false,'injected storage refusal not surfaced');
  await capture('case004-fresh-reload');
  evidence.cases.push({id:'EVAL-KUM-013-004',stored:bytes,before:observed(savedState),fresh:observed(fresh),missing_provider:missingProvider,invalid,storage_refusal:{provenance:'injected Storage.prototype.setItem exception; not actual browser quota denial',result:refusal}});

  const modelBefore=await canonical();
  const large='9007199254740993';
  const envelope={version:1,route:'command',identity:modelBefore.rust.identity,actor:'browser-model-a',operation:large,expected_generation:modelBefore.rust.generation,action:{kind:'equalize',workspace:modelBefore.rust.root}};
  const accepted=await route(envelope); assert(accepted.ok,'large decimal operation rejected');
  assert(accepted.result.receipt.operation===large,'large operation precision lost');
  const next=(await canonical()).rust;
  const later=await route({version:1,route:'command',identity:next.identity,actor:'browser-model-a',operation:next.next_operation,expected_generation:next.generation,action:{kind:'viewport',rect:{x:next.frames[0].rect.x+1,y:next.frames[0].rect.y,width:1100,height:730},scale:1}});
  assert(later.ok,'later command rejected');
  const replay=await route(envelope);
  assert(replay.ok&&replay.result.receipt.replayed,'exact envelope did not replay');
  assert(replay.result.receipt.generation!==replay.result.current.generation,'old receipt substituted for current state');
  const conflict=await route({...envelope,actor:'browser-model-b'}); assert(!conflict.ok,'conflicting actor accepted');
  const mismatch=await route({...envelope,action:{kind:'maximize',frame:(await canonical()).rust.frames[0].id}}); assert(!mismatch.ok,'different intent accepted');
  const conflictCurrent=(await canonical()).rust;
  const staleEnvelope={...envelope,operation:conflictCurrent.next_operation,expected_generation:modelBefore.rust.generation};
  const rejected=await route(staleEnvelope); assert(!rejected.ok,'stale generation new operation accepted');
  const rejectedCurrent=(await canonical()).rust;
  assert(rejectedCurrent.high_watermark===staleEnvelope.operation,'rejected Core operation did not advance watermark');
  assert(rejectedCurrent.generation===conflictCurrent.generation&&membership(rejectedCurrent)===membership(conflictCurrent),'stale generation rejection changed arrangement');
  const rejectedRetry=await route(staleEnvelope); assert(!rejectedRetry.ok&&rejectedRetry.result.receipt.replayed,'rejected exact retry did not replay receipt');
  assert(rejectedRetry.result.receipt.generation===rejected.result.receipt.generation,'rejected retry replaced retained receipt');
  assert(JSON.stringify((await canonical()).rust)===JSON.stringify(rejectedCurrent),'rejected retry changed current Rust state');
  const unknown=await route({version:1,route:'snapshot',extra:'forbidden'}); assert(!unknown.ok,'unknown field accepted');
  const oversized=await route({version:1,route:'snapshot',extra:'x'.repeat(1100000)}); assert(!oversized.ok,'oversize accepted');
  const staleIdentity=await route({...envelope,operation:(await canonical()).rust.next_operation,identity:{...envelope.identity,incarnation:'stale-browser'}}); assert(!staleIdentity.ok,'stale incarnation accepted');
  evidence.cases.push({id:'EVAL-KUM-013-005',envelope,accepted,later,replay,conflict,mismatch,stale_envelope:staleEnvelope,raw_stale_envelope:JSON.stringify(staleEnvelope),rejected,rejected_current:rejectedCurrent,rejected_retry:rejectedRetry,unknown,oversized,stale_identity:staleIdentity,current:observed(await canonical())});

  await page.evaluate(async()=>{await window.kumihimo.adapter.refresh();await window.kumihimo.adapter.whenIdle();});
  const popupBefore=await canonical();
  const iframe=page.locator('#restricted-popup-frame');
  await page.locator('summary').click();
  const sandbox=await iframe.getAttribute('sandbox');
  assert(sandbox.includes('allow-scripts')&&sandbox.includes('allow-same-origin')&&!sandbox.includes('allow-popups'),'actual sandbox not restrictive');
  await page.frameLocator('#restricted-popup-frame').locator('#request-popup').click();
  await page.waitForFunction(()=>window.kumihimo.popup?.denied===true);
  const denial=await page.evaluate(()=>window.kumihimo.popup);
  assert(denial.denied===true&&(denial.result==='null'||denial.result==='exception'||denial.outcome==='null'||denial.outcome==='exception'),'actual popup null/exception absent');
  const popupAfter=await canonical();
  assert(membership(popupAfter.rust)===membership(popupBefore.rust),'popup denial migrated membership');
  assert(context.pages().length===1,'unexpected popup created');
  const accessibility=await bounds();
  const capabilities=await page.evaluate(()=>window.kumihimo.capabilities);
  assert(capabilities.rustWasm===true&&capabilities.nativePopupMigration==='unsupported','capability disclosure missing');
  assert(accessibility.controls.some(c=>c.role==='tab'&&c.selected!==null),'tab selected accessibility absent');
  assert(accessibility.controls.some(c=>c.expanded!==null),'expanded accessibility absent');
  assert(accessibility.controls.every(c=>c.name||c.id),'unlabelled controls');
  await capture('case006-restricted-popup');
  evidence.cases.push({id:'EVAL-KUM-013-006',sandbox,denial,before:observed(popupBefore),after:observed(popupAfter),accessibility,capabilities,quiet:{headless:true,private_profile:true,loopback:true,additional_pages:context.pages().length-1}});
  evidence.product_events=await page.evaluate(()=>window.kumihimo.events.slice(-120));
  assert(!evidence.errors.length,'browser page errors');
  assert(evidence.cases.length===6,'frozen case count');
  evidence.verdict='BROWSER_BATCH_PASSED_PENDING_IMAGE_INSPECTION';
}
(async()=>{
  try {await Promise.race([run(),new Promise((_,reject)=>{deadline=setTimeout(()=>reject(new Error('bounded browser batch deadline')),180000);})]);}
  catch(error){evidence.verdict='FAIL';evidence.failure=String(error).slice(0,1000);process.exitCode=1;await captureFailure();}
  finally {
    clearTimeout(deadline);
    if(context) {try {await context.close();evidence.owned_cleanup=true;}catch(error){evidence.owned_cleanup=false;evidence.cleanup_error=String(error).slice(0,300);process.exitCode=1;}}
    const output=JSON.stringify(evidence,null,2);
    if(Buffer.byteLength(output)>4*1024*1024){evidence.failure='Evidence exceeded 4MiB bound';evidence.verdict='FAIL';evidence.cases=[];process.exitCode=1;}
    fs.writeFileSync(path.join(config.out,'browser-evidence.json'),JSON.stringify(evidence,null,2));
    process.stdout.write(JSON.stringify({verdict:evidence.verdict,evidence:path.join(config.out,'browser-evidence.json'),screenshots:evidence.screenshots,owned_cleanup:evidence.owned_cleanup})+'\n');
  }
})();
'''


def main() -> None:
    project = Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--node", default=os.environ.get("KUMIHIMO_NODE"))
    parser.add_argument("--node-modules", type=Path, default=os.environ.get("KUMIHIMO_NODE_MODULES"))
    parser.add_argument("--browser", type=Path, default=os.environ.get("KUMIHIMO_BROWSER"))
    parser.add_argument("--out", type=Path, default=project / ".local/web/proof")
    parser.add_argument("--build-manifest", type=Path, default=project / ".local/web/build-manifest.json")
    args = parser.parse_args()
    url = urlsplit(args.url)
    if url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port or url.username or url.password:
        parser.error("proof URL must be explicit http://127.0.0.1:port/")
    if not args.node or not args.node_modules or not args.browser:
        parser.error("configure Node, node-modules and an existing browser executable")
    node = shutil.which(args.node) or args.node
    if not Path(node).is_file() or not args.browser.is_file():
        parser.error("configured Node/browser executable not found")
    if not (args.node_modules / "playwright/package.json").is_file():
        parser.error("configured node-modules has no Playwright; installation is not performed")
    out = args.out.resolve()
    if not out.is_relative_to((project / ".local").resolve()):
        parser.error("evidence output must be in this project ignored .local directory")
    build = args.build_manifest.resolve(strict=True)
    out.mkdir(parents=True, exist_ok=True)
    # Every run owns a fresh isolated profile. No operator browser is attached or stopped.
    import tempfile
    with tempfile.TemporaryDirectory(prefix="browser-profile-", dir=out) as profile:
        config = {"url": args.url, "modules": str(args.node_modules.resolve()), "browser": str(args.browser.resolve()), "out": str(out), "build": str(build), "profile": profile}
        script = Path(profile) / "browser-proof.cjs"
        script.write_text(SCRIPT, encoding="utf-8", newline="\n")
        result = subprocess.run([str(node), str(script)], input=json.dumps(config), text=True, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), check=False)
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
