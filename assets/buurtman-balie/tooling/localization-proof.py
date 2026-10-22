"""Scoped language proof: real CRM writes plus injected response-loss reconciliation."""
import argparse
import asyncio
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import threading


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / '.local/localization-proof'


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def fingerprints(args):
    paths = [ROOT / name for name in ('Cargo.toml', 'Cargo.lock', 'crates/buurtman-balie-host-demo/Cargo.toml')]
    for folder, pattern in [('web', '*'), ('src', '*.rs'), ('crates/buurtman-balie-host-demo/examples', '*.rs'), ('tooling', '*.py')]:
        paths.extend(path for path in (ROOT / folder).rglob(pattern) if path.is_file())
    binary = Path(os.environ.get('BB_DEMO_BINARY', ROOT / 'target/debug/examples/json_bridge'))
    if os.name == 'nt' and not binary.suffix:
        binary = binary.with_suffix('.exe')
    binary = binary.resolve(strict=True)
    paths.append(binary)
    result = {path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else str(path):
              sha256_file(path) for path in sorted(set(paths))}
    result['runtime/python'] = sha256_file(Path(sys.executable))
    result['runtime/browser'] = sha256_file(Path(args.browser).resolve(strict=True))
    spec = importlib.util.find_spec('playwright')
    if spec is None or spec.origin is None:
        raise RuntimeError('Selected runtime does not provide Playwright')
    package = Path(spec.origin).parent
    for path in sorted(package.rglob('*')):
        if path.is_file() and '__pycache__' not in path.parts and path.suffix != '.pyc':
            result['runtime/playwright/' + path.relative_to(package).as_posix()] = sha256_file(path)
    return result


def expected_money(minor, scale, decimal):
    # Python arbitrary-precision integers: independent of the browser formatter.
    whole, fraction = divmod(abs(minor), 10 ** scale)
    return ('-' if minor < 0 else '') + str(whole) + (decimal + str(fraction).zfill(scale) if scale else '')


