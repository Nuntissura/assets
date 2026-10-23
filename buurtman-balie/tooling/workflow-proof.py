"""Observed workflow regressions through the actual temporary Rust service; no host data."""
import argparse
import asyncio
import hashlib
import json
import os
from pathlib import Path
import sys
import threading
import uuid
from standalone import serve
from browser_runtime import owned_browser

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / '.local/workflow-proof'


def fingerprints(args):
    paths = [ROOT/name for name in ['Cargo.toml','Cargo.lock','crates/buurtman-balie-host-demo/Cargo.toml',
             'tooling/workflow-proof.py','tooling/browser_runtime.py','tooling/standalone.py']]
    for folder,pattern in [('web','*'),('src','*.rs'),('crates/buurtman-balie-host-demo/examples','*.rs')]:
        paths.extend(path for path in (ROOT/folder).rglob(pattern) if path.is_file())
    binary = Path(os.environ.get('BB_DEMO_BINARY',ROOT/'target/debug/examples/json_bridge'))
    if os.name == 'nt' and not binary.suffix:
        binary=binary.with_suffix('.exe')
    paths.extend([binary,Path(args.browser),Path(args.runtime)/'playwright/__init__.py'])
    return {str(path.resolve()):hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(set(paths))}


def reject_drift(before,after,moment):
    if before == after:
        return
    differences={'added':sorted(after.keys()-before.keys()),'removed':sorted(before.keys()-after.keys()),
                 'changed':sorted(path for path in before.keys() & after.keys() if before[path]!=after[path])}
    (OUTPUT/'result.json').write_text(json.dumps({'status':'failed','source_hashes':before,
        'source_hashes_after':after,'input_drift':differences,'drift_moment':moment},indent=2),encoding='utf-8')
    raise RuntimeError('Proof inputs changed '+moment+'; result is stale: '+json.dumps(differences,ensure_ascii=True))


