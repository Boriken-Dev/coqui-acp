// A client that attaches late reads what the agent said as updates and reaches the same conversation.
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

void main() {
  test(
    'what a conversation replays, another builds again: messages, tools with diffs and terminals, plan, commands, usage, title',
    () {
      final (:store, :session, :agent) = started();
      session.attachClient('desk');
      feed(session, state('running'));
      feed(session, {
        'sessionUpdate': 'user_message',
        'messageId': 'u1',
        'content': {'type': 'text', 'text': 'Fix the test'},
        '_meta': {chatClientMetaKey: 'desk'},
      });
      feed(session, text('agent_thought', 'Looking at calc.js', id: 'th'));
      feed(session, text('agent_message_chunk', 'It subtracts.', id: 'a1'));
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 'e',
        'title': 'Edit calc.js',
        'kind': 'edit',
        'status': 'completed',
        'locations': [
          {'path': 'calc.js', 'line': 2},
        ],
        'content': [
          {
            'type': 'diff',
            'path': 'calc.js',
            'oldText': 'a - b',
            'newText': 'a + b',
          },
          {'type': 'diff', 'path': 'new.js', 'newText': 'x'},
          {
            'patch': {'text': '+++ b/p.js\n'},
            'changes': [
              {'path': 'p.js'},
            ],
          },
        ],
      });
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 'r',
        'title': 'Run the tests',
        'kind': 'execute',
        'status': 'in_progress',
        'content': [
          {'type': 'terminal', 'terminalId': 'term'},
          {
            'type': 'content',
            'content': {'type': 'text', 'text': 'running'},
          },
        ],
      });
      feed(session, {
        'sessionUpdate': 'terminal_update',
        'terminalId': 'term',
        'command': 'dart test',
        'output': {'data': base64.encode(utf8.encode('1 failed\n'))},
        'exitStatus': {'exitCode': 1},
      });
      feed(session, {
        'sessionUpdate': 'compaction_update',
        'compactionId': 'c',
        'status': 'completed',
        'summary': [
          {'type': 'text', 'text': 'Short.'},
        ],
      });
      feed(session, {
        'sessionUpdate': 'plan',
        'entries': [
          {'content': 'Read', 'status': 'completed'},
          {'content': 'Fix', 'status': 'in_progress'},
        ],
      });
      feed(session, {
        'sessionUpdate': 'available_commands_update',
        'availableCommands': [
          {'name': 'review', 'description': 'Review it'},
        ],
      });
      feed(session, {
        'sessionUpdate': 'usage_update',
        'used': 5,
        'size': 10,
        'cost': {'amount': 0.25, 'currency': 'USD'},
      });
      feed(session, {
        'sessionUpdate': 'config_option_update',
        'configOptions': [
          {
            'id': 'model',
            'name': 'Model',
            'category': 'model',
            'currentValue': 'a',
            'options': [
              {'value': 'a', 'name': 'A'},
            ],
          },
        ],
      });
      feed(session, {
        'sessionUpdate': 'session_info_update',
        'title': 'Fix the test',
      });

      final other = ChatSession(id: 'phone', sessionId: 's1');
      for (final update in session.replay()) {
        // Through JSON, as a wire would carry it.
        other.apply(
          AcpSessionUpdate(
            update.sessionId,
            jsonDecode(jsonEncode(update.raw)) as Map<String, dynamic>,
          ),
        );
      }

      expect(
        other.messages.map((m) => (m.role, m.text, m.messageId, m.sentBy)),
        [
          for (final m in session.messages)
            (m.role, m.text, m.messageId, m.sentBy),
        ],
      );
      final e = other.toolEntries['e']!.tool!,
          original = session.toolEntries['e']!.tool!;
      expect(
        (e.title, e.kind, e.status),
        (original.title, original.kind, original.status),
      );
      expect(e.locations, original.locations);
      expect(
        e.blocks.whereType<ToolDiff>().map(
          (d) => (d.path, d.oldText, d.newText),
        ),
        [('calc.js', 'a - b', 'a + b'), ('new.js', null, 'x')],
      );
      expect(e.blocks.whereType<ToolPatch>().single.paths, ['p.js']);
      expect(other.toolEntries['r']!.tool!.status, ToolStatus.running);
      final terminal = other.terminals['term']!;
      expect(
        (terminal.command, terminal.output, terminal.exitCode),
        ('dart test', '1 failed\n', 1),
      );
      expect(
        other.messages
            .firstWhere((m) => m.compaction != null)
            .compaction!
            .summary,
        'Short.',
      );
      expect(other.plan.map((p) => (p.content, p.status)), [
        ('Read', PlanStatus.done),
        ('Fix', PlanStatus.running),
      ]);
      expect(other.commands.single.name, 'review');
      expect(
        (
          other.usage.used,
          other.usage.size,
          other.usage.cost,
          other.usage.currency,
        ),
        (5, 10, 0.25, 'USD'),
      );
      expect(other.options.single.current, 'a');
      expect(other.title, 'Fix the test');
      expect(other.status, ChatStatus.working);
      store.dispose();
    },
  );

  test(
    'a stopped tool replays as cancelled, and a new conversation replays nothing',
    () async {
      final (:store, :session, :agent) = started();
      expect(session.replay().map((u) => u.kind), [
        'session_info_update',
      ], reason: 'only its name');
      store.askPermission(request(1), agent);
      await agent.drop();
      await settle();
      expect(session.toolEntries['t1']!.tool!.status, ToolStatus.stopped);
      expect(
        session.replay().firstWhere((u) => u.kind == 'tool_call').raw['status'],
        'cancelled',
      );
      store.dispose();
    },
  );

  test(
    'what waits for the person is read from the conversation, with who answered what already settled',
    () {
      final (:store, :session, :agent) = started();
      session
        ..attachClient('desk')
        ..attachClient('phone');
      store.askPermission(request(1), agent);
      store.askPermission(request(2), agent);
      session.answerPermission(allow: true, by: 'desk');
      expect(session.pendingPermissions.map((q) => q.toolCallId), ['t2']);
      expect(
        session.settledPermissions.map((q) => (q.toolCallId, q.outcome!.by)),
        [('t1', 'desk')],
      );
      expect(session.clients.map((c) => c.id), ['desk', 'phone']);
      store.dispose();
    },
  );
}
