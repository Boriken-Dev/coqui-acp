@TestOn('vm')
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:test/test.dart';

/// Every key of [want] holds the same value in [have], recursively.
bool holds(Object? have, Object? want) {
  if (want is Map) {
    return have is Map && want.entries.every((entry) => holds(have[entry.key], entry.value));
  }
  return have == want;
}

/// The agent's end: plays a conversation's steps against the client.
final class Agent {
  final fromClient = StreamController<String>();
  final toClient = StreamController<String>();
  late final _inbox = StreamIterator<String>(fromClient.stream);
  var _nextId = 1000;

  Future<Map<String, dynamic>> read() async {
    if (!await _inbox.moveNext().timeout(const Duration(seconds: 10))) throw StateError('client hung up');
    return jsonDecode(_inbox.current) as Map<String, dynamic>;
  }

  void write(Map<String, Object?> message) => toClient.add(jsonEncode({'jsonrpc': '2.0', ...message}));

  Future<void> play(List<dynamic> steps) async {
    Object? held;
    for (final raw in steps) {
      final step = raw as Map<String, dynamic>;
      if (step['expect'] case final Map<String, dynamic> expect) {
        final message = await read();
        expect_(message['method'], expect['method']);
        if (expect['params'] case final Object params) {
          if (!holds(message['params'], params)) throw StateError('${message['params']} should hold $params');
        }
        if (step['reply'] case final Object reply) {
          write({'id': message['id'], 'result': reply});
        } else if (step['error'] case final Object error) {
          write({'id': message['id'], 'error': error});
        } else if (step['hold'] != null) {
          held = message['id'];
        }
      } else if (step['send'] case final Map<String, dynamic> send) {
        write({'method': send['method'], 'params': send['params']});
      } else if (step['ask'] case final Map<String, dynamic> ask) {
        final id = ++_nextId;
        write({'id': id, 'method': ask['method'], 'params': ask['params']});
        final answer = await read();
        expect_(answer['id'], id);
        if (!holds(answer['result'], step['answer'])) throw StateError('${answer['result']} should hold ${step['answer']}');
      } else if (step['release'] case final Object result) {
        write({'id': held, 'result': result});
      }
    }
  }

  static void expect_(Object? got, Object? want) {
    if (got != want) throw StateError('got $got, wanted $want');
  }
}

bool matchesEvent(AcpEvent event, Map<String, dynamic> want) => switch (event) {
  AcpText(:final text) => want['type'] == 'text' && want['text'] == text,
  AcpTool(:final title, :final kind) => want['type'] == 'tool' && want['title'] == title && want['kind'] == kind,
  AcpCost(:final amount, :final currency) => want['type'] == 'cost' && want['amount'] == amount && want['currency'] == currency,
  AcpPermission(:final allowed) => want['type'] == 'permission' && want['allowed'] == allowed,
  AcpCompleted(:final stop) => want['type'] == 'completed' && want['stop'] == stop,
  AcpFailed() => want['type'] == 'failed',
  AcpBlocked() => want['type'] == 'blocked',
  _ => false,
};