async def prove(args, url):
    sys.path.insert(0, str(Path(args.runtime).resolve()))
    from playwright.async_api import async_playwright
    OUTPUT.mkdir(parents=True, exist_ok=True)
    source_before=fingerprints(args)
    (OUTPUT/'result.json').write_text(json.dumps({'status':'running','source_hashes':source_before},indent=2),encoding='utf-8')
    async with async_playwright() as playwright:
        async with owned_browser(playwright,args.browser) as browser:
          try:
            page = await browser.new_page(viewport={'width':1280,'height':1000})
            page.set_default_timeout(10000)
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            executes = []
            page.on('request', lambda request: executes.append(request.post_data_json['request'])
                    if request.method == 'POST' and request.post_data_json.get('action') == 'execute' else None)
            await page.goto(url, wait_until='domcontentloaded')
            product = page.locator('#product-editor')
            search = page.locator('#search')
            async def select(name):
                await page.locator('#results button').filter(has_text=name).click()
                assert await product.get_by_label('Product name', exact=True).input_value() == name
            async def save():
                await product.get_by_role('button',name='Save product',exact=True).click()
                await page.locator('#product-status').filter(has_text='Product saved.').wait_for()
            for name,price in [('Camera Orion','80.00'),('Milk 1L','2.00')]:
                await page.get_by_role('button',name='New product',exact=True).click()
                for label,value in [('Product name',name),('Currency','EUR'),('B2C price','120.00'),('B2B price',price)]:
                    await product.get_by_label(label,exact=True).fill(value)
                await product.get_by_label('Tax basis',exact=True).select_option('Included')
                await save()
            await select('Camera Orion')
            await product.get_by_label('Description',exact=True).fill('Retained camera draft')
            await product.get_by_label('B2C price',exact=True).fill('-1')
            await select('Milk 1L')
            await select('Camera Orion')
            assert await product.get_by_label('Description',exact=True).input_value() == 'Retained camera draft'
            assert await product.get_by_label('B2C price',exact=True).input_value() == '-1'
            before = len(executes)
            await product.get_by_role('button',name='Save product',exact=True).click()
            await page.locator('#product-status').filter(has_text='Correct input: Use a nonnegative decimal price.').wait_for()
            assert len(executes) == before
            assert 'reconcile' not in await page.locator('#product-status').inner_text()
            await product.get_by_label('B2C price',exact=True).fill('120.00')
            await save()
            await page.get_by_role('button',name='New product',exact=True).click()
            await product.get_by_label('Product name',exact=True).fill('Unfinished lens')
            await product.get_by_label('Description',exact=True).fill('New draft retained')
            await select('Camera Orion')
            await page.get_by_role('button',name='New product',exact=True).click()
            assert await product.get_by_label('Product name',exact=True).input_value() == 'Unfinished lens'
            assert await product.get_by_label('Description',exact=True).input_value() == 'New draft retained'
            await select('Camera Orion')
            tier=page.locator('[data-form=tier]')
            for label,value in [('Tier key','employee'),('Tier name','Employee'),('Extra discount (%)','10')]:
                await tier.get_by_label(label,exact=True).fill(value)
            await tier.get_by_label('Base pricing',exact=True).select_option('B2B')
            await tier.get_by_role('button',name='Create pricing tier',exact=True).click()
            await page.get_by_text('Pricing tier created.',exact=True).wait_for()
            client=page.locator('[data-form=client]')
            await client.get_by_label('Client name',exact=True).fill('Camera employee')
            await client.get_by_label('Client status',exact=True).select_option('B2C')
            await client.get_by_label('Pricing tier',exact=True).select_option('employee')
            await client.get_by_role('button',name='Save client',exact=True).click()
            await page.get_by_text('Client saved · B2C · Employee',exact=True).wait_for()
            quote=page.locator('[data-form=quote]')
            await quote.get_by_label('Assigned client',exact=True).select_option(label='Camera employee · B2C · Employee')
            await quote.get_by_label('Priced product',exact=True).select_option(label='Camera Orion')
            async def calculate(amount):
                await quote.get_by_role('button',name='Calculate client price',exact=True).click()
                await page.locator('[data-quote]').filter(has_text='EUR '+amount).wait_for()
            await calculate('72.00')
            await select('Milk 1L')
            await product.get_by_label('Description',exact=True).fill('Unrelated update')
            await save()
            assert 'EUR 72.00' in await page.locator('[data-quote]').inner_text()
            await select('Camera Orion')
            await product.get_by_label('B2B price',exact=True).fill('100.00')
            await save()
            await page.get_by_text('Price preview changed. Calculate again.',exact=True).wait_for()
            assert '72.00' not in await page.locator('[data-quote]').inner_text()
            await calculate('90.00')

            # Commit the real request, discard only its response, then reconcile exact intent.
            lost = []
            async def lose_response(route):
                payload = route.request.post_data_json
                command = payload.get('request',{}).get('command',{})
                record = command.get('Update',{})
                if not lost and record.get('kind') == 'Product' and record.get('name') == 'Camera Orion':
                    lost.append(payload['request'])
                    await route.fetch()
                    await route.abort('failed')
                else:
                    await route.continue_()
            await page.route('**/api', lose_response)
            await product.get_by_label('Description',exact=True).fill('Committed but response lost')
            await product.get_by_role('button',name='Save product',exact=True).click()
            await page.locator('#product-status').filter(has_text='Retry to reconcile.').wait_for()
            await select('Milk 1L')
            await select('Camera Orion')
            assert await product.get_by_label('Description',exact=True).input_value() == 'Committed but response lost'
            await product.get_by_label('Description',exact=True).fill('Changed after uncertainty')
            before=len(executes)
            await product.get_by_role('button',name='Save product',exact=True).click()
            await page.get_by_text('Previous save confirmed. Save your current changes.',exact=True).wait_for()
            assert executes[before] == lost[0]
            assert await product.get_by_label('Description',exact=True).input_value() == 'Changed after uncertainty'
            await page.unroute('**/api', lose_response)
            await save()
            assert executes[-1]['command']['Update']['revision'] == lost[0]['command']['Update']['revision'] + 1

            # A second writer advances canonical state while this editor retains its revision.
            async def api(payload):
                return await page.evaluate('async payload => (await (await fetch("/api",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(payload)})).json())',payload)
            loaded=await api({'action':'search','kind':'Product','query':'Camera Orion','fields':{}})
            camera=next(record for record in loaded['value']['records'] if record['name']=='Camera Orion')
            camera['description']='Independent writer update'
            committed=await api({'action':'execute','request':{'operation_id':str(uuid.uuid4()),'command':{'Update':camera}}})
            assert 'value' in committed,committed
            await product.get_by_label('Description',exact=True).fill('Draft after conflicting writer')
            await product.get_by_role('button',name='Save product',exact=True).click()
            await page.locator('#product-status').filter(has_text='Save rejected: search and select').wait_for()
            assert 'reconcile' not in await page.locator('#product-status').inner_text()
            async with page.expect_response(lambda response: response.request.method=='POST' and response.request.post_data_json.get('action')=='search' and response.request.post_data_json.get('kind')=='Product') as response_info:
                await search.get_by_role('button',name='Search',exact=True).click()
            await (await response_info.value).json()
            await select('Camera Orion')
            assert await product.get_by_label('Description',exact=True).input_value() == 'Draft after conflicting writer'
            await page.get_by_text('Record reloaded. Review your retained draft before saving.',exact=True).wait_for()
            await save()

            # An old successful quote response cannot overwrite invalidation while saving.
            ready, release = asyncio.Event(), asyncio.Event()
            async def delay_quote(route):
                if route.request.post_data_json.get('action') == 'quote':
                    response=await route.fetch()
                    ready.set()
                    await release.wait()
                    await route.fulfill(response=response)
                else: await route.continue_()
            await page.route('**/api', delay_quote)
            await quote.get_by_role('button',name='Calculate client price',exact=True).click()
            await asyncio.wait_for(ready.wait(),timeout=10)
            await product.get_by_label('B2B price',exact=True).fill('110.00')
            await save()
            release.set()
            await quote.get_by_role('button',name='Calculate client price',exact=True).wait_for(state='visible')
            await page.wait_for_function('!document.querySelector("[data-form=quote] button").disabled')
            assert 'Calculate again.' in await page.locator('[data-quote]').inner_text()
            assert 'EUR 90.00' not in await page.locator('[data-quote]').inner_text()
            await page.unroute('**/api', delay_quote)
            await calculate('99.00')
            await quote.get_by_label('Priced product',exact=True).select_option(label='Milk 1L')
            assert 'EUR 99.00' not in await page.locator('[data-quote]').inner_text()
            await quote.get_by_label('Priced product',exact=True).select_option(label='Camera Orion')
            await calculate('99.00')
            await page.locator('[data-clients] button').filter(has_text='Camera employee').click()
            await client.get_by_label('Client status',exact=True).select_option('B2B')
            await client.get_by_role('button',name='Save client',exact=True).click()
            await page.get_by_text('Client saved · B2B · Employee',exact=True).wait_for()
            assert 'EUR 99.00' not in await page.locator('[data-quote]').inner_text()

            # Client conflicts must reload canonical revision without an unavailable search control.
            loaded_clients=await api({'action':'search','kind':'Customer','query':'','fields':{}})
            current_client=next(item for item in loaded_clients['value']['records'] if item['name']=='Camera employee')
            current_client['description']='Independent client writer'
            committed_client=await api({'action':'execute','request':{'operation_id':str(uuid.uuid4()),'command':{'Update':current_client}}})
            assert 'value' in committed_client,committed_client
            await client.get_by_label('Client name',exact=True).fill('Retained employee draft')
            await client.get_by_role('button',name='Save client',exact=True).click()
            await page.get_by_text('Record reloaded. Review your retained draft before saving.',exact=True).wait_for()
            assert await client.get_by_label('Client name',exact=True).input_value()=='Retained employee draft'
            await client.get_by_role('button',name='Save client',exact=True).click()
            await page.get_by_text('Client saved · B2B · Employee',exact=True).wait_for()
            reloaded_clients=await api({'action':'search','kind':'Customer','query':'','fields':{}})
            reconciled_client=next(item for item in reloaded_clients['value']['records'] if item['id']==current_client['id'])
            assert reconciled_client['name']=='Retained employee draft'
            assert reconciled_client['description']=='Independent client writer'
            assert reconciled_client['revision']==committed_client['value']['record_revision']+1

            # The public reusable module supports live locale changes for new feedback.
            await page.evaluate("""async () => {
              const {createLocalization}=await import('./localization.js');
              const keys=['Correct input: {error}','Save rejected: {error}','Save not confirmed: {error}. Retry to reconcile.', 'Price preview changed. Calculate again.'];
              window.workflowTranslations=['en','fr','nl'].map(language=>{const locale=createLocalization(language);return {language,phrases:keys.map(key=>locale.text(key,{error:'X'}))};});
            }""")
            translations=await page.evaluate('workflowTranslations')
            assert len(set(row['phrases'][3] for row in translations)) == 3
            await page.screenshot(path=str(OUTPUT/'desktop.png'),full_page=True)
            await page.set_viewport_size({'width':390,'height':844})
            await page.screenshot(path=str(OUTPUT/'mobile.png'),full_page=True)
            assert not await page.evaluate('document.documentElement.scrollWidth > innerWidth')
            assert not errors,errors
            result={'record_and_new_drafts_retained':True,'local_invalid_price_no_dispatch':True,
                    'unrelated_update_preserves_quote':True,'relevant_update_invalidates_quote':True,
                    'employee_quote_72_90_99':True,'uncertain_exact_request_replay_after_switch_and_edit':True,
                    'late_quote_discarded':True,'quote_selection_invalidates':True,'translations':translations,
                    'confirmed_conflict_rejected_and_retained_draft_reloaded':True,'client_revision_invalidates_quote':True,
                    'client_conflict_canonical_reload_retains_draft':True,
                    'page_errors':errors,'backend':'actual Rust CRM / temporary MemoryStore',
                    'source_hashes':source_before}
          except Exception:
            if 'page' in locals():
                await page.screenshot(path=str(OUTPUT/'failure.png'),full_page=True,timeout=10000)
                (OUTPUT/'failure.html').write_text(await page.content(),encoding='utf-8')
            raise
    after=fingerprints(args)
    reject_drift(source_before,after,'during execution')
    result.update(status='behavior-observed',source_hashes_after=after)
    return result


