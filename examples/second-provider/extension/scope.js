// The Scope tables of docs/browser.md, for one connection. No I/O.

const METHOD_NOT_FOUND = -32601;
const INVALID_PARAMS = -32602;
const INVALID_REQUEST = -32600;
const PARSE_ERROR = -32700;

const OWN_SESSION = new Set(['session/prompt', 'session/cancel', 'session/close', 'session/set_model']);
const OPENING = new Set(['session/new', 'session/load', 'session/resume']);
// A model or an effort cannot stop the agent asking. Anything else, or an option the agent did not describe, might.
const OPEN_CATEGORIES = new Set(['model', 'thought_level']);

const isObject = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
const isId = (id) => typeof id === 'string' || typeof id === 'number';

function refusal(id, code) {
  // The same words for a session that does not exist and one that is not this page's, so neither can be probed for.
  return JSON.stringify({ jsonrpc: '2.0', id: isId(id) ? id : null, error: { code, message: 'not allowed' } });
}

function parse(text) {
  if (typeof text !== 'string') return undefined;
  try {
    const message = JSON.parse(text);
    return isObject(message) && message.jsonrpc === '2.0' ? message : undefined;
  } catch {
    return undefined;
  }
}

// At every depth: ACP allows `_meta` on nested objects such as content blocks, and the agent reads those as well.
function withoutMeta(value) {
  if (Array.isArray(value)) return value.map(withoutMeta);
  if (!isObject(value)) return value;
  const out = {};
  for (const [key, item] of Object.entries(value)) {
    if (key !== '_meta') out[key] = withoutMeta(item);
  }
  return out;
}

/** The answer to a permission request the person holds: the option chosen, or none, which is ACP's `cancelled`. */
export function permissionAnswer(request, optionId) {
  const outcome = typeof optionId === 'string' ? { outcome: 'selected', optionId } : { outcome: 'cancelled' };
  return JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { outcome } });
}

export class Scope {
  /** Replaced when the person changes a live grant. */
  grant;
  #origin;
  #sessions;
  #sessionsChanged;
  #opening = new Map();
  #settings = new Map();
  #askedOfPage = new Set();
  #categories = new Map();

  /**
   * @param {{grant: {cwd: string|null, permissions: boolean, mcpOrigins: string[]}, origin: string, sessions: Set<string>,
   *   sessionsChanged?: () => void}} init `sessions` is shared by every connection of this origin to this agent.
   */
  constructor({ grant, origin, sessions, sessionsChanged = () => {} }) {
    this.grant = grant;
    this.#origin = origin;
    this.#sessions = sessions;
    this.#sessionsChanged = sessionsChanged;
  }

  #serversAllowed(servers) {
    if (servers === undefined) return true;
    if (!Array.isArray(servers)) return false;
    const origins = new Set([this.#origin, ...this.grant.mcpOrigins]);
    return servers.every((server) => {
      if (!isObject(server) || 'command' in server) return false;
      if (server.type === 'acp') return true;
      if (server.type !== 'http' && server.type !== 'sse') return false;
      try {
        const url = new URL(server.url);
        return url.protocol === 'https:' && origins.has(url.origin);
      } catch {
        return false;
      }
    });
  }

  #owns(params) {
    return isObject(params) && this.#sessions.has(params.sessionId);
  }

