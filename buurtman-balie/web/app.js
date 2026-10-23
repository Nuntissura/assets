import { mountUserManual } from './user-manual.js';
import { mountClientPricing } from './client-pricing.js';
import { mountFieldEditor } from './field-editor.js';
import { createLocalization } from './localization.js';
const $ = id => document.getElementById(id);
const localization = createLocalization();
async function call(payload) {
  const response = await fetch('/api', { method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(payload) });
  const result = await response.json(); if (!response.ok || result.error) throw new Error(result.error || 'Request failed'); return result.value;
}
const adapter = {
  listFields: () => call({action:'fields'}),
  defineField: (definition, operation_id) => call({action:'execute',request:{operation_id,command:{DefineField:definition}}}),
};
let definitions = [], selected, pendingSave, saving=false, searchGeneration=0;
const drafts = new Map();
function captureDraft() {
  const controls = [...$('product-editor').querySelectorAll('input,select,textarea')].map(input => ({name:input.name,key:input.dataset.key,value:input.value,checked:input.checked}));
  return {selected,pendingSave,controls,status:$('product-status').textContent};
}
function retainDraft() { drafts.set(selected?.id || '__new__',captureDraft()); }
function restoreDraft(draft) {
  selected=draft.selected;pendingSave=draft.pendingSave;renderProductFields();
  for(const saved of draft.controls) {
    const input=saved.key?[...$('product-fields').querySelectorAll('input')].find(input=>input.dataset.key===saved.key):$('product-editor').elements.namedItem(saved.name);
    if(input){input.value=saved.value;if(input.type==='checkbox')input.checked=saved.checked;}
  }
  $('product-heading').textContent=selected?`Edit ${selected.name}`:'New product';$('product-status').textContent=draft.status;
}
function inputFor(field, value) {
  const label = document.createElement('label'); label.textContent=field.label;
  const input=document.createElement('input'); input.dataset.key=field.key; input.dataset.type=field.value_type;
  input.type=field.value_type==='Boolean'?'checkbox':field.value_type==='Integer'?'number':'text';
  if (input.type==='number') input.step='1'; if(input.type==='text')input.maxLength=2048;
  if (value) { if(input.type==='checkbox')input.checked=value.Boolean; else input.value=value[field.value_type]; }
  label.append(input);return label;
}
function readValue(input) {
  if(input.type==='checkbox')return {Boolean:input.checked};
  if(input.value==='')return undefined;
  if(input.type==='number') { let number;try{number=BigInt(input.value);}catch{throw new Error('Use a whole decimal integer.');}if(number<-(2n**63n)||number>2n**63n-1n)throw new Error('Integer is outside the signed 64-bit range.');return {Integer:number.toString()}; }
  return {Text:input.value};
}
function values(root) { const result={};for(const input of root.querySelectorAll('input[data-key]')){const value=readValue(input);if(value!==undefined)result[input.dataset.key]=value;}return result; }
function renderProductFields(saved={}) { $('product-fields').replaceChildren(...definitions.filter(f=>f.applies_to.includes('Product')).map(f=>inputFor(f,saved[f.key]))); }
function renderFilter() { const field=definitions.find(f=>f.key===$('search').elements.field.value);$('filter-value').replaceChildren(...(field?[inputFor(field)]:[])); }
async function search() {
  const generation=++searchGeneration;
  try {
    const result=await call({action:'search',kind:'Product',query:$('search').elements.query.value,fields:values($('filter-value'))});
    if(generation!==searchGeneration)return;
    const products=result.records.filter(r=>r.kind==='Product');$('results').replaceChildren();
    for(const record of products){const button=document.createElement('button');button.className='crm-record';button.textContent=`${record.name} · ${record.tags.join(', ') || 'No tags'}`;button.dataset.recordId=record.id;button.onclick=()=>select(record);$('results').append(button);}
    $('search-status').textContent=`${products.length} ${products.length===1?'product':'products'} loaded${result.total>result.records.length?' · more results exist':''}.`;
  }catch(error){if(generation===searchGeneration)$('search-status').textContent=`Search unavailable: ${error.message}`;}
}
function select(record) {
  if(saving)return;
  retainDraft();const draft=drafts.get(record.id);if(draft){if(draft.selected?.needsReload){draft.selected=record;draft.status=localization.text('Record reloaded. Review your retained draft before saving.');}restoreDraft(draft);return;}
  selected=record;pendingSave=undefined;const form=$('product-editor');form.elements.name.value=record.name;form.elements.description.value=record.description;form.elements.tags.value=record.tags.join(', ');renderProductFields(record.fields);$('product-heading').textContent=`Edit ${record.name}`;$('product-status').textContent='';setPrices(record.prices);
}
mountFieldEditor($('field-editor'),adapter,{onChange:async fields=>{
  const saved=values($('product-fields'));definitions=fields;renderProductFields(saved);
  const select=$('search').elements.field;const previous=select.value;select.replaceChildren(new Option('All products',''),...fields.filter(f=>f.applies_to.includes('Product')).map(f=>new Option(f.label,f.key)));select.value=previous;renderFilter();await search();
}});
$('search').elements.field.addEventListener('change',renderFilter);
$('search').addEventListener('submit',event=>{event.preventDefault();search();});
$('new-product').onclick=()=>{if(saving)return;retainDraft();const draft=drafts.get('__new__');if(draft){restoreDraft(draft);return;}selected=undefined;pendingSave=undefined;$('product-editor').reset();renderProductFields();$('product-heading').textContent='New product';$('product-status').textContent='';setPrices(null);};
$('product-editor').addEventListener('submit',async event=>{
  event.preventDefault();const form=event.currentTarget;const button=form.querySelector('[type=submit]');if(button.disabled)return;
  if(!pendingSave) {
    try {
      const data={name:form.elements.name.value,description:form.elements.description.value,tags:form.elements.tags.value.split(',').map(t=>t.trim()).filter(Boolean),fields:values($('product-fields')),prices:readPrices()};
      const record={...data,id:selected?.id || crypto.randomUUID(),kind:'Product',case_status:null,client_terms:null,revision:selected?.revision || 0};
      pendingSave={controls:captureDraft().controls,record,request:{operation_id:crypto.randomUUID(),command:{[selected?'Update':'Create']:record}}};
    }catch(error){$('product-status').textContent=localization.text('Correct input: {error}',{error:localization.error(error.message)});return;}
  }
  const attempt=pendingSave;
  button.disabled=true;saving=true;$('new-product').disabled=true;
  try {
    const outcome=await call({action:'execute',request:attempt.request});
    const changed=JSON.stringify(captureDraft().controls)!==JSON.stringify(attempt.controls);
    drafts.delete(selected?.id || '__new__');selected={...attempt.record,revision:outcome.record_revision};pendingSave=undefined;
    clientPricing.recordChanged(selected);$('product-heading').textContent=`Edit ${selected.name}`;
    $('product-status').textContent=localization.text(changed?'Previous save confirmed. Save your current changes.':'Product saved.');
    try{await clientPricing.refresh();}catch(error){$('product-status').textContent+=' '+localization.text('Refresh unavailable: {error}',{error:localization.error(error.message)});}
    await search();
  }catch(error){
    const code=error.message.match(/^(Denied|Invalid|NotFound|Conflict|OperationReused|Capacity)(?:\b|\()/)?.[1];
    if(code){pendingSave=undefined;if(code==='Conflict'&&selected)selected={...selected,needsReload:true};$('product-status').textContent=localization.text(code==='Conflict'?'Save rejected: search and select the record again to reload your retained draft.':'Save rejected: {error}',{error:localization.error(error.message)});}
    else $('product-status').textContent=localization.text('Save not confirmed: {error}. Retry to reconcile.',{error:localization.error(error.message)});
  }
  finally{button.disabled=false;saving=false;$('new-product').disabled=false;retainDraft();}
});

function amount(text,scale) {
  if(!/^\d+(\.\d+)?$/.test(text))throw new Error('Use a nonnegative decimal price.');
  const [whole,fraction='']=text.split('.');if(fraction.length>scale)throw new Error('Price has more decimals than the selected currency scale.');
  const minor=BigInt(whole)*(10n**BigInt(scale))+BigInt(fraction.padEnd(scale,'0') || '0');
  if(minor>9223372036854775807n)throw new Error('Price exceeds supported range.');return minor.toString();
}
function decimal(minor,scale) { const value=BigInt(minor),factor=10n**BigInt(scale);return scale?`${value/factor}.${String(value%factor).padStart(scale,'0')}`:value.toString(); }

function readPrices() {
 const form=$('product-editor');const scale=Number(form.elements.price_scale.value);if(!Number.isInteger(scale)||scale<0||scale>6)throw new Error('Select 0 to 6 price decimal places.');const b2c=form.elements.b2c.value.trim(),b2b=form.elements.b2b.value.trim();
 const custom=form.elements.custom_price.value.trim(),key=form.elements.custom_key.value.trim();
 if(!b2c&&!b2b&&!custom&&!Object.keys(selected?.prices?.custom || {}).length)return null;
 if(!form.elements.currency.value||!form.elements.tax_basis.value)throw new Error('Select currency and tax basis for product prices.');
 const prices=Object.fromEntries(Object.entries(selected?.prices?.custom || {}).map(([key,value])=>[key,amount(decimal(value,selected.prices.scale),scale)]));if(custom){if(!key)throw new Error('Set a custom price key.');prices[key]=amount(custom,scale);}else if(key)delete prices[key];
 return {currency:form.elements.currency.value.toUpperCase(),scale,tax_basis:form.elements.tax_basis.value,b2c:b2c?amount(b2c,scale):null,b2b:b2b?amount(b2b,scale):null,custom:prices};
}
function setPrices(book) {
 const form=$('product-editor');form.elements.currency.value=book?.currency || '';form.elements.tax_basis.value=book?.tax_basis || '';form.elements.price_scale.value=book?.scale ?? 2;
 form.elements.b2c.value=book?.b2c!==null&&book?.b2c!==undefined?decimal(book.b2c,book.scale):'';form.elements.b2b.value=book?.b2b!==null&&book?.b2b!==undefined?decimal(book.b2b,book.scale):'';
 const key=book?Object.keys(book.custom)[0]:undefined;form.elements.custom_key.value=key || '';form.elements.custom_price.value=key?decimal(book.custom[key],book.scale):'';
}

const clientPricing=mountClientPricing($('client-pricing'),{
 listTiers:()=>call({action:'tiers'}),
 listRecords:kind=>call({action:'search',kind,query:'',fields:{}}),
 execute:request=>call({action:'execute',request}),
 quote:(client_id,product_id)=>call({action:'quote',client_id,product_id}),
});
mountUserManual($('user-manual'), {instanceId: 'standalone-help'});