async def prove_unknown_errors_and_disposal(page):
    # Deliberately isolated UI failure adapters; these do not prove CRM persistence.
    await page.evaluate("""async () => {
      window.failureLocale=createLocalization('en');
      window.failureRoots={};window.failureMounts=[];
      const root=id=>{const node=document.createElement('div');node.id=id;document.body.append(node);failureRoots[id]=node;return node;};
      const fail=async()=>{throw new Error('SECRET_ADAPTER_TOKEN=never-display');};
      const client={id:'failure-client',kind:'Customer',name:'Failure client',revision:7,client_terms:{classification:'B2B',pricing:'Standard'}};
      const product={id:'failure-product',kind:'Product',name:'Failure product'};
      const adapter={listFields:async()=>[],listTiers:async()=>[],listRecords:async kind=>({records:kind==='Customer'?[client]:[product]}),defineField:fail,execute:fail,quote:fail};
      failureMounts.push(mountFieldEditor(root('failure-field-refresh'),{...adapter,listFields:fail},{localization:failureLocale}));
      failureMounts.push(mountClientPricing(root('failure-pricing-refresh'),{...adapter,listTiers:fail},{localization:failureLocale}));
      const field=mountFieldEditor(root('failure-field-write'),adapter,{localization:failureLocale});
      const pricing=mountClientPricing(root('failure-pricing-write'),adapter,{localization:failureLocale});
      failureMounts.push(field,pricing);await Promise.all([field.refresh(),pricing.refresh()]);
      const form=failureRoots['failure-field-write'].querySelector('form');form.querySelector('details').open=true;form.elements.keyOverride.checked=true;form.elements.key.value='failure-key';form.elements.label.value='Original failure label';form.requestSubmit();
      const writeRoot=failureRoots['failure-pricing-write'];
      const customer=writeRoot.querySelector('[data-form=client]');customer.elements.name.value='Original failure client';customer.elements.classification.value='B2B';customer.requestSubmit();
      const tier=writeRoot.querySelector('[data-form=tier]');tier.elements.id.value='failure-tier';tier.elements.name.value='Original failure tier';tier.elements.discount.value='5,25';tier.requestSubmit();
      const quote=writeRoot.querySelector('[data-form=quote]');quote.elements.client.value=client.id;quote.elements.product.value=product.id;quote.requestSubmit();
    }""")
    selectors = ['#failure-field-refresh [role=status]', '#failure-pricing-refresh [data-client-status]',
                 '#failure-field-write [role=status]', '#failure-pricing-write [data-client-status]',
                 '#failure-pricing-write [data-tier-status]', '#failure-pricing-write [data-quote]']
    for selector in selectors:
        await page.locator(selector).filter(has_text='Request failed').wait_for()
    expected = {
        'en': ['Fields unavailable: Request failed', 'Clients unavailable: Request failed',
               'Field not confirmed: Request failed. Retry to reconcile.', 'Save not confirmed: Request failed. Retry to reconcile.',
               'Save not confirmed: Request failed. Retry to reconcile.', 'Price unavailable: Request failed'],
        'fr': ['Champs indisponibles : Demande non confirmée', 'Clients indisponibles : Demande non confirmée',
               'Champ non confirmé : Demande non confirmée. Réessayez pour vérifier.',
               'Enregistrement non confirmé : Demande non confirmée. Réessayez pour vérifier.',
               'Enregistrement non confirmé : Demande non confirmée. Réessayez pour vérifier.', 'Prix indisponible : Demande non confirmée'],
        'nl': ['Velden niet beschikbaar: Verzoek niet bevestigd', 'Klanten niet beschikbaar: Verzoek niet bevestigd',
               'Veld niet bevestigd: Verzoek niet bevestigd. Probeer opnieuw ter bevestiging.',
               'Opslag niet bevestigd: Verzoek niet bevestigd. Probeer opnieuw ter bevestiging.', 'Opslag niet bevestigd: Verzoek niet bevestigd. Probeer opnieuw ter bevestiging.',
               'Prijs niet beschikbaar: Verzoek niet bevestigd'],
    }
    for language, messages in expected.items():
        await page.evaluate('(language)=>failureLocale.setLanguage(language)', language)
        for selector, message in zip(selectors, messages):
            assert await page.locator(selector).inner_text() == message
        assert 'SECRET_ADAPTER_TOKEN' not in await page.locator('body').inner_text()
        assert await page.locator('#failure-field-write [name=label]').input_value() == 'Original failure label'
        assert await page.locator('#failure-pricing-write [data-form=client] [name=name]').input_value() == 'Original failure client'
        assert await page.locator('#failure-pricing-write [name=discount]').input_value() == '5,25'
        assert await page.locator('#failure-pricing-write [data-form=quote] [name=product]').input_value() == 'failure-product'
    await page.evaluate("""async () => {
      failureMounts.forEach(mount=>mount.dispose());Object.values(failureRoots).forEach(root=>root.remove());
      const locale=createLocalization('en');let subscriptions=0;
      const subscribe=locale.subscribe;locale.subscribe=listener=>{subscriptions++;const off=subscribe(listener);return ()=>{subscriptions--;off();};};
      let releaseField,releaseClient,fieldReads=0,clientReads=0,changes=0;
      const fieldWait=new Promise(resolve=>releaseField=resolve),clientWait=new Promise(resolve=>releaseClient=resolve);
      const fieldRoot=document.createElement('div'),pricingRoot=document.createElement('div');document.body.append(fieldRoot,pricingRoot);
      const field=mountFieldEditor(fieldRoot,{listFields:async()=>{fieldReads++;return [];},defineField:()=>fieldWait},{localization:locale,onChange:()=>changes++});
      const pricing=mountClientPricing(pricingRoot,{listTiers:async()=>[],listRecords:async()=>{clientReads++;return {records:[]};},execute:()=>clientWait},{localization:locale});
      await Promise.all([field.refresh(),pricing.refresh()]);
      const fieldForm=fieldRoot.querySelector('form');fieldForm.querySelector('details').open=true;fieldForm.elements.keyOverride.checked=true;fieldForm.elements.key.value='disposed-key';fieldForm.elements.label.value='Disposed label';fieldForm.requestSubmit();
      const clientForm=pricingRoot.querySelector('[data-form=client]');clientForm.elements.name.value='Disposed client';clientForm.elements.classification.value='B2C';clientForm.requestSubmit();
      if(!fieldForm.querySelector('button:not([data-testid="field-refresh"])').disabled || !clientForm.querySelector('button').disabled)throw new Error('Disposal did not exercise pending saves');
      const oldHeading=fieldRoot.querySelector('h2'),beforeHeading=oldHeading.textContent;
      const before=[fieldReads,clientReads,changes];field.dispose();pricing.dispose();
      if(subscriptions!==0)throw new Error('Disposed module still subscribed');
      locale.setLanguage('fr');releaseField({});releaseClient({record_revision:1});
      await new Promise(resolve=>setTimeout(resolve,0));
      if(oldHeading.textContent!==beforeHeading || fieldRoot.children.length || pricingRoot.children.length || JSON.stringify(before)!==JSON.stringify([fieldReads,clientReads,changes]))throw new Error('Disposed pending result mutated or refreshed module');
      fieldRoot.remove();pricingRoot.remove();
    }""")


