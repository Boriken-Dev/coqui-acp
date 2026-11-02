import assert from 'node:assert/strict';
import { test } from 'node:test';

import { exposeWebMcp, findAcp, registerProvider, Scope } from '../src/index.mjs';
import { CHECKS, runConformance } from '../src/conformance.mjs';
import { makeReferenceDriver } from './reference.mjs';

const provider = (uuid) => ({
  info: { uuid, name: uuid, rdns: `dev.example.${uuid}` },
  agents: async () => [{ id: `${uuid}-agent`, provider: uuid }],
  request: async () => [{ id: `${uuid}-agent`, provider: uuid }],
  connect: async (agent) => ({ agent }),
  revoke: async () => {},
});

class FakeWindow extends EventTarget {
  document = {};
}

test('the reference provider passes every conformance check', async () => {
  const results = await runConformance(makeReferenceDriver);
  assert.equal(results.length, Object.keys(CHECKS).length);
  assert.deepEqual(results.filter((result) => !result.passed), []);
});

test('the conformance checks catch a provider that passes everything through', async () => {
  class Open extends Scope {
    fromPage(text) {
      return { forward: text };
    }
    fromAgent(text) {
      return { forward: text };
    }
  }
  const original = Scope.prototype.fromPage;
  const originalAgent = Scope.prototype.fromAgent;
  Scope.prototype.fromPage = Open.prototype.fromPage;
  Scope.prototype.fromAgent = Open.prototype.fromAgent;
  try {
    const failed = (await runConformance(makeReferenceDriver)).filter((result) => !result.passed).map((result) => result.id);
    for (const id of ['no-stdio', 'mcp-origin', 'cwd', 'meta', 'sessions', 'unknown-methods', 'modes', 'permission-to-person', 'duplicate-keys']) {
      assert.ok(failed.includes(id), `${id} did not notice`);
    }
  } finally {
    Scope.prototype.fromPage = original;
    Scope.prototype.fromAgent = originalAgent;
  }
});

test('two providers share one registry, the earlier stays, and each agent routes to its own', async () => {
  const win = new FakeWindow();
  const first = registerProvider(provider('one'), win);
  const second = registerProvider(provider('two'), win);
  assert.equal(first, second);
  assert.equal(win.document.acp, first);
  assert.deepEqual(first.providers.map((info) => info.uuid), ['one', 'two']);
  assert.equal((await first.agents()).length, 2);
  assert.equal((await first.connect({ id: 'x', provider: 'two' })).agent.provider, 'two');
  await assert.rejects(first.connect({ id: 'x', provider: 'three' }), { name: 'NotFoundError' });
  assert.equal((await first.request())[0].provider, 'one');
  assert.equal((await first.request({ provider: 'two' }))[0].provider, 'two');
});

test('a page finds a provider by event, whichever arrived first', async () => {
  const early = new FakeWindow();
  registerProvider(provider('one'), early);
  assert.equal((await findAcp({ win: early })).version, 1);

  const none = new FakeWindow();
  assert.equal(await findAcp({ win: none, timeoutMs: 5 }), null);

  const late = new FakeWindow();
  const finding = findAcp({ win: late, timeoutMs: 200 });
  registerProvider(provider('one'), late);
  assert.equal((await finding).version, 1);
});

test('markup shadowing document.acp hides it from the property, not from the event or from a later provider', async () => {
  const win = new FakeWindow();
  const registry = registerProvider(provider('one'), win);
  win.document = { acp: { name: 'an img element' } };
  assert.equal(registerProvider(provider('two'), win), registry);
  assert.equal(registry.providers.length, 2);
  assert.equal(await findAcp({ win }), registry);
});

test('a registry that is not ours is replaced rather than trusted', async () => {
  const win = new FakeWindow();
  win.document.acp = { name: 'an img element' };
  const registry = registerProvider(provider('one'), win);
  assert.equal(registry.version, 1);
  assert.equal(win.document.acp, registry);
});

