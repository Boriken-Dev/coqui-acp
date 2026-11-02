// Every check bindings/js/src/conformance.mjs has. `strict-csp` is in the contract's table and not in that module, since
// it needs a browser; test/browser.test.mjs runs it.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runConformance } from '../../../bindings/js/src/conformance.mjs';
import { makeDriver } from './driver.mjs';

test('the conformance checks', { timeout: 60_000 }, async (t) => {
  let last;
  const results = await runConformance(async () => {
    await last?.close();
    last = await makeDriver();
    return last;
  });
  await last?.close();
  for (const { id, passed, why } of results) {
    await t.test(id, () => assert.ok(passed, why));
  }
});
