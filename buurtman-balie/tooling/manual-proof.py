"""Headless offline manual proof in the actual standalone composition.

WP009 handoff, 2026-10-11: inspected README, SurrealQuery, bundled topics,
package allowlist, and .local/manual-proof/{result.json,desktop.png,mobile.png,
mobile-storage.png}. Old images show readable English desktop/mobile guidance,
including table provisioning and COMMIT; result has no source fingerprints.
Added localized whole-batch/$params/RETURN/error-mapping/host-ownership guidance,
current build-rule commands and process ownership. WP014 follow-up: inspected
root/member Cargo manifests, relocated json_bridge and standalone harness;
manual-content.js now owns explicit buurtman-balie-host-demo build commands,
host-owned production GUI, optional presentation, and required separate Windows
x64/ARM64 runtime evidence (macOS/Linux later). No platform proof claimed.
Fingerprint inputs now include the member manifest/source and configured bridge.
Old screenshots/result do not prove edits. No browser,
Cargo, package or runtime proof run by this writer. Devlead must run this script
with verified --runtime/--browser after the shared json_bridge build, inspect
fresh desktop/mobile and FR/NL storage screenshots, and verify package contents.
This proof covers offline manual presentation, not real database or host behavior.
Cleanup follow-up: owned_browser replaces Chromium launch; assertions produce only
a candidate. Publish PASS after owned browser/service cleanup and matching source,
bridge, selected Playwright runtime, Python and Chromium hashes. No proof run here;
Codex browser language screenshots do not prove offline/subscription assertions.
WP015 ready: inspected actual field-editor.js/localization.js; manual names match
Choose a field/Create new field/Field name/Advanced: technical key/Use a custom key
and FR/NL counterparts. Product draft selectors remain unchanged. Preserve every
offline/subscription/draft assertion; additionally check localized field guidance.
Prior full proof reported by devlead is invalidated by these source changes; no run here.
P2 follow-up: actual Refresh fields/FR/NL recovery button and preview inside Advanced
inspected; guidance updated. This proof has no field-create button selector, so the
second type=button control introduces no selector ambiguity here. No proof run.
"""
import argparse
import asyncio
import hashlib
import json
import os
import subprocess
from pathlib import Path
import sys
import threading

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / '.local/manual-proof'


def record_result(result):
    OUTPUT.mkdir(parents=True, exist_ok=True)
    (OUTPUT / 'result.json').write_text(json.dumps(result, indent=2), encoding='utf-8')


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def fingerprints(args):
    paths = [*sorted((ROOT / 'web').glob('*.js')),
             *sorted((ROOT / 'web').glob('*.html')),
             *sorted((ROOT / 'web').glob('*.css')),
             *sorted((ROOT / 'src').glob('*.rs')),
             ROOT / 'web/package.json', ROOT / 'Cargo.toml', ROOT / 'Cargo.lock',
             ROOT / 'crates/buurtman-balie-host-demo/Cargo.toml',
             ROOT / 'crates/buurtman-balie-host-demo/examples/json_bridge.rs',
             ROOT / 'tooling/standalone.py', ROOT / 'tooling/browser_runtime.py',
             Path(__file__).resolve()]
    result = {path.relative_to(ROOT).as_posix(): sha256(path)
              for path in paths}
    binary = Path(os.environ.get('BB_DEMO_BINARY', ROOT / 'target/debug/examples/json_bridge'))
    if os.name == 'nt' and not binary.suffix:
        binary = binary.with_suffix('.exe')
    result['executed_bridge_binary'] = sha256(binary.resolve(strict=True))
    result['browser_executable'] = sha256(Path(args.browser).resolve(strict=True))
    result['python_executable'] = sha256(Path(sys.executable).resolve(strict=True))
    runtime = Path(args.runtime).resolve(strict=True)
    package = runtime / 'playwright'
    if not package.is_dir():
        raise ValueError('Selected runtime must contain the actual Playwright package')
    # Include the Python API, bundled driver/Node and metadata; cache files are derived.
    for path in sorted(package.rglob('*')):
        if path.is_file() and '__pycache__' not in path.parts and path.suffix != '.pyc':
            result['runtime/' + path.relative_to(runtime).as_posix()] = sha256(path)
    return result