const scope = (grant = {}) => new Scope({ cwd: '/granted', ...grant }, 'https://a.example');
const rpc = (id, method, params) => JSON.stringify({ jsonrpc: '2.0', id, method, params });

test('what cannot be parsed, or is a batch, is refused', () => {
  for (const text of ['nonsense', '[]', '[{"jsonrpc":"2.0","id":1,"method":"initialize"}]', '1', 'null']) {
    assert.equal(JSON.parse(scope().fromPage(text).reply).error.code, -32700, text);
  }
});

test('a session id is learned only from the answer to session/new', () => {
  const s = scope();
  assert.ok(s.fromPage(rpc(1, 'session/prompt', { sessionId: 's1' })).reply);
  assert.ok(s.fromPage(rpc(2, 'session/new', {})).forward);
  s.fromAgent(JSON.stringify({ jsonrpc: '2.0', id: 2, result: { sessionId: 's1' } }));
  assert.ok(s.fromPage(rpc(3, 'session/prompt', { sessionId: 's1' })).forward);
  assert.ok(s.fromPage(rpc(4, 'session/load', { sessionId: 's1', mcpServers: [] })).forward);
});

test('a session of this origin from an earlier connection may be resumed, and only that', () => {
  const s = scope({ sessions: new Set(['old']) });
  assert.equal(JSON.parse(s.fromPage(rpc(1, 'session/resume', { sessionId: 'old', cwd: '/x' })).forward).params.cwd, '/granted');
  assert.ok(s.fromPage(rpc(2, 'session/resume', { sessionId: 'other' })).reply);
  assert.ok(s.fromPage(rpc(3, 'session/load', { sessionId: 'old', mcpServers: [{ command: '/bin/sh' }] })).reply);
});

test('no grant of a working directory means no session', () => {
  assert.ok(new Scope({ cwd: null }, 'https://a.example').fromPage(rpc(1, 'session/new', {})).reply);
});

test('mcpOrigins widens exactly what it names, and an entry cannot hide a command', () => {
  const s = scope({ mcpOrigins: ['https://tools.example'] });
  const session = (server) => rpc(1, 'session/new', { mcpServers: [server] });
  assert.ok(s.fromPage(session({ type: 'http', name: 'x', url: 'https://tools.example/mcp' })).forward);
  assert.ok(s.fromPage(session({ type: 'http', name: 'x', url: 'https://tools.example.evil.example/mcp' })).reply);
  assert.ok(s.fromPage(session({ type: 'http', name: 'x', url: 'https://a.example/mcp', command: '/bin/sh' })).reply);
  assert.ok(s.fromPage(session({ type: 'stdio', name: 'x' })).reply);
  assert.ok(s.fromPage(session({ name: 'x' })).reply);
  assert.ok(s.fromPage(session({ type: 'acp', name: 'x', serverId: 'y' })).forward);
  assert.ok(s.fromPage(rpc(1, 'session/new', { mcpServers: 'nope' })).reply);
});

test('a notification that is not allowed is dropped, with nothing to answer', () => {
  assert.deepEqual(scope().fromPage(JSON.stringify({ jsonrpc: '2.0', method: 'session/cancel', params: { sessionId: 'x' } })), {});
  assert.deepEqual(scope().fromPage(JSON.stringify({ jsonrpc: '2.0', method: '_vendor/x' })), {});
});

test('a page may answer only what the agent asked it', () => {
  const s = scope({ sessions: new Set(['s1']) });
  assert.deepEqual(s.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 7, result: {} })), {});
  assert.ok(s.fromAgent(rpc(7, 'fs/read_text_file', { sessionId: 's1', path: '/x' })).forward);
  assert.ok(s.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 7, result: {} })).forward);
  assert.deepEqual(s.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 7, result: {} })), {}, 'only once');
});

test('the agent cannot reach the page about a session of another origin', () => {
  const s = scope({ sessions: new Set(['s1']) });
  assert.deepEqual(s.fromAgent(JSON.stringify({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: 'other' } })), {});
  assert.ok(s.fromAgent(rpc(1, 'fs/read_text_file', { sessionId: 'other' })).reply);
  assert.ok(s.fromAgent(JSON.stringify({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: 's1' } })).forward);
});

