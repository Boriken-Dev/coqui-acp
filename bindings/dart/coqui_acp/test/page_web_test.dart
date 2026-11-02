// The binding in a real browser, against a registry that keeps the contract. A real provider is in Canoa's browser test.
@TestOn('browser')
library;

import 'dart:js_interop';
import 'dart:js_interop_unsafe';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp/page.dart';
import 'package:test/test.dart';

const _provider = r'''
(() => {
  const agent = { id: 'a1', label: 'Mine', provider: 'p', source: 'manual', identity: { peer: 'sha256:aa', verified: false },
    offers: {}, status: 'unknown', scope: { permissions: false, lifetime: 'once' } };
  const acp = {
    version: 1,
    providers: [],
    asked: [],
    agents: async () => [agent],
    request: async (options) => { acp.asked.push(options); return [agent]; },
    revoke: async () => {},
    connect: async (wanted) => {
      if (wanted.id !== 'a1') throw new DOMException('no', 'NotAllowedError');
      let reply;
      let closedWith;
      const closed = new Promise((resolve) => { closedWith = resolve; });
      const readable = new ReadableStream({ start(controller) { reply = controller; } });
      const writable = new WritableStream({
        write(text) {
          const { id, method } = JSON.parse(text);
          if (method === 'initialize') reply.enqueue(JSON.stringify({ jsonrpc: '2.0', id, result: { protocolVersion: 1, agentCapabilities: {} } }));
          else reply.enqueue(JSON.stringify({ jsonrpc: '2.0', id, result: { sessionId: 'from-the-provider' } }));
        },
      });
      return { agent: { ...agent, identity: { ...agent.identity, verified: true } }, readable, writable, closed,
        close() { reply.close(); closedWith({ reason: 'page' }); } };
    },
  };
  window.addEventListener('acp:request', () => window.dispatchEvent(new CustomEvent('acp:ready', { detail: { acp } })));
  window.fakeAcp = acp;
})();
''';

void main() {
  test('no provider is null, within the time given', () async {
    expect(await findAcp(timeout: const Duration(milliseconds: 20)), isNull);
  });

  group('with a provider', () {
    setUpAll(() {
      globalContext.callMethod<JSAny?>('eval'.toJS, _provider.toJS);
    });

    test(
      'a thing that is not version 1 of the contract is not a provider',
      () async {
        final other = globalContext.callMethod<JSObject>(
          'eval'.toJS,
          '''
        (() => {
          const answer = () => window.dispatchEvent(new CustomEvent('acp:ready', { detail: { acp: { version: 2, request() {} } } }));
          window.addEventListener('acp:request', answer);
          return { stop() { window.removeEventListener('acp:request', answer); } };
        })()
      '''
              .toJS,
        );
        final found = await findAcp(timeout: const Duration(milliseconds: 50));
        other.callMethod<JSAny?>('stop'.toJS);
        // The real provider answers too, and only it counts.
        expect(found, isNotNull);
        expect((await found!.agents()).single.label, 'Mine');
      },
    );

    test('agents and what a request says reach the provider', () async {
      final acp = (await findAcp())!;
      final agent = (await acp.agents()).single;
      expect(agent.id, 'a1');
      expect(agent.peer, 'sha256:aa');
      expect(agent.verified, isFalse);
      await acp.request(purpose: 'testing', peers: const ['sha256:aa']);
      final asked = globalContext
          .getProperty<JSObject>('fakeAcp'.toJS)
          .getProperty<JSArray<JSObject>>('asked'.toJS)
          .toDart
          .single;
      expect(asked.getProperty<JSString>('purpose'.toJS).toDart, 'testing');
      expect(
        asked.getProperty<JSArray<JSString>>('peers'.toJS).toDart.single.toDart,
        'sha256:aa',
      );
    });

    test('the shared client runs over a connection as it is', () async {
      final acp = (await findAcp())!;
      final connection = await acp.connect((await acp.agents()).single);
      expect(connection.agent.verified, isTrue);
      final client = AcpClient(
        incoming: connection.messages,
        send: connection.send,
        closeSource: connection.close,
      );
      await client.initialize();
      expect(await client.createSession(cwd: '/'), 'from-the-provider');
      await client.close();
      expect(await connection.closed, 'page');
    });

    test('a refusal arrives with its name', () async {
      final acp = (await findAcp())!;
      globalContext
          .getProperty<JSObject>('fakeAcp'.toJS)
          .setProperty(
            'agents'.toJS,
            globalContext.callMethod<JSFunction>(
              'eval'.toJS,
              "(async () => { throw new DOMException('no', 'NotAllowedError'); })"
                  .toJS,
            ),
          );
      await expectLater(
        acp.agents(),
        throwsA(
          isA<AcpPageFailure>().having(
            (failure) => failure.name,
            'name',
            'NotAllowedError',
          ),
        ),
      );
    });
  });
}
