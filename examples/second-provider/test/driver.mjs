// The driver bindings/js/src/conformance.mjs asks for. The extension's own page-world.js, content.js, provider.js and
// scope.js run here, and real WebSockets reach real sockets. Only the browser around them is played: a window, ports that
// serialize as the browser's do, and the person.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { compileFunction } from 'node:vm';
import { Provider } from '../extension/provider.js';
import { startAgents } from './agents.mjs';

const file = (name) => fileURLToPath(new URL(`../extension/${name}`, import.meta.url));
const compile = (name) => compileFunction(readFileSync(file(name), 'utf8'), ['window', 'document', 'navigator', 'chrome'], { filename: file(name) });
const PAGE_WORLD = compile('page-world.js');
const CONTENT = compile('content.js');

class FakeWindow extends EventTarget {
  constructor(origin, { top = true, secure = true } = {}) {
    super();
    this.document = {};
    this.location = { origin };
    this.isSecureContext = secure;
    this.top = top ? this : {};
  }

  postMessage(data, _origin, ports = []) {
    const event = new Event('message');
    Object.assign(event, { data, source: this, ports });
    this.dispatchEvent(event);
  }
}

function portPair(sender) {
  const make = () => ({ message: [], disconnect: [], gone: false });
  const near = make();
  const far = make();
  const portOver = (mine, theirs) => ({
    sender,
    postMessage(message) {
      if (mine.gone) throw new Error('Attempting to use a disconnected port object');
      const copy = JSON.parse(JSON.stringify(message));
      queueMicrotask(() => {
        if (!theirs.gone) for (const listener of theirs.message) listener(copy);
      });
    },
    onMessage: { addListener: (listener) => mine.message.push(listener) },
    onDisconnect: { addListener: (listener) => mine.disconnect.push(listener) },
    disconnect() {
      if (mine.gone) return;
      mine.gone = true;
      theirs.gone = true;
      // The browser tells only the other side.
      queueMicrotask(() => {
        for (const listener of theirs.disconnect) listener();
      });
    },
  });
  return [portOver(near, far), portOver(far, near)];
}

function memoryStore() {
  let held = {};
  return {
    get: async (keys) => Object.fromEntries(keys.filter((key) => key in held).map((key) => [key, structuredClone(held[key])])),
    set: async (items) => {
      held = { ...held, ...structuredClone(items) };
    },
  };
}

export async function makeDriver({ permissionTimeoutMs, stores } = {}) {
  const agents = await startAgents();
  const answers = [];
  const waitingForAnswer = [];
  const asked = [];
  const channels = new Set();
  const tabs = new Map();
  let nextTab = 100;
  let nextDocument = 1;

  const fromPerson = (answer, prompt) => {
    if (answer.refuse === true) return null;
    const allowed = [answer.allow ?? []].flat();
    const added = new Set(answer.add ?? []);
    return {
      allow: allowed
        .filter((label) => !added.has(label))
        .map((label) => prompt.candidates.find((candidate) => candidate.label === label)?.key)
        .filter((key) => key !== undefined),
      add: [...added].map((label) => ({ label, url: agents.urlFor(label), understood: true, grant: allowed.includes(label) })),
      cwd: answer.cwd,
      lifetime: answer.lifetime,
      permissions: answer.permissions,
      mcpOrigins: answer.mcpOrigins,
    };
  };

  const ui = {
    askGrant(prompt, signal) {
      return new Promise((resolve) => {
        const respond = (answer) => resolve(fromPerson(answer, prompt));
        signal.addEventListener(
          'abort',
          () => {
            const waiting = waitingForAnswer.indexOf(respond);
            if (waiting !== -1) waitingForAnswer.splice(waiting, 1);
            resolve(null);
          },
          { once: true },
        );
        if (answers.length > 0) respond(answers.shift());
        else waitingForAnswer.push(respond);
      });
    },
    askPermission(question, signal) {
      asked.push(question);
      return new Promise((resolve) => {
        signal.addEventListener('abort', () => resolve(null), { once: true });
        driver.person.onPermission?.(question, resolve);
      });
    },
  };

  const local = stores?.local ?? memoryStore();
  const session = stores?.session ?? memoryStore();
  let provider = new Provider({ local, session, ui, permissionTimeoutMs });

  function page(origin, options = {}) {
    if (!tabs.has(origin) || options.newTab) {
      tabs.set(origin, (nextTab += 1));
    }
    const tabId = tabs.get(origin);
    const win = new FakeWindow(origin, options);
    const navigator = { userActivation: { isActive: false } };
    const chrome = {
      runtime: {
        connect() {
          const sender = { origin, frameId: 0, tab: { id: tabId }, documentId: `document-${(nextDocument += 1)}` };
          const [worker, content] = portPair(sender);
          provider.attachPage(worker);
          return content;
        },
      },
    };
    const realPost = win.postMessage.bind(win);
    win.postMessage = (data, target, ports = []) => {
      for (const port of ports) channels.add(port);
      realPost(data, target, ports);
    };
    const realChannel = globalThis.MessageChannel;
    globalThis.MessageChannel = class extends realChannel {
      constructor() {
        super();
        channels.add(this.port1);
        channels.add(this.port2);
      }
    };
    try {
      CONTENT(win, win.document, navigator, chrome);
      PAGE_WORLD(win, win.document, navigator, undefined);
    } finally {
      globalThis.MessageChannel = realChannel;
    }
    return {
      acp: win.document.acp,
      window: win,
      tabId,
      activate() {
        navigator.userActivation.isActive = true;
      },
      navigate() {
        win.dispatchEvent(new Event('pagehide'));
      },
    };
  }

  const driver = {
    page,
    person: {
      answerNext(answer) {
        const waiting = waitingForAnswer.shift();
        if (waiting !== undefined) waiting(answer);
        else answers.push(answer);
      },
      permissionsAsked: () => asked,
      onPermission: undefined,
    },
    discover(label, { proven } = {}) {
      provider.discovered({ label, url: agents.urlFor(label), peer: proven === false ? `sha256:key-of-${label}` : null, source: 'local' });
    },
    agent: (label) => agents.agent(label),
    get provider() {
      return provider;
    },
    /** Stops the provider as the browser stops a service worker, keeping what it stored. */
    restartProvider() {
      provider.dispose();
      provider = new Provider({ local, session, ui, permissionTimeoutMs });
      return provider;
    },
    async close() {
      provider.dispose();
      for (const port of channels) port.close();
      await agents.close();
    },
  };
  return driver;
}
