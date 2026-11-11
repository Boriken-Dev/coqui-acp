// Messages written while a turn runs: queued, editable and removable, sent as each turn ends, held, not dropped, when the turn is
// stopped or fails, and put ahead of the queue when a person steers. In both ACP versions.
import 'dart:async';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

List<String> _users(ChatSession session) => [
  for (final m in session.messages)
    if (m.role == MessageRole.user) m.text,
];

void main() {
  group('under v2', () {
    test(
      'a message written during a turn waits, and is sent when the turn ends, in the order written',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('The test fails. Fix it.');
        feed(session, state('running'));
        await session.send('Say queued one');
        await session.send('Say queued two');
        expect(session.queue.map((m) => m.text), [
          'Say queued one',
          'Say queued two',
        ]);
        expect(agent.prompts, [
          'The test fails. Fix it.',
        ], reason: 'nothing is sent while the turn runs');
        feed(session, state('idle', reason: 'end_turn'));
        await settle();
        expect(agent.prompts, [
          'The test fails. Fix it.',
          'Say queued one',
        ], reason: 'one per turn');
        expect(session.queue.single.text, 'Say queued two');
        feed(session, state('running'));
        feed(session, state('idle', reason: 'end_turn'));
        await settle();
        expect(agent.prompts.last, 'Say queued two');
        expect(session.queue, isEmpty);
        expect(_users(session), [
          'The test fails. Fix it.',
          'Say queued one',
          'Say queued two',
        ]);
        store.dispose();
      },
    );

    test(
      'Stop keeps the queue, held until a person sends it on, and then it goes on',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('Rename addNumbers to sum');
        feed(session, state('running'));
        await session.send('Say after the stop');
        await session.send('Say later still');
        await session.cancel();
        feed(session, state('idle', reason: 'cancelled'));
        await settle();
        expect(session.queue.map((m) => m.text), [
          'Say after the stop',
          'Say later still',
        ]);
        expect(session.queueHeld, 'the turn was stopped');
        expect(agent.prompts, ['Rename addNumbers to sum']);
        await session.sendQueued();
        expect(agent.prompts.last, 'Say after the stop');
        expect(session.queueHeld, isNull);
        feed(session, state('running'));
        feed(session, state('idle', reason: 'end_turn'));
        await settle();
        expect(
          agent.prompts.last,
          'Say later still',
          reason: 'the rest of the queue goes on',
        );
        expect(session.queue, isEmpty);
        store.dispose();
      },
    );

    test(
      'a failed turn holds it too, and a message written then waits with the rest',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('one');
        feed(session, state('running'));
        await session.send('two');
        feed(session, state('idle', reason: 'error'));
        await settle();
        expect(session.queueHeld, 'the turn failed');
        await session.send('three');
        expect(session.queue.map((m) => m.text), ['two', 'three']);
        expect(agent.prompts, ['one']);
        store.dispose();
      },
    );

    test(
      'can be edited and removed one by one or all together, and a queue emptied is no longer held',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('go');
        feed(session, state('running'));
        for (final t in ['first', 'second', 'third']) {
          await session.send(t);
        }
        session.editQueued(session.queue.first, 'first, edited');
        session.editQueued(session.queue[1], '   ');
        expect(
          session.queue[1].text,
          'second',
          reason: 'an empty text is not an edit',
        );
        session.removeQueued(session.queue[1]);
        expect(session.queue.map((m) => m.text), ['first, edited', 'third']);
        feed(session, state('idle', reason: 'end_turn'));
        await settle();
        expect(agent.prompts.last, 'first, edited');
        expect(session.queue.single.text, 'third');
        session.removeQueued();
        expect(session.queue, isEmpty);
        expect(session.queueHeld, isNull);
        store.dispose();
      },
    );

    test('hold no more than the limit says', () async {
      final (:store, :session, :agent) = started(
        limits: const ChatLimits(queued: 2),
      );
      await session.send('go');
      feed(session, state('running'));
      await session.send('a');
      await session.send('b');
      await expectLater(session.send('c'), throwsStateError);
      expect(session.queue, hasLength(2));
      store.dispose();
    });

    test(
      'one that could not be sent stays first, and the queue waits for the person',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('go');
        feed(session, state('running'));
        await session.send('second');
        agent.onPrompt = (_) async => throw StateError('offline');
        feed(session, state('idle', reason: 'end_turn'));
        await settle();
        expect(session.queue.single.text, 'second');
        expect(session.queueHeld, 'it could not be sent: offline');
        expect(
          session.messages.last.text,
          'A queued message was not sent: offline',
        );
        await expectLater(session.sendQueued(), throwsStateError);
        expect(session.queue.single.text, 'second');
        store.dispose();
      },
    );

    test(
      'what goes with a message goes with it from the queue, and is said once',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('go');
        feed(session, state('running'));
        await session.send(
          'with a file',
          attachments: [AcpAttachment.text('notes.txt', 'hi')],
        );
        feed(session, state('idle', reason: 'end_turn'));
        await settle();
        expect(agent.attachmentNames.last, ['notes.txt']);
        expect(
          session.messages.where((m) => m.text == 'Sent with this: notes.txt'),
          hasLength(1),
        );
        store.dispose();
      },
    );

    test(
      'a message with no words and nothing with it is not sent, and one with only a file is',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('   ');
        expect(agent.prompts, isEmpty);
        await session.send(
          '',
          attachments: [AcpAttachment.link('a', 'file:///work/a')],
        );
        expect(agent.prompts, ['']);
        expect(_users(session), isEmpty, reason: 'no empty message is shown');
        store.dispose();
      },
    );

    test('nothing can be sent with no connection, or a closed one', () async {
      final lone = ChatSession(id: 'x', sessionId: 's1');
      await expectLater(lone.send('hi'), throwsStateError);
      final (:store, :session, :agent) = started();
      await agent.close();
      await settle();
      await expectLater(session.send('hi'), throwsStateError);
      store.dispose();
    });
  });

  group('steering', () {
    test(
      'puts a message ahead of the queue and ends the turn, which then goes on with it first',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('long job');
        feed(session, state('running'));
        await session.send('later');
        await session.steer('change course');
        expect(agent.cancels, ['s1']);
        expect(session.queue.map((m) => m.text), ['change course', 'later']);
        feed(session, state('idle', reason: 'cancelled'));
        await settle();
        expect(
          session.queueHeld,
          isNull,
          reason: 'a turn ended to steer is the way the message goes',
        );
        expect(agent.prompts, ['long job', 'change course']);
        expect(session.queue.single.text, 'later');
        // The next turn ends as one that was stopped would: the queue is held again, as it is for any stop.
        feed(session, state('running'));
        await session.cancel();
        feed(session, state('idle', reason: 'cancelled'));
        await settle();
        expect(session.queueHeld, 'the turn was stopped');
        store.dispose();
      },
    );

    test('with no turn running is an ordinary message', () async {
      final (:store, :session, :agent) = started();
      await session.steer('hello');
      expect(agent.prompts, ['hello']);
      expect(agent.cancels, isEmpty);
      store.dispose();
    });

    test('promotes a message already queued', () async {
      final (:store, :session, :agent) = started();
      await session.send('job');
      feed(session, state('running'));
      await session.send('a');
      await session.send('b');
      await session.steerQueued(session.queue.last);
      expect(session.queue.map((m) => m.text), ['b', 'a']);
      feed(session, state('idle', reason: 'cancelled'));
      await settle();
      expect(agent.prompts, ['job', 'b']);
      store.dispose();
    });

    test(
      'that the agent could not be asked for leaves the message waiting, and the queue held as a stop holds it',
      () async {
        final (:store, :session, :agent) = started();
        await session.send('job');
        feed(session, state('running'));
        agent.onCancel = (_) async => throw StateError('not now');
        await expectLater(session.steer('urgent'), throwsStateError);
        expect(session.queue.single.text, 'urgent');
        feed(session, state('idle', reason: 'cancelled'));
        await settle();
        expect(session.queueHeld, 'the turn was stopped');
        expect(agent.prompts, ['job']);
        store.dispose();
      },
    );
  });

  group('under v1, whose turn ends with the answer to its prompt', () {
    test(
      'the message shows and the turn starts when it is sent, and the next goes when the turn ends',
      () async {
        final (:store, :session, :agent) = started(version: 1);
        final turn = Completer<String?>();
        agent.onPrompt = (_) => turn.future;
        final sent = session.send('Fix the test');
        await settle();
        expect(session.status, ChatStatus.working);
        expect(_users(session), ['Fix the test']);
        expect(session.currentTurn, isNotNull);
        await session.send('And then this');
        expect(session.queue.single.text, 'And then this');
        agent.onPrompt = null;
        agent.event(const AcpCompleted('end_turn'));
        turn.complete(null);
        await sent;
        await settle();
        expect(agent.prompts, ['Fix the test', 'And then this']);
        expect(session.turns.first.outcome, TurnOutcome.completed);
        expect(session.queue, isEmpty);
        store.dispose();
      },
    );

    test(
      'a turn the agent ends as cancelled says so and holds the queue',
      () async {
        final (:store, :session, :agent) = started(version: 1);
        final turn = Completer<String?>();
        agent.onPrompt = (_) => turn.future;
        final sent = session.send('one');
        await session.send('two');
        agent.event(const AcpCancelled());
        turn.complete(null);
        await sent;
        await settle();
        expect(session.queueHeld, 'the turn was stopped');
        expect(session.messages.map((m) => m.text), contains('Stopped.'));
        expect(session.status, ChatStatus.connected);
        expect(session.turns.single.outcome, TurnOutcome.cancelled);
        store.dispose();
      },
    );

    test(
      'a failed turn shows why, needs the person, and holds the queue',
      () async {
        final (:store, :session, :agent) = started(version: 1);
        final seen = <AttentionKind>[];
        store.attention.listen((a) => seen.add(a.kind));
        final turn = Completer<String?>();
        agent.onPrompt = (_) => turn.future;
        final sent = session.send('one');
        await session.send('two');
        agent.event(const AcpFailed('Rate limit reached'));
        turn.complete(null);
        await sent;
        await settle();
        expect(
          session.messages
              .where((m) => m.role == MessageRole.notice)
              .map((m) => m.text),
          contains('Rate limit reached'),
        );
        expect(session.queueHeld, 'the turn failed');
        expect(seen, [AttentionKind.finished]);
        store.dispose();
      },
    );

    test(
      'a prompt that fails without the agent saying so ends the turn, or the chat would wait for it for good',
      () async {
        final (:store, :session, :agent) = started(version: 1);
        agent.onPrompt = (_) async => throw const AcpFailure('lost');
        await expectLater(session.send('one'), throwsA(isA<AcpFailure>()));
        expect(session.status, ChatStatus.connected);
        expect(session.turns.single.outcome, TurnOutcome.failed);
        store.dispose();
      },
    );

    test(
      'output the agent lost, and a question that could not be put to a person, are said',
      () async {
        final (:store, :session, :agent) = started(version: 1);
        agent.event(const AcpOutputLost(lines: 1, bytes: 40));
        agent.event(const AcpOutputLost(lines: 3, bytes: 900));
        agent.event(const AcpBlocked('no form'));
        await settle();
        expect(session.messages.map((m) => m.text), [
          'While nobody was connected, 1 message from the agent (40 bytes) were dropped, so something it said or did is missing here.',
          'While nobody was connected, 3 messages from the agent (900 bytes) were dropped, so something it said or did is missing here.',
          'A question from the agent was answered cancelled: no form.',
        ]);
        store.dispose();
      },
    );
  });
  _concurrent();
}

