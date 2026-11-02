// The provider proper: agents, grants, and connections. Takes the browser's storage, ports and windows as arguments, so
// the same code runs in the service worker and under the tests.
import { Scope, permissionAnswer } from './scope.js';

export const PROVIDER_UUID = '3b7c1f4e-52a6-4d0b-9e31-8a6f0c2d7b15';

const NAMED_ERRORS = new Set(['NotAllowedError', 'NetworkError', 'AbortError', 'NotFoundError']);
const refused = () => new DOMException('not allowed', 'NotAllowedError');

// Format characters can hide part of a name or reorder what is shown around it.
const shown = (text, limit) => String(text ?? '').replace(/\p{Cf}/gu, '').slice(0, limit);
const pairOf = (agentKey, origin) => `${agentKey}\n${origin}`;

function isLoopbackHost(host) {
  return host === 'localhost' || host.endsWith('.localhost') || host === '[::1]' || /^127(\.\d{1,3}){3}$/.test(host);
}

/** The URL to connect to, if it is one this provider may offer with no proof: a WebSocket on this machine. */
export function loopbackUrl(text) {
  let url;
  try {
    url = new URL(text);
  } catch {
    return null;
  }
  if (url.protocol !== 'ws:' && url.protocol !== 'wss:') return null;
  if (url.username !== '' || url.password !== '' || url.hash !== '') return null;
  return isLoopbackHost(url.hostname) ? url.href : null;
}

function httpsOrigin(text) {
  try {
    const url = new URL(text);
    return url.protocol === 'https:' ? url.origin : null;
  } catch {
    return null;
  }
}

export class Provider {
  #local;
  #session;
  #ui;
  #permissionTimeoutMs;
  #connectTimeoutMs;
  #busyChanged;
  #ready;
  #secret;
  #saving = Promise.resolve();
  #agents = [];
  #grants = [];
  #sessions = new Map();
  #discovered = [];
  #pages = new Set();
  #askingTabs = new Set();
  #prompts = 0;
  #nextConnection = 1;
  #busy = false;

  /**
   * @param {object} init
   * @param {{get: Function, set: Function}} init.local storage that outlives the browser
   * @param {{get: Function, set: Function}} init.session storage that ends with the browser
   * @param {{askGrant: Function, askPermission: Function}} init.ui asks the person: each takes what to show and an
   *   AbortSignal, and resolves with the answer, or null when there is none
   * @param {(busy: boolean) => void} [init.busyChanged] whether something here must outlive an idle service worker
   */
  constructor({ local, session, ui, permissionTimeoutMs = 120_000, connectTimeoutMs = 10_000, busyChanged = () => {} }) {
    this.#local = local;
    this.#session = session;
    this.#ui = ui;
    this.#permissionTimeoutMs = permissionTimeoutMs;
    this.#connectTimeoutMs = connectTimeoutMs;
    this.#busyChanged = busyChanged;
    this.#ready = this.#load();
  }

  async #load() {
    const stored = await this.#local.get(['secret', 'agents', 'grants', 'sessions']);
    const ending = await this.#session.get(['grants']);
    const fresh = stored.secret === undefined;
    this.#secret = stored.secret ?? [...crypto.getRandomValues(new Uint8Array(32))].map((b) => b.toString(16).padStart(2, '0')).join('');
    this.#agents = stored.agents ?? [];
    this.#grants = [...(stored.grants ?? []), ...(ending.grants ?? [])];
    // The browser ending takes a `once` grant with no event, so its sessions are forgotten on the next start.
    for (const [pair, ids] of Object.entries(stored.sessions ?? {})) {
      if (this.#grants.some((grant) => pairOf(grant.agentKey, grant.origin) === pair)) this.#sessions.set(pair, new Set(ids));
    }
    if (fresh) await this.#save();
  }

