// The page in test/page, through this extension, in real Edge: headless, driven over the DevTools protocol, with the
// provider's window clicked by script. Branded Chrome ignores --load-extension. Every wait is bounded and named, so a
// failure says what stalled.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { startAgents } from './agents.mjs';

const EDGE = process.env.ACP_TEST_EDGE ?? 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe';
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = dirname(HERE);
const EXTENSION = join(ROOT, 'extension');
const PROFILE = join(ROOT, `.profile-${process.pid}`);
const STRICT = "script-src 'self'; connect-src 'self'";
// Longer than the 30 s after which an idle service worker is stopped.
const IDLE_MS = Number(process.env.ACP_TEST_IDLE_MS ?? 35_000);

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function bounded(what, promise, ms = 10_000) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`stalled after ${ms} ms: ${what}`)), ms);
    }),
  ]).finally(() => clearTimeout(timer));
}

async function until(what, probe, ms = 10_000) {
  const deadline = Date.now() + ms;
  let last;
  for (;;) {
    last = await probe();
    if (last) return last;
    if (Date.now() > deadline) throw new Error(`stalled after ${ms} ms: ${what}`);
    await sleep(50);
  }
}

class Cdp {
  #socket;
  #next = 1;
  #pending = new Map();

  static async connect(url) {
    const cdp = new Cdp();
    cdp.#socket = new WebSocket(url);
    await bounded('the DevTools socket opening', new Promise((resolve, reject) => {
      cdp.#socket.onopen = resolve;
      cdp.#socket.onerror = () => reject(new Error('the DevTools socket failed'));
    }));
    cdp.#socket.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      const pending = cdp.#pending.get(message.id);
      if (pending === undefined) return;
      cdp.#pending.delete(message.id);
      if (message.error) pending.reject(new Error(`${pending.method}: ${message.error.message}`));
      else pending.resolve(message.result);
    };
    return cdp;
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.#next;
    this.#next += 1;
    const sent = new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject, method });
      this.#socket.send(JSON.stringify({ id, method, params, sessionId }));
    });
    return bounded(`DevTools ${method}`, sent);
  }

  close() {
    this.#socket.close();
  }

  targets = async () => (await this.send('Target.getTargets')).targetInfos;

  attach = async (targetId) => (await this.send('Target.attachToTarget', { targetId, flatten: true })).sessionId;

  async evaluate(sessionId, expression) {
    const { result, exceptionDetails } = await this.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (exceptionDetails) throw new Error(`in the page: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
    return result.value;
  }
}

let edge;
let cdp;
let agents;
let site;
let origin;
let otherOrigin;

function stopEdge() {
  if (edge?.pid !== undefined) spawnSync('taskkill', ['/PID', String(edge.pid), '/T', '/F'], { stdio: 'ignore' });
}

async function removeProfile() {
  // A dying browser holds its files for a moment.
  for (let attempt = 0; attempt < 40 && existsSync(PROFILE); attempt += 1) {
    try {
      rmSync(PROFILE, { recursive: true, force: true });
    } catch {
      await sleep(250);
    }
  }
}

function serve(request, response) {
  const name = new URL(request.url, 'http://x').pathname;
  const files = { '/': ['index.html', 'text/html'], '/page.js': ['page.js', 'text/javascript'] };
  if (!Object.hasOwn(files, name)) return response.writeHead(404).end();
  const [file, type] = files[name];
  // Not `no-store`, which keeps a page out of the back/forward cache.
  response.writeHead(200, { 'content-type': type, 'content-security-policy': STRICT, 'cache-control': 'no-cache' });
  return response.end(readFileSync(join(HERE, 'page', file)));
}

before(async () => {
  agents = await startAgents();
  site = createServer(serve);
  await new Promise((resolve) => site.listen(0, '127.0.0.1', resolve));
  const { port } = site.address();
  origin = `http://127.0.0.1:${port}`;
  otherOrigin = `http://localhost:${port}`;
  mkdirSync(PROFILE, { recursive: true });
  edge = spawn(
    EDGE,
    [
      '--headless=new',
      '--remote-debugging-port=0',
      `--user-data-dir=${PROFILE}`,
      `--disable-extensions-except=${EXTENSION}`,
      `--load-extension=${EXTENSION}`,
      '--no-first-run',
      '--no-default-browser-check',
      '--disable-sync',
      '--disable-background-networking',
      'about:blank',
    ],
    { stdio: 'ignore' },
  );
  edge.on('error', (error) => {
    throw error;
  });
  const active = join(PROFILE, 'DevToolsActivePort');
  const address = await until('Edge writing DevToolsActivePort', () => {
    if (!existsSync(active)) return null;
    const lines = readFileSync(active, 'utf8').split('\n');
    return lines.length >= 2 && lines[1] !== '' ? lines : null;
  }, 30_000);
  cdp = await Cdp.connect(`ws://127.0.0.1:${address[0].trim()}${address[1].trim()}`);
});

