// What a conversation builds from the updates an agent sends: messages, tool calls with diffs and terminals, plans, notices,
// compactions, commands, options and usage, and the bounds on each.
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

ChatSession fresh({ChatLimits limits = const ChatLimits(), String? key}) =>
    ChatSession(
      id: 'local-1',
      sessionId: 's1',
      limits: limits,
      hostNoticesKey: key,
    );

void main() {
  _grouping();
  group('messages', () {
    test(
      'a v1 chunk continues the last message of its role, and a new role starts another',
      () {
        final session = fresh();
        feed(session, text('user_message_chunk', 'Say '));
        feed(session, text('user_message_chunk', 'hello'));
        feed(session, text('agent_message_chunk', 'Hel'));
        feed(session, text('agent_message_chunk', 'lo.'));
        feed(session, text('agent_thought_chunk', 'Thinking'));
        expect(session.messages.map((m) => (m.role, m.text)), [
          (MessageRole.user, 'Say hello'),
          (MessageRole.agent, 'Hello.'),
          (MessageRole.thought, 'Thinking'),
        ]);
      },
    );

    test(
      'a v2 message is found by its id, a chunk adds to it and a whole message replaces it',
      () {
        final session = fresh();
        feed(session, text('agent_message_chunk', 'One ', id: 'a'));
        feed(session, text('agent_message_chunk', 'two', id: 'a'));
        feed(session, text('agent_message_chunk', 'Other', id: 'b'));
        feed(session, text('agent_message', 'One two three', id: 'a'));
        expect(session.messages.map((m) => (m.messageId, m.text)), [
          ('a', 'One two three'),
          ('b', 'Other'),
        ]);
      },
    );

    test(
      'an agent_thought patch without content keeps the thought, and one with content replaces it',
      () {
        final session = fresh();
        feed(session, text('agent_thought_chunk', 'Looking.', id: 'th'));
        feed(session, {
          'sessionUpdate': 'agent_thought',
          'messageId': 'th',
          '_meta': {'done': true},
        });
        expect(session.messages.single.text, 'Looking.');
        feed(session, {
          'sessionUpdate': 'agent_thought',
          'messageId': 'th',
          'content': [
            {'type': 'text', 'text': 'Decided.'},
          ],
        });
        expect(session.messages.single.text, 'Decided.');
      },
    );

    test('an image chunk from the agent is not a message', () {
      final session = fresh();
      feed(session, {
        'sessionUpdate': 'agent_message_chunk',
        'content': {'type': 'image', 'data': '', 'mimeType': 'image/png'},
      });
      expect(session.messages, isEmpty);
    });

    test(
      'a replayed message says what went with it, and a live one says it once',
      () async {
        final (:store, :session, :agent) = started();
        feed(session, {
          'sessionUpdate': 'user_message',
          'messageId': 'old1',
          'content': [
            {'type': 'text', 'text': 'Do what the notes say.'},
            {
              'type': 'resource',
              'resource': {
                'uri': 'attachment:notes.txt',
                'text': '1. Add a line.',
              },
            },
          ],
        });
        expect(session.messages.map((m) => (m.role, m.text)), [
          (MessageRole.user, 'Do what the notes say.'),
          (MessageRole.notice, 'Sent with this: notes.txt'),
        ]);

        await session.send(
          'And this one.',
          attachments: [AcpAttachment.text('plan.md', '# plan')],
        );
        // The agent echoes the message with its attachments: named once, not again.
        feed(session, {
          'sessionUpdate': 'user_message',
          'messageId': 'm1',
          'content': [
            {'type': 'text', 'text': 'And this one.'},
            {
              'type': 'resource',
              'resource': {'uri': 'attachment:plan.md', 'text': '# plan'},
            },
          ],
        });
        expect(
          session.messages.where((m) => m.text == 'Sent with this: plan.md'),
          hasLength(1),
        );
        expect(
          session.messages.where((m) => m.text == 'And this one.'),
          hasLength(1),
        );
        store.dispose();
      },
    );

    test(
      'a message over the limit is cut, and the oldest entries go past the most kept',
      () {
        final session = fresh(
          limits: const ChatLimits(messageText: 10, entries: 3),
        );
        feed(session, text('agent_message', 'x' * 50, id: 'big'));
        expect(session.messages.single.text, '${'x' * 10}\n…');
        for (final id in ['t1', 't2', 't3', 't4']) {
          feed(session, {
            'sessionUpdate': 'tool_call',
            'toolCallId': id,
            'title': id,
          });
        }
        expect(session.messages, hasLength(3));
        expect(session.toolEntries.keys, [
          't2',
          't3',
          't4',
        ], reason: 'a trimmed tool line is forgotten too');
      },
    );

    test(
      'what an agent names is one line a person can read, and cut at the limit',
      () {
        final session = fresh(limits: const ChatLimits(shortText: 8));
        feed(session, {
          'sessionUpdate': 'tool_call',
          'toolCallId': 't',
          'title': 'Run${String.fromCharCode(0x202e)}\ntests please',
        });
        expect(session.toolEntries['t']!.tool!.title, 'Run  tes…');
      },
    );
  });

  group('tool calls', () {
    test('are built from updates, a field left out keeping its value', () {
      final session = fresh();
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 't',
        'title': 'Edit calc.js',
        'kind': 'edit',
        'status': 'pending',
        'locations': [
          {'path': 'calc.js', 'line': 3},
          {'path': 'b.js'},
        ],
      });
      feed(session, {
        'sessionUpdate': 'tool_call_update',
        'toolCallId': 't',
        'status': 'in_progress',
      });
      final tool = session.toolEntries['t']!.tool!;
      expect(
        (tool.title, tool.kind, tool.status),
        ('Edit calc.js', 'edit', ToolStatus.running),
      );
      expect(tool.locations, ['calc.js:3', 'b.js']);
      expect(tool.isOpen, isTrue);
      feed(session, {
        'sessionUpdate': 'tool_call_update',
        'toolCallId': 't',
        'status': 'completed',
      });
      expect(tool.isOpen, isFalse);
      expect(
        session.messages,
        hasLength(1),
        reason: 'one line however many updates',
      );
    });

    test(
      'carry a diff, with no old text for a new file, and a patch with the files it changes',
      () {
        final session = fresh();
        feed(session, {
          'sessionUpdate': 'tool_call',
          'toolCallId': 't',
          'content': [
            {'type': 'diff', 'path': 'calc.js', 'oldText': 'a', 'newText': 'b'},
            {'type': 'diff', 'path': 'new.js', 'newText': 'c'},
            {
              'patch': {
                'text':
                    '--- a/x.js\n+++ b/x.js\n@@\n-1\n+2\n--- /dev/null\n+++ /dev/null\n',
              },
            },
          ],
        });
        final blocks = session.toolEntries['t']!.tool!.blocks;
        expect(blocks[0], isA<ToolDiff>().having((d) => d.oldText, 'old', 'a'));
        expect(
          blocks[1],
          isA<ToolDiff>()
              .having((d) => d.oldText, 'old', isNull)
              .having((d) => d.path, 'path', 'new.js'),
        );
        expect((blocks[2] as ToolPatch).paths, ['x.js']);
      },
    );

    test(
      'an empty content list does not forget what came before, and a content chunk adds one item',
      () {
        final session = fresh();
        feed(session, {
          'sessionUpdate': 'tool_call_update',
          'toolCallId': 't',
          'title': 'Run the tests',
          'kind': 'execute',
          'status': 'in_progress',
          'content': [
            {
              'type': 'content',
              'content': {'type': 'text', 'text': 'first'},
            },
          ],
        });
        feed(session, {
          'sessionUpdate': 'tool_call_update',
          'toolCallId': 't',
          'content': <Object>[],
        });
        feed(session, {
          'sessionUpdate': 'tool_call_content_chunk',
          'toolCallId': 't',
          'content': {
            'type': 'content',
            'content': {'type': 'text', 'text': 'second'},
          },
        });
        feed(session, {
          'sessionUpdate': 'tool_call_content_chunk',
          'toolCallId': 't',
          'content': {'type': 'terminal', 'terminalId': 'term'},
        });
        final tool = session.toolEntries['t']!.tool!;
        expect(tool.blocks.whereType<ToolText>().map((b) => b.text), [
          'first',
          'second',
        ]);
        expect(tool.blocks.last, isA<ToolTerminal>());
      },
    );

    test('keep the most blocks the limit says, the oldest going first', () {
      final session = fresh(limits: const ChatLimits(toolBlocks: 2));
      for (final word in ['a', 'b', 'c']) {
        feed(session, {
          'sessionUpdate': 'tool_call_content_chunk',
          'toolCallId': 't',
          'content': {
            'type': 'content',
            'content': {'type': 'text', 'text': word},
          },
        });
      }
      expect(
        session.toolEntries['t']!.tool!.blocks.whereType<ToolText>().map(
          (b) => b.text,
        ),
        ['b', 'c'],
      );
    });

    test(
      'a command shows its output as v2 sends it, base64, with its end kept and its exit code',
      () {
        final session = fresh(limits: const ChatLimits(terminalOutput: 6));
        String b64(String s) => base64.encode(utf8.encode(s));
        feed(session, {
          'sessionUpdate': 'terminal_update',
          'terminalId': 'x',
          'command': 'dart test',
          'output': {'data': b64('abc')},
        });
        feed(session, {
          'sessionUpdate': 'terminal_output_chunk',
          'terminalId': 'x',
          'data': b64('defgh'),
        });
        feed(session, {
          'sessionUpdate': 'terminal_update',
          'terminalId': 'x',
          'exitStatus': {'exitCode': 1},
        });
        final terminal = session.terminals['x']!;
        expect(
          (terminal.command, terminal.output, terminal.exitCode),
          ('dart test', 'cdefgh', 1),
        );
        feed(session, {
          'sessionUpdate': 'terminal_output_chunk',
          'terminalId': 'y',
          'data': 'bad!',
        });
        expect(
          session.terminals['y']!.output,
          'bad!',
          reason: 'what is not base64 is shown as it came',
        );
      },
    );

    test(
      'keep the output of at most the most commands the limit says, the oldest going first',
      () {
        final session = fresh(limits: const ChatLimits(terminals: 2));
        for (final id in ['a', 'b', 'c']) {
          feed(session, {
            'sessionUpdate': 'terminal_update',
            'terminalId': id,
            'command': id,
          });
        }
        expect(session.terminals.keys, ['b', 'c']);
      },
    );
  });

  group('plans', () {
    test(
      'plan_update replaces the plan, a cancelled entry is not still to do, and a plan type not known leaves it',
      () {
        final session = fresh();
        feed(session, {
          'sessionUpdate': 'plan_update',
          'plan': {
            'type': 'items',
            'planId': 'p1',
            'entries': [
              {
                'content': 'Read the code',
                'priority': 'high',
                'status': 'completed',
              },
              {
                'content': 'Change it',
                'priority': 'high',
                'status': 'in_progress',
              },
              {
                'content': 'Try another way',
                'priority': 'low',
                'status': 'cancelled',
              },
              {'content': 'Ship', 'status': 'pending'},
            ],
          },
        });
        expect(session.plan.map((e) => e.status), [
          PlanStatus.done,
          PlanStatus.running,
          PlanStatus.cancelled,
          PlanStatus.pending,
        ]);
        feed(session, {
          'sessionUpdate': 'plan_update',
          'plan': {'type': 'markdown', 'planId': 'p2', 'text': '# later'},
        });
        expect(session.plan, hasLength(4));
        feed(session, {
          'sessionUpdate': 'plan_update',
          'plan': {
            'type': 'items',
            'planId': 'p1',
            'entries': [
              {'content': 'Read the code', 'status': 'completed'},
              {'content': 'Change it', 'status': 'completed'},
            ],
          },
        });
        expect(session.plan.map((e) => e.content), [
          'Read the code',
          'Change it',
        ]);
      },
    );

    test('a v1 plan is its entries, and is cut at the limit', () {
      final session = fresh(limits: const ChatLimits(planEntries: 2));
      feed(session, {
        'sessionUpdate': 'plan',
        'entries': [
          for (final c in ['a', 'b', 'c']) {'content': c, 'status': 'pending'},
        ],
      });
      expect(session.plan.map((e) => e.content), ['a', 'b']);
    });
  });

  group('notices and compactions', () {
    test("a notice shows as the agent's word, with its severity", () {
      final session = fresh();
      feed(session, {
        'sessionUpdate': 'notice',
        'severity': 'warning',
        'title': 'Rate limit close',
        'description': 'About 10 requests are left this hour.',
      });
      feed(session, {
        'sessionUpdate': 'notice',
        'severity': 'info',
        'title': 'Model switched',
      });
      expect(session.messages.map((m) => (m.role, m.text)), [
        (
          MessageRole.notice,
          'Warning from the agent: Rate limit close\nAbout 10 requests are left this hour.',
        ),
        (MessageRole.notice, 'From the agent: Model switched'),
      ]);
    });

    test(
      'a compaction is one entry, updated by its id, whole summaries replace chunks and null clears',
      () {
        final session = fresh();
        Map<String, Object?> chunk(String id, String t) => {
          'sessionUpdate': 'compaction_summary_chunk',
          'compactionId': id,
          'content': {'type': 'text', 'text': t},
        };
        feed(session, chunk('c1', 'Fixed calc; '));
        expect(
          session.messages.single.compaction!.status,
          CompactionStatus.running,
          reason: 'a first chunk starts one',
        );
        feed(session, chunk('c1', 'tests pass.'));
        feed(session, {
          'sessionUpdate': 'compaction_update',
          'compactionId': 'c1',
          'status': 'completed',
        });
        feed(session, {
          'sessionUpdate': 'compaction_update',
          'compactionId': 'c2',
          'status': 'failed',
          'error': 'Too little room',
        });
        expect(session.messages.map((m) => m.role), [
          MessageRole.compaction,
          MessageRole.compaction,
        ]);
        final first = session.messages.first.compaction!;
        expect(
          (first.status, first.summary),
          (CompactionStatus.done, 'Fixed calc; tests pass.'),
        );
        expect(session.messages.last.compaction!.error, 'Too little room');
        feed(session, {
          'sessionUpdate': 'compaction_update',
          'compactionId': 'c1',
          'status': 'completed',
          'summary': [
            {'type': 'text', 'text': 'One line.'},
          ],
        });
        expect(first.summary, 'One line.');
        feed(session, {
          'sessionUpdate': 'compaction_update',
          'compactionId': 'c1',
          'summary': null,
        });
        expect(first.summary, isEmpty);
      },
    );
  });

  group('what the agent offers', () {
    test(
      'commands, usage and the title come from their updates, the title the agent names until a person has',
      () {
        final session = fresh();
        feed(session, {
          'sessionUpdate': 'available_commands_update',
          'availableCommands': [
            {'name': 'review', 'description': 'Review the diff'},
            {'description': 'no name'},
          ],
        });
        expect(session.commands.map((c) => c.name), ['review']);
        feed(session, {
          'sessionUpdate': 'usage_update',
          'used': 50,
          'size': 200,
          'cost': {'amount': 0.5, 'currency': 'USD'},
        });
        feed(session, {'sessionUpdate': 'usage_update', 'used': 100});
        expect(
          (
            session.usage.used,
            session.usage.size,
            session.usage.cost,
            session.usage.currency,
            session.usage.percent,
          ),
          (100, 200, 0.5, 'USD', 50),
        );
        feed(session, {
          'sessionUpdate': 'session_info_update',
          'title': 'Fix the calculator',
        });
        expect(session.title, 'Fix the calculator');
        session.rename('Mine');
        feed(session, {
          'sessionUpdate': 'session_info_update',
          'title': 'Agent again',
        });
        expect(session.title, 'Mine');
      },
    );

    test(
      'options come from config_option_update and the mode follows current_mode_update',
      () {
        final session = fresh();
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
                {
                  'name': 'group',
                  'options': [
                    {'value': 'b', 'name': 'B'},
                  ],
                },
              ],
            },
            {'id': 'broken', 'currentValue': 'zz', 'options': <Object>[]},
          ],
        });
        expect(session.options.map((o) => o.id), ['model']);
        expect(session.options.single.choices.map((c) => c.value), ['a', 'b']);
        // An agent that says both says the same thing twice, and the options win.
        session.applyOpened(
          AcpOpenedSession('s1', {
            'configOptions': [
              {
                'id': 'mode',
                'category': 'mode',
                'currentValue': 'manual',
                'options': [
                  {'value': 'manual', 'name': 'Manual'},
                ],
              },
            ],
            'modes': {
              'currentModeId': 'auto',
              'availableModes': [
                {'id': 'auto', 'name': 'Auto'},
              ],
            },
          }),
        );
        expect(session.options.single.current, 'manual');
      },
    );

    test(
      'an agent with only v1 modes has one mode option, which current_mode_update changes',
      () {
        final session = fresh();
        session.applyOpened(
          AcpOpenedSession('s1', {
            'modes': {
              'currentModeId': 'manual',
              'availableModes': [
                {'id': 'manual', 'name': 'Manual'},
                {'id': 'auto', 'name': 'Auto'},
              ],
            },
          }),
        );
        expect(session.mode, 'manual');
        feed(session, {
          'sessionUpdate': 'current_mode_update',
          'currentModeId': 'auto',
        });
        expect(session.mode, 'auto');
        expect(roleOf(session.options.single), OptionRole.mode);
      },
    );

    test(
      "the host's notices about an agent show when its conversation opens, bounded and said to be the agent's",
      () {
        final session = fresh(key: 'example.test/notices');
        session.applyOpened(
          AcpOpenedSession('s1', {
            '_meta': {
              'example.test/notices': [
                'pi runs bash${String.fromCharCode(0x202e)} without asking',
                42,
                'Each message runs pi anew.',
                'x' * 2000,
                'four',
                'five',
              ],
            },
          }),
        );
        final notices = [
          for (final m in session.messages)
            if (m.role == MessageRole.notice) m.text,
        ];
        expect(
          notices,
          hasLength(3),
          reason: 'four taken, one of them not text',
        );
        expect(notices[0], 'The agent says: pi runs bash  without asking');
        expect(notices[1], 'The agent says: Each message runs pi anew.');
        expect(
          notices[2].length,
          lessThanOrEqualTo(513 + 'The agent says: '.length),
        );

        final plain = fresh(key: 'example.test/notices');
        plain.applyOpened(
          AcpOpenedSession('s2', {
            '_meta': {'example.test/notices': 'not a list'},
          }),
        );
        expect(plain.messages, isEmpty);
        final unset = fresh();
        unset.applyOpened(
          AcpOpenedSession('s2', {
            '_meta': {
              'example.test/notices': ['said'],
            },
          }),
        );
        expect(
          unset.messages,
          isEmpty,
          reason: 'a host that names no key reads none',
        );
      },
    );

    test(
      'an update for another session of the connection is not this conversation\'s',
      () async {
        final (:store, :session, :agent) = started();
        agent.update(
          text('agent_message', 'for another', id: 'x'),
          session: 'other',
        );
        agent.update(text('agent_message', 'for me', id: 'y'));
        await settle();
        expect(session.messages.map((m) => m.text), ['for me']);
        store.dispose();
      },
    );
  });

  group('turns', () {
    test(
      'start when the agent runs and end when it is idle, with how long and how it ended',
      () {
        var now = DateTime.utc(2026, 11, 12, 10);
        final store = ChatStore(clock: () => now);
        final session = store.add(title: 't', agent: 'a', sessionId: 's1');
        expect(session.currentTurn, isNull);
        feed(session, state('running'));
        now = now.add(const Duration(seconds: 3));
        feed(session, text('agent_message', 'Working', id: 'm'));
        now = now.add(const Duration(seconds: 9));
        feed(session, state('idle', reason: 'end_turn'));
        expect(session.currentTurn, isNull);
        final turn = session.turns.single;
        expect(
          (turn.outcome, turn.duration(now)),
          (TurnOutcome.completed, const Duration(seconds: 12)),
        );
        expect(
          session.messages.single.turn,
          turn.id,
          reason: 'what was added during it belongs to it',
        );
        feed(session, state('running'));
        feed(session, state('idle', reason: 'cancelled'));
        feed(session, state('running'));
        feed(session, state('idle', reason: 'refusal'));
        expect(session.turns.map((t) => t.outcome), [
          TurnOutcome.completed,
          TurnOutcome.cancelled,
          TurnOutcome.failed,
        ]);
        store.dispose();
      },
    );

    test('are remembered up to the limit, the oldest going first', () {
      final session = fresh(limits: const ChatLimits(turns: 2));
      for (var i = 0; i < 3; i++) {
        feed(session, state('running'));
        feed(session, state('idle'));
      }
      expect(session.turns.map((t) => t.id), [2, 3]);
    });

    test(
      'an idle state outside a turn ends nothing, and says nothing',
      () async {
        final (:store, :session, :agent) = started();
        final seen = <AttentionKind>[];
        store.attention.listen((a) => seen.add(a.kind));
        feed(session, state('idle'));
        await settle();
        expect(seen, isEmpty);
        expect(session.status, ChatStatus.connected);
        store.dispose();
      },
    );
  });
}

