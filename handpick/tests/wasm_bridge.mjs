// Execute generated --target web glue with the real compiled WASM, without a mock core.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { resolve, dirname, join } from 'node:path';

const gluePath = process.argv[2];
if (!gluePath) throw new Error('Generated Handpick web glue path is required');
const glue = await import(pathToFileURL(resolve(gluePath)).href);
glue.initSync({ module: await readFile(join(dirname(resolve(gluePath)), 'handpick_bg.wasm')) });
const fixtures = JSON.parse(await readFile(new URL('./fixtures/writing-bridge-v1.json', import.meta.url), 'utf8'));
assert.equal(glue.resolveDraftTitle('  My idea  ', 'first body'), 'My idea');
assert.equal(glue.resolveDraftTitle('My idea', 'edited body'), 'My idea');
assert.equal(glue.resolveDraftTitle(' \n\t', '  first body  '), 'first body');
assert.throws(() => glue.resolveDraftTitle('x'.repeat(65537), ''), error => String(error) === 'PayloadLimit');
assert.equal(fixtures.version, 'handpick.v1');
assert.ok(fixtures.scenarios.length > 0);
const jsonReturns = new Set(['fence', 'pins', 'beginLookup', 'beginComposition', 'pendingSave', 'selected']);
const jsonArgs = new Map([['edited', [0]], ['beginComposition', [0]], ['endComposition', [0]], ['submit', [0]], ['acknowledge', [0]], ['deliver', [0, 3]], ['select', [0, 1]], ['expandIfEmpty', [0]], ['expandForProse', [0]], ['presentContent', [0]], ['presentDraft', [0]]]);
function substitute(value, vars) {
  if (typeof value === 'string' && value.startsWith('$')) {
    assert.ok(vars.has(value.slice(1)), 'Missing fixture variable');
    return vars.get(value.slice(1));
  }
  if (Array.isArray(value)) return value.map(v => substitute(v, vars));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, substitute(v, vars)]));
  return value;
}
let assertions = 0;
for (const scenario of fixtures.scenarios) {
  const session = new glue.WritingSession(scenario.context, scenario.session, scenario.max_pins);
  assert.equal(session.wireVersion(), fixtures.version);
  const vars = new Map();
  try {
    for (const step of scenario.steps) {
      const args = substitute(step.args || [], vars);
      for (const index of jsonArgs.get(step.op) || []) args[index] = JSON.stringify(args[index]);
      if (step.error) {
        assert.throws(() => session[step.op](...args), error => String(error) === step.error);
      } else {
        let value = session[step.op](...args);
        if (jsonReturns.has(step.op)) value = JSON.parse(value);
        if (value === undefined) value = null;
        if ('expect' in step) assert.deepEqual(value, step.expect);
        if (step.store) vars.set(step.store, value);
      }
      assertions++;
    }
  } finally { session.free(); }
}
for (const bad of [-1, 1.5, Infinity, NaN, 1025]) assert.throws(() => new glue.WritingSession('private', 'invalid', bad), error => String(error) === 'PinLimit');
const invalid = new glue.WritingSession('private', 'wire-negative', 1);
try {
  invalid.bind('editor-a', '1');
  const before = invalid.fence();
  for (const metric of [-1, 1.5, Infinity, NaN, 4294967296]) {
    assert.throws(() => invalid.expandForProse(before, metric, 1), error => String(error) === 'InvalidWire');
    assert.throws(() => invalid.expandForProse(before, 160, metric), error => String(error) === 'InvalidWire');
    assert.throws(() => invalid.presentContent(before, metric, 1, false), error => String(error) === 'InvalidWire');
    assert.throws(() => invalid.presentContent(before, 160, metric, false), error => String(error) === 'InvalidWire');
  }
  for (const flag of [null, undefined, 0, 1, 'false', {}]) assert.throws(() => invalid.presentContent(before, 0, 1, flag), error => String(error) === 'InvalidWire');
  for (const flag of [null, undefined, 0, 1, 'false', {}]) assert.throws(() => invalid.presentDraft(before, 0, 1, flag, 'Title'), error => String(error) === 'InvalidWire');
  assert.throws(() => invalid.presentDraft(before, 0, 1, true, 'x'.repeat(65537)), error => String(error) === 'PayloadLimit');
  assert.throws(() => invalid.edited('secret malformed input', '2'), error => String(error) === 'InvalidWire');
  assert.throws(() => invalid.edited('x'.repeat(65537), '2'), error => String(error) === 'PayloadLimit');
  assert.throws(() => invalid.edited(before, '01'), error => String(error) === 'InvalidRevision');
  assert.equal(invalid.fence(), before);
} finally { invalid.free(); }
const encode = JSON.stringify;
const decode = JSON.parse;
let foundationChecks = 0, richChecks = 0;
const check = (actual, expected) => { assert.deepEqual(actual, expected); foundationChecks++; };
const rejected = (fn, code) => { assert.throws(fn, error => String(error) === code); foundationChecks++; };
const limits = {max_text_bytes:64,max_pins:1,max_items:2,max_item_bytes:32,max_actions_per_item:1};
const item = id => ({key:{provider:'notes',result:id},label:id,detail:null,actions:[]});
const foundation = new glue.Session('private', encode(limits));
try {
  check(foundation.wireVersion(), 'handpick.v1');
  foundation.setDraft('Café');
  check(foundation.draftRevision(), '1');
  const fence = decode(foundation.begin());
  check(fence, {context:'private',generation:'2',draft_revision:'1'});
  foundation.deliver(encode(fence), 'suggestions', 'complete', encode([item('a'),item('b')]));
  foundation.select(encode(item('b').key));
  foundation.deliver(encode(fence), 'suggestions', 'complete', encode([item('b'),item('a')]));
  check(decode(foundation.selected()), item('b').key);
  rejected(() => foundation.deliver(encode(fence), 'suggestions', 'complete', encode([item('a'),item('a')])), 'DuplicateResult');
  check(decode(foundation.delivery('suggestions')).items.map(i => i.key.result), ['b','a']);
  foundation.deliver(encode(fence), 'suggestions', 'complete', encode([item('a')]));
  check(decode(foundation.selected()), null);
  const completion = {fence,span:{start:4,end:5},replacement:'Ω'};
  rejected(() => foundation.complete(encode(completion)), 'InvalidSpan');
  check(foundation.draft(), 'Café');
  check(foundation.complete(encode({...completion,span:{start:3,end:5}})), 5);
  check(foundation.draft(), 'CafΩ');
  rejected(() => foundation.deliver(encode(fence), 'search', 'complete', '[]'), 'Stale');
  for (const generation of [2,'02','18446744073709551616']) {
    rejected(() => foundation.deliver(encode({...fence,generation}), 'search', 'complete', '[]'), typeof generation === 'number' ? 'InvalidWire' : 'InvalidRevision');
  }
  rejected(() => foundation.deliver(encode({...fence,generation:'9007199254740993'}), 'search', 'complete', '[]'), 'Stale');
  foundation.pin('first'); foundation.setDraft('other');
  rejected(() => foundation.restore('first'), 'OccupiedDraft');
  check(foundation.draft(), 'other');
  foundation.restore('first','second');
  check(foundation.draft(), 'CafΩ');
  check(decode(foundation.pins()), [{id:'second',tooltip_text:'other'}]);
  rejected(() => foundation.setDraft('x'.repeat(65)), 'TextLimit');
  foundation.switchContext('work');
  check(foundation.draft(), ''); check(decode(foundation.pins()), []);
  foundation.revoke(); rejected(() => foundation.begin(), 'Revoked');
} finally { foundation.free(); }
for (const badLimits of [{...limits,max_text_bytes:65537},{...limits,max_pins:1025},{...limits,max_actions_per_item:65}]) {
  rejected(() => new glue.Session('private', encode(badLimits)), 'PayloadLimit');
}
rejected(() => new glue.Session('private', encode({...limits,max_items:-1})), 'InvalidWire');

