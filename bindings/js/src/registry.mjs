// `document.acp`: one registry however many providers there are, and in whatever order they arrive.

// Kept on the window as well: a `document.acp` shadowed by the page's markup must not start a second registry.
const REGISTRY = Symbol.for('coqui-acp.registry');

function route(providers, agent) {
  const found = providers.find((provider) => provider.info.uuid === agent?.provider);
  if (found === undefined) throw new DOMException('no such agent', 'NotFoundError');
  return found;
}

function makeRegistry(win) {
  const providers = [];
  const events = new EventTarget();
  const announce = () => win.dispatchEvent(new CustomEvent('acp:ready', { detail: { acp: registry } }));
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
        options.provider === undefined
          ? providers[0]
          : providers.find((provider) => provider.info.uuid === options.provider);
      if (chosen === undefined) throw new DOMException('no such provider', 'NotFoundError');
      return chosen.request(options);
    },
    async connect(agent, options = {}) {
      return route(providers, agent).connect(agent, options);
    },
    async revoke(agent) {
      return route(providers, agent).revoke(agent);
    },
    addEventListener: (...args) => events.addEventListener(...args),
    removeEventListener: (...args) => events.removeEventListener(...args),
    register(provider) {
      providers.push(provider);
      provider.addEventListener?.('agentschange', () => events.dispatchEvent(new Event('agentschange')));
      announce();
    },
  });
  win.addEventListener('acp:request', announce);
  return registry;
}

/** For a provider: defines the registry if nobody has, and adds this provider to it. An earlier provider stays. */
export function registerProvider(provider, win = globalThis.window) {
  let registry = win[REGISTRY];
  if (registry === undefined) {
    registry = makeRegistry(win);
    Object.defineProperty(win, REGISTRY, { value: registry });
    Object.defineProperty(win.document, 'acp', { value: registry, enumerable: true });
  }
  registry.register(provider);
  return registry;
}
