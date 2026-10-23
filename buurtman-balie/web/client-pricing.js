import { createLocalization, bindLocalization } from './localization.js';
/** Reusable client classification, pricing tiers and authoritative quote screen. */
export function mountClientPricing(root, adapter, { localization = createLocalization() } = {}) {
  root.innerHTML=`<div class="crm-layout"><section class="crm-panel"><h2>Clients · B2C / B2B</h2><form data-form="client"><label>Client name<input name="name" required maxlength="128"></label><label>Client status (mandatory)<select name="classification" aria-label="Client status" required><option value="">Choose B2C or B2B</option><option value="B2C">B2C</option><option value="B2B">B2B</option></select></label><label>Pricing tier<select name="tier" aria-label="Pricing tier"><option value="">Standard · follows B2C/B2B status</option></select></label><button type="submit">Save client</button><button type="button" data-new-client>New client</button></form><p data-client-status role="status"></p><div data-clients></div></section><section class="crm-panel"><h2>Extra pricing tiers</h2><form data-form="tier"><label>Tier key<input name="id" required maxlength="128"></label><label>Tier name<input name="name" required maxlength="128" placeholder="Employee"></label><label>Base pricing<select name="base" aria-label="Base pricing"><option value="B2C">B2C</option><option value="B2B">B2B</option><option value="Custom">Custom</option></select></label><label>Custom base key<input name="custom" maxlength="128"></label><label>Extra discount (%)<input name="discount" required inputmode="decimal" value="0"></label><button type="submit">Create pricing tier</button></form><p data-tier-status role="status"></p><ul data-tiers></ul></section></div><section class="crm-panel"><h2>Client price preview</h2><form data-form="quote"><label>Assigned client<select name="client" aria-label="Assigned client" required></select></label><label>Priced product<select name="product" aria-label="Priced product" required></select></label><button type="submit">Calculate client price</button></form><p data-quote role="status"></p></section>`;
  const get = selector => root.querySelector(selector), t = (key, values) => localization.text(key, values);
  const unbind = bindLocalization(root, localization), statuses = new Map();
  let clients = [], tiers = [], products = [], selected, lastQuote, disposed = false;
  let pendingClient, pendingTier, generation = 0, quoteGeneration = 0;
  const clientForm = get('[data-form=client]'), tierForm = get('[data-form=tier]'), quoteForm = get('[data-form=quote]');
  const clientDrafts=new Map();
  function clientInput(){return {name:clientForm.elements.name.value,classification:clientForm.elements.classification.value,tier:clientForm.elements.tier.value};}
  function rememberClient(){clientDrafts.set(selected?.id || '__new__',{selected,pendingClient,input:clientInput(),status:statuses.get('[data-client-status]')});}
  function chooseClient(client) {
    rememberClient();const draft=clientDrafts.get(client?.id || '__new__');
    selected=draft?.selected || client;pendingClient=draft?.pendingClient;
    if(selected?.needsReload && client && client.revision !== selected.revision){selected=client;pendingClient=undefined;}
    const input=draft?.input || {name:client?.name || '',classification:client?.client_terms.classification || '',tier:client?.client_terms.pricing.Tier || ''};
    for(const key of ['name','classification','tier'])clientForm.elements[key].value=input[key];
    statuses.delete('[data-client-status]');get('[data-client-status]').textContent='';
    if(draft?.status)status('[data-client-status]',draft.status.key,draft.status.values);
  }
  function saveFailure(selector,error) {
    const code=error.message.match(/^(Denied|Invalid|NotFound|Conflict|OperationReused|Capacity)(?:\b|\()/)?.[1];
    status(selector,code==='Conflict'?'Save rejected: search and select the record again to reload your retained draft.':code?'Save rejected: {error}':'Save not confirmed: {error}. Retry to reconcile.',{errorKey:error.message});
    return code;
  }
  const tierLabel = client => client.client_terms.pricing === 'Standard' ? t('Standard') : tiers.find(tier => tier.id === client.client_terms.pricing.Tier)?.name || client.client_terms.pricing.Tier;
  function status(selector, key, values = {}) {
    statuses.set(selector, {key, values});
    get(selector).textContent = t(key, {...values, error: values.errorKey ? localization.error(values.errorKey) : values.error, tier: values.client ? tierLabel(values.client) : values.tier});
  }
  const percentage = bps => localization.money(String(bps), 2).replace(/[.,]00$/, '');
  const baseLabel = base => typeof base === 'string' ? base : `${t('Custom')} ${base.Custom}`;
  function dependencies() {
    const client=clients.find(record=>record.id===quoteForm.elements.client.value), product=products.find(record=>record.id===quoteForm.elements.product.value);
    const tier=tiers.find(tier=>tier.id===client?.client_terms?.pricing?.Tier);
    return JSON.stringify([client?.id,client?.revision,product?.id,product?.revision,tier]);
  }
  function invalidateQuote() {
    quoteGeneration++;lastQuote=undefined;
    status('[data-quote]','Price preview changed. Calculate again.');
  }
  function recordChanged(record) {
    const current=[...clients,...products].find(item=>item.id===record.id);
    if((record.id===quoteForm.elements.client.value || record.id===quoteForm.elements.product.value) && current?.revision!==record.revision)invalidateQuote();
  }
  function render() {
    const prior = clientForm.elements.tier.value;
    clientForm.elements.tier.replaceChildren(new Option(t('Standard · follows B2C/B2B status'), ''), ...tiers.map(tier => new Option(tier.name, tier.id)));
    clientForm.elements.tier.value = prior;
    get('[data-clients]').replaceChildren(...clients.map(client => {
      const button = document.createElement('button'); button.className = 'crm-record';
      button.textContent = `${client.name} · ${client.client_terms.classification} · ${tierLabel(client)}`;
      button.onclick = () => {
        if (clientForm.querySelector('button').disabled) return;
        chooseClient(client);
      };
      return button;
    }));
    const selectedClient = quoteForm.elements.client.value, selectedProduct = quoteForm.elements.product.value;
    quoteForm.elements.client.replaceChildren(new Option(t('Choose client'), ''), ...clients.map(client => new Option(`${client.name} · ${client.client_terms.classification} · ${tierLabel(client)}`, client.id)));
    quoteForm.elements.product.replaceChildren(new Option(t('Choose product'), ''), ...products.map(product => new Option(product.name, product.id)));
    quoteForm.elements.client.value = selectedClient; quoteForm.elements.product.value = selectedProduct;
    get('[data-tiers]').replaceChildren(...tiers.map(tier => {
      const li = document.createElement('li'); li.textContent = `${tier.name} · ${baseLabel(tier.base)} · ${t('extra')} ${percentage(tier.discount_basis_points)}%`; return li;
    }));
  }
  async function refresh() {
    const own = ++generation;
    const [loadedTiers, loadedClients, loadedProducts] = await Promise.all([adapter.listTiers(), adapter.listRecords('Customer'), adapter.listRecords('Product')]);
    if (disposed || own !== generation) return;
    const previous=dependencies();
    tiers = loadedTiers; clients = loadedClients.records; products = loadedProducts.records; render();
    if(previous!==dependencies())invalidateQuote();
  }
  function renderQuote() {
    const quote = lastQuote; if (!quote) return;
    const money = minor => localization.money(minor, quote.scale);
    get('[data-quote]').textContent = `${quote.classification} · ${quote.tier || t('Standard')} · ${baseLabel(quote.base)} ${t('base')} ${money(quote.base_amount)} − ${percentage(quote.discount_basis_points)}% (${money(quote.discount_amount)}) = ${quote.currency} ${money(quote.final_amount)} · ${t('tax')} ${t(quote.tax_basis.toLowerCase())}`;
  }
  const unsubscribe = localization.subscribe(() => { render(); for (const [selector, {key, values}] of statuses) status(selector, key, values); renderQuote(); });
  clientForm.addEventListener('submit', async event => {
    event.preventDefault(); const button = clientForm.querySelector('button'); if (button.disabled) return;
    async function reloadRejectedClient() {
      const id=selected.id;
      const loaded=await adapter.listRecords('Customer');
      if(disposed)return;
      const fresh=loaded.records.find(client=>client.id===id);
      if(!fresh)throw new Error('NotFound');
      const previous=dependencies();clients=loaded.records;selected=fresh;pendingClient=undefined;render();
      if(previous!==dependencies())invalidateQuote();
      status('[data-client-status]','Record reloaded. Review your retained draft before saving.');
    }
    if(selected?.needsReload){
      button.disabled=true;get('[data-new-client]').disabled=true;
      try{await reloadRejectedClient();}catch(error){if(!disposed)status('[data-client-status]','Refresh unavailable: {error}',{errorKey:error.message});}
      finally{if(!disposed){button.disabled=false;get('[data-new-client]').disabled=false;rememberClient();}}
      return;
    }
    const data = {name: clientForm.elements.name.value, client_terms: {classification: clientForm.elements.classification.value, pricing: clientForm.elements.tier.value ? {Tier: clientForm.elements.tier.value} : 'Standard'}};
    if(!pendingClient){
      const id=selected?.id || crypto.randomUUID();
      const record={...(selected || {kind:'Customer', description:'', tags:[], fields:{}, case_status:null, prices:null, revision:0}),...data,id};delete record.needsReload;
      pendingClient={input:clientInput(),request:{operation_id:crypto.randomUUID(),command:{[selected?'Update':'Create']:record}}};
    }
    const attempt=pendingClient,record=Object.values(attempt.request.command)[0];
    button.disabled = true; get('[data-new-client]').disabled = true;
    try {
      const outcome = await adapter.execute(attempt.request);
      if (disposed) return;const changed=JSON.stringify(clientInput())!==JSON.stringify(attempt.input);clientDrafts.delete(selected?.id || '__new__');selected = {...record, revision:outcome.record_revision}; pendingClient = undefined;
      recordChanged(selected);
      status('[data-client-status]',changed?'Previous save confirmed. Save your current changes.':'Client saved · {classification} · {tier}', {classification:record.client_terms.classification, client:record});
      try{await refresh();}catch(error){status('[data-client-status]','Refresh unavailable: {error}',{errorKey:error.message});}
    } catch (error) { if (!disposed){const code=saveFailure('[data-client-status]',error);if(code){pendingClient=undefined;if(code==='Conflict'&&selected){selected={...selected,needsReload:true};try{await reloadRejectedClient();}catch(reloadError){if(!disposed)status('[data-client-status]','Refresh unavailable: {error}',{errorKey:reloadError.message});}}}} }
    finally { if (!disposed) { button.disabled = false; get('[data-new-client]').disabled = false;rememberClient(); } }
  });
  get('[data-new-client]').onclick = () => chooseClient(undefined);
  tierForm.addEventListener('submit', async event => {
    event.preventDefault(); const button = tierForm.querySelector('button'); if (button.disabled) return;
    let dispatched=false;
    try {
      if(!pendingTier){
        const raw = tierForm.elements.discount.value, percent = raw.replace(',', '.');
        if (!/^\d+(\.\d{1,2})?$/.test(percent)) throw new Error('Use a percentage with up to two decimals.');
        const [whole, fraction = ''] = percent.split('.'); const exact = BigInt(whole) * 100n + BigInt(fraction.padEnd(2, '0'));
        if (exact > 10000n) throw new Error('Discount must be between 0 and 100%.');
        const tier = {id:tierForm.elements.id.value, name:tierForm.elements.name.value, base:tierForm.elements.base.value === 'Custom' ? {Custom:tierForm.elements.custom.value} : tierForm.elements.base.value, discount_basis_points:Number(exact)};
        pendingTier={operation_id:crypto.randomUUID(),tier};
      }
      button.disabled = true;dispatched=true;
      await adapter.execute({operation_id:pendingTier.operation_id, command:{DefinePricingTier:pendingTier.tier}});
      if (disposed) return; pendingTier = undefined;
      status('[data-tier-status]', 'Pricing tier created.');
      try{await refresh();}catch(error){if(!disposed)status('[data-tier-status]','Refresh unavailable: {error}',{errorKey:error.message});}
    } catch (error) { if (!disposed){if(!dispatched)status('[data-tier-status]','Correct input: {error}',{errorKey:error.message});else if(saveFailure('[data-tier-status]',error))pendingTier=undefined;} }
    finally { if (!disposed) button.disabled = false; }
  });
  quoteForm.addEventListener('submit', async event => {
    event.preventDefault(); const button = quoteForm.querySelector('button'); if (button.disabled) return;
    button.disabled = true; lastQuote = undefined; status('[data-quote]', 'Calculating…');
    const own=++quoteGeneration, identity=dependencies(), clientId=quoteForm.elements.client.value, productId=quoteForm.elements.product.value;
    try {
      const quote = await adapter.quote(clientId,productId);
      if (disposed || own!==quoteGeneration)return;
      const client=clients.find(record=>record.id===clientId),product=products.find(record=>record.id===productId);
      if(identity!==dependencies() || client?.revision!==quote.client_revision || product?.revision!==quote.product_revision){invalidateQuote();return;}
      lastQuote = quote; statuses.delete('[data-quote]'); renderQuote();
    } catch (error) { if (!disposed && own===quoteGeneration) status('[data-quote]', 'Price unavailable: {error}', {errorKey:error.message}); }
    finally { if (!disposed) button.disabled = false; }
  });
  const selectionChanged=()=>{if(lastQuote || get('[data-quote]').textContent)invalidateQuote();else quoteGeneration++;};
  quoteForm.elements.client.addEventListener('change',selectionChanged);
  quoteForm.elements.product.addEventListener('change',selectionChanged);
  refresh().catch(error => { if (!disposed) status('[data-client-status]', 'Clients unavailable: {error}', {errorKey:error.message}); });
  return {refresh, recordChanged, setLanguage:localization.setLanguage, dispose() { disposed = true;quoteGeneration++; unbind(); unsubscribe(); root.replaceChildren(); }};
}