void main() {
  final vectors = jsonDecode(File('../../../vectors/conversations.json').readAsStringSync()) as Map<String, dynamic>;

  for (final raw in vectors['conversations'] as List<dynamic>) {
    final conversation = raw as Map<String, dynamic>;
    test(conversation['name'] as String, () async {
      final agent = Agent();
      final client = conversation['client'] as Map<String, dynamic>;
      final acp = AcpClient(
        incoming: agent.toClient.stream,
        send: (line) async => agent.fromClient.add(line),
        maxProtocolVersion: conversation['version'] as int,
        decider: (request) async => client['decider'] == 'allow' ? const AcpAllow() : const AcpDeny('no'),
      );
      final events = <AcpEvent>[];
      final done = Completer<void>();
      final want = (conversation['events'] as List<dynamic>).cast<Map<String, dynamic>>();
      acp.events.listen((event) {
        events.add(event);
        if (events.length == want.length && !done.isCompleted) done.complete();
      });
      final script = agent.play(conversation['steps'] as List<dynamic>);
      await acp.initialize();
      final session = await acp.createSession(cwd: client['cwd'] as String);
      await acp.prompt(session, client['prompt'] as String);
      await script.timeout(const Duration(seconds: 10));
      await done.future.timeout(const Duration(seconds: 10));
      for (var at = 0; at < want.length; at++) {
        expect(matchesEvent(events[at], want[at]), isTrue, reason: '${events[at]} should match ${want[at]}');
      }
      await acp.close();
    });
  }

  test('asked for v2, a v1 agent gets a v1 session', () async {
    final agent = Agent();
    final acp = AcpClient(incoming: agent.toClient.stream, send: (line) async => agent.fromClient.add(line));
    final opening = acp.initialize();
    final init = await agent.read();
    expect((init['params'] as Map)['protocolVersion'], 2);
    agent.write({'id': init['id'], 'result': {'protocolVersion': 1, 'agentCapabilities': <String, Object?>{}}});
    await opening;
    expect(acp.protocolVersion, 1);
    await acp.close();
  });

  test('an agent of a version this client lacks is refused', () async {
    final agent = Agent();
    final acp = AcpClient(incoming: agent.toClient.stream, send: (line) async => agent.fromClient.add(line));
    final opening = expectLater(acp.initialize(), throwsA(isA<AcpFailure>()));
    final init = await agent.read();
    agent.write({'id': init['id'], 'result': {'protocolVersion': 9}});
    await opening;
    await acp.close();
  });

  test('a v1 prompt outlives the request timeout, since it lasts the whole turn', () async {
    final agent = Agent();
    final acp = AcpClient(
      incoming: agent.toClient.stream,
      send: (line) async => agent.fromClient.add(line),
      maxProtocolVersion: 1,
      requestTimeout: const Duration(milliseconds: 50),
    );
    final events = <AcpEvent>[];
    acp.events.listen(events.add);
    final opening = acp.initialize();
    final init = await agent.read();
    agent.write({'id': init['id'], 'result': {'protocolVersion': 1, 'agentCapabilities': <String, Object?>{}}});
    await opening;
    final created = acp.createSession(cwd: '/w');
    final create = await agent.read();
    agent.write({'id': create['id'], 'result': {'sessionId': 's'}});
    final session = await created;
    final prompting = acp.prompt(session, 'go');
    final prompt = await agent.read();
    await Future<void>.delayed(const Duration(milliseconds: 150));
    agent.write({'id': prompt['id'], 'result': {'stopReason': 'end_turn'}});
    await prompting;
    await Future<void>.delayed(const Duration(milliseconds: 10));
    expect(events.single, isA<AcpCompleted>());
    await acp.close();
  });

  test('a relative working directory is refused before anything is sent', () async {
    final agent = Agent();
    final acp = AcpClient(incoming: agent.toClient.stream, send: (line) async => agent.fromClient.add(line), maxProtocolVersion: 1);
    final opening = acp.initialize();
    final init = await agent.read();
    agent.write({'id': init['id'], 'result': {'protocolVersion': 1, 'agentCapabilities': <String, Object?>{}}});
    await opening;
    expect(() => acp.createSession(cwd: 'relative'), throwsArgumentError);
    await acp.close();
  });

  test('a lost connection fails what is pending and never retries', () async {
    final agent = Agent();
    final acp = AcpClient(incoming: agent.toClient.stream, send: (line) async => agent.fromClient.add(line), maxProtocolVersion: 1);
    final opening = expectLater(acp.initialize(), throwsA(isA<AcpFailure>()));
    await agent.read();
    await agent.toClient.close();
    await opening;
    expect(acp.isClosed, isTrue);
  });

  Future<(Agent, AcpClient)> opened({AcpDecider? decider}) async {
    final agent = Agent();
    final acp = AcpClient(incoming: agent.toClient.stream, send: (line) async => agent.fromClient.add(line), decider: decider);
    final opening = acp.initialize();
    final init = await agent.read();
    agent.write({'id': init['id'], 'result': {'protocolVersion': 1, 'agentCapabilities': <String, Object?>{}}});
    await opening;
    return (agent, acp);
  }

  test('what an agent says about a new session reaches the caller', () async {
    final (agent, acp) = await opened();
    final opening = acp.openSession(cwd: '/work');
    final asked = await agent.read();
    agent.write({'id': asked['id'], 'result': {
      'sessionId': 's1',
      'configOptions': [{'id': 'model', 'category': 'model', 'currentValue': 'a'}],
      'modes': {'currentModeId': 'plan', 'availableModes': <Object>[]},
    }});
    final session = await opening;
    expect(session.id, 's1');
    expect((session.raw['configOptions'] as List).single, containsPair('id', 'model'));
    expect((session.raw['modes'] as Map)['currentModeId'], 'plan');
    await acp.close();
  });

  test('an option and a mode go to the agent, named as ACP names them', () async {
    final (agent, acp) = await opened();
    final setting = acp.setConfigOption('s1', 'model', 'b');
    final option = await agent.read();
    expect(option['method'], 'session/set_config_option');
    expect(option['params'], {'sessionId': 's1', 'configId': 'model', 'value': 'b'});
    agent.write({'id': option['id'], 'result': {'configOptions': <Object>[]}});
    expect(await setting, containsPair('configOptions', isEmpty));
    final moving = acp.setMode('s1', 'plan');
    final mode = await agent.read();
    expect(mode['method'], 'session/set_mode');
    expect(mode['params'], {'sessionId': 's1', 'modeId': 'plan'});
    agent.write({'id': mode['id'], 'result': <String, Object?>{}});
    await moving;
    await acp.close();
  });

  test('a turn cancelled while a permission waits answers cancelled, not a refusal', () async {
    final (agent, acp) = await opened(decider: (_) async => const AcpCancel());
    agent.write({'id': 9, 'method': 'session/request_permission', 'params': {
      'sessionId': 's1',
      'toolCall': {'toolCallId': 't1', 'title': 'x'},
      'options': [{'optionId': 'no', 'kind': 'reject_once'}],
    }});
    final answer = await agent.read();
    expect(answer['id'], 9);
    expect(answer['result'], {'outcome': {'outcome': 'cancelled'}});
    await acp.close();
  });
}