  #save() {
    const sessions = Object.fromEntries([...this.#sessions].map(([pair, ids]) => [pair, [...ids]]));
    const local = {
      secret: this.#secret,
      agents: this.#agents,
      grants: this.#grants.filter((grant) => grant.lifetime === 'always'),
      sessions,
    };
    const ending = { grants: this.#grants.filter((grant) => grant.lifetime !== 'always') };
    this.#saving = this.#saving.then(() => Promise.all([this.#local.set(local), this.#session.set(ending)]));
    return this.#saving;
  }

  #updateBusy() {
    let busy = this.#prompts > 0;
    for (const page of this.#pages) busy ||= page.connections.size > 0;
    if (busy !== this.#busy) {
      this.#busy = busy;
      this.#busyChanged(busy);
    }
  }

  async #idFor(origin, agentKey) {
    const key = await crypto.subtle.importKey('raw', new TextEncoder().encode(this.#secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
    const mac = await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(pairOf(agentKey, origin)));
    return [...new Uint8Array(mac).slice(0, 16)].map((b) => b.toString(16).padStart(2, '0')).join('');
  }

  #sessionsOf(agentKey, origin) {
    const pair = pairOf(agentKey, origin);
    if (!this.#sessions.has(pair)) this.#sessions.set(pair, new Set());
    return this.#sessions.get(pair);
  }

  #forgetSessionsIfUnused(agentKey, origin) {
    if (!this.#grants.some((grant) => grant.agentKey === agentKey && grant.origin === origin)) {
      this.#sessions.delete(pairOf(agentKey, origin));
    }
  }

  #visibleTo(page) {
    return this.#grants.filter(
      (grant) => grant.origin === page.origin && (grant.lifetime === 'always' || grant.tabId === page.tabId),
    );
  }

  #allConnections() {
    return [...this.#pages].flatMap((page) => [...page.connections.values()]);
  }

  #toAgent(grant) {
    const agent = this.#agents.find((candidate) => candidate.key === grant.agentKey);
    const connected = this.#allConnections().some((connection) => connection.grant.agentKey === grant.agentKey);
    return {
      id: grant.id,
      label: agent.label,
      provider: PROVIDER_UUID,
      source: agent.source,
      // This provider holds no proof code, so nothing it reports is ever verified.
      identity: { peer: agent.peer ?? null, verified: false, method: null },
      offers: { acpVersions: agent.acpVersions ?? [], transports: ['websocket'] },
      // Answering `available` would mean opening a socket to the agent whenever a page lists it, which tells the agent
      // the person visited that page.
      status: connected ? 'available' : 'unknown',
      scope: { permissions: grant.permissions, lifetime: grant.lifetime, mcpOrigins: grant.mcpOrigins },
    };
  }

  #announce(origin) {
    for (const page of this.#pages) {
      if (page.origin === origin) this.#post(page, { t: 'agentschange' });
    }
  }

  #post(page, message) {
    try {
      page.port.postMessage(message);
    } catch {
      // The document has gone, and its disconnect is on the way.
    }
  }

  /** A record found on the way to a prompt: shown to the person, and never to a page until the person chooses it. */
  discovered({ label, url, peer = null, source = 'local', acpVersions = [] }) {
    if (loopbackUrl(url) === null) return;
    this.#discovered.push({ key: crypto.randomUUID(), label: shown(label, 80), url, peer, source, acpVersions });
  }

  #addByHand({ label, url, fingerprint, understood }) {
    const usable = loopbackUrl(url);
    if (usable === null || understood !== true) return null;
    const peer = typeof fingerprint === 'string' && fingerprint.trim() !== '' ? fingerprint.trim() : null;
    const existing = this.#agents.find((agent) => agent.url === usable && agent.peer === peer);
    if (existing !== undefined) return existing;
    const agent = { key: crypto.randomUUID(), label: shown(label, 80) || new URL(usable).host, url: usable, peer, source: 'manual', acpVersions: [] };
    this.#agents.push(agent);
    return agent;
  }

  /** A browser port from the content script of a top-level document, whose sender the browser itself reports. */
  attachPage(port) {
    const { sender } = port;
    if (sender?.frameId !== 0 || typeof sender?.origin !== 'string' || sender.tab?.id === undefined) {
      port.disconnect();
      return;
    }
    const page = { port, origin: sender.origin, tabId: sender.tab.id, connections: new Map(), connecting: new Map(), prompts: new Set() };
    this.#pages.add(page);
    port.onMessage.addListener((message) => this.#fromPage(page, message));
    port.onDisconnect.addListener(() => this.#pageGone(page));
  }

  #pageGone(page) {
    for (const connection of [...page.connections.values()]) this.#close(connection, 'navigation');
    for (const abort of [...page.connecting.values()]) abort();
    for (const prompt of page.prompts) prompt.abort();
    this.#pages.delete(page);
    this.#updateBusy();
  }

  #fromPage(page, message) {
    if (message?.t === 'send') {
      const connection = page.connections.get(message.c);
      if (connection !== undefined) this.#pageSent(connection, message.text);
    } else if (message?.t === 'close') {
      const connection = page.connections.get(message.c);
      if (connection !== undefined) this.#close(connection, 'page');
    } else if (message?.t === 'abort') {
      page.connecting.get(message.n)?.();
    } else if (message?.t === 'call') {
      this.#answerCall(page, message);
    }
  }

  async #answerCall(page, { n, op, args, activated }) {
    try {
      await this.#ready;
      let value;
      if (op === 'agents') value = this.#visibleTo(page).map((grant) => this.#toAgent(grant));
      else if (op === 'request') value = await this.#request(page, args ?? {}, activated === true);
      else if (op === 'connect') value = await this.#connect(page, args ?? {}, n);
      else if (op === 'revoke') value = await this.#pageRevokes(page, args ?? {});
      else throw refused();
      this.#post(page, { t: 'reply', n, value });
    } catch (error) {
      this.#post(page, { t: 'reply', n, error: NAMED_ERRORS.has(error?.name) ? error.name : 'NotAllowedError' });
    }
  }

  #candidates(peers) {
    const all = [...this.#agents, ...this.#discovered];
    const first = (agent) => (peers.includes(agent.peer) ? 0 : 1);
    return all
      .map(({ key, label, url, peer, source }) => ({ key, label, url, peer, source }))
      .sort((a, b) => first(a) - first(b));
  }

  async #request(page, args, activated) {
    // Activation is not consumed by this check, so one click would otherwise allow a stream of windows.
    if (!activated || this.#askingTabs.has(page.tabId)) throw refused();
    const prompt = new AbortController();
    page.prompts.add(prompt);
    this.#askingTabs.add(page.tabId);
    this.#prompts += 1;
    this.#updateBusy();
    try {
      const answer = await this.#ui.askGrant(
        {
          origin: page.origin,
          purpose: args.purpose === undefined ? undefined : shown(args.purpose, 500),
          mcpOrigins: [...new Set((args.mcpOrigins ?? []).map(httpsOrigin).filter((origin) => origin !== null))],
          permissions: args.permissions === true,
          candidates: this.#candidates(args.peers ?? []),
        },
        prompt.signal,
      );
      if (answer === null || answer === undefined) throw refused();
      return await this.#applyAnswer(page, answer);
    } finally {
      page.prompts.delete(prompt);
      this.#askingTabs.delete(page.tabId);
      this.#prompts -= 1;
      this.#updateBusy();
    }
  }

  async #applyAnswer(page, answer) {
    const chosen = new Set(answer.allow ?? []);
    for (const added of answer.add ?? []) {
      const agent = this.#addByHand(added);
      if (agent !== null && added.grant === true) chosen.add(agent.key);
    }
    const known = [];
    for (const key of chosen) {
      let agent = this.#agents.find((candidate) => candidate.key === key);
      if (agent === undefined) {
        agent = this.#discovered.find((candidate) => candidate.key === key);
        if (agent !== undefined) this.#agents.push(agent);
      }
      if (agent !== undefined) known.push(agent);
    }
    if (known.length === 0) {
      await this.#save();
      throw refused();
    }
    const cwd = typeof answer.cwd === 'string' && answer.cwd.trim() !== '' ? answer.cwd.trim() : null;
    const lifetime = answer.lifetime === 'always' ? 'always' : 'once';
    const mcpOrigins = (answer.mcpOrigins ?? []).filter((origin) => httpsOrigin(origin) === origin);
    const granted = [];
    for (const agent of known) {
      const id = await this.#idFor(page.origin, agent.key);
      const grant = {
        uid: crypto.randomUUID(),
        id,
        agentKey: agent.key,
        origin: page.origin,
        lifetime,
        tabId: lifetime === 'always' ? null : page.tabId,
        cwd,
        permissions: answer.permissions === true,
        mcpOrigins,
      };
      const replaced = this.#visibleTo(page).filter((old) => old.agentKey === agent.key);
      this.#grants = this.#grants.filter((old) => !replaced.includes(old));
      this.#grants.push(grant);
      for (const connection of this.#allConnections()) {
        if (replaced.includes(connection.grant)) {
          connection.grant = grant;
          connection.scope.grant = grant;
        }
      }
      granted.push(grant);
    }
    await this.#save();
    this.#announce(page.origin);
    return granted.map((grant) => this.#toAgent(grant));
  }

  #connect(page, { id }, n) {
    const grant = this.#visibleTo(page).find((candidate) => candidate.id === id);
    const agent = grant && this.#agents.find((candidate) => candidate.key === grant.agentKey);
    if (agent === undefined || agent === null) throw refused();
    return new Promise((resolve, reject) => {
      let socket;
      try {
        socket = new WebSocket(agent.url);
      } catch {
        reject(new DOMException('cannot reach the agent', 'NetworkError'));
        return;
      }
      const connection = {
        c: this.#nextConnection,
        socket,
        page,
        grant,
        asks: new Map(),
        scope: new Scope({
          grant,
          origin: page.origin,
          sessions: this.#sessionsOf(grant.agentKey, page.origin),
          sessionsChanged: () => this.#save(),
        }),
      };
      this.#nextConnection += 1;
      let open = false;
      const fail = (name) => {
        clearTimeout(timer);
        page.connecting.delete(n);
        socket.onclose = null;
        socket.close();
        reject(new DOMException('cannot reach the agent', name));
      };
      const timer = setTimeout(() => fail('NetworkError'), this.#connectTimeoutMs);
      timer.unref?.();
      page.connecting.set(n, () => fail('AbortError'));
      socket.onopen = () => {
        open = true;
        clearTimeout(timer);
        page.connecting.delete(n);
        page.connections.set(connection.c, connection);
        this.#updateBusy();
        resolve({ c: connection.c, agent: this.#toAgent(grant) });
      };
      socket.onmessage = (event) => {
        // ACP's WebSocket profile ignores binary frames.
        if (typeof event.data === 'string') this.#agentSent(connection, event.data);
      };
      socket.onclose = (event) => {
        if (open) this.#close(connection, event.wasClean ? 'agent' : 'failed');
        else fail('NetworkError');
      };
    });
  }

  #close(connection, reason) {
    const { page, socket } = connection;
    if (!page.connections.delete(connection.c)) return;
    socket.onclose = null;
    socket.onmessage = null;
    socket.close();
    for (const ask of connection.asks.values()) ask.abort();
    this.#post(page, { t: 'closed', c: connection.c, reason });
    this.#updateBusy();
  }

  #pageSent(connection, text) {
    const out = connection.scope.fromPage(text);
    if (out.reply !== undefined) this.#post(connection.page, { t: 'msg', c: connection.c, text: out.reply });
    if (out.forward !== undefined && connection.socket.readyState === WebSocket.OPEN) connection.socket.send(out.forward);
    // ACP requires a pending permission request to be answered `cancelled` when its turn is cancelled.
    if (out.cancelled !== undefined) {
      for (const [sessionId, ask] of connection.asks) {
        if (sessionId.startsWith(`${out.cancelled}\n`)) ask.abort();
      }
    }
  }

  #agentSent(connection, text) {
    const out = connection.scope.fromAgent(text);
    if (out.forward !== undefined) this.#post(connection.page, { t: 'msg', c: connection.c, text: out.forward });
    if (out.reply !== undefined && connection.socket.readyState === WebSocket.OPEN) connection.socket.send(out.reply);
    if (out.ask !== undefined) this.#askPerson(connection, out.ask);
  }

  #askPerson(connection, request) {
    const ask = new AbortController();
    const slot = `${request.params.sessionId}\n${request.id}`;
    connection.asks.set(slot, ask);
    const timer = setTimeout(() => ask.abort(), this.#permissionTimeoutMs);
    timer.unref?.();
    const agent = this.#agents.find((candidate) => candidate.key === connection.grant.agentKey);
    const options = Array.isArray(request.params.options) ? request.params.options : [];
    const answered = this.#ui.askPermission(
      {
        origin: connection.page.origin,
        agent: agent?.label ?? '',
        title: shown(request.params.toolCall?.title, 500),
        options: options.map((option) => ({ optionId: String(option?.optionId), name: shown(option?.name, 120) })),
      },
      ask.signal,
    );
    Promise.resolve(answered)
      .catch(() => null)
      .then((optionId) => {
        clearTimeout(timer);
        connection.asks.delete(slot);
        const offered = options.some((option) => option?.optionId === optionId);
        if (connection.socket.readyState === WebSocket.OPEN) {
          connection.socket.send(permissionAnswer(request, offered ? optionId : null));
        }
      });
  }

  async #pageRevokes(page, { id }) {
    const grants = this.#visibleTo(page).filter((grant) => grant.id === id);
    if (grants.length === 0) throw refused();
    await this.#endGrants(grants);
    return undefined;
  }

  async #endGrants(grants) {
    this.#grants = this.#grants.filter((grant) => !grants.includes(grant));
    for (const grant of grants) this.#forgetSessionsIfUnused(grant.agentKey, grant.origin);
    for (const connection of this.#allConnections()) {
      if (grants.includes(connection.grant)) this.#close(connection, 'revoked');
    }
    await this.#save();
    for (const origin of new Set(grants.map((grant) => grant.origin))) this.#announce(origin);
  }

  /** A `once` grant lasts until its tab closes. */
  async tabClosed(tabId) {
    await this.#ready;
    await this.#endGrants(this.#grants.filter((grant) => grant.lifetime !== 'always' && grant.tabId === tabId));
  }

  /** What the person sees outside any page's request. */
  async state() {
    await this.#ready;
    return {
      agents: this.#agents.map(({ key, label, url, peer, source }) => ({ key, label, url, peer, source })),
      grants: this.#grants.map(({ uid, agentKey, origin, lifetime, cwd, permissions }) => ({ uid, agentKey, origin, lifetime, cwd, permissions })),
    };
  }

  async personAdds(record) {
    await this.#ready;
    const agent = this.#addByHand(record);
    await this.#save();
    return agent !== null;
  }

  async personRemoves(agentKey) {
    await this.#ready;
    await this.#endGrants(this.#grants.filter((grant) => grant.agentKey === agentKey));
    this.#agents = this.#agents.filter((agent) => agent.key !== agentKey);
    await this.#save();
  }

  async personRevokes(uid) {
    await this.#ready;
    await this.#endGrants(this.#grants.filter((grant) => grant.uid === uid));
  }

  /** Ends everything live, leaving what is stored, as a stopped service worker would. */
  dispose() {
    for (const page of [...this.#pages]) this.#pageGone(page);
  }
}