async def proof(args, url):
    from browser_runtime import owned_browser
    sys.path.insert(0, str(Path(args.runtime).resolve(strict=True)))
    import playwright
    Path(playwright.__file__).resolve(strict=True).relative_to(Path(args.runtime).resolve(strict=True))
    from playwright.async_api import async_playwright
    output = OUTPUT
    output.mkdir(parents=True, exist_ok=True)
    async with async_playwright() as p, owned_browser(p, args.browser) as browser:
        try:
            page = await browser.new_page(viewport={'width':1280, 'height':1100})
            page.set_default_timeout(10000)
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            await page.goto(url, wait_until='networkidle')
            manual = page.locator('[data-testid=user-manual]')
            assert await manual.count() == 1
            await page.locator('#product-editor [name=name]').fill('Unsaved operator text')
            await manual.locator('summary').focus()
            await page.keyboard.press('Enter')
            assert await manual.evaluate('(node)=>node.open')
            assert await manual.locator('[data-manual-topic]').count() == 9
            assert len(set(await manual.locator('[data-manual-topic]').evaluate_all('(nodes)=>nodes.map(n=>n.id)'))) == 9
            assert 'SurrealStore::new' in await manual.inner_text()
            assert 'DEFINE TABLE IF NOT EXISTS bb_workspaces' in await manual.inner_text()
            assert 'cargo build --locked --offline -p buurtman-balie-host-demo --example json_bridge' in await manual.inner_text()
            await page.context.set_offline(True)
            for lang, heading in [('fr','Manuel utilisateur interne'),('nl','Interne gebruikershandleiding'),('en','Internal user manual')]:
                await manual.locator('[data-manual-language]').select_option(lang)
                assert await manual.get_attribute('lang') == lang
                assert await manual.locator('h2').inner_text() == heading
                assert await page.locator('#product-editor [name=name]').input_value() == 'Unsaved operator text'
                assert await manual.evaluate('(node)=>node.open')
                field_guidance = await manual.locator('[data-manual-topic=fields]').inner_text()
                controls = {
                    'en':['Choose a field','Create new field','Field name','Advanced: technical key','Use a custom key','Refresh fields'],
                    'fr':['Choisissez un champ','Créer un nouveau champ','Nom du champ','Avancé : clé technique','Utiliser une clé personnalisée','Actualiser les champs'],
                    'nl':['Kies een veld','Nieuw veld aanmaken','Veldnaam','Geavanceerd: technische sleutel','Een eigen sleutel gebruiken','Velden vernieuwen'],
                }
                for control in controls[lang]:
                    assert control in field_guidance, (lang, control)
                purpose = await manual.locator('[data-manual-topic=purpose]').inner_text()
                for literal in ['crates/buurtman-balie-host-demo', 'Windows x64', 'ARM64', 'macOS/Linux']:
                    assert literal in purpose, (lang, literal)
                storage = manual.locator('[data-manual-topic=storage-swarm]')
                assert 'DEFINE TABLE IF NOT EXISTS bb_workspaces SCHEMALESS PERMISSIONS NONE;' in await storage.inner_text()
                transport = await storage.locator('[data-manual-transport]').inner_text()
                for literal in ['bb_workspaces', '$params', 'SurrealQuery', 'RETURN', 'COMMIT',
                                'BB_STALE_REVISION', 'Error::Conflict', 'Error::Storage', 'TLS']:
                    assert literal in transport, (lang, literal)
                commands = await manual.locator('[data-manual-topic=run-diagnostics] code').inner_text()
                assert 'cargo test --locked --offline --all-features --lib --tests' in commands
                assert 'cargo clippy --locked --offline --all-targets --all-features -- -D warnings' in commands
                assert 'BB_DEMO_BINARY' in await manual.locator('[data-manual-topic=run-diagnostics]').inner_text()
                await storage.scroll_into_view_if_needed()
                assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
                if lang != 'en':
                    await page.screenshot(path=str(output / f'desktop-storage-{lang}.png'), full_page=True)
                    await page.set_viewport_size({'width':390,'height':844})
                    await storage.scroll_into_view_if_needed()
                    assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
                    await page.screenshot(path=str(output / f'mobile-storage-{lang}.png'), full_page=True)
                    await page.set_viewport_size({'width':1280,'height':1100})
            await manual.scroll_into_view_if_needed()
            await page.screenshot(path=str(output/'desktop.png'))
            await page.set_viewport_size({'width':390,'height':844})
            await page.screenshot(path=str(output/'mobile.png'))
            assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
            await manual.locator('[data-manual-topic=storage-swarm]').scroll_into_view_if_needed()
            await page.screenshot(path=str(output/'mobile-storage.png'), full_page=True)
            assert await page.evaluate('document.documentElement.scrollWidth <= innerWidth')
            await page.context.set_offline(False)
            independent = await page.evaluate('''async () => {
              const {createLocalization} = await import('./localization.js');
              const {mountUserManual} = await import('./user-manual.js');
              const a=document.createElement('div'), b=document.createElement('div');document.body.append(a,b);
              const locale=createLocalization('fr');
              const one=mountUserManual(a,{localization:locale,instanceId:'one-help'});
              const two=mountUserManual(b,{localization:createLocalization('nl'),instanceId:'two-help'});
              locale.setLanguage('en');
              const result=a.querySelector('details').lang==='en'&&b.querySelector('details').lang==='nl';
              one.dispose(); locale.setLanguage('fr');
              const disposed=!a.querySelector('details');two.dispose();a.remove();b.remove();
              return {result,disposed};
            }''')
            assert independent == {'result':True,'disposed':True}
            assert not errors, errors
            result={'result':'candidate','topics':9,'languages':['en','fr','nl'],
                    'offline_without_login':True,'keyboard_disclosure':True,'pending_form_preserved':True,
                    'independent_instances_and_disposal':True,'desktop_mobile_no_horizontal_overflow':True,
                    'localized_surreal_transport_contract':True,
                    'localized_existing_new_field_guidance':True,
                    'actual_composition':'Rust MemoryStore standalone harness',
                    'browser_version':browser.version,
                    'page_errors':errors}
        except Exception:
            if 'page' in locals():
                try:
                    await page.screenshot(path=str(output/'failure.png'), timeout=10000)
                except Exception:
                    pass  # Diagnostic failure must not replace the acceptance failure.
            raise
    return result  # Both owned_browser and Playwright cleanup completed.


