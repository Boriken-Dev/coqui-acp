// Earlier sessions and attachments, against an agent saying what the real adapter says, in both versions.
@TestOn('vm')
library;

import 'dart:async';
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:test/test.dart';

final class Agent {
  Agent(this.initialize);

  final Map<String, Object?> initialize;
  final toClient = StreamController<String>();
  final received = <Map<String, dynamic>>[];
  final answers = <String, Object? Function(Map<String, dynamic>)>{};

  Future<void> send(String text) async {
    final message = jsonDecode(text) as Map<String, dynamic>;
    received.add(message);
    final method = message['method'] as String?;
    if (method == null || message['id'] == null) return;
    final answer = method == 'initialize' ? (_) => initialize : answers[method];
    final result = answer?.call(message);
    toClient.add(
      jsonEncode({
        'jsonrpc': '2.0',
        'id': message['id'],
        if (answer == null)
          'error': {'code': -32601, 'message': 'Method not found'}
        else
          'result': result,
      }),
    );
  }

  Future<AcpClient> connected({int maxVersion = 2}) async {
    final client = AcpClient(
      incoming: toClient.stream,
      send: send,
      maxProtocolVersion: maxVersion,
    );
    await client.initialize();
    return client;
  }

  Map<String, dynamic> last(String method) =>
      received.lastWhere((message) => message['method'] == method);
}

// What the real adapter reports, 0.99.1.
final Map<String, Object?> v1 = {
  'protocolVersion': 1,
  'agentCapabilities': {
    'promptCapabilities': {'image': true, 'embeddedContext': true},
    'mcpCapabilities': {'http': true, 'sse': true},
    'loadSession': true,
    'sessionCapabilities': {
      'list': <String, Object?>{},
      'resume': <String, Object?>{},
    },
  },
};
final Map<String, Object?> v2 = {
  'protocolVersion': 2,
  'capabilities': {
    'session': {
      'prompt': {
        'image': <String, Object?>{},
        'embeddedContext': <String, Object?>{},
      },
      'mcp': {'stdio': <String, Object?>{}, 'http': <String, Object?>{}},
    },
  },
};

/// The texts of a turn whose agent sends [chunks], read through a client [open] gives.
Future<List<String>> textsOf(
  List<String> chunks,
  AcpClient Function(Agent) open,
) async {
  final agent = Agent(v1);
  agent.answers['session/new'] = (_) => {'sessionId': 's1'};
  agent.answers['session/prompt'] = (_) {
    for (final chunk in chunks) {
      agent.toClient.add(
        jsonEncode({
          'jsonrpc': '2.0',
          'method': 'session/update',
          'params': {
            'sessionId': 's1',
            'update': {
              'sessionUpdate': 'agent_message_chunk',
              'content': {'type': 'text', 'text': chunk},
            },
          },
        }),
      );
    }
    return {'stopReason': 'end_turn'};
  };
  final client = open(agent);
  final texts = <String>[];
  client.events.listen((event) {
    if (event is AcpText) texts.add(event.text);
  });
  await client.initialize();
  final session = await client.openSession(cwd: '/w');
  await client.prompt(session.id, 'go');
  await client.close();
  return texts;
}

