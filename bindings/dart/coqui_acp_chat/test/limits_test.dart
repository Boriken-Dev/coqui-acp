// Every limit is a setting with a default, and takes any value from 0 up; the notifier a host wraps in its own.
import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

void main() {
  _titles();
  test('the defaults are the ones documented', () {
    const limits = ChatLimits();
    expect(
      [
        limits.entries,
        limits.messageText,
        limits.pendingPermissions,
        limits.pendingQuestions,
        limits.settledRequests,
        limits.queued,
        limits.clients,
        limits.turns,
        limits.toolText,
        limits.terminalOutput,
        limits.planEntries,
        limits.commands,
        limits.configOptions,
        limits.choices,
        limits.toolBlocks,
        limits.locations,
        limits.shortText,
        limits.terminals,
        limits.describedInput,
        limits.hostNotices,
        limits.listShapes,
        limits.shapeDepth,
        limits.titleLength,
        limits.usageBuckets,
      ],
      [
        5000,
        2097152,
        64,
        16,
        32,
        20,
        16,
        1000,
        262144,
        262144,
        200,
        500,
        32,
        200,
        64,
        64,
        512,
        256,
        4000,
        4,
        8,
        8,
        60,
        64,
      ],
    );
  });

  test(
    'zero is a value: it keeps nothing of that kind and breaks nothing',
    () async {
      const zero = ChatLimits(
        entries: 0,
        messageText: 0,
        pendingPermissions: 0,
        pendingQuestions: 0,
        settledRequests: 0,
        queued: 0,
        clients: 0,
        turns: 0,
        toolText: 0,
        terminalOutput: 0,
        planEntries: 0,
        commands: 0,
        configOptions: 0,
        choices: 0,
        toolBlocks: 0,
        locations: 0,
        shortText: 0,
        terminals: 0,
        describedInput: 0,
        hostNotices: 0,
        listShapes: 0,
        shapeDepth: 0,
        titleLength: 0,
        usageBuckets: 0,
      );
      final (:store, :session, :agent) = started(limits: zero);
      feed(session, state('running'));
      feed(session, text('agent_message', 'said', id: 'm'));
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 't',
        'title': 'x',
        'content': [
          {'type': 'terminal', 'terminalId': 'a'},
        ],
      });
      feed(session, {
        'sessionUpdate': 'terminal_update',
        'terminalId': 'a',
        'command': 'x',
      });
      feed(session, {
        'sessionUpdate': 'plan',
        'entries': [
          {'content': 'x'},
        ],
      });
      feed(session, state('idle'));
      expect(session.messages, isEmpty);
      expect(session.terminals, isEmpty);
      expect(session.plan, isEmpty);
      expect(session.turns, isEmpty);
      expect(await store.askPermission(request(1), agent), isA<AcpCancel>());
      expect(session.pendingPermissions, isEmpty);
      await session.send('go');
      feed(session, state('running'));
      await expectLater(session.send('queued'), throwsStateError);
      expect(() => session.attachClient('desk'), throwsStateError);
      store.dispose();
    },
  );

  test(
    'a listener that throws does not stop the others, and the first error is rethrown after all have run',
    () {
      final notifier = ChatNotifier();
      var heard = 0;
      notifier
        ..addListener(() => throw StateError('first'))
        ..addListener(() => heard++)
        ..addListener(() => throw StateError('second'));
      expect(
        notifier.notifyListeners,
        throwsA(isA<StateError>().having((e) => e.message, 'message', 'first')),
      );
      expect(heard, 1);
      notifier.dispose();
      notifier.notifyListeners();
      expect(heard, 1, reason: 'a disposed notifier tells nobody');
    },
  );

  test(
    'what is shown of a name is one line, cut at the limit, and a blank or a non-string is nothing',
    () {
      const limits = ChatLimits(shortText: 5);
      expect(shortText('abcdefgh', limits), 'abcde…');
      expect(shortText('  a\nb  ', limits), 'a b');
      expect(shortText('   ', limits), isNull);
      expect(shortText(7, limits), isNull);
    },
  );
}

void _titles() {
  test('a title is the first line of a message, cut at the limit', () {
    expect(titleFrom('Fix the test\nplease'), 'Fix the test');
    expect(titleFrom('x' * 100), '${'x' * 59}…');
    expect(titleFrom('x' * 100, const ChatLimits(titleLength: 5)), 'xxxx…');
    expect(titleFrom('abc', const ChatLimits(titleLength: 0)), '');
    expect(
      shapeOf({
        'a': {'b': 1},
      }, limits: const ChatLimits(shapeDepth: 1)),
      {
        'a': {'b': '…'},
      },
    );
  });
}
