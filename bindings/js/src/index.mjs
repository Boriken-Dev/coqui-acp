// For a page: find a provider, and offer an agent this page's WebMCP tools. Neither is needed to conform: docs/browser.md
// is the contract and this is a convenience over it.

export { registerProvider } from './registry.mjs';
export { Scope } from './scope.mjs';

const USABLE = (acp) => typeof acp?.request === 'function' && acp.version === 1;

/** The registry, or null if no provider answered within `timeoutMs`. By event, so markup cannot shadow it. */
export function findAcp({ timeoutMs = 100, win = globalThis.window } = {}) {
  return new Promise((resolve) => {
    const answered = (event) => {
      if (!USABLE(event.detail?.acp)) return;
      finish(event.detail.acp);
    };
    const finish = (acp) => {
      win.removeEventListener('acp:ready', answered);
      clearTimeout(timer);
      resolve(acp);
    };
    const timer = setTimeout(() => finish(null), timeoutMs);
    win.addEventListener('acp:ready', answered);
    win.dispatchEvent(new Event('acp:request'));
  });
}

/** One connection's streams as parsed JSON-RPC objects, for a library that wants those. */
export function asObjects({ readable, writable }) {
  return {
    readable: readable.pipeThrough(
      new TransformStream({ transform: (text, out) => out.enqueue(JSON.parse(text)) }),
    ),
    writable: (() => {
      const encode = new TransformStream({ transform: (message, out) => out.enqueue(JSON.stringify(message)) });
      encode.readable.pipeTo(writable).catch(() => {});
      return encode.writable;
    })(),
  };
}

const MCP_VERSION = '2026-07-28';

/**
 * Serves this page's WebMCP tools to an agent over MCP over ACP. Returns the entry for `session/new`'s `mcpServers`, and
 * `handle`, which answers an `mcp/message` request and returns the response to send, or null for any other message.
 * Without `confirm`, a tool marked `consequentialHint` is refused.
 */
export function exposeWebMcp({ modelContext = globalThis.document?.modelContext, name = 'page', confirm, filter } = {}) {
  if (typeof modelContext?.getTools !== 'function' || typeof modelContext?.executeTool !== 'function') {
    throw new DOMException('this document has no WebMCP tools', 'NotSupportedError');
  }
  const serverId = `${name}:${crypto.randomUUID()}`;
  const tools = async () => {
    const all = await modelContext.getTools();
    return filter === undefined ? all : all.filter(filter);
  };
  const call = async (params) => {
    const tool = (await tools()).find((candidate) => candidate.name === params?.name);
    if (tool === undefined) throw new Error('no such tool');
    if (tool.annotations?.consequentialHint === true && !(await confirm?.(tool, params.arguments))) {
      return { isError: true, content: [{ type: 'text', text: 'the person did not confirm this' }] };
    }
    const text = await modelContext.executeTool(tool, params.arguments ?? {});
    return { content: [{ type: 'text', text: text ?? '' }] };
  };
  const answer = async (method, params) => {
    if (method === 'tools/list') {
      const listed = (await tools()).map(({ name: toolName, description, inputSchema, annotations }) => ({
        name: toolName,
        description,
        inputSchema: inputSchema ?? { type: 'object' },
        annotations,
      }));
      return { tools: listed };
    }
    if (method === 'tools/call') return call(params);
    if (method === 'initialize') {
      return { protocolVersion: MCP_VERSION, capabilities: { tools: {} }, serverInfo: { name, version: '1' } };
    }
    throw new Error('method not found');
  };
  return {
    server: { type: 'acp', name, serverId },
    async handle(message) {
      if (message?.method !== 'mcp/message' || message.params?.serverId !== serverId) return null;
      try {
        return { jsonrpc: '2.0', id: message.id, result: await answer(message.params.method, message.params.params) };
      } catch (error) {
        return { jsonrpc: '2.0', id: message.id, error: { code: -32603, message: String(error?.message ?? error) } };
      }
    },
  };
}