void _grouping() {
  group('what a transcript folds', () {
    test('a tool call names the files it changes, each once', () {
      final session = fresh();
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 't',
        'content': [
          {'type': 'diff', 'path': 'a.js', 'newText': 'x'},
          {'type': 'diff', 'path': 'a.js', 'oldText': 'x', 'newText': 'y'},
          {
            'patch': {'text': '+++ b/b.js\n'},
          },
          {
            'type': 'content',
            'content': {'type': 'text', 'text': 'done'},
          },
        ],
      });
      expect(session.toolEntries['t']!.tool!.changedPaths, ['a.js', 'b.js']);
    });

    test('consecutive tool calls are a run, and anything else breaks it', () {
      final session = fresh();
      feed(session, text('agent_message', 'Starting', id: 'a'));
      for (final id in ['t1', 't2', 't3']) {
        feed(session, {
          'sessionUpdate': 'tool_call',
          'toolCallId': id,
          'title': id,
        });
      }
      feed(session, text('agent_message', 'Done', id: 'b'));
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 't4',
        'title': 't4',
      });
      expect(
        toolRuns(
          session.messages,
        ).map((run) => run.map((m) => m.tool?.id ?? m.text).toList()),
        [
          ['Starting'],
          ['t1', 't2', 't3'],
          ['Done'],
          ['t4'],
        ],
      );
      expect(toolRuns(const []), isEmpty);
    });
  });
}
