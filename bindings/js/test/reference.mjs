// A provider that does what docs/browser.md says, in memory, so the conformance checks themselves can be tested.

import { registerProvider } from '../src/registry.mjs';
import { Scope } from '../src/scope.mjs';

class FakeWindow extends EventTarget {
  document = {};
}

class Agent {
  received_ = [];
  constructor() {
    this.toPage = () => {};
    this.next = 0;
  }
  received() {
    return this.received_;
  }
  send(message) {
    this.toPage(JSON.stringify(message));
  }
  deliver(text) {
    const message = JSON.parse(text);
    this.received_.push(message);
    if (message.id === undefined || message.method === undefined) return;
    const result = message.method === 'session/new' ? { sessionId: `s${(this.next += 1)}` } : {};
    this.toPage(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }));
  }
}

export function makeReferenceDriver() {
  const found = new Map();
  const grants = new Map();
  const live = new Set();
  const asked = [];
  let answer;
  const key = (origin, label) => `${origin} ${label}`;
  const person = {
    answerNext: (next) => {
      answer = next;
    },
    permissionsAsked: () => asked,
  };

  const page = (origin) => {
    const win = new FakeWindow();
    let activated = false;
    const open = new Set();
    const toAgent = (origin_, grant) => ({ id: `${origin_}#${grant.label}`, label: grant.label, provider: 'reference', source: 'manual', identity: { peer: null, verified: true, method: null }, offers: {}, status: 'available', scope: { permissions: grant.permissions === true, lifetime: 'always', mcpOrigins: grant.mcpOrigins ?? [] } });
    const held = () => [...grants.entries()].filter(([name]) => name.startsWith(`${origin} `)).map(([, grant]) => grant);
    const find = (agent) => held().find((grant) => toAgent(origin, grant).id === agent?.id);
    const closeAll = (reason, only) => {
      for (const connection of open) {
        if (only === undefined || connection.grant === only) connection.finish(reason);
      }
    };
    const provider = {
      info: { uuid: 'reference', name: 'Reference', rdns: 'dev.example.reference' },
      agents: async () => held().map((grant) => toAgent(origin, grant)),
      async request() {
        if (!activated) throw new DOMException('no user activation', 'NotAllowedError');
        activated = false;
        const decision = answer;
        answer = undefined;
        if (decision === undefined || decision.refuse) throw new DOMException('refused', 'NotAllowedError');
        const grant = { label: decision.allow, cwd: decision.cwd, permissions: decision.permissions, mcpOrigins: decision.mcpOrigins, sessions: new Set() };
        grants.set(key(origin, decision.allow), grant);
        return [toAgent(origin, grant)];
      },
      async connect(agent) {
        const grant = find(agent);
        if (grant === undefined) throw new DOMException('no grant', 'NotAllowedError');
        const scope = new Scope(grant, origin);
        const remote = found.get(grant.label);
        let output;
        const readable = new ReadableStream({ start: (controller) => { output = controller; } });
        let finished;
        const closed = new Promise((resolve) => { finished = resolve; });
        remote.toPage = (text) => {
          const step = scope.fromAgent(text);
          if (step.forward !== undefined) output.enqueue(step.forward);
          if (step.reply !== undefined) remote.deliver(step.reply);
          if (step.ask !== undefined) {
            asked.push(step.ask);
            remote.deliver(scope.answerPermission(step.ask, null));
          }
        };
        const writable = new WritableStream({
          write(text) {
            const step = scope.fromPage(text);
            if (step.forward !== undefined) remote.deliver(step.forward);
            if (step.reply !== undefined) output.enqueue(step.reply);
          },
        });
        const connection = {
          agent,
          readable,
          writable,
          closed,
          grant,
          finish(reason) {
            open.delete(connection);
            live.delete(connection);
            finished({ reason });
          },
          close() {
            connection.finish('page');
          },
        };
        open.add(connection);
        live.add(connection);
        return connection;
      },
      async revoke(agent) {
        const grant = find(agent);
        if (grant === undefined) return;
        closeAll('revoked', grant);
        grants.delete(key(origin, grant.label));
      },
    };
    const acp = registerProvider(provider, win);
    return {
      acp,
      window: win,
      activate() {
        activated = true;
      },
      navigate() {
        closeAll('navigation');
      },
    };
  };

  return {
    person,
    page,
    discover(label) {
      if (!found.has(label)) found.set(label, new Agent());
    },
    agent: (label) => found.get(label),
  };
}