test('a permission request goes to the person, or to the page only under that grant', () => {
  const request = rpc(5, 'session/request_permission', { sessionId: 's1', options: [] });
  const person = scope({ sessions: new Set(['s1']) });
  const step = person.fromAgent(request);
  assert.ok(step.ask);
  assert.equal(step.forward, undefined);
  assert.equal(JSON.parse(person.answerPermission(step.ask, 'yes')).result.outcome.optionId, 'yes');
  assert.equal(JSON.parse(person.answerPermission(step.ask, null)).result.outcome.outcome, 'cancelled');

  const page = scope({ sessions: new Set(['s1']), permissions: true });
  assert.ok(page.fromAgent(request).forward);
  assert.ok(page.fromPage(rpc(9, 'session/set_mode', { sessionId: 's1', modeId: 'x' })).forward);
  assert.ok(page.fromPage(rpc(9, 'session/set_mode', { sessionId: 'other', modeId: 'x' })).reply);
});

const webMcp = (tools) => ({
  getTools: async () => tools,
  executeTool: async (tool, input) => `${tool.name}:${JSON.stringify(input)}`,
});
const call = (server, id, method, params) => ({ jsonrpc: '2.0', id, method: 'mcp/message', params: { serverId: server.serverId, requestId: 'r', method, params } });

test('WebMCP tools reach an agent as MCP over ACP, and a consequential one needs confirming', async () => {
  const tools = [
    { name: 'read', description: 'r', inputSchema: { type: 'object' }, annotations: { readOnlyHint: true } },
    { name: 'pay', description: 'p', inputSchema: { type: 'object' }, annotations: { consequentialHint: true } },
  ];
  const refused = exposeWebMcp({ modelContext: webMcp(tools), name: 'p' });
  assert.equal(refused.server.type, 'acp');
  const listed = await refused.handle(call(refused.server, 1, 'tools/list'));
  assert.deepEqual(listed.result.tools.map((tool) => tool.name), ['read', 'pay']);
  const read = await refused.handle(call(refused.server, 2, 'tools/call', { name: 'read', arguments: { a: 1 } }));
  assert.equal(read.result.content[0].text, 'read:{"a":1}');
  const unconfirmed = await refused.handle(call(refused.server, 3, 'tools/call', { name: 'pay', arguments: {} }));
  assert.equal(unconfirmed.result.isError, true);

  const confirmed = exposeWebMcp({ modelContext: webMcp(tools), name: 'p', confirm: async () => true });
  const paid = await confirmed.handle(call(confirmed.server, 4, 'tools/call', { name: 'pay', arguments: {} }));
  assert.equal(paid.result.content[0].text, 'pay:{}');
});

test('a filter hides a tool from listing and calling, and another message is not answered', async () => {
  const tools = [{ name: 'secret' }, { name: 'open' }];
  const exposed = exposeWebMcp({ modelContext: webMcp(tools), filter: (tool) => tool.name === 'open' });
  assert.deepEqual((await exposed.handle(call(exposed.server, 1, 'tools/list'))).result.tools.map((tool) => tool.name), ['open']);
  assert.ok((await exposed.handle(call(exposed.server, 2, 'tools/call', { name: 'secret' }))).error);
  assert.equal(await exposed.handle({ method: 'mcp/message', id: 3, params: { serverId: 'someone-else' } }), null);
  assert.equal(await exposed.handle({ method: 'session/update' }), null);
});

test('a document without WebMCP says so', () => {
  assert.throws(() => exposeWebMcp({ modelContext: undefined }), { name: 'NotSupportedError' });
});

