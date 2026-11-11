// What the update recorder keeps, and which rendering each kind of update gets: every kind ACP names is taken, and a test of a host
// can fail when a recorded kind reaches the fallback, which shows nothing.
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

const _limits = ChatLimits();

AcpSessionUpdate _update(Map<String, Object?> raw) =>
    AcpSessionUpdate('s1', raw);

void main() {
  test('a shape keeps the protocol words and nothing anyone wrote', () {
    final shape = jsonEncode(
      shapeOf({
        'sessionUpdate': 'tool_call',
        'toolCallId': 'call-secret-1',
        'title': 'Read the secret file',
        'kind': 'read',
        'status': 'Not A Status secret',
        'rawInput': {
          'file_path': '/home/secret.txt',
          'limit': 20,
          'whole': true,
          'ratio': 0.5,
          'nothing': null,
        },
        'content': [
          {
            'type': 'content',
            'content': {'type': 'text', 'text': 'secret one'},
          },
          {
            'type': 'content',
            'content': {'type': 'text', 'text': 'secret two'},
          },
          {
            'type': 'diff',
            'path': 'a',
            'oldText': 'secret',
            'newText': 'secret',
          },
        ],
        'secret key with spaces': 'x',
      }),
    );
    expect(shape, isNot(contains('secret')));
    expect(shape, contains('"kind":"read"'));
    expect(
      shape,
      contains('"status":"string"'),
      reason: 'a word under a protocol key, but not one the protocol could use',
    );
    expect(shape, contains('"limit":"int"'));
    expect(shape, contains('"whole":"bool"'));
    expect(shape, contains('"ratio":"double"'));
    expect(shape, contains('"nothing":"null"'));
    expect(shape, contains('"key":"string"'));
    final content = (jsonDecode(shape) as Map)['content'] as List;
    expect(content, hasLength(2), reason: 'each distinct element shape once');
  });

  test(
    'a long list keeps at most the shapes the limit says, and a deep one ends',
    () {
      final many = [
        for (var i = 0; i < 50; i++) {'field$i': i},
      ];
      expect(shapeOf(many) as List, hasLength(8));
      expect(
        shapeOf(many, limits: const ChatLimits(listShapes: 3)) as List,
        hasLength(3),
      );
      expect(
        shapeOf(many, limits: const ChatLimits(listShapes: 0)) as List,
        isEmpty,
      );
      Object? deep = 'bottom';
      for (var i = 0; i < 20; i++) {
        deep = {'next': deep};
      }
      expect(jsonEncode(shapeOf(deep)), contains('…'));
    },
  );

  test(
    'each kind has a rendering of its own, and anything else reaches the fallback',
    () {
      const content = {'type': 'text', 'text': 'hi'};
      final expected = {
        'user_message': Rendering.message,
        'user_message_chunk': Rendering.message,
        'agent_message': Rendering.message,
        'agent_message_chunk': Rendering.message,
        'agent_thought': Rendering.thought,
        'agent_thought_chunk': Rendering.thought,
        'tool_call': Rendering.toolCard,
        'tool_call_update': Rendering.toolCard,
        'terminal_update': Rendering.terminal,
        'plan': Rendering.plan,
        'available_commands_update': Rendering.commands,
        'usage_update': Rendering.usage,
        'config_option_update': Rendering.choices,
        'current_mode_update': Rendering.choices,
        'session_info_update': Rendering.title,
        'state_update': Rendering.status,
        'something_new': Rendering.unshown,
      };
      expected.forEach((kind, rendering) {
        expect(
          renderingOf(
            _update({'sessionUpdate': kind, 'content': content}),
            _limits,
          ),
          rendering,
          reason: kind,
        );
      });
      expect(
        renderingOf(
          _update({
            'sessionUpdate': 'agent_message_chunk',
            'content': {'type': 'image', 'data': '', 'mimeType': 'image/png'},
          }),
          _limits,
        ),
        Rendering.unshown,
        reason: 'an image from the agent is not drawn',
      );
    },
  );

  test(
    'every kind ACP v2 names has a rendering, in the shapes its schema gives',
    () {
      // Each `sessionUpdate` value of SessionUpdate in `@agentclientprotocol/sdk` schema/v2/schema.json, with its required fields.
      const content = {'type': 'text', 'text': 'hi'};
      final v2 = <Map<String, Object?>>[
        {
          'sessionUpdate': 'user_message_chunk',
          'messageId': 'm',
          'content': content,
        },
        {'sessionUpdate': 'user_message', 'messageId': 'm'},
        {
          'sessionUpdate': 'agent_message_chunk',
          'messageId': 'm',
          'content': content,
        },
        {'sessionUpdate': 'agent_message', 'messageId': 'm'},
        {
          'sessionUpdate': 'agent_thought_chunk',
          'messageId': 'm',
          'content': content,
        },
        {'sessionUpdate': 'agent_thought', 'messageId': 'm'},
        {'sessionUpdate': 'state_update', 'state': 'idle'},
        {
          'sessionUpdate': 'tool_call_content_chunk',
          'toolCallId': 't',
          'content': {'type': 'content', 'content': content},
        },
        {'sessionUpdate': 'tool_call_update', 'toolCallId': 't'},
        {'sessionUpdate': 'terminal_update', 'terminalId': 'x'},
        {
          'sessionUpdate': 'terminal_output_chunk',
          'terminalId': 'x',
          'data': 'aGk=',
        },
        {
          'sessionUpdate': 'plan_update',
          'plan': {'type': 'items', 'planId': 'p', 'entries': <Object>[]},
        },
        {
          'sessionUpdate': 'available_commands_update',
          'availableCommands': <Object>[],
        },
        {'sessionUpdate': 'config_option_update', 'configOptions': <Object>[]},
        {'sessionUpdate': 'session_info_update'},
        {'sessionUpdate': 'usage_update', 'used': 1, 'size': 2},
        {'sessionUpdate': 'notice', 'severity': 'info', 'title': 'hi'},
        {
          'sessionUpdate': 'compaction_update',
          'compactionId': 'c',
          'status': 'in_progress',
        },
        {
          'sessionUpdate': 'compaction_summary_chunk',
          'compactionId': 'c',
          'content': content,
        },
      ];
      for (final raw in v2) {
        expect(
          renderingOf(_update(raw), _limits),
          isNot(Rendering.unshown),
          reason: '${raw['sessionUpdate']}',
        );
      }
      expect(renderingOf(_update(v2[7]), _limits), Rendering.toolCard);
      expect(renderingOf(_update(v2[11]), _limits), Rendering.plan);
      expect(renderingOf(_update(v2[16]), _limits), Rendering.notice);
      expect(renderingOf(_update(v2[17]), _limits), Rendering.compaction);
      expect(renderingOf(_update(v2[18]), _limits), Rendering.compaction);
      expect(
        renderingOf(
          _update({
            'sessionUpdate': 'user_message_chunk',
            'content': {
              'type': 'resource',
              'resource': {'uri': 'attachment:a.txt', 'text': 'a'},
            },
          }),
          _limits,
        ),
        Rendering.message,
        reason: 'a replayed attachment is said in the conversation',
      );
    },
  );

  test('each distinct update is written once, and counted every time', () {
    final lines = <String>[];
    final recorder = UpdateRecorder(write: lines.add);
    for (final said in ['one', 'two', 'three']) {
      final update = _update({
        'sessionUpdate': 'agent_message_chunk',
        'content': {'type': 'text', 'text': said},
      });
      recorder.record(update, renderingOf(update, _limits));
    }
    final odd = _update({'sessionUpdate': 'Something Odd', 'x': 1});
    recorder.record(odd, renderingOf(odd, _limits));
    expect(lines, hasLength(2));
    expect(lines.join(), isNot(contains('one')));
    expect(recorder.seen.first.count, 3);
    expect(
      recorder.unshown,
      {'other'},
      reason: 'a kind that is not a protocol word is not kept either',
    );
  });

  test(
    'a conversation records what it applies, never what was said, and every update of a full session has a rendering',
    () {
      final recorder = UpdateRecorder();
      final session = ChatSession(id: 'x', sessionId: 's1', recorder: recorder);
      feed(session, text('agent_message_chunk', 'a secret message'));
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 't',
        'title': 'a secret file',
      });
      feed(session, {'sessionUpdate': 'a_kind_nobody_made_yet'});
      expect(
        jsonEncode(recorder.seen.map((s) => s.toJson()).toList()),
        isNot(contains('secret')),
      );
      expect(
        recorder.unshown,
        {'a_kind_nobody_made_yet'},
        reason: 'what reaches nothing is named, for a host\'s test to fail on',
      );
    },
  );
}
