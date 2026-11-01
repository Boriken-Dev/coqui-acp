// The checks in docs/browser.md "Conformance", against any provider, through a driver.
//
// driver.page(origin) -> a page: { acp, activate(), navigate() }
// driver.person.answerNext(answer): what the person does at the next prompt: { allow: agentLabel or a list of them, add?: labels they
//   add by hand during it, cwd, permissions?, mcpOrigins? } or { refuse: true }. driver.person.permissionsAsked(): the permission
//   requests put to the person.
// driver.discover(label): an agent the provider has found and nobody has granted.
// driver.agent(label): what the agent received: { received(): objects[], send(object) }

const RPC = (id, method, params) => JSON.stringify({ jsonrpc: '2.0', id, method, params });

async function rejectsWith(promise, name) {
  try {
    await promise;
  } catch (error) {
    if (error?.name === name) return;
    throw new Error(`expected ${name}, got ${error?.name ?? error}`);
  }
  throw new Error(`expected ${name}, and it succeeded`);
}

function check(condition, why) {
  if (!condition) throw new Error(why);
}

async function grantAndConnect(driver, origin, grant = {}) {
  const label = grant.label ?? 'agent';
  driver.discover(label);
  const page = driver.page(origin);
  page.activate();
  driver.person.answerNext({ allow: label, cwd: '/granted', ...grant });
  const [agent] = await page.acp.request();
  const connection = await page.acp.connect(agent);
  const writer = connection.writable.getWriter();
  const reader = connection.readable.getReader();
  const seen = driver.agent(label);
  return {
    page,
    agent,
    connection,
    seen,
    send: (text) => writer.write(text),
    next: async () => JSON.parse((await reader.read()).value),
    // Round trip through the agent's own answer, so whatever went before it has arrived.
    settle: async (id) => {
      await writer.write(RPC(id, 'initialize', { protocolVersion: 1 }));
      for (;;) {
        const message = JSON.parse((await reader.read()).value);
        if (message.id === id) return message;
      }
    },
    openSession: async (id, params = {}) => {
      await writer.write(RPC(id, 'session/new', { cwd: '/mine', mcpServers: [], ...params }));
      for (;;) {
        const message = JSON.parse((await reader.read()).value);
        if (message.id === id) return message;
      }
    },
  };
}

const lastOf = (seen, method) => seen.received().filter((message) => message.method === method).at(-1);