after(async () => {
  // The answer may never come from a browser that is closing, and the process is killed next either way.
  await Promise.race([cdp?.send('Browser.close').catch(() => {}), sleep(2000)]);
  cdp?.close();
  stopEdge();
  await removeProfile();
  await agents?.close();
  site?.closeAllConnections();
  await new Promise((resolve) => (site ? site.close(resolve) : resolve()));
});

async function openTab(url) {
  const { targetId } = await cdp.send('Target.createTarget', { url });
  const session = await cdp.attach(targetId);
  await cdp.send('Runtime.enable', {}, session);
  await cdp.send('Page.enable', {}, session);
  // A new target starts on about:blank, which is complete at once, so the address has to match as well.
  await until(`${url} finishing loading`, () => cdp.evaluate(session, `location.href === ${JSON.stringify(url)} && document.readyState === 'complete'`));
  return { targetId, session };
}

// A real mouse event, so the page gets real user activation.
async function click({ targetId, session }, id) {
  await cdp.send('Target.activateTarget', { targetId });
  await cdp.send('Page.bringToFront', {}, session);
  const box = await cdp.evaluate(
    session,
    `(() => { const r = document.getElementById(${JSON.stringify(id)}).getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`,
  );
  const mouse = { x: box.x, y: box.y, button: 'left', clickCount: 1 };
  await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', ...mouse }, session);
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...mouse }, session);
}

const logOf = async ({ session }) => (await cdp.evaluate(session, "document.getElementById('log').textContent")).split('\n').filter(Boolean).map((line) => JSON.parse(line));
const saidOf = async ({ session }) => cdp.evaluate(session, "document.getElementById('said').textContent");

async function logEntry(tab, what, predicate, ms) {
  return until(`the page logging ${what}`, async () => {
    const entries = await logOf(tab);
    const failed = entries.find((entry) => entry.error !== undefined);
    if (failed) throw new Error(`the page reported ${JSON.stringify(failed)}`);
    return entries.find(predicate);
  }, ms);
}

async function providerWindow(kind) {
  const target = await until(`the provider's ${kind} window opening`, async () => (await cdp.targets()).find((info) => info.type === 'page' && info.url.includes('/ask.html')));
  extensionId = new URL(target.url).host;
  const session = await cdp.attach(target.targetId);
  await cdp.send('Runtime.enable', {}, session);
  await until(`the provider's ${kind} window drawing`, async () => (await cdp.evaluate(session, `document.body?.dataset.ready === ${JSON.stringify(kind)}`)));
  return { target, session };
}

const windowGone = () => until("the provider's window closing", async () => !(await cdp.targets()).some((info) => info.url.includes('/ask.html')));

let extensionId;
let tab;

test('strict-csp: document.acp is defined under the strict policy', async () => {
  tab = await openTab(`${origin}/`);
  const facts = await until(
    'document.acp being defined (did Edge take --load-extension?)',
    () => cdp.evaluate(tab.session, "document.acp ? { version: document.acp.version, providers: document.acp.providers.length, provider: document.getElementById('provider').textContent } : null"),
    30_000,
  );
  assert.equal(facts.version, 1);
  assert.equal(facts.providers, 1);
  assert.match(facts.provider, /^Provider: /, 'the page did not find the provider by event');
});

test('a page sees nothing before a grant, and needs a click to ask', async () => {
  assert.equal(await cdp.evaluate(tab.session, 'pageTools.agentCount()'), 0);
  assert.equal(await cdp.evaluate(tab.session, 'pageTools.requestWithNoClick()'), 'NotAllowedError');
  assert.equal((await cdp.targets()).filter((info) => info.url.includes('/ask.html')).length, 0, 'a window opened with no click');
});

test('a click asks the person in the provider\'s own window, who adds an agent by hand', async () => {
  await click(tab, 'ask');
  const { session } = await providerWindow('grant');
  await cdp.evaluate(
    session,
    `(() => {
      document.getElementById('add-url').value = ${JSON.stringify(agents.urlFor('mine'))};
      document.getElementById('add-label').value = 'My agent';
      document.getElementById('add-understood').checked = true;
      document.getElementById('add').click();
      document.getElementById('cwd').value = '/chosen/by/the/person';
      document.getElementById('allow').click();
    })()`,
  ).catch(() => {});
  await windowGone();
  const { agents: listed } = await logEntry(tab, 'the granted agent', (entry) => entry.agents?.length === 1);
  assert.deepEqual(listed, [{ label: 'My agent', verified: false, status: 'unknown' }]);
});

test('a turn runs, with the permission request going to the person and not the page', async () => {
  await click(tab, 'turn');
  const { session } = await providerWindow('permission');
  await cdp.evaluate(session, "document.querySelector('[data-option=yes]').click()").catch(() => {});
  await windowGone();
  await logEntry(tab, 'the end of the turn', (entry) => entry.stopReason === 'end_turn');
  assert.equal(await saidOf(tab), 'thinking\ntold:yes\n');
  const entries = await logOf(tab);
  assert.ok(!entries.some((entry) => entry.fromAgent === 'session/request_permission'), 'the page saw the permission request');
  const received = agents.agent('mine').received();
  assert.equal(received.find((message) => message.method === 'session/new').params.cwd, '/chosen/by/the/person');
  assert.ok(received.some((message) => message.result?.outcome?.optionId === 'yes'), 'the agent did not get the person\'s answer');
});

