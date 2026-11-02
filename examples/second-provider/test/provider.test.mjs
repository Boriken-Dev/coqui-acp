// What the contract says and the conformance module does not check.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { registerProvider } from '../../../bindings/js/src/registry.mjs';
import { PROVIDER_UUID, loopbackUrl } from '../extension/provider.js';
import { makeDriver } from './driver.mjs';

const RPC = (id, method, params) => JSON.stringify({ jsonrpc: '2.0', id, method, params });
const ORIGIN = 'https://a.example';

async function withDriver(t, options) {
  const driver = await makeDriver(options);
  t.after(() => driver.close());
  return driver;
}

async function grant(driver, answer = {}, origin = ORIGIN) {
  driver.discover('agent');
  const page = driver.page(origin);
  page.activate();
  driver.person.answerNext({ allow: 'agent', cwd: '/granted', ...answer });
  const [agent] = await page.acp.request();
  return { page, agent };
}

function speak(connection) {
  const writer = connection.writable.getWriter();
  const reader = connection.readable.getReader();
  const seen = [];
  const until = async (id) => {
    for (;;) {
      const { value, done } = await reader.read();
      assert.equal(done, false, `the connection closed while waiting for ${id}`);
      const message = JSON.parse(value);
      if (message.id === id) return message;
      seen.push(message);
    }
  };
  return {
    seen,
    until,
    send: (text) => writer.write(text),
    call: async (id, method, params) => {
      await writer.write(RPC(id, method, params));
      return until(id);
    },
    settle: async (id) => {
      await writer.write(RPC(id, 'initialize', { protocolVersion: 1 }));
      return until(id);
    },
  };
}

async function connected(driver, answer) {
  const { page, agent } = await grant(driver, answer);
  const connection = await page.acp.connect(agent);
  return { page, agent, connection, ...speak(connection), peer: driver.agent('agent') };
}

const fakeProvider = (uuid) => ({
  info: { uuid, name: uuid, rdns: `test.${uuid}` },
  agents: async () => [],
  request: async () => [],
  connect: async () => ({}),
  revoke: async () => {},
});

test('the uuid the page sees is the one agents name', async (t) => {
  const driver = await withDriver(t);
  assert.equal(driver.page(ORIGIN).acp.providers[0].uuid, PROVIDER_UUID);
});

test('the registry interoperates with the algorithm in bindings/js, in either order', async (t) => {
  const driver = await withDriver(t);
  const page = driver.page(ORIGIN);
  const registry = registerProvider(fakeProvider('later'), page.window);
  assert.equal(registry, page.acp, 'a later provider started a second registry');
  assert.deepEqual(page.acp.providers.map((provider) => provider.uuid), [PROVIDER_UUID, 'later']);
  assert.equal(page.window.document.acp, registry);

  const other = new EventTarget();
  other.document = {};
  const first = registerProvider(fakeProvider('earlier'), other);
  assert.equal(first.providers.length, 1);
  // The same global symbol finds a registry however it was made.
  assert.equal(other[Symbol.for('coqui-acp.registry')], first);
});

test('onagentschange can be assigned, as the page API declares, and hears a grant and a revocation', async (t) => {
  const driver = await withDriver(t);
  driver.discover('agent');
  const page = driver.page(ORIGIN);
  let heard = 0;
  page.acp.onagentschange = () => {
    heard += 1;
  };
  page.activate();
  driver.person.answerNext({ allow: 'agent', cwd: '/granted' });
  const [agent] = await page.acp.request();
  await page.acp.agents();
  assert.equal(heard, 1);
  await page.acp.revoke(agent);
  await page.acp.agents();
  assert.equal(heard, 2);
});

test('nothing is defined in a frame or an insecure context', async (t) => {
  const driver = await withDriver(t);
  assert.equal(driver.page('https://frame.example', { top: false }).acp, undefined);
  assert.equal(driver.page('http://plain.example', { secure: false }).acp, undefined);
});