async def proof(args, url, inputs, started_at):
    from playwright.async_api import async_playwright
    from browser_runtime import owned_browser
    output = OUTPUT
    output.mkdir(parents=True, exist_ok=True)
    async with async_playwright() as p, owned_browser(p, args.browser) as browser:
        page = None
        try:
            page = await browser.new_page(viewport={'width': 1280, 'height': 1100})
            page.set_default_timeout(10000)
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            await page.goto(url, wait_until='domcontentloaded')
            await page.evaluate("""async () => {
              const {createLocalization}=await import('./localization.js');
              const {mountFieldEditor}=await import('./field-editor.js');
              const {mountClientPricing}=await import('./client-pricing.js');
              window.createLocalization=createLocalization;
              window.mountFieldEditor=mountFieldEditor;window.mountClientPricing=mountClientPricing;
              document.body.replaceChildren();
              window.locale=createLocalization();window.otherLocale=createLocalization('nl');
              window.calls=[];window.lose=new Set();window.waiters={};
              window.call=async payload=>{const response=await fetch('/api',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)});const result=await response.json();if(result.error)throw new Error(result.error);return result.value;};
              const adapter={listFields:()=>call({action:'fields'}),listTiers:()=>call({action:'tiers'}),listRecords:kind=>call({action:'search',kind,query:'',fields:{}}),quote:(client_id,product_id)=>call({action:'quote',client_id,product_id}),
                execute:async request=>{calls.push(JSON.stringify(request));const result=await call({action:'execute',request});const key=Object.keys(request.command)[0];if(lose.delete(key)){if(waiters[key])await waiters[key];throw new Error('Storage');}return result;},
                defineField:async(definition,operation_id)=>{const request={operation_id,command:{DefineField:definition}};calls.push(JSON.stringify(request));const result=await call({action:'execute',request});if(lose.delete('DefineField')){if(waiters.DefineField)await waiters.DefineField;throw new Error('Storage');}return result;}};
              for(const id of ['fields','pricing','other']){const root=document.createElement('div');root.id=id;document.body.append(root);}
              window.fields=mountFieldEditor(document.querySelector('#fields'),adapter,{localization:locale});
              window.pricing=mountClientPricing(document.querySelector('#pricing'),adapter,{localization:locale});
              window.other=mountFieldEditor(document.querySelector('#other'),adapter,{localization:otherLocale});
              await Promise.all([fields.refresh(),pricing.refresh(),other.refresh()]);
              const record={id:'precision-product',kind:'Product',name:'Original product name',description:'',tags:[],fields:{},case_status:null,client_terms:null,revision:0,prices:{currency:'EUR',scale:2,tax_basis:'Included',b2c:'9223372036854775807',b2b:'9223372036854775807',custom:{}}};
              await call({action:'execute',request:{operation_id:crypto.randomUUID(),command:{Create:record}}});await pricing.refresh();
            }""")
            field = page.locator('#fields [data-testid=field-editor]')
            client = page.locator('#pricing [data-form=client]')
            tier = page.locator('#pricing [data-form=tier]')
            quote = page.locator('#pricing [data-form=quote]')
            await field.get_by_label('Field name', exact=True).fill('Original user field')
            await field.locator('[data-testid=field-advanced] summary').click()
            await field.get_by_label('Use a custom key', exact=True).check()
            await field.locator('[name=key]').fill('unchanged-key')
            await client.locator('[name=name]').fill('Original client name')
            await client.locator('[name=classification]').select_option('B2C')
            await tier.locator('[name=id]').fill('original-tier-id')
            await tier.locator('[name=name]').fill('Original tier name')
            await tier.locator('[name=base]').select_option('B2B')
            await tier.locator('[name=discount]').fill('10,25')
            await page.evaluate("""() => {
              window.draftNodes=[...document.querySelectorAll('#fields input,#fields select,#pricing input,#pricing select')];
              window.draftSnapshot=()=>draftNodes.map(node=>[node.name,node.value,node.checked]);
              window.originalDraft=JSON.stringify(draftSnapshot());
            }""")
            # In-flight language switch: retain the exact DOM input and canonical intent.
            await page.evaluate("lose.add('DefineField');waiters.DefineField=new Promise(resolve=>window.releaseField=resolve);window.fieldNode=document.querySelector('#fields input[name=key]')")
            await field.get_by_role('button', name='Create field', exact=True).click()
            await page.wait_for_function("calls.length===1")
            await page.evaluate("locale.setLanguage('fr')")
            assert await field.get_by_role('button', name='Réessayer le champ en attente', exact=True).is_disabled()
            assert await page.evaluate("fieldNode===document.querySelector('#fields input[name=key]')")
            assert await field.locator('[name=key]').input_value() == 'unchanged-key'
            assert await page.evaluate("JSON.stringify(draftSnapshot())===originalDraft && draftNodes.every(node=>node.isConnected)")
            await page.evaluate('releaseField()')
            await page.locator('#fields [role=status]').filter(has_text='Champ non confirmé').wait_for()
            await page.evaluate("locale.setLanguage('nl')")
            await field.get_by_role('button', name='Onbevestigd veld opnieuw proberen', exact=True).click()
            await page.locator('#fields [role=status]').filter(has_text='Veld aangemaakt').wait_for()
            assert await page.evaluate('calls[0]===calls[1]')
            assert 'Original user field · Tekst · Product' in await page.locator('#fields [data-testid=field-list]').inner_text()
            # Selection only reuses canonical definitions; new drafts remain unsent.
            selector = field.locator('[data-testid=field-selector]')
            await selector.select_option('unchanged-key')
            await page.evaluate('fields.refresh()')
            for language, create_label, refresh_label in [('en','Create field','Refresh fields'),('fr','Créer le champ','Actualiser les champs'),('nl','Veld aanmaken','Velden vernieuwen')]:
                await page.evaluate('(language)=>locale.setLanguage(language)', language)
                assert await selector.input_value() == 'unchanged-key'
                assert 'unchanged-key' in await field.locator('[data-testid=field-definition]').inner_text()
                assert await field.get_by_role('button', name=create_label, exact=True, include_hidden=True).is_hidden()
                assert await field.get_by_role('button', name=refresh_label, exact=True).is_visible()
                assert await page.evaluate('calls.length===2')
                assert await page.locator('#other section').get_attribute('lang') == 'nl'
            await selector.select_option('@create-new')
            await field.locator('[name=label]').fill('Énergie draft')
            assert await field.locator('[name=key]').input_value() == 'energie-draft'
            assert not await field.locator('[data-testid=field-advanced]').evaluate('(node)=>node.open')
            for language in ('en', 'fr', 'nl'):
                await page.evaluate('(language)=>locale.setLanguage(language)', language)
                assert await field.locator('[name=label]').input_value() == 'Énergie draft'
                assert await field.locator('[name=key]').input_value() == 'energie-draft'
                assert await field.locator('[name=key]').evaluate('(node)=>node.readOnly')
                assert 'energie-draft' in await field.locator('[data-testid=field-key-preview]').text_content()
                assert await page.evaluate('calls.length===2')
            await field.locator('[data-testid=field-advanced] summary').click()
            await field.locator('[name=keyOverride]').check()
            await field.locator('[name=key]').fill('draft-key')
            await field.locator('[name=type]').select_option('Integer')
            await field.locator('[name=kind][value=Customer]').check()
            for language, name_label, selector_label in [('en','Field name','Choose a field'),('fr','Nom du champ','Choisissez un champ'),('nl','Veldnaam','Kies een veld')]:
                await page.evaluate('(language)=>locale.setLanguage(language)', language)
                assert await field.get_by_label(name_label, exact=True).input_value() == 'Énergie draft'
                assert await field.get_by_label(selector_label, exact=True).input_value() == '@create-new'
                assert await field.locator('[name=key]').input_value() == 'draft-key'
                assert 'draft-key' in await field.locator('[data-testid=field-key-preview]').inner_text()
                assert await field.locator('[name=keyOverride]').is_checked()
                assert await field.locator('[data-testid=field-advanced]').evaluate('(node)=>node.open')
                assert await field.locator('[name=type]').input_value() == 'Integer'
                assert await field.locator('[name=kind][value=Customer]').is_checked()
                assert await page.evaluate('calls.length===2')
                assert await page.locator('#other section').get_attribute('lang') == 'nl'
            await page.evaluate("lose.add('DefinePricingTier');waiters.DefinePricingTier=new Promise(resolve=>window.releaseTier=resolve);window.tierNodes=[...document.querySelectorAll('#pricing [data-form=tier] input,#pricing [data-form=tier] select')];window.tierDraft=JSON.stringify(tierNodes.map(node=>[node.name,node.value]))")
            await tier.get_by_role('button', name='Prijsgroep aanmaken').click()
            await page.wait_for_function('calls.length===3')
            await page.evaluate("locale.setLanguage('fr')")
            assert await tier.get_by_role('button', name='Créer le tarif').is_disabled()
            assert await page.evaluate('tierNodes.every(node=>node.isConnected) && JSON.stringify(tierNodes.map(node=>[node.name,node.value]))===tierDraft')
            await page.evaluate("locale.setLanguage('nl');releaseTier()")
            await page.locator('[data-tier-status]').filter(has_text='niet bevestigd').wait_for()
            await page.evaluate("locale.setLanguage('en')")
            assert await tier.locator('[name=discount]').input_value() == '10,25'
            await tier.get_by_role('button', name='Create pricing tier').click()
            await page.locator('[data-tier-status]').filter(has_text='Pricing tier created.').wait_for()
            assert await page.evaluate('calls[2]===calls[3] && JSON.parse(calls[3]).command.DefinePricingTier.discount_basis_points===1025')
            await client.locator('[name=tier]').select_option('original-tier-id')
            await page.evaluate("lose.add('Create');waiters.Create=new Promise(resolve=>window.releaseClient=resolve);window.clientNodes=[...document.querySelectorAll('#pricing [data-form=client] input,#pricing [data-form=client] select')];window.clientDraft=JSON.stringify(clientNodes.map(node=>[node.name,node.value]))")
            await client.get_by_role('button', name='Save client', exact=True).click()
            await page.wait_for_function('calls.length===5')
            await page.evaluate("locale.setLanguage('nl')")
            assert await client.get_by_role('button', name='Klant opslaan', exact=True).is_disabled()
            assert await page.evaluate('clientNodes.every(node=>node.isConnected) && JSON.stringify(clientNodes.map(node=>[node.name,node.value]))===clientDraft')
            await page.evaluate("locale.setLanguage('en');releaseClient()")
            await page.locator('[data-client-status]').filter(has_text='not confirmed').wait_for()
            await page.evaluate("locale.setLanguage('fr')")
            await client.get_by_role('button', name='Enregistrer le client', exact=True).click()
            await page.locator('[data-client-status]').filter(has_text='Client enregistré').wait_for()
            assert await page.evaluate('calls[4]===calls[5]')
            await quote.locator('[name=client]').select_option(index=1)
            await quote.locator('[name=product]').select_option('precision-product')
            await quote.get_by_role('button', name='Calculer le prix client').click()
            await page.locator('[data-quote]').filter(has_text='EUR').wait_for()
            selected = await quote.locator('[name=client]').input_value()
            base_minor = 9223372036854775807
            discount_minor = (base_minor * 1025 + 5000) // 10000
            final_minor = base_minor - discount_minor
            for language, heading, decimal in [('en', 'Client price preview', '.'), ('fr', 'Aperçu du prix client', ','), ('nl', 'Klantprijsvoorbeeld', ',')]:
                await page.evaluate('(language)=>locale.setLanguage(language)', language)
                assert await page.locator('#pricing').get_by_role('heading', name=heading).count() == 1
                assert await quote.locator('[name=client]').input_value() == selected
                assert await quote.locator('[name=product]').input_value() == 'precision-product'
                text = await page.locator('[data-quote]').inner_text()
                for minor in (base_minor, discount_minor, final_minor):
                    assert expected_money(minor, 2, decimal) in text, text
                assert f'= EUR {expected_money(final_minor, 2, decimal)}' in text, text
                assert 'Original tier name' in text
                assert await page.locator('#other section').get_attribute('lang') == 'nl'
                assert await page.locator('#pricing').get_attribute('lang') == language
                await page.screenshot(path=str(output/f'{language}-desktop.png'), full_page=True)
                assert await page.evaluate('document.documentElement.scrollWidth<=innerWidth')
                await page.set_viewport_size({'width':390, 'height':844})
                await page.screenshot(path=str(output/f'{language}-mobile.png'), full_page=True)
                assert await page.evaluate('document.documentElement.scrollWidth<=innerWidth')
                await page.set_viewport_size({'width':1280, 'height':1100})
            assert await page.evaluate("()=>{try{locale.setLanguage('de');return false;}catch(error){return error instanceof RangeError && locale.language==='nl';}}")
            assert await page.evaluate("()=>{try{createLocalization('de');return false;}catch(error){return error instanceof RangeError;}}")
            money_cases = []
            for language, decimal in [('en', '.'), ('fr', ','), ('nl', ',')]:
                for scale in range(7):
                    for minor in (0, 1, -1, 101, -101, base_minor, -base_minor):
                        money_cases.append([language, str(minor), scale, expected_money(minor, scale, decimal)])
            await page.evaluate("""cases => {
              for(const [language,minor,scale,expected] of cases) {
                locale.setLanguage(language);
                const actual=locale.money(minor,scale);
                if(actual!==expected)throw new Error(`Money mismatch ${language}/${minor}/${scale}: ${actual}`);
              }
            }""", money_cases)
            await prove_unknown_errors_and_disposal(page)
            assert not errors, errors
            result={'languages':['en','fr','nl'],'independent_scopes':True,'exact_bigint_money':True,
                    'inflight_switch_dom_and_drafts_retained':True,'response_loss_replays_identical_field_tier_client_operations':True,
                    'localized_decimal_canonical_basis_points':1025,'unsupported_language_rejected':True,
                    'desktop_mobile_no_overflow':True,'page_errors':errors,'backend':'actual Rust CRM / ephemeral MemoryStore',
                    'unknown_errors_retranslated_without_raw_details':True,'unsupported_initial_language_rejected':True,
                    'disposal_unsubscribes_and_ignores_inflight_results':True,'money_scale_cases':len(money_cases),
                    'field_selection_and_new_draft_key_override_retained_across_languages':True,
                    'expected_minor_amounts':{'base':str(base_minor),'discount':str(discount_minor),'final':str(final_minor)},
                    'provenance':{'started_at_utc':started_at,'completed_at_utc':datetime.now(timezone.utc).isoformat(),
                                  'sha256':inputs,'browser_version':browser.version,
                                  'python_version':sys.version,'runtime_root':str(Path(args.runtime).resolve()),
                                  'browser_executable':str(Path(args.browser).resolve())}}
        except Exception:
            (output/'result.json').unlink(missing_ok=True)
            if page is not None:
                await page.screenshot(path=str(output/'failure.png'),full_page=True)
            raise
    # owned_browser and Playwright have both exited successfully before return.
    return result