void _concurrent() {
  group('messages written in the same moment', () {
    test(
      'the second waits for the first to be handed over, whichever version',
      () async {
        for (final version in [1, 2]) {
          final (:store, :session, :agent) = started(version: version);
          final hold = Completer<String?>();
          agent.onPrompt = (_) => hold.future;
          final first = session.send('one');
          final second = session.send('two');
          await second;
          expect(session.queue.map((m) => m.text), [
            'two',
          ], reason: 'v$version');
          expect(agent.prompts, ['one'], reason: 'v$version');
          agent.onPrompt = null;
          if (version == 1) {
            agent.event(const AcpCompleted('end_turn'));
          } else {
            feed(session, state('running'));
          }
          hold.complete(version == 1 ? null : 'm1');
          await first;
          if (version == 2) feed(session, state('idle', reason: 'end_turn'));
          await settle();
          expect(agent.prompts, ['one', 'two'], reason: 'v$version');
          store.dispose();
        }
      },
    );

    test(
      'a turn that ends before the agent has acknowledged the prompt does not strand the queue',
      () async {
        final (:store, :session, :agent) = started();
        final hold = Completer<String?>();
        agent.onPrompt = (_) => hold.future;
        final first = session.send('one');
        await session.send('two');
        feed(session, state('running'));
        feed(session, state('idle', reason: 'end_turn'));
        agent.onPrompt = null;
        hold.complete('m1');
        await first;
        await settle();
        expect(agent.prompts, ['one', 'two']);
        store.dispose();
      },
    );
  });
}