test('a connection left quiet for longer than a service worker\'s idle timeout still works', async () => {
  await sleep(IDLE_MS);
  assert.equal(agents.agent('mine').counts().open, 1, 'the socket closed while idle');
  await click(tab, 'again');
  const { session } = await providerWindow('permission');
  await cdp.evaluate(session, "document.querySelector('[data-option=no]').click()").catch(() => {});
  await windowGone();
  await until('the second turn finishing', async () => (await saidOf(tab)).includes('told:no'));
}, { timeout: IDLE_MS + 30_000 });

test('navigation closes the connection at the agent, and a reload finds its agent with no prompt and resumes', async () => {
  await cdp.send('Page.reload', {}, tab.session);
  await until('the agent seeing the socket close on reload', () => agents.agent('mine').counts().open === 0);
  await until('the reloaded page', async () => (await cdp.evaluate(tab.session, "document.readyState === 'complete' && document.getElementById('log').textContent === ''")));
  await until('the reloaded page finding the provider', () => cdp.evaluate(tab.session, "!!document.acp && document.getElementById('provider').textContent.startsWith('Provider')"));
  assert.equal(await cdp.evaluate(tab.session, 'pageTools.agentCount()'), 1);
  assert.equal((await cdp.targets()).filter((info) => info.url.includes('/ask.html')).length, 0, 'a reload prompted');
  await click(tab, 'resume');
  const { session } = await providerWindow('permission');
  await cdp.evaluate(session, "document.querySelector('[data-option=yes]').click()").catch(() => {});
  await windowGone();
  await logEntry(tab, 'the resumed turn ending', (entry) => entry.stopReason === 'end_turn');
  const entries = await logOf(tab);
  assert.ok(entries.some((entry) => entry.resumed === true));
  assert.equal(agents.agent('mine').received().filter((message) => message.method === 'session/resume').at(-1).params.cwd, '/chosen/by/the/person');
});

test('a page restored from the back/forward cache was told its connection closed, and carries on', async () => {
  await cdp.evaluate(tab.session, "window.keptAcrossNavigation = 'yes'");
  await cdp.send('Page.navigate', { url: `${origin}/?elsewhere` }, tab.session);
  await until('the agent seeing the socket close on leaving', () => agents.agent('mine').counts().open === 0);
  await until('the next page', () => cdp.evaluate(tab.session, "location.search === '?elsewhere' && document.readyState === 'complete'"));
  const { entries, currentIndex } = await cdp.send('Page.getNavigationHistory', {}, tab.session);
  await cdp.send('Page.navigateToHistoryEntry', { entryId: entries[currentIndex - 1].id }, tab.session);
  await until('the page coming back', () => cdp.evaluate(tab.session, "location.search === '' && document.readyState === 'complete'"));
  assert.equal(await cdp.evaluate(tab.session, 'window.keptAcrossNavigation'), 'yes', 'Edge did not restore the page from the cache, so this proved nothing');
  await logEntry(tab, 'the connection closing as navigation', (entry) => entry.closed === 'navigation');
  assert.equal(await cdp.evaluate(tab.session, 'pageTools.agentCount()'), 1, 'the restored page lost its way to the provider');
  await click(tab, 'resume');
  const { session } = await providerWindow('permission');
  await cdp.evaluate(session, "document.querySelector('[data-option=yes]').click()").catch(() => {});
  await windowGone();
  await until('the agent seeing a new connection', () => agents.agent('mine').counts().open === 1);
});

test('another origin sees nothing and cannot use the first origin\'s agent', async () => {
  const held = await cdp.evaluate(tab.session, 'pageTools.firstAgent()');
  const other = await openTab(`${otherOrigin}/`);
  await until('document.acp on the other origin', () => cdp.evaluate(other.session, '!!document.acp'));
  assert.equal(await cdp.evaluate(other.session, 'pageTools.agentCount()'), 0);
  assert.equal(await cdp.evaluate(other.session, `pageTools.connectTo(${JSON.stringify(held)})`), 'NotAllowedError');
  await cdp.send('Target.closeTarget', { targetId: other.targetId });
});

test('the person revokes in the provider\'s own page, and the live connection closes as revoked', async () => {
  const manage = await openTab(`chrome-extension://${extensionId}/manage.html`);
  await until('the manage page drawing', () => cdp.evaluate(manage.session, "document.body.dataset.ready === 'yes'"));
  await cdp.evaluate(manage.session, "document.querySelector('[data-revoke]').click()");
  await logEntry(tab, 'the connection closing as revoked', (entry) => entry.closed === 'revoked');
  await until('the agent seeing the socket close on revocation', () => agents.agent('mine').counts().open === 0);
  assert.equal(await cdp.evaluate(tab.session, 'pageTools.agentCount()'), 0);
  await cdp.send('Target.closeTarget', { targetId: manage.targetId });
});
