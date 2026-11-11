// What a session starts with besides its directory: MCP servers, a system prompt, more directories and `_meta`, each in the
// shape the version spoken takes, and refused before anything is sent where the agent does not say it takes it.
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

  Future<void> send(String text) async {
    final message = jsonDecode(text) as Map<String, dynamic>;
    received.add(message);
    final method = message['method'] as String?;
    if (method == null || message['id'] == null) return;
    final result = switch (method) {
      'initialize' => initialize,
      'session/new' => {'sessionId': 's1'},
      _ => <String, Object?>{},
    };
    toClient.add(
      jsonEncode({'jsonrpc': '2.0', 'id': message['id'], 'result': result}),
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
  bool sent(String method) =>
      received.any((message) => message['method'] == method);
}

// What Claude's adapter 0.99.1 says in v1.
final Map<String, Object?> claudeV1 = {
  'protocolVersion': 1,
  'agentCapabilities': {
    'mcpCapabilities': {'http': true, 'sse': true},
    'loadSession': true,
    'sessionCapabilities': {
      'resume': <String, Object?>{},
      'additionalDirectories': <String, Object?>{},
      'systemPrompt': {
        'mode': {'override': <String, Object?>{}},
      },
    },
  },
};

// And in v2.
final Map<String, Object?> claudeV2 = {
  'protocolVersion': 2,
  'capabilities': {
    'session': {
      'mcp': {'stdio': <String, Object?>{}, 'http': <String, Object?>{}},
      'additionalDirectories': <String, Object?>{},
      'systemPrompt': {
        'mode': {'override': <String, Object?>{}},
      },
    },
  },
};

// What pi-acp 0.0.34 says: no HTTP or SSE, nothing else.
final Map<String, Object?> piV1 = {
  'protocolVersion': 1,
  'agentCapabilities': {
    'loadSession': true,
    'mcpCapabilities': {'http': false, 'sse': false},
  },
};

const stdio = AcpMcpStdio(
  name: 'marker',
  command: '/usr/bin/node',
  args: ['server.mjs'],
  env: {'TOKEN': 'value'},
);
const http = AcpMcpHttp(
  name: 'web',
  url: 'https://example.test/mcp',
  headers: {'Authorization': 'Bearer x'},
);
const sse = AcpMcpHttp(
  name: 'events',
  url: 'https://example.test/sse',
  sse: true,
);

void main() {
  test(
    'v1 sends stdio with no type, HTTP and SSE typed, name and value pairs, and the rest',
    () async {
      final agent = Agent(claudeV1);
      final client = await agent.connected(maxVersion: 1);
      expect(client.takesMcp(AcpMcpTransport.stdio), isTrue);
      expect(client.takesMcp(AcpMcpTransport.http), isTrue);
      expect(client.takesMcp(AcpMcpTransport.sse), isTrue);
      expect(client.takesSystemPrompt, isTrue);
      expect(client.takesAdditionalDirectories, isTrue);
      await client.openSession(
        cwd: '/w',
        setup: const AcpSessionSetup(
          mcpServers: [stdio, http, sse],
          systemPrompt: 'Say the marker.',
          additionalDirectories: ['/skills'],
          meta: {
            'claudeCode': {
              'options': {'settingSources': <String>[]},
            },
          },
        ),
      );
      final params = agent.last('session/new')['params'] as Map;
      expect(params['mcpServers'], [
        {
          'name': 'marker',
          'command': '/usr/bin/node',
          'args': ['server.mjs'],
          'env': [
            {'name': 'TOKEN', 'value': 'value'},
          ],
        },
        {
          'type': 'http',
          'name': 'web',
          'url': 'https://example.test/mcp',
          'headers': [
            {'name': 'Authorization', 'value': 'Bearer x'},
          ],
        },
        {
          'type': 'sse',
          'name': 'events',
          'url': 'https://example.test/sse',
          'headers': <Object>[],
        },
      ]);
      expect(params['systemPrompt'], {
        'mode': 'append',
        'content': [
          {'type': 'text', 'text': 'Say the marker.'},
        ],
      });
      expect(params['additionalDirectories'], ['/skills']);
      expect(params['_meta'], {
        'claudeCode': {
          'options': {'settingSources': <Object>[]},
        },
      });
    },
  );

  test('v2 types every server and refuses SSE, which it has not', () async {
    final agent = Agent(claudeV2);
    final client = await agent.connected();
    expect(client.takesMcp(AcpMcpTransport.sse), isFalse);
    await client.openSession(
      cwd: '/w',
      setup: const AcpSessionSetup(mcpServers: [stdio, http]),
    );
    final servers =
        (agent.last('session/new')['params'] as Map)['mcpServers'] as List;
    expect(
      [for (final server in servers) (server as Map)['type']],
      ['stdio', 'http'],
    );
    await expectLater(
      client.openSession(
        cwd: '/w',
        setup: const AcpSessionSetup(mcpServers: [sse]),
      ),
      throwsA(isA<AcpFailure>()),
    );
  });

  test(
    'what the agent does not say it takes is refused before anything is sent',
    () async {
      final agent = Agent(piV1);
      final client = await agent.connected(maxVersion: 1);
      expect(client.takesMcp(AcpMcpTransport.stdio), isTrue);
      expect(client.takesMcp(AcpMcpTransport.http), isFalse);
      expect(client.takesSystemPrompt, isFalse);
      expect(client.takesAdditionalDirectories, isFalse);
      for (final setup in const [
        AcpSessionSetup(mcpServers: [http]),
        AcpSessionSetup(systemPrompt: 'x'),
        AcpSessionSetup(additionalDirectories: ['/d']),
      ]) {
        await expectLater(
          client.openSession(cwd: '/w', setup: setup),
          throwsA(isA<AcpFailure>()),
        );
      }
      expect(agent.sent('session/new'), isFalse);
    },
  );

  test(
    'a relative command or directory, or two servers of one name, are refused',
    () async {
      final agent = Agent(claudeV1);
      final client = await agent.connected(maxVersion: 1);
      for (final setup in const [
        AcpSessionSetup(
          mcpServers: [AcpMcpStdio(name: 'x', command: 'node')],
        ),
        AcpSessionSetup(additionalDirectories: ['skills']),
        AcpSessionSetup(mcpServers: [stdio, stdio]),
      ]) {
        expect(
          () => client.openSession(cwd: '/w', setup: setup),
          throwsArgumentError,
        );
      }
      expect(agent.sent('session/new'), isFalse);
    },
  );

  test('opening an earlier session sends the same setup', () async {
    final agent = Agent(claudeV1);
    final client = await agent.connected(maxVersion: 1);
    const setup = AcpSessionSetup(
      mcpServers: [stdio],
      systemPrompt: 'Say the marker.',
    );
    await client.reopenSession('s', cwd: '/w', setup: setup);
    final load = agent.last('session/resume')['params'] as Map;
    expect((load['mcpServers'] as List).single, containsPair('name', 'marker'));
    expect(load['systemPrompt'], isNotNull);
    await client.resumeSession('s', cwd: '/w', setup: setup);
    final resume = agent.last('session/resume')['params'] as Map;
    expect(
      (resume['mcpServers'] as List).single,
      containsPair('name', 'marker'),
    );

    final v2 = Agent(claudeV2);
    final two = await v2.connected();
    await two.reopenSession('s', cwd: '/w', setup: setup);
    final resumed = v2.last('session/resume')['params'] as Map;
    expect(
      (resumed['mcpServers'] as List).single,
      containsPair('type', 'stdio'),
    );
    expect(resumed['replayFrom'], {'type': 'start'});
  });

  test('a server never shows its environment or headers', () {
    expect('$stdio', isNot(contains('value')));
    expect('$http', isNot(contains('Bearer')));
  });
}