  #learnCategories(sessionId, options) {
    if (!Array.isArray(options)) return;
    const known = new Map();
    for (const option of options) {
      if (isObject(option) && typeof option.id === 'string') known.set(option.id, option.category);
    }
    this.#categories.set(sessionId, known);
  }

  // The category is read from what the agent described and never from the request, where the page would choose it.
  #settingAllowed(method, params) {
    if (this.grant.permissions === true) return true;
    if (method !== 'session/set_config_option') return false;
    return OPEN_CATEGORIES.has(this.#categories.get(params.sessionId)?.get(params.configId));
  }

  /** From the page: `{forward}` for the agent, `{reply}` for the page, or neither when it is dropped. `cancelled` names a session cancelled. */
  fromPage(text) {
    const parsed = parse(text);
    if (parsed === undefined) return { reply: refusal(null, PARSE_ERROR) };
    const message = withoutMeta(parsed);
    if (message.method === undefined) {
      if (!isId(message.id) || !this.#askedOfPage.delete(message.id)) return {};
      return { forward: JSON.stringify(message) };
    }
    const hasId = message.id !== undefined;
    const refuse = (code) => (hasId ? { reply: refusal(message.id, code) } : {});
    if (typeof message.method !== 'string' || (hasId && !isId(message.id))) return refuse(INVALID_REQUEST);
    const { method, params } = message;
    if (method === 'initialize') return { forward: JSON.stringify(message) };
    if (OPENING.has(method)) {
      if (!isObject(params)) return refuse(INVALID_PARAMS);
      if (method !== 'session/new' && !this.#owns(params)) return refuse(METHOD_NOT_FOUND);
      if (typeof this.grant.cwd !== 'string') return refuse(INVALID_PARAMS);
      if (!this.#serversAllowed(params.mcpServers)) return refuse(INVALID_PARAMS);
      if (hasId) this.#opening.set(message.id, { method, sessionId: params.sessionId });
      return { forward: JSON.stringify({ ...message, params: { ...params, cwd: this.grant.cwd } }) };
    }
    if (OWN_SESSION.has(method)) {
      if (!this.#owns(params)) return refuse(METHOD_NOT_FOUND);
      const forward = { forward: JSON.stringify(message) };
      return method === 'session/cancel' ? { ...forward, cancelled: params.sessionId } : forward;
    }
    if (method === 'session/set_mode' || method === 'session/set_config_option') {
      if (!this.#owns(params) || !this.#settingAllowed(method, params)) return refuse(METHOD_NOT_FOUND);
      if (hasId && method === 'session/set_config_option') this.#settings.set(message.id, params.sessionId);
      return { forward: JSON.stringify(message) };
    }
    return refuse(METHOD_NOT_FOUND);
  }

  #agentAnswered(message) {
    const opening = this.#opening.get(message.id);
    if (opening !== undefined) {
      this.#opening.delete(message.id);
      const sessionId = opening.method === 'session/new' ? message.result?.sessionId : opening.sessionId;
      if (typeof sessionId === 'string' && isObject(message.result)) {
        if (!this.#sessions.has(sessionId)) {
          this.#sessions.add(sessionId);
          this.#sessionsChanged();
        }
        this.#learnCategories(sessionId, message.result.configOptions);
      }
    }
    const setting = this.#settings.get(message.id);
    if (setting !== undefined) {
      this.#settings.delete(message.id);
      this.#learnCategories(setting, message.result?.configOptions);
    }
  }

  /** From the agent: `{forward}` for the page, `{reply}` for the agent, `{ask}` for the person, or none when it is dropped. */
  fromAgent(text) {
    const message = parse(text);
    if (message === undefined) return {};
    if (message.method === undefined) {
      this.#agentAnswered(message);
      return { forward: JSON.stringify(message) };
    }
    const hasId = message.id !== undefined;
    const params = isObject(message.params) ? message.params : {};
    if (message.method === 'session/update') {
      if (!this.#owns(params)) return {};
      if (params.update?.sessionUpdate === 'config_option_update') {
        this.#learnCategories(params.sessionId, params.update.configOptions);
      }
      return { forward: JSON.stringify(message) };
    }
    if (params.sessionId !== undefined && !this.#owns(params)) {
      return hasId ? { reply: refusal(message.id, METHOD_NOT_FOUND) } : {};
    }
    if (message.method === 'session/request_permission') {
      if (!this.#owns(params)) return hasId ? { reply: refusal(message.id, METHOD_NOT_FOUND) } : {};
      if (!hasId) return {};
      if (this.grant.permissions !== true) return { ask: message };
    }
    if (hasId) this.#askedOfPage.add(message.id);
    return { forward: JSON.stringify(message) };
  }
}