export const CHECKS = {
  async registry(driver) {
    const page = driver.page('https://a.example');
    check(page.acp?.version === 1, 'no registry at version 1');
    check(page.acp.providers.length >= 1, 'no provider registered');
    let answered;
    page.window.addEventListener('acp:ready', (event) => {
      answered = event.detail.acp;
    });
    page.window.dispatchEvent(new Event('acp:request'));
    check(answered === page.acp, 'acp:request was not answered');
  },
  async 'no-grant-no-agents'(driver) {
    driver.discover('agent');
    check((await driver.page('https://a.example').acp.agents()).length === 0, 'an ungranted agent was shown');
  },
  async 'no-grant-no-connection'(driver) {
    driver.discover('agent');
    const page = driver.page('https://a.example');
    const provider = page.acp.providers[0].uuid;
    await rejectsWith(page.acp.connect({ id: 'anything', provider }), 'NotAllowedError');
  },
  async activation(driver) {
    driver.discover('agent');
    driver.person.answerNext({ allow: 'agent', cwd: '/granted' });
    await rejectsWith(driver.page('https://a.example').acp.request(), 'NotAllowedError');
  },
  async 'person-refuses'(driver) {
    driver.discover('agent');
    const page = driver.page('https://a.example');
    page.activate();
    driver.person.answerNext({ refuse: true });
    await rejectsWith(page.acp.request(), 'NotAllowedError');
    check((await page.acp.agents()).length === 0, 'a refusal was remembered as a grant');
  },
  async 'person-chooses'(driver) {
    for (const label of ['first', 'second', 'not-chosen']) driver.discover(label);
    const page = driver.page('https://a.example');
    page.activate();
    driver.person.answerNext({ allow: ['first', 'second', 'added-by-hand'], add: ['added-by-hand'], cwd: '/granted' });
    const granted = await page.acp.request();
    const labels = (agents) => agents.map((agent) => agent.label).sort();
    check(labels(granted).join() === 'added-by-hand,first,second', 'the page did not get what the person chose');
    check(labels(await page.acp.agents()).join() === 'added-by-hand,first,second', 'the grants were not kept');
    check(!JSON.stringify(granted).includes('not-chosen'), 'an agent the person did not choose was shown');
  },
  async 'per-origin'(driver) {
    const first = await grantAndConnect(driver, 'https://a.example');
    const other = driver.page('https://b.example');
    check((await other.acp.agents()).length === 0, 'another origin saw the grant');
    await rejectsWith(other.acp.connect(first.agent), 'NotAllowedError');
    other.activate();
    driver.person.answerNext({ allow: 'agent', cwd: '/granted' });
    const [mine] = await other.acp.request();
    check(mine.id !== first.agent.id, 'two origins share an id');
  },
  async messages(driver) {
    const { send, next, seen } = await grantAndConnect(driver, 'https://a.example');
    for (let id = 1; id <= 5; id += 1) await send(RPC(id, 'initialize', { protocolVersion: 1 }));
    for (let id = 1; id <= 5; id += 1) check((await next()).id === id, 'answers out of order');
    check(seen.received().filter((message) => message.method === 'initialize').length === 5, 'messages were lost');
  },
  async revoke(driver) {
    const { page, agent, connection } = await grantAndConnect(driver, 'https://a.example');
    await page.acp.revoke(agent);
    check((await connection.closed).reason === 'revoked', 'not closed as revoked');
    await rejectsWith(page.acp.connect(agent), 'NotAllowedError');
  },
  async navigation(driver) {
    const { page, connection } = await grantAndConnect(driver, 'https://a.example');
    page.navigate();
    check((await connection.closed).reason === 'navigation', 'not closed by navigation');
  },
  async 'no-stdio'(driver) {
    const { seen, openSession, settle } = await grantAndConnect(driver, 'https://a.example');
    const answer = await openSession(1, { mcpServers: [{ name: 'x', command: '/bin/sh', args: ['-c', 'id'], env: [] }] });
    check(answer.error !== undefined, 'a stdio server was not refused');
    await settle(2);
    check(lastOf(seen, 'session/new') === undefined, 'a stdio server reached the agent');
  },
  async 'mcp-origin'(driver) {
    const { seen, openSession, settle } = await grantAndConnect(driver, 'https://a.example');
    for (const url of ['https://b.example/mcp', 'http://a.example/mcp', 'https://10.0.0.1/mcp', 'not a url']) {
      const answer = await openSession(url.length, { mcpServers: [{ type: 'http', name: 'x', url, headers: [] }] });
      check(answer.error !== undefined, `${url} was not refused`);
    }
    await settle(99);
    check(lastOf(seen, 'session/new') === undefined, 'a foreign server reached the agent');
    const ok = await openSession(100, { mcpServers: [{ type: 'http', name: 'x', url: 'https://a.example/mcp', headers: [] }] });
    check(ok.error === undefined, "the page's own origin was refused");
  },
  async cwd(driver) {
    const { seen, openSession } = await grantAndConnect(driver, 'https://a.example');
    await openSession(1, { cwd: '/etc' });
    check(lastOf(seen, 'session/new').params.cwd === '/granted', "the page's cwd reached the agent");
  },
  async meta(driver) {
    const { seen, send, settle } = await grantAndConnect(driver, 'https://a.example');
    await send(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', _meta: { a: 1 }, params: { protocolVersion: 1, _meta: { launch: 'x' } } }));
    await settle(2);
    const got = seen.received().find((message) => message.id === 1);
    check(got._meta === undefined && got.params._meta === undefined, '_meta reached the agent');
  },
  async sessions(driver) {
    const { seen, send, next, openSession } = await grantAndConnect(driver, 'https://a.example');
    await openSession(1);
    for (const [id, method] of [
      [2, 'session/load'],
      [3, 'session/prompt'],
      [4, 'session/list'],
    ]) {
      await send(RPC(id, method, { sessionId: 'someone-elses', cwd: '/x', mcpServers: [] }));
      check((await next()).error !== undefined, `${method} on another session was not refused`);
      check(lastOf(seen, method) === undefined, `${method} reached the agent`);
    }
  },
  async 'unknown-methods'(driver) {
    const { seen, send, next } = await grantAndConnect(driver, 'https://a.example');
    for (const [id, method] of [
      [1, 'authenticate'],
      [2, '_vendor/secret'],
      [3, 'whatever'],
    ]) {
      await send(RPC(id, method, {}));
      check((await next()).error?.code === -32601, `${method} was not refused`);
      check(lastOf(seen, method) === undefined, `${method} reached the agent`);
    }
  },
  async modes(driver) {
    const { seen, send, next, openSession } = await grantAndConnect(driver, 'https://a.example');
    const opened = await openSession(1);
    await send(RPC(2, 'session/set_mode', { sessionId: opened.result.sessionId, modeId: 'bypass' }));
    check((await next()).error !== undefined, 'a mode change was not refused');
    check(lastOf(seen, 'session/set_mode') === undefined, 'a mode change reached the agent');
    const session = opened.result.sessionId;
    await send(RPC(3, 'session/set_config_option', { sessionId: session, category: 'mode', configId: 'm', value: 'x' }));
    check((await next()).error !== undefined, 'a mode option was not refused');
    await send(RPC(4, 'session/set_config_option', { sessionId: session, category: 'something-new', configId: 'm', value: 'x' }));
    check((await next()).error !== undefined, 'an unknown category was not refused');
    await send(RPC(5, 'session/set_model', { sessionId: session, modelId: 'm' }));
    check((await next()).error === undefined, 'choosing a model was refused');
    await send(RPC(6, 'session/set_config_option', { sessionId: session, category: 'thought_level', configId: 'e', value: 'high' }));
    check((await next()).error === undefined, 'choosing an effort was refused');
  },
  async resume(driver) {
    const first = await grantAndConnect(driver, 'https://a.example');
    const opened = await first.openSession(1);
    first.page.navigate();
    const page = driver.page('https://a.example');
    const [agent] = await page.acp.agents();
    const again = await page.acp.connect(agent);
    const writer = again.writable.getWriter();
    const reader = again.readable.getReader();
    await writer.write(RPC(2, 'session/resume', { sessionId: opened.result.sessionId, cwd: '/x', mcpServers: [] }));
    check(JSON.parse((await reader.read()).value).error === undefined, 'its own session could not be resumed');
    await writer.write(RPC(3, 'session/resume', { sessionId: 'someone-elses', cwd: '/x', mcpServers: [] }));
    check(JSON.parse((await reader.read()).value).error !== undefined, 'another session was resumed');
  },
  async verified(driver) {
    driver.discover('unproven', { proven: false });
    const page = driver.page('https://a.example');
    page.activate();
    driver.person.answerNext({ allow: 'unproven', cwd: '/granted' });
    const [agent] = await page.acp.request();
    check(agent.identity.verified === false, 'an unproven key was reported as verified');
  },
  async 'permission-to-person'(driver) {
    const { seen, settle, openSession } = await grantAndConnect(driver, 'https://a.example');
    const opened = await openSession(1);
    seen.send({ jsonrpc: '2.0', id: 900, method: 'session/request_permission', params: { sessionId: opened.result.sessionId, toolCall: { toolCallId: 't' }, options: [] } });
    const reply = await settle(2);
    check(reply.id === 2, 'the page saw a permission request');
    check(driver.person.permissionsAsked().length === 1, 'the person was not asked');
  },
  async 'duplicate-keys'(driver) {
    const { seen, send, settle } = await grantAndConnect(driver, 'https://a.example');
    await send('{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/x","mcpServers":[],"mcpServers":[{"name":"x","command":"/bin/sh"}]}}');
    await send('{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/x","mcpServers":[{"name":"x","command":"/bin/sh"}],"mcpServers":[]}}');
    await settle(3);
    for (const got of seen.received().filter((message) => message.method === 'session/new')) {
      check(!JSON.stringify(got).includes('/bin/sh'), 'a duplicate key reached the agent');
    }
  },
};

/** Runs every check, each against a fresh provider from `makeDriver`. Returns [{ id, passed, why }]. */
export async function runConformance(makeDriver) {
  const results = [];
  for (const [id, run] of Object.entries(CHECKS)) {
    try {
      await run(await makeDriver());
      results.push({ id, passed: true });
    } catch (error) {
      results.push({ id, passed: false, why: String(error?.message ?? error) });
    }
  }
  return results;
}