test('a category is the one the agent described, never the one the page claims', async (t) => {
  const driver = await withDriver(t);
  const { call, peer } = await connected(driver);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  const session = { sessionId: result.sessionId };
  // `m` is the agent's mode option. Calling it a model must not let it through.
  const lied = await call(2, 'session/set_config_option', { ...session, category: 'model', configId: 'm', value: 'bypass' });
  assert.equal(lied.error?.code, -32601);
  assert.equal(peer.received().filter((message) => message.method === 'session/set_config_option').length, 0);
  // ACP's own request has no category: `x` is the agent's model option.
  const honest = await call(3, 'session/set_config_option', { ...session, configId: 'x', value: 'two' });
  assert.equal(honest.error, undefined);
  const unknown = await call(4, 'session/set_config_option', { ...session, configId: 'never-described', value: 'y' });
  assert.equal(unknown.error?.code, -32601);
});

test('a grant with permissions may change the mode', async (t) => {
  const driver = await withDriver(t);
  const { call } = await connected(driver, { permissions: true });
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  assert.equal((await call(2, 'session/set_mode', { sessionId: result.sessionId, modeId: 'bypass' })).error, undefined);
  assert.equal((await call(3, 'session/set_config_option', { sessionId: result.sessionId, configId: 'm', value: 'x' })).error, undefined);
});

test('_meta goes at every depth', async (t) => {
  const driver = await withDriver(t);
  const { call, peer } = await connected(driver);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [{ type: 'acp', name: 'n', _meta: { launch: 1 } }] });
  const prompt = peer.received().find((message) => message.method === 'session/new');
  assert.ok(!JSON.stringify(prompt).includes('_meta'));
  await call(2, 'session/set_model', { sessionId: result.sessionId, modelId: 'm', nested: { _meta: { launch: 2 } } });
  assert.ok(!JSON.stringify(peer.received()).includes('_meta'));
});

test('anything that is not one JSON-RPC object is refused', async (t) => {
  const driver = await withDriver(t);
  const { send, until, peer, settle } = await connected(driver);
  for (const text of ['{"id":1,"method":"initialize"}', '[]', 'not json', '{"jsonrpc":"2.0","id":{},"method":"initialize"}']) {
    await send(text);
  }
  assert.equal((await until(null)).error.code, -32700);
  assert.equal((await until(null)).error.code, -32700);
  assert.equal((await until(null)).error.code, -32700);
  assert.equal((await until(null)).error.code, -32600);
  await settle(1);
  assert.equal(peer.received().length, 1);
});

test('the agent reaches the page only for this origin\'s sessions', async (t) => {
  const driver = await withDriver(t);
  const { call, peer, settle, seen } = await connected(driver);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  peer.send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: result.sessionId, update: { n: 'mine' } } });
  peer.send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: 'other', update: { n: 'theirs' } } });
  peer.send({ jsonrpc: '2.0', method: 'session/update', params: { update: { n: 'nobodys' } } });
  peer.send({ jsonrpc: '2.0', method: 'custom/note', params: { sessionId: 'other', n: 'theirs' } });
  peer.send({ jsonrpc: '2.0', method: 'custom/note', params: { n: 'general' } });
  peer.send({ jsonrpc: '2.0', id: 70, method: 'fs/read_text_file', params: { sessionId: 'other', path: '/x' } });
  peer.send({ jsonrpc: '2.0', id: 71, method: 'fs/read_text_file', params: { sessionId: result.sessionId, path: '/x' } });
  peer.send({ jsonrpc: '2.0', id: 72, method: 'session/request_permission', params: { options: [] } });
  await settle(2);
  assert.deepEqual(
    seen.map((message) => message.params?.update?.n ?? message.params?.n ?? message.id),
    ['mine', 'general', 71],
  );
  assert.equal(peer.received().find((message) => message.id === 70)?.error?.code, -32601);
  assert.equal(peer.received().find((message) => message.id === 72)?.error?.code, -32601, 'an agent was left waiting');
  assert.equal(driver.person.permissionsAsked().length, 0);
});

test('a page cannot answer a permission request it was never shown', async (t) => {
  const driver = await withDriver(t);
  const { call, peer, send, settle } = await connected(driver);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  peer.send({ jsonrpc: '2.0', id: 900, method: 'session/request_permission', params: { sessionId: result.sessionId, options: [{ optionId: 'yes', name: 'Yes' }] } });
  await settle(2);
  await send(JSON.stringify({ jsonrpc: '2.0', id: 900, result: { outcome: { outcome: 'selected', optionId: 'yes' } } }));
  await settle(3);
  assert.equal(peer.received().filter((message) => message.id === 900).length, 0);
});

