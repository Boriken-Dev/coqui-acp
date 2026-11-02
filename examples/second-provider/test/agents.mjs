// Agents on loopback, one per URL path, over WebSocket frames written by hand so the example has no dependency. They
// follow ACP closely enough to be spoken to and record what they received.
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';

const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

function frame(payload, opcode = 1) {
  const body = Buffer.from(payload);
  let head;
  if (body.length < 126) {
    head = Buffer.from([0x80 | opcode, body.length]);
  } else if (body.length < 65536) {
    head = Buffer.alloc(4);
    head[0] = 0x80 | opcode;
    head[1] = 126;
    head.writeUInt16BE(body.length, 2);
  } else {
    head = Buffer.alloc(10);
    head[0] = 0x80 | opcode;
    head[1] = 127;
    head.writeBigUInt64BE(BigInt(body.length), 2);
  }
  return Buffer.concat([head, body]);
}

/** Calls `onFrame(opcode, fin, payload)` for each complete frame a browser sends, which are always masked. */
function frameReader(onFrame) {
  let pending = Buffer.alloc(0);
  return (chunk) => {
    pending = Buffer.concat([pending, chunk]);
    for (;;) {
      if (pending.length < 2) return;
      let length = pending[1] & 0x7f;
      let offset = 2;
      if (length === 126) {
        if (pending.length < 4) return;
        length = pending.readUInt16BE(2);
        offset = 4;
      } else if (length === 127) {
        if (pending.length < 10) return;
        length = Number(pending.readBigUInt64BE(2));
        offset = 10;
      }
      if (pending.length < offset + 4 + length) return;
      const mask = pending.subarray(offset, offset + 4);
      const payload = Buffer.from(pending.subarray(offset + 4, offset + 4 + length));
      for (let i = 0; i < payload.length; i += 1) payload[i] ^= mask[i % 4];
      onFrame(pending[0] & 0x0f, (pending[0] & 0x80) !== 0, payload);
      pending = pending.subarray(offset + 4 + length);
    }
  };
}

const CONFIG_OPTIONS = [
  { id: 'e', name: 'Effort', category: 'thought_level', type: 'select', currentValue: 'high', options: [] },
  { id: 'm', name: 'Mode', category: 'mode', type: 'select', currentValue: 'ask', options: [] },
  { id: 'x', name: 'Model', category: 'model', type: 'select', currentValue: 'one', options: [] },
];

function makeAgent() {
  const received = [];
  const connections = new Set();
  const waiting = new Map();
  let opened = 0;
  let closed = 0;
  let nextSession = 1;
  let nextRequest = 5000;

  const write = (socket, message) => socket.write(frame(JSON.stringify(message)));
  const update = (socket, sessionId, text) =>
    write(socket, {
      jsonrpc: '2.0',
      method: 'session/update',
      params: { sessionId, update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text } } },
    });

  // One turn: says something, asks for permission through ACP, and reports what it was told.
  async function turn(socket, message) {
    const { sessionId } = message.params;
    update(socket, sessionId, 'thinking');
    const id = (nextRequest += 1);
    const answered = new Promise((resolve) => waiting.set(id, resolve));
    write(socket, {
      jsonrpc: '2.0',
      id,
      method: 'session/request_permission',
      params: {
        sessionId,
        toolCall: { toolCallId: 'call-1', title: 'Write a file' },
        options: [
          { optionId: 'yes', name: 'Allow once', kind: 'allow_once' },
          { optionId: 'no', name: 'Reject', kind: 'reject_once' },
        ],
      },
    });
    const { result } = await answered;
    const { outcome } = result;
    update(socket, sessionId, `told:${outcome.optionId ?? outcome.outcome}`);
    write(socket, { jsonrpc: '2.0', id: message.id, result: { stopReason: 'end_turn' } });
  }

  function answer(socket, message) {
    const respond = (result) => write(socket, { jsonrpc: '2.0', id: message.id, result });
    switch (message.method) {
      case 'initialize':
        return respond({ protocolVersion: message.params?.protocolVersion ?? 1, agentCapabilities: { loadSession: true }, authMethods: [] });
      case 'session/new':
        return respond({ sessionId: `session-${(nextSession += 1)}`, configOptions: CONFIG_OPTIONS });
      case 'session/load':
      case 'session/resume':
      case 'session/set_config_option':
        return respond({ configOptions: CONFIG_OPTIONS });
      case 'session/prompt':
        return turn(socket, message);
      default:
        return message.id === undefined ? undefined : respond({});
    }
  }

  return {
    connect(socket) {
      opened += 1;
      connections.add(socket);
      const gone = () => {
        if (connections.delete(socket)) closed += 1;
      };
      socket.on('close', gone);
      socket.on('error', gone);
      let fragments = [];
      socket.on(
        'data',
        frameReader((opcode, fin, payload) => {
          if (opcode === 8) {
            socket.end(frame(payload, 8));
          } else if (opcode === 9) {
            socket.write(frame(payload, 10));
          } else if (opcode === 1 || opcode === 0) {
            fragments.push(payload);
            if (!fin) return;
            const message = JSON.parse(Buffer.concat(fragments).toString());
            fragments = [];
            received.push(message);
            if (message.method !== undefined) {
              answer(socket, message);
            } else {
              waiting.get(message.id)?.(message);
              waiting.delete(message.id);
            }
          }
        }),
      );
    },
    received: () => received,
    send(message) {
      for (const socket of connections) write(socket, message);
    },
    // Ends the connection from the agent's side with a proper closing handshake.
    closeAll() {
      for (const socket of connections) socket.write(frame(Buffer.alloc(0), 8));
    },
    destroyAll() {
      for (const socket of connections) socket.destroy();
    },
    counts: () => ({ opened, closed, open: connections.size }),
  };
}

/** A server for any number of agents, with their addresses known before anything has connected. */
export async function startAgents() {
  const agents = new Map();
  const sockets = new Set();
  const agent = (label) => {
    if (!agents.has(label)) agents.set(label, makeAgent());
    return agents.get(label);
  };
  const server = createServer((_request, response) => response.writeHead(426).end());
  server.on('upgrade', (request, socket) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    socket.setNoDelay(true);
    const accept = createHash('sha1').update(`${request.headers['sec-websocket-key']}${GUID}`).digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    agent(decodeURIComponent(new URL(request.url, 'http://x').pathname.slice(1))).connect(socket);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  return {
    agent,
    urlFor: (label) => `ws://127.0.0.1:${port}/${encodeURIComponent(label)}`,
    close() {
      for (const socket of sockets) socket.destroy();
      return new Promise((resolve) => server.close(resolve));
    },
  };
}
