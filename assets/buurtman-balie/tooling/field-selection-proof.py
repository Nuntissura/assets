"""Label-first field selection/reconciliation proof against isolated actual MemoryStore."""
import argparse
import asyncio
import hashlib
import json
import os
import subprocess
from pathlib import Path
import sys
import threading
import uuid
from standalone import serve
from browser_runtime import owned_browser

ROOT=Path(__file__).resolve().parents[1]
OUTPUT=ROOT/'.local/field-selection-proof'


def fingerprints(args):
    paths=[ROOT/name for name in ['Cargo.toml','Cargo.lock','crates/buurtman-balie-host-demo/Cargo.toml',
           'tooling/field-selection-proof.py','tooling/browser_runtime.py','tooling/standalone.py']]
    for folder,pattern in [('web','*'),('src','*.rs'),('crates/buurtman-balie-host-demo/examples','*.rs')]:
        paths.extend(path for path in (ROOT/folder).rglob(pattern) if path.is_file())
    binary=Path(os.environ.get('BB_DEMO_BINARY',ROOT/'target/debug/examples/json_bridge'))
    if os.name=='nt' and not binary.suffix:binary=binary.with_suffix('.exe')
    paths.extend([binary,Path(args.browser),Path(args.runtime)/'playwright/__init__.py'])
    return {str(path.resolve()):hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(set(paths))}


async def mount(page,initial_failure=False):
    await page.evaluate("""async initialFailure => {
      const {mountFieldEditor}=await import('./field-editor.js');
      const {createLocalization}=await import('./localization.js');
      window.proofCall=async payload=>{
        const response=await fetch('/api',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)});
        const result=await response.json();if(!response.ok || result.error)throw new Error(result.error || 'Request failed');return result.value;
      };
      window.proofLocale=createLocalization('en');window.proofCalls=[];window.proofChanges=[];window.proofSettled=0;
      window.proofFaults={list:initialFailure,mode:'normal',resolve:null,delayThenFail:false};
      window.proofAdapter={
        listFields:async()=>{if(proofFaults.list)throw new Error('Storage("catalogue unavailable")');return proofCall({action:'fields'});},
        defineField:async(definition,operation_id)=>{
          const request={operation_id,command:{DefineField:JSON.parse(JSON.stringify(definition))}};proofCalls.push(request);
          try {
          const value=await proofCall({action:'execute',request});
          if(proofFaults.mode==='loss'){proofFaults.mode='normal';throw new Error('Storage("response lost after commit")');}
          if(proofFaults.mode==='delay'){proofFaults.mode='normal';await new Promise(resolve=>proofFaults.resolve=resolve);}
          if(proofFaults.delayThenFail){proofFaults.delayThenFail=false;proofFaults.list=true;}
          if(proofFaults.mode==='refresh-failure'){proofFaults.mode='normal';proofFaults.list=true;}
          return value;
          } finally {proofSettled++;}
        }
      };
      let root=document.getElementById('proof-editor');if(!root){root=document.createElement('div');root.id='proof-editor';document.querySelector('main').prepend(root);}
      window.proofEditor=mountFieldEditor(root,proofAdapter,{localization:proofLocale,onChange:fields=>proofChanges.push(JSON.parse(JSON.stringify(fields)))});
      try {await proofEditor.refresh();} catch(error) {if(!initialFailure)throw error;}
    }""",initial_failure)