test('a category is what the agent said of an option, and a page claiming one means nothing', () => {
  const s = scope({ sessions: new Set(['s1']) });
  const set = (id, configId, extra = {}) => s.fromPage(JSON.stringify({ jsonrpc: '2.0', id, method: 'session/set_config_option', params: { sessionId: 's1', configId, value: 'x', ...extra } }));
  assert.ok(set(1, 'm', { category: 'model' }).reply, 'never described, whatever the page says');
  s.fromAgent(JSON.stringify({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: 's1', update: { sessionUpdate: 'config_option_update', configOptions: [{ id: 'm', category: 'mode' }, { id: 'e', category: 'thought_level' }, { id: 'x' }] } } }));
  assert.ok(set(2, 'm', { category: 'model' }).reply, 'a mode option, whatever the page says');
  assert.ok(set(3, 'x').reply, 'no category is not an open one');
  assert.ok(set(4, 'e').forward, 'the agent said this one is an effort');
  const granted = scope({ sessions: new Set(['s1']), permissions: true });
  assert.ok(JSON.parse(granted.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 5, method: 'session/set_config_option', params: { sessionId: 's1', configId: 'm', value: 'x' } })).forward), 'a grant with permissions may');
});

test('what an agent says of an option when a session opens counts too, for that session only', () => {
  const s = scope();
  s.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'session/new', params: { cwd: '/x', mcpServers: [] } }));
  s.fromAgent(JSON.stringify({ jsonrpc: '2.0', id: 1, result: { sessionId: 's1', configOptions: [{ id: 'e', category: 'model' }] } }));
  s.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'session/new', params: { cwd: '/x', mcpServers: [] } }));
  s.fromAgent(JSON.stringify({ jsonrpc: '2.0', id: 2, result: { sessionId: 's2', configOptions: [{ id: 'e', category: 'mode' }] } }));
  const set = (id, sessionId) => s.fromPage(JSON.stringify({ jsonrpc: '2.0', id, method: 'session/set_config_option', params: { sessionId, configId: 'e', value: 'x' } }));
  assert.ok(set(3, 's1').forward);
  assert.ok(set(4, 's2').reply);
});

test('_meta goes at every depth toward the agent, except what MCP carries through it', () => {
  const s = scope({ sessions: new Set(['s1']) });
  const sent = JSON.parse(s.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 1, _meta: { a: 1 }, method: 'session/prompt', params: { _meta: { b: 1 }, sessionId: 's1', prompt: [{ type: 'text', text: 'hi', _meta: { c: 1 } }] } })).forward);
  assert.equal(JSON.stringify(sent).includes('_meta'), false);
  s.fromAgent(JSON.stringify({ jsonrpc: '2.0', id: 9, method: 'mcp/message', params: { connectionId: 'c', message: {} } }));
  const answer = JSON.parse(s.fromPage(JSON.stringify({ jsonrpc: '2.0', id: 9, _meta: { a: 1 }, result: { content: [{ _meta: { theirs: 1 } }] } })).forward);
  assert.equal(answer._meta, undefined);
  assert.deepEqual(answer.result.content[0]._meta, { theirs: 1 });
});

test('a message about no session of this page never reaches it, or the person', () => {
  const s = scope({ sessions: new Set(['s1']) });
  assert.deepEqual(s.fromAgent(JSON.stringify({ jsonrpc: '2.0', method: 'session/update', params: {} })), {});
  const refused = s.fromAgent(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'session/request_permission', params: { options: [] } }));
  assert.equal(refused.ask, undefined);
  assert.ok(JSON.parse(refused.reply).error);
});

test('a message with no jsonrpc member, or an id a number cannot hold, is refused', () => {
  const s = scope();
  assert.equal(JSON.parse(s.fromPage(JSON.stringify({ id: 1, method: 'initialize' })).reply).error.code, -32700);
  assert.equal(JSON.parse(s.fromPage('{"jsonrpc":"2.0","id":9007199254740993,"method":"initialize"}').reply).error.code, -32700);
  assert.ok(s.fromPage('{"jsonrpc":"2.0","id":9007199254740991,"method":"initialize"}').forward);
  assert.ok(s.fromPage('{"jsonrpc":"2.0","id":"a string","method":"initialize"}').forward);
});
