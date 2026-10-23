import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const [modulePath, fixturesPath] = process.argv.slice(2);
if (!modulePath || !fixturesPath) throw new Error('Expected generated module and native fixture paths');
const { default: init, HsscrapBridge } = await import(pathToFileURL(modulePath));
await init({ module_or_path: await readFile(modulePath.replace(/\.js$/, '_bg.wasm')) });
const cases = JSON.parse(await readFile(fixturesPath, 'utf8'));
let steps = 0;
for (const scenario of cases) {
  if (scenario.initial_error !== null) {
    assert.throws(() => new HsscrapBridge(scenario.initial), error => String(error) === scenario.initial_error, scenario.name);
    continue;
  }
  const bridge = new HsscrapBridge(scenario.initial);
  try {
    for (const step of scenario.steps) {
      const run = () => step.method === 'snapshot' ? bridge.snapshot() : bridge[step.method](step.input);
      if (step.error !== null) {
        assert.throws(run, error => String(error) === step.error, `${scenario.name}:${step.method}`);
      } else {
        assert.deepEqual(JSON.parse(run()), step.ok, `${scenario.name}:${step.method}`);
      }
      steps += 1;
    }
  } finally {
    bridge.free();
  }
}
console.log(JSON.stringify({ status: 'pass', cases: cases.length, steps, runtime: 'actual hsscrap WebAssembly', reference: 'native Rust bridge plus direct Session assertions' }));