def cleanup_service(server, child, thread):
    errors = []
    if server is not None:
        try:
            if thread is not None and thread.is_alive():
                server.shutdown()
                thread.join(timeout=5)
                if thread.is_alive():
                    raise RuntimeError('Owned harness thread remains alive')
        except Exception as error:
            errors.append(error)
        try:
            server.server_close()
        except Exception as error:
            errors.append(error)
    if child is not None:
        try:
            child.stdin.close()
        except Exception as error:
            errors.append(error)
        try:
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()  # Only this session's owned bridge handle.
                child.wait(timeout=5)
            if child.poll() is None:
                raise RuntimeError('Owned bridge remains alive')
        except Exception as error:
            errors.append(error)
    if errors:
        raise RuntimeError('Owned service cleanup failed') from errors[0]


if __name__ == '__main__':
    # Invalidate a prior PASS even when argument parsing or import inputs fail.
    record_result({'result':'running'})
    parser=argparse.ArgumentParser()
    parser.add_argument('--runtime',required=True)
    parser.add_argument('--browser',required=True)
    args=parser.parse_args()
    args.source_sha256 = {}
    server = child = thread = None
    try:
        from standalone import serve
        Path(args.browser).resolve(strict=True)
        args.source_sha256 = fingerprints(args)
        record_result({'result':'running','source_sha256':args.source_sha256})
        try:
            server, child=serve()
            thread = threading.Thread(target=server.serve_forever,daemon=True)
            thread.start()
            result = asyncio.run(proof(args,f'http://127.0.0.1:{server.server_port}'))
        finally:
            cleanup_service(server, child, thread)
        after = fingerprints(args)
        assert after == args.source_sha256, 'Proof inputs changed before cleanup completed'
        result.update(result='passed', cleanup={'browser':'completed','service':'completed'},
                      fingerprints_before=args.source_sha256, fingerprints_after=after)
        record_result(result)
        print(json.dumps(result))
    except BaseException as error:
        record_result({'result':'failed','error_type':type(error).__name__,
                       'source_sha256':args.source_sha256})
        raise
