// Written only against docs/browser.md "The page API", so it works with whichever provider the person has.
const byId = (id) => document.getElementById(id);
const log = (entry) => {
  byId('log').textContent += `${JSON.stringify(entry)}\n`;
};

let acp = null;
let conversation = null;
window.addEventListener('acp:ready', (event) => {
  acp = event.detail.acp;
  byId('provider').textContent = `Provider: ${acp.providers.map((provider) => provider.name).join(', ')}`;
});
window.dispatchEvent(new Event('acp:request'));

function show(agents) {
  byId('agents').replaceChildren(
    ...agents.map((agent) => Object.assign(document.createElement('li'), { textContent: `${agent.label} (${agent.source}, verified: ${agent.identity.verified})` })),
  );
  log({ agents: agents.map((agent) => ({ label: agent.label, verified: agent.identity.verified, status: agent.status })) });
}

async function open(agent) {
  const connection = await acp.connect(agent);
  const writer = connection.writable.getWriter();
  const reader = connection.readable.getReader();
  const waiting = new Map();
  let nextId = 1;
  connection.closed.then(({ reason }) => log({ closed: reason }));
  (async () => {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return;
      const message = JSON.parse(value);
      if (message.method !== undefined) log({ fromAgent: message.method });
      if (message.method === 'session/update') byId('said').textContent += `${message.params.update.content?.text}\n`;
      if (message.id !== undefined && message.method === undefined) {
        waiting.get(message.id)?.(message);
        waiting.delete(message.id);
      }
    }
  })();
  const call = (method, params) =>
    new Promise((resolve, reject) => {
      const id = nextId;
      nextId += 1;
      waiting.set(id, (message) => (message.error === undefined ? resolve(message.result) : reject(new Error(`${method}: ${message.error.code}`))));
      writer.write(JSON.stringify({ jsonrpc: '2.0', id, method, params })).catch(reject);
    });
  await call('initialize', { protocolVersion: 1, clientCapabilities: {} });
  return { connection, call };
}

async function prompt(words) {
  const { stopReason } = await conversation.call('session/prompt', {
    sessionId: conversation.sessionId,
    prompt: [{ type: 'text', text: words }],
  });
  log({ stopReason });
}

function action(id, run) {
  byId(id).addEventListener('click', () => {
    run().catch((error) => log({ action: id, error: error.name, message: error.message }));
  });
}

action('ask', async () => {
  show(await acp.request({ purpose: 'To help with this page.' }));
});

action('list', async () => {
  show(await acp.agents());
});

action('turn', async () => {
  const [agent] = await acp.agents();
  const opened = await open(agent);
  const { sessionId } = await opened.call('session/new', { cwd: '/a/folder/the/page/would/like', mcpServers: [] });
  conversation = { ...opened, sessionId };
  // A page keeps these itself to carry on after a reload.
  localStorage.setItem('conversation', JSON.stringify({ agentId: agent.id, sessionId }));
  await prompt('hello');
});

action('again', () => prompt('more'));

action('resume', async () => {
  const { agentId, sessionId } = JSON.parse(localStorage.getItem('conversation'));
  const agent = (await acp.agents()).find((candidate) => candidate.id === agentId);
  const opened = await open(agent);
  await opened.call('session/resume', { sessionId, cwd: '/a/folder/the/page/would/like', mcpServers: [] });
  conversation = { ...opened, sessionId };
  log({ resumed: true });
  await prompt('back again');
});

action('leave', async () => {
  const [agent] = await acp.agents();
  await acp.revoke(agent);
  log({ revoked: true });
});

// For a test driving this page from outside: the same calls with no click behind them.
window.pageTools = {
  requestWithNoClick: () => acp.request().then(() => 'granted', (error) => error.name),
  connectTo: (agent) => acp.connect(agent).then(() => 'connected', (error) => error.name),
  agentCount: () => acp.agents().then((agents) => agents.length),
  firstAgent: () => acp.agents().then(([agent]) => agent),
};