const rich = new glue.WritingSession('private','rich-delivery',1);
const richCheck = (actual, expected) => { assert.deepEqual(actual, expected); richChecks++; };
const richReject = (fn, code) => { assert.throws(fn, error => String(error) === code); richChecks++; };
try {
  rich.bind('editor-a','1');
  const fence = decode(rich.beginLookup());
  richCheck(rich.canExpand(), false);
  rich.deliver(encode(fence),'suggestions','offline','[]');
  rich.deliver(encode(fence),'search','offline','[]');
  richCheck(rich.expandIfEmpty(encode(fence)), false);
  rich.deliver(encode(fence),'search','complete',encode([item('a'),item('b')]));
  rich.select(encode(fence),encode(item('b').key));
  rich.deliver(encode(fence),'search','complete',encode([item('b'),item('a')]));
  richCheck(decode(rich.selected()),item('b').key);
  richReject(() => rich.deliver(encode(fence),'search','complete',encode([item('a'),item('a')])), 'DuplicateResult');
  rich.deliver(encode(fence),'search','complete','[]');
  rich.deliver(encode(fence),'suggestions','complete','[]');
  richCheck(rich.canExpand(),true);
  richCheck(rich.expandIfEmpty(encode(fence)),true);
  rich.compact(); richCheck(rich.expanded(),false);
  const page = {fence,provider:'notes',projection_revision:'projection-1',freshness:'ready',status:'complete',counts:{scope:'page_only'},next_cursor:null,rows:[]};
  rich.validateRelationships(encode(page),'notes',10); richChecks++;
  richReject(() => rich.validateRelationships(encode({...page,projection_revision:null}),'notes',10),'Stale');
  richReject(() => rich.validateRelationships(encode(page),'other',10),'Stale');
  richReject(() => rich.validateRelationships(encode(page),'notes',1.5),'PayloadLimit');
  richReject(() => rich.validateRelationships('private malformed','notes',10),'InvalidWire');
  richReject(() => rich.validateRelationships(encode({...page,counts:{scope:'canonical_authorized',source_notes:1,relationships:0}}),'notes',10),'InvalidWire');
  rich.beginLookup();
  richReject(() => rich.select(encode(fence),encode(item('a').key)),'Stale');
  richReject(() => rich.validateRelationships(encode(page),'notes',10),'Stale');
  richCheck(decode(rich.delivery('search')),{status:'pending',items:[]});
} finally { rich.free(); }
let pickerChecks = 0;
const pickerCheck = (actual, expected) => { assert.deepEqual(actual, expected); pickerChecks++; };
const pickerReject = (fn, code) => { assert.throws(fn, error => String(error) === code); pickerChecks++; };
for (const purpose of ['navigation', 'commands', 'settings']) {
  const mode = new glue.WritingSession('private', `mode-${purpose}`, 1);
  try {
    mode.bind('editor', '9007199254740993');
    const stale = mode.beginLookup();
    pickerCheck(mode.setQueryPurpose(purpose), true);
    pickerCheck(mode.queryPurpose(), purpose);
    pickerReject(() => mode.deliver(stale, 'suggestions', 'complete', '[]'), 'Stale');
    const fence = mode.beginLookup();
    mode.deliver(fence, 'suggestions', 'complete', '[]');
    pickerCheck(mode.canExpand(), false);
    pickerCheck(mode.expandIfEmpty(fence), false);
    pickerCheck(mode.expandForProse(fence, 200, 2), false);
    pickerCheck(mode.presentDraft(fence, 200, 2, false, 'Title'), false);
    mode.expand(); pickerCheck(mode.expanded(), true);
    pickerCheck(mode.setQueryPurpose('search_write'), true);
    pickerCheck(mode.expanded(), true);
    pickerCheck(decode(mode.fence()).revision, '9007199254740993');
    pickerReject(() => mode.setQueryPurpose('unknown'), 'InvalidWire');
    const token = mode.beginComposition(mode.fence());
    pickerReject(() => mode.setQueryPurpose('settings'), 'Stale');
    mode.endComposition(token);
  } finally { mode.free(); }
}
pickerCheck(decode(glue.parseQuery('> open settings')).purpose, 'commands');
pickerCheck(decode(glue.parseQuery('type:folder drafts')).purpose, 'navigation');
pickerCheck(decode(glue.parseQuery('type:settings theme')).purpose, 'settings');
pickerCheck(decode(glue.parseQuery('an idea to write')).purpose, 'search_write');
pickerCheck(decode(glue.parseQuery('type:')).incomplete, true);
pickerReject(() => glue.parseQuery('x'.repeat(65537)), 'TextLimit');
const picker = new glue.Picker();
const rows = ['a','b','c','d'].map(id => ({item:item(id),category:'notes',location:'Ideas',highlights:[]}));
rows.push({item:item('file'),category:'files',highlights:[],preview:{kind:'image',owner_ref:'thumbnail-1'}});
const coverage = [{category:'notes',status:'complete',loaded:4,total:4},{category:'files',status:'partial',loaded:1,total:null}];
try {
  picker.setResults(encode(rows),encode(coverage));
  pickerCheck(decode(picker.view()).groups.map(group => group.shown), [3,1]);
  pickerCheck(decode(picker.view()).groups[0].has_more, true);
  picker.select(encode(item('b').key));
  picker.setResults(encode([rows[2],rows[1],rows[0],rows[3],rows[4]]),encode(coverage));
  pickerCheck(decode(picker.view()).selected, item('b').key);
  picker.showMore('notes'); pickerCheck(decode(picker.view()).groups[0].shown, 4);
  picker.toggle('notes'); pickerCheck(decode(picker.view()).selected, item('file').key);
  picker.toggle('notes'); picker.moveSelection(-1); pickerCheck(decode(picker.view()).selected, item('d').key);
  const before = picker.view();
  pickerReject(() => picker.setResults(encode(rows),encode([{...coverage[0],loaded:3},coverage[1]])), 'InvalidCoverage');
  pickerReject(() => picker.setResults(encode(rows),encode([{...coverage[0],total:3},coverage[1]])), 'InvalidCoverage');
  pickerReject(() => picker.setResults(encode(rows),encode([{...coverage[0],loaded:4.5},coverage[1]])), 'InvalidWire');
  pickerReject(() => picker.setResults(encode([...rows,rows[0]]),encode(coverage)), 'DuplicateResult');
  pickerReject(() => picker.setResults(encode([{...rows[4],preview:{kind:'image',owner_ref:'data:image/png;base64,secret'}}]),encode([coverage[1]])), 'InvalidPreview');
  pickerReject(() => picker.setResults(encode([{...rows[0],item:{...item('unicode'),label:'é'},highlights:[{start:0,end:1}]}]),encode([{...coverage[0],loaded:1,total:1}])), 'InvalidHighlight');
  pickerReject(() => picker.setResults('x'.repeat(65537),'[]'), 'PayloadLimit');
  pickerReject(() => picker.toggle('unknown'), 'InvalidWire');
  pickerReject(() => picker.moveSelection(0.5), 'InvalidWire');
  pickerCheck(picker.view(), before);
  picker.setResults('[]','[]'); pickerCheck(decode(picker.view()), {groups:[],selected:null});
} finally { picker.free(); }
console.log(JSON.stringify({status:'pass', scenarios:fixtures.scenarios.length, steps:assertions, bridge_negative_checks:35, foundation_checks:foundationChecks,rich_checks:richChecks,picker_checks:pickerChecks}));
