// A client that attaches late reads what the agent said as updates and reaches the same conversation.
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat/experimental.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

void main() {
  _more();
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
      other.applyReplay([
        for (final update in session.replay())
          // Through JSON, as a wire would carry it.
          AcpSessionUpdate(
            update.sessionId,
            jsonDecode(jsonEncode(update.raw)) as Map<String, dynamic>,
          ),
      ]);

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

void _more() {
  test(
    'a replay keeps the turns with their times and how they ended, the times of entries, the naming and a v1 mode',
    () {
      var now = DateTime.utc(2026, 11, 12, 9);
      final store = ChatStore(clock: () => now);
      final session = store.add(title: 'Fix it', agent: 'A', sessionId: 's1');
      session.applyOpened(
        AcpOpenedSession('s1', {
          'modes': {
            'currentModeId': 'auto',
            'availableModes': [
              {'id': 'manual', 'name': 'Manual'},
              {'id': 'auto', 'name': 'Auto'},
            ],
          },
        }),
      );
      session.rename('Mine');
      feed(session, state('running'));
      now = now.add(const Duration(seconds: 2));
      feed(session, text('agent_message', 'First', id: 'a1'));
      now = now.add(const Duration(seconds: 8));
      feed(session, state('idle', reason: 'end_turn'));
      now = now.add(const Duration(minutes: 5));
      feed(session, state('running'));
      now = now.add(const Duration(seconds: 3));
      feed(session, state('idle', reason: 'cancelled'));

      var later = DateTime.utc(2026, 11, 13);
      final other = ChatSession(
        id: 'phone',
        sessionId: 's1',
        clock: () => later,
      );
      final told = <ChatEvent>[];
      other.events.listen(told.add);
      other.applyReplay(session.replay());

      expect(other.turns.map((t) => t.outcome), [
        TurnOutcome.completed,
        TurnOutcome.cancelled,
      ]);
      expect(other.turns.map((t) => t.duration(later)), [
        const Duration(seconds: 10),
        const Duration(seconds: 3),
      ]);
      expect(other.turns.first.startedAt, DateTime.utc(2026, 11, 12, 9));
      expect(
        other.messages.firstWhere((m) => m.text == 'First').at,
        DateTime.utc(2026, 11, 12, 9, 0, 2),
      );
      expect(
        other.messages.firstWhere((m) => m.text == 'First').turn,
        other.turns.first.id,
      );
      expect(other.title, 'Mine');
      expect(other.agentNames, isFalse);
      other.applyReplay([
        AcpSessionUpdate('s1', {
          'sessionUpdate': 'session_info_update',
          'title': 'Agent says',
        }),
      ]);
      expect(other.title, 'Mine', reason: 'the person had named it');
      expect(other.options.single.isMode, isTrue);
      expect(other.mode, 'auto');
      later = later.add(const Duration(hours: 1));
      return Future<void>.delayed(const Duration(milliseconds: 10)).then((_) {
        expect(
          told.whereType<AttentionEvent>(),
          isEmpty,
          reason: 'history is not news',
        );
        expect(told.whereType<TurnEvent>(), isEmpty);
        store.dispose();
      });
    },
  );

  test(
    'a time an agent puts in an update is not believed outside a replay',
    () {
      final session = ChatSession(
        id: 'x',
        sessionId: 's1',
        clock: () => DateTime.utc(2026, 1, 1),
      );
      feed(session, {
        'sessionUpdate': 'agent_message',
        'messageId': 'm',
        'content': {'type': 'text', 'text': 'hi'},
        '_meta': {'dev.boriken.coqui.acp/at': '1999-01-01T00:00:00Z'},
      });
      expect(session.messages.single.at, DateTime.utc(2026, 1, 1));
    },
  );

  test('a sender an update names is one clean line', () {
    final session = ChatSession(
      id: 'x',
      sessionId: 's1',
      limits: const ChatLimits(shortText: 6),
    );
    feed(session, {
      'sessionUpdate': 'user_message',
      'messageId': 'm',
      'content': {'type': 'text', 'text': 'hi'},
      '_meta': {chatClientMetaKey: 'tv\nroom-with-a-long-name'},
    });
    expect(session.messages.single.sentBy, 'tv roo…');
  });

  test('settling elsewhere needs the client to be attached', () {
    final (:store, :session, :agent) = started();
    store.askPermission(request(1), agent);
    expect(
      () => session.settleElsewhere(
        session.pendingPermissions.single,
        by: 'ghost',
        optionId: 'reject',
      ),
      throwsStateError,
    );
    expect(session.pendingPermissions, hasLength(1));
    store.dispose();
  });
}
