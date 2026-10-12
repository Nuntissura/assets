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
async function capture(name) {
  assert(evidence.screenshots.length<8,'capture count bound');
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
  await page.locator('[data-action="move-frame"]').click(); await idle();
  const parentMove=await page.evaluate(()=>window.kumihimo.adapter.lastCommand);
  assert(parentMove?.request.action.kind==='move_frame'&&parentMove.request.action.destination===eligibleDestination&&parentMove.response.ok===true&&parentMove.response.result.receipt.ok===true,'actual parent move route rejected');
  assert((await canonical()).rust.frames.find(f=>f.id===first.id).workspace===eligibleDestination,'parent move click did not commit');
  await page.locator('#destination-select').selectOption(gestureBefore.rust.root);
  await page.locator('[data-action="move-frame"]').click(); await idle();
  const returnMove=await page.evaluate(()=>window.kumihimo.adapter.lastCommand);
  assert(returnMove?.request.action.kind==='move_frame'&&returnMove.request.action.destination===gestureBefore.rust.root&&returnMove.response.ok===true&&returnMove.response.result.receipt.ok===true,'actual return parent move route rejected');
  assert((await canonical()).rust.frames.find(f=>f.id===first.id).workspace===gestureBefore.rust.root,'return parent move click did not commit');
  await page.locator('#dock-edge').selectOption('Left');
  await page.locator('[data-action="dock"]').click(); await idle();
  const slider=page.locator('#resize-weight');
  await slider.focus(); await page.keyboard.press('ArrowRight'); await page.keyboard.press('Enter'); await idle();
  const handles=page.locator('[data-action="resize-split"]');
  if(await handles.count()) { const b=await handles.first().boundingBox(); if(b){await page.mouse.move(b.x+b.width/2,b.y+b.height/2);await page.mouse.down();await page.mouse.move(b.x+b.width/2+16,b.y+b.height/2,{steps:3});await page.mouse.up();await idle();} }
  const afterGesture=await canonical();
  assert(JSON.stringify(tasks(afterGesture.host))===JSON.stringify(tasks(gestureBefore.host)),'arrangement altered host task meaning');
  const stale=await route({version:1,route:'preview',identity:gestureBefore.rust.identity,expected_generation:gestureBefore.rust.generation,action:{kind:'equalize',workspace:gestureBefore.rust.root}});
  assert(stale.ok===false,'stale generation preview accepted');
  await capture('case002-interactions');
  evidence.cases.push({id:'EVAL-KUM-013-002',before:observed(gestureBefore),after:observed(afterGesture),parent_move:parentMove,return_move:returnMove,stale,event_entry:'actual mouse/keyboard/controls'});

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
  await capture('case003-provider-recovery');
  await page.locator('[data-action="provider-toggle"]').click(); await idle();
  evidence.cases.push({id:'EVAL-KUM-013-003',before:observed(taskBefore),edited:observed(edited),domain:observed(domain),wrong_recipient:wrongRecipient,stale_host:staleHost,unavailable:observed(unavailable)});

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
  assert(JSON.stringify(tasks(fresh.host))===JSON.stringify(tasks(savedState.host)),'host store lost');
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
  evidence.cases.push({id:'EVAL-KUM-013-004',stored:bytes,before:observed(savedState),fresh:observed(fresh),invalid,storage_refusal:{provenance:'injected Storage.prototype.setItem exception; not actual browser quota denial',result:refusal}});

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
  catch(error){evidence.verdict='FAIL';evidence.failure=String(error).slice(0,1000);process.exitCode=1;}
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
        result = subprocess.run([str(node), "-e", SCRIPT], input=json.dumps(config), text=True, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), check=False)
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
