import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
const [modulePath] = process.argv.slice(2);
const binding = await import(pathToFileURL(modulePath));
await binding.default({ module_or_path: await readFile(modulePath.replace(/\.js$/, '_bg.wasm')) });
const input = JSON.parse(binding.example_input());
assert.equal(typeof binding.RssBridge, 'function', 'selected browser module must export the actual public RssBridge');
assert.throws(() => new binding.Consumer(JSON.stringify({ ...input, version: 2 })), error => String(error) === 'unsupported_version');
assert.throws(() => new binding.Consumer('{'), error => String(error) === 'invalid_json');
const facade = new binding.Consumer(JSON.stringify(input));
assert.equal(JSON.parse(facade.snapshot()).view.phase, 'idle');
facade.free();
assert.throws(() => new binding.RssBridge(JSON.stringify({ ...input, version: 2 })), error => String(error) === 'unsupported_version');
const bridge = new binding.RssBridge(JSON.stringify(input));
try {
  const snapshot = JSON.parse(bridge.snapshot());
  assert.equal(snapshot.view.phase, 'idle');
  assert.equal(snapshot.view.query.library_revision, '9007199254740993');
  const loading = JSON.parse(bridge.apply(JSON.stringify({ version: 1, action: { type: 'load' } })));
  assert.equal(loading.view.phase, 'loading');
  const effect = loading.effects.find(effect => effect.type === 'query');
  assert.ok(effect.token);
  const complete = JSON.parse(bridge.apply(JSON.stringify({ version: 1, action: { type: 'complete_page', token: effect.token, result: { type: 'page', page: {
    snapshot: { token: 'canonical-example', query: effect.request.query }, rows: [], next: null, anchor: 'NotRequested', health: 'Ready'
  } } } })));
  assert.equal(complete.view.phase, 'empty');
  assert.deepEqual(complete.view.rows, []);
  assert.equal(typeof bridge.details_apply, 'function');
  assert.equal(typeof bridge.details_panel, 'function');
  const missingDetails = JSON.stringify({ version: 1, token: 'missing-inspection', input: JSON.stringify({ version: 1, action: { type: 'revoke' } }) });
  assert.throws(() => bridge.details_apply(missingDetails), error => String(error) === 'stale_ticket');
  assert.throws(() => bridge.details_panel(missingDetails), error => String(error) === 'stale_ticket');
  assert.equal(typeof binding.RssBridge.compose_query, 'function');
  const query = input.query;
  const source = query.branches[0].source;
  const composed = JSON.parse(binding.RssBridge.compose_query(JSON.stringify({
    version: 1, scope: query.scope, library_revision: query.library_revision,
    sources: [{ source_ref: source, owner_scope: query.scope, provider: 'syndication', original_url: 'https://example.org/feed', display_name: 'Example', revision: '1', paused: false }],
    groups: [], memberships: [], rules: [],
    selection: { owner_scope: query.scope, profile_ref: query.profile, selected_groups: [], selected_sources: [source] },
    selection_revision: query.selection_revision, authority_epoch: query.authority_epoch, authorized_sources: [source], access_partitions: query.access_partitions,
    focused_group: null, context: query.context, filter: query.filter, sort: query.sort, page_size: query.page_size, health: [], bounds: input.bounds
  })));
  assert.deepEqual(composed.query, query);
  assert.equal(composed.manager.sources[0].selected, true);
  const revoked = JSON.parse(bridge.apply(JSON.stringify({ version: 1, action: { type: 'revoke' } })));
  assert.equal(revoked.view.phase, 'revoked');
  assert.equal(revoked.view.query, null);
  assert.deepEqual(revoked.view.pending_tokens, []);
} finally { bridge.free(); }
console.log(JSON.stringify({ status: 'pass', checks: ['constructor_errors', 'direct_rss_bridge', 'exact_revision', 'actual_query_effect', 'actual_empty_completion', 'direct_details_boundary', 'direct_compose_query', 'actual_revoke'] }));
