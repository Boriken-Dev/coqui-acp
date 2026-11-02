// The page's own world: the registry and this provider's half of the page API. A classic script, because a declared
// content script cannot be a module, so the registry algorithm is restated here rather than imported. Everything here is
// the page's to tamper with; nothing it could forge is trusted by the other side.
(() => {
  if (!window.isSecureContext || window.top !== window) return;

  const INFO = Object.freeze({
    // The same for every install of this build, so it says no more about a person than `rdns` does.
    uuid: '3b7c1f4e-52a6-4d0b-9e31-8a6f0c2d7b15',
    name: 'ACP second provider',
    rdns: 'dev.boriken.coqui_acp.second_provider',
  });

  const REGISTRY = Symbol.for('coqui-acp.registry');

  function makeRegistry() {
    const providers = [];
    const events = new EventTarget();
    let handler = null;
    events.addEventListener('agentschange', (event) => {
      if (typeof handler === 'function') handler.call(registry, event);
    });
    const announce = () => window.dispatchEvent(new CustomEvent('acp:ready', { detail: { acp: registry } }));
    const route = (agent) => {
      const found = providers.find((provider) => provider.info.uuid === agent?.provider);
      if (found === undefined) throw new DOMException('no such agent', 'NotFoundError');
      return found;
    };
    const registry = Object.freeze({
      version: 1,
      get providers() {
        return Object.freeze(providers.map((provider) => provider.info));
      },
      async agents() {
        return (await Promise.all(providers.map((provider) => provider.agents()))).flat();
      },
      async request(options = {}) {
        const chosen =
          options.provider === undefined ? providers[0] : providers.find((provider) => provider.info.uuid === options.provider);
        if (chosen === undefined) throw new DOMException('no such provider', 'NotFoundError');
        return chosen.request(options);
      },
      async connect(agent, options = {}) {
        return route(agent).connect(agent, options);
      },
      async revoke(agent) {
        return route(agent).revoke(agent);
      },
      addEventListener: (...args) => events.addEventListener(...args),
      removeEventListener: (...args) => events.removeEventListener(...args),
      // An accessor, because the object is frozen and the page API declares this attribute as assignable.
      get onagentschange() {
        return handler;
      },
      set onagentschange(value) {
        handler = typeof value === 'function' ? value : null;
      },
      register(provider) {
        providers.push(provider);
        provider.addEventListener?.('agentschange', () => events.dispatchEvent(new Event('agentschange')));
        announce();
      },
    });
    window.addEventListener('acp:request', announce);
    return registry;
  }

  function registerProvider(provider) {
    let registry = window[REGISTRY];
    if (registry === undefined) {
      registry = makeRegistry();
      Object.defineProperty(window, REGISTRY, { value: registry });
      Object.defineProperty(document, 'acp', { value: registry, enumerable: true });
    }
    registry.register(provider);
  }

  const channel = new MessageChannel();
  const port = channel.port1;
  const post = (message) => port.postMessage(message);
  // Posted before any of the page's scripts run, so the content script's first hello is this one. Reading it later
  // gains the page nothing: the channel can only ask for what `document.acp` already offers it.
  window.postMessage({ acpProviderChannel: INFO.uuid }, window.location.origin, [channel.port2]);

  const events = new EventTarget();
  const calls = new Map();
  const connections = new Map();
  let nextCall = 1;

  const refusal = (name) => new DOMException('the provider refused', name ?? 'NotAllowedError');

  function call(op, args, settle = (value) => value) {
    return new Promise((resolve, reject) => {
      const n = nextCall;
      nextCall += 1;
      calls.set(n, { resolve: (value) => resolve(settle(value)), reject });
      post({ t: 'call', n, op, args });
    });
  }

  function makeConnection({ c, agent }) {
    let finished = false;
    let reading;
    let writing;
    let finishClosed;
    const closed = new Promise((resolve) => {
      finishClosed = resolve;
    });
    const finish = (reason) => {
      if (finished) return;
      finished = true;
      connections.delete(c);
      finishClosed({ reason });
      reading.close();
      writing.error(new DOMException('the connection is closed', 'InvalidStateError'));
    };
    const closeFromPage = () => {
      if (finished) return;
      post({ t: 'close', c });
      finish('page');
    };
    const readable = new ReadableStream(
      {
        start(controller) {
          reading = controller;
        },
        cancel: closeFromPage,
      },
      { highWaterMark: Number.MAX_SAFE_INTEGER },
    );
    const writable = new WritableStream({
      start(controller) {
        writing = controller;
      },
      write(text) {
        if (typeof text !== 'string') throw new TypeError('a connection carries strings');
        post({ t: 'send', c, text });
      },
      close: closeFromPage,
      abort: closeFromPage,
    });
    connections.set(c, { finish, deliver: (text) => reading.enqueue(text) });
    return Object.freeze({ agent, readable, writable, closed, close: closeFromPage });
  }

  port.onmessage = ({ data }) => {
    if (data.t === 'reply') {
      const pending = calls.get(data.n);
      calls.delete(data.n);
      if (data.error !== undefined) pending?.reject(refusal(data.error));
      else pending?.resolve(data.value);
    } else if (data.t === 'msg') {
      connections.get(data.c)?.deliver(data.text);
    } else if (data.t === 'closed') {
      connections.get(data.c)?.finish(data.reason);
    } else if (data.t === 'agentschange') {
      events.dispatchEvent(new Event('agentschange'));
    } else if (data.t === 'lost') {
      for (const pending of calls.values()) pending.reject(refusal('NetworkError'));
      calls.clear();
      for (const connection of [...connections.values()]) connection.finish('failed');
    }
  };

  // A page kept for the back button stays alive with no provider behind it, so it has to learn here.
  window.addEventListener('pagehide', () => {
    for (const pending of calls.values()) pending.reject(refusal('NetworkError'));
    calls.clear();
    for (const connection of [...connections.values()]) connection.finish('navigation');
  });

  const strings = (value) => (value === undefined ? undefined : Array.from(value, String));

  registerProvider({
    info: INFO,
    addEventListener: (...args) => events.addEventListener(...args),
    agents: () => call('agents'),
    request(options = {}) {
      return call('request', {
        purpose: options.purpose === undefined ? undefined : String(options.purpose),
        acpVersions: options.acpVersions === undefined ? undefined : Array.from(options.acpVersions, Number),
        mcpOrigins: strings(options.mcpOrigins),
        peers: strings(options.peers),
        permissions: options.permissions === true,
      });
    },
    connect(agent, options = {}) {
      const { signal } = options ?? {};
      if (signal?.aborted) return Promise.reject(signal.reason);
      const n = nextCall;
      const opened = call('connect', { id: String(agent?.id) }, makeConnection);
      if (signal === undefined) return opened;
      const abort = () => post({ t: 'abort', n });
      signal.addEventListener('abort', abort, { once: true });
      // The signal ends the wait for a connection and not the connection, so a timeout signal cannot cut a live one.
      const stop = () => signal.removeEventListener('abort', abort);
      opened.then(stop, stop);
      return opened;
    },
    revoke: (agent) => call('revoke', { id: String(agent?.id) }),
  });
})();
