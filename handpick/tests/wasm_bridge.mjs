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
assert.equal(fixtures.version, 'handpick.v1');
assert.ok(fixtures.scenarios.length > 0);
const jsonReturns = new Set(['fence', 'pins', 'beginLookup', 'beginComposition', 'pendingSave', 'selected']);
const jsonArgs = new Map([['edited', [0]], ['beginComposition', [0]], ['endComposition', [0]], ['submit', [0]], ['acknowledge', [0]], ['deliver', [0, 3]], ['select', [0, 1]], ['expandIfEmpty', [0]], ['expandForProse', [0]]]);
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
  }
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
console.log(JSON.stringify({status:'pass', scenarios:fixtures.scenarios.length, steps:assertions, bridge_negative_checks:19, foundation_checks:foundationChecks,rich_checks:richChecks}));
