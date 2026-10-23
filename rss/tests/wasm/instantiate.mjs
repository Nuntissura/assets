import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const [modulePath, fixturesPath] = process.argv.slice(2);
const binding = await import(pathToFileURL(modulePath));
const wasmPath = modulePath.replace(/\.js$/, '_bg.wasm');
await binding.default({ module_or_path: await readFile(wasmPath) });
const scenarios = JSON.parse(await readFile(fixturesPath, 'utf8'));
let steps = 0;
function actual(call) {
  try { return { ok: JSON.parse(call()) }; }
  catch (error) { return { error: String(error) }; }
}
for (const scenario of scenarios) {
  let bridge;
  try { bridge = new binding.RssBridge(scenario.initial); }
  catch (error) {
    assert.equal(String(error), scenario.constructor_error, `${scenario.name} constructor`);
    continue;
  }
  assert.equal(scenario.constructor_error, null, `${scenario.name} unexpected construction`);
  try {
    for (const step of scenario.steps) {
      const output = actual(() => step.method === 'snapshot' ? bridge.snapshot()
        : step.method === 'compose_query' ? binding.RssBridge.compose_query(step.input)
          : bridge[step.method](step.input));
      assert.deepEqual(output, step.expected, `${scenario.name} step ${steps} ${step.method}`);
      steps += 1;
    }
  } finally { bridge.free(); }
}
assert.ok(scenarios.length > 0 && steps > 0, 'nonempty actual compiled boundary proof');
console.log(JSON.stringify({ status: 'pass', scenarios: scenarios.length, steps }));
