"""Real browser -> harness -> Rust service field/filter proof; ephemeral local data only."""
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
OUTPUT = ROOT / '.local/ui-proof'


def fingerprints(args):
    paths = [ROOT/name for name in ['Cargo.toml','Cargo.lock','crates/buurtman-balie-host-demo/Cargo.toml',
             'tooling/browser-proof.py','tooling/browser_runtime.py','tooling/standalone.py']]
    for folder,pattern in [('web','*'),('src','*.rs'),('crates/buurtman-balie-host-demo/examples','*.rs')]:
        paths.extend(path for path in (ROOT/folder).rglob(pattern) if path.is_file())
    binary = Path(os.environ.get('BB_DEMO_BINARY', ROOT/'target/debug/examples/json_bridge'))
    if os.name == 'nt' and not binary.suffix:
        binary = binary.with_suffix('.exe')
    paths.extend([binary,Path(args.browser),Path(args.runtime)/'playwright/__init__.py'])
    return {str(path.resolve()):hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(set(paths))}


async def proof(args, url, output, before):
    sys.path.insert(0, str(Path(args.runtime).resolve()))
    from playwright.async_api import async_playwright
    async with async_playwright() as p:
        async with owned_browser(p,args.browser) as browser:
          try:
            page = await browser.new_page(viewport={'width': 1280, 'height': 1100})
            page.set_default_timeout(10000)
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            await page.goto(url, wait_until='domcontentloaded')
            editor = page.locator('[data-testid=field-editor]')
            await editor.get_by_label('Field name', exact=True).fill('Brand')
            await editor.get_by_role('button', name='Create field').click()
            await page.get_by_text('Field created. Available for records and filters.', exact=True).wait_for()
            assert await page.locator('[data-field-key=brand]').count() == 1
            product = page.locator('#product-editor')
            for name, brand in [('Camera Alpha', 'Alpha'), ('Camera Beta', 'Beta')]:
                await page.get_by_role('button', name='New product', exact=True).click()
                await product.get_by_label('Product name', exact=True).fill(name)
                await product.get_by_label('Brand', exact=True).fill(brand)
                await product.get_by_role('button', name='Save product').click()
                await page.get_by_text('Product saved.', exact=True).wait_for()
            search = page.locator('#search')
            await search.get_by_label('Filter field', exact=True).select_option('brand')
            await search.get_by_label('Brand', exact=True).fill('Alpha')
            await search.get_by_role('button', name='Search', exact=True).click()
            await page.get_by_text('1 product loaded.', exact=True).wait_for()
            assert await page.locator('#results button').count() == 1
            assert 'Camera Alpha' in await page.locator('#results').inner_text()
            assert 'Camera Beta' not in await page.locator('#results').inner_text()
            await page.locator('#results button').click()
            assert await product.get_by_label('Brand', exact=True).input_value() == 'Alpha'
            await product.get_by_label('Brand', exact=True).fill('Gamma')
            await product.get_by_role('button', name='Save product').click()
            await page.get_by_text('0 products loaded.', exact=True).wait_for()
            await search.get_by_label('Brand', exact=True).fill('Gamma')
            await search.get_by_role('button', name='Search', exact=True).click()
            await page.get_by_text('1 product loaded.', exact=True).wait_for()
            await editor.locator('[data-testid=field-advanced] summary').click()
            await editor.get_by_label('Use a custom key', exact=True).check()
            await editor.get_by_label('Field key', exact=True).fill('brand')
            await editor.get_by_label('Field name', exact=True).fill('Conflicting name')
            await editor.get_by_role('button', name='Create field').click()
            await page.get_by_text('A field with this key already exists. Reuse it or choose another key.', exact=True).wait_for()
            assert await page.locator('[data-field-key=brand]').count() == 1
            await editor.get_by_label('Field key', exact=True).fill('reference-number')
            await editor.get_by_label('Field name', exact=True).fill('Reference number')
            await editor.get_by_label('Field type', exact=True).select_option('Integer')
            await editor.get_by_role('button', name='Create field').click()
            await page.get_by_text('Field created. Available for records and filters.', exact=True).wait_for()
            await product.get_by_label('Reference number', exact=True).fill('9223372036854775807')
            await product.get_by_role('button', name='Save product').click()
            await page.get_by_text('Product saved.', exact=True).wait_for()
            await search.get_by_label('Filter field', exact=True).select_option('reference-number')
            await search.get_by_label('Reference number', exact=True).fill('9223372036854775807')
            await search.get_by_role('button', name='Search', exact=True).click()
            await page.get_by_text('1 product loaded.', exact=True).wait_for()
            await page.locator('#results button').click()
            assert await product.get_by_label('Reference number', exact=True).input_value() == '9223372036854775807'
            await product.get_by_label('Currency', exact=True).fill('EUR')
            await product.get_by_label('Tax basis', exact=True).select_option('Included')
            await product.get_by_label('B2C price', exact=True).fill('120.00')
            await product.get_by_label('B2B price', exact=True).fill('80.00')
            await product.get_by_role('button', name='Save product').click()
            await page.get_by_text('Product saved.', exact=True).wait_for()
            tier = page.locator('[data-form=tier]')
            await tier.get_by_label('Tier key', exact=True).fill('employee')
            await tier.get_by_label('Tier name', exact=True).fill('Employee')
            await tier.get_by_label('Base pricing', exact=True).select_option('B2B')
            await tier.get_by_label('Extra discount (%)', exact=True).fill('10')
            await tier.get_by_role('button', name='Create pricing tier').click()
            await page.get_by_text('Pricing tier created.', exact=True).wait_for()
            client = page.locator('[data-form=client]')
            await client.get_by_label('Client name', exact=True).fill('Employee client')
            assert await client.get_by_label('Client status', exact=True).input_value() == ''
            await client.get_by_label('Client status', exact=True).select_option('B2C')
            await client.get_by_label('Pricing tier', exact=True).select_option('employee')
            await client.get_by_role('button', name='Save client', exact=True).click()
            await page.get_by_text('Client saved · B2C · Employee', exact=True).wait_for()
            assert 'B2C' in await page.locator('[data-clients]').inner_text()
            quote = page.locator('[data-form=quote]')
            await quote.get_by_label('Assigned client', exact=True).select_option(label='Employee client · B2C · Employee')
            await quote.get_by_label('Priced product', exact=True).select_option(label='Camera Alpha')
            await quote.get_by_role('button', name='Calculate client price').click()
            await page.locator('[data-quote]').filter(has_text='EUR 72.00').wait_for()
            assert 'B2B base 80.00' in await page.locator('[data-quote]').inner_text()
            await page.screenshot(path=str(output/'desktop.png'), full_page=True)
            await page.set_viewport_size({'width': 390, 'height': 844})
            await page.screenshot(path=str(output/'mobile.png'), full_page=True)
            assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
            assert not errors, errors
            result = {'field_catalogue_reuse': True, 'typed_filter': True, 'same_record_edit': True,
                      'conflicting_definition_rejected': True, 'mobile_no_horizontal_overflow': True,
                      'full_integer_precision_roundtrip': True,
                      'mandatory_b2c_b2b_status_visible': True, 'employee_b2b_plus_discount': True,
                      'page_errors': errors, 'backend': 'actual Rust CRM / ephemeral MemoryStore'}
            result.update(status='behavior-observed',artifact_directory=str(output),source_hashes=before)
          except Exception:
            if 'page' in locals():
                await page.screenshot(path=str(output/'failure.png'), full_page=True, timeout=10000)
                (output/'failure.html').write_text(await page.content(), encoding='utf-8')
                print('Failure diagnostics: ' + str(output), flush=True)
            raise
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--runtime', required=True)
    parser.add_argument('--browser', required=True)
    args = parser.parse_args()
    output=OUTPUT/'runs'/uuid.uuid4().hex
    output.mkdir(parents=True,exist_ok=False)
    latest=OUTPUT/'result.json'
    if latest.exists():
        (output/'prior-result.json').write_bytes(latest.read_bytes())
    latest.write_text(json.dumps({'status':'running','artifact_directory':str(output)},indent=2),encoding='utf-8')
    server=child=None
    try:
        before=fingerprints(args)
        latest.write_text(json.dumps({'status':'running','artifact_directory':str(output),'source_hashes':before},indent=2),encoding='utf-8')
        try:
            server,child=serve()
            threading.Thread(target=server.serve_forever,daemon=True).start()
            result=asyncio.run(proof(args,f'http://127.0.0.1:{server.server_port}',output,before))
        finally:
            if server is not None:
                server.shutdown();server.server_close()
            if child is not None:
                child.stdin.close()
                if child.poll() is None:child.terminate()
                child.wait(timeout=5)
        after=fingerprints(args)
        if before != after:
            raise RuntimeError('Proof inputs changed during execution or cleanup; result is stale')
        result.update(status='passed',source_hashes_after=after)
        encoded=json.dumps(result,indent=2,ensure_ascii=True)
        (output/'result.json').write_text(encoded,encoding='utf-8')
        latest.write_text(encoded,encoding='utf-8')
        print(json.dumps(result,ensure_ascii=True),flush=True)
    except BaseException as error:
        result={'status':'failed','artifact_directory':str(output),'error':repr(error)}
        if 'before' in locals():result['source_hashes']=before
        encoded=json.dumps(result,indent=2,ensure_ascii=True)
        (output/'result.json').write_text(encoded,encoding='utf-8')
        latest.write_text(encoded,encoding='utf-8')
        raise
