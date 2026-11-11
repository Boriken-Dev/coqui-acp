// Points a verification of the model raised, each held by a test: a message written while a conversation opens, modes that do not
// change then, state that cannot be written from outside, a v1 prompt the agent refuses, hostile numbers, and the edges of bounded text.
import 'dart:async';
import 'dart:convert';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat/experimental.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

/// A connection whose prompt throws before it returns anything.
final class SyncThrowConnection extends ChatConnection {
  SyncThrowConnection({this.version = 1});
  final int version;
  final _u = StreamController<AcpSessionUpdate>.broadcast();
  final _e = StreamController<AcpEvent>.broadcast();
  @override
  Stream<AcpSessionUpdate> get updates => _u.stream;
  @override
  Stream<AcpEvent> get events => _e.stream;
  @override
  bool get isClosed => false;
  @override
  int? get protocolVersion => version;
  @override
  Future<String?> prompt(
    String sessionId,
    String text, {
    List<AcpAttachment> attachments = const [],
  }) {
    throw StateError('closed under us');
  }

  @override
  Future<void> cancel(String sessionId) async {}
  @override
  Future<void> setMode(String sessionId, String modeId) async {}
  @override
  Future<Map<String, dynamic>> setConfigOption(
    String sessionId,
    String configId,
    String value,
  ) async => {};
  @override
  Future<void> close() async {}
}

Map<String, Object?> modes(String current) => {
  'currentModeId': current,
  'availableModes': [
    {'id': 'default', 'name': 'Default'},
    {'id': 'plan', 'name': 'Plan'},
    {'id': 'auto', 'name': 'Auto'},
  ],
};

List<String> _shown(ChatSession s) => [
  for (final m in s.messages) '${m.role.name}:${m.text}',
];

