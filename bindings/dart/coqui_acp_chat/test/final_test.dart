// Points of a final check, each held by a test: a queue the person was holding stays held when a conversation opens, two openings at
// once are refused, a mode that could not be put back holds what waits, a hand-over that fails or never answers does not stall the
// queue, and the second end of a v1 turn does not end the next one.
import 'dart:async';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

Map<String, Object?> _modes(String current) => {
  'currentModeId': current,
  'availableModes': [
    {'id': 'default', 'name': 'Default'},
    {'id': 'plan', 'name': 'Plan'},
  ],
};

AcpOpenedSession _opened([String current = 'default']) =>
    AcpOpenedSession('s1', {'modes': _modes(current)});

void main() {
  group('a queue the person was holding', () {
    test(
      'stays held when the conversation is opened again, and what is written meanwhile joins it',
      () async {
        final s = started();
        await s.session.send('A');
        feed(s.session, state('running'));
        await s.session.send('B');
        await s.session.cancel();
        feed(s.session, state('idle', reason: 'cancelled'));
        await settle();
        expect(s.session.queueHeld, 'the turn was stopped');
        final open = Completer<AcpOpenedSession>();
        final reopened = s.session.reopen(() => open.future);
        await s.session.send('C');
        open.complete(AcpOpenedSession('s1', const {}));
        await reopened;
        await settle();
        expect(s.agent.prompts, [
          'A',
        ], reason: 'nothing the person held is sent');
        expect(s.session.queue.map((m) => m.text), ['B', 'C']);
        expect(s.session.queueHeld, 'the turn was stopped');
        s.store.dispose();
      },
    );

    test(
      'is held behind a v1 turn that the opening ended, which is told as a turn that ended',
      () async {
        final s = started(version: 1);
        final turn = Completer<String?>();
        s.agent.onPrompt = (_) => turn.future;
        final told = <ChatEvent>[];
        s.session.events.listen(told.add);
        await s.session.send('A');
        await s.session.send('B');
        await s.session.reopen(
          () async => AcpOpenedSession('s1', const {}, replayed: true),
        );
        await settle();
        expect(s.agent.prompts, ['A']);
        expect(s.session.queue.single.text, 'B');
        expect(s.session.queueHeld, 'the turn was stopped');
        expect(told.whereType<TurnEvent>().map((e) => e.turn.outcome), [
          TurnOutcome.cancelled,
        ]);
        expect(told.whereType<AttentionEvent>(), isEmpty);
        s.store.dispose();
      },
    );
  });

  group('two openings at once', () {
    test(
      'the second is refused, and the first goes on, keeping the conversation',
      () async {
        final s = started();
        feed(s.session, text('agent_message', 'kept', id: 'a1'));
        final open = Completer<AcpOpenedSession>();
        final first = s.session.reopen(() => open.future);
        await expectLater(
          s.session.reopen(
            () async => AcpOpenedSession('s1', const {}, replayed: true),
          ),
          throwsStateError,
        );
        expect(s.session.opening, isTrue);
        open.complete(AcpOpenedSession('s1', const {}));
        await first;
        expect(s.session.opening, isFalse);
        expect(s.session.messages.where((m) => m.text == 'kept'), hasLength(1));
        s.store.dispose();
      },
    );

    test('a first that fails leaves the flag as it was for the next', () async {
      final s = started();
      await expectLater(
        s.session.reopen(() async => throw const AcpFailure('no')),
        throwsA(isA<AcpFailure>()),
      );
      expect(s.session.opening, isFalse);
      await s.session.reopen(() async => AcpOpenedSession('s1', const {}));
      s.store.dispose();
    });
  });

  group('a mode it was left in that is not taken', () {
    test(
      'holds what waits, is recorded as refused, and the conversation follows the agent\'s mode',
      () async {
        final s = started();
        s.session.applyOpened(_opened());
        final kept = <(String?, bool)>[];
        s.session.onModeKept = (mode, bypass) => kept.add((mode, bypass));
        s.agent.onSetMode = (mode) async =>
            throw const AcpFailure('Agent error: {code: 1, message: no plan}');
        final open = Completer<AcpOpenedSession>();
        final reopened = s.session.reopen(() => open.future, keptMode: 'plan');
        await s.session.send('early');
        open.complete(_opened());
        await reopened;
        await settle();
        expect(s.session.refusedModes, {'plan'});
        expect(s.session.chosenMode, 'default');
        expect(kept.last.$1, 'default');
        expect(s.agent.prompts, isEmpty);
        expect(s.session.queue.single.text, 'early');
        expect(
          s.session.queueHeld,
          'the mode it was left in (plan) was not taken',
        );
        s.store.dispose();
      },
    );

    test(
      'and one that throws something else is said, and does not fail an opening that worked',
      () async {
        final s = started();
        s.session.applyOpened(_opened());
        s.agent.onSetMode = (mode) async =>
            throw StateError('connection closed');
        final open = Completer<AcpOpenedSession>();
        final reopened = s.session.reopen(() => open.future, keptMode: 'plan');
        await s.session.send('early');
        open.complete(_opened());
        await reopened;
        await settle();
        expect(s.session.opening, isFalse);
        expect(
          s.session.messages.map((m) => m.text),
          contains(
            'The mode it was left in could not be put back: connection closed',
          ),
        );
        expect(
          s.session.queueHeld,
          'the mode it was left in could not be put back',
        );
        expect(s.session.chosenMode, 'default');
        expect(s.agent.prompts, isEmpty);
        s.store.dispose();
      },
    );
  });

  group('a hand-over', () {
    test(
      'whose acknowledgement fails holds what was queued behind it',
      () async {
        final s = started();
        final ack = Completer<String?>();
        s.agent.onPrompt = (_) => ack.future;
        final a = s.session.send('A');
        await settle();
        await s.session.send('B');
        ack.completeError(
          const AcpFailure('Agent error: {code: 1, message: busy}'),
        );
        await expectLater(a, throwsA(isA<AcpFailure>()));
        await settle();
        expect(s.session.queue.single.text, 'B');
        expect(s.session.queueHeld, 'it could not be sent: busy');
        expect(s.session.messages.where((m) => m.text == 'A'), isEmpty);
        s.store.dispose();
      },
    );

    test(
      'whose acknowledgement never comes fails at the time the limit sets, and holds the queue',
      () async {
        final s = started(limits: const ChatLimits(handOverSeconds: 1));
        s.agent.onPrompt = (_) => Completer<String?>().future;
        final a = s.session.send('A');
        await settle();
        await s.session.send('B');
        await expectLater(
          a,
          throwsA(
            isA<StateError>().having(
              (e) => e.message,
              'message',
              contains('did not acknowledge'),
            ),
          ),
        );
        expect(s.session.messages.where((m) => m.text == 'A'), isEmpty);
        expect(s.session.queue.single.text, 'B');
        expect(s.session.queueHeld, contains('did not acknowledge'));
        s.store.dispose();
      },
    );

    test('is not bounded where the limit is 0', () async {
      final s = started(limits: const ChatLimits(handOverSeconds: 0));
      final ack = Completer<String?>();
      s.agent.onPrompt = (_) => ack.future;
      final a = s.session.send('A');
      await settle();
      ack.complete('m1');
      await a;
      expect(s.session.messages.single.messageId, 'm1');
      s.store.dispose();
    });
  });

  group('the second end of a v1 turn', () {
    test(
      'an event for the turn before is not the end of the one running now',
      () async {
        final s = started(version: 1);
        final answers = <Completer<String?>>[];
        s.agent.onPrompt = (_) {
          final c = Completer<String?>();
          answers.add(c);
          return c.future;
        };
        await s.session.send('A');
        s.agent.event(const AcpCompleted('end_turn', sessionId: 's1'));
        await settle();
        expect(s.session.status, ChatStatus.connected);
        await s.session.send('B');
        expect(s.session.status, ChatStatus.working);
        // The end that goes with A's answer, after B began.
        s.agent.event(const AcpCompleted('end_turn', sessionId: 's1'));
        await settle();
        expect(s.session.status, ChatStatus.working, reason: 'B goes on');
        answers[0].complete(null);
        await settle();
        expect(
          s.session.status,
          ChatStatus.working,
          reason: 'and A\'s answer does not end it either',
        );
        s.agent.event(const AcpCompleted('end_turn', sessionId: 's1'));
        answers[1].complete(null);
        await settle();
        expect(s.session.status, ChatStatus.connected);
        expect(s.session.turns.map((t) => t.outcome), [
          TurnOutcome.completed,
          TurnOutcome.completed,
        ]);
        s.store.dispose();
      },
    );

    test(
      'a state update that ended a turn, then its prompt\'s answer, after the next began',
      () async {
        final s = started(version: 1);
        final answers = <Completer<String?>>[];
        s.agent.onPrompt = (_) {
          final c = Completer<String?>();
          answers.add(c);
          return c.future;
        };
        await s.session.send('A');
        s.agent.update(state('idle', reason: 'end_turn'));
        await settle();
        expect(s.session.status, ChatStatus.connected);
        await s.session.send('B');
        s.agent.event(const AcpCompleted('end_turn', sessionId: 's1'));
        await settle();
        expect(
          s.session.status,
          ChatStatus.working,
          reason: 'that was A\'s answer',
        );
        answers[0].complete(null);
        await settle();
        expect(s.session.status, ChatStatus.working);
        s.store.dispose();
      },
    );
  });
}
