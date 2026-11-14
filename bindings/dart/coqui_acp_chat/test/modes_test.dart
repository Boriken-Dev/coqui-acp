// The mode of a conversation: one change at a time in the order asked, quick presses that go on from the mode last asked for, a mode
// the agent refused skipped, nothing changing while the conversation is being opened again, and the mode put back on reopening.
import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

AcpOpenedSession _modes([String current = 'manual']) => AcpOpenedSession('s1', {
  'modes': {
    'currentModeId': current,
    'availableModes': [
      {'id': 'manual', 'name': 'Manual'},
      {'id': 'auto', 'name': 'Auto'},
      {'id': 'plan', 'name': 'Plan'},
    ],
  },
});

({ChatStore store, ChatSession session, FakeConnection agent}) _moded() {
  final started0 = started();
  started0.session.applyOpened(_modes());
  return started0;
}

void main() {
  test(
    'cycling goes through the modes in the order the translators give them, Bypass last, and round',
    () async {
      final (:store, :session, :agent) = _moded();
      await session.cycleMode();
      await session.cycleMode();
      expect(agent.modes, ['auto', 'plan']);
      await session.cycleMode();
      expect(
        session.bypass,
        isTrue,
        reason:
            'the agent has no mode that asks for nothing, so Bypass is the chat\'s own',
      );
      expect(session.mode, ownBypass);
      expect(agent.modes, [
        'auto',
        'plan',
      ], reason: 'the agent\'s own mode stays');
      await session.cycleMode();
      expect(session.bypass, isFalse);
      expect(agent.modes, ['auto', 'plan', 'manual']);
      store.dispose();
    },
  );

  test(
    'quick presses go on from the mode last asked for, not the one the agent still shows',
    () async {
      final (:store, :session, :agent) = _moded();
      final first = session.cycleMode();
      final second = session.cycleMode();
      expect(session.targetMode, 'plan');
      await Future.wait([first, second]);
      expect(agent.modes, ['auto', 'plan']);
      expect(session.targetMode, isNull);
      store.dispose();
    },
  );

  test(
    'changes made together run in the order they were asked, each after the one before',
    () async {
      final (:store, :session, :agent) = _moded();
      final options = session.options.single;
      final entries = modeEntries(options);
      final order = <String>[];
      agent.onSetMode = (mode) async {
        order.add('start $mode');
        await Future<void>.delayed(const Duration(milliseconds: 5));
        order.add('end $mode');
      };
      await Future.wait([
        session.chooseMode(entries.firstWhere((e) => e.value == 'plan')),
        session.chooseMode(entries.firstWhere((e) => e.value == 'auto')),
      ]);
      expect(order, ['start plan', 'end plan', 'start auto', 'end auto']);
      expect(session.chosenMode, 'auto');
      store.dispose();
    },
  );

  test('a mode the agent refused is not asked for again by cycling', () async {
    final (:store, :session, :agent) = _moded();
    agent.onSetMode = (mode) async {
      if (mode == 'auto') {
        throw const AcpFailure('Agent error: {code: 1, message: not allowed}');
      }
    };
    await session.cycleMode();
    expect(session.refusedModes, {'auto'});
    expect(session.mode, 'manual');
    expect(
      session.messages.last.text,
      'The agent did not change Mode to Auto: not allowed',
    );
    await session.cycleMode();
    expect(agent.modes, ['auto', 'plan'], reason: 'it went on to the next');
    store.dispose();
  });

  test(
    'leaving Bypass takes the chat\'s own off and keeps the agent\'s mode, an agent\'s own goes to Manual',
    () async {
      final (:store, :session, :agent) = _moded();
      session.setBypass(true);
      await session.leaveBypass();
      expect(session.bypass, isFalse);
      expect(
        agent.modes,
        isEmpty,
        reason: 'nothing is sent for the chat\'s own',
      );
      session.applyOpened(
        AcpOpenedSession('s1', {
          'modes': {
            'currentModeId': 'bypassPermissions',
            'availableModes': [
              {'id': 'default', 'name': 'Default'},
              {'id': 'bypassPermissions', 'name': 'Bypass'},
            ],
          },
        }),
      );
      await session.leaveBypass();
      expect(agent.modes, ['default']);
      store.dispose();
    },
  );

  test(
    'the mode set, or the agent\'s own change of it, is told to be kept; its starting default is not',
    () async {
      final (:store, :session, :agent) = _moded();
      final kept = <(String?, bool)>[];
      session.onModeKept = (mode, bypass) => kept.add((mode, bypass));
      expect(session.chosenMode, isNull);
      await session.cycleMode();
      session.setBypass(true);
      feed(session, {
        'sessionUpdate': 'current_mode_update',
        'currentModeId': 'plan',
      });
      expect(kept, [('auto', false), ('auto', true), ('plan', true)]);
      expect(session.chosenMode, 'plan');
      store.dispose();
    },
  );

  test(
    'nothing changes while it is being opened again, and what is kept is told once it is open',
    () async {
      final (:store, :session, :agent) = _moded();
      final kept = <(String?, bool)>[];
      session.onModeKept = (mode, bypass) => kept.add((mode, bypass));
      var during = <bool>[];
      final opened = session.reopen(() async {
        during = [session.canChangeMode, session.opening];
        await session.cycleMode();
        session.setBypass(true);
        return _modes();
      });
      await opened;
      expect(during, [false, true]);
      expect(
        agent.modes,
        isEmpty,
        reason: 'a press while it opens does nothing',
      );
      expect(session.canChangeMode, isTrue);
      expect(
        session.bypass,
        isFalse,
        reason: 'setting Bypass while it opens does nothing',
      );
      expect(kept.single, (
        null,
        false,
      ), reason: 'what is kept is told once it is open');
      store.dispose();
    },
  );

  test('the mode it was left in is put back before anything is sent', () async {
    final (:store, :session, :agent) = _moded();
    await session.reopen(() async => _modes('manual'), keptMode: 'plan');
    expect(agent.modes, ['plan']);
    expect(session.chosenMode, 'plan');
    await session.send('go');
    expect(agent.calls, ['mode:plan', 'prompt:go']);
    store.dispose();
  });

  test('and Bypass of the chat\'s own with it', () async {
    final (:store, :session, :agent) = _moded();
    await session.reopen(
      () async => _modes(),
      keptBypass: true,
      keptMode: 'auto',
    );
    expect(session.bypass, isTrue);
    expect(agent.modes, ['auto']);
    store.dispose();
  });

  test(
    'Bypass the chat provides can be left while it opens, and the ledger is told once it is open',
    () async {
      final (:store, :session, :agent) = _moded();
      final kept = <(String?, bool)>[];
      session.onModeKept = (mode, bypass) => kept.add((mode, bypass));
      await session.reopen(
        () async {
          await session.leaveBypass();
          expect(session.bypass, isFalse);
          expect(kept, isEmpty, reason: 'not while it opens');
          return _modes();
        },
        keptBypass: true,
        keptMode: 'auto',
      );
      expect(session.bypass, isFalse, reason: 'leaving it was not undone');
      expect(agent.modes, ['auto']);
      expect(kept, [('auto', false)]);
      store.dispose();
    },
  );

  test('a choice made while it opens waits and goes when it is open', () async {
    final (:store, :session, :agent) = _moded();
    await session.reopen(() async {
      await session.choose(session.options.single, 'plan');
      expect(agent.modes, isEmpty);
      return _modes();
    });
    await session.send('go');
    expect(agent.calls, ['mode:plan', 'prompt:go']);
    store.dispose();
  });

  group('reopening a conversation that has history', () {
    Future<void> history(ChatSession session) async {
      feed(session, text('user_message', 'Fix it', id: 'u1'));
      feed(session, text('agent_message', 'Done', id: 'a1'));
      feed(session, text('agent_message_chunk', 'A v1 line with no id'));
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 't',
        'title': 'Edit',
      });
      feed(session, {
        'sessionUpdate': 'plan',
        'entries': [
          {'content': 'x', 'status': 'pending'},
        ],
      });
    }

    test('does not show it twice when the agent sends it again', () async {
      final (:store, :session, :agent) = _moded();
      await history(session);
      await session.reopen(() async {
        await history(session);
        return AcpOpenedSession('s1', {}, replayed: true);
      });
      expect(
        session.messages.where((m) => m.role == MessageRole.user),
        hasLength(1),
      );
      expect(
        session.messages.where((m) => m.role == MessageRole.agent),
        hasLength(2),
      );
      expect(session.toolEntries.keys, ['t']);
      expect(
        session.messages.where((m) => m.role == MessageRole.tool),
        hasLength(1),
      );
      store.dispose();
    });

    test('keeps it when the agent only resumes', () async {
      final (:store, :session, :agent) = _moded();
      await history(session);
      await session.reopen(() async => AcpOpenedSession('s1', {}));
      expect(
        session.messages
            .where((m) => m.role == MessageRole.user)
            .map((m) => m.text),
        ['Fix it'],
      );
      expect(session.plan, hasLength(1));
      store.dispose();
    });

    test('keeps it when the agent refuses', () async {
      final (:store, :session, :agent) = _moded();
      await history(session);
      await expectLater(
        session.reopen(() async => throw const AcpFailure('no')),
        throwsA(isA<AcpFailure>()),
      );
      expect(
        session.messages.where((m) => m.role == MessageRole.user),
        hasLength(1),
      );
      expect(session.toolEntries.keys, ['t']);
      store.dispose();
    });
  });
}