test('a permission request nobody answers in time is cancelled, and so is one whose turn is cancelled', async (t) => {
  const driver = await withDriver(t, { permissionTimeoutMs: 30 });
  const { call, peer, settle } = await connected(driver);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  peer.send({ jsonrpc: '2.0', id: 900, method: 'session/request_permission', params: { sessionId: result.sessionId, options: [] } });
  await settle(2);
  await new Promise((resolve) => setTimeout(resolve, 80));
  await settle(3);
  assert.deepEqual(peer.received().find((message) => message.id === 900)?.result, { outcome: { outcome: 'cancelled' } });
});

test('cancelling a turn answers its pending permission request', async (t) => {
  const driver = await withDriver(t);
  const { call, peer, settle, send } = await connected(driver);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  peer.send({ jsonrpc: '2.0', id: 900, method: 'session/request_permission', params: { sessionId: result.sessionId, options: [] } });
  await settle(2);
  await send(JSON.stringify({ jsonrpc: '2.0', method: 'session/cancel', params: { sessionId: result.sessionId } }));
  await settle(3);
  await settle(4);
  assert.deepEqual(peer.received().find((message) => message.id === 900)?.result, { outcome: { outcome: 'cancelled' } });
});

test('the person\'s chosen option reaches the agent, and one not offered does not', async (t) => {
  const driver = await withDriver(t);
  const { call, peer, settle } = await connected(driver);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  const options = [{ optionId: 'yes', name: 'Yes' }];
  driver.person.onPermission = (_question, answer) => answer('yes');
  peer.send({ jsonrpc: '2.0', id: 900, method: 'session/request_permission', params: { sessionId: result.sessionId, options } });
  await settle(2);
  await settle(3);
  assert.deepEqual(peer.received().find((message) => message.id === 900)?.result, { outcome: { outcome: 'selected', optionId: 'yes' } });
  driver.person.onPermission = (_question, answer) => answer('invented');
  peer.send({ jsonrpc: '2.0', id: 901, method: 'session/request_permission', params: { sessionId: result.sessionId, options } });
  await settle(4);
  await settle(5);
  assert.deepEqual(peer.received().find((message) => message.id === 901)?.result, { outcome: { outcome: 'cancelled' } });
});

test('a `once` grant lasts across reloads in its tab, is not another tab\'s, and ends with its tab', async (t) => {
  const driver = await withDriver(t);
  const { page, agent } = await grant(driver);
  const connection = await page.acp.connect(agent);
  const { call } = speak(connection);
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  page.navigate();
  assert.equal((await connection.closed).reason, 'navigation');

  const reloaded = driver.page(ORIGIN);
  assert.equal((await reloaded.acp.agents()).length, 1);
  assert.equal((await reloaded.acp.agents())[0].id, agent.id, 'the id is not stable');

  const elsewhere = driver.page(ORIGIN, { newTab: true });
  assert.equal((await elsewhere.acp.agents()).length, 0);
  await assert.rejects(elsewhere.acp.connect(agent), { name: 'NotAllowedError' });

  const firstTab = reloaded.tabId;
  await driver.provider.tabClosed(firstTab);
  assert.equal((await reloaded.acp.agents()).length, 0);
  const asked = driver.page(ORIGIN);
  asked.activate();
  driver.person.answerNext({ allow: 'agent', cwd: '/granted' });
  const [again] = await asked.acp.request();
  const { call: callAgain } = speak(await asked.acp.connect(again));
  const forgotten = await callAgain(2, 'session/resume', { sessionId: result.sessionId, cwd: '/x', mcpServers: [] });
  assert.equal(forgotten.error?.code, -32601, 'a session outlived its grant');
});

test('grants, ids and sessions survive the service worker stopping', async (t) => {
  const driver = await withDriver(t);
  const { page, agent } = await grant(driver, { lifetime: 'always' });
  const { call } = speak(await page.acp.connect(agent));
  const { result } = await call(1, 'session/new', { cwd: '/x', mcpServers: [] });
  await new Promise((resolve) => setTimeout(resolve, 10));
  driver.restartProvider();
  const fresh = driver.page(ORIGIN, { newTab: true });
  const [again] = await fresh.acp.agents();
  assert.equal(again.id, agent.id);
  const { call: callAgain } = speak(await fresh.acp.connect(again));
  assert.equal((await callAgain(2, 'session/resume', { sessionId: result.sessionId, cwd: '/x', mcpServers: [] })).error, undefined);
});