void main() {
  test('text is flushed at the size set, in UTF-8 bytes', () async {
    AcpClient over(Agent agent, {int? flush}) => flush == null
        ? AcpClient(incoming: agent.toClient.stream, send: agent.send)
        : AcpClient(
            incoming: agent.toClient.stream,
            send: agent.send,
            textFlush: flush,
          );
    expect(await textsOf(['é', 'ab'], over), ['éab']);
    expect(await textsOf(['é', 'ab'], (agent) => over(agent, flush: 2)), [
      'é',
      'ab',
    ]);
  });

  test(
    'what an agent takes is read from each version, and nothing said is no',
    () async {
      for (final said in [v1, v2]) {
        final client = await Agent(said).connected();
        expect(client.takesImages, isTrue);
        expect(client.takesFiles, isTrue);
        expect(client.canReopenSessions, isTrue);
      }
      final bare = await Agent({'protocolVersion': 1}).connected();
      expect(bare.takesImages, isFalse);
      expect(bare.takesFiles, isFalse);
      expect(bare.canReopenSessions, isFalse);
      final off = await Agent({
        'protocolVersion': 1,
        'agentCapabilities': {
          'promptCapabilities': {'image': false},
          'loadSession': false,
        },
      }).connected();
      expect(off.takesImages, isFalse);
      expect(off.canReopenSessions, isFalse);
    },
  );

  test(
    'request ids do not start again with a new connection, so a late answer is never taken for a new request',
    () async {
      final first = Agent(v1);
      final second = Agent(v1);
      await first.connected();
      await second.connected();
      expect(first.received.first['id'], isA<int>());
      expect(first.received.first['id'], isNot(second.received.first['id']));
    },
  );

  test('a listing takes what is well formed, and bounds how much', () async {
    final agent = Agent(v2);
    agent.answers['session/list'] = (_) => {
      'sessions': [
        {
          'sessionId': 'a',
          'cwd': '/w',
          'title': 'First',
          'updatedAt': '2026-11-02T00:00:00Z',
        },
        {
          'sessionId': 'b',
          'cwd': 7,
          'title': ['no'],
        },
        {'title': 'no id'},
        {'sessionId': ''},
        'not an object',
        for (var i = 0; i < 600; i++) {'sessionId': 'n$i'},
      ],
      'nextCursor': 'more',
    };
    final page = await (await agent.connected()).listSessions(cwd: '/w');
    expect(page.sessions.first.title, 'First');
    expect(page.sessions[1].id, 'b');
    expect(page.sessions[1].cwd, isNull);
    expect(page.sessions[1].title, isNull);
    expect(page.sessions, hasLength(500));
    expect(page.sessions.last.id, 'n497');
    expect(page.incomplete, 'the sessions limit was reached');
    expect(page.nextCursor, 'more');
    expect(agent.last('session/list')['params'], {'cwd': '/w'});
  });

  test(
    'the sessions kept of a page are a setting, and a page cut short says so',
    () async {
      Future<AcpSessionPage> listed(int count, int? keep) async {
        final agent = Agent(v2);
        agent.answers['session/list'] = (_) => {
          'sessions': [
            {'title': 'no id'},
            for (var i = 0; i < count; i++) {'sessionId': 'n$i'},
          ],
        };
        final client = keep == null
            ? AcpClient(incoming: agent.toClient.stream, send: agent.send)
            : AcpClient(
                incoming: agent.toClient.stream,
                send: agent.send,
                maxListedSessions: keep,
              );
        expect(client.maxListedSessions, keep ?? 500);
        await client.initialize();
        return client.listSessions();
      }

      final cut = await listed(3, 2);
      expect(cut.sessions.map((session) => session.id), ['n0', 'n1']);
      expect(cut.incomplete, 'the sessions limit was reached');
      final whole = await listed(2, 2);
      expect(whole.sessions, hasLength(2));
      expect(whole.incomplete, isNull);
      final none = await listed(1, 0);
      expect(none.sessions, isEmpty);
      expect(none.incomplete, 'the sessions limit was reached');
      expect((await listed(0, 0)).incomplete, isNull);
      expect((await listed(500, null)).incomplete, isNull);
    },
  );

  test(
    'a v1 agent that does not offer a list is not asked, and a v2 one that fails says so',
    () async {
      final plain = Agent({'protocolVersion': 1});
      await expectLater(
        (await plain.connected()).listSessions(),
        throwsA(isA<AcpFailure>()),
      );
      expect(
        plain.received.where((message) => message['method'] == 'session/list'),
        isEmpty,
      );
      await expectLater(
        (await Agent(v2).connected()).listSessions(),
        throwsA(isA<AcpFailure>()),
      );
    },
  );

  test(
    'v1 reopens by resume where it is offered, even beside load, and by load, which replays, where it is not',
    () async {
      final both = Agent(v1);
      both.answers['session/resume'] = (_) => {'configOptions': <Object>[]};
      final resumed = await (await both.connected()).reopenSession(
        's',
        cwd: '/w',
      );
      expect(resumed.replayed, isFalse);
      expect(resumed.raw, {'configOptions': <Object>[]});
      expect(both.last('session/resume')['params'], {
        'sessionId': 's',
        'cwd': '/w',
        'mcpServers': <Object>[],
      });
      expect(both.received.any((m) => m['method'] == 'session/load'), isFalse);

      final agent = Agent({
        'protocolVersion': 1,
        'agentCapabilities': {'loadSession': true},
      });
      agent.answers['session/load'] = (_) => <String, Object?>{};
      final opened = await (await agent.connected()).reopenSession(
        's',
        cwd: '/w',
      );
      expect(opened.replayed, isTrue);
      expect(agent.last('session/load')['params'], {
        'sessionId': 's',
        'cwd': '/w',
        'mcpServers': <Object>[],
      });

      final resuming = Agent({
        'protocolVersion': 1,
        'agentCapabilities': {
          'sessionCapabilities': {'resume': <String, Object?>{}},
        },
      });
      resuming.answers['session/resume'] = (_) => <String, Object?>{};
      expect(
        (await (await resuming.connected()).reopenSession(
          's',
          cwd: '/w',
        )).replayed,
        isFalse,
      );

      final neither = await Agent({'protocolVersion': 1}).connected();
      await expectLater(
        neither.reopenSession('s', cwd: '/w'),
        throwsA(isA<AcpFailure>()),
      );
    },
  );

  test(
    'v2 reopens by resume from the start, and a relative directory goes nowhere',
    () async {
      final agent = Agent(v2);
      agent.answers['session/resume'] = (_) => {'configOptions': <Object>[]};
      final client = await agent.connected();
      final opened = await client.reopenSession('s', cwd: 'C:/w');
      expect(opened.replayed, isTrue);
      expect(opened.raw['configOptions'], isEmpty);
      expect(agent.last('session/resume')['params']['replayFrom'], {
        'type': 'start',
      });
      expect(
        () => client.reopenSession('s', cwd: 'relative'),
        throwsA(isA<ArgumentError>()),
      );
    },
  );

  test('resuming returns what the agent answered, in v1 and v2', () async {
    for (final (initialize, replays) in [(v1, false), (v2, true)]) {
      final agent = Agent(initialize);
      agent.answers['session/resume'] = (_) => {
        'configOptions': [
          {'id': 'mode'},
        ],
      };
      final client = await agent.connected(
        maxVersion: initialize['protocolVersion'] as int,
      );
      final opened = await client.resumeSession(
        's',
        cwd: '/w',
        replayHistory: replays,
      );
      expect(opened.id, 's');
      expect(opened.replayed, replays);
      expect(opened.raw['configOptions'], [
        {'id': 'mode'},
      ]);
    }
  });

  test('attachments go with a prompt in the shapes ACP gives them', () async {
    final agent = Agent(v2);
    agent.answers['session/prompt'] = (_) => {'messageId': 'm'};
    final client = await agent.connected();
    await client.prompt(
      's',
      'look',
      attachments: [
        AcpAttachment.image('a.png', 'image/png', [1, 2, 3]),
        AcpAttachment.text('my notes.txt', 'hello', mimeType: 'text/plain'),
      ],
    );
    expect(agent.last('session/prompt')['params']['prompt'], [
      {'type': 'text', 'text': 'look'},
      {'type': 'image', 'mimeType': 'image/png', 'data': 'AQID'},
      {
        'type': 'resource',
        'resource': {
          'uri': 'attachment:my%20notes.txt',
          'text': 'hello',
          'mimeType': 'text/plain',
        },
      },
    ]);
    await client.prompt('s', '', attachments: [AcpAttachment.text('a', 'b')]);
    expect(agent.last('session/prompt')['params']['prompt'], hasLength(1));
    expect(() => client.prompt('s', ' '), throwsA(isA<ArgumentError>()));
  });

  test('a link goes to any agent, as a resource_link', () async {
    final agent = Agent({'protocolVersion': 1});
    agent.answers['session/prompt'] = (_) => {'stopReason': 'end_turn'};
    final client = await agent.connected();
    final link = AcpAttachment.link(
      'big.log',
      'file:///work/big.log',
      mimeType: 'text/plain',
      size: 5000000,
    );
    expect((link.isImage, link.isLink), (false, true));
    expect(AcpAttachment.text('a', 'b').isLink, isFalse);
    await client.prompt('s', 'look', attachments: [link]);
    expect(agent.last('session/prompt')['params']['prompt'], [
      {'type': 'text', 'text': 'look'},
      {
        'type': 'resource_link',
        'name': 'big.log',
        'uri': 'file:///work/big.log',
        'mimeType': 'text/plain',
        'size': 5000000,
      },
    ]);
    expect(() => AcpAttachment.link('x', ''), throwsA(isA<ArgumentError>()));
  });

  test('a v2 choice names its value type, which v1 leaves out', () async {
    final v2agent = Agent(v2);
    v2agent.answers['session/set_config_option'] = (_) => {
      'configOptions': <Object>[],
    };
    final client2 = await v2agent.connected();
    await client2.setConfigOption('s', 'model', 'b');
    expect(v2agent.last('session/set_config_option')['params'], {
      'sessionId': 's',
      'configId': 'model',
      'type': 'id',
      'value': 'b',
    });
    final v1agent = Agent({'protocolVersion': 1});
    v1agent.answers['session/set_config_option'] = (_) => {
      'configOptions': <Object>[],
    };
    final client1 = await v1agent.connected();
    await client1.setConfigOption('s', 'model', 'b');
    expect(v1agent.last('session/set_config_option')['params'], {
      'sessionId': 's',
      'configId': 'model',
      'value': 'b',
    });
  });

  test(
    'what an agent does not take is refused before anything is sent',
    () async {
      final agent = Agent({'protocolVersion': 1});
      final client = await agent.connected();
      await expectLater(
        client.prompt(
          's',
          'look',
          attachments: [
            AcpAttachment.image('a', 'image/png', [1]),
          ],
        ),
        throwsA(isA<AcpFailure>()),
      );
      await expectLater(
        client.prompt('s', 'look', attachments: [AcpAttachment.text('a', 'b')]),
        throwsA(isA<AcpFailure>()),
      );
      expect(
        agent.received.where(
          (message) => message['method'] == 'session/prompt',
        ),
        isEmpty,
      );
    },
  );
}
