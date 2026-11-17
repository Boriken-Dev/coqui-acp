// The checks in docs/browser.md "Conformance", against any provider, through a driver.
//
// driver.page(origin) -> a page: { acp, activate(), navigate() }
// driver.person.answerNext(answer): what the person does at the next prompt: { allow: agentLabel or a list of them, add?: labels they
//   add by hand during it, cwd, lifetime? ('once', the default, or 'always'), permissions?, mcpOrigins? } or { refuse: true }. driver.person.permissionsAsked(): the permission
//   requests put to the person.
// driver.discover(label): an agent the provider has found and nobody has granted.
// driver.agent(label): what the agent received: { received(): objects[], send(object) }. The agent answers `session/new` with a
//   `sessionId`, and any other request with an empty result.
// driver.page(origin).window, when given: the window the provider's registry is defined on, for `registry`.
// driver.timeoutMs, when given: the time bound of each check, in place of 10 seconds.
// `strict-csp` needs a real browser, so it is not here: run it as the document says.

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
    writer,
    reader,
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
    check(Object.isFrozen(page.acp) && Object.isFrozen(page.acp.providers), 'the registry or its providers are not frozen');
    for (const member of ['agents', 'request', 'connect', 'revoke', 'register', 'addEventListener', 'removeEventListener']) {
      check(typeof page.acp[member] === 'function', `the registry has no ${member}()`);
    }
    check(page.acp.onagentschange === null, 'onagentschange does not start as null');
    const win = page.window;
    const held = Object.getOwnPropertyDescriptor(win, Symbol.for('coqui-acp.registry'));
    check(held?.value === page.acp && !held.writable && !held.configurable, "the window does not hold the registry under Symbol.for('coqui-acp.registry') as a fixed value");
    const prop = Object.getOwnPropertyDescriptor(win.document, 'acp');
    check(prop?.value === page.acp && prop.enumerable && !prop.writable && !prop.configurable, 'document.acp is not a fixed, enumerable value');

    // A later provider joins the registry and leaves the earlier ones where they were.
    const before = [...page.acp.providers];
    const later = {
      info: { uuid: 'conformance-later-provider', name: 'Later', rdns: 'dev.example.later' },
      agents: async () => [],
      request: async () => [],
      connect: async () => {
        throw new DOMException('no', 'NotAllowedError');
      },
      revoke: async () => {},
    };
    let announced;
    win.addEventListener('acp:ready', (event) => {
      announced = event.detail.acp;
    });
    page.acp.register(later);
    check(announced === page.acp, 'registering did not dispatch acp:ready');
    const after = page.acp.providers;
    check(after.length === before.length + 1, 'a later provider was not added');
    check(before.every((info, index) => after[index] === info), 'an earlier provider was dropped or reordered');
    check(after.at(-1) === later.info, 'the later provider was not added last');
    check(win.document.acp === page.acp, 'a later provider replaced the registry');
    await page.acp.agents();
    check((await page.acp.request({ provider: later.info.uuid })).length === 0, 'request did not reach the provider it names');
    await rejectsWith(page.acp.request({ provider: 'conformance-nobody' }), 'NotFoundError');
    await rejectsWith(page.acp.connect({ id: 'x', provider: 'conformance-nobody' }), 'NotFoundError');
  },
  async lifetime(driver) {
    const kept = async (origin, label, lifetime) => {
      driver.discover(label);
      const page = driver.page(origin);
      page.activate();
      driver.person.answerNext({ allow: label, cwd: '/granted', ...(lifetime === undefined ? {} : { lifetime }) });
      const [agent] = await page.acp.request();
      check(agent.provider === page.acp.providers[0].uuid, "an agent does not carry its provider's uuid");
      return agent.scope.lifetime;
    };
    check((await kept('https://a.example', 'one', 'always')) === 'always', 'the lifetime the person chose, always, was not kept');
    check((await kept('https://b.example', 'two', 'once')) === 'once', 'the lifetime the person chose, once, was not kept');
    check((await kept('https://c.example', 'three')) === 'once', 'a person who chose no lifetime did not get once');
  },
  async 'stream-end'(driver) {
    const { page, connection, reader } = await grantAndConnect(driver, 'https://a.example');
    page.navigate();
    // The stream ends cleanly, not with an error, and `closed` resolves with the reason.
    check((await reader.read()).done === true, 'readable did not end');
    check((await connection.closed).reason === 'navigation', 'closed does not name why');
    check((await reader.read()).done === true, 'readable did not stay ended');
  },
  async 'write-after-close'(driver) {
    for (const [origin, end] of [
      ['https://a.example', (grant) => grant.connection.close()],
      ['https://b.example', (grant) => grant.page.navigate()],
      ['https://c.example', (grant) => grant.page.acp.revoke(grant.agent)],
    ]) {
      const grant = await grantAndConnect(driver, origin);
      await end(grant);
      await grant.connection.closed;
      const received = grant.seen.received().length;
      await rejectsWith(grant.send(RPC(1, 'initialize', { protocolVersion: 1 })), 'InvalidStateError');
      check(grant.seen.received().length === received, 'a write after the end reached the agent');
    }
  },
  async 'stream-close'(driver) {
    const ways = {
      'close()': (grant) => grant.connection.close(),
      'writable.close()': (grant) => grant.writer.close(),
      'writable.abort()': (grant) => grant.writer.abort(new Error('the page gave up')),
      'readable.cancel()': (grant) => grant.reader.cancel(),
    };
    let origin = 0;
    for (const [way, end] of Object.entries(ways)) {
      const grant = await grantAndConnect(driver, `https://o${(origin += 1)}.example`);
      await end(grant);
      check((await grant.connection.closed).reason === 'page', `${way} did not end the connection as page`);
      grant.connection.close();
      check((await grant.connection.closed).reason === 'page', `${way}: ending it again changed the reason`);
    }
  },
  async 'cancel-drops-messages'(driver) {
    const { connection, reader, seen, openSession } = await grantAndConnect(driver, 'https://a.example');
    const opened = await openSession(1);
    await reader.cancel();
    check((await connection.closed).reason === 'page', 'cancel did not end the connection');
    // What the agent says later is dropped, and nothing is thrown at the page.
    const failures = [];
    const catcher = (event) => failures.push(event);
    process.on('uncaughtException', catcher);
    process.on('unhandledRejection', catcher);
    try {
      seen.send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: opened.result.sessionId, update: { sessionUpdate: 'agent_message_chunk' } } });
      await new Promise((resolve) => setTimeout(resolve, 30));
    } finally {
      process.off('uncaughtException', catcher);
      process.off('unhandledRejection', catcher);
    }
    check(failures.length === 0, `a message after cancel threw: ${failures[0]}`);
  },
  async 'write-non-string'(driver) {
    const { connection, writer, seen, settle } = await grantAndConnect(driver, 'https://a.example');
    await settle(1);
    const received = seen.received().length;
    await rejectsWith(writer.write({ jsonrpc: '2.0' }), 'TypeError');
    await writer.write(RPC(2, 'initialize', { protocolVersion: 1 })).then(
      () => check(false, 'a write after a non-string succeeded'),
      () => {},
    );
    check(seen.received().length === received, 'a write after a non-string reached the agent');
    // The connection stays open until close() or its readable's end.
    const state = await Promise.race([connection.closed.then(() => 'closed'), new Promise((resolve) => setTimeout(() => resolve('open'), 30))]);
    check(state === 'open', 'a non-string ended the connection');
    connection.close();
    check((await connection.closed).reason === 'page', 'close() after a non-string did not end it as page');
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
    // A category is what the agent says of an option, so a page claiming one means nothing.
    seen.send({
      jsonrpc: '2.0',
      method: 'session/update',
      params: {
        sessionId: session,
        update: {
          sessionUpdate: 'config_option_update',
          configOptions: [{ id: 'm', category: 'mode' }, { id: 'n', category: 'something-new' }, { id: 'e', category: 'thought_level' }],
        },
      },
    });
    await next();
    await send(RPC(3, 'session/set_config_option', { sessionId: session, category: 'model', configId: 'm', value: 'x' }));
    check((await next()).error !== undefined, 'a mode option claimed to be a model was not refused');
    await send(RPC(4, 'session/set_config_option', { sessionId: session, configId: 'n', value: 'x' }));
    check((await next()).error !== undefined, 'an unknown category was not refused');
    await send(RPC(7, 'session/set_config_option', { sessionId: session, category: 'model', configId: 'never-described', value: 'x' }));
    check((await next()).error !== undefined, 'an option the agent never described was not refused');
    await send(RPC(5, 'session/set_model', { sessionId: session, modelId: 'm' }));
    check((await next()).error === undefined, 'choosing a model was refused');
    await send(RPC(6, 'session/set_config_option', { sessionId: session, configId: 'e', value: 'high' }));
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
    await writer.write(RPC(10, 'initialize', { protocolVersion: 1 }));
    await reader.read();
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

export const DEFAULT_TIMEOUT_MS = 10_000;

/**
 * Runs every check, each against a fresh provider from `makeDriver` and within a time bound (`driver.timeoutMs`, else
 * `options.timeoutMs`, else 10 seconds): a provider that never answers fails that check by name. Returns [{ id, passed, why }].
 */
export async function runConformance(makeDriver, options = {}) {
  const results = [];
  for (const [id, run] of Object.entries(CHECKS)) {
    let timer;
    let expire;
    const late = new Promise((_, reject) => {
      expire = (ms) => reject(new Error(`${id} did not finish in ${ms} ms`));
    });
    const arm = (ms) => {
      clearTimeout(timer);
      timer = setTimeout(() => expire(ms), ms);
    };
    arm(options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
    try {
      await Promise.race([
        (async () => {
          const driver = await makeDriver();
          if (driver.timeoutMs !== undefined) arm(driver.timeoutMs);
          await run(driver);
        })(),
        late,
      ]);
      results.push({ id, passed: true });
    } catch (error) {
      results.push({ id, passed: false, why: String(error?.message ?? error) });
    } finally {
      clearTimeout(timer);
    }
  }
  return results;
}