test('the person can remove and revoke outside any request, and a live connection closes', async (t) => {
  const driver = await withDriver(t);
  const { page, agent, connection } = await connected(driver);
  const before = await driver.provider.state();
  assert.equal(before.agents.length, 1);
  assert.equal(before.grants.length, 1);
  await driver.provider.personRevokes(before.grants[0].uid);
  assert.equal((await connection.closed).reason, 'revoked');
  await assert.rejects(page.acp.connect(agent), { name: 'NotAllowedError' });

  page.activate();
  driver.person.answerNext({ allow: 'agent', cwd: '/granted' });
  const [granted] = await page.acp.request();
  const live = await page.acp.connect(granted);
  await driver.provider.personRemoves(before.agents[0].key);
  assert.equal((await live.closed).reason, 'revoked');
  assert.equal((await page.acp.agents()).length, 0);
});

test('only this machine can be added with no proof', async (t) => {
  const driver = await withDriver(t);
  assert.equal(await driver.provider.personAdds({ url: 'ws://10.0.0.1/acp', understood: true }), false);
  assert.equal(await driver.provider.personAdds({ url: 'ws://127.0.0.1:1/acp', understood: false }), false);
  assert.equal(await driver.provider.personAdds({ url: 'ws://127.0.0.1:1/acp', understood: true }), true);
  assert.equal(loopbackUrl('ws://user@127.0.0.1/'), null);
  assert.equal(loopbackUrl('ws://127.0.0.1.example/'), null);
  assert.equal(loopbackUrl('http://127.0.0.1/'), null);
  assert.equal(loopbackUrl('ws://[::1]:9/acp'), 'ws://[::1]:9/acp');
});

test('one window per tab while a request is open, and a page that leaves closes it', async (t) => {
  const driver = await withDriver(t);
  driver.discover('agent');
  const page = driver.page(ORIGIN);
  page.activate();
  const first = page.acp.request();
  await assert.rejects(page.acp.request(), { name: 'NotAllowedError' });
  page.navigate();
  await assert.rejects(first, { name: 'NetworkError' });
  const next = driver.page(ORIGIN);
  await next.acp.agents();
  next.activate();
  driver.person.answerNext({ allow: 'agent', cwd: '/granted' });
  assert.equal((await next.acp.request()).length, 1, 'the tab stayed locked after its page left');
});

test('how a connection ends', async (t) => {
  const driver = await withDriver(t);
  const { page, agent } = await grant(driver);
  const connection = await page.acp.connect(agent);
  const peer = driver.agent('agent');
  peer.closeAll();
  assert.equal((await connection.closed).reason, 'agent');
  assert.equal((await connection.readable.getReader().read()).done, true);

  const second = await page.acp.connect(agent);
  peer.destroyAll();
  assert.equal((await second.closed).reason, 'failed');

  const third = await page.acp.connect(agent);
  third.close();
  assert.equal((await third.closed).reason, 'page');
  await assert.rejects(third.writable.getWriter().write('{}'), { name: 'InvalidStateError' });
});

test('a signal ends the wait for a connection and not the connection', async (t) => {
  const driver = await withDriver(t);
  const { page, agent } = await grant(driver);
  const already = new AbortController();
  already.abort();
  await assert.rejects(page.acp.connect(agent, { signal: already.signal }), { name: 'AbortError' });

  const later = new AbortController();
  const connection = await page.acp.connect(agent, { signal: later.signal });
  later.abort();
  const { settle } = speak(connection);
  assert.equal((await settle(1)).id, 1);
});

test('an agent nobody answers at is a NetworkError', async (t) => {
  const driver = await withDriver(t);
  const page = driver.page(ORIGIN);
  // Port 1 refuses at once on loopback.
  await driver.provider.personAdds({ url: 'ws://127.0.0.1:1/acp', label: 'dead', understood: true });
  const { agents } = await driver.provider.state();
  page.activate();
  driver.person.answerNext({ allow: agents[0].label, cwd: '/granted' });
  const [dead] = await page.acp.request();
  assert.equal(dead.identity.verified, false);
  await assert.rejects(page.acp.connect(dead), { name: 'NetworkError' });
});
