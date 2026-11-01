// What a page may ask of an agent: the table in docs/browser.md "Scope", with no I/O, so any provider can use it.

export const METHOD_NOT_FOUND = -32601;
const INVALID_PARAMS = -32602;
const PARSE_ERROR = -32700;

const OWN_SESSION = new Set(['session/prompt', 'session/cancel', 'session/close']);
// A mode can stop the agent asking. A model or an effort cannot, and an unknown category is taken to be able to.
const OPEN_CATEGORIES = new Set(['model', 'thought_level']);
const GATED_SETTINGS = new Set(['session/set_mode', 'session/set_config_option']);
const REMOTE_SERVERS = new Set(['http', 'sse']);

const isObject = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
const hasId = (message) => message.id !== undefined && message.id !== null;

function refusal(id, code, why) {
  return JSON.stringify({ jsonrpc: '2.0', id: id ?? null, error: { code, message: why } });
}

function parse(text) {
  try {
    const message = JSON.parse(text);
    return isObject(message) ? message : undefined;
  } catch {
    return undefined;
  }
}

function withoutMeta(message) {
  const { _meta: _dropped, ...rest } = message;
  if (isObject(rest.params)) {
    const { _meta: _alsoDropped, ...params } = rest.params;
    rest.params = params;
  }
  return rest;
}

function remoteOrigin(server) {
  try {
    const url = new URL(server.url);
    return url.protocol === 'https:' ? url.origin : undefined;
  } catch {
    return undefined;
  }
}

/** One connection's view of a grant. `sessions` is the set of session ids this origin opened with this agent, kept by the provider. */
export class Scope {
  #grant;
  #origin;
  #sessions;
  #opening = new Map();
  #fromAgent = new Set();

  /**
   * @param {{cwd: string|null, permissions?: boolean, mcpOrigins?: string[], sessions?: Set<string>}} grant
   * @param {string} origin the page's origin, as the browser reported it
   */
  constructor(grant, origin) {
    this.#grant = grant;
    this.#origin = origin;
    this.#sessions = grant.sessions ?? new Set();
  }

  #serversAllowed(servers) {
    if (servers === undefined) return true;
    if (!Array.isArray(servers)) return false;
    const origins = new Set([this.#origin, ...(this.#grant.mcpOrigins ?? [])]);
    return servers.every((server) => {
      if (!isObject(server) || 'command' in server) return false;
      if (server.type === 'acp') return true;
      return REMOTE_SERVERS.has(server.type) && origins.has(remoteOrigin(server));
    });
  }

  #ownsSession(params) {
    return isObject(params) && this.#sessions.has(params.sessionId);
  }

  /** A message from the page: `{forward}` for the agent, or `{reply}` for the page, or neither when it is dropped. */
  fromPage(text) {
    const message = parse(text);
    if (message === undefined) return { reply: refusal(null, PARSE_ERROR, 'not one JSON-RPC object') };
    if (message.method === undefined) return this.#pageAnswers(message);
    const clean = withoutMeta(message);
    const { method, params } = clean;
    const refuse = (code, why) => (hasId(message) ? { reply: refusal(message.id, code, why) } : {});
    if (method === 'initialize') return { forward: JSON.stringify(clean) };
    if (method === 'session/new' || method === 'session/load' || method === 'session/resume') {
      if (!isObject(params)) return refuse(INVALID_PARAMS, 'no parameters');
      if (method !== 'session/new' && !this.#ownsSession(params)) {
        return refuse(METHOD_NOT_FOUND, 'not a session of this page');
      }
      if (typeof this.#grant.cwd !== 'string') return refuse(INVALID_PARAMS, 'no working directory was granted');
      if (!this.#serversAllowed(params.mcpServers)) {
        return refuse(INVALID_PARAMS, 'that MCP server is not allowed for this page');
      }
      if (hasId(message)) this.#opening.set(message.id, method);
      return { forward: JSON.stringify({ ...clean, params: { ...params, cwd: this.#grant.cwd } }) };
    }
    if (OWN_SESSION.has(method)) {
      return this.#ownsSession(params)
        ? { forward: JSON.stringify(clean) }
        : refuse(METHOD_NOT_FOUND, 'not a session of this page');
    }
    const ungated = method === 'session/set_model' || (method === 'session/set_config_option' && OPEN_CATEGORIES.has(params?.category));
    const allowed = ungated || (GATED_SETTINGS.has(method) && this.#grant.permissions === true);
    if (allowed && this.#ownsSession(params)) return { forward: JSON.stringify(clean) };
    return refuse(METHOD_NOT_FOUND, 'this page may not call that');
  }

  #pageAnswers(message) {
    if (!hasId(message) || !this.#fromAgent.delete(message.id)) return {};
    return { forward: JSON.stringify(withoutMeta(message)) };
  }

  /**
   * A message from the agent: `{forward}` for the page, `{reply}` for the agent, `{ask}` when the provider must put a
   * permission request to the person (and then call `answerPermission`), or none of them when it is dropped.
   */
  fromAgent(text) {
    const message = parse(text);
    if (message === undefined) return {};
    if (message.method === undefined) {
      if (hasId(message) && this.#opening.has(message.id)) {
        const opened = this.#opening.get(message.id);
        this.#opening.delete(message.id);
        const session = opened === 'session/new' ? message.result?.sessionId : undefined;
        if (typeof session === 'string') this.#sessions.add(session);
      }
      return { forward: JSON.stringify(message) };
    }
    const named = isObject(message.params) ? message.params.sessionId : undefined;
    if (named !== undefined && !this.#sessions.has(named)) {
      return hasId(message) ? { reply: refusal(message.id, METHOD_NOT_FOUND, 'not a session of this page') } : {};
    }
    if (message.method === 'session/request_permission' && this.#grant.permissions !== true) {
      return { ask: message };
    }
    if (hasId(message)) this.#fromAgent.add(message.id);
    return { forward: JSON.stringify(message) };
  }

  /** The answer to a request `fromAgent` returned as `{ask}`: an option the person chose, or none, which cancels it. */
  answerPermission(request, optionId) {
    const outcome = typeof optionId === 'string' ? { outcome: 'selected', optionId } : { outcome: 'cancelled' };
    return JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { outcome } });
  }
}