async def proof(args,urls,output):
    sys.path.insert(0,str(Path(args.runtime).resolve()))
    from playwright.async_api import async_playwright
    async with async_playwright() as playwright:
        async with owned_browser(playwright,args.browser) as browser:
            page=await browser.new_page(viewport={'width':1280,'height':1100})
            page.set_default_timeout(10000)
            errors=[];page.on('pageerror',lambda error:errors.append(str(error)))
            try:
                await page.goto(urls[0],wait_until='domcontentloaded')
                await mount(page,initial_failure=True)
                root=page.locator('#proof-editor')
                form=root.locator('form[data-testid=field-editor]')
                selector=form.locator('[name=existing]')
                name=form.locator('[name=label]')
                key=form.locator('[name=key]')
                button=form.locator('button[type=button]:not([data-testid=field-refresh])')
                refresh_button=form.locator('[data-testid=field-refresh]')
                status=root.locator('[role=status]')
                async def call_count():return await page.evaluate('proofCalls.length')
                async def canonical():return await page.evaluate('proofCall({action:"fields"})')
                async def advanced():
                    if not await form.locator('[data-testid=field-advanced]').evaluate('node=>node.open'):
                        await form.locator('[data-testid=field-advanced] summary').click()
                async def new_mode(label):
                    await selector.select_option('@create-new')
                    if await form.locator('[name=keyOverride]').is_checked():
                        await advanced();await form.locator('[name=keyOverride]').uncheck()
                    await name.fill(label)
                async def create(label,expected_key):
                    await new_mode(label)
                    assert await key.input_value()==expected_key
                    assert expected_key in await form.locator('[data-testid=field-key-preview]').text_content()
                    count=await call_count()
                    await button.click()
                    await root.locator('[data-field-key="'+expected_key+'"]').wait_for()
                    await page.wait_for_function('!document.querySelector("#proof-editor form button").disabled')
                    assert await call_count()==count+1
                    record=next(field for field in await canonical() if field['key']==expected_key)
                    assert record['label']==label.strip()
                    return record
                # Recover a failed initial catalogue using the user-visible control, no remount.
                await status.filter(has_text='Fields unavailable:').wait_for()
                assert await button.is_disabled() and await refresh_button.is_enabled()
                await name.fill('Retained startup draft')
                await form.locator('[name=type]').select_option('Boolean')
                await form.locator('[name=kind][value=Customer]').check()
                await advanced();await form.locator('[name=keyOverride]').check();await key.fill('startup.custom')
                await page.evaluate('proofFaults.list=false')
                await refresh_button.click()
                await status.filter(has_text='Fields refreshed.').wait_for()
                assert await name.input_value()=='Retained startup draft' and await key.input_value()=='startup.custom'
                assert await form.locator('[name=type]').input_value()=='Boolean'
                assert await form.locator('[name=kind][value=Customer]').is_checked()
                await button.click()
                await root.locator('[data-field-key="startup.custom"]').wait_for()
                await page.wait_for_function('!document.querySelector("#proof-editor form button").disabled')
                await page.evaluate('proofCall({action:"execute",request:{operation_id:crypto.randomUUID(),command:{DefineField:{key:"e",label:"Existing E",value_type:"Integer",applies_to:["Customer"]}}}})')
                await refresh_button.click()
                await status.filter(has_text='Fields refreshed.').wait_for()
                count=await call_count()
                await selector.select_option('e')
                assert await call_count()==count
                info=root.locator('[data-testid=field-definition]')
                assert await info.inner_text()=='Existing E · Integer · Customer · Canonical key: e'
                assert await info.locator('input,select,textarea').count()==0
                assert await name.is_hidden() and not await button.is_visible()
                assert await form.locator('[name=type]').is_disabled()
                assert await form.locator('[name=kind][value=Customer]').is_disabled()

                # Cross-kind canonical collision: e is Customer-only, still occupies the key.
                await create('é','e-2')
                await create('e\u0301','e-3')
                await create('Équipements photos','equipements-photos')
                await create('📷✨','field')
                await create('日本語','field-2')
                await create('A'*128,'a'*120)
                await create('A'*128,'a'*120+'-2')
                for field in await canonical():
                    assert len(field['key'].encode('ascii'))<=128
                await selector.select_option('a'*120)
                await page.set_viewport_size({'width':390,'height':844})
                assert not await page.evaluate('document.documentElement.scrollWidth > innerWidth')
                await page.screenshot(path=str(output/'long-key-mobile.png'),full_page=True)
                await page.set_viewport_size({'width':1280,'height':1100})

                await new_mode('Custom override')
                await advanced()
                await form.locator('[name=keyOverride]').check()
                await key.fill('Custom_KEY.v1:slot')
                await button.click()
                await root.locator('[data-field-key="Custom_KEY.v1:slot"]').wait_for()
                await page.wait_for_function('!document.querySelector("#proof-editor form button").disabled')
                assert next(field for field in await canonical() if field['label']=='Custom override')['key']=='Custom_KEY.v1:slot'
                await new_mode('Invalid override')
                await advanced();await form.locator('[name=keyOverride]').check()
                for invalid in ['bad key','é','']:
                    await key.fill(invalid);count=await call_count();await button.click()
                    await status.filter(has_text='Field key must use letters').wait_for()
                    assert await call_count()==count and await key.input_value()==invalid
                await key.fill('e');count=await call_count();await button.click()
                await status.filter(has_text='A field with this key already exists.').wait_for()
                assert await call_count()==count and await name.input_value()=='Invalid override'
                await new_mode('Empty scopes')
                await form.locator('[name=kind][value=Product]').uncheck()
                count=await call_count();await button.click()
                await status.filter(has_text='Select at least one record type.').wait_for()
                assert await call_count()==count
                await form.locator('[name=kind][value=Product]').check()
                await name.fill('B'*129);count=await call_count();await button.click()
                await status.filter(has_text='Field name is too long.').wait_for()
                assert await call_count()==count

                # Concurrent backend writer races the stale catalogue; confirmed Conflict keeps draft.
                await new_mode('Race collision')
                assert await key.input_value()=='race-collision'
                await page.evaluate('proofCall({action:"execute",request:{operation_id:crypto.randomUUID(),command:{DefineField:{key:"race-collision",label:"Other writer",value_type:"Text",applies_to:["Product"]}}}})')
                count=await call_count();await button.click()
                await status.filter(has_text='A field with this key already exists.').wait_for()
                await page.wait_for_function('!document.querySelector("#proof-editor form button").disabled')
                assert await call_count()==count+1 and await name.input_value()=='Race collision'
                assert 'not confirmed' not in await status.inner_text()
                await refresh_button.click()
                await status.filter(has_text='Fields refreshed.').wait_for()
                assert await name.input_value()=='Race collision' and await key.input_value()=='race-collision-2'
                await create('Race collision','race-collision-2')

                # Commit/lose response; edited locale/invalid draft/existing-mode cannot alter replay.
                await new_mode('Uncertain é label')
                await form.locator('[name=type]').select_option('Integer')
                await page.evaluate('proofFaults.mode="loss"')
                count=await call_count();await button.click()
                await status.filter(has_text='Retry to reconcile.').wait_for()
                immutable=await page.evaluate('proofCalls[proofCalls.length-1]')
                await name.fill('Changed draft 🌈')
                await advanced();await form.locator('[name=keyOverride]').check();await key.fill('invalid key')
                await page.evaluate('proofLocale.setLanguage("fr")')
                assert await name.input_value()=='Changed draft 🌈' and await key.input_value()=='invalid key'
                await page.evaluate('proofFaults.list=true')
                await refresh_button.click()
                await page.wait_for_function('!document.querySelector("#proof-editor [data-testid=field-refresh]").disabled')
                assert await button.is_enabled() and await name.input_value()=='Changed draft 🌈'
                await page.evaluate('proofFaults.list=false')
                await refresh_button.click()
                await page.wait_for_function('!document.querySelector("#proof-editor [data-testid=field-refresh]").disabled')
                assert await key.input_value()=='invalid key'
                await selector.select_option('e')
                await button.click()
                await page.wait_for_function('proofCalls.length > '+str(count+1)+' && !document.querySelector("#proof-editor form button").disabled')
                assert await page.evaluate('proofCalls[proofCalls.length-1]')==immutable
                assert await selector.input_value()=='e'
                await selector.select_option('@create-new')
                assert await name.input_value()=='Changed draft 🌈' and await key.input_value()=='invalid key'
                assert await form.locator('[name=type]').input_value()=='Integer'
                await page.evaluate('proofLocale.setLanguage("en")')
                await status.filter(has_text='Previous save confirmed. Save your current changes.').wait_for()
                old=next(field for field in await canonical() if field['key']=='uncertain-e-label')
                assert old['label']=='Uncertain é label' and old['value_type']=='Integer'
                await advanced();await form.locator('[name=keyOverride]').uncheck()
                await button.click()
                await root.locator('[data-field-key=changed-draft]').wait_for()
                await page.wait_for_function('!document.querySelector("#proof-editor form button").disabled')

                # Edits during successful earlier dispatch must remain an unsent new draft.
                await new_mode('Delayed field')
                await form.locator('[name=type]').select_option('Boolean')
                await form.locator('[name=kind][value=Distributor]').check()
                await page.evaluate('proofFaults.mode="delay"')
                await button.click()
                await page.wait_for_function('typeof proofFaults.resolve === "function"')
                await name.fill('Newer edited field')
                await page.evaluate('proofLocale.setLanguage("nl");proofFaults.resolve();proofFaults.resolve=null')
                await page.wait_for_function('!document.querySelector("#proof-editor form button").disabled')
                assert await name.input_value()=='Newer edited field'
                assert await form.locator('[name=type]').input_value()=='Boolean'
                assert await form.locator('[name=kind][value=Distributor]').is_checked()
                assert next(field for field in await canonical() if field['key']=='delayed-field')['label']=='Delayed field'
                assert not any(field['key']=='newer-edited-field' for field in await canonical())
                await page.evaluate('proofLocale.setLanguage("en")')
                await status.filter(has_text='Previous save confirmed. Save your current changes.').wait_for()

                await new_mode('Refresh recovery')
                await page.evaluate('proofFaults.mode="delay";proofFaults.delayThenFail=true')
                count=await call_count();await button.click()
                await page.wait_for_function('typeof proofFaults.resolve === "function"')
                await name.fill('Retained after refresh failure')
                await form.locator('[name=type]').select_option('Integer')
                await page.evaluate('proofFaults.resolve();proofFaults.resolve=null')
                await status.filter(has_text='Field created. Catalogue refresh failed:').wait_for()
                assert await button.is_disabled() and await call_count()==count+1
                assert any(field['key']=='refresh-recovery' for field in await canonical())
                assert 'Retry to reconcile' not in await status.inner_text()
                await page.evaluate('proofFaults.list=false')
                await refresh_button.click()
                await status.filter(has_text='Fields refreshed.').wait_for()
                assert not await button.is_disabled()
                assert await name.input_value()=='Retained after refresh failure'
                assert await form.locator('[name=type]').input_value()=='Integer'
                await button.click()
                await root.locator('[data-field-key=retained-after-refresh-failure]').wait_for()
                await page.wait_for_function('!document.querySelector("#proof-editor form button").disabled')

                # Each locale preserves canonical selection and editable draft; inspect actual screenshots.
                names={'en':'Field name','fr':'Nom du champ','nl':'Veldnaam'}
                selectors={'en':'Choose a field','fr':'Choisissez un champ','nl':'Kies een veld'}
                for language in ['en','fr','nl']:
                    await page.evaluate('language=>proofLocale.setLanguage(language)',language)
                    await selector.select_option('e')
                    count=await call_count()
                    assert await root.get_by_label(selectors[language],exact=True).input_value()=='e'
                    assert 'Existing E' in await info.inner_text()
                    assert await call_count()==count
                    await selector.select_option('@create-new')
                    await root.get_by_label(names[language],exact=True).fill('Stable é camera label')
                    assert await key.input_value()=='stable-e-camera-label'
                    await page.set_viewport_size({'width':1280,'height':1100})
                    await page.screenshot(path=str(output/(language+'-desktop.png')),full_page=True)
                    await page.set_viewport_size({'width':390,'height':844})
                    await page.screenshot(path=str(output/(language+'-mobile.png')),full_page=True)
                    assert not await page.evaluate('document.documentElement.scrollWidth > innerWidth')

                # Disposal ignores late confirmation; a new business demo has its own empty catalogue.
                await page.evaluate('proofLocale.setLanguage("en")')
                await new_mode('Dispose delayed')
                await page.evaluate('proofFaults.mode="delay"')
                await button.click()
                await page.wait_for_function('typeof proofFaults.resolve === "function"')
                change_count=await page.evaluate('proofChanges.length')
                settled=await page.evaluate('proofSettled')
                await page.evaluate('proofEditor.dispose();proofLocale.setLanguage("fr");proofFaults.resolve();proofFaults.resolve=null')
                assert await root.locator('form').count()==0
                await page.wait_for_function('proofSettled > '+str(settled))
                assert await page.evaluate('proofChanges.length')==change_count
                assert any(field['key']=='dispose-delayed' for field in await canonical())
                second=await browser.new_page(viewport={'width':390,'height':844})
                second.set_default_timeout(10000)
                second.on('pageerror',lambda error:errors.append(str(error)))
                await second.goto(urls[1],wait_until='domcontentloaded');await mount(second)
                assert await second.evaluate('proofCalls.length')==0
                assert await second.evaluate('proofCall({action:"fields"})')==[]
                assert await second.locator('#proof-editor [name=label]').input_value()==''
                assert await second.locator('#proof-editor [name=existing] option').count()==1
                assert not errors,errors
                result={'backend':'two isolated actual Rust MemoryStore services','existing_selection_zero_define_calls':True,
                        'readonly_type_scopes_identity':True,'unicode_combining_emoji_long_collision_keys':True,
                        'cross_kind_canonical_collision':True,'valid_and_invalid_advanced_overrides':True,
                        'empty_scopes_and_long_label_no_dispatch':True,'confirmed_backend_collision_keeps_draft':True,
                        'uncertain_exact_receipt_after_label_locale_and_mode_edits':True,
                        'earlier_confirmation_preserves_later_draft':True,'confirmed_write_refresh_failure_distinct':True,
                        'visible_refresh_initial_failure_preserves_draft_and_creation':True,
                        'visible_refresh_unknown_pending_preserves_exact_retry':True,
                        'visible_refresh_postcommit_failure_retains_newer_draft_and_creation':True,
                        'en_fr_nl_desktop_mobile':True,'disposal_and_isolated_workspace_remount':True,'page_errors':errors}
                return result
            except Exception:
                await page.screenshot(path=str(output/'failure.png'),full_page=True,timeout=10000)
                (output/'failure.html').write_text(await page.content(),encoding='utf-8')
                raise


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--runtime',required=True);parser.add_argument('--browser',required=True)
    args=parser.parse_args()
    output=OUTPUT/'runs'/uuid.uuid4().hex;output.mkdir(parents=True,exist_ok=False)
    latest=OUTPUT/'result.json'
    if latest.exists():(output/'prior-result.json').write_bytes(latest.read_bytes())
    latest.write_text(json.dumps({'status':'running','artifact_directory':str(output)},indent=2),encoding='utf-8')
    services=[]
    try:
        source_before=fingerprints(args)
        try:
            for _ in range(2):
                server,child=serve();services.append((server,child))
                threading.Thread(target=server.serve_forever,daemon=True).start()
            urls=[f'http://127.0.0.1:{server.server_port}' for server,_ in services]
            result=asyncio.run(proof(args,urls,output))
        finally:
            cleanup_errors=[]
            for server,child in reversed(services):
                try:
                    server.shutdown();server.server_close();child.stdin.close()
                    if child.poll() is None:child.terminate()
                    try:child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill();child.wait(timeout=5)
                except Exception as error:cleanup_errors.append(repr(error))
            if cleanup_errors:raise RuntimeError('Owned field-proof service cleanup failed: '+json.dumps(cleanup_errors))
        source_after=fingerprints(args)
        if source_before!=source_after:
            result={'status':'failed','source_hashes':source_before,'source_hashes_after':source_after,
                    'input_drift':{'added':sorted(source_after.keys()-source_before.keys()),'removed':sorted(source_before.keys()-source_after.keys()),
                        'changed':sorted(path for path in source_before.keys() & source_after.keys() if source_before[path]!=source_after[path])}}
            raise RuntimeError('Field-selection proof inputs changed during run or teardown')
        result.update(status='passed',artifact_directory=str(output),source_hashes=source_before,source_hashes_after=source_after)
        encoded=json.dumps(result,indent=2,ensure_ascii=True)
        latest.write_text(encoded,encoding='utf-8');(output/'result.json').write_text(encoded,encoding='utf-8')
        print(json.dumps(result,ensure_ascii=True),flush=True)
    except BaseException as error:
        if 'result' not in locals():result={}
        result.update(status='failed',artifact_directory=str(output),error=repr(error))
        if 'source_before' in locals():result['source_hashes']=source_before
        encoded=json.dumps(result,indent=2,ensure_ascii=True)
        latest.write_text(encoded,encoding='utf-8');(output/'result.json').write_text(encoded,encoding='utf-8')
        raise
