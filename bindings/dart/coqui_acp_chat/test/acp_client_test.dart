// The chat over a real `coqui_acp` client on messages in memory: updates arrive on the connection, a permission request reaches its
// conversation and its answer goes back on the wire, and a v1 turn ends with the answer to its prompt.
import 'dart:async';
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

final class _Agent {
  final toClient = StreamController<String>();
  final received = <Map<String, dynamic>>[];

  Future<void> send(String line) async {
    final message = jsonDecode(line) as Map<String, dynamic>;
    received.add(message);
    final method = message['method'];
    final id = message['id'];
    if (method == null || id == null) return;
    final result = switch (method) {
      'initialize' => {
        'protocolVersion': 1,
        'agentCapabilities': <String, Object?>{},
      },
      'session/new' => {'sessionId': 's1'},
      'session/prompt' => null,
      _ => <String, Object?>{},
    };
    if (result != null) reply(id, result);
  }

  void reply(Object? id, Object? result) =>
      toClient.add(jsonEncode({'jsonrpc': '2.0', 'id': id, 'result': result}));

  void notify(Map<String, Object?> update) => toClient.add(
    jsonEncode({
      'jsonrpc': '2.0',
      'method': 'session/update',
      'params': {'sessionId': 's1', 'update': update},
    }),
  );

  Map<String, dynamic>? answerTo(int id) =>
      received.where((m) => m['id'] == id && m['method'] == null).firstOrNull;

  Map<String, dynamic> last(String method) =>
      received.lastWhere((m) => m['method'] == method);
}

Future<
  ({
    _Agent agent,
    ChatStore store,
    ChatSession session,
    AcpChatConnection connection,
  })
>
_connected() async {
  final agent = _Agent();
  final store = ChatStore();
  final connection = AcpChatConnection.routed(
    store,
    (decider, elicitor) => AcpClient(
      incoming: agent.toClient.stream,
      send: agent.send,
      decider: decider,
      elicitor: elicitor,
    ),
  );
  await connection.client.initialize();
  final opened = await connection.client.openSession(cwd: '/work/demo');
  final session = store.add(
    title: 'Scripted',
    agent: 'Scripted',
    sessionId: opened.id,
  )..attach(connection);
  return (agent: agent, store: store, session: session, connection: connection);
}

void main() {
  test(
    'what the agent sends on the connection builds the conversation',
    () async {
      final (:agent, :store, :session, :connection) = await _connected();
      agent.notify(text('agent_message_chunk', 'Hello'));
      agent.notify(text('agent_message_chunk', ', there.'));
      agent.notify({
        'sessionUpdate': 'plan',
        'entries': [
          {'content': 'Say hello', 'status': 'in_progress'},
        ],
      });
      await settle();
      expect(session.messages.single.text, 'Hello, there.');
      expect(session.plan.single.status, PlanStatus.running);
      expect(connection.protocolVersion, 1);
      store.dispose();
    },
  );

  test(
    'a permission request waits for the person, and the answer chosen is what the agent is sent',
    () async {
      final (:agent, :store, :session, :connection) = await _connected();
      agent.toClient.add(
        jsonEncode({
          'jsonrpc': '2.0',
          'id': 41,
          'method': 'session/request_permission',
          'params': {
            'sessionId': 's1',
            'toolCall': {
              'toolCallId': 't41',
              'title': 'Edit a.txt',
              'kind': 'edit',
              'rawInput': {'file_path': 'a.txt'},
            },
            'options': [
              {
                'optionId': 'allow-always',
                'name': 'Always',
                'kind': 'allow_always',
              },
              {'optionId': 'reject', 'name': 'No', 'kind': 'reject_once'},
            ],
          },
        }),
      );
      await settle();
      final question = session.pendingPermissions.single;
      expect(question.options.map((o) => o.optionId), [
        'allow-always',
        'reject',
      ]);
      expect(session.toolEntries['t41']!.tool!.title, 'Edit a.txt');
      expect(
        agent.answerTo(41),
        isNull,
        reason: 'nothing is answered until a person does',
      );
      session.choosePermission(question.options.first);
      await settle();
      expect(
        jsonEncode(agent.answerTo(41)!['result']),
        contains('allow-always'),
      );
      store.dispose();
    },
  );

  test(
    'a request after the conversation was closed is refused on the wire, and Stop answers a waiting one cancelled',
    () async {
      final (:agent, :store, :session, :connection) = await _connected();
      Map<String, Object?> ask(int id) => {
        'jsonrpc': '2.0',
        'id': id,
        'method': 'session/request_permission',
        'params': {
          'sessionId': 's1',
          'toolCall': {'toolCallId': 't$id', 'title': 'Run', 'kind': 'execute'},
          'options': [
            {'optionId': 'allow', 'name': 'Yes', 'kind': 'allow_once'},
          ],
        },
      };
      agent.toClient.add(jsonEncode(ask(50)));
      await settle();
      await session.cancel();
      await settle();
      expect(jsonEncode(agent.answerTo(50)!['result']), contains('cancelled'));
      expect(agent.last('session/cancel')['params'], {'sessionId': 's1'});
      await store.close(session);
      agent.toClient.add(jsonEncode(ask(51)));
      await settle();
      expect(
        agent.answerTo(51),
        isNull,
        reason: 'a closed connection answers nothing',
      );
      store.dispose();
    },
  );

  test(
    'a v1 turn shows its message at once and ends with the answer to its prompt',
    () async {
      final (:agent, :store, :session, :connection) = await _connected();
      await session.send('Say hello');
      await settle();
      expect(session.status, ChatStatus.working);
      expect(session.messages.single.text, 'Say hello');
      final prompt = agent.last('session/prompt');
      expect((prompt['params'] as Map)['sessionId'], 's1');
      await session.send('And then this');
      expect(session.queue.single.text, 'And then this');
      agent.reply(prompt['id'], {'stopReason': 'end_turn'});
      await settle();
      expect(
        agent.received.where((m) => m['method'] == 'session/prompt'),
        hasLength(2),
        reason: 'the queued message went when the turn ended',
      );
      expect(session.turns.first.outcome, TurnOutcome.completed);
      store.dispose();
    },
  );
}