def cleanup_service(server, child):
    failures = []
    for action in (server.shutdown, server.server_close, child.stdin.close):
        try:
            action()
        except BaseException as error:
            failures.append(error)
    try:
        if child.poll() is None:
            child.terminate()
        child.wait(timeout=5)
    except BaseException as error:
        failures.append(error)
        if child.poll() is None:
            child.kill()
            child.wait(timeout=5)
    if failures:
        raise RuntimeError('Owned localization service cleanup failed') from failures[0]


if __name__ == '__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--runtime',required=True)
    parser.add_argument('--browser',required=True)
    args=parser.parse_args()
    OUTPUT.mkdir(parents=True, exist_ok=True)
    # Invalidate success before any launch/build-input/import failure can occur.
    (OUTPUT/'result.json').unlink(missing_ok=True)
    try:
        from standalone import serve
        sys.path.insert(0, str(Path(args.runtime).resolve(strict=True)))
        inputs = fingerprints(args)
        started_at = datetime.now(timezone.utc).isoformat()
        server,child=serve()
        threading.Thread(target=server.serve_forever,daemon=True).start()
        try:
            result = asyncio.run(proof(args,f'http://127.0.0.1:{server.server_port}',inputs,started_at))
        finally:
            cleanup_service(server, child)
        final_inputs = fingerprints(args)
        assert inputs == final_inputs, 'Proof source, bridge binary, Python, Playwright or browser changed during run'
        result['provenance']['sha256_after_cleanup'] = final_inputs
        result['provenance']['completed_at_utc'] = datetime.now(timezone.utc).isoformat()
        result['cleanup'] = {'browser':'owned_browser exited successfully','service':'owned bridge exited; server closed'}
        result['status'] = 'passed'
        (OUTPUT/'result.json').write_text(json.dumps(result,indent=2),encoding='utf-8')
        print(json.dumps(result))
    except BaseException:
        (OUTPUT/'result.json').unlink(missing_ok=True)
        raise