if __name__ == '__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--runtime',required=True)
    parser.add_argument('--browser',required=True)
    args=parser.parse_args()
    OUTPUT.mkdir(parents=True,exist_ok=True)
    (OUTPUT/'result.json').write_text(json.dumps({'status':'running'}),encoding='utf-8')
    server=child=None
    try:
        try:
            server,child=serve()
            threading.Thread(target=server.serve_forever,daemon=True).start()
            result=asyncio.run(prove(args,f'http://127.0.0.1:{server.server_port}'))
        finally:
            if server is not None:
                server.shutdown();server.server_close()
            if child is not None:
                child.stdin.close()
                if child.poll() is None:child.terminate()
                child.wait(timeout=5)
        after=fingerprints(args)
        reject_drift(result['source_hashes'],after,'during cleanup')
        result.update(status='passed',source_hashes_after=after)
        (OUTPUT/'result.json').write_text(json.dumps(result,indent=2,ensure_ascii=False),encoding='utf-8')
        print(json.dumps(result,ensure_ascii=True),flush=True)
    except BaseException as error:
        OUTPUT.mkdir(parents=True,exist_ok=True)
        previous=json.loads((OUTPUT/'result.json').read_text(encoding='utf-8')) if (OUTPUT/'result.json').exists() else {}
        previous.update(status='failed',error=repr(error))
        (OUTPUT/'result.json').write_text(json.dumps(previous,indent=2),encoding='utf-8')
        raise
