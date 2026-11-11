// The events that end a turn or say a request was blocked name the session they are about, so a client that shares one connection
// between sessions can tell whose turn ended.
import 'dart:async';
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:test/test.dart';

String? _session(AcpEvent event) => switch (event) {
  AcpCompleted(:final sessionId) => sessionId,
  AcpCancelled(:final sessionId) => sessionId,
  AcpFailed(:final sessionId) => sessionId,
  AcpBlocked(:final sessionId) => sessionId,
  _ => null,
};

Future<
  ({AcpClient client, StreamController<String> toClient, List<AcpEvent> events})
>
_connect(int version, {AcpDecider? decider}) async {
  final toClient = StreamController<String>();
  late AcpClient client;
  Future<void> send(String line) async {
    final message = jsonDecode(line) as Map<String, dynamic>;
    final id = message['id'];
    final method = message['method'];
    if (id == null || method == null) return;
    Object? result = <String, Object?>{};
    if (method == 'initialize') {
      result = version == 1
          ? {'protocolVersion': 1, 'agentCapabilities': <String, Object?>{}}
          : {
              'protocolVersion': 2,
              'capabilities': {'session': <String, Object?>{}},
            };
    } else if (method == 'session/new') {
      result = {'sessionId': 's${message['id']}'};
    } else if (method == 'session/prompt') {
      // The answer to a v1 prompt ends its turn; a v2 turn ends with a state update.
      if (version == 2) result = {'messageId': 'm1'};
      final stop = (message['params'] as Map)['prompt'] is List
          ? (((message['params'] as Map)['prompt'] as List).first
                as Map)['text']
          : null;
      if (version == 1) {
        result = {'stopReason': stop == 'cancel me' ? 'cancelled' : 'end_turn'};
      }
    }
    toClient.add(jsonEncode({'jsonrpc': '2.0', 'id': id, 'result': result}));
  }

  client = AcpClient(incoming: toClient.stream, send: send, decider: decider);
  final events = <AcpEvent>[];
  client.events.listen(events.add);
  await client.initialize();
  return (client: client, toClient: toClient, events: events);
}

void main() {
  test('a v1 turn ends with its session\'s id, however it ends', () async {
    final (:client, :toClient, :events) = await _connect(1);
    final one = await client.openSession(cwd: '/w');
    final two = await client.openSession(cwd: '/w');
    await client.prompt(one.id, 'hello');
    await client.prompt(two.id, 'cancel me');
    expect(events.map((e) => (e.runtimeType, _session(e))), [
      (AcpCompleted, one.id),
      (AcpCancelled, two.id),
    ]);
    await client.close();
  });

  test(
    'a v2 turn ends with the id of the session whose state update said so',
    () async {
      final (:client, :toClient, :events) = await _connect(2);
      final one = await client.openSession(cwd: '/w');
      final two = await client.openSession(cwd: '/w');
      for (final (session, stop) in [
        (two.id, 'end_turn'),
        (one.id, 'cancelled'),
        (two.id, 'refusal'),
      ]) {
        toClient.add(
          jsonEncode({
            'jsonrpc': '2.0',
            'method': 'session/update',
            'params': {
              'sessionId': session,
              'update': {
                'sessionUpdate': 'state_update',
                'state': 'idle',
                'stopReason': stop,
              },
            },
          }),
        );
      }
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(events.map((e) => (e.runtimeType, _session(e))), [
        (AcpCompleted, two.id),
        (AcpCancelled, one.id),
        (AcpFailed, two.id),
      ]);
      await client.close();
    },
  );

  test(
    'a request that could not be put to a person is blocked with the id of the session it was for',
    () async {
      final (:client, :toClient, :events) = await _connect(
        1,
        decider: (_) async => const AcpSelect('not offered'),
      );
      final one = await client.openSession(cwd: '/w');
      toClient.add(
        jsonEncode({
          'jsonrpc': '2.0',
          'id': 90,
          'method': 'session/request_permission',
          'params': {
            'sessionId': one.id,
            'toolCall': {'toolCallId': 't', 'title': 'Edit'},
            'options': [
              {'optionId': 'a', 'name': 'Yes', 'kind': 'allow_once'},
            ],
          },
        }),
      );
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(events.whereType<AcpBlocked>().map((e) => e.sessionId), [one.id]);
      await client.close();
    },
  );

  test('the ids are optional, so an event built without one is as it was', () {
    expect(const AcpCancelled().sessionId, isNull);
    expect(const AcpCompleted('end_turn').sessionId, isNull);
    expect(const AcpFailed('x').sessionId, isNull);
    expect(const AcpBlocked('x').sessionId, isNull);
  });
}