void main() {
  group('a conversation that is being opened again', () {
    test(
      'queues what is written, and sends it after the kept mode is back',
      () async {
        final s = started();
        final open = Completer<AcpOpenedSession>();
        final reopened = s.session.reopen(() => open.future, keptMode: 'plan');
        await settle();
        expect(s.session.opening, isTrue);
        await s.session.send('early');
        await settle();
        expect(
          s.agent.calls,
          isEmpty,
          reason: 'nothing goes before the agent has loaded the conversation',
        );
        expect(s.session.queue.single.text, 'early');
        open.complete(AcpOpenedSession('s1', {'modes': modes('default')}));
        await reopened;
        await settle();
        expect(s.agent.calls, ['mode:plan', 'prompt:early']);
        expect(
          s.session.messages.where((m) => m.text == 'early'),
          hasLength(1),
        );
        expect(s.session.queue, isEmpty);
        s.store.dispose();
      },
    );

    test('keeps what is written, held, when it could not be opened', () async {
      final s = started();
      final open = Completer<AcpOpenedSession>();
      final reopened = s.session.reopen(() => open.future);
      await s.session.send('early');
      open.completeError(const AcpFailure('no such session'));
      await expectLater(reopened, throwsA(isA<AcpFailure>()));
      await settle();
      expect(s.session.queue.single.text, 'early');
      expect(s.session.queueHeld, 'the conversation could not be opened');
      expect(s.agent.prompts, isEmpty);
      s.store.dispose();
    });

    test('a steer then goes next, ahead of the rest', () async {
      final s = started();
      final open = Completer<AcpOpenedSession>();
      final reopened = s.session.reopen(() => open.future);
      await s.session.send('one');
      await s.session.steer('urgent');
      expect(s.session.queue.map((m) => m.text), ['urgent', 'one']);
      open.complete(AcpOpenedSession('s1', const {}));
      await reopened;
      s.store.dispose();
    });

    test(
      'changes no mode: choosing, cycling, leaving Bypass and setting it do nothing, and the kept mode holds',
      () async {
        final s = started();
        s.session.applyOpened(
          AcpOpenedSession('s1', {'modes': modes('default')}),
        );
        final open = Completer<AcpOpenedSession>();
        final reopened = s.session.reopen(() => open.future, keptMode: 'plan');
        await settle();
        final entries = modeEntries(modeOptionIn(s.session.options));
        await s.session.chooseMode(
          entries.firstWhere((e) => e.value == 'auto'),
        );
        await s.session.cycleMode();
        await s.session.leaveBypass();
        s.session.setBypass(true);
        expect(s.session.bypass, isFalse);
        expect(s.session.pendingChoices, isEmpty);
        open.complete(
          AcpOpenedSession('s1', {'modes': modes('default')}, replayed: true),
        );
        await reopened;
        expect(s.agent.modes, ['plan']);
        expect(s.session.chosenMode, 'plan');
        expect(s.session.bypass, isFalse);
        s.store.dispose();
      },
    );

    test(
      'a turn running when it is opened is over, silently, and its late end is nobody\'s news',
      () async {
        final s = started(version: 1);
        final turn = Completer<String?>();
        s.agent.onPrompt = (_) => turn.future;
        final told = <ChatEvent>[];
        s.session.events.listen(told.add);
        await s.session.send('A');
        expect(s.session.status, ChatStatus.working);
        await s.session.reopen(
          () async => AcpOpenedSession('s1', const {}, replayed: true),
        );
        expect(s.session.status, ChatStatus.connected);
        expect(s.session.currentTurn, isNull);
        turn.complete(null);
        s.agent.event(const AcpCompleted('end_turn', sessionId: 's1'));
        await settle();
        expect(told.whereType<TurnEvent>(), isEmpty);
        expect(told.whereType<AttentionEvent>(), isEmpty);
        s.store.dispose();
      },
    );
  });

  group('state is not writable from outside', () {
    test(
      'options, plan, commands, choices, blocks, locations and a request\'s options',
      () {
        final s = started();
        s.session.applyOpened(
          AcpOpenedSession('s1', {
            'modes': modes('default'),
            'availableCommands': [
              {'name': 'x', 'description': 'y'},
            ],
          }),
        );
        feed(s.session, {
          'sessionUpdate': 'plan',
          'entries': [
            {'content': 'a', 'status': 'pending'},
          ],
        });
        feed(s.session, {
          'sessionUpdate': 'tool_call',
          'toolCallId': 't',
          'content': [
            {'type': 'diff', 'path': 'a', 'newText': 'b'},
          ],
          'locations': [
            {'path': 'a'},
          ],
        });
        s.store.askPermission(request(2), s.agent);
        void throws(void Function() f) => expect(f, throwsUnsupportedError);
        throws(() => s.session.options.clear());
        throws(() => s.session.plan.clear());
        throws(() => s.session.commands.clear());
        throws(() => s.session.options.single.choices.clear());
        throws(() => s.session.toolEntries['t']!.tool!.blocks.clear());
        throws(() => s.session.toolEntries['t']!.tool!.locations.clear());
        throws(() => s.session.pendingPermissions.single.options.clear());
        s.store.dispose();
      },
    );
  });

  group('how a turn ended', () {
    test('is said in that turn, not in the next one', () async {
      final s = started();
      await s.session.send('A');
      s.agent.update(state('running'));
      await settle();
      final first = s.session.currentTurn!.id;
      s.agent.update(state('idle', reason: 'cancelled'));
      await settle();
      final stopped = s.session.messages.lastWhere((m) => m.text == 'Stopped.');
      await s.session.send('B');
      s.agent.update(state('running'));
      await settle();
      expect(stopped.turn, first);
      s.agent.update(state('idle', reason: 'error'));
      await settle();
      expect(
        s.session.messages
            .lastWhere((m) => m.text == 'Agent reported an operation failure.')
            .turn,
        s.session.turns.last.id,
      );
      s.store.dispose();
    });
  });

  group('a v1 prompt', () {
    test(
      'that throws before it returns leaves no running turn and no message',
      () async {
        final session = ChatSession(id: 'x', sessionId: 's1')
          ..attach(SyncThrowConnection());
        await expectLater(session.send('hi'), throwsStateError);
        expect(session.status, isNot(ChatStatus.working));
        expect(session.currentTurn, isNull);
        expect(
          session.messages.where((m) => m.role == MessageRole.user),
          isEmpty,
        );
      },
    );

    test(
      'that the agent refuses before it says anything leaves nothing behind, the note of attachments included',
      () async {
        final s = started(version: 1);
        s.agent.onPrompt = (text) async {
          await Future<void>.delayed(const Duration(milliseconds: 1));
          s.agent.event(
            const AcpFailed(
              'Agent error: {code: -32000, message: nope}',
              sessionId: 's1',
            ),
          );
          return null;
        };
        await s.session.send(
          'refused',
          attachments: [AcpAttachment.link('a.txt', 'https://e.test/a')],
        );
        await settle();
        expect(_shown(s.session), [
          'notice:Agent error: {code: -32000, message: nope}',
        ]);
        expect(s.session.status, ChatStatus.connected);
        expect(s.session.turns.single.outcome, TurnOutcome.failed);
        s.agent.onPrompt = null;
        await s.session.send('refused');
        expect(
          s.session.messages.where((m) => m.text == 'refused'),
          hasLength(1),
          reason: 'a retry shows it once',
        );
        s.store.dispose();
      },
    );

    test(
      'that the agent fails after it said something keeps the message, which it may have recorded',
      () async {
        final s = started(version: 1);
        s.agent.onPrompt = (_) async {
          await Future<void>.delayed(const Duration(milliseconds: 1));
          feed(s.session, text('agent_message_chunk', 'Working'));
          s.agent.event(const AcpFailed('then it broke', sessionId: 's1'));
          return null;
        };
        await s.session.send('go');
        await settle();
        expect(s.session.messages.where((m) => m.text == 'go'), hasLength(1));
        s.store.dispose();
      },
    );

    test(
      'that fails later with no answer takes back the message and the note of attachments',
      () async {
        final s = started(version: 1);
        s.agent.onPrompt = (text) async {
          await Future<void>.delayed(const Duration(milliseconds: 1));
          throw const AcpFailure('gone');
        };
        await s.session.send(
          'x',
          attachments: [AcpAttachment.link('a.txt', 'https://e.test/a')],
        );
        await settle();
        expect(_shown(s.session), ['notice:The prompt failed: gone']);
        s.store.dispose();
      },
    );

    test('refused at once, with attachments, leaves no note', () async {
      final s = started(version: 1);
      s.agent.onPrompt = (text) => Future.error(const AcpFailure('no'));
      feed(s.session, text('agent_message_chunk', 'earlier'));
      await expectLater(
        s.session.send(
          'x',
          attachments: [AcpAttachment.link('a.txt', 'https://e.test/a')],
        ),
        throwsA(isA<AcpFailure>()),
      );
      expect(s.session.messages.map((m) => m.text), ['earlier']);
      expect(s.session.status, ChatStatus.connected);
      s.store.dispose();
    });
  });

  group('a v2 prompt', () {
    test(
      'the agent echoed before it refused stays, for the agent recorded it',
      () async {
        final s = started();
        s.agent.onPrompt = (text) async {
          s.agent.update(text0('user_message', text, 'm1'));
          await settle();
          throw const AcpFailure('Agent error: {code: -1, message: busy}');
        };
        await expectLater(s.session.send('hello'), throwsA(isA<AcpFailure>()));
        expect(_shown(s.session), ['user:hello']);
        expect(s.session.messages.single.messageId, 'm1');
        s.store.dispose();
      },
    );

    test(
      'a steer while one is handed over with no room in the queue goes next, and is not refused',
      () async {
        final s = started(limits: const ChatLimits(queued: 0));
        final ack = Completer<String?>();
        s.agent.onPrompt = (text) =>
            text == 'A' ? ack.future : Future.value('m2');
        final a = s.session.send('A');
        await settle();
        await s.session.steer('B');
        expect(s.session.queue.single.text, 'B');
        ack.complete('m1');
        await a;
        s.agent.update(state('running'));
        s.agent.update(state('idle', reason: 'end_turn'));
        await settle();
        expect(s.agent.prompts, ['A', 'B']);
        s.store.dispose();
      },
    );

    test(
      'the conversation let go while it is handed over is left as it is when the acknowledgement comes',
      () async {
        final s = started();
        final ack = Completer<String?>();
        s.agent.onPrompt = (_) => ack.future;
        final sent = s.session.send('x');
        await settle();
        s.session.detach();
        ack.complete('m1');
        await sent;
        expect(s.session.status, ChatStatus.disconnected);
        expect(_shown(s.session), ['user:x']);
        s.store.dispose();
      },
    );
  });

  group('what an agent sends as an identifier', () {
    test(
      'is used as it came or not at all: too long, or with a control character, it is dropped',
      () {
        final session = ChatSession(id: 'x', sessionId: 's1');
        feed(session, {
          'sessionUpdate': 'tool_call',
          'toolCallId': 'a' * 600,
          'title': 'long',
        });
        feed(session, {
          'sessionUpdate': 'tool_call',
          'toolCallId': 'a\nb',
          'title': 'newline',
        });
        feed(session, {
          'sessionUpdate': 'terminal_update',
          'terminalId': 't\u0007',
          'command': 'x',
        });
        feed(session, {
          'sessionUpdate': 'compaction_update',
          'compactionId': '',
          'status': 'completed',
        });
        feed(session, {
          'sessionUpdate': 'tool_call',
          'toolCallId': 'ok',
          'title': 'fine',
        });
        expect(session.toolEntries.keys, ['ok']);
        expect(session.terminals, isEmpty);
        expect(session.compactions, isEmpty);
        final options = parseConfigOptions([
          {
            'id': 'bad\nid',
            'currentValue': 'a',
            'options': [
              {'value': 'a'},
            ],
          },
          {
            'id': 'model',
            'currentValue': 'a',
            'options': [
              {'value': 'a'},
              {'value': 'b\nc'},
              {'value': 'd' * 600},
            ],
          },
        ], const ChatLimits())!;
        expect(options.map((o) => o.id), ['model']);
        expect(options.single.choices.map((c) => c.value), ['a']);
      },
    );

    test('a request\'s kind is a clean line', () {
      final s = started();
      s.store.askPermission(request(1, kind: 'ed\nit'), s.agent);
      expect(s.session.pendingPermissions.single.kind, 'ed it');
      s.store.dispose();
    });
  });

  group('usage figures an agent sends', () {
    test('that are not finite whole numbers are skipped, never thrown on', () {
      final hostile = <Object?>[
        jsonDecode('{"supported": true, "revision": 1e400, "buckets": []}'),
        {
          'supported': true,
          'revision': double.nan,
          'observed_at': double.negativeInfinity,
        },
        {'supported': true, 'revision': 1e300, 'observed_at': 1.5},
        {
          'supported': true,
          'buckets': [
            {
              'id': 'b',
              'used': double.infinity,
              'limit': double.nan,
              'remaining': 1e400,
              'unit': 'tokens',
            },
            {
              'id': 'c',
              'refill': {
                'kind': 'fixed_window',
                'length': [1e400, 0],
                'resets_at': 1e19,
              },
            },
            {
              'id': 'd',
              'refill': {
                'kind': 'token_bucket',
                'amount': 1,
                'per': [double.infinity, double.nan],
                'max': -1e400,
              },
            },
            {
              'id': 'e',
              'refill': {
                'kind': 'fixed_window',
                'length': [9007199254740990, 0],
              },
            },
          ],
          'limit': {
            'retry_after': [1e400, 1e400],
            'resets_at': double.infinity,
          },
          'spend': {
            'session_id': 's',
            'input': double.infinity,
            'cost_micro_usd': 1e400,
            'by_model': [
              {'model': 'm', 'output': double.nan},
            ],
          },
        },
        {
          'supported': true,
          'buckets': 'x',
          'sources': 3,
          'limit': [],
          'spend': 'no',
        },
        {
          'supported': true,
          'buckets': [null, 1, 'x', [], {}],
        },
      ];
      for (final json in hostile) {
        final report = UsageReport.parse(json);
        expect(report, isNotNull, reason: '$json');
        expect(report!.toJson, returnsNormally, reason: '$json');
      }
      final report = UsageReport.parse(hostile[3])!;
      expect(report.buckets.first.used, isNull);
      expect(report.buckets.first.fractionPermille, isNull);
      expect(report.spend!.input, 0);
      expect(report.spend!.costMicroUsd, isNull);
      expect(
        UsageSupport.parse({
          usageCapability: {'version': 1e400, 'sources': 'x'},
        }),
        isNotNull,
      );
    });
  });

  group('bounded text', () {
    test(
      'at the bound, across a split character, and when it is read twice',
      () {
        final h = BoundedText.head(5)
          ..append('abc')
          ..append('de');
        expect(h.value, 'abcde');
        h.append('f');
        expect(h.value, 'abcde\n…');
        h.append('g');
        expect(h.value, 'abcde\n…');
        final cut = BoundedText.head(3)..append('ab\u{1F600}');
        expect(cut.value, 'ab\n…');
        final split = BoundedText.head(10)
          ..append('a\uD83D')
          ..append('\uDE00b');
        expect(split.value, 'a\u{1F600}b');
        final tail = BoundedText.tail(3);
        for (var i = 0; i < 100; i++) {
          tail.append('$i');
        }
        expect(tail.value, '899');
        expect(
          identical(tail.value, tail.value),
          isTrue,
          reason: 'read again without an append, it is not made again',
        );
        final smile = BoundedText.tail(2)..append('x\u{1F600}');
        expect(smile.value, '\u{1F600}');
        final lone = BoundedText.tail(2)..append('\u{1F600}y');
        expect(lone.value, 'y');
        expect((BoundedText.head(0)..append('abc')).value, '\n…');
      },
    );
  });

  group('serving a conversation to a client that attaches late', () {
    test(
      'a mirror has the turns, times and senders, and a v2 mode option under its own id',
      () async {
        var t = DateTime.utc(2026, 1, 1);
        final s = started(clock: () => t = t.add(const Duration(seconds: 1)));
        s.session.attachClient('w1');
        s.session.applyOpened(
          AcpOpenedSession('s1', {
            'configOptions': [
              {
                'configId': 'permission_mode',
                'name': 'Mode',
                'category': 'mode',
                'currentValue': 'default',
                'options': [
                  {'value': 'default', 'name': 'Default'},
                  {'value': 'plan', 'name': 'Plan'},
                ],
              },
            ],
          }),
        );
        await s.session.send('A', from: 'w1');
        s.agent.update(state('running'));
        s.agent.update(text('agent_message_chunk', 'ok', id: 'a1'));
        s.agent.update(state('idle', reason: 'end_turn'));
        await settle();
        await s.session.send('B', from: 'w1');
        s.agent.update(state('running'));
        s.agent.update(text('agent_message_chunk', 'no', id: 'a2'));
        s.agent.update(state('idle', reason: 'cancelled'));
        await settle();
        final mirror = ChatSession(id: 'm', sessionId: 's1')
          ..applyReplay(s.session.replay());
        String shape(ChatSession x) => [
          for (final m in x.messages)
            if (m.role != MessageRole.notice)
              '${m.role.name}:${m.text}:${m.sentBy}:${m.turn == null ? '-' : x.turns.indexWhere((t) => t.id == m.turn)}:${m.at.toUtc()}',
        ].join('|');
        expect(shape(mirror), shape(s.session));
        expect(
          [for (final t in mirror.turns) t.outcome],
          [for (final t in s.session.turns) t.outcome],
        );
        expect(
          [for (final t in mirror.turns) t.startedAt.toUtc()],
          [for (final t in s.session.turns) t.startedAt.toUtc()],
        );
        expect(
          [
            for (final o in mirror.options)
              '${o.id}/${o.category}/${o.current}',
          ],
          [
            for (final o in s.session.options)
              '${o.id}/${o.category}/${o.current}',
          ],
        );
        s.store.dispose();
      },
    );
  });
}
